//! What an agent is set up with on its own: its login or key, as the agent
//! itself reports it. Asked over ACP (`auth/status`, or the `_auth/status_update`
//! push that claude-agent-acp sends), or through the agent's own read-only
//! status command: `codex login status`, `opencode auth list` or
//! `pi --list-models`. Credential files are never read, and no command that
//! prints a credential (such as `pi auth`) is run. No session is created and
//! no model is called.
use crate::executable::resolve_configured_executable;
use anyhow::{Context, Result};
use hmesh_core::service::{OWN_CONFIGURED, OWN_NOT_CONFIGURED, OWN_UNKNOWN, OwnLogin};
use hmesh_core::{AgentConfig, HarnessKind};
use serde_json::{Value, json};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};

/// How long the agent may take to start and report over ACP.
const ACP_DEADLINE: Duration = Duration::from_secs(12);
/// How long a pushed identity is awaited after `initialize`.
const PUSH_WAIT: Duration = Duration::from_secs(6);
/// How long a status command may run.
const COMMAND_DEADLINE: Duration = Duration::from_secs(10);
const OUTPUT_LIMIT: u64 = 256 * 1024;
const TEXT_LIMIT: usize = 200;

pub const SOURCE_ACP: &str = "acp auth/status";

fn text(value: Option<&Value>) -> Option<String> {
    let text: String = value?
        .as_str()?
        .chars()
        .filter(|c| !c.is_control())
        .take(TEXT_LIMIT)
        .collect();
    let text = text.trim().to_owned();
    (!text.is_empty()).then_some(text)
}
fn login(state: &str, source: &str, at: i64) -> OwnLogin {
    OwnLogin {
        state: state.into(),
        kind: None,
        label: None,
        detail: None,
        account: None,
        source: source.into(),
        checked_at: at,
    }
}

/// claude-agent-acp's identity kinds, in HarnessMesh's words.
fn claude_kind(kind: &str) -> Option<&'static str> {
    match kind {
        "account" => Some("subscription"),
        "api_key" => Some("api_key"),
        "gateway" => Some("gateway"),
        "external" => Some("cloud"),
        _ => None,
    }
}

/// An ACP `auth/status` answer (or `auth/status_update` payload).
pub fn from_acp_status(value: &Value, at: i64) -> OwnLogin {
    let state = match value.get("state").and_then(Value::as_str) {
        Some("authenticated") => OWN_CONFIGURED,
        Some("unauthenticated") => OWN_NOT_CONFIGURED,
        _ => OWN_UNKNOWN,
    };
    let mut out = login(state, SOURCE_ACP, at);
    if state != OWN_CONFIGURED {
        return out;
    }
    let accounts = value
        .get("accounts")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let Some(account) = accounts
        .iter()
        .find(|a| a.get("active").and_then(Value::as_bool) == Some(true))
        .or_else(|| accounts.first())
    else {
        return out;
    };
    let claude = account.pointer("/_meta/claudeCode");
    out.kind = claude
        .and_then(|c| c.get("kind"))
        .and_then(Value::as_str)
        .and_then(claude_kind)
        .map(Into::into);
    out.label = text(account.get("label"));
    let plan = claude.and_then(|c| text(c.get("plan")));
    out.detail = match (text(account.get("description")), plan) {
        (Some(d), Some(p)) => Some(format!("{d} · {p}")),
        (d, p) => d.or(p),
    };
    out.account = text(account.get("email"));
    out
}

/// claude-agent-acp's `_auth/status_update` push: `{kind, label, detail?,
/// account?: {email, organization, plan}}`.
pub fn from_claude_push(status: &Value, at: i64) -> OwnLogin {
    let kind = status.get("kind").and_then(Value::as_str).unwrap_or("");
    if kind == "none" {
        return login(OWN_NOT_CONFIGURED, SOURCE_ACP, at);
    }
    let Some(kind) = claude_kind(kind) else {
        return login(OWN_UNKNOWN, SOURCE_ACP, at);
    };
    let mut out = login(OWN_CONFIGURED, SOURCE_ACP, at);
    out.kind = Some(kind.into());
    out.label = text(status.get("label"));
    let plan = status.pointer("/account/plan").and_then(|p| text(Some(p)));
    out.detail = match (text(status.get("detail")), plan) {
        (Some(d), Some(p)) => Some(format!("{d} · {p}")),
        (d, p) => d.or(p),
    };
    out.account = status.pointer("/account/email").and_then(|e| text(Some(e)));
    out
}

