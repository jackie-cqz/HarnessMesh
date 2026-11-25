//! Computers nearby (direct network). A computer that takes connections
//! announces itself over mDNS (`_hmesh._tcp`) only while its pairing window
//! is open and its passcode unused, and only on the interface it listens on.
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
    /// The full service name announced now.
    announced: Mutex<Option<String>>,
    /// One look around at a time: they share the daemon's browse.
    looking: tokio::sync::Mutex<()>,
    /// Woken when the pairing window opens or the passcode is used.
    pub changed: tokio::sync::Notify,
}

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
    let own = own_address(&s);
    let paired: Vec<String> = s
        .store
        .peers()?
        .into_iter()
        .filter(|p| p.status == "trusted")
        .map(|p| p.card.endpoint)
        .collect();
    let mut heard = s.nearby.look(own).await.map_err(|error| {
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
    hmesh_security::validate_endpoint(&s.instance.card.endpoint)
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
    fn announced(&self) -> bool {
        self.announced.lock().is_ok_and(|a| a.is_some())
    }
    /// Announce this computer on the interface that has `address`, and only there.
    fn announce(&self, name: &str, address: SocketAddr) -> anyhow::Result<()> {
        if self.announced() {
            return Ok(());
        }
        let mut label = [0u8; 4];
        ring::rand::SecureRandom::fill(&ring::rand::SystemRandom::new(), &mut label)
            .map_err(|_| anyhow::anyhow!("the system random number generator failed"))?;
        // A host name of its own each time: the announcement says nothing
        // lasting about this computer beyond its name and address.
        let host = format!("hmesh-{}.local.", hex::encode(label));
        let mut info = ServiceInfo::new(
            SERVICE,
            &instance_name(name),
            &host,
            address.ip(),
            address.port(),
            &[("v", VERSION)][..],
        )?;
        info.set_interfaces(vec![IfKind::Addr(address.ip())]);
        let fullname = info.get_fullname().to_owned();
        self.daemon()?.register(info)?;
        *self
            .announced
            .lock()
            .map_err(|_| anyhow::anyhow!("nearby state poisoned"))? = Some(fullname);
        Ok(())
    }
    /// Stop announcing, telling the network the name is gone.
    async fn withdraw(&self) {
        let Some(fullname) = self.announced.lock().ok().and_then(|mut a| a.take()) else {
            return;
        };
        if let Ok(daemon) = self.daemon()
            && let Ok(done) = daemon.unregister(&fullname)
        {
            let _ = tokio::time::timeout(Duration::from_millis(500), done.recv_async()).await;
        }
    }
    /// Listen for a moment for computers announcing themselves; `own` is
    /// this computer's address, left out.
    pub async fn look(&self, own: Option<SocketAddr>) -> anyhow::Result<Vec<Heard>> {
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
                    if let Some(heard) = heard(&service).filter(|h| Some(h.address) != own) {
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
        if let Some(daemon) = self.daemon.lock().ok().and_then(|mut d| d.take()) {
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
    // IPv4 first, then the rest, each in a stable order; never link-local,
    // multicast and the like (peer_address).
    let mut addresses: Vec<IpAddr> = service
        .get_addresses()
        .iter()
        .map(|a| a.to_ip_addr())
        .filter(|ip| hmesh_security::peer_address(*ip))
        .collect();
    addresses.sort_by_key(|ip| (ip.is_ipv6(), *ip));
    let address = SocketAddr::new(*addresses.first()?, service.get_port());
    let name = instance_name(&mdns_sd::instance_name_unescaped(&service.fullname));
    Some(Heard {
        name,
        endpoint: format!("https://{address}"),
        address,
        paired: false,
    })
}

/// Announce this computer while its passcode can be used; nothing
/// otherwise. Runs until the daemon stops.
pub async fn run(state: AppState, mut stop: watch::Receiver<bool>) {
    if !state.offers_passcode() {
        return;
    }
    let Some(address) = own_address(&state) else {
        return;
    };
    let mut failed = false;
    loop {
        if state.passcode_open() {
            match state
                .nearby
                .announce(&state.instance.card.display_name, address)
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
        tokio::select! {
            changed = stop.changed() => if changed.is_err() || *stop.borrow() { break },
            _ = state.nearby.changed.notified() => {}
            _ = tokio::time::sleep(CHECK_EVERY) => {}
        }
    }
    state.nearby.shutdown().await;
}
