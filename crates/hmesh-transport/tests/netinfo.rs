//! Reading what networks a computer is on (every platform's command output,
//! on any platform), and the fence a listener on every interface keeps.
use hmesh_core::{LocalNetwork, default_allowed_sources};
use hmesh_transport::{Door, Fence, netinfo::*};
use std::net::{IpAddr, Ipv4Addr};

const GATEWAY: Ipv4Addr = Ipv4Addr::new(192, 168, 1, 1);

#[test]
fn macos_default_gateway_is_the_first_default_route_through_an_address() {
    let netstat = "Routing tables

Internet:
Destination        Gateway            Flags               Netif Expire
default            link#21            UCSIg               utun4
default            192.168.1.1        UGScg                 en0
default            10.0.0.1           UGScIg                en5
127                127.0.0.1          UCS                   lo0
127.0.0.1          127.0.0.1          UH                    lo0
169.254            link#11            UCS                   en0      !
192.168.1          link#11            UCS                   en0      !
192.168.1.1/32     link#11            UCS                   en0      !
192.168.1.1        a0:b1:c2:d3:e4:f5  UHLWIir               en0   1171
";
    assert_eq!(parse_macos_default_gateway(netstat), Some(GATEWAY));
    assert_eq!(
        parse_macos_default_gateway(
            "Routing tables\n\nInternet:\nDestination Gateway Flags Netif Expire\n127 127.0.0.1 UCS lo0\n"
        ),
        None
    );
    assert_eq!(parse_macos_default_gateway(""), None);
}

#[test]
fn linux_default_gateway_is_the_lowest_metric_route_to_everywhere() {
    let route =
        "Iface\tDestination\tGateway \tFlags\tRefCnt\tUse\tMetric\tMask\t\tMTU\tWindow\tIRTT
wlan0\t00000000\tFE01A8C0\t0003\t0\t0\t600\t00000000\t0\t0\t0
eth0\t00000000\t0101A8C0\t0003\t0\t0\t100\t00000000\t0\t0\t0
eth0\t0001A8C0\t00000000\t0001\t0\t0\t100\t00FFFFFF\t0\t0\t0
docker0\t000011AC\t00000000\t0001\t0\t0\t0\t0000FFFF\t0\t0\t0
";
    // The kernel prints the address as a host-order word.
    #[cfg(target_endian = "little")]
    assert_eq!(parse_linux_default_gateway(route), Some(GATEWAY));
    // A route that is down, or has no gateway, is not a default gateway.
    let down = "Iface\tDestination\tGateway \tFlags\tRefCnt\tUse\tMetric\tMask\t\tMTU\tWindow\tIRTT
eth0\t00000000\t0101A8C0\t0002\t0\t0\t100\t00000000\t0\t0\t0
eth0\t00000000\t00000000\t0001\t0\t0\t100\t00000000\t0\t0\t0
";
    assert_eq!(parse_linux_default_gateway(down), None);
    assert_eq!(parse_linux_default_gateway("garbage\n"), None);
}

#[test]
fn windows_default_gateway_is_read_from_the_route_table_in_any_language() {
    let english = "===========================================================================
Interface List
 12...00 15 5d 01 02 03 ......Intel(R) Ethernet Connection
  1...........................Software Loopback Interface 1
===========================================================================

IPv4 Route Table
===========================================================================
Active Routes:
Network Destination        Netmask          Gateway       Interface  Metric
          0.0.0.0          0.0.0.0       10.0.0.1       10.0.0.42     50
          0.0.0.0          0.0.0.0      192.168.1.1    192.168.1.23     25
        127.0.0.0        255.0.0.0         On-link         127.0.0.1    331
      192.168.1.0    255.255.255.0         On-link      192.168.1.23    281
===========================================================================
Persistent Routes:
  Network Address          Netmask  Gateway Address  Metric
          0.0.0.0          0.0.0.0      172.16.0.1  Default
===========================================================================
";
    assert_eq!(parse_windows_default_gateway(english), Some(GATEWAY));
    let german = "IPv4-Routentabelle
===========================================================================
Aktive Routen:
     Netzwerkziel    Netzwerkmaske          Gateway    Schnittstelle Metrik
          0.0.0.0          0.0.0.0      192.168.1.1    192.168.1.23     35
        127.0.0.0        255.0.0.0   Auf Verbindung         127.0.0.1    331
===========================================================================
Ständige Routen:
  Keine
";
    assert_eq!(parse_windows_default_gateway(german), Some(GATEWAY));
    let none = "Active Routes:\n        127.0.0.0        255.0.0.0         On-link         127.0.0.1    331\nPersistent Routes:\n          0.0.0.0          0.0.0.0      192.168.1.1  Default\n";
    assert_eq!(parse_windows_default_gateway(none), None);
}

