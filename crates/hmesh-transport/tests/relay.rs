//! The built-in relay, in one process: a computer holding a connection to
//! it is reached through it, with the right token only; the relay knows
//! nothing of what passes; it takes on only so much, and one source cannot
//! use up what everyone shares; the computer behind it takes only the
//! computers it is paired with, closes what goes unused, and a request is
//! sent once, on a new connection when a kept one has just closed; and a
//! call through it over a slow link still answers in the time the callers
//! give it.
use axum::{Json, Router, extract::State, routing::get, routing::post};
use hmesh_core::*;
use hmesh_security::{Instance, relay::RelayIdentity};
use hmesh_transport::{
    Client, RelayDialer, RouteMemory,
    relay::{self, Limits, Line, Rate, Token},
    unreachable_reasons,
};
use std::{
    net::SocketAddr,
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicUsize, Ordering},
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

fn instance(root: &std::path::Path, name: &str) -> Arc<Instance> {
    let mut config = NodeConfig::default();
    config.node.name = name.into();
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
/// A relay on `listener`, its relay address there, and its token (to
/// change it).
fn relay_on(
    root: &std::path::Path,
    listener: TcpListener,
    limits: Limits,
) -> (RelayAddress, Token, watch::Sender<bool>) {
    let identity = RelayIdentity::open(&root.join("relay")).expect("relay identity");
    let address = listener.local_addr().expect("address");
    let code = identity
        .code(&SocketAddr::new([127, 0, 0, 1].into(), address.port()).to_string())
        .expect("relay code");
    let token = Token::fixed(identity.token().expect("token"));
    let (stop, stopped) = watch::channel(false);
    tokio::spawn(relay::serve(
        listener,
        Arc::new(identity.server_config().expect("TLS")),
        token.clone(),
        limits,
        stopped,
    ));
    (code, token, stop)
}
/// A relay on a new loopback port.
async fn relay(
    root: &std::path::Path,
    limits: Limits,
) -> (RelayAddress, Token, watch::Sender<bool>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("listen");
    relay_on(root, listener, limits)
}
/// The same relay at another address (`host:port`).
fn at(relay: &RelayAddress, authority: &str) -> RelayAddress {
    RelayAddress::parse(&format!(
        "hmesh-relay://{authority}/{}/{}",
        relay.fingerprint(),
        relay.token()
    ))
    .expect("relay address")
}
/// `instance` holding a connection to `relay`, serving `/count` (it counts
/// the calls it runs) and `/who` to the computer it trusts, on what comes
/// through; at most `slots` at once, and only computers it trusts unless
/// `strangers`. Each connection is closed after `idle` unused. Whether it
/// holds the relay now, its calls, and the connections it serves now.
fn holding_for(
    instance: Arc<Instance>,
    trusted: Peer,
    relay: RelayAddress,
    slots: usize,
    strangers: bool,
    idle: Duration,
) -> (
    Arc<AtomicBool>,
    Arc<AtomicUsize>,
    watch::Sender<bool>,
    Arc<AtomicUsize>,
) {
    let calls = Arc::new(AtomicUsize::new(0));
    let name = instance.card.display_name.clone();
    let app = Router::new()
        .route(
            "/count",
            post(|State(calls): State<Arc<AtomicUsize>>| async move {
                Json(calls.fetch_add(1, Ordering::SeqCst) + 1)
            }),
        )
        .route("/who", get(move || async move { Json(name) }))
        .with_state(calls.clone());
    let (stop, stopped) = watch::channel(false);
    let held = Arc::new(AtomicBool::new(false));
    let shown = held.clone();
    let server = instance.clone();
    let serving = stopped.clone();
    let open = Arc::new(AtomicUsize::new(0));
    let counted = open.clone();
    let paired = trusted.card.fingerprint.clone();
    let arrived: relay::Arrived = Arc::new(move |stream, seat| {
        let config = server
            .server_config(std::slice::from_ref(&trusted))
            .expect("TLS");
        let (app, serving, counted) = (app.clone(), serving.clone(), counted.clone());
        tokio::spawn(async move {
            counted.fetch_add(1, Ordering::SeqCst);
            hmesh_transport::serve_relayed_connection(
                stream,
                seat.caller(),
                || true,
                app,
                config,
                idle,
                serving,
            )
            .await;
            counted.fetch_sub(1, Ordering::SeqCst);
            drop(seat);
        });
    });
    let tls = Arc::new(
        instance
            .client_config(relay.fingerprint(), false)
            .expect("TLS to the relay"),
    );
    let seats = relay::Seats::new(slots, slots, slots, move |caller: &str| {
        if caller == paired {
            relay::Caller::Paired
        } else if strangers {
            relay::Caller::Stranger
        } else {
            relay::Caller::Refused
        }
    });
    tokio::spawn(relay::hold(
        relay,
        tls,
        seats,
        arrived,
        Arc::new(move |now| shown.store(now, Ordering::SeqCst)),
        stopped,
    ));
    (held, calls, stop, open)
}
fn holding_with(
    instance: Arc<Instance>,
    trusted: Peer,
    relay: RelayAddress,
    slots: usize,
) -> (Arc<AtomicBool>, Arc<AtomicUsize>, watch::Sender<bool>) {
    let (held, calls, stop, _) = holding_for(
        instance,
        trusted,
        relay,
        slots,
        true,
        hmesh_transport::RELAYED_IDLE,
    );
    (held, calls, stop)
}
fn holding(
    instance: Arc<Instance>,
    trusted: Peer,
    relay: RelayAddress,
) -> (Arc<AtomicBool>, Arc<AtomicUsize>, watch::Sender<bool>) {
    holding_with(instance, trusted, relay, relay::SERVED_AT_ONCE)
}
async fn until(what: &str, ready: impl Fn() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(15);
    while !ready() {
        assert!(Instant::now() < deadline, "{what}");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}
type Reached = tokio_rustls::client::TlsStream<TcpStream>;
/// A connection to the relay as `instance`, said `line` on, and its first
/// answer (`None` when the relay closed it without a word).
async fn said(instance: &Instance, relay: &RelayAddress, line: &str) -> (Reached, Option<String>) {
    let tls = Arc::new(
        instance
            .client_config(relay.fingerprint(), false)
            .expect("TLS"),
    );
    let mut stream = relay::reach(relay, tls).await.expect("reach the relay");
    stream
        .write_all(format!("{line}\n").as_bytes())
        .await
        .expect("say");
    stream.flush().await.expect("flush");
    let answer = tokio::time::timeout(Duration::from_secs(12), relay::read_line(&mut stream))
        .await
        .expect("an answer or the end, in time")
        .ok();
    (stream, answer)
}
async fn say(instance: &Instance, relay: &RelayAddress, line: &str) -> Option<String> {
    said(instance, relay, line).await.1
}
fn dial(relay: &RelayAddress, fingerprint: &str) -> String {
    format!("DIAL {} {fingerprint}", relay.token())
}
fn client(
    a: &Instance,
    candidates: Vec<String>,
    fingerprint: &str,
    dialer: &Arc<RelayDialer>,
) -> Client {
    Client::routed_through(
        a,
        candidates,
        fingerprint,
        Arc::new(RouteMemory::new()),
        dialer,
        false,
    )
    .expect("client")
}

/// The computer behind the relay answers a call through it, exactly once
/// each; the address it is remembered by is the relay's, which is only
/// ever shown by where it is, and the connection is kept for the next call.
/// A wrong token is refused, the computer is not reached, and the error
/// says only that its relay did not put it through.
#[tokio::test]
async fn a_computer_holding_a_relay_is_reached_through_it_with_its_token_only() {
    let root = tempfile::tempdir().expect("tempdir");
    let (address, token, _relay) = relay(root.path(), Limits::default()).await;
    let a = instance(root.path(), "laptop");
    let b = instance(root.path(), "studio");
    let (held, calls, _b) = holding(b.clone(), trusting(&a), address.clone());
    until("the studio holds the relay", || held.load(Ordering::SeqCst)).await;

    let (stop, stopped) = watch::channel(false);
    let dialer = RelayDialer::new(a.clone(), stopped);
    let memory = Arc::new(RouteMemory::new());
    let client = Client::routed_through(
        &a,
        vec![address.to_string()],
        &b.card.fingerprint,
        memory.clone(),
        &dialer,
        false,
    )
    .expect("client");
    assert_eq!(client.origins(), [address.label()], "shown by its label");
    for expected in 1..=3usize {
        let counted: usize = client
            .post("/count", &serde_json::json!({}))
            .await
            .expect("through the relay");
        assert_eq!(counted, expected);
        assert_eq!(dialer.kept(), 1, "one connection, kept for the next call");
    }
    assert_eq!(calls.load(Ordering::SeqCst), 3);
    assert_eq!(
        memory.last_good(&b.card.fingerprint).as_deref(),
        Some(address.to_string().as_str())
    );

    // Not with another token.
    let wrong = address.with_token(&"0".repeat(64)).expect("address");
    let error = client_for(&a, &wrong, &b, &dialer)
        .post::<_, usize>("/count", &serde_json::json!({}))
        .await
        .expect_err("a wrong token is refused");
    assert_eq!(error.code, ErrorCode::PeerUnreachable, "{error:?}");
    assert!(
        error.message.contains("was not put through by its relay"),
        "{}",
        error.message
    );
    assert!(!error.message.contains(wrong.token()), "{}", error.message);
    assert!(
        !error.message.contains(address.token()),
        "{}",
        error.message
    );
    assert_eq!(unreachable_reasons(&error.message), ["relay"]);
    assert_eq!(calls.load(Ordering::SeqCst), 3, "not reached");
    // Said straight to the relay: closed, without a word, whatever the line.
    for line in [
        format!("LISTEN {}", wrong.token()),
        dial(&wrong, &b.card.fingerprint),
        format!("ACCEPT {} 1", wrong.token()),
        "HELLO".to_owned(),
        format!("PING {}", address.token()),
    ] {
        assert_eq!(
            say(&a, &address, &line).await,
            None,
            "{line:?} was answered"
        );
    }
    // A computer the relay does not hold is offline.
    let elsewhere = instance(root.path(), "elsewhere");
    assert_eq!(
        say(&a, &address, &dial(&address, &elsewhere.card.fingerprint))
            .await
            .as_deref(),
        Some("ERR offline")
    );

    // A new token: the studio is let go, and the old one no longer works.
    token.set("1".repeat(64));
    until("the studio is let go", || !held.load(Ordering::SeqCst)).await;
    let fresh = RelayDialer::new(a.clone(), stop.subscribe());
    let error = client_for(&a, &address, &b, &fresh)
        .post::<_, usize>("/count", &serde_json::json!({}))
        .await
        .expect_err("the old token is refused");
    assert_eq!(unreachable_reasons(&error.message), ["relay"], "{error:?}");
    let _ = stop.send(true);
}
fn client_for(
    a: &Instance,
    relay: &RelayAddress,
    b: &Instance,
    dialer: &Arc<RelayDialer>,
) -> Client {
    client(a, vec![relay.to_string()], &b.card.fingerprint, dialer)
}

/// The computer behind a relay must still be the one pinned: a relay that
/// puts the call through to another computer gets nothing sent to it.
#[tokio::test]
async fn a_relay_cannot_pass_another_computer_off_as_the_one_asked_for() {
    let root = tempfile::tempdir().expect("tempdir");
    let a = instance(root.path(), "laptop");
    let b = instance(root.path(), "studio");
    let c = instance(root.path(), "impostor");
    // A relay that puts every DIAL through to the impostor.
    let identity = RelayIdentity::open(&root.path().join("relay")).expect("relay");
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("listen");
    let address = identity
        .code(&listener.local_addr().expect("address").to_string())
        .expect("code");
    let acceptor =
        tokio_rustls::TlsAcceptor::from(Arc::new(identity.server_config().expect("TLS")));
    let calls = Arc::new(AtomicUsize::new(0));
    let counted = calls.clone();
    let impostor = c.clone();
    let trusted = trusting(&a);
    tokio::spawn(async move {
        while let Ok((stream, _)) = listener.accept().await {
            let (acceptor, impostor, trusted, counted) = (
                acceptor.clone(),
                impostor.clone(),
                trusted.clone(),
                counted.clone(),
            );
            tokio::spawn(async move {
                let Ok(mut tls) = acceptor.accept(stream).await else {
                    return;
                };
                let _ = relay::read_line(&mut tls).await;
                let _ = relay::write_line(&mut tls, &Line::Ok).await;
                let app = Router::new()
                    .route(
                        "/count",
                        post(|State(calls): State<Arc<AtomicUsize>>| async move {
                            Json(calls.fetch_add(1, Ordering::SeqCst) + 1)
                        }),
                    )
                    .with_state(counted);
                let config = impostor
                    .server_config(std::slice::from_ref(&trusted))
                    .expect("TLS");
                let (_stop, stopped) = watch::channel(false);
                hmesh_transport::serve_connection(
                    tls,
                    SocketAddr::new(relay::RELAYED, 0),
                    app,
                    config,
                    stopped,
                )
                .await;
            });
        }
    });
    let (_stop, stopped) = watch::channel(false);
    let dialer = RelayDialer::new(a.clone(), stopped);
    let error = client_for(&a, &address, &b, &dialer)
        .post::<_, usize>("/count", &serde_json::json!({}))
        .await
        .expect_err("another computer");
    assert!(error.message.contains("is another computer"), "{error:?}");
    assert_eq!(unreachable_reasons(&error.message), ["another_computer"]);
    assert_eq!(calls.load(Ordering::SeqCst), 0);
}

/// Only the computer a `CONNECT` was for may accept it: another computer
/// with the token, saying the same id with its own certificate, is closed
/// without a word, and the computer meant still accepts it.
#[tokio::test]
async fn only_the_computer_asked_for_accepts_its_connection() {
    let root = tempfile::tempdir().expect("tempdir");
    let (address, _token, _relay) = relay(root.path(), Limits::default()).await;
    let a = instance(root.path(), "laptop");
    let b = instance(root.path(), "studio");
    let c = instance(root.path(), "intruder");
    // The studio's control connection, by hand.
    let (mut control, answer) = said(&b, &address, &format!("LISTEN {}", address.token())).await;
    assert_eq!(answer.as_deref(), Some("OK"));
    let asking = tokio::spawn({
        let (a, address, fingerprint) = (a.clone(), address.clone(), b.card.fingerprint.clone());
        async move { said(&a, &address, &dial(&address, &fingerprint)).await }
    });
    let Ok(Line::Connect { id, fingerprint }) = relay::read_command(&mut control).await else {
        panic!("no CONNECT for the studio");
    };
    assert_eq!(
        fingerprint, a.card.fingerprint,
        "it names the computer asking"
    );
    let accept = format!("ACCEPT {} {id}", address.token());
    assert_eq!(
        say(&c, &address, &accept).await,
        None,
        "taken by another computer"
    );
    let (_accepted, answer) = said(&b, &address, &accept).await;
    assert_eq!(answer.as_deref(), Some("OK"), "the studio still accepts it");
    let (_through, answer) = asking.await.expect("dial");
    assert_eq!(answer.as_deref(), Some("OK"));
}

/// A computer serving all it can turns a new one down at once (`BUSY`),
/// and the computer asking is told the relay is busy, not left waiting.
#[tokio::test]
async fn a_computer_serving_all_it_can_turns_the_next_one_down() {
    let root = tempfile::tempdir().expect("tempdir");
    let (address, _token, _relay) = relay(root.path(), Limits::default()).await;
    let a = instance(root.path(), "laptop");
    let b = instance(root.path(), "studio");
    let (held, calls, _b) = holding_with(b.clone(), trusting(&a), address.clone(), 0);
    until("the studio holds the relay", || held.load(Ordering::SeqCst)).await;
    let started = Instant::now();
    assert_eq!(
        say(&a, &address, &dial(&address, &b.card.fingerprint))
            .await
            .as_deref(),
        Some("ERR busy")
    );
    assert!(
        started.elapsed() < Duration::from_secs(3),
        "{:?}",
        started.elapsed()
    );
    assert_eq!(calls.load(Ordering::SeqCst), 0);
}

/// A relay holds so many computers and puts so many pairs through, and no
/// more: one more computer is told the relay is full, one more call busy;
/// one computer asks for only so many at once, while another is still put
/// through. A connection that says nothing is closed within five seconds.
#[tokio::test]
async fn a_relay_takes_on_only_so_much() {
    let root = tempfile::tempdir().expect("tempdir");
    let limits = Limits {
        registrations: 1,
        pairs: 2,
        pairs_per_computer: 1,
        ..Limits::default()
    };
    let (address, _token, _relay) = relay(root.path(), limits).await;
    let a = instance(root.path(), "laptop");
    let b = instance(root.path(), "studio");
    let c = instance(root.path(), "tablet");
    let (held, _calls, _b) = holding(b.clone(), trusting(&a), address.clone());
    until("the studio holds the relay", || held.load(Ordering::SeqCst)).await;
    // Another computer: full.
    let other = instance(root.path(), "other");
    let listen = format!("LISTEN {}", address.token());
    assert_eq!(
        say(&other, &address, &listen).await.as_deref(),
        Some("ERR full")
    );
    // The laptop's one pair, kept open: its next is busy, the tablet's
    // goes through; then the relay's two are taken, and the next is busy.
    let to_b = dial(&address, &b.card.fingerprint);
    let (open, answer) = said(&a, &address, &to_b).await;
    assert_eq!(answer.as_deref(), Some("OK"));
    assert_eq!(say(&a, &address, &to_b).await.as_deref(), Some("ERR busy"));
    let (tablet, answer) = said(&c, &address, &to_b).await;
    assert_eq!(
        answer.as_deref(),
        Some("OK"),
        "another computer has its own share"
    );
    assert_eq!(
        say(&other, &address, &to_b).await.as_deref(),
        Some("ERR busy")
    );
    drop((open, tablet));
    // Free again once those pairs are gone.
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        match said(&a, &address, &to_b).await.1.as_deref() {
            Some("OK") => break,
            other => assert!(Instant::now() < deadline, "still busy: {other:?}"),
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    // Nothing said: closed within its five seconds.
    let tls = Arc::new(a.client_config(address.fingerprint(), false).expect("TLS"));
    let mut silent = relay::reach(&address, tls).await.expect("reach");
    let started = Instant::now();
    let mut byte = [0u8; 1];
    let read = tokio::time::timeout(Duration::from_secs(15), silent.read(&mut byte))
        .await
        .expect("closed in time");
    assert!(matches!(read, Ok(0) | Err(_)), "{read:?}");
    let waited = started.elapsed();
    assert!(
        waited >= Duration::from_secs(4) && waited < Duration::from_secs(8),
        "closed after {waited:?}"
    );
}

/// One source address opens connections at its rate: past its burst, a
/// new connection is closed before TLS.
#[tokio::test]
async fn one_source_opens_connections_at_its_rate() {
    let root = tempfile::tempdir().expect("tempdir");
    let limits = Limits {
        rate: Rate {
            burst: 2,
            per_second: 1,
        },
        ..Limits::default()
    };
    let (address, _token, _relay) = relay(root.path(), limits).await;
    let a = instance(root.path(), "laptop");
    let tls = Arc::new(a.client_config(address.fingerprint(), false).expect("TLS"));
    let mut reached = 0;
    for _ in 0..4 {
        if relay::reach(&address, tls.clone()).await.is_ok() {
            reached += 1;
        }
    }
    assert_eq!(reached, 2, "the burst, then nothing");
}

fn has_ipv6_loopback() -> bool {
    let present = std::net::TcpListener::bind((std::net::Ipv6Addr::LOCALHOST, 0)).is_ok();
    if !present {
        eprintln!("skipped: this computer has no IPv6 loopback");
    }
    present
}
/// Whether a TCP connection to `address` is closed at once, before any TLS.
async fn closed_at_once(address: SocketAddr) -> bool {
    let mut stream = TcpStream::connect(address).await.expect("TCP");
    let mut byte = [0u8; 1];
    matches!(
        tokio::time::timeout(Duration::from_secs(1), stream.read(&mut byte)).await,
        Ok(Ok(0) | Err(_))
    )
}

/// Connections that never say a word cost nothing to open (no token, not
/// even TLS), so one source holds only so many at once: 127.0.0.1 opens
/// its sixteen and the next is closed at once, while ::1 (another source)
/// still holds the relay and is put through. With every place taken, the
/// source holding the most gives one up for a source holding none.
#[tokio::test]
async fn one_source_cannot_use_up_the_places_of_the_others() {
    if !has_ipv6_loopback() {
        return;
    }
    let root = tempfile::tempdir().expect("tempdir");
    let listener =
        hmesh_transport::bind_every_interface("[::]:0".parse().expect("address")).expect("listen");
    let port = listener.local_addr().expect("address").port();
    let limits = Limits::default();
    let (v4, _token, _relay) = relay_on(root.path(), listener, limits);
    let v6 = at(&v4, &format!("[::1]:{port}"));
    let a = instance(root.path(), "laptop");
    let b = instance(root.path(), "studio");
    // 127.0.0.1 holds all its places with connections that say nothing.
    let v4_port = SocketAddr::from(([127, 0, 0, 1], port));
    let mut silent = Vec::new();
    for _ in 0..limits.unauthenticated_per_source {
        silent.push(TcpStream::connect(v4_port).await.expect("TCP"));
    }
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert!(closed_at_once(v4_port).await, "a seventeenth was let in");
    // ::1 is another source: the studio holds the relay there, and the
    // laptop is put through and answered.
    let (held, calls, _b) = holding(b.clone(), trusting(&a), v6.clone());
    until("the studio holds the relay over ::1", || {
        held.load(Ordering::SeqCst)
    })
    .await;
    let (_stop, stopped) = watch::channel(false);
    let dialer = RelayDialer::new(a.clone(), stopped);
    let counted: usize = client_for(&a, &v6, &b, &dialer)
        .post("/count", &serde_json::json!({}))
        .await
        .expect("put through while another source holds its places");
    assert_eq!(counted, 1);
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    drop(silent);

    // Every place taken: 127.0.0.1 holding four of four, ::1 none. A
    // connection from ::1 is let in (127.0.0.1's oldest goes), and is
    // answered.
    let root = tempfile::tempdir().expect("tempdir");
    let listener =
        hmesh_transport::bind_every_interface("[::]:0".parse().expect("address")).expect("listen");
    let port = listener.local_addr().expect("address").port();
    let limits = Limits {
        unauthenticated: 4,
        unauthenticated_per_source: 4,
        ..Limits::default()
    };
    let (v4, _token, _relay) = relay_on(root.path(), listener, limits);
    let v6 = at(&v4, &format!("[::1]:{port}"));
    let v4_port = SocketAddr::from(([127, 0, 0, 1], port));
    let mut silent = Vec::new();
    for _ in 0..4 {
        silent.push(TcpStream::connect(v4_port).await.expect("TCP"));
    }
    tokio::time::sleep(Duration::from_millis(100)).await;
    let listen = format!("LISTEN {}", v6.token());
    assert_eq!(
        say(&b, &v6, &listen).await.as_deref(),
        Some("OK"),
        "no place made for another source"
    );
    let mut byte = [0u8; 1];
    let oldest = tokio::time::timeout(Duration::from_secs(1), silent[0].read(&mut byte)).await;
    assert!(
        matches!(oldest, Ok(Ok(0) | Err(_))),
        "the oldest of the source holding the most was kept: {oldest:?}"
    );
}

/// Reaches `to` over a long link: a connection takes a whole round trip to
/// open, and everything read either way arrives half a round trip later.
async fn far(to: SocketAddr, round_trip: Duration) -> SocketAddr {
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("listen");
    let address = listener.local_addr().expect("address");
    tokio::spawn(async move {
        while let Ok((inbound, _)) = listener.accept().await {
            tokio::spawn(async move {
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
    address
}
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
/// A loopback address where nothing comes back: a listener whose one-place
/// queue is full and never taken.
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

/// The relay far away (150 ms there and back, the studio next to it), and
/// the address on the studio's card sends nothing back: in the time the
/// panel's list of computers gives each call (1.5 s for the first, then
/// 750 ms), the laptop reaches it through the relay and then asks again.
#[tokio::test]
async fn a_computer_behind_a_far_relay_answers_in_the_time_its_callers_give() {
    let root = tempfile::tempdir().expect("tempdir");
    let (address, _token, _relay) = relay(root.path(), Limits::default()).await;
    let a = instance(root.path(), "laptop");
    let b = instance(root.path(), "studio");
    let (held, calls, _b) = holding(b.clone(), trusting(&a), address.clone());
    until("the studio holds the relay", || held.load(Ordering::SeqCst)).await;
    let relay_port = SocketAddr::from(([127, 0, 0, 1], address.port()));
    let far_relay = at(
        &address,
        &far(relay_port, Duration::from_millis(150))
            .await
            .to_string(),
    );
    let (card, _silent) = silent().await;
    let (_stop, stopped) = watch::channel(false);
    let dialer = RelayDialer::new(a.clone(), stopped);
    let memory = Arc::new(RouteMemory::new());
    let routed = |memory: &Arc<RouteMemory>| {
        Client::routed_through(
            &a,
            vec![card.clone(), far_relay.to_string()],
            &b.card.fingerprint,
            memory.clone(),
            &dialer,
            false,
        )
        .expect("client")
    };
    let started = Instant::now();
    let who = tokio::time::timeout(
        Duration::from_millis(1500),
        routed(&memory).get::<String>("/who"),
    )
    .await;
    assert!(
        matches!(&who, Ok(Ok(name)) if name == "studio"),
        "{who:?} after {:?}",
        started.elapsed()
    );
    let started = Instant::now();
    let counted = tokio::time::timeout(
        Duration::from_millis(750),
        routed(&memory).post::<_, usize>("/count", &serde_json::json!({})),
    )
    .await;
    assert!(
        matches!(counted, Ok(Ok(1))),
        "{counted:?} after {:?}",
        started.elapsed()
    );
    assert_eq!(calls.load(Ordering::SeqCst), 1, "run once");
    assert_eq!(
        memory.last_good(&b.card.fingerprint).as_deref(),
        Some(far_relay.to_string().as_str())
    );
}

/// `instance` holding a connection to `relay`, taking every computer that
/// asks, each connection handed to `arrived` as it comes.
fn holding_by_hand(
    instance: Arc<Instance>,
    relay: RelayAddress,
    arrived: relay::Arrived,
) -> (Arc<AtomicBool>, watch::Sender<bool>) {
    let (stop, stopped) = watch::channel(false);
    let held = Arc::new(AtomicBool::new(false));
    let shown = held.clone();
    let tls = Arc::new(
        instance
            .client_config(relay.fingerprint(), false)
            .expect("TLS to the relay"),
    );
    let seats = relay::Seats::new(
        relay::SERVED_AT_ONCE,
        relay::SERVED_PER_CALLER,
        relay::SERVED_STRANGERS,
        |_: &str| relay::Caller::Paired,
    );
    tokio::spawn(relay::hold(
        relay,
        tls,
        seats,
        arrived,
        Arc::new(move |now| shown.store(now, Ordering::SeqCst)),
        stopped,
    ));
    (held, stop)
}

/// The studio reads a request's head and drops the connection without an
/// answer. The request was sent: the call fails, and it is not sent again,
/// on a new connection or anywhere else.
#[tokio::test]
async fn a_request_sent_before_its_connection_broke_is_not_sent_again() {
    let root = tempfile::tempdir().expect("tempdir");
    let (address, _token, _relay) = relay(root.path(), Limits::default()).await;
    let a = instance(root.path(), "laptop");
    let b = instance(root.path(), "studio");
    let acceptor = tokio_rustls::TlsAcceptor::from(Arc::new(
        b.server_config(std::slice::from_ref(&trusting(&a)))
            .expect("TLS"),
    ));
    let (heads, connections) = (Arc::new(AtomicUsize::new(0)), Arc::new(AtomicUsize::new(0)));
    let (read, came) = (heads.clone(), connections.clone());
    let arrived: relay::Arrived = Arc::new(move |stream, seat| {
        let (acceptor, read, came) = (acceptor.clone(), read.clone(), came.clone());
        tokio::spawn(async move {
            let _seat = seat;
            came.fetch_add(1, Ordering::SeqCst);
            let Ok(mut tls) = acceptor.accept(stream).await else {
                return;
            };
            let mut head = Vec::new();
            let mut byte = [0u8; 1];
            while !head.ends_with(b"\r\n\r\n") {
                match tls.read(&mut byte).await {
                    Ok(1) => head.push(byte[0]),
                    _ => return,
                }
            }
            read.fetch_add(1, Ordering::SeqCst);
            // Dropped here, without a word.
        });
    });
    let (held, _b) = holding_by_hand(b.clone(), address.clone(), arrived);
    until("the studio holds the relay", || held.load(Ordering::SeqCst)).await;
    let (_stop, stopped) = watch::channel(false);
    let dialer = RelayDialer::new(a.clone(), stopped);
    let error = client_for(&a, &address, &b, &dialer)
        .post::<_, usize>("/count", &serde_json::json!({}))
        .await
        .expect_err("no answer came");
    assert_eq!(error.code, ErrorCode::PeerUnreachable, "{error:?}");
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert_eq!(heads.load(Ordering::SeqCst), 1, "sent once");
    assert_eq!(
        connections.load(Ordering::SeqCst),
        1,
        "no second connection"
    );
}

/// The studio closes the connection kept for the next call before that
/// call: the call goes on a new connection, runs once, and is answered.
#[tokio::test]
async fn a_kept_connection_closed_before_the_next_call_is_replaced() {
    let root = tempfile::tempdir().expect("tempdir");
    let (address, _token, _relay) = relay(root.path(), Limits::default()).await;
    let a = instance(root.path(), "laptop");
    let b = instance(root.path(), "studio");
    let calls = Arc::new(AtomicUsize::new(0));
    let app = Router::new()
        .route(
            "/count",
            post(|State(calls): State<Arc<AtomicUsize>>| async move {
                Json(calls.fetch_add(1, Ordering::SeqCst) + 1)
            }),
        )
        .with_state(calls.clone());
    let closers: Arc<std::sync::Mutex<Vec<watch::Sender<bool>>>> = Arc::default();
    let (server, kept) = (b.clone(), closers.clone());
    let trusted = trusting(&a);
    let arrived: relay::Arrived = Arc::new(move |stream, seat| {
        let config = server
            .server_config(std::slice::from_ref(&trusted))
            .expect("TLS");
        let (close, closing) = watch::channel(false);
        kept.lock().expect("closers").push(close);
        let app = app.clone();
        tokio::spawn(async move {
            let _seat = seat;
            hmesh_transport::serve_connection(
                stream,
                SocketAddr::new(relay::RELAYED, 0),
                app,
                config,
                closing,
            )
            .await;
        });
    });
    let (held, _b) = holding_by_hand(b.clone(), address.clone(), arrived);
    until("the studio holds the relay", || held.load(Ordering::SeqCst)).await;
    let (_stop, stopped) = watch::channel(false);
    let dialer = RelayDialer::new(a.clone(), stopped);
    let client = client_for(&a, &address, &b, &dialer);
    for expected in 1..=3usize {
        let counted: usize = client
            .post("/count", &serde_json::json!({}))
            .await
            .expect("answered");
        assert_eq!(counted, expected);
        assert_eq!(calls.load(Ordering::SeqCst), expected, "run once");
        assert_eq!(dialer.kept(), 1);
        // The studio closes it (the end reaches the laptop through the
        // relay a moment later).
        for close in closers.lock().expect("closers").drain(..) {
            let _ = close.send(true);
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
}

/// A connection through the relay that goes unused for the studio's idle
/// time is closed there; the next call opens a new one.
#[tokio::test]
async fn a_connection_through_a_relay_left_unused_is_closed() {
    let root = tempfile::tempdir().expect("tempdir");
    let (address, _token, _relay) = relay(root.path(), Limits::default()).await;
    let a = instance(root.path(), "laptop");
    let b = instance(root.path(), "studio");
    let idle = Duration::from_millis(400);
    let (held, calls, _b, open) = holding_for(
        b.clone(),
        trusting(&a),
        address.clone(),
        relay::SERVED_AT_ONCE,
        true,
        idle,
    );
    until("the studio holds the relay", || held.load(Ordering::SeqCst)).await;
    let (_stop, stopped) = watch::channel(false);
    let dialer = RelayDialer::new(a.clone(), stopped);
    let client = client_for(&a, &address, &b, &dialer);
    let counted: usize = client
        .post("/count", &serde_json::json!({}))
        .await
        .expect("answered");
    assert_eq!(counted, 1);
    assert_eq!(open.load(Ordering::SeqCst), 1);
    let started = Instant::now();
    until("closed once unused", || open.load(Ordering::SeqCst) == 0).await;
    assert!(started.elapsed() >= idle / 2, "{:?}", started.elapsed());
    // The laptop hears of it through the relay a moment later; a call made
    // before that would still go out on the closed connection.
    until("the laptop sees it closed", || dialer.kept() == 0).await;
    let counted: usize = client
        .post("/count", &serde_json::json!({}))
        .await
        .expect("answered on a new connection");
    assert_eq!(counted, 2);
    assert_eq!(calls.load(Ordering::SeqCst), 2);
    assert!(relay::KEPT_IDLE < hmesh_transport::RELAYED_IDLE);
}

/// A studio that is not pairing takes only the computers it is paired
/// with: another one is turned down at once and told why, and the one it
/// is paired with is still put through.
#[tokio::test]
async fn a_computer_turns_down_one_it_is_not_paired_with() {
    let root = tempfile::tempdir().expect("tempdir");
    let (address, _token, _relay) = relay(root.path(), Limits::default()).await;
    let a = instance(root.path(), "laptop");
    let b = instance(root.path(), "studio");
    let c = instance(root.path(), "stranger");
    let (held, calls, _b, _open) = holding_for(
        b.clone(),
        trusting(&a),
        address.clone(),
        relay::SERVED_AT_ONCE,
        false,
        hmesh_transport::RELAYED_IDLE,
    );
    until("the studio holds the relay", || held.load(Ordering::SeqCst)).await;
    let started = Instant::now();
    assert_eq!(
        say(&c, &address, &dial(&address, &b.card.fingerprint))
            .await
            .as_deref(),
        Some("ERR closed")
    );
    assert!(started.elapsed() < Duration::from_secs(3));
    let (_stop, stopped) = watch::channel(false);
    let dialer = RelayDialer::new(c.clone(), stopped.clone());
    let error = client_for(&c, &address, &b, &dialer)
        .post::<_, usize>("/count", &serde_json::json!({}))
        .await
        .expect_err("turned down");
    assert_eq!(
        unreachable_reasons(&error.message),
        ["not_accepted"],
        "{error:?}"
    );
    let dialer = RelayDialer::new(a.clone(), stopped);
    let counted: usize = client_for(&a, &address, &b, &dialer)
        .post("/count", &serde_json::json!({}))
        .await
        .expect("the paired one");
    assert_eq!(counted, 1);
    assert_eq!(calls.load(Ordering::SeqCst), 1);
}

/// Inside, a connection shows the certificate it showed the relay (the one
/// its places were counted by): one that shows the certificate of another
/// computer the studio trusts is closed, and nothing is run.
#[tokio::test]
async fn a_connection_shows_inside_the_certificate_it_showed_the_relay() {
    let root = tempfile::tempdir().expect("tempdir");
    let (address, _token, _relay) = relay(root.path(), Limits::default()).await;
    let a = instance(root.path(), "laptop");
    let b = instance(root.path(), "studio");
    let d = instance(root.path(), "desk");
    let (held, calls, _b, _open) = holding_for(
        b.clone(),
        trusting(&d),
        address.clone(),
        relay::SERVED_AT_ONCE,
        true,
        hmesh_transport::RELAYED_IDLE,
    );
    until("the studio holds the relay", || held.load(Ordering::SeqCst)).await;
    let (_stop, stopped) = watch::channel(false);
    // To the relay as the laptop, inside as the desk.
    let as_laptop = RelayDialer::new(a.clone(), stopped.clone());
    let mixed = client_for(&d, &address, &b, &as_laptop)
        .post::<_, usize>("/count", &serde_json::json!({}))
        .await;
    assert!(mixed.is_err(), "{mixed:?}");
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    let as_desk = RelayDialer::new(d.clone(), stopped);
    let counted: usize = client_for(&d, &address, &b, &as_desk)
        .post("/count", &serde_json::json!({}))
        .await
        .expect("the desk as itself");
    assert_eq!(counted, 1);
}
