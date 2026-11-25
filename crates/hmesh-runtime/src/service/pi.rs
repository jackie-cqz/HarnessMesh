//! Launch pi-acp with a provider configuration owned by this registered agent.
//! Pi and pi-acp both honor PI_CODING_AGENT_DIR. The model registry contains
//! only a reference to a key passed to the child process.
use super::{AgentService, Protocol};
use crate::prepare::native_path;
use anyhow::{Context, Result, bail};
use hmesh_core::HarnessKind;
use hmesh_core::service::{MODE_AGENT_LOGIN, MODE_API_KEY, MODE_CUSTOM};
use serde_json::{Value, json};
use std::{
    fs,
    path::{Path, PathBuf},
};
use tokio::process::Command;
use uuid::Uuid;

fn provider(service: &AgentService) -> Result<(&str, &str, Value)> {
    let model = service
        .default_model
        .as_deref()
        .context("Pi needs a default model")?;
    let key = "${HMESH_PI_API_KEY}";
    match service.mode.as_str() {
        MODE_API_KEY => {
            let (provider, id) = model.split_once('/').context("Pi needs a provider/model")?;
            let origin = match provider {
                "anthropic" => "https://api.anthropic.com",
                "openai" => "https://api.openai.com/v1",
                "deepseek" => "https://api.deepseek.com",
                _ => bail!("unknown Pi API key provider"),
            };
            Ok((provider, id, json!({"baseUrl": origin, "apiKey": key})))
        }
        MODE_CUSTOM => {
            let api = match service.custom_protocol(HarnessKind::Pi)? {
                Protocol::OpenAi => "openai-completions",
                Protocol::Responses => "openai-responses",
                Protocol::Anthropic => "anthropic-messages",
            };
            let models: Vec<_> = service
                .model_list()
                .into_iter()
                .map(|id| json!({"id": id}))
                .collect();
            let endpoint = service
                .base_url
                .as_deref()
                .context("Pi needs a custom endpoint")?;
            Ok((
                "hmesh",
                model,
                json!({"api": api, "baseUrl": endpoint, "apiKey": key, "models": models}),
            ))
        }
        _ => bail!("unsupported Pi model service mode"),
    }
}

/// Preserve resource discovery through Pi's supported settings paths. Settings
/// for models, credentials and remote package installation stay independent.
fn resources(settings: &mut Value, source: &Path) -> Result<()> {
    let user: Value = match fs::read(source.join("settings.json")) {
        Ok(bytes) => serde_json::from_slice(&bytes).context("invalid Pi user settings")?,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Value::Null,
        Err(e) => return Err(e).context("read Pi resource preferences"),
    };
    if let Some(enabled) = user["enableSkillCommands"].as_bool() {
        settings["enableSkillCommands"] = json!(enabled);
    }
    for kind in ["prompts", "skills", "extensions", "themes"] {
        let mut paths = Vec::new();
        let standard = source.join(kind);
        if standard.is_dir() {
            paths.push(standard.to_string_lossy().into_owned());
        }
        for entry in user[kind]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
        {
            let (prefix, path) = if entry.starts_with(['!', '+', '-']) {
                entry.split_at(1)
            } else {
                ("", entry)
            };
            if path.is_empty() {
                continue;
            }
            if path.starts_with("builtin:")
                || path.starts_with('~')
                || Path::new(path).is_absolute()
            {
                paths.push(entry.to_owned());
            } else {
                // Pi tests exclusion globs against both basename and full path.
                if prefix == "!" && !path.contains(['/', '\\']) {
                    paths.push(entry.to_owned());
                }
                let relative = path
                    .strip_prefix("./")
                    .or_else(|| path.strip_prefix(".\\"))
                    .unwrap_or(path);
                paths.push(format!("{prefix}{}", source.join(relative).display()));
            }
        }
        if !paths.is_empty() {
            settings[kind] = json!(paths);
        }
    }
    Ok(())
}

