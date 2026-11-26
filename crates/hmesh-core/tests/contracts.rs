use hmesh_core::*;
use serde_json::json;
use uuid::Uuid;

#[test]
fn artifact_contract_is_optional_portable_bounded_and_not_a_file_grant() {
    let mut task = TaskSubmit::new(Uuid::new_v4(), "fixture", "create an artifact");
    let old = serde_json::to_value(&task).unwrap();
    assert!(old.get("expected_artifacts").is_none());
    assert!(
        serde_json::from_value::<TaskSubmit>(old)
            .unwrap()
            .expected_artifacts
            .is_none()
    );
    task.expected_artifacts = Some(vec!["out/health.json".into()]);
    task.validate().unwrap();
    assert_eq!(
        serde_json::to_value(&task).unwrap()["expected_artifacts"],
        json!(["out/health.json"])
    );
    for path in [
        "",
        "../secret",
        "/absolute",
        "out/../secret",
        "C:\\secret",
        "out\\secret",
        "out/\nsecret",
    ] {
        task.expected_artifacts = Some(vec![path.into()]);
        assert!(task.validate().is_err(), "{path:?}");
    }
    task.expected_artifacts = Some(vec![]);
    assert!(task.validate().is_err());
    task.expected_artifacts = Some(vec!["out/a".into(), "out/a".into()]);
    assert!(task.validate().is_err());
    task.expected_artifacts = Some((0..17).map(|i| format!("out/{i}").into()).collect());
    assert!(task.validate().is_err());
}

#[test]
fn old_node_config_gets_bounded_pending_approval_expiry() {
    let mut config: NodeConfig = serde_json::from_value(json!({"security": {}})).unwrap();
    assert_eq!(config.security.pending_approval_ttl_seconds, 86400);
    for ttl in [1, 604800] {
        config.security.pending_approval_ttl_seconds = ttl;
        config.validate().unwrap();
    }
    for ttl in [0, 604801, u64::MAX] {
        config.security.pending_approval_ttl_seconds = ttl;
        assert!(config.validate().is_err());
    }
}

#[test]
fn tailnet_listeners_require_explicit_opt_in_and_exact_node_ip() {
    let mut config = NodeConfig::default();
    config.validate().unwrap();
    config.node.listen_address = "100.80.10.11".parse().unwrap();
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
fn a_computer_that_only_connects_out_listens_on_loopback_and_names_its_tailnet_ip() {
    let mut config = NodeConfig::default();
    config.node.network = "tailscale".into();
    config.node.tailnet_address = Some("100.80.10.11".parse().unwrap());
    config.validate().unwrap();
    assert!(config.outbound_only());
    // It never listens on the Tailnet, and the address must be a Tailnet one.
    config.node.listen_address = "100.80.10.11".parse().unwrap();
    assert!(config.validate().is_err());
    config.node.listen_address = "127.0.0.1".parse().unwrap();
    config.node.tailnet_address = Some("192.168.1.1".parse().unwrap());
    assert!(config.validate().is_err());
    config.node.tailnet_address = None;
    assert!(config.validate().is_err());
    // A loopback node has no Tailnet address.
    config.node.network = "loopback".into();
    config.node.tailnet_address = Some("100.80.10.11".parse().unwrap());
    assert!(config.validate().is_err());
    // A full node may name the address it listens on, and no other.
    config.node.network = "tailscale".into();
    config.node.expose_to_tailnet = true;
    config.node.listen_address = "100.80.10.11".parse().unwrap();
    config.validate().unwrap();
    assert!(!config.outbound_only());
    config.node.tailnet_address = Some("100.80.10.13".parse().unwrap());
    assert!(config.validate().is_err());
}

#[test]
fn a_direct_computer_listens_on_one_address_and_names_who_may_connect_when_public() {
    let direct = |listen: &str| {
        let mut config = NodeConfig::default();
        config.node.network = "direct".into();
        config.node.expose_to_tailnet = true;
        config.node.listen_address = listen.parse().unwrap();
        config
    };
    // LAN, overlay and loopback addresses: private ranges are allowed in.
    for listen in [
        "192.168.1.20",
        "10.147.17.5",
        "172.20.0.5",
        "100.70.0.3",
        "fd12::5",
        "127.0.0.1",
    ] {
        let config = direct(listen);
        config
            .validate()
            .unwrap_or_else(|e| panic!("{listen}: {e:?}"));
        assert!(!config.outbound_only());
        assert_eq!(config.allowed_sources(), default_allowed_sources());
    }
    assert!(direct("192.168.1.20").listens_for_peers());
    assert!(!direct("127.0.0.1").listens_for_peers());
    // Never every address at once, and never a group or broadcast address.
    for listen in ["0.0.0.0", "::", "224.0.0.251", "255.255.255.255"] {
        assert!(direct(listen).validate().is_err(), "{listen}");
    }
    // A public address must say who may connect, and that cannot be nobody.
    let mut public = direct("203.0.113.9");
    assert!(public.validate().is_err());
    public.node.allowed_sources = Some(vec![]);
    assert!(public.validate().is_err());
    public.node.allowed_sources = Some(vec!["198.51.100.0/24".parse().unwrap()]);
    public.validate().unwrap();
    // Only connects out: loopback for its owner, no ranges, no Tailnet IP.
    let mut outbound = direct("127.0.0.1");
    outbound.node.expose_to_tailnet = false;
    outbound.validate().unwrap();
    assert!(outbound.outbound_only());
    outbound.node.listen_address = "192.168.1.20".parse().unwrap();
    assert!(outbound.validate().is_err());
    outbound.node.listen_address = "127.0.0.1".parse().unwrap();
    outbound.node.tailnet_address = Some("100.64.0.7".parse().unwrap());
    assert!(outbound.validate().is_err());
    // Source ranges belong to the direct network only.
    let mut tailnet = NodeConfig::default();
    tailnet.node.network = "tailscale".into();
    tailnet.node.expose_to_tailnet = true;
    tailnet.node.listen_address = "100.80.10.11".parse().unwrap();
    tailnet.node.allowed_sources = Some(vec!["10.0.0.0/8".parse().unwrap()]);
    assert!(tailnet.validate().is_err());
    // The setting survives being written out and read back.
    public.node.allowed_sources = Some(vec!["198.51.100.0/24".parse().unwrap()]);
    let text = serde_json::to_string(&public).unwrap();
    let back: NodeConfig = serde_json::from_str(&text).unwrap();
    assert_eq!(back.allowed_sources(), public.allowed_sources());
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
