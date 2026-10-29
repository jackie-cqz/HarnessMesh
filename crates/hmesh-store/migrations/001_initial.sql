CREATE TABLE nodes (node_id TEXT PRIMARY KEY, card_json TEXT NOT NULL);
CREATE TABLE trusted_peers (
    peer_node_id TEXT PRIMARY KEY, public_key_fingerprint TEXT NOT NULL UNIQUE,
    status TEXT NOT NULL, approved_at INTEGER, card_json TEXT NOT NULL
);
CREATE TABLE pair_challenges (nonce TEXT PRIMARY KEY, expires_at INTEGER NOT NULL, consumed INTEGER NOT NULL DEFAULT 0);
CREATE TABLE pair_requests (request_id TEXT PRIMARY KEY, record_json TEXT NOT NULL);
CREATE TABLE agents (agent_id TEXT PRIMARY KEY, name TEXT NOT NULL UNIQUE, project_id TEXT NOT NULL, card_json TEXT NOT NULL, config_json TEXT NOT NULL);
CREATE TABLE tasks (task_id TEXT PRIMARY KEY, status TEXT NOT NULL, created_at INTEGER NOT NULL, record_json TEXT NOT NULL);
CREATE TABLE task_attempts (
    attempt_id TEXT PRIMARY KEY, task_id TEXT NOT NULL REFERENCES tasks(task_id),
    runtime_pid INTEGER, status TEXT NOT NULL, started_at INTEGER NOT NULL,
    ended_at INTEGER, recovery_reason TEXT
);
CREATE TABLE task_events (
    task_id TEXT NOT NULL REFERENCES tasks(task_id), seq INTEGER NOT NULL,
    event_json TEXT NOT NULL, PRIMARY KEY(task_id, seq)
);
CREATE TABLE request_dedup (
    sender_node_id TEXT NOT NULL, request_id TEXT NOT NULL,
    task_id TEXT NOT NULL REFERENCES tasks(task_id), payload_json TEXT NOT NULL,
    PRIMARY KEY(sender_node_id, request_id)
);
CREATE TABLE audit_records (
    id TEXT PRIMARY KEY, actor_node_id TEXT, action TEXT NOT NULL,
    target TEXT NOT NULL, decision TEXT NOT NULL, trace_id TEXT NOT NULL,
    created_at INTEGER NOT NULL
);
CREATE TABLE verifications (
    verification_id TEXT PRIMARY KEY, task_id TEXT NOT NULL REFERENCES tasks(task_id),
    status TEXT NOT NULL, evidence_json TEXT NOT NULL, created_at INTEGER NOT NULL
);
CREATE INDEX tasks_queue ON tasks(status, created_at);
PRAGMA user_version = 1;
