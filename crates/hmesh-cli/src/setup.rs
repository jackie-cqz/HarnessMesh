//! `hmesh setup`: set this computer up one of two ways.
//!
//! - **Only MCP**: this computer's own agents (Claude Code, Codex, ...) call
//!   agents on paired computers through `hmesh mcp`. The daemon listens on
//!   loopback only and accepts no connection from other computers.
//! - **Full**: it also listens for paired computers (on its Tailnet IP, or
//!   on a direct network on every interface, open only to the networks it
//!   trusts) and registers the ACP agents installed here, so paired
//!   computers can use them.
//!
//! Besides the node itself it only adds what the owner agrees to: a login
//! item for the daemon and HarnessMesh's entry in each MCP client. Nothing is
//! downloaded or installed, and no model is called.
use anyhow::{Context, Result, bail};
use hmesh_core::{
    AgentCard, AgentConfig, HarnessKind, IpNet, LocalNetwork, NodeConfig, TrustedNetwork,
};
use hmesh_security::{Instance, atomic_write};
use hmesh_transport::tailscale::{CliTailscale, TailscaleProvider};
use serde_json::{Value, json};
use std::{
    io::{BufRead, IsTerminal, Write},
    net::{IpAddr, Ipv4Addr},
    path::{Path, PathBuf},
    process::{Command, Stdio},
    time::Duration,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    McpOnly,
    Full,
}
impl Mode {
    fn name(self) -> &'static str {
        match self {
            Mode::McpOnly => "mcp-only",
            Mode::Full => "full",
        }
    }
}

pub struct Options {
    pub mode: Option<Mode>,
    pub name: Option<String>,
    pub port: u16,
    /// MCP clients to connect without asking (`none` for none); `None` asks.
    pub connect: Option<Vec<String>>,
    /// Start the daemon at login: yes (--startup), no (--no-startup), or
    /// ask (neither; nobody to ask means no).
    pub startup: Option<bool>,
    pub tailscale_executable: Option<PathBuf>,
    pub tailnet_address: Option<IpAddr>,
    /// `tailscale` or `direct`; `None` picks Tailscale when it is here.
    pub network: Option<String>,
    /// Direct network: trust the network this address is on (a loopback
    /// one listens there alone: tests, demos).
    pub listen: Option<IpAddr>,
    /// Direct network: also trust these subnets.
    pub trust: Vec<IpNet>,
    /// Direct network: also take connections over Tailscale (`None` asks
    /// when Tailscale runs here; nobody to ask means yes).
    pub with_tailscale: Option<bool>,
    /// Direct network: who may connect (default: private networks).
    pub allow_from: Option<Vec<hmesh_core::IpNet>>,
    pub project: Option<PathBuf>,
    pub agents: Option<Vec<String>>,
    /// The data directory was given explicitly, so MCP entries must name it.
    pub explicit_dir: bool,
}

/// Progress for a person (one line per step) and the same steps for `--json`.
struct Report {
    json: bool,
    steps: Vec<Value>,
}
impl Report {
    fn step(&mut self, done: bool, text: impl Into<String>) {
        let text = text.into();
        if !self.json {
            println!("{} {text}", if done { "✓" } else { "·" });
        }
        self.steps.push(json!({"done": done, "text": text}));
    }
}

