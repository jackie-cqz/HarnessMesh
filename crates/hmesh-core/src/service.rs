//! How an agent reaches its model service, chosen by its owner per agent:
//! the agent's own login, an API key, or a custom endpoint. Keys and other
//! secret values stay on the agent's computer; these views only say whether
//! one is stored and show its last characters.
use crate::HarnessKind;
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
    /// The key is read from this variable of the daemon's environment when
    /// the agent starts, instead of being stored.
    #[serde(default)]
    pub api_key_env: Option<String>,
    /// What the agent itself is set up with, from the last check on this
    /// computer.
    #[serde(default)]
    pub own_login: Option<OwnLogin>,
    /// Endpoint presets this agent type can use for a custom endpoint.
    #[serde(default)]
    pub presets: Vec<PresetView>,
}

/// The agent is set up with a login or key of its own.
pub const OWN_CONFIGURED: &str = "configured";
/// The agent reports no login or key of its own.
pub const OWN_NOT_CONFIGURED: &str = "not_configured";
/// Its setup could not be determined.
pub const OWN_UNKNOWN: &str = "unknown";

/// What an agent is set up with on its own, as the agent itself reports it
/// (its ACP `auth/status` or its own read-only status command). HarnessMesh
/// never reads an agent's credential files. Kept on this computer; paired
/// computers see only `state`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OwnLogin {
    /// `configured`, `not_configured` or `unknown`.
    pub state: String,
    /// `subscription`, `api_key`, `gateway`, `cloud`, `credentials` or
    /// `providers`, when known.
    #[serde(default)]
    pub kind: Option<String>,
    /// How the agent describes it, e.g. "Claude subscription" or "ChatGPT".
    #[serde(default)]
    pub label: Option<String>,
    /// More about it, e.g. where an API key comes from or which providers
    /// hold credentials. Never a key or part of one.
    #[serde(default)]
    pub detail: Option<String>,
    /// The account's email, when the agent reports one.
    #[serde(default)]
    pub account: Option<String>,
    /// What answered: `acp auth/status` or the command that was run.
    pub source: String,
    /// Unix seconds.
    pub checked_at: i64,
}
impl OwnLogin {
    pub fn unknown(source: impl Into<String>, why: impl Into<String>, at: i64) -> Self {
        Self {
            state: OWN_UNKNOWN.into(),
            kind: None,
            label: None,
            detail: Some(why.into()),
            account: None,
            source: source.into(),
            checked_at: at,
        }
    }
}

/// A known endpoint, as one agent type would use it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PresetView {
    pub id: String,
    pub name: String,
    pub group: String,
    pub region: String,
    /// The address for this agent type's protocol.
    pub base_url: String,
    /// The environment variable this provider's key usually lives in.
    #[serde(default)]
    pub env_key: Option<String>,
}

