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

pub struct Prepared {
    pub command: Command,
    /// Drives the ACP turn: initialize, session/new, the prompt, and answers
    /// to the agent's permission requests.
    pub driver: AcpTaskDriver,
    secrets: Vec<String>,
    pending_text: String,
}
impl Prepared {
    pub fn redact_stream(&mut self, delta: &str) -> String {
        self.pending_text.push_str(delta);
        for secret in &self.secrets {
            if secret.len() >= 8 {
                self.pending_text = self.pending_text.replace(secret, "[REDACTED]");
            }
        }
        let retain = self
            .secrets
            .iter()
            .filter(|s| s.len() >= 8)
            .map(String::len)
            .max()
            .unwrap_or(1)
            - 1;
        let mut bound = self.pending_text.len().saturating_sub(retain);
        while !self.pending_text.is_char_boundary(bound) {
            bound -= 1;
        }
        let remainder = self.pending_text.split_off(bound);
        let ready = std::mem::replace(&mut self.pending_text, remainder);
        self.redact_full(&ready)
    }
    pub fn flush_text(&mut self) -> String {
        let pending = std::mem::take(&mut self.pending_text);
        self.redact_full(&pending)
    }
    fn redact_full(&self, value: &str) -> String {
        let mut value = value.to_owned();
        for secret in &self.secrets {
            if secret.len() >= 8 {
                value = value.replace(secret, "[REDACTED]");
            }
        }
        value
    }
    pub fn redact(&self, value: &str) -> String {
        let mut value = self.redact_full(value);
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
pub(crate) fn run_secrets(config: &AgentConfig) -> Vec<String> {
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
/// The ACP launch command for an Agent: its executable and ACP arguments, run
/// in the project with the daemon's environment so the agent uses its own
/// login and model settings.
pub fn agent_command(config: &AgentConfig) -> Result<Command> {
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
    command
        .args(config.launch_args())
        .current_dir(native_path(&config.project.root));
    Ok(command)
}
pub fn prepare(config: &AgentConfig, task: &TaskRecord, directory: &Path) -> Result<Prepared> {
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
    let command = agent_command(config)?;
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
        secrets: run_secrets(config),
        pending_text: String::new(),
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
            pending_text: String::new(),
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
            pending_text: String::new(),
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
        assert!(agent_command(&config("dsh", json!([]))).is_err());
    }
}
