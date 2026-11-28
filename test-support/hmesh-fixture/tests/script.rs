//! `--script`: the fixture plays a scripted conversation over ACP.
use serde_json::{Value, json};
use std::{
    io::{BufRead, BufReader, Write},
    process::{Child, ChildStdin, Command, Stdio},
    sync::mpsc,
    time::Duration,
};

struct Agent {
    child: Child,
    stdin: ChildStdin,
    frames: mpsc::Receiver<Value>,
}
impl Agent {
    fn start(script: &Value) -> (Self, tempfile::TempDir) {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("script.json");
        std::fs::write(&path, script.to_string()).unwrap();
        let mut child = Command::new(env!("CARGO_BIN_EXE_hmesh-fixture"))
            .arg("--script")
            .arg(&path)
            .args(["--models", "standard,deep", "--mode", "code", "--no-list"])
            .arg("--state-dir")
            .arg(temp.path())
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        let stdin = child.stdin.take().unwrap();
        let stdout = BufReader::new(child.stdout.take().unwrap());
        let (sender, frames) = mpsc::channel();
        std::thread::spawn(move || {
            for line in stdout.lines().map_while(Result::ok) {
                if let Ok(frame) = serde_json::from_str(&line) {
                    let _ = sender.send(frame);
                }
            }
        });
        (
            Self {
                child,
                stdin,
                frames,
            },
            temp,
        )
    }
    fn send(&mut self, frame: Value) {
        writeln!(self.stdin, "{frame}").unwrap();
    }
    fn next(&self) -> Value {
        self.frames
            .recv_timeout(Duration::from_secs(10))
            .expect("the fixture answered")
    }
    /// Frames up to and including the one `last` accepts.
    fn until(&self, last: impl Fn(&Value) -> bool) -> Vec<Value> {
        let mut seen = vec![];
        loop {
            let frame = self.next();
            let done = last(&frame);
            seen.push(frame);
            if done {
                return seen;
            }
        }
    }
}
impl Drop for Agent {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}
fn updates(frames: &[Value]) -> Vec<&Value> {
    frames
        .iter()
        .filter(|f| f["method"] == "session/update")
        .map(|f| &f["params"]["update"])
        .collect()
}
fn said(text: &str) -> Value {
    json!({"update":{"sessionUpdate":"agent_message_chunk","content":{"type":"text","text":text}}})
}

#[test]
fn a_script_plays_the_turn_its_prompt_names() {
    let script = json!({"turns":[
        {"when":"install","steps":[
            {"update":{"sessionUpdate":"tool_call","toolCallId":"f2","title":"Run npm install","kind":"execute","status":"pending"}},
            {"ask":{"toolCall":{"toolCallId":"f2"},"options":[
                {"optionId":"allow","name":"Allow once","kind":"allow_once"},
                {"optionId":"reject","name":"Reject","kind":"reject_once"}]},
             "answers":{"allow":[said("installed")],"reject":[said("not installed")]}}
        ]},
        {"when":"review","steps":[
            {"delegate":{"toolCallId":"d1","title":"mcp__harnessmesh__mesh_send","agent":"laptop/reviewer","message":"Please review"}}
        ]},
        {"when":"","steps":[said("anything else")]}
    ]});
    let (mut agent, _temp) = Agent::start(&script);
    agent
        .send(json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":1}}));
    agent.until(|f| f["id"] == 1);
    agent.send(json!({"jsonrpc":"2.0","id":2,"method":"session/new","params":{"cwd":"/work","mcpServers":[]}}));
    let created = agent.until(|f| f["id"] == 2).pop().unwrap();
    let session = created["result"]["sessionId"].as_str().unwrap().to_owned();
    // `--models` and `--mode` choose what the session offers and starts with.
    let options = created["result"]["configOptions"].as_array().unwrap();
    let model = options.iter().find(|o| o["id"] == "model").unwrap();
    assert_eq!(model["currentValue"], "standard");
    assert_eq!(model["options"].as_array().unwrap().len(), 2);
    assert_eq!(
        options.iter().find(|o| o["id"] == "mode").unwrap()["currentValue"],
        "code"
    );

    let prompt = |id: u64, text: &str| json!({"jsonrpc":"2.0","id":id,"method":"session/prompt","params":{"sessionId":session,"prompt":[{"type":"text","text":text}]}});
    // A permission request, and the steps of the option chosen.
    agent.send(prompt(3, "please install it"));
    let asked = agent.until(|f| f["method"] == "session/request_permission");
    assert_eq!(updates(&asked)[0]["toolCallId"], "f2");
    let request = asked.last().unwrap();
    assert_eq!(request["params"]["sessionId"], session.as_str());
    assert_eq!(request["params"]["toolCall"]["toolCallId"], "f2");
    agent.send(json!({"jsonrpc":"2.0","id":request["id"],"result":{"outcome":{"outcome":"selected","optionId":"allow"}}}));
    let answered = agent.until(|f| f["id"] == 3);
    assert_eq!(updates(&answered)[0]["content"]["text"], "installed");
    assert_eq!(answered.last().unwrap()["result"]["stopReason"], "end_turn");

    // A call to another agent is a tool call; without a mesh server it fails.
    agent.send(prompt(4, "review this"));
    let delegated = agent.until(|f| f["id"] == 4);
    let steps = updates(&delegated);
    assert_eq!(
        steps[0]["rawInput"],
        json!({"agent":"laptop/reviewer","message":"Please review"})
    );
    assert_eq!(steps[0]["title"], "mcp__harnessmesh__mesh_send");
    assert_eq!(steps[1]["status"], "failed");

    // An empty `when` takes any other prompt.
    agent.send(prompt(5, "something"));
    let other = agent.until(|f| f["id"] == 5);
    assert_eq!(updates(&other)[0]["content"]["text"], "anything else");
}

#[test]
fn the_readme_script_is_a_script() {
    let path = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../scripts/readme-screenshots.json"
    );
    let script: Value = serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap();
    let turns = script["turns"].as_array().unwrap();
    assert!(
        turns
            .iter()
            .all(|t| t["when"].is_string() && t["steps"].is_array())
    );
    assert_eq!(
        turns.last().unwrap()["when"],
        "",
        "the last turn takes any prompt"
    );
}
