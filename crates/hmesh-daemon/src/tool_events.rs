//! What a tool call recorded in a session keeps from the agent's ACP
//! `tool_call` / `tool_call_update`.
//!
//! Agents fill the same ACP fields differently: a command's output comes as
//! `content` text (Claude Code, OpenCode, DSH, Kimi), as appended chunks in
//! `_meta.terminal_output` / `terminal_output_delta` (Codex, Pi, Qoder) or in
//! `rawOutput` (MiniMax, Hermes); edits come as `diff` content in hunks or as
//! whole files, or not at all. Everything is kept by meaning, not by agent, so
//! an agent HarnessMesh has never seen is recorded the same way. Every text is
//! redacted and bounded, so one event stays under the store's event limit.

use hmesh_core::sessions::clip;
use hmesh_runtime::{RedactedStream, redact_text as redact};
use serde_json::{Map, Value, json};
use std::collections::HashMap;

/// Title, as before.
const TITLE_BYTES: usize = 512;
/// The tool's input as one JSON string, kept for panels of earlier versions.
const INPUT_BYTES: usize = 2048;
/// The tool's input as structured arguments.
const ARGS_BYTES: usize = 8 * 1024;
const ARG_TEXT_BYTES: usize = 2048;
const ARG_ITEMS: usize = 32;
const ARG_DEPTH: usize = 6;
/// Output text: its start and its end are kept when it is longer.
const OUTPUT_BYTES: usize = 16 * 1024;
/// All diffs of one update together.
const DIFF_BYTES: usize = 24 * 1024;
const DIFFS: usize = 8;
const LOCATIONS: usize = 16;
const PATH_BYTES: usize = 1024;
/// Command output recorded as it arrives, then only its last part.
const TERMINAL_HEAD: usize = 32 * 1024;
const TERMINAL_TAIL: usize = 8 * 1024;
/// Output buffered before it is recorded without waiting for the timer.
const TERMINAL_FLUSH: usize = 2048;

/// Keep the start and the end of a long text, with how much was left out.
pub fn clip_middle(text: &str, limit: usize) -> (String, bool) {
    if text.len() <= limit {
        return (text.to_owned(), false);
    }
    let half = limit / 2;
    let (head, _) = clip(text, half);
    let mut start = text.len() - half;
    while !text.is_char_boundary(start) {
        start += 1;
    }
    let omitted = start - head.len();
    (
        format!("{head}\n[… {omitted} bytes omitted …]\n{}", &text[start..]),
        true,
    )
}

/// Redacted and cut, marked when cut, as session events have always been.
fn truncated(secrets: &[String], value: &str, limit: usize) -> String {
    let (shown, cut) = clip(&redact(secrets, value), limit);
    if cut {
        format!("{shown} [truncated]")
    } else {
        shown
    }
}

fn text(secrets: &[String], value: &str, limit: usize) -> String {
    clip(&redact(secrets, value), limit).0
}

/// The tool's input as JSON within bounds: long strings are cut, long lists
/// and objects shortened, deep values dropped. `None` when even that is too big.
pub fn bounded_args(secrets: &[String], input: &Value) -> Option<Value> {
    fn walk(secrets: &[String], value: &Value, depth: usize) -> Value {
        match value {
            Value::String(s) => Value::String(clip_middle(&redact(secrets, s), ARG_TEXT_BYTES).0),
            Value::Array(items) if depth < ARG_DEPTH => Value::Array(
                items
                    .iter()
                    .take(ARG_ITEMS)
                    .map(|v| walk(secrets, v, depth + 1))
                    .collect(),
            ),
            Value::Object(map) if depth < ARG_DEPTH => Value::Object(
                map.iter()
                    .take(ARG_ITEMS)
                    .map(|(k, v)| (clip(k, 128).0, walk(secrets, v, depth + 1)))
                    .collect::<Map<_, _>>(),
            ),
            Value::Array(_) | Value::Object(_) => Value::String("[…]".into()),
            other => other.clone(),
        }
    }
    if input.is_null() {
        return None;
    }
    let args = walk(secrets, input, 0);
    (args.to_string().len() <= ARGS_BYTES).then_some(args)
}

