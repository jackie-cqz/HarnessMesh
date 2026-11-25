//! Projects (folders sessions can work in) and this computer's marks on sessions.
use super::{Store, internal};
use hmesh_core::projects::{MAX_PROJECTS, Project, ProjectSource, SessionMark};
use hmesh_core::*;
use rusqlite::{OptionalExtension, Row, params};
use std::path::{Path, PathBuf};
use uuid::Uuid;

const COLUMNS: &str =
    "project_id, path, name, source, pinned, hidden, shared, position, created_at";
/// Pinned first, then the owner's order, then oldest first.
const ORDER: &str = "ORDER BY pinned DESC, position ASC, created_at ASC, project_id ASC";

fn source_text(source: ProjectSource) -> &'static str {
    match source {
        ProjectSource::Added => "added",
        ProjectSource::Agent => "agent",
    }
}
fn project(row: &Row<'_>) -> rusqlite::Result<Project> {
    Ok(Project {
        project_id: row.get(0)?,
        path: PathBuf::from(row.get::<_, String>(1)?),
        name: row.get(2)?,
        source: if row.get::<_, String>(3)? == "agent" {
            ProjectSource::Agent
        } else {
            ProjectSource::Added
        },
        pinned: row.get(4)?,
        hidden: row.get(5)?,
        shared: row.get(6)?,
        position: row.get(7)?,
        created_at: row.get(8)?,
        agent_ids: vec![],
    })
}
fn not_found() -> MeshError {
    MeshError::new(
        ErrorCode::ProjectNotFound,
        "no such project on this computer",
    )
}
fn path_text(path: &Path) -> Result<String> {
    path.to_str()
        .map(str::to_owned)
        .ok_or_else(|| MeshError::input("the folder path is not valid UTF-8"))
}

impl Store {
    /// Every project, in list order.
    pub fn projects(&self) -> Result<Vec<Project>> {
        self.read(|c| {
            let mut q = c
                .prepare(&format!("SELECT {COLUMNS} FROM projects {ORDER}"))
                .map_err(internal)?;
            let rows = q.query_map([], project).map_err(internal)?;
            rows.map(|r| r.map_err(internal)).collect()
        })
    }
    pub fn project(&self, id: &str) -> Result<Project> {
        self.read(|c| {
            c.query_row(
                &format!("SELECT {COLUMNS} FROM projects WHERE project_id=?1"),
                [id],
                project,
            )
            .optional()
            .map_err(internal)?
            .ok_or_else(not_found)
        })
    }
    pub fn project_at(&self, path: &Path) -> Result<Option<Project>> {
        let path = path_text(path)?;
        self.read(|c| {
            c.query_row(
                &format!("SELECT {COLUMNS} FROM projects WHERE path=?1"),
                [path],
                project,
            )
            .optional()
            .map_err(internal)
        })
    }
    /// Add a project at the end of the list. Refuses a folder or ID that is
    /// already a project, and more than [`MAX_PROJECTS`].
    pub fn add_project(&self, project: &Project) -> Result<Project> {
        let path = path_text(&project.path)?;
        self.transaction(|tx| {
            let (count, last): (i64, Option<i64>) = tx
                .query_row("SELECT count(*), max(position) FROM projects", [], |r| {
                    Ok((r.get(0)?, r.get(1)?))
                })
                .map_err(internal)?;
            if count as usize >= MAX_PROJECTS {
                return Err(MeshError::new(
                    ErrorCode::RateLimited,
                    format!("at most {MAX_PROJECTS} projects can be kept"),
                ));
            }
            let taken: bool = tx
                .query_row(
                    "SELECT EXISTS(SELECT 1 FROM projects WHERE path=?1 OR project_id=?2)",
                    params![path, project.project_id],
                    |r| r.get(0),
                )
                .map_err(internal)?;
            if taken {
                return Err(MeshError::new(
                    ErrorCode::TaskConflict,
                    "this folder is already a project",
                ));
            }
            let mut added = project.clone();
            added.position = last.map_or(0, |p| p + 1);
            tx.execute(
                &format!("INSERT INTO projects({COLUMNS}) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9)"),
                params![
                    added.project_id,
                    path,
                    added.name,
                    source_text(added.source),
                    added.pinned,
                    added.hidden,
                    added.shared,
                    added.position,
                    added.created_at
                ],
            )
            .map_err(internal)?;
            Ok(added)
        })
    }
    /// Change a project's name, pin, visibility or sharing.
    pub fn update_project(&self, id: &str, change: impl FnOnce(&mut Project)) -> Result<Project> {
        self.transaction(|tx| {
            let mut current = tx
                .query_row(
                    &format!("SELECT {COLUMNS} FROM projects WHERE project_id=?1"),
                    [id],
                    project,
                )
                .optional()
                .map_err(internal)?
                .ok_or_else(not_found)?;
            change(&mut current);
            tx.execute(
                "UPDATE projects SET name=?2, pinned=?3, hidden=?4, shared=?5 WHERE project_id=?1",
                params![
                    id,
                    current.name,
                    current.pinned,
                    current.hidden,
                    current.shared
                ],
            )
            .map_err(internal)?;
            Ok(current)
        })
    }
    /// Move a project `by` places in the list (negative moves it up), among
    /// the projects with the same pin.
    pub fn move_project(&self, id: &str, by: i64) -> Result<()> {
        self.transaction(|tx| {
            let mut q = tx
                .prepare(&format!("SELECT {COLUMNS} FROM projects {ORDER}"))
                .map_err(internal)?;
            let mut list: Vec<Project> = q
                .query_map([], project)
                .map_err(internal)?
                .collect::<rusqlite::Result<_>>()
                .map_err(internal)?;
            drop(q);
            let from = list
                .iter()
                .position(|p| p.project_id == id)
                .ok_or_else(not_found)?;
            let pinned = list[from].pinned;
            let group: Vec<usize> = (0..list.len())
                .filter(|&i| list[i].pinned == pinned)
                .collect();
            let at = group.iter().position(|&i| i == from).unwrap_or(0) as i64;
            let to = (at + by).clamp(0, group.len() as i64 - 1) as usize;
            let moved = list.remove(from);
            let target = group[to];
            list.insert(target, moved);
            for (position, p) in list.iter().enumerate() {
                tx.execute(
                    "UPDATE projects SET position=?2 WHERE project_id=?1",
                    params![p.project_id, position as i64],
                )
                .map_err(internal)?;
            }
            Ok(())
        })
    }
    /// Remove a project the owner added; agent directories can only be hidden.
    pub fn remove_project(&self, id: &str) -> Result<()> {
        self.transaction(|tx| {
            let source: Option<String> = tx
                .query_row(
                    "SELECT source FROM projects WHERE project_id=?1",
                    [id],
                    |r| r.get(0),
                )
                .optional()
                .map_err(internal)?;
            match source.as_deref() {
                None => Err(not_found()),
                Some("agent") => Err(MeshError::new(
                    ErrorCode::PermissionDenied,
                    "an agent's registered directory can be hidden but not removed",
                )),
                Some(_) => {
                    tx.execute("DELETE FROM projects WHERE project_id=?1", [id])
                        .map_err(internal)?;
                    Ok(())
                }
            }
        })
    }

