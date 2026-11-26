//! Project-first, read-only execution directory. File/reference checks stay on
//! the execution node; no project paths, launch arguments or credentials leave it.
use super::*;
use std::collections::BTreeMap;
use std::time::Duration;

const DISCOVERY_TIMEOUT: Duration = Duration::from_secs(3);

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Check {
    code: String,
    passed: bool,
    action: String,
}
#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Approval {
    /// Approval of a new human-opened session, on the execution computer.
    session_open: String,
    /// A human-opened session sends tool permission questions to its origin.
    tool_permissions: String,
    /// Agent-origin delegations use the execution computer's peer access mode.
    agent_tool_permissions: String,
    task: String,
}
#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Agent {
    agent_id: Uuid,
    name: String,
    harness: HarnessKind,
    ready: bool,
    session_ready: bool,
    task_ready: bool,
    /// A previous managed model run, independent of the registration handshake.
    verified: bool,
    requires_approval: bool,
    approval: Approval,
    checks: Vec<Check>,
}
#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Project {
    project_id: String,
    available: bool,
    ready: bool,
    agents: Vec<Agent>,
}
#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Issue {
    code: String,
    action: String,
}
#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Directory {
    node_id: Uuid,
    platform: Option<String>,
    architecture: Option<String>,
    reachable: bool,
    supported: Option<bool>,
    projects: Vec<Project>,
    issue: Option<Issue>,
    observed_at: i64,
    /// Every check is observational; no agent process or model was started.
    model_called: bool,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Target {
    peer_id: Option<Uuid>,
}

fn check(code: &str, passed: bool, action: &str) -> Check {
    Check {
        code: code.into(),
        passed,
        action: action.into(),
    }
}

fn legacy_pi(config: &AgentConfig) -> bool {
    if config.agent.harness.family() != HarnessKind::Pi {
        return false;
    }
    config.adapter.credential_source.is_some()
        || config.adapter.models_file.is_some()
        || config
            .adapter
            .executable
            .file_name()
            .and_then(|s| s.to_str())
            .is_some_and(|name| {
                matches!(
                    name.to_ascii_lowercase().as_str(),
                    "pi" | "pi.cmd" | "pi.ps1"
                )
            })
        || config.adapter.executable.components().any(|part| {
            part.as_os_str()
                .to_str()
                .is_some_and(|name| name.eq_ignore_ascii_case("pi-coding-agent"))
        })
}

fn executable_present(config: &AgentConfig) -> bool {
    let Ok(executable) = hmesh_adapters::executable::resolve_configured_executable(config) else {
        return false;
    };
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::metadata(executable.program)
            .is_ok_and(|metadata| metadata.permissions().mode() & 0o111 != 0)
    }
    #[cfg(not(unix))]
    {
        executable.program.is_file()
    }
}

fn inspect(card: &AgentCard, config: &AgentConfig, actor: Actor, decision: Decision) -> Agent {
    let current_dsh = !config.is_legacy_dsh();
    let current_pi = !legacy_pi(config);
    let configuration_valid = config.validate().is_ok();
    let executable = executable_present(config);
    let references = config
        .adapter
        .env_allowlist
        .iter()
        .all(|name| std::env::var_os(name).is_some_and(|value| !value.is_empty()))
        && config
            .project
            .git_executable
            .as_ref()
            .is_none_or(|path| path.is_file());
    let acp = card.probe.installed
        && card.probe.version_supported
        && matches!(
            card.probe.protocol_probe.as_str(),
            "acp_initialize_passed" | "passed_real_run"
        );
    let enabled = !matches!(card.availability.as_str(), "disabled" | "retired");
    let checks = vec![
        check(
            "LEGACY_DSH_UNSUPPORTED",
            current_dsh,
            "This headless DSH registration predates ACP. On the execution computer, use hmesh agent update with this Agent ID and the official dsh executable, or register a new DSH Agent under a different name.",
        ),
        check(
            "LEGACY_PI_ADAPTER",
            current_pi,
            "On the execution computer, register pi-acp instead of the retired Pi JSON adapter. ACP uses the agent's own login and model settings; do not copy model credentials.",
        ),
        check(
            "CONFIGURATION_INVALID",
            configuration_valid,
            "On the execution computer, review this Agent's registration and register or update its ACP configuration. This directory does not change the configuration.",
        ),
        check(
            "PROJECT_UNAVAILABLE",
            config.project.root.is_dir(),
            "Restore this registered project directory on the execution computer, or register an Agent for the intended project. No arbitrary directory can be supplied from another node.",
        ),
        check(
            "EXECUTABLE_UNAVAILABLE",
            executable,
            "On the execution computer, check the registered ACP executable and its Node runtime when required. Update the registration after installing or moving the executable.",
        ),
        check(
            "REFERENCES_INVALID",
            references,
            "On the execution computer, restore the registered environment references or Git executable when required. Their names, values and paths are not shared.",
        ),
        check(
            "ACP_PROBE_REQUIRED",
            acp,
            "On the execution computer, register or update the Agent to repeat its ACP initialize handshake. This read-only check does not launch it or call a model.",
        ),
        check(
            "AGENT_DISABLED",
            enabled,
            "On the execution computer, restore or update the Agent before opening a new session. Existing history is retained.",
        ),
    ];
    let session_ready = checks.iter().all(|c| c.passed);
    let verified = card.probe.managed_run == "passed";
    let task_ready = session_ready
        && (actor.owner || (verified && matches!(card.availability.as_str(), "idle" | "busy")));
    let requires_approval = decision == Decision::ApprovalRequired;
    Agent {
        agent_id: card.agent_id,
        name: card.name.clone(),
        harness: card.harness,
        ready: session_ready,
        session_ready,
        task_ready,
        verified,
        requires_approval,
        approval: Approval {
            session_open: if requires_approval {
                "local_owner"
            } else {
                "none"
            }
            .into(),
            tool_permissions: "origin".into(),
            agent_tool_permissions: if requires_approval {
                "local_owner"
            } else {
                "auto"
            }
            .into(),
            task: if requires_approval {
                "local_owner"
            } else {
                "none"
            }
            .into(),
        },
        checks,
    }
}

