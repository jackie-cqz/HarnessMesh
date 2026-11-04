CREATE TABLE task_usage (
    task_id TEXT PRIMARY KEY REFERENCES tasks(task_id),
    attempt_id TEXT NOT NULL,
    record_json TEXT NOT NULL
);
CREATE TABLE project_budgets (project_id TEXT PRIMARY KEY, record_json TEXT NOT NULL);
PRAGMA user_version=8;
