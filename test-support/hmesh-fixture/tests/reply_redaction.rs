//! Its own test binary: it sets one process environment variable, so no other
//! test thread may read the environment concurrently. Never calls a model.
use hmesh_core::*;
use hmesh_security::Instance;
use hmesh_store::Store;
use std::{path::PathBuf, sync::Arc, time::Duration};
use uuid::Uuid;

#[tokio::test]
async fn final_reply_excludes_narration_while_redaction_holds_text_back() {
    // A configured credential makes the redactor hold back up to len-1 bytes of
    // text; the tool boundary must still separate narration from the answer.
    // SAFETY: the only test in this binary, run before any thread reads the environment.
    unsafe { std::env::set_var("HMESH_FIXTURE_REPLY_SECRET", "s".repeat(35)) };
    let temp = tempfile::tempdir().unwrap();
    let mut node = NodeConfig::default();
    node.node.listen_port = 0;
    let instance = Arc::new(Instance::init(&temp.path().join("node"), node).unwrap());
    let project = temp.path().join("project");
    std::fs::create_dir(&project).unwrap();
    let config: AgentConfig = serde_json::from_value(serde_json::json!({
        "agent":{"name":"fixture","harness":"pi","project_id":"fixture"},
        "project":{"root":project,"allowed_tools":["read","write","edit"]},
        "adapter":{"executable":PathBuf::from(env!("CARGO_BIN_EXE_hmesh-fixture")),"provider":"fixture","model":"fixture","env_allowlist":["HMESH_FIXTURE_REPLY_SECRET"]}
    }))
    .unwrap();
    let agent: AgentCard = serde_json::from_value(serde_json::json!({
        "agent_id":Uuid::new_v4(),"node_id":instance.card.node_id,"name":"fixture","harness":"pi","project_ids":["fixture"],"availability":"test-fixture","capabilities":[],
        "probe":{"installed":true,"version":"test-fixture","version_supported":true,"protocol_probe":"fixture","authentication":"not_applicable_test_fixture","managed_run":"not_checked","streaming":"not_checked","cancel":"not_checked","session_resume":"not_checked","unavailable_reason":null}
    }))
    .unwrap();
    let store = Store::open(&instance.dir.join("state.sqlite3")).unwrap();
    store.register_agent(&agent, &config).unwrap();
    let mut task = TaskSubmit::new(agent.agent_id, "fixture", "fixture:reply");
    task.limits.timeout_seconds = 10;
    let envelope = Envelope::new(instance.card.node_id, "task.submit", task);
    let task = store
        .admit(instance.card.node_id, &envelope, false, 20)
        .unwrap();
    let (stop, receiver) = tokio::sync::watch::channel(false);
    let job = tokio::spawn(hmesh_runtime::run_queue(
        store.clone(),
        instance.dir.clone(),
        RuntimeOptions::default(),
        Arc::new(|_| Ok(())),
        receiver,
    ));
    let mut terminal = None;
    for _ in 0..200 {
        let current = store.task(task.task_id).unwrap();
        if current.status.is_terminal() {
            terminal = Some(current);
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    let terminal = terminal.expect("fixture task did not finish");
    assert_eq!(
        terminal.status,
        TaskStatus::Succeeded,
        "{:?}",
        terminal.error
    );
    let result = store
        .last_event_of_type(task.task_id, "result")
        .unwrap()
        .expect("result event");
    assert_eq!(result.payload["text"], "Final answer: the build is green.");
    assert_eq!(result.payload["truncated"], false);
    let events = store.events(task.task_id, 0).unwrap();
    let first_tool = events
        .iter()
        .position(|e| e.payload["type"] == "tool_event")
        .expect("tool event");
    let narration: String = events[..first_tool]
        .iter()
        .filter(|e| e.payload["type"] == "text_delta")
        .map(|e| e.payload["text"].as_str().unwrap())
        .collect();
    assert_eq!(narration, "Let me check the project.");
    stop.send(true).unwrap();
    job.await.unwrap();
}
