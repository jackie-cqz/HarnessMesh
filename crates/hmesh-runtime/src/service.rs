//! Per-agent model service settings: how the agent reaches its model (its own
//! login, an API key, or a custom endpoint), an optional default model, and
//! extra environment variables. Stored on the agent's computer only, in
//! `<data dir>/agent-secrets/<agent id>.json` (owner-only permissions), and
//! applied as environment variables when the agent process starts. The
//! agent's own configuration files are never changed.
use anyhow::{Context, Result, bail};
use hmesh_core::HarnessKind;
use hmesh_core::service::*;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use uuid::Uuid;

const MAX_SECRET_BYTES: usize = 8 * 1024;
const MAX_ENV: usize = 32;
/// Environment variable carrying the key OpenCode's `hmesh` provider reads.
const OPENCODE_KEY_VAR: &str = "HMESH_OPENCODE_API_KEY";
/// OpenCode's provider ID for a custom endpoint configured here.
pub const OPENCODE_PROVIDER: &str = "hmesh";

/// The stored settings, secrets included. Never sent over the network.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AgentService {
    pub mode: String,
    #[serde(default)]
    pub base_url: Option<String>,
    #[serde(default)]
    pub default_model: Option<String>,
    #[serde(default)]
    pub api_key: Option<String>,
    #[serde(default)]
    pub env: BTreeMap<String, String>,
}

fn directory(data_dir: &Path) -> PathBuf {
    data_dir.join("agent-secrets")
}
fn file(data_dir: &Path, agent: Uuid) -> PathBuf {
    directory(data_dir).join(format!("{agent}.json"))
}

/// Modes an agent type supports. Every type may use its own login and
/// extra environment variables.
pub fn modes(harness: HarnessKind) -> Vec<String> {
    let mut modes = vec![MODE_AGENT_LOGIN.to_owned()];
    if matches!(
        harness,
        HarnessKind::Opencode | HarnessKind::ClaudeCode | HarnessKind::Codex
    ) {
        modes.extend([MODE_API_KEY.to_owned(), MODE_CUSTOM.to_owned()]);
    }
    modes
}

/// The stored settings for an agent, if its owner set any.
pub fn load(data_dir: &Path, agent: Uuid) -> Result<Option<AgentService>> {
    let path = file(data_dir, agent);
    match fs::read(&path) {
        Ok(bytes) => Ok(Some(
            serde_json::from_slice(&bytes).context("agent service settings are unreadable")?,
        )),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e).context("agent service settings are unreadable"),
    }
}

/// Store settings atomically with owner-only permissions; `None` removes them.
pub fn save(data_dir: &Path, agent: Uuid, service: Option<&AgentService>) -> Result<()> {
    let path = file(data_dir, agent);
    let Some(service) = service else {
        return match fs::remove_file(&path) {
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e.into()),
            _ => Ok(()),
        };
    };
    let dir = directory(data_dir);
    fs::create_dir_all(&dir)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&dir, fs::Permissions::from_mode(0o700))?;
    }
    let temp = dir.join(format!(".{agent}.{}.tmp", Uuid::new_v4().simple()));
    let bytes = serde_json::to_vec_pretty(service)?;
    {
        use std::io::Write;
        let mut options = fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut out = options.open(&temp)?;
        out.write_all(&bytes)?;
        out.sync_all()?;
    }
    if let Err(e) = fs::rename(&temp, &path) {
        let _ = fs::remove_file(&temp);
        return Err(e.into());
    }
    Ok(())
}

fn plain(text: &str, limit: usize) -> bool {
    !text.is_empty() && text.len() <= limit && !text.chars().any(|c| c.is_control())
}
fn valid_env_name(name: &str) -> bool {
    let mut chars = name.chars();
    name.len() <= 128
        && chars
            .next()
            .is_some_and(|c| c.is_ascii_uppercase() || c == '_')
        && chars.all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_')
}
const RESERVED_ENV: [&str; 7] = [
    "PATH",
    "HOME",
    "USER",
    "LOGNAME",
    "SHELL",
    "TMPDIR",
    "USERPROFILE",
];

