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
const MAX_MODELS: usize = 100;
/// Environment variable carrying the key OpenCode's `hmesh` provider reads.
const OPENCODE_KEY_VAR: &str = "HMESH_OPENCODE_API_KEY";
/// OpenCode's provider ID for a custom endpoint configured here.
pub const OPENCODE_PROVIDER: &str = "hmesh";

/// Where an agent reaches its custom endpoint through the daemon's local
/// relay: the relay's address and the token it accepts in place of the key.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Relayed {
    pub url: String,
    pub token: String,
}

/// One proxy exclusion list from several (`NO_PROXY` and `no_proxy`, from
/// the agent's extra variables and the daemon's environment) that also leaves
/// the relay's address alone. Clients disagree on which name wins when both
/// are set, so both get every exclusion.
fn with_loopback<'a>(lists: impl IntoIterator<Item = &'a str>) -> String {
    const RELAY_HOST: &str = "127.0.0.1";
    let mut entries: Vec<&str> = Vec::new();
    for entry in lists
        .into_iter()
        .flat_map(|list| list.split(','))
        .map(str::trim)
        .chain([RELAY_HOST])
    {
        if !entry.is_empty() && !entries.contains(&entry) {
            entries.push(entry);
        }
    }
    entries.join(",")
}

/// Registers an endpoint and its key with the relay: `(endpoint, key)`.
pub type Relay<'a> = &'a dyn Fn(&str, &str) -> Result<Relayed>;

/// The stored settings, secrets included. Never sent over the network.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AgentService {
    pub mode: String,
    #[serde(default)]
    pub base_url: Option<String>,
    #[serde(default)]
    pub default_model: Option<String>,
    /// OpenCode custom endpoint only: every model offered, default first.
    /// Settings saved before lists existed have none and offer the default.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub models: Vec<String>,
    #[serde(default)]
    pub api_key: Option<String>,
    /// The key is read from this variable of the daemon's environment when
    /// the agent starts, instead of being stored.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub api_key_env: Option<String>,
    #[serde(default)]
    pub env: BTreeMap<String, String>,
}

/// The wire format a connection test speaks.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Protocol {
    /// `GET {base}/models`, `POST {base}/chat/completions`.
    OpenAi,
    /// `GET {base}/models`, `POST {base}/responses`: Codex speaks only this.
    Responses,
    /// `GET {base}/v1/models`, `POST {base}/v1/messages`.
    Anthropic,
}
/// How the key is sent, matching what the agent itself would send.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeyHeader {
    Bearer,
    XApiKey,
}
/// Where and how to test a model service.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TestTarget {
    pub protocol: Protocol,
    pub base: String,
    pub key: String,
    pub header: KeyHeader,
    pub model: Option<String>,
}

fn directory(data_dir: &Path) -> PathBuf {
    data_dir.join("agent-secrets")
}
fn file(data_dir: &Path, agent: Uuid) -> PathBuf {
    directory(data_dir).join(format!("{agent}.json"))
}
/// What each agent was last found to be set up with on its own: private to
/// this computer (it can hold an account's email), but not a secret.
fn login_directory(data_dir: &Path) -> PathBuf {
    data_dir.join("agent-login")
}
fn login_file(data_dir: &Path, agent: Uuid) -> PathBuf {
    login_directory(data_dir).join(format!("{agent}.json"))
}

/// The last check of what an agent is set up with on its own.
pub fn load_own_login(data_dir: &Path, agent: Uuid) -> Result<Option<OwnLogin>> {
    match fs::read(login_file(data_dir, agent)) {
        Ok(bytes) => Ok(serde_json::from_slice(&bytes).ok()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e).context("agent login check is unreadable"),
    }
}
pub fn save_own_login(data_dir: &Path, agent: Uuid, login: &OwnLogin) -> Result<()> {
    write_private(
        &login_directory(data_dir),
        &login_file(data_dir, agent),
        agent,
        &serde_json::to_vec_pretty(login)?,
    )
}

