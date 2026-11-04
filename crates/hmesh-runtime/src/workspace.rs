//! Opt-in Git worktrees. Applying a reviewed patch is an offline owner operation.
use anyhow::{Context, Result, bail};
use hmesh_core::{AgentConfig, TaskRecord};
use hmesh_store::Store;
use serde_json::{Value, json};
use std::{
    fs,
    io::Read,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    time::{Duration, Instant},
};

fn git(executable: &Path, root: &Path, args: &[&str], scratch: &Path) -> Result<Vec<u8>> {
    fs::create_dir_all(scratch)?;
    let hooks = crate::prepare::native_path(&scratch.join("disabled-hooks"));
    fs::create_dir_all(&hooks)?;
    let out = scratch.join(format!("git-{}.out", uuid::Uuid::new_v4()));
    let file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&out)?;
    let mut child = Command::new(crate::prepare::native_path(executable))
        .arg("-c")
        .arg(format!("core.hooksPath={}", hooks.display()))
        .arg("-c")
        .arg("core.fsmonitor=false")
        .arg("-C")
        .arg(crate::prepare::native_path(root))
        .args(args)
        .stdin(Stdio::null())
        .stdout(file)
        .stderr(Stdio::null())
        .env("GIT_TERMINAL_PROMPT", "0")
        .spawn()
        .context("configured Git could not start")?;
    let start = Instant::now();
    let result = loop {
        if let Some(status) = child.try_wait()? {
            break status.success();
        }
        if start.elapsed() > Duration::from_secs(30) || fs::metadata(&out)?.len() > 4 * 1024 * 1024
        {
            let _ = child.kill();
            let _ = child.wait();
            break false;
        }
        std::thread::sleep(Duration::from_millis(20));
    };
    let mut bytes = Vec::new();
    fs::File::open(&out)?
        .take(4 * 1024 * 1024 + 1)
        .read_to_end(&mut bytes)?;
    fs::remove_file(out)?;
    if !result || bytes.len() > 4 * 1024 * 1024 {
        bail!(
            "Git operation failed or exceeded its 30-second/4-MiB budget; inspect the retained workspace"
        );
    }
    Ok(bytes)
}
fn text(bytes: Vec<u8>) -> Result<String> {
    Ok(String::from_utf8(bytes)?.trim().to_owned())
}
pub fn prepare(
    store: &Store,
    task: &TaskRecord,
    config: &AgentConfig,
    run: &Path,
) -> Result<AgentConfig> {
    if config.project.workspace_mode == "shared" {
        return Ok(config.clone());
    }
    let executable = config
        .project
        .git_executable
        .as_ref()
        .context("absolute Git executable required")?;
    let original = crate::prepare::native_path(&fs::canonicalize(&config.project.root)?);
    let top = PathBuf::from(text(git(
        executable,
        &original,
        &["rev-parse", "--show-toplevel"],
        run,
    )?)?);
    if fs::canonicalize(top)? != fs::canonicalize(&original)? {
        bail!("worktree project must be the repository root");
    }
    if !git(executable, &original, &["status", "--porcelain"], run)?.is_empty() {
        bail!("worktree tasks require a clean committed project");
    }
    let base = text(git(executable, &original, &["rev-parse", "HEAD"], run)?)?;
    let path = crate::prepare::native_path(&run.join("workspace"));
    let mut record = json!({"task_id":task.task_id,"attempt_id":task.last_attempt_id,"root":original,"path":path,"git":executable,"base_commit":base,"state":"creating"});
    store.workspace_put(task.task_id, &record)?;
    git(
        executable,
        &original,
        &[
            "worktree",
            "add",
            "--detach",
            path.to_str().context("workspace path is not UTF-8")?,
            &base,
        ],
        run,
    )?;
    record["state"] = json!("running");
    store.workspace_put(task.task_id, &record)?;
    let mut isolated = config.clone();
    isolated.project.root = path;
    Ok(isolated)
}
pub fn finish(store: &Store, task: &TaskRecord, run: &Path) -> Result<()> {
    let Some(mut record) = store.workspace(task.task_id)? else {
        return Ok(());
    };
    let path = Path::new(record["path"].as_str().context("invalid workspace")?);
    let executable = Path::new(record["git"].as_str().context("invalid Git reference")?);
    git(executable, path, &["add", "--all", "--", "."], run)?;
    let patch = git(
        executable,
        path,
        &[
            "diff",
            "--cached",
            "--binary",
            "--full-index",
            "--no-ext-diff",
            "--no-textconv",
            record["base_commit"].as_str().context("missing base")?,
        ],
        run,
    )?;
    record["patch_sha256"] = json!(hmesh_security::sha256_hex(&patch));
    record["patch"] = json!(String::from_utf8(patch)?);
    record["state"] = json!("review");
    store.workspace_put(task.task_id, &record)?;
    Ok(())
}
pub fn apply(store: &Store, id: uuid::Uuid, expected: &str, scratch: &Path) -> Result<Value> {
    if store.tasks()?.iter().any(|t| {
        matches!(
            t.status,
            hmesh_core::TaskStatus::Running
                | hmesh_core::TaskStatus::Cancelling
                | hmesh_core::TaskStatus::WaitingInput
        )
    }) {
        bail!("unfinished execution records require recovery before applying a patch");
    }
    let task = store.task(id)?;
    if task.status != hmesh_core::TaskStatus::Succeeded {
        bail!("only successful tasks can be applied");
    }
    let mut record = store
        .workspace(id)?
        .context("task has no isolated workspace")?;
    if record["state"] != "review" || record["patch_sha256"] != expected {
        bail!("reviewed patch hash/state does not match");
    }
    let patch = record["patch"].as_str().context("patch unavailable")?;
    if hmesh_security::sha256_hex(patch.as_bytes()) != expected {
        bail!("patch integrity check failed");
    }
    let root = Path::new(record["root"].as_str().context("invalid project")?);
    let executable = Path::new(record["git"].as_str().context("invalid Git")?);
    if text(git(executable, root, &["rev-parse", "HEAD"], scratch)?)?
        != record["base_commit"].as_str().context("missing base")?
        || !git(executable, root, &["status", "--porcelain"], scratch)?.is_empty()
    {
        bail!("project changed since the task started; review and reconcile manually");
    }
    let patch_path = crate::prepare::native_path(&scratch.join(format!("reviewed-{id}.patch")));
    hmesh_security::atomic_write(&patch_path, patch.as_bytes())?;
    if !patch.is_empty() {
        git(
            executable,
            root,
            &[
                "apply",
                "--check",
                "--index",
                patch_path.to_str().context("invalid patch path")?,
            ],
            scratch,
        )?;
        git(
            executable,
            root,
            &[
                "apply",
                "--index",
                patch_path.to_str().context("invalid patch path")?,
            ],
            scratch,
        )?;
    }
    record["state"] = json!("applied");
    store.workspace_put(id, &record)?;
    store.audit(
        None,
        "workspace.apply",
        &id.to_string(),
        "owner-reviewed",
        uuid::Uuid::new_v4(),
    )?;
    Ok(json!({"task_id":id,"state":"applied","patch_sha256":expected,"committed":false}))
}
pub fn prune(store: &Store, id: uuid::Uuid, expected: &str, node: &Path) -> Result<Value> {
    let task = store.task(id)?;
    if task.status != hmesh_core::TaskStatus::Succeeded {
        bail!("only successful, already-applied worktrees may be pruned");
    }
    let flows: Vec<hmesh_core::development::WorkflowRun> = store.managed_list("workflow")?;
    if flows.iter().any(|f| {
        !matches!(f.state.as_str(), "completed" | "cancelled")
            && f.plans.iter().flatten().any(|p| p.task.task_id == id)
    }) {
        bail!("worktree is referenced by an unfinished workflow");
    }
    let mut record = store.workspace(id)?.context("no worktree")?;
    if record["state"] != "applied" || record["patch_sha256"] != expected {
        bail!("prune requires the exact already-applied patch hash");
    }
    let attempt = task.last_attempt_id.context("missing attempt")?;
    let run = node.join("runs").join(attempt.to_string());
    let path = Path::new(record["path"].as_str().context("invalid worktree path")?);
    if !fs::canonicalize(path)?.starts_with(fs::canonicalize(&run)?) {
        bail!("worktree resolves outside its managed run");
    }
    for candidate in [
        node.to_path_buf(),
        node.join("runs"),
        run.clone(),
        path.to_path_buf(),
    ] {
        let metadata = fs::symlink_metadata(candidate)?;
        if metadata.file_type().is_symlink() {
            bail!("prune refuses symbolic links");
        }
        #[cfg(windows)]
        {
            use std::os::windows::fs::MetadataExt;
            if metadata.file_attributes() & 0x400 != 0 {
                bail!("prune refuses reparse points");
            }
        }
    }
    if fs::canonicalize(path)? != fs::canonicalize(run.join("workspace"))?
        || fs::symlink_metadata(&run)?.file_type().is_symlink()
        || fs::symlink_metadata(node.join("runs"))?
            .file_type()
            .is_symlink()
    {
        bail!("worktree is outside this managed attempt");
    }
    let executable = Path::new(record["git"].as_str().context("invalid Git reference")?);
    let scratch = node.join("workspace-prune");
    git(executable, path, &["add", "--all", "--", "."], &scratch)?;
    let patch = git(
        executable,
        path,
        &[
            "diff",
            "--cached",
            "--binary",
            "--full-index",
            "--no-ext-diff",
            "--no-textconv",
            record["base_commit"].as_str().context("missing base")?,
        ],
        &scratch,
    )?;
    if hmesh_security::sha256_hex(&patch) != expected {
        bail!("worktree changed after review; it was preserved");
    }
    let root = Path::new(record["root"].as_str().context("invalid root")?);
    git(
        executable,
        root,
        &[
            "worktree",
            "remove",
            "--force",
            path.to_str().context("invalid worktree path")?,
        ],
        &scratch,
    )?;
    record["state"] = json!("pruned");
    store.workspace_put(id, &record)?;
    store.audit(
        None,
        "workspace.prune",
        &id.to_string(),
        "owner-reviewed",
        uuid::Uuid::new_v4(),
    )?;
    Ok(json!({"task_id":id,"state":"pruned","patch_preserved":true}))
}
