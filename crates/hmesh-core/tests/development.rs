use hmesh_core::development::*;
use serde_json::json;
use uuid::Uuid;
fn template() -> TaskTemplate {
    serde_json::from_value(json!({"id":Uuid::new_v4(),"revision":1,"name":"fixture","peer_id":Uuid::new_v4(),"agent_id":Uuid::new_v4(),"project_id":"demo","goal":"Implement {{goal}}","limits":{"timeout_seconds":10,"max_output_bytes":1048576},"expected_tools":["read","write","edit"],"verification":{"artifact":"out/health.json","expected":{"status":"ok"}}})).unwrap()
}
#[test]
fn template_parameters_are_literal_and_never_disable_approval() {
    let t = template();
    t.validate().unwrap();
    let id = Uuid::new_v4();
    let task = t.task(id, "$(unsafe); `command` {{goal}}").unwrap();
    assert_eq!(task.task_id, id);
    assert!(task.requires_approval);
    assert_eq!(task.goal, "Implement $(unsafe); `command` {{goal}}");
    assert!(t.task(id, &"x".repeat(65536)).is_err());
    let mut bad = t.clone();
    bad.verification.as_mut().unwrap().artifact = "../private".into();
    assert!(bad.validate().is_err());
    let mut raw = serde_json::to_value(t).unwrap();
    raw["requires_approval"] = json!(false);
    assert!(serde_json::from_value::<TaskTemplate>(raw).is_err());
}
#[test]
fn workflow_budget_includes_every_possible_repair_and_freezes_target() {
    let t = template();
    let mut spec = WorkflowSpec {
        id: Uuid::new_v4(),
        name: "pipeline".into(),
        stages: vec![WorkflowStage {
            name: "implement".into(),
            template: t.clone(),
            goal: "fixture".into(),
            repair: Some(t.clone()),
            max_repairs: 2,
        }],
        max_total_timeout_seconds: 30,
    };
    spec.validate().unwrap();
    spec.max_total_timeout_seconds = 29;
    assert!(spec.validate().is_err());
    spec.max_total_timeout_seconds = 30;
    spec.stages[0].repair.as_mut().unwrap().peer_id = Uuid::new_v4();
    assert!(spec.validate().is_err());
    spec.stages[0].repair = Some(t);
    spec.stages[0].max_repairs = 3;
    assert!(spec.validate().is_err());
}
#[test]
fn pricing_needs_explicit_rates_source_and_date() {
    let mut b:ProjectBudget=serde_json::from_value(json!({"project_id":"demo","max_runtime_seconds":60,"max_reported_tokens":null,"input_usd_per_million":null,"output_usd_per_million":null,"price_source":null,"price_as_of":null})).unwrap();
    b.validate().unwrap();
    b.pricing_model = Some("fixture".into());
    b.pricing_provider = Some("fixture".into());
    b.input_usd_per_million = Some(1.0);
    assert!(b.validate().is_err());
    b.output_usd_per_million = Some(2.0);
    b.price_source = Some("owner-fixture".into());
    b.price_as_of = Some("2026-11-04".into());
    b.validate().unwrap();
    b.output_usd_per_million = Some(f64::INFINITY);
    assert!(b.validate().is_err());
}
