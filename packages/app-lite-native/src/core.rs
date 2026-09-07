use crate::body::BodyError;
use crate::html_body::{HtmlBodyError, parse_html, resource_ids, search_text, serialize_html};
pub use crate::resource_store::ResourceImport;
use crate::resource_store::{ResourceError, ResourceStore};
use rusqlite::{Connection, OpenFlags, OptionalExtension, params};
#[cfg(test)]
use std::collections::HashMap;
use std::collections::HashSet;
use std::ffi::CString;
use std::fs::{File, OpenOptions};
use std::path::Path;
use std::path::PathBuf;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};
use thiserror::Error;

#[cfg(unix)]
use std::os::unix::fs::{MetadataExt, OpenOptionsExt};

static NEXT_ID: AtomicU64 = AtomicU64::new(1);

#[cfg(test)]
#[derive(Default)]
struct BackupTestHooks {
    fail_after_destination_open: HashSet<PathBuf>,
    publish_race_targets: HashSet<PathBuf>,
    replace_partial_before_cleanup: HashSet<PathBuf>,
    aba_swap_targets: HashSet<PathBuf>,
    partial_paths: HashMap<PathBuf, PathBuf>,
}

#[cfg(test)]
static BACKUP_TEST_HOOKS: std::sync::OnceLock<Mutex<BackupTestHooks>> = std::sync::OnceLock::new();

#[cfg(test)]
#[derive(Default)]
struct MigrationTestHooks {
    pause_after_backup: HashSet<PathBuf>,
    backup_paused: HashSet<PathBuf>,
    release_backup: HashSet<PathBuf>,
}

#[cfg(test)]
static MIGRATION_TEST_HOOKS: std::sync::OnceLock<Mutex<MigrationTestHooks>> =
    std::sync::OnceLock::new();

#[cfg(test)]
fn backup_test_hooks() -> &'static Mutex<BackupTestHooks> {
    BACKUP_TEST_HOOKS.get_or_init(|| Mutex::new(BackupTestHooks::default()))
}

