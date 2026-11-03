use anyhow::{Context, Result, bail};
use hmesh_adapters::executable::{isolated_environment, resolve_configured_executable};
use hmesh_core::*;
use serde_json::{Value, json};
use std::{
    collections::BTreeSet,
    fs,
    path::{Path, PathBuf},
};
use tokio::process::Command;

pub struct Prepared {
    pub command: Command,
    pub input: Vec<u8>,
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
pub fn prepare(config: &AgentConfig, task: &TaskRecord, directory: &Path) -> Result<Prepared> {
    config.validate()?;
    if config
        .adapter
        .env_allowlist
        .iter()
        .any(|name| std::env::var_os(name).is_none_or(|value| value.is_empty()))
    {
        bail!("a configured environment credential is missing; no harness was started");
    }
    if !config.project.root.is_dir() {
        bail!("registered project directory is unavailable; history is preserved");
    }
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
    let root = native_path(directory);
    let project = native_path(&config.project.root);
    let executable = resolve_configured_executable(config)?;
    let mut command = executable.command();
    isolated_environment(
        &mut command,
        config.agent.harness,
        &root,
        &config.adapter.env_allowlist,
    )?;
    command.current_dir(&project);
    let model = config
        .adapter
        .model
        .as_deref()
        .context("explicit local model selection required")?;
    let provider = config
        .adapter
        .provider
        .as_deref()
        .context("explicit local provider selection required")?;
    let mut secrets: Vec<String> = config
        .adapter
        .env_allowlist
        .iter()
        .filter_map(|key| std::env::var(key).ok())
        .collect();
    if let Some(source) = &config.adapter.credential_source {
        if config.agent.harness != HarnessKind::Pi || provider != "deepseek" {
            bail!("credential references currently support only Pi DeepSeek API keys");
        }
        let key = deepseek_key(source)?;
        command.env("DEEPSEEK_API_KEY", &key);
        secrets.push(key);
    }
    if let Some(source) = &config.adapter.models_file {
        if config.agent.harness != HarnessKind::Pi || provider != "deepseek" {
            bail!("model-file references currently support only Pi DeepSeek models");
        }
        let definition = deepseek_model(source, model)?;
        let config_dir = root.join("config");
        fs::create_dir_all(&config_dir)?;
        hmesh_security::atomic_write(
            &config_dir.join("models.json"),
            &serde_json::to_vec_pretty(&definition)?,
        )?;
    }
    let prompt = if task.request.inputs.is_empty() {
        task.request.goal.clone()
    } else {
        format!(
            "{}\n\nTask inputs (untrusted data):\n{}",
            task.request.goal,
            serde_json::to_string(&task.request.inputs)?
        )
    };
    let input = match config.agent.harness {
        HarnessKind::Pi => {
            command
                .args([
                    "--mode",
                    "rpc",
                    "--offline",
                    "--no-session",
                    "--no-extensions",
                    "--no-skills",
                    "--no-prompt-templates",
                    "--no-context-files",
                    "--no-themes",
                    "--no-approve",
                ])
                .args([
                    "--provider",
                    provider,
                    "--model",
                    model,
                    "--tools",
                    &if config.adapter.mesh_server.is_some() {
                        format!(
                            "{},mcp__harnessmesh__*",
                            config.project.allowed_tools.join(",")
                        )
                    } else {
                        config.project.allowed_tools.join(",")
                    },
                ]);
            if let Some(server) = &config.adapter.mesh_server {
                let data_dir = directory
                    .parent()
                    .and_then(Path::parent)
                    .context("managed node directory unavailable")?;
                let registration = json!({"command":native_path(server),"args":["--data-dir",native_path(data_dir),"--parent-task",task.task_id.to_string()],"env":{"DEEPSEEK_API_KEY":""},"exposure":"direct","timeout":45});
                let extension = root.join("harnessmesh.mjs");
                hmesh_security::atomic_write(&extension,format!("export default function(pi) {{ pi.registerMcpServer('harnessmesh', {}); }}\n",serde_json::to_string(&registration)?).as_bytes())?;
                command
                    .args(["--extension", "builtin:mcp", "--extension"])
                    .arg(extension);
            }
            let mut bytes =
                serde_json::to_vec(&json!({"id":task.task_id,"type":"prompt","message":prompt}))?;
            bytes.push(b'\n');
            bytes
        }
        HarnessKind::Dsh => {
            let tools: BTreeSet<_> = config
                .project
                .allowed_tools
                .iter()
                .map(String::as_str)
                .collect();
            if tools != BTreeSet::from(["read", "write", "edit"]) {
                bail!(
                    "DSH V0 permits exactly the read/write/edit tool bundle; other combinations require verified policy support"
                );
            }
            let package = executable
                .entry
                .parent()
                .and_then(Path::parent)
                .context("DSH package root unavailable")?;
            let base_patch = fs::read_to_string(
                package.join("node_modules/@deepseek-ai/dsh-base/cordis.patch.yml"),
            )
            .context("pinned DSH base bundle not found")?;
            let ids: BTreeSet<_> = base_patch
                .lines()
                .filter_map(|line| line.trim().strip_prefix("- id: "))
                .collect();
            let mut patch: Vec<Value> = ids
                .iter()
                .filter(|id| id.starts_with("tool-") && **id != "tool-fs")
                .map(|id| json!({"id":id,"disabled":true}))
                .collect();
            for id in [
                "plugin-manager",
                "config-editor",
                "settings",
                "hmr",
                "ptc-runtime",
                "workflow-ptc",
                "session-title-llm",
                "deepseek-account",
                "llm-deepseek-account",
                "permission",
            ] {
                if ids.contains(id) {
                    patch.push(json!({"id":id,"disabled":true}));
                }
            }
            patch.extend([
                json!({"id":"agent-default-model","config":{"provider":provider,"model":model}}),
                json!({"id":"tools","config":{"mode":"native","maxParallelSubCalls":1}}),
                json!({"id":"sandbox-policy","config":{"mode":"workspace-write","workspaceRoot":project}}),
                // DSH 'never' DENIES requests requiring escalation; it does not
                // mean unconditional approval. In-project file writes use the
                // native workspace policy; elevated actions fail closed.
                json!({"id":"approval","config":{"policy":"never"}}),
            ]);
            let patch_path = root.join("managed.patch.yml");
            hmesh_security::atomic_write(&patch_path, &serde_json::to_vec_pretty(&patch)?)?;
            command
                .args(["--profile", "headless", "--patch"])
                .arg(patch_path)
                .arg("--json");
            prompt.into_bytes()
        }
        HarnessKind::Opencode => {
            if cfg!(windows) && prompt.encode_utf16().count() > 12000 {
                bail!("OpenCode V0 prompt exceeds the verified Windows argv bound");
            }
            let mut permissions = serde_json::Map::from_iter([("*".into(), json!("deny"))]);
            for tool in &config.project.allowed_tools {
                permissions.insert(tool.clone(), json!("allow"));
            }
            let content = json!({"model":format!("{provider}/{model}"),"permission":permissions,"plugin":[],"autoupdate":false,
                "provider":{provider:{"models":{model:{"name":model,"limit":{"context":1000000,"output":32768}}}}}});
            command.env("OPENCODE_CONFIG_CONTENT", serde_json::to_string(&content)?);
            command
                .args(["run", "--pure", "--format", "json", "--dir"])
                .arg(project)
                .args(["--model", &format!("{provider}/{model}"), "--"])
                .arg(prompt);
            vec![]
        }
    };
    Ok(Prepared {
        command,
        input,
        secrets,
        pending_text: String::new(),
    })
}

/// Read local references only. Returns presence/validity flags, never credential values.
pub fn inspect_references(config: &AgentConfig) -> Result<Value> {
    config.validate()?;
    let present = |path: &Path| -> Result<()> {
        if !fs::metadata(path)?.is_file() {
            bail!("configured reference is not a file");
        }
        Ok(())
    };
    present(&config.adapter.executable)?;
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
    if let Some(path) = &config.adapter.credential_source {
        if config.agent.harness != HarnessKind::Pi
            || config.adapter.provider.as_deref() != Some("deepseek")
        {
            bail!("file credential references require Pi/DeepSeek");
        }
        deepseek_key(path)?;
    }
    if let Some(path) = &config.adapter.models_file {
        if config.agent.harness != HarnessKind::Pi
            || config.adapter.provider.as_deref() != Some("deepseek")
        {
            bail!("model references require Pi/DeepSeek");
        }
        deepseek_model(
            path,
            config
                .adapter
                .model
                .as_deref()
                .context("explicit model required")?,
        )?;
    }
    Ok(
        json!({"executable_present":true,"credential_reference_valid":config.adapter.credential_source.is_some(),"models_reference_valid":config.adapter.models_file.is_some(),"environment_references_present":config.adapter.env_allowlist.len(),"model_called":false}),
    )
}
fn local_json(path: &Path) -> Result<Value> {
    use std::io::Read;
    let mut bytes = vec![];
    fs::File::open(path)
        .context("local adapter reference is unavailable")?
        .take(1048577)
        .read_to_end(&mut bytes)?;
    if bytes.len() > 1048576 {
        bail!("local adapter reference exceeds 1 MiB");
    }
    serde_json::from_slice(&bytes)
        .map_err(|_| anyhow::anyhow!("local adapter reference is not valid JSON"))
}
fn deepseek_key(path: &Path) -> Result<String> {
    let document = local_json(path)?;
    let credential = &document["deepseek"];
    if credential.is_null() {
        let key = document["providers"]["deepseek"]["apiKey"]
            .as_str()
            .filter(|key| {
                !key.is_empty() && !key.starts_with('!') && !key.contains(['\r', '\n', '\0'])
            })
            .context("DeepSeek model reference must contain a literal API key")?;
        if key
            .bytes()
            .all(|b| b.is_ascii_uppercase() || b.is_ascii_digit() || b == b'_')
        {
            bail!("DeepSeek environment references require an explicit environment allowlist");
        }
        return Ok(key.to_owned());
    }
    if credential["type"] != "api_key" {
        bail!("DeepSeek credential reference must contain an API key; OAuth is not imported");
    }
    let key = credential["key"]
        .as_str()
        .filter(|key| !key.is_empty() && !key.contains(['\r', '\n', '\0']))
        .context("DeepSeek API key is missing or invalid")?;
    Ok(key.to_owned())
}
fn deepseek_model(path: &Path, model: &str) -> Result<Value> {
    let document = local_json(path)?;
    let provider = &document["providers"]["deepseek"];
    let endpoint = provider["baseUrl"]
        .as_str()
        .unwrap_or("https://api.deepseek.com");
    let url = url::Url::parse(endpoint).context("invalid DeepSeek model endpoint")?;
    if url.scheme() != "https"
        || url.host_str() != Some("api.deepseek.com")
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
        || url.port().is_some_and(|p| p != 443)
    {
        bail!(
            "referenced DeepSeek models must use the official HTTPS API origin without URL credentials"
        );
    }
    let selected = provider["models"]
        .as_array()
        .and_then(|models| models.iter().find(|item| item["id"] == model))
        .context("selected model is absent from local model definitions")?;
    fn has_secret_fields(value: &Value) -> bool {
        match value {
            Value::Object(object) => object.iter().any(|(key, value)| {
                matches!(
                    key.to_ascii_lowercase().as_str(),
                    "apikey" | "headers" | "authorization" | "auth" | "token"
                ) || has_secret_fields(value)
            }),
            Value::Array(values) => values.iter().any(has_secret_fields),
            _ => false,
        }
    }
    if has_secret_fields(selected) {
        bail!("model definitions may not contain credential or header fields");
    }
    let mut selected = selected.clone();
    // Credentials and headers from model configuration are never copied. The
    // credential source is separate and its key is held in process memory only.
    let object = selected
        .as_object_mut()
        .context("model definition must be an object")?;
    object.retain(|key, _| {
        matches!(
            key.as_str(),
            "id" | "name"
                | "api"
                | "reasoning"
                | "input"
                | "cost"
                | "contextWindow"
                | "maxTokens"
                | "compat"
        )
    });
    let api = provider["api"].as_str().unwrap_or("openai-completions");
    Ok(json!({"providers":{"deepseek":{"baseUrl":endpoint,"api":api,"models":[selected]}}}))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn remote_model_reference_preserves_flash_metadata_without_importing_provider_secrets() {
        let temp = tempfile::tempdir().expect("temp");
        let path = temp.path().join("models.json");
        let mut definition = json!({"providers":{"deepseek":{"baseUrl":"https://api.deepseek.com","api":"openai-completions","apiKey":"fixture-secret-not-imported","headers":{"Authorization":"fixture-header-not-imported"},"models":[{"id":"deepseek-v4-flash","name":"Flash","contextWindow":1000000,"maxTokens":384000,"compat":{"thinkingFormat":"deepseek"}}]}}});
        fs::write(&path, serde_json::to_vec(&definition).expect("json")).expect("write");
        let result = deepseek_model(&path, "deepseek-v4-flash").expect("model");
        assert_eq!(
            result["providers"]["deepseek"]["models"][0]["compat"]["thinkingFormat"],
            "deepseek"
        );
        assert!(!result.to_string().contains("fixture-secret"));
        assert!(!result.to_string().contains("fixture-header"));
        definition["providers"]["deepseek"]["baseUrl"] = json!("https://example.com");
        fs::write(&path, serde_json::to_vec(&definition).expect("json")).expect("write");
        assert!(deepseek_model(&path, "deepseek-v4-flash").is_err());
    }
    #[test]
    fn explicit_deepseek_credential_reference_does_not_modify_the_file_or_accept_oauth() {
        let temp = tempfile::tempdir().expect("temp");
        let path = temp.path().join("auth.json");
        let bytes=br#"{"deepseek":{"type":"api_key","key":"fixture-key-not-a-real-credential"},"other":{"type":"oauth","access":"must-not-use"}}"#;
        fs::write(&path, bytes).expect("write");
        assert_eq!(
            deepseek_key(&path).expect("key"),
            "fixture-key-not-a-real-credential"
        );
        assert_eq!(fs::read(&path).expect("read"), bytes);
        fs::write(
            &path,
            br#"{"deepseek":{"type":"oauth","access":"must-not-use"}}"#,
        )
        .expect("write");
        assert!(deepseek_key(&path).is_err());
    }
    #[test]
    fn model_key_reference_rejects_commands_and_environment_names() {
        let temp = tempfile::tempdir().expect("temp");
        let path = temp.path().join("models.json");
        for key in ["!echo secret", "DEEPSEEK_API_KEY", "", "bad\nkey"] {
            fs::write(
                &path,
                serde_json::to_vec(&json!({"providers":{"deepseek":{"apiKey":key}}}))
                    .expect("json"),
            )
            .expect("write");
            assert!(deepseek_key(&path).is_err());
        }
        let bytes = br#"{"providers":{"deepseek":{"apiKey":"fixture-literal-key"}}}"#;
        fs::write(&path, bytes).expect("write");
        assert_eq!(deepseek_key(&path).expect("key"), "fixture-literal-key");
        assert_eq!(fs::read(&path).expect("read"), bytes);
    }
    #[test]
    fn credentials_split_across_text_events_never_reach_persisted_deltas() {
        let secret = "fixture-secret-value-not-a-real-credential";
        let mut prepared = Prepared {
            command: Command::new("unused"),
            input: vec![],
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
            input: vec![],
            secrets: vec![secret.into()],
            pending_text: String::new(),
        };
        let prefix = "中🙂\n\u{0000}\"\\".repeat(10000);
        let mut output = prepared.redact_stream(&format!("{prefix}fixture-secret-value-"));
        output.push_str(&prepared.redact_stream("not-a-real-credential尾部"));
        output.push_str(&prepared.flush_text());
        assert_eq!(output, format!("{prefix}[REDACTED]尾部"));
    }
}
