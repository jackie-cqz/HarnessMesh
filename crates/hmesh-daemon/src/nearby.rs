//! Computers nearby (direct network). A computer that takes connections
//! announces itself over mDNS (`_hmesh._tcp`) only while its pairing window
//! is open and its passcode unused, and only on the interface it listens on
//! (on every interface: its addresses on trusted LANs, none on others).
//! The pairing dialog on another computer lists what it hears. Hearing a
//! name proves nothing: anyone on the network can announce one. The passcode
//! shown on that computer's screen is what confirms it (see `passcode`).
use super::*;
use mdns_sd::{IfKind, ResolvedService, ServiceDaemon, ServiceEvent, ServiceInfo};
use std::{
    collections::BTreeMap,
    time::{Duration, Instant},
};

pub const SERVICE: &str = "_hmesh._tcp.local.";
/// The TXT record's discovery version.
const VERSION: &str = "1";
/// How long one look around listens for answers.
const LISTEN_FOR: Duration = Duration::from_millis(1500);
/// How often the announcement is checked against the pairing window.
const CHECK_EVERY: Duration = Duration::from_millis(500);
/// At most this many announcements are probed, this many at a time, and
/// this many listed: a network full of announcements does not make this
/// computer dial everywhere.
const MAX_PROBED: usize = 128;
const PROBES_AT_ONCE: usize = 16;
const MAX_LISTED: usize = 32;

#[derive(Default)]
pub struct Nearby {
    daemon: Mutex<Option<ServiceDaemon>>,
    /// The full service name announced now, and on which addresses.
    announced: Mutex<Option<(String, Vec<IpAddr>)>>,
    /// One look around at a time: they share the daemon's browse.
    looking: tokio::sync::Mutex<()>,
    /// Woken when the pairing window opens or the passcode is used.
    pub changed: tokio::sync::Notify,
    /// The mark announced now for paired computers to find this one by (see
    /// `finding`): its full service name, and the mark, addresses and port.
    marked: Mutex<Option<(String, Mark)>>,
    /// One look for paired computers at a time.
    finding: tokio::sync::Mutex<()>,
    /// The mDNS daemon looks for paired computers go through, and the
    /// addresses whose interfaces it is limited to: it neither asks nor
    /// hears anywhere else (the daemon above serves every interface, for
    /// the computers nearby a pairing dialog lists).
    seeker: Mutex<Option<(ServiceDaemon, Vec<IpAddr>)>>,
    /// The last look for each paired computer, by fingerprint: when it
    /// began, and where it was heard once it ended.
    looks: Mutex<HashMap<String, Look>>,
}
/// A mark announced: the mark, on which addresses, at which port.
type Mark = (String, Vec<IpAddr>, u16);
/// A look for one paired computer: when it began, and where it was heard
/// (`None` while it is still listening).
type Look = (Instant, Option<Vec<SocketAddr>>);
/// At most one look for a paired computer this often; until the next, what
/// the last one heard is used.
const LOOK_EVERY: Duration = Duration::from_secs(30);

/// A computer heard on this network.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Heard {
    pub name: String,
    /// Where to reach it: `https://<address>:<port>`.
    pub endpoint: String,
    pub address: SocketAddr,
    /// Already paired with this computer.
    pub paired: bool,
}

pub fn routes() -> Router<AppState> {
    Router::new().route("/local/pairing/nearby", get(nearby))
}

