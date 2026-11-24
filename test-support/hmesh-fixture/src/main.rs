//! Test-only ACP agent. Never calls a model; never counts as harness smoke.
//!
//! Default mode speaks the Agent Client Protocol on stdio. Prompts select
//! scripted behaviour (`fixture:create`, `fixture:reply`, ...); anything else is
//! echoed. Session history is kept under the temp directory so `session/load`
//! works across process restarts.
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
}
fn state_dir() -> PathBuf {
    std::env::temp_dir().join("hmesh-fixture-sessions")
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
}

fn run_acp() -> anyhow::Result<()> {
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
                let text = if chosen == Some("allow") {
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
                respond(
                    &id,
                    json!({"protocolVersion":1,"agentCapabilities":{"loadSession":true,"promptCapabilities":{},"mcpCapabilities":{"http":false,"sse":false}},"authMethods":[],"agentInfo":{"name":"hmesh-fixture","version":"0.84.2"}}),
                )
            }
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
                };
                save(&session_id, &session);
                sessions.insert(session_id.clone(), session);
                respond(&id, json!({"sessionId":session_id}));
            }
            "session/load" => {
                let session_id = params["sessionId"].as_str().unwrap_or("").to_owned();
                match restore(&session_id) {
                    Some(history) => {
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
                        sessions.insert(
                            session_id,
                            Session {
                                cwd: PathBuf::from(params["cwd"].as_str().unwrap_or(".")),
                                mcp_servers: params["mcpServers"]
                                    .as_array()
                                    .cloned()
                                    .unwrap_or_default(),
                                history,
                            },
                        );
                        respond(&id, json!({}));
                    }
                    None => fail(&id, -32002, "unknown session"),
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
                    "fixture:usage" => {
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
                    _ if prompt.starts_with("fixture:echo ") => {
                        let text = format!("Echo #{turn}: {}", &prompt["fixture:echo ".len()..]);
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
