//! Explicit network audit of every registry package in Cargo.lock, all targets.
use anyhow::{Context, Result, bail};
use serde::Deserialize;
use serde_json::{Value, json};
use std::{
    collections::BTreeSet,
    path::PathBuf,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

#[derive(Deserialize)]
struct Lock {
    package: Vec<Package>,
}
#[derive(Clone, Deserialize)]
struct Package {
    name: String,
    version: String,
    source: Option<String>,
}

#[tokio::main]
async fn main() -> Result<()> {
    let mut args = std::env::args().skip(1);
    if args.next().as_deref() != Some("audit") {
        bail!("usage: cargo run --locked -p xtask -- audit [report.json]");
    }
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .context("workspace root")?
        .to_owned();
    let output = args
        .next()
        .map(PathBuf::from)
        .unwrap_or_else(|| root.join("docs/evidence/dependency-audit.json"));
    if args.next().is_some() {
        bail!("unexpected audit argument");
    }
    let bytes = std::fs::read(root.join("Cargo.lock"))?;
    let lock: Lock = toml::from_str(std::str::from_utf8(&bytes)?)?;
    let packages: Vec<_> = lock
        .package
        .into_iter()
        .filter(|p| {
            p.source
                .as_deref()
                .is_some_and(|s| s.starts_with("registry+"))
        })
        .collect();
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(45))
        .redirect(reqwest::redirect::Policy::none())
        .build()?;
    let mut findings = vec![];
    for batch in packages.chunks(100) {
        let mut pending: Vec<_> = batch.iter().cloned().map(|p| (p, String::new())).collect();
        let mut seen_tokens = BTreeSet::new();
        for _ in 0..20 {
            let queries: Vec<_> = pending.iter().map(|(p,token)| {
                let mut q = json!({"package":{"name":p.name,"ecosystem":"crates.io"},"version":p.version});
                if !token.is_empty() { q["page_token"] = json!(token); }
                q
            }).collect();
            let response: Value = client
                .post("https://api.osv.dev/v1/querybatch")
                .json(&json!({"queries":queries}))
                .send()
                .await
                .context("OSV audit unavailable; no clean result is claimed")?
                .error_for_status()?
                .json()
                .await?;
            let results = response
                .get("results")
                .and_then(Value::as_array)
                .context("OSV returned no result array")?;
            if results.len() != pending.len() {
                bail!("OSV returned an incomplete batch");
            }
            let mut next = vec![];
            for ((p, _), result) in pending.into_iter().zip(results) {
                if !result.is_object() || result.get("error").is_some() {
                    bail!("OSV package query failed: {} {}", p.name, p.version);
                }
                if let Some(vulns) = result.get("vulns") {
                    for vuln in vulns.as_array().context("malformed OSV vulnerabilities")? {
                        let id = vuln
                            .get("id")
                            .and_then(Value::as_str)
                            .context("missing advisory id")?;
                        findings.push(json!({"package":p.name,"version":p.version,"id":id,"url":format!("https://osv.dev/vulnerability/{id}")}));
                    }
                }
                if let Some(token) = result.get("next_page_token") {
                    let token = token.as_str().context("invalid OSV page token")?;
                    if !token.is_empty() {
                        if !seen_tokens.insert((
                            p.name.clone(),
                            p.version.clone(),
                            token.to_owned(),
                        )) {
                            bail!("OSV repeated a page token");
                        }
                        next.push((p, token.to_owned()));
                    }
                }
            }
            pending = next;
            if pending.is_empty() {
                break;
            }
        }
        if !pending.is_empty() {
            bail!("OSV exceeded the bounded pagination limit; audit incomplete");
        }
    }
    let clean = findings.is_empty();
    let report = json!({"schema_version":1,"provider":"https://api.osv.dev/v1/querybatch","queried_at_unix_seconds":SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs(),"cargo_lock_sha256":hmesh_security::sha256_hex(&bytes),"registry_packages_checked":packages.len(),"all_lockfile_targets":true,"complete":true,"status":if clean {"passed"} else {"failed"},"findings":findings});
    if let Some(parent) = output.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(&output, serde_json::to_vec_pretty(&report)?)?;
    println!(
        "Dependency audit: {} registry packages, {} findings; {}",
        packages.len(),
        findings.len(),
        output.display()
    );
    if !clean {
        bail!("known vulnerability advisories require resolution");
    }
    Ok(())
}
