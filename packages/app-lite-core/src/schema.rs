use crate::{CanonicalDocument, repository::LibraryError, resource::ResourceStore};
use rusqlite::{Connection, OptionalExtension, Transaction, TransactionBehavior, params};

pub const SCHEMA_VERSION: i64 = 5;

pub(crate) fn migrate_schema(
    connection: &mut Connection,
    next_id: &mut dyn FnMut() -> Result<String, LibraryError>,
    preflight: impl FnOnce() -> Result<ResourceStore, LibraryError>,
    verify_profile: impl Fn(&Transaction<'_>) -> Result<(), LibraryError>,
    after_legacy_gate: impl Fn(),
    before_commit: impl Fn(),
    after_migration_commit: impl Fn(),
) -> Result<(bool, ResourceStore), LibraryError> {
    // BEGIN IMMEDIATE is deliberately the first migration operation. It keeps
    // the legacy-RTF gate authoritative until the schema publication commits.
    let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let version: i64 = transaction.query_row("PRAGMA user_version", [], |row| row.get(0))?;
    if version > SCHEMA_VERSION {
        return Err(LibraryError::UnsupportedSchema(version));
    }
    if version == 3 && column_exists(&transaction, "notes", "markup_language")? {
        let legacy: i64 = transaction.query_row(
            "SELECT EXISTS(SELECT 1 FROM notes WHERE markup_language = 1)",
            [],
            |row| row.get(0),
        )?;
        if legacy != 0 {
            return Err(LibraryError::LegacyRtfMigrationRequired);
        }
    }
    after_legacy_gate();
    // Resource binding is intentionally after the legacy gate but before any
    // schema/data mutation, while this migration-wide lock is still held.
    let mut resource_store = preflight()?;
    if version == SCHEMA_VERSION {
        verify_profile(&transaction)?;
        transaction.commit()?;
        resource_store.mark_published();
        return Ok((false, resource_store));
    }
    transaction.execute_batch("CREATE TABLE IF NOT EXISTS stacks (id TEXT PRIMARY KEY NOT NULL, title TEXT NOT NULL, revision INTEGER NOT NULL DEFAULT 1, created_time INTEGER NOT NULL DEFAULT 0, updated_time INTEGER NOT NULL DEFAULT 0, deleted_time INTEGER NOT NULL DEFAULT 0);
CREATE TABLE IF NOT EXISTS notebooks (id TEXT PRIMARY KEY NOT NULL, title TEXT NOT NULL, stack_id TEXT REFERENCES stacks(id) ON DELETE SET NULL, is_default INTEGER NOT NULL DEFAULT 0, revision INTEGER NOT NULL DEFAULT 1, created_time INTEGER NOT NULL DEFAULT 0, updated_time INTEGER NOT NULL DEFAULT 0, deleted_time INTEGER NOT NULL DEFAULT 0);
CREATE TABLE IF NOT EXISTS resource_blobs (sha256 TEXT PRIMARY KEY NOT NULL, size INTEGER NOT NULL, mime TEXT NOT NULL, relative_path TEXT NOT NULL, created_time INTEGER NOT NULL, revision INTEGER NOT NULL DEFAULT 1);
CREATE TABLE IF NOT EXISTS resources (id TEXT PRIMARY KEY NOT NULL, sha256 TEXT NOT NULL REFERENCES resource_blobs(sha256), title TEXT NOT NULL, mime TEXT NOT NULL, file_extension TEXT NOT NULL, size INTEGER NOT NULL, created_time INTEGER NOT NULL, updated_time INTEGER NOT NULL, deleted_time INTEGER NOT NULL DEFAULT 0, revision INTEGER NOT NULL DEFAULT 1);
CREATE TABLE IF NOT EXISTS notes (id TEXT PRIMARY KEY NOT NULL, title TEXT NOT NULL DEFAULT '', body_html TEXT NOT NULL DEFAULT '', body_text TEXT NOT NULL DEFAULT '', snippet TEXT NOT NULL DEFAULT '', notebook_id TEXT NOT NULL REFERENCES notebooks(id) ON DELETE RESTRICT, selected_thumbnail_id TEXT REFERENCES resources(id) ON DELETE SET NULL, merge_state BLOB, created_time INTEGER NOT NULL DEFAULT 0, updated_time INTEGER NOT NULL DEFAULT 0, deleted_time INTEGER NOT NULL DEFAULT 0, revision INTEGER NOT NULL DEFAULT 1);
CREATE TABLE IF NOT EXISTS tags (id TEXT PRIMARY KEY NOT NULL, title TEXT NOT NULL, revision INTEGER NOT NULL DEFAULT 1, created_time INTEGER NOT NULL DEFAULT 0, updated_time INTEGER NOT NULL DEFAULT 0, deleted_time INTEGER NOT NULL DEFAULT 0);
CREATE TABLE IF NOT EXISTS note_tags (note_id TEXT NOT NULL REFERENCES notes(id) ON DELETE CASCADE, tag_id TEXT NOT NULL REFERENCES tags(id) ON DELETE CASCADE, position INTEGER NOT NULL DEFAULT 0, PRIMARY KEY(note_id, tag_id));
CREATE TABLE IF NOT EXISTS note_resources (note_id TEXT NOT NULL REFERENCES notes(id) ON DELETE CASCADE, position INTEGER NOT NULL, resource_id TEXT NOT NULL REFERENCES resources(id) ON DELETE RESTRICT, is_associated INTEGER NOT NULL DEFAULT 1, PRIMARY KEY(note_id, position));
CREATE TABLE IF NOT EXISTS note_revisions (note_id TEXT NOT NULL, revision INTEGER NOT NULL, title TEXT NOT NULL, body_html TEXT NOT NULL, body_text TEXT NOT NULL, created_time INTEGER NOT NULL, PRIMARY KEY(note_id, revision));
CREATE TABLE IF NOT EXISTS edit_journal (id TEXT PRIMARY KEY NOT NULL, note_id TEXT NOT NULL REFERENCES notes(id) ON DELETE CASCADE, expected_revision INTEGER NOT NULL DEFAULT 1, writer_token TEXT NOT NULL DEFAULT '', sequence INTEGER NOT NULL DEFAULT 0, generation INTEGER NOT NULL, delta_utf8 TEXT NOT NULL, created_time INTEGER NOT NULL);
CREATE TABLE IF NOT EXISTS journal_sequence (id INTEGER PRIMARY KEY CHECK(id = 1), next_sequence INTEGER NOT NULL);
CREATE TABLE IF NOT EXISTS search_queue (note_id TEXT PRIMARY KEY NOT NULL, updated_time INTEGER NOT NULL, reason TEXT NOT NULL);
CREATE TABLE IF NOT EXISTS sync_outbox (id TEXT PRIMARY KEY NOT NULL, entity_type TEXT NOT NULL, entity_id TEXT NOT NULL, entity_revision INTEGER NOT NULL, operation TEXT NOT NULL, created_time INTEGER NOT NULL);
CREATE TABLE IF NOT EXISTS sync_cursor (name TEXT PRIMARY KEY NOT NULL, cursor TEXT NOT NULL, updated_time INTEGER NOT NULL);
CREATE TABLE IF NOT EXISTS sync_conflicts (id TEXT PRIMARY KEY NOT NULL, entity_id TEXT NOT NULL, local_revision INTEGER NOT NULL, remote_revision INTEGER NOT NULL, created_time INTEGER NOT NULL, resolved_time INTEGER NOT NULL DEFAULT 0);
CREATE TABLE IF NOT EXISTS shortcuts (id TEXT PRIMARY KEY NOT NULL, entity_type TEXT NOT NULL, entity_id TEXT NOT NULL, position INTEGER NOT NULL, created_time INTEGER NOT NULL);
CREATE TABLE IF NOT EXISTS search_history (query TEXT PRIMARY KEY NOT NULL, last_used_time INTEGER NOT NULL, use_count INTEGER NOT NULL DEFAULT 1);
CREATE TABLE IF NOT EXISTS settings (key TEXT PRIMARY KEY NOT NULL, value TEXT NOT NULL, updated_time INTEGER NOT NULL);
CREATE TABLE IF NOT EXISTS tombstones (entity_type TEXT NOT NULL, entity_id TEXT NOT NULL, final_revision INTEGER NOT NULL, deleted_time INTEGER NOT NULL, purged_time INTEGER NOT NULL, PRIMARY KEY(entity_type, entity_id));
CREATE INDEX IF NOT EXISTS notes_list_idx ON notes(deleted_time, updated_time DESC, id ASC); CREATE INDEX IF NOT EXISTS note_resources_position_idx ON note_resources(note_id, position, resource_id); CREATE INDEX IF NOT EXISTS note_resources_resource_idx ON note_resources(resource_id); CREATE INDEX IF NOT EXISTS note_tags_position_idx ON note_tags(note_id, position, tag_id); CREATE INDEX IF NOT EXISTS sync_outbox_entity_idx ON sync_outbox(entity_type, entity_id, entity_revision);")?;
    for (table, column, definition) in [
        ("notes", "body_html", "TEXT NOT NULL DEFAULT ''"),
        ("notes", "snippet", "TEXT NOT NULL DEFAULT ''"),
        ("notes", "notebook_id", "TEXT NOT NULL DEFAULT ''"),
        ("notes", "selected_thumbnail_id", "TEXT"),
        ("notes", "merge_state", "BLOB"),
        ("notes", "revision", "INTEGER NOT NULL DEFAULT 1"),
        ("resources", "revision", "INTEGER NOT NULL DEFAULT 1"),
        ("resource_blobs", "revision", "INTEGER NOT NULL DEFAULT 1"),
        (
            "edit_journal",
            "expected_revision",
            "INTEGER NOT NULL DEFAULT 1",
        ),
        ("edit_journal", "writer_token", "TEXT NOT NULL DEFAULT ''"),
        ("edit_journal", "sequence", "INTEGER NOT NULL DEFAULT 0"),
    ] {
        ensure_column(&transaction, table, column, definition)?;
    }
    transaction.execute_batch("CREATE TABLE IF NOT EXISTS journal_sequence (id INTEGER PRIMARY KEY CHECK(id = 1), next_sequence INTEGER NOT NULL); INSERT OR IGNORE INTO journal_sequence (id, next_sequence) VALUES (1, 0); UPDATE edit_journal SET sequence = rowid WHERE sequence = 0; UPDATE journal_sequence SET next_sequence = (SELECT COALESCE(MAX(sequence), 0) FROM edit_journal) WHERE id = 1; CREATE INDEX IF NOT EXISTS edit_journal_latest_idx ON edit_journal(note_id, expected_revision, sequence DESC);")?;
    let existing_default: Option<String> = transaction
        .query_row(
            "SELECT id FROM notebooks WHERE is_default = 1 AND deleted_time = 0 ORDER BY id LIMIT 1",
            [],
            |row| row.get(0),
        )
        .optional()?;
    let notebook = if let Some(id) = existing_default {
        id
    } else {
        let mut created = None;
        for _ in 0..16 {
            let id = next_id()?;
            if id.len() != 32
                || !id
                    .bytes()
                    .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
            {
                return Err(LibraryError::InvalidId);
            }
            match transaction.execute("INSERT INTO notebooks (id, title, is_default, revision, created_time, updated_time) VALUES (?1, '默认笔记本', 1, 1, 0, 0)", [&id]) {
                Ok(_) => { created = Some(id); break; }
                Err(rusqlite::Error::SqliteFailure(error, _)) if error.code == rusqlite::ErrorCode::ConstraintViolation => continue,
                Err(error) => return Err(error.into()),
            }
        }
        created.ok_or(LibraryError::IdCollisionExhausted)?
    };
    if column_exists(&transaction, "notes", "body")? {
        transaction.execute(
            "UPDATE notes SET body_html = body WHERE body_html = '' AND body <> ''",
            [],
        )?;
    }
    transaction.execute(
        "UPDATE notes SET notebook_id = ?1 WHERE notebook_id = ''",
        [&notebook],
    )?;
    canonicalize_notes(&transaction)?;
    if version == 3 {
        rebuild_v3_notes(&transaction, &notebook)?;
    }
    if version == 3 {
        rebuild_occurrences(&transaction)?;
    }
    // A v3 upgrade rebuilds `edit_journal` after the generic column pass
    // above. Reapply the journal identity/ordering shape to that replacement
    // before publishing v5, otherwise the first crash checkpoint after an
    // apparently successful upgrade fails at runtime.
    ensure_edit_journal_v5(&transaction)?;
    transaction.execute("INSERT OR IGNORE INTO note_revisions (note_id, revision, title, body_html, body_text, created_time) SELECT id, 1, title, body_html, body_text, updated_time FROM notes", [])?;
    transaction.execute("INSERT OR IGNORE INTO search_queue (note_id, updated_time, reason) SELECT id, updated_time, 'migration-bootstrap' FROM notes", [])?;
    transaction.execute_batch("PRAGMA user_version = 5")?;
    before_commit();
    // The test hook models the last pathname/descriptor race.  It must run
    // before the final identity check so a swapped profile aborts the still
    // uncommitted publication.
    verify_profile(&transaction)?;
    transaction.commit()?;
    resource_store.mark_published();
    after_migration_commit();
    Ok((true, resource_store))
}

fn canonicalize_notes(transaction: &Transaction<'_>) -> Result<(), LibraryError> {
    let mut statement = transaction.prepare("SELECT id, body_html FROM notes")?;
    let rows = statement
        .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?
        .collect::<Result<Vec<_>, _>>()?;
    drop(statement);
    for (id, body) in rows {
        let document = CanonicalDocument::parse_html(&body)?;
        let html = document.to_canonical_html().as_str().to_owned();
        let text = document.search_text().as_str().to_owned();
        transaction.execute(
            "UPDATE notes SET body_html=?2, body_text=?3, snippet=?4 WHERE id=?1",
            params![id, html, text, text.chars().take(160).collect::<String>()],
        )?;
    }
    Ok(())
}

fn rebuild_occurrences(transaction: &Transaction<'_>) -> Result<(), LibraryError> {
    transaction.execute_batch("CREATE TABLE note_resources_new (note_id TEXT NOT NULL REFERENCES notes(id) ON DELETE CASCADE, position INTEGER NOT NULL, resource_id TEXT NOT NULL REFERENCES resources(id) ON DELETE RESTRICT, is_associated INTEGER NOT NULL DEFAULT 1, PRIMARY KEY(note_id, position));")?;
    let mut statement = transaction.prepare("SELECT id, body_html FROM notes")?;
    let rows = statement
        .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?
        .collect::<Result<Vec<_>, _>>()?;
    drop(statement);
    for (note, body) in rows {
        let document = CanonicalDocument::parse_html(&body)?;
        for (position, resource) in document.resource_ids().iter().enumerate() {
            let exists: i64 = transaction.query_row(
                "SELECT EXISTS(SELECT 1 FROM resources WHERE id=?1 AND deleted_time=0)",
                [resource.as_str()],
                |r| r.get(0),
            )?;
            if exists == 0 {
                return Err(LibraryError::NotFound);
            }
            transaction.execute("INSERT INTO note_resources_new (note_id, position, resource_id) VALUES (?1, ?2, ?3)", params![note, position as i64, resource.as_str()])?;
        }
    }
    transaction.execute_batch("DROP TABLE note_resources; ALTER TABLE note_resources_new RENAME TO note_resources; CREATE INDEX note_resources_position_idx ON note_resources(note_id, position, resource_id); CREATE INDEX note_resources_resource_idx ON note_resources(resource_id);")?;
    Ok(())
}

fn rebuild_v3_notes(
    transaction: &Transaction<'_>,
    default_notebook: &str,
) -> Result<(), LibraryError> {
    let mut statement = transaction.prepare("SELECT id,title,body_html,body_text,snippet,notebook_id,selected_thumbnail_id,created_time,updated_time,deleted_time,revision FROM notes")?;
    let rows = statement
        .query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, String>(3)?,
                r.get::<_, String>(4)?,
                r.get::<_, String>(5)?,
                r.get::<_, Option<String>>(6)?,
                r.get::<_, i64>(7)?,
                r.get::<_, i64>(8)?,
                r.get::<_, i64>(9)?,
                r.get::<_, i64>(10)?,
            ))
        })?
        .collect::<Result<Vec<_>, _>>()?;
    drop(statement);
    transaction.execute_batch("DROP TABLE note_resources; DROP TABLE note_tags; DROP TABLE edit_journal; DROP TABLE search_queue; ALTER TABLE notes RENAME TO notes_v3_source; CREATE TABLE notes (id TEXT PRIMARY KEY NOT NULL, title TEXT NOT NULL DEFAULT '', body_html TEXT NOT NULL DEFAULT '', body_text TEXT NOT NULL DEFAULT '', snippet TEXT NOT NULL DEFAULT '', notebook_id TEXT NOT NULL REFERENCES notebooks(id) ON DELETE RESTRICT, selected_thumbnail_id TEXT REFERENCES resources(id) ON DELETE SET NULL, merge_state BLOB, created_time INTEGER NOT NULL DEFAULT 0, updated_time INTEGER NOT NULL DEFAULT 0, deleted_time INTEGER NOT NULL DEFAULT 0, revision INTEGER NOT NULL DEFAULT 1);")?;
    for (
        id,
        title,
        html,
        text,
        snippet,
        notebook,
        thumbnail,
        created,
        updated,
        deleted,
        revision,
    ) in rows
    {
        let notebook = if notebook.is_empty() {
            default_notebook
        } else {
            &notebook
        };
        transaction.execute("INSERT INTO notes (id,title,body_html,body_text,snippet,notebook_id,selected_thumbnail_id,created_time,updated_time,deleted_time,revision) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11)", params![id,title,html,text,snippet,notebook,thumbnail,created,updated,deleted,revision])?;
    }
    transaction.execute_batch("DROP TABLE notes_v3_source; CREATE TABLE note_tags (note_id TEXT NOT NULL REFERENCES notes(id) ON DELETE CASCADE, tag_id TEXT NOT NULL REFERENCES tags(id) ON DELETE CASCADE, position INTEGER NOT NULL DEFAULT 0, PRIMARY KEY(note_id,tag_id)); CREATE TABLE note_resources (note_id TEXT NOT NULL REFERENCES notes(id) ON DELETE CASCADE, position INTEGER NOT NULL, resource_id TEXT NOT NULL REFERENCES resources(id) ON DELETE RESTRICT, is_associated INTEGER NOT NULL DEFAULT 1, PRIMARY KEY(note_id,position)); CREATE TABLE edit_journal (id TEXT PRIMARY KEY NOT NULL, note_id TEXT NOT NULL REFERENCES notes(id) ON DELETE CASCADE, generation INTEGER NOT NULL, delta_utf8 TEXT NOT NULL, created_time INTEGER NOT NULL); CREATE TABLE search_queue (note_id TEXT PRIMARY KEY NOT NULL, updated_time INTEGER NOT NULL, reason TEXT NOT NULL);")?;
    Ok(())
}
/// Install the durable journal identity fields after every migration branch
/// that may have recreated the table. `v3` does exactly that while rebuilding
/// notes, so doing this only in the earlier generic pass would leave a profile
/// claiming schema v5 with the legacy journal shape.
fn ensure_edit_journal_v5(transaction: &Transaction<'_>) -> Result<(), LibraryError> {
    for (column, definition) in [
        ("expected_revision", "INTEGER NOT NULL DEFAULT 1"),
        ("writer_token", "TEXT NOT NULL DEFAULT ''"),
        ("sequence", "INTEGER NOT NULL DEFAULT 0"),
    ] {
        ensure_column(transaction, "edit_journal", column, definition)?;
    }
    transaction.execute_batch(
        "CREATE TABLE IF NOT EXISTS journal_sequence (id INTEGER PRIMARY KEY CHECK(id = 1), next_sequence INTEGER NOT NULL);
         INSERT OR IGNORE INTO journal_sequence (id, next_sequence) VALUES (1, 0);
         UPDATE edit_journal SET sequence = rowid WHERE sequence = 0;
         UPDATE journal_sequence SET next_sequence = (SELECT COALESCE(MAX(sequence), 0) FROM edit_journal) WHERE id = 1;
         CREATE INDEX IF NOT EXISTS edit_journal_latest_idx ON edit_journal(note_id, expected_revision, sequence DESC);",
    )?;
    Ok(())
}

