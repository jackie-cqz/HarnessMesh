//! Loopback browser bridge. Owner TLS credentials never leave this process.
use axum::{
    Json, Router,
    extract::{DefaultBodyLimit, Path, Query, Request, State, rejection::JsonRejection},
    http::{HeaderValue, Method, StatusCode, header},
    middleware::{self, Next},
    response::{Html, IntoResponse, Response},
    routing::{get, post},
};
use hmesh_core::{
    GitVersionRequirement, GitVersionState, MeshError, NodeConfig, RunLimits,
    TaskAcceptanceContract, TaskSubmit, now,
};
use hmesh_daemon::{OnboardingRequest, RemoteTask, VerificationRequest};
use hmesh_security::Instance;
use hmesh_transport::Client;
use serde::Deserialize;
use serde_json::{Value, json};
use std::{
    net::Ipv4Addr,
    path::{Path as FilePath, PathBuf},
    process::Stdio,
    sync::Arc,
    time::Duration,
};
use subtle::ConstantTimeEq;
use tokio::{net::TcpListener, sync::Semaphore};
use uuid::Uuid;

const CLI_SCHEMA_VERSION: i64 = hmesh_store::SCHEMA_VERSION;
const KNOWN_FEATURES: &[&str] = &[
    "panel-task-workflow-v1",
    "task-preflight-v1",
    "development-workflows-v1",
    "reliability-v2",
    "acp-sessions-v1",
    "project-collaboration-v1",
    "agent-settings-v1",
    "agent-service-test-v1",
    "agent-own-login-v1",
    "agent-service-relay-v1",
    "pairing-auto-complete-v1",
    "session-commands-v1",
    "agent-projects-v1",
];

#[derive(Clone)]
struct Panel {
    client: Client,
    host: String,
    origin: String,
    token: String,
    requests: Arc<Semaphore>,
    stop: tokio::sync::watch::Sender<bool>,
}

