//! Paired computers find each other again on the local network. Each
//! computer has a random find key, told only to the computers paired with
//! it, with its address list (over connections that show certificates).
//! While it takes connections on a trusted LAN it announces
//! `_hmesh-find._tcp` there (see `nearby`) under a mark: the first 16 hex
//! digits of HMAC-SHA256(key, the current ten minutes), a new one every ten
//! minutes, with no name and no fingerprint. A paired computer none of whose
//! addresses answers works out the marks for this and the previous ten
//! minutes, listens for a moment where it may look (see `nearby::find_on`:
//! its own trusted LANs, or the interface of its default route when it only
//! connects out), and dials what it heard, where the pinned
//! certificate still decides who answered. Anyone else hears random-looking
//! names that change every ten minutes, but at an address and port that
//! stay the same, where this computer's named `_hmesh._tcp` announcement
//! also is while its pairing window is open: the mark keeps the key from
//! others, not which computer this is. Revoking a pairing gives this
//! computer a new key, so the computer revoked cannot tell its mark again.
use super::*;
use hmesh_transport::Finder;
use std::{future::Future, pin::Pin};

pub const SERVICE: &str = "_hmesh-find._tcp.local.";
/// A mark lasts this long.
pub const WINDOW_SECONDS: i64 = 600;
/// Where the key is kept (`node_secrets`).
const SECRET: &str = "find_key";

/// The ten minutes `unix_seconds` falls in.
pub fn window(unix_seconds: i64) -> u64 {
    (unix_seconds.max(0) / WINDOW_SECONDS) as u64
}
/// The mark `key` makes in `window`: HMAC-SHA256 over the window as eight
/// big-endian bytes, its first 16 hex digits.
pub fn token(key: &[u8], window: u64) -> String {
    let key = ring::hmac::Key::new(ring::hmac::HMAC_SHA256, key);
    let tag = ring::hmac::sign(&key, &window.to_be_bytes());
    hex::encode(&tag.as_ref()[..8])
}
/// Whether `name` is the mark `key` makes now or made in the previous ten
/// minutes (a mark heard just before it changed, or a clock a little ahead).
pub fn matches(key: &[u8], name: &str, unix_seconds: i64) -> bool {
    let now = window(unix_seconds);
    [now, now.saturating_sub(1)]
        .into_iter()
        .any(|window| token(key, window) == name)
}
fn new_key() -> anyhow::Result<[u8; FIND_KEY_BYTES]> {
    let mut key = [0u8; FIND_KEY_BYTES];
    ring::rand::SecureRandom::fill(&ring::rand::SystemRandom::new(), &mut key)
        .map_err(|_| anyhow::anyhow!("the system random number generator failed"))?;
    Ok(key)
}
/// This computer's find key: the one kept in `store`, or a new one kept there.
pub(crate) fn load_key(store: &Store) -> anyhow::Result<[u8; FIND_KEY_BYTES]> {
    if let Some(kept) = store.node_secret(SECRET)?
        && let Ok(key) = <[u8; FIND_KEY_BYTES]>::try_from(kept.as_slice())
    {
        return Ok(key);
    }
    let key = new_key()?;
    store.set_node_secret(SECRET, &key)?;
    Ok(key)
}

impl AppState {
    /// Whether this computer announces its mark: on a direct network, when
    /// it takes connections (where, `nearby` decides: its trusted LANs).
    fn announces_mark(&self) -> bool {
        self.offers_passcode()
    }
    /// The find key as paired computers are told it, if they are.
    pub(crate) fn find_key_told(&self) -> Option<String> {
        if !self.announces_mark() {
            return None;
        }
        self.find_key.read().ok().map(|key| hex::encode(*key))
    }
    /// The mark this computer announces now, if it announces one.
    pub(crate) fn find_mark(&self) -> Option<String> {
        if !self.announces_mark() {
            return None;
        }
        let key = self.find_key.read().ok()?;
        Some(token(key.as_slice(), window(now())))
    }
    /// A new find key, after a pairing was revoked: the computer revoked
    /// knows only the old one. The paired computers left are told the new
    /// one (a newer address list) at once, and the mark changes now.
    pub(crate) fn rotate_find_key(&self) -> hmesh_core::Result<()> {
        let key =
            new_key().map_err(|error| MeshError::new(ErrorCode::Internal, error.to_string()))?;
        self.store.set_node_secret(SECRET, &key)?;
        {
            // Changed while the list is held, so the list never goes out
            // with a key that is no longer this computer's.
            let mut list = self
                .addresses
                .write()
                .map_err(|_| MeshError::new(ErrorCode::Internal, "address list poisoned"))?;
            *self
                .find_key
                .write()
                .map_err(|_| MeshError::new(ErrorCode::Internal, "find key poisoned"))? = key;
            list.find_key = self.find_key_told();
            let millis = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|elapsed| elapsed.as_millis() as i64)
                .unwrap_or_default();
            list.revision = millis.max(list.revision + 1);
        }
        self.peers_changed();
        self.nearby.changed.notify_one();
        Ok(())
    }
    /// Where else `peer` might be, when none of its addresses answers: heard
    /// on the local network by its mark. Only on a direct network, and only
    /// for a computer that told its key.
    pub(crate) fn finder(&self, peer: &Peer) -> Option<Arc<dyn Finder>> {
        if self.instance.config.node.network != "direct" {
            return None;
        }
        let key = self.store.peer_find_key(peer.card.node_id).ok()??;
        let key = hex::decode(key).ok()?;
        Some(Arc::new(Finding {
            state: self.clone(),
            node_id: peer.card.node_id,
            fingerprint: peer.card.fingerprint.clone(),
            key,
        }))
    }
}

