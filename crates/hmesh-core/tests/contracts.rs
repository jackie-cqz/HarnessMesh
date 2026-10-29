use hmesh_core::*;
use serde_json::json;
use uuid::Uuid;

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
