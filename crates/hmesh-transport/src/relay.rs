//! The built-in relay (`hmesh relay serve`), and how computers use one.
//!
//! Two computers that cannot reach each other (both behind NAT, say) both
//! reach a relay on a server. One holds a control connection to it; the
//! other asks the relay to put it through, and the relay joins the two
//! connections and passes their bytes on unchanged. Inside, the two speak
//! TLS with their pinned certificates as on any other address, so the relay
//! sees who connects to whom, when, and how much, but not what they say,
//! and it cannot pass for either of them.
//!
//! Every connection to a relay is TLS 1.3: the computer pins the relay's
//! certificate fingerprint and shows its own certificate, so the relay
//! knows each computer by its fingerprint. Then one line, at most
//! `LINE_BYTES` long, TLS and the line together within `LINE_TIME`:
//!
//! - `LISTEN <token>`: this computer may be put through to (`OK`). The relay
//!   then says `CONNECT <id> <fingerprint>` on this connection for each
//!   computer asking for it (its certificate's fingerprint, as the relay
//!   saw it); the computer answers each with a new connection, with
//!   `BUSY <id>` when it is serving all it can (or all that computer may
//!   have), or with `CLOSED <id>` when it does not take that computer at
//!   all (not paired with it, and not pairing). It says `PING` every 30
//!   seconds and the relay `PONG`.
//! - `ACCEPT <token> <id>`: a new connection from that computer, for
//!   `CONNECT <id>` (`OK`); from here on it carries the asking computer's
//!   bytes. Only the computer that `CONNECT <id>` was for may accept it.
//! - `DIAL <token> <fingerprint>`: put this connection through to that
//!   computer: `OK` once it has accepted, or `ERR <reason>` (`offline`,
//!   `busy`, `closed`, `timeout`; `full` when the relay holds all the
//!   computers it may).
//!
//! A wrong token, or anything else, closes the connection at once, without
//! a word. Limits: `Limits`. Until a connection has said its first line it
//! costs nothing to open (no token, not even TLS), so those are counted by
//! source address (an IPv6 one by its /64, and the /48 around it too): each
//! source opens only a few at once, and at a bounded rate.
use crate::client::{ANOTHER, NOT_RELAYED, TURNED_DOWN, reason};
use hmesh_core::{RelayAddress, RelayHost};
use hmesh_security::{
    Instance, fingerprint, peer_address, relay::RelayIdentity, relay::same_secret,
};
use std::{
    collections::{HashMap, VecDeque},
    net::{IpAddr, Ipv6Addr, SocketAddr},
    sync::{
        Arc, Mutex,
        atomic::{AtomicU64, Ordering},
    },
    time::{Duration, Instant},
};
use tokio::{
    io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt},
    net::{TcpListener, TcpStream},
    sync::{OwnedSemaphorePermit, Semaphore, mpsc, oneshot, watch},
    task::JoinSet,
};
use tokio_rustls::{TlsAcceptor, TlsConnector};

/// The longest line, its newline included.
pub const LINE_BYTES: usize = 512;
/// How long a connection to a relay has to complete TLS and say its first
/// line, and how long a computer waits for a relay's answer to it.
pub const LINE_TIME: Duration = Duration::from_secs(5);
/// How often a computer holding a control connection pings the relay.
pub const PING_EVERY: Duration = Duration::from_secs(30);
/// How long a computer waits for its `PONG` before it reconnects.
const PONG_WAIT: Duration = Duration::from_secs(10);
/// A relay drops a control connection it has heard nothing on for this long.
const SILENT_CONTROL: Duration = Duration::from_secs(75);
/// How long a `DIAL` waits for the computer it asks for to accept.
pub const ACCEPT_WAIT: Duration = Duration::from_secs(10);
/// How long the TCP connection to a relay may take.
const CONNECT_TIME: Duration = Duration::from_secs(5);
/// A connection through a relay kept for the next call to the same
/// computer is closed after this long unused.
pub const KEPT_IDLE: Duration = Duration::from_secs(60);
/// Connections kept per relay and computer.
const KEPT_MOST: usize = 4;
/// A computer that loses its control connection tries again after a second,
/// then waits twice as long each time it fails, up to a minute.
const BACKOFF_FIRST: Duration = Duration::from_secs(1);
const BACKOFF_MOST: Duration = Duration::from_secs(60);
/// Connections put through a relay that a computer serves at once (as
/// many as a listener takes from other computers).
pub const SERVED_AT_ONCE: usize = 64;
/// Of those, for one computer it is paired with (by its certificate), so
/// one paired computer cannot use up the places of the others.
pub const SERVED_PER_CALLER: usize = 16;
/// Of those, for all the computers it is not paired with together, and
/// only while it is pairing.
pub const SERVED_STRANGERS: usize = 16;

/// The source address a connection put through a relay is served as: not
/// loopback (never this computer's owner), the same for every relayed
/// computer (the relay does not say where the asking one is), and in the
/// IPv6 discard prefix 100::/64, so it is no computer's real address.
pub const RELAYED: IpAddr = IpAddr::V6(Ipv6Addr::new(0x100, 0, 0, 0, 0, 0, 0, 0));
pub fn is_relayed(ip: IpAddr) -> bool {
    ip == RELAYED
}

/// One line of the relay protocol.
#[derive(Clone, PartialEq, Eq)]
pub enum Line {
    Listen { token: String },
    Accept { token: String, id: u64 },
    Dial { token: String, fingerprint: String },
    Connect { id: u64, fingerprint: String },
    Busy { id: u64 },
    Closed { id: u64 },
    Ok,
    Err(String),
    Ping,
    Pong,
}
/// As sent, token included.
impl std::fmt::Display for Line {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Listen { token } => write!(f, "LISTEN {token}"),
            Self::Accept { token, id } => write!(f, "ACCEPT {token} {id}"),
            Self::Dial { token, fingerprint } => write!(f, "DIAL {token} {fingerprint}"),
            Self::Connect { id, fingerprint } => write!(f, "CONNECT {id} {fingerprint}"),
            Self::Busy { id } => write!(f, "BUSY {id}"),
            Self::Closed { id } => write!(f, "CLOSED {id}"),
            Self::Ok => f.write_str("OK"),
            Self::Err(reason) => write!(f, "ERR {reason}"),
            Self::Ping => f.write_str("PING"),
            Self::Pong => f.write_str("PONG"),
        }
    }
}
/// Without the token.
impl std::fmt::Debug for Line {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Listen { .. } => f.write_str("LISTEN <token>"),
            Self::Accept { id, .. } => write!(f, "ACCEPT <token> {id}"),
            Self::Dial { fingerprint, .. } => write!(f, "DIAL <token> {fingerprint}"),
            other => write!(f, "{other}"),
        }
    }
}
/// Why a line was not read.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LineError {
    /// No newline within `LINE_BYTES`.
    TooLong,
    /// Not printable ASCII.
    NotText,
    /// Not finished within its time.
    TimedOut,
    /// The connection ended first.
    Closed,
    /// A command the relay protocol does not have.
    Unknown,
    /// A known command with the wrong words.
    Malformed,
}
impl std::fmt::Display for LineError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::TooLong => "line too long",
            Self::NotText => "line is not text",
            Self::TimedOut => "line not finished in time",
            Self::Closed => "connection closed",
            Self::Unknown => "unknown command",
            Self::Malformed => "malformed command",
        })
    }
}
impl std::error::Error for LineError {}
/// A line as received (without its newline).
pub fn parse(line: &str) -> Result<Line, LineError> {
    let mut words = line.split(' ');
    let command = words.next().unwrap_or_default();
    let args: Vec<&str> = words.collect();
    let known = [
        "LISTEN", "ACCEPT", "DIAL", "CONNECT", "BUSY", "CLOSED", "OK", "ERR", "PING", "PONG",
    ];
    if !known.contains(&command) {
        return Err(LineError::Unknown);
    }
    Ok(match (command, args.as_slice()) {
        ("LISTEN", [token]) => Line::Listen {
            token: hex64(token)?,
        },
        ("ACCEPT", [token, id]) => Line::Accept {
            token: hex64(token)?,
            id: number(id)?,
        },
        ("DIAL", [token, fingerprint]) => Line::Dial {
            token: hex64(token)?,
            fingerprint: hex64(fingerprint)?,
        },
        ("CONNECT", [id, fingerprint]) => Line::Connect {
            id: number(id)?,
            fingerprint: hex64(fingerprint)?,
        },
        ("BUSY", [id]) => Line::Busy { id: number(id)? },
        ("CLOSED", [id]) => Line::Closed { id: number(id)? },
        ("OK", []) => Line::Ok,
        ("ERR", [reason])
            if (1..=32).contains(&reason.len())
                && reason.bytes().all(|b| b.is_ascii_lowercase() || b == b'_') =>
        {
            Line::Err((*reason).to_owned())
        }
        ("PING", []) => Line::Ping,
        ("PONG", []) => Line::Pong,
        _ => return Err(LineError::Malformed),
    })
}
fn hex64(word: &str) -> Result<String, LineError> {
    if word.len() == 64
        && word
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    {
        Ok(word.to_owned())
    } else {
        Err(LineError::Malformed)
    }
}
fn number(word: &str) -> Result<u64, LineError> {
    if word.is_empty() || word.len() > 20 || !word.bytes().all(|b| b.is_ascii_digit()) {
        return Err(LineError::Malformed);
    }
    word.parse().map_err(|_| LineError::Malformed)
}
/// One line, read a byte at a time: nothing after its newline is taken,
/// so the bytes that follow (TLS, once a connection is put through) stay
/// for whoever reads next.
pub async fn read_line<R: AsyncRead + Unpin>(reader: &mut R) -> Result<String, LineError> {
    let mut line = Vec::with_capacity(80);
    loop {
        let mut byte = [0u8; 1];
        match reader.read(&mut byte).await {
            Ok(1) => {}
            _ => return Err(LineError::Closed),
        }
        if byte[0] == b'\n' {
            break;
        }
        line.push(byte[0]);
        if line.len() >= LINE_BYTES {
            return Err(LineError::TooLong);
        }
    }
    if !line.iter().all(|b| (0x20..0x7f).contains(b)) {
        return Err(LineError::NotText);
    }
    String::from_utf8(line).map_err(|_| LineError::NotText)
}
/// One line within `LINE_TIME`, read as a command.
pub async fn read_command<R: AsyncRead + Unpin>(reader: &mut R) -> Result<Line, LineError> {
    read_command_within(reader, LINE_TIME).await
}
/// One line within `time`, read as a command.
pub async fn read_command_within<R: AsyncRead + Unpin>(
    reader: &mut R,
    time: Duration,
) -> Result<Line, LineError> {
    let line = tokio::time::timeout(time, read_line(reader))
        .await
        .unwrap_or(Err(LineError::TimedOut))?;
    parse(&line)
}
pub async fn write_line<W: AsyncWrite + Unpin>(writer: &mut W, line: &Line) -> std::io::Result<()> {
    writer.write_all(format!("{line}\n").as_bytes()).await?;
    writer.flush().await
}

