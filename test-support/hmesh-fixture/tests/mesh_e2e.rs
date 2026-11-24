//! Two real daemons on loopback mTLS, the stock MCP server and the fixture ACP
//! agent. Exercises sessions across computers: discovery, multi-turn
//! conversations, permission routing, agent-to-agent delegation and its
//! limits, per-peer access and revocation. Never calls a model.
use hmesh_core::sessions::*;
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
/// A fixture ACP agent that passed its initialize check.
fn register_fixture(instance: &Instance, project: &Path) -> Uuid {
    std::fs::create_dir_all(project).unwrap();
    let config: AgentConfig = serde_json::from_value(json!({
        "agent":{"name":"fixture","harness":"acp","project_id":"demo"},
        "project":{"root":project,"allowed_tools":["read","write","edit"]},
        "adapter":{"executable":env!("CARGO_BIN_EXE_hmesh-fixture"),"provider":"fixture","model":"fixture"}
    }))
    .unwrap();
    let agent_id = Uuid::new_v4();
    let card: AgentCard = serde_json::from_value(json!({
        "agent_id":agent_id,"node_id":instance.card.node_id,"name":"fixture","harness":"acp","project_ids":["demo"],"availability":"idle","capabilities":[],
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

type Owner = hmesh_transport::Client;

async fn open(owner: &Owner, body: Value) -> SessionRecord {
    owner.post("/local/sessions", &body).await.unwrap()
}
async fn post(owner: &Owner, path: &str) -> SessionRecord {
    owner.post(path, &json!({})).await.unwrap()
}
async fn page(owner: &Owner, id: Uuid, after: u64, wait: u64) -> SessionEventPage {
    owner
        .get(&format!(
            "/local/sessions/{id}/events?after={after}&wait={wait}"
        ))
        .await
        .unwrap()
}
/// Follow a session until `done` holds for it; returns its events so far.
async fn until(
    owner: &Owner,
    id: Uuid,
    done: impl Fn(&SessionRecord, &[SessionEvent]) -> bool,
) -> (SessionRecord, Vec<SessionEvent>) {
    let deadline = Instant::now() + Duration::from_secs(90);
    let mut events = vec![];
    let mut after = 0;
    loop {
        let next = page(owner, id, after, 5).await;
        after = next.next;
        events.extend(next.events);
        if done(&next.session, &events) {
            return (next.session, events);
        }
        assert!(
            Instant::now() < deadline,
            "session {id} stuck: {:?} {events:#?}",
            next.session.status
        );
    }
}
async fn turn_done(owner: &Owner, id: Uuid, turns: u64) -> (SessionRecord, Vec<SessionEvent>) {
    until(owner, id, |s, _| {
        s.turns >= turns && !s.status.is_busy() || s.status == SessionStatus::Closed
    })
    .await
}
fn kinds(events: &[SessionEvent]) -> Vec<&str> {
    events.iter().map(|e| e.kind.as_str()).collect()
}

#[tokio::test(flavor = "multi_thread")]
async fn sessions_span_paired_computers_with_delegation_permissions_and_access_rules() {
    // The daemons run in this process, so the injected `hmesh mcp` server is
    // the fixture binary's stand-in mode.
    unsafe { std::env::set_var("HMESH_MCP_COMMAND", env!("CARGO_BIN_EXE_hmesh-fixture")) };
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
    let local_agent = register_fixture(&a, &root.path().join("project-a"));
    let remote_agent = register_fixture(&b, &root.path().join("project-b"));
    let daemon_a = Daemon::start(&a.dir);
    let daemon_b = Daemon::start(&b.dir);
    daemon_a.ready().await;
    daemon_b.ready().await;
    let owner_a = daemon_a.owner();
    let owner_b = daemon_b.owner();
    let mut mcp = initialized(&a.dir).await;

    // Pairing alone makes node-b's agents visible.
    let listed = call(&mut mcp, "mesh_list_agents", json!({})).await;
    let refs: Vec<&str> = listed["agents"]
        .as_array()
        .unwrap()
        .iter()
        .map(|agent| agent["ref"].as_str().unwrap())
        .collect();
    assert_eq!(refs, ["node-a/fixture", "node-b/fixture"]);
    assert_eq!(listed["agents"][1]["ready"], true, "{listed}");

    // An MCP client opens a session on the paired computer and keeps talking
    // in it: the agent remembers the conversation (turn numbers grow).
    let first = call(
        &mut mcp,
        "mesh_send",
        json!({"agent":"node-b/fixture","message":"fixture:reply","wait_seconds":60}),
    )
    .await;
    assert_eq!(first["done"], true, "{first}");
    assert_eq!(first["final_text"], "Final answer: the build is green.");
    assert_eq!(first["agent"], "node-b/fixture");
    let remote_session: Uuid = first["session_id"].as_str().unwrap().parse().unwrap();
    let second = call(
        &mut mcp,
        "mesh_send",
        json!({"session_id":remote_session,"message":"fixture:echo again","wait_seconds":60}),
    )
    .await;
    assert_eq!(second["final_text"], "Echo #2: again", "{second}");
    assert_eq!(second["turns"], 2);
    let progress = call(
        &mut mcp,
        "mesh_get_session",
        json!({"session_id":remote_session}),
    )
    .await;
    let seen: Vec<&str> = progress["events"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["kind"].as_str().unwrap())
        .collect();
    for kind in [
        "user_message",
        "agent_message_chunk",
        "tool_call",
        "turn_end",
    ] {
        assert!(seen.contains(&kind), "{kind} missing from {seen:?}");
    }
    // The session lives on node-b; node-a keeps a mirror of it.
    let on_b: SessionRecord = owner_b
        .get(&format!("/local/sessions/{remote_session}"))
        .await
        .unwrap();
    assert_eq!(on_b.origin.node_id, a.card.node_id);
    assert_eq!(on_b.origin.kind, OriginKind::Human);
    assert_eq!(on_b.approver, Approver::Origin);
    let listed_a: Value = owner_a.get("/local/sessions").await.unwrap();
    assert_eq!(listed_a["remote"][0]["peer_id"], b.card.node_id.to_string());

    // A person on node-a drives node-b's agent and answers its permission
    // request from node-a: whoever opened the session approves.
    let asked = open(
        &owner_a,
        json!({"agent_id":remote_agent,"peer_id":b.card.node_id,"message":"fixture:permission"}),
    )
    .await;
    let (waiting, events) = until(&owner_a, asked.session_id, |s, _| {
        s.status == SessionStatus::WaitingPermission
    })
    .await;
    assert_eq!(waiting.approver, Approver::Origin);
    let request = events
        .iter()
        .find(|e| e.kind == "permission_request")
        .expect("permission request reached node-a");
    let key = request.payload["key"].as_str().unwrap();
    // node-b only lets the opener answer through the peer API.
    let _: SessionRecord = owner_a
        .post(
            &format!("/local/sessions/{}/permissions/{key}", asked.session_id),
            &json!({"allow":true}),
        )
        .await
        .unwrap();
    let (done, events) = turn_done(&owner_a, asked.session_id, 1).await;
    assert_eq!(
        done.last_final_text.as_deref(),
        Some("Permission granted: notes.txt edited.")
    );
    assert!(kinds(&events).contains(&"permission_resolved"));

    // Agents call each other: node-a's agent delegates to node-b's agent
    // through its own injected `hmesh mcp`. node-b runs the child session
    // with automatic permission answers (node-a's access is auto).
    let parent = open(
        &owner_a,
        json!({"agent_id":local_agent,"message":"fixture:delegate node-b/fixture fixture:permission"}),
    )
    .await;
    let (parent_done, _) = turn_done(&owner_a, parent.session_id, 1).await;
    assert!(
        parent_done
            .last_final_text
            .as_deref()
            .is_some_and(|t| t
                .ends_with("Delegated to node-b/fixture: Permission granted: notes.txt edited.")),
        "{parent_done:?}"
    );
    let children: Value = owner_b.get("/local/sessions").await.unwrap();
    let child: SessionRecord = serde_json::from_value(
        children["sessions"]
            .as_array()
            .unwrap()
            .iter()
            .find(|s| s["origin"]["kind"] == "agent")
            .cloned()
            .expect("child session on node-b"),
    )
    .unwrap();
    assert_eq!(child.approver, Approver::Auto);
    assert_eq!(child.origin.chain.len(), 1);
    assert_eq!(child.origin.chain[0].session_id, parent.session_id);
    assert_eq!(child.origin.label.as_deref(), Some("node-a / fixture"));

    // An agent may not call itself again on the same chain.
    let looped = open(
        &owner_a,
        json!({"agent_id":local_agent,"message":"fixture:delegate node-a/fixture fixture:reply"}),
    )
    .await;
    let (looped, _) = turn_done(&owner_a, looped.session_id, 1).await;
    let text = looped.last_final_text.unwrap();
    assert!(
        text.contains("Delegation failed:") && text.contains("loop"),
        "{text}"
    );
    // A session node-a opened on node-b may use node-b's agents but cannot
    // carry node-b's identity on to further computers.
    let hop = open(
        &owner_a,
        json!({"agent_id":remote_agent,"peer_id":b.card.node_id,"message":"fixture:delegate node-a/fixture fixture:reply"}),
    )
    .await;
    let (hop, _) = turn_done(&owner_a, hop.session_id, 1).await;
    let text = hop.last_final_text.unwrap();
    assert!(
        text.contains("Delegation failed:") && text.contains("only call agents on this computer"),
        "{text}"
    );

    // node-b switches node-a to confirm: new sessions wait for node-b's owner.
    let _: Value = owner_b
        .post(
            &format!("/local/peers/{}/access", a.card.node_id),
            &json!({"access":"confirm"}),
        )
        .await
        .unwrap();
    let parked = call(
        &mut mcp,
        "mesh_send",
        json!({"agent":"node-b/fixture","message":"fixture:reply","wait_seconds":1}),
    )
    .await;
    assert_eq!(parked["status"], "pending_approval", "{parked}");
    let parked_id: Uuid = parked["session_id"].as_str().unwrap().parse().unwrap();
    // The opener cannot prompt it before approval.
    let early = owner_a
        .post::<_, SessionRecord>(
            &format!("/local/sessions/{parked_id}/prompt"),
            &json!({"message":"fixture:reply"}),
        )
        .await
        .unwrap_err();
    assert_eq!(early.code, ErrorCode::ApprovalRequired);
    let _ = post(&owner_b, &format!("/local/sessions/{parked_id}/approve")).await;
    let approved = call(
        &mut mcp,
        "mesh_wait",
        json!({"session_id":parked_id,"wait_seconds":60}),
    )
    .await;
    assert_eq!(
        approved["final_text"], "Final answer: the build is green.",
        "{approved}"
    );

    // Revoking the pairing closes every session node-a opened on node-b.
    let _: Value = owner_b
        .post(
            &format!("/local/peers/{}/revoke", a.card.node_id),
            &json!({}),
        )
        .await
        .unwrap();
    let after: SessionRecord = owner_b
        .get(&format!("/local/sessions/{remote_session}"))
        .await
        .unwrap();
    assert_eq!(after.status, SessionStatus::Closed);
    let refused = owner_a
        .post::<_, SessionRecord>(
            &format!("/local/sessions/{remote_session}/prompt"),
            &json!({"message":"fixture:reply"}),
        )
        .await
        .unwrap_err();
    assert!(
        matches!(
            refused.code,
            ErrorCode::PermissionDenied | ErrorCode::Unauthenticated | ErrorCode::PeerUnreachable
        ),
        "{refused:?}"
    );

    daemon_a.stop().await;
    daemon_b.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_session_survives_a_daemon_restart_and_turns_can_be_cancelled() {
    let root = tempfile::tempdir().unwrap();
    let a = init(root.path(), "solo");
    let agent = register_fixture(&a, &root.path().join("project"));
    let daemon = Daemon::start(&a.dir);
    daemon.ready().await;
    let owner = daemon.owner();
    let session = open(
        &owner,
        json!({"agent_id":agent,"message":"fixture:echo one"}),
    )
    .await;
    let (first, _) = turn_done(&owner, session.session_id, 1).await;
    assert_eq!(first.last_final_text.as_deref(), Some("Echo #1: one"));
    // A second prompt while a turn runs is refused, then the turn is cancelled.
    let _: SessionRecord = owner
        .post(
            &format!("/local/sessions/{}/prompt", session.session_id),
            &json!({"message":"fixture:wait"}),
        )
        .await
        .unwrap();
    let busy = owner
        .post::<_, SessionRecord>(
            &format!("/local/sessions/{}/prompt", session.session_id),
            &json!({"message":"fixture:reply"}),
        )
        .await
        .unwrap_err();
    assert_eq!(busy.code, ErrorCode::TaskConflict);
    let _ = post(
        &owner,
        &format!("/local/sessions/{}/cancel", session.session_id),
    )
    .await;
    let (cancelled, _) = turn_done(&owner, session.session_id, 2).await;
    assert_eq!(cancelled.last_stop_reason.as_deref(), Some("cancelled"));
    daemon.stop().await;

    // After a restart the agent reloads the conversation (session/load).
    let daemon = Daemon::start(&a.dir);
    daemon.ready().await;
    let owner = daemon.owner();
    let _: SessionRecord = owner
        .post(
            &format!("/local/sessions/{}/prompt", session.session_id),
            &json!({"message":"fixture:echo two"}),
        )
        .await
        .unwrap();
    let (second, events) = turn_done(&owner, session.session_id, 3).await;
    assert_eq!(second.last_final_text.as_deref(), Some("Echo #3: two"));
    assert!(
        events.iter().any(|e| e.kind == "status"
            && e.payload["message"] == "the agent reloaded this conversation"),
        "{events:#?}"
    );
    // Closing stops the agent; a closed session takes no more prompts.
    let closed = post(
        &owner,
        &format!("/local/sessions/{}/close", session.session_id),
    )
    .await;
    assert_eq!(closed.status, SessionStatus::Closed);
    let refused = owner
        .post::<_, SessionRecord>(
            &format!("/local/sessions/{}/prompt", session.session_id),
            &json!({"message":"fixture:reply"}),
        )
        .await
        .unwrap_err();
    assert_eq!(refused.code, ErrorCode::TaskConflict);
    daemon.stop().await;
}
