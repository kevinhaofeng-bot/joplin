use app_lite_core::document::Block;
#[cfg(feature = "test-support")]
use app_lite_core::schema::SCHEMA_VERSION;
use app_lite_core::{
    AssociateResource, CanonicalDocument, CreateNote, EditJournalEntry, LibraryError,
    LibraryRepository, SaveNote,
};
use rusqlite::Connection;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use tempfile::tempdir;

#[cfg(feature = "test-support")]
use app_lite_core::{OpenTestHook, OpenTestPhase, RepositoryClock, RepositoryIdSource};
#[cfg(feature = "test-support")]
use std::sync::{Arc, Mutex, mpsc};
#[cfg(feature = "test-support")]
use std::thread;

#[cfg(feature = "test-support")]
struct FixedClock;
#[cfg(feature = "test-support")]
impl RepositoryClock for FixedClock {
    fn now_millis(&self) -> i64 {
        1
    }
}
#[cfg(feature = "test-support")]
struct FixedIds;
#[cfg(feature = "test-support")]
impl RepositoryIdSource for FixedIds {
    fn next_id(&self) -> Result<String, LibraryError> {
        Ok("a".repeat(32))
    }
}

#[test]
fn open_creates_clean_v10_database_idempotently() {
    // Catches a fresh profile missing v7 schema/PRAGMAs or a second open changing it.
    let profile = tempdir().unwrap();
    let path = profile.path().join("library.sqlite");
    LibraryRepository::open(&path).unwrap();
    LibraryRepository::open(&path).unwrap();
    let connection = Connection::open(path).unwrap();
    assert_eq!(
        connection
            .query_row("PRAGMA user_version", [], |row| row.get::<_, i64>(0))
            .unwrap(),
        10
    );
    assert_eq!(
        connection
            .query_row(
                "SELECT count(*) FROM sqlite_master WHERE type = 'table' AND name = 'sync_outbox'",
                [],
                |row| row.get::<_, i64>(0)
            )
            .unwrap(),
        1
    );
    for table in [
        "notes",
        "notebooks",
        "stacks",
        "tags",
        "note_tags",
        "resources",
        "resource_blobs",
        "resource_gc_queue",
        "note_resources",
        "note_revisions",
        "edit_journal",
        "search_queue",
        "sync_outbox",
        "sync_cursor",
        "sync_conflicts",
        "shortcuts",
        "search_history",
        "settings",
        "search_unicode",
        "search_trigram",
        "resource_filename_unicode",
        "resource_filename_trigram",
        "derived_text_unicode",
        "derived_text_trigram",
    ] {
        assert_eq!(
            connection
                .query_row(
                    "SELECT count(*) FROM sqlite_master WHERE type = 'table' AND name = ?1",
                    [table],
                    |row| row.get::<_, i64>(0)
                )
                .unwrap(),
            1,
            "missing {table}"
        );
    }
}

#[test]
fn v8_to_v9_does_not_rewrite_existing_note_bodies() {
    // D2 only adds a disposable resource-title projection.  An already-v8
    // profile must not parse or canonicalize every note body just to install
    // it: that is both needless I/O and an unsafe content rewrite.
    let profile = tempdir().unwrap();
    let path = profile.path().join("library.sqlite");
    let repo = LibraryRepository::open(&path).unwrap();
    let body_note = repo
        .create_note(app_lite_core::CreateNote {
            title: "migration body".into(),
            notebook_id: None,
            document: CanonicalDocument::parse_html("<p>initial</p>").unwrap(),
        })
        .unwrap();
    let resource = repo
        .import_resource(b"old", "v8-ledger.pdf", "application/pdf", "pdf")
        .unwrap();
    let note = repo
        .create_note(CreateNote {
            title: "resource migration".into(),
            notebook_id: None,
            document: CanonicalDocument::parse_html("<p>neutral</p>").unwrap(),
        })
        .unwrap();
    repo.associate_resource(AssociateResource {
        snapshot: SaveNote {
            id: note.id.clone(),
            expected_revision: note.revision,
            title: note.title,
            document: CanonicalDocument::from_blocks(vec![Block::Attachment {
                resource_id: resource.clone(),
                filename: "display-name.pdf".into(),
                media_type: "application/pdf".into(),
            }]),
            resource_ids: vec![resource.clone()],
            selected_thumbnail_id: None,
        },
    })
    .unwrap();
    repo.process_search_jobs().unwrap();
    drop(repo);
    let connection = Connection::open(&path).unwrap();
    connection
        .execute(
            "UPDATE notes SET body_html = ?1, body_text = ?2, snippet = ?3 WHERE id = ?4",
            rusqlite::params![
                "<p>  preserve  </p>",
                "preserve original text",
                "original snippet",
                body_note.id.as_str()
            ],
        )
        .unwrap();
    connection.execute_batch("DROP TRIGGER resource_filename_search_insert; DROP TRIGGER resource_filename_search_update; DROP TRIGGER resource_filename_search_delete; DROP TABLE resource_filename_unicode; DROP TABLE resource_filename_trigram; DROP TABLE resource_search_rows; PRAGMA user_version = 8;").unwrap();
    drop(connection);

    let reopened = LibraryRepository::open(&path).unwrap();
    let hits = reopened
        .search(app_lite_core::SearchQuery::parse("ledger"))
        .unwrap();
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].matched_resource, Some(resource));
    drop(reopened);
    let check = Connection::open(&path).unwrap();
    assert_eq!(
        check
            .query_row(
                "SELECT body_html FROM notes WHERE id = ?1",
                [body_note.id.as_str()],
                |row| row.get::<_, String>(0)
            )
            .unwrap(),
        "<p>  preserve  </p>"
    );
    assert_eq!(
        check
            .query_row(
                "SELECT body_text FROM notes WHERE id = ?1",
                [body_note.id.as_str()],
                |row| row.get::<_, String>(0)
            )
            .unwrap(),
        "preserve original text"
    );
}

