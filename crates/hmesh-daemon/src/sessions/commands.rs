//! ACP command announcements belong to the current session and agent process.
//! An old snapshot is never authority to dispatch a slash-prefixed prompt.
use super::*;
use axum::http::Method;

fn valid_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 256
        && !name.starts_with('/')
        && !name.chars().any(|c| c.is_whitespace() || c.is_control())
}

fn parse_commands(
    update: &Value,
    secrets: &[String],
    harness: HarnessKind,
    agent_info: &Value,
) -> Option<Vec<SessionCommand>> {
    let items = update["availableCommands"].as_array()?;
    let mut commands = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for item in items {
        let Some(name) = item["name"].as_str() else {
            continue;
        };
        if !valid_name(name) || redact(secrets, name) != name || !seen.insert(name) {
            continue;
        }
        let text = |raw: &str| clip(&redact(secrets, raw), 512).0;
        commands.push(SessionCommand {
            name: name.to_owned(),
            description: text(item["description"].as_str().unwrap_or("")),
            input_hint: item
                .pointer("/input/hint")
                .and_then(Value::as_str)
                .map(text),
        });
        if commands.len() >= 256 {
            break;
        }
    }
    // OpenCode 1.19.6 handles /compact with its native summarization path,
    // but leaves it out of the command notification. Scope this exception to
    // the observed binary; every other session follows its announcement.
    if harness == HarnessKind::Opencode
        && agent_info["name"] == "OpenCode"
        && agent_info["version"] == "1.19.6"
        && !commands.iter().any(|c| c.name == "compact")
    {
        commands.push(SessionCommand {
            name: "compact".to_owned(),
            description: "Compact this session".to_owned(),
            input_hint: None,
        });
    }
    Some(commands)
}

pub(super) fn advertised(s: &AppState, id: Uuid, live: &Arc<Live>, params: &Value) {
    let Some(session) = params["sessionId"].as_str() else {
        return;
    };
    if session.is_empty()
        || session.len() > 1024
        || live
            .agent_session
            .lock()
            .is_ok_and(|current| !current.is_empty() && *current != session)
    {
        return;
    }
    let Some(commands) = parse_commands(
        &params["update"],
        &live.secrets,
        live.harness,
        &live.conn.initialize["agentInfo"],
    ) else {
        return;
    };
    // Some agents announce before the session/new response arrives. Keep the
    // announced ID and match it against the response before showing the list.
    if let Ok(mut slot) = live.commands.lock() {
        let updated = (session.to_owned(), commands);
        if slot.as_ref() == Some(&updated) {
            return;
        }
        *slot = Some(updated);
    }
    if live.agent_session.lock().is_ok_and(|id| *id == session)
        && s.sessions
            .live(id)
            .is_some_and(|current| Arc::ptr_eq(&current, live))
    {
        let _ = record_event(s, id, "commands_updated", json!({}));
    }
}

pub(super) fn current_commands(live: &Live) -> Option<Vec<SessionCommand>> {
    if !live.conn.alive() {
        return None;
    }
    let id = live.agent_session.lock().ok()?;
    let snapshot = live.commands.lock().ok()?;
    snapshot
        .as_ref()
        .filter(|(source, _)| source == &*id)
        .map(|(_, commands)| commands.clone())
}

pub(super) fn require_advertised(live: Option<&Live>, name: &str) -> hmesh_core::Result<()> {
    if !valid_name(name)
        || !live
            .and_then(current_commands)
            .is_some_and(|v| v.iter().any(|c| c.name == name))
    {
        return Err(MeshError::input(
            "this command is not advertised by the current agent session",
        ));
    }
    Ok(())
}

