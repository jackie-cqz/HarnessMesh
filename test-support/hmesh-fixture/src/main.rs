//! Test-only protocol process. Never calls a model; never counts as harness smoke.
use serde_json::{Value, json};
use std::{
    io::{BufRead, Write},
    process::{Command, Stdio},
    time::Duration,
};

fn emit(value: Value) {
    println!("{value}");
    let _ = std::io::stdout().flush();
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
    let mut lines = std::io::stdin().lock().lines();
    let line = lines
        .next()
        .ok_or_else(|| anyhow::anyhow!("no fixture prompt"))??;
    let record: Value = serde_json::from_str(&line)?;
    let prompt = record["message"].as_str().unwrap_or("");
    emit(json!({"type":"response","command":"prompt","success":true}));
    emit(json!({"type":"agent_start"}));
    match prompt {
        "fixture:create" => {
            std::fs::create_dir_all("out")?;
            std::fs::write(
                "out/health.json",
                b"{\"status\":\"ok\",\"harness\":\"fixture\"}\n",
            )?;
            emit(json!({"type":"message_end","message":{"role":"assistant","stopReason":"stop"}}));
            emit(json!({"type":"agent_settled"}));
        }
        "fixture:needs-input" => {
            emit(json!({"type":"extension_ui_request","id":"fixture-approval","method":"confirm"}))
        }
        "fixture:malformed" => {
            println!("invalid-json");
        }
        "fixture:oversized" => {
            println!("{}", "x".repeat(5 * 1024 * 1024));
        }
        "fixture:large-events" => {
            let text = "中🙂\n\u{0000}\"\\".repeat(10000);
            emit(
                json!({"type":"message_update","message":{"content":[{"type":"thinking","thinking":"snapshot".repeat(15000)}]},"assistantMessageEvent":{"type":"text_delta","delta":text}}),
            );
            emit(
                json!({"type":"tool_execution_start","toolName":"write","args":{"content":"code".repeat(20000)}}),
            );
            emit(
                json!({"type":"tool_execution_end","toolName":"write","result":{"content":"result".repeat(15000)}}),
            );
            emit(
                json!({"type":"message_end","message":{"role":"assistant","stopReason":"stop","content":[{"type":"text","text":text}]}}),
            );
            emit(json!({"type":"agent_settled"}));
        }
        "fixture:native-over-limit" => {
            emit(json!({"type":"message_update","snapshot":"x".repeat(4 * 1024 * 1024)}));
        }
        "fixture:crash" => std::process::exit(7),
        "fixture:child" | "fixture:escaped-child" => {
            let mut command = Command::new(std::env::current_exe()?);
            command
                .arg("--child")
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
            std::fs::write("owned-child.pid", child.id().to_string())?;
            let _child = child;
            loop {
                std::thread::sleep(Duration::from_secs(1));
            }
        }
        "fixture:wait" => loop {
            std::thread::sleep(Duration::from_secs(1));
        },
        _ => emit(json!({"type":"response","command":"prompt","success":false})),
    }
    // Match Pi's EOF shutdown contract after the adapter receives a settled event.
    for line in lines {
        let _ = line?;
    }
    Ok(())
}