fn local(s: &AppState, actor: Actor) -> std::result::Result<Directory, ApiError> {
    let decision = s.task_decision(actor);
    // Authenticate and recheck pairing before examining any project/configuration.
    if decision == Decision::Denied {
        return Err(MeshError::new(
            ErrorCode::PermissionDenied,
            "currently paired peer required",
        )
        .into());
    }
    let mut projects = BTreeMap::<String, Project>::new();
    for card in s.store.agents()? {
        let (_, config) = s.store.agent(card.agent_id)?;
        let agent = inspect(&card, &config, actor, decision);
        let project = projects
            .entry(config.agent.project_id.clone())
            .or_insert_with(|| Project {
                project_id: config.agent.project_id.clone(),
                available: config.project.root.is_dir(),
                ready: false,
                agents: vec![],
            });
        project.ready |= agent.session_ready;
        project.agents.push(agent);
    }
    let issue = if projects.is_empty() {
        Some(Issue {
            code: "NO_REGISTERED_PROJECTS".into(),
            action: "On the execution computer, register an ACP Agent bound to the project you want to use. This directory never creates a project or grants access.".into(),
        })
    } else {
        None
    };
    Ok(Directory {
        node_id: s.instance.card.node_id,
        platform: Some(std::env::consts::OS.into()),
        architecture: Some(std::env::consts::ARCH.into()),
        reachable: true,
        supported: Some(true),
        projects: projects.into_values().collect(),
        issue,
        observed_at: now(),
        model_called: false,
    })
}

fn unavailable(peer: Uuid, error: MeshError) -> Directory {
    let (code, action, reachable, supported) = match error.code {
        ErrorCode::PermissionDenied | ErrorCode::Unauthenticated => (
            "DISCOVERY_PERMISSION_DENIED",
            "Verify current pairing on both computers. Only a currently trusted peer can read this directory; no policy is changed by this check.",
            true,
            None,
        ),
        ErrorCode::UnsupportedVersion => (
            "DISCOVERY_UNSUPPORTED",
            "The execution computer does not support this discovery response. Review its Daemon version and upgrade explicitly; do not infer readiness or approval mode from old cached Agent cards.",
            true,
            Some(false),
        ),
        ErrorCode::PeerUnreachable if error.message.contains("HTTP 404") => (
            "DISCOVERY_UNSUPPORTED",
            "The execution computer does not offer the project directory API. Review its Daemon version and upgrade explicitly; existing tasks are not restarted.",
            true,
            Some(false),
        ),
        ErrorCode::PeerUnreachable => (
            "PEER_UNREACHABLE",
            "Check that the execution computer and its existing Daemon are online, then refresh. Keep the original session or task ID; this read-only check does not retry model work.",
            false,
            None,
        ),
        _ => (
            "DISCOVERY_UNAVAILABLE",
            "The execution directory could not be read. Check that computer's local Daemon diagnostics and registration, then refresh. Readiness and approval mode remain unknown.",
            false,
            None,
        ),
    };
    Directory {
        node_id: peer,
        platform: None,
        architecture: None,
        reachable,
        supported,
        projects: vec![],
        issue: Some(Issue {
            code: code.into(),
            action: action.into(),
        }),
        observed_at: now(),
        model_called: false,
    }
}

