//! Optional task version restrictions. They grant neither repository nor network access.
use crate::{MeshError, Result};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "mode", rename_all = "snake_case", deny_unknown_fields)]
pub enum GitVersionRequirement {
    Current {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        expected_commit: Option<String>,
        #[serde(default)]
        require_clean: bool,
    },
    Pinned {
        commit: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GitVersionState {
    pub branch: Option<String>,
    pub commit: String,
    pub dirty: bool,
}

pub fn is_full_commit(value: &str) -> bool {
    matches!(value.len(), 40 | 64) && value.bytes().all(|b| b.is_ascii_hexdigit())
}
impl GitVersionRequirement {
    pub fn validate(&self) -> Result<()> {
        if matches!(
            self,
            Self::Current {
                expected_commit: None,
                ..
            }
        ) {
            return Err(MeshError::input(
                "current Git version requires the observed full executor commit",
            ));
        }
        let commit = match self {
            Self::Current {
                expected_commit, ..
            } => expected_commit.as_deref(),
            Self::Pinned { commit } => Some(commit.as_str()),
        };
        if commit.is_some_and(|value| !is_full_commit(value)) {
            return Err(MeshError::input(
                "Git version requires a full 40- or 64-digit hexadecimal commit",
            ));
        }
        Ok(())
    }
}
impl GitVersionState {
    pub fn validate(&self) -> Result<()> {
        if !is_full_commit(&self.commit)
            || self.branch.as_ref().is_some_and(|branch| {
                branch.is_empty() || branch.len() > 256 || branch.chars().any(char::is_control)
            })
        {
            return Err(MeshError::input("invalid origin Git observation"));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn version_restrictions_accept_only_full_commits_and_known_fields() {
        for commit in [
            "main",
            "abc123",
            "--help",
            "https://example.invalid/code",
            &"a".repeat(41),
        ] {
            assert!(
                GitVersionRequirement::Pinned {
                    commit: commit.into()
                }
                .validate()
                .is_err()
            );
        }
        assert!(
            GitVersionRequirement::Pinned {
                commit: "a".repeat(40)
            }
            .validate()
            .is_ok()
        );
        assert!(
            GitVersionRequirement::Current {
                expected_commit: None,
                require_clean: false
            }
            .validate()
            .is_err()
        );
        assert!(
            GitVersionRequirement::Pinned {
                commit: "b".repeat(64)
            }
            .validate()
            .is_ok()
        );
        assert!(
            serde_json::from_value::<GitVersionRequirement>(
                serde_json::json!({"mode":"pinned","commit":"a".repeat(40),"fetch":true})
            )
            .is_err()
        );
    }
    #[test]
    fn legacy_task_serialization_omits_version_fields() {
        let task = crate::TaskSubmit::new(uuid::Uuid::new_v4(), "fixture", "legacy");
        let value = serde_json::to_value(task).unwrap();
        assert!(value.get("git_version").is_none());
        assert!(value.get("origin_git").is_none());
        let restored: crate::TaskSubmit = serde_json::from_value(value).unwrap();
        assert!(restored.git_version.is_none());
    }
}
