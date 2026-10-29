//! Real daemon/managed processes and mTLS; fixture HTTP is not model evidence.
use hmesh_core::{managed::*, *};
use hmesh_security::{Instance, sha256_hex};
use hmesh_store::Store;
use hmesh_transport::Client;
use serde_json::{Value, json};
use std::{
    path::PathBuf,
    process::{Child, Command, Stdio},
    time::Duration,
};
use uuid::Uuid;

struct Node {
    instance: Instance,
    store: Store,
    process: Option<Child>,
    agent: Uuid,
    root: PathBuf,
}
impl Drop for Node {
    fn drop(&mut self) {
        if let Some(c) = &mut self.process {
            let _ = c.kill();
            let _ = c.wait();
        }
    }
}
impl Node {
    fn new(root: PathBuf) -> Self {
        std::fs::create_dir_all(&root).unwrap();
        let project = root.join("project");
        std::fs::create_dir(&project).unwrap();
        let mut node = NodeConfig::default();
        let reserved = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        node.node.listen_port = reserved.local_addr().unwrap().port();
        drop(reserved);
        let instance = Instance::init(&root.join("node"), node).unwrap();
        let store = Store::open(&instance.dir.join("state.sqlite3")).unwrap();
        let agent = Uuid::new_v4();
        let config:AgentConfig=serde_json::from_value(json!({"agent":{"name":"fixture","harness":"pi","project_id":"fixture","mode":"managed"},"project":{"root":project,"allowed_tools":["read"],"require_remote_task_approval":true},"adapter":{"executable":env!("CARGO_BIN_EXE_hmesh-fixture"),"provider":"fixture","model":"fixture"}})).unwrap();
        let card:AgentCard=serde_json::from_value(json!({"agent_id":agent,"node_id":instance.card.node_id,"name":"fixture","harness":"pi","project_ids":["fixture"],"availability":"idle","capabilities":[],"probe":{"installed":true,"version":"fixture","version_supported":true,"protocol_probe":"fixture","authentication":"fixture","managed_run":"passed","streaming":"fixture","cancel":"fixture","session_resume":"unsupported","unavailable_reason":null}})).unwrap();
        store.register_agent(&card, &config).unwrap();
        Self {
            instance,
            store,
            process: None,
            agent,
            root,
        }
    }
    async fn start(&mut self) -> Client {
        let mut c = Command::new(env!("CARGO_BIN_EXE_hmesh-fixture"));
        c.arg("--daemon")
            .arg(&self.instance.dir)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(std::fs::File::create(self.root.join("daemon-error.log")).unwrap());
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            c.creation_flags(0x08000000);
        }
        self.process = Some(c.spawn().unwrap());
        for _ in 0..100 {
            if let Ok(c) = hmesh_daemon::local_client(&self.instance)
                && c.get::<Value>("/local/status").await.is_ok()
            {
                return c;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        panic!(
            "daemon startup failed in {:?}: {}",
            self.root,
            std::fs::read_to_string(self.root.join("daemon-error.log")).unwrap()
        );
    }
    async fn stop(&mut self, c: &Client) {
        let _: Value = c.post("/local/stop", &json!({})).await.unwrap();
        for _ in 0..100 {
            if self.process.as_mut().unwrap().try_wait().unwrap().is_some() {
                self.process.take();
                return;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        panic!("daemon did not stop");
    }
    fn service(&self, peer: Uuid, seconds: u64, exit: bool) -> ServiceSpec {
        let socket = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = socket.local_addr().unwrap().port();
        drop(socket);
        ServiceSpec {
            id: Uuid::new_v4(),
            project_id: "fixture".into(),
            name: "fixture-api".into(),
            target_port: port,
            allowed_peer_ids: vec![peer],
            allowed_restart_peer_ids: vec![],
            lease_seconds: seconds,
            health_path: "/health".into(),
            request_timeout_seconds: 3,
            process: Some(ServiceProcess {
                executable: PathBuf::from(env!("CARGO_BIN_EXE_hmesh-fixture")),
                cwd: self.root.join("project"),
                args: if exit {
                    vec!["--exit".into()]
                } else {
                    vec!["--http".into(), port.to_string()]
                },
                env: Default::default(),
            }),
        }
    }
}
async fn status(c: &Client, kind: &str, id: Uuid, predicate: impl Fn(&Value) -> bool) -> Value {
    for _ in 0..260 {
        let records: Vec<Value> = c.get(&format!("/local/{kind}")).await.unwrap();
        if let Some(v) = records.iter().find(|v| v["id"] == id.to_string())
            && predicate(v)
        {
            return v.clone();
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    panic!(
        "{kind} {id} did not reach expected state: {:?}",
        c.get::<Value>(&format!("/local/{kind}")).await
    );
}
#[tokio::test]
async fn managed_process_http_lease_authorization_restart_and_frozen_integration_contract() {
    let temp = tempfile::tempdir().unwrap();
    let mut a = Node::new(temp.path().join("a"));
    let mut b = Node::new(temp.path().join("b"));
    let other = Node::new(temp.path().join("c"));
    a.store.trust_peer(&b.instance.card).unwrap();
    b.store.trust_peer(&a.instance.card).unwrap();
    b.store.trust_peer(&other.instance.card).unwrap();
    let owner_b = b.start().await;
    let peer_b = Client::new(
        &a.instance,
        &serde_json::from_slice::<NodeCard>(
            &std::fs::read(b.instance.dir.join("daemon.json")).unwrap(),
        )
        .unwrap()
        .endpoint,
        &b.instance.card.fingerprint,
        false,
        false,
    )
    .unwrap();
    let other_b = Client::new(
        &other.instance,
        &serde_json::from_slice::<NodeCard>(
            &std::fs::read(b.instance.dir.join("daemon.json")).unwrap(),
        )
        .unwrap()
        .endpoint,
        &b.instance.card.fingerprint,
        false,
        false,
    )
    .unwrap();
    // Use the actual published port in the paired card (this test uses port 0).
    let card: NodeCard = peer_b.get("/hmesh/v1/hello").await.unwrap();
    a.store.trust_peer(&card).unwrap();
    let owner_a = a.start().await;
    let spec = b.service(a.instance.card.node_id, 60, false);
    assert_eq!(
        peer_b
            .post::<_, Value>("/local/services", &spec)
            .await
            .unwrap_err()
            .code,
        ErrorCode::PermissionDenied
    );
    let _: Value = owner_b.post("/local/services", &spec).await.unwrap();
    status(&owner_b, "services", spec.id, |v| {
        v["status"]["http_health"] == "healthy"
    })
    .await;
    assert_eq!(
        other_b
            .get::<ServiceCard>(&format!("/hmesh/v1/services/{}", spec.id))
            .await
            .unwrap_err()
            .code,
        ErrorCode::PermissionDenied
    );
    assert_eq!(
        peer_b
            .post::<_, Value>(
                &format!("/hmesh/v1/services/{}/restart", spec.id),
                &json!({"request_id":Uuid::new_v4()})
            )
            .await
            .unwrap_err()
            .code,
        ErrorCode::PermissionDenied
    );
    let cards: Vec<Value> = peer_b.get("/hmesh/v1/services").await.unwrap();
    assert!(cards[0].get("target_port").is_none());
    assert!(cards[0].get("process").is_none());
    let connection = ConnectSpec {
        id: Uuid::new_v4(),
        peer_id: b.instance.card.node_id,
        service_id: spec.id,
        local_port: 0,
        lease_seconds: 60,
    };
    let _: Value = owner_a
        .post("/local/connections", &connection)
        .await
        .unwrap();
    let v = status(&owner_a, "connections", connection.id, |v| {
        v["status"]["endpoint"].is_string()
    })
    .await;
    let endpoint = v["status"]["endpoint"].as_str().unwrap();
    let http = reqwest::Client::builder()
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .unwrap();
    let value: Value = http
        .get(format!("{endpoint}/health"))
        .header("authorization", "Bearer test-credential-never-forwarded")
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(value["status"], "ok");
    assert_eq!(value["has_auth"], false);
    assert_eq!(
        http.get(format!("{endpoint}/large"))
            .send()
            .await
            .unwrap()
            .status(),
        429
    );
    let redirect = http
        .get(format!("{endpoint}/redirect"))
        .send()
        .await
        .unwrap();
    assert_eq!(redirect.status(), 302);
    assert!(redirect.headers().get("location").is_none());
    let policy = Policy {
        grant: vec![Grant {
            peer: a.instance.card.node_id,
            project: "fixture".into(),
            agent: Some(b.agent),
            actions: vec!["agent.read".into()],
            require_approval: true,
        }],
        ..Policy::default()
    };
    let _: Value = owner_b.post("/local/policy", &policy).await.unwrap();
    let contract = "{\"schema\":\"fixture\"}";
    let mut integration = IntegrationSpec {
        id: Uuid::new_v4(),
        connection_id: connection.id,
        backend_peer_id: b.instance.card.node_id,
        backend_agent_id: b.agent,
        backend_project_id: "fixture".into(),
        backend_service_id: spec.id,
        contract: contract.into(),
        contract_sha256: sha256_hex(contract.as_bytes()),
        contract_revision: 1,
        tests: vec![HttpTest {
            name: "health".into(),
            path: "/health".into(),
            expected_status: 200,
            equals: std::collections::BTreeMap::from([("/status".into(), json!("ok"))]),
        }],
        max_fix_cycles: 0,
        repair_timeout_seconds: 10,
        repair_max_output_bytes: 65536,
    };
    for expected in ["verified", "needs_input"] {
        if expected == "needs_input" {
            integration.id = Uuid::new_v4();
            integration.tests[0]
                .equals
                .insert("/missing".into(), json!(true));
        }
        let _: Value = owner_a
            .post("/local/integrations", &integration)
            .await
            .unwrap();
        for _ in 0..100 {
            let run: IntegrationRun = owner_a
                .get(&format!("/local/integrations/{}", integration.id))
                .await
                .unwrap();
            if matches!(run.state.as_str(), "verified" | "needs_input") {
                assert_eq!(run.state, expected);
                assert_eq!(run.cycles, 0);
                assert!(run.repair_task_ids.is_empty());
                assert!(run.reports[0].get("response_body").is_none());
                break;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        let run: IntegrationRun = owner_a
            .get(&format!("/local/integrations/{}", integration.id))
            .await
            .unwrap();
        assert_eq!(run.state, expected);
        assert_eq!(
            owner_a
                .post::<_, Value>("/local/integrations", &integration)
                .await
                .unwrap_err()
                .code,
            ErrorCode::TaskConflict
        );
    }
    let _ = http.get(format!("{endpoint}/crash")).send().await;
    status(&owner_b, "services", spec.id, |v| {
        v["status"]["starts"].as_u64().unwrap_or(0) >= 2 && v["status"]["http_health"] == "healthy"
    })
    .await;
    let _: Value = owner_b
        .post(&format!("/local/services/{}/revoke", spec.id), &json!({}))
        .await
        .unwrap();
    status(&owner_a, "connections", connection.id, |v| {
        v["status"]["state"] == "closed"
    })
    .await;
    assert!(http.get(format!("{endpoint}/health")).send().await.is_err());
    status(&owner_b, "services", spec.id, |v| {
        v["status"]["process"] == "stopped"
    })
    .await;
    a.stop(&owner_a).await;
    b.stop(&owner_b).await;
}
#[tokio::test]
async fn expiry_restart_budget_and_daemon_restart_never_adopt_old_processes() {
    let temp = tempfile::tempdir().unwrap();
    let mut node = Node::new(temp.path().join("owner"));
    let c = node.start().await;
    let spec = node.service(Uuid::new_v4(), 5, false);
    let _: Value = c.post("/local/services", &spec).await.unwrap();
    status(&c, "services", spec.id, |v| {
        v["status"]["http_health"] == "healthy"
    })
    .await;
    status(&c, "services", spec.id, |v| {
        v["status"]["process"] == "stopped" && v["status"]["state"] == "expired"
    })
    .await;
    assert!(
        tokio::net::TcpStream::connect((std::net::Ipv4Addr::LOCALHOST, spec.target_port))
            .await
            .is_err()
    );
    let failing = node.service(Uuid::new_v4(), 60, true);
    let _: Value = c.post("/local/services", &failing).await.unwrap();
    status(&c, "services", failing.id, |v| {
        v["status"]["state"] == "failed_needs_input" && v["status"]["starts"] == 4
    })
    .await;
    node.stop(&c).await;
    let c = node.start().await;
    let records: Vec<Value> = c.get("/local/services").await.unwrap();
    assert!(records.iter().all(|r| r["enabled"] == false));
    node.stop(&c).await;
}

#[tokio::test]
async fn owner_http_deadline_is_honored_and_a_timed_out_write_is_never_replayed() {
    let temp = tempfile::tempdir().unwrap();
    let a = Node::new(temp.path().join("caller"));
    let mut b = Node::new(temp.path().join("server"));
    b.store.trust_peer(&a.instance.card).unwrap();
    let owner = b.start().await;
    let peer = Client::new(
        &a.instance,
        &b.instance.card.endpoint,
        &b.instance.card.fingerprint,
        false,
        false,
    )
    .unwrap();
    for (seconds, expected_count) in [(1, 1), (3, 2)] {
        let mut spec = b.service(a.instance.card.node_id, 60, false);
        spec.request_timeout_seconds = seconds;
        let _: Value = owner.post("/local/services", &spec).await.unwrap();
        status(&owner, "services", spec.id, |v| {
            v["status"]["http_health"] == "healthy"
        })
        .await;
        let response = peer
            .post_with_timeout::<_, HttpReply>(
                &format!("/hmesh/v1/services/{}/http", spec.id),
                &HttpCall {
                    method: "POST".into(),
                    path: "/slow-write".into(),
                    content_type: Some("application/json".into()),
                    body_base64: "e30=".into(),
                },
                seconds + 5,
            )
            .await;
        if seconds == 1 {
            let error = response.unwrap_err();
            assert_eq!(error.code, ErrorCode::ServiceUnhealthy);
            assert!(error.message.contains("deadline exceeded"));
        } else {
            assert_eq!(response.unwrap().status, 200);
        }
        tokio::time::sleep(Duration::from_millis(800)).await;
        assert_eq!(
            std::fs::read(b.root.join("project/slow-writes"))
                .unwrap()
                .len(),
            expected_count
        );
    }
    b.stop(&owner).await;
}
