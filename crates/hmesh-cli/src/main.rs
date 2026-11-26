use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use hmesh_core::*;
use hmesh_daemon::{
    ArtifactGrant, PairApproval, PairTarget, RemoteTask, VerificationRequest, VerifyRequest,
    local_client,
};
use hmesh_security::Instance;
use hmesh_transport::tailscale::{CliTailscale, TailscaleProvider};
use serde_json::{Value, json};
use std::{fs, path::PathBuf, process::ExitCode};
use uuid::Uuid;
mod maintenance;
mod panel;
mod relay;
mod setup;
mod startup;
mod storage;

fn parse_provider_protocol(value: &str) -> serde_json::Result<hmesh_core::service::Protocol> {
    serde_json::from_value(json!(value))
}

#[derive(Parser)]
#[command(name = "hmesh", version, about = "Local-first coding harness mesh")]
struct Cli {
    #[arg(long, global = true)]
    data_dir: Option<PathBuf>,
    #[arg(long, global = true)]
    json: bool,
    #[command(subcommand)]
    command: Commands,
}
#[derive(Subcommand)]
enum Commands {
    Template {
        #[command(subcommand)]
        command: TemplateCommand,
    },
    Workflow {
        #[command(subcommand)]
        command: WorkflowCommand,
    },
    Usage,
    Budget {
        #[command(subcommand)]
        command: BudgetCommand,
    },
    Compatibility {
        #[arg(long)]
        agent_id: Option<Uuid>,
    },
    Storage {
        #[command(subcommand)]
        command: StorageCommand,
    },
    /// Private, offline Node backups. Never copies external Harness credential files.
    Backup {
        #[command(subcommand)]
        command: BackupCommand,
    },
    Upgrade {
        #[command(subcommand)]
        command: UpgradeCommand,
    },
    Startup {
        #[command(subcommand)]
        command: StartupCommand,
    },
    Workspace {
        #[command(subcommand)]
        command: WorkspaceCommand,
    },
    /// Serve the owner management panel on 127.0.0.1 only. No model is called at startup.
    Panel {
        #[arg(long, default_value_t = 38900)]
        port: u16,
        /// Initialize an empty local instance if needed and own a Daemon only when not running.
        #[arg(long)]
        start_daemon: bool,
    },
    /// Serve the HarnessMesh MCP tools over stdio for any agent (Claude Code, Codex, Pi, …).
    Mcp {
        /// Set by HarnessMesh for the agent sessions it runs.
        #[arg(long, hide = true)]
        session: Option<uuid::Uuid>,
    },
    Init {
        #[arg(long, default_value = "local-node")]
        name: String,
        #[arg(long, default_value_t = 38880)]
        port: u16,
        /// loopback (one computer), tailscale, or direct (a LAN, ZeroTier,
        /// WireGuard: any network this computer is on).
        #[arg(long, default_value = "loopback", value_parser = ["loopback", "tailscale", "direct"])]
        network: String,
        /// The address other computers connect to (a direct node that
        /// accepts connections names this or --trust). 0.0.0.0 or :: listens
        /// on every interface, open only to the --trust networks.
        #[arg(long)]
        listen_address: Option<std::net::IpAddr>,
        #[arg(long)]
        tailscale_executable: Option<PathBuf>,
        /// With --network tailscale or direct: reach paired computers but
        /// accept no connection from them (listen on loopback only).
        #[arg(long)]
        outbound_only: bool,
        /// This computer's Tailnet IP for --outbound-only; asked from
        /// Tailscale when omitted.
        #[arg(long, requires = "outbound_only")]
        tailnet_address: Option<std::net::IpAddr>,
        /// Direct network: the source ranges other computers may connect
        /// from (CIDR, comma-separated). Default: private, overlay and
        /// loopback networks; required for a public address.
        #[arg(long, value_delimiter = ',')]
        allow_from: Option<Vec<hmesh_core::IpNet>>,
        /// Direct network: listen on every interface and open the port to
        /// this subnet (repeat for more). The gateway is not recorded: the
        /// subnet is trusted wherever it is (`hmesh networks trust` records it).
        #[arg(long)]
        trust: Vec<hmesh_core::IpNet>,
    },
    /// Set this computer up: only MCP (its own agents call agents on other
    /// computers; it accepts no connections), or full (it also shares its
    /// agents). Adds a login item and MCP entries only with your consent.
    Setup {
        /// Only call agents on other computers from this one.
        #[arg(long, conflicts_with = "full")]
        mcp_only: bool,
        /// Also share this computer's agents with paired computers.
        #[arg(long)]
        full: bool,
        /// Node name; defaults to this computer's Tailscale host name.
        #[arg(long)]
        name: Option<String>,
        #[arg(long, default_value_t = hmesh_core::SETUP_PORT)]
        port: u16,
        /// MCP clients to connect without asking: claude, codex, or none.
        #[arg(long, value_delimiter = ',')]
        connect: Option<Vec<String>>,
        /// Start the daemon at login without asking (macOS, Windows).
        #[arg(long, conflicts_with = "no_startup")]
        startup: bool,
        /// Do not start the daemon at login, and do not ask.
        #[arg(long)]
        no_startup: bool,
        #[arg(long)]
        tailscale_executable: Option<PathBuf>,
        /// This computer's Tailnet IP; asked from Tailscale when omitted.
        #[arg(long)]
        tailnet_address: Option<std::net::IpAddr>,
        /// How other computers reach this one: tailscale, or direct (a LAN,
        /// ZeroTier, WireGuard: any network it is on). Default: the network
        /// a node set up before already uses; for a new one, Tailscale when
        /// it is signed in here.
        #[arg(long, value_parser = ["tailscale", "direct"])]
        network: Option<String>,
        /// Direct, full: trust the network this address of this computer is
        /// on (it listens on every interface, open to trusted networks). A
        /// loopback address listens there alone (tests, demos).
        #[arg(long)]
        listen: Option<std::net::IpAddr>,
        /// Direct, full: also trust this subnet (repeat for more).
        #[arg(long)]
        trust: Vec<hmesh_core::IpNet>,
        /// Direct, full: also take connections from computers on this
        /// computer's Tailnet, when Tailscale runs here (asked otherwise).
        #[arg(long, conflicts_with = "without_tailscale")]
        with_tailscale: bool,
        /// Direct, full: do not take connections over Tailscale.
        #[arg(long)]
        without_tailscale: bool,
        /// Direct, full: who may connect (CIDR, comma-separated). Default:
        /// private networks; required for a public address.
        #[arg(long, value_delimiter = ',')]
        allow_from: Option<Vec<hmesh_core::IpNet>>,
        /// Full: the project directory registered agents work in.
        #[arg(long)]
        project: Option<PathBuf>,
        /// Full: register only these installed agents (opencode, claude-code, codex, pi, dsh).
        #[arg(long, value_delimiter = ',')]
        agents: Option<Vec<String>>,
    },
    Doctor {
        #[arg(long)]
        peer: Option<Uuid>,
        #[arg(long)]
        report: bool,
        #[arg(long, requires = "report")]
        out: Option<PathBuf>,
    },
    Node {
        #[command(subcommand)]
        command: NodeCommand,
    },
    Daemon {
        #[command(subcommand)]
        command: DaemonCommand,
    },
    Pair {
        #[command(subcommand)]
        command: PairCommand,
    },
    Peers {
        #[command(subcommand)]
        command: PeersCommand,
    },
    /// The networks this computer is on, and the ones it opens its port to
    /// (direct network, listening on every interface).
    Networks {
        #[command(subcommand)]
        command: NetworksCommand,
    },
    /// A relay for computers that cannot reach each other: run one on a
    /// server both can reach (`serve`), and have a computer hold a
    /// connection to it (`use`) so the computers paired with it reach it
    /// through the relay. The relay passes their TLS on; it cannot read it.
    Relay {
        #[command(subcommand)]
        command: RelayCommand,
    },
    Agent {
        #[command(subcommand)]
        command: AgentCommand,
    },
    Agents {
        #[arg(value_parser = ["list"], default_value = "list")]
        action: String,
        #[arg(long, conflicts_with = "all")]
        peer: Option<Uuid>,
        /// Agents on this node and every paired node.
        #[arg(long)]
        all: bool,
    },
    Policy {
        #[arg(long)]
        file: PathBuf,
    },
    Task {
        #[command(subcommand)]
        command: TaskCommand,
    },
    /// Agent sessions on this and paired computers.
    Session {
        #[command(subcommand)]
        command: SessionCommand,
    },
    Service {
        #[command(subcommand)]
        command: ServiceCommand,
    },
    Tunnel {
        #[command(subcommand)]
        command: TunnelCommand,
    },
    Connection {
        #[command(subcommand)]
        command: ConnectionCommand,
    },
    Integration {
        #[command(subcommand)]
        command: IntegrationCommand,
    },
    Audit,
}
#[derive(Subcommand)]
enum TemplateCommand {
    List,
    Save {
        #[arg(long)]
        file: PathBuf,
    },
    Run {
        template_id: Uuid,
        #[arg(long)]
        revision: u32,
        #[arg(long)]
        goal: String,
        #[arg(long)]
        run_id: Option<Uuid>,
    },
}
#[derive(Subcommand)]
enum WorkflowCommand {
    List {
        #[arg(long)]
        archived: bool,
    },
    Archive {
        id: Uuid,
    },
    Start {
        #[arg(long)]
        file: PathBuf,
    },
    Show {
        id: Uuid,
    },
    Cancel {
        id: Uuid,
    },
    Accept {
        id: Uuid,
    },
}
#[derive(Subcommand)]
enum BudgetCommand {
    List,
    Set {
        #[arg(long)]
        file: PathBuf,
    },
}
#[derive(Subcommand)]
enum StorageCommand {
    Status,
    Plan {
        #[arg(long, default_value_t = 30)]
        older_than_days: u32,
        #[arg(long, default_value_t = 20)]
        keep_latest: usize,
        #[arg(long)]
        out: PathBuf,
    },
    Apply {
        #[arg(long)]
        plan: PathBuf,
        #[arg(long)]
        sha256: String,
    },
    Compact,
}
#[derive(Subcommand)]
enum BackupCommand {
    Create {
        #[arg(long)]
        out: PathBuf,
    },
    Check {
        #[arg(long)]
        from: PathBuf,
    },
    Restore {
        #[arg(long)]
        from: PathBuf,
    },
}
#[derive(Subcommand)]
enum UpgradeCommand {
    Check {
        #[arg(long)]
        bundle: PathBuf,
    },
    Prepare {
        #[arg(long)]
        bundle: PathBuf,
        #[arg(long)]
        backup_dir: PathBuf,
    },
}
#[derive(Subcommand)]
enum StartupCommand {
    Plan,
    Install,
    Uninstall,
    Status,
}
#[derive(Subcommand)]
enum WorkspaceCommand {
    /// Remove only an already-applied, unchanged worktree. The stored patch remains.
    Prune {
        task_id: Uuid,
        #[arg(long)]
        sha256: String,
    },
    Show {
        task_id: Uuid,
    },
    /// Requires an offline Daemon and the exact reviewed patch hash. Never creates a commit.
    Apply {
        task_id: Uuid,
        #[arg(long)]
        sha256: String,
    },
}
#[derive(Subcommand)]
enum ServiceCommand {
    Publish {
        #[arg(long)]
        config: PathBuf,
    },
    List {
        #[arg(long)]
        peer: Option<Uuid>,
    },
    Connect {
        #[arg(long)]
        config: PathBuf,
    },
    Revoke {
        id: Uuid,
    },
    Restart {
        id: Uuid,
    },
}
#[derive(Subcommand)]
enum TunnelCommand {
    Add {
        #[arg(long)]
        config: PathBuf,
    },
    List,
    Restart {
        id: Uuid,
    },
    Revoke {
        id: Uuid,
    },
}
#[derive(Subcommand)]
enum ConnectionCommand {
    List,
    Revoke { id: Uuid },
}
#[derive(Subcommand)]
enum IntegrationCommand {
    Start {
        #[arg(long)]
        config: PathBuf,
        #[arg(long)]
        allow_model: bool,
    },
    List,
    Show {
        id: Uuid,
    },
    Watch {
        id: Uuid,
    },
    Cancel {
        id: Uuid,
    },
}
#[derive(Subcommand)]
enum NodeCommand {
    Show,
}
#[derive(Subcommand)]
enum DaemonCommand {
    Start,
    Status,
    Stop,
}
#[derive(Subcommand)]
enum PeersCommand {
    List,
    Candidates {
        #[arg(long)]
        tailscale_executable: Option<PathBuf>,
    },
    /// Run this peer's tasks immediately (auto) or wait for local confirmation.
    Access {
        /// Peer node ID or display name.
        peer: String,
        #[arg(value_parser = ["auto", "confirm"])]
        mode: String,
    },
    /// Where else to reach a paired computer, kept only here: tried after
    /// the address on its card, before the ones it told this computer.
    Address {
        /// Peer node ID or display name.
        peer: String,
        /// Addresses as host:port or https://host:port (an IP address or a
        /// host name). They replace the ones set before.
        #[arg(required_unless_present = "clear", conflicts_with = "clear")]
        addresses: Vec<String>,
        /// Remove the addresses set here.
        #[arg(long)]
        clear: bool,
    },
}
#[derive(Subcommand)]
enum NetworksCommand {
    /// The networks this computer is on now, and which it trusts.
    List,
    /// Open the port to a network this computer is on now, by its subnet
    /// (as `hmesh networks list` shows it); its gateway is recorded too.
    Trust {
        subnet: hmesh_core::IpNet,
        #[arg(long)]
        name: Option<String>,
    },
    /// Close the port to a trusted network again.
    Forget {
        subnet: hmesh_core::IpNet,
        /// Only the entry behind this gateway hardware address.
        #[arg(long)]
        gateway: Option<String>,
    },
}
#[derive(Subcommand)]
enum RelayCommand {
    /// Run a relay here until stopped. The first run makes its key,
    /// certificate and token in --dir; its relay code is printed first.
    Serve {
        /// Where to listen, e.g. 0.0.0.0:7443 (open this TCP port to the
        /// computers that use it).
        #[arg(long)]
        listen: std::net::SocketAddr,
        /// The relay's own directory (not a computer's data directory).
        #[arg(long)]
        dir: PathBuf,
        /// The address computers reach the relay at, as <ip>:<port>, for
        /// its relay code (a cloud server's public IP).
        #[arg(long)]
        public_address: Option<String>,
    },
    /// Print the relay code of the relay in --dir again.
    Show {
        #[arg(long)]
        dir: PathBuf,
        /// Give the relay a new token first: every computer using the old
        /// one is cut off and needs the new code.
        #[arg(long)]
        rotate_token: bool,
        /// The address computers reach the relay at, as <ip>:<port>.
        #[arg(long)]
        public_address: Option<String>,
    },
    /// Hold a connection to a relay (its relay code, from `hmesh relay
    /// show` there): paired computers reach this one through it.
    Use {
        /// The relay code. Leave it out (or write -) to paste it when asked,
        /// or pipe it in: then it is read from standard input and stays out
        /// of the shell history and the process list. It is a secret.
        code: Option<String>,
    },
    /// Stop using every relay.
    Forget,
    /// The relays this computer uses, and whether it holds each now.
    List,
}
#[derive(Subcommand)]
enum PairCommand {
    /// This computer's card, with the pairing code to give another computer.
    Show,
    List,
    /// Ask another computer to pair. Once its owner approves, this computer
    /// trusts it too; there is nothing to approve here.
    Request {
        /// The other computer's pairing code (`hmesh pair show` there), or
        /// its relay pairing code (`hmesh-relay://...`) to pair through its
        /// relay. `--code` with nothing after it reads the code from
        /// standard input (asked for at a terminal): a relay pairing code
        /// holds the relay's token, which should stay out of the shell
        /// history and the process list.
        #[arg(long, num_args = 0..=1, default_missing_value = "-", conflicts_with_all = ["peer", "expect_fingerprint"], required_unless_present = "peer")]
        code: Option<String>,
        #[arg(long, requires = "expect_fingerprint")]
        peer: Option<String>,
        #[arg(long)]
        expect_fingerprint: Option<String>,
    },
    Approve {
        request_id: Uuid,
        #[arg(long)]
        expect_fingerprint: String,
    },
    /// Decline a request from another computer, or cancel one sent from here.
    Decline {
        request_id: Uuid,
    },
    /// Computers on this network with pairing open (direct network), for
    /// `hmesh pair join`.
    Nearby,
    /// Pair with a computer found nearby, or at a known address, using the
    /// 6-digit passcode its screen shows (`hmesh pair show` there). Both
    /// computers trust each other at once: nothing to approve.
    Join {
        /// Its address, as `hmesh pair nearby` lists it (192.168.1.20:39880),
        /// or an IP address or host name typed by hand (studio.local); without
        /// a port, the one `hmesh setup` uses (39880).
        address: String,
        /// The 6 digits. Omit it to be asked, so it stays out of the shell
        /// history; it is read from standard input then.
        #[arg(long)]
        passcode: Option<String>,
    },
    Revoke {
        peer_id: Uuid,
    },
}
#[derive(Subcommand)]
enum AgentCommand {
    /// List the ACP agent commands installed on this computer (nothing is run).
    Detect,
    /// Install and check a recommended local ACP adapter.
    Install {
        #[arg(value_parser = ["codex", "claude-code", "pi", "dsh"])]
        harness: String,
    },
    Retired,
    Retire {
        agent_id: Uuid,
    },
    Restore {
        agent_id: Uuid,
    },
    Summary {
        agent_id: Uuid,
    },
    Disable {
        agent_id: Uuid,
    },
    Update {
        agent_id: Uuid,
        #[arg(long)]
        config: PathBuf,
    },
    Register {
        #[arg(long)]
        config: PathBuf,
    },
    Verify {
        agent_id: Uuid,
        #[arg(long, required = true)]
        allow_model: bool,
        #[arg(
            long,
            default_value = "Reply exactly READY without invoking any tools."
        )]
        prompt: String,
        #[arg(long, default_value_t = 1048576)]
        max_output_bytes: usize,
        #[arg(long, default_value_t = 600)]
        timeout: u64,
    },
    Artifacts {
        agent_id: Uuid,
        #[arg(long = "path", required = true)]
        paths: Vec<PathBuf>,
    },
    /// Show or change how an agent reaches its model service. Keys stay on
    /// this computer and are never shown again.
    Service {
        agent_id: Uuid,
        /// agent_login, api_key or custom; with no options the current
        /// settings are shown.
        #[arg(long)]
        mode: Option<String>,
        /// Endpoint address for the custom mode.
        #[arg(long, conflicts_with = "preset")]
        base_url: Option<String>,
        /// Wire format for a custom endpoint: openai-chat, anthropic-messages or openai-responses.
        #[arg(long, value_parser = parse_provider_protocol)]
        protocol: Option<hmesh_core::service::Protocol>,
        /// Provider key variable that a generic ACP agent reads.
        #[arg(long)]
        key_variable: Option<String>,
        /// Use a known provider's address for the custom mode (the presets
        /// listed for this agent type).
        #[arg(long)]
        preset: Option<String>,
        /// First find out what the agent is set up with on its own: its own
        /// report or status command; no credential files are read.
        #[arg(long)]
        check: bool,
        /// Model new sessions start with; an empty value removes it.
        #[arg(long)]
        default_model: Option<String>,
        /// For OpenCode, Pi or DSH on a custom endpoint: offer this model (repeatable;
        /// the complete list). Without --model the stored list is kept.
        #[arg(long = "model")]
        models: Vec<String>,
        /// Remove the model list, keeping only the default model.
        #[arg(long, conflicts_with = "models")]
        clear_models: bool,
        /// After any change, check that the settings reach the model service:
        /// its model list is asked for first, and only if it cannot list them
        /// is one minimal request sent with the default model.
        #[arg(long)]
        test: bool,
        /// Read a new API key from standard input (one line).
        #[arg(long)]
        api_key_stdin: bool,
        /// Read the key from this variable of the daemon's environment when
        /// the agent starts, instead of storing it.
        #[arg(long, conflicts_with_all = ["api_key_stdin", "clear_api_key"])]
        api_key_env: Option<String>,
        /// Remove the stored API key.
        #[arg(long, conflicts_with = "api_key_stdin")]
        clear_api_key: bool,
        /// Give the agent this extra variable, taking its value from the
        /// variable of the same name in this command's environment so the value
        /// never appears on the command line (repeatable). With --keep-env it
        /// is the complete list.
        #[arg(long = "env")]
        env: Vec<String>,
        /// Keep the stored value of this extra variable (repeatable).
        #[arg(long = "keep-env")]
        keep_env: Vec<String>,
        /// Remove every extra variable.
        #[arg(long, conflicts_with_all = ["env", "keep_env"])]
        clear_env: bool,
    },
}
#[derive(Subcommand)]
enum SessionCommand {
    /// Sessions here and the ones this computer opened on paired computers.
    List,
    Show {
        session_id: Uuid,
    },
    /// Open a session; omit --peer for an agent on this computer.
    Open {
        #[arg(long)]
        agent: Uuid,
        #[arg(long)]
        peer: Option<Uuid>,
        #[arg(long)]
        message: Option<String>,
        /// Session setting SETTING=VALUE to start with, e.g. model=... (repeatable).
        #[arg(long = "set")]
        settings: Vec<String>,
    },
    /// Change a session setting (model, mode, ...) between turns.
    Config {
        session_id: Uuid,
        setting: String,
        /// A choice's value, or true/false for a switch.
        value: String,
    },
    /// Send the next message in a session.
    Send {
        session_id: Uuid,
        #[arg(long)]
        message: String,
    },
    /// Discover commands offered by this session, starting its agent if needed.
    Commands {
        session_id: Uuid,
    },
    /// Run a command advertised by this session, e.g. compact.
    Command {
        session_id: Uuid,
        name: String,
        #[arg(long, default_value = "")]
        arguments: String,
    },
    /// Read events after a sequence number (waits up to --wait seconds for new ones).
    Events {
        session_id: Uuid,
        #[arg(long, default_value_t = 0)]
        after: u64,
        #[arg(long, default_value_t = 0)]
        wait: u64,
    },
    /// Answer a permission request (key from the permission_request event).
    Permission {
        session_id: Uuid,
        key: String,
        #[arg(long, conflicts_with = "deny")]
        allow: bool,
        #[arg(long)]
        deny: bool,
    },
    Cancel {
        session_id: Uuid,
    },
    Close {
        session_id: Uuid,
    },
    /// Approve a session a paired computer opened while its access is confirm.
    Approve {
        session_id: Uuid,
    },
    Reject {
        session_id: Uuid,
    },
}
#[derive(Subcommand)]
enum TaskCommand {
    /// Inspect the original task against the execution owner's current configuration.
    Preflight {
        task_id: Uuid,
    },
    Archive {
        task_id: Uuid,
    },
    /// Recover a reserved submission using its exact original request and payload.
    Recover {
        request_id: Uuid,
    },
    List {
        #[arg(long)]
        pending: bool,
    },
    Send {
        agent_id: String,
        #[arg(long)]
        peer: String,
        #[arg(long)]
        project: String,
        #[arg(long)]
        prompt: String,
        #[arg(long)]
        request_id: Option<Uuid>,
        #[arg(long)]
        task_id: Option<Uuid>,
        #[arg(long, default_value_t = 600)]
        timeout: u64,
        #[arg(long, default_value_t = 1048576)]
        max_output_bytes: usize,
        /// Require this relative artifact to be declared before execution. Repeat for multiple files.
        #[arg(long = "artifact")]
        artifacts: Vec<PathBuf>,
    },
    Show {
        task_id: Uuid,
    },
    Watch {
        task_id: Uuid,
        #[arg(long, default_value_t = 0)]
        after: i64,
    },
    Approve {
        task_id: Uuid,
    },
    Reject {
        task_id: Uuid,
    },
    Cancel {
        task_id: Uuid,
    },
    Verify {
        task_id: Uuid,
        #[arg(long)]
        artifact: PathBuf,
        #[arg(long)]
        expect_json: String,
    },
    Verification {
        task_id: Uuid,
    },
    Events {
        task_id: Uuid,
        #[arg(long, default_value_t = 0)]
        after: i64,
    },
}

