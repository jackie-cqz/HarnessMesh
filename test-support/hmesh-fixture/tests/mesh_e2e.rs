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
    register_named(instance, project, "fixture")
}
fn register_named(instance: &Instance, project: &Path, name: &str) -> Uuid {
    register_with_args(instance, project, name, &[])
}
fn register_with_args(instance: &Instance, project: &Path, name: &str, args: &[&str]) -> Uuid {
    std::fs::create_dir_all(project).unwrap();
    let config: AgentConfig = serde_json::from_value(json!({
        "agent":{"name":name,"harness":"acp","project_id":if name == "fixture" { "demo".to_owned() } else { format!("demo-{name}") }},
        "project":{"root":project,"allowed_tools":["read","write","edit"]},
        "adapter":{"executable":env!("CARGO_BIN_EXE_hmesh-fixture"),"args":args,"provider":"fixture","model":"fixture"}
    }))
    .unwrap();
    let agent_id = Uuid::new_v4();
    let card: AgentCard = serde_json::from_value(json!({
        "agent_id":agent_id,"node_id":instance.card.node_id,"name":name,"harness":"acp","project_ids":[if name == "fixture" { "demo".to_owned() } else { format!("demo-{name}") }],"availability":"idle","capabilities":[],
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

async fn answer(owner: &Owner, id: Uuid, allow: bool) -> SessionRecord {
    let (_, events) = until(owner, id, |s, _| {
        s.status == SessionStatus::WaitingPermission
    })
    .await;
    let key = events
        .iter()
        .rev()
        .find(|e| e.kind == "permission_request")
        .unwrap()
        .payload["key"]
        .as_str()
        .unwrap()
        .to_owned();
    owner
        .post(
            &format!("/local/sessions/{id}/permissions/{key}"),
            &json!({"allow":allow}),
        )
        .await
        .unwrap()
}

#[tokio::test(flavor = "multi_thread")]
async fn rejected_permissions_depth_and_running_children_are_enforced() {
    unsafe { std::env::set_var("HMESH_MCP_COMMAND", env!("CARGO_BIN_EXE_hmesh-fixture")) };
    let root = tempfile::tempdir().unwrap();
    let a = init(root.path(), "solo");
    let agents: Vec<Uuid> = (1..=5)
        .map(|i| register_named(&a, &root.path().join(format!("p{i}")), &format!("f{i}")))
        .collect();
    let daemon = Daemon::start(&a.dir);
    daemon.ready().await;
    let owner = daemon.owner();

    // Rejecting a permission request reaches the agent as a rejection.
    let asked = open(
        &owner,
        json!({"agent_id":agents[0],"message":"fixture:permission"}),
    )
    .await;
    let answered = answer(&owner, asked.session_id, false).await;
    assert_eq!(answered.session_id, asked.session_id);
    let (done, events) = turn_done(&owner, asked.session_id, 1).await;
    assert_eq!(
        done.last_final_text.as_deref(),
        Some("Permission denied: nothing changed.")
    );
    assert!(
        events
            .iter()
            .any(|e| e.kind == "permission_resolved" && e.payload["allow"] == false)
    );

    // Depth through the injected MCP tools: f1 -> f2 -> f3 -> f4 is allowed,
    // f4 -> f5 would be a fourth level below the person and is refused.
    let message = "fixture:delegate solo/f2 fixture:delegate solo/f3 fixture:delegate solo/f4 fixture:delegate solo/f5 fixture:reply";
    let deep = open(&owner, json!({"agent_id":agents[0],"message":message})).await;
    let (deep, _) = turn_done(&owner, deep.session_id, 1).await;
    let text = deep.last_final_text.unwrap();
    assert!(
        text.contains("Delegated to solo/f2")
            && text.contains("Delegated to solo/f4")
            && text.contains("Delegation failed")
            && text.contains("levels"),
        "{text}"
    );
    let listed: Value = owner.get("/local/sessions").await.unwrap();
    let depths: Vec<usize> = listed["sessions"]
        .as_array()
        .unwrap()
        .iter()
        .map(|s| s["origin"]["chain"].as_array().unwrap().len())
        .collect();
    assert_eq!(depths.iter().max(), Some(&3), "{depths:?}");

    // At most four delegated sessions run at once under one session.
    let parent = open(&owner, json!({"agent_id":agents[0]})).await;
    let mut children = vec![];
    for _ in 0..4 {
        let child = open(
            &owner,
            json!({"agent_id":agents[1],"parent_session_id":parent.session_id,"message":"fixture:wait"}),
        )
        .await;
        assert_eq!(child.approver, Approver::Auto);
        children.push(child.session_id);
    }
    let fifth = owner
        .post::<_, SessionRecord>(
            "/local/sessions",
            &json!({"agent_id":agents[2],"parent_session_id":parent.session_id,"message":"fixture:wait"}),
        )
        .await
        .unwrap_err();
    assert_eq!(fifth.code, ErrorCode::RateLimited, "{fifth:?}");
    // Once one finishes, another may start.
    let _ = post(&owner, &format!("/local/sessions/{}/cancel", children[0])).await;
    turn_done(&owner, children[0], 1).await;
    let _ = open(
        &owner,
        json!({"agent_id":agents[2],"parent_session_id":parent.session_id,"message":"fixture:echo ok"}),
    )
    .await;
    // A closed caller cannot open more.
    let _ = post(
        &owner,
        &format!("/local/sessions/{}/close", parent.session_id),
    )
    .await;
    let closed = owner
        .post::<_, SessionRecord>(
            "/local/sessions",
            &json!({"agent_id":agents[3],"parent_session_id":parent.session_id}),
        )
        .await
        .unwrap_err();
    assert_eq!(closed.code, ErrorCode::PermissionDenied);
    daemon.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_paired_computers_delegation_follows_its_access_mode_and_ends_with_the_pairing() {
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
    let front = register_named(&a, &root.path().join("front"), "front");
    let _worker = register_named(&a, &root.path().join("worker"), "worker");
    let slow = register_named(&a, &root.path().join("slow"), "slow");
    let daemon_a = Daemon::start(&a.dir);
    let daemon_b = Daemon::start(&b.dir);
    daemon_a.ready().await;
    daemon_b.ready().await;
    let owner_a = daemon_a.owner();
    let owner_b = daemon_b.owner();
    let child_of = |list: &Value, parent: Uuid| -> Option<SessionRecord> {
        list["sessions"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|s| serde_json::from_value::<SessionRecord>(s.clone()).ok())
            .find(|s| {
                s.origin
                    .chain
                    .last()
                    .is_some_and(|l| l.session_id == parent)
            })
    };

    // node-a asks to confirm node-b. A session node-b opens waits for
    // approval, and so does every session its agent delegates to here.
    let _: Value = owner_a
        .post(
            &format!("/local/peers/{}/access", b.card.node_id),
            &json!({"access":"confirm"}),
        )
        .await
        .unwrap();
    let parent = open(
        &owner_b,
        json!({"agent_id":front,"peer_id":a.card.node_id,"message":"fixture:delegate node-a/worker fixture:permission"}),
    )
    .await;
    assert_eq!(parent.status, SessionStatus::PendingApproval);
    let _ = post(
        &owner_a,
        &format!("/local/sessions/{}/approve", parent.session_id),
    )
    .await;
    let (parent_done, _) = turn_done(&owner_b, parent.session_id, 1).await;
    assert!(
        parent_done
            .last_final_text
            .as_deref()
            .is_some_and(|t| t.contains("pending_approval")),
        "{parent_done:?}"
    );
    let listed: Value = owner_a.get("/local/sessions").await.unwrap();
    let child = child_of(&listed, parent.session_id).expect("delegated session on node-a");
    assert_eq!(child.status, SessionStatus::PendingApproval);
    assert_eq!(child.approver, Approver::Local);
    assert_eq!(child.origin.root_node_id, b.card.node_id);
    assert_eq!(child.origin.node_id, a.card.node_id);

    // node-b cannot read or drive a session it did not open itself.
    let as_b = hmesh_transport::Client::new(
        &Instance::load(&b.dir).unwrap(),
        &a.card.endpoint,
        &a.card.fingerprint,
        false,
        false,
    )
    .unwrap();
    for denied in [
        as_b.get::<SessionRecord>(&format!("/hmesh/v1/sessions/{}", child.session_id))
            .await
            .unwrap_err(),
        as_b.post::<_, SessionRecord>(
            &format!("/hmesh/v1/sessions/{}/close", child.session_id),
            &json!({}),
        )
        .await
        .unwrap_err(),
    ] {
        assert_eq!(denied.code, ErrorCode::PermissionDenied, "{denied:?}");
    }
    // node-a's owner approves it and answers its permission request.
    let _ = post(
        &owner_a,
        &format!("/local/sessions/{}/approve", child.session_id),
    )
    .await;
    answer(&owner_a, child.session_id, true).await;
    let (child_done, events) = turn_done(&owner_a, child.session_id, 1).await;
    assert_eq!(
        child_done.last_final_text.as_deref(),
        Some("Permission granted: notes.txt edited.")
    );
    assert!(
        events
            .iter()
            .any(|e| e.kind == "permission_resolved" && e.payload["by"] == "owner")
    );

    // Back to auto; a delegation from node-b is still running when node-a
    // revokes the pairing: the session node-b opened and its descendants close.
    let _: Value = owner_a
        .post(
            &format!("/local/peers/{}/access", b.card.node_id),
            &json!({"access":"auto"}),
        )
        .await
        .unwrap();
    let parent2 = open(
        &owner_b,
        json!({"agent_id":front,"peer_id":a.card.node_id,"message":"fixture:delegate node-a/slow fixture:wait"}),
    )
    .await;
    let deadline = Instant::now() + Duration::from_secs(60);
    let child2 = loop {
        let listed: Value = owner_a.get("/local/sessions").await.unwrap();
        if let Some(child) = child_of(&listed, parent2.session_id)
            && child.status == SessionStatus::Running
        {
            break child;
        }
        assert!(Instant::now() < deadline, "delegated session did not start");
        tokio::time::sleep(Duration::from_millis(200)).await;
    };
    assert_eq!(child2.agent_id, slow);
    assert_eq!(child2.approver, Approver::Auto);
    let _: Value = owner_a
        .post(
            &format!("/local/peers/{}/revoke", b.card.node_id),
            &json!({}),
        )
        .await
        .unwrap();
    for id in [parent2.session_id, child2.session_id] {
        let record: SessionRecord = owner_a.get(&format!("/local/sessions/{id}")).await.unwrap();
        assert_eq!(record.status, SessionStatus::Closed, "{record:?}");
    }
    // The agent process of the closed child is gone, so its turn cannot finish.
    let (_, events) = until(&owner_a, child2.session_id, |_, e| {
        e.iter()
            .any(|e| e.kind == "status" && e.payload["status"] == "closed")
    })
    .await;
    assert!(!events.iter().any(|e| e.kind == "turn_end"), "{events:#?}");
    daemon_a.stop().await;
    daemon_b.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn closing_while_the_agent_starts_never_runs_the_turn() {
    let root = tempfile::tempdir().unwrap();
    let a = init(root.path(), "slowstart");
    // This agent takes 2.5 s to answer initialize.
    let agent = register_with_args(
        &a,
        &root.path().join("project"),
        "slow-start",
        &["--start-delay-ms", "2500"],
    );
    let daemon = Daemon::start(&a.dir);
    daemon.ready().await;
    let owner = daemon.owner();
    let session = open(&owner, json!({"agent_id":agent,"message":"fixture:reply"})).await;
    tokio::time::sleep(Duration::from_millis(500)).await;
    let closed = post(
        &owner,
        &format!("/local/sessions/{}/close", session.session_id),
    )
    .await;
    assert_eq!(closed.status, SessionStatus::Closed);
    tokio::time::sleep(Duration::from_secs(5)).await;
    let page = page(&owner, session.session_id, 0, 0).await;
    let kinds = kinds(&page.events);
    assert!(
        !kinds
            .iter()
            .any(|k| *k == "agent_message_chunk" || *k == "turn_end"),
        "{kinds:?}"
    );
    assert_eq!(page.session.status, SessionStatus::Closed);
    assert_eq!(page.session.turns, 0);
    daemon.stop().await;
}
