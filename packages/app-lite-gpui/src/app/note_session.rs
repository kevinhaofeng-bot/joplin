//! Retained editable note session for the library route.
//!
//! The UI owns a session entity rather than a throw-away read-only canvas: it
//! retains title input, `EditorCore`, local-save timing, and the last durable
//! revision as one generation-scoped unit.  Repository calls stay in core.

use super::save_coordinator::{FlushReason, SaveClock, SaveCoordinator, SaveState, SaveWork};
use crate::native_editor::chrome::TitleInput;
use crate::native_editor::codec::{
    export_canonical_with_resources, import_canonical_with_resources,
};
use crate::native_editor::core::{EditorCore, ResourceInsertAnchor};
use crate::native_editor::images::{
    EncodedImagePayload, ImagePayload, ResourceImport, ResourceKind, ResourceSource,
    inspect_persisted_image,
};
use crate::native_editor::model::{BlockContent, Document, NodeId, Selection};
use crate::native_editor::transaction::Transaction;
use app_lite_core::{
    CanonicalDocument, EditJournalEntry, JournalOwnership, LegacyJournalPayload, LibraryError,
    LibraryRepository, Note, NoteId, ResourceId, SaveNote, SavedRevision, StagedResource,
    StagedResourceSnapshotCommit,
};
use gpui::{AppContext, Context, Entity, Subscription, Task};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, VecDeque};
use std::fmt;
use std::fs::OpenOptions;
use std::io::{Read, Write};
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::sync::Arc;
#[cfg(test)]
use std::sync::Mutex;
use std::time::Duration;

#[cfg(test)]
use futures::channel::oneshot;

const JOURNAL_SCHEMA_VERSION: u8 = 2;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct SaveError(String);

impl SaveError {
    fn new(message: impl Into<String>) -> Self {
        Self(message.into())
    }
}

impl fmt::Display for SaveError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for SaveError {}

impl From<LibraryError> for SaveError {
    fn from(error: LibraryError) -> Self {
        Self::new(error.to_string())
    }
}

#[derive(Clone)]
struct SessionSnapshot {
    title: String,
    document: CanonicalDocument,
    resource_ids: Vec<ResourceId>,
}

/// A clone of the native document at the unmarked input boundary. It is Send
/// and immutable, which lets a worker run codec/SQLite work without ever
/// consulting a live GPUI entity (and without serializing IME candidates).
#[derive(Clone)]
struct NativeSessionSnapshot {
    title: String,
    document: crate::native_editor::model::Document,
    /// The resource identities which this retained session is allowed to
    /// reference. This is an authorization set inherited from the durable
    /// base plus IDs introduced by a validated staged insert; it is *not* the
    /// current note_resources order. The latter must always be exported from
    /// `document`, otherwise a normal Backspace/Delete of an atom can never
    /// persist.
    allowed_resource_ids: Vec<ResourceId>,
}

/// Metadata for one persisted image. The resource stream never lives here:
/// first paint receives only document geometry, then renderer residency
/// requests cause one descriptor-safe background materialization at a time.
#[derive(Clone)]
struct PreparedImageSource {
    resource_id: ResourceId,
    node_id: NodeId,
    natural_size: (u32, u32),
    /// Old canonical HTML had no presentation attributes. It gets a stable
    /// 1024×768 first frame from the codec, then the first successful visible
    /// hydration repairs *only* this node without adding a history entry.
    needs_legacy_geometry_repair: bool,
}

/// Aggregated by resource ID because a single durable blob may be referenced
/// by multiple image atoms. A worker opens/materializes the blob once, while
/// a legacy repair still knows exactly which native nodes lacked dimensions.
#[derive(Clone)]
struct PersistedImageHydration {
    resource_id: ResourceId,
    natural_size: (u32, u32),
    legacy_node_ids: Vec<NodeId>,
}

struct ImageHydrationJob {
    hydration: PersistedImageHydration,
    repository: Arc<LibraryRepository>,
    /// A sibling of the editor cache root, never the root itself. A late
    /// worker may safely write here after its session is gone; only a live UI
    /// callback is allowed to adopt the result into ImageStore's root.
    image_staging_parent: PathBuf,
    #[cfg(test)]
    gate: Option<Arc<BackgroundResourceGate>>,
}

/// A worker-owned cache staging directory. It is intentionally a sibling of
/// the editor's per-session root: `ImageStore::drop` may remove that root
/// while a descriptor copy is in flight, and no background code may recreate
/// it. Dropping a completion that cannot upgrade its weak session also removes
/// this directory without touching any live editor cache.
struct ImageHydrationStaging {
    root: PathBuf,
    source: Option<PathBuf>,
}

impl ImageHydrationStaging {
    fn new(parent: &Path) -> Self {
        Self {
            root: parent.join(format!(".hydration-{}", uuid::Uuid::new_v4().simple())),
            source: None,
        }
    }

    fn record_source(&mut self, source: PathBuf) {
        self.source = Some(source);
    }

    /// The foreground only calls this while the exact retained EditorCore is
    /// still alive. Moving a fully-synced staged file is O(1) on the sibling
    /// cache filesystem and therefore never rereads bytes on the GPUI thread.
    fn adopt_into(self, editor_root: &Path) -> std::io::Result<PathBuf> {
        let source = self.source.as_ref().ok_or_else(|| {
            std::io::Error::new(
                std::io::ErrorKind::NotFound,
                "image hydration staging has no materialized source",
            )
        })?;
        let filename = source.file_name().ok_or_else(|| {
            std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "image hydration source has no filename",
            )
        })?;
        std::fs::create_dir_all(editor_root)?;
        std::fs::set_permissions(editor_root, std::fs::Permissions::from_mode(0o700))?;
        let target = editor_root.join(filename);
        std::fs::rename(source, &target)?;
        Ok(target)
    }
}

impl Drop for ImageHydrationStaging {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

enum ImageHydrationCompletion {
    Success {
        hydration: PersistedImageHydration,
        staging: ImageHydrationStaging,
        format: gpui::ImageFormat,
        cache_natural_size: (u32, u32),
        measured_natural_size: (u32, u32),
    },
    Failure {
        hydration: PersistedImageHydration,
        error: SaveError,
    },
}

struct LegacyImageGeometryRepair {
    resource_id: String,
    node_ids: Vec<NodeId>,
    natural_size: (u32, u32),
}

/// Attachment cards need only durable metadata when a note mounts. Their
/// opaque bytes stay inside `ResourceStore` until the user explicitly opens
/// the card, so a note with many PDFs/audio files cannot hydrate a second
/// in-memory copy merely to paint a list of headers.
enum PreparedAttachmentSource {
    Ready { resource_id: ResourceId, size: u64 },
    Unavailable { resource_id: ResourceId },
}

/// A document point captured when an external resource action begins. A
/// native picker may remain open while focus changes, so completion must never
/// silently insert at a later live caret.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct InsertIntent {
    note_id: NoteId,
    anchor: ResourceInsertAnchor,
    editor_revision: u64,
}

impl InsertIntent {
    fn new(note_id: NoteId, anchor: ResourceInsertAnchor) -> Self {
        Self {
            note_id,
            editor_revision: anchor.captured_revision(),
            anchor,
        }
    }

    fn belongs_to(&self, note_id: &NoteId) -> bool {
        &self.note_id == note_id
    }
}

/// The UI captures only a small, movable source descriptor plus its tracked
/// document Selection. Expensive image inspection, hashing, fsync and SQLite
/// work all begin after this value has left the GPUI update callback.
#[derive(Debug)]
pub(crate) enum ResourceImportRequest {
    Image(ImagePayload),
    ImageCandidates(Vec<ImagePayload>),
    EncodedImage(EncodedImagePayload),
    Path(PathBuf),
    Paths(Vec<PathBuf>),
}

impl ResourceImportRequest {
    fn normalize(self) -> Result<ResourceImport, SaveError> {
        match self {
            Self::Image(payload) => ResourceImport::from_image_payload(payload)
                .map_err(|error| SaveError::new(error.to_string())),
            Self::ImageCandidates(candidates) => {
                Self::first_valid_image_candidate(candidates.into_iter().map(Ok))
            }
            Self::EncodedImage(payload) => {
                Self::first_valid_image_candidate(std::iter::once(payload.decode_bounded()))
            }
            Self::Path(path) => {
                ResourceImport::from_path(&path).map_err(|error| SaveError::new(error.to_string()))
            }
            Self::Paths(paths) => Self::first_valid_path_candidate(paths),
        }
    }

    fn first_valid_image_candidate(
        candidates: impl IntoIterator<
            Item = Result<ImagePayload, crate::native_editor::images::ResourceImportError>,
        >,
    ) -> Result<ResourceImport, SaveError> {
        let mut last_error = None;
        for candidate in candidates {
            match candidate.and_then(ResourceImport::from_image_payload) {
                Ok(import) => return Ok(import),
                Err(error) => last_error = Some(error),
            }
        }
        Err(SaveError::new(last_error.map_or_else(
            || "没有可安全解码的图片候选".to_owned(),
            |error| error.to_string(),
        )))
    }

    fn first_valid_path_candidate(paths: Vec<PathBuf>) -> Result<ResourceImport, SaveError> {
        let mut last_error = None;
        for path in paths {
            match ResourceImport::from_path(&path) {
                Ok(import) => return Ok(import),
                Err(error) => last_error = Some(error),
            }
        }
        Err(SaveError::new(last_error.map_or_else(
            || "没有可安全读取的资源路径".to_owned(),
            |error| error.to_string(),
        )))
    }
}

/// A clipboard bridge may grant ownership of a private temporary file. Keep
/// its cleanup with the background stage job so closing the session cannot
/// leave a GPUI callback doing filesystem work or leak the exact temp path.
struct TemporaryResourcePaths(Vec<PathBuf>);

impl Drop for TemporaryResourcePaths {
    fn drop(&mut self) {
        for path in &self.0 {
            let _ = std::fs::remove_file(path);
        }
    }
}

struct ResourceStageJob {
    request: ResourceImportRequest,
    repository: Arc<LibraryRepository>,
    _temporary_paths: TemporaryResourcePaths,
    #[cfg(test)]
    gate: Option<Arc<BackgroundResourceGate>>,
}

struct StagedResourceInsert {
    staged: StagedResource,
    kind: ResourceKind,
}

struct PendingStagedResourceInsert {
    staged: StagedResourceInsert,
    intent: InsertIntent,
}

/// The live editor receives the already-prevalidated structural transaction
/// before this job starts. The worker owns canonical encoding, the SQLite
/// snapshot transaction and durable image-source materialization; it never
/// reads or later reinstalls an old live editor.
struct ResourceCommitJob {
    note_id: NoteId,
    expected_revision: i64,
    journal_ownership: Option<JournalOwnership>,
    repository: Arc<LibraryRepository>,
    staged: StagedResourceInsert,
    snapshot: NativeSessionSnapshot,
    /// The dirty generation created by the optimistic resource transaction.
    /// A later input generation must survive this worker's completion.
    resource_generation: i64,
    image_materialization_root: Option<PathBuf>,
    #[cfg(test)]
    gate: Option<Arc<BackgroundResourceGate>>,
    #[cfg(test)]
    injected_failure: Option<SaveError>,
}

struct ResourceCommitSuccess {
    committed: StagedResourceSnapshotCommit,
    resource_id: ResourceId,
    kind: ResourceKind,
    snapshot: SessionSnapshot,
    resource_generation: i64,
    materialized_image_source: Result<Option<PathBuf>, String>,
    attachment_size: Option<u64>,
}

/// The worker gives an uncommitted stage back to the retained session on
/// error.  This matters when inverse replay cannot safely rebase every later
/// input operation: the atom stays visibly retryable from descriptor-safe
/// staged bytes instead of becoming a silently dangling document reference.
enum ResourceCommitCompletion {
    Success(ResourceCommitSuccess),
    Failure {
        staged: StagedResourceInsert,
        error: SaveError,
    },
}

/// Platform opening is deliberately injected at the session boundary.  The
/// native surface only emits a resource id, and the worker passes this opener
/// a session-private, descriptor-safe copy rather than a profile path.
pub(crate) type AttachmentOpener = Arc<dyn Fn(&Path) -> Result<(), String> + Send + Sync>;

struct AttachmentOpenJob {
    repository: Arc<LibraryRepository>,
    resource_id: ResourceId,
    opener: AttachmentOpener,
    #[cfg(test)]
    gate: Option<Arc<BackgroundAttachmentOpenGate>>,
}

/// A private, worker-owned handoff source for one platform attachment open.
///
/// This deliberately lives outside `ImageStore`'s session root. A note switch
/// is allowed to destroy the editor while `/usr/bin/open` is still receiving
/// the file, so the worker owns this directory until either the live session
/// adopts it or the detached completion drops it after a failed weak upgrade.
struct AttachmentMaterializationLease {
    root: PathBuf,
    source: Option<PathBuf>,
}

impl AttachmentMaterializationLease {
    fn new() -> Result<Self, SaveError> {
        let parent = std::env::temp_dir().join("joplin-lite-native-attachment-handoffs");
        Self::new_in(&parent)
    }

    fn new_in(parent: &Path) -> Result<Self, SaveError> {
        if let Ok(metadata) = std::fs::symlink_metadata(parent) {
            if metadata.file_type().is_symlink() || !metadata.is_dir() {
                return Err(SaveError::new("附件临时目录不是可信的私有目录，已拒绝打开"));
            }
        } else {
            std::fs::create_dir_all(parent)
                .map_err(|error| SaveError::new(format!("无法创建附件临时目录：{error}")))?;
        }
        std::fs::set_permissions(parent, std::fs::Permissions::from_mode(0o700))
            .map_err(|error| SaveError::new(format!("无法保护附件临时目录：{error}")))?;

        for _ in 0..16 {
            let root = parent.join(format!("open-{}", uuid::Uuid::new_v4().simple()));
            let mut builder = std::fs::DirBuilder::new();
            builder.mode(0o700);
            match builder.create(&root) {
                Ok(()) => {
                    std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o700))
                        .map_err(|error| {
                            let _ = std::fs::remove_dir_all(&root);
                            SaveError::new(format!("无法保护附件交接目录：{error}"))
                        })?;
                    return Ok(Self { root, source: None });
                }
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(error) => {
                    return Err(SaveError::new(format!("无法创建附件私有交接目录：{error}")));
                }
            }
        }
        Err(SaveError::new("无法分配唯一附件交接目录"))
    }

    fn root(&self) -> &Path {
        &self.root
    }

    fn record_source(&mut self, source: PathBuf) {
        self.source = Some(source);
    }
}

