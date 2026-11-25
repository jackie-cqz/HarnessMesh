//! Address rules: Tailnet ranges, the ranges a computer set up for direct
//! connections accepts them from unless told otherwise, and the networks a
//! computer listening on every interface opens its port to.
use ipnet::IpNet;
use serde::{Deserialize, Serialize};
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

/// Most networks one computer trusts.
pub const MAX_TRUSTED_NETWORKS: usize = 32;
/// Longest name of a trusted network, in bytes.
pub const MAX_NETWORK_NAME_BYTES: usize = 64;

/// A network a computer listening on every interface opens its port to:
/// its subnet and, when it was seen, its gateway's hardware address (a home
/// and a café may both be 192.168.1.0/24, but not behind the same router).
/// `tailnet` stands for this computer's Tailscale addresses instead, open to
/// Tailnet sources only.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TrustedNetwork {
    pub subnet: IpNet,
    /// The gateway's hardware address, lowercase and colon-separated.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gateway: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub tailnet: bool,
}
impl TrustedNetwork {
    /// The entry for this computer's Tailscale addresses.
    pub fn tailnet() -> Self {
        Self {
            subnet: IpNet::V4(
                ipnet::Ipv4Net::new(std::net::Ipv4Addr::new(100, 64, 0, 0), 10).unwrap_or_default(),
            ),
            gateway: None,
            name: Some("Tailscale".into()),
            tailnet: true,
        }
    }
    /// Whether this entry is `network`: `Some(gateway_unverified)` when it
    /// is. A Tailscale entry is any Tailscale address. Any other is the same
    /// subnet, behind the gateway it was trusted behind; a gateway that
    /// cannot be seen now (no hardware address in the ARP table) is not held
    /// against it, but the match is marked unverified.
    pub fn recognizes(&self, network: &LocalNetwork) -> Option<bool> {
        if self.tailnet || network.tailnet {
            return (self.tailnet && network.tailnet).then_some(false);
        }
        if self.subnet.trunc() != network.subnet.trunc() {
            return None;
        }
        match (&self.gateway, &network.gateway_mac) {
            (None, _) => Some(false),
            (Some(_), None) => Some(true),
            (Some(trusted), Some(seen)) => trusted.eq_ignore_ascii_case(seen).then_some(false),
        }
    }
    /// Trusted by its subnet alone: no gateway hardware address was seen
    /// when it was trusted, so any other network with the same subnet (the
    /// same 192.168.1.0/24 in a café) counts as this one.
    pub fn subnet_only(&self) -> bool {
        !self.tailnet && self.gateway.is_none()
    }
    /// The entry's own problems, if any.
    pub fn problem(&self) -> Option<&'static str> {
        if self
            .gateway
            .as_deref()
            .is_some_and(|g| normalize_hardware_address(g).as_deref() != Some(g))
        {
            return Some(
                "a trusted network's gateway is a hardware address like a0:b1:c2:d3:e4:f5",
            );
        }
        if self.tailnet && self.gateway.is_some() {
            return Some("the Tailscale entry has no gateway");
        }
        if self.name.as_deref().is_some_and(|name| {
            name.len() > MAX_NETWORK_NAME_BYTES || name.chars().any(char::is_control)
        }) {
            return Some(
                "a trusted network's name is at most 64 bytes, without control characters",
            );
        }
        None
    }
}

/// A network this computer is on now: one of its addresses, that address's
/// subnet and, on the network with the default route, the gateway and its
/// hardware address when the ARP table has it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LocalNetwork {
    pub interface: String,
    pub address: IpAddr,
    pub subnet: IpNet,
    #[serde(default)]
    pub gateway: Option<IpAddr>,
    #[serde(default)]
    pub gateway_mac: Option<String>,
    /// A Tailscale address.
    #[serde(default)]
    pub tailnet: bool,
}

