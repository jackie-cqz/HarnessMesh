use anyhow::Result;
use clap::{Parser, Subcommand};
use hmesh_core::*;
use hmesh_daemon::{
    ArtifactGrant, PairApproval, PairTarget, RemoteTask, VerificationRequest, VerifyRequest,
    local_client,
};
use hmesh_security::Instance;
use serde_json::{Value, json};
use std::{fs, path::PathBuf, process::ExitCode};
use uuid::Uuid;

#[derive(Parser)]
#[command(
    name = "hmesh",
    version,
    about = "Local-first coding harness mesh (V0)"
)]
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
    },
    Doctor,
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
    Audit,
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
    List,
    Send {
        agent_id: Uuid,
        #[arg(long)]
        peer: Uuid,
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
    if let Commands::Init { name, port } = cli.command {
        let mut config = NodeConfig::default();
        config.node.name = name;
        config.node.listen_port = port;
        let instance = Instance::init(&dir, config)?;
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
    if matches!(cli.command, Commands::Doctor) {
        let daemon = match local_client(&instance) {
            Ok(client) => client.get::<Value>("/local/status").await.ok(),
            Err(_) => None,
        };
        return Ok(Some(
            json!({"node_id":instance.card.node_id,"data_dir":instance.dir,"os":std::env::consts::OS,"arch":std::env::consts::ARCH,
            "protocol":PROTOCOL,"network":"loopback","tailscale":"not_required","identity":"loaded","daemon":daemon,"harness_execution":"not_checked"}),
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
            TaskCommand::List => client.get("/local/tasks").await?,
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
        Commands::Audit => client.get("/local/audit").await?,
        _ => anyhow::bail!("command not valid in this context"),
    };
    Ok(Some(value))
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
