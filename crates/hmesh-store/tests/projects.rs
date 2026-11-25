use hmesh_core::ErrorCode;
use hmesh_core::projects::{Project, ProjectSource};
use hmesh_store::Store;
use std::path::PathBuf;
use uuid::Uuid;

fn project(id: &str, path: &str, source: ProjectSource) -> Project {
    Project {
        project_id: id.into(),
        name: id.into(),
        path: PathBuf::from(path),
        source,
        pinned: false,
        hidden: false,
        shared: false,
        position: 0,
        created_at: 1,
        agent_ids: vec![],
    }
}
fn ids(store: &Store) -> Vec<String> {
    store
        .projects()
        .unwrap()
        .into_iter()
        .map(|p| p.project_id)
        .collect()
}

#[test]
fn projects_keep_one_row_per_folder_in_the_owners_order() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(&dir.path().join("state.sqlite3")).unwrap();
    store
        .add_project(&project("a", "/work/a", ProjectSource::Added))
        .unwrap();
    store
        .add_project(&project("b", "/work/b", ProjectSource::Agent))
        .unwrap();
    store
        .add_project(&project("c", "/work/c", ProjectSource::Added))
        .unwrap();
    // The same folder, or the same ID, is refused.
    let again = store
        .add_project(&project("d", "/work/a", ProjectSource::Added))
        .unwrap_err();
    assert_eq!(again.code, ErrorCode::TaskConflict);
    assert!(
        store
            .add_project(&project("a", "/work/x", ProjectSource::Added))
            .is_err()
    );
    assert_eq!(ids(&store), ["a", "b", "c"]);

    store.move_project("c", -1).unwrap();
    assert_eq!(ids(&store), ["a", "c", "b"]);
    store.move_project("a", 5).unwrap();
    assert_eq!(ids(&store), ["c", "b", "a"]);
    // Pinned projects come first; moving stays within the pinned group.
    store.update_project("a", |p| p.pinned = true).unwrap();
    assert_eq!(ids(&store), ["a", "c", "b"]);
    store.move_project("b", -5).unwrap();
    assert_eq!(ids(&store), ["a", "b", "c"]);
    // Hidden projects do not count: "e" moves above "c", the visible one before it.
    store.update_project("b", |p| p.hidden = true).unwrap();
    store
        .add_project(&project("e", "/work/e", ProjectSource::Added))
        .unwrap();
    store.move_project("e", -1).unwrap();
    assert_eq!(ids(&store), ["a", "b", "e", "c"]);
    store.update_project("b", |p| p.hidden = false).unwrap();
    store.remove_project("e", false).unwrap();

    let changed = store
        .update_project("c", |p| {
            p.name = "Client".into();
            p.hidden = true;
            p.shared = true;
        })
        .unwrap();
    assert!(changed.hidden && changed.shared);
    let read = store.project("c").unwrap();
    assert_eq!(
        (read.name.as_str(), read.hidden, read.shared),
        ("Client", true, true)
    );
    assert_eq!(
        store
            .project_at(&PathBuf::from("/work/b"))
            .unwrap()
            .unwrap()
            .project_id,
        "b"
    );

    // While an agent is registered there, its directory is hidden, not removed.
    assert_eq!(
        store.remove_project("b", true).unwrap_err().code,
        ErrorCode::PermissionDenied
    );
    store.remove_project("c", false).unwrap();
    assert_eq!(
        store.project("c").unwrap_err().code,
        ErrorCode::ProjectNotFound
    );
    // Once no agent is registered there, its directory can go too.
    store.remove_project("b", false).unwrap();
    assert_eq!(ids(&store), ["a"]);
}

#[test]
fn session_marks_are_kept_until_nothing_is_set() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(&dir.path().join("state.sqlite3")).unwrap();
    let id = Uuid::new_v4();
    let mark = store
        .mark_session(id, 5, |m| {
            m.title = Some("Release notes".into());
            m.pinned = true;
        })
        .unwrap();
    assert_eq!(mark.updated_at, 5);
    assert_eq!(store.session_marks().unwrap(), [mark]);
    store.mark_session(id, 6, |m| m.hidden = true).unwrap();
    let kept = &store.session_marks().unwrap()[0];
    assert!(kept.pinned && kept.hidden && kept.title.is_some());
    store
        .mark_session(id, 7, |m| {
            m.title = None;
            m.pinned = false;
            m.hidden = false;
        })
        .unwrap();
    assert!(store.session_marks().unwrap().is_empty());
}
