//! Whole-library backup and restore into a new, empty library.

use std::{fs, path::Path, sync::atomic::AtomicBool};

use app_lite_core::document::{Block, BlockStyle, Inline};
use app_lite_core::{
    BackupError, CanonicalDocument, CreateNote, LibraryRepository, NoteId, SaveNote, SearchQuery,
    backup_library, library_counts, restore_library_backup,
};
use tempfile::tempdir;

fn text(text: &str) -> CanonicalDocument {
    CanonicalDocument::from_blocks(vec![Block::Paragraph {
        style: BlockStyle::default(),
        inlines: vec![Inline::Text {
            text: text.into(),
            marks: Default::default(),
        }],
    }])
}

fn with_image(prefix: &str, resource: app_lite_core::ResourceId) -> CanonicalDocument {
    CanonicalDocument::from_blocks(vec![Block::Paragraph {
        style: BlockStyle::default(),
        inlines: vec![
            Inline::Text {
                text: prefix.into(),
                marks: Default::default(),
            },
            Inline::Image {
                resource_id: resource,
                alt: "图".into(),
                display_width: None,
                link: None,
            },
        ],
    }])
}

struct Library {
    _root: tempfile::TempDir,
    profile: std::path::PathBuf,
    notes: Vec<NoteId>,
    history_only: app_lite_core::ResourceId,
}

/// Stack + notebooks, tags, a blob shared by two resources, a resource only
/// referenced by retained history, a trashed note and a setting.
fn library() -> Library {
    let root = tempdir().unwrap();
    let profile = root.path().join("library");
    fs::create_dir(&profile).unwrap();
    let repo = LibraryRepository::open(profile.join("library.sqlite")).unwrap();
    let stack = repo.create_stack("项目组").unwrap();
    let inner = repo.create_notebook("合同", Some(&stack.id)).unwrap();
    let outer = repo.create_notebook("日记", None).unwrap();
    let urgent = repo.create_tag("要务").unwrap();
    let later = repo.create_tag("稍后").unwrap();
    let shared_a = repo
        .import_resource(b"shared bytes", "a.png", "image/png", "png")
        .unwrap();
    let shared_b = repo
        .import_resource(b"shared bytes", "b.png", "image/png", "png")
        .unwrap();
    let history_only = repo
        .import_resource(b"old picture", "old.png", "image/png", "png")
        .unwrap();

    let first = repo
        .create_note(CreateNote {
            title: "合同草稿".into(),
            notebook_id: Some(inner.id.clone()),
            document: with_image("甲方乙方", shared_a.clone()),
        })
        .unwrap();
    repo.set_note_tags(&first.id, &[urgent.id.clone(), later.id.clone()])
        .unwrap();
    let second = repo
        .create_note(CreateNote {
            title: "日记".into(),
            notebook_id: Some(outer.id.clone()),
            document: with_image("晴", shared_b.clone()),
        })
        .unwrap();
    let revised = repo
        .create_note(CreateNote {
            title: "改过的".into(),
            notebook_id: None,
            document: with_image("旧版", history_only.clone()),
        })
        .unwrap();
    repo.save_note(SaveNote {
        id: revised.id.clone(),
        expected_revision: revised.revision,
        title: "改过的".into(),
        document: text("新版没有图"),
        resource_ids: vec![],
        selected_thumbnail_id: None,
    })
    .unwrap();
    let trashed = repo
        .create_note(CreateNote {
            title: "已删除".into(),
            notebook_id: None,
            document: text("回收站里的内容"),
        })
        .unwrap();
    repo.trash_note(&trashed.id).unwrap();
    repo.write_setting("ui.list_mode", "compact").unwrap();
    repo.process_search_jobs().unwrap();
    Library {
        _root: root,
        profile,
        notes: vec![first.id, second.id, revised.id, trashed.id],
        history_only,
    }
}

fn assert_same_notes(source: &Path, restored: &Path, notes: &[NoteId]) {
    let a = LibraryRepository::open(source.join("library.sqlite")).unwrap();
    let b = LibraryRepository::open(restored.join("library.sqlite")).unwrap();
    for id in notes {
        let left = a.load_note(id).unwrap().unwrap();
        let right = b.load_note(id).unwrap().unwrap();
        assert_eq!(left.title, right.title);
        assert_eq!(left.body_html, right.body_html);
        assert_eq!(left.notebook_id, right.notebook_id);
        assert_eq!(left.tag_ids, right.tag_ids);
        assert_eq!(left.resource_ids, right.resource_ids);
        assert_eq!(left.deleted_time, right.deleted_time);
        let left_history = a.readable_export_note_state(id, 10_000, 1 << 24).unwrap();
        let right_history = b.readable_export_note_state(id, 10_000, 1 << 24).unwrap();
        assert_eq!(
            left_history.map(|state| state.revisions),
            right_history.map(|state| state.revisions)
        );
    }
}

