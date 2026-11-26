use anyhow::{Context, Result, bail};
use hmesh_core::{AgentConfig, HarnessKind, ProbeReport};
use serde_json::Value;
use std::{
    ffi::OsString,
    path::{Path, PathBuf},
    process::Stdio,
    time::Duration,
};
use tokio::{
    io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader},
    process::Command,
};

#[derive(Debug, Clone)]
pub struct ResolvedExecutable {
    pub program: PathBuf,
    pub prefix_args: Vec<OsString>,
    pub entry: PathBuf,
}
impl ResolvedExecutable {
    pub fn command(&self) -> Command {
        let mut command = Command::new(&self.program);
        command.args(&self.prefix_args);
        #[cfg(windows)]
        {
            command.creation_flags(0x08000000);
        }
        command
    }
}
pub fn resolve_executable(kind: HarnessKind, executable: &Path) -> Result<ResolvedExecutable> {
    resolve_with_node(kind, executable, None)
}
pub fn resolve_configured_executable(config: &AgentConfig) -> Result<ResolvedExecutable> {
    resolve_with_node(
        config.agent.harness,
        &config.adapter.executable,
        config.adapter.node_executable.as_deref(),
    )
}
/// Like `resolve_executable`, running a JavaScript entry with `node` instead
/// of the first Node.js on PATH when one is given.
pub fn resolve_executable_with_node(
    kind: HarnessKind,
    executable: &Path,
    node: Option<&Path>,
) -> Result<ResolvedExecutable> {
    resolve_with_node(kind, executable, node)
}
fn resolve_with_node(
    kind: HarnessKind,
    executable: &Path,
    node_override: Option<&Path>,
) -> Result<ResolvedExecutable> {
    if !executable.is_absolute() || !executable.is_file() {
        bail!("harness executable must be an existing absolute local path");
    }
    let extension = executable
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("");
    // npm installs commands as symlinks to their JS entry; follow them so the
    // entry runs under a resolved Node instead of relying on the shebang.
    let mut entry = if executable.is_symlink() {
        std::fs::canonicalize(executable).unwrap_or_else(|_| executable.to_path_buf())
    } else {
        executable.to_path_buf()
    };
    if extension.eq_ignore_ascii_case("cmd") || extension.eq_ignore_ascii_case("ps1") {
        let parent = executable.parent().context("missing shim directory")?;
        let relative = match kind {
            HarnessKind::Pi => "node_modules/pi-acp/dist/index.js",
            HarnessKind::Dsh => "node_modules/@deepseek-ai/dsh/lib/bin.js",
            HarnessKind::Opencode => "node_modules/opencode-ai/bin/opencode.exe",
            HarnessKind::ClaudeCode => {
                "node_modules/@agentclientprotocol/claude-agent-acp/dist/index.js"
            }
            HarnessKind::Codex => {
                const CURRENT: &str = "node_modules/@agentclientprotocol/codex-acp/dist/index.js";
                const LEGACY: &str = "node_modules/@zed-industries/codex-acp/bin/codex-acp.js";
                let shim = std::fs::read_to_string(executable).unwrap_or_default();
                if shim.contains("@zed-industries")
                    || (!parent.join(CURRENT).is_file() && !shim.contains("@agentclientprotocol"))
                {
                    LEGACY
                } else {
                    CURRENT
                }
            }
            HarnessKind::Acp => {
                bail!("configure the agent's real JS entry or native executable, not a shell shim")
            }
        };
        entry = parent.join(relative);
        if !entry.is_file() {
            bail!("unsupported shell shim; configure the real JS entry or native executable");
        }
    }
    if entry
        .extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| e.eq_ignore_ascii_case("js") || e.eq_ignore_ascii_case("mjs"))
    {
        let node = if let Some(node) = node_override {
            if !node.is_absolute() || !node.is_file() {
                bail!("Node override must be an existing absolute executable path");
            }
            node.to_owned()
        } else {
            let paths = std::env::var_os("PATH").context("PATH unavailable for Node resolution")?;
            let node_name = if cfg!(windows) { "node.exe" } else { "node" };
            std::env::split_paths(&paths)
                .filter(|path| path.is_absolute())
                .map(|path| path.join(node_name))
                .find(|path| path.is_file())
                .context("Node.js executable not found")?
        };
        Ok(ResolvedExecutable {
            program: node,
            prefix_args: vec![entry.clone().into_os_string()],
            entry,
        })
    } else {
        if node_override.is_some() {
            bail!("Node override requires the actual JavaScript entry file");
        }
        Ok(ResolvedExecutable {
            program: entry.clone(),
            prefix_args: vec![],
            entry,
        })
    }
}

