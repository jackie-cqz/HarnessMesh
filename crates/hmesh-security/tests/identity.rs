use hmesh_core::*;
use hmesh_security::*;
use uuid::Uuid;

/// Addresses in an address list may name hosts; cards and pairing codes
/// still may not. Either way the dialed IP passes the usual rules.
#[test]
fn peer_addresses_take_host_names_but_cards_keep_literal_ips() {
    for good in [
        "https://studio.local:38880",
        "https://localhost:38880",
        "https://build-01.corp.example.com:443",
        "https://STUDIO.local:38880",
        "https://192.168.1.20:38880",
        "https://[fd7a:115c:a1e0::1]:38880",
        "https://100.99.145.22:38880/",
    ] {
        validate_peer_address(good).unwrap_or_else(|e| panic!("{good}: {e}"));
    }
    let long = format!("https://{}.local:1", "a".repeat(300));
    let long_label = format!("https://{}.local:1", "a".repeat(64));
    for bad in [
        "https://169.254.169.254:38880",
        "https://[fe80::1]:38880",
        "https://0.0.0.0:38880",
        "https://user@studio.local:38880",
        "https://user:secret@studio.local:38880",
        "https://studio.local:38880/path",
        "https://studio.local:38880/?q=1",
        "https://studio.local:38880/#f",
        "https://studio.local:0",
        "http://studio.local:38880",
        "https://-studio.local:38880",
        "https://studio-.local:38880",
        "https://stu_dio.local:38880",
        "https://studio..local:38880",
        long.as_str(),
        long_label.as_str(),
        "studio.local:38880",
    ] {
        assert!(validate_peer_address(bad).is_err(), "{bad} accepted");
    }
    assert_eq!(
        validate_peer_addresses(&[
            "https://Studio.local:38880".into(),
            "https://studio.local:38880/".into(),
            "https://[::1]:7".into(),
        ])
        .unwrap(),
        ["https://studio.local:38880", "https://[::1]:7"]
    );
    let many: Vec<String> = (0..=MAX_PEER_ADDRESSES)
        .map(|n| format!("https://10.0.0.{n}:1"))
        .collect();
    assert!(validate_peer_addresses(&many).is_err());
    // The card and the pairing code: literal IPs (or localhost) only.
    assert!(validate_endpoint("https://studio.local:38880").is_err());
    assert!(parse_pair_code(&format!("hmesh://studio.local:38880/{}", "a".repeat(64))).is_err());
}

/// Later versions add fields to an address list; this one ignores them.
#[test]
fn an_address_list_with_fields_this_version_does_not_know_still_reads() {
    let id = Uuid::new_v4();
    let read: PeerAddresses = serde_json::from_value(serde_json::json!({
        "node_id": id,
        "revision": 7,
        "addresses": ["https://studio.local:38880"],
        "find_key": "00ff",
        "relay": {"later": true},
    }))
    .unwrap();
    assert_eq!(
        read,
        PeerAddresses {
            node_id: id,
            revision: 7,
            addresses: vec!["https://studio.local:38880".into()],
        }
    );
    // The card is still read strictly: one unknown field rejects it.
    let mut card = serde_json::to_value(NodeCard {
        version: PROTOCOL.into(),
        node_id: id,
        display_name: "studio".into(),
        fingerprint: "a".repeat(64),
        certificate_pem: String::new(),
        endpoint: "https://192.168.1.20:38880".into(),
    })
    .unwrap();
    card["addresses"] = serde_json::json!([]);
    assert!(serde_json::from_value::<NodeCard>(card).is_err());
}

/// `host_names` in node.toml: DNS names only, and not too many.
#[test]
fn host_names_in_the_configuration_are_dns_names() {
    let mut config = NodeConfig::default();
    config.node.host_names = Some(vec!["studio.local".into(), "localhost".into()]);
    config.validate().unwrap();
    for bad in [
        "studio.local:38880",
        "Studio.local",
        "192.168.1.20",
        "-x.local",
        "",
    ] {
        config.node.host_names = Some(vec![bad.into()]);
        assert!(config.validate().is_err(), "{bad} accepted");
    }
    config.node.host_names = Some(
        (0..MAX_PEER_ADDRESSES)
            .map(|n| format!("h{n}.local"))
            .collect(),
    );
    assert!(config.validate().is_err());
    config.node.host_names = Some(vec![]);
    config.validate().unwrap();
    // Not written unless set, so older versions still read node.toml.
    assert!(
        !toml::to_string(&NodeConfig::default())
            .unwrap()
            .contains("host_names")
    );
}

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
    // A direct network's address, on a LAN or the internet.
    for address in ["192.168.1.20:39880", "203.0.113.9:39880"] {
        a.card.endpoint = format!("https://{address}");
        let (endpoint, _) = parse_pair_code(&pair_code(&a.card)).expect("direct code");
        assert_eq!(endpoint, format!("https://{address}"));
    }
    for bad in [
        "https://127.0.0.1:7443/abc".to_string(),
        "hmesh://127.0.0.1:7443/abc".to_string(),
        format!("hmesh://169.254.169.254:7443/{}", a.card.fingerprint),
        format!("hmesh://studio.local:7443/{}", a.card.fingerprint),
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
