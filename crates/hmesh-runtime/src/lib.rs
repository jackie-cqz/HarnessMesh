//! Persistent queue executor and bounded, individually tracked managed runs.
pub mod acp_conn;
mod prepare;
pub use prepare::inspect_references;
pub use prepare::{agent_command, run_secrets};
pub mod git_version;
pub mod process_tree;
pub mod verification;
pub mod workspace;
use hmesh_adapters::{JsonlDecoder, ObservedEvent, native_record_limit};
use hmesh_core::*;
use hmesh_store::Store;
use process_tree::ProcessTree;
use serde_json::json;
use std::{path::PathBuf, process::Stdio, sync::Arc, time::Duration};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    sync::watch,
    task::JoinSet,
};

pub type ExecutionAuthorizer = Arc<dyn Fn(&TaskRecord) -> Result<()> + Send + Sync>;
pub async fn run_queue(
    store: Store,
    data_dir: PathBuf,
    limits: RuntimeOptions,
    authorize: ExecutionAuthorizer,
    mut shutdown: watch::Receiver<bool>,
) {
    let mut workers = JoinSet::new();
    let mut tick = tokio::time::interval(Duration::from_millis(100));
    loop {
        tokio::select! {
            changed = shutdown.changed() => { if changed.is_err() || *shutdown.borrow() { break; } }
            _ = tick.tick(), if workers.len() < limits.max_concurrent_runs => {
                match store.claim_next_with_limits(&limits) {
                    Ok(Some(task)) => {
                        let store = store.clone(); let dir = data_dir.clone(); let stop = shutdown.clone(); let authorize=authorize.clone();
                        workers.spawn(async move {
                            let result=match authorize(&task) {Ok(())=>execute(&store,&dir,&task,stop, &authorize).await,Err(error)=>Err(error)};
                            if let Err(error) = result {
                                let current = store.task(task.task_id);
                                if let Ok(current) = current && !current.status.is_terminal() {
                                    let next = if current.status == TaskStatus::Cancelling || error.code==ErrorCode::TaskUnknown { TaskStatus::Unknown } else { TaskStatus::Failed };
                                    let _ = store.transition(task.task_id, next, Some(error));
                                }
                            }
                        });
                    }
                    Ok(None) => {}
                    Err(error) => tracing::error!(code=?error.code, "queue claim failed"),
                }
            }
            Some(result) = workers.join_next(), if !workers.is_empty() => {
                if result.is_err() { tracing::error!("managed-run worker panicked; recovery required"); }
            }
        }
    }
    while workers.join_next().await.is_some() {}
}

