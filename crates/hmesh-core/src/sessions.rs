//! Interactive agent sessions driven over ACP. A session runs on the node that
//! owns the agent; it may have been opened there by its owner, by a paired
//! node's owner, or by another agent through `hmesh mcp`.
use crate::{ErrorCode, MeshError, Result};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;
use uuid::Uuid;

/// Maximum number of sessions above a session in its call chain.
pub const MAX_SESSION_DEPTH: usize = 3;
/// Maximum running child sessions one session may have at once.
pub const MAX_RUNNING_CHILDREN: usize = 4;
/// Longest prompt accepted for one turn.
pub const MAX_PROMPT_BYTES: usize = 64 * 1024;
/// Longest text kept in one recorded session event.
pub const MAX_EVENT_TEXT_BYTES: usize = 16 * 1024;
/// Most settings kept for one session, and choices kept per setting.
pub const MAX_CONFIG_OPTIONS: usize = 32;
pub const MAX_CONFIG_CHOICES: usize = 1000;
const MAX_CONFIG_TEXT: usize = 256;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SessionStatus {
    /// A paired node opened it and this node requires local approval first.
    PendingApproval,
    /// Ready for the next prompt.
    Idle,
    /// A turn is in progress.
    Running,
    /// The agent asked for permission and is waiting for an answer.
    WaitingPermission,
    /// Closed by an owner, by pairing revocation, or after rejection.
    Closed,
}
impl SessionStatus {
    pub fn is_open(self) -> bool {
        !matches!(self, Self::Closed)
    }
    pub fn is_busy(self) -> bool {
        matches!(self, Self::Running | Self::WaitingPermission)
    }
}

/// Who opened the session.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OriginKind {
    /// A person, from the panel or a CLI/MCP client they run themselves.
    Human,
    /// Another agent, through the `hmesh mcp` tools of its own session.
    Agent,
}

/// Who answers this session's permission requests.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Approver {
    /// The person who opened it (possibly on another node).
    Origin,
    /// This node's owner.
    Local,
    /// Answered automatically with a one-time allow.
    Auto,
}