pub const SOURCE_CODEX: &str = "codex login status";
/// `codex login status` prints one line. A key's characters it may show are
/// never kept.
pub fn from_codex_status(output: &str, at: i64) -> OwnLogin {
    let lower = output.to_lowercase();
    let mut out = login(OWN_UNKNOWN, SOURCE_CODEX, at);
    if lower.contains("not logged in") {
        out.state = OWN_NOT_CONFIGURED.into();
    } else if lower.contains("logged in using chatgpt") {
        out.state = OWN_CONFIGURED.into();
        out.kind = Some("subscription".into());
        out.label = Some("ChatGPT".into());
    } else if lower.contains("logged in using an api key") {
        out.state = OWN_CONFIGURED.into();
        out.kind = Some("api_key".into());
        out.label = Some("API key".into());
    }
    out
}

/// Terminal colour and cursor sequences removed.
fn plain(output: &str) -> String {
    let mut out = String::with_capacity(output.len());
    let mut chars = output.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\u{1b}' {
            if chars.peek() == Some(&'[') {
                chars.next();
                for c in chars.by_ref() {
                    if c.is_ascii_alphabetic() {
                        break;
                    }
                }
            }
            continue;
        }
        out.push(c);
    }
    out
}

pub const SOURCE_OPENCODE: &str = "opencode auth list";
/// `opencode auth list` lists stored credentials (provider and type) and the
/// provider keys it finds in the environment (provider and variable name);
/// it never prints a key.
pub fn from_opencode_list(output: &str, at: i64) -> OwnLogin {
    let output = plain(output);
    let mut section = "";
    let mut stored = Vec::new();
    let mut environment = Vec::new();
    let mut counted = false;
    for line in output.lines() {
        let line = line.trim();
        if let Some(rest) = line.strip_prefix('┌') {
            let rest = rest.trim();
            section = if rest.starts_with("Credentials") {
                "credentials"
            } else if rest.starts_with("Environment") {
                "environment"
            } else {
                ""
            };
        } else if let Some(entry) = line.strip_prefix('●') {
            let mut words = entry.split_whitespace();
            let Some(name) = words.next() else { continue };
            let rest: Vec<&str> = words.collect();
            // Provider names may have spaces; the last word is the type or variable.
            let (name, last) = match rest.split_last() {
                Some((last, middle)) => (
                    std::iter::once(name)
                        .chain(middle.iter().copied())
                        .collect::<Vec<_>>()
                        .join(" "),
                    Some(*last),
                ),
                None => (name.to_owned(), None),
            };
            let shown = match last {
                Some(last) => format!("{name} ({last})"),
                None => name,
            };
            match section {
                "credentials" => stored.push(shown),
                "environment" => environment.push(shown),
                _ => {}
            }
        } else if line.starts_with('└') && line.contains("credential") {
            counted = true;
        }
    }
    let mut out = login(OWN_UNKNOWN, SOURCE_OPENCODE, at);
    let all: Vec<String> = stored.iter().chain(environment.iter()).cloned().collect();
    if !all.is_empty() {
        out.state = OWN_CONFIGURED.into();
        out.kind = Some("credentials".into());
        out.label = Some("Stored credentials".into());
        out.detail = Some(all.join(", ").chars().take(TEXT_LIMIT).collect());
    } else if counted {
        out.state = OWN_NOT_CONFIGURED.into();
    }
    out
}

