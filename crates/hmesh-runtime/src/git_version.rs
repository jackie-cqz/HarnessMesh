//! Bounded local Git observations. No fetch, shell, hooks, or checkout filters.
use anyhow::{Context, Result, bail};
use hmesh_core::{AgentConfig, GitVersionRequirement, GitVersionState};
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeSet,
    fs,
    io::Read,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    time::{Duration, Instant},
};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GitInspection {
    pub available: bool,
    pub current: Option<GitVersionState>,
    pub requested: Option<GitVersionRequirement>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pinned_available: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error_code: Option<String>,
}

pub(crate) fn executable(config: &AgentConfig) -> Result<PathBuf> {
    if let Some(path) = &config.project.git_executable {
        if !path.is_absolute() || !path.is_file() {
            bail!("configured Git is unavailable");
        }
        return Ok(path.clone());
    }
    let paths = std::env::var_os("PATH").context("Git is unavailable")?;
    for dir in std::env::split_paths(&paths).filter(|p| p.is_absolute()) {
        let candidate = dir.join(if cfg!(windows) { "git.exe" } else { "git" });
        if candidate.is_file() {
            return Ok(candidate);
        }
    }
    bail!("Git is unavailable")
}

fn run(
    executable: &Path,
    root: &Path,
    args: &[&str],
    scratch: &Path,
    configs: &[String],
) -> Result<Vec<u8>> {
    fs::create_dir_all(scratch)?;
    let hooks = crate::prepare::native_path(&scratch.join("disabled-hooks"));
    fs::create_dir_all(&hooks)?;
    let out = scratch.join(format!("git-{}.out", uuid::Uuid::new_v4()));
    let file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&out)?;
    let mut command = Command::new(crate::prepare::native_path(executable));
    for (name, _) in std::env::vars_os() {
        if name
            .to_string_lossy()
            .to_ascii_uppercase()
            .starts_with("GIT_")
        {
            command.env_remove(name);
        }
    }
    command
        .arg("--no-optional-locks")
        .arg("-c")
        .arg(format!("core.hooksPath={}", hooks.display()))
        .arg("-c")
        .arg("core.fsmonitor=false")
        .arg("-c")
        .arg("core.untrackedCache=false")
        .arg("-c")
        .arg("protocol.allow=never")
        .arg("-c")
        .arg("submodule.recurse=false")
        .arg("-c")
        .arg(format!(
            "core.attributesFile={}",
            if cfg!(windows) { "NUL" } else { "/dev/null" }
        ));
    for value in configs {
        command.arg("-c").arg(value);
    }
    let mut child = command
        .arg("-C")
        .arg(crate::prepare::native_path(root))
        .args(args)
        .stdin(Stdio::null())
        .stdout(file)
        .stderr(Stdio::null())
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_NO_LAZY_FETCH", "1")
        .env(
            "GIT_CONFIG_GLOBAL",
            if cfg!(windows) { "NUL" } else { "/dev/null" },
        )
        .env("GIT_ATTR_NOSYSTEM", "1")
        .spawn()
        .context("local Git could not start")?;
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
        bail!("local Git operation failed or exceeded its 30-second/4-MiB budget");
    }
    Ok(bytes)
}

pub(crate) fn git(
    executable: &Path,
    root: &Path,
    args: &[&str],
    scratch: &Path,
) -> Result<Vec<u8>> {
    // Names only: never load or expose credential/config values. Checkout filters
    // may download objects or execute commands, so disable every configured one.
    let names = run(
        executable,
        root,
        &["config", "--null", "--name-only", "--list"],
        scratch,
        &[],
    )?;
    let mut filters = BTreeSet::new();
    for name in names.split(|b| *b == 0).filter(|n| !n.is_empty()) {
        let key = std::str::from_utf8(name)?;
        if let Some(rest) = key.strip_prefix("filter.")
            && let Some((filter, _)) = rest.rsplit_once('.')
        {
            if filter.is_empty()
                || filter.len() > 256
                || !filter
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.'))
            {
                bail!("unsupported local Git filter name");
            }
            filters.insert(filter.to_owned());
        }
    }
    let configs: Vec<String> = filters
        .iter()
        .flat_map(|name| {
            [
                format!("filter.{name}.process="),
                format!("filter.{name}.smudge="),
                format!("filter.{name}.clean="),
                format!("filter.{name}.required=false"),
            ]
        })
        .collect();
    run(executable, root, args, scratch, &configs)
}
pub(crate) fn text(bytes: Vec<u8>) -> Result<String> {
    Ok(String::from_utf8(bytes)?.trim().to_owned())
}

