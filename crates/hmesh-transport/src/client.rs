//! Calling another computer, or this computer's own daemon. A paired
//! computer may be known by several addresses: they are tried in turn, and
//! where it answered is tried first next time.
use hmesh_core::{ErrorCode, MeshError, NodeCard};
use hmesh_security::{
    Instance, normalize_fingerprint, peer_address, validate_endpoint, validate_peer_address,
};
use rustls::pki_types::ServerName;
use serde::{Serialize, de::DeserializeOwned};
use std::{
    collections::{HashMap, HashSet},
    error::Error as StdError,
    future::Future,
    net::SocketAddr,
    pin::Pin,
    sync::{
        Arc, Mutex,
        atomic::{AtomicU8, Ordering},
    },
    task::{Context, Poll},
    time::Duration,
};
use tokio::{net::TcpStream, sync::Notify, task::JoinSet};

/// How long a connection may take: resolving a host name, TCP and TLS.
const CONNECT: Duration = Duration::from_secs(3);
/// How long the address tried first is tried alone before the others are
/// probed (a TLS handshake each, nothing sent), so a call that connects at
/// once costs nothing more. The first address keeps its whole `CONNECT`: it
/// is given up only for another address that has completed a handshake as
/// the computer that is meant, and only while it has not connected itself.
/// So a slow but working way to the computer (a relayed path, a lossy
/// Wi-Fi) is waited for, an address that sends nothing back costs about
/// `HEAD_START` and a handshake, and a request is never sent twice.
const HEAD_START: Duration = Duration::from_millis(200);
/// A route that keeps answering is written down again at most this often.
const PERSIST_EVERY_SECONDS: i64 = 60;

type Persist = Box<dyn Fn(&str, &str, i64) + Send + Sync>;
/// Where each paired computer answered last, by certificate fingerprint:
/// tried first next time. Kept in memory; `persisted` also hands each new
/// route (fingerprint, origin, unix seconds) to a function that keeps it, so
/// a restarted daemon begins there too.
#[derive(Default)]
pub struct RouteMemory {
    routes: Mutex<HashMap<String, (String, i64)>>,
    persist: Option<Persist>,
}
impl RouteMemory {
    pub fn new() -> Self {
        Self::default()
    }
    pub fn persisted(persist: impl Fn(&str, &str, i64) + Send + Sync + 'static) -> Self {
        Self {
            routes: Mutex::default(),
            persist: Some(Box::new(persist)),
        }
    }
    /// The origin where the computer with this fingerprint answered last.
    pub fn last_good(&self, fingerprint: &str) -> Option<String> {
        let routes = self.routes.lock().ok()?;
        routes.get(fingerprint).map(|(origin, _)| origin.clone())
    }
    fn remember(&self, fingerprint: &str, origin: &str) {
        let at = hmesh_core::now();
        let changed = match self.routes.lock() {
            Ok(mut routes) => {
                let unchanged = routes.get(fingerprint).is_some_and(|(known, when)| {
                    known == origin && at - when < PERSIST_EVERY_SECONDS
                });
                if !unchanged {
                    routes.insert(fingerprint.to_owned(), (origin.to_owned(), at));
                }
                !unchanged
            }
            Err(_) => false,
        };
        if changed && let Some(persist) = &self.persist {
            persist(fingerprint, origin, at);
        }
    }
}

