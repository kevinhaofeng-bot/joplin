use crate::resource::{
    DatabaseFile, ProfileDir, ResourceBlob, ResourceError, ResourceInput, ResourceStore,
};
use crate::schema::{DERIVED_TEXT_EXTRACTOR_VERSION, migrate_schema};
use crate::{
    BlobHash, CanonicalDocument, CreateNote, DerivedTextFailure, DerivedTextJob, DerivedTextStatus,
    EditJournalEntry, EntityRef, JournalOwnership, LibraryNavigationIndex, ListQuery,
    ListQueryError, Note, NoteId, NoteOrganizationState, NoteProjection, Notebook, NotebookId,
    ResourceId, SaveNote, SavedRevision, SearchFilter, SearchHit, SearchQuery, SearchQueryError,
    SearchTerm, Stack, StackId, Tag, TagId, compile_note_list_query,
};
use rusqlite::hooks::{AuthAction, Authorization};
use rusqlite::{
    Connection, OpenFlags, OptionalExtension, Transaction, TransactionBehavior, params,
    params_from_iter,
};
use std::collections::{BTreeSet, HashMap};
use std::fs::File;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;
use std::sync::mpsc::{Receiver, Sender, channel};
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};
use thiserror::Error;

const LIBRARY_SHELL_PANES_SETTING: &str = "library-shell.panes";
const LIBRARY_SHELL_SELECTED_NOTE_SETTING: &str = "library-shell.selected-note-id";
const MAX_DERIVED_TEXT_BYTES: usize = 1024 * 1024;

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

