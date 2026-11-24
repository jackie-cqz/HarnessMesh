//! Read-only checks for an exact task. They never reserve a request or grant access.
use super::*;

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Check {
    pub code: String,
    pub passed: bool,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub field: Option<String>,
}
#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Report {
    pub ready: bool,
    pub agent_id: Uuid,
    pub project_id: String,
    pub requires_approval: bool,
    pub configuration_sha256: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub git_version: Option<hmesh_runtime::git_version::GitInspection>,
    pub checks: Vec<Check>,
    pub observed_at: i64,
}
pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/local/tasks/preflight", post(outgoing))
        .route("/local/tasks/{id}/preflight", get(owner_existing))
        .route("/hmesh/v1/tasks/preflight", post(incoming))
        .route("/hmesh/v1/tasks/{id}/preflight", get(peer_existing))
        .route("/local/agents/{id}/git", get(owner_git))
}
async fn owner_git(
    State(s): State<AppState>,
    Extension(c): Extension<ConnectionIdentity>,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
) -> ApiResult<hmesh_runtime::git_version::GitInspection> {
    s.owner(&headers, &c)?;
    let (_, config) = s.store.agent(id)?;
    Ok(Json(hmesh_runtime::git_version::inspect(&config, None)))
}
fn check(code: &str, passed: bool) -> Check {
    Check {
        code: code.into(),
        passed,
        field: None,
    }
}
pub(super) fn enforce_artifacts(s: &AppState, task: &TaskSubmit) -> Result<()> {
    if let Some(paths) = &task.expected_artifacts {
        let (_, config) = s.store.agent(task.target_agent_id)?;
        if paths
            .iter()
            .any(|path| !config.project.artifact_paths.contains(path))
        {
            return Err(MeshError::new(
                ErrorCode::PermissionDenied,
                "required completion artifact is not declared by the execution owner",
            ));
        }
    }
    Ok(())
}
pub(super) fn enforce_version(s: &AppState, task: &TaskSubmit) -> Result<()> {
    if let Some(version) = &task.git_version {
        let (_, config) = s.store.agent(task.target_agent_id)?;
        hmesh_runtime::git_version::enforce(&config, version).map_err(|_| {
            MeshError::new(
                ErrorCode::PermissionDenied,
                "execution repository does not meet the task Git version restriction",
            )
        })?;
    }
    Ok(())
}
pub(super) fn origin_actor(s: &AppState, task: &TaskRecord) -> Actor {
    Actor {
        node_id: task.origin_node_id,
        owner: task.origin_node_id == s.instance.card.node_id,
    }
}
pub(super) fn inspect_task(
    s: &AppState,
    actor: Actor,
    task: &TaskSubmit,
) -> std::result::Result<Report, ApiError> {
    inspect_task_with_git(s, actor, task, false)
}

#[derive(Deserialize)]
struct InspectQuery {
    #[serde(default)]
    observe_git: bool,
}

