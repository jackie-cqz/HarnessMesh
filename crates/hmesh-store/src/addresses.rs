//! Where paired computers can be reached besides their cards: what each one
//! told this computer, what this computer's owner set, and what answered last.
use super::{Store, decode, encode, internal};
use hmesh_core::*;
use rusqlite::{OptionalExtension, params};
use uuid::Uuid;

impl Store {
    /// The addresses a trusted peer told this computer, and the key its mark
    /// on the local network is made with. Only a newer revision replaces the
    /// stored one; an untrusted or unknown peer changes nothing. Whether
    /// anything changed.
    pub fn set_advertised_addresses(&self, addresses: &PeerAddresses) -> Result<bool> {
        self.read(|c| {
            let changed = c
                .execute(
                    "INSERT INTO peer_addresses(peer_node_id,revision,advertised_json,find_key) \
                     SELECT ?1,?2,?3,?4 WHERE EXISTS(SELECT 1 FROM trusted_peers WHERE peer_node_id=?1 AND status='trusted') \
                     ON CONFLICT(peer_node_id) DO UPDATE SET revision=excluded.revision,advertised_json=excluded.advertised_json,find_key=excluded.find_key \
                     WHERE excluded.revision>peer_addresses.revision",
                    params![
                        addresses.node_id.to_string(),
                        addresses.revision,
                        encode(&addresses.addresses)?,
                        addresses.find_key
                    ],
                )
                .map_err(internal)?;
            Ok(changed == 1)
        })
    }
    /// The key a trusted peer's mark on the local network is made with, if
    /// it told one.
    pub fn peer_find_key(&self, node_id: Uuid) -> Result<Option<String>> {
        self.read(|c| {
            let key: Option<Option<String>> = c
                .query_row(
                    "SELECT a.find_key FROM peer_addresses a JOIN trusted_peers p ON p.peer_node_id=a.peer_node_id \
                     WHERE a.peer_node_id=?1 AND p.status='trusted'",
                    [node_id.to_string()],
                    |r| r.get(0),
                )
                .optional()
                .map_err(internal)?;
            Ok(key.flatten())
        })
    }
    /// A trusted peer was found at `address` (on the local network) and
    /// answered there: kept first among the addresses it told, until it tells
    /// a newer list.
    pub fn add_found_address(&self, node_id: Uuid, address: &str) -> Result<()> {
        self.transaction(|tx| {
            let trusted: bool = tx
                .query_row(
                    "SELECT EXISTS(SELECT 1 FROM trusted_peers WHERE peer_node_id=?1 AND status='trusted')",
                    [node_id.to_string()],
                    |r| r.get(0),
                )
                .map_err(internal)?;
            if !trusted {
                return Ok(());
            }
            let told: Option<String> = tx
                .query_row(
                    "SELECT advertised_json FROM peer_addresses WHERE peer_node_id=?1",
                    [node_id.to_string()],
                    |r| r.get(0),
                )
                .optional()
                .map_err(internal)?;
            let mut addresses: Vec<String> = match told {
                Some(told) => decode(&told)?,
                None => Vec::new(),
            };
            addresses.retain(|known| known != address);
            addresses.insert(0, address.to_owned());
            addresses.truncate(MAX_PEER_ADDRESSES);
            tx.execute(
                "INSERT INTO peer_addresses(peer_node_id,advertised_json) VALUES(?1,?2) \
                 ON CONFLICT(peer_node_id) DO UPDATE SET advertised_json=excluded.advertised_json",
                params![node_id.to_string(), encode(&addresses)?],
            )
            .map_err(internal)?;
            Ok(())
        })
    }
    /// One of this computer's own secrets, by name.
    pub fn node_secret(&self, name: &str) -> Result<Option<Vec<u8>>> {
        self.read(|c| {
            c.query_row(
                "SELECT value FROM node_secrets WHERE name=?1",
                [name],
                |r| r.get(0),
            )
            .optional()
            .map_err(internal)
        })
    }
    /// Keep one of this computer's own secrets, replacing the one before.
    pub fn set_node_secret(&self, name: &str, value: &[u8]) -> Result<()> {
        self.read(|c| {
            c.execute(
                "INSERT INTO node_secrets(name,value) VALUES(?1,?2) \
                 ON CONFLICT(name) DO UPDATE SET value=excluded.value",
                params![name, value],
            )
            .map_err(internal)?;
            Ok(())
        })
    }
    /// The addresses this computer's owner set for a peer; empty clears them.
    pub fn set_manual_addresses(&self, node_id: Uuid, addresses: &[String]) -> Result<()> {
        self.read(|c| {
            let changed = c
                .execute(
                    "INSERT INTO peer_addresses(peer_node_id,manual_json) \
                     SELECT ?1,?2 WHERE EXISTS(SELECT 1 FROM trusted_peers WHERE peer_node_id=?1) \
                     ON CONFLICT(peer_node_id) DO UPDATE SET manual_json=excluded.manual_json",
                    params![node_id.to_string(), encode(&addresses)?],
                )
                .map_err(internal)?;
            if changed == 0 {
                return Err(MeshError::new(ErrorCode::PeerUnreachable, "peer not found"));
            }
            Ok(())
        })
    }
    /// The address where the trusted peer with this certificate answered last.
    pub fn remember_route(&self, fingerprint: &str, endpoint: &str, at: i64) -> Result<()> {
        self.read(|c| {
            c.execute(
                "INSERT INTO peer_addresses(peer_node_id,last_good,last_good_at) \
                 SELECT peer_node_id,?2,?3 FROM trusted_peers WHERE public_key_fingerprint=?1 AND status='trusted' \
                 ON CONFLICT(peer_node_id) DO UPDATE SET last_good=excluded.last_good,last_good_at=excluded.last_good_at",
                params![fingerprint, endpoint, at],
            )
            .map_err(internal)?;
            Ok(())
        })
    }
    /// How a peer can be reached besides its card; nothing known is empty.
    pub fn peer_routes(&self, node_id: Uuid) -> Result<PeerRoutes> {
        self.read(|c| {
            let row: Option<(String, String, Option<String>, Option<i64>)> = c
                .query_row(
                    "SELECT advertised_json,manual_json,last_good,last_good_at FROM peer_addresses WHERE peer_node_id=?1",
                    [node_id.to_string()],
                    |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
                )
                .optional()
                .map_err(internal)?;
            let Some((advertised, manual, last_good, last_good_at)) = row else {
                return Ok(PeerRoutes::default());
            };
            Ok(PeerRoutes {
                advertised: decode(&advertised)?,
                manual: decode(&manual)?,
                last_good,
                last_good_at,
            })
        })
    }
}
