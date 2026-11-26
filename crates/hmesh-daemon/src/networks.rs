//! Listening on every interface (direct network, `listen_address` 0.0.0.0
//! or ::): which networks this computer is on now, which of them it trusts,
//! and the fence in front of its one port. Every few seconds it looks
//! again; when its addresses on trusted networks change, the fence, its
//! card and its address list follow at once, and paired computers are told.
use super::*;
use hmesh_transport::{Fence, netinfo};
use std::{
    sync::atomic::{AtomicU16, Ordering},
    time::Duration,
};

/// How often this computer looks at its networks again.
const LOOK_EVERY: Duration = Duration::from_secs(5);

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/local/networks", get(list))
        .route("/local/networks/trust", post(trust))
        .route("/local/networks/forget", post(forget))
}

pub struct Networks {
    every_interface: bool,
    /// The port it listens on (known once it is bound).
    port: AtomicU16,
    sources: Vec<IpNet>,
    trusted: RwLock<Vec<TrustedNetwork>>,
    observed: RwLock<Vec<LocalNetwork>>,
    /// What the listener reads for every connection.
    pub fence: Arc<RwLock<Fence>>,
    /// One change at a time: a look around, or the owner trusting or
    /// forgetting a network (which also writes node.toml).
    changing: tokio::sync::Mutex<()>,
}
impl Networks {
    pub fn new(config: &NodeConfig) -> Self {
        Self {
            every_interface: config.every_interface(),
            port: AtomicU16::new(config.node.listen_port),
            sources: config.allowed_sources(),
            trusted: RwLock::new(config.node.trusted_networks.clone().unwrap_or_default()),
            observed: RwLock::default(),
            fence: Arc::default(),
            changing: tokio::sync::Mutex::new(()),
        }
    }
    pub fn every_interface(&self) -> bool {
        self.every_interface
    }
    pub fn set_port(&self, port: u16) {
        self.port.store(port, Ordering::Relaxed);
    }
    fn trusted(&self) -> Vec<TrustedNetwork> {
        self.trusted.read().map(|t| t.clone()).unwrap_or_default()
    }
    fn observed(&self) -> Vec<LocalNetwork> {
        self.observed.read().map(|o| o.clone()).unwrap_or_default()
    }
    /// This computer's addresses on the networks it trusts, each with
    /// whether it is a Tailscale one (open to Tailnet sources only).
    pub fn open(&self) -> Vec<(IpAddr, bool)> {
        open_addresses(&self.trusted(), &self.observed())
    }
    /// The same, as addresses paired computers can reach it at.
    pub fn open_endpoints(&self) -> Vec<String> {
        let port = self.port.load(Ordering::Relaxed);
        self.open()
            .into_iter()
            .map(|(ip, _)| endpoint(ip, port))
            .collect()
    }
    /// Whether `address` (an `https://` origin) names one of the addresses
    /// this computer sees itself at. Loopback is never one, except where a
    /// test made it stand in for a network: then a peer on it is callable.
    pub fn names_own_network(&self, address: &str) -> bool {
        let ip: Option<IpAddr> = hmesh_security::validate_peer_address(address)
            .ok()
            .and_then(|url| url.host_str()?.trim_matches(['[', ']']).parse().ok());
        ip.is_some_and(|ip| self.observed().iter().any(|n| n.address == ip))
    }
}
fn endpoint(ip: IpAddr, port: u16) -> String {
    format!("https://{}", SocketAddr::new(ip, port))
}
/// The addresses on networks `trusted` recognizes, in the order observed.
pub fn open_addresses(
    trusted: &[TrustedNetwork],
    observed: &[LocalNetwork],
) -> Vec<(IpAddr, bool)> {
    let mut open: Vec<(IpAddr, bool)> = Vec::new();
    for network in observed {
        if recognize(trusted, network).is_some()
            && !open.iter().any(|(ip, _)| *ip == network.address)
        {
            open.push((network.address, network.tailnet));
        }
    }
    open
}
/// What the owner sees: the networks this computer is on (each trusted or
/// not, by its subnet only or not, and whether its gateway could be checked
/// now), the trusted ones, the network to offer trusting (the one with the
/// default route, or the first, when it is not trusted), and `notes` in
/// words for each network trusted by its subnet only.
pub fn view(every_interface: bool, trusted: &[TrustedNetwork], observed: &[LocalNetwork]) -> Value {
    let mut notes: Vec<String> = Vec::new();
    let current: Vec<Value> = observed
        .iter()
        .map(|network| {
            let recognized = recognize(trusted, network).filter(|_| every_interface);
            let entry = recognized.map(|(i, _)| &trusted[i]);
            let unverified = recognized.is_some_and(|(_, u)| u);
            let mut shown = json!(network);
            shown["trusted"] = json!(entry.is_some());
            shown["subnet_only"] = json!(entry.is_some_and(TrustedNetwork::subnet_only));
            shown["gateway_unverified"] = json!(unverified);
            shown["name"] = json!(entry.and_then(|e| e.name.clone()));
            if unverified {
                notes.push(format!(
                    "{}: its gateway's hardware address cannot be read now, so it is recognized by its subnet only",
                    network.subnet
                ));
            }
            shown
        })
        .collect();
    let listed: Vec<Value> = trusted
        .iter()
        .map(|entry| {
            let mut shown = json!(entry);
            shown["subnet_only"] = json!(entry.subnet_only());
            shown
        })
        .collect();
    if every_interface {
        for entry in trusted.iter().filter(|e| e.subnet_only()) {
            notes.push(format!(
                "{} is trusted by its subnet only (no gateway hardware address was seen when it was trusted): any other network with the same subnet counts as trusted too. Trust it again where its gateway can be seen to tie it to that gateway.",
                entry.subnet
            ));
        }
    }
    let suggested = observed
        .iter()
        .find(|n| !n.tailnet)
        .filter(|n| every_interface && recognize(trusted, n).is_none())
        .map(|n| {
            json!({
                "subnet": n.subnet,
                "interface": n.interface,
                "address": n.address,
                "subnet_only": n.gateway_mac.is_none(),
            })
        });
    json!({
        "every_interface": every_interface,
        "current": current,
        "trusted": listed,
        "suggested": suggested,
        "tailnet": observed.iter().any(|n| n.tailnet),
        "notes": notes,
    })
}

