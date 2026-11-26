//! Give a Pi session the ACP-provided MCP servers.
//!
//! `pi-acp` accepts `mcpServers` in `session/new` but keeps them in its own
//! session state; the `pi` process it spawns never sees them. The
//! `pi-mcp-adapter` extension adds a `--mcp-config` flag, so this module writes
//! the session's server list and a small wrapper that starts `pi` with that
//! flag. The wrapper is passed to `pi-acp` as `PI_ACP_PI_COMMAND`, which is the
//! command it spawns instead of `pi` (with `--mode rpc --no-themes` appended).
//!
//! The bridge is only used when the adapter is installed (detected from
//! `pi --help`), because `--mcp-config` is an unknown flag without it.
use anyhow::{Context, Result};
use serde_json::{Value, json};
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use tokio::process::Command;
use uuid::Uuid;

const MCP_FLAG: &str = "--mcp-config";
const PROBE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(8);

/// Whether this `pi` understands `--mcp-config` (pi-mcp-adapter installed).
/// Probed once per daemon process: `pi --help` loads extensions, so it is not
/// free and the answer does not change while the daemon runs.
pub async fn supported(pi_command: &str) -> bool {
    static SUPPORTED: OnceLock<bool> = OnceLock::new();
    if let Some(known) = SUPPORTED.get() {
        return *known;
    }
    let found = probe(pi_command).await;
    let _ = SUPPORTED.set(found);
    found
}

async fn probe(pi_command: &str) -> bool {
    let run = Command::new(pi_command).arg("--help").output();
    match tokio::time::timeout(PROBE_TIMEOUT, run).await {
        Ok(Ok(output)) => {
            let text = String::from_utf8_lossy(&output.stdout);
            text.contains(MCP_FLAG)
        }
        _ => false,
    }
}

/// Where one session's bridge lives.
pub struct Bridge {
    /// Executable for `PI_ACP_PI_COMMAND`; starts `pi` with the config.
    pub command: PathBuf,
    /// Config for `PI_ACP_MCP_CONFIG`, in pi-mcp-adapter's `mcpServers` form.
    pub config: PathBuf,
}

fn bridge_root(data_dir: &Path, session: Uuid) -> PathBuf {
    data_dir.join("session-mcp").join(session.to_string())
}

/// One standard MCP server entry from an ACP `McpServerStdio`.
fn server_entry(server: &Value) -> Option<(String, Value)> {
    let name = server.get("name")?.as_str()?.to_owned();
    let command = server.get("command")?.as_str()?;
    let args = server
        .get("args")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let mut env = serde_json::Map::new();
    for pair in server
        .get("env")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        if let (Some(name), Some(value)) = (
            pair.get("name").and_then(Value::as_str),
            pair.get("value").and_then(Value::as_str),
        ) {
            env.insert(name.to_owned(), json!(value));
        }
    }
    Some((name, json!({"command": command, "args": args, "env": env})))
}

/// Write the session's MCP config and wrapper. Callers start the agent with
/// `PI_ACP_PI_COMMAND` = `bridge.command` and `PI_ACP_MCP_CONFIG` = `bridge.config`.
pub fn install(
    data_dir: &Path,
    session: Uuid,
    servers: &[Value],
    pi_command: &str,
) -> Result<Bridge> {
    let root = bridge_root(data_dir, session);
    let mut entries = serde_json::Map::new();
    for (name, entry) in servers.iter().filter_map(server_entry) {
        entries.insert(name, entry);
    }
    if entries.is_empty() {
        anyhow::bail!("no MCP servers to bridge");
    }
    let mut builder = std::fs::DirBuilder::new();
    builder.recursive(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        builder.mode(0o700);
    }
    builder.create(&root).context("create MCP bridge folder")?;
    let config = root.join("mcp.json");
    let bytes = serde_json::to_vec_pretty(&json!({"mcpServers": Value::Object(entries)}))?;
    // The owner's other MCP sources stay in effect: this replaces only the
    // adapter's Pi-global layer for the life of this session.
    write_private(&config, &bytes)?;
    let (command, script) = wrapper(&root, pi_command);
    write_private(&command, script.as_bytes())?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&command, std::fs::Permissions::from_mode(0o700))
            .context("mark MCP bridge wrapper executable")?;
    }
    Ok(Bridge { command, config })
}

/// Remove one session's bridge (best effort).
pub fn remove(data_dir: &Path, session: Uuid) {
    let _ = std::fs::remove_dir_all(bridge_root(data_dir, session));
}

fn write_private(path: &Path, bytes: &[u8]) -> Result<()> {
    std::fs::write(path, bytes).with_context(|| format!("write {}", path.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))
            .with_context(|| format!("restrict {}", path.display()))?;
    }
    Ok(())
}

/// The wrapper pi-acp spawns. It receives `--mode rpc --no-themes` and adds
/// the bridge config before them.
fn wrapper(root: &Path, pi_command: &str) -> (PathBuf, String) {
    let quoted = if pi_command.contains([' ', '\t']) {
        format!("'{pi_command}'")
    } else {
        pi_command.to_owned()
    };
    #[cfg(windows)]
    {
        let path = root.join("pi-mcp.cmd");
        let script = format!("@echo off\r\n{quoted} {MCP_FLAG} \"%PI_ACP_MCP_CONFIG%\" %*\r\n");
        (path, script)
    }
    #[cfg(not(windows))]
    {
        let path = root.join("pi-mcp.sh");
        let script = format!(
            "#!/bin/sh\n# Added by HarnessMesh: load this session's MCP servers.\nexec {quoted} {MCP_FLAG} \"$PI_ACP_MCP_CONFIG\" \"$@\"\n"
        );
        (path, script)
    }
}

#[cfg(test)]
mod tests {
    use super::{install, remove};
    use serde_json::json;
    use uuid::Uuid;

    #[test]
    fn writes_the_session_servers_and_a_wrapper() {
        let dir = tempfile::tempdir().unwrap();
        let session = Uuid::new_v4();
        let servers = vec![json!({
            "name":"harnessmesh",
            "command":"/opt/hmesh",
            "args":["--data-dir","/data/node","mcp","--session","abc"],
            "env":[]
        })];
        let bridge = install(dir.path(), session, &servers, "pi").unwrap();
        let config: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&bridge.config).unwrap()).unwrap();
        assert_eq!(
            config["mcpServers"]["harnessmesh"]["command"],
            json!("/opt/hmesh")
        );
        assert_eq!(
            config["mcpServers"]["harnessmesh"]["args"],
            json!(["--data-dir", "/data/node", "mcp", "--session", "abc"])
        );
        let wrapper = std::fs::read_to_string(&bridge.command).unwrap();
        assert!(wrapper.contains("--mcp-config"));
        assert!(wrapper.contains("PI_ACP_MCP_CONFIG"));
        remove(dir.path(), session);
        assert!(!bridge.command.exists());
    }

    #[test]
    fn refuses_an_empty_server_list() {
        let dir = tempfile::tempdir().unwrap();
        assert!(install(dir.path(), Uuid::new_v4(), &[], "pi").is_err());
    }
}
