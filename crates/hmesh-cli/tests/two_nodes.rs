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

/// The stderr of a CLI call that must fail.
fn cli_fails(dir: &Path, args: &[&str]) -> String {
    let output = Command::new(env!("CARGO_BIN_EXE_hmesh"))
        .arg("--data-dir")
        .arg(dir)
        .arg("--json")
        .args(args)
        .output()
        .expect("CLI process");
    assert!(!output.status.success(), "CLI unexpectedly succeeded");
    String::from_utf8_lossy(&output.stderr).into_owned()
}
fn code_of(dir: &Path) -> String {
    cli(dir, &["pair", "show"])["pair_code"]
        .as_str()
        .expect("pairing code")
        .to_string()
}

#[tokio::test]
async fn a_computer_that_only_connects_out_pairs_and_calls_but_lets_nothing_in() {
    let root = tempfile::tempdir().expect("tempdir");
    let a_dir = root.path().join("calls out");
    let b_dir = root.path().join("shares");
    // The "only MCP" install: Tailscale network, nothing listening on it.
    // An explicit Tailnet address keeps Tailscale itself out of the test.
    cli(
        &a_dir,
        &[
            "init",
            "--name",
            "calls-out",
            "--port",
            "0",
            "--network",
            "tailscale",
            "--outbound-only",
            "--tailnet-address",
            "100.64.0.7",
        ],
    );
    cli(&b_dir, &["init", "--name", "shares", "--port", "0"]);
    let _a = daemon(&a_dir);
    let _b = daemon(&b_dir);
    let a_card = ready(&a_dir).await;
    let b_card = ready(&b_dir).await;
    assert!(hmesh_security::endpoint_is_loopback(&a_card.endpoint));
    assert_eq!(cli(&a_dir, &["daemon", "status"])["outbound_only"], true);
    assert_eq!(cli(&b_dir, &["daemon", "status"])["outbound_only"], false);

    // Nobody pairs into it...
    let refused = cli_fails(&b_dir, &["pair", "request", "--code", &code_of(&a_dir)]);
    assert!(refused.contains("PERMISSION_DENIED"), "{refused}");

    // ...but it pairs out, and the other side's one approval is enough.
    let request = cli(&a_dir, &["pair", "request", "--code", &code_of(&b_dir)]);
    let id = request["request_id"].as_str().unwrap().to_string();
    cli(
        &b_dir,
        &[
            "pair",
            "approve",
            &id,
            "--expect-fingerprint",
            &a_card.fingerprint,
        ],
    );
    assert_eq!(pair_status(&a_dir, &id, "approved").await, "approved");
    let mut a = Instance::load(&a_dir).expect("a");
    a.card = a_card.clone();
    let b = Instance::load(&b_dir).expect("b");
    let a_to_b =
        Client::new(&a, &b_card.endpoint, &b_card.fingerprint, false, false).expect("a to b");
    assert_eq!(
        a_to_b
            .get::<NodeCard>("/hmesh/v1/node")
            .await
            .expect("a reaches b")
            .node_id,
        b_card.node_id
    );
    let listed = cli(&a_dir, &["agents", "list", "--all"]);
    assert!(
        listed.to_string().contains(&b_card.node_id.to_string()),
        "{listed}"
    );

    // Not even the computer it paired with gets in.
    let b_to_a =
        Client::new(&b, &a_card.endpoint, &a_card.fingerprint, false, false).expect("b to a");
    assert_eq!(
        b_to_a
            .get::<NodeCard>("/hmesh/v1/node")
            .await
            .unwrap_err()
            .code,
        ErrorCode::PermissionDenied
    );
    let anonymous =
        Client::new(&b, &a_card.endpoint, &a_card.fingerprint, true, false).expect("anonymous");
    assert_eq!(
        anonymous
            .get::<Value>("/hmesh/v1/hello")
            .await
            .unwrap_err()
            .code,
        ErrorCode::PermissionDenied
    );
    // Its owner still drives it.
    assert_eq!(cli(&a_dir, &["peers", "list"]).as_array().unwrap().len(), 1);
}

