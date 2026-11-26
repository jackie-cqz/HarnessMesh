//! Interactive ACP sessions: one agent process per open session, events stored
//! in order and streamed by long-polling. Sessions on a paired node's agent are
//! opened and driven through that node's peer API.
use crate::{Actor, ApiError, ApiResult, AppState};
use axum::{
    Extension, Json, Router,
    extract::{Path, Query, State},
    http::HeaderMap,
    routing::{get, post},
};
use hmesh_adapters::acp;
use hmesh_core::service::{
    AgentServiceView, MODE_AGENT_LOGIN, ServiceTestResult, SetAgentService, TestAgentService,
};
use hmesh_core::sessions::*;
use hmesh_core::*;
use hmesh_runtime::acp_conn::{AcpConnection, AgentMessage};
use hmesh_runtime::service;
use hmesh_transport::{Client, ConnectionIdentity};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, HashMap},
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};
use tokio::sync::watch;
use uuid::Uuid;

mod commands;
mod projects;

/// Longest single long-poll wait; clients loop.
pub const MAX_WAIT_SECONDS: u64 = 10;
/// Agent processes of idle sessions are stopped after this long; the next
/// prompt starts them again and reloads the conversation when supported.
const IDLE_PROCESS_SECONDS: u64 = 600;
/// How long a requested stop may wait for the agent before its process is
/// ended: some adapters keep the ACP pipe open after their child agent died,
/// and a session must not stay busy forever.
const CANCEL_GRACE: std::time::Duration = std::time::Duration::from_secs(10);
/// A turn that runs longer than this is stopped even if the agent keeps
/// retrying: no session may stay busy forever. `HMESH_TURN_BUDGET_SECONDS`
/// overrides it (tests use a few seconds); the default is 30 minutes.
const DEFAULT_TURN_BUDGET_SECONDS: u64 = 1800;
fn turn_budget() -> std::time::Duration {
    let seconds = std::env::var("HMESH_TURN_BUDGET_SECONDS")
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
        .filter(|seconds| *seconds >= 5)
        .unwrap_or(DEFAULT_TURN_BUDGET_SECONDS);
    std::time::Duration::from_secs(seconds)
}
const MAX_PAGE_EVENTS: usize = 500;
const MAX_FINAL_TEXT: usize = 32 * 1024;

struct Live {
    conn: Arc<AcpConnection>,
    agent_session: Mutex<String>,
    commands: Mutex<Option<(String, Vec<SessionCommand>)>>,
    harness: HarnessKind,
    secrets: Vec<String>,
    permissions: Mutex<HashMap<String, (Value, Vec<Value>)>>,
    reply: Mutex<Reply>,
    replaying: std::sync::atomic::AtomicBool,
    /// Replayed updates are this session's history: it continues a session
    /// the agent already had and is being reloaded for the first time.
    keep_replay: std::sync::atomic::AtomicBool,
    /// That history as it is replayed, recorded at once when the reload
    /// succeeds: a reload that stops part way records none of it.
    replayed: Mutex<Vec<(String, Value)>>,
    /// Set just before the barrier that follows a reload: the pump ends the
    /// replay when it reaches that barrier, so every update before it counts
    /// as replayed however long the pump takes, and sets `replay_over`.
    replay_ends: std::sync::atomic::AtomicBool,
    replay_over: std::sync::atomic::AtomicBool,
    last_active: Mutex<Instant>,
    /// The model service settings the process was started with.
    service_digest: String,
    /// The agent's latest context and cost report (ACP `usage_update`),
    /// and whether one arrived since the last turn ended.
    usage: Mutex<Option<Value>>,
    usage_fresh: std::sync::atomic::AtomicBool,
    managed_models: Option<service::ManagedModels>,
    /// The startup block an adapter reports in `session/new`'s `_meta`; it is
    /// banner text about the adapter, not part of the conversation.
    startup_info: Mutex<Option<String>>,
    /// Tool calls the agent has started but not finished, by id and title.
    open_tools: Mutex<HashMap<String, String>>,
    /// Which turn is running, so a budget timer for an older turn is ignored.
    turn: std::sync::atomic::AtomicU64,
}
#[derive(Default)]
struct Reply {
    current: String,
    previous: String,
}
impl Reply {
    fn push(&mut self, text: &str) {
        self.current.push_str(text);
    }
    fn tool(&mut self) {
        if !self.current.trim().is_empty() {
            self.previous = std::mem::take(&mut self.current);
        }
        self.current.clear();
    }
    fn take(&mut self) -> Option<String> {
        let current = std::mem::take(&mut self.current);
        let previous = std::mem::take(&mut self.previous);
        let text = if current.trim().is_empty() {
            previous
        } else {
            current
        };
        (!text.trim().is_empty()).then(|| text.trim().to_owned())
    }
}

#[derive(Default)]
pub struct Sessions {
    live: Mutex<HashMap<Uuid, Arc<Live>>>,
    watchers: Mutex<HashMap<Uuid, watch::Sender<u64>>>,
    starting: Mutex<std::collections::HashSet<Uuid>>,
    /// Sessions whose settings are being changed; no turn starts meanwhile.
    configuring: Mutex<std::collections::HashSet<Uuid>>,
    /// Sessions whose owner asked to stop the current turn: if the agent has
    /// to be ended instead of answering, the turn still counts as cancelled.
    cancelling: Mutex<std::collections::HashSet<Uuid>>,
    /// Consecutive failed refreshes of this computer's copies of sessions on a
    /// peer. A peer that stays away stops the local copy from looking busy.
    remote_misses: Mutex<HashMap<Uuid, u32>>,
    /// Turns stopped by the time budget rather than by the owner.
    expired: Mutex<std::collections::HashSet<Uuid>>,
    refreshing: std::sync::atomic::AtomicBool,
    history: projects::HistoryCache,
    /// Sessions this computer's sessions opened on a paired computer:
    /// parent session -> (peer node, child session). Cancelling or closing the
    /// parent cancels them there.
    delegations: Mutex<HashMap<Uuid, Vec<(Uuid, Uuid)>>>,
}
/// Marks a session's setting change as finished when dropped.
struct Configuring<'a> {
    sessions: &'a Sessions,
    id: Uuid,
}
impl Drop for Configuring<'_> {
    fn drop(&mut self) {
        if let Ok(mut changing) = self.sessions.configuring.lock() {
            changing.remove(&self.id);
        }
    }
}
impl Sessions {
    fn live(&self, id: Uuid) -> Option<Arc<Live>> {
        self.live.lock().ok()?.get(&id).cloned()
    }
    fn watch(&self, id: Uuid) -> watch::Receiver<u64> {
        let mut watchers = self.watchers.lock().expect("session watchers");
        watchers
            .entry(id)
            .or_insert_with(|| watch::channel(0).0)
            .subscribe()
    }
    fn add_delegation(&self, parent: Uuid, peer: Uuid, child: Uuid) {
        if let Ok(mut links) = self.delegations.lock() {
            let entry = links.entry(parent).or_default();
            if !entry.contains(&(peer, child)) {
                entry.push((peer, child));
            }
        }
    }
    fn mark_cancelling(&self, id: Uuid) {
        if let Ok(mut set) = self.cancelling.lock() {
            set.insert(id);
        }
    }
    fn mark_expired(&self, id: Uuid) {
        if let Ok(mut set) = self.expired.lock() {
            set.insert(id);
        }
    }
    fn take_expired(&self, id: Uuid) -> bool {
        self.expired
            .lock()
            .ok()
            .is_some_and(|mut set| set.remove(&id))
    }
    fn note_remote_miss(&self, id: Uuid) -> u32 {
        self.remote_misses
            .lock()
            .map(|mut misses| {
                let count = misses.entry(id).or_insert(0);
                *count += 1;
                *count
            })
            .unwrap_or(0)
    }
    fn clear_remote_miss(&self, id: Uuid) {
        if let Ok(mut misses) = self.remote_misses.lock() {
            misses.remove(&id);
        }
    }
    fn take_cancelling(&self, id: Uuid) -> bool {
        self.cancelling
            .lock()
            .ok()
            .is_some_and(|mut set| set.remove(&id))
    }
    fn take_delegations(&self, parent: Uuid) -> Vec<(Uuid, Uuid)> {
        self.delegations
            .lock()
            .ok()
            .and_then(|mut links| links.remove(&parent))
            .unwrap_or_default()
    }
    fn notify(&self, id: Uuid, seq: u64) {
        if let Ok(watchers) = self.watchers.lock()
            && let Some(sender) = watchers.get(&id)
        {
            let _ = sender.send(seq);
        }
    }
}

use hmesh_runtime::{RedactedStream, redact_text as redact};
fn clipped(secrets: &[String], text: &str, limit: usize) -> Value {
    let (text, truncated) = clip(&redact(secrets, text), limit);
    if truncated {
        json!(format!("{text} [truncated]"))
    } else {
        json!(text)
    }
}

/// The banner an adapter prints before a turn (pi-acp reports it in the
/// `session/new` response `_meta`). It names the adapter version, skills,
/// prompts and extensions; it is not an answer and must not become the
/// conversation or the turn's final text.
fn is_startup_banner(text: &str, reported: Option<&str>) -> bool {
    if reported.is_some_and(|banner| banner.trim() == text.trim()) {
        return true;
    }
    // A banner split across frames keeps its opening and a section heading.
    let trimmed = text.trim_start();
    trimmed.starts_with("pi v")
        && (text.contains("\n## Skills")
            || text.contains("\n## Prompts")
            || text.contains("\n## Extensions"))
}

/// Provider retry chatter from pi's adapter: progress notices, not answers.
fn is_retry_notice(text: &str) -> bool {
    let text = text.trim();
    (text.starts_with("Retrying (attempt ") && text.ends_with(")..."))
        || text == "Retry finished, resuming."
        || (text.starts_with("New version available: ") && text.contains("npm i -g"))
}
fn err(code: ErrorCode, message: impl Into<String>) -> ApiError {
    ApiError(MeshError::new(code, message))
}

fn record_event(s: &AppState, id: Uuid, kind: &str, payload: Value) -> hmesh_core::Result<()> {
    let event = s.store.append_session_event(id, kind, &payload)?;
    s.sessions.notify(id, event.seq);
    Ok(())
}
fn save(s: &AppState, record: &mut SessionRecord) -> hmesh_core::Result<()> {
    record.updated_at = now();
    s.store.update_session(record)?;
    s.sessions.notify(record.session_id, u64::MAX);
    Ok(())
}
/// Read, change and store a session record under the turn lock, so changes to
/// its settings made at the same time do not overwrite each other.
fn update_record(
    s: &AppState,
    id: Uuid,
    change: impl FnOnce(&mut SessionRecord),
) -> hmesh_core::Result<SessionRecord> {
    let _guard = s
        .sessions
        .starting
        .lock()
        .map_err(|_| MeshError::new(ErrorCode::Internal, "session lock poisoned"))?;
    let mut record = s.store.session(id)?;
    change(&mut record);
    save(s, &mut record)?;
    Ok(record)
}
fn set_status(s: &AppState, id: Uuid, status: SessionStatus) -> hmesh_core::Result<SessionRecord> {
    let mut record = s.store.session(id)?;
    if record.status != SessionStatus::Closed {
        record.status = status;
        save(s, &mut record)?;
    }
    Ok(record)
}

/// Values to keep out of everything recorded for an agent's sessions: its
/// configured references and its owner's model service secrets.
fn agent_secrets(s: &AppState, agent: Uuid) -> Vec<String> {
    let Ok((_, config)) = s.store.agent(agent) else {
        return vec![];
    };
    let mut secrets = hmesh_runtime::run_secrets(&config);
    if let Ok(Some(service)) = service::load(&s.instance.dir, agent) {
        secrets.extend(service.secrets());
    }
    secrets
}

/// The `hmesh mcp` server every session receives, bound to that session.
fn mcp_server(s: &AppState, session: Uuid) -> Vec<Value> {
    let command = std::env::var_os("HMESH_MCP_COMMAND")
        .map(std::path::PathBuf::from)
        .or_else(|| std::env::current_exe().ok());
    let Some(command) = command else {
        return vec![];
    };
    let args = vec![
        "--data-dir".to_owned(),
        s.instance.dir.to_string_lossy().into_owned(),
        "mcp".to_owned(),
        "--session".to_owned(),
        session.to_string(),
    ];
    vec![acp::stdio_mcp_server("harnessmesh", &command, &args, &[])]
}

/// A count the agent reported, when it is a plain non-negative integer.
fn count(value: Option<&Value>) -> Option<u64> {
    value.and_then(Value::as_u64)
}
/// The context window and cumulative cost from an ACP `usage_update`
/// (`used` and `size` in tokens, optional `cost` with an amount and unit).
fn context_usage(update: &Value) -> Option<Value> {
    let used = count(update.get("used"))?;
    let size = count(update.get("size"))?;
    let cost = update.get("cost").and_then(|cost| {
        let amount = cost
            .get("amount")
            .and_then(Value::as_f64)
            .filter(|a| a.is_finite() && *a >= 0.0)?;
        let currency = cost
            .get("currency")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|c| {
                !c.is_empty() && c.len() <= 16 && c.chars().all(|ch| ch.is_ascii_alphanumeric())
            })?;
        Some(json!({"amount":amount,"currency":currency}))
    });
    Some(json!({"context":{"used":used,"size":size},"cost":cost}))
}
/// The context and cost a session last recorded, if any.
fn earlier_usage(s: &AppState, session: Uuid) -> Option<Value> {
    let last = s
        .store
        .latest_session_event(session, "usage")
        .ok()
        .flatten()?;
    let (context, cost) = (last.get("context")?, last.get("cost")?);
    (!context.is_null() || !cost.is_null()).then(|| json!({"context":context,"cost":cost}))
}
/// What a finished turn records about model use: the latest context and
/// cost report, and the token counts the prompt response carried (ACP
/// `usage`, still unstable). Only what the agent reported; nothing is added
/// up. A turn that reported nothing new records nothing.
fn turn_usage(latest: Option<&Value>, fresh: bool, reported: Option<&Value>) -> Option<Value> {
    let tokens = reported.and_then(|u| {
        let field = |name: &str| count(u.get(name));
        let (total, input, output) = (
            field("totalTokens"),
            field("inputTokens"),
            field("outputTokens"),
        );
        (total.is_some() || input.is_some() || output.is_some()).then(|| {
            json!({
                "total_tokens":total,
                "input_tokens":input,
                "output_tokens":output,
                "thought_tokens":field("thoughtTokens"),
                "cached_read_tokens":field("cachedReadTokens"),
                "cached_write_tokens":field("cachedWriteTokens"),
            })
        })
    });
    if !fresh && tokens.is_none() {
        return None;
    }
    let part = |name: &str| {
        latest
            .and_then(|l| l.get(name))
            .cloned()
            .unwrap_or(Value::Null)
    };
    Some(json!({"context":part("context"),"cost":part("cost"),"turn":tokens}))
}

