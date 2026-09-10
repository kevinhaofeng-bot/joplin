mod actions;
mod navigation;

pub use actions::*;
pub use navigation::*;

use app_lite_core::{
    CanonicalDocument, CreateNote as RepositoryCreateNote, LibraryError, LibraryRepository,
    ListQuery, NoteId, NoteProjection,
};
use std::sync::Arc;

const SELECTED_NOTE_SETTING: &str = "library-shell.selected-note-id";
const PANE_SETTING: &str = "library-shell.panes";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PaneState {
    pub sidebar_width: u16,
    pub list_width: u16,
    pub sidebar_visible: bool,
    pub list_visible: bool,
}

impl PaneState {
    pub const fn new(sidebar_width: u16, list_width: u16) -> Self {
        Self {
            sidebar_width,
            list_width,
            sidebar_visible: true,
            list_visible: true,
        }
    }

    fn from_setting(value: Option<String>) -> Self {
        let Some(value) = value else {
            return Self::new(220, 360);
        };
        let mut pieces = value.split(',');
        let Some(sidebar_width) = pieces.next().and_then(|item| item.parse().ok()) else {
            return Self::new(220, 360);
        };
        let Some(list_width) = pieces.next().and_then(|item| item.parse().ok()) else {
            return Self::new(220, 360);
        };
        let sidebar_visible = pieces.next() == Some("1");
        let list_visible = pieces.next() == Some("1");
        Self {
            sidebar_width,
            list_width,
            sidebar_visible,
            list_visible,
        }
    }

    fn setting_value(self) -> String {
        format!(
            "{},{},{},{}",
            self.sidebar_width,
            self.list_width,
            self.sidebar_visible as u8,
            self.list_visible as u8
        )
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ActiveSessionPlaceholder {
    pub note_id: NoteId,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AppStatus {
    Ready,
    Error(String),
}

pub struct AppModel {
    repository: Arc<LibraryRepository>,
    navigation: NavigationState,
    projections: Vec<NoteProjection>,
    active_session: Option<ActiveSessionPlaceholder>,
    panes: PaneState,
    status: AppStatus,
}

impl AppModel {
    pub fn open(repository: Arc<LibraryRepository>) -> Result<Self, LibraryError> {
        let panes = PaneState::from_setting(repository.read_setting(PANE_SETTING)?);
        let projections = repository.list_notes(ListQuery::default())?;
        let mut model = Self {
            repository,
            navigation: NavigationState::default(),
            projections,
            active_session: None,
            panes,
            status: AppStatus::Ready,
        };
        if let Some(saved) = model.repository.read_setting(SELECTED_NOTE_SETTING)?
            && let Ok(id) = NoteId::parse(saved)
            && model
                .projections
                .iter()
                .any(|projection| projection.id == id)
        {
            model.select_note(id)?;
        }
        Ok(model)
    }

    pub fn dispatch(&mut self, action: AppAction) -> Result<(), LibraryError> {
        let result = match action {
            AppAction::CreateNote => self.create_note(),
            AppAction::SelectNote(id) => self.select_note(id),
            AppAction::TrashNote(id) => self.trash_note(id),
            AppAction::ToggleSidebar => {
                self.panes.sidebar_visible = !self.panes.sidebar_visible;
                self.persist_shell_state()
            }
            AppAction::ToggleNoteList => {
                self.panes.list_visible = !self.panes.list_visible;
                self.persist_shell_state()
            }
        };
        if let Err(error) = &result {
            self.status = AppStatus::Error(error.to_string());
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
        self.refresh_list()?;
        self.select_note(note.id)
    }

    fn select_note(&mut self, id: NoteId) -> Result<(), LibraryError> {
        if self
            .active_session
            .as_ref()
            .is_some_and(|session| session.note_id != id)
        {
            // Task 4 owns durable saves. Until then, a switch is allowed only because this
            // shell never exposes unsaved editing as persisted content.
        }
        if self.repository.load_note(&id)?.is_none() {
            return Err(LibraryError::NotFound);
        }
        self.navigation.select(Some(id.clone()));
        self.active_session = Some(ActiveSessionPlaceholder { note_id: id });
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
        self.refresh_list()?;
        if was_selected {
            let replacement = nearest_before_refresh.filter(|candidate| {
                self.projections
                    .iter()
                    .any(|projection| projection.id == *candidate)
            });
            if let Some(replacement) = replacement {
                self.select_note(replacement)?;
            } else {
                self.navigation.select(None);
                self.active_session = None;
                self.persist_shell_state()?;
            }
        }
        Ok(())
    }

    pub fn refresh_list(&mut self) -> Result<(), LibraryError> {
        self.projections = self.repository.list_notes(ListQuery::default())?;
        if self.navigation.selected_note_id().is_some_and(|id| {
            !self
                .projections
                .iter()
                .any(|projection| &projection.id == id)
        }) {
            self.navigation.select(None);
            self.active_session = None;
        }
        Ok(())
    }

    pub fn persist_shell_state(&self) -> Result<(), LibraryError> {
        self.repository
            .write_setting(PANE_SETTING, &self.panes.setting_value())?;
        if let Some(id) = self.navigation.selected_note_id() {
            self.repository
                .write_setting(SELECTED_NOTE_SETTING, id.as_str())?;
        }
        Ok(())
    }
    pub fn navigation(&self) -> &NavigationState {
        &self.navigation
    }
    pub fn projections(&self) -> &[NoteProjection] {
        &self.projections
    }
    pub fn active_session_note_id(&self) -> Option<&NoteId> {
        self.active_session.as_ref().map(|session| &session.note_id)
    }
    pub fn panes(&self) -> PaneState {
        self.panes
    }
    pub fn status(&self) -> &AppStatus {
        &self.status
    }
    pub fn set_search_query(&mut self, query: Option<String>) {
        self.navigation.set_search_query(query);
    }
    pub fn set_panes(&mut self, panes: PaneState) {
        self.panes = panes;
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
}

#[cfg(test)]
mod tests;
