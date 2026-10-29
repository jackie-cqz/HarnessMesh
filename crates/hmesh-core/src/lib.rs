//! Harness-independent contracts. No network, process or credential access.
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

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum HarnessKind {
    Pi,
    Dsh,
    Opencode,
}
impl fmt::Display for HarnessKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Pi => "pi",
            Self::Dsh => "dsh",
            Self::Opencode => "opencode",
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
            requires_approval: true,
        }
    }
    pub fn validate(&self) -> Result<()> {
        if self.goal.trim().is_empty() || self.goal.len() > MAX_GOAL_BYTES {
            return Err(MeshError::input("goal must contain 1..65536 UTF-8 bytes"));
        }
        validate_name(&self.project_id)?;
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
    pub grant: Vec<Grant>,
}
impl Default for Policy {
    fn default() -> Self {
        Self {
            default: DefaultPolicy {
                action: "deny".into(),
            },
            grant: vec![],
        }
    }
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
}
impl Default for NodeOptions {
    fn default() -> Self {
        Self {
            name: "local-node".into(),
            listen_address: IpAddr::V4(Ipv4Addr::LOCALHOST),
            listen_port: 38880,
            network: "loopback".into(),
            expose_to_tailnet: false,
        }
    }
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct RuntimeOptions {
    pub max_concurrent_runs: usize,
    pub max_queue_depth: usize,
}
impl Default for RuntimeOptions {
    fn default() -> Self {
        Self {
            max_concurrent_runs: 1,
            max_queue_depth: 20,
        }
    }
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct SecurityOptions {
    pub require_mutual_auth: bool,
    pub policy_file: PathBuf,
    pub default_action: String,
}
impl Default for SecurityOptions {
    fn default() -> Self {
        Self {
            require_mutual_auth: true,
            policy_file: "policy.toml".into(),
            default_action: "deny".into(),
        }
    }
}
impl NodeConfig {
    pub fn validate(&self) -> Result<()> {
        validate_name(&self.node.name)?;
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
        if !self.node.listen_address.is_loopback()
            || self.node.network != "loopback"
            || self.node.expose_to_tailnet
        {
            return Err(MeshError::input(
                "V0 permits only explicit loopback listeners",
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
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AdapterOptions {
    pub executable: PathBuf,
    #[serde(default)]
    pub node_executable: Option<PathBuf>,
    #[serde(default)]
    pub models_file: Option<PathBuf>,
    #[serde(default)]
    pub model: Option<String>,
    #[serde(default)]
    pub provider: Option<String>,
    #[serde(default)]
    pub credential_source: Option<PathBuf>,
    #[serde(default)]
    pub env_allowlist: Vec<String>,
    #[serde(default)]
    pub mesh_server: Option<PathBuf>,
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
    pub fn validate(&self) -> Result<()> {
        validate_name(&self.agent.name)?;
        validate_name(&self.agent.project_id)?;
        if self.adapter.mesh_server.is_some() != self.adapter.mesh_scope.is_some()
            || (self.adapter.mesh_server.is_some() && self.agent.harness != HarnessKind::Pi)
        {
            return Err(MeshError::input(
                "Mesh tools require both owner-selected paths and the Pi adapter",
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