#[tokio::test]
async fn a_computer_that_moved_tells_the_computers_it_paired_with() {
    let root = tempfile::tempdir().expect("tempdir");
    let a_dir = root.path().join("stays");
    let b_dir = root.path().join("moves");
    cli(&a_dir, &["init", "--name", "stays", "--port", "0"]);
    cli(&b_dir, &["init", "--name", "moves", "--port", "0"]);
    let _a = daemon(&a_dir);
    let mut b = daemon(&b_dir);
    let a_card = ready(&a_dir).await;
    let old = ready(&b_dir).await;
    let request = cli(&a_dir, &["pair", "request", "--code", &code_of(&b_dir)]);
    let id = request["request_id"].as_str().unwrap().to_string();
    cli(
        &b_dir,
        &[
            "pair",
            "approve",
            &id,
            "--expect-fingerprint",
            &a_card.fingerprint,
        ],
    );
    assert_eq!(pair_status(&a_dir, &id, "approved").await, "approved");

    // B comes back on another port (port 0 picks a new one).
    let _ = b.0.kill();
    let _ = b.0.wait();
    let _ = std::fs::remove_file(b_dir.join("daemon.json"));
    b = daemon(&b_dir);
    let moved = ready(&b_dir).await;
    assert_ne!(moved.endpoint, old.endpoint);
    let mut known = String::new();
    for _ in 0..40 {
        known = cli(&a_dir, &["peers", "list"])[0]["card"]["endpoint"]
            .as_str()
            .unwrap_or_default()
            .to_string();
        if known == moved.endpoint {
            break;
        }
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
    assert_eq!(known, moved.endpoint, "A still has B's old address");
    let mut a = Instance::load(&a_dir).expect("a");
    a.card = a_card;
    let a_to_b = Client::new(&a, &known, &moved.fingerprint, false, false).expect("a to b");
    assert_eq!(
        a_to_b
            .get::<NodeCard>("/hmesh/v1/node")
            .await
            .expect("a reaches b")
            .node_id,
        moved.node_id
    );
    drop(b);
}

/// A direct network (a LAN, ZeroTier, WireGuard...): the pairing handshake
/// answers only while the owner has pairing open, and paired computers then
/// talk as on any other network.
#[tokio::test]
async fn a_direct_computer_answers_pairing_only_while_its_owner_has_it_open() {
    let root = tempfile::tempdir().expect("tempdir");
    let a_dir = root.path().join("laptop");
    let b_dir = root.path().join("studio");
    // Loopback stands in for a LAN address; the rules are the same. A fixed
    // port, so the studio is found again after it restarts.
    let port = std::net::TcpListener::bind("127.0.0.1:0")
        .expect("free port")
        .local_addr()
        .expect("port")
        .port()
        .to_string();
    cli(
        &b_dir,
        &[
            "init",
            "--name",
            "studio",
            "--port",
            &port,
            "--network",
            "direct",
            "--listen-address",
            "127.0.0.1",
        ],
    );
    cli(
        &a_dir,
        &[
            "init",
            "--name",
            "laptop",
            "--port",
            "0",
            "--network",
            "direct",
            "--outbound-only",
        ],
    );
    let _a = daemon(&a_dir);
    let b = daemon(&b_dir);
    let a_card = ready(&a_dir).await;
    let b_card = ready(&b_dir).await;
    let status = cli(&b_dir, &["daemon", "status"]);
    assert_eq!(status["network"], "direct");
    assert_eq!(status["pairing_window"]["required"], true);
    assert!(status["pairing_window"]["closes_at"].is_null(), "{status}");
    assert!(
        status["allowed_sources"]
            .as_array()
            .is_some_and(|s| !s.is_empty())
    );

    // Closed: neither a pair request nor an anonymous hello is answered.
    let code = hmesh_security::pair_code(&b_card);
    let refused = cli_fails(&a_dir, &["pair", "request", "--code", &code]);
    assert!(refused.contains("pairing is closed"), "{refused}");
    let a = Instance::load(&a_dir).expect("a");
    let anonymous =
        Client::new(&a, &b_card.endpoint, &b_card.fingerprint, true, false).expect("anonymous");
    assert_eq!(
        anonymous
            .get::<Value>("/hmesh/v1/hello")
            .await
            .unwrap_err()
            .code,
        ErrorCode::PermissionDenied
    );
    // Nor the other two steps, asked for directly.
    assert_eq!(
        anonymous
            .get::<Value>("/hmesh/v1/pair/challenge")
            .await
            .unwrap_err()
            .code,
        ErrorCode::PermissionDenied
    );
    let mut asker = Instance::load(&a_dir).expect("a");
    asker.card = a_card.clone();
    let submission = asker
        .pair_submission(PairChallenge {
            nonce: uuid::Uuid::new_v4(),
            node_id: b_card.node_id,
            expires_at: 0,
        })
        .expect("submission");
    assert_eq!(
        anonymous
            .post::<_, Value>("/hmesh/v1/pair/requests", &submission)
            .await
            .unwrap_err()
            .code,
        ErrorCode::PermissionDenied
    );

    // Its owner opens it (the panel's 配对新电脑 does the same).
    let shown = cli(&b_dir, &["pair", "show"]);
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("clock")
        .as_secs() as i64;
    assert!(
        shown["pairing_window"]["closes_at"]
            .as_i64()
            .is_some_and(|t| t > now),
        "{shown}"
    );
    let request = cli(&a_dir, &["pair", "request", "--code", &code]);
    let id = request["request_id"].as_str().unwrap().to_string();
    cli(
        &b_dir,
        &[
            "pair",
            "approve",
            &id,
            "--expect-fingerprint",
            &a_card.fingerprint,
        ],
    );
    assert_eq!(pair_status(&a_dir, &id, "approved").await, "approved");
    let mut a = Instance::load(&a_dir).expect("a");
    a.card = a_card;
    let a_to_b =
        Client::new(&a, &b_card.endpoint, &b_card.fingerprint, false, false).expect("a to b");
    assert_eq!(
        a_to_b
            .get::<NodeCard>("/hmesh/v1/node")
            .await
            .expect("a reaches b")
            .node_id,
        b_card.node_id
    );

    // Restarted, the studio's pairing window is closed: the laptop still
    // sees it online and reads its agents, by its certificate.
    drop(b);
    let _ = std::fs::remove_file(b_dir.join("daemon.json"));
    let _b = daemon(&b_dir);
    ready(&b_dir).await;
    assert!(cli(&b_dir, &["daemon", "status"])["pairing_window"]["closes_at"].is_null());
    let peers = cli(&a_dir, &["peers", "list"]);
    assert_eq!(peers[0]["node_connectivity"], "online", "{peers}");
    assert_eq!(peers[0]["registry_readable"], true, "{peers}");
}

/// A direct node set up for an address this computer no longer has (a new
/// DHCP lease) says so instead of failing to bind.
#[test]
fn a_direct_computer_whose_address_is_gone_says_how_to_fix_it() {
    let root = tempfile::tempdir().expect("tempdir");
    let dir = root.path().join("moved");
    // 192.0.2.0/24 is reserved for documentation; no computer has it.
    cli(
        &dir,
        &[
            "init",
            "--name",
            "moved",
            "--port",
            "0",
            "--network",
            "direct",
            "--listen-address",
            "192.0.2.10",
            "--allow-from",
            "192.0.2.0/24",
        ],
    );
    let output = Command::new(env!("CARGO_BIN_EXE_hmesh"))
        .arg("--data-dir")
        .arg(&dir)
        .args(["daemon", "start"])
        .output()
        .expect("daemon process");
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("no longer has the address 192.0.2.10"),
        "{stderr}"
    );
    assert!(stderr.contains("hmesh setup"), "{stderr}");
}

