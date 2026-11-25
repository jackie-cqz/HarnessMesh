//! Optional, owner-initiated installation of reviewed ACP adapter packages.
use crate::prepare::native_path;
use anyhow::{Context, Result, bail, ensure};
use hmesh_adapters::executable::probe_command;
use hmesh_core::{DetectedAgent, HarnessKind};
use serde::Serialize;
use serde_json::Value;
use std::{
    fs::{self, OpenOptions},
    path::{Path, PathBuf},
    process::Stdio,
    time::Duration,
};
use tokio::process::Command;

#[derive(Clone, Copy, Serialize)]
pub struct Recommendation {
    pub harness: HarnessKind,
    pub version: &'static str,
    pub node: &'static str,
}

pub const RECOMMENDED: [Recommendation; 2] = [
    Recommendation {
        harness: HarnessKind::Codex,
        version: "2.8.0",
        node: "20.3",
    },
    Recommendation {
        harness: HarnessKind::ClaudeCode,
        version: "0.109.0",
        node: "22.0",
    },
];

const INSTALL_TIMEOUT: Duration = Duration::from_secs(120);
const TOOL_TIMEOUT: Duration = Duration::from_secs(8);

impl Recommendation {
    pub fn package(self) -> &'static str {
        self.harness.acp_package().expect("recommended ACP package")
    }
    fn destination(self, root: &Path) -> PathBuf {
        root.join(self.harness.to_string()).join(self.version)
    }
    // Both pinned packages ship the same layout, including on Windows.
    fn detect(self, directory: &Path) -> Option<DetectedAgent> {
        let package_dir = directory.join("node_modules").join(self.package());
        let package: Value =
            serde_json::from_slice(&fs::read(package_dir.join("package.json")).ok()?).ok()?;
        let command = self.harness.acp_command()?;
        if package["name"] != self.package()
            || package["version"] != self.version
            || package["bin"][command] != "dist/index.js"
        {
            return None;
        }
        let entry = package_dir.join("dist/index.js");
        entry.is_file().then(|| DetectedAgent {
            harness: self.harness,
            command: command.into(),
            executable: entry,
        })
    }
}

pub fn detect(data_dir: &Path) -> Vec<DetectedAgent> {
    let mut agents = hmesh_core::detect_installed_agents();
    if let Ok(dir) = fs::canonicalize(data_dir) {
        let root = native_path(&dir).join("agent-runtimes");
        for recommendation in RECOMMENDED {
            if !agents.iter().any(|a| a.harness == recommendation.harness)
                && let Some(agent) = recommendation.detect(&recommendation.destination(&root))
            {
                agents.push(agent);
            }
        }
    }
    agents
}

#[derive(Serialize)]
pub struct Installation {
    pub agent: DetectedAgent,
    pub source: &'static str,
    pub protocol_probe: &'static str,
}

async fn probe(agent: DetectedAgent, source: &'static str) -> Result<Installation> {
    let cwd = tempfile::tempdir()?;
    let mut command =
        hmesh_adapters::resolve_executable(agent.harness, &agent.executable)?.command();
    command
        .args(agent.harness.default_acp_args())
        .current_dir(cwd.path());
    let report = probe_command(command).await?;
    ensure!(
        report.version_supported,
        "the adapter did not pass ACP initialize; check its installation and Node.js version"
    );
    Ok(Installation {
        agent,
        source,
        protocol_probe: "acp_initialize_passed",
    })
}

fn path_lookup(name: &str) -> Option<PathBuf> {
    std::env::split_paths(&std::env::var_os("PATH")?)
        .filter(|path| path.is_absolute())
        .map(|path| path.join(name))
        .find(|path| path.is_file())
}

fn npm_entry(node: &Path) -> Result<PathBuf> {
    if let Some(npm) = path_lookup("npm")
        && let Ok(entry) = fs::canonicalize(npm)
        && entry.extension().is_some_and(|ext| ext == "js")
    {
        return Ok(native_path(&entry));
    }
    let bundled_entry = node
        .parent()
        .context("Node.js executable has no parent directory")?
        .join("node_modules/npm/bin/npm-cli.js");
    ensure!(
        bundled_entry.is_file(),
        "npm CLI was not found alongside Node.js or on PATH"
    );
    Ok(bundled_entry)
}

fn npm_environment(command: &mut Command) {
    // npm fetches public packages. Keep system/network settings, so adapter
    // credentials and package-registry tokens are not inherited by npm.
    command.env_clear();
    for name in [
        "PATH",
        "HOME",
        "USERPROFILE",
        "APPDATA",
        "LOCALAPPDATA",
        "SystemRoot",
        "TEMP",
        "TMPDIR",
        "TMP",
        "HTTP_PROXY",
        "HTTPS_PROXY",
        "NO_PROXY",
        "http_proxy",
        "https_proxy",
        "no_proxy",
        "ALL_PROXY",
        "all_proxy",
        "NODE_EXTRA_CA_CERTS",
    ] {
        if let Some(value) = std::env::var_os(name) {
            command.env(name, value);
        }
    }
}

