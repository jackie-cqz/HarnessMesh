use hmesh_core::sessions::SessionRecord;
use hmesh_store::{QUEUED_MESSAGE, Store};
use serde_json::json;
use uuid::Uuid;

fn session(store: &Store) -> Uuid {
    let id = Uuid::new_v4();
    let node = Uuid::new_v4();
    let record: SessionRecord = serde_json::from_value(json!({
        "session_id":id,"node_id":node,"agent_id":Uuid::new_v4(),"agent_name":"fixture",
        "project_id":"p","approver":"origin","status":"running","title":"","turns":0,
        "origin":{"kind":"human","node_id":node,"root_node_id":node,"chain":[],"label":null},
        "created_at":1,"updated_at":1
    }))
    .unwrap();
    store.create_session(&record).unwrap();
    id
}

/// Queued messages become user messages after everything recorded so far,
/// numbered past the queued rows so a reader that already saw them misses
/// nothing; a replayed history is found by its mark.
#[test]
fn queued_messages_are_released_once_after_the_history() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(&dir.path().join("state.sqlite3")).unwrap();
    let id = session(&store);
    assert_eq!(store.release_queued_messages(id).unwrap(), None);
    assert!(!store.has_replayed_events(id).unwrap());
    store
        .append_session_event(id, QUEUED_MESSAGE, &json!({"text":"first"}))
        .unwrap();
    store
        .append_session_event(id, QUEUED_MESSAGE, &json!({"text":"second"}))
        .unwrap();
    let last = store
        .append_session_events(
            id,
            &[
                (
                    "user_message".into(),
                    json!({"text":"earlier","replayed":true}),
                ),
                (
                    "agent_message_chunk".into(),
                    json!({"text":"answer","replayed":true}),
                ),
            ],
        )
        .unwrap();
    assert_eq!(last, Some(4));
    assert!(store.has_replayed_events(id).unwrap());
    assert_eq!(store.release_queued_messages(id).unwrap(), Some(6));
    // Nothing is left to release.
    assert_eq!(store.release_queued_messages(id).unwrap(), None);
    let events = store.session_events(id, 0, 100).unwrap();
    let shown: Vec<(u64, &str, &str)> = events
        .iter()
        .map(|e| {
            (
                e.seq,
                e.kind.as_str(),
                e.payload["text"].as_str().unwrap_or(""),
            )
        })
        .collect();
    assert_eq!(
        shown,
        [
            (3, "user_message", "earlier"),
            (4, "agent_message_chunk", "answer"),
            (5, "user_message", "first"),
            (6, "user_message", "second"),
        ]
    );
    // A reader already past the queued rows still sees the released ones.
    assert_eq!(store.session_events(id, 2, 100).unwrap().len(), 4);
    assert_eq!(store.session_events(id, 4, 100).unwrap().len(), 2);
}

/// Released after a queued row that was the last event: numbered past it.
#[test]
fn a_released_message_is_numbered_past_the_queued_row() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(&dir.path().join("state.sqlite3")).unwrap();
    let id = session(&store);
    store
        .append_session_event(id, "status", &json!({"status":"running"}))
        .unwrap();
    store
        .append_session_event(id, QUEUED_MESSAGE, &json!({"text":"held"}))
        .unwrap();
    assert_eq!(store.release_queued_messages(id).unwrap(), Some(3));
    let after = store.session_events(id, 2, 100).unwrap();
    assert_eq!(after.len(), 1);
    assert_eq!(after[0].kind, "user_message");
}