/// Write a file atomically, readable only by this user.
fn write_private(dir: &Path, path: &Path, agent: Uuid, bytes: &[u8]) -> Result<()> {
    fs::create_dir_all(dir)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(dir, fs::Permissions::from_mode(0o700))?;
    }
    let temp = dir.join(format!(".{agent}.{}.tmp", Uuid::new_v4().simple()));
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
        out.write_all(bytes)?;
        out.sync_all()?;
    }
    if let Err(e) = fs::rename(&temp, path) {
        let _ = fs::remove_file(&temp);
        return Err(e.into());
    }
    Ok(())
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
    write_private(
        &directory(data_dir),
        &path,
        agent,
        &serde_json::to_vec_pretty(service)?,
    )
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
/// Claude Code's model variables, all set to the chosen model on a custom
/// endpoint.
const CLAUDE_MODEL_VARS: [&str; 5] = [
    "ANTHROPIC_MODEL",
    "ANTHROPIC_DEFAULT_OPUS_MODEL",
    "ANTHROPIC_DEFAULT_SONNET_MODEL",
    "ANTHROPIC_DEFAULT_HAIKU_MODEL",
    "CLAUDE_CODE_SUBAGENT_MODEL",
];
const RESERVED_ENV: [&str; 7] = [
    "PATH",
    "HOME",
    "USER",
    "LOGNAME",
    "SHELL",
    "TMPDIR",
    "USERPROFILE",
];

/// An endpoint address the agent may be given: http(s), with a host, without
/// credentials, query or fragment. A trailing slash is dropped.
fn endpoint(url: Option<String>) -> Result<String> {
    let url = url
        .map(|u| u.trim().trim_end_matches('/').to_owned())
        .filter(|u| !u.is_empty())
        .context("a custom endpoint needs its address")?;
    let parsed = url::Url::parse(&url).context("the endpoint address is not a URL")?;
    if !matches!(parsed.scheme(), "http" | "https")
        || parsed.host_str().is_none()
        || !parsed.username().is_empty()
        || parsed.password().is_some()
        || parsed.query().is_some()
        || parsed.fragment().is_some()
        || url.len() > 2048
    {
        bail!("the endpoint address must be an http(s) URL without credentials, query or fragment");
    }
    Ok(url)
}