/// The computers on this network that have pairing open now.
async fn nearby(
    State(s): State<AppState>,
    Extension(c): Extension<ConnectionIdentity>,
    headers: HeaderMap,
) -> ApiResult<Value> {
    s.owner(&headers, &c)?;
    if s.instance.config.node.network != "direct" {
        return Ok(Json(json!({"supported": false, "computers": []})));
    }
    let (addresses, port) = announce_on(&s);
    let own: Vec<SocketAddr> = addresses
        .into_iter()
        .map(|ip| SocketAddr::new(ip, port))
        .collect();
    let paired: Vec<String> = s
        .store
        .peers()?
        .into_iter()
        .filter(|p| p.status == "trusted")
        .map(|p| p.card.endpoint)
        .collect();
    let mut heard = s.nearby.look(&own).await.map_err(|error| {
        ApiError(MeshError::new(
            ErrorCode::PeerUnreachable,
            format!("could not look for computers on this network: {error}"),
        ))
    })?;
    // An announcement outlives a computer that stopped without saying
    // goodbye (until its record expires): list only those still taking
    // connections there.
    // Probed before the list is cut, so announcements nobody answers cannot
    // push a real computer off it.
    use futures_util::StreamExt;
    heard.truncate(MAX_PROBED);
    let addresses: Vec<SocketAddr> = heard.iter().map(|computer| computer.address).collect();
    let reachable: Vec<bool> =
        futures_util::stream::iter(addresses.into_iter().map(|address| async move {
            matches!(
                tokio::time::timeout(
                    Duration::from_millis(700),
                    tokio::net::TcpStream::connect(address),
                )
                .await,
                Ok(Ok(_))
            )
        }))
        .buffered(PROBES_AT_ONCE)
        .collect()
        .await;
    let mut reachable = reachable.into_iter();
    heard.retain(|_| reachable.next().unwrap_or(false));
    heard.truncate(MAX_LISTED);
    for computer in &mut heard {
        computer.paired = paired.contains(&computer.endpoint);
    }
    Ok(Json(json!({"supported": true, "computers": heard})))
}

/// Where this computer listens for other computers, if it does.
fn own_address(s: &AppState) -> Option<SocketAddr> {
    hmesh_security::validate_endpoint(&s.card().endpoint)
        .ok()
        .and_then(|url| {
            Some(SocketAddr::new(
                url.host_str()?.trim_matches(['[', ']']).parse().ok()?,
                url.port()?,
            ))
        })
}