/// Two direct computers on one machine: the studio takes connections (on
/// loopback, standing in for a LAN address, at a fixed port) and the laptop
/// only connects out.
async fn direct_pair(root: &Path) -> (std::path::PathBuf, std::path::PathBuf, String) {
    let a_dir = root.join("laptop");
    let b_dir = root.join("studio");
    let port = std::net::TcpListener::bind("127.0.0.1:0")
        .expect("free port")
        .local_addr()
        .expect("port")
        .port()
        .to_string();
    cli(
        &b_dir,
        &[
            "init",
            "--name",
            "studio",
            "--port",
            &port,
            "--network",
            "direct",
            "--listen-address",
            "127.0.0.1",
        ],
    );
    cli(
        &a_dir,
        &[
            "init",
            "--name",
            "laptop",
            "--port",
            "0",
            "--network",
            "direct",
            "--outbound-only",
        ],
    );
    (a_dir, b_dir, format!("127.0.0.1:{port}"))
}
fn trusted(dir: &Path) -> Vec<String> {
    cli(dir, &["peers", "list"])
        .as_array()
        .unwrap()
        .iter()
        .filter(|p| p["status"] == "trusted")
        .map(|p| p["card"]["display_name"].as_str().unwrap().to_owned())
        .collect()
}

/// Pairing with the 6-digit passcode the other computer shows: a wrong one
/// trusts nobody, the right one pairs both ways with no approval, and it
/// works once.
#[tokio::test]
async fn a_computer_pairs_with_the_passcode_the_other_shows_and_nobody_approves() {
    let root = tempfile::tempdir().expect("tempdir");
    let (a_dir, b_dir, address) = direct_pair(root.path()).await;
    let _a = daemon(&a_dir);
    let _b = daemon(&b_dir);
    let a_card = ready(&a_dir).await;
    let b_card = ready(&b_dir).await;

    // Pairing closed: there is no passcode to try, even asked directly.
    let refused = cli_fails(&a_dir, &["pair", "join", &address, "--passcode", "123456"]);
    assert!(refused.contains("pairing is closed"), "{refused}");
    let mut asker = Instance::load(&a_dir).expect("a");
    asker.card = a_card.clone();
    let submission = asker
        .pair_submission(PairChallenge {
            nonce: uuid::Uuid::new_v4(),
            node_id: b_card.node_id,
            expires_at: 0,
        })
        .expect("submission");
    let (_, message) = hmesh_security::passcode::Asker::start(
        "123456",
        &a_card.fingerprint,
        &b_card.fingerprint,
        submission.request_id,
    )
    .expect("asker");
    let anonymous =
        Client::new(&asker, &b_card.endpoint, &b_card.fingerprint, true, false).expect("anonymous");
    assert_eq!(
        anonymous
            .post::<_, Value>(
                "/hmesh/v1/pair/passcode",
                &hmesh_daemon::passcode::PasscodeStart {
                    submission,
                    message
                },
            )
            .await
            .unwrap_err()
            .code,
        ErrorCode::PermissionDenied
    );

    let shown = cli(&b_dir, &["pair", "show"]);
    let passcode = shown["pairing_window"]["passcode"]
        .as_str()
        .unwrap_or_else(|| panic!("no passcode: {shown}"))
        .to_owned();
    assert!(passcode.len() == 6 && passcode.bytes().all(|b| b.is_ascii_digit()));
    assert_eq!(shown["pairing_window"]["tries_left"], 3);
    let wrong = if passcode == "000000" {
        "000001"
    } else {
        "000000"
    };

    // A wrong passcode: refused, and the studio trusts nobody.
    let refused = cli_fails(&a_dir, &["pair", "join", &address, "--passcode", wrong]);
    assert!(refused.contains("passcode did not match"), "{refused}");
    assert!(trusted(&b_dir).is_empty());
    assert!(trusted(&a_dir).is_empty());
    let status = cli(&b_dir, &["daemon", "status"]);
    assert_eq!(status["pairing_window"]["tries_left"], 2, "{status}");

    // The right one: both trust each other, nobody approved anything.
    // Doctor output (shared to get help) never carries the passcode.
    let doctor = cli(&b_dir, &["doctor"]);
    assert_eq!(
        doctor["daemon"]["status"]["pairing_window"]["passcode"], "hidden",
        "{doctor}"
    );
    assert!(!doctor.to_string().contains(&passcode));
    // Typed when asked, not on the command line (out of the shell history).
    let output = {
        use std::io::Write;
        let mut child = Command::new(env!("CARGO_BIN_EXE_hmesh"))
            .arg("--data-dir")
            .arg(&a_dir)
            .args(["--json", "pair", "join", &address])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("CLI process");
        writeln!(child.stdin.take().expect("stdin"), "{passcode}").expect("type passcode");
        child.wait_with_output().expect("CLI output")
    };
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let joined: Value = serde_json::from_slice(&output.stdout).expect("json");
    assert_eq!(joined["status"], "approved", "{joined}");
    assert_eq!(trusted(&a_dir), ["studio"]);
    assert_eq!(trusted(&b_dir), ["laptop"]);
    let mut a = Instance::load(&a_dir).expect("a");
    a.card = a_card;
    let a_to_b =
        Client::new(&a, &b_card.endpoint, &b_card.fingerprint, false, false).expect("a to b");
    assert_eq!(
        a_to_b
            .get::<NodeCard>("/hmesh/v1/node")
            .await
            .expect("a reaches b")
            .node_id,
        b_card.node_id
    );

    // Used up: no passcode now, and the same one does not pair again.
    let status = cli(&b_dir, &["daemon", "status"]);
    assert!(status["pairing_window"]["passcode"].is_null(), "{status}");
    assert_eq!(status["pairing_window"]["passcode_used"], true, "{status}");
    let again = cli_fails(&a_dir, &["pair", "join", &address, "--passcode", &passcode]);
    assert!(again.contains("no passcode open"), "{again}");
}

