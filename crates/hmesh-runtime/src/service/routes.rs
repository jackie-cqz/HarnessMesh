//! The models a managed Pi, DSH, or OpenCode profile is meant to call.
use super::AgentService;
use hmesh_core::HarnessKind;
use hmesh_core::service::{MODE_API_KEY, MODE_CUSTOM};
use hmesh_core::sessions::ConfigOption;
use serde_json::Value;

pub struct ManagedModels {
    pub default: String,
    allowed: Vec<String>,
    provider: Option<String>,
    dsh: bool,
}
impl ManagedModels {
    pub fn from_service(service: &AgentService, harness: HarnessKind) -> Option<Self> {
        let dsh = harness == HarnessKind::Dsh;
        let (default, allowed, provider) = match (service.mode.as_str(), harness) {
            (MODE_CUSTOM, HarnessKind::Pi | HarnessKind::Dsh | HarnessKind::Opencode) => {
                let wire = |id: String| {
                    if dsh {
                        serde_json::json!(["hmesh", id]).to_string()
                    } else {
                        format!("hmesh/{id}")
                    }
                };
                (
                    service.default_model_value(harness)?,
                    service.model_list().into_iter().map(wire).collect(),
                    None,
                )
            }
            (MODE_API_KEY, HarnessKind::Pi) => {
                let default = service.default_model_value(harness)?;
                let (provider, _) = default.split_once('/')?;
                (default.clone(), Vec::new(), Some(provider.to_owned()))
            }
            (MODE_API_KEY, HarnessKind::Opencode) => (
                service.default_model_value(harness)?,
                Vec::new(),
                Some("opencode".to_owned()),
            ),
            (MODE_API_KEY, HarnessKind::Dsh) => (
                service.default_model_value(harness).unwrap_or_else(|| {
                    serde_json::json!(["deepseek-official", "deepseek-v4-flash"]).to_string()
                }),
                Vec::new(),
                Some("deepseek-official".to_owned()),
            ),
            _ => return None,
        };
        Some(Self {
            default,
            allowed,
            provider,
            dsh,
        })
    }

    pub fn allows(&self, value: &str) -> bool {
        let Some(provider) = &self.provider else {
            return self.allowed.iter().any(|v| v == value);
        };
        if self.dsh {
            serde_json::from_str::<[String; 2]>(value)
                .is_ok_and(|[p, id]| &p == provider && !id.is_empty())
        } else {
            value
                .split_once('/')
                .is_some_and(|(p, id)| p == provider && !id.is_empty())
        }
    }

    pub fn filter(&self, options: &mut [ConfigOption]) {
        for option in options.iter_mut().filter(|o| o.is_category("model")) {
            option.choices.retain(|choice| self.allows(&choice.value));
        }
    }

    pub fn selected<'a>(&'a self, requested: Option<&'a Value>) -> &'a str {
        requested
            .and_then(Value::as_str)
            .filter(|v| self.allows(v))
            .unwrap_or(&self.default)
    }
}