/// How much a relay takes on.
#[derive(Clone, Copy, Debug)]
pub struct Limits {
    /// Computers holding a control connection.
    pub registrations: usize,
    /// Of those, from one source address. There is one token per relay:
    /// whoever has it could otherwise hold every place with new
    /// certificates from one address.
    pub registrations_per_source: usize,
    /// Pairs of connections put through (or waiting to be) at once.
    pub pairs: usize,
    /// Of those, asked for by one computer (by its certificate).
    pub pairs_per_computer: usize,
    /// Of those, asked for from one source address.
    pub pairs_per_source: usize,
    /// A pair passing nothing either way for this long is closed.
    pub idle: Duration,
    /// Connections that have not said their first line yet.
    pub unauthenticated: usize,
    /// Of those, from one source address. When all of them are taken, one
    /// from the source with the most goes to make room; failing that, the
    /// oldest one that has had `YIELD_AFTER`.
    pub unauthenticated_per_source: usize,
    /// Of those, from one IPv6 /48 (a site can have 65536 /64s, each a
    /// source of its own). IPv4 addresses are not grouped.
    pub unauthenticated_per_network: usize,
    /// How long a connection has for TLS and its first line.
    pub first_line: Duration,
    /// New connections from one source address.
    pub rate: Rate,
}
impl Default for Limits {
    fn default() -> Self {
        Self {
            registrations: 256,
            registrations_per_source: 16,
            pairs: 512,
            pairs_per_computer: 32,
            pairs_per_source: 64,
            idle: Duration::from_secs(15 * 60),
            unauthenticated: 256,
            unauthenticated_per_source: 16,
            unauthenticated_per_network: 32,
            first_line: LINE_TIME,
            rate: Rate {
                burst: 32,
                per_second: 8,
            },
        }
    }
}
/// New connections from one source: up to `burst` at once, refilled at
/// `per_second`.
#[derive(Clone, Copy, Debug)]
pub struct Rate {
    pub burst: u32,
    pub per_second: u32,
}
/// Where a connection comes from, as limits count it: an IPv4 address, or
/// the /64 an IPv6 address is in (one computer can have many of those).
pub fn source_of(ip: IpAddr) -> IpAddr {
    match ip.to_canonical() {
        IpAddr::V6(ip) => {
            let s = ip.segments();
            IpAddr::V6(Ipv6Addr::new(s[0], s[1], s[2], s[3], 0, 0, 0, 0))
        }
        v4 => v4,
    }
}
/// The IPv6 /48 an address is in (what one site is often given), counted
/// as a whole as well; none for IPv4.
pub fn network_of(ip: IpAddr) -> Option<IpAddr> {
    match ip.to_canonical() {
        IpAddr::V6(ip) => {
            let s = ip.segments();
            Some(IpAddr::V6(Ipv6Addr::new(s[0], s[1], s[2], 0, 0, 0, 0, 0)))
        }
        IpAddr::V4(_) => None,
    }
}
/// What each source has left of its `Rate`.
pub struct Sources {
    rate: Rate,
    buckets: HashMap<IpAddr, (f64, Instant)>,
}
/// Sources kept at most; full ones are forgotten first.
const MAX_SOURCES: usize = 65536;
impl Sources {
    pub fn new(rate: Rate) -> Self {
        Self {
            rate,
            buckets: HashMap::new(),
        }
    }
    /// Whether a new connection from `ip` is let in at `now`.
    pub fn admit(&mut self, ip: IpAddr, now: Instant) -> bool {
        let source = source_of(ip);
        let (burst, refill) = (f64::from(self.rate.burst), f64::from(self.rate.per_second));
        if self.buckets.len() >= MAX_SOURCES && !self.buckets.contains_key(&source) {
            self.buckets.retain(|_, (left, at)| {
                *left + now.saturating_duration_since(*at).as_secs_f64() * refill < burst
            });
            if self.buckets.len() >= MAX_SOURCES {
                return false;
            }
        }
        let (left, at) = self.buckets.entry(source).or_insert((burst, now));
        *left = (*left + now.saturating_duration_since(*at).as_secs_f64() * refill).min(burst);
        *at = now;
        if *left >= 1.0 {
            *left -= 1.0;
            true
        } else {
            false
        }
    }
}

