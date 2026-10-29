use hmesh_core::*;
use hmesh_security::*;
use uuid::Uuid;

#[test]
fn initialization_is_idempotent_and_instances_have_distinct_keys_and_tokens() {
    let root = tempfile::tempdir().expect("tempdir");
    let a = Instance::init(&root.path().join("a"), NodeConfig::default()).expect("init a");
    let old_key = std::fs::read(a.dir.join("identity.pk8")).expect("key");
    let again = Instance::init(&a.dir, NodeConfig::default()).expect("init again");
    let b = Instance::init(&root.path().join("b"), NodeConfig::default()).expect("init b");
    assert_eq!(a.card.node_id, again.card.node_id);
    assert_eq!(
        old_key,
        std::fs::read(again.dir.join("identity.pk8")).expect("key")
    );
    assert_ne!(a.card.node_id, b.card.node_id);
    assert_ne!(a.card.fingerprint, b.card.fingerprint);
    assert_ne!(a.owner_token(), b.owner_token());
}

#[test]
fn signed_pairing_is_bound_to_target_challenge_and_card() {
    let root = tempfile::tempdir().expect("tempdir");
    let a = Instance::init(&root.path().join("a"), NodeConfig::default()).expect("init");
    let target = Uuid::new_v4();
    let challenge = PairChallenge {
        nonce: Uuid::new_v4(),
        node_id: target,
        expires_at: now() + 120,
    };
    let mut submission = a.pair_submission(challenge).expect("sign");
    verify_pair_submission(&submission, target).expect("valid signature");
    assert!(verify_pair_submission(&submission, Uuid::new_v4()).is_err());
    submission.card.display_name = "spoofed".into();
    assert!(verify_pair_submission(&submission, target).is_err());
}

#[test]
fn wrong_fingerprint_and_non_loopback_card_are_rejected() {
    let root = tempfile::tempdir().expect("tempdir");
    let a = Instance::init(&root.path().join("a"), NodeConfig::default()).expect("init");
    validate_card(&a.card).expect("card");
    let mut bad = a.card.clone();
    bad.fingerprint = "0".repeat(64);
    assert!(validate_card(&bad).is_err());
    bad = a.card;
    bad.endpoint = "https://169.254.169.254:80".into();
    assert!(validate_card(&bad).is_err());
}

#[test]
fn partial_identity_is_not_silently_replaced() {
    let root = tempfile::tempdir().expect("tempdir");
    let dir = root.path().join("a");
    std::fs::create_dir(&dir).expect("dir");
    std::fs::write(dir.join("identity.pk8"), b"existing key").expect("seed");
    assert!(Instance::init(&dir, NodeConfig::default()).is_err());
    assert_eq!(
        std::fs::read(dir.join("identity.pk8")).expect("retained"),
        b"existing key"
    );
}

#[test]
fn daemon_lock_prevents_same_directory_double_start() {
    let root = tempfile::tempdir().expect("tempdir");
    let a = Instance::init(&root.path().join("a"), NodeConfig::default()).expect("init");
    let lock = a.lock_daemon().expect("lock");
    assert!(a.lock_daemon().is_err());
    drop(lock);
    assert!(a.lock_daemon().is_ok());
}
