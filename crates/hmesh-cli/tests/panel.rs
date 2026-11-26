//! Real CLI processes, loopback HTTP panel and pinned owner mTLS. No registered model.
use hmesh_core::*;
use hmesh_security::Instance;
use hmesh_store::Store;
use serde_json::{Value, json};
use std::{process::Stdio, time::Duration};
use tokio::{
    io::{AsyncBufReadExt, BufReader},
    process::Command,
};

async fn start_panel(dir: &std::path::Path) -> (tokio::process::Child, Value) {
    let mut child = Command::new(env!("CARGO_BIN_EXE_hmesh"))
        .arg("--data-dir")
        .arg(dir)
        .args(["--json", "panel", "--start-daemon", "--port", "0"])
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .kill_on_drop(true)
        .spawn()
        .unwrap();
    let mut lines = BufReader::new(child.stdout.take().unwrap()).lines();
    let line = tokio::time::timeout(Duration::from_secs(25), lines.next_line())
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    (child, serde_json::from_str(&line).unwrap())
}
async fn stop_panel(http: &reqwest::Client, ready: &Value) {
    let (origin, token) = ready["url"]
        .as_str()
        .unwrap()
        .split_once("/#access=")
        .unwrap();
    assert_eq!(
        http.post(format!("{origin}/api/panel/stop"))
            .bearer_auth(token)
            .header("origin", origin)
            .json(&json!({}))
            .send()
            .await
            .unwrap()
            .status(),
        200
    );
}