async fn owner_directory(
    State(s): State<AppState>,
    Extension(c): Extension<ConnectionIdentity>,
    headers: HeaderMap,
    Query(target): Query<Target>,
) -> ApiResult<Directory> {
    s.owner(&headers, &c)?;
    let me = s.instance.card.node_id;
    let Some(peer_id) = target.peer_id.filter(|id| *id != me) else {
        return local(
            &s,
            Actor {
                node_id: me,
                owner: true,
            },
        )
        .map(Json);
    };
    let peer = s.store.peer(peer_id)?;
    if peer.status != "trusted" {
        return Err(MeshError::new(
            ErrorCode::PermissionDenied,
            "currently paired peer required",
        )
        .into());
    }
    if !s.reaches(&peer) {
        return Ok(Json(unavailable(
            peer_id,
            MeshError::new(
                ErrorCode::PeerUnreachable,
                "this computer only connects out and offers no agents",
            ),
        )));
    }
    let client = match s.peer_client(&peer) {
        Ok(client) => client,
        Err(_) => {
            return Ok(Json(unavailable(
                peer_id,
                MeshError::new(
                    ErrorCode::UnsupportedVersion,
                    "peer transport configuration unavailable",
                ),
            )));
        }
    };
    let fetched = tokio::time::timeout(
        DISCOVERY_TIMEOUT,
        client.get::<Directory>("/hmesh/v1/discovery"),
    )
    .await;
    match fetched {
        Ok(Ok(report)) if report.node_id == peer_id => {
            // Pairing may have changed while the remote read was in flight.
            if s.store.peer(peer_id)?.status != "trusted" {
                return Err(MeshError::new(
                    ErrorCode::PermissionDenied,
                    "pairing was revoked during discovery",
                )
                .into());
            }
            Ok(Json(report))
        }
        Ok(Ok(_)) => Ok(Json(unavailable(
            peer_id,
            MeshError::new(ErrorCode::UnsupportedVersion, "different node identity"),
        ))),
        Ok(Err(error)) => Ok(Json(unavailable(peer_id, error))),
        Err(_) => Ok(Json(unavailable(
            peer_id,
            MeshError::new(ErrorCode::PeerUnreachable, "discovery deadline exceeded"),
        ))),
    }
}

async fn peer_directory(
    State(s): State<AppState>,
    Extension(c): Extension<ConnectionIdentity>,
    headers: HeaderMap,
) -> ApiResult<Directory> {
    let actor = s.actor(&headers, &c)?;
    local(&s, actor).map(Json)
}

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/local/discovery", get(owner_directory))
        .route("/hmesh/v1/discovery", get(peer_directory))
}

#[cfg(test)]
mod tests {
    use super::*;
    use hmesh_transport::serve;
    use tokio::net::TcpListener;

    fn fixture() -> (tempfile::TempDir, AppState, AgentCard, AgentConfig) {
        let root = tempfile::tempdir().unwrap();
        let instance =
            Arc::new(Instance::init(&root.path().join("node"), NodeConfig::default()).unwrap());
        let store = Store::open(&instance.dir.join("state.sqlite3")).unwrap();
        let (shutdown, _) = watch::channel(false);
        let state = AppState::load(instance.clone(), store, shutdown).unwrap();
        let project = root.path().join("private-project-root");
        fs::create_dir(&project).unwrap();
        let config: AgentConfig = serde_json::from_value(json!({
            "agent":{"name":"directory-fixture","harness":"acp","project_id":"project-one","mode":"managed"},
            "project":{"root":project,"allowed_tools":["read","write"],"require_remote_task_approval":true},
            "adapter":{"executable":std::env::current_exe().unwrap()}
        })).unwrap();
        let card: AgentCard = serde_json::from_value(json!({
            "agent_id":Uuid::new_v4(),"node_id":instance.card.node_id,"name":config.agent.name,
            "harness":"acp","project_ids":["project-one"],"availability":"unverified","capabilities":[],
            "probe":{"installed":true,"version":"fixture-only","version_supported":true,
                "protocol_probe":"acp_initialize_passed","authentication":"not_checked","managed_run":"not_checked",
                "streaming":"not_checked","cancel":"not_checked","session_resume":"not_supported","unavailable_reason":null}
        })).unwrap();
        state.store.register_agent(&card, &config).unwrap();
        (root, state, card, config)
    }

