//! Test-only ACP agent. Never calls a model; never counts as harness smoke.
//!
//! Default mode speaks the Agent Client Protocol on stdio. Prompts select
//! scripted behaviour (`fixture:create`, `fixture:reply`, ...); anything else is
//! echoed. Session history is kept under the temp directory so both ACP
//! restore methods work across process restarts.
use serde_json::{Value, json};
use std::{
    collections::HashMap,
    io::{BufRead, BufReader, Write},
    path::PathBuf,
    process::{Command, Stdio},
    sync::mpsc,
    time::Duration,
};

fn send(value: Value) {
    let mut out = std::io::stdout().lock();
    let _ = writeln!(out, "{value}");
    let _ = out.flush();
}
fn announce_commands(session: &str) {
    if dsh_profile() {
        return;
    }
    update(
        session,
        json!({"sessionUpdate":"available_commands_update","availableCommands":[
            {"name":"compact","description":"Compact this session","input":{"hint":"instructions"}},
            {"name":"skill:review","description":"Review this project"}
        ]}),
    );
}
fn update(session: &str, update: Value) {
    send(
        json!({"jsonrpc":"2.0","method":"session/update","params":{"sessionId":session,"update":update}}),
    );
}
fn say(session: &str, text: &str) {
    update(
        session,
        json!({"sessionUpdate":"agent_message_chunk","content":{"type":"text","text":text}}),
    );
}
fn tool(session: &str, id: &str, title: &str, kind: &str, input: Value, output: &str) {
    update(
        session,
        json!({"sessionUpdate":"tool_call","toolCallId":id,"title":title,"kind":kind,"status":"pending","rawInput":input}),
    );
    update(
        session,
        json!({"sessionUpdate":"tool_call_update","toolCallId":id,"status":"completed","content":[{"type":"content","content":{"type":"text","text":output}}]}),
    );
}
/// Tool calls as each agent family reports them over ACP, read from their
/// adapters' sources: where command output goes, what a diff carries, what
/// a title is. `style` is one family; `all` plays every one in turn.
fn stream(session: &str, style: &str) -> String {
    let call = |id: &str, fields: Value| {
        let mut update = json!({"sessionUpdate":"tool_call","toolCallId":id,"status":"pending"});
        update
            .as_object_mut()
            .unwrap()
            .extend(fields.as_object().unwrap().clone());
        self::update(session, update);
    };
    let done = |id: &str, fields: Value| {
        let mut update = json!({"sessionUpdate":"tool_call_update","toolCallId":id});
        update
            .as_object_mut()
            .unwrap()
            .extend(fields.as_object().unwrap().clone());
        self::update(session, update);
    };
    let text = |t: &str| json!([{"type":"content","content":{"type":"text","text":t}}]);
    let plan = |states: [&str; 3]| {
        let steps = [
            "Find the failing test",
            "Fix the addition",
            "Run the tests again",
        ];
        let entries: Vec<Value> = steps
            .iter()
            .zip(states)
            .map(|(content, status)| json!({"content":content,"priority":"medium","status":status}))
            .collect();
        update(session, json!({"sessionUpdate":"plan","entries":entries}));
    };
    let styles: Vec<&str> = if style == "all" {
        vec![
            "claude", "codex", "opencode", "pi", "dsh", "minimax", "kimi", "mesh",
        ]
    } else {
        vec![style]
    };
    for style in styles {
        match style {
            // Claude: hunks with locations, command output as a console fence
            // in content text, thoughts streamed, a plan replaced as it moves.
            "claude" => {
                update(
                    session,
                    json!({"sessionUpdate":"agent_thought_chunk","content":{"type":"text","text":"The test name says addition, so start with `add`."}}),
                );
                plan(["in_progress", "pending", "pending"]);
                call(
                    "claude-read",
                    json!({"title":"Read src/lib.rs","kind":"read","locations":[{"path":"/work/demo/src/lib.rs","line":1}],"rawInput":{"file_path":"/work/demo/src/lib.rs"}}),
                );
                done(
                    "claude-read",
                    json!({"status":"completed","content":text("```\npub fn add(a: i32, b: i32) -> i32 {\n    a - b\n}\n```")}),
                );
                call(
                    "claude-grep",
                    json!({"title":"grep \"fn add\" src","kind":"search","rawInput":{"pattern":"fn add","path":"src"}}),
                );
                done(
                    "claude-grep",
                    json!({"status":"completed","content":text("Found 1 file\nsrc/lib.rs")}),
                );
                plan(["completed", "in_progress", "pending"]);
                call(
                    "claude-edit",
                    json!({"title":"Edit src/lib.rs","kind":"edit","locations":[{"path":"/work/demo/src/lib.rs","line":2}],"rawInput":{"file_path":"/work/demo/src/lib.rs","old_string":"a - b","new_string":"a + b"},"content":[{"type":"diff","path":"/work/demo/src/lib.rs","oldText":"pub fn add(a: i32, b: i32) -> i32 {\n    a - b\n}","newText":"pub fn add(a: i32, b: i32) -> i32 {\n    a + b\n}"}]}),
                );
                done("claude-edit", json!({"status":"completed"}));
                plan(["completed", "completed", "in_progress"]);
                call(
                    "claude-bash",
                    json!({"title":"`cargo test`","kind":"execute","rawInput":{"command":"cargo test","description":"Run the tests"}}),
                );
                done(
                    "claude-bash",
                    json!({"status":"completed","content":text("```console\nrunning 3 tests\ntest add_works ... ok\ntest result: ok. 3 passed\n```")}),
                );
                plan(["completed", "completed", "completed"]);
            }
            // Codex: a terminal item, output streamed in `_meta`, the exit
            // code at the end.
            "codex" => {
                call(
                    "codex-run",
                    json!({"title":"Run cargo test","kind":"execute","rawInput":{"command":["bash","-lc","cargo test"],"cwd":"/work/demo"},"content":[{"type":"terminal","terminalId":"codex-run"}],"_meta":{"terminal_info":{"terminal_id":"codex-run","cwd":"/work/demo"}}}),
                );
                done(
                    "codex-run",
                    json!({"status":"in_progress","_meta":{"terminal_output":{"terminal_id":"codex-run","data":"running 3 tests\n"}}}),
                );
                done(
                    "codex-run",
                    json!({"_meta":{"terminal_output":{"terminal_id":"codex-run","data":"test add_works ... FAILED\n"}}}),
                );
                done(
                    "codex-run",
                    json!({"status":"failed","_meta":{"terminal_exit":{"terminal_id":"codex-run","exit_code":101,"signal":null}}}),
                );
                call(
                    "codex-patch",
                    json!({"title":"Edit src/lib.rs","kind":"edit","locations":[{"path":"/work/demo/src/lib.rs"}],"content":[{"type":"diff","path":"/work/demo/src/lib.rs","oldText":"    a - b\n","newText":"    a + b\n"}]}),
                );
                done("codex-patch", json!({"status":"completed"}));
            }
            // OpenCode: raw tool names as titles, the whole output again on
            // every update, the final output in rawOutput.
            "opencode" => {
                call(
                    "oc-bash",
                    json!({"title":"bash","kind":"execute","rawInput":{"command":"npm test","description":"Run the tests"}}),
                );
                done(
                    "oc-bash",
                    json!({"status":"in_progress","content":text("> demo@1.0.0 test\n")}),
                );
                done(
                    "oc-bash",
                    json!({"status":"in_progress","content":text("> demo@1.0.0 test\n> node --test\n")}),
                );
                done(
                    "oc-bash",
                    json!({"status":"completed","content":text("> demo@1.0.0 test\n> node --test\n# pass 3\n"),"rawOutput":{"output":"> demo@1.0.0 test\n> node --test\n# pass 3\n","metadata":{"exit":0}}}),
                );
                call(
                    "oc-edit",
                    json!({"title":"edit","kind":"edit","rawInput":{"filePath":"/work/demo/index.js","oldString":"a - b","newString":"a + b"},"content":[{"type":"diff","path":"/work/demo/index.js","oldText":"return a - b","newText":"return a + b"}]}),
                );
                done("oc-edit", json!({"status":"completed"}));
            }
            // Pi: raw tool names, whole files as diffs (a new file has no old
            // text), output as `_meta` deltas.
            "pi" => {
                call(
                    "pi-read",
                    json!({"title":"read","kind":"read","rawInput":{"path":"README.md"}}),
                );
                done(
                    "pi-read",
                    json!({"status":"completed","content":text("# Demo\n")}),
                );
                call(
                    "pi-write",
                    json!({"title":"write","kind":"edit","rawInput":{"path":"NOTES.md","content":"First line\nSecond line\n"},"content":[{"type":"diff","path":"/work/demo/NOTES.md","oldText":null,"newText":"First line\nSecond line\n"}]}),
                );
                done("pi-write", json!({"status":"completed"}));
                call(
                    "pi-bash",
                    json!({"title":"bash","kind":"execute","rawInput":{"command":"ls"}}),
                );
                done(
                    "pi-bash",
                    json!({"status":"in_progress","_meta":{"terminal_output_delta":{"terminal_id":"pi-bash","data":"NOTES.md\n"}}}),
                );
                done(
                    "pi-bash",
                    json!({"status":"completed","_meta":{"terminal_output_delta":{"terminal_id":"pi-bash","data":"README.md\n"},"terminal_exit":{"terminal_id":"pi-bash","exit_code":0}}}),
                );
            }
            // DeepSeek Harness: kind is always `other`, the title is the raw
            // tool name, and only the arguments say what it does.
            "dsh" => {
                call(
                    "dsh-shell",
                    json!({"title":"run_shell","kind":"other","rawInput":{"command":"git status --short"}}),
                );
                done(
                    "dsh-shell",
                    json!({"status":"completed","content":text(" M src/lib.rs\n")}),
                );
                call(
                    "dsh-read",
                    json!({"title":"read_file","kind":"other","rawInput":{"path":"src/lib.rs"}}),
                );
                done(
                    "dsh-read",
                    json!({"status":"completed","content":text("pub fn add(a: i32, b: i32) -> i32 {\n    a + b\n}\n")}),
                );
            }
            // MiniMax: output only in rawOutput, a failure as `{error}`.
            "minimax" => {
                call(
                    "mm-run",
                    json!({"title":"Shell","kind":"execute","rawInput":{"command":"make lint"}}),
                );
                done(
                    "mm-run",
                    json!({"status":"failed","rawOutput":{"error":"make: *** No rule to make target 'lint'.  Stop."}}),
                );
            }
            // Kimi: the call opens while its arguments still stream in as
            // content text, then they come back once more when final.
            "kimi" => {
                let args = json!({"pattern":"TODO","path":"."});
                call(
                    "kimi-grep",
                    json!({"title":"Grep","kind":"search","content":text("{\"pat")}),
                );
                done(
                    "kimi-grep",
                    json!({"status":"in_progress","content":text("{\"pattern\":\"TODO\",")}),
                );
                done(
                    "kimi-grep",
                    json!({"status":"in_progress","rawInput":args.clone(),"content":text(&args.to_string())}),
                );
                done(
                    "kimi-grep",
                    json!({"status":"completed","content":text("src/main.rs:12: // TODO: handle overflow")}),
                );
            }
            // Any agent handing work to another one through HarnessMesh's MCP server.
            "mesh" => {
                call(
                    "mesh-send",
                    json!({"title":"mcp__harnessmesh__mesh_send","kind":"other","rawInput":{"agent":"mac-b/worker","message":"Run the integration tests"}}),
                );
                done(
                    "mesh-send",
                    json!({"status":"completed","content":text("All 12 integration tests passed.")}),
                );
            }
            _ => {}
        }
    }
    "## Fixed\n\nThe test failed because `add` **subtracted** its arguments.\n\n\
     - Changed `a - b` to `a + b` in `src/lib.rs`\n\
     - Ran the tests: *3 passed*\n\n\
     | Check | Result |\n| --- | --- |\n| Unit tests | 3 passed |\n| Lint | no target |\n\n\
     ```rust\npub fn add(a: i32, b: i32) -> i32 {\n    a + b\n}\n```\n\n\
     1. See the [Rust book](https://doc.rust-lang.org/book/)\n\
     2. Ignore [this link](javascript:alert(1))\n"
        .to_owned()
}
fn respond(id: &Value, result: Value) {
    send(json!({"jsonrpc":"2.0","id":id,"result":result}));
}
fn fail(id: &Value, code: i64, message: &str) {
    send(json!({"jsonrpc":"2.0","id":id,"error":{"code":code,"message":message}}));
}
fn end_turn(id: &Value) {
    respond(id, json!({"stopReason":"end_turn"}));
}

