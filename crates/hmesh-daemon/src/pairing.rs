//! Pairing with one approval on each side. The computer that asks keeps
//! checking whether the other one approved; once it has, and the other
//! computer accepts this one's certificate, it trusts that computer too.
//! Nothing is trusted here before the other side's owner approves.
use super::*;
use std::time::Duration;

const CHECK_EVERY: Duration = Duration::from_secs(3);
/// After the request's deadline (plus a minute for an approval in its last
/// seconds), keep asking, less often: the other computer may have approved
/// while this one was off or asleep, and only it can say.
const GRACE_SECONDS: i64 = 60;
const LATE_CHECK_EVERY: Duration = Duration::from_secs(60);
/// Give up on a computer that cannot be reached for this long.
const GIVE_UP_SECONDS: i64 = 24 * 60 * 60;
/// How often to retry telling paired computers this one's card.
const ANNOUNCE_EVERY: Duration = Duration::from_secs(300);
/// Status questions per minute; counted apart from new pairing handshakes.
const STATUS_PER_MINUTE: usize = 600;

/// What the other computer says about a request this one sent it.
#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PairStatus {
    pub request_id: Uuid,
    /// pending, approved, declined, revoked or expired.
    pub status: String,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PairDecline {
    pub request_id: Uuid,
}

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/hmesh/v1/pair/requests/{id}", get(status))
        .route("/local/pairs/decline", post(decline))
        .route("/hmesh/v1/peers/card", post(peer_card))
}

/// A paired computer tells this one its new card: it moved to another
/// address (switched between "only MCP" and "full", or a new port) or was
/// renamed. Only its own card, with the same identity and key, is accepted.
async fn peer_card(
    State(s): State<AppState>,
    Extension(c): Extension<ConnectionIdentity>,
    headers: HeaderMap,
    Json(card): Json<NodeCard>,
) -> ApiResult<Value> {
    let actor = s.actor(&headers, &c)?;
    let fingerprint = c.fingerprint.as_deref().unwrap_or_default();
    if actor.owner || card.node_id != actor.node_id || card.fingerprint != fingerprint {
        return Err(ApiError(MeshError::new(
            ErrorCode::PermissionDenied,
            "a computer may only update its own card",
        )));
    }
    validate_card(&card).map_err(|e| ApiError(MeshError::input(e.to_string())))?;
    let updated = s.store.update_peer_card(&card)?;
    if updated {
        s.store.audit(
            Some(actor.node_id),
            "peer.card",
            &card.endpoint,
            "updated",
            Uuid::new_v4(),
        )?;
    }
    Ok(Json(json!({"updated": updated})))
}

/// Tell every paired computer this one's current card and address list,
/// and ask each for its own list, now and every few minutes: a computer that
/// only connects out learns where the others are only by asking. Computers
/// that are off are tried again later, as are ones too old to take the card;
/// one too old to know address lists is not told again. A card that names a
/// new address (a computer on every interface moved) and a newer list are
/// told again. Nothing waits on this.
pub async fn announce(state: AppState, mut stop: watch::Receiver<bool>) {
    let mut told = Told::default();
    let mut told_addresses = Told::default();
    loop {
        announce_round(&state, &mut told, &mut told_addresses).await;
        tokio::select! {
            changed = stop.changed() => if changed.is_err() || *stop.borrow() { return },
            _ = tokio::time::sleep(ANNOUNCE_EVERY) => {}
            _ = state.announce_now.notified() => {}
        }
    }
}
/// One round of `announce`. The address list told is the one this computer
/// had when the round began, read before the computers to tell are: a
/// revocation changes the find key only once the computer revoked is no
/// longer trusted (`revoke_pairing`), so a list read with the new key
/// never goes to a computer read as trusted before it was revoked. Each
/// computer is also checked again just before it is told: one revoked while
/// the round was under way is told nothing more. The others get the new
/// list in the next round, which the revocation starts (`peers_changed`).
async fn announce_round(state: &AppState, told: &mut Told<String>, told_addresses: &mut Told<i64>) {
    let list = state.addresses();
    let Ok(peers) = state.store.peers() else {
        return;
    };
    let endpoint = state.card().endpoint;
    for peer in peers.into_iter().filter(|p| p.status == "trusted") {
        // No address this computer can call: a computer that only
        // connects out, seen from one that takes connections.
        let Ok(client) = state.peer_client(&peer) else {
            continue;
        };
        if !addresses::pull(state, &peer, &client).await {
            continue;
        }
        let id = peer.card.node_id;
        if told.needs(id, &endpoint) && peer_told(state, &client).await {
            told.record(id, endpoint.clone());
        }
        if told_addresses.needs(id, &list.revision)
            && still_trusted(state, id)
            && addresses::push(&client, &list).await
        {
            told_addresses.record(id, list.revision);
        }
    }
}
/// Whether `peer` is still trusted now (not revoked since it was read).
fn still_trusted(state: &AppState, peer: Uuid) -> bool {
    state
        .store
        .peer(peer)
        .is_ok_and(|peer| peer.status == "trusted")
}
/// What each paired computer was last told (a card's address, an address
/// list's revision): told again whenever this computer's is something else,
/// so moving back to an earlier address is told too.
struct Told<T>(HashMap<Uuid, T>);
impl<T> Default for Told<T> {
    fn default() -> Self {
        Self(HashMap::new())
    }
}
impl<T: PartialEq> Told<T> {
    fn needs(&self, peer: Uuid, value: &T) -> bool {
        self.0.get(&peer) != Some(value)
    }
    fn record(&mut self, peer: Uuid, value: T) {
        self.0.insert(peer, value);
    }
}
pub(crate) async fn peer_told(state: &AppState, client: &Client) -> bool {
    tokio::time::timeout(
        Duration::from_secs(5),
        client.post::<_, Value>("/hmesh/v1/peers/card", &state.card()),
    )
    .await
    .is_ok_and(|r| r.is_ok())
}

