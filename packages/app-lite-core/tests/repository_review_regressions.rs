#![cfg(feature = "test-support")]

use app_lite_core::document::{Block, BlockStyle, Inline};
use app_lite_core::{
    AssociateResource, CanonicalDocument, CreateNote, DeletionScope, LibraryError,
    LibraryRepository, ListQuery, RepositoryClock, RepositoryIdSource, SaveNote,
};
use rusqlite::{Connection, OptionalExtension};
use std::collections::VecDeque;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tempfile::tempdir;

struct SequenceClock(Mutex<VecDeque<i64>>);

impl RepositoryClock for SequenceClock {
    fn now_millis(&self) -> i64 {
        let mut moments = self.0.lock().unwrap();
        moments
            .pop_front()
            .or_else(|| moments.back().copied())
            .unwrap_or(0)
    }
}

struct SequenceIds(Mutex<VecDeque<String>>);

impl RepositoryIdSource for SequenceIds {
    fn next_id(&self) -> Result<String, LibraryError> {
        self.0
            .lock()
            .unwrap()
            .pop_front()
            .ok_or(LibraryError::IdCollisionExhausted)
    }
}

fn fixture_ids(ids: &[char]) -> Arc<dyn RepositoryIdSource> {
    Arc::new(SequenceIds(Mutex::new(
        ids.iter()
            .map(|character| character.to_string().repeat(32))
            .collect(),
    )))
}

fn document(text: &str) -> CanonicalDocument {
    CanonicalDocument::from_blocks(vec![Block::Paragraph {
        style: BlockStyle::default(),
        inlines: vec![Inline::Text {
            text: text.into(),
            marks: Default::default(),
        }],
    }])
}

fn image_document(ids: &[app_lite_core::ResourceId]) -> CanonicalDocument {
    CanonicalDocument::from_blocks(vec![Block::Paragraph {
        style: BlockStyle::default(),
        inlines: ids
            .iter()
            .enumerate()
            .map(|(index, id)| Inline::Image {
                resource_id: id.clone(),
                alt: format!("image-{index}"),
            })
            .collect(),
    }])
}

#[test]
fn v3_rtf_rows_fail_closed_without_changing_version_or_source_rows() {
    // Catches treating fallback HTML/marker text as a replacement for rich legacy RTF.
    let profile = tempdir().unwrap();
    let path = profile.path().join("library.sqlite");
    let db = Connection::open(&path).unwrap();
    db.execute_batch("CREATE TABLE notes (id TEXT PRIMARY KEY, title TEXT, body TEXT, body_text TEXT, body_rtf BLOB, markup_language INTEGER, is_draft INTEGER, created_time INTEGER, updated_time INTEGER, deleted_time INTEGER); PRAGMA user_version = 3;").unwrap();
    let rtf = b"{\\rtf1\\ansi {\\b bold} {\\i italic} {\\ul underlined} \\pict deadbeef}";
    db.execute(
        "INSERT INTO notes VALUES (?1, ?2, ?3, ?4, ?5, 1, 1, 1, 2, 3)",
        rusqlite::params![
            "0123456789abcdef0123456789abcdef",
            "legacy",
            "<p>fallback</p>",
            "fallback",
            rtf
        ],
    )
    .unwrap();
    drop(db);
    assert!(matches!(
        LibraryRepository::open(&path),
        Err(LibraryError::LegacyRtfMigrationRequired)
    ));
    let check = Connection::open(&path).unwrap();
    assert_eq!(
        check
            .query_row("PRAGMA user_version", [], |row| row.get::<_, i64>(0))
            .unwrap(),
        3
    );
    assert_eq!(
        check
            .query_row("SELECT markup_language FROM notes", [], |row| row
                .get::<_, i64>(0))
            .unwrap(),
        1
    );
    assert_eq!(
        check
            .query_row("SELECT body_rtf FROM notes", [], |row| row
                .get::<_, Vec<u8>>(0))
            .unwrap(),
        rtf
    );
    assert_eq!(
        check
            .query_row(
                "SELECT count(*) FROM sqlite_master WHERE name = 'sync_outbox'",
                [],
                |row| row.get::<_, i64>(0)
            )
            .unwrap(),
        0
    );
}