fn model_name(model: Option<String>) -> Result<Option<String>> {
    let model = model.map(|m| m.trim().to_owned()).filter(|m| !m.is_empty());
    if let Some(model) = &model
        && !plain(model, 256)
    {
        bail!("invalid model name {model:?}");
    }
    Ok(model)
}

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
    let mut default_model = model_name(request.default_model)?;
    let mut models: Vec<String> = Vec::new();
    for model in request.models {
        if let Some(model) = model_name(Some(model))?
            && !models.contains(&model)
        {
            models.push(model);
        }
    }
    if !models.is_empty() {
        if harness != HarnessKind::Opencode || mode != MODE_CUSTOM {
            bail!(
                "a model list only applies to OpenCode with a custom endpoint; other agents list their own models"
            );
        }
        // The default comes first; with none chosen, the first listed is it.
        let default = default_model
            .get_or_insert_with(|| models[0].clone())
            .clone();
        models.retain(|m| *m != default);
        models.insert(0, default);
        if models.len() > MAX_MODELS {
            bail!("at most {MAX_MODELS} models");
        }
    }
    let mut service = AgentService {
        mode: mode.clone(),
        base_url: None,
        default_model,
        models,
        api_key: None,
        api_key_env: None,
        env,
    };
    if mode != MODE_AGENT_LOGIN {
        let named = request
            .api_key_env
            .as_deref()
            .map(str::trim)
            .filter(|n| !n.is_empty());
        if let Some(name) = named {
            // A key read from the environment: nothing stored.
            if !valid_env_name(name) {
                bail!(
                    "invalid environment variable name {name:?}: use capital letters, digits and _"
                );
            }
            if RESERVED_ENV.contains(&name) || name.starts_with("HMESH_") {
                bail!("{name} is managed by the system or by HarnessMesh and cannot hold the key");
            }
            service.api_key_env = Some(name.to_owned());
        } else {
            service.api_key = match request.api_key {
                Some(key) if key.is_empty() => None,
                Some(key) => {
                    let key = key.trim().to_owned();
                    if key.len() < 8 || !plain(&key, MAX_SECRET_BYTES) || key.contains(' ') {
                        bail!(
                            "the API key must be one line of 8 characters or more, without spaces"
                        );
                    }
                    Some(key)
                }
                None => existing.and_then(|e| e.api_key.clone()),
            };
            // An empty name drops the reference; omitted keeps it while no key is given.
            if service.api_key.is_none() && request.api_key_env.is_none() {
                service.api_key_env = existing.and_then(|e| e.api_key_env.clone());
            }
        }
        if service.api_key.is_none() && service.api_key_env.is_none() {
            bail!("this mode needs an API key");
        }
    }
    if mode == MODE_CUSTOM {
        let url = endpoint(request.base_url)?;
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
    ///
    /// With a relay, an agent that goes through it (see [`Self::relayed`])
    /// gets the relay's address and token instead of the endpoint and key.
    pub fn launch_env(
        &self,
        harness: HarnessKind,
        relay: Option<Relay>,
    ) -> Result<Vec<(String, Option<String>)>> {
        let key = self.key()?.unwrap_or_default();
        match (relay, self.relayed(harness)) {
            (Some(relay), Some(endpoint)) => {
                let route = relay(endpoint, &key)?;
                self.env_for(harness, &route.token, Some(&route.url))
            }
            _ => self.env_with_key(harness, &key),
        }
    }
    /// The endpoint an agent reaches through the daemon's relay rather than
    /// directly: OpenCode on a custom endpoint, whose strict client rejects
    /// replies some OpenAI-compatible gateways send.
    pub fn relayed(&self, harness: HarnessKind) -> Option<&str> {
        (harness == HarnessKind::Opencode && self.mode == MODE_CUSTOM)
            .then_some(self.base_url.as_deref())
            .flatten()
    }
    /// The key the agent receives: the stored one, or the named variable of
    /// the daemon's environment, which must be set.
    pub fn key(&self) -> Result<Option<String>> {
        match &self.api_key_env {
            Some(name) => match std::env::var(name) {
                Ok(value) if !value.trim().is_empty() => Ok(Some(value.trim().to_owned())),
                _ => bail!(
                    "the API key comes from the environment variable {name}, which is not set where HarnessMesh runs; set it and restart the daemon"
                ),
            },
            None => Ok(self.api_key.clone()),
        }
    }
    fn env_with_key(
        &self,
        harness: HarnessKind,
        key: &str,
    ) -> Result<Vec<(String, Option<String>)>> {
        self.env_for(harness, key, None)
    }
    fn env_for(
        &self,
        harness: HarnessKind,
        key: &str,
        relay_url: Option<&str>,
    ) -> Result<Vec<(String, Option<String>)>> {
        let set = |name: &str, value: &str| (name.to_owned(), Some(value.to_owned()));
        let unset = |name: &str| (name.to_owned(), None);
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
                let url = relay_url.or(self.base_url.as_deref()).unwrap_or_default();
                let models: serde_json::Map<String, serde_json::Value> = self
                    .model_list()
                    .into_iter()
                    .map(|m| (m.clone(), serde_json::json!({ "name": m })))
                    .collect();
                let config = serde_json::json!({
                    "provider":{OPENCODE_PROVIDER:{
                        "name":"HarnessMesh",
                        "npm":"@ai-sdk/openai-compatible",
                        "options":{"baseURL":url,"apiKey":format!("{{env:{OPENCODE_KEY_VAR}}}")},
                        "models":models
                    }},
                    // Sessions offer only this endpoint's models, not the ones
                    // from the user's own OpenCode accounts.
                    "enabled_providers":[OPENCODE_PROVIDER],
                    "model":format!("{OPENCODE_PROVIDER}/{model}"),
                    // Titles and summaries use the same endpoint, not another service.
                    "small_model":format!("{OPENCODE_PROVIDER}/{model}")
                });
                vec![
                    set(OPENCODE_KEY_VAR, key),
                    set("OPENCODE_CONFIG_CONTENT", &config.to_string()),
                ]
            }
            (MODE_CUSTOM, HarnessKind::ClaudeCode) => {
                let mut env = vec![
                    set(
                        "ANTHROPIC_BASE_URL",
                        self.base_url.as_deref().unwrap_or_default(),
                    ),
                    set("ANTHROPIC_AUTH_TOKEN", key),
                    unset("ANTHROPIC_API_KEY"),
                    // No background requests to Anthropic with another provider's key.
                    set("CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC", "1"),
                ];
                // Claude Code otherwise sends its own model names, which other
                // providers reject; every tier uses the chosen model.
                if let Some(model) = &self.default_model {
                    for name in CLAUDE_MODEL_VARS {
                        env.push(set(name, model));
                    }
                }
                env
            }
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
        if relay_url.is_some() {
            // The relay is on this computer: a proxy the agent inherits must
            // not carry its requests, or the relay token, anywhere else.
            const NAMES: [&str; 2] = ["NO_PROXY", "no_proxy"];
            let inherited: Vec<String> = NAMES
                .iter()
                .filter_map(|name| std::env::var(name).ok())
                .collect();
            let lists = NAMES
                .iter()
                .filter_map(|name| self.env.get(*name).map(String::as_str))
                .chain(inherited.iter().map(String::as_str));
            let merged = with_loopback(lists);
            for name in NAMES {
                env.push(set(name, &merged));
            }
        }
        Ok(env)
    }
    /// Models offered through the custom endpoint, default first.
    pub fn model_list(&self) -> Vec<String> {
        if self.models.is_empty() {
            self.default_model.iter().cloned().collect()
        } else {
            self.models.clone()
        }
    }
    /// Values to keep out of recorded output.
    pub fn secrets(&self) -> Vec<String> {
        let from_env = self
            .key()
            .ok()
            .flatten()
            .filter(|_| self.api_key_env.is_some());
        self.api_key
            .iter()
            .chain(from_env.iter())
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
    /// The model setting HarnessMesh asks the agent for over ACP. Claude Code
    /// on a custom endpoint gets its model through the environment instead,
    /// since its own model choices are Claude names.
    pub fn model_setting(&self, harness: HarnessKind) -> Option<String> {
        if harness == HarnessKind::ClaudeCode && self.mode == MODE_CUSTOM {
            return None;
        }
        self.default_model_value(harness)
    }
    pub fn view(&self, agent: Uuid, harness: HarnessKind) -> AgentServiceView {
        AgentServiceView {
            agent_id: agent,
            mode: self.mode.clone(),
            base_url: self.base_url.clone(),
            default_model: self.default_model.clone(),
            // The stored list only: settings with just a default model show
            // an empty list, so changing the default does not keep the old one.
            models: if harness == HarnessKind::Opencode && self.mode == MODE_CUSTOM {
                self.models.clone()
            } else {
                Vec::new()
            },
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
            // Names only: a key from the environment need not be set to list them.
            injected: self
                .env_with_key(harness, "")
                .map(|env| {
                    env.into_iter()
                        .filter(|(_, v)| v.is_some())
                        .map(|(k, _)| k)
                        .collect()
                })
                .unwrap_or_default(),
            api_key_env: self.api_key_env.clone(),
            own_login: None,
            presets: presets_for(harness),
        }
    }
}

