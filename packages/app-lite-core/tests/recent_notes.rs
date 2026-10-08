use app_lite_core::{CanonicalDocument, CreateNote, LibraryRepository, LibraryShellState, NoteId};
use rusqlite::Connection;

const KEY: &str = "library-recent-note-ids";

fn create(repo: &LibraryRepository, title: &str) -> NoteId {
    repo.create_note(CreateNote { title: title.into(), notebook_id: None,
        document: CanonicalDocument::default() }).unwrap().id
}

fn select(repo: &LibraryRepository, id: &NoteId) {
    repo.write_library_shell_state(&LibraryShellState {
        selected_note_id: Some(id.clone()), ..Default::default()
    }).unwrap();
}

#[test]
fn successful_selection_records_deduplicated_recent_notes_and_reopens_without_content_mutation() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("library.sqlite");
    let repo = LibraryRepository::open(&path).unwrap();
    let a = create(&repo, "A");
    let b = create(&repo, "B");
    let sql = Connection::open(&path).unwrap();
    let before: Vec<(String, i64, i64)> = sql.prepare("SELECT id,revision,updated_time FROM notes ORDER BY id").unwrap()
        .query_map([], |r| Ok((r.get(0)?,r.get(1)?,r.get(2)?))).unwrap().collect::<Result<_,_>>().unwrap();
    let outbox: i64 = sql.query_row("SELECT count(*) FROM sync_outbox", [], |r| r.get(0)).unwrap();
    select(&repo, &a); select(&repo, &b); select(&repo, &a);
    let value = repo.read_setting(KEY).unwrap().expect("successful note opens need persistent MRU, not recent searches");
    assert_eq!(serde_json::from_str::<Vec<String>>(&value).unwrap(), vec![a.as_str(), b.as_str()]);
    assert_eq!(repo.list_recent_notes(16).unwrap().iter().map(|n| n.id.clone()).collect::<Vec<_>>(), vec![a.clone(), b.clone()]);
    drop(repo);
    let reopened = LibraryRepository::open(&path).unwrap();
    assert_eq!(reopened.read_setting(KEY).unwrap(), Some(value));
    assert_eq!(reopened.list_recent_notes(1).unwrap()[0].id, a);
    assert!(reopened.list_recent_notes(0).unwrap().is_empty());
    let after: Vec<(String, i64, i64)> = sql.prepare("SELECT id,revision,updated_time FROM notes ORDER BY id").unwrap()
        .query_map([], |r| Ok((r.get(0)?,r.get(1)?,r.get(2)?))).unwrap().collect::<Result<_,_>>().unwrap();
    assert_eq!(before, after);
    assert_eq!(outbox, sql.query_row("SELECT count(*) FROM sync_outbox", [], |r| r.get::<_,i64>(0)).unwrap());
}

#[test]
fn recent_notes_are_bounded_and_missing_or_trashed_selection_does_not_enter_mru() {
    let root = tempfile::tempdir().unwrap();
    let repo = LibraryRepository::open(root.path().join("library.sqlite")).unwrap();
    let ids: Vec<_> = (0..20).map(|n| create(&repo, &format!("Note{n}"))).collect();
    for id in &ids { select(&repo, id); }
    let value = repo.read_setting(KEY).unwrap().expect("bounded recent notes must be recorded");
    assert_eq!(serde_json::from_str::<Vec<String>>(&value).unwrap(), ids[4..].iter().rev().map(|id| id.as_str()).collect::<Vec<_>>());
    let trashed = repo.load_note(&ids[0]).unwrap().unwrap();
    repo.trash_note(&trashed.id).unwrap();
    select(&repo, &trashed.id);
    select(&repo, &NoteId::parse("11111111111111111111111111111111").unwrap());
    assert_eq!(repo.read_setting(KEY).unwrap(), Some(value));
    repo.trash_note(&ids[19]).unwrap();
    assert_eq!(repo.list_recent_notes(usize::MAX).unwrap().iter().map(|n| n.id.clone()).collect::<Vec<_>>(), ids[4..19].iter().rev().cloned().collect::<Vec<_>>());
}

#[test]
fn recent_note_failure_rolls_back_selection_and_panes_together() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("library.sqlite");
    let repo = LibraryRepository::open(&path).unwrap();
    let id = create(&repo, "Transactional");
    let before = LibraryShellState::default();
    repo.write_library_shell_state(&before).unwrap();
    let sql = Connection::open(&path).unwrap();
    sql.execute_batch("CREATE TRIGGER fail_recent BEFORE INSERT ON settings WHEN NEW.key='library-recent-note-ids' BEGIN SELECT RAISE(ABORT,'owned MRU fault'); END;").unwrap();
    assert!(repo.write_library_shell_state(&LibraryShellState {
        selected_note_id: Some(id), sidebar_width: 320, ..before.clone()
    }).is_err(), "MRU must commit atomically with the actual selected note");
    assert_eq!(repo.read_library_shell_state().unwrap(), before);
    assert_eq!(repo.read_setting(KEY).unwrap(), None);
}
