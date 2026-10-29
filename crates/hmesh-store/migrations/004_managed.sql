CREATE TABLE managed_records (
    kind TEXT NOT NULL,
    id TEXT NOT NULL,
    record_json TEXT NOT NULL,
    PRIMARY KEY(kind,id)
);
PRAGMA user_version=4;