fn failure(error: impl std::fmt::Display) -> MeshError {
    MeshError::new(ErrorCode::AgentUnavailable, error.to_string())
}
fn append_text(store: &Store, task_id: uuid::Uuid, text: &str) -> Result<()> {
    let mut remaining = text;
    // JSON escaping uses at most six bytes per input byte; 8 KiB plus the
    // envelope remains below the 64 KiB normalized payload bound.
    while !remaining.is_empty() {
        let mut end = remaining.len().min(8 * 1024);
        while !remaining.is_char_boundary(end) {
            end -= 1;
        }
        store.append_event(
            task_id,
            "run",
            json!({"type":"text_delta","text":&remaining[..end]}),
        )?;
        remaining = &remaining[end..];
    }
    Ok(())
}
/// The final reply is the assistant text after the last tool call; text written
/// before a tool call is usually narration ("let me check…").
#[derive(Default)]
struct ReplyTracker {
    current: String,
    previous: String,
}
impl ReplyTracker {
    fn push(&mut self, text: &str) {
        self.current.push_str(text);
    }
    fn tool_boundary(&mut self) {
        if !self.current.trim().is_empty() {
            self.previous = std::mem::take(&mut self.current);
        } else {
            self.current.clear();
        }
    }
    fn finish(self) -> Option<(String, bool)> {
        let text = if self.current.trim().is_empty() {
            self.previous
        } else {
            self.current
        };
        let mut text = text.trim().to_owned();
        if text.is_empty() {
            return None;
        }
        // Bound the raw bytes, then the JSON-escaped size: escaping can grow text
        // up to six times and one event payload must stay under 64 KiB.
        let mut truncated = false;
        let mut limit = MAX_FINAL_TEXT_BYTES;
        loop {
            if text.len() > limit {
                let mut end = limit;
                while !text.is_char_boundary(end) {
                    end -= 1;
                }
                text.truncate(end);
                truncated = true;
            }
            let encoded = serde_json::to_string(&text).map_or(usize::MAX, |s| s.len());
            if encoded <= MAX_FINAL_TEXT_ENCODED_BYTES {
                return Some((text, truncated));
            }
            limit = text.len() * MAX_FINAL_TEXT_ENCODED_BYTES / encoded;
        }
    }
}
/// Escaped JSON size of the reply inside one event, leaving room for the envelope.
const MAX_FINAL_TEXT_ENCODED_BYTES: usize = 48 * 1024;
fn append_result(store: &Store, task_id: uuid::Uuid, reply: ReplyTracker) {
    if let Some((text, truncated)) = reply.finish()
        && let Err(error) = store.append_event(
            task_id,
            "result",
            json!({"type":"final_text","text":text,"truncated":truncated}),
        )
    {
        // The reply is a convenience copy; the run outcome must not depend on it.
        tracing::warn!(code=?error.code, "could not record final reply event");
    }
}
fn append_observation(
    store: &Store,
    task_id: uuid::Uuid,
    prepared: &mut prepare::Prepared,
    reply: &mut ReplyTracker,
    event: ObservedEvent,
) -> Result<()> {
    let value = match event {
        ObservedEvent::TextDelta { text } => {
            let text = prepared.redact_stream(&text);
            reply.push(&text);
            return append_text(store, task_id, &text);
        }
        ObservedEvent::ToolEvent { name, phase } => {
            // A tool call ends the assistant's text segment: release text the
            // redactor held back so it lands before the boundary, not after it.
            let held = prepared.flush_text();
            reply.push(&held);
            append_text(store, task_id, &held)?;
            reply.tool_boundary();
            json!({"type":"tool_event","name":prepared.redact(&name),"phase":prepared.redact(&phase)})
        }
        event => serde_json::to_value(event).map_err(failure)?,
    };
    store.append_event(task_id, "run", value)?;
    Ok(())
}
async fn execute(
    store: &Store,
    data_dir: &std::path::Path,
    task: &TaskRecord,
    mut shutdown: watch::Receiver<bool>,
    authorize: &ExecutionAuthorizer,
) -> Result<()> {
    let (_, config) = store.agent(task.agent_id)?;
    // Also check the exact configuration captured for this attempt. A later
    // allowlist edit must not make an older captured configuration look ready.
    if task
        .request
        .expected_artifacts
        .as_ref()
        .is_some_and(|paths| {
            paths
                .iter()
                .any(|path| !config.project.artifact_paths.contains(path))
        })
    {
        return Err(MeshError::new(
            ErrorCode::PermissionDenied,
            "required artifact is absent from the captured execution configuration",
        ));
    }
    let budget = store.project_budget(&task.project_id)?;
    let digest = hmesh_security::sha256_hex(&serde_json::to_vec(&config).map_err(failure)?);
    let attempt = task
        .last_attempt_id
        .ok_or_else(|| MeshError::new(ErrorCode::TaskUnknown, "execution attempt lease missing"))?;
    let dir = data_dir.join("runs").join(attempt.to_string());
    let config = workspace::prepare(store, task, &config, &dir).map_err(failure)?;
    let mut prepared = prepare::prepare(&config, task, &dir).map_err(failure)?;
    if store.task(task.task_id)?.status == TaskStatus::Cancelling {
        store.transition(task.task_id, TaskStatus::Cancelled, None)?;
        return Ok(());
    }
    if *shutdown.borrow() {
        return Err(MeshError::new(
            ErrorCode::TaskUnknown,
            "Daemon stopped during preparation; no prompt was sent",
        ));
    }
    // Workspace preparation may take time; recheck the current grant immediately before spawning.
    authorize(task)?;
    workspace::enforce_prepared(store, task, &config).map_err(failure)?;
    prepared
        .command
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    ProcessTree::configure(&mut prepared.command);
    let mut child = prepared
        .command
        .spawn()
        .map_err(|_| failure("harness failed to start"))?;
    let mut tree = match ProcessTree::attach(&child) {
        Ok(tree) => tree,
        Err(_) => {
            let _ = child.kill().await;
            return Err(MeshError::new(
                ErrorCode::TaskUnknown,
                "could not establish process-tree tracking; run aborted before prompt",
            ));
        }
    };
    store.record_pid(
        attempt,
        child.id().ok_or_else(|| failure("child pid unavailable"))?,
    )?;
    let mut input = child
        .stdin
        .take()
        .ok_or_else(|| failure("missing harness stdin"))?;
    // ACP is bidirectional: stdin stays open for requests and for answers to
    // the agent's permission requests until the turn settles.
    for frame in prepared.driver.take_outbound() {
        write_frame(&mut input, &frame).await?;
    }
    let mut input = Some(input);
    let mut stdout = child
        .stdout
        .take()
        .ok_or_else(|| failure("missing harness stdout"))?;
    let stderr = child
        .stderr
        .take()
        .ok_or_else(|| failure("missing harness stderr"))?;
    let max_output = task.request.limits.max_output_bytes;
    let mut diagnostics = tokio::spawn(async move {
        let mut data = vec![];
        let _ = stderr.take(max_output as u64).read_to_end(&mut data).await;
        data
    });
    let record_limit = native_record_limit(max_output);
    let mut decoder = JsonlDecoder::new(record_limit);
    let mut reply = ReplyTracker::default();
    let mut output_bytes = 0;
    let mut buffer = [0u8; 8192];
    let mut exited = None;
    let mut output_closed = false;
    let mut cancellation = false;
    let mut run_error = None;
    // Agents need not exit when stdin closes; stop them shortly after the turn.
    let mut settled_at: Option<tokio::time::Instant> = None;
    let timeout = budget
        .as_ref()
        .map_or(task.request.limits.timeout_seconds, |b| {
            b.max_runtime_seconds
                .min(task.request.limits.timeout_seconds)
        });
    let deadline = tokio::time::sleep(Duration::from_secs(timeout));
    tokio::pin!(deadline);
    let mut inspect = tokio::time::interval(Duration::from_millis(100));
    loop {
        if exited.is_some() && output_closed {
            break;
        }
        tokio::select! {
            biased;
            changed = shutdown.changed() => {
                if changed.is_err() || *shutdown.borrow() { cancellation = true; break; }
            }
            _ = inspect.tick() => { if let Err(error)=tree.refresh() { run_error=Some(MeshError::new(ErrorCode::TaskUnknown,format!("owned descendant tracking failed: {error}"))); break; }
                if store.task(task.task_id)?.status == TaskStatus::Cancelling { cancellation = true; break; }
                if let Err(error) = authorize(task) { run_error = Some(error); break; }
                if settled_at.is_some_and(|at| at.elapsed() >= SETTLED_EXIT_GRACE) { break; } }
            _ = &mut deadline => { run_error = Some(failure("managed run deadline exceeded")); break; }
            status = child.wait(), if exited.is_none() => { exited = Some(status.map_err(|_| failure("cannot observe harness exit"))?); }
            read = stdout.read(&mut buffer), if !output_closed => {
                let read = read.map_err(|_| failure("harness stdout failed"))?;
                if read == 0 { output_closed = true; continue; }
                output_bytes += read;
                if output_bytes > max_output { run_error = Some(MeshError::new(ErrorCode::RateLimited, "managed stdout exceeded output limit")); break; }
                match decoder.push(&buffer[..read]) {
                    Ok(records) => for record in records {
                        for event in prepared.driver.observe(&record) {
                            if let ObservedEvent::Usage{usage}=&event {
                                let mut persisted=usage.clone();persisted.configured_model=config.adapter.model.clone();persisted.configured_provider=config.adapter.provider.clone();
                                if let Err(error)=store.record_usage(task.task_id,attempt,&persisted) {run_error=Some(error);break;}
                                if budget.as_ref().and_then(|b|b.max_reported_tokens).zip(usage.total_tokens).is_some_and(|(cap,total)|total>=cap) {
                                    run_error=Some(MeshError::new(ErrorCode::RateLimited,"reported token observation cap reached; provider billing may exceed this delayed observation"));
                                    break;
                                }
                            }
                            if let Err(error) = append_observation(store, task.task_id, &mut prepared, &mut reply, event) {
                                run_error = Some(error);
                                break;
                            }
                        }
                        if run_error.is_some() { break; }
                        if let Some(stdin) = input.as_mut() {
                            for frame in prepared.driver.take_outbound() {
                                if let Err(error) = write_frame(stdin, &frame).await { run_error = Some(error); break; }
                            }
                        }
                        if run_error.is_some() { break; }
                        if prepared.driver.settled() && settled_at.is_none() {
                            input.take();
                            settled_at = Some(tokio::time::Instant::now());
                        }
                    },
                    Err(error) => { run_error = Some(error); break; }
                }
                if run_error.is_some() { break; }
            }
        }
    }
    input.take();
    // Reclaim tracked child processes even after a successful parent exit.
    let stopped = stop_tree(&mut tree, &mut child).await;
    let diagnostics = match tokio::time::timeout(Duration::from_secs(3), &mut diagnostics).await {
        Ok(Ok(data)) => data,
        _ => {
            diagnostics.abort();
            vec![]
        }
    };
    let text = prepared.flush_text();
    reply.push(&text);
    append_text(store, task.task_id, &text)?;
    store.append_event(
        task.task_id,
        "output_summary",
        json!({
            "stdout_bytes":output_bytes,
            "stdout_limit_bytes":max_output,
            "native_record_limit_bytes":record_limit,
            "largest_complete_record_bytes":decoder.largest_record_bytes(),
            "complete_records":decoder.record_count(),
        }),
    )?;
    // Recorded for every outcome so callers can read the reply without replaying events.
    append_result(store, task.task_id, reply);
    let diagnostic = prepared.redact(&String::from_utf8_lossy(&diagnostics));
    if !diagnostic.is_empty() {
        hmesh_security::atomic_write(&dir.join("stderr.log"), diagnostic.as_bytes())
            .map_err(failure)?;
    }
    if cancellation {
        let current = store.task(task.task_id)?;
        if current.status == TaskStatus::Running {
            store.transition(task.task_id, TaskStatus::Cancelling, None)?;
        }
        store.transition(
            task.task_id,
            if stopped {
                TaskStatus::Cancelled
            } else {
                TaskStatus::Unknown
            },
            if stopped {
                None
            } else {
                Some(MeshError::new(
                    ErrorCode::TaskUnknown,
                    "process-tree shutdown could not be confirmed",
                ))
            },
        )?;
        return Ok(());
    }
    if !stopped {
        store.transition(
            task.task_id,
            TaskStatus::Unknown,
            Some(MeshError::new(
                ErrorCode::TaskUnknown,
                "managed process cleanup is unproven",
            )),
        )?;
        return Ok(());
    }
    if run_error.is_none() {
        run_error = authorize(task).err();
    }
    let result = match run_error {
        Some(error) => Err(error),
        None => decoder.finish().and_then(|_| {
            prepared.driver.finish().map_err(|error| {
                // An agent that crashed mid-turn failed; one that exited
                // cleanly without answering leaves the outcome unknown.
                match exited.and_then(|s| s.code()) {
                    Some(code) if code != 0 && error.code == ErrorCode::TaskUnknown => {
                        MeshError::new(
                            ErrorCode::AgentUnavailable,
                            format!("the agent exited with code {code} before finishing the turn"),
                        )
                    }
                    _ => error,
                }
            })
        }),
    };
    let current = store.task(task.task_id)?;
    if current.status == TaskStatus::Cancelling {
        store.transition(task.task_id, TaskStatus::Cancelled, None)?;
        return Ok(());
    }
    match result {
        Ok(()) => {
            workspace::finish(store, task, &dir).map_err(failure)?;
            let entries = verification::capture(&config);
            for entry in &entries {
                if let Some(bytes) = &entry.bytes {
                    store.append_event(task.task_id,"artifact",json!({"name":entry.name,"sha256":hmesh_security::sha256_hex(bytes),"size_bytes":bytes.len(),"source":"completion-snapshot"}))?;
                } else {
                    store.append_event(task.task_id,"verification",json!({"status":"not_verified","artifact":entry.name,"reason":"unavailable_at_completion"}))?;
                }
            }
            store.complete_with_artifacts(task.task_id, attempt, &digest, &entries)?;
        }
        Err(error) => {
            store.transition(
                task.task_id,
                if error.code == ErrorCode::TaskUnknown {
                    TaskStatus::Unknown
                } else {
                    TaskStatus::Failed
                },
                Some(error),
            )?;
        }
    }
    Ok(())
}
/// How long an agent may keep running after its turn settled and stdin closed.
const SETTLED_EXIT_GRACE: Duration = Duration::from_secs(3);
async fn write_frame(
    stdin: &mut tokio::process::ChildStdin,
    frame: &serde_json::Value,
) -> Result<()> {
    let mut line = serde_json::to_vec(frame).map_err(failure)?;
    line.push(b'\n');
    stdin
        .write_all(&line)
        .await
        .map_err(|_| failure("could not deliver an ACP message to the agent"))?;
    stdin
        .flush()
        .await
        .map_err(|_| failure("could not flush an ACP message to the agent"))
}
async fn stop_tree(tree: &mut ProcessTree, child: &mut tokio::process::Child) -> bool {
    if tree.terminate().is_err() {
        return false;
    }
    if !matches!(
        tokio::time::timeout(Duration::from_secs(3), child.wait()).await,
        Ok(Ok(_))
    ) {
        return false;
    }
    for _ in 0..40 {
        match tree.stopped() {
            Ok(true) => return true,
            Err(_) => return false,
            Ok(false) => tokio::time::sleep(Duration::from_millis(25)).await,
        }
    }
    false
}