impl Drop for AttachmentMaterializationLease {
    fn drop(&mut self) {
        if let Some(source) = self.source.take() {
            let _ = std::fs::remove_file(source);
        }
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

#[cfg(all(test, unix))]
mod attachment_materialization_lease_tests {
    use super::AttachmentMaterializationLease;

    #[test]
    fn rejects_a_symlinked_shared_handoff_parent() {
        // A handoff root lives below a shared temp parent, so following a
        // pre-existing symlink would turn a descriptor-safe source into an
        // arbitrary-path write. This must fail before the 0700 leaf exists.
        let fixture = tempfile::tempdir().expect("temporary attachment parent fixture");
        let real_parent = fixture.path().join("real-parent");
        std::fs::create_dir(&real_parent).expect("create real parent");
        let linked_parent = fixture.path().join("linked-parent");
        std::os::unix::fs::symlink(&real_parent, &linked_parent)
            .expect("create hostile shared-parent symlink");
        assert!(
            AttachmentMaterializationLease::new_in(&linked_parent).is_err(),
            "a symlinked shared attachment parent must be rejected before materialization"
        );
        assert!(
            std::fs::read_dir(&real_parent)
                .expect("real parent remains readable")
                .next()
                .is_none(),
            "rejection must not create a handoff directory through the symlink"
        );
    }
}

struct AttachmentOpenCompletion {
    resource_id: ResourceId,
    lease: AttachmentMaterializationLease,
}

pub(crate) struct AttachmentOpenSuccess {
    pub(crate) resource_id: ResourceId,
}

fn system_attachment_opener(path: &Path) -> Result<(), String> {
    #[cfg(target_os = "macos")]
    {
        let status = std::process::Command::new("/usr/bin/open")
            .arg(path)
            .status()
            .map_err(|error| format!("无法交给系统默认应用：{error}"))?;
        if status.success() {
            Ok(())
        } else {
            Err(format!("系统默认应用打开命令退出失败：{status}"))
        }
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = path;
        Err("当前平台不支持使用系统默认应用打开附件".to_owned())
    }
}

fn safe_attachment_extension(value: &str) -> String {
    let value = value.trim().trim_start_matches('.');
    if !value.is_empty()
        && value.len() <= 16
        && value
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit())
    {
        value.to_owned()
    } else {
        "bin".to_owned()
    }
}

/// Stream one already hash-verified repository descriptor into a private
/// worker lease. Its destination never comes from a profile/resource path,
/// gets a fresh random name, is opened no-follow with 0600 permissions,
/// synced, then atomically published. This keeps even a 50MiB attachment out
/// of a foreground `Vec`.
fn materialize_verified_attachment_reader<R: Read>(
    destination_root: &Path,
    resource_id: &ResourceId,
    extension: &str,
    mut reader: R,
) -> Result<PathBuf, SaveError> {
    let nonce = uuid::Uuid::new_v4().simple();
    let extension = safe_attachment_extension(extension);
    let destination =
        destination_root.join(format!("{}-{nonce}.{extension}", resource_id.as_str()));
    let temporary = destination_root.join(format!(".{}-{nonce}.tmp", resource_id.as_str()));
    let copied = (|| -> std::io::Result<()> {
        let mut target = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
            .open(&temporary)?;
        let mut buffer = [0_u8; 64 * 1024];
        loop {
            let count = reader.read(&mut buffer)?;
            if count == 0 {
                break;
            }
            target.write_all(&buffer[..count])?;
        }
        target.sync_all()?;
        std::fs::rename(&temporary, &destination)?;
        std::fs::set_permissions(&destination, std::fs::Permissions::from_mode(0o600))
    })();
    if let Err(error) = copied {
        let _ = std::fs::remove_file(&temporary);
        let _ = std::fs::remove_file(&destination);
        return Err(SaveError::new(format!("无法物化附件安全副本：{error}")));
    }
    Ok(destination)
}

/// The committed fact returned to the library shell. It intentionally carries
/// no blob bytes: presentation can update its active note/card from the same
/// SQLite transaction that associated the resource.
#[derive(Clone)]
pub(crate) struct InsertedResource {
    pub(crate) note: Note,
    pub(crate) resource_id: ResourceId,
    pub(crate) is_image: bool,
    /// Final value chosen inside the same SQLite transaction that associated
    /// the resource.  The card surface must use this rather than infer a
    /// cover from its previous projection (which can still name an image that
    /// this snapshot removed).
    pub(crate) selected_thumbnail_id: Option<ResourceId>,
    /// The SQLite snapshot is already authoritative if cache materialization
    /// fails. Surface code receives this separately so it can show an honest
    /// decode notice without pretending that the durable insertion failed.
    pub(crate) presentation_warning: Option<String>,
}

enum SaveCompletion {
    Journal {
        ownership: JournalOwnership,
    },
    Snapshot {
        /// The normal text/title save path must carry the same complete Note
        /// assembled inside its SQLite transaction.  Organization mutations
        /// can otherwise remount a stale `AppModel::active_session` body
        /// after a successful autosave and overwrite that durable text with a
        /// later edit.
        note: Note,
        snapshot: SessionSnapshot,
        expected_revision: i64,
    },
}

/// A lifecycle action is not allowed to cross this barrier until the exact
/// captured generation has become a durable snapshot. It deliberately lives
/// with the retained session rather than the UI so a second close/switch call
/// cannot reinterpret an in-flight journal as the old `last_saved` success.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct FlushBarrier {
    generation: i64,
    expected_revision: i64,
}

#[derive(Clone)]
struct SaveJob {
    work: SaveWork,
    note_id: NoteId,
    expected_revision: i64,
    writer_token: String,
    /// The exact checkpoint this snapshot worker is allowed to compact. A
    /// token without its SQLite sequence is insufficient: the same owner may
    /// have published a newer checkpoint after this job was queued.
    journal_ownership: Option<JournalOwnership>,
    journal_base: SessionSnapshot,
    snapshot: NativeSessionSnapshot,
    repository: Arc<LibraryRepository>,
    #[cfg(test)]
    gate: Option<Arc<BackgroundSaveGate>>,
}

/// Test-only asynchronous gate used to prove a blocked worker never blocks a
/// GPUI paint. It lives inside the worker future, not in an entity callback.
#[cfg(test)]
struct BackgroundSaveGate {
    release: Mutex<Option<oneshot::Receiver<()>>>,
}

#[cfg(test)]
impl BackgroundSaveGate {
    async fn wait(&self) {
        let receiver = self
            .release
            .lock()
            .expect("background save gate mutex poisoned")
            .take();
        if let Some(receiver) = receiver {
            let _ = receiver.await;
        }
    }
}

/// Test-only gate for the Task-5 import workers. It deliberately sits inside
/// the same retained/background future used in production, so a mounted draw
/// proves that picker/drop hashing and SQLite work did not slip back into a
/// GPUI update callback.
#[cfg(test)]
struct BackgroundResourceGate {
    release: Mutex<Option<oneshot::Receiver<()>>>,
}

#[cfg(test)]
impl BackgroundResourceGate {
    async fn wait(&self) {
        let receiver = self
            .release
            .lock()
            .expect("background resource gate mutex poisoned")
            .take();
        if let Some(receiver) = receiver {
            let _ = receiver.await;
        }
    }
}

/// Test-only gate placed after descriptor-safe attachment materialization and
/// before the injected/platform opener. It proves that a normal reducer note
/// switch cannot remove a handoff file while the worker still owns it.
#[cfg(test)]
struct BackgroundAttachmentOpenGate {
    release: Mutex<Option<oneshot::Receiver<()>>>,
    materialized: std::sync::mpsc::Sender<PathBuf>,
}

#[cfg(test)]
impl BackgroundAttachmentOpenGate {
    async fn wait_after_materialization(&self, path: &Path) {
        let _ = self.materialized.send(path.to_path_buf());
        let receiver = self
            .release
            .lock()
            .expect("attachment open gate mutex poisoned")
            .take();
        if let Some(receiver) = receiver {
            let _ = receiver.await;
        }
    }
}

/// A readable splice over a durable string. Normal keystrokes contain only
/// the inserted grapheme rather than another pretty-printed copy of a note.
#[derive(Clone, Debug, Serialize, Deserialize)]
struct TextDelta {
    start_byte: usize,
    remove_bytes: usize,
    insert: String,
}

impl TextDelta {
    fn between(before: &str, after: &str) -> Self {
        let mut prefix = 0;
        for ((before_at, before_char), (after_at, after_char)) in
            before.char_indices().zip(after.char_indices())
        {
            if before_char != after_char {
                break;
            }
            prefix = before_at + before_char.len_utf8();
            debug_assert_eq!(prefix, after_at + after_char.len_utf8());
        }
        let before_tail = &before[prefix..];
        let after_tail = &after[prefix..];
        let mut suffix = 0;
        for (before_char, after_char) in before_tail.chars().rev().zip(after_tail.chars().rev()) {
            if before_char != after_char {
                break;
            }
            suffix += before_char.len_utf8();
        }
        // The common prefix/suffix cannot overlap because each side's tail is
        // independently bounded by the original text.
        suffix = suffix.min(before_tail.len()).min(after_tail.len());
        Self {
            start_byte: prefix,
            remove_bytes: before_tail.len().saturating_sub(suffix),
            insert: after_tail[..after_tail.len().saturating_sub(suffix)].to_owned(),
        }
    }

    fn apply(&self, base: &str, field: &str) -> Result<String, SaveError> {
        let end = self
            .start_byte
            .checked_add(self.remove_bytes)
            .ok_or_else(|| SaveError::new(format!("编辑日志 {field} 偏移溢出")))?;
        if self.start_byte > base.len()
            || end > base.len()
            || !base.is_char_boundary(self.start_byte)
            || !base.is_char_boundary(end)
        {
            return Err(SaveError::new(format!("编辑日志 {field} 偏移无效")));
        }
        Ok(format!(
            "{}{}{}",
            &base[..self.start_byte],
            self.insert,
            &base[end..]
        ))
    }
}

#[derive(Debug, Serialize, Deserialize)]
struct JournalPayload {
    version: u8,
    note_id: String,
    expected_revision: i64,
    writer_token: String,
    generation: i64,
    title: TextDelta,
    body: TextDelta,
    resource_ids: Vec<String>,
}

pub(crate) struct PreparedNoteSession {
    note: Note,
    snapshot: SessionSnapshot,
    journal_base: SessionSnapshot,
    native_document: crate::native_editor::model::Document,
    image_sources: Vec<PreparedImageSource>,
    image_load_notices: Vec<String>,
    attachment_sources: Vec<PreparedAttachmentSource>,
    attachment_load_notices: Vec<String>,
    recovered_generation: Option<i64>,
    /// Every prepared session receives its future retained writer identity
    /// before entity construction. A recovered checkpoint must atomically
    /// install this exact value before any real input may schedule work.
    writer_token: String,
    /// Set only after a recovery claim succeeds. The mounted session carries
    /// this exact SQLite lease through its first snapshot rather than treating
    /// a writer token as a broad permission to clear any checkpoint.
    journal_ownership: Option<JournalOwnership>,
    recovery_ownership: Option<RecoveryOwnership>,
}

/// Identity read from a validated durable checkpoint. It is deliberately kept
/// in the fallible pre-entity phase: a conflict must show a visible recovery
/// error, never mount an apparently editable session with no write ownership.
struct RecoveryOwnership {
    expected_revision: i64,
    previous_writer_token: String,
    sequence: i64,
}

/// `from_prepared` accepts only this consumed wrapper, so callers cannot
/// accidentally mount a recovered document before its SQLite owner transfer
/// has completed.
pub(crate) struct ClaimedPreparedNoteSession(PreparedNoteSession);

impl PreparedNoteSession {
    /// Moves one valid recovered journal from its crashed writer to this new
    /// retained session. The JSON token and database token change in the same
    /// IMMEDIATE transaction so the next restart validates one coherent owner.
    pub(crate) fn claim_recovery_ownership(
        mut self,
        repository: &LibraryRepository,
    ) -> Result<ClaimedPreparedNoteSession, SaveError> {
        if let Some(recovery) = self.recovery_ownership.take() {
            if recovery.expected_revision != self.note.revision {
                return Err(SaveError::new("编辑日志的恢复版本不一致"));
            }
            let generation = self
                .recovered_generation
                .ok_or_else(|| SaveError::new("编辑日志缺少恢复代际"))?;
            let payload = JournalPayload::from_snapshots(
                &self.note.id,
                self.note.revision,
                self.writer_token.clone(),
                generation,
                &self.journal_base,
                &self.snapshot,
            );
            let delta_utf8 = serde_json::to_string(&payload)
                .map_err(|error| SaveError::new(format!("无法接管编辑日志: {error}")))?;
            let ownership = repository.claim_edit_journal_ownership(
                &self.note.id,
                recovery.expected_revision,
                &recovery.previous_writer_token,
                recovery.sequence,
                &self.writer_token,
                &delta_utf8,
            )?;
            self.journal_ownership = Some(ownership);
        }
        Ok(ClaimedPreparedNoteSession(self))
    }
}

impl JournalPayload {
    fn from_snapshots(
        note_id: &NoteId,
        expected_revision: i64,
        writer_token: String,
        generation: i64,
        base: &SessionSnapshot,
        snapshot: &SessionSnapshot,
    ) -> Self {
        Self {
            version: JOURNAL_SCHEMA_VERSION,
            note_id: note_id.as_str().to_owned(),
            expected_revision,
            writer_token,
            generation,
            title: TextDelta::between(&base.title, &snapshot.title),
            body: TextDelta::between(
                base.document.to_canonical_html().as_str(),
                snapshot.document.to_canonical_html().as_str(),
            ),
            resource_ids: snapshot
                .resource_ids
                .iter()
                .map(|id| id.as_str().to_owned())
                .collect(),
        }
    }

    fn into_snapshot(
        self,
        note: &Note,
        entry: &EditJournalEntry,
        durable_resource_provenance: &[ResourceId],
    ) -> Result<(i64, SessionSnapshot), SaveError> {
        if self.version != JOURNAL_SCHEMA_VERSION
            || self.note_id != note.id.as_str()
            || self.expected_revision != note.revision
            || self.expected_revision != entry.expected_revision
            || self.writer_token.is_empty()
            || self.writer_token != entry.writer_token
        {
            return Err(SaveError::new("编辑日志与当前笔记版本不匹配"));
        }
        let resource_ids = self
            .resource_ids
            .into_iter()
            .map(|raw| ResourceId::new(raw).map_err(|_| SaveError::new("编辑日志包含无效资源 ID")))
            .collect::<Result<Vec<_>, _>>()?;
        let title = self.title.apply(&note.title, "标题")?;
        let body_html = self.body.apply(&note.body_html, "正文")?;
        let document = CanonicalDocument::parse_html(&body_html)
            .map_err(|error| SaveError::new(format!("无法恢复编辑日志正文: {error}")))?;
        if document.resource_ids() != resource_ids {
            return Err(SaveError::new("编辑日志正文的资源关系不完整"));
        }
        if !resource_ids_are_occurrence_bounded(&resource_ids, durable_resource_provenance) {
            return Err(SaveError::new(
                "编辑日志引用了该笔记持久历史之外的资源或放大了出现次数",
            ));
        }
        Ok((
            self.generation.max(1),
            SessionSnapshot {
                title,
                document,
                resource_ids,
            },
        ))
    }
}