/// Anonymous like the rest of the handshake, because the asking computer is
/// not trusted yet. Only whoever holds the request's random ID learns its
/// status word, and nothing else.
async fn status(
    State(s): State<AppState>,
    Extension(c): Extension<ConnectionIdentity>,
    Path(id): Path<Uuid>,
) -> ApiResult<PairStatus> {
    s.anonymous_limit("pair-status", STATUS_PER_MINUTE, c.address.ip())?;
    let record = s
        .store
        .pair(id)?
        .filter(|r| r.direction == "incoming")
        .ok_or_else(|| ApiError(MeshError::input("pair request not found")))?;
    let status = match record.status.as_str() {
        "pending" if record.expires_at < now() => "expired",
        "approved" => {
            let trusted = s
                .store
                .peer_by_fingerprint(&record.card.fingerprint)?
                .is_some_and(|p| p.card.node_id == record.card.node_id);
            if trusted { "approved" } else { "revoked" }
        }
        other => other,
    };
    Ok(Json(PairStatus {
        request_id: id,
        status: status.into(),
    }))
}

/// Decline a request from another computer, or cancel one this computer sent.
async fn decline(
    State(s): State<AppState>,
    Extension(c): Extension<ConnectionIdentity>,
    headers: HeaderMap,
    Json(body): Json<PairDecline>,
) -> ApiResult<PairRecord> {
    s.owner(&headers, &c)?;
    let incoming = s
        .store
        .pair(body.request_id)?
        .ok_or_else(|| ApiError(MeshError::input("pair request not found")))?
        .direction
        == "incoming";
    let (status, action) = if incoming {
        ("declined", "pair.decline")
    } else {
        ("cancelled", "pair.cancel")
    };
    let record = s.store.close_pair(body.request_id, status)?;
    s.store.audit(
        Some(s.instance.card.node_id),
        action,
        &record.card.node_id.to_string(),
        status,
        Uuid::new_v4(),
    )?;
    Ok(Json(record))
}

/// After a restart, go back to waiting on the requests this computer sent
/// that are still open.
pub fn resume(state: &AppState) -> hmesh_core::Result<()> {
    for record in state.store.pairs()? {
        if record.direction == "outgoing" && record.status == "pending" {
            watch(state.clone(), record);
        }
    }
    Ok(())
}

