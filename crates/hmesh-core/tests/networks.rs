//! Listening on every interface: which networks a computer trusts, how a
//! network it is on now is recognized, and which configurations allow it.
use hmesh_core::*;

fn lan(address: &str, subnet: &str, mac: Option<&str>) -> LocalNetwork {
    LocalNetwork {
        interface: "en0".into(),
        address: address.parse().unwrap(),
        subnet: subnet.parse().unwrap(),
        gateway: mac.map(|_| "192.168.1.1".parse().unwrap()),
        gateway_mac: mac.map(str::to_owned),
        tailnet: false,
    }
}
fn trusted(subnet: &str, gateway: Option<&str>) -> TrustedNetwork {
    TrustedNetwork {
        subnet: subnet.parse().unwrap(),
        gateway: gateway.map(str::to_owned),
        name: None,
        tailnet: false,
    }
}
fn tailnet_address() -> LocalNetwork {
    LocalNetwork {
        interface: "utun4".into(),
        address: "100.101.102.103".parse().unwrap(),
        subnet: "100.101.102.103/32".parse().unwrap(),
        gateway: None,
        gateway_mac: None,
        tailnet: true,
    }
}

/// Home and a café are both 192.168.1.0/24; their routers are not the same
/// device. A gateway that cannot be seen now does not shut the door, but is
/// reported as unchecked.
#[test]
fn a_trusted_network_is_its_subnet_behind_its_gateway() {
    let home = "a0:b1:c2:d3:e4:f5";
    let at_home = lan("192.168.1.20", "192.168.1.0/24", Some(home));
    let in_a_cafe = lan("192.168.1.57", "192.168.1.0/24", Some("10:20:30:40:50:60"));
    let unseen = lan("192.168.1.20", "192.168.1.0/24", None);
    let elsewhere = lan("10.0.0.5", "10.0.0.0/24", Some(home));
    let entry = trusted("192.168.1.0/24", Some(home));
    assert_eq!(entry.recognizes(&at_home), Some(false));
    assert_eq!(entry.recognizes(&in_a_cafe), None);
    assert_eq!(entry.recognizes(&unseen), Some(true), "unverified");
    assert_eq!(entry.recognizes(&elsewhere), None);
    // Hardware addresses compare without regard to case.
    assert_eq!(
        trusted("192.168.1.0/24", Some(home)).recognizes(&lan(
            "192.168.1.20",
            "192.168.1.0/24",
            Some("A0:B1:C2:D3:E4:F5")
        )),
        Some(false)
    );
    // A subnet trusted without a gateway is that subnet anywhere.
    let anywhere = trusted("192.168.1.0/24", None);
    assert_eq!(anywhere.recognizes(&in_a_cafe), Some(false));
    assert_eq!(anywhere.recognizes(&unseen), Some(false));
    // Subnets compare by their network, not how they were written.
    assert_eq!(
        trusted("192.168.1.77/24", None).recognizes(&at_home),
        Some(false)
    );
    // A narrower or wider subnet is another network.
    assert_eq!(trusted("192.168.0.0/16", None).recognizes(&at_home), None);
    // The Tailscale entry is any Tailscale address, and nothing else; a LAN
    // entry never stands for a Tailscale address.
    assert_eq!(
        TrustedNetwork::tailnet().recognizes(&tailnet_address()),
        Some(false)
    );
    assert_eq!(TrustedNetwork::tailnet().recognizes(&at_home), None);
    assert_eq!(
        trusted("100.101.102.103/32", None).recognizes(&tailnet_address()),
        None
    );
    // A checked gateway wins over an unchecked one.
    let list = [trusted("192.168.1.0/24", Some(home)), anywhere.clone()];
    assert_eq!(recognize(&list, &unseen), Some((1, false)));
    assert_eq!(recognize(&list, &at_home), Some((0, false)));
    assert_eq!(recognize(&list[..1], &unseen), Some((0, true)));
    assert_eq!(recognize(&[], &at_home), None);
}

/// Trusting takes the gateway from what this computer sees, never from
/// whoever asks, and only for a network it is on now.
#[test]
fn trusting_a_network_takes_its_gateway_from_what_is_observed() {
    let home = "a0:b1:c2:d3:e4:f5";
    let observed = [
        lan("192.168.1.20", "192.168.1.0/24", Some(home)),
        tailnet_address(),
    ];
    let mut list = vec![];
    let entry = trust_network(
        &mut list,
        &observed,
        "192.168.1.0/24".parse().unwrap(),
        Some("Home".into()),
    )
    .unwrap();
    assert_eq!(entry.gateway.as_deref(), Some(home));
    assert_eq!(list, std::slice::from_ref(&entry));
    // Again, renamed: replaced, not added.
    trust_network(
        &mut list,
        &observed,
        "192.168.1.0/24".parse().unwrap(),
        Some("Study".into()),
    )
    .unwrap();
    assert_eq!(list.len(), 1);
    assert_eq!(list[0].name.as_deref(), Some("Study"));
    // A Tailscale address's subnet trusts the Tailscale entry.
    let tailnet = trust_network(
        &mut list,
        &observed,
        "100.101.102.103/32".parse().unwrap(),
        None,
    )
    .unwrap();
    assert!(tailnet.tailnet);
    assert_eq!(tailnet, TrustedNetwork::tailnet());
    // Not a network this computer is on.
    let refused =
        trust_network(&mut list, &observed, "10.0.0.0/8".parse().unwrap(), None).unwrap_err();
    assert!(
        refused.contains("not a network this computer is on"),
        "{refused}"
    );
    assert!(refused.contains("192.168.1.0/24"), "{refused}");
    // A name that is not one.
    assert!(
        trust_network(
            &mut list,
            &observed,
            "192.168.1.0/24".parse().unwrap(),
            Some("x".repeat(65))
        )
        .is_err()
    );
    // Forgetting by subnet, or by subnet and gateway.
    assert_eq!(
        forget_network(
            &mut list,
            "192.168.1.0/24".parse().unwrap(),
            Some("10:20:30:40:50:60")
        ),
        0
    );
    assert_eq!(
        forget_network(&mut list, "192.168.1.0/24".parse().unwrap(), Some(home)),
        1
    );
    assert_eq!(
        forget_network(&mut list, "100.64.0.0/10".parse().unwrap(), None),
        1
    );
    assert!(list.is_empty());
}

