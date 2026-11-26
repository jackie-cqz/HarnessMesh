//! One paired computer at several addresses: the client moves on only when
//! nothing was sent, passes over a computer with another certificate, and
//! remembers where the right one answered.
use axum::{Json, Router, extract::State, routing::get, routing::post};
use hmesh_core::*;
use hmesh_security::Instance;
use hmesh_transport::{Client, RouteMemory, serve};
use std::{
    collections::HashSet,
    net::SocketAddr,
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
    time::{Duration, Instant},
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{
        TcpListener, TcpSocket, TcpStream,
        tcp::{OwnedReadHalf, OwnedWriteHalf},
    },
    sync::{mpsc, watch},
};

/// Serve `instance` on a new loopback port: `/who` answers its name, `/count`
/// counts calls, `/later` counts and answers after 600 ms, `/slow` counts and
/// answers after two seconds, `/slower` after eight, `/fail` counts and
/// answers 503.
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
            "/later",
            post(|State(calls): State<Arc<AtomicUsize>>| async move {
                calls.fetch_add(1, Ordering::SeqCst);
                tokio::time::sleep(Duration::from_millis(600)).await;
                Json(0)
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
            "/slower",
            post(|State(calls): State<Arc<AtomicUsize>>| async move {
                calls.fetch_add(1, Ordering::SeqCst);
                tokio::time::sleep(std::time::Duration::from_secs(8)).await;
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
fn socket_address(endpoint: &str) -> SocketAddr {
    endpoint
        .trim_start_matches("https://")
        .parse()
        .expect("address")
}
/// A loopback address that reaches `to` over a long link, a relayed path
/// across the world, say: a connection takes a whole round trip to open, and
/// everything read either way arrives half a round trip after it was read.
async fn far(to: &str, round_trip: Duration) -> String {
    let to = socket_address(to);
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("listen");
    let address = listener.local_addr().expect("address");
    tokio::spawn(async move {
        while let Ok((inbound, _)) = listener.accept().await {
            tokio::spawn(async move {
                // Opening the connection: nothing is read before then.
                tokio::time::sleep(round_trip).await;
                let Ok(outbound) = TcpStream::connect(to).await else {
                    return;
                };
                let (from_client, to_client) = inbound.into_split();
                let (from_server, to_server) = outbound.into_split();
                tokio::join!(
                    late(from_client, to_server, round_trip / 2),
                    late(from_server, to_client, round_trip / 2),
                );
            });
        }
    });
    format!("https://{address}")
}
/// Copies `from` to `to`, each read written `delay` after it was read.
async fn late(mut from: OwnedReadHalf, mut to: OwnedWriteHalf, delay: Duration) {
    let (queue, mut queued) = mpsc::unbounded_channel::<(tokio::time::Instant, Vec<u8>)>();
    let reading = async move {
        let mut buffer = vec![0; 16384];
        while let Ok(read @ 1..) = from.read(&mut buffer).await {
            let due = tokio::time::Instant::now() + delay;
            if queue.send((due, buffer[..read].to_vec())).is_err() {
                break;
            }
        }
    };
    let writing = async move {
        while let Some((due, bytes)) = queued.recv().await {
            tokio::time::sleep_until(due).await;
            if to.write_all(&bytes).await.is_err() {
                return;
            }
        }
        let _ = to.shutdown().await;
    };
    tokio::join!(reading, writing);
}
/// A loopback address that reaches `to` and counts the connections made to
/// it.
async fn counted(to: &str) -> (String, Arc<AtomicUsize>) {
    let to = socket_address(to);
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("listen");
    let address = listener.local_addr().expect("address");
    let connections = Arc::new(AtomicUsize::new(0));
    let count = connections.clone();
    tokio::spawn(async move {
        while let Ok((mut inbound, _)) = listener.accept().await {
            count.fetch_add(1, Ordering::SeqCst);
            tokio::spawn(async move {
                if let Ok(mut outbound) = TcpStream::connect(to).await {
                    let _ = tokio::io::copy_bidirectional(&mut inbound, &mut outbound).await;
                }
            });
        }
    });
    (format!("https://{address}"), connections)
}
/// A loopback address that reaches `to` and keeps what comes back until
/// `release` turns true (from then on everything passes). `held` hears
/// `name` each time an answer is held.
async fn holding(
    to: &str,
    release: watch::Receiver<bool>,
    held: mpsc::UnboundedSender<&'static str>,
    name: &'static str,
) -> String {
    let to = socket_address(to);
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("listen");
    let address = listener.local_addr().expect("address");
    tokio::spawn(async move {
        while let Ok((inbound, _)) = listener.accept().await {
            let mut release = release.clone();
            let held = held.clone();
            tokio::spawn(async move {
                let Ok(outbound) = TcpStream::connect(to).await else {
                    return;
                };
                let (mut from_client, mut to_client) = inbound.into_split();
                let (mut from_server, mut to_server) = outbound.into_split();
                let asking = async move {
                    let _ = tokio::io::copy(&mut from_client, &mut to_server).await;
                    let _ = to_server.shutdown().await;
                };
                let answering = async move {
                    let mut kept = Vec::new();
                    let mut buffer = vec![0; 16384];
                    while !*release.borrow() {
                        tokio::select! {
                            read = from_server.read(&mut buffer) => match read {
                                Ok(read @ 1..) => {
                                    if kept.is_empty() {
                                        let _ = held.send(name);
                                    }
                                    kept.extend_from_slice(&buffer[..read]);
                                }
                                _ => return,
                            },
                            changed = release.changed() => if changed.is_err() {
                                return;
                            },
                        }
                    }
                    if to_client.write_all(&kept).await.is_ok() {
                        let _ = tokio::io::copy(&mut from_server, &mut to_client).await;
                    }
                    let _ = to_client.shutdown().await;
                };
                tokio::join!(asking, answering);
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

    // Slow to connect, with nothing else answering: it is waited for, and
    // the call runs once.
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

/// The first address is a slow but working way to the computer (a relayed
/// Tailscale path, every packet half a round trip each way), and the other
/// one is gone. Waited for as long as the computer itself, the 1.5 s the
/// peers list gives it, the call gets through, and runs once.
#[tokio::test]
async fn a_slow_but_working_first_address_is_waited_for() {
    let root = tempfile::tempdir().expect("tempdir");
    let a = instance(root.path(), "asks");
    let b = instance(root.path(), "studio");
    let (b_endpoint, b_calls, _b) = computer(b.clone(), trusting(&a)).await;
    let (quiet, _queue) = silent().await;
    let mut calls = 0;
    for round_trip in [260, 400] {
        let road = far(&b_endpoint, Duration::from_millis(round_trip)).await;
        for gone in [refused(), quiet.clone()] {
            let memory = Arc::new(RouteMemory::new());
            let client = Client::routed(
                &a,
                vec![road.clone(), gone.clone()],
                &b.card.fingerprint,
                memory.clone(),
            )
            .expect("client");
            let started = Instant::now();
            let counted = tokio::time::timeout(
                Duration::from_millis(1500),
                client.post::<_, usize>("/count", &()),
            )
            .await;
            assert!(
                matches!(counted, Ok(Ok(_))),
                "{round_trip} ms round trip, then {gone}: {counted:?} after {:?}",
                started.elapsed()
            );
            assert_eq!(memory.last_good(&b.card.fingerprint), Some(road.clone()));
            calls += 1;
            assert_eq!(b_calls.load(Ordering::SeqCst), calls);
        }
    }
}

/// Once the first address has connected, the call waits for its answer
/// however long it takes, a slow write included: nothing is sent, or even
/// probed, anywhere else, and the first is not connected to a second time.
#[tokio::test]
async fn a_first_address_that_connected_is_the_only_one_used() {
    let root = tempfile::tempdir().expect("tempdir");
    let a = instance(root.path(), "asks");
    let b = instance(root.path(), "studio");
    let (b_endpoint, b_calls, _b) = computer(b.clone(), trusting(&a)).await;
    let (first, first_connections) = counted(&b_endpoint).await;
    let (other, other_connections) = counted(&b_endpoint).await;
    let memory = Arc::new(RouteMemory::new());
    let client = Client::routed(
        &a,
        vec![first.clone(), other.clone()],
        &b.card.fingerprint,
        memory.clone(),
    )
    .expect("client");
    let answered = tokio::time::timeout(
        Duration::from_millis(1500),
        client.post::<_, usize>("/later", &()),
    )
    .await;
    assert!(matches!(answered, Ok(Ok(0))), "{answered:?}");
    assert_eq!(b_calls.load(Ordering::SeqCst), 1);
    assert_eq!(first_connections.load(Ordering::SeqCst), 1);
    assert_eq!(other_connections.load(Ordering::SeqCst), 0);

    // Too slow for the call's own deadline: it fails, and is not sent again.
    let failed = client
        .post_with_timeout::<_, usize>("/slow", &(), 1)
        .await
        .unwrap_err();
    assert_eq!(failed.code, ErrorCode::PeerUnreachable);
    assert_eq!(b_calls.load(Ordering::SeqCst), 2);
    assert_eq!(first_connections.load(Ordering::SeqCst), 2);
    assert_eq!(other_connections.load(Ordering::SeqCst), 0);
    assert_eq!(memory.last_good(&b.card.fingerprint), Some(first));
}

/// The first address connects in the very turn a probe of the other one
/// completes its handshake. Both answers are held back, then let through
/// while the calling side is held still, so it takes them together: the
/// first keeps its connection and the other address is not used. A probe
/// answering alone takes over (the first's late connection is never used),
/// and a first answering alone ends the probing. Every time the call runs
/// exactly once.
#[test]
fn a_first_address_connecting_as_another_answers_runs_the_call_once() {
    let servers = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .expect("runtime");
    let root = tempfile::tempdir().expect("tempdir");
    let a = instance(root.path(), "asks");
    let b = instance(root.path(), "studio");
    let (b_endpoint, b_calls, _b) = servers.block_on(computer(b.clone(), trusting(&a)));
    let mut calls = 0;
    for (lets_through, used) in [
        (["other", "first"].as_slice(), "first"),
        (["other"].as_slice(), "other"),
        (["first"].as_slice(), "first"),
    ] {
        let (release_first, first_held) = watch::channel(false);
        let (release_other, other_held) = watch::channel(false);
        let (held, mut holding_now) = mpsc::unbounded_channel();
        let first = servers.block_on(holding(&b_endpoint, first_held, held.clone(), "first"));
        let other = servers.block_on(holding(&b_endpoint, other_held, held, "other"));
        let memory = Arc::new(RouteMemory::new());
        let client = Client::routed(
            &a,
            vec![first.clone(), other.clone()],
            &b.card.fingerprint,
            memory.clone(),
        )
        .expect("client");
        let calling = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("runtime");
        let answered = calling.block_on(async {
            let call = tokio::spawn(async move { client.post::<_, usize>("/count", &()).await });
            // The first's handshake from the start, the other's probe once
            // the first has had its head start.
            let mut waiting = HashSet::from(["first", "other"]);
            while !waiting.is_empty() {
                let name = tokio::time::timeout(Duration::from_secs(5), holding_now.recv())
                    .await
                    .unwrap_or_else(|_| panic!("no handshake held at {waiting:?}"))
                    .expect("held");
                waiting.remove(name);
            }
            // Held still (this runtime has one thread) while the answers
            // arrive, the other's first, so the next turn takes them together.
            for name in lets_through {
                match *name {
                    "first" => release_first.send(true).expect("release"),
                    _ => release_other.send(true).expect("release"),
                }
                std::thread::sleep(Duration::from_millis(20));
            }
            std::thread::sleep(Duration::from_millis(150));
            let answered = tokio::time::timeout(Duration::from_secs(5), call).await;
            let _ = release_first.send(true);
            let _ = release_other.send(true);
            answered
        });
        assert!(
            matches!(answered, Ok(Ok(Ok(_)))),
            "{lets_through:?} let through: {answered:?}"
        );
        calls += 1;
        assert_eq!(b_calls.load(Ordering::SeqCst), calls, "{lets_through:?}");
        let used = if used == "first" { first } else { other };
        assert_eq!(
            memory.last_good(&b.card.fingerprint),
            Some(used),
            "{lets_through:?} let through"
        );
    }
    servers.shutdown_background();
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

/// Says where a computer was heard (as the local network would), counts how
/// often it is asked, and keeps what it is told answered.
struct Heard {
    at: Vec<String>,
    asked: AtomicUsize,
    answered: Mutex<Vec<String>>,
}
impl Heard {
    fn at(at: &[&str]) -> Arc<Self> {
        Arc::new(Self {
            at: at.iter().map(|a| a.to_string()).collect(),
            asked: AtomicUsize::new(0),
            answered: Mutex::new(Vec::new()),
        })
    }
}
impl hmesh_transport::Finder for Heard {
    fn find(
        &self,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Vec<String>> + Send + 'static>> {
        self.asked.fetch_add(1, Ordering::SeqCst);
        let at = self.at.clone();
        Box::pin(async move { at })
    }
    fn found(&self, origin: &str) {
        self.answered.lock().unwrap().push(origin.to_owned());
    }
}

/// No address a computer is known by answers: it is looked for (once per
/// call) and reached where it was heard, which is then tried first. Heard
/// only where another computer answers, or not heard at all, the error says so.
#[tokio::test]
async fn a_computer_none_of_whose_addresses_answers_is_reached_where_it_was_heard() {
    let root = tempfile::tempdir().expect("tempdir");
    let a = instance(root.path(), "asks");
    let b = instance(root.path(), "studio");
    let c = instance(root.path(), "another");
    let (b_endpoint, b_calls, _b) = computer(b.clone(), trusting(&a)).await;
    let (c_endpoint, c_calls, _c) = computer(c.clone(), trusting(&a)).await;
    let dead = refused();
    let memory = Arc::new(RouteMemory::new());
    let heard = Heard::at(&["not an address", &c_endpoint, &b_endpoint]);
    let client = Client::routed(&a, vec![dead.clone()], &b.card.fingerprint, memory.clone())
        .expect("client")
        .finding(heard.clone());
    assert_eq!(
        client.post::<_, usize>("/count", &()).await.expect("found"),
        1
    );
    assert_eq!(heard.asked.load(Ordering::SeqCst), 1);
    assert_eq!(
        *heard.answered.lock().unwrap(),
        std::slice::from_ref(&b_endpoint)
    );
    assert_eq!(
        memory.last_good(&b.card.fingerprint),
        Some(b_endpoint.clone())
    );
    assert_eq!(
        c_calls.load(Ordering::SeqCst),
        0,
        "another computer got a request"
    );
    // Answering where it was found: nothing more to look for.
    let client = Client::routed(
        &a,
        vec![dead.clone(), b_endpoint.clone()],
        &b.card.fingerprint,
        memory.clone(),
    )
    .expect("client")
    .finding(heard.clone());
    assert_eq!(client.origins()[0], b_endpoint);
    assert_eq!(
        client.post::<_, usize>("/count", &()).await.expect("count"),
        2
    );
    assert_eq!(heard.asked.load(Ordering::SeqCst), 1);
    assert_eq!(b_calls.load(Ordering::SeqCst), 2);

    // Heard only where another computer answers.
    let elsewhere = Heard::at(&[&c_endpoint]);
    let lost = Client::routed(
        &a,
        vec![dead.clone()],
        &b.card.fingerprint,
        Arc::new(RouteMemory::new()),
    )
    .expect("client")
    .finding(elsewhere.clone())
    .get::<String>("/who")
    .await
    .unwrap_err();
    assert_eq!(lost.code, ErrorCode::PeerUnreachable);
    assert!(
        lost.message
            .contains(&format!("{c_endpoint} is another computer")),
        "{}",
        lost.message
    );
    assert!(elsewhere.answered.lock().unwrap().is_empty());
    assert_eq!(
        hmesh_transport::unreachable_reasons(&lost.message),
        ["refused", "another_computer"]
    );
    // Not heard at all: the error says that too.
    let unheard = Client::routed(
        &a,
        vec![dead.clone()],
        &b.card.fingerprint,
        Arc::new(RouteMemory::new()),
    )
    .expect("client")
    .finding(Heard::at(&[]))
    .get::<String>("/who")
    .await
    .unwrap_err();
    assert_eq!(
        hmesh_transport::unreachable_reasons(&unheard.message),
        ["refused", "not_heard"],
        "{}",
        unheard.message
    );
    // Without a finder, nothing is said about the local network.
    let plain = Client::routed(
        &a,
        vec![dead],
        &b.card.fingerprint,
        Arc::new(RouteMemory::new()),
    )
    .expect("client")
    .get::<String>("/who")
    .await
    .unwrap_err();
    assert_eq!(
        hmesh_transport::unreachable_reasons(&plain.message),
        ["refused"]
    );
    assert!(hmesh_transport::unreachable_reasons("RPC deadline exceeded").is_empty());
}

/// First contact at a host name (pairing by an address someone typed):
/// the card is learned there; nothing answering says where it stopped.
#[tokio::test]
async fn first_contact_works_at_a_host_name_and_says_where_it_stopped() {
    let root = tempfile::tempdir().expect("tempdir");
    let a = instance(root.path(), "asks");
    let b = instance(root.path(), "studio");
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("listen");
    let port = listener.local_addr().expect("address").port();
    let card = b.card.clone();
    let app = Router::new().route("/hmesh/v1/hello", get(move || async move { Json(card) }));
    let (_stop, receiver) = watch::channel(false);
    let server = b.clone();
    tokio::spawn(serve(
        listener,
        app,
        Arc::new(move || server.server_config(&[])),
        receiver,
    ));
    let by_name = format!("https://localhost:{port}");
    let (card, seen) = Client::learn(&a, &by_name).await.expect("learned by name");
    assert_eq!(card.node_id, b.card.node_id);
    assert_eq!(seen, b.card.fingerprint);
    let pinned = Client::pinned(&a, &by_name, &seen, true).expect("pinned");
    assert_eq!(
        pinned
            .get::<NodeCard>("/hmesh/v1/hello")
            .await
            .expect("hello")
            .node_id,
        b.card.node_id
    );
    let dead = refused();
    let lost = Client::learn(&a, &dead).await.unwrap_err();
    assert_eq!(lost.code, ErrorCode::PeerUnreachable);
    assert!(
        lost.message
            .contains(&format!("{dead} refused the connection")),
        "{}",
        lost.message
    );
    let unnamed = Client::learn(&a, "https://nowhere.invalid:1")
        .await
        .unwrap_err();
    assert_eq!(
        hmesh_transport::unreachable_reasons(&unnamed.message),
        ["unresolved"],
        "{}",
        unnamed.message
    );
    assert_eq!(
        Client::learn(&a, "https://studio.local:1/path")
            .await
            .unwrap_err()
            .code,
        ErrorCode::InvalidInput
    );
}

/// A loopback address where `instance` is the computer (TLS completes with
/// its certificate), which reads a request and hangs up without answering:
/// the request was sent, and may have run there. Counts the requests read.
async fn hangs_up(instance: Arc<Instance>, trusted: Peer) -> (String, Arc<AtomicUsize>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("listen");
    let address = listener.local_addr().expect("address");
    let config = instance
        .server_config(std::slice::from_ref(&trusted))
        .expect("TLS");
    let acceptor = tokio_rustls::TlsAcceptor::from(Arc::new(config));
    let requests = Arc::new(AtomicUsize::new(0));
    let count = requests.clone();
    tokio::spawn(async move {
        while let Ok((stream, _)) = listener.accept().await {
            let (acceptor, count) = (acceptor.clone(), count.clone());
            tokio::spawn(async move {
                let Ok(mut tls) = acceptor.accept(stream).await else {
                    return;
                };
                let mut read = Vec::new();
                let mut buffer = [0u8; 4096];
                while !read.windows(4).any(|end| end == b"\r\n\r\n") {
                    match tls.read(&mut buffer).await {
                        Ok(n @ 1..) => read.extend_from_slice(&buffer[..n]),
                        _ => return,
                    }
                }
                count.fetch_add(1, Ordering::SeqCst);
                // Dropped without a word back.
            });
        }
    });
    (format!("https://{address}"), requests)
}

/// None of its known addresses answering, a computer is reached where it was
/// heard, takes the request, and the call fails there: it hangs up before
/// answering, or answers too late for the call. The request may have run,
/// so it is not sent to the next address heard: it ran once, and the other
/// address got nothing. An answer that is an error is the computer's answer
/// too, and nothing else is tried.
#[tokio::test]
async fn a_call_that_failed_where_the_computer_was_heard_is_not_sent_again() {
    let root = tempfile::tempdir().expect("tempdir");
    let a = instance(root.path(), "asks");
    let b = instance(root.path(), "studio");
    let (next, next_calls, _next) = computer(b.clone(), trusting(&a)).await;
    let dead = refused();
    let memory = Arc::new(RouteMemory::new());
    let call = |heard: &Arc<Heard>| {
        Client::routed(&a, vec![dead.clone()], &b.card.fingerprint, memory.clone())
            .expect("client")
            .finding(heard.clone())
    };

    let (hanging, read) = hangs_up(b.clone(), trusting(&a)).await;
    let heard = Heard::at(&[&hanging, &next]);
    let failed = call(&heard)
        .post::<_, usize>("/count", &())
        .await
        .unwrap_err();
    assert_eq!(failed.code, ErrorCode::PeerUnreachable);
    assert_eq!(failed.message, "RPC transport failed");
    assert_eq!(read.load(Ordering::SeqCst), 1, "the request was not read");
    assert_eq!(
        next_calls.load(Ordering::SeqCst),
        0,
        "sent again to the next address heard"
    );
    assert_eq!(heard.asked.load(Ordering::SeqCst), 1);
    assert!(heard.answered.lock().unwrap().is_empty());
    assert_eq!(memory.last_good(&b.card.fingerprint), None);

    // The call's deadline also covers the known address refusing it first,
    // which takes about two seconds on Windows (it tries again before giving
    // up): long enough for that, shorter than the answer.
    let (late, late_calls, _late) = computer(b.clone(), trusting(&a)).await;
    let heard = Heard::at(&[&late, &next]);
    let failed = call(&heard)
        .post_with_timeout::<_, usize>("/slower", &(), 4)
        .await
        .unwrap_err();
    assert_eq!(failed.message, "RPC deadline exceeded");
    assert_eq!(late_calls.load(Ordering::SeqCst), 1);
    assert_eq!(next_calls.load(Ordering::SeqCst), 0);
    assert!(heard.answered.lock().unwrap().is_empty());

    let heard = Heard::at(&[&late, &next]);
    let failed = call(&heard)
        .post::<_, usize>("/fail", &())
        .await
        .unwrap_err();
    assert_eq!(failed.code, ErrorCode::ServiceUnhealthy);
    assert_eq!(late_calls.load(Ordering::SeqCst), 2);
    assert_eq!(next_calls.load(Ordering::SeqCst), 0);
    assert_eq!(*heard.answered.lock().unwrap(), std::slice::from_ref(&late));
}
