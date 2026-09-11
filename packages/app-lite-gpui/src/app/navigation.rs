use app_lite_core::{LibraryRoute, NoteId, SortSpec};
use std::collections::BTreeMap;

/// A history entry contains only stable identities and typed route state. A
/// row index, hydrated note body, or UI entity never crosses this boundary.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NavigationSnapshot {
    pub route: LibraryRoute,
    pub selected_note_id: Option<NoteId>,
}

impl NavigationSnapshot {
    fn initial() -> Self {
        Self {
            route: LibraryRoute::AllNotes,
            selected_note_id: None,
        }
    }
}

/// Native Back/Forward state for a GPUI application. It deliberately does not
/// reuse browser history: a projection refresh is not navigation and cannot
/// create an entry here.
#[derive(Clone, Debug)]
pub struct NavigationHistory {
    entries: Vec<NavigationSnapshot>,
    cursor: usize,
}

impl Default for NavigationHistory {
    fn default() -> Self {
        Self {
            entries: vec![NavigationSnapshot::initial()],
            cursor: 0,
        }
    }
}

impl NavigationHistory {
    fn replace_current(&mut self, snapshot: NavigationSnapshot) {
        self.entries[self.cursor] = snapshot;
    }

    fn push(&mut self, snapshot: NavigationSnapshot) {
        // A route action after Back is a new branch even when the selected
        // note happens to equal the current one.
        self.entries.truncate(self.cursor + 1);
        if self.entries.last() == Some(&snapshot) {
            return;
        }
        self.entries.push(snapshot);
        self.cursor = self.entries.len() - 1;
    }

    fn back(&mut self) -> Option<NavigationSnapshot> {
        self.cursor.checked_sub(1).map(|cursor| {
            self.cursor = cursor;
            self.entries[cursor].clone()
        })
    }

    fn forward(&mut self) -> Option<NavigationSnapshot> {
        let cursor = self.cursor + 1;
        (cursor < self.entries.len()).then(|| {
            self.cursor = cursor;
            self.entries[cursor].clone()
        })
    }

    pub fn can_navigate_back(&self) -> bool {
        self.cursor > 0
    }

    pub fn can_navigate_forward(&self) -> bool {
        self.cursor + 1 < self.entries.len()
    }

    #[cfg(test)]
    pub(crate) fn len_for_test(&self) -> usize {
        self.entries.len()
    }
}

#[derive(Clone, Debug)]
pub struct NavigationState {
    route: LibraryRoute,
    selected_note_id: Option<NoteId>,
    search_query: Option<String>,
    route_sorts: BTreeMap<LibraryRoute, SortSpec>,
    history: NavigationHistory,
}

impl Default for NavigationState {
    fn default() -> Self {
        Self {
            route: LibraryRoute::AllNotes,
            selected_note_id: None,
            search_query: None,
            route_sorts: BTreeMap::new(),
            history: NavigationHistory::default(),
        }
    }
}

impl NavigationState {
    pub fn route(&self) -> &LibraryRoute {
        &self.route
    }

    pub fn selected_note_id(&self) -> Option<&NoteId> {
        self.selected_note_id.as_ref()
    }

    pub fn search_query(&self) -> Option<&str> {
        self.search_query.as_deref()
    }

    pub fn sort(&self) -> SortSpec {
        self.route_sorts
            .get(&self.route)
            .copied()
            .unwrap_or_else(|| self.route.default_sort())
    }

    pub fn snapshot(&self) -> NavigationSnapshot {
        NavigationSnapshot {
            route: self.route.clone(),
            selected_note_id: self.selected_note_id.clone(),
        }
    }

    pub fn can_navigate_back(&self) -> bool {
        self.history.can_navigate_back()
    }

    pub fn can_navigate_forward(&self) -> bool {
        self.history.can_navigate_forward()
    }

    #[cfg(test)]
    pub(crate) fn history_len_for_test(&self) -> usize {
        self.history.len_for_test()
    }

    /// Updates the selected NoteId inside the current history entry but does
    /// not create a new navigation branch for ordinary row selection.
    pub(crate) fn select(&mut self, id: Option<NoteId>) {
        self.selected_note_id = id;
        self.history.replace_current(self.snapshot());
    }

    pub(crate) fn navigate_to(&mut self, snapshot: NavigationSnapshot) {
        self.route = snapshot.route;
        self.selected_note_id = snapshot.selected_note_id;
        self.history.push(self.snapshot());
    }

    pub(crate) fn navigate_back(&mut self) -> Option<NavigationSnapshot> {
        let snapshot = self.history.back()?;
        self.apply_history_snapshot(&snapshot);
        Some(snapshot)
    }

    pub(crate) fn navigate_forward(&mut self) -> Option<NavigationSnapshot> {
        let snapshot = self.history.forward()?;
        self.apply_history_snapshot(&snapshot);
        Some(snapshot)
    }

    pub(crate) fn set_sort_for_route(&mut self, sort: SortSpec) {
        self.route_sorts.insert(self.route.clone(), sort);
    }

    pub(crate) fn set_search_query(&mut self, query: Option<String>) {
        self.search_query = query.filter(|query| !query.trim().is_empty());
    }

    pub(crate) fn clear_search(&mut self) {
        self.search_query = None;
    }

    fn apply_history_snapshot(&mut self, snapshot: &NavigationSnapshot) {
        self.route = snapshot.route.clone();
        self.selected_note_id = snapshot.selected_note_id.clone();
    }
}
