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
    net::SocketAddr,
    sync::{Arc, Mutex},
    time::Duration,
};
use tokio::{net::TcpStream, task::JoinSet};

/// How long a connection may take: resolving a host name, TCP and TLS.
const CONNECT: Duration = Duration::from_secs(3);
/// With other addresses to move on to, the one tried first gets this long
/// to connect. An address that sends nothing back (a network the computer
/// left) would otherwise use up a caller's whole wait, the shortest being
/// 750 ms, before the next address is tried. Nothing has been sent when a
/// connection is given up, so trying elsewhere never runs a call twice.
const FIRST_TRY: Duration = Duration::from_millis(500);
/// How long the first try runs alone before the other addresses are probed
/// (a TLS handshake, nothing sent), so a call answered at once costs no more.
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
    routes: Arc<[Route]>,
    owner_token: Option<String>,
    /// For a paired computer known by several addresses.
    routed: Option<Arc<Routed>>,
}
/// Where a paired computer answers is remembered under its fingerprint;
/// `tls` is the TLS the routes use, to see which addresses answer.
struct Routed {
    memory: Arc<RouteMemory>,
    fingerprint: String,
    tls: Arc<rustls::ClientConfig>,
}
struct Route {
    origin: String,
    target: Target,
    http: reqwest::Client,
    /// The same, with `FIRST_TRY` to connect: there are other addresses.
    first_try: Option<reqwest::Client>,
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
            routes: Arc::from([Route {
                origin: url.origin().ascii_serialization(),
                target,
                http: http(config, false, CONNECT)?,
                first_try: None,
            }]),
            owner_token: owner.then(|| instance.owner_token().to_owned()),
            routed: None,
        })
    }
    /// A paired computer at any of `candidates`, in order (its card's
    /// address, host names, addresses set by hand...). Host names resolve
    /// to the addresses `peer_address` permits; whoever answers must show
    /// the certificate `fingerprint` names. A call moves on to the next
    /// address only when the request was never sent (no connection, or the
    /// wrong certificate), so nothing runs twice. With several addresses
    /// the first one gets `FIRST_TRY` to connect, and the others are probed
    /// meanwhile, so the next one that answers is used at once.
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
        let mut targets = Vec::new();
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
            targets.push((origin, target));
        }
        if targets.is_empty() {
            anyhow::bail!("this computer has no address to reach that computer at");
        }
        let several = targets.len() > 1;
        let routes = targets
            .into_iter()
            .map(|(origin, target)| {
                Ok(Route {
                    origin,
                    target,
                    http: http(config.clone(), true, CONNECT)?,
                    first_try: several
                        .then(|| http(config.clone(), true, FIRST_TRY))
                        .transpose()?,
                })
            })
            .collect::<anyhow::Result<Vec<_>>>()?;
        Ok(Self {
            routes: routes.into(),
            owner_token: None,
            routed: Some(Arc::new(Routed {
                memory,
                fingerprint,
                tls: Arc::new(config),
            })),
        })
    }
    /// The addresses tried, in the order they are tried first.
    pub fn origins(&self) -> Vec<String> {
        self.order()
            .iter()
            .map(|&i| self.routes[i].origin.clone())
            .collect()
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
            routes: Arc::from([Route {
                origin: url.origin().ascii_serialization(),
                target: Target::of(&url).map_err(refused)?,
                http: http(config, false, CONNECT).map_err(refused)?,
                first_try: None,
            }]),
            owner_token: None,
            routed: None,
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
    /// Route indices in the order to try them: where the computer answered
    /// last first, then as given.
    fn order(&self) -> Vec<usize> {
        let mut order: Vec<usize> = (0..self.routes.len()).collect();
        if let Some(routed) = &self.routed
            && let Some(last) = routed.memory.last_good(&routed.fingerprint)
            && let Some(index) = self.routes.iter().position(|r| r.origin == last)
        {
            order.retain(|&i| i != index);
            order.insert(0, index);
        }
        order
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
        let Some(routed) = &self.routed else {
            let route = &self.routes[0];
            let response = self
                .request(route, &route.http, method, path, body, timeout)
                .await
                .map_err(transport_error)?;
            return read(response).await;
        };
        let order = self.order();
        let first = order[0];
        let several = order.len() > 1;
        let mut failed: Vec<Option<&'static str>> = vec![None; self.routes.len()];
        // The first address, with less time to connect when there are others;
        // they are probed if it has not answered after `HEAD_START`.
        let mut probes = JoinSet::new();
        let mut probing = false;
        let route = &self.routes[first];
        let quick = route.first_try.as_ref().unwrap_or(&route.http);
        let attempt = self.request(route, quick, method.clone(), path, body.clone(), timeout);
        tokio::pin!(attempt);
        let head_start = tokio::time::sleep(HEAD_START);
        tokio::pin!(head_start);
        let tried = loop {
            tokio::select! {
                tried = &mut attempt => break tried,
                () = &mut head_start, if several && !probing => {
                    self.probe(&routed.tls, order.iter().copied(), &mut probes);
                    probing = true;
                }
            }
        };
        // Sent, maybe even carried out: never sent again elsewhere. Not sent
        // (no connection, or not the computer that was meant): move on.
        let again = match tried {
            Ok(response) => {
                routed.memory.remember(&routed.fingerprint, &route.origin);
                return read(response).await;
            }
            Err(error) if error.is_connect() => {
                let why = failure(&error);
                failed[first] = Some(why);
                // Too slow for the first try, but it may still answer.
                why == SILENT
            }
            Err(error) => return Err(transport_error(error)),
        };
        if several && !probing {
            let others = order.iter().copied().filter(|&i| i != first || again);
            self.probe(&routed.tls, others, &mut probes);
        }
        // The others in the order they answer; the first again only if it
        // was merely slow and answers after all.
        while let Some(probed) = probes.join_next().await {
            let Ok((index, answered)) = probed else {
                continue;
            };
            if index == first && !(again && answered.is_ok()) {
                continue;
            }
            if let Err(why) = answered {
                failed[index] = Some(why);
                continue;
            }
            let route = &self.routes[index];
            match self
                .request(
                    route,
                    &route.http,
                    method.clone(),
                    path,
                    body.clone(),
                    timeout,
                )
                .await
            {
                Ok(response) => {
                    routed.memory.remember(&routed.fingerprint, &route.origin);
                    return read(response).await;
                }
                Err(error) if error.is_connect() => failed[index] = Some(failure(&error)),
                Err(error) => return Err(transport_error(error)),
            }
        }
        let failures: Vec<(String, &'static str)> = order
            .iter()
            .filter_map(|&i| failed[i].map(|why| (self.routes[i].origin.clone(), why)))
            .collect();
        Err(unreachable(&failures))
    }
    /// Start a TLS handshake with each of these routes (nothing is sent
    /// over it): which ones answer, as the right computer, and how soon.
    fn probe(
        &self,
        tls: &Arc<rustls::ClientConfig>,
        routes: impl IntoIterator<Item = usize>,
        probes: &mut JoinSet<(usize, Result<(), &'static str>)>,
    ) {
        for index in routes {
            let target = self.routes[index].target.clone();
            let tls = tls.clone();
            probes.spawn(async move {
                let answered = tokio::time::timeout(CONNECT, handshake(target, tls))
                    .await
                    .unwrap_or(Err(SILENT));
                (index, answered)
            });
        }
    }
    async fn request(
        &self,
        route: &Route,
        http: &reqwest::Client,
        method: reqwest::Method,
        path: &str,
        body: Option<Vec<u8>>,
        timeout: Option<Duration>,
    ) -> reqwest::Result<reqwest::Response> {
        let mut request = http.request(method, format!("{}{path}", route.origin));
        if let Some(token) = &self.owner_token {
            request = request.bearer_auth(token);
        }
        if let Some(body) = body {
            request = request
                .header(reqwest::header::CONTENT_TYPE, "application/json")
                .body(body);
        }
        if let Some(timeout) = timeout {
            request = request.timeout(timeout);
        }
        request.send().await
    }
}
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

/// Whether `target` completes a TLS handshake as the computer `tls` pins,
/// or where it stopped. Nothing is sent over it.
async fn handshake(target: Target, tls: Arc<rustls::ClientConfig>) -> Result<(), &'static str> {
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
    tokio_rustls::TlsConnector::from(tls)
        .connect(name, stream)
        .await
        .map_err(|error| reason(&error).unwrap_or(UNREACHED))?;
    Ok(())
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

fn http(
    config: rustls::ClientConfig,
    names: bool,
    connect: Duration,
) -> anyhow::Result<reqwest::Client> {
    let mut builder = reqwest::Client::builder()
        .use_preconfigured_tls(config)
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .connect_timeout(connect)
        .timeout(Duration::from_secs(15))
        .pool_max_idle_per_host(0);
    if names {
        builder = builder.dns_resolver(Arc::new(PeerResolver));
    }
    Ok(builder.build()?)
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
