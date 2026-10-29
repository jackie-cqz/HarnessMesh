CREATE TABLE outbound_tasks (
    task_id TEXT PRIMARY KEY,
    peer_id TEXT NOT NULL,
    request_id TEXT NOT NULL UNIQUE,
    envelope_json TEXT NOT NULL,
    last_record_json TEXT
);
PRAGMA user_version = 2;
