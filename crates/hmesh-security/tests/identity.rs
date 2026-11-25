use hmesh_core::*;
use hmesh_security::*;
use uuid::Uuid;

#[test]
fn tailnet_identity_has_exact_ip_san_without_rekeying_existing_instances() {
    let root = tempfile::tempdir().unwrap();
    let mut config = NodeConfig::default();
    config.node.network = "tailscale".into();
    config.node.expose_to_tailnet = true;
    config.node.listen_address = "100.99.145.22".parse().unwrap();
    let instance = Instance::init(&root.path().join("tailnet"), config).unwrap();
    validate_card(&instance.card).unwrap();
    let der = certificate_der(&instance.card.certificate_pem).unwrap();
    let (_, certificate) = x509_parser::parse_x509_certificate(der.as_ref()).unwrap();
    let san = certificate.subject_alternative_name().unwrap().unwrap();
    assert!(san.value.general_names.iter().any(|name| matches!(name,
        x509_parser::extensions::GeneralName::IPAddress(bytes) if *bytes == [100, 99, 145, 22])));
    let again = Instance::init(&instance.dir, NodeConfig::default()).unwrap();
    assert_eq!(again.card.fingerprint, instance.card.fingerprint);
    assert_eq!(again.config.node.network, "tailscale");
}

#[test]
fn an_outbound_identity_already_names_its_tailnet_ip_for_a_later_switch() {
    let root = tempfile::tempdir().unwrap();
    let mut config = NodeConfig::default();
    config.node.network = "tailscale".into();
    config.node.tailnet_address = Some("100.99.145.22".parse().unwrap());
    let instance = Instance::init(&root.path().join("outbound"), config).unwrap();
    validate_card(&instance.card).unwrap();
    assert!(endpoint_is_loopback(&instance.card.endpoint));
    let der = certificate_der(&instance.card.certificate_pem).unwrap();
    let (_, certificate) = x509_parser::parse_x509_certificate(der.as_ref()).unwrap();
    let san = certificate.subject_alternative_name().unwrap().unwrap();
    assert!(san.value.general_names.iter().any(|name| matches!(name,
        x509_parser::extensions::GeneralName::IPAddress(bytes) if *bytes == [100, 99, 145, 22])));
    assert!(endpoint_is_loopback("https://localhost:39880"));
    assert!(!endpoint_is_loopback("https://100.99.145.22:39880"));
}

#[test]
fn a_full_tailnet_node_does_not_call_peers_that_only_connect_out() {
    let only_mcp_peer = "https://127.0.0.1:39880";
    let full_peer = "https://100.99.145.23:39880";
    // Full Tailnet node: calls full peers, never an only-MCP one.
    let mut full = NodeConfig::default();
    full.node.network = "tailscale".into();
    full.node.expose_to_tailnet = true;
    full.node.listen_address = "100.99.145.22".parse().unwrap();
    assert!(can_call(&full, full_peer));
    assert!(!can_call(&full, only_mcp_peer));
    assert!(!can_call(&full, "https://localhost:39880"));
    // An only-MCP node calls out to full peers.
    let mut outbound = NodeConfig::default();
    outbound.node.network = "tailscale".into();
    outbound.node.tailnet_address = Some("100.99.145.22".parse().unwrap());
    assert!(can_call(&outbound, full_peer));
    // A loopback node (one computer) calls loopback peers as before.
    assert!(can_call(&NodeConfig::default(), only_mcp_peer));
}

#[test]
fn initialization_is_idempotent_and_instances_have_distinct_keys_and_tokens() {
    let root = tempfile::tempdir().expect("tempdir");
    let a = Instance::init(&root.path().join("a"), NodeConfig::default()).expect("init a");
    let old_key = std::fs::read(a.dir.join("identity.pk8")).expect("key");
    let again = Instance::init(&a.dir, NodeConfig::default()).expect("init again");
    let b = Instance::init(&root.path().join("b"), NodeConfig::default()).expect("init b");
    assert_eq!(a.card.node_id, again.card.node_id);
    assert_eq!(
        old_key,
        std::fs::read(again.dir.join("identity.pk8")).expect("key")
    );
    assert_ne!(a.card.node_id, b.card.node_id);
    assert_ne!(a.card.fingerprint, b.card.fingerprint);
    assert_ne!(a.owner_token(), b.owner_token());
}

