//! An observation of verification, independent of the execution state.
use super::*;

pub(super) fn summary(task: &TaskRecord, evidence: &[Value]) -> Value {
    let mut latest = std::collections::BTreeMap::new();
    for item in evidence {
        if item["task_id"].as_str() == Some(&task.task_id.to_string())
            && item["attempt_id"].as_str()
                == task
                    .last_attempt_id
                    .as_ref()
                    .map(|id| id.to_string())
                    .as_deref()
            && let Some(path) = item.pointer("/artifact/name").and_then(Value::as_str)
        {
            latest.insert(path, item["passed"] == true);
        }
    }
    let required: Vec<_> = task
        .request
        .expected_artifacts
        .iter()
        .flatten()
        .filter_map(|p| p.to_str())
        .collect();
    let state = if task.status != TaskStatus::Succeeded {
        "not_ready"
    } else if !required.is_empty() {
        if required.iter().any(|p| latest.get(p) == Some(&false)) {
            "failed"
        } else if required.iter().all(|p| latest.get(p) == Some(&true)) {
            "passed"
        } else {
            "pending"
        }
    } else if latest.values().any(|v| !v) {
        "failed"
    } else if !latest.is_empty() {
        "passed"
    } else {
        "not_required"
    };
    json!({"state":state,"observed_at":now(),"cached":false})
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn success_is_not_acceptance_and_latest_failed_check_wins() {
        let mut request = TaskSubmit::new(Uuid::new_v4(), "fixture", "fixture only");
        request.expected_artifacts = Some(vec!["out/health.json".into()]);
        let task = TaskRecord {
            task_id: request.task_id,
            origin_node_id: Uuid::new_v4(),
            target_node_id: Uuid::new_v4(),
            agent_id: request.target_agent_id,
            project_id: request.project_id.clone(),
            status: TaskStatus::Succeeded,
            created_at: now(),
            updated_at: now(),
            last_attempt_id: Some(Uuid::new_v4()),
            request,
            error: None,
        };
        assert_eq!(summary(&task, &[])["state"], "pending");
        let pass = json!({"task_id":task.task_id,"attempt_id":task.last_attempt_id,"artifact":{"name":"out/health.json"},"passed":true});
        assert_eq!(
            summary(&task, std::slice::from_ref(&pass))["state"],
            "passed"
        );
        let mut fail = pass.clone();
        fail["passed"] = json!(false);
        assert_eq!(summary(&task, &[pass.clone(), fail])["state"], "failed");
        let mut stale = pass;
        stale["attempt_id"] = json!(Uuid::new_v4());
        assert_eq!(summary(&task, &[stale])["state"], "pending");
    }
}
