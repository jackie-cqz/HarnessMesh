use anyhow::{Context, Result, bail};
use hmesh_security::{Instance, atomic_write, file_sha256, secure_directory};
use rusqlite::{Connection, OpenFlags};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
};

const FILES: [&str; 7] = [
    "identity.json",
    "identity.pk8",
    "certificate.pem",
    "owner.token",
    "node.toml",
    "policy.toml",
    "state.sqlite3",
];
const OPTIONAL: [&str; 2] = ["startup.json", "login-task.xml"];
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Manifest {
    format: u32,
    node_id: uuid::Uuid,
    original_path: PathBuf,
    schema: i64,
    files: BTreeMap<String, String>,
}
fn regular(path: &Path) -> Result<()> {
    let metadata = fs::symlink_metadata(path)?;
    if !metadata.is_file() || metadata.file_type().is_symlink() {
        bail!("backup inputs must be regular files");
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        if metadata.file_attributes() & 0x400 != 0 {
            bail!("backup reparse points are forbidden");
        }
    }
    Ok(())
}
fn private_new(path: &Path) -> Result<()> {
    #[cfg(unix)]
    let mut builder = fs::DirBuilder::new();
    #[cfg(not(unix))]
    let builder = fs::DirBuilder::new();
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        builder.mode(0o700);
    }
    builder
        .create(path)
        .context("destination must not already exist")?;
    if let Err(error) = secure_directory(path) {
        let _ = fs::remove_dir(path);
        return Err(error);
    }
    Ok(())
}
fn budget(name: &str) -> u64 {
    if name == "state.sqlite3" {
        10 * 1024 * 1024 * 1024
    } else {
        1024 * 1024
    }
}
fn private_copy(source: &Path, target: &Path) -> Result<()> {
    use std::io::Read;
    let maximum = budget(
        source
            .file_name()
            .and_then(|n| n.to_str())
            .context("invalid backup filename")?,
    );
    if fs::metadata(source)?.len() > maximum {
        bail!("backup input exceeds file budget");
    }
    let mut options = fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut output = options.open(target)?;
    let mut input = fs::File::open(source)?.take(maximum + 1);
    if std::io::copy(&mut input, &mut output)? > maximum {
        bail!("backup input grew beyond file budget");
    }
    output.sync_all()?;
    Ok(())
}
fn db_check(path: &Path) -> Result<i64> {
    regular(path)?;
    let db = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY)?;
    let integrity: String = db.query_row("PRAGMA integrity_check", [], |r| r.get(0))?;
    if integrity != "ok" {
        bail!("database integrity check failed");
    }
    let foreign: bool = db.prepare("PRAGMA foreign_key_check")?.exists([])?;
    if foreign {
        bail!("database foreign key check failed");
    }
    let version: i64 = db.pragma_query_value(None, "user_version", |r| r.get(0))?;
    if !(1..=7).contains(&version) {
        bail!("backup database schema is unsupported by this binary");
    }
    Ok(version)
}
pub fn create(node: &Path, destination: &Path) -> Result<Value> {
    for name in FILES {
        let path = node.join(name);
        if path.exists() {
            regular(&path)?;
            if fs::metadata(path)?.len() > budget(name) {
                bail!("backup input exceeds file budget: {name}");
            }
        }
    }
    let instance = Instance::load(node)?;
    let _lock = instance
        .lock_daemon()
        .context("stop the Daemon before backing up; it was not interrupted")?;
    let dest = absolute_new(destination)?;
    if dest.starts_with(&instance.dir) {
        bail!("backup must be outside the instance directory");
    }
    private_new(&dest)?;
    let result = (|| -> Result<Value> {
        let mut names: Vec<&str> = FILES.to_vec();
        for name in OPTIONAL {
            if instance.dir.join(name).exists() {
                regular(&instance.dir.join(name))?;
                private_copy(&instance.dir.join(name), &dest.join(name))?;
                names.push(name);
            }
        }
        for name in &FILES[..4] {
            regular(&instance.dir.join(name))?;
            atomic_write(&dest.join(name), &fs::read(instance.dir.join(name))?)?;
        }
        let config = instance.config.clone();
        let policy = if config.security.policy_file.is_absolute() {
            config.security.policy_file.clone()
        } else {
            instance.dir.join(&config.security.policy_file)
        };
        regular(&policy)?;
        if fs::metadata(&policy)?.len() > budget("policy.toml") {
            bail!("effective policy exceeds backup budget");
        }
        let policy_bytes = fs::read(policy)?;
        let policy: hmesh_core::Policy = toml::from_str(std::str::from_utf8(&policy_bytes)?)?;
        policy.validate()?;
        atomic_write(&dest.join("policy.toml"), &policy_bytes)?;
        // Preserve the source config's fields so a pre-upgrade backup remains readable by its older binary.
        let mut config_document: toml::Value =
            toml::from_str(&fs::read_to_string(instance.dir.join("node.toml"))?)?;
        config_document["security"]["policy_file"] = toml::Value::String("policy.toml".into());
        atomic_write(
            &dest.join("node.toml"),
            toml::to_string_pretty(&config_document)?.as_bytes(),
        )?;
        let source = instance.dir.join("state.sqlite3");
        if source.exists() {
            db_check(&source)?;
            let source = Connection::open_with_flags(source, OpenFlags::SQLITE_OPEN_READ_ONLY)?;
            let mut target = Connection::open(dest.join("state.sqlite3"))?;
            rusqlite::backup::Backup::new(&source, &mut target)?.run_to_completion(
                128,
                std::time::Duration::from_millis(1),
                None,
            )?;
        } else {
            drop(hmesh_store::Store::open(&dest.join("state.sqlite3"))?);
        }
        // Persist the sealed DB, then publish a manifest last. External Harness files are only references.
        fs::OpenOptions::new()
            .write(true)
            .open(dest.join("state.sqlite3"))?
            .sync_all()?;
        let schema = db_check(&dest.join("state.sqlite3"))?;
        let files = names
            .iter()
            .map(|name| {
                Ok((
                    name.to_string(),
                    file_sha256(&dest.join(name), budget(name))?,
                ))
            })
            .collect::<Result<BTreeMap<_, _>>>()?;
        let manifest = Manifest {
            format: 1,
            node_id: instance.card.node_id,
            original_path: instance.dir.clone(),
            schema,
            files,
        };
        atomic_write(
            &dest.join("manifest.json"),
            &serde_json::to_vec_pretty(&manifest)?,
        )?;
        check(&dest)?;
        Ok(
            json!({"backup":dest,"node_id":instance.card.node_id,"schema":schema,"contains_private_identity":true,"external_harness_credentials_copied":false}),
        )
    })();
    if result.is_err() {
        let _ = fs::remove_dir_all(&dest);
    }
    result
}
fn manifest(path: &Path) -> Result<Manifest> {
    regular(&path.join("manifest.json"))?;
    if fs::metadata(path.join("manifest.json"))?.len() > 65536 {
        bail!("manifest too large");
    }
    let manifest: Manifest = serde_json::from_slice(&fs::read(path.join("manifest.json"))?)?;
    if manifest.format != 1
        || !(FILES.len()..=FILES.len() + OPTIONAL.len()).contains(&manifest.files.len())
        || FILES.iter().any(|n| !manifest.files.contains_key(*n))
        || manifest
            .files
            .keys()
            .any(|n| !FILES.contains(&n.as_str()) && !OPTIONAL.contains(&n.as_str()))
    {
        bail!("invalid backup allowlist");
    }
    Ok(manifest)
}
pub fn check(path: &Path) -> Result<Value> {
    let manifest = manifest(path)?;
    for (name, digest) in &manifest.files {
        regular(&path.join(name))?;
        if file_sha256(&path.join(name), budget(name))? != *digest {
            bail!("backup file integrity check failed: {name}");
        }
    }
    let identity = Instance::load(path)?;
    let policy: hmesh_core::Policy =
        toml::from_str(&fs::read_to_string(path.join("policy.toml"))?)?;
    policy.validate()?;
    let schema = db_check(&path.join("state.sqlite3"))?;
    let db =
        Connection::open_with_flags(path.join("state.sqlite3"), OpenFlags::SQLITE_OPEN_READ_ONLY)?;
    if db.query_row(
        "SELECT EXISTS(SELECT 1 FROM nodes WHERE node_id!=?1)",
        [identity.card.node_id.to_string()],
        |r| r.get::<_, bool>(0),
    )? {
        bail!("backup database node identity mismatch");
    }
    if identity.card.node_id != manifest.node_id || schema != manifest.schema {
        bail!("backup identity/schema does not match manifest");
    }
    Ok(
        json!({"passed":true,"node_id":manifest.node_id,"schema":schema,"original_path":manifest.original_path}),
    )
}
fn absolute_new(path: &Path) -> Result<PathBuf> {
    let path = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()?.join(path)
    };
    let parent = fs::canonicalize(path.parent().context("missing parent")?)?;
    let name = path.file_name().context("missing directory name")?;
    let path = parent.join(name);
    if fs::symlink_metadata(&path).is_ok() {
        bail!("destination must be a new directory");
    }
    Ok(path)
}
pub fn restore(node: &Path, backup: &Path) -> Result<Value> {
    check(backup)?;
    let manifest = manifest(backup)?;
    let dest = absolute_new(node)?;
    if dest != manifest.original_path {
        bail!(
            "restore is restricted to the original offline path; cloning a paired identity is forbidden"
        );
    }
    let staging = dest.with_file_name(format!(".hmesh-restore-{}", uuid::Uuid::new_v4()));
    private_new(&staging)?;
    let mut reserved = false;
    let result = (|| -> Result<Value> {
        for name in manifest
            .files
            .keys()
            .filter(|n| n.as_str() != "identity.json")
        {
            private_copy(&backup.join(name), &staging.join(name))?;
            if file_sha256(&staging.join(name), budget(name))? != manifest.files[name] {
                bail!("backup changed while restoring");
            }
        }
        atomic_write(
            &staging.join("identity.json"),
            &fs::read(backup.join("identity.json"))?,
        )?;
        if file_sha256(&staging.join("identity.json"), budget("identity.json"))?
            != manifest.files["identity.json"]
        {
            bail!("backup identity changed while restoring");
        }
        let identity = Instance::load(&staging)?;
        db_check(&staging.join("state.sqlite3"))?;
        if identity.card.node_id != manifest.node_id {
            bail!("restored identity mismatch");
        }
        // Atomically reserve a NEW destination rather than allowing rename to replace an empty directory.
        private_new(&dest)?;
        reserved = true;
        for name in manifest
            .files
            .keys()
            .filter(|n| n.as_str() != "identity.json")
        {
            fs::rename(staging.join(name), dest.join(name))?;
        }
        fs::rename(staging.join("identity.json"), dest.join("identity.json"))?;
        let _ = fs::remove_dir_all(&staging);
        Ok(
            json!({"restored":dest,"node_id":manifest.node_id,"schema":manifest.schema,"daemon_started":false,"external_references_require_owner_check":true}),
        )
    })();
    if result.is_err() {
        let _ = fs::remove_dir_all(staging);
        if reserved {
            let _ = fs::remove_dir_all(&dest);
        }
    }
    result
}
pub fn bundle_check(bundle: &Path) -> Result<Value> {
    regular(&bundle.join("manifest.json"))?;
    if fs::metadata(bundle.join("manifest.json"))?.len() > 65536 {
        bail!("bundle manifest too large");
    }
    let value: Value = serde_json::from_slice(&fs::read(bundle.join("manifest.json"))?)?;
    let version = value["version"]
        .as_str()
        .and_then(|s| s.strip_prefix("hmesh "))
        .context("bundle version is missing")?;
    let numbers: Vec<_> = version.split('-').next().unwrap_or("").split('.').collect();
    if numbers.len() != 3
        || numbers.iter().any(|n| n.parse::<u64>().is_err())
        || value["database_schema"]
            .as_i64()
            .is_none_or(|s| !(1..=7).contains(&s))
    {
        bail!("unsupported version/schema metadata");
    }
    let expected_os = std::env::consts::OS;
    let expected_os = if expected_os == "macos" {
        "darwin"
    } else if expected_os == "windows" {
        "win32"
    } else {
        expected_os
    };
    let arch = match std::env::consts::ARCH {
        "aarch64" => "arm64",
        "x86_64" => "x64",
        other => other,
    };
    if value["schema"] != 1 || value["platform"] != expected_os || value["arch"] != arch {
        bail!("bundle platform/CPU/schema mismatch");
    }
    let files = value["files"]
        .as_object()
        .context("invalid bundle manifest")?;
    let binary = if cfg!(windows) { "hmesh.exe" } else { "hmesh" };
    let mcp = if cfg!(windows) {
        "hmesh-mcp.exe"
    } else {
        "hmesh-mcp"
    };
    let launcher = if cfg!(windows) {
        "start-windows.ps1"
    } else if cfg!(target_os = "macos") {
        "start-mac.command"
    } else {
        "start.sh"
    };
    let allowed = [binary, mcp, "README.txt", "NOTICE.txt", launcher];
    if files.len() != allowed.len() || allowed.iter().any(|n| !files.contains_key(*n)) {
        bail!("bundle executable missing");
    }
    for (name, digest) in files {
        if name.is_empty()
            || !name
                .bytes()
                .all(|c| c.is_ascii_alphanumeric() || b"._-".contains(&c))
            || name == "."
            || name == ".."
        {
            bail!("invalid bundle filename");
        }
        regular(&bundle.join(name))?;
        if file_sha256(&bundle.join(name), 256 * 1024 * 1024)?
            != digest.as_str().context("invalid bundle digest")?
        {
            bail!("bundle checksum failed");
        }
    }
    Ok(
        json!({"passed":true,"version":value["version"],"commit":value["commit"],"platform":value["platform"],"arch":value["arch"],"database_schema":value["database_schema"],"publisher_signature_verified":false}),
    )
}
#[cfg(test)]
mod tests {
    use super::*;
    fn node() -> (tempfile::TempDir, Instance) {
        let temp = tempfile::tempdir().unwrap();
        let instance =
            Instance::init(&temp.path().join("node"), hmesh_core::NodeConfig::default()).unwrap();
        (temp, instance)
    }
    #[test]
    fn private_backup_restores_original_identity_history_and_old_config_fields() {
        let (temp, instance) = node();
        let mut document: toml::Value =
            toml::from_str(&fs::read_to_string(instance.dir.join("node.toml")).unwrap()).unwrap();
        document["runtime"]
            .as_table_mut()
            .unwrap()
            .remove("project_limits");
        document["runtime"]
            .as_table_mut()
            .unwrap()
            .remove("agent_limits");
        fs::write(
            instance.dir.join("node.toml"),
            toml::to_string(&document).unwrap(),
        )
        .unwrap();
        let store = hmesh_store::Store::open(&instance.dir.join("state.sqlite3")).unwrap();
        store.set_node(&instance.card).unwrap();
        let request = hmesh_core::Envelope::new(
            uuid::Uuid::new_v4(),
            "task.submit",
            hmesh_core::TaskSubmit::new(uuid::Uuid::new_v4(), "fixture", "preserve history"),
        );
        let task = store
            .admit(instance.card.node_id, &request, true, 20)
            .unwrap();
        drop(store);
        let backup = temp.path().join("backup");
        create(&instance.dir, &backup).unwrap();
        assert_eq!(check(&backup).unwrap()["passed"], true);
        let saved: toml::Value =
            toml::from_str(&fs::read_to_string(backup.join("node.toml")).unwrap()).unwrap();
        assert!(saved["runtime"].get("project_limits").is_none());
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                fs::metadata(&backup).unwrap().permissions().mode() & 0o777,
                0o700
            );
        }
        assert!(restore(&temp.path().join("clone"), &backup).is_err());
        assert!(restore(&instance.dir, &backup).is_err());
        fs::rename(&instance.dir, temp.path().join("old-node")).unwrap();
        restore(&instance.dir, &backup).unwrap();
        let restored = Instance::load(&instance.dir).unwrap();
        assert_eq!(restored.card.node_id, instance.card.node_id);
        assert_eq!(restored.card.fingerprint, instance.card.fingerprint);
        assert_eq!(restored.owner_token(), instance.owner_token());
        let store = hmesh_store::Store::open(&restored.dir.join("state.sqlite3")).unwrap();
        assert_eq!(
            store.task(task.task_id).unwrap().request.goal,
            "preserve history"
        );
        assert_eq!(store.attempt_count(task.task_id).unwrap(), 0);
    }
    #[test]
    fn backup_refuses_live_lock_existing_destination_and_tampered_files() {
        let (temp, instance) = node();
        let backup = temp.path().join("backup");
        let lock = instance.lock_daemon().unwrap();
        assert!(create(&instance.dir, &backup).is_err());
        assert!(!backup.exists());
        drop(lock);
        create(&instance.dir, &backup).unwrap();
        assert!(create(&instance.dir, &backup).is_err());
        fs::write(
            backup.join("policy.toml"),
            "[default]\naction='deny'\n# changed",
        )
        .unwrap();
        assert!(check(&backup).is_err());
        fs::rename(&instance.dir, temp.path().join("old")).unwrap();
        assert!(restore(&instance.dir, &backup).is_err());
        assert!(!instance.dir.exists());
    }
    #[test]
    fn backup_captures_external_policy_but_never_copies_external_harness_files() {
        let (temp, instance) = node();
        let policy = temp.path().join("owner-policy.toml");
        fs::write(&policy, "[default]\naction='deny'\n").unwrap();
        let mut config = instance.config.clone();
        config.security.policy_file = policy;
        fs::write(
            instance.dir.join("node.toml"),
            toml::to_string(&config).unwrap(),
        )
        .unwrap();
        fs::write(instance.dir.join("unrelated-private.json"), b"DO NOT COPY").unwrap();
        fs::write(
            instance.dir.join("startup.json"),
            br#"{"name":"local-reference-only"}"#,
        )
        .unwrap();
        let backup = temp.path().join("backup");
        create(&instance.dir, &backup).unwrap();
        assert!(!backup.join("unrelated-private.json").exists());
        assert!(backup.join("startup.json").is_file());
        assert_eq!(
            Instance::load(&backup).unwrap().config.security.policy_file,
            PathBuf::from("policy.toml")
        );
        fs::write(&config.security.policy_file, "[default]\naction='allow'\n").unwrap();
        let rejected = temp.path().join("invalid-policy-backup");
        assert!(create(&instance.dir, &rejected).is_err());
        assert!(!rejected.exists());
    }
    #[test]
    fn upgrade_check_rejects_changed_binary_wrong_cpu_and_traversal() {
        let temp = tempfile::tempdir().unwrap();
        let bundle = temp.path();
        let binary = if cfg!(windows) { "hmesh.exe" } else { "hmesh" };
        let mut files = serde_json::Map::new();
        let mcp = if cfg!(windows) {
            "hmesh-mcp.exe"
        } else {
            "hmesh-mcp"
        };
        let launcher = if cfg!(windows) {
            "start-windows.ps1"
        } else if cfg!(target_os = "macos") {
            "start-mac.command"
        } else {
            "start.sh"
        };
        for name in [binary, mcp, "README.txt", "NOTICE.txt", launcher] {
            fs::write(bundle.join(name), b"fixture package").unwrap();
            files.insert(
                name.into(),
                json!(file_sha256(&bundle.join(name), 1024).unwrap()),
            );
        }
        let os = if cfg!(target_os = "macos") {
            "darwin"
        } else if cfg!(windows) {
            "win32"
        } else {
            "linux"
        };
        let arch = if cfg!(target_arch = "aarch64") {
            "arm64"
        } else {
            "x64"
        };
        let mut value = json!({"schema":1,"database_schema":7,"platform":os,"arch":arch,"version":"hmesh 0.0.1","commit":"fixture","files":files});
        fs::write(
            bundle.join("manifest.json"),
            serde_json::to_vec(&value).unwrap(),
        )
        .unwrap();
        assert!(bundle_check(bundle).is_ok());
        fs::write(bundle.join(binary), b"changed").unwrap();
        assert!(bundle_check(bundle).is_err());
        value["arch"] = json!("other");
        fs::write(
            bundle.join("manifest.json"),
            serde_json::to_vec(&value).unwrap(),
        )
        .unwrap();
        assert!(bundle_check(bundle).is_err());
        value["arch"] = json!(arch);
        value["files"]["../private"] = json!("not permitted");
        fs::write(
            bundle.join("manifest.json"),
            serde_json::to_vec(&value).unwrap(),
        )
        .unwrap();
        assert!(bundle_check(bundle).is_err());
    }
}