async fn download(pkg: Recommendation, stage: &Path) -> Result<()> {
    let node = path_lookup(if cfg!(windows) { "node.exe" } else { "node" })
        .context("Node.js is required for this adapter; install Node.js and restart HarnessMesh")?;
    let npm = npm_entry(&node)?;
    let version = tokio::time::timeout(
        TOOL_TIMEOUT,
        Command::new(&node)
            .arg("--version")
            .kill_on_drop(true)
            .output(),
    )
    .await
    .context("Node.js version check timed out")??;
    let version = std::str::from_utf8(&version.stdout)?
        .trim()
        .trim_start_matches('v');
    let major_minor = |s: &str| -> Option<(u64, u64)> {
        let mut parts = s.split('.');
        Some((parts.next()?.parse().ok()?, parts.next()?.parse().ok()?))
    };
    let minimum = major_minor(pkg.node).context("invalid recommended Node.js version")?;
    ensure!(
        major_minor(version).is_some_and(|v| v >= minimum),
        "this adapter requires Node.js {} or newer",
        pkg.node
    );
    let home_config = stage.join("npm-user.conf");
    let global_config = stage.join("npm-global.conf");
    fs::write(&home_config, "")?;
    fs::write(&global_config, "")?;
    let mut command = Command::new(node);
    command
        .arg(npm)
        .args([
            "install",
            "--ignore-scripts",
            "--include=optional",
            "--no-audit",
            "--no-fund",
            "--no-progress",
            "--no-update-notifier",
            "--save-exact",
            "--registry=https://registry.npmjs.org/",
            "--fetch-retries=1",
        ])
        .arg("--userconfig")
        .arg(home_config)
        .arg("--globalconfig")
        .arg(global_config)
        .arg("--cache")
        .arg(stage.join(".npm-cache"))
        .arg(format!("{}@{}", pkg.package(), pkg.version))
        .current_dir(stage)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .kill_on_drop(true);
    #[cfg(windows)]
    command.creation_flags(0x08000000);
    npm_environment(&mut command);
    let status = tokio::time::timeout(INSTALL_TIMEOUT, command.status())
        .await
        .context("npm install timed out; check connectivity to registry.npmjs.org")?
        .context("could not start npm")?;
    ensure!(
        status.success(),
        "npm install failed ({status}); check registry connectivity and the local Node.js installation"
    );
    // The cache contains installation logs and can be large. Keep only the
    // installed package tree, manifest and npm lockfile in the node runtime.
    fs::remove_dir_all(stage.join(".npm-cache"))?;
    fs::remove_file(stage.join("npm-user.conf"))?;
    fs::remove_file(stage.join("npm-global.conf"))?;
    Ok(())
}

pub async fn install(data_dir: &Path, harness: HarnessKind) -> Result<Installation> {
    let Some(pkg) = RECOMMENDED
        .iter()
        .copied()
        .find(|pkg| pkg.harness == harness)
    else {
        bail!("this agent does not have a managed adapter package");
    };
    if harness == HarnessKind::Codex
        && let Some(home) = std::env::var_os("CODEX_HOME").filter(|path| !path.is_empty())
    {
        let home = PathBuf::from(home);
        ensure!(
            home.is_absolute() && home.is_dir(),
            "CODEX_HOME must refer to an existing absolute directory; correct it where HarnessMesh starts"
        );
    }
    if let Some(agent) = hmesh_core::detect_installed_agents()
        .into_iter()
        .find(|a| a.harness == harness)
    {
        return probe(agent, "system").await;
    }
    let dir = native_path(&fs::canonicalize(data_dir)?);
    let root = dir.join("agent-runtimes");
    let target = pkg.destination(&root);
    let parent = target
        .parent()
        .context("managed installation has no parent directory")?;
    fs::create_dir_all(parent)?;
    let lease = OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(parent.join("install.lock"))?;
    lease
        .try_lock()
        .context("an adapter installation is already in progress")?;
    if let Some(agent) = pkg.detect(&target) {
        return probe(agent, "managed").await;
    }
    ensure!(
        !target.exists(),
        "the managed runtime directory is incomplete; inspect or move it before retrying"
    );
    let stage = tempfile::Builder::new()
        .prefix("install-")
        .tempdir_in(parent)?;
    // An explicit manifest makes npm install into this temporary directory,
    // even when the HarnessMesh instance resides inside another npm project.
    fs::write(stage.path().join("package.json"), r#"{"private":true}"#)?;
    download(pkg, stage.path()).await?;
    let agent = pkg
        .detect(stage.path())
        .context("the downloaded package did not contain the expected ACP executable")?;
    let mut installed = probe(agent, "managed").await?;
    let entry = installed
        .agent
        .executable
        .strip_prefix(stage.path())?
        .to_owned();
    fs::rename(stage.path(), &target).context("could not activate the installed adapter")?;
    installed.agent.executable = target.join(entry);
    Ok(installed)
}