/// Endpoint presets: the provider list of AgentMeld (adapted from
/// DeepSeek-Reasonix, MIT; see THIRD_PARTY_NOTICES.md) plus its built-in
/// DeepSeek, OpenAI and Volcano Ark endpoints. Each has the OpenAI Chat
/// Completions address; Anthropic Messages and OpenAI Responses addresses are
/// filled only where the provider's own documentation confirms them. Models
/// are not listed: a connection test asks the provider.
pub struct Preset {
    pub id: &'static str,
    pub name: &'static str,
    pub group: &'static str,
    pub region: &'static str,
    pub chat: &'static str,
    pub anthropic: Option<&'static str>,
    pub responses: Option<&'static str>,
    pub env_key: Option<&'static str>,
}
const fn preset(
    id: &'static str,
    name: &'static str,
    group: &'static str,
    region: &'static str,
    chat: &'static str,
    env_key: &'static str,
) -> Preset {
    Preset {
        id,
        name,
        group,
        region,
        chat,
        anthropic: None,
        responses: None,
        env_key: Some(env_key),
    }
}
const fn with_anthropic(p: Preset, url: &'static str) -> Preset {
    Preset {
        anthropic: Some(url),
        ..p
    }
}
const fn with_responses(p: Preset, url: &'static str) -> Preset {
    Preset {
        responses: Some(url),
        ..p
    }
}
const OFFICIAL: &str = "官方 API";
const PLAN: &str = "Coding Plan";
const ROUTER: &str = "聚合平台";
pub const PRESETS: &[Preset] = &[
    with_anthropic(
        preset(
            "deepseek",
            "DeepSeek",
            OFFICIAL,
            "全球",
            "https://api.deepseek.com/v1",
            "DEEPSEEK_API_KEY",
        ),
        "https://api.deepseek.com/anthropic",
    ),
    with_responses(
        preset(
            "openai",
            "OpenAI",
            OFFICIAL,
            "全球",
            "https://api.openai.com/v1",
            "OPENAI_API_KEY",
        ),
        "https://api.openai.com/v1",
    ),
    preset(
        "volcano-ark",
        "火山方舟",
        OFFICIAL,
        "国内",
        "https://ark.cn-beijing.volces.com/api/v3",
        "ARK_API_KEY",
    ),
    with_anthropic(
        preset(
            "glm-cn",
            "智谱 GLM",
            OFFICIAL,
            "国内",
            "https://open.bigmodel.cn/api/paas/v4",
            "GLM_API_KEY",
        ),
        "https://open.bigmodel.cn/api/anthropic",
    ),
    preset(
        "zai-global",
        "Z.AI",
        OFFICIAL,
        "国际",
        "https://api.z.ai/api/paas/v4",
        "ZAI_API_KEY",
    ),
    preset(
        "kimi-cn",
        "Kimi / Moonshot",
        OFFICIAL,
        "国内",
        "https://api.moonshot.cn/v1",
        "KIMI_API_KEY",
    ),
    with_anthropic(
        preset(
            "kimi-global",
            "Kimi / Moonshot",
            OFFICIAL,
            "国际",
            "https://api.moonshot.ai/v1",
            "MOONSHOT_API_KEY",
        ),
        "https://api.moonshot.ai/anthropic",
    ),
    preset(
        "qwen-cn",
        "通义千问 Qwen",
        OFFICIAL,
        "国内",
        "https://dashscope.aliyuncs.com/compatible-mode/v1",
        "QWEN_API_KEY",
    ),
    with_anthropic(
        preset(
            "qwen-global",
            "通义千问 Qwen",
            OFFICIAL,
            "国际",
            "https://dashscope-intl.aliyuncs.com/compatible-mode/v1",
            "QWEN_API_KEY",
        ),
        "https://dashscope-intl.aliyuncs.com/apps/anthropic",
    ),
    with_anthropic(
        preset(
            "minimax-cn-api",
            "MiniMax",
            OFFICIAL,
            "国内",
            "https://api.minimaxi.com/v1",
            "MINIMAX_API_KEY",
        ),
        "https://api.minimaxi.com/anthropic",
    ),
    with_anthropic(
        preset(
            "minimax-global-api",
            "MiniMax",
            OFFICIAL,
            "国际",
            "https://api.minimax.io/v1",
            "MINIMAX_API_KEY",
        ),
        "https://api.minimax.io/anthropic",
    ),
    preset(
        "mimo-api",
        "小米 MiMo",
        OFFICIAL,
        "全球",
        "https://api.xiaomimimo.com/v1",
        "MIMO_API_KEY",
    ),
    preset(
        "longcat-openai",
        "LongCat",
        OFFICIAL,
        "全球",
        "https://api.longcat.chat/openai/v1",
        "LONGCAT_API_KEY",
    ),
    preset(
        "stepfun-api",
        "阶跃星辰 StepFun",
        OFFICIAL,
        "按量计费",
        "https://api.stepfun.com/v1",
        "STEPFUN_API_KEY",
    ),
    with_anthropic(
        preset(
            "glm-coding-plan-cn",
            "智谱 GLM Coding Plan",
            PLAN,
            "国内",
            "https://open.bigmodel.cn/api/coding/paas/v4",
            "GLM_PLAN_API_KEY",
        ),
        "https://open.bigmodel.cn/api/anthropic",
    ),
    preset(
        "zai-coding-plan-global",
        "Z.AI Coding Plan",
        PLAN,
        "国际",
        "https://api.z.ai/api/coding/paas/v4",
        "ZAI_CODING_API_KEY",
    ),
    preset(
        "qwen-coding-plan-cn",
        "Qwen Coding Plan",
        PLAN,
        "国内",
        "https://coding.dashscope.aliyuncs.com/v1",
        "QWEN_CODING_API_KEY",
    ),
    with_anthropic(
        preset(
            "qwen-coding-plan-global",
            "Qwen Coding Plan",
            PLAN,
            "国际",
            "https://coding-intl.dashscope.aliyuncs.com/v1",
            "QWEN_CODING_API_KEY",
        ),
        "https://coding-intl.dashscope.aliyuncs.com/apps/anthropic",
    ),
    preset(
        "mimo-token-plan-cn",
        "MiMo Token Plan",
        PLAN,
        "中国",
        "https://token-plan-cn.xiaomimimo.com/v1",
        "MIMO_TOKEN_PLAN_API_KEY",
    ),
    preset(
        "mimo-token-plan-sgp",
        "MiMo Token Plan",
        PLAN,
        "新加坡",
        "https://token-plan-sgp.xiaomimimo.com/v1",
        "MIMO_TOKEN_PLAN_API_KEY",
    ),
    preset(
        "mimo-token-plan-ams",
        "MiMo Token Plan",
        PLAN,
        "阿姆斯特丹",
        "https://token-plan-ams.xiaomimimo.com/v1",
        "MIMO_TOKEN_PLAN_API_KEY",
    ),
    preset(
        "stepfun",
        "阶跃星辰 Step Plan",
        PLAN,
        "套餐",
        "https://api.stepfun.com/step_plan/v1",
        "STEPFUN_API_KEY",
    ),
    preset(
        "opencode-go",
        "OpenCode Go",
        PLAN,
        "Chat Completions",
        "https://opencode.ai/zen/go/v1",
        "OPENCODE_GO_API_KEY",
    ),
    preset(
        "scnet",
        "国家超算互联网 SCNet",
        PLAN,
        "国内",
        "https://api.scnet.cn/api/llm/v1",
        "SCNET_API_KEY",
    ),
    preset(
        "token-rhythm",
        "基元律动 Token Rhythm",
        ROUTER,
        "全球",
        "https://tokenrhythm.studio/v1",
        "TOKEN_RHYTHM_API_KEY",
    ),
    preset(
        "novita",
        "NovitaAI",
        ROUTER,
        "全球",
        "https://api.novita.ai/openai/v1",
        "NOVITA_API_KEY",
    ),
    preset(
        "gmi",
        "GMI Cloud",
        ROUTER,
        "全球",
        "https://api.gmi-serving.com/v1",
        "GMI_API_KEY",
    ),
    preset(
        "huggingface",
        "Hugging Face Router",
        ROUTER,
        "全球",
        "https://router.huggingface.co/v1",
        "HF_TOKEN",
    ),
    preset(
        "nvidia",
        "NVIDIA NIM",
        ROUTER,
        "全球",
        "https://integrate.api.nvidia.com/v1",
        "NVIDIA_API_KEY",
    ),
    preset(
        "kilocode",
        "KiloCode",
        ROUTER,
        "自动路由",
        "https://api.kilo.ai/api/gateway",
        "KILOCODE_API_KEY",
    ),
    preset(
        "ollama-cloud",
        "Ollama Cloud",
        ROUTER,
        "云端",
        "https://ollama.com/v1",
        "OLLAMA_API_KEY",
    ),
];

impl Preset {
    /// The address this agent type would use, if the preset has one: Claude
    /// Code speaks only Anthropic Messages, Codex only OpenAI Responses, and
    /// OpenCode OpenAI Chat Completions. Other types take no endpoint.
    pub fn address(&self, harness: HarnessKind) -> Option<&'static str> {
        match harness {
            HarnessKind::ClaudeCode => self.anthropic,
            HarnessKind::Codex => self.responses,
            HarnessKind::Opencode => Some(self.chat),
            _ => None,
        }
    }
}
/// Presets this agent type can use.
pub fn presets_for(harness: HarnessKind) -> Vec<PresetView> {
    PRESETS
        .iter()
        .filter_map(|p| {
            Some(PresetView {
                id: p.id.into(),
                name: p.name.into(),
                group: p.group.into(),
                region: p.region.into(),
                base_url: p.address(harness)?.into(),
                env_key: p.env_key.map(Into::into),
            })
        })
        .collect()
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
    /// Read the key from this variable of the daemon's environment instead
    /// of storing one; any stored key is removed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub api_key_env: Option<String>,
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
    /// Read the key from this variable of the daemon's environment.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub api_key_env: Option<String>,
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
