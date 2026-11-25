//! Where paired computers can be reached besides their cards: what each one
//! told this computer, what this computer's owner set, and what answered last.
use super::{Store, decode, encode, internal};
use hmesh_core::*;
use rusqlite::{OptionalExtension, params};
use uuid::Uuid;

impl Store {
    /// The addresses a trusted peer told this computer. Only a newer revision
    /// replaces the stored one; an untrusted or unknown peer changes
    /// nothing. Whether anything changed.
    pub fn set_advertised_addresses(&self, addresses: &PeerAddresses) -> Result<bool> {
        self.read(|c| {
            let changed = c
                .execute(
                    "INSERT INTO peer_addresses(peer_node_id,revision,advertised_json) \
                     SELECT ?1,?2,?3 WHERE EXISTS(SELECT 1 FROM trusted_peers WHERE peer_node_id=?1 AND status='trusted') \
                     ON CONFLICT(peer_node_id) DO UPDATE SET revision=excluded.revision,advertised_json=excluded.advertised_json \
                     WHERE excluded.revision>peer_addresses.revision",
                    params![
                        addresses.node_id.to_string(),
                        addresses.revision,
                        encode(&addresses.addresses)?
                    ],
                )
                .map_err(internal)?;
            Ok(changed == 1)
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
