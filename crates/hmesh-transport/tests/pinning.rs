//! Peers are identified by their pinned certificate, not by the address that
//! was dialed: a LAN address may change (DHCP) or differ by network, and the
//! certificate need not name it.
use hmesh_core::NodeConfig;
use hmesh_security::Instance;
use rustls::pki_types::ServerName;
use std::sync::Arc;
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream},
};

/// A TLS handshake from `client` to `server`, pinning `pin` and dialing the
/// server under the name `dialed`; then one short read. The client shows no
/// certificate (as when pairing): only how it checks the server is tested.
async fn handshake(
    server: &Instance,
    client: &Instance,
    pin: &str,
    dialed: &str,
) -> Result<(), String> {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let acceptor = tokio_rustls::TlsAcceptor::from(Arc::new(server.server_config(&[]).unwrap()));
    let accept = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        if let Ok(mut tls) = acceptor.accept(stream).await {
            let _ = tls.write_all(b"ok").await;
            let _ = tls.shutdown().await;
        }
    });
    let config = client.client_config(pin, true).map_err(|e| e.to_string())?;
    let connector = tokio_rustls::TlsConnector::from(Arc::new(config));
    let stream = TcpStream::connect(address).await.unwrap();
    let name = ServerName::try_from(dialed.to_owned()).unwrap();
    let result = match connector.connect(name, stream).await {
        Ok(mut tls) => {
            let mut answer = [0u8; 2];
            tls.read_exact(&mut answer)
                .await
                .map(|_| ())
                .map_err(|e| e.to_string())
        }
        Err(error) => Err(error.to_string()),
    };
    accept.abort();
    result
}

#[tokio::test]
async fn a_pinned_peer_is_reached_on_an_address_its_certificate_does_not_name() {
    let root = tempfile::tempdir().unwrap();
    let server = Instance::init(&root.path().join("studio"), NodeConfig::default()).unwrap();
    let client = Instance::init(&root.path().join("laptop"), NodeConfig::default()).unwrap();
    // The certificate names localhost and loopback only.
    for dialed in ["192.168.1.20", "10.147.17.5", "studio.example"] {
        handshake(&server, &client, &server.card.fingerprint, dialed)
            .await
            .unwrap_or_else(|e| panic!("pinned peer dialed as {dialed}: {e}"));
    }
    // Any other certificate answering there is refused.
    let other = Instance::init(&root.path().join("other"), NodeConfig::default()).unwrap();
    let refused = handshake(&server, &client, &other.card.fingerprint, "192.168.1.20")
        .await
        .unwrap_err();
    assert!(refused.contains("fingerprint mismatch"), "{refused}");
}