/// Apply a requested change to the stored settings, checking it against the
/// agent type.
pub fn update(
    harness: HarnessKind,
    existing: Option<&AgentService>,
    request: SetAgentService,
) -> Result<Option<AgentService>> {
    let mode = request.mode.trim().to_owned();
    if !modes(harness).contains(&mode) {
        bail!("this agent type does not support the model service mode {mode:?}");
    }
    let mut env = BTreeMap::new();
    if request.env.len() > MAX_ENV {
        bail!("at most {MAX_ENV} extra environment variables");
    }
    for (name, value) in request.env {
        if !valid_env_name(&name) {
            bail!("invalid environment variable name {name:?}: use capital letters, digits and _");
        }
        if RESERVED_ENV.contains(&name.as_str()) || name.starts_with("HMESH_") {
            bail!("{name} is managed by the system or by HarnessMesh and cannot be set here");
        }
        let value = match value {
            Some(value) => value,
            None => existing
                .and_then(|e| e.env.get(&name).cloned())
                .with_context(|| format!("{name} has no stored value to keep"))?,
        };
        if !plain(&value, MAX_SECRET_BYTES) {
            bail!("the value of {name} must be one line of at most 8 KiB");
        }
        env.insert(name, value);
    }
    let default_model = request
        .default_model
        .map(|m| m.trim().to_owned())
        .filter(|m| !m.is_empty());
    if let Some(model) = &default_model
        && !plain(model, 256)
    {
        bail!("invalid default model name");
    }
    let mut service = AgentService {
        mode: mode.clone(),
        base_url: None,
        default_model,
        api_key: None,
        env,
    };
    if mode != MODE_AGENT_LOGIN {
        service.api_key = match request.api_key {
            Some(key) if key.is_empty() => None,
            Some(key) => {
                let key = key.trim().to_owned();
                if key.len() < 8 || !plain(&key, MAX_SECRET_BYTES) || key.contains(' ') {
                    bail!("the API key must be one line of 8 characters or more, without spaces");
                }
                Some(key)
            }
            None => existing.and_then(|e| e.api_key.clone()),
        };
        if service.api_key.is_none() {
            bail!("this mode needs an API key");
        }
    }
    if mode == MODE_CUSTOM {
        let url = request
            .base_url
            .map(|u| u.trim().to_owned())
            .filter(|u| !u.is_empty())
            .context("a custom endpoint needs its address")?;
        let parsed = url::Url::parse(&url).context("the endpoint address is not a URL")?;
        if !matches!(parsed.scheme(), "http" | "https")
            || parsed.host_str().is_none()
            || !parsed.username().is_empty()
            || parsed.password().is_some()
            || url.len() > 2048
        {
            bail!("the endpoint address must be an http(s) URL without credentials");
        }
        if harness == HarnessKind::Opencode && service.default_model.is_none() {
            bail!("OpenCode needs the model name the custom endpoint serves");
        }
        service.base_url = Some(url);
    }
    let empty = service.mode == MODE_AGENT_LOGIN
        && service.env.is_empty()
        && service.default_model.is_none();
    Ok((!empty).then_some(service))
}

