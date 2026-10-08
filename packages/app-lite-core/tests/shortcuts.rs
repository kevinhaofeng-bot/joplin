use app_lite_core::{CanonicalDocument, CreateNote, LibraryRepository, ShortcutTarget};
use tempfile::tempdir;

#[test]
fn shortcut_targets_keep_order_identity_and_survive_reopen_without_duplicate_rows() {
    // Missing writes, replacing the target, or appending a duplicate all break
    // the observed target sequence after reopening the real database.
    let profile = tempdir().unwrap();
    let path = profile.path().join("library.sqlite");
    let repo = LibraryRepository::open(&path).unwrap();
    let note = repo
        .create_note(CreateNote {
            title: "笔记".into(),
            notebook_id: None,
            document: CanonicalDocument::default(),
        })
        .unwrap();
    let book = repo.create_notebook("本", None).unwrap();
    let tag = repo.create_tag("标签").unwrap();
    let targets = vec![
        ShortcutTarget::Note(note.id.clone()),
        ShortcutTarget::Notebook(book.id.clone()),
        ShortcutTarget::Tag(tag.id.clone()),
    ];
    repo.add_shortcuts(&targets).unwrap();
    repo.add_shortcuts(&targets).unwrap();
    drop(repo);
    let repo = LibraryRepository::open(&path).unwrap();
    let items = repo.list_shortcuts().unwrap();
    assert_eq!(
        items.iter().map(|s| s.target.clone()).collect::<Vec<_>>(),
        targets
    );
    assert_eq!(
        items.iter().map(|s| s.title.as_str()).collect::<Vec<_>>(),
        vec!["笔记", "本", "标签"]
    );
    assert_eq!(
        items.iter().map(|s| s.position).collect::<Vec<_>>(),
        vec![0, 1, 2]
    );
    repo.remove_shortcuts(&[targets[1].clone()]).unwrap();
    let items = repo.list_shortcuts().unwrap();
    assert_eq!(
        items.iter().map(|s| s.target.clone()).collect::<Vec<_>>(),
        vec![targets[0].clone(), targets[2].clone()]
    );
    assert!(repo.load_note(&note.id).unwrap().is_some());
}

#[test]
fn invalid_batch_cannot_publish_a_partial_shortcut() {
    let p = tempdir().unwrap();
    let repo = LibraryRepository::open(p.path().join("library.sqlite")).unwrap();
    let book = repo.create_notebook("目标", None).unwrap();
    let absent = app_lite_core::TagId::parse("ffffffffffffffffffffffffffffffff").unwrap();
    assert!(
        repo.add_shortcuts(&[
            ShortcutTarget::Notebook(book.id),
            ShortcutTarget::Tag(absent)
        ])
        .is_err()
    );
    assert!(repo.list_shortcuts().unwrap().is_empty());
    let sql = rusqlite::Connection::open(p.path().join("library.sqlite")).unwrap();
    assert_eq!(
        sql.query_row("SELECT count(*) FROM shortcuts", [], |row| row
            .get::<_, i64>(0))
            .unwrap(),
        0
    );
}

#[test]
fn shortcut_titles_follow_renames_and_trashed_notes_are_not_navigable() {
    let p = tempdir().unwrap();
    let repo = LibraryRepository::open(p.path().join("library.sqlite")).unwrap();
    let note = repo
        .create_note(CreateNote {
            title: "图文".into(),
            notebook_id: None,
            document: CanonicalDocument::default(),
        })
        .unwrap();
    let tag = repo.create_tag("旧名").unwrap();
    repo.add_shortcuts(&[
        ShortcutTarget::Note(note.id.clone()),
        ShortcutTarget::Tag(tag.id.clone()),
    ])
    .unwrap();
    repo.rename_tag(&tag.id, "新名").unwrap();
    repo.trash_note(&note.id).unwrap();
    let shortcuts = repo.list_shortcuts().unwrap();
    assert_eq!(shortcuts.len(), 1);
    assert_eq!(shortcuts[0].title, "新名");
    assert!(
        repo.add_shortcuts(&[ShortcutTarget::Note(note.id.clone())])
            .is_err()
    );
    repo.restore_note(&note.id).unwrap();
    assert_eq!(repo.list_shortcuts().unwrap().len(), 2);
}

#[cfg(feature = "test-support")]
#[test]
fn navigation_shortcut_projection_never_reads_body_or_blobs() {
    let p = tempdir().unwrap();
    let repo = LibraryRepository::open(p.path().join("library.sqlite")).unwrap();
    let note = repo
        .create_note(CreateNote {
            title: "仅标题".into(),
            notebook_id: None,
            document: CanonicalDocument::default(),
        })
        .unwrap();
    let group = repo.create_stack("组").unwrap();
    repo.add_shortcuts(&[
        ShortcutTarget::Note(note.id.clone()),
        ShortcutTarget::Stack(group.id.clone()),
    ])
    .unwrap();
    let observer = repo.observe_next_navigation_index_query();
    let index = repo.list_navigation_index().unwrap();
    assert_eq!(
        index
            .shortcuts
            .iter()
            .map(|s| s.title.as_str())
            .collect::<Vec<_>>(),
        vec!["仅标题", "组"]
    );
    let reads = observer.recv().unwrap();
    assert!(reads.iter().any(|column| column == "notes.title"));
    for forbidden in [
        "notes.body_html",
        "notes.body_text",
        "notes.merge_state",
        "notes.snippet",
        "resource_blobs.bytes",
    ] {
        assert!(
            !reads.iter().any(|column| column == forbidden),
            "unexpected read: {reads:?}"
        );
    }
}