/// Normalize one `session/update` into a bounded recorded event.
fn normalize(
    update: &Value,
    secrets: &[String],
    inputs: &mut crate::tool_events::ToolInputs,
) -> Option<(&'static str, Value)> {
    match update.get("sessionUpdate").and_then(Value::as_str)? {
        kind @ ("tool_call" | "tool_call_update") => Some((
            if kind == "tool_call" {
                "tool_call"
            } else {
                "tool_call_update"
            },
            inputs.payload(update, secrets),
        )),
        "plan" => {
            let entries: Vec<Value> = update
                .get("entries")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .take(64)
                .map(|e| json!({"content":clipped(secrets, e.get("content").and_then(Value::as_str).unwrap_or(""), 512),"status":e.get("status"),"priority":e.get("priority")}))
                .collect();
            Some(("plan", json!({"entries":entries})))
        }
        _ => None,
    }
}

/// Store all safe text, splitting on UTF-8 boundaries for bounded events.
fn record_text(write: &impl Fn(&str, Value), kind: &str, mut text: &str) {
    while !text.is_empty() {
        let mut end = text.len().min(MAX_EVENT_TEXT_BYTES);
        while !text.is_char_boundary(end) {
            end -= 1;
        }
        write(kind, json!({"text": &text[..end]}));
        text = &text[end..];
    }
}

/// Text waiting to be recorded. Adapters stream by token; one event per token
/// bloats the store and makes the panel re-walk thousands of entries on every
/// refresh, so adjacent chunks of one kind are merged and flushed shortly after
/// they arrive or at a turn boundary.
#[derive(Default)]
struct PendingText {
    entries: Vec<(&'static str, String)>,
    bytes: usize,
}
impl PendingText {
    fn push(&mut self, kind: &'static str, text: &str) {
        if text.is_empty() {
            return;
        }
        if let Some((last, buffer)) = self.entries.last_mut()
            && *last == kind
        {
            buffer.push_str(text);
        } else {
            self.entries.push((kind, text.to_owned()));
        }
        self.bytes += text.len();
    }
    fn take(&mut self) -> Vec<(&'static str, String)> {
        self.bytes = 0;
        std::mem::take(&mut self.entries)
    }
}
const TEXT_FLUSH_BYTES: usize = 512;
const TEXT_FLUSH_DELAY: std::time::Duration = std::time::Duration::from_millis(150);
/// What is recorded about one tool call so far.
#[derive(Clone, Default)]
struct ToolTrace {
    status: String,
    title: String,
    /// Bytes of the recorded output.
    output: usize,
    /// Hash of the recorded output: past its limit output keeps one length
    /// while its tail moves on.
    output_hash: u64,
    /// Hash and size of each recorded detail, by `DETAIL_KEYS`; 0 when none.
    details: [(u64, usize); DETAIL_KEYS.len()],
}
impl ToolTrace {
    fn record(&mut self, payload: &Value) {
        for (i, detail) in details_of(payload) {
            self.details[i] = detail;
        }
    }
}
/// What a tool call's update says beyond its status, title and output.
const DETAIL_KEYS: [&str; 6] = [
    "args",
    "input",
    "diffs",
    "diffs_omitted",
    "locations",
    "terminal",
];
fn text_hash(text: &str) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    text.hash(&mut hasher);
    hasher.finish().max(1)
}
/// The details a payload carries, by index into `DETAIL_KEYS`, as a hash of
/// their text (never 0) and its size.
fn details_of(payload: &Value) -> impl Iterator<Item = (usize, (u64, usize))> + '_ {
    DETAIL_KEYS.iter().enumerate().filter_map(|(i, key)| {
        let value = payload.get(*key).filter(|v| !v.is_null())?;
        let text = value.to_string();
        Some((i, (text_hash(&text), text.len())))
    })
}
/// What one page of events may carry, well within the 1 MiB a reply may be.
const MAX_PAGE_BYTES: usize = 512 * 1024;
/// Large events fill a page sooner; the caller asks again from `next`. A
/// page always holds at least one event.
fn fit_page(events: &mut Vec<SessionEvent>) {
    let mut used = 0;
    let fit = events
        .iter()
        .take_while(|e| {
            used += e.payload.to_string().len() + 256;
            used <= MAX_PAGE_BYTES
        })
        .count();
    events.truncate(fit.max(1));
}
/// Streamed command output is recorded at most about once a second.
const TERMINAL_FLUSH_DELAY: std::time::Duration = std::time::Duration::from_secs(1);

fn flush_text(write: &impl Fn(&str, Value), pending: &mut PendingText) {
    for (kind, text) in pending.take() {
        record_text(write, kind, &text);
    }
}

async fn pump(
    s: AppState,
    id: Uuid,
    live: Arc<Live>,
    mut messages: tokio::sync::mpsc::UnboundedReceiver<AgentMessage>,
) {
    // Thoughts and replies can be interleaved. Keep their partial credentials
    // independent, even when tool events occur before the next text delta.
    let mut message_text = RedactedStream::default();
    let mut thought_text = RedactedStream::default();
    // Last recorded status and title length per tool call: adapters stream the
    // title character by character, and each repeat need not become an event.
    let mut tool_trace: HashMap<String, ToolTrace> = HashMap::new();
    // The latest details of updates not recorded on their own; they ride
    // along with the call's next recorded update, or are written when the
    // turn ends.
    let mut unrecorded: HashMap<String, serde_json::Map<String, Value>> = HashMap::new();
    // Command output streamed through `_meta`, recorded in parts.
    let mut terminals = crate::tool_events::TerminalLogs::default();
    let mut tool_inputs = crate::tool_events::ToolInputs::default();
    let mut terminal_at: Option<tokio::time::Instant> = None;
    // A first reload's history is kept until the reload succeeds.
    let write = |kind: &str, payload: Value| {
        if live.keep_replay.load(std::sync::atomic::Ordering::SeqCst) {
            if let Ok(mut kept) = live.replayed.lock() {
                kept.push((kind.to_owned(), payload));
            }
        } else {
            let _ = record_event(&s, id, kind, payload);
        }
    };
    // Text merged for a moment before it becomes events; `flush_at` is when
    // that happens without further chunks arriving.
    let mut pending_text = PendingText::default();
    let mut flush_at: Option<tokio::time::Instant> = None;
    // A replayed user message, recorded once the agent moves on.
    let mut replayed_user = String::new();
    let flush_user = |text: &mut String| {
        let text = std::mem::take(text);
        if !text.trim().is_empty() {
            write(
                "user_message",
                json!({"text":clipped(&live.secrets, text.trim(), MAX_EVENT_TEXT_BYTES),"from":{"replayed":true}}),
            );
        }
    };
    // Writes the held details of one call, or of every call.
    let write_unrecorded = |held: &mut HashMap<String, serde_json::Map<String, Value>>,
                            traces: &mut HashMap<String, ToolTrace>,
                            only: Option<&str>| {
        let mut calls: Vec<_> = match only {
            Some(id) => held.remove_entry(id).into_iter().collect(),
            None => held.drain().collect(),
        };
        calls.sort_by(|a, b| a.0.cmp(&b.0));
        for (id, mut fields) in calls {
            fields.insert("id".into(), json!(id));
            let payload = Value::Object(fields);
            if let Some(trace) = traces.get_mut(&id) {
                trace.record(&payload);
                if let Some(output) = payload["output"].as_str() {
                    trace.output = output.len();
                    trace.output_hash = text_hash(output);
                }
            }
            write("tool_call_update", payload);
        }
    };
    let write_terminals = |terminals: &mut crate::tool_events::TerminalLogs,
                           when: crate::tool_events::Drain| {
        for payload in terminals.drain(when) {
            write("tool_call_update", payload);
        }
    };
    loop {
        let until = [flush_at, terminal_at]
            .into_iter()
            .flatten()
            .min()
            .unwrap_or_else(|| tokio::time::Instant::now() + std::time::Duration::from_secs(3600));
        tokio::select! {
            maybe = messages.recv() => {
                let Some(message) = maybe else { break };
                match message {
            AgentMessage::Update(params) => {
                if params["update"]["sessionUpdate"] == "available_commands_update" {
                    commands::advertised(&s, id, &live, &params);
                    continue;
                }
                if live.replaying.load(std::sync::atomic::Ordering::SeqCst) {
                    continue;
                }
                let update = &params["update"];
                if live.keep_replay.load(std::sync::atomic::Ordering::SeqCst) {
                    if update["sessionUpdate"] == "user_message_chunk" {
                        pending_text.push("agent_message_chunk", &message_text.finish());
                        pending_text.push("agent_thought_chunk", &thought_text.finish());
                        flush_text(&write, &mut pending_text);
                        flush_at = None;
                        if let Some(text) = update.pointer("/content/text").and_then(Value::as_str)
                        {
                            replayed_user.push_str(text);
                        }
                        continue;
                    }
                    flush_user(&mut replayed_user);
                }
                // Context and cost reports can come many times a turn; the
                // latest is recorded once, when the turn ends.
                if update.get("sessionUpdate").and_then(Value::as_str) == Some("usage_update") {
                    if let Some(reported) = context_usage(update)
                        && let Ok(mut usage) = live.usage.lock()
                    {
                        *usage = Some(reported);
                        live.usage_fresh
                            .store(true, std::sync::atomic::Ordering::SeqCst);
                    }
                    continue;
                }
                // The agent changed a setting itself (or reports new ones).
                if update.get("sessionUpdate").and_then(Value::as_str)
                    == Some("config_option_update")
                {
                    let options = live.options(&update["configOptions"]);
                    if update["configOptions"].is_array() {
                        let _ = update_record(&s, id, |record| record.config_options = options);
                        // Model choices also supply the /model(s) selector.
                        let _ = record_event(&s, id, "commands_updated", json!({}));
                    }
                    continue;
                }
                if let Some(text) = acp::message_text(update) {
                    // An adapter banner and provider retries are not answers.
                    let banner = live.startup_info.lock().ok().and_then(|b| b.clone());
                    if live.harness == HarnessKind::Pi && is_startup_banner(text, banner.as_deref())
                    {
                        continue;
                    }
                    if live.harness == HarnessKind::Pi && is_retry_notice(text) {
                        write("status", json!({"message": redact(&live.secrets, text)}));
                        continue;
                    }
                    if let Ok(mut reply) = live.reply.lock() {
                        reply.push(text);
                    }
                }
                let kind = update["sessionUpdate"].as_str();
                let stream = match kind {
                    Some("agent_message_chunk") => Some(("agent_message_chunk", &mut message_text)),
                    Some("agent_thought_chunk") => Some(("agent_thought_chunk", &mut thought_text)),
                    _ => None,
                };
                if let Some((kind, stream)) = stream {
                    if let Some(text) = update.pointer("/content/text").and_then(Value::as_str) {
                        pending_text.push(kind, &stream.push(&live.secrets, text));
                        if pending_text.bytes >= TEXT_FLUSH_BYTES {
                            flush_text(&write, &mut pending_text);
                            flush_at = None;
                        } else {
                            flush_at
                                .get_or_insert_with(|| tokio::time::Instant::now() + TEXT_FLUSH_DELAY);
                        }
                    }
                    continue;
                }
                if matches!(
                    update.get("sessionUpdate").and_then(Value::as_str),
                    Some("tool_call")
                ) && let Ok(mut reply) = live.reply.lock()
                {
                    reply.tool();
                }
                // Text before a tool card stays before it.
                flush_text(&write, &mut pending_text);
                flush_at = None;
                if matches!(kind, Some("tool_call" | "tool_call_update")) {
                    terminals.observe(update, &live.secrets);
                }
                if let Some((kind, mut payload)) = normalize(update, &live.secrets, &mut tool_inputs) {
                    if kind == "tool_call_update" {
                        let call = payload["id"].as_str().unwrap_or_default().to_owned();
                        let status = payload["status"].as_str().unwrap_or("").to_owned();
                        let title = payload["title"].as_str().unwrap_or("").to_owned();
                        let terminal =
                            matches!(status.as_str(), "completed" | "failed" | "cancelled");
                        if let Ok(mut open) = live.open_tools.lock() {
                            if terminal {
                                open.remove(&call);
                            } else if !call.is_empty() {
                                open.insert(call.clone(), title.clone());
                            }
                        }
                        let output = payload["output"].as_str().map_or(0, str::len);
                        let previous = tool_trace.get(&call).cloned();
                        // Status changes, the final title, and output or details
                        // that first appear or change by a kilobyte are kept;
                        // character-by-character growth of a title, of streamed
                        // arguments and of streamed output is not.
                        let changed = previous.as_ref().is_none_or(|was| {
                            was.status != status
                                || terminal
                                || title.chars().count() >= was.title.chars().count() + 8
                                || (output > 0 && (was.output == 0 || output.abs_diff(was.output) >= 1024))
                                || details_of(&payload).any(|(i, (hash, size))| {
                                    let (seen, seen_size) = was.details[i];
                                    seen == 0 || (hash != seen && size.abs_diff(seen_size) >= 1024)
                                })
                        });
                        if changed {
                            if let Some(held) = unrecorded.remove(&call) {
                                for (key, value) in held {
                                    if payload.get(&key).is_none_or(Value::is_null) {
                                        payload[&key] = value;
                                    }
                                }
                            }
                        } else if let Some(was) = &previous {
                            // Only what differs from the record is held; an agent
                            // that repeats the same arguments adds nothing.
                            let held = unrecorded.entry(call.clone()).or_default();
                            for (i, (hash, _)) in details_of(&payload) {
                                let key = DETAIL_KEYS[i];
                                if hash == was.details[i].0 {
                                    held.remove(key);
                                } else {
                                    held.insert(key.to_owned(), payload[key].clone());
                                }
                            }
                            // Output that grew too little to record now (an agent
                            // sending the whole output again each time) still
                            // shows within about a second.
                            if output > 0
                                && payload["output"].as_str().map(text_hash) != Some(was.output_hash)
                            {
                                held.insert("output".into(), payload["output"].clone());
                            }
                            if held.is_empty() {
                                unrecorded.remove(&call);
                            }
                        }
                        let trace = match (changed, previous) {
                            (false, Some(was)) => was,
                            (_, previous) => {
                                let mut trace = previous.unwrap_or_default();
                                trace.status = status.clone();
                                if !title.is_empty() {
                                    trace.title = title.clone();
                                }
                                trace.output = output;
                                if let Some(text) = payload["output"].as_str() {
                                    trace.output_hash = text_hash(text);
                                }
                                trace.record(&payload);
                                trace
                            }
                        };
                        tool_trace.insert(call, trace);
                        if !changed {
                            write_terminals(&mut terminals, crate::tool_events::Drain::Ready);
                            // Held details are written within about a second.
                            if terminals.has_pending() || !unrecorded.is_empty() {
                                terminal_at.get_or_insert_with(|| tokio::time::Instant::now() + TERMINAL_FLUSH_DELAY);
                            }
                            continue;
                        }
                    } else if kind == "tool_call" {
                        let call = payload["id"].as_str().unwrap_or_default().to_owned();
                        let title = payload["title"].as_str().unwrap_or("").to_owned();
                        if !call.is_empty()
                            && let Ok(mut open) = live.open_tools.lock()
                        {
                            open.insert(call.clone(), title.clone());
                        }
                        let mut trace = ToolTrace {
                            status: "pending".into(),
                            title: title.clone(),
                            output: payload["output"].as_str().map_or(0, str::len),
                            output_hash: payload["output"].as_str().map_or(0, text_hash),
                            ..ToolTrace::default()
                        };
                        trace.record(&payload);
                        tool_trace.insert(call, trace);
                    }
                    write(kind, payload);
                }
                write_terminals(&mut terminals, crate::tool_events::Drain::Ready);
                if terminals.has_pending() {
                    terminal_at.get_or_insert_with(|| tokio::time::Instant::now() + TERMINAL_FLUSH_DELAY);
                }
            }
            AgentMessage::Permission { id: rpc, params } => {
                // Text before a permission card stays before it.
                flush_text(&write, &mut pending_text);
                flush_at = None;
                // Unique per session even when the agent process restarts.
                let key = Uuid::new_v4().simple().to_string()[..12].to_owned();
                let call = &params["toolCall"];
                let options: Vec<Value> = params["options"].as_array().cloned().unwrap_or_default();
                // An id too long to show is still answered through `allow`.
                let shown: Vec<Value> = options
                    .iter()
                    .take(16)
                    .map(|o| {
                        let option_id = o
                            .get("optionId")
                            .and_then(Value::as_str)
                            .filter(|i| i.len() <= 256);
                        let name = o
                            .get("name")
                            .and_then(Value::as_str)
                            .map(|n| clipped(&live.secrets, n, 256));
                        json!({"option_id":option_id,"name":name,"kind":o.get("kind")})
                    })
                    .collect();
                // A call the stream never showed (some agents only ask) gets
                // its row from what the request says about it.
                let call_id = call
                    .get("toolCallId")
                    .and_then(Value::as_str)
                    .filter(|i| i.len() <= 256);
                // What is asked about must be what the call says now.
                if let Some(call_id) = call_id {
                    write_unrecorded(&mut unrecorded, &mut tool_trace, Some(call_id));
                    // The request may describe the call more recently than the stream.
                    if let Some(trace) = tool_trace.get_mut(call_id) {
                        let asked = crate::tool_events::tool_payload(call, &live.secrets, None);
                        let mut newer = serde_json::Map::new();
                        for (i, (hash, _)) in details_of(&asked) {
                            if hash != trace.details[i].0 {
                                newer.insert(DETAIL_KEYS[i].into(), asked[DETAIL_KEYS[i]].clone());
                            }
                        }
                        if let Some(title) = asked["title"].as_str().filter(|t| !t.is_empty() && *t != trace.title) {
                            newer.insert("title".into(), json!(title));
                            trace.title = title.to_owned();
                        }
                        if !newer.is_empty() {
                            let mut payload = Value::Object(newer);
                            trace.record(&payload);
                            payload["id"] = json!(call_id);
                            write("tool_call_update", payload);
                        }
                    }
                }
                if let Some(call_id) = call_id
                    && !tool_trace.contains_key(call_id)
                    && ["title", "content", "rawInput", "locations"]
                        .iter()
                        .any(|k| call.get(*k).is_some())
                {
                    write(
                        "tool_call_update",
                        crate::tool_events::tool_payload(call, &live.secrets, None),
                    );
                }
                let request = json!({
                    "key":key,
                    "title":call.get("title").and_then(Value::as_str).map(|t| clipped(&live.secrets, t, 512)),
                    "kind":call.get("kind"),
                    // Some agents send only the id; the panel finds the call by it.
                    "tool_call_id":call_id,
                    "options":shown,
                });
                let approver = s
                    .store
                    .session(id)
                    .map(|r| r.approver)
                    .unwrap_or(Approver::Local);
                let _ = record_event(&s, id, "permission_request", request);
                if approver == Approver::Auto {
                    let _ = live
                        .conn
                        .respond(&rpc, acp::permission_outcome(&options, true));
                    let _ = record_event(
                        &s,
                        id,
                        "permission_resolved",
                        json!({"key":key,"allow":true,"by":"auto"}),
                    );
                } else {
                    if let Ok(mut pending) = live.permissions.lock() {
                        pending.insert(key, (rpc, options));
                    }
                    let _ = set_status(&s, id, SessionStatus::WaitingPermission);
                }
            }
            AgentMessage::Barrier { end_turn, done } => {
                flush_user(&mut replayed_user);
                if end_turn || live.keep_replay.load(std::sync::atomic::Ordering::SeqCst) {
                    pending_text.push("agent_message_chunk", &message_text.finish());
                    pending_text.push("agent_thought_chunk", &thought_text.finish());
                }
                flush_text(&write, &mut pending_text);
                flush_at = None;
                if end_turn {
                    write_terminals(&mut terminals, crate::tool_events::Drain::End);
                    terminal_at = None;
                    write_unrecorded(&mut unrecorded, &mut tool_trace, None);
                }
                // The barrier after a reload: what came before it was the
                // replay, what comes after it is new.
                if live
                    .replay_ends
                    .swap(false, std::sync::atomic::Ordering::SeqCst)
                {
                    live.replaying
                        .store(false, std::sync::atomic::Ordering::SeqCst);
                    live.keep_replay
                        .store(false, std::sync::atomic::Ordering::SeqCst);
                    live.replay_over
                        .store(true, std::sync::atomic::Ordering::SeqCst);
                }
                let _ = done.send(());
            }
            AgentMessage::Exited => break,
                }
            }
            _ = tokio::time::sleep_until(until) => {
                let now = tokio::time::Instant::now();
                if flush_at.is_some_and(|at| at <= now) {
                    flush_text(&write, &mut pending_text);
                    flush_at = None;
                }
                if terminal_at.is_some_and(|at| at <= now) {
                    flush_text(&write, &mut pending_text);
                    flush_at = None;
                    write_terminals(&mut terminals, crate::tool_events::Drain::Timer);
                    write_unrecorded(&mut unrecorded, &mut tool_trace, None);
                    terminal_at = None;
                }
            }
        }
    }
    flush_user(&mut replayed_user);
    pending_text.push("agent_message_chunk", &message_text.finish());
    pending_text.push("agent_thought_chunk", &thought_text.finish());
    flush_text(&write, &mut pending_text);
    write_terminals(&mut terminals, crate::tool_events::Drain::End);
    write_unrecorded(&mut unrecorded, &mut tool_trace, None);
    // The process is gone; a later prompt starts a new one.
    if let Ok(mut live_map) = s.sessions.live.lock()
        && live_map.get(&id).is_some_and(|l| Arc::ptr_eq(l, &live))
    {
        live_map.remove(&id);
        drop(live_map);
        let _ = record_event(&s, id, "commands_updated", json!({}));
    }
}