#[test]
fn v9_to_v10_queues_live_eligible_attachments_without_rewriting_note_text() {
    let profile = tempdir().unwrap();
    let path = profile.path().join("library.sqlite");
    let repository = LibraryRepository::open(&path).unwrap();
    let resource = repository
        .import_resource(
            b"must not be read by migration",
            "scan.pdf",
            "application/pdf",
            "pdf",
        )
        .unwrap();
    let note = repository
        .create_note(CreateNote {
            title: "migration queue".into(),
            notebook_id: None,
            document: CanonicalDocument::from_blocks(vec![Block::Attachment {
                resource_id: resource.clone(),
                filename: "scan.pdf".into(),
                media_type: "application/pdf".into(),
            }]),
        })
        .unwrap();
    drop(repository);
    let connection = Connection::open(&path).unwrap();
    connection
        .execute(
            "UPDATE notes SET body_text='migration body sentinel' WHERE id=?1",
            [note.id.as_str()],
        )
        .unwrap();
    connection.execute_batch("DROP TRIGGER derived_text_resource_delete; DROP TABLE derived_text_unicode; DROP TABLE derived_text_trigram; DROP TABLE derived_text_rows; DROP TABLE derived_text_jobs; PRAGMA user_version=9;").unwrap();
    drop(connection);

    let reopened = LibraryRepository::open(&path).unwrap();
    let jobs = reopened.take_derived_text_jobs(10).unwrap();
    assert_eq!(jobs.len(), 1);
    assert_eq!(jobs[0].resource_id, resource);
    drop(reopened);
    let connection = Connection::open(path).unwrap();
    assert_eq!(
        connection
            .query_row(
                "SELECT body_text FROM notes WHERE id=?1",
                [note.id.as_str()],
                |row| row.get::<_, String>(0)
            )
            .unwrap(),
        "migration body sentinel"
    );
}

#[test]
fn v4_reopen_repairs_a_non_wal_profile_without_schema_writes() {
    // Catches the old migrated-only WAL branch, which permanently rejected a
    // v4 database left in DELETE mode after an interrupted publication.
    let profile = tempdir().unwrap();
    let path = profile.path().join("library.sqlite");
    drop(LibraryRepository::open(&path).unwrap());
    let connection = Connection::open(&path).unwrap();
    let mode: String = connection
        .query_row("PRAGMA journal_mode=DELETE", [], |row| row.get(0))
        .unwrap();
    assert_eq!(mode.to_ascii_lowercase(), "delete");
    drop(connection);
    drop(LibraryRepository::open(&path).unwrap());
    assert_eq!(
        Connection::open(&path)
            .unwrap()
            .query_row("PRAGMA journal_mode", [], |row| row.get::<_, String>(0))
            .unwrap()
            .to_ascii_lowercase(),
        "wal"
    );
}

#[test]
fn v4_journal_table_upgrades_before_its_v5_index_is_created() {
    // A real v4 profile already has edit_journal without the Task-4 identity
    // columns. The index must be delayed until those columns exist; otherwise
    // SQLite rejects the whole migration before `ensure_column` can run.
    let profile = tempdir().unwrap();
    let path = profile.path().join("library.sqlite");
    let connection = Connection::open(&path).unwrap();
    connection
        .execute_batch(
            "CREATE TABLE edit_journal (
                id TEXT PRIMARY KEY NOT NULL,
                note_id TEXT NOT NULL,
                generation INTEGER NOT NULL,
                delta_utf8 TEXT NOT NULL,
                created_time INTEGER NOT NULL
            );
            PRAGMA user_version = 4;",
        )
        .unwrap();
    drop(connection);

    drop(LibraryRepository::open(&path).expect("upgrade v4 journal shape"));
    let check = Connection::open(&path).unwrap();
    for column in ["expected_revision", "writer_token", "sequence"] {
        let exists: i64 = check
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM pragma_table_info('edit_journal') WHERE name = ?1)",
                [column],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(exists, 1, "missing v5 journal column {column}");
    }
    let index_exists: i64 = check
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type = 'index' AND name = 'edit_journal_latest_idx')",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(index_exists, 1);
}

#[test]
fn v4_legacy_journal_backfills_payload_revision_without_losing_recovery() {
    // A real v4 journal payload already carries the revision it was computed
    // from. Adding v5's column with DEFAULT 1 must not make a checkpoint for
    // a note that has reached revision 2 invisible after restart.
    let profile = tempdir().unwrap();
    let path = profile.path().join("library.sqlite");
    let repository = LibraryRepository::open(&path).expect("create release-shaped profile");
    let created = repository
        .create_note(CreateNote {
            title: "迁移前".into(),
            notebook_id: None,
            document: CanonicalDocument::default(),
        })
        .expect("create note");
    let revised = repository
        .flush_snapshot(
            SaveNote {
                id: created.id.clone(),
                expected_revision: created.revision,
                title: "迁移基线".into(),
                document: CanonicalDocument::default(),
                resource_ids: Vec::new(),
                selected_thumbnail_id: None,
            },
            None,
        )
        .expect("advance durable note to revision two");
    assert_eq!(revised.revision, 2);
    drop(repository);

    let connection = Connection::open(&path).unwrap();
    connection
        .execute_batch(
            "DROP TABLE edit_journal;
             CREATE TABLE edit_journal (
                 id TEXT PRIMARY KEY NOT NULL,
                 note_id TEXT NOT NULL,
                 generation INTEGER NOT NULL,
                 delta_utf8 TEXT NOT NULL,
                 created_time INTEGER NOT NULL
             );
             PRAGMA user_version = 4;",
        )
        .unwrap();
    let payload = format!(
        r#"{{"version":1,"note_id":"{}","expected_revision":2,"generation":7,"title":"迁移后标题","body_html":"<p><strong>中文样式</strong></p>","resource_ids":[]}}"#,
        created.id.as_str()
    );
    connection
        .execute(
            "INSERT INTO edit_journal (id, note_id, generation, delta_utf8, created_time)
             VALUES (?1, ?2, 7, ?3, 1)",
            rusqlite::params!["b".repeat(32), created.id.as_str(), payload],
        )
        .unwrap();
    drop(connection);

    let migrated = LibraryRepository::open(&path).expect("migrate v4 release profile");
    let recovered = migrated
        .latest_edit_journal_for_revision(&created.id, Some(2))
        .expect("read migrated checkpoint")
        .expect("revision-two legacy checkpoint remains recoverable");
    assert_eq!(recovered.expected_revision, 2);
    assert!(!recovered.writer_token.is_empty());
    assert!(recovered.sequence > 0);
    assert!(recovered.delta_utf8.contains("中文样式"));
}

