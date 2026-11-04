use anyhow::{Context, Result, bail};
use hmesh_core::{AgentConfig, HarnessKind, ProbeReport};
use std::{
    ffi::OsString,
    path::{Path, PathBuf},
    process::Stdio,
    time::Duration,
};
use tokio::{io::AsyncReadExt, process::Command};

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
    let mut entry = executable.to_path_buf();
    if extension.eq_ignore_ascii_case("cmd") || extension.eq_ignore_ascii_case("ps1") {
        let parent = executable.parent().context("missing shim directory")?;
        let relative = match kind {
            HarnessKind::Pi => "node_modules/@earendil-works/pi-coding-agent/dist/cli.js",
            HarnessKind::Dsh => "node_modules/@deepseek-ai/dsh/lib/bin.js",
            HarnessKind::Opencode => "node_modules/opencode-ai/bin/opencode.exe",
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

/// This checks an isolated version entry, never calls a model or reads auth files.
pub async fn probe(config: &AgentConfig, probe_dir: &Path) -> Result<ProbeReport> {
    config.validate()?;
    let executable = resolve_configured_executable(config)?;
    std::fs::create_dir_all(probe_dir)?;
    let mut command = executable.command();
    isolated_environment(&mut command, config.agent.harness, probe_dir, &[])?;
    command
        .arg("--version")
        .current_dir(probe_dir)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    let mut child = command
        .spawn()
        .context("harness version process failed to start")?;
    let stdout = child.stdout.take().context("missing stdout")?;
    let stderr = child.stderr.take().context("missing stderr")?;
    let output = tokio::spawn(async move {
        let mut bytes = vec![];
        stdout
            .take(8192)
            .read_to_end(&mut bytes)
            .await
            .map(|_| bytes)
    });
    let diagnostics = tokio::spawn(async move {
        let mut bytes = vec![];
        stderr
            .take(8192)
            .read_to_end(&mut bytes)
            .await
            .map(|_| bytes)
    });
    let status = match tokio::time::timeout(Duration::from_secs(15), child.wait()).await {
        Ok(result) => result?,
        Err(_) => {
            child.kill().await?;
            bail!("harness version probe timed out");
        }
    };
    let output = output.await??;
    let _ = diagnostics.await??;
    let version = String::from_utf8(output)?.trim().to_owned();
    let expected = crate::compatibility::versions(config.agent.harness);
    let supported = status.success() && expected.contains(&version.as_str());
    Ok(ProbeReport {
        installed: true,
        version: Some(version),
        version_supported: supported,
        protocol_probe: if supported {
            "version_entry_passed"
        } else {
            "unsupported_version"
        }
        .into(),
        authentication: "not_checked".into(),
        managed_run: "not_checked".into(),
        streaming: "not_checked".into(),
        cancel: "not_checked".into(),
        session_resume: "not_supported_in_v0".into(),
        unavailable_reason: if supported {
            Some("real managed run not verified".into())
        } else {
            Some("installed version has no approved protocol fixture".into())
        },
    })
}

pub fn isolated_environment(
    command: &mut Command,
    kind: HarnessKind,
    root: &Path,
    allowlist: &[String],
) -> Result<()> {
    command.env_clear();
    for name in [
        "PATH",
        "SystemRoot",
        "WINDIR",
        "ComSpec",
        "PATHEXT",
        "TEMP",
        "TMP",
        "USERPROFILE",
        "APPDATA",
        "LOCALAPPDATA",
        "LANG",
        "LC_ALL",
    ] {
        if let Some(value) = std::env::var_os(name) {
            command.env(name, value);
        }
    }
    for name in allowlist {
        if let Some(value) = std::env::var_os(name) {
            command.env(name, value);
        }
    }
    match kind {
        HarnessKind::Pi => {
            command
                .env("PI_CODING_AGENT_DIR", root.join("config"))
                .env("PI_CODING_AGENT_SESSION_DIR", root.join("sessions"))
                .env("NODE_COMPILE_CACHE", root.join("node-cache"))
                .env("PI_OFFLINE", "1")
                .env("PI_TELEMETRY", "0");
        }
        HarnessKind::Dsh => {
            command.env("DSH_HOME", root.join("dsh"));
        }
        HarnessKind::Opencode => {
            command
                .env("XDG_CONFIG_HOME", root.join("config"))
                .env("XDG_DATA_HOME", root.join("data"))
                .env("XDG_CACHE_HOME", root.join("cache"))
                .env("XDG_STATE_HOME", root.join("state"))
                .env("OPENCODE_CONFIG_DIR", root.join("config/opencode"))
                .env("OPENCODE_DISABLE_AUTOUPDATE", "true")
                .env("OPENCODE_DISABLE_MODELS_FETCH", "true")
                .env("OPENCODE_DISABLE_DEFAULT_PLUGINS", "true")
                .env("OPENCODE_DISABLE_LSP_DOWNLOAD", "true");
        }
    }
    Ok(())
}
