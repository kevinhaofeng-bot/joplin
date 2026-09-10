use app_lite_core::{LibraryError, LibraryRepository};
use rusqlite::Connection;
use tempfile::tempdir;

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
}

#[test]
fn deterministic_mid_migration_failure_preserves_v3_version_and_rows() {
    // Catches a forward migration which exposes schema/data writes before its transaction commits.
    let profile = tempdir().unwrap();
    let path = profile.path().join("library.sqlite");
    let connection = Connection::open(&path).unwrap();
    seed_native_v3(&connection, true);
    drop(connection);
    assert!(matches!(
        LibraryRepository::open(&path),
        Err(LibraryError::MigrationFailed)
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
