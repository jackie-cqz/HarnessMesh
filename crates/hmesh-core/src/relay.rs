//! Where a relay is, and how a computer is reached through one. A relay
//! (`hmesh relay serve`, on a server both computers can reach) passes bytes
//! between two computers that cannot reach each other, without seeing what
//! they say: the two still speak TLS with their pinned certificates inside.
//!
//! A relay address, `hmesh-relay://<host>:<port>/<relay fingerprint>/<token>`,
//! names the relay, the certificate it must show and the token that lets a
//! computer use it. A computer that holds a connection to a relay tells it
//! to the computers it is paired with, in its address list. A relay pairing
//! code adds the fingerprint of the computer to pair with:
//! `hmesh-relay://<host>:<port>/<relay fingerprint>/<token>/<fingerprint>`.
//! The token is a secret: only the relay's label (`hmesh-relay://<host>:<port>`)
//! is ever logged or shown.
use crate::{MAX_PEER_ADDRESS_BYTES, MeshError, Result, valid_host_name};
use std::net::IpAddr;

/// How a relay address begins.
pub const RELAY_SCHEME: &str = "hmesh-relay://";
/// Bytes in a relay's token (written as twice as many hex digits).
pub const RELAY_TOKEN_BYTES: usize = 32;
/// The most relays one computer holds connections to.
pub const MAX_RELAYS: usize = 4;