pub async fn run(dir: &Path, options: Options, json_output: bool) -> Result<Value> {
    let interactive = std::io::stdin().is_terminal() && !json_output;
    // Check what can be checked before changing anything.
    if let Some(name) = options
        .connect
        .iter()
        .flatten()
        .find(|n| !matches!(n.as_str(), "claude" | "codex" | "none"))
    {
        bail!("unknown MCP client in --connect: {name} (claude, codex or none)");
    }
    let mode = match options.mode {
        Some(mode) => mode,
        None if interactive => ask_mode()?,
        None => bail!("choose how to set up this computer: --mcp-only or --full"),
    };
    let mut report = Report {
        json: json_output,
        steps: vec![],
    };

    // The network paired computers reach this one on. Run again without
    // naming one, a node keeps the network, address and sources it has.
    let existing = dir
        .join("node.toml")
        .exists()
        .then(|| Instance::load(dir))
        .transpose()?
        .map(|instance| instance.config);
    let (place, hostname) = find_place(mode, &options, interactive, existing.as_ref()).await?;
    let allow_from = options
        .allow_from
        .clone()
        .or_else(|| match (&existing, &place) {
            // Kept across a new address too (the fix for a DHCP change): a
            // narrowed range is never widened without being asked.
            (Some(config), Place::Direct(Some(_)) | Place::Networks(_))
                if config.node.network == "direct" =>
            {
                config.node.allowed_sources.clone()
            }
            _ => None,
        });
    let options = Options {
        allow_from,
        ..options
    };
    let accepted = match &options.allow_from {
        Some(sources) => sources
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join(", "),
        None => "private networks only".into(),
    };
    match &place {
        Place::Tailnet(address) => {
            report.step(true, format!("This computer is {address} on the Tailnet"))
        }
        Place::Networks(trusted) if trusted.is_empty() => report.step(
            false,
            "Listens on every network interface, but trusts no network yet, so no other computer can connect: trust one with `hmesh networks trust <subnet>` (`hmesh networks list` shows them) or in the panel (配对节点 → 信任这个网络)",
        ),
        Place::Networks(trusted) => {
            report.step(
                true,
                format!(
                    "Listens on every network interface; other computers connect over the networks it trusts ({}), from {accepted}. On any other network nobody can connect",
                    trusted.iter().map(describe_network).collect::<Vec<_>>().join(", ")
                ),
            );
            let subnet_only: Vec<String> = trusted
                .iter()
                .filter(|t| t.subnet_only())
                .map(|t| t.subnet.to_string())
                .collect();
            if !subnet_only.is_empty() {
                report.step(
                    false,
                    format!(
                        "Trusted by subnet only ({}): no gateway hardware address was seen, so any other network with the same subnet counts as trusted too. Trust it again there with `hmesh networks trust <subnet>` once its gateway can be seen",
                        subnet_only.join(", ")
                    ),
                );
            }
        }
        Place::Direct(Some(address)) => report.step(
            true,
            format!("Other computers on this network connect to {address}; it accepts them from {accepted}"),
        ),
        Place::Direct(None) => report.step(
            true,
            "Connects out over this computer's own network (no Tailscale needed)",
        ),
    }

    // The node: create it, or switch an existing one between the two ways
    // and networks. Peers know it by its certificate, so pairings stay.
    let target = node_config(mode, &options, &place, hostname.as_deref());
    // Only MCP accepts no connections, so it has no sources to keep.
    if let Some(sources) = existing
        .as_ref()
        .and_then(|config| config.node.allowed_sources.as_ref())
        .filter(|_| target.node.allowed_sources.is_none())
    {
        report.step(
            false,
            format!(
                "The sources it accepted ({}) are not kept: pass --allow-from again when it goes back to --full",
                sources
                    .iter()
                    .map(ToString::to_string)
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
        );
    }
    target.validate()?;
    if let Some(networks) = existing
        .as_ref()
        .and_then(|config| config.node.trusted_networks.as_ref())
        .filter(|networks| !networks.is_empty() && target.node.trusted_networks.is_none())
    {
        report.step(
            false,
            format!(
                "The networks it trusted ({}) are not kept: run setup with --full --network direct again to trust networks",
                networks
                    .iter()
                    .map(describe_network)
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
        );
    }
    let mut restart = false;
    if dir.join("node.toml").exists() {
        let mut instance = Instance::load(dir)?;
        if switch(&mut instance.config, &target)? {
            atomic_write(
                &dir.join("node.toml"),
                toml::to_string_pretty(&instance.config)?.as_bytes(),
            )?;
            restart = true;
            report.step(
                true,
                format!("Switched {} to {}", dir.display(), describe(mode)),
            );
        } else {
            report.step(
                true,
                format!(
                    "Node {} is already set up for {}",
                    dir.display(),
                    describe(mode)
                ),
            );
        }
    } else {
        Instance::init(dir, target)?;
        report.step(
            true,
            format!("Created node {} ({})", dir.display(), describe(mode)),
        );
    }
    let instance = Instance::load(dir)?;
    // `hmesh mcp` starts the daemon on demand for nodes set up here.
    atomic_write(
        &dir.join("setup.json"),
        &serde_json::to_vec_pretty(&json!({"mode": mode.name(), "start_daemon_on_demand": true}))?,
    )?;

    // Start at login (Mac, Windows), and now.
    let binary = std::env::current_exe()?;
    let mut startup = json!(null);
    if options.startup != Some(false) {
        match std::env::consts::OS {
            "macos" | "windows" => {
                let status = crate::startup::execute(dir, &crate::StartupCommand::Status)?;
                if status["installed"] == true {
                    report.step(true, "Already starts at login");
                } else if status["record_exists"] == true {
                    report.step(false, "A login item exists but was changed; inspect it with `hmesh startup status`");
                } else if consented(options.startup, interactive, || {
                    confirm("Start the HarnessMesh daemon when you log in?")
                })? {
                    startup = crate::startup::execute(dir, &crate::StartupCommand::Install)?;
                    report.step(true, "Starts at login from now on");
                } else {
                    report.step(false, "Does not start at login; to add it later: hmesh startup install (`hmesh mcp` starts the daemon when it is needed)");
                }
            }
            _ => report.step(false, "Login startup is not available on this system; `hmesh mcp` starts the daemon when it is needed"),
        }
    }
    let client = ensure_daemon(dir, &binary, restart).await?;
    report.step(
        true,
        if restart {
            "Restarted the daemon"
        } else {
            "The daemon is running"
        },
    );

    // Full: register the ACP agents installed here.
    let mut registered = vec![];
    if mode == Mode::Full {
        registered = register_agents(dir, &instance, &client, &options, &mut report).await?;
    }

    // Connect HarnessMesh's MCP server to the MCP clients found here.
    let mut command = vec![binary.display().to_string(), "mcp".into()];
    if options.explicit_dir {
        // Clients start it from anywhere: always an absolute directory.
        command.push("--data-dir".into());
        command.push(std::fs::canonicalize(dir)?.display().to_string());
    }
    let connected = connect_clients(
        &command,
        options.connect.as_deref(),
        interactive,
        &mut report,
    )?;

    if matches!(place, Place::Direct(_) | Place::Networks(_)) {
        direct_notes(mode, &mut report);
    }
    let next = match (mode, &place) {
        (Mode::McpOnly, _) => {
            "Next: run `hmesh panel`, open 配对节点 → 配对新电脑 and paste another computer's pairing code. This computer only connects out, so pairing starts here."
        }
        (Mode::Full, Place::Direct(_) | Place::Networks(_)) => {
            "Next: run `hmesh panel` and open 配对节点 → 配对新电脑: for 10 minutes, other computers can pair with the code it shows (`hmesh pair show` does the same)."
        }
        (Mode::Full, _) => {
            "Next: run `hmesh panel` to pair with other computers and talk to agents. Others can pair with this computer's code (`hmesh pair show`)."
        }
    };
    if !json_output {
        println!("{next}");
    }
    // The running daemon's card: on every interface it names the address
    // on a trusted network it found, not the configured `::`.
    let node = match client.get::<Value>("/local/status").await {
        Ok(status) if status["node"].is_object() => status["node"].clone(),
        _ => json!(instance.card),
    };
    Ok(json!({
        "mode": mode.name(),
        "data_dir": dir,
        "node": node,
        "network": instance.config.node.network,
        "tailnet_address": match place { Place::Tailnet(address) => Some(address), _ => None },
        "listen_address": match &place {
            Place::Direct(address) => *address,
            Place::Networks(_) => Some(IpAddr::V6(std::net::Ipv6Addr::UNSPECIFIED)),
            Place::Tailnet(address) => (mode == Mode::Full).then_some(*address),
        },
        "trusted_networks": instance.config.node.trusted_networks,
        "outbound_only": mode == Mode::McpOnly,
        "startup": startup,
        "registered_agents": registered,
        "mcp_clients": connected,
        "steps": report.steps,
        "next": next,
    }))
}

fn describe(mode: Mode) -> &'static str {
    match mode {
        Mode::McpOnly => "only MCP: calls agents on other computers, accepts no connections",
        Mode::Full => "full: also shares this computer's agents",
    }
}

fn ask(question: &str) -> Result<String> {
    eprint!("{question}");
    std::io::stderr().flush()?;
    let mut line = String::new();
    std::io::stdin().lock().read_line(&mut line)?;
    Ok(line.trim().to_owned())
}
fn ask_mode() -> Result<Mode> {
    eprintln!("How will this computer use HarnessMesh?");
    eprintln!(
        "  1) Only MCP: let Claude Code, Codex and other agents here call agents on other computers (nothing listens for other computers)"
    );
    eprintln!("  2) Full: also share this computer's agents with paired computers");
    loop {
        match ask("> ")?.as_str() {
            "1" => return Ok(Mode::McpOnly),
            "2" => return Ok(Mode::Full),
            _ => eprintln!("Type 1 or 2."),
        }
    }
}
fn confirm(question: &str) -> Result<bool> {
    Ok(matches!(
        ask(&format!("{question} [Y/n] "))?
            .to_ascii_lowercase()
            .as_str(),
        "" | "y" | "yes"
    ))
}

/// Where paired computers find this one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Place {
    /// On a Tailnet, at this computer's Tailnet IP.
    Tailnet(IpAddr),
    /// On a network this computer is on (a LAN, ZeroTier, WireGuard, ...):
    /// the one address others connect to (loopback: tests, demos), or none
    /// when it only connects out.
    Direct(Option<IpAddr>),
    /// A direct network, on every interface: open to these networks only.
    Networks(Vec<TrustedNetwork>),
}

