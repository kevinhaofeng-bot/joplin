mod actions;
mod navigation;
pub(crate) mod note_session;
pub(crate) mod save_coordinator;

pub use actions::*;
pub use navigation::*;

use app_lite_core::{
    CanonicalDocument, CreateNote as RepositoryCreateNote, LibraryError, LibraryEvent,
    LibraryRepository, LibraryShellState, ListQuery, Note, NoteId, NoteProjection,
};
use std::sync::Arc;
use std::sync::mpsc::Receiver;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PaneState {
    pub sidebar_width: u16,
    pub list_width: u16,
    pub sidebar_visible: bool,
    pub list_visible: bool,
}

impl PaneState {
    pub fn new(sidebar_width: u16, list_width: u16) -> Self {
        Self {
            sidebar_width,
            list_width,
            sidebar_visible: true,
            list_visible: true,
        }
        .normalized()
    }

    fn from_shell_state(state: &LibraryShellState) -> Self {
        let mut panes = Self::new(state.sidebar_width, state.list_width);
        panes.sidebar_visible = state.sidebar_visible;
        panes.list_visible = state.list_visible;
        panes
    }

    fn normalized(self) -> Self {
        if LibraryShellState::pane_width_is_valid(self.sidebar_width)
            && LibraryShellState::pane_width_is_valid(self.list_width)
        {
            self
        } else {
            Self {
                sidebar_width: LibraryShellState::DEFAULT_SIDEBAR_WIDTH,
                list_width: LibraryShellState::DEFAULT_LIST_WIDTH,
                sidebar_visible: self.sidebar_visible,
                list_visible: self.list_visible,
            }
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ActiveSession {
    pub note: Note,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AppStatus {
    Ready,
    Error(String),
}

/// Tracks who owns the currently visible status. A projection event may
/// refresh its own transient error, but must never erase a user action's
/// committed-but-not-yet-recovered warning.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum StatusOrigin {
    Neutral,
    Action,
    ProjectionEvent,
}

pub struct AppModel {
    repository: Arc<LibraryRepository>,
    navigation: NavigationState,
    projections: Vec<NoteProjection>,
    active_session: Option<ActiveSession>,
    panes: PaneState,
    list_view_mode: ListViewMode,
    sort: NoteSort,
    status: AppStatus,
    status_origin: StatusOrigin,
    // A repository mutation can commit before the subsequent projection
    // refresh/selection persistence fails. Keep that fact until an explicit
    // recovery action completes; a later projection event must not falsely
    // imply the user action was rolled back.
    partial_commit_message: Option<String>,
    #[cfg(test)]
    next_refresh_failure: Option<LibraryError>,
    #[cfg(test)]
    projection_event_refreshes: usize,
}

impl AppModel {
    pub fn open(repository: Arc<LibraryRepository>) -> Result<Self, LibraryError> {
        let saved_shell_state = repository.read_library_shell_state()?;
        let panes = PaneState::from_shell_state(&saved_shell_state);
        let projections = repository.list_notes(ListQuery::default())?;
        let mut model = Self {
            repository,
            navigation: NavigationState::default(),
            projections,
            active_session: None,
            panes,
            list_view_mode: ListViewMode::default(),
            sort: NoteSort::default(),
            status: AppStatus::Ready,
            status_origin: StatusOrigin::Neutral,
            partial_commit_message: None,
            #[cfg(test)]
            next_refresh_failure: None,
            #[cfg(test)]
            projection_event_refreshes: 0,
        };
        model.sort_projections();
        if let Some(id) = saved_shell_state.selected_note_id {
            if model
                .projections
                .iter()
                .any(|projection| projection.id == id)
            {
                model.select_note(id)?;
            } else {
                // Do not keep retrying a deleted/stale selection on every
                // launch. The typed atomic write also preserves the panes.
                model.persist_shell_state()?;
            }
        }
        Ok(model)
    }

    pub fn dispatch(&mut self, action: AppAction) -> Result<(), LibraryError> {
        let action_can_recover_partial = matches!(
            action,
            AppAction::CreateNote
                | AppAction::SelectNote(_)
                | AppAction::TrashNote(_)
                | AppAction::TrashSelected
        );
        let result = match action {
            AppAction::CreateNote => self.create_note(),
            AppAction::SelectNote(id) => self.select_note(id),
            AppAction::TrashNote(id) => self.trash_note(id),
            AppAction::TrashSelected => self
                .navigation
                .selected_note_id()
                .cloned()
                .ok_or(LibraryError::NotFound)
                .and_then(|id| self.trash_note(id)),
            AppAction::ToggleSidebar => {
                self.panes.sidebar_visible = !self.panes.sidebar_visible;
                self.persist_shell_state()
            }
            AppAction::ToggleNoteList => {
                self.panes.list_visible = !self.panes.list_visible;
                self.persist_shell_state()
            }
            AppAction::SetListViewMode(mode) => {
                self.list_view_mode = mode;
                Ok(())
            }
            AppAction::SetSort(sort) => {
                self.sort = sort;
                self.sort_projections();
                Ok(())
            }
            // The retained UI session performs the blocking flush before it
            // reaches this shared reducer. Keeping the resulting visible
            // success/error state here means menu, key and button callers all
            // still use one action path.
            AppAction::ManualSync => Ok(()),
        };
        match result {
            Ok(()) => {
                // Only an explicit user action that re-runs the
                // refresh/selection path may resolve a prior committed
                // mutation warning. Cosmetic list actions do not retry it.
                if action_can_recover_partial {
                    self.partial_commit_message = None;
                }
                self.set_action_success_status();
            }
            Err(ref error) => {
                self.set_action_error_status(error);
            }
        }
        result
    }

    fn create_note(&mut self) -> Result<(), LibraryError> {
        // The repository transaction is the source of truth: no temporary UI note exists.
        let note = self.repository.create_note(RepositoryCreateNote {
            title: String::new(),
            notebook_id: None,
            document: CanonicalDocument::default(),
        })?;
        self.navigation.clear_search();
        if let Err(error) = self.refresh_list() {
            self.record_partial_commit("笔记已创建", &error);
            return Err(error);
        }
        if let Err(error) = self.select_loaded_note(note) {
            self.record_partial_commit("笔记已创建，但无法恢复选中状态", &error);
            return Err(error);
        }
        Ok(())
    }

    fn select_note(&mut self, id: NoteId) -> Result<(), LibraryError> {
        if self
            .active_session
            .as_ref()
            .is_some_and(|session| session.note.id == id)
        {
            // Sorting, a repeated card click, or a scroll-to-selected request
            // must not hydrate the already retained full body again.
            self.navigation.select(Some(id));
            return self.persist_shell_state();
        }
        let note = self
            .repository
            .load_note(&id)?
            .ok_or(LibraryError::NotFound)?;
        self.select_loaded_note(note)
    }

    /// Installs a fully hydrated repository result as the active session. The
    /// create path already owns such a result, so routing it through here
    /// avoids immediately loading the same Note a second time.
    fn select_loaded_note(&mut self, note: Note) -> Result<(), LibraryError> {
        self.navigation.select(Some(note.id.clone()));
        self.active_session = Some(ActiveSession { note });
        self.persist_shell_state()
    }

    fn trash_note(&mut self, id: NoteId) -> Result<(), LibraryError> {
        let old_index = self
            .projections
            .iter()
            .position(|projection| projection.id == id);
        let was_selected = self.navigation.selected_note_id() == Some(&id);
        // Preserve the user's visible ordering while the repository refreshes
        // its updated-time sort; selection is still an ID, never an index.
        let nearest_before_refresh = old_index.and_then(|index| {
            self.projections
                .get(index + 1)
                .or_else(|| {
                    index
                        .checked_sub(1)
                        .and_then(|previous| self.projections.get(previous))
                })
                .map(|projection| projection.id.clone())
        });
        self.repository.trash_note(&id)?;
        if let Err(error) = self.refresh_list() {
            self.record_partial_commit("笔记已移至废纸篓", &error);
            return Err(error);
        }
        if was_selected {
            let replacement = nearest_before_refresh.filter(|candidate| {
                self.projections
                    .iter()
                    .any(|projection| projection.id == *candidate)
            });
            if let Some(replacement) = replacement {
                if let Err(error) = self.select_note(replacement) {
                    self.record_partial_commit(
                        "笔记已移至废纸篓，但无法恢复相邻笔记选中状态",
                        &error,
                    );
                    return Err(error);
                }
            } else {
                self.navigation.select(None);
                self.active_session = None;
                if let Err(error) = self.persist_shell_state() {
                    self.record_partial_commit(
                        "笔记已移至废纸篓，但无法清除已保存的选中状态",
                        &error,
                    );
                    return Err(error);
                }
            }
        }
        Ok(())
    }

    pub fn refresh_list(&mut self) -> Result<(), LibraryError> {
        #[cfg(test)]
        if let Some(error) = self.next_refresh_failure.take() {
            return Err(error);
        }
        self.projections = self.repository.list_notes(ListQuery::default())?;
        self.sort_projections();
        if self.navigation.selected_note_id().is_some_and(|id| {
            !self
                .projections
                .iter()
                .any(|projection| &projection.id == id)
        }) {
            self.navigation.select(None);
            self.active_session = None;
            self.persist_shell_state()?;
        }
        Ok(())
    }

    /// Coalesces a bounded batch from the repository event stream into one
    /// projection-only refresh. This deliberately does not load a body: the
    /// selected `Note` remains a session boundary until the user explicitly
    /// selects another card or Task 4 installs save/reload coordination.
    pub fn refresh_projection_events(
        &mut self,
        events: impl IntoIterator<Item = LibraryEvent>,
    ) -> Result<bool, LibraryError> {
        let refresh_needed = events.into_iter().any(|event| {
            matches!(
                event,
                LibraryEvent::NoteCreated(_)
                    | LibraryEvent::NoteProjectionChanged(_)
                    | LibraryEvent::NoteTrashed(_)
                    | LibraryEvent::NoteRestored(_)
                    | LibraryEvent::OrganizationChanged
            )
        });
        if !refresh_needed {
            return Ok(false);
        }
        #[cfg(test)]
        {
            self.projection_event_refreshes += 1;
        }
        let result = self.refresh_list();
        match &result {
            Ok(()) if self.status_origin != StatusOrigin::Action => {
                self.status = AppStatus::Ready;
                self.status_origin = StatusOrigin::Neutral;
            }
            Err(error) if self.status_origin != StatusOrigin::Action => {
                self.status = AppStatus::Error(error.to_string());
                self.status_origin = StatusOrigin::ProjectionEvent;
            }
            Ok(()) | Err(_) => {}
        }
        result.map(|()| true)
    }

    pub fn subscribe_library_events(&self) -> Receiver<LibraryEvent> {
        self.repository.subscribe()
    }

    pub(crate) fn repository(&self) -> Arc<LibraryRepository> {
        Arc::clone(&self.repository)
    }

    pub fn persist_shell_state(&self) -> Result<(), LibraryError> {
        self.repository
            .write_library_shell_state(&LibraryShellState {
                sidebar_width: self.panes.sidebar_width,
                list_width: self.panes.list_width,
                sidebar_visible: self.panes.sidebar_visible,
                list_visible: self.panes.list_visible,
                selected_note_id: self.navigation.selected_note_id().cloned(),
            })
    }
    pub fn navigation(&self) -> &NavigationState {
        &self.navigation
    }
    pub fn projections(&self) -> &[NoteProjection] {
        &self.projections
    }
    pub fn active_session_note_id(&self) -> Option<&NoteId> {
        self.active_session.as_ref().map(|session| &session.note.id)
    }
    pub fn active_note(&self) -> Option<&Note> {
        self.active_session.as_ref().map(|session| &session.note)
    }
    pub fn panes(&self) -> PaneState {
        self.panes
    }
    pub fn status(&self) -> &AppStatus {
        &self.status
    }
    pub fn list_view_mode(&self) -> ListViewMode {
        self.list_view_mode
    }
    pub fn sort(&self) -> NoteSort {
        self.sort
    }
    pub fn set_search_query(&mut self, query: Option<String>) {
        self.navigation.set_search_query(query);
    }
    pub fn set_panes(&mut self, panes: PaneState) {
        self.panes = panes.normalized();
    }
    #[cfg(test)]
    pub fn fail_next_refresh_for_test(&mut self, error: LibraryError) {
        self.next_refresh_failure = Some(error);
    }
    #[cfg(test)]
    pub fn projection_event_refreshes_for_test(&self) -> usize {
        self.projection_event_refreshes
    }
    #[cfg(test)]
    pub fn set_projection_for_test(&mut self, ids: Vec<NoteId>) {
        self.projections = ids
            .into_iter()
            .filter_map(|id| {
                self.projections
                    .iter()
                    .find(|projection| projection.id == id)
                    .cloned()
            })
            .collect();
    }

    fn sort_projections(&mut self) {
        match self.sort {
            NoteSort::UpdatedDescending => self.projections.sort_by(|left, right| {
                right
                    .updated_time
                    .cmp(&left.updated_time)
                    .then_with(|| left.id.cmp(&right.id))
            }),
            NoteSort::TitleAscending => self.projections.sort_by(|left, right| {
                left.title_prefix
                    .cmp(&right.title_prefix)
                    .then_with(|| left.id.cmp(&right.id))
            }),
            NoteSort::TitleDescending => self.projections.sort_by(|left, right| {
                right
                    .title_prefix
                    .cmp(&left.title_prefix)
                    .then_with(|| left.id.cmp(&right.id))
            }),
        }
    }

    fn record_partial_commit(&mut self, committed_action: &str, error: &LibraryError) {
        self.partial_commit_message = Some(format!(
            "{committed_action}，但后续界面同步失败：{error}。资料库数据已提交；请重新打开资料库以恢复显示。"
        ));
    }

    fn set_action_success_status(&mut self) {
        if let Some(message) = &self.partial_commit_message {
            self.status = AppStatus::Error(message.clone());
            self.status_origin = StatusOrigin::Action;
        } else {
            self.status = AppStatus::Ready;
            self.status_origin = StatusOrigin::Neutral;
        }
    }

    fn set_action_error_status(&mut self, error: &LibraryError) {
        self.status = AppStatus::Error(
            self.partial_commit_message
                .clone()
                .unwrap_or_else(|| error.to_string()),
        );
        self.status_origin = StatusOrigin::Action;
    }
}

#[cfg(test)]
mod note_session_tests;
#[cfg(test)]
mod tests;