#[derive(Default, Clone)]
struct Session {
    cwd: PathBuf,
    mcp_servers: Vec<Value>,
    history: Vec<(String, String)>,
    /// Session settings. Like OpenCode, they are not kept across
    /// `session/load`: a reloaded session starts from the defaults.
    settings: Settings,
}
#[derive(Clone)]
struct Settings {
    model: String,
    mode: String,
    effort: String,
}
impl Default for Settings {
    fn default() -> Self {
        Self {
            model: "fixture-small".into(),
            mode: "ask".into(),
            effort: "low".into(),
        }
    }
}
impl Settings {
    /// ACP `configOptions`; the large model adds a reasoning-effort setting.
    fn options(&self) -> Value {
        let select = |id: &str, name: &str, category: &str, current: &str, choices: &[&str]| {
            json!({"id":id,"name":name,"category":category,"type":"select","currentValue":current,
                "options":choices.iter().map(|c| json!({"value":c,"name":c})).collect::<Vec<_>>()})
        };
        // A configured OpenCode publishes its custom provider's model names.
        let config: Option<Value> = std::env::var("OPENCODE_CONFIG_CONTENT")
            .ok()
            .and_then(|raw| serde_json::from_str(&raw).ok());
        let custom: Vec<String> = config
            .as_ref()
            .and_then(|c| c["provider"]["hmesh"]["models"].as_object())
            .into_iter()
            .flat_map(|models| models.keys().map(|id| format!("hmesh/{id}")))
            .collect();
        let choices: Vec<&str> = if custom.is_empty() {
            vec!["fixture-small", "fixture-large"]
        } else {
            custom.iter().map(String::as_str).collect()
        };
        let mut options = vec![
            select("model", "Model", "model", &self.model, &choices),
            select("mode", "Mode", "mode", &self.mode, &["ask", "code"]),
        ];
        if self.model == "fixture-large" {
            options.push(select(
                "effort",
                "Effort",
                "thought_level",
                &self.effort,
                &["low", "high"],
            ));
        }
        Value::Array(options)
    }
    fn set(&mut self, id: &str, value: &str) -> Result<(), String> {
        let offered = self.options();
        let option = offered
            .as_array()
            .and_then(|o| o.iter().find(|o| o["id"] == id))
            .ok_or_else(|| format!("Invalid params: unknown setting {id}"))?;
        if !option["options"]
            .as_array()
            .is_some_and(|c| c.iter().any(|c| c["value"] == value))
        {
            return Err(format!("Invalid params: {id} has no choice {value}"));
        }
        match id {
            "model" => self.model = value.into(),
            "mode" => self.mode = value.into(),
            _ => self.effort = value.into(),
        }
        Ok(())
    }
}
/// FNV-1a, so tests can check an environment value without seeing it.
fn fnv(text: &str) -> u64 {
    text.bytes().fold(0xcbf29ce484222325, |h, b| {
        (h ^ u64::from(b)).wrapping_mul(0x100000001b3)
    })
}
/// `--state-dir DIR` keeps this agent's sessions apart from other tests'.
fn state_dir() -> PathBuf {
    flag("--state-dir")
        .map(PathBuf::from)
        .unwrap_or_else(|| std::env::temp_dir().join("hmesh-fixture-sessions"))
}
/// RFC 3339 UTC time of a file's last change.
fn modified(path: &std::path::Path) -> (u64, String) {
    let secs = std::fs::metadata(path)
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map_or(0, |d| d.as_secs());
    let (days, rest) = (secs / 86_400, secs % 86_400);
    // Civil date from days since 1970-01-01 (Howard Hinnant's algorithm).
    let z = days as i64 + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    (
        secs,
        format!(
            "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}Z",
            rest / 3600,
            rest % 3600 / 60,
            rest % 60
        ),
    )
}
/// ACP `session/list` over the saved sessions: newest first, optionally one
/// folder only, `--list-page-size` per page (default 50).
fn list_sessions(params: &Value) -> Value {
    // `--list-everywhere`: an agent that ignores the folder it is asked about.
    let only = params["cwd"]
        .as_str()
        .map(PathBuf::from)
        .filter(|_| !std::env::args().any(|a| a == "--list-everywhere"));
    let mut all: Vec<(u64, String, Value)> = vec![];
    for entry in std::fs::read_dir(state_dir())
        .into_iter()
        .flatten()
        .flatten()
    {
        let path = entry.path();
        let Some(id) = path
            .file_name()
            .and_then(|n| n.to_str())
            .and_then(|n| n.strip_suffix(".json"))
            .map(str::to_owned)
        else {
            continue;
        };
        let Some(saved) = std::fs::read_to_string(&path)
            .ok()
            .and_then(|text| serde_json::from_str::<Value>(&text).ok())
        else {
            continue;
        };
        let cwd = PathBuf::from(saved["cwd"].as_str().unwrap_or(""));
        if only.as_ref().is_some_and(|only| only != &cwd) {
            continue;
        }
        let title = saved["history"]
            .as_array()
            .into_iter()
            .flatten()
            .find(|e| e[0] == "user")
            .and_then(|e| e[1].as_str())
            .map(str::to_owned);
        let (secs, at) = modified(&path);
        all.push((
            secs,
            id.clone(),
            json!({"sessionId":id,"cwd":cwd,"title":title,"updatedAt":at}),
        ));
    }
    all.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.cmp(&b.1)));
    let size = flag("--list-page-size")
        .and_then(|v| v.parse::<usize>().ok())
        .unwrap_or(50)
        .max(1);
    let start = params["cursor"]
        .as_str()
        .and_then(|c| c.parse::<usize>().ok())
        .unwrap_or(0);
    let page: Vec<Value> = all
        .iter()
        .skip(start)
        .take(size)
        .map(|(_, _, s)| s.clone())
        .collect();
    let mut result = json!({"sessions":page});
    if start + size < all.len() {
        result["nextCursor"] = json!((start + size).to_string());
    }
    result
}
fn save(id: &str, session: &Session) {
    let _ = std::fs::create_dir_all(state_dir());
    let history: Vec<Value> = session
        .history
        .iter()
        .map(|(role, text)| json!([role, text]))
        .collect();
    let _ = std::fs::write(
        state_dir().join(format!("{id}.json")),
        json!({"cwd":session.cwd,"history":history}).to_string(),
    );
}
fn restore(id: &str) -> Option<Vec<(String, String)>> {
    if !id.chars().all(|c| c.is_ascii_alphanumeric() || c == '-') {
        return None;
    }
    let text = std::fs::read_to_string(state_dir().join(format!("{id}.json"))).ok()?;
    let value: Value = serde_json::from_str(&text).ok()?;
    Some(
        value["history"]
            .as_array()?
            .iter()
            .filter_map(|e| Some((e[0].as_str()?.to_owned(), e[1].as_str()?.to_owned())))
            .collect(),
    )
}

