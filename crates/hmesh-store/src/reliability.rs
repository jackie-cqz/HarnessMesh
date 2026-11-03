use super::*;

pub struct SnapshotEntry {
    pub name: String,
    pub bytes: Option<Vec<u8>>,
    pub error: Option<String>,
}
pub struct ArtifactSet {
    pub attempt_id: Uuid,
    pub configuration_sha256: String,
    pub entries: Vec<SnapshotEntry>,
}

pub(super) fn scoped_admission(
    c: &Connection,
    request: &TaskSubmit,
    limits: &RuntimeOptions,
) -> Result<()> {
    let project = limits
        .project_limits
        .get(&request.project_id)
        .cloned()
        .unwrap_or_default();
    let agent = limits
        .agent_limits
        .get(&request.target_agent_id)
        .cloned()
        .unwrap_or_default();
    for (field, value, maximum) in [
        (
            "project_id",
            request.project_id.clone(),
            project.max_queue_depth,
        ),
        (
            "agent_id",
            request.target_agent_id.to_string(),
            agent.max_queue_depth,
        ),
    ] {
        let count: usize = c.query_row(&format!("SELECT count(*) FROM tasks WHERE status IN ('PENDING_APPROVAL','QUEUED','RUNNING','CANCELLING','WAITING_INPUT') AND json_extract(record_json,'$.{field}')=?1"), [value], |r|r.get(0)).map_err(internal)?;
        if count >= maximum {
            return Err(MeshError::new(
                ErrorCode::RateLimited,
                format!("{field} queue capacity reached"),
            ));
        }
    }
    Ok(())
}

