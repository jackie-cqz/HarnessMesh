//! A long-lived ACP connection to one agent process, for interactive sessions.
//! Requests are multiplexed by ID; `session/update` notifications and
//! `session/request_permission` requests arrive on a channel.
use crate::process_tree::ProcessTree;
use hmesh_adapters::acp::{self, Incoming};
use hmesh_adapters::{JsonlDecoder, framing::MAX_ACP_FRAME_BYTES};
use hmesh_core::{ErrorCode, MeshError, Result};
use serde_json::Value;
use std::{
    collections::HashMap,
    process::Stdio,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    time::Duration,
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    process::{Child, Command},
    sync::{mpsc, oneshot},
};

/// What the agent sends without being asked.
#[derive(Debug)]
pub enum AgentMessage {
    /// `session/update` params.
    Update(Value),
    /// `session/request_permission`: the JSON-RPC ID to answer and its params.
    Permission { id: Value, params: Value },
    /// The process closed its output.
    Exited,
    /// Queued behind everything received so far; answered by the consumer.
    Barrier(oneshot::Sender<()>),
}

type Pending = Arc<Mutex<HashMap<u64, oneshot::Sender<std::result::Result<Value, String>>>>>;

pub struct AcpConnection {
    writer: mpsc::UnboundedSender<Vec<u8>>,
    events: mpsc::UnboundedSender<AgentMessage>,
    pending: Pending,
    next_id: AtomicU64,
    alive: Arc<AtomicBool>,
    child: tokio::sync::Mutex<Child>,
    tree: Mutex<Option<ProcessTree>>,
    /// The agent's `initialize` result.
    pub initialize: Value,
}

fn failure(message: impl Into<String>) -> MeshError {
    MeshError::new(ErrorCode::AgentUnavailable, message)
}
const SETUP_TIMEOUT: Duration = Duration::from_secs(60);