/// A crash journal is hostile input: its declared relation can never be its
/// own authority. A normal snapshot may remove A from A,B,A and a later
/// Cmd-Z may restore that first A before the next snapshot, so recovery uses
/// the note's committed revision history rather than only the current B,A
/// relation. It still cannot manufacture a global resource or amplify any
/// historical occurrence count.
fn resource_ids_are_occurrence_bounded(
    candidate: &[ResourceId],
    durable_provenance: &[ResourceId],
) -> bool {
    let mut remaining = HashMap::<&ResourceId, usize>::new();
    for resource_id in durable_provenance {
        *remaining.entry(resource_id).or_default() += 1;
    }
    candidate.iter().all(|resource_id| {
        let Some(count) = remaining.get_mut(resource_id) else {
            return false;
        };
        if *count == 0 {
            return false;
        }
        *count -= 1;
        true
    })
}

/// Keep the retained session's authorization set monotonic across ordinary
/// snapshots. A resource remains a valid local reference after its relation
/// is deleted so Cmd-Z can restore the same durable blob; the next snapshot
/// still derives the visible `note_resources` order from the document.
fn extend_resource_allowlist(allowlist: &mut Vec<ResourceId>, resource_ids: &[ResourceId]) {
    for resource_id in resource_ids {
        if !allowlist.iter().any(|known| known == resource_id) {
            allowlist.push(resource_id.clone());
        }
    }
}

/// Build geometry-only hydration descriptors before a session mounts. No
/// repository call belongs here: a note with one visible image and hundreds
/// offscreen must be able to paint immediately without hashing/reading every
/// original blob on the GPUI thread.
fn prepare_persisted_image_hydration(
    document: Document,
    canonical: &CanonicalDocument,
) -> Result<(Document, Vec<PreparedImageSource>), SaveError> {
    let mut canonical_images = canonical.blocks().iter().filter_map(|block| match block {
        app_lite_core::document::Block::Image {
            resource_id,
            presentation,
            ..
        } => Some((resource_id, presentation.natural_size.is_none())),
        _ => None,
    });
    let mut sources = Vec::new();
    for block in document.blocks() {
        let BlockContent::Image {
            resource_id,
            natural_size,
            ..
        } = &block.content
        else {
            continue;
        };
        let id =
            ResourceId::new(resource_id).map_err(|_| SaveError::new("正文图片含无效资源 ID"))?;
        let Some((canonical_id, needs_legacy_geometry_repair)) = canonical_images.next() else {
            return Err(SaveError::new("图片节点与持久化正文顺序不一致"));
        };
        if canonical_id != &id {
            return Err(SaveError::new("图片节点与持久化资源引用不一致"));
        }
        sources.push(PreparedImageSource {
            resource_id: id,
            node_id: block.id,
            natural_size: *natural_size,
            needs_legacy_geometry_repair,
        });
    }
    if canonical_images.next().is_some() {
        return Err(SaveError::new("图片节点与持久化正文数量不一致"));
    }
    Ok((document, sources))
}

/// Resolve cards independently from images. A missing attachment must not
/// reject the whole note or cause a blob read during mount; the renderer gets
/// an explicit unavailable state and the rest of the body stays editable.
fn hydrate_persisted_attachments(
    document: &Document,
    repository: &LibraryRepository,
) -> (Vec<PreparedAttachmentSource>, Vec<String>) {
    let mut sources = Vec::new();
    let mut notices = Vec::new();
    for block in document.blocks() {
        let BlockContent::Attachment { resource_id, .. } = &block.content else {
            continue;
        };
        let id = match ResourceId::new(resource_id) {
            Ok(id) => id,
            Err(_) => {
                notices.push("附件含无效资源 ID".to_owned());
                continue;
            }
        };
        match repository.resource_metadata(&id) {
            Ok(Some(metadata)) if metadata.size >= 0 => {
                sources.push(PreparedAttachmentSource::Ready {
                    resource_id: id,
                    size: metadata.size as u64,
                });
            }
            Ok(Some(_)) => {
                notices.push(format!("附件 {} 大小无效，已显示为不可用", id.as_str()));
                sources.push(PreparedAttachmentSource::Unavailable { resource_id: id });
            }
            Ok(None) => {
                notices.push(format!("附件 {} 暂不可用：资源记录不存在", id.as_str()));
                sources.push(PreparedAttachmentSource::Unavailable { resource_id: id });
            }
            Err(error) => {
                notices.push(format!("附件 {} 暂不可用：{error}", id.as_str()));
                sources.push(PreparedAttachmentSource::Unavailable { resource_id: id });
            }
        }
    }
    (sources, notices)
}

/// One active library note. The entity subscriptions observe real title/body
/// input notifications, while a semantic snapshot comparison filters focus,
/// selection and paint notifications that must never create false saves.
pub(crate) struct NoteSession {
    note_id: NoteId,
    /// A deleted note has no legal journal/snapshot CAS target. Keep its
    /// retained detail session for copy, scrolling and attachment open, but
    /// place both title and body behind the same read-only authority.
    read_only: bool,
    /// A local organization/trash mutation committed in SQLite but has not
    /// yet produced a coherent replacement candidate for the retained shell.
    /// While that candidate is pending, the old revision is only a stale
    /// presentation snapshot and must not accept title/body/format/resource
    /// mutations that a following event could otherwise discard.
    reconciliation_locked: bool,
    expected_revision: i64,
    writer_token: String,
    /// The current journal lease owned by this session. It changes every time
    /// a journal append allocates a new sequence and is consumed only by the
    /// exact snapshot that compacts that row.
    journal_ownership: Option<JournalOwnership>,
    resource_ids: Vec<ResourceId>,
    /// A cache-source failure after a durable resource commit is presentation
    /// state, not a false save failure. The shell renders it as a notice.
    resource_load_warning: Option<String>,
    title: Entity<TitleInput>,
    editor: Entity<EditorCore>,
    repository: Arc<LibraryRepository>,
    save: SaveCoordinator,
    last_saved: SavedRevision,
    /// The last *unmarked* title/body pair captured from the real input
    /// entities. Save work is required to consume this immutable value rather
    /// than rereading a live entity after an IME has installed a candidate.
    committed_snapshot: NativeSessionSnapshot,
    /// Exact durable base of the current journal revision. Recovery applies a
    /// compact delta only to this revision, never to a later note snapshot.
    journal_base: SessionSnapshot,
    /// All three deadline tasks are retained by the entity so destruction
    /// cancels their weak callbacks. Production therefore has no 50ms
    /// foreground polling loop for local saves.
    _journal_deadline_task: Option<Task<()>>,
    _settled_deadline_task: Option<Task<()>>,
    _hard_deadline_task: Option<Task<()>>,
    _save_task: Option<Task<()>>,
    /// Resource normalization/staging and the following snapshot transaction
    /// both run in separately retained GPUI tasks.  They belong to this
    /// session rather than the shell so note switch/destruction cancels their
    /// weak callbacks without ever retaining a stale window.
    _resource_stage_task: Option<Task<()>>,
    _resource_commit_task: Option<Task<()>>,
    /// Exactly one verified persisted-image materialization runs at a time.
    /// Requests originate from `EditorCore`'s visible/prefetch queue, never
    /// from note preparation or list projection.
    _image_hydration_task: Option<Task<()>>,
    persisted_image_hydration: HashMap<String, PersistedImageHydration>,
    pending_image_hydration: VecDeque<String>,
    pending_legacy_image_repairs: VecDeque<LegacyImageGeometryRepair>,
    /// An attachment platform handoff must outlive a session switch until the
    /// opener returns. The detached worker owns its lease; this flag is only
    /// the live-session serialization guard and is cleared by a successful
    /// weak completion update.
    attachment_open_in_flight: bool,
    /// Successful platform handoffs remain readable for the active retained
    /// session. Dropping/switching that session drops the leases and removes
    /// their private 0700 roots.
    _opened_attachment_sources: Vec<AttachmentMaterializationLease>,
    pending_staged_resource: Option<PendingStagedResourceInsert>,
    pending_failed_resource_commit: Option<StagedResourceInsert>,
    /// A normal title/body snapshot has committed.  The retained shell
    /// consumes this authoritative result before any organization candidate
    /// is allowed to reuse the active-session note as metadata scaffolding.
    /// Coalescing to the latest completion is safe: it is a complete durable
    /// note, and an observer never needs an intermediate body revision.
    saved_note_outcome: Option<Note>,
    resource_import_outcome: Option<Result<InsertedResource, SaveError>>,
    attachment_open_outcome: Option<Result<AttachmentOpenSuccess, SaveError>>,
    attachment_opener: AttachmentOpener,
    pending_flush: Option<FlushBarrier>,
    /// A lifecycle boundary observed an in-flight staged resource operation.
    /// This is deliberately separate from `pending_flush`: a descriptor stage
    /// starts while the text save state may still be Clean, and treating it as
    /// an ordinary save fence would prevent the later stage completion from
    /// starting its own atomic resource commit.
    pending_resource_lifecycle_flush: bool,
    #[cfg(test)]
    deadline_tasks_enabled_for_test: bool,
    #[cfg(test)]
    next_background_save_gate: Option<Arc<BackgroundSaveGate>>,
    #[cfg(test)]
    next_resource_stage_gate: Option<Arc<BackgroundResourceGate>>,
    #[cfg(test)]
    next_resource_commit_gate: Option<Arc<BackgroundResourceGate>>,
    #[cfg(test)]
    next_image_hydration_gate: Option<Arc<BackgroundResourceGate>>,
    #[cfg(test)]
    next_attachment_open_gate: Option<Arc<BackgroundAttachmentOpenGate>>,
    #[cfg(test)]
    next_resource_commit_failure: Option<SaveError>,
    observed_title: String,
    observed_document: crate::native_editor::model::SemanticSnapshot,
    _title_observation: Subscription,
    _editor_observation: Subscription,
}

impl NoteSession {
    /// Performs every fallible repository/codec step before an entity is
    /// allocated. A library-window closure can therefore show a concrete
    /// error instead of `expect`ing halfway through retained UI construction.
    pub(crate) fn prepare(
        note: Note,
        repository: &LibraryRepository,
    ) -> Result<PreparedNoteSession, SaveError> {
        let mut snapshot = SessionSnapshot {
            title: note.title.clone(),
            document: CanonicalDocument::parse_html(&note.body_html)
                .map_err(|error| SaveError::new(format!("无法解析已保存的正文: {error}")))?,
            resource_ids: note.resource_ids.clone(),
        };
        let journal_base = snapshot.clone();
        let mut recovered_generation = None;
        let journal_ownership = None;
        let mut recovery_ownership = None;
        if let Some(entry) =
            repository.latest_edit_journal_for_revision(&note.id, Some(note.revision))?
        {
            if entry.writer_token.is_empty() || entry.sequence < 1 {
                return Err(SaveError::new("编辑日志缺少写入者身份"));
            }
            let version = serde_json::from_str::<serde_json::Value>(&entry.delta_utf8)
                .ok()
                .and_then(|value| value.get("version").and_then(serde_json::Value::as_u64));
            let (generation, recovered) = match version {
                Some(1) => {
                    let payload = LegacyJournalPayload::parse_and_validate(
                        &entry.delta_utf8,
                        &entry.note_id,
                        entry.generation,
                        &note.resource_ids,
                    )
                    .map_err(|error| SaveError::new(format!("无法读取旧编辑日志: {error}")))?;
                    if payload.expected_revision() != note.revision
                        || payload.expected_revision() != entry.expected_revision
                    {
                        return Err(SaveError::new("编辑日志与当前笔记版本不匹配"));
                    }
                    let generation = payload.generation();
                    let (title, document, resource_ids) = payload.into_parts();
                    (
                        generation,
                        SessionSnapshot {
                            title,
                            document,
                            resource_ids,
                        },
                    )
                }
                Some(version) if version == JOURNAL_SCHEMA_VERSION as u64 => {
                    let durable_resource_provenance = repository
                        .durable_resource_provenance_for_recovery(&note.id, note.revision)?;
                    serde_json::from_str::<JournalPayload>(&entry.delta_utf8)
                        .map_err(|error| SaveError::new(format!("无法读取编辑日志: {error}")))?
                        .into_snapshot(&note, &entry, &durable_resource_provenance)?
                }
                _ => return Err(SaveError::new("编辑日志版本不受支持")),
            };
            snapshot = recovered;
            recovered_generation = Some(generation.max(entry.generation));
            recovery_ownership = Some(RecoveryOwnership {
                expected_revision: entry.expected_revision,
                previous_writer_token: entry.writer_token,
                sequence: entry.sequence,
            });
        }
        let native_document =
            import_canonical_with_resources(&snapshot.document, &snapshot.resource_ids)
                .map_err(|error| SaveError::new(format!("无法转换正文: {error}")))?;
        let (native_document, image_sources) =
            prepare_persisted_image_hydration(native_document, &snapshot.document)?;
        let (attachment_sources, attachment_load_notices) =
            hydrate_persisted_attachments(&native_document, repository);
        Ok(PreparedNoteSession {
            note,
            snapshot,
            journal_base,
            native_document,
            image_sources,
            image_load_notices: Vec::new(),
            attachment_sources,
            attachment_load_notices,
            recovered_generation,
            writer_token: uuid::Uuid::new_v4().simple().to_string(),
            journal_ownership,
            recovery_ownership,
        })
    }

