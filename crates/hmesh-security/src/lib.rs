//! Instance identity, explicit certificate pins and signed one-time pairing.
//! No public CA fallback, certificate bypass or shared development key.
use anyhow::{Context, Result, bail};
use base64::{Engine, engine::general_purpose::STANDARD};
use hmesh_core::*;
use rcgen::{CertificateParams, DnType, ExtendedKeyUsagePurpose, KeyPair, PKCS_ED25519};
use ring::signature;
use rustls::{
    ClientConfig, RootCertStore, ServerConfig,
    client::{
        WebPkiServerVerifier,
        danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier},
    },
    crypto::CryptoProvider,
    pki_types::{CertificateDer, PrivateKeyDer, ServerName, UnixTime, pem::PemObject},
};
use serde::{Deserialize, Serialize};
use std::{
    fs::{self, File, OpenOptions},
    io::Write,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
};
use uuid::Uuid;
pub mod passcode;

pub struct Instance {
    pub dir: PathBuf,
    pub config: NodeConfig,
    pub card: NodeCard,
    key_der: Vec<u8>,
    token: String,
}
/// Bounded, streaming file integrity check; never returns file contents.
pub fn file_sha256(path: &Path, maximum: u64) -> Result<String> {
    use std::io::Read;
    let mut file = File::open(path)?;
    let mut digest = ring::digest::Context::new(&ring::digest::SHA256);
    let mut buffer = [0u8; 65536];
    let mut total = 0;
    loop {
        let count = file.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        total += count as u64;
        if total > maximum {
            bail!("file exceeds integrity-check budget");
        }
        digest.update(&buffer[..count]);
    }
    Ok(hex::encode(digest.finish().as_ref()))
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct IdentityMetadata {
    node_id: Uuid,
}

impl Instance {
    pub fn init(dir: &Path, config: NodeConfig) -> Result<Self> {
        config.validate()?;
        fs::create_dir_all(dir).context("create instance directory")?;
        reject_link(dir)?;
        if !dir.join("identity.json").exists()
            && fs::read_dir(dir)?
                .any(|entry| entry.map_or(true, |entry| entry.file_name() != "init.lock"))
        {
            bail!(
                "initialization requires an empty dedicated directory; refusing to change existing files or permissions"
            );
        }
        secure_directory(dir)?;
        let dir = fs::canonicalize(dir)?;
        let lock = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(dir.join("init.lock"))?;
        lock.lock().context("lock initialization")?;
        if dir.join("identity.json").exists() {
            return Self::load(&dir);
        }
        for name in [
            "identity.pk8",
            "certificate.pem",
            "owner.token",
            "node.toml",
            "policy.toml",
        ] {
            if dir.join(name).exists() {
                bail!(
                    "partial instance exists ({name}); refusing to overwrite keys or configuration"
                );
            }
        }
        let node_id = Uuid::new_v4();
        let key = KeyPair::generate_for(&PKCS_ED25519)?;
        let mut names = vec!["localhost".into(), "127.0.0.1".into(), "::1".into()];
        // Cosmetic since connections check the pinned fingerprint, not the
        // address (see PinnedServer), but it names where the node listens.
        if matches!(config.node.network.as_str(), "tailscale" | "direct")
            && !config.node.listen_address.is_loopback()
            && !config.node.listen_address.is_unspecified()
        {
            names.push(config.node.listen_address.to_string());
        }
        if config.node.network == "tailscale" {
            // Only connects out for now; the address it may listen on later.
            if let Some(address) = config.node.tailnet_address
                && address != config.node.listen_address
            {
                names.push(address.to_string());
            }
        }
        let mut params = CertificateParams::new(names)?;
        params
            .distinguished_name
            .push(DnType::CommonName, node_id.to_string());
        params.extended_key_usages = vec![
            ExtendedKeyUsagePurpose::ServerAuth,
            ExtendedKeyUsagePurpose::ClientAuth,
        ];
        let certificate = params.self_signed(&key)?;
        write_private_new(&dir.join("identity.pk8"), &key.serialize_der())?;
        write_private_new(&dir.join("certificate.pem"), certificate.pem().as_bytes())?;
        let token = format!("{}{}", Uuid::new_v4().simple(), Uuid::new_v4().simple());
        write_private_new(&dir.join("owner.token"), token.as_bytes())?;
        write_private_new(
            &dir.join("node.toml"),
            toml::to_string_pretty(&config)?.as_bytes(),
        )?;
        write_private_new(
            &dir.join("policy.toml"),
            toml::to_string_pretty(&Policy::default())?.as_bytes(),
        )?;
        // Commit marker is last. A crash before this point is reported, never re-keyed.
        write_private_new(
            &dir.join("identity.json"),
            &serde_json::to_vec_pretty(&IdentityMetadata { node_id })?,
        )?;
        Self::load(&dir)
    }
    pub fn load(dir: &Path) -> Result<Self> {
        reject_link(dir)?;
        let dir = fs::canonicalize(dir).context("instance not initialized")?;
        for name in [
            "identity.json",
            "identity.pk8",
            "certificate.pem",
            "owner.token",
            "node.toml",
            "policy.toml",
        ] {
            reject_link(&dir.join(name))?;
        }
        let metadata: IdentityMetadata =
            serde_json::from_slice(&fs::read(dir.join("identity.json"))?)?;
        let config: NodeConfig = toml::from_str(&fs::read_to_string(dir.join("node.toml"))?)?;
        config.validate()?;
        let certificate_pem = fs::read_to_string(dir.join("certificate.pem"))?;
        let cert = certificate_der(&certificate_pem)?;
        let key_der = fs::read(dir.join("identity.pk8"))?;
        let signing = signature::Ed25519KeyPair::from_pkcs8(&key_der)
            .map_err(|_| anyhow::anyhow!("invalid Ed25519 private key"))?;
        let (_, parsed) = x509_parser::parse_x509_certificate(cert.as_ref())
            .map_err(|_| anyhow::anyhow!("invalid certificate"))?;
        use signature::KeyPair as _;
        if parsed.public_key().subject_public_key.data.as_ref() != signing.public_key().as_ref() {
            bail!("identity private key does not match certificate");
        }
        let token = fs::read_to_string(dir.join("owner.token"))?;
        if token.len() != 64 || !token.bytes().all(|c| c.is_ascii_hexdigit()) {
            bail!("invalid local API credential");
        }
        // A listener on every interface has no one address of its own: its
        // card names one once the daemon sees which networks it is on, and
        // until then its owner's loopback.
        let listen = if config.node.listen_address.is_unspecified() {
            std::net::IpAddr::V4(std::net::Ipv4Addr::LOCALHOST)
        } else {
            config.node.listen_address
        };
        let endpoint = format!(
            "https://{}",
            std::net::SocketAddr::new(listen, config.node.listen_port)
        );
        let card = NodeCard {
            version: PROTOCOL.into(),
            node_id: metadata.node_id,
            display_name: config.node.name.clone(),
            fingerprint: fingerprint(cert.as_ref()),
            certificate_pem,
            endpoint,
        };
        Ok(Self {
            dir,
            config,
            card,
            key_der,
            token,
        })
    }
    pub fn owner_token(&self) -> &str {
        &self.token
    }
    pub fn verify_owner_token(&self, token: &str) -> bool {
        use subtle::ConstantTimeEq;
        self.token.as_bytes().ct_eq(token.as_bytes()).into()
    }
    pub fn lock_daemon(&self) -> Result<File> {
        let lock = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(self.dir.join("daemon.lock"))?;
        lock.try_lock()
            .context("another daemon already owns this data directory")?;
        Ok(lock)
    }
    pub fn pair_submission(&self, challenge: PairChallenge) -> Result<PairSubmission> {
        self.pair_submission_as(&self.card, challenge)
    }
    /// A pair request signed with this computer's key for `card`, its
    /// current card (a computer on every interface names the address it has
    /// now). The card must be this computer's own.
    pub fn pair_submission_as(
        &self,
        card: &NodeCard,
        challenge: PairChallenge,
    ) -> Result<PairSubmission> {
        if card.node_id != self.card.node_id || card.fingerprint != self.card.fingerprint {
            bail!("a pair request carries this computer's own card");
        }
        let mut submission = PairSubmission {
            request_id: Uuid::new_v4(),
            challenge,
            card: card.clone(),
            signature: String::new(),
        };
        let key = signature::Ed25519KeyPair::from_pkcs8(&self.key_der)
            .map_err(|_| anyhow::anyhow!("invalid signing key"))?;
        submission.signature = STANDARD.encode(key.sign(&pair_bytes(&submission)?).as_ref());
        Ok(submission)
    }
    pub fn server_config(&self, peers: &[Peer]) -> Result<ServerConfig> {
        let mut roots = RootCertStore::empty();
        roots.add(certificate_der(&self.card.certificate_pem)?)?;
        for peer in peers.iter().filter(|p| p.status == "trusted") {
            validate_card(&peer.card)?;
            roots.add(certificate_der(&peer.card.certificate_pem)?)?;
        }
        let provider = provider();
        // Anonymous TLS is limited to hello/challenge/pair-request routes. All data
        // routes additionally require a verified, currently pinned client cert.
        let verifier = rustls::server::WebPkiClientVerifier::builder_with_provider(
            Arc::new(roots),
            provider.clone(),
        )
        .allow_unauthenticated()
        .build()?;
        let mut config = ServerConfig::builder_with_provider(provider)
            .with_protocol_versions(&[&rustls::version::TLS13])?
            .with_client_cert_verifier(verifier)
            .with_single_cert(
                vec![certificate_der(&self.card.certificate_pem)?],
                PrivateKeyDer::Pkcs8(self.key_der.clone().into()),
            )?;
        config.alpn_protocols = vec![b"h2".to_vec(), b"http/1.1".to_vec()];
        Ok(config)
    }
    pub fn client_config(
        &self,
        expected_fingerprint: &str,
        bootstrap: bool,
    ) -> Result<ClientConfig> {
        let expected = normalize_fingerprint(expected_fingerprint)?;
        let provider = provider();
        let builder = ClientConfig::builder_with_provider(provider.clone())
            .with_protocol_versions(&[&rustls::version::TLS13])?
            .dangerous()
            .with_custom_certificate_verifier(Arc::new(PinnedServer {
                pin: Pin::Exact(expected),
                provider,
            }));
        let config = if bootstrap {
            builder.with_no_client_auth()
        } else {
            builder.with_client_auth_cert(
                vec![certificate_der(&self.card.certificate_pem)?],
                PrivateKeyDer::Pkcs8(self.key_der.clone().into()),
            )?
        };
        Ok(config)
    }
    /// First contact with a computer whose certificate is not known yet
    /// (passcode pairing): any certificate passing the usual WebPKI checks is
    /// accepted, and its fingerprint written to `seen`, for the caller to pin
    /// from then on. Trust comes from the passcode, never from this.
    pub fn first_contact_config(&self, seen: Arc<Mutex<Option<String>>>) -> Result<ClientConfig> {
        let provider = provider();
        Ok(ClientConfig::builder_with_provider(provider.clone())
            .with_protocol_versions(&[&rustls::version::TLS13])?
            .dangerous()
            .with_custom_certificate_verifier(Arc::new(PinnedServer {
                pin: Pin::Learn(seen),
                provider,
            }))
            .with_no_client_auth())
    }
}

pub fn certificate_der(pem: &str) -> Result<CertificateDer<'static>> {
    CertificateDer::from_pem_slice(pem.as_bytes()).context("invalid certificate PEM")
}
pub fn fingerprint(der: &[u8]) -> String {
    sha256_hex(der)
}
pub fn sha256_hex(bytes: &[u8]) -> String {
    hex::encode(ring::digest::digest(&ring::digest::SHA256, bytes).as_ref())
}
pub fn normalize_fingerprint(value: &str) -> Result<String> {
    let value = value.replace(':', "").to_ascii_lowercase();
    if value.len() != 64 || !value.bytes().all(|b| b.is_ascii_hexdigit()) {
        bail!("expected a SHA-256 certificate fingerprint");
    }
    Ok(value)
}
/// One line a person copies to another computer to pair with this one: the
/// node's HTTPS address and its certificate fingerprint,
/// `hmesh://100.64.0.1:7443/<64 hex digits>`.
pub fn pair_code(card: &NodeCard) -> String {
    let authority = card
        .endpoint
        .strip_prefix("https://")
        .unwrap_or(&card.endpoint)
        .trim_end_matches('/');
    let fingerprint =
        normalize_fingerprint(&card.fingerprint).unwrap_or_else(|_| card.fingerprint.clone());
    format!("hmesh://{authority}/{fingerprint}")
}
/// The endpoint and fingerprint in a pairing code. Whitespace is ignored, so
/// a code that wrapped across lines when pasted still reads.
pub fn parse_pair_code(code: &str) -> Result<(String, String)> {
    let code: String = code.split_whitespace().collect();
    let Some((authority, fingerprint)) = code
        .strip_prefix("hmesh://")
        .and_then(|rest| rest.split_once('/'))
    else {
        bail!("a pairing code looks like hmesh://<address>:<port>/<fingerprint>");
    };
    let endpoint = format!("https://{authority}");
    validate_endpoint(&endpoint)?;
    Ok((endpoint, normalize_fingerprint(fingerprint)?))
}
/// Whether an endpoint names this computer's own loopback address.
pub fn endpoint_is_loopback(endpoint: &str) -> bool {
    url::Url::parse(endpoint).is_ok_and(|url| match url.host() {
        Some(url::Host::Ipv4(ip)) => ip.is_loopback(),
        Some(url::Host::Ipv6(ip)) => ip.is_loopback(),
        Some(url::Host::Domain(name)) => name == "localhost",
        None => false,
    })
}
/// Whether a computer configured as `config` can call a peer whose card
/// names `endpoint`. A node that other computers reach on a real address
/// (Tailscale or direct) reads a loopback endpoint as a peer set up with only
/// MCP: it accepts no connections, so there is nothing to call. Nodes on
/// loopback themselves (one computer, tests, only-MCP nodes) call loopback
/// peers as usual.
pub fn can_call(config: &NodeConfig, endpoint: &str) -> bool {
    !(config.listens_for_peers() && endpoint_is_loopback(endpoint))
}
/// Where a peer may be: any literal unicast address a computer can have on
/// loopback, a Tailnet, a LAN, another overlay network or the internet. Not
/// link-local addresses (169.254.0.0/16 is where cloud metadata services
/// answer, fe80::/10 needs a zone), and not unspecified, multicast,
/// broadcast or reserved ones. No names are resolved, and who answers there
/// is settled by the pinned certificate fingerprint, not by the address.
pub fn peer_address(ip: std::net::IpAddr) -> bool {
    match ip.to_canonical() {
        std::net::IpAddr::V4(ip) => {
            !ip.is_unspecified()
                && !ip.is_multicast()
                && !ip.is_broadcast()
                && !ip.is_link_local()
                && ip.octets()[0] != 0
                && ip.octets()[0] < 240
        }
        std::net::IpAddr::V6(ip) => {
            !ip.is_unspecified() && !ip.is_multicast() && !ip.is_unicast_link_local()
        }
    }
}
pub fn validate_endpoint(endpoint: &str) -> Result<url::Url> {
    let url = url::Url::parse(endpoint)?;
    let permitted = match url.host() {
        Some(url::Host::Ipv4(ip)) => peer_address(ip.into()),
        Some(url::Host::Ipv6(ip)) => peer_address(ip.into()),
        Some(url::Host::Domain("localhost")) => true,
        _ => false,
    };
    if url.scheme() != "https"
        || !permitted
        || !url.username().is_empty()
        || url.password().is_some()
        || url.port_or_known_default().is_none_or(|p| p == 0)
        || url.query().is_some()
        || url.fragment().is_some()
        || url.path() != "/"
    {
        bail!(
            "peer endpoints must be HTTPS origins on a literal unicast IP address (or localhost), without credentials, paths or queries"
        );
    }
    Ok(url)
}
/// One address in a paired computer's address list, or one its owner set:
/// the rules of `validate_endpoint`, and DNS host names too (`studio.local`,
/// a name on a company network, `localhost`). A name is resolved when it is
/// dialed, and only the addresses `peer_address` permits are used; who
/// answers is still settled by the pinned certificate. Cards and pairing
/// codes keep literal IP addresses (`validate_endpoint`).
pub fn validate_peer_address(address: &str) -> Result<url::Url> {
    if address.len() > MAX_PEER_ADDRESS_BYTES {
        bail!("a peer address is at most {MAX_PEER_ADDRESS_BYTES} bytes");
    }
    let url = url::Url::parse(address)?;
    let permitted = match url.host() {
        Some(url::Host::Ipv4(ip)) => peer_address(ip.into()),
        Some(url::Host::Ipv6(ip)) => peer_address(ip.into()),
        Some(url::Host::Domain(name)) => valid_host_name(name),
        None => false,
    };
    if url.scheme() != "https"
        || !permitted
        || !url.username().is_empty()
        || url.password().is_some()
        || url.port_or_known_default().is_none_or(|p| p == 0)
        || url.query().is_some()
        || url.fragment().is_some()
        || url.path() != "/"
    {
        bail!(
            "peer addresses must be HTTPS origins on a unicast IP address or a DNS host name, without credentials, paths or queries"
        );
    }
    Ok(url)
}
/// An address a person typed to pair with a computer: an IP address or a
/// host name, with or without `https://` and a port (`default_port` when it
/// has none), checked like any peer address (`validate_peer_address`). A
/// bare IPv6 address may leave out its brackets when it has no port.
pub fn typed_peer_address(typed: &str, default_port: u16) -> Result<url::Url> {
    let typed = typed.trim();
    let rest = typed.strip_prefix("https://").unwrap_or(typed);
    let rest = rest.strip_suffix('/').unwrap_or(rest);
    if rest.is_empty()
        || rest.contains(['/', '?', '#', '@', '\\'])
        || rest.contains(char::is_whitespace)
    {
        bail!("write an address as an IP address or a host name, with a port if it has one");
    }
    let (authority, has_port) = match rest.strip_prefix('[') {
        Some(inside) => (
            rest.to_owned(),
            inside
                .split_once(']')
                .is_some_and(|(_, tail)| !tail.is_empty()),
        ),
        // More than one colon, no brackets: an IPv6 address without a port.
        None if rest.matches(':').count() > 1 => (format!("[{rest}]"), false),
        None => (rest.to_owned(), rest.contains(':')),
    };
    let address = if has_port {
        format!("https://{authority}")
    } else {
        format!("https://{authority}:{default_port}")
    };
    validate_peer_address(&address)
}
/// A computer's address list, checked: its count and each address, written
/// as the origin it names (`https://studio.local:38880`), without repeats.
pub fn validate_peer_addresses(addresses: &[String]) -> Result<Vec<String>> {
    if addresses.len() > MAX_PEER_ADDRESSES {
        bail!("an address list has at most {MAX_PEER_ADDRESSES} addresses");
    }
    let mut origins: Vec<String> = Vec::new();
    for address in addresses {
        let origin = validate_peer_address(address)?
            .origin()
            .ascii_serialization();
        if !origins.contains(&origin) {
            origins.push(origin);
        }
    }
    Ok(origins)
}
pub fn validate_card(card: &NodeCard) -> Result<()> {
    if card.version != PROTOCOL || card.node_id.is_nil() {
        bail!("invalid node identity/protocol");
    }
    validate_name(&card.display_name)?;
    validate_endpoint(&card.endpoint)?;
    if fingerprint(certificate_der(&card.certificate_pem)?.as_ref())
        != normalize_fingerprint(&card.fingerprint)?
    {
        bail!("certificate fingerprint does not match node card");
    }
    Ok(())
}
fn pair_bytes(submission: &PairSubmission) -> Result<Vec<u8>> {
    Ok(serde_json::to_vec(&(
        "hmesh/1:pair",
        submission.request_id,
        &submission.challenge,
        &submission.card,
    ))?)
}
pub fn verify_pair_submission(submission: &PairSubmission, target: Uuid) -> Result<()> {
    validate_card(&submission.card)?;
    if submission.challenge.node_id != target
        || submission.challenge.expires_at < now()
        || submission.challenge.expires_at > now() + 180
    {
        bail!("pairing challenge target or expiry mismatch");
    }
    let cert = certificate_der(&submission.card.certificate_pem)?;
    let (_, parsed) = x509_parser::parse_x509_certificate(cert.as_ref())
        .map_err(|_| anyhow::anyhow!("invalid certificate"))?;
    if parsed.public_key().algorithm.algorithm.to_id_string() != "1.3.101.112" {
        bail!("Ed25519 node identity required");
    }
    let sig = STANDARD
        .decode(&submission.signature)
        .context("invalid pairing signature encoding")?;
    signature::UnparsedPublicKey::new(
        &signature::ED25519,
        parsed.public_key().subject_public_key.data.as_ref(),
    )
    .verify(&pair_bytes(submission)?, &sig)
    .map_err(|_| anyhow::anyhow!("pairing proof of key possession failed"))?;
    Ok(())
}
pub fn provider() -> Arc<CryptoProvider> {
    Arc::new(rustls::crypto::ring::default_provider())
}

