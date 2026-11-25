//! Owner and peer RPC boundaries. Every request rechecks current trust/policy.
mod acceptance;
mod addresses;
mod development;
mod discovery;
pub mod finding;
mod integration;
mod nearby;
mod networks;
mod onboarding;
mod pairing;
pub mod passcode;
mod preflight;
mod relay;
mod service_test;
mod services;
mod sessions;
pub use addresses::ManualAddresses;
use anyhow::Context;
use axum::{
    Json, Router,
    extract::{DefaultBodyLimit, Extension, Path, Query, State},
    http::{HeaderMap, StatusCode},
    middleware,
    response::{IntoResponse, Response},
    routing::{get, post},
};
pub use development::TemplateRunRequest;
use hmesh_core::*;
use hmesh_security::{
    Instance, atomic_write, normalize_fingerprint, validate_card, verify_pair_submission,
};
use hmesh_store::Store;
use hmesh_transport::tailscale::{CliTailscale, TailscaleProvider};
use hmesh_transport::{Client, ConnectionIdentity, RouteMemory};
pub use networks::{ForgetNetwork, TrustNetwork, view as networks_view, write_trusted};
pub use onboarding::OnboardingRequest;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    collections::HashMap,
    fs,
    net::{IpAddr, SocketAddr},
    path::Path as FilePath,
    sync::{Arc, Mutex, RwLock},
};
use tokio::{net::TcpListener, sync::watch};
use uuid::Uuid;