impl Nearby {
    fn daemon(&self) -> anyhow::Result<ServiceDaemon> {
        let mut slot = self
            .daemon
            .lock()
            .map_err(|_| anyhow::anyhow!("nearby state poisoned"))?;
        if let Some(daemon) = slot.as_ref() {
            return Ok(daemon.clone());
        }
        let daemon = ServiceDaemon::new()?;
        *slot = Some(daemon.clone());
        Ok(daemon)
    }
    /// The mDNS daemon to look for paired computers with, on the interfaces
    /// of `on` only (each address's own IP version there): every other
    /// interface is disabled before anything is asked. A new one when `on`
    /// changed.
    fn seeker(&self, on: &[IpAddr]) -> anyhow::Result<ServiceDaemon> {
        let mut slot = self
            .seeker
            .lock()
            .map_err(|_| anyhow::anyhow!("nearby state poisoned"))?;
        if let Some((daemon, at)) = slot.as_ref()
            && at == on
        {
            return Ok(daemon.clone());
        }
        if let Some((old, _)) = slot.take() {
            let _ = old.shutdown();
        }
        let daemon = ServiceDaemon::new()?;
        // Commands run in order: these two before any browse.
        daemon.disable_interface(IfKind::All)?;
        daemon.enable_interface(on.iter().map(|ip| IfKind::Addr(*ip)).collect::<Vec<_>>())?;
        *slot = Some((daemon.clone(), on.to_vec()));
        Ok(daemon)
    }
    /// The addresses announced on now (none when nothing is).
    fn announced(&self) -> Option<Vec<IpAddr>> {
        self.announced
            .lock()
            .ok()
            .and_then(|a| a.as_ref().map(|(_, addresses)| addresses.clone()))
    }
    /// Announce this computer on the interfaces that have `addresses`, and
    /// only there.
    fn announce(&self, name: &str, addresses: &[IpAddr], port: u16) -> anyhow::Result<()> {
        if self.announced().is_some() || addresses.is_empty() {
            return Ok(());
        }
        let fullname = self.register(SERVICE, &instance_name(name), addresses, port)?;
        *self
            .announced
            .lock()
            .map_err(|_| anyhow::anyhow!("nearby state poisoned"))? =
            Some((fullname, addresses.to_vec()));
        Ok(())
    }
    /// Register `instance` of `service` at `addresses` and `port`, on their
    /// interfaces only: its full name.
    fn register(
        &self,
        service: &str,
        instance: &str,
        addresses: &[IpAddr],
        port: u16,
    ) -> anyhow::Result<String> {
        let mut label = [0u8; 4];
        ring::rand::SecureRandom::fill(&ring::rand::SystemRandom::new(), &mut label)
            .map_err(|_| anyhow::anyhow!("the system random number generator failed"))?;
        // A host name of its own each time: the announcement says nothing
        // lasting about this computer beyond what it names and its address.
        let host = format!("hmesh-{}.local.", hex::encode(label));
        let mut info = ServiceInfo::new(
            service,
            instance,
            &host,
            addresses,
            port,
            &[("v", VERSION)][..],
        )?;
        info.set_interfaces(addresses.iter().map(|ip| IfKind::Addr(*ip)).collect());
        let fullname = info.get_fullname().to_owned();
        self.daemon()?.register(info)?;
        Ok(fullname)
    }
    /// Stop announcing, telling the network the name is gone.
    async fn withdraw(&self) {
        let fullname = self.announced.lock().ok().and_then(|mut a| a.take());
        self.unregister(fullname.map(|(fullname, _)| fullname))
            .await;
    }
    async fn unregister(&self, fullname: Option<String>) {
        let Some(fullname) = fullname else {
            return;
        };
        if let Ok(daemon) = self.daemon()
            && let Ok(done) = daemon.unregister(&fullname)
        {
            let _ = tokio::time::timeout(Duration::from_millis(500), done.recv_async()).await;
        }
    }
    /// The mark announced now, if any.
    fn marked(&self) -> Option<Mark> {
        self.marked
            .lock()
            .ok()
            .and_then(|m| m.as_ref().map(|(_, mark)| mark.clone()))
    }
    /// Announce `mark` (with its addresses and port) for paired computers to
    /// find this one by, in place of the one before.
    async fn mark(&self, mark: Option<Mark>) -> anyhow::Result<()> {
        if self.marked() == mark {
            return Ok(());
        }
        let before = self.marked.lock().ok().and_then(|mut m| m.take());
        self.unregister(before.map(|(fullname, _)| fullname)).await;
        let Some(mark) = mark else {
            return Ok(());
        };
        let (name, addresses, port) = &mark;
        if addresses.is_empty() {
            return Ok(());
        }
        let fullname = self.register(finding::SERVICE, name, addresses, *port)?;
        *self
            .marked
            .lock()
            .map_err(|_| anyhow::anyhow!("nearby state poisoned"))? = Some((fullname, mark));
        Ok(())
    }
    /// Where the paired computer with `fingerprint`, whose find key is `key`,
    /// is heard on the local network now, listening only on the interfaces
    /// of the addresses `on` gives (see `find_on`). Looked for at most once
    /// every `LOOK_EVERY`: until then, what that look heard (nothing while it
    /// is still listening). The look finishes even when its caller gives up
    /// waiting, so the next call has its answer.
    pub async fn find_peer(
        self: Arc<Self>,
        fingerprint: String,
        key: Vec<u8>,
        on: impl Future<Output = Vec<IpAddr>> + Send + 'static,
    ) -> Vec<SocketAddr> {
        {
            let Ok(mut looks) = self.looks.lock() else {
                return vec![];
            };
            looks.retain(|_, (began, _)| began.elapsed() < LOOK_EVERY);
            if let Some((_, heard)) = looks.get(&fingerprint) {
                return heard.clone().unwrap_or_default();
            }
            looks.insert(fingerprint.clone(), (Instant::now(), None));
        }
        let nearby = self.clone();
        let look = tokio::spawn(async move {
            let on = on.await;
            let heard: Vec<SocketAddr> = match nearby.find(&[(fingerprint.clone(), key)], &on).await
            {
                Ok(found) => found.into_iter().map(|(_, address)| address).collect(),
                Err(error) => {
                    tracing::warn!(%error, "could not look for a paired computer on the local network");
                    vec![]
                }
            };
            if let Ok(mut looks) = nearby.looks.lock()
                && let Some((_, kept)) = looks.get_mut(&fingerprint)
            {
                *kept = Some(heard.clone());
            }
            heard
        });
        look.await.unwrap_or_default()
    }
    /// Listen for a moment for paired computers' marks: where each of
    /// `wanted` (a fingerprint, and the find key it told) is heard, by its
    /// mark for this or the previous ten minutes. Stops early once all are.
    /// Only on the interfaces of the addresses in `on`: with none, nothing
    /// is asked or heard at all.
    pub async fn find(
        &self,
        wanted: &[(String, Vec<u8>)],
        on: &[IpAddr],
    ) -> anyhow::Result<Vec<(String, SocketAddr)>> {
        if on.is_empty() {
            return Ok(vec![]);
        }
        let _one = self.finding.lock().await;
        let daemon = self.seeker(on)?;
        let events = daemon.browse(finding::SERVICE)?;
        let deadline = Instant::now() + LISTEN_FOR;
        let at = now();
        let mut found: Vec<(String, SocketAddr)> = Vec::new();
        loop {
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                break;
            }
            match tokio::time::timeout(left, events.recv_async()).await {
                Ok(Ok(ServiceEvent::ServiceResolved(service))) => {
                    let port = service.get_port();
                    if service.get_property_val_str("v") != Some(VERSION) || port == 0 {
                        continue;
                    }
                    let mark = mdns_sd::instance_name_unescaped(&service.fullname);
                    for (fingerprint, key) in wanted {
                        if !finding::matches(key, &mark, at) {
                            continue;
                        }
                        for ip in addresses(&service) {
                            let heard = (fingerprint.clone(), SocketAddr::new(ip, port));
                            if !found.contains(&heard) {
                                found.push(heard);
                            }
                        }
                    }
                    if wanted
                        .iter()
                        .all(|(fingerprint, _)| found.iter().any(|(f, _)| f == fingerprint))
                    {
                        break;
                    }
                }
                Ok(Ok(_)) => {}
                Ok(Err(_)) | Err(_) => break,
            }
        }
        let _ = daemon.stop_browse(finding::SERVICE);
        Ok(found)
    }
    /// Listen for a moment for computers announcing themselves; `own` is
    /// this computer's addresses, left out.
    pub async fn look(&self, own: &[SocketAddr]) -> anyhow::Result<Vec<Heard>> {
        let _one = self.looking.lock().await;
        let daemon = self.daemon()?;
        let events = daemon.browse(SERVICE)?;
        let deadline = Instant::now() + LISTEN_FOR;
        let mut found = BTreeMap::new();
        loop {
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                break;
            }
            match tokio::time::timeout(left, events.recv_async()).await {
                Ok(Ok(ServiceEvent::ServiceResolved(service))) => {
                    if let Some(heard) = heard(&service).filter(|h| !own.contains(&h.address)) {
                        found.insert(service.fullname.clone(), heard);
                    }
                }
                Ok(Ok(ServiceEvent::ServiceRemoved(_, fullname))) => {
                    found.remove(&fullname);
                }
                Ok(Ok(_)) => {}
                Ok(Err(_)) | Err(_) => break,
            }
        }
        let _ = daemon.stop_browse(SERVICE);
        let mut found: Vec<Heard> = found.into_values().collect();
        found.sort_by(|a, b| a.name.cmp(&b.name).then(a.address.cmp(&b.address)));
        found.dedup();
        Ok(found)
    }
    async fn shutdown(&self) {
        self.withdraw().await;
        let _ = self.mark(None).await;
        if let Some(daemon) = self.daemon.lock().ok().and_then(|mut d| d.take()) {
            let _ = daemon.shutdown();
        }
        if let Some((daemon, _)) = self.seeker.lock().ok().and_then(|mut d| d.take()) {
            let _ = daemon.shutdown();
        }
    }
}

