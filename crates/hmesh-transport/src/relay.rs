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
//! `LINE_BYTES` long and within `LINE_TIME`:
//!
//! - `LISTEN <token>`: this computer may be put through to (`OK`). The relay
//!   then says `CONNECT <id>` on this connection for each computer asking
//!   for it; the computer says `PING` every 30 seconds and the relay `PONG`.
//! - `ACCEPT <token> <id>`: a new connection from that computer, for
//!   `CONNECT <id>` (`OK`); from here on it carries the asking computer's bytes.
//! - `DIAL <token> <fingerprint>`: put this connection through to that
//!   computer: `OK` once it has accepted, or `ERR <reason>` (`offline`,
//!   `busy`, `timeout`, `full` for a relay with no room for another computer).
//!
//! A wrong token, or anything else, closes the connection at once, without
//! a word. Limits: `Limits`. Each source address (an IPv6 one by its /64)
//! opens connections at a bounded rate.
use hmesh_core::{RelayAddress, RelayHost};
use hmesh_security::{
    Instance, fingerprint, peer_address, relay::RelayIdentity, relay::same_secret,
};
use std::{
    collections::HashMap,
    net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr},
    sync::{
        Arc, Mutex,
        atomic::{AtomicU64, AtomicUsize, Ordering},
    },
    time::{Duration, Instant},
};
use tokio::{
    io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt},
    net::{TcpListener, TcpStream},
    sync::{Semaphore, mpsc, oneshot, watch},
    task::JoinSet,
};
use tokio_rustls::{TlsAcceptor, TlsConnector};

