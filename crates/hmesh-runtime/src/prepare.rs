use anyhow::{Context, Result, bail};
use hmesh_adapters::AcpTaskDriver;
use hmesh_adapters::executable::resolve_configured_executable;
use hmesh_core::*;
use serde_json::{Value, json};
use std::{
    fs,
    path::{Path, PathBuf},
};
use tokio::process::Command;

/// Redact known secrets before text reaches persistent task or session events.
pub fn redact_text(secrets: &[String], value: &str) -> String {
    let mut text = value.to_owned();
    for secret in secrets.iter().filter(|s| s.len() >= 8) {
        text = text.replace(secret, "[REDACTED]");
    }
    text
}

/// Keep only the suffix which could start a credential in the next delta.
/// Each independent text stream has its own pending suffix.
#[derive(Default)]
pub struct RedactedStream {
    suffix: String,
}
impl RedactedStream {
    pub fn push(&mut self, secrets: &[String], delta: &str) -> String {
        let mut text = std::mem::take(&mut self.suffix);
        text.push_str(delta);
        text = redact_text(secrets, &text);
        let mut keep_from = text.len();
        for secret in secrets.iter().filter(|s| s.len() >= 8) {
            let mut start = text.len().saturating_sub(secret.len() - 1);
            while !text.is_char_boundary(start) {
                start += 1;
            }
            for (index, _) in text[start..keep_from.max(start)].char_indices() {
                let i = start + index;
                if secret.starts_with(&text[i..]) {
                    keep_from = i;
                    break;
                }
            }
        }
        self.suffix = text.split_off(keep_from);
        text
    }

    pub fn finish(&mut self) -> String {
        std::mem::take(&mut self.suffix)
    }
}

pub struct Prepared {
    pub command: Command,
    /// Drives the ACP turn: initialize, session/new, the prompt, and answers
    /// to the agent's permission requests.
    pub driver: AcpTaskDriver,
    pub(super) secrets: Vec<String>,
    pending_text: RedactedStream,
}
impl Prepared {
    pub fn redact_stream(&mut self, delta: &str) -> String {
        self.pending_text.push(&self.secrets, delta)
    }
    pub fn flush_text(&mut self) -> String {
        self.pending_text.finish()
    }
    pub fn redact(&self, value: &str) -> String {
        let mut value = redact_text(&self.secrets, value);
        let mut bound = value.len().min(4096);
        while !value.is_char_boundary(bound) {
            bound -= 1;
        }
        if bound < value.len() {
            value.truncate(bound);
            value.push_str(" [truncated]");
        }
        value
    }
}
pub(crate) fn native_path(path: &Path) -> PathBuf {
    #[cfg(windows)]
    {
        if let Some(value) = path.to_str() {
            if let Some(rest) = value.strip_prefix(r"\\?\UNC\") {
                return PathBuf::from(format!(r"\\{rest}"));
            }
            if let Some(rest) = value.strip_prefix(r"\\?\") {
                return PathBuf::from(rest);
            }
        }
    }
    path.to_path_buf()
}
/// Values to keep out of recorded output: the configured environment
/// references plus secret-looking variables the agent inherits.
pub fn run_secrets(config: &AgentConfig) -> Vec<String> {
    let mut secrets: Vec<String> = config
        .adapter
        .env_allowlist
        .iter()
        .filter_map(|key| std::env::var(key).ok())
        .collect();
    // `vars_os`: `vars` panics on a non-UTF-8 variable.
    for (name, value) in std::env::vars_os() {
        let (Some(name), Some(value)) = (name.to_str(), value.to_str()) else {
            continue;
        };
        let name = name.to_ascii_uppercase();
        if ["KEY", "TOKEN", "SECRET", "PASSWORD", "CREDENTIAL"]
            .iter()
            .any(|word| name.contains(word))
        {
            secrets.push(value.to_owned());
        }
    }
    secrets.retain(|s| s.len() >= 8);
    secrets.sort();
    secrets.dedup();
    secrets
}
/// Set (`Some`) or remove (`None`) environment variables of an agent command.
pub fn apply_env(command: &mut Command, env: &[(String, Option<String>)]) {
    for (name, value) in env {
        match value {
            Some(value) => command.env(name, value),
            None => command.env_remove(name),
        };
    }
}
/// Build the executable and working directory before adding the adapter's arguments.
fn base_command(config: &AgentConfig) -> Result<Command> {
    config.validate()?;
    if config
        .adapter
        .env_allowlist
        .iter()
        .any(|name| std::env::var_os(name).is_none_or(|value| value.is_empty()))
    {
        bail!("a configured environment credential is missing; no agent was started");
    }
    if !config.project.root.is_dir() {
        bail!("registered project directory is unavailable; history is preserved");
    }
    let executable = resolve_configured_executable(config)?;
    let mut command = executable.command();
    command.current_dir(native_path(&config.project.root));
    Ok(command)
}