pub async fn run(
    dir: &FilePath,
    port: u16,
    start_daemon: bool,
    json_output: bool,
) -> anyhow::Result<()> {
    // Bind first: a port conflict must not start a Daemon or execute its queued work.
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, port)).await?;
    let instance = if start_daemon && !dir.join("identity.json").exists() {
        let mut config = NodeConfig::default();
        config.node.listen_port = 0;
        Instance::init(dir, config)?
    } else {
        Instance::load(dir)?
    };
    let existing = ready_client(&instance).await?;
    let mut owned = None;
    let client = if let Some(client) = existing {
        client
    } else if start_daemon {
        let mut child = tokio::process::Command::new(std::env::current_exe()?)
            .arg("--data-dir")
            .arg(&instance.dir)
            .args(["daemon", "start"])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .kill_on_drop(true)
            .spawn()?;
        let deadline = tokio::time::Instant::now() + Duration::from_secs(20);
        let client = loop {
            if let Some(status) = child.try_wait()? {
                anyhow::bail!(
                    "owned Daemon exited during startup ({status}); existing instance files were preserved"
                );
            }
            if let Some(c) = ready_client(&instance).await? {
                let status = c.get::<Value>("/local/status").await?;
                if status["pid"].as_u64() != child.id().map(u64::from) {
                    anyhow::bail!("another Daemon started concurrently; it was preserved");
                }
                break c;
            }
            if tokio::time::Instant::now() >= deadline {
                anyhow::bail!("owned Daemon readiness deadline exceeded");
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        };
        owned = Some(child);
        client
    } else {
        anyhow::bail!("Daemon is not reachable; start it or pass --start-daemon");
    };
    let owned_pid = owned.as_ref().and_then(|p| p.id());
    let host = format!("127.0.0.1:{}", listener.local_addr()?.port());
    let token = format!("{}{}", Uuid::new_v4().simple(), Uuid::new_v4().simple());
    let origin = format!("http://{host}");
    let url = format!("{origin}/#access={token}");
    let (stop, mut stopping) = tokio::sync::watch::channel(false);
    let state = Panel {
        client,
        host,
        origin,
        token,
        requests: Arc::new(Semaphore::new(32)),
        stop,
    };
    let app = Router::new()
        .route(
            "/assets/onboarding.js",
            get(|| async {
                (
                    [(header::CONTENT_TYPE, "text/javascript; charset=utf-8")],
                    include_str!("../../../web/panel/onboarding.js"),
                )
            }),
        )
        .route("/api/onboarding/preview", post(preview_onboarding))
        .route("/api/onboarding/register", post(register_onboarding))
        .route("/api/tasks/{id}/workspace", get(workspace_diff))
        .route(
            "/api/development/compatibility/probe",
            post(development_probe),
        )
        .route(
            "/api/development/{kind}",
            get(development_read).post(development_write),
        )
        .route(
            "/api/development/templates/run",
            post(development_template_run),
        )
        .route(
            "/api/development/workflows/{id}/{action}",
            post(development_workflow_action),
        )
        .route(
            "/assets/development.js",
            get(|| async {
                (
                    [(header::CONTENT_TYPE, "text/javascript; charset=utf-8")],
                    include_str!("../../../web/panel/development.js"),
                )
            }),
        )
        .route(
            "/",
            get(|| async { Html(include_str!("../../../web/panel/index.html")) }),
        )
        .route(
            "/assets/panel.css",
            get(|| async {
                (
                    [(header::CONTENT_TYPE, "text/css; charset=utf-8")],
                    include_str!("../../../web/panel/panel.css"),
                )
            }),
        )
        .route(
            "/assets/panel.js",
            get(|| async {
                (
                    [(header::CONTENT_TYPE, "text/javascript; charset=utf-8")],
                    include_str!("../../../web/panel/panel.js"),
                )
            }),
        )
        .route(
            "/assets/workflow.js",
            get(|| async {
                (
                    [(header::CONTENT_TYPE, "text/javascript; charset=utf-8")],
                    include_str!("../../../web/panel/workflow.js"),
                )
            }),
        )
        .route("/api/runtime", get(runtime))
        .route("/api/overview", get(overview))
        .route("/api/panel/stop", post(stop_panel))
        .route("/api/tasks", post(create_task))
        .route("/api/tasks/preflight", post(preflight_task))
        .route("/api/tasks/{id}", get(task))
        .route("/api/tasks/{id}/observation", get(task_observation))
        .route("/api/tasks/{id}/result", get(task_reply))
        .route("/api/discovery", get(project_discovery))
        .route("/api/agents/{id}/git", get(agent_git))
        .route("/api/tasks/{id}/preflight", get(existing_task_preflight))
        .route("/api/tasks/{id}/verify", post(verify_artifact))
        .route("/api/requests/{id}/recover", post(recover))
        .route("/api/tasks/{id}/events", get(events))
        .route("/api/tasks/{id}/{action}", post(task_action))
        .route("/api/agents/{id}/{action}", post(agent_action))
        .route("/api/services/{id}/{action}", post(service_action))
        .route("/api/peers/{id}/agents", get(remote_agents))
        .route("/api/pairs/request", post(request_pair))
        .route("/api/pairing/window", post(open_pairing_window))
        .route("/api/pairs/{id}/{action}", post(pair_action))
        .route(
            "/assets/sessions.js",
            get(|| async {
                (
                    [(header::CONTENT_TYPE, "text/javascript; charset=utf-8")],
                    include_str!("../../../web/panel/sessions.js"),
                )
            }),
        )
        .route(
            "/assets/session-commands.js",
            get(|| async {
                (
                    [(header::CONTENT_TYPE, "text/javascript; charset=utf-8")],
                    include_str!("../../../web/panel/session-commands.js"),
                )
            }),
        )
        .route(
            "/assets/projects.js",
            get(|| async {
                (
                    [(header::CONTENT_TYPE, "text/javascript; charset=utf-8")],
                    include_str!("../../../web/panel/projects.js"),
                )
            }),
        )
        .route(
            "/assets/project-work.js",
            get(|| async {
                (
                    [(header::CONTENT_TYPE, "text/javascript; charset=utf-8")],
                    include_str!("../../../web/panel/project-work.js"),
                )
            }),
        )
        .route(
            "/assets/inbox.js",
            get(|| async {
                (
                    [(header::CONTENT_TYPE, "text/javascript; charset=utf-8")],
                    include_str!("../../../web/panel/inbox.js"),
                )
            }),
        )
        .route(
            "/assets/pairing.js",
            get(|| async {
                (
                    [(header::CONTENT_TYPE, "text/javascript; charset=utf-8")],
                    include_str!("../../../web/panel/pairing.js"),
                )
            }),
        )
        .route(
            "/assets/inbox.css",
            get(|| async {
                (
                    [(header::CONTENT_TYPE, "text/css; charset=utf-8")],
                    include_str!("../../../web/panel/inbox.css"),
                )
            }),
        )
        .route("/api/agents/detect", get(detect_agents))
        .route(
            "/api/agents/{id}/service",
            get(agent_service).post(set_agent_service),
        )
        .route("/api/agents/{id}/service/test", post(test_agent_service))
        .route("/api/agents/{id}/service/check", post(check_agent_service))
        .route("/api/projects", get(projects).post(add_project))
        .route("/api/projects/{id}", post(update_project))
        .route("/api/projects/{id}/remove", post(remove_project))
        .route("/api/projects/{id}/history", get(project_history))
        .route("/api/history", get(history_folders))
        .route("/api/sessions/{id}/mark", post(mark_session))
        .route("/api/sessions", get(sessions).post(open_session))
        .route("/api/sessions/summary", get(session_summary))
        .route("/api/sessions/{id}/events", get(session_events))
        .route("/api/sessions/{id}/prompt", post(session_prompt))
        .route("/api/sessions/{id}/config", post(session_config))
        .route(
            "/api/sessions/{id}/commands",
            get(session_commands).post(prepare_session_commands),
        )
        .route("/api/sessions/{id}/command", post(run_session_command))
        .route(
            "/api/sessions/{id}/permissions/{key}",
            post(session_permission),
        )
        .route("/api/sessions/{id}/{action}", post(session_action))
        .fallback(|| async { error(StatusCode::NOT_FOUND, "NOT_FOUND") })
        .layer(DefaultBodyLimit::max(524288))
        .layer(middleware::from_fn_with_state(state.clone(), guard))
        .with_state(state);
    if json_output {
        println!(
            "{}",
            json!({"url":url,"pid":std::process::id(),"owned_daemon_pid":owned_pid,"scope":"loopback-owner-panel"})
        );
    } else {
        println!(
            "HarnessMesh 本机管理面板\n{url}\n访问链接仅供本机使用。Ctrl+C 正常关闭面板；仅本次启动的 Daemon 会一并停止。"
        );
    }
    let served = axum::serve(listener, app)
        .with_graceful_shutdown(async move {
            tokio::select! {_ = shutdown_signal()=>{},_ = stopping.changed()=>{}}
        })
        .await;
    if let Some(mut child) = owned {
        if matches!(child.try_wait(), Ok(None))
            && let Ok(owner) = hmesh_daemon::local_client(&instance)
            && let Ok(status) = owner.get::<Value>("/local/status").await
            && status["pid"].as_u64() == child.id().map(u64::from)
        {
            let _ = owner.post::<_, Value>("/local/stop", &json!({})).await;
        }
        if tokio::time::timeout(Duration::from_secs(10), child.wait())
            .await
            .is_err()
        {
            let _ = child.kill().await;
            let _ = child.wait().await;
        }
    }
    served?;
    Ok(())
}
async fn shutdown_signal() {
    #[cfg(unix)]
    {
        if let Ok(mut term) =
            tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
        {
            tokio::select! {_=ctrl_c_signal()=>{},_=term.recv()=>{}};
            return;
        }
    }
    ctrl_c_signal().await;
}
async fn ctrl_c_signal() {
    // A missing console/signal handler is not a shutdown request; the authenticated UI stop remains usable.
    if tokio::signal::ctrl_c().await.is_err() {
        std::future::pending::<()>().await;
    }
}
async fn stop_panel(State(s): State<Panel>, Json(body): Json<Value>) -> Response {
    if body != json!({}) {
        return error(StatusCode::BAD_REQUEST, "UNSUPPORTED_ACTION");
    }
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(100)).await;
        let _ = s.stop.send(true);
    });
    Json(json!({"data":{"stopping":true}})).into_response()
}
async fn ready_client(instance: &Instance) -> anyhow::Result<Option<Client>> {
    let Ok(client) = hmesh_daemon::local_client(instance) else {
        return Ok(None);
    };
    match tokio::time::timeout(Duration::from_secs(2), client.get::<Value>("/local/status")).await {
        // A reachable older Daemon is preserved and exposed in compatibility mode.
        Ok(Ok(_)) => Ok(Some(client)),
        _ => Ok(None),
    }
}

