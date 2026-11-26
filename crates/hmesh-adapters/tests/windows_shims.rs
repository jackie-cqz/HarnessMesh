//! A Windows npm shim (`pi-acp.cmd`) is a batch file; HarnessMesh runs the
//! entry it points at instead. Which entry each agent's shim leads to comes
//! from the agent catalog; these are the paths used before there was one.
use hmesh_adapters::executable::resolve_executable_with_node;
use hmesh_core::HarnessKind;
use std::{
    fs,
    path::{Path, PathBuf},
    sync::atomic::{AtomicUsize, Ordering},
};

/// A fresh directory holding a shim and the files beside it.
fn place(shim: &str, text: &str, files: &[&str]) -> (PathBuf, PathBuf) {
    static NEXT: AtomicUsize = AtomicUsize::new(0);
    let dir = std::env::temp_dir().join(format!(
        "hmesh-shims-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    for file in files {
        let path = dir.join(file);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, "").unwrap();
    }
    fs::write(dir.join(shim), text).unwrap();
    (dir.join(shim), dir)
}

fn entry(kind: HarnessKind, shim: &Path, node: &Path) -> Result<PathBuf, String> {
    let js = |kind: HarnessKind| kind != HarnessKind::Opencode;
    resolve_executable_with_node(kind, shim, js(kind).then_some(node))
        .map(|resolved| resolved.entry)
        .map_err(|e| e.to_string())
}

#[test]
fn each_agents_shim_leads_to_the_entry_it_did_before_the_catalog() {
    let (node, node_dir) = place("node", "", &[]);
    for (kind, shim, text, entry_file) in [
        (
            HarnessKind::Pi,
            "pi-acp.cmd",
            "",
            "node_modules/pi-acp/dist/index.js",
        ),
        (
            HarnessKind::Dsh,
            "dsh.cmd",
            "",
            "node_modules/@deepseek-ai/dsh/lib/bin.js",
        ),
        (
            HarnessKind::Opencode,
            "opencode.cmd",
            "",
            "node_modules/opencode-ai/bin/opencode.exe",
        ),
        (
            HarnessKind::ClaudeCode,
            "claude-agent-acp.cmd",
            "",
            "node_modules/@agentclientprotocol/claude-agent-acp/dist/index.js",
        ),
        (
            HarnessKind::Codex,
            "codex-acp.cmd",
            r#""%dp0%\node_modules\@agentclientprotocol\codex-acp\dist\index.js" %*"#,
            "node_modules/@agentclientprotocol/codex-acp/dist/index.js",
        ),
    ] {
        let (shim, dir) = place(shim, text, &[entry_file]);
        assert_eq!(
            entry(kind, &shim, &node),
            Ok(dir.join(entry_file)),
            "{kind}"
        );
        fs::remove_dir_all(dir).unwrap();
    }
    fs::remove_dir_all(node_dir).unwrap();
}

#[test]
fn codex_shims_from_its_old_package_lead_to_the_old_entry() {
    let (node, node_dir) = place("node", "", &[]);
    const OLD: &str = "node_modules/@zed-industries/codex-acp/bin/codex-acp.js";
    const NEW: &str = "node_modules/@agentclientprotocol/codex-acp/dist/index.js";
    // The shim names the old package, though the new one is installed too.
    let (shim, dir) = place(
        "codex-acp.cmd",
        r#""%dp0%\node_modules\@zed-industries\codex-acp\bin\codex-acp.js" %*"#,
        &[OLD, NEW],
    );
    assert_eq!(entry(HarnessKind::Codex, &shim, &node), Ok(dir.join(OLD)));
    fs::remove_dir_all(dir).unwrap();
    // Only the old package is there and the shim names neither.
    let (shim, dir) = place("codex-acp.cmd", "@echo off", &[OLD]);
    assert_eq!(entry(HarnessKind::Codex, &shim, &node), Ok(dir.join(OLD)));
    fs::remove_dir_all(dir).unwrap();
    // The shim names the new package, which is missing: no fallback.
    let (shim, dir) = place(
        "codex-acp.cmd",
        r#""%dp0%\node_modules\@agentclientprotocol\codex-acp\dist\index.js" %*"#,
        &[OLD],
    );
    assert_eq!(
        entry(HarnessKind::Codex, &shim, &node),
        Err("unsupported shell shim; configure the real JS entry or native executable".into())
    );
    fs::remove_dir_all(dir).unwrap();
    fs::remove_dir_all(node_dir).unwrap();
}

#[test]
fn a_generic_acp_shim_is_refused_with_the_same_advice() {
    let (shim, dir) = place("some-agent.cmd", "@echo off", &[]);
    assert_eq!(
        resolve_executable_with_node(HarnessKind::Acp, &shim, None)
            .map(|r| r.entry)
            .map_err(|e| e.to_string()),
        Err("configure the agent's real JS entry or native executable, not a shell shim".into())
    );
    fs::remove_dir_all(dir).unwrap();
}
