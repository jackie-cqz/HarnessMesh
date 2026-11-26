use hmesh_core::*;
use hmesh_store::Store;
use serde_json::json;
use uuid::Uuid;

fn peer_card(fingerprint: &str) -> NodeCard {
    NodeCard {
        version: PROTOCOL.into(),
        node_id: Uuid::new_v4(),
        display_name: "studio".into(),
        fingerprint: fingerprint.into(),
        certificate_pem: String::new(),
        endpoint: "https://192.168.1.20:38880".into(),
    }
}

/// A trusted peer's addresses: only a newer list replaces the stored one,
/// an untrusted peer stores nothing, the owner's own addresses and the last
/// one that answered are kept apart, and revoking keeps them all.
#[test]
fn peer_addresses_are_kept_per_trusted_peer_and_only_newer_ones_replace_them() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("addresses.sqlite3");
    let store = Store::open(&path).unwrap();
    let card = peer_card(&"a".repeat(64));
    let told = |revision, addresses: &[&str]| PeerAddresses {
        node_id: card.node_id,
        revision,
        addresses: addresses.iter().map(|a| a.to_string()).collect(),
    };
    // Not paired yet: nothing is stored, nothing can be set.
    assert!(
        !store
            .set_advertised_addresses(&told(5, &["https://studio.local:38880"]))
            .unwrap()
    );
    assert!(store.set_manual_addresses(card.node_id, &[]).is_err());
    assert_eq!(
        store.peer_routes(card.node_id).unwrap(),
        PeerRoutes::default()
    );

    store.trust_peer(&card).unwrap();
    assert!(
        store
            .set_advertised_addresses(&told(5, &[&card.endpoint, "https://studio.local:38880"]))
            .unwrap()
    );
    // Older or the same revision: ignored.
    assert!(
        !store
            .set_advertised_addresses(&told(4, &["https://old:1"]))
            .unwrap()
    );
    assert!(
        !store
            .set_advertised_addresses(&told(5, &["https://same:1"]))
            .unwrap()
    );
    store
        .set_manual_addresses(card.node_id, &["https://10.0.0.7:38880".into()])
        .unwrap();
    store
        .remember_route(&card.fingerprint, "https://studio.local:38880", 77)
        .unwrap();
    // Another certificate is not this peer.
    store
        .remember_route(&"b".repeat(64), "https://elsewhere:1", 78)
        .unwrap();
    let expected = PeerRoutes {
        advertised: vec![card.endpoint.clone(), "https://studio.local:38880".into()],
        manual: vec!["https://10.0.0.7:38880".into()],
        last_good: Some("https://studio.local:38880".into()),
        last_good_at: Some(77),
    };
    assert_eq!(store.peer_routes(card.node_id).unwrap(), expected);

    // Revoked: kept for a pairing later, but a revoked peer changes nothing.
    store.revoke_peer(card.node_id).unwrap();
    assert!(
        !store
            .set_advertised_addresses(&told(9, &["https://new:1"]))
            .unwrap()
    );
    store
        .remember_route(&card.fingerprint, "https://new:1", 99)
        .unwrap();
    drop(store);
    let store = Store::open(&path).unwrap();
    assert_eq!(store.peer_routes(card.node_id).unwrap(), expected);
    // Paired again: its newer list is taken.
    store.trust_peer(&card).unwrap();
    assert!(
        store
            .set_advertised_addresses(&told(9, &["https://new:1"]))
            .unwrap()
    );
    store.set_manual_addresses(card.node_id, &[]).unwrap();
    let routes = store.peer_routes(card.node_id).unwrap();
    assert_eq!(routes.advertised, ["https://new:1"]);
    assert!(routes.manual.is_empty());
    assert_eq!(routes.last_good_at, Some(77));
}

#[test]
fn outbound_observation_time_is_unknown_until_a_real_response_is_cached() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("observed.sqlite3");
    let store = Store::open(&path).unwrap();
    let envelope = request();
    let peer = Uuid::new_v4();
    store.remember_outbound(peer, &envelope).unwrap();
    assert!(store.outbound_tasks().unwrap()[0]["last_observed_at"].is_null());
    drop(store);
    let store = Store::open(&path).unwrap();
    assert!(store.outbound_tasks().unwrap()[0]["last_observed_at"].is_null());
    let task = store.admit(peer, &envelope, true, 20).unwrap();
    store.cache_outbound(&task).unwrap();
    let rows = store.outbound_tasks().unwrap();
    assert!(
        rows[0]["last_observed_at"]
            .as_i64()
            .is_some_and(|t| t <= now())
    );
    assert_eq!(rows[0]["request_id"], envelope.request_id.to_string());
    assert_eq!(rows[0]["last_observed"]["status"], "PENDING_APPROVAL");
}

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
        db.pragma_query_value(None, "journal_mode", |r| r.get::<_, String>(0))
            .unwrap(),
        "delete"
    );
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

