//! Projects (folders on this computer that sessions can work in) and the
//! sessions agents already keep there, read through ACP `session/list`.
//! Earlier sessions stay on this computer: only its owner can list them.
use super::*;
use hmesh_core::projects::*;
use std::path::{Path as FilePath, PathBuf};

/// How long one agent's list for one folder is reused.
const HISTORY_TTL: Duration = Duration::from_secs(30);
/// Pages and sessions read per agent and folder.
const MAX_LIST_PAGES: usize = 20;
const MAX_LISTED: usize = 1000;
/// Starting the agent and reading its list, together. Kept under the panel's
/// longer history deadline so the owner gets this reason instead of a raw RPC
/// timeout; agents that scan many local sessions can take most of it.
const LIST_TIMEOUT: Duration = Duration::from_secs(25);
/// A page that takes this long means more pages would not arrive in time.
const SLOW_PAGE: Duration = Duration::from_secs(8);

type CacheKey = (Uuid, Option<PathBuf>);
#[derive(Default)]
pub(super) struct HistoryCache {
    entries: Mutex<HashMap<CacheKey, (Instant, AgentHistory)>>,
}

/// Where a session works, chosen when it opens.
#[derive(Default)]
pub(super) struct Place {
    /// `None` keeps the agent's registered directory.
    pub cwd: Option<PathBuf>,
    pub project_id: Option<String>,
    pub resume: Option<String>,
    pub title: Option<String>,
}

/// An existing directory, with symbolic links resolved.
fn real_folder(path: &FilePath) -> hmesh_core::Result<PathBuf> {
    if !path.is_absolute() {
        return Err(MeshError::input("the folder must be an absolute path"));
    }
    let real = std::fs::canonicalize(path)
        .map_err(|_| MeshError::input("the folder does not exist on this computer"))?;
    if !real.is_dir() {
        return Err(MeshError::input("the path is not a folder"));
    }
    Ok(real)
}
/// A folder as agents report it, compared with projects after resolving links.
fn same_folder(path: &FilePath) -> PathBuf {
    std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf())
}

/// Every project, with each agent's registered directory added as one.
pub(super) fn projects(s: &AppState) -> hmesh_core::Result<Vec<Project>> {
    let mut list = s.store.projects()?;
    for card in s.store.agents()? {
        let Ok((_, config)) = s.store.agent(card.agent_id) else {
            continue;
        };
        let root = same_folder(&config.project.root);
        if let Some(project) = list.iter_mut().find(|p| p.path == root) {
            project.agent_ids.push(card.agent_id);
            continue;
        }
        let preferred = config.agent.project_id.clone();
        let project_id = (0..)
            .map(|n| {
                if n == 0 {
                    preferred.clone()
                } else {
                    format!("{preferred}-{n}")
                }
            })
            .find(|id| !list.iter().any(|p| &p.project_id == id))
            .unwrap_or(preferred);
        let added = s.store.add_project(&Project {
            project_id,
            name: config.agent.project_id.clone(),
            path: root,
            source: ProjectSource::Agent,
            pinned: false,
            hidden: false,
            shared: false,
            position: 0,
            created_at: now(),
            agent_ids: vec![],
        });
        match added {
            Ok(mut project) => {
                project.agent_ids.push(card.agent_id);
                list.push(project);
            }
            // Another request added it meanwhile; the next read shows it.
            Err(_) => continue,
        }
    }
    Ok(list)
}

/// Resolve the project a new session asks for. `owner_chain` is false for
/// sessions a paired computer started: they may only use shared projects or
/// the agent's own directory.
pub(super) fn place(
    s: &AppState,
    agent_id: Uuid,
    project_id: Option<&str>,
    owner_chain: bool,
) -> hmesh_core::Result<Place> {
    let Some(id) = project_id else {
        return Ok(Place::default());
    };
    validate_project_id(id)?;
    let project = projects(s)?
        .into_iter()
        .find(|p| p.project_id == id)
        .ok_or_else(|| MeshError::new(ErrorCode::ProjectNotFound, "no such project"))?;
    let own = project.agent_ids.contains(&agent_id);
    if !owner_chain && !project.shared && !own {
        return Err(MeshError::new(
            ErrorCode::PermissionDenied,
            "that project is not shared with other computers",
        ));
    }
    if own {
        return Ok(Place {
            project_id: Some(project.project_id),
            ..Place::default()
        });
    }
    if !project.path.is_dir() {
        return Err(MeshError::new(
            ErrorCode::ProjectNotFound,
            "the project folder is no longer on this computer",
        ));
    }
    Ok(Place {
        cwd: Some(project.path),
        project_id: Some(project.project_id),
        ..Place::default()
    })
}