/// The text of `rawOutput`: a string, or the text field an agent puts its
/// result in (`output`, `stdout`, `result`, `content`, `error`).
fn raw_output_text(raw: &Value) -> Option<String> {
    match raw {
        Value::String(s) if !s.is_empty() => Some(s.clone()),
        Value::Object(map) => ["output", "stdout", "result", "content", "error", "message"]
            .iter()
            .find_map(|k| {
                map.get(*k)
                    .and_then(Value::as_str)
                    .filter(|s| !s.is_empty())
            })
            .map(str::to_owned),
        _ => None,
    }
}

/// The arguments each running call was last seen with. Some agents repeat
/// them as content text, or stream them there before they are final; that
/// is not output.
#[derive(Default)]
pub struct ToolInputs {
    known: HashMap<String, Value>,
}

/// Arguments larger than this are not remembered for the comparison.
const KNOWN_INPUT_BYTES: usize = 64 * 1024;
/// Calls remembered at once; past it the memory starts over.
const KNOWN_CALLS: usize = 256;

impl ToolInputs {
    pub fn payload(&mut self, update: &Value, secrets: &[String]) -> Value {
        let id = update.get("toolCallId").and_then(Value::as_str);
        if let (Some(id), Some(input)) = (id, update.get("rawInput").filter(|v| !v.is_null()))
            && input.to_string().len() <= KNOWN_INPUT_BYTES
        {
            if self.known.len() >= KNOWN_CALLS && !self.known.contains_key(id) {
                self.known.clear();
            }
            self.known.insert(id.to_owned(), input.clone());
        }
        let payload = tool_payload(update, secrets, id.and_then(|i| self.known.get(i)));
        if let Some(id) = id
            && matches!(
                update.get("status").and_then(Value::as_str),
                Some("completed" | "failed" | "cancelled")
            )
        {
            self.known.remove(id);
        }
        payload
    }
}

/// The recorded payload of one `tool_call` or `tool_call_update`. Fields an
/// update does not carry are left out, so the panel keeps what it had.
/// `known_input` is what an earlier update of the call gave as its arguments.
pub fn tool_payload(update: &Value, secrets: &[String], known_input: Option<&Value>) -> Value {
    let raw_input = update.get("rawInput").filter(|v| !v.is_null());
    let input_json = raw_input.map(|v| v.to_string());
    let arguments = raw_input.or(known_input);
    let items: &[Value] = update
        .get("content")
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or_default();
    let mut texts = vec![];
    let mut diffs = vec![];
    let mut terminal = false;
    for item in items {
        match item.get("type").and_then(Value::as_str) {
            Some("content") => {
                if let Some(t) = item.pointer("/content/text").and_then(Value::as_str) {
                    // Some agents repeat the arguments as text, or stream them
                    // there before the call has any; neither is output.
                    let repeat = serde_json::from_str::<Value>(t)
                        .ok()
                        .is_some_and(|v| Some(&v) == arguments);
                    let writing = arguments.is_none() && t.trim_start().starts_with('{');
                    if !repeat && !writing {
                        texts.push(t.to_owned());
                    }
                }
            }
            Some("diff") => diffs.push(item),
            Some("terminal") => terminal = true,
            _ => {}
        }
    }
    let mut output = texts.join("\n");
    if output.is_empty()
        && let Some(raw) = update.get("rawOutput").and_then(raw_output_text)
    {
        output = raw;
    }
    let mut payload = json!({
        "id": update.get("toolCallId"),
        "title": update.get("title").and_then(Value::as_str).map(|t| text(secrets, t, TITLE_BYTES)),
        "kind": update.get("kind"),
        "status": update.get("status"),
        "input": input_json.map(|i| truncated(secrets, &i, INPUT_BYTES)),
        "output": (!output.is_empty()).then(|| clip_middle(&redact(secrets, &output), OUTPUT_BYTES).0),
    });
    if let Some(args) = raw_input.and_then(|v| bounded_args(secrets, v)) {
        payload["args"] = args;
    }
    let locations: Vec<Value> = update
        .get("locations")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|l| {
            let path = l.get("path").and_then(Value::as_str)?;
            Some(json!({"path": text(secrets, path, PATH_BYTES), "line": l.get("line").and_then(Value::as_u64)}))
        })
        .take(LOCATIONS)
        .collect();
    if !locations.is_empty() {
        payload["locations"] = Value::Array(locations);
    }
    if !diffs.is_empty() {
        let shown = diffs.len().min(DIFFS);
        let side = (DIFF_BYTES / (2 * shown)).max(1024);
        let side_text = |d: &Value, key: &str| {
            d.get(key)
                .and_then(Value::as_str)
                .map(|t| clip_middle(&redact(secrets, t), side))
        };
        payload["diffs"] = diffs
            .iter()
            .take(DIFFS)
            .map(|d| {
                let old = side_text(d, "oldText");
                let new = side_text(d, "newText");
                let truncated = old.as_ref().is_some_and(|o| o.1) || new.as_ref().is_some_and(|n| n.1);
                json!({
                    "path": d.get("path").and_then(Value::as_str).map(|p| text(secrets, p, PATH_BYTES)),
                    "old": old.map(|o| o.0),
                    "new": new.map(|n| n.0),
                    "truncated": truncated,
                })
            })
            .collect();
        if diffs.len() > DIFFS {
            payload["diffs_omitted"] = json!(diffs.len() - DIFFS);
        }
    }
    if terminal {
        payload["terminal"] = json!({});
    }
    payload
}