/// How `trusted` recognizes `network`: which entry, and whether its gateway
/// could not be checked now. The best match wins: an entry whose gateway
/// checked out (or the Tailscale entry), then one trusted by its subnet
/// only, then one whose gateway cannot be seen now.
pub fn recognize(trusted: &[TrustedNetwork], network: &LocalNetwork) -> Option<(usize, bool)> {
    trusted
        .iter()
        .enumerate()
        .filter_map(|(index, entry)| {
            let unverified = entry.recognizes(network)?;
            let rank = match (unverified, entry.subnet_only()) {
                (false, false) => 0,
                (false, true) => 1,
                (true, _) => 2,
            };
            Some((rank, index, unverified))
        })
        .min_by_key(|(rank, index, _)| (*rank, *index))
        .map(|(_, index, unverified)| (index, unverified))
}

/// Trust the network this computer is on now with `subnet`: its gateway's
/// hardware address is taken from what was observed, never from the caller.
/// A Tailscale address's subnet trusts the Tailscale entry. An entry for
/// the same network is replaced (a new name). Returns the entry.
pub fn trust_network(
    trusted: &mut Vec<TrustedNetwork>,
    observed: &[LocalNetwork],
    subnet: IpNet,
    name: Option<String>,
) -> Result<TrustedNetwork, String> {
    let subnet = subnet.trunc();
    let network = observed
        .iter()
        .filter(|n| n.subnet.trunc() == subnet)
        .min_by_key(|n| (n.tailnet, n.gateway.is_none()))
        .ok_or_else(|| {
            format!(
                "{subnet} is not a network this computer is on now; it is on {}",
                if observed.is_empty() {
                    "none".to_owned()
                } else {
                    observed
                        .iter()
                        .map(|n| n.subnet.trunc().to_string())
                        .collect::<Vec<_>>()
                        .join(", ")
                }
            )
        })?;
    let mut entry = if network.tailnet {
        TrustedNetwork::tailnet()
    } else {
        TrustedNetwork {
            subnet,
            gateway: network.gateway_mac.clone(),
            name: None,
            tailnet: false,
        }
    };
    if name.is_some() {
        entry.name = name;
    }
    if let Some(problem) = entry.problem() {
        return Err(problem.into());
    }
    trusted.retain(|t| {
        !(t.tailnet == entry.tailnet
            && (entry.tailnet || (t.subnet.trunc() == subnet && t.gateway == entry.gateway)))
    });
    if trusted.len() >= MAX_TRUSTED_NETWORKS {
        return Err(format!(
            "this computer trusts at most {MAX_TRUSTED_NETWORKS} networks"
        ));
    }
    trusted.push(entry.clone());
    Ok(entry)
}

/// Stop trusting the networks with `subnet` (only the one behind `gateway`
/// when it is given; the Tailscale entry by its subnet, 100.64.0.0/10).
/// Returns how many entries were removed.
pub fn forget_network(
    trusted: &mut Vec<TrustedNetwork>,
    subnet: IpNet,
    gateway: Option<&str>,
) -> usize {
    let before = trusted.len();
    let subnet = subnet.trunc();
    trusted.retain(|t| {
        !(t.subnet.trunc() == subnet
            && gateway.is_none_or(|g| {
                t.gateway
                    .as_deref()
                    .is_some_and(|t| t.eq_ignore_ascii_case(g))
            }))
    });
    before - trusted.len()
}

/// A hardware address as `a0:b1:c2:d3:e4:f5`: six octets of one or two hex
/// digits (macOS drops leading zeros), separated by `:` or `-`. Not the
/// all-zero address an incomplete ARP entry shows, nor broadcast.
pub fn normalize_hardware_address(text: &str) -> Option<String> {
    let octets: Vec<&str> = text.trim().split([':', '-']).collect();
    if octets.len() != 6
        || !octets
            .iter()
            .all(|o| (1..=2).contains(&o.len()) && o.bytes().all(|b| b.is_ascii_hexdigit()))
    {
        return None;
    }
    let bytes: Vec<u8> = octets
        .iter()
        .filter_map(|o| u8::from_str_radix(o, 16).ok())
        .collect();
    if bytes.iter().all(|b| *b == 0) || bytes.iter().all(|b| *b == 0xff) {
        return None;
    }
    Some(
        bytes
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect::<Vec<_>>()
            .join(":"),
    )
}