/// Call `mesh_send` on the first MCP server the client gave this session.
fn delegate(server: &Value, agent: &str, message: &str) -> Result<String, String> {
    let command = server["command"].as_str().ok_or("no MCP server command")?;
    let args: Vec<String> = server["args"]
        .as_array()
        .map(|a| {
            a.iter()
                .filter_map(|v| v.as_str().map(str::to_owned))
                .collect()
        })
        .unwrap_or_default();
    let mut child = Command::new(command);
    child
        .args(&args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    for pair in server["env"].as_array().into_iter().flatten() {
        if let (Some(name), Some(value)) = (pair["name"].as_str(), pair["value"].as_str()) {
            child.env(name, value);
        }
    }
    let mut child = child
        .spawn()
        .map_err(|e| format!("MCP server did not start: {e}"))?;
    let mut stdin = child.stdin.take().ok_or("no MCP stdin")?;
    let mut lines = BufReader::new(child.stdout.take().ok_or("no MCP stdout")?).lines();
    let mut call = |id: u64, method: &str, params: Value| -> Result<Value, String> {
        writeln!(
            stdin,
            "{}",
            json!({"jsonrpc":"2.0","id":id,"method":method,"params":params})
        )
        .map_err(|e| e.to_string())?;
        for line in lines.by_ref() {
            let frame: Value = serde_json::from_str(&line.map_err(|e| e.to_string())?)
                .map_err(|e| e.to_string())?;
            if frame["id"] == json!(id) {
                if let Some(error) = frame.get("error") {
                    return Err(error["message"].as_str().unwrap_or("MCP error").to_owned());
                }
                return Ok(frame["result"].clone());
            }
        }
        Err("MCP server closed".into())
    };
    call(
        1,
        "initialize",
        json!({"protocolVersion":"2025-06-18","capabilities":{},"clientInfo":{"name":"hmesh-fixture","version":"0"}}),
    )?;
    let result = call(
        2,
        "tools/call",
        json!({"name":"mesh_send","arguments":{"agent":agent,"message":message,"wait_seconds":30}}),
    );
    let _ = child.kill();
    let result = result?;
    let text = result["content"][0]["text"]
        .as_str()
        .unwrap_or("")
        .to_owned();
    if result["isError"] == json!(true) {
        return Err(text);
    }
    let value: Value = serde_json::from_str(&text).map_err(|_| text.clone())?;
    Ok(value["final_text"]
        .as_str()
        .map(str::to_owned)
        .unwrap_or_else(|| format!("status {}", value["status"])))
}

enum Event {
    Line(String),
    Closed,
    Delegated {
        id: Value,
        session: String,
        text: String,
    },
}
struct Pending {
    prompt_id: Value,
    session: String,
    granted: &'static str,
    denied: &'static str,
    /// Reply with the chosen option instead of granted/denied.
    echo: bool,
}

async fn probe_model_relay() -> anyhow::Result<Value> {
    let config: Value = serde_json::from_str(&std::env::var("OPENCODE_CONFIG_CONTENT")?)?;
    let relay = config["provider"]["hmesh"]["options"]["baseURL"]
        .as_str()
        .unwrap_or_default();
    let url = reqwest::Url::parse(relay)?;
    anyhow::ensure!(url.scheme() == "http" && url.host_str() == Some("127.0.0.1"));
    let token = std::env::var("HMESH_OPENCODE_API_KEY")?;
    let http = reqwest::Client::builder()
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(Duration::from_secs(5))
        .build()?;
    let request =
        json!({"model":"m-one","stream":true,"messages":[{"role":"user","content":"hi"}]});
    let response = http
        .post(format!("{relay}/chat/completions"))
        .bearer_auth(&token)
        .json(&request)
        .send()
        .await?;
    let chat = response.status().as_u16();
    let body = response.text().await?;
    let listed = http
        .get(format!("{relay}/models?limit=5"))
        .bearer_auth(&token)
        .send()
        .await?;
    let moved = http
        .get(format!("{relay}/moved"))
        .bearer_auth(&token)
        .send()
        .await?;
    Ok(json!({
        "chat":chat,"listed":listed.status().as_u16(),"moved":moved.status().as_u16(),
        "fixed":!body.contains(r#""role":"""#)
            && body.contains(r#""role":"assistant","content":"你好""#)
            && body.contains(r#""finish_reason":"stop""#)
            && body.trim_end().ends_with("data: [DONE]")
    }))
}

fn run_acp() -> anyhow::Result<()> {
    let resume_only = dsh_profile();
    let (events, inbox) = mpsc::channel();
    let reader = events.clone();
    std::thread::spawn(move || {
        for line in std::io::stdin().lock().lines() {
            match line {
                Ok(line) => {
                    if reader.send(Event::Line(line)).is_err() {
                        return;
                    }
                }
                Err(_) => break,
            }
        }
        let _ = reader.send(Event::Closed);
    });
    let mut sessions: HashMap<String, Session> = HashMap::new();
    let mut waiting: HashMap<String, Value> = HashMap::new();
    let mut permissions: HashMap<String, Pending> = HashMap::new();
    let mut next = 0u64;
    for event in inbox {
        let line = match event {
            Event::Line(line) => line,
            Event::Closed => break,
            Event::Delegated { id, session, text } => {
                say(&session, &text);
                if let Some(s) = sessions.get_mut(&session) {
                    s.history.push(("agent".into(), text));
                    save(&session, s);
                }
                end_turn(&id);
                continue;
            }
        };
        let Ok(frame) = serde_json::from_str::<Value>(&line) else {
            continue;
        };
        let method = frame["method"].as_str().unwrap_or("");
        let id = frame.get("id").cloned().unwrap_or(Value::Null);
        let params = &frame["params"];
        // Responses to our permission requests.
        if method.is_empty() {
            if let Some(pending) = id.as_str().and_then(|k| permissions.remove(k)) {
                let chosen = frame
                    .pointer("/result/outcome/optionId")
                    .and_then(Value::as_str);
                let echoed;
                let text = if pending.echo {
                    echoed = match chosen {
                        Some(option) => format!("Chose {option}."),
                        None => "Cancelled.".to_owned(),
                    };
                    echoed.as_str()
                } else if chosen == Some("allow") {
                    pending.granted
                } else {
                    pending.denied
                };
                say(&pending.session, text);
                if let Some(s) = sessions.get_mut(&pending.session) {
                    s.history.push(("agent".into(), text.into()));
                    save(&pending.session, s);
                }
                end_turn(&pending.prompt_id);
            }
            continue;
        }
        match method {
            "initialize" => {
                // `--start-delay-ms N` simulates an agent that starts slowly.
                let args: Vec<String> = std::env::args().collect();
                if let Some(ms) = args
                    .iter()
                    .position(|a| a == "--start-delay-ms")
                    .and_then(|i| args.get(i + 1))
                    .and_then(|v| v.parse::<u64>().ok())
                {
                    std::thread::sleep(Duration::from_millis(ms));
                }
                // `--auth-status subscription|api_key|none` answers `auth/status`
                // the way claude-agent-acp does.
                let mut capabilities = json!({"loadSession":true,"promptCapabilities":{},"mcpCapabilities":{"http":false,"sse":false}});
                if resume_only {
                    capabilities["loadSession"] = json!(false);
                    capabilities["sessionCapabilities"] = json!({"resume":{}});
                }
                // `--no-list` is an agent that cannot list its sessions.
                if !std::env::args().any(|a| a == "--no-list") {
                    capabilities["sessionCapabilities"]["list"] = json!({});
                }
                if auth_status().is_some() {
                    capabilities["auth"] = json!({"status":{}});
                }
                // `--auth-push subscription|none` only pushes `_auth/status_update`,
                // like an agent with the extension but without `auth/status`.
                let push = flag("--auth-push");
                if push.is_some() {
                    capabilities["_meta"] = json!({"authStatus":{}});
                }
                respond(
                    &id,
                    json!({"protocolVersion":1,"agentCapabilities":capabilities,"authMethods":[],"agentInfo":{"name":"hmesh-fixture","version":"0.84.2"}}),
                );
                match push.as_deref() {
                    Some("subscription") => send(
                        json!({"jsonrpc":"2.0","method":"_auth/status_update","params":{"authStatus":{"kind":"account","label":"Claude subscription","account":{"email":"pushed@example.com","plan":"pro"}}}}),
                    ),
                    Some(_) => send(
                        json!({"jsonrpc":"2.0","method":"_auth/status_update","params":{"authStatus":{"kind":"none","label":"Not logged in"}}}),
                    ),
                    None => {}
                }
            }
            "auth/status" => match auth_status().as_deref() {
                Some("subscription") => respond(
                    &id,
                    json!({"state":"authenticated","accounts":[{"label":"Claude subscription","description":"OAuth token","email":"fixture@example.com","active":true,"_meta":{"claudeCode":{"kind":"account","plan":"max"}}}]}),
                ),
                Some("api_key") => respond(
                    &id,
                    json!({"state":"authenticated","accounts":[{"label":"Anthropic API key","description":"ANTHROPIC_API_KEY","active":true,"_meta":{"claudeCode":{"kind":"api_key"}}}]}),
                ),
                Some(_) => respond(&id, json!({"state":"unauthenticated"})),
                None => fail(&id, -32601, "Method not found"),
            },
            "session/new" => {
                next += 1;
                let session_id = format!(
                    "fixture-{}-{}-{next}",
                    std::process::id(),
                    std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .map(|d| d.as_nanos())
                        .unwrap_or(0)
                );
                let session = Session {
                    cwd: PathBuf::from(params["cwd"].as_str().unwrap_or(".")),
                    mcp_servers: params["mcpServers"].as_array().cloned().unwrap_or_default(),
                    history: vec![],
                    settings: Settings::default(),
                };
                save(&session_id, &session);
                let options = session.settings.options();
                sessions.insert(session_id.clone(), session);
                if let Some(delay) = flag("--commands-delay-ms").and_then(|v| v.parse::<u64>().ok())
                {
                    respond(&id, json!({"sessionId":session_id,"configOptions":options}));
                    std::thread::sleep(Duration::from_millis(delay));
                    announce_commands(&session_id);
                } else {
                    announce_commands(&session_id);
                    respond(&id, json!({"sessionId":session_id,"configOptions":options}));
                }
            }
            "session/set_config_option" => {
                // `--config-delay-ms N` simulates an agent slow to switch.
                let args: Vec<String> = std::env::args().collect();
                if let Some(ms) = args
                    .iter()
                    .position(|a| a == "--config-delay-ms")
                    .and_then(|i| args.get(i + 1))
                    .and_then(|v| v.parse::<u64>().ok())
                {
                    std::thread::sleep(Duration::from_millis(ms));
                }
                let session_id = params["sessionId"].as_str().unwrap_or("").to_owned();
                let Some(session) = sessions.get_mut(&session_id) else {
                    fail(&id, -32002, "unknown session");
                    continue;
                };
                let value = params["value"].as_str().unwrap_or_default();
                match session
                    .settings
                    .set(params["configId"].as_str().unwrap_or_default(), value)
                {
                    Ok(()) => {
                        let options = session.settings.options();
                        update(
                            &session_id,
                            json!({"sessionUpdate":"config_option_update","configOptions":options}),
                        );
                        respond(&id, json!({"configOptions":options}));
                    }
                    Err(message) => fail(&id, -32602, &message),
                }
            }
            "session/load" | "session/resume" => {
                if (method == "session/resume") != resume_only {
                    fail(&id, -32601, "unadvertised restore method");
                    continue;
                }
                let session_id = params["sessionId"].as_str().unwrap_or("").to_owned();
                // `<id>.hold-load` in the state directory: the reload waits
                // (up to 30 s) until that file is gone.
                let hold = state_dir().join(format!("{session_id}.hold-load"));
                let until = std::time::Instant::now() + std::time::Duration::from_secs(30);
                while hold.exists() && std::time::Instant::now() < until {
                    std::thread::sleep(std::time::Duration::from_millis(50));
                }
                match restore(&session_id) {
                    Some(history) => {
                        if method == "session/load" {
                            for (role, text) in &history {
                                let kind = if role == "user" {
                                    "user_message_chunk"
                                } else {
                                    "agent_message_chunk"
                                };
                                update(
                                    &session_id,
                                    json!({"sessionUpdate":kind,"content":{"type":"text","text":text}}),
                                );
                            }
                        }
                        // `<id>.fail-after-replay` in the state directory: the
                        // reload fails once it has replayed the history.
                        if state_dir()
                            .join(format!("{session_id}.fail-after-replay"))
                            .exists()
                        {
                            fail(&id, -32603, "the reload stopped part way");
                            continue;
                        }
                        announce_commands(&session_id);
                        sessions.insert(
                            session_id,
                            Session {
                                cwd: PathBuf::from(params["cwd"].as_str().unwrap_or(".")),
                                mcp_servers: params["mcpServers"]
                                    .as_array()
                                    .cloned()
                                    .unwrap_or_default(),
                                history,
                                settings: Settings::default(),
                            },
                        );
                        respond(&id, json!({"configOptions":Settings::default().options()}));
                    }
                    None => fail(&id, -32002, "unknown session"),
                }
            }
            "session/list" => {
                if std::env::args().any(|a| a == "--no-list") {
                    fail(&id, -32601, "Method not found");
                } else {
                    respond(&id, list_sessions(params));
                }
            }
            "session/cancel" => {
                if let Some(prompt) = params["sessionId"].as_str().and_then(|s| waiting.remove(s)) {
                    respond(&prompt, json!({"stopReason":"cancelled"}));
                }
                let session = params["sessionId"].as_str().unwrap_or("");
                let stale: Vec<String> = permissions
                    .iter()
                    .filter(|(_, p)| p.session == session)
                    .map(|(k, _)| k.clone())
                    .collect();
                for key in stale {
                    if let Some(p) = permissions.remove(&key) {
                        respond(&p.prompt_id, json!({"stopReason":"cancelled"}));
                    }
                }
            }
            "session/prompt" => {
                let session_id = params["sessionId"].as_str().unwrap_or("").to_owned();
                let Some(session) = sessions.get_mut(&session_id) else {
                    fail(&id, -32002, "unknown session");
                    continue;
                };
                let text: String = params["prompt"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter_map(|b| b["text"].as_str())
                    .collect::<Vec<_>>()
                    .join("\n");
                let prompt = text.trim().to_owned();
                session.history.push(("user".into(), prompt.clone()));
                let turn = session.history.iter().filter(|(r, _)| r == "user").count();
                let cwd = session.cwd.clone();
                let reply = |session: &mut Session, text: &str| {
                    say(&session_id, text);
                    session.history.push(("agent".into(), text.into()));
                };
                match prompt.as_str() {
                    "fixture:cwd" => {
                        let process = std::env::current_dir().unwrap_or_default();
                        let text = format!("cwd={} process={}", cwd.display(), process.display());
                        reply(session, &text);
                        save(&session_id, session);
                        end_turn(&id);
                    }
                    "fixture:create" => {
                        std::fs::create_dir_all(cwd.join("out"))?;
                        std::fs::write(
                            cwd.join("out/health.json"),
                            b"{\"status\":\"ok\",\"harness\":\"fixture\"}\n",
                        )?;
                        tool(
                            &session_id,
                            "write-1",
                            "Write out/health.json",
                            "edit",
                            json!({"path":"out/health.json"}),
                            "written",
                        );
                        reply(session, "Created out/health.json.");
                        end_turn(&id);
                    }
                    "fixture:reply" => {
                        // Narration, one tool call, then the answer the caller should receive.
                        say(&session_id, "Let me check ");
                        say(&session_id, "the project.");
                        tool(
                            &session_id,
                            "read-1",
                            "Read the project",
                            "read",
                            json!({"path":"."}),
                            "ok",
                        );
                        say(&session_id, "Final answer: ");
                        say(&session_id, "the build is green.");
                        session
                            .history
                            .push(("agent".into(), "Final answer: the build is green.".into()));
                        end_turn(&id);
                    }
                    "fixture:slow" => {
                        for word in ["Working", " step", " by", " step", " done."] {
                            say(&session_id, word);
                            std::thread::sleep(Duration::from_millis(150));
                        }
                        session
                            .history
                            .push(("agent".into(), "Working step by step done.".into()));
                        end_turn(&id);
                    }
                    // Tokens only, as some agents report right after a restart.
                    "fixture:usage-tokens" => {
                        respond(
                            &id,
                            json!({"stopReason":"end_turn","usage":{"inputTokens":5,"outputTokens":2,"totalTokens":7}}),
                        );
                    }
                    // Later in the conversation: more context and cost.
                    "fixture:usage-more" => {
                        update(
                            &session_id,
                            json!({"sessionUpdate":"usage_update","used":2400,"size":128000,"cost":{"amount":0.03,"currency":"USD"}}),
                        );
                        respond(
                            &id,
                            json!({"stopReason":"end_turn","usage":{"inputTokens":60,"outputTokens":20,"totalTokens":80}}),
                        );
                    }
                    "fixture:usage" => {
                        update(
                            &session_id,
                            json!({"sessionUpdate":"usage_update","used":1200,"size":128000,"cost":{"amount":0.0125,"currency":"USD"}}),
                        );
                        respond(
                            &id,
                            json!({"stopReason":"end_turn","usage":{"inputTokens":30,"outputTokens":10,"totalTokens":40,"cachedReadTokens":0,"cachedWriteTokens":0}}),
                        );
                    }
                    "fixture:needs-input" | "fixture:permission" => {
                        let (kind, title, granted, denied) = if prompt == "fixture:needs-input" {
                            (
                                "execute",
                                "Run a shell command",
                                "Command ran.",
                                "Command was refused.",
                            )
                        } else {
                            (
                                "edit",
                                "Edit notes.txt",
                                "Permission granted: notes.txt edited.",
                                "Permission denied: nothing changed.",
                            )
                        };
                        next += 1;
                        let key = format!("perm-{next}");
                        update(
                            &session_id,
                            json!({"sessionUpdate":"tool_call","toolCallId":key,"title":title,"kind":kind,"status":"pending"}),
                        );
                        send(
                            json!({"jsonrpc":"2.0","id":key,"method":"session/request_permission","params":{"sessionId":session_id,"toolCall":{"toolCallId":key,"title":title,"kind":kind},"options":[{"optionId":"allow","name":"Allow once","kind":"allow_once"},{"optionId":"reject","name":"Reject","kind":"reject_once"}]}}),
                        );
                        permissions.insert(
                            key,
                            Pending {
                                prompt_id: id.clone(),
                                session: session_id.clone(),
                                granted,
                                denied,
                                echo: false,
                            },
                        );
                    }
                    // Every kind of option ACP has, lasting ones included.
                    "fixture:permission-options" => {
                        next += 1;
                        let key = format!("perm-{next}");
                        update(
                            &session_id,
                            json!({"sessionUpdate":"tool_call","toolCallId":key,"title":"Run make test","kind":"execute","status":"pending","rawInput":{"command":"make test"}}),
                        );
                        send(
                            json!({"jsonrpc":"2.0","id":key,"method":"session/request_permission","params":{"sessionId":session_id,"toolCall":{"toolCallId":key},"options":[
                                {"optionId":"allow","name":"Allow once","kind":"allow_once"},
                                {"optionId":"always","name":"Always allow make","kind":"allow_always"},
                                {"optionId":"reject","name":"Reject","kind":"reject_once"},
                                {"optionId":"never","name":"Never allow make","kind":"reject_always"}
                            ]}}),
                        );
                        permissions.insert(
                            key,
                            Pending {
                                prompt_id: id.clone(),
                                session: session_id.clone(),
                                granted: "",
                                denied: "",
                                echo: true,
                            },
                        );
                    }
                    // A request for a call the stream never showed.
                    "fixture:permission-unseen" => {
                        next += 1;
                        let key = format!("perm-{next}");
                        send(
                            json!({"jsonrpc":"2.0","id":key,"method":"session/request_permission","params":{"sessionId":session_id,"toolCall":{"toolCallId":key,"title":"Edit notes.txt","kind":"edit","locations":[{"path":"notes.txt","line":3}],"content":[{"type":"diff","path":"notes.txt","oldText":"a\n","newText":"b\n"}]},"options":[
                                {"optionId":"allow","name":"Allow once","kind":"allow_once"},
                                {"optionId":"reject","name":"Reject","kind":"reject_once"}
                            ]}}),
                        );
                        permissions.insert(
                            key,
                            Pending {
                                prompt_id: id.clone(),
                                session: session_id.clone(),
                                granted: "",
                                denied: "",
                                echo: true,
                            },
                        );
                    }
                    "fixture:large-events" => {
                        let text = "中🙂\n\u{0000}\"\\".repeat(10000);
                        say(&session_id, &text);
                        tool(
                            &session_id,
                            "write-big",
                            "Write a large file",
                            "edit",
                            json!({"content":"code".repeat(20000)}),
                            &"result".repeat(15000),
                        );
                        end_turn(&id);
                    }
                    "fixture:native-over-limit" => say(&session_id, &"x".repeat(4 * 1024 * 1024)),
                    "fixture:malformed" => println!("invalid-json"),
                    "fixture:oversized" => println!("{}", "x".repeat(5 * 1024 * 1024)),
                    "fixture:crash" => std::process::exit(7),
                    "fixture:unknown" => std::process::exit(0),
                    "fixture:wait" => {
                        waiting.insert(session_id.clone(), id.clone());
                    }
                    "fixture:child" | "fixture:escaped-child" => {
                        let mut command = Command::new(std::env::current_exe()?);
                        command
                            .arg("--child")
                            .current_dir(&cwd)
                            .stdin(Stdio::null())
                            .stdout(Stdio::null())
                            .stderr(Stdio::null());
                        #[cfg(windows)]
                        {
                            use std::os::windows::process::CommandExt;
                            command.creation_flags(0x08000000);
                        }
                        #[cfg(unix)]
                        if prompt == "fixture:escaped-child" {
                            use std::os::unix::process::CommandExt;
                            command.process_group(0);
                        }
                        let child = command.spawn()?;
                        std::fs::write(cwd.join("owned-child.pid"), child.id().to_string())?;
                        std::mem::forget(child);
                        waiting.insert(session_id.clone(), id.clone());
                    }
                    _ if prompt.starts_with("fixture:delegate ") => {
                        let mut parts = prompt["fixture:delegate ".len()..].splitn(2, ' ');
                        let agent = parts.next().unwrap_or("").to_owned();
                        let message = parts.next().unwrap_or("fixture:reply").to_owned();
                        let server = session.mcp_servers.first().cloned();
                        let events = events.clone();
                        let (prompt_id, session_key) = (id.clone(), session_id.clone());
                        say(&session_id, &format!("Asking {agent}. "));
                        std::thread::spawn(move || {
                            let text = match server {
                                Some(server) => match delegate(&server, &agent, &message) {
                                    Ok(answer) => format!("Delegated to {agent}: {answer}"),
                                    Err(error) => format!("Delegation failed: {error}"),
                                },
                                None => "Delegation failed: no MCP server was provided".into(),
                            };
                            let _ = events.send(Event::Delegated {
                                id: prompt_id,
                                session: session_key,
                                text,
                            });
                        });
                    }
                    _ if prompt == "fixture:stream" || prompt.starts_with("fixture:stream ") => {
                        let style = prompt.strip_prefix("fixture:stream ").unwrap_or("all");
                        say(&session_id, "Let me look at the failing test.");
                        let answer = stream(&session_id, style.trim());
                        reply(session, &answer);
                        end_turn(&id);
                    }
                    // A command still running when the turn is looked at.
                    "fixture:stream-running" => {
                        stream(&session_id, "claude");
                        update(
                            &session_id,
                            json!({"sessionUpdate":"tool_call","toolCallId":"long-run","title":"Run the watcher","kind":"execute","status":"in_progress","rawInput":{"command":"npm run watch"},"_meta":{"terminal_output":{"terminal_id":"long-run","data":"watching for changes…\n"}}}),
                        );
                        waiting.insert(session_id.clone(), id.clone());
                    }
                    _ if prompt.starts_with("fixture:echo ") => {
                        let text = format!("Echo #{turn}: {}", &prompt["fixture:echo ".len()..]);
                        reply(session, &text);
                        end_turn(&id);
                    }
                    "fixture:clear-commands" => {
                        update(
                            &session_id,
                            json!({"sessionUpdate":"available_commands_update","availableCommands":[]}),
                        );
                        reply(session, "Commands cleared");
                        end_turn(&id);
                    }
                    _ if prompt == "/compact" || prompt.starts_with("/compact ") => {
                        let args = prompt.strip_prefix("/compact").unwrap_or_default().trim();
                        reply(session, &format!("Fixture compacted: {args}"));
                        end_turn(&id);
                    }
                    "fixture:config" => {
                        let s = session.settings.clone();
                        let mut text = format!("model={} mode={}", s.model, s.mode);
                        if s.model == "fixture-large" {
                            text.push_str(&format!(" effort={}", s.effort));
                        }
                        reply(session, &text);
                        end_turn(&id);
                    }
                    "fixture:split-key end"
                    | "fixture:split-key error"
                    | "fixture:split-key cancel" => {
                        let value = std::env::var("FIXTURE_SERVICE_TOKEN")?;
                        let thought = |text: &str| {
                            update(
                                &session_id,
                                json!({"sessionUpdate":"agent_thought_chunk","content":{"type":"text","text":text}}),
                            )
                        };
                        let (first, second) =
                            value.split_at(value.char_indices().nth(6).unwrap().0);
                        say(&session_id, &format!("answer: {first}"));
                        thought(&format!("reason: {first}"));
                        say(&session_id, &format!("{second} 中🙂 fixture-"));
                        thought(&format!("{second} done fix"));
                        match prompt.as_str() {
                            "fixture:split-key error" => fail(&id, -32603, "fixture stopped"),
                            "fixture:split-key cancel" => {
                                waiting.insert(session_id.clone(), id.clone());
                            }
                            _ => end_turn(&id),
                        }
                    }
                    _ if prompt.starts_with("fixture:fail-env ") => {
                        // An agent error that quotes its environment.
                        let name = prompt["fixture:fail-env ".len()..].trim();
                        let value = std::env::var(name).unwrap_or_default();
                        fail(
                            &id,
                            -32603,
                            &format!("provider rejected credential {value}"),
                        );
                    }
                    "fixture:model-relay" => {
                        match tokio::runtime::Handle::current().block_on(probe_model_relay()) {
                            Ok(result) => {
                                reply(session, &result.to_string());
                                end_turn(&id);
                            }
                            Err(_) => fail(&id, -32603, "fixture relay probe failed"),
                        }
                    }
                    _ if prompt.starts_with("fixture:env ") => {
                        let name = prompt["fixture:env ".len()..].trim();
                        let text = match std::env::var(name) {
                            Ok(value) => format!(
                                "{name} len={} fnv={:016x} value={value}",
                                value.len(),
                                fnv(&value)
                            ),
                            Err(_) => format!("{name} unset"),
                        };
                        reply(session, &text);
                        end_turn(&id);
                    }
                    _ => {
                        let text = format!("Fixture received: {prompt}");
                        reply(session, &text);
                        end_turn(&id);
                    }
                }
                if let Some(session) = sessions.get(&session_id) {
                    save(&session_id, session);
                }
            }
            _ if !id.is_null() => fail(
                &id,
                -32601,
                &format!("{method} is not supported by the fixture"),
            ),
            _ => {}
        }
    }
    Ok(())
}

fn flag(name: &str) -> Option<String> {
    let args: Vec<String> = std::env::args().collect();
    args.iter()
        .position(|a| a == name)
        .and_then(|i| args.get(i + 1))
        .cloned()
}
fn dsh_profile() -> bool {
    flag("--profile").as_deref() == Some("acp")
}
fn auth_status() -> Option<String> {
    flag("--auth-status")
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let args: Vec<_> = std::env::args().skip(1).collect();
    if args.first().map(String::as_str) == Some("--exit") {
        std::process::exit(7);
    }
    if args.first().map(String::as_str) == Some("--http") {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let listener = tokio::net::TcpListener::bind(format!("127.0.0.1:{}", args[1])).await?;
        loop {
            let (mut stream, _) = listener.accept().await?;
            tokio::spawn(async move {
                let mut input = [0; 8192];
                let n = stream.read(&mut input).await.unwrap_or(0);
                let request = String::from_utf8_lossy(&input[..n]);
                if request.starts_with("POST /slow-write ") {
                    let mut file = std::fs::OpenOptions::new()
                        .create(true)
                        .append(true)
                        .open("slow-writes")
                        .unwrap();
                    let _ = file.write_all(b"x");
                    tokio::time::sleep(Duration::from_millis(1500)).await;
                }
                if request.starts_with("GET /crash ") {
                    std::process::exit(7);
                }
                let (status, body, extra) = if request.starts_with("GET /large ") {
                    ("200 OK", "x".repeat(300000), "")
                } else if request.starts_with("GET /redirect ") {
                    (
                        "302 Found",
                        "{}".into(),
                        "Location: http://127.0.0.1:1/private\r\n",
                    )
                } else {
                    ("200 OK",json!({"status":"ok","node":"fixture","has_auth":request.to_ascii_lowercase().contains("authorization:")}).to_string(),"")
                };
                let response = format!(
                    "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n{extra}\r\n{body}",
                    body.len()
                );
                let _ = stream.write_all(response.as_bytes()).await;
            });
        }
    }
    if args.first().map(String::as_str) == Some("--daemon") {
        return hmesh_daemon::run(std::path::Path::new(&args[1])).await;
    }
    // Stands in for `hmesh --data-dir <dir> mcp --session <id>` when tests run
    // the daemon in-process (HMESH_MCP_COMMAND points here).
    if args.len() >= 3 && args[0] == "--data-dir" && args[2] == "mcp" {
        let session = args
            .iter()
            .position(|a| a == "--session")
            .and_then(|i| args.get(i + 1))
            .map(|id| id.parse())
            .transpose()?;
        return hmesh_mcp::serve_stdio(std::path::Path::new(&args[1]), session).await;
    }
    if args.iter().any(|a| a == "--version") {
        println!("0.84.2");
        return Ok(());
    }
    if args.iter().any(|a| a == "--child") {
        let mut counter = 0u64;
        loop {
            counter += 1;
            std::fs::write("heartbeat", counter.to_string())?;
            std::thread::sleep(Duration::from_millis(25));
        }
    }
    tokio::task::spawn_blocking(run_acp).await?
}