fn runtime_view(status: Option<&Value>) -> Value {
    let running = status.is_some_and(|s| s["running"] == true);
    let features: Vec<&str> = status
        .and_then(|s| s["api_features"].as_array())
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .filter(|feature| KNOWN_FEATURES.contains(feature))
        .collect();
    let task_workflow = running && features.contains(&"panel-task-workflow-v1");
    let task_preflight = running && features.contains(&"task-preflight-v1");
    let development_workflows = running && features.contains(&"development-workflows-v1");
    let sessions = running && features.contains(&"acp-sessions-v1");
    let project_collaboration = running && features.contains(&"project-collaboration-v1");
    let mode = if !task_workflow || !task_preflight {
        "read_only"
    } else if development_workflows {
        "full"
    } else {
        "compatible"
    };
    let schema = status.and_then(|s| s["schema_version"].as_i64());
    // Losing contact with the owner API does not establish that an upgrade is needed.
    let upgrade_required =
        running && (mode != "full" || schema.is_some_and(|version| version < CLI_SCHEMA_VERSION));
    let steps = if upgrade_required {
        vec![
            "先确认运行中的 Daemon、节点目录和未结束任务；当前面板不会停止或升级它。",
            "在该节点本机使用旧版本 CLI 正常停止 Daemon，再创建并检查私有备份。",
            "使用新运行包执行 upgrade check 和 upgrade prepare，核对平台、文件校验和与数据库版本。",
            "显式从新版本启动同一节点，重新检查任务和工件；通过 startup status 核对登录自启引用，必要时重新安装。",
        ]
    } else {
        Vec::new()
    };
    json!({
        "cli_version":env!("CARGO_PKG_VERSION"),"cli_schema_version":CLI_SCHEMA_VERSION,
        "daemon":{
            "running":running,
            "version":status.and_then(|s|s["daemon_version"].as_str()).filter(|v|v.len()<=64 && v.starts_with(|c:char|c.is_ascii_digit()) && v.bytes().filter(|c|*c==b'.').count()>=2 && v.bytes().all(|c|c.is_ascii_alphanumeric() || matches!(c,b'.'|b'-'|b'+'))),
            "pid":status.and_then(|s|s["pid"].as_u64()),
            "schema_version":schema,
            "network":status.and_then(|s|s["network"].as_str()).filter(|v|matches!(*v,"loopback"|"tailscale")),
            "api_features":features
        },
        "capabilities":{"task_workflow":task_workflow,"task_preflight":task_preflight,"development_workflows":development_workflows,"sessions":sessions,"project_collaboration":project_collaboration,"agent_settings":running && features.contains(&"agent-settings-v1"),"agent_service_test":running && features.contains(&"agent-service-test-v1"),"session_commands":running && features.contains(&"session-commands-v1"),"agent_own_login":running && features.contains(&"agent-own-login-v1"),"agent_service_relay":running && features.contains(&"agent-service-relay-v1"),"pairing":running && features.contains(&"pairing-auto-complete-v1"),"projects":running && features.contains(&"agent-projects-v1")},
        "mode":mode,"upgrade":{"required":upgrade_required,"steps":steps,"automatic":false}
    })
}
async fn current_runtime(s: &Panel) -> Value {
    let status = tokio::time::timeout(
        Duration::from_secs(2),
        s.client.get::<Value>("/local/status"),
    )
    .await
    .ok()
    .and_then(Result::ok);
    runtime_view(status.as_ref())
}
async fn runtime(State(s): State<Panel>) -> Json<Value> {
    Json(json!({"data":current_runtime(&s).await}))
}

fn unavailable(capability: &str) -> Response {
    (
        StatusCode::CONFLICT,
        Json(json!({"error":{"code":"FEATURE_UNAVAILABLE","capability":capability,"automatic_retry":false,"submission_attempted":false}})),
    )
        .into_response()
}

async fn guarded_api(s: &Panel, req: Request, next: Next) -> Response {
    let path = req.uri().path();
    let write = !matches!(*req.method(), Method::GET | Method::HEAD | Method::OPTIONS);
    let development = path.starts_with("/api/development/");
    let session_commands = path.starts_with("/api/sessions/")
        && matches!(path.rsplit('/').next(), Some("command" | "commands"));
    let preflight = path == "/api/tasks/preflight"
        || (path.starts_with("/api/tasks/") && path.ends_with("/preflight"));
    if path != "/api/panel/stop" && (write || development || preflight || session_commands) {
        // Read capabilities for each protected request, not just panel startup:
        // an owner can replace/restart a Daemon while this panel remains open.
        let view = current_runtime(s).await;
        if write && view["mode"] == "read_only" {
            return (
                StatusCode::CONFLICT,
                Json(json!({"error":{"code":"PANEL_READ_ONLY","automatic_retry":false,"submission_attempted":false},"runtime":view})),
            )
                .into_response();
        }
        if development && view["capabilities"]["development_workflows"] != true {
            return unavailable("development_workflows");
        }
        if preflight && view["capabilities"]["task_preflight"] != true {
            return unavailable("task_preflight");
        }
        if session_commands && view["capabilities"]["session_commands"] != true {
            return unavailable("session_commands");
        }
    }
    next.run(req).await
}