    pub(crate) fn from_prepared(
        prepared: ClaimedPreparedNoteSession,
        repository: Arc<LibraryRepository>,
        clock: Arc<dyn SaveClock>,
        cx: &mut Context<Self>,
    ) -> Self {
        let PreparedNoteSession {
            note,
            snapshot,
            journal_base,
            native_document: document,
            image_sources,
            image_load_notices,
            attachment_sources,
            attachment_load_notices,
            recovered_generation,
            writer_token,
            journal_ownership,
            recovery_ownership: _,
        } = prepared.0;
        let read_only = note.deleted_time.is_some();
        let committed_snapshot = NativeSessionSnapshot {
            title: snapshot.title.clone(),
            document: document.clone(),
            allowed_resource_ids: snapshot.resource_ids.clone(),
        };
        let title_text = snapshot.title.clone();
        let title = cx.new(move |cx| {
            if read_only {
                TitleInput::new_read_only(title_text, cx)
            } else {
                TitleInput::new(title_text, cx)
            }
        });
        let editor = cx.new(move |cx| {
            if read_only {
                EditorCore::new_read_only(document, cx)
            } else {
                EditorCore::new(document, cx)
            }
        });
        let mut load_notices = image_load_notices;
        load_notices.extend(attachment_load_notices);
        let mut resource_load_warning = (!load_notices.is_empty()).then(|| load_notices.join("；"));
        let mut persisted_image_hydration = HashMap::<String, PersistedImageHydration>::new();
        for source in image_sources {
            let entry = persisted_image_hydration
                .entry(source.resource_id.as_str().to_owned())
                .or_insert_with(|| PersistedImageHydration {
                    resource_id: source.resource_id.clone(),
                    natural_size: source.natural_size,
                    legacy_node_ids: Vec::new(),
                });
            if source.needs_legacy_geometry_repair {
                entry.legacy_node_ids.push(source.node_id);
            }
        }
        for source in attachment_sources {
            let result = match source {
                PreparedAttachmentSource::Ready { resource_id, size } => editor
                    .update(cx, |editor, _| {
                        editor.register_attachment(resource_id.as_str(), size)
                    })
                    .map_err(|error| error.to_string()),
                PreparedAttachmentSource::Unavailable { resource_id } => editor
                    .update(cx, |editor, _| {
                        editor.register_unavailable_attachment(resource_id.as_str())
                    })
                    .map_err(|error| error.to_string()),
            };
            if let Err(error) = result {
                let warning = format!("已保存附件暂无法显示：{error}");
                match &mut resource_load_warning {
                    Some(existing) => {
                        existing.push('；');
                        existing.push_str(&warning);
                    }
                    None => resource_load_warning = Some(warning),
                }
            }
        }
        let observed_title = snapshot.title;
        let observed_document = editor.read(cx).document().semantic_snapshot();
        let title_for_observation = title.clone();
        let editor_for_observation = editor.clone();
        let title_observation = cx.observe(&title, move |session, _, session_cx| {
            session.observe_entities(&title_for_observation, &editor_for_observation, session_cx);
        });
        let title_for_editor_observation = title.clone();
        let editor_for_editor_observation = editor.clone();
        let editor_observation = cx.observe(&editor, move |session, _, session_cx| {
            session.observe_entities(
                &title_for_editor_observation,
                &editor_for_editor_observation,
                session_cx,
            );
        });
        let mut save = SaveCoordinator::new(clock);
        if let Some(generation) = recovered_generation {
            save.restore_journaled(generation);
        }
        Self {
            note_id: note.id,
            read_only,
            reconciliation_locked: false,
            expected_revision: note.revision,
            writer_token,
            journal_ownership,
            resource_ids: snapshot.resource_ids,
            resource_load_warning,
            title,
            editor,
            repository,
            save,
            last_saved: SavedRevision {
                revision: note.revision,
                saved_time: note.updated_time,
            },
            committed_snapshot,
            journal_base,
            _journal_deadline_task: None,
            _settled_deadline_task: None,
            _hard_deadline_task: None,
            _save_task: None,
            _resource_stage_task: None,
            _resource_commit_task: None,
            _image_hydration_task: None,
            persisted_image_hydration,
            pending_image_hydration: VecDeque::new(),
            pending_legacy_image_repairs: VecDeque::new(),
            attachment_open_in_flight: false,
            _opened_attachment_sources: Vec::new(),
            pending_staged_resource: None,
            pending_failed_resource_commit: None,
            saved_note_outcome: None,
            resource_import_outcome: None,
            attachment_open_outcome: None,
            attachment_opener: Arc::new(system_attachment_opener),
            pending_flush: None,
            pending_resource_lifecycle_flush: false,
            #[cfg(test)]
            deadline_tasks_enabled_for_test: false,
            #[cfg(test)]
            next_background_save_gate: None,
            #[cfg(test)]
            next_resource_stage_gate: None,
            #[cfg(test)]
            next_resource_commit_gate: None,
            #[cfg(test)]
            next_image_hydration_gate: None,
            #[cfg(test)]
            next_attachment_open_gate: None,
            #[cfg(test)]
            next_resource_commit_failure: None,
            observed_title,
            observed_document,
            _title_observation: title_observation,
            _editor_observation: editor_observation,
        }
    }

    #[cfg(test)]
    pub(crate) fn open(
        note: Note,
        repository: Arc<LibraryRepository>,
        clock: Arc<dyn SaveClock>,
        cx: &mut Context<Self>,
    ) -> Result<Self, SaveError> {
        let prepared = Self::prepare(note, repository.as_ref())?;
        let prepared = prepared.claim_recovery_ownership(repository.as_ref())?;
        Ok(Self::from_prepared(prepared, repository, clock, cx))
    }

    pub(crate) fn title(&self) -> &Entity<TitleInput> {
        &self.title
    }

    /// Any current input path must use this capability, rather than infer it
    /// from the durable deleted flag. A reconciliation lock is deliberately
    /// as strong as Trash for document/title/format/resource mutation.
    pub(crate) fn is_read_only(&self) -> bool {
        self.read_only || self.reconciliation_locked
    }

    /// Only the durable deleted-note preview has Trash lifecycle semantics.
    /// In particular, a temporary reconciliation lock must never let
    /// Restore/Purge bypass the ordinary active-session flush boundary.
    pub(crate) fn is_durable_read_only(&self) -> bool {
        self.read_only
    }

    pub(crate) fn is_reconciliation_locked(&self) -> bool {
        self.reconciliation_locked
    }

    /// Freeze or unfreeze a retained, otherwise editable session while the
    /// AppModel reconciles a committed metadata mutation. The durable Trash
    /// flag remains authoritative, so a successful candidate can only unlock
    /// sessions which were editable before this temporary fence.
    pub(crate) fn set_reconciliation_locked(&mut self, locked: bool, cx: &mut Context<Self>) {
        if self.reconciliation_locked == locked {
            return;
        }
        self.reconciliation_locked = locked;
        if locked {
            // A staged descriptor has no visible SQLite metadata yet, but
            // its tracked Selection was captured from the revision which is
            // now being reconciled.  Do not keep it around to run after an
            // unlock: that would turn an old picker/drop completion into a
            // mutation of whichever packet eventually wins recovery.
            if let Some(pending) = self.pending_staged_resource.take() {
                self.discard_resource_insert_intent(pending.intent, cx);
                self.pending_resource_lifecycle_flush = false;
                self.resource_import_outcome = Some(Err(SaveError::new(
                    "资料库已提交，正在恢复界面；已取消本次资源插入，请恢复后重新选择",
                )));
            }
        }
        let title_read_only = self.read_only || locked;
        let _ = self.title.update(cx, |title, title_cx| {
            title.set_read_only_for_session(title_read_only);
            title_cx.notify();
        });
        let _ = self.editor.update(cx, |editor, editor_cx| {
            editor.set_recovery_locked(locked);
            editor_cx.notify();
        });
        cx.notify();
    }

    pub(crate) fn editor(&self) -> &Entity<EditorCore> {
        &self.editor
    }

    pub(crate) fn resource_load_warning(&self) -> Option<&str> {
        self.resource_load_warning.as_deref()
    }

    fn push_resource_load_warning(&mut self, message: impl Into<String>) {
        let message = message.into();
        match &mut self.resource_load_warning {
            Some(existing) if !existing.is_empty() => {
                existing.push('；');
                existing.push_str(&message);
            }
            Some(existing) => *existing = message,
            None => self.resource_load_warning = Some(message),
        }
    }

    pub(crate) fn save_state(&self) -> SaveState {
        self.save.state()
    }

    pub(crate) fn save_generation(&self) -> i64 {
        self.save.generation()
    }

    pub(crate) fn expected_revision(&self) -> i64 {
        self.expected_revision
    }

    #[cfg(test)]
    pub(crate) fn force_save_failure_for_test(&mut self, message: impl Into<String>) {
        self.save.fail(message);
    }

    /// A lifecycle action has asked for an exact durable confirmation.  A
    /// resource retry owns the same barrier even while its coordinator state
    /// is `Dirty`, so callers must not infer this solely from
    /// `Journaling`/`Snapshotting`.
    pub(crate) fn flush_confirmation_pending(&self) -> bool {
        self.pending_flush.is_some() || self.pending_resource_lifecycle_flush
    }

    /// A descriptor stage has no database-visible entity yet, but it owns a
    /// saved Selection and must be allowed to reach its one resource/note
    /// transaction. Lifecycle actions use this distinct fact to keep the
    /// session alive even while the text save coordinator is Clean.
    fn resource_lifecycle_operation_pending(&self) -> bool {
        self._resource_stage_task.is_some()
            || self.pending_staged_resource.is_some()
            || self._resource_commit_task.is_some()
            || self.pending_failed_resource_commit.is_some()
    }

    /// An external resource must not race a worker that owns an immutable
    /// journal/snapshot payload or a lifecycle flush barrier. The shell keeps
    /// the saved `InsertIntent` and retries through this exact fence once the
    /// retained worker completes; ordinary just-typed paste/drop therefore
    /// queues instead of being rejected as "saving".
    pub(crate) fn resource_insert_is_fenced(&self) -> bool {
        self._save_task.is_some()
            || self._resource_commit_task.is_some()
            || self.pending_failed_resource_commit.is_some()
            || self.pending_flush.is_some()
            || matches!(
                self.save.state(),
                SaveState::Journaling | SaveState::Snapshotting
            )
    }

    /// Register the exact current body selection before a picker/paste/drop
    /// leaves the editor. The completion path resolves this tracked selection
    /// against the latest document; it never consults a later live caret.
    pub(crate) fn capture_resource_insert_intent(
        &mut self,
        cx: &mut Context<Self>,
    ) -> Result<InsertIntent, SaveError> {
        let editor = self.editor.clone();
        let anchor = editor
            .update(cx, |editor, _| {
                editor.capture_resource_insert_anchor(editor.selection())
            })
            .map_err(|error| SaveError::new(error.to_string()))?;
        Ok(InsertIntent::new(self.note_id.clone(), anchor))
    }

    /// Capture a hit-tested selection from a drop coordinate. A Finder drop
    /// uses this instead of a raw layout point so later typing maps both ends
    /// through the same tracked-selection registry as paste and picker.
    pub(crate) fn capture_resource_insert_intent_at(
        &mut self,
        selection: Selection,
        cx: &mut Context<Self>,
    ) -> Result<InsertIntent, SaveError> {
        let editor = self.editor.clone();
        let anchor = editor
            .update(cx, |editor, _| {
                editor.capture_resource_insert_anchor(selection)
            })
            .map_err(|error| SaveError::new(error.to_string()))?;
        Ok(InsertIntent::new(self.note_id.clone(), anchor))
    }

    pub(crate) fn discard_resource_insert_intent(
        &mut self,
        intent: InsertIntent,
        cx: &mut Context<Self>,
    ) {
        if !intent.belongs_to(&self.note_id) {
            return;
        }
        self.editor.update(cx, |editor, _| {
            editor.discard_resource_insert_anchor(intent.anchor);
        });
    }

    /// Start the real picker/paste/drop resource route. The UI has already
    /// captured a tracked full Selection; it now hands over only a source
    /// descriptor/payload. Normalization, descriptor validation, hashing,
    /// blob fsync and the later SQLite snapshot never execute in this GPUI
    /// update callback.
    pub(crate) fn start_resource_import(
        &mut self,
        request: ResourceImportRequest,
        intent: InsertIntent,
        owned_temporary_paths: Vec<PathBuf>,
        cx: &mut Context<Self>,
    ) -> Result<(), SaveError> {
        // Own cleanup from the first instruction: a validation error or a
        // canceled retained task must remove only bridge-owned temp files.
        let temporary_paths = TemporaryResourcePaths(owned_temporary_paths);
        if self.is_read_only() {
            return Err(SaveError::new(if self.read_only {
                "废纸篓中的笔记为只读；请先恢复后再插入资源"
            } else {
                "资料库已提交，正在恢复界面；暂不可插入资源"
            }));
        }
        if !intent.belongs_to(&self.note_id) {
            return Err(SaveError::new(
                "资源插入意图不属于当前笔记；请重新选择插入位置",
            ));
        }
        if self._resource_stage_task.is_some()
            || self._resource_commit_task.is_some()
            || self.pending_staged_resource.is_some()
            || self.pending_failed_resource_commit.is_some()
        {
            return Err(SaveError::new("已有资源正在准备或提交；请稍后再试"));
        }
        if let SaveState::Failed(error) = self.save.state() {
            return Err(SaveError::new(format!("当前笔记保存失败：{error}")));
        }
        let job = ResourceStageJob {
            request,
            repository: Arc::clone(&self.repository),
            _temporary_paths: temporary_paths,
            #[cfg(test)]
            gate: self.next_resource_stage_gate.take(),
        };
        let task = cx
            .background_executor()
            .spawn(async move { Self::perform_resource_stage_on_worker(job).await });
        self._resource_stage_task = Some(cx.spawn(async move |this, cx| {
            let result = task.await;
            let _ = this.update(cx, |session, session_cx| {
                session._resource_stage_task = None;
                session.finish_resource_stage(result, intent, session_cx);
            });
        }));
        cx.notify();
        Ok(())
    }

    fn perform_resource_stage(job: ResourceStageJob) -> Result<StagedResourceInsert, SaveError> {
        let import = job.request.normalize()?;
        let (source, title, mime, extension, kind) = import.into_parts();
        let staged = match source {
            ResourceSource::Bytes(bytes) => job
                .repository
                .stage_resource(&bytes, &title, &mime, &extension)?,
            ResourceSource::File { file, size } => job
                .repository
                .stage_resource_reader(file, size, &title, &mime, &extension)?,
        };
        Ok(StagedResourceInsert { staged, kind })
    }

    async fn perform_resource_stage_on_worker(
        job: ResourceStageJob,
    ) -> Result<StagedResourceInsert, SaveError> {
        #[cfg(test)]
        if let Some(gate) = job.gate.as_ref() {
            gate.wait().await;
        }
        Self::perform_resource_stage(job)
    }

    fn finish_resource_stage(
        &mut self,
        result: Result<StagedResourceInsert, SaveError>,
        intent: InsertIntent,
        cx: &mut Context<Self>,
    ) {
        let staged = match result {
            Ok(staged) => staged,
            Err(error) => {
                self.discard_resource_insert_intent(intent, cx);
                // No live editor mutation or SQLite row exists on a staging
                // failure. The resource-specific notice remains visible via
                // the outcome, but the lifecycle token must not leave an
                // otherwise clean session permanently blocked.
                self.pending_resource_lifecycle_flush = false;
                self.resource_import_outcome = Some(Err(error));
                cx.notify();
                return;
            }
        };
        if self.is_read_only() {
            self.discard_resource_insert_intent(intent, cx);
            self.pending_resource_lifecycle_flush = false;
            self.resource_import_outcome = Some(Err(SaveError::new(if self.read_only {
                "废纸篓中的笔记为只读；已取消本次资源插入"
            } else {
                "资料库已提交，正在恢复界面；已取消本次资源插入，请恢复后重新选择"
            })));
            cx.notify();
            return;
        }
        // A Task-4 journal/snapshot may have started while the descriptor was
        // being copied. Preserve the tracked Selection and wait for that exact
        // writer instead of resolving a later live caret or racing its lease.
        if self.resource_insert_is_fenced() {
            self.pending_staged_resource = Some(PendingStagedResourceInsert { staged, intent });
            cx.notify();
            return;
        }
        if let Err(error) = self.start_staged_resource_commit(staged, intent.clone(), cx) {
            self.discard_resource_insert_intent(intent, cx);
            self.pending_resource_lifecycle_flush = false;
            self.resource_import_outcome = Some(Err(error));
            cx.notify();
        }
    }