#[test]
fn legacy_refusal_leaves_delete_journal_mode_and_profile_entries_unchanged() {
    // Catches opening a legacy profile by creating resources or switching WAL
    // before the authoritative refusal is protected by the migration lock.
    let profile = tempdir().unwrap();
    let path = profile.path().join("library.sqlite");
    let db = Connection::open(&path).unwrap();
    db.execute_batch("CREATE TABLE notes (id TEXT PRIMARY KEY, title TEXT, body TEXT, body_text TEXT, body_rtf BLOB, markup_language INTEGER, is_draft INTEGER, created_time INTEGER, updated_time INTEGER, deleted_time INTEGER); INSERT INTO notes VALUES ('0123456789abcdef0123456789abcdef', 'legacy', '<p>fallback</p>', 'fallback', X'7b5c727466317d', 1, 1, 1, 2, 3); PRAGMA user_version = 3;").unwrap();
    assert_eq!(
        db.query_row("PRAGMA journal_mode", [], |row| row.get::<_, String>(0))
            .unwrap()
            .to_ascii_lowercase(),
        "delete"
    );
    drop(db);
    assert!(matches!(
        LibraryRepository::open(&path),
        Err(LibraryError::LegacyRtfMigrationRequired)
    ));
    let after = Connection::open(&path).unwrap();
    assert_eq!(
        after
            .query_row("PRAGMA journal_mode", [], |row| row.get::<_, String>(0))
            .unwrap()
            .to_ascii_lowercase(),
        "delete"
    );
    assert_eq!(
        std::fs::read_dir(profile.path())
            .unwrap()
            .map(|entry| entry.unwrap().file_name())
            .collect::<Vec<_>>(),
        vec![std::ffi::OsString::from("library.sqlite")]
    );
}

#[test]
fn repeated_resource_occurrences_round_trip_through_snapshot_and_reopen() {
    // Catches a unique note/resource relation that cannot represent canonical A,B,A occurrence order.
    let profile = tempdir().unwrap();
    let path = profile.path().join("library.sqlite");
    let repository = LibraryRepository::open(&path).unwrap();
    let a = repository
        .import_image(b"a", "a", "image/png", "png")
        .unwrap();
    let b = repository
        .import_image(b"b", "b", "image/png", "png")
        .unwrap();
    let note = repository
        .create_note(CreateNote {
            title: "repeat".into(),
            notebook_id: None,
            document: document("empty"),
        })
        .unwrap();
    let expected = vec![a.clone(), b.clone(), a.clone()];
    let saved = repository
        .save_note(SaveNote {
            id: note.id.clone(),
            expected_revision: note.revision,
            title: "repeat".into(),
            document: image_document(&expected),
            resource_ids: expected.clone(),
            selected_thumbnail_id: Some(b.clone()),
        })
        .unwrap();
    assert_eq!(saved.resource_ids, expected);
    drop(repository);
    assert_eq!(
        LibraryRepository::open(&path)
            .unwrap()
            .load_note(&note.id)
            .unwrap()
            .unwrap()
            .resource_ids,
        expected
    );
}

