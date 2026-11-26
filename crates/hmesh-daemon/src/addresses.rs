//! Every address paired computers can reach this one at, told only to them,
//! and how this computer reaches each of them in turn. The card keeps its
//! one address: older versions read cards strictly.
use super::*;
use std::time::Duration;

/// How long one push or pull of an address list may take.
const EXCHANGE: Duration = Duration::from_secs(5);

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/hmesh/v1/peers/addresses", get(addresses).post(receive))
        .route("/local/peers/{id}/addresses", post(set_manual))
}

/// This computer's address list as paired computers get it: its card's
/// address, then its other addresses on trusted networks (`also`; a
/// computer on every interface), then `https://<name>:<port>` for each host
/// name. A computer that only connects out has none. `revision` tells
/// newer lists from older.
pub fn local(
    config: &NodeConfig,
    card: &NodeCard,
    also: &[String],
    revision: i64,
) -> PeerAddresses {
    let mut wanted = Vec::new();
    if !config.outbound_only() {
        wanted.push(card.endpoint.clone());
        wanted.extend(also.iter().cloned());
        if let Some(port) = hmesh_security::validate_endpoint(&card.endpoint)
            .ok()
            .and_then(|url| url.port_or_known_default())
        {
            for name in host_names(config) {
                wanted.push(format!("https://{name}:{port}"));
            }
        }
    }
    let mut addresses: Vec<String> = Vec::new();
    for address in wanted {
        if let Ok(url) = hmesh_security::validate_peer_address(&address) {
            let origin = url.origin().ascii_serialization();
            if !addresses.contains(&origin) && addresses.len() < MAX_PEER_ADDRESSES {
                addresses.push(origin);
            }
        }
    }
    PeerAddresses {
        node_id: card.node_id,
        revision,
        addresses,
    }
}
/// The names in `host_names`, or by default this computer's own name with
/// `.local` (resolved by mDNS on the local network) when it takes
/// connections from other computers.
fn host_names(config: &NodeConfig) -> Vec<String> {
    match &config.node.host_names {
        Some(names) => names.clone(),
        None if config.node.network != "loopback" => system_host_name()
            .and_then(|name| local_host_name(&name))
            .into_iter()
            .collect(),
        None => Vec::new(),
    }
}
/// A computer's name as a `.local` host name: lowercase, other characters
/// made hyphens, `.local` added unless it ends so already.
pub fn local_host_name(name: &str) -> Option<String> {
    let name: String = name
        .trim()
        .to_ascii_lowercase()
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '.' {
                c
            } else {
                '-'
            }
        })
        .collect();
    let labels: Vec<&str> = name
        .split('.')
        .map(|label| label.trim_matches('-'))
        .filter(|label| !label.is_empty())
        .collect();
    if labels.is_empty() {
        return None;
    }
    let mut name = labels.join(".");
    if !name.ends_with(".local") {
        name.push_str(".local");
    }
    valid_host_name(&name).then_some(name)
}
fn system_host_name() -> Option<String> {
    #[cfg(unix)]
    {
        let mut buffer = [0u8; 256];
        // SAFETY: the buffer is valid for its whole length, and gethostname
        // writes at most that many bytes into it.
        let result = unsafe { libc::gethostname(buffer.as_mut_ptr().cast(), buffer.len()) };
        if result != 0 {
            return None;
        }
        let end = buffer.iter().position(|&b| b == 0).unwrap_or(buffer.len());
        String::from_utf8(buffer[..end].to_vec()).ok()
    }
    #[cfg(windows)]
    {
        std::env::var("COMPUTERNAME").ok()
    }
    #[cfg(not(any(unix, windows)))]
    {
        None
    }
}

impl AppState {
    /// This computer's current address list.
    pub fn addresses(&self) -> PeerAddresses {
        self.addresses
            .read()
            .map(|addresses| addresses.clone())
            .unwrap_or_default()
    }
    /// Where this computer tries `peer`, in order: where it answered last,
    /// its card's address, the ones set here by hand, the ones it told this
    /// computer. Only addresses this computer can call (see `can_call`),
    /// and loopback ones a test made stand in for one of its networks.
    pub(crate) fn peer_candidates(&self, peer: &Peer) -> Vec<String> {
        let routes = self
            .store
            .peer_routes(peer.card.node_id)
            .unwrap_or_default();
        let mut candidates = Vec::new();
        for address in routes
            .last_good
            .into_iter()
            .chain([peer.card.endpoint.clone()])
            .chain(routes.manual)
            .chain(routes.advertised)
        {
            if (hmesh_security::can_call(&self.instance.config, &address)
                || self.networks.names_own_network(&address))
                && !candidates.contains(&address)
            {
                candidates.push(address);
            }
        }
        candidates
    }
    /// A client for a trusted peer, at every address it may be reached at.
    /// None it can call (it only connects out, and no address set here
    /// reaches it) is `PEER_UNREACHABLE`.
    pub fn peer_client(&self, peer: &Peer) -> hmesh_core::Result<Client> {
        self.routed(peer, false)
    }
    /// The same, without this computer's certificate (for `hello`).
    pub(crate) fn peer_hello_client(&self, peer: &Peer) -> hmesh_core::Result<Client> {
        self.routed(peer, true)
    }
    fn routed(&self, peer: &Peer, anonymous: bool) -> hmesh_core::Result<Client> {
        let candidates = self.peer_candidates(peer);
        if candidates.is_empty() {
            return Err(MeshError::new(
                ErrorCode::PeerUnreachable,
                "that computer only connects out, and no address set here reaches it",
            ));
        }
        let build = if anonymous {
            Client::routed_anonymous
        } else {
            Client::routed
        };
        build(
            &self.instance,
            candidates,
            &peer.card.fingerprint,
            self.routes.clone(),
        )
        .map_err(|error| MeshError::new(ErrorCode::Internal, error.to_string()))
    }
    /// A peer was paired, or how to reach one changed: tell and ask the
    /// paired computers now instead of at the next round.
    pub(crate) fn peers_changed(&self) {
        self.announce_now.notify_one();
    }
}