/// The network to set this computer up for: as asked, or Tailscale when it
/// is here and this computer's own network otherwise.
async fn find_place(
    mode: Mode,
    options: &Options,
    interactive: bool,
    existing: Option<&NodeConfig>,
) -> Result<(Place, Option<String>)> {
    let wants_direct = options.listen.is_some() || options.allow_from.is_some();
    let kept = existing
        .map(|config| config.node.network.as_str())
        .filter(|network| matches!(*network, "tailscale" | "direct"));
    let network = match options.network.as_deref() {
        Some(network) => network.to_owned(),
        None if wants_direct => "direct".into(),
        None if options.tailnet_address.is_some() => "tailscale".into(),
        // Set up before: the same network again.
        None if kept.is_some() => kept.unwrap_or_default().to_owned(),
        None => {
            // Here and signed in: Tailscale. Installed but signed out or
            // stopped counts as not here.
            let tailnet = match CliTailscale::discover(options.tailscale_executable.as_deref()) {
                Ok(provider) => provider
                    .status()
                    .await
                    .ok()
                    .filter(|status| status.require_running().is_ok()),
                Err(_) => None,
            };
            match (tailnet, mode) {
                (Some(_), Mode::Full) if interactive && !ask_tailscale()? => "direct".into(),
                (Some(_), _) => "tailscale".into(),
                (None, Mode::McpOnly) => "direct".into(),
                (None, Mode::Full) if interactive => {
                    eprintln!(
                        "Tailscale is not installed or not signed in here, so other computers connect over this computer's own network."
                    );
                    "direct".into()
                }
                (None, Mode::Full) => bail!(
                    "Tailscale is not installed or not signed in here: sign in to it, or set this computer up for its own network with --network direct --listen <one of its addresses>"
                ),
            }
        }
    };
    if network == "tailscale" {
        if wants_direct {
            bail!("--listen and --allow-from are for --network direct");
        }
        if let Some(address) = options.tailnet_address {
            return Ok((Place::Tailnet(address), None));
        }
        // Tailscale's current IP; an only-MCP node set up before may keep
        // the one it has (it only goes into the certificate) when Tailscale
        // is not running now.
        let status = match CliTailscale::discover(options.tailscale_executable.as_deref()) {
            Ok(provider) => provider.status().await,
            Err(error) => Err(error),
        };
        let known = existing
            .filter(|config| config.node.network == "tailscale")
            .and_then(|config| config.node.tailnet_address);
        return Ok(
            match (
                status.and_then(|s| Ok((s.preferred_local_ip()?, s.self_node.hostname))),
                known,
            ) {
                (Ok((address, hostname)), _) => (Place::Tailnet(address), Some(hostname)),
                (Err(_), Some(address)) if mode == Mode::McpOnly => (Place::Tailnet(address), None),
                (Err(error), _) => {
                    return Err(error.context(
                    "Tailscale is needed for --network tailscale: install it, sign in and keep it running (or pass --tailscale-executable), or use this computer's own network with --network direct",
                ));
                }
            },
        );
    }
    if options.tailnet_address.is_some() {
        bail!("--tailnet-address is for --network tailscale");
    }
    if mode == Mode::McpOnly {
        if wants_direct {
            bail!(
                "an only-MCP computer accepts no connections: --listen and --allow-from are for --full"
            );
        }
        return Ok((Place::Direct(None), host_name()));
    }
    Ok((
        choose_networks(options, interactive, existing).await?,
        host_name(),
    ))
}

