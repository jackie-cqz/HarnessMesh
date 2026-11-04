use hmesh_core::development::UsageTotals;
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
    Usage { usage: UsageTotals },
}

pub struct ProtocolObserver {
    kind: HarnessKind,
    completed: bool,
    assistant_stopped: bool,
    assistant_failed: bool,
    failed: bool,
    needs_input: bool,
    calls: HashMap<String, String>,
    usage: UsageTotals,
    usage_ids: std::collections::HashSet<String>,
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
            usage: UsageTotals::default(),
            usage_ids: std::collections::HashSet::new(),
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
        if let Some(usage) = self.observe_usage(record, kind) {
            result.push(ObservedEvent::Usage { usage });
        }
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
    fn observe_usage(&mut self, record: &Value, kind: &str) -> Option<UsageTotals> {
        let (value, fields, id, source) = match self.kind {
            HarnessKind::Pi
                if kind == "message_end"
                    && record.pointer("/message/role")?.as_str()? == "assistant" =>
            {
                (
                    record.pointer("/message/usage")?,
                    ["input", "output", "cacheRead", "cacheWrite", "totalTokens"],
                    record
                        .pointer("/message/id")
                        .or_else(|| record.pointer("/message/timestamp")),
                    "pi.message_end.usage",
                )
            }
            HarnessKind::Opencode if kind == "step_finish" => (
                record.pointer("/part/tokens")?,
                ["input", "output", "/cache/read", "/cache/write", "total"],
                record.pointer("/part/id"),
                "opencode.step_finish.tokens",
            ),
            // DSH's pinned fixture does not establish a usage schema.
            _ => return None,
        };
        let get = |key: &str| {
            if key.starts_with('/') {
                value.pointer(key)
            } else {
                value.get(key)
            }
        };
        let values: Vec<Option<u64>> = fields
            .iter()
            .map(|f| {
                get(f)
                    .and_then(Value::as_u64)
                    .filter(|n| *n <= 1_000_000_000)
            })
            .collect();
        if values.iter().all(Option::is_none) {
            return None;
        }
        if let Some(id) = id.filter(|v| v.is_string() || v.is_u64())
            && !self.usage_ids.insert(id.to_string())
        {
            return None;
        }
        let sum = |old: Option<u64>, new: Option<u64>| {
            if self.usage.reports > 0 && old.is_none() {
                None
            } else {
                new.map(|n| old.unwrap_or(0).saturating_add(n))
            }
        };
        self.usage.input_tokens = sum(self.usage.input_tokens, values[0]);
        self.usage.output_tokens = sum(self.usage.output_tokens, values[1]);
        self.usage.cache_read_tokens = sum(self.usage.cache_read_tokens, values[2]);
        self.usage.cache_write_tokens = sum(self.usage.cache_write_tokens, values[3]);
        self.usage.total_tokens = sum(self.usage.total_tokens, values[4]);
        self.usage.reports += 1;
        self.usage.source = Some(source.into());
        Some(self.usage.clone())
    }
    pub fn usage(&self) -> &UsageTotals {
        &self.usage
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