/// Where a connection test goes for these (possibly unsaved) settings. A key
/// left out of the request is the stored one.
pub fn test_target(
    harness: HarnessKind,
    existing: Option<&AgentService>,
    request: TestAgentService,
) -> Result<TestTarget> {
    let mode = request.mode.trim();
    if !modes(harness).iter().any(|m| m == mode) {
        bail!("this agent type does not support the model service mode {mode:?}");
    }
    if mode == MODE_AGENT_LOGIN {
        bail!("this agent uses its own login; there is no address or key here to test");
    }
    let named = request
        .api_key_env
        .as_deref()
        .map(str::trim)
        .filter(|n| !n.is_empty());
    let key = match (request.api_key.map(|k| k.trim().to_owned()), named) {
        (Some(key), _) if !key.is_empty() => key,
        (_, Some(name)) => std::env::var(name)
            .ok()
            .map(|v| v.trim().to_owned())
            .filter(|v| !v.is_empty())
            .with_context(|| format!("{name} is not set where HarnessMesh runs"))?,
        _ => match existing {
            Some(e) => e.key()?.context("enter an API key to test")?,
            None => bail!("enter an API key to test"),
        },
    };
    if !plain(&key, MAX_SECRET_BYTES) || key.contains(' ') {
        bail!("the API key must be one line without spaces");
    }
    let base = match (mode, harness) {
        (MODE_CUSTOM, _) => endpoint(request.base_url)?,
        (_, HarnessKind::ClaudeCode) => "https://api.anthropic.com".to_owned(),
        (_, HarnessKind::Codex) => "https://api.openai.com/v1".to_owned(),
        _ => bail!(
            "OpenCode's own service cannot be tested from here; test it in OpenCode, or use a custom endpoint"
        ),
    };
    let (protocol, header) = match (harness, mode) {
        (HarnessKind::ClaudeCode, MODE_API_KEY) => (Protocol::Anthropic, KeyHeader::XApiKey),
        // A custom endpoint for Claude Code gets ANTHROPIC_AUTH_TOKEN, a bearer token.
        (HarnessKind::ClaudeCode, _) => (Protocol::Anthropic, KeyHeader::Bearer),
        (HarnessKind::Codex, _) => (Protocol::Responses, KeyHeader::Bearer),
        _ => (Protocol::OpenAi, KeyHeader::Bearer),
    };
    let model = match model_name(request.model)? {
        Some(model) => Some(model),
        None => existing.and_then(|e| e.default_model.clone()),
    };
    Ok(TestTarget {
        protocol,
        base,
        key,
        header,
        model,
    })
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
    fn opencode_on_a_custom_endpoint_gets_the_relay_and_never_the_key() {
        let service = AgentService {
            mode: MODE_CUSTOM.into(),
            base_url: Some("https://gateway.example/v1".into()),
            default_model: Some("demo-model".into()),
            api_key: Some("sk-test-0123456789".into()),
            env: BTreeMap::from([("NO_PROXY".into(), "corp.example".into())]),
            ..AgentService::default()
        };
        let asked = std::sync::Mutex::new(Vec::new());
        let relay = |endpoint: &str, key: &str| {
            asked
                .lock()
                .unwrap()
                .push((endpoint.to_owned(), key.to_owned()));
            Ok(Relayed {
                url: "http://127.0.0.1:7/relay".into(),
                token: "hmr-local-token".into(),
            })
        };
        let env: BTreeMap<_, _> = service
            .launch_env(HarnessKind::Opencode, Some(&relay))
            .unwrap()
            .into_iter()
            .collect();
        let config: serde_json::Value =
            serde_json::from_str(env["OPENCODE_CONFIG_CONTENT"].as_deref().unwrap()).unwrap();
        assert_eq!(
            config["provider"]["hmesh"]["options"]["baseURL"],
            "http://127.0.0.1:7/relay"
        );
        assert_eq!(env[OPENCODE_KEY_VAR].as_deref(), Some("hmr-local-token"));
        assert!(env.values().flatten().all(|v| !v.contains("sk-test")));
        // A proxy the agent inherits leaves the relay alone; the owner's own
        // exclusions stay.
        // Both names carry every exclusion: a client reading either one
        // still skips the proxy for the owner's own hosts.
        assert_eq!(env["NO_PROXY"], env["no_proxy"]);
        let merged = env["no_proxy"].as_deref().unwrap();
        assert!(
            merged.starts_with("corp.example,") && merged.contains("127.0.0.1"),
            "{merged}"
        );
        assert_eq!(with_loopback([""]), "127.0.0.1");
        assert_eq!(
            with_loopback(["localhost, 127.0.0.1", "internal.corp,localhost,"]),
            "localhost,127.0.0.1,internal.corp"
        );
        assert_eq!(
            *asked.lock().unwrap(),
            [(
                "https://gateway.example/v1".to_owned(),
                "sk-test-0123456789".to_owned()
            )]
        );
        // Other agents and other modes reach their service directly.
        let claude: BTreeMap<_, _> = service
            .launch_env(HarnessKind::ClaudeCode, Some(&relay))
            .unwrap()
            .into_iter()
            .collect();
        assert_eq!(
            claude["ANTHROPIC_BASE_URL"].as_deref(),
            Some("https://gateway.example/v1")
        );
        assert_eq!(claude["NO_PROXY"].as_deref(), Some("corp.example"));
        let own_key = AgentService {
            mode: MODE_API_KEY.into(),
            ..service.clone()
        };
        assert_eq!(own_key.relayed(HarnessKind::Opencode), None);
        assert_eq!(asked.lock().unwrap().len(), 1);
    }
    #[test]
    fn each_agent_type_receives_its_own_variables_and_secrets_are_hidden() {
        let service = AgentService {
            mode: MODE_CUSTOM.into(),
            base_url: Some("http://127.0.0.1:9/v1".into()),
            default_model: Some("demo-model".into()),
            models: vec!["demo-model".into(), "second-model".into()],
            api_key: Some("sk-test-0123456789".into()),
            api_key_env: None,
            env: BTreeMap::from([("EXTRA_TOKEN".into(), "extra-secret-value".into())]),
        };
        let env: BTreeMap<_, _> = service
            .launch_env(HarnessKind::Opencode, None)
            .unwrap()
            .into_iter()
            .collect();
        let config = env["OPENCODE_CONFIG_CONTENT"].as_deref().unwrap();
        // The key travels in its own variable, not inside the config text.
        assert!(!config.contains("sk-test"), "{config}");
        assert!(config.contains("{env:HMESH_OPENCODE_API_KEY}"));
        assert!(config.contains("\"model\":\"hmesh/demo-model\""));
        let parsed: serde_json::Value = serde_json::from_str(config).unwrap();
        let offered: Vec<&String> = parsed["provider"]["hmesh"]["models"]
            .as_object()
            .unwrap()
            .keys()
            .collect();
        assert_eq!(offered, ["demo-model", "second-model"]);
        // Sessions list only this endpoint's models.
        assert_eq!(parsed["enabled_providers"], serde_json::json!(["hmesh"]));
        assert_eq!(env[OPENCODE_KEY_VAR].as_deref(), Some("sk-test-0123456789"));
        assert_eq!(env["EXTRA_TOKEN"].as_deref(), Some("extra-secret-value"));
        let claude: BTreeMap<_, _> = service
            .launch_env(HarnessKind::ClaudeCode, None)
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
        // Every Claude Code model tier uses the chosen model, and the model is
        // not also asked for over ACP (Claude Code lists only Claude names).
        for name in CLAUDE_MODEL_VARS {
            assert_eq!(claude[name].as_deref(), Some("demo-model"), "{name}");
        }
        assert_eq!(
            claude["CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC"].as_deref(),
            Some("1")
        );
        assert_eq!(service.model_setting(HarnessKind::ClaudeCode), None);
        assert_eq!(
            service.model_setting(HarnessKind::Opencode).as_deref(),
            Some("hmesh/demo-model")
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
    #[test]
    fn model_lists_belong_to_opencode_custom_endpoints() {
        let mut custom = request(MODE_CUSTOM);
        custom.api_key = Some("sk-local-0123456789".into());
        custom.base_url = Some("http://127.0.0.1:9/v1/".into());
        custom.models = vec![
            " model-b ".into(),
            "model-a".into(),
            "model-b".into(),
            "".into(),
        ];
        // With no default chosen, the first listed model is the default.
        let stored = update(HarnessKind::Opencode, None, custom.clone())
            .unwrap()
            .unwrap();
        assert_eq!(stored.default_model.as_deref(), Some("model-b"));
        assert_eq!(stored.models, ["model-b", "model-a"]);
        assert_eq!(
            stored.base_url.as_deref(),
            Some("http://127.0.0.1:9/v1"),
            "trailing slash dropped"
        );
        // A chosen default moves to the front, and is added when missing.
        custom.default_model = Some("model-c".into());
        let stored = update(HarnessKind::Opencode, None, custom.clone())
            .unwrap()
            .unwrap();
        assert_eq!(stored.models, ["model-c", "model-b", "model-a"]);
        assert_eq!(
            stored.view(Uuid::nil(), HarnessKind::Opencode).models,
            ["model-c", "model-b", "model-a"]
        );
        assert!(
            update(HarnessKind::ClaudeCode, None, custom.clone()).is_err(),
            "other agents list their own models"
        );
        custom.models = (0..=MAX_MODELS).map(|i| format!("m{i}")).collect();
        assert!(update(HarnessKind::Opencode, None, custom).is_err());
        // Settings saved before model lists existed still load and offer
        // their default model.
        let old: AgentService = serde_json::from_str(
            r#"{"mode":"custom","base_url":"http://127.0.0.1:9/v1","default_model":"only","api_key":"sk-local-0123456789","env":{}}"#,
        )
        .unwrap();
        assert_eq!(old.model_list(), ["only"]);
        let unchanged = serde_json::to_string(&old).unwrap();
        assert!(!unchanged.contains("models"), "{unchanged}");
        // Its view lists no models, so a new default replaces the old one.
        assert!(
            old.view(Uuid::nil(), HarnessKind::Opencode)
                .models
                .is_empty()
        );
    }
    #[test]
    fn connection_tests_go_where_the_agent_would_go() {
        let stored = AgentService {
            mode: MODE_API_KEY.into(),
            default_model: Some("stored-model".into()),
            api_key: Some("sk-stored-0123456789".into()),
            ..Default::default()
        };
        let test = |harness, mode: &str, base: Option<&str>, key: Option<&str>| {
            test_target(
                harness,
                Some(&stored),
                TestAgentService {
                    mode: mode.into(),
                    base_url: base.map(Into::into),
                    api_key: key.map(Into::into),
                    api_key_env: None,
                    model: None,
                },
            )
        };
        let claude = test(HarnessKind::ClaudeCode, MODE_API_KEY, None, None).unwrap();
        assert_eq!(
            (claude.protocol, claude.header, claude.base.as_str()),
            (
                Protocol::Anthropic,
                KeyHeader::XApiKey,
                "https://api.anthropic.com"
            )
        );
        assert_eq!(claude.key, "sk-stored-0123456789", "stored key used");
        assert_eq!(claude.model.as_deref(), Some("stored-model"));
        let gateway = test(
            HarnessKind::ClaudeCode,
            MODE_CUSTOM,
            Some("https://gateway.test/anthropic/"),
            Some("sk-typed-0123456789"),
        )
        .unwrap();
        assert_eq!(
            (gateway.header, gateway.base.as_str(), gateway.key.as_str()),
            (
                KeyHeader::Bearer,
                "https://gateway.test/anthropic",
                "sk-typed-0123456789"
            )
        );
        let codex = test(HarnessKind::Codex, MODE_API_KEY, None, None).unwrap();
        assert_eq!(
            (codex.protocol, codex.base.as_str()),
            (Protocol::Responses, "https://api.openai.com/v1")
        );
        assert!(test(HarnessKind::Opencode, MODE_API_KEY, None, None).is_err());
        assert!(test(HarnessKind::Opencode, MODE_AGENT_LOGIN, None, None).is_err());
        assert!(test(HarnessKind::Acp, MODE_CUSTOM, Some("http://x.test"), None).is_err());
        assert!(
            test(
                HarnessKind::Codex,
                MODE_CUSTOM,
                Some("http://x.test/v1?k=1"),
                None
            )
            .is_err()
        );
        assert!(
            test_target(
                HarnessKind::Codex,
                None,
                TestAgentService {
                    mode: MODE_API_KEY.into(),
                    ..Default::default()
                }
            )
            .is_err(),
            "no key to test"
        );
    }
    #[test]
    fn a_key_can_come_from_the_environment() {
        let mut named = request(MODE_API_KEY);
        named.api_key_env = Some("CARGO_MANIFEST_DIR".into());
        let stored = update(HarnessKind::Codex, None, named).unwrap().unwrap();
        assert_eq!(stored.api_key, None, "nothing stored");
        let expected = std::env::var("CARGO_MANIFEST_DIR").unwrap();
        let env: BTreeMap<_, _> = stored
            .launch_env(HarnessKind::Codex, None)
            .unwrap()
            .into_iter()
            .collect();
        assert_eq!(env["OPENAI_API_KEY"].as_deref(), Some(expected.as_str()));
        let saved = serde_json::to_string(&stored).unwrap();
        assert!(
            saved.contains("CARGO_MANIFEST_DIR") && !saved.contains(&expected),
            "{saved}"
        );
        assert!(
            stored.secrets().contains(&expected),
            "redacted like a stored key"
        );
        // Omitted, the reference stays; a typed key replaces it.
        let kept = update(HarnessKind::Codex, Some(&stored), request(MODE_API_KEY))
            .unwrap()
            .unwrap();
        assert_eq!(kept.api_key_env.as_deref(), Some("CARGO_MANIFEST_DIR"));
        let mut typed = request(MODE_API_KEY);
        typed.api_key = Some("sk-typed-0123456789".into());
        let replaced = update(HarnessKind::Codex, Some(&stored), typed)
            .unwrap()
            .unwrap();
        assert_eq!(
            (replaced.api_key_env.as_deref(), replaced.api_key.as_deref()),
            (None, Some("sk-typed-0123456789"))
        );
        let mut reserved = request(MODE_API_KEY);
        reserved.api_key_env = Some("HOME".into());
        assert!(update(HarnessKind::Codex, None, reserved).is_err());
        // A missing variable stops the start and names the variable.
        let missing = AgentService {
            mode: MODE_API_KEY.into(),
            api_key_env: Some("NOT_SET_ANYWHERE_0123".into()),
            ..Default::default()
        };
        let error = missing
            .launch_env(HarnessKind::Codex, None)
            .unwrap_err()
            .to_string();
        assert!(error.contains("NOT_SET_ANYWHERE_0123"), "{error}");
        assert_eq!(
            missing.view(Uuid::nil(), HarnessKind::Codex).injected,
            ["OPENAI_API_KEY"]
        );
    }
    #[test]
    fn presets_follow_each_agent_types_protocol() {
        assert_eq!(
            PRESETS.len(),
            31,
            "AgentMeld's 28 and its 3 built-in endpoints"
        );
        let mut ids: Vec<&str> = PRESETS.iter().map(|p| p.id).collect();
        ids.sort();
        ids.dedup();
        assert_eq!(ids.len(), 31, "unique ids");
        assert_eq!(presets_for(HarnessKind::Opencode).len(), 31);
        let claude = presets_for(HarnessKind::ClaudeCode);
        assert!(
            claude.iter().all(|p| p.base_url.contains("/anthropic")),
            "{claude:?}"
        );
        assert!(
            claude
                .iter()
                .any(|p| p.id == "deepseek" && p.base_url == "https://api.deepseek.com/anthropic")
        );
        let codex: Vec<String> = presets_for(HarnessKind::Codex)
            .into_iter()
            .map(|p| p.id)
            .collect();
        assert_eq!(codex, ["openai"]);
        assert!(presets_for(HarnessKind::Pi).is_empty());
        assert!(presets_for(HarnessKind::Acp).is_empty());
        assert!(
            PRESETS
                .iter()
                .all(|p| endpoint(Some(p.chat.into())).is_ok())
        );
    }
}