/// Keep `trusted` in the instance's node.toml, for the next start.
pub fn write_trusted(dir: &FilePath, trusted: &[TrustedNetwork]) -> hmesh_core::Result<()> {
    let path = dir.join("node.toml");
    let mut config: NodeConfig =
        toml::from_str(&fs::read_to_string(&path).map_err(internal_error)?)
            .map_err(internal_error)?;
    config.node.trusted_networks = Some(trusted.to_vec());
    config.validate()?;
    atomic_write(
        &path,
        toml::to_string_pretty(&config)
            .map_err(internal_error)?
            .as_bytes(),
    )
    .map_err(internal_error)
}
fn internal_error(error: impl std::fmt::Display) -> MeshError {
    MeshError::new(ErrorCode::Internal, error.to_string())
}

impl AppState {
    /// This computer's card as it is now: a computer on every interface
    /// names its address on a trusted network.
    pub fn card(&self) -> NodeCard {
        self.card
            .read()
            .map(|card| card.clone())
            .unwrap_or_else(|_| self.instance.card.clone())
    }
    /// A pair request carrying this computer's current card.
    pub(crate) fn pair_submission(
        &self,
        challenge: PairChallenge,
    ) -> std::result::Result<PairSubmission, ApiError> {
        self.instance
            .pair_submission_as(&self.card(), challenge)
            .map_err(internal)
    }
    /// The networks as the owner sees them, from the last look.
    pub(crate) fn networks_view(&self) -> Value {
        view(
            self.networks.every_interface(),
            &self.networks.trusted(),
            &self.networks.observed(),
        )
    }
    /// Take in what was just observed (and a new list of trusted networks):
    /// the fence first, then, when the addresses on trusted networks
    /// changed (or at `startup`), the card and the address list. Paired
    /// computers are told at once, not at the next round.
    pub(crate) fn apply_networks(
        &self,
        observed: Vec<LocalNetwork>,
        trusted: Option<Vec<TrustedNetwork>>,
        startup: bool,
    ) {
        let before = self.networks.open();
        if let Ok(mut current) = self.networks.observed.write() {
            *current = observed.clone();
        }
        if let Some(trusted) = trusted
            && let Ok(mut current) = self.networks.trusted.write()
        {
            *current = trusted;
        }
        let open = self.networks.open();
        if let Ok(mut fence) = self.networks.fence.write() {
            *fence = Fence {
                open: open.clone(),
                observed: observed.iter().map(|n| n.address).collect(),
                sources: self.networks.sources.clone(),
                lans: observed
                    .iter()
                    .filter(|n| !n.tailnet)
                    .map(|n| n.subnet)
                    .collect(),
            };
        }
        if open == before && !startup {
            return;
        }
        // The card names the first address on a trusted LAN, else a
        // Tailscale one; with neither, the one it named before.
        let mut card = self.card();
        let port = self.networks.port.load(Ordering::Relaxed);
        if let Some((ip, _)) = open
            .iter()
            .find(|(_, tailnet)| !tailnet)
            .or_else(|| open.first())
        {
            card.endpoint = endpoint(*ip, port);
        }
        let moved = card.endpoint != self.card().endpoint;
        if let Ok(mut current) = self.card.write() {
            *current = card.clone();
        }
        if moved && !startup {
            if let Err(error) = self.store.set_node(&card) {
                tracing::warn!(code = ?error.code, "the new card was not kept");
            }
            if let Ok(bytes) = serde_json::to_vec(&card)
                && let Err(error) = atomic_write(&self.instance.dir.join("daemon.json"), &bytes)
            {
                tracing::warn!(%error, "the new card was not published to daemon.json");
            }
        }
        let previous = self.addresses().revision;
        let revision = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|elapsed| elapsed.as_millis() as i64)
            .unwrap_or_default()
            .max(previous + 1);
        let mut list = addresses::local(
            &self.instance.config,
            &card,
            &self.networks.open_endpoints(),
            &self.relays(),
            if startup { previous } else { revision },
        );
        if let Ok(mut current) = self.addresses.write() {
            // Read while the list is held: a key changed meanwhile (see
            // `rotate_find_key`) is not put back.
            list.find_key = self.find_key_told();
            *current = list;
        }
        if !startup {
            tracing::info!(
                endpoint = %card.endpoint,
                open = open.len(),
                "this computer's addresses on trusted networks changed"
            );
            self.peers_changed();
            self.nearby.changed.notify_one();
        }
    }
    /// The Tailscale CLI node.toml names, if any (asked which addresses are
    /// Tailscale's own).
    pub(crate) fn tailscale(&self) -> Option<&std::path::Path> {
        self.instance.config.node.tailscale_executable.as_deref()
    }
    fn every_interface_only(&self) -> std::result::Result<(), ApiError> {
        if self.networks.every_interface() {
            return Ok(());
        }
        Err(ApiError(MeshError::input(format!(
            "this computer does not listen on every interface (network = \"{}\", listen_address = {}): it has no trusted networks; run `hmesh setup --full --network direct` to listen on every interface",
            self.instance.config.node.network, self.instance.config.node.listen_address
        ))))
    }
}

