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
use crate::native_editor::core::EditorCore;
use app_lite_core::{
    CanonicalDocument, EditJournalEntry, LibraryError, LibraryRepository, Note, NoteId, ResourceId,
    SaveNote, SavedRevision,
};
use gpui::{AppContext, Context, Entity, Subscription, Task};
use serde::{Deserialize, Serialize};
use std::fmt;
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
    resource_ids: Vec<ResourceId>,
}

enum SaveCompletion {
    Journal,
    Snapshot {
        saved: SavedRevision,
        snapshot: SessionSnapshot,
    },
}

#[derive(Clone)]
struct SaveJob {
    work: SaveWork,
    note_id: NoteId,
    expected_revision: i64,
    writer_token: String,
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

#[derive(Debug, Deserialize)]
struct LegacyJournalPayload {
    version: u8,
    note_id: String,
    expected_revision: i64,
    generation: i64,
    title: String,
    body_html: String,
    resource_ids: Vec<String>,
}

pub(crate) struct PreparedNoteSession {
    note: Note,
    snapshot: SessionSnapshot,
    journal_base: SessionSnapshot,
    native_document: crate::native_editor::model::Document,
    recovered_generation: Option<i64>,
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
        if resource_ids != note.resource_ids {
            return Err(SaveError::new("编辑日志引用的资源与当前笔记不一致"));
        }
        let title = self.title.apply(&note.title, "标题")?;
        let body_html = self.body.apply(&note.body_html, "正文")?;
        let document = CanonicalDocument::parse_html(&body_html)
            .map_err(|error| SaveError::new(format!("无法恢复编辑日志正文: {error}")))?;
        if document.resource_ids() != resource_ids {
            return Err(SaveError::new("编辑日志正文的资源关系不完整"));
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

impl LegacyJournalPayload {
    fn into_snapshot(self, note: &Note) -> Result<(i64, SessionSnapshot), SaveError> {
        if self.version != 1
            || self.note_id != note.id.as_str()
            || self.expected_revision != note.revision
        {
            return Err(SaveError::new("编辑日志与当前笔记版本不匹配"));
        }
        let resource_ids = self
            .resource_ids
            .into_iter()
            .map(|raw| ResourceId::new(raw).map_err(|_| SaveError::new("编辑日志包含无效资源 ID")))
            .collect::<Result<Vec<_>, _>>()?;
        if resource_ids != note.resource_ids {
            return Err(SaveError::new("编辑日志引用的资源与当前笔记不一致"));
        }
        let document = CanonicalDocument::parse_html(&self.body_html)
            .map_err(|error| SaveError::new(format!("无法恢复编辑日志正文: {error}")))?;
        if document.resource_ids() != resource_ids {
            return Err(SaveError::new("编辑日志正文的资源关系不完整"));
        }
        Ok((
            self.generation.max(1),
            SessionSnapshot {
                title: self.title,
                document,
                resource_ids,
            },
        ))
    }
}

/// One active library note. The entity subscriptions observe real title/body
/// input notifications, while a semantic snapshot comparison filters focus,
/// selection and paint notifications that must never create false saves.
pub(crate) struct NoteSession {
    note_id: NoteId,
    expected_revision: i64,
    writer_token: String,
    resource_ids: Vec<ResourceId>,
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
    #[cfg(test)]
    deadline_tasks_enabled_for_test: bool,
    #[cfg(test)]
    next_background_save_gate: Option<Arc<BackgroundSaveGate>>,
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
        if let Some(entry) =
            repository.latest_edit_journal_for_revision(&note.id, Some(note.revision))?
        {
            let version = serde_json::from_str::<serde_json::Value>(&entry.delta_utf8)
                .ok()
                .and_then(|value| value.get("version").and_then(serde_json::Value::as_u64));
            let (generation, recovered) = match version {
                Some(1) => serde_json::from_str::<LegacyJournalPayload>(&entry.delta_utf8)
                    .map_err(|error| SaveError::new(format!("无法读取旧编辑日志: {error}")))?
                    .into_snapshot(&note)?,
                Some(version) if version == JOURNAL_SCHEMA_VERSION as u64 => {
                    serde_json::from_str::<JournalPayload>(&entry.delta_utf8)
                        .map_err(|error| SaveError::new(format!("无法读取编辑日志: {error}")))?
                        .into_snapshot(&note, &entry)?
                }
                _ => return Err(SaveError::new("编辑日志版本不受支持")),
            };
            snapshot = recovered;
            recovered_generation = Some(generation.max(entry.generation));
        }
        let native_document =
            import_canonical_with_resources(&snapshot.document, &snapshot.resource_ids)
                .map_err(|error| SaveError::new(format!("无法转换正文: {error}")))?;
        Ok(PreparedNoteSession {
            note,
            snapshot,
            journal_base,
            native_document,
            recovered_generation,
        })
    }

    pub(crate) fn from_prepared(
        prepared: PreparedNoteSession,
        repository: Arc<LibraryRepository>,
        clock: Arc<dyn SaveClock>,
        cx: &mut Context<Self>,
    ) -> Self {
        let note = prepared.note;
        let snapshot = prepared.snapshot;
        let journal_base = prepared.journal_base;
        let document = prepared.native_document;
        let committed_snapshot = NativeSessionSnapshot {
            title: snapshot.title.clone(),
            document: document.clone(),
            resource_ids: snapshot.resource_ids.clone(),
        };
        let title = cx.new(|cx| TitleInput::new(snapshot.title.clone(), cx));
        let editor = cx.new(|cx| EditorCore::new(document, cx));
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
        if let Some(generation) = prepared.recovered_generation {
            save.restore_journaled(generation);
        }
        Self {
            note_id: note.id,
            expected_revision: note.revision,
            writer_token: uuid::Uuid::new_v4().simple().to_string(),
            resource_ids: snapshot.resource_ids,
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
            #[cfg(test)]
            deadline_tasks_enabled_for_test: false,
            #[cfg(test)]
            next_background_save_gate: None,
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
        Ok(Self::from_prepared(prepared, repository, clock, cx))
    }

    pub(crate) fn title(&self) -> &Entity<TitleInput> {
        &self.title
    }

    pub(crate) fn editor(&self) -> &Entity<EditorCore> {
        &self.editor
    }

    pub(crate) fn save_state(&self) -> SaveState {
        self.save.state()
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
            #[cfg(not(test))]
            if composition_was_active {
                self.drive_due_work(cx);
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
        self.save.mark_dirty();
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
            resource_ids: self.resource_ids.clone(),
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
        let document =
            export_canonical_with_resources(&snapshot.document, Some(&snapshot.resource_ids))
                .map_err(|error| SaveError::new(format!("无法{stage}: {error}")))?;
        let resource_ids = document.resource_ids();
        if resource_ids != snapshot.resource_ids {
            return Err(SaveError::new(format!(
                "无法{stage}: 正文资源关系与笔记不一致"
            )));
        }
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
                job.repository.append_edit_journal(EditJournalEntry {
                    note_id: job.note_id,
                    expected_revision: job.expected_revision,
                    writer_token: job.writer_token,
                    sequence: 0,
                    generation,
                    delta_utf8,
                })?;
                Ok(SaveCompletion::Journal)
            }
            SaveWork::Snapshot { .. } => {
                let snapshot = Self::encode_snapshot(job.snapshot, "保存快照")?;
                let saved = job.repository.flush_snapshot(SaveNote {
                    id: job.note_id,
                    expected_revision: job.expected_revision,
                    title: snapshot.title.clone(),
                    document: snapshot.document.clone(),
                    resource_ids: snapshot.resource_ids.clone(),
                    selected_thumbnail_id: None,
                })?;
                Ok(SaveCompletion::Snapshot { saved, snapshot })
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
            Ok(SaveCompletion::Journal) => {
                self.save.journaled(generation);
                Ok(None)
            }
            Ok(SaveCompletion::Snapshot { saved, snapshot }) => {
                self.expected_revision = saved.revision;
                self.last_saved = saved.clone();
                self.journal_base = snapshot;
                self.save.snapshotted(generation);
                // An old snapshot can advance the durable base while a newer
                // input generation is already dirty. It must not cancel that
                // newer generation's retained 100/500/15s deadline tasks.
                if publishes_current_generation {
                    self._journal_deadline_task = None;
                    self._settled_deadline_task = None;
                    self._hard_deadline_task = None;
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
        #[cfg(not(test))]
        if outcome.is_ok() {
            self.drive_due_work(cx);
        }
        cx.notify();
        outcome
    }

    #[cfg(test)]
    fn execute(
        &mut self,
        work: SaveWork,
        cx: &mut Context<Self>,
    ) -> Result<Option<SavedRevision>, SaveError> {
        if !self.save.begin(work) {
            return Ok(None);
        }
        let job = self.save_job(work);
        let result = Self::perform_save(job);
        self.finish_save(work, result, cx)
    }

    fn start_background_work(&mut self, work: SaveWork, cx: &mut Context<Self>) {
        if self._save_task.is_some() || !self.save.begin(work) {
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

    /// Exercises the exact production background dispatch path under the
    /// deterministic clock. The synchronous `poll` seam remains useful for
    /// deadline arithmetic, while this proves the immutable job handoff does
    /// not read an entity after it has been queued to a worker.
    #[cfg(test)]
    pub(crate) fn dispatch_due_background_work_for_test(&mut self, cx: &mut Context<Self>) {
        self.observe_current_entities(cx);
        self.drive_due_work(cx);
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

    fn arm_deadlines(&mut self, start_hard_deadline: bool, cx: &mut Context<Self>) {
        #[cfg(test)]
        if !self.deadline_tasks_enabled_for_test {
            return;
        }
        self._journal_deadline_task = Some(cx.spawn(async move |this, cx| {
            cx.background_executor()
                .timer(Duration::from_millis(100))
                .await;
            let _ = this.update(cx, |session, session_cx| session.drive_due_work(session_cx));
        }));
        self._settled_deadline_task = Some(cx.spawn(async move |this, cx| {
            cx.background_executor()
                .timer(Duration::from_millis(500))
                .await;
            let _ = this.update(cx, |session, session_cx| session.drive_due_work(session_cx));
        }));
        if start_hard_deadline {
            self._hard_deadline_task = Some(cx.spawn(async move |this, cx| {
                cx.background_executor()
                    .timer(Duration::from_secs(15))
                    .await;
                let _ = this.update(cx, |session, session_cx| session.drive_due_work(session_cx));
            }));
        }
    }

    pub(crate) fn poll(&mut self, cx: &mut Context<Self>) -> Result<(), SaveError> {
        self.observe_current_entities(cx);
        #[cfg(test)]
        {
            // Manual-clock tests drive exact deadline boundaries without wall
            // sleeps; production always uses the retained deadline tasks.
            for _ in 0..2 {
                let Some(work) = self.save.due_work() else {
                    break;
                };
                self.execute(work, cx)?;
            }
        }
        #[cfg(not(test))]
        self.drive_due_work(cx);
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
        if let SaveState::Failed(error) = self.save.state() {
            return Err(SaveError::new(format!("{reason:?} 前无法保存: {error}")));
        }
        let Some(work) = self.save.force_snapshot() else {
            return Ok(self.last_saved.clone());
        };
        #[cfg(test)]
        {
            self.execute(work, cx)?;
            Ok(self.last_saved.clone())
        }
        #[cfg(not(test))]
        {
            self.start_background_work(work, cx);
            Err(SaveError::new(format!(
                "{reason:?} 正在后台保存；完成前请保持当前窗口打开"
            )))
        }
    }
}
