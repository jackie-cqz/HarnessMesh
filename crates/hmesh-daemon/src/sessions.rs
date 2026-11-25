//! Interactive ACP sessions: one agent process per open session, events stored
//! in order and streamed by long-polling. Sessions on a paired node's agent are
//! opened and driven through that node's peer API.
use crate::{Actor, ApiError, ApiResult, AppState, internal};
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

/// Longest single long-poll wait; clients loop.
pub const MAX_WAIT_SECONDS: u64 = 10;
/// Agent processes of idle sessions are stopped after this long; the next
/// prompt starts them again and reloads the conversation when supported.
const IDLE_PROCESS_SECONDS: u64 = 600;
const MAX_PAGE_EVENTS: usize = 500;
const MAX_FINAL_TEXT: usize = 32 * 1024;

struct Live {
    conn: Arc<AcpConnection>,
    agent_session: Mutex<String>,
    secrets: Vec<String>,
    permissions: Mutex<HashMap<String, (Value, Vec<Value>)>>,
    reply: Mutex<Reply>,
    replaying: std::sync::atomic::AtomicBool,
    last_active: Mutex<Instant>,
    /// The model service settings the process was started with.
    service_digest: String,
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
    refreshing: std::sync::atomic::AtomicBool,
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
    fn notify(&self, id: Uuid, seq: u64) {
        if let Ok(watchers) = self.watchers.lock()
            && let Some(sender) = watchers.get(&id)
        {
            let _ = sender.send(seq);
        }
    }
}