    fn drive_pending_staged_resource(&mut self, cx: &mut Context<Self>) {
        if self.is_read_only() {
            if let Some(pending) = self.pending_staged_resource.take() {
                self.discard_resource_insert_intent(pending.intent, cx);
                self.pending_resource_lifecycle_flush = false;
                self.resource_import_outcome = Some(Err(SaveError::new(if self.read_only {
                    "废纸篓中的笔记为只读；已取消本次资源插入"
                } else {
                    "资料库已提交，正在恢复界面；已取消本次资源插入，请恢复后重新选择"
                })));
                cx.notify();
            }
            return;
        }
        if self.resource_insert_is_fenced() {
            return;
        }
        let Some(pending) = self.pending_staged_resource.take() else {
            return;
        };
        if let Err(error) =
            self.start_staged_resource_commit(pending.staged, pending.intent.clone(), cx)
        {
            self.discard_resource_insert_intent(pending.intent, cx);
            self.pending_resource_lifecycle_flush = false;
            self.resource_import_outcome = Some(Err(error));
            cx.notify();
        }
    }

    fn start_staged_resource_commit(
        &mut self,
        staged: StagedResourceInsert,
        intent: InsertIntent,
        cx: &mut Context<Self>,
    ) -> Result<(), SaveError> {
        if self.is_read_only() {
            return Err(SaveError::new(if self.read_only {
                "废纸篓中的笔记为只读；请先恢复后再插入资源"
            } else {
                "资料库已提交，正在恢复界面；暂不可插入资源"
            }));
        }
        if !intent.belongs_to(&self.note_id) {
            return Err(SaveError::new(
                "资源插入意图不属于当前笔记；请重新选择插入位置",
            ));
        }
        if self.title.read(cx).marked_range().is_some()
            || self.editor.read(cx).marked_text().is_some()
        {
            return Err(SaveError::new("输入法组合文本尚未确认，无法插入资源"));
        }
        if self.resource_insert_is_fenced() {
            self.pending_staged_resource = Some(PendingStagedResourceInsert { staged, intent });
            return Ok(());
        }
        if let SaveState::Failed(error) = self.save.state() {
            return Err(SaveError::new(format!("当前笔记保存失败：{error}")));
        }

        let resource_id = staged.staged.resource_id().clone();
        let selection = self
            .editor
            .update(cx, |editor, _| {
                editor.resolve_resource_insert_anchor(intent.anchor)
            })
            .map_err(|error| SaveError::new(error.to_string()))?;
        let transaction = match staged.kind {
            ResourceKind::Image { natural_size, .. } => Transaction::InsertImage {
                selection,
                resource_id: resource_id.as_str().to_owned(),
                natural_size,
            },
            ResourceKind::Attachment => Transaction::InsertAttachment {
                selection,
                resource_id: resource_id.as_str().to_owned(),
                filename: staged.staged.title().to_owned(),
                media_type: staged.staged.mime().to_owned(),
            },
        };
        let prepared_editor_commit = self
            .editor
            .read(cx)
            .prepare_durable_transaction(transaction)
            .map_err(|error| SaveError::new(error.to_string()))?;
        // Apply the validated structural transaction *before* the worker
        // leaves the UI thread.  This deliberately gives normal typing the
        // live, post-resource document to mutate while SQLite is busy.  The
        // worker below receives an immutable clone for durability only; it
        // never gets to swap that old clone back over later text.
        let mut resource_ids = self.resource_ids.clone();
        resource_ids.push(resource_id.clone());
        let snapshot = NativeSessionSnapshot {
            title: self.title.read(cx).text().to_owned(),
            document: prepared_editor_commit.document().clone(),
            allowed_resource_ids: resource_ids.clone(),
        };
        let image_materialization_root = matches!(staged.kind, ResourceKind::Image { .. })
            .then(|| self.editor.read(cx).image_materialization_root());
        // `observe_entities` reads this vector when the editor notification
        // arrives.  Publish it before the notification so an immediately
        // following keystroke journals the resource relation with its text,
        // never a resource block whose relation was accidentally omitted.
        extend_resource_allowlist(&mut self.resource_ids, &resource_ids);
        let kind = staged.kind;
        let staged_size = staged.staged.size() as u64;
        let initial_presentation_warning = self.editor.update(cx, |editor, editor_cx| {
            editor.install_prepared_durable_commit(prepared_editor_commit);
            let registration = match kind {
                ResourceKind::Image { natural_size, .. } => {
                    // The source will be streamed into the session-private
                    // cache by the worker. Until then, preserve the real
                    // image atom and its fixed geometry with a visible
                    // loading/failed placeholder rather than hide the block
                    // or defer it until a note reopen.
                    editor.register_unavailable_image(resource_id.as_str(), natural_size)
                }
                ResourceKind::Attachment => {
                    editor.register_attachment(resource_id.as_str(), staged_size)
                }
            };
            editor_cx.notify();
            registration.err().map(|error| error.to_string())
        });
        if let Some(warning) = initial_presentation_warning {
            self.push_resource_load_warning(format!("资源正在保存，当前显示可能受限：{warning}"));
        }
        // Force the same semantic observation that the retained entity
        // subscription sees. This records an immutable resource-generation
        // candidate before the worker can complete, while a later input gets
        // a newer generation and is therefore never clobbered on completion.
        self.observe_current_entities(cx);
        let resource_generation = self.save.generation();
        let job = ResourceCommitJob {
            note_id: self.note_id.clone(),
            expected_revision: self.expected_revision,
            journal_ownership: self.journal_ownership.clone(),
            repository: Arc::clone(&self.repository),
            staged,
            snapshot,
            resource_generation,
            image_materialization_root,
            #[cfg(test)]
            gate: self.next_resource_commit_gate.take(),
            #[cfg(test)]
            injected_failure: self.next_resource_commit_failure.take(),
        };
        self.spawn_resource_commit(job, cx);
        cx.notify();
        Ok(())
    }

    fn spawn_resource_commit(&mut self, job: ResourceCommitJob, cx: &mut Context<Self>) {
        let task = cx
            .background_executor()
            .spawn(async move { Self::perform_resource_commit_on_worker(job).await });
        self._resource_commit_task = Some(cx.spawn(async move |this, cx| {
            let result = task.await;
            let _ = this.update(cx, |session, session_cx| {
                session._resource_commit_task = None;
                session.finish_resource_commit(result, session_cx);
            });
        }));
    }

    /// A context-free worker body: it encodes the immutable optimistic
    /// snapshot, makes the resource/note relation visible in one SQLite
    /// transaction, and streams the verified image into the editor-owned
    /// cache path. It has no entity handle and cannot overwrite live typing.
    fn perform_resource_commit(job: ResourceCommitJob) -> ResourceCommitCompletion {
        let ResourceCommitJob {
            note_id,
            expected_revision,
            journal_ownership,
            repository,
            staged,
            snapshot: native_snapshot,
            resource_generation,
            image_materialization_root,
            #[cfg(test)]
                gate: _,
            #[cfg(test)]
                injected_failure: _,
        } = job;
        let resource_id = staged.staged.resource_id().clone();
        let snapshot = match Self::encode_snapshot(native_snapshot, "编码资源插入快照") {
            Ok(snapshot) => snapshot,
            Err(error) => return ResourceCommitCompletion::Failure { staged, error },
        };
        let committed = match repository.commit_staged_resource_snapshot(
            SaveNote {
                id: note_id,
                expected_revision,
                title: snapshot.title.clone(),
                document: snapshot.document.clone(),
                resource_ids: snapshot.resource_ids.clone(),
                selected_thumbnail_id: None,
            },
            journal_ownership,
            &staged.staged,
        ) {
            Ok(committed) => committed,
            Err(error) => {
                return ResourceCommitCompletion::Failure {
                    staged,
                    error: error.into(),
                };
            }
        };
        let materialized_image_source = match staged.kind {
            ResourceKind::Image {
                format,
                natural_size,
            } => {
                let Some(root) = image_materialization_root.as_ref() else {
                    return ResourceCommitCompletion::Success(ResourceCommitSuccess {
                        committed,
                        resource_id,
                        kind: staged.kind,
                        snapshot,
                        resource_generation,
                        materialized_image_source: Err("图片资源缺少编辑器缓存目标".to_owned()),
                        attachment_size: None,
                    });
                };
                match repository.open_verified_resource_file(&resource_id) {
                    Ok(Some((_metadata, file))) => {
                        crate::native_editor::images::ImageStore::materialize_durable_reader_at(
                            root,
                            &crate::native_editor::images::ImageMetadata::new(
                                resource_id.as_str(),
                                natural_size.0,
                                natural_size.1,
                            ),
                            file,
                            format,
                        )
                        .map(Some)
                        .map_err(|error| error.to_string())
                    }
                    Ok(None) => Err("资源记录或字节不存在".to_owned()),
                    Err(error) => Err(error.to_string()),
                }
            }
            ResourceKind::Attachment => Ok(None),
        };
        let attachment_size =
            matches!(staged.kind, ResourceKind::Attachment).then_some(staged.staged.size() as u64);
        ResourceCommitCompletion::Success(ResourceCommitSuccess {
            committed,
            resource_id,
            kind: staged.kind,
            snapshot,
            resource_generation,
            materialized_image_source,
            attachment_size,
        })
    }

    async fn perform_resource_commit_on_worker(job: ResourceCommitJob) -> ResourceCommitCompletion {
        #[cfg(test)]
        if let Some(gate) = job.gate.as_ref() {
            gate.wait().await;
        }
        #[cfg(test)]
        if let Some(error) = job.injected_failure.clone() {
            return ResourceCommitCompletion::Failure {
                staged: job.staged,
                error,
            };
        }
        Self::perform_resource_commit(job)
    }

    fn finish_resource_commit(&mut self, result: ResourceCommitCompletion, cx: &mut Context<Self>) {
        let outcome = match result {
            ResourceCommitCompletion::Success(success) => {
                let ResourceCommitSuccess {
                    committed,
                    resource_id,
                    kind,
                    snapshot,
                    resource_generation,
                    materialized_image_source,
                    attachment_size,
                } = success;
                let note = committed.note;
                let selected_thumbnail_id = committed.selected_thumbnail_id;
                let presentation_warning = self.editor.update(cx, |editor, editor_cx| {
                    let registration = match kind {
                        ResourceKind::Image {
                            format,
                            natural_size,
                        } => match materialized_image_source {
                            Ok(Some(source)) => editor.register_materialized_durable_image(
                                resource_id.as_str(),
                                natural_size,
                                source,
                                format,
                            ),
                            Ok(None) => Err(
                                crate::native_editor::model::DocumentError::InvalidOperation(
                                    "图片资源缺少已物化缓存来源".into(),
                                ),
                            ),
                            Err(error) => Err(
                                crate::native_editor::model::DocumentError::InvalidOperation(
                                    format!(
                                        "unable to materialize durable image cache source: {error}"
                                    ),
                                ),
                            ),
                        },
                        ResourceKind::Attachment => attachment_size
                            .ok_or_else(|| {
                                crate::native_editor::model::DocumentError::InvalidOperation(
                                    "附件资源缺少持久化大小".into(),
                                )
                            })
                            .and_then(|size| {
                                editor.register_attachment(resource_id.as_str(), size)
                            }),
                    };
                    let warning = registration.err().map(|error| {
                        if matches!(kind, ResourceKind::Image { .. }) {
                            if let ResourceKind::Image { natural_size, .. } = kind {
                                let _ = editor
                                    .register_unavailable_image(resource_id.as_str(), natural_size);
                            }
                        }
                        error.to_string()
                    });
                    editor_cx.notify();
                    warning
                });
                let previous_expected_revision = self.expected_revision;
                self.expected_revision = note.revision;
                self.last_saved = SavedRevision {
                    revision: note.revision,
                    saved_time: note.updated_time,
                };
                extend_resource_allowlist(&mut self.resource_ids, &snapshot.resource_ids);
                self.journal_base = snapshot;
                self.journal_ownership = None;
                // A newer input generation may have changed title/body while
                // this SQLite worker was running. The live editor and its
                // semantic snapshot already hold that newer state; advance
                // only the durable base, never swap an old prepared document
                // back into the entity.
                let completes_current_generation =
                    self.save.is_current_generation(resource_generation);
                let completes_pending_flush = self.pending_flush.is_some_and(|barrier| {
                    completes_current_generation
                        && barrier.generation == resource_generation
                        && barrier.expected_revision == previous_expected_revision
                        && note.revision == previous_expected_revision.saturating_add(1)
                });
                if !matches!(self.save.state(), SaveState::Failed(_)) {
                    self.save.snapshotted(resource_generation);
                    if completes_current_generation {
                        self._journal_deadline_task = None;
                        self._settled_deadline_task = None;
                        self._hard_deadline_task = None;
                    }
                    if completes_pending_flush {
                        self.pending_flush = None;
                    } else if let Some(barrier) = self.pending_flush.as_mut() {
                        barrier.generation = self.save.generation();
                        barrier.expected_revision = self.expected_revision;
                    }
                }
                // This exact resource/note transaction is now durable. Only
                // now may a lifecycle action that was blocked during either
                // descriptor staging or the SQLite half be retried.
                self.pending_resource_lifecycle_flush = false;
                Ok(InsertedResource {
                    note,
                    resource_id,
                    is_image: matches!(kind, ResourceKind::Image { .. }),
                    selected_thumbnail_id,
                    presentation_warning,
                })
            }
            ResourceCommitCompletion::Failure { staged, error } => {
                let resource_id = staged.staged.resource_id().clone();
                let rollback = self
                    .editor
                    .update(cx, |editor, editor_cx| {
                        let result =
                            editor.rollback_failed_optimistic_resource(resource_id.as_str());
                        if result.as_ref().is_ok_and(|rolled_back| *rolled_back) {
                            editor_cx.notify();
                        }
                        result
                    })
                    .map_err(|update_error| SaveError::new(update_error.to_string()));
                match rollback {
                    Ok(true) => {
                        // The failed atom and any presentation cache source
                        // are gone from the live document. Publish a fresh
                        // immutable candidate so later text can snapshot
                        // normally; no old resource relation survives.
                        self.resource_ids.retain(|id| id != &resource_id);
                        self.pending_resource_lifecycle_flush = false;
                        self.save.fail(error.to_string());
                        self.observe_current_entities(cx);
                        Err(error)
                    }
                    Ok(false) | Err(_) => {
                        // A history suffix may refer to a structural node
                        // introduced by the resource insertion. Rather than
                        // lose that later input, retain the verified staged
                        // source and require an explicit flush/retry. The
                        // blob remains invisible to SQLite until it wins the
                        // same atomic note snapshot transaction.
                        self.pending_failed_resource_commit = Some(staged);
                        self.save.fail(error.to_string());
                        Err(SaveError::new(format!(
                            "{error}；资源仍在暂存区，手动同步可重试且不会丢失后续输入"
                        )))
                    }
                }
            }
        };
        self.resource_import_outcome = Some(outcome);
        self.drive_pending_flush_or_due_work(cx);
        cx.notify();
    }