pub(crate) fn configure(
    command: &mut Command,
    data_dir: &Path,
    agent: Uuid,
    service: &AgentService,
) -> Result<()> {
    if service.mode == MODE_AGENT_LOGIN {
        return Ok(());
    }
    let (provider, model, config) = provider(service)?;
    let models = serde_json::to_vec_pretty(&json!({"providers": {(provider): config}}))?;
    let root = native_path(&fs::canonicalize(data_dir)?)
        .join("agent-profiles")
        .join(agent.to_string())
        .join("pi");
    let mut builder = fs::DirBuilder::new();
    builder.recursive(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        builder.mode(0o700);
    }
    builder.create(&root)?;
    // Existing processes can still perform requests while their owner saves
    // another endpoint. Pin each process to one complete endpoint/model pair.
    // Keep sessions in a stable directory so pi-acp can resume them on restart.
    let mut settings = json!({
        "defaultProvider": provider, "defaultModel": model, "sessionDir": root.join("sessions"),
        "quietStartup": true
    });
    let source = service
        .env
        .get("PI_CODING_AGENT_DIR")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("PI_CODING_AGENT_DIR").map(PathBuf::from))
        .or_else(|| std::env::home_dir().map(|home| home.join(".pi").join("agent")));
    if let Some(source) = source {
        let source = command
            .as_std()
            .get_current_dir()
            .unwrap_or(Path::new("."))
            .join(source);
        if source.is_dir() {
            resources(&mut settings, &native_path(&fs::canonicalize(source)?))?;
        }
    }
    let settings = serde_json::to_vec_pretty(&settings)?;
    let digest = hmesh_security::sha256_hex(&[models.as_slice(), settings.as_slice()].concat());
    let profile = root.join(digest);
    if !profile.is_dir() {
        let temp = tempfile::tempdir_in(&root)?;
        fs::write(temp.path().join("settings.json"), settings)?;
        fs::write(temp.path().join("models.json"), models)?;
        if let Err(error) = fs::rename(temp.path(), &profile)
            && !profile.is_dir()
        {
            return Err(error).context("install Pi agent profile");
        }
    }
    command.env("PI_CODING_AGENT_DIR", profile);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::OsStr;
    use std::path::PathBuf;

    fn profile(root: &Path, agent: Uuid, service: &AgentService) -> PathBuf {
        let mut command = Command::new("pi-acp");
        configure(&mut command, root, agent, service).unwrap();
        command
            .as_std()
            .get_envs()
            .find_map(|(name, value)| {
                (name == OsStr::new("PI_CODING_AGENT_DIR")).then(|| PathBuf::from(value.unwrap()))
            })
            .unwrap()
    }

    #[test]
    fn configured_profiles_pin_routes_and_keep_only_key_references() {
        let dir = tempfile::tempdir().unwrap();
        let agent = Uuid::new_v4();
        let mut service = AgentService {
            mode: MODE_CUSTOM.to_owned(),
            protocol: Some(Protocol::Responses),
            base_url: Some("https://gateway.test/v1".into()),
            api_key: Some("fixture-private-credential".into()),
            default_model: Some("fast".into()),
            models: vec!["fast".into(), "slow".into()],
            ..AgentService::default()
        };
        let original = dir.path().join("original");
        fs::create_dir_all(original.join("prompts")).unwrap();
        fs::write(
            original.join("settings.json"),
            serde_json::to_vec(
                &json!({"extensions":["-extensions/off.js"], "enableSkillCommands":false}),
            )
            .unwrap(),
        )
        .unwrap();
        service.env.insert(
            "PI_CODING_AGENT_DIR".to_owned(),
            original.display().to_string(),
        );
        let first = profile(dir.path(), agent, &service);
        let settings: Value =
            serde_json::from_slice(&fs::read(first.join("settings.json")).unwrap()).unwrap();
        let source = native_path(&fs::canonicalize(original).unwrap());
        assert_eq!(settings["prompts"], json!([source.join("prompts")]));
        assert_eq!(
            settings["extensions"],
            json!([format!("-{}", source.join("extensions/off.js").display())])
        );
        assert_eq!(settings["enableSkillCommands"], false);
        assert_eq!(first, profile(dir.path(), agent, &service));
        let models = fs::read_to_string(first.join("models.json")).unwrap();
        assert!(!models.contains(service.api_key.as_ref().unwrap()));
        let models: Value = serde_json::from_str(&models).unwrap();
        assert_eq!(
            models["providers"]["hmesh"],
            json!({
                "api":"openai-responses","apiKey":"${HMESH_PI_API_KEY}",
                "baseUrl":"https://gateway.test/v1","models":[{"id":"fast"},{"id":"slow"}]
            })
        );
        service.base_url = Some("https://elsewhere.test/v1".into());
        let next = profile(dir.path(), agent, &service);
        assert_ne!(first, next);
        let sessions = |p: &Path| {
            serde_json::from_slice::<Value>(&fs::read(p.join("settings.json")).unwrap()).unwrap()["sessionDir"].clone()
        };
        assert_eq!(sessions(&first), sessions(&next));
        assert_ne!(
            sessions(&next),
            sessions(&profile(dir.path(), Uuid::new_v4(), &service))
        );
    }
}
