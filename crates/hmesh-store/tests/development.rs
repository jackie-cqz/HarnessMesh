use hmesh_core::{development::*, *};
use hmesh_store::{SnapshotEntry, Store};
use serde_json::json;
use uuid::Uuid;
fn template() -> TaskTemplate {
    serde_json::from_value(json!({"id":Uuid::new_v4(),"revision":1,"name":"fixture","peer_id":Uuid::new_v4(),"agent_id":Uuid::new_v4(),"project_id":"fixture","goal":"fixture:create","limits":{"timeout_seconds":10,"max_output_bytes":1048576},"verification":null})).unwrap()
}
#[test]
fn template_library_byte_budget_counts_unicode_bytes() {
    let temp = tempfile::tempdir().unwrap();
    let s = Store::open(&temp.path().join("db")).unwrap();
    let mut t = template();
    t.goal = "中".repeat(20000);
    for _ in 0..8 {
        t.id = Uuid::new_v4();
        s.save_template(&t).unwrap();
    }
    t.id = Uuid::new_v4();
    assert_eq!(
        s.save_template(&t).unwrap_err().code,
        ErrorCode::RateLimited
    );
}
#[test]
fn immutable_template_revisions_and_library_budget_survive_restart() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("db");
    let s = Store::open(&path).unwrap();
    let mut t = template();
    s.save_template(&t).unwrap();
    s.save_template(&t).unwrap();
    t.goal = "changed".into();
    assert_eq!(
        s.save_template(&t).unwrap_err().code,
        ErrorCode::TaskConflict
    );
    t.revision = 2;
    s.save_template(&t).unwrap();
    drop(s);
    let s = Store::open(&path).unwrap();
    assert_eq!(s.templates().unwrap().len(), 2);
    t.revision = 4;
    assert!(s.save_template(&t).is_err());
}
#[test]
fn usage_is_attempt_bound_unknown_stays_unknown_and_cache_cost_is_not_guessed() {
    let temp = tempfile::tempdir().unwrap();
    let s = Store::open(&temp.path().join("db")).unwrap();
    let e = Envelope::new(
        Uuid::new_v4(),
        "task.submit",
        TaskSubmit::new(Uuid::new_v4(), "fixture", "fixture"),
    );
    let t = s.admit(Uuid::new_v4(), &e, false, 20).unwrap();
    let running = s.claim_next().unwrap().unwrap();
    assert!(s.usage(t.task_id).unwrap().total_tokens.is_none());
    let mut usage = UsageTotals {
        configured_model: Some("fixture".into()),
        configured_provider: Some("fixture".into()),
        input_tokens: Some(30),
        output_tokens: Some(10),
        cache_read_tokens: Some(0),
        cache_write_tokens: Some(0),
        total_tokens: Some(40),
        reports: 1,
        source: Some("fixture".into()),
    };
    assert!(s.record_usage(t.task_id, Uuid::new_v4(), &usage).is_err());
    s.record_usage(t.task_id, running.last_attempt_id.unwrap(), &usage)
        .unwrap();
    let b:ProjectBudget=serde_json::from_value(json!({"project_id":"fixture","max_runtime_seconds":10,"max_reported_tokens":100,"input_usd_per_million":1.0,"output_usd_per_million":2.0,"pricing_model":"fixture","pricing_provider":"fixture","price_source":"fixture-rate","price_as_of":"2026-11-04"})).unwrap();
    s.set_project_budget(&b).unwrap();
    let report = s.usage_report().unwrap();
    assert!((report["rows"][0]["estimated_usd"].as_f64().unwrap() - 0.00005).abs() < 1e-12);
    usage.cache_read_tokens = Some(50);
    s.record_usage(t.task_id, running.last_attempt_id.unwrap(), &usage)
        .unwrap();
    assert!(s.usage_report().unwrap()["rows"][0]["estimated_usd"].is_null());
}
#[test]
fn snapshot_purge_preserves_history_and_marks_unavailable_instead_of_reading_current_file() {
    let temp = tempfile::tempdir().unwrap();
    let s = Store::open(&temp.path().join("db")).unwrap();
    let e = Envelope::new(
        Uuid::new_v4(),
        "task.submit",
        TaskSubmit::new(Uuid::new_v4(), "fixture", "fixture"),
    );

    let config:hmesh_core::AgentConfig=serde_json::from_value(json!({"agent":{"name":"retention-fixture","harness":"pi","project_id":"fixture"},"project":{"root":std::env::temp_dir(),"allowed_tools":["read"]},"adapter":{"executable":std::env::current_exe().unwrap(),"provider":"fixture","model":"fixture"}})).unwrap();
    let card:hmesh_core::AgentCard=serde_json::from_value(json!({"agent_id":e.payload.target_agent_id,"node_id":Uuid::new_v4(),"name":"retention-fixture","harness":"pi","project_ids":["fixture"],"availability":"unverified","capabilities":[],"probe":{"installed":true,"version":null,"version_supported":true,"protocol_probe":"fixture","authentication":"not_checked","managed_run":"not_checked","streaming":"not_checked","cancel":"not_checked","session_resume":"not_checked","unavailable_reason":null}})).unwrap();
    s.register_agent(&card, &config).unwrap();
    let t = s.admit(Uuid::new_v4(), &e, false, 20).unwrap();
    let r = s.claim_next().unwrap().unwrap();
    assert!(s.purge_snapshots(&[t.task_id]).is_err());
    s.complete_with_artifacts(
        t.task_id,
        r.last_attempt_id.unwrap(),
        "config",
        &[SnapshotEntry {
            name: "out/health.json".into(),
            bytes: Some(b"original".to_vec()),
            error: None,
        }],
    )
    .unwrap();
    assert_eq!(s.storage_summary().unwrap()["snapshot_bytes"], 8);
    assert_eq!(s.purge_snapshots(&[t.task_id]).unwrap(), 1);
    let set = s.artifact_set(t.task_id).unwrap().unwrap();
    assert_eq!(set.entries[0].error.as_deref(), Some("purged_by_owner"));
    assert!(set.entries[0].bytes.is_none());
    assert_eq!(
        s.task(t.task_id).unwrap().last_attempt_id,
        r.last_attempt_id
    );
    assert_eq!(s.attempt_count(t.task_id).unwrap(), 1);
}
#[test]
fn unfinished_workflow_references_and_unknown_tasks_are_retained() {
    let temp = tempfile::tempdir().unwrap();
    let s = Store::open(&temp.path().join("db")).unwrap();
    let original = TaskSubmit::new(Uuid::new_v4(), "fixture", "fixture");
    let e = Envelope::new(Uuid::new_v4(), "task.submit", original.clone());
    let t = s.admit(Uuid::new_v4(), &e, true, 20).unwrap();
    s.transition(t.task_id, TaskStatus::Rejected, None).unwrap();
    let spec = WorkflowSpec {
        id: Uuid::new_v4(),
        name: "flow".into(),
        stages: vec![WorkflowStage {
            name: "one".into(),
            template: template(),
            goal: "".into(),
            repair: None,
            max_repairs: 0,
        }],
        max_total_timeout_seconds: 10,
    };
    let mut flow = WorkflowRun {
        spec,
        state: "review".into(),
        stage: 1,
        repair_index: 0,
        plans: vec![vec![PlannedTask {
            request_id: e.request_id,
            peer_id: Uuid::new_v4(),
            task: original,
        }]],
        reports: vec![],
        last_error: None,
        updated_at: now(),
        revision: 0,
    };
    s.managed_create("workflow", flow.spec.id, &flow).unwrap();
    assert!(s.retention_candidates(now() + 1, 0).unwrap().is_empty());
    flow.state = "completed".into();
    s.managed_put("workflow", flow.spec.id, &flow).unwrap();
    assert_eq!(s.retention_candidates(now() + 1, 0).unwrap().len(), 1);
    let e = Envelope::new(
        Uuid::new_v4(),
        "task.submit",
        TaskSubmit::new(Uuid::new_v4(), "fixture", "unknown"),
    );
    let unknown = s.admit(Uuid::new_v4(), &e, false, 20).unwrap();
    s.claim_next().unwrap().unwrap();
    s.transition(unknown.task_id, TaskStatus::Unknown, None)
        .unwrap();
    assert!(
        !s.retention_candidates(now() + 1, 0)
            .unwrap()
            .iter()
            .any(|t| t.task_id == unknown.task_id)
    );
}
#[test]
fn stale_workflow_worker_cannot_overwrite_cancel() {
    let temp = tempfile::tempdir().unwrap();
    let s = Store::open(&temp.path().join("db")).unwrap();
    let spec = WorkflowSpec {
        id: Uuid::new_v4(),
        name: "flow".into(),
        stages: vec![WorkflowStage {
            name: "one".into(),
            template: template(),
            goal: "".into(),
            repair: None,
            max_repairs: 0,
        }],
        max_total_timeout_seconds: 10,
    };
    let mut r = WorkflowRun {
        spec,
        state: "running".into(),
        stage: 0,
        repair_index: 0,
        plans: vec![],
        reports: vec![],
        last_error: None,
        updated_at: now(),
        revision: 0,
    };
    s.managed_create("workflow", r.spec.id, &r).unwrap();
    let mut stale = r.clone();
    r.state = "cancelling".into();
    r.revision = 1;
    assert!(s.save_workflow(&r, 0).unwrap());
    stale.stage = 1;
    stale.revision = 1;
    assert!(!s.save_workflow(&stale, 0).unwrap());
    assert_eq!(
        s.managed_get::<WorkflowRun>("workflow", r.spec.id)
            .unwrap()
            .state,
        "cancelling"
    );
}
