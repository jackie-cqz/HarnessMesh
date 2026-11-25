//! Local, deterministic repositories only; no harness process or network.
use hmesh_core::{AgentConfig, Envelope, GitVersionRequirement, TaskRecord, TaskSubmit};
use hmesh_runtime::{git_version, workspace};
use hmesh_store::Store;
use serde_json::json;
use std::{
    fs,
    path::{Path, PathBuf},
    process::Command,
};
use uuid::Uuid;

fn git_path() -> PathBuf {
    std::env::split_paths(&std::env::var_os("PATH").unwrap())
        .filter(|p| p.is_absolute())
        .map(|p| p.join(if cfg!(windows) { "git.exe" } else { "git" }))
        .find(|p| p.is_file())
        .expect("Git required by worktree regression tests")
}
fn git(root: &Path, args: &[&str]) -> String {
    let output = Command::new(git_path())
        .arg("-C")
        .arg(root)
        .arg("-c")
        .arg("user.name=HarnessMesh fixture")
        .arg("-c")
        .arg("user.email=fixture@example.invalid")
        .arg("-c")
        .arg("commit.gpgsign=false")
        .args(args)
        .output()
        .unwrap();
    assert!(output.status.success(), "fixture Git failed");
    String::from_utf8(output.stdout).unwrap().trim().into()
}
fn config(root: &Path) -> AgentConfig {
    serde_json::from_value(json!({
        "agent":{"name":"git-fixture","harness":"acp","project_id":"fixture"},
        "project":{"root":root,"allowed_tools":["read","write"],"git_executable":git_path()},
        "adapter":{"executable":std::env::current_exe().unwrap()}
    }))
    .unwrap()
}
fn setup() -> (tempfile::TempDir, AgentConfig, String) {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("project");
    fs::create_dir(&root).unwrap();
    git(&root, &["init", "-b", "main"]);
    fs::write(root.join("file.txt"), "first\n").unwrap();
    git(&root, &["add", "."]);
    git(&root, &["commit", "-m", "first"]);
    let first = git(&root, &["rev-parse", "HEAD"]);
    (temp, config(&root), first)
}
fn task(store: &Store, requested: Option<GitVersionRequirement>) -> TaskRecord {
    let mut request = TaskSubmit::new(
        Uuid::new_v4(),
        "fixture",
        "local Git fixture, never execute",
    );
    request.git_version = requested;
    store
        .admit(
            Uuid::new_v4(),
            &Envelope::new(Uuid::new_v4(), "task.submit", request),
            false,
            32,
        )
        .unwrap()
}

#[test]
fn legacy_non_git_project_stays_shared_and_optional_observation_does_not_fail() {
    let temp = tempfile::tempdir().unwrap();
    let config = config(temp.path());
    let store = Store::open(&temp.path().join("state.sqlite3")).unwrap();
    let task = task(&store, None);
    assert!(!git_version::inspect(&config, None).available);
    let prepared = workspace::prepare(&store, &task, &config, &temp.path().join("run")).unwrap();
    assert_eq!(prepared.project.root, config.project.root);
    assert!(store.workspace(task.task_id).unwrap().is_none());
}

#[test]
fn current_restrictions_block_mismatch_and_clean_requirement_without_reserving_workspace() {
    let (temp, config, commit) = setup();
    let store = Store::open(&temp.path().join("state.sqlite3")).unwrap();
    let mismatch = GitVersionRequirement::Current {
        expected_commit: Some("0".repeat(40)),
        require_clean: false,
    };
    let report = git_version::inspect(&config, Some(&mismatch));
    assert_eq!(
        report.current.as_ref().unwrap().branch.as_deref(),
        Some("main")
    );
    assert_eq!(report.current.as_ref().unwrap().commit, commit);
    assert!(
        git_version::version_checks(&config, &report, &mismatch)
            .contains(&("GIT_COMMIT_MISMATCH", false))
    );
    let blocked = task(&store, Some(mismatch));
    assert!(workspace::prepare(&store, &blocked, &config, &temp.path().join("blocked")).is_err());
    assert!(store.workspace(blocked.task_id).unwrap().is_none());
    fs::write(config.project.root.join("file.txt"), "dirty\n").unwrap();
    let clean = GitVersionRequirement::Current {
        expected_commit: Some(commit),
        require_clean: true,
    };
    assert!(git_version::enforce(&config, &clean).is_err());
}

#[test]
fn explicit_current_uses_existing_tree_and_finish_never_stages_its_changes() {
    let (temp, config, commit) = setup();
    let store = Store::open(&temp.path().join("state.sqlite3")).unwrap();
    fs::write(config.project.root.join("file.txt"), "dirty\n").unwrap();
    let task = task(
        &store,
        Some(GitVersionRequirement::Current {
            expected_commit: Some(commit),
            require_clean: false,
        }),
    );
    let run = temp.path().join("run");
    let prepared = workspace::prepare(&store, &task, &config, &run).unwrap();
    assert_eq!(prepared.project.root, config.project.root);
    workspace::enforce_prepared(&store, &task, &prepared).unwrap();
    workspace::finish(&store, &task, &run).unwrap();
    let record = store.workspace(task.task_id).unwrap().unwrap();
    assert_eq!(record["state"], "shared");
    assert_eq!(record["actual_git"]["dirty"], true);
    assert_eq!(git(&config.project.root, &["diff", "--cached"]), "");
    git(&config.project.root, &["add", "."]);
    git(
        &config.project.root,
        &["commit", "-m", "changed after review"],
    );
    assert!(workspace::enforce_prepared(&store, &task, &prepared).is_err());
}

