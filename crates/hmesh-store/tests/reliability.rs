use hmesh_core::*;
use hmesh_store::{SnapshotEntry, Store};
use serde_json::json;
use uuid::Uuid;
fn request(project: &str, agent: Uuid) -> Envelope<TaskSubmit> {
    Envelope::new(
        Uuid::new_v4(),
        "task.submit",
        TaskSubmit::new(agent, project, "explicit fixture only"),
    )
}
fn agent(store: &Store, project: &str, root: &std::path::Path) -> Uuid {
    let id = Uuid::new_v4();
    let config:AgentConfig=serde_json::from_value(json!({"agent":{"name":format!("agent-{id}"),"harness":"pi","project_id":project},"project":{"root":root,"allowed_tools":["read"]},"adapter":{"executable":std::env::current_exe().unwrap(),"model":"fixture","provider":"fixture"}})).unwrap();
    let card:AgentCard=serde_json::from_value(json!({"agent_id":id,"node_id":Uuid::new_v4(),"name":config.agent.name,"harness":"pi","project_ids":[project],"availability":"fixture","capabilities":[],"probe":{"installed":true,"version":"fixture","version_supported":true,"protocol_probe":"fixture","authentication":"fixture","managed_run":"fixture","streaming":"fixture","cancel":"fixture","session_resume":"fixture","unavailable_reason":null}})).unwrap();
    store.register_agent(&card, &config).unwrap();
    id
}
#[test]
fn completion_snapshot_is_attempt_bound_sealed_and_atomic_on_write_failure() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("db");
    let store = Store::open(&path).unwrap();
    let agent = agent(&store, "one", temp.path());
    let task = store
        .admit(Uuid::new_v4(), &request("one", agent), false, 20)
        .unwrap();
    let active = store.claim_next().unwrap().unwrap();
    let entry = SnapshotEntry {
        name: "out/result.json".into(),
        bytes: Some(br#"{"status":"ok"}"#.to_vec()),
        error: None,
    };
    let db = rusqlite::Connection::open(&path).unwrap();
    db.execute_batch("CREATE TRIGGER fail_snapshot BEFORE INSERT ON artifact_snapshots BEGIN SELECT RAISE(ABORT,'injected write failure'); END;").unwrap();
    assert!(
        store
            .complete_with_artifacts(
                task.task_id,
                active.last_attempt_id.unwrap(),
                "captured-config",
                &[entry]
            )
            .is_err()
    );
    assert_eq!(
        store.task(task.task_id).unwrap().status,
        TaskStatus::Running
    );
    assert!(store.artifact_set(task.task_id).unwrap().is_none());
    db.execute_batch("DROP TRIGGER fail_snapshot").unwrap();
    let entry = SnapshotEntry {
        name: "out/result.json".into(),
        bytes: Some(b"original bytes".to_vec()),
        error: None,
    };
    store
        .complete_with_artifacts(
            task.task_id,
            active.last_attempt_id.unwrap(),
            "captured-config",
            &[entry],
        )
        .unwrap();
    assert!(
        store
            .complete_with_artifacts(task.task_id, Uuid::new_v4(), "changed", &[])
            .is_err()
    );
    drop(store);
    let store = Store::open(&path).unwrap();
    let snapshot = store.artifact_set(task.task_id).unwrap().unwrap();
    assert_eq!(snapshot.attempt_id, active.last_attempt_id.unwrap());
    assert_eq!(snapshot.configuration_sha256, "captured-config");
    assert_eq!(
        snapshot.entries[0].bytes.as_deref(),
        Some(b"original bytes".as_slice())
    );
}
#[test]
fn scoped_admission_is_atomic_and_duplicates_still_resolve_at_capacity() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("db");
    drop(Store::open(&path).unwrap());
    let id = Uuid::new_v4();
    let mut limits = RuntimeOptions {
        max_queue_depth: 100,
        ..Default::default()
    };
    limits.project_limits.insert(
        "one".into(),
        QueueLimits {
            max_queue_depth: 2,
            ..Default::default()
        },
    );
    let requests: Vec<_> = (0..16).map(|_| request("one", id)).collect();
    let threads: Vec<_> = requests
        .iter()
        .cloned()
        .map(|r| {
            let path = path.clone();
            let limits = limits.clone();
            std::thread::spawn(move || {
                Store::open(&path)
                    .unwrap()
                    .admit_with_limits(Uuid::new_v4(), &r, true, &limits)
                    .ok()
                    .map(|_| r)
            })
        })
        .collect();
    let admitted: Vec<_> = threads
        .into_iter()
        .filter_map(|t| t.join().unwrap())
        .collect();
    assert_eq!(admitted.len(), 2);
    let store = Store::open(&path).unwrap();
    assert!(
        store
            .admit_with_limits(Uuid::new_v4(), &admitted[0], true, &limits)
            .is_ok()
    );
    assert_eq!(store.tasks().unwrap().len(), 2);
}
#[test]
fn round_robin_projects_and_shared_directory_exclusion_survive_reopen() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("db");
    let store = Store::open(&path).unwrap();
    let a = agent(&store, "a", temp.path());
    let b = agent(&store, "b", temp.path());
    for _ in 0..3 {
        store
            .admit(Uuid::new_v4(), &request("a", a), false, 20)
            .unwrap();
    }
    store
        .admit(Uuid::new_v4(), &request("b", b), false, 20)
        .unwrap();
    let limits = RuntimeOptions {
        max_concurrent_runs: 2,
        ..Default::default()
    };
    let first = store.claim_next_with_limits(&limits).unwrap().unwrap();
    assert!(store.claim_next_with_limits(&limits).unwrap().is_none());
    assert!(
        store.queue_status(&limits).unwrap()["waiting"]
            .as_array()
            .unwrap()
            .iter()
            .any(|r| r["reason"] == "shared_workspace")
    );
    store
        .transition(first.task_id, TaskStatus::Failed, None)
        .unwrap();
    drop(store);
    let store = Store::open(&path).unwrap();
    let second = store.claim_next_with_limits(&limits).unwrap().unwrap();
    assert_ne!(first.project_id, second.project_id);
}
#[test]
fn migration_failure_rolls_back_entire_new_schema_and_can_be_retried() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("db");
    let db = rusqlite::Connection::open(&path).unwrap();
    for sql in [
        include_str!("../migrations/001_initial.sql"),
        include_str!("../migrations/002_outbound.sql"),
        include_str!("../migrations/003_tool_submissions.sql"),
        include_str!("../migrations/004_managed.sql"),
        include_str!("../migrations/005_lifecycle.sql"),
        include_str!("../migrations/006_observation.sql"),
    ] {
        db.execute_batch(sql).unwrap();
    }
    db.execute_batch("CREATE TABLE artifact_snapshots (sentinel TEXT); INSERT INTO artifact_snapshots VALUES('preserve');").unwrap();
    assert!(Store::open(&path).is_err());
    assert_eq!(
        db.pragma_query_value(None, "user_version", |r| r.get::<_, i64>(0))
            .unwrap(),
        6
    );
    assert!(
        !db.query_row(
            "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE name='artifact_sets')",
            [],
            |r| r.get::<_, bool>(0)
        )
        .unwrap()
    );
    assert_eq!(
        db.query_row("SELECT sentinel FROM artifact_snapshots", [], |r| r
            .get::<_, String>(0))
            .unwrap(),
        "preserve"
    );
    db.execute_batch("DROP TABLE artifact_snapshots").unwrap();
    assert!(Store::open(&path).is_ok());
}
#[test]
fn concurrent_claim_cancel_load_keeps_one_attempt_and_contiguous_history() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("db");
    let store = Store::open(&path).unwrap();
    let limits = RuntimeOptions {
        max_concurrent_runs: 4,
        max_queue_depth: 100,
        ..Default::default()
    };
    for n in 0..80 {
        store
            .admit_with_limits(
                Uuid::new_v4(),
                &request(&format!("project-{}", n % 8), Uuid::new_v4()),
                false,
                &limits,
            )
            .unwrap();
    }
    let threads: Vec<_> = (0..8)
        .map(|_| {
            let path = path.clone();
            let limits = limits.clone();
            std::thread::spawn(move || {
                let store = Store::open(&path).unwrap();
                loop {
                    if let Some(task) = store.claim_next_with_limits(&limits).unwrap() {
                        store
                            .transition(task.task_id, TaskStatus::Cancelling, None)
                            .unwrap();
                        store
                            .transition(task.task_id, TaskStatus::Cancelled, None)
                            .unwrap();
                    } else if store
                        .tasks()
                        .unwrap()
                        .iter()
                        .all(|t| t.status.is_terminal())
                    {
                        break;
                    } else {
                        std::thread::yield_now();
                    }
                }
            })
        })
        .collect();
    for t in threads {
        t.join().unwrap();
    }
    for task in store.tasks().unwrap() {
        assert_eq!(task.status, TaskStatus::Cancelled);
        assert_eq!(store.attempt_count(task.task_id).unwrap(), 1);
    }
    let db = rusqlite::Connection::open(path).unwrap();
    let broken:i64=db.query_row("SELECT count(*) FROM (SELECT task_id,count(*) n,max(seq) last FROM task_events GROUP BY task_id HAVING n!=last)",[],|r|r.get(0)).unwrap();
    assert_eq!(broken, 0);
}
