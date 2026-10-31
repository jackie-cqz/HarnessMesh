use anyhow::Result;
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
    Init {
        #[arg(long, default_value = "local-node")]
        name: String,
        #[arg(long, default_value_t = 38880)]
        port: u16,
        #[arg(long, default_value = "loopback", value_parser = ["loopback", "tailscale"])]
        network: String,
        #[arg(long)]
        listen_address: Option<std::net::IpAddr>,
        #[arg(long)]
        tailscale_executable: Option<PathBuf>,
    },
    Doctor {
        #[arg(long)]
        peer: Option<Uuid>,
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
    Agent {
        #[command(subcommand)]
        command: AgentCommand,
    },
    Agents {
        #[arg(value_parser = ["list"], default_value = "list")]
        action: String,
        #[arg(long)]
        peer: Option<Uuid>,
    },
    Policy {
        #[arg(long)]
        file: PathBuf,
    },
    Task {
        #[command(subcommand)]
        command: TaskCommand,
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
}
#[derive(Subcommand)]
enum PairCommand {
    Show,
    List,
    Request {
        #[arg(long)]
        peer: String,
        #[arg(long)]
        expect_fingerprint: String,
    },
    Approve {
        request_id: Uuid,
        #[arg(long)]
        expect_fingerprint: String,
    },
    Revoke {
        peer_id: Uuid,
    },
}
#[derive(Subcommand)]
enum AgentCommand {
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
    },
    Artifacts {
        agent_id: Uuid,
        #[arg(long = "path", required = true)]
        paths: Vec<PathBuf>,
    },
}
#[derive(Subcommand)]
enum TaskCommand {
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
    if let Commands::Init {
        name,
        port,
        network,
        listen_address,
        tailscale_executable,
    } = cli.command
    {
        let mut config = NodeConfig::default();
        config.node.name = name;
        config.node.listen_port = port;
        config.node.network = network.clone();
        config.node.tailscale_executable = tailscale_executable;
        if network == "tailscale" {
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
    let instance = Instance::load(&dir)?;
    if let Commands::Doctor { peer } = &cli.command {
        let daemon = match local_client(&instance) {
            Ok(client) => Some(match client.get::<Value>("/local/status").await {
                Ok(status) => json!({"state":"reachable", "status":status}),
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
                        match status.require_local_ip(instance.config.node.listen_address) {
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
        return Ok(Some(serde_json::to_value(card)?));
    }
    let client = local_client(&instance)?;
    let value: Value = match cli.command {
        Commands::Daemon {
            command: DaemonCommand::Status,
        } => client.get("/local/status").await?,
        Commands::Daemon {
            command: DaemonCommand::Stop,
        } => client.post("/local/stop", &json!({})).await?,
        Commands::Peers {
            command: PeersCommand::List,
        } => client.get("/local/peers").await?,
        Commands::Pair { command } => match command {
            PairCommand::List => client.get("/local/pairs").await?,
            PairCommand::Request {
                peer,
                expect_fingerprint,
            } => {
                let endpoint = if peer.starts_with("https://") {
                    peer
                } else {
                    format!("https://{peer}")
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
                },
        } => {
            client
                .post(
                    &format!("/local/agents/{agent_id}/verify"),
                    &VerifyRequest {
                        allow_model,
                        prompt,
                    },
                )
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
        Commands::Task { command } => match command {
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
                client
                    .post(
                        "/local/tasks/submit",
                        &RemoteTask {
                            peer_id: peer,
                            request_id,
                            task,
                        },
                    )
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
}