impl AgentService {
    /// Environment changes for the agent process: `Some` sets a variable,
    /// `None` removes one inherited from the daemon that would conflict.
    pub fn launch_env(&self, harness: HarnessKind) -> Result<Vec<(String, Option<String>)>> {
        let set = |name: &str, value: &str| (name.to_owned(), Some(value.to_owned()));
        let unset = |name: &str| (name.to_owned(), None);
        let key = self.api_key.as_deref().unwrap_or_default();
        let mut env = match (self.mode.as_str(), harness) {
            (MODE_AGENT_LOGIN, _) => vec![],
            (MODE_API_KEY, HarnessKind::Opencode) => vec![set("OPENCODE_API_KEY", key)],
            (MODE_API_KEY, HarnessKind::ClaudeCode) => vec![
                set("ANTHROPIC_API_KEY", key),
                unset("ANTHROPIC_AUTH_TOKEN"),
                unset("ANTHROPIC_BASE_URL"),
            ],
            (MODE_API_KEY, HarnessKind::Codex) => {
                vec![set("OPENAI_API_KEY", key), unset("OPENAI_BASE_URL")]
            }
            (MODE_CUSTOM, HarnessKind::Opencode) => {
                let model = self.default_model.as_deref().unwrap_or_default();
                let url = self.base_url.as_deref().unwrap_or_default();
                let config = serde_json::json!({
                    "provider":{OPENCODE_PROVIDER:{
                        "name":"HarnessMesh",
                        "npm":"@ai-sdk/openai-compatible",
                        "options":{"baseURL":url,"apiKey":format!("{{env:{OPENCODE_KEY_VAR}}}")},
                        "models":{model:{"name":model}}
                    }},
                    "model":format!("{OPENCODE_PROVIDER}/{model}"),
                    // Titles and summaries use the same endpoint, not another service.
                    "small_model":format!("{OPENCODE_PROVIDER}/{model}")
                });
                vec![
                    set(OPENCODE_KEY_VAR, key),
                    set("OPENCODE_CONFIG_CONTENT", &config.to_string()),
                ]
            }
            (MODE_CUSTOM, HarnessKind::ClaudeCode) => vec![
                set(
                    "ANTHROPIC_BASE_URL",
                    self.base_url.as_deref().unwrap_or_default(),
                ),
                set("ANTHROPIC_AUTH_TOKEN", key),
                unset("ANTHROPIC_API_KEY"),
            ],
            (MODE_CUSTOM, HarnessKind::Codex) => vec![
                set(
                    "OPENAI_BASE_URL",
                    self.base_url.as_deref().unwrap_or_default(),
                ),
                set("OPENAI_API_KEY", key),
            ],
            (mode, _) => bail!("this agent type does not support the model service mode {mode:?}"),
        };
        env.extend(self.env.iter().map(|(k, v)| (k.clone(), Some(v.clone()))));
        Ok(env)
    }
    /// Values to keep out of recorded output.
    pub fn secrets(&self) -> Vec<String> {
        self.api_key
            .iter()
            .chain(self.env.values())
            .filter(|v| v.len() >= 8)
            .cloned()
            .collect()
    }
    /// Changes whenever anything the agent process sees changes.
    pub fn digest(&self) -> String {
        hmesh_security::sha256_hex(&serde_json::to_vec(self).unwrap_or_default())
    }
    /// The model setting value new sessions start with.
    pub fn default_model_value(&self, harness: HarnessKind) -> Option<String> {
        let model = self.default_model.clone()?;
        Some(
            if harness == HarnessKind::Opencode && self.mode == MODE_CUSTOM {
                format!("{OPENCODE_PROVIDER}/{model}")
            } else {
                model
            },
        )
    }
    pub fn view(&self, agent: Uuid, harness: HarnessKind) -> AgentServiceView {
        AgentServiceView {
            agent_id: agent,
            mode: self.mode.clone(),
            base_url: self.base_url.clone(),
            default_model: self.default_model.clone(),
            api_key: SecretHint::of(self.api_key.as_deref()),
            env: self
                .env
                .iter()
                .map(|(name, value)| EnvHint {
                    name: name.clone(),
                    value: SecretHint::of(Some(value)),
                })
                .collect(),
            modes: modes(harness),
            injected: self
                .launch_env(harness)
                .map(|env| {
                    env.into_iter()
                        .filter(|(_, v)| v.is_some())
                        .map(|(k, _)| k)
                        .collect()
                })
                .unwrap_or_default(),
        }
    }
}

