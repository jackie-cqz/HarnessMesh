//! Owner-authored, bounded development plans. Templates never grant permissions.
use crate::*;
use serde_json::Value;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ArtifactCheck {
    pub artifact: PathBuf,
    pub expected: Value,
}
impl ArtifactCheck {
    pub fn validate(&self) -> Result<()> {
        if self.artifact.as_os_str().is_empty()
            || self
                .artifact
                .components()
                .any(|c| !matches!(c, std::path::Component::Normal(_)))
            || self
                .expected
                .as_object()
                .is_none_or(|o| o.is_empty() || o.len() > 16)
            || serde_json::to_vec(&self.expected).map_or(true, |v| v.len() > 8192)
        {
            return Err(MeshError::input("invalid relative artifact/JSON check"));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TaskTemplate {
    pub id: Uuid,
    pub revision: u32,
    pub name: String,
    pub peer_id: Uuid,
    pub agent_id: Uuid,
    pub project_id: String,
    /// Literal text with optional {{goal}} parameters. No shell expansion.
    pub goal: String,
    pub limits: RunLimits,
    #[serde(default)]
    pub expected_tools: Option<Vec<String>>,
    pub verification: Option<ArtifactCheck>,
}
impl TaskTemplate {
    pub fn validate(&self) -> Result<()> {
        validate_name(&self.name)?;
        if self.revision == 0 || self.revision > 10000 {
            return Err(MeshError::input("invalid template revision"));
        }
        self.task(Uuid::new_v4(), "template preview")?.validate()?;
        if let Some(v) = &self.verification {
            v.validate()?;
        }
        Ok(())
    }
    pub fn task(&self, id: Uuid, goal: &str) -> Result<TaskSubmit> {
        if goal.len() > MAX_GOAL_BYTES {
            return Err(MeshError::input("goal parameter too large"));
        }
        let mut task = TaskSubmit::new(
            self.agent_id,
            &self.project_id,
            &self.goal.replace("{{goal}}", goal),
        );
        task.task_id = id;
        task.limits = self.limits.clone();
        task.expected_tools = self.expected_tools.clone();
        task.validate()?;
        Ok(task)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorkflowStage {
    pub name: String,
    pub template: TaskTemplate,
    pub goal: String,
    pub repair: Option<TaskTemplate>,
    #[serde(default)]
    pub max_repairs: u8,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorkflowSpec {
    pub id: Uuid,
    pub name: String,
    pub stages: Vec<WorkflowStage>,
    pub max_total_timeout_seconds: u64,
}
impl WorkflowSpec {
    pub fn validate(&self) -> Result<()> {
        validate_name(&self.name)?;
        if self.stages.is_empty() || self.stages.len() > 8 || self.max_total_timeout_seconds > 14400
        {
            return Err(MeshError::input(
                "workflow requires 1..8 stages and at most four hours of task time",
            ));
        }
        let mut total = 0;
        for stage in &self.stages {
            validate_name(&stage.name)?;
            stage.template.validate()?;
            stage.template.task(Uuid::new_v4(), &stage.goal)?;
            total += stage.template.limits.timeout_seconds;
            if stage.max_repairs > 2 || (stage.max_repairs > 0) != stage.repair.is_some() {
                return Err(MeshError::input(
                    "repair requires a template and a limit of 1..2",
                ));
            }
            if let Some(repair) = &stage.repair {
                repair.validate()?;
                if stage.template.verification.is_none()
                    || repair.peer_id != stage.template.peer_id
                    || repair.project_id != stage.template.project_id
                {
                    return Err(MeshError::input(
                        "repair needs a frozen check and the same peer/project",
                    ));
                }
                total += repair.limits.timeout_seconds * u64::from(stage.max_repairs);
            }
        }
        if total > self.max_total_timeout_seconds {
            return Err(MeshError::input("workflow task-time budget exceeded"));
        }
        Ok(())
    }
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PlannedTask {
    pub request_id: Uuid,
    pub peer_id: Uuid,
    pub task: TaskSubmit,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorkflowRun {
    pub spec: WorkflowSpec,
    pub state: String,
    pub stage: usize,
    pub repair_index: u8,
    /// Preallocated requests are persisted before any network side effect.
    pub plans: Vec<Vec<PlannedTask>>,
    pub reports: Vec<Value>,
    pub last_error: Option<ErrorCode>,
    pub updated_at: i64,
    pub revision: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProjectBudget {
    pub project_id: String,
    pub max_runtime_seconds: u64,
    /// Stops after observed reports reach the cap; not a prepaid/provider hard limit.
    pub max_reported_tokens: Option<u64>,
    pub input_usd_per_million: Option<f64>,
    pub output_usd_per_million: Option<f64>,
    pub price_source: Option<String>,
    pub price_as_of: Option<String>,
    pub pricing_model: Option<String>,
    pub pricing_provider: Option<String>,
}
impl ProjectBudget {
    pub fn validate(&self) -> Result<()> {
        validate_name(&self.project_id)?;
        if !(1..=1800).contains(&self.max_runtime_seconds)
            || self
                .max_reported_tokens
                .is_some_and(|n| n == 0 || n > 1_000_000_000)
        {
            return Err(MeshError::input("invalid project run budget"));
        }
        let pricing = self.input_usd_per_million.is_some() || self.output_usd_per_million.is_some();
        if pricing
            && (self
                .pricing_model
                .as_ref()
                .is_none_or(|s| s.is_empty() || s.len() > 256)
                || self
                    .pricing_provider
                    .as_ref()
                    .is_none_or(|s| s.is_empty() || s.len() > 256))
        {
            return Err(MeshError::input(
                "pricing requires the exact configured model and provider",
            ));
        }
        if pricing
            && (self.input_usd_per_million.is_none()
                || self.output_usd_per_million.is_none()
                || self
                    .price_source
                    .as_ref()
                    .is_none_or(|s| s.is_empty() || s.len() > 256)
                || self.price_as_of.as_ref().is_none_or(|s| {
                    s.len() != 10
                        || time::format_description::parse("[year]-[month]-[day]")
                            .ok()
                            .and_then(|format| time::Date::parse(s, &format).ok())
                            .is_none()
                }))
            || [self.input_usd_per_million, self.output_usd_per_million]
                .into_iter()
                .flatten()
                .any(|p| !p.is_finite() || !(0.0..=1_000_000.0).contains(&p))
        {
            return Err(MeshError::input(
                "pricing requires finite rates, a source and YYYY-MM-DD date",
            ));
        }
        Ok(())
    }
}
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UsageTotals {
    pub configured_model: Option<String>,
    pub configured_provider: Option<String>,
    pub input_tokens: Option<u64>,
    pub output_tokens: Option<u64>,
    pub cache_read_tokens: Option<u64>,
    pub cache_write_tokens: Option<u64>,
    pub total_tokens: Option<u64>,
    pub reports: u64,
    pub source: Option<String>,
}
