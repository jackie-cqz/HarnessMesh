use hmesh_core::managed::*;
use serde_json::json;

#[test]
fn forwards_and_service_restart_scope_cannot_be_widened_by_configuration_injection() {
    let root = std::env::current_dir().unwrap();
    let mut spec = TunnelSpec {
        id: uuid::Uuid::new_v4(),
        project_id: "demo".into(),
        name: "ssh".into(),
        ssh_executable: root.join("ssh"),
        hostname: "127.0.0.1".into(),
        user: "owner".into(),
        port: 22,
        identity_file: root.join("id"),
        known_hosts_file: root.join("known_hosts"),
        forwards: vec![SshForward {
            direction: ForwardDirection::Local,
            bind_port: 18080,
            target_port: 8000,
        }],
        lease_seconds: 30,
    };
    spec.validate().unwrap();
    spec.hostname = "-oProxyCommand=bad".into();
    assert!(spec.validate().is_err());
    assert!(serde_json::from_value::<SshForward>(json!({"direction":"local","bind_port":18080,"target_port":8000,"target_host":"10.1.1.1"})).is_err());
    let service = ServiceSpec {
        id: uuid::Uuid::new_v4(),
        project_id: "demo".into(),
        name: "api".into(),
        target_port: 8000,
        allowed_peer_ids: vec![],
        allowed_restart_peer_ids: vec![uuid::Uuid::new_v4()],
        lease_seconds: 30,
        health_path: "/health".into(),
        request_timeout_seconds: 10,
        process: None,
    };
    assert!(service.validate().is_err());
    for input in [
        "//other.example/path",
        "http://host/",
        "/x\r\nInjected: value",
        "/x\\y",
    ] {
        assert!(!http_path(input));
    }
}

#[test]
fn repair_contract_and_prompt_inputs_have_a_fixed_finite_budget() {
    let mut spec:IntegrationSpec=serde_json::from_value(json!({"id":uuid::Uuid::new_v4(),"connection_id":uuid::Uuid::new_v4(),"backend_peer_id":uuid::Uuid::new_v4(),"backend_agent_id":uuid::Uuid::new_v4(),"backend_project_id":"demo","backend_service_id":uuid::Uuid::new_v4(),"contract":"{}","contract_sha256":"a".repeat(64),"contract_revision":1,"tests":[{"name":"health","path":"/health","expected_status":200,"equals":{"/status":"ok"}}],"max_fix_cycles":2,"repair_timeout_seconds":600,"repair_max_output_bytes":65536})).unwrap();
    spec.validate().unwrap();
    spec.max_fix_cycles = 3;
    assert!(spec.validate().is_err());
    spec.max_fix_cycles = 2;
    spec.contract = format!("\"{}\"", "x".repeat(32768));
    assert!(spec.validate().is_err());
}
