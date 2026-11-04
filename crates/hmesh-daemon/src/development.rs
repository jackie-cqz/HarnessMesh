use super::*;
use hmesh_core::development::*;

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/local/templates", get(templates).post(save_template))
        .route("/local/templates/run", post(template_run))
        .route("/local/workflows", get(workflows).post(start))
        .route("/local/workflows/archived", get(archived))
        .route("/local/workflows/{id}", get(show))
        .route("/local/workflows/{id}/archive", post(archive))
        .route("/local/workflows/{id}/cancel", post(cancel))
        .route("/local/workflows/{id}/accept", post(accept))
        .route("/local/diagnostics", get(diagnostics))
        .route("/local/storage", get(storage))
        .route("/local/usage", get(usage))
        .route("/local/budgets", get(budgets).post(set_budget))
        .route("/local/compatibility", get(compatibility))
        .route("/local/compatibility/probe", post(probe_compatibility))
}
async fn templates(
    State(s): State<AppState>,
    Extension(c): Extension<ConnectionIdentity>,
    headers: HeaderMap,
) -> ApiResult<Vec<TaskTemplate>> {
    s.owner(&headers, &c)?;
    Ok(Json(s.store.templates()?))
}
async fn save_template(
    State(s): State<AppState>,
    Extension(c): Extension<ConnectionIdentity>,
    headers: HeaderMap,
    Json(body): Json<TaskTemplate>,
) -> ApiResult<Value> {
    s.owner(&headers, &c)?;
    s.store.save_template(&body)?;
    s.store.audit(
        Some(s.instance.card.node_id),
        "template.save",
        &body.id.to_string(),
        "allowed",
        Uuid::new_v4(),
    )?;
    Ok(Json(
        json!({"id":body.id,"revision":body.revision,"immutable":true}),
    ))
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TemplateRunRequest {
    pub id: Uuid,
    pub template_id: Uuid,
    pub revision: u32,
    pub goal: String,
}
async fn template_run(
    State(s): State<AppState>,
    Extension(c): Extension<ConnectionIdentity>,
    headers: HeaderMap,
    Json(body): Json<TemplateRunRequest>,
) -> ApiResult<WorkflowRun> {
    s.owner(&headers, &c)?;
    let template = s
        .store
        .templates()?
        .into_iter()
        .find(|t| t.id == body.template_id && t.revision == body.revision)
        .ok_or_else(|| ApiError(MeshError::input("template revision not found")))?;
    let spec = WorkflowSpec {
        id: body.id,
        name: template.name.clone(),
        max_total_timeout_seconds: template.limits.timeout_seconds,
        stages: vec![WorkflowStage {
            name: "task".into(),
            template,
            goal: body.goal,
            repair: None,
            max_repairs: 0,
        }],
    };
    Ok(Json(create(&s, spec)?))
}
fn create(s: &AppState, spec: WorkflowSpec) -> std::result::Result<WorkflowRun, ApiError> {
    spec.validate()?;
    let library = s.store.templates()?;
    for template in spec
        .stages
        .iter()
        .flat_map(|stage| std::iter::once(&stage.template).chain(stage.repair.iter()))
    {
        if !library.iter().any(|saved| {
            saved.id == template.id
                && saved.revision == template.revision
                && serde_json::to_value(saved).ok() == serde_json::to_value(template).ok()
        }) {
            return Err(MeshError::new(
                ErrorCode::TaskConflict,
                "embedded template must exactly match a saved immutable revision",
            )
            .into());
        }
    }
    if let Ok(old) = load_workflow(s, spec.id) {
        if serde_json::to_value(&old.spec).map_err(internal)?
            == serde_json::to_value(&spec).map_err(internal)?
        {
            return Ok(old);
        }
        return Err(MeshError::new(
            ErrorCode::TaskConflict,
            "workflow ID is already bound to a different plan",
        )
        .into());
    }
    let mut plans = Vec::new();
    for stage in &spec.stages {
        let mut row = vec![PlannedTask {
            request_id: Uuid::new_v4(),
            peer_id: stage.template.peer_id,
            task: stage.template.task(Uuid::new_v4(), &stage.goal)?,
        }];
        if let Some(repair) = &stage.repair {
            for _ in 0..stage.max_repairs {
                let goal = format!(
                    "{}\nRepair this stage to satisfy the unchanged artifact assertion. The owner must independently approve this repair.\nArtifact contract: {}",
                    stage.goal,
                    serde_json::to_string(&stage.template.verification).map_err(internal)?
                );
                row.push(PlannedTask {
                    request_id: Uuid::new_v4(),
                    peer_id: repair.peer_id,
                    task: repair.task(Uuid::new_v4(), &goal)?,
                });
            }
        }
        plans.push(row);
    }
    let run = WorkflowRun {
        spec,
        state: "running".into(),
        stage: 0,
        repair_index: 0,
        plans,
        reports: vec![],
        last_error: None,
        updated_at: now(),
        revision: 0,
    };
    if serde_json::to_vec(&run).map_err(internal)?.len() > 512 * 1024 {
        return Err(MeshError::input("expanded workflow exceeds 512 KiB").into());
    }
    s.store.managed_create("workflow", run.spec.id, &run)?;
    s.store.audit(
        Some(s.instance.card.node_id),
        "workflow.start",
        &run.spec.id.to_string(),
        "allowed",
        Uuid::new_v4(),
    )?;
    Ok(run)
}
async fn start(
    State(s): State<AppState>,
    Extension(c): Extension<ConnectionIdentity>,
    headers: HeaderMap,
    Json(spec): Json<WorkflowSpec>,
) -> ApiResult<WorkflowRun> {
    s.owner(&headers, &c)?;
    Ok(Json(create(&s, spec)?))
}
async fn workflows(
    State(s): State<AppState>,
    Extension(c): Extension<ConnectionIdentity>,
    headers: HeaderMap,
) -> ApiResult<Value> {
    s.owner(&headers, &c)?;
    let runs: Vec<WorkflowRun> = s.store.managed_list("workflow")?;
    Ok(Json(Value::Array(runs.iter().map(|r|json!({"spec":{"id":r.spec.id,"name":r.spec.name,"stages":r.spec.stages.iter().map(|stage|json!({"name":stage.name,"template":{"name":stage.template.name,"revision":stage.template.revision},"max_repairs":stage.max_repairs})).collect::<Vec<_>>()},"state":r.state,"stage":r.stage,"repair_index":r.repair_index,"last_error":r.last_error,"plans":r.plans.iter().map(|plans|plans.iter().map(|p|json!({"task":{"task_id":p.task.task_id},"request_id":p.request_id,"peer_id":p.peer_id})).collect::<Vec<_>>()).collect::<Vec<_>>(),"updated_at":r.updated_at})).collect())))
}
async fn show(
    State(s): State<AppState>,
    Extension(c): Extension<ConnectionIdentity>,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
) -> ApiResult<WorkflowRun> {
    s.owner(&headers, &c)?;
    Ok(Json(load_workflow(&s, id)?))
}
fn load_workflow(s: &AppState, id: Uuid) -> Result<WorkflowRun> {
    s.store
        .managed_get("workflow", id)
        .or_else(|_| s.store.managed_get("workflow_archive", id))
}
async fn archived(
    State(s): State<AppState>,
    Extension(c): Extension<ConnectionIdentity>,
    headers: HeaderMap,
) -> ApiResult<Value> {
    s.owner(&headers, &c)?;
    Ok(Json(s.store.archived_workflows()?))
}
async fn archive(
    State(s): State<AppState>,
    Extension(c): Extension<ConnectionIdentity>,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
) -> ApiResult<Value> {
    s.owner(&headers, &c)?;
    s.store.archive_workflow(id)?;
    s.store.audit(
        Some(s.instance.card.node_id),
        "workflow.archive",
        &id.to_string(),
        "allowed",
        Uuid::new_v4(),
    )?;
    Ok(Json(
        json!({"id":id,"archived":true,"history_preserved":true}),
    ))
}
fn update(s: &AppState, run: &mut WorkflowRun) -> std::result::Result<bool, ApiError> {
    let before = run.revision;
    run.revision += 1;
    run.updated_at = now();
    Ok(s.store.save_workflow(run, before)?)
}
async fn cancel(
    State(s): State<AppState>,
    Extension(c): Extension<ConnectionIdentity>,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
) -> ApiResult<WorkflowRun> {
    s.owner(&headers, &c)?;
    let mut run: WorkflowRun = s.store.managed_get("workflow", id)?;
    if !matches!(run.state.as_str(), "completed" | "cancelled") {
        run.state = "cancelling".into();
        if !update(&s, &mut run)? {
            return Err(MeshError::new(
                ErrorCode::TaskConflict,
                "workflow changed; refresh before cancelling",
            )
            .into());
        }
    }
    Ok(Json(run))
}
async fn accept(
    State(s): State<AppState>,
    Extension(c): Extension<ConnectionIdentity>,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
) -> ApiResult<WorkflowRun> {
    s.owner(&headers, &c)?;
    let mut run: WorkflowRun = s.store.managed_get("workflow", id)?;
    if run.state == "completed" {
        return Ok(Json(run));
    }
    if run.state != "review" {
        return Err(MeshError::input("workflow must reach owner review first").into());
    }
    run.state = "completed".into();
    if !update(&s, &mut run)? {
        return Err(
            MeshError::new(ErrorCode::TaskConflict, "workflow changed during review").into(),
        );
    }
    s.store.audit(
        Some(s.instance.card.node_id),
        "workflow.accept",
        &id.to_string(),
        "allowed",
        Uuid::new_v4(),
    )?;
    Ok(Json(run))
}
async fn current_task(
    s: &AppState,
    plan: &PlannedTask,
) -> std::result::Result<TaskRecord, ApiError> {
    if s.store.outbound_request(plan.request_id).is_err() {
        return dispatch_task(
            s,
            RemoteTask {
                peer_id: plan.peer_id,
                request_id: plan.request_id,
                task: plan.task.clone(),
            },
        )
        .await;
    }
    if let Some(client) = outbound_client(s, plan.task.task_id)? {
        match client
            .get::<TaskRecord>(&format!("/hmesh/v1/tasks/{}", plan.task.task_id))
            .await
        {
            Ok(task) => {
                s.store.cache_outbound(&task)?;
                Ok(task)
            }
            // A reserved envelope might have crashed before dispatch. Only resend that exact request.
            Err(e) if e.code == ErrorCode::TaskUnknown => {
                dispatch_task(
                    s,
                    RemoteTask {
                        peer_id: plan.peer_id,
                        request_id: plan.request_id,
                        task: plan.task.clone(),
                    },
                )
                .await
            }
            Err(e) => Err(e.into()),
        }
    } else if let Ok(task) = s.store.task(plan.task.task_id) {
        Ok(task)
    } else {
        dispatch_task(
            s,
            RemoteTask {
                peer_id: plan.peer_id,
                request_id: plan.request_id,
                task: plan.task.clone(),
            },
        )
        .await
    }
}
async fn step(s: &AppState, run: &mut WorkflowRun) -> std::result::Result<(), ApiError> {
    if *s.shutdown.borrow() {
        return Ok(());
    }
    if run.stage >= run.plans.len() {
        if run.state == "cancelling" {
            run.state = "cancelled".into();
            update(s, run)?;
        }
        return Ok(());
    }
    let plan = run.plans[run.stage][usize::from(run.repair_index)].clone();
    if run.state == "cancelling" {
        if s.store.outbound_request(plan.request_id).is_ok() {
            if let Some(client) = outbound_client(s, plan.task.task_id)? {
                let _: TaskRecord = client
                    .post(
                        &format!("/hmesh/v1/tasks/{}/cancel", plan.task.task_id),
                        &json!({}),
                    )
                    .await?;
            } else if let Ok(task) = s.store.task(plan.task.task_id)
                && !task.status.is_terminal()
            {
                s.store.request_cancel(task.task_id)?;
            }
            let task = current_task(s, &plan).await?;
            if !task.status.is_terminal() {
                return Ok(());
            }
        }
        run.state = "cancelled".into();
        update(s, run)?;
        return Ok(());
    }
    let task = current_task(s, &plan).await?;
    let was_error = run.last_error.take().is_some();
    if !task.status.is_terminal() {
        if was_error {
            update(s, run)?;
        }
        return Ok(());
    }
    if task.status == TaskStatus::Unknown {
        run.state = "needs_input".into();
        run.last_error = Some(ErrorCode::TaskUnknown);
        update(s, run)?;
        return Ok(());
    }
    let check = &run.spec.stages[run.stage].template.verification;
    let mut report = json!({"stage":run.stage,"repair_index":run.repair_index,"task_id":task.task_id,"attempt_id":task.last_attempt_id,"execution":task.status});
    let passed = if task.status == TaskStatus::Succeeded {
        if let Some(check) = check {
            let body = VerificationRequest {
                artifact: check.artifact.clone(),
                expected: check.expected.clone(),
            };
            let evidence = if let Some(client) = outbound_client(s, task.task_id)? {
                client
                    .post::<_, Value>(&format!("/hmesh/v1/tasks/{}/verify", task.task_id), &body)
                    .await?
            } else {
                verify_task_as(
                    s,
                    Actor {
                        node_id: s.instance.card.node_id,
                        owner: true,
                    },
                    task.task_id,
                    body,
                )?
            };
            let passed = evidence["passed"] == true;
            report["verification"] = evidence;
            passed
        } else {
            true
        }
    } else {
        false
    };
    run.reports.push(report);
    if passed {
        run.stage += 1;
        run.repair_index = 0;
        if run.stage == run.plans.len() {
            run.state = "review".into();
        }
    } else if matches!(task.status, TaskStatus::Succeeded | TaskStatus::Failed)
        && usize::from(run.repair_index) + 1 < run.plans[run.stage].len()
    {
        run.repair_index += 1;
    } else {
        run.state = "needs_input".into();
        run.last_error = Some(ErrorCode::FailedNeedsInput);
    }
    update(s, run)?;
    Ok(())
}
pub async fn run(s: AppState, mut stop: watch::Receiver<bool>) {
    let mut tick = tokio::time::interval(std::time::Duration::from_millis(500));
    loop {
        tokio::select! {changed=stop.changed()=>{if changed.is_err()||*stop.borrow(){break;}},_=tick.tick()=>{
            let Ok(runs)=s.store.managed_list::<WorkflowRun>("workflow") else{continue;};
            for mut run in runs.into_iter().filter(|r|matches!(r.state.as_str(),"running"|"cancelling")) {
                if *stop.borrow(){break;}
                let result=tokio::select! {
                    _=stop.changed()=>return,
                    result=tokio::time::timeout(std::time::Duration::from_secs(5),step(&s,&mut run))=>result.unwrap_or_else(|_|Err(MeshError::new(ErrorCode::PeerUnreachable,"workflow observation deadline").into())),
                };
                if let Err(e)=result {
                    // Read failures do not invent success or new IDs. Owner may cancel; a later tick observes the same task.
                    run.last_error=Some(e.0.code);
                    if e.0.code!=ErrorCode::PeerUnreachable {run.state="needs_input".into();}
                    let _=update(&s,&mut run);
                }
            }
        }}
    }
}
fn suggestion(code: ErrorCode) -> (&'static str, &'static str) {
    match code {
        ErrorCode::PeerUnreachable => (
            "network",
            "检查目标 Daemon、Tailnet 可达性与证书配对；恢复原请求。",
        ),
        ErrorCode::PermissionDenied | ErrorCode::Unauthenticated => (
            "authorization",
            "检查当前 peer、项目、Agent 授权与 owner 入口。",
        ),
        ErrorCode::AdapterNotSupported | ErrorCode::UnsupportedVersion => {
            ("compatibility", "核对固定 Harness 版本与协议回归矩阵。")
        }
        ErrorCode::FailedNeedsInput | ErrorCode::ApprovalRequired => (
            "approval_or_interaction",
            "检查待审批任务及 Harness 非交互配置。",
        ),
        ErrorCode::RateLimited => (
            "budget_or_capacity",
            "检查队列容量、执行时限、输出与已报告 token 上限。",
        ),
        ErrorCode::TaskUnknown => (
            "uncertain_execution",
            "检查原 attempt 及工件；不要自动重新执行。",
        ),
        ErrorCode::AgentUnavailable => (
            "harness_execution",
            "检查本机凭据引用、可执行文件与任务诊断分类。",
        ),
        _ => ("input_or_state", "检查结构化错误码、trace 和当前任务状态。"),
    }
}
async fn diagnostics(
    State(s): State<AppState>,
    Extension(c): Extension<ConnectionIdentity>,
    headers: HeaderMap,
) -> ApiResult<Value> {
    s.owner(&headers, &c)?;
    let mut agents = Vec::new();
    for card in s.store.agents()? {
        let (_, config) = s.store.agent(card.agent_id)?;
        let executable = config.adapter.executable.is_file()
            && config
                .adapter
                .node_executable
                .as_ref()
                .is_none_or(|p| p.is_file());
        let credential_ok = hmesh_runtime::inspect_references(&config).is_ok();
        agents.push(json!({"agent_id":card.agent_id,"harness":card.harness,"version_supported":card.probe.version_supported,"executable_present":executable,"references_valid":credential_ok,"managed_run":card.probe.managed_run,"action":if !executable{"检查可执行文件引用"}else if !credential_ok{"检查凭据、模型或环境引用；报告不包含引用路径或内容"}else{"引用检查通过；未调用模型"}}));
    }
    let mut tasks = s.store.tasks()?;
    tasks.sort_by_key(|t| std::cmp::Reverse(t.updated_at));
    let rows:Vec<_>=tasks.into_iter().take(100).filter_map(|t|t.error.map(|e| {
        let (mut category,mut action)=suggestion(e.code);
        if e.message.contains("deadline") {category="timeout";action="核对任务与项目时限，检查原 attempt；增加时限不会恢复旧任务。";}
        if e.message.contains("protocol")||e.message.contains("JSON") {category="protocol";action="核对 Harness 版本与固定协议 fixture。";}
        json!({"task_id":t.task_id,"attempt_id":t.last_attempt_id,"status":t.status,"code":e.code,"trace_id":e.trace_id,"category":category,"suggestion":action})
    })).collect();
    let failed_checks=s.store.managed_list::<WorkflowRun>("workflow")?.into_iter().flat_map(|r|r.reports).filter(|r|r.pointer("/verification/passed")==Some(&Value::Bool(false))).map(|r|json!({"task_id":r["task_id"],"category":"artifact_verification","suggestion":"执行已结束，工件断言未通过；查看冻结契约和原 attempt。"})).collect::<Vec<_>>();
    Ok(Json(
        json!({"schema":1,"observed_at":now(),"node_id":s.instance.card.node_id,"network_mode":s.instance.config.node.network,"agents":agents,"task_problems":rows,"verification_problems":failed_checks,"queue":s.store.queue_status(&s.instance.config.runtime)?,"sensitive_text_included":false,"real_model_called":false}),
    ))
}
async fn storage(
    State(s): State<AppState>,
    Extension(c): Extension<ConnectionIdentity>,
    headers: HeaderMap,
) -> ApiResult<Value> {
    s.owner(&headers, &c)?;
    Ok(Json(s.store.storage_summary()?))
}
async fn usage(
    State(s): State<AppState>,
    Extension(c): Extension<ConnectionIdentity>,
    headers: HeaderMap,
) -> ApiResult<Value> {
    s.owner(&headers, &c)?;
    Ok(Json(s.store.usage_report()?))
}
async fn budgets(
    State(s): State<AppState>,
    Extension(c): Extension<ConnectionIdentity>,
    headers: HeaderMap,
) -> ApiResult<Vec<ProjectBudget>> {
    s.owner(&headers, &c)?;
    Ok(Json(s.store.budgets()?))
}
async fn set_budget(
    State(s): State<AppState>,
    Extension(c): Extension<ConnectionIdentity>,
    headers: HeaderMap,
    Json(body): Json<ProjectBudget>,
) -> ApiResult<Value> {
    s.owner(&headers, &c)?;
    s.store.set_project_budget(&body)?;
    s.store.audit(
        Some(s.instance.card.node_id),
        "budget.set",
        &body.project_id,
        "allowed",
        Uuid::new_v4(),
    )?;
    Ok(Json(
        json!({"saved":true,"applies_to":"new attempts","provider_hard_spend_cap":false}),
    ))
}
async fn compatibility(
    State(s): State<AppState>,
    Extension(c): Extension<ConnectionIdentity>,
    headers: HeaderMap,
) -> ApiResult<Value> {
    s.owner(&headers, &c)?;
    Ok(Json(hmesh_adapters::compatibility::matrix()))
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ProbeRequest {
    agent_id: Uuid,
}
async fn probe_compatibility(
    State(s): State<AppState>,
    Extension(c): Extension<ConnectionIdentity>,
    headers: HeaderMap,
    Json(body): Json<ProbeRequest>,
) -> ApiResult<Value> {
    s.owner(&headers, &c)?;
    let (card, config) = s.store.agent(body.agent_id)?;
    let root = s
        .instance
        .dir
        .join("version-probes")
        .join(Uuid::new_v4().to_string());
    let result = hmesh_adapters::probe(&config, &root).await;
    let _ = fs::remove_dir_all(&root);
    let probe = result.map_err(|_| {
        ApiError(MeshError::new(
            ErrorCode::AgentUnavailable,
            "isolated version entry failed; no model was called",
        ))
    })?;
    let known = hmesh_adapters::compatibility::versions(card.harness);
    let version = probe
        .version
        .as_ref()
        .filter(|v| known.contains(&v.as_str()));
    s.store.audit(
        Some(s.instance.card.node_id),
        "compatibility.probe",
        &body.agent_id.to_string(),
        "allowed",
        Uuid::new_v4(),
    )?;
    Ok(Json(
        json!({"agent_id":body.agent_id,"harness":card.harness,"version":version,"version_supported":probe.version_supported,"matches_registration":probe.version==card.probe.version,"model_called":false,"authentication":"not_checked","next":if probe.version_supported&&probe.version==card.probe.version{"version entry matches; real execution is separate"}else{"update Agent registration and explicitly reverify before using the changed Harness"}}),
    ))
}
