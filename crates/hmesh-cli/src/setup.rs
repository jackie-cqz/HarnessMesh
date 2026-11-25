//! `hmesh setup`: set this computer up one of two ways.
//!
//! - **Only MCP**: this computer's own agents (Claude Code, Codex, ...) call
//!   agents on paired computers through `hmesh mcp`. The daemon listens on
//!   loopback only and accepts no connection from other computers.
//! - **Full**: it also listens on its Tailnet IP and registers the ACP agents
//!   installed here, so paired computers can use them.
//!
//! Besides the node itself it only adds what the owner agrees to: a login
//! item for the daemon and HarnessMesh's entry in each MCP client. Nothing is
//! downloaded or installed, and no model is called.
use anyhow::{Context, Result, bail};
use hmesh_core::{AgentCard, AgentConfig, HarnessKind, NodeConfig, detect_installed_agents};
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
    pub startup: bool,
    pub tailscale_executable: Option<PathBuf>,
    pub tailnet_address: Option<IpAddr>,
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

    // This computer's Tailnet IP: the address paired computers know it by.
    let (address, hostname) = match options.tailnet_address {
        Some(address) => (address, None),
        None => {
            let status = CliTailscale::discover(options.tailscale_executable.as_deref())
                .context("Tailscale is needed to reach other computers; install it and sign in, or pass --tailscale-executable")?
                .status()
                .await?;
            (
                status.preferred_local_ip()?,
                Some(status.self_node.hostname),
            )
        }
    };
    report.step(true, format!("This computer is {address} on the Tailnet"));

    // The node: create it, or switch an existing one between the two ways.
    let target = node_config(mode, &options, address, hostname.as_deref());
    let mut restart = false;
    if dir.join("node.toml").exists() {
        let mut instance = Instance::load(dir)?;
        if switch(&mut instance.config, mode, address)? {
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
    if options.startup {
        match std::env::consts::OS {
            "macos" | "windows" => {
                let status = crate::startup::execute(dir, &crate::StartupCommand::Status)?;
                if status["installed"] == true {
                    report.step(true, "Already starts at login");
                } else if status["record_exists"] == true {
                    report.step(false, "A login item exists but was changed; inspect it with `hmesh startup status`");
                } else {
                    startup = crate::startup::execute(dir, &crate::StartupCommand::Install)?;
                    report.step(true, "Starts at login from now on");
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
        command.push("--data-dir".into());
        command.push(dir.display().to_string());
    }
    let connected = connect_clients(
        &command,
        options.connect.as_deref(),
        interactive,
        &mut report,
    )?;

    let next = match mode {
        Mode::McpOnly => {
            "Next: run `hmesh panel`, open 配对节点 → 配对新电脑 and paste another computer's pairing code. This computer only connects out, so pairing starts here."
        }
        Mode::Full => {
            "Next: run `hmesh panel` to pair with other computers and talk to agents. Others can pair with this computer's code (`hmesh pair show`)."
        }
    };
    if !json_output {
        println!("{next}");
    }
    Ok(json!({
        "mode": mode.name(),
        "data_dir": dir,
        "node": instance.card,
        "tailnet_address": address,
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

/// A new node's configuration for `mode`.
pub fn node_config(
    mode: Mode,
    options: &Options,
    address: IpAddr,
    hostname: Option<&str>,
) -> NodeConfig {
    let mut config = NodeConfig::default();
    config.node.name = options
        .name
        .clone()
        .or_else(|| hostname.map(node_name))
        .unwrap_or_else(|| "my-computer".into());
    config.node.listen_port = options.port;
    config.node.network = "tailscale".into();
    config.node.tailscale_executable = options.tailscale_executable.clone();
    config.node.tailnet_address = Some(address);
    match mode {
        Mode::McpOnly => {
            config.node.listen_address = IpAddr::V4(Ipv4Addr::LOCALHOST);
            config.node.expose_to_tailnet = false;
        }
        Mode::Full => {
            config.node.listen_address = address;
            config.node.expose_to_tailnet = true;
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

/// Change an existing node to `mode`. Its certificate already names its
/// Tailnet IP, so pairings stay valid; true when anything changed.
pub fn switch(config: &mut NodeConfig, mode: Mode, address: IpAddr) -> Result<bool> {
    if config.node.network != "tailscale" {
        bail!("this node was set up for one computer only; run setup with a new --data-dir");
    }
    let known = config
        .node
        .tailnet_address
        .unwrap_or(config.node.listen_address);
    if known != address {
        bail!(
            "this node's certificate is for Tailnet IP {known}, but this computer is {address} now; run setup with a new --data-dir and pair again"
        );
    }
    let changed = match mode {
        Mode::McpOnly if config.outbound_only() => false,
        Mode::Full if !config.outbound_only() => false,
        Mode::McpOnly => {
            config.node.listen_address = IpAddr::V4(Ipv4Addr::LOCALHOST);
            config.node.expose_to_tailnet = false;
            true
        }
        Mode::Full => {
            config.node.listen_address = address;
            config.node.expose_to_tailnet = true;
            true
        }
    };
    config.node.tailnet_address = Some(address);
    config.validate()?;
    Ok(changed)
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
            while dir.join("daemon.json").exists() && std::time::Instant::now() < deadline {
                tokio::time::sleep(Duration::from_millis(200)).await;
            }
        }
        None => {}
    }
    hmesh_daemon::start_detached(binary, dir, Duration::from_secs(20)).await
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
    let installed = detect_installed_agents();
    let wanted = options.agents.as_deref();
    for name in wanted.unwrap_or_default() {
        if !installed.iter().any(|a| kind_name(a.harness) == name) {
            bail!("not installed on this computer: {name} (HarnessMesh does not install agents)");
        }
    }
    let existing: Vec<AgentCard> = client.get("/hmesh/v1/agents").await?;
    let mut registered = vec![];
    for agent in installed
        .iter()
        .filter(|a| wanted.is_none_or(|w| w.iter().any(|n| n == kind_name(a.harness))))
    {
        let name = format!(
            "{}-{}",
            instance.card.display_name,
            kind_name(agent.harness)
        );
        if existing.iter().any(|card| card.name == name) {
            report.step(true, format!("Agent {name} is already registered"));
            continue;
        }
        let config: AgentConfig = serde_json::from_value(json!({
            "agent": {"name": name, "harness": agent.harness, "project_id": format!("{}-project", instance.card.display_name)},
            "project": {"root": project, "allowed_tools": ["read", "write", "edit"]},
            "adapter": {"executable": agent.executable},
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
fn kind_name(kind: HarnessKind) -> &'static str {
    match kind {
        HarnessKind::Opencode => "opencode",
        HarnessKind::ClaudeCode => "claude-code",
        HarnessKind::Codex => "codex",
        HarnessKind::Pi => "pi",
        _ => "acp",
    }
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
    let chosen = |client: &str, question: &str| -> Result<bool> {
        match wanted {
            Some(list) => Ok(list.iter().any(|c| c == client)),
            None if interactive => confirm(question),
            None => Ok(false),
        }
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
            let text = std::fs::read_to_string(&path).unwrap_or_default();
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
                atomic_write(&path, updated.as_bytes())?;
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

    fn options() -> Options {
        Options {
            mode: None,
            name: Some("studio".into()),
            port: 39880,
            connect: None,
            startup: false,
            tailscale_executable: None,
            tailnet_address: None,
            project: None,
            agents: None,
            explicit_dir: false,
        }
    }

    #[test]
    fn both_ways_name_the_tailnet_ip_and_only_full_listens_on_it() {
        let ip: IpAddr = "100.64.0.7".parse().unwrap();
        let only = node_config(Mode::McpOnly, &options(), ip, None);
        only.validate().unwrap();
        assert!(only.outbound_only());
        assert!(only.node.listen_address.is_loopback());
        assert_eq!(only.node.tailnet_address, Some(ip));
        let full = node_config(Mode::Full, &options(), ip, None);
        full.validate().unwrap();
        assert!(!full.outbound_only());
        assert_eq!(full.node.listen_address, ip);
    }

    #[test]
    fn switching_keeps_the_address_and_refuses_another_one() {
        let ip: IpAddr = "100.64.0.7".parse().unwrap();
        let mut config = node_config(Mode::McpOnly, &options(), ip, None);
        assert!(!switch(&mut config, Mode::McpOnly, ip).unwrap());
        assert!(switch(&mut config, Mode::Full, ip).unwrap());
        assert_eq!(config.node.listen_address, ip);
        assert!(switch(&mut config, Mode::McpOnly, ip).unwrap());
        assert!(config.outbound_only());
        assert!(switch(&mut config, Mode::Full, "100.64.0.8".parse().unwrap()).is_err());
        let mut local = NodeConfig::default();
        assert!(switch(&mut local, Mode::Full, ip).is_err());
    }

    #[test]
    fn host_names_become_node_names() {
        assert_eq!(node_name("Jackie's MacBook Pro"), "jackie-s-macbook-pro");
        assert_eq!(node_name("..."), "my-computer");
    }
}