    #[test]
    fn projects_are_grouped_without_paths_secrets_or_model_calls() {
        let (_root, state, card, config) = fixture();
        let owner = Actor {
            node_id: state.instance.card.node_id,
            owner: true,
        };
        let report = local(&state, owner).unwrap();
        assert_eq!(report.projects.len(), 1);
        let agent = &report.projects[0].agents[0];
        assert!(agent.session_ready && agent.ready && agent.task_ready);
        assert!(!agent.verified && !agent.requires_approval);
        assert_eq!(report.platform.as_deref(), Some(std::env::consts::OS));
        assert!(!report.model_called);
        assert!(state.store.tasks().unwrap().is_empty());
        assert!(state.store.sessions(10).unwrap().is_empty());
        let mut private = config.clone();
        private.adapter.model = Some("private-model-canary".into());
        private.adapter.provider = Some("private-provider-canary".into());
        private.adapter.args = vec!["--private-argument-canary".into()];
        private.adapter.env_allowlist = vec!["HMESH_DISCOVERY_PRIVATE_REFERENCE_CANARY".into()];
        let safe =
            serde_json::to_string(&inspect(&card, &private, owner, Decision::Allowed)).unwrap();
        for secret in [
            "private-model-canary",
            "private-provider-canary",
            "private-argument-canary",
            "HMESH_DISCOVERY_PRIVATE_REFERENCE_CANARY",
            config.project.root.to_str().unwrap(),
            config.adapter.executable.to_str().unwrap(),
        ] {
            assert!(!safe.contains(secret));
        }
    }

    #[test]
    fn legacy_and_missing_resources_have_specific_safe_actions() {
        let (root, state, card, config) = fixture();
        let owner = Actor {
            node_id: state.instance.card.node_id,
            owner: true,
        };
        let mut dsh = config.clone();
        let mut dsh_card = card.clone();
        dsh.agent.harness = HarnessKind::Dsh;
        dsh_card.harness = HarnessKind::Dsh;
        let legacy = inspect(&dsh_card, &dsh, owner, Decision::Allowed);
        assert!(!legacy.ready);
        assert!(
            legacy
                .checks
                .iter()
                .any(|c| c.code == "LEGACY_DSH_UNSUPPORTED"
                    && !c.passed
                    && c.action.contains("ACP"))
        );
        dsh.prepare_registration().unwrap();
        assert!(inspect(&dsh_card, &dsh, owner, Decision::Allowed).session_ready);
        let mut pi = config.clone();
        let mut pi_card = card.clone();
        pi.agent.harness = HarnessKind::Pi;
        pi_card.harness = HarnessKind::Pi;
        pi.adapter.executable = root.path().join("pi");
        let legacy = inspect(&pi_card, &pi, owner, Decision::Allowed);
        assert!(
            legacy
                .checks
                .iter()
                .any(|c| c.code == "LEGACY_PI_ADAPTER" && !c.passed)
        );
        let mut missing = config;
        missing.project.root = root.path().join("missing-project");
        missing.adapter.executable = root.path().join("missing-executable");
        let unavailable = inspect(&card, &missing, owner, Decision::Allowed);
        assert!(!unavailable.ready);
        assert!(
            unavailable
                .checks
                .iter()
                .any(|c| c.code == "PROJECT_UNAVAILABLE" && !c.passed)
        );
        assert!(
            unavailable
                .checks
                .iter()
                .any(|c| c.code == "EXECUTABLE_UNAVAILABLE" && !c.passed)
        );
    }

    #[test]
    fn execution_owner_peer_mode_is_authoritative_and_not_the_legacy_project_flag() {
        let (root, state, _, _) = fixture();
        let peer = Instance::init(&root.path().join("peer"), NodeConfig::default()).unwrap();
        let actor = Actor {
            node_id: peer.card.node_id,
            owner: false,
        };
        assert!(local(&state, actor).is_err());
        state.store.trust_peer(&peer.card).unwrap();
        let report = local(&state, actor).unwrap();
        let agent = &report.projects[0].agents[0];
        // The obsolete require_remote_task_approval=true is not the peer mode.
        assert!(agent.session_ready && !agent.task_ready && !agent.verified);
        assert!(!agent.requires_approval);
        assert_eq!(agent.approval.tool_permissions, "origin");
        let mut policy = Policy::default();
        policy
            .peers
            .overrides
            .insert(peer.card.node_id, PeerAccess::Confirm);
        fs::write(
            state.instance.dir.join("policy.toml"),
            toml::to_string(&policy).unwrap(),
        )
        .unwrap();
        let report = local(&state, actor).unwrap();
        let agent = &report.projects[0].agents[0];
        assert!(agent.requires_approval);
        assert_eq!(agent.approval.session_open, "local_owner");
        assert_eq!(agent.approval.agent_tool_permissions, "local_owner");
        assert_eq!(agent.approval.tool_permissions, "origin");
    }