/// The longest line, its newline included.
pub const LINE_BYTES: usize = 512;
/// How long a connection has to complete TLS and say its first line, and
/// how long any later line may take once it has begun.
pub const LINE_TIME: Duration = Duration::from_secs(10);
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
/// A loopback port forwarding to a computer through a relay closes after
/// this long without a connection.
pub const FORWARD_IDLE: Duration = Duration::from_secs(600);
/// A computer that loses its control connection tries again after a second,
/// then waits twice as long each time it fails, up to a minute.
const BACKOFF_FIRST: Duration = Duration::from_secs(1);
const BACKOFF_MOST: Duration = Duration::from_secs(60);

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
    Connect { id: u64 },
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
            Self::Connect { id } => write!(f, "CONNECT {id}"),
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
        "LISTEN", "ACCEPT", "DIAL", "CONNECT", "OK", "ERR", "PING", "PONG",
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
        ("CONNECT", [id]) => Line::Connect { id: number(id)? },
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
    /// Pairs of connections put through (or waiting to be) at once.
    pub pairs: usize,
    /// A pair passing nothing either way for this long is closed.
    pub idle: Duration,
    /// Connections that have not said their first line yet.
    pub unauthenticated: usize,
    /// New connections from one source address.
    pub rate: Rate,
}
impl Default for Limits {
    fn default() -> Self {
        Self {
            registrations: 256,
            pairs: 512,
            idle: Duration::from_secs(15 * 60),
            unauthenticated: 256,
            rate: Rate {
                burst: 200,
                per_second: 10,
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
    /// Whether a new connection from `ip` is let in at `now`. IPv6
    /// addresses count by their /64: one computer can have many of them.
    pub fn admit(&mut self, ip: IpAddr, now: Instant) -> bool {
        let source = match ip.to_canonical() {
            IpAddr::V6(ip) => {
                let segments = ip.segments();
                IpAddr::V6(Ipv6Addr::new(
                    segments[0],
                    segments[1],
                    segments[2],
                    segments[3],
                    0,
                    0,
                    0,
                    0,
                ))
            }
            v4 => v4,
        };
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
    unauthenticated: Arc<Semaphore>,
    sources: Mutex<Sources>,
    tap: Option<Tap>,
}
struct Registered {
    /// Tells the computers apart over time: a computer that registers
    /// again replaces its older connection.
    id: u64,
    connect: mpsc::Sender<u64>,
}
struct Waiting {
    target: String,
    answer: oneshot::Sender<Accepted>,
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
        unauthenticated: Arc::new(Semaphore::new(limits.unauthenticated)),
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
                let Ok(permit) = hub.unauthenticated.clone().try_acquire_owned() else {
                    continue;
                };
                let _ = stream.set_nodelay(true);
                let hub = hub.clone();
                let acceptor = acceptor.clone();
                let stop = shutdown.clone();
                connections.spawn(async move {
                    hub.connection(stream, acceptor, permit, stop).await;
                });
            }
            Some(_) = connections.join_next(), if !connections.is_empty() => {}
        }
    }
    connections.abort_all();
    while connections.join_next().await.is_some() {}
    Ok(())
}
impl Hub {
    fn registered(&self) -> usize {
        self.computers.lock().map(|c| c.len()).unwrap_or_default()
    }
    async fn connection(
        self: Arc<Self>,
        stream: TcpStream,
        acceptor: TlsAcceptor,
        permit: tokio::sync::OwnedSemaphorePermit,
        stop: watch::Receiver<bool>,
    ) {
        // TLS and the first line, within LINE_TIME together.
        let first = tokio::time::timeout(LINE_TIME, async {
            let mut tls = acceptor.accept(stream).await.ok()?;
            let seen = tls
                .get_ref()
                .1
                .peer_certificates()?
                .first()
                .map(|cert| fingerprint(cert.as_ref()))?;
            let line = read_line(&mut tls).await.ok()?;
            Some((tls, seen, line))
        })
        .await
        .ok()
        .flatten();
        drop(permit);
        let Some((tls, seen, line)) = first else {
            return;
        };
        let token = self.token.subscribe();
        // A wrong token, an unknown command or a malformed one: closed at
        // once, and nothing says which.
        match parse(&line) {
            Ok(Line::Listen { token: given }) if self.token.matches(&given) => {
                self.listen(tls, seen, token, stop).await
            }
            Ok(Line::Dial {
                token: given,
                fingerprint,
            }) if self.token.matches(&given) => self.dial(tls, fingerprint, token, stop).await,
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
        mut token: watch::Receiver<String>,
        mut stop: watch::Receiver<bool>,
    ) {
        let id = self.next.fetch_add(1, Ordering::Relaxed);
        let (connect, mut connects) = mpsc::channel(32);
        let taken = match self.computers.lock() {
            Ok(mut computers)
                if computers.contains_key(&computer)
                    || computers.len() < self.limits.registrations =>
            {
                // An older connection of the same computer ends: its channel closes.
                computers.insert(computer.clone(), Registered { id, connect });
                true
            }
            _ => false,
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
                        Some(id) => if write_line(&mut writer, &Line::Connect { id }).await.is_err() { break },
                        None => break,
                    },
                    line = hearing.recv() => match line {
                        Some(Ok(Line::Ping)) => {
                            silence.as_mut().reset(tokio::time::Instant::now() + SILENT_CONTROL);
                            if write_line(&mut writer, &Line::Pong).await.is_err() { break }
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
    /// Put a connection through to `target`, once it accepts.
    async fn dial(
        &self,
        mut tls: Accepted,
        target: String,
        mut token: watch::Receiver<String>,
        mut stop: watch::Receiver<bool>,
    ) {
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
        let other = if connect.try_send(id).is_ok() {
            tokio::time::timeout(ACCEPT_WAIT, answered)
                .await
                .ok()
                .and_then(Result::ok)
        } else {
            None
        };
        let Some(other) = other else {
            if let Ok(mut waiting) = self.waiting.lock() {
                waiting.remove(&id);
            }
            let _ = write_line(&mut tls, &Line::Err("timeout".into())).await;
            return;
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
            let _ = waiting.answer.send(tls);
        }
    }
}

type Reached = tokio_rustls::client::TlsStream<TcpStream>;
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
/// A new connection to `relay`, said `line` on, answered `OK`.
async fn said(
    relay: &RelayAddress,
    tls: &Arc<rustls::ClientConfig>,
    line: &Line,
    wait: Duration,
) -> Option<Reached> {
    let mut stream = reach(relay, tls.clone()).await.ok()?;
    write_line(&mut stream, line).await.ok()?;
    let answer = tokio::time::timeout(wait, read_line(&mut stream))
        .await
        .ok()?
        .ok()?;
    (parse(&answer).ok()? == Line::Ok).then_some(stream)
}

/// What a computer holding a relay is told: whether it holds it now, and
/// each connection put through to it.
pub type Held = Arc<dyn Fn(bool) + Send + Sync>;
pub type Arrived = Arc<dyn Fn(Reached) + Send + Sync>;
/// Hold a control connection to `relay` until `stop`, connecting again
/// after a second whenever it is lost, and waiting longer (up to a minute)
/// each time that fails. Each connection put through to this computer is
/// handed to `arrived`, as it comes: TLS from the computer that asked,
/// to be served like any other connection from another computer.
pub async fn hold(
    relay: RelayAddress,
    tls: Arc<rustls::ClientConfig>,
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
            registered = control(&relay, &tls, &arrived, &held) => registered,
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
                Some(Ok(Line::Connect { id })) => {
                    let (relay, tls, arrived) = (relay.clone(), tls.clone(), arrived.clone());
                    tokio::spawn(async move {
                        let accept = Line::Accept { token: relay.token().to_owned(), id };
                        if let Some(stream) = said(&relay, &tls, &accept, LINE_TIME).await {
                            arrived(stream);
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

/// Loopback ports that reach other computers through relays: each port
/// forwards to one computer through one relay, so a client dials it like
/// any address and still speaks TLS with that computer's pinned
/// certificate. A port takes connections from loopback only, and closes
/// once nothing has used it for `FORWARD_IDLE`.
pub struct RelayDialer {
    instance: Arc<Instance>,
    forwards: Mutex<HashMap<(String, String), Forward>>,
    stop: watch::Receiver<bool>,
}
struct Forward {
    port: u16,
    /// When it was last handed out or used.
    touched: Arc<Mutex<Instant>>,
}
impl RelayDialer {
    pub fn new(instance: Arc<Instance>, stop: watch::Receiver<bool>) -> Arc<Self> {
        Arc::new(Self {
            instance,
            forwards: Mutex::default(),
            stop,
        })
    }
    /// The loopback port that reaches the computer with `fingerprint`
    /// through `relay`, opened now if there is none.
    pub fn port(self: &Arc<Self>, relay: &RelayAddress, fingerprint: &str) -> anyhow::Result<u16> {
        let key = (relay.to_string(), fingerprint.to_owned());
        let mut forwards = self
            .forwards
            .lock()
            .map_err(|_| anyhow::anyhow!("relay ports poisoned"))?;
        if let Some(forward) = forwards.get(&key) {
            if let Ok(mut touched) = forward.touched.lock() {
                *touched = Instant::now();
            }
            return Ok(forward.port);
        }
        if tokio::runtime::Handle::try_current().is_err() {
            anyhow::bail!("relays are dialed from within the daemon's runtime");
        }
        let tls = Arc::new(self.instance.client_config(relay.fingerprint(), false)?);
        let listener = std::net::TcpListener::bind((Ipv4Addr::LOCALHOST, 0))?;
        listener.set_nonblocking(true)?;
        let listener = TcpListener::from_std(listener)?;
        let port = listener.local_addr()?.port();
        let touched = Arc::new(Mutex::new(Instant::now()));
        forwards.insert(
            key.clone(),
            Forward {
                port,
                touched: touched.clone(),
            },
        );
        let dialer = self.clone();
        let (relay, fingerprint) = (relay.clone(), fingerprint.to_owned());
        tokio::spawn(async move {
            dialer
                .forward(listener, key, relay, fingerprint, tls, touched)
                .await
        });
        Ok(port)
    }
    async fn forward(
        self: Arc<Self>,
        listener: TcpListener,
        key: (String, String),
        relay: RelayAddress,
        fingerprint: String,
        tls: Arc<rustls::ClientConfig>,
        touched: Arc<Mutex<Instant>>,
    ) {
        let active = Arc::new(AtomicUsize::new(0));
        let mut stop = self.stop.clone();
        let touch = |touched: &Mutex<Instant>| {
            if let Ok(mut touched) = touched.lock() {
                *touched = Instant::now();
            }
        };
        loop {
            tokio::select! {
                accepted = listener.accept() => {
                    let Ok((local, from)) = accepted else { continue };
                    if !from.ip().is_loopback() {
                        continue;
                    }
                    touch(&touched);
                    active.fetch_add(1, Ordering::SeqCst);
                    let (relay, fingerprint, tls) = (relay.clone(), fingerprint.clone(), tls.clone());
                    let (active, touched) = (active.clone(), touched.clone());
                    tokio::spawn(async move {
                        through(local, &relay, fingerprint, &tls).await;
                        touch(&touched);
                        active.fetch_sub(1, Ordering::SeqCst);
                    });
                }
                () = tokio::time::sleep(Duration::from_secs(30).min(FORWARD_IDLE)) => {
                    let Ok(mut forwards) = self.forwards.lock() else { return };
                    let idle = touched.lock().is_ok_and(|t| t.elapsed() >= FORWARD_IDLE);
                    if idle && active.load(Ordering::SeqCst) == 0 {
                        forwards.remove(&key);
                        return;
                    }
                }
                changed = stop.changed() => if changed.is_err() || *stop.borrow() { return },
            }
        }
    }
}
/// One connection from loopback, put through `relay` to the computer with
/// `fingerprint`. Closed at once when the relay does not put it through.
async fn through(
    local: TcpStream,
    relay: &RelayAddress,
    fingerprint: String,
    tls: &Arc<rustls::ClientConfig>,
) {
    let dial = Line::Dial {
        token: relay.token().to_owned(),
        fingerprint,
    };
    if let Some(stream) = said(relay, tls, &dial, ACCEPT_WAIT + LINE_TIME).await {
        let _ = local.set_nodelay(true);
        splice(local, stream, Limits::default().idle).await;
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
                fingerprint,
            },
            Line::Connect { id: 7 },
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
        assert_eq!(parse("CONNECT -1"), Err(LineError::Malformed));
        assert_eq!(
            parse("CONNECT 18446744073709551616"),
            Err(LineError::Malformed)
        );
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
    /// own ten seconds are tested against a real relay, in tests/relay.rs.)
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
        assert_eq!(LINE_TIME, Duration::from_secs(10));
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
