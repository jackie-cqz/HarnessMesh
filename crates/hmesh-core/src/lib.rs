//! Harness-independent contracts. No network, process or credential access.
mod network;
pub use network::is_tailnet_ip;
pub mod development;
pub mod managed;
pub mod sessions;
use serde::{Deserialize, Serialize};
use std::{
    fmt,
    net::{IpAddr, Ipv4Addr},
    path::PathBuf,
};
use uuid::Uuid;

pub const PROTOCOL: &str = "hmesh/1";
pub const MAX_GOAL_BYTES: usize = 65536;
pub const MAX_RUN_OUTPUT_BYTES: usize = 32 * 1024 * 1024;
pub const MAX_EVENT_PAYLOAD_BYTES: usize = 64 * 1024;
pub type Result<T> = std::result::Result<T, MeshError>;

pub fn now() -> i64 {
    time::OffsetDateTime::now_utc().unix_timestamp()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum ErrorCode {
    InvalidInput,
    Unauthenticated,
    PermissionDenied,
    PeerUnreachable,
    AgentUnavailable,
    AdapterNotSupported,
    ProjectNotFound,
    ApprovalRequired,
    TaskConflict,
    TaskUnknown,
    ServiceExpired,
    ServiceUnhealthy,
    UnsupportedVersion,
    RateLimited,
    FailedNeedsInput,
    Internal,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MeshError {
    pub code: ErrorCode,
    pub message: String,
    pub retryable: bool,
    pub trace_id: Uuid,
}
impl MeshError {
    pub fn new(code: ErrorCode, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
            retryable: false,
            trace_id: Uuid::new_v4(),
        }
    }
    pub fn input(message: impl Into<String>) -> Self {
        Self::new(ErrorCode::InvalidInput, message)
    }
}
impl fmt::Display for MeshError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:?}: {}", self.code, self.message)
    }
}
impl std::error::Error for MeshError {}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum TaskStatus {
    Created,
    PendingApproval,
    Queued,
    Running,
    WaitingInput,
    Cancelling,
    Succeeded,
    Failed,
    Cancelled,
    Rejected,
    Expired,
    Unknown,
}
impl TaskStatus {
    pub fn is_terminal(self) -> bool {
        matches!(
            self,
            Self::Succeeded
                | Self::Failed
                | Self::Cancelled
                | Self::Rejected
                | Self::Expired
                | Self::Unknown
        )
    }
    pub fn can_transition_to(self, next: Self) -> bool {
        use TaskStatus::*;
        matches!(
            (self, next),
            (Created, PendingApproval | Queued | Expired)
                | (PendingApproval, Queued | Rejected | Expired | Cancelled)
                | (Queued, Running | Expired | Cancelled)
                | (
                    Running,
                    Succeeded | Failed | Cancelling | WaitingInput | Unknown
                )
                | (WaitingInput, Running | Failed | Cancelling | Unknown)
                | (Cancelling, Cancelled | Unknown)
        )
    }
}

