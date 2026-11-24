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
async fn tailnet_router_blocks_every_owner_namespace_even_with_owner_certificate_and_token() {
    // Real TLS with the Tailnet route filter on a local test socket; no Mac/model claim.
    let root = tempfile::tempdir().unwrap();
    let b = Arc::new(Instance::init(&root.path().join("owner"), NodeConfig::default()).unwrap());
    let a = Instance::init(&root.path().join("peer"), NodeConfig::default()).unwrap();
    let store = Store::open(&b.dir.join("state.sqlite3")).unwrap();
    store.trust_peer(&a.card).unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let endpoint = format!("https://{}", listener.local_addr().unwrap());
    let (shutdown, receiver) = watch::channel(false);
    let state = AppState::load(b.clone(), store.clone(), shutdown.clone()).unwrap();
    let server = b.clone();
    let task = tokio::spawn(serve(
        listener,
        hmesh_daemon::tailnet_router(state),
        Arc::new(move || server.server_config(&store.peers()?)),
        receiver,
    ));
    let owner = Client::new(&b, &endpoint, &b.card.fingerprint, false, true).unwrap();
    for path in [
        "/local/status",
        "/local/pairs",
        "/local/services",
        "/local/integrations",
        "/local/templates",
        "/local/workflows",
        "/local/diagnostics",
        "/local/storage",
        "/local/usage",
        "/local/budgets",
        "/local/compatibility",
    ] {
        assert_eq!(
            owner.get::<Value>(path).await.unwrap_err().code,
            ErrorCode::PermissionDenied
        );
    }
    assert_eq!(
        owner
            .post::<_, Value>("/local/stop", &json!({}))
            .await
            .unwrap_err()
            .code,
        ErrorCode::PermissionDenied
    );
    let peer = Client::new(&a, &endpoint, &b.card.fingerprint, false, false).unwrap();
    assert_eq!(
        peer.get::<NodeCard>("/hmesh/v1/node")
            .await
            .unwrap()
            .node_id,
        b.card.node_id
    );
    let anonymous = Client::new(&a, &endpoint, &b.card.fingerprint, true, false).unwrap();
    anonymous.get::<NodeCard>("/hmesh/v1/hello").await.unwrap();
    assert_eq!(
        anonymous
            .get::<Value>("/hmesh/v1/agents")
            .await
            .unwrap_err()
            .code,
        ErrorCode::Unauthenticated
    );
    shutdown.send(true).unwrap();
    task.await.unwrap().unwrap();
}

