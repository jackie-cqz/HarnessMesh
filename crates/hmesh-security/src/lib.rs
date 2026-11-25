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
    sync::Arc,
};
use uuid::Uuid;

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
        if config.node.network == "tailscale" {
            names.push(config.node.listen_address.to_string());
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
        let endpoint = format!(
            "https://{}",
            std::net::SocketAddr::new(config.node.listen_address, config.node.listen_port)
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
        let mut submission = PairSubmission {
            request_id: Uuid::new_v4(),
            challenge,
            card: self.card.clone(),
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
            .with_custom_certificate_verifier(Arc::new(PinnedServer { expected, provider }));
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
pub fn validate_endpoint(endpoint: &str) -> Result<url::Url> {
    let url = url::Url::parse(endpoint)?;
    let permitted = match url.host() {
        Some(url::Host::Ipv4(ip)) => ip.is_loopback() || is_tailnet_ip(ip.into()),
        Some(url::Host::Ipv6(ip)) => ip.is_loopback() || is_tailnet_ip(ip.into()),
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
            "peer endpoints must be literal loopback/Tailnet HTTPS origins without credentials, paths or queries (localhost is also allowed)"
        );
    }
    Ok(url)
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
    expected: String,
    provider: Arc<CryptoProvider>,
}
impl ServerCertVerifier for PinnedServer {
    fn verify_server_cert(
        &self,
        cert: &CertificateDer<'_>,
        intermediates: &[CertificateDer<'_>],
        name: &ServerName<'_>,
        ocsp: &[u8],
        now: UnixTime,
    ) -> std::result::Result<ServerCertVerified, rustls::Error> {
        if fingerprint(cert.as_ref()) != self.expected {
            return Err(rustls::Error::General(
                "pinned certificate fingerprint mismatch".into(),
            ));
        }
        let mut roots = RootCertStore::empty();
        roots.add(cert.clone().into_owned()).map_err(|_| {
            rustls::Error::InvalidCertificate(rustls::CertificateError::BadEncoding)
        })?;
        // Pin comparison comes first. WebPKI still checks encoding, validity,
        // purpose and hostname; TLS CertificateVerify proves key possession.
        let verifier =
            WebPkiServerVerifier::builder_with_provider(Arc::new(roots), self.provider.clone())
                .build()
                .map_err(|e| rustls::Error::General(e.to_string()))?;
        verifier.verify_server_cert(cert, intermediates, name, ocsp, now)
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
