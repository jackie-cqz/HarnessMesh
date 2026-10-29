//! Deterministic owner-controlled artifact checks, separate from harness status.
use anyhow::{Context, Result, bail};
use hmesh_core::AgentConfig;
use serde_json::{Value, json};
use std::{
    fs,
    io::Read,
    path::{Component, Path},
};

fn artifact(config: &AgentConfig, name: &Path) -> Result<(Vec<u8>, Value)> {
    if !config.project.artifact_paths.iter().any(|p| p == name) {
        bail!("artifact has no local owner grant");
    }
    if name.as_os_str().is_empty()
        || name
            .components()
            .any(|c| !matches!(c, Component::Normal(_)))
    {
        bail!("artifact path must be a relative path without traversal");
    }
    let root = fs::canonicalize(&config.project.root)?;
    let path = fs::canonicalize(root.join(name)).context("artifact is unavailable")?;
    if !path.starts_with(&root) || !path.is_file() {
        bail!("artifact resolves outside the registered project or is not a file");
    }
    let mut bytes = vec![];
    fs::File::open(path)?
        .take(10485761)
        .read_to_end(&mut bytes)?;
    if bytes.len() > 10485760 {
        bail!("artifact exceeds 10 MiB");
    }
    let summary =
        json!({"name":name,"sha256":hmesh_security::sha256_hex(&bytes),"size_bytes":bytes.len()});
    Ok((bytes, summary))
}
pub fn summaries(config: &AgentConfig) -> Result<Vec<Value>> {
    config
        .project
        .artifact_paths
        .iter()
        .map(|name| artifact(config, name).map(|(_, summary)| summary))
        .collect()
}
pub fn verify_json(config: &AgentConfig, name: &Path, expected: &Value) -> Result<Value> {
    let expected = expected
        .as_object()
        .context("expected JSON must be an object")?;
    let (bytes, summary) = artifact(config, name)?;
    let actual: Option<Value> = serde_json::from_slice(&bytes).ok();
    let assertions: Vec<_> = expected.iter().map(|(key,value)| json!({"field":key,"passed":actual.as_ref().and_then(|a| a.get(key)) == Some(value)})).collect();
    let passed = !assertions.is_empty() && assertions.iter().all(|a| a["passed"] == true);
    Ok(
        json!({"verifier":"deterministic-json-subset-v1","passed":passed,"artifact":summary,"assertions":assertions}),
    )
}