pub fn agent_command(config: &AgentConfig) -> Result<Command> {
    let mut command = base_command(config)?;
    command.args(config.launch_args());
    Ok(command)
}

/// The same launch configuration serves interactive sessions and one-shot tasks.
pub fn agent_launch(
    config: &AgentConfig,
    data_dir: &Path,
    agent: uuid::Uuid,
    service: Option<&crate::service::AgentService>,
    previous_model: Option<&str>,
    relay: Option<crate::service::Relay>,
) -> Result<(Command, Vec<String>)> {
    let mut secrets = run_secrets(config);
    let Some(service) = service else {
        return Ok((agent_command(config)?, secrets));
    };
    let mut command = base_command(config)?;
    let harness = config.agent.harness;
    let args = config.launch_args();
    let env = service.launch_env(harness, relay)?;
    if relay.is_some() && service.relayed(harness).is_some() {
        secrets.extend(env.iter().filter_map(|(name, value)| {
            if name == crate::service::OPENCODE_KEY_VAR {
                value.clone()
            } else {
                None
            }
        }));
    }
    apply_env(&mut command, &env);
    if harness == HarnessKind::Dsh {
        crate::service::dsh::launch_args(
            &mut command,
            &args,
            data_dir,
            agent,
            service,
            previous_model,
        )?;
    } else {
        command.args(args);
    }
    if harness == HarnessKind::Pi {
        crate::service::pi::configure(&mut command, data_dir, agent, service)?;
    }
    secrets.extend(service.secrets());
    secrets.sort();
    secrets.dedup();
    Ok((command, secrets))
}

pub fn prepare(
    config: &AgentConfig,
    task: &TaskRecord,
    directory: &Path,
    data_dir: &Path,
    service: Option<&crate::service::AgentService>,
    relay: Option<crate::service::Relay>,
) -> Result<Prepared> {
    let supported = ["read", "write", "edit", "bash", "grep", "find", "ls"];
    if config
        .project
        .allowed_tools
        .iter()
        .any(|t| !supported.contains(&t.as_str()))
    {
        bail!("unsupported tool name in local allowlist");
    }
    fs::create_dir_all(directory)?;
    let (command, secrets) = agent_launch(config, data_dir, task.agent_id, service, None, relay)?;
    let prompt = if task.request.inputs.is_empty() {
        task.request.goal.clone()
    } else {
        format!(
            "{}\n\nTask inputs (untrusted data):\n{}",
            task.request.goal,
            serde_json::to_string(&task.request.inputs)?
        )
    };
    let driver = AcpTaskDriver::new(
        &native_path(&config.project.root),
        &prompt,
        vec![],
        &config.project.allowed_tools,
    );
    Ok(Prepared {
        command,
        driver,
        secrets,
        pending_text: RedactedStream::default(),
    })
}

