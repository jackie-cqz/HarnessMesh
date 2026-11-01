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
async fn legacy_daemon_is_preserved_and_requires_explicit_update() {
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
    let result = tokio::time::timeout(
        Duration::from_secs(10),
        Command::new(env!("CARGO_BIN_EXE_hmesh"))
            .arg("--data-dir")
            .arg(&dir)
            .args(["panel", "--start-daemon", "--port", "0"])
            .kill_on_drop(true)
            .output(),
    )
    .await
    .unwrap()
    .unwrap();
    assert!(!result.status.success());
    assert!(String::from_utf8_lossy(&result.stderr).contains("explicit update/restart"));
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
