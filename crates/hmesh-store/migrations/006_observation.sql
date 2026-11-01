ALTER TABLE outbound_tasks ADD COLUMN last_observed_at INTEGER;
PRAGMA user_version=6;