#[derive(Debug)]
struct PinnedServer {
    pin: Pin,
    provider: Arc<CryptoProvider>,
}
#[derive(Debug)]
enum Pin {
    /// Exactly this certificate fingerprint.
    Exact(String),
    /// Whichever certificate answers, recorded here (first contact).
    Learn(Arc<Mutex<Option<String>>>),
}
impl ServerCertVerifier for PinnedServer {
    fn verify_server_cert(
        &self,
        cert: &CertificateDer<'_>,
        intermediates: &[CertificateDer<'_>],
        _dialed: &ServerName<'_>,
        ocsp: &[u8],
        now: UnixTime,
    ) -> std::result::Result<ServerCertVerified, rustls::Error> {
        let seen = fingerprint(cert.as_ref());
        if let Pin::Exact(expected) = &self.pin
            && seen != *expected
        {
            return Err(rustls::Error::General(
                "pinned certificate fingerprint mismatch".into(),
            ));
        }
        let mut roots = RootCertStore::empty();
        roots.add(cert.clone().into_owned()).map_err(|_| {
            rustls::Error::InvalidCertificate(rustls::CertificateError::BadEncoding)
        })?;
        // Pin comparison comes first and is what identifies the peer: exactly
        // this certificate, whose key TLS CertificateVerify proves the peer
        // holds. WebPKI still checks encoding, validity and purpose. The
        // address that was dialed is not checked against the certificate:
        // with the certificate pinned it adds nothing, and a LAN address may
        // change (DHCP) or differ by network. Every HarnessMesh certificate
        // names localhost, so that is the name WebPKI is asked about.
        let verifier =
            WebPkiServerVerifier::builder_with_provider(Arc::new(roots), self.provider.clone())
                .build()
                .map_err(|e| rustls::Error::General(e.to_string()))?;
        let pinned =
            ServerName::try_from("localhost").map_err(|e| rustls::Error::General(e.to_string()))?;
        let verified = verifier.verify_server_cert(cert, intermediates, &pinned, ocsp, now)?;
        if let Pin::Learn(slot) = &self.pin {
            *slot
                .lock()
                .map_err(|_| rustls::Error::General("certificate record poisoned".into()))? =
                Some(seen);
        }
        Ok(verified)
    }
    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        signature: &rustls::DigitallySignedStruct,
    ) -> std::result::Result<HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls12_signature(
            message,
            cert,
            signature,
            &self.provider.signature_verification_algorithms,
        )
    }
    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        signature: &rustls::DigitallySignedStruct,
    ) -> std::result::Result<HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls13_signature(
            message,
            cert,
            signature,
            &self.provider.signature_verification_algorithms,
        )
    }
    fn supported_verify_schemes(&self) -> Vec<rustls::SignatureScheme> {
        self.provider
            .signature_verification_algorithms
            .supported_schemes()
    }
}

