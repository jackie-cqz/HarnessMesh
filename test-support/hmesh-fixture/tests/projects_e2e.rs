//! Projects and the sessions agents already keep: listing them through ACP
//! `session/list`, continuing one with its history, working in another
//! folder, this computer's marks, and projects shared with a paired computer.
//! Uses the fixture ACP agent; never calls a model.
use hmesh_core::projects::*;
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
fn register(instance: &Instance, project: &Path, name: &str, args: &[&str]) -> Uuid {
    std::fs::create_dir_all(project).unwrap();
    let project_id = format!("demo-{name}");
    let config: AgentConfig = serde_json::from_value(json!({
        "agent":{"name":name,"harness":"acp","project_id":project_id},
        "project":{"root":project,"allowed_tools":["read","write","edit"]},
        "adapter":{"executable":env!("CARGO_BIN_EXE_hmesh-fixture"),"args":args,"provider":"fixture","model":"fixture"}
    }))
    .unwrap();
    let agent_id = Uuid::new_v4();
    let card: AgentCard = serde_json::from_value(json!({
        "agent_id":agent_id,"node_id":instance.card.node_id,"name":name,"harness":"acp","project_ids":[project_id],"availability":"idle","capabilities":[],
        "probe":{"installed":true,"version":"test-fixture","version_supported":true,"protocol_probe":"fixture","authentication":"not_applicable_test_fixture","managed_run":"passed","streaming":"fixture","cancel":"not_checked","session_resume":"not_checked","unavailable_reason":null}
    }))
    .unwrap();
    Store::open(&instance.dir.join("state.sqlite3"))
        .unwrap()
        .register_agent(&card, &config)
        .unwrap();
    agent_id
}
/// A session the agent kept before HarnessMesh saw it, in the fixture's store.
fn seed(state: &Path, id: &str, cwd: &Path, history: &[(&str, &str)]) {
    std::fs::create_dir_all(state).unwrap();
    let history: Vec<Value> = history.iter().map(|(r, t)| json!([r, t])).collect();
    std::fs::write(
        state.join(format!("{id}.json")),
        json!({"cwd":cwd,"history":history}).to_string(),
    )
    .unwrap();
}
fn real(path: &Path) -> PathBuf {
    std::fs::create_dir_all(path).unwrap();
    std::fs::canonicalize(path).unwrap()
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
type Owner = hmesh_transport::Client;

async fn page(owner: &Owner, id: Uuid, after: u64, wait: u64) -> SessionEventPage {
    owner
        .get(&format!(
            "/local/sessions/{id}/events?after={after}&wait={wait}"
        ))
        .await
        .unwrap()
}
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
async fn idle_after(owner: &Owner, id: Uuid, turns: u64) -> (SessionRecord, Vec<SessionEvent>) {
    until(owner, id, |s, _| {
        s.turns >= turns && !s.status.is_busy() || s.status == SessionStatus::Closed
    })
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
fn texts<'a>(events: &'a [SessionEvent], kind: &str) -> Vec<&'a str> {
    events
        .iter()
        .filter(|e| e.kind == kind)
        .filter_map(|e| e.payload["text"].as_str())
        .collect()
}
async fn initialized(dir: &Path) -> Protocol<OwnerBackend> {
    let mut mcp = Protocol::new(OwnerBackend::load(dir).unwrap());
    mcp.handle(json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-06-18","capabilities":{},"clientInfo":{"name":"e2e","version":"1"}}}))
        .await
        .unwrap();
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
async fn earlier_sessions_are_listed_per_folder_and_continue_with_their_history() {
    unsafe { std::env::set_var("HMESH_MCP_COMMAND", env!("CARGO_BIN_EXE_hmesh-fixture")) };
    let root = tempfile::tempdir().unwrap();
    let a = init(root.path(), "node-a");
    let home = real(&root.path().join("home"));
    let web = real(&root.path().join("work/web"));
    let api = real(&root.path().join("work/api"));
    let state = root.path().join("fixture-state");
    let state_arg = state.to_str().unwrap();
    // Sessions the agent already had, in two folders. Paths as the agent
    // reports them may differ from the resolved folder (e.g. /tmp on macOS):
    // `web/.` resolves to the same folder.
    seed(
        &state,
        "native-login",
        &web.join("."),
        &[
            ("user", "Fix the login bug"),
            ("agent", "Fixed the token check."),
        ],
    );
    seed(
        &state,
        "native-readme",
        &web,
        &[("user", "Rewrite the README"), ("agent", "Done.")],
    );
    seed(
        &state,
        "native-api",
        &api,
        &[("user", "Add /health"), ("agent", "Added.")],
    );
    let agent = register(
        &a,
        &home,
        "fixture",
        &["--state-dir", state_arg, "--list-page-size", "2"],
    );
    let quiet = register(
        &a,
        &root.path().join("quiet"),
        "quiet",
        &["--no-list", "--state-dir", state_arg],
    );
    let daemon = Daemon::start(&a.dir);
    daemon.ready().await;
    let owner = daemon.owner();

    // Each agent's registered folder is a project already.
    let projects: Vec<Project> = owner.get("/local/projects").await.unwrap();
    let own = projects.iter().find(|p| p.path == home).unwrap();
    assert_eq!(
        (
            own.source,
            own.project_id.as_str(),
            own.agent_ids.as_slice()
        ),
        (ProjectSource::Agent, "demo-fixture", [agent].as_slice())
    );

    // Adding a folder: absolute, existing, once.
    for bad in [
        json!({"path":"work/web"}),
        json!({"path":root.path().join("missing")}),
        json!({"path":home}),
    ] {
        assert!(
            owner
                .post::<_, Project>("/local/projects", &bad)
                .await
                .is_err(),
            "{bad}"
        );
    }
    let web_project: Project = owner
        .post(
            "/local/projects",
            &json!({"path":root.path().join("work/../work/web")}),
        )
        .await
        .unwrap();
    assert_eq!(
        (web_project.path.as_path(), web_project.name.as_str()),
        (web.as_path(), "web")
    );
    let again = owner
        .post::<_, Project>("/local/projects", &json!({"path":web}))
        .await
        .unwrap_err();
    assert_eq!(again.code, ErrorCode::TaskConflict);

    // Folders in the agents' histories, for adding projects. Three sessions
    // in pages of two; the agent that cannot list says so.
    let found: HistoryFolders = owner.get("/local/history").await.unwrap();
    let folder = |path: &Path| found.folders.iter().find(|f| f.path == path).unwrap();
    assert_eq!(folder(&web).sessions, 2);
    assert_eq!(
        folder(&web).project_id.as_deref(),
        Some(web_project.project_id.as_str())
    );
    assert_eq!(folder(&api).sessions, 1);
    assert_eq!(folder(&api).project_id, None);
    let state_of = |id: Uuid| {
        found
            .agents
            .iter()
            .find(|h| h.agent_id == id)
            .unwrap()
            .state
    };
    assert_eq!(state_of(agent), HistoryState::Listed);
    assert_eq!(state_of(quiet), HistoryState::Unsupported);

    // One project's earlier sessions, from the agents that can list them.
    let history: ProjectHistory = owner
        .get(&format!(
            "/local/projects/{}/history",
            web_project.project_id
        ))
        .await
        .unwrap();
    let listed = &history
        .agents
        .iter()
        .find(|h| h.agent_id == agent)
        .unwrap()
        .sessions;
    let mut ids: Vec<&str> = listed.iter().map(|s| s.agent_session_id.as_str()).collect();
    ids.sort();
    assert_eq!(ids, ["native-login", "native-readme"]);
    assert!(
        listed
            .iter()
            .all(|s| s.cwd == web && s.session_id.is_none())
    );
    let login = listed
        .iter()
        .find(|s| s.agent_session_id == "native-login")
        .unwrap();
    assert_eq!(login.title.as_deref(), Some("Fix the login bug"));

    // Continuing one shows its conversation, once, then carries on in its folder.
    let opened: SessionRecord = owner
        .post(
            "/local/sessions",
            &json!({"agent_id":agent,"project_id":web_project.project_id,"resume":"native-login","title":"Fix the login bug"}),
        )
        .await
        .unwrap();
    assert_eq!(opened.status, SessionStatus::Running);
    assert_eq!(opened.cwd.as_deref(), Some(web.as_path()));
    assert_eq!(opened.project_id, web_project.project_id);
    assert_eq!(opened.title, "Fix the login bug");
    let (loaded, events) = until(&owner, opened.session_id, |s, _| {
        s.status == SessionStatus::Idle
    })
    .await;
    assert!(!loaded.history_pending);
    assert_eq!(texts(&events, "user_message"), ["Fix the login bug"]);
    assert!(
        events
            .iter()
            .any(|e| e.kind == "user_message" && e.payload["from"]["replayed"] == true)
    );
    assert_eq!(
        texts(&events, "agent_message_chunk").concat(),
        "Fixed the token check."
    );
    assert!(
        events
            .iter()
            .any(|e| e.payload["message"] == "continued a session the agent already had"),
        "{events:#?}"
    );
    // The same earlier session is not continued twice.
    let twice: SessionRecord = owner
        .post(
            "/local/sessions",
            &json!({"agent_id":agent,"project_id":web_project.project_id,"resume":"native-login"}),
        )
        .await
        .unwrap();
    assert_eq!(twice.session_id, opened.session_id);
    let linked: ProjectHistory = owner
        .get(&format!(
            "/local/projects/{}/history?refresh=true",
            web_project.project_id
        ))
        .await
        .unwrap();
    assert!(
        linked.agents[0].sessions.iter().any(
            |s| s.agent_session_id == "native-login" && s.session_id == Some(opened.session_id)
        )
    );

    prompt(&owner, opened.session_id, "fixture:cwd").await;
    let (record, events) = idle_after(&owner, opened.session_id, 1).await;
    let expected = format!("cwd={} process={}", web.display(), web.display());
    assert_eq!(record.last_final_text.as_deref(), Some(expected.as_str()));
    // The replayed answer is history, not this turn's reply; it stays single.
    assert_eq!(
        texts(&events, "user_message"),
        ["Fix the login bug", "fixture:cwd"]
    );

    // After a restart the agent reloads quietly: no second copy of the history.
    daemon.stop().await;
    // Even if the daemon had stopped before it noted that the replay was
    // kept, the history already recorded is not recorded again.
    {
        let store = Store::open(&a.dir.join("state.sqlite3")).unwrap();
        let mut record = store.session(opened.session_id).unwrap();
        record.history_pending = true;
        store.update_session(&record).unwrap();
    }
    let daemon = Daemon::start(&a.dir);
    daemon.ready().await;
    let owner = daemon.owner();
    prompt(&owner, opened.session_id, "fixture:echo still here").await;
    let (_, events) = idle_after(&owner, opened.session_id, 2).await;
    assert_eq!(
        texts(&events, "user_message"),
        [
            "Fix the login bug",
            "fixture:cwd",
            "fixture:echo still here"
        ]
    );
    // Nor are the agent's replayed answers recorded again as new output.
    let said = texts(&events, "agent_message_chunk").concat();
    assert_eq!(said.matches("Fixed the token check.").count(), 1, "{said}");
    assert_eq!(said.matches("cwd=").count(), 1, "{said}");

    // An earlier session whose first reloads fail: once before the agent
    // replays anything, once after it replayed the history. Neither records
    // any of it; the reload that succeeds records the history once, before
    // the message that triggered it.
    seed(
        &state,
        "native-flaky",
        &web,
        &[
            ("user", "Add a logout button"),
            ("agent", "Added the button."),
        ],
    );
    let saved = state.join("native-flaky.json");
    let aside = state.join("native-flaky.aside");
    std::fs::rename(&saved, &aside).unwrap();
    let flaky: SessionRecord = owner
        .post(
            "/local/sessions",
            &json!({"agent_id":agent,"project_id":web_project.project_id,"resume":"native-flaky"}),
        )
        .await
        .unwrap();
    let (failed, _) = until(&owner, flaky.session_id, |s, _| {
        s.status == SessionStatus::Idle
    })
    .await;
    assert!(
        failed.history_pending && failed.last_error.is_some(),
        "{failed:?}"
    );
    std::fs::rename(&aside, &saved).unwrap();
    let stop = state.join("native-flaky.fail-after-replay");
    std::fs::write(&stop, "").unwrap();
    prompt(&owner, flaky.session_id, "fixture:echo one").await;
    let (failed, events) = idle_after(&owner, flaky.session_id, 0).await;
    assert!(
        failed.history_pending && failed.last_error.is_some(),
        "{failed:?}"
    );
    assert!(
        events.iter().all(|e| e.payload.get("replayed").is_none()),
        "{events:#?}"
    );
    assert_eq!(texts(&events, "user_message"), ["fixture:echo one"]);
    std::fs::remove_file(&stop).unwrap();
    prompt(&owner, flaky.session_id, "fixture:echo two").await;
    let (continued, events) = idle_after(&owner, flaky.session_id, 1).await;
    assert!(!continued.history_pending, "{continued:?}");
    assert_eq!(
        texts(&events, "user_message"),
        [
            "fixture:echo one",
            "Add a logout button",
            "fixture:echo two"
        ]
    );
    let said = texts(&events, "agent_message_chunk").concat();
    assert_eq!(said.matches("Added the button.").count(), 1, "{said}");
    // Every replayed event is marked, and only those.
    let replayed: Vec<&str> = events
        .iter()
        .filter(|e| e.payload["replayed"] == true)
        .filter_map(|e| e.payload["text"].as_str())
        .collect();
    assert_eq!(replayed.concat(), "Add a logout buttonAdded the button.");
    // Had the daemon stopped right after recording that history, before it
    // noted it as kept, the next reload still finds it: the first message
    // here is not a replayed one, yet nothing is recorded twice.
    daemon.stop().await;
    {
        let store = Store::open(&a.dir.join("state.sqlite3")).unwrap();
        let mut record = store.session(flaky.session_id).unwrap();
        record.history_pending = true;
        store.update_session(&record).unwrap();
    }
    let mut daemon = Daemon::start(&a.dir);
    daemon.ready().await;
    let owner = daemon.owner();
    prompt(&owner, flaky.session_id, "fixture:echo three").await;
    let (_, events) = idle_after(&owner, flaky.session_id, 2).await;
    assert_eq!(
        texts(&events, "user_message"),
        [
            "fixture:echo one",
            "Add a logout button",
            "fixture:echo two",
            "fixture:echo three"
        ]
    );
    let said = texts(&events, "agent_message_chunk").concat();
    assert_eq!(said.matches("Added the button.").count(), 1, "{said}");

    // A message sent while the agent is still starting is kept whatever
    // happens to that start: the session closes, or HarnessMesh restarts.
    for restart in [false, true] {
        let id = format!("native-held-{restart}");
        seed(
            &state,
            &id,
            &web,
            &[("user", "Earlier question"), ("agent", "Earlier answer.")],
        );
        let saved = state.join(format!("{id}.json"));
        let aside = state.join(format!("{id}.aside"));
        let hold = state.join(format!("{id}.hold-load"));
        // The first continue fails: the agent cannot find the session yet.
        std::fs::rename(&saved, &aside).unwrap();
        let owner = daemon.owner();
        let held: SessionRecord = owner
            .post(
                "/local/sessions",
                &json!({"agent_id":agent,"project_id":web_project.project_id,"resume":id}),
            )
            .await
            .unwrap();
        until(&owner, held.session_id, |s, _| {
            s.status == SessionStatus::Idle
        })
        .await;
        std::fs::rename(&aside, &saved).unwrap();
        if restart {
            // HarnessMesh stopped abruptly while that start waited on the
            // agent: the message is stored as queued, the turn still running.
            daemon.stop().await;
            {
                let store = Store::open(&a.dir.join("state.sqlite3")).unwrap();
                store
                    .append_session_event(
                        held.session_id,
                        hmesh_store::QUEUED_MESSAGE,
                        &json!({"text":"fixture:echo kept","from":{"node_id":a.card.node_id}}),
                    )
                    .unwrap();
                let mut record = store.session(held.session_id).unwrap();
                record.status = SessionStatus::Running;
                store.update_session(&record).unwrap();
            }
            daemon = Daemon::start(&a.dir);
            daemon.ready().await;
        } else {
            // The next start waits on the agent, with a message sent, and the
            // session is closed meanwhile.
            std::fs::write(&hold, "").unwrap();
            prompt(&owner, held.session_id, "fixture:echo kept").await;
            let _: SessionRecord = owner
                .post(
                    &format!("/local/sessions/{}/close", held.session_id),
                    &json!({}),
                )
                .await
                .unwrap();
            std::fs::remove_file(&hold).unwrap();
        }
        let owner = daemon.owner();
        let (_, events) = until(&owner, held.session_id, |s, _| !s.status.is_busy()).await;
        assert_eq!(
            texts(&events, "user_message"),
            ["fixture:echo kept"],
            "{events:#?}"
        );
        assert!(
            events.iter().all(|e| e.kind != hmesh_store::QUEUED_MESSAGE),
            "{events:#?}"
        );
        let shown = events
            .iter()
            .position(|e| e.kind == "user_message")
            .unwrap();
        if restart {
            // Shown before the note that the turn was interrupted, and before
            // the history the next start records.
            let note = events
                .iter()
                .position(|e| {
                    e.payload["message"]
                        .as_str()
                        .is_some_and(|m| m.contains("HarnessMesh restarted"))
                })
                .unwrap();
            assert!(shown < note, "{events:#?}");
            prompt(&owner, held.session_id, "fixture:echo after").await;
            let (_, events) = idle_after(&owner, held.session_id, 1).await;
            assert_eq!(
                texts(&events, "user_message"),
                [
                    "fixture:echo kept",
                    "Earlier question",
                    "fixture:echo after"
                ]
            );
        } else {
            let closed = events
                .iter()
                .rposition(|e| e.kind == "status" && e.payload["status"] == "closed")
                .unwrap();
            assert!(shown < closed, "{events:#?}");
        }
    }
    let owner = daemon.owner();

    // A new session in another project runs there.
    let api_project: Project = owner
        .post("/local/projects", &json!({"path":api,"name":" API "}))
        .await
        .unwrap();
    assert_eq!(api_project.name, "API");
    let fresh: SessionRecord = owner
        .post(
            "/local/sessions",
            &json!({"agent_id":agent,"project_id":api_project.project_id,"message":"fixture:cwd"}),
        )
        .await
        .unwrap();
    let (record, _) = idle_after(&owner, fresh.session_id, 1).await;
    assert_eq!(
        record.last_final_text.as_deref(),
        Some(format!("cwd={} process={}", api.display(), api.display()).as_str())
    );
    // Without a project the agent keeps its own folder, as before.
    let plain: SessionRecord = owner
        .post(
            "/local/sessions",
            &json!({"agent_id":agent,"message":"fixture:cwd"}),
        )
        .await
        .unwrap();
    assert_eq!(plain.cwd, None);
    let (record, _) = idle_after(&owner, plain.session_id, 1).await;
    assert!(
        record
            .last_final_text
            .unwrap()
            .starts_with(&format!("cwd={}", home.display()))
    );

    // Continuing an earlier session needs no message and a real ID.
    for bad in [
        json!({"agent_id":agent,"resume":"native-api","message":"hi"}),
        json!({"agent_id":agent,"resume":"has space"}),
        json!({"agent_id":agent,"project_id":"../etc","message":"hi"}),
        json!({"agent_id":agent,"project_id":"no-such","message":"hi"}),
    ] {
        assert!(
            owner
                .post::<_, SessionRecord>("/local/sessions", &bad)
                .await
                .is_err(),
            "{bad}"
        );
    }

    // This computer's marks: a name, a pin, hidden; an empty name clears it.
    let mark: SessionMark = owner
        .post(
            &format!("/local/sessions/{}/mark", opened.session_id),
            &json!({"title":"Login","pinned":true}),
        )
        .await
        .unwrap();
    assert_eq!(
        (mark.title.as_deref(), mark.pinned, mark.hidden),
        (Some("Login"), true, false)
    );
    owner
        .post::<_, SessionMark>(
            &format!("/local/sessions/{}/mark", plain.session_id),
            &json!({"hidden":true}),
        )
        .await
        .unwrap();
    let list: Value = owner.get("/local/sessions").await.unwrap();
    assert_eq!(list["marks"].as_array().unwrap().len(), 2, "{list}");
    let cleared: SessionMark = owner
        .post(
            &format!("/local/sessions/{}/mark", opened.session_id),
            &json!({"title":""}),
        )
        .await
        .unwrap();
    assert_eq!((cleared.title, cleared.pinned), (None, true));
    assert!(
        owner
            .post::<_, SessionMark>(
                &format!("/local/sessions/{}/mark", Uuid::new_v4()),
                &json!({"pinned":true})
            )
            .await
            .is_err()
    );
    // The session's own title is untouched.
    let record: SessionRecord = owner
        .get(&format!("/local/sessions/{}", opened.session_id))
        .await
        .unwrap();
    assert_eq!(record.title, "Fix the login bug");

    // Projects: rename, pin, hide, reorder, remove what was added.
    let renamed: Project = owner
        .post(
            &format!("/local/projects/{}", api_project.project_id),
            &json!({"name":"Backend","pinned":true}),
        )
        .await
        .unwrap();
    assert!(renamed.pinned && renamed.name == "Backend");
    let order = |list: Vec<Project>| list.into_iter().map(|p| p.project_id).collect::<Vec<_>>();
    let before = order(owner.get("/local/projects").await.unwrap());
    assert_eq!(before[0], api_project.project_id);
    let _: Project = owner
        .post(
            &format!("/local/projects/{}", web_project.project_id),
            &json!({"move_by":-1}),
        )
        .await
        .unwrap();
    let after = order(owner.get("/local/projects").await.unwrap());
    let position = |list: &[String], id: &str| list.iter().position(|p| p == id).unwrap();
    assert_eq!(
        position(&after, &web_project.project_id) + 1,
        position(&before, &web_project.project_id),
        "{before:?} -> {after:?}"
    );
    // A pinned project stays above the others however far one moves.
    let _: Project = owner
        .post(
            &format!("/local/projects/{}", web_project.project_id),
            &json!({"move_by":-10}),
        )
        .await
        .unwrap();
    let after = order(owner.get("/local/projects").await.unwrap());
    assert_eq!(
        &after[..2],
        [
            api_project.project_id.clone(),
            web_project.project_id.clone()
        ]
    );
    let hidden: Project = owner
        .post("/local/projects/demo-fixture", &json!({"hidden":true}))
        .await
        .unwrap();
    assert!(hidden.hidden);
    assert!(
        owner
            .post::<_, Value>("/local/projects/demo-fixture/remove", &json!({}))
            .await
            .is_err()
    );
    let _: Value = owner
        .post(
            &format!("/local/projects/{}/remove", api_project.project_id),
            &json!({}),
        )
        .await
        .unwrap();
    // The local directory offers every visible project to MCP clients.
    let mut mcp = initialized(&a.dir).await;
    let agents = call(&mut mcp, "mesh_list_agents", json!({})).await;
    let offered: Vec<&str> = agents["agents"][0]["available_projects"]
        .as_array()
        .unwrap()
        .iter()
        .map(|p| p["name"].as_str().unwrap())
        .collect();
    assert!(
        offered.contains(&"web") && !offered.contains(&"demo-fixture"),
        "{offered:?}"
    );
    daemon.stop().await;
}

/// A folder picked in the panel shows each local agent's sessions there
/// before it is a project: only that folder's, each once, the ones already
/// continued linked, and which agents cannot list.
#[tokio::test(flavor = "multi_thread")]
async fn a_folder_shows_each_agents_sessions_before_it_is_added() {
    unsafe { std::env::set_var("HMESH_MCP_COMMAND", env!("CARGO_BIN_EXE_hmesh-fixture")) };
    let root = tempfile::tempdir().unwrap();
    let a = init(root.path(), "node-a");
    let state = root.path().join("fixture-state");
    let state_arg = state.to_str().unwrap();
    let app = real(&root.path().join("work/app"));
    let other = real(&root.path().join("work/other"));
    seed(
        &state,
        "app-older",
        &app,
        &[("user", "Old question"), ("agent", "Old answer.")],
    );
    seed(
        &state,
        "app-newer",
        &app,
        &[("user", "Newer question"), ("agent", "Newer answer.")],
    );
    seed(
        &state,
        "elsewhere",
        &other,
        &[("user", "Somewhere else"), ("agent", "Yes.")],
    );
    // Two agents of one kind share their sessions; one cannot list at all.
    let first = register(
        &a,
        &root.path().join("home/first"),
        "first",
        &["--state-dir", state_arg],
    );
    register(
        &a,
        &root.path().join("home/second"),
        "second",
        &["--state-dir", state_arg],
    );
    register(
        &a,
        &root.path().join("home/quiet"),
        "quiet",
        &["--state-dir", state_arg, "--no-list"],
    );
    let daemon = Daemon::start(&a.dir);
    daemon.ready().await;
    let owner = daemon.owner();
    let folder = |path: &Path| {
        let owner = &owner;
        let query = format!("/local/history/folder?path={}&refresh=true", encode(path));
        async move { owner.get::<FolderHistory>(&query).await }
    };

    let view = folder(&app).await.unwrap();
    assert_eq!(view.path, app);
    assert_eq!(view.name, "app");
    assert_eq!(view.project_id, None);
    let names: Vec<&str> = view.agents.iter().map(|a| a.agent_name.as_str()).collect();
    assert_eq!(names, ["first", "quiet", "second"]);
    let by = |name: &str| view.agents.iter().find(|a| a.agent_name == name).unwrap();
    assert_eq!(by("first").state, HistoryState::Listed);
    let mut ids: Vec<&str> = by("first")
        .sessions
        .iter()
        .map(|s| s.agent_session_id.as_str())
        .collect();
    ids.sort();
    assert_eq!(
        ids,
        ["app-newer", "app-older"],
        "only this folder's sessions"
    );
    assert!(
        by("second").sessions.is_empty(),
        "the same sessions are shown once"
    );
    assert_eq!(by("quiet").state, HistoryState::Unsupported);
    assert!(by("quiet").sessions.is_empty());

    // The same folder through a link resolves to the same view.
    #[cfg(unix)]
    {
        let link = root.path().join("app-link");
        std::os::unix::fs::symlink(&app, &link).unwrap();
        let linked = folder(&link).await.unwrap();
        assert_eq!(linked.path, app);
        assert_eq!(
            linked
                .agents
                .iter()
                .map(|a| a.sessions.len())
                .sum::<usize>(),
            2
        );
    }

    // Only an existing folder given in full.
    for bad in [
        PathBuf::from("work/app"),
        root.path().join("missing"),
        state.join("app-older.json"),
    ] {
        let refused = folder(&bad).await.unwrap_err();
        assert_eq!(
            refused.code,
            ErrorCode::InvalidInput,
            "{bad:?}: {refused:?}"
        );
    }

    // Once added and one session continued, the view says so.
    let project: Project = owner
        .post("/local/projects", &json!({"path":app}))
        .await
        .unwrap();
    let continued: SessionRecord = owner
        .post(
            "/local/sessions",
            &json!({"agent_id":first,"project_id":project.project_id,"resume":"app-older"}),
        )
        .await
        .unwrap();
    until(&owner, continued.session_id, |s, _| {
        s.status == SessionStatus::Idle
    })
    .await;
    let view = folder(&app).await.unwrap();
    assert_eq!(
        view.project_id.as_deref(),
        Some(project.project_id.as_str())
    );
    let older = view
        .agents
        .iter()
        .flat_map(|a| &a.sessions)
        .find(|s| s.agent_session_id == "app-older")
        .unwrap();
    assert_eq!(older.session_id, Some(continued.session_id));
    daemon.stop().await;
}

/// A path as a query value.
fn encode(path: &Path) -> String {
    let mut out = String::new();
    for byte in path.to_string_lossy().bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' | b'/' => {
                out.push(byte as char)
            }
            _ => out.push_str(&format!("%{byte:02X}")),
        }
    }
    out
}

