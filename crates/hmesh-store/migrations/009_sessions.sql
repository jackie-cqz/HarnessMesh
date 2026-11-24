-- Interactive ACP sessions and their ordered events.
CREATE TABLE sessions (
    session_id TEXT PRIMARY KEY,
    agent_id TEXT NOT NULL,
    status TEXT NOT NULL,
    updated_at INTEGER NOT NULL,
    record_json TEXT NOT NULL
);
CREATE INDEX sessions_by_agent ON sessions(agent_id, updated_at);
CREATE TABLE session_events (
    session_id TEXT NOT NULL REFERENCES sessions(session_id) ON DELETE CASCADE,
    seq INTEGER NOT NULL,
    created_at INTEGER NOT NULL,
    kind TEXT NOT NULL,
    payload_json TEXT NOT NULL,
    PRIMARY KEY (session_id, seq)
);
-- Sessions this node opened on a paired node, so they can be listed here.
CREATE TABLE remote_sessions (
    session_id TEXT PRIMARY KEY,
    peer_id TEXT NOT NULL,
    created_at INTEGER NOT NULL,
    record_json TEXT NOT NULL
);
PRAGMA user_version=9;