#[test]
fn backup_then_restore_into_an_empty_library_round_trips_everything() {
    let source = library();
    let out = tempdir().unwrap();
    let backup = out.path().join("backup");

    let report = backup_library(&source.profile, &backup, &AtomicBool::new(false)).unwrap();
    let counts = library_counts(&source.profile).unwrap();
    assert_eq!(report.counts, counts);
    assert_eq!(
        report.blobs, 2,
        "shared bytes stored once plus the history-only blob"
    );

    let restored = out.path().join("restored");
    let published = restore_library_backup(&backup, &restored, &AtomicBool::new(false)).unwrap();
    assert_eq!(published.path, restored);
    assert_eq!(published.counts, counts);
    assert_same_notes(&source.profile, &restored, &source.notes);

    let repo = LibraryRepository::open(restored.join("library.sqlite")).unwrap();
    assert_eq!(
        repo.read_resource_bytes(&source.history_only)
            .unwrap()
            .unwrap(),
        b"old picture",
        "history-only attachment survives"
    );
    assert_eq!(
        repo.read_setting("ui.list_mode").unwrap().as_deref(),
        Some("compact")
    );
    assert_eq!(
        repo.outbox_count().unwrap(),
        0,
        "pending sync ops are not cloned"
    );
    assert!(
        !repo
            .search(SearchQuery::parse("甲方乙方"))
            .unwrap()
            .is_empty()
    );
}

#[test]
fn backup_refuses_an_existing_target_and_leaves_it_untouched() {
    let source = library();
    let out = tempdir().unwrap();
    let backup = out.path().join("backup");
    fs::create_dir(&backup).unwrap();
    fs::write(backup.join("keep"), b"x").unwrap();

    let error = backup_library(&source.profile, &backup, &AtomicBool::new(false)).unwrap_err();

    assert!(matches!(error, BackupError::TargetExists), "{error:?}");
    assert_eq!(fs::read(backup.join("keep")).unwrap(), b"x");
    assert_eq!(
        fs::read_dir(out.path()).unwrap().count(),
        1,
        "no temp leftovers"
    );
}

#[test]
fn cancelled_backup_publishes_nothing() {
    let source = library();
    let out = tempdir().unwrap();
    let backup = out.path().join("backup");

    let error = backup_library(&source.profile, &backup, &AtomicBool::new(true)).unwrap_err();

    assert!(matches!(error, BackupError::Cancelled), "{error:?}");
    assert_eq!(fs::read_dir(out.path()).unwrap().count(), 0);
}

fn backup_of(source: &Library) -> (tempfile::TempDir, std::path::PathBuf) {
    let out = tempdir().unwrap();
    let backup = out.path().join("backup");
    backup_library(&source.profile, &backup, &AtomicBool::new(false)).unwrap();
    (out, backup)
}

fn restore_fails(backup: &Path) -> BackupError {
    let target_parent = tempdir().unwrap();
    let target = target_parent.path().join("restored");
    let error = restore_library_backup(backup, &target, &AtomicBool::new(false)).unwrap_err();
    assert!(!target.exists(), "failed restore publishes nothing");
    assert_eq!(fs::read_dir(target_parent.path()).unwrap().count(), 0);
    error
}

#[test]
fn restore_rejects_unknown_version_tampered_or_missing_blob_and_tampered_database() {
    let source = library();

    let (_keep, backup) = backup_of(&source);
    let manifest = backup.join("manifest.json");
    let text = fs::read_to_string(&manifest).unwrap();
    fs::write(&manifest, text.replace("\"version\": 1", "\"version\": 99")).unwrap();
    assert!(matches!(
        restore_fails(&backup),
        BackupError::UnsupportedVersion(99)
    ));

    let (_keep, backup) = backup_of(&source);
    let blob = fs::read_dir(backup.join("blobs"))
        .unwrap()
        .next()
        .unwrap()
        .unwrap()
        .path();
    fs::write(&blob, b"tampered!!!").unwrap();
    assert!(matches!(
        restore_fails(&backup),
        BackupError::BlobMismatch { .. }
    ));

    let (_keep, backup) = backup_of(&source);
    let blob = fs::read_dir(backup.join("blobs"))
        .unwrap()
        .next()
        .unwrap()
        .unwrap()
        .path();
    fs::remove_file(&blob).unwrap();
    assert!(matches!(
        restore_fails(&backup),
        BackupError::BlobMismatch { .. }
    ));

    let (_keep, backup) = backup_of(&source);
    let mut db = fs::read(backup.join("library.sqlite")).unwrap();
    let last = db.len() - 1;
    db[last] ^= 0xff;
    fs::write(backup.join("library.sqlite"), db).unwrap();
    assert!(matches!(
        restore_fails(&backup),
        BackupError::DatabaseMismatch
    ));
}

#[test]
fn restore_refuses_an_existing_target() {
    let source = library();
    let (_keep, backup) = backup_of(&source);
    let error =
        restore_library_backup(&backup, &source.profile, &AtomicBool::new(false)).unwrap_err();
    assert!(matches!(error, BackupError::TargetExists), "{error:?}");
}
