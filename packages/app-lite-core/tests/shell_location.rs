use app_lite_core::{LibraryRepository, LibraryRoute, LibraryShellLocation, LibraryShellState, NoteId, NotebookId, StackId, TagId};
use rusqlite::Connection;

fn fixture() -> (tempfile::TempDir, LibraryRepository, Connection) {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("library.sqlite");
    let repository = LibraryRepository::open(&path).unwrap();
    let raw = Connection::open(path).unwrap();
    (root, repository, raw)
}

#[test]
fn shell_location_roundtrips_complete_destinations_and_legacy_default() {
    let (_root, repository, raw) = fixture();
    let first = "11111111111111111111111111111111";
    let second = "22222222222222222222222222222222";
    let locations = [
        LibraryShellLocation::Browse(LibraryRoute::Notebook(NotebookId::parse(first).unwrap())),
        LibraryShellLocation::Browse(LibraryRoute::Stack(StackId::parse(first).unwrap())),
        LibraryShellLocation::Browse(LibraryRoute::tags(vec![TagId::parse(first).unwrap(), TagId::parse(second).unwrap()]).unwrap()),
        LibraryShellLocation::Browse(LibraryRoute::Trash),
        LibraryShellLocation::Search("会议 tag:\"我的标签\"".into()),
        LibraryShellLocation::default(),
    ];
    for location in locations {
        let state = LibraryShellState {
            location, selected_note_id: Some(NoteId::parse(first).unwrap()),
            sidebar_width: 280, list_visible: false, ..Default::default()
        };
        repository.write_library_shell_state(&state).unwrap();
        assert_eq!(repository.read_library_shell_state().unwrap(), state);
    }
    assert_eq!(raw.query_row("SELECT count(*) FROM settings WHERE key='library-shell.location-v1'", [], |row| row.get::<_, i64>(0)).unwrap(), 0);
}

#[test]
fn shell_location_corrupt_or_future_state_keeps_legacy_panes_and_selection() {
    let (_root, repository, raw) = fixture();
    let state = LibraryShellState {
        selected_note_id: Some(NoteId::parse("11111111111111111111111111111111").unwrap()),
        sidebar_width: 280, sidebar_visible: false, ..Default::default()
    };
    repository.write_library_shell_state(&state).unwrap();
    for corrupt in [
        "not JSON", "{}", "{\"version\":2,\"destination\":{\"kind\":\"trash\"}}",
        "{\"version\":1,\"destination\":{\"kind\":\"notebook\",\"value\":\"invalid\"}}",
        "{\"version\":1,\"destination\":{\"kind\":\"tags\",\"value\":[]}}",
        "{\"version\":1,\"destination\":{\"kind\":\"search\",\"value\":\"  \"}}",
    ] {
        raw.execute("INSERT OR REPLACE INTO settings VALUES('library-shell.location-v1',?1,1)", [corrupt]).unwrap();
        assert_eq!(repository.read_library_shell_state().unwrap(), state);
    }
    let oversized = "x".repeat(16 * 1024 + 1);
    raw.execute("UPDATE settings SET value=?1 WHERE key='library-shell.location-v1'", [oversized]).unwrap();
    assert_eq!(repository.read_library_shell_state().unwrap(), state);
}

#[test]
fn shell_location_invalid_write_leaves_previous_generation_unchanged() {
    let (_root, repository, _raw) = fixture();
    let before = LibraryShellState { location: LibraryShellLocation::Browse(LibraryRoute::Trash), ..Default::default() };
    repository.write_library_shell_state(&before).unwrap();
    for location in [
        LibraryShellLocation::Search(" ".into()),
        LibraryShellLocation::Search("x".repeat(4097)),
        LibraryShellLocation::Browse(LibraryRoute::Tags(Default::default())),
    ] {
        assert!(repository.write_library_shell_state(&LibraryShellState {
            sidebar_width: 320, location, ..before.clone()
        }).is_err());
        assert_eq!(repository.read_library_shell_state().unwrap(), before);
    }
    assert!(repository.read_setting("library-shell.location-v1").is_err());
    assert!(repository.write_setting("library-shell.location-v1", "bypass").is_err());
}

#[test]
fn shell_location_sql_failure_rolls_back_panes_selection_and_destination() {
    let (_root, repository, raw) = fixture();
    let before = LibraryShellState::default();
    repository.write_library_shell_state(&before).unwrap();
    raw.execute_batch("CREATE TRIGGER fail_location BEFORE INSERT ON settings WHEN NEW.key='library-shell.location-v1' BEGIN SELECT RAISE(ABORT,'owned transaction fault'); END;").unwrap();
    assert!(repository.write_library_shell_state(&LibraryShellState {
        sidebar_width: 320,
        selected_note_id: Some(NoteId::parse("11111111111111111111111111111111").unwrap()),
        location: LibraryShellLocation::Search("会议".into()), ..Default::default()
    }).is_err());
    assert_eq!(repository.read_library_shell_state().unwrap(), before);
}