fn ensure_column(
    t: &Transaction<'_>,
    table: &str,
    column: &str,
    definition: &str,
) -> Result<(), LibraryError> {
    if !column_exists(t, table, column)? {
        t.execute_batch(&format!(
            "ALTER TABLE {table} ADD COLUMN {column} {definition}"
        ))?;
    }
    Ok(())
}
fn column_exists(t: &Transaction<'_>, table: &str, column: &str) -> Result<bool, LibraryError> {
    let mut s = t.prepare(&format!("PRAGMA table_info({table})"))?;
    Ok(s.query_map([], |r| r.get::<_, String>(1))?
        .collect::<Result<Vec<_>, _>>()?
        .iter()
        .any(|name| name == column))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::LibraryRepository;
    use rusqlite::hooks::{AuthAction, Authorization};
    use std::sync::{Arc, Mutex};
    use tempfile::tempdir;

    #[test]
    fn v4_fast_path_denies_every_logical_write() {
        // Catches a future same-value UPDATE/DDL hidden by unchanged WAL/main bytes.
        let profile = tempdir().unwrap();
        let path = profile.path().join("library.sqlite");
        LibraryRepository::open(&path).unwrap();
        let mut connection = Connection::open(&path).unwrap();
        let writes = Arc::new(Mutex::new(Vec::new()));
        let captured = Arc::clone(&writes);
        connection.authorizer(Some(
            move |context: rusqlite::hooks::AuthContext<'_>| match context.action {
                AuthAction::Insert { .. }
                | AuthAction::Update { .. }
                | AuthAction::Delete { .. }
                | AuthAction::CreateIndex { .. }
                | AuthAction::CreateTable { .. }
                | AuthAction::CreateTempIndex { .. }
                | AuthAction::CreateTempTable { .. }
                | AuthAction::DropIndex { .. }
                | AuthAction::DropTable { .. }
                | AuthAction::Reindex { .. } => {
                    captured
                        .lock()
                        .unwrap()
                        .push(format!("{:?}", context.action));
                    Authorization::Deny
                }
                _ => Authorization::Allow,
            },
        ));
        let mut ids = || Ok("a".repeat(32));
        let result = migrate_schema(
            &mut connection,
            &mut ids,
            || ResourceStore::new(profile.path()).map_err(Into::into),
            |_| Ok(()),
            || {},
            || {},
            || {},
        );
        connection.authorizer(None::<fn(rusqlite::hooks::AuthContext<'_>) -> Authorization>);
        assert!(matches!(result, Ok((false, _))));
        assert!(writes.lock().unwrap().is_empty());
    }
}
