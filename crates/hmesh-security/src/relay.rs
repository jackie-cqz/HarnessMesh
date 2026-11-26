//! A relay's own identity (`hmesh relay serve`): its key and certificate,
//! whose fingerprint computers pin, and the token that lets a computer use
//! it. Kept in a directory of its own, readable by its owner only. The
//! token is a secret: it never goes into a log.
use anyhow::{Context, Result, bail};
use hmesh_core::{RELAY_TOKEN_BYTES, RelayAddress};
use rcgen::{CertificateParams, DnType, ExtendedKeyUsagePurpose, KeyPair, PKCS_ED25519};
use rustls::{
    DistinguishedName, ServerConfig, SignatureScheme,
    client::danger::HandshakeSignatureValid,
    crypto::CryptoProvider,
    pki_types::{CertificateDer, PrivateKeyDer, UnixTime},
    server::danger::{ClientCertVerified, ClientCertVerifier},
};
use serde::{Deserialize, Serialize};
use std::{
    fs,
    path::{Path, PathBuf},
    sync::Arc,
};

const KEY: &str = "relay.pk8";
const CERTIFICATE: &str = "relay-certificate.pem";
const TOKEN: &str = "relay.token";
const SETTINGS: &str = "relay.json";

/// A relay's key and certificate.
pub struct RelayIdentity {
    pub dir: PathBuf,
    certificate_pem: String,
    key_der: Vec<u8>,
    /// What computers pin: the SHA-256 of its certificate.
    pub fingerprint: String,
}
/// Where the relay listened last, and the address its code names, so that
/// `hmesh relay show` prints the same code.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RelaySettings {
    pub listen: std::net::SocketAddr,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub public_address: Option<String>,
}
impl RelayIdentity {
    /// The relay in `dir`, made the first time: a new key, a certificate
    /// and a token. A directory that exists without a relay in it must be
    /// empty: nothing else is changed.
    pub fn open(dir: &Path) -> Result<Self> {
        if dir.join(KEY).exists() {
            return Self::load(dir);
        }
        if dir.exists() {
            if fs::read_dir(dir)?.next().is_some() {
                bail!(
                    "a new relay needs an empty or new directory; {} has other files",
                    dir.display()
                );
            }
        } else {
            fs::create_dir_all(dir).context("create the relay directory")?;
        }
        crate::secure_directory(dir)?;
        let key = KeyPair::generate_for(&PKCS_ED25519)?;
        let mut params = CertificateParams::new(vec![
            "localhost".to_owned(),
            "127.0.0.1".to_owned(),
            "::1".to_owned(),
        ])?;
        params
            .distinguished_name
            .push(DnType::CommonName, "HarnessMesh relay");
        params.extended_key_usages = vec![ExtendedKeyUsagePurpose::ServerAuth];
        let certificate = params.self_signed(&key)?;
        crate::write_private_new(&dir.join(TOKEN), new_token()?.as_bytes())?;
        crate::write_private_new(&dir.join(CERTIFICATE), certificate.pem().as_bytes())?;
        // Written last: a relay is there once its key is.
        crate::write_private_new(&dir.join(KEY), &key.serialize_der())?;
        Self::load(dir)
    }
    /// The relay already in `dir`.
    pub fn load(dir: &Path) -> Result<Self> {
        let dir = fs::canonicalize(dir).context("no relay in that directory")?;
        for name in [KEY, CERTIFICATE, TOKEN] {
            let metadata = fs::symlink_metadata(dir.join(name))
                .with_context(|| format!("no relay in {} ({name} is missing)", dir.display()))?;
            if metadata.file_type().is_symlink() {
                bail!("relay files must not be symlinks");
            }
        }
        let certificate_pem = fs::read_to_string(dir.join(CERTIFICATE))?;
        let der = crate::certificate_der(&certificate_pem)?;
        let key_der = fs::read(dir.join(KEY))?;
        let signing = ring::signature::Ed25519KeyPair::from_pkcs8(&key_der)
            .map_err(|_| anyhow::anyhow!("invalid relay key"))?;
        let (_, parsed) = x509_parser::parse_x509_certificate(der.as_ref())
            .map_err(|_| anyhow::anyhow!("invalid relay certificate"))?;
        use ring::signature::KeyPair as _;
        if parsed.public_key().subject_public_key.data.as_ref() != signing.public_key().as_ref() {
            bail!("relay key does not match its certificate");
        }
        let identity = Self {
            fingerprint: crate::fingerprint(der.as_ref()),
            dir,
            certificate_pem,
            key_der,
        };
        identity.token()?;
        Ok(identity)
    }
    /// The file the token is kept in (`--rotate-token` replaces it; a
    /// running relay reads it again within seconds).
    pub fn token_path(&self) -> PathBuf {
        self.dir.join(TOKEN)
    }
    /// The token computers use this relay with now.
    pub fn token(&self) -> Result<String> {
        read_token(&self.token_path())
    }
    /// A new token: computers using the old one are cut off and need the
    /// new relay code.
    pub fn rotate_token(&self) -> Result<String> {
        let token = new_token()?;
        crate::atomic_write(&self.token_path(), token.as_bytes())?;
        Ok(token)
    }
    /// Where it listened last and the address its code names.
    pub fn settings(&self) -> Option<RelaySettings> {
        serde_json::from_slice(&fs::read(self.dir.join(SETTINGS)).ok()?).ok()
    }
    pub fn set_settings(&self, settings: &RelaySettings) -> Result<()> {
        crate::atomic_write(
            &self.dir.join(SETTINGS),
            &serde_json::to_vec_pretty(settings)?,
        )
    }
    /// The relay code computers use it with, naming it at `authority`
    /// (`host:port`).
    pub fn code(&self, authority: &str) -> Result<RelayAddress> {
        crate::validate_relay(&format!(
            "{}{authority}/{}/{}",
            hmesh_core::RELAY_SCHEME,
            self.fingerprint,
            self.token()?
        ))
    }
    /// TLS 1.3 with this relay's certificate. Every computer shows its own
    /// certificate, whatever it is: the relay knows it by its fingerprint,
    /// and TLS proves it holds that certificate's key.
    pub fn server_config(&self) -> Result<ServerConfig> {
        let provider = crate::provider();
        Ok(ServerConfig::builder_with_provider(provider.clone())
            .with_protocol_versions(&[&rustls::version::TLS13])?
            .with_client_cert_verifier(Arc::new(AnyComputer { provider }))
            .with_single_cert(
                vec![crate::certificate_der(&self.certificate_pem)?],
                PrivateKeyDer::Pkcs8(self.key_der.clone().into()),
            )?)
    }
}
/// A new token: random bytes as lowercase hex.
pub fn new_token() -> Result<String> {
    use ring::rand::SecureRandom;
    let mut bytes = [0u8; RELAY_TOKEN_BYTES];
    ring::rand::SystemRandom::new()
        .fill(&mut bytes)
        .map_err(|_| anyhow::anyhow!("no randomness for a relay token"))?;
    Ok(hex::encode(bytes))
}
fn read_token(path: &Path) -> Result<String> {
    let token = fs::read_to_string(path).context("read the relay token")?;
    let token = token.trim();
    if token.len() != RELAY_TOKEN_BYTES * 2
        || !token
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    {
        bail!("the relay token file does not hold a token");
    }
    Ok(token.to_owned())
}
/// Whether two secrets are the same, in time that does not depend on where
/// they differ.
pub fn same_secret(a: &str, b: &str) -> bool {
    use subtle::ConstantTimeEq;
    a.len() == b.len() && bool::from(a.as_bytes().ct_eq(b.as_bytes()))
}