pub fn write_private_new(path: &Path, contents: &[u8]) -> Result<()> {
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(path)?;
    file.write_all(contents)?;
    file.sync_all()?;
    Ok(())
}
pub fn atomic_write(path: &Path, contents: &[u8]) -> Result<()> {
    let parent = path.parent().context("file has no parent")?;
    let temp = parent.join(format!(".write-{}", Uuid::new_v4()));
    write_private_new(&temp, contents)?;
    if let Err(error) = fs::rename(&temp, path) {
        let _ = fs::remove_file(&temp);
        return Err(error.into());
    }
    Ok(())
}
fn reject_link(path: &Path) -> Result<()> {
    let metadata = fs::symlink_metadata(path)?;
    if metadata.file_type().is_symlink() {
        bail!("instance files must not be symlinks");
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        if metadata.file_attributes() & 0x400 != 0 {
            bail!("instance files must not be reparse points");
        }
    }
    Ok(())
}
pub fn secure_directory(path: &Path) -> Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o700))?;
    }
    #[cfg(windows)]
    {
        use std::{os::windows::process::CommandExt, process::Command};
        let system =
            PathBuf::from(std::env::var_os("SystemRoot").context("SystemRoot unavailable")?)
                .join("System32");
        let user = Command::new(system.join("whoami.exe"))
            .args(["/user", "/fo", "csv", "/nh"])
            .creation_flags(0x08000000)
            .output()?;
        if !user.status.success() {
            bail!("cannot determine owner SID");
        }
        let output = String::from_utf8(user.stdout)?;
        let sid = output
            .trim()
            .rsplit(',')
            .next()
            .context("missing owner SID")?
            .trim_matches('"');
        if !sid.starts_with("S-1-")
            || !sid
                .bytes()
                .all(|b| b.is_ascii_digit() || matches!(b, b'S' | b'-'))
        {
            bail!("invalid owner SID");
        }
        let grant = format!("*{sid}:(OI)(CI)F");
        let result = Command::new(system.join("icacls.exe"))
            .arg(path)
            .args(["/inheritance:r", "/grant:r", &grant, "/q"])
            .creation_flags(0x08000000)
            .output()?;
        if !result.status.success() {
            bail!("cannot restrict instance directory to its owner");
        }
    }
    Ok(())
}