#[derive(Clone)]
pub struct AppState {
    managed: services::Manager,
    integration_slots: Arc<tokio::sync::Semaphore>,
    pub instance: Arc<Instance>,
    pub store: Store,
    policy: Arc<RwLock<Policy>>,
    handshake_requests: Arc<Mutex<HandshakeCounts>>,
    /// Direct network: when the pairing window closes, and its passcode.
    pairing: Arc<Mutex<passcode::PairingWindow>>,
    /// Direct network: this computer announced to, and heard from, others.
    nearby: Arc<nearby::Nearby>,
    acceptance: Arc<Mutex<HashMap<Uuid, Value>>>,
    pub shutdown: watch::Sender<bool>,
    sessions: Arc<sessions::Sessions>,
    relay: Arc<relay::ModelRelay>,
    /// Every address paired computers may reach this one at.
    addresses: Arc<RwLock<PeerAddresses>>,
    /// Where each paired computer answered last (kept in the store too).
    routes: Arc<RouteMemory>,
    /// Wakes the announcer: a computer was paired, or a way to reach one changed.
    announce_now: Arc<tokio::sync::Notify>,
    /// This computer's card as it is now (see `card()`).
    card: Arc<RwLock<NodeCard>>,
    /// Listening on every interface: the networks it is on and trusts.
    networks: Arc<networks::Networks>,
    /// What this computer's mark on the local network is made with (see
    /// `finding`). Never logged.
    find_key: Arc<RwLock<[u8; FIND_KEY_BYTES]>>,
}
#[derive(Debug)]
pub struct ApiError(pub MeshError);
impl From<MeshError> for ApiError {
    fn from(error: MeshError) -> Self {
        Self(error)
    }
}
impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let status = match self.0.code {
            ErrorCode::InvalidInput | ErrorCode::UnsupportedVersion => StatusCode::BAD_REQUEST,
            ErrorCode::Unauthenticated => StatusCode::UNAUTHORIZED,
            ErrorCode::PermissionDenied | ErrorCode::ApprovalRequired => StatusCode::FORBIDDEN,
            ErrorCode::TaskConflict => StatusCode::CONFLICT,
            ErrorCode::TaskUnknown | ErrorCode::ProjectNotFound => StatusCode::NOT_FOUND,
            ErrorCode::RateLimited => StatusCode::TOO_MANY_REQUESTS,
            ErrorCode::ServiceExpired => StatusCode::GONE,
            ErrorCode::FailedNeedsInput => StatusCode::CONFLICT,
            ErrorCode::AgentUnavailable
            | ErrorCode::PeerUnreachable
            | ErrorCode::ServiceUnhealthy => StatusCode::SERVICE_UNAVAILABLE,
            _ => StatusCode::INTERNAL_SERVER_ERROR,
        };
        (status, Json(json!({"error":self.0}))).into_response()
    }
}
type ApiResult<T> = std::result::Result<Json<T>, ApiError>;
/// This node only connects out: no other computer may call it.
fn outbound_only() -> ApiError {
    ApiError(MeshError::new(
        ErrorCode::PermissionDenied,
        "this computer only connects out; it accepts no connections from other computers",
    ))
}
/// Anonymous requests per (minute, kind, source); `None` counts them across
/// all sources.
#[derive(Default)]
struct HandshakeCounts(HashMap<(i64, &'static str, Option<IpAddr>), usize>);
impl HandshakeCounts {
    /// Count one request. Refused over `per_minute` from one source, or ten
    /// times that across all of them. Refused requests count nowhere, so one
    /// computer cannot use up the allowance of everyone else.
    fn admit(
        &mut self,
        minute: i64,
        kind: &'static str,
        source: IpAddr,
        per_minute: usize,
    ) -> bool {
        self.0.retain(|key, _| key.0 == minute);
        let mine = (minute, kind, Some(source.to_canonical()));
        let everyone = (minute, kind, None);
        let count = |key| self.0.get(&key).copied().unwrap_or(0);
        if count(mine) >= per_minute || count(everyone) >= per_minute * 10 {
            return false;
        }
        *self.0.entry(mine).or_default() += 1;
        *self.0.entry(everyone).or_default() += 1;
        true
    }
}
#[cfg(test)]
mod handshake_counts {
    use super::*;
    #[test]
    fn each_source_has_its_own_allowance_under_a_shared_ceiling() {
        let mut counts = HandshakeCounts::default();
        let one: IpAddr = "192.168.1.9".parse().unwrap();
        let other: IpAddr = "192.168.1.10".parse().unwrap();
        for _ in 0..3 {
            assert!(counts.admit(7, "handshake", one, 3));
        }
        assert!(!counts.admit(7, "handshake", one, 3));
        // The same address written IPv4-mapped is the same source.
        assert!(!counts.admit(7, "handshake", "::ffff:192.168.1.9".parse().unwrap(), 3));
        // Another computer is still let in, and so is another kind.
        assert!(counts.admit(7, "handshake", other, 3));
        assert!(counts.admit(7, "pair-status", one, 3));
        // One computer asking on and on does not use up the others' share.
        let mut counts = HandshakeCounts::default();
        for _ in 0..600 {
            counts.admit(7, "handshake", one, 3);
        }
        assert!(counts.admit(7, "handshake", other, 3));
        // A new minute starts over.
        assert!(counts.admit(8, "handshake", one, 3));
        // Many sources together stop at ten times the allowance.
        let mut counts = HandshakeCounts::default();
        let admitted = (0..40u8)
            .filter(|n| counts.admit(1, "handshake", IpAddr::from([10, 0, 0, *n]), 3))
            .count();
        assert_eq!(admitted, 30);
    }
}
/// How long the pairing window stays open on a direct network.
const PAIRING_WINDOW_SECONDS: i64 = 600;
fn pairing_closed() -> ApiError {
    ApiError(MeshError::new(
        ErrorCode::PermissionDenied,
        "pairing is closed on this computer: open 配对新电脑 in its panel (or run `hmesh pair show` there), then pair within 10 minutes",
    ))
}
/// Ctrl-C, or SIGTERM on Unix (a login item or service manager stopping
/// the daemon), so it still shuts down cleanly and removes daemon.json.
async fn stop_signal() -> bool {
    #[cfg(unix)]
    {
        use tokio::signal::unix::{SignalKind, signal};
        let Ok(mut terminate) = signal(SignalKind::terminate()) else {
            return tokio::signal::ctrl_c().await.is_ok();
        };
        tokio::select! {
            result = tokio::signal::ctrl_c() => result.is_ok(),
            _ = terminate.recv() => true,
        }
    }
    #[cfg(not(unix))]
    {
        tokio::signal::ctrl_c().await.is_ok()
    }
}
fn internal(error: impl std::fmt::Display) -> ApiError {
    ApiError(MeshError::new(ErrorCode::Internal, error.to_string()))
}
#[derive(Clone, Copy)]
struct Actor {
    node_id: Uuid,
    owner: bool,
}

impl AppState {
    pub fn load(
        instance: Arc<Instance>,
        store: Store,
        shutdown: watch::Sender<bool>,
    ) -> anyhow::Result<Self> {
        let path = policy_path(&instance);
        let policy: Policy = toml::from_str(&fs::read_to_string(path)?)?;
        policy.validate()?;
        // A new list each start: newer than anything told before.
        let revision = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|elapsed| elapsed.as_millis() as i64)
            .unwrap_or_default();
        let find_key = finding::load_key(&store)?;
        let mut addresses = addresses::local(&instance.config, &instance.card, &[], revision);
        let networks = Arc::new(networks::Networks::new(&instance.config));
        let routes_store = store.clone();
        let routes = Arc::new(RouteMemory::persisted(move |fingerprint, origin, at| {
            if let Err(error) = routes_store.remember_route(fingerprint, origin, at) {
                tracing::warn!(code = ?error.code, "where a paired computer answered was not kept");
            }
        }));
        let state = Self {
            managed: services::Manager::default(),
            integration_slots: Arc::new(tokio::sync::Semaphore::new(1)),
            store,
            policy: Arc::new(RwLock::new(policy)),
            handshake_requests: Arc::default(),
            pairing: Arc::new(Mutex::new(passcode::PairingWindow::started())),
            nearby: Arc::default(),
            acceptance: Arc::new(Mutex::new(HashMap::new())),
            relay: Arc::new(relay::ModelRelay::new(shutdown.subscribe())),
            shutdown,
            sessions: Arc::default(),
            addresses: Arc::default(),
            routes,
            announce_now: Arc::default(),
            card: Arc::new(RwLock::new(instance.card.clone())),
            networks,
            find_key: Arc::new(RwLock::new(find_key)),
            instance,
        };
        addresses.find_key = state.find_key_told();
        if let Ok(mut current) = state.addresses.write() {
            *current = addresses;
        }
        Ok(state)
    }
    fn actor(
        &self,
        headers: &HeaderMap,
        connection: &ConnectionIdentity,
    ) -> std::result::Result<Actor, ApiError> {
        if connection.fingerprint.is_none() {
            self.store
                .audit(None, "authenticate", "peer", "denied", Uuid::new_v4())?;
        }
        let fingerprint = connection.fingerprint.as_deref().ok_or_else(|| {
            ApiError(MeshError::new(
                ErrorCode::Unauthenticated,
                "verified client certificate required",
            ))
        })?;
        if fingerprint == self.instance.card.fingerprint {
            let token = headers
                .get("authorization")
                .and_then(|h| h.to_str().ok())
                .and_then(|h| h.strip_prefix("Bearer "))
                .unwrap_or("");
            if connection.address.ip().is_loopback() && self.instance.verify_owner_token(token) {
                return Ok(Actor {
                    node_id: self.instance.card.node_id,
                    owner: true,
                });
            }
            self.store.audit(
                Some(self.instance.card.node_id),
                "owner.authenticate",
                "local-api",
                "denied",
                Uuid::new_v4(),
            )?;
            return Err(ApiError(MeshError::new(
                ErrorCode::Unauthenticated,
                "local owner credential required",
            )));
        }
        if self.instance.config.outbound_only() {
            return Err(outbound_only());
        }
        if let Some(peer) = self.store.peer_by_fingerprint(fingerprint)? {
            return Ok(Actor {
                node_id: peer.card.node_id,
                owner: false,
            });
        }
        self.store
            .audit(None, "authenticate", "peer", "denied", Uuid::new_v4())?;
        Err(ApiError(MeshError::new(
            ErrorCode::PermissionDenied,
            "peer certificate is not currently trusted",
        )))
    }
    fn owner(
        &self,
        headers: &HeaderMap,
        connection: &ConnectionIdentity,
    ) -> std::result::Result<(), ApiError> {
        let actor = self.actor(headers, connection)?;
        if actor.owner {
            Ok(())
        } else {
            self.store.audit(
                Some(actor.node_id),
                "peer.manage",
                "owner-api",
                "denied",
                Uuid::new_v4(),
            )?;
            Err(ApiError(MeshError::new(
                ErrorCode::PermissionDenied,
                "local owner operation",
            )))
        }
    }
    /// Whether this node calls `peer` at all: whether any address it may be
    /// at can be called. A peer that only connects out names a loopback
    /// address in its card and tells no other; a node listening on its
    /// Tailnet IP never calls it (that address is this node's own loopback).
    /// Nodes that themselves use loopback (one computer) call every peer.
    fn reaches(&self, peer: &Peer) -> bool {
        !self.peer_candidates(peer).is_empty()
    }
    fn handshake_limit(&self, source: IpAddr) -> std::result::Result<(), ApiError> {
        self.anonymous_limit("handshake", 60, source)
    }
    /// Open the pairing window for ten minutes from now (direct network:
    /// the pairing handshake answers only while it is open). Returns when
    /// it closes.
    pub fn open_pairing(&self) -> std::result::Result<i64, ApiError> {
        let mut window = self.pairing.lock().map_err(internal)?;
        window
            .open(now(), self.offers_passcode())
            .map_err(internal)?;
        drop(window);
        self.nearby.changed.notify_one();
        Ok(self.pairing_closes_at().unwrap_or_default())
    }
    /// When the pairing window closes, if it is open.
    fn pairing_closes_at(&self) -> Option<i64> {
        let window = self.pairing.lock().ok()?;
        window.is_open(now()).then_some(window.closes_at)
    }
    /// A direct node that takes connections pairs with a passcode too.
    fn offers_passcode(&self) -> bool {
        self.instance.config.node.network == "direct" && !self.instance.config.outbound_only()
    }
    /// Whether the passcode can be used now (announced on the network).
    fn passcode_open(&self) -> bool {
        self.pairing
            .lock()
            .is_ok_and(|window| window.passcode(now()).is_some())
    }
    /// The pairing window as the owner sees it, in status and when opened:
    /// read at once, so its fields belong together. `passcode_used` tells a
    /// used passcode from one whose tries ran out.
    fn pairing_window(&self) -> Value {
        let required = self.offers_passcode();
        let Ok(window) = self.pairing.lock() else {
            return json!({"required": required, "closes_at": null});
        };
        let now = now();
        let mut shown = json!({
            "required": required,
            "closes_at": window.is_open(now).then_some(window.closes_at),
            "boot": window.boot,
        });
        if required {
            let code = window.passcode(now);
            shown["passcode"] = json!(code);
            shown["tries_left"] = json!(if code.is_some() { window.tries_left } else { 0 });
            shown["passcode_used"] = json!(code.is_none() && window.used);
            shown["revision"] = json!(window.revision);
        }
        shown
    }
    /// Anonymous requests per minute, counted separately per kind so that a
    /// computer waiting for its pair request to be answered does not use up
    /// the budget for new pairing handshakes.
    fn anonymous_limit(
        &self,
        kind: &'static str,
        per_minute: usize,
        source: IpAddr,
    ) -> std::result::Result<(), ApiError> {
        // Pairing with this node starts on this node; nobody pairs into it.
        if self.instance.config.outbound_only() {
            return Err(outbound_only());
        }
        // On a direct network anyone on it can reach the port, so the
        // pairing handshake answers only while the owner has pairing open.
        // Status queries name an existing request by its random ID and stay
        // open, so a request sent in time is still answered.
        if self.instance.config.node.network == "direct"
            && kind != "pair-status"
            && self.pairing_closes_at().is_none()
        {
            return Err(pairing_closed());
        }
        let admitted = self.handshake_requests.lock().map_err(internal)?.admit(
            now() / 60,
            kind,
            source,
            per_minute,
        );
        if !admitted {
            Err(ApiError(MeshError::new(
                ErrorCode::RateLimited,
                "handshake request limit reached",
            )))
        } else {
            Ok(())
        }
    }
    fn current_policy(&self) -> Policy {
        // Invalid hot reload never replaces the last valid policy.
        if let Ok(text) = fs::read_to_string(policy_path(&self.instance))
            && text.len() <= 65536
            && let Ok(policy) = toml::from_str::<Policy>(&text)
            && policy.validate().is_ok()
            && let Ok(mut current) = self.policy.write()
        {
            *current = policy;
        }
        self.policy.read().map(|p| p.clone()).unwrap_or_default()
    }
    /// Pairing is the trust boundary: a currently trusted peer may submit, and
    /// its access mode only decides whether local confirmation is needed.
    fn task_decision(&self, actor: Actor) -> Decision {
        if actor.owner {
            return Decision::Allowed;
        }
        match self.store.peer(actor.node_id) {
            Ok(peer) if peer.status == "trusted" => {
                self.current_policy().peer_decision(actor.node_id)
            }
            _ => Decision::Denied,
        }
    }
    fn task_access(&self, actor: Actor, id: Uuid) -> std::result::Result<TaskRecord, ApiError> {
        let task = self.store.task(id)?;
        if actor.owner || actor.node_id == task.origin_node_id {
            Ok(task)
        } else {
            self.store.audit(
                Some(actor.node_id),
                "task.read",
                &id.to_string(),
                "denied",
                Uuid::new_v4(),
            )?;
            Err(ApiError(MeshError::new(
                ErrorCode::PermissionDenied,
                "task belongs to a different origin peer",
            )))
        }
    }
    fn authorize_execution(&self, task: &TaskRecord) -> Result<()> {
        // Rechecked every 100 ms during ACP turns: keep this process-free.
        // Git contracts are enforced by workspace preparation and before spawn.
        preflight::enforce_artifacts(self, &task.request)?;
        if let Some(expected) = &task.request.expected_tools {
            let (_, config) = self.store.agent(task.agent_id)?;
            let mut actual = config.project.allowed_tools.clone();
            let mut expected = expected.clone();
            actual.sort();
            expected.sort();
            if actual != expected {
                return Err(MeshError::new(
                    ErrorCode::PermissionDenied,
                    "Agent tools differ from the reviewed task template",
                ));
            }
        }
        if task.origin_node_id == self.instance.card.node_id {
            return Ok(());
        }
        let peer = self.store.peer(task.origin_node_id)?;
        if peer.status != "trusted" {
            self.store.audit(
                Some(task.origin_node_id),
                "task.start",
                &task.task_id.to_string(),
                "denied",
                Uuid::new_v4(),
            )?;
            return Err(MeshError::new(
                ErrorCode::PermissionDenied,
                "peer pairing was revoked before execution",
            ));
        }
        Ok(())
    }
}
fn policy_path(instance: &Instance) -> std::path::PathBuf {
    let path = &instance.config.security.policy_file;
    if path.is_absolute() {
        path.clone()
    } else {
        instance.dir.join(path)
    }
}

pub fn router(state: AppState) -> Router {
    Router::new()
        .merge(services::routes())
        .merge(integration::routes())
        .merge(development::routes())
        .merge(discovery::routes())
        .merge(preflight::routes())
        .merge(sessions::routes())
        .merge(pairing::routes())
        .merge(passcode::routes())
        .merge(nearby::routes())
        .merge(addresses::routes())
        .merge(networks::routes())
        .route("/hmesh/v1/hello", get(hello))
        .route("/hmesh/v1/pair/challenge", get(challenge))
        .route("/hmesh/v1/pair/requests", post(incoming_pair))
        .route("/hmesh/v1/node", get(node))
        .route("/hmesh/v1/agents", get(agents))
        .route("/hmesh/v1/tasks", post(submit_task))
        .route("/hmesh/v1/tasks/{id}", get(task))
        .route("/hmesh/v1/tasks/{id}/events", get(events))
        .route("/hmesh/v1/tasks/{id}/result", get(task_result))
        .route("/hmesh/v1/tasks/{id}/cancel", post(cancel_task))
        .route("/hmesh/v1/tasks/{id}/verification", get(task_verifications))
        .route("/hmesh/v1/tasks/{id}/artifacts", get(peer_task_artifacts))
        .route("/hmesh/v1/tasks/{id}/workspace", get(peer_task_workspace))
        .route("/hmesh/v1/tasks/{id}/verify", post(peer_verify_task))
        .route("/local/status", get(status))
        .route("/local/pairing/window", post(open_pairing_window))
        .route("/local/queue", get(queue_status))
        .route("/local/tasks/{id}/workspace", get(task_workspace))
        .route("/local/stop", post(stop))
        .route("/local/peers", get(peers))
        .route("/local/pairs", get(pairs))
        .route("/local/pairs/request", post(outgoing_pair))
        .route("/local/pairs/approve", post(approve_pair))
        .route("/local/peers/{id}/revoke", post(revoke))
        .route("/local/peers/{id}/access", post(set_peer_access))
        .route("/local/mesh/agents", get(mesh_agents))
        .route("/local/agents", post(register_agent))
        .route("/local/onboarding/preview", post(onboarding::preview))
        .route("/local/onboarding/register", post(onboarding::register))
        .route("/local/agents/{id}/update", post(update_agent))
        .route("/local/agents/{id}/disable", post(disable_agent))
        .route("/local/agents/{id}/summary", get(agent_summary))
        .route("/local/agents/retired", get(retired_agents))
        .route("/local/agents/{id}/retire", post(retire_agent))
        .route("/local/agents/{id}/restore", post(restore_agent))
        .route("/local/agents/{id}/verify", post(verify_agent))
        .route("/local/agents/{id}/artifacts", post(grant_artifacts))
        .route("/local/agents/remote/{id}", get(remote_agents))
        .route("/local/policy", post(set_policy))
        .route("/local/tasks", get(local_tasks))
        .route("/local/tasks/submit", post(outgoing_task))
        .route("/local/tasks/{id}", get(owner_task))
        .route("/local/requests/{id}/recover", post(recover_request))
        .route("/local/tasks/{id}/events", get(owner_events))
        .route("/local/tasks/{id}/result", get(owner_result))
        .route("/local/tasks/{id}/cancel", post(owner_cancel))
        .route("/local/tasks/{id}/verify", post(verify_task))
        .route("/local/tasks/{id}/artifacts", get(owner_task_artifacts))
        .route("/local/tasks/{id}/archive", post(archive_task_events))
        .route("/local/tasks/{id}/verification", get(owner_verifications))
        .route("/local/tasks/{id}/approve", post(approve_task))
        .route("/local/tasks/{id}/reject", post(reject_task))
        .route("/local/audit", get(audits))
        .layer(DefaultBodyLimit::max(1048576))
        .layer(middleware::from_fn(contract_errors))
        .with_state(state)
}
// Reject the whole owner namespace on the Tailnet listener, even with the
// instance's own certificate and token. Owner RPCs use a separate loopback socket.
pub fn tailnet_router(state: AppState) -> Router {
    router(state).layer(middleware::from_fn(
        |request: axum::extract::Request, next: middleware::Next| async move {
            if !request.uri().path().starts_with("/hmesh/v1/") {
                return ApiError(MeshError::new(
                    ErrorCode::PermissionDenied,
                    "owner API is loopback-only",
                ))
                .into_response();
            }
            next.run(request).await
        },
    ))
}
async fn contract_errors(request: axum::extract::Request, next: middleware::Next) -> Response {
    let response = next.run(request).await;
    if response.status().is_client_error()
        && !response
            .headers()
            .get("content-type")
            .is_some_and(|v| v.to_str().is_ok_and(|v| v.starts_with("application/json")))
    {
        return ApiError(MeshError::input(
            "request does not match the route contract or body limit",
        ))
        .into_response();
    }
    response
}

/// What a daemon listens on.
enum Listeners {
    /// One listener for everyone: loopback (one computer, or a node that
    /// only connects out), or one direct address on loopback.
    One(TcpListener),
    /// Other computers on a Tailnet IP or one direct address, the owner on a
    /// separate loopback listener at the same port.
    Two {
        owner: TcpListener,
        peers: TcpListener,
        scope: hmesh_transport::Scope,
    },
    /// Every interface, one port: each connection goes through the fence.
    Every(TcpListener),
}

pub async fn run(dir: &FilePath) -> anyhow::Result<()> {
    let mut instance = Instance::load(dir)?;
    let _lock = instance.lock_daemon()?;
    // Other computers connect on a Tailnet IP, or on a direct network's
    // address; the owner then uses a separate loopback listener. A node that
    // only connects out listens on loopback for its owner alone. One on
    // every interface has a single port for both, behind its fence.
    let for_peers = instance.config.listens_for_peers();
    if for_peers && instance.config.node.network == "tailscale" {
        CliTailscale::discover(instance.config.node.tailscale_executable.as_deref())?
            .status()
            .await?
            .require_local_ip(instance.config.node.listen_address)?;
    }
    let address = SocketAddr::new(
        instance.config.node.listen_address,
        instance.config.node.listen_port,
    );
    let listeners = if instance.config.every_interface() {
        Listeners::Every(
            hmesh_transport::bind_every_interface(address)
                .context("bind node listener on every interface")?,
        )
    } else {
        let peer_listener = TcpListener::bind(address).await.map_err(|error| {
            if error.kind() == std::io::ErrorKind::AddrNotAvailable
                && instance.config.node.network == "direct"
            {
                anyhow::anyhow!(
                    "this computer no longer has the address {} it is set up to listen on (network = \"direct\"); run `hmesh setup` to use its current address",
                    address.ip()
                )
            } else {
                anyhow::Error::new(error).context("bind node listener on its configured address")
            }
        })?;
        instance.card.endpoint = format!("https://{}", peer_listener.local_addr()?);
        if for_peers {
            let owner = TcpListener::bind((
                std::net::Ipv4Addr::LOCALHOST,
                peer_listener.local_addr()?.port(),
            ))
            .await
            .context("bind separate loopback owner listener (same port)")?;
            Listeners::Two {
                owner,
                peers: peer_listener,
                scope: if instance.config.node.network == "tailscale" {
                    hmesh_transport::Scope::Tailnet
                } else {
                    hmesh_transport::Scope::Sources(Arc::new(instance.config.allowed_sources()))
                },
            }
        } else {
            Listeners::One(peer_listener)
        }
    };
    let store = Store::open(&instance.dir.join("state.sqlite3"))?;
    if let Listeners::Every(listener) = &listeners {
        // Until a trusted network gives it an address: the one its card
        // named last time, at this port, or its owner's loopback.
        let previous = store
            .node(instance.card.node_id)?
            .and_then(|card| hmesh_security::validate_endpoint(&card.endpoint).ok())
            .and_then(|url| {
                url.host_str()?
                    .trim_matches(['[', ']'])
                    .parse::<IpAddr>()
                    .ok()
            })
            .filter(|ip| !ip.is_loopback());
        instance.card.endpoint = format!(
            "https://{}",
            SocketAddr::new(
                previous.unwrap_or(IpAddr::V4(std::net::Ipv4Addr::LOCALHOST)),
                listener.local_addr()?.port()
            )
        );
    }
    let instance = Arc::new(instance);
    let recovered = store.recover_interrupted()?;
    let (shutdown, receiver) = watch::channel(false);
    let state = AppState::load(instance.clone(), store.clone(), shutdown.clone())?;
    if let Listeners::Every(listener) = &listeners {
        state.networks.set_port(listener.local_addr()?.port());
        state.apply_networks(
            hmesh_transport::netinfo::observe(state.tailscale()).await,
            None,
            true,
        );
    }
    let card = state.card();
    store.set_node(&card)?;
    let legacy = state.current_policy();
    if !legacy.grant.is_empty() {
        tracing::warn!(
            grants = legacy.grant.len(),
            "policy [[grant]] entries and agent require_remote_task_approval are no longer used: paired peers' tasks run immediately unless set to confirm with `hmesh peers access <peer> confirm`"
        );
    }
    services::recover(&state)?;
    pairing::resume(&state)?;
    atomic_write(
        &instance.dir.join("daemon.json"),
        &serde_json::to_vec(&card)?,
    )?;
    let signal_shutdown = shutdown.clone();
    let signal = tokio::spawn(async move {
        if stop_signal().await {
            let _ = signal_shutdown.send(true);
        }
    });
    tracing::info!(node_id=%card.node_id, endpoint=%card.endpoint, every_interface=instance.config.every_interface(), recovered, "daemon listening");
    let tls_instance = instance.clone();
    let execution_state = state.clone();
    let relay = state.relay.clone();
    let runtime = tokio::spawn(hmesh_runtime::run_queue(
        store.clone(),
        instance.dir.clone(),
        instance.config.runtime.clone(),
        Arc::new(move |task| execution_state.authorize_execution(task)),
        Some(Arc::new(move |endpoint: &str, key: &str, protocol| {
            relay.route(endpoint, key, protocol)
        })),
        shutdown.subscribe(),
    ));
    let managed = tokio::spawn(services::run(state.clone(), shutdown.subscribe()));
    let workflows = tokio::spawn(development::run(state.clone(), shutdown.subscribe()));
    let session_manager = tokio::spawn(sessions::run(state.clone(), shutdown.subscribe()));
    let announce = tokio::spawn(pairing::announce(state.clone(), shutdown.subscribe()));
    let nearby = tokio::spawn(nearby::run(state.clone(), shutdown.subscribe()));
    let networks = tokio::spawn(networks::watch(state.clone(), shutdown.subscribe()));
    let expiry_store = state.store.clone();
    let ttl = instance.config.security.pending_approval_ttl_seconds;
    let mut expiry_stop = shutdown.subscribe();
    let expiry = tokio::spawn(async move {
        let mut tick = tokio::time::interval(std::time::Duration::from_secs(1));
        loop {
            tokio::select! { changed=expiry_stop.changed()=>{ if changed.is_err() || *expiry_stop.borrow() {break;} }, _=tick.tick()=>{ if let Err(error)=expiry_store.expire_pending_approvals_at(ttl,now()) { tracing::error!(code=?error.code,"approval expiry failed"); } } }
        }
    });
    let state_for_cleanup = state.clone();
    let tls: hmesh_transport::TlsConfigProvider =
        Arc::new(move || tls_instance.server_config(&store.peers()?));
    let result = match listeners {
        Listeners::One(listener) => {
            hmesh_transport::serve(listener, router(state), tls, receiver).await
        }
        Listeners::Two {
            owner,
            peers,
            scope,
        } => {
            let owner = hmesh_transport::serve(owner, router(state.clone()), tls.clone(), receiver);
            let peer = hmesh_transport::serve_peers(
                peers,
                tailnet_router(state),
                tls,
                shutdown.subscribe(),
                scope,
            );
            tokio::try_join!(
                async {
                    let result = owner.await;
                    let _ = shutdown.send(true);
                    result
                },
                async {
                    let result = peer.await;
                    let _ = shutdown.send(true);
                    result
                }
            )
            .map(|_| ())
        }
        Listeners::Every(listener) => {
            let fence = state.networks.fence.clone();
            hmesh_transport::serve_every_interface(
                listener,
                router(state.clone()),
                tailnet_router(state),
                tls,
                receiver,
                fence,
            )
            .await
        }
    };
    let _ = shutdown.send(true);
    let _ = runtime.await;
    let _ = managed.await;
    let _ = workflows.await;
    let _ = session_manager.await;
    announce.abort();
    let _ = nearby.await;
    let _ = networks.await;
    let _ = expiry.await;
    services::recover(&state_for_cleanup)?;
    signal.abort();
    let _ = fs::remove_file(instance.dir.join("daemon.json"));
    result
}

async fn hello(
    State(s): State<AppState>,
    Extension(c): Extension<ConnectionIdentity>,
) -> ApiResult<NodeCard> {
    s.handshake_limit(c.address.ip())?;
    Ok(Json(s.card()))
}
async fn challenge(
    State(s): State<AppState>,
    Extension(c): Extension<ConnectionIdentity>,
) -> ApiResult<PairChallenge> {
    s.handshake_limit(c.address.ip())?;
    Ok(Json(s.store.challenge(s.instance.card.node_id)?))
}
async fn incoming_pair(
    State(s): State<AppState>,
    Extension(c): Extension<ConnectionIdentity>,
    Json(body): Json<PairSubmission>,
) -> ApiResult<PairRecord> {
    s.handshake_limit(c.address.ip())?;
    verify_pair_submission(&body, s.instance.card.node_id).map_err(|_| {
        ApiError(MeshError::new(
            ErrorCode::Unauthenticated,
            "pairing proof rejected",
        ))
    })?;
    if body.card.node_id == s.instance.card.node_id {
        return Err(ApiError(MeshError::input("cannot pair a node with itself")));
    }
    Ok(Json(s.store.accept_pair(&body)?))
}
async fn node(
    State(s): State<AppState>,
    Extension(c): Extension<ConnectionIdentity>,
    headers: HeaderMap,
) -> ApiResult<NodeCard> {
    s.actor(&headers, &c)?;
    Ok(Json(s.card()))
}
async fn agents(
    State(s): State<AppState>,
    Extension(c): Extension<ConnectionIdentity>,
    headers: HeaderMap,
) -> ApiResult<Vec<AgentCard>> {
    // Any authenticated actor is the owner or a currently trusted peer;
    // pairing is enough to see this node's registered agents.
    s.actor(&headers, &c)?;
    Ok(Json(s.store.agents()?))
}
/// Open the pairing window (direct network). Harmless elsewhere: other
/// networks answer the pairing handshake whenever they are reachable.
async fn open_pairing_window(
    State(s): State<AppState>,
    Extension(c): Extension<ConnectionIdentity>,
    headers: HeaderMap,
) -> ApiResult<Value> {
    s.owner(&headers, &c)?;
    s.open_pairing()?;
    Ok(Json(s.pairing_window()))
}
async fn status(
    State(s): State<AppState>,
    Extension(c): Extension<ConnectionIdentity>,
    headers: HeaderMap,
) -> ApiResult<Value> {
    s.owner(&headers, &c)?;
    Ok(Json(
        json!({"node":s.card(),"protocol":PROTOCOL,"network":s.instance.config.node.network,
        "tailscale":match (s.instance.config.node.network.as_str(), s.instance.config.outbound_only()) {("tailscale", true) => "not_checked", ("tailscale", false) => "verified_at_startup", _ => "not_required"},
        "allowed_sources":(s.instance.config.node.network == "direct" && !s.instance.config.outbound_only()).then(|| s.instance.config.allowed_sources()),
        "pairing_window":s.pairing_window(),
        "running":true,"daemon_version":env!("CARGO_PKG_VERSION"),"pid":std::process::id(),"tasks":s.store.task_count()?,"api_features":["panel-task-workflow-v1","reliability-v2","development-workflows-v1","task-preflight-v1","acp-sessions-v1","project-collaboration-v1","task-git-version-v1","agent-settings-v1","agent-service-test-v1","agent-own-login-v1","session-commands-v1","agent-service-relay-v1","pairing-auto-complete-v1","direct-network-v1","agent-projects-v1","pairing-nearby-v1","peer-addresses-v1","trusted-networks-v1","peer-finding-v1"],"schema_version":hmesh_store::SCHEMA_VERSION,"pair_code":hmesh_security::pair_code(&s.card()),"outbound_only":s.instance.config.outbound_only(),"addresses":s.addresses().addresses,"networks":s.networks_view()}),
    ))
}
async fn stop(
    State(s): State<AppState>,
    Extension(c): Extension<ConnectionIdentity>,
    headers: HeaderMap,
) -> ApiResult<Value> {
    s.owner(&headers, &c)?;
    let shutdown = s.shutdown.clone();
    tokio::spawn(async move {
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        let _ = shutdown.send(true);
    });
    Ok(Json(json!({"stopping":true})))
}
async fn peers(
    State(s): State<AppState>,
    Extension(c): Extension<ConnectionIdentity>,
    headers: HeaderMap,
) -> ApiResult<Vec<PeerView>> {
    s.owner(&headers, &c)?;
    let policy = s.current_policy();
    let mut views = vec![];
    for peer in s.store.peers()? {
        let mut view = PeerView {
            access: policy.peer_access(peer.card.node_id),
            peer: peer.clone(),
            node_connectivity: NodeConnectivity::NotProbed,
            registry_readable: false,
            visible_agent_count: None,
            runnable_agent_count: None,
            services_status: "not_supported_in_v0".into(),
            routes: PeerRoutes::default(),
            unreachable_reasons: Vec::new(),
        };
        if peer.status == "trusted" && !s.reaches(&peer) {
            view.node_connectivity = NodeConnectivity::OutboundOnly;
        } else if peer.status == "trusted" {
            // Asked with this computer's certificate first: a direct node
            // answers the anonymous hello only while its pairing window is
            // open. One that no longer accepts this computer (it revoked the
            // pairing there) still answers hello. Long enough for a slow but
            // working first address (three round trips of 400 ms), or for a
            // first that refuses or sends nothing back (given up once another
            // completes a handshake, 200 ms on) and a next one that answers.
            let client = s.peer_client(&peer)?;
            // Left to finish when this gives up waiting: where it stopped is
            // then known (as its last failure) the next time.
            let probing = client.clone();
            let probe =
                tokio::spawn(async move { probing.get::<NodeCard>("/hmesh/v1/node").await });
            let asked =
                match tokio::time::timeout(std::time::Duration::from_millis(1500), probe).await {
                    Ok(Ok(answer)) => Some(answer),
                    _ => None,
                };
            let accepted = matches!(asked, Some(Ok(_)));
            // Whether it answered hello at all: a direct node outside its
            // pairing window answers with a refusal, which still says it is there.
            let mut answered = accepted;
            let online = accepted || {
                let hello = s.peer_hello_client(&peer)?;
                let said = tokio::time::timeout(
                    std::time::Duration::from_millis(750),
                    hello.get::<NodeCard>("/hmesh/v1/hello"),
                )
                .await;
                answered = match &said {
                    Ok(Ok(_)) => true,
                    Ok(Err(error)) => error.code != ErrorCode::PeerUnreachable,
                    Err(_) => false,
                };
                matches!(said, Ok(Ok(_)))
            };
            view.node_connectivity = if online {
                NodeConnectivity::Online
            } else {
                NodeConnectivity::Unreachable
            };
            // Why, in the words the panel says in its own language: where
            // each address stopped (this time, or the last time a call got
            // that far, a minute ago at most), or that it answers but refuses
            // this computer's certificate (it revoked the pairing there).
            let message = match &asked {
                Some(Err(error)) => Some(error.message.clone()),
                Some(Ok(_)) => None,
                None => s
                    .routes
                    .last_failure(&peer.card.fingerprint)
                    .filter(|(_, at)| now() - at <= 60)
                    .map(|(message, _)| message),
            };
            // Refused on the way in, by a computer that answers hello.
            let refused_here = matches!(&asked, Some(Err(error)) if matches!(
                error.code,
                ErrorCode::PeerUnreachable | ErrorCode::PermissionDenied | ErrorCode::Unauthenticated
            ));
            if let Some(message) = message {
                let reasons = hmesh_transport::unreachable_reasons(&message);
                view.unreachable_reasons = if answered && refused_here && reasons.is_empty() {
                    vec!["not_accepted".into()]
                } else if online {
                    Vec::new()
                } else {
                    reasons.into_iter().map(str::to_owned).collect()
                };
            }
            if !accepted {
                view.routes = s.store.peer_routes(peer.card.node_id)?;
                views.push(view);
                continue;
            }
            if let Ok(Ok(agents)) = tokio::time::timeout(
                std::time::Duration::from_millis(750),
                client.get::<Vec<AgentCard>>("/hmesh/v1/agents"),
            )
            .await
            {
                view.registry_readable = true;
                view.visible_agent_count = Some(agents.len());
                view.runnable_agent_count = Some(
                    agents
                        .iter()
                        .filter(|a| {
                            a.probe.managed_run == "passed"
                                && matches!(a.availability.as_str(), "idle" | "busy")
                        })
                        .count(),
                );
            }
        }
        view.routes = s.store.peer_routes(peer.card.node_id)?;
        views.push(view);
    }
    Ok(Json(views))
}
async fn pairs(
    State(s): State<AppState>,
    Extension(c): Extension<ConnectionIdentity>,
    headers: HeaderMap,
) -> ApiResult<Vec<PairRecord>> {
    s.owner(&headers, &c)?;
    Ok(Json(s.store.pairs()?))
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PairTarget {
    pub endpoint: String,
    pub expected_fingerprint: String,
}
async fn outgoing_pair(
    State(s): State<AppState>,
    Extension(c): Extension<ConnectionIdentity>,
    headers: HeaderMap,
    Json(target): Json<PairTarget>,
) -> ApiResult<PairRecord> {
    s.owner(&headers, &c)?;
    let expected = normalize_fingerprint(&target.expected_fingerprint).map_err(internal)?;
    let client =
        Client::new(&s.instance, &target.endpoint, &expected, true, false).map_err(internal)?;
    let card: NodeCard = client.get("/hmesh/v1/hello").await?;
    validate_card(&card).map_err(internal)?;
    if card.fingerprint != expected || card.node_id == s.instance.card.node_id {
        return Err(ApiError(MeshError::new(
            ErrorCode::Unauthenticated,
            "peer identity mismatch",
        )));
    }
    let challenge: PairChallenge = client.get("/hmesh/v1/pair/challenge").await?;
    if challenge.node_id != card.node_id {
        return Err(ApiError(MeshError::new(
            ErrorCode::Unauthenticated,
            "pair challenge belongs to a different node",
        )));
    }
    let submission = s.pair_submission(challenge)?;
    let response: PairRecord = client.post("/hmesh/v1/pair/requests", &submission).await?;
    if response.request_id != submission.request_id {
        return Err(ApiError(MeshError::new(
            ErrorCode::Unauthenticated,
            "pair request correlation failed",
        )));
    }
    let record = PairRecord {
        request_id: submission.request_id,
        card,
        status: "pending".into(),
        direction: "outgoing".into(),
        expires_at: response.expires_at,
    };
    s.store.save_outgoing_pair(&record)?;
    // Trust the other computer as soon as its owner approves this request.
    pairing::watch(s.clone(), record.clone());
    Ok(Json(record))
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PairApproval {
    pub request_id: Uuid,
    pub expected_fingerprint: String,
}
async fn approve_pair(
    State(s): State<AppState>,
    Extension(c): Extension<ConnectionIdentity>,
    headers: HeaderMap,
    Json(body): Json<PairApproval>,
) -> ApiResult<Peer> {
    s.owner(&headers, &c)?;
    let expected = normalize_fingerprint(&body.expected_fingerprint).map_err(internal)?;
    let peer = s.store.approve_pair(body.request_id, &expected)?;
    s.peers_changed();
    s.store.audit(
        Some(s.instance.card.node_id),
        "pair.approve",
        &peer.card.node_id.to_string(),
        "approved",
        Uuid::new_v4(),
    )?;
    Ok(Json(peer))
}
async fn revoke(
    State(s): State<AppState>,
    Extension(c): Extension<ConnectionIdentity>,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
) -> ApiResult<Value> {
    s.owner(&headers, &c)?;
    revoke_pairing(&s, id)?;
    Ok(Json(json!({"revoked":id})))
}
/// Stop trusting the paired computer `id`. In this order: it is no longer
/// trusted first, then this computer's find key changes (the computer
/// revoked knows the old one, so it cannot tell this computer's mark on the
/// local network any more). Whoever reads the address list, then checks
/// whether a computer is trusted (`announce_round`, the list a paired
/// computer asks for), so never hands the new key to the computer revoked.
pub(crate) fn revoke_pairing(s: &AppState, id: Uuid) -> hmesh_core::Result<()> {
    s.store.revoke_peer(id)?;
    sessions::close_for_peer(s, id);
    if let Err(error) = s.rotate_find_key() {
        tracing::warn!(code = ?error.code, "the find key was not changed after a revocation");
    }
    s.store.audit(
        Some(s.instance.card.node_id),
        "pair.revoke",
        &id.to_string(),
        "revoked",
        Uuid::new_v4(),
    )?;
    Ok(())
}
async fn register_agent(
    State(s): State<AppState>,
    Extension(c): Extension<ConnectionIdentity>,
    headers: HeaderMap,
    Json(config): Json<AgentConfig>,
) -> ApiResult<AgentCard> {
    s.owner(&headers, &c)?;
    Ok(Json(register_config(&s, config, Uuid::new_v4()).await?))
}
async fn register_config(
    s: &AppState,
    mut config: AgentConfig,
    id: Uuid,
) -> std::result::Result<AgentCard, ApiError> {
    config.prepare_registration()?;
    config.project.root = fs::canonicalize(&config.project.root).map_err(|_| {
        ApiError(MeshError::new(
            ErrorCode::ProjectNotFound,
            "registered project directory does not exist",
        ))
    })?;
    if !config.project.root.is_dir() {
        return Err(ApiError(MeshError::input(
            "project root is not a directory",
        )));
    }
    let probe = hmesh_adapters::probe(
        &config,
        &s.instance
            .dir
            .join("probes")
            .join(Uuid::new_v4().to_string()),
    )
    .await
    .map_err(internal)?;
    let card = AgentCard {
        agent_id: id,
        node_id: s.instance.card.node_id,
        name: config.agent.name.clone(),
        harness: config.agent.harness,
        project_ids: vec![config.agent.project_id.clone()],
        availability: "unverified".into(),
        capabilities: config.agent.display_capabilities.clone(),
        probe,
        config_options: vec![],
        service_configured: false,
        default_model: config.adapter.model.clone(),
        own_login: None,
    };
    s.store.register_agent(&card, &config)?;
    Ok(card)
}
async fn retired_agents(
    State(s): State<AppState>,
    Extension(c): Extension<ConnectionIdentity>,
    headers: HeaderMap,
) -> ApiResult<Vec<AgentCard>> {
    s.owner(&headers, &c)?;
    Ok(Json(s.store.retired_agents()?))
}
async fn retire_agent(
    State(s): State<AppState>,
    Extension(c): Extension<ConnectionIdentity>,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
) -> ApiResult<AgentCard> {
    s.owner(&headers, &c)?;
    Ok(Json(s.store.retire_agent(id, false)?))
}
async fn restore_agent(
    State(s): State<AppState>,
    Extension(c): Extension<ConnectionIdentity>,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
) -> ApiResult<AgentCard> {
    s.owner(&headers, &c)?;
    Ok(Json(s.store.retire_agent(id, true)?))
}
async fn archive_task_events(
    State(s): State<AppState>,
    Extension(c): Extension<ConnectionIdentity>,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
) -> ApiResult<Value> {
    s.owner(&headers, &c)?;
    Ok(Json(
        json!({"task_id":id,"archived_events":s.store.archive_events(id)?,"replay_preserved":true}),
    ))
}
async fn agent_summary(
    State(s): State<AppState>,
    Extension(c): Extension<ConnectionIdentity>,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
) -> ApiResult<Value> {
    s.owner(&headers, &c)?;
    let (card, config) = s.store.agent(id)?;
    let digest = hmesh_security::sha256_hex(&serde_json::to_vec(&config).map_err(internal)?);
    Ok(Json(
        json!({"agent":card,"configuration_sha256":digest,"project_id":config.agent.project_id,"note":"Configuration digest excludes credential file contents"}),
    ))
}
async fn update_agent(
    State(s): State<AppState>,
    Extension(c): Extension<ConnectionIdentity>,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
    Json(mut config): Json<AgentConfig>,
) -> ApiResult<AgentCard> {
    s.owner(&headers, &c)?;
    config.prepare_registration()?;
    config.project.root = fs::canonicalize(&config.project.root).map_err(internal)?;
    let probe = hmesh_adapters::probe(
        &config,
        &s.instance
            .dir
            .join("probes")
            .join(Uuid::new_v4().to_string()),
    )
    .await
    .map_err(internal)?;
    let (mut card, _) = s.store.agent(id)?;
    card.probe = probe;
    card.name = config.agent.name.clone();
    card.harness = config.agent.harness;
    card.default_model = hmesh_runtime::service::load(&s.instance.dir, id)
        .ok()
        .flatten()
        .and_then(|v| v.default_model_value(config.agent.harness))
        .or_else(|| config.adapter.model.clone());
    card.availability = "unverified".into();
    card.probe.authentication = "not_checked".into();
    card.probe.managed_run = "not_checked".into();
    card.probe.streaming = "not_checked".into();
    if card.probe.version_supported {
        card.probe.unavailable_reason =
            Some("configuration changed; explicit model verification required".into());
    }
    s.store.replace_agent(&card, &config)?;
    Ok(Json(card))
}
async fn disable_agent(
    State(s): State<AppState>,
    Extension(c): Extension<ConnectionIdentity>,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
) -> ApiResult<AgentCard> {
    s.owner(&headers, &c)?;
    let (mut card, config) = s.store.agent(id)?;
    card.availability = "disabled".into();
    card.probe.managed_run = "not_checked".into();
    card.probe.unavailable_reason = Some("disabled by owner".into());
    s.store.replace_agent(&card, &config)?;
    Ok(Json(card))
}

async fn remote_agents(
    State(s): State<AppState>,
    Extension(c): Extension<ConnectionIdentity>,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
) -> ApiResult<Vec<AgentCard>> {
    s.owner(&headers, &c)?;
    let peer = s.store.peer(id)?;
    if peer.status != "trusted" {
        return Err(ApiError(MeshError::new(
            ErrorCode::PermissionDenied,
            "peer is not trusted",
        )));
    }
    if !s.reaches(&peer) {
        return Ok(Json(vec![]));
    }
    let client = s.peer_client(&peer)?;
    Ok(Json(client.get("/hmesh/v1/agents").await?))
}
async fn set_policy(
    State(s): State<AppState>,
    Extension(c): Extension<ConnectionIdentity>,
    headers: HeaderMap,
    Json(policy): Json<Policy>,
) -> ApiResult<Policy> {
    s.owner(&headers, &c)?;
    policy.validate()?;
    atomic_write(
        &policy_path(&s.instance),
        toml::to_string_pretty(&policy)
            .map_err(internal)?
            .as_bytes(),
    )
    .map_err(internal)?;
    *s.policy.write().map_err(internal)? = policy.clone();
    s.store.audit(
        Some(s.instance.card.node_id),
        "policy.update",
        "policy",
        "approved",
        Uuid::new_v4(),
    )?;
    Ok(Json(policy))
}
/// Choose whether one paired peer's tasks start immediately or wait for local
/// confirmation. Rewrites the policy file; comments in it are not preserved.
async fn set_peer_access(
    State(s): State<AppState>,
    Extension(c): Extension<ConnectionIdentity>,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
    Json(body): Json<PeerAccessRequest>,
) -> ApiResult<Value> {
    s.owner(&headers, &c)?;
    s.store
        .peer(id)
        .map_err(|_| MeshError::input("no paired peer has this node ID"))?;
    let path = policy_path(&s.instance);
    {
        // Hold the write lock across read-modify-write so concurrent updates serialize.
        let mut current = s.policy.write().map_err(internal)?;
        let mut policy = fs::read_to_string(&path)
            .ok()
            .and_then(|text| toml::from_str::<Policy>(&text).ok())
            .filter(|policy| policy.validate().is_ok())
            .unwrap_or_else(|| current.clone());
        if body.access == policy.peers.default {
            policy.peers.overrides.remove(&id);
        } else {
            policy.peers.overrides.insert(id, body.access);
        }
        policy.validate()?;
        atomic_write(
            &path,
            toml::to_string_pretty(&policy)
                .map_err(internal)?
                .as_bytes(),
        )
        .map_err(internal)?;
        *current = policy;
    }
    s.store.audit(
        Some(s.instance.card.node_id),
        "peer.access",
        &id.to_string(),
        &body.access.to_string(),
        Uuid::new_v4(),
    )?;
    Ok(Json(json!({"peer_id":id,"access":body.access})))
}
/// The owner's merged directory: this node's agents plus every trusted peer's.
async fn mesh_agents(
    State(s): State<AppState>,
    Extension(c): Extension<ConnectionIdentity>,
    headers: HeaderMap,
) -> ApiResult<Vec<MeshNode>> {
    s.owner(&headers, &c)?;
    let policy = s.current_policy();
    let mut nodes = vec![MeshNode {
        node_id: s.instance.card.node_id,
        name: s.instance.card.display_name.clone(),
        local: true,
        reachable: true,
        access: None,
        error: None,
        agents: s.store.agents()?,
        projects: sessions::usable_projects(&s)?,
    }];
    let mut lookups = tokio::task::JoinSet::new();
    for (index, peer) in s
        .store
        .peers()?
        .into_iter()
        // A computer that only connects out has no agents to offer.
        .filter(|peer| peer.status == "trusted" && s.reaches(peer))
        .enumerate()
    {
        let access = policy.peer_access(peer.card.node_id);
        let client = s.peer_client(&peer);
        lookups.spawn(async move {
            let client_for_projects = client.as_ref().ok().cloned();
            let fetched = match client {
                Ok(client) => match tokio::time::timeout(
                    std::time::Duration::from_secs(3),
                    client.get::<Vec<AgentCard>>("/hmesh/v1/agents"),
                )
                .await
                {
                    Ok(Ok(agents)) => Ok(agents),
                    Ok(Err(error)) => Err(error.message),
                    Err(_) => Err("peer did not answer within 3 seconds".to_owned()),
                },
                Err(error) => Err(error.to_string()),
            };
            let (reachable, error, agents) = match fetched {
                Ok(agents) => (true, None, agents),
                Err(error) => (false, Some(error), vec![]),
            };
            // Older computers do not share projects (404): none to offer.
            let projects = match (reachable, &client_for_projects) {
                (true, Some(client)) => tokio::time::timeout(
                    std::time::Duration::from_secs(3),
                    client.get::<Vec<hmesh_core::projects::SharedProject>>("/hmesh/v1/projects"),
                )
                .await
                .ok()
                .and_then(Result::ok)
                .unwrap_or_default(),
                _ => vec![],
            };
            (
                index,
                MeshNode {
                    node_id: peer.card.node_id,
                    name: peer.card.display_name,
                    local: false,
                    reachable,
                    access: Some(access),
                    error,
                    agents,
                    projects,
                },
            )
        });
    }
    let mut peers = vec![];
    while let Some(joined) = lookups.join_next().await {
        peers.push(joined.map_err(internal)?);
    }
    peers.sort_by_key(|(index, _)| *index);
    nodes.extend(peers.into_iter().map(|(_, node)| node));
    Ok(Json(nodes))
}
async fn submit_task(
    State(s): State<AppState>,
    Extension(c): Extension<ConnectionIdentity>,
    headers: HeaderMap,
    Json(envelope): Json<Envelope<TaskSubmit>>,
) -> ApiResult<TaskRecord> {
    let actor = s.actor(&headers, &c)?;
    Ok(Json(admit_task(&s, actor, envelope)?))
}
fn validate_source_session(
    s: &AppState,
    actor: Actor,
    task: &TaskSubmit,
) -> std::result::Result<(), ApiError> {
    if let Some(id) = task.source_session_id {
        let session = s.store.session(id)?;
        if session.origin.node_id != actor.node_id
            || session.agent_id != task.target_agent_id
            || session.project_id != task.project_id
        {
            return Err(MeshError::new(
                ErrorCode::PermissionDenied,
                "linked task must use its source session's origin, Agent and project",
            )
            .into());
        }
    }
    Ok(())
}

fn admit_task(
    s: &AppState,
    actor: Actor,
    envelope: Envelope<TaskSubmit>,
) -> std::result::Result<TaskRecord, ApiError> {
    envelope.validate(actor.node_id, "task.submit")?;
    envelope.payload.validate()?;
    // A trusted peer may submit; its access mode only selects auto vs confirm.
    let decision = s.task_decision(actor);
    if decision == Decision::Denied {
        s.store.audit(
            Some(actor.node_id),
            "task.submit",
            &envelope.payload.project_id,
            "denied",
            envelope.trace_id,
        )?;
        return Err(ApiError(MeshError::new(
            ErrorCode::PermissionDenied,
            "sender is not a currently paired peer",
        )));
    }
    validate_source_session(s, actor, &envelope.payload)?;
    let (agent, config) = s.store.agent(envelope.payload.target_agent_id)?;
    preflight::enforce_artifacts(s, &envelope.payload)?;
    preflight::enforce_version(s, &envelope.payload)?;
    if let Some(expected) = &envelope.payload.expected_tools {
        let mut actual = config.project.allowed_tools.clone();
        let mut expected = expected.clone();
        actual.sort();
        expected.sort();
        if actual != expected {
            return Err(MeshError::new(
                ErrorCode::PermissionDenied,
                "Agent tool configuration differs from the template",
            )
            .into());
        }
    }
    if !agent.probe.installed || !agent.probe.version_supported {
        return Err(ApiError(MeshError::new(
            ErrorCode::AdapterNotSupported,
            "agent version is not supported",
        )));
    }
    if config.agent.project_id != envelope.payload.project_id {
        return Err(ApiError(MeshError::new(
            ErrorCode::ProjectNotFound,
            "agent is not assigned to this project",
        )));
    }
    if !actor.owner
        && (agent.probe.managed_run != "passed"
            || !matches!(agent.availability.as_str(), "idle" | "busy"))
    {
        return Err(ApiError(MeshError::new(
            ErrorCode::AgentUnavailable,
            "agent has not passed a local managed-run verification",
        )));
    }
    let approval = envelope.payload.requires_approval || decision == Decision::ApprovalRequired;
    Ok(s.store.admit_with_limits(
        s.instance.card.node_id,
        &envelope,
        approval,
        &s.instance.config.runtime,
    )?)
}
async fn task(
    State(s): State<AppState>,
    Extension(c): Extension<ConnectionIdentity>,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
) -> ApiResult<TaskRecord> {
    Ok(Json(s.task_access(s.actor(&headers, &c)?, id)?))
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct EventQuery {
    #[serde(default)]
    after: i64,
}
async fn events(
    State(s): State<AppState>,
    Extension(c): Extension<ConnectionIdentity>,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
    Query(query): Query<EventQuery>,
) -> ApiResult<Vec<TaskEvent>> {
    s.task_access(s.actor(&headers, &c)?, id)?;
    if query.after < 0 {
        return Err(ApiError(MeshError::input(
            "event cursor must be nonnegative",
        )));
    }
    Ok(Json(s.store.events(id, query.after)?))
}
async fn local_tasks(
    State(s): State<AppState>,
    Extension(c): Extension<ConnectionIdentity>,
    headers: HeaderMap,
) -> ApiResult<Value> {
    s.owner(&headers, &c)?;
    let tasks = s.store.tasks()?;
    let mut summaries = s.acceptance.lock().map_err(internal)?.clone();
    // Bound local SQLite work; older rows remain explicitly unobserved in the UI.
    for task in tasks.iter().rev().take(100) {
        let evidence = s.store.verifications(task.task_id)?;
        summaries.insert(task.task_id, acceptance::summary(task, &evidence));
    }
    Ok(Json(
        json!({"executions":tasks,"submitted":s.store.outbound_tasks()?,"acceptance":summaries}),
    ))
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RemoteTask {
    pub peer_id: Uuid,
    pub request_id: Uuid,
    pub task: TaskSubmit,
}
async fn outgoing_task(
    State(s): State<AppState>,
    Extension(c): Extension<ConnectionIdentity>,
    headers: HeaderMap,
    Json(body): Json<RemoteTask>,
) -> ApiResult<TaskRecord> {
    s.owner(&headers, &c)?;
    dispatch_task(&s, body).await.map(Json)
}
async fn dispatch_task(
    s: &AppState,
    body: RemoteTask,
) -> std::result::Result<TaskRecord, ApiError> {
    body.task.validate()?;
    if body.peer_id == s.instance.card.node_id {
        let mut envelope = Envelope::new(s.instance.card.node_id, "task.submit", body.task);
        envelope.request_id = body.request_id;
        let envelope = s.store.remember_outbound(body.peer_id, &envelope)?;
        let task = admit_task(
            s,
            Actor {
                node_id: s.instance.card.node_id,
                owner: true,
            },
            envelope,
        )?;
        s.store.cache_outbound(&task)?;
        return Ok(task);
    }
    let peer = s.store.peer(body.peer_id)?;
    if peer.status != "trusted" {
        return Err(ApiError(MeshError::new(
            ErrorCode::PermissionDenied,
            "peer is not trusted",
        )));
    }
    let client = s.peer_client(&peer)?;
    let mut envelope = Envelope::new(s.instance.card.node_id, "task.submit", body.task);
    envelope.request_id = body.request_id;
    let envelope = s.store.remember_outbound(body.peer_id, &envelope)?;
    let task: TaskRecord = client.post("/hmesh/v1/tasks", &envelope).await?;
    s.store.cache_outbound(&task)?;
    Ok(task)
}
async fn recover_request(
    State(s): State<AppState>,
    Extension(c): Extension<ConnectionIdentity>,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
) -> ApiResult<TaskRecord> {
    s.owner(&headers, &c)?;
    let (peer, envelope) = s.store.outbound_request(id)?;
    if peer == s.instance.card.node_id {
        let task = admit_task(
            &s,
            Actor {
                node_id: s.instance.card.node_id,
                owner: true,
            },
            envelope,
        )?;
        s.store.cache_outbound(&task)?;
        return Ok(Json(task));
    }
    let task_id = envelope.payload.task_id;
    let client = outbound_client(&s, task_id)?
        .ok_or_else(|| ApiError(MeshError::input("request has no outbound peer")))?;
    // Resend only the exact original envelope; the receiver's durable dedup owns admission.
    let task: TaskRecord = client.post("/hmesh/v1/tasks", &envelope).await?;
    if task.task_id != task_id {
        return Err(ApiError(MeshError::new(
            ErrorCode::TaskConflict,
            "recovery returned a different task ID",
        )));
    }
    s.store.cache_outbound(&task)?;
    Ok(Json(task))
}

fn outbound_client(s: &AppState, id: Uuid) -> std::result::Result<Option<Client>, ApiError> {
    let Some(peer_id) = s.store.outbound_peer(id)? else {
        return Ok(None);
    };
    if peer_id == s.instance.card.node_id {
        return Ok(None);
    }
    let peer = s.store.peer(peer_id)?;
    if peer.status != "trusted" {
        return Err(ApiError(MeshError::new(
            ErrorCode::PermissionDenied,
            "outbound task peer has been revoked",
        )));
    }
    Ok(Some(s.peer_client(&peer)?))
}
async fn owner_task(
    State(s): State<AppState>,
    Extension(c): Extension<ConnectionIdentity>,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
) -> ApiResult<TaskRecord> {
    s.owner(&headers, &c)?;
    let task = if let Some(client) = outbound_client(&s, id)? {
        client.get(&format!("/hmesh/v1/tasks/{id}")).await?
    } else {
        s.store.task(id)?
    };
    s.store.cache_outbound(&task)?;
    Ok(Json(task))
}
async fn owner_events(
    State(s): State<AppState>,
    Extension(c): Extension<ConnectionIdentity>,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
    Query(query): Query<EventQuery>,
) -> ApiResult<Vec<TaskEvent>> {
    s.owner(&headers, &c)?;
    if query.after < 0 {
        return Err(ApiError(MeshError::input(
            "event cursor must be nonnegative",
        )));
    }
    if let Some(client) = outbound_client(&s, id)? {
        Ok(Json(
            client
                .get(&format!(
                    "/hmesh/v1/tasks/{id}/events?after={}",
                    query.after
                ))
                .await?,
        ))
    } else {
        Ok(Json(s.store.events(id, query.after)?))
    }
}
/// Keep the tail of a long assembled reply, cut on a character boundary.
fn tail_text(mut text: String) -> (String, bool) {
    if text.len() <= MAX_FINAL_TEXT_BYTES {
        return (text, false);
    }
    let mut start = text.len() - MAX_FINAL_TEXT_BYTES;
    while !text.is_char_boundary(start) {
        start += 1;
    }
    text.drain(..start);
    (text, true)
}
/// Reply text from a result event, or assembled from text deltas for tasks
/// finished by an executor that predates result events.
fn result_from_events(task: TaskRecord, events: &[TaskEvent]) -> TaskResult {
    if let Some(event) = events.iter().rev().find(|e| e.event_type == "result") {
        return TaskResult {
            final_text: event.payload["text"].as_str().map(str::to_owned),
            truncated: event.payload["truncated"].as_bool().unwrap_or(false),
            source: "result_event".into(),
            task,
        };
    }
    // Same rule as the runtime: the text after the last tool call, or the last
    // segment that had text when the run ended on a tool call.
    let (mut current, mut previous) = (String::new(), String::new());
    for event in events {
        match event.payload["type"].as_str() {
            Some("text_delta") => current.push_str(event.payload["text"].as_str().unwrap_or("")),
            Some("tool_event") if !current.trim().is_empty() => {
                previous = std::mem::take(&mut current);
            }
            Some("tool_event") => current.clear(),
            _ => {}
        }
    }
    let text = if current.trim().is_empty() {
        previous
    } else {
        current
    };
    let text = text.trim().to_owned();
    if text.is_empty() {
        return TaskResult {
            task,
            final_text: None,
            truncated: false,
            source: "none".into(),
        };
    }
    let (final_text, truncated) = tail_text(text);
    TaskResult {
        task,
        final_text: Some(final_text),
        truncated,
        source: "assembled_from_events".into(),
    }
}
fn local_result(s: &AppState, task: TaskRecord) -> std::result::Result<TaskResult, ApiError> {
    if let Some(event) = s.store.last_event_of_type(task.task_id, "result")? {
        return Ok(result_from_events(task, &[event]));
    }
    let mut events = vec![];
    let mut after = 0;
    // Bounded fallback for runs recorded before result events existed.
    for _ in 0..64 {
        let page = s.store.events(task.task_id, after)?;
        let Some(last) = page.last() else { break };
        after = last.seq;
        events.extend(page);
    }
    Ok(result_from_events(task, &events))
}
async fn task_result(
    State(s): State<AppState>,
    Extension(c): Extension<ConnectionIdentity>,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
) -> ApiResult<TaskResult> {
    let task = s.task_access(s.actor(&headers, &c)?, id)?;
    Ok(Json(local_result(&s, task)?))
}
async fn owner_result(
    State(s): State<AppState>,
    Extension(c): Extension<ConnectionIdentity>,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
) -> ApiResult<TaskResult> {
    s.owner(&headers, &c)?;
    let Some(client) = outbound_client(&s, id)? else {
        return Ok(Json(local_result(&s, s.store.task(id)?)?));
    };
    match client
        .get::<TaskResult>(&format!("/hmesh/v1/tasks/{id}/result"))
        .await
    {
        Ok(result) => Ok(Json(result)),
        Err(original) => {
            // Older executors have no result endpoint: rebuild from task + events.
            let Ok(task) = client
                .get::<TaskRecord>(&format!("/hmesh/v1/tasks/{id}"))
                .await
            else {
                return Err(ApiError(original));
            };
            let mut events = vec![];
            let mut after = 0;
            for _ in 0..64 {
                let page: Vec<TaskEvent> = client
                    .get(&format!("/hmesh/v1/tasks/{id}/events?after={after}"))
                    .await?;
                let Some(last) = page.last() else { break };
                after = last.seq;
                events.extend(page);
            }
            Ok(Json(result_from_events(task, &events)))
        }
    }
}
async fn owner_cancel(
    State(s): State<AppState>,
    Extension(c): Extension<ConnectionIdentity>,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
) -> ApiResult<TaskRecord> {
    s.owner(&headers, &c)?;
    if let Some(client) = outbound_client(&s, id)? {
        let task = client
            .post(&format!("/hmesh/v1/tasks/{id}/cancel"), &json!({}))
            .await?;
        s.store.cache_outbound(&task)?;
        Ok(Json(task))
    } else {
        cancel_task(State(s), Extension(c), headers, Path(id)).await
    }
}
async fn approve_task(
    State(s): State<AppState>,
    Extension(c): Extension<ConnectionIdentity>,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
) -> ApiResult<TaskRecord> {
    s.owner(&headers, &c)?;
    let pending = s.store.task(id)?;
    if pending.status == TaskStatus::PendingApproval
        && (pending.request.expected_artifacts.is_some()
            || pending.request.expected_tools.is_some()
            || pending.request.git_version.is_some())
    {
        let report =
            preflight::inspect_task(&s, preflight::origin_actor(&s, &pending), &pending.request)?;
        if !report.ready {
            return Err(ApiError(MeshError::new(
                ErrorCode::PermissionDenied,
                "task preflight changed before approval; inspect the original task",
            )));
        }
    }
    let task = s
        .store
        .approve_pending(id, s.instance.config.security.pending_approval_ttl_seconds)?;
    s.store.audit(
        Some(s.instance.card.node_id),
        "task.approve",
        &id.to_string(),
        if task.status == TaskStatus::Queued {
            "approved"
        } else {
            "expired"
        },
        Uuid::new_v4(),
    )?;
    Ok(Json(task))
}
async fn cancel_task(
    State(s): State<AppState>,
    Extension(c): Extension<ConnectionIdentity>,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
) -> ApiResult<TaskRecord> {
    let actor = s.actor(&headers, &c)?;
    s.task_access(actor, id)?;
    let task = s.store.request_cancel(id)?;
    s.store.audit(
        Some(actor.node_id),
        "task.cancel",
        &id.to_string(),
        "accepted",
        Uuid::new_v4(),
    )?;
    Ok(Json(task))
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ArtifactGrant {
    pub paths: Vec<std::path::PathBuf>,
}
async fn grant_artifacts(
    State(s): State<AppState>,
    Extension(c): Extension<ConnectionIdentity>,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
    Json(body): Json<ArtifactGrant>,
) -> ApiResult<Value> {
    s.owner(&headers, &c)?;
    s.store.set_artifact_paths(id, &body.paths)?;
    Ok(Json(json!({"agent_id":id,"artifact_paths":body.paths})))
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct VerifyRequest {
    pub allow_model: bool,
    pub prompt: String,
    #[serde(default)]
    pub limits: RunLimits,
}
async fn verify_agent(
    State(s): State<AppState>,
    Extension(c): Extension<ConnectionIdentity>,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
    Json(body): Json<VerifyRequest>,
) -> ApiResult<TaskRecord> {
    s.owner(&headers, &c)?;
    if !body.allow_model {
        return Err(ApiError(MeshError::new(
            ErrorCode::PermissionDenied,
            "real model verification requires explicit opt-in",
        )));
    }
    let (agent, _) = s.store.agent(id)?;
    if s.store.agent_retired(id)? {
        return Err(ApiError(MeshError::new(
            ErrorCode::AgentUnavailable,
            "restore retired Agent before verification",
        )));
    }
    if !agent.probe.version_supported {
        return Err(ApiError(MeshError::new(
            ErrorCode::AdapterNotSupported,
            "agent version is not supported",
        )));
    }
    let mut task = TaskSubmit::new(id, &agent.project_ids[0], &body.prompt);
    task.requires_approval = false;
    task.limits = body.limits;
    task.validate()?;
    let envelope = Envelope::new(s.instance.card.node_id, "task.submit", task);
    Ok(Json(s.store.admit_with_limits(
        s.instance.card.node_id,
        &envelope,
        false,
        &s.instance.config.runtime,
    )?))
}
async fn reject_task(
    State(s): State<AppState>,
    Extension(c): Extension<ConnectionIdentity>,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
) -> ApiResult<TaskRecord> {
    s.owner(&headers, &c)?;
    let task = s.store.transition(id, TaskStatus::Rejected, None)?;
    s.store.audit(
        Some(s.instance.card.node_id),
        "task.reject",
        &id.to_string(),
        "rejected",
        Uuid::new_v4(),
    )?;
    Ok(Json(task))
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct VerificationRequest {
    pub artifact: std::path::PathBuf,
    pub expected: Value,
}
async fn verify_task(
    State(s): State<AppState>,
    Extension(c): Extension<ConnectionIdentity>,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
    Json(body): Json<VerificationRequest>,
) -> ApiResult<Value> {
    s.owner(&headers, &c)?;
    if let Some(client) = outbound_client(&s, id)? {
        return Ok(Json(
            client
                .post(&format!("/hmesh/v1/tasks/{id}/verify"), &body)
                .await?,
        ));
    }
    Ok(Json(verify_task_as(&s, s.actor(&headers, &c)?, id, body)?))
}
fn artifact_task(
    s: &AppState,
    actor: Actor,
    id: Uuid,
) -> std::result::Result<TaskRecord, ApiError> {
    let task = s.task_access(actor, id)?;
    // task_access already restricts peers to tasks they originated while trusted.
    if task.status != TaskStatus::Succeeded {
        return Err(ApiError(MeshError::new(
            ErrorCode::TaskConflict,
            "only completed successful runs may be verified",
        )));
    }
    Ok(task)
}
async fn queue_status(
    State(s): State<AppState>,
    Extension(c): Extension<ConnectionIdentity>,
    headers: HeaderMap,
) -> ApiResult<Value> {
    s.owner(&headers, &c)?;
    Ok(Json(s.store.queue_status(&s.instance.config.runtime)?))
}
#[derive(Deserialize, Default)]
struct WorkspaceQuery {
    #[serde(default)]
    diff: bool,
    #[serde(default)]
    offset: usize,
}
async fn task_workspace(
    State(s): State<AppState>,
    Extension(c): Extension<ConnectionIdentity>,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
    Query(query): Query<WorkspaceQuery>,
) -> ApiResult<Value> {
    s.owner(&headers, &c)?;
    if let Some(client) = outbound_client(&s, id)? {
        return Ok(Json(
            client
                .get(&format!(
                    "/hmesh/v1/tasks/{id}/workspace?diff={}&offset={}",
                    query.diff, query.offset
                ))
                .await?,
        ));
    }
    Ok(Json(workspace_page(s.store.workspace(id)?, query)?))
}

async fn peer_task_workspace(
    State(s): State<AppState>,
    Extension(c): Extension<ConnectionIdentity>,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
    Query(query): Query<WorkspaceQuery>,
) -> ApiResult<Value> {
    let task = s.task_access(s.actor(&headers, &c)?, id)?;
    // Share only requested collaboration results, with the original sender.
    if task.request.source_session_id.is_none() && task.request.git_version.is_none() {
        return Err(MeshError::new(
            ErrorCode::PermissionDenied,
            "this task has no collaboration workspace contract",
        )
        .into());
    }
    let workspace = s.store.workspace(id)?;
    if query.diff
        && let Some(record) = &workspace
        && let Some(patch) = record["patch"].as_str()
    {
        let (_, config) = s.store.agent(task.agent_id)?;
        if record["patch_contains_credential"] == true
            || patch.contains(s.instance.owner_token())
            || hmesh_runtime::run_secrets(&config)
                .iter()
                .any(|secret| patch.contains(secret))
        {
            return Err(MeshError::new(
                ErrorCode::PermissionDenied,
                "the diff contains a local credential reference; review it on the execution node",
            )
            .into());
        }
    }
    let mut value = workspace_page(workspace, query)?;
    if let Some(fields) = value.as_object_mut() {
        fields.retain(|name, _| {
            matches!(
                name.as_str(),
                "state"
                    | "base_commit"
                    | "actual_git"
                    | "requested_git_version"
                    | "patch"
                    | "patch_offset"
                    | "patch_next_offset"
                    | "patch_size_bytes"
                    | "patch_sha256"
            )
        });
    }
    Ok(Json(value))
}

fn workspace_page(
    value: Option<Value>,
    query: WorkspaceQuery,
) -> std::result::Result<Value, ApiError> {
    let mut value = value.unwrap_or(json!({"state":"shared"}));
    if let Some(patch) = value.get("patch").and_then(Value::as_str) {
        let length = patch.len();
        if query.offset > length || !patch.is_char_boundary(query.offset) {
            return Err(ApiError(MeshError::input("invalid diff byte cursor")));
        }
        if query.diff {
            let mut end = (query.offset + 65536).min(length);
            while !patch.is_char_boundary(end) {
                end -= 1;
            }
            let page = patch[query.offset..end].to_owned();
            value["patch"] = json!(page);
            value["patch_offset"] = json!(query.offset);
            value["patch_next_offset"] = if end < length {
                json!(end)
            } else {
                Value::Null
            };
        } else if let Some(object) = value.as_object_mut() {
            object.remove("patch");
        }
        value["patch_size_bytes"] = json!(length);
    }
    Ok(value)
}
fn artifact_names(s: &AppState, actor: Actor, id: Uuid) -> std::result::Result<Value, ApiError> {
    let task = artifact_task(s, actor, id)?;
    let (_, config) = s.store.agent(task.agent_id)?;
    if let Some(set) = s.store.artifact_set(id)? {
        let names: Vec<_> = set
            .entries
            .iter()
            .filter(|e| {
                e.bytes.is_some()
                    && config
                        .project
                        .artifact_paths
                        .contains(&std::path::PathBuf::from(&e.name))
            })
            .map(|e| e.name.clone())
            .collect();
        let unavailable:Vec<_>=set.entries.iter().filter(|e|e.bytes.is_none()&&config.project.artifact_paths.contains(&std::path::PathBuf::from(&e.name))).map(|e|json!({"name":e.name,"reason":if e.error.as_deref()==Some("purged_by_owner"){"purged_by_owner"}else{"unavailable_at_completion"}})).collect();
        return Ok(
            json!({"task_id":id,"artifacts":names,"source":"completion-snapshot","attempt_id":set.attempt_id,"unavailable":unavailable}),
        );
    }
    Ok(
        json!({"task_id":id,"artifacts":config.project.artifact_paths,"source":"legacy-current-file"}),
    )
}
async fn owner_task_artifacts(
    State(s): State<AppState>,
    Extension(c): Extension<ConnectionIdentity>,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
) -> ApiResult<Value> {
    s.owner(&headers, &c)?;
    if let Some(client) = outbound_client(&s, id)? {
        return Ok(Json(
            client
                .get(&format!("/hmesh/v1/tasks/{id}/artifacts"))
                .await?,
        ));
    }
    Ok(Json(artifact_names(&s, s.actor(&headers, &c)?, id)?))
}
async fn peer_task_artifacts(
    State(s): State<AppState>,
    Extension(c): Extension<ConnectionIdentity>,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
) -> ApiResult<Value> {
    Ok(Json(artifact_names(&s, s.actor(&headers, &c)?, id)?))
}
async fn peer_verify_task(
    State(s): State<AppState>,
    Extension(c): Extension<ConnectionIdentity>,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
    Json(body): Json<VerificationRequest>,
) -> ApiResult<Value> {
    Ok(Json(verify_task_as(&s, s.actor(&headers, &c)?, id, body)?))
}
fn verify_task_as(
    s: &AppState,
    actor: Actor,
    id: Uuid,
    body: VerificationRequest,
) -> std::result::Result<Value, ApiError> {
    let task = artifact_task(s, actor, id)?;
    if let Some(contract) = &task.request.acceptance_contract
        && contract.artifact == body.artifact
        && contract.expected != body.expected
    {
        return Err(
            MeshError::input("expected JSON differs from this task's acceptance contract").into(),
        );
    }
    if body.artifact.as_os_str().is_empty()
        || body
            .artifact
            .components()
            .any(|c| !matches!(c, std::path::Component::Normal(_)))
    {
        return Err(ApiError(MeshError::input(
            "artifact path must be relative without traversal",
        )));
    }
    if body
        .expected
        .as_object()
        .is_none_or(|o| o.is_empty() || o.len() > 16)
        || serde_json::to_vec(&body.expected).map_err(internal)?.len() > 8192
    {
        return Err(ApiError(MeshError::input(
            "expected JSON must contain 1..16 fields and fit in 8192 bytes",
        )));
    }
    let (_, config) = s.store.agent(task.agent_id)?;
    if !config.project.artifact_paths.contains(&body.artifact) {
        return Err(ApiError(MeshError::new(
            ErrorCode::PermissionDenied,
            "artifact is not currently granted",
        )));
    }
    let (mut evidence, digest) = if let Some(set) = s.store.artifact_set(id)? {
        if Some(set.attempt_id) != task.last_attempt_id {
            return Err(ApiError(MeshError::input("snapshot attempt mismatch")));
        }
        let entry = set
            .entries
            .iter()
            .find(|e| std::path::Path::new(&e.name) == body.artifact)
            .ok_or_else(|| ApiError(MeshError::input("artifact was not captured at completion")))?;
        let bytes = entry.bytes.as_ref().ok_or_else(|| {
            ApiError(MeshError::input(
                if entry.error.as_deref() == Some("purged_by_owner") {
                    "artifact snapshot was purged by owner retention"
                } else {
                    "artifact was unavailable at completion"
                },
            ))
        })?;
        let summary = json!({"name":entry.name,"sha256":hmesh_security::sha256_hex(bytes),"size_bytes":bytes.len()});
        let mut evidence =
            hmesh_runtime::verification::verify_bytes(bytes, summary, &body.expected)
                .map_err(internal)?;
        evidence["source"] = json!("completion-snapshot");
        (evidence, set.configuration_sha256)
    } else {
        let mut evidence =
            hmesh_runtime::verification::verify_json(&config, &body.artifact, &body.expected)
                .map_err(|e| ApiError(MeshError::input(e.to_string())))?;
        evidence["source"] = json!("legacy-current-file");
        (
            evidence,
            hmesh_security::sha256_hex(&serde_json::to_vec(&config).map_err(internal)?),
        )
    };
    evidence["task_id"] = json!(id);
    evidence["attempt_id"] = json!(task.last_attempt_id);
    evidence["expected"] = body.expected;
    evidence["configuration_sha256"] = json!(digest);
    let evidence = s.store.record_verification(id, &evidence)?;
    s.store.audit(
        Some(actor.node_id),
        "task.verify",
        &id.to_string(),
        if evidence["passed"] == true {
            "passed"
        } else {
            "failed"
        },
        Uuid::new_v4(),
    )?;
    Ok(evidence)
}
async fn task_verifications(
    State(s): State<AppState>,
    Extension(c): Extension<ConnectionIdentity>,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
) -> ApiResult<Vec<Value>> {
    s.task_access(s.actor(&headers, &c)?, id)?;
    Ok(Json(s.store.verifications(id)?))
}
async fn owner_verifications(
    State(s): State<AppState>,
    Extension(c): Extension<ConnectionIdentity>,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
) -> ApiResult<Vec<Value>> {
    s.owner(&headers, &c)?;
    if let Some(client) = outbound_client(&s, id)? {
        let evidence: Vec<Value> = client
            .get(&format!("/hmesh/v1/tasks/{id}/verification"))
            .await?;
        if let Some(task) = s.store.cached_outbound_task(id)? {
            let mut cache = s.acceptance.lock().map_err(internal)?;
            if cache.len() >= 256
                && !cache.contains_key(&id)
                && let Some(oldest) = cache
                    .iter()
                    .min_by_key(|(_, v)| v["observed_at"].as_i64().unwrap_or(0))
                    .map(|(id, _)| *id)
            {
                cache.remove(&oldest);
            }
            let mut value = acceptance::summary(&task, &evidence);
            value["cached"] = json!(true);
            cache.insert(id, value);
        }
        Ok(Json(evidence))
    } else {
        Ok(Json(s.store.verifications(id)?))
    }
}
async fn audits(
    State(s): State<AppState>,
    Extension(c): Extension<ConnectionIdentity>,
    headers: HeaderMap,
) -> ApiResult<Vec<Value>> {
    s.owner(&headers, &c)?;
    Ok(Json(s.store.audits()?))
}

/// Start `hmesh daemon start` for `dir` in the background, detached from
/// the caller and logging to `<dir>/daemon.log`, and return its owner client
/// once it answers. The data-directory lock keeps a second daemon from
/// running, so starting one while another starts is harmless. A daemon that
/// exits early fails at once with the last line it logged.
pub async fn start_detached(
    binary: &FilePath,
    dir: &FilePath,
    timeout: std::time::Duration,
) -> anyhow::Result<Client> {
    let log_path = dir.join("daemon.log");
    let log = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&log_path)
        .context("open daemon.log")?;
    let logged_before = log.metadata().map(|m| m.len()).unwrap_or(0);
    let mut command = std::process::Command::new(binary);
    command
        .arg("--data-dir")
        .arg(dir)
        .args(["daemon", "start"])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(log);
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        command.process_group(0);
    }
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        // DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP | CREATE_NO_WINDOW
        command.creation_flags(0x0000_0008 | 0x0000_0200 | 0x0800_0000);
    }
    let mut child = command.spawn().context("start the daemon")?;
    // Reap it whenever it ends, so it never lingers as a zombie of the caller.
    let exited = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let flag = exited.clone();
    std::thread::spawn(move || {
        let _ = child.wait();
        flag.store(true, std::sync::atomic::Ordering::SeqCst);
    });
    let deadline = std::time::Instant::now() + timeout;
    let mut exited_at = None;
    loop {
        if let Some(client) = ready(dir).await {
            return Ok(client);
        }
        let now = std::time::Instant::now();
        if exited.load(std::sync::atomic::Ordering::SeqCst) {
            // Another daemon may be starting for the same directory; give
            // it a moment before reporting why this one stopped.
            let since = *exited_at.get_or_insert(now);
            if now.duration_since(since) >= std::time::Duration::from_secs(2) {
                anyhow::bail!(
                    "the daemon stopped: {} (log: {})",
                    last_logged(&log_path, logged_before),
                    log_path.display()
                );
            }
        }
        if now >= deadline {
            anyhow::bail!(
                "the daemon did not start within {} seconds; see {}",
                timeout.as_secs(),
                log_path.display()
            );
        }
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
    }
}
/// The owner client of the daemon for `dir` if it answers now.
pub async fn running(dir: &FilePath) -> Option<Client> {
    ready(dir).await
}
async fn ready(dir: &FilePath) -> Option<Client> {
    let instance = Instance::load(dir).ok()?;
    let client = local_client(&instance).ok()?;
    client.get::<Value>("/local/status").await.ok()?;
    Some(client)
}
/// The last line logged since `offset`, or its error message when it is a
/// structured error.
fn last_logged(path: &FilePath, offset: u64) -> String {
    let text = fs::read(path)
        .map(|bytes| {
            String::from_utf8_lossy(&bytes[(offset as usize).min(bytes.len())..]).into_owned()
        })
        .unwrap_or_default();
    let Some(line) = text.lines().rev().map(str::trim).find(|l| !l.is_empty()) else {
        return "it logged nothing".into();
    };
    serde_json::from_str::<Value>(line)
        .ok()
        .and_then(|v| v["error"]["message"].as_str().map(str::to_owned))
        .unwrap_or_else(|| line.chars().take(300).collect())
}
pub fn local_client(instance: &Instance) -> anyhow::Result<Client> {
    let card: NodeCard = serde_json::from_slice(
        &fs::read(instance.dir.join("daemon.json"))
            .context("daemon has not published a listener")?,
    )?;
    if card.node_id != instance.card.node_id || card.fingerprint != instance.card.fingerprint {
        anyhow::bail!("daemon endpoint identity does not match instance");
    }
    // A daemon listening for other computers also listens on loopback, on
    // the same port, for its owner. Judged by the running daemon's card, not
    // this configuration: setup may have just switched it to only MCP. On
    // every interface the owner always comes in at 127.0.0.1: a card naming
    // ::1 there means a test made ::1 stand in for a network, fenced.
    let url = hmesh_security::validate_endpoint(&card.endpoint)?;
    let endpoint = if hmesh_security::endpoint_is_loopback(&card.endpoint)
        && !instance.config.every_interface()
    {
        card.endpoint
    } else {
        format!(
            "https://127.0.0.1:{}",
            url.port_or_known_default().context("missing daemon port")?
        )
    };
    Client::new(instance, &endpoint, &card.fingerprint, false, true)
}

#[cfg(test)]
mod local_client_tests {
    use super::*;
    /// Setup switched this node to only MCP while its daemon still listens
    /// for other computers: the owner reaches that daemon on loopback, the
    /// same port, to stop it.
    #[test]
    fn the_owner_reaches_a_daemon_on_loopback_whatever_the_configuration_says() {
        let root = tempfile::tempdir().unwrap();
        let mut config = NodeConfig::default();
        config.node.network = "direct".into();
        let instance = Instance::init(&root.path().join("node"), config).unwrap();
        assert!(instance.config.outbound_only());
        let mut card = instance.card.clone();
        card.endpoint = "https://192.168.1.20:39880".into();
        fs::write(
            instance.dir.join("daemon.json"),
            serde_json::to_vec(&card).unwrap(),
        )
        .unwrap();
        local_client(&instance).expect("owner client on loopback");
    }
}
#[cfg(test)]
mod verification_limits_tests {
    use super::*;
    #[test]
    fn owner_verification_limits_keep_defaults_and_validate_explicit_budgets() {
        let old: VerifyRequest =
            serde_json::from_value(json!({"allow_model":true,"prompt":"READY"}))
                .expect("old request");
        assert_eq!(old.limits.max_output_bytes, 1048576);
        let explicit: VerifyRequest = serde_json::from_value(json!({"allow_model":true,"prompt":"READY","limits":{"max_output_bytes":16777216,"timeout_seconds":600}})).expect("explicit request");
        let mut task = TaskSubmit::new(Uuid::new_v4(), "fixture", &explicit.prompt);
        task.limits = explicit.limits;
        assert!(task.validate().is_ok());
        task.limits.max_output_bytes = MAX_RUN_OUTPUT_BYTES + 1;
        assert!(task.validate().is_err());
    }
}

#[cfg(test)]
mod result_tests {
    use super::*;
    fn record() -> TaskRecord {
        let id = Uuid::new_v4();
        TaskRecord {
            task_id: id,
            origin_node_id: Uuid::new_v4(),
            target_node_id: Uuid::new_v4(),
            agent_id: Uuid::new_v4(),
            project_id: "demo".into(),
            status: TaskStatus::Succeeded,
            created_at: 0,
            updated_at: 0,
            last_attempt_id: None,
            request: TaskSubmit::new(Uuid::new_v4(), "demo", "goal"),
            error: None,
        }
    }
    fn event(seq: i64, event_type: &str, payload: Value) -> TaskEvent {
        TaskEvent {
            task_id: Uuid::nil(),
            seq,
            event_id: Uuid::new_v4(),
            attempt_id: None,
            event_type: event_type.into(),
            payload,
            created_at: 0,
        }
    }
    #[test]
    fn result_event_wins_over_assembled_text() {
        let events = [
            event(1, "run", json!({"type":"text_delta","text":"narration"})),
            event(
                2,
                "result",
                json!({"type":"final_text","text":"answer","truncated":true}),
            ),
        ];
        let result = result_from_events(record(), &events);
        assert_eq!(result.final_text.as_deref(), Some("answer"));
        assert!(result.truncated);
        assert_eq!(result.source, "result_event");
    }
    #[test]
    fn older_executors_get_text_assembled_from_events_keeping_the_tail() {
        let events = [
            event(1, "run", json!({"type":"text_delta","text":"Let me "})),
            event(2, "run", json!({"type":"text_delta","text":"check."})),
            event(3, "run", json!({"type":"tool_event","name":"read"})),
            event(4, "run", json!({"type":"text_delta","text":"Done: "})),
            event(5, "run", json!({"type":"text_delta","text":"42"})),
        ];
        let result = result_from_events(record(), &events);
        // Narration before the last tool call is not part of the reply.
        assert_eq!(result.final_text.as_deref(), Some("Done: 42"));
        assert_eq!(result.source, "assembled_from_events");
        assert!(!result.truncated);
        // A run that ends on a tool call keeps the last segment with text.
        let ended_on_tool = result_from_events(record(), &events[..3]);
        assert_eq!(ended_on_tool.final_text.as_deref(), Some("Let me check."));
        let none = result_from_events(record(), &[]);
        assert_eq!((none.final_text, none.source.as_str()), (None, "none"));
        // Over the limit: the tail is kept, cut on a character boundary.
        let (tail, truncated) = tail_text("界".repeat(MAX_FINAL_TEXT_BYTES));
        assert!(truncated && tail.len() <= MAX_FINAL_TEXT_BYTES);
        assert!(tail.chars().all(|c| c == '界'));
    }
}
