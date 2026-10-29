//! Bounded loopback/Tailnet HTTPS transport with TLS 1.3 and certificate pins.
pub mod tailscale;
use axum::{Extension, Router};
use hmesh_core::{ErrorCode, MeshError};
use hmesh_security::{Instance, fingerprint, validate_endpoint};
use hyper_util::{
    rt::{TokioExecutor, TokioIo},
    server::conn::auto::Builder,
    service::TowerToHyperService,
};
use serde::{Serialize, de::DeserializeOwned};
use std::{net::SocketAddr, sync::Arc, time::Duration};
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
    serve_scoped(listener, router, tls, shutdown, false).await
}
pub async fn serve_tailnet(
    listener: TcpListener,
    router: Router,
    tls: TlsConfigProvider,
    shutdown: watch::Receiver<bool>,
) -> anyhow::Result<()> {
    serve_scoped(listener, router, tls, shutdown, true).await
}
pub fn permits_source(tailnet: bool, ip: std::net::IpAddr) -> bool {
    if tailnet {
        hmesh_core::is_tailnet_ip(ip)
    } else {
        ip.is_loopback()
    }
}
async fn serve_scoped(
    listener: TcpListener,
    router: Router,
    tls: TlsConfigProvider,
    mut shutdown: watch::Receiver<bool>,
    tailnet: bool,
) -> anyhow::Result<()> {
    if !permits_source(tailnet, listener.local_addr()?.ip()) {
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
                if !permits_source(tailnet, address.ip()) { continue; }
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

#[derive(Clone)]
pub struct Client {
    inner: reqwest::Client,
    endpoint: String,
    owner_token: Option<String>,
}
impl Client {
    pub fn new(
        instance: &Instance,
        endpoint: &str,
        expected_fingerprint: &str,
        bootstrap: bool,
        owner: bool,
    ) -> anyhow::Result<Self> {
        let url = validate_endpoint(endpoint)?;
        let loopback = match url.host() {
            Some(url::Host::Ipv4(ip)) => ip.is_loopback(),
            Some(url::Host::Ipv6(ip)) => ip.is_loopback(),
            Some(url::Host::Domain("localhost")) => true,
            _ => false,
        };
        if !loopback && (owner || instance.config.node.network != "tailscale") {
            anyhow::bail!(
                "Tailnet RPC requires an opted-in tailscale instance; owner credentials remain loopback-only"
            );
        }
        let config = instance.client_config(expected_fingerprint, bootstrap)?;
        let inner = reqwest::Client::builder()
            .use_preconfigured_tls(config)
            .no_proxy()
            .redirect(reqwest::redirect::Policy::none())
            .connect_timeout(Duration::from_secs(3))
            .timeout(Duration::from_secs(15))
            .pool_max_idle_per_host(0)
            .build()?;
        Ok(Self {
            inner,
            endpoint: url.origin().ascii_serialization(),
            owner_token: owner.then(|| instance.owner_token().to_owned()),
        })
    }
    pub async fn get<T: DeserializeOwned>(&self, path: &str) -> hmesh_core::Result<T> {
        self.send::<(), T>(reqwest::Method::GET, path, None, None)
            .await
    }
    pub async fn post<B: Serialize, T: DeserializeOwned>(
        &self,
        path: &str,
        body: &B,
    ) -> hmesh_core::Result<T> {
        self.send(reqwest::Method::POST, path, Some(body), None)
            .await
    }
    pub async fn post_with_timeout<B: Serialize, T: DeserializeOwned>(
        &self,
        path: &str,
        body: &B,
        seconds: u64,
    ) -> hmesh_core::Result<T> {
        if !(1..=40).contains(&seconds) {
            return Err(MeshError::input("RPC deadline must be 1..40 seconds"));
        }
        self.send(
            reqwest::Method::POST,
            path,
            Some(body),
            Some(Duration::from_secs(seconds)),
        )
        .await
    }
    async fn send<B: Serialize, T: DeserializeOwned>(
        &self,
        method: reqwest::Method,
        path: &str,
        body: Option<&B>,
        timeout: Option<Duration>,
    ) -> hmesh_core::Result<T> {
        if !path.starts_with('/') || path.starts_with("//") || path.contains(['\r', '\n']) {
            return Err(MeshError::input("invalid RPC path"));
        }
        let mut request = self
            .inner
            .request(method, format!("{}{path}", self.endpoint));
        if let Some(token) = &self.owner_token {
            request = request.bearer_auth(token);
        }
        if let Some(body) = body {
            request = request.json(body);
        }
        if let Some(timeout) = timeout {
            request = request.timeout(timeout);
        }
        let mut response = request.send().await.map_err(transport_error)?;
        let status = response.status();
        let mut bytes = Vec::new();
        while let Some(chunk) = response.chunk().await.map_err(transport_error)? {
            if bytes.len() + chunk.len() > 1048576 {
                return Err(MeshError::new(
                    ErrorCode::RateLimited,
                    "RPC response exceeds 1 MiB",
                ));
            }
            bytes.extend_from_slice(&chunk);
        }
        if !status.is_success() {
            if let Ok(value) = serde_json::from_slice::<serde_json::Value>(&bytes)
                && let Some(error) = value.get("error")
                && let Ok(error) = serde_json::from_value::<MeshError>(error.clone())
            {
                return Err(error);
            }
            return Err(MeshError::new(
                ErrorCode::PeerUnreachable,
                format!("peer returned HTTP {status}"),
            ));
        }
        serde_json::from_slice(&bytes).map_err(|_| {
            MeshError::new(
                ErrorCode::UnsupportedVersion,
                "peer response does not match the expected contract",
            )
        })
    }
}
fn transport_error(error: reqwest::Error) -> MeshError {
    let message = if error.is_timeout() {
        "RPC deadline exceeded"
    } else if error.is_connect() {
        "peer connection/TLS verification failed"
    } else {
        "RPC transport failed"
    };
    MeshError::new(ErrorCode::PeerUnreachable, message)
}
