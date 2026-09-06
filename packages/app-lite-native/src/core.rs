use crate::body::{BodyError, project_search_text};
pub use crate::resource_store::ResourceImport;
use crate::resource_store::{ResourceError, ResourceStore};
use rusqlite::{Connection, OpenFlags, OptionalExtension, params};
use std::path::Path;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};
use thiserror::Error;

static NEXT_ID: AtomicU64 = AtomicU64::new(1);

#[derive(Debug, Error)]
pub enum CoreError {
    #[error("storage error")]
    Storage(#[from] rusqlite::Error),
    #[error("invalid note id")]
    InvalidId,
    #[error("invalid database path")]
    InvalidDatabasePath,
    #[error("body error")]
    Body(#[from] BodyError),
    #[error("resource error")]
    Resource(#[from] ResourceError),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Note {
    pub id: String,
    pub title: String,
    pub body: String,
    pub body_text: String,
    pub body_rtf: Vec<u8>,
    pub is_draft: bool,
    pub created_time: i64,
    pub updated_time: i64,
    pub deleted_time: i64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CreateNote {
    pub title: String,
    pub body: String,
    pub body_rtf: Vec<u8>,
    pub is_draft: bool,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct UpdateNote {
    pub title: Option<String>,
    pub body: Option<String>,
    pub body_rtf: Option<Vec<u8>>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NoteContentUpdate {
    pub title: String,
    pub body: String,
    pub body_text: String,
    pub body_rtf: Vec<u8>,
    pub resource_ids: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoredResource {
    pub id: String,
    pub sha256: String,
    pub size: usize,
    pub title: String,
    pub mime: String,
    pub file_extension: String,
    pub path: std::path::PathBuf,
    pub bytes: Vec<u8>,
}

pub struct NoteRepository {
    connection: Mutex<Connection>,
    resource_store: ResourceStore,
}

impl NoteRepository {
    pub fn open(path: impl AsRef<Path>) -> Result<Self, CoreError> {
        let input_path = path.as_ref();
        ensure_database_inode(input_path)?;
        let parent = input_path.parent().ok_or(CoreError::InvalidDatabasePath)?;
        let file_name = input_path
            .file_name()
            .ok_or(CoreError::InvalidDatabasePath)?;
        let path = std::fs::canonicalize(parent)
            .map_err(|_| CoreError::InvalidDatabasePath)?
            .join(file_name);
        let flags = OpenFlags::SQLITE_OPEN_READ_WRITE
            | OpenFlags::SQLITE_OPEN_CREATE
            | OpenFlags::SQLITE_OPEN_NO_MUTEX
            | OpenFlags::SQLITE_OPEN_NOFOLLOW;
        let connection = Connection::open_with_flags(&path, flags)?;
        connection.pragma_update(None, "journal_mode", "WAL")?;
        connection.pragma_update(None, "foreign_keys", "ON")?;
        migrate_schema(&connection)?;
        let resource_store = ResourceStore::new(
            path.parent()
                .ok_or(CoreError::InvalidDatabasePath)?
                .to_path_buf(),
        )?;
        let repository = Self {
            connection: Mutex::new(connection),
            resource_store,
        };
        repository.rebuild_search_index()?;
        Ok(repository)
    }

    pub fn create_note(&self, input: CreateNote) -> Result<Note, CoreError> {
        let now = timestamp();
        let id = new_id();
        let body_text = project_search_text(&input.body);
        let connection = self.connection.lock().expect("repository mutex poisoned");
        let transaction = connection.unchecked_transaction()?;
        transaction.execute(
            "INSERT INTO notes (id, title, body, body_text, body_rtf, is_draft, created_time, updated_time)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?7)",
            params![id, input.title, input.body, body_text, input.body_rtf, input.is_draft, now],
        )?;
        transaction.commit()?;
        let _ = replace_index_entry(&connection, &id, &input.title, &body_text);
        Ok(Note {
            id,
            title: input.title,
            body: input.body,
            body_text,
            body_rtf: input.body_rtf,
            is_draft: input.is_draft,
            created_time: now,
            updated_time: now,
            deleted_time: 0,
        })
    }

    pub fn get_note(&self, id: &str) -> Result<Option<Note>, CoreError> {
        validate_id(id)?;
        let connection = self.connection.lock().expect("repository mutex poisoned");
        connection
            .query_row(
                "SELECT id, title, body, body_text, body_rtf, is_draft, created_time, updated_time, deleted_time
                 FROM notes WHERE id = ?1 AND deleted_time = 0",
                [id],
                row_to_note,
            )
            .optional()
            .map_err(CoreError::from)
    }

    pub fn list_notes(&self) -> Result<Vec<Note>, CoreError> {
        let connection = self.connection.lock().expect("repository mutex poisoned");
        let mut statement = connection.prepare(
            "SELECT id, title, body, body_text, body_rtf, is_draft, created_time, updated_time, deleted_time
             FROM notes WHERE deleted_time = 0 ORDER BY updated_time DESC, id ASC",
        )?;
        let rows = statement.query_map([], row_to_note)?;
        Ok(rows.collect::<Result<Vec<_>, _>>()?)
    }

    pub fn update_note(&self, id: &str, input: UpdateNote) -> Result<Note, CoreError> {
        validate_id(id)?;
        let connection = self.connection.lock().expect("repository mutex poisoned");
        let transaction = connection.unchecked_transaction()?;
        let current = transaction
            .query_row(
                "SELECT id, title, body, body_text, body_rtf, is_draft, created_time, updated_time, deleted_time
                 FROM notes WHERE id = ?1 AND deleted_time = 0",
                [id],
                row_to_note,
            )
            .optional()?;
        let Some(mut note) = current else {
            return Err(CoreError::InvalidId);
        };
        if let Some(title) = input.title {
            note.title = title;
        }
        if let Some(body) = input.body {
            note.body = body;
            note.body_text = project_search_text(&note.body);
        }
        if let Some(body_rtf) = input.body_rtf {
            note.body_rtf = body_rtf;
        }
        note.updated_time = timestamp().max(note.updated_time + 1);
        transaction.execute(
            "UPDATE notes SET title = ?2, body = ?3, body_text = ?4, body_rtf = ?5,
             is_draft = 0, updated_time = ?6 WHERE id = ?1 AND deleted_time = 0",
            params![
                note.id,
                note.title,
                note.body,
                note.body_text,
                note.body_rtf,
                note.updated_time
            ],
        )?;
        note.is_draft = false;
        transaction.commit()?;
        let _ = replace_index_entry(&connection, &note.id, &note.title, &note.body_text);
        Ok(note)
    }

    pub fn import_resource(&self, input: ResourceImport<'_>) -> Result<StoredResource, CoreError> {
        let blob = self.resource_store.put(input)?;
        let now = timestamp();
        let connection = self.connection.lock().expect("repository mutex poisoned");
        let transaction = connection.unchecked_transaction()?;
        transaction.execute(
            "INSERT OR IGNORE INTO resource_blobs (sha256, size) VALUES (?1, ?2)",
            params![blob.sha256, blob.size as i64],
        )?;
        transaction.execute(
            "INSERT INTO resources (id, sha256, title, mime, file_extension, created_time)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![
                blob.id,
                blob.sha256,
                input.title,
                input.mime,
                input.file_extension,
                now
            ],
        )?;
        transaction.commit()?;
        Ok(StoredResource {
            id: blob.id,
            sha256: blob.sha256,
            size: blob.size,
            title: input.title.to_owned(),
            mime: input.mime.to_owned(),
            file_extension: input.file_extension.to_owned(),
            path: blob.path,
            bytes: input.bytes.to_vec(),
        })
    }

    pub fn get_resource(&self, id: &str) -> Result<Option<StoredResource>, CoreError> {
        validate_id(id)?;
        let metadata = {
            let connection = self.connection.lock().expect("repository mutex poisoned");
            connection
                .query_row(
                    "SELECT r.id, r.sha256, b.size, r.title, r.mime, r.file_extension
                     FROM resources r JOIN resource_blobs b ON b.sha256 = r.sha256
                     WHERE r.id = ?1",
                    [id],
                    |row| {
                        Ok((
                            row.get::<_, String>(0)?,
                            row.get::<_, String>(1)?,
                            row.get::<_, i64>(2)?,
                            row.get::<_, String>(3)?,
                            row.get::<_, String>(4)?,
                            row.get::<_, String>(5)?,
                        ))
                    },
                )
                .optional()?
        };
        let Some((id, sha256, size, title, mime, file_extension)) = metadata else {
            return Ok(None);
        };
        let bytes = self.resource_store.read_blob(&sha256)?;
        Ok(Some(StoredResource {
            id,
            sha256: sha256.clone(),
            size: usize::try_from(size).map_err(|_| ResourceError::CorruptBlob)?,
            title,
            mime,
            file_extension,
            path: self.resource_store.path_for(&sha256),
            bytes,
        }))
    }

    pub fn update_note_content(
        &self,
        id: &str,
        input: NoteContentUpdate,
    ) -> Result<Note, CoreError> {
        validate_id(id)?;
        for resource_id in &input.resource_ids {
            validate_id(resource_id)?;
        }
        let connection = self.connection.lock().expect("repository mutex poisoned");
        let transaction = connection.unchecked_transaction()?;
        let mut note = transaction
            .query_row(
                "SELECT id, title, body, body_text, body_rtf, is_draft, created_time, updated_time, deleted_time
                 FROM notes WHERE id = ?1 AND deleted_time = 0",
                [id],
                row_to_note,
            )
            .optional()?
            .ok_or(CoreError::InvalidId)?;
        for resource_id in &input.resource_ids {
            let exists = transaction.query_row(
                "SELECT EXISTS(SELECT 1 FROM resources WHERE id = ?1)",
                [resource_id],
                |row| row.get::<_, i64>(0),
            )?;
            if exists == 0 {
                return Err(CoreError::InvalidId);
            }
        }
        note.title = input.title;
        note.body = input.body;
        note.body_text = input.body_text;
        note.body_rtf = input.body_rtf;
        note.is_draft = false;
        note.updated_time = timestamp().max(note.updated_time + 1);
        transaction.execute(
            "UPDATE notes SET title = ?2, body = ?3, body_text = ?4, body_rtf = ?5,
             is_draft = 0, updated_time = ?6 WHERE id = ?1 AND deleted_time = 0",
            params![
                note.id,
                note.title,
                note.body,
                note.body_text,
                note.body_rtf,
                note.updated_time
            ],
        )?;
        transaction.execute("DELETE FROM note_resources WHERE note_id = ?1", [id])?;
        for (position, resource_id) in input.resource_ids.iter().enumerate() {
            transaction.execute(
                "INSERT INTO note_resources (note_id, resource_id, position) VALUES (?1, ?2, ?3)",
                params![id, resource_id, position as i64],
            )?;
        }
        transaction.commit()?;
        let _ = replace_index_entry(&connection, &note.id, &note.title, &note.body_text);
        Ok(note)
    }

    pub fn soft_delete(&self, id: &str) -> Result<(), CoreError> {
        validate_id(id)?;
        let connection = self.connection.lock().expect("repository mutex poisoned");
        let transaction = connection.unchecked_transaction()?;
        transaction.execute(
            "UPDATE notes SET deleted_time = ?2, updated_time = ?2
             WHERE id = ?1 AND deleted_time = 0",
            params![id, timestamp()],
        )?;
        transaction.commit()?;
        let _ = connection.execute("DELETE FROM notes_fts WHERE id = ?1", [id]);
        Ok(())
    }

    pub fn cleanup_abandoned_drafts(&self) -> Result<usize, CoreError> {
        let connection = self.connection.lock().expect("repository mutex poisoned");
        let transaction = connection.unchecked_transaction()?;
        let mut statement = transaction.prepare(
            "SELECT id FROM notes WHERE is_draft = 1 AND deleted_time = 0
             AND trim(title) = '' AND trim(body) = ''",
        )?;
        let ids = statement
            .query_map([], |row| row.get::<_, String>(0))?
            .collect::<Result<Vec<_>, _>>()?;
        drop(statement);
        for id in &ids {
            transaction.execute("DELETE FROM notes WHERE id = ?1", [id])?;
        }
        transaction.commit()?;
        for id in &ids {
            let _ = connection.execute("DELETE FROM notes_fts WHERE id = ?1", [id]);
        }
        Ok(ids.len())
    }

    pub fn search(&self, query: &str) -> Result<Vec<Note>, CoreError> {
        let query = query.trim();
        if query.is_empty() {
            return self.list_notes();
        }
        let connection = self.connection.lock().expect("repository mutex poisoned");
        let mut statement = connection.prepare(
            "SELECT n.id, n.title, n.body, n.body_text, n.body_rtf, n.is_draft, n.created_time,
                    n.updated_time, n.deleted_time
             FROM notes_fts f JOIN notes n ON n.id = f.id
             WHERE notes_fts MATCH ?1 AND n.deleted_time = 0
             ORDER BY n.updated_time DESC, n.id ASC",
        )?;
        let rows = statement
            .query_map([sanitize_fts_query(query)], row_to_note)?
            .collect::<Result<Vec<_>, _>>()?;
        if !rows.is_empty() {
            return Ok(rows);
        }
        let mut fallback = connection.prepare(
            "SELECT id, title, body, body_text, body_rtf, is_draft, created_time, updated_time, deleted_time
             FROM notes WHERE deleted_time = 0 AND (instr(title, ?1) > 0 OR instr(body_text, ?1) > 0)
             ORDER BY updated_time DESC, id ASC",
        )?;
        let fallback_rows = fallback.query_map([query], row_to_note)?;
        Ok(fallback_rows.collect::<Result<Vec<_>, _>>()?)
    }

    fn rebuild_search_index(&self) -> Result<(), CoreError> {
        let connection = self.connection.lock().expect("repository mutex poisoned");
        connection.execute("DELETE FROM notes_fts", [])?;
        let mut statement =
            connection.prepare("SELECT id, title, body_text FROM notes WHERE deleted_time = 0")?;
        let rows = statement.query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
            ))
        })?;
        let values = rows.collect::<Result<Vec<_>, _>>()?;
        drop(statement);
        for (id, title, body_text) in values {
            connection.execute(
                "INSERT INTO notes_fts (id, title, body_text) VALUES (?1, ?2, ?3)",
                params![id, title, body_text],
            )?;
        }
        Ok(())
    }
}

fn migrate_schema(connection: &Connection) -> Result<(), CoreError> {
    connection.execute_batch(
        "
        CREATE TABLE IF NOT EXISTS notes (
            id TEXT PRIMARY KEY NOT NULL,
            title TEXT NOT NULL DEFAULT '',
            body TEXT NOT NULL DEFAULT '',
            body_text TEXT NOT NULL DEFAULT '',
            body_rtf BLOB NOT NULL DEFAULT X'',
            is_draft INTEGER NOT NULL DEFAULT 0,
            created_time INTEGER NOT NULL,
            updated_time INTEGER NOT NULL,
            deleted_time INTEGER NOT NULL DEFAULT 0
        );
        ",
    )?;
    let columns = connection
        .prepare("PRAGMA table_info(notes)")?
        .query_map([], |row| row.get::<_, String>(1))?
        .collect::<Result<Vec<_>, _>>()?;
    if !columns.iter().any(|column| column == "body_rtf") {
        connection.execute(
            "ALTER TABLE notes ADD COLUMN body_rtf BLOB NOT NULL DEFAULT X''",
            [],
        )?;
    }
    if !columns.iter().any(|column| column == "body_text") {
        connection.execute(
            "ALTER TABLE notes ADD COLUMN body_text TEXT NOT NULL DEFAULT ''",
            [],
        )?;
    }
    connection.execute("UPDATE notes SET body_text = body WHERE body_text = ''", [])?;
    connection.execute_batch(
        "
        CREATE TABLE IF NOT EXISTS resource_blobs (
            sha256 TEXT PRIMARY KEY NOT NULL,
            size INTEGER NOT NULL
        );
        CREATE TABLE IF NOT EXISTS resources (
            id TEXT PRIMARY KEY NOT NULL,
            sha256 TEXT NOT NULL REFERENCES resource_blobs(sha256),
            title TEXT NOT NULL DEFAULT '',
            mime TEXT NOT NULL,
            file_extension TEXT NOT NULL,
            created_time INTEGER NOT NULL
        );
        CREATE TABLE IF NOT EXISTS note_resources (
            note_id TEXT NOT NULL REFERENCES notes(id) ON DELETE CASCADE,
            resource_id TEXT NOT NULL REFERENCES resources(id) ON DELETE RESTRICT,
            position INTEGER NOT NULL,
            PRIMARY KEY (note_id, resource_id)
        );
        DROP TABLE IF EXISTS notes_fts;
        CREATE VIRTUAL TABLE notes_fts USING fts5(id UNINDEXED, title, body_text);
        PRAGMA user_version = 2;
        ",
    )?;
    Ok(())
}

fn ensure_database_inode(path: &Path) -> Result<(), CoreError> {
    match std::fs::symlink_metadata(path) {
        Ok(metadata) => {
            let file_type = metadata.file_type();
            if file_type.is_symlink() || !file_type.is_file() {
                return Err(CoreError::InvalidDatabasePath);
            }
            Ok(())
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            match std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(path)
            {
                Ok(_) => Ok(()),
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                    ensure_database_inode(path)
                }
                Err(_) => Err(CoreError::InvalidDatabasePath),
            }
        }
        Err(_) => Err(CoreError::InvalidDatabasePath),
    }
}

fn row_to_note(row: &rusqlite::Row<'_>) -> rusqlite::Result<Note> {
    Ok(Note {
        id: row.get(0)?,
        title: row.get(1)?,
        body: row.get(2)?,
        body_text: row.get(3)?,
        body_rtf: row.get(4)?,
        is_draft: row.get::<_, i64>(5)? != 0,
        created_time: row.get(6)?,
        updated_time: row.get(7)?,
        deleted_time: row.get(8)?,
    })
}

fn replace_index_entry(
    connection: &Connection,
    id: &str,
    title: &str,
    body_text: &str,
) -> Result<(), rusqlite::Error> {
    let transaction = connection.unchecked_transaction()?;
    transaction.execute("DELETE FROM notes_fts WHERE id = ?1", [id])?;
    transaction.execute(
        "INSERT INTO notes_fts (id, title, body_text) VALUES (?1, ?2, ?3)",
        params![id, title, body_text],
    )?;
    transaction.commit()?;
    Ok(())
}

fn validate_id(id: &str) -> Result<(), CoreError> {
    if id.len() == 32
        && id
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
    {
        Ok(())
    } else {
        Err(CoreError::InvalidId)
    }
}

pub(crate) fn new_id() -> String {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let counter = NEXT_ID.fetch_add(1, Ordering::Relaxed) as u128;
    format!("{:032x}", nanos ^ counter)
}

fn timestamp() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as i64
}