/// How long a connection must have had for TLS and its first line before
/// it is closed to make room when every place is taken and no source holds
/// more than another: one on a long link still has time for both.
pub const YIELD_AFTER: Duration = Duration::from_secs(1);
/// The connections that have not said their first line yet, by source:
/// each source (and each IPv6 /48) holds at most so many. When every place
/// is taken, the oldest of the source holding the most is closed to make
/// room for a source holding fewer; when none holds more, the oldest of
/// all, once it has had `YIELD_AFTER`.
pub struct Doorway {
    total: usize,
    per_source: usize,
    per_network: usize,
    held: usize,
    by_source: HashMap<IpAddr, VecDeque<(u64, Instant, oneshot::Sender<()>)>>,
    by_network: HashMap<IpAddr, usize>,
    next: u64,
}
/// A place in the doorway: `closed` resolves when it was taken away to
/// make room. Leave with `Doorway::leave`.
pub struct Place {
    pub id: u64,
    pub source: IpAddr,
    pub closed: oneshot::Receiver<()>,
}
impl Doorway {
    pub fn new(total: usize, per_source: usize, per_network: usize) -> Self {
        Self {
            total,
            per_source,
            per_network,
            held: 0,
            by_source: HashMap::new(),
            by_network: HashMap::new(),
            next: 0,
        }
    }
    /// A place for a connection from `ip` now, if there is one.
    pub fn enter(&mut self, ip: IpAddr) -> Option<Place> {
        self.enter_at(ip, Instant::now())
    }
    /// A place for a connection from `ip` at `now`, if there is one.
    pub fn enter_at(&mut self, ip: IpAddr, now: Instant) -> Option<Place> {
        let source = source_of(ip);
        let network = network_of(source);
        let mine = self.by_source.get(&source).map_or(0, VecDeque::len);
        if mine >= self.per_source {
            return None;
        }
        if network.is_some_and(|n| {
            self.by_network
                .get(&n)
                .is_some_and(|c| *c >= self.per_network)
        }) {
            return None;
        }
        if self.held >= self.total {
            // The source holding the most gives up its oldest, if it holds
            // more than this one would; otherwise the oldest of all, once
            // it has had its time.
            let (most, count) = self
                .by_source
                .iter()
                .map(|(source, held)| (*source, held.len()))
                .max_by_key(|(_, count)| *count)?;
            let giving = if count > mine + 1 {
                most
            } else {
                let (oldest, since) = self
                    .by_source
                    .iter()
                    .filter_map(|(source, held)| held.front().map(|(_, at, _)| (*source, *at)))
                    .min_by_key(|(_, at)| *at)?;
                if now.saturating_duration_since(since) < YIELD_AFTER {
                    return None;
                }
                oldest
            };
            self.give_up_oldest(giving);
        }
        let (close, closed) = oneshot::channel();
        self.next += 1;
        let id = self.next;
        self.by_source
            .entry(source)
            .or_default()
            .push_back((id, now, close));
        if let Some(network) = network {
            *self.by_network.entry(network).or_default() += 1;
        }
        self.held += 1;
        Some(Place { id, source, closed })
    }
    /// Close the oldest connection of `source` (dropping its sender ends it).
    fn give_up_oldest(&mut self, source: IpAddr) {
        if let Some(waiting) = self.by_source.get_mut(&source)
            && waiting.pop_front().is_some()
        {
            if waiting.is_empty() {
                self.by_source.remove(&source);
            }
            self.left(source);
        }
    }
    /// The connection said its first line, or ended: its place is free.
    pub fn leave(&mut self, place: &Place) {
        if let Some(waiting) = self.by_source.get_mut(&place.source)
            && let Some(at) = waiting.iter().position(|(id, _, _)| *id == place.id)
        {
            waiting.remove(at);
            if waiting.is_empty() {
                self.by_source.remove(&place.source);
            }
            self.left(place.source);
        }
    }
    /// One place of `source` is free.
    fn left(&mut self, source: IpAddr) {
        self.held -= 1;
        if let Some(network) = network_of(source)
            && let Some(count) = self.by_network.get_mut(&network)
        {
            *count -= 1;
            if *count == 0 {
                self.by_network.remove(&network);
            }
        }
    }
    /// Places held now (for tests).
    pub fn held(&self) -> usize {
        self.held
    }
}

/// The token computers use a relay with, kept current: a new one ends
/// every connection that came in with the old one.
#[derive(Clone)]
pub struct Token(Arc<watch::Sender<String>>);
impl Token {
    pub fn fixed(token: String) -> Self {
        Self(Arc::new(watch::channel(token).0))
    }
    /// The relay's token, read again every two seconds (`hmesh relay show
    /// --rotate-token` writes a new one) until `stop`.
    pub fn of(
        identity: Arc<RelayIdentity>,
        mut stop: watch::Receiver<bool>,
    ) -> anyhow::Result<Self> {
        let token = Self::fixed(identity.token()?);
        let kept = token.clone();
        tokio::spawn(async move {
            let mut every = tokio::time::interval(Duration::from_secs(2));
            loop {
                tokio::select! {
                    _ = every.tick() => {}
                    changed = stop.changed() => if changed.is_err() || *stop.borrow() { return },
                }
                if let Ok(now) = identity.token()
                    && *kept.0.borrow() != now
                {
                    tracing::info!(
                        "the relay token was changed: connections made with the old one are closed"
                    );
                    kept.set(now);
                }
            }
        });
        Ok(token)
    }
    pub fn set(&self, token: String) {
        self.0.send_replace(token);
    }
    fn matches(&self, given: &str) -> bool {
        same_secret(&self.0.borrow(), given)
    }
    fn subscribe(&self) -> watch::Receiver<String> {
        self.0.subscribe()
    }
}

/// The bytes a relay passes on, written to a file for tests (a debug build
/// with `HMESH_RELAY_TAP` set): nothing in a release build.
#[cfg(debug_assertions)]
type Tap = Arc<Mutex<std::fs::File>>;
#[cfg(not(debug_assertions))]
type Tap = std::convert::Infallible;
#[cfg(debug_assertions)]
fn tap_from_env() -> Option<Tap> {
    let path = std::env::var_os("HMESH_RELAY_TAP")?;
    let file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .ok()?;
    Some(Arc::new(Mutex::new(file)))
}
#[cfg(not(debug_assertions))]
fn tap_from_env() -> Option<Tap> {
    None
}
#[cfg(debug_assertions)]
fn tap_write(tap: &Tap, bytes: &[u8]) {
    use std::io::Write;
    if let Ok(mut file) = tap.lock() {
        let _ = file.write_all(bytes);
    }
}
#[cfg(not(debug_assertions))]
fn tap_write(tap: &Tap, _bytes: &[u8]) {
    match *tap {}
}

/// Pass bytes both ways between `a` and `b` until both have ended, or
/// nothing has passed for `idle`. One side ending its writing ends the
/// other's (half close), so a reply still gets through.
pub async fn splice<A, B>(a: A, b: B, idle: Duration)
where
    A: AsyncRead + AsyncWrite + Unpin + Send,
    B: AsyncRead + AsyncWrite + Unpin + Send,
{
    splice_tapped(a, b, idle, None).await
}
async fn splice_tapped<A, B>(a: A, b: B, idle: Duration, tap: Option<&Tap>)
where
    A: AsyncRead + AsyncWrite + Unpin + Send,
    B: AsyncRead + AsyncWrite + Unpin + Send,
{
    let (mut a_read, mut a_write) = tokio::io::split(a);
    let (mut b_read, mut b_write) = tokio::io::split(b);
    let start = Instant::now();
    let last = AtomicU64::new(0);
    let both = async {
        tokio::join!(
            pass(&mut a_read, &mut b_write, start, &last, tap),
            pass(&mut b_read, &mut a_write, start, &last, tap)
        )
    };
    let check = (idle / 4).clamp(Duration::from_millis(10), Duration::from_secs(30));
    let watchdog = async {
        loop {
            tokio::time::sleep(check).await;
            let quiet = start
                .elapsed()
                .saturating_sub(Duration::from_millis(last.load(Ordering::Relaxed)));
            if quiet >= idle {
                return;
            }
        }
    };
    tokio::select! {
        _ = both => {}
        () = watchdog => {}
    }
}
async fn pass<R, W>(from: &mut R, to: &mut W, start: Instant, last: &AtomicU64, tap: Option<&Tap>)
where
    R: AsyncRead + Unpin,
    W: AsyncWrite + Unpin,
{
    let mut buffer = vec![0u8; 16 * 1024];
    loop {
        let read = match from.read(&mut buffer).await {
            Ok(0) | Err(_) => break,
            Ok(read) => read,
        };
        last.store(start.elapsed().as_millis() as u64, Ordering::Relaxed);
        if let Some(tap) = tap {
            tap_write(tap, &buffer[..read]);
        }
        if to.write_all(&buffer[..read]).await.is_err() || to.flush().await.is_err() {
            break;
        }
    }
    let _ = to.shutdown().await;
}

