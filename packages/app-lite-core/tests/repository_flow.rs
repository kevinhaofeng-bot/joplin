use app_lite_core::EditJournalEntry;
use app_lite_core::ResourceId;
use app_lite_core::document::{Block, BlockStyle, Inline};
use app_lite_core::{
    CanonicalDocument, CreateNote, LibraryEvent, LibraryRepository, LibraryShellState, ListQuery,
    NoteId, SaveNote,
};
use rusqlite::Connection;
use std::sync::mpsc::TryRecvError;
use tempfile::tempdir;

fn document(text: &str) -> CanonicalDocument {
    CanonicalDocument::from_blocks(vec![Block::Paragraph {
        style: BlockStyle::default(),
        inlines: vec![Inline::Text {
            text: text.into(),
            marks: Default::default(),
        }],
    }])
}

#[test]
fn library_shell_state_is_typed_strict_and_clears_stale_selection_atomically() {
    // This is deliberately a repository-level test: the UI must never assemble two
    // unrelated string writes and leave a cross-generation pane/selection pair behind.
    let profile = tempdir().unwrap();
    let repository = LibraryRepository::open(profile.path().join("library.sqlite")).unwrap();
    let selected = NoteId::parse("11111111111111111111111111111111").unwrap();
    let state = LibraryShellState {
        sidebar_width: 260,
        list_width: 410,
        sidebar_visible: false,
        list_visible: true,
        selected_note_id: Some(selected.clone()),
    };

    repository.write_library_shell_state(&state).unwrap();
    assert_eq!(repository.read_library_shell_state().unwrap(), state);

    repository
        .write_library_shell_state(&LibraryShellState {
            selected_note_id: None,
            ..state.clone()
        })
        .unwrap();
    assert_eq!(
        repository
            .read_library_shell_state()
            .unwrap()
            .selected_note_id,
        None
    );

    // A malformed field count, a non-boolean flag, or an out-of-range width is
    // never allowed to turn a partially valid setting into invisible panes.
    assert!(
        repository
            .write_library_shell_state(&LibraryShellState {
                sidebar_width: 0,
                ..state.clone()
            })
            .is_err(),
        "the typed writer must reject invalid pane state before it reaches SQLite"
    );
    assert!(
        repository
            .write_setting("library-shell.panes", "260,410,1")
            .is_err(),
        "the generic settings API must not bypass the typed shell-state writer"
    );

    // A raw database write models pre-Task-3/corrupt storage. The reader must
    // still fail closed to defaults; product code has no generic reserved-key
    // escape hatch for creating this state.
    let raw = Connection::open(profile.path().join("library.sqlite")).unwrap();
    raw.execute(
        "INSERT INTO settings (key, value, updated_time) VALUES (?1, ?2, 1)
         ON CONFLICT(key) DO UPDATE SET value = excluded.value",
        ["library-shell.panes", "260,410,1"],
    )
    .unwrap();
    assert_eq!(
        repository.read_library_shell_state().unwrap().pane_state(),
        LibraryShellState::default().pane_state()
    );
    raw.execute(
        "UPDATE settings SET value = ?2 WHERE key = ?1",
        ["library-shell.panes", "260,410,yes,1"],
    )
    .unwrap();
    assert_eq!(
        repository.read_library_shell_state().unwrap().pane_state(),
        LibraryShellState::default().pane_state()
    );
    raw.execute(
        "UPDATE settings SET value = ?2 WHERE key = ?1",
        ["library-shell.panes", "0,410,1,1"],
    )
    .unwrap();
    assert_eq!(
        repository.read_library_shell_state().unwrap().pane_state(),
        LibraryShellState::default().pane_state()
    );
}

