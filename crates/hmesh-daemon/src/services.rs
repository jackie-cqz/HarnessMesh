//! Daemon-owned leases and processes. HTTP only: no CONNECT, arbitrary hosts,
//! shell commands, credential forwarding, WebSocket or unbounded streams.
use super::*;
use axum::{
    body::{Body, to_bytes},
    extract::Request,
};
use base64::{Engine, engine::general_purpose::STANDARD};
use hmesh_core::managed::*;
use hmesh_runtime::process_tree::ProcessTree;
use std::{
    collections::HashSet,
    process::Stdio,
    time::{Duration, Instant},
};
use tokio::{
    process::{Child, Command},
    sync::Semaphore,
    task::JoinSet,
};

const HTTP_LIMIT: usize = 256 * 1024;
type WorkerConfiguration = (bool, Option<Command>, Option<(u16, String)>);
#[derive(Clone, Default)]
pub struct Manager {
    observations: Arc<Mutex<HashMap<(String, Uuid), ResourceStatus>>>,
}
impl Manager {
    fn set(&self, kind: &str, id: Uuid, status: ResourceStatus) {
        if let Ok(mut map) = self.observations.lock() {
            map.insert((kind.into(), id), status);
        }
    }
    fn get(&self, kind: &str, id: Uuid) -> ResourceStatus {
        self.observations
            .lock()
            .ok()
            .and_then(|m| m.get(&(kind.into(), id)).cloned())
            .unwrap_or_default()
    }
}
fn fault(code: ErrorCode, message: &str) -> ApiError {
    MeshError::new(code, message).into()
}
fn peer_client(s: &AppState, id: Uuid) -> std::result::Result<Client, ApiError> {
    let peer = s.store.peer(id)?;
    if peer.status != "trusted" {
        return Err(fault(ErrorCode::PermissionDenied, "peer revoked"));
    }
    Ok(s.peer_client(&peer)?)
}
fn service(
    s: &AppState,
    id: Uuid,
    actor: Actor,
) -> std::result::Result<Managed<ServiceSpec>, ApiError> {
    let record: Managed<ServiceSpec> = s.store.managed_get("service", id)?;
    if !record.active() {
        return Err(fault(ErrorCode::ServiceExpired, "service lease inactive"));
    }
    if !actor.owner && !record.spec.allowed_peer_ids.contains(&actor.node_id) {
        s.store.audit(
            Some(actor.node_id),
            "service.connect",
            &id.to_string(),
            "denied",
            Uuid::new_v4(),
        )?;
        return Err(fault(
            ErrorCode::PermissionDenied,
            "service not shared with this peer",
        ));
    }
    Ok(record)
}
fn card(s: &AppState, record: &Managed<ServiceSpec>) -> ServiceCard {
    let status = s.managed.get("service", record.spec.id);
    ServiceCard {
        id: record.spec.id,
        project_id: record.spec.project_id.clone(),
        name: record.spec.name.clone(),
        expires_at: record.expires_at,
        protocol: "http-buffered-v1".into(),
        health: if status.revision == record.revision {
            status.http_health
        } else {
            "pending".into()
        },
        revision: record.revision,
        request_timeout_seconds: record.spec.request_timeout_seconds,
    }
}
fn view<T: Serialize>(s: &AppState, kind: &str, id: Uuid, record: &Managed<T>) -> Value {
    let spec = serde_json::to_value(&record.spec).unwrap_or(Value::Null);
    let mut status = s.managed.get(kind, id);
    if !record.active() {
        status.state = if record.enabled { "expired" } else { "stopped" }.into();
    }
    json!({"id":id,"name":spec.get("name"),"project_id":spec.get("project_id"),"enabled":record.enabled,"expires_at":record.expires_at,"revision":record.revision,"status":status})
}
fn reserve(
    s: &AppState,
    kind: &str,
    id: Uuid,
    record: &impl Serialize,
) -> std::result::Result<(), ApiError> {
    // Fixed per-kind count also bounds supervisor tasks, even for an owner.
    if s.store.managed_list::<Value>(kind)?.len() >= 16 {
        return Err(fault(
            ErrorCode::RateLimited,
            "16 managed records per kind maximum",
        ));
    }
    s.store.managed_create(kind, id, record)?;
    s.store.audit(
        Some(s.instance.card.node_id),
        &format!("{kind}.create"),
        &id.to_string(),
        "allowed",
        Uuid::new_v4(),
    )?;
    Ok(())
}
pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/local/services", get(local_services).post(publish))
        .route("/local/services/{id}/{action}", post(service_action))
        .route("/local/tunnels", get(local_tunnels).post(add_tunnel))
        .route("/local/tunnels/{id}/{action}", post(tunnel_action))
        .route("/local/connections", get(local_connections).post(connect))
        .route("/local/connections/{id}/revoke", post(disconnect))
        .route("/local/services/remote/{peer}", get(remote_services))
        .route("/hmesh/v1/services", get(peer_services))
        .route("/hmesh/v1/services/{id}", get(peer_service))
        .route("/hmesh/v1/services/{id}/http", post(peer_http))
        .route("/hmesh/v1/services/{id}/restart", post(peer_restart))
}
async fn publish(
    State(s): State<AppState>,
    Extension(c): Extension<ConnectionIdentity>,
    headers: HeaderMap,
    Json(spec): Json<ServiceSpec>,
) -> ApiResult<Value> {
    s.owner(&headers, &c)?;
    spec.validate()?;
    let agents = s.store.agents()?;
    if !agents
        .iter()
        .any(|a| a.project_ids.contains(&spec.project_id))
    {
        return Err(fault(
            ErrorCode::ProjectNotFound,
            "service project must be registered",
        ));
    }
    if let Some(p) = &spec.process {
        let probe = TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, spec.target_port))
            .await
            .map_err(|_| {
                fault(
                    ErrorCode::TaskConflict,
                    "service port already occupied; existing process was not adopted",
                )
            })?;
        drop(probe);
        if !p.executable.is_file() || !p.cwd.is_dir() {
            return Err(fault(
                ErrorCode::InvalidInput,
                "service executable/cwd does not exist",
            ));
        }
        let mut assigned = false;
        for a in agents {
            let (_, config) = s.store.agent(a.agent_id)?;
            if config.agent.project_id == spec.project_id
                && fs::canonicalize(&config.project.root).map_err(internal)?
                    == fs::canonicalize(&p.cwd).map_err(internal)?
            {
                assigned = true;
            }
        }
        if !assigned {
            return Err(fault(
                ErrorCode::PermissionDenied,
                "service cwd must equal registered project root",
            ));
        }
    }
    let record = Managed {
        expires_at: now() + spec.lease_seconds as i64,
        spec,
        enabled: true,
        revision: 1,
    };
    reserve(&s, "service", record.spec.id, &record)?;
    Ok(Json(view(&s, "service", record.spec.id, &record)))
}
async fn add_tunnel(
    State(s): State<AppState>,
    Extension(c): Extension<ConnectionIdentity>,
    headers: HeaderMap,
    Json(spec): Json<TunnelSpec>,
) -> ApiResult<Value> {
    s.owner(&headers, &c)?;
    spec.validate()?;
    if !spec.ssh_executable.is_file()
        || !spec.identity_file.is_file()
        || !spec.known_hosts_file.is_file()
    {
        return Err(fault(
            ErrorCode::InvalidInput,
            "SSH executable/identity/known_hosts missing",
        ));
    }
    let record = Managed {
        expires_at: now() + spec.lease_seconds as i64,
        spec,
        enabled: true,
        revision: 1,
    };
    reserve(&s, "tunnel", record.spec.id, &record)?;
    Ok(Json(view(&s, "tunnel", record.spec.id, &record)))
}
macro_rules! local_list {
    ($name:ident,$kind:literal,$ty:ty) => {
        async fn $name(
            State(s): State<AppState>,
            Extension(c): Extension<ConnectionIdentity>,
            headers: HeaderMap,
        ) -> ApiResult<Vec<Value>> {
            s.owner(&headers, &c)?;
            Ok(Json(
                s.store
                    .managed_list::<Managed<$ty>>($kind)?
                    .iter()
                    .map(|r| view(&s, $kind, r.spec.id, r))
                    .collect(),
            ))
        }
    };
}
local_list!(local_services, "service", ServiceSpec);
local_list!(local_tunnels, "tunnel", TunnelSpec);
local_list!(local_connections, "connection", ConnectSpec);
fn change<T: Serialize + serde::de::DeserializeOwned>(
    s: &AppState,
    kind: &str,
    id: Uuid,
    action: &str,
) -> std::result::Result<Value, ApiError> {
    let mut r: Managed<T> = s.store.managed_get(kind, id)?;
    match action {
        "revoke" => r.enabled = false,
        "restart" if r.active() => r.revision += 1,
        _ => {
            return Err(fault(
                ErrorCode::InvalidInput,
                "only revoke or restart of an active lease is supported; publish a new id after expiry",
            ));
        }
    }
    s.store.managed_put(kind, id, &r)?;
    s.store.audit(
        Some(s.instance.card.node_id),
        &format!("{kind}.{action}"),
        &id.to_string(),
        "allowed",
        Uuid::new_v4(),
    )?;
    Ok(view(s, kind, id, &r))
}
async fn service_action(
    State(s): State<AppState>,
    Extension(c): Extension<ConnectionIdentity>,
    headers: HeaderMap,
    Path((id, action)): Path<(Uuid, String)>,
) -> ApiResult<Value> {
    s.owner(&headers, &c)?;
    Ok(Json(change::<ServiceSpec>(&s, "service", id, &action)?))
}
async fn tunnel_action(
    State(s): State<AppState>,
    Extension(c): Extension<ConnectionIdentity>,
    headers: HeaderMap,
    Path((id, action)): Path<(Uuid, String)>,
) -> ApiResult<Value> {
    s.owner(&headers, &c)?;
    Ok(Json(change::<TunnelSpec>(&s, "tunnel", id, &action)?))
}
async fn disconnect(
    State(s): State<AppState>,
    Extension(c): Extension<ConnectionIdentity>,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
) -> ApiResult<Value> {
    s.owner(&headers, &c)?;
    Ok(Json(change::<ConnectSpec>(&s, "connection", id, "revoke")?))
}
async fn peer_services(
    State(s): State<AppState>,
    Extension(c): Extension<ConnectionIdentity>,
    headers: HeaderMap,
) -> ApiResult<Vec<ServiceCard>> {
    let actor = s.actor(&headers, &c)?;
    Ok(Json(
        s.store
            .managed_list::<Managed<ServiceSpec>>("service")?
            .iter()
            .filter(|r| {
                r.active() && (actor.owner || r.spec.allowed_peer_ids.contains(&actor.node_id))
            })
            .map(|r| card(&s, r))
            .collect(),
    ))
}
async fn peer_service(
    State(s): State<AppState>,
    Extension(c): Extension<ConnectionIdentity>,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
) -> ApiResult<ServiceCard> {
    let actor = s.actor(&headers, &c)?;
    Ok(Json(card(&s, &service(&s, id, actor)?)))
}
async fn remote_services(
    State(s): State<AppState>,
    Extension(c): Extension<ConnectionIdentity>,
    headers: HeaderMap,
    Path(peer): Path<Uuid>,
) -> ApiResult<Vec<ServiceCard>> {
    s.owner(&headers, &c)?;
    Ok(Json(
        peer_client(&s, peer)?.get("/hmesh/v1/services").await?,
    ))
}
async fn connect(
    State(s): State<AppState>,
    Extension(c): Extension<ConnectionIdentity>,
    headers: HeaderMap,
    Json(spec): Json<ConnectSpec>,
) -> ApiResult<Value> {
    s.owner(&headers, &c)?;
    lease(spec.lease_seconds)?;
    let card: ServiceCard = peer_client(&s, spec.peer_id)?
        .get(&format!("/hmesh/v1/services/{}", spec.service_id))
        .await?;
    let record = Managed {
        expires_at: (now() + spec.lease_seconds as i64).min(card.expires_at),
        spec,
        enabled: true,
        revision: 1,
    };
    reserve(&s, "connection", record.spec.id, &record)?;
    Ok(Json(view(&s, "connection", record.spec.id, &record)))
}
fn http_client() -> anyhow::Result<reqwest::Client> {
    Ok(reqwest::Client::builder()
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        // The owner's total request budget covers connect, headers and body.
        // A shorter hidden phase deadline caused false failures under load.
        .timeout(Duration::from_secs(30))
        .build()?)
}
pub(crate) async fn bounded_body(
    mut response: reqwest::Response,
) -> std::result::Result<Vec<u8>, ApiError> {
    let mut bytes = vec![];
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|_| fault(ErrorCode::ServiceUnhealthy, "service response failed"))?
    {
        if bytes.len() + chunk.len() > HTTP_LIMIT {
            return Err(fault(ErrorCode::RateLimited, "HTTP body exceeds 256 KiB"));
        }
        bytes.extend_from_slice(&chunk);
    }
    Ok(bytes)
}
async fn peer_http(
    State(s): State<AppState>,
    Extension(c): Extension<ConnectionIdentity>,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
    Json(call): Json<HttpCall>,
) -> ApiResult<HttpReply> {
    let actor = s.actor(&headers, &c)?;
    let record = service(&s, id, actor)?;
    if !matches!(
        call.method.as_str(),
        "GET" | "HEAD" | "POST" | "PUT" | "PATCH" | "DELETE" | "OPTIONS"
    ) || !http_path(&call.path)
        || call.body_base64.len() > HTTP_LIMIT * 4 / 3 + 8
    {
        return Err(fault(ErrorCode::InvalidInput, "invalid bounded HTTP call"));
    }
    let body = STANDARD
        .decode(&call.body_base64)
        .map_err(|_| fault(ErrorCode::InvalidInput, "invalid base64"))?;
    if body.len() > HTTP_LIMIT {
        return Err(fault(ErrorCode::RateLimited, "HTTP body exceeds 256 KiB"));
    }
    let mut request = http_client()
        .map_err(internal)?
        .request(
            reqwest::Method::from_bytes(call.method.as_bytes()).map_err(internal)?,
            format!("http://127.0.0.1:{}{}", record.spec.target_port, call.path),
        )
        .body(body)
        .timeout(Duration::from_secs(record.spec.request_timeout_seconds));
    if let Some(t) = call.content_type {
        if t.len() > 128 {
            return Err(fault(ErrorCode::InvalidInput, "invalid content type"));
        }
        request = request.header("content-type", t);
    }
    let response = request.send().await.map_err(|error| {
        let reason = if error.is_timeout() {
            "service HTTP deadline exceeded"
        } else if error.is_connect() {
            "service HTTP connection failed"
        } else {
            "service HTTP transport failed"
        };
        ApiError(MeshError::new(
            ErrorCode::ServiceUnhealthy,
            format!("{reason}; mutation outcome may be unknown; no retry"),
        ))
    })?;
    let status = response.status().as_u16();
    let content_type = response
        .headers()
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .filter(|v| v.len() <= 128)
        .map(str::to_owned);
    let bytes = bounded_body(response).await?;
    // Recheck both revocation and lease before releasing any buffered data.
    let current = s.actor(&headers, &c)?;
    service(&s, id, current)?;
    Ok(Json(HttpReply {
        status,
        content_type,
        body_base64: STANDARD.encode(bytes),
    }))
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RestartRequest {
    request_id: Uuid,
}
async fn peer_restart(
    State(s): State<AppState>,
    Extension(c): Extension<ConnectionIdentity>,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
    Json(body): Json<RestartRequest>,
) -> ApiResult<Value> {
    let actor = s.actor(&headers, &c)?;
    let record = service(&s, id, actor)?;
    if !actor.owner
        && !record
            .spec
            .allowed_restart_peer_ids
            .contains(&actor.node_id)
    {
        return Err(fault(
            ErrorCode::PermissionDenied,
            "owner has not delegated restart of this service",
        ));
    }
    if record.spec.process.is_none() {
        return Err(fault(
            ErrorCode::InvalidInput,
            "external service cannot be restarted",
        ));
    }
    // Reservation is durable before restart. Ambiguous requests are never automatically retried.
    s.store.managed_create(
        "service_restart",
        body.request_id,
        &json!({"service_id":id,"actor":actor.node_id}),
    )?;
    Ok(Json(change::<ServiceSpec>(&s, "service", id, "restart")?))
}
struct Owned {
    child: Child,
    tree: ProcessTree,
}
impl Owned {
    async fn stop(&mut self) -> bool {
        let terminated = self.tree.terminate().is_ok();
        if !matches!(
            tokio::time::timeout(Duration::from_secs(3), self.child.wait()).await,
            Ok(Ok(_))
        ) {
            return false;
        }
        for _ in 0..40 {
            match self.tree.stopped() {
                Ok(true) => return terminated,
                Err(_) => return false,
                Ok(false) => tokio::time::sleep(Duration::from_millis(25)).await,
            }
        }
        false
    }
}
async fn spawn(mut command: Command) -> anyhow::Result<Owned> {
    command
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .kill_on_drop(true);
    #[cfg(windows)]
    command.creation_flags(0x08000000);
    ProcessTree::configure(&mut command);
    let mut child = command.spawn()?;
    match ProcessTree::attach(&child) {
        Ok(tree) => Ok(Owned { child, tree }),
        Err(e) => {
            let _ = child.kill().await;
            Err(e)
        }
    }
}
fn command_service(p: &ServiceProcess) -> Command {
    let mut c = Command::new(&p.executable);
    c.args(&p.args).current_dir(&p.cwd).env_clear();
    for key in ["SystemRoot", "WINDIR", "TEMP", "TMP", "PATH"] {
        if let Some(value) = std::env::var_os(key) {
            c.env(key, value);
        }
    }
    c.envs(&p.env);
    c
}
fn command_tunnel(spec: &TunnelSpec, empty: &FilePath) -> Command {
    let mut c = Command::new(&spec.ssh_executable);
    c.arg("-F").arg(empty).args([
        "-N",
        "-T",
        "-o",
        "BatchMode=yes",
        "-o",
        "IdentitiesOnly=yes",
        "-o",
        "StrictHostKeyChecking=yes",
        "-o",
        "ExitOnForwardFailure=yes",
        "-o",
        "ServerAliveInterval=2",
        "-o",
        "ServerAliveCountMax=2",
        "-o",
        "ConnectTimeout=5",
        "-o",
        "PermitLocalCommand=no",
        "-o",
        "GlobalKnownHostsFile=none",
    ]);
    c.arg("-o").arg(format!(
        "UserKnownHostsFile=\"{}\"",
        spec.known_hosts_file.to_string_lossy().replace('\\', "/")
    ));
    c.arg("-i")
        .arg(&spec.identity_file)
        .arg("-p")
        .arg(spec.port.to_string())
        .arg("-l")
        .arg(&spec.user);
    for f in &spec.forwards {
        c.arg(match f.direction {
            ForwardDirection::Local => "-L",
            ForwardDirection::Remote => "-R",
        })
        .arg(format!(
            "127.0.0.1:{}:127.0.0.1:{}",
            f.bind_port, f.target_port
        ));
    }
    c.arg(&spec.hostname);
    c
}
async fn process_worker(
    s: AppState,
    kind: &'static str,
    id: Uuid,
    revision: u64,
    mut shutdown: watch::Receiver<bool>,
) {
    let mut owned: Option<Owned> = None;
    let mut starts = 0;
    let mut next = Instant::now();
    let mut status = ResourceStatus {
        revision,
        ..Default::default()
    };
    let mut tick = tokio::time::interval(Duration::from_millis(250));
    loop {
        tokio::select! { _=shutdown.changed()=>break,_=tick.tick()=>{} }
        let configuration: std::result::Result<WorkerConfiguration, ApiError> = (|| {
            if kind == "service" {
                let r: Managed<ServiceSpec> = s.store.managed_get(kind, id)?;
                Ok((
                    r.active() && r.revision == revision,
                    r.spec.process.as_ref().map(command_service),
                    Some((r.spec.target_port, r.spec.health_path)),
                ))
            } else {
                let r: Managed<TunnelSpec> = s.store.managed_get(kind, id)?;
                Ok((
                    r.active() && r.revision == revision,
                    Some(command_tunnel(
                        &r.spec,
                        &s.instance.dir.join("managed-ssh-empty.conf"),
                    )),
                    None,
                ))
            }
        })();
        let Ok((active, command, health)) = configuration else {
            status.error = Some("resource configuration unreadable".into());
            break;
        };
        if !active || *shutdown.borrow() {
            break;
        }
        if let Some(p) = &mut owned {
            if p.tree.refresh().is_err() {
                status.error = Some("owned descendant tracking failed".into());
                break;
            }
            match p.child.try_wait() {
                Ok(None) => {}
                _ => {
                    if !p.stop().await {
                        status.state = "failed_needs_input".into();
                        status.error = Some(
                            "owned process cleanup unconfirmed; automatic restart disabled".into(),
                        );
                        break;
                    }
                    owned = None;
                    status.pid = None;
                    status.process = "backoff".into();
                    status.tunnel = if kind == "tunnel" {
                        "disconnected"
                    } else {
                        "not_applicable"
                    }
                    .into();
                    next = Instant::now() + Duration::from_secs(1 << starts.min(3));
                }
            }
        }
        let external = command.is_none();
        if owned.is_none()
            && Instant::now() >= next
            && let Some(command) = command
        {
            if starts >= 4 {
                status.state = "failed_needs_input".into();
                status.error = Some("restart budget exhausted (initial start + 3 retries)".into());
                s.managed.set(kind, id, status.clone());
                break;
            }
            starts += 1;
            status.starts = starts;
            match spawn(command).await {
                Ok(p) => {
                    status.pid = p.child.id();
                    owned = Some(p);
                    status.process = "running".into();
                    status.error = None;
                }
                Err(_) => {
                    status.process = "backoff".into();
                    status.error = Some("managed process startup failed".into());
                    next = Instant::now() + Duration::from_secs(1 << starts.min(3));
                }
            }
        }
        if kind == "tunnel" {
            status.tunnel = if owned.is_some() {
                "ssh_process_running"
            } else {
                "disconnected"
            }
            .into();
        }
        if let Some((port, path)) = health {
            if external {
                status.process = "external".into();
            }
            status.http_health = match http_client() {
                Ok(client) => match tokio::time::timeout(
                    Duration::from_millis(700),
                    client.get(format!("http://127.0.0.1:{port}{path}")).send(),
                )
                .await
                {
                    Ok(Ok(r)) if r.status().is_success() => "healthy",
                    _ => "unhealthy",
                },
                Err(_) => "unhealthy",
            }
            .into();
        }
        status.state = "active".into();
        s.managed.set(kind, id, status.clone());
    }
    let stopped = if let Some(mut p) = owned {
        p.stop().await
    } else {
        true
    };
    status.pid = None;
    status.process = if stopped { "stopped" } else { "unknown" }.into();
    if !stopped {
        status.state = "failed_needs_input".into();
        status.error = Some("owned process cleanup unconfirmed".into());
    }
    if status.state != "failed_needs_input" {
        status.state = "stopped".into();
    }
    if kind == "tunnel" {
        status.tunnel = "disconnected".into();
    }
    s.managed.set(kind, id, status);
}
#[derive(Clone)]
struct Proxy {
    s: AppState,
    spec: ConnectSpec,
    stop: watch::Receiver<bool>,
    limit: Arc<Semaphore>,
    request_timeout_seconds: u64,
}
async fn proxy_http(State(p): State<Proxy>, request: Request) -> Response {
    let Ok(_permit) = p.limit.clone().try_acquire_owned() else {
        return StatusCode::TOO_MANY_REQUESTS.into_response();
    };
    let mut stop = p.stop.clone();
    let forward = async {
        let r: Managed<ConnectSpec> = p.s.store.managed_get("connection", p.spec.id)?;
        if !r.active() {
            return Err(fault(
                ErrorCode::ServiceExpired,
                "local connection lease inactive",
            ));
        }
        let method = request.method().as_str().to_string();
        let path = request
            .uri()
            .path_and_query()
            .map(|v| v.as_str().to_string())
            .unwrap_or_else(|| "/".into());
        let content_type = request
            .headers()
            .get("content-type")
            .and_then(|v| v.to_str().ok())
            .map(str::to_owned);
        let bytes = to_bytes(request.into_body(), HTTP_LIMIT)
            .await
            .map_err(|_| fault(ErrorCode::RateLimited, "HTTP request exceeds 256 KiB"))?;
        let client = peer_client(&p.s, p.spec.peer_id)?;
        let reply: HttpReply = client
            .post_with_timeout(
                &format!("/hmesh/v1/services/{}/http", p.spec.service_id),
                &HttpCall {
                    method,
                    path,
                    content_type,
                    body_base64: STANDARD.encode(bytes),
                },
                p.request_timeout_seconds + 5,
            )
            .await?;
        let bytes = STANDARD
            .decode(reply.body_base64)
            .map_err(|_| fault(ErrorCode::InvalidInput, "invalid peer response"))?;
        if bytes.len() > HTTP_LIMIT {
            return Err(fault(
                ErrorCode::RateLimited,
                "peer HTTP response too large",
            ));
        }
        let r: Managed<ConnectSpec> = p.s.store.managed_get("connection", p.spec.id)?;
        if !r.active() || *p.stop.borrow() {
            return Err(fault(ErrorCode::ServiceExpired, "connection closed"));
        }
        let mut response = Response::builder()
            .status(reply.status)
            .header("cache-control", "no-store");
        if let Some(content_type) = reply.content_type {
            response = response.header("content-type", content_type);
        }
        response.body(Body::from(bytes)).map_err(internal)
    };
    tokio::select! {_=stop.changed()=>StatusCode::GONE.into_response(),result=forward=>match result {Ok(r)=>r,Err(e)=>e.into_response()}}
}
async fn connection_worker(
    s: AppState,
    id: Uuid,
    revision: u64,
    mut shutdown: watch::Receiver<bool>,
) {
    let result=async {
        if *shutdown.borrow() {return Ok::<_,ApiError>(());}
        let r:Managed<ConnectSpec>=s.store.managed_get("connection",id)?;
        let service:ServiceCard=peer_client(&s,r.spec.peer_id)?.get(&format!("/hmesh/v1/services/{}",r.spec.service_id)).await?;
        if !(1..=30).contains(&service.request_timeout_seconds) {return Err(fault(ErrorCode::UnsupportedVersion,"invalid service HTTP deadline"));}
        let listener=TcpListener::bind((std::net::Ipv4Addr::LOCALHOST,r.spec.local_port)).await.map_err(internal)?;
        let endpoint=format!("http://{}",listener.local_addr().map_err(internal)?);
        let (stop,rx)=watch::channel(false);
        let app=Router::new().fallback(proxy_http).with_state(Proxy{s:s.clone(),spec:r.spec.clone(),stop:rx.clone(),limit:Arc::new(Semaphore::new(8)),request_timeout_seconds:service.request_timeout_seconds});
        let mut server=tokio::spawn(async move {axum::serve(listener,app).with_graceful_shutdown(async move {let mut rx=rx;let _=rx.changed().await;}).await});
        s.managed.set("connection",id,ResourceStatus{revision,state:"active".into(),tunnel:"mTLS_HTTP".into(),endpoint:Some(endpoint),..Default::default()});
        let mut server_finished=false;
        loop {
            tokio::select! {_=shutdown.changed()=>break,_=&mut server=>{server_finished=true;break},_=tokio::time::sleep(Duration::from_millis(250))=>{}}
            let Ok(r)=s.store.managed_get::<Managed<ConnectSpec>>("connection",id) else {break;};
            if !r.active() || r.revision!=revision {break;}
            let check=async {peer_client(&s,r.spec.peer_id)?.get::<ServiceCard>(&format!("/hmesh/v1/services/{}",r.spec.service_id)).await.map_err(ApiError)};
            match tokio::time::timeout(Duration::from_secs(1),check).await {
                Ok(Err(ApiError(e))) if matches!(e.code,ErrorCode::PermissionDenied|ErrorCode::ServiceExpired|ErrorCode::Unauthenticated)=>break,
                Ok(Ok(_))=>{let mut status=s.managed.get("connection",id);status.tunnel="mTLS_HTTP".into();s.managed.set("connection",id,status);},
                _=>{let mut status=s.managed.get("connection",id);status.tunnel="unreachable".into();s.managed.set("connection",id,status);}
            }
        }
        let _=stop.send(true);
        if !server_finished && tokio::time::timeout(Duration::from_secs(1),&mut server).await.is_err() {server.abort();}
        Ok::<_,ApiError>(())
    }.await;
    s.managed.set(
        "connection",
        id,
        ResourceStatus {
            revision,
            state: "closed".into(),
            error: result.err().map(|_| {
                "connection unavailable; create a new connection after inspecting the cause".into()
            }),
            ..Default::default()
        },
    );
}
/// A new daemon never assumes ownership of a previous Unix process/PID or
/// resumes an interrupted repair. Configurations and evidence remain queryable.
pub fn recover(s: &AppState) -> hmesh_core::Result<()> {
    for kind in ["service", "tunnel", "connection"] {
        for mut r in s.store.managed_list::<Managed<Value>>(kind)? {
            let id = serde_json::from_value(r.spec["id"].clone())
                .map_err(|_| MeshError::input("corrupt managed id"))?;
            if r.enabled {
                r.enabled = false;
                s.store.managed_put(kind, id, &r)?;
            }
        }
    }
    for mut run in s.store.managed_list::<IntegrationRun>("integration")? {
        if !matches!(run.state.as_str(), "verified" | "needs_input" | "cancelled") {
            run.state = "needs_input".into();
            run.error = Some(
                "daemon restarted; previous side effects may be unknown; no automatic replay"
                    .into(),
            );
            s.store.managed_put("integration", run.spec.id, &run)?;
        }
    }
    Ok(())
}
pub async fn run(s: AppState, mut shutdown: watch::Receiver<bool>) {
    let mut seen = HashSet::new();
    let mut workers = JoinSet::new();
    let mut tick = tokio::time::interval(Duration::from_millis(100));
    if atomic_write(
        &s.instance.dir.join("managed-ssh-empty.conf"),
        b"# Managed exact forwards; user SSH configuration is not loaded.\n",
    )
    .is_err()
    {
        tracing::error!("cannot create managed SSH configuration");
        return;
    }
    loop {
        tokio::select! {_=shutdown.changed()=>break,_=tick.tick()=>{
            for kind in ["service","tunnel","connection"] {
                if let Ok(records)=s.store.managed_list::<Managed<Value>>(kind) {
                    for r in records {
                        if !r.active() {continue;}
                        let Ok(id)=serde_json::from_value::<Uuid>(r.spec["id"].clone()) else {continue;};
                        if !seen.insert((kind,id,r.revision)) {continue;}
                        let s=s.clone();let stop=shutdown.clone();
                        // Worker for previous revision notices its revision change before
                        // replacement starts. Port conflicts fail closed with bounded retry.
                        workers.spawn(async move {
                            if r.revision>1 {tokio::time::sleep(Duration::from_millis(1100)).await;}
                            if kind=="connection" {connection_worker(s,id,r.revision,stop).await}else{process_worker(s,kind,id,r.revision,stop).await}
                        });
                    }
                }
            }
        },Some(_)=workers.join_next(),if !workers.is_empty()=>{}}
    }
    while workers.join_next().await.is_some() {}
}

pub(crate) fn connection_endpoint(s: &AppState, id: Uuid) -> std::result::Result<String, ApiError> {
    let r: Managed<ConnectSpec> = s.store.managed_get("connection", id)?;
    if !r.active() {
        return Err(fault(
            ErrorCode::ServiceExpired,
            "connection lease inactive",
        ));
    }
    s.managed.get("connection", id).endpoint.ok_or_else(|| {
        fault(
            ErrorCode::ServiceUnhealthy,
            "connection endpoint is not active",
        )
    })
}
pub(crate) fn remote_client(s: &AppState, id: Uuid) -> std::result::Result<Client, ApiError> {
    peer_client(s, id)
}
