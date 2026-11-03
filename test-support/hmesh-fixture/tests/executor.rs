use hmesh_core::*;
use hmesh_security::Instance;
use hmesh_store::Store;
#[cfg(windows)]
use std::process::{Child, Command, Stdio};
use std::{path::PathBuf, sync::Arc, time::Duration};
use tokio::sync::watch;
use uuid::Uuid;

struct Fixture {
    _temp: tempfile::TempDir,
    instance: Arc<Instance>,
    store: Store,
    config: AgentConfig,
    agent: AgentCard,
}
impl Fixture {
    fn new() -> Self {
        Self::new_with_env(vec![])
    }
    fn new_with_env(env_allowlist: Vec<String>) -> Self {
        let temp = tempfile::tempdir().expect("tempdir");
        let mut node = NodeConfig::default();
        node.node.listen_port = 0;
        let instance = Arc::new(Instance::init(&temp.path().join("node"), node).expect("init"));
        let project = temp.path().join("project");
        std::fs::create_dir(&project).expect("project");
        let config = AgentConfig {
            agent: AgentOptions {
                name: "fixture".into(),
                harness: HarnessKind::Pi,
                project_id: "fixture".into(),
                mode: "managed".into(),
                display_capabilities: vec![],
            },
            project: ProjectOptions {
                root: project,
                allowed_tools: vec!["read".into(), "write".into(), "edit".into()],
                require_remote_task_approval: true,
                artifact_paths: vec!["out/health.json".into()],
                workspace_mode: "shared".into(),
                git_executable: None,
            },
            adapter: AdapterOptions {
                node_executable: None,
                models_file: None,
                executable: PathBuf::from(env!("CARGO_BIN_EXE_hmesh-fixture")),
                provider: Some("fixture".into()),
                model: Some("fixture".into()),
                credential_source: None,
                env_allowlist,
                mesh_server: None,
                mesh_scope: None,
            },
        };
        let agent = AgentCard {
            agent_id: Uuid::new_v4(),
            node_id: instance.card.node_id,
            name: "fixture".into(),
            harness: HarnessKind::Pi,
            project_ids: vec!["fixture".into()],
            availability: "test-fixture".into(),
            capabilities: vec![],
            probe: ProbeReport {
                installed: true,
                version: Some("test-fixture".into()),
                version_supported: true,
                protocol_probe: "fixture".into(),
                authentication: "not_applicable_test_fixture".into(),
                managed_run: "not_checked".into(),
                streaming: "not_checked".into(),
                cancel: "not_checked".into(),
                session_resume: "not_checked".into(),
                unavailable_reason: None,
            },
        };
        let store = Store::open(&instance.dir.join("state.sqlite3")).expect("store");
        store.register_agent(&agent, &config).expect("register");
        Self {
            _temp: temp,
            instance,
            store,
            config,
            agent,
        }
    }
    fn submit(&self, prompt: &str, seconds: u64) -> TaskRecord {
        self.submit_with_output(prompt, seconds, RunLimits::default().max_output_bytes)
    }
    fn submit_with_output(&self, prompt: &str, seconds: u64, output: usize) -> TaskRecord {
        let mut task = TaskSubmit::new(self.agent.agent_id, "fixture", prompt);
        task.limits.timeout_seconds = seconds;
        task.limits.max_output_bytes = output;
        let envelope = Envelope::new(self.instance.card.node_id, "task.submit", task);
        self.store
            .admit(self.instance.card.node_id, &envelope, false, 20)
            .expect("admit")
    }
    fn executor(&self) -> (watch::Sender<bool>, tokio::task::JoinHandle<()>) {
        let (sender, receiver) = watch::channel(false);
        let job = tokio::spawn(hmesh_runtime::run_queue(
            self.store.clone(),
            self.instance.dir.clone(),
            RuntimeOptions::default(),
            Arc::new(|_| Ok(())),
            receiver,
        ));
        (sender, job)
    }
    async fn terminal(&self, id: Uuid) -> TaskRecord {
        for _ in 0..160 {
            let task = self.store.task(id).expect("task");
            if task.status.is_terminal() {
                return task;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        panic!("fixture task did not reach a terminal state");
    }
}
#[tokio::test]
async fn completed_snapshot_does_not_follow_later_project_edits_or_new_grants() {
    let f = Fixture::new();
    let task = f.submit("fixture:create", 5);
    let (stop, job) = f.executor();
    let terminal = f.terminal(task.task_id).await;
    assert_eq!(terminal.status, TaskStatus::Succeeded);
    let set = f.store.artifact_set(task.task_id).unwrap().unwrap();
    assert_eq!(Some(set.attempt_id), terminal.last_attempt_id);
    let bytes = set.entries[0].bytes.as_ref().unwrap();
    std::fs::write(
        f.config.project.root.join("out/health.json"),
        br#"{"status":"changed"}"#,
    )
    .unwrap();
    let evidence = hmesh_runtime::verification::verify_bytes(
        bytes,
        serde_json::json!({}),
        &serde_json::json!({"status":"ok"}),
    )
    .unwrap();
    assert_eq!(evidence["passed"], true);
    f.store
        .set_artifact_paths(f.agent.agent_id, &["later.json".into()])
        .unwrap();
    assert_eq!(
        f.store.artifact_set(task.task_id).unwrap().unwrap().entries[0]
            .bytes
            .as_ref(),
        Some(bytes)
    );
    stop.send(true).unwrap();
    job.await.unwrap();
}
fn setup_git(f: &mut Fixture) {
    let tool = if cfg!(windows) { "where.exe" } else { "which" };
    let found = std::process::Command::new(tool)
        .arg("git")
        .output()
        .unwrap();
    assert!(found.status.success());
    let git = PathBuf::from(
        String::from_utf8(found.stdout)
            .unwrap()
            .lines()
            .next()
            .unwrap()
            .trim(),
    );
    f.config.project.workspace_mode = "git-worktree".into();
    f.config.project.git_executable = Some(git.clone());
    let run = |args: &[&str]| {
        let result = std::process::Command::new(&git)
            .arg("-C")
            .arg(&f.config.project.root)
            .args(args)
            .output()
            .unwrap();
        assert!(result.status.success(), "Git fixture setup failed");
    };
    run(&["init", "-q"]);
    std::fs::write(f.config.project.root.join("README.txt"), "fixture base\n").unwrap();
    run(&["add", "README.txt"]);
    run(&[
        "-c",
        "user.name=fixture",
        "-c",
        "user.email=fixture@invalid",
        "-c",
        "commit.gpgsign=false",
        "commit",
        "-qm",
        "fixture base",
    ]);
    f.store.replace_agent(&f.agent, &f.config).unwrap();
}
#[tokio::test]
async fn worktree_execution_leaves_original_untouched_and_applies_only_reviewed_patch() {
    let mut f = Fixture::new();
    setup_git(&mut f);
    let task = f.submit("fixture:create", 5);
    let (stop, job) = f.executor();
    let terminal = f.terminal(task.task_id).await;
    assert_eq!(
        terminal.status,
        TaskStatus::Succeeded,
        "{:?}",
        terminal.error
    );
    stop.send(true).unwrap();
    job.await.unwrap();
    assert!(!f.config.project.root.join("out/health.json").exists());
    let workspace = f.store.workspace(task.task_id).unwrap().unwrap();
    assert_eq!(workspace["state"], "review");
    assert!(
        workspace["patch"]
            .as_str()
            .unwrap()
            .contains("out/health.json")
    );
    let scratch = f.instance.dir.join("review");
    assert!(
        hmesh_runtime::workspace::apply(&f.store, task.task_id, "wrong-hash", &scratch).is_err()
    );
    hmesh_runtime::workspace::apply(
        &f.store,
        task.task_id,
        workspace["patch_sha256"].as_str().unwrap(),
        &scratch,
    )
    .unwrap();
    assert!(f.config.project.root.join("out/health.json").is_file());
    assert!(
        hmesh_runtime::workspace::apply(
            &f.store,
            task.task_id,
            workspace["patch_sha256"].as_str().unwrap(),
            &scratch
        )
        .is_err()
    );
}
#[tokio::test]
async fn worktree_refuses_uncommitted_original_without_touching_its_files() {
    let mut f = Fixture::new();
    setup_git(&mut f);
    std::fs::write(
        f.config.project.root.join("README.txt"),
        "owner unfinished work",
    )
    .unwrap();
    let task = f.submit("fixture:create", 5);
    let (stop, job) = f.executor();
    let terminal = f.terminal(task.task_id).await;
    assert_eq!(terminal.status, TaskStatus::Failed);
    assert_eq!(
        std::fs::read_to_string(f.config.project.root.join("README.txt")).unwrap(),
        "owner unfinished work"
    );
    assert!(!f.config.project.root.join("out").exists());
    stop.send(true).unwrap();
    job.await.unwrap();
}

#[tokio::test]
async fn large_native_snapshots_and_text_complete_without_losing_text_or_storing_raw_tools() {
    let f = Fixture::new();
    let task = f.submit("fixture:large-events", 5);
    let (stop, job) = f.executor();
    let terminal = f.terminal(task.task_id).await;
    assert_eq!(
        terminal.status,
        TaskStatus::Succeeded,
        "{:?}",
        terminal.error
    );
    let mut cursor = 0;
    let mut text = String::new();
    let mut measured_large_record = false;
    loop {
        let events = f.store.events(task.task_id, cursor).unwrap();
        if events.is_empty() {
            break;
        }
        for event in events {
            cursor = event.seq;
            assert!(serde_json::to_vec(&event.payload).unwrap().len() <= MAX_EVENT_PAYLOAD_BYTES);
            if event.payload["type"] == "text_delta" {
                text.push_str(event.payload["text"].as_str().unwrap());
            }
            if event.event_type == "output_summary" {
                measured_large_record = event.payload["largest_complete_record_bytes"]
                    .as_u64()
                    .unwrap()
                    > 65536;
            }
            assert!(event.payload.get("args").is_none());
            assert!(event.payload.get("result").is_none());
            assert!(event.payload.get("snapshot").is_none());
        }
    }
    assert!(measured_large_record);
    assert_eq!(text, "中🙂\n\u{0000}\"\\".repeat(10000));
    stop.send(true).unwrap();
    job.await.unwrap();
}

#[tokio::test]
async fn native_message_and_cumulative_stdout_limits_still_stop_runs() {
    let f = Fixture::new();
    let (stop, job) = f.executor();
    for (prompt, output, expected) in [
        (
            "fixture:native-over-limit",
            8 * 1024 * 1024,
            "native message limit",
        ),
        (
            "fixture:large-events",
            16384,
            "stdout exceeded output limit",
        ),
    ] {
        let task = f.submit_with_output(prompt, 5, output);
        let task = f.terminal(task.task_id).await;
        assert_eq!(task.status, TaskStatus::Failed);
        let error = task.error.unwrap();
        assert_eq!(error.code, ErrorCode::RateLimited);
        assert!(error.message.contains(expected), "{error}");
    }
    stop.send(true).unwrap();
    job.await.unwrap();
}

#[tokio::test]
async fn deterministic_artifact_verification_is_separate_from_harness_success() {
    let f = Fixture::new();
    let task = f.submit("fixture:create", 5);
    let (stop, job) = f.executor();
    assert_eq!(f.terminal(task.task_id).await.status, TaskStatus::Succeeded);
    let good = hmesh_runtime::verification::verify_json(
        &f.config,
        std::path::Path::new("out/health.json"),
        &serde_json::json!({"status":"ok"}),
    )
    .expect("verify");
    assert_eq!(good["passed"], true);
    let wrong = hmesh_runtime::verification::verify_json(
        &f.config,
        std::path::Path::new("out/health.json"),
        &serde_json::json!({"status":"broken"}),
    )
    .expect("verify");
    assert_eq!(wrong["passed"], false);
    f.store
        .record_verification(task.task_id, &wrong)
        .expect("record");
    assert_eq!(
        f.store.task(task.task_id).expect("task").status,
        TaskStatus::Succeeded
    );
    assert!(
        hmesh_runtime::verification::verify_json(
            &f.config,
            std::path::Path::new("../secret"),
            &serde_json::json!({"status":"ok"})
        )
        .is_err()
    );
    assert!(
        f.store
            .events(task.task_id, 0)
            .expect("events")
            .iter()
            .any(|e| e.event_type == "artifact")
    );
    stop.send(true).expect("stop");
    job.await.expect("job");
}

#[tokio::test]
async fn cancellation_reclaims_owned_child_processes_and_timeout_is_not_success() {
    let f = Fixture::new();
    let task = f.submit("fixture:child", 5);
    let (stop, job) = f.executor();
    let heartbeat = f.config.project.root.join("heartbeat");
    for _ in 0..80 {
        if heartbeat.is_file() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    assert!(heartbeat.is_file(), "fixture child never started");
    f.store
        .transition(task.task_id, TaskStatus::Cancelling, None)
        .expect("cancel");
    let terminal = f.terminal(task.task_id).await;
    assert!(matches!(
        terminal.status,
        TaskStatus::Cancelled | TaskStatus::Unknown
    ));
    #[cfg(windows)]
    assert_eq!(terminal.status, TaskStatus::Cancelled);
    let before = std::fs::read(&heartbeat).expect("heartbeat");
    tokio::time::sleep(Duration::from_millis(150)).await;
    assert_eq!(
        before,
        std::fs::read(&heartbeat).expect("stopped heartbeat")
    );
    let timeout = f.submit("fixture:wait", 1);
    assert_eq!(f.terminal(timeout.task_id).await.status, TaskStatus::Failed);
    stop.send(true).expect("stop");
    job.await.expect("job");
}

#[tokio::test]
async fn malformed_output_noninteractive_input_and_crash_are_never_success() {
    let f = Fixture::new();
    let (stop, job) = f.executor();
    for prompt in [
        "fixture:malformed",
        "fixture:oversized",
        "fixture:crash",
        "fixture:needs-input",
    ] {
        let task = f.submit(prompt, 5);
        let task = f.terminal(task.task_id).await;
        assert_eq!(
            task.status,
            TaskStatus::Failed,
            "{prompt}: {:?}",
            task.error
        );
        if prompt == "fixture:needs-input" {
            assert_eq!(task.error.expect("error").code, ErrorCode::FailedNeedsInput);
        }
    }
    stop.send(true).expect("stop");
    job.await.expect("job");
}

#[tokio::test]
async fn missing_configured_credential_fails_before_starting_the_harness() {
    let f = Fixture::new_with_env(vec![
        format!("HMESH_MISSING_FIXTURE_{}", Uuid::new_v4().simple()).to_ascii_uppercase(),
    ]);
    let task = f.submit("fixture:create", 5);
    let (stop, job) = f.executor();
    let task = f.terminal(task.task_id).await;
    assert_eq!(task.status, TaskStatus::Failed);
    assert!(
        task.error
            .expect("error")
            .message
            .contains("credential is missing")
    );
    assert!(!f.config.project.root.join("out/health.json").exists());
    stop.send(true).expect("stop");
    job.await.expect("join");
}

#[tokio::test]
async fn execution_authorization_is_rechecked_before_a_queued_process_starts() {
    let f = Fixture::new();
    let task = f.submit("fixture:create", 5);
    let (stop, receiver) = watch::channel(false);
    let job = tokio::spawn(hmesh_runtime::run_queue(
        f.store.clone(),
        f.instance.dir.clone(),
        RuntimeOptions::default(),
        Arc::new(|_| {
            Err(MeshError::new(
                ErrorCode::PermissionDenied,
                "grant revoked after admission",
            ))
        }),
        receiver,
    ));
    let task = f.terminal(task.task_id).await;
    assert_eq!(task.status, TaskStatus::Failed);
    assert_eq!(task.error.expect("error").code, ErrorCode::PermissionDenied);
    assert!(!f.config.project.root.join("out/health.json").exists());
    stop.send(true).expect("stop");
    job.await.expect("join");
}

#[cfg(windows)]
struct Daemon(Child);
#[cfg(windows)]
impl Drop for Daemon {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}
#[cfg(windows)]
fn daemon(f: &Fixture) -> Daemon {
    let mut command = Command::new(env!("CARGO_BIN_EXE_hmesh-fixture"));
    command
        .arg("--daemon")
        .arg(&f.instance.dir)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(0x08000000);
    }
    Daemon(command.spawn().expect("daemon"))
}
#[tokio::test]
#[cfg(windows)]
async fn killed_daemon_recovers_unknown_without_restarting_a_side_effect_task() {
    let f = Fixture::new();
    let task = f.submit("fixture:child", 30);
    let mut process = daemon(&f);
    let heartbeat = f.config.project.root.join("heartbeat");
    for _ in 0..120 {
        if heartbeat.is_file() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    assert!(heartbeat.is_file());
    assert_eq!(
        f.store.task(task.task_id).expect("running").status,
        TaskStatus::Running
    );
    process.0.kill().expect("kill this daemon");
    process.0.wait().expect("exit");
    let restarted = daemon(&f);
    assert_eq!(f.terminal(task.task_id).await.status, TaskStatus::Unknown);
    tokio::time::sleep(Duration::from_millis(150)).await;
    assert_eq!(f.store.attempt_count(task.task_id).expect("attempts"), 1);
    assert_eq!(
        f.store.task(task.task_id).expect("unknown").status,
        TaskStatus::Unknown
    );
    drop(restarted);
}

#[tokio::test]
async fn running_grant_revocation_stops_execution_without_retry() {
    use std::sync::atomic::{AtomicUsize, Ordering};
    let f = Fixture::new();
    let task = f.submit("fixture:wait", 10);
    let calls = Arc::new(AtomicUsize::new(0));
    let seen = calls.clone();
    let (stop, receiver) = watch::channel(false);
    let job = tokio::spawn(hmesh_runtime::run_queue(
        f.store.clone(),
        f.instance.dir.clone(),
        RuntimeOptions::default(),
        Arc::new(move |_| {
            if seen.fetch_add(1, Ordering::SeqCst) < 2 {
                Ok(())
            } else {
                Err(MeshError::new(
                    ErrorCode::PermissionDenied,
                    "running grant revoked",
                ))
            }
        }),
        receiver,
    ));
    let result = f.terminal(task.task_id).await;
    assert_eq!(result.status, TaskStatus::Failed);
    assert_eq!(
        result.error.expect("revocation error").code,
        ErrorCode::PermissionDenied
    );
    assert_eq!(f.store.attempt_count(task.task_id).unwrap(), 1);
    assert!(calls.load(Ordering::SeqCst) >= 2);
    stop.send(true).expect("stop");
    job.await.expect("join");
}

#[test]
fn agent_changes_preserve_history_and_reject_outstanding_tasks() {
    let f = Fixture::new();
    let mut disabled = f.agent.clone();
    disabled.availability = "disabled".into();
    disabled.probe.managed_run = "not_checked".into();
    f.store
        .replace_agent(&disabled, &f.config)
        .expect("disable idle agent");
    assert_eq!(
        f.store.agent(disabled.agent_id).unwrap().0.availability,
        "disabled"
    );
    let task = f.submit("fixture:wait", 5);
    assert_eq!(
        f.store
            .replace_agent(&disabled, &f.config)
            .expect_err("outstanding task")
            .code,
        ErrorCode::TaskConflict
    );
    f.store
        .request_cancel(task.task_id)
        .expect("cancel queued task");
    let mut changed = f.config.clone();
    changed.project.root = f.instance.dir.clone();
    assert!(f.store.replace_agent(&disabled, &changed).is_err());
    f.store
        .replace_agent(&disabled, &f.config)
        .expect("resolved tasks allow update");
    assert_eq!(
        f.store.task(task.task_id).unwrap().status,
        TaskStatus::Cancelled
    );
}

#[test]
fn retired_agent_disappears_from_discovery_without_deleting_history() {
    let f = Fixture::new();
    let task = f.submit("fixture:wait", 5);
    assert!(f.store.retire_agent(f.agent.agent_id, false).is_err());
    f.store.request_cancel(task.task_id).unwrap();
    f.store.retire_agent(f.agent.agent_id, false).unwrap();
    assert!(f.store.agents().unwrap().is_empty());
    assert_eq!(f.store.retired_agents().unwrap().len(), 1);
    assert!(f.store.agent_retired(f.agent.agent_id).unwrap());
    let request = Envelope::new(
        f.instance.card.node_id,
        "task.submit",
        TaskSubmit::new(f.agent.agent_id, "fixture", "new task"),
    );
    assert!(
        f.store
            .admit(f.instance.card.node_id, &request, false, 20)
            .is_err()
    );
    assert!(f.store.replace_agent(&f.agent, &f.config).is_err());
    let restored = f.store.retire_agent(f.agent.agent_id, true).unwrap();
    assert_eq!(restored.availability, "unverified");
    assert_eq!(restored.probe.managed_run, "not_checked");
    assert_eq!(
        f.store.task(task.task_id).unwrap().status,
        TaskStatus::Cancelled
    );
    assert_eq!(f.store.agents().unwrap().len(), 1);
}

#[cfg(unix)]
#[tokio::test]
async fn cancellation_stops_an_observed_child_that_left_the_process_group() {
    let f = Fixture::new();
    let task = f.submit("fixture:escaped-child", 10);
    let (stop, job) = f.executor();
    let heartbeat = f.config.project.root.join("heartbeat");
    for _ in 0..100 {
        if heartbeat.exists() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    assert!(heartbeat.exists());
    tokio::time::sleep(Duration::from_millis(250)).await;
    f.store.request_cancel(task.task_id).unwrap();
    let terminal = f.terminal(task.task_id).await;
    assert_eq!(terminal.status, TaskStatus::Cancelled);
    let after = std::fs::read(&heartbeat).unwrap();
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert_eq!(std::fs::read(&heartbeat).unwrap(), after);
    stop.send(true).unwrap();
    job.await.unwrap();
}
