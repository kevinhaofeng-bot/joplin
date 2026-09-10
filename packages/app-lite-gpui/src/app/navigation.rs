use app_lite_core::NoteId;

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct NavigationState {
    selected_note_id: Option<NoteId>,
    search_query: Option<String>,
}

impl NavigationState {
    pub fn selected_note_id(&self) -> Option<&NoteId> {
        self.selected_note_id.as_ref()
    }

    pub fn search_query(&self) -> Option<&str> {
        self.search_query.as_deref()
    }

    pub(crate) fn select(&mut self, id: Option<NoteId>) {
        self.selected_note_id = id;
    }

    pub(crate) fn set_search_query(&mut self, query: Option<String>) {
        self.search_query = query.filter(|query| !query.trim().is_empty());
    }

    pub(crate) fn clear_search(&mut self) {
        self.search_query = None;
    }
}