    #[test]
    fn unavailable_peer_does_not_guess_platform_or_approval_and_never_echoes_errors() {
        let peer = Uuid::new_v4();
        for error in [
            MeshError::new(
                ErrorCode::PeerUnreachable,
                "private-path-and-credential-canary",
            ),
            MeshError::new(
                ErrorCode::PeerUnreachable,
                "peer returned HTTP 404 Not Found",
            ),
        ] {
            let report = unavailable(peer, error);
            assert_eq!(report.node_id, peer);
            assert!(report.platform.is_none() && report.projects.is_empty());
            assert!(
                !serde_json::to_string(&report)
                    .unwrap()
                    .contains("private-path-and-credential-canary")
            );
        }
    }

    #[tokio::test]
    async fn owner_remote_discovery_uses_the_execution_peers_actual_approval_mode() {
        let (root, execution, _, _) = fixture();
        let origin_instance =
            Arc::new(Instance::init(&root.path().join("origin"), NodeConfig::default()).unwrap());
        let origin_store = Store::open(&origin_instance.dir.join("state.sqlite3")).unwrap();
        let (origin_stop, origin_rx) = watch::channel(false);
        let origin = AppState::load(
            origin_instance.clone(),
            origin_store.clone(),
            origin_stop.clone(),
        )
        .unwrap();
        let mut policy = Policy::default();
        policy
            .peers
            .overrides
            .insert(origin_instance.card.node_id, PeerAccess::Confirm);
        fs::write(
            execution.instance.dir.join("policy.toml"),
            toml::to_string(&policy).unwrap(),
        )
        .unwrap();
        let execution_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let execution_endpoint = format!("https://{}", execution_listener.local_addr().unwrap());
        let mut execution_card = execution.instance.card.clone();
        execution_card.endpoint = execution_endpoint.clone();
        origin_store.trust_peer(&execution_card).unwrap();
        execution.store.trust_peer(&origin_instance.card).unwrap();
        let execution_tls = execution.instance.clone();
        let execution_store = execution.store.clone();
        let (execution_stop, execution_rx) = watch::channel(false);
        let execution_server = tokio::spawn(serve(
            execution_listener,
            router(execution),
            Arc::new(move || execution_tls.server_config(&execution_store.peers()?)),
            execution_rx,
        ));
        let origin_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let origin_endpoint = format!("https://{}", origin_listener.local_addr().unwrap());
        let origin_tls = origin_instance.clone();
        let origin_tls_store = origin_store.clone();
        let origin_server = tokio::spawn(serve(
            origin_listener,
            router(origin),
            Arc::new(move || origin_tls.server_config(&origin_tls_store.peers()?)),
            origin_rx,
        ));
        let owner = Client::new(
            &origin_instance,
            &origin_endpoint,
            &origin_instance.card.fingerprint,
            false,
            true,
        )
        .unwrap();
        let report: Directory = owner
            .get(&format!(
                "/local/discovery?peer_id={}",
                execution_card.node_id
            ))
            .await
            .unwrap();
        assert_eq!(report.node_id, execution_card.node_id);
        assert!(report.reachable && report.projects[0].agents[0].requires_approval);
        assert_eq!(
            report.projects[0].agents[0].approval.session_open,
            "local_owner"
        );
        // The origin's own automatic default must not replace the remote mode.
        assert_eq!(
            report.projects[0].agents[0].approval.tool_permissions,
            "origin"
        );
        let anonymous = Client::new(
            &origin_instance,
            &execution_endpoint,
            &execution_card.fingerprint,
            true,
            false,
        )
        .unwrap();
        assert_eq!(
            anonymous
                .get::<Value>("/hmesh/v1/discovery")
                .await
                .unwrap_err()
                .code,
            ErrorCode::Unauthenticated
        );
        execution_stop.send(true).unwrap();
        origin_stop.send(true).unwrap();
        execution_server.await.unwrap().unwrap();
        origin_server.await.unwrap().unwrap();
    }
}