fn error(status: StatusCode, code: &str) -> Response {
    (status, Json(json!({"error":{"code":code}}))).into_response()
}
async fn guard(State(s): State<Panel>, req: Request, next: Next) -> Response {
    let permit = s.requests.clone().try_acquire_owned();
    let mut response = if permit.is_err() {
        error(StatusCode::TOO_MANY_REQUESTS, "PANEL_BUSY")
    } else if req
        .headers()
        .get(header::HOST)
        .and_then(|h| h.to_str().ok())
        != Some(s.host.as_str())
    {
        error(StatusCode::FORBIDDEN, "INVALID_HOST")
    } else if req
        .headers()
        .get(header::ORIGIN)
        .is_some_and(|h| h.as_bytes() != s.origin.as_bytes())
        || req
            .headers()
            .get("sec-fetch-site")
            .is_some_and(|h| h == "cross-site")
    {
        error(StatusCode::FORBIDDEN, "CROSS_ORIGIN_DENIED")
    } else if req.uri().path().starts_with("/api/") {
        let token = req
            .headers()
            .get(header::AUTHORIZATION)
            .and_then(|h| h.to_str().ok())
            .and_then(|h| h.strip_prefix("Bearer "))
            .unwrap_or("");
        if !bool::from(token.as_bytes().ct_eq(s.token.as_bytes())) {
            error(StatusCode::UNAUTHORIZED, "PANEL_ACCESS_REQUIRED")
        } else if req.method() == Method::POST
            && req
                .headers()
                .get(header::ORIGIN)
                .and_then(|h| h.to_str().ok())
                != Some(s.origin.as_str())
        {
            error(StatusCode::FORBIDDEN, "ORIGIN_REQUIRED")
        } else {
            guarded_api(&s, req, next).await
        }
    } else {
        next.run(req).await
    };
    if response.status().is_client_error()
        && !response
            .headers()
            .get(header::CONTENT_TYPE)
            .is_some_and(|h| h.to_str().is_ok_and(|s| s.starts_with("application/json")))
    {
        response = error(response.status(), "INVALID_REQUEST");
    }
    let headers = response.headers_mut();
    for (name, value) in [
        ("cache-control", "no-store"),
        ("referrer-policy", "no-referrer"),
        ("x-content-type-options", "nosniff"),
        ("x-frame-options", "DENY"),
        (
            "content-security-policy",
            "default-src 'none'; script-src 'self'; style-src 'self'; connect-src 'self'; img-src 'self' data:; base-uri 'none'; frame-ancestors 'none'; form-action 'none'",
        ),
    ] {
        headers.insert(name, HeaderValue::from_static(value));
    }
    response
}

fn result(value: Result<Value, MeshError>) -> Value {
    match value {
        Ok(value) => json!({"data":value}),
        Err(e) => {
            json!({"error":{"code":e.code,"retryable":e.retryable,"trace_id":e.trace_id,"automatic_retry":false}})
        }
    }
}
// Managed process configurations may contain secrets in env/argv. Never send them to the UI.
fn resources(value: Result<Value, MeshError>) -> Value {
    result(value.map(|rows| Value::Array(rows.as_array().into_iter().flatten().map(|r| {
        json!({"id":r["id"],"name":r["name"],"project_id":r["project_id"],
            "managed":r["process"].is_object() || matches!(r["status"]["process"].as_str(),Some("running"|"backoff")),
            "enabled":r["enabled"],"expires_at":r["expires_at"],"revision":r["revision"],"status":{
                "state":r["status"]["state"],"process":r["status"]["process"],"http_health":r["status"]["http_health"],
                "pid":r["status"]["pid"],"starts":r["status"]["starts"],"endpoint":r["status"]["endpoint"]}})
    }).collect())))
}
async fn overview(State(s): State<Panel>) -> Json<Value> {
    let (node, peers, pairs, agents, retired, tasks, services, connections, tunnels, queue) = tokio::join!(
        s.client.get::<Value>("/local/status"),
        s.client.get::<Value>("/local/peers"),
        s.client.get::<Value>("/local/pairs"),
        s.client.get::<Value>("/hmesh/v1/agents"),
        s.client.get::<Value>("/local/agents/retired"),
        s.client.get::<Value>("/local/tasks"),
        s.client.get::<Value>("/local/services"),
        s.client.get::<Value>("/local/connections"),
        s.client.get::<Value>("/local/tunnels"),
        s.client.get::<Value>("/local/queue"),
    );
    Json(
        json!({"observed_at":now(),"node":result(node),"peers":result(peers),"pairs":result(pairs.map(recent_pairs)),"agents":result(agents),
        "retired":result(retired),"tasks":result(tasks),"services":resources(services),"connections":resources(connections),"tunnels":resources(tunnels),"queue":result(queue)}),
    )
}
async fn task(State(s): State<Panel>, Path(id): Path<Uuid>) -> Json<Value> {
    let path = format!("/local/tasks/{id}");
    let verification_path = format!("/local/tasks/{id}/verification");
    let artifacts_path = format!("/local/tasks/{id}/artifacts");
    let workspace_path = format!("/local/tasks/{id}/workspace");
    let (task, verification, artifacts, workspace) = tokio::join!(
        s.client.get::<Value>(&path),
        s.client.get::<Value>(&verification_path),
        s.client.get::<Value>(&artifacts_path),
        s.client.get::<Value>(&workspace_path)
    );
    Json(
        json!({"observed_at":now(),"task":result(task),"verification":result(verification),"artifacts":result(artifacts),"workspace":result(workspace)}),
    )
}

async fn task_observation(State(s): State<Panel>, Path(id): Path<Uuid>) -> Response {
    let path = format!("/local/tasks/{id}");
    let task = match s.client.get::<Value>(&path).await {
        Ok(task) => task,
        Err(e) => return mutation(Err(e)),
    };
    let observed_at = now();
    let verification = if task["status"] == "SUCCEEDED" {
        Some(result(
            s.client.get::<Value>(&format!("{path}/verification")).await,
        ))
    } else {
        None
    };
    mutation(Ok(
        json!({"task":task,"observed_at":observed_at,"verification":verification}),
    ))
}

async fn task_reply(State(s): State<Panel>, Path(id): Path<Uuid>) -> Response {
    mutation(s.client.get(&format!("/local/tasks/{id}/result")).await)
}

#[derive(Deserialize)]
struct DiscoveryQuery {
    peer_id: Option<Uuid>,
}

async fn project_discovery(
    State(s): State<Panel>,
    Query(query): Query<DiscoveryQuery>,
) -> Response {
    let path = match query.peer_id {
        Some(id) => format!("/local/discovery?peer_id={id}"),
        None => "/local/discovery".to_owned(),
    };
    mutation(s.client.get(&path).await)
}