/// Which agent an Agent registration launches. Every kind is driven over the
/// Agent Client Protocol (ACP); the kind only selects default launch arguments
/// and how the agent is labelled. `acp` is any other ACP agent.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum HarnessKind {
    /// Pi through the `pi-acp` adapter.
    Pi,
    /// DSH has no ACP mode; kept so older registrations still load.
    Dsh,
    /// OpenCode's built-in `opencode acp`.
    Opencode,
    /// Claude Code through the `claude-agent-acp` adapter.
    #[serde(rename = "claude-code")]
    ClaudeCode,
    /// Codex through the `codex-acp` adapter.
    Codex,
    /// Any other ACP agent; `adapter.args` gives its launch arguments.
    Acp,
}
impl HarnessKind {
    /// Arguments that start the agent in ACP mode when the configuration does
    /// not list its own.
    pub fn default_acp_args(self) -> &'static [&'static str] {
        match self {
            Self::Opencode => &["acp"],
            _ => &[],
        }
    }
}
impl fmt::Display for HarnessKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Pi => "pi",
            Self::Dsh => "dsh",
            Self::Opencode => "opencode",
            Self::ClaudeCode => "claude-code",
            Self::Codex => "codex",
            Self::Acp => "acp",
        })
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NodeCard {
    pub version: String,
    pub node_id: Uuid,
    pub display_name: String,
    pub fingerprint: String,
    pub certificate_pem: String,
    pub endpoint: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Envelope<T> {
    pub version: String,
    pub request_id: Uuid,
    pub sender_node_id: Uuid,
    pub timestamp: i64,
    pub operation: String,
    pub payload: T,
    pub trace_id: Uuid,
}
impl<T> Envelope<T> {
    pub fn new(sender: Uuid, operation: &str, payload: T) -> Self {
        Self {
            version: PROTOCOL.into(),
            request_id: Uuid::new_v4(),
            sender_node_id: sender,
            timestamp: now(),
            operation: operation.into(),
            payload,
            trace_id: Uuid::new_v4(),
        }
    }
    pub fn validate(&self, sender: Uuid, operation: &str) -> Result<()> {
        if self.version != PROTOCOL {
            return Err(MeshError::new(
                ErrorCode::UnsupportedVersion,
                "unsupported protocol version",
            ));
        }
        if self.sender_node_id != sender {
            return Err(MeshError::new(
                ErrorCode::Unauthenticated,
                "sender does not match TLS identity",
            ));
        }
        if self.operation != operation {
            return Err(MeshError::input("operation does not match route"));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RunLimits {
    pub timeout_seconds: u64,
    pub max_output_bytes: usize,
}
impl Default for RunLimits {
    fn default() -> Self {
        Self {
            timeout_seconds: 600,
            max_output_bytes: 1048576,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TaskInput {
    pub r#type: String,
    pub name: String,
    pub value: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TaskSubmit {
    pub task_id: Uuid,
    pub target_agent_id: Uuid,
    pub project_id: String,
    pub goal: String,
    #[serde(default)]
    pub inputs: Vec<TaskInput>,
    #[serde(default)]
    pub limits: RunLimits,
    #[serde(default = "yes")]
    pub requires_approval: bool,
    /// A restriction, never a tool grant. Omitted on ordinary/legacy requests.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expected_tools: Option<Vec<String>>,
    /// Required completion artifacts. This restricts a task; it never grants file access.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expected_artifacts: Option<Vec<PathBuf>>,
}
fn yes() -> bool {
    true
}
impl TaskSubmit {
    pub fn new(agent: Uuid, project: &str, goal: &str) -> Self {
        Self {
            task_id: Uuid::new_v4(),
            target_agent_id: agent,
            project_id: project.into(),
            goal: goal.into(),
            inputs: vec![],
            limits: RunLimits::default(),
            // The executor decides from its per-peer access mode. A sender may
            // still opt in to local confirmation by setting this to true.
            requires_approval: false,
            expected_tools: None,
            expected_artifacts: None,
        }
    }
    pub fn validate(&self) -> Result<()> {
        if self.goal.trim().is_empty() || self.goal.len() > MAX_GOAL_BYTES {
            return Err(MeshError::input("goal must contain 1..65536 UTF-8 bytes"));
        }
        validate_name(&self.project_id)?;
        if let Some(paths) = &self.expected_artifacts {
            let unique: std::collections::BTreeSet<_> = paths.iter().collect();
            if paths.is_empty()
                || paths.len() > 16
                || unique.len() != paths.len()
                || paths.iter().any(|p| {
                    p.as_os_str().is_empty()
                        || p.components()
                            .any(|c| !matches!(c, std::path::Component::Normal(_)))
                        || p.to_str().is_none_or(|s| {
                            s.len() > 1024
                                || s.contains(['\\', ':'])
                                || s.chars().any(char::is_control)
                        })
                })
            {
                return Err(MeshError::input(
                    "expected artifacts require 1..16 unique portable relative file paths",
                ));
            }
        }
        if let Some(tools) = &self.expected_tools
            && (tools.len() > 32 || tools.iter().any(|t| validate_name(t).is_err()))
        {
            return Err(MeshError::input("invalid expected tools"));
        }
        if !(1..=1800).contains(&self.limits.timeout_seconds)
            || !(1..=MAX_RUN_OUTPUT_BYTES).contains(&self.limits.max_output_bytes)
        {
            return Err(MeshError::input("task limits exceed local bounds"));
        }
        if self.inputs.len() > 16
            || self
                .inputs
                .iter()
                .any(|i| i.r#type != "text" || i.name.len() > 128 || i.value.len() > MAX_GOAL_BYTES)
        {
            return Err(MeshError::input("unsupported or oversized task inputs"));
        }
        Ok(())
    }
}
pub fn validate_name(name: &str) -> Result<()> {
    if name.is_empty()
        || name.len() > 128
        || !name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.'))
        || matches!(name, "." | "..")
    {
        return Err(MeshError::input(
            "name must be 1..128 ASCII letters, digits, dots, hyphens or underscores",
        ));
    }
    Ok(())
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TaskRecord {
    pub task_id: Uuid,
    pub origin_node_id: Uuid,
    pub target_node_id: Uuid,
    pub agent_id: Uuid,
    pub project_id: String,
    pub status: TaskStatus,
    pub created_at: i64,
    pub updated_at: i64,
    pub last_attempt_id: Option<Uuid>,
    pub request: TaskSubmit,
    pub error: Option<MeshError>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TaskEvent {
    pub task_id: Uuid,
    pub seq: i64,
    pub event_id: Uuid,
    pub attempt_id: Option<Uuid>,
    pub event_type: String,
    pub payload: serde_json::Value,
    pub created_at: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Policy {
    pub default: DefaultPolicy,
    /// Legacy per-project grants. Paired peers no longer need them; they are
    /// still parsed so existing policy files keep loading.
    pub grant: Vec<Grant>,
    /// Pairing is the trust boundary. This only decides whether a paired
    /// peer's task starts immediately or waits for local confirmation.
    pub peers: PeerAccessPolicy,
}
impl Default for Policy {
    fn default() -> Self {
        Self {
            default: DefaultPolicy {
                action: "deny".into(),
            },
            grant: vec![],
            peers: PeerAccessPolicy::default(),
        }
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PeerAccess {
    /// Tasks from this paired peer start without a local approval step.
    #[default]
    Auto,
    /// Tasks from this paired peer wait in PENDING_APPROVAL for the local owner.
    Confirm,
}
impl fmt::Display for PeerAccess {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Auto => "auto",
            Self::Confirm => "confirm",
        })
    }
}
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct PeerAccessPolicy {
    /// Applies to every paired peer without an override.
    pub default: PeerAccess,
    /// Per-peer overrides keyed by node ID.
    pub overrides: std::collections::BTreeMap<Uuid, PeerAccess>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DefaultPolicy {
    pub action: String,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Grant {
    pub peer: Uuid,
    pub project: String,
    #[serde(default)]
    pub agent: Option<Uuid>,
    pub actions: Vec<String>,
    #[serde(default = "yes")]
    pub require_approval: bool,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Decision {
    Denied,
    ApprovalRequired,
    Allowed,
}
impl Policy {
    pub fn validate(&self) -> Result<()> {
        if self.default.action != "deny" {
            return Err(MeshError::input("default policy must deny"));
        }
        for g in &self.grant {
            validate_name(&g.project)?;
            if g.actions.is_empty()
                || g.actions
                    .iter()
                    .any(|a| !matches!(a.as_str(), "agent.read" | "task.submit"))
            {
                return Err(MeshError::input("unsupported policy action"));
            }
        }
        Ok(())
    }
    /// Access mode for one paired peer.
    pub fn peer_access(&self, peer: Uuid) -> PeerAccess {
        self.peers
            .overrides
            .get(&peer)
            .copied()
            .unwrap_or(self.peers.default)
    }
    /// Decision for a task from an already authenticated, trusted peer.
    pub fn peer_decision(&self, peer: Uuid) -> Decision {
        match self.peer_access(peer) {
            PeerAccess::Auto => Decision::Allowed,
            PeerAccess::Confirm => Decision::ApprovalRequired,
        }
    }
    /// Legacy project-grant evaluation; no longer used for paired peers.
    pub fn decide(&self, peer: Uuid, project: &str, agent: Uuid, action: &str) -> Decision {
        let grants: Vec<_> = self
            .grant
            .iter()
            .filter(|g| {
                g.peer == peer
                    && g.project == project
                    && g.agent.is_none_or(|id| id == agent)
                    && g.actions.iter().any(|a| a == action)
            })
            .collect();
        if grants.is_empty() {
            Decision::Denied
        } else if grants.iter().any(|g| g.require_approval) {
            Decision::ApprovalRequired
        } else {
            Decision::Allowed
        }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct NodeConfig {
    pub node: NodeOptions,
    pub runtime: RuntimeOptions,
    pub security: SecurityOptions,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct NodeOptions {
    pub name: String,
    pub listen_address: IpAddr,
    pub listen_port: u16,
    pub network: String,
    pub expose_to_tailnet: bool,
    pub tailscale_executable: Option<PathBuf>,
}
impl Default for NodeOptions {
    fn default() -> Self {
        Self {
            name: "local-node".into(),
            listen_address: IpAddr::V4(Ipv4Addr::LOCALHOST),
            listen_port: 38880,
            network: "loopback".into(),
            expose_to_tailnet: false,
            tailscale_executable: None,
        }
    }
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct RuntimeOptions {
    pub max_concurrent_runs: usize,
    pub max_queue_depth: usize,
    pub project_limits: std::collections::BTreeMap<String, QueueLimits>,
    pub agent_limits: std::collections::BTreeMap<Uuid, QueueLimits>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct QueueLimits {
    pub max_concurrent_runs: usize,
    pub max_queue_depth: usize,
}
impl Default for QueueLimits {
    fn default() -> Self {
        Self {
            max_concurrent_runs: 1,
            max_queue_depth: 20,
        }
    }
}
impl Default for RuntimeOptions {
    fn default() -> Self {
        Self {
            max_concurrent_runs: 1,
            max_queue_depth: 20,
            project_limits: Default::default(),
            agent_limits: Default::default(),
        }
    }
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct SecurityOptions {
    pub require_mutual_auth: bool,
    pub policy_file: PathBuf,
    pub default_action: String,
    pub pending_approval_ttl_seconds: u64,
}
impl Default for SecurityOptions {
    fn default() -> Self {
        Self {
            require_mutual_auth: true,
            policy_file: "policy.toml".into(),
            default_action: "deny".into(),
            pending_approval_ttl_seconds: 86400,
        }
    }
}
impl NodeConfig {
    pub fn validate(&self) -> Result<()> {
        validate_name(&self.node.name)?;
        if !(1..=604800).contains(&self.security.pending_approval_ttl_seconds) {
            return Err(MeshError::input(
                "pending approval TTL must be 1..604800 seconds",
            ));
        }
        if self
            .security
            .policy_file
            .components()
            .next()
            .is_some_and(|part| part.as_os_str() == "~")
        {
            return Err(MeshError::input(
                "policy_file must be absolute or relative to the instance; ~ is not expanded",
            ));
        }
        let valid_network = match self.node.network.as_str() {
            "loopback" => self.node.listen_address.is_loopback() && !self.node.expose_to_tailnet,
            "tailscale" => is_tailnet_ip(self.node.listen_address) && self.node.expose_to_tailnet,
            _ => false,
        };
        if !valid_network {
            return Err(MeshError::input(
                "use loopback, or explicitly enable tailscale with one literal Tailnet IP; wildcard/LAN/public listeners are forbidden",
            ));
        }
        if self
            .node
            .tailscale_executable
            .as_ref()
            .is_some_and(|p| !p.is_absolute())
        {
            return Err(MeshError::input(
                "tailscale_executable must be an absolute local path",
            ));
        }
        if !self.security.require_mutual_auth || self.security.default_action != "deny" {
            return Err(MeshError::input(
                "mutual authentication and default deny are required",
            ));
        }
        if self.runtime.max_concurrent_runs == 0
            || self.runtime.max_concurrent_runs > 16
            || self.runtime.max_queue_depth == 0
            || self.runtime.max_queue_depth > 1000
        {
            return Err(MeshError::input("invalid local runtime limits"));
        }
        for name in self.runtime.project_limits.keys() {
            validate_name(name)?;
        }
        for limit in self
            .runtime
            .project_limits
            .values()
            .chain(self.runtime.agent_limits.values())
        {
            if !(1..=16).contains(&limit.max_concurrent_runs)
                || !(1..=1000).contains(&limit.max_queue_depth)
            {
                return Err(MeshError::input("invalid project/Agent queue limits"));
            }
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AgentConfig {
    pub agent: AgentOptions,
    pub project: ProjectOptions,
    pub adapter: AdapterOptions,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AgentOptions {
    pub name: String,
    pub harness: HarnessKind,
    pub project_id: String,
    #[serde(default = "managed")]
    pub mode: String,
    #[serde(default)]
    pub display_capabilities: Vec<String>,
}
fn managed() -> String {
    "managed".into()
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProjectOptions {
    pub root: PathBuf,
    pub allowed_tools: Vec<String>,
    #[serde(default = "yes")]
    pub require_remote_task_approval: bool,
    #[serde(default)]
    pub artifact_paths: Vec<PathBuf>,
    #[serde(default = "shared_workspace")]
    pub workspace_mode: String,
    #[serde(default)]
    pub git_executable: Option<PathBuf>,
}
fn shared_workspace() -> String {
    "shared".into()
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AdapterOptions {
    pub executable: PathBuf,
    /// Launch arguments; omitted means the harness kind's ACP default
    /// (`opencode acp`, none for adapters such as `claude-agent-acp`).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub args: Vec<String>,
    #[serde(default)]
    pub node_executable: Option<PathBuf>,
    /// Retired: only the old Pi adapter used it. Rejected by `validate`.
    #[serde(default)]
    pub models_file: Option<PathBuf>,
    /// Informational; ACP agents use the model chosen in their own settings.
    #[serde(default)]
    pub model: Option<String>,
    /// Informational; ACP agents use the provider chosen in their own settings.
    #[serde(default)]
    pub provider: Option<String>,
    /// Retired: only the old Pi adapter used it. Rejected by `validate`.
    #[serde(default)]
    pub credential_source: Option<PathBuf>,
    /// Environment variables whose values are redacted from output and must be
    /// present before a run starts. The agent inherits the daemon environment.
    #[serde(default)]
    pub env_allowlist: Vec<String>,
    /// Retired: sessions now receive `hmesh mcp` automatically.
    #[serde(default)]
    pub mesh_server: Option<PathBuf>,
    /// Retired: sessions now receive `hmesh mcp` automatically.
    #[serde(default)]
    pub mesh_scope: Option<PathBuf>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MeshTarget {
    pub peer_id: Uuid,
    pub agent_id: Uuid,
    pub project_id: String,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MeshToolScope {
    pub targets: Vec<MeshTarget>,
    pub max_submissions: usize,
    pub max_timeout_seconds: u64,
    pub max_output_bytes: usize,
}
impl MeshToolScope {
    pub fn validate(&self) -> Result<()> {
        if self.targets.is_empty()
            || self.targets.len() > 16
            || !(1..=4).contains(&self.max_submissions)
            || !(1..=600).contains(&self.max_timeout_seconds)
            || !(1..=MAX_RUN_OUTPUT_BYTES).contains(&self.max_output_bytes)
        {
            return Err(MeshError::input("invalid owner-selected Mesh tool scope"));
        }
        for target in &self.targets {
            validate_name(&target.project_id)?;
        }
        Ok(())
    }
}
impl AgentConfig {
    /// Arguments that start this agent in ACP mode.
    pub fn launch_args(&self) -> Vec<String> {
        if self.adapter.args.is_empty() {
            self.agent
                .harness
                .default_acp_args()
                .iter()
                .map(|a| (*a).to_owned())
                .collect()
        } else {
            self.adapter.args.clone()
        }
    }
    pub fn validate(&self) -> Result<()> {
        validate_name(&self.agent.name)?;
        validate_name(&self.agent.project_id)?;
        if self.agent.harness == HarnessKind::Dsh {
            return Err(MeshError::input(
                "DSH has no Agent Client Protocol mode; register an ACP agent such as OpenCode or Claude Code",
            ));
        }
        if self.adapter.credential_source.is_some() || self.adapter.models_file.is_some() {
            return Err(MeshError::input(
                "credential_source and models_file belonged to the retired Pi adapter; ACP agents use their own login and model settings",
            ));
        }
        if self.adapter.mesh_server.is_some() || self.adapter.mesh_scope.is_some() {
            return Err(MeshError::input(
                "mesh_server and mesh_scope are retired; every session receives hmesh mcp automatically",
            ));
        }
        if self.adapter.args.len() > 32
            || self
                .adapter
                .args
                .iter()
                .any(|a| a.len() > 1024 || a.contains('\0'))
        {
            return Err(MeshError::input(
                "at most 32 launch arguments of up to 1 KiB each are allowed",
            ));
        }
        if self.agent.mode != "managed"
            || !self.project.root.is_absolute()
            || !self.adapter.executable.is_absolute()
        {
            return Err(MeshError::input(
                "managed mode and absolute local project/executable paths required",
            ));
        }
        if self.project.allowed_tools.is_empty() {
            return Err(MeshError::input("an explicit tool allowlist is required"));
        }
        if self
            .project
            .allowed_tools
            .iter()
            .any(|t| !["read", "write", "edit", "bash", "grep", "find", "ls"].contains(&t.as_str()))
        {
            return Err(MeshError::input("unsupported tool in project allowlist"));
        }
        let unique: std::collections::BTreeSet<_> = self.project.artifact_paths.iter().collect();
        if unique.len() != self.project.artifact_paths.len() {
            return Err(MeshError::input("artifact paths must be unique"));
        }
        if !matches!(
            self.project.workspace_mode.as_str(),
            "shared" | "git-worktree"
        ) || (self.project.workspace_mode == "git-worktree"
            && self
                .project
                .git_executable
                .as_ref()
                .is_none_or(|p| !p.is_absolute()))
            || self
                .project
                .git_executable
                .as_ref()
                .is_some_and(|p| !p.is_absolute())
        {
            return Err(MeshError::input(
                "workspace mode must be shared or git-worktree with an absolute Git executable",
            ));
        }
        if self.project.artifact_paths.len() > 16
            || self.project.artifact_paths.iter().any(|p| {
                p.as_os_str().is_empty()
                    || p.components()
                        .any(|c| !matches!(c, std::path::Component::Normal(_)))
            })
        {
            return Err(MeshError::input(
                "at most 16 relative artifact paths without traversal are allowed",
            ));
        }
        if [
            &self.adapter.node_executable,
            &self.adapter.models_file,
            &self.adapter.credential_source,
            &self.adapter.mesh_server,
            &self.adapter.mesh_scope,
        ]
        .into_iter()
        .flatten()
        .any(|path| !path.is_absolute())
        {
            return Err(MeshError::input(
                "adapter Node, model and credential file paths must be absolute",
            ));
        }
        if self.adapter.env_allowlist.iter().any(|e| {
            e.is_empty()
                || !e
                    .bytes()
                    .all(|b| b.is_ascii_uppercase() || b.is_ascii_digit() || b == b'_')
        }) {
            return Err(MeshError::input("invalid environment allowlist"));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProbeReport {
    pub installed: bool,
    pub version: Option<String>,
    pub version_supported: bool,
    pub protocol_probe: String,
    pub authentication: String,
    pub managed_run: String,
    pub streaming: String,
    pub cancel: String,
    pub session_resume: String,
    pub unavailable_reason: Option<String>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AgentCard {
    pub agent_id: Uuid,
    pub node_id: Uuid,
    pub name: String,
    pub harness: HarnessKind,
    pub project_ids: Vec<String>,
    pub availability: String,
    pub capabilities: Vec<String>,
    pub probe: ProbeReport,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Peer {
    pub card: NodeCard,
    pub status: String,
    pub approved_at: Option<i64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PairChallenge {
    pub nonce: Uuid,
    pub node_id: Uuid,
    pub expires_at: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NodeConnectivity {
    Online,
    Unreachable,
    NotProbed,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PeerView {
    #[serde(flatten)]
    pub peer: Peer,
    pub node_connectivity: NodeConnectivity,
    pub registry_readable: bool,
    pub visible_agent_count: Option<usize>,
    pub runnable_agent_count: Option<usize>,
    pub services_status: String,
    /// What this node does with tasks sent by the peer.
    #[serde(default)]
    pub access: PeerAccess,
}
/// Largest final reply kept in one result event; the full text stays in events.
pub const MAX_FINAL_TEXT_BYTES: usize = 32 * 1024;
/// The last assistant reply of a task, without replaying every event.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TaskResult {
    pub task: TaskRecord,
    pub final_text: Option<String>,
    #[serde(default)]
    pub truncated: bool,
    /// `result_event`, `assembled_from_events` (older executors) or `none`.
    pub source: String,
}
/// One node in the owner's merged agent directory.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MeshNode {
    pub node_id: Uuid,
    pub name: String,
    pub local: bool,
    pub reachable: bool,
    /// For peers: what this node does with tasks the peer sends here.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub access: Option<PeerAccess>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    pub agents: Vec<AgentCard>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PeerAccessRequest {
    pub access: PeerAccess,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PairSubmission {
    pub request_id: Uuid,
    pub challenge: PairChallenge,
    pub card: NodeCard,
    pub signature: String,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PairRecord {
    pub request_id: Uuid,
    pub card: NodeCard,
    pub status: String,
    pub direction: String,
    pub expires_at: i64,
}
