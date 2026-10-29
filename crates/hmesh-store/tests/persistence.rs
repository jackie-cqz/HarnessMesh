use hmesh_core::*;
use hmesh_store::Store;
use serde_json::json;
use uuid::Uuid;

fn request() -> Envelope<TaskSubmit> {
    Envelope::new(
        Uuid::new_v4(),
        "task.submit",
        TaskSubmit::new(Uuid::new_v4(), "demo", "create fixture"),
    )
}

#[test]
fn duplicate_admission_survives_restart_and_conflicting_reuse_is_rejected() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("node.sqlite3");
    let target = Uuid::new_v4();
    let request = request();
    let store = Store::open(&path).expect("store");
    let first = store.admit(target, &request, false, 20).expect("admit");
    drop(store);
    let reopened = Store::open(&path).expect("reopen");
    assert_eq!(
        first.task_id,
        reopened
            .admit(target, &request, false, 20)
            .expect("duplicate")
            .task_id
    );
    assert_eq!(reopened.tasks().expect("list").len(), 1);
    assert_eq!(reopened.attempt_count(first.task_id).expect("attempts"), 0);
    let mut conflict = request;
    conflict.payload.goal = "different side effect".into();
    assert_eq!(
        reopened
            .admit(target, &conflict, false, 20)
            .expect_err("conflict")
            .code,
        ErrorCode::TaskConflict
    );
}

#[test]
fn concurrent_executors_claim_a_single_attempt_and_recovery_never_reexecutes() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("node.sqlite3");
    let store = Store::open(&path).expect("store");
    let task = store
        .admit(Uuid::new_v4(), &request(), false, 20)
        .expect("admit");
    let threads: Vec<_> = (0..8)
        .map(|_| {
            let path = path.clone();
            std::thread::spawn(move || {
                Store::open(&path)
                    .expect("connection")
                    .claim_next()
                    .expect("claim")
            })
        })
        .collect();
    let claimed = threads
        .into_iter()
        .filter_map(|t| t.join().expect("thread"))
        .count();
    assert_eq!(claimed, 1);
    assert_eq!(store.attempt_count(task.task_id).expect("attempts"), 1);
    assert_eq!(store.recover_interrupted().expect("recover"), 1);
    assert_eq!(
        store.task(task.task_id).expect("task").status,
        TaskStatus::Unknown
    );
    assert!(store.claim_next().expect("next").is_none());
    assert_eq!(store.recover_interrupted().expect("idempotent recovery"), 0);
}

#[test]
fn rejected_transition_does_not_append_an_event_and_replay_is_ordered() {
    let dir = tempfile::tempdir().expect("tempdir");
    let store = Store::open(&dir.path().join("node.sqlite3")).expect("store");
    let task = store
        .admit(Uuid::new_v4(), &request(), true, 20)
        .expect("admit");
    assert!(store.claim_next().expect("not approved").is_none());
    let before = store.events(task.task_id, 0).expect("events");
    assert!(
        store
            .transition(task.task_id, TaskStatus::Succeeded, None)
            .is_err()
    );
    assert_eq!(
        store.events(task.task_id, 0).expect("unchanged").len(),
        before.len()
    );
    store
        .transition(task.task_id, TaskStatus::Queued, None)
        .expect("approve");
    store.claim_next().expect("claim").expect("task");
    store
        .append_event(task.task_id, "text", json!({"text":"result"}))
        .expect("event");
    let events = store.events(task.task_id, 1).expect("replay");
    assert_eq!(
        events.iter().map(|e| e.seq).collect::<Vec<_>>(),
        vec![2, 3, 4]
    );
}

#[test]
fn future_database_version_is_refused_without_reset() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("future.sqlite3");
    let db = rusqlite::Connection::open(&path).expect("sqlite");
    db.execute_batch("CREATE TABLE keep_me(value TEXT); INSERT INTO keep_me VALUES('retained'); PRAGMA user_version=99;").expect("seed");
    drop(db);
    assert!(Store::open(&path).is_err());
    let db = rusqlite::Connection::open(path).expect("sqlite");
    assert_eq!(
        db.query_row("SELECT value FROM keep_me", [], |r| r.get::<_, String>(0))
            .expect("preserved"),
        "retained"
    );
}

#[test]
fn large_event_pages_are_byte_bounded_and_cursor_replay_is_lossless() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(&dir.path().join("events.sqlite3")).unwrap();
    let task = store.admit(Uuid::new_v4(), &request(), false, 20).unwrap();
    for index in 0..24 {
        store
            .append_event(
                task.task_id,
                "run",
                json!({"index":index,"text":"\u{0000}".repeat(10000)}),
            )
            .unwrap();
    }
    assert!(
        store
            .append_event(
                task.task_id,
                "run",
                json!({"text":"x".repeat(MAX_EVENT_PAYLOAD_BYTES)})
            )
            .is_err()
    );
    let mut cursor = 0;
    let mut seen = vec![];
    let mut pages = 0;
    loop {
        let events = store.events(task.task_id, cursor).unwrap();
        if events.is_empty() {
            break;
        }
        assert!(serde_json::to_vec(&events).unwrap().len() <= 512 * 1024);
        pages += 1;
        for event in events {
            assert_eq!(event.seq, cursor + 1);
            cursor = event.seq;
            if let Some(index) = event.payload["index"].as_u64() {
                seen.push(index);
            }
        }
    }
    assert!(pages > 1);
    assert_eq!(seen, (0..24).collect::<Vec<_>>());
}