#[test]
fn v4_migration_keeps_one_latest_current_checkpoint_per_note_without_touching_other_notes() {
    // Catches the old v4->v5 migration preserving every legacy row. A
    // lifecycle snapshot could then delete only the recovered row and leave
    // a different old owner behind to block the next edit forever. The v4
    // writer legitimately appended many rows, so select one current-base
    // checkpoint deterministically per note and discard stale-base rows.
    let profile = tempdir().unwrap();
    let path = profile.path().join("library.sqlite");
    let repository = LibraryRepository::open(&path).expect("create release-shaped profile");
    let first = repository
        .create_note(CreateNote {
            title: "first durable title".into(),
            notebook_id: None,
            document: CanonicalDocument::default(),
        })
        .expect("create first note");
    let first_revision_two = repository
        .flush_snapshot(
            SaveNote {
                id: first.id.clone(),
                expected_revision: first.revision,
                title: "first revision two base".into(),
                document: CanonicalDocument::default(),
                resource_ids: Vec::new(),
                selected_thumbnail_id: None,
            },
            None,
        )
        .expect("advance first note to revision two");
    assert_eq!(first_revision_two.revision, 2);
    let second = repository
        .create_note(CreateNote {
            title: "second durable title".into(),
            notebook_id: None,
            document: CanonicalDocument::default(),
        })
        .expect("create second note");
    drop(repository);

    let connection = Connection::open(&path).unwrap();
    connection
        .execute_batch(
            "DROP TABLE edit_journal;
             CREATE TABLE edit_journal (
                 id TEXT PRIMARY KEY NOT NULL,
                 note_id TEXT NOT NULL,
                 generation INTEGER NOT NULL,
                 delta_utf8 TEXT NOT NULL,
                 created_time INTEGER NOT NULL
             );
             PRAGMA user_version = 4;",
        )
        .expect("seed a real v4 journal table");
    for (id, note_id, revision, generation, created_time, title, body_html) in [
        (
            "a".repeat(32),
            first.id.as_str(),
            2_i64,
            3_i64,
            100_i64,
            "first J1",
            "<p>first J1</p>",
        ),
        (
            "b".repeat(32),
            first.id.as_str(),
            2_i64,
            9_i64,
            200_i64,
            "first J2 newest",
            "<p><strong>first J2 newest</strong></p>",
        ),
        // This row is syntactically valid but was based on revision one. Its
        // later timestamp/generation must not let it beat revision-two J2.
        (
            "c".repeat(32),
            first.id.as_str(),
            1_i64,
            99_i64,
            999_i64,
            "first stale must not win",
            "<p>first stale must not win</p>",
        ),
        (
            "d".repeat(32),
            second.id.as_str(),
            1_i64,
            4_i64,
            150_i64,
            "second checkpoint survives",
            "<p>second checkpoint survives</p>",
        ),
    ] {
        let payload = format!(
            r#"{{"version":1,"note_id":"{note_id}","expected_revision":{revision},"generation":{generation},"title":"{title}","body_html":"{body_html}","resource_ids":[]}}"#
        );
        connection
            .execute(
                "INSERT INTO edit_journal (id, note_id, generation, delta_utf8, created_time)
                 VALUES (?1, ?2, ?3, ?4, ?5)",
                rusqlite::params![id, note_id, generation, payload, created_time],
            )
            .expect("seed v4 checkpoint");
    }
    drop(connection);

    drop(LibraryRepository::open(&path).expect("migrate v4 multi-checkpoint profile"));
    let check = Connection::open(&path).unwrap();
    let rows = check
        .prepare(
            "SELECT id, note_id, expected_revision, writer_token, sequence, generation, delta_utf8
             FROM edit_journal
             ORDER BY note_id, sequence",
        )
        .unwrap()
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, i64>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, i64>(4)?,
                row.get::<_, i64>(5)?,
                row.get::<_, String>(6)?,
            ))
        })
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap();
    assert_eq!(
        rows.len(),
        2,
        "migration keeps one recoverable row per note"
    );
    let first_row = rows
        .iter()
        .find(|(_, note_id, ..)| note_id == first.id.as_str())
        .expect("first note retains a checkpoint");
    assert_eq!(first_row.0, "b".repeat(32));
    assert_eq!(first_row.2, first_revision_two.revision);
    assert_eq!(first_row.3, format!("legacy-v4-{}", "b".repeat(32)));
    assert!(first_row.4 > 0);
    assert_eq!(first_row.5, 9);
    assert!(first_row.6.contains("first J2 newest"));
    let second_row = rows
        .iter()
        .find(|(_, note_id, ..)| note_id == second.id.as_str())
        .expect("migration must not delete another note's valid checkpoint");
    assert_eq!(second_row.0, "d".repeat(32));
    assert_eq!(second_row.2, second.revision);
    assert!(second_row.6.contains("second checkpoint survives"));
    assert!(
        rows.iter()
            .all(|(_, _, _, _, _, _, delta)| !delta.contains("stale must not win")),
        "a stale base revision must never remain eligible after migration"
    );
}

#[test]
fn v4_migration_rejects_a_malformed_newer_checkpoint_without_writing_the_profile() {
    // Catches the R6 data-loss ordering: J2 has a later timestamp and looks
    // current to the old three-field probe, but it omits body_html. Migration
    // must reject the entire v4 profile before it can delete readable J1 or
    // publish v5 columns/defaults.
    let (profile, path, note_id) = v4_profile_with_revision_two_note();
    let connection = Connection::open(&path).expect("open v4 fixture");
    insert_v4_journal(
        &connection,
        &"a".repeat(32),
        &note_id,
        7,
        legacy_v1_payload(
            &note_id,
            2,
            7,
            json!("readable J1"),
            json!("<p>readable J1</p>"),
            json!([]),
        ),
        100,
    );
    let mut malformed = serde_json::json!({
        "version": 1,
        "note_id": note_id,
        "expected_revision": 2,
        "generation": 8,
        "title": "malformed J2",
        "resource_ids": [],
    });
    malformed
        .as_object_mut()
        .expect("object payload")
        .remove("body_html");
    insert_v4_journal(
        &connection,
        &"b".repeat(32),
        &note_id,
        8,
        malformed.to_string(),
        200,
    );
    drop(connection);

    let before = legacy_v4_journal_snapshot(&path);
    assert!(matches!(
        LibraryRepository::open(&path),
        Err(LibraryError::InvalidLegacyEditJournal)
    ));
    assert_eq!(
        legacy_v4_journal_snapshot(&path),
        before,
        "a corrupt newer J2 must not publish schema changes or delete readable J1"
    );
    drop(profile);
}