/// A full computer on a direct network listens on every interface, open to
/// the networks it trusts: the one `--listen` names an address of, those
/// `--trust` names, the ones it trusted before (one that listened on one
/// address before trusts that address's network now), or, set up for the
/// first time, the ones the owner picks (the network with the default route
/// offered first; nobody to ask, none). Tailscale is added when it runs here
/// and the owner agrees (or with --with-tailscale). Only a
/// loopback address listens there alone (one computer: tests, demos).
async fn choose_networks(
    options: &Options,
    interactive: bool,
    existing: Option<&NodeConfig>,
) -> Result<Place> {
    let observed = hmesh_transport::netinfo::observe(options.tailscale_executable.as_deref()).await;
    let listed = || {
        observed
            .iter()
            .map(|n| format!("{} ({})", n.address, n.interface))
            .collect::<Vec<_>>()
            .join(", ")
    };
    let kept =
        existing.filter(|config| config.node.network == "direct" && config.node.expose_to_tailnet);
    let before = kept
        .filter(|config| config.every_interface())
        .and_then(|config| config.node.trusted_networks.clone());
    let mut trusted = before.clone().unwrap_or_default();
    let alone = |address: IpAddr| -> Result<Place> {
        if !options.trust.is_empty() || options.with_tailscale == Some(true) {
            bail!(
                "--trust and --with-tailscale listen on every interface; a loopback --listen listens there alone"
            );
        }
        Ok(Place::Direct(Some(address)))
    };
    if let Some(address) = options.listen {
        match observed.iter().find(|n| n.address == address) {
            Some(network) => add(&mut trusted, trusted_entry(network)),
            None if address.is_loopback() => return alone(address),
            None => bail!(
                "{address} is not an address of this computer; its addresses are: {}",
                listed()
            ),
        }
    } else if let Some(config) = kept.filter(|config| !config.every_interface()) {
        // Set up before for one address: the network that address is on.
        let address = config.node.listen_address;
        match observed.iter().find(|n| n.address == address) {
            Some(network) => add(&mut trusted, trusted_entry(network)),
            None if address.is_loopback() && options.trust.is_empty() => return alone(address),
            // That address is gone (a new lease): as for a new computer
            // (asked; with nobody to ask, nothing).
            None if options.trust.is_empty() => {
                for entry in pick_networks(&observed, interactive)? {
                    add(&mut trusted, entry);
                }
            }
            None => {}
        }
    } else if before.is_none() && options.trust.is_empty() {
        for entry in pick_networks(&observed, interactive)? {
            add(&mut trusted, entry);
        }
    }
    for subnet in &options.trust {
        let subnet = subnet.trunc();
        let entry = observed
            .iter()
            .find(|n| !n.tailnet && n.subnet.trunc() == subnet)
            .map(trusted_entry)
            .unwrap_or(TrustedNetwork {
                subnet,
                gateway: None,
                name: None,
                tailnet: false,
            });
        add(&mut trusted, entry);
    }
    let tailscale_here = observed.iter().any(|n| n.tailnet);
    let with_tailscale = match options.with_tailscale {
        Some(choice) => choice,
        // Asked once, when it first listens on every interface; with
        // nobody to ask, not without --with-tailscale.
        None if before.is_none() && tailscale_here && interactive => {
            confirm("Tailscale runs here. Also accept connections from computers on your Tailnet?")?
        }
        None => trusted.iter().any(|t| t.tailnet),
    };
    trusted.retain(|t| !t.tailnet);
    if with_tailscale {
        trusted.push(TrustedNetwork::tailnet());
    }
    Ok(Place::Networks(trusted))
}
/// The entry that trusts `network`, behind the gateway seen now.
fn trusted_entry(network: &LocalNetwork) -> TrustedNetwork {
    if network.tailnet {
        return TrustedNetwork::tailnet();
    }
    TrustedNetwork {
        subnet: network.subnet.trunc(),
        gateway: network.gateway_mac.clone(),
        name: None,
        tailnet: false,
    }
}
fn add(trusted: &mut Vec<TrustedNetwork>, entry: TrustedNetwork) {
    if !trusted.iter().any(|t| {
        t.tailnet == entry.tailnet
            && t.subnet.trunc() == entry.subnet.trunc()
            && t.gateway == entry.gateway
    }) {
        trusted.push(entry);
    }
}
fn describe_network(network: &TrustedNetwork) -> String {
    if network.tailnet {
        return "Tailscale".into();
    }
    let only = if network.subnet_only() {
        ", by subnet only"
    } else {
        ""
    };
    match &network.name {
        Some(name) => format!("{} ({name}{only})", network.subnet),
        None if network.subnet_only() => format!("{} (by subnet only)", network.subnet),
        None => network.subnet.to_string(),
    }
}
/// The network offered first: the one with the default route, or the only one.
fn default_network(lans: &[&LocalNetwork]) -> Option<usize> {
    lans.iter()
        .position(|n| n.gateway.is_some())
        .or((lans.len() == 1).then_some(0))
}
/// The networks to trust on a computer set up for the first time, asked
/// (the one with the default route offered first). With nobody to ask, none.
fn pick_networks(observed: &[LocalNetwork], interactive: bool) -> Result<Vec<TrustedNetwork>> {
    let lans: Vec<&LocalNetwork> = observed.iter().filter(|n| !n.tailnet).collect();
    let default = default_network(&lans);
    // Nobody to ask: nothing is trusted that was not named (--trust, --listen).
    if !interactive || lans.is_empty() {
        return Ok(vec![]);
    }
    if let [only] = lans.as_slice() {
        return Ok(
            if confirm(&format!(
                "Trust the network this computer is on, {} ({}), so computers on it can connect?",
                only.subnet, only.interface
            ))? {
                vec![trusted_entry(only)]
            } else {
                vec![]
            },
        );
    }
    eprintln!(
        "Which networks do you trust? Computers on them can connect to this one; on any other network nobody can."
    );
    for (index, network) in lans.iter().enumerate() {
        eprintln!(
            "  {}) {}   {} ({}){}",
            index + 1,
            network.subnet,
            network.address,
            network.interface,
            if Some(index) == default {
                "   default route"
            } else {
                ""
            }
        );
    }
    let hint = default.map_or("none".to_owned(), |i| (i + 1).to_string());
    loop {
        let answer = ask(&format!("Numbers, separated by spaces [{hint}]: "))?;
        if answer.is_empty() {
            return Ok(default
                .map(|i| trusted_entry(lans[i]))
                .into_iter()
                .collect());
        }
        let picked: Option<Vec<TrustedNetwork>> = answer
            .split([' ', ','])
            .filter(|part| !part.is_empty())
            .map(|part| {
                part.parse::<usize>()
                    .ok()
                    .and_then(|n| lans.get(n.wrapping_sub(1)))
                    .map(|n| trusted_entry(n))
            })
            .collect();
        match picked {
            Some(picked) => return Ok(picked),
            None => eprintln!("Type numbers from the list."),
        }
    }
}

