use app_lite_core::{LibraryRoute, NoteId, SortSpec};
use std::collections::BTreeMap;

/// A history entry contains only stable identities and typed route state. A
/// row index, hydrated note body, or UI entity never crosses this boundary.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AppDestination {
    Library(LibraryRoute),
    /// Global offline search is an application destination.  Its underlying
    /// library container remains All Notes; the query itself is durable native
    /// history state rather than an incidental text-field value.
    SearchRoute {
        query: String,
    },
}

impl AppDestination {
    fn library_route(&self) -> LibraryRoute {
        match self {
            Self::Library(route) => route.clone(),
            Self::SearchRoute { .. } => LibraryRoute::AllNotes,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NavigationSnapshot {
    pub route: LibraryRoute,
    pub destination: AppDestination,
    pub selected_note_id: Option<NoteId>,
}

impl NavigationSnapshot {
    pub fn library(route: LibraryRoute, selected_note_id: Option<NoteId>) -> Self {
        Self {
            route: route.clone(),
            destination: AppDestination::Library(route),
            selected_note_id,
        }
    }

    pub fn search(query: String, selected_note_id: Option<NoteId>) -> Self {
        Self {
            route: LibraryRoute::AllNotes,
            destination: AppDestination::SearchRoute { query },
            selected_note_id,
        }
    }

    fn normalized(mut self) -> Self {
        // `route` remains a compatibility mirror for the existing container
        // callers, but it is never an authority: destination derives it.
        self.route = self.destination.library_route();
        self
    }

    fn initial() -> Self {
        Self {
            route: LibraryRoute::AllNotes,
            destination: AppDestination::Library(LibraryRoute::AllNotes),
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
        self.entries[self.cursor] = snapshot.normalized();
    }

    fn push(&mut self, snapshot: NavigationSnapshot) {
        let snapshot = snapshot.normalized();
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

    /// A container/tag can be deleted while several typed snapshots still
    /// point at it. Keep user selection where All Notes can still resolve it,
    /// but never leave a Back/Forward entry that can resurrect a tombstoned
    /// navigation entity.
    fn replace_unavailable_routes(&mut self, available: impl Fn(&LibraryRoute) -> bool) {
        for snapshot in &mut self.entries {
            if let AppDestination::Library(route) = &snapshot.destination {
                if !available(route) {
                    snapshot.destination = AppDestination::Library(LibraryRoute::AllNotes);
                    snapshot.route = LibraryRoute::AllNotes;
                }
            }
        }
    }

    fn current(&self) -> NavigationSnapshot {
        self.entries[self.cursor].clone()
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
    destination: AppDestination,
    selected_note_id: Option<NoteId>,
    route_sorts: BTreeMap<LibraryRoute, SortSpec>,
    history: NavigationHistory,
}

impl Default for NavigationState {
    fn default() -> Self {
        Self {
            route: LibraryRoute::AllNotes,
            destination: AppDestination::Library(LibraryRoute::AllNotes),
            selected_note_id: None,
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
        match &self.destination {
            AppDestination::SearchRoute { query } => Some(query),
            AppDestination::Library(_) => None,
        }
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
            destination: self.destination.clone(),
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
        let snapshot = snapshot.normalized();
        self.apply_history_snapshot(&snapshot);
        self.history.push(self.snapshot());
    }

    /// A destructive organization mutation can make the current typed route
    /// unavailable (for example a deleted notebook or tag). That is not a
    /// user navigation, so replace the current history snapshot instead of
    /// appending a phantom route which Back would immediately revisit.
    pub(crate) fn replace_current_route(&mut self, route: LibraryRoute) {
        self.route = route.clone();
        self.destination = AppDestination::Library(route);
        self.selected_note_id = None;
        self.history.replace_current(self.snapshot());
    }

    /// Organization deletion is not user navigation, but it can invalidate
    /// more than the current route. Rewrite the complete native history in
    /// one candidate before `AppModel` commits its new index/projection
    /// packet, so Back/Forward cannot land on a ghost Notebook/Tag/Stack.
    pub(crate) fn sanitize_unavailable_routes(
        &mut self,
        available: impl Fn(&LibraryRoute) -> bool,
    ) {
        self.history.replace_unavailable_routes(&available);
        self.route_sorts.retain(|route, _| available(route));
        self.apply_history_snapshot(&self.history.current());
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
        self.destination = query.filter(|query| !query.trim().is_empty()).map_or_else(
            || AppDestination::Library(self.route.clone()),
            |query| AppDestination::SearchRoute { query },
        );
        self.route = self.destination.library_route();
        self.history.replace_current(self.snapshot());
    }

    pub(crate) fn clear_search(&mut self) {
        if matches!(self.destination, AppDestination::SearchRoute { .. }) {
            self.destination = AppDestination::Library(LibraryRoute::AllNotes);
            self.route = LibraryRoute::AllNotes;
            self.history.replace_current(self.snapshot());
        }
    }

    fn apply_history_snapshot(&mut self, snapshot: &NavigationSnapshot) {
        self.destination = snapshot.destination.clone();
        self.route = self.destination.library_route();
        self.selected_note_id = snapshot.selected_note_id.clone();
    }
}
