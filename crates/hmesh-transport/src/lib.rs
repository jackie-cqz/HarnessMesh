//! Bounded HTTPS transport with TLS 1.3 and certificate pins, on loopback,
//! Tailnet or a direct network.
mod client;
pub mod netinfo;
pub mod tailscale;
use axum::{Extension, Router};
pub use client::{Client, RouteMemory};
use hmesh_security::fingerprint;
use hyper_util::{
    rt::{TokioExecutor, TokioIo},
    server::conn::auto::Builder,
    service::TowerToHyperService,
};
use std::{
    net::{IpAddr, SocketAddr},
    sync::Arc,
    time::Duration,
};
use tokio::{
    net::TcpListener,
    sync::{Semaphore, watch},
    task::JoinSet,
};
use tokio_rustls::TlsAcceptor;

#[derive(Clone, Debug)]
pub struct ConnectionIdentity {
    pub fingerprint: Option<String>,
    pub address: SocketAddr,
}
pub type TlsConfigProvider = Arc<dyn Fn() -> anyhow::Result<rustls::ServerConfig> + Send + Sync>;

pub async fn serve(
    listener: TcpListener,
    router: Router,
    tls: TlsConfigProvider,
    shutdown: watch::Receiver<bool>,
) -> anyhow::Result<()> {
    serve_scoped(listener, router, tls, shutdown, Scope::Loopback).await
}
/// Serve other computers: Tailnet sources, or a direct node's allowed ones.
pub async fn serve_peers(
    listener: TcpListener,
    router: Router,
    tls: TlsConfigProvider,
    shutdown: watch::Receiver<bool>,
    scope: Scope,
) -> anyhow::Result<()> {
    serve_scoped(listener, router, tls, shutdown, scope).await
}
/// Who may open a connection to a listener, by source address. A fence in
/// front of TLS, not authentication: peers are identified by their pinned
/// certificates, and only the pairing handshake is anonymous.
#[derive(Clone, Debug)]
pub enum Scope {
    Loopback,
    Tailnet,
    /// A direct node's allowed source ranges.
    Sources(Arc<Vec<hmesh_core::IpNet>>),
}
impl Scope {
    pub fn permits(&self, ip: IpAddr) -> bool {
        let ip = ip.to_canonical();
        match self {
            Self::Loopback => ip.is_loopback(),
            Self::Tailnet => hmesh_core::is_tailnet_ip(ip),
            Self::Sources(nets) => nets.iter().any(|net| net.contains(&ip)),
        }
    }
}
/// Which way a connection to a listener on every interface goes, decided
/// from its two addresses before TLS.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Door {
    /// This computer's owner, on loopback: the owner API, as on a loopback listener.
    Owner,
    /// Another computer, over a trusted network: the peer API only.
    Peers,
    /// Dropped before TLS.
    Closed,
}
/// The fence in front of a listener on every interface: the addresses of
/// this computer on the networks it trusts, which take connections from
/// other computers, and the sources they may come from. Kept current as
/// networks come and go, without a restart.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Fence {
    /// This computer's addresses on trusted networks, each with whether
    /// only Tailnet sources may use it (its Tailscale addresses).
    pub open: Vec<(IpAddr, bool)>,
    /// Every network address it is on now, trusted or not.
    pub observed: Vec<IpAddr>,
    /// The sources other computers may connect from (`allowed_sources`).
    pub sources: Vec<hmesh_core::IpNet>,
}
impl Fence {
    /// Where a connection to `local` from `source` goes. The owner: both
    /// on loopback, unless loopback is one of this computer's networks (only
    /// tests make it one). Another computer: `local` on a trusted network
    /// and `source` allowed (a Tailnet address, for a Tailscale address).
    pub fn door(&self, local: IpAddr, source: IpAddr) -> Door {
        let (local, source) = (local.to_canonical(), source.to_canonical());
        if local.is_loopback() && source.is_loopback() && !self.observed.contains(&local) {
            return Door::Owner;
        }
        match self.open.iter().find(|(address, _)| *address == local) {
            Some((_, true)) if hmesh_core::is_tailnet_ip(source) => Door::Peers,
            Some((_, false)) if self.sources.iter().any(|net| net.contains(&source)) => Door::Peers,
            _ => Door::Closed,
        }
    }
}
/// A listener on every interface at `address` (`[::]:port` or
/// `0.0.0.0:port`). `::` takes IPv4 too (one dual-stack socket); where IPv6
/// is not available it falls back to IPv4 alone. A port already in use is
/// an error either way.
pub fn bind_every_interface(address: SocketAddr) -> std::io::Result<TcpListener> {
    use socket2::{Domain, Protocol, Socket, Type};
    let bind = |address: SocketAddr, dual: bool| -> std::io::Result<TcpListener> {
        let socket = Socket::new(
            Domain::for_address(address),
            Type::STREAM,
            Some(Protocol::TCP),
        )?;
        if dual {
            socket.set_only_v6(false)?;
        }
        // As std's listeners do: a restart is not held up by connections
        // the last run left in TIME_WAIT.
        #[cfg(unix)]
        socket.set_reuse_address(true)?;
        socket.bind(&address.into())?;
        socket.listen(1024)?;
        socket.set_nonblocking(true)?;
        TcpListener::from_std(socket.into())
    };
    let v4 = SocketAddr::new(std::net::Ipv4Addr::UNSPECIFIED.into(), address.port());
    match address.ip() {
        IpAddr::V6(ip) if ip.is_unspecified() => match bind(address, true) {
            Err(error) if error.kind() != std::io::ErrorKind::AddrInUse => {
                tracing::warn!(%error, "no IPv6 listener; listening on IPv4 only");
                bind(v4, false)
            }
            result => result,
        },
        IpAddr::V4(ip) if ip.is_unspecified() => bind(v4, false),
        _ => Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "a listener on every interface binds 0.0.0.0 or ::",
        )),
    }
}
/// How often a connection from another computer is checked against the
/// fence again: one over a network no longer trusted is closed.
const FENCE_RECHECK: Duration = Duration::from_secs(1);
/// Serve a listener on every interface: each connection goes through the
/// door `fence` gives it (read again for every connection, so a change of
/// networks takes effect at once). The owner gets `owner`, other computers
/// `peers`; anything else is dropped before TLS. A connection from another
/// computer is checked again every second and closed (gracefully, as at
/// shutdown) once the fence no longer lets it in: a network forgotten or
/// left also ends the connections kept open over it.
pub async fn serve_every_interface(
    listener: TcpListener,
    owner: Router,
    peers: Router,
    tls: TlsConfigProvider,
    mut shutdown: watch::Receiver<bool>,
    fence: Arc<std::sync::RwLock<Fence>>,
) -> anyhow::Result<()> {
    if !listener.local_addr()?.ip().is_unspecified() {
        anyhow::bail!("a listener on every interface binds 0.0.0.0 or ::");
    }
    // Apart, so other computers cannot use up the owner's connections.
    let owner_slots = Arc::new(Semaphore::new(64));
    let peer_slots = Arc::new(Semaphore::new(64));
    let mut connections = JoinSet::new();
    loop {
        tokio::select! {
            biased;
            changed = shutdown.changed() => { if changed.is_err() || *shutdown.borrow() { break; } }
            accepted = listener.accept() => {
                let (stream, address) = accepted?;
                let Ok(local) = stream.local_addr() else { continue; };
                let door = fence.read().map(|f| f.door(local.ip(), address.ip())).unwrap_or(Door::Closed);
                let (app, slots) = match door {
                    Door::Owner => (owner.clone(), &owner_slots),
                    Door::Peers => (peers.clone(), &peer_slots),
                    Door::Closed => continue,
                };
                let Ok(permit) = slots.clone().try_acquire_owned() else { continue; };
                let config = match tls() { Ok(config) => config, Err(error) => { tracing::error!(%error, "TLS configuration rejected"); continue; } };
                let mut stop = shutdown.clone();
                let fence = fence.clone();
                connections.spawn(async move {
                    let _permit = permit;
                    let (close, closed) = watch::channel(false);
                    let serving = serve_connection(stream, address, app, config, closed);
                    tokio::pin!(serving);
                    let still_let_in = || {
                        fence.read().map(|f| f.door(local.ip(), address.ip())).unwrap_or(Door::Closed) == door
                    };
                    let mut recheck = tokio::time::interval(FENCE_RECHECK);
                    loop {
                        tokio::select! {
                            _ = &mut serving => return,
                            changed = stop.changed() => if changed.is_err() || *stop.borrow() { break },
                            _ = recheck.tick(), if door == Door::Peers => if !still_let_in() { break },
                        }
                    }
                    let _ = close.send(true);
                    serving.await;
                });
            }
            Some(_) = connections.join_next(), if !connections.is_empty() => {}
        }
    }
    drain(connections).await;
    Ok(())
}
/// Serve one connection that got past its listener's fence: TLS, then HTTP
/// with `app`, until it closes or `stop` says so (then gracefully, within a
/// few seconds). Any byte stream will do, not only TCP: `address` is who
/// it came from, for the routes that limit by source.
pub async fn serve_connection<S>(
    stream: S,
    address: SocketAddr,
    app: Router,
    config: rustls::ServerConfig,
    mut stop: watch::Receiver<bool>,
) where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin + Send + 'static,
{
    let acceptor = TlsAcceptor::from(Arc::new(config));
    let tls = match tokio::time::timeout(Duration::from_secs(5), acceptor.accept(stream)).await {
        Ok(Ok(tls)) => tls,
        _ => return,
    };
    let fingerprint = tls
        .get_ref()
        .1
        .peer_certificates()
        .and_then(|certs| certs.first())
        .map(|cert| fingerprint(cert.as_ref()));
    // An IPv4 client of a dual-stack listener arrives as ::ffff:a.b.c.d.
    let address = SocketAddr::new(address.ip().to_canonical(), address.port());
    let app = app.layer(Extension(ConnectionIdentity {
        fingerprint,
        address,
    }));
    let builder = Builder::new(TokioExecutor::new());
    let connection =
        builder.serve_connection_with_upgrades(TokioIo::new(tls), TowerToHyperService::new(app));
    tokio::pin!(connection);
    tokio::select! {
        _ = &mut connection => {}
        _ = stop.changed() => {
            connection.as_mut().graceful_shutdown();
            let _ = tokio::time::timeout(Duration::from_secs(3), &mut connection).await;
        }
    }
}
/// Let open connections finish for a few seconds, then end them.
async fn drain(mut connections: JoinSet<()>) {
    if tokio::time::timeout(Duration::from_secs(5), async {
        while connections.join_next().await.is_some() {}
    })
    .await
    .is_err()
    {
        connections.abort_all();
        while connections.join_next().await.is_some() {}
    }
}
async fn serve_scoped(
    listener: TcpListener,
    router: Router,
    tls: TlsConfigProvider,
    mut shutdown: watch::Receiver<bool>,
    scope: Scope,
) -> anyhow::Result<()> {
    let local = listener.local_addr()?.ip();
    let listens_where_it_should = match scope {
        Scope::Loopback | Scope::Tailnet => scope.permits(local),
        Scope::Sources(_) => !local.is_unspecified(),
    };
    if !listens_where_it_should {
        anyhow::bail!("listener address does not match its explicit network scope");
    }
    let semaphore = Arc::new(Semaphore::new(64));
    let mut connections = JoinSet::new();
    loop {
        tokio::select! {
            biased;
            changed = shutdown.changed() => { if changed.is_err() || *shutdown.borrow() { break; } }
            accepted = listener.accept() => {
                let (stream, address) = accepted?;
                if !scope.permits(address.ip()) { continue; }
                let Ok(permit) = semaphore.clone().try_acquire_owned() else { continue; };
                let config = match tls() { Ok(config) => config, Err(error) => { tracing::error!(%error, "TLS configuration rejected"); continue; } };
                let app = router.clone();
                let stop = shutdown.clone();
                connections.spawn(async move {
                    let _permit = permit;
                    serve_connection(stream, address, app, config, stop).await;
                });
            }
            Some(_) = connections.join_next(), if !connections.is_empty() => {}
        }
    }
    drain(connections).await;
    Ok(())
}
