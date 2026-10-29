use hmesh_core::*;
use serde_json::json;
use uuid::Uuid;

#[test]
fn tailnet_listeners_require_explicit_opt_in_and_exact_node_ip() {
    let mut config = NodeConfig::default();
    config.validate().unwrap();
    config.node.listen_address = "100.99.145.22".parse().unwrap();
    assert!(config.validate().is_err());
    config.node.network = "tailscale".into();
    assert!(config.validate().is_err());
    config.node.expose_to_tailnet = true;
    config.validate().unwrap();
    for address in [
        "0.0.0.0",
        "::",
        "127.0.0.1",
        "192.168.1.1",
        "100.63.255.255",
        "100.128.0.0",
        "::ffff:100.64.0.1",
    ] {
        config.node.listen_address = address.parse().unwrap();
        assert!(config.validate().is_err(), "{address}");
    }
    config.node.listen_address = "fd7a:115c:a1e0::1".parse().unwrap();
    config.validate().unwrap();
    config.node.tailscale_executable = Some("relative-tailscale".into());
    assert!(config.validate().is_err());
}

#[test]
fn coding_runs_can_explicitly_request_bounded_wire_output_without_changing_the_default() {
    let mut request = TaskSubmit::new(Uuid::new_v4(), "coding", "write server files");
    assert_eq!(request.limits.max_output_bytes, 1048576);
    request.limits.max_output_bytes = MAX_RUN_OUTPUT_BYTES;
    request.validate().expect("explicit coding-run quota");
    request.limits.max_output_bytes += 1;
    assert_eq!(
        request
            .validate()
            .expect_err("wire output remains bounded")
            .code,
        ErrorCode::InvalidInput
    );
}

#[test]
fn terminal_states_never_restart_and_unknown_is_not_retryable() {
    for state in [
        TaskStatus::Succeeded,
        TaskStatus::Failed,
        TaskStatus::Cancelled,
        TaskStatus::Rejected,
        TaskStatus::Expired,
        TaskStatus::Unknown,
    ] {
        assert!(state.is_terminal());
        assert!(!state.can_transition_to(TaskStatus::Running));
        assert!(!state.can_transition_to(TaskStatus::Queued));
    }
    assert!(!TaskStatus::Running.can_transition_to(TaskStatus::Cancelled));
    assert!(TaskStatus::Running.can_transition_to(TaskStatus::Cancelling));
    assert!(TaskStatus::Cancelling.can_transition_to(TaskStatus::Unknown));
}

#[test]
fn submit_rejects_remote_execution_options_and_oversized_goals() {
    let request = TaskSubmit::new(Uuid::new_v4(), "demo", "create health.json");
    let mut value = serde_json::to_value(&request).expect("serialize");
    value["cwd"] = json!("C:/Windows");
    assert!(serde_json::from_value::<TaskSubmit>(value).is_err());
    let mut oversized = request;
    oversized.goal = "x".repeat(MAX_GOAL_BYTES + 1);
    assert_eq!(
        oversized.validate().expect_err("oversize").code,
        ErrorCode::InvalidInput
    );
}

#[test]
fn authorization_is_scoped_to_peer_project_and_agent() {
    let peer = Uuid::new_v4();
    let agent = Uuid::new_v4();
    let policy = Policy {
        grant: vec![Grant {
            peer,
            project: "demo".into(),
            agent: Some(agent),
            actions: vec!["task.submit".into()],
            require_approval: true,
        }],
        ..Policy::default()
    };
    assert_eq!(
        policy.decide(peer, "demo", agent, "task.submit"),
        Decision::ApprovalRequired
    );
    assert_eq!(
        policy.decide(Uuid::new_v4(), "demo", agent, "task.submit"),
        Decision::Denied
    );
    assert_eq!(
        policy.decide(peer, "other", agent, "task.submit"),
        Decision::Denied
    );
    assert_eq!(
        policy.decide(peer, "demo", Uuid::new_v4(), "task.submit"),
        Decision::Denied
    );
    assert_eq!(
        policy.decide(peer, "demo", agent, "peer.manage"),
        Decision::Denied
    );
}

#[test]
fn protocol_version_and_sender_are_checked_against_authenticated_identity() {
    let sender = Uuid::new_v4();
    let mut request = Envelope::new(
        sender,
        "task.submit",
        TaskSubmit::new(Uuid::new_v4(), "demo", "test"),
    );
    assert!(request.validate(sender, "task.submit").is_ok());
    assert_eq!(
        request
            .validate(Uuid::new_v4(), "task.submit")
            .expect_err("spoof")
            .code,
        ErrorCode::Unauthenticated
    );
    request.version = "hmesh/99".into();
    assert_eq!(
        request
            .validate(sender, "task.submit")
            .expect_err("version")
            .code,
        ErrorCode::UnsupportedVersion
    );
}
