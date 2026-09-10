use crate::resource::{ResourceError, ResourceInput, ResourceStore};
use crate::schema::migrate_schema;
use crate::{
    CreateNote, EditJournalEntry, EntityRef, ListQuery, Note, NoteId, NoteProjection, Notebook,
    NotebookId, ResourceId, SaveNote, SavedRevision, Stack, StackId, Tag, TagId,
};
use rusqlite::hooks::{AuthAction, Authorization};
use rusqlite::{Connection, OpenFlags, OptionalExtension, Transaction, params};
use sha2::{Digest, Sha256};
use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{Receiver, Sender, channel};
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};
use thiserror::Error;

static NEXT_ID: AtomicU64 = AtomicU64::new(1);

#[derive(Debug, Error)]
pub enum LibraryError {
    #[error("SQLite storage error")]
    Storage(#[from] rusqlite::Error),
    #[error("resource storage error")]
    Resource(#[from] ResourceError),
    #[error("canonical document error")]
    Document(#[from] crate::DocumentError),
    #[error("library database path is unsafe")]
    InvalidDatabasePath,
    #[error("library schema migration failed")]
    MigrationFailed,
    #[error("schema version {0} is newer than this library supports")]
    UnsupportedSchema(i64),
    #[error("invalid opaque entity ID")]
    InvalidId,
    #[error("invalid note snapshot")]
    InvalidSnapshot,
    #[error("requested entity was not found")]
    NotFound,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LibraryEvent {
    NoteCreated(NoteId),
    NoteProjectionChanged(NoteId),
    NoteTrashed(NoteId),
    NoteRestored(NoteId),
    OrganizationChanged,
    SearchProjectionQueued(NoteId),
    SyncQueued(EntityRef),
}

pub struct LibraryRepository {
    connection: Mutex<Connection>,
    resource_store: ResourceStore,
    events: Mutex<Vec<Sender<LibraryEvent>>>,
    list_observers: Mutex<Vec<Sender<Vec<String>>>>,
    #[allow(dead_code)]
    database_path: PathBuf,
}

impl LibraryRepository {
    pub fn open(path: impl AsRef<Path>) -> Result<Self, LibraryError> {
        let path = checked_database_path(path.as_ref())?;
        let flags = OpenFlags::SQLITE_OPEN_READ_WRITE
            | OpenFlags::SQLITE_OPEN_CREATE
            | OpenFlags::SQLITE_OPEN_NO_MUTEX
            | OpenFlags::SQLITE_OPEN_NOFOLLOW;
        let mut connection = Connection::open_with_flags(&path, flags)
            .map_err(|_| LibraryError::InvalidDatabasePath)?;
        connection.pragma_update(None, "journal_mode", "WAL")?;
        connection.pragma_update(None, "foreign_keys", "ON")?;
        migrate_schema(&mut connection, &new_id()).map_err(|error| match error {
            LibraryError::Storage(_) => LibraryError::MigrationFailed,
            other => other,
        })?;
        let resource_store =
            ResourceStore::new(path.parent().ok_or(LibraryError::InvalidDatabasePath)?)?;
        Ok(Self {
            connection: Mutex::new(connection),
            resource_store,
            events: Mutex::new(Vec::new()),
            list_observers: Mutex::new(Vec::new()),
            database_path: path,
        })
    }

    pub fn subscribe(&self) -> Receiver<LibraryEvent> {
        let (sender, receiver) = channel();
        self.events
            .lock()
            .expect("event mutex poisoned")
            .push(sender);
        receiver
    }

    /// Reports actual SQLite read columns for the next list query. This is a
    /// diagnostic boundary used to keep card loading honest as the schema grows.
    pub fn observe_next_list_query(&self) -> Receiver<Vec<String>> {
        let (sender, receiver) = channel();
        self.list_observers
            .lock()
            .expect("observer mutex poisoned")
            .push(sender);
        receiver
    }

    pub fn default_notebook(&self) -> Result<Notebook, LibraryError> {
        let connection = self.connection.lock().expect("library mutex poisoned");
        connection
            .query_row(
                "SELECT id, title, stack_id, revision, is_default FROM notebooks
             WHERE is_default = 1 AND deleted_time = 0 ORDER BY id LIMIT 1",
                [],
                row_to_notebook,
            )
            .map_err(Into::into)
    }

    pub fn create_note(&self, input: CreateNote) -> Result<Note, LibraryError> {
        let now = timestamp();
        let id = NoteId::parse(new_id()).expect("generated ID is valid");
        let html = input.document.to_canonical_html().as_str().to_owned();
        let text = input.document.search_text().as_str().to_owned();
        let resource_ids = input.document.resource_ids();
        let mut connection = self.connection.lock().expect("library mutex poisoned");
        let transaction = connection.transaction()?;
        let notebook_id = match input.notebook_id {
            Some(id) => {
                require_notebook(&transaction, &id)?;
                id
            }
            None => default_notebook_id(&transaction)?,
        };
        transaction.execute(
            "INSERT INTO notes (id, title, body_html, body_text, snippet, notebook_id, selected_thumbnail_id, created_time, updated_time, revision)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, NULL, ?7, ?7, 1)",
            params![id.as_str(), input.title, html, text, snippet(input.document.search_text().as_str()), notebook_id.as_str(), now],
        )?;
        replace_note_resources(&transaction, &id, &resource_ids)?;
        let thumbnail = thumbnail_id(&transaction, &id)?;
        transaction.execute(
            "UPDATE notes SET selected_thumbnail_id = ?2 WHERE id = ?1",
            params![id.as_str(), thumbnail.as_ref().map(ResourceId::as_str)],
        )?;
        transaction.execute(
            "INSERT INTO note_revisions (note_id, revision, title, body_html, body_text, created_time)
             SELECT id, revision, title, body_html, body_text, updated_time FROM notes WHERE id = ?1",
            [id.as_str()],
        )?;
        queue_search(&transaction, &id, now, "snapshot")?;
        enqueue_sync(&transaction, &EntityRef::Note(id.clone()), 1, "create", now)?;
        transaction.commit()?;
        drop(connection);
        self.publish(vec![
            LibraryEvent::NoteCreated(id.clone()),
            LibraryEvent::NoteProjectionChanged(id.clone()),
            LibraryEvent::SearchProjectionQueued(id.clone()),
            LibraryEvent::SyncQueued(EntityRef::Note(id.clone())),
        ]);
        self.load_note(&id)?.ok_or(LibraryError::NotFound)
    }

    pub fn load_note(&self, id: &NoteId) -> Result<Option<Note>, LibraryError> {
        let connection = self.connection.lock().expect("library mutex poisoned");
        let base = connection.query_row(
            "SELECT id, title, body_html, body_text, snippet, notebook_id, created_time, updated_time, deleted_time, revision
             FROM notes WHERE id = ?1", [id.as_str()], row_to_note_base,
        ).optional()?;
        let Some(mut note) = base else {
            return Ok(None);
        };
        note.resource_ids = note_resource_ids(&connection, id)?;
        note.tag_ids = note_tag_ids(&connection, id)?;
        Ok(Some(note))
    }

    pub fn load_note_by_hex(&self, id: &str) -> Result<Option<Note>, LibraryError> {
        self.load_note(&NoteId::parse(id).map_err(|_| LibraryError::InvalidId)?)
    }

    pub fn save_note(&self, input: SaveNote) -> Result<Note, LibraryError> {
        self.save_note_inner(input, false)
    }

    fn save_note_inner(&self, input: SaveNote, clear_journal: bool) -> Result<Note, LibraryError> {
        let now = timestamp();
        let html = input.document.to_canonical_html().as_str().to_owned();
        let text = input.document.search_text().as_str().to_owned();
        if input.document.resource_ids() != input.resource_ids {
            return Err(LibraryError::InvalidSnapshot);
        }
        let mut connection = self.connection.lock().expect("library mutex poisoned");
        let transaction = connection.transaction()?;
        let (notebook_id, revision): (String, i64) = transaction
            .query_row(
                "SELECT notebook_id, revision FROM notes WHERE id = ?1 AND deleted_time = 0",
                [input.id.as_str()],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?
            .ok_or(LibraryError::NotFound)?;
        let revision = revision + 1;
        replace_note_resources(&transaction, &input.id, &input.resource_ids)?;
        let thumbnail = thumbnail_id(&transaction, &input.id)?;
        transaction.execute(
            "UPDATE notes SET title = ?2, body_html = ?3, body_text = ?4, snippet = ?5,
             selected_thumbnail_id = ?6, updated_time = ?7, revision = ?8 WHERE id = ?1",
            params![
                input.id.as_str(),
                input.title,
                html,
                text,
                snippet(&input.document.search_text().as_str()),
                thumbnail.as_ref().map(ResourceId::as_str),
                now,
                revision
            ],
        )?;
        transaction.execute(
            "INSERT INTO note_revisions (note_id, revision, title, body_html, body_text, created_time)
             SELECT id, revision, title, body_html, body_text, updated_time FROM notes WHERE id = ?1",
            [input.id.as_str()],
        )?;
        queue_search(&transaction, &input.id, now, "snapshot")?;
        if clear_journal {
            transaction.execute(
                "DELETE FROM edit_journal WHERE note_id = ?1",
                [input.id.as_str()],
            )?;
        }
        enqueue_sync(
            &transaction,
            &EntityRef::Note(input.id.clone()),
            revision,
            "save",
            now,
        )?;
        transaction.commit()?;
        drop(connection);
        self.publish(vec![
            LibraryEvent::NoteProjectionChanged(input.id.clone()),
            LibraryEvent::SearchProjectionQueued(input.id.clone()),
            LibraryEvent::SyncQueued(EntityRef::Note(input.id.clone())),
        ]);
        let _ = notebook_id;
        self.load_note(&input.id)?.ok_or(LibraryError::NotFound)
    }

    pub fn list_notes(&self, query: ListQuery) -> Result<Vec<NoteProjection>, LibraryError> {
        let observers =
            std::mem::take(&mut *self.list_observers.lock().expect("observer mutex poisoned"));
        let columns = Arc::new(Mutex::new(BTreeSet::new()));
        let connection = self.connection.lock().expect("library mutex poisoned");
        if !observers.is_empty() {
            let captured = Arc::clone(&columns);
            connection.authorizer(Some(move |context: rusqlite::hooks::AuthContext<'_>| {
                if let AuthAction::Read {
                    table_name,
                    column_name,
                } = context.action
                {
                    captured
                        .lock()
                        .expect("column observer mutex poisoned")
                        .insert(format!("{table_name}.{column_name}"));
                }
                Authorization::Allow
            }));
        }
        let result = (|| {
            let mut statement = connection.prepare(
                "SELECT n.id, substr(n.title, 1, 120), substr(n.snippet, 1, 160), n.updated_time,
                        n.deleted_time, n.notebook_id,
                        (SELECT nr.resource_id FROM note_resources nr JOIN resources r ON r.id = nr.resource_id
                         WHERE nr.note_id = n.id AND nr.is_associated = 1 AND r.deleted_time = 0
                           AND r.mime IN ('image/png', 'image/jpeg') ORDER BY nr.position, nr.resource_id LIMIT 1),
                        (SELECT count(*) FROM note_resources nr WHERE nr.note_id = n.id AND nr.is_associated = 1)
                 FROM notes n
                 WHERE (?1 = 1 OR n.deleted_time = 0)
                   AND (?2 IS NULL OR n.notebook_id = ?2)
                   AND (?3 IS NULL OR EXISTS (SELECT 1 FROM note_tags nt WHERE nt.note_id = n.id AND nt.tag_id = ?3))
                 ORDER BY n.updated_time DESC, n.id ASC
                 LIMIT COALESCE(?4, -1)",
            )?;
            let rows = statement.query_map(
                params![
                    query.include_trashed,
                    query.notebook_id.as_ref().map(NotebookId::as_str),
                    query.tag_id.as_ref().map(TagId::as_str),
                    query.limit.map(|limit| limit as i64)
                ],
                row_to_projection,
            )?;
            rows.collect::<Result<Vec<_>, _>>()
                .map_err(LibraryError::from)
        })();
        connection.authorizer(None::<fn(rusqlite::hooks::AuthContext<'_>) -> Authorization>);
        if !observers.is_empty() {
            let fields = columns
                .lock()
                .expect("column observer mutex poisoned")
                .iter()
                .cloned()
                .collect::<Vec<_>>();
            for observer in observers {
                let _ = observer.send(fields.clone());
            }
        }
        result
    }

    pub fn trash_note(&self, id: &NoteId) -> Result<(), LibraryError> {
        self.set_deleted(id, true)
    }
    pub fn restore_note(&self, id: &NoteId) -> Result<(), LibraryError> {
        self.set_deleted(id, false)
    }

    pub fn purge_note(&self, id: &NoteId) -> Result<(), LibraryError> {
        let now = timestamp();
        let mut connection = self.connection.lock().expect("library mutex poisoned");
        let transaction = connection.transaction()?;
        let revision: i64 = transaction
            .query_row(
                "SELECT revision FROM notes WHERE id = ?1 AND deleted_time <> 0",
                [id.as_str()],
                |row| row.get(0),
            )
            .optional()?
            .ok_or(LibraryError::NotFound)?;
        transaction.execute("DELETE FROM notes WHERE id = ?1", [id.as_str()])?;
        enqueue_sync(
            &transaction,
            &EntityRef::Note(id.clone()),
            revision + 1,
            "purge",
            now,
        )?;
        transaction.commit()?;
        self.publish(vec![
            LibraryEvent::NoteProjectionChanged(id.clone()),
            LibraryEvent::SyncQueued(EntityRef::Note(id.clone())),
        ]);
        Ok(())
    }

    pub fn create_notebook(
        &self,
        title: &str,
        stack_id: Option<&StackId>,
    ) -> Result<Notebook, LibraryError> {
        let now = timestamp();
        let id = NotebookId::parse(new_id()).expect("generated ID is valid");
        let mut connection = self.connection.lock().expect("library mutex poisoned");
        let transaction = connection.transaction()?;
        if let Some(stack_id) = stack_id {
            require_stack(&transaction, stack_id)?;
        }
        transaction.execute("INSERT INTO notebooks (id, title, stack_id, revision, created_time, updated_time) VALUES (?1, ?2, ?3, 1, ?4, ?4)", params![id.as_str(), title, stack_id.map(StackId::as_str), now])?;
        enqueue_sync(
            &transaction,
            &EntityRef::Notebook(id.clone()),
            1,
            "create",
            now,
        )?;
        transaction.commit()?;
        self.publish(vec![
            LibraryEvent::OrganizationChanged,
            LibraryEvent::SyncQueued(EntityRef::Notebook(id.clone())),
        ]);
        Ok(Notebook {
            id,
            title: title.into(),
            stack_id: stack_id.cloned(),
            revision: 1,
            is_default: false,
        })
    }

    pub fn create_stack(&self, title: &str) -> Result<Stack, LibraryError> {
        let now = timestamp();
        let id = StackId::parse(new_id()).expect("generated ID is valid");
        let mut connection = self.connection.lock().expect("library mutex poisoned");
        let transaction = connection.transaction()?;
        transaction.execute("INSERT INTO stacks (id, title, revision, created_time, updated_time) VALUES (?1, ?2, 1, ?3, ?3)", params![id.as_str(), title, now])?;
        enqueue_sync(
            &transaction,
            &EntityRef::Stack(id.clone()),
            1,
            "create",
            now,
        )?;
        transaction.commit()?;
        self.publish(vec![
            LibraryEvent::OrganizationChanged,
            LibraryEvent::SyncQueued(EntityRef::Stack(id.clone())),
        ]);
        Ok(Stack {
            id,
            title: title.into(),
            revision: 1,
        })
    }

    pub fn create_tag(&self, title: &str) -> Result<Tag, LibraryError> {
        let now = timestamp();
        let id = TagId::parse(new_id()).expect("generated ID is valid");
        let mut connection = self.connection.lock().expect("library mutex poisoned");
        let transaction = connection.transaction()?;
        transaction.execute("INSERT INTO tags (id, title, revision, created_time, updated_time) VALUES (?1, ?2, 1, ?3, ?3)", params![id.as_str(), title, now])?;
        enqueue_sync(&transaction, &EntityRef::Tag(id.clone()), 1, "create", now)?;
        transaction.commit()?;
        self.publish(vec![
            LibraryEvent::OrganizationChanged,
            LibraryEvent::SyncQueued(EntityRef::Tag(id.clone())),
        ]);
        Ok(Tag {
            id,
            title: title.into(),
            revision: 1,
        })
    }

    pub fn move_notes(
        &self,
        note_ids: &[NoteId],
        notebook_id: &NotebookId,
    ) -> Result<(), LibraryError> {
        let now = timestamp();
        let mut connection = self.connection.lock().expect("library mutex poisoned");
        let transaction = connection.transaction()?;
        require_notebook(&transaction, notebook_id)?;
        let mut events = vec![LibraryEvent::OrganizationChanged];
        for id in note_ids {
            let revision: i64 = transaction
                .query_row(
                    "SELECT revision FROM notes WHERE id = ?1 AND deleted_time = 0",
                    [id.as_str()],
                    |row| row.get::<_, i64>(0),
                )
                .optional()?
                .ok_or(LibraryError::NotFound)?
                + 1;
            transaction.execute(
                "UPDATE notes SET notebook_id = ?2, updated_time = ?3, revision = ?4 WHERE id = ?1",
                params![id.as_str(), notebook_id.as_str(), now, revision],
            )?;
            enqueue_sync(
                &transaction,
                &EntityRef::Note(id.clone()),
                revision,
                "move",
                now,
            )?;
            events.push(LibraryEvent::NoteProjectionChanged(id.clone()));
            events.push(LibraryEvent::SyncQueued(EntityRef::Note(id.clone())));
        }
        transaction.commit()?;
        self.publish(events);
        Ok(())
    }

    pub fn set_note_tags(&self, note_id: &NoteId, tag_ids: &[TagId]) -> Result<(), LibraryError> {
        let now = timestamp();
        let mut connection = self.connection.lock().expect("library mutex poisoned");
        let transaction = connection.transaction()?;
        let revision: i64 = transaction
            .query_row(
                "SELECT revision FROM notes WHERE id = ?1 AND deleted_time = 0",
                [note_id.as_str()],
                |row| row.get::<_, i64>(0),
            )
            .optional()?
            .ok_or(LibraryError::NotFound)?
            + 1;
        for tag_id in tag_ids {
            require_tag(&transaction, tag_id)?;
        }
        transaction.execute(
            "DELETE FROM note_tags WHERE note_id = ?1",
            [note_id.as_str()],
        )?;
        for (position, tag_id) in tag_ids.iter().enumerate() {
            transaction.execute(
                "INSERT INTO note_tags (note_id, tag_id, position) VALUES (?1, ?2, ?3)",
                params![note_id.as_str(), tag_id.as_str(), position as i64],
            )?;
        }
        transaction.execute(
            "UPDATE notes SET updated_time = ?2, revision = ?3 WHERE id = ?1",
            params![note_id.as_str(), now, revision],
        )?;
        enqueue_sync(
            &transaction,
            &EntityRef::Note(note_id.clone()),
            revision,
            "tags",
            now,
        )?;
        transaction.commit()?;
        self.publish(vec![
            LibraryEvent::OrganizationChanged,
            LibraryEvent::NoteProjectionChanged(note_id.clone()),
            LibraryEvent::SyncQueued(EntityRef::Note(note_id.clone())),
        ]);
        Ok(())
    }

    pub fn append_edit_journal(&self, entry: EditJournalEntry) -> Result<(), LibraryError> {
        let now = timestamp();
        let mut connection = self.connection.lock().expect("library mutex poisoned");
        let transaction = connection.transaction()?;
        let exists: bool = transaction.query_row(
            "SELECT EXISTS(SELECT 1 FROM notes WHERE id = ?1 AND deleted_time = 0)",
            [entry.note_id.as_str()],
            |row| row.get(0),
        )?;
        if !exists {
            return Err(LibraryError::NotFound);
        }
        transaction.execute("INSERT INTO edit_journal (id, note_id, generation, delta_utf8, created_time) VALUES (?1, ?2, ?3, ?4, ?5)", params![new_id(), entry.note_id.as_str(), entry.generation, entry.delta_utf8, now])?;
        transaction.commit()?;
        Ok(())
    }

    pub fn flush_snapshot(&self, input: SaveNote) -> Result<SavedRevision, LibraryError> {
        let note = self.save_note_inner(input, true)?;
        Ok(SavedRevision {
            revision: note.revision,
            saved_time: note.updated_time,
        })
    }

    pub fn import_image(
        &self,
        bytes: &[u8],
        title: &str,
        mime: &str,
        extension: &str,
    ) -> Result<ResourceId, LibraryError> {
        let blob = self.resource_store.put(ResourceInput {
            bytes,
            title,
            mime,
            file_extension: extension,
        })?;
        let now = timestamp();
        let mut connection = self.connection.lock().expect("library mutex poisoned");
        let transaction = connection.transaction()?;
        transaction.execute("INSERT OR IGNORE INTO resource_blobs (sha256, size, mime, relative_path, created_time, revision) VALUES (?1, ?2, ?3, ?4, ?5, 1)", params![blob.sha256.as_str(), blob.size as i64, mime, format!("resources/blobs/{}", blob.sha256.as_str()), now])?;
        transaction.execute("INSERT INTO resources (id, sha256, title, mime, file_extension, size, created_time, updated_time, revision) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?7, 1)", params![blob.id.as_str(), blob.sha256.as_str(), title, mime, extension, blob.size as i64, now])?;
        enqueue_sync(
            &transaction,
            &EntityRef::Resource(blob.id.clone()),
            1,
            "create",
            now,
        )?;
        transaction.commit()?;
        self.publish(vec![LibraryEvent::SyncQueued(EntityRef::Resource(
            blob.id.clone(),
        ))]);
        Ok(blob.id)
    }

    pub fn outbox_count(&self) -> Result<i64, LibraryError> {
        let connection = self.connection.lock().expect("library mutex poisoned");
        connection
            .query_row("SELECT count(*) FROM sync_outbox", [], |row| row.get(0))
            .map_err(Into::into)
    }

    fn set_deleted(&self, id: &NoteId, deleted: bool) -> Result<(), LibraryError> {
        let now = timestamp();
        let mut connection = self.connection.lock().expect("library mutex poisoned");
        let transaction = connection.transaction()?;
        let revision: i64 = transaction
            .query_row(
                "SELECT revision FROM notes WHERE id = ?1",
                [id.as_str()],
                |row| row.get::<_, i64>(0),
            )
            .optional()?
            .ok_or(LibraryError::NotFound)?
            + 1;
        let count = if deleted {
            transaction.execute("UPDATE notes SET deleted_time = ?2, updated_time = ?2, revision = ?3 WHERE id = ?1 AND deleted_time = 0", params![id.as_str(), now, revision])?
        } else {
            transaction.execute("UPDATE notes SET deleted_time = 0, updated_time = ?2, revision = ?3 WHERE id = ?1 AND deleted_time <> 0", params![id.as_str(), now, revision])?
        };
        if count != 1 {
            return Err(LibraryError::NotFound);
        }
        enqueue_sync(
            &transaction,
            &EntityRef::Note(id.clone()),
            revision,
            if deleted { "trash" } else { "restore" },
            now,
        )?;
        transaction.commit()?;
        self.publish(vec![
            if deleted {
                LibraryEvent::NoteTrashed(id.clone())
            } else {
                LibraryEvent::NoteRestored(id.clone())
            },
            LibraryEvent::NoteProjectionChanged(id.clone()),
            LibraryEvent::SyncQueued(EntityRef::Note(id.clone())),
        ]);
        Ok(())
    }

    fn publish(&self, events: Vec<LibraryEvent>) {
        let mut subscribers = self.events.lock().expect("event mutex poisoned");
        for event in events {
            subscribers.retain(|sender| sender.send(event.clone()).is_ok());
        }
    }
}

fn checked_database_path(path: &Path) -> Result<PathBuf, LibraryError> {
    let parent = path.parent().ok_or(LibraryError::InvalidDatabasePath)?;
    let metadata = fs::symlink_metadata(parent).map_err(|_| LibraryError::InvalidDatabasePath)?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Err(LibraryError::InvalidDatabasePath);
    }
    if path.exists()
        && fs::symlink_metadata(path)
            .map_err(|_| LibraryError::InvalidDatabasePath)?
            .file_type()
            .is_symlink()
    {
        return Err(LibraryError::InvalidDatabasePath);
    }
    let name = path.file_name().ok_or(LibraryError::InvalidDatabasePath)?;
    Ok(fs::canonicalize(parent)
        .map_err(|_| LibraryError::InvalidDatabasePath)?
        .join(name))
}

fn new_id() -> String {
    let counter = NEXT_ID.fetch_add(1, Ordering::Relaxed);
    let now = timestamp();
    Sha256::digest(format!("{now}:{counter}"))
        .iter()
        .take(16)
        .map(|byte| format!("{byte:02x}"))
        .collect()
}
fn timestamp() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system clock before epoch")
        .as_millis() as i64
}
fn snippet(text: &str) -> String {
    text.chars().take(160).collect()
}

fn default_notebook_id(transaction: &Transaction<'_>) -> Result<NotebookId, LibraryError> {
    let id: String = transaction.query_row(
        "SELECT id FROM notebooks WHERE is_default = 1 AND deleted_time = 0 ORDER BY id LIMIT 1",
        [],
        |row| row.get(0),
    )?;
    NotebookId::parse(id).map_err(|_| LibraryError::InvalidId)
}
fn require_notebook(transaction: &Transaction<'_>, id: &NotebookId) -> Result<(), LibraryError> {
    require_entity(transaction, "notebooks", id.as_str())
}
fn require_stack(transaction: &Transaction<'_>, id: &StackId) -> Result<(), LibraryError> {
    require_entity(transaction, "stacks", id.as_str())
}
fn require_tag(transaction: &Transaction<'_>, id: &TagId) -> Result<(), LibraryError> {
    require_entity(transaction, "tags", id.as_str())
}
fn require_entity(
    transaction: &Transaction<'_>,
    table: &str,
    id: &str,
) -> Result<(), LibraryError> {
    let count: i64 = transaction.query_row(
        &format!("SELECT count(*) FROM {table} WHERE id = ?1 AND deleted_time = 0"),
        [id],
        |row| row.get(0),
    )?;
    if count == 1 {
        Ok(())
    } else {
        Err(LibraryError::NotFound)
    }
}

fn replace_note_resources(
    transaction: &Transaction<'_>,
    note_id: &NoteId,
    resource_ids: &[ResourceId],
) -> Result<(), LibraryError> {
    for id in resource_ids {
        let count: i64 = transaction.query_row(
            "SELECT count(*) FROM resources WHERE id = ?1 AND deleted_time = 0",
            [id.as_str()],
            |row| row.get(0),
        )?;
        if count != 1 {
            return Err(LibraryError::NotFound);
        }
    }
    transaction.execute(
        "DELETE FROM note_resources WHERE note_id = ?1",
        [note_id.as_str()],
    )?;
    for (position, id) in resource_ids.iter().enumerate() {
        transaction.execute(
            "INSERT INTO note_resources (note_id, resource_id, position) VALUES (?1, ?2, ?3)",
            params![note_id.as_str(), id.as_str(), position as i64],
        )?;
    }
    Ok(())
}
fn thumbnail_id(
    transaction: &Transaction<'_>,
    note_id: &NoteId,
) -> Result<Option<ResourceId>, LibraryError> {
    transaction.query_row("SELECT nr.resource_id FROM note_resources nr JOIN resources r ON r.id = nr.resource_id WHERE nr.note_id = ?1 AND nr.is_associated = 1 AND r.deleted_time = 0 AND r.mime IN ('image/png', 'image/jpeg') ORDER BY nr.position, nr.resource_id LIMIT 1", [note_id.as_str()], |row| row.get::<_, String>(0)).optional()?.map(|id| ResourceId::new(id).map_err(LibraryError::from)).transpose()
}
fn queue_search(
    transaction: &Transaction<'_>,
    note_id: &NoteId,
    now: i64,
    reason: &str,
) -> Result<(), LibraryError> {
    transaction.execute("INSERT INTO search_queue (note_id, updated_time, reason) VALUES (?1, ?2, ?3) ON CONFLICT(note_id) DO UPDATE SET updated_time = excluded.updated_time, reason = excluded.reason", params![note_id.as_str(), now, reason])?;
    Ok(())
}
fn enqueue_sync(
    transaction: &Transaction<'_>,
    entity: &EntityRef,
    revision: i64,
    operation: &str,
    now: i64,
) -> Result<(), LibraryError> {
    let (kind, id) = match entity {
        EntityRef::Note(id) => ("note", id.as_str()),
        EntityRef::Notebook(id) => ("notebook", id.as_str()),
        EntityRef::Stack(id) => ("stack", id.as_str()),
        EntityRef::Tag(id) => ("tag", id.as_str()),
        EntityRef::Resource(id) => ("resource", id.as_str()),
    };
    transaction.execute("INSERT INTO sync_outbox (id, entity_type, entity_id, entity_revision, operation, created_time) VALUES (?1, ?2, ?3, ?4, ?5, ?6)", params![new_id(), kind, id, revision, operation, now])?;
    Ok(())
}

fn row_to_notebook(row: &rusqlite::Row<'_>) -> rusqlite::Result<Notebook> {
    Ok(Notebook {
        id: NotebookId::parse(row.get::<_, String>(0)?).map_err(invalid_column)?,
        title: row.get(1)?,
        stack_id: row
            .get::<_, Option<String>>(2)?
            .map(|id| StackId::parse(id).map_err(invalid_column))
            .transpose()?,
        revision: row.get(3)?,
        is_default: row.get::<_, i64>(4)? != 0,
    })
}
fn row_to_note_base(row: &rusqlite::Row<'_>) -> rusqlite::Result<Note> {
    Ok(Note {
        id: NoteId::parse(row.get::<_, String>(0)?).map_err(invalid_column)?,
        title: row.get(1)?,
        body_html: row.get(2)?,
        body_text: row.get(3)?,
        snippet: row.get(4)?,
        notebook_id: NotebookId::parse(row.get::<_, String>(5)?).map_err(invalid_column)?,
        resource_ids: Vec::new(),
        tag_ids: Vec::new(),
        created_time: row.get(6)?,
        updated_time: row.get(7)?,
        deleted_time: positive_time(row.get(8)?),
        revision: row.get(9)?,
    })
}
fn row_to_projection(row: &rusqlite::Row<'_>) -> rusqlite::Result<NoteProjection> {
    Ok(NoteProjection {
        id: NoteId::parse(row.get::<_, String>(0)?).map_err(invalid_column)?,
        title_prefix: row.get(1)?,
        snippet: row.get(2)?,
        updated_time: row.get(3)?,
        deleted_time: positive_time(row.get(4)?),
        notebook_id: NotebookId::parse(row.get::<_, String>(5)?).map_err(invalid_column)?,
        selected_thumbnail_id: row
            .get::<_, Option<String>>(6)?
            .map(|id| ResourceId::new(id).map_err(invalid_column))
            .transpose()?,
        attachment_count: row.get(7)?,
    })
}
fn note_resource_ids(
    connection: &Connection,
    note_id: &NoteId,
) -> Result<Vec<ResourceId>, LibraryError> {
    let mut statement = connection.prepare("SELECT resource_id FROM note_resources WHERE note_id = ?1 AND is_associated = 1 ORDER BY position, resource_id")?;
    let rows = statement.query_map([note_id.as_str()], |row| {
        ResourceId::new(row.get::<_, String>(0)?).map_err(invalid_column)
    })?;
    Ok(rows.collect::<Result<Vec<_>, _>>()?)
}
fn note_tag_ids(connection: &Connection, note_id: &NoteId) -> Result<Vec<TagId>, LibraryError> {
    let mut statement = connection
        .prepare("SELECT tag_id FROM note_tags WHERE note_id = ?1 ORDER BY position, tag_id")?;
    let rows = statement.query_map([note_id.as_str()], |row| {
        TagId::parse(row.get::<_, String>(0)?).map_err(invalid_column)
    })?;
    Ok(rows.collect::<Result<Vec<_>, _>>()?)
}
fn positive_time(value: i64) -> Option<i64> {
    (value != 0).then_some(value)
}
fn invalid_column(_: impl std::fmt::Display) -> rusqlite::Error {
    rusqlite::Error::InvalidColumnType(0, "opaque ID".into(), rusqlite::types::Type::Text)
}