#[test]
fn v3_migration_rebuilds_resource_occurrences_from_canonical_html() {
    // Catches a v3 upgrade collapsing A,B,A to a unique note/resource pair.
    let profile = tempdir().unwrap();
    let path = profile.path().join("library.sqlite");
    let a = app_lite_core::ResourceId::new("a".repeat(32)).unwrap();
    let b = app_lite_core::ResourceId::new("b".repeat(32)).unwrap();
    let html = image_document(&[a.clone(), b.clone(), a.clone()])
        .to_canonical_html()
        .as_str()
        .to_owned();
    let db = Connection::open(&path).unwrap();
    db.execute_batch("CREATE TABLE notes (id TEXT PRIMARY KEY NOT NULL, title TEXT NOT NULL DEFAULT '', body TEXT NOT NULL DEFAULT '', body_text TEXT NOT NULL DEFAULT '', body_rtf BLOB NOT NULL DEFAULT X'', markup_language INTEGER NOT NULL DEFAULT 2, is_draft INTEGER NOT NULL DEFAULT 0, created_time INTEGER NOT NULL, updated_time INTEGER NOT NULL, deleted_time INTEGER NOT NULL DEFAULT 0);
CREATE TABLE resource_blobs (sha256 TEXT PRIMARY KEY NOT NULL, size INTEGER NOT NULL DEFAULT 0, mime TEXT NOT NULL DEFAULT '', relative_path TEXT NOT NULL DEFAULT '', created_time INTEGER NOT NULL DEFAULT 0);
CREATE TABLE resources (id TEXT PRIMARY KEY NOT NULL, sha256 TEXT NOT NULL REFERENCES resource_blobs(sha256), title TEXT NOT NULL DEFAULT '', mime TEXT NOT NULL DEFAULT '', file_extension TEXT NOT NULL DEFAULT '', created_time INTEGER NOT NULL DEFAULT 0, size INTEGER NOT NULL DEFAULT 0, updated_time INTEGER NOT NULL DEFAULT 0, deleted_time INTEGER NOT NULL DEFAULT 0);
CREATE TABLE note_resources (note_id TEXT NOT NULL REFERENCES notes(id) ON DELETE CASCADE, resource_id TEXT NOT NULL REFERENCES resources(id) ON DELETE RESTRICT, position INTEGER NOT NULL DEFAULT 0, is_associated INTEGER NOT NULL DEFAULT 1, last_seen_time INTEGER NOT NULL DEFAULT 0, PRIMARY KEY (note_id, resource_id));
PRAGMA user_version = 3;").unwrap();
    for (id, hash) in [(a.as_str(), "1".repeat(64)), (b.as_str(), "2".repeat(64))] {
        db.execute(
            "INSERT INTO resource_blobs VALUES (?1, 1, 'image/png', 'x', 0)",
            [hash.as_str()],
        )
        .unwrap();
        db.execute(
            "INSERT INTO resources VALUES (?1, ?2, 'image', 'image/png', 'png', 0, 1, 0, 0)",
            rusqlite::params![id, hash],
        )
        .unwrap();
    }
    db.execute(
        "INSERT INTO notes VALUES (?1, 'legacy', ?2, '', X'', 2, 0, 0, 0, 0)",
        rusqlite::params!["c".repeat(32), html],
    )
    .unwrap();
    drop(db);
    let migrated = LibraryRepository::open(&path)
        .unwrap()
        .load_note_by_hex(&"c".repeat(32))
        .unwrap()
        .unwrap();
    assert_eq!(migrated.resource_ids, vec![a.clone(), b, a]);
}

#[test]
fn stale_snapshot_cannot_overwrite_a_newer_revision() {
    // Catches delayed session/sync saves silently replacing a newer durable snapshot.
    let profile = tempdir().unwrap();
    let path = profile.path().join("library.sqlite");
    let first = LibraryRepository::open(&path).unwrap();
    let note = first
        .create_note(CreateNote {
            title: "base".into(),
            notebook_id: None,
            document: document("base"),
        })
        .unwrap();
    let second = LibraryRepository::open(&path).unwrap();
    let newest = first
        .save_note(SaveNote {
            id: note.id.clone(),
            expected_revision: 1,
            title: "newest".into(),
            document: document("newest"),
            resource_ids: vec![],
            selected_thumbnail_id: None,
        })
        .unwrap();
    assert!(matches!(
        second.save_note(SaveNote {
            id: note.id.clone(),
            expected_revision: 1,
            title: "stale".into(),
            document: document("stale"),
            resource_ids: vec![],
            selected_thumbnail_id: None
        }),
        Err(LibraryError::StaleRevision {
            expected: 1,
            actual: 2
        })
    ));
    assert_eq!(
        first.load_note(&note.id).unwrap().unwrap().body_html,
        newest.body_html
    );
}

#[test]
fn association_snapshot_is_all_or_nothing_when_revision_is_stale() {
    // Catches a failed insert attachment leaving a note/resource relation visible
    // without the matching body snapshot and revision.
    let profile = tempdir().unwrap();
    let repository = LibraryRepository::open(profile.path().join("library.sqlite")).unwrap();
    let image = repository
        .import_image(b"image", "image", "image/png", "png")
        .unwrap();
    let note = repository
        .create_note(CreateNote {
            title: "n".into(),
            notebook_id: None,
            document: document("n"),
        })
        .unwrap();
    assert!(matches!(
        repository.associate_resource(AssociateResource {
            snapshot: SaveNote {
                id: note.id.clone(),
                expected_revision: 0,
                title: "n".into(),
                document: image_document(std::slice::from_ref(&image)),
                resource_ids: vec![image.clone()],
                selected_thumbnail_id: Some(image),
            },
        }),
        Err(LibraryError::StaleRevision { .. })
    ));
    let durable = repository.load_note(&note.id).unwrap().unwrap();
    assert_eq!(durable.revision, 1);
    assert!(durable.resource_ids.is_empty());
    assert_eq!(durable.body_html, note.body_html);
}

#[test]
fn ids_retry_collisions_and_note_time_never_moves_back() {
    // Catches replacing OS entropy with a clock-derived ID, accepting a primary-key
    // collision, or making a later save sort before its already durable snapshot.
    let profile = tempdir().unwrap();
    let repository = LibraryRepository::open_with_sources(
        profile.path().join("library.sqlite"),
        Arc::new(SequenceClock(Mutex::new(VecDeque::from([100, 100, 99])))),
        fixture_ids(&['d', 'a', 'e', 'a', 'b', 'c', 'f']),
    )
    .unwrap();
    let first = repository
        .create_note(CreateNote {
            title: "first".into(),
            notebook_id: None,
            document: document("first"),
        })
        .unwrap();
    let second = repository
        .create_note(CreateNote {
            title: "second".into(),
            notebook_id: None,
            document: document("second"),
        })
        .unwrap();
    assert_eq!(first.id.as_str(), "a".repeat(32));
    assert_eq!(second.id.as_str(), "b".repeat(32));
    let saved = repository
        .save_note(SaveNote {
            id: second.id,
            expected_revision: 1,
            title: "later".into(),
            document: document("later"),
            resource_ids: vec![],
            selected_thumbnail_id: None,
        })
        .unwrap();
    assert_eq!(saved.updated_time, 101);
}

#[test]
fn resource_entity_ids_retry_database_collisions() {
    // Catches treating the blob-store's random handle as a committed entity ID
    // without checking the SQLite uniqueness boundary.
    let profile = tempdir().unwrap();
    let repository = LibraryRepository::open_with_sources(
        profile.path().join("library.sqlite"),
        Arc::new(SequenceClock(Mutex::new(VecDeque::from([1, 2])))),
        fixture_ids(&['d', 'a', 'e', 'a', 'b', 'f']),
    )
    .unwrap();
    assert_eq!(
        repository
            .import_image(b"first", "first", "image/png", "png")
            .unwrap()
            .as_str(),
        "a".repeat(32)
    );
    assert_eq!(
        repository
            .import_image(b"second", "second", "image/png", "png")
            .unwrap()
            .as_str(),
        "b".repeat(32)
    );
}

#[test]
fn staged_resource_ids_skip_durable_rows_and_a_commit_race_publishes_nothing() {
    // A stage must select its ResourceId through the same repository allocator
    // as every other durable entity.  The second connection models the only
    // remaining stage-to-commit race without a schema-level reservation: the
    // selected ID becomes durable after staging but before the one snapshot
    // transaction.  That transaction must fail atomically and emit nothing.
    let profile = tempdir().unwrap();
    let path = profile.path().join("library.sqlite");
    let repository = LibraryRepository::open_with_sources(
        &path,
        Arc::new(SequenceClock(Mutex::new(VecDeque::from([1, 2, 3])))),
        fixture_ids(&['d', 'a', 'e', 'c', 'f', 'a', 'b']),
    )
    .unwrap();

    let existing = repository
        .import_image(b"already durable", "existing.png", "image/png", "png")
        .unwrap();
    assert_eq!(existing.as_str(), "a".repeat(32));
    let note = repository
        .create_note(CreateNote {
            title: "stage race".into(),
            notebook_id: None,
            document: CanonicalDocument::default(),
        })
        .unwrap();
    let events = repository.subscribe();
    let before_outbox = repository.outbox_count().unwrap();

    let staged = repository
        .stage_resource_reader(
            std::io::Cursor::new(b"candidate"),
            b"candidate".len(),
            "candidate.png",
            "image/png",
            "png",
        )
        .unwrap();
    assert_eq!(staged.resource_id().as_str(), "b".repeat(32));

    // A second repository/connection can commit the selected ID after stage
    // selection.  Do not make this a normal repository import: its direct
    // write represents the cross-connection primary-key race precisely.
    let competing = Connection::open(&path).unwrap();
    competing
        .execute(
            "INSERT INTO resource_blobs (sha256, size, mime, relative_path, created_time, revision)
             VALUES (?1, ?2, 'image/png', ?3, 4, 1)",
            rusqlite::params![
                staged.sha256().as_str(),
                staged.size() as i64,
                format!("resources/blobs/{}", staged.sha256().as_str()),
            ],
        )
        .unwrap();
    competing
        .execute(
            "INSERT INTO resources (id, sha256, title, mime, file_extension, size, created_time, updated_time, revision)
             VALUES (?1, ?2, 'racer', 'image/png', 'png', ?3, 4, 4, 1)",
            rusqlite::params![
                staged.resource_id().as_str(),
                staged.sha256().as_str(),
                staged.size() as i64,
            ],
        )
        .unwrap();
    drop(competing);

    let document = image_document(&[staged.resource_id().clone()]);
    assert!(
        repository
            .commit_staged_resource_snapshot(
                SaveNote {
                    id: note.id.clone(),
                    expected_revision: note.revision,
                    title: note.title.clone(),
                    resource_ids: vec![staged.resource_id().clone()],
                    document,
                    selected_thumbnail_id: None,
                },
                None,
                &staged,
            )
            .is_err()
    );
    assert!(events.recv_timeout(Duration::from_millis(30)).is_err());
    assert_eq!(repository.outbox_count().unwrap(), before_outbox);
    assert!(
        repository
            .load_note(&note.id)
            .unwrap()
            .unwrap()
            .resource_ids
            .is_empty()
    );
}

#[test]
fn outbox_operations_retry_the_repository_id_source_after_a_collision() {
    // Catches durable sync operations bypassing the per-repository allocator.
    let profile = tempdir().unwrap();
    let path = profile.path().join("library.sqlite");
    let repository = LibraryRepository::open_with_sources(
        &path,
        Arc::new(SequenceClock(Mutex::new(VecDeque::from([1])))),
        fixture_ids(&['d', 'a', 'b', 'c']),
    )
    .unwrap();
    Connection::open(&path)
        .unwrap()
        .execute(
            "INSERT INTO sync_outbox VALUES (?1, 'note', ?2, 1, 'create', 0)",
            rusqlite::params!["b".repeat(32), "f".repeat(32)],
        )
        .unwrap();
    repository
        .create_note(CreateNote {
            title: "n".into(),
            notebook_id: None,
            document: document("n"),
        })
        .unwrap();
    assert_eq!(
        Connection::open(&path)
            .unwrap()
            .query_row(
                "SELECT count(*) FROM sync_outbox WHERE id=?1",
                ["c".repeat(32)],
                |row| row.get::<_, i64>(0),
            )
            .unwrap(),
        1
    );
}

#[test]
fn purged_note_ids_remain_reserved_for_future_allocations() {
    // Catches reusing an opaque ID that is still named by retained history and a tombstone.
    let profile = tempdir().unwrap();
    let repository = LibraryRepository::open_with_sources(
        profile.path().join("library.sqlite"),
        Arc::new(SequenceClock(Mutex::new(VecDeque::from([1, 2, 3, 4])))),
        fixture_ids(&['d', 'a', 'b', 'c', 'd', 'a', 'e', 'f']),
    )
    .unwrap();
    let retired = repository
        .create_note(CreateNote {
            title: "retired".into(),
            notebook_id: None,
            document: document("retired"),
        })
        .unwrap();
    repository.trash_note(&retired.id).unwrap();
    repository.purge_note(&retired.id).unwrap();
    let replacement = repository
        .create_note(CreateNote {
            title: "replacement".into(),
            notebook_id: None,
            document: document("replacement"),
        })
        .unwrap();
    assert_eq!(retired.id.as_str(), "a".repeat(32));
    assert_eq!(replacement.id.as_str(), "e".repeat(32));
}

#[test]
fn organization_and_trash_changes_queue_search_and_restore_to_live_notebook() {
    // Catches durable organization/trash mutations that leave search stale or restore into a deleted notebook.
    let profile = tempdir().unwrap();
    let path = profile.path().join("library.sqlite");
    let repository = LibraryRepository::open(&path).unwrap();
    let default = repository.default_notebook().unwrap();
    let work = repository.create_notebook("work", None).unwrap();
    let note = repository
        .create_note(CreateNote {
            title: "n".into(),
            notebook_id: Some(work.id.clone()),
            document: document("n"),
        })
        .unwrap();
    Connection::open(&path)
        .unwrap()
        .execute("DELETE FROM search_queue", [])
        .unwrap();
    repository.move_notes(&[note.id.clone()], &work.id).unwrap();
    assert_eq!(
        Connection::open(&path)
            .unwrap()
            .query_row(
                "SELECT count(*) FROM search_queue WHERE note_id = ?1",
                [note.id.as_str()],
                |row| row.get::<_, i64>(0)
            )
            .unwrap(),
        1
    );
    Connection::open(&path)
        .unwrap()
        .execute(
            "UPDATE notebooks SET deleted_time = 1 WHERE id = ?1",
            [work.id.as_str()],
        )
        .unwrap();
    repository.trash_note(&note.id).unwrap();
    repository.restore_note(&note.id).unwrap();
    assert_eq!(
        repository.load_note(&note.id).unwrap().unwrap().notebook_id,
        default.id
    );
}

#[test]
fn selected_thumbnail_and_trash_scope_are_persisted_without_body_hydration() {
    // Catches a card query that recomputes thumbnails or returns active rows for the Trash route under a limit.
    let profile = tempdir().unwrap();
    let repository = LibraryRepository::open(profile.path().join("library.sqlite")).unwrap();
    let a = repository
        .import_image(b"a", "a", "image/png", "png")
        .unwrap();
    let b = repository
        .import_image(b"b", "b", "image/png", "png")
        .unwrap();
    let note = repository
        .create_note(CreateNote {
            title: "n".into(),
            notebook_id: None,
            document: document("n"),
        })
        .unwrap();
    let resources = vec![a.clone(), b.clone()];
    repository
        .associate_resource(AssociateResource {
            snapshot: SaveNote {
                id: note.id.clone(),
                expected_revision: note.revision,
                title: "n".into(),
                document: image_document(&resources),
                resource_ids: resources,
                selected_thumbnail_id: Some(b.clone()),
            },
        })
        .unwrap();
    assert_eq!(
        repository.list_notes(ListQuery::default()).unwrap()[0].selected_thumbnail_id,
        Some(b)
    );
    repository.trash_note(&note.id).unwrap();
    let trash = repository
        .list_notes(ListQuery {
            deletion_scope: DeletionScope::Trash,
            limit: Some(1),
            ..Default::default()
        })
        .unwrap();
    assert_eq!(trash.len(), 1);
    assert_eq!(trash[0].id, note.id);
}

#[test]
fn purge_keeps_a_durable_tombstone_and_rejects_active_notes() {
    // Catches irreversible deletion without audit/history retention or an active-note bypass.
    let profile = tempdir().unwrap();
    let path = profile.path().join("library.sqlite");
    let repository = LibraryRepository::open(&path).unwrap();
    let note = repository
        .create_note(CreateNote {
            title: "n".into(),
            notebook_id: None,
            document: document("n"),
        })
        .unwrap();
    assert!(repository.purge_note(&note.id).is_err());
    repository.trash_note(&note.id).unwrap();
    repository.purge_note(&note.id).unwrap();
    assert_eq!(
        Connection::open(path)
            .unwrap()
            .query_row(
                "SELECT count(*) FROM tombstones WHERE entity_id = ?1",
                [note.id.as_str()],
                |row| row.get::<_, i64>(0)
            )
            .unwrap(),
        1
    );
}

#[test]
fn purge_keeps_a_durable_search_delete_job_after_reopen() {
    // Catches foreign-key cascade deleting the only search-removal instruction.
    let profile = tempdir().unwrap();
    let path = profile.path().join("library.sqlite");
    let repository = LibraryRepository::open(&path).unwrap();
    let note = repository
        .create_note(CreateNote {
            title: "purge".into(),
            notebook_id: None,
            document: document("purge"),
        })
        .unwrap();
    let events = repository.subscribe();
    repository.trash_note(&note.id).unwrap();
    repository.purge_note(&note.id).unwrap();
    drop(repository);
    let reopened = LibraryRepository::open(&path).unwrap();
    let jobs = reopened.take_search_jobs(10).unwrap();
    let delete = jobs
        .iter()
        .find(|job| job.note_id == note.id && job.reason == "purge")
        .cloned()
        .expect("durable purge delete job");
    reopened.ack_search_jobs(&[delete]).unwrap();
    assert!(reopened.take_search_jobs(10).unwrap().is_empty());
    assert!(events
        .try_iter()
        .any(|event| matches!(event, app_lite_core::LibraryEvent::SearchProjectionQueued(id) if id == note.id)));
}

#[test]
fn associated_resource_rollback_is_a_complete_noop() {
    // Catches cleanup deleting the pending resource upload while an association survives.
    let profile = tempdir().unwrap();
    let path = profile.path().join("library.sqlite");
    let repository = LibraryRepository::open(&path).unwrap();
    let image = repository
        .import_image(b"image", "image", "image/png", "png")
        .unwrap();
    let note = repository
        .create_note(CreateNote {
            title: "n".into(),
            notebook_id: None,
            document: document("n"),
        })
        .unwrap();
    repository
        .associate_resource(AssociateResource {
            snapshot: SaveNote {
                id: note.id.clone(),
                expected_revision: note.revision,
                title: "n".into(),
                document: image_document(std::slice::from_ref(&image)),
                resource_ids: vec![image.clone()],
                selected_thumbnail_id: Some(image.clone()),
            },
        })
        .unwrap();
    let before = Connection::open(&path)
        .unwrap()
        .query_row(
            "SELECT count(*) FROM sync_outbox WHERE entity_type='resource' AND entity_id=?1",
            [image.as_str()],
            |row| row.get::<_, i64>(0),
        )
        .unwrap();
    repository.rollback_unassociated_resource(&image).unwrap();
    drop(repository);
    let check = Connection::open(&path).unwrap();
    assert!(
        check
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM resources WHERE id=?1)",
                [image.as_str()],
                |row| row.get::<_, i64>(0),
            )
            .unwrap()
            == 1
    );
    assert_eq!(
        check
            .query_row(
                "SELECT count(*) FROM sync_outbox WHERE entity_type='resource' AND entity_id=?1",
                [image.as_str()],
                |row| row.get::<_, i64>(0),
            )
            .unwrap(),
        before
    );
}