#[derive(Clone)]
pub struct Client {
    dial: Arc<Dial>,
}
enum Dial {
    /// One address: this computer's own daemon, or a computer reached by
    /// the address it was given (a pairing handshake).
    One {
        origin: String,
        http: reqwest::Client,
        owner_token: Option<String>,
    },
    /// A paired computer, at whichever of its addresses answers.
    Routed(Routed),
}
/// A paired computer's addresses. Where it answers is remembered under its
/// fingerprint; `tls` pins its certificate, for each try and each probe.
struct Routed {
    routes: Vec<Route>,
    memory: Arc<RouteMemory>,
    fingerprint: String,
    tls: Arc<rustls::ClientConfig>,
}
struct Route {
    origin: String,
    target: Target,
}
#[derive(Clone)]
enum Target {
    Ip(SocketAddr),
    Name(String, u16),
}
impl Target {
    fn of(url: &url::Url) -> anyhow::Result<Self> {
        let port = url
            .port_or_known_default()
            .ok_or_else(|| anyhow::anyhow!("address without a port"))?;
        Ok(match url.host() {
            Some(url::Host::Ipv4(ip)) => Self::Ip(SocketAddr::new(ip.into(), port)),
            Some(url::Host::Ipv6(ip)) => Self::Ip(SocketAddr::new(ip.into(), port)),
            Some(url::Host::Domain(name)) => Self::Name(name.to_owned(), port),
            None => anyhow::bail!("address without a host"),
        })
    }
    fn loopback(&self) -> bool {
        match self {
            Self::Ip(address) => address.ip().is_loopback(),
            Self::Name(name, _) => name == "localhost",
        }
    }
}
impl Client {
    pub fn new(
        instance: &Instance,
        endpoint: &str,
        expected_fingerprint: &str,
        bootstrap: bool,
        owner: bool,
    ) -> anyhow::Result<Self> {
        let url = validate_endpoint(endpoint)?;
        let target = Target::of(&url)?;
        if !target.loopback() && (owner || instance.config.node.network == "loopback") {
            anyhow::bail!(
                "other computers are reached from a tailscale or direct instance only; owner credentials remain loopback-only"
            );
        }
        let config = instance.client_config(expected_fingerprint, bootstrap)?;
        Ok(Self {
            dial: Arc::new(Dial::One {
                origin: url.origin().ascii_serialization(),
                http: builder(config, CONNECT).build()?,
                owner_token: owner.then(|| instance.owner_token().to_owned()),
            }),
        })
    }
    /// A paired computer at any of `candidates`, in order (its card's
    /// address, host names, addresses set by hand...). Host names resolve
    /// to the addresses `peer_address` permits; whoever answers must show
    /// the certificate `fingerprint` names. A call moves on from an address
    /// only while nothing was sent there (no connection, or the wrong
    /// certificate), so nothing runs twice. The first address tried gets
    /// the whole time to connect; if it has not connected after
    /// `HEAD_START`, the others are probed, and the first to complete a
    /// handshake is used instead.
    pub fn routed(
        instance: &Instance,
        candidates: Vec<String>,
        fingerprint: &str,
        memory: Arc<RouteMemory>,
    ) -> anyhow::Result<Self> {
        Self::route(instance, candidates, fingerprint, memory, false)
    }
    /// The same, without showing this computer's certificate: for the
    /// pairing handshake's `hello`, which also answers a computer that no
    /// longer trusts this one.
    pub fn routed_anonymous(
        instance: &Instance,
        candidates: Vec<String>,
        fingerprint: &str,
        memory: Arc<RouteMemory>,
    ) -> anyhow::Result<Self> {
        Self::route(instance, candidates, fingerprint, memory, true)
    }
    fn route(
        instance: &Instance,
        candidates: Vec<String>,
        fingerprint: &str,
        memory: Arc<RouteMemory>,
        bootstrap: bool,
    ) -> anyhow::Result<Self> {
        let fingerprint = normalize_fingerprint(fingerprint)?;
        let config = instance.client_config(&fingerprint, bootstrap)?;
        let loopback_only = instance.config.node.network == "loopback";
        let mut routes = Vec::new();
        let mut seen = HashSet::new();
        for candidate in candidates {
            let Ok(url) = validate_peer_address(&candidate) else {
                continue;
            };
            let origin = url.origin().ascii_serialization();
            let target = Target::of(&url)?;
            if (loopback_only && !target.loopback()) || !seen.insert(origin.clone()) {
                continue;
            }
            routes.push(Route { origin, target });
        }
        if routes.is_empty() {
            anyhow::bail!("this computer has no address to reach that computer at");
        }
        Ok(Self {
            dial: Arc::new(Dial::Routed(Routed {
                routes,
                memory,
                fingerprint,
                tls: Arc::new(config),
            })),
        })
    }
    /// The addresses tried, in the order they are tried first.
    pub fn origins(&self) -> Vec<String> {
        match &*self.dial {
            Dial::One { origin, .. } => vec![origin.clone()],
            Dial::Routed(routed) => routed
                .order()
                .iter()
                .map(|&i| routed.routes[i].origin.clone())
                .collect(),
        }
    }
    /// First contact with a computer whose certificate is not known yet
    /// (passcode pairing): its card, and the fingerprint of the certificate
    /// this connection saw, which the card must name. The caller pins that
    /// fingerprint from here on; nothing is trusted because of this.
    pub async fn learn(
        instance: &Instance,
        endpoint: &str,
    ) -> hmesh_core::Result<(NodeCard, String)> {
        let refused = |error: anyhow::Error| MeshError::input(error.to_string());
        let url = validate_endpoint(endpoint).map_err(refused)?;
        if instance.config.node.network == "loopback"
            && !hmesh_security::endpoint_is_loopback(endpoint)
        {
            return Err(MeshError::input(
                "other computers are reached from a tailscale or direct instance only",
            ));
        }
        let seen = Arc::new(Mutex::new(None));
        let config = instance
            .first_contact_config(seen.clone())
            .map_err(refused)?;
        let client = Self {
            dial: Arc::new(Dial::One {
                origin: url.origin().ascii_serialization(),
                http: builder(config, CONNECT)
                    .build()
                    .map_err(|error| refused(error.into()))?,
                owner_token: None,
            }),
        };
        let card: NodeCard = client.get("/hmesh/v1/hello").await?;
        let seen = seen
            .lock()
            .ok()
            .and_then(|seen| seen.clone())
            .ok_or_else(|| MeshError::new(ErrorCode::PeerUnreachable, "no certificate was seen"))?;
        if card.fingerprint != seen {
            return Err(MeshError::new(
                ErrorCode::Unauthenticated,
                "that computer's card names another certificate than the one it showed",
            ));
        }
        Ok((card, seen))
    }
    pub async fn get<T: DeserializeOwned>(&self, path: &str) -> hmesh_core::Result<T> {
        self.send::<(), T>(reqwest::Method::GET, path, None, None)
            .await
    }
    pub async fn post<B: Serialize, T: DeserializeOwned>(
        &self,
        path: &str,
        body: &B,
    ) -> hmesh_core::Result<T> {
        self.send(reqwest::Method::POST, path, Some(body), None)
            .await
    }
    pub async fn post_with_timeout<B: Serialize, T: DeserializeOwned>(
        &self,
        path: &str,
        body: &B,
        seconds: u64,
    ) -> hmesh_core::Result<T> {
        if !(1..=40).contains(&seconds) {
            return Err(MeshError::input("RPC deadline must be 1..40 seconds"));
        }
        self.send(
            reqwest::Method::POST,
            path,
            Some(body),
            Some(Duration::from_secs(seconds)),
        )
        .await
    }
    async fn send<B: Serialize, T: DeserializeOwned>(
        &self,
        method: reqwest::Method,
        path: &str,
        body: Option<&B>,
        timeout: Option<Duration>,
    ) -> hmesh_core::Result<T> {
        if !path.starts_with('/') || path.starts_with("//") || path.contains(['\r', '\n']) {
            return Err(MeshError::input("invalid RPC path"));
        }
        let body = body
            .map(serde_json::to_vec)
            .transpose()
            .map_err(|_| MeshError::input("RPC body cannot be encoded"))?;
        let call = Call {
            method,
            path,
            body,
            timeout,
        };
        let response = match &*self.dial {
            Dial::One {
                origin,
                http,
                owner_token,
            } => call
                .send(http, origin, owner_token.as_deref())
                .await
                .map_err(transport_error)?,
            Dial::Routed(routed) => routed.call(&call).await?,
        };
        read(response).await
    }
}