/// Three wrong passcodes end the passcode, not pairing: the pairing code
/// still works, so guessing cannot shut the owner out.
#[tokio::test]
async fn three_wrong_passcodes_end_the_passcode_and_pairing_codes_still_work() {
    let root = tempfile::tempdir().expect("tempdir");
    let (a_dir, b_dir, address) = direct_pair(root.path()).await;
    let _a = daemon(&a_dir);
    let _b = daemon(&b_dir);
    ready(&a_dir).await;
    ready(&b_dir).await;
    let passcode = cli(&b_dir, &["pair", "show"])["pairing_window"]["passcode"]
        .as_str()
        .expect("passcode")
        .to_owned();
    let wrong = if passcode == "000000" {
        "000001"
    } else {
        "000000"
    };
    for _ in 0..3 {
        let refused = cli_fails(&a_dir, &["pair", "join", &address, "--passcode", wrong]);
        assert!(refused.contains("passcode did not match"), "{refused}");
    }
    let status = cli(&b_dir, &["daemon", "status"]);
    let window = &status["pairing_window"];
    assert!(window["closes_at"].is_i64(), "{status}");
    assert!(
        window["passcode"].is_null() && window["passcode_used"] == false,
        "{status}"
    );
    // Even the right passcode is too late now.
    let refused = cli_fails(&a_dir, &["pair", "join", &address, "--passcode", &passcode]);
    assert!(refused.contains("no passcode open"), "{refused}");
    assert!(trusted(&b_dir).is_empty());
    // The pairing code still sends a request.
    let code = cli(&b_dir, &["pair", "show"])["pair_code"]
        .as_str()
        .unwrap()
        .to_owned();
    let request = cli(&a_dir, &["pair", "request", "--code", &code]);
    assert_eq!(request["status"], "pending", "{request}");
}