/// One command's output as it streams in through `_meta`.
#[derive(Default)]
struct TerminalLog {
    redact: RedactedStream,
    /// Output not yet recorded.
    pending: String,
    /// Bytes already recorded as appends.
    recorded: usize,
    /// Past the recorded start, the last part of the output.
    tail: String,
    /// Bytes dropped between the recorded start and the tail.
    skipped: usize,
    exit: Option<Value>,
}
impl TerminalLog {
    fn add(&mut self, safe: &str) {
        let room = TERMINAL_HEAD.saturating_sub(self.recorded + self.pending.len());
        let (head, _) = clip(safe, room);
        self.pending.push_str(&head);
        let rest = &safe[head.len()..];
        if rest.is_empty() {
            return;
        }
        self.tail.push_str(rest);
        if self.tail.len() > TERMINAL_TAIL {
            let mut cut = self.tail.len() - TERMINAL_TAIL;
            while !self.tail.is_char_boundary(cut) {
                cut += 1;
            }
            self.skipped += cut;
            self.tail.drain(..cut);
        }
    }
}

/// When buffered command output is recorded.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Drain {
    /// Enough has gathered, or the command exited.
    Ready,
    /// The flush timer fired: record whatever is buffered.
    Timer,
    /// The turn or the process ended: record everything and forget it.
    End,
}

/// Command output of the session's running tool calls, by tool call id.
#[derive(Default)]
pub struct TerminalLogs {
    logs: HashMap<String, TerminalLog>,
}
impl TerminalLogs {
    /// Take the output chunks and the exit an update carries in `_meta`.
    pub fn observe(&mut self, update: &Value, secrets: &[String]) {
        let Some(meta) = update.get("_meta").and_then(Value::as_object) else {
            return;
        };
        let Some(id) = update.get("toolCallId").and_then(Value::as_str) else {
            return;
        };
        let chunks: Vec<&str> = ["terminal_output_delta", "terminal_output"]
            .iter()
            .filter_map(|k| meta.get(*k))
            .filter_map(|v| v.as_str().or_else(|| v.get("data").and_then(Value::as_str)))
            .collect();
        let exit = meta.get("terminal_exit");
        if chunks.is_empty() && exit.is_none() {
            return;
        }
        let log = self.logs.entry(id.to_owned()).or_default();
        for chunk in chunks {
            let safe = log.redact.push(secrets, chunk);
            log.add(&safe);
        }
        if let Some(exit) = exit {
            let rest = log.redact.finish();
            log.add(&rest);
            log.exit = Some(json!({
                "exit_code": exit.get("exit_code").or_else(|| exit.get("exitCode")).cloned().unwrap_or(Value::Null),
                "signal": exit.get("signal").cloned().unwrap_or(Value::Null),
            }));
        }
    }

    pub fn has_pending(&self) -> bool {
        self.logs.values().any(|l| !l.pending.is_empty())
    }