async fn agent_git(State(s): State<Panel>, Path(id): Path<Uuid>) -> Response {
    mutation(s.client.get(&format!("/local/agents/{id}/git")).await)
}
async fn preview_onboarding(
    State(s): State<Panel>,
    Json(body): Json<OnboardingRequest>,
) -> Response {
    mutation(s.client.post("/local/onboarding/preview", &body).await)
}
async fn register_onboarding(
    State(s): State<Panel>,
    Json(body): Json<OnboardingRequest>,
) -> Response {
    mutation(s.client.post("/local/onboarding/register", &body).await)
}
async fn workspace_diff(
    State(s): State<Panel>,
    Path(id): Path<Uuid>,
    Query(cursor): Query<Cursor>,
) -> Response {
    if cursor.after < 0 || cursor.after > 4 * 1024 * 1024 {
        return error(StatusCode::BAD_REQUEST, "INVALID_CURSOR");
    }
    mutation(
        s.client
            .get(&format!(
                "/local/tasks/{id}/workspace?diff=true&offset={}",
                cursor.after
            ))
            .await,
    )
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct NewTask {
    request_id: Uuid,
    peer_id: Uuid,
    agent_id: Uuid,
    project_id: String,
    goal: String,
    timeout_seconds: u64,
    max_output_bytes: usize,
    #[serde(default)]
    expected_artifacts: Option<Vec<PathBuf>>,
    #[serde(default)]
    source_session_id: Option<Uuid>,
    #[serde(default)]
    acceptance_contract: Option<TaskAcceptanceContract>,
    #[serde(default)]
    git_version: Option<GitVersionRequirement>,
    #[serde(default)]
    origin_git: Option<GitVersionState>,
}
fn remote_task(body: NewTask) -> Result<RemoteTask, MeshError> {
    let mut task = TaskSubmit::new(body.agent_id, &body.project_id, &body.goal);
    task.task_id = body.request_id;
    task.limits = RunLimits {
        timeout_seconds: body.timeout_seconds,
        max_output_bytes: body.max_output_bytes,
    };
    // An empty composer selection means no artifact contract. Explicit empty
    // contracts remain invalid at the core protocol boundary.
    task.expected_artifacts = body.expected_artifacts.filter(|paths| !paths.is_empty());
    task.source_session_id = body.source_session_id;
    task.acceptance_contract = body.acceptance_contract;
    task.git_version = body.git_version;
    task.origin_git = body.origin_git;
    task.validate()?;
    Ok(RemoteTask {
        peer_id: body.peer_id,
        request_id: body.request_id,
        task,
    })
}
fn preflight_view(value: Value) -> Value {
    let checks: Vec<Value> = value["checks"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|check| {
            let mut safe = json!({"code":check["code"].as_str(),"passed":check["passed"] == true});
            if let Some(field) = check["field"].as_str() {
                safe["field"] = json!(field);
            }
            safe
        })
        .collect();
    json!({
        "ready":value["ready"] == true,"agent_id":value["agent_id"].as_str(),
        "project_id":value["project_id"].as_str(),"requires_approval":value["requires_approval"].as_bool(),
        "configuration_sha256":value["configuration_sha256"].as_str(),"checks":checks,
        "observed_at":value["observed_at"].as_i64(),
        "git_version":value.get("git_version")
    })
}
fn not_submitted(status: StatusCode, code: &str) -> Response {
    (
        status,
        Json(json!({"error":{"code":code,"automatic_retry":false,"submission_attempted":false}})),
    )
        .into_response()
}
#[derive(Deserialize)]
struct PreflightOptions {
    #[serde(default)]
    observe_git: bool,
}
async fn preflight_task(
    State(s): State<Panel>,
    Query(query): Query<PreflightOptions>,
    body: Result<Json<NewTask>, JsonRejection>,
) -> Response {
    let Ok(Json(body)) = body else {
        return not_submitted(StatusCode::BAD_REQUEST, "INVALID_TASK_REQUEST");
    };
    let Ok(request) = remote_task(body) else {
        return not_submitted(StatusCode::BAD_REQUEST, "INVALID_TASK_REQUEST");
    };
    let path = if query.observe_git {
        "/local/tasks/preflight?observe_git=true"
    } else {
        "/local/tasks/preflight"
    };
    mutation(
        s.client
            .post::<_, Value>(path, &request)
            .await
            .map(preflight_view),
    )
}
async fn existing_task_preflight(State(s): State<Panel>, Path(id): Path<Uuid>) -> Response {
    mutation(
        s.client
            .get::<Value>(&format!("/local/tasks/{id}/preflight"))
            .await
            .map(preflight_view),
    )
}
fn failed_preflight(preflight: Value) -> Response {
    (
        StatusCode::CONFLICT,
        Json(json!({"error":{"code":"TASK_PREFLIGHT_FAILED","automatic_retry":false,"submission_attempted":false},"preflight":preflight})),
    )
        .into_response()
}
fn preflight_error(e: MeshError) -> Response {
    (
        StatusCode::BAD_GATEWAY,
        Json(json!({"error":{"code":e.code,"retryable":e.retryable,"trace_id":e.trace_id,"automatic_retry":false,"submission_attempted":false}})),
    )
        .into_response()
}
async fn create_task(
    State(s): State<Panel>,
    body: Result<Json<NewTask>, JsonRejection>,
) -> Response {
    let Ok(Json(body)) = body else {
        return not_submitted(StatusCode::BAD_REQUEST, "INVALID_TASK_REQUEST");
    };
    let Ok(request) = remote_task(body) else {
        return not_submitted(StatusCode::BAD_REQUEST, "INVALID_TASK_REQUEST");
    };
    let preflight = match s
        .client
        .post::<_, Value>("/local/tasks/preflight", &request)
        .await
    {
        Ok(value) => preflight_view(value),
        Err(e) => return preflight_error(e),
    };
    if preflight["ready"] != true
        || preflight["agent_id"].as_str() != Some(&request.task.target_agent_id.to_string())
        || preflight["project_id"].as_str() != Some(request.task.project_id.as_str())
    {
        return failed_preflight(preflight);
    }
    mutation(s.client.post("/local/tasks/submit", &request).await)
}
async fn verify_artifact(
    State(s): State<Panel>,
    Path(id): Path<Uuid>,
    Json(body): Json<VerificationRequest>,
) -> Response {
    mutation(
        s.client
            .post(&format!("/local/tasks/{id}/verify"), &body)
            .await,
    )
}
async fn recover(
    State(s): State<Panel>,
    Path(id): Path<Uuid>,
    Json(body): Json<Value>,
) -> Response {
    if body != json!({}) {
        return error(StatusCode::BAD_REQUEST, "UNSUPPORTED_ACTION");
    }
    mutation(
        s.client
            .post(&format!("/local/requests/{id}/recover"), &body)
            .await,
    )
}
fn mutation(response: Result<Value, MeshError>) -> Response {
    match response {Ok(v)=>Json(json!({"data":v})).into_response(),Err(e)=>(StatusCode::BAD_GATEWAY,Json(json!({"error":{"code":e.code,"retryable":e.retryable,"trace_id":e.trace_id,"automatic_retry":false}}))).into_response()}
}
#[derive(Deserialize)]
struct Cursor {
    #[serde(default)]
    after: i64,
}
async fn events(
    State(s): State<Panel>,
    Path(id): Path<Uuid>,
    Query(cursor): Query<Cursor>,
) -> Response {
    if cursor.after < 0 {
        return error(StatusCode::BAD_REQUEST, "INVALID_CURSOR");
    }
    Json(result(
        s.client
            .get::<Value>(&format!("/local/tasks/{id}/events?after={}", cursor.after))
            .await,
    ))
    .into_response()
}
/// ACP agent commands on this computer's PATH; nothing is run or installed.
async fn detect_agents() -> Json<Value> {
    Json(json!({"data":hmesh_core::detect_installed_agents()}))
}
/// Owner sessions plus every node's agents, for the session picker.
async fn sessions(State(s): State<Panel>) -> Json<Value> {
    let (sessions, nodes) = tokio::join!(
        s.client.get::<Value>("/local/sessions"),
        s.client.get::<Value>("/local/mesh/agents"),
    );
    Json(json!({"observed_at":now(),"sessions":result(sessions),"nodes":result(nodes)}))
}
async fn session_summary(State(s): State<Panel>) -> Json<Value> {
    let sessions = s.client.get::<Value>("/local/sessions").await;
    Json(json!({"observed_at":now(),"sessions":result(sessions)}))
}
/// Session errors carry the daemon's explanation; it is shown as text.
fn session_reply(value: Result<Value, MeshError>) -> Response {
    match value {
        Ok(value) => Json(json!({"data":value})).into_response(),
        Err(e) => (
            StatusCode::BAD_GATEWAY,
            Json(json!({"error":{"code":e.code,"message":e.message,"automatic_retry":false}})),
        )
            .into_response(),
    }
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct OpenSessionForm {
    agent_id: Uuid,
    #[serde(default)]
    peer_id: Option<Uuid>,
    #[serde(default)]
    message: Option<String>,
    #[serde(default)]
    config: std::collections::BTreeMap<String, Value>,
    #[serde(default)]
    project_id: Option<String>,
    #[serde(default)]
    resume: Option<String>,
    #[serde(default)]
    title: Option<String>,
}
async fn open_session(State(s): State<Panel>, Json(body): Json<OpenSessionForm>) -> Response {
    let mut request = json!({"agent_id":body.agent_id,"peer_id":body.peer_id,"message":body.message,"config":body.config});
    for (key, value) in [
        ("project_id", body.project_id),
        ("resume", body.resume),
        ("title", body.title),
    ] {
        if let Some(value) = value {
            request[key] = json!(value);
        }
    }
    session_reply(s.client.post::<_, Value>("/local/sessions", &request).await)
}
/// A project ID from the address bar, checked before it reaches a daemon path.
fn project_path(id: &str, rest: &str) -> Option<String> {
    hmesh_core::projects::validate_project_id(id)
        .ok()
        .map(|_| format!("/local/projects/{id}{rest}"))
}
fn invalid_project() -> Response {
    error(StatusCode::BAD_REQUEST, "INVALID_PROJECT")
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct HistoryForm {
    #[serde(default)]
    refresh: bool,
}
async fn projects(State(s): State<Panel>) -> Response {
    session_reply(s.client.get::<Value>("/local/projects").await)
}
async fn add_project(
    State(s): State<Panel>,
    Json(body): Json<hmesh_core::projects::AddProject>,
) -> Response {
    session_reply(s.client.post::<_, Value>("/local/projects", &body).await)
}
async fn update_project(
    State(s): State<Panel>,
    Path(id): Path<String>,
    Json(body): Json<hmesh_core::projects::UpdateProject>,
) -> Response {
    match project_path(&id, "") {
        Some(path) => session_reply(s.client.post::<_, Value>(&path, &body).await),
        None => invalid_project(),
    }
}
async fn remove_project(State(s): State<Panel>, Path(id): Path<String>) -> Response {
    match project_path(&id, "/remove") {
        Some(path) => session_reply(s.client.post::<_, Value>(&path, &json!({})).await),
        None => invalid_project(),
    }
}
async fn project_history(
    State(s): State<Panel>,
    Path(id): Path<String>,
    Query(form): Query<HistoryForm>,
) -> Response {
    match project_path(&id, &format!("/history?refresh={}", form.refresh)) {
        Some(path) => session_reply(s.client.get::<Value>(&path).await),
        None => invalid_project(),
    }
}
async fn history_folders(State(s): State<Panel>, Query(form): Query<HistoryForm>) -> Response {
    session_reply(
        s.client
            .get::<Value>(&format!("/local/history?refresh={}", form.refresh))
            .await,
    )
}
async fn mark_session(
    State(s): State<Panel>,
    Path(id): Path<Uuid>,
    Json(body): Json<hmesh_core::projects::UpdateSessionMark>,
) -> Response {
    session_reply(
        s.client
            .post::<_, Value>(&format!("/local/sessions/{id}/mark"), &body)
            .await,
    )
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SessionConfigForm {
    config_id: String,
    value: Value,
}
async fn session_config(
    State(s): State<Panel>,
    Path(id): Path<Uuid>,
    Json(body): Json<SessionConfigForm>,
) -> Response {
    session_reply(
        s.client
            .post::<_, Value>(
                &format!("/local/sessions/{id}/config"),
                &json!({"config_id":body.config_id,"value":body.value}),
            )
            .await,
    )
}
async fn agent_service(State(s): State<Panel>, Path(id): Path<Uuid>) -> Response {
    session_reply(
        s.client
            .get::<Value>(&format!("/local/agents/{id}/service?protocols=true"))
            .await,
    )
}
async fn session_commands(State(s): State<Panel>, Path(id): Path<Uuid>) -> Response {
    session_reply(
        s.client
            .get(&format!("/local/sessions/{id}/commands"))
            .await,
    )
}
async fn prepare_session_commands(
    State(s): State<Panel>,
    Path(id): Path<Uuid>,
    Json(body): Json<Value>,
) -> Response {
    if body != json!({}) {
        return error(StatusCode::BAD_REQUEST, "INVALID_COMMAND_DISCOVERY");
    }
    session_reply(
        s.client
            .post_with_timeout(&format!("/local/sessions/{id}/commands"), &body, 40)
            .await,
    )
}
async fn run_session_command(
    State(s): State<Panel>,
    Path(id): Path<Uuid>,
    Json(body): Json<hmesh_core::sessions::RunSessionCommand>,
) -> Response {
    session_reply(
        s.client
            .post(&format!("/local/sessions/{id}/command"), &body)
            .await,
    )
}
/// The browser sends a key once; the panel passes it to the local daemon and
/// never returns it.
async fn set_agent_service(
    State(s): State<Panel>,
    Path(id): Path<Uuid>,
    Json(body): Json<hmesh_core::service::SetAgentService>,
) -> Response {
    session_reply(
        s.client
            .post::<_, Value>(&format!("/local/agents/{id}/service?protocols=true"), &body)
            .await,
    )
}
/// Find out what the agent is set up with on its own.
async fn check_agent_service(State(s): State<Panel>, Path(id): Path<Uuid>) -> Response {
    session_reply(
        s.client
            .post::<_, Value>(
                &format!("/local/agents/{id}/service/check?protocols=true"),
                &json!({}),
            )
            .await,
    )
}
/// Test the settings in the dialog; a typed key goes to the local daemon only.
async fn test_agent_service(
    State(s): State<Panel>,
    Path(id): Path<Uuid>,
    Json(body): Json<hmesh_core::service::TestAgentService>,
) -> Response {
    session_reply(
        s.client
            .post::<_, Value>(&format!("/local/agents/{id}/service/test"), &body)
            .await,
    )
}
#[derive(Deserialize)]
struct SessionCursor {
    #[serde(default)]
    after: u64,
    #[serde(default)]
    wait: u64,
    #[serde(default)]
    status_only: bool,
}
async fn session_events(
    State(s): State<Panel>,
    Path(id): Path<Uuid>,
    Query(cursor): Query<SessionCursor>,
) -> Response {
    session_reply(
        s.client
            .get::<Value>(&format!(
                "/local/sessions/{id}/events?after={}&wait={}&status_only={}",
                cursor.after,
                cursor.wait.min(10),
                cursor.status_only
            ))
            .await,
    )
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SessionPromptForm {
    message: String,
}
async fn session_prompt(
    State(s): State<Panel>,
    Path(id): Path<Uuid>,
    Json(body): Json<SessionPromptForm>,
) -> Response {
    session_reply(
        s.client
            .post::<_, Value>(
                &format!("/local/sessions/{id}/prompt"),
                &json!({"message":body.message}),
            )
            .await,
    )
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PermissionForm {
    allow: bool,
}
async fn session_permission(
    State(s): State<Panel>,
    Path((id, key)): Path<(Uuid, String)>,
    Json(body): Json<PermissionForm>,
) -> Response {
    if key.is_empty() || key.len() > 32 || !key.bytes().all(|b| b.is_ascii_alphanumeric()) {
        return error(StatusCode::BAD_REQUEST, "INVALID_PERMISSION_KEY");
    }
    session_reply(
        s.client
            .post::<_, Value>(
                &format!("/local/sessions/{id}/permissions/{key}"),
                &json!({"allow":body.allow}),
            )
            .await,
    )
}
async fn session_action(
    State(s): State<Panel>,
    Path((id, name)): Path<(Uuid, String)>,
    Json(body): Json<Value>,
) -> Response {
    if !["cancel", "close", "approve", "reject"].contains(&name.as_str()) || body != json!({}) {
        return error(StatusCode::BAD_REQUEST, "UNSUPPORTED_ACTION");
    }
    session_reply(
        s.client
            .post::<_, Value>(&format!("/local/sessions/{id}/{name}"), &body)
            .await,
    )
}
/// Pair requests worth showing: open ones, and ones settled in the last few
/// minutes so the person sees how theirs ended. Certificates stay out.
fn recent_pairs(rows: Value) -> Value {
    let recent = now() - 600;
    Value::Array(
        rows.as_array()
            .into_iter()
            .flatten()
            .filter(|r| {
                let expires = r["expires_at"].as_i64().unwrap_or(0);
                // A request from another computer that expired can no longer be
                // approved; one sent from here stays until its answer is known.
                if r["status"] == "pending" {
                    r["direction"] == "outgoing" || expires > now()
                } else {
                    expires > recent
                }
            })
            .map(|r| {
                let card = &r["card"];
                json!({"request_id":r["request_id"],"direction":r["direction"],"status":r["status"],"expires_at":r["expires_at"],
                    "card":{"node_id":card["node_id"],"display_name":card["display_name"],"fingerprint":card["fingerprint"],"endpoint":card["endpoint"]}})
            })
            .collect(),
    )
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PairCode {
    code: String,
}
/// Send a pair request to the computer whose pairing code was pasted.
async fn request_pair(
    State(s): State<Panel>,
    body: Result<Json<PairCode>, JsonRejection>,
) -> Response {
    let Some((endpoint, expected_fingerprint)) = body
        .ok()
        .and_then(|Json(body)| hmesh_security::parse_pair_code(&body.code).ok())
    else {
        return error(StatusCode::BAD_REQUEST, "INVALID_PAIR_CODE");
    };
    mutation(
        s.client
            .post(
                "/local/pairs/request",
                &hmesh_daemon::PairTarget {
                    endpoint,
                    expected_fingerprint,
                },
            )
            .await,
    )
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PairCheck {
    #[serde(default)]
    check: String,
}
/// Approve or decline a request from another computer, or cancel one sent
/// from here. Approving takes the last six digits of the other computer's
/// fingerprint, which its panel shows after it sends the request.
/// Open the pairing window for ten minutes (a direct network answers the
/// pairing handshake only while it is open).
async fn open_pairing_window(State(s): State<Panel>) -> Response {
    mutation(s.client.post("/local/pairing/window", &json!({})).await)
}
async fn pair_action(
    State(s): State<Panel>,
    Path((id, name)): Path<(Uuid, String)>,
    body: Result<Json<PairCheck>, JsonRejection>,
) -> Response {
    let Ok(Json(body)) = body else {
        return error(StatusCode::BAD_REQUEST, "UNSUPPORTED_ACTION");
    };
    match name.as_str() {
        "decline" => mutation(
            s.client
                .post("/local/pairs/decline", &json!({"request_id": id}))
                .await,
        ),
        "approve" => {
            let pairs = match s.client.get::<Value>("/local/pairs").await {
                Ok(pairs) => pairs,
                Err(e) => return mutation(Err(e)),
            };
            let Some(record) = pairs.as_array().into_iter().flatten().find(|r| {
                r["request_id"] == json!(id)
                    && r["direction"] == "incoming"
                    && r["status"] == "pending"
            }) else {
                return error(StatusCode::CONFLICT, "PAIR_REQUEST_CLOSED");
            };
            let fingerprint = record["card"]["fingerprint"]
                .as_str()
                .unwrap_or_default()
                .to_ascii_lowercase();
            let check = body.check.trim().to_ascii_lowercase();
            if check.len() != 6 || !fingerprint.ends_with(&check) {
                return error(StatusCode::BAD_REQUEST, "PAIR_CHECK_MISMATCH");
            }
            // Six digits can be matched by a certificate made for the purpose.
            // When another open request carries the same check code, one of
            // them is not who it says: approve neither from here.
            let alike = pairs
                .as_array()
                .into_iter()
                .flatten()
                .filter(|r| {
                    r["direction"] == "incoming"
                        && r["status"] == "pending"
                        && r["expires_at"].as_i64().is_some_and(|t| t > now())
                        && r["card"]["fingerprint"]
                            .as_str()
                            .is_some_and(|f| f.to_ascii_lowercase().ends_with(&check))
                })
                .count();
            if alike > 1 {
                return error(StatusCode::CONFLICT, "PAIR_CHECK_AMBIGUOUS");
            }
            mutation(
                s.client
                    .post(
                        "/local/pairs/approve",
                        &hmesh_daemon::PairApproval {
                            request_id: id,
                            expected_fingerprint: fingerprint,
                        },
                    )
                    .await,
            )
        }
        _ => error(StatusCode::BAD_REQUEST, "UNSUPPORTED_ACTION"),
    }
}
async fn remote_agents(State(s): State<Panel>, Path(id): Path<Uuid>) -> Json<Value> {
    Json(result(
        s.client
            .get::<Value>(&format!("/local/agents/remote/{id}"))
            .await,
    ))
}
async fn action(
    s: Panel,
    kind: &str,
    id: Uuid,
    action: &str,
    body: Value,
    allowed: &[&str],
) -> Response {
    if !allowed.contains(&action) || body != json!({}) {
        return error(StatusCode::BAD_REQUEST, "UNSUPPORTED_ACTION");
    }
    // One dispatch only. An uncertain upstream result is never automatically retried.
    let response = s
        .client
        .post::<_, Value>(&format!("/local/{kind}/{id}/{action}"), &body)
        .await;
    match response {
        Ok(value) => Json(json!({"data":value})).into_response(),
        Err(e) => (
            StatusCode::BAD_GATEWAY,
            Json(json!({"error":{"code":e.code,"automatic_retry":false}})),
        )
            .into_response(),
    }
}
async fn task_action(
    State(s): State<Panel>,
    Path((id, name)): Path<(Uuid, String)>,
    Json(body): Json<Value>,
) -> Response {
    if name == "approve" && body == json!({}) {
        // The browser's preview is informational. Recheck even legacy tasks
        // without a frozen contract immediately before dispatching approval.
        let preflight = match s
            .client
            .get::<Value>(&format!("/local/tasks/{id}/preflight"))
            .await
        {
            Ok(value) => preflight_view(value),
            Err(e) => return preflight_error(e),
        };
        if preflight["ready"] != true {
            return failed_preflight(preflight);
        }
    }
    action(
        s,
        "tasks",
        id,
        &name,
        body,
        &["approve", "reject", "cancel", "archive"],
    )
    .await
}
async fn agent_action(
    State(s): State<Panel>,
    Path((id, name)): Path<(Uuid, String)>,
    Json(body): Json<Value>,
) -> Response {
    action(s, "agents", id, &name, body, &["retire", "restore"]).await
}
async fn service_action(
    State(s): State<Panel>,
    Path((id, name)): Path<(Uuid, String)>,
    Json(body): Json<Value>,
) -> Response {
    action(s, "services", id, &name, body, &["restart", "revoke"]).await
}

async fn development_read(State(s): State<Panel>, Path(kind): Path<String>) -> Response {
    if ![
        "templates",
        "workflows",
        "diagnostics",
        "storage",
        "usage",
        "budgets",
        "compatibility",
    ]
    .contains(&kind.as_str())
    {
        return error(StatusCode::NOT_FOUND, "NOT_FOUND");
    }
    mutation(s.client.get::<Value>(&format!("/local/{kind}")).await)
}
async fn development_write(
    State(s): State<Panel>,
    Path(kind): Path<String>,
    Json(body): Json<Value>,
) -> Response {
    let valid = match kind.as_str() {
        "templates" => {
            serde_json::from_value::<hmesh_core::development::TaskTemplate>(body.clone()).is_ok()
        }
        "workflows" => {
            serde_json::from_value::<hmesh_core::development::WorkflowSpec>(body.clone()).is_ok()
        }
        "budgets" => {
            serde_json::from_value::<hmesh_core::development::ProjectBudget>(body.clone()).is_ok()
        }
        _ => return error(StatusCode::NOT_FOUND, "NOT_FOUND"),
    };
    if !valid {
        return error(StatusCode::BAD_REQUEST, "INVALID_REQUEST");
    }
    mutation(s.client.post(&format!("/local/{kind}"), &body).await)
}
async fn development_template_run(
    State(s): State<Panel>,
    Json(body): Json<hmesh_daemon::TemplateRunRequest>,
) -> Response {
    mutation(s.client.post("/local/templates/run", &body).await)
}
async fn development_probe(State(s): State<Panel>, Json(body): Json<Value>) -> Response {
    if body.as_object().is_none_or(|o| o.len() != 1)
        || body
            .get("agent_id")
            .and_then(Value::as_str)
            .is_none_or(|id| Uuid::parse_str(id).is_err())
    {
        return error(StatusCode::BAD_REQUEST, "INVALID_REQUEST");
    }
    mutation(s.client.post("/local/compatibility/probe", &body).await)
}
async fn development_workflow_action(
    State(s): State<Panel>,
    Path((id, name)): Path<(Uuid, String)>,
    Json(body): Json<Value>,
) -> Response {
    action(s, "workflows", id, &name, body, &["cancel", "accept"]).await
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn resource_projection_removes_owner_process_configuration() {
        let view = resources(Ok(
            json!([{"id":"demo","process":{"env":{"KEY":"private-key-canary"},"args":["private-arg-canary"]},"status":{"error":"private-error-canary","process":"running"}}]),
        ));
        let text = view.to_string();
        assert!(!text.contains("private-"));
        assert_eq!(view["data"][0]["managed"], true);
    }
}
