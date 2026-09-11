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
use gpui::{AppContext, Context, Entity, Subscription};
use serde::{Deserialize, Serialize};
use std::fmt;
use std::sync::Arc;

const JOURNAL_SCHEMA_VERSION: u8 = 1;

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

#[derive(Debug, Serialize, Deserialize)]
struct JournalPayload {
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
    native_document: crate::native_editor::model::Document,
    recovered_generation: Option<i64>,
}

impl JournalPayload {
    fn from_snapshot(
        note_id: &NoteId,
        expected_revision: i64,
        generation: i64,
        snapshot: &SessionSnapshot,
    ) -> Self {
        Self {
            version: JOURNAL_SCHEMA_VERSION,
            note_id: note_id.as_str().to_owned(),
            expected_revision,
            generation,
            title: snapshot.title.clone(),
            body_html: snapshot.document.to_canonical_html().as_str().to_owned(),
            resource_ids: snapshot
                .resource_ids
                .iter()
                .map(|id| id.as_str().to_owned())
                .collect(),
        }
    }

    fn into_snapshot(self, note: &Note) -> Result<(i64, SessionSnapshot), SaveError> {
        if self.version != JOURNAL_SCHEMA_VERSION
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
    resource_ids: Vec<ResourceId>,
    title: Entity<TitleInput>,
    editor: Entity<EditorCore>,
    repository: Arc<LibraryRepository>,
    save: SaveCoordinator,
    last_saved: SavedRevision,
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
        let mut recovered_generation = None;
        if let Some(entry) = repository.latest_edit_journal(&note.id)? {
            let payload = serde_json::from_str::<JournalPayload>(&entry.delta_utf8)
                .map_err(|error| SaveError::new(format!("无法读取编辑日志: {error}")))?;
            let (generation, recovered) = payload.into_snapshot(&note)?;
            snapshot = recovered;
            recovered_generation = Some(generation.max(entry.generation));
        }
        let native_document =
            import_canonical_with_resources(&snapshot.document, &snapshot.resource_ids)
                .map_err(|error| SaveError::new(format!("无法转换正文: {error}")))?;
        Ok(PreparedNoteSession {
            note,
            snapshot,
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
        let document = prepared.native_document;
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
            resource_ids: snapshot.resource_ids,
            title,
            editor,
            repository,
            save,
            last_saved: SavedRevision {
                revision: note.revision,
                saved_time: note.updated_time,
            },
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
            return;
        }
        let next_title = title.read(cx).text().to_owned();
        let next_document = editor.read(cx).document().semantic_snapshot();
        if next_title == self.observed_title && next_document == self.observed_document {
            return;
        }
        self.observed_title = next_title;
        self.observed_document = next_document;
        self.save.mark_dirty();
        cx.notify();
    }

    fn observe_current_entities(&mut self, cx: &mut Context<Self>) {
        let title = self.title.clone();
        let editor = self.editor.clone();
        self.observe_entities(&title, &editor, cx);
    }

    fn snapshot(
        &self,
        generation: i64,
        journal: bool,
        cx: &mut Context<Self>,
    ) -> Result<SessionSnapshot, SaveError> {
        let document = export_canonical_with_resources(
            self.editor.read(cx).document(),
            Some(&self.resource_ids),
        )
        .map_err(|error| {
            let stage = if journal {
                "写入编辑日志"
            } else {
                "保存快照"
            };
            SaveError::new(format!("无法{stage}: {error}"))
        })?;
        let resource_ids = document.resource_ids();
        if resource_ids != self.resource_ids {
            return Err(SaveError::new(format!(
                "无法{}: 正文资源关系与笔记不一致（generation {generation}）",
                if journal {
                    "写入编辑日志"
                } else {
                    "保存快照"
                }
            )));
        }
        Ok(SessionSnapshot {
            title: self.title.read(cx).text().to_owned(),
            document,
            resource_ids,
        })
    }

    fn execute(
        &mut self,
        work: SaveWork,
        cx: &mut Context<Self>,
    ) -> Result<Option<SavedRevision>, SaveError> {
        if !self.save.begin(work) {
            return Ok(None);
        }
        let generation = match work {
            SaveWork::Journal { generation } | SaveWork::Snapshot { generation } => generation,
        };
        let result: Result<Option<SavedRevision>, SaveError> = match work {
            SaveWork::Journal { .. } => {
                let snapshot = self.snapshot(generation, true, cx)?;
                let payload = JournalPayload::from_snapshot(
                    &self.note_id,
                    self.expected_revision,
                    generation,
                    &snapshot,
                );
                let delta_utf8 = serde_json::to_string_pretty(&payload)
                    .map_err(|error| SaveError::new(format!("无法编码编辑日志: {error}")))?;
                self.repository.append_edit_journal(EditJournalEntry {
                    note_id: self.note_id.clone(),
                    generation,
                    delta_utf8,
                })?;
                self.save.journaled(generation);
                Ok(None)
            }
            SaveWork::Snapshot { .. } => {
                let snapshot = self.snapshot(generation, false, cx)?;
                let saved = self.repository.flush_snapshot(SaveNote {
                    id: self.note_id.clone(),
                    expected_revision: self.expected_revision,
                    title: snapshot.title,
                    document: snapshot.document,
                    resource_ids: snapshot.resource_ids,
                    selected_thumbnail_id: None,
                })?;
                self.expected_revision = saved.revision;
                self.last_saved = saved.clone();
                self.save.snapshotted(generation);
                Ok(Some(saved))
            }
        };
        if let Err(error) = &result {
            self.save.fail(error.to_string());
        }
        result
    }

    pub(crate) fn poll(&mut self, cx: &mut Context<Self>) -> Result<(), SaveError> {
        self.observe_current_entities(cx);
        // At most one journal and the following snapshot can become due in
        // one turn. A bounded loop prevents a bad clock from monopolizing UI.
        for _ in 0..2 {
            let Some(work) = self.save.due_work() else {
                break;
            };
            self.execute(work, cx)?;
        }
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
        self.execute(work, cx)?;
        Ok(self.last_saved.clone())
    }
}