#[test]
fn rollback_unassociated_resource_collects_only_a_uniquely_owned_blob_record() {
    // Catches leaving resource_blobs metadata orphaned after a successfully
    // rolled-back import (the immutable byte file remains GC-owned).
    let profile = tempdir().unwrap();
    let path = profile.path().join("library.sqlite");
    let repository = LibraryRepository::open(&path).unwrap();
    let image = repository
        .import_image(b"unique", "unique", "image/png", "png")
        .unwrap();
    let hash: String = Connection::open(&path)
        .unwrap()
        .query_row(
            "SELECT sha256 FROM resources WHERE id=?1",
            [image.as_str()],
            |row| row.get(0),
        )
        .unwrap();
    repository.rollback_unassociated_resource(&image).unwrap();
    drop(repository);
    let reopened = LibraryRepository::open(&path).unwrap();
    assert!(reopened.resource_metadata(&image).unwrap().is_none());
    drop(reopened);
    let check = Connection::open(&path).unwrap();
    assert_eq!(
        check
            .query_row(
                "SELECT count(*) FROM resource_blobs WHERE sha256=?1",
                [hash],
                |row| row.get::<_, i64>(0),
            )
            .unwrap(),
        0
    );
}

#[test]
fn fresh_schema_enforces_relation_foreign_keys() {
    // Catches a schema that requests foreign_keys but leaves note organization links as unchecked text.
    let profile = tempdir().unwrap();
    let path = profile.path().join("library.sqlite");
    let repository = LibraryRepository::open(&path).unwrap();
    let notebook = repository.create_notebook("linked", None).unwrap();
    let note = repository
        .create_note(CreateNote {
            title: "n".into(),
            notebook_id: Some(notebook.id.clone()),
            document: document("n"),
        })
        .unwrap();
    drop(repository);
    let db = Connection::open(&path).unwrap();
    db.execute_batch("PRAGMA foreign_keys=ON").unwrap();
    assert!(
        db.execute("DELETE FROM notebooks WHERE id=?1", [notebook.id.as_str()])
            .is_err()
    );
    assert_eq!(
        db.query_row("PRAGMA foreign_key_check", [], |row| row
            .get::<_, String>(0))
            .optional()
            .unwrap(),
        None
    );
    assert_eq!(
        db.query_row(
            "SELECT count(*) FROM notes WHERE id=?1",
            [note.id.as_str()],
            |row| row.get::<_, i64>(0)
        )
        .unwrap(),
        1
    );
}
