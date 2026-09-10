use crate::{CanonicalDocument, repository::LibraryError};
use rusqlite::{Connection, Transaction};

pub const SCHEMA_VERSION: i64 = 4;

pub(crate) fn migrate_schema(
    connection: &mut Connection,
    default_notebook_id: &str,
) -> Result<(), LibraryError> {
    let version: i64 = connection.query_row("PRAGMA user_version", [], |row| row.get(0))?;
    if version > SCHEMA_VERSION {
        return Err(LibraryError::UnsupportedSchema(version));
    }
    let transaction = connection.transaction()?;
    transaction.execute_batch("CREATE TABLE IF NOT EXISTS notes (id TEXT PRIMARY KEY NOT NULL, title TEXT NOT NULL DEFAULT '', body_html TEXT NOT NULL DEFAULT '', body_text TEXT NOT NULL DEFAULT '', snippet TEXT NOT NULL DEFAULT '', notebook_id TEXT NOT NULL DEFAULT '', selected_thumbnail_id TEXT, merge_state BLOB, created_time INTEGER NOT NULL DEFAULT 0, updated_time INTEGER NOT NULL DEFAULT 0, deleted_time INTEGER NOT NULL DEFAULT 0, revision INTEGER NOT NULL DEFAULT 1);
CREATE TABLE IF NOT EXISTS stacks (id TEXT PRIMARY KEY NOT NULL, title TEXT NOT NULL, revision INTEGER NOT NULL DEFAULT 1, created_time INTEGER NOT NULL DEFAULT 0, updated_time INTEGER NOT NULL DEFAULT 0, deleted_time INTEGER NOT NULL DEFAULT 0);
CREATE TABLE IF NOT EXISTS notebooks (id TEXT PRIMARY KEY NOT NULL, title TEXT NOT NULL, stack_id TEXT, is_default INTEGER NOT NULL DEFAULT 0, revision INTEGER NOT NULL DEFAULT 1, created_time INTEGER NOT NULL DEFAULT 0, updated_time INTEGER NOT NULL DEFAULT 0, deleted_time INTEGER NOT NULL DEFAULT 0);
CREATE TABLE IF NOT EXISTS tags (id TEXT PRIMARY KEY NOT NULL, title TEXT NOT NULL, revision INTEGER NOT NULL DEFAULT 1, created_time INTEGER NOT NULL DEFAULT 0, updated_time INTEGER NOT NULL DEFAULT 0, deleted_time INTEGER NOT NULL DEFAULT 0);
CREATE TABLE IF NOT EXISTS note_tags (note_id TEXT NOT NULL REFERENCES notes(id) ON DELETE CASCADE, tag_id TEXT NOT NULL REFERENCES tags(id) ON DELETE CASCADE, position INTEGER NOT NULL DEFAULT 0, PRIMARY KEY(note_id, tag_id));
CREATE TABLE IF NOT EXISTS resource_blobs (sha256 TEXT PRIMARY KEY NOT NULL, size INTEGER NOT NULL, mime TEXT NOT NULL, relative_path TEXT NOT NULL, created_time INTEGER NOT NULL, revision INTEGER NOT NULL DEFAULT 1);
CREATE TABLE IF NOT EXISTS resources (id TEXT PRIMARY KEY NOT NULL, sha256 TEXT NOT NULL REFERENCES resource_blobs(sha256), title TEXT NOT NULL, mime TEXT NOT NULL, file_extension TEXT NOT NULL, size INTEGER NOT NULL, created_time INTEGER NOT NULL, updated_time INTEGER NOT NULL, deleted_time INTEGER NOT NULL DEFAULT 0, revision INTEGER NOT NULL DEFAULT 1);
CREATE TABLE IF NOT EXISTS note_resources (note_id TEXT NOT NULL REFERENCES notes(id) ON DELETE CASCADE, resource_id TEXT NOT NULL REFERENCES resources(id) ON DELETE RESTRICT, position INTEGER NOT NULL, is_associated INTEGER NOT NULL DEFAULT 1, PRIMARY KEY(note_id, resource_id));
CREATE TABLE IF NOT EXISTS note_revisions (note_id TEXT NOT NULL REFERENCES notes(id) ON DELETE CASCADE, revision INTEGER NOT NULL, title TEXT NOT NULL, body_html TEXT NOT NULL, body_text TEXT NOT NULL, created_time INTEGER NOT NULL, PRIMARY KEY(note_id, revision));
CREATE TABLE IF NOT EXISTS edit_journal (id TEXT PRIMARY KEY NOT NULL, note_id TEXT NOT NULL REFERENCES notes(id) ON DELETE CASCADE, generation INTEGER NOT NULL, delta_utf8 TEXT NOT NULL, created_time INTEGER NOT NULL);
CREATE TABLE IF NOT EXISTS search_queue (note_id TEXT PRIMARY KEY NOT NULL REFERENCES notes(id) ON DELETE CASCADE, updated_time INTEGER NOT NULL, reason TEXT NOT NULL);
CREATE TABLE IF NOT EXISTS sync_outbox (id TEXT PRIMARY KEY NOT NULL, entity_type TEXT NOT NULL, entity_id TEXT NOT NULL, entity_revision INTEGER NOT NULL, operation TEXT NOT NULL, created_time INTEGER NOT NULL);
CREATE TABLE IF NOT EXISTS sync_cursor (name TEXT PRIMARY KEY NOT NULL, cursor TEXT NOT NULL, updated_time INTEGER NOT NULL);
CREATE TABLE IF NOT EXISTS sync_conflicts (id TEXT PRIMARY KEY NOT NULL, entity_id TEXT NOT NULL, local_revision INTEGER NOT NULL, remote_revision INTEGER NOT NULL, created_time INTEGER NOT NULL, resolved_time INTEGER NOT NULL DEFAULT 0);
CREATE TABLE IF NOT EXISTS shortcuts (id TEXT PRIMARY KEY NOT NULL, entity_type TEXT NOT NULL, entity_id TEXT NOT NULL, position INTEGER NOT NULL, created_time INTEGER NOT NULL);
CREATE TABLE IF NOT EXISTS search_history (query TEXT PRIMARY KEY NOT NULL, last_used_time INTEGER NOT NULL, use_count INTEGER NOT NULL DEFAULT 1);
CREATE TABLE IF NOT EXISTS settings (key TEXT PRIMARY KEY NOT NULL, value TEXT NOT NULL, updated_time INTEGER NOT NULL);
CREATE INDEX IF NOT EXISTS notes_list_idx ON notes(deleted_time, updated_time DESC, id ASC);
CREATE INDEX IF NOT EXISTS note_resources_position_idx ON note_resources(note_id, position, resource_id);
CREATE INDEX IF NOT EXISTS note_tags_position_idx ON note_tags(note_id, position, tag_id);
CREATE INDEX IF NOT EXISTS sync_outbox_entity_idx ON sync_outbox(entity_type, entity_id, entity_revision);")?;
    for (table, column, definition) in [
        ("notes", "body_html", "TEXT NOT NULL DEFAULT ''"),
        ("notes", "snippet", "TEXT NOT NULL DEFAULT ''"),
        ("notes", "notebook_id", "TEXT NOT NULL DEFAULT ''"),
        ("notes", "selected_thumbnail_id", "TEXT"),
        ("notes", "merge_state", "BLOB"),
        ("notes", "revision", "INTEGER NOT NULL DEFAULT 1"),
        ("resources", "revision", "INTEGER NOT NULL DEFAULT 1"),
        ("resource_blobs", "revision", "INTEGER NOT NULL DEFAULT 1"),
    ] {
        ensure_column(&transaction, table, column, definition)?;
    }
    transaction.execute("INSERT INTO notebooks (id, title, is_default, revision, created_time, updated_time) SELECT ?1, '默认笔记本', 1, 1, 0, 0 WHERE NOT EXISTS (SELECT 1 FROM notebooks WHERE is_default = 1 AND deleted_time = 0)", [default_notebook_id])?;
    let default_id: String = transaction.query_row(
        "SELECT id FROM notebooks WHERE is_default = 1 AND deleted_time = 0 ORDER BY id LIMIT 1",
        [],
        |row| row.get(0),
    )?;
    if column_exists(&transaction, "notes", "body")? {
        transaction.execute(
            "UPDATE notes SET body_html = body WHERE body_html = '' AND body <> ''",
            [],
        )?;
    }
    let mut statement = transaction.prepare("SELECT id, body_html FROM notes")?;
    let bodies = statement
        .query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })?
        .collect::<Result<Vec<_>, _>>()?;
    drop(statement);
    for (id, body_html) in bodies {
        let document = CanonicalDocument::parse_html(&body_html)?;
        let html = document.to_canonical_html().as_str().to_owned();
        let text = document.search_text().as_str().to_owned();
        transaction.execute(
            "UPDATE notes SET body_html = ?2, body_text = ?3, snippet = ?4 WHERE id = ?1",
            rusqlite::params![id, html, text, text.chars().take(160).collect::<String>()],
        )?;
    }
    transaction.execute(
        "UPDATE notes SET notebook_id = ?1 WHERE notebook_id = ''",
        [default_id],
    )?;
    transaction.execute_batch("PRAGMA user_version = 4;")?;
    transaction.commit()?;
    Ok(())
}

fn ensure_column(
    transaction: &Transaction<'_>,
    table: &str,
    column: &str,
    definition: &str,
) -> Result<(), LibraryError> {
    if !column_exists(transaction, table, column)? {
        transaction.execute_batch(&format!(
            "ALTER TABLE {table} ADD COLUMN {column} {definition}"
        ))?;
    }
    Ok(())
}

fn column_exists(
    transaction: &Transaction<'_>,
    table: &str,
    column: &str,
) -> Result<bool, LibraryError> {
    let mut statement = transaction.prepare(&format!("PRAGMA table_info({table})"))?;
    let names = statement.query_map([], |row| row.get::<_, String>(1))?;
    Ok(names
        .collect::<Result<Vec<_>, _>>()?
        .iter()
        .any(|name| name == column))
}
