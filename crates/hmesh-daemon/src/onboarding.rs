use super::*;

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OnboardingRequest {
    pub agent_id: Uuid,
    pub config: AgentConfig,
    pub peer_id: Option<Uuid>,
    #[serde(default)]
    pub expected_sha256: Option<String>,
}
fn inspect(s: &AppState, body: &mut OnboardingRequest) -> std::result::Result<Value, ApiError> {
    body.config.prepare_registration()?;
    if s.store
        .agents()?
        .into_iter()
        .chain(s.store.retired_agents()?)
        .any(|a| a.name == body.config.agent.name && a.agent_id != body.agent_id)
    {
        return Err(ApiError(MeshError::new(
            ErrorCode::TaskConflict,
            "Agent name is already registered; use its original ID or choose another name",
        )));
    }
    body.config.project.root = fs::canonicalize(&body.config.project.root)
        .map_err(|_| ApiError(MeshError::input("local project directory is unavailable")))?;
    if !body.config.project.root.is_dir() {
        return Err(ApiError(MeshError::input(
            "project root must be a directory",
        )));
    }
    let references = hmesh_runtime::inspect_references(&body.config).map_err(|_| {
        ApiError(MeshError::input(
            "local executable/model/credential reference check failed; no model was called",
        ))
    })?;
    let mut policy = Policy::default();
    if let Some(peer) = body.peer_id {
        if !s
            .store
            .peers()?
            .iter()
            .any(|p| p.card.node_id == peer && p.status == "trusted")
        {
            return Err(ApiError(MeshError::input(
                "select a currently trusted peer",
            )));
        }
        policy.grant.push(Grant {
            peer,
            agent: Some(body.agent_id),
            project: body.config.agent.project_id.clone(),
            actions: vec!["agent.read".into(), "task.submit".into()],
            require_approval: true,
        });
    }
    let digest = hmesh_security::sha256_hex(
        &serde_json::to_vec(
            &json!({"agent_id":body.agent_id,"config":body.config,"peer_id":body.peer_id}),
        )
        .map_err(internal)?,
    );
    Ok(
        json!({"agent_id":body.agent_id,"name":body.config.agent.name,"project_id":body.config.agent.project_id,"project_root":body.config.project.root,"workspace_mode":body.config.project.workspace_mode,"harness":body.config.agent.harness,"launch_args":body.config.launch_args(),"model":body.config.adapter.model,"references":references,"configuration_sha256":digest,"policy_preview":toml::to_string_pretty(&policy).map_err(internal)?,"policy_applied":false,"model_called":false}),
    )
}
pub async fn preview(
    State(s): State<AppState>,
    Extension(c): Extension<ConnectionIdentity>,
    headers: HeaderMap,
    Json(mut body): Json<OnboardingRequest>,
) -> ApiResult<Value> {
    s.owner(&headers, &c)?;
    Ok(Json(inspect(&s, &mut body)?))
}
pub async fn register(
    State(s): State<AppState>,
    Extension(c): Extension<ConnectionIdentity>,
    headers: HeaderMap,
    Json(mut body): Json<OnboardingRequest>,
) -> ApiResult<Value> {
    s.owner(&headers, &c)?;
    let preview = inspect(&s, &mut body)?;
    if body.expected_sha256.as_deref() != preview["configuration_sha256"].as_str() {
        return Err(ApiError(MeshError::new(
            ErrorCode::TaskConflict,
            "preview changed; review this exact configuration again",
        )));
    }
    let card = if let Ok((card, config)) = s.store.agent(body.agent_id) {
        if serde_json::to_value(config).map_err(internal)?
            != serde_json::to_value(&body.config).map_err(internal)?
        {
            return Err(ApiError(MeshError::new(
                ErrorCode::TaskConflict,
                "registration ID already has another configuration",
            )));
        }
        card
    } else {
        register_config(&s, body.config, body.agent_id).await?
    };
    s.store.audit(
        Some(s.instance.card.node_id),
        "agent.onboard",
        &card.agent_id.to_string(),
        "unverified-policy-not-applied",
        Uuid::new_v4(),
    )?;
    Ok(Json(
        json!({"agent":card,"preview":preview,"policy_applied":false,"model_called":false}),
    ))
}