/// Whether a session a paired computer started may still use its folder.
pub(super) fn still_shared(s: &AppState, cwd: &FilePath) -> bool {
    s.store
        .project_at(cwd)
        .ok()
        .flatten()
        .is_some_and(|p| p.shared)
}

/// Start the agent in `cwd` (or its own directory) and read its sessions.
async fn read_history(
    s: &AppState,
    card: &AgentCard,
    config: &AgentConfig,
    cwd: Option<&FilePath>,
) -> AgentHistory {
    let mut history = AgentHistory {
        agent_id: card.agent_id,
        agent_name: card.name.clone(),
        harness: card.harness,
        state: HistoryState::Listed,
        message: None,
        sessions: vec![],
        truncated: false,
    };
    let fail = |mut history: AgentHistory, message: String| {
        history.state = HistoryState::Failed;
        history.message = Some(message);
        history
    };
    let mut launch = config.clone();
    if let Some(cwd) = cwd {
        launch.project.root = cwd.to_path_buf();
    }
    let service = match service::load(&s.instance.dir, card.agent_id) {
        Ok(service) => service,
        Err(e) => return fail(history, e.to_string()),
    };
    let relay = |endpoint: &str, key: &str, protocol| s.relay.route(endpoint, key, protocol);
    let command = match hmesh_runtime::agent_launch(
        &launch,
        &s.instance.dir,
        card.agent_id,
        service.as_ref(),
        None,
        Some(&relay),
    ) {
        Ok((command, _)) => command,
        Err(e) => return fail(history, e.to_string()),
    };
    let listed = tokio::time::timeout(LIST_TIMEOUT, async {
        let (conn, mut messages) = AcpConnection::start(command).await?;
        // Nothing the agent says while listing is shown.
        tokio::spawn(async move { while messages.recv().await.is_some() {} });
        if !acp::lists_sessions(&conn.initialize) {
            conn.shutdown().await;
            return Ok(None);
        }
        let mut sessions = vec![];
        let mut cursor: Option<String> = None;
        let mut truncated = false;
        // Some adapters (pi-acp) rescan every session file for each page, so a
        // folder with many sessions can cost 15 s per page. One slow page is
        // enough: list what the first page gave and say it is partial instead
        // of letting the whole read hit the timeout.
        let mut slow_page = false;
        for page in 0.. {
            let started = Instant::now();
            let result = match conn.list_sessions(cwd, cursor.as_deref()).await {
                Ok(result) => result,
                Err(e) => {
                    conn.shutdown().await;
                    return Err(e);
                }
            };
            slow_page |= started.elapsed() >= SLOW_PAGE;
            sessions.extend(result["sessions"].as_array().cloned().unwrap_or_default());
            cursor = result["nextCursor"].as_str().map(str::to_owned);
            if cursor.is_none() {
                break;
            }
            if slow_page || page + 1 >= MAX_LIST_PAGES || sessions.len() >= MAX_LISTED {
                truncated = true;
                break;
            }
        }
        conn.shutdown().await;
        Ok::<_, MeshError>(Some((sessions, truncated)))
    })
    .await;
    let (raw, truncated) = match listed {
        Ok(Ok(Some(listed))) => listed,
        Ok(Ok(None)) => {
            history.state = HistoryState::Unsupported;
            return history;
        }
        Ok(Err(e)) => return fail(history, e.message),
        Err(_) => {
            return fail(
                history,
                format!(
                    "the agent did not list its sessions within {} s",
                    LIST_TIMEOUT.as_secs()
                ),
            );
        }
    };
    history.truncated = truncated || raw.len() > MAX_LISTED;
    let secrets = agent_secrets(s, card.agent_id);
    for entry in raw.into_iter().take(MAX_LISTED) {
        let (Some(id), Some(folder)) = (entry["sessionId"].as_str(), entry["cwd"].as_str()) else {
            continue;
        };
        if validate_agent_session_id(id).is_err() || !FilePath::new(folder).is_absolute() {
            continue;
        }
        let title = entry["title"]
            .as_str()
            .map(|t| t.chars().filter(|c| !c.is_control()).collect::<String>())
            .map(|t| clip(&redact(&secrets, t.trim()), 400).0)
            .filter(|t| !t.is_empty());
        history.sessions.push(AgentSessionInfo {
            agent_session_id: id.to_owned(),
            cwd: same_folder(FilePath::new(folder)),
            title,
            updated_at: entry["updatedAt"]
                .as_str()
                .map(|t| clip(t, 64).0)
                .filter(|t| !t.is_empty()),
            session_id: None,
        });
    }
    history
}

