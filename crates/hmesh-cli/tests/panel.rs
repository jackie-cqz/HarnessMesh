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
    assert_eq!(kinds, ["codex", "claude-code", "pi", "dsh"]);
    let dsh = &offers["data"][3];
    assert_eq!(dsh["package"], "@deepseek-ai/dsh");
    assert_eq!(dsh["node"], "24.9");
    assert!(dsh["timeout_s"].as_u64().unwrap() > 300);
    assert_eq!(offers["data"][2]["needs"]["command"], "pi");
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
        install(json!({"harness":"opencode"}))
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
async fn peer_access_requires_owner_origin_and_typed_input() {
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
    let path = format!("{origin}/api/peers/{}/access", uuid::Uuid::new_v4());
    let post = |body: Value| http.post(&path).json(&body);
    let owner_post = |body: Value| post(body).bearer_auth(token).header("origin", origin);
    let setting = json!({"access":"confirm"});
    assert_eq!(
        post(setting.clone())
            .header("origin", origin)
            .send()
            .await
            .unwrap()
            .status(),
        401
    );
    assert_eq!(
        post(setting.clone())
            .bearer_auth(token)
            .header("origin", "https://other.test")
            .send()
            .await
            .unwrap()
            .status(),
        403
    );
    for invalid in [
        json!({"access":"invalid"}),
        json!({"access":"auto","peer_id":uuid::Uuid::new_v4()}),
    ] {
        let response = owner_post(invalid).send().await.unwrap();
        assert_eq!(response.status(), 400);
        assert_eq!(
            response.json::<Value>().await.unwrap()["error"]["code"],
            "INVALID_INPUT"
        );
    }
    let response = owner_post(setting).send().await.unwrap();
    assert_eq!(response.status(), 502);
    assert_eq!(
        response.json::<Value>().await.unwrap()["error"]["code"],
        "INVALID_INPUT"
    );
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

/// The panel shows this computer's own folder dialog (here a stand-in
/// program), one at a time, and reads what a folder holds before it is added.
#[cfg(unix)]
#[tokio::test(flavor = "multi_thread")]
async fn the_folder_dialog_is_this_computers_own_one_at_a_time() {
    use std::os::unix::fs::PermissionsExt;
    let root = tempfile::tempdir().unwrap();
    let data = root.path().join("node");
    let folder = std::fs::canonicalize(root.path())
        .unwrap()
        .join("my project");
    std::fs::create_dir_all(&folder).unwrap();
    let mode = root.path().join("pick-mode");
    let picker = root.path().join("pick.sh");
    std::fs::write(
        &picker,
        "#!/bin/sh\necho $$ > \"$PICK_PID\"\ncase \"$(cat \"$PICK_MODE\")\" in\n  echo) printf '%s\\n' \"$1\";;\n  cancel) ;;\n  slow) sleep 3; printf '%s/\\n' \"$PICK_DIR\";;\n  hang) exec sleep 60;;\n  *) printf '%s/\\n' \"$PICK_DIR\";;\nesac\n",
    )
    .unwrap();
    std::fs::set_permissions(&picker, std::fs::Permissions::from_mode(0o755)).unwrap();
    std::fs::write(&mode, "pick").unwrap();
    let mut child = Command::new(env!("CARGO_BIN_EXE_hmesh"))
        .arg("--data-dir")
        .arg(&data)
        .args(["--json", "panel", "--start-daemon", "--port", "0"])
        .env("HMESH_FOLDER_PICKER", &picker)
        .env("PICK_MODE", &mode)
        .env("PICK_DIR", &folder)
        .env("PICK_PID", root.path().join("pick-pid"))
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
    let ready: Value = serde_json::from_str(&line).unwrap();
    let (origin, token) = ready["url"]
        .as_str()
        .unwrap()
        .split_once("/#access=")
        .unwrap();
    let http = reqwest::Client::builder()
        .no_proxy()
        .timeout(Duration::from_secs(20))
        .build()
        .unwrap();
    let pick = |body: Value| {
        http.post(format!("{origin}/api/folders/pick"))
            .bearer_auth(token)
            .header("origin", origin)
            .json(&body)
    };
    let runtime: Value = http
        .get(format!("{origin}/api/runtime"))
        .bearer_auth(token)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(
        runtime["data"]["capabilities"]["folder_picker"], true,
        "{runtime}"
    );

    // Only the panel's own page may open it.
    let anonymous = http
        .post(format!("{origin}/api/folders/pick"))
        .header("origin", origin)
        .json(&json!({}))
        .send()
        .await
        .unwrap();
    assert_eq!(anonymous.status(), 401);
    let foreign = http
        .post(format!("{origin}/api/folders/pick"))
        .bearer_auth(token)
        .header("origin", "http://evil.example")
        .json(&json!({}))
        .send()
        .await
        .unwrap();
    assert_eq!(foreign.status(), 403);

    // The folder chosen, without the trailing slash; a cancel; the start.
    let chosen: Value = pick(json!({})).send().await.unwrap().json().await.unwrap();
    assert_eq!(chosen["data"]["path"], folder.to_str().unwrap(), "{chosen}");
    std::fs::write(&mode, "cancel").unwrap();
    let cancelled: Value = pick(json!({})).send().await.unwrap().json().await.unwrap();
    assert_eq!(cancelled["data"]["cancelled"], true, "{cancelled}");
    std::fs::write(&mode, "echo").unwrap();
    let started: Value = pick(json!({"start":folder}))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(
        started["data"]["path"],
        folder.to_str().unwrap(),
        "{started}"
    );
    // A start that is not an existing folder is not passed on: the
    // stand-in gets no argument and answers nothing, i.e. cancelled.
    let ignored: Value = pick(json!({"start":"relative/folder"}))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(ignored["data"]["cancelled"], true, "{ignored}");

    // One dialog at a time.
    std::fs::write(&mode, "slow").unwrap();
    let first = tokio::spawn(pick(json!({})).send());
    tokio::time::sleep(Duration::from_millis(800)).await;
    let busy = pick(json!({})).send().await.unwrap();
    assert_eq!(busy.status(), 409);
    let busy: Value = busy.json().await.unwrap();
    assert_eq!(busy["error"]["code"], "FOLDER_PICKER_BUSY");
    let first: Value = first.await.unwrap().unwrap().json().await.unwrap();
    assert_eq!(first["data"]["path"], folder.to_str().unwrap());
    // A browser that stops waiting closes the dialog; the next one opens.
    let gave_up = pick(json!({}))
        .timeout(Duration::from_millis(500))
        .send()
        .await;
    assert!(gave_up.is_err());
    // `kill -0` also finds a zombie: gone means killed and waited for by the
    // panel, which is still running.
    let alive = |pid: &str| {
        std::process::Command::new("kill")
            .args(["-0", pid.trim()])
            .stderr(Stdio::null())
            .status()
            .unwrap()
            .success()
    };
    let gone = std::fs::read_to_string(root.path().join("pick-pid")).unwrap();
    let closed = tokio::time::timeout(Duration::from_secs(5), async {
        while alive(&gone) {
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await;
    assert!(
        closed.is_ok(),
        "the dialog the browser gave up on still runs"
    );
    std::fs::write(&mode, "pick").unwrap();
    let again: Value = pick(json!({})).send().await.unwrap().json().await.unwrap();
    assert_eq!(again["data"]["path"], folder.to_str().unwrap(), "{again}");

    // What the folder holds: no agents here, not a project yet.
    let history: Value = http
        .get(format!("{origin}/api/folders/history"))
        .query(&[("path", folder.to_str().unwrap())])
        .bearer_auth(token)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(
        history["data"]["path"],
        folder.to_str().unwrap(),
        "{history}"
    );
    assert_eq!(history["data"]["name"], "my project");
    assert_eq!(history["data"]["project_id"], Value::Null);
    assert_eq!(history["data"]["agents"], json!([]));
    for bad in ["relative/folder", "/no/such/folder"] {
        let refused = http
            .get(format!("{origin}/api/folders/history"))
            .query(&[("path", bad)])
            .bearer_auth(token)
            .send()
            .await
            .unwrap();
        assert_eq!(refused.status(), 502, "{bad}");
    }
    // Stopping the panel closes a dialog that is still open, and the panel
    // (with the daemon it started) stops promptly.
    std::fs::write(&mode, "hang").unwrap();
    std::fs::remove_file(root.path().join("pick-pid")).unwrap();
    let open = tokio::spawn(pick(json!({})).send());
    let pid_file = root.path().join("pick-pid");
    tokio::time::timeout(Duration::from_secs(10), async {
        while !pid_file.exists() {
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await
    .unwrap();
    tokio::time::sleep(Duration::from_millis(200)).await;
    let hung = std::fs::read_to_string(&pid_file).unwrap();
    assert!(alive(&hung));
    stop_panel(&http, &ready).await;
    assert!(
        tokio::time::timeout(Duration::from_secs(15), child.wait())
            .await
            .unwrap()
            .unwrap()
            .success()
    );
    assert!(!alive(&hung), "the dialog outlived the panel");
    let answer = open.await.unwrap().unwrap();
    assert_eq!(answer.status(), 503);
    let answer: Value = answer.json().await.unwrap();
    assert_eq!(answer["error"]["code"], "PANEL_STOPPING");
}

/// Ctrl+C or a stop signal to the panel also closes an open folder dialog.
#[cfg(unix)]
#[tokio::test(flavor = "multi_thread")]
async fn a_stop_signal_closes_an_open_folder_dialog() {
    use std::os::unix::fs::PermissionsExt;
    let root = tempfile::tempdir().unwrap();
    let picker = root.path().join("hang.sh");
    let pid_file = root.path().join("pick-pid");
    std::fs::write(
        &picker,
        "#!/bin/sh\necho $$ > \"$PICK_PID\"\nexec sleep 60\n",
    )
    .unwrap();
    std::fs::set_permissions(&picker, std::fs::Permissions::from_mode(0o755)).unwrap();
    let mut child = Command::new(env!("CARGO_BIN_EXE_hmesh"))
        .arg("--data-dir")
        .arg(root.path().join("node"))
        .args(["--json", "panel", "--start-daemon", "--port", "0"])
        .env("HMESH_FOLDER_PICKER", &picker)
        .env("PICK_PID", &pid_file)
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
    let ready: Value = serde_json::from_str(&line).unwrap();
    let (origin, token) = ready["url"]
        .as_str()
        .unwrap()
        .split_once("/#access=")
        .unwrap();
    let http = reqwest::Client::builder()
        .no_proxy()
        .timeout(Duration::from_secs(30))
        .build()
        .unwrap();
    let open = tokio::spawn(
        http.post(format!("{origin}/api/folders/pick"))
            .bearer_auth(token)
            .header("origin", origin)
            .json(&json!({}))
            .send(),
    );
    tokio::time::timeout(Duration::from_secs(10), async {
        while !pid_file.exists() {
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await
    .unwrap();
    tokio::time::sleep(Duration::from_millis(200)).await;
    let alive = |pid: &str| {
        std::process::Command::new("kill")
            .args(["-0", pid.trim()])
            .stderr(Stdio::null())
            .status()
            .unwrap()
            .success()
    };
    let hung = std::fs::read_to_string(&pid_file).unwrap();
    assert!(alive(&hung));
    let panel = child.id().unwrap().to_string();
    assert!(
        std::process::Command::new("kill")
            .args(["-TERM", &panel])
            .status()
            .unwrap()
            .success()
    );
    assert!(
        tokio::time::timeout(Duration::from_secs(15), child.wait())
            .await
            .unwrap()
            .unwrap()
            .success()
    );
    assert!(!alive(&hung), "the dialog outlived the panel");
    let answer = open.await.unwrap().unwrap();
    assert_eq!(answer.status(), 503);
    let answer: Value = answer.json().await.unwrap();
    assert_eq!(answer["error"]["code"], "PANEL_STOPPING");
    // The daemon this panel started stopped with it.
    let daemon = ready["owned_daemon_pid"].as_u64().unwrap().to_string();
    tokio::time::timeout(Duration::from_secs(10), async {
        while alive(&daemon) {
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    })
    .await
    .expect("the daemon the panel started still runs");
}