fn sanitize_fts_query(query: &str) -> String {
    query
        .split_whitespace()
        .map(|term| format!("\"{}\"", term.replace('"', "")))
        .collect::<Vec<_>>()
        .join(" AND ")
}

#[cfg(test)]
mod tests {
    use super::{CreateNote, NoteContentUpdate, NoteRepository};
    use crate::body::{markdown_marker, project_search_text};
    use crate::resource_store::ResourceImport;
    use rusqlite::Connection;
    use tempfile::tempdir;

    const TINY_PNG: &[u8] = b"tiny png bytes";

    fn png_import(bytes: &'static [u8], title: &'static str) -> ResourceImport<'static> {
        ResourceImport {
            bytes,
            title,
            mime: "image/png",
            file_extension: "png",
        }
    }

    fn count_rows(repo: &NoteRepository, table: &str) -> i64 {
        let connection = repo.connection.lock().unwrap();
        connection
            .query_row(&format!("SELECT count(*) FROM {table}"), [], |row| {
                row.get(0)
            })
            .unwrap()
    }

    #[test]
    fn importing_same_bytes_reuses_blob_and_survives_reopen() {
        let temp = tempdir().unwrap();
        let db = temp.path().join("notes.sqlite");
        let repo = NoteRepository::open(&db).unwrap();
        let first = repo
            .import_resource(png_import(TINY_PNG, "first.png"))
            .unwrap();
        let second = repo
            .import_resource(png_import(TINY_PNG, "copy.png"))
            .unwrap();
        assert_ne!(first.id, second.id);
        assert_eq!(first.sha256, second.sha256);
        assert_eq!(count_rows(&repo, "resource_blobs"), 1);
        drop(repo);
        let reopened = NoteRepository::open(&db).unwrap();
        assert_eq!(
            reopened.get_resource(&first.id).unwrap().unwrap().sha256,
            first.sha256
        );
    }

    #[test]
    fn old_notes_are_backfilled_and_content_update_replaces_only_associations() {
        let temp = tempdir().unwrap();
        let db = temp.path().join("notes.sqlite");
        let connection = Connection::open(&db).unwrap();
        connection
            .execute_batch(
                "CREATE TABLE notes (
                    id TEXT PRIMARY KEY, title TEXT NOT NULL, body TEXT NOT NULL,
                    body_rtf BLOB NOT NULL DEFAULT X'', is_draft INTEGER NOT NULL,
                    created_time INTEGER NOT NULL, updated_time INTEGER NOT NULL,
                    deleted_time INTEGER NOT NULL DEFAULT 0
                );
                INSERT INTO notes VALUES ('0123456789abcdef0123456789abcdef', 'old', 'legacy body', X'', 0, 1, 1, 0);",
            )
            .unwrap();
        drop(connection);
        let repo = NoteRepository::open(&db).unwrap();
        assert_eq!(
            repo.get_note("0123456789abcdef0123456789abcdef")
                .unwrap()
                .unwrap()
                .body_text,
            "legacy body"
        );
        let first = repo
            .import_resource(png_import(TINY_PNG, "first.png"))
            .unwrap();
        let second = repo
            .import_resource(png_import(b"other bytes", "second.png"))
            .unwrap();
        let note = repo
            .update_note_content(
                "0123456789abcdef0123456789abcdef",
                NoteContentUpdate {
                    title: "updated".into(),
                    body: "![first](:/0123456789abcdef0123456789abc0)".into(),
                    body_text: "first".into(),
                    body_rtf: Vec::new(),
                    resource_ids: vec![first.id.clone()],
                },
            )
            .unwrap();
        assert_eq!(note.body_text, "first");
        assert_eq!(count_rows(&repo, "note_resources"), 1);
        repo.update_note_content(
            &note.id,
            NoteContentUpdate {
                title: "updated again".into(),
                body: "second".into(),
                body_text: "second".into(),
                body_rtf: Vec::new(),
                resource_ids: vec![second.id.clone()],
            },
        )
        .unwrap();
        assert_eq!(count_rows(&repo, "note_resources"), 1);
        assert!(repo.get_resource(&first.id).unwrap().is_some());
        assert!(repo.get_resource(&second.id).unwrap().is_some());
    }

    #[test]
    fn search_uses_alt_text_but_not_digest_or_path_text() {
        let temp = tempdir().unwrap();
        let repo = NoteRepository::open(temp.path().join("notes.sqlite")).unwrap();
        let id = "0123456789abcdef0123456789abcdef";
        let body = format!("证据\n\n{}", markdown_marker(id, "庭审截图 [1]").unwrap());
        let note = repo
            .create_note(CreateNote {
                title: "图片笔记".into(),
                body: body.clone(),
                body_rtf: Vec::new(),
                is_draft: false,
            })
            .unwrap();
        assert_eq!(note.body_text, project_search_text(&body));
        assert_eq!(repo.search("庭审截图").unwrap().len(), 1);
        assert!(repo.search(id).unwrap().is_empty());
    }
}
