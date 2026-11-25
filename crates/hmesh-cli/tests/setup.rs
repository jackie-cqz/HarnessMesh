//! `hmesh setup --mcp-only` and the MCP server starting its daemon, with a
//! fake `claude` command and a throwaway home directory. No Tailscale (the
//! Tailnet address is given), no model and no real agent.
#![cfg(unix)]
use serde_json::{Value, json};
use std::{
    io::Write,
    os::unix::fs::PermissionsExt,
    path::PathBuf,
    process::{Command, Output, Stdio},
};

/// A home directory with Codex's folder and a fake `claude` that records
/// what it is asked and knows whether HarnessMesh was added.
struct Home {
    _root: tempfile::TempDir,
    home: PathBuf,
    bin: PathBuf,
    node: PathBuf,
    /// The networks this computer seems to be on (`HMESH_TEST_NETWORKS`).
    networks: Option<PathBuf>,
}
impl Home {
    fn new() -> Self {
        let root = tempfile::tempdir().unwrap();
        let home = root.path().join("home");
        let bin = root.path().join("bin");
        std::fs::create_dir_all(home.join(".codex")).unwrap();
        std::fs::write(home.join(".codex/config.toml"), "model = \"kept\"\n").unwrap();
        std::fs::create_dir_all(&bin).unwrap();
        let claude = bin.join("claude");
        std::fs::write(
            &claude,
            "#!/bin/sh\necho \"$@\" >> \"$HOME/claude.log\"\nif [ \"$1 $2\" = \"mcp get\" ]; then [ -f \"$HOME/claude-added\" ] && exit 0; exit 1; fi\nif [ \"$1 $2\" = \"mcp add\" ]; then touch \"$HOME/claude-added\"; echo \"Added stdio MCP server harnessmesh\"; fi\nexit 0\n",
        )
        .unwrap();
        std::fs::set_permissions(&claude, std::fs::Permissions::from_mode(0o755)).unwrap();
        let node = root.path().join("node");
        Self {
            _root: root,
            home,
            bin,
            node,
            networks: None,
        }
    }
    fn hmesh(&self, args: &[&str], input: Option<&str>) -> Output {
        let mut child = Command::new(env!("CARGO_BIN_EXE_hmesh"))
            .arg("--data-dir")
            .arg(&self.node)
            .args(args)
            .env("HOME", &self.home)
            .env_remove("CODEX_HOME")
            .env("PATH", format!("{}:/usr/bin:/bin", self.bin.display()))
            .envs(
                self.networks
                    .iter()
                    .map(|path| ("HMESH_TEST_NETWORKS", path)),
            )
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        let mut stdin = child.stdin.take().unwrap();
        stdin
            .write_all(input.unwrap_or_default().as_bytes())
            .unwrap();
        drop(stdin);
        child.wait_with_output().unwrap()
    }
    fn setup(&self, extra: &[&str]) -> Value {
        let mut args = vec![
            "--json",
            "setup",
            "--mcp-only",
            "--tailnet-address",
            "100.64.0.9",
            "--no-startup",
            "--name",
            "probe",
            // Tests run side by side; each node gets a free port.
            "--port",
            "0",
        ];
        args.extend_from_slice(extra);
        let output = self.hmesh(&args, None);
        assert!(
            output.status.success(),
            "setup failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        serde_json::from_slice(&output.stdout).unwrap()
    }
    fn stop(&self) {
        let _ = self.hmesh(&["daemon", "stop"], None);
        for _ in 0..100 {
            if !self.node.join("daemon.json").exists() {
                return;
            }
            std::thread::sleep(std::time::Duration::from_millis(100));
        }
    }
    fn state(summary: &Value, client: &str) -> String {
        summary["mcp_clients"]
            .as_array()
            .unwrap()
            .iter()
            .find(|c| c["client"] == client)
            .map(|c| c["state"].as_str().unwrap().to_owned())
            .unwrap_or_default()
    }
}
impl Drop for Home {
    fn drop(&mut self) {
        self.stop();
    }
}

#[test]
fn mcp_only_setup_creates_an_outbound_node_and_connects_both_clients_once() {
    let home = Home::new();
    let summary = home.setup(&["--connect", "claude,codex"]);
    assert_eq!(summary["mode"], "mcp-only");
    assert_eq!(summary["outbound_only"], true);
    assert_eq!(Home::state(&summary, "claude"), "connected");
    assert_eq!(Home::state(&summary, "codex"), "connected");

    // The node listens for its owner only, and its daemon runs.
    let config: toml::Table =
        toml::from_str(&std::fs::read_to_string(home.node.join("node.toml")).unwrap()).unwrap();
    assert_eq!(config["node"]["network"].as_str(), Some("tailscale"));
    assert_eq!(config["node"]["expose_to_tailnet"].as_bool(), Some(false));
    assert_eq!(config["node"]["listen_address"].as_str(), Some("127.0.0.1"));
    assert_eq!(
        config["node"]["tailnet_address"].as_str(),
        Some("100.64.0.9")
    );
    let status = home.hmesh(&["--json", "daemon", "status"], None);
    let status: Value = serde_json::from_slice(&status.stdout).unwrap();
    assert_eq!(status["outbound_only"], true);

    // Both clients run this hmesh with this node directory.
    let hmesh = std::fs::canonicalize(env!("CARGO_BIN_EXE_hmesh")).unwrap();
    let node = std::fs::canonicalize(&home.node).unwrap();
    let log = std::fs::read_to_string(home.home.join("claude.log")).unwrap();
    let added: Vec<&str> = log.lines().filter(|l| l.starts_with("mcp add")).collect();
    assert_eq!(added.len(), 1, "{log}");
    let program = added[0]
        .strip_prefix("mcp add -s user harnessmesh -- ")
        .and_then(|rest| rest.strip_suffix(&format!(" mcp --data-dir {}", node.display())))
        .unwrap_or_else(|| panic!("unexpected claude call: {}", added[0]));
    assert_eq!(std::fs::canonicalize(program).unwrap(), hmesh);
    let codex: toml::Table =
        toml::from_str(&std::fs::read_to_string(home.home.join(".codex/config.toml")).unwrap())
            .unwrap();
    assert_eq!(codex["model"].as_str(), Some("kept"));
    let entry = &codex["mcp_servers"]["harnessmesh"];
    assert_eq!(
        std::fs::canonicalize(entry["command"].as_str().unwrap()).unwrap(),
        hmesh
    );
    assert_eq!(
        entry["args"]
            .as_array()
            .unwrap()
            .iter()
            .map(|a| a.as_str().unwrap())
            .collect::<Vec<_>>(),
        ["mcp", "--data-dir", &node.display().to_string()]
    );
    assert_eq!(entry["tool_timeout_sec"].as_integer(), Some(660));

    // Again: nothing is added twice.
    let again = home.setup(&["--connect", "claude,codex"]);
    assert_eq!(Home::state(&again, "claude"), "already");
    assert_eq!(Home::state(&again, "codex"), "already");
    let log = std::fs::read_to_string(home.home.join("claude.log")).unwrap();
    assert_eq!(log.lines().filter(|l| l.starts_with("mcp add")).count(), 1);
    let text = std::fs::read_to_string(home.home.join(".codex/config.toml")).unwrap();
    assert_eq!(
        text.matches("[mcp_servers.harnessmesh]").count(),
        1,
        "{text}"
    );
}

#[test]
fn without_consent_no_client_is_touched_and_a_mode_is_required() {
    let home = Home::new();
    // No mode and nobody to ask: refused before anything is created.
    let refused = home.hmesh(
        &["--json", "setup", "--tailnet-address", "100.64.0.9"],
        None,
    );
    assert!(!refused.status.success());
    assert!(!home.node.join("node.toml").exists());
    // Without --connect and without a person to ask, clients are left alone.
    let summary = home.setup(&[]);
    assert_eq!(Home::state(&summary, "claude"), "skipped");
    assert_eq!(Home::state(&summary, "codex"), "skipped");
    // So is login startup without --startup, even without --no-startup.
    let output = home.hmesh(
        &[
            "--json",
            "setup",
            "--mcp-only",
            "--tailnet-address",
            "100.64.0.9",
            "--name",
            "probe",
            "--port",
            "0",
        ],
        None,
    );
    assert!(output.status.success());
    let summary: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert!(summary["startup"].is_null(), "{summary}");
    assert!(!home.home.join("Library/LaunchAgents").exists());
    assert!(!home.node.join("startup.json").exists());
    assert!(!home.home.join("claude-added").exists());
    assert!(
        !std::fs::read_to_string(home.home.join(".codex/config.toml"))
            .unwrap()
            .contains("harnessmesh")
    );
    // --connect none is accepted; an unknown client is not.
    home.setup(&["--connect", "none"]);
    let unknown = home.hmesh(
        &[
            "--json",
            "setup",
            "--mcp-only",
            "--tailnet-address",
            "100.64.0.9",
            "--no-startup",
            "--connect",
            "cursor",
        ],
        None,
    );
    assert!(!unknown.status.success());
}

impl Home {
    /// One MCP tool call through `hmesh mcp`, as a client would make it.
    fn list_agents_through_mcp(&self) -> Value {
        let input = format!(
            "{}\n{}\n{}\n",
            json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-11-25","capabilities":{},"clientInfo":{"name":"fixture","version":"1"}}}),
            json!({"jsonrpc":"2.0","method":"notifications/initialized"}),
            json!({"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"mesh_list_agents","arguments":{}}})
        );
        let output = self.hmesh(&["mcp"], Some(&input));
        assert!(output.status.success());
        let records: Vec<Value> = String::from_utf8(output.stdout)
            .unwrap()
            .lines()
            .map(|l| serde_json::from_str(l).unwrap())
            .collect();
        records.iter().find(|r| r["id"] == 2).unwrap()["result"].clone()
    }
    fn daemon_pid(&self) -> Option<u64> {
        let status = self.hmesh(&["--json", "daemon", "status"], None);
        serde_json::from_slice::<Value>(&status.stdout).ok()?["pid"].as_u64()
    }
    fn kill(pid: u64, signal: &str) {
        assert!(
            Command::new("kill")
                .args([signal, &pid.to_string()])
                .status()
                .unwrap()
                .success()
        );
    }
}

#[test]
fn the_mcp_server_starts_the_daemon_of_a_node_set_up_here() {
    let home = Home::new();
    home.setup(&["--connect", "none"]);
    home.stop();
    assert!(!home.node.join("daemon.json").exists());
    let listed = home.list_agents_through_mcp();
    assert_ne!(listed["isError"], true, "{listed}");
    let first = home
        .daemon_pid()
        .expect("the MCP server started the daemon");

    // Killed outright, the daemon leaves daemon.json behind; the MCP server
    // asks the daemon itself and starts a new one.
    Home::kill(first, "-KILL");
    for _ in 0..50 {
        if home.daemon_pid().is_none() {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
    assert!(home.node.join("daemon.json").exists());
    let listed = home.list_agents_through_mcp();
    assert_ne!(listed["isError"], true, "{listed}");
    let second = home.daemon_pid().expect("started again");
    assert_ne!(second, first);

    // Asked to stop with SIGTERM, as a login item or service manager does,
    // it shuts down cleanly and removes daemon.json.
    Home::kill(second, "-TERM");
    for _ in 0..100 {
        if !home.node.join("daemon.json").exists() {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
    assert!(!home.node.join("daemon.json").exists());
}

#[test]
fn a_daemon_that_cannot_start_is_reported_at_once_with_its_reason() {
    let home = Home::new();
    home.setup(&["--connect", "none"]);
    // Full needs Tailscale at start; this one does not exist.
    let started = std::time::Instant::now();
    let output = home.hmesh(
        &[
            "--json",
            "setup",
            "--full",
            "--tailnet-address",
            "100.64.0.9",
            "--tailscale-executable",
            "/nonexistent/tailscale",
            "--no-startup",
            "--connect",
            "none",
            "--port",
            "0",
        ],
        None,
    );
    assert!(!output.status.success());
    let error: Value = serde_json::from_slice(&output.stderr).unwrap();
    let message = error["error"]["message"].as_str().unwrap();
    assert!(message.contains("the daemon stopped"), "{message}");
    assert!(message.contains("Tailscale"), "{message}");
    assert!(started.elapsed() < std::time::Duration::from_secs(15));
}

#[test]
fn a_symlinked_codex_config_stays_a_link_and_clients_get_an_absolute_node_dir() {
    let home = Home::new();
    // Codex's config.toml kept elsewhere (dotfiles) and linked in.
    let dotfiles = home.home.join("dotfiles");
    std::fs::create_dir_all(&dotfiles).unwrap();
    let real = dotfiles.join("codex.toml");
    std::fs::rename(home.home.join(".codex/config.toml"), &real).unwrap();
    std::os::unix::fs::symlink(&real, home.home.join(".codex/config.toml")).unwrap();
    // A relative --data-dir, from the directory that holds the node.
    let parent = home.node.parent().unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_hmesh"))
        .current_dir(parent)
        .args(["--data-dir", "node", "--json", "setup", "--mcp-only"])
        .args([
            "--tailnet-address",
            "100.64.0.9",
            "--no-startup",
            "--port",
            "0",
        ])
        .args(["--connect", "codex"])
        .env("HOME", &home.home)
        .env_remove("CODEX_HOME")
        .env("PATH", format!("{}:/usr/bin:/bin", home.bin.display()))
        .stdin(Stdio::null())
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let link = home.home.join(".codex/config.toml");
    assert!(
        std::fs::symlink_metadata(&link)
            .unwrap()
            .file_type()
            .is_symlink()
    );
    let codex: toml::Table = toml::from_str(&std::fs::read_to_string(&real).unwrap()).unwrap();
    assert_eq!(codex["model"].as_str(), Some("kept"));
    let args: Vec<&str> = codex["mcp_servers"]["harnessmesh"]["args"]
        .as_array()
        .unwrap()
        .iter()
        .map(|a| a.as_str().unwrap())
        .collect();
    let node = std::fs::canonicalize(&home.node).unwrap();
    assert_eq!(args, ["mcp", "--data-dir", &node.display().to_string()]);
}

impl Home {
    fn setup_with(&self, args: &[&str]) -> Value {
        let mut all = vec!["--json", "setup", "--no-startup", "--connect", "none"];
        all.extend_from_slice(&["--name", "probe", "--port", "0"]);
        all.extend_from_slice(args);
        let output = self.hmesh(&all, None);
        assert!(
            output.status.success(),
            "setup failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        serde_json::from_slice(&output.stdout).unwrap()
    }
    fn config(&self) -> toml::Table {
        toml::from_str(&std::fs::read_to_string(self.node.join("node.toml")).unwrap()).unwrap()
    }
}

#[test]
fn without_tailscale_only_mcp_connects_out_over_this_computers_own_network() {
    let home = Home::new();
    // No Tailscale here (forced, so a developer machine with it behaves the same).
    let summary = home.setup_with(&[
        "--mcp-only",
        "--tailscale-executable",
        "/nonexistent/tailscale",
    ]);
    assert_eq!(summary["network"], "direct");
    assert_eq!(summary["outbound_only"], true);
    assert!(summary["tailnet_address"].is_null());
    let config = home.config();
    assert_eq!(config["node"]["network"].as_str(), Some("direct"));
    assert_eq!(config["node"]["expose_to_tailnet"].as_bool(), Some(false));
    assert_eq!(config["node"]["listen_address"].as_str(), Some("127.0.0.1"));
    let status = home.hmesh(&["--json", "daemon", "status"], None);
    let status: Value = serde_json::from_slice(&status.stdout).unwrap();
    assert_eq!(status["outbound_only"], true);
    assert_eq!(status["pairing_window"]["required"], false);
}

#[test]
fn full_on_a_direct_network_listens_on_the_given_address_and_switches_back() {
    let home = Home::new();
    // Loopback stands in for this computer's LAN address.
    let summary = home.setup_with(&[
        "--full",
        "--network",
        "direct",
        "--listen",
        "127.0.0.1",
        "--allow-from",
        "127.0.0.0/8",
    ]);
    assert_eq!(summary["network"], "direct");
    assert_eq!(summary["outbound_only"], false);
    assert_eq!(summary["listen_address"], "127.0.0.1");
    let config = home.config();
    assert_eq!(config["node"]["expose_to_tailnet"].as_bool(), Some(true));
    assert_eq!(
        config["node"]["allowed_sources"]
            .as_array()
            .map(|a| a.iter().map(|v| v.as_str().unwrap()).collect::<Vec<_>>()),
        Some(vec!["127.0.0.0/8"])
    );
    let status = home.hmesh(&["--json", "daemon", "status"], None);
    let status: Value = serde_json::from_slice(&status.stdout).unwrap();
    assert_eq!(status["network"], "direct");
    assert_eq!(status["pairing_window"]["required"], true);
    let fingerprint = summary["node"]["fingerprint"].clone();

    // Not an address of this computer: refused before anything changes.
    let refused = home.hmesh(
        &[
            "--json",
            "setup",
            "--full",
            "--no-startup",
            "--connect",
            "none",
            "--network",
            "direct",
            "--listen",
            "192.0.2.10",
        ],
        None,
    );
    assert!(!refused.status.success());
    assert!(
        String::from_utf8_lossy(&refused.stderr).contains("not an address of this computer"),
        "{}",
        String::from_utf8_lossy(&refused.stderr)
    );
    assert_eq!(
        home.config()["node"]["listen_address"].as_str(),
        Some("127.0.0.1")
    );

    // Back to only MCP on Tailscale: same identity, pairings would stay.
    let back = home.setup_with(&["--mcp-only", "--tailnet-address", "100.64.0.9"]);
    assert_eq!(back["network"], "tailscale");
    assert_eq!(back["node"]["fingerprint"], fingerprint);
    assert!(home.config()["node"].get("allowed_sources").is_none());
    // Said, not silent: the narrowed sources are gone.
    assert!(
        back["steps"]
            .as_array()
            .unwrap()
            .iter()
            .any(|s| s["text"].as_str().unwrap().contains("127.0.0.0/8")),
        "{back}"
    );
}

impl Home {
    /// A Tailscale CLI here that reports `state`.
    fn tailscale(&self, state: &str) -> String {
        let path = self.bin.join(format!("tailscale-{state}"));
        std::fs::write(
            &path,
            format!(
                "#!/bin/sh\necho '{{\"BackendState\":\"{state}\",\"Self\":{{\"HostName\":\"probe\",\"TailscaleIPs\":[\"100.64.0.9\"],\"Online\":true}}}}'\n"
            ),
        )
        .unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        path.display().to_string()
    }
}

/// Run again without naming a network, a node keeps the one it has, with
/// its address and sources, even with Tailscale running here now.
#[test]
fn setup_again_keeps_the_network_address_and_sources_it_has() {
    let home = Home::new();
    home.setup_with(&[
        "--full",
        "--network",
        "direct",
        "--listen",
        "127.0.0.1",
        "--allow-from",
        "127.0.0.0/8",
    ]);
    let before = home.config();
    let running = home.tailscale("Running");
    let again = home.setup_with(&["--full", "--tailscale-executable", &running]);
    assert_eq!(again["network"], "direct", "{again}");
    let after = home.config();
    for key in [
        "network",
        "listen_address",
        "expose_to_tailnet",
        "allowed_sources",
    ] {
        assert_eq!(after["node"].get(key), before["node"].get(key), "{key}");
    }
    assert!(after["node"].get("tailnet_address").is_none());
}

/// Tailscale installed but signed out counts as not here: only MCP connects
/// out over this computer's own network instead of failing.
#[test]
fn only_mcp_with_tailscale_signed_out_connects_out_directly() {
    let home = Home::new();
    let signed_out = home.tailscale("NeedsLogin");
    let summary = home.setup_with(&["--mcp-only", "--tailscale-executable", &signed_out]);
    assert_eq!(summary["network"], "direct", "{summary}");
    assert_eq!(summary["outbound_only"], true);
}

/// A new address (the fix for a DHCP change) keeps a narrowed source range:
/// it is never widened to every private network without being asked.
#[test]
fn a_new_address_keeps_the_narrowed_sources() {
    let home = Home::new();
    // Set up for an address this computer does not have (192.0.2.0/24 is
    // for documentation), as after its lease changed.
    let output = home.hmesh(
        &[
            "--json",
            "init",
            "--name",
            "probe",
            "--port",
            "0",
            "--network",
            "direct",
            "--listen-address",
            "192.0.2.10",
            "--allow-from",
            "192.0.2.0/24",
        ],
        None,
    );
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    home.setup_with(&["--full", "--listen", "127.0.0.1"]);
    let config = home.config();
    assert_eq!(config["node"]["listen_address"].as_str(), Some("127.0.0.1"));
    assert_eq!(
        config["node"]["allowed_sources"]
            .as_array()
            .map(|a| a.iter().map(|v| v.as_str().unwrap()).collect::<Vec<_>>()),
        Some(vec!["192.0.2.0/24"])
    );
}

impl Home {
    /// From now on, this computer seems to be on `networks` (debug builds
    /// read them instead of the real interfaces), the daemon setup starts too.
    fn sees(&mut self, networks: Value) {
        let path = self.home.join("networks.json");
        std::fs::write(&path, serde_json::to_vec(&networks).unwrap()).unwrap();
        self.networks = Some(path);
    }
}
fn ipv6_loopback_present() -> bool {
    let present = std::net::TcpListener::bind((std::net::Ipv6Addr::LOCALHOST, 0)).is_ok();
    if !present {
        eprintln!("skipped: this computer has no IPv6 loopback");
    }
    present
}

/// Set up again, a direct computer that listened on one address now
/// listens on every interface and trusts the network that address is on:
/// same identity, so its pairings stay. Its owner still reaches it.
#[test]
fn setup_again_moves_a_one_address_computer_to_every_interface_on_its_network() {
    if !ipv6_loopback_present() {
        return;
    }
    let mut home = Home::new();
    // Set up before this version: one address (::1 stands in for a LAN one).
    let output = home.hmesh(
        &[
            "--json",
            "init",
            "--name",
            "probe",
            "--port",
            "0",
            "--network",
            "direct",
            "--listen-address",
            "::1",
        ],
        None,
    );
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let before: Value = serde_json::from_slice(&output.stdout).unwrap();
    home.sees(json!([
        {"interface": "lo0", "address": "::1", "subnet": "::1/128"}
    ]));
    let summary = home.setup_with(&["--full", "--without-tailscale"]);
    assert_eq!(summary["network"], "direct", "{summary}");
    assert_eq!(summary["listen_address"], "::", "{summary}");
    assert_eq!(summary["node"]["fingerprint"], before["fingerprint"]);
    let config = home.config();
    assert_eq!(config["node"]["listen_address"].as_str(), Some("::"));
    assert_eq!(config["node"]["expose_to_tailnet"].as_bool(), Some(true));
    let trusted = config["node"]["trusted_networks"].as_array().unwrap();
    assert_eq!(trusted.len(), 1, "{trusted:?}");
    assert_eq!(trusted[0]["subnet"].as_str(), Some("::1/128"));
    assert!(trusted[0].get("tailnet").is_none());
    let status = home.hmesh(&["--json", "daemon", "status"], None);
    let status: Value = serde_json::from_slice(&status.stdout).unwrap();
    assert_eq!(status["networks"]["every_interface"], true, "{status}");
    assert_eq!(
        status["networks"]["current"][0]["trusted"], true,
        "{status}"
    );
    assert_eq!(status["pairing_window"]["required"], true);
    // Run again: nothing to move, the trusted network stays.
    let again = home.setup_with(&["--full", "--without-tailscale"]);
    assert_eq!(again["listen_address"], "::", "{again}");
    assert_eq!(
        home.config()["node"]["trusted_networks"],
        config["node"]["trusted_networks"]
    );
}

/// A new full computer on a direct network listens on every interface and
/// trusts the network with the default route; another network it is on is
/// not opened. Run again, `--trust` and `--listen` add networks.
#[test]
fn a_new_direct_computer_trusts_the_network_with_the_default_route() {
    let mut home = Home::new();
    home.sees(json!([
        {"interface": "en0", "address": "192.0.2.10", "subnet": "192.0.2.0/24",
         "gateway": "192.0.2.1", "gateway_mac": "02:00:5e:00:53:01"},
        {"interface": "en5", "address": "198.51.100.7", "subnet": "198.51.100.0/24"}
    ]));
    let summary = home.setup_with(&["--full", "--network", "direct", "--without-tailscale"]);
    assert_eq!(summary["listen_address"], "::", "{summary}");
    let trusted = |home: &Home| -> Vec<(String, Option<String>)> {
        home.config()["node"]["trusted_networks"]
            .as_array()
            .unwrap()
            .iter()
            .map(|t| {
                (
                    t["subnet"].as_str().unwrap().to_owned(),
                    t.get("gateway").and_then(|g| g.as_str()).map(str::to_owned),
                )
            })
            .collect()
    };
    let home_network = (
        "192.0.2.0/24".to_owned(),
        Some("02:00:5e:00:53:01".to_owned()),
    );
    assert_eq!(trusted(&home), std::slice::from_ref(&home_network));
    // Said, with where to allow it on Windows.
    let steps = summary["steps"].to_string();
    assert!(steps.contains("192.0.2.0/24"), "{steps}");
    if cfg!(any(target_os = "macos", target_os = "windows")) {
        assert!(steps.contains("private networks"), "{steps}");
    }
    let status = home.hmesh(&["--json", "daemon", "status"], None);
    let status: Value = serde_json::from_slice(&status.stdout).unwrap();
    assert_eq!(status["networks"]["every_interface"], true, "{status}");
    assert_eq!(
        summary["node"]["endpoint"]
            .as_str()
            .map(|e| e.starts_with("https://192.0.2.10:")),
        Some(true),
        "{summary}"
    );
    // Run again: a subnet by hand (wherever it is), and a network named by
    // one of this computer's addresses.
    home.setup_with(&["--full", "--trust", "10.20.0.0/16", "--without-tailscale"]);
    home.setup_with(&["--full", "--listen", "198.51.100.7", "--without-tailscale"]);
    assert_eq!(
        trusted(&home),
        [
            home_network,
            ("10.20.0.0/16".to_owned(), None),
            ("198.51.100.0/24".to_owned(), None)
        ]
    );
}