/// One session in a delegation chain, root first.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ChainLink {
    pub node_id: Uuid,
    pub agent_id: Uuid,
    pub session_id: Uuid,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SessionOrigin {
    pub node_id: Uuid,
    pub kind: OriginKind,
    /// Readable caller, e.g. `mac-a / Claude Code` for agent-opened sessions.
    #[serde(default)]
    pub label: Option<String>,
    /// Sessions above this one, root first (empty for a person's session).
    #[serde(default)]
    pub chain: Vec<ChainLink>,
    /// The node whose owner started the chain, as this node knows it: itself
    /// for its own sessions, the paired node for sessions a peer opened here.
    /// Only chains this node started may reach other computers.
    pub root_node_id: Uuid,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SessionRecord {
    pub session_id: Uuid,
    /// The node running the agent.
    pub node_id: Uuid,
    pub agent_id: Uuid,
    pub agent_name: String,
    pub project_id: String,
    pub origin: SessionOrigin,
    pub approver: Approver,
    pub status: SessionStatus,
    pub title: String,
    pub turns: u64,
    /// The agent's own session ID, used to load or resume it after restarts.
    #[serde(default)]
    pub agent_session_id: Option<String>,
    #[serde(default)]
    pub last_stop_reason: Option<String>,
    #[serde(default)]
    pub last_final_text: Option<String>,
    #[serde(default)]
    pub last_error: Option<String>,
    /// First message held while the session waits for local approval.
    #[serde(default)]
    pub pending_message: Option<String>,
    /// The settings the agent offers in this session (model, mode, ...), as
    /// it last reported them.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub config_options: Vec<ConfigOption>,
    /// Settings chosen for this session, by setting ID. They are applied
    /// again whenever the agent process restarts, because agents may reset
    /// them when a conversation is reloaded.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub config_values: BTreeMap<String, Value>,
    pub created_at: i64,
    pub updated_at: i64,
    /// The folder the agent works in when it is not the agent's registered
    /// project directory (a project chosen on this computer). Stays on this
    /// computer: [`SessionRecord::for_peer`] removes it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cwd: Option<std::path::PathBuf>,
    /// The session continues one the agent already had; its first reload
    /// keeps the replayed conversation as this session's history.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub history_pending: bool,
}
impl SessionRecord {
    /// This record as the paired computer that opened it may see it: without
    /// the local folder.
    pub fn for_peer(mut self) -> Self {
        self.cwd = None;
        self
    }
    /// This session as the newest link of a chain.
    pub fn link(&self) -> ChainLink {
        ChainLink {
            node_id: self.node_id,
            agent_id: self.agent_id,
            session_id: self.session_id,
        }
    }
    /// The chain a child of this session carries.
    pub fn child_chain(&self) -> Vec<ChainLink> {
        let mut chain = self.origin.chain.clone();
        chain.push(self.link());
        chain
    }
}

/// Refuse delegation chains that are too deep or that return to an agent
/// already working on the same chain.
pub fn check_chain(chain: &[ChainLink], node_id: Uuid, agent_id: Uuid) -> Result<()> {
    if chain.len() > MAX_SESSION_DEPTH {
        return Err(MeshError::new(
            ErrorCode::PermissionDenied,
            format!(
                "delegation is limited to {MAX_SESSION_DEPTH} levels; this agent was asked at level {}",
                chain.len()
            ),
        ));
    }
    if chain
        .iter()
        .any(|link| link.node_id == node_id && link.agent_id == agent_id)
    {
        return Err(MeshError::new(
            ErrorCode::PermissionDenied,
            "this agent is already working on the same delegation chain; calling it again would loop",
        ));
    }
    let mut seen = std::collections::BTreeSet::new();
    if !chain.iter().all(|link| seen.insert(link.session_id)) {
        return Err(MeshError::input("delegation chain repeats a session"));
    }
    Ok(())
}

/// Open a session on a local or paired node's agent.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OpenSession {
    pub agent_id: Uuid,
    /// The node that owns the agent; omitted or this node's ID for local.
    #[serde(default)]
    pub peer_id: Option<Uuid>,
    /// First message; omitted opens an empty session.
    #[serde(default)]
    pub message: Option<String>,
    /// Set by `hmesh mcp` when an agent's session opens a child session.
    #[serde(default)]
    pub parent_session_id: Option<Uuid>,
    /// Settings to use from the first turn on, by setting ID.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub config: BTreeMap<String, Value>,
    /// Work in this project instead of the agent's registered directory: any
    /// project added on this computer, or one a paired computer shares.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project_id: Option<String>,
    /// Continue this session the agent already has (its own session ID, from
    /// the project's history). Only for this computer's owner and agents.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resume: Option<String>,
    /// Title shown for a continued session until its history says otherwise.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
}
/// What a paired node sends to open a session here.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PeerOpenSession {
    pub session_id: Uuid,
    pub agent_id: Uuid,
    pub kind: OriginKind,
    #[serde(default)]
    pub label: Option<String>,
    #[serde(default)]
    pub chain: Vec<ChainLink>,
    #[serde(default)]
    pub message: Option<String>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub config: BTreeMap<String, Value>,
    /// A project this node shares, by ID. Sent only to nodes that list shared
    /// projects; older nodes refuse unknown fields.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project_id: Option<String>,
}
/// A command the running agent advertised for this particular ACP session.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionCommand {
    pub name: String,
    pub description: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input_hint: Option<String>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionCommands {
    pub session_id: Uuid,
    pub harness: crate::HarnessKind,
    pub state: CommandListState,
    pub commands: Vec<SessionCommand>,
    pub config_options: Vec<ConfigOption>,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CommandListState {
    Inactive,
    PendingApproval,
    Closed,
    Unadvertised,
    Ready,
}
#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct RunSessionCommand {
    pub name: String,
    #[serde(default)]
    pub arguments: String,
}

/// Change one session setting between turns.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SetSessionConfig {
    pub config_id: String,
    /// A choice's value for a selector, or true/false for a switch.
    pub value: Value,
}

