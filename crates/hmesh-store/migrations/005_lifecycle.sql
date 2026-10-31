CREATE TABLE archived_task_events (
    task_id TEXT NOT NULL REFERENCES tasks(task_id), seq INTEGER NOT NULL,
    event_json TEXT NOT NULL, PRIMARY KEY(task_id,seq)
);
CREATE TABLE retired_agents (
    agent_id TEXT PRIMARY KEY REFERENCES agents(agent_id), retired_at INTEGER NOT NULL
);
PRAGMA user_version=5;
