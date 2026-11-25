//! Pairing with a six-digit passcode that only the other computer's screen
//! shows: a computer found on the LAN is chosen, its passcode typed in, and
//! both computers trust each other without another approval.
//!
//! SPAKE2 turns the passcode into a shared key without sending it, so a
//! computer pretending to be either side gets one guess per handshake and
//! nothing it could try offline. Both certificate fingerprints are SPAKE2
//! identities: the key, and the confirmations made with it, match only when
//! both sides used the same passcode and mean the same two certificates. A
//! computer relaying the handshake between them (answering on the LAN under
//! another computer's name) shows its own certificate to the asker, so the
//! confirmations do not match and nobody is trusted.
use crate::normalize_fingerprint;
use anyhow::{Result, anyhow, bail};
use base64::{Engine, engine::general_purpose::STANDARD};
use ring::{
    hmac,
    rand::{SecureRandom, SystemRandom},
};
use spake2::{Ed25519Group, Identity, Password, Spake2};
use uuid::Uuid;

/// How many tries a passcode allows before the pairing window closes.
pub const TRIES: u8 = 3;
const ASKER: &[u8] = b"hmesh passcode pairing v1: asker confirms";
const ANSWERER: &[u8] = b"hmesh passcode pairing v1: answerer confirms";
/// What the asker hears when the passcode, or a certificate, differs.
pub const MISMATCH: &str = "the passcode did not match: check the 6 digits shown on the other computer (each wrong try uses up one of its 3)";

/// A new passcode: six decimal digits, every one of the million equally likely.
pub fn new_passcode() -> Result<String> {
    let rng = SystemRandom::new();
    loop {
        let mut bytes = [0u8; 4];
        rng.fill(&mut bytes)
            .map_err(|_| anyhow!("the system random number generator failed"))?;
        // 4_294_000_000 is the largest multiple of a million below 2^32;
        // drawing again above it keeps every passcode equally likely.
        let value = u32::from_be_bytes(bytes);
        if value < 4_294_000_000 {
            return Ok(format!("{:06}", value % 1_000_000));
        }
    }
}
/// What a person typed: six digits, ignoring spaces and dashes.
pub fn normalize_passcode(input: &str) -> Result<String> {
    let digits: String = input
        .chars()
        .filter(|c| !c.is_whitespace() && *c != '-')
        .collect();
    if digits.len() != 6 || !digits.bytes().all(|b| b.is_ascii_digit()) {
        bail!("a passcode is the 6 digits shown on the other computer");
    }
    Ok(digits)
}
fn identities(asker: &str, answerer: &str) -> Result<(Identity, Identity)> {
    Ok((
        Identity::new(format!("hmesh asker {}", normalize_fingerprint(asker)?).as_bytes()),
        Identity::new(format!("hmesh answerer {}", normalize_fingerprint(answerer)?).as_bytes()),
    ))
}
fn confirmation_input(label: &[u8], request_id: Uuid) -> Vec<u8> {
    [label, request_id.as_bytes()].concat()
}
fn decode(value: &str) -> Result<Vec<u8>> {
    STANDARD
        .decode(value)
        .map_err(|_| anyhow!("malformed passcode handshake message"))
}

/// The asking computer's side, between sending its message and checking the
/// answer.
pub struct Asker {
    spake: Spake2<Ed25519Group>,
    request_id: Uuid,
}
impl Asker {
    /// Begin with the passcode the person typed; returns the message to send.
    /// `request_id` is the pair request's, so the confirmations belong to it.
    pub fn start(
        passcode: &str,
        asker_fingerprint: &str,
        answerer_fingerprint: &str,
        request_id: Uuid,
    ) -> Result<(Self, String)> {
        let passcode = normalize_passcode(passcode)?;
        let (asker, answerer) = identities(asker_fingerprint, answerer_fingerprint)?;
        let (spake, message) =
            Spake2::<Ed25519Group>::start_a(&Password::new(passcode), &asker, &answerer);
        Ok((Self { spake, request_id }, STANDARD.encode(message)))
    }
    /// Check the answer: when the other computer used the same passcode and
    /// the same two certificates, returns this side's confirmation to send.
    pub fn finish(self, message: &str, confirmation: &str) -> Result<String> {
        let key = self
            .spake
            .finish(&decode(message)?)
            .map_err(|_| anyhow!("malformed passcode handshake message"))?;
        let key = hmac::Key::new(hmac::HMAC_SHA256, &key);
        hmac::verify(
            &key,
            &confirmation_input(ANSWERER, self.request_id),
            &decode(confirmation)?,
        )
        .map_err(|_| anyhow!(MISMATCH))?;
        Ok(STANDARD.encode(hmac::sign(
            &key,
            &confirmation_input(ASKER, self.request_id),
        )))
    }
}