impl AcpConnection {
    /// Spawn the agent, run `initialize`, and start routing its messages.
    pub async fn start(
        mut command: Command,
    ) -> Result<(Arc<Self>, mpsc::UnboundedReceiver<AgentMessage>)> {
        command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .kill_on_drop(true);
        ProcessTree::configure(&mut command);
        let mut child = command
            .spawn()
            .map_err(|e| failure(format!("the agent failed to start: {e}")))?;
        let tree = ProcessTree::attach(&child).ok();
        let mut stdin = child
            .stdin
            .take()
            .ok_or_else(|| failure("missing agent stdin"))?;
        let mut stdout = child
            .stdout
            .take()
            .ok_or_else(|| failure("missing agent stdout"))?;
        let (writer, mut outbound) = mpsc::unbounded_channel::<Vec<u8>>();
        tokio::spawn(async move {
            while let Some(line) = outbound.recv().await {
                if stdin.write_all(&line).await.is_err() || stdin.flush().await.is_err() {
                    break;
                }
            }
        });
        let pending: Pending = Arc::new(Mutex::new(HashMap::new()));
        let alive = Arc::new(AtomicBool::new(true));
        let (events, receiver) = mpsc::unbounded_channel();
        let own_events = events.clone();
        {
            let pending = pending.clone();
            let alive = alive.clone();
            let writer = writer.clone();
            tokio::spawn(async move {
                let mut decoder = JsonlDecoder::new(MAX_ACP_FRAME_BYTES);
                let mut buffer = vec![0u8; 16384];
                'read: loop {
                    let read = match stdout.read(&mut buffer).await {
                        Ok(0) | Err(_) => break,
                        Ok(n) => n,
                    };
                    let Ok(frames) = decoder.push(&buffer[..read]) else {
                        break;
                    };
                    for frame in frames {
                        match acp::classify(&frame) {
                            Some(Incoming::Response { id, result }) => {
                                let waiter = pending.lock().ok().and_then(|mut p| p.remove(&id));
                                if let Some(waiter) = waiter {
                                    let _ = waiter.send(result);
                                }
                            }
                            Some(Incoming::Notification { method, params }) => {
                                if method == "session/update"
                                    && events.send(AgentMessage::Update(params)).is_err()
                                {
                                    break 'read;
                                }
                            }
                            Some(Incoming::Request { id, method, params }) => {
                                if method == "session/request_permission" {
                                    if events
                                        .send(AgentMessage::Permission { id, params })
                                        .is_err()
                                    {
                                        break 'read;
                                    }
                                } else {
                                    let mut line = acp::method_not_found(&id, &method)
                                        .to_string()
                                        .into_bytes();
                                    line.push(b'\n');
                                    let _ = writer.send(line);
                                }
                            }
                            None => {}
                        }
                    }
                }
                alive.store(false, Ordering::SeqCst);
                if let Ok(mut waiting) = pending.lock() {
                    for (_, waiter) in waiting.drain() {
                        let _ = waiter.send(Err("the agent process exited".into()));
                    }
                }
                let _ = events.send(AgentMessage::Exited);
            });
        }
        let mut connection = Self {
            writer,
            events: own_events,
            pending,
            next_id: AtomicU64::new(1),
            alive,
            child: tokio::sync::Mutex::new(child),
            tree: Mutex::new(tree),
            initialize: Value::Null,
        };
        let id = connection.next_id.fetch_add(1, Ordering::SeqCst);
        let result = connection
            .call(id, acp::initialize_request(id), SETUP_TIMEOUT)
            .await;
        let initialize = match result {
            Ok(value) => value,
            Err(error) => {
                connection.shutdown().await;
                return Err(error);
            }
        };
        if initialize.get("protocolVersion").and_then(Value::as_u64) != Some(acp::PROTOCOL_VERSION)
        {
            connection.shutdown().await;
            return Err(MeshError::new(
                ErrorCode::AdapterNotSupported,
                "the agent does not speak ACP protocol version 1",
            ));
        }
        connection.initialize = initialize;
        Ok((Arc::new(connection), receiver))
    }
    /// Wait until the consumer has handled every message received so far.
    /// Responses overtake the notifications queued before them, so call this
    /// after a request returns and before reading state its updates build.
    pub async fn barrier(&self) {
        let (done, wait) = oneshot::channel();
        if self.events.send(AgentMessage::Barrier(done)).is_ok() {
            let _ = tokio::time::timeout(Duration::from_secs(10), wait).await;
        }
    }
    pub fn alive(&self) -> bool {
        self.alive.load(Ordering::SeqCst)
    }
    pub fn supports_load(&self) -> bool {
        self.initialize
            .pointer("/agentCapabilities/loadSession")
            .and_then(Value::as_bool)
            == Some(true)
    }
    fn send(&self, frame: &Value) -> Result<()> {
        let mut line = frame.to_string().into_bytes();
        line.push(b'\n');
        self.writer
            .send(line)
            .map_err(|_| failure("the agent process is not running"))
    }
    async fn call(&self, id: u64, frame: Value, timeout: Duration) -> Result<Value> {
        let (sender, receiver) = oneshot::channel();
        self.pending
            .lock()
            .map_err(|_| failure("connection state poisoned"))?
            .insert(id, sender);
        self.send(&frame)?;
        let answer = if timeout.is_zero() {
            receiver
                .await
                .map_err(|_| failure("the agent process exited"))
        } else {
            match tokio::time::timeout(timeout, receiver).await {
                Ok(answer) => answer.map_err(|_| failure("the agent process exited")),
                Err(_) => {
                    if let Ok(mut pending) = self.pending.lock() {
                        pending.remove(&id);
                    }
                    Err(failure(format!(
                        "the agent did not answer {} within {} s",
                        frame["method"].as_str().unwrap_or("a request"),
                        timeout.as_secs()
                    )))
                }
            }
        }?;
        answer.map_err(|message| {
            failure(format!(
                "the agent rejected {}: {message}",
                frame["method"].as_str().unwrap_or("a request")
            ))
        })
    }
    fn id(&self) -> u64 {
        self.next_id.fetch_add(1, Ordering::SeqCst)
    }
    pub async fn new_session(&self, cwd: &std::path::Path, mcp: &[Value]) -> Result<String> {
        let id = self.id();
        let result = self
            .call(id, acp::new_session_request(id, cwd, mcp), SETUP_TIMEOUT)
            .await?;
        result
            .get("sessionId")
            .and_then(Value::as_str)
            .map(str::to_owned)
            .ok_or_else(|| failure("the agent created a session without an ID"))
    }
    pub async fn load_session(
        &self,
        session: &str,
        cwd: &std::path::Path,
        mcp: &[Value],
    ) -> Result<()> {
        let id = self.id();
        self.call(
            id,
            acp::load_session_request(id, session, cwd, mcp),
            SETUP_TIMEOUT,
        )
        .await
        .map(|_| ())
    }
    /// Send one prompt and wait for the turn to end (no time limit: turns can
    /// be long; cancel with `cancel`).
    pub async fn prompt(&self, session: &str, text: &str) -> Result<Value> {
        let id = self.id();
        self.call(id, acp::prompt_request(id, session, text), Duration::ZERO)
            .await
    }
    pub fn cancel(&self, session: &str) -> Result<()> {
        self.send(&acp::cancel_notification(session))
    }
    pub fn respond(&self, id: &Value, result: Value) -> Result<()> {
        self.send(&acp::result_response(id, result))
    }
    /// Stop the agent and everything it started.
    pub async fn shutdown(&self) {
        if let Ok(tree) = self.tree.lock()
            && let Some(tree) = tree.as_ref()
        {
            let _ = tree.terminate();
        }
        let mut child = self.child.lock().await;
        let _ = child.start_kill();
        let _ = tokio::time::timeout(Duration::from_secs(3), child.wait()).await;
        self.alive.store(false, Ordering::SeqCst);
    }
}
