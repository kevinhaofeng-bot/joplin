use crate::resource::{DatabaseFile, ProfileDir, ResourceError, ResourceInput, ResourceStore};
use crate::schema::migrate_schema;
use crate::{
    BlobHash, CreateNote, EditJournalEntry, EntityRef, ListQuery, Note, NoteId, NoteProjection,
    Notebook, NotebookId, ResourceId, SaveNote, SavedRevision, Stack, StackId, Tag, TagId,
};
use rusqlite::hooks::{AuthAction, Authorization};
use rusqlite::{
    Connection, OpenFlags, OptionalExtension, Transaction, TransactionBehavior, params,
};
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{Receiver, Sender, channel};
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};
use thiserror::Error;

const LIBRARY_SHELL_PANES_SETTING: &str = "library-shell.panes";
const LIBRARY_SHELL_SELECTED_NOTE_SETTING: &str = "library-shell.selected-note-id";

fn is_reserved_library_shell_setting(key: &str) -> bool {
    matches!(
        key,
        LIBRARY_SHELL_PANES_SETTING | LIBRARY_SHELL_SELECTED_NOTE_SETTING
    )
}

/// The durable, application-owned portion of the library window state.
///
/// Keeping this DTO in core prevents the GPUI shell from making unrelated
/// string writes for panes and selection.  The two fields are committed in one
/// SQLite transaction so an interrupted write cannot resurrect an old
/// selection alongside new pane settings.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LibraryShellState {
    pub sidebar_width: u16,
    pub list_width: u16,
    pub sidebar_visible: bool,
    pub list_visible: bool,
    pub selected_note_id: Option<NoteId>,
}

impl LibraryShellState {
    pub const DEFAULT_SIDEBAR_WIDTH: u16 = 220;
    pub const DEFAULT_LIST_WIDTH: u16 = 360;
    pub const MIN_PANE_WIDTH: u16 = 120;
    pub const MAX_PANE_WIDTH: u16 = 960;

    pub const fn new(sidebar_width: u16, list_width: u16) -> Self {
        Self {
            sidebar_width,
            list_width,
            sidebar_visible: true,
            list_visible: true,
            selected_note_id: None,
        }
    }

    /// Builds a writable shell state only when both persisted dimensions are
    /// within the product's real pane range.
    pub fn try_new(sidebar_width: u16, list_width: u16) -> Result<Self, LibraryError> {
        let state = Self::new(sidebar_width, list_width);
        state.validate()?;
        Ok(state)
    }

    pub fn validate(&self) -> Result<(), LibraryError> {
        if Self::pane_width_is_valid(self.sidebar_width)
            && Self::pane_width_is_valid(self.list_width)
        {
            Ok(())
        } else {
            Err(LibraryError::InvalidLibraryShellState)
        }
    }

    pub const fn pane_width_is_valid(width: u16) -> bool {
        width >= Self::MIN_PANE_WIDTH && width <= Self::MAX_PANE_WIDTH
    }

    pub const fn pane_state(&self) -> (u16, u16, bool, bool) {
        (
            self.sidebar_width,
            self.list_width,
            self.sidebar_visible,
            self.list_visible,
        )
    }

    fn panes_setting_value(&self) -> String {
        format!(
            "{},{},{},{}",
            self.sidebar_width,
            self.list_width,
            self.sidebar_visible as u8,
            self.list_visible as u8
        )
    }

    fn from_settings(panes: Option<&str>, selected_note_id: Option<&str>) -> Self {
        let mut state = panes
            .and_then(Self::parse_panes)
            .unwrap_or_else(Self::default);
        state.selected_note_id = selected_note_id.and_then(|id| NoteId::parse(id).ok());
        state
    }

    fn parse_panes(value: &str) -> Option<Self> {
        let fields = value.split(',').collect::<Vec<_>>();
        let [sidebar_width, list_width, sidebar_visible, list_visible] = fields.as_slice() else {
            return None;
        };
        let sidebar_width = sidebar_width.parse::<u16>().ok()?;
        let list_width = list_width.parse::<u16>().ok()?;
        if !Self::pane_width_is_valid(sidebar_width) || !Self::pane_width_is_valid(list_width) {
            return None;
        }
        let sidebar_visible = match *sidebar_visible {
            "0" => false,
            "1" => true,
            _ => return None,
        };
        let list_visible = match *list_visible {
            "0" => false,
            "1" => true,
            _ => return None,
        };
        Some(Self {
            sidebar_width,
            list_width,
            sidebar_visible,
            list_visible,
            selected_note_id: None,
        })
    }
}

impl Default for LibraryShellState {
    fn default() -> Self {
        Self::new(Self::DEFAULT_SIDEBAR_WIDTH, Self::DEFAULT_LIST_WIDTH)
    }
}

#[cfg(feature = "test-support")]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OpenTestPhase {
    BeforeProfileBind,
    AfterProfileBound,
    BeforeSqliteOpen,
    AfterSqliteOpen,
    AfterLegacyGate,
    BeforeMigrationCommit,
    AfterMigrationCommit,
}

#[cfg(feature = "test-support")]
pub type OpenTestHook = Arc<dyn Fn(OpenTestPhase) + Send + Sync>;

/// Supplies wall-clock milliseconds.  Kept at the repository boundary so a
/// transaction can protect monotonic note timestamps even if the wall clock is
/// adjusted backwards.
#[cfg(feature = "test-support")]
pub trait RepositoryClock: Send + Sync {
    fn now_millis(&self) -> i64;
}

#[cfg(not(feature = "test-support"))]
trait RepositoryClock: Send + Sync {
    fn now_millis(&self) -> i64;
}

/// Supplies opaque identifiers. Production uses the operating-system CSPRNG;
/// callers may supply a narrow per-repository source in deterministic tests.
#[cfg(feature = "test-support")]
pub trait RepositoryIdSource: Send + Sync {
    fn next_id(&self) -> Result<String, LibraryError>;
}

#[cfg(not(feature = "test-support"))]
trait RepositoryIdSource: Send + Sync {
    fn next_id(&self) -> Result<String, LibraryError>;
}

struct SystemClock;

impl RepositoryClock for SystemClock {
    fn now_millis(&self) -> i64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system clock before epoch")
            .as_millis() as i64
    }
}

struct SystemIdSource;

