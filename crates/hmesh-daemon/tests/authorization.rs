//! Actual loopback TLS/API authorization. Seeded registry only; no model claim.
use hmesh_core::*;
use hmesh_daemon::{AppState, router};
use hmesh_security::Instance;
use hmesh_store::Store;
use hmesh_transport::{Client, serve};
use serde_json::{Value, json};
use std::sync::Arc;
use tokio::{net::TcpListener, sync::watch};
use uuid::Uuid;

#[tokio::test]
async fn project_grants_sender_identity_ownership_and_approval_are_enforced_over_tls() {
    let temp = tempfile::tempdir().expect("temp");
    let a = Instance::init(&temp.path().join("a"), NodeConfig::default()).expect("a");
    let c = Instance::init(&temp.path().join("c"), NodeConfig::default()).expect("c");
    let b = Arc::new(Instance::init(&temp.path().join("b"), NodeConfig::default()).expect("b"));
    let store = Store::open(&b.dir.join("state.sqlite3")).expect("store");
    store.trust_peer(&a.card).expect("trust a");
    store.trust_peer(&c.card).expect("trust c");
    let project = temp.path().join("project");
    std::fs::create_dir(&project).expect("project");
    let id = Uuid::new_v4();
    let config: AgentConfig = serde_json::from_value(json!({
        "agent":{"name":"registry-fixture","harness":"pi","project_id":"demo","mode":"managed"},
        "project":{"root":project,"allowed_tools":["read","write","edit"],"require_remote_task_approval":true},
        "adapter":{"executable":std::env::current_exe().expect("test executable"),"provider":"fixture","model":"fixture"}
    })).expect("config");
    let card: AgentCard = serde_json::from_value(json!({
        "agent_id":id,"node_id":b.card.node_id,"name":"registry-fixture","harness":"pi","project_ids":["demo"],"availability":"idle","capabilities":[],
        "probe":{"installed":true,"version":"test-registry-fixture","version_supported":true,"protocol_probe":"fixture","authentication":"fixture","managed_run":"passed","streaming":"fixture","cancel":"not_checked","session_resume":"unsupported","unavailable_reason":null}
    })).expect("card");
    store.register_agent(&card, &config).expect("seed registry");
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("listener");
    let endpoint = format!("https://{}", listener.local_addr().expect("addr"));
    let (stop, receiver) = watch::channel(false);
    let state = AppState::load(b.clone(), store.clone(), stop.clone()).expect("state");
    let tls_b = b.clone();
    let tls_store = store.clone();
    let server = tokio::spawn(serve(
        listener,
        router(state),
        Arc::new(move || tls_b.server_config(&tls_store.peers()?)),
        receiver,
    ));
    let peer = Client::new(&a, &endpoint, &b.card.fingerprint, false, false).expect("peer");
    let other = Client::new(&c, &endpoint, &b.card.fingerprint, false, false).expect("other");
    let owner = Client::new(&b, &endpoint, &b.card.fingerprint, false, true).expect("owner");
    let mut envelope = Envelope::new(
        a.card.node_id,
        "task.submit",
        TaskSubmit::new(id, "demo", "must not execute"),
    );
    assert_eq!(
        peer.post::<_, Value>("/hmesh/v1/tasks", &envelope)
            .await
            .expect_err("default deny")
            .code,
        ErrorCode::PermissionDenied
    );
    assert!(store.tasks().expect("tasks").is_empty());
    assert!(
        peer.get::<Vec<AgentCard>>("/hmesh/v1/agents")
            .await
            .expect("filtered registry")
            .is_empty()
    );
    let policy = Policy {
        grant: vec![Grant {
            peer: a.card.node_id,
            project: "demo".into(),
            agent: Some(id),
            actions: vec!["task.submit".into(), "agent.read".into()],
            require_approval: true,
        }],
        ..Policy::default()
    };
    owner
        .post::<_, Policy>("/local/policy", &policy)
        .await
        .expect("grant");
    assert_eq!(
        peer.get::<Vec<AgentCard>>("/hmesh/v1/agents")
            .await
            .expect("registry")
            .len(),
        1
    );
    let mut spoof = envelope.clone();
    spoof.sender_node_id = c.card.node_id;
    assert_eq!(
        peer.post::<_, Value>("/hmesh/v1/tasks", &spoof)
            .await
            .expect_err("spoof")
            .code,
        ErrorCode::Unauthenticated
    );
    let mut incompatible = envelope.clone();
    incompatible.version = "hmesh/99".into();
    assert_eq!(
        peer.post::<_, Value>("/hmesh/v1/tasks", &incompatible)
            .await
            .expect_err("version")
            .code,
        ErrorCode::UnsupportedVersion
    );
    let mut wrong_project = envelope.clone();
    wrong_project.payload.project_id = "unregistered".into();
    assert_eq!(
        peer.post::<_, Value>("/hmesh/v1/tasks", &wrong_project)
            .await
            .expect_err("project")
            .code,
        ErrorCode::ProjectNotFound
    );
    let mut injected = serde_json::to_value(&envelope).expect("json");
    injected["payload"]["cwd"] = json!("C:/unregistered");
    assert_eq!(
        peer.post::<_, Value>("/hmesh/v1/tasks", &injected)
            .await
            .expect_err("cwd override")
            .code,
        ErrorCode::InvalidInput
    );
    let task: TaskRecord = peer
        .post("/hmesh/v1/tasks", &envelope)
        .await
        .expect("pending");
    assert_eq!(task.status, TaskStatus::PendingApproval);
    let path = format!("/hmesh/v1/tasks/{}", task.task_id);
    for suffix in ["", "/events", "/verification"] {
        assert_eq!(
            other
                .get::<Value>(&format!("{path}{suffix}"))
                .await
                .expect_err("foreign task")
                .code,
            ErrorCode::PermissionDenied
        );
    }
    assert_eq!(
        other
            .post::<_, Value>(&format!("{path}/cancel"), &json!({}))
            .await
            .expect_err("foreign cancel")
            .code,
        ErrorCode::PermissionDenied
    );
    assert_eq!(
        peer.post::<_, Value>(
            &format!("/local/tasks/{}/approve", task.task_id),
            &json!({})
        )
        .await
        .expect_err("peer approval")
        .code,
        ErrorCode::PermissionDenied
    );
    let rejected: TaskRecord = owner
        .post(&format!("/local/tasks/{}/reject", task.task_id), &json!({}))
        .await
        .expect("reject");
    assert_eq!(rejected.status, TaskStatus::Rejected);
    let duplicate: TaskRecord = peer
        .post("/hmesh/v1/tasks", &envelope)
        .await
        .expect("dedup");
    assert_eq!(duplicate.status, TaskStatus::Rejected);
    assert_eq!(store.attempt_count(task.task_id).expect("attempt count"), 0);
    envelope.request_id = Uuid::new_v4();
    envelope.payload.task_id = Uuid::new_v4();
    let task: TaskRecord = peer
        .post("/hmesh/v1/tasks", &envelope)
        .await
        .expect("new pending");
    let cancelled: TaskRecord = peer
        .post(
            &format!("/hmesh/v1/tasks/{}/cancel", task.task_id),
            &json!({}),
        )
        .await
        .expect("pre-start cancel");
    assert_eq!(cancelled.status, TaskStatus::Cancelled);
    assert_eq!(store.attempt_count(task.task_id).expect("attempt count"), 0);
    // A malformed hot reload retains the previous valid policy. A valid revoke applies immediately.
    std::fs::write(b.dir.join("policy.toml"), "not valid [").expect("bad reload");
    assert_eq!(
        peer.get::<Vec<AgentCard>>("/hmesh/v1/agents")
            .await
            .expect("last valid policy")
            .len(),
        1
    );
    owner
        .post::<_, Policy>("/local/policy", &Policy::default())
        .await
        .expect("revoke grant");
    envelope.request_id = Uuid::new_v4();
    envelope.payload.task_id = Uuid::new_v4();
    assert_eq!(
        peer.post::<_, Value>("/hmesh/v1/tasks", &envelope)
            .await
            .expect_err("revoked grant")
            .code,
        ErrorCode::PermissionDenied
    );
    store.revoke_peer(a.card.node_id).expect("revoke trust");
    assert!(peer.get::<Value>("/hmesh/v1/node").await.is_err());
    assert_eq!(std::fs::read_dir(project).expect("project").count(), 0);
    let audits = store.audits().expect("audits");
    assert!(audits.iter().any(|a| a["action"] == "task.reject"));
    assert!(audits.iter().any(|a| a["action"] == "task.cancel"));
    stop.send(true).expect("stop");
    server.await.expect("server join").expect("server");
}
