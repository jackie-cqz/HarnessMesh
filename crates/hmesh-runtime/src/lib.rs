//! Persistent queue executor and bounded, individually tracked managed runs.
mod prepare;
pub use prepare::inspect_references;
pub mod process_tree;
pub mod verification;
pub mod workspace;
use hmesh_adapters::{JsonlDecoder, ObservedEvent, ProtocolObserver, native_record_limit};
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
fn append_observation(
    store: &Store,
    task_id: uuid::Uuid,
    prepared: &mut prepare::Prepared,
    event: ObservedEvent,
) -> Result<()> {
    let value = match event {
        ObservedEvent::TextDelta { text } => {
            return append_text(store, task_id, &prepared.redact_stream(&text));
        }
        ObservedEvent::ToolEvent { name, phase } => {
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
    input
        .write_all(&prepared.input)
        .await
        .map_err(|_| failure("could not deliver managed prompt"))?;
    input
        .flush()
        .await
        .map_err(|_| failure("could not flush managed prompt"))?;
    let mut input = if config.agent.harness == HarnessKind::Pi {
        Some(input)
    } else {
        drop(input);
        None
    };
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
    let record_limit = native_record_limit(config.agent.harness, max_output);
    let mut decoder = JsonlDecoder::new(record_limit);
    let mut observer = ProtocolObserver::new(config.agent.harness);
    let mut output_bytes = 0;
    let mut buffer = [0u8; 8192];
    let mut exited = None;
    let mut output_closed = false;
    let mut cancellation = false;
    let mut run_error = None;
    let deadline = tokio::time::sleep(Duration::from_secs(task.request.limits.timeout_seconds));
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
                if let Err(error) = authorize(task) { run_error = Some(error); break; } }
            _ = &mut deadline => { run_error = Some(failure("managed run deadline exceeded")); break; }
            status = child.wait(), if exited.is_none() => { exited = Some(status.map_err(|_| failure("cannot observe harness exit"))?); }
            read = stdout.read(&mut buffer), if !output_closed => {
                let read = read.map_err(|_| failure("harness stdout failed"))?;
                if read == 0 { output_closed = true; continue; }
                output_bytes += read;
                if output_bytes > max_output { run_error = Some(MeshError::new(ErrorCode::RateLimited, "managed stdout exceeded output limit")); break; }
                match decoder.push(&buffer[..read]) {
                    Ok(records) => for record in records {
                        for event in observer.observe(&record) {
                            if let Err(error) = append_observation(store, task.task_id, &mut prepared, event) {
                                run_error = Some(error);
                                break;
                            }
                        }
                        if run_error.is_some() { break; }
                        if config.agent.harness == HarnessKind::Pi && (observer.settled() || observer.failed_or_waiting()) { input.take(); }
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
        None => decoder
            .finish()
            .and_then(|_| observer.finish(exited.and_then(|s| s.code()))),
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