pub const SOURCE_PI: &str = "pi --list-models";
/// `pi --list-models` lists models of the providers pi is set up for, one per
/// row under a `provider model …` header.
pub fn from_pi_models(output: &str, success: bool, at: i64) -> OwnLogin {
    let output = plain(output);
    let mut lines = output.lines().map(str::trim).filter(|l| !l.is_empty());
    let mut out = login(OWN_UNKNOWN, SOURCE_PI, at);
    let header = lines.by_ref().find(|l| {
        let mut words = l.split_whitespace();
        words.next() == Some("provider") && words.next() == Some("model")
    });
    if header.is_none() {
        if success && output.to_lowercase().contains("no models") {
            out.state = OWN_NOT_CONFIGURED.into();
        }
        return out;
    }
    let mut providers: Vec<&str> = Vec::new();
    let mut models = 0;
    for line in lines {
        let mut columns = line.split_whitespace();
        let (Some(provider), Some(_model)) = (columns.next(), columns.next()) else {
            continue;
        };
        models += 1;
        if !providers.contains(&provider) {
            providers.push(provider);
        }
    }
    if models == 0 {
        out.state = OWN_NOT_CONFIGURED.into();
        return out;
    }
    out.state = OWN_CONFIGURED.into();
    out.kind = Some("providers".into());
    out.label = Some("Configured providers".into());
    out.detail = Some(format!("{} providers, {models} models", providers.len()));
    out
}

/// Ask the agent over ACP. `None` when it reports nothing about its login.
async fn over_acp(config: &AgentConfig, at: i64) -> Option<OwnLogin> {
    let executable = resolve_configured_executable(config).ok()?;
    let mut command = executable.command();
    command
        .args(config.launch_args())
        .current_dir(&config.project.root)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .kill_on_drop(true);
    let mut child = command.spawn().ok()?;
    let mut stdin = child.stdin.take()?;
    let stdout = child.stdout.take()?;
    let overall = tokio::time::Instant::now() + ACP_DEADLINE;
    let answer = async move {
        let send = |frame: Value| {
            let mut line = frame.to_string();
            line.push('\n');
            line
        };
        stdin
            .write_all(send(crate::acp::initialize_request(1)).as_bytes())
            .await
            .ok()?;
        stdin.flush().await.ok()?;
        let mut lines = BufReader::new(stdout.take(4 * 1024 * 1024)).lines();
        let mut push_deadline = None;
        let mut pushed = None;
        loop {
            let deadline = push_deadline.map_or(overall, |d: tokio::time::Instant| d.min(overall));
            // Out of time: what was pushed so far is the answer.
            let Ok(next) = tokio::time::timeout_at(deadline, lines.next_line()).await else {
                return pushed;
            };
            let Ok(Some(line)) = next else {
                return pushed;
            };
            let Ok(frame) = serde_json::from_str::<Value>(&line) else {
                continue;
            };
            match frame.get("method").and_then(Value::as_str) {
                Some("_auth/status_update") => {
                    if let Some(status) = frame.pointer("/params/authStatus") {
                        pushed = Some(from_claude_push(status, at));
                        if push_deadline.is_some() {
                            return pushed;
                        }
                    }
                    continue;
                }
                Some("auth/status_update") => {
                    if let Some(params) = frame.get("params") {
                        pushed = Some(from_acp_status(params, at));
                    }
                    continue;
                }
                _ => {}
            }
            match frame.get("id").and_then(Value::as_u64) {
                Some(1) => {
                    let capabilities = frame.pointer("/result/agentCapabilities")?;
                    if capabilities.pointer("/auth/status").is_some() {
                        stdin
                            .write_all(
                                send(json!({"jsonrpc":"2.0","id":2,"method":"auth/status","params":{}}))
                                    .as_bytes(),
                            )
                            .await
                            .ok()?;
                        stdin.flush().await.ok()?;
                    } else if capabilities.pointer("/_meta/authStatus").is_some() {
                        if pushed.is_some() {
                            return pushed;
                        }
                        push_deadline = Some(tokio::time::Instant::now() + PUSH_WAIT);
                    } else {
                        return None;
                    }
                }
                Some(2) => {
                    // An unknown answer may still be followed by a push.
                    let status = frame.get("result").map(|r| from_acp_status(r, at));
                    return match status {
                        Some(s) if s.state != OWN_UNKNOWN => Some(s),
                        _ => pushed,
                    };
                }
                _ => {}
            }
        }
    }
    .await;
    let _ = child.kill().await;
    answer
}

