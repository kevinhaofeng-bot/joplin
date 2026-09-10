use app_lite_core::{LibraryError, LibraryRepository};
use rusqlite::Connection;
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
fn open_creates_clean_v4_database_idempotently() {
    // Catches a fresh profile missing v4 schema/PRAGMAs or a second open changing it.
    let profile = tempdir().unwrap();
    let path = profile.path().join("library.sqlite");
    LibraryRepository::open(&path).unwrap();
    LibraryRepository::open(&path).unwrap();
    let connection = Connection::open(path).unwrap();
    assert_eq!(
        connection
            .query_row("PRAGMA user_version", [], |row| row.get::<_, i64>(0))
            .unwrap(),
        4
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
        4
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
        4
    );
}

#[cfg(all(feature = "test-support", unix))]
#[test]
fn profile_swaps_at_both_open_gaps_abort_before_v4_publication() {
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