fn snapshot(s: &AppState, id: Uuid) -> hmesh_core::Result<SessionCommands> {
    let record = s.store.session(id)?;
    let harness = s.store.agent(record.agent_id)?.0.harness;
    let live = s
        .sessions
        .live(id)
        .filter(|l| l.conn.alive() && l.agent_session.lock().is_ok_and(|id| !id.is_empty()));
    let commands = live.as_deref().and_then(current_commands);
    let state = match record.status {
        SessionStatus::Closed => CommandListState::Closed,
        SessionStatus::PendingApproval => CommandListState::PendingApproval,
        _ if commands.is_some() => CommandListState::Ready,
        _ if live.is_some() => CommandListState::Unadvertised,
        _ => CommandListState::Inactive,
    };
    Ok(SessionCommands {
        session_id: id,
        harness,
        state,
        commands: if state == CommandListState::Ready {
            commands.unwrap_or_default()
        } else {
            vec![]
        },
        config_options: if matches!(
            state,
            CommandListState::Ready | CommandListState::Unadvertised
        ) {
            record.config_options
        } else {
            vec![]
        },
    })
}

/// Warm the agent process without opening a prompt turn. Reserve the same
/// between-turn slot used for settings changes, under the session turn lock.
async fn prepare(s: &AppState, id: Uuid) -> hmesh_core::Result<()> {
    let record = {
        let _guard = s
            .sessions
            .starting
            .lock()
            .map_err(|_| MeshError::new(ErrorCode::Internal, "session lock poisoned"))?;
        let record = s.store.session(id)?;
        if record.status != SessionStatus::Idle {
            return Ok(());
        }
        if record.origin.root_node_id != s.instance.card.node_id
            && s.task_decision(Actor {
                node_id: record.origin.root_node_id,
                owner: false,
            }) == Decision::Denied
        {
            return Err(MeshError::new(
                ErrorCode::PermissionDenied,
                "the computer that started this session is no longer paired",
            ));
        }
        super::check_place(s, &record)?;
        if !s
            .sessions
            .configuring
            .lock()
            .map_err(|_| MeshError::new(ErrorCode::Internal, "session lock poisoned"))?
            .insert(id)
        {
            return Err(MeshError::new(
                ErrorCode::TaskConflict,
                "the session is being prepared or configured",
            ));
        }
        record
    };
    // A disconnected HTTP client must not abandon a partially initialized
    // ACP process in the live map. Initialization has its own ACP deadlines.
    let state = s.clone();
    tokio::spawn(async move {
        let setup_guard = Configuring {
            sessions: &state.sessions,
            id,
        };
        let live = ensure_live(&state, &record).await.map_err(|mut e| {
            e.message = redact(&agent_secrets(&state, record.agent_id), &e.message);
            e
        })?;
        if let Ok(mut active) = live.last_active.lock() {
            *active = Instant::now();
        }
        live.conn.barrier().await;
        drop(setup_guard);
        // DSH supplies its model picker in the session response. Its ACP
        // profile has no native command catalog to wait for. Any later
        // command notification still goes through the normal event listener.
        if live.harness == HarnessKind::Dsh {
            return Ok(());
        }
        // ACP allows the first command announcement to arrive after session/new.
        // Subscribe before checking the list so that no update can be missed.
        let mut watcher = state.sessions.watch(id);
        let deadline = tokio::time::Instant::now() + Duration::from_secs(2);
        while live.conn.alive()
            && state
                .sessions
                .live(id)
                .is_some_and(|active| Arc::ptr_eq(&active, &live))
            && current_commands(&live).is_none()
        {
            if !matches!(
                tokio::time::timeout_at(deadline, watcher.changed()).await,
                Ok(Ok(()))
            ) {
                break;
            }
        }
        Ok(())
    })
    .await
    .map_err(|_| MeshError::new(ErrorCode::Internal, "agent initialization was interrupted"))?
}

fn invoke(
    s: &AppState,
    id: Uuid,
    body: RunSessionCommand,
    from: Value,
) -> hmesh_core::Result<SessionRecord> {
    if !valid_name(&body.name) {
        return Err(MeshError::input(
            "expected an advertised command name without its leading slash",
        ));
    }
    let mut text = format!("/{}", body.name);
    if !body.arguments.is_empty() {
        text.push(' ');
        text.push_str(&body.arguments);
    }
    begin_checked_turn(s, id, &text, from, Some(body.name))
}

