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

/// Tell every paired computer this one's current card, now and every few
/// minutes until each has it. Computers that are off, or too old to take it,
/// are tried again later; nothing waits on this.
pub async fn announce(state: AppState, mut stop: watch::Receiver<bool>) {
    let mut told = std::collections::HashSet::new();
    loop {
        if let Ok(peers) = state.store.peers() {
            for peer in peers {
                if peer.status != "trusted"
                    || told.contains(&peer.card.node_id)
                    || !state.reaches(&peer)
                {
                    continue;
                }
                if peer_told(&state, &peer).await {
                    told.insert(peer.card.node_id);
                }
            }
        }
        tokio::select! {
            changed = stop.changed() => if changed.is_err() || *stop.borrow() { return },
            _ = tokio::time::sleep(ANNOUNCE_EVERY) => {}
        }
    }
}
async fn peer_told(state: &AppState, peer: &Peer) -> bool {
    let Ok(client) = Client::new(
        &state.instance,
        &peer.card.endpoint,
        &peer.card.fingerprint,
        false,
        false,
    ) else {
        return false;
    };
    tokio::time::timeout(
        Duration::from_secs(5),
        client.post::<_, Value>("/hmesh/v1/peers/card", &state.instance.card),
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
            if check(&state, &record).await {
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
/// other computer says pending or cannot be reached.
pub(crate) async fn check(s: &AppState, record: &PairRecord) -> bool {
    let fingerprint = &record.card.fingerprint;
    let path = format!("/hmesh/v1/pair/requests/{}", record.request_id);
    // Not reachable right now, or a computer without this check: keep asking
    // (its owner can still finish by hand).
    let Ok(client) = Client::new(&s.instance, &record.card.endpoint, fingerprint, true, false)
    else {
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
            let Ok(client) = Client::new(
                &s.instance,
                &record.card.endpoint,
                fingerprint,
                false,
                false,
            ) else {
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

#[cfg(test)]
mod tests {
    use super::*;
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
            assert_eq!(check(&state, &record).await, accepts);
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
}