pub(crate) fn state(executable: &Path, root: &Path, scratch: &Path) -> Result<GitVersionState> {
    let top = PathBuf::from(text(git(
        executable,
        root,
        &["rev-parse", "--show-toplevel"],
        scratch,
    )?)?);
    if fs::canonicalize(top)? != fs::canonicalize(root)? {
        bail!("registered project must be the repository root");
    }
    let commit = text(git(
        executable,
        root,
        &["rev-parse", "--verify", "HEAD"],
        scratch,
    )?)?;
    let branch = text(git(
        executable,
        root,
        &["rev-parse", "--abbrev-ref", "HEAD"],
        scratch,
    )?)?;
    let dirty = !git(
        executable,
        root,
        &["status", "--porcelain=v1", "--untracked-files=normal"],
        scratch,
    )?
    .is_empty();
    let observed = GitVersionState {
        branch: (branch != "HEAD").then_some(branch),
        commit,
        dirty,
    };
    observed.validate()?;
    Ok(observed)
}

pub(crate) fn has_commit(executable: &Path, root: &Path, commit: &str, scratch: &Path) -> bool {
    // Validation excludes options, refs, expressions, short hashes and URLs.
    let requirement = GitVersionRequirement::Pinned {
        commit: commit.into(),
    };
    requirement.validate().is_ok()
        && text(
            git(
                executable,
                root,
                &[
                    "rev-parse",
                    "--verify",
                    "--end-of-options",
                    &format!("{commit}^{{commit}}"),
                ],
                scratch,
            )
            .unwrap_or_default(),
        )
        .is_ok_and(|actual| actual.eq_ignore_ascii_case(commit))
}

pub fn inspect(config: &AgentConfig, requested: Option<&GitVersionRequirement>) -> GitInspection {
    let mut report = GitInspection {
        available: false,
        current: None,
        requested: requested.cloned(),
        pinned_available: None,
        error_code: Some("GIT_UNAVAILABLE".into()),
    };
    let Ok(scratch) = tempfile::tempdir() else {
        return report;
    };
    let Ok(executable) = executable(config) else {
        return report;
    };
    let Ok(current) = state(&executable, &config.project.root, scratch.path()) else {
        return report;
    };
    report.available = true;
    report.current = Some(current);
    report.error_code = None;
    if let Some(GitVersionRequirement::Pinned { commit }) = requested {
        report.pinned_available = Some(has_commit(
            &executable,
            &config.project.root,
            commit,
            scratch.path(),
        ));
    }
    report
}

pub fn version_checks(
    config: &AgentConfig,
    report: &GitInspection,
    requested: &GitVersionRequirement,
) -> Vec<(&'static str, bool)> {
    let mut checks = vec![("GIT_UNAVAILABLE", report.available)];
    match requested {
        GitVersionRequirement::Current {
            expected_commit,
            require_clean,
        } => {
            if let Some(expected) = expected_commit {
                checks.push((
                    "GIT_COMMIT_MISMATCH",
                    report
                        .current
                        .as_ref()
                        .is_some_and(|state| state.commit.eq_ignore_ascii_case(expected)),
                ));
            }
            if *require_clean || config.project.workspace_mode == "git-worktree" {
                checks.push((
                    "GIT_DIRTY",
                    report.current.as_ref().is_some_and(|state| !state.dirty),
                ));
            }
        }
        GitVersionRequirement::Pinned { .. } => checks.push((
            "GIT_COMMIT_UNAVAILABLE",
            report.pinned_available == Some(true),
        )),
    }
    checks
}

pub fn enforce(config: &AgentConfig, requested: &GitVersionRequirement) -> Result<GitInspection> {
    requested.validate()?;
    let report = inspect(config, Some(requested));
    if let Some((code, _)) = version_checks(config, &report, requested)
        .into_iter()
        .find(|(_, passed)| !passed)
    {
        bail!("{code}: execution repository no longer meets the task version restriction");
    }
    Ok(report)
}