/// How long an agent may take to answer `initialize` during registration.
/// Adapters started through `npx` can need several seconds.
pub const PROBE_TIMEOUT: Duration = Duration::from_secs(30);

/// Start the agent in ACP mode, send `initialize`, record what it reports and
/// stop it. No session is created and no model is called.
pub async fn probe(config: &AgentConfig, probe_dir: &Path) -> Result<ProbeReport> {
    config.validate()?;
    let executable = resolve_configured_executable(config)?;
    std::fs::create_dir_all(probe_dir)?;
    let mut command = executable.command();
    command
        .args(config.launch_args())
        .current_dir(&config.project.root);
    probe_command(command).await
}

/// Run only the ACP initialize exchange on a preconfigured process command.
pub async fn probe_command(mut command: Command) -> Result<ProbeReport> {
    command
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .kill_on_drop(true);
    let mut child = command.spawn().context("agent process failed to start")?;
    let mut stdin = child.stdin.take().context("missing stdin")?;
    let stdout = child.stdout.take().context("missing stdout")?;
    let mut line = crate::acp::initialize_request(1).to_string();
    line.push('\n');
    let answer = tokio::time::timeout(PROBE_TIMEOUT, async move {
        stdin.write_all(line.as_bytes()).await?;
        stdin.flush().await?;
        let mut lines = BufReader::new(stdout.take(4 * 1024 * 1024)).lines();
        while let Some(line) = lines.next_line().await? {
            let Ok(frame) = serde_json::from_str::<Value>(&line) else {
                continue;
            };
            if let Some(crate::acp::Incoming::Response { id: 1, result }) =
                crate::acp::classify(&frame)
            {
                return Ok::<_, std::io::Error>(Some(result));
            }
        }
        Ok(None)
    })
    .await;
    let _ = child.kill().await;
    let unavailable = |reason: &str| ProbeReport {
        installed: true,
        version: None,
        version_supported: false,
        protocol_probe: "acp_initialize_failed".into(),
        authentication: "not_checked".into(),
        managed_run: "not_checked".into(),
        streaming: "not_checked".into(),
        cancel: "not_checked".into(),
        session_resume: "not_checked".into(),
        unavailable_reason: Some(reason.into()),
    };
    let result = match answer {
        Ok(Ok(Some(Ok(result)))) => result,
        Ok(Ok(Some(Err(message)))) => {
            return Ok(unavailable(&format!(
                "the agent rejected ACP initialize: {message}"
            )));
        }
        Ok(_) => {
            return Ok(unavailable(
                "the agent exited without answering ACP initialize; check that it is started in ACP mode (for example `opencode acp` or `claude-agent-acp`)",
            ));
        }
        Err(_) => {
            return Ok(unavailable(&format!(
                "the agent did not answer ACP initialize within {} s; check that it is started in ACP mode (for example `opencode acp` or `claude-agent-acp`)",
                PROBE_TIMEOUT.as_secs()
            )));
        }
    };
    let protocol = result.get("protocolVersion").and_then(Value::as_u64);
    let supported = protocol == Some(crate::acp::PROTOCOL_VERSION);
    let info = |key: &str| {
        result
            .pointer(&format!("/agentInfo/{key}"))
            .and_then(Value::as_str)
            .map(|s| s.chars().take(128).collect::<String>())
    };
    let resumable = crate::acp::SessionRestore::advertised(&result).is_some();
    let needs_login = result
        .get("authMethods")
        .and_then(Value::as_array)
        .is_some_and(|m| !m.is_empty());
    Ok(ProbeReport {
        installed: true,
        version: info("version"),
        version_supported: supported,
        protocol_probe: if supported {
            "acp_initialize_passed".into()
        } else {
            format!(
                "acp_protocol_{}",
                protocol.map_or("unknown".into(), |p| p.to_string())
            )
        },
        authentication: if needs_login {
            "agent_offers_login_methods".into()
        } else {
            "not_checked".into()
        },
        managed_run: "not_checked".into(),
        streaming: "not_checked".into(),
        cancel: "not_checked".into(),
        session_resume: if resumable {
            "supported".into()
        } else {
            "not_supported".into()
        },
        unavailable_reason: Some(if supported {
            match info("name") {
                Some(name) => format!("{name} answered ACP initialize; no session has run yet"),
                None => "agent answered ACP initialize; no session has run yet".into(),
            }
        } else {
            format!("agent speaks ACP protocol {protocol:?}; version 1 is required")
        }),
    })
}
