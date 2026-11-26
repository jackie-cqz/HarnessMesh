//! Two computers that cannot reach each other, joined by the built-in relay
//! (`hmesh relay serve`, here on 127.0.0.1): real processes, TLS and the
//! fixture agent. The studio listens on every interface but trusts no
//! network, so nothing reaches it directly; it holds a connection to the
//! relay. The laptop only connects out. The laptop pairs with the studio
//! by its relay pairing code, lists its agents and has a conversation with
//! one through the relay, which passes on bytes it cannot read. A wrong
//! token is refused, the token appears in no log, audit record or doctor
//! output, and once the relay is restarted the studio holds it again and
//! the laptop gets through again. No model is called.
use serde_json::{Value, json};
use std::{
    io::BufRead,
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    time::{Duration, Instant},
};

struct Process(Child);
impl Drop for Process {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}
fn hmesh() -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_hmesh"));
    command.stdin(Stdio::null());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(0x08000000);
    }
    command
}
fn cli(dir: &Path, args: &[&str]) -> Value {
    let output = hmesh()
        .arg("--data-dir")
        .arg(dir)
        .arg("--json")
        .args(args)
        .output()
        .expect("CLI process");
    assert!(
        output.status.success(),
        "CLI {args:?} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).expect("JSON output")
}
/// The CLI with `secret` on its standard input, as a person pastes a relay
/// code (or pipes it in) so it stays out of the command line.
fn piped(dir: &Path, args: &[&str], secret: &str) -> std::process::Output {
    use std::io::Write;
    let mut child = hmesh()
        .arg("--data-dir")
        .arg(dir)
        .arg("--json")
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("CLI process");
    child
        .stdin
        .take()
        .expect("stdin")
        .write_all(format!("{secret}\n").as_bytes())
        .expect("write the secret");
    child.wait_with_output().expect("CLI output")
}
fn cli_piped(dir: &Path, args: &[&str], secret: &str) -> Value {
    let output = piped(dir, args, secret);
    assert!(
        output.status.success(),
        "CLI {args:?} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).expect("JSON output")
}
fn cli_piped_fails(dir: &Path, args: &[&str], secret: &str) -> String {
    let output = piped(dir, args, secret);
    assert!(
        !output.status.success(),
        "CLI {args:?} unexpectedly succeeded"
    );
    String::from_utf8_lossy(&output.stderr).into_owned()
}
/// A daemon for `dir`, logging to `log`, seeing the networks in `networks`
/// when given (`HMESH_TEST_NETWORKS`).
fn daemon(dir: &Path, log: &Path, networks: Option<&Path>) -> Process {
    let file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(log)
        .expect("log file");
    let mut command = hmesh();
    command
        .arg("--data-dir")
        .arg(dir)
        .args(["daemon", "start"])
        .stdout(Stdio::null())
        .stderr(file);
    if let Some(networks) = networks {
        command.env("HMESH_TEST_NETWORKS", networks);
    }
    Process(command.spawn().expect("daemon process"))
}
async fn ready(dir: &Path) -> Value {
    for _ in 0..200 {
        if let Ok(bytes) = std::fs::read(dir.join("daemon.json"))
            && let Ok(card) = serde_json::from_slice::<Value>(&bytes)
        {
            return card;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    panic!("daemon did not publish its listener");
}
/// The relay in `dir` on `port`, passing what it relays to `tap` too, and
/// the relay code it printed first.
fn relay(port: u16, dir: &Path, tap: &Path, log: &Path) -> (Process, String) {
    let file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(log)
        .expect("log file");
    let mut child = hmesh()
        .arg("--json")
        .args(["relay", "serve", "--listen", &format!("127.0.0.1:{port}")])
        .arg("--dir")
        .arg(dir)
        .env("HMESH_RELAY_TAP", tap)
        .stdout(Stdio::piped())
        .stderr(file)
        .spawn()
        .expect("relay process");
    let mut line = String::new();
    std::io::BufReader::new(child.stdout.take().expect("relay output"))
        .read_line(&mut line)
        .expect("relay code");
    let printed: Value = serde_json::from_str(&line).unwrap_or_else(|_| panic!("{line:?}"));
    let code = printed["relay_code"]
        .as_str()
        .expect("relay code")
        .to_owned();
    (Process(child), code)
}
fn free_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0")
        .expect("free port")
        .local_addr()
        .expect("port")
        .port()
}
fn edit_node(dir: &Path, change: impl FnOnce(&mut toml::Table)) {
    let path = dir.join("node.toml");
    let mut config: toml::Table =
        toml::from_str(&std::fs::read_to_string(&path).expect("node.toml")).expect("TOML");
    change(
        config
            .get_mut("node")
            .and_then(toml::Value::as_table_mut)
            .expect("[node]"),
    );
    std::fs::write(&path, toml::to_string_pretty(&config).expect("TOML")).expect("write");
}
fn networks_file(path: &Path, networks: Value) -> PathBuf {
    let partial = path.with_extension("partial");
    std::fs::write(
        &partial,
        serde_json::to_vec_pretty(&networks).expect("JSON"),
    )
    .expect("write");
    std::fs::rename(&partial, path).expect("rename");
    path.to_owned()
}
/// The fixture ACP agent, registered on the computer in `dir` before its
/// daemon starts.
fn register_fixture(dir: &Path, project: &Path) -> String {
    let fixture = Path::new(env!("CARGO_BIN_EXE_hmesh"))
        .with_file_name(format!("hmesh-fixture{}", std::env::consts::EXE_SUFFIX));
    assert!(
        fixture.exists(),
        "{} is missing: build it first (cargo build -p hmesh-fixture; cargo test --workspace does)",
        fixture.display()
    );
    std::fs::create_dir_all(project).expect("project");
    let instance = hmesh_security::Instance::load(dir).expect("instance");
    let config: hmesh_core::AgentConfig = serde_json::from_value(json!({
        "agent":{"name":"fixture","harness":"acp","project_id":"demo"},
        "project":{"root":project,"allowed_tools":["read","write","edit"]},
        "adapter":{"executable":fixture,"args":[],"provider":"fixture","model":"fixture"}
    }))
    .expect("agent config");
    let agent_id = uuid::Uuid::new_v4();
    let card: hmesh_core::AgentCard = serde_json::from_value(json!({
        "agent_id":agent_id,"node_id":instance.card.node_id,"name":"fixture","harness":"acp","project_ids":["demo"],"availability":"idle","capabilities":[],
        "probe":{"installed":true,"version":"test-fixture","version_supported":true,"protocol_probe":"fixture","authentication":"not_applicable_test_fixture","managed_run":"passed","streaming":"fixture","cancel":"not_checked","session_resume":"not_checked","unavailable_reason":null}
    }))
    .expect("agent card");
    hmesh_store::Store::open(&instance.dir.join("state.sqlite3"))
        .expect("store")
        .register_agent(&card, &config)
        .expect("register");
    agent_id.to_string()
}
/// `check` on what `ask` says, every quarter second until it holds or the
/// wait runs out; what it said last.
async fn until(
    what: &str,
    seconds: u64,
    ask: impl Fn() -> Value,
    check: impl Fn(&Value) -> bool,
) -> Value {
    let deadline = Instant::now() + Duration::from_secs(seconds);
    loop {
        let said = ask();
        if check(&said) {
            return said;
        }
        assert!(Instant::now() < deadline, "{what}: {said}");
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
}
fn holds_relay(dir: &Path) -> bool {
    cli(dir, &["daemon", "status"])["relays"][0]["connected"] == true
}

#[tokio::test]
async fn computers_that_cannot_reach_each_other_pair_and_talk_through_a_relay() {
    let root = tempfile::tempdir().expect("tempdir");
    let relay_dir = root.path().join("relay");
    let tap = root.path().join("relayed.bytes");
    let relay_log = root.path().join("relay.log");
    let relay_port = free_port();
    let (first_relay, code) = relay(relay_port, &relay_dir, &tap, &relay_log);
    assert!(
        code.starts_with(&format!("hmesh-relay://127.0.0.1:{relay_port}/")),
        "{code}"
    );
    let token = hmesh_core::RelayAddress::parse(&code)
        .expect("relay code")
        .token()
        .to_owned();
    let shown = cli(
        root.path(),
        &["relay", "show", "--dir", relay_dir.to_str().expect("path")],
    );
    assert_eq!(shown["relay_code"], code, "{shown}");

    // The studio: on every interface, on a network it trusted and then
    // forgot. Its card still names that network's address; nothing comes in.
    let b_dir = root.path().join("hearth");
    let b_log = root.path().join("hearth.log");
    let lan = json!({"interface": "en0", "address": "192.0.2.10", "subnet": "192.0.2.0/24"});
    let networks = networks_file(&root.path().join("hearth-networks.json"), json!([lan]));
    cli(
        &b_dir,
        &[
            "init",
            "--name",
            "hearth",
            "--port",
            "0",
            "--network",
            "direct",
            "--trust",
            "192.0.2.0/24",
        ],
    );
    edit_node(&b_dir, |node| {
        node.insert("host_names".into(), toml::Value::Array(vec![]));
    });
    let agent = register_fixture(&b_dir, &root.path().join("hearth-project"));
    let _b = daemon(&b_dir, &b_log, Some(&networks));
    let b_card = ready(&b_dir).await;
    let port = b_card["endpoint"]
        .as_str()
        .and_then(|e| e.rsplit(':').next())
        .expect("port")
        .to_owned();
    assert_eq!(
        b_card["endpoint"],
        format!("https://192.0.2.10:{port}"),
        "{b_card}"
    );
    let forgotten = cli(&b_dir, &["networks", "forget", "192.0.2.0/24"]);
    assert_eq!(forgotten["trusted"], json!([]), "{forgotten}");
    let config: toml::Table =
        toml::from_str(&std::fs::read_to_string(b_dir.join("node.toml")).expect("node.toml"))
            .expect("TOML");
    assert_eq!(
        config["node"]["trusted_networks"],
        toml::Value::Array(vec![]),
        "no direct way in"
    );
    let b_id = b_card["node_id"].as_str().expect("id").to_owned();
    // It holds the relay, from now on.
    // Pasted, not on the command line.
    let used = cli_piped(&b_dir, &["relay", "use"], &code);
    assert_eq!(
        used[0]["relay"],
        format!("hmesh-relay://127.0.0.1:{relay_port}"),
        "{used}"
    );
    until(
        "the studio holds the relay",
        20,
        || json!(holds_relay(&b_dir)),
        |held| held == true,
    )
    .await;
    let addresses = cli(&b_dir, &["daemon", "status"])["addresses"].clone();
    assert!(
        addresses
            .to_string()
            .contains(&format!("hmesh-relay://127.0.0.1:{relay_port}")),
        "{addresses}"
    );

    // The laptop only connects out.
    let a_dir = root.path().join("rover");
    let a_log = root.path().join("rover.log");
    cli(
        &a_dir,
        &[
            "init",
            "--name",
            "rover",
            "--port",
            "0",
            "--network",
            "direct",
            "--outbound-only",
        ],
    );
    let _a = daemon(&a_dir, &a_log, None);
    let a_card = ready(&a_dir).await;

    // Through its relay, the studio answers pairing only while it is open.
    let b_fingerprint = b_card["fingerprint"].as_str().expect("fp").to_owned();
    let closed = cli_piped_fails(
        &a_dir,
        &["pair", "request", "--code"],
        &format!("{code}/{b_fingerprint}"),
    );
    assert!(
        closed.contains("PERMISSION_DENIED") && closed.contains("pairing is closed"),
        "{closed}"
    );

    // The studio's relay pairing code; a wrong token gets nowhere.
    let pairing = cli(&b_dir, &["pair", "show"]);
    let relay_code = pairing["relay_pair_codes"][0]
        .as_str()
        .expect("relay pairing code")
        .to_owned();
    assert_eq!(relay_code, format!("{code}/{b_fingerprint}"));
    assert!(pairing["pairing_window"]["closes_at"].is_i64(), "{pairing}");
    let wrong = relay_code.replace(&token, &"0".repeat(64));
    let refused = cli_piped_fails(&a_dir, &["pair", "request", "--code"], &wrong);
    assert!(refused.contains("PEER_UNREACHABLE"), "{refused}");
    assert!(
        refused.contains("was not put through by its relay"),
        "{refused}"
    );
    assert!(!refused.contains(&token), "{refused}");

    // Paired through the relay, with one approval on the studio.
    let request = cli_piped(&a_dir, &["pair", "request", "--code"], &relay_code);
    let id = request["request_id"].as_str().expect("request").to_owned();
    cli(
        &b_dir,
        &[
            "pair",
            "approve",
            &id,
            "--expect-fingerprint",
            a_card["fingerprint"].as_str().expect("fp"),
        ],
    );
    until(
        "paired",
        30,
        || cli(&a_dir, &["pair", "list"]),
        |pairs| {
            pairs.as_array().is_some_and(|p| {
                p.iter()
                    .any(|p| p["request_id"] == id.as_str() && p["status"] == "approved")
            })
        },
    )
    .await;

    // Its agents, through the relay.
    let agents = until(
        "the studio's agents through the relay",
        30,
        || {
            let output = hmesh()
                .arg("--data-dir")
                .arg(&a_dir)
                .args(["--json", "agents", "--peer", &b_id])
                .output()
                .expect("CLI");
            serde_json::from_slice(&output.stdout).unwrap_or(Value::Null)
        },
        |agents| agents.to_string().contains(&agent),
    )
    .await;
    assert!(agents.to_string().contains("fixture"), "{agents}");
    let peer = until(
        "the studio reached last through its relay",
        30,
        || cli(&a_dir, &["peers", "list"])[0].clone(),
        |peer| peer["routes"]["last_good"] == format!("hmesh-relay://127.0.0.1:{relay_port}"),
    )
    .await;
    assert!(
        peer["routes"]["advertised"]
            .to_string()
            .contains(&format!("hmesh-relay://127.0.0.1:{relay_port}")),
        "{peer}"
    );
    assert_eq!(peer["node_connectivity"], "online", "{peer}");
    assert!(
        !peer.to_string().contains(&token),
        "the token is shown: {peer}"
    );

    // A conversation with its agent, through the relay.
    let marker = format!("relay-sealed-{}", uuid::Uuid::new_v4().simple());
    let message = format!("fixture:echo {marker}");
    let opened = cli(
        &a_dir,
        &[
            "session",
            "open",
            "--agent",
            &agent,
            "--peer",
            &b_id,
            "--message",
            &message,
        ],
    );
    let session = opened["session_id"].as_str().expect("session").to_owned();
    let reply = format!("Echo #1: {marker}");
    until(
        "the agent's reply",
        60,
        || cli(&a_dir, &["session", "events", &session, "--wait", "5"]),
        |page| page.to_string().contains(&reply),
    )
    .await;

    // The relay passed bytes on, and none of them were the conversation.
    let relayed = std::fs::read(&tap).expect("tap file");
    assert!(relayed.len() > 4096, "only {} bytes relayed", relayed.len());
    for plain in [
        marker.as_str(),
        "fixture:echo",
        "Echo #1",
        "/hmesh/v1/",
        "fixture",
    ] {
        assert!(
            !relayed.windows(plain.len()).any(|w| w == plain.as_bytes()),
            "{plain:?} passed the relay unencrypted"
        );
    }

    // The token is in no log, audit record or doctor output.
    let doctor = cli(&b_dir, &["doctor"]);
    assert_eq!(
        doctor["daemon"]["status"]["relays"][0]["code"], "hidden",
        "{doctor}"
    );
    let peer_doctor = cli(&a_dir, &["doctor", "--peer", &b_id]);
    for (what, text) in [
        ("studio log", std::fs::read_to_string(&b_log).expect("log")),
        ("laptop log", std::fs::read_to_string(&a_log).expect("log")),
        (
            "relay log",
            std::fs::read_to_string(&relay_log).expect("log"),
        ),
        ("studio audit", cli(&b_dir, &["audit"]).to_string()),
        ("laptop audit", cli(&a_dir, &["audit"]).to_string()),
        ("studio doctor", doctor.to_string()),
        ("laptop doctor", peer_doctor.to_string()),
        (
            "studio status",
            cli(&b_dir, &["daemon", "status"]).to_string(),
        ),
        ("laptop peers", cli(&a_dir, &["peers", "list"]).to_string()),
        ("studio relays", cli(&b_dir, &["relay", "list"]).to_string()),
    ] {
        assert!(!text.contains(&token), "the token is in the {what}");
    }

    // The relay restarts: the studio holds it again, the laptop gets through again.
    drop(first_relay);
    until(
        "the studio sees the relay go",
        20,
        || json!(holds_relay(&b_dir)),
        |held| held == false,
    )
    .await;
    let (_second_relay, again) = relay(relay_port, &relay_dir, &tap, &relay_log);
    assert_eq!(again, code, "the same relay, the same code");
    until(
        "the studio holds the relay again",
        30,
        || json!(holds_relay(&b_dir)),
        |held| held == true,
    )
    .await;
    let again = until(
        "the studio's agents through the restarted relay",
        30,
        || {
            let output = hmesh()
                .arg("--data-dir")
                .arg(&a_dir)
                .args(["--json", "agents", "--peer", &b_id])
                .output()
                .expect("CLI");
            serde_json::from_slice(&output.stdout).unwrap_or(Value::Null)
        },
        |agents| agents.to_string().contains(&agent),
    )
    .await;
    assert!(again.is_array(), "{again}");
    let second = format!("fixture:echo again-{marker}");
    cli(&a_dir, &["session", "send", &session, "--message", &second]);
    until(
        "the agent's second reply",
        60,
        || cli(&a_dir, &["session", "events", &session, "--wait", "5"]),
        |page| {
            page.to_string()
                .contains(&format!("Echo #2: again-{marker}"))
        },
    )
    .await;
    let relayed = std::fs::read(&tap).expect("tap file");
    assert!(
        !relayed
            .windows(marker.len())
            .any(|w| w == marker.as_bytes()),
        "the conversation passed the restarted relay unencrypted"
    );

    // A new token: the old code stops working, the new one is held again.
    let rotated = cli(
        root.path(),
        &[
            "relay",
            "show",
            "--dir",
            relay_dir.to_str().expect("path"),
            "--rotate-token",
        ],
    );
    let new_code = rotated["relay_code"].as_str().expect("code").to_owned();
    assert_ne!(new_code, code);
    until(
        "the studio is let go",
        20,
        || json!(holds_relay(&b_dir)),
        |held| held == false,
    )
    .await;
    cli_piped(&b_dir, &["relay", "use", "-"], &new_code);
    let listed = cli(&b_dir, &["relay", "list"]);
    assert_eq!(
        listed.as_array().map(Vec::len),
        Some(1),
        "replaced, not added: {listed}"
    );
    until(
        "the studio holds the relay with its new token",
        20,
        || json!(holds_relay(&b_dir)),
        |held| held == true,
    )
    .await;
    assert_eq!(cli(&b_dir, &["relay", "forget"]), json!([]));
}