/// An executable named `name` beside `near`, else on `PATH`.
fn find(name: &str, near: &Path) -> Option<PathBuf> {
    let beside = near.parent().map(|dir| dir.join(name));
    beside.filter(|p| p.is_file()).or_else(|| {
        std::env::split_paths(&std::env::var_os("PATH")?)
            .map(|dir| dir.join(name))
            .find(|p| p.is_file())
    })
}

/// Run a status command: its exit, standard output and standard error, or
/// `None` when it could not run in time.
async fn run(mut command: tokio::process::Command) -> Option<(bool, String, String)> {
    command
        .env("NO_COLOR", "1")
        .env("FORCE_COLOR", "0")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    let mut child = command.spawn().ok()?;
    let mut stdout = child.stdout.take()?.take(OUTPUT_LIMIT);
    let mut stderr = child.stderr.take()?.take(OUTPUT_LIMIT);
    tokio::time::timeout(COMMAND_DEADLINE, async move {
        let mut out = Vec::new();
        let mut err = Vec::new();
        let (a, b) = tokio::join!(stdout.read_to_end(&mut out), stderr.read_to_end(&mut err));
        a.ok()?;
        b.ok()?;
        let status = child.wait().await.ok()?;
        Some((
            status.success(),
            String::from_utf8_lossy(&out).into_owned(),
            String::from_utf8_lossy(&err).into_owned(),
        ))
    })
    .await
    .ok()
    .flatten()
}

async fn by_command(config: &AgentConfig, at: i64) -> Option<OwnLogin> {
    let executable = resolve_configured_executable(config).ok()?;
    let near = executable.entry.clone();
    match config.agent.harness {
        HarnessKind::Opencode => {
            let mut command = executable.command();
            command.args(["auth", "list"]);
            let (_, out, err) = run(command).await?;
            Some(from_opencode_list(&format!("{out}\n{err}"), at))
        }
        HarnessKind::Codex => {
            let mut command = tokio::process::Command::new(find("codex", &near)?);
            command.args(["login", "status"]);
            let (_, out, err) = run(command).await?;
            Some(from_codex_status(&format!("{out}\n{err}"), at))
        }
        HarnessKind::Pi => {
            let mut command = tokio::process::Command::new(find("pi", &near)?);
            command.arg("--list-models");
            // The table is on standard output; warnings on standard error are not rows.
            let (success, out, _) = run(command).await?;
            Some(from_pi_models(&out, success, at))
        }
        _ => None,
    }
}