#[cfg(test)]
mod reply_tests {
    use super::*;

    #[test]
    fn reply_is_the_text_after_the_last_tool_call_or_the_last_nonempty_segment() {
        let mut reply = ReplyTracker::default();
        reply.push("Let me check.");
        reply.tool_boundary();
        reply.push("  Done: 42.  ");
        assert_eq!(reply.finish(), Some(("Done: 42.".into(), false)));
        // Ending on a tool call keeps the last segment that had text.
        let mut reply = ReplyTracker::default();
        reply.push("Answer first.");
        reply.tool_boundary();
        reply.push("   ");
        reply.tool_boundary();
        assert_eq!(reply.finish(), Some(("Answer first.".into(), false)));
        assert_eq!(ReplyTracker::default().finish(), None);
    }

    #[test]
    fn reply_fits_one_event_even_when_json_escaping_inflates_it() {
        // Control characters escape to six bytes each.
        let mut reply = ReplyTracker::default();
        reply.push(&"\u{1}".repeat(MAX_FINAL_TEXT_BYTES));
        let (text, truncated) = reply.finish().unwrap();
        assert!(truncated);
        assert!(serde_json::to_string(&text).unwrap().len() <= MAX_FINAL_TEXT_ENCODED_BYTES);
        // Multi-byte text is cut on a character boundary.
        let mut reply = ReplyTracker::default();
        reply.push(&"界".repeat(MAX_FINAL_TEXT_BYTES));
        let (text, truncated) = reply.finish().unwrap();
        assert!(truncated && text.len() <= MAX_FINAL_TEXT_BYTES);
        assert!(text.chars().all(|c| c == '界'));
    }
}