#[test]
fn the_gateways_hardware_address_is_read_from_each_arp_table() {
    let mac = Some("a0:b1:c2:d3:e4:f5".to_owned());
    // macOS
    assert_eq!(
        parse_macos_arp(
            "? (192.168.1.1) at a0:b1:c2:d3:e4:f5 on en0 ifscope [ethernet]\n",
            GATEWAY
        ),
        mac
    );
    assert_eq!(
        parse_macos_arp(
            "? (192.168.1.1) at 0:1a:2b:3:4d:5e on en0 ifscope [ethernet]\n",
            GATEWAY
        )
        .as_deref(),
        Some("00:1a:2b:03:4d:5e")
    );
    for nothing in [
        "? (192.168.1.1) at (incomplete) on en0 ifscope [ethernet]\n",
        "192.168.1.1 (192.168.1.1) -- no entry\n",
        "? (192.168.1.10) at a0:b1:c2:d3:e4:f5 on en0 ifscope [ethernet]\n",
        "",
    ] {
        assert_eq!(parse_macos_arp(nothing, GATEWAY), None, "{nothing}");
    }
    // Linux
    let table = "IP address       HW type     Flags       HW address            Mask     Device
192.168.1.10     0x1         0x2         11:22:33:44:55:66     *        eth0
192.168.1.1      0x1         0x2         a0:b1:c2:d3:e4:f5     *        eth0
192.168.1.99     0x1         0x0         00:00:00:00:00:00     *        eth0
";
    assert_eq!(parse_linux_arp(table, GATEWAY), mac);
    assert_eq!(
        parse_linux_arp(table, Ipv4Addr::new(192, 168, 1, 99)),
        None,
        "incomplete"
    );
    assert_eq!(parse_linux_arp(table, Ipv4Addr::new(192, 168, 1, 2)), None);
    // Windows
    let arp = "
Interface: 192.168.1.23 --- 0xc
  Internet Address      Physical Address      Type
  192.168.1.10          11-22-33-44-55-66     dynamic
  192.168.1.1           a0-b1-c2-d3-e4-f5     dynamic
";
    assert_eq!(parse_windows_arp(arp, GATEWAY), mac);
    assert_eq!(parse_windows_arp("No ARP Entries Found.\n", GATEWAY), None);
}

/// Loopback, link-local and repeated addresses are left out; the network
/// with the default route comes first and gets the gateway; Tailscale
/// addresses are marked and come last.
#[test]
fn networks_are_the_addresses_other_computers_could_reach() {
    let interface = |name: &str, address: &str, prefix: u8| {
        (name.to_owned(), address.parse::<IpAddr>().unwrap(), prefix)
    };
    let found = networks_from(
        &[
            interface("lo0", "127.0.0.1", 8),
            interface("lo0", "::1", 128),
            interface("en0", "fe80::1", 64),
            interface("utun4", "fd7a:115c:a1e0::1", 128),
            interface("en0", "2001:db8::5", 64),
            interface("utun4", "100.101.102.103", 32),
            interface("bridge0", "169.254.3.4", 16),
            interface("en5", "10.0.0.42", 24),
            interface("en0", "192.168.1.20", 24),
            interface("en0", "192.168.1.20", 24),
        ],
        Some(GATEWAY),
        Some("a0:b1:c2:d3:e4:f5".into()),
        &Tailscale::Addresses(vec![
            "100.101.102.103".parse().unwrap(),
            "fd7a:115c:a1e0::1".parse().unwrap(),
        ]),
    );
    let network = |name: &str, address: &str, subnet: &str, tailnet: bool| LocalNetwork {
        interface: name.into(),
        address: address.parse().unwrap(),
        subnet: subnet.parse().unwrap(),
        gateway: None,
        gateway_mac: None,
        tailnet,
    };
    assert_eq!(
        found,
        [
            LocalNetwork {
                gateway: Some(GATEWAY.into()),
                gateway_mac: Some("a0:b1:c2:d3:e4:f5".into()),
                ..network("en0", "192.168.1.20", "192.168.1.0/24", false)
            },
            network("en5", "10.0.0.42", "10.0.0.0/24", false),
            network("en0", "2001:db8::5", "2001:db8::/64", false),
            network("utun4", "100.101.102.103", "100.101.102.103/32", true),
            network("utun4", "fd7a:115c:a1e0::1", "fd7a:115c:a1e0::1/128", true),
        ]
    );
    // No default route: no gateway anywhere.
    assert!(
        networks_from(
            &[interface("en0", "192.168.1.20", 24)],
            None,
            None,
            &Tailscale::Unknown
        )[0]
        .gateway
        .is_none()
    );
}

