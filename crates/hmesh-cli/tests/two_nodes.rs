//! Real OS processes and TLS. No model/API credentials and no mocked transport.
use hmesh_core::*;
use hmesh_security::Instance;
use hmesh_transport::Client;
use serde_json::{Value, json};
use std::{
    path::Path,
    process::{Child, Command, Stdio},
    time::Duration,
};

struct Daemon(Child);
impl Drop for Daemon {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

#[tokio::test]
async fn watch_drains_all_byte_bounded_event_pages_after_task_completion() {
    let root = tempfile::tempdir().unwrap();
    let dir = root.path().join("watch node");
    cli(&dir, &["init", "--name", "watch-node", "--port", "0"]);
    let instance = Instance::load(&dir).unwrap();
    let store = hmesh_store::Store::open(&dir.join("state.sqlite3")).unwrap();
    // Seed an explicitly synthetic finished task; this tests CLI replay, not a model.
    let envelope = Envelope::new(
        instance.card.node_id,
        "task.submit",
        TaskSubmit::new(uuid::Uuid::new_v4(), "fixture", "synthetic pagination"),
    );
    let task = store
        .admit(instance.card.node_id, &envelope, false, 20)
        .unwrap();
    store.claim_next().unwrap().unwrap();
    for index in 0..24 {
        store
            .append_event(
                task.task_id,
                "run",
                json!({"index":index,"text":"\u{0000}".repeat(10000)}),
            )
            .unwrap();
    }
    store
        .transition(
            task.task_id,
            TaskStatus::Failed,
            Some(MeshError::new(
                ErrorCode::AgentUnavailable,
                "synthetic pagination fixture",
            )),
        )
        .unwrap();
    let _daemon = daemon(&dir);
    ready(&dir).await;
    let output = tokio::time::timeout(
        Duration::from_secs(30),
        tokio::process::Command::new(env!("CARGO_BIN_EXE_hmesh"))
            .arg("--json")
            .arg("--data-dir")
            .arg(&dir)
            .args(["task", "watch", &task.task_id.to_string()])
            .output(),
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(
        output.status.code(),
        Some(5),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let records: Vec<Value> = String::from_utf8(output.stdout)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    let indices: Vec<_> = records
        .iter()
        .filter_map(|event| event.pointer("/payload/index").and_then(Value::as_u64))
        .collect();
    assert_eq!(indices, (0..24).collect::<Vec<_>>());
    assert_eq!(records.last().unwrap()["status"], "FAILED");
    let last_seq = records
        .iter()
        .filter_map(|row| row["seq"].as_i64())
        .max()
        .unwrap();
    drop(_daemon);
    std::fs::remove_file(dir.join("daemon.json")).unwrap();
    let _restarted = daemon(&dir);
    ready(&dir).await;
    let restored = cli(&dir, &["task", "show", &task.task_id.to_string()]);
    assert_eq!(restored["status"], "FAILED");
    let tail = cli(
        &dir,
        &[
            "task",
            "events",
            &task.task_id.to_string(),
            "--after",
            &last_seq.to_string(),
        ],
    );
    assert!(tail.as_array().unwrap().is_empty());
}

fn cli(dir: &Path, args: &[&str]) -> Value {
    let output = Command::new(env!("CARGO_BIN_EXE_hmesh"))
        .arg("--data-dir")
        .arg(dir)
        .arg("--json")
        .args(args)
        .output()
        .expect("CLI process");
    assert!(
        output.status.success(),
        "CLI failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).expect("JSON output")
}
fn daemon(dir: &Path) -> Daemon {
    let mut command = Command::new(env!("CARGO_BIN_EXE_hmesh"));
    command
        .arg("--data-dir")
        .arg(dir)
        .args(["daemon", "start"])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(0x08000000);
    }
    Daemon(command.spawn().expect("daemon process"))
}
async fn ready(dir: &Path) -> NodeCard {
    for _ in 0..100 {
        if let Ok(bytes) = std::fs::read(dir.join("daemon.json"))
            && let Ok(card) = serde_json::from_slice::<NodeCard>(&bytes)
        {
            return card;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    panic!("daemon did not publish its listener");
}

async fn rejected_start(dir: &Path) {
    let mut process = daemon(dir);
    for _ in 0..100 {
        if let Some(status) = process.0.try_wait().expect("observe rejected daemon") {
            assert!(!status.success(), "conflicting daemon must fail explicitly");
            return;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    panic!("conflicting daemon did not fail within its startup deadline");
}

#[tokio::test]
async fn two_daemon_processes_pair_bidirectionally_and_revoke_without_owner_leakage() {
    let root = tempfile::tempdir().expect("tempdir");
    let a_dir = root.path().join("node a");
    let b_dir = root.path().join("node b");
    cli(&a_dir, &["init", "--name", "node-a", "--port", "0"]);
    cli(&b_dir, &["init", "--name", "node-b", "--port", "0"]);
    let _a_daemon = daemon(&a_dir);
    let _b_daemon = daemon(&b_dir);
    let a_card = ready(&a_dir).await;
    let b_card = ready(&b_dir).await;
    rejected_start(&a_dir).await;
    let collision = root.path().join("port collision");
    let port = a_card.endpoint.rsplit(':').next().expect("loopback port");
    cli(&collision, &["init", "--name", "collision", "--port", port]);
    rejected_start(&collision).await;
    let mut a = Instance::load(&a_dir).expect("a");
    let b = Instance::load(&b_dir).expect("b");
    a.card = a_card.clone();
    assert_ne!(a_card.node_id, b_card.node_id);
    assert_ne!(a_card.fingerprint, b_card.fingerprint);
    let anonymous =
        Client::new(&a, &b_card.endpoint, &b_card.fingerprint, true, false).expect("anonymous");
    let error = anonymous
        .get::<Value>("/hmesh/v1/agents")
        .await
        .expect_err("unpaired request");
    assert_eq!(error.code, ErrorCode::Unauthenticated);
    let wrong = Client::new(&a, &b_card.endpoint, &"0".repeat(64), true, false).expect("wrong pin");
    assert!(wrong.get::<Value>("/hmesh/v1/hello").await.is_err());

    let request = cli(
        &a_dir,
        &[
            "pair",
            "request",
            "--peer",
            &b_card.endpoint,
            "--expect-fingerprint",
            &b_card.fingerprint,
        ],
    );
    let request_id = request["request_id"].as_str().expect("request id");
    cli(
        &b_dir,
        &[
            "pair",
            "approve",
            request_id,
            "--expect-fingerprint",
            &a_card.fingerprint,
        ],
    );
    cli(
        &a_dir,
        &[
            "pair",
            "approve",
            request_id,
            "--expect-fingerprint",
            &b_card.fingerprint,
        ],
    );
    let a_to_b = Client::new(&a, &b_card.endpoint, &b_card.fingerprint, false, false)
        .expect("paired client");
    let b_to_a = Client::new(&b, &a_card.endpoint, &a_card.fingerprint, false, false)
        .expect("paired client");
    assert_eq!(
        a_to_b
            .get::<NodeCard>("/hmesh/v1/node")
            .await
            .expect("a to b")
            .node_id,
        b_card.node_id
    );
    assert_eq!(
        b_to_a
            .get::<NodeCard>("/hmesh/v1/node")
            .await
            .expect("b to a")
            .node_id,
        a_card.node_id
    );
    assert_eq!(
        a_to_b
            .get::<Value>("/local/peers")
            .await
            .expect_err("owner-only")
            .code,
        ErrorCode::PermissionDenied
    );

    // Signed challenges are single use, independently of owner approval.
    let challenge: PairChallenge = anonymous
        .get("/hmesh/v1/pair/challenge")
        .await
        .expect("challenge");
    let proof = a.pair_submission(challenge).expect("proof");
    anonymous
        .post::<_, PairRecord>("/hmesh/v1/pair/requests", &proof)
        .await
        .expect("first request");
    assert_eq!(
        anonymous
            .post::<_, Value>("/hmesh/v1/pair/requests", &proof)
            .await
            .expect_err("replay")
            .code,
        ErrorCode::Unauthenticated
    );

    cli(&b_dir, &["pair", "revoke", &a_card.node_id.to_string()]);
    assert!(a_to_b.get::<NodeCard>("/hmesh/v1/node").await.is_err());
    let status = cli(&b_dir, &["daemon", "status"]);
    assert_eq!(status["tailscale"], json!("not_required"));
}

/// The status of one of `dir`'s pair requests, once it is `status` (or the
/// wait runs out).
async fn pair_status(dir: &Path, request_id: &str, status: &str) -> String {
    let mut seen = String::new();
    for _ in 0..40 {
        let pairs = cli(dir, &["pair", "list"]);
        seen = pairs
            .as_array()
            .unwrap()
            .iter()
            .find(|p| p["request_id"] == request_id)
            .map(|p| p["status"].as_str().unwrap().to_string())
            .unwrap_or_default();
        if seen == status {
            break;
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
    seen
}

#[tokio::test]
async fn one_approval_on_the_other_computer_pairs_both_ways() {
    let root = tempfile::tempdir().expect("tempdir");
    let dirs: Vec<_> = ["asks", "approves", "declined", "restarts"]
        .iter()
        .map(|name| root.path().join(name))
        .collect();
    for (dir, name) in dirs.iter().zip(["node-a", "node-b", "node-c", "node-d"]) {
        cli(dir, &["init", "--name", name, "--port", "0"]);
    }
    let (a_dir, b_dir, c_dir, d_dir) = (&dirs[0], &dirs[1], &dirs[2], &dirs[3]);
    let _a = daemon(a_dir);
    let _b = daemon(b_dir);
    let _c = daemon(c_dir);
    let mut d = daemon(d_dir);
    let a_card = ready(a_dir).await;
    let b_card = ready(b_dir).await;
    let c_card = ready(c_dir).await;
    ready(d_dir).await;
    let b_code = cli(b_dir, &["pair", "show"])["pair_code"]
        .as_str()
        .expect("pairing code")
        .to_string();
    assert_eq!(
        b_code,
        format!(
            "hmesh://{}/{}",
            b_card.endpoint.trim_start_matches("https://"),
            b_card.fingerprint
        )
    );

    // A pastes B's code. Until B's owner approves, neither side trusts the
    // other, and B reports the request as pending to whoever holds its ID.
    let request = cli(a_dir, &["pair", "request", "--code", &b_code]);
    let id = request["request_id"]
        .as_str()
        .expect("request id")
        .to_string();
    let mut a = Instance::load(a_dir).expect("a");
    a.card = a_card.clone();
    let b = Instance::load(b_dir).expect("b");
    let anonymous =
        Client::new(&a, &b_card.endpoint, &b_card.fingerprint, true, false).expect("anonymous");
    let pending: Value = anonymous
        .get(&format!("/hmesh/v1/pair/requests/{id}"))
        .await
        .expect("status");
    assert_eq!(pending["status"], "pending");
    assert_eq!(
        anonymous
            .get::<Value>(&format!("/hmesh/v1/pair/requests/{}", uuid::Uuid::new_v4()))
            .await
            .expect_err("unknown request")
            .code,
        ErrorCode::InvalidInput
    );
    tokio::time::sleep(Duration::from_secs(4)).await;
    assert_eq!(pair_status(a_dir, &id, "pending").await, "pending");
    let b_to_a =
        Client::new(&b, &a_card.endpoint, &a_card.fingerprint, false, false).expect("b to a");
    assert!(b_to_a.get::<NodeCard>("/hmesh/v1/node").await.is_err());

    // B approves once; A finishes on its own and both directions work.
    cli(
        b_dir,
        &[
            "pair",
            "approve",
            &id,
            "--expect-fingerprint",
            &a_card.fingerprint,
        ],
    );
    assert_eq!(pair_status(a_dir, &id, "approved").await, "approved");
    let a_to_b =
        Client::new(&a, &b_card.endpoint, &b_card.fingerprint, false, false).expect("a to b");
    assert_eq!(
        a_to_b
            .get::<NodeCard>("/hmesh/v1/node")
            .await
            .expect("a to b")
            .node_id,
        b_card.node_id
    );
    assert_eq!(
        b_to_a
            .get::<NodeCard>("/hmesh/v1/node")
            .await
            .expect("b to a")
            .node_id,
        a_card.node_id
    );
    // Approving by hand afterwards, as before this change, still works.
    let again = cli(
        a_dir,
        &[
            "pair",
            "approve",
            &id,
            "--expect-fingerprint",
            &b_card.fingerprint,
        ],
    );
    assert_eq!(again["status"], "trusted");
    // Revoking B ends A's open requests to it: B approving one of them
    // afterwards does not quietly make A trust B again.
    let late = cli(a_dir, &["pair", "request", "--code", &b_code]);
    let late = late["request_id"].as_str().unwrap().to_string();
    cli(a_dir, &["pair", "revoke", &b_card.node_id.to_string()]);
    assert_eq!(pair_status(a_dir, &late, "cancelled").await, "cancelled");
    cli(
        b_dir,
        &[
            "pair",
            "approve",
            &late,
            "--expect-fingerprint",
            &a_card.fingerprint,
        ],
    );
    tokio::time::sleep(Duration::from_secs(5)).await;
    assert_eq!(pair_status(a_dir, &late, "cancelled").await, "cancelled");
    assert!(b_to_a.get::<NodeCard>("/hmesh/v1/node").await.is_err());

    // B declines C: C learns it and trusts nothing.
    let declined = cli(c_dir, &["pair", "request", "--code", &b_code]);
    let declined = declined["request_id"].as_str().unwrap().to_string();
    cli(b_dir, &["pair", "decline", &declined]);
    assert_eq!(pair_status(c_dir, &declined, "declined").await, "declined");
    let b_to_c =
        Client::new(&b, &c_card.endpoint, &c_card.fingerprint, false, false).expect("b to c");
    assert!(b_to_c.get::<NodeCard>("/hmesh/v1/node").await.is_err());

    // C cancels its own request: even if B approves it afterwards, C does
    // not start trusting B.
    let cancelled = cli(c_dir, &["pair", "request", "--code", &b_code]);
    let cancelled = cancelled["request_id"].as_str().unwrap().to_string();
    cli(c_dir, &["pair", "decline", &cancelled]);
    cli(
        b_dir,
        &[
            "pair",
            "approve",
            &cancelled,
            "--expect-fingerprint",
            &c_card.fingerprint,
        ],
    );
    tokio::time::sleep(Duration::from_secs(5)).await;
    assert_eq!(
        pair_status(c_dir, &cancelled, "cancelled").await,
        "cancelled"
    );
    assert!(b_to_c.get::<NodeCard>("/hmesh/v1/node").await.is_err());

    // D asks, stops before B approves, and finishes after it starts again.
    let waiting = cli(d_dir, &["pair", "request", "--code", &b_code]);
    let waiting = waiting["request_id"].as_str().unwrap().to_string();
    let d_fingerprint = Instance::load(d_dir).unwrap().card.fingerprint;
    let _ = d.0.kill();
    let _ = d.0.wait();
    let _ = std::fs::remove_file(d_dir.join("daemon.json"));
    cli(
        b_dir,
        &[
            "pair",
            "approve",
            &waiting,
            "--expect-fingerprint",
            &d_fingerprint,
        ],
    );
    d = daemon(d_dir);
    ready(d_dir).await;
    assert_eq!(pair_status(d_dir, &waiting, "approved").await, "approved");
    drop(d);
}
