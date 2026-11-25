//! The relays this computer holds a connection to (`hmesh relay use`), so
//! that the computers paired with it reach it through one when nothing else
//! does: behind NAT, on a network it does not trust, or away from the others.
//! It only ever connects out to a relay; what comes through is served like
//! any connection from another computer (TLS with pinned certificates, the
//! peer API only), and the pairing handshake answers through a relay only
//! while pairing is open here. Relay tokens never go into a log, an event,
//! an audit record or what doctor shows.
use super::*;
use hmesh_transport::relay::{self as relay_protocol, RELAYED};

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/local/relays", get(list))
        .route("/local/relays/use", post(use_relay))
        .route("/local/relays/forget", post(forget))
}

/// A relay to hold a connection to: its relay code (`hmesh relay show` on
/// the relay).
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UseRelay {
    pub code: String,
}

/// Connections put through relays served at once.
const RELAYED_CONNECTIONS: usize = 64;

/// Whether a computer configured as `config` is reached through relays:
/// one that takes connections from other computers.
pub fn takes_relayed(config: &NodeConfig) -> bool {
    !config.outbound_only() && config.node.network != "loopback"
}
/// The relays in `config` (checked when it was loaded).
pub fn configured(config: &NodeConfig) -> Vec<RelayAddress> {
    config
        .node
        .relays
        .iter()
        .flatten()
        .filter_map(|code| hmesh_security::validate_relay(code).ok())
        .collect()
}
/// Keep `relays` in node.toml (none: the setting is removed).
pub fn write_relays(dir: &FilePath, relays: &[RelayAddress]) -> hmesh_core::Result<()> {
    let path = dir.join("node.toml");
    let mut config: NodeConfig =
        toml::from_str(&fs::read_to_string(&path).map_err(internal_error)?)
            .map_err(internal_error)?;
    config.node.relays =
        (!relays.is_empty()).then(|| relays.iter().map(ToString::to_string).collect());
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
/// `relays` with `relay` added, replacing one at the same place (a relay
/// whose token was changed).
pub fn with_relay(
    relays: &[RelayAddress],
    relay: RelayAddress,
) -> hmesh_core::Result<Vec<RelayAddress>> {
    let mut relays: Vec<RelayAddress> = relays
        .iter()
        .filter(|known| known.authority() != relay.authority())
        .cloned()
        .collect();
    if relays.len() >= MAX_RELAYS {
        return Err(MeshError::input(format!(
            "this computer holds at most {MAX_RELAYS} relays; hmesh relay forget first"
        )));
    }
    relays.push(relay);
    Ok(relays)
}

impl AppState {
    /// The relays this computer holds a connection to.
    pub(crate) fn relays(&self) -> Vec<RelayAddress> {
        self.relays.borrow().clone()
    }
    /// The relays as its owner sees them: each by its label, and whether
    /// this computer holds it now (and since when).
    pub(crate) fn relays_view(&self) -> Value {
        let held = self
            .relays_held
            .lock()
            .map(|h| h.clone())
            .unwrap_or_default();
        Value::Array(
            self.relays()
                .iter()
                .map(|relay| {
                    let label = relay.label();
                    let (connected, since) = held.get(&label).copied().unwrap_or((false, 0));
                    json!({"relay": label, "connected": connected, "since": (since > 0).then_some(since)})
                })
                .collect(),
        )
    }
    /// A client for the computer `fingerprint` through `relay` (pairing
    /// through a relay).
    pub(crate) fn relay_client(
        &self,
        relay: &str,
        fingerprint: &str,
        bootstrap: bool,
    ) -> std::result::Result<Client, ApiError> {
        if self.instance.config.node.network == "loopback" {
            return Err(ApiError(MeshError::input(
                "a computer set up for one computer pairs only with computers on itself",
            )));
        }
        Client::pinned_through(
            &self.instance,
            relay,
            fingerprint,
            bootstrap,
            &self.relay_dialer,
        )
        .map_err(|error| ApiError(MeshError::input(error.to_string())))
    }
    /// Hold these relays from now on, and tell the paired computers.
    fn set_relays(&self, relays: Vec<RelayAddress>) -> hmesh_core::Result<()> {
        write_relays(&self.instance.dir, &relays)?;
        self.relays.send_replace(relays);
        self.rebuild_addresses();
        Ok(())
    }
    /// This computer's address list again, newer than the last (the relays
    /// it holds changed), told to the paired computers now.
    pub(crate) fn rebuild_addresses(&self) {
        let previous = self.addresses().revision;
        let revision = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|elapsed| elapsed.as_millis() as i64)
            .unwrap_or_default()
            .max(previous + 1);
        let mut list = addresses::local(
            &self.instance.config,
            &self.card(),
            &self.networks.open_endpoints(),
            &self.relays(),
            revision,
        );
        if let Ok(mut current) = self.addresses.write() {
            // Read while the list is held (see `rotate_find_key`).
            list.find_key = self.find_key_told();
            *current = list;
        }
        self.peers_changed();
    }
    fn relay_held(&self, label: &str, held: bool) {
        if let Ok(mut all) = self.relays_held.lock() {
            let was = all.get(label).is_some_and(|(connected, _)| *connected);
            if was != held {
                all.insert(label.to_owned(), (held, now()));
                if held {
                    tracing::info!(relay = %label, "holding a connection to the relay");
                } else {
                    tracing::info!(relay = %label, "the connection to the relay was lost");
                }
            }
        }
    }
}