/// One request, as it goes to whichever address is used.
struct Call<'a> {
    method: reqwest::Method,
    path: &'a str,
    body: Option<Vec<u8>>,
    timeout: Option<Duration>,
}
impl Call<'_> {
    async fn send(
        &self,
        http: &reqwest::Client,
        origin: &str,
        owner_token: Option<&str>,
    ) -> reqwest::Result<reqwest::Response> {
        let mut request = http.request(self.method.clone(), format!("{origin}{}", self.path));
        if let Some(token) = owner_token {
            request = request.bearer_auth(token);
        }
        if let Some(body) = &self.body {
            request = request
                .header(reqwest::header::CONTENT_TYPE, "application/json")
                .body(body.clone());
        }
        if let Some(timeout) = self.timeout {
            request = request.timeout(timeout);
        }
        request.send().await
    }
}

/// Handshake probes under way: each route's index, and where it completed a
/// handshake as the computer that is meant, or where it stopped.
type Probes = JoinSet<(usize, Result<SocketAddr, &'static str>)>;
/// How the first address tried came out.
enum First {
    Answered(reqwest::Response),
    /// Nothing was sent there; the others are being probed.
    Failed,
    /// Given up before it connected, for this route, which completed a
    /// handshake at this address.
    GivenUp(usize, SocketAddr),
}
impl Routed {
    /// Route indices in the order to try them: where the computer answered
    /// last first, then as given.
    fn order(&self) -> Vec<usize> {
        let mut order: Vec<usize> = (0..self.routes.len()).collect();
        if let Some(last) = self.memory.last_good(&self.fingerprint)
            && let Some(index) = self.routes.iter().position(|r| r.origin == last)
        {
            order.retain(|&i| i != index);
            order.insert(0, index);
        }
        order
    }
    async fn call(&self, call: &Call<'_>) -> hmesh_core::Result<reqwest::Response> {
        let order = self.order();
        let Some((&first, others)) = order.split_first() else {
            return Err(unreachable(&[]));
        };
        let mut failed: Vec<Option<&'static str>> = vec![None; self.routes.len()];
        let mut probes = Probes::new();
        let mut next = match self
            .first(first, others, call, &mut probes, &mut failed)
            .await?
        {
            First::Answered(response) => return Ok(self.used(first, response)),
            First::Failed => None,
            First::GivenUp(index, at) => Some((index, Some(at))),
        };
        // Given up rather than failed, the first is tried once more if
        // nothing else gets through.
        let mut first_again = next.is_some();
        // The others in the order their handshakes complete; one whose
        // handshake failed is not tried.
        loop {
            let (index, at) = match next.take() {
                Some(next) => next,
                None => match probes.join_next().await {
                    Some(Ok((index, Ok(at)))) => (index, Some(at)),
                    Some(Ok((index, Err(why)))) => {
                        failed[index] = Some(why);
                        continue;
                    }
                    Some(Err(_)) => continue,
                    None if first_again => {
                        first_again = false;
                        (first, None)
                    }
                    None => break,
                },
            };
            let gate = Arc::new(Gate::default());
            match self.attempt(index, &gate, at, call).await {
                Ok(response) => return Ok(self.used(index, response)),
                Err(error) if unsent(&error, &gate) => failed[index] = Some(failure(&error)),
                Err(error) => return Err(transport_error(error)),
            }
        }
        let failures: Vec<(String, &'static str)> = order
            .iter()
            .filter_map(|&i| failed[i].map(|why| (self.routes[i].origin.clone(), why)))
            .collect();
        Err(unreachable(&failures))
    }
    /// The first address, alone for `HEAD_START`, then with the others
    /// probed meanwhile. Once it has connected its answer is awaited, however
    /// long that takes, and the probing stops; until then the first probe to
    /// complete a handshake takes over.
    async fn first(
        &self,
        first: usize,
        others: &[usize],
        call: &Call<'_>,
        probes: &mut Probes,
        failed: &mut [Option<&'static str>],
    ) -> hmesh_core::Result<First> {
        let gate = Arc::new(Gate::default());
        let attempt = self.attempt(first, &gate, None, call);
        tokio::pin!(attempt);
        let head_start = tokio::time::sleep(HEAD_START);
        tokio::pin!(head_start);
        let mut waited = others.is_empty();
        let tried = loop {
            tokio::select! {
                // When the first connects in the same turn a probe
                // completes, the first is polled first and keeps its
                // connection; the probe then finds it cannot give it up.
                biased;
                tried = &mut attempt => break tried,
                Some(probed) = probes.join_next(), if !probes.is_empty() => match probed {
                    Ok((index, Ok(at))) => {
                        if gate.give_up() {
                            return Ok(First::GivenUp(index, at));
                        }
                        // It connected just now: its answer is awaited.
                        *probes = Probes::new();
                    }
                    Ok((index, Err(why))) => failed[index] = Some(why),
                    Err(_) => {}
                },
                () = gate.connected.notified(), if !probes.is_empty() => *probes = Probes::new(),
                () = &mut head_start, if !waited => {
                    waited = true;
                    if !gate.is_connected() {
                        self.probe(others, probes);
                    }
                }
            }
        };
        match tried {
            Ok(response) => Ok(First::Answered(response)),
            Err(error) if unsent(&error, &gate) => {
                failed[first] = Some(failure(&error));
                if !waited {
                    self.probe(others, probes);
                }
                Ok(First::Failed)
            }
            Err(error) => Err(transport_error(error)),
        }
    }
    /// One try at one route, with a client of its own whose connection
    /// passes `gate`. `at`: where a probe found the computer, so a host
    /// name is not looked up again.
    async fn attempt(
        &self,
        index: usize,
        gate: &Arc<Gate>,
        at: Option<SocketAddr>,
        call: &Call<'_>,
    ) -> reqwest::Result<reqwest::Response> {
        let route = &self.routes[index];
        let mut builder = builder((*self.tls).clone(), CONNECT)
            .dns_resolver(Arc::new(PeerResolver))
            .connector_layer(Gated(gate.clone()));
        if let (Target::Name(name, _), Some(at)) = (&route.target, at) {
            builder = builder.resolve_to_addrs(name, &[at]);
        }
        call.send(&builder.build()?, &route.origin, None).await
    }
    /// Start a TLS handshake with each of these routes (nothing is sent
    /// over it): which ones answer, as the right computer, and how soon.
    fn probe(&self, routes: &[usize], probes: &mut Probes) {
        for &index in routes {
            let target = self.routes[index].target.clone();
            let tls = self.tls.clone();
            probes.spawn(async move {
                let answered = tokio::time::timeout(CONNECT, handshake(target, tls))
                    .await
                    .unwrap_or(Err(SILENT));
                (index, answered)
            });
        }
    }
    fn used(&self, index: usize, response: reqwest::Response) -> reqwest::Response {
        self.memory
            .remember(&self.fingerprint, &self.routes[index].origin);
        response
    }
}
/// Whether a try that failed never sent its request: it failed to connect,
/// and no connection was handed over for it.
fn unsent(error: &reqwest::Error, gate: &Gate) -> bool {
    error.is_connect() && !gate.is_connected()
}

/// One try at one address: whether its connection was made, or it was given
/// up for another address first. Settled once, whichever comes first: a
/// connection made after the try was given up is dropped before anything is
/// sent over it, and a try that has connected is not given up.
#[derive(Default)]
struct Gate {
    state: AtomicU8,
    /// Woken when the connection is made.
    connected: Notify,
}
const CONNECTING: u8 = 0;
const CONNECTED: u8 = 1;
const GIVEN_UP: u8 = 2;
impl Gate {
    /// A connection is made: whether it may be used.
    fn connect(&self) -> bool {
        let made = self
            .state
            .compare_exchange(CONNECTING, CONNECTED, Ordering::AcqRel, Ordering::Acquire)
            .is_ok();
        if made {
            self.connected.notify_one();
        }
        made
    }
    /// Give the try up: whether it was, not having connected yet.
    fn give_up(&self) -> bool {
        self.state
            .compare_exchange(CONNECTING, GIVEN_UP, Ordering::AcqRel, Ordering::Acquire)
            .is_ok()
    }
    fn is_connected(&self) -> bool {
        self.state.load(Ordering::Acquire) == CONNECTED
    }
}
/// Puts a try's `Gate` between its client and each connection made for it
/// (DNS, TCP and TLS done): HTTP gets the connection only if the gate lets
/// it through.
#[derive(Clone)]
struct Gated(Arc<Gate>);
impl<S> tower_layer::Layer<S> for Gated {
    type Service = GatedConnect<S>;
    fn layer(&self, inner: S) -> GatedConnect<S> {
        GatedConnect {
            inner,
            gate: self.0.clone(),
        }
    }
}
#[derive(Clone)]
struct GatedConnect<S> {
    inner: S,
    gate: Arc<Gate>,
}
type Connecting<T, E> = Pin<Box<dyn Future<Output = Result<T, E>> + Send>>;
impl<S, R> tower_service::Service<R> for GatedConnect<S>
where
    S: tower_service::Service<R>,
    S::Response: Send + 'static,
    S::Error: From<GivenUp> + 'static,
    S::Future: Send + 'static,
{
    type Response = S::Response;
    type Error = S::Error;
    type Future = Connecting<S::Response, S::Error>;
    fn poll_ready(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), S::Error>> {
        self.inner.poll_ready(cx)
    }
    fn call(&mut self, request: R) -> Self::Future {
        let connecting = self.inner.call(request);
        let gate = self.gate.clone();
        Box::pin(async move {
            let connection = connecting.await?;
            if gate.connect() {
                Ok(connection)
            } else {
                drop(connection);
                Err(GivenUp.into())
            }
        })
    }
}
/// The try was given up for another address before its connection was made.
#[derive(Debug)]
struct GivenUp;
impl std::fmt::Display for GivenUp {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("another address answered first")
    }
}
impl StdError for GivenUp {}

async fn read<T: DeserializeOwned>(mut response: reqwest::Response) -> hmesh_core::Result<T> {
    let status = response.status();
    let mut bytes = Vec::new();
    while let Some(chunk) = response.chunk().await.map_err(transport_error)? {
        if bytes.len() + chunk.len() > 1048576 {
            return Err(MeshError::new(
                ErrorCode::RateLimited,
                "RPC response exceeds 1 MiB",
            ));
        }
        bytes.extend_from_slice(&chunk);
    }
    if !status.is_success() {
        if let Ok(value) = serde_json::from_slice::<serde_json::Value>(&bytes)
            && let Some(error) = value.get("error")
            && let Ok(error) = serde_json::from_value::<MeshError>(error.clone())
        {
            return Err(error);
        }
        return Err(MeshError::new(
            ErrorCode::PeerUnreachable,
            format!("peer returned HTTP {status}"),
        ));
    }
    serde_json::from_slice(&bytes).map_err(|_| {
        MeshError::new(
            ErrorCode::UnsupportedVersion,
            "peer response does not match the expected contract",
        )
    })
}

/// Where `target` completes a TLS handshake as the computer `tls` pins, or
/// where it stopped. Nothing is sent over it.
async fn handshake(
    target: Target,
    tls: Arc<rustls::ClientConfig>,
) -> Result<SocketAddr, &'static str> {
    let (name, addresses) = match target {
        Target::Ip(address) => (ServerName::IpAddress(address.ip().into()), vec![address]),
        Target::Name(name, port) => (
            ServerName::try_from(name.as_str())
                .map_err(|_| UNRESOLVED)?
                .to_owned(),
            resolve(&name, port).await,
        ),
    };
    if addresses.is_empty() {
        return Err(UNRESOLVED);
    }
    let stream = connect(addresses)
        .await
        .map_err(|error| reason(&error).unwrap_or(UNREACHED))?;
    let at = stream.peer_addr().map_err(|_| UNREACHED)?;
    tokio_rustls::TlsConnector::from(tls)
        .connect(name, stream)
        .await
        .map_err(|error| reason(&error).unwrap_or(UNREACHED))?;
    Ok(at)
}
/// A TCP connection to whichever of `addresses` accepts first.
async fn connect(addresses: Vec<SocketAddr>) -> std::io::Result<TcpStream> {
    let mut connections = JoinSet::new();
    for address in addresses {
        connections.spawn(TcpStream::connect(address));
    }
    let mut last = None;
    while let Some(connected) = connections.join_next().await {
        match connected {
            Ok(Ok(stream)) => return Ok(stream),
            Ok(Err(error)) => last = Some(error),
            Err(_) => {}
        }
    }
    Err(last.unwrap_or_else(|| std::io::Error::other("no address to connect to")))
}
/// The addresses a host name resolves to that a peer may have.
async fn resolve(name: &str, port: u16) -> Vec<SocketAddr> {
    tokio::net::lookup_host((name, port))
        .await
        .map(permitted)
        .unwrap_or_default()
}
/// Of the addresses a name resolved to, the ones a peer may be at
/// (`peer_address`): never a link-local one such as the cloud metadata
/// service at 169.254.169.254, whether written as IPv4 or as IPv4 inside
/// IPv6, nor an unspecified or multicast one.
fn permitted(found: impl IntoIterator<Item = SocketAddr>) -> Vec<SocketAddr> {
    found.into_iter().filter(|a| peer_address(a.ip())).collect()
}

/// Host names in addresses resolve through the system (its DNS, and mDNS
/// for `.local`), keeping only the addresses a peer may be at: never a
/// link-local or unspecified one, whatever a name says.
struct PeerResolver;
#[derive(Debug)]
struct Unresolved;
impl std::fmt::Display for Unresolved {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("the name resolved to no address a peer may have")
    }
}
impl StdError for Unresolved {}
impl reqwest::dns::Resolve for PeerResolver {
    fn resolve(&self, name: reqwest::dns::Name) -> reqwest::dns::Resolving {
        let name = name.as_str().to_owned();
        Box::pin(async move {
            let found = resolve(&name, 0).await;
            if found.is_empty() {
                return Err(Box::new(Unresolved) as Box<dyn StdError + Send + Sync>);
            }
            Ok(Box::new(found.into_iter()) as reqwest::dns::Addrs)
        })
    }
}