    /// `tool_call_update` payloads to record now. A payload's `terminal`
    /// carries `append` (output to add to what was recorded before) and, once
    /// the command is over, `exit_code`, `signal`, and the `tail` with how
    /// many bytes were `skipped` before it.
    pub fn drain(&mut self, when: Drain) -> Vec<Value> {
        let mut events = vec![];
        let mut done = vec![];
        for (id, log) in &mut self.logs {
            if when == Drain::End {
                let rest = log.redact.finish();
                log.add(&rest);
            }
            let finished = log.exit.is_some() || when == Drain::End;
            let ready = finished || when == Drain::Timer || log.pending.len() >= TERMINAL_FLUSH;
            let mut terminal = Map::new();
            if ready && !log.pending.is_empty() {
                log.recorded += log.pending.len();
                terminal.insert(
                    "append".into(),
                    Value::String(std::mem::take(&mut log.pending)),
                );
            }
            if finished {
                if let Some(exit) = log.exit.take() {
                    terminal.insert("exit_code".into(), exit["exit_code"].clone());
                    terminal.insert("signal".into(), exit["signal"].clone());
                }
                if !log.tail.is_empty() {
                    terminal.insert("tail".into(), Value::String(std::mem::take(&mut log.tail)));
                    terminal.insert("skipped".into(), json!(log.skipped));
                }
                done.push(id.clone());
            }
            if !terminal.is_empty() {
                events.push(json!({"id": id, "terminal": terminal}));
            }
        }
        for id in done {
            self.logs.remove(&id);
        }
        events
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn secrets() -> Vec<String> {
        vec!["sk-test-secret-0123".into()]
    }

    #[test]
    fn claude_style_hunks_locations_and_console_text_are_kept() {
        let update = json!({
            "sessionUpdate": "tool_call_update", "toolCallId": "t1", "status": "completed",
            "kind": "edit", "title": "Edit src/a.ts",
            "locations": [{"path": "/w/src/a.ts", "line": 12}],
            "content": [
                {"type": "diff", "path": "/w/src/a.ts", "oldText": "let a = 1;", "newText": "let a = 2;"},
                {"type": "diff", "path": "/w/src/a.ts", "oldText": "b", "newText": "c"},
                {"type": "content", "content": {"type": "text", "text": "```console\nok\n```"}}
            ],
            "rawInput": {"file_path": "/w/src/a.ts", "old_string": "let a = 1;", "new_string": "let a = 2;"}
        });
        let p = tool_payload(&update, &secrets(), None);
        assert_eq!(p["diffs"].as_array().unwrap().len(), 2);
        assert_eq!(p["diffs"][0]["new"], "let a = 2;");
        assert_eq!(
            p["locations"][0],
            json!({"path": "/w/src/a.ts", "line": 12})
        );
        assert_eq!(p["args"]["file_path"], "/w/src/a.ts");
        assert_eq!(p["output"], "```console\nok\n```");
        assert!(p["input"].as_str().unwrap().contains("old_string"));
    }

    #[test]
    fn a_whole_file_diff_without_old_text_is_a_new_file() {
        let update = json!({"toolCallId": "t", "content": [{"type": "diff", "path": "/w/new.md", "oldText": null, "newText": "# hi"}]});
        let p = tool_payload(&update, &[], None);
        assert!(p["diffs"][0]["old"].is_null());
        assert_eq!(p["diffs"][0]["new"], "# hi");
    }

    #[test]
    fn raw_output_is_the_output_when_content_has_no_text() {
        // OpenCode: {output, metadata}; MiniMax: a string or {error}.
        for (raw, want) in [
            (
                json!({"output": "3 passed", "metadata": {"exit": 0}}),
                "3 passed",
            ),
            (json!("listing"), "listing"),
            (json!({"error": "not found"}), "not found"),
        ] {
            let p = tool_payload(
                &json!({"toolCallId": "t", "status": "completed", "rawOutput": raw}),
                &[],
                None,
            );
            assert_eq!(p["output"], want);
        }
        // Content text wins over rawOutput.
        let p = tool_payload(
            &json!({"toolCallId": "t", "rawOutput": "raw", "content": [{"type": "content", "content": {"type": "text", "text": "shown"}}]}),
            &[],
            None,
        );
        assert_eq!(p["output"], "shown");
    }

    #[test]
    fn input_repeated_as_text_is_not_output() {
        // Kimi puts the arguments' JSON in content next to the diff.
        let input = json!({"path": "a.txt", "content": "x"});
        let update = json!({"toolCallId": "t", "rawInput": input, "content": [{"type": "content", "content": {"type": "text", "text": input.to_string()}}]});
        assert!(tool_payload(&update, &[], None)["output"].is_null());
    }

    #[test]
    fn a_terminal_content_marks_a_command() {
        let p = tool_payload(
            &json!({"toolCallId": "t", "kind": "execute", "content": [{"type": "terminal", "terminalId": "t"}]}),
            &[],
            None,
        );
        assert_eq!(p["terminal"], json!({}));
    }

    #[test]
    fn secrets_are_redacted_everywhere() {
        let s = secrets();
        let leak = "token sk-test-secret-0123 here";
        let update = json!({
            "toolCallId": "t", "title": leak, "rawInput": {"command": leak},
            "locations": [{"path": leak}],
            "content": [{"type": "diff", "path": leak, "oldText": leak, "newText": leak}, {"type": "content", "content": {"type": "text", "text": leak}}]
        });
        let p = tool_payload(&update, &s, None).to_string();
        assert!(!p.contains("sk-test-secret-0123"), "{p}");
    }

    #[test]
    fn large_inputs_stay_within_one_event() {
        let big = "x".repeat(200 * 1024);
        let update = json!({
            "toolCallId": "t", "title": big, "rawInput": {"content": big, "list": vec![big.clone(); 100]},
            "locations": vec![json!({"path": big}); 100],
            "content": (0..20).map(|_| json!({"type": "diff", "path": "p", "oldText": big, "newText": big})).collect::<Vec<_>>()
        });
        let p = tool_payload(&update, &[], None);
        assert!(
            p.to_string().len() < hmesh_core::MAX_EVENT_PAYLOAD_BYTES,
            "{}",
            p.to_string().len()
        );
        assert_eq!(p["diffs"].as_array().unwrap().len(), DIFFS);
        assert_eq!(p["diffs_omitted"], 12);
        assert_eq!(p["locations"].as_array().unwrap().len(), LOCATIONS);
        assert!(p["output"].is_null());
    }

    #[test]
    fn long_output_keeps_its_start_and_end() {
        let out = format!("START{}END", "y".repeat(100 * 1024));
        let p = tool_payload(&json!({"toolCallId": "t", "rawOutput": out}), &[], None);
        let shown = p["output"].as_str().unwrap();
        assert!(
            shown.starts_with("START") && shown.ends_with("END") && shown.contains("bytes omitted")
        );
        assert!(shown.len() < OUTPUT_BYTES + 64);
    }

    #[test]
    fn streamed_command_output_is_appended_then_exits() {
        // Codex: terminal_output_delta; Pi: terminal_output {terminal_id, data}.
        let mut logs = TerminalLogs::default();
        logs.observe(&json!({"toolCallId": "c", "_meta": {"terminal_output_delta": {"terminal_id": "c", "data": "line 1\n"}}}), &[]);
        assert!(logs.has_pending());
        assert!(
            logs.drain(Drain::Ready).is_empty(),
            "small output waits for the timer"
        );
        let timer = logs.drain(Drain::Timer);
        assert_eq!(
            timer,
            vec![json!({"id": "c", "terminal": {"append": "line 1\n"}})]
        );
        logs.observe(
            &json!({"toolCallId": "c", "_meta": {
                "terminal_output": {"terminal_id": "c", "data": "line 2\n"},
                "terminal_exit": {"terminal_id": "c", "exit_code": 1, "signal": null}
            }}),
            &[],
        );
        let done = logs.drain(Drain::Ready);
        assert_eq!(
            done,
            vec![
                json!({"id": "c", "terminal": {"append": "line 2\n", "exit_code": 1, "signal": null}})
            ]
        );
        assert!(
            logs.drain(Drain::End).is_empty(),
            "an exited command is forgotten"
        );
    }

    #[test]
    fn a_secret_split_across_chunks_is_redacted() {
        let s = secrets();
        let mut logs = TerminalLogs::default();
        for part in ["key=sk-test-", "secret-0123 ok"] {
            logs.observe(
                &json!({"toolCallId": "c", "_meta": {"terminal_output_delta": part}}),
                &s,
            );
        }
        let all = logs
            .drain(Drain::End)
            .iter()
            .map(|e| e.to_string())
            .collect::<String>();
        assert!(!all.contains("sk-test-secret-0123"), "{all}");
        assert!(all.contains("ok"));
    }

    #[test]
    fn very_long_command_output_keeps_its_start_and_its_end() {
        let mut logs = TerminalLogs::default();
        let mut appended = 0;
        for i in 0..200 {
            logs.observe(&json!({"toolCallId": "c", "_meta": {"terminal_output_delta": format!("{i:04}{}\n", "z".repeat(1000))}}), &[]);
            for e in logs.drain(Drain::Ready) {
                appended += e["terminal"]["append"].as_str().map_or(0, str::len);
                assert!(e.to_string().len() < hmesh_core::MAX_EVENT_PAYLOAD_BYTES);
            }
        }
        logs.observe(
            &json!({"toolCallId": "c", "_meta": {"terminal_exit": {"exit_code": 0}}}),
            &[],
        );
        let end = logs.drain(Drain::Ready);
        appended += end[0]["terminal"]["append"].as_str().map_or(0, str::len);
        assert_eq!(appended, TERMINAL_HEAD);
        let tail = end[0]["terminal"]["tail"].as_str().unwrap();
        assert!(tail.len() <= TERMINAL_TAIL && tail.contains("0199"));
        assert!(end[0]["terminal"]["skipped"].as_u64().unwrap() > 100_000);
        assert_eq!(end[0]["terminal"]["exit_code"], 0);
    }

    #[test]
    fn output_of_a_command_that_never_exits_is_kept_at_the_end() {
        let mut logs = TerminalLogs::default();
        logs.observe(
            &json!({"toolCallId": "c", "_meta": {"terminal_output_delta": "partial"}}),
            &[],
        );
        let end = logs.drain(Drain::End);
        assert_eq!(
            end,
            vec![json!({"id": "c", "terminal": {"append": "partial"}})]
        );
    }

    #[test]
    fn arguments_streamed_or_repeated_as_text_are_not_output() {
        // Kimi: the call opens before its arguments are known and streams
        // them as content text, then repeats them once they are final.
        let mut inputs = ToolInputs::default();
        let text = |t: &str| json!([{"type": "content", "content": {"type": "text", "text": t}}]);
        let opened = inputs.payload(&json!({"sessionUpdate": "tool_call", "toolCallId": "k", "title": "Grep", "status": "pending", "content": text("{\"pat")}), &[]);
        assert!(opened["output"].is_null());
        let streaming = inputs.payload(&json!({"toolCallId": "k", "status": "in_progress", "content": text("{\"pattern\":\"TODO\"")}), &[]);
        assert!(streaming["output"].is_null());
        let args = json!({"pattern": "TODO", "path": "."});
        let started = inputs.payload(&json!({"toolCallId": "k", "status": "in_progress", "rawInput": args, "content": text(&args.to_string())}), &[]);
        assert!(started["output"].is_null());
        assert_eq!(started["args"]["pattern"], "TODO");
        // Without the arguments in this update, the remembered ones still tell.
        let again = inputs.payload(&json!({"toolCallId": "k", "status": "in_progress", "content": text(&args.to_string())}), &[]);
        assert!(again["output"].is_null());
        let finished = inputs.payload(&json!({"toolCallId": "k", "status": "completed", "content": text("src/main.rs:12: // TODO")}), &[]);
        assert_eq!(finished["output"], "src/main.rs:12: // TODO");
        assert!(inputs.known.is_empty(), "a finished call is forgotten");
        // Real JSON output of a call with known arguments stays output.
        let mut inputs = ToolInputs::default();
        inputs.payload(&json!({"toolCallId": "c", "status": "pending", "rawInput": {"command": "cat package.json"}}), &[]);
        let output = inputs.payload(&json!({"toolCallId": "c", "status": "completed", "content": text("{\"name\": \"demo\"}")}), &[]);
        assert_eq!(output["output"], "{\"name\": \"demo\"}");
    }
}
