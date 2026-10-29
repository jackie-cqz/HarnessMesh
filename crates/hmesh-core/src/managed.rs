//! Owner-configured HTTP services, exact SSH forwards and bounded integration runs.
use crate::{MeshError, Result, now};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{collections::BTreeMap, path::PathBuf};
use uuid::Uuid;

fn text(s: &str, limit: usize) -> bool {
    !s.is_empty() && s.len() <= limit && !s.chars().any(char::is_control)
}
fn path(p: &std::path::Path) -> bool {
    p.is_absolute()
        && p.to_str()
            .is_some_and(|s| text(s, 1024) && !s.contains('"'))
}
pub fn lease(seconds: u64) -> Result<()> {
    if !(5..=3600).contains(&seconds) {
        return Err(MeshError::input("lease must be 5..3600 seconds"));
    }
    Ok(())
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ServiceProcess {
    pub executable: PathBuf,
    pub cwd: PathBuf,
    #[serde(default)]
    pub args: Vec<String>,
    #[serde(default)]
    pub env: BTreeMap<String, String>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ServiceSpec {
    pub id: Uuid,
    pub project_id: String,
    pub name: String,
    pub target_port: u16,
    pub allowed_peer_ids: Vec<Uuid>,
    #[serde(default)]
    pub allowed_restart_peer_ids: Vec<Uuid>,
    pub lease_seconds: u64,
    pub health_path: String,
    #[serde(default = "default_http_timeout")]
    pub request_timeout_seconds: u64,
    pub process: Option<ServiceProcess>,
}
impl ServiceSpec {
    pub fn validate(&self) -> Result<()> {
        lease(self.lease_seconds)?;
        if !text(&self.project_id, 128)
            || !text(&self.name, 128)
            || self.target_port == 0
            || self.allowed_peer_ids.len() > 16
            || self
                .allowed_restart_peer_ids
                .iter()
                .any(|p| !self.allowed_peer_ids.contains(p))
            || !http_path(&self.health_path)
            || !(1..=30).contains(&self.request_timeout_seconds)
        {
            return Err(MeshError::input(
                "invalid service project/name/port/peers/health path",
            ));
        }
        if let Some(p) = &self.process
            && (!path(&p.executable)
                || !path(&p.cwd)
                || p.args.len() > 32
                || p.args.iter().any(|a| a.len() > 4096 || a.contains('\0'))
                || p.env.len() > 16
                || p.env.iter().any(|(k, v)| {
                    !text(k, 128) || k.contains('=') || v.len() > 4096 || v.contains('\0')
                }))
        {
            return Err(MeshError::input(
                "invalid owner service process configuration",
            ));
        }
        Ok(())
    }
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "snake_case")]
pub enum ForwardDirection {
    Local,
    Remote,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SshForward {
    pub direction: ForwardDirection,
    pub bind_port: u16,
    pub target_port: u16,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TunnelSpec {
    pub id: Uuid,
    pub project_id: String,
    pub name: String,
    pub ssh_executable: PathBuf,
    pub hostname: String,
    pub user: String,
    pub port: u16,
    pub identity_file: PathBuf,
    pub known_hosts_file: PathBuf,
    pub forwards: Vec<SshForward>,
    pub lease_seconds: u64,
}
impl TunnelSpec {
    pub fn validate(&self) -> Result<()> {
        lease(self.lease_seconds)?;
        if !text(&self.project_id, 128)
            || !text(&self.name, 128)
            || self.port == 0
            || !path(&self.ssh_executable)
            || !path(&self.identity_file)
            || !path(&self.known_hosts_file)
            || self.hostname.is_empty()
            || self.hostname.starts_with('-')
            || self.hostname.len() > 253
            || !self
                .hostname
                .bytes()
                .all(|c| c.is_ascii_alphanumeric() || b".-:".contains(&c))
            || self.user.is_empty()
            || self.user.starts_with('-')
            || self.user.len() > 128
            || !self
                .user
                .bytes()
                .all(|c| c.is_ascii_alphanumeric() || b"._-".contains(&c))
            || self.forwards.is_empty()
            || self.forwards.len() > 2
            || self
                .forwards
                .iter()
                .any(|f| f.bind_port == 0 || f.target_port == 0)
        {
            return Err(MeshError::input(
                "invalid exact-target SSH tunnel configuration",
            ));
        }
        Ok(())
    }
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ConnectSpec {
    pub id: Uuid,
    pub peer_id: Uuid,
    pub service_id: Uuid,
    pub local_port: u16,
    pub lease_seconds: u64,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Managed<T> {
    pub spec: T,
    pub enabled: bool,
    pub expires_at: i64,
    pub revision: u64,
}
impl<T> Managed<T> {
    pub fn active(&self) -> bool {
        self.enabled && self.expires_at > now()
    }
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ResourceStatus {
    pub revision: u64,
    pub state: String,
    pub process: String,
    pub tunnel: String,
    pub http_health: String,
    pub endpoint: Option<String>,
    pub pid: Option<u32>,
    pub starts: u32,
    pub error: Option<String>,
}
impl Default for ResourceStatus {
    fn default() -> Self {
        Self {
            revision: 0,
            state: "pending".into(),
            process: "not_applicable".into(),
            tunnel: "not_applicable".into(),
            http_health: "not_checked".into(),
            endpoint: None,
            pid: None,
            starts: 0,
            error: None,
        }
    }
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ServiceCard {
    pub id: Uuid,
    pub project_id: String,
    pub name: String,
    pub expires_at: i64,
    pub protocol: String,
    pub health: String,
    pub revision: u64,
    #[serde(default = "default_http_timeout")]
    pub request_timeout_seconds: u64,
}
fn default_http_timeout() -> u64 {
    10
}
pub fn http_path(s: &str) -> bool {
    s.starts_with('/')
        && !s.starts_with("//")
        && s.len() <= 2048
        && !s.contains(['\\', '#', '\r', '\n', '\0'])
        && s.bytes().all(|b| b >= 32 && b != 127)
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HttpCall {
    pub method: String,
    pub path: String,
    pub content_type: Option<String>,
    pub body_base64: String,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct HttpReply {
    pub status: u16,
    pub content_type: Option<String>,
    pub body_base64: String,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HttpTest {
    pub name: String,
    pub path: String,
    pub expected_status: u16,
    /// JSON Pointer -> exact expected JSON value. Response bodies are not persisted.
    pub equals: BTreeMap<String, Value>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct IntegrationSpec {
    pub id: Uuid,
    pub connection_id: Uuid,
    pub backend_peer_id: Uuid,
    pub backend_agent_id: Uuid,
    pub backend_project_id: String,
    pub backend_service_id: Uuid,
    pub contract_sha256: String,
    pub contract: String,
    pub contract_revision: u32,
    pub tests: Vec<HttpTest>,
    pub max_fix_cycles: u32,
    pub repair_timeout_seconds: u64,
    pub repair_max_output_bytes: usize,
}
impl IntegrationSpec {
    pub fn validate(&self) -> Result<()> {
        if !text(&self.backend_project_id, 128)
            || self.contract.len() > 32768
            || serde_json::from_str::<Value>(&self.contract).is_err()
            || self.contract_sha256.len() != 64
            || !self.contract_sha256.bytes().all(|b| b.is_ascii_hexdigit())
            || self.contract_revision == 0
            || serde_json::to_vec(&self.tests).map_or(true, |v| v.len() > 16384)
            || self.tests.is_empty()
            || self.tests.len() > 16
            || self.max_fix_cycles > 2
            || !(10..=600).contains(&self.repair_timeout_seconds)
            || !(65536..=crate::MAX_RUN_OUTPUT_BYTES).contains(&self.repair_max_output_bytes)
            || self.tests.iter().any(|t| {
                !text(&t.name, 128)
                    || !http_path(&t.path)
                    || !(100..=599).contains(&t.expected_status)
                    || t.equals.len() > 16
                    || t.equals
                        .keys()
                        .any(|p| !p.starts_with('/') || p.len() > 256)
                    || serde_json::to_vec(&t.equals).map_or(true, |v| v.len() > 8192)
            })
        {
            return Err(MeshError::input("invalid bounded integration contract"));
        }
        Ok(())
    }
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct IntegrationRun {
    pub spec: IntegrationSpec,
    pub state: String,
    pub cycles: u32,
    pub repair_task_ids: Vec<Uuid>,
    pub reports: Vec<Value>,
    pub error: Option<String>,
    pub updated_at: i64,
}
