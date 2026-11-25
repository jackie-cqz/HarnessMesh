//! Pairing with the passcode shown on the other computer (direct network).
//! The asking computer, found the answering one on the network (or was given
//! its address), sends a pair submission and its SPAKE2 message; the answer
//! carries the answering computer's message and confirmation; the asker
//! checks it and sends its own confirmation, and on a match the answering
//! computer trusts it at once. The asker then completes the way any approved
//! request does (pairing::check), which also proves the other side now
//! accepts its certificate.
//!
//! The passcode allows three tries: each handshake started uses one, and a
//! wrong confirmation with none left ends the passcode (the pairing window
//! stays open for pairing codes). A correct one uses it up.
use super::*;
use hmesh_security::passcode::{self, Answer, Asker};

/// Passcode handshake requests per minute and source.
const PER_MINUTE: usize = 10;
/// How long a started handshake waits for the asker's confirmation.
const SESSION_SECONDS: i64 = 60;

/// The window during which this computer answers the pairing handshake
/// (direct network), and its passcode.
#[derive(Default)]
pub struct PairingWindow {
    /// When it closes; 0 when closed.
    pub closes_at: i64,
    /// Six digits, until used or the tries run out.
    pub passcode: Option<String>,
    pub tries_left: u8,
    /// The passcode was used: a computer paired with it.
    pub used: bool,
    /// Grows with every change within one daemon run, so the panel can tell
    /// a newer state from one read before it opened pairing.
    pub revision: u64,
    /// This daemon run: a restart (which closes the window) is a new one,
    /// whatever the clock says.
    pub boot: Uuid,
    sessions: HashMap<Uuid, Session>,
}
struct Session {
    submission: PairSubmission,
    answer: Answer,
    expires_at: i64,
}
/// A matched confirmation whose trust is being recorded.
pub struct Taken {
    pub submission: PairSubmission,
    code: String,
    revision: u64,
}
/// Why a confirmation was not taken.
pub enum Refused {
    NotFound,
    Mismatch(Box<PairSubmission>),
    /// Another matched confirmation has the passcode.
    NoPasscode,
}
impl PairingWindow {
    pub fn is_open(&self, now: i64) -> bool {
        self.closes_at > now
    }
    /// Open for ten minutes from `now`. A passcode that can still be used is
    /// kept (with its tries); otherwise, with `passcode` set, a new one starts.
    pub fn open(&mut self, now: i64, passcode: bool) -> anyhow::Result<()> {
        if self.passcode(now).is_none() {
            self.sessions.clear();
            self.passcode = passcode.then(passcode::new_passcode).transpose()?;
            self.tries_left = passcode::TRIES;
            self.used = false;
        }
        self.closes_at = now + PAIRING_WINDOW_SECONDS;
        self.touch();
        Ok(())
    }
    /// Closed, for a daemon starting now.
    pub fn started() -> Self {
        let mut window = Self {
            boot: Uuid::new_v4(),
            ..Self::default()
        };
        window.touch();
        window
    }
    fn touch(&mut self) {
        let millis = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_millis() as u64);
        self.revision = (self.revision + 1).max(millis);
    }
    /// No more passcode: it was used, or its tries ran out. The window stays
    /// open for pairing codes; only the passcode (and the announcement that
    /// goes with it) ends, so guessing it cannot shut pairing out.
    fn void(&mut self, used: bool) {
        self.passcode = None;
        self.sessions.clear();
        self.used = used;
        self.touch();
    }
    /// Answer an asker's first message, spending a try (a handshake never
    /// confirmed is still a guess). `asker` and `answerer` are the two
    /// certificate fingerprints.
    pub fn begin(
        &mut self,
        now: i64,
        submission: PairSubmission,
        answerer: &str,
        message: &str,
    ) -> std::result::Result<PasscodeAnswer, ApiError> {
        self.sessions.retain(|_, session| session.expires_at >= now);
        if self.tries_left == 0 {
            return Err(no_passcode());
        }
        let Some(code) = self.passcode(now).map(str::to_owned) else {
            return Err(no_passcode());
        };
        self.tries_left -= 1;
        self.touch();
        let answer = Answer::new(
            &code,
            &submission.card.fingerprint,
            answerer,
            submission.request_id,
            message,
        )
        .map_err(|e| ApiError(MeshError::input(e.to_string())))?;
        let session_id = Uuid::new_v4();
        let reply = PasscodeAnswer {
            session_id,
            message: answer.message.clone(),
            confirmation: answer.confirmation.clone(),
        };
        self.sessions.insert(
            session_id,
            Session {
                submission,
                answer,
                expires_at: now + SESSION_SECONDS,
            },
        );
        Ok(reply)
    }
    /// Check an asker's confirmation. On a match the passcode is taken out
    /// (shown as used) while trust is recorded, so it pairs one computer
    /// only: any other confirmation now finds no passcode. `settle` then
    /// ends it, or gives it back.
    pub fn take_matched(
        &mut self,
        now: i64,
        id: Uuid,
        confirmation: &str,
    ) -> std::result::Result<Taken, Refused> {
        let session = self
            .sessions
            .remove(&id)
            .filter(|session| session.expires_at >= now)
            .ok_or(Refused::NotFound)?;
        if !session.answer.confirms(confirmation) {
            if self.tries_left == 0 {
                self.void(false);
            }
            return Err(Refused::Mismatch(Box::new(session.submission)));
        }
        let Some(code) = self.passcode.take() else {
            return Err(Refused::NoPasscode);
        };
        self.used = true;
        self.touch();
        Ok(Taken {
            submission: session.submission,
            code,
            revision: self.revision,
        })
    }
    /// After trust was recorded (or not) for `taken`: used up, or back with
    /// its try (the right passcode is not a guess). Left alone when the window
    /// changed meanwhile, as when reopened with a new passcode.
    pub fn settle(&mut self, taken: Taken, trusted: bool) {
        if self.revision != taken.revision {
            if trusted && self.passcode.is_none() {
                self.used = true;
            }
            return;
        }
        if trusted {
            self.void(true);
        } else {
            self.passcode = Some(taken.code);
            self.used = false;
            self.tries_left += 1;
            self.touch();
        }
    }
    /// The passcode, while the window is open and it has tries left, or a
    /// try still being checked. A try is spent when its handshake starts, so
    /// an asker that never confirms still uses one up.
    pub fn passcode(&self, now: i64) -> Option<&str> {
        self.passcode.as_deref().filter(|_| {
            self.is_open(now)
                && (self.tries_left > 0 || self.sessions.values().any(|s| s.expires_at >= now))
        })
    }
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PasscodeStart {
    pub submission: PairSubmission,
    pub message: String,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PasscodeAnswer {
    pub session_id: Uuid,
    pub message: String,
    pub confirmation: String,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PasscodeConfirm {
    pub confirmation: String,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PasscodeDone {
    pub request_id: Uuid,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PasscodeTarget {
    pub endpoint: String,
    pub passcode: String,
}

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/hmesh/v1/pair/passcode", post(start))
        .route("/hmesh/v1/pair/passcode/{id}", post(confirm))
        .route("/local/pairs/passcode", post(pair_with_passcode))
}

fn no_passcode() -> ApiError {
    ApiError(MeshError::new(
        ErrorCode::PermissionDenied,
        "this computer has no passcode open: open 配对新电脑 in its panel (or run `hmesh pair show` there) for a new one",
    ))
}
fn mismatch() -> ApiError {
    ApiError(MeshError::new(
        ErrorCode::Unauthenticated,
        passcode::MISMATCH,
    ))
}

/// Answer an asker's first message with this computer's passcode.
async fn start(
    State(s): State<AppState>,
    Extension(c): Extension<ConnectionIdentity>,
    Json(body): Json<PasscodeStart>,
) -> ApiResult<PasscodeAnswer> {
    s.anonymous_limit("passcode", PER_MINUTE, c.address.ip())?;
    if !s.offers_passcode() {
        return Err(no_passcode());
    }
    verify_pair_submission(&body.submission, s.instance.card.node_id).map_err(|_| {
        ApiError(MeshError::new(
            ErrorCode::Unauthenticated,
            "pairing proof rejected",
        ))
    })?;
    if body.submission.card.node_id == s.instance.card.node_id {
        return Err(ApiError(MeshError::input("cannot pair a node with itself")));
    }
    let reply = s.pairing.lock().map_err(internal)?.begin(
        now(),
        body.submission,
        &s.instance.card.fingerprint,
        &body.message,
    )?;
    Ok(Json(reply))
}

/// The asker's confirmation: on a match, trust it now.
async fn confirm(
    State(s): State<AppState>,
    Extension(c): Extension<ConnectionIdentity>,
    Path(id): Path<Uuid>,
    Json(body): Json<PasscodeConfirm>,
) -> ApiResult<PasscodeDone> {
    s.anonymous_limit("passcode", PER_MINUTE, c.address.ip())?;
    let taken = s
        .pairing
        .lock()
        .map_err(internal)?
        .take_matched(now(), id, &body.confirmation);
    let taken = match taken {
        Ok(taken) => taken,
        Err(Refused::NotFound) => {
            return Err(ApiError(MeshError::input(
                "passcode handshake not found or expired: start again",
            )));
        }
        Err(Refused::NoPasscode) => return Err(no_passcode()),
        Err(Refused::Mismatch(submission)) => {
            s.nearby.changed.notify_one();
            let _ = s.store.audit(
                Some(s.instance.card.node_id),
                "pair.passcode",
                &submission.card.node_id.to_string(),
                "mismatch",
                Uuid::new_v4(),
            );
            return Err(mismatch());
        }
    };
    // Trust first, without holding the window: a challenge that expired or
    // was used meanwhile, or a key change, gives the passcode back with its
    // try. A request recorded but not trusted is closed, not left to approve.
    let fingerprint = taken.submission.card.fingerprint.clone();
    let trusted = s.store.accept_pair(&taken.submission).and_then(|record| {
        match s.store.approve_pair(record.request_id, &fingerprint) {
            Ok(peer) => Ok((record, peer)),
            Err(error) => {
                let _ = s.store.close_pair(record.request_id, "failed");
                Err(error)
            }
        }
    });
    s.pairing
        .lock()
        .map_err(internal)?
        .settle(taken, trusted.is_ok());
    let (record, peer) = match trusted {
        Ok(trusted) => trusted,
        Err(error) => {
            return Err(ApiError(MeshError::new(
                ErrorCode::TaskConflict,
                format!(
                    "the passcode matched, but pairing could not finish on this computer ({}); choose it again",
                    error.message
                ),
            )));
        }
    };
    s.nearby.changed.notify_one();
    s.peers_changed();
    // Trusted already: a failed audit write does not undo it.
    let _ = s.store.audit(
        Some(s.instance.card.node_id),
        "pair.passcode",
        &peer.card.node_id.to_string(),
        "approved",
        Uuid::new_v4(),
    );
    Ok(Json(PasscodeDone {
        request_id: record.request_id,
    }))
}

/// Pair with the computer at `endpoint` using the passcode its screen shows.
async fn pair_with_passcode(
    State(s): State<AppState>,
    Extension(c): Extension<ConnectionIdentity>,
    headers: HeaderMap,
    Json(target): Json<PasscodeTarget>,
) -> ApiResult<PairRecord> {
    s.owner(&headers, &c)?;
    let code = passcode::normalize_passcode(&target.passcode)
        .map_err(|e| ApiError(MeshError::input(e.to_string())))?;
    // Its certificate is not known yet: learn it from this first answer,
    // then pin it. The passcode confirmations below are what make it trusted.
    let (card, seen) = Client::learn(&s.instance, &target.endpoint).await?;
    validate_card(&card).map_err(|e| ApiError(MeshError::input(e.to_string())))?;
    if card.node_id == s.instance.card.node_id {
        return Err(ApiError(MeshError::input("cannot pair a node with itself")));
    }
    let client =
        Client::new(&s.instance, &target.endpoint, &seen, true, false).map_err(internal)?;
    let challenge: PairChallenge = client.get("/hmesh/v1/pair/challenge").await?;
    if challenge.node_id != card.node_id {
        return Err(ApiError(MeshError::new(
            ErrorCode::Unauthenticated,
            "pair challenge belongs to a different node",
        )));
    }
    let submission = s.pair_submission(challenge)?;
    let request_id = submission.request_id;
    let (asker, message) = Asker::start(
        &code,
        &s.instance.card.fingerprint,
        &card.fingerprint,
        request_id,
    )
    .map_err(internal)?;
    let answer: PasscodeAnswer = client
        .post(
            "/hmesh/v1/pair/passcode",
            &PasscodeStart {
                submission,
                message,
            },
        )
        .await?;
    let confirmation = match asker.finish(&answer.message, &answer.confirmation) {
        Ok(confirmation) => confirmation,
        Err(_) => {
            // Tell the other computer at once, with an empty confirmation
            // (it carries nothing), so its last try closes the window now.
            let _ = client
                .post::<_, Value>(
                    &format!("/hmesh/v1/pair/passcode/{}", answer.session_id),
                    &PasscodeConfirm {
                        confirmation: String::new(),
                    },
                )
                .await;
            return Err(mismatch());
        }
    };
    // Recorded before confirming: if the answer to the confirmation is lost,
    // the other computer may trust this one already, and the request still
    // finishes in the background like any other.
    let record = PairRecord {
        request_id,
        card,
        status: "pending".into(),
        direction: "outgoing".into(),
        expires_at: now() + 300,
    };
    s.store.save_outgoing_pair(&record)?;
    let done = client
        .post::<_, PasscodeDone>(
            &format!("/hmesh/v1/pair/passcode/{}", answer.session_id),
            &PasscodeConfirm { confirmation },
        )
        .await;
    match done {
        Ok(done) if done.request_id == request_id => {}
        // No answer: it may have trusted this computer; ask in the background.
        Err(error) if error.code == ErrorCode::PeerUnreachable => {
            pairing::watch(s.clone(), record.clone());
            return Ok(Json(record));
        }
        // Refused (or answered for another request): nothing was trusted.
        other => {
            let _ = s.store.close_pair(request_id, "cancelled");
            return Err(match other {
                Err(error) => ApiError(error),
                Ok(_) => ApiError(MeshError::new(
                    ErrorCode::Unauthenticated,
                    "pair request correlation failed",
                )),
            });
        }
    }
    // The other computer already trusts this one: finish now, or keep
    // trying in the background if it cannot be reached this moment.
    if !pairing::check(&s, &record).await {
        pairing::watch(s.clone(), record.clone());
    }
    Ok(Json(s.store.pair(request_id)?.unwrap_or(record)))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Asker's side of a handshake against `window`: its confirmation.
    fn handshake(
        window: &mut PairingWindow,
        now: i64,
        code: &str,
        asker: &Instance,
        answerer: &str,
    ) -> std::result::Result<(Uuid, String), ApiError> {
        let challenge = PairChallenge {
            nonce: Uuid::new_v4(),
            node_id: Uuid::new_v4(),
            expires_at: now + 60,
        };
        let submission = asker.pair_submission(challenge).unwrap();
        let (start, message) = passcode::Asker::start(
            code,
            &asker.card.fingerprint,
            answerer,
            submission.request_id,
        )
        .unwrap();
        let answer = window.begin(now, submission, answerer, &message)?;
        Ok((
            answer.session_id,
            start
                .finish(&answer.message, &answer.confirmation)
                .unwrap_or_default(),
        ))
    }

    #[test]
    fn a_passcode_pairs_one_computer_even_when_two_know_it() {
        let root = tempfile::tempdir().unwrap();
        let one = Instance::init(&root.path().join("one"), NodeConfig::default()).unwrap();
        let two = Instance::init(&root.path().join("two"), NodeConfig::default()).unwrap();
        let answerer = Instance::init(&root.path().join("answers"), NodeConfig::default()).unwrap();
        let fingerprint = &answerer.card.fingerprint;
        let now = 1000;
        let mut window = PairingWindow::started();
        window.open(now, true).unwrap();
        let code = window.passcode(now).unwrap().to_owned();
        // Both start with the right passcode before either confirms.
        let (first, first_confirms) =
            handshake(&mut window, now, &code, &one, fingerprint).unwrap();
        let (second, second_confirms) =
            handshake(&mut window, now, &code, &two, fingerprint).unwrap();
        let taken = window
            .take_matched(now, first, &first_confirms)
            .ok()
            .expect("first matches");
        // While the first is being recorded, the second finds no passcode.
        assert!(matches!(
            window.take_matched(now, second, &second_confirms),
            Err(Refused::NoPasscode)
        ));
        assert!(window.passcode(now).is_none() && window.used);
        window.settle(taken, true);
        assert!(window.passcode(now).is_none() && window.used);
        // A well-formed handshake with the right passcode: none to answer it.
        let refused = handshake(&mut window, now, &code, &one, fingerprint).unwrap_err();
        assert_eq!(
            refused.0.code,
            ErrorCode::PermissionDenied,
            "{}",
            refused.0.message
        );
    }

    #[test]
    fn the_right_passcode_that_could_not_be_trusted_comes_back_with_its_try() {
        let root = tempfile::tempdir().unwrap();
        let asker = Instance::init(&root.path().join("asks"), NodeConfig::default()).unwrap();
        let answerer = Instance::init(&root.path().join("answers"), NodeConfig::default()).unwrap();
        let fingerprint = &answerer.card.fingerprint;
        let now = 1000;
        let mut window = PairingWindow::started();
        window.open(now, true).unwrap();
        let code = window.passcode(now).unwrap().to_owned();
        let (id, confirms) = handshake(&mut window, now, &code, &asker, fingerprint).unwrap();
        assert_eq!(window.tries_left, passcode::TRIES - 1);
        let taken = window
            .take_matched(now, id, &confirms)
            .ok()
            .expect("matches");
        window.settle(taken, false);
        assert_eq!(window.passcode(now), Some(code.as_str()));
        assert_eq!(window.tries_left, passcode::TRIES);
        assert!(!window.used);
        // Reopened meanwhile with a new passcode: that one is left alone.
        let (id, confirms) = handshake(&mut window, now, &code, &asker, fingerprint).unwrap();
        let taken = window
            .take_matched(now, id, &confirms)
            .ok()
            .expect("matches");
        window.open(now + 1, true).unwrap();
        let fresh = window.passcode(now + 1).unwrap().to_owned();
        window.settle(taken, false);
        assert_eq!(window.passcode(now + 1), Some(fresh.as_str()));
    }

    #[test]
    fn an_unused_passcode_survives_reopening_and_a_used_spent_or_closed_one_does_not() {
        let mut window = PairingWindow::default();
        window.open(1000, true).unwrap();
        let first = window.passcode(1000).unwrap().to_owned();
        window.tries_left = 1;
        // Reopened while open and unused: same passcode, same tries.
        window.open(1100, true).unwrap();
        assert_eq!(window.passcode(1100), Some(first.as_str()));
        assert_eq!(window.tries_left, 1);
        assert_eq!(window.closes_at, 1100 + PAIRING_WINDOW_SECONDS);
        // Closed by the clock: no passcode, and reopening starts over.
        assert_eq!(window.passcode(window.closes_at), None);
        window.open(window.closes_at + 1, true).unwrap();
        assert_eq!(window.tries_left, passcode::TRIES);
        // Used: reopening gives a fresh one.
        let now = window.closes_at - 1;
        window.void(true);
        assert!(window.is_open(now) && window.passcode(now).is_none() && window.used);
        window.open(now, true).unwrap();
        assert!(window.passcode(now).is_some() && !window.used);
        // Out of tries: the passcode ends, the window does not (pairing
        // codes still work), and reopening gives a fresh passcode.
        window.tries_left = 0;
        assert!(window.is_open(now) && window.passcode(now).is_none());
        window.open(now, true).unwrap();
        assert_eq!(window.tries_left, passcode::TRIES);
        // Not a direct network: open, without a passcode.
        let mut window = PairingWindow::default();
        window.open(1000, false).unwrap();
        assert!(window.is_open(1000) && window.passcode(1000).is_none());
    }
}