/// Stop a session's agent process and forget it, if it is still the
/// registered one.
async fn discard(s: &AppState, id: Uuid, live: &Arc<Live>) {
    if let Ok(mut map) = s.sessions.live.lock()
        && map.get(&id).is_some_and(|l| Arc::ptr_eq(l, live))
    {
        map.remove(&id);
    }
    hmesh_runtime::mcp_bridge::remove(&s.instance.dir, id);
    live.conn.shutdown().await;
}

impl Live {
    fn options(&self, reported: &Value) -> Vec<ConfigOption> {
        let mut options = ConfigOption::parse_all(reported);
        if let Some(models) = &self.managed_models {
            models.filter(&mut options);
        }
        options
    }
}

/// Start (or reuse) the agent process for a session and open or reload the
/// agent-side session, then apply the session's settings.
async fn ensure_live(s: &AppState, record: &SessionRecord) -> hmesh_core::Result<Arc<Live>> {
    check_place(s, record)?;
    let (card, config) = s.store.agent(record.agent_id)?;
    let service = service::load(&s.instance.dir, record.agent_id)
        .map_err(|e| MeshError::new(ErrorCode::Internal, e.to_string()))?;
    let digest = service.as_ref().map(|v| v.digest()).unwrap_or_default();
    if let Some(live) = s.sessions.live(record.session_id)
        && live.conn.alive()
    {
        if live.service_digest == digest
            && live.managed_models.as_ref().is_none_or(|models| {
                record
                    .config_options
                    .iter()
                    .find(|o| o.is_category("model"))
                    .and_then(|o| o.current.as_str())
                    .is_some_and(|v| models.allows(v))
            })
        {
            return Ok(live);
        }
        // The owner changed how this agent reaches its model service: start
        // it again so it sees the new settings (the conversation reloads).
        discard(s, record.session_id, &live).await;
    }
    let previous_model = record
        .config_options
        .iter()
        .find(|o| o.is_category("model"))
        .and_then(|o| o.current.as_str());
    let relay = |endpoint: &str, key: &str, protocol| s.relay.route(endpoint, key, protocol);
    // A session in another project runs the same agent in that folder.
    let cwd = record
        .cwd
        .clone()
        .unwrap_or_else(|| config.project.root.clone());
    let mut launch = config.clone();
    launch.project.root = cwd.clone();
    let mcp = mcp_server(s, record.session_id);
    let (mut command, secrets) = hmesh_runtime::agent_launch(
        &launch,
        &s.instance.dir,
        record.agent_id,
        service.as_ref(),
        previous_model,
        Some(&relay),
    )
    .map_err(|e| MeshError::new(ErrorCode::AgentUnavailable, e.to_string()))?;
    // The owner's explicit default is reported when the agent refuses it; the
    // older informational `adapter.model` applies only when the agent offers it.
    let default_model = match service.as_ref().and_then(|v| v.model_setting(card.harness)) {
        Some(model) => Some((model, true)),
        None => config.adapter.model.clone().map(|model| (model, false)),
    };
    // pi-acp accepts the ACP MCP servers but never forwards them to pi. When
    // the agent uses its own Pi configuration and the MCP adapter is installed,
    // hand the session's servers to pi through the adapter's --mcp-config flag.
    // A custom model service runs an isolated profile without the adapter.
    if card.harness == HarnessKind::Pi
        && !mcp.is_empty()
        && service.as_ref().is_none_or(|v| v.mode == MODE_AGENT_LOGIN)
        && std::env::var_os("PI_ACP_PI_COMMAND").is_none()
        && hmesh_runtime::mcp_bridge::supported("pi").await
        && let Ok(bridge) =
            hmesh_runtime::mcp_bridge::install(&s.instance.dir, record.session_id, &mcp, "pi")
    {
        command.env("PI_ACP_PI_COMMAND", &bridge.command);
        command.env("PI_ACP_MCP_CONFIG", &bridge.config);
    }
    let (conn, messages) = AcpConnection::start(command).await?;
    if record.history_pending && acp::SessionRestore::advertised(&conn.initialize).is_none() {
        conn.shutdown().await;
        return Err(MeshError::new(
            ErrorCode::AdapterNotSupported,
            "this agent cannot continue its earlier sessions",
        ));
    }
    let restore = record
        .agent_session_id
        .as_ref()
        .zip(acp::SessionRestore::advertised(&conn.initialize));
    // An earlier start may already have recorded the replayed history (the
    // daemon stopped, or the reload failed, before it was noted as kept).
    // Then this replay, answers included, is not recorded a second time.
    let history_kept = record.history_pending && replay_recorded(s, record.session_id);
    let keep_history = record.history_pending && !history_kept;
    // Cached options from an older process are useful when choosing an agent,
    // but must not create a command alias once a new process is starting.
    // DSH checks the old model when resuming, before it accepts a new one.
    // Keep that metadata across failed/interrupted restores. Command snapshots
    // ignore options until this process has published its session ID.
    if card.harness != HarnessKind::Dsh && !record.config_options.is_empty() {
        update_record(s, record.session_id, |r| r.config_options.clear())?;
    }
    let live = Arc::new(Live {
        conn: conn.clone(),
        agent_session: Mutex::new(String::new()),
        commands: Mutex::new(None),
        harness: card.harness,
        secrets,
        permissions: Mutex::new(HashMap::new()),
        reply: Mutex::new(Reply::default()),
        // Updates the agent replays while reloading are not new events.
        replaying: std::sync::atomic::AtomicBool::new(
            matches!(restore, Some((_, acp::SessionRestore::Load))) && !keep_history,
        ),
        keep_replay: std::sync::atomic::AtomicBool::new(
            matches!(restore, Some((_, acp::SessionRestore::Load))) && keep_history,
        ),
        replayed: Mutex::new(Vec::new()),
        replay_ends: std::sync::atomic::AtomicBool::new(false),
        replay_over: std::sync::atomic::AtomicBool::new(false),
        last_active: Mutex::new(Instant::now()),
        service_digest: digest,
        // A restarted agent may report only tokens at first: its last known
        // context and cost carry over until it reports new ones.
        usage: Mutex::new(earlier_usage(s, record.session_id)),
        usage_fresh: std::sync::atomic::AtomicBool::new(false),
        managed_models: service
            .as_ref()
            .and_then(|svc| service::ManagedModels::from_service(svc, card.harness)),
        startup_info: Mutex::new(None),
        open_tools: Mutex::new(HashMap::new()),
        turn: std::sync::atomic::AtomicU64::new(0),
    });
    s.sessions
        .live
        .lock()
        .map_err(|_| MeshError::new(ErrorCode::Internal, "session map poisoned"))?
        .insert(record.session_id, live.clone());
    tokio::spawn(pump(s.clone(), record.session_id, live.clone(), messages));
    // Registered first, checked second: a close that ran while the agent was
    // starting either sees this process in the map or is seen here.
    if !s.store.session(record.session_id)?.status.is_open() {
        discard(s, record.session_id, &live).await;
        return Err(MeshError::new(
            ErrorCode::TaskConflict,
            "the session was closed while its agent was starting",
        ));
    }
    const RELOADED: &str = "the agent reloaded this conversation";
    const CONTINUED: &str = "continued a session the agent already had";
    const CONTINUED_QUIETLY: &str =
        "continued a session the agent already had; this agent does not show its earlier messages";
    const NO_RELOAD: &str =
        "this agent cannot reload conversations; it continues without the earlier context";
    let started = match restore {
        Some((previous, method)) => {
            let restored = conn.restore_session(method, previous, &cwd, &mcp).await;
            // Drain any replay and initial command/config updates before the
            // next turn. The pump ends the replay at this barrier.
            live.replay_ends
                .store(true, std::sync::atomic::Ordering::SeqCst);
            conn.barrier_within(REPLAY_WAIT).await;
            // Not reached in time: start again later rather than record a
            // replay as new output or continue without it.
            let restored = if live.replay_over.load(std::sync::atomic::Ordering::SeqCst) {
                restored
            } else {
                restored.and_then(|_| {
                    Err(MeshError::new(
                        ErrorCode::AgentUnavailable,
                        "the agent's earlier messages took too long to load; send again to retry",
                    ))
                })
            };
            if keep_history
                && matches!(method, acp::SessionRestore::Load)
                && let Ok(mut reply) = live.reply.lock()
            {
                // The replayed answers are history, not the next turn's reply.
                reply.take();
            }
            let note = match (keep_history, method) {
                (false, _) => RELOADED,
                (true, acp::SessionRestore::Load) => CONTINUED,
                (true, acp::SessionRestore::Resume) => CONTINUED_QUIETLY,
            };
            restored.map(|result| (previous.clone(), result, false, Some(note)))
        }
        None => {
            let note = record.agent_session_id.as_ref().map(|_| NO_RELOAD);
            conn.new_session(&cwd, &mcp).await.map(|(id, result)| {
                // Some adapters report their startup banner here and send
                // it as an update right after; keep it out of the turn.
                if let Some(banner) = result
                    .pointer("/_meta/piAcp/startupInfo")
                    .and_then(Value::as_str)
                    && let Ok(mut slot) = live.startup_info.lock()
                {
                    *slot = Some(banner.to_owned());
                }
                (id, result, true, note)
            })
        }
    };
    let (agent_session, result, fresh, note) = match started {
        Ok(started) => {
            // Record the replayed history, then note it as kept so a later
            // start does not record it again, even if applying settings fails
            // below.
            if record.history_pending
                && !started.2
                && let Err(error) = still_open(s, record.session_id)
                    .and_then(|()| record_history(s, record.session_id, &live))
                    .and_then(|()| {
                        update_record(s, record.session_id, |r| r.history_pending = false)
                    })
            {
                discard(s, record.session_id, &live).await;
                return Err(error);
            }
            started
        }
        Err(mut error) => {
            // Native DSH validates the persisted route during session/resume,
            // before ACP permits a model change. Its native profile may no
            // longer contain the managed gateway. Retain the upstream cause:
            // another recoverable fault could also have prevented resuming.
            if restore.is_some()
                && card.harness == HarnessKind::Dsh
                && service.as_ref().is_none_or(|s| s.mode == MODE_AGENT_LOGIN)
                && previous_model
                    .and_then(service::dsh::gateway_model)
                    .is_some()
            {
                error.message.push_str(". This DSH session used a HarnessMesh gateway model. To continue it, restore its custom model service and retry. To use the native DSH profile, open a new session.");
            }
            discard(s, record.session_id, &live).await;
            return Err(error);
        }
    };
    let reported = live.options(&result["configOptions"]);
    if fresh && !reported.is_empty() {
        // The agent's own defaults, offered before a session starts.
        let defaults = reported.clone();
        let _ = s
            .store
            .update_agent_card(record.agent_id, |card| card.config_options = defaults);
    }
    let (options, rejected) = apply_settings(
        s,
        &live,
        &agent_session,
        record.session_id,
        reported,
        default_model,
    )
    .await;
    // Native project settings or restored agent state can select another
    // provider. The agent must confirm an owner-managed route before a turn.
    if let Some(models) = &live.managed_models
        && !options
            .iter()
            .find(|o| o.is_category("model"))
            .is_some_and(|o| {
                o.current.as_str() == Some(models.selected(record.config_values.get(&o.id)))
            })
    {
        discard(s, record.session_id, &live).await;
        return Err(MeshError::new(
            ErrorCode::AgentUnavailable,
            "the agent did not confirm the configured model",
        ));
    }
    let current = update_record(s, record.session_id, |current| {
        current.agent_session_id = Some(agent_session.clone());
        current.history_pending = false;
        if card.harness == HarnessKind::Dsh || !options.is_empty() {
            current.config_options = options;
        }
        current.config_values.retain(|id, _| !rejected.contains(id));
    })?;
    if let Ok(mut current) = live.agent_session.lock() {
        *current = agent_session;
    }
    // A list sent before session/new returned could not be shown until the
    // session ID and the corresponding settings were known.
    if commands::current_commands(&live).is_some() {
        record_event(s, record.session_id, "commands_updated", json!({}))?;
    }
    if let Some(note) = note {
        record_event(
            s,
            record.session_id,
            "status",
            json!({"status":current.status,"message":note}),
        )?;
    }
    Ok(live)
}

