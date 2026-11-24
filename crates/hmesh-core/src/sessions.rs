//! Interactive agent sessions driven over ACP. A session runs on the node that
//! owns the agent; it may have been opened there by its owner, by a paired
//! node's owner, or by another agent through `hmesh mcp`.
use crate::{ErrorCode, MeshError, Result};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use uuid::Uuid;

/// Maximum number of sessions above a session in its call chain.
pub const MAX_SESSION_DEPTH: usize = 3;
/// Maximum running child sessions one session may have at once.
pub const MAX_RUNNING_CHILDREN: usize = 4;
/// Longest prompt accepted for one turn.
pub const MAX_PROMPT_BYTES: usize = 64 * 1024;
/// Longest text kept in one recorded session event.
pub const MAX_EVENT_TEXT_BYTES: usize = 16 * 1024;

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
    /// The agent's own session ID, used with `session/load` after restarts.
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
    pub created_at: i64,
    pub updated_at: i64,
}
impl SessionRecord {
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
/// `turn_end`, `status`, `error`.
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
}
