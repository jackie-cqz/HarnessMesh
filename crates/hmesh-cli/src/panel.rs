//! Loopback browser bridge. Owner TLS credentials never leave this process.
use axum::{
    Json, Router,
    extract::{DefaultBodyLimit, Path, Query, Request, State},
    http::{HeaderValue, Method, StatusCode, header},
    middleware::{self, Next},
    response::{Html, IntoResponse, Response},
    routing::{get, post},
};
use hmesh_core::{MeshError, NodeConfig, RunLimits, TaskSubmit, now};
use hmesh_daemon::{RemoteTask, VerificationRequest};
use hmesh_security::Instance;
use hmesh_transport::Client;
use serde::Deserialize;
use serde_json::{Value, json};
use std::{net::Ipv4Addr, path::Path as FilePath, process::Stdio, sync::Arc, time::Duration};
use subtle::ConstantTimeEq;
use tokio::{net::TcpListener, sync::Semaphore};
use uuid::Uuid;

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
        .route("/api/overview", get(overview))
        .route("/api/panel/stop", post(stop_panel))
        .route("/api/tasks", post(create_task))
        .route("/api/tasks/{id}", get(task))
        .route("/api/tasks/{id}/verify", post(verify_artifact))
        .route("/api/requests/{id}/recover", post(recover))
        .route("/api/tasks/{id}/events", get(events))
        .route("/api/tasks/{id}/{action}", post(task_action))
        .route("/api/agents/{id}/{action}", post(agent_action))
        .route("/api/services/{id}/{action}", post(service_action))
        .route("/api/peers/{id}/agents", get(remote_agents))
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
        Ok(Ok(status)) => {
            if !status["api_features"]
                .as_array()
                .is_some_and(|v| v.iter().any(|x| x == "panel-task-workflow-v1"))
            {
                anyhow::bail!(
                    "existing Daemon needs an explicit update/restart for panel-task-workflow-v1; it was preserved"
                );
            }
            Ok(Some(client))
        }
        _ => Ok(None),
    }
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
            next.run(req).await
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
    let (node, peers, agents, retired, tasks, services, connections, tunnels) = tokio::join!(
        s.client.get::<Value>("/local/status"),
        s.client.get::<Value>("/local/peers"),
        s.client.get::<Value>("/hmesh/v1/agents"),
        s.client.get::<Value>("/local/agents/retired"),
        s.client.get::<Value>("/local/tasks"),
        s.client.get::<Value>("/local/services"),
        s.client.get::<Value>("/local/connections"),
        s.client.get::<Value>("/local/tunnels"),
    );
    Json(
        json!({"observed_at":now(),"node":result(node),"peers":result(peers),"agents":result(agents),
        "retired":result(retired),"tasks":result(tasks),"services":resources(services),"connections":resources(connections),"tunnels":resources(tunnels)}),
    )
}
async fn task(State(s): State<Panel>, Path(id): Path<Uuid>) -> Json<Value> {
    let path = format!("/local/tasks/{id}");
    let verification_path = format!("/local/tasks/{id}/verification");
    let artifacts_path = format!("/local/tasks/{id}/artifacts");
    let (task, verification, artifacts) = tokio::join!(
        s.client.get::<Value>(&path),
        s.client.get::<Value>(&verification_path),
        s.client.get::<Value>(&artifacts_path)
    );
    Json(
        json!({"observed_at":now(),"task":result(task),"verification":result(verification),"artifacts":result(artifacts)}),
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
}
async fn create_task(State(s): State<Panel>, Json(body): Json<NewTask>) -> Response {
    let mut task = TaskSubmit::new(body.agent_id, &body.project_id, &body.goal);
    task.task_id = body.request_id;
    task.limits = RunLimits {
        timeout_seconds: body.timeout_seconds,
        max_output_bytes: body.max_output_bytes,
    };
    if task.validate().is_err() {
        return error(StatusCode::BAD_REQUEST, "INVALID_TASK_REQUEST");
    }
    mutation(
        s.client
            .post(
                "/local/tasks/submit",
                &RemoteTask {
                    peer_id: body.peer_id,
                    request_id: body.request_id,
                    task,
                },
            )
            .await,
    )
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