/// A DNS label is at most 63 bytes; names are cut on a character boundary.
fn instance_name(name: &str) -> String {
    let mut out = String::new();
    for c in name.chars().filter(|c| !c.is_control()) {
        if out.len() + c.len_utf8() > 63 {
            break;
        }
        out.push(c);
    }
    if out.trim().is_empty() {
        "HarnessMesh".into()
    } else {
        out
    }
}

/// What an announcement says, if it is one this version understands: a
/// readable name and one address another computer can be dialed on.
fn heard(service: &ResolvedService) -> Option<Heard> {
    if service.get_property_val_str("v") != Some(VERSION) || service.get_port() == 0 {
        return None;
    }
    let address = SocketAddr::new(*addresses(service).first()?, service.get_port());
    let name = instance_name(&mdns_sd::instance_name_unescaped(&service.fullname));
    Some(Heard {
        name,
        endpoint: format!("https://{address}"),
        address,
        paired: false,
    })
}
/// The addresses an announcement gives that another computer may be dialed
/// on: IPv4 first, then the rest, each in a stable order; never link-local,
/// multicast and the like (`peer_address`).
fn addresses(service: &ResolvedService) -> Vec<IpAddr> {
    let mut addresses: Vec<IpAddr> = service
        .get_addresses()
        .iter()
        .map(|a| a.to_ip_addr())
        .filter(|ip| hmesh_security::peer_address(*ip))
        .collect();
    addresses.sort_by_key(|ip| (ip.is_ipv6(), *ip));
    addresses.dedup();
    addresses
}