#[cfg(test)]
fn migration_test_hooks() -> &'static Mutex<MigrationTestHooks> {
    MIGRATION_TEST_HOOKS.get_or_init(|| Mutex::new(MigrationTestHooks::default()))
}

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
    #[error("HTML body error")]
    Html(#[from] HtmlBodyError),
    #[error("resource error")]
    Resource(#[from] ResourceError),
    #[error("invalid HTML migration: {0}")]
    MigrationValidation(String),
    #[error("note requires HTML migration before it can be edited")]
    MigrationRequired,
    #[error("invalid backup target")]
    InvalidBackupTarget,
    #[error("HTML migration backup failed")]
    BackupFailure,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Note {
    pub id: String,
    pub title: String,
    pub body: String,
    pub body_text: String,
    pub markup_language: i64,
    pub is_draft: bool,
    pub created_time: i64,
    pub updated_time: i64,
    pub deleted_time: i64,
}

/// Lightweight row used by the note browser. It deliberately excludes the
/// canonical HTML body so large notes never become part of the browser's
/// resident list projection.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NoteListItem {
    pub id: String,
    pub title: String,
    pub body_text: String,
    pub updated_time: i64,
    pub first_image_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CreateNote {
    pub title: String,
    pub body: String,
    pub is_draft: bool,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct UpdateNote {
    pub title: Option<String>,
    pub body: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NoteContentUpdate {
    pub title: String,
    pub body: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LegacyNoteForHtmlMigration {
    pub id: String,
    pub title: String,
    pub body: String,
    pub body_rtf: Vec<u8>,
    pub is_draft: bool,
    pub created_time: i64,
    pub updated_time: i64,
    pub deleted_time: i64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HtmlNoteConversion {
    pub id: String,
    pub source_updated_time: i64,
    pub body: String,
    pub body_text: String,
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
    database_path: PathBuf,
}

impl NoteRepository {
    pub fn open(path: impl AsRef<Path>) -> Result<Self, CoreError> {
        let input_path = path.as_ref();
        let input_parent = input_path.parent().ok_or(CoreError::InvalidDatabasePath)?;
        if std::fs::symlink_metadata(input_parent)
            .map_err(|_| CoreError::InvalidDatabasePath)?
            .file_type()
            .is_symlink()
        {
            return Err(CoreError::InvalidDatabasePath);
        }
        ensure_database_inode(input_path)?;
        let parent = input_parent;
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
        let resource_store = ResourceStore::new(
            path.parent()
                .ok_or(CoreError::InvalidDatabasePath)?
                .to_path_buf(),
        )?;
        migrate_schema(&connection)?;
        let repository = Self {
            connection: Mutex::new(connection),
            resource_store,
            database_path: path,
        };
        repository.rebuild_search_index()?;
        Ok(repository)
    }

    pub fn create_note(&self, input: CreateNote) -> Result<Note, CoreError> {
        let now = timestamp();
        let id = new_id();
        let canonical = canonicalize_html(&input.body)?;
        let connection = self.connection.lock().expect("repository mutex poisoned");
        let transaction = connection.unchecked_transaction()?;
        transaction.execute(
            "INSERT INTO notes (id, title, body, body_text, body_rtf, markup_language, is_draft, created_time, updated_time)
             VALUES (?1, ?2, ?3, ?4, X'', 2, ?5, ?6, ?6)",
            params![id, input.title, canonical.body, canonical.body_text, input.is_draft, now],
        )?;
        replace_note_resources(&transaction, &id, &canonical.resource_ids)?;
        transaction.commit()?;
        let _ = replace_index_entry(&connection, &id, &input.title, &canonical.body_text);
        Ok(Note {
            id,
            title: input.title,
            body: canonical.body,
            body_text: canonical.body_text,
            markup_language: 2,
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
                "SELECT id, title, body, body_text, markup_language, is_draft, created_time, updated_time, deleted_time
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
            "SELECT id, title, body, body_text, markup_language, is_draft, created_time, updated_time, deleted_time
             FROM notes WHERE deleted_time = 0 ORDER BY updated_time DESC, id ASC",
        )?;
        let rows = statement.query_map([], row_to_note)?;
        Ok(rows.collect::<Result<Vec<_>, _>>()?)
    }

    pub fn list_note_previews(&self) -> Result<Vec<NoteListItem>, CoreError> {
        let connection = self.connection.lock().expect("repository mutex poisoned");
        let mut statement = connection.prepare(
            "SELECT n.id, substr(n.title, 1, 120), substr(n.body_text, 1, 96), n.updated_time,
                    (SELECT nr.resource_id
                     FROM note_resources nr JOIN resources r ON r.id = nr.resource_id
                     WHERE nr.note_id = n.id AND nr.is_associated = 1
                       AND r.deleted_time = 0
                       AND r.mime IN ('image/png', 'image/jpeg')
                     ORDER BY nr.position ASC, nr.resource_id ASC LIMIT 1)
             FROM notes n WHERE n.deleted_time = 0
             ORDER BY n.updated_time DESC, n.id ASC",
        )?;
        let rows = statement.query_map([], row_to_note_list_item)?;
        Ok(rows.collect::<Result<Vec<_>, _>>()?)
    }

    pub fn update_note(&self, id: &str, input: UpdateNote) -> Result<Note, CoreError> {
        validate_id(id)?;
        let connection = self.connection.lock().expect("repository mutex poisoned");
        let transaction = connection.unchecked_transaction()?;
        let current = transaction
            .query_row(
                "SELECT id, title, body, body_text, markup_language, is_draft, created_time, updated_time, deleted_time
                 FROM notes WHERE id = ?1 AND deleted_time = 0",
                [id],
                row_to_note,
            )
            .optional()?;
        let Some(mut note) = current else {
            return Err(CoreError::InvalidId);
        };
        if note.markup_language == 1 {
            return Err(CoreError::MigrationRequired);
        }
        let mut resource_ids = note_resource_ids(&transaction, id)?;
        if let Some(title) = input.title {
            note.title = title;
        }
        if let Some(body) = input.body {
            let canonical = canonicalize_html(&body)?;
            note.body = canonical.body;
            note.body_text = canonical.body_text;
            resource_ids = canonical.resource_ids;
        }
        note.markup_language = 2;
        note.updated_time = timestamp().max(note.updated_time + 1);
        transaction.execute(
            "UPDATE notes SET title = ?2, body = ?3, body_text = ?4, body_rtf = X'',
             markup_language = 2, is_draft = 0, updated_time = ?5 WHERE id = ?1 AND deleted_time = 0",
            params![
                note.id,
                note.title,
                note.body,
                note.body_text,
                note.updated_time
            ],
        )?;
        replace_note_resources(&transaction, &note.id, &resource_ids)?;
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
            "INSERT OR IGNORE INTO resource_blobs
             (sha256, size, mime, relative_path, created_time) VALUES (?1, ?2, ?3, ?4, ?5)",
            params![
                blob.sha256,
                blob.size as i64,
                input.mime,
                format!("resources/blobs/{}", blob.sha256),
                now
            ],
        )?;
        transaction.execute(
            "INSERT INTO resources
             (id, sha256, title, mime, file_extension, created_time, size, updated_time, deleted_time)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, 0)",
            params![
                blob.id,
                blob.sha256,
                input.title,
                input.mime,
                input.file_extension,
                now,
                blob.size as i64,
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

    /// Rolls back resource metadata created by an import that was never
    /// attached to a note. The content-addressed blob remains on disk so it
    /// can be reused safely by a later import or collected by GC.
    pub fn rollback_unassociated_resource(&self, resource_id: &str) -> Result<(), CoreError> {
        validate_id(resource_id)?;
        let connection = self.connection.lock().expect("repository mutex poisoned");
        let transaction = connection.unchecked_transaction()?;
        let sha256 = transaction
            .query_row(
                "SELECT sha256 FROM resources
                 WHERE id = ?1
                   AND NOT EXISTS (
                       SELECT 1 FROM note_resources WHERE resource_id = ?1
                   )",
                [resource_id],
                |row| row.get::<_, String>(0),
            )
            .optional()?;
        let Some(sha256) = sha256 else {
            transaction.commit()?;
            return Ok(());
        };
        transaction.execute(
            "DELETE FROM resources
             WHERE id = ?1
               AND NOT EXISTS (
                   SELECT 1 FROM note_resources WHERE resource_id = ?1
               )",
            [resource_id],
        )?;
        transaction.execute(
            "DELETE FROM resource_blobs
             WHERE sha256 = ?1
               AND NOT EXISTS (
                   SELECT 1 FROM resources WHERE sha256 = ?1
               )",
            [sha256],
        )?;
        transaction.commit()?;
        Ok(())
    }

    pub fn get_resource(&self, id: &str) -> Result<Option<StoredResource>, CoreError> {
        validate_id(id)?;
        let metadata = {
            let connection = self.connection.lock().expect("repository mutex poisoned");
            connection
                .query_row(
                    "SELECT r.id, r.sha256, r.size, r.title, r.mime, r.file_extension
                     FROM resources r
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
        let connection = self.connection.lock().expect("repository mutex poisoned");
        let transaction = connection.unchecked_transaction()?;
        let mut note = transaction
            .query_row(
                "SELECT id, title, body, body_text, markup_language, is_draft, created_time, updated_time, deleted_time
                 FROM notes WHERE id = ?1 AND deleted_time = 0",
                [id],
                row_to_note,
            )
            .optional()?
            .ok_or(CoreError::InvalidId)?;
        if note.markup_language == 1 {
            return Err(CoreError::MigrationRequired);
        }
        let canonical = canonicalize_html(&input.body)?;
        note.title = input.title;
        note.body = canonical.body;
        note.body_text = canonical.body_text;
        note.markup_language = 2;
        note.is_draft = false;
        note.updated_time = timestamp().max(note.updated_time + 1);
        transaction.execute(
            "UPDATE notes SET title = ?2, body = ?3, body_text = ?4, body_rtf = X'',
             markup_language = 2, is_draft = 0, updated_time = ?5 WHERE id = ?1 AND deleted_time = 0",
            params![
                note.id,
                note.title,
                note.body,
                note.body_text,
                note.updated_time
            ],
        )?;
        replace_note_resources(&transaction, id, &canonical.resource_ids)?;
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
        let mut statement = connection.prepare(&search_query_sql(
            "n.id, n.title, n.body, n.body_text, n.markup_language, n.is_draft,
             n.created_time, n.updated_time, n.deleted_time",
        ))?;
        let rows = statement.query_map(
            params![
                sanitize_fts_query(query),
                sanitize_fts_query_for_column(query, "title"),
                query
            ],
            row_to_note,
        )?;
        Ok(rows.collect::<Result<Vec<_>, _>>()?)
    }

    pub fn search_note_previews(&self, query: &str) -> Result<Vec<NoteListItem>, CoreError> {
        let query = query.trim();
        if query.is_empty() {
            return self.list_note_previews();
        }
        let connection = self.connection.lock().expect("repository mutex poisoned");
        let mut statement = connection.prepare(&search_query_sql(
            // Search cards need the projected body text to derive a context
            // window around a deep match; canonical HTML is still excluded.
            "n.id, substr(n.title, 1, 120), n.body_text, n.updated_time,
             (SELECT nr.resource_id
              FROM note_resources nr JOIN resources r ON r.id = nr.resource_id
              WHERE nr.note_id = n.id AND nr.is_associated = 1
                AND r.deleted_time = 0
                AND r.mime IN ('image/png', 'image/jpeg')
              ORDER BY nr.position ASC, nr.resource_id ASC LIMIT 1)",
        ))?;
        let rows = statement.query_map(
            params![
                sanitize_fts_query(query),
                sanitize_fts_query_for_column(query, "title"),
                query
            ],
            row_to_note_list_item,
        )?;
        Ok(rows.collect::<Result<Vec<_>, _>>()?)
    }

    fn rebuild_search_index(&self) -> Result<(), CoreError> {
        let connection = self.connection.lock().expect("repository mutex poisoned");
        let transaction = connection.unchecked_transaction()?;
        transaction.execute("DELETE FROM notes_fts", [])?;
        let mut statement =
            transaction.prepare("SELECT id, title, body_text FROM notes WHERE deleted_time = 0")?;
        let rows = statement
            .query_map([], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                ))
            })?
            .collect::<Result<Vec<_>, _>>()?;
        drop(statement);
        for (id, title, body_text) in rows {
            transaction.execute(
                "INSERT INTO notes_fts (id, title, body_text) VALUES (?1, ?2, ?3)",
                params![id, title, body_text],
            )?;
        }
        transaction.commit()?;
        Ok(())
    }

    pub fn list_legacy_notes_for_html_migration(
        &self,
    ) -> Result<Vec<LegacyNoteForHtmlMigration>, CoreError> {
        let connection = self.connection.lock().expect("repository mutex poisoned");
        let mut statement = connection.prepare(
            "SELECT id, title, body, body_rtf, is_draft, created_time, updated_time, deleted_time
             FROM notes WHERE markup_language = 1 ORDER BY id ASC",
        )?;
        let rows = statement.query_map([], |row| {
            Ok(LegacyNoteForHtmlMigration {
                id: row.get(0)?,
                title: row.get(1)?,
                body: row.get(2)?,
                body_rtf: row.get(3)?,
                is_draft: row.get::<_, i64>(4)? != 0,
                created_time: row.get(5)?,
                updated_time: row.get(6)?,
                deleted_time: row.get(7)?,
            })
        })?;
        Ok(rows.collect::<Result<Vec<_>, _>>()?)
    }

    pub fn backup_before_html_migration(&self) -> Result<PathBuf, CoreError> {
        let connection = self.connection.lock().expect("repository mutex poisoned");
        self.backup_before_html_migration_locked(&connection)
    }

    fn backup_before_html_migration_locked(
        &self,
        connection: &Connection,
    ) -> Result<PathBuf, CoreError> {
        let profile = self
            .database_path
            .parent()
            .ok_or(CoreError::InvalidBackupTarget)?;
        let version =
            connection.query_row("PRAGMA user_version", [], |row| row.get::<_, i64>(0))?;
        for _ in 0..32 {
            let counter = NEXT_ID.fetch_add(1, Ordering::Relaxed);
            let target = profile.join(format!(
                "{}.html-migration-v{}-{}-{}.sqlite",
                self.database_path
                    .file_name()
                    .and_then(|name| name.to_str())
                    .ok_or(CoreError::InvalidBackupTarget)?,
                version,
                timestamp(),
                counter
            ));
            match self.backup_before_html_migration_to_locked(connection, &target) {
                Ok(()) => return Ok(target),
                Err(CoreError::InvalidBackupTarget) => {
                    continue;
                }
                Err(error) => return Err(error),
            }
        }
        Err(CoreError::InvalidBackupTarget)
    }

    pub fn backup_before_html_migration_to(
        &self,
        target: impl AsRef<Path>,
    ) -> Result<PathBuf, CoreError> {
        let target = target.as_ref().to_path_buf();
        let connection = self.connection.lock().expect("repository mutex poisoned");
        self.backup_before_html_migration_to_locked(&connection, &target)?;
        Ok(target)
    }

    fn backup_before_html_migration_to_locked(
        &self,
        source: &Connection,
        target: &Path,
    ) -> Result<(), CoreError> {
        let profile = self
            .database_path
            .parent()
            .ok_or(CoreError::InvalidBackupTarget)?;
        let target_parent = target.parent().ok_or(CoreError::InvalidBackupTarget)?;
        let canonical_parent =
            std::fs::canonicalize(target_parent).map_err(|_| CoreError::InvalidBackupTarget)?;
        if canonical_parent != profile
            || std::fs::symlink_metadata(target_parent)
                .map_err(|_| CoreError::InvalidBackupTarget)?
                .file_type()
                .is_symlink()
            || std::fs::symlink_metadata(target).is_ok()
        {
            return Err(CoreError::InvalidBackupTarget);
        }
        let target_name = target
            .file_name()
            .and_then(|name| name.to_str())
            .ok_or(CoreError::InvalidBackupTarget)?;
        let partial_path = profile.join(format!(
            ".{target_name}.html-migration-{}.partial",
            new_id()
        ));
        let partial = OwnedPartial::claim(partial_path, target)?;
        let result = (|| {
            if !partial.still_owned() {
                return Err(CoreError::BackupFailure);
            }
            #[cfg(test)]
            let aba_stash = prepare_aba_swap(&partial.path, target)?;
            let destination_result = Connection::open_with_flags(
                &partial.path,
                OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_NOFOLLOW,
            );
            #[cfg(test)]
            if let Some(stash) = aba_stash {
                restore_aba_swap(&partial.path, &stash)?;
            }
            let mut destination = destination_result?;
            ensure_destination_has_not_moved(&destination)?;
            if !partial.still_owned() {
                return Err(CoreError::BackupFailure);
            }
            #[cfg(test)]
            if consume_backup_hook(
                |hooks| hooks.fail_after_destination_open.remove(target),
                target,
            ) {
                return Err(CoreError::BackupFailure);
            }
            let backup = rusqlite::backup::Backup::new(source, &mut destination)?;
            backup.run_to_completion(64, std::time::Duration::from_millis(0), None)?;
            drop(backup);
            let integrity = destination
                .query_row("PRAGMA integrity_check", [], |row| row.get::<_, String>(0))?;
            if integrity != "ok" {
                return Err(CoreError::BackupFailure);
            }
            ensure_destination_has_not_moved(&destination)?;
            if !partial.still_owned() {
                return Err(CoreError::BackupFailure);
            }
            partial
                .file
                .sync_all()
                .map_err(|_| CoreError::BackupFailure)?;
            if !partial.still_owned() {
                return Err(CoreError::BackupFailure);
            }
            #[cfg(test)]
            if consume_backup_hook(|hooks| hooks.publish_race_targets.remove(target), target) {
                std::fs::write(target, b"foreign final").map_err(|_| CoreError::BackupFailure)?;
            }
            ensure_destination_has_not_moved(&destination)?;
            std::fs::hard_link(&partial.path, target).map_err(|_| CoreError::BackupFailure)?;
            Ok(())
        })();
        partial.cleanup();
        result
    }

    pub fn apply_html_migration(
        &self,
        conversions: Vec<HtmlNoteConversion>,
    ) -> Result<PathBuf, CoreError> {
        let connection = self.connection.lock().expect("repository mutex poisoned");
        let backup = self.backup_before_html_migration_locked(&connection)?;
        #[cfg(test)]
        if consume_migration_hook(
            |hooks| hooks.pause_after_backup.remove(&self.database_path),
            &self.database_path,
        ) {
            migration_test_hooks()
                .lock()
                .expect("migration hooks mutex poisoned")
                .backup_paused
                .insert(self.database_path.clone());
            while !migration_test_hooks()
                .lock()
                .expect("migration hooks mutex poisoned")
                .release_backup
                .contains(&self.database_path)
            {
                std::thread::yield_now();
            }
            let mut hooks = migration_test_hooks()
                .lock()
                .expect("migration hooks mutex poisoned");
            hooks.backup_paused.remove(&self.database_path);
            hooks.release_backup.remove(&self.database_path);
        }
        let transaction = connection.unchecked_transaction()?;
        let expected = {
            let mut statement = transaction
                .prepare("SELECT id FROM notes WHERE markup_language = 1 ORDER BY id ASC")?;
            statement
                .query_map([], |row| row.get::<_, String>(0))?
                .collect::<Result<Vec<_>, _>>()?
        };
        if expected.len() != conversions.len() {
            return Err(CoreError::MigrationValidation(
                "conversion batch does not cover exactly all legacy notes".into(),
            ));
        }
        let expected_ids = expected.iter().cloned().collect::<HashSet<_>>();
        let mut seen_ids = HashSet::new();
        for conversion in &conversions {
            if !seen_ids.insert(conversion.id.clone()) || !expected_ids.contains(&conversion.id) {
                return Err(CoreError::MigrationValidation(
                    "conversion batch contains duplicate or unexpected note ids".into(),
                ));
            }
            validate_conversion(&transaction, conversion)?;
        }
        for conversion in conversions {
            let deleted_time = transaction.query_row(
                "SELECT deleted_time FROM notes WHERE id = ?1 AND markup_language = 1",
                [&conversion.id],
                |row| row.get::<_, i64>(0),
            )?;
            let changed = transaction.execute(
                "UPDATE notes SET body = ?2, body_text = ?3, body_rtf = X'', markup_language = 2
                 WHERE id = ?1 AND markup_language = 1 AND updated_time = ?4",
                params![
                    conversion.id,
                    conversion.body,
                    conversion.body_text,
                    conversion.source_updated_time
                ],
            )?;
            if changed != 1 {
                return Err(CoreError::MigrationValidation(
                    "legacy note changed while migration was being applied".into(),
                ));
            }
            replace_note_resources(&transaction, &conversion.id, &conversion.resource_ids)?;
            transaction.execute("DELETE FROM notes_fts WHERE id = ?1", [&conversion.id])?;
            if deleted_time == 0 {
                transaction.execute(
                    "INSERT INTO notes_fts (id, title, body_text)
                     SELECT id, title, body_text FROM notes WHERE id = ?1",
                    [&conversion.id],
                )?;
            }
        }
        transaction.commit()?;
        Ok(backup)
    }
}

#[derive(Debug, Clone)]
struct CanonicalBody {
    body: String,
    body_text: String,
    resource_ids: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct FileIdentity {
    device: u64,
    inode: u64,
}

fn file_identity(metadata: &std::fs::Metadata) -> FileIdentity {
    #[cfg(unix)]
    {
        FileIdentity {
            device: metadata.dev(),
            inode: metadata.ino(),
        }
    }
    #[cfg(not(unix))]
    {
        FileIdentity {
            device: metadata.len(),
            inode: metadata.len(),
        }
    }
}

fn ensure_destination_has_not_moved(destination: &Connection) -> Result<(), CoreError> {
    let database_name = CString::new("main").expect("static database name has no nul bytes");
    let mut has_moved: std::os::raw::c_int = 0;
    let result = unsafe {
        rusqlite::ffi::sqlite3_file_control(
            destination.handle(),
            database_name.as_ptr(),
            rusqlite::ffi::SQLITE_FCNTL_HAS_MOVED,
            (&mut has_moved as *mut std::os::raw::c_int).cast(),
        )
    };
    if result != rusqlite::ffi::SQLITE_OK || has_moved != 0 {
        return Err(CoreError::BackupFailure);
    }
    Ok(())
}

#[cfg(test)]
fn consume_backup_hook<F>(consume: F, _target: &Path) -> bool
where
    F: FnOnce(&mut BackupTestHooks) -> bool,
{
    consume(
        &mut backup_test_hooks()
            .lock()
            .expect("backup hooks mutex poisoned"),
    )
}

#[cfg(test)]
fn consume_migration_hook<F>(consume: F, _database: &Path) -> bool
where
    F: FnOnce(&mut MigrationTestHooks) -> bool,
{
    consume(
        &mut migration_test_hooks()
            .lock()
            .expect("migration hooks mutex poisoned"),
    )
}

#[cfg(test)]
fn prepare_aba_swap(path: &Path, target: &Path) -> Result<Option<PathBuf>, CoreError> {
    let enabled = consume_backup_hook(|hooks| hooks.aba_swap_targets.remove(target), target);
    if !enabled {
        return Ok(None);
    }
    let stash = path.with_file_name(format!(
        ".{}.aba-{}",
        path.file_name()
            .and_then(|name| name.to_str())
            .ok_or(CoreError::BackupFailure)?,
        new_id()
    ));
    std::fs::rename(path, &stash).map_err(|_| CoreError::BackupFailure)?;
    if std::fs::write(path, b"foreign partial").is_err() {
        let _ = std::fs::rename(&stash, path);
        return Err(CoreError::BackupFailure);
    }
    Ok(Some(stash))
}

#[cfg(test)]
fn restore_aba_swap(path: &Path, stash: &Path) -> Result<(), CoreError> {
    std::fs::remove_file(path).map_err(|_| CoreError::BackupFailure)?;
    std::fs::rename(stash, path).map_err(|_| CoreError::BackupFailure)
}

struct OwnedPartial {
    path: PathBuf,
    file: File,
    identity: FileIdentity,
    #[cfg(test)]
    target: PathBuf,
}

impl OwnedPartial {
    fn claim(path: PathBuf, _target: &Path) -> Result<Self, CoreError> {
        let mut options = OpenOptions::new();
        options.read(true).write(true).create_new(true);
        #[cfg(unix)]
        options.custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC);
        let file = options
            .open(&path)
            .map_err(|_| CoreError::InvalidBackupTarget)?;
        let identity = file_identity(&file.metadata().map_err(|_| CoreError::BackupFailure)?);
        #[cfg(test)]
        backup_test_hooks()
            .lock()
            .expect("backup hooks mutex poisoned")
            .partial_paths
            .insert(_target.to_path_buf(), path.clone());
        Ok(Self {
            path,
            file,
            identity,
            #[cfg(test)]
            target: _target.to_path_buf(),
        })
    }

    fn still_owned(&self) -> bool {
        let Ok(handle_identity) = self
            .file
            .metadata()
            .map(|metadata| file_identity(&metadata))
        else {
            return false;
        };
        let Ok(path_metadata) = std::fs::symlink_metadata(&self.path) else {
            return false;
        };
        handle_identity == self.identity && file_identity(&path_metadata) == self.identity
    }

    fn cleanup(&self) {
        #[cfg(test)]
        if consume_backup_hook(
            |hooks| hooks.replace_partial_before_cleanup.remove(&self.target),
            &self.target,
        ) {
            let _ = std::fs::remove_file(&self.path);
            let _ = std::fs::write(&self.path, b"foreign partial");
        }
        if self.still_owned() {
            let _ = std::fs::remove_file(&self.path);
        }
    }
}

#[cfg(test)]
fn partial_path_for_target(target: &Path) -> Option<PathBuf> {
    backup_test_hooks()
        .lock()
        .expect("backup hooks mutex poisoned")
        .partial_paths
        .get(target)
        .cloned()
}

fn canonicalize_html(body: &str) -> Result<CanonicalBody, CoreError> {
    let document = parse_html(body)?;
    Ok(CanonicalBody {
        body: serialize_html(&document),
        body_text: search_text(&document),
        resource_ids: resource_ids(&document),
    })
}

fn note_resource_ids(
    transaction: &rusqlite::Transaction<'_>,
    note_id: &str,
) -> Result<Vec<String>, CoreError> {
    let mut statement = transaction.prepare(
        "SELECT resource_id FROM note_resources WHERE note_id = ?1 ORDER BY position ASC",
    )?;
    Ok(statement
        .query_map([note_id], |row| row.get::<_, String>(0))?
        .collect::<Result<Vec<_>, _>>()?)
}

fn replace_note_resources(
    transaction: &rusqlite::Transaction<'_>,
    note_id: &str,
    resource_ids: &[String],
) -> Result<(), CoreError> {
    let mut seen = HashSet::new();
    let mut associated = Vec::with_capacity(resource_ids.len());
    for resource_id in resource_ids {
        validate_id(resource_id)?;
        if !seen.insert(resource_id) {
            continue;
        }
        let exists = transaction.query_row(
            "SELECT EXISTS(SELECT 1 FROM resources WHERE id = ?1)",
            [resource_id],
            |row| row.get::<_, i64>(0),
        )?;
        if exists == 0 {
            continue;
        }
        associated.push(resource_id.clone());
    }
    transaction.execute("DELETE FROM note_resources WHERE note_id = ?1", [note_id])?;
    for (position, resource_id) in associated.iter().enumerate() {
        transaction.execute(
            "INSERT INTO note_resources
             (note_id, resource_id, position, is_associated, last_seen_time)
             VALUES (?1, ?2, ?3, 1, ?4)",
            params![note_id, resource_id, position as i64, timestamp()],
        )?;
    }
    Ok(())
}

fn validate_conversion(
    transaction: &rusqlite::Transaction<'_>,
    conversion: &HtmlNoteConversion,
) -> Result<(), CoreError> {
    let document = parse_html(&conversion.body)?;
    if serialize_html(&document) != conversion.body {
        return Err(CoreError::MigrationValidation(format!(
            "body is not canonical HTML: {}",
            conversion.id
        )));
    }
    let (markup_language, updated_time) = transaction.query_row(
        "SELECT markup_language, updated_time FROM notes WHERE id = ?1",
        [&conversion.id],
        |row| Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)?)),
    )?;
    if markup_language != 1 || updated_time != conversion.source_updated_time {
        return Err(CoreError::MigrationValidation(format!(
            "legacy note source changed: {}",
            conversion.id
        )));
    }
    if search_text(&document) != conversion.body_text {
        return Err(CoreError::MigrationValidation(format!(
            "body_text does not match body: {}",
            conversion.id
        )));
    }
    let derived_resource_ids = resource_ids(&document);
    if derived_resource_ids != conversion.resource_ids {
        return Err(CoreError::MigrationValidation(format!(
            "resource ids do not match body: {}",
            conversion.id
        )));
    }
    // This checks both IDs and metadata, while leaving the transaction
    // untouched until every conversion in the batch has passed validation.
    let _ = note_resource_ids(transaction, &conversion.id)?;
    for resource_id in &conversion.resource_ids {
        validate_id(resource_id)?;
        let exists = transaction.query_row(
            "SELECT EXISTS(SELECT 1 FROM resources WHERE id = ?1)",
            [resource_id],
            |row| row.get::<_, i64>(0),
        )?;
        if exists == 0 {
            return Err(CoreError::MigrationValidation(format!(
                "resource metadata is missing: {resource_id}"
            )));
        }
    }
    Ok(())
}

fn migrate_schema(connection: &Connection) -> Result<(), CoreError> {
    let version = connection.query_row("PRAGMA user_version", [], |row| row.get::<_, i64>(0))?;
    let legacy = version < 2;
    let notes_existed = table_exists(connection, "notes")?;
    let transaction = connection.unchecked_transaction()?;
    transaction.execute_batch(
        "
        CREATE TABLE IF NOT EXISTS notes (
            id TEXT PRIMARY KEY NOT NULL,
            title TEXT NOT NULL DEFAULT '',
            body TEXT NOT NULL DEFAULT '',
            body_text TEXT NOT NULL DEFAULT '',
            body_rtf BLOB NOT NULL DEFAULT X'',
            markup_language INTEGER NOT NULL DEFAULT 2,
            is_draft INTEGER NOT NULL DEFAULT 0,
            created_time INTEGER NOT NULL,
            updated_time INTEGER NOT NULL,
            deleted_time INTEGER NOT NULL DEFAULT 0
        );
        CREATE TABLE IF NOT EXISTS resource_blobs (
            sha256 TEXT PRIMARY KEY NOT NULL,
            size INTEGER NOT NULL DEFAULT 0,
            mime TEXT NOT NULL DEFAULT '',
            relative_path TEXT NOT NULL DEFAULT '',
            created_time INTEGER NOT NULL DEFAULT 0
        );
        CREATE TABLE IF NOT EXISTS resources (
            id TEXT PRIMARY KEY NOT NULL,
            sha256 TEXT NOT NULL REFERENCES resource_blobs(sha256),
            title TEXT NOT NULL DEFAULT '',
            mime TEXT NOT NULL DEFAULT '',
            file_extension TEXT NOT NULL DEFAULT '',
            created_time INTEGER NOT NULL DEFAULT 0,
            size INTEGER NOT NULL DEFAULT 0,
            updated_time INTEGER NOT NULL DEFAULT 0,
            deleted_time INTEGER NOT NULL DEFAULT 0
        );
        CREATE TABLE IF NOT EXISTS note_resources (
            note_id TEXT NOT NULL REFERENCES notes(id) ON DELETE CASCADE,
            resource_id TEXT NOT NULL REFERENCES resources(id) ON DELETE RESTRICT,
            position INTEGER NOT NULL DEFAULT 0,
            is_associated INTEGER NOT NULL DEFAULT 1,
            last_seen_time INTEGER NOT NULL DEFAULT 0,
            PRIMARY KEY (note_id, resource_id)
        );
        ",
    )?;
    ensure_column(
        &transaction,
        "notes",
        "body_rtf",
        "BLOB NOT NULL DEFAULT X''",
    )?;
    ensure_column(
        &transaction,
        "notes",
        "body_text",
        "TEXT NOT NULL DEFAULT ''",
    )?;
    if version < 3 && notes_existed {
        ensure_column(
            &transaction,
            "notes",
            "markup_language",
            "INTEGER NOT NULL DEFAULT 1",
        )?;
    } else {
        ensure_column(
            &transaction,
            "notes",
            "markup_language",
            "INTEGER NOT NULL DEFAULT 2",
        )?;
    }
    ensure_column(
        &transaction,
        "resource_blobs",
        "size",
        "INTEGER NOT NULL DEFAULT 0",
    )?;
    ensure_column(
        &transaction,
        "resource_blobs",
        "mime",
        "TEXT NOT NULL DEFAULT ''",
    )?;
    ensure_column(
        &transaction,
        "resource_blobs",
        "relative_path",
        "TEXT NOT NULL DEFAULT ''",
    )?;
    ensure_column(
        &transaction,
        "resource_blobs",
        "created_time",
        "INTEGER NOT NULL DEFAULT 0",
    )?;
    ensure_column(
        &transaction,
        "resources",
        "size",
        "INTEGER NOT NULL DEFAULT 0",
    )?;
    ensure_column(
        &transaction,
        "resources",
        "created_time",
        "INTEGER NOT NULL DEFAULT 0",
    )?;
    ensure_column(
        &transaction,
        "resources",
        "updated_time",
        "INTEGER NOT NULL DEFAULT 0",
    )?;
    ensure_column(
        &transaction,
        "resources",
        "deleted_time",
        "INTEGER NOT NULL DEFAULT 0",
    )?;
    ensure_column(
        &transaction,
        "note_resources",
        "is_associated",
        "INTEGER NOT NULL DEFAULT 1",
    )?;
    ensure_column(
        &transaction,
        "note_resources",
        "last_seen_time",
        "INTEGER NOT NULL DEFAULT 0",
    )?;
    if legacy {
        transaction.execute("UPDATE notes SET body_text = body WHERE body_text = ''", [])?;
    }
    let fts_columns = table_columns(&transaction, "notes_fts")?;
    let rebuild_fts = legacy
        || !fts_columns.iter().any(|column| column == "body_text")
        || fts_columns.iter().any(|column| column == "body");
    if rebuild_fts {
        transaction.execute_batch(
            "DROP TABLE IF EXISTS notes_fts;
             CREATE VIRTUAL TABLE notes_fts USING fts5(id UNINDEXED, title, body_text);",
        )?;
        let mut statement =
            transaction.prepare("SELECT id, title, body_text FROM notes WHERE deleted_time = 0")?;
        let values = statement
            .query_map([], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                ))
            })?
            .collect::<Result<Vec<_>, _>>()?;
        drop(statement);
        for (id, title, body_text) in values {
            transaction.execute(
                "INSERT INTO notes_fts (id, title, body_text) VALUES (?1, ?2, ?3)",
                params![id, title, body_text],
            )?;
        }
    }
    if version < 3 {
        transaction.execute_batch("PRAGMA user_version = 3;")?;
    }
    transaction.commit()?;
    Ok(())
}

fn table_columns(
    connection: &rusqlite::Transaction<'_>,
    table: &str,
) -> Result<Vec<String>, CoreError> {
    let mut statement = connection.prepare(&format!("PRAGMA table_info({table})"))?;
    Ok(statement
        .query_map([], |row| row.get::<_, String>(1))?
        .collect::<Result<Vec<_>, _>>()?)
}

fn table_exists(connection: &Connection, table: &str) -> Result<bool, CoreError> {
    Ok(connection.query_row(
        "SELECT EXISTS(
             SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = ?1
         )",
        [table],
        |row| row.get::<_, i64>(0),
    )? != 0)
}