/// Looks for one paired computer by its mark.
struct Finding {
    state: AppState,
    node_id: Uuid,
    fingerprint: String,
    key: Vec<u8>,
}
impl Finder for Finding {
    fn find(&self) -> Pin<Box<dyn Future<Output = Vec<String>> + Send + 'static>> {
        let state = self.state.clone();
        let fingerprint = self.fingerprint.clone();
        let key = self.key.clone();
        Box::pin(async move {
            // Listened for only where this computer may look (`find_on`).
            let looking = state.clone();
            let on = async move { crate::nearby::find_on(&looking).await };
            state
                .nearby
                .clone()
                .find_peer(fingerprint, key, on)
                .await
                .into_iter()
                .map(|address| format!("https://{address}"))
                .filter(|address| {
                    hmesh_security::can_call(&state.instance.config, address)
                        || state.networks.names_own_network(address)
                })
                .collect()
        })
    }
    fn found(&self, origin: &str) {
        tracing::info!(peer = %self.node_id, endpoint = %origin, "found a paired computer again on the local network");
        if let Err(error) = self.state.store.add_found_address(self.node_id, origin) {
            tracing::warn!(code = ?error.code, "where a paired computer was found was not kept");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The mark is the HMAC of its ten minutes, the same for those ten
    /// minutes and another after them; one key's marks are not another's.
    #[test]
    fn a_mark_is_the_hmac_of_its_ten_minutes() {
        let key = [7u8; FIND_KEY_BYTES];
        let at = 1_700_000_000;
        assert_eq!(window(at), 2_833_333);
        assert_eq!(window(at + 399), 2_833_333);
        assert_eq!(window(at + 400), 2_833_334);
        // Worked out apart from this code (Python's hmac).
        assert_eq!(token(&key, 2_833_333), "edd72b012ae68d96");
        assert_eq!(token(&key, 2_833_332), "c63836e78ad7bbaf");
        assert_eq!(token(&key, 2_833_334), "40b25eb1527b4f70");
        assert!(matches(&key, "edd72b012ae68d96", at));
        // Heard just before it changed: still this computer's.
        assert!(matches(&key, "c63836e78ad7bbaf", at));
        // Two windows back, or not yet: not.
        assert!(!matches(&key, &token(&key, 2_833_331), at));
        assert!(!matches(&key, "40b25eb1527b4f70", at));
        let other = [8u8; FIND_KEY_BYTES];
        assert!(!matches(&other, "edd72b012ae68d96", at));
        assert_ne!(token(&other, 2_833_333), token(&key, 2_833_333));
        assert!(
            token(&other, 2_833_333)
                .bytes()
                .all(|b| b.is_ascii_hexdigit())
        );
        assert_eq!(token(&other, 2_833_333).len(), 16);
    }

    fn state(root: &std::path::Path, name: &str, config: NodeConfig) -> AppState {
        let instance = Arc::new(Instance::init(&root.join(name), config).unwrap());
        let store = Store::open(&instance.dir.join("state.sqlite3")).unwrap();
        let (shutdown, _) = watch::channel(false);
        AppState::load(instance, store, shutdown).unwrap()
    }
    fn direct() -> NodeConfig {
        let mut config = NodeConfig::default();
        config.node.network = "direct".into();
        config.node.expose_to_tailnet = true;
        config.node.listen_address = "127.0.0.1".parse().unwrap();
        config.node.host_names = Some(vec![]);
        config
    }

    /// A computer keeps one key across restarts and tells it with its
    /// address list; revoking a pairing gives it a new key, a newer list
    /// with it, and a new mark the old key does not make. One that only
    /// connects out announces nothing and tells no key.
    #[test]
    fn revoking_a_pairing_gives_this_computer_a_new_find_key() {
        let root = tempfile::tempdir().unwrap();
        let a = state(root.path(), "desk", direct());
        let first = a.find_key_told().expect("a key told");
        assert!(valid_find_key(&first));
        assert_eq!(a.addresses().find_key.as_deref(), Some(first.as_str()));
        let mark = a.find_mark().expect("a mark");
        let old = hex::decode(&first).unwrap();
        assert_eq!(mark, token(&old, window(now())));
        // Restarted: the same key.
        let again = AppState::load(
            a.instance.clone(),
            Store::open(&a.instance.dir.join("state.sqlite3")).unwrap(),
            watch::channel(false).0,
        )
        .unwrap();
        assert_eq!(again.find_key_told().as_deref(), Some(first.as_str()));

        let before = a.addresses().revision;
        a.rotate_find_key().unwrap();
        let second = a.find_key_told().expect("a key told");
        assert_ne!(second, first);
        let list = a.addresses();
        assert_eq!(list.find_key.as_deref(), Some(second.as_str()));
        assert!(list.revision > before);
        // The old key makes neither the mark now nor the one before it.
        let mark = a.find_mark().expect("a mark");
        assert!(!matches(&old, &mark, now()));
        assert!(matches(&hex::decode(&second).unwrap(), &mark, now()));
        // Kept: a restart goes on with the new key.
        let restarted = AppState::load(
            a.instance.clone(),
            Store::open(&a.instance.dir.join("state.sqlite3")).unwrap(),
            watch::channel(false).0,
        )
        .unwrap();
        assert_eq!(restarted.find_key_told().as_deref(), Some(second.as_str()));

        let mut outbound = NodeConfig::default();
        outbound.node.network = "direct".into();
        let b = state(root.path(), "laptop", outbound);
        assert!(b.instance.config.outbound_only());
        assert_eq!(b.find_key_told(), None);
        assert_eq!(b.find_mark(), None);
        assert_eq!(b.addresses().find_key, None);
    }
}
