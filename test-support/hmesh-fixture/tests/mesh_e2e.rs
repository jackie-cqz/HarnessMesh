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
    register_as(instance, project, name, "acp", args)
}
fn register_as(
    instance: &Instance,
    project: &Path,
    name: &str,
    harness: &str,
    args: &[&str],
) -> Uuid {
    std::fs::create_dir_all(project).unwrap();
    let config: AgentConfig = serde_json::from_value(json!({
        "agent":{"name":name,"harness":harness,"project_id":if name == "fixture" { "demo".to_owned() } else { format!("demo-{name}") }},
        "project":{"root":project,"allowed_tools":["read","write","edit"]},
        "adapter":{"executable":env!("CARGO_BIN_EXE_hmesh-fixture"),"args":args,"provider":"fixture","model":"fixture"}
    }))
    .unwrap();
    let agent_id = Uuid::new_v4();
    let card: AgentCard = serde_json::from_value(json!({
        "agent_id":agent_id,"node_id":instance.card.node_id,"name":name,"harness":harness,"project_ids":[if name == "fixture" { "demo".to_owned() } else { format!("demo-{name}") }],"availability":"idle","capabilities":[],
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

    // Answering from another computer is only ever once: a lasting option
    // chosen on node-a reaches node-b as a one-time allow.
    let asked = open(
        &owner_a,
        json!({"agent_id":remote_agent,"peer_id":b.card.node_id,"message":"fixture:permission-options"}),
    )
    .await;
    let (_, events) = until(&owner_a, asked.session_id, |s, _| {
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
    let _: SessionRecord = owner_a
        .post(
            &format!("/local/sessions/{}/permissions/{key}", asked.session_id),
            &json!({"allow":true,"option_id":"always"}),
        )
        .await
        .unwrap();
    let (done, events) = turn_done(&owner_a, asked.session_id, 1).await;
    assert_eq!(done.last_final_text.as_deref(), Some("Chose allow."));
    let resolved = events
        .iter()
        .find(|e| e.kind == "permission_resolved")
        .unwrap();
    assert_eq!(resolved.payload["by"], "origin");
    assert!(resolved.payload.get("option").is_none());

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

/// A computer set up with only MCP (`hmesh setup --mcp-only`) opens sessions
/// on a paired computer through its MCP server and gets the answers, while
/// that computer can open nothing on it, not even with its own agent there.
#[tokio::test(flavor = "multi_thread")]
async fn a_computer_that_only_connects_out_opens_sessions_but_none_open_on_it() {
    let root = tempfile::tempdir().unwrap();
    let mut config = NodeConfig::default();
    config.node.name = "laptop".into();
    config.node.network = "tailscale".into();
    config.node.tailnet_address = Some("100.64.0.7".parse().unwrap());
    config.node.listen_port = free_port();
    let laptop = Instance::init(&root.path().join("laptop"), config).unwrap();
    assert!(laptop.config.outbound_only());
    let studio = init(root.path(), "studio");
    Store::open(&laptop.dir.join("state.sqlite3"))
        .unwrap()
        .trust_peer(&studio.card)
        .unwrap();
    Store::open(&studio.dir.join("state.sqlite3"))
        .unwrap()
        .trust_peer(&laptop.card)
        .unwrap();
    register_fixture(&studio, &root.path().join("studio-project"));
    let laptop_agent = register_named(&laptop, &root.path().join("laptop-project"), "local");
    let daemon_laptop = Daemon::start(&laptop.dir);
    let daemon_studio = Daemon::start(&studio.dir);
    daemon_laptop.ready().await;
    daemon_studio.ready().await;

    let mut mcp = initialized(&laptop.dir).await;
    let answer = call(
        &mut mcp,
        "mesh_send",
        json!({"agent":"studio/fixture","message":"fixture:reply","wait_seconds":60}),
    )
    .await;
    assert_eq!(answer["done"], true, "{answer}");
    assert_eq!(answer["final_text"], "Final answer: the build is green.");

    let refused = daemon_studio
        .owner()
        .post::<_, SessionRecord>(
            "/local/sessions",
            &json!({"agent_id":laptop_agent,"peer_id":laptop.card.node_id,"message":"fixture:reply"}),
        )
        .await;
    let refused = refused.expect_err("laptop accepts no sessions");
    assert_eq!(refused.code, ErrorCode::PermissionDenied, "{refused:?}");
    let listed: Value = daemon_laptop.owner().get("/local/sessions").await.unwrap();
    assert!(
        listed["sessions"]
            .as_array()
            .unwrap()
            .iter()
            .all(|s| s["agent_id"] != json!(laptop_agent)),
        "{listed}"
    );
    daemon_laptop.stop().await;
    daemon_studio.stop().await;
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
async fn resume_only_agent_keeps_its_session_id_and_context_after_a_failed_restore() {
    let root = tempfile::tempdir().unwrap();
    let node = init(root.path(), "solo");
    let agent = register_as(
        &node,
        &root.path().join("project"),
        "resume",
        "dsh",
        &["--profile", "acp"],
    );
    let daemon = Daemon::start(&node.dir);
    daemon.ready().await;
    let session = open(
        &daemon.owner(),
        json!({"agent_id":agent,"message":"fixture:echo one"}),
    )
    .await;
    let id = session.session_id;
    let (first, _) = turn_done(&daemon.owner(), id, 1).await;
    assert_eq!(first.last_final_text.as_deref(), Some("Echo #1: one"));
    daemon.stop().await;

    // Simulate an agent whose persisted state is temporarily unavailable.
    let saved = std::env::temp_dir()
        .join("hmesh-fixture-sessions")
        .join(format!(
            "{}.json",
            first.agent_session_id.as_deref().unwrap()
        ));
    let backup = saved.with_extension("bak");
    std::fs::rename(&saved, &backup).unwrap();
    let daemon = Daemon::start(&node.dir);
    daemon.ready().await;
    let owner = daemon.owner();
    prompt(&owner, id, "fixture:echo must not run").await;
    let (failed, _) = until(&owner, id, |r, _| r.status == SessionStatus::Idle).await;
    assert!(failed.last_error.unwrap().contains("session/resume"));
    assert_eq!(failed.agent_session_id, first.agent_session_id);
    assert_eq!(failed.turns, 1);

    std::fs::rename(&backup, &saved).unwrap();
    prompt(&owner, id, "fixture:echo two").await;
    let (resumed, events) = turn_done(&owner, id, 2).await;
    assert_eq!(resumed.agent_session_id, first.agent_session_id);
    assert_eq!(resumed.last_final_text.as_deref(), Some("Echo #2: two"));
    assert_eq!(
        events
            .iter()
            .filter(|e| e.kind == "agent_message_chunk")
            .count(),
        2
    );
    daemon.stop().await;
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

    // On the agent's own computer every option it offers can be chosen,
    // lasting ones included; the kind, not `allow`, decides what it means.
    let asked = open(
        &owner,
        json!({"agent_id":agents[0],"message":"fixture:permission-options"}),
    )
    .await;
    let (_, events) = until(&owner, asked.session_id, |s, _| {
        s.status == SessionStatus::WaitingPermission
    })
    .await;
    let request = events
        .iter()
        .rev()
        .find(|e| e.kind == "permission_request")
        .unwrap();
    let kinds_offered: Vec<&str> = request.payload["options"]
        .as_array()
        .unwrap()
        .iter()
        .map(|o| o["kind"].as_str().unwrap())
        .collect();
    assert_eq!(
        kinds_offered,
        ["allow_once", "allow_always", "reject_once", "reject_always"]
    );
    let call_id = request.payload["tool_call_id"].as_str().unwrap().to_owned();
    assert!(
        events
            .iter()
            .any(|e| e.kind == "tool_call" && e.payload["id"] == call_id.as_str()),
        "the request names the call the stream showed"
    );
    let key = request.payload["key"].as_str().unwrap().to_owned();
    let path = format!("/local/sessions/{}/permissions/{key}", asked.session_id);
    let forged = owner
        .post::<_, SessionRecord>(&path, &json!({"allow":true,"option_id":"bogus"}))
        .await
        .unwrap_err();
    assert_eq!(forged.code, ErrorCode::InvalidInput, "{forged:?}");
    let still: SessionRecord = owner
        .get(&format!("/local/sessions/{}", asked.session_id))
        .await
        .unwrap();
    assert_eq!(
        still.status,
        SessionStatus::WaitingPermission,
        "a refused answer leaves the request open"
    );
    let _: SessionRecord = owner
        .post(&path, &json!({"allow":false,"option_id":"always"}))
        .await
        .unwrap();
    let (done, events) = turn_done(&owner, asked.session_id, 1).await;
    assert_eq!(done.last_final_text.as_deref(), Some("Chose always."));
    let resolved = events
        .iter()
        .find(|e| e.kind == "permission_resolved")
        .unwrap();
    assert_eq!(resolved.payload["allow"], true);
    assert_eq!(resolved.payload["option"]["kind"], "allow_always");
    assert_eq!(resolved.payload["option"]["name"], "Always allow make");

    // A request for a call the stream never showed brings the call along.
    let asked = open(
        &owner,
        json!({"agent_id":agents[0],"message":"fixture:permission-unseen"}),
    )
    .await;
    let (_, events) = until(&owner, asked.session_id, |s, _| {
        s.status == SessionStatus::WaitingPermission
    })
    .await;
    let request = events
        .iter()
        .rev()
        .find(|e| e.kind == "permission_request")
        .unwrap();
    let call_id = request.payload["tool_call_id"].as_str().unwrap();
    let call = events
        .iter()
        .find(|e| e.kind == "tool_call_update" && e.payload["id"] == call_id)
        .expect("the call's row comes from the request");
    assert_eq!(call.payload["kind"], "edit");
    assert_eq!(call.payload["diffs"][0]["path"], "notes.txt");
    assert_eq!(call.payload["locations"][0]["line"], 3);
    let answered = answer(&owner, asked.session_id, true).await;
    assert_eq!(answered.session_id, asked.session_id);
    let (done, _) = turn_done(&owner, asked.session_id, 1).await;
    assert_eq!(done.last_final_text.as_deref(), Some("Chose allow."));

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

fn fnv(text: &str) -> u64 {
    text.bytes().fold(0xcbf29ce484222325, |h, b| {
        (h ^ u64::from(b)).wrapping_mul(0x100000001b3)
    })
}
async fn configure(
    owner: &Owner,
    id: Uuid,
    config_id: &str,
    value: Value,
) -> std::result::Result<SessionRecord, MeshError> {
    owner
        .post(
            &format!("/local/sessions/{id}/config"),
            &json!({"config_id":config_id,"value":value}),
        )
        .await
}
async fn prompt(owner: &Owner, id: Uuid, message: &str) {
    let _: SessionRecord = owner
        .post(
            &format!("/local/sessions/{id}/prompt"),
            &json!({"message":message}),
        )
        .await
        .unwrap();
}
fn current(record: &SessionRecord, id: &str) -> Value {
    record
        .config_options
        .iter()
        .find(|o| o.id == id)
        .map(|o| o.current.clone())
        .unwrap_or(Value::Null)
}
async fn set_service(owner: &Owner, agent: Uuid, body: Value) -> service::AgentServiceView {
    owner
        .post(&format!("/local/agents/{agent}/service"), &body)
        .await
        .unwrap()
}

/// A model service on loopback that records every request it receives:
/// `/listed` lists two models, `/unlisted` has no model list but answers
/// chat requests, `/locked` rejects the key (quoting it back),
/// `/anthropic` lists models the Anthropic way, and `/gateway` streams
/// replies like a gateway whose last chunk has an empty `delta.role`.
#[derive(Debug, Clone)]
struct Seen {
    method: String,
    path: String,
    authorization: String,
    api_key: String,
    body: String,
}
async fn fake_model_service() -> (String, std::sync::Arc<std::sync::Mutex<Vec<Seen>>>) {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let seen = std::sync::Arc::new(std::sync::Mutex::new(Vec::<Seen>::new()));
    let log = seen.clone();
    tokio::spawn(async move {
        loop {
            let Ok((mut stream, _)) = listener.accept().await else {
                return;
            };
            let log = log.clone();
            tokio::spawn(async move {
                let mut bytes = Vec::new();
                let mut buffer = [0; 4096];
                let (head, body) = loop {
                    let n = stream.read(&mut buffer).await.unwrap_or(0);
                    if n == 0 {
                        return;
                    }
                    bytes.extend_from_slice(&buffer[..n]);
                    let text = String::from_utf8_lossy(&bytes).into_owned();
                    if let Some(end) = text.find("\r\n\r\n") {
                        let head = text[..end].to_owned();
                        let length = head
                            .lines()
                            .find_map(|l| {
                                l.to_ascii_lowercase()
                                    .strip_prefix("content-length:")
                                    .map(|v| v.trim().parse::<usize>().unwrap_or(0))
                            })
                            .unwrap_or(0);
                        if text.len() >= end + 4 + length {
                            break (head, text[end + 4..end + 4 + length].to_owned());
                        }
                    }
                };
                let mut lines = head.lines();
                let mut first = lines.next().unwrap_or_default().split(' ');
                let method = first.next().unwrap_or_default().to_owned();
                let path = first.next().unwrap_or_default().to_owned();
                let header = |name: &str| {
                    head.lines()
                        .find_map(|l| {
                            let (k, v) = l.split_once(':')?;
                            k.eq_ignore_ascii_case(name).then(|| v.trim().to_owned())
                        })
                        .unwrap_or_default()
                };
                let authorization = header("authorization");
                let api_key = header("x-api-key");
                log.lock().unwrap().push(Seen {
                    method: method.clone(),
                    path: path.clone(),
                    authorization: authorization.clone(),
                    api_key,
                    body,
                });
                let (status, reply) = match (method.as_str(), path.as_str()) {
                    ("GET", "/listed/v1/models") => (
                        "200 OK",
                        r#"{"object":"list","data":[{"id":"m-one"},{"id":"m-two"}]}"#.to_owned(),
                    ),
                    ("POST", "/unlisted/v1/chat/completions") => (
                        "200 OK",
                        r#"{"choices":[{"message":{"content":"p"}}]}"#.to_owned(),
                    ),
                    ("POST", "/unlisted/v1/responses") => (
                        "200 OK",
                        r#"{"object":"response","status":"completed","output":[]}"#.to_owned(),
                    ),
                    ("GET", "/locked/v1/models") => (
                        "401 Unauthorized",
                        format!(r#"{{"error":"invalid key: {authorization}"}}"#),
                    ),
                    ("GET", p) if p.starts_with("/anthropic/v1/models") => (
                        "200 OK",
                        r#"{"data":[{"id":"claude-fake","type":"model"}],"has_more":false}"#
                            .to_owned(),
                    ),
                    ("GET", "/slow/v1/models") => {
                        tokio::time::sleep(Duration::from_secs(30)).await;
                        ("200 OK", r#"{"data":[]}"#.to_owned())
                    }
                    // Points at a model list that would succeed, if followed.
                    ("GET", "/redirect/v1/models" | "/gateway/v1/moved") => {
                        ("302 Found", String::new())
                    }
                    ("POST", "/gateway/v1/chat/completions") => (
                        "200 OK",
                        [
                            r#"{"id":"c1","choices":[{"index":0,"delta":{"role":"assistant","content":"你好"}}]}"#,
                            r#"{"id":"c1","choices":[{"index":0,"delta":{"role":"","content":"","reasoning_content":""},"finish_reason":"stop"}]}"#,
                            "[DONE]",
                        ]
                        .map(|data| format!("data: {data}\n\n"))
                        .concat(),
                    ),
                    ("GET", p) if p.starts_with("/gateway/v1/models") => {
                        ("200 OK", r#"{"data":[{"id":"m-one"}]}"#.to_owned())
                    }
                    _ => ("404 Not Found", "not here".to_owned()),
                };
                let location = if status.starts_with("302") {
                    "Location: /listed/v1/models\r\n"
                } else {
                    ""
                };
                let kind = if path == "/gateway/v1/chat/completions" {
                    "text/event-stream"
                } else {
                    "application/json"
                };
                let _ = stream
                    .write_all(
                        format!(
                            "HTTP/1.1 {status}\r\n{location}Content-Type: {kind}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{reply}",
                            reply.len()
                        )
                        .as_bytes(),
                    )
                    .await;
            });
        }
    });
    (base, seen)
}

/// A session's model use is what its agent reported: the latest context and
/// cost report (`usage_update`) and the turn's token counts, recorded once
/// per turn and summed per currency only across sessions.
#[tokio::test(flavor = "multi_thread")]
async fn session_usage_is_what_the_agent_reported() {
    let root = tempfile::tempdir().unwrap();
    let a = init(root.path(), "node-a");
    let agent = register_fixture(&a, &root.path().join("project"));
    let daemon = Daemon::start(&a.dir);
    daemon.ready().await;
    let owner = daemon.owner();
    let quiet = open(
        &owner,
        json!({"agent_id":agent,"message":"fixture:echo hi"}),
    )
    .await;
    turn_done(&owner, quiet.session_id, 1).await;
    let session = open(&owner, json!({"agent_id":agent,"message":"fixture:usage"})).await;
    // The session reads idle a moment before its `turn_end` is recorded:
    // wait for that too.
    let (_, events) = until(&owner, session.session_id, |s, events| {
        s.turns >= 1 && !s.status.is_busy() && events.iter().any(|e| e.kind == "turn_end")
    })
    .await;
    let usage: Vec<&SessionEvent> = events.iter().filter(|e| e.kind == "usage").collect();
    assert_eq!(usage.len(), 1, "{:?}", kinds(&events));
    assert_eq!(
        usage[0].payload,
        json!({
            "context":{"used":1200,"size":128000},
            "cost":{"amount":0.0125,"currency":"USD"},
            "turn":{"total_tokens":40,"input_tokens":30,"output_tokens":10,"thought_tokens":null,"cached_read_tokens":0,"cached_write_tokens":0},
        })
    );
    // The usage event comes before the turn ends, so waiting for the turn's
    // end sees it.
    let order: Vec<&str> = kinds(&events)
        .into_iter()
        .filter(|k| ["usage", "turn_end"].contains(k))
        .collect();
    assert_eq!(order, ["usage", "turn_end"]);
    // A later turn reports more; the summary keeps the latest report (the
    // cost is already cumulative), not a sum.
    prompt(&owner, session.session_id, "fixture:usage-more").await;
    turn_done(&owner, session.session_id, 2).await;
    // A turn that reports nothing records nothing, so old numbers are not
    // repeated as if they were new.
    prompt(&owner, session.session_id, "fixture:echo again").await;
    let (_, events) = turn_done(&owner, session.session_id, 3).await;
    assert_eq!(events.iter().filter(|e| e.kind == "usage").count(), 2);
    let report: Value = owner.get("/local/usage").await.unwrap();
    let sessions = &report["sessions"];
    assert_eq!(sessions["sessions_total"], 2, "{sessions}");
    assert_eq!(sessions["sessions_reported"], 1, "{sessions}");
    let row = &sessions["rows"][0];
    assert_eq!(row["session_id"], json!(session.session_id));
    assert_eq!(row["turns"], 3);
    assert_eq!(row["usage"]["context"], json!({"used":2400,"size":128000}));
    assert_eq!(row["usage"]["turn"]["total_tokens"], 80);
    assert_eq!(
        sessions["cost_totals"],
        json!([{"currency":"USD","amount":0.03}])
    );
    // Delegated tasks keep their own report.
    assert!(
        report["rows"].is_array() && report["total_tasks"] == 0,
        "{report}"
    );
    // After a restart the agent reports only tokens at first: the session's
    // cost and context are not lost from the summary.
    daemon.stop().await;
    let daemon = Daemon::start(&a.dir);
    daemon.ready().await;
    let owner = daemon.owner();
    prompt(&owner, session.session_id, "fixture:usage-tokens").await;
    turn_done(&owner, session.session_id, 4).await;
    let report: Value = owner.get("/local/usage").await.unwrap();
    let row = &report["sessions"]["rows"][0];
    assert_eq!(row["usage"]["turn"]["total_tokens"], 7, "{row}");
    assert_eq!(row["usage"]["context"], json!({"used":2400,"size":128000}));
    assert_eq!(
        report["sessions"]["cost_totals"],
        json!([{"currency":"USD","amount":0.03}])
    );
    daemon.stop().await;
}

/// OpenCode on a custom endpoint reaches it through the daemon's relay: the
/// agent receives only a local address and token, the endpoint receives the
/// real key, and a gateway's empty `delta.role` never reaches the agent.
#[tokio::test(flavor = "multi_thread")]
async fn opencode_reaches_a_custom_endpoint_through_the_local_relay() {
    let root = tempfile::tempdir().unwrap();
    let a = init(root.path(), "node-a");
    let opencode = register_as(
        &a,
        &root.path().join("opencode"),
        "opencode",
        "opencode",
        &[],
    );
    let (base, seen) = fake_model_service().await;
    let daemon = Daemon::start(&a.dir);
    daemon.ready().await;
    let owner = daemon.owner();
    let key = "sk-fake-gateway-0123456789";
    set_service(
        &owner,
        opencode,
        json!({"mode":"custom","base_url":format!("{base}/gateway/v1"),"api_key":key,"models":["m-one"]}),
    )
    .await;
    let value = |text: Option<String>| {
        let text = text.unwrap_or_default();
        match text.split_once("value=") {
            Some((_, value)) => value.to_owned(),
            None => panic!("{text}"),
        }
    };
    let session = open(
        &owner,
        json!({"agent_id":opencode,"message":"fixture:env OPENCODE_CONFIG_CONTENT"}),
    )
    .await;
    let (done, _) = turn_done(&owner, session.session_id, 1).await;
    let config: Value = serde_json::from_str(&value(done.last_final_text)).unwrap();
    let relay = config["provider"]["hmesh"]["options"]["baseURL"]
        .as_str()
        .unwrap()
        .to_owned();
    assert!(
        relay.starts_with("http://127.0.0.1:") && !relay.contains(&base),
        "{relay}"
    );
    prompt(
        &owner,
        session.session_id,
        "fixture:env HMESH_OPENCODE_API_KEY",
    )
    .await;
    let (done, _) = turn_done(&owner, session.session_id, 2).await;
    let token_report = done.last_final_text.unwrap();
    assert!(token_report.contains("len=68 fnv=") && token_report.ends_with("value=[REDACTED]"));
    let take = || std::mem::take(&mut *seen.lock().unwrap());
    take();

    let http = reqwest::Client::builder()
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .unwrap();
    let chat = format!("{relay}/chat/completions");
    let request =
        json!({"model":"m-one","stream":true,"messages":[{"role":"user","content":"hi"}]});
    // Without the relay's token nothing is sent on, and the key is not one.
    let anonymous = http.post(&chat).json(&request).send().await.unwrap();
    assert_eq!(anonymous.status(), 401);
    let with_key = http
        .post(&chat)
        .bearer_auth(key)
        .json(&request)
        .send()
        .await
        .unwrap();
    assert_eq!(with_key.status(), 401);
    assert!(take().is_empty());

    // The fixture uses the token in its process and reports only the result.
    prompt(&owner, session.session_id, "fixture:model-relay").await;
    let (done, _) = turn_done(&owner, session.session_id, 3).await;
    let result: Value = serde_json::from_str(&done.last_final_text.unwrap()).unwrap();
    assert_eq!(
        result,
        json!({"chat":200,"listed":200,"moved":302,"fixed":true})
    );
    let requests = take();
    assert_eq!(
        requests.iter().map(|r| r.path.as_str()).collect::<Vec<_>>(),
        [
            "/gateway/v1/chat/completions",
            "/gateway/v1/models?limit=5",
            "/gateway/v1/moved"
        ]
    );
    assert!(
        requests
            .iter()
            .all(|r| r.authorization == format!("Bearer {key}"))
    );
    assert_eq!(requests[0].method, "POST");
    assert_eq!(
        serde_json::from_str::<Value>(&requests[0].body).unwrap(),
        request
    );
    // Another session of the agent gets the same token, so agents already
    // running keep working.
    let second = open(
        &owner,
        json!({"agent_id":opencode,"message":"fixture:env HMESH_OPENCODE_API_KEY"}),
    )
    .await;
    let (done, _) = turn_done(&owner, second.session_id, 1).await;
    assert_eq!(done.last_final_text.as_deref(), Some(token_report.as_str()));
    daemon.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn model_lists_reach_opencode_and_connection_tests_ask_for_models_first() {
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
    let opencode = register_as(
        &a,
        &root.path().join("opencode"),
        "opencode",
        "opencode",
        &[],
    );
    let claude = register_as(
        &a,
        &root.path().join("claude"),
        "claude",
        "claude-code",
        &[],
    );
    let codex = register_as(&a, &root.path().join("codex"), "codex", "codex", &[]);
    let (base, seen) = fake_model_service().await;
    let daemon_a = Daemon::start(&a.dir);
    let daemon_b = Daemon::start(&b.dir);
    daemon_a.ready().await;
    daemon_b.ready().await;
    let owner = daemon_a.owner();
    let key = "sk-fake-service-0123456789";
    let test_path = format!("/local/agents/{opencode}/service/test");
    let test = |body: Value| {
        let owner = &owner;
        let path = &test_path;
        async move {
            owner
                .post::<_, service::ServiceTestResult>(path, &body)
                .await
                .unwrap()
        }
    };
    let take = || std::mem::take(&mut *seen.lock().unwrap());

    // Before saving: the typed key is tried, the model list comes back, and
    // one request is enough.
    let listed =
        test(json!({"mode":"custom","base_url":format!("{base}/listed/v1/"),"api_key":key})).await;
    assert!(listed.ok, "{listed:?}");
    assert_eq!(listed.method.as_deref(), Some("models"));
    assert_eq!(listed.models, ["m-one", "m-two"]);
    let requests = take();
    assert_eq!(requests.len(), 1, "{requests:?}");
    assert_eq!(
        (requests[0].method.as_str(), requests[0].path.as_str()),
        ("GET", "/listed/v1/models")
    );
    assert_eq!(requests[0].authorization, format!("Bearer {key}"));

    let legacy: Value = owner
        .get(&format!("/local/agents/{opencode}/service"))
        .await
        .unwrap();
    assert!(legacy.get("protocols").is_none());
    assert!(legacy["presets"][0].get("addresses").is_none());
    let current: Value = owner
        .get(&format!("/local/agents/{opencode}/service?protocols=true"))
        .await
        .unwrap();
    assert_eq!(current["protocol"], "openai-chat");
    assert_eq!(current["protocols"].as_array().unwrap().len(), 3);
    assert_eq!(
        current["presets"][0]["addresses"]["anthropic-messages"],
        "https://api.deepseek.com/anthropic"
    );

    // Save the list; the agent process gets every model and only them.
    let saved = set_service(
        &owner,
        opencode,
        json!({"mode":"custom","base_url":format!("{base}/unlisted/v1"),"api_key":key,"models":["m-one","m-two"]}),
    )
    .await;
    assert_eq!(saved.default_model.as_deref(), Some("m-one"));
    assert_eq!(saved.models, ["m-one", "m-two"]);
    let session = open(
        &owner,
        json!({"agent_id":opencode,"message":"fixture:env OPENCODE_CONFIG_CONTENT"}),
    )
    .await;
    let (done, _) = turn_done(&owner, session.session_id, 1).await;
    let text = done.last_final_text.unwrap();
    let config: Value = serde_json::from_str(text.split_once("value=").unwrap().1)
        .unwrap_or_else(|_| panic!("{text}"));
    assert_eq!(config["enabled_providers"], json!(["hmesh"]));
    let offered: Vec<&String> = config["provider"]["hmesh"]["models"]
        .as_object()
        .unwrap()
        .keys()
        .collect();
    assert_eq!(offered, ["m-one", "m-two"]);
    assert!(!text.contains(key), "{text}");

    // With the stored key: no model list, so one minimal request follows.
    let unlisted = test(json!({"mode":"custom","base_url":format!("{base}/unlisted/v1")})).await;
    assert!(unlisted.ok, "{unlisted:?}");
    assert_eq!(unlisted.method.as_deref(), Some("request"));
    let requests = take();
    assert_eq!(
        requests
            .iter()
            .map(|r| (r.method.as_str(), r.path.as_str()))
            .collect::<Vec<_>>(),
        [
            ("GET", "/unlisted/v1/models"),
            ("POST", "/unlisted/v1/chat/completions")
        ]
    );
    let body: Value = serde_json::from_str(&requests[1].body).unwrap();
    assert_eq!(
        (body["model"].clone(), body["max_tokens"].clone()),
        (json!("m-one"), json!(1))
    );
    assert_eq!(requests[1].authorization, format!("Bearer {key}"));

    // A rejected key is reported at once, without a second request, and the
    // service quoting the key back does not show it.
    let locked = test(json!({"mode":"custom","base_url":format!("{base}/locked/v1")})).await;
    assert!(!locked.ok);
    assert_eq!(take().len(), 1);
    let shown = serde_json::to_string(&locked).unwrap();
    assert!(
        shown.contains("401") && shown.contains("[REDACTED]") && !shown.contains(key),
        "{shown}"
    );
    // A redirect is not followed: the key never reaches the other address.
    let redirected = test(json!({"mode":"custom","base_url":format!("{base}/redirect/v1")})).await;
    assert!(!redirected.ok, "{redirected:?}");
    let requests = take();
    assert!(
        requests.iter().all(|r| r.path.starts_with("/redirect/")),
        "{requests:?}"
    );
    // Nothing reachable at all.
    let closed = format!("http://127.0.0.1:{}/v1", free_port());
    let unreachable = test(json!({"mode":"custom","base_url":closed})).await;
    assert!(
        !unreachable.ok && unreachable.method.is_none(),
        "{unreachable:?}"
    );
    // A service that never answers ends the test before the owner's own
    // request to the daemon gives up.
    let started = Instant::now();
    let slow = test(json!({"mode":"custom","base_url":format!("{base}/slow/v1")})).await;
    assert!(
        !slow.ok && slow.message.contains("in time") && started.elapsed() < Duration::from_secs(15),
        "{slow:?} after {:?}",
        started.elapsed()
    );
    take();
    // Its own login has nothing to test.
    let login = owner
        .post::<_, Value>(&test_path, &json!({"mode":"agent_login"}))
        .await
        .unwrap_err();
    assert_eq!(login.code, ErrorCode::InvalidInput);

    // Codex speaks only the Responses API, so its minimal request goes there.
    let responses = owner
        .post::<_, service::ServiceTestResult>(
            &format!("/local/agents/{codex}/service/test"),
            &json!({"mode":"custom","base_url":format!("{base}/unlisted/v1"),"api_key":key,"model":"m-one"}),
        )
        .await
        .unwrap();
    assert!(responses.ok, "{responses:?}");
    let requests = take();
    assert_eq!(
        requests
            .iter()
            .map(|r| (r.method.as_str(), r.path.as_str()))
            .collect::<Vec<_>>(),
        [
            ("GET", "/unlisted/v1/models"),
            ("POST", "/unlisted/v1/responses")
        ]
    );
    let body: Value = serde_json::from_str(&requests[1].body).unwrap();
    assert_eq!(body["max_output_tokens"], json!(16));

    // Claude Code through a custom endpoint speaks Anthropic with a bearer token.
    let anthropic = owner
        .post::<_, service::ServiceTestResult>(
            &format!("/local/agents/{claude}/service/test"),
            &json!({"mode":"custom","base_url":format!("{base}/anthropic"),"api_key":key}),
        )
        .await
        .unwrap();
    assert!(anthropic.ok, "{anthropic:?}");
    assert_eq!(anthropic.models, ["claude-fake"]);
    let requests = take();
    assert!(
        requests[0].path.starts_with("/anthropic/v1/models"),
        "{requests:?}"
    );
    assert_eq!(requests[0].authorization, format!("Bearer {key}"));
    assert!(requests[0].api_key.is_empty());

    // A paired computer cannot test, and its attempt reaches no service.
    let as_peer =
        hmesh_transport::Client::new(&b, &a.card.endpoint, &a.card.fingerprint, false, false)
            .unwrap();
    assert_eq!(
        as_peer
            .post::<_, Value>(
                &test_path,
                &json!({"mode":"custom","base_url":format!("{base}/listed/v1")})
            )
            .await
            .unwrap_err()
            .code,
        ErrorCode::PermissionDenied
    );
    assert!(take().is_empty());
    // The audit trail says a test happened, never the key.
    let audit: Value = owner.get("/local/audit").await.unwrap();
    let audit = audit.to_string();
    assert!(
        audit.contains("agent.service.test") && !audit.contains(key),
        "{audit}"
    );
    daemon_a.stop().await;
    daemon_b.stop().await;
    for name in ["state.sqlite3", "state.sqlite3-wal"] {
        if let Ok(bytes) = std::fs::read(a.dir.join(name)) {
            assert!(!String::from_utf8_lossy(&bytes).contains(key), "{name}");
        }
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn session_settings_follow_their_opener_and_survive_restarts() {
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
    let agent = register_fixture(&b, &root.path().join("project-b"));
    let daemon_a = Daemon::start(&a.dir);
    let daemon_b = Daemon::start(&b.dir);
    daemon_a.ready().await;
    daemon_b.ready().await;
    let owner_a = daemon_a.owner();
    let owner_b = daemon_b.owner();

    // node-a opens a session on node-b's agent with a model chosen up front.
    let opened = open(
        &owner_a,
        json!({"agent_id":agent,"peer_id":b.card.node_id,"message":"fixture:config","config":{"model":"fixture-large"}}),
    )
    .await;
    let id = opened.session_id;
    let (done, _) = turn_done(&owner_a, id, 1).await;
    assert_eq!(
        done.last_final_text.as_deref(),
        Some("model=fixture-large mode=ask effort=low")
    );
    // The settings the agent offers are shown, including one the large model added.
    let ids: Vec<&str> = done.config_options.iter().map(|o| o.id.as_str()).collect();
    assert_eq!(ids, ["model", "mode", "effort"]);
    // The agent's own defaults are cached for the next new session.
    let nodes: Vec<MeshNode> = owner_a.get("/local/mesh/agents").await.unwrap();
    let cached = &nodes.iter().find(|n| !n.local).unwrap().agents[0].config_options;
    assert_eq!(cached[0].current, json!("fixture-small"));

    // The opener changes settings between turns.
    let changed = configure(&owner_a, id, "mode", json!("code"))
        .await
        .unwrap();
    assert_eq!(current(&changed, "mode"), json!("code"));
    let changed = configure(&owner_a, id, "effort", json!("high"))
        .await
        .unwrap();
    assert_eq!(current(&changed, "effort"), json!("high"));
    let bad = configure(&owner_a, id, "mode", json!("nope"))
        .await
        .unwrap_err();
    assert_eq!(bad.code, ErrorCode::InvalidInput, "{}", bad.message);
    let unknown = configure(&owner_a, id, "colour", json!("red"))
        .await
        .unwrap_err();
    assert_eq!(unknown.code, ErrorCode::InvalidInput, "{}", unknown.message);
    // node-b's owner runs the agent but did not open the session.
    let denied = configure(&owner_b, id, "mode", json!("ask"))
        .await
        .unwrap_err();
    assert_eq!(denied.code, ErrorCode::PermissionDenied);
    // Not through the peer route either.
    let bypass = owner_b
        .post::<_, SessionRecord>(
            &format!("/hmesh/v1/sessions/{id}/config"),
            &json!({"config_id":"mode","value":"ask"}),
        )
        .await
        .unwrap_err();
    assert_eq!(bypass.code, ErrorCode::PermissionDenied);
    // Nor may node-a change a session node-b's owner opened there.
    let own = open(&owner_b, json!({"agent_id":agent})).await;
    let as_peer =
        hmesh_transport::Client::new(&a, &b.card.endpoint, &b.card.fingerprint, false, false)
            .unwrap();
    let refused = as_peer
        .post::<_, SessionRecord>(
            &format!("/hmesh/v1/sessions/{}/config", own.session_id),
            &json!({"config_id":"mode","value":"code"}),
        )
        .await
        .unwrap_err();
    assert_eq!(refused.code, ErrorCode::PermissionDenied);

    // While a turn runs, settings wait.
    prompt(&owner_a, id, "fixture:wait").await;
    until(&owner_a, id, |s, _| s.status.is_busy()).await;
    let busy = configure(&owner_a, id, "mode", json!("ask"))
        .await
        .unwrap_err();
    assert_eq!(busy.code, ErrorCode::TaskConflict);
    let _ = post(&owner_a, &format!("/local/sessions/{id}/cancel")).await;
    turn_done(&owner_a, id, 2).await;

    // After node-b restarts, the agent reloads the conversation with its
    // default settings; the chosen ones are applied again.
    daemon_b.stop().await;
    let daemon_b = Daemon::start(&b.dir);
    daemon_b.ready().await;
    prompt(&owner_a, id, "fixture:config").await;
    let (again, events) = turn_done(&owner_a, id, 3).await;
    assert_eq!(
        again.last_final_text.as_deref(),
        Some("model=fixture-large mode=code effort=high")
    );
    assert_eq!(
        events.iter().filter(|e| e.kind == "config_changed").count(),
        2,
        "{events:#?}"
    );
    daemon_a.stop().await;
    daemon_b.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn model_service_settings_reach_the_agent_redacted_and_stay_on_its_computer() {
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
    let agent = register_fixture(&a, &root.path().join("project"));
    let claude = register_as(
        &a,
        &root.path().join("claude"),
        "claude",
        "claude-code",
        &[],
    );
    let daemon_a = Daemon::start(&a.dir);
    let daemon_b = Daemon::start(&b.dir);
    daemon_a.ready().await;
    daemon_b.ready().await;
    let owner = daemon_a.owner();
    let path = format!("/local/agents/{agent}/service");

    let fresh: service::AgentServiceView = owner.get(&path).await.unwrap();
    assert_eq!((fresh.mode.as_str(), fresh.modes.len()), ("agent_login", 2));
    // A generic ACP agent must declare which variable its provider reads.
    let refused = owner
        .post::<_, service::AgentServiceView>(
            &path,
            &json!({"mode":"api_key","api_key":"sk-0123456789"}),
        )
        .await
        .unwrap_err();
    assert_eq!(refused.code, ErrorCode::InvalidInput);
    let secret = "fixture-secret-0123456789";
    let view = set_service(
        &owner,
        agent,
        json!({"mode":"api_key","default_model":"fixture-large","key_variable":"FIXTURE_SERVICE_TOKEN","api_key":secret}),
    )
    .await;
    assert_eq!(view.api_key.last4.as_deref(), Some("6789"));
    let shown = serde_json::to_string(&owner.get::<Value>(&path).await.unwrap()).unwrap();
    assert!(!shown.contains(secret), "{shown}");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let file = a.dir.join("agent-secrets").join(format!("{agent}.json"));
        assert_eq!(
            std::fs::metadata(&file).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }

    // The agent process sees the value; recorded output does not.
    let first = open(
        &owner,
        json!({"agent_id":agent,"message":"fixture:env FIXTURE_SERVICE_TOKEN"}),
    )
    .await;
    let (done, events) = turn_done(&owner, first.session_id, 1).await;
    let expected = format!(
        "FIXTURE_SERVICE_TOKEN len={} fnv={:016x} value=[REDACTED]",
        secret.len(),
        fnv(secret)
    );
    assert_eq!(done.last_final_text.as_deref(), Some(expected.as_str()));
    assert!(!serde_json::to_string(&events).unwrap().contains(secret));
    // A provider can split a credential over several ACP frames and interleave
    // thoughts and replies. Both streams must stay safe before persistence.
    for ending in ["end", "error", "cancel"] {
        let split = open(
            &owner,
            json!({"agent_id":agent,"message":format!("fixture:split-key {ending}")}),
        )
        .await;
        let gather = |events: &[SessionEvent], kind: &str| -> String {
            events
                .iter()
                .filter(|e| e.kind == kind)
                .filter_map(|e| e.payload["text"].as_str())
                .collect()
        };
        let terminal = |s: &SessionRecord, events: &[SessionEvent]| {
            let kind = if ending == "error" {
                "error"
            } else {
                "turn_end"
            };
            !s.status.is_busy() && events.iter().any(|e| e.kind == kind)
        };
        let (_, events) = match ending {
            "cancel" => {
                let (_, running) = until(&owner, split.session_id, |s, ev| {
                    s.status.is_busy() && gather(ev, "agent_thought_chunk").contains("done")
                })
                .await;
                assert_eq!(
                    gather(&running, "agent_message_chunk"),
                    "answer: [REDACTED] 中🙂 "
                );
                assert_eq!(
                    gather(&running, "agent_thought_chunk"),
                    "reason: [REDACTED] done "
                );
                post(
                    &owner,
                    &format!("/local/sessions/{}/cancel", split.session_id),
                )
                .await;
                until(&owner, split.session_id, terminal).await
            }
            _ => until(&owner, split.session_id, terminal).await,
        };
        assert_eq!(
            gather(&events, "agent_message_chunk"),
            "answer: [REDACTED] 中🙂 fixture-",
            "{ending}"
        );
        assert_eq!(
            gather(&events, "agent_thought_chunk"),
            "reason: [REDACTED] done fix",
            "{ending}"
        );
        let terminal = events
            .iter()
            .position(|e| e.kind == "error" || e.kind == "turn_end")
            .unwrap();
        assert!(events[terminal..].iter().all(|e| !matches!(
            e.kind.as_str(),
            "agent_message_chunk" | "agent_thought_chunk"
        )));
    }
    // An agent error that quotes the value is redacted too.
    prompt(
        &owner,
        first.session_id,
        "fixture:fail-env FIXTURE_SERVICE_TOKEN",
    )
    .await;
    let (failed, events) = until(&owner, first.session_id, |s, e| {
        !s.status.is_busy() && e.iter().any(|e| e.kind == "error")
    })
    .await;
    let error = failed.last_error.unwrap_or_default();
    assert!(
        error.contains("[REDACTED]") && !error.contains(secret),
        "{error}"
    );
    assert!(!serde_json::to_string(&events).unwrap().contains(secret));
    // The default model applies to new sessions; an explicit choice wins.
    prompt(&owner, first.session_id, "fixture:config").await;
    let (configured, _) = turn_done(&owner, first.session_id, 2).await;
    assert_eq!(
        configured.last_final_text.as_deref(),
        Some("model=fixture-large mode=ask effort=low")
    );
    let chosen = open(
        &owner,
        json!({"agent_id":agent,"message":"fixture:config","config":{"model":"fixture-small"}}),
    )
    .await;
    let (chosen, _) = turn_done(&owner, chosen.session_id, 1).await;
    assert_eq!(
        chosen.last_final_text.as_deref(),
        Some("model=fixture-small mode=ask")
    );

    // A changed value reaches a session that is already open on its next turn.
    let rotated = "fixture-rotated-987654321";
    set_service(
        &owner,
        agent,
        json!({"mode":"agent_login","default_model":"fixture-large","env":{"FIXTURE_SERVICE_TOKEN":rotated}}),
    )
    .await;
    prompt(
        &owner,
        first.session_id,
        "fixture:env FIXTURE_SERVICE_TOKEN",
    )
    .await;
    let (after, _) = turn_done(&owner, first.session_id, 3).await;
    let after = after.last_final_text.unwrap();
    assert!(
        after.contains(&format!("fnv={:016x}", fnv(rotated))),
        "{after}"
    );
    // A model the agent does not offer is reported, and the agent keeps its own.
    set_service(
        &owner,
        agent,
        json!({"mode":"agent_login","default_model":"no-such-model","env":{"FIXTURE_SERVICE_TOKEN":null}}),
    )
    .await;
    let missing = open(&owner, json!({"agent_id":agent,"message":"fixture:config"})).await;
    let (missing, events) = turn_done(&owner, missing.session_id, 1).await;
    assert_eq!(
        missing.last_final_text.as_deref(),
        Some("model=fixture-small mode=ask")
    );
    assert!(
        events.iter().any(|e| e.kind == "status"
            && e.payload["message"]
                .as_str()
                .is_some_and(|m| m.contains("did not accept the setting model"))),
        "{events:#?}"
    );

    // Claude Code receives its key in its own variable.
    let key = "sk-ant-fixture-0123456789";
    let claude_view = set_service(&owner, claude, json!({"mode":"api_key","api_key":key})).await;
    assert_eq!(claude_view.injected, ["ANTHROPIC_API_KEY"]);
    let asked = open(
        &owner,
        json!({"agent_id":claude,"message":"fixture:env ANTHROPIC_API_KEY"}),
    )
    .await;
    let (asked, _) = turn_done(&owner, asked.session_id, 1).await;
    assert_eq!(
        asked.last_final_text,
        Some(format!(
            "ANTHROPIC_API_KEY len={} fnv={:016x} value=[REDACTED]",
            key.len(),
            fnv(key)
        ))
    );

    // A paired computer sees that a service is configured, never the values,
    // and cannot read or change them.
    let nodes: Vec<MeshNode> = daemon_b.owner().get("/local/mesh/agents").await.unwrap();
    let listed = serde_json::to_string(&nodes).unwrap();
    assert!(
        !listed.contains(key) && !listed.contains(rotated),
        "{listed}"
    );
    let remote = nodes.iter().find(|n| !n.local).unwrap();
    let card = remote.agents.iter().find(|c| c.agent_id == claude).unwrap();
    assert!(card.service_configured);
    let as_peer =
        hmesh_transport::Client::new(&b, &a.card.endpoint, &a.card.fingerprint, false, false)
            .unwrap();
    assert!(as_peer.get::<Value>(&path).await.is_err());
    assert!(
        as_peer
            .post::<_, Value>(&path, &json!({"mode":"agent_login"}))
            .await
            .is_err()
    );
    daemon_a.stop().await;
    daemon_b.stop().await;
    // Nothing the daemon stored holds the values.
    for name in ["state.sqlite3", "state.sqlite3-wal"] {
        if let Ok(bytes) = std::fs::read(a.dir.join(name)) {
            let text = String::from_utf8_lossy(&bytes);
            assert!(!text.contains(secret) && !text.contains(key), "{name}");
        }
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_turn_waits_while_a_setting_changes() {
    let root = tempfile::tempdir().unwrap();
    let a = init(root.path(), "solo");
    let agent = register_with_args(
        &a,
        &root.path().join("project"),
        "slow-settings",
        &["--config-delay-ms", "1500"],
    );
    let daemon = Daemon::start(&a.dir);
    daemon.ready().await;
    let owner = daemon.owner();
    let session = open(
        &owner,
        json!({"agent_id":agent,"message":"fixture:echo one"}),
    )
    .await;
    let id = session.session_id;
    turn_done(&owner, id, 1).await;
    // While the agent is switching, a new message and another change wait.
    let switching = {
        let owner = daemon.owner();
        tokio::spawn(async move { configure(&owner, id, "mode", json!("code")).await })
    };
    tokio::time::sleep(Duration::from_millis(400)).await;
    let early = owner
        .post::<_, SessionRecord>(
            &format!("/local/sessions/{id}/prompt"),
            &json!({"message":"fixture:config"}),
        )
        .await
        .unwrap_err();
    assert_eq!(early.code, ErrorCode::TaskConflict, "{}", early.message);
    let second = configure(&owner, id, "model", json!("fixture-large"))
        .await
        .unwrap_err();
    assert_eq!(second.code, ErrorCode::TaskConflict);
    let switched = switching.await.unwrap().unwrap();
    assert_eq!(current(&switched, "mode"), json!("code"));
    // Afterwards the turn runs with the new setting.
    prompt(&owner, id, "fixture:config").await;
    let (done, _) = turn_done(&owner, id, 2).await;
    assert_eq!(
        done.last_final_text.as_deref(),
        Some("model=fixture-small mode=code")
    );
    daemon.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn an_agents_own_login_is_checked_on_its_computer_and_peers_see_only_whether_it_is_set_up() {
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
    let claude = register_as(
        &a,
        &root.path().join("claude"),
        "claude",
        "claude-code",
        &["--auth-status", "subscription"],
    );
    let logged_out = register_as(
        &a,
        &root.path().join("out"),
        "out",
        "claude-code",
        &["--auth-status", "none"],
    );
    let silent = register_fixture(&a, &root.path().join("project"));
    let codex = register_as(&a, &root.path().join("codex"), "codex", "codex", &[]);
    let pushed = register_as(
        &a,
        &root.path().join("pushed"),
        "pushed",
        "claude-code",
        &["--auth-push", "subscription"],
    );
    let daemon_a = Daemon::start(&a.dir);
    let daemon_b = Daemon::start(&b.dir);
    daemon_a.ready().await;
    daemon_b.ready().await;
    let owner = daemon_a.owner();
    let check = |agent: Uuid| {
        let owner = &owner;
        async move {
            owner
                .post::<_, service::AgentServiceView>(
                    &format!("/local/agents/{agent}/service/check"),
                    &json!({}),
                )
                .await
                .unwrap()
                .own_login
                .unwrap()
        }
    };

    // The agent reports its own login over ACP; HarnessMesh reads no files.
    let own = check(claude).await;
    assert_eq!(
        (
            own.state.as_str(),
            own.kind.as_deref(),
            own.label.as_deref(),
            own.account.as_deref(),
            own.source.as_str()
        ),
        (
            "configured",
            Some("subscription"),
            Some("Claude subscription"),
            Some("fixture@example.com"),
            "acp auth/status"
        )
    );
    assert_eq!(check(logged_out).await.state, "not_configured");
    assert_eq!(check(silent).await.state, "unknown");
    // An agent that only pushes its identity (claude-agent-acp's extension).
    let push = check(pushed).await;
    assert_eq!(
        (
            push.state.as_str(),
            push.kind.as_deref(),
            push.account.as_deref(),
            push.detail.as_deref()
        ),
        (
            "configured",
            Some("subscription"),
            Some("pushed@example.com"),
            Some("pro")
        )
    );
    // The last check is kept on this computer and shown with the settings.
    let view: service::AgentServiceView = owner
        .get(&format!("/local/agents/{claude}/service"))
        .await
        .unwrap();
    assert_eq!(
        view.own_login.as_ref().map(|o| o.state.as_str()),
        Some("configured")
    );
    assert!(
        !view.presets.is_empty()
            && view
                .presets
                .iter()
                .all(|p| p.base_url.contains("/anthropic"))
    );

    // A paired computer learns only whether it is set up.
    let nodes: Vec<MeshNode> = daemon_b.owner().get("/local/mesh/agents").await.unwrap();
    let listed = serde_json::to_string(&nodes).unwrap();
    assert!(
        !listed.contains("fixture@example.com") && !listed.contains("Claude subscription"),
        "{listed}"
    );
    let remote = nodes.iter().find(|n| !n.local).unwrap();
    let state = |id: Uuid| {
        remote
            .agents
            .iter()
            .find(|c| c.agent_id == id)
            .unwrap()
            .own_login
            .clone()
    };
    assert_eq!(state(claude).as_deref(), Some("configured"));
    assert_eq!(state(logged_out).as_deref(), Some("not_configured"));
    let as_peer =
        hmesh_transport::Client::new(&b, &a.card.endpoint, &a.card.fingerprint, false, false)
            .unwrap();
    assert_eq!(
        as_peer
            .post::<_, Value>(&format!("/local/agents/{claude}/service/check"), &json!({}))
            .await
            .unwrap_err()
            .code,
        ErrorCode::PermissionDenied
    );

    // Claude Code on a custom endpoint gets the chosen model in every tier
    // variable, and the model is not also asked for over ACP.
    set_service(
        &owner,
        claude,
        json!({"mode":"custom","base_url":"https://api.deepseek.com/anthropic","api_key":"sk-fixture-0123456789","default_model":"deepseek-v4-pro"}),
    )
    .await;
    let asked = open(
        &owner,
        json!({"agent_id":claude,"message":"fixture:env ANTHROPIC_DEFAULT_HAIKU_MODEL"}),
    )
    .await;
    let (asked, events) = turn_done(&owner, asked.session_id, 1).await;
    assert!(
        asked
            .last_final_text
            .as_deref()
            .is_some_and(|t| t.ends_with("value=deepseek-v4-pro")),
        "{:?}",
        asked.last_final_text
    );
    assert!(
        !events.iter().any(|e| e.payload["message"]
            .as_str()
            .is_some_and(|m| m.contains("did not accept"))),
        "{events:#?}"
    );

    // A key read from the daemon's environment reaches the agent, redacted,
    // and is never stored.
    let from_env = std::env::var("CARGO_MANIFEST_DIR").unwrap();
    let view = set_service(
        &owner,
        codex,
        json!({"mode":"api_key","api_key_env":"CARGO_MANIFEST_DIR"}),
    )
    .await;
    assert_eq!(
        (view.api_key_env.as_deref(), view.api_key.set),
        (Some("CARGO_MANIFEST_DIR"), false)
    );
    let session = open(
        &owner,
        json!({"agent_id":codex,"message":"fixture:env OPENAI_API_KEY"}),
    )
    .await;
    let (done, _) = turn_done(&owner, session.session_id, 1).await;
    assert_eq!(
        done.last_final_text,
        Some(format!(
            "OPENAI_API_KEY len={} fnv={:016x} value=[REDACTED]",
            from_env.len(),
            fnv(&from_env)
        ))
    );
    let stored =
        std::fs::read_to_string(a.dir.join("agent-secrets").join(format!("{codex}.json"))).unwrap();
    assert!(
        stored.contains("CARGO_MANIFEST_DIR") && !stored.contains(&from_env),
        "{stored}"
    );
    // A missing variable stops the agent from starting and says which.
    set_service(
        &owner,
        codex,
        json!({"mode":"api_key","api_key_env":"NOT_SET_ANYWHERE_0123"}),
    )
    .await;
    let missing = open(&owner, json!({"agent_id":codex,"message":"fixture:reply"})).await;
    let (missing, _) = until(&owner, missing.session_id, |s, _| !s.status.is_busy()).await;
    assert!(
        missing
            .last_error
            .as_deref()
            .is_some_and(|e| e.contains("NOT_SET_ANYWHERE_0123")),
        "{:?}",
        missing.last_error
    );
    daemon_a.stop().await;
    daemon_b.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn advertised_commands_follow_the_approved_session_and_live_process() {
    let root = tempfile::tempdir().unwrap();
    let a = init(root.path(), "a");
    let b = init(root.path(), "b");
    Store::open(&a.dir.join("state.sqlite3"))
        .unwrap()
        .trust_peer(&b.card)
        .unwrap();
    Store::open(&b.dir.join("state.sqlite3"))
        .unwrap()
        .trust_peer(&a.card)
        .unwrap();
    // Like Pi/OpenCode, this fixture returns from session/new before
    // announcing its commands; the first discovery request must see them.
    let agent = register_with_args(
        &b,
        &root.path().join("project"),
        "fixture",
        &["--commands-delay-ms", "180"],
    );
    let da = Daemon::start(&a.dir);
    let db = Daemon::start(&b.dir);
    da.ready().await;
    db.ready().await;
    let owner = da.owner();
    db.owner()
        .post::<_, Value>(
            &format!("/local/peers/{}/access", a.card.node_id),
            &json!({"access":"confirm"}),
        )
        .await
        .unwrap();
    let record = open(&owner, json!({"agent_id":agent,"peer_id":b.card.node_id})).await;
    let id = record.session_id;
    let commands = format!("/local/sessions/{id}/commands");
    let command = format!("/local/sessions/{id}/command");
    let list = || owner.post::<_, SessionCommands>(&commands, &Value::Null);
    let compact = json!({"name":"compact","arguments":"keep tests"});
    assert_eq!(
        list().await.unwrap().state,
        CommandListState::PendingApproval
    );
    assert_eq!(
        owner
            .post::<_, Value>(&command, &compact)
            .await
            .unwrap_err()
            .code,
        ErrorCode::ApprovalRequired
    );
    post(&db.owner(), &format!("/local/sessions/{id}/approve")).await;
    assert_eq!(
        owner.get::<SessionCommands>(&commands).await.unwrap().state,
        CommandListState::Inactive
    );
    let catalog = list().await.unwrap();
    assert_eq!(catalog.state, CommandListState::Ready);
    assert!(
        catalog
            .config_options
            .iter()
            .any(|o| o.is_category("model"))
    );
    assert!(
        catalog
            .commands
            .iter()
            .any(|c| c.name == "compact" && c.input_hint.as_deref() == Some("instructions"))
    );
    let ready = page(&owner, id, 0, 0).await;
    assert_eq!(ready.session.turns, 0);
    assert!(!kinds(&ready.events).contains(&"user_message"));
    assert!(
        owner
            .post::<_, Value>(&command, &json!({"name":"typo"}))
            .await
            .is_err()
    );
    owner.post::<_, Value>(&command, &compact).await.unwrap();
    let (done, events) = turn_done(&owner, id, 1).await;
    assert_eq!(
        done.last_final_text.as_deref(),
        Some("Fixture compacted: keep tests")
    );
    assert_eq!(
        kinds(&events)
            .iter()
            .filter(|k| **k == "user_message")
            .count(),
        1
    );
    prompt(&owner, id, "fixture:clear-commands").await;
    turn_done(&owner, id, 2).await;
    let cleared = owner.get::<SessionCommands>(&commands).await.unwrap();
    assert_eq!(cleared.state, CommandListState::Ready);
    assert!(cleared.commands.is_empty());
    assert!(owner.post::<_, Value>(&command, &compact).await.is_err());
    db.stop().await;
    let db = Daemon::start(&b.dir);
    db.ready().await;
    assert_eq!(
        owner.get::<SessionCommands>(&commands).await.unwrap().state,
        CommandListState::Inactive
    );
    assert!(owner.post::<_, Value>(&command, &compact).await.is_err());
    assert_eq!(list().await.unwrap().state, CommandListState::Ready);
    post(&owner, &format!("/local/sessions/{id}/close")).await;
    assert_eq!(list().await.unwrap().state, CommandListState::Closed);
    da.stop().await;
    db.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn tool_calls_keep_their_output_diffs_and_plans_however_the_agent_reports_them() {
    let root = tempfile::tempdir().unwrap();
    let a = init(root.path(), "solo");
    let agent = register_named(&a, &root.path().join("project"), "fixture");
    let daemon = Daemon::start(&a.dir);
    daemon.ready().await;
    let owner = daemon.owner();
    let asked = open(&owner, json!({"agent_id":agent,"message":"fixture:stream"})).await;
    let (done, events) = turn_done(&owner, asked.session_id, 1).await;
    assert!(done.last_final_text.unwrap().starts_with("## Fixed"));
    let of = |id: &str| -> Vec<&Value> {
        events
            .iter()
            .filter(|e| e.kind.starts_with("tool_call") && e.payload["id"] == id)
            .map(|e| &e.payload)
            .collect()
    };
    let output = |id: &str| -> String {
        of(id)
            .iter()
            .filter_map(|p| p["output"].as_str())
            .next_back()
            .unwrap_or_default()
            .to_owned()
    };
    // Command output in content text (Claude), in `_meta` (Codex, Pi) and
    // in rawOutput (MiniMax, OpenCode).
    assert!(output("claude-bash").contains("3 passed"));
    let streamed = |id: &str| -> (String, Option<i64>) {
        let calls = of(id);
        let text = calls
            .iter()
            .filter_map(|p| p["terminal"]["append"].as_str())
            .collect::<String>();
        let exit = calls
            .iter()
            .find_map(|p| p["terminal"]["exit_code"].as_i64());
        (text, exit)
    };
    assert_eq!(
        streamed("codex-run"),
        (
            "running 3 tests\ntest add_works ... FAILED\n".to_owned(),
            Some(101)
        )
    );
    assert!(of("codex-run").iter().any(|p| p["terminal"].is_object()));
    assert_eq!(
        streamed("pi-bash"),
        ("NOTES.md\nREADME.md\n".to_owned(), Some(0))
    );
    assert!(output("mm-run").contains("No rule to make target"));
    assert!(output("oc-bash").ends_with("# pass 3\n"));
    // Diffs: hunks, and a new file without old text.
    let diff = |id: &str| -> Value {
        of(id)
            .iter()
            .find_map(|p| p.get("diffs").cloned())
            .unwrap_or(Value::Null)
    };
    assert_eq!(diff("claude-edit")[0]["path"], "/work/demo/src/lib.rs");
    assert!(
        diff("claude-edit")[0]["new"]
            .as_str()
            .unwrap()
            .contains("a + b")
    );
    assert!(diff("pi-write")[0]["old"].is_null());
    assert_eq!(diff("pi-write")[0]["new"], "First line\nSecond line\n");
    // Structured arguments, whatever the kind says.
    assert!(
        of("dsh-shell")
            .iter()
            .any(|p| p["kind"] == "other" && p["args"]["command"] == "git status --short")
    );
    // Kimi's repeated arguments are not mistaken for output.
    assert!(
        of("kimi-grep")
            .iter()
            .all(|p| !p["output"].as_str().unwrap_or("").contains("\"pattern\""))
    );
    assert!(output("kimi-grep").contains("TODO: handle overflow"));
    // Each plan is the whole plan.
    let plans: Vec<&Value> = events
        .iter()
        .filter(|e| e.kind == "plan")
        .map(|e| &e.payload)
        .collect();
    assert_eq!(plans.len(), 4);
    assert!(
        plans
            .iter()
            .all(|p| p["entries"].as_array().unwrap().len() == 3)
    );
}
