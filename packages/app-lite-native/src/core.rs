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
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Note {
    pub id: String,
    pub title: String,
    pub body: String,
    /// Native rich-text bytes (RTF/RTFD). `body` remains the searchable text.
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

pub struct NoteRepository {
    connection: Mutex<Connection>,
}

impl NoteRepository {
    pub fn open(path: impl AsRef<Path>) -> Result<Self, CoreError> {
        let input_path = path.as_ref();
        ensure_database_inode(input_path)?;
        // NOFOLLOW also checks parent components on macOS. Canonicalize only
        // the parent, never the database leaf, so a concurrent leaf swap is
        // still rejected by SQLite rather than followed here.
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
        let connection = Connection::open_with_flags(path, flags)?;
        connection.pragma_update(None, "journal_mode", "WAL")?;
        connection.pragma_update(None, "foreign_keys", "ON")?;
        connection.execute_batch(
            "
            CREATE TABLE IF NOT EXISTS notes (
                id TEXT PRIMARY KEY NOT NULL,
                title TEXT NOT NULL DEFAULT '',
                body TEXT NOT NULL DEFAULT '',
                body_rtf BLOB NOT NULL DEFAULT X'',
                is_draft INTEGER NOT NULL DEFAULT 0,
                created_time INTEGER NOT NULL,
                updated_time INTEGER NOT NULL,
                deleted_time INTEGER NOT NULL DEFAULT 0
            );
            CREATE VIRTUAL TABLE IF NOT EXISTS notes_fts USING fts5(
                id UNINDEXED,
                title,
                body
            );
            ",
        )?;
        // Keep databases created by the first core revision readable.
        let _ = connection.execute(
            "ALTER TABLE notes ADD COLUMN body_rtf BLOB NOT NULL DEFAULT X''",
            [],
        );
        let repository = Self {
            connection: Mutex::new(connection),
        };
        repository.rebuild_search_index()?;
        Ok(repository)
    }

    pub fn create_note(&self, input: CreateNote) -> Result<Note, CoreError> {
        let now = timestamp();
        let id = new_id();
        let connection = self.connection.lock().expect("repository mutex poisoned");
        let transaction = connection.unchecked_transaction()?;
        transaction.execute(
            "INSERT INTO notes (id, title, body, is_draft, created_time, updated_time)
             VALUES (?1, ?2, ?3, ?4, ?5, ?5)",
            params![id, input.title, input.body, input.is_draft, now],
        )?;
        transaction.execute(
            "UPDATE notes SET body_rtf = ?2 WHERE id = ?1",
            params![id, input.body_rtf],
        )?;
        transaction.commit()?;
        let _ = replace_index_entry(&connection, &id, &input.title, &input.body);
        Ok(Note {
            id,
            title: input.title,
            body: input.body,
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
                "SELECT id, title, body, body_rtf, is_draft, created_time, updated_time, deleted_time
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
            "SELECT id, title, body, body_rtf, is_draft, created_time, updated_time, deleted_time
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
                "SELECT id, title, body, body_rtf, is_draft, created_time, updated_time, deleted_time
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
        }
        if let Some(body_rtf) = input.body_rtf {
            note.body_rtf = body_rtf;
        }
        note.updated_time = timestamp().max(note.updated_time + 1);
        transaction.execute(
            "UPDATE notes SET title = ?2, body = ?3, body_rtf = ?4, is_draft = 0, updated_time = ?5
             WHERE id = ?1 AND deleted_time = 0",
            params![
                note.id,
                note.title,
                note.body,
                note.body_rtf,
                note.updated_time
            ],
        )?;
        note.is_draft = false;
        transaction.commit()?;
        let _ = replace_index_entry(&connection, &note.id, &note.title, &note.body);
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
            "SELECT n.id, n.title, n.body, n.body_rtf, n.is_draft, n.created_time,
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
        // SQLite's default unicode tokenizer does not split every CJK script.
        // Keep FTS as the normal path, with a small source-row fallback so a
        // native search cannot silently miss a valid contiguous CJK match.
        let mut fallback = connection.prepare(
            "SELECT id, title, body, body_rtf, is_draft, created_time, updated_time, deleted_time
             FROM notes WHERE deleted_time = 0 AND (instr(title, ?1) > 0 OR instr(body, ?1) > 0)
             ORDER BY updated_time DESC, id ASC",
        )?;
        let fallback_rows = fallback.query_map([query], row_to_note)?;
        Ok(fallback_rows.collect::<Result<Vec<_>, _>>()?)
    }

    fn rebuild_search_index(&self) -> Result<(), CoreError> {
        let connection = self.connection.lock().expect("repository mutex poisoned");
        connection.execute("DELETE FROM notes_fts", [])?;
        let mut statement =
            connection.prepare("SELECT id, title, body FROM notes WHERE deleted_time = 0")?;
        let rows = statement.query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
            ))
        })?;
        let values = rows.collect::<Result<Vec<_>, _>>()?;
        drop(statement);
        for (id, title, body) in values {
            connection.execute(
                "INSERT INTO notes_fts (id, title, body) VALUES (?1, ?2, ?3)",
                params![id, title, body],
            )?;
        }
        Ok(())
    }
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
        body_rtf: row.get(3)?,
        is_draft: row.get::<_, i64>(4)? != 0,
        created_time: row.get(5)?,
        updated_time: row.get(6)?,
        deleted_time: row.get(7)?,
    })
}

fn replace_index_entry(
    connection: &Connection,
    id: &str,
    title: &str,
    body: &str,
) -> Result<(), rusqlite::Error> {
    let transaction = connection.unchecked_transaction()?;
    transaction.execute("DELETE FROM notes_fts WHERE id = ?1", [id])?;
    transaction.execute(
        "INSERT INTO notes_fts (id, title, body) VALUES (?1, ?2, ?3)",
        params![id, title, body],
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

fn new_id() -> String {
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