#[test]
fn schema_two_upgrade_preserves_historical_task_records() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("old.sqlite3");
    let db = rusqlite::Connection::open(&path).unwrap();
    db.execute_batch(include_str!("../migrations/001_initial.sql"))
        .unwrap();
    db.execute_batch(include_str!("../migrations/002_outbound.sql"))
        .unwrap();
    let request = request();
    let old = TaskRecord {
        task_id: request.payload.task_id,
        origin_node_id: request.sender_node_id,
        target_node_id: Uuid::new_v4(),
        agent_id: request.payload.target_agent_id,
        project_id: request.payload.project_id.clone(),
        status: TaskStatus::Failed,
        created_at: now(),
        updated_at: now(),
        last_attempt_id: None,
        request: request.payload,
        error: None,
    };
    let text = serde_json::to_string(&old).unwrap();
    db.execute(
        "INSERT INTO tasks VALUES(?1,'FAILED',?2,?3)",
        rusqlite::params![old.task_id.to_string(), old.created_at, text],
    )
    .unwrap();
    drop(db);
    let store = Store::open(&path).unwrap();
    assert_eq!(
        serde_json::to_string(&store.task(old.task_id).unwrap()).unwrap(),
        text
    );
    let db = rusqlite::Connection::open(&path).unwrap();
    assert_eq!(
        db.pragma_query_value(None, "user_version", |r| r.get::<_, i64>(0))
            .unwrap(),
        11
    );
}

#[test]
fn stopped_integration_is_durable_and_cannot_be_overwritten_by_a_late_worker() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("managed.sqlite3");
    let store = Store::open(&path).unwrap();
    let id = Uuid::new_v4();
    let running = serde_json::json!({"state":"repair_running","repair_task_ids":[Uuid::new_v4()]});
    store.managed_create("integration", id, &running).unwrap();
    assert_eq!(
        store
            .managed_create("integration", id, &running)
            .unwrap_err()
            .code,
        ErrorCode::TaskConflict
    );
    let mut cancelled = running.clone();
    cancelled["state"] = serde_json::json!("cancelled");
    assert!(store.integration_save(id, &cancelled).unwrap());
    assert!(!store.integration_save(id, &running).unwrap());
    drop(store);
    let store = Store::open(&path).unwrap();
    assert_eq!(
        store
            .managed_get::<serde_json::Value>("integration", id)
            .unwrap(),
        cancelled
    );
}

#[test]
fn lost_submission_response_recovers_exact_reservation_after_restart() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("origin.sqlite3");
    let receiver = Store::open(&dir.path().join("receiver.sqlite3")).expect("receiver");
    let peer = Uuid::new_v4();
    let request = request();
    let origin = Store::open(&path).expect("origin");
    origin.remember_outbound(peer, &request).expect("reserve");
    let accepted = receiver
        .admit(peer, &request, false, 20)
        .expect("accepted but response lost");
    receiver.claim_next().expect("claim").expect("task");
    drop(origin);
    let origin = Store::open(&path).expect("reopen");
    let (recovered_peer, recovered) = origin
        .outbound_request(request.request_id)
        .expect("recover");
    assert_eq!(recovered_peer, peer);
    assert_eq!(
        serde_json::to_value(&recovered).unwrap(),
        serde_json::to_value(&request).unwrap()
    );
    let replay = receiver.admit(peer, &recovered, false, 20).expect("dedup");
    assert_eq!(replay.task_id, accepted.task_id);
    assert_eq!(receiver.attempt_count(accepted.task_id).unwrap(), 1);
    assert!(origin.outbound_request(Uuid::new_v4()).is_err());
}

#[test]
fn event_cursor_rejects_negative_values_without_mutating_history() {
    let dir = tempfile::tempdir().expect("tempdir");
    let store = Store::open(&dir.path().join("events.sqlite3")).unwrap();
    let task = store.admit(Uuid::new_v4(), &request(), false, 20).unwrap();
    assert!(store.events(task.task_id, -1).is_err());
    let events = store.events(task.task_id, 0).unwrap();
    let last = events.last().unwrap().seq;
    assert!(store.events(task.task_id, last).unwrap().is_empty());
}

