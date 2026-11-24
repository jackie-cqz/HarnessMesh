//! Agent Client Protocol (ACP) v1 client pieces shared by one-shot tasks and
//! interactive sessions. Only the subset HarnessMesh needs is implemented;
//! field names follow `agent-client-protocol-schema` 1.22.0 (camelCase, with
//! `sessionUpdate` / `outcome` as variant tags).
use hmesh_core::development::UsageTotals;
use hmesh_core::{ErrorCode, MeshError, Result};
use serde_json::{Value, json};
use std::collections::HashMap;
use std::path::Path;

pub const PROTOCOL_VERSION: u64 = 1;
const METHOD_NOT_FOUND: i64 = -32601;

/// The client does not offer file-system or terminal methods, so agents use
/// their own tools inside the project directory.
pub fn initialize_request(id: u64) -> Value {
    json!({"jsonrpc":"2.0","id":id,"method":"initialize","params":{
        "protocolVersion":PROTOCOL_VERSION,
        "clientCapabilities":{"fs":{"readTextFile":false,"writeTextFile":false},"terminal":false},
        "clientInfo":{"name":"harnessmesh","version":env!("CARGO_PKG_VERSION")}
    }})
}
pub fn new_session_request(id: u64, cwd: &Path, mcp_servers: &[Value]) -> Value {
    json!({"jsonrpc":"2.0","id":id,"method":"session/new","params":{"cwd":cwd,"mcpServers":mcp_servers}})
}
pub fn load_session_request(id: u64, session_id: &str, cwd: &Path, mcp_servers: &[Value]) -> Value {
    json!({"jsonrpc":"2.0","id":id,"method":"session/load","params":{"sessionId":session_id,"cwd":cwd,"mcpServers":mcp_servers}})
}
pub fn prompt_request(id: u64, session_id: &str, text: &str) -> Value {
    json!({"jsonrpc":"2.0","id":id,"method":"session/prompt","params":{"sessionId":session_id,"prompt":[{"type":"text","text":text}]}})
}
pub fn cancel_notification(session_id: &str) -> Value {
    json!({"jsonrpc":"2.0","method":"session/cancel","params":{"sessionId":session_id}})
}
/// A stdio MCP server entry for `session/new` (`McpServerStdio`).
pub fn stdio_mcp_server(
    name: &str,
    command: &Path,
    args: &[String],
    env: &[(String, String)],
) -> Value {
    let env: Vec<Value> = env
        .iter()
        .map(|(name, value)| json!({"name":name,"value":value}))
        .collect();
    json!({"name":name,"command":command,"args":args,"env":env})
}
pub fn result_response(id: &Value, result: Value) -> Value {
    json!({"jsonrpc":"2.0","id":id,"result":result})
}
pub fn error_response(id: &Value, code: i64, message: &str) -> Value {
    json!({"jsonrpc":"2.0","id":id,"error":{"code":code,"message":message}})
}
/// Reply to an agent request this client does not implement.
pub fn method_not_found(id: &Value, method: &str) -> Value {
    error_response(
        id,
        METHOD_NOT_FOUND,
        &format!("{method} is not offered by this client"),
    )
}

/// One decoded JSON-RPC frame from the agent.
#[derive(Debug, Clone, PartialEq)]
pub enum Incoming {
    /// A response to one of our requests.
    Response {
        id: u64,
        result: std::result::Result<Value, String>,
    },
    /// A notification such as `session/update`.
    Notification { method: String, params: Value },
    /// A request the agent expects us to answer, such as
    /// `session/request_permission`.
    Request {
        id: Value,
        method: String,
        params: Value,
    },
}
pub fn classify(frame: &Value) -> Option<Incoming> {
    let method = frame.get("method").and_then(Value::as_str);
    match (frame.get("id"), method) {
        (Some(id), Some(method)) if !id.is_null() => Some(Incoming::Request {
            id: id.clone(),
            method: method.to_owned(),
            params: frame.get("params").cloned().unwrap_or(Value::Null),
        }),
        (_, Some(method)) => Some(Incoming::Notification {
            method: method.to_owned(),
            params: frame.get("params").cloned().unwrap_or(Value::Null),
        }),
        (Some(id), None) => {
            let id = id.as_u64()?;
            let result = match frame.get("error") {
                Some(error) => Err(error
                    .get("message")
                    .and_then(Value::as_str)
                    .unwrap_or("agent returned an error")
                    .chars()
                    .take(500)
                    .collect()),
                None => Ok(frame.get("result").cloned().unwrap_or(Value::Null)),
            };
            Some(Incoming::Response { id, result })
        }
        _ => None,
    }
}