fn all_in(c: &Connection, condition: &str) -> Result<Vec<TaskRecord>> {
    let mut q = c
        .prepare(&format!(
            "SELECT record_json FROM tasks WHERE {condition} ORDER BY created_at,task_id"
        ))
        .map_err(internal)?;
    q.query_map([], |r| r.get::<_, String>(0))
        .map_err(internal)?
        .map(|r| decode(&r.map_err(internal)?))
        .collect()
}
fn wait_reason(
    c: &Connection,
    task: &TaskRecord,
    active: &[TaskRecord],
    limits: &RuntimeOptions,
) -> Result<Option<&'static str>> {
    if active.len() >= limits.max_concurrent_runs {
        return Ok(Some("global_limit"));
    }
    let project = limits
        .project_limits
        .get(&task.project_id)
        .cloned()
        .unwrap_or_default();
    let agent = limits
        .agent_limits
        .get(&task.agent_id)
        .cloned()
        .unwrap_or_default();
    if active
        .iter()
        .filter(|t| t.agent_id == task.agent_id)
        .count()
        >= agent.max_concurrent_runs
    {
        return Ok(Some("agent_limit"));
    }
    if active
        .iter()
        .filter(|t| t.project_id == task.project_id)
        .count()
        >= project.max_concurrent_runs
    {
        return Ok(Some("project_limit"));
    }
    let config = |id: Uuid| -> Result<Option<AgentConfig>> {
        c.query_row(
            "SELECT config_json FROM agents WHERE agent_id=?1",
            [id.to_string()],
            |r| r.get::<_, String>(0),
        )
        .optional()
        .map_err(internal)?
        .map(|s| decode(&s))
        .transpose()
    };
    if let Some(current) = config(task.agent_id)? {
        for running in active {
            if let Some(other) = config(running.agent_id)?
                && other.project.root == current.project.root
                && (other.project.workspace_mode == "shared"
                    || current.project.workspace_mode == "shared")
            {
                return Ok(Some("shared_workspace"));
            }
        }
    }
    Ok(None)
}
impl Store {
    pub fn claim_next_with_limits(&self, limits: &RuntimeOptions) -> Result<Option<TaskRecord>> {
        self.transaction(|tx| {
            let active=all_in(tx,"status IN ('RUNNING','CANCELLING','WAITING_INPUT')")?;
            let mut queued=all_in(tx,"status='QUEUED'")?;
            let mut ranked=Vec::new();
            for task in queued.drain(..) {
                let ordinal:i64=tx.query_row("SELECT ordinal FROM project_dispatch WHERE project_id=?1",[&task.project_id],|r|r.get(0)).optional().map_err(internal)?.unwrap_or(0);
                ranked.push((ordinal,task));
            }
            ranked.sort_by_key(|(ordinal,t)|(*ordinal,t.created_at,t.task_id));
            for (_,mut task) in ranked {
                if wait_reason(tx,&task,&active,limits)?.is_some() {continue;}
                task.last_attempt_id=Some(Uuid::new_v4());
                let task=transition_in(tx,task,TaskStatus::Running,None)?;
                tx.execute("INSERT INTO task_attempts(attempt_id,task_id,status,started_at) VALUES(?1,?2,'RUNNING',?3)",params![task.last_attempt_id.map(|id|id.to_string()),task.task_id.to_string(),now()]).map_err(internal)?;
                tx.execute("INSERT INTO project_dispatch(project_id,ordinal) VALUES(?1,(SELECT COALESCE(max(ordinal),0)+1 FROM project_dispatch)) ON CONFLICT(project_id) DO UPDATE SET ordinal=excluded.ordinal",[&task.project_id]).map_err(internal)?;
                return Ok(Some(task));
            }
            Ok(None)
        })
    }
    pub fn queue_status(&self, limits: &RuntimeOptions) -> Result<Value> {
        self.read(|c| {
            let active=all_in(c,"status IN ('RUNNING','CANCELLING','WAITING_INPUT')")?;
            let queued=all_in(c,"status='QUEUED'")?;
            let mut rows=Vec::new();
            for task in queued {rows.push(json!({"task_id":task.task_id,"project_id":task.project_id,"agent_id":task.agent_id,"reason":wait_reason(c,&task,&active,limits)?.unwrap_or("ready")}));}
            Ok(json!({"running":active.len(),"limits":limits,"waiting":rows}))
        })
    }
    /// Commit a successful completion and its entire artifact set together. Never overwrite a sealed set.
    pub fn complete_with_artifacts(
        &self,
        id: Uuid,
        attempt: Uuid,
        digest: &str,
        entries: &[SnapshotEntry],
    ) -> Result<TaskRecord> {
        if entries.len() > 16
            || entries
                .iter()
                .map(|e| e.bytes.as_ref().map_or(0, Vec::len))
                .sum::<usize>()
                > 32 * 1024 * 1024
            || entries.iter().any(|e| {
                e.bytes.as_ref().is_some_and(|b| b.len() > 10 * 1024 * 1024)
                    || e.name.is_empty()
                    || std::path::Path::new(&e.name)
                        .components()
                        .any(|c| !matches!(c, std::path::Component::Normal(_)))
            })
        {
            return Err(MeshError::input("artifact snapshot limits exceeded"));
        }
        self.transaction(|tx| {
            let task = task_in(tx, id)?;
            if task.status != TaskStatus::Running || task.last_attempt_id != Some(attempt) {
                return Err(MeshError::new(
                    ErrorCode::TaskConflict,
                    "completion attempt is no longer active",
                ));
            }
            tx.execute(
                "INSERT INTO artifact_sets VALUES(?1,?2,?3,?4)",
                params![id.to_string(), attempt.to_string(), digest, now()],
            )
            .map_err(internal)?;
            for entry in entries {
                tx.execute(
                    "INSERT INTO artifact_snapshots VALUES(?1,?2,?3,?4)",
                    params![id.to_string(), entry.name, entry.bytes, entry.error],
                )
                .map_err(internal)?;
            }
            Self::mark_agent_verified_in(tx, task.agent_id)?;
            transition_in(tx, task, TaskStatus::Succeeded, None)
        })
    }
    pub fn artifact_set(&self, id: Uuid) -> Result<Option<ArtifactSet>> {
        self.read(|c| {
            let set:Option<(String,String)>=c.query_row("SELECT attempt_id,config_digest FROM artifact_sets WHERE task_id=?1",[id.to_string()],|r|Ok((r.get(0)?,r.get(1)?))).optional().map_err(internal)?;
            let Some((attempt,digest))=set else{return Ok(None)};
            let mut q=c.prepare("SELECT name,bytes,error FROM artifact_snapshots WHERE task_id=?1 ORDER BY name").map_err(internal)?;
            let entries=q.query_map([id.to_string()],|r|Ok(SnapshotEntry{name:r.get(0)?,bytes:r.get(1)?,error:r.get(2)?})).map_err(internal)?.collect::<std::result::Result<Vec<_>,_>>().map_err(internal)?;
            Ok(Some(ArtifactSet{attempt_id:Uuid::parse_str(&attempt).map_err(internal)?,configuration_sha256:digest,entries}))
        })
    }
    pub fn workspace_put(&self, id: Uuid, record: &Value) -> Result<()> {
        self.read(|c|{c.execute("INSERT INTO workspaces VALUES(?1,?2) ON CONFLICT(task_id) DO UPDATE SET record_json=excluded.record_json",params![id.to_string(),encode(record)?]).map_err(internal)?;Ok(())})
    }
    pub fn workspace(&self, id: Uuid) -> Result<Option<Value>> {
        self.read(|c| {
            c.query_row(
                "SELECT record_json FROM workspaces WHERE task_id=?1",
                [id.to_string()],
                |r| r.get::<_, String>(0),
            )
            .optional()
            .map_err(internal)?
            .map(|v| decode(&v))
            .transpose()
        })
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn sqlite_full_during_completion_rolls_back_the_seal_and_success() {
        let temp = tempfile::tempdir().unwrap();
        let store = Store::open(&temp.path().join("db")).unwrap();
        let envelope = Envelope::new(
            Uuid::new_v4(),
            "task.submit",
            TaskSubmit::new(Uuid::new_v4(), "fixture", "no model"),
        );
        let task = store.admit(Uuid::new_v4(), &envelope, false, 20).unwrap();
        let active = store.claim_next().unwrap().unwrap();
        store
            .read(|c| {
                let pages: i64 = c
                    .pragma_query_value(None, "page_count", |r| r.get(0))
                    .map_err(internal)?;
                c.pragma_update(None, "max_page_count", pages)
                    .map_err(internal)
            })
            .unwrap();
        let entry = SnapshotEntry {
            name: "out.bin".into(),
            bytes: Some(vec![1; 1024 * 1024]),
            error: None,
        };
        let error = store
            .complete_with_artifacts(
                task.task_id,
                active.last_attempt_id.unwrap(),
                "config",
                &[entry],
            )
            .unwrap_err();
        assert!(error.message.to_ascii_lowercase().contains("full"));
        assert_eq!(
            store.task(task.task_id).unwrap().status,
            TaskStatus::Running
        );
        assert!(store.artifact_set(task.task_id).unwrap().is_none());
    }
}