/// The view for an agent with no stored settings.
pub fn default_view(agent: Uuid, harness: HarnessKind) -> AgentServiceView {
    AgentService {
        mode: MODE_AGENT_LOGIN.into(),
        ..Default::default()
    }
    .view(agent, harness)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn request(mode: &str) -> SetAgentService {
        SetAgentService {
            mode: mode.into(),
            ..Default::default()
        }
    }
    #[test]
    fn settings_are_checked_against_the_agent_type() {
        assert!(update(HarnessKind::Acp, None, request(MODE_API_KEY)).is_err());
        let mut key = request(MODE_API_KEY);
        assert!(
            update(HarnessKind::ClaudeCode, None, key.clone()).is_err(),
            "key required"
        );
        key.api_key = Some("short".into());
        assert!(update(HarnessKind::ClaudeCode, None, key.clone()).is_err());
        key.api_key = Some("sk-ant-0123456789".into());
        let stored = update(HarnessKind::ClaudeCode, None, key).unwrap().unwrap();
        // Omitting the key keeps it; switching to the agent's login drops it.
        let kept = update(
            HarnessKind::ClaudeCode,
            Some(&stored),
            request(MODE_API_KEY),
        )
        .unwrap()
        .unwrap();
        assert_eq!(kept.api_key, stored.api_key);
        assert!(
            update(
                HarnessKind::ClaudeCode,
                Some(&stored),
                request(MODE_AGENT_LOGIN)
            )
            .unwrap()
            .is_none()
        );
        let mut custom = request(MODE_CUSTOM);
        custom.api_key = Some("sk-local-0123456789".into());
        custom.base_url = Some("ftp://example.test".into());
        assert!(update(HarnessKind::Codex, None, custom.clone()).is_err());
        custom.base_url = Some("https://user:pw@example.test/v1".into());
        assert!(update(HarnessKind::Codex, None, custom.clone()).is_err());
        custom.base_url = Some("http://127.0.0.1:9/v1".into());
        assert!(
            update(HarnessKind::Opencode, None, custom.clone()).is_err(),
            "model required"
        );
        custom.default_model = Some("demo".into());
        assert!(update(HarnessKind::Opencode, None, custom).is_ok());
        let mut env = request(MODE_AGENT_LOGIN);
        env.env.insert("PATH".into(), Some("/tmp".into()));
        assert!(update(HarnessKind::Acp, None, env.clone()).is_err());
        env.env.clear();
        env.env.insert("lower".into(), Some("x".into()));
        assert!(update(HarnessKind::Acp, None, env.clone()).is_err());
        env.env.clear();
        env.env.insert("EXTRA_TOKEN".into(), None);
        assert!(
            update(HarnessKind::Acp, None, env).is_err(),
            "nothing to keep"
        );
    }
    #[test]
    fn each_agent_type_receives_its_own_variables_and_secrets_are_hidden() {
        let service = AgentService {
            mode: MODE_CUSTOM.into(),
            base_url: Some("http://127.0.0.1:9/v1".into()),
            default_model: Some("demo-model".into()),
            api_key: Some("sk-test-0123456789".into()),
            env: BTreeMap::from([("EXTRA_TOKEN".into(), "extra-secret-value".into())]),
        };
        let env: BTreeMap<_, _> = service
            .launch_env(HarnessKind::Opencode)
            .unwrap()
            .into_iter()
            .collect();
        let config = env["OPENCODE_CONFIG_CONTENT"].as_deref().unwrap();
        // The key travels in its own variable, not inside the config text.
        assert!(!config.contains("sk-test"), "{config}");
        assert!(config.contains("{env:HMESH_OPENCODE_API_KEY}"));
        assert!(config.contains("\"model\":\"hmesh/demo-model\""));
        assert_eq!(env[OPENCODE_KEY_VAR].as_deref(), Some("sk-test-0123456789"));
        assert_eq!(env["EXTRA_TOKEN"].as_deref(), Some("extra-secret-value"));
        let claude: BTreeMap<_, _> = service
            .launch_env(HarnessKind::ClaudeCode)
            .unwrap()
            .into_iter()
            .collect();
        assert_eq!(
            claude["ANTHROPIC_AUTH_TOKEN"].as_deref(),
            Some("sk-test-0123456789")
        );
        assert_eq!(
            claude["ANTHROPIC_API_KEY"], None,
            "a conflicting key is removed"
        );
        assert_eq!(
            service
                .default_model_value(HarnessKind::Opencode)
                .as_deref(),
            Some("hmesh/demo-model")
        );
        let view = service.view(Uuid::nil(), HarnessKind::Opencode);
        let shown = serde_json::to_string(&view).unwrap();
        assert!(
            !shown.contains("0123456789") && !shown.contains("extra-secret"),
            "{shown}"
        );
        assert_eq!(view.api_key.last4.as_deref(), Some("6789"));
        assert_eq!(
            service.secrets(),
            vec![
                "sk-test-0123456789".to_owned(),
                "extra-secret-value".to_owned()
            ]
        );
    }
    #[test]
    fn settings_files_are_owner_only_and_removable() {
        let temp = tempfile::tempdir().unwrap();
        let agent = Uuid::new_v4();
        assert_eq!(load(temp.path(), agent).unwrap(), None);
        let service = AgentService {
            mode: MODE_AGENT_LOGIN.into(),
            env: BTreeMap::from([("EXTRA_TOKEN".into(), "extra-secret-value".into())]),
            ..Default::default()
        };
        save(temp.path(), agent, Some(&service)).unwrap();
        assert_eq!(load(temp.path(), agent).unwrap(), Some(service));
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = |p: &Path| fs::metadata(p).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode(&file(temp.path(), agent)), 0o600);
            assert_eq!(mode(&directory(temp.path())), 0o700);
        }
        save(temp.path(), agent, None).unwrap();
        assert_eq!(load(temp.path(), agent).unwrap(), None);
    }
}