/// Wait in the background for the other computer's answer to `record`.
pub fn watch(state: AppState, record: PairRecord) {
    watch_via(state, record, None);
}
/// The same, asking at `via` (an address typed to pair with it) rather
/// than at its card's address.
pub(crate) fn watch_via(state: AppState, record: PairRecord, via: Option<String>) {
    tokio::spawn(async move {
        let mut stop = state.shutdown.subscribe();
        loop {
            // Cancelled here, revoked, approved by hand, or settled: stop asking.
            match state.store.pair(record.request_id) {
                Ok(Some(current)) if current.status == "pending" => {}
                _ => return,
            }
            // Ask before judging by the clock: only the other computer knows
            // whether its owner approved.
            if check_via(&state, &record, via.as_deref()).await {
                return;
            }
            if now() > record.expires_at + GIVE_UP_SECONDS {
                let _ = state.store.close_pair(record.request_id, "expired");
                return;
            }
            let wait = if now() > record.expires_at + GRACE_SECONDS {
                LATE_CHECK_EVERY
            } else {
                CHECK_EVERY
            };
            tokio::select! {
                changed = stop.changed() => if changed.is_err() || *stop.borrow() { return },
                _ = tokio::time::sleep(wait) => {}
            }
        }
    });
}

/// Ask once. True when the request is settled, either way; false while the
/// other computer says pending or cannot be reached. Asked at its card's
/// address, or at `via` (an address typed to pair with it, which may name a
/// host, or the relay of a relay pairing code) when there is one. Once
/// paired, a typed `via` is kept as an address set by hand for that
/// computer, tried when the others fail; a relay among the addresses it
/// told, until it tells its own list.
pub(crate) async fn check_via(s: &AppState, record: &PairRecord, via: Option<&str>) -> bool {
    let fingerprint = &record.card.fingerprint;
    let path = format!("/hmesh/v1/pair/requests/{}", record.request_id);
    let at = |bootstrap| match via {
        Some(address) if is_relay_address(address) => Client::pinned_through(
            &s.instance,
            address,
            fingerprint,
            bootstrap,
            &s.relay_dialer,
        ),
        Some(address) => Client::pinned(&s.instance, address, fingerprint, bootstrap),
        None => Client::new(
            &s.instance,
            &record.card.endpoint,
            fingerprint,
            bootstrap,
            false,
        ),
    };
    // Not reachable right now, or a computer without this check: keep asking
    // (its owner can still finish by hand).
    let Ok(client) = at(true) else {
        return false;
    };
    let status = match client.get::<PairStatus>(&path).await {
        Ok(status) if status.request_id == record.request_id => status.status,
        _ => return false,
    };
    match status.as_str() {
        "approved" => {
            // Proof rather than a word: the other computer now accepts this
            // computer's certificate.
            let Ok(client) = at(false) else {
                return false;
            };
            match client.get::<NodeCard>("/hmesh/v1/node").await {
                Ok(card)
                    if card.node_id == record.card.node_id && card.fingerprint == *fingerprint => {}
                _ => return false,
            }
            match s
                .store
                .complete_outgoing_pair(record.request_id, fingerprint)
            {
                Ok(peer) => {
                    match via {
                        // Paired through its relay: the way it is reached
                        // until it tells its own list.
                        Some(relay) if is_relay_address(relay) => {
                            if let Err(error) = s.store.add_found_address(peer.card.node_id, relay)
                            {
                                tracing::warn!(code = ?error.code, "the relay it was paired through was not kept");
                            }
                        }
                        Some(address) => keep_typed_address(s, peer.card.node_id, address),
                        None => {}
                    }
                    s.peers_changed();
                    let _ = s.store.audit(
                        Some(s.instance.card.node_id),
                        "pair.complete",
                        &peer.card.node_id.to_string(),
                        "approved",
                        Uuid::new_v4(),
                    );
                }
                // Cancelled or revoked here in the meantime: nothing to do.
                // Otherwise (a changed key, say) the request ends as failed.
                Err(error) => {
                    tracing::warn!(code = ?error.code, message = %error.message, "pairing approved on the other side but not completed here");
                    let _ = s.store.close_pair(record.request_id, "failed");
                }
            }
            true
        }
        "declined" | "revoked" => {
            let _ = s.store.close_pair(record.request_id, "declined");
            true
        }
        "expired" => {
            let _ = s.store.close_pair(record.request_id, "expired");
            true
        }
        _ => false,
    }
}
/// The address its owner typed to pair with `peer`, kept among the ones set
/// by hand for it (once, after any already there).
fn keep_typed_address(s: &AppState, peer: Uuid, address: &str) {
    let mut manual = s.store.peer_routes(peer).unwrap_or_default().manual;
    if manual.iter().any(|known| known == address) || manual.len() >= MAX_PEER_ADDRESSES {
        return;
    }
    manual.push(address.to_owned());
    if let Err(error) = s.store.set_manual_addresses(peer, &manual) {
        tracing::warn!(code = ?error.code, "the address typed to pair was not kept");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A computer that moves from A to B and back to A tells its peers each
    /// time, not only the first time it is at an address.
    #[test]
    fn a_card_is_told_again_whenever_it_names_another_address() {
        let peer = Uuid::new_v4();
        let mut told = Told::default();
        assert!(told.needs(peer, &"https://192.168.1.20:39880".to_owned()));
        told.record(peer, "https://192.168.1.20:39880".to_owned());
        assert!(!told.needs(peer, &"https://192.168.1.20:39880".to_owned()));
        assert!(told.needs(peer, &"https://10.0.0.5:39880".to_owned()));
        told.record(peer, "https://10.0.0.5:39880".to_owned());
        assert!(told.needs(peer, &"https://192.168.1.20:39880".to_owned()));
        assert!(told.needs(Uuid::new_v4(), &"https://10.0.0.5:39880".to_owned()));
    }
    use hmesh_transport::serve;
    use tokio::net::TcpListener;

    /// A computer that answers "approved" but does not accept this computer's
    /// certificate is not trusted; the same answer with the certificate
    /// accepted is.
    #[tokio::test]
    async fn trust_needs_the_other_side_to_accept_this_certificate() {
        let root = tempfile::tempdir().unwrap();
        let a = Arc::new(Instance::init(&root.path().join("asks"), NodeConfig::default()).unwrap());
        let store = Store::open(&a.dir.join("state.sqlite3")).unwrap();
        let (shutdown, _) = watch::channel(false);
        let state = AppState::load(a.clone(), store.clone(), shutdown).unwrap();
        for accepts in [false, true] {
            let mut b = Instance::init(
                &root.path().join(format!("answers-{accepts}")),
                NodeConfig::default(),
            )
            .unwrap();
            let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
            b.card.endpoint = format!("https://{}", listener.local_addr().unwrap());
            let b = Arc::new(b);
            // Says "approved" to anyone, and shows its card to whoever its
            // TLS lets in: only its own certificate, plus ours when it accepts us.
            let card = b.card.clone();
            let app = Router::new()
                .route(
                    "/hmesh/v1/pair/requests/{id}",
                    get(|Path(id): Path<Uuid>| async move {
                        Json(PairStatus {
                            request_id: id,
                            status: "approved".into(),
                        })
                    }),
                )
                .route("/hmesh/v1/node", get(move || async move { Json(card) }));
            let trusted = if accepts {
                vec![Peer {
                    card: a.card.clone(),
                    status: "trusted".into(),
                    approved_at: Some(now()),
                }]
            } else {
                Vec::new()
            };
            let server = b.clone();
            let (stop, receiver) = watch::channel(false);
            let task = tokio::spawn(serve(
                listener,
                app,
                Arc::new(move || server.server_config(&trusted)),
                receiver,
            ));
            let record = PairRecord {
                request_id: Uuid::new_v4(),
                card: b.card.clone(),
                status: "pending".into(),
                direction: "outgoing".into(),
                expires_at: now() + 300,
            };
            store.save_outgoing_pair(&record).unwrap();
            assert_eq!(check_via(&state, &record, None).await, accepts);
            let is_trusted = store
                .peer_by_fingerprint(&b.card.fingerprint)
                .unwrap()
                .is_some();
            assert_eq!(is_trusted, accepts);
            let status = store.pair(record.request_id).unwrap().unwrap().status;
            assert_eq!(status, if accepts { "approved" } else { "pending" });
            if accepts {
                // Back long after the deadline (off or asleep while the
                // other side approved): ask first, then finish.
                let late = PairRecord {
                    request_id: Uuid::new_v4(),
                    expires_at: now() - 2 * GRACE_SECONDS,
                    ..record.clone()
                };
                store.save_outgoing_pair(&late).unwrap();
                watch(state.clone(), late.clone());
                let mut status = String::new();
                for _ in 0..50 {
                    status = store.pair(late.request_id).unwrap().unwrap().status;
                    if status != "pending" {
                        break;
                    }
                    tokio::time::sleep(Duration::from_millis(100)).await;
                }
                assert_eq!(status, "approved");
            }
            let _ = stop.send(true);
            let _ = task.await;
        }
    }

    /// A computer on a new loopback port, to stand for a paired one.
    async fn standing_in(root: &std::path::Path, name: &str) -> (Arc<Instance>, TcpListener) {
        let mut b = Instance::init(&root.join(name), NodeConfig::default()).unwrap();
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        b.card.endpoint = format!("https://{}", listener.local_addr().unwrap());
        (Arc::new(b), listener)
    }
    /// The find key of each address list a computer was told, in order.
    type KeysTold = Arc<Mutex<Vec<Option<String>>>>;
    /// Serve `b` to `asks` at `listener`: it gives an empty address list of
    /// its own (running `asked` first), takes the card, and keeps the find
    /// key of every address list it is told.
    fn telling(
        b: Arc<Instance>,
        listener: TcpListener,
        asks: &Instance,
        asked: Arc<dyn Fn() + Send + Sync>,
    ) -> (KeysTold, watch::Sender<bool>) {
        let trusted = vec![Peer {
            card: asks.card.clone(),
            status: "trusted".into(),
            approved_at: Some(now()),
        }];
        let told = Arc::new(Mutex::new(Vec::new()));
        let kept = told.clone();
        let own = PeerAddresses {
            node_id: b.card.node_id,
            revision: 1,
            addresses: vec![],
            find_key: None,
        };
        let app = Router::new()
            .route(
                "/hmesh/v1/peers/addresses",
                get(move || {
                    asked();
                    let own = own.clone();
                    async move { Json(own) }
                })
                .post(move |Json(list): Json<PeerAddresses>| {
                    kept.lock().unwrap().push(list.find_key);
                    async { Json(json!({"updated": true})) }
                }),
            )
            .route(
                "/hmesh/v1/peers/card",
                post(|| async { Json(json!({"updated": true})) }),
            );
        let (stop, receiver) = watch::channel(false);
        tokio::spawn(serve(
            listener,
            app,
            Arc::new(move || b.server_config(&trusted)),
            receiver,
        ));
        (told, stop)
    }

    /// A paired computer revoked while a round of telling is under way, and
    /// before that round's address list was told to it, is told nothing
    /// more: not the new find key the revocation made. The revocation is
    /// made at a fixed point (as it is asked for its own list). The computer
    /// still paired is told the list the round began with, then the new key
    /// in the next round; the one revoked never.
    #[tokio::test]
    async fn a_computer_revoked_during_a_round_is_never_told_the_new_find_key() {
        use std::sync::atomic::{AtomicBool, Ordering};
        let root = tempfile::tempdir().unwrap();
        let mut config = NodeConfig::default();
        config.node.network = "direct".into();
        config.node.expose_to_tailnet = true;
        config.node.listen_address = "127.0.0.1".parse().unwrap();
        config.node.host_names = Some(vec![]);
        let a = Arc::new(Instance::init(&root.path().join("harbor"), config).unwrap());
        let store = Store::open(&a.dir.join("state.sqlite3")).unwrap();
        let state = AppState::load(a.clone(), store, watch::channel(false).0).unwrap();
        let first = state.find_key_told().expect("a find key told");

        let (kept, kept_at) = standing_in(root.path(), "quay").await;
        let (revoked, revoked_at) = standing_in(root.path(), "skiff").await;
        let (kept_told, _kept) = telling(kept.clone(), kept_at, &a, Arc::new(|| {}));
        let revoking = state.clone();
        let revoked_id = revoked.card.node_id;
        let once = AtomicBool::new(false);
        let (revoked_told, _revoked) = telling(
            revoked.clone(),
            revoked_at,
            &a,
            Arc::new(move || {
                if !once.swap(true, Ordering::SeqCst) {
                    revoke_pairing(&revoking, revoked_id).unwrap();
                }
            }),
        );
        state.store.trust_peer(&kept.card).unwrap();
        state.store.trust_peer(&revoked.card).unwrap();

        let (mut told, mut told_addresses) = (Told::default(), Told::default());
        announce_round(&state, &mut told, &mut told_addresses).await;
        let second = state.find_key_told().expect("a find key told");
        assert_ne!(second, first, "the revocation did not change the key");
        assert_eq!(
            state.store.peer(revoked_id).unwrap().status,
            "revoked",
            "not revoked during the round"
        );
        assert_eq!(
            *revoked_told.lock().unwrap(),
            Vec::<Option<String>>::new(),
            "told after it was revoked"
        );
        assert_eq!(*kept_told.lock().unwrap(), [Some(first.clone())]);

        announce_round(&state, &mut told, &mut told_addresses).await;
        assert_eq!(*kept_told.lock().unwrap(), [Some(first), Some(second)]);
        assert!(revoked_told.lock().unwrap().is_empty());
    }
}
