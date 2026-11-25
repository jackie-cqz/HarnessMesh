//! What networks this computer is on: its interface addresses with their
//! subnets, the default gateway and that gateway's hardware address. The
//! parsing is separate from asking the system, so every platform's output
//! is tested on any platform. Commands run by absolute path, with a
//! deadline; whatever cannot be read is left out (`None`), never guessed.
use hmesh_core::{IpNet, LocalNetwork, is_tailnet_ip, normalize_hardware_address};
use std::{
    net::{IpAddr, Ipv4Addr},
    time::Duration,
};

/// How long one system command (or file read) may take.
const COMMAND_DEADLINE: Duration = Duration::from_secs(2);

/// The default IPv4 gateway in `netstat -rn -f inet` output (macOS): the
/// first `default` route through an address (a VPN's `link#N` route has none).
pub fn parse_macos_default_gateway(netstat: &str) -> Option<Ipv4Addr> {
    netstat.lines().find_map(|line| {
        let mut fields = line.split_whitespace();
        (fields.next()? == "default")
            .then(|| fields.next()?.parse().ok())
            .flatten()
    })
}
/// The default IPv4 gateway in `/proc/net/route` (Linux): the route to
/// 0.0.0.0/0 that is up and has a gateway, the lowest metric first. The
/// kernel prints each address as a host-order word.
pub fn parse_linux_default_gateway(route: &str) -> Option<Ipv4Addr> {
    const UP: u32 = 0x1;
    const GATEWAY: u32 = 0x2;
    route
        .lines()
        .skip(1)
        .filter_map(|line| {
            let fields: Vec<&str> = line.split_whitespace().collect();
            let hex = |index: usize| u32::from_str_radix(fields.get(index)?, 16).ok();
            let (destination, gateway, flags, metric, mask) = (
                hex(1)?,
                hex(2)?,
                hex(3)?,
                fields.get(6)?.parse::<u32>().ok()?,
                hex(7)?,
            );
            (destination == 0
                && mask == 0
                && flags & (UP | GATEWAY) == UP | GATEWAY
                && gateway != 0)
                .then(|| (metric, Ipv4Addr::from(gateway.to_ne_bytes())))
        })
        .min_by_key(|(metric, _)| *metric)
        .map(|(_, gateway)| gateway)
}
/// The default IPv4 gateway in `ROUTE.EXE print -4` output (Windows): the
/// active route to 0.0.0.0/0 with the lowest metric. Rows are read by their
/// shape, not by the (translated) headings; persistent routes have no
/// interface column and are not active ones.
pub fn parse_windows_default_gateway(route: &str) -> Option<Ipv4Addr> {
    route
        .lines()
        .filter_map(|line| {
            let fields: Vec<&str> = line.split_whitespace().collect();
            if fields.len() != 5 || fields[0] != "0.0.0.0" || fields[1] != "0.0.0.0" {
                return None;
            }
            let gateway: Ipv4Addr = fields[2].parse().ok()?;
            fields[3].parse::<Ipv4Addr>().ok()?;
            Some((fields[4].parse::<u32>().ok()?, gateway))
        })
        .min_by_key(|(metric, _)| *metric)
        .map(|(_, gateway)| gateway)
}
/// `gateway`'s hardware address in `arp -n <gateway>` output (macOS):
/// `? (192.168.1.1) at a0:b1:c2:d3:e4:f5 on en0 ...`.
pub fn parse_macos_arp(arp: &str, gateway: Ipv4Addr) -> Option<String> {
    let wanted = format!("({gateway})");
    arp.lines().find_map(|line| {
        let fields: Vec<&str> = line.split_whitespace().collect();
        let at = fields.iter().position(|f| *f == wanted)?;
        (fields.get(at + 1) == Some(&"at"))
            .then(|| normalize_hardware_address(fields.get(at + 2)?))
            .flatten()
    })
}
/// `gateway`'s hardware address in `/proc/net/arp` (Linux).
pub fn parse_linux_arp(table: &str, gateway: Ipv4Addr) -> Option<String> {
    table.lines().skip(1).find_map(|line| {
        let fields: Vec<&str> = line.split_whitespace().collect();
        (fields.first()?.parse::<Ipv4Addr>().ok()? == gateway)
            .then(|| normalize_hardware_address(fields.get(3)?))
            .flatten()
    })
}
/// `gateway`'s hardware address in `ARP.EXE -a <gateway>` output (Windows).
pub fn parse_windows_arp(arp: &str, gateway: Ipv4Addr) -> Option<String> {
    arp.lines().find_map(|line| {
        let fields: Vec<&str> = line.split_whitespace().collect();
        (fields.first()?.parse::<Ipv4Addr>().ok()? == gateway)
            .then(|| normalize_hardware_address(fields.get(1)?))
            .flatten()
    })
}
/// The networks behind interface addresses `(interface, address, prefix)`:
/// those another computer could reach (not loopback, link-local and the
/// like; see `peer_address`), once each. The one the default gateway is on
/// comes first and carries it; Tailscale addresses are marked and last.
pub fn networks_from(
    interfaces: &[(String, IpAddr, u8)],
    gateway: Option<Ipv4Addr>,
    gateway_mac: Option<String>,
) -> Vec<LocalNetwork> {
    let mut found: Vec<LocalNetwork> = Vec::new();
    for (interface, address, prefix) in interfaces {
        let address = address.to_canonical();
        if address.is_loopback()
            || !hmesh_security::peer_address(address)
            || found.iter().any(|n| n.address == address)
        {
            continue;
        }
        let Ok(subnet) = IpNet::new(address, *prefix).map(|net| net.trunc()) else {
            continue;
        };
        let tailnet = is_tailnet_ip(address);
        let gateway = gateway
            .map(IpAddr::V4)
            .filter(|gateway| !tailnet && subnet.contains(gateway));
        found.push(LocalNetwork {
            interface: interface.clone(),
            address,
            subnet,
            gateway,
            gateway_mac: gateway.and(gateway_mac.clone()),
            tailnet,
        });
    }
    found.sort_by(|a, b| {
        let key = |n: &LocalNetwork| (n.gateway.is_none(), n.tailnet, n.address.is_ipv6());
        key(a)
            .cmp(&key(b))
            .then_with(|| a.interface.cmp(&b.interface))
            .then_with(|| a.address.cmp(&b.address))
    });
    found
}

