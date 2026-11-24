//! Owner and peer RPC boundaries. Every request rechecks current trust/policy.
mod acceptance;
mod development;
mod integration;
mod onboarding;
mod preflight;
mod services;
mod sessions;
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
use hmesh_transport::{Client, ConnectionIdentity};
pub use onboarding::OnboardingRequest;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    collections::HashMap,
    fs,
    net::SocketAddr,
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
    handshake_requests: Arc<Mutex<HashMap<i64, usize>>>,
    acceptance: Arc<Mutex<HashMap<Uuid, Value>>>,
    pub shutdown: watch::Sender<bool>,
    sessions: Arc<sessions::Sessions>,
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
        Ok(Self {
            managed: services::Manager::default(),
            integration_slots: Arc::new(tokio::sync::Semaphore::new(1)),
            instance,
            store,
            policy: Arc::new(RwLock::new(policy)),
            handshake_requests: Arc::new(Mutex::new(HashMap::new())),
            acceptance: Arc::new(Mutex::new(HashMap::new())),
            shutdown,
            sessions: Arc::default(),
        })
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
    fn handshake_limit(&self) -> std::result::Result<(), ApiError> {
        let minute = now() / 60;
        let mut counts = self.handshake_requests.lock().map_err(internal)?;
        counts.retain(|key, _| *key == minute);
        let count = counts.entry(minute).or_default();
        *count += 1;
        if *count > 60 {
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
        .merge(preflight::routes())
        .merge(sessions::routes())
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
        .route("/hmesh/v1/tasks/{id}/verify", post(peer_verify_task))
        .route("/local/status", get(status))
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

pub async fn run(dir: &FilePath) -> anyhow::Result<()> {
    let mut instance = Instance::load(dir)?;
    let _lock = instance.lock_daemon()?;
    let tailnet = instance.config.node.network == "tailscale";
    if tailnet {
        CliTailscale::discover(instance.config.node.tailscale_executable.as_deref())?
            .status()
            .await?
            .require_local_ip(instance.config.node.listen_address)?;
    }
    let address = SocketAddr::new(
        instance.config.node.listen_address,
        instance.config.node.listen_port,
    );
    let peer_listener = TcpListener::bind(address)
        .await
        .context("bind node listener on its configured address")?;
    instance.card.endpoint = format!("https://{}", peer_listener.local_addr()?);
    let (listener, peer_listener) = if tailnet {
        let owner = TcpListener::bind((
            std::net::Ipv4Addr::LOCALHOST,
            peer_listener.local_addr()?.port(),
        ))
        .await
        .context("bind separate loopback owner listener (same port)")?;
        (owner, Some(peer_listener))
    } else {
        (peer_listener, None)
    };
    let instance = Arc::new(instance);
    let store = Store::open(&instance.dir.join("state.sqlite3"))?;
    store.set_node(&instance.card)?;
    let recovered = store.recover_interrupted()?;
    let (shutdown, receiver) = watch::channel(false);
    let state = AppState::load(instance.clone(), store.clone(), shutdown.clone())?;
    let legacy = state.current_policy();
    if !legacy.grant.is_empty() {
        tracing::warn!(
            grants = legacy.grant.len(),
            "policy [[grant]] entries and agent require_remote_task_approval are no longer used: paired peers' tasks run immediately unless set to confirm with `hmesh peers access <peer> confirm`"
        );
    }
    services::recover(&state)?;
    atomic_write(
        &instance.dir.join("daemon.json"),
        &serde_json::to_vec(&instance.card)?,
    )?;
    let signal_shutdown = shutdown.clone();
    let signal = tokio::spawn(async move {
        if tokio::signal::ctrl_c().await.is_ok() {
            let _ = signal_shutdown.send(true);
        }
    });
    tracing::info!(node_id=%instance.card.node_id, endpoint=%instance.card.endpoint, recovered, "daemon listening");
    let tls_instance = instance.clone();
    let execution_state = state.clone();
    let runtime = tokio::spawn(hmesh_runtime::run_queue(
        store.clone(),
        instance.dir.clone(),
        instance.config.runtime.clone(),
        Arc::new(move |task| execution_state.authorize_execution(task)),
        shutdown.subscribe(),
    ));
    let managed = tokio::spawn(services::run(state.clone(), shutdown.subscribe()));
    let workflows = tokio::spawn(development::run(state.clone(), shutdown.subscribe()));
    let session_manager = tokio::spawn(sessions::run(state.clone(), shutdown.subscribe()));
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
    let owner = hmesh_transport::serve(listener, router(state.clone()), tls.clone(), receiver);
    let result = if let Some(listener) = peer_listener {
        let peer = hmesh_transport::serve_tailnet(
            listener,
            tailnet_router(state),
            tls,
            shutdown.subscribe(),
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
    } else {
        owner.await
    };
    let _ = shutdown.send(true);
    let _ = runtime.await;
    let _ = managed.await;
    let _ = workflows.await;
    let _ = session_manager.await;
    let _ = expiry.await;
    services::recover(&state_for_cleanup)?;
    signal.abort();
    let _ = fs::remove_file(instance.dir.join("daemon.json"));
    result
}

async fn hello(State(s): State<AppState>) -> ApiResult<NodeCard> {
    s.handshake_limit()?;
    Ok(Json(s.instance.card.clone()))
}
async fn challenge(State(s): State<AppState>) -> ApiResult<PairChallenge> {
    s.handshake_limit()?;
    Ok(Json(s.store.challenge(s.instance.card.node_id)?))
}
async fn incoming_pair(
    State(s): State<AppState>,
    Json(body): Json<PairSubmission>,
) -> ApiResult<PairRecord> {
    s.handshake_limit()?;
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
    Ok(Json(s.instance.card.clone()))
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
async fn status(
    State(s): State<AppState>,
    Extension(c): Extension<ConnectionIdentity>,
    headers: HeaderMap,
) -> ApiResult<Value> {
    s.owner(&headers, &c)?;
    Ok(Json(
        json!({"node":s.instance.card,"protocol":PROTOCOL,"network":s.instance.config.node.network,
        "tailscale":if s.instance.config.node.network == "tailscale" {"verified_at_startup"} else {"not_required"},
        "running":true,"daemon_version":env!("CARGO_PKG_VERSION"),"pid":std::process::id(),"tasks":s.store.tasks()?.len(),"api_features":["panel-task-workflow-v1","reliability-v2","development-workflows-v1","task-preflight-v1","acp-sessions-v1","agent-settings-v1"],"schema_version":9}),
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
        };
        if peer.status == "trusted" {
            let hello = Client::new(
                &s.instance,
                &peer.card.endpoint,
                &peer.card.fingerprint,
                true,
                false,
            )
            .map_err(internal)?;
            view.node_connectivity = if matches!(
                tokio::time::timeout(
                    std::time::Duration::from_millis(750),
                    hello.get::<NodeCard>("/hmesh/v1/hello")
                )
                .await,
                Ok(Ok(_))
            ) {
                NodeConnectivity::Online
            } else {
                NodeConnectivity::Unreachable
            };
            let client = Client::new(
                &s.instance,
                &peer.card.endpoint,
                &peer.card.fingerprint,
                false,
                false,
            )
            .map_err(internal)?;
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
    let submission = s.instance.pair_submission(challenge).map_err(internal)?;
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
    s.store.revoke_peer(id)?;
    sessions::close_for_peer(&s, id);
    s.store.audit(
        Some(s.instance.card.node_id),
        "pair.revoke",
        &id.to_string(),
        "revoked",
        Uuid::new_v4(),
    )?;
    Ok(Json(json!({"revoked":id})))
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
    config.validate()?;
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
    config.validate()?;
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
    card.probe.protocol_probe = "not_checked".into();
    card.probe.streaming = "not_checked".into();
    card.probe.unavailable_reason =
        Some("configuration changed; explicit model verification required".into());
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
    let client = Client::new(
        &s.instance,
        &peer.card.endpoint,
        &peer.card.fingerprint,
        false,
        false,
    )
    .map_err(internal)?;
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
    }];
    let mut lookups = tokio::task::JoinSet::new();
    for (index, peer) in s
        .store
        .peers()?
        .into_iter()
        .filter(|peer| peer.status == "trusted")
        .enumerate()
    {
        let instance = s.instance.clone();
        let access = policy.peer_access(peer.card.node_id);
        lookups.spawn(async move {
            let fetched = match Client::new(
                &instance,
                &peer.card.endpoint,
                &peer.card.fingerprint,
                false,
                false,
            ) {
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
    let (agent, config) = s.store.agent(envelope.payload.target_agent_id)?;
    preflight::enforce_artifacts(s, &envelope.payload)?;
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
    let client = Client::new(
        &s.instance,
        &peer.card.endpoint,
        &peer.card.fingerprint,
        false,
        false,
    )
    .map_err(internal)?;
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
    Ok(Some(
        Client::new(
            &s.instance,
            &peer.card.endpoint,
            &peer.card.fingerprint,
            false,
            false,
        )
        .map_err(internal)?,
    ))
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
            || pending.request.expected_tools.is_some())
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
    let mut value = s.store.workspace(id)?.unwrap_or(json!({"state":"shared"}));
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
    Ok(Json(value))
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

pub fn local_client(instance: &Instance) -> anyhow::Result<Client> {
    let card: NodeCard = serde_json::from_slice(
        &fs::read(instance.dir.join("daemon.json"))
            .context("daemon has not published a listener")?,
    )?;
    if card.node_id != instance.card.node_id || card.fingerprint != instance.card.fingerprint {
        anyhow::bail!("daemon endpoint identity does not match instance");
    }
    let endpoint = if instance.config.node.network == "tailscale" {
        let url = hmesh_security::validate_endpoint(&card.endpoint)?;
        format!(
            "https://127.0.0.1:{}",
            url.port_or_known_default().context("missing daemon port")?
        )
    } else {
        card.endpoint
    };
    Client::new(instance, &endpoint, &card.fingerprint, false, true)
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