#[tokio::main]
async fn main() -> ExitCode {
    let cli = Cli::parse();
    let json_output = cli.json;
    let report_execution = matches!(
        &cli.command,
        Commands::Task {
            command: TaskCommand::Watch { .. } | TaskCommand::Verify { .. }
        } | Commands::Integration {
            command: IntegrationCommand::Watch { .. }
        }
    );
    let result = execute(cli).await;
    match result {
        Ok(value) => {
            if let Some(value) = value {
                if json_output {
                    println!("{value}");
                } else {
                    println!("{}", human_output(&value));
                }
                if report_execution {
                    if matches!(
                        value.get("state").and_then(Value::as_str),
                        Some("needs_input" | "cancelled")
                    ) {
                        return ExitCode::from(5);
                    }
                    if value.get("passed") == Some(&Value::Bool(false)) {
                        return ExitCode::from(5);
                    }
                    match value.get("status").and_then(Value::as_str) {
                        Some("UNKNOWN") => return ExitCode::from(6),
                        Some("REJECTED") => return ExitCode::from(3),
                        Some("FAILED" | "EXPIRED" | "CANCELLED") => return ExitCode::from(5),
                        _ => {}
                    }
                }
            }
            ExitCode::SUCCESS
        }
        Err(error) => {
            let mesh = error
                .downcast_ref::<MeshError>()
                .cloned()
                .unwrap_or_else(|| MeshError::new(ErrorCode::Internal, error.to_string()));
            eprintln!("{}", json!({"error":mesh}));
            ExitCode::from(match mesh.code {
                ErrorCode::InvalidInput | ErrorCode::UnsupportedVersion => 2,
                ErrorCode::Unauthenticated
                | ErrorCode::PermissionDenied
                | ErrorCode::ApprovalRequired => 3,
                ErrorCode::PeerUnreachable => 4,
                ErrorCode::TaskUnknown => 6,
                _ => 5,
            })
        }
    }
}

