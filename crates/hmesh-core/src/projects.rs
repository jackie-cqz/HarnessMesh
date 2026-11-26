//! Projects: folders on this computer that sessions can work in, and the
//! sessions agents already keep there (listed through ACP `session/list`).
use crate::{HarnessKind, MeshError, Result};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use uuid::Uuid;

/// Longest project or session name.
pub const MAX_NAME_CHARS: usize = 120;
/// Most projects one computer keeps.
pub const MAX_PROJECTS: usize = 500;
/// Longest agent-side session ID accepted.
pub const MAX_AGENT_SESSION_ID_BYTES: usize = 256;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProjectSource {
    /// Added by this computer's owner.
    Added,
    /// The registered directory of one of this computer's agents.
    Agent,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Project {
    pub project_id: String,
    pub name: String,
    /// The folder, with symbolic links resolved.
    pub path: PathBuf,
    pub source: ProjectSource,
    pub pinned: bool,
    pub hidden: bool,
    /// Paired computers may open sessions here.
    pub shared: bool,
    pub position: i64,
    pub created_at: i64,
    /// This computer's agents registered in this folder.
    #[serde(default)]
    pub agent_ids: Vec<Uuid>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AddProject {
    pub path: PathBuf,
    #[serde(default)]
    pub name: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UpdateProject {
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub pinned: Option<bool>,
    #[serde(default)]
    pub hidden: Option<bool>,
    #[serde(default)]
    pub shared: Option<bool>,
    /// Move this many places up (negative) or down in the project list.
    #[serde(default)]
    pub move_by: Option<i64>,
}

/// A project as paired computers see it: never its path.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SharedProject {
    pub project_id: String,
    pub name: String,
}

/// One session an agent already keeps, as its `session/list` reported it.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AgentSessionInfo {
    pub agent_session_id: String,
    pub cwd: PathBuf,
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub updated_at: Option<String>,
    /// The HarnessMesh session that continues it, if one does.
    #[serde(default)]
    pub session_id: Option<Uuid>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HistoryState {
    Listed,
    /// The agent does not offer `session/list`.
    Unsupported,
    /// Starting the agent or listing failed; `message` says why.
    Failed,
}

/// What one agent reported for a folder (or for all folders).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AgentHistory {
    pub agent_id: Uuid,
    pub agent_name: String,
    pub harness: HarnessKind,
    pub state: HistoryState,
    #[serde(default)]
    pub message: Option<String>,
    pub sessions: Vec<AgentSessionInfo>,
    /// More sessions exist than were read.
    pub truncated: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProjectHistory {
    pub project_id: String,
    pub agents: Vec<AgentHistory>,
    pub observed_at: i64,
}

/// Every local agent's sessions in one folder, before or after it is a
/// project: what adding that folder would bring.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FolderHistory {
    /// The folder as this computer resolves it.
    pub path: PathBuf,
    pub name: String,
    /// Already a project here.
    #[serde(default)]
    pub project_id: Option<String>,
    pub agents: Vec<AgentHistory>,
    pub observed_at: i64,
}

/// A folder that appears in agents' histories, for adding it as a project.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HistoryFolder {
    pub path: PathBuf,
    pub name: String,
    pub sessions: u64,
    #[serde(default)]
    pub latest: Option<String>,
    pub agents: Vec<String>,
    /// Already a project here.
    #[serde(default)]
    pub project_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HistoryFolders {
    pub folders: Vec<HistoryFolder>,
    /// Per agent: whether it could list, without the sessions.
    pub agents: Vec<AgentHistory>,
    pub observed_at: i64,
}

/// This computer's own marks on a session: a new name, pinned, hidden. They
/// never reach the agent or a paired computer.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SessionMark {
    pub session_id: Uuid,
    #[serde(default)]
    pub title: Option<String>,
    pub pinned: bool,
    pub hidden: bool,
    pub updated_at: i64,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UpdateSessionMark {
    /// A new name; an empty string goes back to the session's own title.
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub pinned: Option<bool>,
    #[serde(default)]
    pub hidden: Option<bool>,
}

/// A trimmed, non-empty name without control characters.
pub fn clean_name(name: &str) -> Result<String> {
    let name = name.trim();
    if name.is_empty() {
        return Err(MeshError::input("the name is empty"));
    }
    if name.chars().count() > MAX_NAME_CHARS {
        return Err(MeshError::input(format!(
            "names are at most {MAX_NAME_CHARS} characters"
        )));
    }
    if name.chars().any(char::is_control) {
        return Err(MeshError::input("the name contains control characters"));
    }
    Ok(name.to_owned())
}

/// An agent's own session ID as received from a client.
pub fn validate_agent_session_id(id: &str) -> Result<()> {
    if id.is_empty()
        || id.len() > MAX_AGENT_SESSION_ID_BYTES
        || id.chars().any(|c| c.is_control() || c.is_whitespace())
    {
        return Err(MeshError::input("invalid agent session ID"));
    }
    Ok(())
}

/// A project ID as received from a client or a paired computer.
pub fn validate_project_id(id: &str) -> Result<()> {
    crate::validate_name(id).map_err(|_| MeshError::input("invalid project ID"))
}

/// The last component of a folder, as a default project name.
pub fn folder_name(path: &std::path::Path) -> String {
    path.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .filter(|n| !n.is_empty())
        .unwrap_or_else(|| path.display().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_are_trimmed_and_bounded() {
        assert_eq!(clean_name("  api  ").unwrap(), "api");
        assert!(clean_name("   ").is_err());
        assert!(clean_name("a\u{7}b").is_err());
        assert!(clean_name(&"x".repeat(MAX_NAME_CHARS + 1)).is_err());
        assert!(clean_name(&"项".repeat(MAX_NAME_CHARS)).is_ok());
    }
    #[test]
    fn ids_are_checked() {
        assert!(validate_agent_session_id("ses_dee43bd4bffexpdPEJBTVBua9R").is_ok());
        assert!(validate_agent_session_id("a b").is_err());
        assert!(validate_agent_session_id("").is_err());
        assert!(validate_project_id("demo-api_1.x").is_ok());
        assert!(validate_project_id("../etc").is_err());
        assert!(validate_project_id("/tmp").is_err());
        assert!(validate_project_id("..").is_err());
    }
}