/// One agent's sessions in `cwd` (or everywhere), reused for 30 seconds.
async fn agent_history(
    s: &AppState,
    card: &AgentCard,
    config: &AgentConfig,
    cwd: Option<&FilePath>,
    refresh: bool,
) -> AgentHistory {
    let key = (card.agent_id, cwd.map(FilePath::to_path_buf));
    let cached = s
        .sessions
        .history
        .entries
        .lock()
        .ok()
        .and_then(|cache| cache.get(&key).cloned());
    if !refresh
        && let Some((at, history)) = &cached
        && at.elapsed() < HISTORY_TTL
    {
        return history.clone();
    }
    let history = read_history(s, card, config, cwd).await;
    // A failed refresh must not erase what the owner saw a moment ago: keep
    // showing the last good list (the panel's refresh button still works).
    if history.state == HistoryState::Failed
        && let Some((_, previous)) = &cached
        && previous.state == HistoryState::Listed
    {
        return previous.clone();
    }
    if let Ok(mut cache) = s.sessions.history.entries.lock() {
        cache.retain(|_, (at, _)| at.elapsed() < HISTORY_TTL);
        cache.insert(key, (Instant::now(), history.clone()));
    }
    history
}

/// Read every usable local agent's list in parallel. `cwd` limits it to one
/// folder; agents registered in that folder come first.
async fn histories(s: &AppState, cwd: Option<&FilePath>, refresh: bool) -> Vec<AgentHistory> {
    let mut agents = vec![];
    for card in s.store.agents().unwrap_or_default() {
        if card.availability == "disabled" {
            continue;
        }
        if let Ok((_, config)) = s.store.agent(card.agent_id) {
            agents.push((card, config));
        }
    }
    agents.sort_by_key(|(card, config)| {
        let here = cwd.is_some_and(|cwd| same_folder(&config.project.root) == cwd);
        (!here, card.name.clone())
    });
    let mut reads = tokio::task::JoinSet::new();
    for (index, (card, config)) in agents.into_iter().enumerate() {
        let s = s.clone();
        let cwd = cwd.map(FilePath::to_path_buf);
        reads.spawn(async move {
            (
                index,
                agent_history(&s, &card, &config, cwd.as_deref(), refresh).await,
            )
        });
    }
    let mut out = vec![];
    while let Some(Ok(read)) = reads.join_next().await {
        out.push(read);
    }
    out.sort_by_key(|(index, _)| *index);
    out.into_iter().map(|(_, history)| history).collect()
}

/// Point listed sessions at the HarnessMesh sessions that continue them.
fn link_sessions(s: &AppState, agents: &mut [AgentHistory]) {
    let open: Vec<SessionRecord> = s.store.sessions(1000).unwrap_or_default();
    for agent in agents {
        for listed in &mut agent.sessions {
            listed.session_id = open
                .iter()
                .filter(|r| {
                    r.agent_id == agent.agent_id
                        && r.agent_session_id.as_deref() == Some(listed.agent_session_id.as_str())
                })
                .max_by_key(|r| (r.status.is_open(), r.updated_at))
                .map(|r| r.session_id);
        }
    }
}