/// Look at the networks every few seconds while the daemon runs.
pub async fn watch(state: AppState, mut stop: watch::Receiver<bool>) {
    if !state.networks.every_interface() {
        return;
    }
    loop {
        tokio::select! {
            changed = stop.changed() => if changed.is_err() || *stop.borrow() { return },
            _ = tokio::time::sleep(LOOK_EVERY) => {}
        }
        let observed = netinfo::observe(state.tailscale()).await;
        let _one = state.networks.changing.lock().await;
        state.apply_networks(observed, None, false);
    }
}

/// The networks this computer is on now, and which it trusts.
async fn list(
    State(s): State<AppState>,
    Extension(c): Extension<ConnectionIdentity>,
    headers: HeaderMap,
) -> ApiResult<Value> {
    s.owner(&headers, &c)?;
    if !s.networks.every_interface() {
        // Nothing is trusted on one address; still say what is here.
        return Ok(Json(view(
            false,
            &[],
            &netinfo::observe(s.tailscale()).await,
        )));
    }
    let _one = s.networks.changing.lock().await;
    s.apply_networks(netinfo::observe(s.tailscale()).await, None, false);
    Ok(Json(s.networks_view()))
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TrustNetwork {
    /// One of the subnets this computer is on now (its gateway is filled
    /// in from what is observed).
    pub subnet: IpNet,
    #[serde(default)]
    pub name: Option<String>,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ForgetNetwork {
    pub subnet: IpNet,
    /// Only the entry behind this gateway; every entry for the subnet when omitted.
    #[serde(default)]
    pub gateway: Option<String>,
}

/// The owner trusts a network this computer is on now: other computers on
/// it may connect from then on, without a restart.
async fn trust(
    State(s): State<AppState>,
    Extension(c): Extension<ConnectionIdentity>,
    headers: HeaderMap,
    Json(body): Json<TrustNetwork>,
) -> ApiResult<Value> {
    s.owner(&headers, &c)?;
    s.every_interface_only()?;
    let _one = s.networks.changing.lock().await;
    let observed = netinfo::observe(s.tailscale()).await;
    let mut trusted = s.networks.trusted();
    let entry = trust_network(&mut trusted, &observed, body.subnet, body.name)
        .map_err(|e| ApiError(MeshError::input(e)))?;
    write_trusted(&s.instance.dir, &trusted)?;
    s.apply_networks(observed, Some(trusted), false);
    s.store.audit(
        Some(s.instance.card.node_id),
        "network.trust",
        &entry.subnet.to_string(),
        if entry.gateway.is_some() {
            "with gateway"
        } else {
            "subnet only"
        },
        Uuid::new_v4(),
    )?;
    Ok(Json(s.networks_view()))
}

/// The owner stops trusting a network: connections over it are dropped
/// from then on.
async fn forget(
    State(s): State<AppState>,
    Extension(c): Extension<ConnectionIdentity>,
    headers: HeaderMap,
    Json(body): Json<ForgetNetwork>,
) -> ApiResult<Value> {
    s.owner(&headers, &c)?;
    s.every_interface_only()?;
    let _one = s.networks.changing.lock().await;
    let mut trusted = s.networks.trusted();
    let removed = forget_network(&mut trusted, body.subnet, body.gateway.as_deref());
    if removed == 0 {
        return Err(ApiError(MeshError::input(format!(
            "this computer does not trust {}",
            body.subnet.trunc()
        ))));
    }
    write_trusted(&s.instance.dir, &trusted)?;
    s.apply_networks(netinfo::observe(s.tailscale()).await, Some(trusted), false);
    s.store.audit(
        Some(s.instance.card.node_id),
        "network.forget",
        &body.subnet.trunc().to_string(),
        &format!("{removed} removed"),
        Uuid::new_v4(),
    )?;
    Ok(Json(s.networks_view()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn network(address: &str, subnet: &str, tailnet: bool) -> LocalNetwork {
        LocalNetwork {
            interface: "en0".into(),
            address: address.parse().unwrap(),
            subnet: subnet.parse().unwrap(),
            gateway: None,
            gateway_mac: None,
            tailnet,
        }
    }

    #[test]
    fn the_owner_is_offered_the_untrusted_network_with_the_default_route() {
        let observed = [
            network("192.168.1.20", "192.168.1.0/24", false),
            network("10.0.0.42", "10.0.0.0/24", false),
            network("100.101.102.103", "100.101.102.103/32", true),
        ];
        let shown = view(true, &[], &observed);
        assert_eq!(shown["suggested"]["subnet"], "192.168.1.0/24");
        assert_eq!(shown["current"][0]["trusted"], false);
        assert_eq!(shown["tailnet"], true);
        let lan = TrustedNetwork {
            subnet: "192.168.1.0/24".parse().unwrap(),
            gateway: Some("a0:b1:c2:d3:e4:f5".into()),
            name: Some("Home".into()),
            tailnet: false,
        };
        let trusted = [lan, TrustedNetwork::tailnet()];
        let shown = view(true, &trusted, &observed);
        assert!(shown["suggested"].is_null(), "{shown}");
        assert_eq!(shown["current"][0]["trusted"], true);
        assert_eq!(
            shown["current"][0]["gateway_unverified"], true,
            "no gateway seen"
        );
        assert_eq!(shown["current"][0]["name"], "Home");
        assert_eq!(shown["current"][1]["trusted"], false);
        assert_eq!(shown["current"][2]["trusted"], true);
        assert_eq!(
            open_addresses(&trusted, &observed),
            [
                ("192.168.1.20".parse().unwrap(), false),
                ("100.101.102.103".parse().unwrap(), true)
            ]
        );
        // On one address, nothing is trusted or offered.
        let shown = view(false, &[], &observed);
        assert!(shown["suggested"].is_null());
        assert_eq!(shown["current"][0]["trusted"], false);
    }

    /// Trusted without its gateway (none could be read), a network is
    /// trusted by its subnet alone, and the owner is told what that means.
    #[test]
    fn a_network_trusted_by_subnet_only_is_said_to_be() {
        let observed = [network("192.168.1.20", "192.168.1.0/24", false)];
        let subnet_only = [TrustedNetwork {
            subnet: "192.168.1.0/24".parse().unwrap(),
            gateway: None,
            name: None,
            tailnet: false,
        }];
        let shown = view(true, &subnet_only, &observed);
        assert_eq!(shown["current"][0]["trusted"], true);
        assert_eq!(shown["current"][0]["subnet_only"], true, "{shown}");
        assert_eq!(shown["trusted"][0]["subnet_only"], true, "{shown}");
        let notes = shown["notes"].to_string();
        assert!(
            notes.contains("192.168.1.0/24 is trusted by its subnet only"),
            "{shown}"
        );
        assert!(notes.contains("same subnet"), "{shown}");
        // Behind its gateway: nothing to say.
        let mac = "a0:b1:c2:d3:e4:f5";
        let checked = [TrustedNetwork {
            gateway: Some(mac.into()),
            ..subnet_only[0].clone()
        }];
        let seen = [LocalNetwork {
            gateway: Some("192.168.1.1".parse().unwrap()),
            gateway_mac: Some(mac.into()),
            ..observed[0].clone()
        }];
        let shown = view(true, &checked, &seen);
        assert_eq!(shown["current"][0]["subnet_only"], false);
        assert_eq!(shown["notes"], json!([]), "{shown}");
        // The Tailscale entry is not a subnet.
        let shown = view(true, &[TrustedNetwork::tailnet()], &[]);
        assert_eq!(shown["trusted"][0]["subnet_only"], false);
    }

    /// A hotel Wi-Fi with carrier-grade NAT addresses is not Tailscale: with
    /// the Tailscale entry trusted, a computer next door on that Wi-Fi is
    /// still dropped before TLS.
    #[test]
    fn a_carrier_grade_nat_wifi_stays_closed_with_tailscale_trusted() {
        let observed = hmesh_transport::netinfo::networks_from(
            &[("en0".to_owned(), "100.80.1.5".parse().unwrap(), 16)],
            Some("100.80.0.1".parse().unwrap()),
            Some("a0:b1:c2:d3:e4:f5".into()),
            &hmesh_transport::netinfo::Tailscale::Unknown,
        );
        let trusted = [TrustedNetwork::tailnet()];
        let open = open_addresses(&trusted, &observed);
        assert!(open.is_empty(), "{open:?}");
        let fence = Fence {
            open,
            observed: observed.iter().map(|n| n.address).collect(),
            sources: default_allowed_sources(),
            ..Fence::default()
        };
        assert_eq!(
            fence.door("100.80.1.5".parse().unwrap(), "100.80.7.7".parse().unwrap()),
            hmesh_transport::Door::Closed
        );
        assert!(view(true, &trusted, &observed)["current"][0]["trusted"] == false);
    }
}