#[cfg(any(test, feature = "test-support"))]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[doc(hidden)]
pub enum SearchIndexTestPhase {
    TransactionStarted,
    TransactionCommitted,
}

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
    #[error("edit journal is owned by another active writer")]
    JournalOwnershipConflict,
    #[error("legacy edit journal payload cannot be recovered safely")]
    InvalidLegacyEditJournal,
    #[error("could not allocate a unique opaque entity ID")]
    IdCollisionExhausted,
    #[error("library shell state contains invalid pane dimensions")]
    InvalidLibraryShellState,
    #[error("generic settings access cannot address reserved library shell state")]
    ReservedLibraryShellSetting,
    #[error("organization title must contain visible text and fit within 255 characters")]
    InvalidOrganizationTitle,
    #[error("select a child notebook before creating a note in this notebook group")]
    StackNoteContainerRequired,
    #[error("the default notebook cannot be deleted")]
    DefaultNotebookCannotBeDeleted,
    #[error("invalid note-list query")]
    ListQuery(#[from] ListQueryError),
    #[error("invalid search query")]
    SearchQuery(#[from] SearchQueryError),
    #[error("derived attachment text exceeds its bounded storage limit")]
    DerivedTextTooLarge,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LibraryEvent {
    NoteCreated(NoteId),
    NoteProjectionChanged(NoteId),
    NoteTrashed(NoteId),
    NoteRestored(NoteId),
    OrganizationChanged,
    SearchProjectionQueued(NoteId),
    DerivedTextIndexed(ResourceId),
    SyncQueued(EntityRef),
}

/// A descriptor-safe blob that has been written to the content-addressed
/// store but deliberately has no SQLite metadata or observable library event
/// yet.  Task 5 uses this narrow staging state to prepare a native document
/// before one transaction atomically makes the resource and its note
/// relationship visible.  An abandoned stage is harmless content-addressed
/// garbage and is never a user-visible orphan.
#[derive(Debug)]
pub struct StagedResource {
    resource_id: ResourceId,
    blob: ResourceBlob,
    title: String,
    mime: String,
    file_extension: String,
}

impl StagedResource {
    pub fn resource_id(&self) -> &ResourceId {
        &self.resource_id
    }

    pub fn sha256(&self) -> &BlobHash {
        &self.blob.sha256
    }

    pub fn size(&self) -> usize {
        self.blob.size
    }

    /// Metadata was validated together with the descriptor/bytes before this
    /// invisible stage was created. The native document needs these exact
    /// durable values while it prepares the single later snapshot transaction.
    pub fn title(&self) -> &str {
        &self.title
    }

    pub fn mime(&self) -> &str {
        &self.mime
    }
}

/// The complete post-commit fact for a staged resource insertion.  `Note`
/// intentionally does not duplicate projection-only fields, so returning the
/// selected thumbnail explicitly prevents the GPUI shell from guessing from a
/// stale pre-commit card projection in the same presentation cycle.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StagedResourceSnapshotCommit {
    pub note: Note,
    pub selected_thumbnail_id: Option<ResourceId>,
}

#[derive(Debug, Clone)]
struct PurgeResourceCandidate {
    id: ResourceId,
    sha256: BlobHash,
    revision: i64,
}

pub struct LibraryRepository {
    connection: Mutex<Connection>,
    /// A lazily opened, independently verified SQLite handle used only by the
    /// derived search worker.  Foreground reads and saves retain the primary
    /// handle, so a bounded FTS write never also holds its Rust mutex.
    index_connection: Mutex<Option<Connection>>,
    profile_dir: ProfileDir,
    #[allow(dead_code)]
    database_file: DatabaseFile,
    resource_store: ResourceStore,
    events: Mutex<Vec<Sender<LibraryEvent>>>,
    list_observers: Mutex<Vec<Sender<Vec<String>>>>,
    #[cfg(any(test, feature = "test-support"))]
    search_observers: Mutex<Vec<Sender<Vec<String>>>>,
    #[cfg(any(test, feature = "test-support"))]
    navigation_index_observers: Mutex<Vec<Sender<Vec<String>>>>,
    #[cfg(any(test, feature = "test-support"))]
    organization_state_observers: Mutex<Vec<Sender<Vec<String>>>>,
    note_load_observers: Mutex<Vec<Sender<NoteId>>>,
    #[cfg(test)]
    shell_state_read_hook: Mutex<Option<Box<dyn FnOnce() + Send>>>,
    #[cfg(any(test, feature = "test-support"))]
    next_note_load_failure: Mutex<Option<LibraryError>>,
    /// Test seam for the independent derived-index worker. It never affects
    /// the authoritative save transaction and is consumed once per worker
    /// invocation so retry/reopen behavior remains observable.
    next_search_job_failure: Mutex<Option<LibraryError>>,
    #[cfg(any(test, feature = "test-support"))]
    search_index_transaction_hook: Mutex<Option<Arc<dyn Fn(SearchIndexTestPhase) + Send + Sync>>>,
    #[cfg(test)]
    next_staged_resource_snapshot_failure: Mutex<Option<LibraryError>>,
    database_name: std::ffi::OsString,
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
        let repository = Self {
            connection: Mutex::new(connection),
            index_connection: Mutex::new(None),
            profile_dir: profile,
            database_file,
            resource_store,
            events: Mutex::new(Vec::new()),
            list_observers: Mutex::new(Vec::new()),
            #[cfg(any(test, feature = "test-support"))]
            search_observers: Mutex::new(Vec::new()),
            #[cfg(any(test, feature = "test-support"))]
            navigation_index_observers: Mutex::new(Vec::new()),
            #[cfg(any(test, feature = "test-support"))]
            organization_state_observers: Mutex::new(Vec::new()),
            note_load_observers: Mutex::new(Vec::new()),
            #[cfg(test)]
            shell_state_read_hook: Mutex::new(None),
            #[cfg(any(test, feature = "test-support"))]
            next_note_load_failure: Mutex::new(None),
            next_search_job_failure: Mutex::new(None),
            #[cfg(any(test, feature = "test-support"))]
            search_index_transaction_hook: Mutex::new(None),
            #[cfg(test)]
            next_staged_resource_snapshot_failure: Mutex::new(None),
            database_name: name.to_owned(),
            clock,
            id_source,
        };
        // A physical unlink is necessarily outside SQLite, so a crash or a
        // transient filesystem refusal leaves this durable queue for a later
        // open. It must never make an already-published database unusable.
        let _ = repository.drain_resource_gc();
        Ok(repository)
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
            } else if table == "resources" {
                transaction.query_row(
                    "SELECT EXISTS(SELECT 1 FROM resources WHERE id=?1 UNION ALL SELECT 1 FROM tombstones WHERE entity_type='resource' AND entity_id=?1)",
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

    /// Select the opaque ID that a future resource transaction will publish.
    /// Blob storage is intentionally content-addressed only: generating an
    /// entity ID below that boundary would bypass this repository's injected
    /// allocator and its deterministic collision probes.
    ///
    /// This commits no reservation row. A separate connection can still win
    /// the same ID before a staged snapshot commits; that later transaction
    /// then fails as one atomic unit, without events or outbox publication.
    /// Reserving that interval would require a schema-level lease, which the
    /// Task 2 durable-entity baseline does not have.
    fn allocate_resource_id(&self) -> Result<ResourceId, LibraryError> {
        let mut connection = self.connection.lock().expect("library mutex poisoned");
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let raw_id = self.allocate_id(&transaction, "resources")?;
        transaction.commit()?;
        ResourceId::new(raw_id).map_err(Into::into)
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

    #[cfg(any(test, feature = "test-support"))]
    #[doc(hidden)]
    pub fn observe_next_search_query(&self) -> Receiver<Vec<String>> {
        let (sender, receiver) = channel();
        self.search_observers
            .lock()
            .expect("search observer mutex poisoned")
            .push(sender);
        receiver
    }

    /// Reports actual SQLite `Read` actions for the next navigation-index
    /// query. It is deliberately feature-gated so production navigation has
    /// no observer lock or authorizer bookkeeping, while test-support builds
    /// can reject future body/blob prefetches even when their result is thrown
    /// away before reaching the UI.
    #[cfg(any(test, feature = "test-support"))]
    #[doc(hidden)]
    pub fn observe_next_navigation_index_query(&self) -> Receiver<Vec<String>> {
        let (sender, receiver) = channel();
        self.navigation_index_observers
            .lock()
            .expect("navigation-index observer mutex poisoned")
            .push(sender);
        receiver
    }

    /// Reports physical SQLite reads made while reconciling an already
    /// mounted selected note after a typed organization mutation. This stays
    /// feature-gated with the other authorizer probes: production pays no
    /// observer lock or authorizer installation cost.
    #[cfg(any(test, feature = "test-support"))]
    #[doc(hidden)]
    pub fn observe_next_note_organization_state_query(&self) -> Receiver<Vec<String>> {
        let (sender, receiver) = channel();
        self.organization_state_observers
            .lock()
            .expect("organization-state observer mutex poisoned")
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
    #[cfg(any(test, feature = "test-support"))]
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

    #[cfg(any(test, feature = "test-support"))]
    #[doc(hidden)]
    pub fn fail_next_note_load_for_test(&self, error: LibraryError) {
        *self
            .next_note_load_failure
            .lock()
            .expect("note-load failure mutex poisoned") = Some(error);
    }

    #[cfg(any(test, feature = "test-support"))]
    #[doc(hidden)]
    pub fn fail_next_search_jobs_for_test(&self, error: LibraryError) {
        *self
            .next_search_job_failure
            .lock()
            .expect("search-job failure mutex poisoned") = Some(error);
    }

    pub(crate) fn take_search_job_failure(&self) -> Option<LibraryError> {
        self.next_search_job_failure
            .lock()
            .expect("search-job failure mutex poisoned")
            .take()
    }

    #[cfg(any(test, feature = "test-support"))]
    #[doc(hidden)]
    pub fn set_search_index_transaction_hook_for_test(
        &self,
        hook: Arc<dyn Fn(SearchIndexTestPhase) + Send + Sync>,
    ) {
        *self
            .search_index_transaction_hook
            .lock()
            .expect("search-index transaction hook mutex poisoned") = Some(hook);
    }

    #[cfg(any(test, feature = "test-support"))]
    pub(crate) fn notify_search_index_transaction_for_test(&self, phase: SearchIndexTestPhase) {
        let hook = self
            .search_index_transaction_hook
            .lock()
            .expect("search-index transaction hook mutex poisoned")
            .clone();
        if let Some(hook) = hook {
            hook(phase);
        }
    }

    #[cfg(test)]
    fn fail_next_staged_resource_snapshot_for_test(&self, error: LibraryError) {
        *self
            .next_staged_resource_snapshot_failure
            .lock()
            .expect("staged resource snapshot failure mutex poisoned") = Some(error);
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

    /// Returns only the durable organization metadata needed to render a
    /// typed library sidebar. This has no note-card authority: note rows stay
    /// exclusively behind `list_notes`, and this query intentionally never
    /// touches bodies, snippets, thumbnails, or resource blobs.
    pub fn list_navigation_index(&self) -> Result<LibraryNavigationIndex, LibraryError> {
        #[cfg(any(test, feature = "test-support"))]
        let observers = std::mem::take(
            &mut *self
                .navigation_index_observers
                .lock()
                .expect("navigation-index observer mutex poisoned"),
        );
        #[cfg(any(test, feature = "test-support"))]
        let columns = Arc::new(Mutex::new(BTreeSet::new()));
        let mut connection = self.connection.lock().expect("library mutex poisoned");
        #[cfg(any(test, feature = "test-support"))]
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
                        .expect("navigation-index column observer mutex poisoned")
                        .insert(format!("{table_name}.{column_name}"));
                }
                Authorization::Allow
            }));
        }
        let result = (|| -> Result<LibraryNavigationIndex, LibraryError> {
            let transaction =
                connection.transaction_with_behavior(TransactionBehavior::Deferred)?;
            let stacks = {
                let mut statement = transaction.prepare(
                    "SELECT id, title, revision
                     FROM stacks
                     WHERE deleted_time = 0
                     ORDER BY title COLLATE NOCASE ASC, id ASC",
                )?;
                let rows = statement.query_map([], row_to_stack)?;
                rows.collect::<Result<Vec<_>, _>>()?
            };
            let notebooks = {
                let mut statement = transaction.prepare(
                    "SELECT id, title, stack_id, revision, is_default
                     FROM notebooks
                     WHERE deleted_time = 0
                     ORDER BY CASE WHEN stack_id IS NULL THEN 1 ELSE 0 END,
                              title COLLATE NOCASE ASC,
                              id ASC",
                )?;
                let rows = statement.query_map([], row_to_notebook)?;
                rows.collect::<Result<Vec<_>, _>>()?
            };
            let tags = {
                let mut statement = transaction.prepare(
                    "SELECT id, title, revision
                     FROM tags
                     WHERE deleted_time = 0
                     ORDER BY title COLLATE NOCASE ASC, id ASC",
                )?;
                let rows = statement.query_map([], row_to_tag)?;
                rows.collect::<Result<Vec<_>, _>>()?
            };
            transaction.commit()?;
            Ok(LibraryNavigationIndex {
                notebooks,
                stacks,
                tags,
            })
        })();
        #[cfg(any(test, feature = "test-support"))]
        connection.authorizer(None::<fn(rusqlite::hooks::AuthContext<'_>) -> Authorization>);
        #[cfg(any(test, feature = "test-support"))]
        if !observers.is_empty() {
            let fields = columns
                .lock()
                .expect("navigation-index column observer mutex poisoned")
                .iter()
                .cloned()
                .collect::<Vec<_>>();
            for observer in observers {
                let _ = observer.send(fields.clone());
            }
        }
        result
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
        queue_derived_text_for_note(&transaction, &id, now)?;
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
        #[cfg(any(test, feature = "test-support"))]
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
        self.save_note_inner(input, false, None, None)
            .map(|commit| commit.note)
    }

    fn save_note_inner(
        &self,
        input: SaveNote,
        clear_journal: bool,
        journal_ownership: Option<&JournalOwnership>,
        staged_resource: Option<&StagedResource>,
    ) -> Result<StagedResourceSnapshotCommit, LibraryError> {
        let now = self.now();
        let html = input.document.to_canonical_html().as_str().to_owned();
        let text = input.document.search_text().as_str().to_owned();
        let note_snippet = snippet(&text);
        if input.document.resource_ids() != input.resource_ids {
            return Err(LibraryError::InvalidSnapshot);
        }
        if let Some(staged) = staged_resource {
            // A staged blob is not a general metadata import.  Its exact
            // opaque ID must be referenced by the document that this one
            // transaction commits, otherwise a failed/aborted editor action
            // could expose an unattached resource row.
            if !input.resource_ids.contains(staged.resource_id()) {
                return Err(LibraryError::InvalidSnapshot);
            }
        }
        let mut connection = self.connection.lock().expect("library mutex poisoned");
        // Snapshot compaction must serialize with journal ownership transfer
        // and replacement. A deferred transaction could read a lease, then
        // let a foreign writer claim/replace it before the note update.
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
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
        if clear_journal {
            let current_journal: Option<(i64, String, i64)> = transaction
                .query_row(
                    "SELECT expected_revision, writer_token, sequence
                     FROM edit_journal
                     WHERE note_id = ?1 AND expected_revision = ?2
                     ORDER BY sequence DESC
                     LIMIT 1",
                    params![input.id.as_str(), input.expected_revision],
                    |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
                )
                .optional()?;
            let owns_current_journal = match (journal_ownership, current_journal) {
                (None, None) => true,
                (Some(owner), Some((journal_revision, writer_token, sequence))) => {
                    journal_revision == input.expected_revision
                        && writer_token == owner.writer_token
                        && sequence == owner.sequence
                        && !owner.writer_token.is_empty()
                        && owner.sequence > 0
                }
                _ => false,
            };
            if !owns_current_journal {
                // Do this before any note/resource mutation. Returning from
                // this IMMEDIATE transaction leaves both the durable note and
                // the current owner's checkpoint untouched.
                return Err(LibraryError::JournalOwnershipConflict);
            }
        }
        let revision = current_revision + 1;
        let now = now.max(
            previous_updated
                .checked_add(1)
                .ok_or(LibraryError::InvalidSnapshot)?,
        );
        if let Some(staged) = staged_resource {
            // `stage_resource` deliberately has no SQLite publication. Hold
            // this IMMEDIATE writer transaction while re-opening its
            // descriptor-safe bytes: if queued physical GC already won, this
            // returns before resource metadata, note relations, outbox work,
            // or events can become visible. If this writer won, GC must see
            // the metadata committed below before it can unlink the hash.
            self.resource_store.verify_staged_blob(&staged.blob)?;
            self.insert_staged_resource_metadata(&transaction, staged, now)?;
            #[cfg(test)]
            if let Some(error) = self
                .next_staged_resource_snapshot_failure
                .lock()
                .expect("staged resource snapshot failure mutex poisoned")
                .take()
            {
                // Returning while `transaction` is live is the important
                // fault boundary: resource_blobs, resources, note_resources,
                // search work and both outbox entries all roll back together,
                // and publication is still below `commit`.
                return Err(error);
            }
        }
        replace_note_resources(&transaction, &input.id, &input.resource_ids)?;
        queue_derived_text_for_note(&transaction, &input.id, now)?;
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
            // The exact-current lease was verified above while this IMMEDIATE
            // transaction was held. Once it owns the newest current-base row,
            // compact it together with any safely obsolete lower-base rows
            // (and legacy same-base predecessors) for this note. Never touch
            // a hypothetical future-base row.
            let deleted = transaction.execute(
                "DELETE FROM edit_journal
                 WHERE note_id = ?1 AND expected_revision <= ?2",
                params![input.id.as_str(), input.expected_revision],
            )?;
            if journal_ownership.is_some() && deleted == 0 {
                return Err(LibraryError::JournalOwnershipConflict);
            }
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
        let mut events = vec![
            LibraryEvent::NoteProjectionChanged(input.id.clone()),
            LibraryEvent::SearchProjectionQueued(input.id.clone()),
        ];
        if let Some(staged) = staged_resource {
            events.push(LibraryEvent::SyncQueued(EntityRef::Resource(
                staged.resource_id().clone(),
            )));
        }
        events.push(LibraryEvent::SyncQueued(EntityRef::Note(input.id.clone())));
        self.publish(events);
        Ok(StagedResourceSnapshotCommit {
            note,
            selected_thumbnail_id: thumbnail,
        })
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
            let compiled = compile_note_list_query(&query)?;
            let mut statement = connection.prepare(&compiled.sql)?;
            let rows =
                statement.query_map(params_from_iter(compiled.params.iter()), row_to_projection)?;
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

    /// Reads only the metadata an already-mounted note session needs after a
    /// move/tag/delete mutation. This is deliberately not `load_note`: the
    /// organization UI must never hydrate canonical bodies or resources just
    /// to repaint route membership.
    pub fn note_organization_state(
        &self,
        note_id: &NoteId,
    ) -> Result<Option<NoteOrganizationState>, LibraryError> {
        #[cfg(any(test, feature = "test-support"))]
        let observers = std::mem::take(
            &mut *self
                .organization_state_observers
                .lock()
                .expect("organization-state observer mutex poisoned"),
        );
        #[cfg(any(test, feature = "test-support"))]
        let columns = Arc::new(Mutex::new(BTreeSet::new()));
        let connection = self.connection.lock().expect("library mutex poisoned");
        #[cfg(any(test, feature = "test-support"))]
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
                        .expect("organization-state column observer mutex poisoned")
                        .insert(format!("{table_name}.{column_name}"));
                }
                Authorization::Allow
            }));
        }
        let result = (|| -> Result<Option<NoteOrganizationState>, LibraryError> {
            let row: Option<(String, i64, i64, i64)> = connection
                .query_row(
                    "SELECT notebook_id, updated_time, deleted_time, revision FROM notes WHERE id = ?1",
                    [note_id.as_str()],
                    |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
                )
                .optional()?;
            let Some((notebook_id, updated_time, deleted_time, revision)) = row else {
                return Ok(None);
            };
            let mut statement = connection.prepare(
                "SELECT tag_id FROM note_tags WHERE note_id = ?1 ORDER BY position, tag_id",
            )?;
            let tag_ids = statement
                .query_map([note_id.as_str()], |row| {
                    TagId::parse(row.get::<_, String>(0)?).map_err(invalid_column)
                })?
                .collect::<Result<Vec<_>, _>>()?;
            Ok(Some(NoteOrganizationState {
                id: note_id.clone(),
                notebook_id: NotebookId::parse(notebook_id).map_err(|_| LibraryError::InvalidId)?,
                tag_ids,
                updated_time,
                deleted_time: (deleted_time != 0).then_some(deleted_time),
                revision,
            }))
        })();
        #[cfg(any(test, feature = "test-support"))]
        connection.authorizer(None::<fn(rusqlite::hooks::AuthContext<'_>) -> Authorization>);
        #[cfg(any(test, feature = "test-support"))]
        if !observers.is_empty() {
            let fields = columns
                .lock()
                .expect("organization-state column observer mutex poisoned")
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
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let (revision, deleted_time): (i64, i64) = transaction
            .query_row(
                "SELECT revision, deleted_time FROM notes WHERE id = ?1 AND deleted_time <> 0",
                [id.as_str()],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?
            .ok_or(LibraryError::NotFound)?;
        let final_revision = revision
            .checked_add(1)
            .ok_or(LibraryError::InvalidSnapshot)?;
        let resource_candidates = purge_resource_candidates(&transaction, id)?;
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
        let reclaimed_resources =
            self.reclaim_unreferenced_purge_resources(&transaction, resource_candidates, now)?;
        transaction.commit()?;
        drop(connection);
        let mut events = vec![
            LibraryEvent::NoteProjectionChanged(id.clone()),
            LibraryEvent::SearchProjectionQueued(id.clone()),
            LibraryEvent::SyncQueued(EntityRef::Note(id.clone())),
        ];
        events.extend(
            reclaimed_resources
                .into_iter()
                .map(|id| LibraryEvent::SyncQueued(EntityRef::Resource(id))),
        );
        self.publish(events);
        // The queue is durable before publication. A failed unlink leaves it
        // for the next repository open/retry rather than rolling back a note
        // tombstone or allowing a live resource relation to disappear.
        let _ = self.drain_resource_gc();
        Ok(())
    }

    fn reclaim_unreferenced_purge_resources(
        &self,
        transaction: &Transaction<'_>,
        candidates: Vec<PurgeResourceCandidate>,
        now: i64,
    ) -> Result<Vec<ResourceId>, LibraryError> {
        let mut reclaimed = Vec::new();
        for candidate in candidates {
            let deleted = transaction.execute(
                "DELETE FROM resources
                 WHERE id = ?1
                   AND NOT EXISTS(SELECT 1 FROM note_resources WHERE resource_id = ?1)",
                [candidate.id.as_str()],
            )?;
            if deleted == 0 {
                continue;
            }
            let final_revision = candidate
                .revision
                .checked_add(1)
                .ok_or(LibraryError::InvalidSnapshot)?;
            transaction.execute(
                "DELETE FROM sync_outbox WHERE entity_type = 'resource' AND entity_id = ?1",
                [candidate.id.as_str()],
            )?;
            transaction.execute(
                "INSERT INTO tombstones (entity_type, entity_id, final_revision, deleted_time, purged_time)
                 VALUES ('resource', ?1, ?2, ?3, ?3)",
                params![candidate.id.as_str(), final_revision, now],
            )?;
            enqueue_sync(
                transaction,
                self.id_source.as_ref(),
                &EntityRef::Resource(candidate.id.clone()),
                final_revision,
                "purge",
                now,
            )?;
            let blob_is_unreferenced: i64 = transaction.query_row(
                "SELECT NOT EXISTS(SELECT 1 FROM resources WHERE sha256 = ?1)",
                [candidate.sha256.as_str()],
                |row| row.get(0),
            )?;
            if blob_is_unreferenced != 0 {
                transaction.execute(
                    "DELETE FROM resource_blobs
                     WHERE sha256 = ?1
                       AND NOT EXISTS(SELECT 1 FROM resources WHERE sha256 = ?1)",
                    [candidate.sha256.as_str()],
                )?;
                transaction.execute(
                    "INSERT OR IGNORE INTO resource_gc_queue (sha256, created_time) VALUES (?1, ?2)",
                    params![candidate.sha256.as_str(), now],
                )?;
            }
            reclaimed.push(candidate.id);
        }
        Ok(reclaimed)
    }

    fn drain_resource_gc(&self) -> Result<(), LibraryError> {
        loop {
            let mut connection = self.connection.lock().expect("library mutex poisoned");
            let transaction =
                connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
            let queued: Option<String> = transaction
                .query_row(
                    "SELECT sha256 FROM resource_gc_queue ORDER BY created_time, sha256 LIMIT 1",
                    [],
                    |row| row.get(0),
                )
                .optional()?;
            let Some(queued) = queued else {
                transaction.commit()?;
                return Ok(());
            };
            let sha256 = BlobHash::new(&queued).map_err(|_| LibraryError::InvalidSnapshot)?;
            let metadata_reappeared: i64 = transaction.query_row(
                "SELECT EXISTS(SELECT 1 FROM resource_blobs WHERE sha256 = ?1)",
                [sha256.as_str()],
                |row| row.get(0),
            )?;
            if metadata_reappeared != 0 {
                transaction.execute(
                    "DELETE FROM resource_gc_queue WHERE sha256 = ?1",
                    [sha256.as_str()],
                )?;
                transaction.commit()?;
                continue;
            }
            // Keep this IMMEDIATE transaction until the descriptor-relative
            // unlink and queue acknowledgement have both occurred. A second
            // repository cannot publish a resource row for this hash midway.
            self.resource_store.remove_blob(&sha256)?;
            transaction.execute(
                "DELETE FROM resource_gc_queue WHERE sha256 = ?1",
                [sha256.as_str()],
            )?;
            transaction.commit()?;
        }
    }

    pub fn create_notebook(
        &self,
        title: &str,
        stack_id: Option<&StackId>,
    ) -> Result<Notebook, LibraryError> {
        let title = organization_title(title)?;
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
            title,
            stack_id: stack_id.cloned(),
            revision: 1,
            is_default: false,
        })
    }

    pub fn create_stack(&self, title: &str) -> Result<Stack, LibraryError> {
        let title = organization_title(title)?;
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
            title,
            revision: 1,
        })
    }

    pub fn create_tag(&self, title: &str) -> Result<Tag, LibraryError> {
        let title = organization_title(title)?;
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
            title,
            revision: 1,
        })
    }

    /// Renames a stack in place. Sidebar routes, history entries and nested
    /// notebooks retain their durable StackId; a title update is never a
    /// delete-and-recreate operation.
    pub fn rename_stack(&self, id: &StackId, title: &str) -> Result<Stack, LibraryError> {
        let title = organization_title(title)?;
        let now = self.now();
        let mut connection = self.connection.lock().expect("library mutex poisoned");
        let transaction = connection.transaction()?;
        let revision = next_organization_revision(&transaction, "stacks", id.as_str())?;
        let updated_time = next_organization_time(&transaction, "stacks", id.as_str(), now)?;
        let changed = transaction.execute(
            "UPDATE stacks SET title = ?2, revision = ?3, updated_time = ?4
             WHERE id = ?1 AND deleted_time = 0",
            params![id.as_str(), title, revision, updated_time],
        )?;
        if changed != 1 {
            return Err(LibraryError::NotFound);
        }
        enqueue_sync(
            &transaction,
            self.id_source.as_ref(),
            &EntityRef::Stack(id.clone()),
            revision,
            "rename",
            updated_time,
        )?;
        transaction.commit()?;
        self.publish(vec![
            LibraryEvent::OrganizationChanged,
            LibraryEvent::SyncQueued(EntityRef::Stack(id.clone())),
        ]);
        Ok(Stack {
            id: id.clone(),
            title,
            revision,
        })
    }

    /// Renames a notebook without changing its StackId or note membership.
    pub fn rename_notebook(&self, id: &NotebookId, title: &str) -> Result<Notebook, LibraryError> {
        let title = organization_title(title)?;
        let now = self.now();
        let mut connection = self.connection.lock().expect("library mutex poisoned");
        let transaction = connection.transaction()?;
        let (stack_id, is_default): (Option<String>, i64) = transaction
            .query_row(
                "SELECT stack_id, is_default FROM notebooks WHERE id = ?1 AND deleted_time = 0",
                [id.as_str()],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?
            .ok_or(LibraryError::NotFound)?;
        let revision = next_organization_revision(&transaction, "notebooks", id.as_str())?;
        let updated_time = next_organization_time(&transaction, "notebooks", id.as_str(), now)?;
        transaction.execute(
            "UPDATE notebooks SET title = ?2, revision = ?3, updated_time = ?4 WHERE id = ?1",
            params![id.as_str(), title, revision, updated_time],
        )?;
        enqueue_sync(
            &transaction,
            self.id_source.as_ref(),
            &EntityRef::Notebook(id.clone()),
            revision,
            "rename",
            updated_time,
        )?;
        transaction.commit()?;
        self.publish(vec![
            LibraryEvent::OrganizationChanged,
            LibraryEvent::SyncQueued(EntityRef::Notebook(id.clone())),
        ]);
        Ok(Notebook {
            id: id.clone(),
            title,
            stack_id: stack_id
                .map(StackId::parse)
                .transpose()
                .map_err(|_| LibraryError::InvalidId)?,
            revision,
            is_default: is_default != 0,
        })
    }

    /// Renames a tag in place, retaining its stable TagId and all active note
    /// relationships.
    pub fn rename_tag(&self, id: &TagId, title: &str) -> Result<Tag, LibraryError> {
        let title = organization_title(title)?;
        let now = self.now();
        let mut connection = self.connection.lock().expect("library mutex poisoned");
        let transaction = connection.transaction()?;
        let revision = next_organization_revision(&transaction, "tags", id.as_str())?;
        let updated_time = next_organization_time(&transaction, "tags", id.as_str(), now)?;
        let changed = transaction.execute(
            "UPDATE tags SET title = ?2, revision = ?3, updated_time = ?4
             WHERE id = ?1 AND deleted_time = 0",
            params![id.as_str(), title, revision, updated_time],
        )?;
        if changed != 1 {
            return Err(LibraryError::NotFound);
        }
        enqueue_sync(
            &transaction,
            self.id_source.as_ref(),
            &EntityRef::Tag(id.clone()),
            revision,
            "rename",
            updated_time,
        )?;
        transaction.commit()?;
        self.publish(vec![
            LibraryEvent::OrganizationChanged,
            LibraryEvent::SyncQueued(EntityRef::Tag(id.clone())),
        ]);
        Ok(Tag {
            id: id.clone(),
            title,
            revision,
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

    /// The single-note form used by the C1 shell. It deliberately delegates
    /// to the same transaction/event path as future multi-select work rather
    /// than creating a second move authority.
    pub fn move_selected_note(
        &self,
        note_id: &NoteId,
        notebook_id: &NotebookId,
    ) -> Result<(), LibraryError> {
        self.move_notes(std::slice::from_ref(note_id), notebook_id)
    }

    pub fn set_note_tags(&self, note_id: &NoteId, tag_ids: &[TagId]) -> Result<(), LibraryError> {
        let tag_ids = normalized_tag_ids(tag_ids)?;
        let now = self.now();
        let mut connection = self.connection.lock().expect("library mutex poisoned");
        let transaction = connection.transaction()?;
        replace_note_tags_in_transaction(
            &transaction,
            self.id_source.as_ref(),
            note_id,
            &tag_ids,
            now,
            "tags",
        )?;
        transaction.commit()?;
        self.publish(note_tag_events(note_id));
        Ok(())
    }

    /// Adds one tag without hydrating the note body. Existing membership is
    /// idempotent, matching the donor's typed AddTag action semantics.
    pub fn add_note_tag(&self, note_id: &NoteId, tag_id: &TagId) -> Result<(), LibraryError> {
        let now = self.now();
        let mut connection = self.connection.lock().expect("library mutex poisoned");
        let transaction = connection.transaction()?;
        require_tag(&transaction, tag_id)?;
        let mut tag_ids = active_note_tag_ids(&transaction, note_id)?;
        if tag_ids.contains(tag_id) {
            transaction.commit()?;
            return Ok(());
        }
        tag_ids.push(tag_id.clone());
        replace_note_tags_in_transaction(
            &transaction,
            self.id_source.as_ref(),
            note_id,
            &tag_ids,
            now,
            "tags",
        )?;
        transaction.commit()?;
        self.publish(note_tag_events(note_id));
        Ok(())
    }

    /// Removes exactly one active relation. It does not silently edit a
    /// deleted note or a non-existent tag, so the UI can display a truthful
    /// failure without publishing a partial navigation refresh.
    pub fn remove_note_tag(&self, note_id: &NoteId, tag_id: &TagId) -> Result<(), LibraryError> {
        let now = self.now();
        let mut connection = self.connection.lock().expect("library mutex poisoned");
        let transaction = connection.transaction()?;
        require_tag(&transaction, tag_id)?;
        let mut tag_ids = active_note_tag_ids(&transaction, note_id)?;
        let Some(position) = tag_ids.iter().position(|candidate| candidate == tag_id) else {
            return Err(LibraryError::NotFound);
        };
        tag_ids.remove(position);
        replace_note_tags_in_transaction(
            &transaction,
            self.id_source.as_ref(),
            note_id,
            &tag_ids,
            now,
            "tags",
        )?;
        transaction.commit()?;
        self.publish(note_tag_events(note_id));
        Ok(())
    }

    /// Retires a non-default notebook in one transaction and moves every
    /// surviving durable note relationship to the default notebook. Notes are
    /// never left pointing at a route that the navigation index no longer
    /// exposes, including notes currently in Trash that may later be restored.
    pub fn delete_notebook(&self, id: &NotebookId) -> Result<(), LibraryError> {
        let now = self.now();
        let mut connection = self.connection.lock().expect("library mutex poisoned");
        let transaction = connection.transaction()?;
        let (is_default, revision): (i64, i64) = transaction
            .query_row(
                "SELECT is_default, revision FROM notebooks WHERE id = ?1 AND deleted_time = 0",
                [id.as_str()],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?
            .ok_or(LibraryError::NotFound)?;
        if is_default != 0 {
            return Err(LibraryError::DefaultNotebookCannotBeDeleted);
        }
        let fallback = default_notebook_id(&transaction)?;
        let note_ids = note_ids_for_notebook(&transaction, id)?;
        let mut events = vec![LibraryEvent::OrganizationChanged];
        for note_id in note_ids {
            let note_revision = next_note_revision_any(&transaction, &note_id)?;
            let updated = next_note_time(&transaction, &note_id, now)?;
            transaction.execute(
                "UPDATE notes SET notebook_id = ?2, updated_time = ?3, revision = ?4 WHERE id = ?1",
                params![note_id.as_str(), fallback.as_str(), updated, note_revision],
            )?;
            queue_search(&transaction, &note_id, updated, "organization")?;
            enqueue_sync(
                &transaction,
                self.id_source.as_ref(),
                &EntityRef::Note(note_id.clone()),
                note_revision,
                "move",
                updated,
            )?;
            events.push(LibraryEvent::NoteProjectionChanged(note_id.clone()));
            events.push(LibraryEvent::SearchProjectionQueued(note_id.clone()));
            events.push(LibraryEvent::SyncQueued(EntityRef::Note(note_id)));
        }
        let updated_time = next_organization_time(&transaction, "notebooks", id.as_str(), now)?;
        let deleted_revision = revision
            .checked_add(1)
            .ok_or(LibraryError::InvalidSnapshot)?;
        transaction.execute(
            "UPDATE notebooks SET deleted_time = ?2, updated_time = ?2, revision = ?3 WHERE id = ?1",
            params![id.as_str(), updated_time, deleted_revision],
        )?;
        enqueue_sync(
            &transaction,
            self.id_source.as_ref(),
            &EntityRef::Notebook(id.clone()),
            deleted_revision,
            "delete",
            updated_time,
        )?;
        transaction.commit()?;
        events.push(LibraryEvent::SyncQueued(EntityRef::Notebook(id.clone())));
        self.publish(events);
        Ok(())
    }

    /// Disbands a Stack without deleting its child notebooks or their notes.
    ///
    /// Evernote models this as a typed `DESTROY_STACK` operation rather than
    /// an expunge of each contained notebook. We retain that distinction in
    /// the local durable model: every child notebook becomes floating in the
    /// same transaction, including an already-soft-deleted notebook so a
    /// later restore cannot point back at an unavailable Stack. Notes retain
    /// their notebook IDs and need no body/resource hydration or revision
    /// rewrite.
    pub fn delete_stack(&self, id: &StackId) -> Result<(), LibraryError> {
        let now = self.now();
        let mut connection = self.connection.lock().expect("library mutex poisoned");
        let transaction = connection.transaction()?;
        let (stack_revision, stack_updated_time): (i64, i64) = transaction
            .query_row(
                "SELECT revision, updated_time FROM stacks WHERE id = ?1 AND deleted_time = 0",
                [id.as_str()],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?
            .ok_or(LibraryError::NotFound)?;
        let children = {
            let mut statement = transaction.prepare(
                "SELECT id, revision, updated_time
                 FROM notebooks
                 WHERE stack_id = ?1
                 ORDER BY id",
            )?;
            statement
                .query_map([id.as_str()], |row| {
                    Ok((
                        NotebookId::parse(row.get::<_, String>(0)?).map_err(invalid_column)?,
                        row.get::<_, i64>(1)?,
                        row.get::<_, i64>(2)?,
                    ))
                })?
                .collect::<Result<Vec<_>, _>>()?
        };
        let mut events = vec![LibraryEvent::OrganizationChanged];
        for (notebook_id, revision, updated_time) in children {
            let revision = revision
                .checked_add(1)
                .ok_or(LibraryError::InvalidSnapshot)?;
            let updated_time = now.max(
                updated_time
                    .checked_add(1)
                    .ok_or(LibraryError::InvalidSnapshot)?,
            );
            let changed = transaction.execute(
                "UPDATE notebooks
                 SET stack_id = NULL, revision = ?2, updated_time = ?3
                 WHERE id = ?1 AND stack_id = ?4",
                params![notebook_id.as_str(), revision, updated_time, id.as_str()],
            )?;
            if changed != 1 {
                return Err(LibraryError::InvalidSnapshot);
            }
            enqueue_sync(
                &transaction,
                self.id_source.as_ref(),
                &EntityRef::Notebook(notebook_id.clone()),
                revision,
                "stack_remove",
                updated_time,
            )?;
            events.push(LibraryEvent::SyncQueued(EntityRef::Notebook(notebook_id)));
        }
        let deleted_revision = stack_revision
            .checked_add(1)
            .ok_or(LibraryError::InvalidSnapshot)?;
        let deleted_time = now.max(
            stack_updated_time
                .checked_add(1)
                .ok_or(LibraryError::InvalidSnapshot)?,
        );
        let changed = transaction.execute(
            "UPDATE stacks
             SET deleted_time = ?2, updated_time = ?2, revision = ?3
             WHERE id = ?1 AND deleted_time = 0",
            params![id.as_str(), deleted_time, deleted_revision],
        )?;
        if changed != 1 {
            return Err(LibraryError::NotFound);
        }
        enqueue_sync(
            &transaction,
            self.id_source.as_ref(),
            &EntityRef::Stack(id.clone()),
            deleted_revision,
            "delete",
            deleted_time,
        )?;
        transaction.commit()?;
        events.push(LibraryEvent::SyncQueued(EntityRef::Stack(id.clone())));
        self.publish(events);
        Ok(())
    }

    /// Retires a tag and removes all typed note-tag rows in one transaction.
    /// The note's body, resources, and selected thumbnail remain untouched.
    pub fn delete_tag(&self, id: &TagId) -> Result<(), LibraryError> {
        let now = self.now();
        let mut connection = self.connection.lock().expect("library mutex poisoned");
        let transaction = connection.transaction()?;
        let revision: i64 = transaction
            .query_row(
                "SELECT revision FROM tags WHERE id = ?1 AND deleted_time = 0",
                [id.as_str()],
                |row| row.get(0),
            )
            .optional()?
            .ok_or(LibraryError::NotFound)?;
        let note_ids = note_ids_for_tag(&transaction, id)?;
        let mut events = vec![LibraryEvent::OrganizationChanged];
        for note_id in note_ids {
            let note_revision = next_note_revision_any(&transaction, &note_id)?;
            let updated = next_note_time(&transaction, &note_id, now)?;
            transaction.execute(
                "UPDATE notes SET updated_time = ?2, revision = ?3 WHERE id = ?1",
                params![note_id.as_str(), updated, note_revision],
            )?;
            queue_search(&transaction, &note_id, updated, "organization")?;
            enqueue_sync(
                &transaction,
                self.id_source.as_ref(),
                &EntityRef::Note(note_id.clone()),
                note_revision,
                "tags",
                updated,
            )?;
            events.push(LibraryEvent::NoteProjectionChanged(note_id.clone()));
            events.push(LibraryEvent::SearchProjectionQueued(note_id.clone()));
            events.push(LibraryEvent::SyncQueued(EntityRef::Note(note_id)));
        }
        transaction.execute("DELETE FROM note_tags WHERE tag_id = ?1", [id.as_str()])?;
        let updated_time = next_organization_time(&transaction, "tags", id.as_str(), now)?;
        let deleted_revision = revision
            .checked_add(1)
            .ok_or(LibraryError::InvalidSnapshot)?;
        transaction.execute(
            "UPDATE tags SET deleted_time = ?2, updated_time = ?2, revision = ?3 WHERE id = ?1",
            params![id.as_str(), updated_time, deleted_revision],
        )?;
        enqueue_sync(
            &transaction,
            self.id_source.as_ref(),
            &EntityRef::Tag(id.clone()),
            deleted_revision,
            "delete",
            updated_time,
        )?;
        transaction.commit()?;
        events.push(LibraryEvent::SyncQueued(EntityRef::Tag(id.clone())));
        self.publish(events);
        Ok(())
    }

    pub fn append_edit_journal(
        &self,
        entry: EditJournalEntry,
    ) -> Result<JournalOwnership, LibraryError> {
        let now = self.now();
        if entry.writer_token.is_empty() {
            return Err(LibraryError::InvalidSnapshot);
        }
        let mut connection = self.connection.lock().expect("library mutex poisoned");
        // A writable journal is a compare-and-swap against one concrete note
        // revision. `IMMEDIATE` makes two retained windows serialize here, so
        // a local generation from one window can never outrank a newer
        // committed revision from another.
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let actual_revision: Option<i64> = transaction
            .query_row(
                "SELECT revision FROM notes WHERE id = ?1 AND deleted_time = 0",
                [entry.note_id.as_str()],
                |row| row.get(0),
            )
            .optional()?;
        let actual_revision = actual_revision.ok_or(LibraryError::NotFound)?;
        if actual_revision != entry.expected_revision {
            return Err(LibraryError::StaleRevision {
                expected: entry.expected_revision,
                actual: actual_revision,
            });
        }
        let existing_writer: Option<String> = transaction
            .query_row(
                "SELECT writer_token
                 FROM edit_journal
                 WHERE note_id = ?1 AND expected_revision = ?2
                 ORDER BY sequence DESC
                 LIMIT 1",
                params![entry.note_id.as_str(), entry.expected_revision],
                |row| row.get(0),
            )
            .optional()?;
        if existing_writer
            .as_deref()
            .is_some_and(|writer| writer != entry.writer_token)
        {
            // `sequence` records commit order, not semantic capture order.
            // Once a writer has published this same-base checkpoint, a late
            // worker from another retained session must fail closed instead
            // of deleting the newer checkpoint it did not own.
            return Err(LibraryError::JournalOwnershipConflict);
        }
        let sequence = transaction
            .query_row(
                "SELECT next_sequence FROM journal_sequence WHERE id = 1",
                [],
                |row| row.get::<_, i64>(0),
            )?
            .checked_add(1)
            .ok_or(LibraryError::InvalidSnapshot)?;
        transaction.execute(
            "UPDATE journal_sequence SET next_sequence = ?1 WHERE id = 1",
            [sequence],
        )?;
        let ownership = JournalOwnership {
            writer_token: entry.writer_token.clone(),
            sequence,
        };
        // The journal is a compact crash-recovery checkpoint, not an edit
        // history. Keeping precisely the newest same-base record bounds disk
        // use and makes a stale session unable to win by generation sorting.
        transaction.execute(
            "DELETE FROM edit_journal WHERE note_id = ?1",
            [entry.note_id.as_str()],
        )?;
        self.insert_with_unique_id(&transaction, "edit_journal", |candidate| {
            transaction.execute("INSERT INTO edit_journal (id, note_id, expected_revision, writer_token, sequence, generation, delta_utf8, created_time) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)", params![candidate, entry.note_id.as_str(), entry.expected_revision, entry.writer_token, sequence, entry.generation, entry.delta_utf8, now])
        })?;
        transaction.commit()?;
        Ok(ownership)
    }

    /// Transfers one recovered crash checkpoint to a fresh retained-session
    /// owner. The checkpoint payload embeds its owner too, so both the SQL
    /// column and JSON must be replaced by the same exact checkpoint-identity
    /// CAS: note, revision, old token, and durable sequence.
    ///
    /// This intentionally preserves the checkpoint's durable sequence and
    /// generation: taking over after a process crash is not a new semantic
    /// edit, and must not let a delayed former writer reorder recovery.
    pub fn claim_edit_journal_ownership(
        &self,
        note_id: &NoteId,
        expected_revision: i64,
        previous_writer_token: &str,
        expected_sequence: i64,
        writer_token: &str,
        delta_utf8: &str,
    ) -> Result<JournalOwnership, LibraryError> {
        if expected_revision < 1
            || previous_writer_token.is_empty()
            || expected_sequence < 1
            || writer_token.is_empty()
            || previous_writer_token == writer_token
            || delta_utf8.is_empty()
        {
            return Err(LibraryError::InvalidSnapshot);
        }
        let mut connection = self.connection.lock().expect("library mutex poisoned");
        // The read and token transfer share one IMMEDIATE transaction. Two
        // restart candidates that read a crashed row before either transfer
        // it can therefore have only one winner. A later checkpoint from the
        // same pre-crash token changes the sequence and makes this prepared
        // claim fail; it can never be overwritten by stale recovery data.
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let actual_revision: Option<i64> = transaction
            .query_row(
                "SELECT revision FROM notes WHERE id = ?1 AND deleted_time = 0",
                [note_id.as_str()],
                |row| row.get(0),
            )
            .optional()?;
        let actual_revision = actual_revision.ok_or(LibraryError::NotFound)?;
        if actual_revision != expected_revision {
            return Err(LibraryError::StaleRevision {
                expected: expected_revision,
                actual: actual_revision,
            });
        }
        let checkpoint_id: Option<String> = transaction
            .query_row(
                "SELECT id FROM edit_journal
                 WHERE note_id = ?1 AND expected_revision = ?2 AND writer_token = ?3
                   AND sequence = ?4",
                params![
                    note_id.as_str(),
                    expected_revision,
                    previous_writer_token,
                    expected_sequence
                ],
                |row| row.get(0),
            )
            .optional()?;
        let checkpoint_id = checkpoint_id.ok_or(LibraryError::JournalOwnershipConflict)?;
        let changed = transaction.execute(
            "UPDATE edit_journal
             SET writer_token = ?2, delta_utf8 = ?3
             WHERE id = ?1 AND note_id = ?4 AND expected_revision = ?5
               AND writer_token = ?6 AND sequence = ?7",
            params![
                checkpoint_id,
                writer_token,
                delta_utf8,
                note_id.as_str(),
                expected_revision,
                previous_writer_token,
                expected_sequence
            ],
        )?;
        if changed != 1 {
            return Err(LibraryError::JournalOwnershipConflict);
        }
        transaction.commit()?;
        Ok(JournalOwnership {
            writer_token: writer_token.to_owned(),
            sequence: expected_sequence,
        })
    }

    /// Returns the most recent durable edit-journal payload for one live note.
    /// The GPUI layer never opens SQLite itself: recovery is deliberately a
    /// core-owned read so a crashed editor can reconstruct its latest readable
    /// snapshot before the next settled flush compacts the journal.
    pub fn latest_edit_journal(
        &self,
        note_id: &NoteId,
    ) -> Result<Option<EditJournalEntry>, LibraryError> {
        self.latest_edit_journal_for_revision(note_id, None)
    }

    /// Reads only a crash checkpoint that was computed from the exact durable
    /// revision being opened. A note may have accumulated stale rows before a
    /// prior build was interrupted; recovery must never apply one across a
    /// newer snapshot boundary.
    pub fn latest_edit_journal_for_revision(
        &self,
        note_id: &NoteId,
        expected_revision: Option<i64>,
    ) -> Result<Option<EditJournalEntry>, LibraryError> {
        let connection = self.connection.lock().expect("library mutex poisoned");
        let mut statement = connection.prepare(
            "SELECT note_id, expected_revision, writer_token, sequence, generation, delta_utf8
             FROM edit_journal
             WHERE note_id = ?1 AND (?2 IS NULL OR expected_revision = ?2)
             ORDER BY sequence DESC
             LIMIT 1",
        )?;
        statement
            .query_row(params![note_id.as_str(), expected_revision], |row| {
                Ok(EditJournalEntry {
                    note_id: NoteId::parse(row.get::<_, String>(0)?).map_err(invalid_column)?,
                    expected_revision: row.get(1)?,
                    writer_token: row.get(2)?,
                    sequence: row.get(3)?,
                    generation: row.get(4)?,
                    delta_utf8: row.get(5)?,
                })
            })
            .optional()
            .map_err(Into::into)
    }

    /// Returns the occurrence-bounded resource provenance that a crash
    /// checkpoint may restore for one exact durable note revision.
    ///
    /// This deliberately derives authority from the note's own committed
    /// revision bodies, never from the journal's JSON `resource_ids` field.
    /// A normal snapshot can remove a relation and a later Cmd-Z can restore
    /// it before the following snapshot; retaining each resource's highest
    /// durable occurrence count lets that valid undo recover after a crash
    /// without granting unrelated global resources. The query is recovery
    /// only, so normal note opens do not scan revision history.
    pub fn durable_resource_provenance_for_recovery(
        &self,
        note_id: &NoteId,
        expected_revision: i64,
    ) -> Result<Vec<ResourceId>, LibraryError> {
        if expected_revision < 1 {
            return Err(LibraryError::InvalidSnapshot);
        }
        let connection = self.connection.lock().expect("library mutex poisoned");
        let (current_html, current_revision): (String, i64) = connection
            .query_row(
                "SELECT body_html, revision FROM notes WHERE id = ?1 AND deleted_time = 0",
                [note_id.as_str()],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?
            .ok_or(LibraryError::NotFound)?;
        if current_revision != expected_revision {
            return Err(LibraryError::StaleRevision {
                expected: expected_revision,
                actual: current_revision,
            });
        }
        let mut revision_html = vec![current_html];
        let mut statement = connection.prepare(
            "SELECT body_html FROM note_revisions
             WHERE note_id = ?1 AND revision <= ?2
             ORDER BY revision ASC",
        )?;
        revision_html.extend(
            statement
                .query_map(params![note_id.as_str(), expected_revision], |row| {
                    row.get::<_, String>(0)
                })?
                .collect::<Result<Vec<_>, _>>()?,
        );
        drop(statement);

        let mut maximum_occurrences = HashMap::<ResourceId, usize>::new();
        let mut first_seen = Vec::<ResourceId>::new();
        for body_html in revision_html {
            let document = CanonicalDocument::parse_html(&body_html)?;
            // Revisions are created only from canonical storage. Refusing a
            // noncanonical historical row avoids treating a tampered legacy
            // string as authority for a future crash checkpoint.
            if document.to_canonical_html().as_str() != body_html {
                return Err(LibraryError::InvalidSnapshot);
            }
            let mut occurrences = HashMap::<ResourceId, usize>::new();
            for resource_id in document.resource_ids() {
                if !maximum_occurrences.contains_key(&resource_id) {
                    first_seen.push(resource_id.clone());
                }
                *occurrences.entry(resource_id).or_default() += 1;
            }
            for (resource_id, count) in occurrences {
                maximum_occurrences
                    .entry(resource_id)
                    .and_modify(|known| *known = (*known).max(count))
                    .or_insert(count);
            }
        }

        for resource_id in &first_seen {
            let exists: i64 = connection.query_row(
                "SELECT EXISTS(SELECT 1 FROM resources WHERE id = ?1 AND deleted_time = 0)",
                [resource_id.as_str()],
                |row| row.get(0),
            )?;
            if exists != 1 {
                return Err(LibraryError::NotFound);
            }
        }
        let mut provenance = Vec::new();
        for resource_id in first_seen {
            let count = maximum_occurrences
                .get(&resource_id)
                .copied()
                .ok_or(LibraryError::InvalidSnapshot)?;
            provenance.extend(std::iter::repeat_n(resource_id, count));
        }
        Ok(provenance)
    }

    pub fn flush_snapshot(
        &self,
        input: SaveNote,
        journal_ownership: Option<JournalOwnership>,
    ) -> Result<SavedRevision, LibraryError> {
        let note = self.flush_snapshot_note(input, journal_ownership)?;
        Ok(SavedRevision {
            revision: note.revision,
            saved_time: note.updated_time,
        })
    }

    /// Snapshot a note and return the complete durable outcome assembled in
    /// the same SQLite transaction. Resource insertion uses this instead of a
    /// post-commit `load_note`, so an already committed association cannot be
    /// misreported as a failed UI operation if a later hydration faults.
    pub fn flush_snapshot_note(
        &self,
        input: SaveNote,
        journal_ownership: Option<JournalOwnership>,
    ) -> Result<Note, LibraryError> {
        self.save_note_inner(input, true, journal_ownership.as_ref(), None)
            .map(|commit| commit.note)
    }

    /// Stage a resource blob without creating a `resources` row, sync outbox
    /// item, or library event.  Callers that will put the resource into a
    /// note must follow this with [`Self::commit_staged_resource_snapshot`];
    /// abandoning the stage leaves only safe content-addressed bytes for a
    /// later garbage collector.
    pub fn stage_resource(
        &self,
        bytes: &[u8],
        title: &str,
        mime: &str,
        extension: &str,
    ) -> Result<StagedResource, LibraryError> {
        let blob = self.resource_store.put(ResourceInput {
            bytes,
            title,
            mime,
            file_extension: extension,
        })?;
        let resource_id = self.allocate_resource_id()?;
        Ok(StagedResource {
            resource_id,
            blob,
            title: title.to_owned(),
            mime: mime.to_owned(),
            file_extension: extension.to_owned(),
        })
    }

    /// Stream a descriptor/file source into an invisible stage.  The source
    /// is copied and hashed with `ResourceStore`'s fixed buffer; no path is
    /// retained or exposed to the UI and no full resource-sized `Vec` is
    /// constructed for Finder/drop attachments.
    pub fn stage_resource_reader<R: Read>(
        &self,
        reader: R,
        size: usize,
        title: &str,
        mime: &str,
        extension: &str,
    ) -> Result<StagedResource, LibraryError> {
        let blob = self
            .resource_store
            .put_reader(reader, size, title, mime, extension)?;
        let resource_id = self.allocate_resource_id()?;
        Ok(StagedResource {
            resource_id,
            blob,
            title: title.to_owned(),
            mime: mime.to_owned(),
            file_extension: extension.to_owned(),
        })
    }

    /// Atomically expose a prior staged blob together with its complete note
    /// snapshot, resource ordering, thumbnail projection, search work, and
    /// both sync-outbox records.  Events are published only after this one
    /// SQLite commit, so a later snapshot failure cannot leak a resource event
    /// or a visible orphan record.
    pub fn commit_staged_resource_snapshot(
        &self,
        input: SaveNote,
        journal_ownership: Option<JournalOwnership>,
        staged: &StagedResource,
    ) -> Result<StagedResourceSnapshotCommit, LibraryError> {
        self.save_note_inner(input, true, journal_ownership.as_ref(), Some(staged))
    }

    pub fn import_image(
        &self,
        bytes: &[u8],
        title: &str,
        mime: &str,
        extension: &str,
    ) -> Result<ResourceId, LibraryError> {
        self.import_resource(bytes, title, mime, extension)
    }

    /// Persist an opaque, content-addressed resource before it is associated
    /// with a note snapshot.  The caller still has to complete the note
    /// transaction (or explicitly roll this metadata back) before the
    /// resource becomes visible in a document.
    pub fn import_resource(
        &self,
        bytes: &[u8],
        title: &str,
        mime: &str,
        extension: &str,
    ) -> Result<ResourceId, LibraryError> {
        let staged = self.stage_resource(bytes, title, mime, extension)?;
        self.commit_staged_resource_metadata(&staged)?;
        Ok(staged.resource_id().clone())
    }

    /// Stand-alone resource persistence remains available for import tools
    /// that intentionally expose a resource without changing a note.  The
    /// note-session route must use `stage_*` plus
    /// `commit_staged_resource_snapshot` instead.
    pub fn import_resource_reader<R: Read>(
        &self,
        reader: R,
        size: usize,
        title: &str,
        mime: &str,
        extension: &str,
    ) -> Result<ResourceId, LibraryError> {
        let staged = self.stage_resource_reader(reader, size, title, mime, extension)?;
        self.commit_staged_resource_metadata(&staged)?;
        Ok(staged.resource_id().clone())
    }

    fn commit_staged_resource_metadata(&self, staged: &StagedResource) -> Result<(), LibraryError> {
        let now = self.now();
        let mut connection = self.connection.lock().expect("library mutex poisoned");
        // Stand-alone imports share the exact staged-byte publication fence
        // with note snapshots. A deferred transaction would leave a gap for
        // durable GC between validation and INSERT; IMMEDIATE serializes that
        // physical decision with `drain_resource_gc`.
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        self.resource_store.verify_staged_blob(&staged.blob)?;
        self.insert_staged_resource_metadata(&transaction, staged, now)?;
        transaction.commit()?;
        drop(connection);
        self.publish(vec![LibraryEvent::SyncQueued(EntityRef::Resource(
            staged.resource_id().clone(),
        ))]);
        Ok(())
    }

    fn insert_staged_resource_metadata(
        &self,
        transaction: &Transaction<'_>,
        staged: &StagedResource,
        now: i64,
    ) -> Result<(), LibraryError> {
        transaction.execute(
            "INSERT OR IGNORE INTO resource_blobs (sha256, size, mime, relative_path, created_time, revision) VALUES (?1, ?2, ?3, ?4, ?5, 1)",
            params![
                staged.blob.sha256.as_str(),
                staged.blob.size as i64,
                &staged.mime,
                format!("resources/blobs/{}", staged.blob.sha256.as_str()),
                now
            ],
        )?;
        transaction.execute(
            "INSERT INTO resources (id, sha256, title, mime, file_extension, size, created_time, updated_time, revision) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?7, 1)",
            params![
                staged.resource_id().as_str(),
                staged.blob.sha256.as_str(),
                &staged.title,
                &staged.mime,
                &staged.file_extension,
                staged.blob.size as i64,
                now,
            ],
        )?;
        enqueue_sync(
            transaction,
            self.id_source.as_ref(),
            &EntityRef::Resource(staged.resource_id().clone()),
            1,
            "create",
            now,
        )?;
        Ok(())
    }

    /// Read bytes only after resolving an already-validated resource record.
    /// Projection paths never call this API; the narrow method keeps image
    /// decoding and attachment preview materialization out of list/card
    /// hydration.
    pub fn read_resource_bytes(&self, id: &ResourceId) -> Result<Option<Vec<u8>>, LibraryError> {
        let Some(resource) = self.resource_metadata(id)? else {
            return Ok(None);
        };
        Ok(Some(self.resource_store.read(&resource.sha256)?))
    }

    /// Resolve metadata and return a descriptor-safe, hash-verified stream of
    /// the durable bytes. The profile pathname never escapes this boundary;
    /// consumers can materialize one cache source at a time without turning a
    /// note with many images into many simultaneous `Vec<u8>` allocations.
    pub fn open_verified_resource_file(
        &self,
        id: &ResourceId,
    ) -> Result<Option<(crate::StoredResource, File)>, LibraryError> {
        let Some(resource) = self.resource_metadata(id)? else {
            return Ok(None);
        };
        let file = self.resource_store.open_verified(&resource.sha256)?;
        Ok(Some((resource, file)))
    }

    /// Resolves resource metadata and opens a hash-verified descriptor with a
    /// physical byte ceiling. This is for bounded extractors; existing image
    /// and preview paths retain [`Self::open_verified_resource_file`].
    pub fn open_verified_resource_file_with_limit(
        &self,
        id: &ResourceId,
        maximum_bytes: usize,
    ) -> Result<Option<(crate::StoredResource, File)>, LibraryError> {
        let Some(resource) = self.resource_metadata(id)? else {
            return Ok(None);
        };
        let file = self
            .resource_store
            .open_verified_with_limit(&resource.sha256, maximum_bytes)?;
        Ok(Some((resource, file)))
    }

    /// Observes descriptor-safe resource opens without exposing raw bytes.
    /// This is deliberately a diagnostic seam rather than a projection API:
    /// callers use it to guard against eager resource hydration on a detail
    /// mount while normal application code keeps using
    /// [`Self::open_verified_resource_file`].
    #[cfg(any(test, feature = "test-support"))]
    pub fn observe_verified_resource_opens(&self) -> std::sync::mpsc::Receiver<crate::BlobHash> {
        self.resource_store.observe_verified_opens()
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

    /// Takes a bounded set of live attachment identities for the future local
    /// extractor. It returns no blob handle or bytes; D3b must opt into
    /// `open_verified_resource_file` after taking one job.
    pub fn take_derived_text_jobs(
        &self,
        limit: usize,
    ) -> Result<Vec<DerivedTextJob>, LibraryError> {
        let limit = i64::try_from(limit.min(100)).map_err(|_| LibraryError::InvalidSnapshot)?;
        let connection = self.connection.lock().expect("library mutex poisoned");
        let mut statement = connection.prepare(
            "SELECT j.resource_id,j.sha256,j.extractor_version
             FROM derived_text_jobs j JOIN resources r ON r.id=j.resource_id
             WHERE j.state='pending' AND j.extractor_version=?1 AND j.sha256=r.sha256 AND r.deleted_time=0
               AND EXISTS(SELECT 1 FROM note_resources nr JOIN notes n ON n.id=nr.note_id WHERE nr.resource_id=j.resource_id AND nr.is_associated=1 AND n.deleted_time=0)
             ORDER BY CASE r.mime WHEN 'application/pdf' THEN 0 ELSE 1 END,
                      CASE WHEN r.mime='application/pdf' THEN j.updated_time ELSE -j.updated_time END,
                      j.resource_id LIMIT ?2",
        )?;
        statement
            .query_map(params![DERIVED_TEXT_EXTRACTOR_VERSION, limit], |row| {
                Ok(DerivedTextJob {
                    resource_id: ResourceId::new(row.get::<_, String>(0)?)
                        .map_err(invalid_column)?,
                    sha256: crate::BlobHash::new(row.get::<_, String>(1)?)
                        .map_err(invalid_column)?,
                    extractor_version: row.get(2)?,
                })
            })?
            .collect::<Result<Vec<_>, _>>()
            .map_err(Into::into)
    }

    /// Atomically accepts synthetic/platform extraction output only when the
    /// queued identity is still the current, live associated resource.
    /// `false` means stale, detached, deleted, or already superseded; none of
    /// those cases may revive a searchable projection.
    pub fn publish_derived_text(
        &self,
        job: &DerivedTextJob,
        text: &str,
    ) -> Result<bool, LibraryError> {
        if text.len() > MAX_DERIVED_TEXT_BYTES {
            return Err(LibraryError::DerivedTextTooLarge);
        }
        let now = self.now();
        let mut connection = self.connection.lock().expect("library mutex poisoned");
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let current: i64 = transaction.query_row(
            "SELECT EXISTS(SELECT 1 FROM derived_text_jobs j JOIN resources r ON r.id=j.resource_id WHERE j.resource_id=?1 AND j.sha256=?2 AND r.sha256=?2 AND j.extractor_version=?3 AND j.extractor_version=?4 AND j.state='pending' AND r.deleted_time=0 AND EXISTS(SELECT 1 FROM note_resources nr JOIN notes n ON n.id=nr.note_id WHERE nr.resource_id=j.resource_id AND nr.is_associated=1 AND n.deleted_time=0))",
            params![job.resource_id.as_str(), job.sha256.as_str(), &job.extractor_version, DERIVED_TEXT_EXTRACTOR_VERSION], |row| row.get(0))?;
        if current == 0 {
            transaction.commit()?;
            return Ok(false);
        }
        transaction.execute("INSERT INTO derived_text_rows(resource_id,sha256,extractor_version) VALUES(?1,?2,?3) ON CONFLICT(resource_id) DO UPDATE SET sha256=excluded.sha256,extractor_version=excluded.extractor_version", params![job.resource_id.as_str(), job.sha256.as_str(), &job.extractor_version])?;
        let rowid: i64 = transaction.query_row(
            "SELECT fts_rowid FROM derived_text_rows WHERE resource_id=?1",
            [job.resource_id.as_str()],
            |row| row.get(0),
        )?;
        transaction.execute("DELETE FROM derived_text_unicode WHERE rowid=?1", [rowid])?;
        transaction.execute("DELETE FROM derived_text_trigram WHERE rowid=?1", [rowid])?;
        transaction.execute(
            "INSERT INTO derived_text_unicode(rowid,resource_id,text) VALUES(?1,?2,?3)",
            params![rowid, job.resource_id.as_str(), text],
        )?;
        transaction.execute(
            "INSERT INTO derived_text_trigram(rowid,resource_id,text) VALUES(?1,?2,?3)",
            params![rowid, job.resource_id.as_str(), text],
        )?;
        transaction.execute("UPDATE derived_text_jobs SET state='indexed',failure=NULL,updated_time=?2 WHERE resource_id=?1", params![job.resource_id.as_str(), now])?;
        transaction.commit()?;
        drop(connection);
        self.publish(vec![LibraryEvent::DerivedTextIndexed(
            job.resource_id.clone(),
        )]);
        Ok(true)
    }

    /// Persist a classified extractor outcome without affecting the saved
    /// note. The caller may subsequently expose this state or retry it.
    pub fn fail_derived_text(
        &self,
        job: &DerivedTextJob,
        failure: DerivedTextFailure,
    ) -> Result<bool, LibraryError> {
        let now = self.now();
        let connection = self.connection.lock().expect("library mutex poisoned");
        let changed = connection.execute("UPDATE derived_text_jobs SET state='failed',failure=?4,attempts=attempts+1,updated_time=?5 WHERE resource_id=?1 AND sha256=?2 AND extractor_version=?3 AND extractor_version=?6 AND state='pending' AND EXISTS(SELECT 1 FROM resources r WHERE r.id=derived_text_jobs.resource_id AND r.sha256=derived_text_jobs.sha256 AND r.deleted_time=0) AND EXISTS(SELECT 1 FROM note_resources nr JOIN notes n ON n.id=nr.note_id WHERE nr.resource_id=derived_text_jobs.resource_id AND nr.is_associated=1 AND n.deleted_time=0)", params![job.resource_id.as_str(), job.sha256.as_str(), &job.extractor_version, failure.as_str(), now, DERIVED_TEXT_EXTRACTOR_VERSION])?;
        Ok(changed == 1)
    }

    pub fn retry_derived_text(&self, job: &DerivedTextJob) -> Result<bool, LibraryError> {
        let now = self.now();
        let connection = self.connection.lock().expect("library mutex poisoned");
        let changed = connection.execute("UPDATE derived_text_jobs SET state='pending',failure=NULL,updated_time=?4 WHERE resource_id=?1 AND sha256=?2 AND extractor_version=?3 AND extractor_version=?5 AND state='failed' AND EXISTS(SELECT 1 FROM resources r WHERE r.id=derived_text_jobs.resource_id AND r.sha256=derived_text_jobs.sha256 AND r.deleted_time=0) AND EXISTS(SELECT 1 FROM note_resources nr JOIN notes n ON n.id=nr.note_id WHERE nr.resource_id=derived_text_jobs.resource_id AND nr.is_associated=1 AND n.deleted_time=0)", params![job.resource_id.as_str(), job.sha256.as_str(), &job.extractor_version, now, DERIVED_TEXT_EXTRACTOR_VERSION])?;
        Ok(changed == 1)
    }

    pub fn derived_text_status(
        &self,
        id: &ResourceId,
    ) -> Result<Option<DerivedTextStatus>, LibraryError> {
        let connection = self.connection.lock().expect("library mutex poisoned");
        connection
            .query_row(
                "SELECT state,failure,attempts FROM derived_text_jobs WHERE resource_id=?1",
                [id.as_str()],
                |row| {
                    let state: String = row.get(0)?;
                    let failure: Option<String> = row.get(1)?;
                    let attempts = row.get(2)?;
                    Ok(match state.as_str() {
                        "pending" => DerivedTextStatus::Pending { attempts },
                        "indexed" => DerivedTextStatus::Indexed { attempts },
                        "failed" => DerivedTextStatus::Failed {
                            failure: failure
                                .as_deref()
                                .and_then(DerivedTextFailure::parse)
                                .ok_or_else(|| {
                                    rusqlite::Error::InvalidColumnType(
                                        1,
                                        "derived failure".into(),
                                        rusqlite::types::Type::Text,
                                    )
                                })?,
                            attempts,
                        },
                        _ => {
                            return Err(rusqlite::Error::InvalidColumnType(
                                0,
                                "derived state".into(),
                                rusqlite::types::Type::Text,
                            ));
                        }
                    })
                },
            )
            .optional()
            .map_err(Into::into)
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
        take_search_jobs(&connection, limit)
    }

    /// True while a durable FTS projection transaction is still queued. The
    /// UI uses this only from its background refresh coordinator: it is the
    /// ordering authority between a saved note and a SearchRoute reread.
    pub fn has_pending_search_jobs(&self) -> Result<bool, LibraryError> {
        let connection = self.connection.lock().expect("library mutex poisoned");
        connection
            .query_row("SELECT EXISTS(SELECT 1 FROM search_queue)", [], |row| {
                row.get(0)
            })
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

    /// Runs one bounded local indexing batch.  It is intentionally explicit:
    /// a note save is authoritative before this disposable projection exists.
    pub fn process_search_jobs(&self) -> Result<usize, LibraryError> {
        crate::search::process_search_jobs(self)
    }

    /// Stops only at a per-note transaction boundary. The current derived
    /// projection either commits with its queue acknowledgement or rolls back;
    /// no later queue identity is touched after cancellation is observed.
    pub fn process_search_jobs_until_cancelled(
        &self,
        cancelled: &AtomicBool,
    ) -> Result<usize, LibraryError> {
        crate::search::process_search_jobs_until_cancelled(self, cancelled)
    }

    pub(crate) fn with_search_index_connection<T>(
        &self,
        operation: impl FnOnce(&mut Connection) -> Result<T, LibraryError>,
    ) -> Result<T, LibraryError> {
        let mut slot = self
            .index_connection
            .lock()
            .expect("search-index mutex poisoned");
        if slot.is_none() {
            *slot = Some(self.open_verified_search_index_connection()?);
        }
        operation(slot.as_mut().expect("index connection was initialized"))
    }

    fn open_verified_search_index_connection(&self) -> Result<Connection, LibraryError> {
        if !self
            .profile_dir
            .verify_path_identity()
            .map_err(|_| LibraryError::InvalidDatabasePath)?
        {
            return Err(LibraryError::InvalidDatabasePath);
        }
        let path = self
            .profile_dir
            .database_path(&self.database_name)
            .map_err(|_| LibraryError::InvalidDatabasePath)?;
        let connection = Connection::open_with_flags(
            path,
            OpenFlags::SQLITE_OPEN_READ_WRITE
                | OpenFlags::SQLITE_OPEN_NO_MUTEX
                | OpenFlags::SQLITE_OPEN_NOFOLLOW,
        )?;
        if connection_has_moved(&connection)?
            || !self
                .profile_dir
                .verify_path_identity()
                .map_err(|_| LibraryError::InvalidDatabasePath)?
            || !self
                .database_file
                .matches_sqlite_connection(&connection, &self.profile_dir)
                .map_err(|_| LibraryError::InvalidDatabasePath)?
        {
            return Err(LibraryError::InvalidDatabasePath);
        }
        connection.pragma_update(None, "foreign_keys", "ON")?;
        let foreign_keys: i64 =
            connection.query_row("PRAGMA foreign_keys", [], |row| row.get(0))?;
        if !journal_mode(&connection)?.eq_ignore_ascii_case("wal") || foreign_keys != 1 {
            return Err(LibraryError::Pragma);
        }
        // The indexer yields to foreground writers rather than holding a
        // process-wide repository mutex.  Each note still uses one immediate
        // transaction, so this timeout is bounded by that single projection.
        connection.busy_timeout(std::time::Duration::from_millis(250))?;
        Ok(connection)
    }

    /// Searches only derived FTS tables plus card projection metadata.  The
    /// canonical HTML/body and blob bytes remain unavailable to this path.
    pub fn search(&self, query: SearchQuery) -> Result<Vec<SearchHit>, LibraryError> {
        let mut predicates = Vec::<String>::new();
        let mut values = Vec::<rusqlite::types::Value>::new();
        let mut filename_provenance = None;
        let mut mime_provenance = None;
        let mut ordinary_filename_provenance = Vec::new();
        let mut ordinary_derived_text_provenance = Vec::new();
        let trash = query
            .filters
            .iter()
            .find_map(|filter| match filter {
                SearchFilter::Trash(value) => Some(*value),
                _ => None,
            })
            .unwrap_or(false);
        predicates.push(
            if trash {
                "n.deleted_time <> 0"
            } else {
                "n.deleted_time = 0"
            }
            .into(),
        );
        for filter in &query.filters {
            match filter {
                SearchFilter::Trash(_) => {}
                SearchFilter::Notebook(value) => { predicates.push("n.notebook_id IN (SELECT id FROM notebooks WHERE id=? OR title COLLATE NOCASE=?)".into()); values.push(rusqlite::types::Value::Text(value.clone())); values.push(rusqlite::types::Value::Text(value.clone())); }
                SearchFilter::Stack(value) => { predicates.push("n.notebook_id IN (SELECT nb.id FROM notebooks nb JOIN stacks s ON s.id=nb.stack_id WHERE s.id=? OR s.title COLLATE NOCASE=?)".into()); values.push(rusqlite::types::Value::Text(value.clone())); values.push(rusqlite::types::Value::Text(value.clone())); }
                SearchFilter::Tag(value) => { predicates.push("n.id IN (SELECT nt.note_id FROM note_tags nt JOIN tags t ON t.id=nt.tag_id WHERE t.id=? OR t.title COLLATE NOCASE=?)".into()); values.push(rusqlite::types::Value::Text(value.clone())); values.push(rusqlite::types::Value::Text(value.clone())); }
                SearchFilter::Created(range) => add_range(&mut predicates, &mut values, "n.created_time", range),
                SearchFilter::Updated(range) => add_range(&mut predicates, &mut values, "n.updated_time", range),
                SearchFilter::HasAttachment(value) => predicates.push(if *value { "EXISTS (SELECT 1 FROM note_resources nr JOIN resources r ON r.id=nr.resource_id WHERE nr.note_id=n.id AND nr.is_associated=1 AND r.deleted_time=0)" } else { "NOT EXISTS (SELECT 1 FROM note_resources nr JOIN resources r ON r.id=nr.resource_id WHERE nr.note_id=n.id AND nr.is_associated=1 AND r.deleted_time=0)" }.into()),
                SearchFilter::Filename(value) => {
                    filename_provenance.get_or_insert_with(|| value.clone());
                    predicates.push("EXISTS (SELECT 1 FROM note_resources nr JOIN resources r ON r.id=nr.resource_id WHERE nr.note_id=n.id AND nr.is_associated=1 AND r.deleted_time=0 AND r.title LIKE ? ESCAPE '\\')".into());
                    values.push(rusqlite::types::Value::Text(like_contains(value)));
                }
                SearchFilter::Mime(value) => {
                    mime_provenance.get_or_insert_with(|| value.clone());
                    predicates.push("EXISTS (SELECT 1 FROM note_resources nr JOIN resources r ON r.id=nr.resource_id WHERE nr.note_id=n.id AND nr.is_associated=1 AND r.deleted_time=0 AND r.mime LIKE ? ESCAPE '\\')".into());
                    values.push(rusqlite::types::Value::Text(like_contains(value)));
                }
                SearchFilter::Not(inner) => match inner.as_ref() {
                    SearchFilter::Tag(value) => { predicates.push("n.id NOT IN (SELECT nt.note_id FROM note_tags nt JOIN tags t ON t.id=nt.tag_id WHERE t.id=? OR t.title COLLATE NOCASE=?)".into()); values.push(rusqlite::types::Value::Text(value.clone())); values.push(rusqlite::types::Value::Text(value.clone())); }
                    SearchFilter::Notebook(value) => { predicates.push("n.notebook_id NOT IN (SELECT id FROM notebooks WHERE id=? OR title COLLATE NOCASE=?)".into()); values.push(rusqlite::types::Value::Text(value.clone())); values.push(rusqlite::types::Value::Text(value.clone())); }
                    _ => return Err(LibraryError::InvalidSnapshot),
                },
            }
        }
        for term in &query.terms {
            let (text, negated) = match term {
                SearchTerm::Text(text) | SearchTerm::Phrase(text) => (text, false),
                SearchTerm::NegatedText(text) | SearchTerm::NegatedPhrase(text) => (text, true),
            };
            if !negated {
                ordinary_filename_provenance.push(text.clone());
                ordinary_derived_text_provenance.push(text.clone());
            }
            let condition = if contains_short_cjk(text) {
                values.push(rusqlite::types::Value::Text(like_contains(text)));
                values.push(rusqlite::types::Value::Text(like_contains(text)));
                values.push(rusqlite::types::Value::Text(like_contains(text)));
                values.push(rusqlite::types::Value::Text(
                    DERIVED_TEXT_EXTRACTOR_VERSION.into(),
                ));
                values.push(rusqlite::types::Value::Text(like_contains(text)));
                "(n.id IN (SELECT sim.note_id FROM search_index_rows sim JOIN search_trigram st ON st.rowid=sim.fts_rowid WHERE st.title LIKE ? ESCAPE '\\' OR st.body LIKE ? ESCAPE '\\') OR n.id IN (SELECT nr.note_id FROM note_resources nr JOIN resources r ON r.id=nr.resource_id JOIN resource_search_rows rsr ON rsr.resource_id=r.id JOIN resource_filename_trigram rf ON rf.rowid=rsr.fts_rowid WHERE nr.is_associated=1 AND r.deleted_time=0 AND rf.filename LIKE ? ESCAPE '\\') OR n.id IN (SELECT nr.note_id FROM note_resources nr JOIN resources r ON r.id=nr.resource_id JOIN derived_text_rows dr ON dr.resource_id=r.id JOIN derived_text_jobs dj ON dj.resource_id=r.id JOIN derived_text_trigram dt ON dt.rowid=dr.fts_rowid WHERE nr.is_associated=1 AND r.deleted_time=0 AND r.sha256=dr.sha256 AND dj.state='indexed' AND dj.sha256=r.sha256 AND dj.extractor_version=dr.extractor_version AND dj.extractor_version=? AND dt.text LIKE ? ESCAPE '\\'))".to_owned()
            } else if contains_cjk(text) {
                values.push(rusqlite::types::Value::Text(fts_literal(text)));
                values.push(rusqlite::types::Value::Text(fts_literal(text)));
                values.push(rusqlite::types::Value::Text(
                    DERIVED_TEXT_EXTRACTOR_VERSION.into(),
                ));
                values.push(rusqlite::types::Value::Text(fts_literal(text)));
                "(n.id IN (SELECT sim.note_id FROM search_index_rows sim JOIN search_trigram st ON st.rowid=sim.fts_rowid WHERE search_trigram MATCH ?) OR n.id IN (SELECT nr.note_id FROM note_resources nr JOIN resources r ON r.id=nr.resource_id JOIN resource_search_rows rsr ON rsr.resource_id=r.id JOIN resource_filename_trigram rf ON rf.rowid=rsr.fts_rowid WHERE nr.is_associated=1 AND r.deleted_time=0 AND resource_filename_trigram MATCH ?) OR n.id IN (SELECT nr.note_id FROM note_resources nr JOIN resources r ON r.id=nr.resource_id JOIN derived_text_rows dr ON dr.resource_id=r.id JOIN derived_text_jobs dj ON dj.resource_id=r.id JOIN derived_text_trigram dt ON dt.rowid=dr.fts_rowid WHERE nr.is_associated=1 AND r.deleted_time=0 AND r.sha256=dr.sha256 AND dj.state='indexed' AND dj.sha256=r.sha256 AND dj.extractor_version=dr.extractor_version AND dj.extractor_version=? AND derived_text_trigram MATCH ?))".to_owned()
            } else {
                let fts = fts_literal(text);
                values.push(rusqlite::types::Value::Text(fts.clone()));
                values.push(rusqlite::types::Value::Text(fts));
                values.push(rusqlite::types::Value::Text(
                    DERIVED_TEXT_EXTRACTOR_VERSION.into(),
                ));
                values.push(rusqlite::types::Value::Text(fts_literal(text)));
                "(n.id IN (SELECT sim.note_id FROM search_index_rows sim JOIN search_unicode su ON su.rowid=sim.fts_rowid WHERE search_unicode MATCH ?) OR n.id IN (SELECT nr.note_id FROM note_resources nr JOIN resources r ON r.id=nr.resource_id JOIN resource_search_rows rsr ON rsr.resource_id=r.id JOIN resource_filename_unicode rf ON rf.rowid=rsr.fts_rowid WHERE nr.is_associated=1 AND r.deleted_time=0 AND resource_filename_unicode MATCH ?) OR n.id IN (SELECT nr.note_id FROM note_resources nr JOIN resources r ON r.id=nr.resource_id JOIN derived_text_rows dr ON dr.resource_id=r.id JOIN derived_text_jobs dj ON dj.resource_id=r.id JOIN derived_text_unicode dt ON dt.rowid=dr.fts_rowid WHERE nr.is_associated=1 AND r.deleted_time=0 AND r.sha256=dr.sha256 AND dj.state='indexed' AND dj.sha256=r.sha256 AND dj.extractor_version=dr.extractor_version AND dj.extractor_version=? AND derived_text_unicode MATCH ?))".to_owned()
            };
            predicates.push(if negated {
                format!("NOT ({condition})")
            } else {
                condition
            });
        }
        // Filters remain note-level AND predicates: filename and mime may be
        // satisfied by different current attachments. For UI provenance, use
        // the first filename filter when present, otherwise the first mime
        // filter; within either candidate set, relation order is stable.
        let mut provenance_values = Vec::<rusqlite::types::Value>::new();
        let matched_resource: String = if let Some(value) = filename_provenance {
            provenance_values.push(rusqlite::types::Value::Text(like_contains(&value)));
            "(SELECT nr.resource_id FROM note_resources nr JOIN resources r ON r.id=nr.resource_id WHERE nr.note_id=n.id AND nr.is_associated=1 AND r.deleted_time=0 AND r.title LIKE ? ESCAPE '\\' ORDER BY nr.position,nr.resource_id LIMIT 1)".into()
        } else if let Some(value) = mime_provenance {
            provenance_values.push(rusqlite::types::Value::Text(like_contains(&value)));
            "(SELECT nr.resource_id FROM note_resources nr JOIN resources r ON r.id=nr.resource_id WHERE nr.note_id=n.id AND nr.is_associated=1 AND r.deleted_time=0 AND r.mime LIKE ? ESCAPE '\\' ORDER BY nr.position,nr.resource_id LIMIT 1)".into()
        } else if !ordinary_filename_provenance.is_empty() {
            let mut matches = Vec::new();
            for term in &ordinary_filename_provenance {
                let (subquery, value) = filename_provenance_subquery(term);
                matches.push(subquery);
                provenance_values.push(value);
            }
            // COALESCE walks positive terms in query order. Each subquery
            // applies the same unicode/trigram semantics as the candidate
            // predicate, then relation order makes the chosen attachment
            // deterministic.
            let filename_matches = if matches.len() == 1 {
                matches.pop().expect("one filename provenance predicate")
            } else {
                format!("COALESCE({})", matches.join(","))
            };
            let mut derived_matches = Vec::new();
            for term in &ordinary_derived_text_provenance {
                let (subquery, mut values) = derived_text_provenance_subquery(term);
                derived_matches.push(subquery);
                provenance_values.append(&mut values);
            }
            let derived_matches = if derived_matches.len() == 1 {
                derived_matches
                    .pop()
                    .expect("one derived provenance predicate")
            } else {
                format!("COALESCE({})", derived_matches.join(","))
            };
            format!("COALESCE({filename_matches},{derived_matches})")
        } else {
            "NULL".into()
        };
        provenance_values.append(&mut values);
        let mut values = provenance_values;
        let limit =
            i64::try_from(query.limit()).map_err(|_| SearchQueryError::SqlIntegerOverflow)?;
        let offset =
            i64::try_from(query.offset()).map_err(|_| SearchQueryError::SqlIntegerOverflow)?;
        values.push(rusqlite::types::Value::Integer(limit));
        values.push(rusqlite::types::Value::Integer(offset));
        let sql = format!(
            "SELECT n.id, substr(n.title,1,120), substr(n.snippet,1,160), n.updated_time, n.deleted_time, n.notebook_id, COALESCE((SELECT n.selected_thumbnail_id WHERE EXISTS (SELECT 1 FROM note_resources snr JOIN resources sr ON sr.id=snr.resource_id WHERE snr.note_id=n.id AND snr.resource_id=n.selected_thumbnail_id AND snr.is_associated=1 AND sr.deleted_time=0 AND sr.mime LIKE 'image/%')), (SELECT nr.resource_id FROM note_resources nr JOIN resources r ON r.id=nr.resource_id WHERE nr.note_id=n.id AND nr.is_associated=1 AND r.deleted_time=0 AND r.mime IN ('image/png','image/jpeg') ORDER BY nr.position,nr.resource_id LIMIT 1)), (SELECT count(*) FROM note_resources nr JOIN resources r ON r.id=nr.resource_id WHERE nr.note_id=n.id AND nr.is_associated=1 AND r.deleted_time=0), {matched_resource} FROM notes n WHERE {} ORDER BY n.updated_time DESC, n.id ASC LIMIT ? OFFSET ?",
            predicates.join(" AND "),
        );
        #[cfg(any(test, feature = "test-support"))]
        let observers = std::mem::take(
            &mut *self
                .search_observers
                .lock()
                .expect("search observer mutex poisoned"),
        );
        #[cfg(any(test, feature = "test-support"))]
        let columns = Arc::new(Mutex::new(BTreeSet::new()));
        let connection = self.connection.lock().expect("library mutex poisoned");
        #[cfg(any(test, feature = "test-support"))]
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
                        .expect("search column observer mutex poisoned")
                        .insert(format!("{table_name}.{column_name}"));
                }
                Authorization::Allow
            }));
        }
        let result = (|| -> Result<Vec<SearchHit>, LibraryError> {
            let mut statement = connection.prepare(&sql)?;
            Ok(statement
                .query_map(params_from_iter(values), |row| {
                    Ok(SearchHit {
                        note: row_to_projection(row)?,
                        snippet: row.get(2)?,
                        matched_resource: row
                            .get::<_, Option<String>>(8)?
                            .map(ResourceId::new)
                            .transpose()
                            .map_err(invalid_column)?,
                    })
                })?
                .collect::<Result<Vec<_>, _>>()?)
        })();
        #[cfg(any(test, feature = "test-support"))]
        {
            connection.authorizer(None::<fn(rusqlite::hooks::AuthContext<'_>) -> Authorization>);
            if !observers.is_empty() {
                let fields = columns
                    .lock()
                    .expect("search column observer mutex poisoned")
                    .iter()
                    .cloned()
                    .collect::<Vec<_>>();
                for observer in observers {
                    let _ = observer.send(fields.clone());
                }
            }
        }
        result
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

pub(crate) fn take_search_jobs(
    connection: &Connection,
    limit: usize,
) -> Result<Vec<crate::SearchJob>, LibraryError> {
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

fn organization_title(value: &str) -> Result<String, LibraryError> {
    let value = value.trim();
    if value.is_empty() || value.chars().count() > 255 {
        return Err(LibraryError::InvalidOrganizationTitle);
    }
    Ok(value.to_owned())
}

/// Organization table names at these private call sites are static literals;
/// keeping the column/table shape here avoids public stringly-typed mutation
/// APIs while retaining a single revision/timestamp rule for all entities.
fn next_organization_revision(
    transaction: &Transaction<'_>,
    table: &str,
    id: &str,
) -> Result<i64, LibraryError> {
    let revision: Option<i64> = transaction
        .query_row(
            &format!("SELECT revision FROM {table} WHERE id = ?1 AND deleted_time = 0"),
            [id],
            |row| row.get(0),
        )
        .optional()?;
    revision
        .ok_or(LibraryError::NotFound)?
        .checked_add(1)
        .ok_or(LibraryError::InvalidSnapshot)
}

fn next_organization_time(
    transaction: &Transaction<'_>,
    table: &str,
    id: &str,
    now: i64,
) -> Result<i64, LibraryError> {
    let previous: Option<i64> = transaction
        .query_row(
            &format!("SELECT updated_time FROM {table} WHERE id = ?1 AND deleted_time = 0"),
            [id],
            |row| row.get(0),
        )
        .optional()?;
    let previous = previous.ok_or(LibraryError::NotFound)?;
    Ok(now.max(
        previous
            .checked_add(1)
            .ok_or(LibraryError::InvalidSnapshot)?,
    ))
}

fn next_note_revision_any(transaction: &Transaction<'_>, id: &NoteId) -> Result<i64, LibraryError> {
    let revision: Option<i64> = transaction
        .query_row(
            "SELECT revision FROM notes WHERE id = ?1",
            [id.as_str()],
            |row| row.get(0),
        )
        .optional()?;
    revision
        .ok_or(LibraryError::NotFound)?
        .checked_add(1)
        .ok_or(LibraryError::InvalidSnapshot)
}

fn normalized_tag_ids(tag_ids: &[TagId]) -> Result<Vec<TagId>, LibraryError> {
    let mut seen = BTreeSet::new();
    let mut normalized = Vec::with_capacity(tag_ids.len());
    for tag_id in tag_ids {
        if !seen.insert(tag_id.clone()) {
            return Err(LibraryError::InvalidSnapshot);
        }
        normalized.push(tag_id.clone());
    }
    Ok(normalized)
}

fn active_note_tag_ids(
    transaction: &Transaction<'_>,
    note_id: &NoteId,
) -> Result<Vec<TagId>, LibraryError> {
    let exists: i64 = transaction.query_row(
        "SELECT EXISTS(SELECT 1 FROM notes WHERE id = ?1 AND deleted_time = 0)",
        [note_id.as_str()],
        |row| row.get(0),
    )?;
    if exists != 1 {
        return Err(LibraryError::NotFound);
    }
    let mut statement = transaction
        .prepare("SELECT tag_id FROM note_tags WHERE note_id = ?1 ORDER BY position, tag_id")?;
    statement
        .query_map([note_id.as_str()], |row| {
            TagId::parse(row.get::<_, String>(0)?).map_err(invalid_column)
        })?
        .collect::<Result<Vec<_>, _>>()
        .map_err(Into::into)
}

fn replace_note_tags_in_transaction(
    transaction: &Transaction<'_>,
    id_source: &dyn RepositoryIdSource,
    note_id: &NoteId,
    tag_ids: &[TagId],
    now: i64,
    operation: &str,
) -> Result<(), LibraryError> {
    let _ = active_note_tag_ids(transaction, note_id)?;
    for tag_id in tag_ids {
        require_tag(transaction, tag_id)?;
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
    let revision = next_note_revision_any(transaction, note_id)?;
    let updated = next_note_time(transaction, note_id, now)?;
    transaction.execute(
        "UPDATE notes SET updated_time = ?2, revision = ?3 WHERE id = ?1",
        params![note_id.as_str(), updated, revision],
    )?;
    queue_search(transaction, note_id, updated, "organization")?;
    enqueue_sync(
        transaction,
        id_source,
        &EntityRef::Note(note_id.clone()),
        revision,
        operation,
        updated,
    )?;
    Ok(())
}

fn note_tag_events(note_id: &NoteId) -> Vec<LibraryEvent> {
    vec![
        LibraryEvent::OrganizationChanged,
        LibraryEvent::NoteProjectionChanged(note_id.clone()),
        LibraryEvent::SearchProjectionQueued(note_id.clone()),
        LibraryEvent::SyncQueued(EntityRef::Note(note_id.clone())),
    ]
}

fn note_ids_for_notebook(
    transaction: &Transaction<'_>,
    notebook_id: &NotebookId,
) -> Result<Vec<NoteId>, LibraryError> {
    let mut statement =
        transaction.prepare("SELECT id FROM notes WHERE notebook_id = ?1 ORDER BY id")?;
    statement
        .query_map([notebook_id.as_str()], |row| {
            NoteId::parse(row.get::<_, String>(0)?).map_err(invalid_column)
        })?
        .collect::<Result<Vec<_>, _>>()
        .map_err(Into::into)
}

fn note_ids_for_tag(
    transaction: &Transaction<'_>,
    tag_id: &TagId,
) -> Result<Vec<NoteId>, LibraryError> {
    let mut statement =
        transaction.prepare("SELECT note_id FROM note_tags WHERE tag_id = ?1 ORDER BY note_id")?;
    statement
        .query_map([tag_id.as_str()], |row| {
            NoteId::parse(row.get::<_, String>(0)?).map_err(invalid_column)
        })?
        .collect::<Result<Vec<_>, _>>()
        .map_err(Into::into)
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

fn purge_resource_candidates(
    transaction: &Transaction<'_>,
    note_id: &NoteId,
) -> Result<Vec<PurgeResourceCandidate>, LibraryError> {
    // `DISTINCT` turns a same-note A-B-A occurrence into one resource-entity
    // decision. The later NOT EXISTS check sees every surviving relation,
    // including Trash rows that can still be restored.
    let mut statement = transaction.prepare(
        "SELECT DISTINCT r.id, r.sha256, r.revision
         FROM note_resources nr
         JOIN resources r ON r.id = nr.resource_id
         WHERE nr.note_id = ?1 AND nr.is_associated = 1
         ORDER BY r.id",
    )?;
    statement
        .query_map([note_id.as_str()], |row| {
            Ok(PurgeResourceCandidate {
                id: ResourceId::new(row.get::<_, String>(0)?).map_err(invalid_column)?,
                sha256: BlobHash::new(row.get::<_, String>(1)?).map_err(invalid_column)?,
                revision: row.get(2)?,
            })
        })?
        .collect::<Result<Vec<_>, _>>()
        .map_err(Into::into)
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
        let valid: i64 = transaction.query_row("SELECT EXISTS(SELECT 1 FROM note_resources nr JOIN resources r ON r.id=nr.resource_id WHERE nr.note_id=?1 AND nr.resource_id=?2 AND nr.is_associated=1 AND r.deleted_time=0 AND r.mime LIKE 'image/%')", params![note_id.as_str(), requested.as_str()], |row| row.get(0))?;
        if valid == 1 {
            return Ok(Some(requested.clone()));
        }
    }
    // A selected thumbnail is independent note state, not "the most recently
    // inserted image".  This runs after the relation replacement above, so a
    // stale/deleted cover naturally falls through to the first still-valid
    // image, while an existing valid cover survives ordinary body snapshots.
    let current = transaction
        .query_row(
            "SELECT selected_thumbnail_id FROM notes WHERE id = ?1",
            [note_id.as_str()],
            |row| row.get::<_, Option<String>>(0),
        )
        .optional()?
        .flatten()
        .map(ResourceId::new)
        .transpose()?;
    if let Some(current) = current {
        let valid: i64 = transaction.query_row("SELECT EXISTS(SELECT 1 FROM note_resources nr JOIN resources r ON r.id=nr.resource_id WHERE nr.note_id=?1 AND nr.resource_id=?2 AND nr.is_associated=1 AND r.deleted_time=0 AND r.mime LIKE 'image/%')", params![note_id.as_str(), current.as_str()], |row| row.get(0))?;
        if valid == 1 {
            return Ok(Some(current));
        }
    }
    transaction.query_row("SELECT nr.resource_id FROM note_resources nr JOIN resources r ON r.id = nr.resource_id WHERE nr.note_id = ?1 AND nr.is_associated = 1 AND r.deleted_time = 0 AND r.mime LIKE 'image/%' ORDER BY nr.position, nr.resource_id LIMIT 1", [note_id.as_str()], |row| row.get::<_, String>(0)).optional()?.map(|id| ResourceId::new(id).map_err(LibraryError::from)).transpose()
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

/// The relation table is rebuilt on ordinary snapshots, so this is explicitly
/// identity-idempotent: preserving an already indexed ResourceId/SHA/version
/// must never turn a routine note save into another extraction request.
fn queue_derived_text_for_note(
    transaction: &Transaction<'_>,
    note_id: &NoteId,
    now: i64,
) -> Result<(), LibraryError> {
    transaction.execute(
        "INSERT INTO derived_text_jobs(resource_id,sha256,extractor_version,state,failure,attempts,updated_time)
         SELECT DISTINCT r.id,r.sha256,?2,'pending',NULL,0,?3
         FROM note_resources nr JOIN resources r ON r.id=nr.resource_id
         WHERE nr.note_id=?1 AND nr.is_associated=1 AND r.deleted_time=0
           AND (r.mime='application/pdf' OR r.mime LIKE 'image/%')
         ON CONFLICT(resource_id) DO UPDATE SET sha256=excluded.sha256,extractor_version=excluded.extractor_version,state='pending',failure=NULL,attempts=0,updated_time=excluded.updated_time
         WHERE derived_text_jobs.sha256<>excluded.sha256 OR derived_text_jobs.extractor_version<>excluded.extractor_version",
        params![note_id.as_str(), DERIVED_TEXT_EXTRACTOR_VERSION, now],
    )?;
    Ok(())
}

fn add_range(
    predicates: &mut Vec<String>,
    values: &mut Vec<rusqlite::types::Value>,
    column: &str,
    range: &crate::DateRange,
) {
    if let Some(start) = range.start {
        predicates.push(format!("{column} >= ?"));
        values.push(rusqlite::types::Value::Integer(start));
    }
    if let Some(end) = range.end {
        predicates.push(format!("{column} <= ?"));
        values.push(rusqlite::types::Value::Integer(end));
    }
}

fn fts_literal(value: &str) -> String {
    format!("\"{}\"", value.replace('"', "\"\""))
}

fn like_contains(value: &str) -> String {
    format!(
        "%{}%",
        value
            .replace('\\', "\\\\")
            .replace('%', "\\%")
            .replace('_', "\\_")
    )
}

/// A filename-only provenance lookup mirrors the ordinary-term candidate
/// branch.  In particular, Latin terms remain token-boundary FTS matches,
/// rather than silently becoming substring `LIKE` matches while selecting an
/// attachment for the UI.
fn filename_provenance_subquery(term: &str) -> (String, rusqlite::types::Value) {
    let (table, predicate, value) = if contains_short_cjk(term) {
        (
            "resource_filename_trigram",
            "rf.filename LIKE ? ESCAPE '\\'",
            rusqlite::types::Value::Text(like_contains(term)),
        )
    } else if contains_cjk(term) {
        (
            "resource_filename_trigram",
            "resource_filename_trigram MATCH ?",
            rusqlite::types::Value::Text(fts_literal(term)),
        )
    } else {
        (
            "resource_filename_unicode",
            "resource_filename_unicode MATCH ?",
            rusqlite::types::Value::Text(fts_literal(term)),
        )
    };
    (
        format!(
            "(SELECT nr.resource_id FROM note_resources nr JOIN resources r ON r.id=nr.resource_id JOIN resource_search_rows rsr ON rsr.resource_id=r.id JOIN {table} rf ON rf.rowid=rsr.fts_rowid WHERE nr.note_id=n.id AND nr.is_associated=1 AND r.deleted_time=0 AND {predicate} ORDER BY nr.position,nr.resource_id LIMIT 1)"
        ),
        value,
    )
}

fn derived_text_provenance_subquery(term: &str) -> (String, Vec<rusqlite::types::Value>) {
    let (table, predicate, value) = if contains_short_cjk(term) {
        (
            "derived_text_trigram",
            "dt.text LIKE ? ESCAPE '\\'",
            rusqlite::types::Value::Text(like_contains(term)),
        )
    } else if contains_cjk(term) {
        (
            "derived_text_trigram",
            "derived_text_trigram MATCH ?",
            rusqlite::types::Value::Text(fts_literal(term)),
        )
    } else {
        (
            "derived_text_unicode",
            "derived_text_unicode MATCH ?",
            rusqlite::types::Value::Text(fts_literal(term)),
        )
    };
    (
        format!(
            "(SELECT nr.resource_id FROM note_resources nr JOIN resources r ON r.id=nr.resource_id JOIN derived_text_rows dr ON dr.resource_id=r.id JOIN derived_text_jobs dj ON dj.resource_id=r.id JOIN {table} dt ON dt.rowid=dr.fts_rowid WHERE nr.note_id=n.id AND nr.is_associated=1 AND r.deleted_time=0 AND r.sha256=dr.sha256 AND dj.state='indexed' AND dj.sha256=r.sha256 AND dj.extractor_version=dr.extractor_version AND dj.extractor_version=? AND {predicate} ORDER BY nr.position,nr.resource_id LIMIT 1)"
        ),
        vec![
            rusqlite::types::Value::Text(DERIVED_TEXT_EXTRACTOR_VERSION.into()),
            value,
        ],
    )
}

fn contains_short_cjk(value: &str) -> bool {
    contains_cjk(value) && value.chars().count() < 3
}
fn contains_cjk(value: &str) -> bool {
    let cjk = value
        .chars()
        .filter(|ch| matches!(*ch as u32, 0x3400..=0x4dbf | 0x4e00..=0x9fff | 0xf900..=0xfaff))
        .count();
    cjk > 0
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
    #[cfg(unix)]
    use std::os::unix::fs::PermissionsExt;
    use std::sync::mpsc::RecvTimeoutError;
    use std::time::Duration;
    use tempfile::tempdir;

    #[test]
    fn derived_text_is_searchable_only_while_its_exact_live_attachment_is_associated() {
        // This is deliberately injected extractor output, not a claim that an
        // OCR/PDF bridge exists. It establishes the D3a hand-off contract.
        let profile = tempdir().expect("temporary profile");
        let repository = LibraryRepository::open(profile.path().join("library.sqlite"))
            .expect("open repository");
        let resource = repository
            .import_resource(
                b"opaque pdf bytes",
                "evidence.pdf",
                "application/pdf",
                "pdf",
            )
            .expect("import opaque attachment");
        let note = repository
            .create_note(CreateNote {
                title: "unrelated title".into(),
                notebook_id: None,
                document: CanonicalDocument::from_blocks(vec![
                    crate::document::Block::Attachment {
                        resource_id: resource.clone(),
                        filename: "evidence.pdf".into(),
                        media_type: "application/pdf".into(),
                    },
                ]),
            })
            .expect("associate attachment");
        let job = repository
            .take_derived_text_jobs(1)
            .expect("take pending extraction")
            .pop()
            .expect("associated PDF is pending");
        assert_eq!(job.resource_id, resource);
        assert!(
            repository
                .publish_derived_text(&job, "甲乙丙 alpha beta contract")
                .expect("publish exact synthetic text")
        );

        for term in ["甲", "甲乙", "甲乙丙", "\"alpha beta\""] {
            let hits = repository
                .search(SearchQuery::parse(term))
                .expect("search derived text");
            assert_eq!(hits.len(), 1, "{term}");
            assert_eq!(hits[0].note.id, note.id);
            assert_eq!(hits[0].matched_resource, Some(resource.clone()));
        }

        // `replace_note_resources` deletes then reinserts rows. An ordinary
        // save retaining this attachment must not requeue/erase the exact
        // completed projection merely because of that implementation detail.
        let retained = repository
            .save_note(SaveNote {
                id: note.id.clone(),
                expected_revision: note.revision,
                title: "renamed without changing attachment".into(),
                document: CanonicalDocument::from_blocks(vec![
                    crate::document::Block::Attachment {
                        resource_id: resource.clone(),
                        filename: "evidence.pdf".into(),
                        media_type: "application/pdf".into(),
                    },
                ]),
                resource_ids: vec![resource.clone()],
                selected_thumbnail_id: None,
            })
            .expect("ordinary save retains exact completed attachment text");
        assert_eq!(
            repository
                .derived_text_status(&resource)
                .expect("read durable status"),
            Some(DerivedTextStatus::Indexed { attempts: 0 })
        );
        assert_eq!(
            repository
                .search(SearchQuery::parse("甲乙丙"))
                .expect("search after ordinary save")[0]
                .matched_resource,
            Some(resource.clone())
        );

        let detached = repository
            .save_note(SaveNote {
                id: note.id.clone(),
                expected_revision: retained.revision,
                title: retained.title,
                document: CanonicalDocument::default(),
                resource_ids: vec![],
                selected_thumbnail_id: None,
            })
            .expect("detach without touching resource bytes");
        assert!(
            repository
                .search(SearchQuery::parse("甲乙丙"))
                .expect("search after detach")
                .is_empty()
        );
        assert_eq!(detached.resource_ids, Vec::<ResourceId>::new());
    }

    #[test]
    fn derived_text_failure_is_visible_retryable_and_never_requires_a_blob_read() {
        let profile = tempdir().expect("temporary profile");
        let repository = LibraryRepository::open(profile.path().join("library.sqlite"))
            .expect("open repository");
        let resource = repository
            .import_resource(b"opaque image bytes", "scan.png", "image/png", "png")
            .expect("import image");
        repository
            .create_note(CreateNote {
                title: "scan owner".into(),
                notebook_id: None,
                document: CanonicalDocument::from_blocks(vec![
                    crate::document::Block::Attachment {
                        resource_id: resource.clone(),
                        filename: "scan.png".into(),
                        media_type: "image/png".into(),
                    },
                ]),
            })
            .expect("associate image");
        let reads = repository.observe_resource_reads();
        let job = repository.take_derived_text_jobs(1).unwrap().pop().unwrap();
        assert!(
            repository
                .fail_derived_text(&job, DerivedTextFailure::Unavailable)
                .unwrap()
        );
        assert_eq!(
            repository.derived_text_status(&resource).unwrap(),
            Some(DerivedTextStatus::Failed {
                failure: DerivedTextFailure::Unavailable,
                attempts: 1
            })
        );
        assert!(repository.retry_derived_text(&job).unwrap());
        assert_eq!(
            repository.derived_text_status(&resource).unwrap(),
            Some(DerivedTextStatus::Pending { attempts: 1 })
        );
        assert!(matches!(
            reads.recv_timeout(Duration::from_millis(20)),
            Err(RecvTimeoutError::Timeout)
        ));
    }

    #[test]
    fn stale_hash_or_extractor_version_cannot_publish_or_leak_a_derived_hit() {
        let profile = tempdir().unwrap();
        let repository = LibraryRepository::open(profile.path().join("library.sqlite")).unwrap();
        let resource = repository
            .import_resource(b"opaque PDF", "sealed.pdf", "application/pdf", "pdf")
            .unwrap();
        repository
            .create_note(CreateNote {
                title: "owner".into(),
                notebook_id: None,
                document: CanonicalDocument::from_blocks(vec![
                    crate::document::Block::Attachment {
                        resource_id: resource.clone(),
                        filename: "sealed.pdf".into(),
                        media_type: "application/pdf".into(),
                    },
                ]),
            })
            .unwrap();
        let stale_job = repository.take_derived_text_jobs(1).unwrap().pop().unwrap();
        {
            let connection = repository.connection.lock().unwrap();
            let replacement = "b".repeat(64);
            connection.execute("INSERT INTO resource_blobs(sha256,size,mime,relative_path,created_time,revision) VALUES(?1,1,'application/pdf','unused',0,1)", [&replacement]).unwrap();
            connection
                .execute(
                    "UPDATE resources SET sha256=?2 WHERE id=?1",
                    params![resource.as_str(), replacement],
                )
                .unwrap();
        }
        assert!(
            !repository
                .publish_derived_text(&stale_job, "must not publish")
                .unwrap()
        );
        assert!(repository.take_derived_text_jobs(1).unwrap().is_empty());

        // Simulate a future extractor-version rollover with stale FTS rows:
        // the current search contract still excludes the old projection.
        {
            let connection = repository.connection.lock().unwrap();
            connection.execute("UPDATE derived_text_jobs SET sha256=(SELECT sha256 FROM resources WHERE id=?1), extractor_version='obsolete-v0',state='indexed' WHERE resource_id=?1", [resource.as_str()]).unwrap();
            connection.execute("INSERT INTO derived_text_rows(resource_id,sha256,extractor_version) VALUES(?1,(SELECT sha256 FROM resources WHERE id=?1),'obsolete-v0')", [resource.as_str()]).unwrap();
            let rowid: i64 = connection
                .query_row(
                    "SELECT fts_rowid FROM derived_text_rows WHERE resource_id=?1",
                    [resource.as_str()],
                    |row| row.get(0),
                )
                .unwrap();
            connection.execute("INSERT INTO derived_text_unicode(rowid,resource_id,text) VALUES(?1,?2,'obsolete secret')", params![rowid, resource.as_str()]).unwrap();
            connection.execute("INSERT INTO derived_text_trigram(rowid,resource_id,text) VALUES(?1,?2,'obsolete secret')", params![rowid, resource.as_str()]).unwrap();
        }
        assert!(
            repository
                .search(SearchQuery::parse("obsolete"))
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn trashed_and_purged_attachment_owner_cannot_leave_derived_text_live() {
        let profile = tempdir().unwrap();
        let repository = LibraryRepository::open(profile.path().join("library.sqlite")).unwrap();
        let resource = repository
            .import_resource(b"opaque", "final.pdf", "application/pdf", "pdf")
            .unwrap();
        let note = repository
            .create_note(CreateNote {
                title: "owner".into(),
                notebook_id: None,
                document: CanonicalDocument::from_blocks(vec![
                    crate::document::Block::Attachment {
                        resource_id: resource.clone(),
                        filename: "final.pdf".into(),
                        media_type: "application/pdf".into(),
                    },
                ]),
            })
            .unwrap();
        let job = repository.take_derived_text_jobs(1).unwrap().pop().unwrap();
        assert!(
            repository
                .publish_derived_text(&job, "purge-me text")
                .unwrap()
        );
        repository.trash_note(&note.id).unwrap();
        assert!(
            repository
                .search(SearchQuery::parse("purge-me"))
                .unwrap()
                .is_empty()
        );
        repository.purge_note(&note.id).unwrap();
        assert_eq!(repository.derived_text_status(&resource).unwrap(), None);
    }

    #[test]
    fn derived_text_queue_prioritizes_pdf_then_new_images_ahead_of_historical_backlog() {
        let profile = tempdir().unwrap();
        let repository = LibraryRepository::open(profile.path().join("library.sqlite")).unwrap();
        let old_image = repository
            .import_resource(b"image", "old.png", "image/png", "png")
            .unwrap();
        repository
            .create_note(CreateNote {
                title: "image owner".into(),
                notebook_id: None,
                document: CanonicalDocument::from_blocks(vec![
                    crate::document::Block::Attachment {
                        resource_id: old_image.clone(),
                        filename: "old.png".into(),
                        media_type: "image/png".into(),
                    },
                ]),
            })
            .unwrap();
        let pdf = repository
            .import_resource(b"pdf", "urgent.pdf", "application/pdf", "pdf")
            .unwrap();
        repository
            .create_note(CreateNote {
                title: "PDF owner".into(),
                notebook_id: None,
                document: CanonicalDocument::from_blocks(vec![
                    crate::document::Block::Attachment {
                        resource_id: pdf.clone(),
                        filename: "urgent.pdf".into(),
                        media_type: "application/pdf".into(),
                    },
                ]),
            })
            .unwrap();

        let new_image = repository
            .import_resource(b"new image", "new.png", "image/png", "png")
            .unwrap();
        repository
            .create_note(CreateNote {
                title: "new image owner".into(),
                notebook_id: None,
                document: CanonicalDocument::from_blocks(vec![
                    crate::document::Block::Attachment {
                        resource_id: new_image.clone(),
                        filename: "new.png".into(),
                        media_type: "image/png".into(),
                    },
                ]),
            })
            .unwrap();

        let jobs = repository.take_derived_text_jobs(3).unwrap();
        assert_eq!(jobs.len(), 3);
        assert_eq!(jobs[0].resource_id, pdf);
        assert_eq!(jobs[1].resource_id, new_image);
        assert_eq!(jobs[2].resource_id, old_image);
    }

    #[test]
    fn v10_reopen_requeues_live_attachment_once_when_extractor_version_changes() {
        let profile = tempdir().unwrap();
        let database = profile.path().join("library.sqlite");
        let repository = LibraryRepository::open(&database).unwrap();
        let resource = repository
            .import_resource(b"opaque", "upgrade.pdf", "application/pdf", "pdf")
            .unwrap();
        repository
            .create_note(CreateNote {
                title: "owner".into(),
                notebook_id: None,
                document: CanonicalDocument::from_blocks(vec![
                    crate::document::Block::Attachment {
                        resource_id: resource.clone(),
                        filename: "upgrade.pdf".into(),
                        media_type: "application/pdf".into(),
                    },
                ]),
            })
            .unwrap();
        let job = repository.take_derived_text_jobs(1).unwrap().pop().unwrap();
        repository
            .publish_derived_text(&job, "version rollover text")
            .unwrap();
        {
            let connection = repository.connection.lock().unwrap();
            connection.execute("UPDATE derived_text_jobs SET extractor_version='old-extractor',state='indexed' WHERE resource_id=?1", [resource.as_str()]).unwrap();
            connection.execute("UPDATE settings SET value='old-extractor' WHERE key='derived-text.extractor-version'", []).unwrap();
        }
        drop(repository);
        let reopened = LibraryRepository::open(&database).unwrap();
        let requeued = reopened.take_derived_text_jobs(1).unwrap();
        assert_eq!(requeued.len(), 1);
        assert_eq!(requeued[0].resource_id, resource);
        assert_eq!(
            requeued[0].extractor_version,
            DERIVED_TEXT_EXTRACTOR_VERSION
        );
        assert_eq!(
            reopened.derived_text_status(&resource).unwrap(),
            Some(DerivedTextStatus::Pending { attempts: 0 })
        );
        drop(reopened);
        // The sentinel now matches; a subsequent open is the bounded hot path
        // and does not reset the pending job's attempt/status.
        let reopened = LibraryRepository::open(&database).unwrap();
        assert_eq!(
            reopened.derived_text_status(&resource).unwrap(),
            Some(DerivedTextStatus::Pending { attempts: 0 })
        );
    }

    #[test]
    fn v10_reopen_requeues_a_pre_vision_failed_image_from_the_v1_sentinel() {
        let profile = tempdir().unwrap();
        let database = profile.path().join("library.sqlite");
        let repository = LibraryRepository::open(&database).unwrap();
        let resource = repository
            .import_resource(b"opaque", "upgrade.png", "image/png", "png")
            .unwrap();
        repository
            .create_note(CreateNote {
                title: "owner".into(),
                notebook_id: None,
                document: CanonicalDocument::from_blocks(vec![
                    crate::document::Block::Attachment {
                        resource_id: resource.clone(),
                        filename: "upgrade.png".into(),
                        media_type: "image/png".into(),
                    },
                ]),
            })
            .unwrap();
        let job = repository.take_derived_text_jobs(1).unwrap().pop().unwrap();
        assert!(repository
            .fail_derived_text(&job, DerivedTextFailure::Unsupported)
            .unwrap());
        {
            let connection = repository.connection.lock().unwrap();
            connection
                .execute(
                    "UPDATE derived_text_jobs SET extractor_version='pdfkit-selectable-text-v1' WHERE resource_id=?1",
                    [resource.as_str()],
                )
                .unwrap();
            connection
                .execute(
                    "UPDATE settings SET value='pdfkit-selectable-text-v1' WHERE key='derived-text.extractor-version'",
                    [],
                )
                .unwrap();
        }
        drop(repository);

        let reopened = LibraryRepository::open(&database).unwrap();
        assert_eq!(
            reopened.derived_text_status(&resource).unwrap(),
            Some(DerivedTextStatus::Pending { attempts: 0 })
        );
        let requeued = reopened.take_derived_text_jobs(1).unwrap();
        assert_eq!(requeued.len(), 1);
        assert_eq!(requeued[0].resource_id, resource);
        assert_eq!(requeued[0].extractor_version, DERIVED_TEXT_EXTRACTOR_VERSION);
    }

    #[cfg(unix)]
    fn queue_orphan_blob_then_stage_same_bytes(
        repository: &LibraryRepository,
        profile: &std::path::Path,
        bytes: &[u8],
        title: &str,
        mime: &str,
        extension: &str,
    ) -> StagedResource {
        // This is the concrete v6/Task-5 reverse ordering: a successful
        // permanent purge can leave a durable GC queue entry when its
        // descriptor-relative unlink is temporarily refused. An invisible
        // stage of the same content must not turn that queued cleanup into a
        // future dangling resource row.
        let original = repository
            .import_resource(bytes, title, mime, extension)
            .expect("persist original resource");
        let note = repository
            .create_note(CreateNote {
                title: "GC owner".into(),
                notebook_id: None,
                document: CanonicalDocument::from_blocks(vec![
                    crate::document::Block::Attachment {
                        resource_id: original.clone(),
                        filename: title.into(),
                        media_type: mime.into(),
                    },
                ]),
            })
            .expect("create resource owner");
        let hash = repository
            .resource_metadata(&original)
            .expect("load original metadata")
            .expect("original metadata exists")
            .sha256;
        let blobs = profile.join("resources").join("blobs");
        assert!(blobs.join(hash.as_str()).exists());
        repository.trash_note(&note.id).expect("trash owner");
        std::fs::set_permissions(&blobs, std::fs::Permissions::from_mode(0o500))
            .expect("temporarily refuse physical GC unlink");
        repository
            .purge_note(&note.id)
            .expect("purge commits its durable queue before physical cleanup");
        assert!(repository.resource_metadata(&original).unwrap().is_none());
        // `put_reader` always creates a descriptor-relative temporary first,
        // even when an equal final blob is already present. Restore writes
        // only after the purge's automatic drain has failed, then keep the
        // durable queue for the separately opened recovery repository below.
        std::fs::set_permissions(&blobs, std::fs::Permissions::from_mode(0o700))
            .expect("restore staging write permission");
        let staged = repository
            .stage_resource(bytes, title, mime, extension)
            .expect("the still-present uncommitted blob can be staged");
        assert_eq!(staged.sha256(), &hash);
        staged
    }

    #[cfg(unix)]
    fn assert_staged_blob_was_removed(result: Result<(), LibraryError>) {
        assert!(
            matches!(result, Err(LibraryError::Resource(ResourceError::Io(error))) if error.kind() == std::io::ErrorKind::NotFound),
            "GC winning before staged publication must fail closed before any SQLite row is inserted"
        );
    }

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

    #[test]
    fn staged_resource_snapshot_rolls_back_without_events_then_publishes_one_commit() {
        // Task 5 must not make a resource visible in its own transaction and
        // then try to associate it in a second note snapshot.  If the latter
        // fails, no subscriber may observe an orphan resource/sync event and
        // every SQLite relation/outbox row must remain at its pre-insert
        // generation.  The success half also catches a later regression back
        // to two independently published commits.
        use std::sync::mpsc::RecvTimeoutError;
        use std::time::Duration;

        let profile = tempdir().expect("temporary profile");
        let repository = LibraryRepository::open(profile.path().join("library.sqlite"))
            .expect("open repository");
        let note = repository
            .create_note(CreateNote {
                title: "atomic resource".into(),
                notebook_id: None,
                document: crate::CanonicalDocument::default(),
            })
            .expect("create note");
        let events = repository.subscribe();
        let before_outbox = repository.outbox_count().expect("outbox count");
        let staged = repository
            .stage_resource(
                b"%PDF-1.7\natomic\n%%EOF\n",
                "atomic.pdf",
                "application/pdf",
                "pdf",
            )
            .expect("stage opaque blob without SQLite publication");
        let resource_id = staged.resource_id().clone();
        let document =
            crate::CanonicalDocument::from_blocks(vec![crate::document::Block::Attachment {
                resource_id: resource_id.clone(),
                filename: "atomic.pdf".into(),
                media_type: "application/pdf".into(),
            }]);
        let snapshot = SaveNote {
            id: note.id.clone(),
            expected_revision: note.revision,
            title: note.title.clone(),
            resource_ids: vec![resource_id.clone()],
            document,
            selected_thumbnail_id: None,
        };

        repository.fail_next_staged_resource_snapshot_for_test(LibraryError::InvalidSnapshot);
        assert!(matches!(
            repository.commit_staged_resource_snapshot(snapshot.clone(), None, &staged),
            Err(LibraryError::InvalidSnapshot)
        ));
        assert!(matches!(
            events.recv_timeout(Duration::from_millis(30)),
            Err(RecvTimeoutError::Timeout)
        ));
        assert_eq!(repository.outbox_count().unwrap(), before_outbox);
        {
            let connection = repository.connection.lock().unwrap();
            let resources: i64 = connection
                .query_row(
                    "SELECT count(*) FROM resources WHERE id=?1",
                    [resource_id.as_str()],
                    |row| row.get(0),
                )
                .unwrap();
            let blobs: i64 = connection
                .query_row(
                    "SELECT count(*) FROM resource_blobs WHERE sha256=?1",
                    [staged.sha256().as_str()],
                    |row| row.get(0),
                )
                .unwrap();
            let relations: i64 = connection
                .query_row(
                    "SELECT count(*) FROM note_resources WHERE note_id=?1",
                    [note.id.as_str()],
                    |row| row.get(0),
                )
                .unwrap();
            assert_eq!((resources, blobs, relations), (0, 0, 0));
        }

        let committed = repository
            .commit_staged_resource_snapshot(snapshot, None, &staged)
            .expect("one transaction should publish metadata and note relation together");
        assert_eq!(committed.note.resource_ids, vec![resource_id.clone()]);
        assert_eq!(
            [
                events.recv().unwrap(),
                events.recv().unwrap(),
                events.recv().unwrap(),
                events.recv().unwrap(),
            ],
            [
                LibraryEvent::NoteProjectionChanged(note.id.clone()),
                LibraryEvent::SearchProjectionQueued(note.id.clone()),
                LibraryEvent::SyncQueued(EntityRef::Resource(resource_id)),
                LibraryEvent::SyncQueued(EntityRef::Note(note.id)),
            ]
        );
        assert_eq!(repository.outbox_count().unwrap(), before_outbox + 2);
    }

    #[cfg(unix)]
    #[test]
    fn gc_winning_before_direct_staged_metadata_commit_never_publishes_a_missing_blob() {
        // Mutation-sensitive v6 regression: removing the staged-byte
        // verification below makes this commit succeed after a second
        // repository drains the old purge queue, leaving `resources` pointed
        // at a path that no longer exists.
        let profile = tempdir().expect("temporary profile");
        let database = profile.path().join("library.sqlite");
        let repository = LibraryRepository::open(&database).expect("open repository");
        let bytes = b"%PDF-1.7\nqueued-stage\n%%EOF\n";
        let staged = queue_orphan_blob_then_stage_same_bytes(
            &repository,
            profile.path(),
            bytes,
            "queued-stage.pdf",
            "application/pdf",
            "pdf",
        );
        let staged_id = staged.resource_id().clone();
        let staged_hash = staged.sha256().clone();
        let events = repository.subscribe();
        let before_outbox = repository.outbox_count().expect("outbox count");

        // A separately opened repository performs the persisted GC recovery
        // before this invisible stage is committed.
        let gc_runner = LibraryRepository::open(database.clone()).expect("open drains queued GC");
        assert!(
            !profile
                .path()
                .join("resources")
                .join("blobs")
                .join(staged_hash.as_str())
                .exists(),
            "the deterministic reverse ordering must remove the old physical blob"
        );
        drop(gc_runner);

        assert_staged_blob_was_removed(repository.commit_staged_resource_metadata(&staged));
        assert!(repository.resource_metadata(&staged_id).unwrap().is_none());
        assert_eq!(repository.outbox_count().unwrap(), before_outbox);
        assert!(matches!(
            events.recv_timeout(Duration::from_millis(30)),
            Err(RecvTimeoutError::Timeout)
        ));
        let connection = repository.connection.lock().expect("repository connection");
        let blob_rows: i64 = connection
            .query_row(
                "SELECT count(*) FROM resource_blobs WHERE sha256 = ?1",
                [staged_hash.as_str()],
                |row| row.get(0),
            )
            .expect("count no dangling blob metadata");
        assert_eq!(blob_rows, 0);
        drop(connection);

        // A fresh stage may safely retry after the failed, zero-publication
        // attempt. If it succeeds, the normal durable reader must prove that
        // its byte/hash fact exists rather than trusting metadata alone.
        let retry = repository
            .stage_resource(bytes, "queued-stage.pdf", "application/pdf", "pdf")
            .expect("fresh stage recreates the deleted content-addressed blob");
        let retry_id = retry.resource_id().clone();
        repository
            .commit_staged_resource_metadata(&retry)
            .expect("retry direct metadata commit");
        assert_eq!(
            repository
                .read_resource_bytes(&retry_id)
                .expect("read retry resource")
                .expect("metadata and verified bytes both exist"),
            bytes
        );
    }

    #[cfg(unix)]
    #[test]
    fn gc_winning_before_staged_snapshot_commit_never_publishes_a_missing_relation() {
        // The note-snapshot route is the production Task-5 insertion path.
        // It needs the same verification before resource metadata,
        // note_resources, search work, outbox, or events can become visible.
        let profile = tempdir().expect("temporary profile");
        let database = profile.path().join("library.sqlite");
        let repository = LibraryRepository::open(&database).expect("open repository");
        let bytes = b"%PDF-1.7\nqueued-snapshot\n%%EOF\n";
        let staged = queue_orphan_blob_then_stage_same_bytes(
            &repository,
            profile.path(),
            bytes,
            "queued-snapshot.pdf",
            "application/pdf",
            "pdf",
        );
        let target = repository
            .create_note(CreateNote {
                title: "snapshot target".into(),
                notebook_id: None,
                document: CanonicalDocument::default(),
            })
            .expect("create target note");
        let staged_id = staged.resource_id().clone();
        let staged_hash = staged.sha256().clone();
        let snapshot = SaveNote {
            id: target.id.clone(),
            expected_revision: target.revision,
            title: target.title.clone(),
            document: CanonicalDocument::from_blocks(vec![crate::document::Block::Attachment {
                resource_id: staged_id.clone(),
                filename: "queued-snapshot.pdf".into(),
                media_type: "application/pdf".into(),
            }]),
            resource_ids: vec![staged_id.clone()],
            selected_thumbnail_id: None,
        };
        let events = repository.subscribe();
        let before_outbox = repository.outbox_count().expect("outbox count");
        let gc_runner = LibraryRepository::open(database.clone()).expect("open drains queued GC");
        drop(gc_runner);

        let result = repository
            .commit_staged_resource_snapshot(snapshot.clone(), None, &staged)
            .map(|_| ());
        assert_staged_blob_was_removed(result);
        assert!(repository.resource_metadata(&staged_id).unwrap().is_none());
        assert_eq!(repository.outbox_count().unwrap(), before_outbox);
        assert!(matches!(
            events.recv_timeout(Duration::from_millis(30)),
            Err(RecvTimeoutError::Timeout)
        ));
        let retained = repository
            .load_note(&target.id)
            .expect("load target after failed transaction")
            .expect("target remains");
        assert_eq!(retained.revision, target.revision);
        assert!(retained.resource_ids.is_empty());
        let connection = repository.connection.lock().expect("repository connection");
        let relations: i64 = connection
            .query_row(
                "SELECT count(*) FROM note_resources WHERE note_id = ?1",
                [target.id.as_str()],
                |row| row.get(0),
            )
            .expect("count no dangling relation");
        let blob_rows: i64 = connection
            .query_row(
                "SELECT count(*) FROM resource_blobs WHERE sha256 = ?1",
                [staged_hash.as_str()],
                |row| row.get(0),
            )
            .expect("count no dangling blob metadata");
        assert_eq!((relations, blob_rows), (0, 0));
        drop(connection);

        let retry = repository
            .stage_resource(bytes, "queued-snapshot.pdf", "application/pdf", "pdf")
            .expect("fresh retry stage");
        let retry_id = retry.resource_id().clone();
        let committed = repository
            .commit_staged_resource_snapshot(
                SaveNote {
                    id: target.id.clone(),
                    expected_revision: target.revision,
                    title: target.title,
                    document: CanonicalDocument::from_blocks(vec![
                        crate::document::Block::Attachment {
                            resource_id: retry_id.clone(),
                            filename: "queued-snapshot.pdf".into(),
                            media_type: "application/pdf".into(),
                        },
                    ]),
                    resource_ids: vec![retry_id.clone()],
                    selected_thumbnail_id: None,
                },
                None,
                &retry,
            )
            .expect("retry snapshot commit");
        assert_eq!(committed.note.resource_ids, vec![retry_id.clone()]);
        assert_eq!(
            repository
                .read_resource_bytes(&retry_id)
                .expect("read retry resource")
                .expect("metadata and verified bytes both exist"),
            bytes
        );
    }

    #[test]
    fn staged_snapshot_returns_the_new_thumbnail_after_replacing_an_old_cover() {
        // The shell used to infer its card thumbnail from the projection that
        // existed before the resource transaction.  When the snapshot drops
        // old cover A and inserts B, that made the first rendered card show A
        // even though SQLite had already selected B. The transaction outcome
        // is now the sole same-frame source of truth.
        let profile = tempdir().expect("temporary profile");
        let repository = LibraryRepository::open(profile.path().join("library.sqlite"))
            .expect("open repository");
        let old = repository
            .import_resource(b"old image", "old.png", "image/png", "png")
            .expect("persist old image");
        let note = repository
            .create_note(CreateNote {
                title: "cover switch".into(),
                notebook_id: None,
                document: crate::CanonicalDocument::from_blocks(vec![
                    crate::document::Block::Image {
                        resource_id: old,
                        alt: "old".into(),
                        presentation: crate::document::ImagePresentation::default(),
                    },
                ]),
            })
            .expect("create note with old cover");
        let staged = repository
            .stage_resource(b"new image", "new.png", "image/png", "png")
            .expect("stage new image");
        let replacement = staged.resource_id().clone();
        let document = crate::CanonicalDocument::from_blocks(vec![crate::document::Block::Image {
            resource_id: replacement.clone(),
            alt: "new".into(),
            presentation: crate::document::ImagePresentation::default(),
        }]);

        let committed = repository
            .commit_staged_resource_snapshot(
                SaveNote {
                    id: note.id,
                    expected_revision: note.revision,
                    title: note.title,
                    document,
                    resource_ids: vec![replacement.clone()],
                    selected_thumbnail_id: None,
                },
                None,
                &staged,
            )
            .expect("commit replacement image");

        assert_eq!(committed.selected_thumbnail_id, Some(replacement));
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

fn row_to_stack(row: &rusqlite::Row<'_>) -> rusqlite::Result<Stack> {
    Ok(Stack {
        id: StackId::parse(row.get::<_, String>(0)?).map_err(invalid_column)?,
        title: row.get(1)?,
        revision: row.get(2)?,
    })
}

fn row_to_tag(row: &rusqlite::Row<'_>) -> rusqlite::Result<Tag> {
    Ok(Tag {
        id: TagId::parse(row.get::<_, String>(0)?).map_err(invalid_column)?,
        title: row.get(1)?,
        revision: row.get(2)?,
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
