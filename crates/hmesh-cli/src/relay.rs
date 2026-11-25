//! `hmesh relay`: run a relay on a server (`serve`, `show`), and choose the
//! relays this computer holds a connection to (`use`, `forget`, `list`).
//! A relay code carries the relay's token: it is printed for its owner to
//! copy, never logged.
use anyhow::{Context, Result};
use hmesh_core::RelayAddress;
use hmesh_security::{
    Instance,
    relay::{RelayIdentity, RelaySettings},
};
use hmesh_transport::relay::{self, Limits, Token};
use serde_json::{Value, json};
use std::{
    io::Write,
    net::{IpAddr, SocketAddr},
    path::Path,
    sync::Arc,
};

/// Run the relay in `dir` (made the first time) on `listen` until Ctrl-C
/// or SIGTERM. Its relay code goes to standard output first, once.
pub async fn serve(
    listen: SocketAddr,
    dir: &Path,
    public_address: Option<String>,
    json_output: bool,
) -> Result<()> {
    tracing_subscriber::fmt()
        .json()
        .with_writer(std::io::stderr)
        .with_env_filter("info")
        .try_init()
        .ok();
    let identity = Arc::new(RelayIdentity::open(dir)?);
    let listener = tokio::net::TcpListener::bind(listen)
        .await
        .with_context(|| format!("listen on {listen} for the relay"))?;
    let listening = listener.local_addr()?;
    // A public address given before is kept until another one is.
    let settings = RelaySettings {
        listen: listening,
        public_address: public_address
            .or_else(|| identity.settings().and_then(|s| s.public_address)),
    };
    identity.set_settings(&settings)?;
    let (authority, guessed) = authority(&settings)?;
    let code = identity.code(&authority)?;
    say_code(&code, &identity, json_output)?;
    if guessed {
        eprintln!(
            "This code names {authority}, this server's own address. If other computers reach it at another one (a cloud server's public IP), print the code for that: hmesh relay show --dir {} --public-address <public ip>:{}",
            identity.dir.display(),
            listening.port()
        );
    }
    let (shutdown, stopped) = tokio::sync::watch::channel(false);
    tokio::spawn(async move {
        if stop_signal().await {
            let _ = shutdown.send(true);
        }
    });
    let token = Token::of(identity.clone(), stopped.clone())?;
    eprintln!(
        "{}",
        json!({"message": "relay listening", "listening": listening, "fingerprint": identity.fingerprint})
    );
    relay::serve(
        listener,
        Arc::new(identity.server_config()?),
        token,
        Limits::default(),
        stopped,
    )
    .await?;
    eprintln!("{}", json!({"message": "relay stopped"}));
    Ok(())
}
/// The relay code of the relay in `dir` again; with `rotate`, after giving
/// it a new token (a running relay takes it within seconds, and every
/// computer using the old one is cut off).
pub fn show(dir: &Path, rotate: bool, public_address: Option<String>) -> Result<Value> {
    let identity = RelayIdentity::load(dir)?;
    if rotate {
        identity.rotate_token()?;
    }
    let mut settings = identity.settings();
    if let Some(public) = public_address {
        let mut kept = settings.clone().unwrap_or(RelaySettings {
            listen: SocketAddr::from(([0, 0, 0, 0], 0)),
            public_address: None,
        });
        kept.public_address = Some(public);
        // Checked before it is kept.
        identity.code(&authority(&kept)?.0)?;
        identity.set_settings(&kept)?;
        settings = Some(kept);
    }
    let settings = settings.context(
        "this relay has not run yet: hmesh relay serve --listen <address>:<port> --dir <this directory> first, or give --public-address",
    )?;
    let code = identity.code(&authority(&settings)?.0)?;
    Ok(json!({
        "relay_code": code.to_string(),
        "relay": code.label(),
        "fingerprint": identity.fingerprint,
        "token_rotated": rotate,
        "next": "On each computer that should be reached through this relay, run hmesh relay use and paste the relay code when asked (it is read from standard input, so it stays out of the shell history)",
    }))
}
fn say_code(code: &RelayAddress, identity: &RelayIdentity, json_output: bool) -> Result<()> {
    let mut out = std::io::stdout().lock();
    if json_output {
        writeln!(
            out,
            "{}",
            json!({"relay_code": code.to_string(), "relay": code.label(), "fingerprint": identity.fingerprint})
        )?;
    } else {
        writeln!(
            out,
            "Relay code (keep it secret; on each computer that should be reached through this relay, run hmesh relay use and paste it when asked):"
        )?;
        writeln!(out, "{code}")?;
    }
    out.flush()?;
    Ok(())
}
/// Where the relay code says the relay is: the public address it was
/// given, or where it listens; listening on every interface, this
/// computer's address on its default route (and true: a guess).
fn authority(settings: &RelaySettings) -> Result<(String, bool)> {
    if let Some(public) = &settings.public_address {
        return Ok((public.trim().to_owned(), false));
    }
    let listen = settings.listen;
    if !listen.ip().is_unspecified() {
        return Ok((listen.to_string(), false));
    }
    // Connecting a UDP socket sends nothing; it only picks the address
    // this computer would send from.
    let guess = |any: SocketAddr, far: SocketAddr| -> Option<IpAddr> {
        let socket = std::net::UdpSocket::bind(any).ok()?;
        socket.connect(far).ok()?;
        Some(socket.local_addr().ok()?.ip())
    };
    let ip = guess(
        SocketAddr::from(([0, 0, 0, 0], 0)),
        SocketAddr::from(([192, 0, 2, 1], 9)),
    )
    .filter(|ip| !ip.is_unspecified() && !ip.is_loopback())
    .context("listening on every interface, this relay has no address of its own to name: give --public-address <address>:<port>")?;
    Ok((SocketAddr::new(ip, listen.port()).to_string(), true))
}
/// Ctrl-C, or SIGTERM on Unix (a service manager stopping the relay).
async fn stop_signal() -> bool {
    #[cfg(unix)]
    {
        use tokio::signal::unix::{SignalKind, signal};
        let Ok(mut terminate) = signal(SignalKind::terminate()) else {
            return tokio::signal::ctrl_c().await.is_ok();
        };
        tokio::select! {
            result = tokio::signal::ctrl_c() => result.is_ok(),
            _ = terminate.recv() => true,
        }
    }
    #[cfg(not(unix))]
    {
        tokio::signal::ctrl_c().await.is_ok()
    }
}