type Accepted = tokio_rustls::server::TlsStream<TcpStream>;
/// A relay's state while it serves.
struct Hub {
    limits: Limits,
    token: Token,
    /// Each computer holding a control connection, by fingerprint.
    computers: Mutex<HashMap<String, Registered>>,
    /// Each `DIAL` waiting for its computer's `ACCEPT`, by id.
    waiting: Mutex<HashMap<u64, Waiting>>,
    next: AtomicU64,
    pairs: Arc<Semaphore>,
    /// Pairs asked for now, by the asking computer and by its source.
    pairs_by_computer: Mutex<HashMap<String, usize>>,
    pairs_by_source: Mutex<HashMap<IpAddr, usize>>,
    doorway: Mutex<Doorway>,
    sources: Mutex<Sources>,
    tap: Option<Tap>,
}
struct Registered {
    /// Tells the computers apart over time: a computer that registers
    /// again replaces its older connection.
    id: u64,
    source: IpAddr,
    /// Each `DIAL` for it: its id, and the computer asking.
    connect: mpsc::Sender<(u64, String)>,
}
struct Waiting {
    target: String,
    /// Its connection, or why there is none (`closed`); dropped: `busy`.
    answer: oneshot::Sender<Result<Accepted, &'static str>>,
}
/// Serve a relay on `listener` until `shutdown`: `tls` is its identity
/// (`RelayIdentity::server_config`), `token` lets computers in.
pub async fn serve(
    listener: TcpListener,
    tls: Arc<rustls::ServerConfig>,
    token: Token,
    limits: Limits,
    mut shutdown: watch::Receiver<bool>,
) -> anyhow::Result<()> {
    let hub = Arc::new(Hub {
        limits,
        token,
        computers: Mutex::default(),
        waiting: Mutex::default(),
        next: AtomicU64::new(1),
        pairs: Arc::new(Semaphore::new(limits.pairs)),
        pairs_by_computer: Mutex::default(),
        pairs_by_source: Mutex::default(),
        doorway: Mutex::new(Doorway::new(
            limits.unauthenticated,
            limits.unauthenticated_per_source,
            limits.unauthenticated_per_network,
        )),
        sources: Mutex::new(Sources::new(limits.rate)),
        tap: tap_from_env(),
    });
    let acceptor = TlsAcceptor::from(tls);
    let mut connections = JoinSet::new();
    loop {
        tokio::select! {
            biased;
            changed = shutdown.changed() => if changed.is_err() || *shutdown.borrow() { break },
            accepted = listener.accept() => {
                let (stream, address) = match accepted {
                    Ok(accepted) => accepted,
                    Err(error) => {
                        // Out of file descriptors, say: let some close first.
                        tracing::warn!(%error, "the relay could not take a connection");
                        tokio::time::sleep(Duration::from_millis(100)).await;
                        continue;
                    }
                };
                let admitted = hub
                    .sources
                    .lock()
                    .is_ok_and(|mut sources| sources.admit(address.ip(), Instant::now()));
                if !admitted {
                    continue;
                }
                let Some(place) = hub.doorway.lock().ok().and_then(|mut d| d.enter(address.ip())) else {
                    continue;
                };
                let _ = stream.set_nodelay(true);
                let hub = hub.clone();
                let acceptor = acceptor.clone();
                let stop = shutdown.clone();
                connections.spawn(async move {
                    hub.connection(stream, address, acceptor, place, stop).await;
                });
            }
            Some(_) = connections.join_next(), if !connections.is_empty() => {}
        }
    }
    connections.abort_all();
    while connections.join_next().await.is_some() {}
    Ok(())
}
/// One more pair asked for by a computer and from a source, while held.
struct Asked<'a> {
    hub: &'a Hub,
    computer: String,
    source: IpAddr,
}
impl Drop for Asked<'_> {
    fn drop(&mut self) {
        if let Ok(mut map) = self.hub.pairs_by_computer.lock()
            && let Some(count) = map.get_mut(&self.computer)
        {
            *count -= 1;
            if *count == 0 {
                map.remove(&self.computer);
            }
        }
        if let Ok(mut map) = self.hub.pairs_by_source.lock()
            && let Some(count) = map.get_mut(&self.source)
        {
            *count -= 1;
            if *count == 0 {
                map.remove(&self.source);
            }
        }
    }
}
impl Hub {
    fn registered(&self) -> usize {
        self.computers.lock().map(|c| c.len()).unwrap_or_default()
    }
    async fn connection(
        self: Arc<Self>,
        stream: TcpStream,
        address: SocketAddr,
        acceptor: TlsAcceptor,
        mut place: Place,
        stop: watch::Receiver<bool>,
    ) {
        // TLS and the first line, within their time together, unless the
        // place is taken away for a source holding fewer.
        let first = tokio::select! {
            first = tokio::time::timeout(self.limits.first_line, async {
                let mut tls = acceptor.accept(stream).await.ok()?;
                let seen = tls
                    .get_ref()
                    .1
                    .peer_certificates()?
                    .first()
                    .map(|cert| fingerprint(cert.as_ref()))?;
                let line = read_line(&mut tls).await.ok()?;
                Some((tls, seen, line))
            }) => first.ok().flatten(),
            _ = &mut place.closed => None,
        };
        if let Ok(mut doorway) = self.doorway.lock() {
            doorway.leave(&place);
        }
        let Some((tls, seen, line)) = first else {
            return;
        };
        let source = source_of(address.ip());
        let token = self.token.subscribe();
        // A wrong token, an unknown command or a malformed one: closed at
        // once, and nothing says which.
        match parse(&line) {
            Ok(Line::Listen { token: given }) if self.token.matches(&given) => {
                self.listen(tls, seen, source, token, stop).await
            }
            Ok(Line::Dial {
                token: given,
                fingerprint,
            }) if self.token.matches(&given) => {
                self.dial(tls, seen, source, fingerprint, token, stop).await
            }
            Ok(Line::Accept { token: given, id }) if self.token.matches(&given) => {
                self.accept(tls, seen, id).await
            }
            _ => {}
        }
    }
    /// A computer that may be put through to, until its connection ends,
    /// goes quiet, is replaced, or the token changes.
    async fn listen(
        &self,
        mut tls: Accepted,
        computer: String,
        source: IpAddr,
        mut token: watch::Receiver<String>,
        mut stop: watch::Receiver<bool>,
    ) {
        let id = self.next.fetch_add(1, Ordering::Relaxed);
        let (connect, mut connects) = mpsc::channel(32);
        let taken = match self.computers.lock() {
            Ok(mut computers) => {
                let again = computers.contains_key(&computer);
                let from_source = computers
                    .iter()
                    .filter(|(known, r)| r.source == source && **known != computer)
                    .count();
                if again
                    || (computers.len() < self.limits.registrations
                        && from_source < self.limits.registrations_per_source)
                {
                    // An older connection of the same computer ends: its
                    // channel closes.
                    computers.insert(
                        computer.clone(),
                        Registered {
                            id,
                            source,
                            connect,
                        },
                    );
                    true
                } else {
                    false
                }
            }
            Err(_) => false,
        };
        if !taken {
            let _ = write_line(&mut tls, &Line::Err("full".into())).await;
            return;
        }
        tracing::info!(
            computers = self.registered(),
            "a computer is holding a connection"
        );
        if write_line(&mut tls, &Line::Ok).await.is_ok() {
            let (mut reader, mut writer) = tokio::io::split(tls);
            let (heard, mut hearing) = mpsc::channel(4);
            let reading = tokio::spawn(async move {
                loop {
                    let line = read_line(&mut reader).await.and_then(|line| parse(&line));
                    let ended = line.is_err();
                    if heard.send(line).await.is_err() || ended {
                        return;
                    }
                }
            });
            let silence = tokio::time::sleep(SILENT_CONTROL);
            tokio::pin!(silence);
            loop {
                tokio::select! {
                    next = connects.recv() => match next {
                        Some((id, fingerprint)) => {
                            if write_line(&mut writer, &Line::Connect { id, fingerprint }).await.is_err() { break }
                        }
                        None => break,
                    },
                    line = hearing.recv() => match line {
                        Some(Ok(Line::Ping)) => {
                            silence.as_mut().reset(tokio::time::Instant::now() + SILENT_CONTROL);
                            if write_line(&mut writer, &Line::Pong).await.is_err() { break }
                        }
                        // It serves all it can, or does not take the computer
                        // asking at all: that computer is told so now.
                        Some(Ok(Line::Busy { id })) => {
                            self.turned_down(&computer, id, "busy");
                        }
                        Some(Ok(Line::Closed { id })) => {
                            self.turned_down(&computer, id, "closed");
                        }
                        _ => break,
                    },
                    () = &mut silence => break,
                    _ = token.changed() => break,
                    _ = stop.changed() => break,
                }
            }
            reading.abort();
        }
        if let Ok(mut computers) = self.computers.lock()
            && computers.get(&computer).is_some_and(|r| r.id == id)
        {
            computers.remove(&computer);
        }
    }
    /// The `DIAL` waiting with `id`, turned down by `computer`, the one it
    /// asked for (no other computer may answer for it).
    fn turned_down(&self, computer: &str, id: u64, why: &'static str) {
        let waiting = self.waiting.lock().ok().and_then(|mut waiting| {
            if waiting.get(&id).is_some_and(|w| w.target == computer) {
                waiting.remove(&id)
            } else {
                None
            }
        });
        if let Some(waiting) = waiting {
            let _ = waiting.answer.send(Err(why));
        }
    }
    /// Count one more pair asked for by `computer` from `source`, if
    /// neither has asked for all it may.
    fn ask(&self, computer: &str, source: IpAddr) -> Option<Asked<'_>> {
        let mut by_computer = self.pairs_by_computer.lock().ok()?;
        let mut by_source = self.pairs_by_source.lock().ok()?;
        let mine = by_computer.get(computer).copied().unwrap_or(0);
        let from_source = by_source.get(&source).copied().unwrap_or(0);
        if mine >= self.limits.pairs_per_computer || from_source >= self.limits.pairs_per_source {
            return None;
        }
        *by_computer.entry(computer.to_owned()).or_default() += 1;
        *by_source.entry(source).or_default() += 1;
        Some(Asked {
            hub: self,
            computer: computer.to_owned(),
            source,
        })
    }
    /// Put a connection from `asking` through to `target`, once it accepts.
    async fn dial(
        &self,
        mut tls: Accepted,
        asking: String,
        source: IpAddr,
        target: String,
        mut token: watch::Receiver<String>,
        mut stop: watch::Receiver<bool>,
    ) {
        let Some(_asked) = self.ask(&asking, source) else {
            let _ = write_line(&mut tls, &Line::Err("busy".into())).await;
            return;
        };
        let Ok(_pair) = self.pairs.clone().try_acquire_owned() else {
            let _ = write_line(&mut tls, &Line::Err("busy".into())).await;
            return;
        };
        let connect = self
            .computers
            .lock()
            .ok()
            .and_then(|computers| computers.get(&target).map(|r| r.connect.clone()));
        let Some(connect) = connect else {
            let _ = write_line(&mut tls, &Line::Err("offline".into())).await;
            return;
        };
        let id = self.next.fetch_add(1, Ordering::Relaxed);
        let (answer, answered) = oneshot::channel();
        if let Ok(mut waiting) = self.waiting.lock() {
            waiting.insert(id, Waiting { target, answer });
        }
        // Accepted; turned down (it serves all it can, it does not take the
        // computer asking, or its control connection is backed up); or not
        // in time.
        let other = if connect.try_send((id, asking.clone())).is_ok() {
            match tokio::time::timeout(ACCEPT_WAIT, answered).await {
                Ok(Ok(answer)) => answer,
                Ok(Err(_)) => Err("busy"),
                Err(_) => Err("timeout"),
            }
        } else {
            Err("busy")
        };
        let other = match other {
            Ok(other) => other,
            Err(why) => {
                if let Ok(mut waiting) = self.waiting.lock() {
                    waiting.remove(&id);
                }
                let _ = write_line(&mut tls, &Line::Err(why.into())).await;
                return;
            }
        };
        if write_line(&mut tls, &Line::Ok).await.is_err() {
            return;
        }
        tokio::select! {
            () = splice_tapped(tls, other, self.limits.idle, self.tap.as_ref()) => {}
            _ = token.changed() => {}
            _ = stop.changed() => {}
        }
    }
    /// The connection a computer opened for `CONNECT <id>`: handed to the
    /// `DIAL` waiting for it, if that asked for this computer.
    async fn accept(&self, mut tls: Accepted, computer: String, id: u64) {
        let waiting = self.waiting.lock().ok().and_then(|mut waiting| {
            if waiting.get(&id).is_some_and(|w| w.target == computer) {
                waiting.remove(&id)
            } else {
                None
            }
        });
        let Some(waiting) = waiting else {
            return;
        };
        if write_line(&mut tls, &Line::Ok).await.is_ok() {
            let _ = waiting.answer.send(Ok(tls));
        }
    }
}