#[test]
fn repository_flow_persists_ids_relationships_html_text_and_resource_order() {
    // Catches a repository that stores only cards, rewrites IDs on reopen, or loses body/resource relationships.
    let profile = tempdir().unwrap();
    let database = profile.path().join("library.sqlite");
    let repository = LibraryRepository::open(&database).unwrap();
    let events = repository.subscribe();
    let default_notebook = repository.default_notebook().unwrap();
    let work = repository.create_notebook("工作", None).unwrap();
    let urgent = repository.create_tag("紧急").unwrap();
    let reference = repository.create_tag("参考").unwrap();
    let image = repository
        .import_image(b"image bytes", "receipt", "image/png", "png")
        .unwrap();

    let first = repository
        .create_note(CreateNote {
            title: "第一篇".into(),
            notebook_id: None,
            document: document("原文"),
        })
        .unwrap();
    let second = repository
        .create_note(CreateNote {
            title: "第二篇".into(),
            notebook_id: None,
            document: document("第二"),
        })
        .unwrap();
    let third = repository
        .create_note(CreateNote {
            title: "第三篇".into(),
            notebook_id: None,
            document: document("第三"),
        })
        .unwrap();
    let updated = repository
        .save_note(SaveNote {
            id: first.id.clone(),
            expected_revision: first.revision,
            title: "图文笔记".into(),
            document: CanonicalDocument::from_blocks(vec![Block::Paragraph {
                style: BlockStyle::default(),
                inlines: vec![
                    Inline::Text {
                        text: "收据 ".into(),
                        marks: Default::default(),
                    },
                    Inline::Image {
                        resource_id: image.clone(),
                        alt: "receipt".into(),
                    },
                    Inline::Text {
                        text: " 已保存".into(),
                        marks: Default::default(),
                    },
                ],
            }]),
            resource_ids: vec![image.clone()],
            selected_thumbnail_id: Some(image.clone()),
        })
        .unwrap();
    repository
        .move_notes(&[first.id.clone(), second.id.clone()], &work.id)
        .unwrap();
    repository
        .set_note_tags(&first.id, &[urgent.id.clone(), reference.id.clone()])
        .unwrap();
    repository.trash_note(&second.id).unwrap();
    repository.restore_note(&second.id).unwrap();

    let cards = repository.list_notes(ListQuery::default()).unwrap();
    let card = cards.iter().find(|card| card.id == first.id).unwrap();
    assert_eq!(card.title_prefix, "图文笔记");
    assert_eq!(card.snippet, "收据 receipt 已保存");
    assert_eq!(card.notebook_id, work.id);
    assert_eq!(card.selected_thumbnail_id, Some(image.clone()));
    assert_eq!(card.attachment_count, 1);
    drop(repository);

    let reopened = LibraryRepository::open(&database).unwrap();
    let loaded = reopened.load_note(&first.id).unwrap().unwrap();
    assert_eq!(loaded.id, updated.id);
    assert_eq!(loaded.title, "图文笔记");
    assert_eq!(loaded.body_html, updated.body_html);
    assert_eq!(loaded.body_text, "收据 receipt 已保存");
    assert_eq!(loaded.resource_ids, vec![image]);
    assert_eq!(loaded.tag_ids, vec![urgent.id, reference.id]);
    assert_eq!(
        reopened.load_note(&second.id).unwrap().unwrap().notebook_id,
        work.id
    );
    assert_eq!(
        reopened.load_note(&third.id).unwrap().unwrap().notebook_id,
        default_notebook.id
    );
    let emitted = events.try_iter().collect::<Vec<_>>();
    assert!(emitted.contains(&LibraryEvent::NoteCreated(first.id)));
    assert!(emitted.contains(&LibraryEvent::NoteProjectionChanged(second.id.clone())));
    assert!(emitted.contains(&LibraryEvent::NoteTrashed(second.id.clone())));
    assert!(emitted.contains(&LibraryEvent::NoteRestored(second.id)));
    assert!(emitted.contains(&LibraryEvent::OrganizationChanged));
    assert!(emitted.contains(&LibraryEvent::SearchProjectionQueued(updated.id)));
    assert!(
        emitted
            .iter()
            .any(|event| matches!(event, LibraryEvent::SyncQueued(_)))
    );
}

#[test]
fn list_projection_observation_never_reads_large_body_merge_state_or_blob_bytes() {
    // Catches accidental card-list hydration of an expensive body, merge state, or attachment bytes.
    let profile = tempdir().unwrap();
    let repository = LibraryRepository::open(profile.path().join("library.sqlite")).unwrap();
    repository
        .create_note(CreateNote {
            title: "only card fields".into(),
            notebook_id: None,
            document: document("short text"),
        })
        .unwrap();
    let observation = repository.observe_next_list_query();
    let cards = repository.list_notes(ListQuery::default()).unwrap();
    assert_eq!(cards.len(), 1);
    let columns = observation.recv().unwrap();
    assert!(!columns.iter().any(|column| {
        column.ends_with(".body_html")
            || column.ends_with(".merge_state")
            || column.ends_with(".bytes")
    }));
    assert!(columns.iter().any(|column| column == "notes.id"));
    assert!(
        columns
            .iter()
            .any(|column| column == "note_resources.resource_id")
    );
}

#[test]
fn failed_transaction_never_publishes_its_library_events() {
    // Catches event publication before the durable transaction, which could make a card point at a rolled-back note.
    let profile = tempdir().unwrap();
    let repository = LibraryRepository::open(profile.path().join("library.sqlite")).unwrap();
    let events = repository.subscribe();
    let missing = ResourceId::new("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa").unwrap();
    let document = CanonicalDocument::from_blocks(vec![Block::Paragraph {
        style: BlockStyle::default(),
        inlines: vec![Inline::Image {
            resource_id: missing,
            alt: "missing".into(),
        }],
    }]);
    assert!(
        repository
            .create_note(CreateNote {
                title: "will rollback".into(),
                notebook_id: None,
                document
            })
            .is_err()
    );
    assert_eq!(events.try_recv(), Err(TryRecvError::Empty));
    assert!(
        repository
            .list_notes(ListQuery::default())
            .unwrap()
            .is_empty()
    );
}

#[test]
fn flush_snapshot_compacts_crash_journal_with_the_next_durable_revision() {
    // Catches a snapshot that leaves a stale crash delta able to replay over its committed HTML.
    let profile = tempdir().unwrap();
    let path = profile.path().join("library.sqlite");
    let repository = LibraryRepository::open(&path).unwrap();
    let note = repository
        .create_note(CreateNote {
            title: "journal".into(),
            notebook_id: None,
            document: document("before"),
        })
        .unwrap();
    repository
        .append_edit_journal(EditJournalEntry {
            note_id: note.id.clone(),
            generation: 7,
            delta_utf8: "replace before with after".into(),
        })
        .unwrap();
    assert_eq!(
        Connection::open(&path)
            .unwrap()
            .query_row("SELECT count(*) FROM edit_journal", [], |row| row
                .get::<_, i64>(0))
            .unwrap(),
        1
    );
    let saved = repository
        .flush_snapshot(SaveNote {
            id: note.id,
            expected_revision: note.revision,
            title: "journal".into(),
            document: document("after"),
            resource_ids: Vec::new(),
            selected_thumbnail_id: None,
        })
        .unwrap();
    assert_eq!(saved.revision, 2);
    assert_eq!(
        Connection::open(&path)
            .unwrap()
            .query_row("SELECT count(*) FROM edit_journal", [], |row| row
                .get::<_, i64>(0))
            .unwrap(),
        0
    );
}