async fn owner_commands(
    State(s): State<AppState>,
    Extension(c): Extension<ConnectionIdentity>,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
    method: Method,
) -> ApiResult<SessionCommands> {
    s.owner(&headers, &c)?;
    if let Some(peer) = s.store.remote_session_peer(id)? {
        let path = format!("/hmesh/v1/sessions/{id}/commands");
        let client = peer_client(&s, peer)?;
        let result: SessionCommands = if method == Method::POST {
            client.post_with_timeout(&path, &json!({}), 40).await?
        } else {
            client.get(&path).await?
        };
        if result.session_id != id {
            return Err(err(
                ErrorCode::Internal,
                "the peer answered about another session",
            ));
        }
        peer_client(&s, peer)?;
        return Ok(Json(result));
    }
    if method == Method::POST {
        prepare(&s, id).await?;
    }
    Ok(Json(snapshot(&s, id)?))
}
async fn peer_commands(
    State(s): State<AppState>,
    Extension(c): Extension<ConnectionIdentity>,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
    method: Method,
) -> ApiResult<SessionCommands> {
    let actor = s.actor(&headers, &c)?;
    peer_session(&s, actor, id)?;
    if method == Method::POST {
        prepare(&s, id).await?;
        s.actor(&headers, &c)?;
    }
    Ok(Json(snapshot(&s, id)?))
}

async fn owner_command(
    State(s): State<AppState>,
    Extension(c): Extension<ConnectionIdentity>,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
    Json(body): Json<RunSessionCommand>,
) -> ApiResult<SessionRecord> {
    s.owner(&headers, &c)?;
    if let Some(peer) = s.store.remote_session_peer(id)? {
        return Ok(Json(remote_post(&s, peer, id, "command", &body).await?));
    }
    let from = json!({"node_id":s.instance.card.node_id, "label":null});
    Ok(Json(invoke(&s, id, body, from)?))
}
async fn peer_command(
    State(s): State<AppState>,
    Extension(c): Extension<ConnectionIdentity>,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
    Json(body): Json<RunSessionCommand>,
) -> ApiResult<SessionRecord> {
    let actor = s.actor(&headers, &c)?;
    let record = peer_session(&s, actor, id)?;
    let from = json!({"node_id":actor.node_id, "label":record.origin.label});
    Ok(Json(invoke(&s, id, body, from)?))
}

pub(super) fn routes() -> Router<AppState> {
    Router::new()
        .route(
            "/local/sessions/{id}/commands",
            get(owner_commands).post(owner_commands),
        )
        .route(
            "/hmesh/v1/sessions/{id}/commands",
            get(peer_commands).post(peer_commands),
        )
        .route("/local/sessions/{id}/command", post(owner_command))
        .route("/hmesh/v1/sessions/{id}/command", post(peer_command))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compact_fallback_only_applies_to_the_identified_opencode_version() {
        let empty = json!({"availableCommands":[]});
        let pi = json!({"name":"pi-acp","version":"1.19.6"});
        let matched = json!({"name":"OpenCode","version":"1.19.6"});
        let later = json!({"name":"OpenCode","version":"1.19.7"});
        for (kind, info) in [
            (HarnessKind::Pi, &matched),
            (HarnessKind::Opencode, &pi),
            (HarnessKind::Opencode, &later),
        ] {
            assert!(parse_commands(&empty, &[], kind, info).unwrap().is_empty());
        }
        let fallback = parse_commands(&empty, &[], HarnessKind::Opencode, &matched).unwrap();
        assert_eq!(fallback.len(), 1);
        assert_eq!(fallback[0].name, "compact");
        let agent = json!({"availableCommands":[{"name":"compact","description":"Custom project command","input":{"hint":"scope"}}]});
        let native = parse_commands(&agent, &[], HarnessKind::Opencode, &matched).unwrap();
        assert_eq!(native.len(), 1);
        assert_eq!(native[0].input_hint.as_deref(), Some("scope"));
        assert!(parse_commands(&Value::Null, &[], HarnessKind::Opencode, &matched).is_none());
    }
}