#[test]
fn current_selection_preserves_owner_worktree_isolation_and_clean_requirement() {
    let (temp, mut config, commit) = setup();
    config.project.workspace_mode = "git-worktree".into();
    let store = Store::open(&temp.path().join("state.sqlite3")).unwrap();
    let task = task(
        &store,
        Some(GitVersionRequirement::Current {
            expected_commit: Some(commit.clone()),
            require_clean: false,
        }),
    );
    let run = temp.path().join("run");
    let prepared = workspace::prepare(&store, &task, &config, &run).unwrap();
    assert_ne!(prepared.project.root, config.project.root);
    workspace::enforce_prepared(&store, &task, &prepared).unwrap();
    let record = store.workspace(task.task_id).unwrap().unwrap();
    assert_eq!(record["actual_git"]["commit"], commit);
    assert_eq!(record["actual_git"]["branch"], serde_json::Value::Null);
    fs::write(config.project.root.join("file.txt"), "owner dirty\n").unwrap();
    assert!(workspace::prepare(&store, &task, &config, &temp.path().join("other-run")).is_err());
}

#[test]
fn pinned_existing_commit_uses_clean_detached_worktree_and_preserves_dirty_original() {
    let (temp, config, first) = setup();
    fs::write(config.project.root.join("file.txt"), "second\n").unwrap();
    git(&config.project.root, &["add", "."]);
    git(&config.project.root, &["commit", "-m", "second"]);
    let second = git(&config.project.root, &["rev-parse", "HEAD"]);
    fs::write(config.project.root.join("file.txt"), "local uncommitted\n").unwrap();
    let store = Store::open(&temp.path().join("state.sqlite3")).unwrap();
    let task = task(
        &store,
        Some(GitVersionRequirement::Pinned {
            commit: first.to_ascii_uppercase(),
        }),
    );
    let run = temp.path().join("run");
    let prepared = workspace::prepare(&store, &task, &config, &run).unwrap();
    assert_ne!(prepared.project.root, config.project.root);
    assert_eq!(
        fs::read_to_string(prepared.project.root.join("file.txt")).unwrap(),
        "first\n"
    );
    workspace::enforce_prepared(&store, &task, &prepared).unwrap();
    let record = store.workspace(task.task_id).unwrap().unwrap();
    assert_eq!(record["actual_git"]["commit"], first);
    assert_eq!(record["actual_git"]["branch"], serde_json::Value::Null);
    assert_eq!(record["actual_git"]["dirty"], false);
    assert_eq!(git(&config.project.root, &["rev-parse", "HEAD"]), second);
    assert_eq!(
        fs::read_to_string(config.project.root.join("file.txt")).unwrap(),
        "local uncommitted\n"
    );
    fs::write(prepared.project.root.join("file.txt"), "task change\n").unwrap();
    workspace::finish(&store, &task, &run).unwrap();
    let record = store.workspace(task.task_id).unwrap().unwrap();
    assert_eq!(record["state"], "review");
    assert!(record["patch"].as_str().unwrap().contains("task change"));
    assert_eq!(git(&config.project.root, &["diff", "--cached"]), "");
}

#[test]
fn unavailable_pinned_commit_fails_without_worktree_or_implicit_fetch() {
    let (temp, config, _) = setup();
    git(
        &config.project.root,
        &[
            "remote",
            "add",
            "origin",
            "https://example.invalid/do-not-fetch",
        ],
    );
    let store = Store::open(&temp.path().join("state.sqlite3")).unwrap();
    let requested = GitVersionRequirement::Pinned {
        commit: "0".repeat(40),
    };
    let report = git_version::inspect(&config, Some(&requested));
    assert!(report.available);
    assert_eq!(report.pinned_available, Some(false));
    let task = task(&store, Some(requested));
    let run = temp.path().join("run");
    assert!(workspace::prepare(&store, &task, &config, &run).is_err());
    assert!(!run.join("workspace").exists());
    assert!(!config.project.root.join(".git/FETCH_HEAD").exists());
}

#[test]
fn pinned_checkout_disables_configured_filters_and_post_checkout_hooks() {
    let (temp, config, _) = setup();
    fs::write(
        config.project.root.join(".gitattributes"),
        "file.txt filter=tripwire\n",
    )
    .unwrap();
    git(&config.project.root, &["add", "."]);
    git(&config.project.root, &["commit", "-m", "filter attributes"]);
    let commit = git(&config.project.root, &["rev-parse", "HEAD"]);
    git(
        &config.project.root,
        &[
            "config",
            "filter.tripwire.process",
            "hmesh-filter-must-never-run",
        ],
    );
    git(
        &config.project.root,
        &[
            "config",
            "filter.tripwire.smudge",
            "hmesh-smudge-must-never-run",
        ],
    );
    git(
        &config.project.root,
        &["config", "filter.tripwire.required", "true"],
    );
    let hook = config.project.root.join(".git/hooks/post-checkout");
    fs::write(&hook, "#!/bin/sh\nexit 87\n").unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&hook, fs::Permissions::from_mode(0o755)).unwrap();
    }
    let store = Store::open(&temp.path().join("state.sqlite3")).unwrap();
    let task = task(&store, Some(GitVersionRequirement::Pinned { commit }));
    let prepared = workspace::prepare(&store, &task, &config, &temp.path().join("run")).unwrap();
    assert_eq!(
        fs::read_to_string(prepared.project.root.join("file.txt")).unwrap(),
        "first\n"
    );
}
