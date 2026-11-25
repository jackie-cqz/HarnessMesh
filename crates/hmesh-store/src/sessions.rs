//! Interactive sessions and their ordered events.
use super::{Store, decode, encode, internal};
use hmesh_core::sessions::{SessionEvent, SessionRecord};
use hmesh_core::*;
use rusqlite::{OptionalExtension, params};
use serde_json::Value;
use uuid::Uuid;

fn status_text(record: &SessionRecord) -> Result<String> {
    encode(&record.status).map(|s| s.trim_matches('"').to_owned())
}
fn unknown() -> MeshError {
    MeshError::new(ErrorCode::TaskUnknown, "unknown session")
}

impl Store {
    pub fn create_session(&self, record: &SessionRecord) -> Result<()> {
        self.transaction(|tx| {
            let inserted = tx
                .execute(
                    "INSERT OR IGNORE INTO sessions VALUES(?1,?2,?3,?4,?5)",
                    params![
                        record.session_id.to_string(),
                        record.agent_id.to_string(),
                        status_text(record)?,
                        record.updated_at,
                        encode(record)?
                    ],
                )
                .map_err(internal)?;
            if inserted != 1 {
                return Err(MeshError::new(
                    ErrorCode::TaskConflict,
                    "a session with this ID already exists",
                ));
            }
            Ok(())
        })
    }
    pub fn session(&self, id: Uuid) -> Result<SessionRecord> {
        self.read(|c| {
            let text: Option<String> = c
                .query_row(
                    "SELECT record_json FROM sessions WHERE session_id=?1",
                    [id.to_string()],
                    |r| r.get(0),
                )
                .optional()
                .map_err(internal)?;
            decode(&text.ok_or_else(unknown)?)
        })
    }
    /// Replace a session's record with a changed copy.
    pub fn update_session(&self, record: &SessionRecord) -> Result<()> {
        self.transaction(|tx| {
            let changed = tx
                .execute(
                    "UPDATE sessions SET status=?2, updated_at=?3, record_json=?4 WHERE session_id=?1",
                    params![
                        record.session_id.to_string(),
                        status_text(record)?,
                        record.updated_at,
                        encode(record)?
                    ],
                )
                .map_err(internal)?;
            if changed != 1 {
                return Err(unknown());
            }
            Ok(())
        })
    }
    /// Most recently active first.
    pub fn sessions(&self, limit: usize) -> Result<Vec<SessionRecord>> {
        self.read(|c| {
            let mut q = c
                .prepare("SELECT record_json FROM sessions ORDER BY updated_at DESC, rowid DESC LIMIT ?1")
                .map_err(internal)?;
            let rows = q
                .query_map([limit as i64], |r| r.get::<_, String>(0))
                .map_err(internal)?;
            rows.map(|row| decode(&row.map_err(internal)?)).collect()
        })
    }
    /// The latest model use each session of this computer's agents reported
    /// (its `usage` events), newest first, with cost totals per currency.
    /// Sessions whose agent reported nothing are only counted.
    pub fn session_usage_report(&self, limit: usize) -> Result<Value> {
        self.read(|c| {
            let total: i64 = c
                .query_row("SELECT count(*) FROM sessions", [], |r| r.get(0))
                .map_err(internal)?;
            let mut q = c
                .prepare("SELECT s.record_json,e.payload_json,e.created_at FROM session_events e JOIN sessions s ON s.session_id=e.session_id WHERE e.kind='usage' AND e.seq=(SELECT MAX(l.seq) FROM session_events l WHERE l.session_id=e.session_id AND l.kind='usage') ORDER BY e.created_at DESC, e.session_id")
                .map_err(internal)?;
            let rows = q
                .query_map([], |r| {
                    Ok((
                        r.get::<_, String>(0)?,
                        r.get::<_, String>(1)?,
                        r.get::<_, i64>(2)?,
                    ))
                })
                .map_err(internal)?;
            let mut out = Vec::new();
            let mut totals: std::collections::BTreeMap<String, f64> = Default::default();
            for row in rows {
                let (record, usage, at) = row.map_err(internal)?;
                let record: SessionRecord = decode(&record)?;
                let usage: Value = decode(&usage)?;
                if let (Some(amount), Some(currency)) = (
                    usage.pointer("/cost/amount").and_then(Value::as_f64),
                    usage.pointer("/cost/currency").and_then(Value::as_str),
                ) {
                    *totals.entry(currency.to_owned()).or_default() += amount;
                }
                out.push(serde_json::json!({
                    "session_id":record.session_id,
                    "title":record.title,
                    "agent_id":record.agent_id,
                    "agent_name":record.agent_name,
                    "turns":record.turns,
                    "status":record.status,
                    "usage":usage,
                    "reported_at":at,
                }));
            }
            let reported = out.len();
            out.truncate(limit);
            Ok(serde_json::json!({
                "sessions_total":total,
                "sessions_reported":reported,
                "rows":out,
                "row_limit":limit,
                "cost_totals":totals.into_iter().map(|(currency, amount)| serde_json::json!({"currency":currency,"amount":amount})).collect::<Vec<_>>(),
            }))
        })
    }
    pub fn append_session_event(
        &self,
        session_id: Uuid,
        kind: &str,
        payload: &Value,
    ) -> Result<SessionEvent> {
        self.transaction(|tx| {
            let seq: i64 = tx
                .query_row(
                    "SELECT COALESCE(MAX(seq),0)+1 FROM session_events WHERE session_id=?1",
                    [session_id.to_string()],
                    |r| r.get(0),
                )
                .map_err(internal)?;
            let created_at = now();
            tx.execute(
                "INSERT INTO session_events VALUES(?1,?2,?3,?4,?5)",
                params![
                    session_id.to_string(),
                    seq,
                    created_at,
                    kind,
                    encode(payload)?
                ],
            )
            .map_err(internal)?;
            Ok(SessionEvent {
                session_id,
                seq: seq as u64,
                created_at,
                kind: kind.to_owned(),
                payload: payload.clone(),
            })
        })
    }
    /// Append several events at once: all are stored, or none.
    pub fn append_session_events(
        &self,
        session_id: Uuid,
        events: &[(String, Value)],
    ) -> Result<Option<u64>> {
        if events.is_empty() {
            return Ok(None);
        }
        self.transaction(|tx| {
            let mut seq: i64 = tx
                .query_row(
                    "SELECT COALESCE(MAX(seq),0) FROM session_events WHERE session_id=?1",
                    [session_id.to_string()],
                    |r| r.get(0),
                )
                .map_err(internal)?;
            let created_at = now();
            for (kind, payload) in events {
                seq += 1;
                tx.execute(
                    "INSERT INTO session_events VALUES(?1,?2,?3,?4,?5)",
                    params![
                        session_id.to_string(),
                        seq,
                        created_at,
                        kind,
                        encode(payload)?
                    ],
                )
                .map_err(internal)?;
            }
            Ok(Some(seq as u64))
        })
    }
    /// Whether any of a session's events is marked `"replayed": true`.
    pub fn has_replayed_events(&self, id: Uuid) -> Result<bool> {
        self.read(|c| {
            c.query_row(
                "SELECT EXISTS(SELECT 1 FROM session_events WHERE session_id=?1 AND json_extract(payload_json,'$.replayed')=1)",
                [id.to_string()],
                |r| r.get(0),
            )
            .map_err(internal)
        })
    }
    /// The payload of a session's most recent event of one kind.
    pub fn latest_session_event(&self, id: Uuid, kind: &str) -> Result<Option<Value>> {
        self.read(|c| {
            c.query_row(
                "SELECT payload_json FROM session_events WHERE session_id=?1 AND kind=?2 ORDER BY seq DESC LIMIT 1",
                params![id.to_string(), kind],
                |r| r.get::<_, String>(0),
            )
            .optional()
            .map_err(internal)?
            .map(|payload| decode(&payload))
            .transpose()
        })
    }
    pub fn session_events(&self, id: Uuid, after: u64, limit: usize) -> Result<Vec<SessionEvent>> {
        self.read(|c| {
            let mut q = c
                .prepare("SELECT seq,created_at,kind,payload_json FROM session_events WHERE session_id=?1 AND seq>?2 ORDER BY seq LIMIT ?3")
                .map_err(internal)?;
            let rows = q
                .query_map(params![id.to_string(), after as i64, limit as i64], |r| {
                    Ok((
                        r.get::<_, i64>(0)?,
                        r.get::<_, i64>(1)?,
                        r.get::<_, String>(2)?,
                        r.get::<_, String>(3)?,
                    ))
                })
                .map_err(internal)?;
            rows.map(|row| {
                let (seq, created_at, kind, payload) = row.map_err(internal)?;
                Ok(SessionEvent {
                    session_id: id,
                    seq: seq as u64,
                    created_at,
                    kind,
                    payload: decode(&payload)?,
                })
            })
            .collect()
        })
    }
    /// Remember a session this node opened on a paired node.
    pub fn record_remote_session(&self, peer: Uuid, record: &SessionRecord) -> Result<()> {
        self.transaction(|tx| {
            tx.execute(
                "INSERT INTO remote_sessions VALUES(?1,?2,?3,?4) ON CONFLICT(session_id) DO UPDATE SET record_json=excluded.record_json",
                params![record.session_id.to_string(), peer.to_string(), record.created_at, encode(record)?],
            )
            .map_err(internal)?;
            Ok(())
        })
    }
    /// Sessions opened from here on paired nodes, newest first, with the peer.
    pub fn remote_sessions(&self, limit: usize) -> Result<Vec<(Uuid, SessionRecord)>> {
        self.read(|c| {
            let mut q = c
                .prepare("SELECT peer_id, record_json FROM remote_sessions ORDER BY created_at DESC, rowid DESC LIMIT ?1")
                .map_err(internal)?;
            let rows = q
                .query_map([limit as i64], |r| {
                    Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
                })
                .map_err(internal)?;
            rows.map(|row| {
                let (peer, record) = row.map_err(internal)?;
                Ok((
                    Uuid::parse_str(&peer).map_err(internal)?,
                    decode(&record)?,
                ))
            })
            .collect()
        })
    }
    pub fn remote_session_peer(&self, id: Uuid) -> Result<Option<Uuid>> {
        self.read(|c| {
            let peer: Option<String> = c
                .query_row(
                    "SELECT peer_id FROM remote_sessions WHERE session_id=?1",
                    [id.to_string()],
                    |r| r.get(0),
                )
                .optional()
                .map_err(internal)?;
            peer.map(|p| Uuid::parse_str(&p).map_err(internal))
                .transpose()
        })
    }
}
