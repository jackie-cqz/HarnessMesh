//! Optional, owner-initiated installation of reviewed ACP adapter packages.
use crate::prepare::native_path;
use anyhow::{Context, Result, bail, ensure};
use hmesh_adapters::executable::probe_command;
use hmesh_core::{
    AgentSpec, CATALOG, DetectedAgent, HarnessKind,
    agents::{self, Needed},
};
use serde::Serialize;
use serde_json::Value;
use std::{
    fs::{self, OpenOptions},
    path::{Path, PathBuf},
    process::Stdio,
    time::Duration,
};
use tokio::process::Command;

/// A catalog row HarnessMesh installs by itself, at its pinned version.
#[derive(Clone, Copy)]
pub struct Recommendation {
    pub harness: HarnessKind,
    pub version: &'static str,
    /// Oldest Node.js the package declares it runs on.
    pub node: &'static str,
    /// The package's `bin` entry for its ACP command.
    pub entry: &'static str,
    /// A command the adapter starts, which must already be installed.
    pub needs: Option<Needed>,
    /// Rough size once installed, for packages large enough to mention.
    pub download_mb: Option<u32>,
    /// How long `npm install` may take.
    pub timeout: Duration,
    home_variable: Option<&'static str>,
}

/// Every row of `catalog` HarnessMesh installs by itself, in install-list order.
pub fn recommended(catalog: &[&'static AgentSpec]) -> Vec<Recommendation> {
    agents::installable(catalog)
        .into_iter()
        .filter_map(Recommendation::of)
        .collect()
}

const TOOL_TIMEOUT: Duration = Duration::from_secs(8);
/// Written next to a managed installation that runs with a Node.js other
/// than the first one on PATH. It holds only that path.
const RUNTIME_FILE: &str = "hmesh-runtime.json";

impl Recommendation {
    pub fn of(spec: &'static AgentSpec) -> Option<Self> {
        let install = spec.install?;
        Some(Self {
            harness: HarnessKind::of(spec),
            version: install.version,
            node: install.node,
            entry: spec.entry?,
            needs: install.needs,
            download_mb: install.download_mb,
            timeout: Duration::from_secs(install.timeout_s),
            home_variable: install.home_variable,
        })
    }
    pub fn package(self) -> &'static str {
        self.harness
            .spec()
            .package
            .expect("recommended ACP package")
    }
    /// How the panel offers it.
    pub fn offer(self) -> Value {
        serde_json::json!({"harness":self.harness,"package":self.package(),"version":self.version,"node":self.node,"needs":self.needs,"download_mb":self.download_mb,"timeout_s":self.timeout.as_secs()})
    }
    fn destination(self, root: &Path) -> PathBuf {
        root.join(self.harness.to_string()).join(self.version)
    }
    fn display_name(self) -> &'static str {
        self.harness.spec().label
    }
    // The pinned packages keep the same layout on every platform.
    fn detect(self, directory: &Path) -> Option<DetectedAgent> {
        let package_dir = directory.join("node_modules").join(self.package());
        let package: Value =
            serde_json::from_slice(&fs::read(package_dir.join("package.json")).ok()?).ok()?;
        let command = self.harness.spec().command?;
        if package["name"] != self.package()
            || package["version"] != self.version
            || package["bin"][command] != self.entry
        {
            return None;
        }
        let entry = package_dir.join(self.entry);
        entry.is_file().then(|| DetectedAgent {
            harness: self.harness,
            command: command.into(),
            executable: entry,
            node: recorded_node(directory),
        })
    }
}

fn recorded_node(directory: &Path) -> Option<PathBuf> {
    let runtime: Value =
        serde_json::from_slice(&fs::read(directory.join(RUNTIME_FILE)).ok()?).ok()?;
    let node = PathBuf::from(runtime["node"].as_str()?);
    node.is_absolute().then_some(node)
}

/// The Node.js recorded for an installation is no longer there.
fn lost_node(agent: &DetectedAgent) -> bool {
    agent.node.as_deref().is_some_and(|node| !node.is_file())
}