/// Takes any computer's certificate: a relay pins none, it only tells
/// computers apart by their fingerprints. TLS still checks that the
/// computer holds the key of the certificate it shows.
#[derive(Debug)]
struct AnyComputer {
    provider: Arc<CryptoProvider>,
}
impl ClientCertVerifier for AnyComputer {
    fn offer_client_auth(&self) -> bool {
        true
    }
    fn client_auth_mandatory(&self) -> bool {
        true
    }
    fn root_hint_subjects(&self) -> &[DistinguishedName] {
        &[]
    }
    fn verify_client_cert(
        &self,
        end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _now: UnixTime,
    ) -> std::result::Result<ClientCertVerified, rustls::Error> {
        x509_parser::parse_x509_certificate(end_entity.as_ref()).map_err(|_| {
            rustls::Error::InvalidCertificate(rustls::CertificateError::BadEncoding)
        })?;
        Ok(ClientCertVerified::assertion())
    }
    fn verify_tls12_signature(
        &self,
        _message: &[u8],
        _cert: &CertificateDer<'_>,
        _signature: &rustls::DigitallySignedStruct,
    ) -> std::result::Result<HandshakeSignatureValid, rustls::Error> {
        Err(rustls::Error::PeerIncompatible(
            rustls::PeerIncompatible::Tls12NotOffered,
        ))
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
    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.provider
            .signature_verification_algorithms
            .supported_schemes()
    }
}