    /// Retry the exact staged source that a failed optimistic commit kept in
    /// memory. This is deliberately an explicit user/lifecycle flush path:
    /// automatic timers must never silently make a previously failed
    /// cross-store transaction visible. The worker snapshots the *current*
    /// immutable entity state, so text typed in the insertion-created right
    /// paragraph is included without reinstalling an old editor clone.
    ///
    /// `Ok(true)` means a retained resource worker was started. `Ok(false)`
    /// means the person already removed the optimistic atom, so the stage was
    /// discarded and the caller can continue with an ordinary snapshot of the
    /// remaining document.
    fn retry_failed_resource_commit(&mut self, cx: &mut Context<Self>) -> Result<bool, SaveError> {
        if self.is_read_only() {
            return Err(SaveError::new(if self.read_only {
                "废纸篓中的笔记为只读；请先恢复后再重试资源"
            } else {
                "资料库已提交，正在恢复界面；暂不可重试资源"
            }));
        }
        if self._resource_commit_task.is_some() {
            return Err(SaveError::new("资源重试已在后台进行"));
        }
        let Some(staged) = self.pending_failed_resource_commit.take() else {
            return Ok(false);
        };
        let resource_id = staged.staged.resource_id().clone();
        let resource_is_live = self
            .editor
            .read(cx)
            .document()
            .blocks()
            .iter()
            .any(|block| {
                matches!(
                    &block.content,
                    BlockContent::Image { resource_id: id, .. }
                        | BlockContent::Attachment { resource_id: id, .. }
                        if id == resource_id.as_str()
                )
            });
        if !resource_is_live {
            // The user explicitly removed the optimistic node before retrying.
            // Do not write its staged metadata or leave a phantom relation in
            // a later ordinary snapshot. Transfer the lifecycle request to an
            // ordinary exact snapshot barrier: the resource-specific token
            // must remain visible until that snapshot is durable, but may not
            // outlive it and turn Clean into a permanently blocked session.
            self.resource_ids.retain(|id| id != &resource_id);
            let editor = self.editor.clone();
            let title = self.title.read(cx).text().to_owned();
            self.observed_title = title.clone();
            self.observed_document = editor.read(cx).document().semantic_snapshot();
            self.committed_snapshot = self.capture_committed_snapshot(title, &editor, cx);
            let generation = self.save.mark_dirty();
            self.pending_flush = Some(FlushBarrier {
                generation,
                expected_revision: self.expected_revision,
            });
            return Ok(false);
        }

        // Capture all mutations that happened while the original worker was
        // gated/failed before generating a new generation from the visible
        // live state. `mark_dirty` is the one legitimate transition out of
        // `Failed`; it also makes this retry independently stale-safe.
        self.observe_current_entities(cx);
        let resource_generation = self.save.mark_dirty();
        self.pending_flush = Some(FlushBarrier {
            generation: resource_generation,
            expected_revision: self.expected_revision,
        });
        let image_materialization_root = matches!(staged.kind, ResourceKind::Image { .. })
            .then(|| self.editor.read(cx).image_materialization_root());
        let job = ResourceCommitJob {
            note_id: self.note_id.clone(),
            expected_revision: self.expected_revision,
            journal_ownership: self.journal_ownership.clone(),
            repository: Arc::clone(&self.repository),
            staged,
            snapshot: self.snapshot(),
            resource_generation,
            image_materialization_root,
            #[cfg(test)]
            gate: self.next_resource_commit_gate.take(),
            #[cfg(test)]
            injected_failure: self.next_resource_commit_failure.take(),
        };
        self.spawn_resource_commit(job, cx);
        cx.notify();
        Ok(true)
    }

    /// The shell consumes the exact post-commit result from its retained
    /// session observer, which keeps `AppModel` projection updates on the one
    /// normal model path while all staging/SQLite work stays background.
    pub(crate) fn take_resource_import_outcome(
        &mut self,
    ) -> Option<Result<InsertedResource, SaveError>> {
        self.resource_import_outcome.take()
    }

    /// Return the latest complete note produced by the ordinary title/body
    /// snapshot transaction.  This is deliberately separate from resource
    /// import outcomes: a normal save has no resource-side presentation work,
    /// but the model still needs its exact durable body before an
    /// organization action may build a replacement active session.
    pub(crate) fn take_saved_note_outcome(&mut self) -> Option<Note> {
        self.saved_note_outcome.take()
    }

    /// Start the explicit attachment-open path. It is independent from save
    /// fences: selecting or opening a durable card is not a document
    /// mutation, so a concurrent journal/snapshot must not swallow a
    /// double-click. The detached worker reopens a verified descriptor and
    /// streams it into a worker-owned private lease. Detaching is intentional:
    /// a note switch must not cancel an in-progress platform handoff halfway
    /// through and delete its source before the opener returns.
    pub(crate) fn open_attachment(
        &mut self,
        resource_id: String,
        cx: &mut Context<Self>,
    ) -> Result<(), SaveError> {
        if self.attachment_open_in_flight {
            return Err(SaveError::new("已有附件正在交给系统默认应用打开"));
        }
        let resource_id = ResourceId::new(&resource_id)
            .map_err(|_| SaveError::new("附件标识无效，无法安全打开"))?;
        let exists_in_live_document = self.editor.read(cx).document().blocks().iter().any(
            |block| {
                matches!(
                    &block.content,
                    BlockContent::Attachment { resource_id: id, .. } if id == resource_id.as_str()
                )
            },
        );
        if !exists_in_live_document {
            return Err(SaveError::new("附件已不在当前文档中，未执行打开操作"));
        }
        let job = AttachmentOpenJob {
            repository: Arc::clone(&self.repository),
            resource_id,
            opener: Arc::clone(&self.attachment_opener),
            #[cfg(test)]
            gate: self.next_attachment_open_gate.take(),
        };
        let task = cx
            .background_executor()
            .spawn(async move { Self::perform_attachment_open(job).await });
        self.attachment_open_in_flight = true;
        cx.spawn(async move |this, cx| {
            let result = task.await;
            let _ = this.update(cx, |session, session_cx| {
                session.attachment_open_in_flight = false;
                session.attachment_open_outcome = Some(match result {
                    Ok(completion) => {
                        let resource_id = completion.resource_id.clone();
                        session._opened_attachment_sources.push(completion.lease);
                        Ok(AttachmentOpenSuccess { resource_id })
                    }
                    Err(error) => Err(error),
                });
                session_cx.notify();
            });
        })
        .detach();
        cx.notify();
        Ok(())
    }

    async fn perform_attachment_open(
        job: AttachmentOpenJob,
    ) -> Result<AttachmentOpenCompletion, SaveError> {
        let Some((metadata, file)) = job
            .repository
            .open_verified_resource_file(&job.resource_id)?
        else {
            return Err(SaveError::new("附件资源或字节已不存在"));
        };
        let mut lease = AttachmentMaterializationLease::new()?;
        let path = materialize_verified_attachment_reader(
            lease.root(),
            &job.resource_id,
            &metadata.file_extension,
            file,
        )?;
        lease.record_source(path.clone());
        #[cfg(test)]
        if let Some(gate) = job.gate.as_ref() {
            gate.wait_after_materialization(&path).await;
        }
        if let Err(error) = (job.opener)(&path) {
            return Err(SaveError::new(format!("无法打开附件：{error}")));
        }
        Ok(AttachmentOpenCompletion {
            resource_id: job.resource_id,
            lease,
        })
    }

    pub(crate) fn take_attachment_open_outcome(
        &mut self,
    ) -> Option<Result<AttachmentOpenSuccess, SaveError>> {
        self.attachment_open_outcome.take()
    }

    #[cfg(test)]
    pub(crate) fn set_attachment_opener_for_test(&mut self, opener: AttachmentOpener) {
        self.attachment_opener = opener;
    }

    /// Complete one durable resource insertion. The resource metadata is
    /// imported first, then a cloned document is structurally validated and
    /// canonically encoded, and finally one ownership-aware snapshot commits
    /// both the note-resource order and the document block. No editor entity
    /// is notified until that snapshot has succeeded.
    #[cfg(test)]
    pub(crate) fn insert_resource(
        &mut self,
        import: ResourceImport,
        intent: InsertIntent,
        cx: &mut Context<Self>,
    ) -> Result<InsertedResource, SaveError> {
        if self.is_read_only() {
            return Err(SaveError::new(if self.read_only {
                "废纸篓中的笔记为只读；请先恢复后再插入资源"
            } else {
                "资料库已提交，正在恢复界面；暂不可插入资源"
            }));
        }
        if !intent.belongs_to(&self.note_id) {
            return Err(SaveError::new(
                "资源插入意图不属于当前笔记；请重新选择插入位置",
            ));
        }
        if self.title.read(cx).marked_range().is_some()
            || self.editor.read(cx).marked_text().is_some()
        {
            return Err(SaveError::new("输入法组合文本尚未确认，无法插入资源"));
        }
        if self.resource_insert_is_fenced() {
            return Err(SaveError::new(
                "当前笔记正在保存，资源插入将在保存完成后可用",
            ));
        }
        if let SaveState::Failed(error) = self.save.state() {
            return Err(SaveError::new(format!("当前笔记保存失败：{error}")));
        }

        // Resolve only after the writer fence and failure gate. A queued
        // completion retains its tracked Selection while a Task-4 snapshot
        // owns the current generation; a stale/removed anchor then fails
        // visibly instead of falling back to whatever caret is now active.
        let selection = self
            .editor
            .update(cx, |editor, _| {
                editor.resolve_resource_insert_anchor(intent.anchor)
            })
            .map_err(|error| SaveError::new(error.to_string()))?;

        let (source, title, mime, extension, kind) = import.into_parts();
        let source_size = source.len();
        // Blob staging deliberately has no SQLite row, outbox entry, or
        // event. Picker/Finder file descriptors take the reader path, so the
        // full source never becomes a second resource-sized `Vec` in the UI.
        // Only the final snapshot transaction below makes this resource and
        // its note relation visible together.
        let staged = match source {
            ResourceSource::Bytes(bytes) => self
                .repository
                .stage_resource(&bytes, &title, &mime, &extension)?,
            ResourceSource::File { file, size } => self
                .repository
                .stage_resource_reader(file, size, &title, &mime, &extension)?,
        };
        let imported = staged.resource_id().clone();
        let is_image = matches!(kind, ResourceKind::Image { .. });
        let transaction = match kind {
            ResourceKind::Image { natural_size, .. } => Transaction::InsertImage {
                selection,
                resource_id: imported.as_str().to_owned(),
                natural_size,
            },
            ResourceKind::Attachment => Transaction::InsertAttachment {
                selection,
                resource_id: imported.as_str().to_owned(),
                filename: title.clone(),
                media_type: mime.clone(),
            },
        };

        // The expensive/fallible document+history application happens before
        // the cross-store snapshot. After SQLite commits, this prepared value
        // is swapped into the retained editor without replaying an operation.
        let prepared_editor_commit = match self
            .editor
            .read(cx)
            .prepare_durable_transaction(transaction.clone())
        {
            Ok(prepared) => prepared,
            Err(error) => return Err(SaveError::new(error.to_string())),
        };
        let mut available_resources = self.resource_ids.clone();
        available_resources.push(imported.clone());
        let document = match export_canonical_with_resources(
            prepared_editor_commit.document(),
            Some(&available_resources),
        ) {
            Ok(document) => document,
            Err(error) => {
                return Err(SaveError::new(error.to_string()));
            }
        };
        let snapshot = SessionSnapshot {
            title: self.title.read(cx).text().to_owned(),
            resource_ids: document.resource_ids(),
            document,
        };
        let committed = match self.repository.commit_staged_resource_snapshot(
            SaveNote {
                id: self.note_id.clone(),
                expected_revision: self.expected_revision,
                title: snapshot.title.clone(),
                document: snapshot.document.clone(),
                resource_ids: snapshot.resource_ids.clone(),
                // `selected_thumbnail_id` is explicit note projection state.
                // A normal resource insertion must preserve an already valid
                // user-selected cover; the repository chooses the first image
                // only when this note has never had one.
                selected_thumbnail_id: None,
            },
            self.journal_ownership.clone(),
            &staged,
        ) {
            Ok(committed) => committed,
            Err(error) => return Err(SaveError::new(error.to_string())),
        };
        let note = committed.note;
        let selected_thumbnail_id = committed.selected_thumbnail_id;

        let durable_image_source = match kind {
            ResourceKind::Image {
                format,
                natural_size,
            } => match self.repository.open_verified_resource_file(&imported) {
                Ok(Some((_metadata, file))) => Ok(Some((file, format, natural_size))),
                Ok(None) => Err("资源记录或字节不存在".to_owned()),
                Err(error) => Err(error.to_string()),
            },
            ResourceKind::Attachment => Ok(None),
        };
        let durable_attachment_size =
            matches!(kind, ResourceKind::Attachment).then_some(source_size as u64);
        let editor = self.editor.clone();
        let presentation_warning = editor.update(cx, |editor, editor_cx| {
            editor.install_prepared_durable_commit(prepared_editor_commit);
            let image_registration = match durable_image_source {
                Ok(Some((file, format, natural_size))) => editor.register_durable_image_reader(
                    imported.as_str(),
                    natural_size,
                    file,
                    format,
                ),
                Ok(None) => Ok(()),
                Err(error) => Err(
                    crate::native_editor::model::DocumentError::InvalidOperation(format!(
                        "unable to materialize durable image cache source: {error}"
                    )),
                ),
            };
            let attachment_registration = durable_attachment_size
                .map(|size| editor.register_attachment(imported.as_str(), size))
                .transpose();
            editor_cx.notify();
            let mut warnings = Vec::new();
            if let Err(error) = image_registration {
                warnings.push(error.to_string());
            }
            if let Err(error) = attachment_registration {
                warnings.push(error.to_string());
            }
            (!warnings.is_empty()).then(|| warnings.join("；"))
        });
        // The database commit is authoritative at this point. The prepared
        // editor state has already been validated and was installed by a
        // non-fallible swap; a cache-source failure is only a visible
        // presentation warning and never rewrites the durable outcome.

        self.expected_revision = note.revision;
        self.last_saved = SavedRevision {
            revision: note.revision,
            saved_time: note.updated_time,
        };
        extend_resource_allowlist(&mut self.resource_ids, &snapshot.resource_ids);
        self.journal_base = snapshot;
        self.journal_ownership = None;
        self.observed_title = self.title.read(cx).text().to_owned();
        self.observed_document = editor.read(cx).document().semantic_snapshot();
        self.committed_snapshot = NativeSessionSnapshot {
            title: self.observed_title.clone(),
            document: editor.read(cx).document().clone(),
            allowed_resource_ids: self.resource_ids.clone(),
        };
        if matches!(self.save.state(), SaveState::Dirty) {
            self.save.snapshotted(self.save.generation());
            self._journal_deadline_task = None;
            self._settled_deadline_task = None;
            self._hard_deadline_task = None;
        }
        cx.notify();
        Ok(InsertedResource {
            note,
            resource_id: imported,
            is_image,
            selected_thumbnail_id,
            presentation_warning,
        })
    }