#[test]
fn hardware_addresses_are_read_as_six_lowercase_octets() {
    for (text, read) in [
        ("a0:b1:c2:d3:e4:f5", Some("a0:b1:c2:d3:e4:f5")),
        ("A0-B1-C2-D3-E4-F5", Some("a0:b1:c2:d3:e4:f5")),
        // macOS drops leading zeros.
        ("0:1a:2b:3:4d:5e", Some("00:1a:2b:03:4d:5e")),
        // An incomplete ARP entry, broadcast, too short, not hex.
        ("00:00:00:00:00:00", None),
        ("ff:ff:ff:ff:ff:ff", None),
        ("a0:b1:c2:d3:e4", None),
        ("(incomplete)", None),
        ("a0:b1:c2:d3:e4:g5", None),
        ("a0:b1:c2:d3:e4:f5:06", None),
        ("a00:b1:c2:d3:e4:f5", None),
    ] {
        assert_eq!(normalize_hardware_address(text).as_deref(), read, "{text}");
    }
}

/// Every interface needs the direct network, a listener for other
/// computers and the list of networks it opens to (empty is allowed: nobody
/// gets in yet). Nothing else may listen on an unspecified address.
#[test]
fn only_a_direct_computer_with_trusted_networks_listens_on_every_interface() {
    let every = |listen: &str| {
        let mut config = NodeConfig::default();
        config.node.network = "direct".into();
        config.node.expose_to_tailnet = true;
        config.node.listen_address = listen.parse().unwrap();
        config.node.trusted_networks = Some(vec![]);
        config
    };
    for listen in ["0.0.0.0", "::"] {
        let config = every(listen);
        config
            .validate()
            .unwrap_or_else(|e| panic!("{listen}: {e:?}"));
        assert!(config.every_interface());
        assert!(config.listens_for_peers());
        assert!(!config.outbound_only());
    }
    let mut config = every("::");
    config.node.trusted_networks = Some(vec![
        trusted("192.168.1.0/24", Some("a0:b1:c2:d3:e4:f5")),
        TrustedNetwork::tailnet(),
    ]);
    config.validate().unwrap();
    // Read back as written.
    let text = serde_json::to_string(&config).unwrap();
    assert!(!text.contains("\"tailnet\":false"), "{text}");
    let back: NodeConfig = serde_json::from_str(&text).unwrap();
    assert_eq!(back.node.trusted_networks, config.node.trusted_networks);
    // Who may connect still applies, and cannot be nobody.
    config.node.allowed_sources = Some(vec![]);
    assert!(config.validate().is_err());
    config.node.allowed_sources = None;
    // A gateway written otherwise, a name too long, too many networks.
    for bad in [
        trusted("192.168.1.0/24", Some("A0:B1:C2:D3:E4:F5")),
        trusted("192.168.1.0/24", Some("router")),
        TrustedNetwork {
            name: Some("n".repeat(65)),
            ..trusted("10.0.0.0/8", None)
        },
        TrustedNetwork {
            gateway: Some("a0:b1:c2:d3:e4:f5".into()),
            ..TrustedNetwork::tailnet()
        },
    ] {
        let mut config = every("::");
        config.node.trusted_networks = Some(vec![bad.clone()]);
        assert!(config.validate().is_err(), "{bad:?}");
    }
    let mut crowded = every("::");
    crowded.node.trusted_networks = Some(vec![trusted("10.0.0.0/8", None); 33]);
    assert!(crowded.validate().is_err());
    // Unknown fields in an entry are refused, as everywhere in node.toml.
    assert!(
        serde_json::from_str::<TrustedNetwork>(r#"{"subnet":"10.0.0.0/8","router":"x"}"#).is_err()
    );
    // Without trusted networks, an unspecified address stays forbidden; a
    // literal address does not take them; other networks do not either.
    let mut wildcard = every("::");
    wildcard.node.trusted_networks = None;
    assert!(wildcard.validate().is_err());
    assert!(!wildcard.every_interface());
    let mut literal = every("192.168.1.20");
    assert!(literal.validate().is_err());
    literal.node.trusted_networks = None;
    literal.validate().unwrap();
    let mut outbound = every("::");
    outbound.node.expose_to_tailnet = false;
    assert!(outbound.validate().is_err());
    outbound.node.listen_address = "127.0.0.1".parse().unwrap();
    assert!(outbound.validate().is_err());
    let mut tailnet = every("::");
    tailnet.node.network = "tailscale".into();
    assert!(tailnet.validate().is_err());
    tailnet.node.listen_address = "100.99.145.22".parse().unwrap();
    assert!(tailnet.validate().is_err());
    tailnet.node.trusted_networks = None;
    tailnet.validate().unwrap();
}