fn ip(text: &str) -> IpAddr {
    text.parse().unwrap()
}

/// The fence, case by case: the owner on loopback; other computers only on
/// an address of a trusted network, from an allowed source (a Tailnet one
/// for a Tailscale address); everything else dropped before TLS.
#[test]
fn the_fence_opens_trusted_networks_to_allowed_sources_only() {
    let fence = Fence {
        open: vec![(ip("192.168.1.20"), false), (ip("100.101.102.103"), true)],
        observed: vec![ip("192.168.1.20"), ip("10.0.0.42"), ip("100.101.102.103")],
        sources: default_allowed_sources(),
        lans: vec![
            "192.168.1.0/24".parse().unwrap(),
            "10.0.0.0/24".parse().unwrap(),
        ],
    };
    for (local, source, door) in [
        // The owner, on loopback (IPv4, mapped into IPv6, IPv6).
        ("127.0.0.1", "127.0.0.1", Door::Owner),
        ("::ffff:127.0.0.1", "::ffff:127.0.0.1", Door::Owner),
        ("::1", "::1", Door::Owner),
        // A trusted network, from a private source (mapped or not).
        ("192.168.1.20", "192.168.1.30", Door::Peers),
        ("::ffff:192.168.1.20", "::ffff:192.168.1.30", Door::Peers),
        // From the public internet, even on a trusted network.
        ("192.168.1.20", "203.0.113.9", Door::Closed),
        // A network this computer is on but has not trusted.
        ("10.0.0.42", "10.0.0.7", Door::Closed),
        // Its Tailscale address: Tailnet sources only.
        ("100.101.102.103", "100.64.0.5", Door::Peers),
        ("100.101.102.103", "192.168.1.30", Door::Closed),
        // An address it does not know as one of its networks.
        ("203.0.113.1", "192.168.1.30", Door::Closed),
        ("fd7a:115c:a1e0::1", "fd7a:115c:a1e0::9", Door::Closed),
        // Loopback only from loopback.
        ("127.0.0.1", "192.168.1.30", Door::Closed),
    ] {
        assert_eq!(
            fence.door(ip(local), ip(source)),
            door,
            "{local} from {source}"
        );
    }
    // Narrowed sources narrow every trusted network.
    let narrow = Fence {
        sources: vec!["192.168.1.0/24".parse().unwrap()],
        ..fence.clone()
    };
    assert_eq!(
        narrow.door(ip("192.168.1.20"), ip("10.0.0.7")),
        Door::Closed
    );
    assert_eq!(
        narrow.door(ip("192.168.1.20"), ip("192.168.1.30")),
        Door::Peers
    );
    // Nothing trusted: only the owner gets in.
    let shut = Fence {
        open: vec![],
        ..fence.clone()
    };
    assert_eq!(
        shut.door(ip("192.168.1.20"), ip("192.168.1.30")),
        Door::Closed
    );
    assert_eq!(shut.door(ip("127.0.0.1"), ip("127.0.0.1")), Door::Owner);
    // Loopback stands in for a network only when it is one of the
    // computer's networks (tests): then it is fenced like one, and the
    // owner keeps 127.0.0.1.
    let standing_in = Fence {
        open: vec![(ip("::1"), false)],
        observed: vec![ip("::1")],
        sources: default_allowed_sources(),
        lans: vec![],
    };
    assert_eq!(standing_in.door(ip("::1"), ip("::1")), Door::Peers);
    assert_eq!(
        standing_in.door(ip("127.0.0.1"), ip("127.0.0.1")),
        Door::Owner
    );
    let untrusted_stand_in = Fence {
        open: vec![],
        ..standing_in
    };
    assert_eq!(untrusted_stand_in.door(ip("::1"), ip("::1")), Door::Closed);
    assert_eq!(Fence::default().door(ip("::1"), ip("::1")), Door::Owner);
    // On carrier-grade NAT Wi-Fi, a computer next door has an address in
    // Tailscale's range: it does not get in through the Tailscale address.
    let hotel = Fence {
        open: vec![(ip("100.101.102.103"), true)],
        observed: vec![ip("100.80.1.5"), ip("100.101.102.103")],
        sources: default_allowed_sources(),
        lans: vec!["100.80.0.0/16".parse().unwrap()],
    };
    assert_eq!(
        hotel.door(ip("100.101.102.103"), ip("100.80.7.7")),
        Door::Closed
    );
    assert_eq!(
        hotel.door(ip("100.101.102.103"), ip("100.64.0.5")),
        Door::Peers
    );
    assert_eq!(hotel.door(ip("100.80.1.5"), ip("100.80.7.7")), Door::Closed);
}

