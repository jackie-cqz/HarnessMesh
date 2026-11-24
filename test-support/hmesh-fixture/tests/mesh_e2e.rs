//! Two real daemons on loopback mTLS, the stock MCP server and the fixture
//! harness. Exercises discovery, dispatch, per-peer access and final replies.
//! Never calls a model and never counts as real Harness evidence.
use hmesh_core::*;
use hmesh_mcp::{OwnerBackend, Protocol};
use hmesh_security::Instance;
use hmesh_store::Store;
use serde_json::{Value, json};
use std::{
    path::{Path, PathBuf},
    time::{Duration, Instant},
};
use uuid::Uuid;

fn free_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}
fn init(root: &Path, name: &str) -> Instance {
    let mut config = NodeConfig::default();
    config.node.name = name.into();
    config.node.listen_port = free_port();
    Instance::init(&root.join(name), config).unwrap()
}
/// A verified fixture agent; `require_remote_task_approval` keeps its legacy
/// default (true) to prove it no longer forces approval.
fn register_fixture(instance: &Instance, project: &Path) -> Uuid {
    std::fs::create_dir_all(project).unwrap();
    let config: AgentConfig = serde_json::from_value(json!({
        "agent":{"name":"fixture-pi","harness":"pi","project_id":"demo"},
        "project":{"root":project,"allowed_tools":["read","write","edit"]},
        "adapter":{"executable":env!("CARGO_BIN_EXE_hmesh-fixture"),"provider":"fixture","model":"fixture"}
    }))
    .unwrap();
    let agent_id = Uuid::new_v4();
    let card: AgentCard = serde_json::from_value(json!({
        "agent_id":agent_id,"node_id":instance.card.node_id,"name":"fixture-pi","harness":"pi","project_ids":["demo"],"availability":"idle","capabilities":[],
        "probe":{"installed":true,"version":"test-fixture","version_supported":true,"protocol_probe":"fixture","authentication":"not_applicable_test_fixture","managed_run":"passed","streaming":"fixture","cancel":"not_checked","session_resume":"not_checked","unavailable_reason":null}
    }))
    .unwrap();
    Store::open(&instance.dir.join("state.sqlite3"))
        .unwrap()
        .register_agent(&card, &config)
        .unwrap();
    agent_id
}
struct Daemon {
    dir: PathBuf,
    thread: Option<std::thread::JoinHandle<()>>,
}
impl Daemon {
    fn start(dir: &Path) -> Self {
        let owned = dir.to_path_buf();
        let thread = std::thread::spawn(move || {
            let runtime = tokio::runtime::Runtime::new().unwrap();
            runtime.block_on(hmesh_daemon::run(&owned)).unwrap();
        });
        Self {
            dir: dir.to_path_buf(),
            thread: Some(thread),
        }
    }
    fn owner(&self) -> hmesh_transport::Client {
        hmesh_daemon::local_client(&Instance::load(&self.dir).unwrap()).unwrap()
    }
    async fn ready(&self) {
        let deadline = Instant::now() + Duration::from_secs(30);
        loop {
            if let Ok(instance) = Instance::load(&self.dir)
                && let Ok(client) = hmesh_daemon::local_client(&instance)
                && client.get::<Value>("/local/status").await.is_ok()
            {
                return;
            }
            assert!(Instant::now() < deadline, "daemon did not start");
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    }
    async fn stop(mut self) {
        let _ = self
            .owner()
            .post::<_, Value>("/local/stop", &json!({}))
            .await;
        if let Some(thread) = self.thread.take() {
            tokio::task::spawn_blocking(move || thread.join().unwrap())
                .await
                .unwrap();
        }
    }
}
async fn initialized(dir: &Path) -> Protocol<OwnerBackend> {
    let mut mcp = Protocol::new(OwnerBackend::load(dir).unwrap());
    let reply = mcp
        .handle(json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-06-18","capabilities":{},"clientInfo":{"name":"e2e","version":"1"}}}))
        .await
        .unwrap();
    assert_eq!(reply["result"]["protocolVersion"], "2025-06-18");
    mcp
}
async fn call(mcp: &mut Protocol<OwnerBackend>, name: &str, arguments: Value) -> Value {
    let reply = mcp
        .handle(json!({"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":name,"arguments":arguments}}))
        .await
        .unwrap();
    assert!(reply.get("error").is_none(), "{reply}");
    assert_eq!(reply["result"]["isError"], false, "{reply}");
    reply["result"]["structuredContent"].clone()
}

#[tokio::test(flavor = "multi_thread")]
async fn any_agent_lists_paired_agents_dispatches_without_approval_and_reads_the_final_reply() {
    let root = tempfile::tempdir().unwrap();
    let a = init(root.path(), "node-a");
    let b = init(root.path(), "node-b");
    Store::open(&a.dir.join("state.sqlite3"))
        .unwrap()
        .trust_peer(&b.card)
        .unwrap();
    Store::open(&b.dir.join("state.sqlite3"))
        .unwrap()
        .trust_peer(&a.card)
        .unwrap();
    register_fixture(&a, &root.path().join("project-a"));
    let remote_agent = register_fixture(&b, &root.path().join("project-b"));
    let daemon_a = Daemon::start(&a.dir);
    let daemon_b = Daemon::start(&b.dir);
    daemon_a.ready().await;
    daemon_b.ready().await;
    let mut mcp = initialized(&a.dir).await;

    // Pairing alone makes node-b's agents visible; no policy grant was written.
    let listed = call(&mut mcp, "mesh_list_agents", json!({})).await;
    let refs: Vec<&str> = listed["agents"]
        .as_array()
        .unwrap()
        .iter()
        .map(|agent| agent["ref"].as_str().unwrap())
        .collect();
    assert_eq!(refs, ["node-a/fixture-pi", "node-b/fixture-pi"]);
    let remote = &listed["agents"][1];
    assert_eq!(remote["local"], false);
    assert_eq!(remote["ready"], true);
    assert_eq!(remote["agent_id"], remote_agent.to_string());
    assert_eq!(listed["nodes"][1]["reachable"], true);

    // Default access is auto: the task is never parked for approval.
    let sent = call(
        &mut mcp,
        "mesh_send",
        json!({"agent":"node-b/fixture-pi","message":"fixture:reply"}),
    )
    .await;
    assert_ne!(sent["status"], "PENDING_APPROVAL", "{sent}");
    let task_id = sent["task_id"].as_str().unwrap().to_owned();
    let done = call(
        &mut mcp,
        "mesh_wait",
        json!({"task_id":task_id,"wait_seconds":60}),
    )
    .await;
    assert_eq!(done["done"], true, "{done}");
    assert_eq!(done["status"], "SUCCEEDED");
    assert_eq!(done["final_text"], "Final answer: the build is green.");
    assert_eq!(done["reply_source"], "result_event");
    // The full text, narration included, is still in the event log.
    let events = call(&mut mcp, "mesh_get_task", json!({"task_id":task_id})).await;
    let text: String = events["events"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|e| e["payload"]["type"] == "text_delta")
        .map(|e| e["payload"]["text"].as_str().unwrap())
        .collect();
    assert_eq!(
        text,
        "Let me check the project.Final answer: the build is green."
    );

    // Local agents are reachable through the same tools, with send+wait in one call.
    let local = call(
        &mut mcp,
        "mesh_send",
        json!({"agent":"node-a/fixture-pi","message":"fixture:reply","wait_seconds":60}),
    )
    .await;
    assert_eq!(local["status"], "SUCCEEDED", "{local}");
    assert_eq!(local["final_text"], "Final answer: the build is green.");

    // node-b switches node-a to confirm: new tasks wait for node-b's owner.
    let switched: Value = daemon_b
        .owner()
        .post(
            &format!("/local/peers/{}/access", a.card.node_id),
            &json!({"access":"confirm"}),
        )
        .await
        .unwrap();
    assert_eq!(switched["access"], "confirm");
    let peers: Vec<PeerView> = daemon_b.owner().get("/local/peers").await.unwrap();
    assert_eq!(peers[0].access, PeerAccess::Confirm);
    // A bare name is ambiguous between the two nodes and submits nothing.
    let ambiguous = mcp
        .handle(json!({"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"mesh_send","arguments":{"agent":"fixture-pi","message":"fixture:reply"}}}))
        .await
        .unwrap();
    assert_eq!(ambiguous["result"]["isError"], true);
    let message = ambiguous["result"]["structuredContent"]["error"]["message"]
        .as_str()
        .unwrap();
    assert!(message.contains("node-a/fixture-pi") && message.contains("node-b/fixture-pi"));
    let parked = call(
        &mut mcp,
        "mesh_send",
        json!({"agent":"node-b/fixture-pi","message":"fixture:reply"}),
    )
    .await;
    assert_eq!(parked["status"], "PENDING_APPROVAL", "{parked}");
    let parked_id = parked["task_id"].as_str().unwrap().to_owned();
    let still = call(
        &mut mcp,
        "mesh_wait",
        json!({"task_id":parked_id,"wait_seconds":1}),
    )
    .await;
    assert_eq!(still["done"], false);
    assert_eq!(still["status"], "PENDING_APPROVAL");
    let _: TaskRecord = daemon_b
        .owner()
        .post(&format!("/local/tasks/{parked_id}/approve"), &json!({}))
        .await
        .unwrap();
    let approved = call(
        &mut mcp,
        "mesh_wait",
        json!({"task_id":parked_id,"wait_seconds":60}),
    )
    .await;
    assert_eq!(approved["status"], "SUCCEEDED", "{approved}");
    assert_eq!(approved["final_text"], "Final answer: the build is green.");

    // Back to auto: removes the override instead of writing the default.
    let _: Value = daemon_b
        .owner()
        .post(
            &format!("/local/peers/{}/access", a.card.node_id),
            &json!({"access":"auto"}),
        )
        .await
        .unwrap();
    let policy = std::fs::read_to_string(b.dir.join("policy.toml")).unwrap();
    assert!(!policy.contains(&a.card.node_id.to_string()), "{policy}");

    daemon_a.stop().await;
    daemon_b.stop().await;
}
