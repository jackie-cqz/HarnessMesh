//! How an agent reaches its model service, chosen by its owner per agent:
//! the agent's own login, an API key, or a custom endpoint. Keys and other
//! secret values stay on the agent's computer; these views only say whether
//! one is stored and show its last characters.
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use uuid::Uuid;

/// The agent uses its own login and settings; nothing is injected.
pub const MODE_AGENT_LOGIN: &str = "agent_login";
/// An API key for the agent's usual service.
pub const MODE_API_KEY: &str = "api_key";
/// An API key for a custom endpoint (`base_url`).
pub const MODE_CUSTOM: &str = "custom";

/// What the owner sees about one agent's model service.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AgentServiceView {
    pub agent_id: Uuid,
    pub mode: String,
    #[serde(default)]
    pub base_url: Option<String>,
    #[serde(default)]
    pub default_model: Option<String>,
    /// Models offered through a custom endpoint, default model first. Only
    /// OpenCode uses a list; other agents report their own models.
    #[serde(default)]
    pub models: Vec<String>,
    pub api_key: SecretHint,
    #[serde(default)]
    pub env: Vec<EnvHint>,
    /// Modes this agent type supports.
    pub modes: Vec<String>,
    /// Environment variable names HarnessMesh sets when it starts the agent.
    #[serde(default)]
    pub injected: Vec<String>,
}
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SecretHint {
    pub set: bool,
    /// The last four characters, only for values long enough not to give
    /// much away.
    #[serde(default)]
    pub last4: Option<String>,
}
impl SecretHint {
    pub fn of(value: Option<&str>) -> Self {
        match value {
            None => Self::default(),
            Some(value) => {
                let chars: Vec<char> = value.chars().collect();
                Self {
                    set: true,
                    last4: (chars.len() >= 12).then(|| chars[chars.len() - 4..].iter().collect()),
                }
            }
        }
    }
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EnvHint {
    pub name: String,
    pub value: SecretHint,
}

/// Replace one agent's model service settings.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SetAgentService {
    pub mode: String,
    #[serde(default)]
    pub base_url: Option<String>,
    #[serde(default)]
    pub default_model: Option<String>,
    /// The complete model list for a custom endpoint (OpenCode only); the
    /// default model is added when missing.
    #[serde(default)]
    pub models: Vec<String>,
    /// A new key. Omitted keeps the stored key; an empty string removes it.
    #[serde(default)]
    pub api_key: Option<String>,
    /// The complete set of extra environment variables for the agent: a
    /// value sets it, null keeps the stored value, and names not listed are
    /// removed.
    #[serde(default)]
    pub env: BTreeMap<String, Option<String>>,
}

/// Check whether settings reach their model service, before or after saving
/// them. Fields left out are taken from the stored settings.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TestAgentService {
    pub mode: String,
    #[serde(default)]
    pub base_url: Option<String>,
    /// A key typed but not saved yet; omitted uses the stored key.
    #[serde(default)]
    pub api_key: Option<String>,
    /// Model to try when the service cannot list its models.
    #[serde(default)]
    pub model: Option<String>,
}

/// The service answered its model list.
pub const TESTED_BY_MODELS: &str = "models";
/// The service did not list its models; one minimal request was sent.
pub const TESTED_BY_REQUEST: &str = "request";

/// What a connection test found. Never contains the key.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ServiceTestResult {
    pub ok: bool,
    /// `models` or `request`; absent when nothing could be checked.
    #[serde(default)]
    pub method: Option<String>,
    /// The address that was called last.
    pub endpoint: String,
    pub latency_ms: u64,
    /// Models the service listed, when it did.
    #[serde(default)]
    pub models: Vec<String>,
    pub message: String,
}