async fn list(
    State(s): State<AppState>,
    Extension(c): Extension<ConnectionIdentity>,
    headers: HeaderMap,
) -> ApiResult<Value> {
    s.owner(&headers, &c)?;
    Ok(Json(s.relays_view()))
}
async fn use_relay(
    State(s): State<AppState>,
    Extension(c): Extension<ConnectionIdentity>,
    headers: HeaderMap,
    Json(body): Json<UseRelay>,
) -> ApiResult<Value> {
    s.owner(&headers, &c)?;
    if !takes_relayed(&s.instance.config) {
        return Err(ApiError(MeshError::input(
            "this computer only connects out, so nothing reaches it through a relay; it still reaches other computers through theirs",
        )));
    }
    let relay = hmesh_security::validate_relay(&body.code)
        .map_err(|e| ApiError(MeshError::input(e.to_string())))?;
    let label = relay.label();
    s.set_relays(with_relay(&s.relays(), relay)?)?;
    s.store.audit(
        Some(s.instance.card.node_id),
        "relay.use",
        &label,
        "added",
        Uuid::new_v4(),
    )?;
    Ok(Json(s.relays_view()))
}
async fn forget(
    State(s): State<AppState>,
    Extension(c): Extension<ConnectionIdentity>,
    headers: HeaderMap,
) -> ApiResult<Value> {
    s.owner(&headers, &c)?;
    let count = s.relays().len();
    s.set_relays(Vec::new())?;
    s.store.audit(
        Some(s.instance.card.node_id),
        "relay.forget",
        &format!("{count} relays"),
        "removed",
        Uuid::new_v4(),
    )?;
    Ok(Json(s.relays_view()))
}