#[test]
fn v4_migration_rejects_every_unrecoverable_wire_field_without_mutating_legacy_rows() {
    // Each case names a production validation omission. If the migration
    // returns to its former untyped Value probe, at least the body/resource,
    // SQL-generation, noncanonical-body, and malformed-ID cases would again
    // choose J2 and delete J1 while reporting success.
    let unassociated_resource = "c".repeat(32);
    let cases = vec![
        (
            "wrong typed title",
            "b".repeat(32),
            8_i64,
            legacy_v1_payload(
                "NOTE_ID",
                2,
                8,
                json!(42),
                json!("<p>wrong title type</p>"),
                json!([]),
            ),
        ),
        (
            "wrong typed body",
            "b".repeat(32),
            8_i64,
            legacy_v1_payload(
                "NOTE_ID",
                2,
                8,
                json!("wrong body type"),
                json!(["not a string"]),
                json!([]),
            ),
        ),
        (
            "wrong typed resource list",
            "b".repeat(32),
            8_i64,
            legacy_v1_payload(
                "NOTE_ID",
                2,
                8,
                json!("wrong resource list"),
                json!("<p>wrong resource list</p>"),
                json!("not an array"),
            ),
        ),
        (
            "payload and SQL generation differ",
            "b".repeat(32),
            7_i64,
            legacy_v1_payload(
                "NOTE_ID",
                2,
                8,
                json!("generation mismatch"),
                json!("<p>generation mismatch</p>"),
                json!([]),
            ),
        ),
        (
            "zero generation",
            "b".repeat(32),
            0_i64,
            legacy_v1_payload(
                "NOTE_ID",
                2,
                0,
                json!("zero generation"),
                json!("<p>zero generation</p>"),
                json!([]),
            ),
        ),
        (
            "noncanonical unsafe body",
            "b".repeat(32),
            8_i64,
            legacy_v1_payload(
                "NOTE_ID",
                2,
                8,
                json!("unsafe body"),
                json!("<p onclick=\"x\">unsafe body</p>"),
                json!([]),
            ),
        ),
        (
            "unassociated valid resource",
            "b".repeat(32),
            8_i64,
            legacy_v1_payload(
                "NOTE_ID",
                2,
                8,
                json!("foreign resource"),
                json!(format!(
                    "<p><img src=\":/{unassociated_resource}\" alt=\"foreign\"></p>"
                )),
                json!([unassociated_resource]),
            ),
        ),
        (
            "invalid legacy journal id cannot form an owner token",
            "not-an-opaque-journal-id".into(),
            8_i64,
            legacy_v1_payload(
                "NOTE_ID",
                2,
                8,
                json!("bad writer identity"),
                json!("<p>bad writer identity</p>"),
                json!([]),
            ),
        ),
    ];

    for (case, journal_id, sql_generation, template) in cases {
        let (profile, path, note_id) = v4_profile_with_revision_two_note();
        let connection = Connection::open(&path).expect("open v4 field fixture");
        insert_v4_journal(
            &connection,
            &"a".repeat(32),
            &note_id,
            7,
            legacy_v1_payload(
                &note_id,
                2,
                7,
                json!("readable J1"),
                json!("<p>readable J1</p>"),
                json!([]),
            ),
            100,
        );
        let malformed = template.to_string().replace("NOTE_ID", &note_id);
        insert_v4_journal(
            &connection,
            &journal_id,
            &note_id,
            sql_generation,
            malformed,
            200,
        );
        drop(connection);

        let before = legacy_v4_journal_snapshot(&path);
        assert!(
            matches!(
                LibraryRepository::open(&path),
                Err(LibraryError::InvalidLegacyEditJournal)
            ),
            "{case} must fail the migration before winner compaction"
        );
        assert_eq!(
            legacy_v4_journal_snapshot(&path),
            before,
            "{case} must leave the v4 schema and both original journal bytes untouched"
        );
        drop(profile);
    }
}

fn v4_profile_with_revision_two_note() -> (tempfile::TempDir, std::path::PathBuf, String) {
    let profile = tempdir().expect("temporary v4 profile");
    let path = profile.path().join("library.sqlite");
    let repository = LibraryRepository::open(&path).expect("create release-shaped profile");
    let created = repository
        .create_note(CreateNote {
            title: "v4 durable title".into(),
            notebook_id: None,
            document: CanonicalDocument::default(),
        })
        .expect("create v4 fixture note");
    let revision_two = repository
        .flush_snapshot(
            SaveNote {
                id: created.id.clone(),
                expected_revision: created.revision,
                title: "v4 revision two base".into(),
                document: CanonicalDocument::default(),
                resource_ids: Vec::new(),
                selected_thumbnail_id: None,
            },
            None,
        )
        .expect("advance fixture note to revision two");
    assert_eq!(revision_two.revision, 2);
    drop(repository);

    let connection = Connection::open(&path).expect("replace journal with v4 shape");
    connection
        .execute_batch(
            "DROP TABLE edit_journal;
             DROP TABLE journal_sequence;
             CREATE TABLE edit_journal (
                 id TEXT PRIMARY KEY NOT NULL,
                 note_id TEXT NOT NULL,
                 generation INTEGER NOT NULL,
                 delta_utf8 TEXT NOT NULL,
                 created_time INTEGER NOT NULL
             );
             PRAGMA user_version = 4;",
        )
        .expect("install real v4 journal schema");
    drop(connection);
    (profile, path, created.id.as_str().to_owned())
}

fn legacy_v1_payload(
    note_id: &str,
    expected_revision: i64,
    generation: i64,
    title: Value,
    body_html: Value,
    resource_ids: Value,
) -> String {
    json!({
        "version": 1,
        "note_id": note_id,
        "expected_revision": expected_revision,
        "generation": generation,
        "title": title,
        "body_html": body_html,
        "resource_ids": resource_ids,
    })
    .to_string()
}

fn insert_v4_journal(
    connection: &Connection,
    id: &str,
    note_id: &str,
    generation: i64,
    delta_utf8: String,
    created_time: i64,
) {
    connection
        .execute(
            "INSERT INTO edit_journal (id, note_id, generation, delta_utf8, created_time)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            rusqlite::params![id, note_id, generation, delta_utf8, created_time],
        )
        .expect("insert real v4 checkpoint");
}

fn legacy_v4_journal_snapshot(
    path: &std::path::Path,
) -> (i64, String, Vec<(String, String, i64, String, i64)>) {
    let connection = Connection::open(path).expect("inspect v4 profile after refusal");
    let version = connection
        .query_row("PRAGMA user_version", [], |row| row.get::<_, i64>(0))
        .expect("read v4 schema version");
    let schema = connection
        .query_row(
            "SELECT sql FROM sqlite_master WHERE type = 'table' AND name = 'edit_journal'",
            [],
            |row| row.get::<_, String>(0),
        )
        .expect("read v4 journal schema");
    let rows = connection
        .prepare(
            "SELECT id, note_id, generation, delta_utf8, created_time
             FROM edit_journal ORDER BY rowid",
        )
        .expect("prepare v4 raw journal snapshot")
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, i64>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, i64>(4)?,
            ))
        })
        .expect("read v4 raw journal rows")
        .collect::<Result<Vec<_>, _>>()
        .expect("collect v4 raw journal rows");
    (version, schema, rows)
}

