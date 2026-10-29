//! Actual stdio process tests with synthetic identities/tasks; never a model.
use hmesh_core::*;
use hmesh_security::Instance;
use hmesh_store::Store;
use serde_json::{Value, json};
use tokio::io::AsyncWriteExt;
use uuid::Uuid;

fn fixture() -> (tempfile::TempDir, std::path::PathBuf, Uuid, MeshTarget) {
    let root = tempfile::tempdir().unwrap();
    let dir = root.path().join("node");
    let mut options = NodeConfig::default();
    let closed = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    options.node.listen_port = closed.local_addr().unwrap().port();
    drop(closed);
    let instance = Instance::init(&dir, options).unwrap();
    hmesh_security::atomic_write(
        &dir.join("daemon.json"),
        &serde_json::to_vec(&instance.card).unwrap(),
    )
    .unwrap();
    let target = MeshTarget {
        peer_id: Uuid::new_v4(),
        agent_id: Uuid::new_v4(),
        project_id: "target".into(),
    };
    let scope = MeshToolScope {
        targets: vec![target.clone()],
        max_submissions: 1,
        max_timeout_seconds: 300,
        max_output_bytes: 1048576,
    };
    let scope_path = root.path().join("scope.json");
    std::fs::write(&scope_path, serde_json::to_vec(&scope).unwrap()).unwrap();
    let config:AgentConfig=serde_json::from_value(json!({"agent":{"name":"synthetic-parent","harness":"pi","project_id":"fixture"},"project":{"root":root.path(),"allowed_tools":["read"]},"adapter":{"executable":std::env::current_exe().unwrap(),"mesh_server":env!("CARGO_BIN_EXE_hmesh-mcp"),"mesh_scope":scope_path}})).unwrap();
    let card:AgentCard=serde_json::from_value(json!({"agent_id":Uuid::new_v4(),"node_id":instance.card.node_id,"name":"synthetic-parent","harness":"pi","project_ids":["fixture"],"availability":"fixture","capabilities":[],"probe":{"installed":true,"version":"fixture","version_supported":false,"protocol_probe":"fixture","authentication":"not_checked","managed_run":"not_checked","streaming":"not_checked","cancel":"not_checked","session_resume":"not_checked","unavailable_reason":null}})).unwrap();
    let store = Store::open(&dir.join("state.sqlite3")).unwrap();
    store.register_agent(&card, &config).unwrap();
    let request = Envelope::new(
        instance.card.node_id,
        "task.submit",
        TaskSubmit::new(card.agent_id, "fixture", "synthetic parent"),
    );
    let parent = store
        .admit(instance.card.node_id, &request, false, 20)
        .unwrap();
    store.claim_next().unwrap().unwrap();
    (root, dir, parent.task_id, target)
}
fn handshake() -> String {
    format!(
        "{}\n{}\n",
        json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-11-25","capabilities":{},"clientInfo":{"name":"fixture","version":"1"}}}),
        json!({"jsonrpc":"2.0","method":"notifications/initialized"})
    )
}
async fn run(dir: &std::path::Path, parent: Uuid, input: &[u8]) -> std::process::Output {
    let mut child = tokio::process::Command::new(env!("CARGO_BIN_EXE_hmesh-mcp"))
        .arg("--data-dir")
        .arg(dir)
        .arg("--parent-task")
        .arg(parent.to_string())
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .unwrap();
    let mut stdin = child.stdin.take().unwrap();
    let data = input.to_vec();
    let writer = tokio::spawn(async move {
        let _ = stdin.write_all(&data).await;
    });
    let result = tokio::time::timeout(std::time::Duration::from_secs(10), child.wait_with_output())
        .await
        .unwrap()
        .unwrap();
    writer.await.unwrap();
    result
}
#[tokio::test]
async fn real_stdio_has_no_non_json_stdout_and_denies_unrelated_tasks_and_targets() {
    let (_root, dir, parent, _target) = fixture();
    let mut input = String::from("invalid-json\n");
    input.push_str(&handshake());
    for (id, name, args) in [
        (2, "mesh_list_peers", json!({})),
        (3, "mesh_get_task", json!({"task_id":parent})),
        (4, "mesh_cancel_task", json!({"task_id":Uuid::new_v4()})),
        (
            5,
            "mesh_submit_task",
            json!({"peer_id":Uuid::new_v4(),"agent_id":Uuid::new_v4(),"project_id":"target","request_id":Uuid::new_v4(),"goal":"must not execute"}),
        ),
    ] {
        input.push_str(&format!("{}\n",json!({"jsonrpc":"2.0","id":id,"method":"tools/call","params":{"name":name,"arguments":args}})));
    }
    let result = run(&dir, parent, input.as_bytes()).await;
    assert!(result.status.success());
    let records = String::from_utf8(result.stdout)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str::<Value>(line).unwrap())
        .collect::<Vec<_>>();
    assert_eq!(records[0]["error"]["code"], -32700);
    assert_eq!(records.len(), 6);
    for record in records
        .iter()
        .filter(|r| r["id"].as_i64().is_some_and(|id| id >= 3))
    {
        assert_eq!(record["result"]["isError"], true);
        assert_eq!(
            record["result"]["structuredContent"]["error"]["code"],
            "PERMISSION_DENIED"
        );
    }
}
#[tokio::test]
async fn ambiguous_submission_preserves_id_and_consumes_a_persistent_parent_budget() {
    let (_root, dir, parent, target) = fixture();
    let first = Uuid::new_v4();
    let second = Uuid::new_v4();
    let mut input = handshake();
    for (id, request) in [(2, first), (3, second)] {
        input.push_str(&format!("{}\n",json!({"jsonrpc":"2.0","id":id,"method":"tools/call","params":{"name":"mesh_submit_task","arguments":{"peer_id":target.peer_id,"agent_id":target.agent_id,"project_id":target.project_id,"request_id":request,"goal":"synthetic child"}}})));
    }
    let result = run(&dir, parent, input.as_bytes()).await;
    assert!(result.status.success());
    let records = String::from_utf8(result.stdout)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str::<Value>(line).unwrap())
        .collect::<Vec<_>>();
    assert_eq!(records[1]["result"]["isError"], true);
    assert_eq!(
        records[1]["result"]["structuredContent"]["task_id"],
        first.to_string()
    );
    assert_eq!(
        records[2]["result"]["structuredContent"]["error"]["code"],
        "RATE_LIMITED"
    );
    let store = Store::open(&dir.join("state.sqlite3")).unwrap();
    assert!(store.tool_child_allowed(parent, first).unwrap());
    assert!(!store.tool_child_allowed(parent, second).unwrap());
}
#[tokio::test]
async fn incomplete_and_oversized_stdio_frames_fail_without_echoing_input() {
    let (_root, dir, parent, _) = fixture();
    for input in [
        b"{\"truncated\":".to_vec(),
        vec![b'x'; hmesh_mcp::MAX_FRAME_BYTES + 1],
    ] {
        let result = run(&dir, parent, &input).await;
        assert!(!result.status.success());
        assert!(result.stdout.is_empty());
        assert!(result.stderr.len() < 200);
    }
}