impl RepositoryIdSource for SystemIdSource {
    fn next_id(&self) -> Result<String, LibraryError> {
        let mut bytes = [0_u8; 16];
        getrandom::getrandom(&mut bytes).map_err(LibraryError::Entropy)?;
        Ok(bytes.iter().map(|byte| format!("{byte:02x}")).collect())
    }
}

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
    MigrationFailed(#[source] rusqlite::Error),
    #[error("library entropy source failed")]
    Entropy(#[source] getrandom::Error),
    #[error("SQLite pragmas could not be established")]
    Pragma,
    #[error("SQLite VFS cannot verify the live database-file identity")]
    DatabaseFileControlUnavailable(#[source] rusqlite::Error),
    #[error("SQLite VFS live database-file identity check failed")]
    DatabaseFileControl(#[source] rusqlite::Error),
    #[error("migration failed and journal mode could not be restored to {original_mode}")]
    JournalModeRestoreFailed {
        original_mode: String,
        migration: Box<LibraryError>,
        #[source]
        restore: rusqlite::Error,
    },
    #[error("schema version {0} is newer than this library supports")]
    UnsupportedSchema(i64),
    #[error("invalid opaque entity ID")]
    InvalidId,
    #[error("invalid note snapshot")]
    InvalidSnapshot,
    #[error("requested entity was not found")]
    NotFound,
    #[error("legacy RTF requires the dedicated Task 8 migration")]
    LegacyRtfMigrationRequired,
    #[error("stale note revision: expected {expected}, actual {actual}")]
    StaleRevision { expected: i64, actual: i64 },
    #[error("could not allocate a unique opaque entity ID")]
    IdCollisionExhausted,
    #[error("library shell state contains invalid pane dimensions")]
    InvalidLibraryShellState,
    #[error("generic settings access cannot address reserved library shell state")]
    ReservedLibraryShellSetting,
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
    #[allow(dead_code)]
    database_file: DatabaseFile,
    resource_store: ResourceStore,
    events: Mutex<Vec<Sender<LibraryEvent>>>,
    list_observers: Mutex<Vec<Sender<Vec<String>>>>,
    note_load_observers: Mutex<Vec<Sender<NoteId>>>,
    #[cfg(test)]
    shell_state_read_hook: Mutex<Option<Box<dyn FnOnce() + Send>>>,
    #[cfg(test)]
    next_note_load_failure: Mutex<Option<LibraryError>>,
    #[allow(dead_code)]
    database_path: PathBuf,
    clock: Arc<dyn RepositoryClock>,
    id_source: Arc<dyn RepositoryIdSource>,
}

impl LibraryRepository {
    pub fn open(path: impl AsRef<Path>) -> Result<Self, LibraryError> {
        Self::open_inner(
            path,
            Arc::new(SystemClock),
            Arc::new(SystemIdSource),
            #[cfg(feature = "test-support")]
            None,
        )
    }

    #[cfg(feature = "test-support")]
    #[doc(hidden)]
    pub fn open_with_sources(
        path: impl AsRef<Path>,
        clock: Arc<dyn RepositoryClock>,
        id_source: Arc<dyn RepositoryIdSource>,
    ) -> Result<Self, LibraryError> {
        Self::open_inner(path, clock, id_source, None)
    }

    #[cfg(feature = "test-support")]
    #[doc(hidden)]
    pub fn open_with_sources_and_hook(
        path: impl AsRef<Path>,
        clock: Arc<dyn RepositoryClock>,
        id_source: Arc<dyn RepositoryIdSource>,
        hook: OpenTestHook,
    ) -> Result<Self, LibraryError> {
        Self::open_inner(path, clock, id_source, Some(hook))
    }

    fn open_inner(
        path: impl AsRef<Path>,
        clock: Arc<dyn RepositoryClock>,
        id_source: Arc<dyn RepositoryIdSource>,
        #[cfg(feature = "test-support")] hook: Option<OpenTestHook>,
    ) -> Result<Self, LibraryError> {
        let requested_path = checked_database_path(path.as_ref())?;
        let name = requested_path
            .file_name()
            .ok_or(LibraryError::InvalidDatabasePath)?;
        #[cfg(feature = "test-support")]
        if let Some(hook) = &hook {
            hook(OpenTestPhase::BeforeProfileBind);
        }
        let profile = ProfileDir::open(
            requested_path
                .parent()
                .ok_or(LibraryError::InvalidDatabasePath)?,
        )
        .map_err(|_| LibraryError::InvalidDatabasePath)?;
        #[cfg(feature = "test-support")]
        if let Some(hook) = &hook {
            hook(OpenTestPhase::AfterProfileBound);
        }
        if !profile
            .verify_path_identity()
            .map_err(|_| LibraryError::InvalidDatabasePath)?
        {
            return Err(LibraryError::InvalidDatabasePath);
        }
        let database_file = profile
            .bind_database(name)
            .map_err(|_| LibraryError::InvalidDatabasePath)?;
        if !database_file
            .matches_profile_child(&profile)
            .map_err(|_| LibraryError::InvalidDatabasePath)?
        {
            return Err(LibraryError::InvalidDatabasePath);
        }
        let path = profile
            .database_path(name)
            .map_err(|_| LibraryError::InvalidDatabasePath)?;
        let flags = OpenFlags::SQLITE_OPEN_READ_WRITE
            | OpenFlags::SQLITE_OPEN_CREATE
            | OpenFlags::SQLITE_OPEN_NO_MUTEX
            | OpenFlags::SQLITE_OPEN_NOFOLLOW;
        #[cfg(feature = "test-support")]
        if let Some(hook) = &hook {
            hook(OpenTestPhase::BeforeSqliteOpen);
        }
        let mut connection = Connection::open_with_flags(&path, flags)?;
        #[cfg(feature = "test-support")]
        if let Some(hook) = &hook {
            hook(OpenTestPhase::AfterSqliteOpen);
        }
        if connection_has_moved(&connection)? {
            return Err(LibraryError::InvalidDatabasePath);
        }
        if !profile
            .verify_path_identity()
            .map_err(|_| LibraryError::InvalidDatabasePath)?
        {
            return Err(LibraryError::InvalidDatabasePath);
        }
        // SQLite necessarily receives a pathname. Compare both the child
        // reached through our parent descriptor and SQLite's reported main
        // filename to the inode bound before that pathname operation.
        if !database_file
            .matches_sqlite_connection(&connection, &profile)
            .map_err(|_| LibraryError::InvalidDatabasePath)?
        {
            return Err(LibraryError::InvalidDatabasePath);
        }
        // This is connection-local and precedes all schema/data writes.
        connection.pragma_update(None, "foreign_keys", "ON")?;
        // A legacy RTF profile is rejected before even a journal-mode change.
        // The later check inside `migrate_schema` remains authoritative while
        // BEGIN IMMEDIATE is held against a concurrent v3 writer.
        if has_legacy_rtf(&connection)? {
            return Err(LibraryError::LegacyRtfMigrationRequired);
        }
        let original_journal_mode = journal_mode(&connection)?;
        if let Err(error) = establish_wal(&connection) {
            return match restore_journal_mode(&connection, &profile, name, &original_journal_mode) {
                Ok(()) => Err(error),
                Err(restore) => Err(LibraryError::JournalModeRestoreFailed {
                    original_mode: original_journal_mode,
                    migration: Box::new(error),
                    restore,
                }),
            };
        }
        let mut next_migration_id = || id_source.next_id();
        let migration = migrate_schema(
            &mut connection,
            &mut next_migration_id,
            || {
                if !profile
                    .verify_path_identity()
                    .map_err(|_| LibraryError::InvalidDatabasePath)?
                {
                    return Err(LibraryError::InvalidDatabasePath);
                }
                ResourceStore::from_profile_dir(&profile).map_err(Into::into)
            },
            |transaction| {
                if connection_has_moved(transaction)?
                    || !profile
                        .verify_path_identity()
                        .map_err(|_| LibraryError::InvalidDatabasePath)?
                    || !database_file
                        .matches_sqlite_connection(transaction, &profile)
                        .map_err(|_| LibraryError::InvalidDatabasePath)?
                {
                    Err(LibraryError::InvalidDatabasePath)
                } else {
                    let journal_mode = journal_mode(transaction)?;
                    let foreign_keys: i64 =
                        transaction.query_row("PRAGMA foreign_keys", [], |row| row.get(0))?;
                    if journal_mode.to_ascii_lowercase() != "wal" || foreign_keys != 1 {
                        Err(LibraryError::Pragma)
                    } else {
                        Ok(())
                    }
                }
            },
            || {
                #[cfg(feature = "test-support")]
                if let Some(hook) = &hook {
                    hook(OpenTestPhase::AfterLegacyGate);
                }
            },
            || {
                #[cfg(feature = "test-support")]
                if let Some(hook) = &hook {
                    hook(OpenTestPhase::BeforeMigrationCommit);
                }
            },
            || {
                #[cfg(feature = "test-support")]
                if let Some(hook) = &hook {
                    hook(OpenTestPhase::AfterMigrationCommit);
                }
            },
        );
        let (_migrated, resource_store) = match migration {
            Ok(result) => result,
            Err(error) => {
                let error = match error {
                    LibraryError::Storage(error) => LibraryError::MigrationFailed(error),
                    other => other,
                };
                return match restore_journal_mode(
                    &connection,
                    &profile,
                    name,
                    &original_journal_mode,
                ) {
                    Ok(()) => Err(error),
                    Err(restore) => Err(LibraryError::JournalModeRestoreFailed {
                        original_mode: original_journal_mode,
                        migration: Box::new(error),
                        restore,
                    }),
                };
            }
        };
        // Every operation above this line can still return a normal open
        // failure.  Once schema commit succeeds, publication is only owned-fd
        // and in-memory state movement; do not lie that it failed afterward.
        Ok(Self {
            connection: Mutex::new(connection),
            database_file,
            resource_store,
            events: Mutex::new(Vec::new()),
            list_observers: Mutex::new(Vec::new()),
            note_load_observers: Mutex::new(Vec::new()),
            #[cfg(test)]
            shell_state_read_hook: Mutex::new(None),
            #[cfg(test)]
            next_note_load_failure: Mutex::new(None),
            database_path: path,
            clock,
            id_source,
        })
    }

    fn now(&self) -> i64 {
        self.clock.now_millis()
    }

    fn allocate_id(
        &self,
        transaction: &Transaction<'_>,
        table: &str,
    ) -> Result<String, LibraryError> {
        // `table` is always a private literal at its call sites, never user input.
        for _ in 0..16 {
            let id = self.id_source.next_id()?;
            if NoteId::parse(&id).is_err() {
                return Err(LibraryError::InvalidId);
            }
            let exists: i64 = if table == "notes" {
                transaction.query_row(
                    "SELECT EXISTS(SELECT 1 FROM notes WHERE id=?1 UNION ALL SELECT 1 FROM note_revisions WHERE note_id=?1 UNION ALL SELECT 1 FROM tombstones WHERE entity_type='note' AND entity_id=?1)",
                    [&id],
                    |row| row.get(0),
                )?
            } else {
                transaction.query_row(
                    &format!("SELECT EXISTS(SELECT 1 FROM {table} WHERE id=?1)"),
                    [&id],
                    |row| row.get(0),
                )?
            };
            if exists == 0 {
                return Ok(id);
            }
        }
        Err(LibraryError::IdCollisionExhausted)
    }

    /// The existence probe avoids ordinary collisions, but it is only an
    /// optimization: the insert is the authority.  Retrying the actual
    /// primary-key constraint closes the inter-repository race without ever
    /// exposing a partial transaction.
    fn insert_with_unique_id<T>(
        &self,
        transaction: &Transaction<'_>,
        table: &str,
        mut insert: impl FnMut(&str) -> Result<T, rusqlite::Error>,
    ) -> Result<(String, T), LibraryError> {
        for _ in 0..16 {
            let id = self.allocate_id(transaction, table)?;
            match insert(&id) {
                Ok(value) => return Ok((id, value)),
                Err(rusqlite::Error::SqliteFailure(error, _))
                    if error.code == rusqlite::ErrorCode::ConstraintViolation => {}
                Err(error) => return Err(error.into()),
            }
        }
        Err(LibraryError::IdCollisionExhausted)
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

    /// Records complete-note hydration independently from card-list queries.
    /// This lets the product shell prove that sorting, scrolling and projection
    /// refreshes stay cheap until the user explicitly selects a note.
    pub fn observe_note_loads(&self) -> Receiver<NoteId> {
        let (sender, receiver) = channel();
        self.note_load_observers
            .lock()
            .expect("note-load observer mutex poisoned")
            .push(sender);
        receiver
    }

    /// Records actual resource-blob reads separately from SQLite card and
    /// complete-note hydration. A Task 3 projection or read-only selection
    /// must not touch blob bytes before Task 5 owns image resources.
    pub fn observe_resource_reads(&self) -> Receiver<BlobHash> {
        self.resource_store.observe_reads()
    }

    #[cfg(test)]
    fn install_shell_state_interleave_hook_for_test(&self, hook: impl FnOnce() + Send + 'static) {
        *self
            .shell_state_read_hook
            .lock()
            .expect("shell-state read hook mutex poisoned") = Some(Box::new(hook));
    }

    #[cfg(test)]
    fn fail_next_note_load_for_test(&self, error: LibraryError) {
        *self
            .next_note_load_failure
            .lock()
            .expect("note-load failure mutex poisoned") = Some(error);
    }

    /// Reads the complete application-owned library shell state. Corrupt pane
    /// settings are all-or-default; a malformed note ID is simply no selection.
    pub fn read_library_shell_state(&self) -> Result<LibraryShellState, LibraryError> {
        let mut connection = self.connection.lock().expect("library mutex poisoned");
        // A deferred read transaction pins one SQLite snapshot after the first
        // SELECT. A concurrent writer can commit its next atomic generation,
        // but it cannot make the later selection read observe a different
        // generation from the panes read above.
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Deferred)?;
        let panes = transaction
            .query_row(
                "SELECT value FROM settings WHERE key = ?1",
                [LIBRARY_SHELL_PANES_SETTING],
                |row| row.get::<_, String>(0),
            )
            .optional()?;
        #[cfg(test)]
        if let Some(hook) = self
            .shell_state_read_hook
            .lock()
            .expect("shell-state read hook mutex poisoned")
            .take()
        {
            hook();
        }
        let selected_note_id = transaction
            .query_row(
                "SELECT value FROM settings WHERE key = ?1",
                [LIBRARY_SHELL_SELECTED_NOTE_SETTING],
                |row| row.get::<_, String>(0),
            )
            .optional()?;
        let state = LibraryShellState::from_settings(panes.as_deref(), selected_note_id.as_deref());
        transaction.commit()?;
        Ok(state)
    }

    /// Commits pane state and optional selection together. Passing `None`
    /// explicitly removes any stale selection rather than allowing it to be
    /// revived after a future restart.
    pub fn write_library_shell_state(&self, state: &LibraryShellState) -> Result<(), LibraryError> {
        state.validate()?;
        let now = self.now();
        let mut connection = self.connection.lock().expect("library mutex poisoned");
        let transaction = connection.transaction()?;
        transaction.execute(
            "INSERT INTO settings (key, value, updated_time) VALUES (?1, ?2, ?3)
             ON CONFLICT(key) DO UPDATE SET value = excluded.value, updated_time = excluded.updated_time",
            params![LIBRARY_SHELL_PANES_SETTING, state.panes_setting_value(), now],
        )?;
        if let Some(id) = &state.selected_note_id {
            transaction.execute(
                "INSERT INTO settings (key, value, updated_time) VALUES (?1, ?2, ?3)
                 ON CONFLICT(key) DO UPDATE SET value = excluded.value, updated_time = excluded.updated_time",
                params![LIBRARY_SHELL_SELECTED_NOTE_SETTING, id.as_str(), now],
            )?;
        } else {
            transaction.execute(
                "DELETE FROM settings WHERE key = ?1",
                [LIBRARY_SHELL_SELECTED_NOTE_SETTING],
            )?;
        }
        transaction.commit()?;
        Ok(())
    }

    /// Reads application-owned shell state without exposing SQLite to the UI.
    pub fn read_setting(&self, key: &str) -> Result<Option<String>, LibraryError> {
        if is_reserved_library_shell_setting(key) {
            return Err(LibraryError::ReservedLibraryShellSetting);
        }
        let connection = self.connection.lock().expect("library mutex poisoned");
        connection
            .query_row("SELECT value FROM settings WHERE key = ?1", [key], |row| {
                row.get(0)
            })
            .optional()
            .map_err(Into::into)
    }

    /// Persists application-owned shell state without exposing SQLite to the UI.
    pub fn write_setting(&self, key: &str, value: &str) -> Result<(), LibraryError> {
        if is_reserved_library_shell_setting(key) {
            return Err(LibraryError::ReservedLibraryShellSetting);
        }
        let now = self.now();
        let connection = self.connection.lock().expect("library mutex poisoned");
        connection.execute(
            "INSERT INTO settings (key, value, updated_time) VALUES (?1, ?2, ?3)
             ON CONFLICT(key) DO UPDATE SET value = excluded.value, updated_time = excluded.updated_time",
            params![key, value, now],
        )?;
        Ok(())
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
        let now = self.now();
        let title = input.title;
        let html = input.document.to_canonical_html().as_str().to_owned();
        let text = input.document.search_text().as_str().to_owned();
        let note_snippet = snippet(input.document.search_text().as_str());
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
        let (raw_id, _) = self.insert_with_unique_id(&transaction, "notes", |candidate| {
            transaction.execute(
                "INSERT INTO notes (id, title, body_html, body_text, snippet, notebook_id, selected_thumbnail_id, created_time, updated_time, revision)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, NULL, ?7, ?7, 1)",
                params![candidate, &title, &html, &text, &note_snippet, notebook_id.as_str(), now],
            )
        })?;
        let id = NoteId::parse(raw_id).expect("validated generated ID is valid");
        replace_note_resources(&transaction, &id, &resource_ids)?;
        let thumbnail = selected_thumbnail_id(&transaction, &id, None)?;
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
        enqueue_sync(
            &transaction,
            self.id_source.as_ref(),
            &EntityRef::Note(id.clone()),
            1,
            "create",
            now,
        )?;
        // All content and relationships are now known inside the same
        // transaction. Build the full return value before commit so the
        // committed outcome has no later fallible hydration step.
        let note = Note {
            id: id.clone(),
            title,
            body_html: html,
            body_text: text,
            snippet: note_snippet,
            notebook_id,
            resource_ids,
            tag_ids: Vec::new(),
            created_time: now,
            updated_time: now,
            deleted_time: None,
            revision: 1,
        };
        transaction.commit()?;
        drop(connection);
        self.publish(vec![
            LibraryEvent::NoteCreated(id.clone()),
            LibraryEvent::NoteProjectionChanged(id.clone()),
            LibraryEvent::SearchProjectionQueued(id.clone()),
            LibraryEvent::SyncQueued(EntityRef::Note(id.clone())),
        ]);
        Ok(note)
    }

    pub fn load_note(&self, id: &NoteId) -> Result<Option<Note>, LibraryError> {
        #[cfg(test)]
        if let Some(error) = self
            .next_note_load_failure
            .lock()
            .expect("note-load failure mutex poisoned")
            .take()
        {
            return Err(error);
        }
        let connection = self.connection.lock().expect("library mutex poisoned");
        let result = (|| {
            let base = connection
                .query_row(
                    "SELECT id, title, body_html, body_text, snippet, notebook_id, created_time, updated_time, deleted_time, revision
                     FROM notes WHERE id = ?1",
                    [id.as_str()],
                    row_to_note_base,
                )
                .optional()?;
            let Some(mut note) = base else {
                return Ok(None);
            };
            note.resource_ids = note_resource_ids(&connection, id)?;
            note.tag_ids = note_tag_ids(&connection, id)?;
            Ok(Some(note))
        })();
        drop(connection);
        self.note_load_observers
            .lock()
            .expect("note-load observer mutex poisoned")
            .retain(|observer| observer.send(id.clone()).is_ok());
        result
    }

    pub fn load_note_by_hex(&self, id: &str) -> Result<Option<Note>, LibraryError> {
        self.load_note(&NoteId::parse(id).map_err(|_| LibraryError::InvalidId)?)
    }

    pub fn save_note(&self, input: SaveNote) -> Result<Note, LibraryError> {
        self.save_note_inner(input, false)
    }

    fn save_note_inner(&self, input: SaveNote, clear_journal: bool) -> Result<Note, LibraryError> {
        let now = self.now();
        let html = input.document.to_canonical_html().as_str().to_owned();
        let text = input.document.search_text().as_str().to_owned();
        let note_snippet = snippet(&text);
        if input.document.resource_ids() != input.resource_ids {
            return Err(LibraryError::InvalidSnapshot);
        }
        let mut connection = self.connection.lock().expect("library mutex poisoned");
        let transaction = connection.transaction()?;
        let (notebook_id, current_revision, previous_updated, created_time): (String, i64, i64, i64) = transaction
            .query_row(
                "SELECT notebook_id, revision, updated_time, created_time FROM notes WHERE id = ?1 AND deleted_time = 0",
                [input.id.as_str()],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
            )
            .optional()?
            .ok_or(LibraryError::NotFound)?;
        if current_revision != input.expected_revision {
            return Err(LibraryError::StaleRevision {
                expected: input.expected_revision,
                actual: current_revision,
            });
        }
        let revision = current_revision + 1;
        let now = now.max(
            previous_updated
                .checked_add(1)
                .ok_or(LibraryError::InvalidSnapshot)?,
        );
        replace_note_resources(&transaction, &input.id, &input.resource_ids)?;
        let thumbnail = selected_thumbnail_id(
            &transaction,
            &input.id,
            input.selected_thumbnail_id.as_ref(),
        )?;
        transaction.execute(
            "UPDATE notes SET title = ?2, body_html = ?3, body_text = ?4, snippet = ?5,
             selected_thumbnail_id = ?6, updated_time = ?7, revision = ?8 WHERE id = ?1 AND revision = ?9",
            params![
                input.id.as_str(),
                &input.title,
                &html,
                &text,
                &note_snippet,
                thumbnail.as_ref().map(ResourceId::as_str),
                now,
                revision,
                input.expected_revision
            ],
        )?;
        if transaction.changes() != 1 {
            return Err(LibraryError::StaleRevision {
                expected: input.expected_revision,
                actual: current_revision + 1,
            });
        }
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
            self.id_source.as_ref(),
            &EntityRef::Note(input.id.clone()),
            revision,
            "save",
            now,
        )?;
        // All fields that a caller needs after a successful commit are
        // already available while this transaction is live. Construct the
        // durable outcome here rather than doing a second fallible hydration
        // after commit: a filesystem/SQLite fault after commit must not make
        // the UI claim that a completed save failed.
        let tag_ids = {
            let mut statement = transaction.prepare(
                "SELECT tag_id FROM note_tags WHERE note_id = ?1 ORDER BY position, tag_id",
            )?;
            let rows = statement.query_map([input.id.as_str()], |row| {
                TagId::parse(row.get::<_, String>(0)?).map_err(invalid_column)
            })?;
            rows.collect::<Result<Vec<_>, _>>()?
        };
        let note = Note {
            id: input.id.clone(),
            title: input.title,
            body_html: html,
            body_text: text,
            snippet: note_snippet,
            notebook_id: NotebookId::parse(notebook_id).map_err(invalid_column)?,
            resource_ids: input.resource_ids,
            tag_ids,
            created_time,
            updated_time: now,
            deleted_time: None,
            revision,
        };
        transaction.commit()?;
        drop(connection);
        self.publish(vec![
            LibraryEvent::NoteProjectionChanged(input.id.clone()),
            LibraryEvent::SearchProjectionQueued(input.id.clone()),
            LibraryEvent::SyncQueued(EntityRef::Note(input.id.clone())),
        ]);
        Ok(note)
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
                        COALESCE((SELECT n.selected_thumbnail_id WHERE EXISTS (SELECT 1 FROM note_resources snr JOIN resources sr ON sr.id = snr.resource_id WHERE snr.note_id=n.id AND snr.resource_id=n.selected_thumbnail_id AND snr.is_associated=1 AND sr.deleted_time=0 AND sr.mime IN ('image/png','image/jpeg'))),
                        (SELECT nr.resource_id FROM note_resources nr JOIN resources r ON r.id = nr.resource_id
                         WHERE nr.note_id = n.id AND nr.is_associated = 1 AND r.deleted_time = 0
                           AND r.mime IN ('image/png', 'image/jpeg') ORDER BY nr.position, nr.resource_id LIMIT 1)),
                        (SELECT count(*) FROM note_resources nr WHERE nr.note_id = n.id AND nr.is_associated = 1)
                 FROM notes n
                 WHERE (?1 = 2 OR (?1 = 0 AND n.deleted_time = 0) OR (?1 = 1 AND n.deleted_time <> 0))
                   AND (?2 IS NULL OR n.notebook_id = ?2)
                   AND (?3 IS NULL OR EXISTS (SELECT 1 FROM note_tags nt WHERE nt.note_id = n.id AND nt.tag_id = ?3))
                 ORDER BY n.updated_time DESC, n.id ASC
                 LIMIT COALESCE(?4, -1)",
            )?;
            let rows = statement.query_map(
                params![
                    match query.deletion_scope {
                        crate::DeletionScope::Active => 0_i64,
                        crate::DeletionScope::Trash => 1,
                        crate::DeletionScope::All => 2,
                    },
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
        let now = self.now();
        let mut connection = self.connection.lock().expect("library mutex poisoned");
        let transaction = connection.transaction()?;
        let (revision, deleted_time): (i64, i64) = transaction
            .query_row(
                "SELECT revision, deleted_time FROM notes WHERE id = ?1 AND deleted_time <> 0",
                [id.as_str()],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?
            .ok_or(LibraryError::NotFound)?;
        let final_revision = revision + 1;
        transaction.execute("INSERT INTO tombstones (entity_type, entity_id, final_revision, deleted_time, purged_time) VALUES ('note', ?1, ?2, ?3, ?4)", params![id.as_str(), final_revision, deleted_time, now])?;
        transaction.execute("DELETE FROM notes WHERE id = ?1", [id.as_str()])?;
        queue_search(&transaction, id, now, "purge")?;
        enqueue_sync(
            &transaction,
            self.id_source.as_ref(),
            &EntityRef::Note(id.clone()),
            final_revision,
            "purge",
            now,
        )?;
        transaction.commit()?;
        self.publish(vec![
            LibraryEvent::NoteProjectionChanged(id.clone()),
            LibraryEvent::SearchProjectionQueued(id.clone()),
            LibraryEvent::SyncQueued(EntityRef::Note(id.clone())),
        ]);
        Ok(())
    }

    pub fn create_notebook(
        &self,
        title: &str,
        stack_id: Option<&StackId>,
    ) -> Result<Notebook, LibraryError> {
        let now = self.now();
        let mut connection = self.connection.lock().expect("library mutex poisoned");
        let transaction = connection.transaction()?;
        if let Some(stack_id) = stack_id {
            require_stack(&transaction, stack_id)?;
        }
        let (raw_id, _) = self.insert_with_unique_id(&transaction, "notebooks", |candidate| {
            transaction.execute("INSERT INTO notebooks (id, title, stack_id, revision, created_time, updated_time) VALUES (?1, ?2, ?3, 1, ?4, ?4)", params![candidate, title, stack_id.map(StackId::as_str), now])
        })?;
        let id = NotebookId::parse(raw_id).expect("validated generated ID is valid");
        enqueue_sync(
            &transaction,
            self.id_source.as_ref(),
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
        let now = self.now();
        let mut connection = self.connection.lock().expect("library mutex poisoned");
        let transaction = connection.transaction()?;
        let (raw_id, _) = self.insert_with_unique_id(&transaction, "stacks", |candidate| {
            transaction.execute("INSERT INTO stacks (id, title, revision, created_time, updated_time) VALUES (?1, ?2, 1, ?3, ?3)", params![candidate, title, now])
        })?;
        let id = StackId::parse(raw_id).expect("validated generated ID is valid");
        enqueue_sync(
            &transaction,
            self.id_source.as_ref(),
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
        let now = self.now();
        let mut connection = self.connection.lock().expect("library mutex poisoned");
        let transaction = connection.transaction()?;
        let (raw_id, _) = self.insert_with_unique_id(&transaction, "tags", |candidate| {
            transaction.execute("INSERT INTO tags (id, title, revision, created_time, updated_time) VALUES (?1, ?2, 1, ?3, ?3)", params![candidate, title, now])
        })?;
        let id = TagId::parse(raw_id).expect("validated generated ID is valid");
        enqueue_sync(
            &transaction,
            self.id_source.as_ref(),
            &EntityRef::Tag(id.clone()),
            1,
            "create",
            now,
        )?;
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
        let now = self.now();
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
            let updated = next_note_time(&transaction, id, now)?;
            transaction.execute(
                "UPDATE notes SET notebook_id = ?2, updated_time = ?3, revision = ?4 WHERE id = ?1",
                params![id.as_str(), notebook_id.as_str(), updated, revision],
            )?;
            queue_search(&transaction, id, updated, "organization")?;
            enqueue_sync(
                &transaction,
                self.id_source.as_ref(),
                &EntityRef::Note(id.clone()),
                revision,
                "move",
                now,
            )?;
            events.push(LibraryEvent::NoteProjectionChanged(id.clone()));
            events.push(LibraryEvent::SearchProjectionQueued(id.clone()));
            events.push(LibraryEvent::SyncQueued(EntityRef::Note(id.clone())));
        }
        transaction.commit()?;
        self.publish(events);
        Ok(())
    }

    pub fn set_note_tags(&self, note_id: &NoteId, tag_ids: &[TagId]) -> Result<(), LibraryError> {
        let now = self.now();
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
        let updated = next_note_time(&transaction, note_id, now)?;
        transaction.execute(
            "UPDATE notes SET updated_time = ?2, revision = ?3 WHERE id = ?1",
            params![note_id.as_str(), updated, revision],
        )?;
        queue_search(&transaction, note_id, updated, "organization")?;
        enqueue_sync(
            &transaction,
            self.id_source.as_ref(),
            &EntityRef::Note(note_id.clone()),
            revision,
            "tags",
            now,
        )?;
        transaction.commit()?;
        self.publish(vec![
            LibraryEvent::OrganizationChanged,
            LibraryEvent::NoteProjectionChanged(note_id.clone()),
            LibraryEvent::SearchProjectionQueued(note_id.clone()),
            LibraryEvent::SyncQueued(EntityRef::Note(note_id.clone())),
        ]);
        Ok(())
    }

    pub fn append_edit_journal(&self, entry: EditJournalEntry) -> Result<(), LibraryError> {
        let now = self.now();
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
        self.insert_with_unique_id(&transaction, "edit_journal", |candidate| {
            transaction.execute("INSERT INTO edit_journal (id, note_id, generation, delta_utf8, created_time) VALUES (?1, ?2, ?3, ?4, ?5)", params![candidate, entry.note_id.as_str(), entry.generation, entry.delta_utf8, now])
        })?;
        transaction.commit()?;
        Ok(())
    }

    /// Returns the most recent durable edit-journal payload for one live note.
    /// The GPUI layer never opens SQLite itself: recovery is deliberately a
    /// core-owned read so a crashed editor can reconstruct its latest readable
    /// snapshot before the next settled flush compacts the journal.
    pub fn latest_edit_journal(
        &self,
        note_id: &NoteId,
    ) -> Result<Option<EditJournalEntry>, LibraryError> {
        let connection = self.connection.lock().expect("library mutex poisoned");
        connection
            .query_row(
                "SELECT note_id, generation, delta_utf8
                 FROM edit_journal
                 WHERE note_id = ?1
                 ORDER BY generation DESC, created_time DESC, id DESC
                 LIMIT 1",
                [note_id.as_str()],
                |row| {
                    Ok(EditJournalEntry {
                        note_id: NoteId::parse(row.get::<_, String>(0)?).map_err(invalid_column)?,
                        generation: row.get(1)?,
                        delta_utf8: row.get(2)?,
                    })
                },
            )
            .optional()
            .map_err(Into::into)
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
        let now = self.now();
        let mut connection = self.connection.lock().expect("library mutex poisoned");
        let transaction = connection.transaction()?;
        // Blob persistence is content-addressed and may safely precede this
        // transaction; the entity ID itself is allocated against SQLite so an
        // astronomically unlikely CSPRNG collision is retried before exposure.
        transaction.execute("INSERT OR IGNORE INTO resource_blobs (sha256, size, mime, relative_path, created_time, revision) VALUES (?1, ?2, ?3, ?4, ?5, 1)", params![blob.sha256.as_str(), blob.size as i64, mime, format!("resources/blobs/{}", blob.sha256.as_str()), now])?;
        let (raw_id, _) = self.insert_with_unique_id(&transaction, "resources", |candidate| {
            transaction.execute("INSERT INTO resources (id, sha256, title, mime, file_extension, size, created_time, updated_time, revision) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?7, 1)", params![candidate, blob.sha256.as_str(), title, mime, extension, blob.size as i64, now])
        })?;
        let resource_id = ResourceId::new(raw_id).expect("validated generated ID is valid");
        enqueue_sync(
            &transaction,
            self.id_source.as_ref(),
            &EntityRef::Resource(resource_id.clone()),
            1,
            "create",
            now,
        )?;
        transaction.commit()?;
        self.publish(vec![LibraryEvent::SyncQueued(EntityRef::Resource(
            resource_id.clone(),
        ))]);
        Ok(resource_id)
    }

    pub fn associate_resource(
        &self,
        input: crate::AssociateResource,
    ) -> Result<Note, LibraryError> {
        self.save_note(input.snapshot)
    }

    pub fn resource_metadata(
        &self,
        id: &ResourceId,
    ) -> Result<Option<crate::StoredResource>, LibraryError> {
        let connection = self.connection.lock().expect("library mutex poisoned");
        connection.query_row("SELECT id, sha256, title, mime, file_extension, size, revision FROM resources WHERE id=?1", [id.as_str()], |row| {
            Ok(crate::StoredResource { id: ResourceId::new(row.get::<_, String>(0)?).map_err(invalid_column)?, sha256: crate::BlobHash::new(row.get::<_, String>(1)?).map_err(invalid_column)?, title: row.get(2)?, mime: row.get(3)?, file_extension: row.get(4)?, size: row.get(5)?, revision: row.get(6)? })
        }).optional().map_err(Into::into)
    }

    pub fn rollback_unassociated_resource(&self, id: &ResourceId) -> Result<(), LibraryError> {
        let mut connection = self.connection.lock().expect("library mutex poisoned");
        let transaction = connection.transaction()?;
        let hash: Option<String> = transaction
            .query_row(
                "SELECT sha256 FROM resources WHERE id=?1",
                [id.as_str()],
                |row| row.get(0),
            )
            .optional()?;
        let deleted = transaction.execute("DELETE FROM resources WHERE id=?1 AND NOT EXISTS(SELECT 1 FROM note_resources WHERE resource_id=?1)", [id.as_str()])?;
        if deleted == 1 {
            transaction.execute(
                "DELETE FROM sync_outbox WHERE entity_type='resource' AND entity_id=?1",
                [id.as_str()],
            )?;
            if let Some(hash) = hash {
                // Blob bytes are intentionally left for safe content-addressed
                // GC, but database metadata must not survive as an orphan.
                transaction.execute(
                    "DELETE FROM resource_blobs WHERE sha256=?1 AND NOT EXISTS(SELECT 1 FROM resources WHERE sha256=?1)",
                    [hash],
                )?;
            }
        }
        transaction.commit()?;
        Ok(())
    }

    pub fn outbox_count(&self) -> Result<i64, LibraryError> {
        let connection = self.connection.lock().expect("library mutex poisoned");
        connection
            .query_row("SELECT count(*) FROM sync_outbox", [], |row| row.get(0))
            .map_err(Into::into)
    }

    /// Takes immutable search work for the indexer.  Work is acknowledged by
    /// its full queue identity so a newer upsert cannot be accidentally lost.
    pub fn take_search_jobs(&self, limit: usize) -> Result<Vec<crate::SearchJob>, LibraryError> {
        let connection = self.connection.lock().expect("library mutex poisoned");
        let mut statement = connection.prepare(
            "SELECT note_id, updated_time, reason FROM search_queue ORDER BY updated_time, note_id LIMIT ?1",
        )?;
        statement
            .query_map([limit as i64], |row| {
                Ok(crate::SearchJob {
                    note_id: NoteId::parse(row.get::<_, String>(0)?).map_err(invalid_column)?,
                    updated_time: row.get(1)?,
                    reason: row.get(2)?,
                })
            })?
            .collect::<Result<Vec<_>, _>>()
            .map_err(Into::into)
    }

    pub fn ack_search_jobs(&self, jobs: &[crate::SearchJob]) -> Result<(), LibraryError> {
        let mut connection = self.connection.lock().expect("library mutex poisoned");
        let transaction = connection.transaction()?;
        for job in jobs {
            transaction.execute(
                "DELETE FROM search_queue WHERE note_id=?1 AND updated_time=?2 AND reason=?3",
                params![job.note_id.as_str(), job.updated_time, job.reason],
            )?;
        }
        transaction.commit()?;
        Ok(())
    }

    fn set_deleted(&self, id: &NoteId, deleted: bool) -> Result<(), LibraryError> {
        let now = self.now();
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
        let updated = next_note_time(&transaction, id, now)?;
        let count = if deleted {
            transaction.execute("UPDATE notes SET deleted_time = ?2, updated_time = ?3, revision = ?4 WHERE id = ?1 AND deleted_time = 0", params![id.as_str(), now, updated, revision])?
        } else {
            let active_notebook: i64 = transaction.query_row("SELECT EXISTS(SELECT 1 FROM notes n JOIN notebooks b ON b.id=n.notebook_id WHERE n.id=?1 AND b.deleted_time=0)", [id.as_str()], |row| row.get(0))?;
            let notebook = if active_notebook == 1 {
                None
            } else {
                Some(default_notebook_id(&transaction)?)
            };
            transaction.execute("UPDATE notes SET deleted_time = 0, notebook_id = COALESCE(?2, notebook_id), updated_time = ?3, revision = ?4 WHERE id = ?1 AND deleted_time <> 0", params![id.as_str(), notebook.as_ref().map(NotebookId::as_str), updated, revision])?
        };
        if count != 1 {
            return Err(LibraryError::NotFound);
        }
        enqueue_sync(
            &transaction,
            self.id_source.as_ref(),
            &EntityRef::Note(id.clone()),
            revision,
            if deleted { "trash" } else { "restore" },
            updated,
        )?;
        queue_search(
            &transaction,
            id,
            updated,
            if deleted { "trash" } else { "restore" },
        )?;
        transaction.commit()?;
        self.publish(vec![
            if deleted {
                LibraryEvent::NoteTrashed(id.clone())
            } else {
                LibraryEvent::NoteRestored(id.clone())
            },
            LibraryEvent::NoteProjectionChanged(id.clone()),
            LibraryEvent::SearchProjectionQueued(id.clone()),
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
    if parent.as_os_str().is_empty() || path.file_name().is_none() {
        return Err(LibraryError::InvalidDatabasePath);
    }
    // Do not stat/canonicalize here: ProfileDir binds this lexical parent by
    // descriptor before metadata is consulted.
    Ok(path.to_owned())
}

fn has_legacy_rtf(connection: &Connection) -> Result<bool, LibraryError> {
    let version: i64 = connection.query_row("PRAGMA user_version", [], |row| row.get(0))?;
    if version != 3 {
        return Ok(false);
    }
    let has_markup: i64 = connection.query_row(
        "SELECT EXISTS(SELECT 1 FROM pragma_table_info('notes') WHERE name='markup_language')",
        [],
        |row| row.get(0),
    )?;
    if has_markup == 0 {
        return Ok(false);
    }
    Ok(connection.query_row(
        "SELECT EXISTS(SELECT 1 FROM notes WHERE markup_language=1)",
        [],
        |row| row.get::<_, i64>(0),
    )? != 0)
}

fn journal_mode(connection: &Connection) -> Result<String, LibraryError> {
    connection
        .query_row("PRAGMA journal_mode", [], |row| row.get::<_, String>(0))
        .map(|mode| mode.to_ascii_lowercase())
        .map_err(Into::into)
}

fn connection_has_moved(connection: &Connection) -> Result<bool, LibraryError> {
    const MAIN_DATABASE: &[u8] = b"main\0";
    let mut moved: std::os::raw::c_int = 0;
    // `SQLITE_FCNTL_HAS_MOVED` is the public VFS contract for the actual
    // opened file handle.  It closes the pathname/inode ABA which cannot be
    // observed by restatting the currently selected directory entry.
    let result = unsafe {
        rusqlite::ffi::sqlite3_file_control(
            connection.handle(),
            MAIN_DATABASE.as_ptr().cast(),
            rusqlite::ffi::SQLITE_FCNTL_HAS_MOVED,
            (&mut moved as *mut std::os::raw::c_int).cast(),
        )
    };
    match result {
        rusqlite::ffi::SQLITE_OK => Ok(moved != 0),
        rusqlite::ffi::SQLITE_NOTFOUND => Err(LibraryError::DatabaseFileControlUnavailable(
            file_control_error(result, "SQLITE_FCNTL_HAS_MOVED is unavailable for main"),
        )),
        _ => Err(LibraryError::DatabaseFileControl(file_control_error(
            result,
            "SQLITE_FCNTL_HAS_MOVED failed for main",
        ))),
    }
}

fn file_control_error(result: std::os::raw::c_int, context: &str) -> rusqlite::Error {
    rusqlite::Error::SqliteFailure(rusqlite::ffi::Error::new(result), Some(context.to_owned()))
}

fn establish_wal(connection: &Connection) -> Result<(), LibraryError> {
    let mode: String = connection.query_row("PRAGMA journal_mode=WAL", [], |row| row.get(0))?;
    if mode.eq_ignore_ascii_case("wal") && journal_mode(connection)?.eq_ignore_ascii_case("wal") {
        Ok(())
    } else {
        Err(LibraryError::Pragma)
    }
}

fn restore_journal_mode(
    connection: &Connection,
    profile: &ProfileDir,
    name: &std::ffi::OsStr,
    original: &str,
) -> Result<(), rusqlite::Error> {
    match restore_journal_mode_on_connection(connection, original) {
        Ok(()) => Ok(()),
        Err(first) => {
            // A rename can make SQLite's original lexical WAL sidecar path
            // read-only. Re-open the same already-bound directory by its fd's
            // live path, not the replacement profile pathname, to restore the
            // prior v3 journal mode before surfacing InvalidDatabasePath.
            let path = profile.current_database_path(name).map_err(|_| first)?;
            let recovery = Connection::open_with_flags(
                path,
                OpenFlags::SQLITE_OPEN_READ_WRITE
                    | OpenFlags::SQLITE_OPEN_NO_MUTEX
                    | OpenFlags::SQLITE_OPEN_NOFOLLOW,
            )?;
            restore_journal_mode_on_connection(&recovery, original)
        }
    }
}

fn restore_journal_mode_on_connection(
    connection: &Connection,
    original: &str,
) -> Result<(), rusqlite::Error> {
    let mode = match original.to_ascii_lowercase().as_str() {
        "delete" => "DELETE",
        "truncate" => "TRUNCATE",
        "persist" => "PERSIST",
        "memory" => "MEMORY",
        "wal" => "WAL",
        "off" => "OFF",
        _ => return Err(rusqlite::Error::InvalidQuery),
    };
    let returned: String =
        connection.query_row(&format!("PRAGMA journal_mode={mode}"), [], |row| row.get(0))?;
    if returned.eq_ignore_ascii_case(mode) {
        Ok(())
    } else {
        Err(rusqlite::Error::InvalidQuery)
    }
}

fn snippet(text: &str) -> String {
    text.chars().take(160).collect()
}
fn next_note_time(
    transaction: &Transaction<'_>,
    id: &NoteId,
    now: i64,
) -> Result<i64, LibraryError> {
    let previous: i64 = transaction.query_row(
        "SELECT updated_time FROM notes WHERE id=?1",
        [id.as_str()],
        |row| row.get(0),
    )?;
    Ok(now.max(
        previous
            .checked_add(1)
            .ok_or(LibraryError::InvalidSnapshot)?,
    ))
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
fn selected_thumbnail_id(
    transaction: &Transaction<'_>,
    note_id: &NoteId,
    requested: Option<&ResourceId>,
) -> Result<Option<ResourceId>, LibraryError> {
    if let Some(requested) = requested {
        let valid: i64 = transaction.query_row("SELECT EXISTS(SELECT 1 FROM note_resources nr JOIN resources r ON r.id=nr.resource_id WHERE nr.note_id=?1 AND nr.resource_id=?2 AND nr.is_associated=1 AND r.deleted_time=0 AND r.mime IN ('image/png','image/jpeg'))", params![note_id.as_str(), requested.as_str()], |row| row.get(0))?;
        if valid == 1 {
            return Ok(Some(requested.clone()));
        }
    }
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
    id_source: &dyn RepositoryIdSource,
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
    for _ in 0..16 {
        let operation_id = id_source.next_id()?;
        if NoteId::parse(&operation_id).is_err() {
            return Err(LibraryError::InvalidId);
        }
        match transaction.execute("INSERT INTO sync_outbox (id, entity_type, entity_id, entity_revision, operation, created_time) VALUES (?1, ?2, ?3, ?4, ?5, ?6)", params![operation_id, kind, id, revision, operation, now]) {
            Ok(_) => return Ok(()),
            Err(rusqlite::Error::SqliteFailure(error, _)) if error.code == rusqlite::ErrorCode::ConstraintViolation => continue,
            Err(error) => return Err(error.into()),
        }
    }
    Err(LibraryError::IdCollisionExhausted)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn shell_state_read_keeps_one_snapshot_when_a_second_connection_commits_between_fields() {
        // Catches two independent SELECTs combining old panes with a new
        // selection. The hook runs after the first field read, while the
        // reader's SQLite snapshot must remain pinned.
        let profile = tempdir().expect("temporary profile");
        let database = profile.path().join("library.sqlite");
        let reader = LibraryRepository::open(&database).expect("open reader");
        let writer = LibraryRepository::open(&database).expect("open writer");
        let old = LibraryShellState {
            sidebar_width: 260,
            list_width: 410,
            sidebar_visible: true,
            list_visible: false,
            selected_note_id: Some(NoteId::parse("11111111111111111111111111111111").unwrap()),
        };
        let new = LibraryShellState {
            sidebar_width: 300,
            list_width: 480,
            sidebar_visible: false,
            list_visible: true,
            selected_note_id: Some(NoteId::parse("22222222222222222222222222222222").unwrap()),
        };
        reader
            .write_library_shell_state(&old)
            .expect("write initial generation");
        let expected_new = new.clone();
        reader.install_shell_state_interleave_hook_for_test(move || {
            writer
                .write_library_shell_state(&new)
                .expect("writer commits a complete next generation");
        });

        assert_eq!(
            reader
                .read_library_shell_state()
                .expect("read pinned snapshot"),
            old,
            "one reader call must not combine different durable generations"
        );
        assert_eq!(
            reader
                .read_library_shell_state()
                .expect("read next snapshot"),
            expected_new
        );
    }

    #[test]
    fn create_note_does_not_consume_a_post_commit_complete_note_load_fault() {
        // Catches create_note committing/publishing successfully and then
        // calling load_note. The armed real load fault must remain pending
        // until this test explicitly invokes it after create returns.
        let profile = tempdir().expect("temporary profile");
        let repository = LibraryRepository::open(profile.path().join("library.sqlite"))
            .expect("open repository");
        let events = repository.subscribe();
        repository.fail_next_note_load_for_test(LibraryError::NotFound);

        let created = repository
            .create_note(CreateNote {
                title: "commit snapshot".into(),
                notebook_id: None,
                document: crate::CanonicalDocument::default(),
            })
            .expect("a post-commit load fault must not turn a committed create into Err");
        assert_eq!(
            events.recv().expect("create publishes its committed event"),
            LibraryEvent::NoteCreated(created.id.clone())
        );
        assert!(matches!(
            repository.load_note(&created.id),
            Err(LibraryError::NotFound)
        ));
        assert_eq!(
            repository
                .load_note(&created.id)
                .expect("fault consumed by this explicit load"),
            Some(created),
            "the committed record remains durable after the deliberately later load fault"
        );
    }
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