/// Run an installation with `node` from now on, and record it.
fn use_node(directory: &Path, mut agent: DetectedAgent, node: ChosenNode) -> Result<DetectedAgent> {
    agent.node = node.recorded.then_some(node.path);
    record_node(directory, agent.node.as_deref())?;
    Ok(agent)
}

fn record_node(directory: &Path, node: Option<&Path>) -> Result<()> {
    let file = directory.join(RUNTIME_FILE);
    match node {
        Some(node) => fs::write(
            file,
            serde_json::to_vec(&serde_json::json!({ "node": node }))?,
        )?,
        None if file.exists() => fs::remove_file(file)?,
        None => {}
    }
    Ok(())
}

pub fn detect(data_dir: &Path) -> Vec<DetectedAgent> {
    detect_in(data_dir, CATALOG, std::env::var_os("PATH"))
}

/// The agents of `catalog` on `path` (a `PATH` value), then those installed
/// under `data_dir` that are not on it.
pub fn detect_in(
    data_dir: &Path,
    catalog: &[&'static AgentSpec],
    path: Option<std::ffi::OsString>,
) -> Vec<DetectedAgent> {
    let mut agents = agents::detect_on_path(catalog, path);
    if let Ok(dir) = fs::canonicalize(data_dir) {
        for agent in managed(&native_path(&dir).join("agent-runtimes"), catalog) {
            if !agents.iter().any(|a| a.harness == agent.harness) {
                agents.push(agent);
            }
        }
    }
    agents
}

/// Managed installations that can run. One whose recorded Node.js has gone
/// is left out, so the panel offers to install it again and `install`
/// chooses a Node.js anew.
fn managed(root: &Path, catalog: &[&'static AgentSpec]) -> Vec<DetectedAgent> {
    recommended(catalog)
        .iter()
        .filter_map(|pkg| pkg.detect(&pkg.destination(root)))
        .filter(|agent| agent.node.as_deref().is_none_or(Path::is_file))
        .collect()
}

#[derive(Serialize)]
pub struct Installation {
    pub agent: DetectedAgent,
    pub source: &'static str,
    pub protocol_probe: &'static str,
}

async fn probe(agent: DetectedAgent, source: &'static str) -> Result<Installation> {
    let cwd = tempfile::tempdir()?;
    let mut command = hmesh_adapters::resolve_executable_with_node(
        agent.harness,
        &agent.executable,
        agent.node.as_deref(),
    )?
    .command();
    command
        .args(agent.harness.spec().args)
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
    path_lookup_all(name).into_iter().next()
}

/// Every file called `name` in PATH's absolute directories, in PATH order.
fn path_lookup_all(name: &str) -> Vec<PathBuf> {
    let Some(paths) = std::env::var_os("PATH") else {
        return vec![];
    };
    std::env::split_paths(&paths)
        .filter(|path| path.is_absolute())
        .map(|path| path.join(name))
        .filter(|path| path.is_file())
        .collect()
}

const NODE: &str = if cfg!(windows) { "node.exe" } else { "node" };

type Version = (u64, u64, u64);

/// `v24.22.1`, `24.9` or `0.83.0`; pre-release and build suffixes are ignored.
fn parse_version(text: &str) -> Option<Version> {
    let text = text.trim().trim_start_matches('v');
    let mut parts = text.split(['-', '+']).next()?.split('.');
    let major = parts.next()?.parse().ok()?;
    let minor = parts.next().map_or(Some(0), |p| p.parse().ok())?;
    let patch = parts.next().map_or(Some(0), |p| p.parse().ok())?;
    Some((major, minor, patch))
}

fn show(version: Version) -> String {
    format!("{}.{}.{}", version.0, version.1, version.2)
}

/// The first line of `program --version`, when it runs and reads as a version.
async fn tool_version(program: &Path) -> Option<Version> {
    let mut command = Command::new(program);
    command
        .arg("--version")
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .kill_on_drop(true);
    #[cfg(windows)]
    command.creation_flags(0x08000000);
    let output = tokio::time::timeout(TOOL_TIMEOUT, command.output())
        .await
        .ok()?
        .ok()?;
    if !output.status.success() {
        return None;
    }
    parse_version(std::str::from_utf8(&output.stdout).ok()?.lines().next()?)
}

/// The Node.js an adapter is installed and run with.
#[derive(Debug)]
struct ChosenNode {
    path: PathBuf,
    /// It is not the first `node` on PATH, so the agent records it.
    recorded: bool,
}

/// The first `node` on PATH when it is new enough, as before. Otherwise the
/// newest one that is, from the rest of PATH and the places version managers
/// keep their builds.
async fn choose_node(pkg: Recommendation) -> Result<ChosenNode> {
    let minimum = parse_version(pkg.node).context("invalid recommended Node.js version")?;
    let on_path = path_lookup_all(NODE);
    let Some(first) = on_path.first() else {
        return pick_node(pkg, minimum, None, vec![]);
    };
    let first = (first.clone(), tool_version(first).await);
    if first.1.is_some_and(|v| v >= minimum) {
        return pick_node(pkg, minimum, Some(first), vec![]);
    }
    let mut seen: Vec<PathBuf> = fs::canonicalize(&first.0).into_iter().collect();
    let mut others = vec![];
    for path in on_path.into_iter().skip(1).chain(known_node_locations()) {
        if others.len() == 64 {
            break;
        }
        let Ok(real) = fs::canonicalize(&path) else {
            continue;
        };
        if seen.contains(&real) {
            continue;
        }
        seen.push(real);
        let version = tool_version(&path).await;
        others.push((path, version));
    }
    pick_node(pkg, minimum, Some(first), others)
}

fn pick_node(
    pkg: Recommendation,
    minimum: Version,
    first: Option<(PathBuf, Option<Version>)>,
    others: Vec<(PathBuf, Option<Version>)>,
) -> Result<ChosenNode> {
    if let Some((path, Some(version))) = &first
        && *version >= minimum
    {
        return Ok(ChosenNode {
            path: path.clone(),
            recorded: false,
        });
    }
    fn usable(found: &(PathBuf, Option<Version>)) -> Option<(&PathBuf, Version)> {
        found.1.map(|version| (&found.0, version))
    }
    if let Some((path, _)) = others
        .iter()
        .filter_map(usable)
        .filter(|(_, v)| *v >= minimum)
        .max_by_key(|(_, v)| *v)
    {
        return Ok(ChosenNode {
            path: path.clone(),
            recorded: true,
        });
    }
    match first
        .iter()
        .chain(&others)
        .filter_map(usable)
        .max_by_key(|(_, v)| *v)
    {
        Some((path, version)) => bail!(
            "{} needs Node.js {} or newer; the newest Node.js found is {} at {}. Install Node.js {} or newer and retry",
            pkg.display_name(),
            pkg.node,
            show(version),
            path.display(),
            pkg.node
        ),
        None => bail!(
            "Node.js is required for this adapter; install Node.js {} or newer and restart HarnessMesh",
            pkg.node
        ),
    }
}

/// Where version managers and installers commonly keep Node.js builds:
/// each place is a directory with one child per version, a filter on the
/// child's name and the path to `node` inside it.
fn known_node_locations() -> Vec<PathBuf> {
    let env = |name: &str| {
        std::env::var_os(name)
            .filter(|value| !value.is_empty())
            .map(PathBuf::from)
    };
    let under =
        |base: &Path, parts: &[&str]| parts.iter().fold(base.to_path_buf(), |p, s| p.join(s));
    let mut files: Vec<PathBuf> = vec![];
    let mut places: Vec<(PathBuf, &str, &[&str])> = vec![];
    if cfg!(windows) {
        if let Some(dir) = env("ProgramFiles") {
            files.push(under(&dir, &["nodejs", "node.exe"]));
        }
        if let Some(dir) = env("NVM_HOME").or_else(|| env("APPDATA").map(|d| d.join("nvm"))) {
            places.push((dir, "v", &["node.exe"]));
        }
        if let Some(dir) = env("LOCALAPPDATA") {
            places.push((
                under(&dir, &["Volta", "tools", "image", "node"]),
                "",
                &["node.exe"],
            ));
        }
        if let Some(dir) = env("APPDATA") {
            places.push((
                under(&dir, &["fnm", "node-versions"]),
                "",
                &["installation", "node.exe"],
            ));
        }
    } else if let Some(home) = env("HOME") {
        let nvm = env("NVM_DIR").unwrap_or_else(|| home.join(".nvm"));
        places.push((under(&nvm, &["versions", "node"]), "", &["bin", "node"]));
        let fnm = [
            env("FNM_DIR"),
            Some(under(&home, &[".local", "share", "fnm"])),
            Some(under(&home, &["Library", "Application Support", "fnm"])),
            Some(home.join(".fnm")),
        ];
        for dir in fnm.into_iter().flatten() {
            places.push((
                dir.join("node-versions"),
                "",
                &["installation", "bin", "node"],
            ));
        }
        let volta = env("VOLTA_HOME").unwrap_or_else(|| home.join(".volta"));
        places.push((
            under(&volta, &["tools", "image", "node"]),
            "",
            &["bin", "node"],
        ));
        let asdf = env("ASDF_DATA_DIR").unwrap_or_else(|| home.join(".asdf"));
        places.push((under(&asdf, &["installs", "nodejs"]), "", &["bin", "node"]));
        let mise =
            env("MISE_DATA_DIR").unwrap_or_else(|| under(&home, &[".local", "share", "mise"]));
        places.push((under(&mise, &["installs", "node"]), "", &["bin", "node"]));
        // Pi's installer keeps its own Node.js builds here.
        places.push((
            under(&home, &[".local", "share", "pi-node"]),
            "node-",
            &["bin", "node"],
        ));
        let n = env("N_PREFIX").unwrap_or_else(|| PathBuf::from("/usr/local"));
        places.push((under(&n, &["n", "versions", "node"]), "", &["bin", "node"]));
        for brew in ["/opt/homebrew/opt", "/usr/local/opt"] {
            places.push((PathBuf::from(brew), "node", &["bin", "node"]));
        }
    }
    for (dir, prefix, inside) in places {
        let Ok(entries) = fs::read_dir(&dir) else {
            continue;
        };
        let mut names: Vec<_> = entries
            .filter_map(|entry| entry.ok()?.file_name().into_string().ok())
            .filter(|name| name.starts_with(prefix))
            .collect();
        names.sort_unstable_by(|a, b| b.cmp(a));
        files.extend(names.iter().map(|name| under(&dir.join(name), inside)));
    }
    files.retain(|path| path.is_file());
    files
}

/// npm for `node`: its own copy for a Node.js chosen off PATH, otherwise the
/// npm on PATH first, as before.
fn npm_entry(node: &Path, recorded: bool) -> Result<PathBuf> {
    let on_path = || {
        let entry = fs::canonicalize(path_lookup("npm")?).ok()?;
        entry
            .extension()
            .is_some_and(|ext| ext == "js")
            .then(|| native_path(&entry))
    };
    let beside = || {
        let real = fs::canonicalize(node).ok();
        [Some(node), real.as_deref()]
            .into_iter()
            .flatten()
            .filter_map(Path::parent)
            .flat_map(|dir| {
                [
                    dir.join("node_modules/npm/bin/npm-cli.js"),
                    dir.join("../lib/node_modules/npm/bin/npm-cli.js"),
                ]
            })
            .find(|entry| entry.is_file())
    };
    if recorded {
        beside().or_else(on_path)
    } else {
        on_path().or_else(beside)
    }
    .context("npm CLI was not found alongside Node.js or on PATH")
}

/// A command the adapter starts must be on PATH, and new enough when its
/// version can be read.
async fn check_needed(pkg: Recommendation) -> Result<()> {
    let Some(needed) = pkg.needs else {
        return Ok(());
    };
    let names: Vec<String> = if cfg!(windows) {
        [".cmd", ".exe", ""]
            .iter()
            .map(|suffix| format!("{}{suffix}", needed.command))
            .collect()
    } else {
        vec![needed.command.into()]
    };
    let program = names.iter().find_map(|name| path_lookup(name));
    // A Windows .cmd shim needs a shell to run, so only its presence counts.
    let version = match &program {
        Some(program) if !cfg!(windows) => tool_version(program).await,
        _ => None,
    };
    needed_result(pkg, needed, program.is_some(), version)
}

fn needed_result(
    pkg: Recommendation,
    needed: Needed,
    present: bool,
    version: Option<Version>,
) -> Result<()> {
    ensure!(
        present,
        "{} starts the {} command, which is not on PATH; install {} first ({}) and retry",
        pkg.package(),
        needed.command,
        pkg.display_name(),
        needed.install
    );
    let minimum = parse_version(needed.version).context("invalid required version")?;
    if let Some(version) = version {
        ensure!(
            version >= minimum,
            "{} needs {} {} or newer; found {}. Update it ({}) and retry",
            pkg.package(),
            needed.command,
            needed.version,
            show(version),
            needed.install
        );
    }
    Ok(())
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

async fn download(pkg: Recommendation, stage: &Path, node: &ChosenNode) -> Result<()> {
    let npm = npm_entry(&node.path, node.recorded)?;
    let home_config = stage.join("npm-user.conf");
    let global_config = stage.join("npm-global.conf");
    fs::write(&home_config, "")?;
    fs::write(&global_config, "")?;
    let mut command = Command::new(&node.path);
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
    let status = tokio::time::timeout(pkg.timeout, command.status())
        .await
        .with_context(|| {
            format!(
                "npm install did not finish within {} minutes; check connectivity to registry.npmjs.org",
                pkg.timeout.as_secs().div_ceil(60)
            )
        })?
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
    let Some(pkg) = Recommendation::of(harness.spec()) else {
        bail!("this agent does not have a managed adapter package");
    };
    if let Some(variable) = pkg.home_variable
        && let Some(home) = std::env::var_os(variable).filter(|path| !path.is_empty())
    {
        let home = PathBuf::from(home);
        ensure!(
            home.is_absolute() && home.is_dir(),
            "{variable} must refer to an existing absolute directory; correct it where HarnessMesh starts"
        );
    }
    check_needed(pkg).await?;
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
    if let Some(mut agent) = pkg.detect(&target) {
        if lost_node(&agent) {
            // The Node.js it was installed with has gone: choose one again.
            agent = use_node(&target, agent, choose_node(pkg).await?)?;
        }
        return probe(agent, "managed").await;
    }
    ensure!(
        !target.exists(),
        "the managed runtime directory is incomplete; inspect or move it before retrying"
    );
    let node = choose_node(pkg).await?;
    let stage = tempfile::Builder::new()
        .prefix("install-")
        .tempdir_in(parent)?;
    // An explicit manifest makes npm install into this temporary directory,
    // even when the HarnessMesh instance resides inside another npm project.
    fs::write(stage.path().join("package.json"), r#"{"private":true}"#)?;
    download(pkg, stage.path(), &node).await?;
    record_node(stage.path(), node.recorded.then_some(node.path.as_path()))?;
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

#[cfg(test)]
mod tests {
    use super::*;

    fn pkg(harness: HarnessKind) -> Recommendation {
        recommended(CATALOG)
            .into_iter()
            .find(|p| p.harness == harness)
            .unwrap()
    }

    fn write_package(root: &Path, pkg: Recommendation, entry: &str) -> PathBuf {
        let dir = pkg.destination(root);
        let package_dir = dir.join("node_modules").join(pkg.package());
        let entry_file = package_dir.join(entry);
        fs::create_dir_all(entry_file.parent().unwrap()).unwrap();
        fs::write(&entry_file, "").unwrap();
        let command = pkg.harness.spec().command.unwrap();
        let manifest = serde_json::json!({"name": pkg.package(), "version": pkg.version, "bin": {command: entry}});
        fs::write(package_dir.join("package.json"), manifest.to_string()).unwrap();
        dir
    }

    #[test]
    fn versions_read_like_node_and_pi_print_them() {
        assert_eq!(parse_version("v24.22.1\n"), Some((24, 22, 1)));
        assert_eq!(parse_version("24.9"), Some((24, 9, 0)));
        assert_eq!(parse_version("0.83.0"), Some((0, 83, 0)));
        assert_eq!(parse_version("v25.0.0-nightly20261101"), Some((25, 0, 0)));
        assert_eq!(parse_version("node"), None);
        assert!(parse_version("24.10.0") > parse_version("24.9"));
    }

    #[test]
    fn pi_and_dsh_are_offered_with_their_own_entries() {
        let offered: Vec<_> = recommended(CATALOG).iter().map(|p| p.harness).collect();
        assert_eq!(
            offered,
            [
                HarnessKind::Codex,
                HarnessKind::ClaudeCode,
                HarnessKind::Pi,
                HarnessKind::Dsh
            ]
        );
        assert_eq!(pkg(HarnessKind::Pi).package(), "pi-acp");
        assert_eq!(pkg(HarnessKind::Dsh).entry, "lib/bin.js");
        assert!(pkg(HarnessKind::Dsh).timeout > Duration::from_secs(300));
        for pkg in recommended(CATALOG) {
            assert!(parse_version(pkg.node).is_some(), "{}", pkg.node);
        }
    }

    #[test]
    fn a_managed_install_is_found_by_its_own_entry_and_recorded_node() {
        let root = tempfile::tempdir().unwrap();
        let dsh = pkg(HarnessKind::Dsh);
        let dir = write_package(root.path(), dsh, "lib/bin.js");
        let node = root.path().join("node24");
        fs::write(&node, "").unwrap();
        record_node(&dir, Some(&node)).unwrap();
        let found = dsh.detect(&dir).unwrap();
        assert_eq!(
            found.executable,
            dir.join("node_modules/@deepseek-ai/dsh/lib/bin.js")
        );
        assert_eq!(found.node.as_deref(), Some(node.as_path()));
        assert_eq!(managed(root.path(), CATALOG).len(), 1);

        // A Node.js that has gone leaves the install for `install` to repair.
        fs::remove_file(&node).unwrap();
        assert!(dsh.detect(&dir).is_some());
        assert!(managed(root.path(), CATALOG).is_empty());

        // Without a record it runs with the first node on PATH.
        record_node(&dir, None).unwrap();
        assert!(!dir.join(RUNTIME_FILE).exists());
        assert_eq!(dsh.detect(&dir).unwrap().node, None);
    }

    #[test]
    fn a_lost_node_is_replaced_and_recorded_again() {
        let root = tempfile::tempdir().unwrap();
        let dsh = pkg(HarnessKind::Dsh);
        let dir = write_package(root.path(), dsh, "lib/bin.js");
        record_node(&dir, Some(&root.path().join("removed-node"))).unwrap();
        let agent = dsh.detect(&dir).unwrap();
        assert!(lost_node(&agent));
        let newer = root.path().join("node24");
        fs::write(&newer, "").unwrap();
        let chosen = ChosenNode {
            path: newer.clone(),
            recorded: true,
        };
        let agent = use_node(&dir, agent, chosen).unwrap();
        assert!(!lost_node(&agent));
        assert_eq!(agent.node.as_deref(), Some(newer.as_path()));
        assert_eq!(dsh.detect(&dir).unwrap().node, Some(newer));
        // The first node on PATH is enough again: the record goes.
        let path_node = ChosenNode {
            path: PathBuf::from("/path/node"),
            recorded: false,
        };
        let agent = use_node(&dir, agent, path_node).unwrap();
        assert_eq!(agent.node, None);
        assert!(!dir.join(RUNTIME_FILE).exists());
    }

    #[test]
    fn an_entry_other_than_the_pinned_one_is_not_accepted() {
        let root = tempfile::tempdir().unwrap();
        let dsh = pkg(HarnessKind::Dsh);
        let dir = write_package(root.path(), dsh, "dist/index.js");
        assert!(dsh.detect(&dir).is_none());
        let pi = pkg(HarnessKind::Pi);
        let dir = write_package(root.path(), pi, "dist/index.js");
        assert_eq!(pi.detect(&dir).unwrap().command, "pi-acp");
    }

    #[test]
    fn the_first_node_on_path_is_kept_when_it_is_new_enough() {
        let first = (PathBuf::from("/path/node"), Some((22, 23, 2)));
        let chosen = pick_node(pkg(HarnessKind::Pi), (20, 0, 0), Some(first), vec![]).unwrap();
        assert_eq!(chosen.path, Path::new("/path/node"));
        assert!(!chosen.recorded);
    }

    #[test]
    fn otherwise_the_newest_node_that_is_new_enough_is_recorded() {
        let first = (PathBuf::from("/path/node"), Some((22, 23, 2)));
        let others = vec![
            (PathBuf::from("/nvm/24.9.0/bin/node"), Some((24, 9, 0))),
            (PathBuf::from("/broken/node"), None),
            (
                PathBuf::from("/pi-node/24.22.1/bin/node"),
                Some((24, 22, 1)),
            ),
            (PathBuf::from("/old/node"), Some((18, 0, 0))),
        ];
        let chosen = pick_node(pkg(HarnessKind::Dsh), (24, 9, 0), Some(first), others).unwrap();
        assert_eq!(chosen.path, Path::new("/pi-node/24.22.1/bin/node"));
        assert!(chosen.recorded);
    }

    #[test]
    fn no_node_new_enough_names_the_minimum_and_the_newest_found() {
        let first = (PathBuf::from("/path/node"), Some((22, 23, 2)));
        let others = vec![(PathBuf::from("/nvm/22.15.0/bin/node"), Some((22, 15, 0)))];
        let error = pick_node(pkg(HarnessKind::Dsh), (24, 9, 0), Some(first), others)
            .unwrap_err()
            .to_string();
        assert!(
            error.contains("DeepSeek Harness needs Node.js 24.9 or newer"),
            "{error}"
        );
        assert!(error.contains("22.23.2 at /path/node"), "{error}");
        let error = pick_node(pkg(HarnessKind::Dsh), (24, 9, 0), None, vec![])
            .unwrap_err()
            .to_string();
        assert!(error.contains("install Node.js 24.9 or newer"), "{error}");
    }

    #[test]
    fn pi_must_be_installed_before_its_adapter() {
        let pi = pkg(HarnessKind::Pi);
        let needed = pi.needs.unwrap();
        let error = needed_result(pi, needed, false, None)
            .unwrap_err()
            .to_string();
        assert!(error.contains("install Pi first"), "{error}");
        assert!(error.contains("@earendil-works/pi-coding-agent"), "{error}");
        let error = needed_result(pi, needed, true, Some((0, 80, 2)))
            .unwrap_err()
            .to_string();
        assert!(
            error.contains("needs pi 0.81 or newer; found 0.80.2"),
            "{error}"
        );
        needed_result(pi, needed, true, Some((0, 83, 0))).unwrap();
        // A version that cannot be read does not block the install.
        needed_result(pi, needed, true, None).unwrap();
        assert!(pkg(HarnessKind::Dsh).needs.is_none());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn a_version_is_read_from_the_first_line_a_tool_prints() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let tool = dir.path().join("node");
        fs::write(&tool, "#!/bin/sh\necho v24.10.0\necho more\n").unwrap();
        fs::set_permissions(&tool, fs::Permissions::from_mode(0o755)).unwrap();
        assert_eq!(tool_version(&tool).await, Some((24, 10, 0)));
        fs::write(&tool, "#!/bin/sh\nexit 3\n").unwrap();
        assert_eq!(tool_version(&tool).await, None);
    }

    #[cfg(unix)]
    #[test]
    fn npm_beside_a_chosen_node_is_preferred() {
        let dir = tempfile::tempdir().unwrap();
        let node = dir.path().join("bin/node");
        let npm = dir.path().join("lib/node_modules/npm/bin/npm-cli.js");
        fs::create_dir_all(node.parent().unwrap()).unwrap();
        fs::create_dir_all(npm.parent().unwrap()).unwrap();
        fs::write(&node, "").unwrap();
        fs::write(&npm, "").unwrap();
        let found = npm_entry(&node, true).unwrap();
        assert_eq!(
            fs::canonicalize(found).unwrap(),
            fs::canonicalize(npm).unwrap()
        );
    }

    /// A made-up agent: one catalog row, and nothing else here names it.
    static EXAMPLE: AgentSpec = AgentSpec {
        kind: "example-agent",
        label: "Example Agent",
        command: Some("example-acp"),
        args: &["acp"],
        package: Some("@example/agent"),
        entry: Some("bin/acp.js"),
        install: Some(agents::Install {
            version: "1.2.3",
            node: "20.0",
            needs: None,
            download_mb: None,
            timeout_s: 120,
            order: 9,
            home_variable: None,
        }),
        ..agents::BLANK
    };

    #[test]
    fn one_more_row_is_one_more_agent() {
        let catalog: Vec<&'static AgentSpec> = CATALOG.iter().copied().chain([&EXAMPLE]).collect();
        let example = HarnessKind::of(&EXAMPLE);

        // Found on PATH, after the agents found before it.
        let bin = tempfile::tempdir().unwrap();
        for command in ["codex-acp", "example-acp"] {
            fs::write(bin.path().join(command), "").unwrap();
        }
        let on_path = agents::detect_on_path(&catalog, Some(bin.path().into()));
        let found: Vec<_> = on_path
            .iter()
            .map(|a| (a.harness, a.command.as_str()))
            .collect();
        assert_eq!(
            found,
            [(HarnessKind::Codex, "codex-acp"), (example, "example-acp")]
        );
        assert_eq!(on_path[1].executable, bin.path().join("example-acp"));
        // The built-in catalog does not have it.
        assert_eq!(
            agents::detect_on_path(CATALOG, Some(bin.path().into())).len(),
            1
        );

        // Offered for install after the original four, as the panel lists it.
        let offered: Vec<_> = recommended(&catalog).iter().map(|p| p.harness).collect();
        assert_eq!(
            offered,
            [
                HarnessKind::Codex,
                HarnessKind::ClaudeCode,
                HarnessKind::Pi,
                HarnessKind::Dsh,
                example
            ]
        );
        let offer = Recommendation::of(&EXAMPLE).unwrap().offer();
        assert_eq!(
            offer,
            serde_json::json!({"harness":"example-agent","package":"@example/agent","version":"1.2.3","node":"20.0","needs":null,"download_mb":null,"timeout_s":120})
        );

        // A managed install of it is found by its own entry.
        let data = tempfile::tempdir().unwrap();
        let root = native_path(&fs::canonicalize(data.path()).unwrap()).join("agent-runtimes");
        let dir = write_package(&root, Recommendation::of(&EXAMPLE).unwrap(), "bin/acp.js");
        let managed = detect_in(data.path(), &catalog, None);
        assert_eq!(managed.len(), 1);
        assert_eq!(managed[0].harness, example);
        assert_eq!(
            managed[0].executable,
            dir.join("node_modules/@example/agent/bin/acp.js")
        );
        assert!(detect_in(data.path(), CATALOG, None).is_empty());

        // Registered, it starts with its own ACP arguments.
        let mut config: hmesh_core::AgentConfig = serde_json::from_value(serde_json::json!({
            "agent":{"name":"example","harness":"acp","project_id":"demo"},
            "project":{"root":"/tmp","allowed_tools":["read"]},
            "adapter":{"executable":"/usr/bin/true"}
        }))
        .unwrap();
        config.agent.harness = example;
        assert_eq!(config.launch_args(), ["acp"]);

        // The onboarding dialog lists it, and a Windows npm shim leads to its entry.
        assert_eq!(
            EXAMPLE.onboarding_view(),
            serde_json::json!({"kind":"example-agent","label":"Example Agent","command":"example-acp","custom":false})
        );
        assert_eq!(
            EXAMPLE.shim_entry().as_deref(),
            Some("node_modules/@example/agent/bin/acp.js")
        );

        // Other computers see generic ACP; which agent it is goes in a note.
        assert_eq!(example.family(), HarnessKind::Acp);
        assert!(hmesh_core::service::protocols_for(example).is_empty());
        let id = uuid::Uuid::new_v4();
        assert_eq!(
            hmesh_core::AgentKindNote::of(id, example),
            Some(hmesh_core::AgentKindNote {
                agent_id: id,
                kind: "example-agent".into(),
                label: "Example Agent".into()
            })
        );
        for spec in CATALOG {
            assert_eq!(
                hmesh_core::AgentKindNote::of(id, HarnessKind::of(spec)),
                None
            );
        }
    }
}
