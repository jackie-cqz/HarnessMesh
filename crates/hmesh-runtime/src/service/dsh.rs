//! Owner-selected DSH routes are applied after its native profile. The overlay
//! contains credential references only. The usual DSH session store stays in
//! place when an owner changes the service or goes back to the native profile.
use super::{AgentService, Protocol, write_private};
use crate::prepare::native_path;
use anyhow::{Context, Result};
use hmesh_core::HarnessKind;
use hmesh_core::service::{MODE_AGENT_LOGIN, MODE_CUSTOM};
use serde_json::{Value, json};
use std::path::Path;
use tokio::process::Command;
use uuid::Uuid;

const OFFICIAL: &str = "deepseek-official";
const OFFICIAL_ROOT: &str = "https://api.deepseek.com/anthropic";
pub(super) const KEY_VAR: &str = "HMESH_DSH_API_KEY";
const UNUSED_KEY_VAR: &str = "HMESH_DSH_UNUSED_KEY";

/// DSH represents a chosen model as an opaque JSON [provider, model] string.
pub fn gateway_model(value: &str) -> Option<String> {
    let [provider, model] = serde_json::from_str::<[String; 2]>(value).ok()?;
    (provider == "hmesh").then_some(model)
}

fn overlay(
    service: &AgentService,
    credentials: &Path,
    previous_model: Option<&str>,
) -> Result<Value> {
    let custom = service.mode == MODE_CUSTOM;
    let provider = if custom { "hmesh" } else { OFFICIAL };
    let model = if custom {
        service
            .default_model
            .as_deref()
            .context("DSH requires a custom model")?
    } else {
        service
            .default_model
            .as_deref()
            .unwrap_or("deepseek-v4-flash")
    };
    // DSH resolves the old model immediately on resume, even when the owner
    // has removed it. Retain its metadata for this process only so the daemon
    // can select the allowed replacement before any prompt.
    let previous_gateway_model = previous_model.and_then(gateway_model);
    let mut providers = json!({});
    if custom {
        let api = match service.custom_protocol(HarnessKind::Dsh)? {
            Protocol::OpenAi => "openai-completions",
            Protocol::Anthropic => "anthropic-messages",
            Protocol::Responses => "openai-responses",
        };
        let mut models = service.model_list();
        if let Some(old) = previous_gateway_model
            && !models.contains(&old)
        {
            models.push(old);
        }
        providers["hmesh"] = json!({
            "api":api,
            "baseURL":service.base_url.as_deref().context("DSH requires a custom address")?,
            "apiKeyEnv":KEY_VAR,
            "models":models.into_iter().map(|id| json!({"id":id})).collect::<Vec<_>>()
        });
    } else if let Some(old) = previous_gateway_model {
        // The same metadata is needed when returning a gateway session to the
        // official account. This historical route has no configured key.
        providers["hmesh"] = json!({
            "api":"anthropic-messages", "baseURL":OFFICIAL_ROOT,
            "apiKeyEnv":UNUSED_KEY_VAR, "models":[{"id":old}]
        });
    }
    // The native provider can resolve old session metadata even after moving
    // to a gateway. It never receives the gateway's key. The stock loader
    // checks its credential file before the environment, so it must not use
    // the file belonging to another registration or the user's native DSH.
    let official_key = if custom { UNUSED_KEY_VAR } else { KEY_VAR };
    let selection = json!({"provider":provider,"model":model});
    Ok(json!([
        {"id":"credentials","config":{"path":credentials,"watch":false}},
        {"id":"acp","config":selection},
        {"id":"agent-default-model","config":selection},
        {"id":"llm-deepseek","config":{"baseURL":OFFICIAL_ROOT,"apiKeyEnv":official_key}},
        {"id":"llm-deepseek-account","disabled":true},
        {"id":"llm-pi-ai","config":{"providers":providers}}
    ]))
}

