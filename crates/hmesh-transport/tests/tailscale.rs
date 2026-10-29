use hmesh_core::*;
use hmesh_security::{Instance, validate_endpoint};
use hmesh_transport::{
    Client, permits_source,
    tailscale::{CliTailscale, TailscaleProvider, parse_status},
};
use serde_json::json;

#[test]
fn status_normalizes_candidates_without_importing_identity_or_trust() {
    let status = parse_status(&serde_json::to_vec(&json!({
        "BackendState":"Running", "Self":{"HostName":"windows", "TailscaleIPs":["100.99.145.22"], "Online":true},
        "Peer": {
            "ignored-key": {"HostName":"mac", "DNSName":"mac.example.ts.net.", "OS":"macOS", "Online":true,
                "TailscaleIPs":["100.69.246.117", "fd7a:115c:a1e0::1234", "192.168.0.1"], "FutureField":1},
            "offline": {"TailscaleIPs":["100.64.0.2"]},
            "missing-fields": {"HostName":"no-address"}
        }, "UnknownFutureTopLevel": true
    })).unwrap()).unwrap();
    status
        .require_local_ip("100.99.145.22".parse().unwrap())
        .unwrap();
    assert!(
        status
            .require_local_ip("100.69.246.117".parse().unwrap())
            .is_err()
    );
    assert_eq!(status.candidates.len(), 2);
    let mac = status
        .candidates
        .iter()
        .find(|node| node.hostname == "mac")
        .unwrap();
    assert_eq!(mac.dns_name, "mac.example.ts.net");
    assert_eq!(mac.ips.len(), 2);
    assert!(
        !status
            .candidates
            .iter()
            .find(|node| node.hostname.is_empty())
            .unwrap()
            .online
    );
    let serialized = serde_json::to_string(&status).unwrap();
    assert!(!serialized.contains("ignored-key"));
    assert!(!serialized.contains("trusted"));
}

#[test]
fn offline_login_missing_fields_schema_changes_and_large_status_fail_closed() {
    for backend in ["NeedsLogin", "Stopped", "Starting", "FutureState"] {
        let bytes =
            serde_json::to_vec(&json!({"BackendState":backend,"Self":null,"Peer":null})).unwrap();
        assert!(parse_status(&bytes).unwrap().require_running().is_err());
    }
    for bytes in [
        br#"{}"#.as_slice(),
        br#"{"BackendState":"Running","Self":{"TailscaleIPs":"100.64.0.1"}}"#,
        br#"{"BackendState":"Running","Peer":[]}"#,
        b"invalid JSON",
    ] {
        assert!(parse_status(bytes).is_err());
    }
    assert!(parse_status(&vec![b' '; 1048577]).is_err());
    let offline = parse_status(
        br#"{"BackendState":"Running","Self":{"Online":false,"TailscaleIPs":["100.64.0.1"]}}"#,
    )
    .unwrap();
    assert!(offline.require_running().is_err());
    let ipv6 = parse_status(br#"{"BackendState":"Running","Self":{"Online":true,"TailscaleIPs":["fd7a:115c:a1e0::1"]}}"#).unwrap();
    assert!(ipv6.preferred_local_ip().unwrap().is_ipv6());
    assert!(CliTailscale::discover(Some(std::path::Path::new("missing-tailscale"))).is_err());
    let root = tempfile::tempdir().unwrap();
    assert!(CliTailscale::discover(Some(&root.path().join("missing-cli"))).is_err());
}

#[tokio::test]
async fn cli_failure_is_actionable_without_echoing_private_output() {
    // A real child executable with deliberately invalid status args, no network/API.
    let executable = std::env::current_exe().unwrap();
    let provider = CliTailscale::discover(Some(&executable)).unwrap();
    let error = provider.status().await.unwrap_err();
    assert!(error.to_string().contains("Tailscale status failed"));
}

#[test]
fn source_and_destination_scope_reject_lan_public_dns_and_implicit_tailnet() {
    for address in ["100.64.0.1", "100.127.255.254", "fd7a:115c:a1e0::1"] {
        assert!(permits_source(true, address.parse().unwrap()));
        assert!(!permits_source(false, address.parse().unwrap()));
    }
    for address in [
        "127.0.0.1",
        "::1",
        "0.0.0.0",
        "192.168.1.1",
        "8.8.8.8",
        "100.128.0.1",
        "::ffff:100.64.0.1",
    ] {
        assert!(!permits_source(true, address.parse().unwrap()));
    }
    for endpoint in [
        "https://100.64.0.1:38880",
        "https://[fd7a:115c:a1e0::1]:38880",
    ] {
        validate_endpoint(endpoint).unwrap();
    }
    for endpoint in [
        "http://100.64.0.1",
        "https://100.63.255.255",
        "https://192.168.1.1",
        "https://example.ts.net",
        "https://100.64.0.1/path",
        "https://user@100.64.0.1",
    ] {
        assert!(validate_endpoint(endpoint).is_err(), "{endpoint}");
    }
    let root = tempfile::tempdir().unwrap();
    let instance = Instance::init(&root.path().join("local"), NodeConfig::default()).unwrap();
    assert!(
        Client::new(
            &instance,
            "https://100.64.0.1:38880",
            &instance.card.fingerprint,
            false,
            false
        )
        .is_err()
    );
    let mut config = NodeConfig::default();
    config.node.network = "tailscale".into();
    config.node.expose_to_tailnet = true;
    config.node.listen_address = "100.64.0.1".parse().unwrap();
    let tailnet = Instance::init(&root.path().join("tailnet"), config).unwrap();
    assert!(
        Client::new(
            &tailnet,
            "https://100.64.0.1:38880",
            &tailnet.card.fingerprint,
            false,
            true
        )
        .is_err()
    );
    assert!(
        Client::new(
            &tailnet,
            "https://100.64.0.1:38880",
            &tailnet.card.fingerprint,
            false,
            false
        )
        .is_ok()
    );
}