#[derive(Deserialize)]
struct HistoryQuery {
    #[serde(default)]
    refresh: bool,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct FolderQuery {
    path: PathBuf,
    #[serde(default)]
    refresh: bool,
}

/// Each local agent's sessions in `folder`. Some agents ignore the folder
/// filter, and two agents of one kind share their sessions: each session is
/// shown once, under the first agent, and only when it was in that folder.
async fn sessions_in(s: &AppState, folder: &FilePath, refresh: bool) -> Vec<AgentHistory> {
    let mut agents = histories(s, Some(folder), refresh).await;
    let mut seen = std::collections::HashSet::new();
    for agent in &mut agents {
        let harness = agent.harness;
        agent.sessions.retain(|listed| {
            (listed.cwd == folder || same_folder(&listed.cwd) == folder)
                && seen.insert((harness, listed.agent_session_id.clone()))
        });
    }
    link_sessions(s, &mut agents);
    agents
}

async fn list_projects(
    State(s): State<AppState>,
    Extension(c): Extension<ConnectionIdentity>,
    headers: HeaderMap,
) -> ApiResult<Vec<Project>> {
    s.owner(&headers, &c)?;
    Ok(Json(projects(&s)?))
}
async fn add_project(
    State(s): State<AppState>,
    Extension(c): Extension<ConnectionIdentity>,
    headers: HeaderMap,
    Json(body): Json<AddProject>,
) -> ApiResult<Project> {
    s.owner(&headers, &c)?;
    let path = real_folder(&body.path)?;
    let name = match &body.name {
        Some(name) => clean_name(name)?,
        None => clean_name(&folder_name(&path)).unwrap_or_else(|_| "project".into()),
    };
    // An agent's directory is already a project.
    if let Some(existing) = projects(&s)?.into_iter().find(|p| p.path == path) {
        return Err(err(
            ErrorCode::TaskConflict,
            format!("this folder is already the project {:?}", existing.name),
        ));
    }
    let project = s.store.add_project(&Project {
        project_id: format!("p-{}", &Uuid::new_v4().simple().to_string()[..12]),
        name,
        path,
        source: ProjectSource::Added,
        pinned: false,
        hidden: false,
        shared: false,
        position: 0,
        created_at: now(),
        agent_ids: vec![],
    })?;
    Ok(Json(project))
}
async fn update_project(
    State(s): State<AppState>,
    Extension(c): Extension<ConnectionIdentity>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Json(body): Json<UpdateProject>,
) -> ApiResult<Project> {
    s.owner(&headers, &c)?;
    validate_project_id(&id)?;
    let name = body.name.as_deref().map(clean_name).transpose()?;
    projects(&s)?;
    let mut project = s.store.update_project(&id, |p| {
        if let Some(name) = name {
            p.name = name;
        }
        if let Some(pinned) = body.pinned {
            p.pinned = pinned;
        }
        if let Some(hidden) = body.hidden {
            p.hidden = hidden;
        }
        if let Some(shared) = body.shared {
            p.shared = shared;
        }
    })?;
    if let Some(by) = body.move_by.filter(|by| *by != 0) {
        s.store.move_project(&id, by.clamp(-1000, 1000))?;
        project = s.store.project(&id)?;
    }
    project.agent_ids = projects(&s)?
        .into_iter()
        .find(|p| p.project_id == id)
        .map(|p| p.agent_ids)
        .unwrap_or_default();
    Ok(Json(project))
}
async fn remove_project(
    State(s): State<AppState>,
    Extension(c): Extension<ConnectionIdentity>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> ApiResult<Value> {
    s.owner(&headers, &c)?;
    validate_project_id(&id)?;
    let agent_present = projects(&s)?
        .iter()
        .any(|p| p.project_id == id && !p.agent_ids.is_empty());
    s.store.remove_project(&id, agent_present)?;
    Ok(Json(json!({})))
}
async fn project_history(
    State(s): State<AppState>,
    Extension(c): Extension<ConnectionIdentity>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Query(q): Query<HistoryQuery>,
) -> ApiResult<ProjectHistory> {
    s.owner(&headers, &c)?;
    validate_project_id(&id)?;
    let project = projects(&s)?
        .into_iter()
        .find(|p| p.project_id == id)
        .ok_or_else(|| err(ErrorCode::ProjectNotFound, "no such project"))?;
    let agents = sessions_in(&s, &project.path, q.refresh).await;
    Ok(Json(ProjectHistory {
        project_id: project.project_id,
        agents,
        observed_at: now(),
    }))
}
/// What a folder holds before it is added: picked in this computer's own
/// folder dialog, typed, or chosen from the agents' folders.
async fn folder_history(
    State(s): State<AppState>,
    Extension(c): Extension<ConnectionIdentity>,
    headers: HeaderMap,
    Query(q): Query<FolderQuery>,
) -> ApiResult<FolderHistory> {
    s.owner(&headers, &c)?;
    let path = real_folder(&q.path)?;
    let agents = sessions_in(&s, &path, q.refresh).await;
    let project_id = projects(&s)?
        .into_iter()
        .find(|p| p.path == path)
        .map(|p| p.project_id);
    Ok(Json(FolderHistory {
        name: folder_name(&path),
        path,
        project_id,
        agents,
        observed_at: now(),
    }))
}
async fn history_folders(
    State(s): State<AppState>,
    Extension(c): Extension<ConnectionIdentity>,
    headers: HeaderMap,
    Query(q): Query<HistoryQuery>,
) -> ApiResult<HistoryFolders> {
    s.owner(&headers, &c)?;
    let mut agents = histories(&s, None, q.refresh).await;
    let known = projects(&s)?;
    let mut folders: BTreeMap<PathBuf, HistoryFolder> = BTreeMap::new();
    let mut seen = std::collections::HashSet::new();
    for agent in &agents {
        for listed in &agent.sessions {
            if !seen.insert((agent.harness, listed.agent_session_id.clone()))
                || !listed.cwd.is_dir()
            {
                continue;
            }
            let folder = folders
                .entry(listed.cwd.clone())
                .or_insert_with(|| HistoryFolder {
                    path: listed.cwd.clone(),
                    name: folder_name(&listed.cwd),
                    sessions: 0,
                    latest: None,
                    agents: vec![],
                    project_id: known
                        .iter()
                        .find(|p| p.path == listed.cwd)
                        .map(|p| p.project_id.clone()),
                });
            folder.sessions += 1;
            if listed.updated_at > folder.latest {
                folder.latest = listed.updated_at.clone();
            }
            if !folder.agents.contains(&agent.agent_name) {
                folder.agents.push(agent.agent_name.clone());
            }
        }
    }
    for agent in &mut agents {
        agent.sessions.clear();
    }
    let mut folders: Vec<HistoryFolder> = folders.into_values().collect();
    folders.sort_by(|a, b| b.latest.cmp(&a.latest).then(a.path.cmp(&b.path)));
    Ok(Json(HistoryFolders {
        folders,
        agents,
        observed_at: now(),
    }))
}
async fn mark_session(
    State(s): State<AppState>,
    Extension(c): Extension<ConnectionIdentity>,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
    Json(body): Json<UpdateSessionMark>,
) -> ApiResult<SessionMark> {
    s.owner(&headers, &c)?;
    if s.store.session(id).is_err() && s.store.remote_session_peer(id)?.is_none() {
        return Err(err(ErrorCode::TaskUnknown, "unknown session"));
    }
    let title = match body.title.as_deref() {
        Some(title) if title.trim().is_empty() => Some(None),
        Some(title) => Some(Some(clean_name(title)?)),
        None => None,
    };
    Ok(Json(s.store.mark_session(id, now(), |mark| {
        if let Some(title) = title {
            mark.title = title;
        }
        if let Some(pinned) = body.pinned {
            mark.pinned = pinned;
        }
        if let Some(hidden) = body.hidden {
            mark.hidden = hidden;
        }
    })?))
}
/// The projects a paired computer may open sessions in: names, never paths.
async fn shared_projects(
    State(s): State<AppState>,
    Extension(c): Extension<ConnectionIdentity>,
    headers: HeaderMap,
) -> ApiResult<Vec<SharedProject>> {
    let actor = s.actor(&headers, &c)?;
    if !actor.owner && s.task_decision(actor) == Decision::Denied {
        return Err(err(
            ErrorCode::PermissionDenied,
            "sender is not a currently paired peer",
        ));
    }
    Ok(Json(
        projects(&s)?
            .into_iter()
            // Hidden projects are not offered, here as on this computer.
            .filter(|p| p.shared && !p.hidden)
            .map(|p| SharedProject {
                project_id: p.project_id,
                name: p.name,
            })
            .collect(),
    ))
}

pub(super) fn routes() -> Router<AppState> {
    Router::new()
        .route("/local/projects", get(list_projects).post(add_project))
        .route("/local/projects/{id}", post(update_project))
        .route("/local/projects/{id}/remove", post(remove_project))
        .route("/local/projects/{id}/history", get(project_history))
        .route("/local/history", get(history_folders))
        .route("/local/history/folder", get(folder_history))
        .route("/local/sessions/{id}/mark", post(mark_session))
        .route("/hmesh/v1/projects", get(shared_projects))
}