/// Hotels, campuses and carriers hand out carrier-grade NAT addresses
/// (100.64.0.0/10, Tailscale's range) on ordinary Wi-Fi: such a network is
/// not Tailscale, so the Tailscale entry never opens the port on it.
#[test]
fn a_carrier_grade_nat_wifi_is_not_tailscale() {
    for tailscale in [Tailscale::Unknown, Tailscale::Addresses(vec![])] {
        let found = networks_from(
            &[("en0".to_owned(), ip("100.80.1.5"), 16)],
            Some(Ipv4Addr::new(100, 80, 0, 1)),
            Some("a0:b1:c2:d3:e4:f5".into()),
            &tailscale,
        );
        assert_eq!(found.len(), 1);
        assert!(!found[0].tailnet, "{tailscale:?}: {found:?}");
        assert_eq!(found[0].gateway, Some(ip("100.80.0.1")));
    }
    // Tailscale's own answer decides; without one, only an interface named
    // for it counts (macOS gives every VPN a utun, so that is not enough).
    let ours = ip("100.101.102.103");
    let confirmed = Tailscale::Addresses(vec![ours]);
    assert!(is_tailscale_address("utun4", ours, &confirmed));
    assert!(!is_tailscale_address(
        "utun4",
        ip("100.101.102.104"),
        &confirmed
    ));
    assert!(!is_tailscale_address("utun4", ours, &Tailscale::Unknown));
    assert!(!is_tailscale_address(
        "en0",
        ip("100.80.1.5"),
        &Tailscale::Unknown
    ));
    assert!(is_tailscale_address(
        "tailscale0",
        ours,
        &Tailscale::Unknown
    ));
    assert!(is_tailscale_address("Tailscale", ours, &Tailscale::Unknown));
    assert!(!is_tailscale_address(
        "tailscale0",
        ours,
        &Tailscale::Addresses(vec![])
    ));
    assert!(!is_tailscale_address(
        "tailscale0",
        ip("192.168.1.20"),
        &Tailscale::Unknown
    ));
}

/// Tailscale runs here (`Running`) but cannot reach its coordination server
/// just now (`Self.Online` is false): connections with the computers it is
/// already in touch with go on, so its addresses are still Tailscale's and
/// the door on them stays open. Stopped, signed out or starting, none are.
#[test]
fn tailscale_running_without_its_server_still_has_its_addresses() {
    let status = |state: &str| {
        hmesh_transport::tailscale::parse_status(
            format!(
                r#"{{"BackendState":"{state}","Self":{{"Online":false,"TailscaleIPs":["100.101.102.103","fd7a:115c:a1e0::5"]}}}}"#
            )
            .as_bytes(),
        )
        .expect("status")
    };
    let running = Tailscale::from_status(&status("Running"));
    assert_eq!(
        running,
        Tailscale::Addresses(vec![ip("100.101.102.103"), ip("fd7a:115c:a1e0::5")])
    );
    for interface in ["tailscale0", "utun4", "Tailscale"] {
        assert!(
            is_tailscale_address(interface, ip("100.101.102.103"), &running),
            "{interface}"
        );
    }
    let found = networks_from(
        &[("tailscale0".to_owned(), ip("100.101.102.103"), 32)],
        None,
        None,
        &running,
    );
    assert!(found[0].tailnet, "{found:?}");
    for state in [
        "Stopped",
        "NeedsLogin",
        "NeedsMachineAuth",
        "Starting",
        "NoState",
    ] {
        let answer = Tailscale::from_status(&status(state));
        assert_eq!(answer, Tailscale::Addresses(vec![]), "{state}");
        assert!(!is_tailscale_address(
            "tailscale0",
            ip("100.101.102.103"),
            &answer
        ));
    }
}