/// A client for one address, or for one try at one: a new connection for
/// each request, never kept.
fn builder(config: rustls::ClientConfig, connect: Duration) -> reqwest::ClientBuilder {
    reqwest::Client::builder()
        .use_preconfigured_tls(config)
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .connect_timeout(connect)
        .timeout(Duration::from_secs(15))
        .pool_max_idle_per_host(0)
}

/// What stopped a connection to one address, in words the panel can match.
const REFUSED: &str = "refused the connection";
const SILENT: &str = "did not answer";
const UNRESOLVED: &str = "did not resolve";
const ANOTHER: &str = "is another computer";
const NO_ROUTE: &str = "has no route from here";
const UNREACHED: &str = "could not be reached";
fn failure(error: &reqwest::Error) -> &'static str {
    reason(error).unwrap_or(if error.is_timeout() {
        SILENT
    } else {
        UNREACHED
    })
}
/// Where a connection stopped, if `error` or an error behind it says.
fn reason(error: &(dyn StdError + 'static)) -> Option<&'static str> {
    let tls = |error: &rustls::Error| match error {
        rustls::Error::InvalidCertificate(_) | rustls::Error::General(_) => ANOTHER,
        _ => "failed the TLS handshake",
    };
    let mut source = Some(error);
    while let Some(current) = source {
        if current.is::<Unresolved>() {
            return Some(UNRESOLVED);
        }
        if let Some(error) = current.downcast_ref::<rustls::Error>() {
            return Some(tls(error));
        }
        if let Some(io) = current.downcast_ref::<std::io::Error>() {
            match io.kind() {
                std::io::ErrorKind::ConnectionRefused => return Some(REFUSED),
                std::io::ErrorKind::HostUnreachable | std::io::ErrorKind::NetworkUnreachable => {
                    return Some(NO_ROUTE);
                }
                std::io::ErrorKind::TimedOut => return Some(SILENT),
                _ => {}
            }
            // An I/O error's source is its inner error's source: look at
            // the inner error itself (a TLS error inside, say).
            if let Some(inner) = io.get_ref() {
                source = Some(inner);
                continue;
            }
        }
        source = current.source();
    }
    None
}
/// Every address failed before anything was sent: say where each one stopped.
fn unreachable(failures: &[(String, &'static str)]) -> MeshError {
    let each: Vec<String> = failures
        .iter()
        .map(|(origin, why)| format!("{origin} {why}"))
        .collect();
    let mut message = format!(
        "could not reach that computer at any address it is known by: {}",
        each.join("; ")
    );
    if failures.iter().any(|(_, why)| *why == NO_ROUTE) {
        message.push_str(". ");
        message.push_str(NO_ROUTE_HINT);
    }
    MeshError::new(ErrorCode::PeerUnreachable, message)
}
const NO_ROUTE_HINT: &str = if cfg!(target_os = "macos") {
    "No route to that computer: on macOS, allow hmesh under System Settings → Privacy & Security → Local Network (a daemon started at login needs it), and check that both computers are on the same network"
} else {
    "No route to that computer: check that both computers are on the same network"
};
fn transport_error(error: reqwest::Error) -> MeshError {
    // No route: often macOS refusing a LAN connection to a process without
    // Local Network access (a daemon started at login, TN3179), which
    // reports EHOSTUNREACH before a packet is sent.
    let mut source: Option<&(dyn StdError + 'static)> = Some(&error);
    while let Some(current) = source {
        if current
            .downcast_ref::<std::io::Error>()
            .is_some_and(|io| io.kind() == std::io::ErrorKind::HostUnreachable)
        {
            return MeshError::new(
                ErrorCode::PeerUnreachable,
                if cfg!(target_os = "macos") {
                    "no route to that computer: on macOS, allow hmesh under System Settings → Privacy & Security → Local Network (a daemon started at login needs it), and check that both computers are on the same network"
                } else {
                    "no route to that computer: check that both computers are on the same network"
                },
            );
        }
        source = current.source();
    }
    let message = if error.is_timeout() {
        "RPC deadline exceeded"
    } else if error.is_connect() {
        "peer connection/TLS verification failed"
    } else {
        "RPC transport failed"
    };
    MeshError::new(ErrorCode::PeerUnreachable, message)
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{Router, extract::State, routing::post};
    use hmesh_core::{NodeConfig, Peer};
    use std::sync::atomic::{AtomicBool, AtomicUsize};
    use tokio::sync::watch;

    /// A computer serving `/count` (it counts the calls it runs) on a new
    /// loopback port to the computer asking, and the TLS that one reaches
    /// it with.
    async fn counting(
        root: &std::path::Path,
    ) -> (
        String,
        Arc<AtomicUsize>,
        rustls::ClientConfig,
        watch::Sender<bool>,
    ) {
        let init = |name: &str| {
            let mut config = NodeConfig::default();
            config.node.name = name.into();
            config.node.network = "direct".into();
            Arc::new(Instance::init(&root.join(name), config).expect("init"))
        };
        let asks = init("asks");
        let studio = init("studio");
        let trusted = Peer {
            card: asks.card.clone(),
            status: "trusted".into(),
            approved_at: Some(hmesh_core::now()),
        };
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("listen");
        let origin = format!("https://{}", listener.local_addr().expect("address"));
        let calls = Arc::new(AtomicUsize::new(0));
        let app = Router::new()
            .route(
                "/count",
                post(|State(calls): State<Arc<AtomicUsize>>| async move {
                    calls.fetch_add(1, Ordering::SeqCst);
                    "counted"
                }),
            )
            .with_state(calls.clone());
        let (stop, stopped) = watch::channel(false);
        let server = studio.clone();
        tokio::spawn(crate::serve(
            listener,
            app,
            Arc::new(move || server.server_config(std::slice::from_ref(&trusted))),
            stopped,
        ));
        let tls = asks
            .client_config(&studio.card.fingerprint, false)
            .expect("tls");
        (origin, calls, tls, stop)
    }
    /// Does `then` the moment a connection is made, before the layers
    /// around it see the connection.
    #[derive(Clone)]
    struct OnConnect(Arc<dyn Fn() + Send + Sync>);
    impl<S> tower_layer::Layer<S> for OnConnect {
        type Service = OnConnectService<S>;
        fn layer(&self, inner: S) -> OnConnectService<S> {
            OnConnectService {
                inner,
                then: self.0.clone(),
            }
        }
    }
    #[derive(Clone)]
    struct OnConnectService<S> {
        inner: S,
        then: Arc<dyn Fn() + Send + Sync>,
    }
    impl<S, R> tower_service::Service<R> for OnConnectService<S>
    where
        S: tower_service::Service<R>,
        S::Response: Send + 'static,
        S::Error: 'static,
        S::Future: Send + 'static,
    {
        type Response = S::Response;
        type Error = S::Error;
        type Future = Connecting<S::Response, S::Error>;
        fn poll_ready(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), S::Error>> {
            self.inner.poll_ready(cx)
        }
        fn call(&mut self, request: R) -> Self::Future {
            let connecting = self.inner.call(request);
            let then = self.then.clone();
            Box::pin(async move {
                let connection = connecting.await?;
                then();
                Ok(connection)
            })
        }
    }
    /// Gives `gate`'s try up, writing down whether it was.
    fn giving_up(gate: &Arc<Gate>) -> (OnConnect, Arc<AtomicBool>) {
        let gave_up = Arc::new(AtomicBool::new(false));
        let (gate, record) = (gate.clone(), gave_up.clone());
        let then = move || record.store(gate.give_up(), Ordering::SeqCst);
        (OnConnect(Arc::new(then)), gave_up)
    }

    /// The try is given up (another address answered) in the very moment
    /// its connection is made, just before the gate sees the connection: it
    /// is dropped, the request is never sent, and the failure is one that
    /// moves on to another address.
    #[tokio::test]
    async fn a_connection_made_as_its_try_is_given_up_is_never_used() {
        let root = tempfile::tempdir().expect("tempdir");
        let (origin, calls, tls, _stop) = counting(root.path()).await;
        let gate = Arc::new(Gate::default());
        let (give_up, gave_up) = giving_up(&gate);
        let http = builder(tls, CONNECT)
            .connector_layer(give_up)
            .connector_layer(Gated(gate.clone()))
            .build()
            .expect("client");
        let sent = http.post(format!("{origin}/count")).send().await;
        assert!(gave_up.load(Ordering::SeqCst));
        let error = sent.expect_err("the connection is not used");
        assert!(unsent(&error, &gate), "{error:?}");
        assert_eq!(calls.load(Ordering::SeqCst), 0);
    }

    /// The other way round: the gate has let the connection through when
    /// the try is given up. Giving up fails, and the request runs, once.
    #[tokio::test]
    async fn a_try_that_has_connected_is_not_given_up() {
        let root = tempfile::tempdir().expect("tempdir");
        let (origin, calls, tls, _stop) = counting(root.path()).await;
        let gate = Arc::new(Gate::default());
        let (give_up, gave_up) = giving_up(&gate);
        let http = builder(tls, CONNECT)
            .connector_layer(Gated(gate.clone()))
            .connector_layer(give_up)
            .build()
            .expect("client");
        let sent = http
            .post(format!("{origin}/count"))
            .send()
            .await
            .expect("sent");
        assert!(sent.status().is_success());
        assert!(!gave_up.load(Ordering::SeqCst));
        assert!(gate.is_connected());
        assert!(!gate.give_up());
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }

    fn addresses(written: &[&str]) -> Vec<SocketAddr> {
        written
            .iter()
            .map(|a| a.parse().expect("socket address"))
            .collect()
    }

    /// A host name that resolves to the cloud metadata service, as IPv4 or
    /// as IPv4 inside IPv6, is never dialed there; ordinary addresses stay.
    #[test]
    fn a_name_never_leads_to_a_link_local_address() {
        let found = addresses(&[
            "169.254.169.254:38880",
            "[::ffff:169.254.169.254]:38880",
            "192.168.1.20:38880",
            "[fe80::1]:38880",
            "10.0.0.5:38880",
            "0.0.0.0:38880",
            "[::ffff:0.0.0.0]:38880",
            "[fd7a:115c:a1e0::5]:38880",
            "100.101.102.103:38880",
            "224.0.0.251:38880",
            "127.0.0.1:38880",
        ]);
        assert_eq!(
            permitted(found),
            addresses(&[
                "192.168.1.20:38880",
                "10.0.0.5:38880",
                "[fd7a:115c:a1e0::5]:38880",
                "100.101.102.103:38880",
                "127.0.0.1:38880",
            ])
        );
        assert!(permitted(addresses(&["[::ffff:169.254.169.254]:0"])).is_empty());
    }
}