/// A relay: where it listens, the fingerprint of its certificate, and the
/// token it lets computers in with.
#[derive(Clone, PartialEq, Eq, Hash)]
pub struct RelayAddress {
    host: RelayHost,
    port: u16,
    fingerprint: String,
    token: String,
}
/// Where a relay listens: an IP address, or a DNS name resolved when dialed.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum RelayHost {
    Ip(IpAddr),
    Name(String),
}
impl RelayAddress {
    /// A relay address as written (`hmesh-relay://host:port/fingerprint/token`).
    /// Whitespace is ignored, so an address that wrapped when pasted still
    /// reads; fingerprint and token may be in either case.
    pub fn parse(text: &str) -> Result<Self> {
        let (relay, rest) = Self::split(text)?;
        if !rest.is_empty() {
            return Err(bad_address());
        }
        Ok(relay)
    }
    /// The relay address and the parts after it.
    fn split(text: &str) -> Result<(Self, Vec<String>)> {
        let text: String = text.split_whitespace().collect();
        if text.len() > MAX_PEER_ADDRESS_BYTES {
            return Err(MeshError::input(format!(
                "a relay address is at most {MAX_PEER_ADDRESS_BYTES} bytes"
            )));
        }
        let rest = text.strip_prefix(RELAY_SCHEME).ok_or_else(bad_address)?;
        let mut parts = rest.split('/');
        let authority = parts.next().unwrap_or_default();
        let (host, port) = authority_parts(authority).ok_or_else(bad_address)?;
        let fingerprint = parts.next().and_then(hex64).ok_or_else(bad_address)?;
        let token = parts.next().and_then(hex64).ok_or_else(bad_address)?;
        let rest = parts.map(str::to_owned).collect();
        Ok((
            Self {
                host,
                port,
                fingerprint,
                token,
            },
            rest,
        ))
    }
    pub fn host(&self) -> &RelayHost {
        &self.host
    }
    pub fn port(&self) -> u16 {
        self.port
    }
    /// The fingerprint of the certificate the relay must show.
    pub fn fingerprint(&self) -> &str {
        &self.fingerprint
    }
    /// The token the relay lets computers in with. A secret.
    pub fn token(&self) -> &str {
        &self.token
    }
    /// `host:port`, an IPv6 address in brackets.
    pub fn authority(&self) -> String {
        match &self.host {
            RelayHost::Ip(IpAddr::V6(ip)) => format!("[{ip}]:{}", self.port),
            RelayHost::Ip(ip) => format!("{ip}:{}", self.port),
            RelayHost::Name(name) => format!("{name}:{}", self.port),
        }
    }
    /// The relay as it may be logged or shown: where it is, without its
    /// fingerprint and token (`hmesh-relay://203.0.113.10:7443`).
    pub fn label(&self) -> String {
        format!("{RELAY_SCHEME}{}", self.authority())
    }
    /// The same relay with another token.
    pub fn with_token(&self, token: &str) -> Result<Self> {
        Ok(Self {
            token: hex64(token).ok_or_else(bad_address)?,
            ..self.clone()
        })
    }
}
/// The whole address, token included: for address lists and configuration,
/// never for logs (see `label`).
impl std::fmt::Display for RelayAddress {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{RELAY_SCHEME}{}/{}/{}",
            self.authority(),
            self.fingerprint,
            self.token
        )
    }
}
/// The token stays out of debug output, and so out of logs.
impl std::fmt::Debug for RelayAddress {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RelayAddress")
            .field("relay", &self.label())
            .field("token", &"hidden")
            .finish()
    }
}
/// The relay and the fingerprint of the computer to pair with, in a relay
/// pairing code (`hmesh-relay://host:port/relay fingerprint/token/fingerprint`).
pub fn parse_relay_pair_code(code: &str) -> Result<(RelayAddress, String)> {
    let (relay, rest) = RelayAddress::split(code)?;
    match rest.as_slice() {
        [fingerprint] => Ok((relay, hex64(fingerprint).ok_or_else(bad_code)?)),
        _ => Err(bad_code()),
    }
}
/// The code another computer pairs with this one through `relay`.
pub fn relay_pair_code(relay: &RelayAddress, fingerprint: &str) -> String {
    let fingerprint = hex64(fingerprint).unwrap_or_else(|| fingerprint.to_owned());
    format!("{relay}/{fingerprint}")
}
/// Whether `address` is a relay address (it may still be malformed).
pub fn is_relay_address(address: &str) -> bool {
    address.trim_start().starts_with(RELAY_SCHEME)
}
/// An address as it may be logged or shown: a relay's label instead of its
/// whole address, anything else as it is.
pub fn shown_address(address: &str) -> String {
    if is_relay_address(address) {
        RelayAddress::parse(address)
            .map(|relay| relay.label())
            .unwrap_or_else(|_| format!("{RELAY_SCHEME}…"))
    } else {
        address.to_owned()
    }
}
/// 64 hex digits (a SHA-256 fingerprint or a token), lowercase, colons
/// between pairs allowed.
fn hex64(text: &str) -> Option<String> {
    let value = text.replace(':', "").to_ascii_lowercase();
    (value.len() == 64 && value.bytes().all(|b| b.is_ascii_hexdigit())).then_some(value)
}
fn authority_parts(authority: &str) -> Option<(RelayHost, u16)> {
    let (host, port) = match authority.strip_prefix('[') {
        Some(inside) => {
            let (ip, tail) = inside.split_once(']')?;
            (
                RelayHost::Ip(IpAddr::V6(ip.parse().ok()?)),
                tail.strip_prefix(':')?,
            )
        }
        None => {
            let (host, port) = authority.rsplit_once(':')?;
            let host = host.to_ascii_lowercase();
            let host = match host.parse::<std::net::Ipv4Addr>() {
                Ok(ip) => RelayHost::Ip(IpAddr::V4(ip)),
                Err(_) if valid_host_name(&host) => RelayHost::Name(host),
                Err(_) => return None,
            };
            (host, port)
        }
    };
    let port: u16 = port.parse().ok().filter(|port| *port != 0)?;
    if let RelayHost::Ip(ip) = &host
        && (ip.is_unspecified() || ip.is_multicast())
    {
        return None;
    }
    Some((host, port))
}
fn bad_address() -> MeshError {
    MeshError::input(
        "a relay address looks like hmesh-relay://<address>:<port>/<relay fingerprint>/<token> (hmesh relay show prints it)",
    )
}
fn bad_code() -> MeshError {
    MeshError::input(
        "a relay pairing code looks like hmesh-relay://<address>:<port>/<relay fingerprint>/<token>/<fingerprint> (hmesh pair show prints it)",
    )
}
