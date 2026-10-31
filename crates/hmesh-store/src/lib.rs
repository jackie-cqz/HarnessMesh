//! Versioned SQLite state. Admission, state changes and events commit atomically.
use hmesh_core::*;
use rusqlite::{Connection, OptionalExtension, Transaction, TransactionBehavior, params};
use serde::{Serialize, de::DeserializeOwned};
use serde_json::{Value, json};
use std::{
    path::Path,
    sync::{Arc, Mutex},
    time::Duration,
};
use uuid::Uuid;

#[derive(Clone)]
pub struct Store {
    connection: Arc<Mutex<Connection>>,
}
fn internal(e: impl std::fmt::Display) -> MeshError {
    MeshError::new(ErrorCode::Internal, format!("persistence: {e}"))
}
fn encode(v: &impl serde::Serialize) -> Result<String> {
    serde_json::to_string(v).map_err(internal)
}
fn decode<T: DeserializeOwned>(s: &str) -> Result<T> {
    serde_json::from_str(s).map_err(internal)
}
fn status(s: TaskStatus) -> Result<String> {
    encode(&s).map(|s| s.trim_matches('"').to_owned())
}

impl Store {
    pub fn open(path: &Path) -> Result<Self> {
        let mut conn = Connection::open(path).map_err(internal)?;
        conn.busy_timeout(Duration::from_secs(5))
            .map_err(internal)?;
        conn.execute_batch(
            "PRAGMA foreign_keys=ON; PRAGMA journal_mode=WAL; PRAGMA synchronous=FULL;",
        )
        .map_err(internal)?;
        let version: i64 = conn
            .pragma_query_value(None, "user_version", |r| r.get(0))
            .map_err(internal)?;
        if version > 5 {
            return Err(MeshError::new(
                ErrorCode::UnsupportedVersion,
                "database is newer than this binary; refusing to alter it",
            ));
        }
        if version == 0 {
            let tx = conn
                .transaction_with_behavior(TransactionBehavior::Immediate)
                .map_err(internal)?;
            tx.execute_batch(include_str!("../migrations/001_initial.sql"))
                .map_err(internal)?;
            tx.commit().map_err(internal)?;
        }
        if version < 2 {
            let tx = conn
                .transaction_with_behavior(TransactionBehavior::Immediate)
                .map_err(internal)?;
            tx.execute_batch(include_str!("../migrations/002_outbound.sql"))
                .map_err(internal)?;
            tx.commit().map_err(internal)?;
        }
        if version < 3 {
            let tx = conn
                .transaction_with_behavior(TransactionBehavior::Immediate)
                .map_err(internal)?;
            tx.execute_batch(include_str!("../migrations/003_tool_submissions.sql"))
                .map_err(internal)?;
            tx.commit().map_err(internal)?;
        }
        if version < 4 {
            let tx = conn
                .transaction_with_behavior(TransactionBehavior::Immediate)
                .map_err(internal)?;
            tx.execute_batch(include_str!("../migrations/004_managed.sql"))
                .map_err(internal)?;
            tx.commit().map_err(internal)?;
        }
        if version < 5 {
            let tx = conn
                .transaction_with_behavior(TransactionBehavior::Immediate)
                .map_err(internal)?;
            tx.execute_batch(include_str!("../migrations/005_lifecycle.sql"))
                .map_err(internal)?;
            tx.commit().map_err(internal)?;
        }
        Ok(Self {
            connection: Arc::new(Mutex::new(conn)),
        })
    }
    fn read<T>(&self, f: impl FnOnce(&Connection) -> Result<T>) -> Result<T> {
        let conn = self
            .connection
            .lock()
            .map_err(|_| internal("poisoned database lock"))?;
        f(&conn)
    }
    fn transaction<T>(&self, f: impl FnOnce(&Transaction<'_>) -> Result<T>) -> Result<T> {
        let mut conn = self
            .connection
            .lock()
            .map_err(|_| internal("poisoned database lock"))?;
        let tx = conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(internal)?;
        let result = f(&tx)?;
        tx.commit().map_err(internal)?;
        Ok(result)
    }
    pub fn managed_create<T: Serialize>(&self, kind: &str, id: Uuid, record: &T) -> Result<()> {
        self.transaction(|tx| {
            let count: i64 = tx
                .query_row(
                    "SELECT count(*) FROM managed_records WHERE kind=?1",
                    [kind],
                    |r| r.get(0),
                )
                .map_err(internal)?;
            if count >= 64 {
                return Err(MeshError::new(
                    ErrorCode::RateLimited,
                    "managed record limit reached",
                ));
            }
            let inserted = tx
                .execute(
                    "INSERT OR IGNORE INTO managed_records VALUES(?1,?2,?3)",
                    params![kind, id.to_string(), encode(record)?],
                )
                .map_err(internal)?;
            if inserted != 1 {
                return Err(MeshError::new(
                    ErrorCode::TaskConflict,
                    "managed id already exists; query its existing state",
                ));
            }
            Ok(())
        })
    }
    pub fn managed_put<T: Serialize>(&self, kind: &str, id: Uuid, record: &T) -> Result<()> {
        self.read(|c| {
            let changed = c
                .execute(
                    "UPDATE managed_records SET record_json=?3 WHERE kind=?1 AND id=?2",
                    params![kind, id.to_string(), encode(record)?],
                )
                .map_err(internal)?;
            if changed != 1 {
                return Err(MeshError::input("managed record not found"));
            }
            Ok(())
        })
    }
    pub fn integration_save(&self, id: Uuid, record: &impl Serialize) -> Result<bool> {
        self.read(|c| Ok(c.execute("UPDATE managed_records SET record_json=?2 WHERE kind='integration' AND id=?1 AND json_extract(record_json,'$.state') NOT IN ('verified','needs_input','cancelled')",params![id.to_string(),encode(record)?]).map_err(internal)?==1))
    }
    pub fn managed_get<T: DeserializeOwned>(&self, kind: &str, id: Uuid) -> Result<T> {
        self.read(|c| {
            let value: Option<String> = c
                .query_row(
                    "SELECT record_json FROM managed_records WHERE kind=?1 AND id=?2",
                    params![kind, id.to_string()],
                    |r| r.get(0),
                )
                .optional()
                .map_err(internal)?;
            decode(&value.ok_or_else(|| MeshError::input("managed record not found"))?)
        })
    }
    pub fn managed_list<T: DeserializeOwned>(&self, kind: &str) -> Result<Vec<T>> {
        self.read(|c| {
            let mut q = c
                .prepare("SELECT record_json FROM managed_records WHERE kind=?1 ORDER BY id")
                .map_err(internal)?;
            q.query_map([kind], |r| r.get::<_, String>(0))
                .map_err(internal)?
                .map(|r| decode(&r.map_err(internal)?))
                .collect()
        })
    }
    pub fn set_node(&self, card: &NodeCard) -> Result<()> {
        self.read(|c| { c.execute("INSERT INTO nodes VALUES(?1,?2) ON CONFLICT(node_id) DO UPDATE SET card_json=excluded.card_json", params![card.node_id.to_string(), encode(card)?]).map_err(internal)?; Ok(()) })
    }
    pub fn peers(&self) -> Result<Vec<Peer>> {
        self.read(|c| {
            let mut q = c
                .prepare(
                    "SELECT card_json,status,approved_at FROM trusted_peers ORDER BY peer_node_id",
                )
                .map_err(internal)?;
            q.query_map([], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, Option<i64>>(2)?,
                ))
            })
            .map_err(internal)?
            .map(|r| {
                let (card, status, approved_at) = r.map_err(internal)?;
                Ok(Peer {
                    card: decode(&card)?,
                    status,
                    approved_at,
                })
            })
            .collect()
        })
    }
    pub fn peer_by_fingerprint(&self, fingerprint: &str) -> Result<Option<Peer>> {
        Ok(self
            .peers()?
            .into_iter()
            .find(|p| p.status == "trusted" && p.card.fingerprint == fingerprint))
    }
    pub fn peer(&self, node_id: Uuid) -> Result<Peer> {
        self.peers()?
            .into_iter()
            .find(|p| p.card.node_id == node_id)
            .ok_or_else(|| MeshError::new(ErrorCode::PeerUnreachable, "peer not found"))
    }
    pub fn trust_peer(&self, card: &NodeCard) -> Result<()> {
        self.transaction(|tx| {
            let old: Option<String> = tx.query_row("SELECT public_key_fingerprint FROM trusted_peers WHERE peer_node_id=?1", [card.node_id.to_string()], |r| r.get(0)).optional().map_err(internal)?;
            if old.is_some_and(|old| old != card.fingerprint) { return Err(MeshError::new(ErrorCode::TaskConflict, "node key changed; revoke and explicitly rotate identity")); }
            tx.execute("INSERT INTO trusted_peers VALUES(?1,?2,'trusted',?3,?4) ON CONFLICT(peer_node_id) DO UPDATE SET status='trusted',approved_at=excluded.approved_at,card_json=excluded.card_json", params![card.node_id.to_string(), card.fingerprint, now(), encode(card)?]).map_err(internal)?;
            Ok(())
        })
    }
    pub fn revoke_peer(&self, node_id: Uuid) -> Result<()> {
        self.read(|c| {
            c.execute(
                "UPDATE trusted_peers SET status='revoked' WHERE peer_node_id=?1",
                [node_id.to_string()],
            )
            .map_err(internal)?;
            Ok(())
        })
    }
    pub fn challenge(&self, node_id: Uuid) -> Result<PairChallenge> {
        let challenge = PairChallenge {
            nonce: Uuid::new_v4(),
            node_id,
            expires_at: now() + 120,
        };
        self.read(|c| {
            c.execute("DELETE FROM pair_challenges WHERE expires_at < ?1", [now()])
                .map_err(internal)?;
            c.execute(
                "INSERT INTO pair_challenges(nonce,expires_at) VALUES(?1,?2)",
                params![challenge.nonce.to_string(), challenge.expires_at],
            )
            .map_err(internal)?;
            Ok(())
        })?;
        Ok(challenge)
    }
    pub fn accept_pair(&self, submission: &PairSubmission) -> Result<PairRecord> {
        self.transaction(|tx| {
            let changed = tx.execute("UPDATE pair_challenges SET consumed=1 WHERE nonce=?1 AND expires_at=?2 AND expires_at>=?3 AND consumed=0", params![submission.challenge.nonce.to_string(), submission.challenge.expires_at, now()]).map_err(internal)?;
            if changed != 1 { return Err(MeshError::new(ErrorCode::Unauthenticated, "pairing challenge expired, unknown or replayed")); }
            let record = PairRecord { request_id: submission.request_id, card: submission.card.clone(), status: "pending".into(), direction: "incoming".into(), expires_at: now() + 300 };
            tx.execute("INSERT INTO pair_requests VALUES(?1,?2)", params![record.request_id.to_string(), encode(&record)?]).map_err(internal)?;
            Ok(record)
        })
    }
    pub fn save_outgoing_pair(&self, record: &PairRecord) -> Result<()> {
        self.read(|c| {
            c.execute(
                "INSERT INTO pair_requests VALUES(?1,?2)",
                params![record.request_id.to_string(), encode(record)?],
            )
            .map_err(internal)?;
            Ok(())
        })
    }
    pub fn pairs(&self) -> Result<Vec<PairRecord>> {
        self.read(|c| {
            let mut q = c
                .prepare("SELECT record_json FROM pair_requests ORDER BY request_id")
                .map_err(internal)?;
            q.query_map([], |r| r.get::<_, String>(0))
                .map_err(internal)?
                .map(|r| decode(&r.map_err(internal)?))
                .collect()
        })
    }
    pub fn approve_pair(&self, request_id: Uuid, expected: &str) -> Result<Peer> {
        self.transaction(|tx| {
            let text: Option<String> = tx.query_row("SELECT record_json FROM pair_requests WHERE request_id=?1", [request_id.to_string()], |r| r.get(0)).optional().map_err(internal)?;
            let mut record: PairRecord = decode(&text.ok_or_else(|| MeshError::input("pair request not found"))?)?;
            if record.status != "pending" || record.expires_at < now() || record.card.fingerprint != expected {
                return Err(MeshError::new(ErrorCode::PermissionDenied, "pair request is expired, decided, or fingerprint does not match"));
            }
            let old: Option<String> = tx.query_row("SELECT public_key_fingerprint FROM trusted_peers WHERE peer_node_id=?1", [record.card.node_id.to_string()], |r| r.get(0)).optional().map_err(internal)?;
            if old.is_some_and(|fp| fp != expected) { return Err(MeshError::new(ErrorCode::PermissionDenied, "node identity key change requires explicit rotation")); }
            let peer = Peer { card: record.card.clone(), status: "trusted".into(), approved_at: Some(now()) };
            tx.execute("INSERT INTO trusted_peers VALUES(?1,?2,'trusted',?3,?4) ON CONFLICT(peer_node_id) DO UPDATE SET status='trusted',approved_at=excluded.approved_at,card_json=excluded.card_json", params![peer.card.node_id.to_string(), expected, peer.approved_at, encode(&peer.card)?]).map_err(internal)?;
            record.status = "approved".into();
            tx.execute("UPDATE pair_requests SET record_json=?1 WHERE request_id=?2", params![encode(&record)?, request_id.to_string()]).map_err(internal)?;
            Ok(peer)
        })
    }
    pub fn register_agent(&self, card: &AgentCard, config: &AgentConfig) -> Result<()> {
        config.validate()?;
        self.transaction(|tx| {
            let mut q = tx
                .prepare("SELECT config_json FROM agents WHERE project_id=?1")
                .map_err(internal)?;
            for existing in q
                .query_map([&config.agent.project_id], |r| r.get::<_, String>(0))
                .map_err(internal)?
            {
                let existing: AgentConfig = decode(&existing.map_err(internal)?)?;
                if existing.project.root != config.project.root {
                    return Err(MeshError::input(
                        "project_id is already bound to a different root",
                    ));
                }
            }
            tx.execute(
                "INSERT INTO agents VALUES(?1,?2,?3,?4,?5)",
                params![
                    card.agent_id.to_string(),
                    card.name,
                    config.agent.project_id,
                    encode(card)?,
                    encode(config)?
                ],
            )
            .map_err(internal)?;
            Ok(())
        })
    }
    pub fn replace_agent(&self, card: &AgentCard, config: &AgentConfig) -> Result<()> {
        config.validate()?;
        self.transaction(|tx| {
            if tx.query_row("SELECT EXISTS(SELECT 1 FROM retired_agents WHERE agent_id=?1)", [card.agent_id.to_string()], |r| r.get::<_,bool>(0)).map_err(internal)? { return Err(MeshError::input("restore retired Agent before updating")); }
            let old: String = tx.query_row("SELECT config_json FROM agents WHERE agent_id=?1", [card.agent_id.to_string()], |r| r.get(0)).map_err(internal)?;
            let old: AgentConfig = decode(&old)?;
            if old.project.root != config.project.root || old.agent.project_id != config.agent.project_id {
                return Err(MeshError::input("update cannot change registered project identity or root; register a new agent"));
            }
            let busy: bool = tx.query_row("SELECT EXISTS(SELECT 1 FROM tasks WHERE json_extract(record_json,'$.agent_id')=?1 AND status IN ('PENDING_APPROVAL','QUEUED','RUNNING','CANCELLING','WAITING_INPUT'))", [card.agent_id.to_string()], |r| r.get(0)).map_err(internal)?;
            if busy { return Err(MeshError::new(ErrorCode::TaskConflict, "resolve outstanding tasks before changing agent configuration")); }
            tx.execute("UPDATE agents SET name=?1,card_json=?2,config_json=?3 WHERE agent_id=?4", params![card.name,encode(card)?,encode(config)?,card.agent_id.to_string()]).map_err(internal)?;
            audit_in(tx,None,"agent.update",&card.agent_id.to_string(),"verification-invalidated",Uuid::new_v4())?;
            Ok(())
        })
    }
    pub fn agents(&self) -> Result<Vec<AgentCard>> {
        self.read(|c| {
            let mut q = c
                .prepare("SELECT card_json FROM agents WHERE agent_id NOT IN (SELECT agent_id FROM retired_agents) ORDER BY name")
                .map_err(internal)?;
            q.query_map([], |r| r.get::<_, String>(0))
                .map_err(internal)?
                .map(|r| decode(&r.map_err(internal)?))
                .collect()
        })
    }
    pub fn agent(&self, id: Uuid) -> Result<(AgentCard, AgentConfig)> {
        self.read(|c| {
            let row: Option<(String, String)> = c
                .query_row(
                    "SELECT card_json,config_json FROM agents WHERE agent_id=?1",
                    [id.to_string()],
                    |r| Ok((r.get(0)?, r.get(1)?)),
                )
                .optional()
                .map_err(internal)?;
            let (card, config) = row.ok_or_else(|| {
                MeshError::new(ErrorCode::AgentUnavailable, "agent not registered")
            })?;
            Ok((decode(&card)?, decode(&config)?))
        })
    }
    fn mark_agent_verified_in(tx: &Transaction<'_>, id: Uuid) -> Result<()> {
        let text: String = tx
            .query_row(
                "SELECT card_json FROM agents WHERE agent_id=?1",
                [id.to_string()],
                |r| r.get(0),
            )
            .map_err(internal)?;
        let mut card: AgentCard = decode(&text)?;
        card.probe.authentication = "passed_real_run".into();
        card.probe.managed_run = "passed".into();
        card.probe.protocol_probe = "passed_real_run".into();
        card.probe.streaming = "passed_native_json_events".into();
        card.probe.unavailable_reason = None;
        card.availability = "idle".into();
        tx.execute(
            "UPDATE agents SET card_json=?1 WHERE agent_id=?2",
            params![encode(&card)?, id.to_string()],
        )
        .map_err(internal)?;
        occupancy_in(tx, id)?;
        Ok(())
    }
    pub fn set_artifact_paths(&self, id: Uuid, paths: &[std::path::PathBuf]) -> Result<()> {
        if paths.len() > 16
            || paths.iter().any(|p| {
                p.as_os_str().is_empty()
                    || p.components()
                        .any(|c| !matches!(c, std::path::Component::Normal(_)))
            })
        {
            return Err(MeshError::input(
                "artifact grants must contain at most 16 relative paths without traversal",
            ));
        }
        self.transaction(|tx| {
            let text: String = tx
                .query_row(
                    "SELECT config_json FROM agents WHERE agent_id=?1",
                    [id.to_string()],
                    |r| r.get(0),
                )
                .map_err(internal)?;
            let mut config: AgentConfig = decode(&text)?;
            config.project.artifact_paths = paths.to_vec();
            tx.execute(
                "UPDATE agents SET config_json=?1 WHERE agent_id=?2",
                params![encode(&config)?, id.to_string()],
            )
            .map_err(internal)?;
            audit_in(
                tx,
                None,
                "artifact.grant",
                &id.to_string(),
                "owner-approved",
                Uuid::new_v4(),
            )?;
            Ok(())
        })
    }
    pub fn request_cancel(&self, id: Uuid) -> Result<TaskRecord> {
        self.transaction(|tx| {
            let task = task_in(tx, id)?;
            let next = match task.status {
                TaskStatus::PendingApproval | TaskStatus::Queued => TaskStatus::Cancelled,
                TaskStatus::Running | TaskStatus::WaitingInput => TaskStatus::Cancelling,
                TaskStatus::Cancelling | TaskStatus::Cancelled => return Ok(task),
                _ => {
                    return Err(MeshError::new(
                        ErrorCode::TaskConflict,
                        "terminal task cannot be cancelled",
                    ));
                }
            };
            transition_in(tx, task, next, None)
        })
    }
    pub fn remember_outbound(
        &self,
        peer: Uuid,
        envelope: &Envelope<TaskSubmit>,
    ) -> Result<Envelope<TaskSubmit>> {
        self.transaction(|tx| {
            let old: Option<(String, String)> = tx.query_row("SELECT peer_id,envelope_json FROM outbound_tasks WHERE request_id=?1 OR task_id=?2", params![envelope.request_id.to_string(), envelope.payload.task_id.to_string()], |r| Ok((r.get(0)?,r.get(1)?))).optional().map_err(internal)?;
            if let Some((old_peer, old)) = old {
                let old: Envelope<TaskSubmit> = decode(&old)?;
                if old_peer != peer.to_string() || old.request_id != envelope.request_id || encode(&old.payload)? != encode(&envelope.payload)? {
                    return Err(MeshError::new(ErrorCode::TaskConflict, "outbound id reused with a different peer or payload"));
                }
                return Ok(old);
            }
            let local: bool = tx.query_row("SELECT EXISTS(SELECT 1 FROM tasks WHERE task_id=?1)", [envelope.payload.task_id.to_string()], |r| r.get(0)).map_err(internal)?;
            if local { return Err(MeshError::new(ErrorCode::TaskConflict, "task id already identifies a local execution")); }
            tx.execute("INSERT INTO outbound_tasks(task_id,peer_id,request_id,envelope_json) VALUES(?1,?2,?3,?4)", params![envelope.payload.task_id.to_string(), peer.to_string(), envelope.request_id.to_string(), encode(envelope)?]).map_err(internal)?;
            Ok(envelope.clone())
        })
    }
    /// Recover the exact durable outbound reservation without guessing IDs or replaying execution.
    pub fn outbound_request(&self, request: Uuid) -> Result<(Uuid, Envelope<TaskSubmit>)> {
        self.read(|c| {
            let row: Option<(String, String)> = c
                .query_row(
                    "SELECT peer_id,envelope_json FROM outbound_tasks WHERE request_id=?1",
                    [request.to_string()],
                    |r| Ok((r.get(0)?, r.get(1)?)),
                )
                .optional()
                .map_err(internal)?;
            let (peer, envelope) = row.ok_or_else(|| {
                MeshError::input("outbound request ID is not reserved on this node")
            })?;
            Ok((
                Uuid::parse_str(&peer).map_err(internal)?,
                decode(&envelope)?,
            ))
        })
    }
    pub fn outbound_peer(&self, id: Uuid) -> Result<Option<Uuid>> {
        self.read(|c| {
            let peer: Option<String> = c
                .query_row(
                    "SELECT peer_id FROM outbound_tasks WHERE task_id=?1",
                    [id.to_string()],
                    |r| r.get(0),
                )
                .optional()
                .map_err(internal)?;
            peer.map(|peer| Uuid::parse_str(&peer).map_err(internal))
                .transpose()
        })
    }
    pub fn cache_outbound(&self, task: &TaskRecord) -> Result<()> {
        self.read(|c| {
            c.execute(
                "UPDATE outbound_tasks SET last_record_json=?1 WHERE task_id=?2",
                params![encode(task)?, task.task_id.to_string()],
            )
            .map_err(internal)?;
            Ok(())
        })
    }
    pub fn outbound_tasks(&self) -> Result<Vec<Value>> {
        self.read(|c| {
            let mut q = c.prepare("SELECT task_id,peer_id,last_record_json FROM outbound_tasks ORDER BY task_id").map_err(internal)?;
            q.query_map([], |r| Ok((r.get::<_,String>(0)?,r.get::<_,String>(1)?,r.get::<_,Option<String>>(2)?))).map_err(internal)?
                .map(|r| { let (id,peer,record) = r.map_err(internal)?; Ok(json!({"task_id":id,"peer_id":peer,"direction":"outbound","last_observed":record.map(|r| decode::<Value>(&r)).transpose()?})) }).collect()
        })
    }
    pub fn admit(
        &self,
        target: Uuid,
        envelope: &Envelope<TaskSubmit>,
        approval: bool,
        max_queue: usize,
    ) -> Result<TaskRecord> {
        envelope.payload.validate()?;
        self.transaction(|tx| {
            let payload = encode(&envelope.payload)?;
            let duplicate: Option<(String,String)> = tx.query_row("SELECT task_id,payload_json FROM request_dedup WHERE sender_node_id=?1 AND request_id=?2", params![envelope.sender_node_id.to_string(), envelope.request_id.to_string()], |r| Ok((r.get(0)?, r.get(1)?))).optional().map_err(internal)?;
            if let Some((id, old)) = duplicate {
                if old != payload { return Err(MeshError::new(ErrorCode::TaskConflict, "request_id reused with different payload")); }
                return task_in(tx, Uuid::parse_str(&id).map_err(internal)?);
            }
            if tx.query_row("SELECT EXISTS(SELECT 1 FROM retired_agents WHERE agent_id=?1)",[envelope.payload.target_agent_id.to_string()],|r|r.get::<_,bool>(0)).map_err(internal)? { return Err(MeshError::new(ErrorCode::AgentUnavailable,"Agent is retired")); }
            let count: i64 = tx.query_row("SELECT COUNT(*) FROM tasks WHERE status IN ('PENDING_APPROVAL','QUEUED','RUNNING','CANCELLING','WAITING_INPUT')", [], |r| r.get(0)).map_err(internal)?;
            if count >= max_queue as i64 { return Err(MeshError::new(ErrorCode::RateLimited, "local task queue is full")); }
            let task = TaskRecord { task_id: envelope.payload.task_id, origin_node_id: envelope.sender_node_id, target_node_id: target,
                agent_id: envelope.payload.target_agent_id, project_id: envelope.payload.project_id.clone(),
                status: if approval { TaskStatus::PendingApproval } else { TaskStatus::Queued }, created_at: now(), updated_at: now(),
                last_attempt_id: None, request: envelope.payload.clone(), error: None };
            tx.execute("INSERT INTO tasks VALUES(?1,?2,?3,?4)", params![task.task_id.to_string(), status(task.status)?, task.created_at, encode(&task)?]).map_err(|_| MeshError::new(ErrorCode::TaskConflict, "task_id already exists"))?;
            tx.execute("INSERT INTO request_dedup VALUES(?1,?2,?3,?4)", params![envelope.sender_node_id.to_string(), envelope.request_id.to_string(), task.task_id.to_string(), payload]).map_err(internal)?;
            append_in(tx, &task, "status", json!({"status": task.status}))?;
            audit_in(tx, Some(envelope.sender_node_id), "task.submit", &task.task_id.to_string(), if approval { "pending_approval" } else { "accepted" }, envelope.trace_id)?;
            Ok(task)
        })
    }
    pub fn tasks(&self) -> Result<Vec<TaskRecord>> {
        self.read(|c| {
            let mut q = c
                .prepare("SELECT record_json FROM tasks ORDER BY created_at,task_id")
                .map_err(internal)?;
            q.query_map([], |r| r.get::<_, String>(0))
                .map_err(internal)?
                .map(|r| decode(&r.map_err(internal)?))
                .collect()
        })
    }
    pub fn task(&self, id: Uuid) -> Result<TaskRecord> {
        self.read(|c| task_in(c, id))
    }
    pub fn transition(
        &self,
        id: Uuid,
        next: TaskStatus,
        error: Option<MeshError>,
    ) -> Result<TaskRecord> {
        self.transaction(|tx| {
            let task = task_in(tx, id)?;
            if next == TaskStatus::Succeeded {
                Self::mark_agent_verified_in(tx, task.agent_id)?;
            }
            transition_in(tx, task, next, error)
        })
    }
    pub fn claim_next(&self) -> Result<Option<TaskRecord>> {
        self.transaction(|tx| {
            let text: Option<String> = tx.query_row("SELECT record_json FROM tasks WHERE status='QUEUED' ORDER BY created_at,task_id LIMIT 1", [], |r| r.get(0)).optional().map_err(internal)?;
            let Some(text) = text else { return Ok(None); };
            let mut task: TaskRecord = decode(&text)?;
            task.last_attempt_id = Some(Uuid::new_v4());
            let task = transition_in(tx, task, TaskStatus::Running, None)?;
            tx.execute("INSERT INTO task_attempts(attempt_id,task_id,status,started_at) VALUES(?1,?2,'RUNNING',?3)", params![task.last_attempt_id.map(|id| id.to_string()), task.task_id.to_string(), now()]).map_err(internal)?;
            Ok(Some(task))
        })
    }
    pub fn record_pid(&self, attempt: Uuid, pid: u32) -> Result<()> {
        self.read(|c| {
            c.execute(
                "UPDATE task_attempts SET runtime_pid=?1 WHERE attempt_id=?2",
                params![pid, attempt.to_string()],
            )
            .map_err(internal)?;
            Ok(())
        })
    }
    pub fn attempt_count(&self, id: Uuid) -> Result<i64> {
        self.read(|c| {
            c.query_row(
                "SELECT COUNT(*) FROM task_attempts WHERE task_id=?1",
                [id.to_string()],
                |r| r.get(0),
            )
            .map_err(internal)
        })
    }
    pub fn reserve_tool_submission(
        &self,
        parent: Uuid,
        request: Uuid,
        peer: Uuid,
        task: &TaskSubmit,
        maximum: usize,
    ) -> Result<()> {
        task.validate()?;
        self.transaction(|tx| {
            if task_in(tx,parent)?.status != TaskStatus::Running {
                return Err(MeshError::new(ErrorCode::PermissionDenied,"Mesh tools require an active parent run"));
            }
            let payload=encode(task)?;
            let old:Option<(String,String)>=tx.query_row("SELECT peer_id,payload_json FROM tool_submissions WHERE parent_task_id=?1 AND request_id=?2",params![parent.to_string(),request.to_string()],|r|Ok((r.get(0)?,r.get(1)?))).optional().map_err(internal)?;
            if let Some((old_peer,old_payload))=old {
                if old_peer != peer.to_string() || old_payload != payload {return Err(MeshError::new(ErrorCode::TaskConflict,"delegation request reused with different payload"));}
                return Ok(());
            }
            let count:usize=tx.query_row("SELECT COUNT(*) FROM tool_submissions WHERE parent_task_id=?1",[parent.to_string()],|r|r.get(0)).map_err(internal)?;
            if maximum == 0 || count>=maximum {return Err(MeshError::new(ErrorCode::RateLimited,"parent delegation budget exhausted; no automatic retry"));}
            tx.execute("INSERT INTO tool_submissions VALUES(?1,?2,?3,?4,?5)",params![parent.to_string(),request.to_string(),task.task_id.to_string(),peer.to_string(),payload]).map_err(|_|MeshError::new(ErrorCode::TaskConflict,"child task already belongs to a delegation"))?;
            append_in(tx,&task_in(tx,parent)?,"delegation",json!({"child_task_id":task.task_id,"peer_id":peer,"request_id":request,"status":"reserved"}))?;
            Ok(())
        })
    }
    pub fn tool_child_allowed(&self, parent: Uuid, child: Uuid) -> Result<bool> {
        self.read(|c|c.query_row("SELECT EXISTS(SELECT 1 FROM tool_submissions WHERE parent_task_id=?1 AND child_task_id=?2)",params![parent.to_string(),child.to_string()],|r|r.get(0)).map_err(internal))
    }
    pub fn tool_child_target(&self, parent: Uuid, child: Uuid) -> Result<Option<MeshTarget>> {
        self.read(|c|{
            let row:Option<(String,String)>=c.query_row("SELECT peer_id,payload_json FROM tool_submissions WHERE parent_task_id=?1 AND child_task_id=?2",params![parent.to_string(),child.to_string()],|r|Ok((r.get(0)?,r.get(1)?))).optional().map_err(internal)?;
            row.map(|(peer,payload)|{let task:TaskSubmit=decode(&payload)?;Ok(MeshTarget{peer_id:Uuid::parse_str(&peer).map_err(internal)?,agent_id:task.target_agent_id,project_id:task.project_id})}).transpose()
        })
    }
    pub fn recover_interrupted(&self) -> Result<usize> {
        self.transaction(|tx| {
            let mut q = tx.prepare("SELECT record_json FROM tasks WHERE status IN ('RUNNING','CANCELLING','WAITING_INPUT')").map_err(internal)?;
            let tasks: Vec<String> = q.query_map([], |r| r.get(0)).map_err(internal)?.collect::<std::result::Result<_,_>>().map_err(internal)?;
            for text in &tasks {
                let task = decode(text)?;
                transition_in(tx, task, TaskStatus::Unknown, Some(MeshError::new(ErrorCode::TaskUnknown, "daemon restarted; external side effects are unproven; automatic retry disabled")))?;
            }
            Ok(tasks.len())
        })
    }
    pub fn append_event(&self, id: Uuid, kind: &str, payload: Value) -> Result<TaskEvent> {
        if encode(&payload)?.len() > MAX_EVENT_PAYLOAD_BYTES {
            return Err(MeshError::input("event exceeds 65536 bytes"));
        }
        self.transaction(|tx| {
            let stored: i64 = tx.query_row("SELECT COALESCE(SUM(length(CAST(event_json AS BLOB))),0) FROM (SELECT event_json FROM task_events WHERE task_id=?1 UNION ALL SELECT event_json FROM archived_task_events WHERE task_id=?1)", [id.to_string()], |r| r.get(0)).map_err(internal)?;
            if stored + encode(&payload)?.len() as i64 + 1024 > 16 * 1024 * 1024 { return Err(MeshError::new(ErrorCode::RateLimited, "task event storage budget exceeded; execution stopped without discarding history")); }
            append_in(tx, &task_in(tx, id)?, kind, payload)
        })
    }
    pub fn events(&self, id: Uuid, after: i64) -> Result<Vec<TaskEvent>> {
        if after < 0 {
            return Err(MeshError::input("event cursor must be nonnegative"));
        }
        self.read(|c| { let mut q = c.prepare("SELECT event_json FROM (SELECT seq,event_json FROM task_events WHERE task_id=?1 AND seq>?2 UNION ALL SELECT seq,event_json FROM archived_task_events WHERE task_id=?1 AND seq>?2) ORDER BY seq LIMIT 1000").map_err(internal)?;
            let rows=q.query_map(params![id.to_string(), after], |r| r.get::<_,String>(0)).map_err(internal)?;
            // Stay below the 1 MiB transport response cap even for escaped text.
            let mut result=vec![];
            let mut bytes=2usize;
            for row in rows {
                let text=row.map_err(internal)?;
                if bytes+text.len()+1>512*1024 {break;}
                bytes+=text.len()+1;
                result.push(decode(&text)?);
            }
            Ok(result) })
    }
    pub fn audit(
        &self,
        actor: Option<Uuid>,
        action: &str,
        target: &str,
        decision: &str,
        trace: Uuid,
    ) -> Result<()> {
        self.read(|c| audit_in(c, actor, action, target, decision, trace))
    }
    pub fn audits(&self) -> Result<Vec<Value>> {
        self.read(|c| { let mut q = c.prepare("SELECT actor_node_id,action,target,decision,trace_id,created_at FROM audit_records ORDER BY created_at,id LIMIT 1000").map_err(internal)?;
            q.query_map([], |r| Ok(json!({"actor_node_id":r.get::<_,Option<String>>(0)?,"action":r.get::<_,String>(1)?,"target":r.get::<_,String>(2)?,"decision":r.get::<_,String>(3)?,"trace_id":r.get::<_,String>(4)?,"created_at":r.get::<_,i64>(5)?}))).map_err(internal)?.map(|r| r.map_err(internal)).collect() })
    }
    pub fn expire_pending_approvals_at(&self, ttl: u64, at: i64) -> Result<usize> {
        if !(1..=604800).contains(&ttl) {
            return Err(MeshError::input("invalid approval TTL"));
        }
        self.transaction(|tx| {
            let mut q = tx.prepare("SELECT record_json FROM tasks WHERE status='PENDING_APPROVAL' AND created_at<=?1").map_err(internal)?;
            let rows: Vec<String> = q.query_map([at.saturating_sub(ttl as i64)], |r| r.get(0)).map_err(internal)?.collect::<std::result::Result<_,_>>().map_err(internal)?;
            for row in &rows { let task: TaskRecord = decode(row)?; let id=task.task_id; transition_in(tx,task,TaskStatus::Expired,Some(MeshError::new(ErrorCode::PermissionDenied,"pending approval expired; resubmission requires a new explicit request")))?; audit_in(tx,None,"task.approval.expire",&id.to_string(),"expired",Uuid::new_v4())?; }
            Ok(rows.len())
        })
    }
    pub fn approve_pending(&self, id: Uuid, ttl: u64) -> Result<TaskRecord> {
        if !(1..=604800).contains(&ttl) {
            return Err(MeshError::input("invalid approval TTL"));
        }
        self.transaction(|tx| {
            let task = task_in(tx, id)?;
            if task.status != TaskStatus::PendingApproval {
                return Err(MeshError::new(
                    ErrorCode::TaskConflict,
                    "only pending tasks may be approved",
                ));
            }
            if now().saturating_sub(task.created_at) >= ttl as i64 {
                return transition_in(
                    tx,
                    task,
                    TaskStatus::Expired,
                    Some(MeshError::new(
                        ErrorCode::PermissionDenied,
                        "pending approval expired",
                    )),
                );
            }
            transition_in(tx, task, TaskStatus::Queued, None)
        })
    }
    pub fn archive_events(&self, id: Uuid) -> Result<usize> {
        self.transaction(|tx| {
            if !task_in(tx,id)?.status.is_terminal() { return Err(MeshError::new(ErrorCode::TaskConflict,"only terminal task events may be archived")); }
            let count=tx.execute("INSERT INTO archived_task_events SELECT task_id,seq,event_json FROM task_events WHERE task_id=?1",[id.to_string()]).map_err(internal)?;
            tx.execute("DELETE FROM task_events WHERE task_id=?1",[id.to_string()]).map_err(internal)?;
            audit_in(tx,None,"task.events.archive",&id.to_string(),"cold-storage",Uuid::new_v4())?;
            Ok(count)
        })
    }
    pub fn agent_retired(&self, id: Uuid) -> Result<bool> {
        self.read(|c| {
            c.query_row(
                "SELECT EXISTS(SELECT 1 FROM retired_agents WHERE agent_id=?1)",
                [id.to_string()],
                |r| r.get(0),
            )
            .map_err(internal)
        })
    }
    pub fn retired_agents(&self) -> Result<Vec<AgentCard>> {
        self.read(|c| { let mut q=c.prepare("SELECT card_json FROM agents JOIN retired_agents USING(agent_id) ORDER BY name").map_err(internal)?; q.query_map([],|r|r.get::<_,String>(0)).map_err(internal)?.map(|r|decode(&r.map_err(internal)?)).collect() })
    }
    pub fn retire_agent(&self, id: Uuid, restore: bool) -> Result<AgentCard> {
        self.transaction(|tx| {
            let busy: bool=tx.query_row("SELECT EXISTS(SELECT 1 FROM tasks WHERE json_extract(record_json,'$.agent_id')=?1 AND status IN ('PENDING_APPROVAL','QUEUED','RUNNING','WAITING_INPUT','CANCELLING'))",[id.to_string()],|r|r.get(0)).map_err(internal)?;
            if busy { return Err(MeshError::new(ErrorCode::TaskConflict,"resolve outstanding tasks before retiring or restoring Agent")); }
            let text: String=tx.query_row("SELECT card_json FROM agents WHERE agent_id=?1",[id.to_string()],|r|r.get(0)).map_err(internal)?;
            let mut card: AgentCard=decode(&text)?;
            if restore { tx.execute("DELETE FROM retired_agents WHERE agent_id=?1",[id.to_string()]).map_err(internal)?; }
            else { tx.execute("INSERT OR IGNORE INTO retired_agents VALUES(?1,?2)",params![id.to_string(),now()]).map_err(internal)?; }
            card.availability=if restore {"unverified"} else {"retired"}.into(); card.probe.managed_run="not_checked".into(); card.probe.authentication="not_checked".into(); card.probe.unavailable_reason=Some("owner lifecycle change; explicit model verification required".into());
            tx.execute("UPDATE agents SET card_json=?1 WHERE agent_id=?2",params![encode(&card)?,id.to_string()]).map_err(internal)?;
            audit_in(tx,None,if restore {"agent.restore"} else {"agent.retire"},&id.to_string(),"owner-approved",Uuid::new_v4())?;
            Ok(card)
        })
    }
    pub fn record_verification(&self, id: Uuid, evidence: &Value) -> Result<Value> {
        self.transaction(|tx| {
            let task = task_in(tx, id)?;
            let status = if evidence.get("passed") == Some(&Value::Bool(true)) {
                "passed"
            } else {
                "failed"
            };
            tx.execute(
                "INSERT INTO verifications VALUES(?1,?2,?3,?4,?5)",
                params![
                    Uuid::new_v4().to_string(),
                    id.to_string(),
                    status,
                    encode(evidence)?,
                    now()
                ],
            )
            .map_err(internal)?;
            append_in(tx, &task, "verification", evidence.clone())?;
            Ok(evidence.clone())
        })
    }
    pub fn verifications(&self, id: Uuid) -> Result<Vec<Value>> {
        self.read(|c| {
            let mut q = c.prepare("SELECT evidence_json FROM verifications WHERE task_id=?1 ORDER BY created_at,verification_id").map_err(internal)?;
            q.query_map([id.to_string()],|r|r.get::<_,String>(0)).map_err(internal)?.map(|r|decode(&r.map_err(internal)?)).collect()
        })
    }
}
fn task_in(conn: &Connection, id: Uuid) -> Result<TaskRecord> {
    let text: Option<String> = conn
        .query_row(
            "SELECT record_json FROM tasks WHERE task_id=?1",
            [id.to_string()],
            |r| r.get(0),
        )
        .optional()
        .map_err(internal)?;
    decode(&text.ok_or_else(|| MeshError::new(ErrorCode::TaskUnknown, "task not found"))?)
}
fn transition_in(
    tx: &Transaction<'_>,
    mut task: TaskRecord,
    next: TaskStatus,
    error: Option<MeshError>,
) -> Result<TaskRecord> {
    if !task.status.can_transition_to(next) {
        return Err(MeshError::new(
            ErrorCode::TaskConflict,
            format!("invalid state transition {:?} -> {next:?}", task.status),
        ));
    }
    task.status = next;
    task.updated_at = now();
    task.error = error;
    tx.execute(
        "UPDATE tasks SET status=?1,record_json=?2 WHERE task_id=?3",
        params![status(next)?, encode(&task)?, task.task_id.to_string()],
    )
    .map_err(internal)?;
    if next.is_terminal()
        && let Some(attempt) = task.last_attempt_id
    {
        tx.execute(
            "UPDATE task_attempts SET status=?1,ended_at=?2,recovery_reason=?3 WHERE attempt_id=?4",
            params![
                status(next)?,
                now(),
                task.error.as_ref().map(|e| e.message.as_str()),
                attempt.to_string()
            ],
        )
        .map_err(internal)?;
    }
    occupancy_in(tx, task.agent_id)?;
    append_in(
        tx,
        &task,
        "status",
        json!({"status":next,"error":task.error}),
    )?;
    Ok(task)
}
fn occupancy_in(tx: &Transaction<'_>, id: Uuid) -> Result<()> {
    let text: Option<String> = tx
        .query_row(
            "SELECT card_json FROM agents WHERE agent_id=?1",
            [id.to_string()],
            |r| r.get(0),
        )
        .optional()
        .map_err(internal)?;
    let Some(text) = text else {
        return Ok(());
    };
    let mut card: AgentCard = decode(&text)?;
    let busy:bool=tx.query_row("SELECT EXISTS(SELECT 1 FROM tasks WHERE status IN ('RUNNING','CANCELLING','WAITING_INPUT') AND json_extract(record_json,'$.agent_id')=?1)",[id.to_string()],|r|r.get(0)).map_err(internal)?;
    card.availability = if busy {
        "busy"
    } else if card.probe.managed_run == "passed" {
        "idle"
    } else {
        "unverified"
    }
    .into();
    tx.execute(
        "UPDATE agents SET card_json=?1 WHERE agent_id=?2",
        params![encode(&card)?, id.to_string()],
    )
    .map_err(internal)?;
    Ok(())
}
fn append_in(
    tx: &Transaction<'_>,
    task: &TaskRecord,
    kind: &str,
    payload: Value,
) -> Result<TaskEvent> {
    let seq: i64 = tx
        .query_row(
            "SELECT COALESCE(MAX(seq),0)+1 FROM (SELECT seq FROM task_events WHERE task_id=?1 UNION ALL SELECT seq FROM archived_task_events WHERE task_id=?1)",
            [task.task_id.to_string()],
            |r| r.get(0),
        )
        .map_err(internal)?;
    let event = TaskEvent {
        task_id: task.task_id,
        seq,
        event_id: Uuid::new_v4(),
        attempt_id: task.last_attempt_id,
        event_type: kind.into(),
        payload,
        created_at: now(),
    };
    tx.execute(
        "INSERT INTO task_events VALUES(?1,?2,?3)",
        params![task.task_id.to_string(), seq, encode(&event)?],
    )
    .map_err(internal)?;
    Ok(event)
}
fn audit_in(
    conn: &Connection,
    actor: Option<Uuid>,
    action: &str,
    target: &str,
    decision: &str,
    trace: Uuid,
) -> Result<()> {
    conn.execute(
        "INSERT INTO audit_records VALUES(?1,?2,?3,?4,?5,?6,?7)",
        params![
            Uuid::new_v4().to_string(),
            actor.map(|id| id.to_string()),
            action,
            target,
            decision,
            trace.to_string(),
            now()
        ],
    )
    .map_err(internal)?;
    Ok(())
}
