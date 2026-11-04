use super::*;
use hmesh_core::development::*;

impl Store {
    pub fn archived_workflows(&self) -> Result<Value> {
        self.read(|c| {
            let mut q=c.prepare("SELECT json_object('id',id,'name',json_extract(record_json,'$.spec.name'),'state',json_extract(record_json,'$.state'),'updated_at',json_extract(record_json,'$.updated_at')) FROM managed_records WHERE kind='workflow_archive' ORDER BY json_extract(record_json,'$.updated_at') DESC,id LIMIT 1001").map_err(internal)?;
            let mut rows=q.query_map([],|r|r.get::<_,String>(0)).map_err(internal)?.map(|r|decode::<Value>(&r.map_err(internal)?)).collect::<Result<Vec<_>>>()?;
            let truncated=rows.len()>1000;rows.truncate(1000);Ok(json!({"rows":rows,"truncated":truncated,"row_limit":1000}))
        })
    }
    pub fn archive_workflow(&self, id: Uuid) -> Result<()> {
        self.transaction(|tx| {
            let record:Option<String>=tx.query_row("SELECT record_json FROM managed_records WHERE kind='workflow' AND id=?1",[id.to_string()],|r|r.get(0)).optional().map_err(internal)?;
            let Some(record)=record else {
                let archived:bool=tx.query_row("SELECT EXISTS(SELECT 1 FROM managed_records WHERE kind='workflow_archive' AND id=?1)",[id.to_string()],|r|r.get(0)).map_err(internal)?;
                return if archived{Ok(())}else{Err(MeshError::input("workflow not found"))};
            };
            let run:WorkflowRun=decode(&record)?;
            if !matches!(run.state.as_str(),"completed"|"cancelled") {return Err(MeshError::input("only completed/cancelled workflows may be archived"));}
            tx.execute("UPDATE managed_records SET kind='workflow_archive' WHERE kind='workflow' AND id=?1",[id.to_string()]).map_err(internal)?;
            Ok(())
        })
    }
    /// Exact replay is idempotent; revisions are immutable and cannot be overwritten.
    pub fn save_template(&self, template: &TaskTemplate) -> Result<()> {
        template.validate()?;
        let key = format!("template:{}", template.id);
        self.transaction(|tx| {
            let old:Option<String>=tx.query_row("SELECT record_json FROM managed_records WHERE kind=?1 AND id=?2",params![key,template.revision.to_string()],|r|r.get(0)).optional().map_err(internal)?;
            if let Some(old)=old {
                if old==encode(template)? {return Ok(());}
                return Err(MeshError::new(ErrorCode::TaskConflict,"template revision is immutable"));
            }
            let count:u32=tx.query_row("SELECT count(*) FROM managed_records WHERE kind LIKE 'template:%'",[],|r|r.get(0)).map_err(internal)?;
            if count>=256 {return Err(MeshError::new(ErrorCode::RateLimited,"template revision capacity reached"));}
            let bytes:usize=tx.query_row("SELECT coalesce(sum(length(CAST(record_json AS BLOB))),0) FROM managed_records WHERE kind LIKE 'template:%'",[],|r|r.get(0)).map_err(internal)?;
            if bytes+encode(template)?.len()>512*1024 {return Err(MeshError::new(ErrorCode::RateLimited,"template library exceeds 512 KiB"));}
            let previous:u32=tx.query_row("SELECT coalesce(max(CAST(id AS INTEGER)),0) FROM managed_records WHERE kind=?1",[&key],|r|r.get(0)).map_err(internal)?;
            if template.revision!=previous+1 {return Err(MeshError::new(ErrorCode::TaskConflict,"template revision must follow the last revision"));}
            tx.execute("INSERT INTO managed_records VALUES(?1,?2,?3)",params![key,template.revision.to_string(),encode(template)?]).map_err(internal)?;
            Ok(())
        })
    }
    pub fn templates(&self) -> Result<Vec<TaskTemplate>> {
        self.read(|c| {
            let mut q=c.prepare("SELECT record_json FROM managed_records WHERE kind LIKE 'template:%' ORDER BY kind,CAST(id AS INTEGER)").map_err(internal)?;
            q.query_map([],|r|r.get::<_,String>(0)).map_err(internal)?.map(|r|decode(&r.map_err(internal)?)).collect()
        })
    }
    /// Compare-and-swap prevents a stale worker from resurrecting a cancelled plan.
    pub fn save_workflow(&self, run: &WorkflowRun, expected: u64) -> Result<bool> {
        self.read(|c| Ok(c.execute("UPDATE managed_records SET record_json=?2 WHERE kind='workflow' AND id=?1 AND json_extract(record_json,'$.revision')=?3",params![run.spec.id.to_string(),encode(run)?,expected]).map_err(internal)?==1))
    }
    pub fn set_project_budget(&self, budget: &ProjectBudget) -> Result<()> {
        budget.validate()?;
        self.read(|c|{c.execute("INSERT INTO project_budgets VALUES(?1,?2) ON CONFLICT(project_id) DO UPDATE SET record_json=excluded.record_json",params![budget.project_id,encode(budget)?]).map_err(internal)?;Ok(())})
    }
    pub fn project_budget(&self, project: &str) -> Result<Option<ProjectBudget>> {
        self.read(|c| {
            c.query_row(
                "SELECT record_json FROM project_budgets WHERE project_id=?1",
                [project],
                |r| r.get::<_, String>(0),
            )
            .optional()
            .map_err(internal)?
            .map(|s| decode(&s))
            .transpose()
        })
    }
    pub fn budgets(&self) -> Result<Vec<ProjectBudget>> {
        self.read(|c| {
            let mut q = c
                .prepare("SELECT record_json FROM project_budgets ORDER BY project_id")
                .map_err(internal)?;
            q.query_map([], |r| r.get::<_, String>(0))
                .map_err(internal)?
                .map(|r| decode(&r.map_err(internal)?))
                .collect()
        })
    }
    pub fn record_usage(&self, task: Uuid, attempt: Uuid, usage: &UsageTotals) -> Result<()> {
        self.transaction(|tx| {
            let current=task_in(tx,task)?;
            if current.last_attempt_id!=Some(attempt) {return Err(MeshError::new(ErrorCode::TaskConflict,"usage attempt mismatch"));}
            tx.execute("INSERT INTO task_usage VALUES(?1,?2,?3) ON CONFLICT(task_id) DO UPDATE SET record_json=excluded.record_json WHERE attempt_id=excluded.attempt_id",params![task.to_string(),attempt.to_string(),encode(usage)?]).map_err(internal)?;
            Ok(())
        })
    }
    pub fn usage(&self, task: Uuid) -> Result<UsageTotals> {
        self.read(|c| {
            Ok(c.query_row(
                "SELECT record_json FROM task_usage WHERE task_id=?1",
                [task.to_string()],
                |r| r.get::<_, String>(0),
            )
            .optional()
            .map_err(internal)?
            .map(|s| decode(&s))
            .transpose()?
            .unwrap_or_default())
        })
    }
    pub fn usage_report(&self) -> Result<Value> {
        self.read(|c| {
            let mut q=c.prepare("SELECT t.record_json,a.started_at,a.ended_at,u.record_json FROM tasks t LEFT JOIN task_attempts a ON a.attempt_id=json_extract(t.record_json,'$.last_attempt_id') LEFT JOIN task_usage u ON u.task_id=t.task_id ORDER BY t.created_at DESC,t.task_id LIMIT 500").map_err(internal)?;
            let rows=q.query_map([],|r|Ok((r.get::<_,String>(0)?,r.get::<_,Option<i64>>(1)?,r.get::<_,Option<i64>>(2)?,r.get::<_,Option<String>>(3)?))).map_err(internal)?;
            let mut out=Vec::new();
            for row in rows {
                let (task,start,end,usage)=row.map_err(internal)?;
                let task:TaskRecord=decode(&task)?;
                let usage:UsageTotals=usage.map(|s|decode(&s)).transpose()?.unwrap_or_default();
                let budget:Option<ProjectBudget>=c.query_row("SELECT record_json FROM project_budgets WHERE project_id=?1",[&task.project_id],|r|r.get::<_,String>(0)).optional().map_err(internal)?.map(|s|decode(&s)).transpose()?;
                // Cache token prices differ by provider. Never misprice cache traffic using plain input rates.
                let estimate=budget.as_ref().and_then(|b|Some((usage.input_tokens? as f64*b.input_usd_per_million?+usage.output_tokens? as f64*b.output_usd_per_million?)/1_000_000.0)).filter(|_|usage.cache_read_tokens==Some(0)&&usage.cache_write_tokens==Some(0)&&budget.as_ref().is_some_and(|b|b.pricing_model==usage.configured_model&&b.pricing_provider==usage.configured_provider&&b.pricing_model.is_some()&&b.pricing_provider.is_some()));
                out.push(json!({"task_id":task.task_id,"project_id":task.project_id,"agent_id":task.agent_id,"status":task.status,"attempt_id":task.last_attempt_id,"queue_seconds":start.map(|n|(n-task.created_at).max(0)),"runtime_seconds":start.map(|n|(end.unwrap_or_else(now)-n).max(0)),"runtime_complete":end.is_some(),"usage":usage,"estimated_usd":estimate,"price_source":budget.as_ref().and_then(|b|b.price_source.as_ref()),"price_as_of":budget.as_ref().and_then(|b|b.price_as_of.as_ref()),"pricing_basis":"current-owner-rates; estimate only"}));
            }
            let total:i64=c.query_row("SELECT count(*) FROM tasks",[],|r|r.get(0)).map_err(internal)?;
            let success:i64=c.query_row("SELECT count(*) FROM tasks WHERE status='SUCCEEDED'",[],|r|r.get(0)).map_err(internal)?;
            let finished:i64=c.query_row("SELECT count(*) FROM tasks WHERE status IN ('SUCCEEDED','FAILED','CANCELLED','REJECTED','EXPIRED','UNKNOWN')",[],|r|r.get(0)).map_err(internal)?;
            let ended:i64=c.query_row("SELECT count(*) FROM task_attempts WHERE ended_at IS NOT NULL",[],|r|r.get(0)).map_err(internal)?;
            Ok(json!({"total_tasks":total,"terminal_tasks":finished,"completed_attempts":ended,"succeeded_tasks":success,"execution_success_rate":if ended>0 {Some(success as f64/ended as f64)}else{None},"rows":out,"row_limit":500,"truncated":total>500,"billing_authority":false}))
        })
    }
    pub fn storage_summary(&self) -> Result<Value> {
        self.read(|c| {
            let scalar=|sql:&str|->Result<i64>{c.query_row(sql,[],|r|r.get(0)).map_err(internal)};
            Ok(json!({"snapshot_bytes":scalar("SELECT coalesce(sum(length(bytes)),0) FROM artifact_snapshots")?,"snapshot_files":scalar("SELECT count(*) FROM artifact_snapshots WHERE bytes IS NOT NULL")?,"purged_snapshots":scalar("SELECT count(*) FROM artifact_snapshots WHERE error='purged_by_owner'")?,"workspace_patch_bytes":scalar("SELECT coalesce(sum(length(CAST(json_extract(record_json,'$.patch') AS BLOB))),0) FROM workspaces")?,"event_bytes":scalar("SELECT coalesce(sum(length(CAST(event_json AS BLOB))),0) FROM task_events")?,"archived_event_bytes":scalar("SELECT coalesce(sum(length(CAST(event_json AS BLOB))),0) FROM archived_task_events")?,"database_page_bytes":scalar("SELECT page_count*page_size FROM pragma_page_count(),pragma_page_size()")?,"database_free_bytes":scalar("SELECT freelist_count*page_size FROM pragma_freelist_count(),pragma_page_size()")?,"automatic_deletion":false}))
        })
    }
    pub fn retention_candidates(&self, cutoff: i64, keep: usize) -> Result<Vec<TaskRecord>> {
        if keep > 10000 {
            return Err(MeshError::input("keep_latest too large"));
        }
        let flows: Vec<WorkflowRun> = self.managed_list("workflow")?;
        let protected: std::collections::HashSet<Uuid> = flows
            .iter()
            .filter(|f| !matches!(f.state.as_str(), "completed" | "cancelled"))
            .flat_map(|f| f.plans.iter().flatten().map(|p| p.task.task_id))
            .collect();
        let mut tasks = self.tasks()?;
        tasks.sort_by_key(|t| std::cmp::Reverse((t.created_at, t.task_id)));
        Ok(tasks
            .into_iter()
            .skip(keep)
            .filter(|t| {
                t.status.is_terminal()
                    && t.status != TaskStatus::Unknown
                    && t.updated_at < cutoff
                    && !protected.contains(&t.task_id)
            })
            .collect())
    }
    pub fn purge_snapshots(&self, ids: &[Uuid]) -> Result<usize> {
        self.transaction(|tx| {
            let mut count=0;
            for id in ids {
                let task=task_in(tx,*id)?;
                if !task.status.is_terminal()||task.status==TaskStatus::Unknown {return Err(MeshError::input("active/unknown task cannot be purged"));}
                count+=tx.execute("UPDATE artifact_snapshots SET bytes=NULL,error='purged_by_owner' WHERE task_id=?1 AND bytes IS NOT NULL",[id.to_string()]).map_err(internal)?;
                append_in(tx,&task,"retention",json!({"snapshots":"purged_by_owner","history_preserved":true}))?;
            }
            Ok(count)
        })
    }
}