/// Where to announce this computer: the address it listens on, or on
/// every interface its addresses on trusted LANs (not Tailscale ones: mDNS
/// does not cross a Tailnet), at its port.
fn announce_on(state: &AppState) -> (Vec<IpAddr>, u16) {
    let Some(own) = own_address(state) else {
        return (vec![], 0);
    };
    if !state.networks.every_interface() {
        return (vec![own.ip()], own.port());
    }
    let lans = state
        .networks
        .open()
        .into_iter()
        .filter(|(_, tailnet)| !tailnet)
        .map(|(ip, _)| ip)
        .collect();
    (lans, own.port())
}

/// Where this computer listens for paired computers' marks: where it would
/// announce its own (`announce_on`: the address it listens on, or on every
/// interface its addresses on trusted LANs, none while it trusts none). A
/// computer that only connects out trusts no network and listens on the
/// interface its default route goes through (`default_route`). Nowhere
/// else: not on networks this computer has not trusted, not on Tailscale
/// (mDNS does not cross a Tailnet), not on other interfaces (VPNs, virtual
/// machines' bridges).
pub(crate) async fn find_on(state: &AppState) -> Vec<IpAddr> {
    if state.instance.config.outbound_only() {
        return default_route(&hmesh_transport::netinfo::observe(state.tailscale()).await);
    }
    announce_on(state).0
}
/// The addresses of the interface the default route goes through (as
/// `observe` sees it: the network that has the default gateway), not
/// Tailscale ones; none without a default route.
fn default_route(observed: &[LocalNetwork]) -> Vec<IpAddr> {
    let Some(routed) = observed.iter().find(|n| n.gateway.is_some() && !n.tailnet) else {
        return vec![];
    };
    observed
        .iter()
        .filter(|n| n.interface == routed.interface && !n.tailnet)
        .map(|n| n.address)
        .collect()
}

/// The mark to announce for paired computers, and where: where
/// `announce_on` says (its address, or its trusted LANs on every
/// interface), while this computer takes connections. Nothing on networks it
/// has not trusted, nor from a computer that only connects out.
fn mark_on(state: &AppState) -> Option<Mark> {
    let (addresses, port) = announce_on(state);
    state
        .find_mark()
        .filter(|_| !addresses.is_empty())
        .map(|mark| (mark, addresses, port))
}

