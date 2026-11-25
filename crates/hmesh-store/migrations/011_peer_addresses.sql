-- Where paired computers can be reached besides their cards: the addresses
-- each one told this computer, the ones this computer's owner set, and the
-- last address that answered. Kept when a pairing is revoked (pairing again
-- finds them), and used only for peers that are trusted.
CREATE TABLE peer_addresses (
    peer_node_id TEXT PRIMARY KEY,
    revision INTEGER NOT NULL DEFAULT 0,
    advertised_json TEXT NOT NULL DEFAULT '[]',
    manual_json TEXT NOT NULL DEFAULT '[]',
    last_good TEXT,
    last_good_at INTEGER
);
PRAGMA user_version=11;