/// What this computer does about relays: through its daemon when it runs
/// (which takes the change at once), in node.toml otherwise.
pub enum Change {
    Use(String),
    Forget,
    List,
}
pub async fn computer(instance: &Instance, change: Change) -> Result<Value> {
    if let Some(client) = hmesh_daemon::running(&instance.dir).await {
        return Ok(match change {
            Change::Use(code) => {
                client
                    .post("/local/relays/use", &hmesh_daemon::UseRelay { code })
                    .await?
            }
            Change::Forget => client.post("/local/relays/forget", &json!({})).await?,
            Change::List => client.get("/local/relays").await?,
        });
    }
    let mut relays = hmesh_daemon::configured_relays(&instance.config);
    match change {
        Change::Use(code) => {
            if !hmesh_daemon::takes_relayed(&instance.config) {
                return Err(hmesh_core::MeshError::input(
                    "this computer only connects out, so nothing reaches it through a relay; it still reaches other computers through theirs",
                )
                .into());
            }
            let relay = hmesh_security::validate_relay(&code)?;
            relays = hmesh_daemon::with_relay(&relays, relay)?;
            hmesh_daemon::write_relays(&instance.dir, &relays)?;
        }
        Change::Forget => {
            relays.clear();
            hmesh_daemon::write_relays(&instance.dir, &relays)?;
        }
        Change::List => {}
    }
    Ok(Value::Array(
        relays
            .iter()
            .map(|relay| json!({"relay": relay.label(), "connected": false, "since": null}))
            .collect(),
    ))
}