async fn execute(cli: Cli) -> Result<Option<Value>> {
    let explicit_dir = cli.data_dir.is_some();
    let dir = cli.data_dir.unwrap_or_else(|| {
        std::env::var_os(if cfg!(windows) { "USERPROFILE" } else { "HOME" })
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("."))
            .join(".harnessmesh")
    });
    if dir
        .components()
        .next()
        .is_some_and(|part| part.as_os_str() == "~")
    {
        return Err(MeshError::input(
            "--data-dir requires an expanded path; use an absolute path instead of ~",
        )
        .into());
    }
    if let Commands::Agent {
        command: AgentCommand::Detect,
    } = &cli.command
    {
        let found = hmesh_runtime::agent_install::detect(&dir);
        let missing: Vec<Value> = HarnessKind::KNOWN_ACP_AGENTS
        .into_iter()
        .filter(|h| !found.iter().any(|f| f.harness == *h))
        .map(|h| json!({"harness":h,"command":h.acp_command(),"install":h.acp_package().map(|p| format!("npm install -g {p}"))}))
        .collect();
        return Ok(Some(
            json!({"installed":found,"not_found":missing,"note":"Use hmesh agent install for Codex, Claude Code, Pi or DeepSeek Harness (dsh), or install the command yourself."}),
        ));
    }
    if let Commands::Agent {
        command: AgentCommand::Install { harness },
    } = &cli.command
    {
        let kind: HarnessKind = serde_json::from_value(json!(harness))?;
        let instance = Instance::load(&dir).context(
            "initialize this HarnessMesh node with hmesh init before installing an adapter",
        )?;
        let installed = hmesh_runtime::agent_install::install(&instance.dir, kind).await?;
        return Ok(Some(serde_json::to_value(installed)?));
    }
    if let Commands::Mcp { session } = &cli.command {
        // stdout carries only MCP frames from here on.
        hmesh_mcp::serve_stdio(&dir, *session).await?;
        return Ok(None);
    }
    if let Commands::Peers {
        command: PeersCommand::Candidates {
            tailscale_executable,
        },
    } = &cli.command
    {
        let configured = Instance::load(&dir)
            .ok()
            .and_then(|i| i.config.node.tailscale_executable);
        let status =
            CliTailscale::discover(tailscale_executable.as_deref().or(configured.as_deref()))?
                .status()
                .await?;
        return Ok(Some(serde_json::to_value(status)?));
    }
    if let Commands::Setup {
        mcp_only,
        full,
        name,
        port,
        connect,
        startup,
        no_startup,
        tailscale_executable,
        tailnet_address,
        network,
        listen,
        trust,
        with_tailscale,
        without_tailscale,
        allow_from,
        project,
        agents,
    } = cli.command
    {
        let options = setup::Options {
            mode: if mcp_only {
                Some(setup::Mode::McpOnly)
            } else if full {
                Some(setup::Mode::Full)
            } else {
                None
            },
            name,
            port,
            connect,
            startup: match (startup, no_startup) {
                (true, _) => Some(true),
                (_, true) => Some(false),
                _ => None,
            },
            tailscale_executable,
            tailnet_address,
            network,
            listen,
            trust,
            with_tailscale: match (with_tailscale, without_tailscale) {
                (true, _) => Some(true),
                (_, true) => Some(false),
                _ => None,
            },
            allow_from,
            project,
            agents,
            explicit_dir,
        };
        let summary = setup::run(&dir, options, cli.json).await?;
        return Ok(cli.json.then_some(summary));
    }
    if let Commands::Init {
        name,
        port,
        network,
        listen_address,
        tailscale_executable,
        outbound_only,
        tailnet_address,
        allow_from,
        trust,
    } = cli.command
    {
        let mut config = NodeConfig::default();
        config.node.name = name;
        config.node.listen_port = port;
        config.node.network = network.clone();
        config.node.tailscale_executable = tailscale_executable;
        if outbound_only && network == "loopback" {
            anyhow::bail!("--outbound-only needs --network tailscale or direct");
        }
        if allow_from.is_some() && (network != "direct" || outbound_only) {
            anyhow::bail!("--allow-from is for a direct node that accepts connections");
        }
        if !trust.is_empty() && (network != "direct" || outbound_only) {
            anyhow::bail!("--trust is for a direct node that accepts connections");
        }
        if network == "direct" {
            // Only connects out: loopback for its owner, nothing else.
            if !outbound_only {
                config.node.listen_address = match listen_address {
                    Some(address) => address,
                    None if !trust.is_empty() => std::net::Ipv6Addr::UNSPECIFIED.into(),
                    None => anyhow::bail!(
                        "a direct node needs --listen-address (the address of this computer that other computers connect to, or :: for every interface) or --trust (or --outbound-only)"
                    ),
                };
                config.node.expose_to_tailnet = true;
                config.node.allowed_sources = allow_from;
                if config.node.listen_address.is_unspecified() {
                    config.node.trusted_networks = Some(
                        trust
                            .into_iter()
                            .map(|subnet| hmesh_core::TrustedNetwork {
                                subnet: subnet.trunc(),
                                gateway: None,
                                name: None,
                                tailnet: false,
                            })
                            .collect(),
                    );
                } else if !trust.is_empty() {
                    anyhow::bail!(
                        "--trust listens on every interface: leave out --listen-address or make it :: or 0.0.0.0"
                    );
                }
            }
        } else if outbound_only {
            // Listen for the owner only; the Tailnet IP goes into the
            // certificate so the node can listen on it later.
            config.node.tailnet_address = Some(match tailnet_address {
                Some(address) => address,
                None => CliTailscale::discover(config.node.tailscale_executable.as_deref())?
                    .status()
                    .await?
                    .preferred_local_ip()?,
            });
            if let Some(address) = listen_address {
                config.node.listen_address = address;
            }
        } else if network == "tailscale" {
            let status = CliTailscale::discover(config.node.tailscale_executable.as_deref())?
                .status()
                .await?;
            config.node.listen_address = listen_address.unwrap_or(status.preferred_local_ip()?);
            status.require_local_ip(config.node.listen_address)?;
            config.node.expose_to_tailnet = true;
        } else if let Some(address) = listen_address {
            config.node.listen_address = address;
        }
        let instance = Instance::init(&dir, config)?;
        if instance.config.node.network != network {
            anyhow::bail!(
                "existing instance uses a different network; create a fresh --data-dir instead of changing identity/configuration"
            );
        }
        return Ok(Some(serde_json::to_value(instance.card)?));
    }
    if matches!(
        cli.command,
        Commands::Daemon {
            command: DaemonCommand::Start
        }
    ) {
        tracing_subscriber::fmt()
            .json()
            .with_writer(std::io::stderr)
            .with_env_filter("info")
            .try_init()
            .ok();
        hmesh_daemon::run(&dir).await?;
        return Ok(None);
    }
    if let Commands::Panel { port, start_daemon } = cli.command {
        panel::run(&dir, port, start_daemon, cli.json).await?;
        return Ok(None);
    }
    // A relay keeps its own directory: no computer is involved.
    match &cli.command {
        Commands::Relay {
            command:
                RelayCommand::Serve {
                    listen,
                    dir,
                    public_address,
                },
        } => {
            relay::serve(*listen, dir, public_address.clone(), cli.json).await?;
            return Ok(None);
        }
        Commands::Relay {
            command:
                RelayCommand::Show {
                    dir,
                    rotate_token,
                    public_address,
                },
        } => {
            return Ok(Some(relay::show(
                dir,
                *rotate_token,
                public_address.clone(),
            )?));
        }
        _ => {}
    }
    match &cli.command {
        Commands::Storage { command } => {
            return Ok(Some(match command {
                StorageCommand::Status => storage::status(&dir)?,
                StorageCommand::Plan {
                    older_than_days,
                    keep_latest,
                    out,
                } => storage::preview(&dir, *older_than_days, *keep_latest, out)?,
                StorageCommand::Apply { plan, sha256 } => storage::apply(&dir, plan, sha256)?,
                StorageCommand::Compact => storage::compact(&dir)?,
            }));
        }
        Commands::Backup { command } => {
            return Ok(Some(match command {
                BackupCommand::Create { out } => maintenance::create(&dir, out)?,
                BackupCommand::Check { from } => maintenance::check(from)?,
                BackupCommand::Restore { from } => maintenance::restore(&dir, from)?,
            }));
        }
        Commands::Upgrade { command } => {
            return Ok(Some(match command {
                UpgradeCommand::Check { bundle } => maintenance::bundle_check(bundle)?,
                UpgradeCommand::Prepare { bundle, backup_dir } => {
                    let bundle = maintenance::bundle_check(bundle)?;
                    let backup = maintenance::create(&dir, backup_dir)?;
                    if backup["schema"].as_i64() > bundle["database_schema"].as_i64() {
                        anyhow::bail!(
                            "candidate cannot open this Node schema; private backup was preserved"
                        );
                    }
                    json!({"bundle":bundle,"backup":backup,"binary_replaced":false,"next":"Start the verified bundle against this existing Node directory. Restore the matching backup before using an older schema binary."})
                }
            }));
        }
        Commands::Startup { command } => return Ok(Some(startup::execute(&dir, command)?)),
        Commands::Workspace { command } => {
            let instance = Instance::load(&dir)?;
            let _lock = instance
                .lock_daemon()
                .context("stop the Daemon before offline workspace review/apply")?;
            let store = hmesh_store::Store::open(&instance.dir.join("state.sqlite3"))?;
            return Ok(Some(match command {
                WorkspaceCommand::Prune { task_id, sha256 } => {
                    hmesh_runtime::workspace::prune(&store, *task_id, sha256, &instance.dir)?
                }
                WorkspaceCommand::Show { task_id } => store
                    .workspace(*task_id)?
                    .unwrap_or(json!({"state":"shared"})),
                WorkspaceCommand::Apply { task_id, sha256 } => hmesh_runtime::workspace::apply(
                    &store,
                    *task_id,
                    sha256,
                    &instance.dir.join("workspace-review"),
                )?,
            }));
        }
        _ => {}
    }
    let instance = Instance::load(&dir)?;
    if let Commands::Doctor { peer, report, out } = &cli.command {
        if *report {
            let value: Value = local_client(&instance)?.get("/local/diagnostics").await?;
            if let Some(path) = out {
                write_report(path, &value)?;
                return Ok(Some(
                    json!({"exported":path,"sensitive_text_included":false}),
                ));
            }
            return Ok(Some(value));
        }
        let daemon = match local_client(&instance) {
            Ok(client) => Some(match client.get::<Value>("/local/status").await {
                Ok(mut status) => {
                    // Doctor output is shared to get help: never the passcode,
                    // nor the hardware address of a network's gateway.
                    if let Some(passcode) = status.pointer_mut("/pairing_window/passcode")
                        && passcode.is_string()
                    {
                        *passcode = json!("hidden");
                    }
                    // Nor a relay's code: the relays are shown by where they are.
                    for relay in status
                        .pointer_mut("/relays")
                        .and_then(Value::as_array_mut)
                        .into_iter()
                        .flatten()
                    {
                        relay["code"] = json!("hidden");
                    }
                    for (list, field) in [("current", "gateway_mac"), ("trusted", "gateway")] {
                        for network in status
                            .pointer_mut(&format!("/networks/{list}"))
                            .and_then(Value::as_array_mut)
                            .into_iter()
                            .flatten()
                        {
                            if network[field].is_string() {
                                network[field] = json!("hidden");
                            }
                        }
                    }
                    json!({"state":"reachable", "status":status})
                }
                Err(error) => {
                    json!({"state":"unreachable", "error":error.to_string(), "hint":"Check daemon start and local owner credentials"})
                }
            }),
            Err(_) => None,
        };
        let tailscale = if instance.config.node.network == "tailscale" {
            match CliTailscale::discover(instance.config.node.tailscale_executable.as_deref()) {
                Ok(provider) => match provider.status().await {
                    Ok(status) => {
                        // Only-MCP nodes listen on loopback; their Tailnet IP is the one to check.
                        let address = instance
                            .config
                            .node
                            .tailnet_address
                            .filter(|_| instance.config.outbound_only())
                            .unwrap_or(instance.config.node.listen_address);
                        match status.require_local_ip(address) {
                            Ok(()) => serde_json::to_value(status)?,
                            Err(error) => {
                                json!({"state":"not_ready", "status":status, "error":error.to_string(), "hint":"Check coordination connectivity and local Tailnet IP; do not reset identity"})
                            }
                        }
                    }
                    Err(error) => json!({"error":error.to_string()}),
                },
                Err(error) => json!({"error":error.to_string()}),
            }
        } else {
            json!("not_required")
        };
        let peer_diagnostics = if let Some(peer) = peer {
            match local_client(&instance) {
                Ok(client) => match client.get::<Value>("/local/peers").await {
                    Ok(peers) => {
                        let entry = peers.as_array().and_then(|rows| {
                            rows.iter().find(|row| {
                                row["card"]["node_id"].as_str() == Some(&peer.to_string())
                            })
                        });
                        if let Some(entry) = entry {
                            let agents = match client
                                .get::<Value>(&format!("/local/agents/remote/{peer}"))
                                .await
                            {
                                Ok(agents) => {
                                    json!({"state":"reachable", "visible_agents":agents.as_array().map_or(0, Vec::len), "note":"An empty registry does not prove task authorization"})
                                }
                                Err(error) => {
                                    json!({"state":"rpc_failed", "error":error.to_string(), "hint":"Check peer listener, certificate trust and authorization separately"})
                                }
                            };
                            json!({"peer":entry, "registry":agents})
                        } else {
                            json!({"state":"not_paired", "hint":"Pair and compare full fingerprints first"})
                        }
                    }
                    Err(error) => json!({"state":"local_api_failed", "error":error.to_string()}),
                },
                Err(error) => json!({"state":"local_identity_failed", "error":error.to_string()}),
            }
        } else {
            json!("not_requested")
        };
        return Ok(Some(
            json!({"node_id":instance.card.node_id,"data_dir":instance.dir,"os":std::env::consts::OS,"arch":std::env::consts::ARCH,
            "protocol":PROTOCOL,"network":instance.config.node.network,"tailscale":tailscale,"identity":"loaded","daemon":daemon,"harness_execution":"not_checked","peer_diagnostics":peer_diagnostics}),
        ));
    }
    if matches!(
        cli.command,
        Commands::Node {
            command: NodeCommand::Show
        } | Commands::Pair {
            command: PairCommand::Show
        }
    ) {
        let card = fs::read(instance.dir.join("daemon.json"))
            .ok()
            .and_then(|bytes| serde_json::from_slice::<NodeCard>(&bytes).ok())
            .unwrap_or(instance.card.clone());
        let mut value = serde_json::to_value(&card)?;
        if matches!(
            cli.command,
            Commands::Pair {
                command: PairCommand::Show
            }
        ) {
            value["pair_code"] = json!(hmesh_security::pair_code(&card));
            // Through each relay it holds, too: for a computer that cannot
            // reach this one any other way.
            let relays = hmesh_daemon::configured_relays(&instance.config);
            let relayed = hmesh_daemon::takes_relayed(&instance.config) && !relays.is_empty();
            if relayed {
                value["relay_pair_codes"] = json!(
                    relays
                        .iter()
                        .map(|relay| hmesh_core::relay_pair_code(relay, &card.fingerprint))
                        .collect::<Vec<_>>()
                );
            }
            // A direct node answers the pairing handshake for ten minutes
            // after its owner asks for the code; so does one through a relay.
            if (instance.config.node.network == "direct" && !instance.config.outbound_only())
                || relayed
            {
                value["pairing_window"] = match local_client(&instance) {
                    Ok(client) => match client
                        .post::<_, Value>("/local/pairing/window", &json!({}))
                        .await
                    {
                        Ok(window) => window,
                        Err(error) => json!({"closes_at": null, "error": error.message}),
                    },
                    Err(_) => {
                        json!({"closes_at": null, "error": "the daemon is not running; start it, then run hmesh pair show again to open pairing for 10 minutes"})
                    }
                };
            }
        }
        return Ok(Some(value));
    }
    if let Commands::Relay { command } = cli.command {
        let change = match command {
            RelayCommand::Use { code } => relay::Change::Use(secret_line(
                code,
                "Relay code (hmesh relay show on the relay): ",
            )?),
            RelayCommand::Forget => relay::Change::Forget,
            RelayCommand::List => relay::Change::List,
            RelayCommand::Serve { .. } | RelayCommand::Show { .. } => {
                unreachable!("handled before loading a computer")
            }
        };
        return Ok(Some(relay::computer(&instance, change).await?));
    }
    if let Commands::Networks { command } = cli.command {
        return Ok(Some(networks(&instance, command).await?));
    }
    let client = local_client(&instance)?;
    let value: Value = match cli.command {
        Commands::Template { command } => match command {
            TemplateCommand::List => client.get("/local/templates").await?,
            TemplateCommand::Save { file } => {
                client
                    .post(
                        "/local/templates",
                        &read_spec::<hmesh_core::development::TaskTemplate>(&file)?,
                    )
                    .await?
            }
            TemplateCommand::Run {
                template_id,
                revision,
                goal,
                run_id,
            } => {
                client
                    .post(
                        "/local/templates/run",
                        &hmesh_daemon::TemplateRunRequest {
                            id: run_id.unwrap_or_else(Uuid::new_v4),
                            template_id,
                            revision,
                            goal,
                        },
                    )
                    .await?
            }
        },
        Commands::Workflow { command } => match command {
            WorkflowCommand::List { archived } => {
                client
                    .get(if archived {
                        "/local/workflows/archived"
                    } else {
                        "/local/workflows"
                    })
                    .await?
            }
            WorkflowCommand::Archive { id } => {
                client
                    .post(&format!("/local/workflows/{id}/archive"), &json!({}))
                    .await?
            }
            WorkflowCommand::Start { file } => {
                client
                    .post(
                        "/local/workflows",
                        &read_spec::<hmesh_core::development::WorkflowSpec>(&file)?,
                    )
                    .await?
            }
            WorkflowCommand::Show { id } => client.get(&format!("/local/workflows/{id}")).await?,
            WorkflowCommand::Cancel { id } => {
                client
                    .post(&format!("/local/workflows/{id}/cancel"), &json!({}))
                    .await?
            }
            WorkflowCommand::Accept { id } => {
                client
                    .post(&format!("/local/workflows/{id}/accept"), &json!({}))
                    .await?
            }
        },
        Commands::Usage => client.get("/local/usage").await?,
        Commands::Budget { command } => match command {
            BudgetCommand::List => client.get("/local/budgets").await?,
            BudgetCommand::Set { file } => {
                client
                    .post(
                        "/local/budgets",
                        &read_spec::<hmesh_core::development::ProjectBudget>(&file)?,
                    )
                    .await?
            }
        },
        Commands::Compatibility { agent_id } => {
            if let Some(agent_id) = agent_id {
                client
                    .post("/local/compatibility/probe", &json!({"agent_id":agent_id}))
                    .await?
            } else {
                client.get("/local/compatibility").await?
            }
        }
        Commands::Daemon {
            command: DaemonCommand::Status,
        } => client.get("/local/status").await?,
        Commands::Daemon {
            command: DaemonCommand::Stop,
        } => client.post("/local/stop", &json!({})).await?,
        Commands::Peers {
            command: PeersCommand::List,
        } => client.get("/local/peers").await?,
        Commands::Peers {
            command: PeersCommand::Access { peer, mode },
        } => {
            let peers: Vec<PeerView> = client.get("/local/peers").await?;
            let view = one_peer(&peers, &peer)?;
            let access = if mode == "confirm" {
                PeerAccess::Confirm
            } else {
                PeerAccess::Auto
            };
            client
                .post(
                    &format!("/local/peers/{}/access", view.peer.card.node_id),
                    &PeerAccessRequest { access },
                )
                .await?
        }
        Commands::Peers {
            command:
                PeersCommand::Address {
                    peer,
                    addresses,
                    clear: _,
                },
        } => {
            let peers: Vec<PeerView> = client.get("/local/peers").await?;
            let view = one_peer(&peers, &peer)?;
            let addresses = addresses
                .into_iter()
                .map(|address| {
                    if address.starts_with("https://") {
                        address
                    } else {
                        format!("https://{address}")
                    }
                })
                .collect();
            client
                .post(
                    &format!("/local/peers/{}/addresses", view.peer.card.node_id),
                    &hmesh_daemon::ManualAddresses { addresses },
                )
                .await?
        }
        Commands::Pair { command } => match command {
            PairCommand::List => client.get("/local/pairs").await?,
            PairCommand::Request {
                code,
                peer,
                expect_fingerprint,
            } => {
                let code = code
                    .map(|code| secret_line(Some(code), "Pairing code: "))
                    .transpose()?;
                let (endpoint, expect_fingerprint) = match (code, peer, expect_fingerprint) {
                    // Through the other computer's relay.
                    (Some(code), _, _) if hmesh_core::is_relay_address(&code) => {
                        let (relay, fingerprint) = hmesh_core::parse_relay_pair_code(&code)?;
                        (relay.to_string(), fingerprint)
                    }
                    (Some(code), _, _) => hmesh_security::parse_pair_code(&code)?,
                    (None, Some(peer), Some(fingerprint)) => (
                        if peer.starts_with("https://") {
                            peer
                        } else {
                            format!("https://{peer}")
                        },
                        fingerprint,
                    ),
                    _ => anyhow::bail!("pass --code, or --peer with --expect-fingerprint"),
                };
                client
                    .post(
                        "/local/pairs/request",
                        &PairTarget {
                            endpoint,
                            expected_fingerprint: expect_fingerprint,
                        },
                    )
                    .await?
            }
            PairCommand::Approve {
                request_id,
                expect_fingerprint,
            } => {
                client
                    .post(
                        "/local/pairs/approve",
                        &PairApproval {
                            request_id,
                            expected_fingerprint: expect_fingerprint,
                        },
                    )
                    .await?
            }
            PairCommand::Nearby => client.get("/local/pairing/nearby").await?,
            PairCommand::Join { address, passcode } => {
                let passcode = secret_line(passcode, "Passcode shown on the other computer: ")?;
                client
                    .post(
                        "/local/pairs/passcode",
                        &hmesh_daemon::passcode::PasscodeTarget {
                            endpoint: if address.starts_with("https://") {
                                address
                            } else {
                                format!("https://{address}")
                            },
                            passcode,
                        },
                    )
                    .await?
            }
            PairCommand::Decline { request_id } => {
                client
                    .post("/local/pairs/decline", &json!({"request_id": request_id}))
                    .await?
            }
            PairCommand::Revoke { peer_id } => {
                client
                    .post(&format!("/local/peers/{peer_id}/revoke"), &json!({}))
                    .await?
            }
            PairCommand::Show => unreachable!("handled before connecting"),
        },
        Commands::Agent {
            command: AgentCommand::Register { config },
        } => {
            let config: AgentConfig = toml::from_str(&fs::read_to_string(config)?)?;
            client.post("/local/agents", &config).await?
        }
        Commands::Agent {
            command:
                AgentCommand::Verify {
                    agent_id,
                    allow_model,
                    prompt,
                    max_output_bytes,
                    timeout,
                },
        } => {
            client
                .post(
                    &format!("/local/agents/{agent_id}/verify"),
                    &VerifyRequest {
                        allow_model,
                        prompt,
                        limits: RunLimits {
                            max_output_bytes,
                            timeout_seconds: timeout,
                        },
                    },
                )
                .await?
        }
        Commands::Agent {
            command: AgentCommand::Retired,
        } => client.get("/local/agents/retired").await?,
        Commands::Agent {
            command: AgentCommand::Retire { agent_id },
        } => {
            client
                .post(&format!("/local/agents/{agent_id}/retire"), &json!({}))
                .await?
        }
        Commands::Agent {
            command: AgentCommand::Restore { agent_id },
        } => {
            client
                .post(&format!("/local/agents/{agent_id}/restore"), &json!({}))
                .await?
        }
        Commands::Agent {
            command: AgentCommand::Summary { agent_id },
        } => {
            client
                .get(&format!("/local/agents/{agent_id}/summary"))
                .await?
        }
        Commands::Agent {
            command: AgentCommand::Disable { agent_id },
        } => {
            client
                .post(&format!("/local/agents/{agent_id}/disable"), &json!({}))
                .await?
        }
        Commands::Agent {
            command: AgentCommand::Update { agent_id, config },
        } => {
            let config: AgentConfig = toml::from_str(&fs::read_to_string(config)?)?;
            client
                .post(&format!("/local/agents/{agent_id}/update"), &config)
                .await?
        }
        Commands::Agent {
            command: AgentCommand::Artifacts { agent_id, paths },
        } => {
            client
                .post(
                    &format!("/local/agents/{agent_id}/artifacts"),
                    &ArtifactGrant { paths },
                )
                .await?
        }
        Commands::Agent {
            command:
                AgentCommand::Service {
                    agent_id,
                    mode,
                    base_url,
                    protocol,
                    key_variable,
                    preset,
                    check,
                    default_model,
                    models,
                    clear_models,
                    test,
                    api_key_stdin,
                    api_key_env,
                    clear_api_key,
                    env,
                    keep_env,
                    clear_env,
                },
        } => {
            let path = format!("/local/agents/{agent_id}/service");
            let current: hmesh_core::service::AgentServiceView = if check {
                client
                    .post(&format!("{path}/check?protocols=true"), &json!({}))
                    .await?
            } else {
                client.get(&format!("{path}?protocols=true")).await?
            };
            let selected_protocol = protocol.or(current.protocol);
            let base_url = match preset {
                Some(id) => Some(
                    current
                        .presets
                        .iter()
                        .find(|p| p.id == id)
                        .and_then(|p| match selected_protocol {
                            Some(format) => p.addresses.get(&format).cloned(),
                            None => Some(p.base_url.clone()),
                        })
                        .with_context(|| {
                            format!(
                                "preset {id:?} has no address this agent type can use; the usable presets are listed by `hmesh agent service {agent_id}`"
                            )
                        })?,
                ),
                None => base_url,
            };
            let changing = mode.is_some()
                || protocol.is_some()
                || key_variable.is_some()
                || base_url.is_some()
                || api_key_env.is_some()
                || default_model.is_some()
                || !models.is_empty()
                || clear_models
                || api_key_stdin
                || clear_api_key
                || !env.is_empty()
                || !keep_env.is_empty()
                || clear_env;
            let service = if !changing {
                current
            } else {
                let api_key = if clear_api_key {
                    Some(String::new())
                } else if api_key_stdin {
                    let mut line = String::new();
                    std::io::stdin().read_line(&mut line)?;
                    Some(line.trim().to_owned())
                } else {
                    None
                };
                // Without --env/--keep-env the stored variables stay as they are.
                let env: serde_json::Map<String, Value> = if clear_env {
                    serde_json::Map::new()
                } else if env.is_empty() && keep_env.is_empty() {
                    current
                        .env
                        .iter()
                        .map(|e| (e.name.clone(), Value::Null))
                        .collect()
                } else {
                    let mut map: serde_json::Map<String, Value> = keep_env
                        .into_iter()
                        .map(|name| (name, Value::Null))
                        .collect();
                    for name in env {
                        anyhow::ensure!(
                            !name.contains('='),
                            "pass the value through the environment instead: NAME=value hmesh agent service ... --env NAME"
                        );
                        let value = std::env::var(&name).with_context(|| {
                            format!("{name} is not set in this command's environment")
                        })?;
                        map.insert(name, json!(value));
                    }
                    map
                };
                let mode = mode.unwrap_or(current.mode);
                // A stored model list only carries over while the endpoint stays custom.
                let models = if clear_models {
                    vec![]
                } else if !models.is_empty() {
                    models
                } else if mode == hmesh_core::service::MODE_CUSTOM {
                    current.models
                } else {
                    vec![]
                };
                let mut body = json!({
                    "mode":mode,
                    "base_url":base_url.or(current.base_url),
                    "default_model":default_model.or(current.default_model),
                    "models":models,
                    "api_key":api_key,
                    "env":env,
                });
                if let Some(protocol) = protocol {
                    body["protocol"] = json!(protocol);
                }
                if let Some(name) = key_variable {
                    body["key_variable"] = json!(name);
                }
                // Sent only when given, so daemons without it accept the request.
                if let Some(name) = api_key_env {
                    body["api_key_env"] = json!(name);
                }
                client
                    .post::<_, hmesh_core::service::AgentServiceView>(
                        &format!("{path}?protocols=true"),
                        &body,
                    )
                    .await?
            };
            if test {
                let result: Value = client
                    .post(
                        &format!("{path}/test"),
                        &json!({
                            "mode":service.mode,
                            "base_url":service.base_url,
                            "model":service.default_model,
                        }),
                    )
                    .await?;
                json!({"service":service,"test":result})
            } else {
                serde_json::to_value(service)?
            }
        }
        Commands::Agents { all: true, .. } => client.get("/local/mesh/agents").await?,
        Commands::Agents { peer, .. } => {
            client
                .get(
                    &peer
                        .map(|id| format!("/local/agents/remote/{id}"))
                        .unwrap_or_else(|| "/hmesh/v1/agents".into()),
                )
                .await?
        }
        Commands::Policy { file } => {
            let policy: Policy = toml::from_str(&fs::read_to_string(file)?)?;
            client.post("/local/policy", &policy).await?
        }
        Commands::Session { command } => match command {
            SessionCommand::List => client.get("/local/sessions").await?,
            SessionCommand::Show { session_id } => {
                client.get(&format!("/local/sessions/{session_id}")).await?
            }
            SessionCommand::Open {
                agent,
                peer,
                message,
                settings,
            } => {
                let mut config = serde_json::Map::new();
                for setting in settings {
                    let (name, value) = setting
                        .split_once('=')
                        .context("--set takes SETTING=VALUE")?;
                    config.insert(name.to_owned(), setting_value(value));
                }
                client
                    .post(
                        "/local/sessions",
                        &json!({"agent_id":agent,"peer_id":peer,"message":message,"config":config}),
                    )
                    .await?
            }
            SessionCommand::Config {
                session_id,
                setting,
                value,
            } => {
                client
                    .post(
                        &format!("/local/sessions/{session_id}/config"),
                        &json!({"config_id":setting,"value":setting_value(&value)}),
                    )
                    .await?
            }
            SessionCommand::Send {
                session_id,
                message,
            } => {
                client
                    .post(
                        &format!("/local/sessions/{session_id}/prompt"),
                        &json!({"message":message}),
                    )
                    .await?
            }
            SessionCommand::Commands { session_id } => {
                client
                    .post_with_timeout(
                        &format!("/local/sessions/{session_id}/commands"),
                        &json!({}),
                        40,
                    )
                    .await?
            }
            SessionCommand::Command {
                session_id,
                name,
                arguments,
            } => {
                client
                    .post(
                        &format!("/local/sessions/{session_id}/command"),
                        &hmesh_core::sessions::RunSessionCommand {
                            name: name.strip_prefix('/').unwrap_or(&name).to_owned(),
                            arguments,
                        },
                    )
                    .await?
            }
            SessionCommand::Events {
                session_id,
                after,
                wait,
            } => {
                client
                    .get(&format!(
                        "/local/sessions/{session_id}/events?after={after}&wait={}",
                        wait.min(10)
                    ))
                    .await?
            }
            SessionCommand::Permission {
                session_id,
                key,
                allow,
                deny,
            } => {
                if allow == deny {
                    return Err(MeshError::input("pass exactly one of --allow or --deny").into());
                }
                client
                    .post(
                        &format!("/local/sessions/{session_id}/permissions/{key}"),
                        &json!({"allow":allow}),
                    )
                    .await?
            }
            SessionCommand::Cancel { session_id } => {
                client
                    .post(&format!("/local/sessions/{session_id}/cancel"), &json!({}))
                    .await?
            }
            SessionCommand::Close { session_id } => {
                client
                    .post(&format!("/local/sessions/{session_id}/close"), &json!({}))
                    .await?
            }
            SessionCommand::Approve { session_id } => {
                client
                    .post(&format!("/local/sessions/{session_id}/approve"), &json!({}))
                    .await?
            }
            SessionCommand::Reject { session_id } => {
                client
                    .post(&format!("/local/sessions/{session_id}/reject"), &json!({}))
                    .await?
            }
        },
        Commands::Task { command } => match command {
            TaskCommand::Archive { task_id } => {
                client
                    .post(&format!("/local/tasks/{task_id}/archive"), &json!({}))
                    .await?
            }
            TaskCommand::Recover { request_id } => {
                client
                    .post(&format!("/local/requests/{request_id}/recover"), &json!({}))
                    .await?
            }
            TaskCommand::List { pending } => {
                let mut tasks: Value = client.get("/local/tasks").await?;
                if pending {
                    for key in ["executions", "submitted"] {
                        if let Some(rows) = tasks[key].as_array_mut() {
                            rows.retain(|row| row["status"] == "PENDING_APPROVAL");
                        }
                    }
                }
                tasks
            }
            TaskCommand::Send {
                agent_id,
                peer,
                project,
                prompt,
                request_id,
                task_id,
                timeout,
                max_output_bytes,
                artifacts,
            } => {
                let peers: Value = if Uuid::parse_str(&peer).is_ok() {
                    json!([])
                } else {
                    client.get("/local/peers").await?
                };
                let peer = resolve_id(
                    &peer,
                    peers.as_array().map(Vec::as_slice).unwrap_or(&[]),
                    "node_id",
                    "display_name",
                    true,
                )?;
                let agents: Value = if Uuid::parse_str(&agent_id).is_ok() {
                    json!([])
                } else {
                    client.get(&format!("/local/agents/remote/{peer}")).await?
                };
                let agent_id = resolve_id(
                    &agent_id,
                    agents.as_array().map(Vec::as_slice).unwrap_or(&[]),
                    "agent_id",
                    "name",
                    false,
                )?;
                let mut task = TaskSubmit::new(agent_id, &project, &prompt);
                let request_id = request_id.unwrap_or_else(Uuid::new_v4);
                task.task_id = task_id.unwrap_or(request_id);
                task.limits.timeout_seconds = timeout;
                task.limits.max_output_bytes = max_output_bytes;
                task.expected_artifacts = (!artifacts.is_empty()).then_some(artifacts);
                let body = RemoteTask {
                    peer_id: peer,
                    request_id,
                    task,
                };
                if body.task.expected_artifacts.is_some() {
                    let report: Value = client.post("/local/tasks/preflight", &body).await?;
                    if report["ready"] != true {
                        return Err(MeshError::new(
                            ErrorCode::PermissionDenied,
                            "task preflight blocked; no task was submitted",
                        )
                        .into());
                    }
                }
                client.post("/local/tasks/submit", &body).await?
            }
            TaskCommand::Preflight { task_id } => {
                client
                    .get(&format!("/local/tasks/{task_id}/preflight"))
                    .await?
            }
            TaskCommand::Show { task_id } => client.get(&format!("/local/tasks/{task_id}")).await?,
            TaskCommand::Approve { task_id } => {
                client
                    .post(&format!("/local/tasks/{task_id}/approve"), &json!({}))
                    .await?
            }
            TaskCommand::Reject { task_id } => {
                client
                    .post(&format!("/local/tasks/{task_id}/reject"), &json!({}))
                    .await?
            }
            TaskCommand::Cancel { task_id } => {
                client
                    .post(&format!("/local/tasks/{task_id}/cancel"), &json!({}))
                    .await?
            }
            TaskCommand::Verify {
                task_id,
                artifact,
                expect_json,
            } => {
                client
                    .post(
                        &format!("/local/tasks/{task_id}/verify"),
                        &VerificationRequest {
                            artifact,
                            expected: serde_json::from_str(&expect_json)?,
                        },
                    )
                    .await?
            }
            TaskCommand::Verification { task_id } => {
                client
                    .get(&format!("/local/tasks/{task_id}/verification"))
                    .await?
            }
            TaskCommand::Events { task_id, after } => {
                client
                    .get(&format!("/local/tasks/{task_id}/events?after={after}"))
                    .await?
            }
            TaskCommand::Watch { task_id, mut after } => loop {
                // Observe the task before the event page: a terminal status then
                // guarantees its final events were committed before this read.
                let task: TaskRecord = client.get(&format!("/local/tasks/{task_id}")).await?;
                let events: Vec<TaskEvent> = client
                    .get(&format!("/local/tasks/{task_id}/events?after={after}"))
                    .await?;
                let drained = events.is_empty();
                for event in events {
                    after = event.seq;
                    if cli.json {
                        println!("{}", serde_json::to_string(&event)?);
                    } else {
                        println!(
                            "{} {}",
                            event.seq,
                            human_output(&serde_json::to_value(&event.payload)?)
                        );
                    }
                }
                if task.status.is_terminal() && drained {
                    return Ok(Some(serde_json::to_value(task)?));
                }
                if drained {
                    tokio::time::sleep(std::time::Duration::from_millis(250)).await;
                }
            },
        },
        Commands::Service { command } => match command {
            ServiceCommand::Publish { config } => {
                client.post("/local/services", &read_json(&config)?).await?
            }
            ServiceCommand::List { peer } => {
                client
                    .get(
                        &peer
                            .map(|p| format!("/local/services/remote/{p}"))
                            .unwrap_or_else(|| "/local/services".into()),
                    )
                    .await?
            }
            ServiceCommand::Connect { config } => {
                client
                    .post("/local/connections", &read_json(&config)?)
                    .await?
            }
            ServiceCommand::Revoke { id } => {
                client
                    .post(&format!("/local/services/{id}/revoke"), &json!({}))
                    .await?
            }
            ServiceCommand::Restart { id } => {
                client
                    .post(&format!("/local/services/{id}/restart"), &json!({}))
                    .await?
            }
        },
        Commands::Tunnel { command } => match command {
            TunnelCommand::Add { config } => {
                client.post("/local/tunnels", &read_json(&config)?).await?
            }
            TunnelCommand::List => client.get("/local/tunnels").await?,
            TunnelCommand::Restart { id } => {
                client
                    .post(&format!("/local/tunnels/{id}/restart"), &json!({}))
                    .await?
            }
            TunnelCommand::Revoke { id } => {
                client
                    .post(&format!("/local/tunnels/{id}/revoke"), &json!({}))
                    .await?
            }
        },
        Commands::Connection { command } => match command {
            ConnectionCommand::List => client.get("/local/connections").await?,
            ConnectionCommand::Revoke { id } => {
                client
                    .post(&format!("/local/connections/{id}/revoke"), &json!({}))
                    .await?
            }
        },
        Commands::Integration { command } => match command {
            IntegrationCommand::Start {
                config,
                allow_model,
            } => {
                let spec = read_json(&config)?;
                if spec["max_fix_cycles"].as_u64().is_some_and(|v| v > 0) && !allow_model {
                    return Err(MeshError::input(
                        "pass --allow-model to authorize at most the configured repair budget",
                    )
                    .into());
                }
                client.post("/local/integrations", &spec).await?
            }
            IntegrationCommand::List => client.get("/local/integrations").await?,
            IntegrationCommand::Show { id } => {
                client.get(&format!("/local/integrations/{id}")).await?
            }
            IntegrationCommand::Cancel { id } => {
                client
                    .post(&format!("/local/integrations/{id}/cancel"), &json!({}))
                    .await?
            }
            IntegrationCommand::Watch { id } => loop {
                let run: Value = client.get(&format!("/local/integrations/{id}")).await?;
                if matches!(
                    run["state"].as_str(),
                    Some("verified" | "needs_input" | "cancelled")
                ) {
                    break run;
                }
                tokio::time::sleep(std::time::Duration::from_millis(500)).await;
            },
        },
        Commands::Audit => client.get("/local/audit").await?,
        _ => anyhow::bail!("command not valid in this context"),
    };
    Ok(Some(value))
}
/// A session setting value from the command line: true/false switch a
/// boolean setting, anything else names a choice.
fn setting_value(value: &str) -> Value {
    match value {
        "true" => Value::Bool(true),
        "false" => Value::Bool(false),
        other => Value::String(other.to_owned()),
    }
}
/// `hmesh networks`: through the daemon when it runs (it opens or closes
/// its port at once); otherwise read from this computer and written to
/// node.toml, for the daemon's next start.
async fn networks(instance: &Instance, command: NetworksCommand) -> Result<Value> {
    if let Some(client) = hmesh_daemon::running(&instance.dir).await {
        return Ok(match command {
            NetworksCommand::List => client.get("/local/networks").await?,
            NetworksCommand::Trust { subnet, name } => {
                client
                    .post(
                        "/local/networks/trust",
                        &hmesh_daemon::TrustNetwork { subnet, name },
                    )
                    .await?
            }
            NetworksCommand::Forget { subnet, gateway } => {
                client
                    .post(
                        "/local/networks/forget",
                        &hmesh_daemon::ForgetNetwork { subnet, gateway },
                    )
                    .await?
            }
        });
    }
    let every = instance.config.every_interface();
    let observed =
        hmesh_transport::netinfo::observe(instance.config.node.tailscale_executable.as_deref())
            .await;
    let mut trusted = instance
        .config
        .node
        .trusted_networks
        .clone()
        .unwrap_or_default();
    if !matches!(command, NetworksCommand::List) && !every {
        anyhow::bail!(
            "this computer does not listen on every interface (listen_address = {}): it has no trusted networks; run `hmesh setup --full --network direct` to listen on every interface",
            instance.config.node.listen_address
        );
    }
    match command {
        NetworksCommand::List => {}
        NetworksCommand::Trust { subnet, name } => {
            trust_network(&mut trusted, &observed, subnet, name).map_err(anyhow::Error::msg)?;
            hmesh_daemon::write_trusted(&instance.dir, &trusted)?;
        }
        NetworksCommand::Forget { subnet, gateway } => {
            if forget_network(&mut trusted, subnet, gateway.as_deref()) == 0 {
                anyhow::bail!("this computer does not trust {}", subnet.trunc());
            }
            hmesh_daemon::write_trusted(&instance.dir, &trusted)?;
        }
    }
    Ok(hmesh_daemon::networks_view(every, &trusted, &observed))
}
/// A secret given on the command line, or else (none given, or `-`) the
/// first line of standard input, asked for with `prompt` at a terminal: so
/// it can stay out of the shell history and the process list.
fn secret_line(given: Option<String>, prompt: &str) -> Result<String> {
    if let Some(given) = given.filter(|given| given != "-") {
        return Ok(given);
    }
    use std::io::{BufRead, IsTerminal, Write};
    if std::io::stdin().is_terminal() {
        eprint!("{prompt}");
        std::io::stderr().flush()?;
    }
    let mut line = String::new();
    std::io::stdin().lock().read_line(&mut line)?;
    Ok(line.trim().to_owned())
}
/// The one paired computer with this node ID or display name.
fn one_peer<'a>(peers: &'a [PeerView], peer: &str) -> Result<&'a PeerView> {
    let matches: Vec<&PeerView> = peers
        .iter()
        .filter(|view| {
            view.peer.card.node_id.to_string() == peer.to_ascii_lowercase()
                || view.peer.card.display_name.eq_ignore_ascii_case(peer)
        })
        .collect();
    match matches.as_slice() {
        [view] => Ok(view),
        _ => Err(MeshError::input(if matches.is_empty() {
            "no paired peer has that node ID or name"
        } else {
            "several peers share that name; use the node ID"
        })
        .into()),
    }
}
fn read_spec<T: serde::de::DeserializeOwned>(path: &std::path::Path) -> Result<T> {
    if fs::metadata(path)?.len() > 1_048_576 {
        anyhow::bail!("spec exceeds 1 MiB");
    }
    Ok(serde_json::from_slice(&fs::read(path)?)?)
}
fn write_report(path: &std::path::Path, value: &Value) -> Result<()> {
    use std::io::Write;
    let mut options = fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(path)?;
    file.write_all(&serde_json::to_vec_pretty(value)?)?;
    file.sync_all()?;
    Ok(())
}