/// Announce this computer while its passcode can be used, and its mark for
/// paired computers to find it by while it takes connections on a trusted
/// LAN (see `finding`); nothing otherwise. Runs until the daemon stops.
pub async fn run(state: AppState, mut stop: watch::Receiver<bool>) {
    if !state.offers_passcode() {
        // Announces nothing; looking for paired computers may still have
        // started the mDNS daemon, which stops with this one.
        while !*stop.borrow() {
            if stop.changed().await.is_err() {
                break;
            }
        }
        state.nearby.shutdown().await;
        return;
    }
    let mut failed = false;
    let mut mark_failed = false;
    loop {
        let (addresses, port) = announce_on(&state);
        // Moved to other networks: announced again, where it is now.
        if state
            .nearby
            .announced()
            .is_some_and(|announced| announced != addresses)
        {
            state.nearby.withdraw().await;
        }
        if state.passcode_open() {
            match state
                .nearby
                .announce(&state.instance.card.display_name, &addresses, port)
            {
                Ok(()) => failed = false,
                Err(error) if !failed => {
                    failed = true;
                    tracing::warn!(%error, "could not announce this computer on the network; its pairing code still works");
                }
                Err(_) => {}
            }
        } else {
            state.nearby.withdraw().await;
        }
        // A new mark every ten minutes, with a new key, or elsewhere.
        match state.nearby.mark(mark_on(&state)).await {
            Ok(()) => mark_failed = false,
            Err(error) if !mark_failed => {
                mark_failed = true;
                tracing::warn!(%error, "could not announce this computer's mark for paired computers on the network");
            }
            Err(_) => {}
        }
        tokio::select! {
            changed = stop.changed() => if changed.is_err() || *stop.borrow() { break },
            _ = state.nearby.changed.notified() => {}
            _ = tokio::time::sleep(CHECK_EVERY) => {}
        }
    }
    state.nearby.shutdown().await;
}

#[cfg(test)]
mod tests {
    use super::*;

    fn state(root: &std::path::Path, name: &str, change: impl FnOnce(&mut NodeConfig)) -> AppState {
        let mut config = NodeConfig::default();
        config.node.network = "direct".into();
        config.node.host_names = Some(vec![]);
        change(&mut config);
        let instance = Arc::new(Instance::init(&root.join(name), config).unwrap());
        let store = Store::open(&instance.dir.join("state.sqlite3")).unwrap();
        AppState::load(instance, store, watch::channel(false).0).unwrap()
    }
    fn lan(address: &str, subnet: &str) -> LocalNetwork {
        LocalNetwork {
            interface: "en0".into(),
            address: address.parse().unwrap(),
            subnet: subnet.parse().unwrap(),
            gateway: None,
            gateway_mac: None,
            tailnet: false,
        }
    }