/// Apply this session's chosen settings (and the agent's default model when
/// none was chosen) on top of what the agent reported. Returns the settings as
/// the agent now reports them and the chosen setting IDs it refused.
async fn apply_settings(
    s: &AppState,
    live: &Live,
    agent_session: &str,
    id: Uuid,
    mut options: Vec<ConfigOption>,
    default_model: Option<(String, bool)>,
) -> (Vec<ConfigOption>, Vec<String>) {
    let Ok(record) = s.store.session(id) else {
        return (options, vec![]);
    };
    let mut wanted = record.config_values.clone();
    let mut rejected = vec![];
    if let Some(models) = &live.managed_models
        && let Some(option) = options.iter().find(|o| o.is_category("model"))
    {
        let selected = Value::from(models.selected(wanted.get(&option.id)));
        if wanted
            .get(&option.id)
            .is_some_and(|value| *value != selected)
        {
            rejected.push(option.id.clone());
        }
        wanted.insert(option.id.clone(), selected);
    } else if let Some((model, explicit)) = default_model
        && let Some(option) = options.iter().find(|o| o.is_category("model"))
        && !wanted.contains_key(&option.id)
        && (explicit || option.accepts(&Value::String(model.clone())))
    {
        wanted.insert(option.id.clone(), Value::String(model));
    }
    for (config_id, value) in config_apply_order(&wanted, &options) {
        let why = match options.iter().find(|o| o.id == config_id) {
            None if options.is_empty() => continue,
            None => "the agent does not offer this setting".to_owned(),
            Some(option) if option.current == value => continue,
            Some(_) => match live
                .conn
                .set_config_option(agent_session, &config_id, &value)
                .await
            {
                Ok(reported) => {
                    let parsed = live.options(&reported);
                    if parsed.is_empty() {
                        if let Some(option) = options.iter_mut().find(|o| o.id == config_id)
                            && !(live.managed_models.is_some() && option.is_category("model"))
                        {
                            option.current = value.clone();
                        }
                    } else {
                        options = parsed;
                    }
                    continue;
                }
                Err(error) => redact(&live.secrets, &error.message),
            },
        };
        let _ = record_event(
            s,
            id,
            "status",
            json!({"status":record.status,"message":format!("the agent did not accept the setting {config_id} = {value}: {why}")}),
        );
        if record.config_values.contains_key(&config_id) {
            rejected.push(config_id);
        }
    }
    (options, rejected)
}

fn begin_turn(
    s: &AppState,
    id: Uuid,
    text: &str,
    from: Value,
) -> hmesh_core::Result<SessionRecord> {
    begin_checked_turn(s, id, text, from, None)
}

fn begin_checked_turn(
    s: &AppState,
    id: Uuid,
    text: &str,
    from: Value,
    command: Option<String>,
) -> hmesh_core::Result<SessionRecord> {
    validate_prompt(text)?;
    let mut record = {
        let _guard = s
            .sessions
            .starting
            .lock()
            .map_err(|_| MeshError::new(ErrorCode::Internal, "session lock poisoned"))?;
        let mut record = s.store.session(id)?;
        match record.status {
            SessionStatus::Closed => {
                return Err(MeshError::new(
                    ErrorCode::TaskConflict,
                    "the session is closed",
                ));
            }
            SessionStatus::PendingApproval => {
                return Err(MeshError::new(
                    ErrorCode::ApprovalRequired,
                    "the session is waiting for approval on the agent's computer",
                ));
            }
            SessionStatus::Running | SessionStatus::WaitingPermission => {
                return Err(MeshError::new(
                    ErrorCode::TaskConflict,
                    "a turn is already running; wait for it or cancel it",
                ));
            }
            SessionStatus::Idle => {}
        }
        if s.sessions
            .configuring
            .lock()
            .map_err(|_| MeshError::new(ErrorCode::Internal, "session lock poisoned"))?
            .contains(&id)
        {
            return Err(MeshError::new(
                ErrorCode::TaskConflict,
                "a session setting is being changed; send again in a moment",
            ));
        }
        // A delegation a paired computer started stays usable only while
        // that computer is still paired.
        if record.origin.root_node_id != s.instance.card.node_id
            && s.task_decision(Actor {
                node_id: record.origin.root_node_id,
                owner: false,
            }) == Decision::Denied
        {
            return Err(MeshError::new(
                ErrorCode::PermissionDenied,
                "the computer that started this session is no longer paired",
            ));
        }
        check_place(s, &record)?;
        if let Some(name) = &command {
            commands::require_advertised(s.sessions.live(id).as_deref(), name)?;
        }
        record.status = SessionStatus::Running;
        if record.title.is_empty() {
            let first = text
                .lines()
                .find(|l| !l.trim().is_empty())
                .unwrap_or("")
                .trim();
            record.title = clip(first, 120).0;
        }
        record.last_error = None;
        save(s, &mut record)?;
        record
    };
    let (shown, _) = clip(text, MAX_EVENT_TEXT_BYTES);
    // An earlier session not reloaded yet: its history comes first, so the
    // message is stored as queued and shown once the agent has started (or
    // the session closes, or HarnessMesh restarts).
    let kind = if record.history_pending {
        hmesh_store::QUEUED_MESSAGE
    } else {
        "user_message"
    };
    record_event(s, id, kind, json!({"text":shown,"from":from}))?;
    let state = s.clone();
    let text = text.to_owned();
    tokio::spawn(async move { run_turn(state, id, text, command).await });
    record.updated_at = now();
    Ok(record)
}

/// Stop a turn that outlives the budget: the owner may not be watching, and an
/// agent can keep retrying a dead peer forever. The turn ends as `timeout`.
async fn turn_watchdog(s: AppState, id: Uuid, live: Arc<Live>, turn: u64) {
    tokio::time::sleep(turn_budget()).await;
    if live.turn.load(std::sync::atomic::Ordering::SeqCst) != turn {
        return;
    }
    let Ok(record) = s.store.session(id) else {
        return;
    };
    if !record.status.is_busy()
        || !s
            .sessions
            .live(id)
            .is_some_and(|current| Arc::ptr_eq(&current, &live))
    {
        return;
    }
    s.sessions.mark_expired(id);
    s.sessions.mark_cancelling(id);
    let _ = record_event(
        &s,
        id,
        "status",
        json!({"status":"cancelling","message":"the turn exceeded its time budget; stopping it"}),
    );
    cancel_delegations(&s, id);
    let agent_session = live
        .agent_session
        .lock()
        .map(|a| a.clone())
        .unwrap_or_default();
    let _ = live.conn.cancel(&agent_session);
    tokio::time::sleep(CANCEL_GRACE).await;
    if s.store
        .session(id)
        .map(|r| r.status.is_busy())
        .unwrap_or(false)
        && s.sessions
            .live(id)
            .is_some_and(|current| Arc::ptr_eq(&current, &live))
    {
        discard(&s, id, &live).await;
    }
}

async fn run_turn(s: AppState, id: Uuid, text: String, command: Option<String>) {
    let finish =
        |s: &AppState, stop: Option<&str>, final_text: Option<String>, error: Option<String>| {
            if let Ok(mut record) = s.store.session(id) {
                if record.status != SessionStatus::Closed {
                    record.status = SessionStatus::Idle;
                }
                if stop.is_some() {
                    record.turns += 1;
                    record.last_stop_reason = stop.map(str::to_owned);
                    record.last_final_text = final_text.clone();
                }
                record.last_error = error.clone();
                let _ = save(s, &mut record);
            }
            if let Some(stop) = stop {
                let (text, truncated) = final_text
                    .as_deref()
                    .map(|t| clip(t, MAX_FINAL_TEXT))
                    .map_or((None, false), |(t, c)| (Some(t), c));
                let _ = record_event(
                    s,
                    id,
                    "turn_end",
                    json!({"stop_reason":stop,"final_text":text,"truncated":truncated}),
                );
            }
            if let Some(error) = error {
                let _ = record_event(s, id, "error", json!({"message":error}));
            }
        };
    let record = match s.store.session(id) {
        Ok(record) => record,
        Err(_) => return,
    };
    let started = ensure_live(&s, &record).await;
    if record.history_pending {
        release_queued(&s, id);
    }
    let live = match started {
        Ok(live) => live,
        // Closed while starting: the close already explains itself.
        Err(_) if !s.store.session(id).is_ok_and(|r| r.status.is_open()) => return,
        // An agent's error text can quote its environment; keep secrets out.
        Err(error) => {
            let secrets = agent_secrets(&s, record.agent_id);
            return finish(&s, None, None, Some(redact(&secrets, &error.message)));
        }
    };
    // Closed (or revoked) while the agent was starting: never send the prompt.
    if !matches!(
        s.store.session(id).map(|r| r.status),
        Ok(SessionStatus::Running)
    ) {
        return;
    }
    if let Ok(mut active) = live.last_active.lock() {
        *active = Instant::now();
    }
    if let Ok(mut reply) = live.reply.lock() {
        *reply = Reply::default();
    }
    if let Ok(mut open) = live.open_tools.lock() {
        open.clear();
    }
    let turn = live.turn.fetch_add(1, std::sync::atomic::Ordering::SeqCst) + 1;
    tokio::spawn(turn_watchdog(s.clone(), id, live.clone(), turn));
    let agent_session = live
        .agent_session
        .lock()
        .map(|a| a.clone())
        .unwrap_or_default();
    if let Some(name) = &command {
        // A service change can restart the process after submission. Its old
        // catalogue must never authorize a command on the replacement.
        live.conn.barrier().await;
        if let Err(error) = commands::require_advertised(Some(&live), name) {
            return finish(&s, None, None, Some(error.message));
        }
    }
    let result = live.conn.prompt(&agent_session, &text).await;
    // The turn's last chunks may still be queued behind the response.
    live.conn.finish_turn().await;
    let final_text = live
        .reply
        .lock()
        .ok()
        .and_then(|mut r| r.take())
        .map(|t| redact(&live.secrets, &t));
    if let Ok(mut pending) = live.permissions.lock() {
        pending.clear();
    }
    if let Ok(mut active) = live.last_active.lock() {
        *active = Instant::now();
    }
    match result {
        Ok(response) => {
            let stop = response
                .get("stopReason")
                .and_then(Value::as_str)
                .unwrap_or("end_turn")
                .to_owned();
            // A stopped turn leaves its tool call without a terminal state in
            // the agent's own stream; mark it so the panel does not show a
            // command as still waiting (and a reload cannot re-run it quietly).
            if stop == "cancelled"
                && let Ok(mut open) = live.open_tools.lock()
            {
                for (call, title) in open.drain() {
                    let _ = record_event(
                        &s,
                        id,
                        "tool_call_update",
                        json!({"id":call,"title":title,"status":"cancelled"}),
                    );
                }
            }
            let latest = live.usage.lock().ok().and_then(|u| u.clone());
            let fresh = live
                .usage_fresh
                .swap(false, std::sync::atomic::Ordering::SeqCst);
            if let Some(payload) = turn_usage(latest.as_ref(), fresh, response.get("usage")) {
                let _ = record_event(&s, id, "usage", payload);
            }
            // An empty prompt answer is a failure, not a quiet success: the
            // common cause is an exhausted provider retry (rate limit, auth).
            let error =
                (command.is_none() && stop == "end_turn" && final_text.is_none()).then(|| {
                    "the agent ended the turn without an answer; check its login and model service"
                        .to_owned()
                });
            s.sessions.take_cancelling(id);
            let expired = s.sessions.take_expired(id);
            if expired && final_text.is_none() {
                finish(
                    &s,
                    Some("timeout"),
                    None,
                    Some("the turn exceeded its time budget".into()),
                )
            } else {
                finish(&s, Some(&stop), final_text, error)
            }
        }
        Err(error) => {
            // The process is gone: its unfinished tools will never report.
            if let Ok(mut open) = live.open_tools.lock() {
                for (call, title) in open.drain() {
                    let _ = record_event(
                        &s,
                        id,
                        "tool_call_update",
                        json!({"id":call,"title":title,"status":"failed"}),
                    );
                }
            }
            if s.sessions.take_expired(id) {
                finish(
                    &s,
                    Some("timeout"),
                    None,
                    Some("the turn exceeded its time budget".into()),
                )
            } else if s.sessions.take_cancelling(id) {
                finish(&s, Some("cancelled"), None, None)
            } else {
                finish(&s, None, None, Some(redact(&live.secrets, &error.message)))
            }
        }
    }
}