#[tokio::test(flavor = "multi_thread")]
async fn shared_projects_reach_a_paired_computer_by_id_never_by_path() {
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
    let state = root.path().join("fixture-state-b");
    seed(
        &state,
        "native-secret",
        &real(&root.path().join("b/site")),
        &[("user", "private notes"), ("agent", "ok")],
    );
    let agent_b = register(
        &b,
        &root.path().join("b/home"),
        "fixture",
        &["--state-dir", state.to_str().unwrap()],
    );
    let worker_b = register(
        &b,
        &root.path().join("b/worker"),
        "worker",
        &["--state-dir", state.to_str().unwrap()],
    );
    let site = real(&root.path().join("b/site"));
    let private = real(&root.path().join("b/private"));
    let daemon_a = Daemon::start(&a.dir);
    let daemon_b = Daemon::start(&b.dir);
    daemon_a.ready().await;
    daemon_b.ready().await;
    let owner_a = daemon_a.owner();
    let owner_b = daemon_b.owner();
    // What a folder on node-b holds is for node-b's owner only.
    {
        let endpoint = format!("https://127.0.0.1:{}", b.config.node.listen_port);
        let peer =
            hmesh_transport::Client::new(&a, &endpoint, &b.card.fingerprint, false, false).unwrap();
        let refused = peer
            .get::<Value>(&format!(
                "/local/history/folder?path={}",
                root.path().join("b/site").display()
            ))
            .await
            .unwrap_err();
        assert_eq!(refused.code, ErrorCode::PermissionDenied, "{refused:?}");
    }
    let shared: Project = owner_b
        .post("/local/projects", &json!({"path":site}))
        .await
        .unwrap();
    let kept: Project = owner_b
        .post("/local/projects", &json!({"path":private}))
        .await
        .unwrap();
    let _: Project = owner_b
        .post(
            &format!("/local/projects/{}", shared.project_id),
            &json!({"shared":true}),
        )
        .await
        .unwrap();

    // node-a sees only the shared project, by name and ID, never its folder.
    let nodes: Vec<MeshNode> = owner_a.get("/local/mesh/agents").await.unwrap();
    let node_b = nodes.iter().find(|n| n.node_id == b.card.node_id).unwrap();
    assert_eq!(
        node_b.projects,
        [SharedProject {
            project_id: shared.project_id.clone(),
            name: "site".into()
        }]
    );
    let raw: Value = owner_a.get("/local/mesh/agents").await.unwrap();
    assert!(!raw.to_string().contains(site.to_str().unwrap()));
    // A hidden project is not offered, even while it stays shared.
    for hidden in [true, false] {
        let _: Project = owner_b
            .post(
                &format!("/local/projects/{}", shared.project_id),
                &json!({"hidden":hidden}),
            )
            .await
            .unwrap();
        let nodes: Vec<MeshNode> = owner_a.get("/local/mesh/agents").await.unwrap();
        let listed = &nodes
            .iter()
            .find(|n| n.node_id == b.card.node_id)
            .unwrap()
            .projects;
        assert_eq!(listed.len(), usize::from(!hidden), "{listed:?}");
    }

    // A session in the shared project runs there; node-a never sees the path.
    let opened: SessionRecord = owner_a
        .post(
            "/local/sessions",
            &json!({"agent_id":agent_b,"peer_id":b.card.node_id,"project_id":shared.project_id,"message":"fixture:cwd"}),
        )
        .await
        .unwrap();
    assert_eq!(opened.cwd, None);
    assert_eq!(opened.project_id, shared.project_id);
    let (record, _) = idle_after(&owner_a, opened.session_id, 1).await;
    assert_eq!(record.cwd, None);
    assert!(
        record
            .last_final_text
            .unwrap()
            .starts_with(&format!("cwd={}", site.display()))
    );
    let on_b: SessionRecord = owner_b
        .get(&format!("/local/sessions/{}", opened.session_id))
        .await
        .unwrap();
    assert_eq!(on_b.cwd.as_deref(), Some(site.as_path()));
    // Running one of the agent's commands there answers with the same view
    // of the record: no folder.
    let commanded: Value = owner_a
        .post(
            &format!("/local/sessions/{}/command", opened.session_id),
            &json!({"name":"compact"}),
        )
        .await
        .unwrap();
    assert!(commanded.get("cwd").is_none(), "{commanded}");
    idle_after(&owner_a, opened.session_id, 2).await;
    // (The agent's own reply may name its folder; the records never do.)
    let mirrored: Value = owner_a.get("/local/sessions").await.unwrap();
    let remote = mirrored["remote"].as_array().unwrap();
    assert!(!remote.is_empty());
    assert!(
        remote.iter().all(|r| r["session"].get("cwd").is_none()),
        "{mirrored}"
    );

    // Not shared, earlier sessions, unknown projects: refused when the
    // session would open, before any record or agent process exists there.
    let sessions_on_b = || async {
        let list: Value = owner_b.get("/local/sessions").await.unwrap();
        list["sessions"].as_array().unwrap().len()
    };
    let before = sessions_on_b().await;
    for (body, reason) in [
        (
            json!({"agent_id":agent_b,"peer_id":b.card.node_id,"project_id":kept.project_id}),
            "not shared",
        ),
        (
            json!({"agent_id":agent_b,"peer_id":b.card.node_id,"resume":"native-secret"}),
            "owner can continue",
        ),
        (
            json!({"agent_id":agent_b,"peer_id":b.card.node_id,"project_id":"no-such","message":"hi"}),
            "no such project",
        ),
    ] {
        let refused = owner_a
            .post::<_, SessionRecord>("/local/sessions", &body)
            .await
            .unwrap_err();
        assert!(refused.message.contains(reason), "{body}: {refused:?}");
    }
    assert_eq!(sessions_on_b().await, before);
    // A chain node-a started (its session on node-b calling another agent
    // there) is held to the same rule: shared projects only.
    let child = |project: &str| json!({"agent_id":worker_b,"parent_session_id":opened.session_id,"project_id":project,"message":"fixture:echo child"});
    let refused = owner_b
        .post::<_, SessionRecord>("/local/sessions", &child(&kept.project_id))
        .await
        .unwrap_err();
    assert!(refused.message.contains("not shared"), "{refused:?}");
    let allowed: SessionRecord = owner_b
        .post("/local/sessions", &child(&shared.project_id))
        .await
        .unwrap();
    assert_eq!(allowed.cwd.as_deref(), Some(site.as_path()));
    idle_after(&owner_b, allowed.session_id, 1).await;
    // The MCP server node-b gives that session lists only shared projects,
    // so unshared names cannot travel back to node-a in an agent's reply.
    let mut bound = Protocol::new(
        hmesh_mcp::OwnerBackend::load_for_session(&b.dir, opened.session_id).unwrap(),
    );
    bound
        .handle(json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-06-18","capabilities":{},"clientInfo":{"name":"e2e","version":"1"}}}))
        .await
        .unwrap();
    let seen = call(&mut bound, "mesh_list_agents", json!({"node":"node-b"})).await;
    for agent in seen["agents"].as_array().unwrap() {
        assert_eq!(
            agent["available_projects"],
            json!([{"name":"site","project_id":shared.project_id}]),
            "{seen}"
        );
    }
    // node-b's owner still sees every visible project.
    let mut own_mcp = initialized(&b.dir).await;
    let mine = call(&mut own_mcp, "mesh_list_agents", json!({"node":"node-b"})).await;
    assert!(mine.to_string().contains(&kept.project_id), "{mine}");
    // The agent's own folder needs no sharing, and a request without a
    // project (as older computers send it) works as before.
    let own: SessionRecord = owner_a
        .post(
            "/local/sessions",
            &json!({"agent_id":agent_b,"peer_id":b.card.node_id,"project_id":"demo-fixture","message":"fixture:echo own"}),
        )
        .await
        .unwrap();
    idle_after(&owner_a, own.session_id, 1).await;
    let plain: SessionRecord = owner_a
        .post(
            "/local/sessions",
            &json!({"agent_id":agent_b,"peer_id":b.card.node_id,"message":"fixture:echo plain"}),
        )
        .await
        .unwrap();
    idle_after(&owner_a, plain.session_id, 1).await;

    // Through MCP on node-a: the project is offered and used by name.
    let mut mcp = initialized(&a.dir).await;
    let listed = call(&mut mcp, "mesh_list_agents", json!({"node":"node-b"})).await;
    assert_eq!(
        listed["agents"][0]["available_projects"],
        json!([{"name":"site","project_id":shared.project_id}])
    );
    let sent = call(
        &mut mcp,
        "mesh_send",
        json!({"agent":"node-b/fixture","project":"site","message":"fixture:cwd","wait_seconds":60}),
    )
    .await;
    assert!(
        sent["final_text"]
            .as_str()
            .unwrap()
            .starts_with(&format!("cwd={}", site.display())),
        "{sent}"
    );

    // Unsharing stops the next turn of sessions node-a opened there.
    let _: Project = owner_b
        .post(
            &format!("/local/projects/{}", shared.project_id),
            &json!({"shared":false}),
        )
        .await
        .unwrap();
    let refused = owner_a
        .post::<_, SessionRecord>(
            &format!("/local/sessions/{}/prompt", opened.session_id),
            &json!({"message":"fixture:cwd"}),
        )
        .await
        .unwrap_err();
    assert!(refused.message.contains("no longer shared"), "{refused:?}");
    // Nor can node-a start the agent there another way, e.g. to read its
    // commands.
    let refused = owner_a
        .post::<_, Value>(
            &format!("/local/sessions/{}/commands", opened.session_id),
            &json!({}),
        )
        .await
        .unwrap_err();
    assert!(refused.message.contains("no longer shared"), "{refused:?}");
    // The agent's own folder is unaffected.
    let _: SessionRecord = owner_a
        .post(
            &format!("/local/sessions/{}/prompt", own.session_id),
            &json!({"message":"fixture:echo again"}),
        )
        .await
        .unwrap();
    idle_after(&owner_a, own.session_id, 2).await;
    let nodes: Vec<MeshNode> = owner_a.get("/local/mesh/agents").await.unwrap();
    assert!(
        nodes
            .iter()
            .find(|n| n.node_id == b.card.node_id)
            .unwrap()
            .projects
            .is_empty()
    );
    daemon_a.stop().await;
    daemon_b.stop().await;
}

/// The real Claude Code adapter (`claude-agent-acp`) lists a transcript it
/// already has and replays it when HarnessMesh continues it. Opt in with
/// `HMESH_CLAUDE_ACP=/path/to/claude-agent-acp cargo test -p hmesh-fixture
/// --test projects_e2e -- --ignored real_claude`. Uses a throwaway home with a
/// written transcript: no login, and no message is sent, so no model is called.
#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs claude-agent-acp; set HMESH_CLAUDE_ACP"]
async fn real_claude_adapter_lists_and_continues_an_earlier_session() {
    let adapter = std::env::var("HMESH_CLAUDE_ACP").expect("set HMESH_CLAUDE_ACP");
    let root = tempfile::tempdir().unwrap();
    let home = real(&root.path().join("home"));
    let config_dir = home.join(".claude");
    let folder = real(&root.path().join("work/site"));
    unsafe {
        std::env::set_var("HMESH_MCP_COMMAND", env!("CARGO_BIN_EXE_hmesh-fixture"));
        std::env::set_var("HOME", &home);
        std::env::set_var("CLAUDE_CONFIG_DIR", &config_dir);
        std::env::set_var("CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC", "1");
        std::env::set_var("DISABLE_TELEMETRY", "1");
        std::env::set_var("DISABLE_AUTOUPDATER", "1");
    }
    // A transcript in Claude Code's own format, as `claude` writes it.
    let sid = Uuid::new_v4().to_string();
    let sanitized: String = folder
        .to_str()
        .unwrap()
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect();
    let dir = config_dir.join("projects").join(sanitized);
    std::fs::create_dir_all(&dir).unwrap();
    let branch_key = ["gi", "tBranch"].concat();
    let mut base = json!({"sessionId":sid,"cwd":folder,"version":"2.1.0","userType":"external","isSidechain":false});
    base[branch_key.as_str()] = json!("main");
    let ids: Vec<String> = (0..4).map(|_| Uuid::new_v4().to_string()).collect();
    let line = |i: usize, kind: &str, message: Value| {
        let mut entry = base.clone();
        entry["type"] = json!(kind);
        entry["uuid"] = json!(ids[i]);
        entry["parentUuid"] = if i == 0 {
            Value::Null
        } else {
            json!(ids[i - 1])
        };
        entry["timestamp"] = json!(format!("2026-11-20T09:0{i}:00.000Z"));
        entry["message"] = message;
        entry.to_string()
    };
    let assistant = |id: &str, text: &str| json!({"id":id,"type":"message","role":"assistant","model":"claude-sonnet-5-5","content":[{"type":"text","text":text}],"stop_reason":"end_turn","usage":{"input_tokens":1,"output_tokens":1}});
    let transcript = [
        line(
            0,
            "user",
            json!({"role":"user","content":"Explain what README.md says"}),
        ),
        line(
            1,
            "assistant",
            assistant("msg_1", "It describes the project."),
        ),
        line(
            2,
            "user",
            json!({"role":"user","content":"Now list the build steps"}),
        ),
        line(3, "assistant", assistant("msg_2", "Run cargo build.")),
    ]
    .join("\n");
    std::fs::write(dir.join(format!("{sid}.jsonl")), transcript + "\n").unwrap();

    let a = init(root.path(), "node-a");
    std::fs::create_dir_all(home.join("agent")).unwrap();
    let config: AgentConfig = serde_json::from_value(json!({
        "agent":{"name":"claude","harness":"claude-code","project_id":"agent-home"},
        "project":{"root":home.join("agent"),"allowed_tools":["read"]},
        "adapter":{"executable":adapter,"args":[]}
    }))
    .unwrap();
    let agent = Uuid::new_v4();
    let card: AgentCard = serde_json::from_value(json!({
        "agent_id":agent,"node_id":a.card.node_id,"name":"claude","harness":"claude-code","project_ids":["agent-home"],"availability":"idle","capabilities":[],
        "probe":{"installed":true,"version":"test","version_supported":true,"protocol_probe":"real","authentication":"not_checked","managed_run":"not_checked","streaming":"not_checked","cancel":"not_checked","session_resume":"not_checked","unavailable_reason":null}
    }))
    .unwrap();
    Store::open(&a.dir.join("state.sqlite3"))
        .unwrap()
        .register_agent(&card, &config)
        .unwrap();
    let daemon = Daemon::start(&a.dir);
    daemon.ready().await;
    let owner = daemon.owner();
    let project: Project = owner
        .post("/local/projects", &json!({"path":folder}))
        .await
        .unwrap();
    let history: ProjectHistory = owner
        .get(&format!("/local/projects/{}/history", project.project_id))
        .await
        .unwrap();
    let claude = &history.agents[0];
    assert_eq!(claude.state, HistoryState::Listed, "{history:?}");
    let listed = claude
        .sessions
        .iter()
        .find(|s| s.agent_session_id == sid)
        .unwrap_or_else(|| panic!("{history:?}"));
    assert_eq!(listed.title.as_deref(), Some("Explain what README.md says"));
    let opened: SessionRecord = owner
        .post(
            "/local/sessions",
            &json!({"agent_id":agent,"project_id":project.project_id,"resume":sid,"title":listed.title}),
        )
        .await
        .unwrap();
    let (record, events) = until(&owner, opened.session_id, |s, _| {
        s.status != SessionStatus::Running
    })
    .await;
    assert_eq!(record.last_error, None, "{events:#?}");
    assert_eq!(
        texts(&events, "user_message"),
        ["Explain what README.md says", "Now list the build steps"]
    );
    let replies = texts(&events, "agent_message_chunk").concat();
    assert!(
        replies.contains("It describes the project.") && replies.contains("Run cargo build."),
        "{replies}"
    );
    assert_eq!(record.turns, 0, "no message was sent");
    println!(
        "listed {} session(s); continued {sid} with {} replayed events",
        claude.sessions.len(),
        events.len()
    );
    daemon.stop().await;
}