pub(crate) fn launch_args(
    command: &mut Command,
    args: &[String],
    data_dir: &Path,
    agent: Uuid,
    service: &AgentService,
    previous_model: Option<&str>,
) -> Result<()> {
    if service.mode == MODE_AGENT_LOGIN {
        command.args(args);
        return Ok(());
    }
    let dir = native_path(&std::fs::canonicalize(data_dir)?)
        .join("dsh-agents")
        .join(agent.to_string());
    let bytes = serde_json::to_vec(&overlay(
        service,
        &dir.join("credentials.yaml"),
        previous_model,
    )?)?;
    // Address by content so a concurrent launch with a different route cannot
    // overwrite this process's overlay before the DSH CLI has opened it.
    let file = dir.join(format!("{}.yml", hmesh_security::sha256_hex(&bytes)));
    if !file.is_file() {
        write_private(&dir, &file, agent, &bytes)?;
    }
    // DSH stops parsing launcher flags at the first application argument.
    // Insert after owner overlays (last layer wins), before any app arguments.
    let i = launcher_arg_count(args);
    command
        .args(&args[..i])
        .arg("--patch")
        .arg(file)
        .args(&args[i..]);
    Ok(())
}

fn launcher_arg_count(args: &[String]) -> usize {
    let mut i = usize::from(args.first().is_some_and(|a| a == "acp"));
    while let Some(flag) = args.get(i) {
        if matches!(
            flag.as_str(),
            "--profile" | "--patch" | "--from-default-profile"
        ) {
            i = (i + 2).min(args.len());
        } else if ["--profile=", "--patch=", "--from-default-profile="]
            .iter()
            .any(|prefix| flag.starts_with(prefix))
        {
            i += 1;
        } else {
            break;
        }
    }
    i
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn official_key_mode_pins_the_native_endpoint_and_references_only_the_agent_key() {
        let owner_key = "fixture-private-value";
        let service = AgentService {
            mode: hmesh_core::service::MODE_API_KEY.to_owned(),
            api_key: Some(owner_key.to_owned()),
            default_model: Some("deepseek-v4-pro".to_owned()),
            ..Default::default()
        };
        let patch = overlay(&service, Path::new("private-credentials.yaml"), None).unwrap();
        let config = |id| {
            &patch
                .as_array()
                .unwrap()
                .iter()
                .find(|p| p["id"] == id)
                .unwrap()["config"]
        };
        assert_eq!(config("llm-deepseek")["baseURL"], OFFICIAL_ROOT);
        assert_eq!(config("llm-deepseek")["apiKeyEnv"], KEY_VAR);
        assert_eq!(config("acp")["provider"], OFFICIAL);
        assert_eq!(config("acp")["model"], "deepseek-v4-pro");
        assert!(!patch.to_string().contains(owner_key));
    }

    #[test]
    fn switching_to_the_official_key_keeps_only_metadata_for_a_previous_gateway_model() {
        let service = AgentService {
            mode: hmesh_core::service::MODE_API_KEY.to_owned(),
            ..Default::default()
        };
        let patch = overlay(
            &service,
            Path::new("creds.yaml"),
            Some(r#"["hmesh","old-model"]"#),
        )
        .unwrap();
        let gateway = patch
            .as_array()
            .unwrap()
            .iter()
            .find(|p| p["id"] == "llm-pi-ai")
            .unwrap();
        let config = &gateway["config"]["providers"]["hmesh"];
        assert_eq!(config["models"], json!([{"id":"old-model"}]));
        assert_eq!(config["apiKeyEnv"], UNUSED_KEY_VAR);
    }
    #[test]
    fn owner_overlays_precede_managed_overlay_and_app_flags_follow_it() {
        let count = |args: &[&str]| {
            launcher_arg_count(&args.iter().map(|s| (*s).to_owned()).collect::<Vec<_>>())
        };
        assert_eq!(count(&["acp", "--patch", "owner.yml", "app-arg"]), 3);
        assert_eq!(count(&["--profile", "acp", "--", "--patch", "app.yml"]), 2);
        assert_eq!(count(&["--profile=acp", "--patch=owner.yml"]), 2);
    }
}
