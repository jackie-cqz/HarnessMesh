//! Actual stdio process tests with a synthetic node; never a model.
use hmesh_core::*;
use hmesh_security::Instance;
use serde_json::{Value, json};
use tokio::io::AsyncWriteExt;
use uuid::Uuid;

/// A node directory whose daemon is not running.
fn fixture(published: bool) -> (tempfile::TempDir, std::path::PathBuf) {
    let root = tempfile::tempdir().unwrap();
    let dir = root.path().join("node");
    let mut options = NodeConfig::default();
    let closed = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    options.node.listen_port = closed.local_addr().unwrap().port();
    drop(closed);
    let instance = Instance::init(&dir, options).unwrap();
    if published {
        hmesh_security::atomic_write(
            &dir.join("daemon.json"),
            &serde_json::to_vec(&instance.card).unwrap(),
        )
        .unwrap();
    }
    (root, dir)
}
fn handshake() -> String {
    format!(
        "{}\n{}\n",
        json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-11-25","capabilities":{},"clientInfo":{"name":"fixture","version":"1"}}}),
        json!({"jsonrpc":"2.0","method":"notifications/initialized"})
    )
}
async fn run(dir: &std::path::Path, extra: &[&str], input: &[u8]) -> std::process::Output {
    let mut child = tokio::process::Command::new(env!("CARGO_BIN_EXE_hmesh-mcp"))
        .arg("--data-dir")
        .arg(dir)
        .args(extra)
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
    let result = tokio::time::timeout(std::time::Duration::from_secs(20), child.wait_with_output())
        .await
        .unwrap()
        .unwrap();
    writer.await.unwrap();
    result
}
fn records(output: &std::process::Output) -> Vec<Value> {
    String::from_utf8(output.stdout.clone())
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str::<Value>(line).unwrap())
        .collect()
}
#[tokio::test]
async fn server_starts_before_the_daemon_and_stdout_carries_only_json_rpc() {
    let (_root, dir) = fixture(false);
    let mut input = String::from("invalid-json\n\n");
    input.push_str(&handshake());
    input.push_str(&format!(
        "{}\n",
        json!({"jsonrpc":"2.0","id":2,"method":"tools/list"})
    ));
    input.push_str(&format!("{}\n",json!({"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"mesh_list_agents","arguments":{}}})));
    let result = run(&dir, &[], input.as_bytes()).await;
    assert!(result.status.success());
    let records = records(&result);
    assert_eq!(records.len(), 4);
    assert_eq!(records[0]["error"]["code"], -32700);
    assert_eq!(records[2]["result"]["tools"].as_array().unwrap().len(), 6);
    let listed = &records[3]["result"];
    assert_eq!(listed["isError"], true);
    assert_eq!(
        listed["structuredContent"]["error"]["code"],
        "PEER_UNREACHABLE"
    );
    assert!(
        listed["structuredContent"]["error"]["message"]
            .as_str()
            .unwrap()
            .contains("hmesh daemon start")
    );
}
#[tokio::test]
async fn session_mode_refuses_to_start_without_an_open_session() {
    // HarnessMesh passes --session for the agents it runs; an unknown session
    // must never fall back to the unbound owner tools.
    let (_root, dir) = fixture(true);
    let session = Uuid::new_v4().to_string();
    let result = run(&dir, &["--session", &session], handshake().as_bytes()).await;
    assert!(!result.status.success());
    assert!(result.stdout.is_empty());
}
#[tokio::test]
async fn incomplete_and_oversized_stdio_frames_fail_without_echoing_input() {
    let (_root, dir) = fixture(true);
    for input in [
        b"{\"truncated\":".to_vec(),
        vec![b'x'; hmesh_mcp::MAX_FRAME_BYTES + 1],
    ] {
        let result = run(&dir, &[], &input).await;
        assert!(!result.status.success());
        assert!(result.stdout.is_empty());
        assert!(result.stderr.len() < 200);
    }
}