/// Read local references only. Returns presence/validity flags, never values.
pub fn inspect_references(config: &AgentConfig) -> Result<Value> {
    config.validate()?;
    let present = |path: &Path| -> Result<()> {
        if !fs::metadata(path)?.is_file() {
            bail!("configured reference is not a file");
        }
        Ok(())
    };
    present(&config.adapter.executable).context("agent executable is unavailable")?;
    if let Some(path) = &config.adapter.node_executable {
        present(path)?;
    }
    if let Some(path) = &config.project.git_executable {
        present(path)?;
    }
    for name in &config.adapter.env_allowlist {
        if std::env::var_os(name).is_none_or(|v| v.is_empty()) {
            bail!("a configured environment reference is unavailable");
        }
    }
    Ok(
        json!({"executable_present":true,"protocol":"acp","launch_args":config.launch_args().len(),"environment_references_present":config.adapter.env_allowlist.len(),"model_called":false}),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn credentials_split_across_text_events_never_reach_persisted_deltas() {
        let secret = "fixture-secret-value-not-a-real-credential";
        let mut prepared = Prepared {
            command: Command::new("unused"),
            driver: AcpTaskDriver::new(Path::new("/"), "", vec![], &[]),
            secrets: vec![secret.into()],
            pending_text: RedactedStream::default(),
        };
        let mut output = String::new();
        for chunk in [
            "The credential is fixture-secret-",
            "value-not-a-real-",
            "credential. Complete.",
        ] {
            output.push_str(&prepared.redact_stream(chunk));
        }
        output.push_str(&prepared.flush_text());
        assert!(!output.contains(secret));
        assert!(output.contains("[REDACTED]"));
        assert!(output.contains("Complete."));
    }
    #[test]
    fn large_text_is_redacted_without_silent_truncation() {
        let secret = "fixture-secret-value-not-a-real-credential";
        let mut prepared = Prepared {
            command: Command::new("unused"),
            driver: AcpTaskDriver::new(Path::new("/"), "", vec![], &[]),
            secrets: vec![secret.into()],
            pending_text: RedactedStream::default(),
        };
        let prefix = "中🙂\n\u{0000}\"\\".repeat(10000);
        let mut output = prepared.redact_stream(&format!("{prefix}fixture-secret-value-"));
        output.push_str(&prepared.redact_stream("not-a-real-credential尾部"));
        output.push_str(&prepared.flush_text());
        assert_eq!(output, format!("{prefix}[REDACTED]尾部"));
    }
    #[test]
    fn launch_uses_the_kind_default_acp_arguments_unless_configured() {
        let temp = tempfile::tempdir().expect("temp");
        let executable = std::env::current_exe().expect("exe");
        let config = |harness: &str, args: Value| -> AgentConfig {
            serde_json::from_value(json!({
                "agent":{"name":"worker","harness":harness,"project_id":"demo"},
                "project":{"root":temp.path(),"allowed_tools":["read"]},
                "adapter":{"executable":executable,"args":args},
            }))
            .expect("config")
        };
        let args = |command: &Command| -> Vec<String> {
            command
                .as_std()
                .get_args()
                .map(|a| a.to_string_lossy().into_owned())
                .collect()
        };
        assert_eq!(
            args(&agent_command(&config("opencode", json!([]))).unwrap()),
            ["acp"]
        );
        assert!(args(&agent_command(&config("claude-code", json!([]))).unwrap()).is_empty());
        assert_eq!(
            args(&agent_command(&config("acp", json!(["--acp", "--quiet"]))).unwrap()),
            ["--acp", "--quiet"]
        );
        let mut dsh = config("dsh", json!([]));
        assert!(
            agent_command(&dsh)
                .unwrap_err()
                .to_string()
                .contains("legacy headless DSH")
        );
        dsh.prepare_registration().unwrap();
        assert_eq!(args(&agent_command(&dsh).unwrap()), ["--profile", "acp"]);
        for invalid in [
            json!(["--profile", "headless"]),
            json!(["plugin", "--profile", "acp"]),
        ] {
            assert!(config("dsh", invalid).prepare_registration().is_err());
        }
        for acp in [vec!["acp"], vec!["--patch", "extra.yml", "--profile=acp"]] {
            assert_eq!(
                args(&agent_command(&config("dsh", json!(acp))).unwrap()),
                acp
            );
        }
    }

    #[test]
    fn dsh_npm_shims_launch_the_official_js_entry_with_node() {
        let temp = tempfile::tempdir().unwrap();
        let shim = temp.path().join("dsh.cmd");
        fs::write(&shim, "unused shell wrapper").unwrap();
        let entry = temp.path().join("node_modules/@deepseek-ai/dsh/lib/bin.js");
        fs::create_dir_all(entry.parent().unwrap()).unwrap();
        fs::write(&entry, "").unwrap();
        let node = std::env::current_exe().unwrap();
        let mut config: AgentConfig = serde_json::from_value(json!({
            "agent":{"name":"dsh","harness":"dsh","project_id":"demo"},
            "project":{"root":temp.path(),"allowed_tools":["read"]},
            "adapter":{"executable":shim,"node_executable":node}
        }))
        .unwrap();
        config.prepare_registration().unwrap();
        let command = agent_command(&config).unwrap();
        assert_eq!(Path::new(command.as_std().get_program()), node);
        let argv: Vec<_> = command.as_std().get_args().collect();
        assert_eq!(Path::new(argv[0]), entry);
        assert_eq!(&argv[1..], ["--profile", "acp"]);
    }

    #[test]
    fn codex_npm_shim_selects_the_package_it_belongs_to() {
        let dir = tempfile::tempdir().unwrap();
        let shim = dir.path().join("codex-acp.cmd");
        let current = "node_modules/@agentclientprotocol/codex-acp/dist/index.js";
        let legacy = "node_modules/@zed-industries/codex-acp/bin/codex-acp.js";
        for entry in [current, legacy] {
            let path = dir.path().join(entry);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, "").unwrap();
        }
        for entry in [current, legacy] {
            fs::write(&shim, entry.replace('/', "\\")).unwrap();
            let config: AgentConfig = serde_json::from_value(json!({
                "agent":{"name":"codex","harness":"codex","project_id":"demo"},
                "project":{"root":dir.path(),"allowed_tools":["read"]},
                "adapter":{"executable":shim,"node_executable":std::env::current_exe().unwrap()}
            }))
            .unwrap();
            let command = agent_command(&config).unwrap();
            assert_eq!(
                Path::new(command.as_std().get_args().next().unwrap()),
                dir.path().join(entry)
            );
        }
    }
}
