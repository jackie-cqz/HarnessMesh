use axum::{Extension, Json, Router, routing::get};
use hmesh_core::*;
use hmesh_security::Instance;
use hmesh_transport::{Client, ConnectionIdentity, serve};
use std::sync::Arc;
use tokio::{net::TcpListener, sync::watch};

async fn identity(Extension(connection): Extension<ConnectionIdentity>) -> Json<Option<String>> {
    Json(connection.fingerprint)
}

#[tokio::test]
async fn real_tls_validates_pins_and_mutual_client_identity() {
    let temp = tempfile::tempdir().expect("tempdir");
    let a = Instance::init(&temp.path().join("a"), NodeConfig::default()).expect("init a");
    let b =
        Arc::new(Instance::init(&temp.path().join("b"), NodeConfig::default()).expect("init b"));
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("listen");
    let endpoint = format!("https://{}", listener.local_addr().expect("address"));
    let (shutdown, receiver) = watch::channel(false);
    let a_peer = Peer {
        card: a.card.clone(),
        status: "trusted".into(),
        approved_at: Some(now()),
    };
    let server_b = b.clone();
    let task = tokio::spawn(serve(
        listener,
        Router::new().route("/identity", get(identity)),
        Arc::new(move || server_b.server_config(std::slice::from_ref(&a_peer))),
        receiver,
    ));
    let client = Client::new(&a, &endpoint, &b.card.fingerprint, false, false).expect("client");
    let result: Option<String> = client.get("/identity").await.expect("mutual TLS");
    assert_eq!(result.as_deref(), Some(a.card.fingerprint.as_str()));
    let wrong =
        Client::new(&a, &endpoint, &"0".repeat(64), false, false).expect("wrong pin client");
    assert!(wrong.get::<Option<String>>("/identity").await.is_err());
    let anonymous =
        Client::new(&a, &endpoint, &b.card.fingerprint, true, false).expect("bootstrap client");
    assert_eq!(
        anonymous
            .get::<Option<String>>("/identity")
            .await
            .expect("bootstrap TLS"),
        None
    );
    let c = Instance::init(&temp.path().join("c"), NodeConfig::default()).expect("init c");
    let untrusted =
        Client::new(&c, &endpoint, &b.card.fingerprint, false, false).expect("untrusted client");
    assert!(untrusted.get::<Option<String>>("/identity").await.is_err());
    shutdown.send(true).expect("shutdown");
    task.await.expect("server task").expect("server");
}