fn resolve_id(input: &str, rows: &[Value], id: &str, name: &str, nested: bool) -> Result<Uuid> {
    if let Ok(uuid) = Uuid::parse_str(input) {
        return Ok(uuid);
    }
    let matches: Vec<_> = rows
        .iter()
        .map(|row| if nested { &row["card"] } else { row })
        .filter(|row| row[name].as_str() == Some(input))
        .collect();
    if matches.len() != 1 {
        anyhow::bail!(
            "name must match exactly one visible entry; use UUID for missing or ambiguous names"
        );
    }
    Ok(Uuid::parse_str(
        matches[0][id]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("entry has no UUID"))?,
    )?)
}

fn read_json(path: &std::path::Path) -> Result<Value> {
    let bytes = fs::read(path)?;
    if bytes.len() > 1048576 {
        return Err(MeshError::input("owner configuration exceeds 1 MiB").into());
    }
    Ok(serde_json::from_slice(&bytes)?)
}
fn human_output(value: &Value) -> String {
    fn cell(v: &Value, key: &str) -> String {
        v.get(key)
            .filter(|v| !v.is_null())
            .map(|v| {
                v.as_str()
                    .map(str::to_owned)
                    .unwrap_or_else(|| v.to_string())
            })
            .unwrap_or_else(|| "-".into())
            .chars()
            .map(|c| if c.is_control() { ' ' } else { c })
            .take(160)
            .collect()
    }
    if let Some(rows) = value.as_array() {
        if rows.is_empty() {
            return "(empty)".into();
        }
        let mut output = String::from(
            "ID                                    NAME                 STATE               DETAILS\n",
        );
        for row in rows {
            if row.get("id").is_some() {
                let status = &row["status"];
                output.push_str(&format!(
                    "{:<38}{:<21}{:<20}process={} tunnel={} http={} endpoint={}\n",
                    cell(row, "id"),
                    cell(row, "name"),
                    cell(status, "state"),
                    cell(status, "process"),
                    cell(status, "tunnel"),
                    cell(status, "http_health"),
                    cell(status, "endpoint")
                ));
                continue;
            }
            if row.get("spec").is_some() && row.get("state").is_some() {
                output.push_str(&format!(
                    "{:<38}{:<21}{:<20}cycles={}\n",
                    cell(&row["spec"], "id"),
                    "integration",
                    cell(row, "state"),
                    cell(row, "cycles")
                ));
                continue;
            }
            let card = row.get("card").unwrap_or(row);
            let id = if card.get("agent_id").is_some() {
                cell(card, "agent_id")
            } else if row.get("request_id").is_some() {
                cell(row, "request_id")
            } else if card.get("node_id").is_some() {
                cell(card, "node_id")
            } else {
                cell(row, "task_id")
            };
            let name = if card.get("name").is_some() {
                cell(card, "name")
            } else {
                cell(card, "display_name")
            };
            let state = if row.get("status").is_some() {
                cell(row, "status")
            } else {
                cell(card, "availability")
            };
            let detail = if card.get("harness").is_some() {
                cell(card, "harness")
            } else if card.get("endpoint").is_some() {
                cell(card, "endpoint")
            } else {
                cell(row, "action")
            };
            output.push_str(&format!("{id:<38}{name:<21}{state:<20}{detail}\n"));
        }
        return output.trim_end().into();
    }
    if value.get("task_id").is_some() {
        return format!("Task {}  {}", cell(value, "task_id"), cell(value, "status"));
    }
    if value.get("certificate_pem").is_some() {
        return format!(
            "{}\nNode: {}\nEndpoint: {}\nSHA-256: {}",
            cell(value, "display_name"),
            cell(value, "node_id"),
            cell(value, "endpoint"),
            cell(value, "fingerprint")
        );
    }
    serde_json::to_string_pretty(value).unwrap_or_else(|_| "(unavailable)".into())
}

#[cfg(test)]
mod selection_tests {
    use super::*;
    #[test]
    fn names_never_choose_ambiguous_targets() {
        let id = Uuid::new_v4();
        let rows = vec![json!({"agent_id":id,"name":"worker"})];
        assert_eq!(
            resolve_id("worker", &rows, "agent_id", "name", false).expect("unique"),
            id
        );
        assert!(
            resolve_id(
                "worker",
                &[rows[0].clone(), rows[0].clone()],
                "agent_id",
                "name",
                false
            )
            .is_err()
        );
        assert!(resolve_id("missing", &rows, "agent_id", "name", false).is_err());
    }
    #[test]
    fn owner_verification_cli_accepts_explicit_output_and_timeout() {
        let cli = Cli::try_parse_from([
            "hmesh",
            "agent",
            "verify",
            "5583c679-a688-4653-92c1-ee960fde64ea",
            "--allow-model",
            "--max-output-bytes",
            "16777216",
            "--timeout",
            "600",
        ])
        .expect("CLI");
        assert!(matches!(
            cli.command,
            Commands::Agent {
                command: AgentCommand::Verify {
                    max_output_bytes: 16777216,
                    timeout: 600,
                    ..
                }
            }
        ));
    }
}