    /// Drain the renderer's viewport-resident image IDs into this retained
    /// session. The small queue is intentionally independent from the save
    /// coordinator: cache materialization never reads live title/body state,
    /// cannot create a journal by itself, and remains cancellable with this
    /// entity when a note/window goes away.
    fn drain_image_hydration_requests(
        &mut self,
        editor: &Entity<EditorCore>,
        cx: &mut Context<Self>,
    ) {
        let requested = editor.update(cx, |editor, _| {
            editor.take_pending_image_hydration_requests()
        });
        for resource_id in requested {
            if self.persisted_image_hydration.contains_key(&resource_id) {
                // Coalesce behind the one active worker. The core has already
                // pruned by the latest visible/prefetch set; replacing this
                // slot avoids rebuilding a long historical scroll queue in
                // the retained session between paint notifications.
                self.pending_image_hydration.clear();
                self.pending_image_hydration.push_back(resource_id);
            } else {
                // The renderer may race a note refresh that removed the
                // descriptor metadata. Release the core active marker so a
                // later valid residency request is not permanently blocked.
                let _ = editor.update(cx, |editor, editor_cx| {
                    editor.finish_image_hydration_request(&resource_id, false);
                    editor_cx.notify();
                });
            }
        }
        self.start_next_image_hydration(cx);
    }

    fn start_next_image_hydration(&mut self, cx: &mut Context<Self>) {
        if self._image_hydration_task.is_some() {
            return;
        }
        // Metadata can disappear after a projection refresh while a render
        // notification is still queued. Skip it and continue so one stale ID
        // cannot starve a later resident image.
        while let Some(resource_id) = self.pending_image_hydration.pop_front() {
            let Some(hydration) = self.persisted_image_hydration.get(&resource_id).cloned() else {
                let _ = self.editor.update(cx, |editor, editor_cx| {
                    editor.finish_image_hydration_request(&resource_id, false);
                    editor_cx.notify();
                });
                continue;
            };
            let image_staging_parent = self
                .editor
                .read(cx)
                .image_materialization_root()
                .parent()
                .map(Path::to_path_buf)
                .unwrap_or_else(std::env::temp_dir);
            let job = ImageHydrationJob {
                hydration,
                repository: Arc::clone(&self.repository),
                image_staging_parent,
                #[cfg(test)]
                gate: self.next_image_hydration_gate.take(),
            };
            let task = cx
                .background_executor()
                .spawn(async move { Self::perform_image_hydration_on_worker(job).await });
            self._image_hydration_task = Some(cx.spawn(async move |this, cx| {
                let result = task.await;
                let _ = this.update(cx, |session, session_cx| {
                    session._image_hydration_task = None;
                    session.finish_image_hydration(result, session_cx);
                });
            }));
            return;
        }
    }

    fn perform_image_hydration(job: ImageHydrationJob) -> ImageHydrationCompletion {
        let ImageHydrationJob {
            hydration,
            repository,
            image_staging_parent,
            #[cfg(test)]
                gate: _,
        } = job;
        let result = (|| -> Result<
            (
                ImageHydrationStaging,
                gpui::ImageFormat,
                (u32, u32),
                (u32, u32),
            ),
            SaveError,
        > {
            let mut staging = ImageHydrationStaging::new(&image_staging_parent);
            let (metadata, inspect_file) = repository
                .open_verified_resource_file(&hydration.resource_id)?
                .ok_or_else(|| SaveError::new("资源记录或字节不存在"))?;
            let (format, measured_natural_size) = inspect_persisted_image(inspect_file, &metadata.mime)
                .map_err(|error| SaveError::new(format!("资源无法解码: {error}")))?;
            // Reopen the descriptor after inspection: the first verified
            // stream was consumed by format/dimension validation, while this
            // second streaming pass copies only a fixed 64KiB buffer into the
            // editor-private source path.
            let (_, materialize_file) = repository
                .open_verified_resource_file(&hydration.resource_id)?
                .ok_or_else(|| SaveError::new("资源记录或字节不存在"))?;
            let cache_natural_size = if hydration.legacy_node_ids.is_empty() {
                hydration.natural_size
            } else {
                measured_natural_size
            };
            let source = crate::native_editor::images::ImageStore::materialize_durable_reader_at(
                &staging.root,
                &crate::native_editor::images::ImageMetadata::new(
                    hydration.resource_id.as_str(),
                    cache_natural_size.0,
                    cache_natural_size.1,
                ),
                materialize_file,
                format,
            )
            .map_err(|error| SaveError::new(format!("无法物化图片缓存：{error}")))?;
            staging.record_source(source);
            Ok((
                staging,
                format,
                cache_natural_size,
                measured_natural_size,
            ))
        })();
        match result {
            Ok((staging, format, cache_natural_size, measured_natural_size)) => {
                ImageHydrationCompletion::Success {
                    hydration,
                    staging,
                    format,
                    cache_natural_size,
                    measured_natural_size,
                }
            }
            Err(error) => ImageHydrationCompletion::Failure { hydration, error },
        }
    }

    async fn perform_image_hydration_on_worker(job: ImageHydrationJob) -> ImageHydrationCompletion {
        #[cfg(test)]
        if let Some(gate) = job.gate.as_ref() {
            gate.wait().await;
        }
        Self::perform_image_hydration(job)
    }

    fn document_contains_image_resource(&self, resource_id: &str, cx: &mut Context<Self>) -> bool {
        self.editor
            .read(cx)
            .document()
            .blocks()
            .iter()
            .any(|block| {
                matches!(
                    &block.content,
                    BlockContent::Image { resource_id: candidate, .. } if candidate == resource_id
                )
            })
    }

    fn finish_image_hydration(&mut self, result: ImageHydrationCompletion, cx: &mut Context<Self>) {
        match result {
            ImageHydrationCompletion::Success {
                hydration,
                staging,
                format,
                cache_natural_size,
                measured_natural_size,
            } => {
                let resource_id = hydration.resource_id.as_str().to_owned();
                if !self.document_contains_image_resource(&resource_id, cx) {
                    // The session stayed alive but a user removed the atom
                    // while this source was being copied. Never register a
                    // stale cache entry. Dropping the task-owned sibling
                    // staging directory cleans its bytes without ever
                    // recreating the editor-owned root.
                    drop(staging);
                    let _ = self.editor.update(cx, |editor, editor_cx| {
                        editor.finish_image_hydration_request(&resource_id, false);
                        editor_cx.notify();
                    });
                    self.start_next_image_hydration(cx);
                    return;
                }
                let registration = self.editor.update(cx, |editor, editor_cx| {
                    let source = staging
                        .adopt_into(&editor.image_materialization_root())
                        .map_err(|error| {
                            crate::native_editor::model::DocumentError::InvalidOperation(format!(
                                "unable to adopt durable image cache source: {error}"
                            ))
                        })?;
                    let result = editor.register_materialized_durable_image(
                        &resource_id,
                        cache_natural_size,
                        source,
                        format,
                    );
                    editor.finish_image_hydration_request(&resource_id, result.is_ok());
                    editor_cx.notify();
                    result
                });
                match registration {
                    Ok(()) => {
                        if !hydration.legacy_node_ids.is_empty() {
                            self.pending_legacy_image_repairs.push_back(
                                LegacyImageGeometryRepair {
                                    resource_id,
                                    node_ids: hydration.legacy_node_ids,
                                    natural_size: measured_natural_size,
                                },
                            );
                        }
                    }
                    Err(error) => {
                        self.push_resource_load_warning(format!("已保存图片暂无法显示：{error}"));
                    }
                }
            }
            ImageHydrationCompletion::Failure { hydration, error } => {
                let resource_id = hydration.resource_id.as_str().to_owned();
                if self.document_contains_image_resource(&resource_id, cx) {
                    let _ = self.editor.update(cx, |editor, editor_cx| {
                        let _ =
                            editor.register_unavailable_image(&resource_id, hydration.natural_size);
                        editor.finish_image_hydration_request(&resource_id, false);
                        editor_cx.notify();
                    });
                    self.push_resource_load_warning(format!(
                        "图片 {} 暂不可用：{error}",
                        hydration.resource_id.as_str()
                    ));
                } else {
                    let _ = self.editor.update(cx, |editor, editor_cx| {
                        editor.finish_image_hydration_request(&resource_id, false);
                        editor_cx.notify();
                    });
                }
            }
        }
        self.start_next_image_hydration(cx);
        cx.notify();
    }

    fn apply_pending_legacy_image_repairs(
        &mut self,
        editor: &Entity<EditorCore>,
        cx: &mut Context<Self>,
    ) {
        // Image materialization remains a presentation concern for a Trash
        // preview, but legacy geometry repair mutates the canonical document
        // and schedules a save. A deleted note may never take that semantic
        // path merely because it became visible. A temporary reconciliation
        // lock merely postpones the repair until its full candidate unlocks;
        // dropping that queue would permanently preserve legacy geometry.
        if self.read_only {
            self.pending_legacy_image_repairs.clear();
            return;
        }
        if self.reconciliation_locked {
            return;
        }
        if editor.read(cx).marked_text().is_some() || self.title.read(cx).marked_range().is_some() {
            return;
        }
        let mut changed = false;
        while let Some(repair) = self.pending_legacy_image_repairs.pop_front() {
            let repaired = editor.update(cx, |editor, editor_cx| {
                let result = editor.repair_legacy_image_natural_sizes(
                    &repair.resource_id,
                    &repair.node_ids,
                    repair.natural_size,
                );
                if result.as_ref().is_ok_and(|changed| *changed) {
                    editor_cx.notify();
                }
                result
            });
            match repaired {
                Ok(true) => changed = true,
                Ok(false) => {}
                Err(error) => {
                    self.push_resource_load_warning(format!("无法修复旧图片尺寸：{error}"))
                }
            }
        }
        if changed {
            cx.notify();
        }
    }

    fn observe_entities(
        &mut self,
        title: &Entity<TitleInput>,
        editor: &Entity<EditorCore>,
        cx: &mut Context<Self>,
    ) {
        // Marked text is provisional. It is painted and selectable, but must
        // not become a journal/snapshot until the platform commits or unmasks
        // the composition through the same input handler.
        let title_marked = title.read(cx).marked_range().is_some();
        let editor_marked = editor.read(cx).marked_text().is_some();
        if title_marked || editor_marked {
            // Invalidate the scheduler before it can observe the mutable
            // entity again. The previously captured payload stays immutable
            // and may resume only after this composition resolves.
            self.save.freeze_for_composition();
            cx.notify();
            return;
        }
        // A renderer notification can carry only a viewport hydration demand
        // (no semantic document change). Drain it before the equality return
        // below; otherwise a freshly painted placeholder would never start
        // its retained background worker.
        self.apply_pending_legacy_image_repairs(editor, cx);
        self.drain_image_hydration_requests(editor, cx);
        let composition_was_active = self.save.is_composing();
        self.save.resolve_composition();
        let next_title = title.read(cx).text().to_owned();
        let next_document = editor.read(cx).document().semantic_snapshot();
        if next_title == self.observed_title && next_document == self.observed_document {
            // A canceled candidate can restore exactly the old semantic
            // document after all of its original deadline callbacks have
            // already fired while composition was frozen. Resume the retained
            // dirty generation immediately; otherwise it would remain dirty
            // forever until an unrelated user edit.
            if composition_was_active {
                self.drive_pending_flush_or_due_work(cx);
            }
            if composition_was_active {
                cx.notify();
            }
            return;
        }
        let next_snapshot = self.capture_committed_snapshot(next_title.clone(), editor, cx);
        self.observed_title = next_title;
        self.observed_document = next_document;
        let starts_new_dirty_window =
            matches!(self.save.state(), SaveState::Clean | SaveState::Failed(_));
        self.committed_snapshot = next_snapshot;
        let generation = self.save.mark_dirty();
        if let Some(barrier) = self.pending_flush.as_mut() {
            // A boundary remains a boundary for the latest committed input;
            // it must not declare the pre-edit generation durable while the
            // user is still typing in the retained session.
            barrier.generation = generation;
            barrier.expected_revision = self.expected_revision;
        }
        self.arm_deadlines(starts_new_dirty_window, cx);
        cx.notify();
    }

    fn observe_current_entities(&mut self, cx: &mut Context<Self>) {
        let title = self.title.clone();
        let editor = self.editor.clone();
        self.observe_entities(&title, &editor, cx);
    }

    fn capture_committed_snapshot(
        &self,
        title: String,
        editor: &Entity<EditorCore>,
        cx: &mut Context<Self>,
    ) -> NativeSessionSnapshot {
        NativeSessionSnapshot {
            title,
            document: editor.read(cx).document().clone(),
            allowed_resource_ids: self.resource_ids.clone(),
        }
    }

    fn snapshot(&self) -> NativeSessionSnapshot {
        self.committed_snapshot.clone()
    }

    fn save_job(&mut self, work: SaveWork) -> SaveJob {
        SaveJob {
            work,
            note_id: self.note_id.clone(),
            expected_revision: self.expected_revision,
            writer_token: self.writer_token.clone(),
            journal_ownership: self.journal_ownership.clone(),
            journal_base: self.journal_base.clone(),
            snapshot: self.snapshot(),
            repository: Arc::clone(&self.repository),
            #[cfg(test)]
            gate: self.next_background_save_gate.take(),
        }
    }

    fn encode_snapshot(
        snapshot: NativeSessionSnapshot,
        stage: &str,
    ) -> Result<SessionSnapshot, SaveError> {
        let document = export_canonical_with_resources(
            &snapshot.document,
            Some(&snapshot.allowed_resource_ids),
        )
        .map_err(|error| SaveError::new(format!("无法{stage}: {error}")))?;
        let resource_ids = document.resource_ids();
        Ok(SessionSnapshot {
            title: snapshot.title,
            document,
            resource_ids,
        })
    }