/// Pick a permission outcome from the agent's offered options: a one-time
/// allow or reject when offered, the remembered variant otherwise, and
/// `cancelled` when nothing fits. Remembered decisions are never chosen when a
/// one-time option exists.
pub fn permission_outcome(options: &[Value], allow: bool) -> Value {
    let kinds: &[&str] = if allow {
        &["allow_once", "allow_always"]
    } else {
        &["reject_once", "reject_always"]
    };
    for kind in kinds {
        if let Some(id) = options
            .iter()
            .find(|o| o.get("kind").and_then(Value::as_str) == Some(kind))
            .and_then(|o| o.get("optionId"))
        {
            return json!({"outcome":{"outcome":"selected","optionId":id}});
        }
    }
    json!({"outcome":{"outcome":"cancelled"}})
}
/// Whether a project allowlist (Pi-style tool names) covers an ACP tool kind.
pub fn tool_kind_allowed(kind: Option<&str>, allowed: &[String]) -> bool {
    let has = |names: &[&str]| allowed.iter().any(|a| names.contains(&a.as_str()));
    match kind.unwrap_or("other") {
        "read" | "search" => has(&["read", "grep", "find", "ls"]),
        "edit" | "delete" | "move" => has(&["write", "edit"]),
        "execute" => has(&["bash"]),
        "think" => true,
        _ => false,
    }
}
/// Token totals reported with a prompt response (`PromptResponse.usage`).
pub fn usage_from_prompt(result: &Value) -> Option<UsageTotals> {
    let usage = result.get("usage")?;
    let get = |key: &str| {
        usage
            .get(key)
            .and_then(Value::as_u64)
            .filter(|n| *n <= 1_000_000_000)
    };
    let totals = UsageTotals {
        input_tokens: get("inputTokens"),
        output_tokens: get("outputTokens"),
        cache_read_tokens: get("cachedReadTokens"),
        cache_write_tokens: get("cachedWriteTokens"),
        total_tokens: get("totalTokens"),
        reports: 1,
        source: Some("acp.prompt_response.usage".into()),
        ..UsageTotals::default()
    };
    (totals.input_tokens.is_some() || totals.total_tokens.is_some()).then_some(totals)
}
/// Text carried by an `agent_message_chunk` update, if any.
pub fn message_text(update: &Value) -> Option<&str> {
    (update.get("sessionUpdate").and_then(Value::as_str) == Some("agent_message_chunk"))
        .then(|| update.pointer("/content/text").and_then(Value::as_str))
        .flatten()
}
/// Readable stop reasons for failures other than a finished turn.
pub fn stop_reason_error(reason: &str) -> MeshError {
    match reason {
        "cancelled" => MeshError::new(ErrorCode::TaskUnknown, "the agent cancelled the turn"),
        "refusal" => MeshError::new(ErrorCode::AgentUnavailable, "the agent refused to continue"),
        "max_tokens" | "max_turn_requests" => MeshError::new(
            ErrorCode::AgentUnavailable,
            format!("the agent stopped early ({reason})"),
        ),
        _ => MeshError::new(
            ErrorCode::AgentUnavailable,
            "the agent ended the turn without a recognized stop reason",
        ),
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Phase {
    Initializing,
    CreatingSession,
    Prompting,
    Done,
}

/// Drives one non-interactive ACP turn for a task: initialize, open a session
/// in the project, send the prompt and answer permission requests from the
/// project's tool allowlist. Feed every decoded frame to `observe` and write
/// `take_outbound` frames to the agent's stdin.
pub struct AcpTaskDriver {
    cwd: std::path::PathBuf,
    prompt: String,
    mcp_servers: Vec<Value>,
    allowed_tools: Vec<String>,
    phase: Phase,
    session_id: Option<String>,
    outbound: Vec<Value>,
    completed: bool,
    failure: Option<MeshError>,
    needs_input: bool,
    tools: HashMap<String, (String, Option<String>)>,
}
const INITIALIZE_ID: u64 = 1;
const NEW_SESSION_ID: u64 = 2;
const PROMPT_ID: u64 = 3;
impl AcpTaskDriver {
    pub fn new(
        cwd: &Path,
        prompt: &str,
        mcp_servers: Vec<Value>,
        allowed_tools: &[String],
    ) -> Self {
        Self {
            cwd: cwd.to_path_buf(),
            prompt: prompt.to_owned(),
            mcp_servers,
            allowed_tools: allowed_tools.to_vec(),
            phase: Phase::Initializing,
            session_id: None,
            outbound: vec![initialize_request(INITIALIZE_ID)],
            completed: false,
            failure: None,
            needs_input: false,
            tools: HashMap::new(),
        }
    }
    pub fn take_outbound(&mut self) -> Vec<Value> {
        std::mem::take(&mut self.outbound)
    }
    /// The turn ended (successfully or not); stdin may be closed.
    pub fn settled(&self) -> bool {
        self.phase == Phase::Done
    }
    pub fn session_id(&self) -> Option<&str> {
        self.session_id.as_deref()
    }
    fn fail(&mut self, error: MeshError) {
        if self.failure.is_none() {
            self.failure = Some(error);
        }
        self.phase = Phase::Done;
    }
    pub fn observe(&mut self, frame: &Value) -> Vec<crate::ObservedEvent> {
        use crate::ObservedEvent;
        let mut events = vec![];
        let Some(incoming) = classify(frame) else {
            return events;
        };
        match incoming {
            Incoming::Response { id, result } => match (id, result) {
                (_, Err(message)) if self.phase != Phase::Done => {
                    let step = match id {
                        INITIALIZE_ID => "initialize",
                        NEW_SESSION_ID => "session/new",
                        _ => "session/prompt",
                    };
                    self.fail(MeshError::new(
                        ErrorCode::AgentUnavailable,
                        format!("agent rejected {step}: {message}"),
                    ));
                }
                (INITIALIZE_ID, Ok(result)) if self.phase == Phase::Initializing => {
                    match result.get("protocolVersion").and_then(Value::as_u64) {
                        Some(PROTOCOL_VERSION) => {
                            self.phase = Phase::CreatingSession;
                            self.outbound.push(new_session_request(
                                NEW_SESSION_ID,
                                &self.cwd,
                                &self.mcp_servers,
                            ));
                        }
                        other => self.fail(MeshError::new(
                            ErrorCode::AdapterNotSupported,
                            format!(
                                "agent speaks ACP protocol version {other:?}; version 1 is required"
                            ),
                        )),
                    }
                }
                (NEW_SESSION_ID, Ok(result)) if self.phase == Phase::CreatingSession => {
                    match result.get("sessionId").and_then(Value::as_str) {
                        Some(session) => {
                            self.session_id = Some(session.to_owned());
                            self.phase = Phase::Prompting;
                            self.outbound
                                .push(prompt_request(PROMPT_ID, session, &self.prompt));
                            events.push(ObservedEvent::Started);
                        }
                        None => self.fail(MeshError::new(
                            ErrorCode::AgentUnavailable,
                            "agent created a session without an ID",
                        )),
                    }
                }
                (PROMPT_ID, Ok(result)) if self.phase == Phase::Prompting => {
                    if let Some(usage) = usage_from_prompt(&result) {
                        events.push(ObservedEvent::Usage { usage });
                    }
                    match result.get("stopReason").and_then(Value::as_str) {
                        Some("end_turn") => {
                            self.completed = true;
                            self.phase = Phase::Done;
                        }
                        reason => self.fail(stop_reason_error(reason.unwrap_or(""))),
                    }
                }
                _ => {}
            },
            Incoming::Notification { method, params } if method == "session/update" => {
                let update = &params["update"];
                if let Some(text) = message_text(update) {
                    events.push(ObservedEvent::TextDelta { text: text.into() });
                }
                let kind = update.get("sessionUpdate").and_then(Value::as_str);
                if matches!(kind, Some("tool_call" | "tool_call_update"))
                    && let Some(call) = update.get("toolCallId").and_then(Value::as_str)
                {
                    let known = self.tools.get(call).cloned();
                    let title = update
                        .get("title")
                        .and_then(Value::as_str)
                        .map(str::to_owned)
                        .or_else(|| known.as_ref().map(|(t, _)| t.clone()))
                        .unwrap_or_else(|| "tool".into());
                    let tool_kind = update
                        .get("kind")
                        .and_then(Value::as_str)
                        .map(str::to_owned)
                        .or_else(|| known.as_ref().and_then(|(_, k)| k.clone()));
                    self.tools
                        .insert(call.to_owned(), (title.clone(), tool_kind.clone()));
                    let name = tool_kind.unwrap_or_else(|| title.clone());
                    match (kind, update.get("status").and_then(Value::as_str)) {
                        (_, Some("completed" | "failed")) => {
                            events.push(ObservedEvent::ToolEvent {
                                name,
                                phase: "tool_execution_end".into(),
                            })
                        }
                        (Some("tool_call"), _) => events.push(ObservedEvent::ToolEvent {
                            name,
                            phase: "tool_execution_start".into(),
                        }),
                        _ => {}
                    }
                }
            }
            Incoming::Notification { .. } => {}
            Incoming::Request { id, method, params } => {
                if method == "session/request_permission" {
                    let call = &params["toolCall"];
                    let kind = call.get("kind").and_then(Value::as_str).or_else(|| {
                        call.get("toolCallId")
                            .and_then(Value::as_str)
                            .and_then(|c| self.tools.get(c))
                            .and_then(|(_, k)| k.as_deref())
                    });
                    let allow = tool_kind_allowed(kind, &self.allowed_tools);
                    let options = params["options"].as_array().cloned().unwrap_or_default();
                    self.outbound
                        .push(result_response(&id, permission_outcome(&options, allow)));
                    if !allow {
                        // A background run cannot ask anyone; the refusal is
                        // reported as a run that needed input.
                        self.needs_input = true;
                        events.push(ObservedEvent::ApprovalRequested);
                    }
                } else {
                    self.outbound.push(method_not_found(&id, &method));
                }
            }
        }
        events
    }
    pub fn finish(&self) -> Result<()> {
        if self.needs_input {
            return Err(MeshError::new(
                ErrorCode::FailedNeedsInput,
                "the agent asked for a tool outside the project allowlist; background runs cannot approve it",
            ));
        }
        if let Some(error) = &self.failure {
            return Err(error.clone());
        }
        if !self.completed {
            return Err(MeshError::new(
                ErrorCode::TaskUnknown,
                "the agent exited before finishing the turn",
            ));
        }
        Ok(())
    }
}
