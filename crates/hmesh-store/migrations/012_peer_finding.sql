-- Finding paired computers again on the local network. node_secrets holds
-- this computer's own secrets by name (its find key: the mark it announces
-- is made with it). find_key is the key a paired computer told this one,
-- kept with the rest of what it told (its address list), so this computer
-- can tell that computer's mark from anyone else's.
CREATE TABLE node_secrets (
    name TEXT PRIMARY KEY,
    value BLOB NOT NULL
);
ALTER TABLE peer_addresses ADD COLUMN find_key TEXT;
PRAGMA user_version=12;