fn ask_tailscale() -> Result<bool> {
    eprintln!("How do other computers reach this one?");
    eprintln!("  1) Tailscale (recommended: works across networks)");
    eprintln!(
        "  2) This computer's own network: a LAN, ZeroTier, WireGuard (computers on it connect directly)"
    );
    loop {
        match ask("> ")?.as_str() {
            "1" | "" => return Ok(true),
            "2" => return Ok(false),
            _ => eprintln!("Type 1 or 2."),
        }
    }
}

/// This computer's host name, for the node name.
fn host_name() -> Option<String> {
    let output = Command::new("hostname")
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .ok()?;
    let name = String::from_utf8_lossy(&output.stdout)
        .lines()
        .next()?
        .trim()
        .split('.')
        .next()?
        .to_owned();
    (output.status.success() && !name.is_empty()).then_some(name)
}

/// What the owner may need to allow for a direct network.
fn direct_notes(mode: Mode, report: &mut Report) {
    if mode == Mode::Full && matches!(std::env::consts::OS, "macos" | "windows") {
        report.step(
            false,
            "Your firewall may ask whether hmesh may accept connections: allow it (on Windows, tick private networks only, not public ones)",
        );
    }
    if std::env::consts::OS == "macos" {
        report.step(
            false,
            "If the daemon starts at login and cannot reach the other computers, allow hmesh under System Settings → Privacy & Security → Local Network",
        );
    }
}

/// A new node's configuration for `mode` at `place`.
pub fn node_config(
    mode: Mode,
    options: &Options,
    place: &Place,
    hostname: Option<&str>,
) -> NodeConfig {
    let mut config = NodeConfig::default();
    config.node.name = options
        .name
        .clone()
        .or_else(|| hostname.map(node_name))
        .unwrap_or_else(|| "my-computer".into());
    config.node.listen_port = options.port;
    config.node.tailscale_executable = options.tailscale_executable.clone();
    let loopback = IpAddr::V4(Ipv4Addr::LOCALHOST);
    match place {
        Place::Tailnet(address) => {
            config.node.network = "tailscale".into();
            config.node.tailnet_address = Some(*address);
            let full = mode == Mode::Full;
            config.node.listen_address = if full { *address } else { loopback };
            config.node.expose_to_tailnet = full;
        }
        Place::Networks(trusted) if mode == Mode::Full => {
            config.node.network = "direct".into();
            config.node.listen_address = IpAddr::V6(std::net::Ipv6Addr::UNSPECIFIED);
            config.node.expose_to_tailnet = true;
            config.node.allowed_sources = options.allow_from.clone();
            config.node.trusted_networks = Some(trusted.clone());
        }
        Place::Networks(_) => {
            config.node.network = "direct".into();
            config.node.listen_address = loopback;
        }
        Place::Direct(address) => {
            config.node.network = "direct".into();
            match (mode, address) {
                (Mode::Full, Some(address)) => {
                    config.node.listen_address = *address;
                    config.node.expose_to_tailnet = true;
                    config.node.allowed_sources = options.allow_from.clone();
                }
                _ => config.node.listen_address = loopback,
            }
        }
    }
    config
}
/// A node name from a host name: letters, digits, `-`, `_` and `.` only.
fn node_name(hostname: &str) -> String {
    let name: String = hostname
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.') {
                c.to_ascii_lowercase()
            } else {
                '-'
            }
        })
        .take(64)
        .collect();
    let name = name.trim_matches(|c| matches!(c, '-' | '.')).to_owned();
    if name.is_empty() {
        "my-computer".into()
    } else {
        name
    }
}

/// Move an existing node to `target`'s way and network, keeping its name,
/// port and identity: peers know it by its pinned certificate, and the new
/// address reaches them with its card. True when anything changed.
pub fn switch(config: &mut NodeConfig, target: &NodeConfig) -> Result<bool> {
    if config.node.network == "loopback" {
        bail!("this node was set up for one computer only; run setup with a new --data-dir");
    }
    let before = serde_json::to_value(&config.node)?;
    config.node.network = target.node.network.clone();
    config.node.listen_address = target.node.listen_address;
    config.node.expose_to_tailnet = target.node.expose_to_tailnet;
    config.node.tailnet_address = target.node.tailnet_address;
    config.node.allowed_sources = target.node.allowed_sources.clone();
    config.node.trusted_networks = target.node.trusted_networks.clone();
    if target.node.tailscale_executable.is_some() {
        config.node.tailscale_executable = target.node.tailscale_executable.clone();
    }
    config.validate()?;
    Ok(serde_json::to_value(&config.node)? != before)
}

