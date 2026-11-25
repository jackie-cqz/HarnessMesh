//! Address rules: Tailnet ranges, and the ranges a computer set up for
//! direct connections accepts them from unless told otherwise.
use ipnet::IpNet;
use std::net::IpAddr;

pub fn is_tailnet_ip(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(ip) => {
            let octets = ip.octets();
            octets[0] == 100 && (64..=127).contains(&octets[1])
        }
        IpAddr::V6(ip) => ip.segments()[..3] == [0xfd7a, 0x115c, 0xa1e0],
    }
}

/// Private networks (RFC 1918), carrier-grade NAT (used by Tailscale and
/// other overlays), IPv6 unique-local addresses and loopback: the sources a
/// direct node accepts when `allowed_sources` is not set.
pub fn default_allowed_sources() -> Vec<IpNet> {
    [
        "10.0.0.0/8",
        "172.16.0.0/12",
        "192.168.0.0/16",
        "100.64.0.0/10",
        "fc00::/7",
        "127.0.0.0/8",
        "::1/128",
    ]
    .into_iter()
    .filter_map(|net| net.parse().ok())
    .collect()
}

/// Whether an address lies in one of the default ranges: a local network,
/// an overlay network or this computer itself, not the public internet.
pub fn is_private_address(ip: IpAddr) -> bool {
    default_allowed_sources()
        .iter()
        .any(|net| net.contains(&ip))
}