/// A connection to a relay, TLS done.
pub type Reached = tokio_rustls::client::TlsStream<TcpStream>;
/// A new connection to `relay` as this computer (`tls`: the relay pinned by
/// its fingerprint, this computer's certificate shown).
pub async fn reach(
    relay: &RelayAddress,
    tls: Arc<rustls::ClientConfig>,
) -> std::io::Result<Reached> {
    use rustls::pki_types::ServerName;
    let port = relay.port();
    let (name, addresses) = match relay.host() {
        RelayHost::Ip(ip) => (
            ServerName::IpAddress((*ip).into()),
            vec![SocketAddr::new(*ip, port)],
        ),
        RelayHost::Name(name) => (
            ServerName::try_from(name.clone())
                .map_err(|_| std::io::Error::other("not a host name"))?,
            tokio::net::lookup_host((name.as_str(), port))
                .await?
                .filter(|address| peer_address(address.ip()))
                .collect(),
        ),
    };
    let stream = tokio::time::timeout(CONNECT_TIME, crate::client::connect(addresses))
        .await
        .map_err(|_| std::io::Error::from(std::io::ErrorKind::TimedOut))??;
    let _ = stream.set_nodelay(true);
    tokio::time::timeout(LINE_TIME, TlsConnector::from(tls).connect(name, stream))
        .await
        .map_err(|_| std::io::Error::from(std::io::ErrorKind::TimedOut))?
}
/// A new connection to `relay`, said `line` on, answered `OK` within `wait`.
async fn said(
    relay: &RelayAddress,
    tls: &Arc<rustls::ClientConfig>,
    line: &Line,
    wait: Duration,
) -> Option<Reached> {
    answered(relay, tls, line, wait).await.ok()
}
/// The same, or the reason in the relay's `ERR <reason>` if it said one.
async fn answered(
    relay: &RelayAddress,
    tls: &Arc<rustls::ClientConfig>,
    line: &Line,
    wait: Duration,
) -> Result<Reached, Option<String>> {
    let mut stream = reach(relay, tls.clone()).await.map_err(|_| None)?;
    write_line(&mut stream, line).await.map_err(|_| None)?;
    let answer = tokio::time::timeout(wait, read_line(&mut stream))
        .await
        .map_err(|_| None)?
        .map_err(|_| None)?;
    match parse(&answer) {
        Ok(Line::Ok) => Ok(stream),
        Ok(Line::Err(reason)) => Err(Some(reason)),
        _ => Err(None),
    }
}

/// What a computer makes of one the relay puts through to it, by the
/// certificate that computer showed the relay.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Caller {
    /// Paired with it: up to `per_caller` places of its own.
    Paired,
    /// Not paired, while it is pairing: the strangers share `strangers`
    /// places.
    Stranger,
    /// Not paired, and not pairing: turned down at once (`CLOSED`).
    Refused,
}
/// The places a computer has for connections put through relays to it,
/// shared by every relay it holds: so many in all, so many for each
/// computer it is paired with, and so many for the computers it is not
/// paired with together. `judge` says which a computer is, when it asks.
pub struct Seats {
    slots: Arc<Semaphore>,
    per_caller: usize,
    strangers: usize,
    held: Mutex<HashMap<Option<String>, usize>>,
    judge: Box<dyn Fn(&str) -> Caller + Send + Sync>,
}
/// A place a connection put through a relay holds while it is served.
pub struct Seat {
    seats: Arc<Seats>,
    /// The computer it is for, or none for the strangers' places.
    key: Option<String>,
    /// The computer the relay put through (its certificate's fingerprint).
    caller: String,
    _slot: OwnedSemaphorePermit,
}
/// Why a computer asking was turned down.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TurnedDown {
    /// No place left, in all or for it (`BUSY`).
    Busy,
    /// Not taken at all (`CLOSED`).
    Closed,
}
impl Seats {
    pub fn new(
        total: usize,
        per_caller: usize,
        strangers: usize,
        judge: impl Fn(&str) -> Caller + Send + Sync + 'static,
    ) -> Arc<Self> {
        Arc::new(Self {
            slots: Arc::new(Semaphore::new(total)),
            per_caller,
            strangers,
            held: Mutex::default(),
            judge: Box::new(judge),
        })
    }
    /// A place for a connection from `caller`, if it may have one now.
    pub fn take(self: &Arc<Self>, caller: &str) -> Result<Seat, TurnedDown> {
        let (key, most) = match (self.judge)(caller) {
            Caller::Paired => (Some(caller.to_owned()), self.per_caller),
            Caller::Stranger => (None, self.strangers),
            Caller::Refused => return Err(TurnedDown::Closed),
        };
        let mut held = self.held.lock().map_err(|_| TurnedDown::Busy)?;
        if held.get(&key).is_some_and(|count| *count >= most) {
            return Err(TurnedDown::Busy);
        }
        let slot = self
            .slots
            .clone()
            .try_acquire_owned()
            .map_err(|_| TurnedDown::Busy)?;
        *held.entry(key.clone()).or_default() += 1;
        Ok(Seat {
            seats: self.clone(),
            key,
            caller: caller.to_owned(),
            _slot: slot,
        })
    }
    /// Places held now.
    pub fn held(&self) -> usize {
        self.held
            .lock()
            .map(|held| held.values().sum())
            .unwrap_or_default()
    }
}
impl Seat {
    /// The computer the relay put through: a certificate shown inside must
    /// be this one.
    pub fn caller(&self) -> &str {
        &self.caller
    }
}
impl Drop for Seat {
    fn drop(&mut self) {
        if let Ok(mut held) = self.seats.held.lock()
            && let Some(count) = held.get_mut(&self.key)
        {
            *count -= 1;
            if *count == 0 {
                held.remove(&self.key);
            }
        }
    }
}