    /// The mark goes out only where the pairing announcement may: on the
    /// one address a computer listens on, or on its trusted LANs (never a
    /// network it has not trusted, nor Tailscale); a computer that only
    /// connects out announces none.
    #[test]
    fn a_mark_is_announced_only_on_trusted_lans_of_a_computer_that_takes_connections() {
        let root = tempfile::tempdir().unwrap();
        let one = state(root.path(), "one", |c| {
            c.node.expose_to_tailnet = true;
            c.node.listen_address = "127.0.0.1".parse().unwrap();
        });
        let (mark, addresses, _) = mark_on(&one).expect("a mark on its one address");
        assert_eq!(Some(mark), one.find_mark());
        assert_eq!(addresses, ["127.0.0.1".parse::<IpAddr>().unwrap()]);

        let every = state(root.path(), "every", |c| {
            c.node.expose_to_tailnet = true;
            c.node.listen_address = "::".parse().unwrap();
            c.node.trusted_networks = Some(vec![]);
        });
        every.networks.set_port(39880);
        let home = lan("192.168.1.20", "192.168.1.0/24");
        let cafe = lan("10.0.0.5", "10.0.0.0/24");
        let tailnet = LocalNetwork {
            interface: "tailscale0".into(),
            tailnet: true,
            ..lan("100.101.102.103", "100.64.0.0/10")
        };
        let observed = vec![home.clone(), cafe, tailnet];
        // Nothing trusted yet: no mark.
        every.apply_networks(observed.clone(), None, true);
        assert!(mark_on(&every).is_none());
        // Home and Tailscale trusted: on home only.
        every.apply_networks(
            observed,
            Some(vec![
                TrustedNetwork {
                    subnet: home.subnet,
                    gateway: None,
                    name: None,
                    tailnet: false,
                },
                TrustedNetwork {
                    subnet: "100.64.0.0/10".parse().unwrap(),
                    gateway: None,
                    name: Some("Tailscale".into()),
                    tailnet: true,
                },
            ]),
            false,
        );
        let (_, addresses, port) = mark_on(&every).expect("a mark on the trusted LAN");
        assert_eq!(addresses, [home.address]);
        assert_eq!(port, 39880);

        let outbound = state(root.path(), "outbound", |_| {});
        assert!(outbound.instance.config.outbound_only());
        assert!(mark_on(&outbound).is_none());
    }

    /// One paired computer is looked for at most once every thirty
    /// seconds: until then, what that look heard is the answer.
    #[tokio::test]
    async fn a_paired_computer_is_looked_for_at_most_once_every_thirty_seconds() {
        let nearby = Arc::new(Nearby::default());
        let began = |fingerprint: &str| nearby.looks.lock().unwrap().get(fingerprint).map(|l| l.0);
        let first = nearby
            .clone()
            .find_peer("aa".into(), vec![1; 32], loopback())
            .await;
        let looked = began("aa").expect("looked");
        let again = nearby
            .clone()
            .find_peer("aa".into(), vec![1; 32], loopback())
            .await;
        assert_eq!(again, first);
        assert_eq!(
            began("aa"),
            Some(looked),
            "looked again within thirty seconds"
        );
        // Another computer has a look of its own.
        nearby
            .clone()
            .find_peer("bb".into(), vec![2; 32], loopback())
            .await;
        assert!(began("bb").is_some_and(|b| b >= looked));
        // Thirty seconds on: looked for again.
        if let Some(look) = nearby.looks.lock().unwrap().get_mut("aa") {
            look.0 = Instant::now() - LOOK_EVERY;
        }
        nearby
            .clone()
            .find_peer("aa".into(), vec![1; 32], loopback())
            .await;
        assert!(began("aa").is_some_and(|b| b > looked));
        nearby.shutdown().await;
    }
    fn loopback() -> std::future::Ready<Vec<IpAddr>> {
        std::future::ready(vec![std::net::Ipv4Addr::LOCALHOST.into()])
    }

