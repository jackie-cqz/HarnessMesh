CREATE TABLE tool_submissions (
    parent_task_id TEXT NOT NULL REFERENCES tasks(task_id),
    request_id TEXT NOT NULL,
    child_task_id TEXT NOT NULL UNIQUE,
    peer_id TEXT NOT NULL,
    payload_json TEXT NOT NULL,
    PRIMARY KEY(parent_task_id, request_id)
);
PRAGMA user_version = 3;