#[test]
fn v3_upgrade_is_atomic_and_does_not_enqueue_imported_history() {
    // Catches migration that partly commits, loses v3 readable data, or schedules legacy rows for upload.
    let profile = tempdir().unwrap();
    let path = profile.path().join("library.sqlite");
    let connection = Connection::open(&path).unwrap();
    seed_native_v3(&connection, false);
    drop(connection);
    let repository = LibraryRepository::open(&path).unwrap();
    let migrated = repository
        .load_note_by_hex("0123456789abcdef0123456789abcdef")
        .unwrap()
        .unwrap();
    assert_eq!(migrated.title, "legacy");
    assert_eq!(migrated.body_html, "<p>body</p>");
    assert_eq!(repository.outbox_count().unwrap(), 0);
    assert_eq!(migrated.revision, 1);
    // The v3 table rebuild happens after the generic v5 column-upgrade pass.
    // Keep this mutation-sensitive: a rebuilt legacy-shaped journal table
    // would otherwise advertise user_version=5 yet fail on the first crash
    // checkpoint after an upgrade.
    repository
        .append_edit_journal(EditJournalEntry {
            note_id: migrated.id.clone(),
            expected_revision: migrated.revision,
            writer_token: "v3-upgrade-checkpoint".into(),
            sequence: 0,
            generation: 1,
            delta_utf8: "{\"version\":2}".into(),
        })
        .expect("v3 upgrade must produce the v5 journal write shape");
    let checkpoint = repository
        .latest_edit_journal_for_revision(&migrated.id, Some(migrated.revision))
        .expect("read v5 checkpoint")
        .expect("v5 checkpoint");
    assert_eq!(checkpoint.writer_token, "v3-upgrade-checkpoint");
    assert!(checkpoint.sequence > 0);
    assert_eq!(
        Connection::open(&path)
            .unwrap()
            .query_row(
                "SELECT count(*) FROM note_revisions WHERE note_id=?1 AND revision=1",
                [migrated.id.as_str()],
                |row| row.get::<_, i64>(0)
            )
            .unwrap(),
        1
    );
    assert_eq!(
        Connection::open(&path)
            .unwrap()
            .query_row(
                "SELECT reason FROM search_queue WHERE note_id=?1",
                [migrated.id.as_str()],
                |row| row.get::<_, String>(0)
            )
            .unwrap(),
        "migration-bootstrap"
    );
    drop(repository);
    let before = std::fs::read(&path).unwrap();
    LibraryRepository::open(&path).unwrap();
    assert_eq!(std::fs::read(&path).unwrap(), before);
}