#[tokio::test]
async fn project_grants_sender_identity_ownership_and_approval_are_enforced_over_tls() {
    let temp = tempfile::tempdir().expect("temp");
    let a = Instance::init(&temp.path().join("a"), NodeConfig::default()).expect("a");
    let c = Instance::init(&temp.path().join("c"), NodeConfig::default()).expect("c");
    let quota_id = Uuid::new_v4();
    let mut node_config = NodeConfig::default();
    node_config.runtime.agent_limits.insert(
        quota_id,
        QueueLimits {
            max_queue_depth: 1,
            ..Default::default()
        },
    );
    let b = Arc::new(Instance::init(&temp.path().join("b"), node_config).expect("b"));
    let store = Store::open(&b.dir.join("state.sqlite3")).expect("store");
    store.trust_peer(&a.card).expect("trust a");
    store.trust_peer(&c.card).expect("trust c");
    let project = temp.path().join("project");
    std::fs::create_dir(&project).expect("project");
    let id = Uuid::new_v4();
    let config: AgentConfig = serde_json::from_value(json!({
        "agent":{"name":"registry-fixture","harness":"pi","project_id":"demo","mode":"managed"},
        "project":{"root":project,"allowed_tools":["read","write","edit"],"require_remote_task_approval":true,"artifact_paths":["out/health.json"]},
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
    // Owner-only reference preview must deny peers before reading any local reference.
    let mut onboarding =
        json!({"agent_id":Uuid::new_v4(),"peer_id":a.card.node_id,"config":config.clone()});
    onboarding["config"]["agent"]["name"] = json!("onboard-fixture");
    assert_eq!(
        peer.post::<_, Value>("/local/onboarding/preview", &onboarding)
            .await
            .unwrap_err()
            .code,
        ErrorCode::PermissionDenied
    );
    let preview: Value = owner
        .post("/local/onboarding/preview", &onboarding)
        .await
        .unwrap();
    let exact: Policy = toml::from_str(preview["policy_preview"].as_str().unwrap()).unwrap();
    assert_eq!(exact.grant.len(), 1);
    assert_eq!(exact.grant[0].peer, a.card.node_id);
    assert_eq!(
        exact.grant[0].agent,
        Some(Uuid::parse_str(onboarding["agent_id"].as_str().unwrap()).unwrap())
    );
    assert!(exact.grant[0].require_approval);
    onboarding["expected_sha256"] = preview["configuration_sha256"].clone();
    onboarding["config"]["adapter"]["model"] = json!("changed-after-preview");
    assert_eq!(
        owner
            .post::<_, Value>("/local/onboarding/register", &onboarding)
            .await
            .unwrap_err()
            .code,
        ErrorCode::TaskConflict
    );
    assert_eq!(store.agents().unwrap().len(), 1);
    onboarding["peer_id"] = json!(Uuid::new_v4());
    assert!(
        owner
            .post::<_, Value>("/local/onboarding/preview", &onboarding)
            .await
            .is_err()
    );
    let mut envelope = Envelope::new(
        a.card.node_id,
        "task.submit",
        TaskSubmit::new(id, "demo", "must not execute"),
    );
    assert!(!envelope.payload.requires_approval);
    // Pairing is the trust boundary: a paired peer sees the registry with no grant.
    assert_eq!(
        peer.get::<Vec<AgentCard>>("/hmesh/v1/agents")
            .await
            .expect("registry")
            .len(),
        1
    );
    // Peers cannot change their own access mode; the executing owner chooses it.
    assert_eq!(
        peer.post::<_, Value>(
            &format!("/local/peers/{}/access", a.card.node_id),
            &json!({"access":"auto"})
        )
        .await
        .unwrap_err()
        .code,
        ErrorCode::PermissionDenied
    );
    let switched: Value = owner
        .post(
            &format!("/local/peers/{}/access", a.card.node_id),
            &json!({"access":"confirm"}),
        )
        .await
        .expect("confirm this peer");
    assert_eq!(switched["access"], "confirm");
    assert!(
        std::fs::read_to_string(b.dir.join("policy.toml"))
            .unwrap()
            .contains(&a.card.node_id.to_string())
    );
    assert_eq!(
        owner
            .post::<_, Value>(
                &format!("/local/peers/{}/access", Uuid::new_v4()),
                &json!({"access":"confirm"})
            )
            .await
            .unwrap_err()
            .code,
        ErrorCode::InvalidInput
    );
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
    // An agent only works in its registered project, whoever asks.
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
    // Synthetic success only; no executor runs. Checks remain origin/project/file scoped.
    std::fs::create_dir_all(project.join("out")).unwrap();
    std::fs::write(project.join("out/health.json"), br#"{"status":"ok"}"#).unwrap();
    let synthetic = Envelope::new(
        a.card.node_id,
        "task.submit",
        TaskSubmit::new(id, "demo", "synthetic finished artifact"),
    );
    let finished = store.admit(b.card.node_id, &synthetic, false, 20).unwrap();
    let active = store.claim_next().unwrap().unwrap();
    let entry = hmesh_store::SnapshotEntry {
        name: "out/health.json".into(),
        bytes: Some(br#"{"status":"ok"}"#.to_vec()),
        error: None,
    };
    store
        .complete_with_artifacts(
            finished.task_id,
            active.last_attempt_id.unwrap(),
            "original-configuration",
            &[entry],
        )
        .unwrap();
    std::fs::write(
        project.join("out/health.json"),
        br#"{"status":"later-edit"}"#,
    )
    .unwrap();
    // Large Unicode patches stay below the existing per-response transport budget and remain owner-only.
    let patch = "界".repeat(50000);
    let patch_hash = hmesh_security::sha256_hex(patch.as_bytes());
    store
        .workspace_put(
            finished.task_id,
            &json!({"state":"review","patch":patch,"patch_sha256":patch_hash}),
        )
        .unwrap();
    let workspace_path = format!("/local/tasks/{}/workspace", finished.task_id);
    assert_eq!(
        peer.get::<Value>(&workspace_path).await.unwrap_err().code,
        ErrorCode::PermissionDenied
    );
    let metadata: Value = owner.get(&workspace_path).await.unwrap();
    assert!(metadata.get("patch").is_none());
    assert_eq!(metadata["patch_size_bytes"], patch.len());
    let mut offset = 0;
    let mut restored = String::new();
    loop {
        let page: Value = owner
            .get(&format!("{workspace_path}?diff=true&offset={offset}"))
            .await
            .unwrap();
        assert_eq!(page["patch_offset"], offset);
        assert_eq!(page["patch_sha256"], patch_hash);
        assert!(serde_json::to_vec(&page).unwrap().len() < 1048576);
        restored.push_str(page["patch"].as_str().unwrap());
        match page["patch_next_offset"].as_u64() {
            Some(next) => offset = next as usize,
            None => break,
        }
    }
    assert_eq!(restored, patch);
    assert!(
        owner
            .get::<Value>(&format!("{workspace_path}?diff=true&offset=1"))
            .await
            .is_err()
    );
    let artifact_path = format!("/hmesh/v1/tasks/{}/artifacts", finished.task_id);
    let verify_path = format!("/hmesh/v1/tasks/{}/verify", finished.task_id);
    let allowed: Value = peer.get(&artifact_path).await.unwrap();
    assert_eq!(allowed["artifacts"], json!(["out/health.json"]));
    let check = json!({"artifact":"out/health.json","expected":{"status":"ok"}});
    assert_eq!(
        other.get::<Value>(&artifact_path).await.unwrap_err().code,
        ErrorCode::PermissionDenied
    );
    assert_eq!(
        other
            .post::<_, Value>(&verify_path, &check)
            .await
            .unwrap_err()
            .code,
        ErrorCode::PermissionDenied
    );
    assert_eq!(
        peer.post::<_, Value>(
            &verify_path,
            &json!({"artifact":"../private.json","expected":{"status":"ok"}})
        )
        .await
        .unwrap_err()
        .code,
        ErrorCode::InvalidInput
    );
    let verified: Value = peer.post(&verify_path, &check).await.unwrap();
    assert_eq!(verified["passed"], true);
    assert_eq!(verified["task_id"], finished.task_id.to_string());
    assert!(!verified["attempt_id"].is_null());
    assert_eq!(verified["configuration_sha256"], "original-configuration");
    assert_eq!(verified["source"], "completion-snapshot");
    let failed: Value = peer
        .post(
            &verify_path,
            &json!({"artifact":"out/health.json","expected":{"status":"wrong"}}),
        )
        .await
        .unwrap();
    assert_eq!(failed["passed"], false);
    assert_eq!(
        store.task(finished.task_id).unwrap().status,
        TaskStatus::Succeeded
    );
    assert_eq!(store.attempt_count(finished.task_id).unwrap(), 1);
    let request_id = Uuid::new_v4();
    let mut local = TaskSubmit::new(id, "demo", "local owner request");
    local.task_id = request_id;
    let local_request = json!({"peer_id":b.card.node_id,"request_id":request_id,"task":local});
    let submitted: TaskRecord = owner
        .post("/local/tasks/submit", &local_request)
        .await
        .unwrap();
    // The owner's own request needs no approval; no executor runs in this router test.
    assert_eq!(submitted.status, TaskStatus::Queued);
    let recovered: TaskRecord = owner
        .post(&format!("/local/requests/{request_id}/recover"), &json!({}))
        .await
        .unwrap();
    assert_eq!(recovered.task_id, submitted.task_id);
    assert!(recovered.last_attempt_id.is_none());
    let mut conflict = local_request.clone();
    conflict["task"]["goal"] = json!("different payload");
    assert_eq!(
        owner
            .post::<_, Value>("/local/tasks/submit", &conflict)
            .await
            .unwrap_err()
            .code,
        ErrorCode::TaskConflict
    );
    let observations = store.outbound_tasks().unwrap();
    assert_eq!(observations[0]["request_id"], request_id.to_string());
    assert!(observations[0]["last_observed_at"].is_i64());
    // A malformed hot reload keeps the last valid policy: this peer stays on confirm.
    std::fs::write(b.dir.join("policy.toml"), "not valid [").expect("bad reload");
    envelope.request_id = Uuid::new_v4();
    envelope.payload.task_id = Uuid::new_v4();
    let still_confirm: TaskRecord = peer
        .post("/hmesh/v1/tasks", &envelope)
        .await
        .expect("last valid policy");
    assert_eq!(still_confirm.status, TaskStatus::PendingApproval);
    // A valid policy write applies immediately; the default access mode is auto.
    owner
        .post::<_, Policy>("/local/policy", &Policy::default())
        .await
        .expect("reset access");
    envelope.request_id = Uuid::new_v4();
    envelope.payload.task_id = Uuid::new_v4();
    let auto: TaskRecord = peer
        .post("/hmesh/v1/tasks", &envelope)
        .await
        .expect("auto access");
    assert_eq!(auto.status, TaskStatus::Queued);
    // Revoking the pairing is the one switch that removes every peer capability.
    store.revoke_peer(a.card.node_id).expect("revoke trust");
    assert!(peer.get::<Value>("/hmesh/v1/node").await.is_err());
    assert!(peer.get::<Value>("/hmesh/v1/agents").await.is_err());
    assert!(peer.get::<Value>(&artifact_path).await.is_err());
    assert!(peer.post::<_, Value>(&verify_path, &check).await.is_err());
    envelope.request_id = Uuid::new_v4();
    envelope.payload.task_id = Uuid::new_v4();
    assert!(
        peer.post::<_, Value>("/hmesh/v1/tasks", &envelope)
            .await
            .is_err()
    );
    assert_eq!(std::fs::read_dir(&project).expect("project").count(), 1);
    assert_eq!(
        std::fs::read(project.join("out/health.json")).unwrap(),
        br#"{"status":"later-edit"}"#
    );
    // Router-only test: no executor/model runs. Owner verification uses the same scoped admission caps.
    let mut quota_config = config.clone();
    quota_config.agent.name = "quota-fixture".into();
    quota_config.agent.project_id = "quota-test".into();
    let mut quota_card = card.clone();
    quota_card.agent_id = quota_id;
    quota_card.name = quota_config.agent.name.clone();
    quota_card.project_ids = vec!["quota-test".into()];
    store.register_agent(&quota_card, &quota_config).unwrap();
    let quota_path = format!("/local/agents/{quota_id}/verify");
    let body = json!({"allow_model":true,"prompt":"synthetic quota request; never executed"});
    let reserved: TaskRecord = owner.post(&quota_path, &body).await.unwrap();
    assert!(reserved.last_attempt_id.is_none());
    assert_eq!(
        owner
            .post::<_, TaskRecord>(&quota_path, &body)
            .await
            .unwrap_err()
            .code,
        ErrorCode::RateLimited
    );
    assert_eq!(store.attempt_count(reserved.task_id).unwrap(), 0);
    store.request_cancel(reserved.task_id).unwrap();
    let audits = store.audits().expect("audits");
    assert!(audits.iter().any(|a| a["action"] == "task.reject"));
    assert!(audits.iter().any(|a| a["action"] == "task.cancel"));
    stop.send(true).expect("stop");
    server.await.expect("server join").expect("server");
}