/// A try is spent when a handshake starts: an asker that never sends its
/// confirmation still gets three guesses at most.
#[tokio::test]
async fn handshakes_never_confirmed_still_use_up_the_tries() {
    let root = tempfile::tempdir().expect("tempdir");
    let (a_dir, b_dir, _) = direct_pair(root.path()).await;
    let _a = daemon(&a_dir);
    let _b = daemon(&b_dir);
    let a_card = ready(&a_dir).await;
    let b_card = ready(&b_dir).await;
    cli(&b_dir, &["pair", "show"]);
    let mut asker = Instance::load(&a_dir).expect("a");
    asker.card = a_card.clone();
    let anonymous =
        Client::new(&asker, &b_card.endpoint, &b_card.fingerprint, true, false).expect("anonymous");
    let mut answers = vec![];
    for guess in ["111111", "222222", "333333", "444444"] {
        let challenge: PairChallenge = anonymous
            .get("/hmesh/v1/pair/challenge")
            .await
            .expect("challenge");
        let submission = asker.pair_submission(challenge).expect("submission");
        let (_, message) = hmesh_security::passcode::Asker::start(
            guess,
            &a_card.fingerprint,
            &b_card.fingerprint,
            submission.request_id,
        )
        .expect("asker");
        answers.push(
            anonymous
                .post::<_, Value>(
                    "/hmesh/v1/pair/passcode",
                    &hmesh_daemon::passcode::PasscodeStart {
                        submission,
                        message,
                    },
                )
                .await
                .map(|_| ())
                .map_err(|e| e.code),
        );
    }
    assert_eq!(
        answers,
        [Ok(()), Ok(()), Ok(()), Err(ErrorCode::PermissionDenied)]
    );
    assert_eq!(
        cli(&b_dir, &["daemon", "status"])["pairing_window"]["tries_left"],
        0
    );
}