/// One setting an agent offers for a session (ACP `SessionConfigOption`): a
/// model, mode or reasoning-level selector, or an on/off switch.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ConfigOption {
    pub id: String,
    pub name: String,
    /// `model`, `mode`, `thought_level`, `model_config`, or another value the
    /// agent chose. Used for display only; settings are addressed by ID.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub category: Option<String>,
    /// `select` or `boolean`.
    pub kind: String,
    /// A choice's value for `select`, true or false for `boolean`.
    pub current: Value,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub choices: Vec<ConfigChoice>,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ConfigChoice {
    pub value: String,
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub group: Option<String>,
}
fn short(value: &Value) -> Option<String> {
    let text = value.as_str()?.trim();
    (!text.is_empty()).then(|| clip(text, MAX_CONFIG_TEXT).0)
}
impl ConfigOption {
    /// Read the `configOptions` an agent reported, keeping the kinds this
    /// client understands and bounding their size.
    pub fn parse_all(options: &Value) -> Vec<ConfigOption> {
        let Some(options) = options.as_array() else {
            return vec![];
        };
        options
            .iter()
            .filter_map(Self::parse)
            .take(MAX_CONFIG_OPTIONS)
            .collect()
    }
    fn parse(option: &Value) -> Option<ConfigOption> {
        let id = short(option.get("id")?)?;
        let name = option
            .get("name")
            .and_then(short)
            .unwrap_or_else(|| id.clone());
        let category = option.get("category").and_then(short);
        match option
            .get("type")
            .and_then(Value::as_str)
            .unwrap_or("select")
        {
            "boolean" => Some(ConfigOption {
                id,
                name,
                category,
                kind: "boolean".into(),
                current: Value::Bool(option.get("currentValue")?.as_bool()?),
                choices: vec![],
            }),
            "select" => {
                let current = short(option.get("currentValue")?)?;
                let mut choices = vec![];
                for entry in option.get("options")?.as_array()? {
                    let mut add = |choice: &Value, group: Option<String>| {
                        if choices.len() >= MAX_CONFIG_CHOICES {
                            return;
                        }
                        if let Some(value) = choice.get("value").and_then(short) {
                            let name = choice
                                .get("name")
                                .and_then(short)
                                .unwrap_or_else(|| value.clone());
                            choices.push(ConfigChoice { value, name, group });
                        }
                    };
                    match entry.get("options").and_then(Value::as_array) {
                        Some(grouped) => {
                            let group = entry.get("name").and_then(short);
                            for choice in grouped {
                                add(choice, group.clone());
                            }
                        }
                        None => add(entry, None),
                    }
                }
                Some(ConfigOption {
                    id,
                    name,
                    category,
                    kind: "select".into(),
                    current: Value::String(current),
                    choices,
                })
            }
            _ => None,
        }
    }
    pub fn is_category(&self, category: &str) -> bool {
        self.category.as_deref() == Some(category)
    }
    /// Whether this setting accepts the value.
    pub fn accepts(&self, value: &Value) -> bool {
        match (self.kind.as_str(), value) {
            ("boolean", Value::Bool(_)) => true,
            ("select", Value::String(v)) => self.choices.iter().any(|c| &c.value == v),
            _ => false,
        }
    }
    /// The display name of the current value.
    pub fn current_name(&self) -> String {
        match &self.current {
            Value::String(v) => self
                .choices
                .iter()
                .find(|c| &c.value == v)
                .map_or_else(|| v.clone(), |c| c.name.clone()),
            other => other.to_string(),
        }
    }
}
/// Chosen settings in the order to apply them: the model first, because
/// changing it can add or remove other settings.
pub fn config_apply_order(
    values: &BTreeMap<String, Value>,
    options: &[ConfigOption],
) -> Vec<(String, Value)> {
    let is_model = |id: &str| options.iter().any(|o| o.id == id && o.is_category("model"));
    let mut ordered: Vec<(String, Value)> =
        values.iter().map(|(k, v)| (k.clone(), v.clone())).collect();
    ordered.sort_by_key(|(id, _)| !is_model(id));
    ordered
}
/// Check the shape of requested settings; the agent checks the values.
pub fn validate_config(values: &BTreeMap<String, Value>) -> Result<()> {
    if values.len() > MAX_CONFIG_OPTIONS {
        return Err(MeshError::input("too many session settings"));
    }
    for (id, value) in values {
        validate_config_value(id, value)?;
    }
    Ok(())
}
pub fn validate_config_value(id: &str, value: &Value) -> Result<()> {
    let plain = |text: &str| {
        !text.trim().is_empty()
            && text.len() <= MAX_CONFIG_TEXT
            && !text.chars().any(char::is_control)
    };
    if !plain(id) {
        return Err(MeshError::input("invalid session setting ID"));
    }
    match value {
        Value::Bool(_) => Ok(()),
        Value::String(text) if plain(text) => Ok(()),
        _ => Err(MeshError::input(
            "a session setting value must be a choice's value or true/false",
        )),
    }
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PromptSession {
    pub message: String,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PermissionAnswer {
    pub allow: bool,
}
pub fn validate_prompt(message: &str) -> Result<()> {
    if message.trim().is_empty() {
        return Err(MeshError::input("message must not be empty"));
    }
    if message.len() > MAX_PROMPT_BYTES {
        return Err(MeshError::input(format!(
            "message exceeds {} KiB",
            MAX_PROMPT_BYTES / 1024
        )));
    }
    Ok(())
}

/// One recorded session event. `kind` is one of: `user_message`,
/// `agent_message_chunk`, `agent_thought_chunk`, `tool_call`,
/// `tool_call_update`, `plan`, `permission_request`, `permission_resolved`,
/// `config_changed`, `usage` (what the agent reported about model use in a
/// turn), `turn_end`, `status`, `error`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SessionEvent {
    pub session_id: Uuid,
    pub seq: u64,
    pub created_at: i64,
    pub kind: String,
    pub payload: Value,
}
/// A page of events after `after`, with the session's current state.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SessionEventPage {
    pub session: SessionRecord,
    pub events: Vec<SessionEvent>,
    /// Highest sequence number returned (or the requested `after`).
    pub next: u64,
}

