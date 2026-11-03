CREATE TABLE artifact_sets (
    task_id TEXT PRIMARY KEY REFERENCES tasks(task_id),
    attempt_id TEXT NOT NULL,
    config_digest TEXT NOT NULL,
    created_at INTEGER NOT NULL
);
CREATE TABLE artifact_snapshots (
    task_id TEXT NOT NULL REFERENCES artifact_sets(task_id),
    name TEXT NOT NULL,
    bytes BLOB,
    error TEXT,
    PRIMARY KEY(task_id,name)
);
CREATE TABLE project_dispatch (project_id TEXT PRIMARY KEY, ordinal INTEGER NOT NULL);
CREATE TABLE workspaces (
    task_id TEXT PRIMARY KEY REFERENCES tasks(task_id),
    record_json TEXT NOT NULL
);
PRAGMA user_version=7;