/// The answering computer's side: its reply to the asker's message, and the
/// confirmation it expects back.
pub struct Answer {
    pub message: String,
    pub confirmation: String,
    key: hmac::Key,
    request_id: Uuid,
}
impl Answer {
    /// Answer with this computer's own passcode. Nothing here tells whether
    /// the asker used the same one: only its confirmation does.
    pub fn new(
        passcode: &str,
        asker_fingerprint: &str,
        answerer_fingerprint: &str,
        request_id: Uuid,
        message: &str,
    ) -> Result<Self> {
        let (asker, answerer) = identities(asker_fingerprint, answerer_fingerprint)?;
        let (spake, reply) =
            Spake2::<Ed25519Group>::start_b(&Password::new(passcode), &asker, &answerer);
        let key = spake
            .finish(&decode(message)?)
            .map_err(|_| anyhow!("malformed passcode handshake message"))?;
        let key = hmac::Key::new(hmac::HMAC_SHA256, &key);
        Ok(Self {
            message: STANDARD.encode(reply),
            confirmation: STANDARD
                .encode(hmac::sign(&key, &confirmation_input(ANSWERER, request_id))),
            key,
            request_id,
        })
    }
    /// Whether the asker's confirmation shows the same passcode and the same
    /// two certificates. Compared in constant time.
    pub fn confirms(&self, confirmation: &str) -> bool {
        decode(confirmation).is_ok_and(|tag| {
            hmac::verify(&self.key, &confirmation_input(ASKER, self.request_id), &tag).is_ok()
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    const A: &str = "aa00000000000000000000000000000000000000000000000000000000000001";
    const B: &str = "bb00000000000000000000000000000000000000000000000000000000000002";
    const M: &str = "cc00000000000000000000000000000000000000000000000000000000000003";

    fn run(
        asker_code: &str,
        answerer_code: &str,
        asker_sees: &str,
        answerer_is: &str,
        asker_claims: &str,
    ) -> bool {
        let id = Uuid::new_v4();
        let (asker, message) = Asker::start(asker_code, A, asker_sees, id).unwrap();
        let answer = Answer::new(answerer_code, asker_claims, answerer_is, id, &message).unwrap();
        match asker.finish(&answer.message, &answer.confirmation) {
            Ok(confirmation) => answer.confirms(&confirmation),
            Err(_) => false,
        }
    }

    #[test]
    fn the_same_passcode_and_certificates_confirm_and_anything_else_does_not() {
        assert!(run("482913", "482913", B, B, A));
        // A wrong passcode.
        assert!(!run("482914", "482913", B, B, A));
        // The asker is talking to another certificate (a computer relaying
        // under the other one's name).
        assert!(!run("482913", "482913", M, B, A));
        // The asker's claimed certificate was swapped on the way.
        assert!(!run("482913", "482913", B, B, M));
    }

    #[test]
    fn confirmations_belong_to_one_request_and_one_direction() {
        let id = Uuid::new_v4();
        let (asker, message) = Asker::start("111111", A, B, id).unwrap();
        let answer = Answer::new("111111", A, B, id, &message).unwrap();
        // The answerer's own confirmation is not accepted back as the asker's.
        assert!(!answer.confirms(&answer.confirmation));
        let confirmation = asker.finish(&answer.message, &answer.confirmation).unwrap();
        // Replayed into a handshake for another request, it fails.
        let (_, message) = Asker::start("111111", A, B, Uuid::new_v4()).unwrap();
        let other = Answer::new("111111", A, B, Uuid::new_v4(), &message).unwrap();
        assert!(!other.confirms(&confirmation));
        assert!(answer.confirms(&confirmation));
        assert!(!answer.confirms("not base64"));
    }

    #[test]
    fn passcodes_are_six_digits() {
        for _ in 0..200 {
            let code = new_passcode().unwrap();
            assert_eq!(code.len(), 6);
            assert!(code.bytes().all(|b| b.is_ascii_digit()));
        }
        assert_eq!(normalize_passcode(" 482 913 ").unwrap(), "482913");
        assert_eq!(normalize_passcode("482-913").unwrap(), "482913");
        for bad in ["48291", "4829134", "48291a", "４８２９１３", ""] {
            assert!(normalize_passcode(bad).is_err(), "{bad}");
        }
        assert!(Asker::start("12345", A, B, Uuid::new_v4()).is_err());
    }
}