/// Cut text to a byte budget on a character boundary.
pub fn clip(text: &str, limit: usize) -> (String, bool) {
    if text.len() <= limit {
        return (text.to_owned(), false);
    }
    let mut end = limit;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    (text[..end].to_owned(), true)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn link(agent: Uuid) -> ChainLink {
        ChainLink {
            node_id: Uuid::nil(),
            agent_id: agent,
            session_id: Uuid::new_v4(),
        }
    }
    #[test]
    fn chains_are_bounded_and_never_return_to_an_agent_on_them() {
        let target = Uuid::new_v4();
        let chain: Vec<ChainLink> = (0..MAX_SESSION_DEPTH)
            .map(|_| link(Uuid::new_v4()))
            .collect();
        assert!(check_chain(&chain, Uuid::nil(), target).is_ok());
        let mut deeper = chain.clone();
        deeper.push(link(Uuid::new_v4()));
        let error = check_chain(&deeper, Uuid::nil(), target).unwrap_err();
        assert!(error.message.contains("levels"), "{}", error.message);
        let error = check_chain(&chain, Uuid::nil(), chain[1].agent_id).unwrap_err();
        assert!(error.message.contains("loop"), "{}", error.message);
        // The same agent ID on another node is a different agent.
        assert!(check_chain(&chain, Uuid::new_v4(), chain[1].agent_id).is_ok());
        let mut repeated = chain[..2].to_vec();
        let mut again = repeated[0].clone();
        again.agent_id = Uuid::new_v4();
        repeated.push(again);
        assert!(check_chain(&repeated, Uuid::nil(), target).is_err());
    }
    #[test]
    fn prompts_and_event_text_are_bounded_on_character_boundaries() {
        assert!(validate_prompt(" ").is_err());
        assert!(validate_prompt(&"x".repeat(MAX_PROMPT_BYTES + 1)).is_err());
        assert!(validate_prompt("hello").is_ok());
        let (text, cut) = clip("中文", 4);
        assert_eq!((text.as_str(), cut), ("中", true));
    }
    #[test]
    fn agent_settings_are_read_from_acp_config_options() {
        let reported = serde_json::json!([
            {"id":"model","name":"Model","category":"model","type":"select","currentValue":"a/small",
             "options":[{"group":"a","name":"Provider A","options":[{"value":"a/small","name":"Small"},{"value":"a/large","name":"Large"}]}]},
            {"id":"mode","name":"Mode","category":"mode","currentValue":"build",
             "options":[{"value":"build","name":"Build"},{"value":"plan","name":"Plan"}]},
            {"id":"fast","name":"Fast","type":"boolean","currentValue":false},
            {"id":"future","name":"Text","type":"text","currentValue":"x"},
            {"name":"no id","currentValue":"x","options":[]}
        ]);
        let options = ConfigOption::parse_all(&reported);
        assert_eq!(
            options.iter().map(|o| o.id.as_str()).collect::<Vec<_>>(),
            ["model", "mode", "fast"]
        );
        let model = &options[0];
        assert_eq!(model.choices[1].group.as_deref(), Some("Provider A"));
        assert!(model.accepts(&serde_json::json!("a/large")));
        assert!(!model.accepts(&serde_json::json!("b/other")));
        assert!(!model.accepts(&serde_json::json!(true)));
        assert_eq!(model.current_name(), "Small");
        assert_eq!(options[1].kind, "select", "type defaults to select");
        assert!(options[2].accepts(&serde_json::json!(true)));
        // The model goes first: changing it can change the other settings.
        let chosen = BTreeMap::from([
            ("mode".to_owned(), serde_json::json!("plan")),
            ("model".to_owned(), serde_json::json!("a/large")),
        ]);
        let order: Vec<String> = config_apply_order(&chosen, &options)
            .into_iter()
            .map(|(k, _)| k)
            .collect();
        assert_eq!(order, ["model", "mode"]);
        assert!(validate_config_value("model", &serde_json::json!(3)).is_err());
        assert!(validate_config_value("mo\ndel", &serde_json::json!("x")).is_err());
        assert!(validate_config_value("model", &serde_json::json!("a/large")).is_ok());
    }
}
