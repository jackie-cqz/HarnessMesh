//! One paired computer at several addresses: the client moves on only when
//! nothing was sent, passes over a computer with another certificate, and
//! remembers where the right one answered.
use axum::{Json, Router, extract::State, routing::get, routing::post};
use hmesh_core::*;
use hmesh_security::Instance;
use hmesh_transport::{Client, RouteMemory, serve};
use std::{
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
    time::{Duration, Instant},
};
use tokio::{
    net::{TcpListener, TcpSocket, TcpStream},
    sync::watch,
};

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
/// A loopback address where nothing comes back, like an address on a
/// network the computer has left: a listener whose one-place queue is full
/// and never taken, so a new connection's first packet is dropped.
async fn silent() -> (String, (TcpListener, Vec<TcpStream>)) {
    let socket = TcpSocket::new_v4().expect("socket");
    socket
        .bind("127.0.0.1:0".parse().expect("address"))
        .expect("bind");
    let listener = socket.listen(1).expect("listen");
    let address = listener.local_addr().expect("address");
    let mut queued = Vec::new();
    for _ in 0..64 {
        match tokio::time::timeout(Duration::from_millis(300), TcpStream::connect(address)).await {
            Ok(Ok(stream)) => queued.push(stream),
            _ => return (format!("https://{address}"), (listener, queued)),
        }
    }
    panic!("the queue at {address} never filled");
}
/// A loopback address that takes connections and never says a word: TLS
/// never completes there.
async fn mute() -> (String, tokio::task::JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("listen");
    let address = listener.local_addr().expect("address");
    let held = tokio::spawn(async move {
        let mut held = Vec::new();
        while let Ok((stream, _)) = listener.accept().await {
            held.push(stream);
        }
    });
    (format!("https://{address}"), held)
}
/// A loopback address that reaches `to`, each connection only after
/// `delay`: a slow but working way to the computer.
async fn slow(to: &str, delay: Duration) -> String {
    let to: std::net::SocketAddr = to.trim_start_matches("https://").parse().expect("address");
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("listen");
    let address = listener.local_addr().expect("address");
    tokio::spawn(async move {
        while let Ok((mut inbound, _)) = listener.accept().await {
            tokio::spawn(async move {
                tokio::time::sleep(delay).await;
                if let Ok(mut outbound) = TcpStream::connect(to).await {
                    let _ = tokio::io::copy_bidirectional(&mut inbound, &mut outbound).await;
                }
            });
        }
    });
    format!("https://{address}")
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

/// The address tried first gives no answer at all (nothing comes back, or
/// the connection opens and TLS never does, or a documentation address
/// nobody has): the next address is used well within the shortest wait a
/// caller gives a paired computer (750 ms), and the call runs once.
#[tokio::test]
async fn a_computer_is_found_at_its_next_address_when_the_first_does_not_answer() {
    let root = tempfile::tempdir().expect("tempdir");
    let a = instance(root.path(), "asks");
    let b = instance(root.path(), "studio");
    let (b_endpoint, b_calls, _b) = computer(b.clone(), trusting(&a)).await;
    let (quiet, _queue) = silent().await;
    let (mute, held) = mute().await;
    let firsts = [quiet, mute, "https://192.0.2.1:38880".to_owned()];
    for first in &firsts {
        let memory = Arc::new(RouteMemory::new());
        let client = Client::routed(
            &a,
            vec![first.clone(), b_endpoint.clone()],
            &b.card.fingerprint,
            memory.clone(),
        )
        .expect("client");
        let started = Instant::now();
        let counted = tokio::time::timeout(
            Duration::from_millis(750),
            client.post::<_, usize>("/count", &()),
        )
        .await;
        assert!(
            matches!(counted, Ok(Ok(_))),
            "{first} tried first: {counted:?} after {:?}",
            started.elapsed()
        );
        assert_eq!(
            memory.last_good(&b.card.fingerprint),
            Some(b_endpoint.clone()),
            "{first}"
        );
    }
    assert_eq!(b_calls.load(Ordering::SeqCst), firsts.len());
    held.abort();

    // Slower to connect than the first try allows, with nothing else
    // answering: it is tried again with the usual time, and the call runs once.
    let slowly = slow(&b_endpoint, Duration::from_millis(700)).await;
    let memory = Arc::new(RouteMemory::new());
    let client = Client::routed(
        &a,
        vec![slowly.clone(), refused()],
        &b.card.fingerprint,
        memory.clone(),
    )
    .expect("client");
    let counted = tokio::time::timeout(
        Duration::from_secs(3),
        client.post::<_, usize>("/count", &()),
    )
    .await;
    assert!(matches!(counted, Ok(Ok(_))), "{counted:?}");
    assert_eq!(memory.last_good(&b.card.fingerprint), Some(slowly));
    assert_eq!(b_calls.load(Ordering::SeqCst), firsts.len() + 1);
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
