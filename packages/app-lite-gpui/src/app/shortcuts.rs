//! Shortcuts resolve original durable identities through the existing
//! navigation candidate. They never own another copy of a note document.
use super::{AppModel, NavigationSnapshot};
use app_lite_core::{LibraryError, LibraryRoute, ShortcutTarget};

impl AppModel {
    pub(super) fn open_shortcut(&mut self, target: ShortcutTarget) -> Result<(), LibraryError> {
        // Read current membership/lifecycle, not the retained sidebar index.
        // A stale click must not open a trashed or unrelated replacement note.
        if !self
            .repository
            .list_shortcuts()?
            .iter()
            .any(|entry| entry.target == target)
        {
            return Err(LibraryError::NotFound);
        }
        let route = match &target {
            ShortcutTarget::Note(_) => LibraryRoute::AllNotes,
            ShortcutTarget::Notebook(id) => LibraryRoute::Notebook(id.clone()),
            ShortcutTarget::Stack(id) => LibraryRoute::Stack(id.clone()),
            ShortcutTarget::Tag(id) => LibraryRoute::tags(vec![id.clone()])?,
        };
        let exact_note = match &target {
            ShortcutTarget::Note(id) => Some(id),
            _ => None,
        };
        let selected = match exact_note {
            Some(id) => Some(id.clone()),
            None => self.sidebar_selected_note_for_route(&route)?,
        };
        let mut candidate = self.navigation.clone();
        candidate.navigate_to(NavigationSnapshot::library(route, selected));
        let prepared = self.prepare_navigation_commit_requiring_note(candidate, exact_note)?;
        self.commit_navigation(prepared);
        Ok(())
    }
}