#[test]
fn deterministic_mid_migration_failure_preserves_v3_version_and_rows() {
    // Catches a forward migration which exposes schema/data writes before its transaction commits.
    let profile = tempdir().unwrap();
    let path = profile.path().join("library.sqlite");
    let connection = Connection::open(&path).unwrap();
    seed_native_v3(&connection, true);
    drop(connection);
    let before = v3_snapshot(&path);
    let before_entries = profile_entries(profile.path());
    assert!(matches!(
        LibraryRepository::open(&path),
        Err(LibraryError::MigrationFailed(_))
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
            .query_row("SELECT title FROM notes", [], |row| row.get::<_, String>(0))
            .unwrap(),
        "legacy"
    );
    assert_eq!(v3_snapshot(&path), before);
    assert_eq!(profile_entries(profile.path()), before_entries);
    assert_eq!(
        Connection::open(&path)
            .unwrap()
            .query_row("PRAGMA journal_mode", [], |row| row.get::<_, String>(0))
            .unwrap()
            .to_ascii_lowercase(),
        "delete"
    );
}

#[test]
fn legacy_refusal_preserves_the_complete_v3_profile_snapshot() {
    // Catches a refusal that leaves a hidden DDL, WAL, resource directory, or legacy field mutation.
    let profile = tempdir().unwrap();
    let path = profile.path().join("library.sqlite");
    let connection = Connection::open(&path).unwrap();
    seed_native_v3(&connection, false);
    connection.execute(
        "UPDATE notes SET body_rtf=X'7b5c727466315c6220626f6c645c6230', markup_language=1, is_draft=1, deleted_time=9",
        [],
    ).unwrap();
    drop(connection);
    let before = v3_snapshot(&path);
    let before_entries = profile_entries(profile.path());
    assert!(matches!(
        LibraryRepository::open(&path),
        Err(LibraryError::LegacyRtfMigrationRequired)
    ));
    assert_eq!(v3_snapshot(&path), before);
    assert_eq!(profile_entries(profile.path()), before_entries);
}

#[cfg(unix)]
#[test]
fn open_refuses_database_path_replaced_with_symlink() {
    // Catches a migration that follows a path replacement outside the chosen profile.
    use std::os::unix::fs::symlink;
    let profile = tempdir().unwrap();
    let outside = tempdir().unwrap();
    let path = profile.path().join("library.sqlite");
    Connection::open(outside.path().join("outside.sqlite")).unwrap();
    symlink(outside.path().join("outside.sqlite"), &path).unwrap();
    assert!(matches!(
        LibraryRepository::open(&path),
        Err(LibraryError::InvalidDatabasePath)
    ));
}

#[cfg(unix)]
#[test]
fn unsafe_resource_preflight_leaves_v3_database_unmigrated() {
    // Catches publishing schema v4 before the profile resource root has been safety-bound.
    use std::os::unix::fs::symlink;
    let profile = tempdir().unwrap();
    let outside = tempdir().unwrap();
    let path = profile.path().join("library.sqlite");
    let connection = Connection::open(&path).unwrap();
    seed_native_v3(&connection, false);
    drop(connection);
    symlink(outside.path(), profile.path().join("resources")).unwrap();
    assert!(LibraryRepository::open(&path).is_err());
    let check = Connection::open(&path).unwrap();
    assert_eq!(
        check
            .query_row("PRAGMA user_version", [], |row| row.get::<_, i64>(0))
            .unwrap(),
        3
    );
    assert_eq!(
        check
            .query_row("SELECT count(*) FROM notes", [], |row| row.get::<_, i64>(0))
            .unwrap(),
        1
    );
}

#[test]
fn failed_preflight_removes_only_the_nested_blob_directory_it_created() {
    // Catches a ResourceStore constructor that cleans up a new resources root
    // but leaks `resources/blobs` when resources already existed.
    let profile = tempdir().unwrap();
    let path = profile.path().join("library.sqlite");
    std::fs::create_dir(profile.path().join("resources")).unwrap();
    let connection = Connection::open(&path).unwrap();
    seed_native_v3(&connection, true);
    drop(connection);
    let before_entries = profile_entries(profile.path());
    assert!(matches!(
        LibraryRepository::open(&path),
        Err(LibraryError::MigrationFailed(_))
    ));
    assert_eq!(profile_entries(profile.path()), before_entries);
    assert!(!profile.path().join("resources/blobs").exists());
    let check = Connection::open(&path).unwrap();
    assert_eq!(
        check
            .query_row("PRAGMA user_version", [], |row| row.get::<_, i64>(0))
            .unwrap(),
        3
    );
}

fn seed_native_v3(connection: &Connection, incompatible_notebooks: bool) {
    // This is the app-lite-native v3 table shape, not a minimal stand-in.
    connection.execute_batch("CREATE TABLE notes (id TEXT PRIMARY KEY NOT NULL, title TEXT NOT NULL DEFAULT '', body TEXT NOT NULL DEFAULT '', body_text TEXT NOT NULL DEFAULT '', body_rtf BLOB NOT NULL DEFAULT X'', markup_language INTEGER NOT NULL DEFAULT 2, is_draft INTEGER NOT NULL DEFAULT 0, created_time INTEGER NOT NULL, updated_time INTEGER NOT NULL, deleted_time INTEGER NOT NULL DEFAULT 0);
CREATE TABLE resource_blobs (sha256 TEXT PRIMARY KEY NOT NULL, size INTEGER NOT NULL DEFAULT 0, mime TEXT NOT NULL DEFAULT '', relative_path TEXT NOT NULL DEFAULT '', created_time INTEGER NOT NULL DEFAULT 0);
CREATE TABLE resources (id TEXT PRIMARY KEY NOT NULL, sha256 TEXT NOT NULL REFERENCES resource_blobs(sha256), title TEXT NOT NULL DEFAULT '', mime TEXT NOT NULL DEFAULT '', file_extension TEXT NOT NULL DEFAULT '', created_time INTEGER NOT NULL DEFAULT 0, size INTEGER NOT NULL DEFAULT 0, updated_time INTEGER NOT NULL DEFAULT 0, deleted_time INTEGER NOT NULL DEFAULT 0);
CREATE TABLE note_resources (note_id TEXT NOT NULL REFERENCES notes(id) ON DELETE CASCADE, resource_id TEXT NOT NULL REFERENCES resources(id) ON DELETE RESTRICT, position INTEGER NOT NULL DEFAULT 0, is_associated INTEGER NOT NULL DEFAULT 1, last_seen_time INTEGER NOT NULL DEFAULT 0, PRIMARY KEY (note_id, resource_id));
INSERT INTO notes VALUES ('0123456789abcdef0123456789abcdef', 'legacy', '<p>body</p>', 'body', X'', 2, 0, 1, 2, 0);
PRAGMA user_version = 3;").unwrap();
    if incompatible_notebooks {
        connection
            .execute_batch("CREATE TABLE notebooks (wrong TEXT);")
            .unwrap();
    }
}

fn v3_snapshot(path: &std::path::Path) -> String {
    let connection = Connection::open(path).unwrap();
    let master = connection
        .prepare("SELECT type,name,tbl_name,sql FROM sqlite_master ORDER BY type,name")
        .unwrap()
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, Option<String>>(3)?,
            ))
        })
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap();
    let columns = [
        "notes",
        "resource_blobs",
        "resources",
        "note_resources",
        "notebooks",
    ]
    .iter()
    .map(|table| {
        let sql = format!("PRAGMA table_info({table})");
        let values = connection
            .prepare(&sql)
            .unwrap()
            .query_map([], |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, i64>(3)?,
                    row.get::<_, Option<String>>(4)?,
                    row.get::<_, i64>(5)?,
                ))
            })
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap();
        ((*table).to_owned(), values)
    })
    .collect::<Vec<_>>();
    let rows = connection.prepare("SELECT id,title,body,body_text,hex(body_rtf),markup_language,is_draft,created_time,updated_time,deleted_time FROM notes ORDER BY id").unwrap()
        .query_map([], |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?, row.get::<_, String>(2)?, row.get::<_, String>(3)?, row.get::<_, String>(4)?, row.get::<_, i64>(5)?, row.get::<_, i64>(6)?, row.get::<_, i64>(7)?, row.get::<_, i64>(8)?, row.get::<_, i64>(9)?))).unwrap()
        .collect::<Result<Vec<_>, _>>().unwrap();
    let journal: String = connection
        .query_row("PRAGMA journal_mode", [], |row| row.get(0))
        .unwrap();
    let version: i64 = connection
        .query_row("PRAGMA user_version", [], |row| row.get(0))
        .unwrap();
    let blobs = connection
        .prepare("SELECT sha256,size,mime,relative_path,created_time FROM resource_blobs ORDER BY sha256")
        .unwrap()
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, i64>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, i64>(4)?,
            ))
        })
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap();
    let resources = connection
        .prepare("SELECT id,sha256,title,mime,file_extension,created_time,size,updated_time,deleted_time FROM resources ORDER BY id")
        .unwrap()
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?, row.get::<_, String>(1)?, row.get::<_, String>(2)?,
                row.get::<_, String>(3)?, row.get::<_, String>(4)?, row.get::<_, i64>(5)?,
                row.get::<_, i64>(6)?, row.get::<_, i64>(7)?, row.get::<_, i64>(8)?,
            ))
        })
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap();
    let associations = connection
        .prepare("SELECT note_id,resource_id,position,is_associated,last_seen_time FROM note_resources ORDER BY note_id,position,resource_id")
        .unwrap()
        .query_map([], |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?, row.get::<_, i64>(2)?, row.get::<_, i64>(3)?, row.get::<_, i64>(4)?)))
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap();
    format!(
        "{master:?}|{columns:?}|{rows:?}|{blobs:?}|{resources:?}|{associations:?}|{journal}|{version}"
    )
}

fn profile_entries(path: &std::path::Path) -> Vec<String> {
    fn collect(root: &std::path::Path, current: &std::path::Path, entries: &mut Vec<String>) {
        for item in std::fs::read_dir(current).unwrap() {
            let item = item.unwrap();
            let file_type = item.file_type().unwrap();
            let relative = item
                .path()
                .strip_prefix(root)
                .unwrap()
                .display()
                .to_string();
            // sqlite_master/table rows/PRAGMAs above are the semantic database
            // snapshot. Switching WAL then restoring DELETE may legitimately
            // change SQLite's internal header bytes, so the profile-tree hash
            // separately guards everything *around* the main database (and
            // still catches any leaked -wal/-shm/resource entries).
            if relative == "library.sqlite" {
                continue;
            }
            if file_type.is_dir() {
                entries.push(format!("d:{relative}"));
                collect(root, &item.path(), entries);
            } else if file_type.is_file() {
                let bytes = std::fs::read(item.path()).unwrap();
                entries.push(format!(
                    "f:{relative}:{}:{:x}",
                    bytes.len(),
                    Sha256::digest(bytes)
                ));
            } else {
                entries.push(format!("other:{relative}"));
            }
        }
    }
    let mut entries = Vec::new();
    collect(path, path, &mut entries);
    entries.sort();
    entries
}

