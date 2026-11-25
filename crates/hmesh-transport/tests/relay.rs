//! The built-in relay, in one process: a computer holding a connection to
//! it is reached through it, with the right token only; the relay knows
//! nothing of what passes; and it takes on only so much.
use axum::{Json, Router, extract::State, routing::post};
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
    net::TcpListener,
    sync::watch,
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
/// A relay on a new loopback port, its relay address, and its token (to
/// change it).
async fn relay(
    root: &std::path::Path,
    limits: Limits,
) -> (RelayAddress, Token, watch::Sender<bool>) {
    let identity = RelayIdentity::open(&root.join("relay")).expect("relay identity");
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("listen");
    let address = listener.local_addr().expect("address");
    let code = identity.code(&address.to_string()).expect("relay code");
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
/// `instance` holding a connection to `relay`, serving `/count` (it counts
/// the calls it runs) to the computer it trusts, on what comes through.
/// Whether it holds the relay now, and its calls.
fn holding(
    instance: Arc<Instance>,
    trusted: Peer,
    relay: RelayAddress,
) -> (Arc<AtomicBool>, Arc<AtomicUsize>, watch::Sender<bool>) {
    let calls = Arc::new(AtomicUsize::new(0));
    let app = Router::new()
        .route(
            "/count",
            post(|State(calls): State<Arc<AtomicUsize>>| async move {
                Json(calls.fetch_add(1, Ordering::SeqCst) + 1)
            }),
        )
        .with_state(calls.clone());
    let (stop, stopped) = watch::channel(false);
    let held = Arc::new(AtomicBool::new(false));
    let shown = held.clone();
    let server = instance.clone();
    let serving = stopped.clone();
    let arrived: relay::Arrived = Arc::new(move |stream| {
        let config = server
            .server_config(std::slice::from_ref(&trusted))
            .expect("TLS");
        tokio::spawn(hmesh_transport::serve_connection(
            stream,
            SocketAddr::new(relay::RELAYED, 0),
            app.clone(),
            config,
            serving.clone(),
        ));
    });
    let tls = Arc::new(
        instance
            .client_config(relay.fingerprint(), false)
            .expect("TLS to the relay"),
    );
    tokio::spawn(relay::hold(
        relay,
        tls,
        arrived,
        Arc::new(move |now| shown.store(now, Ordering::SeqCst)),
        stopped,
    ));
    (held, calls, stop)
}
async fn until(what: &str, ready: impl Fn() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(15);
    while !ready() {
        assert!(Instant::now() < deadline, "{what}");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}
/// A connection to the relay as `instance`, said `line` on: its first
/// answer, or `None` when the relay closed it without a word.
async fn say(instance: &Instance, relay: &RelayAddress, line: &str) -> Option<String> {
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
    tokio::time::timeout(Duration::from_secs(12), relay::read_line(&mut stream))
        .await
        .expect("an answer or the end, in time")
        .ok()
}

/// The computer behind the relay answers a call through it, exactly once
/// each; the call goes through a loopback port and the address it is
/// remembered by is the relay's, which is only ever shown by its label. A
/// wrong token is refused, the computer is not reached, and the error says
/// only that its relay did not put it through.
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
    }
    assert_eq!(calls.load(Ordering::SeqCst), 3);
    assert_eq!(
        memory.last_good(&b.card.fingerprint).as_deref(),
        Some(address.to_string().as_str())
    );
    // The same port again: one per relay and computer.
    assert_eq!(
        dialer.port(&address, &b.card.fingerprint).expect("port"),
        dialer.port(&address, &b.card.fingerprint).expect("port")
    );

    // Not with another token.
    let wrong = address.with_token(&"0".repeat(64)).expect("address");
    let client = Client::routed_through(
        &a,
        vec![wrong.to_string()],
        &b.card.fingerprint,
        Arc::new(RouteMemory::new()),
        &dialer,
        false,
    )
    .expect("client");
    let error = client
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
        format!("DIAL {} {}", wrong.token(), b.card.fingerprint),
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
        say(
            &a,
            &address,
            &format!("DIAL {} {}", address.token(), elsewhere.card.fingerprint)
        )
        .await
        .as_deref(),
        Some("ERR offline")
    );

    // A new token: the studio is let go, and the old one no longer works.
    token.set("1".repeat(64));
    until("the studio is let go", || !held.load(Ordering::SeqCst)).await;
    let error = Client::routed_through(
        &a,
        vec![address.to_string()],
        &b.card.fingerprint,
        Arc::new(RouteMemory::new()),
        &dialer,
        false,
    )
    .expect("client")
    .post::<_, usize>("/count", &serde_json::json!({}))
    .await
    .expect_err("the old token is refused");
    assert_eq!(unreachable_reasons(&error.message), ["relay"], "{error:?}");
    let _ = stop.send(true);
}

