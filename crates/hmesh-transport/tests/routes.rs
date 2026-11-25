//! One paired computer at several addresses: the client moves on only when
//! nothing was sent, passes over a computer with another certificate, and
//! remembers where the right one answered.
use axum::{Json, Router, extract::State, routing::get, routing::post};
use hmesh_core::*;
use hmesh_security::Instance;
use hmesh_transport::{Client, RouteMemory, serve};
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicUsize, Ordering},
};
use tokio::{net::TcpListener, sync::watch};

/// Serve `instance` on a new loopback port: `/who` answers its name, `/count`
/// counts calls, `/slow` counts and answers after two seconds, `/fail` counts
/// and answers 503.
async fn computer(
    instance: Arc<Instance>,
    trusted: Peer,
) -> (String, Arc<AtomicUsize>, watch::Sender<bool>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("listen");
    let port = listener.local_addr().expect("address").port();
    let calls = Arc::new(AtomicUsize::new(0));
    let name = instance.card.display_name.clone();
    let app = Router::new()
        .route("/who", get(move || async move { Json(name) }))
        .route(
            "/count",
            post(|State(calls): State<Arc<AtomicUsize>>| async move {
                Json(calls.fetch_add(1, Ordering::SeqCst) + 1)
            }),
        )
        .route(
            "/slow",
            post(|State(calls): State<Arc<AtomicUsize>>| async move {
                calls.fetch_add(1, Ordering::SeqCst);
                tokio::time::sleep(std::time::Duration::from_secs(2)).await;
                Json(0)
            }),
        )
        .route(
            "/fail",
            post(|State(calls): State<Arc<AtomicUsize>>| async move {
                calls.fetch_add(1, Ordering::SeqCst);
                (
                    axum::http::StatusCode::SERVICE_UNAVAILABLE,
                    Json(serde_json::json!({"error": MeshError::new(ErrorCode::ServiceUnhealthy, "busy")})),
                )
            }),
        )
        .with_state(calls.clone());
    let (stop, receiver) = watch::channel(false);
    tokio::spawn(serve(
        listener,
        app,
        Arc::new(move || instance.server_config(std::slice::from_ref(&trusted))),
        receiver,
    ));
    (format!("https://127.0.0.1:{port}"), calls, stop)
}
fn instance(root: &std::path::Path, name: &str) -> Arc<Instance> {
    let mut config = NodeConfig::default();
    config.node.name = name.into();
    // Dials other computers (here all on loopback, standing in for a LAN).
    config.node.network = "direct".into();
    Arc::new(Instance::init(&root.join(name), config).expect("init"))
}
fn trusting(a: &Instance) -> Peer {
    Peer {
        card: a.card.clone(),
        status: "trusted".into(),
        approved_at: Some(now()),
    }
}
fn refused() -> String {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("port");
    let port = listener.local_addr().expect("address").port();
    drop(listener);
    format!("https://127.0.0.1:{port}")
}

#[tokio::test]
async fn a_computer_known_by_several_addresses_is_found_at_the_one_that_is_it() {
    let root = tempfile::tempdir().expect("tempdir");
    let a = instance(root.path(), "asks");
    let b = instance(root.path(), "studio");
    let c = instance(root.path(), "another");
    let (b_endpoint, b_calls, _b) = computer(b.clone(), trusting(&a)).await;
    let (c_endpoint, c_calls, _c) = computer(c.clone(), trusting(&a)).await;
    let b_port = b_endpoint.rsplit(':').next().unwrap();
    let by_name = format!("https://localhost:{b_port}");
    let kept = Arc::new(Mutex::new(Vec::new()));
    let keep = kept.clone();
    let memory = Arc::new(RouteMemory::persisted(move |fingerprint, origin, _| {
        keep.lock()
            .unwrap()
            .push((fingerprint.to_owned(), origin.to_owned()));
    }));

    // Nothing listens at the first; the second is another computer; the
    // studio answers by name.
    let dead = refused();
    let client = Client::routed(
        &a,
        vec![dead.clone(), c_endpoint.clone(), by_name.clone()],
        &b.card.fingerprint,
        memory.clone(),
    )
    .expect("client");
    assert_eq!(
        client.get::<String>("/who").await.expect("reached"),
        "studio"
    );
    assert_eq!(memory.last_good(&b.card.fingerprint), Some(by_name.clone()));
    assert_eq!(
        *kept.lock().unwrap(),
        [(b.card.fingerprint.clone(), by_name.clone())]
    );
    // Next time it is tried first, and a route that keeps answering is not
    // written down on every call.
    assert_eq!(client.origins()[0], by_name);
    assert_eq!(
        client.post::<_, usize>("/count", &()).await.expect("count"),
        1
    );
    assert_eq!(kept.lock().unwrap().len(), 1);
    assert_eq!(
        c_calls.load(Ordering::SeqCst),
        0,
        "another computer got a request"
    );

    // Sent and refused, or sent and too slow: never sent again elsewhere.
    let twice = Client::routed(
        &a,
        vec![b_endpoint.clone(), by_name.clone()],
        &b.card.fingerprint,
        Arc::new(RouteMemory::new()),
    )
    .expect("client");
    let before = b_calls.load(Ordering::SeqCst);
    let failed = twice.post::<_, usize>("/fail", &()).await.unwrap_err();
    assert_eq!(failed.code, ErrorCode::ServiceUnhealthy);
    assert_eq!(b_calls.load(Ordering::SeqCst), before + 1);
    let slow = twice
        .post_with_timeout::<_, usize>("/slow", &(), 1)
        .await
        .unwrap_err();
    assert_eq!(slow.code, ErrorCode::PeerUnreachable);
    assert_eq!(b_calls.load(Ordering::SeqCst), before + 2);

    // Every address failed: each says where it stopped.
    let lost = Client::routed(
        &a,
        vec![
            dead.clone(),
            c_endpoint.clone(),
            "https://nowhere.invalid:1".into(),
        ],
        &b.card.fingerprint,
        Arc::new(RouteMemory::new()),
    )
    .expect("client")
    .get::<String>("/who")
    .await
    .unwrap_err();
    assert_eq!(lost.code, ErrorCode::PeerUnreachable);
    for part in [
        format!("{dead} refused the connection"),
        format!("{c_endpoint} is another computer"),
        "https://nowhere.invalid:1 did not resolve".into(),
    ] {
        assert!(lost.message.contains(&part), "{}", lost.message);
    }
}

/// A computer on loopback alone dials only loopback addresses, and an
/// address list that is not one is not dialed at all.
#[tokio::test]
async fn a_loopback_computer_dials_only_loopback_addresses() {
    let root = tempfile::tempdir().expect("tempdir");
    let a = Instance::init(&root.path().join("alone"), NodeConfig::default()).expect("init");
    let b = instance(root.path(), "studio");
    let memory = Arc::new(RouteMemory::new());
    let client = Client::routed(
        &a,
        vec![
            "https://192.168.1.20:38880".into(),
            "https://studio.local:38880".into(),
            "https://localhost:38880".into(),
            "https://user@127.0.0.1:1".into(),
        ],
        &b.card.fingerprint,
        memory.clone(),
    )
    .expect("client");
    assert_eq!(client.origins(), ["https://localhost:38880"]);
    assert!(
        Client::routed(
            &a,
            vec!["https://192.168.1.20:38880".into()],
            &b.card.fingerprint,
            memory,
        )
        .is_err()
    );
}