#[cfg(feature = "test-support")]
#[test]
fn migration_gate_blocks_a_second_v3_writer_before_html_publication() {
    // Catches checking legacy RTF outside the BEGIN IMMEDIATE lock.
    let profile = tempdir().unwrap();
    let path = profile.path().join("library.sqlite");
    let seed = Connection::open(&path).unwrap();
    seed_native_v3(&seed, false);
    drop(seed);
    let (entered_tx, entered_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();
    let release_rx = Arc::new(Mutex::new(release_rx));
    let gate_receiver = Arc::clone(&release_rx);
    let hook: OpenTestHook = Arc::new(move |phase| {
        if phase == OpenTestPhase::AfterLegacyGate {
            entered_tx.send(()).unwrap();
            gate_receiver.lock().unwrap().recv().unwrap();
        }
    });
    let worker_path = path.clone();
    let opener = thread::spawn(move || {
        LibraryRepository::open_with_sources_and_hook(
            worker_path,
            Arc::new(FixedClock),
            Arc::new(FixedIds),
            hook,
        )
    });
    entered_rx.recv().unwrap();
    let writer = Connection::open(&path).unwrap();
    let write = writer.execute("INSERT INTO notes VALUES ('11111111111111111111111111111111', 'rtf', '<p>fallback</p>', 'fallback', X'7b5c727466317d', 1, 1, 1, 1, 0)", []);
    assert!(
        write.is_err(),
        "writer crossed the migration gate: {write:?}"
    );
    drop(writer);
    release_tx.send(()).unwrap();
    let repository = opener.join().unwrap().unwrap();
    drop(repository);
    let check = Connection::open(&path).unwrap();
    assert_eq!(
        check
            .query_row("PRAGMA user_version", [], |row| row.get::<_, i64>(0))
            .unwrap(),
        SCHEMA_VERSION
    );
    assert_eq!(
        check
            .query_row("SELECT count(*) FROM notes", [], |row| row.get::<_, i64>(0))
            .unwrap(),
        1
    );
}

#[cfg(all(feature = "test-support", unix))]
#[test]
fn sqlite_connection_aba_swap_is_rejected_before_wal_or_schema_writes() {
    // Catches a pathname/inode ABA: A is bound, SQLite opens B, then A is
    // restored at the selected pathname before ordinary identity checks run.
    let profile = tempdir().unwrap();
    let path = profile.path().join("library.sqlite");
    let a = Connection::open(&path).unwrap();
    seed_native_v3(&a, false);
    drop(a);
    let before_a = v3_snapshot(&path);

    let staged_b = profile.path().join("staged-b.sqlite");
    let b = Connection::open(&staged_b).unwrap();
    seed_native_v3(&b, false);
    b.execute("UPDATE notes SET title='replacement-b'", [])
        .unwrap();
    drop(b);
    let before_b = v3_snapshot(&staged_b);

    let held_a = profile.path().join("held-a.sqlite");
    let opened_b = profile.path().join("opened-b.sqlite");
    let before_path = path.clone();
    let after_path = path.clone();
    let before_held_a = held_a.clone();
    let before_staged_b = staged_b.clone();
    let after_held_a = held_a.clone();
    let after_opened_b = opened_b.clone();
    let hook: OpenTestHook = Arc::new(move |phase| match phase {
        OpenTestPhase::BeforeSqliteOpen => {
            std::fs::rename(&before_path, &before_held_a).unwrap();
            std::fs::rename(&before_staged_b, &before_path).unwrap();
        }
        OpenTestPhase::AfterSqliteOpen => {
            std::fs::rename(&after_path, &after_opened_b).unwrap();
            std::fs::rename(&after_held_a, &after_path).unwrap();
        }
        _ => {}
    });

    assert!(matches!(
        LibraryRepository::open_with_sources_and_hook(
            &path,
            Arc::new(FixedClock),
            Arc::new(FixedIds),
            hook,
        ),
        Err(LibraryError::InvalidDatabasePath)
    ));
    assert_eq!(v3_snapshot(&path), before_a);
    assert_eq!(v3_snapshot(&opened_b), before_b);
    let b_bytes = std::fs::read(&opened_b).unwrap();
    assert_eq!(
        profile_entries(profile.path()),
        vec![format!(
            "f:opened-b.sqlite:{}:{:x}",
            b_bytes.len(),
            Sha256::digest(b_bytes)
        )]
    );
    assert!(!profile.path().join("resources").exists());
}

#[cfg(all(feature = "test-support", unix))]
#[test]
fn failed_fresh_open_leaves_its_unpublished_database_for_retry() {
    // A failed claim must not race a same-name replacement during Drop. Keeping
    // an empty, un-published child is the deliberate recoverable outcome.
    let profile = tempdir().unwrap();
    let path = profile.path().join("library.sqlite");
    let resources = profile.path().join("resources");
    let target = profile.path().join("outside-resources");
    let hook: OpenTestHook = Arc::new(move |phase| {
        if phase == OpenTestPhase::AfterSqliteOpen {
            std::os::unix::fs::symlink(&target, &resources).unwrap();
        }
    });

    assert!(
        LibraryRepository::open_with_sources_and_hook(
            &path,
            Arc::new(FixedClock),
            Arc::new(FixedIds),
            hook,
        )
        .is_err()
    );
    assert!(path.is_file());
    assert_eq!(
        Connection::open(&path)
            .unwrap()
            .pragma_query_value(None, "user_version", |row| row.get::<_, i64>(0))
            .unwrap(),
        0
    );
    assert!(!profile.path().join("library.sqlite-wal").exists());
    assert!(!profile.path().join("library.sqlite-shm").exists());
}

#[cfg(all(feature = "test-support", unix))]
#[test]
fn database_file_swap_after_sqlite_open_aborts_before_wal_or_schema_writes() {
    // Catches retaining only the parent directory identity while SQLite keeps
    // a pathname-opened, now-unlinked database inode alive for migration.
    let profile = tempdir().unwrap();
    let path = profile.path().join("library.sqlite");
    let seed = Connection::open(&path).unwrap();
    seed_native_v3(&seed, false);
    drop(seed);
    let before = v3_snapshot(&path);
    let original = profile.path().join("original.sqlite");
    let replacement = path.clone();
    let hook_original = original.clone();
    let hook: OpenTestHook = Arc::new(move |phase| {
        if phase == OpenTestPhase::AfterSqliteOpen {
            std::fs::rename(&replacement, &hook_original).unwrap();
            drop(Connection::open(&replacement).unwrap());
        }
    });

    assert!(matches!(
        LibraryRepository::open_with_sources_and_hook(
            &path,
            Arc::new(FixedClock),
            Arc::new(FixedIds),
            hook,
        ),
        Err(LibraryError::InvalidDatabasePath)
    ));
    assert_eq!(v3_snapshot(&original), before);
    for sidecar in [
        original.with_extension("sqlite-wal"),
        original.with_extension("sqlite-shm"),
        path.with_extension("sqlite-wal"),
        path.with_extension("sqlite-shm"),
    ] {
        assert!(
            !sidecar.exists(),
            "database-file swap reached a WAL/schema pathname side effect: {sidecar:?}"
        );
    }
    assert!(!profile.path().join("resources").exists());
    let replacement = Connection::open(&path).unwrap();
    assert_eq!(
        replacement
            .query_row("PRAGMA user_version", [], |row| row.get::<_, i64>(0))
            .unwrap(),
        0
    );
    assert_eq!(
        replacement
            .query_row(
                "SELECT count(*) FROM sqlite_master WHERE type='table'",
                [],
                |row| row.get::<_, i64>(0)
            )
            .unwrap(),
        0
    );
}

#[cfg(all(feature = "test-support", unix))]
#[test]
fn migration_commit_returns_bound_repository_when_selected_profile_is_replaced() {
    // Catches a post-commit pathname check returning an apparent migration
    // failure even though the old, already-published database is readable.
    let root = tempdir().unwrap();
    let profile = root.path().join("profile");
    std::fs::create_dir(&profile).unwrap();
    let path = profile.join("library.sqlite");
    let seed = Connection::open(&path).unwrap();
    seed_native_v3(&seed, false);
    drop(seed);
    let moved = root.path().join("published-profile");
    let swap_profile = profile.clone();
    let hook_moved = moved.clone();
    let hook: OpenTestHook = Arc::new(move |phase| {
        if phase == OpenTestPhase::AfterMigrationCommit {
            std::fs::rename(&swap_profile, &hook_moved).unwrap();
            std::fs::create_dir(&swap_profile).unwrap();
        }
    });

    let repository = LibraryRepository::open_with_sources_and_hook(
        &path,
        Arc::new(FixedClock),
        Arc::new(FixedIds),
        hook,
    )
    .expect("a committed migration must not be reported as an open failure");
    assert_eq!(
        repository
            .load_note_by_hex("0123456789abcdef0123456789abcdef")
            .unwrap()
            .unwrap()
            .title,
        "legacy"
    );
    drop(repository);
    let published = Connection::open(moved.join("library.sqlite")).unwrap();
    assert_eq!(
        published
            .query_row("PRAGMA user_version", [], |row| row.get::<_, i64>(0))
            .unwrap(),
        SCHEMA_VERSION
    );
}

#[cfg(all(feature = "test-support", unix))]
#[test]
fn profile_swaps_at_both_open_gaps_abort_before_v5_publication() {
    // Catches pairing SQLite and resources with different profile directories.
    for phase in [
        OpenTestPhase::AfterProfileBound,
        OpenTestPhase::AfterSqliteOpen,
        OpenTestPhase::BeforeMigrationCommit,
    ] {
        let root = tempdir().unwrap();
        let profile = root.path().join("profile");
        std::fs::create_dir(&profile).unwrap();
        let path = profile.join("library.sqlite");
        let seed = Connection::open(&path).unwrap();
        seed_native_v3(&seed, false);
        drop(seed);
        let original = root.path().join("original");
        let replacement = profile.clone();
        let fired = Arc::new(Mutex::new(false));
        let fired_hook = Arc::clone(&fired);
        let hook_original = original.clone();
        let hook_replacement = replacement.clone();
        let hook: OpenTestHook = Arc::new(move |current| {
            if current == phase {
                std::fs::rename(&hook_replacement, &hook_original).unwrap();
                std::fs::create_dir(&hook_replacement).unwrap();
                *fired_hook.lock().unwrap() = true;
            }
        });
        let result = LibraryRepository::open_with_sources_and_hook(
            &path,
            Arc::new(FixedClock),
            Arc::new(FixedIds),
            hook,
        );
        let error = result.err();
        assert!(
            matches!(error, Some(LibraryError::InvalidDatabasePath)),
            "{phase:?}: {error:?}"
        );
        assert!(*fired.lock().unwrap());
        let original_db = Connection::open(original.join("library.sqlite")).unwrap();
        assert_eq!(
            original_db
                .query_row("PRAGMA user_version", [], |row| row.get::<_, i64>(0))
                .unwrap(),
            3
        );
        assert!(!replacement.join("resources").exists());
    }
}

#[cfg(all(feature = "test-support", unix))]
#[test]
fn lexical_profile_bind_refuses_a_symlink_swap_before_sqlite_opens() {
    // Catches a metadata/canonicalize/open chain that follows a replacement
    // parent before it has acquired O_DIRECTORY|O_NOFOLLOW.
    use std::os::unix::fs::symlink;
    let root = tempdir().unwrap();
    let profile = root.path().join("profile");
    std::fs::create_dir(&profile).unwrap();
    let path = profile.join("library.sqlite");
    let seed = Connection::open(&path).unwrap();
    seed_native_v3(&seed, false);
    drop(seed);
    let outside = tempdir().unwrap();
    let moved = root.path().join("original");
    let swap_profile = profile.clone();
    let hook_outside = outside.path().to_owned();
    let hook_moved = moved.clone();
    let hook: OpenTestHook = Arc::new(move |phase| {
        if phase == OpenTestPhase::BeforeProfileBind {
            std::fs::rename(&swap_profile, &hook_moved).unwrap();
            symlink(&hook_outside, &swap_profile).unwrap();
        }
    });
    assert!(matches!(
        LibraryRepository::open_with_sources_and_hook(
            &path,
            Arc::new(FixedClock),
            Arc::new(FixedIds),
            hook,
        ),
        Err(LibraryError::InvalidDatabasePath)
    ));
    assert!(!outside.path().join("library.sqlite").exists());
    let original = Connection::open(moved.join("library.sqlite")).unwrap();
    assert_eq!(
        original
            .query_row("PRAGMA user_version", [], |row| row.get::<_, i64>(0))
            .unwrap(),
        3
    );
}