/// What a computer holding a relay is told: whether it holds it now, and
/// each connection put through to it, with its seat (held while it is
/// served).
pub type Held = Arc<dyn Fn(bool) + Send + Sync>;
pub type Arrived = Arc<dyn Fn(Reached, Seat) + Send + Sync>;
/// Hold a control connection to `relay` until `stop`, connecting again
/// after a second whenever it is lost, and waiting longer (up to a minute)
/// each time that fails. Each connection put through to this computer is
/// handed to `arrived`, as it comes: TLS from the computer that asked,
/// to be served like any other connection from another computer. `seats`
/// bounds how many it opens and serves at once, in all and for each
/// computer asking; past that the computer asking is turned down (`BUSY`),
/// and one it does not take at all is too (`CLOSED`).
pub async fn hold(
    relay: RelayAddress,
    tls: Arc<rustls::ClientConfig>,
    seats: Arc<Seats>,
    arrived: Arrived,
    held: Held,
    mut stop: watch::Receiver<bool>,
) {
    let mut wait = BACKOFF_FIRST;
    loop {
        if *stop.borrow() {
            return;
        }
        let registered = tokio::select! {
            registered = control(&relay, &tls, &seats, &arrived, &held) => registered,
            _ = stop.changed() => { held(false); return }
        };
        held(false);
        if registered {
            wait = BACKOFF_FIRST;
        }
        tokio::select! {
            () = tokio::time::sleep(wait) => {}
            _ = stop.changed() => return,
        }
        if !registered {
            wait = (wait * 2).min(BACKOFF_MOST);
        }
    }
}
/// One control connection, until it is lost: whether the relay took it.
async fn control(
    relay: &RelayAddress,
    tls: &Arc<rustls::ClientConfig>,
    seats: &Arc<Seats>,
    arrived: &Arrived,
    held: &Held,
) -> bool {
    let listen = Line::Listen {
        token: relay.token().to_owned(),
    };
    let Some(stream) = said(relay, tls, &listen, LINE_TIME).await else {
        return false;
    };
    held(true);
    let (mut reader, mut writer) = tokio::io::split(stream);
    let (heard, mut hearing) = mpsc::channel(8);
    let reading = tokio::spawn(async move {
        loop {
            let line = read_line(&mut reader).await.and_then(|line| parse(&line));
            let ended = line.is_err();
            if heard.send(line).await.is_err() || ended {
                return;
            }
        }
    });
    let mut ping = tokio::time::interval_at(tokio::time::Instant::now() + PING_EVERY, PING_EVERY);
    let mut pong_by: Option<tokio::time::Instant> = None;
    loop {
        let overdue = async {
            match pong_by {
                Some(at) => tokio::time::sleep_until(at).await,
                None => std::future::pending().await,
            }
        };
        tokio::select! {
            line = hearing.recv() => match line {
                Some(Ok(Line::Connect { id, fingerprint })) => {
                    let seat = match seats.take(&fingerprint) {
                        Ok(seat) => seat,
                        Err(turned) => {
                            let answer = match turned {
                                TurnedDown::Busy => Line::Busy { id },
                                TurnedDown::Closed => Line::Closed { id },
                            };
                            if write_line(&mut writer, &answer).await.is_err() { break }
                            continue;
                        }
                    };
                    let (relay, tls, arrived) = (relay.clone(), tls.clone(), arrived.clone());
                    tokio::spawn(async move {
                        let accept = Line::Accept { token: relay.token().to_owned(), id };
                        if let Some(stream) = said(&relay, &tls, &accept, LINE_TIME).await {
                            arrived(stream, seat);
                        }
                    });
                }
                Some(Ok(Line::Pong)) => pong_by = None,
                _ => break,
            },
            _ = ping.tick() => {
                if write_line(&mut writer, &Line::Ping).await.is_err() { break }
                pong_by.get_or_insert(tokio::time::Instant::now() + PONG_WAIT);
            }
            () = overdue => break,
        }
    }
    reading.abort();
    true
}