/// A computer with pairing open is heard nearby, and no longer once its
/// passcode is used. Multicast on loopback works on macOS; other systems
/// may not route it there.
#[cfg(target_os = "macos")]
#[tokio::test]
async fn a_computer_with_pairing_open_is_heard_nearby_until_its_passcode_is_used() {
    let root = tempfile::tempdir().expect("tempdir");
    let (a_dir, b_dir, address) = direct_pair(root.path()).await;
    let _a = daemon(&a_dir);
    let _b = daemon(&b_dir);
    ready(&a_dir).await;
    ready(&b_dir).await;
    let heard = |dir: &Path| -> Vec<String> {
        cli(dir, &["pair", "nearby"])["computers"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|c| c["name"] == "studio")
            .map(|c| c["endpoint"].as_str().unwrap().to_owned())
            .collect()
    };
    // Closed: not announced.
    assert!(heard(&a_dir).is_empty());
    let passcode = cli(&b_dir, &["pair", "show"])["pairing_window"]["passcode"]
        .as_str()
        .expect("passcode")
        .to_owned();
    let mut found = vec![];
    for _ in 0..5 {
        found = heard(&a_dir);
        if !found.is_empty() {
            break;
        }
    }
    assert_eq!(found, [format!("https://{address}")]);
    cli(&a_dir, &["pair", "join", &address, "--passcode", &passcode]);
    let mut gone = false;
    for _ in 0..5 {
        if heard(&a_dir).is_empty() {
            gone = true;
            break;
        }
    }
    assert!(gone, "still announced after its passcode was used");
}