#[tokio::test]
async fn adapter_install_requires_owner_origin_and_an_allowlisted_package() {
    let root = tempfile::tempdir().unwrap();
    let (mut panel, ready) = start_panel(root.path()).await;
    let (origin, token) = ready["url"]
        .as_str()
        .unwrap()
        .split_once("/#access=")
        .unwrap();
    let http = reqwest::Client::builder()
        .no_proxy()
        .timeout(Duration::from_secs(10))
        .build()
        .unwrap();
    let path = format!("{origin}/api/agents/install");
    let offers: Value = http
        .get(&path)
        .bearer_auth(token)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let kinds: Vec<_> = offers["data"]
        .as_array()
        .unwrap()
        .iter()
        .map(|row| row["harness"].as_str().unwrap())
        .collect();
    assert_eq!(kinds, ["codex", "claude-code"]);
    let install = |body: Value| http.post(&path).json(&body);
    assert_eq!(
        install(json!({"harness":"codex"}))
            .header("origin", origin)
            .send()
            .await
            .unwrap()
            .status(),
        401
    );
    assert_eq!(
        install(json!({"harness":"codex"}))
            .bearer_auth(token)
            .send()
            .await
            .unwrap()
            .status(),
        403
    );
    assert_eq!(
        install(json!({"harness":"codex","package":"unreviewed"}))
            .bearer_auth(token)
            .header("origin", origin)
            .send()
            .await
            .unwrap()
            .status(),
        422
    );
    assert_eq!(
        install(json!({"harness":"pi"}))
            .bearer_auth(token)
            .header("origin", origin)
            .send()
            .await
            .unwrap()
            .status(),
        502
    );
    assert!(!root.path().join("agent-runtimes").exists());
    stop_panel(&http, &ready).await;
    assert!(panel.wait().await.unwrap().success());
}
#[tokio::test]
async fn panel_launch_and_stop_own_only_the_daemon_started_here() {
    let root = tempfile::tempdir().unwrap();
    let dir = root.path().join("empty instance");
    let http = reqwest::Client::builder()
        .no_proxy()
        .timeout(Duration::from_secs(10))
        .build()
        .unwrap();
    let (mut panel, ready) = start_panel(&dir).await;
    assert!(ready["owned_daemon_pid"].is_u64());
    let instance = Instance::load(&dir).unwrap();
    let original = instance.card.node_id;
    stop_panel(&http, &ready).await;
    assert!(
        tokio::time::timeout(Duration::from_secs(15), panel.wait())
            .await
            .unwrap()
            .unwrap()
            .success()
    );
    if let Ok(client) = hmesh_daemon::local_client(&instance) {
        assert!(client.get::<Value>("/local/status").await.is_err());
    }
    let mut existing = Command::new(env!("CARGO_BIN_EXE_hmesh"))
        .arg("--data-dir")
        .arg(&dir)
        .args(["daemon", "start"])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .kill_on_drop(true)
        .spawn()
        .unwrap();
    tokio::time::timeout(Duration::from_secs(20), async {
        loop {
            if let Ok(c) = hmesh_daemon::local_client(&instance)
                && c.get::<Value>("/local/status").await.is_ok()
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await
    .unwrap();
    let (mut panel, ready) = start_panel(&dir).await;
    assert!(ready["owned_daemon_pid"].is_null());
    stop_panel(&http, &ready).await;
    assert!(
        tokio::time::timeout(Duration::from_secs(15), panel.wait())
            .await
            .unwrap()
            .unwrap()
            .success()
    );
    let current: Value = hmesh_daemon::local_client(&instance)
        .unwrap()
        .get("/local/status")
        .await
        .unwrap();
    assert_eq!(current["pid"], existing.id().unwrap());
    assert_eq!(Instance::load(&dir).unwrap().card.node_id, original);
    existing.kill().await.unwrap();
    existing.wait().await.unwrap();
}
#[tokio::test]
async fn occupied_panel_port_never_initializes_or_starts_a_daemon() {
    let root = tempfile::tempdir().unwrap();
    let dir = root.path().join("must stay absent");
    let port = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let number = port.local_addr().unwrap().port().to_string();
    let result = tokio::time::timeout(
        Duration::from_secs(10),
        Command::new(env!("CARGO_BIN_EXE_hmesh"))
            .arg("--data-dir")
            .arg(&dir)
            .args(["panel", "--start-daemon", "--port", &number])
            .output(),
    )
    .await
    .unwrap()
    .unwrap();
    assert!(!result.status.success());
    assert!(!dir.exists());
}

#[tokio::test]
async fn legacy_daemon_remains_available_for_read_only_panel_without_migration() {
    let root = tempfile::tempdir().unwrap();
    let dir = root.path().join("legacy");
    let mut config = NodeConfig::default();
    config.node.listen_port = 0;
    let mut instance = Instance::init(&dir, config).unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    instance.card.endpoint = format!("https://{}", listener.local_addr().unwrap());
    std::fs::write(
        dir.join("daemon.json"),
        serde_json::to_vec(&instance.card).unwrap(),
    )
    .unwrap();
    let instance = std::sync::Arc::new(instance);
    let (stop, rx) = tokio::sync::watch::channel(false);
    let identity = instance.clone();
    let app = axum::Router::new().route(
        "/local/status",
        axum::routing::get(|| async {
            axum::Json(json!({"running":true,"pid":std::process::id()}))
        }),
    );
    let server = tokio::spawn(hmesh_transport::serve(
        listener,
        app,
        std::sync::Arc::new(move || identity.server_config(&[])),
        rx,
    ));
    let (mut panel, ready) = start_panel(&dir).await;
    assert!(ready["owned_daemon_pid"].is_null());
    let (origin, token) = ready["url"]
        .as_str()
        .unwrap()
        .split_once("/#access=")
        .unwrap();
    let http = reqwest::Client::builder()
        .no_proxy()
        .timeout(Duration::from_secs(10))
        .build()
        .unwrap();
    let runtime: Value = http
        .get(format!("{origin}/api/runtime"))
        .bearer_auth(token)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(runtime["data"]["mode"], "read_only");
    assert_eq!(runtime["data"]["upgrade"]["required"], true);
    assert_eq!(runtime["data"]["upgrade"]["automatic"], false);
    assert!(
        !runtime["data"]["upgrade"]["steps"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    let denied = http
        .post(format!(
            "{origin}/api/tasks/{}/approve",
            uuid::Uuid::new_v4()
        ))
        .bearer_auth(token)
        .header("origin", origin)
        .json(&json!({}))
        .send()
        .await
        .unwrap();
    assert_eq!(denied.status(), 409);
    assert_eq!(
        denied.json::<Value>().await.unwrap()["error"]["code"],
        "PANEL_READ_ONLY"
    );
    stop_panel(&http, &ready).await;
    assert!(
        tokio::time::timeout(Duration::from_secs(10), panel.wait())
            .await
            .unwrap()
            .unwrap()
            .success()
    );
    assert!(!dir.join("state.sqlite3").exists());
    assert!(
        hmesh_daemon::local_client(&instance)
            .unwrap()
            .get::<Value>("/local/status")
            .await
            .is_ok()
    );
    stop.send(true).unwrap();
    server.await.unwrap().unwrap();
}

#[derive(Clone)]
struct MutableOwner {
    status: std::sync::Arc<std::sync::RwLock<Value>>,
    ready: std::sync::Arc<std::sync::atomic::AtomicBool>,
    preflights: std::sync::Arc<std::sync::Mutex<Vec<Value>>>,
    submissions: std::sync::Arc<std::sync::Mutex<Vec<Value>>>,
    approvals: std::sync::Arc<std::sync::atomic::AtomicUsize>,
    other_writes: std::sync::Arc<std::sync::atomic::AtomicUsize>,
}
fn mock_preflight(body: &Value, ready: bool) -> Value {
    json!({
        "ready":ready,"agent_id":body["task"]["target_agent_id"],
        "project_id":body["task"]["project_id"],"requires_approval":true,
        "configuration_sha256":"a".repeat(64),"observed_at":now(),
        "checks":[{"code":"ARTIFACT_NOT_DECLARED","passed":ready,"field":"out/health.json","private":"private-check-canary"}],
        "owner_token":"private-owner-canary","config":{"credential_source":"private-config-canary"}
    })
}
#[tokio::test]
async fn runtime_capabilities_refresh_and_preflight_blocks_submission_without_leaking_config() {
    use std::sync::{
        Arc, Mutex, RwLock,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    };
    let root = tempfile::tempdir().unwrap();
    let dir = root.path().join("mutable owner");
    let mut config = NodeConfig::default();
    config.node.listen_port = 0;
    let mut instance = Instance::init(&dir, config).unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    instance.card.endpoint = format!("https://{}", listener.local_addr().unwrap());
    let card_bytes = serde_json::to_vec(&instance.card).unwrap();
    std::fs::write(dir.join("daemon.json"), &card_bytes).unwrap();
    let instance = Arc::new(instance);
    let owner = MutableOwner {
        status: Arc::new(RwLock::new(json!({
            "running":true,"pid":std::process::id(),"schema_version":6,"network":"loopback",
            "api_features":["panel-task-workflow-v1","private-feature-canary"],
            "owner_token":"private-status-canary","config":{"env":{"KEY":"private-env-canary"}}
        }))),
        ready: Arc::new(AtomicBool::new(false)),
        preflights: Arc::new(Mutex::new(Vec::new())),
        submissions: Arc::new(Mutex::new(Vec::new())),
        approvals: Arc::new(AtomicUsize::new(0)),
        other_writes: Arc::new(AtomicUsize::new(0)),
    };
    let app = axum::Router::new()
        .route("/local/status", axum::routing::get(|axum::extract::State(s): axum::extract::State<MutableOwner>| async move {
            axum::Json(s.status.read().unwrap().clone())
        }))
        .route("/local/tasks/preflight", axum::routing::post(|axum::extract::State(s): axum::extract::State<MutableOwner>, axum::Json(body): axum::Json<Value>| async move {
            s.preflights.lock().unwrap().push(body.clone());
            axum::Json(mock_preflight(&body, s.ready.load(Ordering::SeqCst)))
        }))
        .route("/local/tasks/{id}/preflight", axum::routing::get(|axum::extract::State(s): axum::extract::State<MutableOwner>| async move {
            let body=s.preflights.lock().unwrap().last().unwrap().clone();
            axum::Json(mock_preflight(&body,s.ready.load(Ordering::SeqCst)))
        }))
        .route("/local/tasks/submit", axum::routing::post(|axum::extract::State(s): axum::extract::State<MutableOwner>, axum::Json(body): axum::Json<Value>| async move {
            s.submissions.lock().unwrap().push(body.clone());
            axum::Json(json!({"task_id":body["task"]["task_id"],"request":body["task"],"status":"PENDING_APPROVAL"}))
        }))
        .route("/local/tasks/{id}/approve", axum::routing::post(|axum::extract::State(s): axum::extract::State<MutableOwner>| async move {
            s.approvals.fetch_add(1, Ordering::SeqCst);
            axum::Json(json!({"status":"QUEUED"}))
        }))
        .fallback(|axum::extract::State(s): axum::extract::State<MutableOwner>, req: axum::extract::Request| async move {
            if req.method()!=axum::http::Method::GET { s.other_writes.fetch_add(1,Ordering::SeqCst); }
            axum::Json(json!([]))
        })
        .with_state(owner.clone());
    let (stop, rx) = tokio::sync::watch::channel(false);
    let identity = instance.clone();
    let server = tokio::spawn(hmesh_transport::serve(
        listener,
        app,
        Arc::new(move || identity.server_config(&[])),
        rx,
    ));
    let (mut panel, ready) = start_panel(&dir).await;
    assert!(ready["owned_daemon_pid"].is_null());
    let (origin, token) = ready["url"]
        .as_str()
        .unwrap()
        .split_once("/#access=")
        .unwrap();
    let http = reqwest::Client::builder()
        .no_proxy()
        .timeout(Duration::from_secs(10))
        .build()
        .unwrap();
    let get = |path: &str| http.get(format!("{origin}{path}")).bearer_auth(token);
    let post = |path: &str, body: &Value| {
        http.post(format!("{origin}{path}"))
            .bearer_auth(token)
            .header("origin", origin)
            .json(body)
    };
    assert_eq!(
        http.get(format!("{origin}/api/runtime"))
            .send()
            .await
            .unwrap()
            .status(),
        401
    );
    let legacy: Value = get("/api/runtime")
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(legacy["data"]["mode"], "read_only");
    assert_eq!(legacy["data"]["daemon"]["schema_version"], 6);
    assert_eq!(legacy["data"]["capabilities"]["task_workflow"], true);
    assert_eq!(legacy["data"]["capabilities"]["task_preflight"], false);
    assert_eq!(legacy["data"]["cli_schema_version"], 12);
    assert!(!legacy.to_string().contains("private-"));
    assert!(!legacy.to_string().contains(instance.owner_token()));
    assert_eq!(get("/api/overview").send().await.unwrap().status(), 200);
    let id = uuid::Uuid::new_v4();
    for path in [
        "/api/tasks".to_owned(),
        "/api/tasks/preflight".to_owned(),
        format!("/api/tasks/{id}/approve"),
        format!("/api/agents/{id}/retire"),
        format!("/api/services/{id}/restart"),
        "/api/onboarding/preview".to_owned(),
        "/api/development/budgets".to_owned(),
    ] {
        let response = post(&path, &json!({})).send().await.unwrap();
        assert_eq!(response.status(), 409);
        assert_eq!(
            response.json::<Value>().await.unwrap()["error"]["code"],
            "PANEL_READ_ONLY"
        );
    }
    assert_eq!(owner.other_writes.load(Ordering::SeqCst), 0);
    assert!(owner.preflights.lock().unwrap().is_empty());
    *owner.status.write().unwrap() = json!({"running":true,"pid":std::process::id(),"schema_version":8,"network":"loopback","api_features":["panel-task-workflow-v1","task-preflight-v1"]});
    let compatible: Value = get("/api/runtime")
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(compatible["data"]["mode"], "compatible");
    let missing_development = get("/api/development/templates").send().await.unwrap();
    assert_eq!(missing_development.status(), 409);
    assert_eq!(
        missing_development.json::<Value>().await.unwrap()["error"]["capability"],
        "development_workflows"
    );
    let payload = json!({"request_id":uuid::Uuid::new_v4(),"peer_id":uuid::Uuid::new_v4(),"agent_id":uuid::Uuid::new_v4(),"project_id":"fixture","goal":"Synthetic task; no model is registered.","timeout_seconds":600,"max_output_bytes":16777216,"expected_artifacts":["out/health.json"]});
    let preview = post("/api/tasks/preflight", &payload).send().await.unwrap();
    assert_eq!(preview.status(), 200);
    let preview: Value = preview.json().await.unwrap();
    assert_eq!(preview["data"]["ready"], false);
    assert!(!preview.to_string().contains("private-"));
    let blocked = post("/api/tasks", &payload).send().await.unwrap();
    assert_eq!(blocked.status(), 409);
    let blocked: Value = blocked.json().await.unwrap();
    assert_eq!(blocked["error"]["code"], "TASK_PREFLIGHT_FAILED");
    assert_eq!(blocked["error"]["submission_attempted"], false);
    assert_eq!(
        blocked["preflight"]["checks"][0]["field"],
        "out/health.json"
    );
    assert!(!blocked.to_string().contains("private-"));
    assert!(owner.submissions.lock().unwrap().is_empty());
    let preflight_count = owner.preflights.lock().unwrap().len();
    let mut unknown = payload.clone();
    unknown["command"] = json!("must never be forwarded");
    let invalid = post("/api/tasks", &unknown).send().await.unwrap();
    assert_eq!(invalid.status(), 400);
    assert_eq!(
        invalid.json::<Value>().await.unwrap()["error"]["submission_attempted"],
        false
    );
    let mut bad_type = payload.clone();
    bad_type["expected_artifacts"] = json!("out/health.json");
    assert_eq!(
        post("/api/tasks/preflight", &bad_type)
            .send()
            .await
            .unwrap()
            .status(),
        400
    );
    assert_eq!(owner.preflights.lock().unwrap().len(), preflight_count);
    owner.ready.store(true, Ordering::SeqCst);
    let created = post("/api/tasks", &payload).send().await.unwrap();
    assert_eq!(created.status(), 200);
    assert_eq!(
        created.json::<Value>().await.unwrap()["data"]["status"],
        "PENDING_APPROVAL"
    );
    {
        let submitted = owner.submissions.lock().unwrap();
        assert_eq!(submitted.len(), 1);
        assert_eq!(
            submitted[0]["task"]["expected_artifacts"],
            payload["expected_artifacts"]
        );
        assert_eq!(submitted[0]["task"]["task_id"], payload["request_id"]);
        // The executing node's per-peer access mode decides about approval.
        assert_eq!(submitted[0]["task"]["requires_approval"], false);
    }
    let existing: Value = get(&format!(
        "/api/tasks/{}/preflight",
        payload["request_id"].as_str().unwrap()
    ))
    .send()
    .await
    .unwrap()
    .json()
    .await
    .unwrap();
    assert_eq!(existing["data"]["ready"], true);
    assert!(!existing.to_string().contains("private-"));
    // Legacy tasks have no expected_artifacts contract. The panel still must
    // recheck current readiness on the server before dispatching approval.
    let mut legacy_payload = payload.clone();
    legacy_payload["request_id"] = json!(uuid::Uuid::new_v4());
    legacy_payload["expected_artifacts"] = json!([]);
    let no_artifacts = post("/api/tasks/preflight", &legacy_payload)
        .send()
        .await
        .unwrap();
    assert_eq!(no_artifacts.status(), 200);
    assert_eq!(
        no_artifacts.json::<Value>().await.unwrap()["data"]["ready"],
        true
    );
    assert!(
        owner.preflights.lock().unwrap().last().unwrap()["task"]
            .get("expected_artifacts")
            .is_none()
    );
    assert_eq!(
        post("/api/tasks", &legacy_payload)
            .send()
            .await
            .unwrap()
            .status(),
        200
    );
    assert!(
        owner.submissions.lock().unwrap()[1]["task"]
            .get("expected_artifacts")
            .is_none()
    );
    owner.ready.store(false, Ordering::SeqCst);
    let approve_path = format!(
        "/api/tasks/{}/approve",
        legacy_payload["request_id"].as_str().unwrap()
    );
    let approval_blocked = post(&approve_path, &json!({})).send().await.unwrap();
    assert_eq!(approval_blocked.status(), 409);
    let approval_blocked: Value = approval_blocked.json().await.unwrap();
    assert_eq!(approval_blocked["error"]["code"], "TASK_PREFLIGHT_FAILED");
    assert_eq!(approval_blocked["error"]["submission_attempted"], false);
    assert_eq!(approval_blocked["preflight"]["ready"], false);
    assert!(!approval_blocked.to_string().contains("private-"));
    assert_eq!(owner.approvals.load(Ordering::SeqCst), 0);
    owner.ready.store(true, Ordering::SeqCst);
    assert_eq!(
        post(&approve_path, &json!({}))
            .send()
            .await
            .unwrap()
            .status(),
        200
    );
    assert_eq!(owner.approvals.load(Ordering::SeqCst), 1);
    *owner.status.write().unwrap() = json!({"running":true,"pid":std::process::id(),"schema_version":12,"network":"loopback","api_features":["panel-task-workflow-v1","task-preflight-v1","development-workflows-v1","acp-sessions-v1"]});
    let full: Value = get("/api/runtime")
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(full["data"]["mode"], "full");
    assert_eq!(full["data"]["upgrade"]["required"], false);
    *owner.status.write().unwrap() = json!({"running":true,"pid":std::process::id(),"schema_version":6,"api_features":["task-preflight-v1","development-workflows-v1"]});
    let denied_again = post(&format!("/api/tasks/{id}/cancel"), &json!({}))
        .send()
        .await
        .unwrap();
    assert_eq!(denied_again.status(), 409);
    assert_eq!(
        denied_again.json::<Value>().await.unwrap()["error"]["code"],
        "PANEL_READ_ONLY"
    );
    assert_eq!(owner.other_writes.load(Ordering::SeqCst), 0);
    stop_panel(&http, &ready).await;
    assert!(
        tokio::time::timeout(Duration::from_secs(10), panel.wait())
            .await
            .unwrap()
            .unwrap()
            .success()
    );
    assert!(!dir.join("state.sqlite3").exists());
    assert!(std::fs::read(dir.join("daemon.json")).unwrap() == card_bytes);
    assert!(
        hmesh_daemon::local_client(&instance)
            .unwrap()
            .get::<Value>("/local/status")
            .await
            .is_ok()
    );
    stop.send(true).unwrap();
    server.await.unwrap().unwrap();
}

#[tokio::test]
async fn panel_requires_launch_access_and_same_origin_before_mutating_a_task() {
    let root = tempfile::tempdir().unwrap();
    let mut config = NodeConfig::default();
    config.node.listen_port = 0;
    let instance = Instance::init(&root.path().join("panel node"), config).unwrap();
    let store = Store::open(&instance.dir.join("state.sqlite3")).unwrap();
    let request = Envelope::new(
        instance.card.node_id,
        "task.submit",
        TaskSubmit::new(
            uuid::Uuid::new_v4(),
            "fixture",
            "<img src=x onerror=alert(1)> synthetic pending request",
        ),
    );
    let task = store
        .admit(instance.card.node_id, &request, true, 20)
        .unwrap();
    let _daemon = Command::new(env!("CARGO_BIN_EXE_hmesh"))
        .args([
            "--data-dir",
            instance.dir.to_str().unwrap(),
            "daemon",
            "start",
        ])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .kill_on_drop(true)
        .spawn()
        .unwrap();
    let http = reqwest::Client::builder()
        .no_proxy()
        .timeout(Duration::from_secs(10))
        .build()
        .unwrap();
    tokio::time::timeout(Duration::from_secs(20), async {
        loop {
            if let Ok(owner) = hmesh_daemon::local_client(&instance)
                && owner.get::<Value>("/local/status").await.is_ok()
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await
    .unwrap();
    let mut panel = Command::new(env!("CARGO_BIN_EXE_hmesh"))
        .args([
            "--json",
            "--data-dir",
            instance.dir.to_str().unwrap(),
            "panel",
            "--port",
            "0",
        ])
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .kill_on_drop(true)
        .spawn()
        .unwrap();
    let mut lines = BufReader::new(panel.stdout.take().unwrap()).lines();
    let line = tokio::time::timeout(Duration::from_secs(20), lines.next_line())
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    let ready: Value = serde_json::from_str(&line).unwrap();
    let (origin, access) = ready["url"]
        .as_str()
        .unwrap()
        .split_once("/#access=")
        .unwrap();
    let get = |path: &str| http.get(format!("{origin}{path}")).bearer_auth(access);
    assert_eq!(
        http.get(format!("{origin}/api/overview"))
            .send()
            .await
            .unwrap()
            .status(),
        401
    );
    assert_eq!(
        get("/api/overview")
            .header("host", "evil.test")
            .send()
            .await
            .unwrap()
            .status(),
        403
    );
    assert_eq!(
        get("/api/overview")
            .header("origin", "https://evil.test")
            .send()
            .await
            .unwrap()
            .status(),
        403
    );
    let page = http.get(origin).send().await.unwrap();
    assert_eq!(page.status(), 200);
    assert!(
        page.headers()["content-security-policy"]
            .to_str()
            .unwrap()
            .contains("frame-ancestors 'none'")
    );
    assert_eq!(page.headers()["cache-control"], "no-store");
    assert!(!page.text().await.unwrap().contains(instance.owner_token()));
    let overview: Value = get("/api/overview")
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(
        overview["node"]["data"]["node"]["node_id"],
        instance.card.node_id.to_string()
    );
    assert_eq!(
        overview["tasks"]["data"]["executions"][0]["status"],
        "PENDING_APPROVAL"
    );
    assert!(!overview.to_string().contains(instance.owner_token()));
    let reject = format!("{origin}/api/tasks/{}/reject", task.task_id);
    assert_eq!(
        http.post(&reject)
            .bearer_auth(access)
            .json(&json!({}))
            .send()
            .await
            .unwrap()
            .status(),
        403
    );
    assert_eq!(
        http.post(&reject)
            .bearer_auth("wrong")
            .header("origin", origin)
            .json(&json!({}))
            .send()
            .await
            .unwrap()
            .status(),
        401
    );
    assert_eq!(
        store.task(task.task_id).unwrap().status,
        TaskStatus::PendingApproval
    );
    assert_eq!(store.attempt_count(task.task_id).unwrap(), 0);
    let rejected: Value = http
        .post(&reject)
        .bearer_auth(access)
        .header("origin", origin)
        .json(&json!({}))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(rejected["data"]["status"], "REJECTED");
    let archive = format!("{origin}/api/tasks/{}/archive", task.task_id);
    assert_eq!(
        http.post(&archive)
            .bearer_auth(access)
            .header("origin", origin)
            .json(&json!({"command":"forbidden"}))
            .send()
            .await
            .unwrap()
            .status(),
        400
    );
    let before = store.events(task.task_id, 0).unwrap().len();
    let archived: Value = http
        .post(&archive)
        .bearer_auth(access)
        .header("origin", origin)
        .json(&json!({}))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(archived["data"]["archived_events"], before);
    let events: Value = get(&format!("/api/tasks/{}/events?after=0", task.task_id))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(events["data"].as_array().unwrap().len(), before);
    assert_eq!(
        get(&format!("/api/tasks/{}/events?after=-1", task.task_id))
            .send()
            .await
            .unwrap()
            .status(),
        400
    );
    assert_eq!(
        http.post(format!("{origin}/api/tasks/{}/verify", task.task_id))
            .bearer_auth(access)
            .header("origin", origin)
            .json(&json!({}))
            .send()
            .await
            .unwrap()
            .status(),
        422
    );
    assert_eq!(
        http.post(format!("{origin}/local/stop"))
            .bearer_auth(access)
            .header("origin", origin)
            .json(&json!({}))
            .send()
            .await
            .unwrap()
            .status(),
        404
    );
    assert_eq!(store.attempt_count(task.task_id).unwrap(), 0);
}
