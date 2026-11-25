//! Bounded HTTPS transport with TLS 1.3 and certificate pins, on loopback,
//! Tailnet or a direct network.
mod client;
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
                let mut stop = shutdown.clone();
                connections.spawn(async move {
                    let _permit = permit;
                    let acceptor = TlsAcceptor::from(Arc::new(config));
                    let tls = match tokio::time::timeout(Duration::from_secs(5), acceptor.accept(stream)).await {
                        Ok(Ok(tls)) => tls, _ => return,
                    };
                    let fingerprint = tls.get_ref().1.peer_certificates().and_then(|certs| certs.first()).map(|cert| fingerprint(cert.as_ref()));
                    let app = app.layer(Extension(ConnectionIdentity { fingerprint, address }));
                    let builder = Builder::new(TokioExecutor::new());
                    let connection = builder.serve_connection_with_upgrades(TokioIo::new(tls), TowerToHyperService::new(app));
                    tokio::pin!(connection);
                    tokio::select! {
                        _ = &mut connection => {}
                        _ = stop.changed() => {
                            connection.as_mut().graceful_shutdown();
                            let _ = tokio::time::timeout(Duration::from_secs(3), &mut connection).await;
                        }
                    }
                });
            }
            Some(_) = connections.join_next(), if !connections.is_empty() => {}
        }
    }
    if tokio::time::timeout(Duration::from_secs(5), async {
        while connections.join_next().await.is_some() {}
    })
    .await
    .is_err()
    {
        connections.abort_all();
        while connections.join_next().await.is_some() {}
    }
    Ok(())
}