/// The networks this computer is on now. Debug builds read them from the
/// JSON file `HMESH_TEST_NETWORKS` names instead, when it is set (read
/// again each time, so a test can move a computer between networks);
/// release builds have no such path.
pub async fn observe() -> Vec<LocalNetwork> {
    #[cfg(debug_assertions)]
    if let Some(path) = std::env::var_os("HMESH_TEST_NETWORKS") {
        return std::fs::read(&path)
            .ok()
            .and_then(|bytes| serde_json::from_slice(&bytes).ok())
            .unwrap_or_default();
    }
    let interfaces: Vec<(String, IpAddr, u8)> = if_addrs::get_if_addrs()
        .unwrap_or_default()
        .into_iter()
        .map(|interface| {
            let prefix = match &interface.addr {
                if_addrs::IfAddr::V4(v4) => v4.prefixlen,
                if_addrs::IfAddr::V6(v6) => v6.prefixlen,
            };
            (interface.name.clone(), interface.ip(), prefix)
        })
        .collect();
    let gateway = default_gateway().await;
    let gateway_mac = match gateway {
        Some(gateway) => hardware_address(gateway).await,
        None => None,
    };
    networks_from(&interfaces, gateway, gateway_mac)
}

async fn default_gateway() -> Option<Ipv4Addr> {
    if cfg!(target_os = "macos") {
        parse_macos_default_gateway(&run("/usr/sbin/netstat", &["-rn", "-f", "inet"]).await?)
    } else if cfg!(target_os = "linux") {
        parse_linux_default_gateway(&read("/proc/net/route").await?)
    } else if cfg!(windows) {
        parse_windows_default_gateway(
            &run(r"C:\Windows\System32\ROUTE.EXE", &["print", "-4"]).await?,
        )
    } else {
        None
    }
}
async fn hardware_address(gateway: Ipv4Addr) -> Option<String> {
    let address = gateway.to_string();
    if cfg!(target_os = "macos") {
        parse_macos_arp(&run("/usr/sbin/arp", &["-n", &address]).await?, gateway)
    } else if cfg!(target_os = "linux") {
        parse_linux_arp(&read("/proc/net/arp").await?, gateway)
    } else if cfg!(windows) {
        parse_windows_arp(
            &run(r"C:\Windows\System32\ARP.EXE", &["-a", &address]).await?,
            gateway,
        )
    } else {
        None
    }
}
/// A system command's output, if it finishes in time (killed otherwise).
/// Its exit status is not judged: `arp` exits 1 when it has no entry, and
/// the parsers find nothing in that output either.
async fn run(program: &str, args: &[&str]) -> Option<String> {
    let mut command = tokio::process::Command::new(program);
    command
        .args(args)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .kill_on_drop(true);
    #[cfg(windows)]
    command.creation_flags(0x08000000);
    let output = tokio::time::timeout(COMMAND_DEADLINE, command.output())
        .await
        .ok()?
        .ok()?;
    Some(String::from_utf8_lossy(&output.stdout).into_owned())
}
async fn read(path: &str) -> Option<String> {
    tokio::time::timeout(COMMAND_DEADLINE, tokio::fs::read_to_string(path))
        .await
        .ok()?
        .ok()
}