/// A paired computer's own address list (it pushes it here).
async fn receive(
    State(s): State<AppState>,
    Extension(c): Extension<ConnectionIdentity>,
    headers: HeaderMap,
    Json(body): Json<PeerAddresses>,
) -> ApiResult<Value> {
    let actor = s.actor(&headers, &c)?;
    if actor.owner || body.node_id != actor.node_id {
        return Err(ApiError(MeshError::new(
            ErrorCode::PermissionDenied,
            "a computer may only tell its own addresses",
        )));
    }
    let addresses = checked(&body.addresses)?;
    let count = addresses.len();
    let updated = s
        .store
        .set_advertised_addresses(&PeerAddresses { addresses, ..body })?;
    if updated {
        s.store.audit(
            Some(actor.node_id),
            "peer.addresses",
            &format!("{count} addresses"),
            "updated",
            Uuid::new_v4(),
        )?;
    }
    Ok(Json(json!({"updated": updated})))
}
/// This computer's address list, for a paired computer (one that only
/// connects out learns it only by asking).
async fn addresses(
    State(s): State<AppState>,
    Extension(c): Extension<ConnectionIdentity>,
    headers: HeaderMap,
) -> ApiResult<PeerAddresses> {
    if s.actor(&headers, &c)?.owner {
        return Err(ApiError(MeshError::new(
            ErrorCode::PermissionDenied,
            "paired computers ask for this; its owner reads it in /local/status",
        )));
    }
    Ok(Json(s.addresses()))
}
fn checked(addresses: &[String]) -> std::result::Result<Vec<String>, ApiError> {
    hmesh_security::validate_peer_addresses(addresses)
        .map_err(|error| ApiError(MeshError::input(error.to_string())))
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ManualAddresses {
    /// Addresses (`https://host:port`) to try for that computer, besides
    /// what it said; empty clears them.
    pub addresses: Vec<String>,
}
/// The owner sets where to reach a paired computer, kept only here.
async fn set_manual(
    State(s): State<AppState>,
    Extension(c): Extension<ConnectionIdentity>,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
    Json(body): Json<ManualAddresses>,
) -> ApiResult<PeerRoutes> {
    s.owner(&headers, &c)?;
    let peer = s.store.peer(id)?;
    let addresses = checked(&body.addresses)?;
    s.store.set_manual_addresses(id, &addresses)?;
    s.store.audit(
        Some(s.instance.card.node_id),
        "peer.addresses.manual",
        &peer.card.node_id.to_string(),
        &format!("{} addresses", addresses.len()),
        Uuid::new_v4(),
    )?;
    s.peers_changed();
    Ok(Json(s.store.peer_routes(id)?))
}

/// Ask a paired computer for its address list and keep it. Whether it
/// answered at all (an older version without the list answers too).
pub async fn pull(s: &AppState, peer: &Peer, client: &Client) -> bool {
    let told = match tokio::time::timeout(
        EXCHANGE,
        client.get::<PeerAddresses>("/hmesh/v1/peers/addresses"),
    )
    .await
    {
        Ok(Ok(told)) => told,
        Ok(Err(error)) => return error.code != ErrorCode::PeerUnreachable,
        Err(_) => return false,
    };
    if told.node_id == peer.card.node_id
        && let Ok(addresses) = hmesh_security::validate_peer_addresses(&told.addresses)
    {
        let _ = s
            .store
            .set_advertised_addresses(&PeerAddresses { addresses, ..told });
    }
    true
}
/// Tell a paired computer this computer's address list. True once it has
/// it, or when it answered without taking it (an older version): telling it
/// again would not change that.
pub async fn push(s: &AppState, client: &Client) -> bool {
    match tokio::time::timeout(
        EXCHANGE,
        client.post::<_, Value>("/hmesh/v1/peers/addresses", &s.addresses()),
    )
    .await
    {
        Ok(Ok(_)) => true,
        Ok(Err(error)) => error.code != ErrorCode::PeerUnreachable,
        Err(_) => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_computer_name_becomes_a_local_host_name() {
        assert_eq!(
            local_host_name("Jackies-MacBook-Pro.local").as_deref(),
            Some("jackies-macbook-pro.local")
        );
        assert_eq!(local_host_name("studio").as_deref(), Some("studio.local"));
        assert_eq!(
            local_host_name("Build Box_2").as_deref(),
            Some("build-box-2.local")
        );
        assert_eq!(
            local_host_name("-edge-.lan").as_deref(),
            Some("edge.lan.local")
        );
        assert_eq!(local_host_name("..."), None);
        assert_eq!(local_host_name(""), None);
    }

    #[test]
    fn the_address_list_is_the_card_address_then_host_names() {
        let root = tempfile::tempdir().unwrap();
        let mut config = NodeConfig::default();
        config.node.host_names = Some(vec!["studio.local".into(), "localhost".into()]);
        let mut instance = Instance::init(&root.path().join("studio"), config).unwrap();
        instance.card.endpoint = "https://127.0.0.1:38880".into();
        let list = local(&instance.config, &instance.card, &[], 7);
        assert_eq!(list.node_id, instance.card.node_id);
        assert_eq!(list.revision, 7);
        assert_eq!(
            list.addresses,
            [
                "https://127.0.0.1:38880",
                "https://studio.local:38880",
                "https://localhost:38880"
            ]
        );
        // On loopback, no name by default.
        let plain = Instance::init(&root.path().join("plain"), NodeConfig::default()).unwrap();
        assert_eq!(
            local(&plain.config, &plain.card, &[], 1).addresses,
            std::slice::from_ref(&plain.card.endpoint)
        );
        // Only connects out: nothing to tell.
        let mut config = NodeConfig::default();
        config.node.network = "direct".into();
        let outbound = Instance::init(&root.path().join("outbound"), config).unwrap();
        assert!(
            local(&outbound.config, &outbound.card, &[], 1)
                .addresses
                .is_empty()
        );
        // On every interface: its other addresses on trusted networks too.
        let mut config = NodeConfig::default();
        config.node.network = "direct".into();
        config.node.expose_to_tailnet = true;
        config.node.listen_address = "::".parse().unwrap();
        config.node.trusted_networks = Some(vec![]);
        config.node.host_names = Some(vec!["studio.local".into()]);
        let every = Instance::init(&root.path().join("every"), config).unwrap();
        let mut card = every.card.clone();
        card.endpoint = "https://192.168.1.20:38880".into();
        assert_eq!(
            local(
                &every.config,
                &card,
                &[
                    "https://192.168.1.20:38880".into(),
                    "https://[fd12::5]:38880".into(),
                    "https://100.101.102.103:38880".into()
                ],
                2
            )
            .addresses,
            [
                "https://192.168.1.20:38880",
                "https://[fd12::5]:38880",
                "https://100.101.102.103:38880",
                "https://studio.local:38880"
            ]
        );
    }

    /// An older computer has no address lists: it answers (its unknown
    /// routes are INVALID_INPUT), so it counts as told and still gets the
    /// card, and nothing is stored for it.
    #[tokio::test]
    async fn an_older_computer_without_address_lists_counts_as_told() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        let root = tempfile::tempdir().unwrap();
        let a = Arc::new(Instance::init(&root.path().join("new"), NodeConfig::default()).unwrap());
        let store = Store::open(&a.dir.join("state.sqlite3")).unwrap();
        let (shutdown, _) = watch::channel(false);
        let state = AppState::load(a.clone(), store, shutdown).unwrap();
        let mut b = Instance::init(&root.path().join("old"), NodeConfig::default()).unwrap();
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        b.card.endpoint = format!("https://{}", listener.local_addr().unwrap());
        let b = Arc::new(b);
        let cards = Arc::new(AtomicUsize::new(0));
        let counted = cards.clone();
        let app = Router::new()
            .route(
                "/hmesh/v1/peers/card",
                post(move || async move {
                    counted.fetch_add(1, Ordering::SeqCst);
                    Json(json!({"updated": true}))
                }),
            )
            .layer(middleware::from_fn(contract_errors));
        let trusted = vec![Peer {
            card: a.card.clone(),
            status: "trusted".into(),
            approved_at: Some(now()),
        }];
        let server = b.clone();
        let (stop, receiver) = watch::channel(false);
        let task = tokio::spawn(hmesh_transport::serve(
            listener,
            app,
            Arc::new(move || server.server_config(&trusted)),
            receiver,
        ));
        state.store.trust_peer(&b.card).unwrap();
        let peer = state.store.peer(b.card.node_id).unwrap();
        let client = state.peer_client(&peer).unwrap();
        assert!(
            pull(&state, &peer, &client).await,
            "an answer, if not a list"
        );
        assert!(push(&state, &client).await, "told: not asked again");
        assert!(
            state
                .store
                .peer_routes(b.card.node_id)
                .unwrap()
                .advertised
                .is_empty()
        );
        assert!(crate::pairing::peer_told(&state, &client).await);
        assert_eq!(cards.load(Ordering::SeqCst), 1);
        let _ = stop.send(true);
        let _ = task.await;
    }
}
