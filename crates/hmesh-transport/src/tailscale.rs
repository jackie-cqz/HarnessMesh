//! Read-only Tailscale CLI adapter. Candidates never confer Mesh trust or grants.
use anyhow::{Context, Result, bail};
use hmesh_core::is_tailnet_ip;
use serde::{Deserialize, Serialize};
use std::{collections::BTreeMap, future::Future, net::IpAddr, path::PathBuf, process::Stdio};
use tokio::{
    io::AsyncReadExt,
    process::Command,
    time::{Duration, timeout},
};

const MAX_STATUS_BYTES: u64 = 1024 * 1024;

#[derive(Debug, Clone, Serialize)]
pub struct TailnetNode {
    pub hostname: String,
    pub dns_name: String,
    pub os: String,
    pub ips: Vec<IpAddr>,
    pub online: bool,
}
#[derive(Debug, Clone, Serialize)]
pub struct TailnetStatus {
    pub backend_state: String,
    pub self_node: TailnetNode,
    pub candidates: Vec<TailnetNode>,
}
impl TailnetStatus {
    /// Tailscale runs on this device and is signed in, whether or not it
    /// reaches its coordination server just now.
    pub fn running(&self) -> bool {
        self.backend_state == "Running"
    }
    pub fn require_running(&self) -> Result<()> {
        if !self.running() || !self.self_node.online || self.self_node.ips.is_empty() {
            bail!(
                "Tailscale must be logged in, running and online with a node IP (state: {})",
                self.backend_state
            );
        }
        Ok(())
    }
    pub fn require_local_ip(&self, ip: IpAddr) -> Result<()> {
        self.require_running()?;
        if !is_tailnet_ip(ip) || !self.self_node.ips.contains(&ip) {
            bail!(
                "configured listener is not this device's current Tailscale IP; initialize a fresh instance after an IP change"
            );
        }
        Ok(())
    }
    pub fn preferred_local_ip(&self) -> Result<IpAddr> {
        self.require_running()?;
        self.self_node
            .ips
            .iter()
            .find(|ip| ip.is_ipv4())
            .or(self.self_node.ips.first())
            .copied()
            .context("Tailscale has no supported node IP")
    }
}

pub trait TailscaleProvider {
    fn status(&self) -> impl Future<Output = Result<TailnetStatus>> + Send;
}
pub struct CliTailscale {
    executable: PathBuf,
}
impl CliTailscale {
    pub fn discover(explicit: Option<&std::path::Path>) -> Result<Self> {
        if let Some(path) = explicit {
            if !path.is_absolute() || !path.is_file() {
                bail!("Tailscale CLI must be an existing absolute executable path");
            }
            return Ok(Self {
                executable: path.to_owned(),
            });
        }
        let mut paths = vec![];
        if cfg!(windows) {
            if let Some(dir) = std::env::var_os("ProgramFiles") {
                paths.push(PathBuf::from(dir).join("Tailscale/tailscale.exe"));
            }
        } else if cfg!(target_os = "macos") {
            paths.extend(
                [
                    "/usr/local/bin/tailscale",
                    "/opt/homebrew/bin/tailscale",
                    "/Applications/Tailscale.app/Contents/MacOS/Tailscale",
                ]
                .map(PathBuf::from),
            );
        }
        if let Some(path) = std::env::var_os("PATH") {
            paths.extend(
                std::env::split_paths(&path)
                    .filter(|p| p.is_absolute())
                    .map(|p| {
                        p.join(if cfg!(windows) {
                            "tailscale.exe"
                        } else {
                            "tailscale"
                        })
                    }),
            );
        }
        let executable = paths
            .into_iter()
            .find(|path| path.is_file())
            .context("Tailscale CLI not found; set --tailscale-executable to its absolute path")?;
        Ok(Self { executable })
    }
    /// This device's own status (`--peers=false`): whether Tailscale runs
    /// here, and its addresses, without the rest of the tailnet, which can
    /// be large and slow to list.
    pub async fn own_status(&self) -> Result<TailnetStatus> {
        self.status_with(&["status", "--json", "--peers=false"])
            .await
    }
    async fn status_with(&self, args: &[&str]) -> Result<TailnetStatus> {
        let mut command = Command::new(&self.executable);
        command
            .args(args)
            .env("TAILSCALE_BE_CLI", "1")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .kill_on_drop(true);
        #[cfg(windows)]
        command.creation_flags(0x08000000);
        let mut child = command
            .spawn()
            .context("start read-only Tailscale status probe")?;
        let stdout = child
            .stdout
            .take()
            .context("Tailscale stdout unavailable")?;
        timeout(Duration::from_secs(5), async {
            let mut bytes = Vec::new();
            stdout.take(MAX_STATUS_BYTES + 1).read_to_end(&mut bytes).await?;
            if bytes.len() as u64 > MAX_STATUS_BYTES { bail!("Tailscale status exceeds 1 MiB"); }
            if !child.wait().await?.success() { bail!("Tailscale status failed; check local CLI access and sign-in with tailscale status"); }
            parse_status(&bytes)
        }).await.context("Tailscale status timed out after 5 seconds")?
    }
}
impl TailscaleProvider for CliTailscale {
    async fn status(&self) -> Result<TailnetStatus> {
        self.status_with(&["status", "--json"]).await
    }
}

#[derive(Default, Deserialize)]
#[serde(default, rename_all = "PascalCase")]
struct WireNode {
    host_name: String,
    #[serde(rename = "DNSName")]
    dns_name: String,
    #[serde(rename = "OS")]
    os: String,
    #[serde(rename = "TailscaleIPs")]
    ips: Vec<IpAddr>,
    online: bool,
}
impl From<WireNode> for TailnetNode {
    fn from(node: WireNode) -> Self {
        Self {
            hostname: node.host_name,
            dns_name: node.dns_name.trim_end_matches('.').to_owned(),
            os: node.os,
            ips: node
                .ips
                .into_iter()
                .filter(|ip| is_tailnet_ip(*ip))
                .collect(),
            online: node.online,
        }
    }
}
#[derive(Deserialize)]
#[serde(rename_all = "PascalCase")]
struct WireStatus {
    backend_state: String,
    #[serde(rename = "Self")]
    self_node: Option<WireNode>,
    peer: Option<BTreeMap<String, WireNode>>,
}
pub fn parse_status(bytes: &[u8]) -> Result<TailnetStatus> {
    if bytes.len() as u64 > MAX_STATUS_BYTES {
        bail!("Tailscale status exceeds 1 MiB");
    }
    let wire: WireStatus = serde_json::from_slice(bytes).context(
        "unsupported Tailscale status JSON; expected BackendState, Self and optional Peer map",
    )?;
    let mut candidates: Vec<TailnetNode> = wire
        .peer
        .unwrap_or_default()
        .into_values()
        .map(Into::into)
        .filter(|node: &TailnetNode| !node.ips.is_empty())
        .collect();
    candidates.sort_by(|a, b| a.dns_name.cmp(&b.dns_name).then(a.ips.cmp(&b.ips)));
    Ok(TailnetStatus {
        backend_state: wire.backend_state,
        self_node: wire.self_node.unwrap_or_default().into(),
        candidates,
    })
}