    pub fn session_marks(&self) -> Result<Vec<SessionMark>> {
        self.read(|c| {
            let mut q = c
                .prepare("SELECT session_id, title, pinned, hidden, updated_at FROM session_marks")
                .map_err(internal)?;
            let rows = q
                .query_map([], |r| {
                    Ok((
                        r.get::<_, String>(0)?,
                        r.get::<_, Option<String>>(1)?,
                        r.get::<_, bool>(2)?,
                        r.get::<_, bool>(3)?,
                        r.get::<_, i64>(4)?,
                    ))
                })
                .map_err(internal)?;
            let mut marks = vec![];
            for row in rows {
                let (id, title, pinned, hidden, updated_at) = row.map_err(internal)?;
                marks.push(SessionMark {
                    session_id: Uuid::parse_str(&id).map_err(internal)?,
                    title,
                    pinned,
                    hidden,
                    updated_at,
                });
            }
            Ok(marks)
        })
    }
    /// Change this computer's marks on a session; a mark with nothing set is
    /// removed.
    pub fn mark_session(
        &self,
        id: Uuid,
        at: i64,
        change: impl FnOnce(&mut SessionMark),
    ) -> Result<SessionMark> {
        self.transaction(|tx| {
            let mut mark = tx
                .query_row(
                    "SELECT title, pinned, hidden, updated_at FROM session_marks WHERE session_id=?1",
                    [id.to_string()],
                    |r| {
                        Ok(SessionMark {
                            session_id: id,
                            title: r.get(0)?,
                            pinned: r.get(1)?,
                            hidden: r.get(2)?,
                            updated_at: r.get(3)?,
                        })
                    },
                )
                .optional()
                .map_err(internal)?
                .unwrap_or(SessionMark {
                    session_id: id,
                    ..Default::default()
                });
            change(&mut mark);
            mark.updated_at = at;
            if mark.title.is_none() && !mark.pinned && !mark.hidden {
                tx.execute(
                    "DELETE FROM session_marks WHERE session_id=?1",
                    [id.to_string()],
                )
                .map_err(internal)?;
            } else {
                tx.execute(
                    "INSERT INTO session_marks VALUES(?1,?2,?3,?4,?5) ON CONFLICT(session_id) DO UPDATE SET title=?2, pinned=?3, hidden=?4, updated_at=?5",
                    params![id.to_string(), mark.title, mark.pinned, mark.hidden, at],
                )
                .map_err(internal)?;
            }
            Ok(mark)
        })
    }
}