fn ensure_column(
    connection: &rusqlite::Transaction<'_>,
    table: &str,
    column: &str,
    definition: &str,
) -> Result<(), CoreError> {
    if !table_columns(connection, table)?
        .iter()
        .any(|name| name == column)
    {
        connection.execute(
            &format!("ALTER TABLE {table} ADD COLUMN {column} {definition}"),
            [],
        )?;
    }
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
        markup_language: row.get(4)?,
        is_draft: row.get::<_, i64>(5)? != 0,
        created_time: row.get(6)?,
        updated_time: row.get(7)?,
        deleted_time: row.get(8)?,
    })
}

fn row_to_note_list_item(row: &rusqlite::Row<'_>) -> rusqlite::Result<NoteListItem> {
    Ok(NoteListItem {
        id: row.get(0)?,
        title: row.get(1)?,
        body_text: row.get(2)?,
        updated_time: row.get(3)?,
        first_image_id: row.get(4)?,
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

/// Build the shared ranked search query used by both full-note and lightweight
/// preview projections. FTS5 supplies prefix candidates and bm25 scores while
/// the literal branch catches substrings (especially CJK) that the tokenizer
/// cannot address. The two candidate sources are deliberately UNIONed rather
/// than used as a fallback, so one source cannot hide relevant results from
/// the other.
fn search_query_sql(projection: &str) -> String {
    format!(
        "WITH fts_hits AS (
             SELECT notes_fts.id, bm25(notes_fts) AS bm25_score
             FROM notes_fts JOIN notes n ON n.id = notes_fts.id
             WHERE notes_fts MATCH ?1 AND n.deleted_time = 0
         ),
         title_hits AS (
             SELECT notes_fts.id
             FROM notes_fts JOIN notes n ON n.id = notes_fts.id
             WHERE notes_fts MATCH ?2 AND n.deleted_time = 0
         ),
         literal_hits AS (
             SELECT n.id
             FROM notes n
             WHERE n.deleted_time = 0
               AND (instr(lower(n.title), lower(?3)) > 0
                    OR instr(lower(n.body_text), lower(?3)) > 0)
         ),
         candidate_ids AS (
             SELECT id FROM fts_hits
             UNION
             SELECT id FROM literal_hits
         ),
         ranked AS (
             SELECT c.id,
                    CASE
                        WHEN lower(n.title) = lower(?3) THEN 0
                        WHEN instr(lower(n.title), lower(?3)) = 1 THEN 1
                        WHEN instr(lower(n.title), lower(?3)) > 0
                             OR EXISTS (SELECT 1 FROM title_hits t WHERE t.id = c.id)
                            THEN 2
                        ELSE 3
                    END AS rank_tier,
                    MIN(f.bm25_score) AS bm25_score,
                    MAX(n.updated_time) AS updated_time
             FROM candidate_ids c
             JOIN notes n ON n.id = c.id AND n.deleted_time = 0
             LEFT JOIN fts_hits f ON f.id = c.id
             GROUP BY c.id
         )
         SELECT {projection}
         FROM ranked JOIN notes n ON n.id = ranked.id
         ORDER BY ranked.rank_tier ASC,
                  CASE WHEN ranked.bm25_score IS NULL THEN 1 ELSE 0 END ASC,
                  ranked.bm25_score ASC,
                  ranked.updated_time DESC,
                  ranked.id ASC"
    )
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
    sanitize_fts_query_for_column(query, "")
}

fn sanitize_fts_query_for_column(query: &str, column: &str) -> String {
    query
        .split_whitespace()
        .map(|term| {
            let escaped = term.replace('"', "\"\"");
            let phrase = format!("\"{escaped}\"*");
            if column.is_empty() {
                phrase
            } else {
                format!("{column} : {phrase}")
            }
        })
        .collect::<Vec<_>>()
        .join(" AND ")
}

#[cfg(test)]
mod tests {
    use super::{
        CoreError, CreateNote, HtmlNoteConversion, NoteContentUpdate, NoteRepository, UpdateNote,
        backup_test_hooks, migration_test_hooks, partial_path_for_target,
    };
    use crate::html_body::{parse_html, search_text};
    use crate::resource_store::ResourceImport;
    use rusqlite::Connection;
    use rusqlite::hooks::{AuthAction, AuthContext, Authorization};
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::mpsc::channel;
    use std::time::Duration;
    use tempfile::tempdir;

    const TINY_PNG: &[u8] = b"tiny png bytes";

    #[test]
    fn lightweight_note_projection_never_reads_canonical_body() {
        let temp = tempdir().unwrap();
        let repo = NoteRepository::open(temp.path().join("notes.sqlite")).unwrap();
        repo.create_note(CreateNote {
            title: "标题".repeat(100),
            body: format!("<p>{}</p>", "不会读取正文 BLOB ".repeat(100)),
            is_draft: false,
        })
        .unwrap();
        let read_body = Arc::new(AtomicBool::new(false));
        let seen = Arc::clone(&read_body);
        repo.connection
            .lock()
            .unwrap()
            .authorizer(Some(move |context: AuthContext<'_>| {
                if let AuthAction::Read {
                    table_name: "notes",
                    column_name: "body",
                } = context.action
                {
                    seen.store(true, Ordering::SeqCst);
                    Authorization::Deny
                } else {
                    Authorization::Allow
                }
            }));
        let rows = repo.list_note_previews().unwrap();
        assert_eq!(rows.len(), 1);
        assert!(rows[0].title.chars().count() <= 120);
        assert!(rows[0].body_text.chars().count() <= 96);
        assert_eq!(repo.search_note_previews("正文").unwrap().len(), 1);
        assert!(!read_body.load(Ordering::SeqCst));
    }

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
    fn rollback_unassociated_resource_preserves_associations_and_shared_blobs() {
        let temp = tempdir().unwrap();
        let repo = NoteRepository::open(temp.path().join("notes.sqlite")).unwrap();
        let first = repo
            .import_resource(png_import(TINY_PNG, "first.png"))
            .unwrap();
        let second = repo
            .import_resource(png_import(TINY_PNG, "second.png"))
            .unwrap();

        repo.rollback_unassociated_resource(&first.id).unwrap();
        assert!(repo.get_resource(&first.id).unwrap().is_none());
        assert!(repo.get_resource(&second.id).unwrap().is_some());
        assert_eq!(count_rows(&repo, "resources"), 1);
        assert_eq!(count_rows(&repo, "resource_blobs"), 1);

        repo.create_note(CreateNote {
            title: "关联图片".into(),
            body: format!("<p><img src=\":/{}\" alt=\"图片\"></p>", second.id),
            is_draft: false,
        })
        .unwrap();
        repo.rollback_unassociated_resource(&second.id).unwrap();
        assert!(repo.get_resource(&second.id).unwrap().is_some());
        assert_eq!(count_rows(&repo, "resources"), 1);
        assert_eq!(count_rows(&repo, "resource_blobs"), 1);

        let unique = repo
            .import_resource(ResourceImport {
                bytes: b"different image bytes",
                title: "unique.png",
                mime: "image/png",
                file_extension: "png",
            })
            .unwrap();
        assert!(unique.path.exists());
        assert_eq!(count_rows(&repo, "resource_blobs"), 2);
        repo.rollback_unassociated_resource(&unique.id).unwrap();
        assert!(repo.get_resource(&unique.id).unwrap().is_none());
        assert_eq!(count_rows(&repo, "resource_blobs"), 1);
        assert!(unique.path.exists());
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
        let connection = repo.connection.lock().unwrap();
        for (table, column) in [
            ("resource_blobs", "mime"),
            ("resource_blobs", "relative_path"),
            ("resource_blobs", "created_time"),
            ("resources", "size"),
            ("resources", "updated_time"),
            ("resources", "deleted_time"),
            ("note_resources", "is_associated"),
            ("note_resources", "last_seen_time"),
        ] {
            let found: i64 = connection
                .query_row(
                    &format!(
                        "SELECT count(*) FROM pragma_table_info('{table}') WHERE name = '{column}'"
                    ),
                    [],
                    |row| row.get(0),
                )
                .unwrap();
            assert_eq!(found, 1, "missing {table}.{column}");
        }
        drop(connection);
        let first = repo
            .import_resource(png_import(TINY_PNG, "first.png"))
            .unwrap();
        let second = repo
            .import_resource(png_import(b"other bytes", "second.png"))
            .unwrap();
        repo.connection
            .lock()
            .unwrap()
            .execute(
                "UPDATE notes SET markup_language = 2 WHERE id = '0123456789abcdef0123456789abcdef'",
                [],
            )
            .unwrap();
        let note = repo
            .update_note_content(
                "0123456789abcdef0123456789abcdef",
                NoteContentUpdate {
                    title: "updated".into(),
                    body: format!("<p><img src=\":/{}\" alt=\"first\"></p>", first.id),
                },
            )
            .unwrap();
        assert_eq!(note.body_text, "first");
        assert_eq!(count_rows(&repo, "note_resources"), 1);
        repo.update_note_content(
            &note.id,
            NoteContentUpdate {
                title: "updated again".into(),
                body: format!("<p><img src=\":/{}\" alt=\"second\"></p>", second.id),
            },
        )
        .unwrap();
        assert_eq!(count_rows(&repo, "note_resources"), 1);
        assert!(repo.get_resource(&first.id).unwrap().is_some());
        assert!(repo.get_resource(&second.id).unwrap().is_some());
    }

    #[test]
    fn content_update_skips_missing_metadata_but_keeps_existing_association() {
        let temp = tempdir().unwrap();
        let repo = NoteRepository::open(temp.path().join("notes.sqlite")).unwrap();
        let resource = repo
            .import_resource(png_import(TINY_PNG, "present.png"))
            .unwrap();
        let note = repo
            .create_note(CreateNote {
                title: "标题".into(),
                body: "初始".into(),
                is_draft: false,
            })
            .unwrap();
        let missing = "0123456789abcdef0123456789abcdef";
        let updated = repo
            .update_note_content(
                &note.id,
                NoteContentUpdate {
                    title: "更新标题".into(),
                    body: format!(
                        "<p><img src=\":/{missing}\" alt=\"缺失\"><img src=\":/{}\" alt=\"存在\"></p>",
                        resource.id
                    ),
                },
            )
            .unwrap();
        assert_eq!(updated.title, "更新标题");
        let connection = repo.connection.lock().unwrap();
        let associations: Vec<(String, i64)> = connection
            .prepare(
                "SELECT resource_id, position FROM note_resources
                 WHERE note_id = ?1 ORDER BY position",
            )
            .unwrap()
            .query_map([&note.id], |row| Ok((row.get(0)?, row.get(1)?)))
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap();
        assert_eq!(associations, vec![(resource.id, 0)]);
    }

    #[test]
    fn content_update_keeps_metadata_for_missing_blob_alongside_normal_resource() {
        let temp = tempdir().unwrap();
        let repo = NoteRepository::open(temp.path().join("notes.sqlite")).unwrap();
        let missing_blob = repo
            .import_resource(png_import(TINY_PNG, "missing-blob.png"))
            .unwrap();
        let normal = repo
            .import_resource(png_import(b"other bytes", "normal.png"))
            .unwrap();
        std::fs::remove_file(&missing_blob.path).unwrap();
        let note = repo
            .create_note(CreateNote {
                title: "混合资源".into(),
                body: "初始".into(),
                is_draft: false,
            })
            .unwrap();
        repo.update_note_content(
            &note.id,
            NoteContentUpdate {
                title: "混合资源已保存".into(),
                body: format!(
                    "<p><img src=\":/{}\" alt=\"缺 blob\"><img src=\":/{}\" alt=\"正常\"></p>",
                    missing_blob.id, normal.id
                ),
            },
        )
        .unwrap();
        let connection = repo.connection.lock().unwrap();
        let count: i64 = connection
            .query_row(
                "SELECT count(*) FROM note_resources WHERE note_id = ?1",
                [&note.id],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(count, 2);
    }

    #[test]
    fn search_uses_alt_text_but_not_digest_or_path_text() {
        let temp = tempdir().unwrap();
        let repo = NoteRepository::open(temp.path().join("notes.sqlite")).unwrap();
        let resource = repo
            .import_resource(png_import(TINY_PNG, "screenshot.png"))
            .unwrap();
        let id = resource.id.as_str();
        let body = format!("<p>证据</p><p><img src=\":/{id}\" alt=\"庭审截图 [1]\"></p>");
        let note = repo
            .create_note(CreateNote {
                title: "图片笔记".into(),
                body: body.clone(),
                is_draft: false,
            })
            .unwrap();
        assert_eq!(note.body_text, search_text(&parse_html(&body).unwrap()));
        assert_eq!(repo.search("庭审截图").unwrap().len(), 1);
        assert!(repo.search(&resource.sha256).unwrap().is_empty());
        assert!(
            repo.search(resource.path.to_string_lossy().as_ref())
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn search_unions_fts_prefix_and_literal_chinese_substring_candidates() {
        let temp = tempdir().unwrap();
        let repo = NoteRepository::open(temp.path().join("notes.sqlite")).unwrap();
        let fts = repo
            .create_note(CreateNote {
                title: "人工智能入门".into(),
                body: "tokenized body".into(),
                is_draft: false,
            })
            .unwrap();
        let literal = repo
            .create_note(CreateNote {
                title: "超级人工智能".into(),
                body: "literal substring".into(),
                is_draft: false,
            })
            .unwrap();

        let ids = repo
            .search("人工")
            .unwrap()
            .into_iter()
            .map(|note| note.id)
            .collect::<Vec<_>>();
        assert!(ids.contains(&fts.id));
        assert!(ids.contains(&literal.id));
        assert_eq!(
            ids,
            repo.search_note_previews("人工")
                .unwrap()
                .into_iter()
                .map(|preview| preview.id)
                .collect::<Vec<_>>()
        );
    }

    #[test]
    fn search_ranks_exact_title_before_prefix_substring_and_body() {
        let temp = tempdir().unwrap();
        let repo = NoteRepository::open(temp.path().join("notes.sqlite")).unwrap();
        let exact = repo
            .create_note(CreateNote {
                title: "目标".into(),
                body: "ordinary body".into(),
                is_draft: false,
            })
            .unwrap();
        let prefix = repo
            .create_note(CreateNote {
                title: "目标 前缀".into(),
                body: "ordinary body".into(),
                is_draft: false,
            })
            .unwrap();
        let substring = repo
            .create_note(CreateNote {
                title: "前目标后".into(),
                body: "ordinary body".into(),
                is_draft: false,
            })
            .unwrap();
        let body = repo
            .create_note(CreateNote {
                title: "普通标题".into(),
                body: "正文里的目标".into(),
                is_draft: false,
            })
            .unwrap();
        repo.update_note(
            &body.id,
            UpdateNote {
                body: Some("更新后的正文目标".into()),
                ..UpdateNote::default()
            },
        )
        .unwrap();

        let ids = repo
            .search("目标")
            .unwrap()
            .into_iter()
            .map(|note| note.id)
            .collect::<Vec<_>>();
        assert_eq!(ids, vec![exact.id, prefix.id, substring.id, body.id]);

        let ascii = repo
            .create_note(CreateNote {
                title: "Target".into(),
                body: "ordinary body".into(),
                is_draft: false,
            })
            .unwrap();
        assert_eq!(
            repo.search("target")
                .unwrap()
                .into_iter()
                .map(|note| note.id)
                .collect::<Vec<_>>(),
            vec![ascii.id]
        );
    }

    #[test]
    fn search_escapes_quotes_and_punctuation_without_fts_errors() {
        let temp = tempdir().unwrap();
        let repo = NoteRepository::open(temp.path().join("notes.sqlite")).unwrap();
        let note = repo
            .create_note(CreateNote {
                title: "引号 \"测试\"".into(),
                body: "方括号 [安全]".into(),
                is_draft: false,
            })
            .unwrap();

        for (query, should_match) in [
            ("\"", true),
            ("\"测试\"", true),
            ("[安全]", true),
            ("field:value", false),
            ("*", false),
            ("-", false),
            ("OR", false),
            ("foo\"bar", false),
        ] {
            let results = repo.search(query).unwrap();
            let preview_results = repo.search_note_previews(query).unwrap();
            assert_eq!(
                results.iter().map(|item| &item.id).collect::<Vec<_>>(),
                preview_results
                    .iter()
                    .map(|item| &item.id)
                    .collect::<Vec<_>>()
            );
            if should_match {
                assert!(results.iter().any(|item| item.id == note.id));
            }
        }
    }

    #[test]
    fn search_excludes_soft_deleted_notes_from_both_projections() {
        let temp = tempdir().unwrap();
        let repo = NoteRepository::open(temp.path().join("notes.sqlite")).unwrap();
        let note = repo
            .create_note(CreateNote {
                title: "待删除目标".into(),
                body: "body".into(),
                is_draft: false,
            })
            .unwrap();
        repo.soft_delete(&note.id).unwrap();
        assert!(repo.search("目标").unwrap().is_empty());
        assert!(repo.search_note_previews("目标").unwrap().is_empty());
    }

    #[test]
    fn empty_image_projection_survives_reopen_without_digest_search_hits() {
        let temp = tempdir().unwrap();
        let db = temp.path().join("notes.sqlite");
        let repo = NoteRepository::open(&db).unwrap();
        let resource = repo
            .import_resource(png_import(TINY_PNG, "empty-alt.png"))
            .unwrap();
        let body = format!("<p><img src=\":/{}\" alt=\"\"></p>", resource.id);
        let note = repo
            .create_note(CreateNote {
                title: "纯图片".into(),
                body,
                is_draft: false,
            })
            .unwrap();
        assert!(note.body_text.is_empty());
        drop(repo);
        let reopened = NoteRepository::open(&db).unwrap();
        assert!(
            reopened
                .get_note(&note.id)
                .unwrap()
                .unwrap()
                .body_text
                .is_empty()
        );
        assert!(reopened.search(&resource.sha256).unwrap().is_empty());
    }

    #[test]
    fn reopen_repairs_missing_fts_rows_from_body_text_without_mutating_notes() {
        let temp = tempdir().unwrap();
        let db = temp.path().join("notes.sqlite");
        let repo = NoteRepository::open(&db).unwrap();
        let first = repo
            .create_note(CreateNote {
                title: "第一笔记".into(),
                body: "共同关键词 一".into(),
                is_draft: false,
            })
            .unwrap();
        let second = repo
            .create_note(CreateNote {
                title: "第二笔记".into(),
                body: "共同关键词 二".into(),
                is_draft: false,
            })
            .unwrap();
        let first_body = repo.get_note(&first.id).unwrap().unwrap().body_text;
        let second_body = repo.get_note(&second.id).unwrap().unwrap().body_text;
        repo.connection
            .lock()
            .unwrap()
            .execute("DELETE FROM notes_fts WHERE id = ?1", [&first.id])
            .unwrap();
        drop(repo);
        let reopened = NoteRepository::open(&db).unwrap();
        let results = reopened.search("共同关键词").unwrap();
        assert_eq!(results.len(), 2);
        assert_eq!(
            reopened.get_note(&first.id).unwrap().unwrap().body_text,
            first_body
        );
        assert_eq!(
            reopened.get_note(&second.id).unwrap().unwrap().body_text,
            second_body
        );
    }

    #[test]
    fn fresh_schema_and_normal_note_use_html_language_two_and_empty_rtf() {
        let temp = tempdir().unwrap();
        let db = temp.path().join("notes.sqlite");
        let repo = NoteRepository::open(&db).unwrap();
        let note = repo
            .create_note(CreateNote {
                title: "HTML".into(),
                body: "<p><strong>正文</strong> 😀</p>".into(),
                is_draft: false,
            })
            .unwrap();
        assert_eq!(note.markup_language, 2);
        assert_eq!(note.body, "<p><strong>正文</strong> 😀</p>");
        assert_eq!(note.body_text, "正文 😀");
        let connection = Connection::open(&db).unwrap();
        assert_eq!(
            connection
                .query_row("PRAGMA user_version", [], |row| row.get::<_, i64>(0))
                .unwrap(),
            3
        );
        assert_eq!(
            connection
                .query_row(
                    "SELECT body_rtf, markup_language FROM notes WHERE id = ?1",
                    [&note.id],
                    |row| Ok((row.get::<_, Vec<u8>>(0)?, row.get::<_, i64>(1)?)),
                )
                .unwrap(),
            (Vec::new(), 2)
        );
    }

    #[test]
    fn v2_schema_upgrade_marks_existing_rows_legacy_without_changing_content() {
        let temp = tempdir().unwrap();
        let db = temp.path().join("notes.sqlite");
        let old_rtf = b"{\\rtf1\\b legacy}".to_vec();
        let connection = Connection::open(&db).unwrap();
        connection
            .execute_batch(
                "CREATE TABLE notes (
                    id TEXT PRIMARY KEY NOT NULL, title TEXT NOT NULL, body TEXT NOT NULL,
                    body_text TEXT NOT NULL, body_rtf BLOB NOT NULL,
                    is_draft INTEGER NOT NULL, created_time INTEGER NOT NULL,
                    updated_time INTEGER NOT NULL, deleted_time INTEGER NOT NULL DEFAULT 0
                );
                PRAGMA user_version = 2;",
            )
            .unwrap();
        connection
            .execute(
                "INSERT INTO notes
                 (id, title, body, body_text, body_rtf, is_draft, created_time, updated_time)
                 VALUES (?1, ?2, ?3, ?4, ?5, 1, 101, 202)",
                rusqlite::params![
                    "0123456789abcdef0123456789abcdef",
                    "旧标题",
                    "旧 body",
                    "旧 text",
                    old_rtf
                ],
            )
            .unwrap();
        drop(connection);

        let repo = NoteRepository::open(&db).unwrap();
        let note = repo
            .list_legacy_notes_for_html_migration()
            .unwrap()
            .pop()
            .unwrap();
        assert_eq!(note.title, "旧标题");
        assert_eq!(note.body, "旧 body");
        assert_eq!(note.body_rtf, b"{\\rtf1\\b legacy}".to_vec());
        assert_eq!((note.created_time, note.updated_time), (101, 202));
        let connection = Connection::open(&db).unwrap();
        assert_eq!(
            connection
                .query_row(
                    "SELECT markup_language FROM notes WHERE id = ?1",
                    [&note.id],
                    |row| row.get::<_, i64>(0),
                )
                .unwrap(),
            1
        );
    }

    #[test]
    fn normal_updates_reject_legacy_rows_without_touching_any_projection() {
        let temp = tempdir().unwrap();
        let db = temp.path().join("notes.sqlite");
        let repo = NoteRepository::open(&db).unwrap();
        let note = repo
            .create_note(CreateNote {
                title: "旧标题".into(),
                body: "旧正文".into(),
                is_draft: true,
            })
            .unwrap();
        repo.connection
            .lock()
            .unwrap()
            .execute(
                "UPDATE notes SET title = '旧标题', body = 'legacy marker', body_text = '旧正文',
                 body_rtf = X'727466', markup_language = 1, updated_time = 12345 WHERE id = ?1",
                [&note.id],
            )
            .unwrap();
        let before = repo.get_note(&note.id).unwrap().unwrap();
        assert!(matches!(
            repo.update_note(
                &note.id,
                UpdateNote {
                    title: Some("不应写入".into()),
                    body: None,
                }
            ),
            Err(CoreError::MigrationRequired)
        ));
        assert!(matches!(
            repo.update_note_content(
                &note.id,
                NoteContentUpdate {
                    title: "不应写入".into(),
                    body: "<p>new</p>".into(),
                }
            ),
            Err(CoreError::MigrationRequired)
        ));
        let after = repo.get_note(&note.id).unwrap().unwrap();
        assert_eq!(after, before);
        let connection = repo.connection.lock().unwrap();
        assert_eq!(
            connection
                .query_row(
                    "SELECT count(*) FROM notes_fts WHERE id = ?1",
                    [&note.id],
                    |row| row.get::<_, i64>(0),
                )
                .unwrap(),
            1
        );
    }

    #[test]
    fn html_migration_is_atomic_and_builds_ordered_resources_and_fts() {
        let temp = tempdir().unwrap();
        let db = temp.path().join("notes.sqlite");
        let repo = NoteRepository::open(&db).unwrap();
        let first_resource = repo
            .import_resource(png_import(TINY_PNG, "first.png"))
            .unwrap();
        let second_resource = repo
            .import_resource(png_import(b"other bytes", "second.png"))
            .unwrap();
        let first = repo
            .create_note(CreateNote {
                title: "第一".into(),
                body: "legacy".into(),
                is_draft: true,
            })
            .unwrap();
        let second = repo
            .create_note(CreateNote {
                title: "第二".into(),
                body: "legacy".into(),
                is_draft: false,
            })
            .unwrap();
        let old_rtf = b"{\\rtf1 first}".to_vec();
        let connection = repo.connection.lock().unwrap();
        connection
            .execute(
                "UPDATE notes SET markup_language = 1, body = 'old first', body_rtf = ?2
                 WHERE id = ?1",
                rusqlite::params![first.id, old_rtf],
            )
            .unwrap();
        connection
            .execute(
                "UPDATE notes SET markup_language = 1, body = 'old second', body_rtf = X'727466', deleted_time = 777
                 WHERE id = ?1",
                [&second.id],
            )
            .unwrap();
        drop(connection);
        let first_body = format!(
            "<p>迁移 <img src=\":/{}\" alt=\"第一图\"><img src=\":/{}\" alt=\"第一图\"><img src=\":/{}\" alt=\"第二图\"></p>",
            first_resource.id, first_resource.id, second_resource.id
        );
        let second_body = "<p><strong>另一个正文</strong></p>".to_owned();
        let backup = repo
            .apply_html_migration(vec![
                HtmlNoteConversion {
                    id: first.id.clone(),
                    source_updated_time: first.updated_time,
                    body: first_body.clone(),
                    body_text: "迁移 第一图第一图第二图".into(),
                    resource_ids: vec![
                        first_resource.id.clone(),
                        first_resource.id.clone(),
                        second_resource.id.clone(),
                    ],
                },
                HtmlNoteConversion {
                    id: second.id.clone(),
                    source_updated_time: second.updated_time,
                    body: second_body.clone(),
                    body_text: "另一个正文".into(),
                    resource_ids: Vec::new(),
                },
            ])
            .unwrap();
        assert!(backup.exists());
        let migrated = repo.get_note(&first.id).unwrap().unwrap();
        assert_eq!(migrated.markup_language, 2);
        assert_eq!(migrated.body, first_body);
        assert_eq!(migrated.body_text, "迁移 第一图第一图第二图");
        assert_eq!(repo.search("第一图").unwrap().len(), 1);
        let connection = repo.connection.lock().unwrap();
        let associations: Vec<String> = connection
            .prepare("SELECT resource_id FROM note_resources WHERE note_id = ?1 ORDER BY position")
            .unwrap()
            .query_map([&first.id], |row| row.get(0))
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap();
        assert_eq!(
            associations,
            vec![first_resource.id.clone(), second_resource.id.clone()]
        );
        drop(connection);
        assert_eq!(repo.search("另一个正文").unwrap().len(), 0);
        let connection = Connection::open(&db).unwrap();
        assert_eq!(
            connection
                .query_row(
                    "SELECT markup_language, deleted_time FROM notes WHERE id = ?1",
                    [&second.id],
                    |row| Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)?)),
                )
                .unwrap(),
            (2, 777)
        );
        assert_eq!(
            connection
                .query_row(
                    "SELECT count(*) FROM notes_fts WHERE id = ?1",
                    [&second.id],
                    |row| row.get::<_, i64>(0),
                )
                .unwrap(),
            0
        );
        drop(connection);
        let backup_connection = Connection::open(backup).unwrap();
        assert_eq!(
            backup_connection
                .query_row(
                    "SELECT body, body_rtf FROM notes WHERE id = ?1",
                    [&first.id],
                    |row| Ok((row.get::<_, String>(0)?, row.get::<_, Vec<u8>>(1)?)),
                )
                .unwrap(),
            ("old first".into(), b"{\\rtf1 first}".to_vec())
        );
    }

    #[test]
    fn invalid_html_migration_batches_roll_back_without_changing_legacy_rows() {
        let temp = tempdir().unwrap();
        let db = temp.path().join("notes.sqlite");
        let repo = NoteRepository::open(&db).unwrap();
        let note = repo
            .create_note(CreateNote {
                title: "legacy".into(),
                body: "old".into(),
                is_draft: false,
            })
            .unwrap();
        repo.connection
            .lock()
            .unwrap()
            .execute(
                "UPDATE notes SET markup_language = 1, body = 'old', body_rtf = X'727466' WHERE id = ?1",
                [&note.id],
            )
            .unwrap();
        let bad = HtmlNoteConversion {
            id: note.id.clone(),
            source_updated_time: note.updated_time,
            body: "<p>new</p>".into(),
            body_text: "wrong".into(),
            resource_ids: Vec::new(),
        };
        assert!(matches!(
            repo.apply_html_migration(vec![bad]),
            Err(CoreError::MigrationValidation(_))
        ));
        let unchanged = repo.get_note(&note.id).unwrap().unwrap();
        assert_eq!(unchanged.body, "old");
        assert_eq!(unchanged.markup_language, 1);
        repo.connection
            .lock()
            .unwrap()
            .execute(
                "UPDATE notes SET updated_time = 54321 WHERE id = ?1",
                [&note.id],
            )
            .unwrap();
        assert!(matches!(
            repo.apply_html_migration(vec![HtmlNoteConversion {
                id: note.id.clone(),
                source_updated_time: note.updated_time,
                body: "<p>new</p>".into(),
                body_text: "new".into(),
                resource_ids: Vec::new(),
            }]),
            Err(CoreError::MigrationValidation(_))
        ));
        assert_eq!(repo.get_note(&note.id).unwrap().unwrap().body, "old");
        assert!(matches!(
            repo.apply_html_migration(Vec::new()),
            Err(CoreError::MigrationValidation(_))
        ));
        assert!(matches!(
            repo.apply_html_migration(vec![HtmlNoteConversion {
                id: "ffffffffffffffffffffffffffffffff".into(),
                source_updated_time: 0,
                body: "<p>new</p>".into(),
                body_text: "new".into(),
                resource_ids: Vec::new(),
            }]),
            Err(CoreError::MigrationValidation(_))
        ));
    }

    #[test]
    fn migration_lock_covers_backup_through_commit() {
        let temp = tempdir().unwrap();
        let repo = Arc::new(NoteRepository::open(temp.path().join("notes.sqlite")).unwrap());
        let note = repo
            .create_note(CreateNote {
                title: "legacy".into(),
                body: "old".into(),
                is_draft: false,
            })
            .unwrap();
        repo.connection
            .lock()
            .unwrap()
            .execute(
                "UPDATE notes SET markup_language = 1 WHERE id = ?1",
                [&note.id],
            )
            .unwrap();
        let database_path = repo.database_path.clone();
        migration_test_hooks()
            .lock()
            .unwrap()
            .pause_after_backup
            .insert(database_path.clone());
        let migration_repo = Arc::clone(&repo);
        let migration_id = note.id.clone();
        let source_updated_time = note.updated_time;
        let migration = std::thread::spawn(move || {
            migration_repo.apply_html_migration(vec![HtmlNoteConversion {
                id: migration_id,
                source_updated_time,
                body: "<p>new</p>".into(),
                body_text: "new".into(),
                resource_ids: Vec::new(),
            }])
        });
        for _ in 0..200 {
            if migration_test_hooks()
                .lock()
                .unwrap()
                .backup_paused
                .contains(&database_path)
            {
                break;
            }
            std::thread::sleep(Duration::from_millis(1));
        }
        assert!(
            migration_test_hooks()
                .lock()
                .unwrap()
                .backup_paused
                .contains(&database_path)
        );
        let (started_sender, started_receiver) = channel();
        let update_repo = Arc::clone(&repo);
        let update_id = note.id.clone();
        let update = std::thread::spawn(move || {
            started_sender.send(()).unwrap();
            update_repo.update_note(
                &update_id,
                UpdateNote {
                    title: Some("after migration".into()),
                    body: None,
                },
            )
        });
        started_receiver.recv().unwrap();
        std::thread::sleep(Duration::from_millis(20));
        migration_test_hooks()
            .lock()
            .unwrap()
            .release_backup
            .insert(database_path.clone());
        assert!(migration.join().unwrap().unwrap().exists());
        assert_eq!(repo.get_note(&note.id).unwrap().unwrap().body, "<p>new</p>");
        assert_eq!(update.join().unwrap().unwrap().title, "after migration");
        assert_eq!(
            repo.get_note(&note.id).unwrap().unwrap().title,
            "after migration"
        );
        let mut hooks = migration_test_hooks().lock().unwrap();
        hooks.pause_after_backup.remove(&database_path);
        hooks.backup_paused.remove(&database_path);
        hooks.release_backup.remove(&database_path);
    }

    #[cfg(unix)]
    #[test]
    fn backup_rejects_symlink_preexisting_and_out_of_profile_targets() {
        let temp = tempdir().unwrap();
        let profile = temp.path().join("profile");
        std::fs::create_dir(&profile).unwrap();
        let repo = NoteRepository::open(profile.join("notes.sqlite")).unwrap();
        let existing = profile.join("existing.sqlite");
        std::fs::write(&existing, b"existing").unwrap();
        assert!(matches!(
            repo.backup_before_html_migration_to(&existing),
            Err(CoreError::InvalidBackupTarget)
        ));
        let target = profile.join("target.sqlite");
        let outside = temp.path().join("outside.sqlite");
        std::fs::write(&outside, b"outside").unwrap();
        std::os::unix::fs::symlink(&outside, &target).unwrap();
        assert!(matches!(
            repo.backup_before_html_migration_to(&target),
            Err(CoreError::InvalidBackupTarget)
        ));
        assert!(matches!(
            repo.backup_before_html_migration_to(&outside),
            Err(CoreError::InvalidBackupTarget)
        ));
        let race = profile.join("race.sqlite");
        backup_test_hooks()
            .lock()
            .unwrap()
            .publish_race_targets
            .insert(race.clone());
        assert!(matches!(
            repo.backup_before_html_migration_to(&race),
            Err(CoreError::BackupFailure)
        ));
        assert_eq!(std::fs::read(&race).unwrap(), b"foreign final");

        let failed = profile.join("failed.sqlite");
        backup_test_hooks()
            .lock()
            .unwrap()
            .fail_after_destination_open
            .insert(failed.clone());
        assert!(matches!(
            repo.backup_before_html_migration_to(&failed),
            Err(CoreError::BackupFailure)
        ));
        assert!(!failed.exists());
        let partial = partial_path_for_target(&failed).unwrap();
        assert!(!partial.exists());

        let replaced = profile.join("replaced.sqlite");
        let mut hooks = backup_test_hooks().lock().unwrap();
        hooks.fail_after_destination_open.insert(replaced.clone());
        hooks
            .replace_partial_before_cleanup
            .insert(replaced.clone());
        drop(hooks);
        assert!(matches!(
            repo.backup_before_html_migration_to(&replaced),
            Err(CoreError::BackupFailure)
        ));
        let foreign_partial = partial_path_for_target(&replaced).unwrap();
        assert_eq!(std::fs::read(foreign_partial).unwrap(), b"foreign partial");
    }

    #[cfg(unix)]
    #[test]
    fn backup_fails_closed_when_partial_path_is_swapped_during_sqlite_open() {
        let temp = tempdir().unwrap();
        let profile = temp.path().join("profile");
        std::fs::create_dir(&profile).unwrap();
        let repo = NoteRepository::open(profile.join("notes.sqlite")).unwrap();
        let target = profile.join("aba.sqlite");
        backup_test_hooks()
            .lock()
            .unwrap()
            .aba_swap_targets
            .insert(target.clone());

        assert!(matches!(
            repo.backup_before_html_migration_to(&target),
            Err(CoreError::BackupFailure)
        ));
        assert!(!target.exists());
        assert!(!partial_path_for_target(&target).unwrap().exists());
    }
}