/// The daemon's owner client: started in the background if it is not
/// running, or restarted when its configuration changed.
async fn ensure_daemon(
    dir: &Path,
    binary: &Path,
    restart: bool,
) -> Result<hmesh_transport::Client> {
    match hmesh_daemon::running(dir).await {
        Some(client) if !restart => return Ok(client),
        Some(client) => {
            let _ = client.post::<_, Value>("/local/stop", &json!({})).await;
            let deadline = std::time::Instant::now() + Duration::from_secs(20);
            // It removes daemon.json first and lets go of its lock as it
            // exits: one started in between would find the lock taken.
            while (dir.join("daemon.json").exists() || !lock_free(dir))
                && std::time::Instant::now() < deadline
            {
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
        }
        None => {}
    }
    hmesh_daemon::start_detached(binary, dir, Duration::from_secs(20)).await
}
/// Whether no daemon holds `dir`'s lock (taken and let go at once here).
fn lock_free(dir: &Path) -> bool {
    std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(dir.join("daemon.lock"))
        .map_or(true, |lock| lock.try_lock().is_ok())
}

/// Register each ACP agent installed here that is not registered yet, named
/// `<node>-<kind>`, working in `--project` (default `<node dir>/project`).
async fn register_agents(
    dir: &Path,
    instance: &Instance,
    client: &hmesh_transport::Client,
    options: &Options,
    report: &mut Report,
) -> Result<Vec<String>> {
    let project = options
        .project
        .clone()
        .unwrap_or_else(|| dir.join("project"));
    std::fs::create_dir_all(&project)?;
    let project = std::fs::canonicalize(project)?;
    let installed = hmesh_runtime::agent_install::detect(&instance.dir);
    let wanted = options.agents.as_deref();
    for name in wanted.unwrap_or_default() {
        if !installed.iter().any(|a| a.harness.to_string() == *name) {
            bail!(
                "not installed on this computer: {name}; see hmesh agent detect for available ACP adapters"
            );
        }
    }
    let existing: Vec<AgentCard> = client.get("/hmesh/v1/agents").await?;
    let mut registered = vec![];
    for agent in installed
        .iter()
        .filter(|a| wanted.is_none_or(|w| w.iter().any(|n| *n == a.harness.to_string())))
    {
        let name = agent_name(&instance.card.display_name, agent.harness);
        if existing.iter().any(|card| card.name == name) {
            report.step(true, format!("Agent {name} is already registered"));
            continue;
        }
        let config: AgentConfig = serde_json::from_value(json!({
            "agent": {"name": name, "harness": agent.harness, "project_id": format!("{}-project", instance.card.display_name)},
            "project": {"root": project, "allowed_tools": ["read", "write", "edit"]},
            "adapter": adapter_for(agent),
        }))?;
        let card: AgentCard = client.post("/local/agents", &config).await?;
        report.step(
            true,
            format!("Registered agent {} ({})", card.name, agent.command),
        );
        registered.push(card.name);
    }
    if installed.is_empty() {
        report.step(false, "No ACP agent found on this computer; install one yourself (see `hmesh agent detect`) and run setup again");
    }
    Ok(registered)
}
/// How setup starts a detected agent: with the Node.js it was installed
/// with, when that is not the first one on PATH.
fn adapter_for(agent: &hmesh_core::DetectedAgent) -> Value {
    json!({"executable": agent.executable, "node_executable": agent.node})
}
/// `<node>-<kind>`, as scripts/start-tailnet.mjs names them. The official
/// DSH ACP registration keeps apart from earlier headless DSH agents.
fn agent_name(node: &str, kind: HarnessKind) -> String {
    format!("{node}-{}", kind.spec().setup_name.unwrap_or(kind.as_str()))
}

/// Add HarnessMesh's MCP server to Claude Code and Codex when they are found
/// here: asked one by one, or as `--connect` lists. Existing entries are left
/// alone.
fn connect_clients(
    command: &[String],
    wanted: Option<&[String]>,
    interactive: bool,
    report: &mut Report,
) -> Result<Vec<Value>> {
    let chosen = |client: &str, question: &str| {
        consented(
            wanted.map(|list| list.iter().any(|c| c == client)),
            interactive,
            || confirm(question),
        )
    };
    let mut results = vec![];
    // Claude Code: `claude mcp add -s user harnessmesh -- <hmesh> mcp`.
    match on_path("claude") {
        Some(claude) => {
            let present = Command::new(&claude)
                .args(["mcp", "get", "harnessmesh"])
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status()
                .is_ok_and(|s| s.success());
            if present {
                report.step(true, "Claude Code already has the HarnessMesh MCP server");
                results.push(json!({"client": "claude", "state": "already"}));
            } else if chosen(
                "claude",
                "Found Claude Code. Connect HarnessMesh's MCP server to it?",
            )? {
                // Its own output stays out of ours (and out of --json).
                let added = Command::new(&claude)
                    .args(["mcp", "add", "-s", "user", "harnessmesh", "--"])
                    .args(command)
                    .stdin(Stdio::null())
                    .output()?;
                if !added.status.success() {
                    bail!(
                        "`claude mcp add` failed ({}): {}",
                        added.status,
                        String::from_utf8_lossy(&added.stderr).trim()
                    );
                }
                report.step(
                    true,
                    "Connected Claude Code (claude mcp add -s user harnessmesh)",
                );
                results.push(json!({"client": "claude", "state": "connected"}));
            } else {
                report.step(false, format!("Claude Code not connected; to do it later: claude mcp add -s user harnessmesh -- {}", command.join(" ")));
                results.push(json!({"client": "claude", "state": "skipped"}));
            }
        }
        None => {
            report.step(false, "Claude Code not found here (no `claude` command)");
            results.push(json!({"client": "claude", "state": "not_found"}));
        }
    }
    // Codex: an [mcp_servers.harnessmesh] table in its config.toml.
    let codex_home = std::env::var_os("CODEX_HOME")
        .map(PathBuf::from)
        .or_else(|| home().map(|h| h.join(".codex")));
    match codex_home.filter(|home| home.exists() || on_path("codex").is_some()) {
        Some(codex_home) => {
            let path = codex_home.join("config.toml");
            let text = match std::fs::read_to_string(&path) {
                Ok(text) => text,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => String::new(),
                Err(error) => {
                    return Err(error).with_context(|| format!("read {}", path.display()));
                }
            };
            let parsed: toml::Table = toml::from_str(&text)
                .with_context(|| format!("{} is not valid TOML", path.display()))?;
            let present = parsed
                .get("mcp_servers")
                .and_then(|s| s.get("harnessmesh"))
                .is_some();
            if present {
                report.step(true, "Codex already has the HarnessMesh MCP server");
                results.push(json!({"client": "codex", "state": "already"}));
            } else if chosen(
                "codex",
                &format!(
                    "Found Codex. Add HarnessMesh's MCP server to {}?",
                    path.display()
                ),
            )? {
                let mut entry = toml::Table::new();
                entry.insert("command".into(), command[0].clone().into());
                entry.insert(
                    "args".into(),
                    command[1..]
                        .iter()
                        .cloned()
                        .map(toml::Value::from)
                        .collect::<Vec<_>>()
                        .into(),
                );
                // Codex waits 60 s per tool call by default; mesh_wait may wait longer.
                entry.insert("tool_timeout_sec".into(), toml::Value::Integer(660));
                let mut table = toml::Table::new();
                table.insert("harnessmesh".into(), entry.into());
                let mut servers = toml::Table::new();
                servers.insert("mcp_servers".into(), table.into());
                let mut updated = text.clone();
                if !updated.is_empty() && !updated.ends_with('\n') {
                    updated.push('\n');
                }
                if !updated.is_empty() {
                    updated.push('\n');
                }
                updated.push_str(&toml::to_string(&servers)?);
                toml::from_str::<toml::Table>(&updated)
                    .context("the updated Codex configuration would not be valid TOML")?;
                std::fs::create_dir_all(&codex_home)?;
                // A config.toml kept as a symlink (dotfiles) stays one: write
                // the file it points to.
                let target =
                    if std::fs::symlink_metadata(&path).is_ok_and(|m| m.file_type().is_symlink()) {
                        std::fs::canonicalize(&path)?
                    } else {
                        path.clone()
                    };
                atomic_write(&target, updated.as_bytes())?;
                report.step(true, format!("Connected Codex ({})", path.display()));
                results.push(json!({"client": "codex", "state": "connected", "config": path}));
            } else {
                report.step(
                    false,
                    format!(
                        "Codex not connected; to do it later add [mcp_servers.harnessmesh] to {}",
                        path.display()
                    ),
                );
                results.push(json!({"client": "codex", "state": "skipped"}));
            }
        }
        None => {
            report.step(
                false,
                "Codex not found here (no `codex` command or ~/.codex)",
            );
            results.push(json!({"client": "codex", "state": "not_found"}));
        }
    }
    Ok(results)
}

/// A flag answers in advance; otherwise ask when someone can answer, and
/// take no for an answer when nobody can.
fn consented(
    answer: Option<bool>,
    interactive: bool,
    ask: impl FnOnce() -> Result<bool>,
) -> Result<bool> {
    match answer {
        Some(answer) => Ok(answer),
        None if interactive => ask(),
        None => Ok(false),
    }
}

fn home() -> Option<PathBuf> {
    std::env::var_os(if cfg!(windows) { "USERPROFILE" } else { "HOME" }).map(PathBuf::from)
}
/// The first `name` on PATH (with `.cmd` / `.exe` on Windows).
fn on_path(name: &str) -> Option<PathBuf> {
    let suffixes: &[&str] = if cfg!(windows) {
        &[".cmd", ".exe", ""]
    } else {
        &[""]
    };
    std::env::split_paths(&std::env::var_os("PATH")?)
        .filter(|dir| dir.is_absolute())
        .flat_map(|dir| suffixes.iter().map(move |s| dir.join(format!("{name}{s}"))))
        .find(|path| path.is_file())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_detected_agent_is_registered_with_its_node() {
        let agent = hmesh_core::DetectedAgent {
            harness: HarnessKind::Dsh,
            command: "dsh".into(),
            executable: PathBuf::from("/runtimes/dsh/lib/bin.js"),
            node: Some(PathBuf::from("/nvm/24.22.1/bin/node")),
        };
        let adapter = adapter_for(&agent);
        assert_eq!(adapter["node_executable"], "/nvm/24.22.1/bin/node");
        let agent = hmesh_core::DetectedAgent {
            node: None,
            ..agent
        };
        assert!(adapter_for(&agent)["node_executable"].is_null());
    }

    fn options() -> Options {
        Options {
            mode: None,
            name: Some("studio".into()),
            port: 39880,
            connect: None,
            startup: Some(false),
            tailscale_executable: None,
            tailnet_address: None,
            network: None,
            listen: None,
            trust: vec![],
            with_tailscale: None,
            allow_from: None,
            project: None,
            agents: None,
            explicit_dir: false,
        }
    }

    #[test]
    fn both_ways_name_the_tailnet_ip_and_only_full_listens_on_it() {
        let ip: IpAddr = "100.64.0.7".parse().unwrap();
        let only = node_config(Mode::McpOnly, &options(), &Place::Tailnet(ip), None);
        only.validate().unwrap();
        assert!(only.outbound_only());
        assert!(only.node.listen_address.is_loopback());
        assert_eq!(only.node.tailnet_address, Some(ip));
        let full = node_config(Mode::Full, &options(), &Place::Tailnet(ip), None);
        full.validate().unwrap();
        assert!(!full.outbound_only());
        assert_eq!(full.node.listen_address, ip);
    }

    #[test]
    fn a_direct_network_listens_on_the_chosen_address_or_only_connects_out() {
        let lan: IpAddr = "192.168.1.20".parse().unwrap();
        let mut options = options();
        options.allow_from = Some(vec!["192.168.1.0/24".parse().unwrap()]);
        let full = node_config(Mode::Full, &options, &Place::Direct(Some(lan)), None);
        full.validate().unwrap();
        assert_eq!(full.node.network, "direct");
        assert_eq!(full.node.listen_address, lan);
        assert!(full.listens_for_peers());
        assert_eq!(full.allowed_sources(), options.allow_from.clone().unwrap());
        assert_eq!(full.node.tailnet_address, None);
        let only = node_config(Mode::McpOnly, &options, &Place::Direct(None), None);
        only.validate().unwrap();
        assert!(only.outbound_only());
        assert!(only.node.listen_address.is_loopback());
        assert_eq!(only.node.allowed_sources, None);
        // Every interface, open to the trusted networks; with the sources.
        let trusted = vec![TrustedNetwork::tailnet()];
        let every = node_config(
            Mode::Full,
            &options,
            &Place::Networks(trusted.clone()),
            None,
        );
        every.validate().unwrap();
        assert!(every.every_interface());
        assert!(every.node.listen_address.is_unspecified());
        assert_eq!(every.node.trusted_networks, Some(trusted));
        assert_eq!(every.allowed_sources(), options.allow_from.clone().unwrap());
    }

    #[test]
    fn switching_moves_a_node_between_ways_and_networks_and_keeps_its_name_and_port() {
        let ip: IpAddr = "100.64.0.7".parse().unwrap();
        let lan: IpAddr = "192.168.1.20".parse().unwrap();
        let at = |mode, place: Place| node_config(mode, &options(), &place, None);
        let mut config = at(Mode::McpOnly, Place::Tailnet(ip));
        config.node.listen_port = 41000;
        assert!(!switch(&mut config, &at(Mode::McpOnly, Place::Tailnet(ip))).unwrap());
        assert!(switch(&mut config, &at(Mode::Full, Place::Tailnet(ip))).unwrap());
        assert_eq!(config.node.listen_address, ip);
        // A new address, then another network: identity is the certificate.
        let moved: IpAddr = "100.64.0.8".parse().unwrap();
        assert!(switch(&mut config, &at(Mode::Full, Place::Tailnet(moved))).unwrap());
        assert_eq!(config.node.listen_address, moved);
        assert!(switch(&mut config, &at(Mode::Full, Place::Direct(Some(lan)))).unwrap());
        assert_eq!(config.node.network, "direct");
        assert_eq!(config.node.listen_address, lan);
        assert_eq!(config.node.tailnet_address, None);
        // To every interface, and back to one address.
        assert!(switch(&mut config, &at(Mode::Full, Place::Networks(vec![]))).unwrap());
        assert!(config.every_interface());
        assert_eq!(config.node.trusted_networks, Some(vec![]));
        assert!(switch(&mut config, &at(Mode::Full, Place::Direct(Some(lan)))).unwrap());
        assert_eq!(config.node.trusted_networks, None);
        assert!(switch(&mut config, &at(Mode::McpOnly, Place::Direct(None))).unwrap());
        assert!(config.outbound_only());
        assert_eq!(config.node.listen_port, 41000);
        assert_eq!(config.node.name, "studio");
        let mut local = NodeConfig::default();
        assert!(switch(&mut local, &at(Mode::Full, Place::Tailnet(ip))).is_err());
    }

    /// Offered first: the network with the default route, else the only one.
    #[test]
    fn the_network_offered_first_has_the_default_route() {
        let lan = |address: &str, gateway: Option<&str>| LocalNetwork {
            interface: "en0".into(),
            address: address.parse().unwrap(),
            subnet: format!("{address}/24").parse::<IpNet>().unwrap().trunc(),
            gateway: gateway.map(|g| g.parse().unwrap()),
            gateway_mac: None,
            tailnet: false,
        };
        let plain = lan("10.0.0.5", None);
        let routed = lan("192.168.1.20", Some("192.168.1.1"));
        assert_eq!(default_network(&[&plain, &routed]), Some(1));
        assert_eq!(default_network(&[&plain]), Some(0));
        assert_eq!(default_network(&[&plain, &plain]), None);
        assert_eq!(default_network(&[]), None);
        // Nobody to ask: nothing, whatever is offered.
        assert!(
            pick_networks(std::slice::from_ref(&routed), false)
                .unwrap()
                .is_empty()
        );
        assert!(pick_networks(&[], true).unwrap().is_empty());
    }

    #[test]
    fn nobody_to_ask_means_no_and_flags_answer_without_asking() {
        let never = || -> Result<bool> { panic!("asked") };
        assert!(!consented(None, false, never).unwrap());
        assert!(consented(Some(true), false, never).unwrap());
        assert!(!consented(Some(false), true, never).unwrap());
        assert!(consented(None, true, || Ok(true)).unwrap());
        assert!(!consented(None, true, || Ok(false)).unwrap());
    }

    #[test]
    fn dsh_keeps_its_acp_name_apart_from_earlier_headless_agents() {
        assert_eq!(agent_name("studio", HarnessKind::Dsh), "studio-dsh-acp");
        assert_eq!(
            agent_name("studio", HarnessKind::ClaudeCode),
            "studio-claude-code"
        );
    }

    #[test]
    fn host_names_become_node_names() {
        assert_eq!(node_name("Sam's MacBook Pro"), "sam-s-macbook-pro");
        assert_eq!(node_name("..."), "my-computer");
    }
}