/// The agent's options a person answers with: all of them on the agent's
/// own computer, only the one-time ones when another computer answers.
fn chosen_outcome(
    options: &[Value],
    allow: bool,
    option_id: Option<&str>,
    once_only: bool,
) -> hmesh_core::Result<(Value, bool, Option<Value>)> {
    let once = |kind: &str| matches!(kind, "allow_once" | "reject_once");
    if let Some(chosen) = option_id {
        let option = options
            .iter()
            .find(|o| o.get("optionId").and_then(Value::as_str) == Some(chosen))
            .ok_or_else(|| MeshError::input("the agent did not offer this option"))?;
        let kind = option.get("kind").and_then(Value::as_str).unwrap_or("");
        if once_only && !once(kind) {
            return Err(MeshError::new(
                ErrorCode::PermissionDenied,
                "another computer can only allow or reject once; lasting permissions are given on the agent's computer",
            ));
        }
        let shown = json!({"option_id":chosen,"name":option.get("name"),"kind":kind});
        return Ok((
            json!({"outcome":{"outcome":"selected","optionId":chosen}}),
            kind.starts_with("allow"),
            Some(shown),
        ));
    }
    if once_only {
        let wanted = if allow { "allow_once" } else { "reject_once" };
        if let Some(id) = options
            .iter()
            .find(|o| o.get("kind").and_then(Value::as_str) == Some(wanted))
            .and_then(|o| o.get("optionId"))
        {
            return Ok((
                json!({"outcome":{"outcome":"selected","optionId":id}}),
                allow,
                None,
            ));
        }
        if allow {
            return Err(MeshError::new(
                ErrorCode::PermissionDenied,
                "this agent offers no one-time permission; answer on the agent's computer",
            ));
        }
        // No one-time rejection either: cancel this request, decide nothing lasting.
        return Ok((json!({"outcome":{"outcome":"cancelled"}}), false, None));
    }
    Ok((acp::permission_outcome(options, allow), allow, None))
}