/// Edit `dir`'s node.toml as its owner would, while its daemon is stopped.
fn edit_node(dir: &Path, change: impl FnOnce(&mut toml::Table)) {
    let path = dir.join("node.toml");
    let mut config: toml::Table =
        toml::from_str(&std::fs::read_to_string(&path).expect("node.toml")).expect("TOML");
    change(
        config
            .get_mut("node")
            .and_then(toml::Value::as_table_mut)
            .expect("[node]"),
    );
    std::fs::write(&path, toml::to_string_pretty(&config).expect("TOML")).expect("write");
}
fn stop(daemon: Daemon, dir: &Path) {
    drop(daemon);
    let _ = std::fs::remove_file(dir.join("daemon.json"));
}
fn free_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0")
        .expect("free port")
        .local_addr()
        .expect("port")
        .port()
}
/// A loopback address where nothing comes back, like an address on a
/// network the computer has left: a listener whose one-place queue is full
/// and never taken, so a new connection's first packet is dropped. Keep the
/// second value for as long as the address should stay silent.
async fn silent_address() -> (
    String,
    (tokio::net::TcpListener, Vec<tokio::net::TcpStream>),
) {
    let socket = tokio::net::TcpSocket::new_v4().expect("socket");
    socket
        .bind("127.0.0.1:0".parse().expect("address"))
        .expect("bind");
    let listener = socket.listen(1).expect("listen");
    let address = listener.local_addr().expect("address");
    let mut queued = Vec::new();
    for _ in 0..64 {
        match tokio::time::timeout(
            Duration::from_millis(300),
            tokio::net::TcpStream::connect(address),
        )
        .await
        {
            Ok(Ok(stream)) => queued.push(stream),
            _ => return (format!("https://{address}"), (listener, queued)),
        }
    }
    panic!("the queue at {address} never filled");
}
/// Pair `a_dir` with the computer at `address` using the passcode `b_dir` shows.
fn join(a_dir: &Path, b_dir: &Path, address: &str) {
    let passcode = cli(b_dir, &["pair", "show"])["pairing_window"]["passcode"]
        .as_str()
        .expect("passcode")
        .to_owned();
    let joined = cli(a_dir, &["pair", "join", address, "--passcode", &passcode]);
    assert_eq!(joined["status"], "approved", "{joined}");
}
/// The one paired computer as `dir` lists it, once `ready` says so (or the
/// wait runs out).
async fn peer_until(dir: &Path, ready: impl Fn(&Value) -> bool) -> Value {
    let mut peer = Value::Null;
    for _ in 0..40 {
        peer = cli(dir, &["peers", "list"])[0].clone();
        if ready(&peer) {
            break;
        }
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
    peer
}

/// A paired computer learns the other's addresses, host names among them,
/// and finds it by name when the address on its card stops answering. The
/// studio moves from 127.0.0.1 to ::1 where `localhost` names both (macOS,
/// Windows); where it names 127.0.0.1 only (some Linux), the other way.
#[tokio::test]
async fn a_computer_finds_a_peer_that_moved_by_its_host_name() {
    use std::net::{Ipv6Addr, ToSocketAddrs};
    if std::net::TcpListener::bind((Ipv6Addr::LOCALHOST, 0)).is_err() {
        eprintln!("skipped: this computer has no IPv6 loopback");
        return;
    }
    let names_v6 = ("localhost", 1)
        .to_socket_addrs()
        .is_ok_and(|mut found| found.any(|a| a.ip() == Ipv6Addr::LOCALHOST));
    let (first, then) = if names_v6 {
        ("127.0.0.1", "::1")
    } else {
        ("::1", "127.0.0.1")
    };
    let authority = |ip: &str| {
        if ip.contains(':') {
            format!("[{ip}]")
        } else {
            ip.to_owned()
        }
    };
    let root = tempfile::tempdir().expect("tempdir");
    let (a_dir, b_dir, address) = direct_pair(root.path()).await;
    let port = address.rsplit(':').next().expect("port").to_owned();
    let address = format!("{}:{port}", authority(first));
    edit_node(&b_dir, |node| {
        node.insert("listen_address".into(), first.into());
        node.insert(
            "host_names".into(),
            toml::Value::Array(vec!["localhost".into()]),
        );
    });
    let a = daemon(&a_dir);
    let b = daemon(&b_dir);
    ready(&a_dir).await;
    let b_card = ready(&b_dir).await;
    assert_eq!(b_card.endpoint, format!("https://{address}"));
    join(&a_dir, &b_dir, &address);
    // The laptop only connects out: it asks the studio for its addresses.
    let by_name = format!("https://localhost:{port}");
    let peer = peer_until(&a_dir, |p| {
        p["routes"]["advertised"]
            .as_array()
            .is_some_and(|a| a.contains(&json!(by_name)))
    })
    .await;
    assert_eq!(
        peer["routes"]["advertised"],
        json!([b_card.endpoint, by_name]),
        "{peer}"
    );

    // While the laptop is off, the studio moves to another address (same
    // port). Nothing listens at the old one any more.
    stop(a, &a_dir);
    stop(b, &b_dir);
    edit_node(&b_dir, |node| {
        node.insert("listen_address".into(), then.into());
    });
    let _b = daemon(&b_dir);
    let moved = ready(&b_dir).await;
    assert_eq!(
        moved.endpoint,
        format!("https://{}:{port}", authority(then))
    );
    assert!(std::net::TcpStream::connect(&address).is_err());
    let _a = daemon(&a_dir);
    ready(&a_dir).await;
    // Called at once: the old address refuses, localhost answers at the new.
    let agents = cli(&a_dir, &["agents", "--peer", &b_card.node_id.to_string()]);
    assert!(agents.is_array(), "{agents}");
    let peer = &cli(&a_dir, &["peers", "list"])[0];
    assert_eq!(peer["card"]["endpoint"], b_card.endpoint, "{peer}");
    assert_eq!(peer["node_connectivity"], "online", "{peer}");
    assert_eq!(peer["registry_readable"], true, "{peer}");
    assert_eq!(peer["routes"]["last_good"], by_name, "{peer}");
    assert!(peer["routes"]["last_good_at"].is_i64(), "{peer}");
}

/// When no address the laptop knows answers, one set by hand does; and an
/// address where another computer answers is passed over.
#[tokio::test]
async fn a_peer_is_reached_at_an_address_set_by_hand_and_never_at_another_computer() {
    let root = tempfile::tempdir().expect("tempdir");
    let (a_dir, b_dir, address) = direct_pair(root.path()).await;
    edit_node(&b_dir, |node| {
        node.insert("host_names".into(), toml::Value::Array(vec![]));
    });
    let a = daemon(&a_dir);
    let b = daemon(&b_dir);
    ready(&a_dir).await;
    let b_card = ready(&b_dir).await;
    join(&a_dir, &b_dir, &address);
    let b_id = b_card.node_id.to_string();

    // The studio comes back on another port; the laptop cannot be told.
    stop(b, &b_dir);
    let port = free_port();
    edit_node(&b_dir, |node| {
        node.insert("listen_port".into(), toml::Value::Integer(port.into()));
    });
    let _b = daemon(&b_dir);
    let moved = ready(&b_dir).await;
    assert_eq!(moved.endpoint, format!("https://127.0.0.1:{port}"));
    let refused = cli_fails(&a_dir, &["agents", "--peer", &b_id]);
    assert!(refused.contains("refused the connection"), "{refused}");

    // Its owner types the new address (with or without https://).
    let set = cli(
        &a_dir,
        &["peers", "address", "studio", &format!("127.0.0.1:{port}")],
    );
    assert_eq!(set["manual"], json!([moved.endpoint]), "{set}");
    assert!(cli(&a_dir, &["agents", "--peer", &b_id]).is_array());
    let peer = &cli(&a_dir, &["peers", "list"])[0];
    assert_eq!(peer["routes"]["manual"], json!([moved.endpoint]), "{peer}");
    assert_eq!(peer["routes"]["last_good"], moved.endpoint, "{peer}");
    assert_eq!(peer["registry_readable"], true, "{peer}");

    // Another computer answers at the address tried first: its certificate
    // is not the studio's, so it is passed over and the next one is used.
    let c_dir = root.path().join("other");
    cli(&c_dir, &["init", "--name", "other", "--port", "0"]);
    let _c = daemon(&c_dir);
    let c_card = ready(&c_dir).await;
    stop(a, &a_dir);
    {
        let store = hmesh_store::Store::open(&a_dir.join("state.sqlite3")).expect("store");
        store
            .remember_route(&b_card.fingerprint, &c_card.endpoint, now())
            .expect("route");
        assert_eq!(
            store.peer_routes(b_card.node_id).expect("routes").last_good,
            Some(c_card.endpoint.clone())
        );
    }
    let a = daemon(&a_dir);
    ready(&a_dir).await;
    assert!(cli(&a_dir, &["agents", "--peer", &b_id]).is_array());
    let peer = &cli(&a_dir, &["peers", "list"])[0];
    assert_eq!(peer["routes"]["last_good"], moved.endpoint, "{peer}");
    assert_eq!(peer["registry_readable"], true, "{peer}");

    // The address tried first sends nothing back (where the studio was
    // reached last is a network the laptop left): the next one answers
    // within the panel's wait, so the studio shows online with its agents.
    let (quiet, _queue) = silent_address().await;
    stop(a, &a_dir);
    hmesh_store::Store::open(&a_dir.join("state.sqlite3"))
        .expect("store")
        .remember_route(&b_card.fingerprint, &quiet, now())
        .expect("route");
    let _a = daemon(&a_dir);
    ready(&a_dir).await;
    let peer = &cli(&a_dir, &["peers", "list"])[0];
    assert_eq!(peer["node_connectivity"], "online", "{peer}");
    assert_eq!(peer["registry_readable"], true, "{peer}");
    assert_eq!(peer["routes"]["last_good"], moved.endpoint, "{peer}");

    // Cleared by hand: only what the studio said remains.
    let cleared = cli(&a_dir, &["peers", "address", &b_id, "--clear"]);
    assert_eq!(cleared["manual"], json!([]), "{cleared}");
    let refused = cli_fails(
        &a_dir,
        &["peers", "address", "studio", "https://user@host:1"],
    );
    assert!(refused.contains("credentials"), "{refused}");
}
