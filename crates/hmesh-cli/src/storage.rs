//! Explicit offline retention. Plans contain IDs and hashes, never arbitrary deletion paths.
use anyhow::{Context, Result, bail};
use hmesh_core::{TaskRecord, now};
use hmesh_security::{Instance, file_sha256, sha256_hex};
use hmesh_store::Store;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{collections::BTreeMap, fs, path::Path};
use uuid::Uuid;

#[derive(Debug, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
struct Item {
    task_id: Uuid,
    attempt_id: Option<Uuid>,
    snapshot_sha256: String,
    run_sha256: Option<String>,
    run_bytes: u64,
    snapshot_bytes: u64,
    remove_run: bool,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Plan {
    schema: u32,
    node_id: Uuid,
    cutoff: i64,
    keep_latest: usize,
    items: Vec<Item>,
}

fn scan(root: &Path) -> Result<(String, u64)> {
    fn walk(
        root: &Path,
        path: &Path,
        out: &mut BTreeMap<String, String>,
        bytes: &mut u64,
    ) -> Result<()> {
        let m = fs::symlink_metadata(path)?;
        if m.file_type().is_symlink() {
            bail!("retention refuses symbolic links");
        }
        #[cfg(windows)]
        {
            use std::os::windows::fs::MetadataExt;
            if m.file_attributes() & 0x400 != 0 {
                bail!("retention refuses reparse points");
            }
        }
        if out.len() > 20000 || *bytes > 1024 * 1024 * 1024 {
            bail!("run scan exceeds 20000 files/1 GiB");
        }
        let name = path
            .strip_prefix(root)?
            .to_str()
            .context("run path is not UTF-8")?
            .to_owned();
        if path.strip_prefix(root)?.components().count() > 64 {
            bail!("run exceeds 64 directory levels");
        }
        if m.is_dir() {
            out.insert(name, "directory".into());
            for entry in fs::read_dir(path)? {
                walk(root, &entry?.path(), out, bytes)?;
            }
        } else if m.is_file() {
            *bytes = bytes.checked_add(m.len()).context("size overflow")?;
            if *bytes > 1024 * 1024 * 1024 {
                bail!("run exceeds 1 GiB");
            }
            out.insert(name, file_sha256(path, 1024 * 1024 * 1024)?);
        } else {
            bail!("retention refuses special files");
        }
        Ok(())
    }
    let mut files = BTreeMap::new();
    let mut bytes = 0;
    walk(root, root, &mut files, &mut bytes)?;
    Ok((sha256_hex(&serde_json::to_vec(&files)?), bytes))
}
fn item(instance: &Instance, store: &Store, task: &TaskRecord) -> Result<Item> {
    let mut snapshots = Vec::new();
    let mut snapshot_bytes = 0;
    if let Some(set) = store.artifact_set(task.task_id)? {
        for e in set.entries {
            snapshot_bytes += e.bytes.as_ref().map_or(0, |b| b.len() as u64);
            snapshots.push(
                json!({"name":e.name,"error":e.error,"sha256":e.bytes.map(|b|sha256_hex(&b))}),
            );
        }
    }
    // Unreviewed worktrees remain protected. Their patch and registry are never pruned here.
    let workspace = store.workspace(task.task_id)?;
    let remove_run = workspace.as_ref().is_none_or(|w| w["state"] == "pruned");
    let path = task
        .last_attempt_id
        .map(|a| instance.dir.join("runs").join(a.to_string()));
    let (run_sha256, run_bytes) = if remove_run {
        if let Some(path) = path.filter(|p| p.exists()) {
            let (hash, size) = scan(&path)?;
            (Some(hash), size)
        } else {
            (None, 0)
        }
    } else {
        (None, 0)
    };
    Ok(Item {
        task_id: task.task_id,
        attempt_id: task.last_attempt_id,
        snapshot_sha256: sha256_hex(&serde_json::to_vec(&snapshots)?),
        run_sha256,
        run_bytes,
        snapshot_bytes,
        remove_run,
    })
}
fn plan(instance: &Instance, store: &Store, cutoff: i64, keep: usize) -> Result<Plan> {
    let items = store
        .retention_candidates(cutoff, keep)?
        .iter()
        .map(|t| item(instance, store, t))
        .collect::<Result<Vec<_>>>()?
        .into_iter()
        .filter(|i| i.snapshot_bytes > 0 || i.run_sha256.is_some())
        .collect();
    Ok(Plan {
        schema: 1,
        node_id: instance.card.node_id,
        cutoff,
        keep_latest: keep,
        items,
    })
}
fn open(node: &Path) -> Result<(Instance, fs::File, Store)> {
    let instance = Instance::load(node)?;
    let lock = instance
        .lock_daemon()
        .context("stop the Daemon before offline storage operations")?;
    let store = Store::open(&instance.dir.join("state.sqlite3"))?;
    let runs = instance.dir.join("runs");
    if let Ok(meta) = fs::symlink_metadata(&runs) {
        if !meta.is_dir() || meta.file_type().is_symlink() {
            bail!("runs root must be an ordinary directory");
        }
        #[cfg(windows)]
        {
            use std::os::windows::fs::MetadataExt;
            if meta.file_attributes() & 0x400 != 0 {
                bail!("runs root must not be a reparse point");
            }
        }
    }
    Ok((instance, lock, store))
}
pub fn status(node: &Path) -> Result<Value> {
    let (instance, _lock, store) = open(node)?;
    let mut value = store.storage_summary()?;
    let runs = instance.dir.join("runs");
    let mut bytes = 0u64;
    let mut dirs = 0;
    let mut scan_complete = true;
    if runs.exists() {
        for e in fs::read_dir(&runs)? {
            let e = e?;
            dirs += 1;
            match scan(&e.path()) {
                Ok((_, n)) => bytes = bytes.saturating_add(n),
                Err(_) => scan_complete = false,
            }
        }
    }
    value["run_bytes"] = json!(bytes);
    value["run_directories"] = json!(dirs);
    value["run_scan_complete"] = json!(scan_complete);
    Ok(value)
}
pub fn preview(node: &Path, days: u32, keep: usize, out: &Path) -> Result<Value> {
    if days == 0 || days > 36500 {
        bail!("older-than-days must be 1..36500");
    }
    let (instance, _lock, store) = open(node)?;
    let plan = plan(&instance, &store, now() - i64::from(days) * 86400, keep)?;
    let bytes = serde_json::to_vec_pretty(&plan)?;
    let mut options = fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    use std::io::Write;
    let mut file = options.open(out)?;
    file.write_all(&bytes)?;
    file.sync_all()?;
    Ok(
        json!({"plan":out,"sha256":sha256_hex(&bytes),"tasks":plan.items.len(),"snapshot_bytes":plan.items.iter().map(|i|i.snapshot_bytes).sum::<u64>(),"run_bytes":plan.items.iter().map(|i|i.run_bytes).sum::<u64>(),"unreviewed_worktrees_preserved":true,"deleted":false}),
    )
}
pub fn apply(node: &Path, path: &Path, expected: &str) -> Result<Value> {
    if fs::metadata(path)?.len() > 1024 * 1024 {
        bail!("retention plan exceeds 1 MiB");
    }
    let bytes = fs::read(path)?;
    if sha256_hex(&bytes) != expected {
        bail!("reviewed retention plan SHA-256 differs");
    }
    let original: Plan = serde_json::from_slice(&bytes)?;
    let (instance, _lock, store) = open(node)?;
    if original.schema != 1
        || original.node_id != instance.card.node_id
        || original.cutoff > now() - 86400
    {
        bail!("invalid retention identity/age boundary");
    }
    let current = plan(&instance, &store, original.cutoff, original.keep_latest)?;
    if original.items != current.items {
        bail!("retention candidates changed; create a fresh preview");
    }
    // Validate the complete plan before the first deletion. Directory names derive only from stored attempt UUIDs.
    let ids: Vec<_> = current.items.iter().map(|i| i.task_id).collect();
    let purged = store.purge_snapshots(&ids)?;
    let mut removed = 0;
    for item in current.items {
        if item.remove_run && item.run_sha256.is_some() {
            let path = instance
                .dir
                .join("runs")
                .join(item.attempt_id.context("missing run attempt")?.to_string());
            // No links were accepted by the full scan; remove_dir_all does not follow symlink entries.
            fs::remove_dir_all(path)?;
            removed += 1;
        }
    }
    store.audit(
        Some(instance.card.node_id),
        "retention.apply",
        expected,
        "owner-reviewed",
        Uuid::new_v4(),
    )?;
    Ok(
        json!({"purged_snapshots":purged,"removed_run_directories":removed,"task_history_preserved":true,"database_file_shrink":"run storage compact explicitly"}),
    )
}
pub fn compact(node: &Path) -> Result<Value> {
    let (instance, _lock, _store) = open(node)?;
    let db = rusqlite::Connection::open(instance.dir.join("state.sqlite3"))?;
    db.execute_batch("PRAGMA wal_checkpoint(TRUNCATE); VACUUM;")?;
    Ok(json!({"compacted":true,"history_deleted":false}))
}

#[cfg(test)]
mod tests {
    use super::*;
    use hmesh_core::*;
    fn fixture() -> (tempfile::TempDir, Instance, Store, Uuid) {
        let temp = tempfile::tempdir().unwrap();
        let instance = Instance::init(&temp.path().join("node"), NodeConfig::default()).unwrap();
        let store = Store::open(&instance.dir.join("state.sqlite3")).unwrap();
        let e = Envelope::new(
            instance.card.node_id,
            "task.submit",
            TaskSubmit::new(Uuid::new_v4(), "fixture", "retention fixture"),
        );

        let config:hmesh_core::AgentConfig=serde_json::from_value(json!({"agent":{"name":"retention-fixture","harness":"pi","project_id":"fixture"},"project":{"root":std::env::temp_dir(),"allowed_tools":["read"]},"adapter":{"executable":std::env::current_exe().unwrap(),"provider":"fixture","model":"fixture"}})).unwrap();
        let card:hmesh_core::AgentCard=serde_json::from_value(json!({"agent_id":e.payload.target_agent_id,"node_id":Uuid::new_v4(),"name":"retention-fixture","harness":"pi","project_ids":["fixture"],"availability":"unverified","capabilities":[],"probe":{"installed":true,"version":null,"version_supported":true,"protocol_probe":"fixture","authentication":"not_checked","managed_run":"not_checked","streaming":"not_checked","cancel":"not_checked","session_resume":"not_checked","unavailable_reason":null}})).unwrap();
        store.register_agent(&card, &config).unwrap();

        let t = store.admit(instance.card.node_id, &e, false, 20).unwrap();
        let r = store.claim_next().unwrap().unwrap();
        store
            .complete_with_artifacts(
                t.task_id,
                r.last_attempt_id.unwrap(),
                "fixture-config",
                &[hmesh_store::SnapshotEntry {
                    name: "out/health.json".into(),
                    bytes: Some(b"original".to_vec()),
                    error: None,
                }],
            )
            .unwrap();
        let run = instance
            .dir
            .join("runs")
            .join(r.last_attempt_id.unwrap().to_string());
        fs::create_dir_all(&run).unwrap();
        fs::write(run.join("private-run.log"), b"fixture-only").unwrap();
        let mut record = store.task(t.task_id).unwrap();
        record.created_at = now() - 86400 * 40;
        record.updated_at = record.created_at;
        rusqlite::Connection::open(instance.dir.join("state.sqlite3"))
            .unwrap()
            .execute(
                "UPDATE tasks SET created_at=?1,record_json=?2 WHERE task_id=?3",
                rusqlite::params![
                    record.created_at,
                    serde_json::to_string(&record).unwrap(),
                    t.task_id.to_string()
                ],
            )
            .unwrap();
        (temp, instance, store, t.task_id)
    }
    #[test]
    fn reviewed_plan_rejects_changed_files_then_purges_only_old_terminal_data() {
        let (temp, instance, store, id) = fixture();
        let path = temp.path().join("plan.json");
        let p = preview(&instance.dir, 30, 0, &path).unwrap();
        assert_eq!(p["tasks"], 1);
        assert!(apply(&instance.dir, &path, "wrong").is_err());
        let run = instance
            .dir
            .join("runs")
            .join(store.task(id).unwrap().last_attempt_id.unwrap().to_string());
        fs::write(run.join("changed"), b"changed after preview").unwrap();
        assert!(apply(&instance.dir, &path, p["sha256"].as_str().unwrap()).is_err());
        assert!(
            store.artifact_set(id).unwrap().unwrap().entries[0]
                .bytes
                .is_some()
        );
        let next = temp.path().join("next.json");
        let p = preview(&instance.dir, 30, 0, &next).unwrap();
        let result = apply(&instance.dir, &next, p["sha256"].as_str().unwrap()).unwrap();
        assert_eq!(result["removed_run_directories"], 1);
        assert!(!run.exists());
        assert_eq!(store.task(id).unwrap().status, TaskStatus::Succeeded);
        assert_eq!(store.attempt_count(id).unwrap(), 1);
        assert_eq!(
            store.artifact_set(id).unwrap().unwrap().entries[0]
                .error
                .as_deref(),
            Some("purged_by_owner")
        );
        compact(&instance.dir).unwrap();
        assert_eq!(store.tasks().unwrap().len(), 1);
    }
    #[test]
    fn live_lock_and_latest_retention_are_preserved() {
        let (temp, instance, _store, _id) = fixture();
        let lock = instance.lock_daemon().unwrap();
        assert!(preview(&instance.dir, 30, 0, &temp.path().join("blocked.json")).is_err());
        drop(lock);
        let p = preview(&instance.dir, 30, 1, &temp.path().join("keep.json")).unwrap();
        assert_eq!(p["tasks"], 0);
    }
    #[cfg(unix)]
    #[test]
    fn symlinked_runs_root_is_never_scanned_or_deleted() {
        let (temp, instance, store, id) = fixture();
        let runs = instance.dir.join("runs");
        let outside = temp.path().join("outside");
        fs::rename(&runs, &outside).unwrap();
        std::os::unix::fs::symlink(&outside, &runs).unwrap();
        assert!(preview(&instance.dir, 30, 0, &temp.path().join("plan.json")).is_err());
        assert!(outside.exists());
        assert!(
            store.artifact_set(id).unwrap().unwrap().entries[0]
                .bytes
                .is_some()
        );
    }
}