    /// This function intentionally has no `Context` or entity handle. It is
    /// run on GPUI's background executor in production, proving codec and
    /// SQLite cannot reread live editor state or stall a drawing callback.
    fn perform_save(job: SaveJob) -> Result<SaveCompletion, SaveError> {
        let generation = match job.work {
            SaveWork::Journal { generation } | SaveWork::Snapshot { generation } => generation,
        };
        match job.work {
            SaveWork::Journal { .. } => {
                let snapshot = Self::encode_snapshot(job.snapshot, "写入编辑日志")?;
                let payload = JournalPayload::from_snapshots(
                    &job.note_id,
                    job.expected_revision,
                    job.writer_token.clone(),
                    generation,
                    &job.journal_base,
                    &snapshot,
                );
                // Compact JSON remains inspectable after a crash, without a
                // second pretty-printed HTML body for every keystroke.
                let delta_utf8 = serde_json::to_string(&payload)
                    .map_err(|error| SaveError::new(format!("无法编码编辑日志: {error}")))?;
                let ownership = job.repository.append_edit_journal(EditJournalEntry {
                    note_id: job.note_id,
                    expected_revision: job.expected_revision,
                    writer_token: job.writer_token,
                    sequence: 0,
                    generation,
                    delta_utf8,
                })?;
                Ok(SaveCompletion::Journal { ownership })
            }
            SaveWork::Snapshot { .. } => {
                let snapshot = Self::encode_snapshot(job.snapshot, "保存快照")?;
                let note = job.repository.flush_snapshot_note(
                    SaveNote {
                        id: job.note_id,
                        expected_revision: job.expected_revision,
                        title: snapshot.title.clone(),
                        document: snapshot.document.clone(),
                        resource_ids: snapshot.resource_ids.clone(),
                        selected_thumbnail_id: None,
                    },
                    job.journal_ownership,
                )?;
                Ok(SaveCompletion::Snapshot {
                    note,
                    snapshot,
                    expected_revision: job.expected_revision,
                })
            }
        }
    }

    async fn perform_save_on_worker(job: SaveJob) -> Result<SaveCompletion, SaveError> {
        #[cfg(test)]
        if let Some(gate) = job.gate.as_ref() {
            gate.wait().await;
        }
        Self::perform_save(job)
    }

    fn finish_save(
        &mut self,
        work: SaveWork,
        result: Result<SaveCompletion, SaveError>,
        cx: &mut Context<Self>,
    ) -> Result<Option<SavedRevision>, SaveError> {
        let generation = match work {
            SaveWork::Journal { generation } | SaveWork::Snapshot { generation } => generation,
        };
        let publishes_current_generation = self.save.is_current_generation(generation);
        let outcome = match result {
            Ok(SaveCompletion::Journal { ownership }) => {
                self.journal_ownership = Some(ownership);
                self.save.journaled(generation);
                Ok(None)
            }
            Ok(SaveCompletion::Snapshot {
                note,
                snapshot,
                expected_revision,
            }) => {
                let saved = SavedRevision {
                    revision: note.revision,
                    saved_time: note.updated_time,
                };
                let completes_pending_flush = self.pending_flush.is_some_and(|barrier| {
                    publishes_current_generation
                        && barrier.generation == generation
                        && barrier.expected_revision == expected_revision
                        && saved.revision == expected_revision.saturating_add(1)
                });
                self.expected_revision = saved.revision;
                self.last_saved = saved.clone();
                self.saved_note_outcome = Some(note);
                self.journal_base = snapshot;
                self.journal_ownership = None;
                self.save.snapshotted(generation);
                // An old snapshot can advance the durable base while a newer
                // input generation is already dirty. It must not cancel that
                // newer generation's retained 100/500/15s deadline tasks.
                if publishes_current_generation {
                    self._journal_deadline_task = None;
                    self._settled_deadline_task = None;
                    self._hard_deadline_task = None;
                }
                if completes_pending_flush {
                    self.pending_flush = None;
                } else if let Some(barrier) = self.pending_flush.as_mut() {
                    // A prior-generation snapshot can legitimately commit
                    // while later input is already dirty. Its new revision is
                    // the required base for the still-blocked lifecycle
                    // barrier; do not accidentally accept that earlier save.
                    barrier.generation = self.save.generation();
                    barrier.expected_revision = self.expected_revision;
                }
                // A failed staged resource may have been explicitly removed
                // before Manual Sync. `retry_failed_resource_commit` then
                // hands its lifecycle request to `pending_flush`; clear the
                // resource token only when that exact ordinary snapshot won.
                // A concurrent stage/commit still owns the token and must
                // keep lifecycle actions blocked even if unrelated text work
                // happened to finish first.
                if completes_pending_flush
                    && self.pending_resource_lifecycle_flush
                    && !self.resource_lifecycle_operation_pending()
                {
                    self.pending_resource_lifecycle_flush = false;
                }
                Ok(Some(saved))
            }
            Err(error) => {
                if publishes_current_generation {
                    // Every error path, including codec and repository
                    // failures, reaches this one terminal state. No timer
                    // retries Failed.
                    self.save.fail(error.to_string());
                    Err(error)
                } else {
                    // A worker never gets to poison a later, independently
                    // captured input generation. That later generation may
                    // have removed the unsupported construct or otherwise
                    // be saveable; its own result is the only one allowed to
                    // publish `Failed` or `Clean`.
                    Ok(None)
                }
            }
        };
        // The same retained/background dispatch is used in tests and
        // production. A boundary queued during a journal starts its exact
        // snapshot only after that worker completion has been observed.
        if outcome.is_ok() {
            // A staged picker/drop source owns a tracked Selection and must
            // get the first chance after the exact journal/snapshot fence
            // lifts. Starting another timer save first could unnecessarily
            // remap it again or race the journal ownership it was waiting on.
            self.drive_pending_staged_resource(cx);
            self.drive_pending_flush_or_due_work(cx);
        }
        cx.notify();
        outcome
    }

    fn start_background_work(&mut self, work: SaveWork, cx: &mut Context<Self>) {
        // A resource worker owns the exact expected revision for its atomic
        // note/resource snapshot. Later typing stays live and dirty, but its
        // regular journal/snapshot must wait until that revision advances;
        // otherwise two workers could race with the same SQLite base.
        if self._save_task.is_some()
            || self._resource_commit_task.is_some()
            || !self.save.begin(work)
        {
            return;
        }
        let job = self.save_job(work);
        let task = cx
            .background_executor()
            .spawn(async move { Self::perform_save_on_worker(job).await });
        self._save_task = Some(cx.spawn(async move |this, cx| {
            let result = task.await;
            let _ = this.update(cx, |session, session_cx| {
                session._save_task = None;
                let _ = session.finish_save(work, result, session_cx);
            });
        }));
    }

    fn drive_due_work(&mut self, cx: &mut Context<Self>) {
        if let Some(work) = self.save.due_work() {
            self.start_background_work(work, cx);
        }
    }

    fn drive_pending_flush_or_due_work(&mut self, cx: &mut Context<Self>) {
        if self._save_task.is_some()
            || self._resource_commit_task.is_some()
            || self.pending_failed_resource_commit.is_some()
        {
            return;
        }
        if self.pending_flush.is_some() {
            if !matches!(self.save.state(), SaveState::Dirty) {
                return;
            }
            if let Some(barrier) = self.pending_flush.as_mut() {
                barrier.generation = self.save.generation();
                barrier.expected_revision = self.expected_revision;
            }
            if let Some(work) = self.save.force_snapshot() {
                self.start_background_work(work, cx);
            }
            return;
        }
        self.drive_due_work(cx);
    }

    /// Exercises the exact production background dispatch path under the
    /// deterministic clock, including lifecycle barriers. It never calls a
    /// synchronous save shortcut or rereads an entity from a worker.
    #[cfg(test)]
    pub(crate) fn dispatch_due_background_work_for_test(&mut self, cx: &mut Context<Self>) {
        self.observe_current_entities(cx);
        self.drive_pending_flush_or_due_work(cx);
    }

    #[cfg(test)]
    pub(crate) fn enable_deadline_tasks_for_test(&mut self) {
        self.deadline_tasks_enabled_for_test = true;
    }

    #[cfg(test)]
    pub(crate) fn stall_next_background_save_for_test(&mut self) -> oneshot::Sender<()> {
        let (sender, receiver) = oneshot::channel();
        self.next_background_save_gate = Some(Arc::new(BackgroundSaveGate {
            release: Mutex::new(Some(receiver)),
        }));
        sender
    }

    #[cfg(test)]
    pub(crate) fn stall_next_resource_stage_for_test(&mut self) -> oneshot::Sender<()> {
        let (sender, receiver) = oneshot::channel();
        self.next_resource_stage_gate = Some(Arc::new(BackgroundResourceGate {
            release: Mutex::new(Some(receiver)),
        }));
        sender
    }

    #[cfg(test)]
    pub(crate) fn stall_next_resource_commit_for_test(&mut self) -> oneshot::Sender<()> {
        let (sender, receiver) = oneshot::channel();
        self.next_resource_commit_gate = Some(Arc::new(BackgroundResourceGate {
            release: Mutex::new(Some(receiver)),
        }));
        sender
    }

    /// Stops only the production persisted-image materializer. This lets the
    /// destruction test prove a weak completion cannot recreate a dropped
    /// ImageStore root; it is not a synchronous hydration shortcut.
    #[cfg(test)]
    pub(crate) fn stall_next_image_hydration_for_test(&mut self) -> oneshot::Sender<()> {
        let (sender, receiver) = oneshot::channel();
        self.next_image_hydration_gate = Some(Arc::new(BackgroundResourceGate {
            release: Mutex::new(Some(receiver)),
        }));
        sender
    }

    /// Pauses the real retained attachment worker after it has streamed the
    /// verified descriptor into its worker-owned lease, but before an opener
    /// observes that path. This is intentionally not a synchronous test path:
    /// it proves a note switch cannot race the platform handoff.
    #[cfg(test)]
    pub(crate) fn stall_next_attachment_open_for_test(
        &mut self,
    ) -> (oneshot::Sender<()>, std::sync::mpsc::Receiver<PathBuf>) {
        let (release_sender, release_receiver) = oneshot::channel();
        let (materialized_sender, materialized_receiver) = std::sync::mpsc::channel();
        self.next_attachment_open_gate = Some(Arc::new(BackgroundAttachmentOpenGate {
            release: Mutex::new(Some(release_receiver)),
            materialized: materialized_sender,
        }));
        (release_sender, materialized_receiver)
    }

    /// Fault only the retained resource worker after the optimistic editor
    /// transaction has become visible. This is intentionally not a direct
    /// `finish_resource_commit` call: tests exercise the same background
    /// handoff and recovery ordering as production.
    #[cfg(test)]
    pub(crate) fn fail_next_resource_commit_for_test(&mut self, message: impl Into<String>) {
        self.next_resource_commit_failure = Some(SaveError::new(message));
    }

    fn arm_deadlines(&mut self, start_hard_deadline: bool, cx: &mut Context<Self>) {
        #[cfg(test)]
        if !self.deadline_tasks_enabled_for_test {
            return;
        }
        if self._journal_deadline_task.is_none() && self.save.needs_journal_deadline() {
            self._journal_deadline_task = Some(cx.spawn(async move |this, cx| {
                cx.background_executor()
                    .timer(Duration::from_millis(100))
                    .await;
                let _ = this.update(cx, |session, session_cx| {
                    session._journal_deadline_task = None;
                    session.drive_pending_flush_or_due_work(session_cx);
                });
            }));
        }
        // The settled snapshot is an idle debounce and therefore is allowed
        // to move with every committed keystroke. Only the journal deadline
        // above is a non-resettable durability promise.
        self._settled_deadline_task = Some(cx.spawn(async move |this, cx| {
            cx.background_executor()
                .timer(Duration::from_millis(500))
                .await;
            let _ = this.update(cx, |session, session_cx| {
                session._settled_deadline_task = None;
                session.drive_pending_flush_or_due_work(session_cx);
            });
        }));
        if start_hard_deadline && self._hard_deadline_task.is_none() {
            self._hard_deadline_task = Some(cx.spawn(async move |this, cx| {
                cx.background_executor()
                    .timer(Duration::from_secs(15))
                    .await;
                let _ = this.update(cx, |session, session_cx| {
                    session._hard_deadline_task = None;
                    session.drive_pending_flush_or_due_work(session_cx);
                });
            }));
        }
    }

    pub(crate) fn poll(&mut self, cx: &mut Context<Self>) -> Result<(), SaveError> {
        self.observe_current_entities(cx);
        self.drive_pending_flush_or_due_work(cx);
        Ok(())
    }

    pub(crate) fn flush(
        &mut self,
        reason: FlushReason,
        cx: &mut Context<Self>,
    ) -> Result<SavedRevision, SaveError> {
        // A marked range is a platform-owned IME candidate, not a durable
        // edit yet. Persisting it would save text the user may still replace;
        // dropping it on a close/switch is worse. Make the lifecycle caller
        // keep the window/session alive and show its existing save error.
        if self.title.read(cx).marked_range().is_some()
            || self.editor.read(cx).marked_text().is_some()
        {
            return Err(SaveError::new(format!(
                "{reason:?} 前仍有未确认的输入法组合文本，请先确认或取消输入"
            )));
        }
        self.observe_current_entities(cx);
        if self.pending_failed_resource_commit.is_some() {
            self.pending_resource_lifecycle_flush = true;
            match self.retry_failed_resource_commit(cx) {
                Ok(true) => {
                    return Err(SaveError::new(format!(
                        "{reason:?} 正在重试暂存资源；完成前请保持当前窗口打开"
                    )));
                }
                Ok(false) => {
                    // The staged atom was removed by the user. Fall through
                    // to force a normal snapshot of the remaining text.
                }
                Err(error) => return Err(error),
            }
        }
        // A stage may be copying/hash-validating a descriptor while the text
        // coordinator remains Clean. It still owns a captured insertion
        // intent, so do not report `last_saved` to a switch/delete/close and
        // let the retained task be destroyed. This token is intentionally not
        // part of `resource_insert_is_fenced`: stage completion must be able
        // to begin its own atomic commit after an unrelated text writer
        // releases its fence.
        if self.resource_lifecycle_operation_pending() {
            self.pending_resource_lifecycle_flush = true;
            self.drive_pending_staged_resource(cx);
            return Err(SaveError::new(format!(
                "{reason:?} 前资源正在准备或提交；完成前请保持当前窗口打开"
            )));
        }
        match self.save.state() {
            SaveState::Clean if self.pending_flush.is_none() => Ok(self.last_saved.clone()),
            SaveState::Failed(error) => {
                Err(SaveError::new(format!("{reason:?} 前无法保存: {error}")))
            }
            SaveState::Clean => Err(SaveError::new(format!("{reason:?} 的保存确认仍未完成"))),
            SaveState::Dirty | SaveState::Journaling | SaveState::Snapshotting => {
                self.pending_flush = Some(FlushBarrier {
                    generation: self.save.generation(),
                    expected_revision: self.expected_revision,
                });
                self.drive_pending_flush_or_due_work(cx);
                Err(SaveError::new(format!(
                    "{reason:?} 正在后台保存；完成前请保持当前窗口打开"
                )))
            }
        }
    }
}