    /// Paired computers are looked for where the mark is announced: on the
    /// one address a computer listens on, or on its trusted LANs (nowhere
    /// while it trusts none, never on a network it has not trusted or on
    /// Tailscale). One that only connects out trusts no network: it looks on
    /// the interface its default route goes through, every address there,
    /// and on no other interface.
    #[tokio::test]
    async fn paired_computers_are_looked_for_only_where_this_computer_may_look() {
        let root = tempfile::tempdir().unwrap();
        let one = state(root.path(), "one", |c| {
            c.node.expose_to_tailnet = true;
            c.node.listen_address = "127.0.0.1".parse().unwrap();
        });
        assert_eq!(
            find_on(&one).await,
            ["127.0.0.1".parse::<IpAddr>().unwrap()]
        );

        let every = state(root.path(), "every", |c| {
            c.node.expose_to_tailnet = true;
            c.node.listen_address = "::".parse().unwrap();
            c.node.trusted_networks = Some(vec![]);
        });
        every.networks.set_port(39880);
        let home = lan("192.168.1.20", "192.168.1.0/24");
        let cafe = LocalNetwork {
            interface: "en1".into(),
            ..lan("10.0.0.5", "10.0.0.0/24")
        };
        let tailnet = LocalNetwork {
            interface: "tailscale0".into(),
            tailnet: true,
            ..lan("100.101.102.103", "100.64.0.0/10")
        };
        let observed = vec![home.clone(), cafe.clone(), tailnet.clone()];
        every.apply_networks(observed.clone(), None, true);
        assert!(
            find_on(&every).await.is_empty(),
            "looked on untrusted networks"
        );
        every.apply_networks(
            observed,
            Some(vec![
                TrustedNetwork {
                    subnet: home.subnet,
                    gateway: None,
                    name: None,
                    tailnet: false,
                },
                TrustedNetwork {
                    subnet: "100.64.0.0/10".parse().unwrap(),
                    gateway: None,
                    name: Some("Tailscale".into()),
                    tailnet: true,
                },
            ]),
            false,
        );
        assert_eq!(find_on(&every).await, [home.address]);

        // Only connects out: the interface of the default route.
        let routed = LocalNetwork {
            gateway: Some("192.168.1.1".parse().unwrap()),
            ..home.clone()
        };
        let routed_v6 = lan("fd12::5", "fd12::/64");
        let cafe_routed = LocalNetwork {
            gateway: Some("10.0.0.1".parse().unwrap()),
            ..cafe.clone()
        };
        assert_eq!(
            default_route(&[
                routed.clone(),
                cafe.clone(),
                routed_v6.clone(),
                tailnet.clone()
            ]),
            [routed.address, routed_v6.address]
        );
        assert_eq!(
            default_route(&[cafe_routed.clone(), routed_v6]),
            [cafe_routed.address]
        );
        assert!(default_route(&[home, cafe, tailnet]).is_empty());
    }

    /// Looking for paired computers goes through an mDNS daemon of its own,
    /// enabled only on the interfaces of the addresses given (each address's
    /// IP version there); with none, nothing is asked and no daemon starts.
    #[tokio::test]
    async fn paired_computers_are_listened_for_only_on_the_interfaces_given() {
        let nearby = Nearby::default();
        let wanted = [("aa".to_owned(), vec![1; 32])];
        assert!(nearby.find(&wanted, &[]).await.unwrap().is_empty());
        assert!(nearby.seeker.lock().unwrap().is_none(), "a daemon started");
        let enabled = |on: &[IpAddr]| {
            let daemon = nearby.seeker(on).unwrap();
            match daemon
                .status()
                .unwrap()
                .recv_timeout(Duration::from_secs(5))
                .unwrap()
            {
                mdns_sd::DaemonStatus::Running(running) => running.enabled_intfs().to_vec(),
                other => panic!("{other:?}"),
            }
        };
        for interface in enabled(&[std::net::Ipv4Addr::LOCALHOST.into()]) {
            assert!(interface.ipv6().is_none(), "{interface:?}");
            assert!(
                interface
                    .ipv4()
                    .is_some_and(|v4| v4.addrs().iter().all(|ip| ip.is_loopback())),
                "{interface:?}"
            );
        }
        // A LAN address of this computer: on its interface, IPv4 only.
        let lan = hmesh_transport::netinfo::observe(None)
            .await
            .into_iter()
            .find(|n| !n.tailnet && n.address.is_ipv4());
        if let Some(lan) = lan {
            let shown = enabled(&[lan.address]);
            assert!(!shown.is_empty(), "enabled nowhere for {lan:?}");
            for interface in shown {
                assert_eq!(interface.name(), lan.interface, "{interface:?}");
                assert!(interface.ipv6().is_none(), "{interface:?}");
            }
        } else {
            eprintln!("no LAN address here: only loopback was checked");
        }
        nearby.shutdown().await;
    }
}