/// What the agent is set up with on its own. Its own report over ACP wins;
/// otherwise its status command; otherwise `unknown`.
pub async fn detect(config: &AgentConfig, at: i64) -> Result<OwnLogin> {
    config.validate().context("invalid agent registration")?;
    let (acp, command) = tokio::join!(over_acp(config, at), by_command(config, at));
    Ok(match (acp, command) {
        (Some(acp), _) if acp.state != OWN_UNKNOWN => acp,
        (_, Some(command)) if command.state != OWN_UNKNOWN => command,
        (Some(acp), _) => acp,
        (_, Some(command)) => command,
        (None, None) => OwnLogin::unknown(
            "none",
            "this agent does not report its own login, and HarnessMesh has no status command for it",
            at,
        ),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn acp_status_from_claude_agent_acp() {
        // The shapes of claude-agent-acp 0.110.1 `dist/auth-status.js`.
        let subscription = json!({"state":"authenticated","accounts":[{"label":"Claude subscription","description":"OAuth token","email":"you@example.com","active":true,"_meta":{"claudeCode":{"kind":"account","detail":"OAuth token","plan":"max"}}}]});
        let seen = from_acp_status(&subscription, 7);
        assert_eq!(
            (
                seen.state.as_str(),
                seen.kind.as_deref(),
                seen.label.as_deref(),
                seen.detail.as_deref(),
                seen.account.as_deref()
            ),
            (
                "configured",
                Some("subscription"),
                Some("Claude subscription"),
                Some("OAuth token · max"),
                Some("you@example.com")
            )
        );
        let key = json!({"state":"authenticated","accounts":[{"label":"Anthropic API key","description":"ANTHROPIC_API_KEY","active":true,"_meta":{"claudeCode":{"kind":"api_key"}}}]});
        let seen = from_acp_status(&key, 7);
        assert_eq!(
            (seen.kind.as_deref(), seen.detail.as_deref()),
            (Some("api_key"), Some("ANTHROPIC_API_KEY"))
        );
        assert_eq!(
            from_acp_status(&json!({"state":"unauthenticated"}), 7).state,
            "not_configured"
        );
        assert_eq!(
            from_acp_status(&json!({"state":"unknown"}), 7).state,
            "unknown"
        );
        let push = json!({"kind":"external","label":"AWS Bedrock"});
        assert_eq!(from_claude_push(&push, 7).kind.as_deref(), Some("cloud"));
        assert_eq!(
            from_claude_push(&json!({"kind":"none","label":"Not logged in"}), 7).state,
            "not_configured"
        );
    }
    #[test]
    fn codex_status_lines_without_key_characters() {
        // Real output of codex-cli 0.153.4 on a computer without a login.
        assert_eq!(
            from_codex_status("Not logged in\n", 1).state,
            "not_configured"
        );
        let chatgpt = from_codex_status("Logged in using ChatGPT\n", 1);
        assert_eq!(
            (chatgpt.state.as_str(), chatgpt.label.as_deref()),
            ("configured", Some("ChatGPT"))
        );
        let key = from_codex_status("Logged in using an API key - sk-proj-***abcde\n", 1);
        assert_eq!(
            (key.kind.as_deref(), key.label.as_deref()),
            (Some("api_key"), Some("API key"))
        );
        assert!(!serde_json::to_string(&key).unwrap().contains("abcde"));
        assert_eq!(from_codex_status("something else", 1).state, "unknown");
    }
    #[test]
    fn opencode_list_with_and_without_credentials() {
        // Real output of opencode 1.18.33 (`opencode auth list`), colours included.
        let real = "\u{1b}[0m\n┌  Credentials \u{1b}[90m~/.local/share/opencode/auth.json\n│\n●  OpenCode \u{1b}[90mapi\n│\n└  1 credentials\n";
        let seen = from_opencode_list(real, 1);
        assert_eq!(
            (seen.state.as_str(), seen.detail.as_deref()),
            ("configured", Some("OpenCode (api)"))
        );
        let env = "┌  Credentials ~/.local/share/opencode/auth.json\n│\n└  0 credentials\n\n┌  Environment\n│\n●  Anthropic ANTHROPIC_API_KEY\n│\n└  1 environment variable\n";
        assert_eq!(
            from_opencode_list(env, 1).detail.as_deref(),
            Some("Anthropic (ANTHROPIC_API_KEY)")
        );
        let none = "┌  Credentials ~/.local/share/opencode/auth.json\n│\n└  0 credentials\n";
        assert_eq!(from_opencode_list(none, 1).state, "not_configured");
        assert_eq!(
            from_opencode_list("error: unknown command", 1).state,
            "unknown"
        );
    }
    #[test]
    fn pi_models_by_provider() {
        // The layout of pi 0.83.0 `--list-models`.
        let real = "provider             model                    context  max-out  thinking  images\nfable-gateway        claude-fable-5-1         200K     32K      yes       yes\ntest-gateway         glm-5.3-highspeed        1M       65.5K    yes       no\ntest-gateway         gpt-5.6-luna             272K     16.4K    no        yes\n";
        let seen = from_pi_models(real, true, 1);
        assert_eq!(
            (seen.state.as_str(), seen.detail.as_deref()),
            ("configured", Some("2 providers, 3 models"))
        );
        let empty = "provider  model  context  max-out  thinking  images\n";
        assert_eq!(from_pi_models(empty, true, 1).state, "not_configured");
        // A stray one-word line is not a model.
        let noisy = "provider  model  context\nWarning:\n";
        assert_eq!(from_pi_models(noisy, true, 1).state, "not_configured");
        assert_eq!(
            from_pi_models("pi: command failed", false, 1).state,
            "unknown"
        );
    }
}