fn inspect_task_with_git(
    s: &AppState,
    actor: Actor,
    task: &TaskSubmit,
    observe_git: bool,
) -> std::result::Result<Report, ApiError> {
    task.validate()?;
    // Pairing is the trust boundary; the peer access mode selects auto/confirm.
    let decision = s.task_decision(actor);
    // Do not reveal configuration existence or metadata to an unpaired peer.
    if decision == Decision::Denied {
        return Err(MeshError::new(
            ErrorCode::PermissionDenied,
            "sender is not a currently paired peer",
        )
        .into());
    }
    super::validate_source_session(s, actor, task)?;
    let (agent, config) = s.store.agent(task.target_agent_id)?;
    let mut checks = vec![
        check(
            "PEER_NOT_TRUSTED",
            actor.owner
                || matches!(s.store.peer(actor.node_id), Ok(peer) if peer.status == "trusted"),
        ),
        check("TASK_PERMISSION_DENIED", decision != Decision::Denied),
        check(
            "PROJECT_MISMATCH",
            config.agent.project_id == task.project_id,
        ),
        check(
            "AGENT_UNSUPPORTED",
            agent.probe.installed && agent.probe.version_supported,
        ),
        check(
            "AGENT_NOT_VERIFIED",
            !matches!(agent.availability.as_str(), "disabled" | "retired")
                && (actor.owner
                    || (agent.probe.managed_run == "passed"
                        && matches!(agent.availability.as_str(), "idle" | "busy"))),
        ),
        check(
            "REFERENCES_INVALID",
            hmesh_runtime::inspect_references(&config).is_ok(),
        ),
        check("PROJECT_UNAVAILABLE", config.project.root.is_dir()),
    ];
    if let Some(expected) = &task.expected_tools {
        let mut expected = expected.clone();
        let mut actual = config.project.allowed_tools.clone();
        expected.sort();
        actual.sort();
        checks.push(check("TOOLS_MISMATCH", expected == actual));
    }
    for path in task.expected_artifacts.iter().flatten() {
        checks.push(Check {
            code: "ARTIFACT_NOT_DECLARED".into(),
            passed: config.project.artifact_paths.contains(path),
            field: Some(path.to_string_lossy().into_owned()),
        });
    }
    // Older peers deserialize a strict response. Preserve its shape unless they
    // ask to observe Git or explicitly send a version restriction.
    let git_version = (observe_git || task.git_version.is_some())
        .then(|| hmesh_runtime::git_version::inspect(&config, task.git_version.as_ref()));
    if let (Some(inspection), Some(version)) = (&git_version, &task.git_version) {
        checks.extend(
            hmesh_runtime::git_version::version_checks(&config, inspection, version)
                .into_iter()
                .map(|(code, passed)| check(code, passed)),
        );
    }
    Ok(Report {
        ready: checks.iter().all(|c| c.passed),
        agent_id: agent.agent_id,
        project_id: task.project_id.clone(),
        requires_approval: task.requires_approval || decision == Decision::ApprovalRequired,
        configuration_sha256: Some(hmesh_security::sha256_hex(
            &serde_json::to_vec(&config).map_err(internal)?,
        )),
        git_version,
        checks,
        observed_at: now(),
    })
}
fn unavailable(task: &TaskSubmit, error: MeshError) -> Report {
    let code = match error.code {
        ErrorCode::PermissionDenied | ErrorCode::Unauthenticated => "TASK_PERMISSION_DENIED",
        ErrorCode::PeerUnreachable => "PEER_UNREACHABLE",
        _ => "PREFLIGHT_UNAVAILABLE",
    };
    Report {
        ready: false,
        agent_id: task.target_agent_id,
        project_id: task.project_id.clone(),
        requires_approval: true,
        configuration_sha256: None,
        git_version: None,
        checks: vec![check(code, false)],
        observed_at: now(),
    }
}
async fn outgoing(
    State(s): State<AppState>,
    Extension(c): Extension<ConnectionIdentity>,
    headers: HeaderMap,
    Query(query): Query<InspectQuery>,
    Json(body): Json<RemoteTask>,
) -> ApiResult<Report> {
    s.owner(&headers, &c)?;
    body.task.validate()?;
    if body.peer_id == s.instance.card.node_id {
        return inspect_task_with_git(
            &s,
            Actor {
                node_id: s.instance.card.node_id,
                owner: true,
            },
            &body.task,
            query.observe_git,
        )
        .map(Json);
    }
    let peer = s.store.peer(body.peer_id)?;
    if peer.status != "trusted" {
        return Err(MeshError::new(ErrorCode::PermissionDenied, "peer is not trusted").into());
    }
    let client = Client::new(
        &s.instance,
        &peer.card.endpoint,
        &peer.card.fingerprint,
        false,
        false,
    )
    .map_err(internal)?;
    let mut envelope = Envelope::new(s.instance.card.node_id, "task.preflight", body.task.clone());
    envelope.request_id = body.request_id;
    let path = if query.observe_git {
        "/hmesh/v1/tasks/preflight?observe_git=true"
    } else {
        "/hmesh/v1/tasks/preflight"
    };
    let result = client.post(path, &envelope).await;
    Ok(Json(
        result.unwrap_or_else(|error| unavailable(&body.task, error)),
    ))
}
async fn incoming(
    State(s): State<AppState>,
    Extension(c): Extension<ConnectionIdentity>,
    headers: HeaderMap,
    Query(query): Query<InspectQuery>,
    Json(envelope): Json<Envelope<TaskSubmit>>,
) -> ApiResult<Report> {
    let actor = s.actor(&headers, &c)?;
    envelope.validate(actor.node_id, "task.preflight")?;
    inspect_task_with_git(&s, actor, &envelope.payload, query.observe_git).map(Json)
}
async fn owner_existing(
    State(s): State<AppState>,
    Extension(c): Extension<ConnectionIdentity>,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
) -> ApiResult<Report> {
    s.owner(&headers, &c)?;
    if let Some(client) = outbound_client(&s, id)? {
        let (_, envelope) = s.store.outbound_request(id)?;
        let result = client.get(&format!("/hmesh/v1/tasks/{id}/preflight")).await;
        Ok(Json(result.unwrap_or_else(|error| {
            unavailable(&envelope.payload, error)
        })))
    } else {
        let task = s.store.task(id)?;
        inspect_task(&s, origin_actor(&s, &task), &task.request).map(Json)
    }
}
async fn peer_existing(
    State(s): State<AppState>,
    Extension(c): Extension<ConnectionIdentity>,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
) -> ApiResult<Report> {
    let actor = s.actor(&headers, &c)?;
    let task = s.task_access(actor, id)?;
    inspect_task(&s, actor, &task.request).map(Json)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture() -> (tempfile::TempDir, AppState, TaskSubmit) {
        let root = tempfile::tempdir().unwrap();
        let mut options = NodeConfig::default();
        options.node.listen_port = 0;
        let instance = Arc::new(Instance::init(&root.path().join("node"), options).unwrap());
        let store = Store::open(&instance.dir.join("state.sqlite3")).unwrap();
        let (shutdown, _) = watch::channel(false);
        let state = AppState::load(instance, store, shutdown).unwrap();
        let config: AgentConfig = serde_json::from_value(json!({
            "agent":{"name":"preflight-fixture","harness":"acp","project_id":"fixture","mode":"managed"},
            "project":{"root":root.path(),"allowed_tools":["read","write"],"artifact_paths":["out/existing.json"]},
            "adapter":{"executable":std::env::current_exe().unwrap()}
        })).unwrap();
        let id = Uuid::new_v4();
        let card: AgentCard = serde_json::from_value(json!({
            "agent_id":id,"node_id":state.instance.card.node_id,"name":"preflight-fixture","harness":"acp","project_ids":["fixture"],"availability":"idle","capabilities":[],
            "probe":{"installed":true,"version":"fixture","version_supported":true,"protocol_probe":"fixture","authentication":"fixture","managed_run":"passed","streaming":"fixture","cancel":"fixture","session_resume":"fixture","unavailable_reason":null}
        })).unwrap();
        state.store.register_agent(&card, &config).unwrap();
        let mut task = TaskSubmit::new(id, "fixture", "synthetic preflight; do not execute");
        task.expected_artifacts = Some(vec!["out/health.json".into()]);
        (root, state, task)
    }

    #[test]
    fn missing_artifact_is_blocked_without_reserving_a_task_or_changing_configuration() {
        let (_root, state, task) = fixture();
        let owner = Actor {
            node_id: state.instance.card.node_id,
            owner: true,
        };
        let before =
            serde_json::to_value(state.store.agent(task.target_agent_id).unwrap().1).unwrap();
        let report = inspect_task(&state, owner, &task).unwrap();
        assert!(!report.ready);
        assert!(
            report
                .checks
                .iter()
                .any(|c| c.code == "ARTIFACT_NOT_DECLARED" && !c.passed)
        );
        assert!(state.store.tasks().unwrap().is_empty());
        assert!(state.store.outbound_tasks().unwrap().is_empty());
        assert_eq!(
            before,
            serde_json::to_value(state.store.agent(task.target_agent_id).unwrap().1).unwrap()
        );
        let envelope = Envelope::new(owner.node_id, "task.submit", task.clone());
        assert_eq!(
            admit_task(&state, owner, envelope).unwrap_err().0.code,
            ErrorCode::PermissionDenied
        );
        assert!(state.store.tasks().unwrap().is_empty());
        state
            .store
            .set_artifact_paths(
                task.target_agent_id,
                &["out/existing.json".into(), "out/health.json".into()],
            )
            .unwrap();
        assert!(inspect_task(&state, owner, &task).unwrap().ready);
        assert!(state.store.tasks().unwrap().is_empty());
    }

    #[tokio::test]
    async fn changed_declaration_blocks_approval_and_execution_of_the_original_task() {
        let (_root, state, mut task) = fixture();
        // The sender opts in to local confirmation so the task waits for approval.
        task.requires_approval = true;
        state
            .store
            .set_artifact_paths(task.target_agent_id, &["out/health.json".into()])
            .unwrap();
        let owner = Actor {
            node_id: state.instance.card.node_id,
            owner: true,
        };
        let admitted = admit_task(
            &state,
            owner,
            Envelope::new(owner.node_id, "task.submit", task.clone()),
        )
        .unwrap();
        state
            .store
            .set_artifact_paths(task.target_agent_id, &[])
            .unwrap();
        let mut headers = HeaderMap::new();
        headers.insert(
            "authorization",
            format!("Bearer {}", state.instance.owner_token())
                .parse()
                .unwrap(),
        );
        let connection = ConnectionIdentity {
            address: "127.0.0.1:1234".parse().unwrap(),
            fingerprint: Some(state.instance.card.fingerprint.clone()),
        };
        let denied = approve_task(
            State(state.clone()),
            Extension(connection),
            headers,
            Path(admitted.task_id),
        )
        .await
        .unwrap_err();
        assert_eq!(denied.0.code, ErrorCode::PermissionDenied);
        assert_eq!(
            state.store.task(admitted.task_id).unwrap().status,
            TaskStatus::PendingApproval
        );
        assert!(state.authorize_execution(&admitted).is_err());
        assert!(
            state
                .store
                .task(admitted.task_id)
                .unwrap()
                .last_attempt_id
                .is_none()
        );
    }

    #[test]
    fn unrelated_peers_cannot_probe_agent_configuration_and_tool_changes_block_readiness() {
        let (_root, state, mut task) = fixture();
        let peer = Actor {
            node_id: Uuid::new_v4(),
            owner: false,
        };
        assert_eq!(
            inspect_task(&state, peer, &task).unwrap_err().0.code,
            ErrorCode::PermissionDenied
        );
        task.target_agent_id = Uuid::new_v4();
        assert_eq!(
            inspect_task(&state, peer, &task).unwrap_err().0.code,
            ErrorCode::PermissionDenied
        );
        let (_root, state, mut task) = fixture();
        task.expected_artifacts = None;
        task.expected_tools = Some(vec!["read".into()]);
        let report = inspect_task(
            &state,
            Actor {
                node_id: state.instance.card.node_id,
                owner: true,
            },
            &task,
        )
        .unwrap();
        assert!(
            !report.ready
                && report
                    .checks
                    .iter()
                    .any(|c| c.code == "TOOLS_MISMATCH" && !c.passed)
        );
    }

    #[tokio::test]
    async fn git_observations_are_optional_but_restrictions_block_admission_approval_and_execution()
    {
        let (root, state, mut task) = fixture();
        task.expected_artifacts = None;
        let owner = Actor {
            node_id: state.instance.card.node_id,
            owner: true,
        };
        let report = inspect_task(&state, owner, &task).unwrap();
        assert!(report.ready);
        assert!(
            serde_json::to_value(report)
                .unwrap()
                .get("git_version")
                .is_none()
        );
        let observed = inspect_task_with_git(&state, owner, &task, true).unwrap();
        assert!(observed.ready && !observed.git_version.unwrap().available);
        let project = root.path().join("git-project");
        std::fs::create_dir(&project).unwrap();
        let executable = std::env::split_paths(&std::env::var_os("PATH").unwrap())
            .filter(|p| p.is_absolute())
            .map(|p| p.join(if cfg!(windows) { "git.exe" } else { "git" }))
            .find(|p| p.is_file())
            .unwrap();
        let git = |args: &[&str]| {
            let result = std::process::Command::new(&executable)
                .arg("-C")
                .arg(&project)
                .args([
                    "-c",
                    "user.name=Fixture",
                    "-c",
                    "user.email=fixture@example.invalid",
                    "-c",
                    "commit.gpgsign=false",
                ])
                .args(args)
                .output()
                .unwrap();
            assert!(result.status.success());
            String::from_utf8(result.stdout).unwrap().trim().to_owned()
        };
        git(&["init", "-b", "main"]);
        git(&["commit", "--allow-empty", "-m", "first"]);
        let first = git(&["rev-parse", "HEAD"]);
        let (mut card, mut config) = state.store.agent(task.target_agent_id).unwrap();
        card.agent_id = Uuid::new_v4();
        card.project_ids = vec!["git-fixture".into()];
        card.name = "git-fixture".into();
        config.agent.name = "git-fixture".into();
        config.agent.project_id = "git-fixture".into();
        config.project.root = project.clone();
        config.project.git_executable = Some(executable.clone());
        state.store.register_agent(&card, &config).unwrap();
        task.target_agent_id = card.agent_id;
        task.project_id = "git-fixture".into();
        task.git_version = Some(GitVersionRequirement::Current {
            expected_commit: Some("0".repeat(40)),
            require_clean: false,
        });
        let report = inspect_task(&state, owner, &task).unwrap();
        assert!(!report.ready);
        assert!(
            report
                .checks
                .iter()
                .any(|c| c.code == "GIT_COMMIT_MISMATCH" && !c.passed)
        );
        assert_eq!(report.git_version.unwrap().current.unwrap().commit, first);
        assert!(
            admit_task(
                &state,
                owner,
                Envelope::new(owner.node_id, "task.submit", task.clone())
            )
            .is_err()
        );
        assert!(state.store.tasks().unwrap().is_empty());
        task.git_version = Some(GitVersionRequirement::Current {
            expected_commit: Some(first),
            require_clean: false,
        });
        task.requires_approval = true;
        assert!(inspect_task(&state, owner, &task).unwrap().ready);
        let admitted = admit_task(
            &state,
            owner,
            Envelope::new(owner.node_id, "task.submit", task),
        )
        .unwrap();
        git(&["commit", "--allow-empty", "-m", "changed before approval"]);
        let mut headers = HeaderMap::new();
        headers.insert(
            "authorization",
            format!("Bearer {}", state.instance.owner_token())
                .parse()
                .unwrap(),
        );
        let connection = ConnectionIdentity {
            address: "127.0.0.1:1234".parse().unwrap(),
            fingerprint: Some(state.instance.card.fingerprint.clone()),
        };
        assert!(
            approve_task(
                State(state.clone()),
                Extension(connection),
                headers,
                Path(admitted.task_id)
            )
            .await
            .is_err()
        );
        assert_eq!(
            state.store.task(admitted.task_id).unwrap().status,
            TaskStatus::PendingApproval
        );
        assert!(enforce_version(&state, &admitted.request).is_err());
        assert!(state.store.workspace(admitted.task_id).unwrap().is_none());
    }
}