/// Hold a connection to each relay this computer uses, until `stop`; a
/// relay added or removed is taken up or let go at once.
pub async fn run(state: AppState, mut stop: watch::Receiver<bool>) {
    if !takes_relayed(&state.instance.config) {
        return;
    }
    let mut wanted = state.relays.subscribe();
    let mut holding: HashMap<String, watch::Sender<bool>> = HashMap::new();
    let peers = tailnet_router(state.clone());
    let slots = Arc::new(tokio::sync::Semaphore::new(RELAYED_CONNECTIONS));
    loop {
        let relays = wanted.borrow_and_update().clone();
        let names: Vec<String> = relays.iter().map(ToString::to_string).collect();
        holding.retain(|name, release| {
            let kept = names.contains(name);
            if !kept {
                let _ = release.send(true);
            }
            kept
        });
        if let Ok(mut held) = state.relays_held.lock() {
            let labels: Vec<String> = relays.iter().map(RelayAddress::label).collect();
            held.retain(|label, _| labels.contains(label));
        }
        for relay in relays {
            let name = relay.to_string();
            if holding.contains_key(&name) {
                continue;
            }
            let tls = match state.instance.client_config(relay.fingerprint(), false) {
                Ok(tls) => Arc::new(tls),
                Err(error) => {
                    tracing::warn!(relay = %relay.label(), %error, "the relay cannot be held");
                    continue;
                }
            };
            let (release, released) = watch::channel(false);
            holding.insert(name, release);
            let label = relay.label();
            let arrived = arrived(state.clone(), peers.clone(), slots.clone());
            let shown = state.clone();
            let held: relay_protocol::Held = Arc::new(move |held| shown.relay_held(&label, held));
            tokio::spawn(relay_protocol::hold(relay, tls, arrived, held, released));
        }
        tokio::select! {
            changed = wanted.changed() => if changed.is_err() { break },
            changed = stop.changed() => if changed.is_err() || *stop.borrow() { break },
        }
    }
    for release in holding.values() {
        let _ = release.send(true);
    }
}
/// What to do with a connection put through a relay to this computer: serve
/// it like one from another computer over a network (the peer API only),
/// as from `RELAYED`, never as from its owner.
fn arrived(
    state: AppState,
    peers: Router,
    slots: Arc<tokio::sync::Semaphore>,
) -> relay_protocol::Arrived {
    Arc::new(move |stream| {
        let Ok(slot) = slots.clone().try_acquire_owned() else {
            return;
        };
        let config = match state
            .instance
            .server_config(&state.store.peers().unwrap_or_default())
        {
            Ok(config) => config,
            Err(error) => {
                tracing::error!(%error, "TLS configuration rejected");
                return;
            }
        };
        let (peers, stop) = (peers.clone(), state.shutdown.subscribe());
        tokio::spawn(async move {
            let _slot = slot;
            hmesh_transport::serve_connection(
                stream,
                SocketAddr::new(RELAYED, 0),
                peers,
                config,
                stop,
            )
            .await;
        });
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn relay(authority: &str, token: char) -> RelayAddress {
        RelayAddress::parse(&format!(
            "hmesh-relay://{authority}/{}/{}",
            "ab".repeat(32),
            token.to_string().repeat(64)
        ))
        .unwrap()
    }

    /// A relay used again with a new token replaces the old entry; another
    /// relay is added; there is a limit.
    #[test]
    fn a_relay_used_again_replaces_the_one_at_its_address() {
        let first = relay("203.0.113.10:7443", 'c');
        let relays = with_relay(&[], first.clone()).unwrap();
        let rotated = relay("203.0.113.10:7443", 'd');
        let replaced = with_relay(&relays, rotated.clone()).unwrap();
        assert_eq!(replaced, std::slice::from_ref(&rotated));
        let other = relay("relay.example.com:7443", 'e');
        let both = with_relay(std::slice::from_ref(&rotated), other.clone()).unwrap();
        assert_eq!(both, [rotated, other]);
        let full: Vec<RelayAddress> = (0..MAX_RELAYS)
            .map(|n| relay(&format!("203.0.113.{n}:7443"), 'f'))
            .collect();
        assert!(with_relay(&full, relay("198.51.100.1:7443", 'f')).is_err());
    }

    /// Anyone with a relay's token can ask it for a computer, so through a
    /// relay the pairing handshake answers only while pairing is open, on
    /// any network (a Tailscale computer answers its Tailnet whenever).
    #[tokio::test]
    async fn the_pairing_handshake_answers_through_a_relay_only_while_pairing_is_open() {
        let root = tempfile::tempdir().unwrap();
        let mut config = NodeConfig::default();
        config.node.network = "tailscale".into();
        config.node.expose_to_tailnet = true;
        config.node.listen_address = "100.101.102.103".parse().unwrap();
        let instance = Arc::new(Instance::init(&root.path().join("tailnet"), config).unwrap());
        let store = Store::open(&instance.dir.join("state.sqlite3")).unwrap();
        let (shutdown, _) = watch::channel(false);
        let state = AppState::load(instance, store, shutdown).unwrap();
        let on_tailnet: IpAddr = "100.64.0.9".parse().unwrap();
        assert!(state.handshake_limit(on_tailnet).is_ok());
        let closed = state.handshake_limit(RELAYED).unwrap_err();
        assert_eq!(closed.0.code, ErrorCode::PermissionDenied, "{closed:?}");
        state.open_pairing().unwrap();
        assert!(state.handshake_limit(RELAYED).is_ok());
        assert!(!RELAYED.is_loopback(), "never this computer's owner");
    }

    /// The address list carries the relays of a computer that takes
    /// connections, in full (only paired computers get it), and none of one
    /// that only connects out; the token never shows in debug output.
    #[test]
    fn the_address_list_carries_the_relays_it_holds() {
        let root = tempfile::tempdir().unwrap();
        let held = relay("203.0.113.10:7443", 'c');
        let mut config = NodeConfig::default();
        config.node.network = "direct".into();
        config.node.expose_to_tailnet = true;
        config.node.listen_address = "::".parse().unwrap();
        config.node.trusted_networks = Some(vec![]);
        config.node.host_names = Some(vec![]);
        config.node.relays = Some(vec![held.to_string()]);
        let every = Instance::init(&root.path().join("every"), config).unwrap();
        assert_eq!(configured(&every.config), std::slice::from_ref(&held));
        let list = addresses::local(
            &every.config,
            &every.card,
            &[],
            std::slice::from_ref(&held),
            3,
        );
        assert_eq!(
            list.addresses,
            [every.card.endpoint.clone(), held.to_string()]
        );
        assert!(!format!("{list:?}").contains(held.token()), "{list:?}");
        assert!(format!("{list:?}").contains("hmesh-relay://203.0.113.10:7443"));
        let mut config = NodeConfig::default();
        config.node.network = "direct".into();
        let outbound = Instance::init(&root.path().join("outbound"), config).unwrap();
        assert!(
            addresses::local(&outbound.config, &outbound.card, &[], &[held], 1)
                .addresses
                .is_empty()
        );
    }
}
