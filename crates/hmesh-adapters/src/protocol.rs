use hmesh_core::{ErrorCode, HarnessKind, MeshError, Result};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashMap;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ObservedEvent {
    Started,
    TextDelta { text: String },
    ToolEvent { name: String, phase: String },
    ApprovalRequested,
}

pub struct ProtocolObserver {
    kind: HarnessKind,
    completed: bool,
    assistant_stopped: bool,
    assistant_failed: bool,
    failed: bool,
    needs_input: bool,
    calls: HashMap<String, String>,
}
impl ProtocolObserver {
    pub fn new(kind: HarnessKind) -> Self {
        Self {
            kind,
            completed: false,
            assistant_stopped: false,
            assistant_failed: false,
            failed: false,
            needs_input: false,
            calls: HashMap::new(),
        }
    }
    pub fn settled(&self) -> bool {
        self.completed
    }
    pub fn failed_or_waiting(&self) -> bool {
        self.failed || self.needs_input
    }
    pub fn observe(&mut self, record: &Value) -> Vec<ObservedEvent> {
        let mut result = vec![];
        let kind = record.get("type").and_then(Value::as_str).unwrap_or("");
        match self.kind {
            HarnessKind::Pi => match kind {
                "agent_start" => result.push(ObservedEvent::Started),
                "agent_settled" => self.completed = true,
                "response" if record.get("success") == Some(&Value::Bool(false)) => {
                    self.failed = true
                }
                "message_end"
                    if record.pointer("/message/role").and_then(Value::as_str)
                        == Some("assistant") =>
                {
                    let reason = record
                        .pointer("/message/stopReason")
                        .and_then(Value::as_str);
                    // Pi may retry a transient assistant failure internally. Only its
                    // last assistant outcome at agent_settled proves the result.
                    self.assistant_stopped = reason == Some("stop");
                    self.assistant_failed = matches!(reason, Some("error" | "aborted" | "length"));
                }
                "message_update" => {
                    if record
                        .pointer("/assistantMessageEvent/type")
                        .and_then(Value::as_str)
                        == Some("text_delta")
                        && let Some(text) = record
                            .pointer("/assistantMessageEvent/delta")
                            .and_then(Value::as_str)
                    {
                        result.push(ObservedEvent::TextDelta { text: text.into() });
                    }
                }
                "tool_execution_start" | "tool_execution_end" => {
                    result.push(ObservedEvent::ToolEvent {
                        name: record
                            .get("toolName")
                            .and_then(Value::as_str)
                            .unwrap_or("unknown")
                            .into(),
                        phase: kind.into(),
                    })
                }
                "extension_ui_request" => {
                    self.needs_input = true;
                    result.push(ObservedEvent::ApprovalRequested);
                }
                _ => {}
            },
            HarnessKind::Dsh => match kind {
                "session" => result.push(ObservedEvent::Started),
                "final" => self.completed = true,
                "error" => self.failed = true,
                "text" => {
                    if let Some(text) = record.get("text").and_then(Value::as_str) {
                        result.push(ObservedEvent::TextDelta { text: text.into() });
                    }
                }
                "tool_call" => {
                    let name = record
                        .get("tool")
                        .and_then(Value::as_str)
                        .unwrap_or("unknown")
                        .to_owned();
                    if let Some(id) = record.get("callId").and_then(Value::as_str) {
                        self.calls.insert(id.into(), name.clone());
                    }
                    result.push(ObservedEvent::ToolEvent {
                        name,
                        phase: "started".into(),
                    });
                }
                "tool_result" => {
                    let name = record
                        .get("callId")
                        .and_then(Value::as_str)
                        .and_then(|id| self.calls.remove(id))
                        .unwrap_or_else(|| "unknown".into());
                    result.push(ObservedEvent::ToolEvent {
                        name,
                        phase: record
                            .get("status")
                            .and_then(Value::as_str)
                            .unwrap_or("unknown")
                            .into(),
                    });
                }
                "status" if record.get("phase").and_then(Value::as_str) == Some("turn_end") => {
                    if matches!(
                        record.get("reason").and_then(Value::as_str),
                        Some("error" | "aborted" | "max_tokens")
                    ) {
                        self.failed = true;
                    }
                }
                _ => {}
            },
            HarnessKind::Opencode => match kind {
                "step_start" => result.push(ObservedEvent::Started),
                "step_finish"
                    if record.pointer("/part/reason").and_then(Value::as_str) == Some("stop") =>
                {
                    self.completed = true
                }
                "error" => {
                    self.failed = true;
                    let name = record
                        .pointer("/error/name")
                        .and_then(Value::as_str)
                        .unwrap_or("");
                    if name.contains("Permission") || name.contains("Question") {
                        self.needs_input = true;
                        result.push(ObservedEvent::ApprovalRequested);
                    }
                }
                "text" => {
                    if let Some(text) = record.pointer("/part/text").and_then(Value::as_str) {
                        result.push(ObservedEvent::TextDelta { text: text.into() });
                    }
                }
                "tool_use" => result.push(ObservedEvent::ToolEvent {
                    name: record
                        .pointer("/part/tool")
                        .and_then(Value::as_str)
                        .unwrap_or("unknown")
                        .into(),
                    phase: record
                        .pointer("/part/state/status")
                        .and_then(Value::as_str)
                        .unwrap_or("unknown")
                        .into(),
                }),
                _ => {}
            },
        }
        result
    }
    pub fn finish(&self, exit_code: Option<i32>) -> Result<()> {
        if self.needs_input {
            return Err(MeshError::new(
                ErrorCode::FailedNeedsInput,
                "non-interactive harness requested input or tool approval",
            ));
        }
        if exit_code != Some(0) || self.failed || self.assistant_failed {
            return Err(MeshError::new(
                ErrorCode::AgentUnavailable,
                "harness reported failure or a nonzero exit",
            ));
        }
        if !self.completed || (self.kind == HarnessKind::Pi && !self.assistant_stopped) {
            return Err(MeshError::new(
                ErrorCode::TaskUnknown,
                "process exited without a proven successful harness outcome",
            ));
        }
        Ok(())
    }
}