fn answer(
    s: &AppState,
    id: Uuid,
    key: &str,
    body: &PermissionAnswer,
    by: &str,
) -> hmesh_core::Result<SessionRecord> {
    let live = s.sessions.live(id).ok_or_else(|| {
        MeshError::new(
            ErrorCode::TaskConflict,
            "the agent for this session is not running",
        )
    })?;
    let (rpc, (outcome, allow, option), left) = {
        let mut pending = live
            .permissions
            .lock()
            .map_err(|_| MeshError::new(ErrorCode::Internal, "permission map poisoned"))?;
        let (_, options) = pending.get(key).ok_or_else(|| {
            MeshError::new(
                ErrorCode::TaskConflict,
                "no pending permission request with this key",
            )
        })?;
        // A refused answer leaves the request pending.
        let chosen = chosen_outcome(
            options,
            body.allow,
            body.option_id.as_deref(),
            by == "origin",
        )?;
        let (rpc, _) = pending.remove(key).expect("checked above");
        (rpc, chosen, pending.len())
    };
    live.conn.respond(&rpc, outcome)?;
    let mut resolved = json!({"key":key,"allow":allow,"by":by});
    if let Some(mut option) = option {
        if let Some(name) = option["name"].as_str() {
            option["name"] = json!(clipped(&live.secrets, name, 256));
        }
        resolved["option"] = option;
    }
    record_event(s, id, "permission_resolved", resolved)?;
    if left == 0 {
        // Only a turn still waiting goes back to running; a turn that ended
        // meanwhile stays idle.
        let mut record = s.store.session(id)?;
        if record.status == SessionStatus::WaitingPermission {
            record.status = SessionStatus::Running;
            save(s, &mut record)?;
        }
        return Ok(record);
    }
    s.store.session(id)
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct DelegationLink {
    peer_id: Uuid,
    session_id: Uuid,
}

/// Record that one of this session's delegated sessions runs on a peer, so a
/// cancel or close here reaches it there.
async fn owner_delegation(
    State(s): State<AppState>,
    Extension(c): Extension<ConnectionIdentity>,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
    Json(body): Json<DelegationLink>,
) -> ApiResult<Value> {
    s.owner(&headers, &c)?;
    let record = s.store.session(id)?;
    if !record.status.is_open() {
        return Err(err(
            ErrorCode::TaskConflict,
            "the calling session is closed",
        ));
    }
    s.sessions.add_delegation(id, body.peer_id, body.session_id);
    Ok(Json(json!({"recorded": true})))
}

/// Cancel the delegated sessions this computer's session opened on peers.
/// Best effort: a peer that is already gone cannot be told.
fn cancel_delegations(s: &AppState, parent: Uuid) {
    let links = s.sessions.take_delegations(parent);
    if links.is_empty() {
        return;
    }
    let state = s.clone();
    tokio::spawn(async move {
        for (peer, child) in links {
            if let Ok(client) = peer_client(&state, peer) {
                let _ = tokio::time::timeout(
                    Duration::from_secs(5),
                    client.post::<_, Value>(
                        &format!("/hmesh/v1/sessions/{child}/cancel"),
                        &json!({}),
                    ),
                )
                .await;
            }
        }
    });
}

fn cancel(s: &AppState, id: Uuid) -> hmesh_core::Result<SessionRecord> {
    let record = s.store.session(id)?;
    if record.status == SessionStatus::PendingApproval {
        return close_session(s, id, "the request was withdrawn before approval");
    }
    if let Some(live) = s.sessions.live(id) {
        // ACP: pending permission requests must be answered `cancelled`.
        let pending: Vec<(Value, Vec<Value>)> = live
            .permissions
            .lock()
            .map(|mut p| p.drain().map(|(_, v)| v).collect())
            .unwrap_or_default();
        for (rpc, _) in pending {
            let _ = live
                .conn
                .respond(&rpc, json!({"outcome":{"outcome":"cancelled"}}));
        }
        if record.status.is_busy() {
            let agent_session = live
                .agent_session
                .lock()
                .map(|a| a.clone())
                .unwrap_or_default();
            live.conn.cancel(&agent_session)?;
            s.sessions.mark_cancelling(id);
            record_event(
                s,
                id,
                "status",
                json!({"status":"cancelling","message":"cancel requested"}),
            )?;
            // The agent may be gone in a way that leaves its ACP connection
            // open. End this session's process if the turn is still busy
            // after a short grace, so the owner can continue.
            let state = s.clone();
            let watched = live.clone();
            tokio::spawn(async move {
                tokio::time::sleep(CANCEL_GRACE).await;
                let Ok(now) = state.store.session(id) else {
                    return;
                };
                if !now.status.is_busy()
                    || !state
                        .sessions
                        .live(id)
                        .is_some_and(|live| Arc::ptr_eq(&live, &watched))
                {
                    return;
                }
                let _ = record_event(
                    &state,
                    id,
                    "status",
                    json!({"status":"cancelling","message":"the agent did not stop in time; its process was ended"}),
                );
                discard(&state, id, &watched).await;
            });
        }
    }
    cancel_delegations(s, id);
    s.store.session(id)
}

fn close_session(s: &AppState, id: Uuid, why: &str) -> hmesh_core::Result<SessionRecord> {
    // Mark it closed before taking its process: an agent that is still
    // starting sees the closed status once it is registered (see ensure_live).
    cancel_delegations(s, id);
    let mut record = s.store.session(id)?;
    let newly = record.status != SessionStatus::Closed;
    if newly {
        record.status = SessionStatus::Closed;
        record.pending_message = None;
        save(s, &mut record)?;
    }
    let live = s
        .sessions
        .live
        .lock()
        .ok()
        .and_then(|mut map| map.remove(&id));
    if let Some(live) = live {
        tokio::spawn(async move { live.conn.shutdown().await });
    }
    if newly {
        release_queued(s, id);
        record_event(s, id, "status", json!({"status":"closed","message":why}))?;
    }
    Ok(record)
}

fn approve(s: &AppState, id: Uuid) -> hmesh_core::Result<SessionRecord> {
    let mut record = s.store.session(id)?;
    if record.status != SessionStatus::PendingApproval {
        return Err(MeshError::new(
            ErrorCode::TaskConflict,
            "the session is not waiting for approval",
        ));
    }
    record.status = SessionStatus::Idle;
    let first = record.pending_message.take();
    save(s, &mut record)?;
    record_event(
        s,
        id,
        "status",
        json!({"status":"idle","message":"approved on this computer"}),
    )?;
    match first {
        Some(message) => {
            let from = json!({"node_id":record.origin.node_id,"label":record.origin.label});
            begin_turn(s, id, &message, from)
        }
        None => Ok(record),
    }
}

/// Change one setting of a session on this computer between turns. A running
/// agent applies it at once; otherwise it is applied when the agent starts.
async fn set_config(
    s: &AppState,
    id: Uuid,
    body: SetSessionConfig,
) -> hmesh_core::Result<SessionRecord> {
    validate_config_value(&body.config_id, &body.value)?;
    let record = {
        let _guard = s
            .sessions
            .starting
            .lock()
            .map_err(|_| MeshError::new(ErrorCode::Internal, "session lock poisoned"))?;
        let record = s.store.session(id)?;
        match record.status {
            SessionStatus::Closed => {
                return Err(MeshError::new(
                    ErrorCode::TaskConflict,
                    "the session is closed",
                ));
            }
            SessionStatus::Running | SessionStatus::WaitingPermission => {
                return Err(MeshError::new(
                    ErrorCode::TaskConflict,
                    "settings change between turns; wait for this turn to end or stop it",
                ));
            }
            SessionStatus::Idle | SessionStatus::PendingApproval => {}
        }
        // No turn starts while a setting is being changed (begin_turn checks
        // this under the same lock).
        if !s
            .sessions
            .configuring
            .lock()
            .map_err(|_| MeshError::new(ErrorCode::Internal, "session lock poisoned"))?
            .insert(id)
        {
            return Err(MeshError::new(
                ErrorCode::TaskConflict,
                "another setting is being changed in this session",
            ));
        }
        record
    };
    let _changing = Configuring {
        sessions: &s.sessions,
        id,
    };
    let known = if record.config_options.is_empty() {
        s.store.agent(record.agent_id)?.0.config_options
    } else {
        record.config_options.clone()
    };
    let name = match known.iter().find(|o| o.id == body.config_id) {
        Some(option) if !option.accepts(&body.value) => {
            return Err(MeshError::input(format!(
                "{} is not one of the choices for {}; start a new session once so the agent reports its current list",
                body.value, option.name
            )));
        }
        Some(option) => option.name.clone(),
        None if known.is_empty() => body.config_id.clone(),
        None => {
            return Err(MeshError::input(format!(
                "this agent has no setting {:?}",
                body.config_id
            )));
        }
    };
    let live = s.sessions.live(id).filter(|live| live.conn.alive());
    let mut options = known;
    if let Some(live) = live {
        let agent_session = live
            .agent_session
            .lock()
            .map(|a| a.clone())
            .unwrap_or_default();
        let reported = live
            .conn
            .set_config_option(&agent_session, &body.config_id, &body.value)
            .await
            .map_err(|e| {
                MeshError::input(format!(
                    "the agent did not accept this setting: {}",
                    redact(&live.secrets, &e.message)
                ))
            })?;
        let parsed = live.options(&reported);
        if live.managed_models.is_some()
            && options
                .iter()
                .any(|o| o.id == body.config_id && o.is_category("model"))
            && !parsed
                .iter()
                .any(|o| o.id == body.config_id && o.current == body.value)
        {
            discard(s, id, &live).await;
            return Err(MeshError::input(
                "the agent did not confirm the selected model",
            ));
        }
        if !parsed.is_empty() {
            options = parsed;
        }
    }
    if let Some(option) = options.iter_mut().find(|o| o.id == body.config_id)
        && option.current != body.value
        && option.accepts(&body.value)
    {
        option.current = body.value.clone();
    }
    let record = update_record(s, id, |record| {
        record.config_options = options;
        record
            .config_values
            .insert(body.config_id.clone(), body.value.clone());
    })?;
    let shown = record
        .config_options
        .iter()
        .find(|o| o.id == body.config_id)
        .map_or_else(|| body.value.to_string(), ConfigOption::current_name);
    record_event(
        s,
        id,
        "config_changed",
        json!({"config_id":body.config_id,"name":name,"value":body.value,"value_name":shown}),
    )?;
    Ok(record)
}

fn running_children(s: &AppState, parent: Uuid) -> usize {
    let child_of = |record: &SessionRecord| {
        record.status.is_busy()
            && record
                .origin
                .chain
                .last()
                .is_some_and(|l| l.session_id == parent)
    };
    let local = s.store.sessions(1000).unwrap_or_default();
    let remote = s.store.remote_sessions(1000).unwrap_or_default();
    local.iter().filter(|r| child_of(r)).count()
        + remote.iter().filter(|(_, r)| child_of(r)).count()
}

/// A session a paired computer started may only run in a project that is
/// still shared. Checked before every turn and every agent start.
fn check_place(s: &AppState, record: &SessionRecord) -> hmesh_core::Result<()> {
    if record.origin.root_node_id != s.instance.card.node_id
        && let Some(cwd) = &record.cwd
        && !projects::still_shared(s, cwd)
    {
        return Err(MeshError::new(
            ErrorCode::PermissionDenied,
            "this project is no longer shared with other computers",
        ));
    }
    Ok(())
}

/// Whether an earlier start already recorded the replayed history, e.g. when
/// the daemon stopped before it noted the history as kept. A history is
/// recorded whole (see `record_history`), so any replayed event tells.
fn replay_recorded(s: &AppState, id: Uuid) -> bool {
    s.store.has_replayed_events(id).unwrap_or(true)
}

/// How long a reload's replay may take to pass through the pump.
const REPLAY_WAIT: Duration = Duration::from_secs(120);

/// A session closed while its agent was starting records nothing more.
fn still_open(s: &AppState, id: Uuid) -> hmesh_core::Result<()> {
    if s.store.session(id)?.status.is_open() {
        Ok(())
    } else {
        Err(MeshError::new(
            ErrorCode::TaskConflict,
            "the session was closed while its agent was starting",
        ))
    }
}

/// Show messages queued while the history was not loaded yet, after
/// everything recorded so far.
fn release_queued(s: &AppState, id: Uuid) {
    if let Ok(Some(seq)) = s.store.release_queued_messages(id) {
        s.sessions.notify(id, seq);
    }
}

/// Record the history replayed during a first reload, all at once and marked
/// as replayed.
fn record_history(s: &AppState, id: Uuid, live: &Live) -> hmesh_core::Result<()> {
    let kept = live
        .replayed
        .lock()
        .map(|mut kept| std::mem::take(&mut *kept))
        .unwrap_or_default();
    let marked: Vec<(String, Value)> = kept
        .into_iter()
        .map(|(kind, mut payload)| {
            payload["replayed"] = json!(true);
            (kind, payload)
        })
        .collect();
    if let Some(seq) = s.store.append_session_events(id, &marked)? {
        s.sessions.notify(id, seq);
    }
    Ok(())
}

/// Projects this computer's agents can work in when asked: every project
/// that is not hidden.
pub(crate) fn usable_projects(
    s: &AppState,
) -> hmesh_core::Result<Vec<hmesh_core::projects::SharedProject>> {
    Ok(projects::projects(s)?
        .into_iter()
        .filter(|p| !p.hidden)
        .map(|p| hmesh_core::projects::SharedProject {
            project_id: p.project_id,
            name: p.name,
        })
        .collect())
}

/// Create a session on one of this node's agents.
#[allow(clippy::too_many_arguments)]
fn open_here(
    s: &AppState,
    session_id: Uuid,
    agent_id: Uuid,
    origin: SessionOrigin,
    approver: Approver,
    needs_approval: bool,
    message: Option<String>,
    config: BTreeMap<String, Value>,
    place: projects::Place,
) -> hmesh_core::Result<SessionRecord> {
    let (card, config_file) = s.store.agent(agent_id)?;
    if s.store.agent_retired(agent_id)? || card.availability == "disabled" {
        return Err(MeshError::new(
            ErrorCode::AgentUnavailable,
            "this agent is disabled on its computer",
        ));
    }
    if !card.probe.version_supported {
        return Err(MeshError::new(
            ErrorCode::AdapterNotSupported,
            "this agent did not pass the ACP initialize check; register it again after fixing its launch command",
        ));
    }
    check_chain(&origin.chain, s.instance.card.node_id, agent_id)?;
    if let Some(message) = &message {
        validate_prompt(message)?;
    }
    // Checking for an existing continuation and creating one happen under
    // the turn lock, so two quick requests continue the session once.
    let continuing = match &place.resume {
        Some(_) => Some(
            s.sessions
                .starting
                .lock()
                .map_err(|_| MeshError::new(ErrorCode::Internal, "session lock poisoned"))?,
        ),
        None => None,
    };
    if let Some(resume) = &place.resume {
        if message.is_some() {
            return Err(MeshError::input(
                "continue the session first, then send a message",
            ));
        }
        // The same earlier session is continued once at a time.
        if let Some(open) = s.store.sessions(1000)?.into_iter().find(|r| {
            r.agent_id == agent_id
                && r.status.is_open()
                && r.agent_session_id.as_deref() == Some(resume.as_str())
        }) {
            return Ok(open);
        }
    }
    validate_config(&config)?;
    // Until the agent starts, show the settings it offered last time with
    // this session's choices (or the default model) as current values.
    let mut options = card.config_options.clone();
    let known = !options.is_empty();
    for (id, value) in &config {
        match options.iter_mut().find(|o| &o.id == id) {
            Some(option) if option.accepts(value) => option.current = value.clone(),
            Some(option) => {
                return Err(MeshError::input(format!(
                    "{value} is not one of the choices for {}; start a new session once so the agent reports its current list",
                    option.name
                )));
            }
            None if known => {
                return Err(MeshError::input(format!(
                    "this agent has no setting {id:?}"
                )));
            }
            None => {}
        }
    }
    if let Some(model) = &card.default_model
        && let Some(option) = options.iter_mut().find(|o| o.is_category("model"))
        && !config.contains_key(&option.id)
    {
        let value = Value::String(model.clone());
        if option.accepts(&value) {
            option.current = value;
        }
    }
    let at = now();
    let mut record = SessionRecord {
        session_id,
        node_id: s.instance.card.node_id,
        agent_id,
        agent_name: card.name,
        project_id: place
            .project_id
            .clone()
            .unwrap_or(config_file.agent.project_id),
        origin,
        approver,
        status: if needs_approval {
            SessionStatus::PendingApproval
        } else if place.resume.is_some() {
            // Busy until the agent has reloaded it.
            SessionStatus::Running
        } else {
            SessionStatus::Idle
        },
        title: place.title.clone().unwrap_or_default(),
        turns: 0,
        agent_session_id: place.resume.clone(),
        last_stop_reason: None,
        last_final_text: None,
        last_error: None,
        pending_message: None,
        config_options: options,
        config_values: config,
        created_at: at,
        updated_at: at,
        cwd: place.cwd.clone(),
        history_pending: place.resume.is_some(),
    };
    if needs_approval {
        record.pending_message = message.clone();
        if let Some(message) = &message {
            record.title = clip(message.lines().next().unwrap_or("").trim(), 120).0;
        }
    }
    s.store.create_session(&record)?;
    drop(continuing);
    let opened_by = match (&record.origin.kind, &record.origin.label) {
        (_, Some(label)) => format!("opened by {label}"),
        (OriginKind::Human, None) if record.origin.node_id == s.instance.card.node_id => {
            "opened on this computer".into()
        }
        _ => "opened from a paired computer".into(),
    };
    record_event(
        s,
        session_id,
        "status",
        json!({"status":record.status,"message":opened_by}),
    )?;
    if place.resume.is_some() {
        let state = s.clone();
        tokio::spawn(async move { continue_earlier(state, session_id).await });
        return Ok(record);
    }
    match message {
        Some(message) if !needs_approval => {
            let from = json!({"node_id":record.origin.node_id,"label":record.origin.label});
            begin_turn(s, session_id, &message, from)
        }
        _ => Ok(record),
    }
}

/// Start the agent for a session that continues one it already had, so its
/// earlier conversation is shown before the first message.
async fn continue_earlier(s: AppState, id: Uuid) {
    let Ok(record) = s.store.session(id) else {
        return;
    };
    let error = match ensure_live(&s, &record).await {
        Ok(_) => None,
        Err(e) => Some(e.message),
    };
    let _ = update_record(&s, id, |r| {
        if r.status == SessionStatus::Running {
            r.status = SessionStatus::Idle;
        }
        if let Some(error) = &error {
            r.last_error = Some(error.clone());
        }
    });
    if let Some(error) = error {
        let _ = record_event(&s, id, "error", json!({"message":error}));
    }
}

fn peer_client(s: &AppState, peer: Uuid) -> std::result::Result<Client, ApiError> {
    let peer = s.store.peer(peer)?;
    if peer.status != "trusted" {
        return Err(err(
            ErrorCode::PermissionDenied,
            "that computer is not paired",
        ));
    }
    Ok(s.peer_client(&peer)?)
}

/// Open a session for this node's owner (a person, or an agent through
/// `hmesh mcp --session`), here or on a paired node.
pub async fn open(s: &AppState, body: OpenSession) -> std::result::Result<SessionRecord, ApiError> {
    let me = s.instance.card.node_id;
    let parent = match body.parent_session_id {
        Some(parent) => {
            let record = s.store.session(parent).map_err(|_| {
                err(
                    ErrorCode::PermissionDenied,
                    "the calling session does not exist on this computer",
                )
            })?;
            if !record.status.is_open() {
                return Err(err(
                    ErrorCode::PermissionDenied,
                    "the calling session is closed",
                ));
            }
            if running_children(s, parent) >= MAX_RUNNING_CHILDREN {
                return Err(err(
                    ErrorCode::RateLimited,
                    format!(
                        "a session may run at most {MAX_RUNNING_CHILDREN} delegated sessions at once"
                    ),
                ));
            }
            Some(record)
        }
        None => None,
    };
    let (kind, chain, label, root) = match &parent {
        Some(p) => (
            OriginKind::Agent,
            p.child_chain(),
            Some(format!(
                "{} / {}",
                s.instance.card.display_name, p.agent_name
            )),
            p.origin.root_node_id,
        ),
        None => (OriginKind::Human, vec![], None, me),
    };
    let session_id = Uuid::new_v4();
    let target = body.peer_id.filter(|p| *p != me);
    if let Some(project) = &body.project_id {
        hmesh_core::projects::validate_project_id(project)?;
    }
    if let Some(resume) = &body.resume {
        hmesh_core::projects::validate_agent_session_id(resume)?;
        // An agent's earlier sessions stay with its computer's owner.
        if target.is_some() || parent.is_some() {
            return Err(err(
                ErrorCode::PermissionDenied,
                "only this computer's owner can continue an agent's earlier sessions",
            ));
        }
    }
    let title = body
        .title
        .as_deref()
        .map(hmesh_core::projects::clean_name)
        .transpose()?;
    if target.is_some() && root != me {
        // Pairing is between two computers; a chain another computer started
        // must not reach further computers with this computer's identity.
        return Err(err(
            ErrorCode::PermissionDenied,
            "cross-computer delegation is limited to one hop: this session was opened by another computer, so it can only call agents on this computer",
        ));
    }
    match target {
        None => {
            let origin = SessionOrigin {
                node_id: me,
                kind,
                label,
                chain,
                root_node_id: root,
            };
            let (approver, needs_approval) = match kind {
                OriginKind::Human => (Approver::Origin, false),
                // A chain started on this computer acts for its owner.
                OriginKind::Agent if root == me => (Approver::Auto, false),
                // A chain a paired computer started follows that computer's
                // access mode here, however deep the delegation goes.
                OriginKind::Agent => match s.task_decision(Actor {
                    node_id: root,
                    owner: false,
                }) {
                    Decision::Allowed => (Approver::Auto, false),
                    Decision::ApprovalRequired => (Approver::Local, true),
                    Decision::Denied => {
                        return Err(err(
                            ErrorCode::PermissionDenied,
                            "the computer that started this delegation is no longer paired",
                        ));
                    }
                },
            };
            let mut place =
                projects::place(s, body.agent_id, body.project_id.as_deref(), root == me)?;
            place.resume = body.resume;
            place.title = title.filter(|_| place.resume.is_some());
            Ok(open_here(
                s,
                session_id,
                body.agent_id,
                origin,
                approver,
                needs_approval,
                body.message,
                body.config,
                place,
            )?)
        }
        Some(peer) => {
            check_chain(&chain, peer, body.agent_id)?;
            validate_config(&body.config)?;
            let client = peer_client(s, peer)?;
            let request = PeerOpenSession {
                session_id,
                agent_id: body.agent_id,
                kind,
                label,
                chain,
                message: body.message,
                config: body.config,
                project_id: body.project_id,
            };
            let record: SessionRecord = client.post("/hmesh/v1/sessions", &request).await?;
            if record.session_id != session_id || record.node_id != peer {
                return Err(err(
                    ErrorCode::Internal,
                    "the paired computer returned a different session",
                ));
            }
            s.store.record_remote_session(peer, &record)?;
            Ok(record)
        }
    }
}

async fn wait_events(
    s: &AppState,
    id: Uuid,
    after: u64,
    wait: u64,
    status_only: bool,
) -> hmesh_core::Result<SessionEventPage> {
    let mut watcher = s.sessions.watch(id);
    let deadline = tokio::time::Instant::now() + Duration::from_secs(wait.min(MAX_WAIT_SECONDS));
    let first = s.store.session(id)?;
    loop {
        let session = s.store.session(id)?;
        let mut events = if status_only {
            Vec::new()
        } else {
            s.store.session_events(id, after, MAX_PAGE_EVENTS)?
        };
        fit_page(&mut events);
        let changed = session.status != first.status || session.updated_at != first.updated_at;
        // A turn-end wait returns at once once nothing more will happen without
        // a new message; a session waiting for approval keeps the caller waiting.
        let settled =
            status_only && matches!(session.status, SessionStatus::Idle | SessionStatus::Closed);
        if !events.is_empty() || changed || settled || tokio::time::Instant::now() >= deadline {
            let next = events.last().map_or(after, |e| e.seq);
            return Ok(SessionEventPage {
                session,
                events,
                next,
            });
        }
        tokio::select! {
            changed = watcher.changed() => {
                if changed.is_err() {
                    tokio::time::sleep(Duration::from_millis(200)).await;
                }
            }
            _ = tokio::time::sleep_until(deadline) => {}
        }
    }
}

/// Stop idle agent processes; restore sessions interrupted by a restart.
pub async fn run(s: AppState, mut shutdown: watch::Receiver<bool>) {
    if let Ok(sessions) = s.store.sessions(10_000) {
        for record in sessions.into_iter().filter(|r| r.status.is_busy()) {
            release_queued(&s, record.session_id);
            let _ = set_status(&s, record.session_id, SessionStatus::Idle);
            let _ = record_event(
                &s,
                record.session_id,
                "error",
                json!({"message":"the turn was interrupted because HarnessMesh restarted"}),
            );
        }
    }
    let mut tick = tokio::time::interval(Duration::from_secs(30));
    loop {
        tokio::select! {
            changed = shutdown.changed() => { if changed.is_err() || *shutdown.borrow() { break; } }
            _ = tick.tick() => {
                let idle: Vec<(Uuid, Arc<Live>)> = s.sessions.live.lock().map(|map| map.iter().map(|(k, v)| (*k, v.clone())).collect()).unwrap_or_default();
                for (id, live) in idle {
                    let quiet = live.last_active.lock().map(|t| t.elapsed() >= Duration::from_secs(IDLE_PROCESS_SECONDS)).unwrap_or(false);
                    if !quiet {
                        continue;
                    }
                    // Decide under the turn lock so a turn starting now either
                    // keeps this process or starts a fresh one.
                    let taken = {
                        let Ok(_guard) = s.sessions.starting.lock() else { continue };
                        let busy = s.store.session(id).map(|r| r.status.is_busy()).unwrap_or(false)
                            || s.sessions.configuring.lock().map(|c| c.contains(&id)).unwrap_or(true);
                        !busy && s.sessions.live.lock().map(|mut map| {
                            map.get(&id).is_some_and(|l| Arc::ptr_eq(l, &live)) && map.remove(&id).is_some()
                        }).unwrap_or(false)
                    };
                    if taken {
                        let _ = record_event(&s, id, "commands_updated", json!({}));
                        live.conn.shutdown().await;
                    }
                }
                // Runs on its own so slow peers never delay reaping or shutdown.
                if !s.sessions.refreshing.swap(true, std::sync::atomic::Ordering::SeqCst) {
                    let state = s.clone();
                    let mut stop = shutdown.clone();
                    tokio::spawn(async move {
                        tokio::select! {
                            _ = refresh_remote(&state) => {}
                            _ = stop.changed() => {}
                        }
                        state.sessions.refreshing.store(false, std::sync::atomic::Ordering::SeqCst);
                    });
                }
            }
        }
    }
    let all: Vec<Arc<Live>> = s
        .sessions
        .live
        .lock()
        .map(|mut map| map.drain().map(|(_, v)| v).collect())
        .unwrap_or_default();
    for live in all {
        live.conn.shutdown().await;
    }
}

/// Close every session a revoked peer started here: the ones it opened and
/// every session its agents' delegation chains opened on this computer.
pub fn close_for_peer(s: &AppState, peer: Uuid) {
    for record in s.store.sessions(10_000).unwrap_or_default() {
        if (record.origin.node_id == peer || record.origin.root_node_id == peer)
            && record.status.is_open()
        {
            let _ = close_session(s, record.session_id, "pairing was revoked");
        }
    }
    for (owner, mut record) in s.store.remote_sessions(10_000).unwrap_or_default() {
        if owner == peer && record.status.is_open() {
            record.status = SessionStatus::Closed;
            let _ = s.store.record_remote_session(peer, &record);
        }
    }
}

// ---- HTTP: owner ----

#[derive(Deserialize)]
struct EventQuery {
    #[serde(default)]
    after: u64,
    #[serde(default)]
    wait: u64,
    #[serde(default)]
    status_only: bool,
}
#[derive(Serialize)]
struct SessionList {
    sessions: Vec<SessionRecord>,
    remote: Vec<RemoteSession>,
    /// This computer's names, pins and hidden flags for sessions.
    marks: Vec<hmesh_core::projects::SessionMark>,
}
#[derive(Serialize)]
struct RemoteSession {
    peer_id: Uuid,
    session: SessionRecord,
}

async fn list(
    State(s): State<AppState>,
    Extension(c): Extension<ConnectionIdentity>,
    headers: HeaderMap,
) -> ApiResult<SessionList> {
    s.owner(&headers, &c)?;
    Ok(Json(SessionList {
        sessions: s.store.sessions(200)?,
        remote: s
            .store
            .remote_sessions(200)?
            .into_iter()
            .map(|(peer_id, session)| RemoteSession { peer_id, session })
            .collect(),
        marks: s.store.session_marks()?,
    }))
}
async fn owner_open(
    State(s): State<AppState>,
    Extension(c): Extension<ConnectionIdentity>,
    headers: HeaderMap,
    Json(body): Json<OpenSession>,
) -> ApiResult<SessionRecord> {
    s.owner(&headers, &c)?;
    Ok(Json(open(&s, body).await?))
}
/// This computer's copy of a session it opened elsewhere.
fn remote_record(s: &AppState, id: Uuid) -> Option<SessionRecord> {
    s.store
        .remote_sessions(10_000)
        .ok()?
        .into_iter()
        .find(|(_, record)| record.session_id == id)
        .map(|(_, record)| record)
}

async fn owner_get(
    State(s): State<AppState>,
    Extension(c): Extension<ConnectionIdentity>,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
) -> ApiResult<SessionRecord> {
    s.owner(&headers, &c)?;
    if let Some(peer) = s.store.remote_session_peer(id)? {
        if let Some(local) = remote_record(&s, id)
            && local.status == SessionStatus::Closed
        {
            return Ok(Json(local));
        }
        let record: SessionRecord = peer_client(&s, peer)?
            .get(&format!("/hmesh/v1/sessions/{id}"))
            .await?;
        mirror(&s, peer, id, &record)?;
        return Ok(Json(record));
    }
    Ok(Json(s.store.session(id)?))
}
async fn owner_events(
    State(s): State<AppState>,
    Extension(c): Extension<ConnectionIdentity>,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
    Query(q): Query<EventQuery>,
) -> ApiResult<SessionEventPage> {
    s.owner(&headers, &c)?;
    if let Some(peer) = s.store.remote_session_peer(id)? {
        if let Some(local) = remote_record(&s, id)
            && local.status == SessionStatus::Closed
        {
            let mut events = s.store.session_events(id, q.after, MAX_PAGE_EVENTS)?;
            fit_page(&mut events);
            let next = events.last().map_or(q.after, |e| e.seq);
            return Ok(Json(SessionEventPage {
                session: local,
                events,
                next,
            }));
        }
        let page: SessionEventPage = peer_client(&s, peer)?
            .get(&format!(
                "/hmesh/v1/sessions/{id}/events?after={}&wait={}&status_only={}",
                q.after,
                q.wait.min(MAX_WAIT_SECONDS),
                q.status_only
            ))
            .await?;
        mirror(&s, peer, id, &page.session)?;
        return Ok(Json(page));
    }
    Ok(Json(
        wait_events(&s, id, q.after, q.wait, q.status_only).await?,
    ))
}
/// Keep the local copy of a session on a paired computer, refusing an answer
/// about a different session.
fn mirror(
    s: &AppState,
    peer: Uuid,
    id: Uuid,
    record: &SessionRecord,
) -> std::result::Result<(), ApiError> {
    if record.session_id != id || record.node_id != peer {
        return Err(err(
            ErrorCode::Internal,
            "the paired computer answered about a different session",
        ));
    }
    s.store.record_remote_session(peer, record)?;
    Ok(())
}
async fn remote_post<B: Serialize>(
    s: &AppState,
    peer: Uuid,
    id: Uuid,
    action: &str,
    body: &B,
) -> std::result::Result<SessionRecord, ApiError> {
    let record: SessionRecord = peer_client(s, peer)?
        .post(&format!("/hmesh/v1/sessions/{id}/{action}"), body)
        .await?;
    mirror(s, peer, id, &record)?;
    Ok(record)
}
/// Refresh copies of sessions this computer opened elsewhere that may still be
/// working, so lists and the running-children limit do not trust stale state.
async fn refresh_remote(s: &AppState) {
    let rows = s.store.remote_sessions(10_000).unwrap_or_default();
    let open: Vec<(Uuid, SessionRecord)> = rows
        .into_iter()
        .filter(|(_, r)| r.status.is_busy() || r.status == SessionStatus::PendingApproval)
        .take(50)
        .collect();
    let mut checks = tokio::task::JoinSet::new();
    for (peer, mut record) in open {
        let s = s.clone();
        checks.spawn(async move {
            let id = record.session_id;
            let path = format!("/hmesh/v1/sessions/{id}");
            let fetched = match peer_client(&s, peer) {
                Ok(client) => {
                    tokio::time::timeout(Duration::from_secs(5), client.get::<SessionRecord>(&path))
                        .await
                        .ok()
                }
                Err(_) => None,
            };
            match fetched {
                Some(Ok(fresh)) => {
                    s.sessions.clear_remote_miss(id);
                    let _ = mirror(&s, peer, id, &fresh);
                }
                // That computer no longer lets this one use the session (or
                // this one stopped trusting it): the copy is closed for good.
                Some(Err(e))
                    if matches!(
                        e.code,
                        ErrorCode::PermissionDenied | ErrorCode::Unauthenticated
                    ) =>
                {
                    record.status = SessionStatus::Closed;
                    let _ = s.store.record_remote_session(peer, &record);
                }
                None if s.store.peer(peer).is_ok_and(|p| p.status != "trusted") => {
                    record.status = SessionStatus::Closed;
                    let _ = s.store.record_remote_session(peer, &record);
                }
                _ => {
                    // The peer is away. Its session may still be running
                    // there, but this copy must not stay busy forever: after
                    // two refresh rounds close it with an explanation, so a
                    // parent waiting on it gets an answer instead of hanging.
                    if s.sessions.note_remote_miss(id) >= 2 {
                        record.status = SessionStatus::Closed;
                        record.last_error = Some(
                            "lost contact with the computer running this session; the work there may still be running"
                                .into(),
                        );
                        let _ = s.store.record_remote_session(peer, &record);
                        let _ = record_event(
                            &s,
                            id,
                            "error",
                            json!({"message":"lost contact with the computer running this session; the work there may still be running"}),
                        );
                    }
                }
            }
        });
    }
    while checks.join_next().await.is_some() {}
}
async fn owner_prompt(
    State(s): State<AppState>,
    Extension(c): Extension<ConnectionIdentity>,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
    Json(body): Json<PromptSession>,
) -> ApiResult<SessionRecord> {
    s.owner(&headers, &c)?;
    if let Some(peer) = s.store.remote_session_peer(id)? {
        return Ok(Json(remote_post(&s, peer, id, "prompt", &body).await?));
    }
    let from = json!({"node_id":s.instance.card.node_id,"label":null});
    Ok(Json(begin_turn(&s, id, &body.message, from)?))
}
async fn owner_permission(
    State(s): State<AppState>,
    Extension(c): Extension<ConnectionIdentity>,
    headers: HeaderMap,
    Path((id, key)): Path<(Uuid, String)>,
    Json(body): Json<PermissionAnswer>,
) -> ApiResult<SessionRecord> {
    s.owner(&headers, &c)?;
    if let Some(peer) = s.store.remote_session_peer(id)? {
        // Another computer only takes one-time answers, which `allow` says;
        // without `option_id` an earlier version accepts it too.
        let once = PermissionAnswer {
            allow: body.allow,
            option_id: None,
        };
        return Ok(Json(
            remote_post(&s, peer, id, &format!("permissions/{key}"), &once).await?,
        ));
    }
    Ok(Json(answer(&s, id, &key, &body, "owner")?))
}
async fn owner_cancel(
    State(s): State<AppState>,
    Extension(c): Extension<ConnectionIdentity>,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
) -> ApiResult<SessionRecord> {
    s.owner(&headers, &c)?;
    if let Some(peer) = s.store.remote_session_peer(id)? {
        return Ok(Json(remote_post(&s, peer, id, "cancel", &json!({})).await?));
    }
    Ok(Json(cancel(&s, id)?))
}
async fn owner_close(
    State(s): State<AppState>,
    Extension(c): Extension<ConnectionIdentity>,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
) -> ApiResult<SessionRecord> {
    s.owner(&headers, &c)?;
    if let Some(peer) = s.store.remote_session_peer(id)? {
        return Ok(Json(remote_post(&s, peer, id, "close", &json!({})).await?));
    }
    Ok(Json(close_session(&s, id, "closed by its owner")?))
}
async fn owner_approve(
    State(s): State<AppState>,
    Extension(c): Extension<ConnectionIdentity>,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
) -> ApiResult<SessionRecord> {
    s.owner(&headers, &c)?;
    Ok(Json(approve(&s, id)?))
}
async fn owner_reject(
    State(s): State<AppState>,
    Extension(c): Extension<ConnectionIdentity>,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
) -> ApiResult<SessionRecord> {
    s.owner(&headers, &c)?;
    let record = s.store.session(id)?;
    if record.status != SessionStatus::PendingApproval {
        return Err(err(
            ErrorCode::TaskConflict,
            "the session is not waiting for approval",
        ));
    }
    Ok(Json(close_session(&s, id, "rejected on this computer")?))
}
async fn owner_config(
    State(s): State<AppState>,
    Extension(c): Extension<ConnectionIdentity>,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
    Json(body): Json<SetSessionConfig>,
) -> ApiResult<SessionRecord> {
    s.owner(&headers, &c)?;
    if let Some(peer) = s.store.remote_session_peer(id)? {
        return Ok(Json(remote_post(&s, peer, id, "config", &body).await?));
    }
    // The person who opened a session chooses its settings; a session a
    // paired computer opened here is changed from that computer.
    if s.store.session(id)?.origin.node_id != s.instance.card.node_id {
        return Err(err(
            ErrorCode::PermissionDenied,
            "only the computer that opened this session may change its settings",
        ));
    }
    Ok(Json(set_config(&s, id, body).await?))
}

// ---- HTTP: this computer's owner, per agent ----

// Earlier CLIs strictly deserialize the service view. They keep receiving the
// original shape until they ask for per-protocol addresses and capabilities.
#[derive(Default, Deserialize)]
struct ServiceViewQuery {
    #[serde(default)]
    protocols: bool,
}

fn service_view(
    s: &AppState,
    agent: Uuid,
    protocols: bool,
) -> std::result::Result<AgentServiceView, ApiError> {
    let (card, _) = s.store.agent(agent)?;
    let stored = service::load(&s.instance.dir, agent).map_err(crate::internal)?;
    let mut view = match stored {
        Some(stored) => stored.view(agent, card.harness),
        None => service::default_view(agent, card.harness),
    };
    view.own_login = service::load_own_login(&s.instance.dir, agent).map_err(crate::internal)?;
    if !protocols {
        view.protocol = None;
        view.protocols.clear();
        view.key_variable = None;
        for preset in &mut view.presets {
            preset.addresses.clear();
        }
    }
    Ok(view)
}
/// Find out what the agent is set up with on its own: the agent's own ACP
/// report or its read-only status command, never its credential files. Only
/// this computer's owner may ask; the details stay here and paired computers
/// see only whether it is configured.
async fn owner_check_service(
    State(s): State<AppState>,
    Extension(c): Extension<ConnectionIdentity>,
    headers: HeaderMap,
    Path(agent): Path<Uuid>,
    Query(query): Query<ServiceViewQuery>,
) -> ApiResult<AgentServiceView> {
    s.owner(&headers, &c)?;
    let (_, config) = s.store.agent(agent)?;
    let login = hmesh_adapters::own_login::detect(&config, hmesh_core::now())
        .await
        .map_err(|e| ApiError(MeshError::input(e.to_string())))?;
    service::save_own_login(&s.instance.dir, agent, &login).map_err(crate::internal)?;
    let state = login.state.clone();
    s.store
        .update_agent_card(agent, |card| card.own_login = Some(state.clone()))?;
    let _ = s.store.audit(
        Some(s.instance.card.node_id),
        "agent.service.check",
        &agent.to_string(),
        &login.state,
        Uuid::new_v4(),
    );
    Ok(Json(service_view(&s, agent, query.protocols)?))
}
async fn owner_get_service(
    State(s): State<AppState>,
    Extension(c): Extension<ConnectionIdentity>,
    headers: HeaderMap,
    Path(agent): Path<Uuid>,
    Query(query): Query<ServiceViewQuery>,
) -> ApiResult<AgentServiceView> {
    s.owner(&headers, &c)?;
    Ok(Json(service_view(&s, agent, query.protocols)?))
}
/// Replace how an agent reaches its model service. Only this computer's owner
/// may do this; keys are stored here and never returned.
async fn owner_set_service(
    State(s): State<AppState>,
    Extension(c): Extension<ConnectionIdentity>,
    headers: HeaderMap,
    Path(agent): Path<Uuid>,
    Query(query): Query<ServiceViewQuery>,
    Json(body): Json<SetAgentService>,
) -> ApiResult<AgentServiceView> {
    s.owner(&headers, &c)?;
    let show_protocols = query.protocols || body.protocol.is_some() || body.key_variable.is_some();
    let (card, config) = s.store.agent(agent)?;
    let existing = service::load(&s.instance.dir, agent).map_err(crate::internal)?;
    let next = service::update(card.harness, existing.as_ref(), body)
        .map_err(|e| ApiError(MeshError::input(e.to_string())))?;
    service::save(&s.instance.dir, agent, next.as_ref()).map_err(crate::internal)?;
    let configured = next
        .as_ref()
        .is_some_and(|n| n.mode != MODE_AGENT_LOGIN || !n.env.is_empty());
    let default_model = next
        .as_ref()
        .and_then(|n| n.default_model_value(card.harness))
        .or(config.adapter.model);
    s.store.update_agent_card(agent, |card| {
        card.service_configured = configured;
        card.default_model = default_model;
    })?;
    let mode = next.as_ref().map_or(MODE_AGENT_LOGIN, |n| n.mode.as_str());
    let _ = s.store.audit(
        Some(s.instance.card.node_id),
        "agent.service",
        &agent.to_string(),
        mode,
        Uuid::new_v4(),
    );
    Ok(Json(service_view(&s, agent, show_protocols)?))
}
/// Check whether (possibly unsaved) settings reach the model service. Only
/// this computer's owner may do this: it sends the key to the address given.
async fn owner_test_service(
    State(s): State<AppState>,
    Extension(c): Extension<ConnectionIdentity>,
    headers: HeaderMap,
    Path(agent): Path<Uuid>,
    Json(body): Json<TestAgentService>,
) -> ApiResult<ServiceTestResult> {
    s.owner(&headers, &c)?;
    let (card, _) = s.store.agent(agent)?;
    let existing = service::load(&s.instance.dir, agent).map_err(crate::internal)?;
    let mode = body.mode.clone();
    let target = service::test_target(card.harness, existing.as_ref(), body)
        .map_err(|e| ApiError(MeshError::input(e.to_string())))?;
    let result = crate::service_test::run(target).await;
    let _ = s.store.audit(
        Some(s.instance.card.node_id),
        "agent.service.test",
        &agent.to_string(),
        &format!("{mode} {}", if result.ok { "ok" } else { "failed" }),
        Uuid::new_v4(),
    );
    Ok(Json(result))
}

// ---- HTTP: paired peers ----

/// The peer that opened a session may drive it; nobody else may.
fn peer_session(
    s: &AppState,
    actor: Actor,
    id: Uuid,
) -> std::result::Result<SessionRecord, ApiError> {
    let record = s.store.session(id)?;
    if actor.owner || record.origin.node_id == actor.node_id {
        Ok(record)
    } else {
        Err(err(
            ErrorCode::PermissionDenied,
            "only the computer that opened this session may use it",
        ))
    }
}
async fn peer_open(
    State(s): State<AppState>,
    Extension(c): Extension<ConnectionIdentity>,
    headers: HeaderMap,
    Json(body): Json<PeerOpenSession>,
) -> ApiResult<SessionRecord> {
    let actor = s.actor(&headers, &c)?;
    let decision = s.task_decision(actor);
    if decision == Decision::Denied {
        return Err(err(
            ErrorCode::PermissionDenied,
            "sender is not a currently paired peer",
        ));
    }
    let needs_approval = decision == Decision::ApprovalRequired;
    let approver = match (body.kind, needs_approval) {
        (OriginKind::Human, _) => Approver::Origin,
        (OriginKind::Agent, false) => Approver::Auto,
        (OriginKind::Agent, true) => Approver::Local,
    };
    let origin = SessionOrigin {
        node_id: actor.node_id,
        kind: body.kind,
        label: body.label,
        chain: body.chain,
        root_node_id: actor.node_id,
    };
    let place = projects::place(&s, body.agent_id, body.project_id.as_deref(), actor.owner)?;
    Ok(Json(
        open_here(
            &s,
            body.session_id,
            body.agent_id,
            origin,
            approver,
            needs_approval,
            body.message,
            body.config,
            place,
        )?
        .for_peer(),
    ))
}
async fn peer_get(
    State(s): State<AppState>,
    Extension(c): Extension<ConnectionIdentity>,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
) -> ApiResult<SessionRecord> {
    let actor = s.actor(&headers, &c)?;
    Ok(Json(peer_session(&s, actor, id)?.for_peer()))
}
async fn peer_events(
    State(s): State<AppState>,
    Extension(c): Extension<ConnectionIdentity>,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
    Query(q): Query<EventQuery>,
) -> ApiResult<SessionEventPage> {
    let actor = s.actor(&headers, &c)?;
    peer_session(&s, actor, id)?;
    let mut page = wait_events(&s, id, q.after, q.wait, q.status_only).await?;
    page.session = page.session.for_peer();
    Ok(Json(page))
}
async fn peer_prompt(
    State(s): State<AppState>,
    Extension(c): Extension<ConnectionIdentity>,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
    Json(body): Json<PromptSession>,
) -> ApiResult<SessionRecord> {
    let actor = s.actor(&headers, &c)?;
    let record = peer_session(&s, actor, id)?;
    let from = json!({"node_id":actor.node_id,"label":record.origin.label});
    Ok(Json(begin_turn(&s, id, &body.message, from)?.for_peer()))
}
async fn peer_permission(
    State(s): State<AppState>,
    Extension(c): Extension<ConnectionIdentity>,
    headers: HeaderMap,
    Path((id, key)): Path<(Uuid, String)>,
    Json(body): Json<PermissionAnswer>,
) -> ApiResult<SessionRecord> {
    let actor = s.actor(&headers, &c)?;
    let record = peer_session(&s, actor, id)?;
    if record.approver != Approver::Origin {
        return Err(err(
            ErrorCode::PermissionDenied,
            "permission requests in this session are answered on the agent's computer",
        ));
    }
    Ok(Json(answer(&s, id, &key, &body, "origin")?.for_peer()))
}
async fn peer_cancel(
    State(s): State<AppState>,
    Extension(c): Extension<ConnectionIdentity>,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
) -> ApiResult<SessionRecord> {
    let actor = s.actor(&headers, &c)?;
    peer_session(&s, actor, id)?;
    Ok(Json(cancel(&s, id)?.for_peer()))
}
async fn peer_close(
    State(s): State<AppState>,
    Extension(c): Extension<ConnectionIdentity>,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
) -> ApiResult<SessionRecord> {
    let actor = s.actor(&headers, &c)?;
    peer_session(&s, actor, id)?;
    Ok(Json(
        close_session(&s, id, "closed by the computer that opened it")?.for_peer(),
    ))
}
async fn peer_config(
    State(s): State<AppState>,
    Extension(c): Extension<ConnectionIdentity>,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
    Json(body): Json<SetSessionConfig>,
) -> ApiResult<SessionRecord> {
    let actor = s.actor(&headers, &c)?;
    let record = peer_session(&s, actor, id)?;
    // The owner here follows the same rule as on the owner API.
    if actor.owner && record.origin.node_id != s.instance.card.node_id {
        return Err(err(
            ErrorCode::PermissionDenied,
            "only the computer that opened this session may change its settings",
        ));
    }
    Ok(Json(set_config(&s, id, body).await?.for_peer()))
}

pub fn routes() -> Router<AppState> {
    Router::new()
        .merge(projects::routes())
        .merge(commands::routes())
        .route("/local/sessions", get(list).post(owner_open))
        .route("/local/sessions/{id}", get(owner_get))
        .route("/local/sessions/{id}/events", get(owner_events))
        .route("/local/sessions/{id}/delegations", post(owner_delegation))
        .route("/local/sessions/{id}/prompt", post(owner_prompt))
        .route(
            "/local/sessions/{id}/permissions/{key}",
            post(owner_permission),
        )
        .route("/local/sessions/{id}/cancel", post(owner_cancel))
        .route("/local/sessions/{id}/close", post(owner_close))
        .route("/local/sessions/{id}/approve", post(owner_approve))
        .route("/local/sessions/{id}/reject", post(owner_reject))
        .route("/local/sessions/{id}/config", post(owner_config))
        .route(
            "/local/agents/{id}/service",
            get(owner_get_service).post(owner_set_service),
        )
        .route("/local/agents/{id}/service/test", post(owner_test_service))
        .route(
            "/local/agents/{id}/service/check",
            post(owner_check_service),
        )
        .route("/hmesh/v1/sessions", post(peer_open))
        .route("/hmesh/v1/sessions/{id}", get(peer_get))
        .route("/hmesh/v1/sessions/{id}/events", get(peer_events))
        .route("/hmesh/v1/sessions/{id}/prompt", post(peer_prompt))
        .route(
            "/hmesh/v1/sessions/{id}/permissions/{key}",
            post(peer_permission),
        )
        .route("/hmesh/v1/sessions/{id}/cancel", post(peer_cancel))
        .route("/hmesh/v1/sessions/{id}/close", post(peer_close))
        .route("/hmesh/v1/sessions/{id}/config", post(peer_config))
}

#[cfg(test)]
mod tests {
    use super::{chosen_outcome, is_retry_notice, is_startup_banner};
    use hmesh_core::ErrorCode;
    use serde_json::{Value, json};

    fn options(kinds: &[&str]) -> Vec<Value> {
        kinds
            .iter()
            .map(|k| json!({"optionId":format!("id-{k}"),"name":k,"kind":k}))
            .collect()
    }
    fn selected(outcome: &Value) -> Option<&str> {
        outcome.pointer("/outcome/optionId").and_then(Value::as_str)
    }

    #[test]
    fn this_computer_chooses_any_offered_option() {
        let all = options(&["allow_once", "allow_always", "reject_once", "reject_always"]);
        let (outcome, allow, option) =
            chosen_outcome(&all, false, Some("id-allow_always"), false).unwrap();
        assert_eq!(selected(&outcome), Some("id-allow_always"));
        assert!(allow, "the option's kind decides");
        assert_eq!(option.unwrap()["kind"], "allow_always");
        let (outcome, allow, _) =
            chosen_outcome(&all, true, Some("id-reject_always"), false).unwrap();
        assert_eq!(selected(&outcome), Some("id-reject_always"));
        assert!(!allow);
        let err = chosen_outcome(&all, true, Some("made-up"), false).unwrap_err();
        assert_eq!(err.code, ErrorCode::InvalidInput);
        // Without an option, `allow` keeps meaning what it did.
        let (outcome, _, option) = chosen_outcome(&all, true, None, false).unwrap();
        assert_eq!(selected(&outcome), Some("id-allow_once"));
        assert!(option.is_none());
    }

    #[test]
    fn another_computer_only_answers_once() {
        let all = options(&["allow_once", "allow_always", "reject_once", "reject_always"]);
        for lasting in ["id-allow_always", "id-reject_always"] {
            let err = chosen_outcome(&all, true, Some(lasting), true).unwrap_err();
            assert_eq!(err.code, ErrorCode::PermissionDenied, "{lasting}");
        }
        let (outcome, _, _) = chosen_outcome(&all, true, Some("id-allow_once"), true).unwrap();
        assert_eq!(selected(&outcome), Some("id-allow_once"));
        // Only lasting options offered: allowing is refused, rejecting cancels.
        let lasting = options(&["allow_always", "reject_always"]);
        let err = chosen_outcome(&lasting, true, None, true).unwrap_err();
        assert_eq!(err.code, ErrorCode::PermissionDenied);
        let (outcome, allow, _) = chosen_outcome(&lasting, false, None, true).unwrap();
        assert_eq!(outcome["outcome"]["outcome"], "cancelled");
        assert!(!allow);
        // The same request on the agent's computer still falls back as before.
        let (outcome, _, _) = chosen_outcome(&lasting, true, None, false).unwrap();
        assert_eq!(selected(&outcome), Some("id-allow_always"));
    }

    #[test]
    fn adapter_banners_are_not_conversation() {
        let banner = "pi v0.83.0\n---\n\n## Skills\n- /skills/a/SKILL.md\n\n## Prompts\n- /clip\n";
        assert!(is_startup_banner(banner, Some(banner)));
        assert!(is_startup_banner(banner, None));
        assert!(is_startup_banner(
            "pi v0.83.0\n---\n\n## Extensions\n- npm:pi-lens",
            None
        ));
        let answer = "pi v0.83.0 是一个版本号，答案是 4。";
        assert!(!is_startup_banner(answer, None));
        assert!(!is_startup_banner("好的", None));
    }

    #[test]
    fn provider_retries_are_not_answers() {
        assert!(is_retry_notice("Retrying (attempt 1/3, waiting 2s)..."));
        assert!(is_retry_notice("Retry finished, resuming."));
        assert!(is_retry_notice(
            "New version available: v1.6.1 (installed v0.83.0). Run: `npm i -g @earendil-works/pi-coding-agent`"
        ));
        assert!(!is_retry_notice("Retry finished, resuming. 好的"));
        assert!(!is_retry_notice("答案是 4"));
    }
}