/// How a client sends HTTP over a connection put through a relay.
pub(crate) type Sender = hyper::client::conn::http1::SendRequest<axum::body::Body>;
/// A relay, a computer behind it, and whether this computer shows its own
/// certificate there (not for the pairing handshake).
pub(crate) type Through = (String, String, bool);
/// Dials computers through relays, in this process: each connection is TLS
/// to the relay and `DIAL`, and TLS with the computer's pinned certificate
/// inside it. Connections are kept a while for the next call to the same
/// computer through the same relay, so a call there costs one round trip
/// once one has been made.
pub struct RelayDialer {
    instance: Arc<Instance>,
    kept: Mutex<HashMap<Through, Vec<(u64, Sender)>>>,
    next: AtomicU64,
    stop: watch::Receiver<bool>,
}
impl RelayDialer {
    pub fn new(instance: Arc<Instance>, stop: watch::Receiver<bool>) -> Arc<Self> {
        Arc::new(Self {
            instance,
            kept: Mutex::default(),
            next: AtomicU64::new(0),
            stop,
        })
    }
    /// A new connection to the computer with `fingerprint` through `relay`:
    /// TLS to the relay, `DIAL`, TLS with that computer (`inner`, which pins
    /// its certificate), ready for HTTP. Or where it stopped: the relay did
    /// not put it through (`NOT_RELAYED`), the computer turned this one down
    /// (`TURNED_DOWN`: not paired with it, and not pairing), or the computer
    /// behind it is not the one meant (`ANOTHER`).
    pub(crate) async fn open(
        &self,
        relay: &RelayAddress,
        fingerprint: &str,
        inner: Arc<rustls::ClientConfig>,
    ) -> Result<Sender, &'static str> {
        let outer = Arc::new(
            self.instance
                .client_config(relay.fingerprint(), false)
                .map_err(|_| NOT_RELAYED)?,
        );
        let dial = Line::Dial {
            token: relay.token().to_owned(),
            fingerprint: fingerprint.to_owned(),
        };
        let through = answered(relay, &outer, &dial, ACCEPT_WAIT + LINE_TIME)
            .await
            .map_err(|why| match why.as_deref() {
                Some("closed") => TURNED_DOWN,
                _ => NOT_RELAYED,
            })?;
        let name = rustls::pki_types::ServerName::try_from("localhost").map_err(|_| NOT_RELAYED)?;
        let tls = TlsConnector::from(inner)
            .connect(name, through)
            .await
            .map_err(|error| match reason(&error) {
                Some(ANOTHER) => ANOTHER,
                _ => NOT_RELAYED,
            })?;
        let (sender, connection) =
            hyper::client::conn::http1::handshake(hyper_util::rt::TokioIo::new(tls))
                .await
                .map_err(|_| NOT_RELAYED)?;
        let mut stop = self.stop.clone();
        tokio::spawn(async move {
            tokio::select! {
                _ = connection => {}
                _ = stop.changed() => {}
            }
        });
        Ok(sender)
    }
    /// A kept connection through `through`, if one is still open.
    pub(crate) fn take(&self, through: &Through) -> Option<Sender> {
        let mut kept = self.kept.lock().ok()?;
        let connections = kept.get_mut(through)?;
        while let Some((_, sender)) = connections.pop() {
            if !sender.is_closed() {
                return Some(sender);
            }
        }
        None
    }
    /// Keep `sender` for the next call through `through`, for `KEPT_IDLE`.
    pub(crate) fn keep(self: &Arc<Self>, through: Through, sender: Sender) {
        if sender.is_closed() || tokio::runtime::Handle::try_current().is_err() {
            return;
        }
        let id = self.next.fetch_add(1, Ordering::Relaxed);
        let Ok(mut kept) = self.kept.lock() else {
            return;
        };
        let connections = kept.entry(through.clone()).or_default();
        connections.retain(|(_, sender)| !sender.is_closed());
        if connections.len() >= KEPT_MOST {
            return;
        }
        connections.push((id, sender));
        drop(kept);
        let dialer = Arc::downgrade(self);
        tokio::spawn(async move {
            tokio::time::sleep(KEPT_IDLE).await;
            let Some(dialer) = dialer.upgrade() else {
                return;
            };
            if let Ok(mut kept) = dialer.kept.lock()
                && let Some(connections) = kept.get_mut(&through)
            {
                connections.retain(|(kept, _)| *kept != id);
                if connections.is_empty() {
                    kept.remove(&through);
                }
            }
        });
    }
    /// Connections kept now, through any relay.
    pub fn kept(&self) -> usize {
        self.kept
            .lock()
            .map(|kept| kept.values().map(Vec::len).sum())
            .unwrap_or_default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn token() -> String {
        "ab".repeat(32)
    }

    #[test]
    fn each_line_reads_back_as_written() {
        let fingerprint = "cd".repeat(32);
        for line in [
            Line::Listen { token: token() },
            Line::Accept {
                token: token(),
                id: 18446744073709551615,
            },
            Line::Dial {
                token: token(),
                fingerprint: fingerprint.clone(),
            },
            Line::Connect { id: 7, fingerprint },
            Line::Busy { id: 7 },
            Line::Closed { id: 7 },
            Line::Ok,
            Line::Err("offline".into()),
            Line::Ping,
            Line::Pong,
        ] {
            assert_eq!(parse(&line.to_string()), Ok(line.clone()), "{line:?}");
        }
        // The token never shows in debug output.
        assert!(!format!("{:?}", Line::Listen { token: token() }).contains(&token()));
    }

    #[test]
    fn an_unknown_or_malformed_line_is_refused() {
        for unknown in ["HELLO", "listen abc", "GET / HTTP/1.1", "", "PING "] {
            let parsed = parse(unknown);
            assert!(parsed.is_err(), "{unknown:?} read as {parsed:?}");
        }
        assert_eq!(parse("HELLO there"), Err(LineError::Unknown));
        assert_eq!(parse("GET / HTTP/1.1"), Err(LineError::Unknown));
        assert_eq!(parse("LISTEN"), Err(LineError::Malformed));
        assert_eq!(parse("LISTEN short"), Err(LineError::Malformed));
        assert_eq!(
            parse(&format!("LISTEN {}", token().to_uppercase())),
            Err(LineError::Malformed)
        );
        assert_eq!(
            parse(&format!("LISTEN {} extra", token())),
            Err(LineError::Malformed)
        );
        let fingerprint = "cd".repeat(32);
        assert_eq!(
            parse(&format!("CONNECT -1 {fingerprint}")),
            Err(LineError::Malformed)
        );
        assert_eq!(
            parse(&format!("CONNECT 18446744073709551616 {fingerprint}")),
            Err(LineError::Malformed)
        );
        assert_eq!(parse("CONNECT 7"), Err(LineError::Malformed));
        assert_eq!(parse("CONNECT 7 cd"), Err(LineError::Malformed));
        assert_eq!(parse("BUSY"), Err(LineError::Malformed));
        assert_eq!(parse("CLOSED"), Err(LineError::Malformed));
        assert_eq!(parse("ERR Not-Found"), Err(LineError::Malformed));
        assert_eq!(parse("OK now"), Err(LineError::Malformed));
    }

    #[tokio::test]
    async fn a_line_without_a_newline_in_512_bytes_is_too_long() {
        let (mut ours, mut theirs) = tokio::io::duplex(4096);
        let longest = format!("ERR {}\n", "a".repeat(LINE_BYTES - 5));
        assert_eq!(longest.len(), LINE_BYTES);
        ours.write_all(longest.as_bytes()).await.unwrap();
        assert_eq!(
            read_line(&mut theirs).await,
            Ok(longest.trim_end().to_owned()),
            "512 bytes with the newline is still a line"
        );
        ours.write_all(&[b'a'; LINE_BYTES]).await.unwrap();
        ours.write_all(b"\n").await.unwrap();
        assert_eq!(read_line(&mut theirs).await, Err(LineError::TooLong));
        let (mut ours, mut theirs) = tokio::io::duplex(64);
        ours.write_all(b"PING\x00\n").await.unwrap();
        assert_eq!(read_line(&mut theirs).await, Err(LineError::NotText));
        drop(ours);
        let (ours, mut theirs) = tokio::io::duplex(64);
        drop(ours);
        assert_eq!(read_line(&mut theirs).await, Err(LineError::Closed));
    }

    /// Nothing after the newline is taken: the bytes that follow belong to
    /// whoever reads next (TLS between the two computers).
    #[tokio::test]
    async fn a_line_takes_nothing_after_its_newline() {
        let (mut ours, mut theirs) = tokio::io::duplex(64);
        ours.write_all(b"OK\n\x16\x03\x01rest").await.unwrap();
        assert_eq!(read_line(&mut theirs).await, Ok("OK".to_owned()));
        let mut rest = [0u8; 7];
        theirs.read_exact(&mut rest).await.unwrap();
        assert_eq!(&rest, b"\x16\x03\x01rest");
    }

    /// A line begun and not finished in its time is given up. (A relay's
    /// own five seconds are tested against a real relay, in tests/relay.rs.)
    #[tokio::test]
    async fn a_line_not_finished_in_time_times_out() {
        let (mut ours, mut theirs) = tokio::io::duplex(64);
        ours.write_all(b"PIN").await.unwrap();
        let started = Instant::now();
        let time = Duration::from_millis(200);
        assert_eq!(
            read_command_within(&mut theirs, time).await,
            Err(LineError::TimedOut)
        );
        assert!(started.elapsed() >= time && started.elapsed() < Duration::from_secs(5));
        assert_eq!(LINE_TIME, Duration::from_secs(5));
        assert!(Limits::default().first_line <= Duration::from_secs(5));
    }

    #[test]
    fn each_source_opens_connections_at_its_own_rate() {
        let rate = Rate {
            burst: 3,
            per_second: 2,
        };
        let mut sources = Sources::new(rate);
        let now = Instant::now();
        let one: IpAddr = "203.0.113.5".parse().unwrap();
        let other: IpAddr = "198.51.100.7".parse().unwrap();
        for _ in 0..3 {
            assert!(sources.admit(one, now));
        }
        assert!(!sources.admit(one, now), "the burst is used up");
        assert!(
            !sources.admit("::ffff:203.0.113.5".parse().unwrap(), now),
            "the same address written as IPv6"
        );
        assert!(sources.admit(other, now), "another source has its own");
        // Half a second later, one more.
        let later = now + Duration::from_millis(500);
        assert!(sources.admit(one, later));
        assert!(!sources.admit(one, later));
        // An IPv6 computer counts by its /64.
        let mut sources = Sources::new(rate);
        for host in 1..=3 {
            assert!(sources.admit(format!("2001:db8:1:2::{host}").parse().unwrap(), now));
        }
        assert!(!sources.admit("2001:db8:1:2:ffff::9".parse().unwrap(), now));
        assert!(sources.admit("2001:db8:1:3::1".parse().unwrap(), now));
        // A burst is a small part of what a relay takes before first lines.
        let limits = Limits::default();
        assert!(limits.rate.burst as usize * 4 <= limits.unauthenticated);
        assert!(limits.unauthenticated_per_source <= 16);
    }

    /// A source holds only so many connections before their first line;
    /// with every place taken, the source holding the most gives up its
    /// oldest for one holding fewer, and a source never takes another's
    /// last place.
    #[test]
    fn the_doorway_keeps_room_for_every_source() {
        let busy: IpAddr = "203.0.113.5".parse().unwrap();
        let other: IpAddr = "198.51.100.7".parse().unwrap();
        let third: IpAddr = "2001:db8:1:2::1".parse().unwrap();
        // All at one moment: nobody has had its time yet.
        let now = Instant::now();
        let mut doorway = Doorway::new(6, 4, 32);
        let mut places: Vec<Place> = (0..4)
            .map(|_| doorway.enter_at(busy, now).unwrap())
            .collect();
        assert!(
            doorway.enter_at(busy, now).is_none(),
            "four from one source at most"
        );
        assert!(
            doorway
                .enter_at("::ffff:203.0.113.5".parse().unwrap(), now)
                .is_none(),
            "the same source written as IPv6"
        );
        places.push(doorway.enter_at(other, now).unwrap());
        places.push(doorway.enter_at(other, now).unwrap());
        assert_eq!(doorway.held(), 6);
        // Full: the busy source's oldest place goes to the third source.
        let third_place = doorway.enter_at(third, now).unwrap();
        assert!(
            matches!(
                places[0].closed.try_recv(),
                Err(oneshot::error::TryRecvError::Closed)
            ),
            "the oldest was let go"
        );
        assert!(matches!(
            places[1].closed.try_recv(),
            Err(oneshot::error::TryRecvError::Empty)
        ));
        assert_eq!(doorway.held(), 6);
        // `other` holds 2 and the most anyone holds is 3: not more than it
        // would, and nobody has had its time, so nobody makes room.
        assert!(doorway.enter_at(other, now).is_none());
        // Leaving frees a place; leaving twice frees nothing more.
        doorway.leave(&third_place);
        doorway.leave(&third_place);
        assert_eq!(doorway.held(), 5);
        assert!(doorway.enter_at(other, now).is_some());
        // A place let go to make room is not counted again when it leaves.
        doorway.leave(&places[0]);
        assert_eq!(doorway.held(), 6);
    }

    /// Every place taken by sources holding one each (many addresses, one
    /// connection from each): nobody holds more, so a newcomer waits until
    /// the oldest has had its time for TLS and a first line, then that one
    /// goes; a newcomer is never let in at the cost of a young connection.
    #[test]
    fn with_every_place_taken_one_each_the_oldest_goes_once_it_had_its_time() {
        let start = Instant::now();
        let mut doorway = Doorway::new(4, 16, 32);
        let mut places: Vec<Place> = (1..=4)
            .map(|n| {
                let at = start + Duration::from_millis(100 * n);
                doorway
                    .enter_at(format!("203.0.113.{n}").parse().unwrap(), at)
                    .unwrap()
            })
            .collect();
        let newcomer: IpAddr = "198.51.100.9".parse().unwrap();
        let soon = start + Duration::from_millis(900);
        assert!(
            doorway.enter_at(newcomer, soon).is_none(),
            "a connection still within its time was let go"
        );
        assert_eq!(doorway.held(), 4);
        // The first came at 100 ms: at 1.2 s it has had its second.
        let later = start + Duration::from_millis(1200);
        let let_in = doorway.enter_at(newcomer, later);
        assert!(let_in.is_some(), "the oldest kept its place for good");
        assert!(matches!(
            places[0].closed.try_recv(),
            Err(oneshot::error::TryRecvError::Closed)
        ));
        for place in &mut places[1..] {
            assert!(matches!(
                place.closed.try_recv(),
                Err(oneshot::error::TryRecvError::Empty)
            ));
        }
        assert_eq!(doorway.held(), 4);
        // The next oldest (200 ms) has had its second at 1.2 s too; the
        // newcomer itself (1.2 s) has not.
        assert!(
            doorway
                .enter_at("198.51.100.10".parse().unwrap(), later)
                .is_some()
        );
        assert!(matches!(
            places[1].closed.try_recv(),
            Err(oneshot::error::TryRecvError::Closed)
        ));
        let mut newest = let_in.unwrap();
        let last = start + Duration::from_millis(1500);
        for n in 11..=12 {
            assert!(
                doorway
                    .enter_at(format!("198.51.100.{n}").parse().unwrap(), last)
                    .is_some()
            );
        }
        // Now the newcomers are the oldest, and young.
        assert!(
            doorway
                .enter_at("198.51.100.13".parse().unwrap(), last)
                .is_none()
        );
        assert!(matches!(
            newest.closed.try_recv(),
            Err(oneshot::error::TryRecvError::Empty)
        ));
    }

    /// A site with many /64s (an IPv6 /48) holds only so many places in
    /// all, so it cannot take every place with a new /64 for each
    /// connection; another /48, and IPv4, still get in.
    #[test]
    fn an_ipv6_site_holds_only_so_many_places_across_its_64s() {
        let now = Instant::now();
        let mut doorway = Doorway::new(256, 16, 32);
        let mut held = Vec::new();
        for subnet in 0..32 {
            let ip: IpAddr = format!("2001:db8:5:{subnet:x}::1").parse().unwrap();
            held.push(doorway.enter_at(ip, now).unwrap());
        }
        assert!(
            doorway
                .enter_at("2001:db8:5:ff::1".parse().unwrap(), now)
                .is_none(),
            "a 33rd from the same /48"
        );
        assert!(
            doorway
                .enter_at("2001:db8:6::1".parse().unwrap(), now)
                .is_some(),
            "another /48"
        );
        assert!(
            doorway
                .enter_at("203.0.113.1".parse().unwrap(), now)
                .is_some()
        );
        // A place it leaves is its again.
        doorway.leave(&held[0]);
        assert!(
            doorway
                .enter_at("2001:db8:5:ff::1".parse().unwrap(), now)
                .is_some()
        );
        assert_eq!(doorway.held(), 34);
    }

    /// A computer's places for connections through relays: so many for each
    /// computer it is paired with, so many for all the others together
    /// while it is pairing, none at all for them otherwise; a seat given
    /// back is free again.
    #[test]
    fn a_computer_keeps_places_for_each_paired_computer() {
        let paired = "aa".repeat(32);
        let other_paired = "bb".repeat(32);
        let pairing = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let open = pairing.clone();
        let seats = Seats::new(6, 2, 2, move |caller: &str| {
            if caller == "aa".repeat(32) || caller == "bb".repeat(32) {
                Caller::Paired
            } else if open.load(Ordering::SeqCst) {
                Caller::Stranger
            } else {
                Caller::Refused
            }
        });
        let stranger = "cc".repeat(32);
        assert!(matches!(seats.take(&stranger), Err(TurnedDown::Closed)));
        let mine: Vec<Seat> = (0..2).map(|_| seats.take(&paired).unwrap()).collect();
        assert!(matches!(seats.take(&paired), Err(TurnedDown::Busy)));
        let theirs = seats.take(&other_paired).unwrap();
        assert_eq!(theirs.caller(), other_paired);
        pairing.store(true, Ordering::SeqCst);
        let strangers: Vec<Seat> = (0..2)
            .map(|n| seats.take(&format!("{n:0>64}")).unwrap())
            .collect();
        assert!(
            matches!(seats.take(&stranger), Err(TurnedDown::Busy)),
            "the strangers' places are shared"
        );
        assert!(seats.take(&other_paired).is_ok(), "theirs are their own");
        assert_eq!(seats.held(), 5);
        drop(mine);
        assert!(seats.take(&paired).is_ok());
        drop(strangers);
        assert!(seats.take(&stranger).is_ok());
        drop(theirs);
        assert_eq!(seats.held(), 0);
        // So many in all, whoever holds them.
        let seats = Seats::new(3, 2, 2, |_: &str| Caller::Paired);
        let _held: Vec<Seat> = [&paired, &paired, &other_paired]
            .into_iter()
            .map(|caller| seats.take(caller).unwrap())
            .collect();
        assert!(matches!(seats.take(&other_paired), Err(TurnedDown::Busy)));
    }

    /// A pair passing nothing for its idle time is closed; one passing
    /// bytes is not.
    #[tokio::test]
    async fn a_quiet_pair_is_closed_after_its_idle_time() {
        let (mut a, a_far) = tokio::io::duplex(1024);
        let (mut b, b_far) = tokio::io::duplex(1024);
        let idle = Duration::from_millis(300);
        let spliced = tokio::spawn(splice(a_far, b_far, idle));
        for _ in 0..4 {
            a.write_all(b"ping").await.unwrap();
            let mut got = [0u8; 4];
            b.read_exact(&mut got).await.unwrap();
            assert_eq!(&got, b"ping");
            tokio::time::sleep(Duration::from_millis(150)).await;
        }
        assert!(!spliced.is_finished(), "closed while bytes were passing");
        let started = Instant::now();
        tokio::time::timeout(Duration::from_secs(5), spliced)
            .await
            .expect("closed once quiet")
            .unwrap();
        assert!(started.elapsed() >= Duration::from_millis(150));
        let mut rest = Vec::new();
        assert_eq!(
            b.read_to_end(&mut rest).await.unwrap(),
            0,
            "the other end sees it end"
        );
    }
}