/// The computer behind a relay must still be the one pinned: a relay that
/// puts the call through to another computer gets nothing sent to it.
#[tokio::test]
async fn a_relay_cannot_pass_another_computer_off_as_the_one_asked_for() {
    let root = tempfile::tempdir().expect("tempdir");
    let (address, _token, _relay) = relay(root.path(), Limits::default()).await;
    let a = instance(root.path(), "laptop");
    let b = instance(root.path(), "studio");
    let c = instance(root.path(), "impostor");
    // The impostor holds the relay; the laptop asks for it but pins the studio.
    let (held, calls, _c) = holding(c.clone(), trusting(&a), address.clone());
    until("the impostor holds the relay", || {
        held.load(Ordering::SeqCst)
    })
    .await;
    let (_stop, stopped) = watch::channel(false);
    let dialer = RelayDialer::new(a.clone(), stopped);
    let port = dialer.port(&address, &c.card.fingerprint).expect("port");
    let client = Client::routed(
        &a,
        vec![format!("https://127.0.0.1:{port}")],
        &b.card.fingerprint,
        Arc::new(RouteMemory::new()),
    )
    .expect("client");
    let error = client
        .post::<_, usize>("/count", &serde_json::json!({}))
        .await
        .expect_err("another computer");
    assert!(error.message.contains("is another computer"), "{error:?}");
    assert_eq!(calls.load(Ordering::SeqCst), 0);
}

/// A relay holds so many computers and puts so many pairs through, and no
/// more: one more computer is told the relay is full, one more call busy.
/// A connection that says nothing is closed within ten seconds.
#[tokio::test]
async fn a_relay_takes_on_only_so_much() {
    let root = tempfile::tempdir().expect("tempdir");
    let limits = Limits {
        registrations: 1,
        pairs: 1,
        ..Limits::default()
    };
    let (address, _token, _relay) = relay(root.path(), limits).await;
    let a = instance(root.path(), "laptop");
    let b = instance(root.path(), "studio");
    let (held, _calls, _b) = holding(b.clone(), trusting(&a), address.clone());
    until("the studio holds the relay", || held.load(Ordering::SeqCst)).await;
    // Another computer: full. The studio again: it replaces its own.
    let other = instance(root.path(), "other");
    let listen = format!("LISTEN {}", address.token());
    assert_eq!(
        say(&other, &address, &listen).await.as_deref(),
        Some("ERR full")
    );
    // One pair at once: kept open (put through, nothing said yet), the
    // next call is busy.
    let tls = Arc::new(a.client_config(address.fingerprint(), false).expect("TLS"));
    let mut open = relay::reach(&address, tls).await.expect("reach");
    relay::write_line(
        &mut open,
        &Line::Dial {
            token: address.token().to_owned(),
            fingerprint: b.card.fingerprint.clone(),
        },
    )
    .await
    .expect("dial");
    assert_eq!(relay::read_command(&mut open).await, Ok(Line::Ok));
    let dial = format!("DIAL {} {}", address.token(), b.card.fingerprint);
    assert_eq!(say(&a, &address, &dial).await.as_deref(), Some("ERR busy"));
    drop(open);
    // Free again once that pair is gone.
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let tls = Arc::new(a.client_config(address.fingerprint(), false).expect("TLS"));
        let mut next = relay::reach(&address, tls).await.expect("reach");
        relay::write_line(
            &mut next,
            &Line::Dial {
                token: address.token().to_owned(),
                fingerprint: b.card.fingerprint.clone(),
            },
        )
        .await
        .expect("dial");
        match relay::read_command(&mut next).await {
            Ok(Line::Ok) => break,
            other => assert!(Instant::now() < deadline, "still busy: {other:?}"),
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    // Nothing said: closed within its ten seconds.
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
        waited >= Duration::from_secs(9) && waited < Duration::from_secs(13),
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
