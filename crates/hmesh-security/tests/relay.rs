//! Relay addresses and relay pairing codes as written and read, and a
//! relay's own identity on disk.
use hmesh_core::*;
use hmesh_security::{relay::RelayIdentity, validate_peer_address, validate_peer_addresses};

fn fp() -> String {
    "ab".repeat(32)
}
fn token() -> String {
    "cd".repeat(32)
}

#[test]
fn a_relay_address_reads_back_as_written() {
    for (written, authority) in [
        (
            format!("hmesh-relay://203.0.113.10:7443/{}/{}", fp(), token()),
            "203.0.113.10:7443",
        ),
        (
            format!("hmesh-relay://[2001:db8::7]:7443/{}/{}", fp(), token()),
            "[2001:db8::7]:7443",
        ),
        (
            format!("hmesh-relay://relay.example.com:7443/{}/{}", fp(), token()),
            "relay.example.com:7443",
        ),
        (
            format!("hmesh-relay://127.0.0.1:1/{}/{}", fp(), token()),
            "127.0.0.1:1",
        ),
    ] {
        let relay = RelayAddress::parse(&written).unwrap_or_else(|e| panic!("{written}: {e}"));
        assert_eq!(relay.to_string(), written);
        assert_eq!(relay.authority(), authority);
        assert_eq!(relay.label(), format!("hmesh-relay://{authority}"));
        assert_eq!(relay.fingerprint(), fp());
        assert_eq!(relay.token(), token());
        // Never the token in debug output, nor in what is shown.
        assert!(!format!("{relay:?}").contains(&token()));
        assert_eq!(shown_address(&written), relay.label());
        // A peer address too, kept in full.
        validate_peer_address(&written).unwrap_or_else(|e| panic!("{written}: {e}"));
    }
    // Copied with line breaks, colons in the fingerprint, upper case: the same.
    let messy = format!(
        "hmesh-relay://Relay.Example.com:7443/{}/\n{}",
        "AB:".repeat(31) + "AB",
        token().to_uppercase()
    );
    assert_eq!(
        RelayAddress::parse(&messy).unwrap().to_string(),
        format!("hmesh-relay://relay.example.com:7443/{}/{}", fp(), token())
    );
    assert_eq!(
        shown_address("https://studio.local:39880"),
        "https://studio.local:39880"
    );
}

#[test]
fn a_malformed_relay_address_is_refused() {
    for bad in [
        format!("hmesh-relay://203.0.113.10/{}/{}", fp(), token()),
        format!("hmesh-relay://203.0.113.10:0/{}/{}", fp(), token()),
        format!("hmesh-relay://203.0.113.10:7443/{}", fp()),
        format!("hmesh-relay://203.0.113.10:7443/{}/{}/extra", fp(), token()),
        format!("hmesh-relay://203.0.113.10:7443/{}/short", fp()),
        format!("hmesh-relay://0.0.0.0:7443/{}/{}", fp(), token()),
        format!("hmesh-relay://[::]:7443/{}/{}", fp(), token()),
        format!("hmesh-relay://224.0.0.1:7443/{}/{}", fp(), token()),
        format!("hmesh-relay://user@203.0.113.10:7443/{}/{}", fp(), token()),
        format!("hmesh-relay://bad_name:7443/{}/{}", fp(), token()),
        format!("hmesh://203.0.113.10:7443/{}/{}", fp(), token()),
    ] {
        assert!(RelayAddress::parse(&bad).is_err(), "{bad} read");
        assert!(validate_peer_address(&bad).is_err(), "{bad} accepted");
    }
    // Well formed, but at a link-local address: not a peer address.
    let metadata = format!("hmesh-relay://169.254.169.254:80/{}/{}", fp(), token());
    assert!(RelayAddress::parse(&metadata).is_ok());
    assert!(validate_peer_address(&metadata).is_err());
    let long = format!(
        "hmesh-relay://{}.example.com:7443/{}/{}",
        "a".repeat(63),
        fp(),
        token()
    );
    assert!(long.len() <= MAX_PEER_ADDRESS_BYTES);
    assert!(RelayAddress::parse(&long).is_ok());
    let too_long = format!(
        "hmesh-relay://{}.example.com:7443/{}/{}",
        ["a".repeat(63), "b".repeat(63), "c".repeat(30)].join("."),
        fp(),
        token()
    );
    assert!(RelayAddress::parse(&too_long).is_err());
}

#[test]
fn a_relay_pairing_code_names_the_relay_and_the_computer() {
    let relay = RelayAddress::parse(&format!(
        "hmesh-relay://203.0.113.10:7443/{}/{}",
        fp(),
        token()
    ))
    .unwrap();
    let computer = "ef".repeat(32);
    let code = relay_pair_code(&relay, &computer);
    assert_eq!(code, format!("{relay}/{computer}"));
    assert_eq!(
        parse_relay_pair_code(&code).unwrap(),
        (relay.clone(), computer)
    );
    // A relay address alone is not a pairing code, nor is one with more.
    assert!(parse_relay_pair_code(&relay.to_string()).is_err());
    assert!(parse_relay_pair_code(&format!("{code}/{}", fp())).is_err());
    assert!(parse_relay_pair_code(&format!("{relay}/nothex")).is_err());
}

/// Relay addresses in an address list stay whole; repeats go.
#[test]
fn an_address_list_keeps_relay_addresses_whole() {
    let relay = format!("hmesh-relay://203.0.113.10:7443/{}/{}", fp(), token());
    let upper = format!(
        "hmesh-relay://203.0.113.10:7443/{}/{}",
        fp().to_uppercase(),
        token()
    );
    assert_eq!(
        validate_peer_addresses(&["https://studio.local:39880/".into(), relay.clone(), upper,])
            .unwrap(),
        ["https://studio.local:39880".to_owned(), relay]
    );
}

/// A relay's key, certificate and token are made once, readable by its
/// owner only; a new token replaces the old one, and the code says it.
#[test]
fn a_relay_is_made_once_and_its_token_changes_on_request() {
    let root = tempfile::tempdir().unwrap();
    let dir = root.path().join("relay");
    let made = RelayIdentity::open(&dir).unwrap();
    let again = RelayIdentity::open(&dir).unwrap();
    assert_eq!(made.fingerprint, again.fingerprint);
    let first = made.token().unwrap();
    assert_eq!(first.len(), 64);
    let code = made.code("203.0.113.10:7443").unwrap();
    assert_eq!(code.fingerprint(), made.fingerprint);
    assert_eq!(code.token(), first);
    let rotated = made.rotate_token().unwrap();
    assert_ne!(rotated, first);
    assert_eq!(again.token().unwrap(), rotated);
    assert_eq!(made.code("203.0.113.10:7443").unwrap().token(), rotated);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode =
            |path: &std::path::Path| std::fs::metadata(path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode(&dir), 0o700);
        for entry in std::fs::read_dir(&dir).unwrap() {
            let path = entry.unwrap().path();
            assert_eq!(mode(&path), 0o600, "{}", path.display());
        }
    }
    // A directory with other things in it is not made into a relay.
    let busy = root.path().join("busy");
    std::fs::create_dir(&busy).unwrap();
    std::fs::write(busy.join("notes.txt"), "mine").unwrap();
    assert!(RelayIdentity::open(&busy).is_err());
    assert!(RelayIdentity::load(&root.path().join("none")).is_err());
}
