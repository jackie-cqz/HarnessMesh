-- Folders sessions can work in, and this computer's own marks on sessions.
CREATE TABLE projects (
    project_id TEXT PRIMARY KEY,
    path TEXT NOT NULL UNIQUE,
    name TEXT NOT NULL,
    source TEXT NOT NULL,
    pinned INTEGER NOT NULL DEFAULT 0,
    hidden INTEGER NOT NULL DEFAULT 0,
    shared INTEGER NOT NULL DEFAULT 0,
    position INTEGER NOT NULL DEFAULT 0,
    created_at INTEGER NOT NULL
);
CREATE TABLE session_marks (
    session_id TEXT PRIMARY KEY,
    title TEXT,
    pinned INTEGER NOT NULL DEFAULT 0,
    hidden INTEGER NOT NULL DEFAULT 0,
    updated_at INTEGER NOT NULL
);
PRAGMA user_version=10;