#[test]
fn event_budget_preserves_history_and_allows_terminal_status() {
    let dir = tempfile::tempdir().expect("tempdir");
    let store = Store::open(&dir.path().join("budget.sqlite3")).unwrap();
    let task = store.admit(Uuid::new_v4(), &request(), false, 20).unwrap();
    store.claim_next().unwrap().unwrap();
    let mut limited = false;
    for _ in 0..300 {
        if let Err(error) =
            store.append_event(task.task_id, "run", json!({"text":"x".repeat(60000)}))
        {
            assert_eq!(error.code, ErrorCode::RateLimited);
            limited = true;
            break;
        }
    }
    assert!(limited);
    store
        .transition(task.task_id, TaskStatus::Failed, None)
        .unwrap();
    assert_eq!(store.task(task.task_id).unwrap().status, TaskStatus::Failed);
    assert_eq!(store.events(task.task_id, 0).unwrap()[0].seq, 1);
}

#[test]
fn pending_approval_expiry_is_durable_and_never_creates_an_attempt() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("expiry.sqlite3");
    let store = Store::open(&path).unwrap();
    let envelope = request();
    let task = store.admit(Uuid::new_v4(), &envelope, true, 20).unwrap();
    assert_eq!(
        store
            .expire_pending_approvals_at(60, task.created_at + 59)
            .unwrap(),
        0
    );
    assert_eq!(
        store
            .expire_pending_approvals_at(60, task.created_at + 60)
            .unwrap(),
        1
    );
    assert_eq!(
        store.task(task.task_id).unwrap().status,
        TaskStatus::Expired
    );
    assert_eq!(store.attempt_count(task.task_id).unwrap(), 0);
    assert!(store.approve_pending(task.task_id, 60).is_err());
    drop(store);
    let store = Store::open(&path).unwrap();
    assert_eq!(
        store
            .admit(Uuid::new_v4(), &envelope, true, 20)
            .unwrap()
            .status,
        TaskStatus::Expired
    );
    assert!(store.claim_next().unwrap().is_none());
}

#[test]
fn approval_rechecks_deadline_without_waiting_for_background_expiry() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("late-approval.sqlite3");
    let store = Store::open(&path).unwrap();
    let mut task = store.admit(Uuid::new_v4(), &request(), true, 20).unwrap();
    // Simulate an old pending task without a timer or a sweep racing the approval.
    task.created_at -= 120;
    let db = rusqlite::Connection::open(&path).unwrap();
    db.execute(
        "UPDATE tasks SET created_at=?1,record_json=?2 WHERE task_id=?3",
        rusqlite::params![
            task.created_at,
            serde_json::to_string(&task).unwrap(),
            task.task_id.to_string()
        ],
    )
    .unwrap();
    assert_eq!(
        store.approve_pending(task.task_id, 60).unwrap().status,
        TaskStatus::Expired
    );
    assert!(store.claim_next().unwrap().is_none());
    assert_eq!(store.attempt_count(task.task_id).unwrap(), 0);
    let fresh = store.admit(Uuid::new_v4(), &request(), true, 20).unwrap();
    assert_eq!(
        store.approve_pending(fresh.task_id, 60).unwrap().status,
        TaskStatus::Queued
    );
    assert!(store.approve_pending(fresh.task_id, 60).is_err());
}
#[test]
fn archive_preserves_all_events_cursors_and_future_sequence_numbers() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("archive.sqlite3");
    let store = Store::open(&path).unwrap();
    let task = store.admit(Uuid::new_v4(), &request(), false, 20).unwrap();
    assert!(store.archive_events(task.task_id).is_err());
    store.request_cancel(task.task_id).unwrap();
    let before = store.events(task.task_id, 0).unwrap();
    assert_eq!(store.archive_events(task.task_id).unwrap(), before.len());
    assert_eq!(store.archive_events(task.task_id).unwrap(), 0);
    let after = store.events(task.task_id, 0).unwrap();
    assert_eq!(
        serde_json::to_value(&before).unwrap(),
        serde_json::to_value(&after).unwrap()
    );
    let last = before.last().unwrap().seq;
    let event = store
        .append_event(task.task_id, "test", json!({"after_archive":true}))
        .unwrap();
    assert_eq!(event.seq, last + 1);
    drop(store);
    let store = Store::open(&path).unwrap();
    assert_eq!(store.events(task.task_id, last).unwrap().len(), 1);
    assert_eq!(store.archive_events(task.task_id).unwrap(), 1);
    assert_eq!(
        store.events(task.task_id, 0).unwrap().len(),
        before.len() + 1
    );
}