#[test]
fn signed_pairing_is_bound_to_target_challenge_and_card() {
    let root = tempfile::tempdir().expect("tempdir");
    let a = Instance::init(&root.path().join("a"), NodeConfig::default()).expect("init");
    let target = Uuid::new_v4();
    let challenge = PairChallenge {
        nonce: Uuid::new_v4(),
        node_id: target,
        expires_at: now() + 120,
    };
    let mut submission = a.pair_submission(challenge).expect("sign");
    verify_pair_submission(&submission, target).expect("valid signature");
    assert!(verify_pair_submission(&submission, Uuid::new_v4()).is_err());
    submission.card.display_name = "spoofed".into();
    assert!(verify_pair_submission(&submission, target).is_err());
}

#[test]
fn wrong_fingerprint_and_non_loopback_card_are_rejected() {
    let root = tempfile::tempdir().expect("tempdir");
    let a = Instance::init(&root.path().join("a"), NodeConfig::default()).expect("init");
    validate_card(&a.card).expect("card");
    let mut bad = a.card.clone();
    bad.fingerprint = "0".repeat(64);
    assert!(validate_card(&bad).is_err());
    bad = a.card;
    bad.endpoint = "https://169.254.169.254:80".into();
    assert!(validate_card(&bad).is_err());
}

#[test]
fn pairing_code_carries_the_address_and_fingerprint_and_survives_wrapping() {
    let root = tempfile::tempdir().expect("tempdir");
    let mut a = Instance::init(&root.path().join("a"), NodeConfig::default()).expect("init");
    a.card.endpoint = "https://127.0.0.1:7443".into();
    let code = pair_code(&a.card);
    assert_eq!(
        code,
        format!("hmesh://127.0.0.1:7443/{}", a.card.fingerprint)
    );
    let wrapped = format!("  {}\n{}  ", &code[..30], &code[30..]);
    assert_eq!(
        parse_pair_code(&wrapped).expect("wrapped code"),
        (
            "https://127.0.0.1:7443".to_string(),
            a.card.fingerprint.clone()
        )
    );
    a.card.endpoint = "https://[fd7a:115c:a1e0::1]:7443".into();
    let (endpoint, _) = parse_pair_code(&pair_code(&a.card)).expect("IPv6 Tailnet code");
    assert_eq!(endpoint, "https://[fd7a:115c:a1e0::1]:7443");
    for bad in [
        "https://127.0.0.1:7443/abc".to_string(),
        "hmesh://127.0.0.1:7443/abc".to_string(),
        format!("hmesh://8.8.8.8:7443/{}", a.card.fingerprint),
        format!("hmesh://127.0.0.1:7443/x/{}", a.card.fingerprint),
    ] {
        assert!(parse_pair_code(&bad).is_err(), "{bad}");
    }
}

#[test]
fn partial_identity_is_not_silently_replaced() {
    let root = tempfile::tempdir().expect("tempdir");
    let dir = root.path().join("a");
    std::fs::create_dir(&dir).expect("dir");
    std::fs::write(dir.join("identity.pk8"), b"existing key").expect("seed");
    assert!(Instance::init(&dir, NodeConfig::default()).is_err());
    assert_eq!(
        std::fs::read(dir.join("identity.pk8")).expect("retained"),
        b"existing key"
    );
}

#[test]
fn daemon_lock_prevents_same_directory_double_start() {
    let root = tempfile::tempdir().expect("tempdir");
    let a = Instance::init(&root.path().join("a"), NodeConfig::default()).expect("init");
    let lock = a.lock_daemon().expect("lock");
    assert!(a.lock_daemon().is_err());
    drop(lock);
    assert!(a.lock_daemon().is_ok());
}