fn redact(secrets: &[String], text: &str) -> String {
    let mut text = text.to_owned();
    for secret in secrets {
        if secret.len() >= 8 {
            text = text.replace(secret, "[REDACTED]");
        }
    }
    text
}
fn clipped(secrets: &[String], text: &str, limit: usize) -> Value {
    let (text, truncated) = clip(&redact(secrets, text), limit);
    if truncated {
        json!(format!("{text} [truncated]"))
    } else {
        json!(text)
    }
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

/// Normalize one `session/update` into a bounded recorded event.
fn normalize(update: &Value, secrets: &[String]) -> Option<(&'static str, Value)> {
    fn text(v: &Value) -> Option<&str> {
        v.pointer("/content/text").and_then(Value::as_str)
    }
    match update.get("sessionUpdate").and_then(Value::as_str)? {
        "agent_message_chunk" => Some((
            "agent_message_chunk",
            json!({"text":clipped(secrets, text(update)?, MAX_EVENT_TEXT_BYTES)}),
        )),
        "agent_thought_chunk" => Some((
            "agent_thought_chunk",
            json!({"text":clipped(secrets, text(update)?, MAX_EVENT_TEXT_BYTES)}),
        )),
        kind @ ("tool_call" | "tool_call_update") => {
            let output: Vec<String> = update
                .get("content")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .filter_map(|c| c.pointer("/content/text").and_then(Value::as_str))
                .map(str::to_owned)
                .collect();
            let input = update
                .get("rawInput")
                .map(|v| v.to_string())
                .filter(|s| s != "null");
            Some((
                if kind == "tool_call" {
                    "tool_call"
                } else {
                    "tool_call_update"
                },
                json!({
                    "id":update.get("toolCallId"),
                    "title":update.get("title").and_then(Value::as_str).map(|t| clipped(secrets, t, 512)),
                    "kind":update.get("kind"),
                    "status":update.get("status"),
                    "input":input.map(|i| clipped(secrets, &i, 2048)),
                    "output":(!output.is_empty()).then(|| clipped(secrets, &output.join("\n"), 2048)),
                }),
            ))
        }
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

async fn pump(
    s: AppState,
    id: Uuid,
    live: Arc<Live>,
    mut messages: tokio::sync::mpsc::UnboundedReceiver<AgentMessage>,
) {
    while let Some(message) = messages.recv().await {
        match message {
            AgentMessage::Update(params) => {
                if live.replaying.load(std::sync::atomic::Ordering::SeqCst) {
                    continue;
                }
                let update = &params["update"];
                // The agent changed a setting itself (or reports new ones).
                if update.get("sessionUpdate").and_then(Value::as_str)
                    == Some("config_option_update")
                {
                    let options = ConfigOption::parse_all(&update["configOptions"]);
                    if !options.is_empty() {
                        let _ = update_record(&s, id, |record| record.config_options = options);
                    }
                    continue;
                }
                if let Some(text) = acp::message_text(update)
                    && let Ok(mut reply) = live.reply.lock()
                {
                    reply.push(text);
                }
                if matches!(
                    update.get("sessionUpdate").and_then(Value::as_str),
                    Some("tool_call")
                ) && let Ok(mut reply) = live.reply.lock()
                {
                    reply.tool();
                }
                if let Some((kind, payload)) = normalize(update, &live.secrets) {
                    let _ = record_event(&s, id, kind, payload);
                }
            }
            AgentMessage::Permission { id: rpc, params } => {
                // Unique per session even when the agent process restarts.
                let key = Uuid::new_v4().simple().to_string()[..12].to_owned();
                let call = &params["toolCall"];
                let options: Vec<Value> = params["options"].as_array().cloned().unwrap_or_default();
                let shown: Vec<Value> = options
                    .iter()
                    .map(|o| json!({"option_id":o.get("optionId"),"name":o.get("name"),"kind":o.get("kind")}))
                    .collect();
                let request = json!({
                    "key":key,
                    "title":call.get("title").and_then(Value::as_str).map(|t| clipped(&live.secrets, t, 512)),
                    "kind":call.get("kind"),
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
            AgentMessage::Barrier(done) => {
                let _ = done.send(());
            }
            AgentMessage::Exited => break,
        }
    }
    // The process is gone; a later prompt starts a new one.
    if let Ok(mut live_map) = s.sessions.live.lock()
        && live_map.get(&id).is_some_and(|l| Arc::ptr_eq(l, &live))
    {
        live_map.remove(&id);
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
    live.conn.shutdown().await;
}

/// Start (or reuse) the agent process for a session and open or reload the
/// agent-side session, then apply the session's settings.
async fn ensure_live(s: &AppState, record: &SessionRecord) -> hmesh_core::Result<Arc<Live>> {
    let (card, config) = s.store.agent(record.agent_id)?;
    let service = service::load(&s.instance.dir, record.agent_id)
        .map_err(|e| MeshError::new(ErrorCode::Internal, e.to_string()))?;
    let digest = service.as_ref().map(|v| v.digest()).unwrap_or_default();
    if let Some(live) = s.sessions.live(record.session_id)
        && live.conn.alive()
    {
        if live.service_digest == digest {
            return Ok(live);
        }
        // The owner changed how this agent reaches its model service: start
        // it again so it sees the new settings (the conversation reloads).
        discard(s, record.session_id, &live).await;
    }
    let mut command = hmesh_runtime::agent_command(&config)
        .map_err(|e| MeshError::new(ErrorCode::AgentUnavailable, e.to_string()))?;
    let mut secrets = hmesh_runtime::run_secrets(&config);
    if let Some(service) = &service {
        let env = service
            .launch_env(card.harness)
            .map_err(|e| MeshError::new(ErrorCode::AgentUnavailable, e.to_string()))?;
        hmesh_runtime::apply_env(&mut command, &env);
        secrets.extend(service.secrets());
        secrets.sort();
        secrets.dedup();
    }
    // The owner's explicit default is reported when the agent refuses it; the
    // older informational `adapter.model` applies only when the agent offers it.
    let default_model = match service.as_ref().and_then(|v| v.model_setting(card.harness)) {
        Some(model) => Some((model, true)),
        None => config.adapter.model.clone().map(|model| (model, false)),
    };
    let (conn, messages) = AcpConnection::start(command).await?;
    let cwd = config.project.root.clone();
    let mcp = mcp_server(s, record.session_id);
    let reload = record
        .agent_session_id
        .clone()
        .filter(|_| conn.supports_load());
    let live = Arc::new(Live {
        conn: conn.clone(),
        agent_session: Mutex::new(String::new()),
        secrets,
        permissions: Mutex::new(HashMap::new()),
        reply: Mutex::new(Reply::default()),
        // Updates the agent replays while reloading are not new events.
        replaying: std::sync::atomic::AtomicBool::new(reload.is_some()),
        last_active: Mutex::new(Instant::now()),
        service_digest: digest,
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
    const LOST: &str =
        "the agent could not reload the earlier conversation; it continues without that context";
    const NO_RELOAD: &str =
        "this agent cannot reload conversations; it continues without the earlier context";
    let started = async {
        match reload {
            Some(previous) => {
                let loaded = conn.load_session(&previous, &cwd, &mcp).await;
                // The replayed history is queued ahead of the barrier.
                conn.barrier().await;
                live.replaying
                    .store(false, std::sync::atomic::Ordering::SeqCst);
                match loaded {
                    Ok(result) => Ok((previous, result, false, Some(RELOADED))),
                    Err(_) => conn
                        .new_session(&cwd, &mcp)
                        .await
                        .map(|(id, result)| (id, result, true, Some(LOST))),
                }
            }
            None => {
                let note = record.agent_session_id.as_ref().map(|_| NO_RELOAD);
                conn.new_session(&cwd, &mcp)
                    .await
                    .map(|(id, result)| (id, result, true, note))
            }
        }
    }
    .await;
    let (agent_session, result, fresh, note) = match started {
        Ok(started) => started,
        Err(error) => {
            discard(s, record.session_id, &live).await;
            return Err(error);
        }
    };
    if let Ok(mut current) = live.agent_session.lock() {
        *current = agent_session.clone();
    }
    let reported = ConfigOption::parse_all(&result["configOptions"]);
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
    let current = update_record(s, record.session_id, |current| {
        current.agent_session_id = Some(agent_session);
        if !options.is_empty() {
            current.config_options = options;
        }
        current.config_values.retain(|id, _| !rejected.contains(id));
    })?;
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
    if let Some((model, explicit)) = default_model
        && let Some(option) = options.iter().find(|o| o.is_category("model"))
        && !wanted.contains_key(&option.id)
        && (explicit || option.accepts(&Value::String(model.clone())))
    {
        wanted.insert(option.id.clone(), Value::String(model));
    }
    let mut rejected = vec![];
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
                    let parsed = ConfigOption::parse_all(&reported);
                    if parsed.is_empty() {
                        if let Some(option) = options.iter_mut().find(|o| o.id == config_id) {
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
    record_event(s, id, "user_message", json!({"text":shown,"from":from}))?;
    let state = s.clone();
    let text = text.to_owned();
    tokio::spawn(async move { run_turn(state, id, text).await });
    record.updated_at = now();
    Ok(record)
}

async fn run_turn(s: AppState, id: Uuid, text: String) {
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
    let live = match ensure_live(&s, &record).await {
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
    let agent_session = live
        .agent_session
        .lock()
        .map(|a| a.clone())
        .unwrap_or_default();
    let result = live.conn.prompt(&agent_session, &text).await;
    // The turn's last chunks may still be queued behind the response.
    live.conn.barrier().await;
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
            finish(&s, Some(&stop), final_text, None)
        }
        Err(error) => finish(&s, None, None, Some(redact(&live.secrets, &error.message))),
    }
}

fn answer(
    s: &AppState,
    id: Uuid,
    key: &str,
    allow: bool,
    by: &str,
) -> hmesh_core::Result<SessionRecord> {
    let live = s.sessions.live(id).ok_or_else(|| {
        MeshError::new(
            ErrorCode::TaskConflict,
            "the agent for this session is not running",
        )
    })?;
    let (rpc, options, left) = {
        let mut pending = live
            .permissions
            .lock()
            .map_err(|_| MeshError::new(ErrorCode::Internal, "permission map poisoned"))?;
        let (rpc, options) = pending.remove(key).ok_or_else(|| {
            MeshError::new(
                ErrorCode::TaskConflict,
                "no pending permission request with this key",
            )
        })?;
        (rpc, options, pending.len())
    };
    live.conn
        .respond(&rpc, acp::permission_outcome(&options, allow))?;
    record_event(
        s,
        id,
        "permission_resolved",
        json!({"key":key,"allow":allow,"by":by}),
    )?;
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
            record_event(
                s,
                id,
                "status",
                json!({"status":"cancelling","message":"cancel requested"}),
            )?;
        }
    }
    s.store.session(id)
}

fn close_session(s: &AppState, id: Uuid, why: &str) -> hmesh_core::Result<SessionRecord> {
    // Mark it closed before taking its process: an agent that is still
    // starting sees the closed status once it is registered (see ensure_live).
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
                "{} is not one of the choices for {}",
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
        let parsed = ConfigOption::parse_all(&reported);
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
                    "{value} is not one of the choices for {}",
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
        project_id: config_file.agent.project_id,
        origin,
        approver,
        status: if needs_approval {
            SessionStatus::PendingApproval
        } else {
            SessionStatus::Idle
        },
        title: String::new(),
        turns: 0,
        agent_session_id: None,
        last_stop_reason: None,
        last_final_text: None,
        last_error: None,
        pending_message: None,
        config_options: options,
        config_values: config,
        created_at: at,
        updated_at: at,
    };
    if needs_approval {
        record.pending_message = message.clone();
        if let Some(message) = &message {
            record.title = clip(message.lines().next().unwrap_or("").trim(), 120).0;
        }
    }
    s.store.create_session(&record)?;
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
    match message {
        Some(message) if !needs_approval => {
            let from = json!({"node_id":record.origin.node_id,"label":record.origin.label});
            begin_turn(s, session_id, &message, from)
        }
        _ => Ok(record),
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
    Client::new(
        &s.instance,
        &peer.card.endpoint,
        &peer.card.fingerprint,
        false,
        false,
    )
    .map_err(internal)
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
    if target.is_some() && root != me {
        // Pairing is between two computers; a chain another computer started
        // must not reach further computers with this computer's identity.
        return Err(err(
            ErrorCode::PermissionDenied,
            "a session opened from another computer can only call agents on this computer",
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
            Ok(open_here(
                s,
                session_id,
                body.agent_id,
                origin,
                approver,
                needs_approval,
                body.message,
                body.config,
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
        let events = if status_only {
            vec![]
        } else {
            s.store.session_events(id, after, MAX_PAGE_EVENTS)?
        };
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
async fn owner_get(
    State(s): State<AppState>,
    Extension(c): Extension<ConnectionIdentity>,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
) -> ApiResult<SessionRecord> {
    s.owner(&headers, &c)?;
    if let Some(peer) = s.store.remote_session_peer(id)? {
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
                _ => {}
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
        return Ok(Json(
            remote_post(&s, peer, id, &format!("permissions/{key}"), &body).await?,
        ));
    }
    Ok(Json(answer(&s, id, &key, body.allow, "owner")?))
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

fn service_view(s: &AppState, agent: Uuid) -> std::result::Result<AgentServiceView, ApiError> {
    let (card, _) = s.store.agent(agent)?;
    let stored = service::load(&s.instance.dir, agent).map_err(crate::internal)?;
    let mut view = match stored {
        Some(stored) => stored.view(agent, card.harness),
        None => service::default_view(agent, card.harness),
    };
    view.own_login = service::load_own_login(&s.instance.dir, agent).map_err(crate::internal)?;
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
    Ok(Json(service_view(&s, agent)?))
}
async fn owner_get_service(
    State(s): State<AppState>,
    Extension(c): Extension<ConnectionIdentity>,
    headers: HeaderMap,
    Path(agent): Path<Uuid>,
) -> ApiResult<AgentServiceView> {
    s.owner(&headers, &c)?;
    Ok(Json(service_view(&s, agent)?))
}
/// Replace how an agent reaches its model service. Only this computer's owner
/// may do this; keys are stored here and never returned.
async fn owner_set_service(
    State(s): State<AppState>,
    Extension(c): Extension<ConnectionIdentity>,
    headers: HeaderMap,
    Path(agent): Path<Uuid>,
    Json(body): Json<SetAgentService>,
) -> ApiResult<AgentServiceView> {
    s.owner(&headers, &c)?;
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
    Ok(Json(service_view(&s, agent)?))
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
    Ok(Json(open_here(
        &s,
        body.session_id,
        body.agent_id,
        origin,
        approver,
        needs_approval,
        body.message,
        body.config,
    )?))
}
async fn peer_get(
    State(s): State<AppState>,
    Extension(c): Extension<ConnectionIdentity>,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
) -> ApiResult<SessionRecord> {
    let actor = s.actor(&headers, &c)?;
    Ok(Json(peer_session(&s, actor, id)?))
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
    Ok(Json(
        wait_events(&s, id, q.after, q.wait, q.status_only).await?,
    ))
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
    Ok(Json(begin_turn(&s, id, &body.message, from)?))
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
    Ok(Json(answer(&s, id, &key, body.allow, "origin")?))
}
async fn peer_cancel(
    State(s): State<AppState>,
    Extension(c): Extension<ConnectionIdentity>,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
) -> ApiResult<SessionRecord> {
    let actor = s.actor(&headers, &c)?;
    peer_session(&s, actor, id)?;
    Ok(Json(cancel(&s, id)?))
}
async fn peer_close(
    State(s): State<AppState>,
    Extension(c): Extension<ConnectionIdentity>,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
) -> ApiResult<SessionRecord> {
    let actor = s.actor(&headers, &c)?;
    peer_session(&s, actor, id)?;
    Ok(Json(close_session(
        &s,
        id,
        "closed by the computer that opened it",
    )?))
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
    Ok(Json(set_config(&s, id, body).await?))
}

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/local/sessions", get(list).post(owner_open))
        .route("/local/sessions/{id}", get(owner_get))
        .route("/local/sessions/{id}/events", get(owner_events))
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
