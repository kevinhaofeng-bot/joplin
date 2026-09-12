mod actions;
mod navigation;
pub(crate) mod note_session;
pub(crate) mod save_coordinator;

pub use actions::*;
pub use navigation::*;

use app_lite_core::{
    CanonicalDocument, CreateNote as RepositoryCreateNote, LibraryError, LibraryEvent,
    LibraryNavigationIndex, LibraryRepository, LibraryRoute, LibraryShellState, ListQuery, Note,
    NoteId, NoteOrganizationState, NoteProjection, NotebookId, ResourceId, SearchHit,
    SortDirection, SortField,
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

/// Durable mutation already succeeded, but its route/index/projection/session
/// candidate did not. The recovery target is typed rather than inferred from
/// a status string: Create Note must recover its newly-created NoteId under
/// All Notes, whereas ordinary organization/trash mutations recover from the
/// existing route and deterministic selection fallback.
#[derive(Clone, Debug)]
enum PendingReconciliation {
    CurrentRoute,
    CreateNote {
        note: Note,
        destination_route: LibraryRoute,
    },
}

/// A note creation command is compiled from the current typed route before a
/// repository write. This is the local equivalent of Evernote's explicit
/// `CREATE_NEW_NOTE.container`: a Stack filters notes but never becomes a
/// fake direct note container.
#[derive(Clone, Debug)]
struct CreateNoteDestination {
    notebook_id: Option<NotebookId>,
    route: LibraryRoute,
}

#[derive(Clone, Debug)]
struct SearchRequestFence {
    generation: u64,
    query: String,
    base_snapshot: NavigationSnapshot,
}

pub struct AppModel {
    repository: Arc<LibraryRepository>,
    navigation: NavigationState,
    projections: Vec<NoteProjection>,
    /// Monotonic fence for retained background search work. A completed query
    /// may only publish if it still names the currently requested search.
    search_generation: u64,
    search_request: Option<SearchRequestFence>,
    /// Repository events may invalidate a committed SearchRoute packet. They
    /// are deliberately deferred to the shell's background FTS coordinator;
    /// a generic All Notes refresh must never replace these cards in place.
    search_refresh_pending: bool,
    navigation_index: LibraryNavigationIndex,
    active_session: Option<ActiveSession>,
    panes: PaneState,
    list_view_mode: ListViewMode,
    status: AppStatus,
    status_origin: StatusOrigin,
    // A repository mutation can commit before the subsequent projection
    // refresh/selection persistence fails. Keep that fact until an explicit
    // recovery action completes; a later projection event must not falsely
    // imply the user action was rolled back.
    partial_commit_message: Option<String>,
    /// A repository mutation has committed but its full route/index/list/
    /// session candidate has not yet been installed. This is deliberately
    /// broader than organization rows: Trash, Restore and Purge can also
    /// change the selected note's legal lifecycle or revision. Until a full
    /// candidate commits, the shell freezes its retained editor rather than
    /// letting a stale save fence accept input.
    reconciliation_pending: Option<PendingReconciliation>,
    #[cfg(test)]
    next_refresh_failure: Option<LibraryError>,
    #[cfg(test)]
    next_shell_state_persist_failure: Option<LibraryError>,
    #[cfg(test)]
    projection_event_refreshes: usize,
}

/// A fully prepared navigation/sort transition. Every field has already
/// passed its fallible repository work before the live model is touched, so a
/// failed target query, hydration, or selection persistence cannot publish a
/// route that disagrees with the mounted cards/editor.
struct PreparedNavigationCommit {
    navigation: NavigationState,
    projections: Vec<NoteProjection>,
    active_session: Option<ActiveSession>,
}

/// A repository organization mutation has already committed, but none of its
/// new sidebar/projection/session state becomes visible until all candidate
/// reads and any required target hydration have succeeded. The index belongs
/// in this same prepared packet so the three library columns cannot describe
/// different durable generations.
struct PreparedOrganizationCommit {
    navigation: NavigationState,
    projections: Vec<NoteProjection>,
    navigation_index: LibraryNavigationIndex,
    active_session: Option<ActiveSession>,
}

impl AppModel {
    pub fn open(repository: Arc<LibraryRepository>) -> Result<Self, LibraryError> {
        let saved_shell_state = repository.read_library_shell_state()?;
        let panes = PaneState::from_shell_state(&saved_shell_state);
        let navigation = NavigationState::default();
        let navigation_index = repository.list_navigation_index()?;
        let projections = repository.list_notes(
            ListQuery::for_route(navigation.route().clone()).with_sort(navigation.sort()),
        )?;
        let mut model = Self {
            repository,
            navigation,
            projections,
            search_generation: 0,
            search_request: None,
            search_refresh_pending: false,
            navigation_index,
            active_session: None,
            panes,
            list_view_mode: ListViewMode::default(),
            status: AppStatus::Ready,
            status_origin: StatusOrigin::Neutral,
            partial_commit_message: None,
            reconciliation_pending: None,
            #[cfg(test)]
            next_refresh_failure: None,
            #[cfg(test)]
            next_shell_state_persist_failure: None,
            #[cfg(test)]
            projection_event_refreshes: 0,
        };
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
            &action,
            AppAction::CreateNote
                | AppAction::CreateStack { .. }
                | AppAction::CreateNotebook { .. }
                | AppAction::CreateTag { .. }
                | AppAction::RenameStack { .. }
                | AppAction::RenameNotebook { .. }
                | AppAction::RenameTag { .. }
                | AppAction::DeleteStack(_)
                | AppAction::DeleteNotebook(_)
                | AppAction::DeleteTag(_)
                | AppAction::MoveSelectedNote(_)
                | AppAction::SetSelectedNoteTags(_)
                | AppAction::AddTagToSelectedNote(_)
                | AppAction::RemoveTagFromSelectedNote(_)
                | AppAction::SelectNote(_)
                | AppAction::TrashNote(_)
                | AppAction::TrashSelected
                | AppAction::RestoreNote(_)
                | AppAction::RestoreSelected
                | AppAction::PurgeNote(_)
                | AppAction::PurgeSelected
        );
        let result = match action {
            AppAction::CreateNote => self.create_note(),
            AppAction::CreateStack { title } => self
                .apply_organization_mutation("笔记本组已创建", move |repository| {
                    repository.create_stack(&title).map(|_| ())
                }),
            AppAction::CreateNotebook { title, stack_id } => {
                self.apply_organization_mutation("笔记本已创建", move |repository| {
                    repository
                        .create_notebook(&title, stack_id.as_ref())
                        .map(|_| ())
                })
            }
            AppAction::CreateTag { title } => self
                .apply_organization_mutation("标签已创建", move |repository| {
                    repository.create_tag(&title).map(|_| ())
                }),
            AppAction::RenameStack { id, title } => self
                .apply_organization_mutation("笔记本组已重命名", move |repository| {
                    repository.rename_stack(&id, &title).map(|_| ())
                }),
            AppAction::RenameNotebook { id, title } => self
                .apply_organization_mutation("笔记本已重命名", move |repository| {
                    repository.rename_notebook(&id, &title).map(|_| ())
                }),
            AppAction::RenameTag { id, title } => self
                .apply_organization_mutation("标签已重命名", move |repository| {
                    repository.rename_tag(&id, &title).map(|_| ())
                }),
            AppAction::DeleteStack(id) => self
                .apply_organization_mutation("笔记本组已解散", move |repository| {
                    repository.delete_stack(&id)
                }),
            AppAction::DeleteNotebook(id) => self
                .apply_organization_mutation("笔记本已删除", move |repository| {
                    repository.delete_notebook(&id)
                }),
            AppAction::DeleteTag(id) => self
                .apply_organization_mutation("标签已删除", move |repository| {
                    repository.delete_tag(&id)
                }),
            AppAction::MoveSelectedNote(notebook_id) => {
                self.selected_note_for_organization().and_then(|note_id| {
                    self.apply_organization_mutation("笔记已移动", move |repository| {
                        repository.move_selected_note(&note_id, &notebook_id)
                    })
                })
            }
            AppAction::SetSelectedNoteTags(tag_ids) => {
                self.selected_note_for_organization().and_then(|note_id| {
                    self.apply_organization_mutation("笔记标签已更新", move |repository| {
                        repository.set_note_tags(&note_id, &tag_ids)
                    })
                })
            }
            AppAction::AddTagToSelectedNote(tag_id) => {
                self.selected_note_for_organization().and_then(|note_id| {
                    self.apply_organization_mutation("笔记标签已更新", move |repository| {
                        repository.add_note_tag(&note_id, &tag_id)
                    })
                })
            }
            AppAction::RemoveTagFromSelectedNote(tag_id) => {
                self.selected_note_for_organization().and_then(|note_id| {
                    self.apply_organization_mutation("笔记标签已更新", move |repository| {
                        repository.remove_note_tag(&note_id, &tag_id)
                    })
                })
            }
            AppAction::SelectNote(id) => self.select_note(id),
            AppAction::NavigateTo {
                route,
                selected_note_id,
            } => self.navigate_to(route, selected_note_id),
            AppAction::NavigateBack => self.navigate_history(false),
            AppAction::NavigateForward => self.navigate_history(true),
            AppAction::TrashNote(id) => self.trash_note(id),
            AppAction::TrashSelected => self
                .navigation
                .selected_note_id()
                .cloned()
                .ok_or(LibraryError::NotFound)
                .and_then(|id| self.trash_note(id)),
            AppAction::RestoreNote(id) => self
                .apply_organization_mutation("笔记已恢复", move |repository| {
                    repository.restore_note(&id)
                }),
            AppAction::RestoreSelected => self.selected_note_for_organization().and_then(|id| {
                self.apply_organization_mutation("笔记已恢复", move |repository| {
                    repository.restore_note(&id)
                })
            }),
            AppAction::PurgeNote(id) => self
                .apply_organization_mutation("笔记已永久删除", move |repository| {
                    repository.purge_note(&id)
                }),
            AppAction::PurgeSelected => self.selected_note_for_organization().and_then(|id| {
                self.apply_organization_mutation("笔记已永久删除", move |repository| {
                    repository.purge_note(&id)
                })
            }),
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
                let mut candidate = self.navigation.clone();
                candidate.set_sort_for_route(sort.sort_spec());
                let prepared = self.prepare_navigation_commit(candidate)?;
                self.commit_navigation(prepared);
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
                if action_can_recover_partial && self.reconciliation_pending.is_none() {
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
        let destination = self.resolve_create_note_destination()?;
        // The repository transaction is the source of truth: no temporary UI note exists.
        let note = self.repository.create_note(RepositoryCreateNote {
            title: String::new(),
            notebook_id: destination.notebook_id.clone(),
            document: CanonicalDocument::default(),
        })?;
        match self
            .prepare_create_note_reconciliation_commit(note.clone(), destination.route.clone())
        {
            Ok(prepared) => {
                self.commit_organization(prepared);
                Ok(())
            }
            Err(error) => {
                self.record_create_note_reconciliation_partial_commit(
                    note,
                    destination.route,
                    &error,
                );
                Err(error)
            }
        }
    }

    /// Resolve a concrete Notebook owner before the Create Note transaction.
    /// A notebook route is already a durable note container. A stack route is
    /// not: prefer the currently selected child Note's notebook, otherwise
    /// accept the only child notebook. Multiple or zero children are an
    /// explicit user choice error, never an accidental default-notebook write.
    fn resolve_create_note_destination(&self) -> Result<CreateNoteDestination, LibraryError> {
        match self.navigation.route() {
            LibraryRoute::Notebook(id) => Ok(CreateNoteDestination {
                notebook_id: Some(id.clone()),
                route: LibraryRoute::Notebook(id.clone()),
            }),
            LibraryRoute::Stack(stack_id) => {
                let index = self.repository.list_navigation_index()?;
                let children = index
                    .notebooks
                    .iter()
                    .filter(|notebook| notebook.stack_id.as_ref() == Some(stack_id))
                    .collect::<Vec<_>>();
                let selected_child = self
                    .navigation
                    .selected_note_id()
                    .and_then(|selected_id| {
                        self.active_session.as_ref().filter(|active| {
                            active.note.id == *selected_id
                                && children
                                    .iter()
                                    .any(|child| child.id == active.note.notebook_id)
                        })
                    })
                    .map(|active| active.note.notebook_id.clone());
                let notebook_id = selected_child
                    .or_else(|| (children.len() == 1).then(|| children[0].id.clone()));
                let notebook_id = notebook_id.ok_or(LibraryError::StackNoteContainerRequired)?;
                Ok(CreateNoteDestination {
                    notebook_id: Some(notebook_id),
                    route: LibraryRoute::Stack(stack_id.clone()),
                })
            }
            // Tags are filters, not durable containers; a new untagged note
            // cannot legally remain selected there. Trash is deliberately
            // handled identically to preserve the existing safe editable
            // All Notes fallback.
            LibraryRoute::AllNotes | LibraryRoute::Tags(_) | LibraryRoute::Trash => {
                Ok(CreateNoteDestination {
                    notebook_id: None,
                    route: LibraryRoute::AllNotes,
                })
            }
        }
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
        self.repository.trash_note(&id)?;
        // The SQLite mutation has already committed, but its successor
        // selection, target Note hydration and persisted shell selection are
        // each fallible. Build them in the same candidate packet used for
        // organization mutations; never publish a list without its coherent
        // selected/active pair.
        match self.prepare_organization_commit() {
            Ok(prepared) => {
                self.commit_organization(prepared);
                Ok(())
            }
            Err(error) => {
                self.record_reconciliation_partial_commit("笔记已移至废纸篓", &error);
                Err(error)
            }
        }
    }

    fn selected_note_for_organization(&self) -> Result<NoteId, LibraryError> {
        self.navigation
            .selected_note_id()
            .cloned()
            .ok_or(LibraryError::NotFound)
    }

    /// Executes one durable organization mutation, then constructs all three
    /// visible library columns as a candidate before changing the live model.
    /// If the repository mutation itself fails, nothing is touched; if it has
    /// committed but a later candidate read fails, the old route/projections/
    /// selection/session remain coherent and the status truthfully records the
    /// committed data fact.
    fn apply_organization_mutation(
        &mut self,
        committed_action: &str,
        mutation: impl FnOnce(&LibraryRepository) -> Result<(), LibraryError>,
    ) -> Result<(), LibraryError> {
        mutation(Arc::as_ref(&self.repository))?;
        match self.prepare_organization_commit() {
            Ok(prepared) => {
                self.commit_organization(prepared);
                Ok(())
            }
            Err(error) => {
                self.record_reconciliation_partial_commit(committed_action, &error);
                Err(error)
            }
        }
    }

    /// Candidate used both immediately after a successful Create Note
    /// transaction and by the queued `NoteCreated` recovery event if that
    /// first candidate failed. The transaction-built `Note` is safe to reuse
    /// only while the durable lifecycle/revision metadata still matches it.
    /// A concurrent writer can advance or trash the new note before the
    /// queued event arrives, so recovery rehydrates the latest full note
    /// rather than publishing a newer card beside a stale active session.
    fn prepare_create_note_reconciliation_commit(
        &mut self,
        cached_note: Note,
        destination_route: LibraryRoute,
    ) -> Result<PreparedOrganizationCommit, LibraryError> {
        let Some(metadata) = self.repository.note_organization_state(&cached_note.id)? else {
            // The create did commit, but another writer may have purged it
            // before our first UI candidate. There is no remaining full Note
            // to mount, so clear the typed target through a coherent All
            // Notes/no-selection packet rather than freezing indefinitely.
            return self.prepare_removed_created_note_reconciliation_commit();
        };
        let note = if metadata.revision == cached_note.revision
            && metadata.deleted_time == cached_note.deleted_time
        {
            // The create transaction returned this complete snapshot. Avoid a
            // second body load in the ordinary no-race path.
            cached_note
        } else {
            let latest = self
                .repository
                .load_note(&cached_note.id)?
                .ok_or(LibraryError::NotFound)?;
            // Do not commit a packet assembled across two durable revisions.
            // The later event retry will build a fresh candidate if another
            // writer won between this metadata probe and full-note load.
            if latest.revision != metadata.revision || latest.deleted_time != metadata.deleted_time
            {
                return Err(LibraryError::StaleRevision {
                    expected: metadata.revision,
                    actual: latest.revision,
                });
            }
            latest
        };
        if note.deleted_time.is_some() {
            // A deleted note cannot be mounted as the editable active session
            // under All Notes. The source of truth is still represented by
            // the event and durable data, but the deterministic route
            // fallback has no stale selection to mutate.
            return self.prepare_removed_created_note_reconciliation_commit();
        }
        let navigation_index = self.repository.list_navigation_index()?;
        let destination_route = if route_is_available(&destination_route, &navigation_index) {
            destination_route
        } else {
            LibraryRoute::AllNotes
        };
        let (navigation, projections) =
            self.prepare_create_note_navigation(note.id.clone(), destination_route)?;
        self.persist_shell_state_for(&navigation)?;
        Ok(PreparedOrganizationCommit {
            navigation,
            projections,
            navigation_index,
            // The create transaction already returned a complete Note. Do
            // not insert a fallible post-commit body hydration here.
            active_session: Some(ActiveSession { note }),
        })
    }

    /// Build a route/projection candidate for a newly durable Note. When a
    /// concurrent organization mutation makes its requested context invalid
    /// (or makes the note leave that filter), All Notes is the only universal
    /// legal fallback. Construct from the old navigation each time so a
    /// failed first route never leaves a phantom history entry.
    fn prepare_create_note_navigation(
        &mut self,
        note_id: NoteId,
        destination_route: LibraryRoute,
    ) -> Result<(NavigationState, Vec<NoteProjection>), LibraryError> {
        let mut navigation = self.navigation.clone();
        // Creating a note leaves the current visible destination, but must
        // append that ordinary route after (not overwrite) SearchRoute. Back
        // must still be able to restore the exact committed search query.
        navigation.navigate_to(NavigationSnapshot {
            route: destination_route.clone(),
            destination: AppDestination::Library(destination_route.clone()),
            selected_note_id: Some(note_id.clone()),
        });
        let projections = self.load_projections_for(&navigation)?;
        if projections
            .iter()
            .any(|projection| projection.id == note_id)
        {
            return Ok((navigation, projections));
        }
        if destination_route == LibraryRoute::AllNotes {
            return Err(LibraryError::NotFound);
        }

        let mut fallback = self.navigation.clone();
        fallback.clear_search();
        fallback.navigate_to(NavigationSnapshot {
            route: LibraryRoute::AllNotes,
            destination: AppDestination::Library(LibraryRoute::AllNotes),
            selected_note_id: Some(note_id.clone()),
        });
        let projections = self.load_projections_for(&fallback)?;
        if !projections
            .iter()
            .any(|projection| projection.id == note_id)
        {
            return Err(LibraryError::NotFound);
        }
        Ok((fallback, projections))
    }

    fn prepare_removed_created_note_reconciliation_commit(
        &mut self,
    ) -> Result<PreparedOrganizationCommit, LibraryError> {
        let mut navigation = self.navigation.clone();
        navigation.clear_search();
        navigation.navigate_to(NavigationSnapshot {
            route: LibraryRoute::AllNotes,
            destination: AppDestination::Library(LibraryRoute::AllNotes),
            selected_note_id: None,
        });
        let projections = self.load_projections_for(&navigation)?;
        let navigation_index = self.repository.list_navigation_index()?;
        self.persist_shell_state_for(&navigation)?;
        Ok(PreparedOrganizationCommit {
            navigation,
            projections,
            navigation_index,
            active_session: None,
        })
    }

    fn prepare_pending_reconciliation_commit(
        &mut self,
    ) -> Result<PreparedOrganizationCommit, LibraryError> {
        match self.reconciliation_pending.clone() {
            Some(PendingReconciliation::CreateNote {
                note,
                destination_route,
            }) => self.prepare_create_note_reconciliation_commit(note, destination_route),
            Some(PendingReconciliation::CurrentRoute) | None => self.prepare_organization_commit(),
        }
    }

    fn prepare_organization_commit(&mut self) -> Result<PreparedOrganizationCommit, LibraryError> {
        let navigation_index = self.repository.list_navigation_index()?;
        if self.navigation.search_query().is_some() {
            // A SearchRoute owns the packet already in `projections`.  An
            // organization mutation may change sidebar metadata immediately,
            // but its compatibility AllNotes container is not permission to
            // publish an AllNotes card list into a still-visible SearchRoute.
            // Build the small candidate atomically, then let the shell query
            // the same typed route in the background.
            let mut navigation = self.navigation.clone();
            let active_session = match self.active_session.as_ref() {
                Some(active) => match self.repository.load_note(&active.note.id)? {
                    Some(note) => Some(ActiveSession { note }),
                    None => {
                        navigation.select(None);
                        None
                    }
                },
                None => None,
            };
            if navigation.selected_note_id() != self.navigation.selected_note_id() {
                self.persist_shell_state_for(&navigation)?;
            }
            return Ok(PreparedOrganizationCommit {
                navigation,
                projections: self.projections.clone(),
                navigation_index,
                active_session,
            });
        }
        let mut navigation = self.navigation.clone();
        navigation
            .sanitize_unavailable_routes(|route| route_is_available(route, &navigation_index));
        let prior_selected = navigation.selected_note_id().cloned();
        if !route_is_available(navigation.route(), &navigation_index) {
            navigation.replace_current_route(LibraryRoute::AllNotes);
            // A deleted tag/notebook route does not itself delete the note.
            // Preserve its stable ID long enough for the post-fallback card
            // query to retain the existing session when it still belongs in
            // All Notes; otherwise the deterministic successor rule below
            // clears/replaces it.
            navigation.select(prior_selected);
        }
        let projections = self.load_projections_for(&navigation)?;
        let selected_note_id = self.organization_selection_after_refresh(&projections, &navigation);
        navigation.select(selected_note_id);
        let active_session = match navigation.selected_note_id().cloned() {
            Some(id)
                if self
                    .active_session
                    .as_ref()
                    .is_some_and(|active| active.note.id == id) =>
            {
                let metadata = self
                    .repository
                    .note_organization_state(&id)?
                    .ok_or(LibraryError::NotFound)?;
                let mut note = self
                    .active_session
                    .as_ref()
                    .expect("matching active session was checked")
                    .note
                    .clone();
                note.notebook_id = metadata.notebook_id;
                note.tag_ids = metadata.tag_ids;
                note.updated_time = metadata.updated_time;
                note.deleted_time = metadata.deleted_time;
                note.revision = metadata.revision;
                Some(ActiveSession { note })
            }
            Some(id) => Some(ActiveSession {
                note: self
                    .repository
                    .load_note(&id)?
                    .ok_or(LibraryError::NotFound)?,
            }),
            None => None,
        };
        if navigation.selected_note_id() != self.navigation.selected_note_id() {
            self.persist_shell_state_for(&navigation)?;
        }
        Ok(PreparedOrganizationCommit {
            navigation,
            projections,
            navigation_index,
            active_session,
        })
    }

    fn organization_selection_after_refresh(
        &self,
        projections: &[NoteProjection],
        navigation: &NavigationState,
    ) -> Option<NoteId> {
        let selected = navigation.selected_note_id()?.clone();
        if projections
            .iter()
            .any(|projection| projection.id == selected)
        {
            return Some(selected);
        }
        let contains = |candidate: &NoteId| {
            projections
                .iter()
                .any(|projection| projection.id == *candidate)
        };
        self.projections
            .iter()
            .position(|projection| projection.id == selected)
            .and_then(|index| {
                self.projections[index + 1..]
                    .iter()
                    .map(|projection| &projection.id)
                    .find(|candidate| contains(candidate))
                    .cloned()
                    .or_else(|| {
                        self.projections[..index]
                            .iter()
                            .rev()
                            .map(|projection| &projection.id)
                            .find(|candidate| contains(candidate))
                            .cloned()
                    })
            })
            .or_else(|| projections.first().map(|projection| projection.id.clone()))
    }

    fn commit_organization(&mut self, prepared: PreparedOrganizationCommit) {
        let search_route_remains_active = prepared.navigation.search_query().is_some();
        self.navigation = prepared.navigation;
        self.projections = prepared.projections;
        self.navigation_index = prepared.navigation_index;
        self.active_session = prepared.active_session;
        // The candidate above deliberately preserved the old bounded packet.
        // Its FTS replacement is owned by the retained shell worker, never by
        // `load_projections_for(AllNotes)` on this foreground mutation path.
        self.search_refresh_pending |= search_route_remains_active;
        if self.reconciliation_pending.take().is_some() {
            self.partial_commit_message = None;
        }
    }

    pub fn refresh_list(&mut self) -> Result<(), LibraryError> {
        let navigation = self.navigation.clone();
        self.projections = self.load_projections_for(&navigation)?;
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
        let events = events.into_iter().collect::<Vec<_>>();
        let refresh_needed = events.iter().any(|event| {
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
        if self.navigation.search_query().is_some() {
            // SearchRoute owns a bounded FTS packet, not `ListQuery(AllNotes)`.
            // The retained shell observes this flag and recomputes the packet
            // on its background executor with a snapshot fence.
            if matches!(
                self.reconciliation_pending,
                Some(PendingReconciliation::CurrentRoute)
            ) {
                // Organization mutations still need their index/active-note
                // state reconciled, but must not install an All Notes card
                // packet while a typed SearchRoute is visible. Prepare every
                // fallible read first: never pair a retained old body with a
                // newer revision, and never partly publish the index.
                let navigation_index = self.repository.list_navigation_index()?;
                let active_session = match self.active_session.as_ref() {
                    Some(active) => self
                        .repository
                        .load_note(&active.note.id)?
                        .map(|note| ActiveSession { note }),
                    None => None,
                };
                let mut navigation = self.navigation.clone();
                if active_session.is_none() {
                    navigation.select(None);
                }
                if navigation.selected_note_id() != self.navigation.selected_note_id() {
                    self.persist_shell_state_for(&navigation)?;
                }
                self.navigation_index = navigation_index;
                self.active_session = active_session;
                self.navigation = navigation;
                self.reconciliation_pending = None;
                self.partial_commit_message = None;
                self.status = AppStatus::Ready;
                self.status_origin = StatusOrigin::Neutral;
            }
            self.search_refresh_pending = true;
            return Ok(true);
        }
        // Once any committed action is unreconciled, every relevant queued
        // event must retry the one full candidate, including NoteRestored and
        // NoteTrashed (which do not necessarily emit OrganizationChanged).
        // Otherwise an old active session could be paired with a newly legal
        // Trash/All Notes route and a same-card click could falsely clear its
        // committed-action warning.
        if self.reconciliation_pending.is_some()
            || events
                .iter()
                .any(|event| matches!(event, LibraryEvent::OrganizationChanged))
        {
            match self.prepare_pending_reconciliation_commit() {
                Ok(prepared) => {
                    self.commit_organization(prepared);
                    self.status = AppStatus::Ready;
                    self.status_origin = StatusOrigin::Neutral;
                    return Ok(true);
                }
                Err(error) => {
                    if self.status_origin != StatusOrigin::Action {
                        self.status = AppStatus::Error(error.to_string());
                        self.status_origin = StatusOrigin::ProjectionEvent;
                    }
                    return Err(error);
                }
            }
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

    /// Decide whether a queued repository event can actually replace the
    /// retained editor packet. This deliberately uses only navigation/index
    /// and note-organization metadata: a local action's echoed
    /// OrganizationChanged is often already reflected in `active_session`,
    /// and forcing a lifecycle flush for that no-op echo would reset the
    /// editor's 100ms journal/500ms settled-save timeline.
    pub(crate) fn event_batch_requires_active_session_replacement(
        &self,
        events: &[LibraryEvent],
    ) -> Result<bool, LibraryError> {
        let Some(active) = self.active_session.as_ref() else {
            return Ok(false);
        };
        let active_id = &active.note.id;
        // `PendingRepositoryEvents` intentionally bounds the bridge by
        // retaining one representative id per kind. A same-kind event for B
        // can therefore replace an earlier event for dirty active A in the
        // same 50ms batch. Never decide lifecycle safety from that lossy
        // representative: every event which can refresh a projection probes
        // A's own durable metadata before the shell considers unmounting it.
        let projection_affecting = events.iter().any(|event| {
            matches!(
                event,
                LibraryEvent::NoteCreated(_)
                    | LibraryEvent::NoteProjectionChanged(_)
                    | LibraryEvent::NoteTrashed(_)
                    | LibraryEvent::NoteRestored(_)
                    | LibraryEvent::OrganizationChanged
            )
        });
        if !projection_affecting {
            return Ok(false);
        }

        let Some(metadata) = self.repository.note_organization_state(active_id)? else {
            return Ok(true);
        };
        if metadata.revision != active.note.revision
            || metadata.deleted_time != active.note.deleted_time
        {
            return Ok(true);
        }
        // Stack/notebook/tag destruction can invalidate a route without
        // changing this note's own revision (for example stack disband).
        // This remains a lightweight index query; it never hydrates a body
        // or blob, and it must run for a lossy coalesced projection batch as
        // well as an explicit OrganizationChanged representative.
        let index = self.repository.list_navigation_index()?;
        if !route_is_available(self.navigation.route(), &index) {
            return Ok(true);
        }
        Ok(false)
    }

    pub fn subscribe_library_events(&self) -> Receiver<LibraryEvent> {
        self.repository.subscribe()
    }

    pub(crate) fn repository(&self) -> Arc<LibraryRepository> {
        Arc::clone(&self.repository)
    }

    /// Resolves the current selection only for the sidebar's lifecycle
    /// preflight. `prepare_navigation_commit` still performs the authoritative
    /// destination-projection check before publishing a route.
    pub(crate) fn sidebar_selected_note_for_route(
        &self,
        route: &LibraryRoute,
    ) -> Result<Option<NoteId>, LibraryError> {
        let Some(selected_note_id) = self.navigation.selected_note_id().cloned() else {
            return Ok(None);
        };
        let Some(metadata) = self.repository.note_organization_state(&selected_note_id)? else {
            return Ok(None);
        };
        // All Notes/Trash membership is fully described by the selected
        // note's metadata. Avoid a duplicate sidebar-tree query for those
        // two common routes; Notebook/Stack/Tag additionally need the
        // current typed entity index to reject tombstoned route IDs.
        let matches = match route {
            LibraryRoute::AllNotes => metadata.deleted_time.is_none(),
            LibraryRoute::Trash => metadata.deleted_time.is_some(),
            LibraryRoute::Notebook(_) | LibraryRoute::Stack(_) | LibraryRoute::Tags(_) => {
                let index = self.repository.list_navigation_index()?;
                note_organization_state_matches_route(&metadata, route, &index)
            }
        };
        Ok(matches.then_some(selected_note_id))
    }

    pub(crate) fn report_navigation_preflight_error(&mut self, error: &LibraryError) {
        self.set_action_error_status(error);
    }

    pub fn persist_shell_state(&mut self) -> Result<(), LibraryError> {
        let navigation = self.navigation.clone();
        self.persist_shell_state_for(&navigation)
    }

    fn persist_shell_state_for(
        &mut self,
        navigation: &NavigationState,
    ) -> Result<(), LibraryError> {
        #[cfg(test)]
        if let Some(error) = self.next_shell_state_persist_failure.take() {
            return Err(error);
        }
        self.repository
            .write_library_shell_state(&LibraryShellState {
                sidebar_width: self.panes.sidebar_width,
                list_width: self.panes.list_width,
                sidebar_visible: self.panes.sidebar_visible,
                list_visible: self.panes.list_visible,
                selected_note_id: navigation.selected_note_id().cloned(),
            })
    }
    pub fn navigation(&self) -> &NavigationState {
        &self.navigation
    }
    pub fn projections(&self) -> &[NoteProjection] {
        &self.projections
    }
    /// The metadata-only source for sidebar rows. It intentionally has a
    /// different shape from note projections, so rendering navigation cannot
    /// accidentally hydrate a note body or become a second card query.
    pub fn navigation_index(&self) -> &LibraryNavigationIndex {
        &self.navigation_index
    }
    pub fn active_session_note_id(&self) -> Option<&NoteId> {
        self.active_session.as_ref().map(|session| &session.note.id)
    }
    pub fn active_note(&self) -> Option<&Note> {
        self.active_session.as_ref().map(|session| &session.note)
    }

    /// Install a complete note result already assembled by the same durable
    /// snapshot transaction that changed the active session. This avoids a
    /// second body load and makes the selected card truthful in the very frame
    /// that the editor accepts a resource block; the normal projection event
    /// later remains a projection-only reconciliation, not the visual trigger.
    pub(crate) fn apply_active_resource_commit(
        &mut self,
        note: Note,
        selected_thumbnail_id: Option<ResourceId>,
    ) {
        if self.navigation.selected_note_id() != Some(&note.id) {
            return;
        }
        if let Some(active) = self.active_session.as_mut()
            && active.note.id == note.id
        {
            active.note = note.clone();
        }
        if let Some(projection) = self
            .projections
            .iter_mut()
            .find(|projection| projection.id == note.id)
        {
            projection.title_prefix = note.title.chars().take(120).collect();
            projection.snippet = note.snippet.chars().take(160).collect();
            projection.updated_time = note.updated_time;
            projection.attachment_count = note.resource_ids.len() as i64;
            // This is the canonical transaction outcome, not merely the ID
            // of an image that happened to be inserted. `None` is equally
            // meaningful: the transaction may have removed the old cover or
            // committed an attachment-only document, and the currently
            // mounted card must clear it in this same presentation cycle.
            projection.selected_thumbnail_id = selected_thumbnail_id;
        }
        self.sort_projections();
    }

    /// Apply the complete result of a normal title/body snapshot before a
    /// later organization candidate is allowed to reuse `active_session`.
    /// The session observer that delivers this result is retained only for
    /// the currently mounted note, and the ID check protects a queued stale
    /// completion after a selection change.  Unlike a resource transaction,
    /// this save has no independent thumbnail outcome, so it intentionally
    /// preserves the projection's canonical thumbnail field.
    pub(crate) fn apply_active_note_snapshot(&mut self, note: Note) {
        if self.navigation.selected_note_id() != Some(&note.id) {
            return;
        }
        let Some(active) = self.active_session.as_mut() else {
            return;
        };
        if active.note.id != note.id {
            return;
        }
        active.note = note.clone();
        if let Some(projection) = self
            .projections
            .iter_mut()
            .find(|projection| projection.id == note.id)
        {
            projection.title_prefix = note.title.chars().take(120).collect();
            projection.snippet = note.snippet.chars().take(160).collect();
            projection.updated_time = note.updated_time;
            projection.attachment_count = note.resource_ids.len() as i64;
        }
        self.sort_projections();
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
        NoteSort::from_sort_spec(self.navigation.sort())
    }
    pub fn set_search_query(&mut self, query: Option<String>) {
        self.navigation.set_search_query(query);
    }

    /// Begin an offline global-search request. This changes no projections and
    /// performs no repository I/O, so callers can schedule the query on GPUI's
    /// background executor while the editor remains mounted.
    pub fn begin_search(&mut self, query: impl Into<String>) -> u64 {
        let query = query.into();
        self.search_generation = self.search_generation.wrapping_add(1);
        self.search_request = Some(SearchRequestFence {
            generation: self.search_generation,
            query,
            base_snapshot: self.navigation.snapshot(),
        });
        self.search_generation
    }

    /// Read-only history lookahead for the shell's background coordinator.
    /// It never advances the cursor before FTS has produced a packet.
    pub fn pending_history_search_query(&self, forward: bool) -> Option<String> {
        self.navigation.history_search_query(forward)
    }

    /// Read-only target for a background SearchRoute history restoration.
    /// Advancing the native cursor is intentionally deferred until its
    /// bounded repository packet is ready to install atomically.
    pub fn pending_history_search(&self, forward: bool) -> Option<(String, NavigationSnapshot)> {
        self.navigation.history_search_snapshot(forward)
    }

    /// Returns an active SearchRoute packet that repository events have
    /// invalidated. This is read-only: the UI must do the query off-thread and
    /// call `commit_search_refresh` only after it has the bounded result set.
    pub fn pending_search_refresh(&self) -> Option<(String, NavigationSnapshot)> {
        if !self.search_refresh_pending {
            return None;
        }
        Some((
            self.navigation.search_query()?.to_owned(),
            self.navigation.snapshot(),
        ))
    }

    /// Atomically replace the cards of the already-active SearchRoute. It
    /// never creates history and cannot turn the search view into All Notes.
    pub fn commit_search_refresh(
        &mut self,
        query: &str,
        expected_snapshot: &NavigationSnapshot,
        hits: Vec<SearchHit>,
    ) -> Result<bool, LibraryError> {
        if !self.search_refresh_pending
            || self.navigation.snapshot() != *expected_snapshot
            || self.navigation.search_query() != Some(query)
        {
            return Ok(false);
        }
        let projections = hits.into_iter().map(|hit| hit.note).collect::<Vec<_>>();
        let mut navigation = self.navigation.clone();
        let active_session = match navigation.selected_note_id().cloned() {
            Some(id) if projections.iter().any(|projection| projection.id == id) => {
                match self.active_session.as_ref() {
                    Some(active) if active.note.id == id => Some(active.clone()),
                    _ => Some(ActiveSession {
                        note: self
                            .repository
                            .load_note(&id)?
                            .ok_or(LibraryError::NotFound)?,
                    }),
                }
            }
            Some(_) => {
                navigation.select(None);
                None
            }
            None => None,
        };
        if navigation.selected_note_id() != self.navigation.selected_note_id() {
            self.persist_shell_state_for(&navigation)?;
        }
        self.navigation = navigation;
        self.projections = projections;
        self.active_session = active_session;
        self.search_refresh_pending = false;
        Ok(true)
    }

    /// Publish an already-computed offline result packet into an existing
    /// Back/Forward SearchRoute. Unlike `commit_search_results`, this moves
    /// the current history cursor and never creates a new branch.
    pub fn commit_history_search_results(
        &mut self,
        forward: bool,
        query: &str,
        expected_current: &NavigationSnapshot,
        expected_target: &NavigationSnapshot,
        hits: Vec<SearchHit>,
    ) -> Result<bool, LibraryError> {
        if &self.navigation.snapshot() != expected_current {
            return Ok(false);
        }
        let mut navigation = self.navigation.clone();
        let snapshot = if forward {
            navigation.navigate_forward()
        } else {
            navigation.navigate_back()
        };
        let Some(snapshot) = snapshot else {
            return Ok(false);
        };
        if &snapshot != expected_target || navigation.search_query() != Some(query) {
            return Ok(false);
        }
        let projections = hits.into_iter().map(|hit| hit.note).collect::<Vec<_>>();
        let active_session = match navigation.selected_note_id().cloned() {
            Some(id) if projections.iter().any(|projection| projection.id == id) => {
                match self.active_session.as_ref() {
                    Some(active) if active.note.id == id => Some(active.clone()),
                    _ => Some(ActiveSession {
                        note: self
                            .repository
                            .load_note(&id)?
                            .ok_or(LibraryError::NotFound)?,
                    }),
                }
            }
            Some(_) => {
                navigation.select(None);
                None
            }
            None => None,
        };
        if navigation.selected_note_id() != self.navigation.selected_note_id() {
            self.persist_shell_state_for(&navigation)?;
        }
        // `snapshot` is intentionally consumed only after all fallible work:
        // a failed query must leave both cursor and mounted editor untouched.
        let _ = snapshot;
        self.navigation = navigation;
        self.projections = projections;
        self.active_session = active_session;
        self.search_refresh_pending = false;
        Ok(true)
    }

    /// Atomically publish a background FTS packet when it still belongs to the
    /// active request. `projections` is intentionally the single card-list
    /// authority; snippets stay transient to the palette renderer.
    pub fn commit_search_results(
        &mut self,
        generation: u64,
        query: String,
        hits: Vec<SearchHit>,
        selected_note_id: Option<NoteId>,
    ) -> Result<bool, LibraryError> {
        let Some(fence) = self.search_request.as_ref() else {
            return Ok(false);
        };
        if generation != self.search_generation
            || generation != fence.generation
            || query != fence.query
            || query.trim().is_empty()
            || self.navigation.snapshot() != fence.base_snapshot
        {
            return Ok(false);
        }
        let mut navigation = self.navigation.clone();
        navigation.navigate_to(NavigationSnapshot::search(query, selected_note_id));
        let projections = hits.into_iter().map(|hit| hit.note).collect::<Vec<_>>();
        let selected = navigation.selected_note_id().cloned();
        let active_session = match selected {
            Some(ref id) if projections.iter().any(|projection| &projection.id == id) => {
                match self.active_session.as_ref() {
                    Some(active) if &active.note.id == id => Some(active.clone()),
                    _ => Some(ActiveSession {
                        note: self
                            .repository
                            .load_note(id)?
                            .ok_or(LibraryError::NotFound)?,
                    }),
                }
            }
            Some(_) => {
                navigation.select(None);
                None
            }
            None => None,
        };
        if navigation.selected_note_id() != self.navigation.selected_note_id() {
            self.persist_shell_state_for(&navigation)?;
        }
        self.navigation = navigation;
        self.projections = projections;
        self.active_session = active_session;
        self.search_refresh_pending = false;
        self.search_request = None;
        Ok(true)
    }
    pub fn set_panes(&mut self, panes: PaneState) {
        self.panes = panes.normalized();
    }
    #[cfg(test)]
    pub fn fail_next_refresh_for_test(&mut self, error: LibraryError) {
        self.next_refresh_failure = Some(error);
    }
    #[cfg(test)]
    pub fn fail_next_shell_state_persist_for_test(&mut self, error: LibraryError) {
        self.next_shell_state_persist_failure = Some(error);
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

    fn navigate_to(
        &mut self,
        route: app_lite_core::LibraryRoute,
        selected_note_id: Option<NoteId>,
    ) -> Result<(), LibraryError> {
        let mut candidate = self.navigation.clone();
        candidate.navigate_to(NavigationSnapshot {
            destination: AppDestination::Library(route.clone()),
            route,
            selected_note_id,
        });
        let prepared = self.prepare_navigation_commit(candidate)?;
        self.commit_navigation(prepared);
        Ok(())
    }

    fn navigate_history(&mut self, forward: bool) -> Result<(), LibraryError> {
        let mut candidate = self.navigation.clone();
        let snapshot = if forward {
            candidate.navigate_forward()
        } else {
            candidate.navigate_back()
        };
        if snapshot.is_none() {
            return Ok(());
        }
        let prepared = self.prepare_navigation_commit(candidate)?;
        self.commit_navigation(prepared);
        Ok(())
    }

    fn prepare_navigation_commit(
        &mut self,
        mut navigation: NavigationState,
    ) -> Result<PreparedNavigationCommit, LibraryError> {
        // Search history has to be restored from an already-computed
        // background packet. Never let a browser-style Forward action quietly
        // reinterpret its All Notes container as a generic ListQuery.
        if navigation.search_query().is_some() {
            return Err(LibraryError::InvalidSnapshot);
        }
        let projections = self.load_projections_for(&navigation)?;
        let active_session = match navigation.selected_note_id().cloned() {
            Some(id) if projections.iter().any(|projection| projection.id == id) => {
                match self.active_session.as_ref() {
                    Some(active) if active.note.id == id => Some(active.clone()),
                    _ => Some(ActiveSession {
                        note: self
                            .repository
                            .load_note(&id)?
                            .ok_or(LibraryError::NotFound)?,
                    }),
                }
            }
            Some(_) => {
                navigation.select(None);
                None
            }
            None => None,
        };
        if navigation.selected_note_id() != self.navigation.selected_note_id() {
            self.persist_shell_state_for(&navigation)?;
        }
        Ok(PreparedNavigationCommit {
            navigation,
            projections,
            active_session,
        })
    }

    fn load_projections_for(
        &mut self,
        navigation: &NavigationState,
    ) -> Result<Vec<NoteProjection>, LibraryError> {
        #[cfg(test)]
        if let Some(error) = self.next_refresh_failure.take() {
            return Err(error);
        }
        self.repository.list_notes(
            ListQuery::for_route(navigation.route().clone()).with_sort(navigation.sort()),
        )
    }

    fn commit_navigation(&mut self, prepared: PreparedNavigationCommit) {
        self.navigation = prepared.navigation;
        self.projections = prepared.projections;
        self.active_session = prepared.active_session;
    }

    fn sort_projections(&mut self) {
        match (
            self.navigation.sort().field(),
            self.navigation.sort().direction(),
        ) {
            (SortField::Updated, SortDirection::Descending) => {
                self.projections.sort_by(|left, right| {
                    right
                        .updated_time
                        .cmp(&left.updated_time)
                        .then_with(|| left.id.cmp(&right.id))
                })
            }
            (SortField::Updated, SortDirection::Ascending) => {
                self.projections.sort_by(|left, right| {
                    left.updated_time
                        .cmp(&right.updated_time)
                        .then_with(|| left.id.cmp(&right.id))
                })
            }
            (SortField::Deleted, SortDirection::Descending) => {
                self.projections.sort_by(|left, right| {
                    right
                        .deleted_time
                        .cmp(&left.deleted_time)
                        .then_with(|| left.id.cmp(&right.id))
                })
            }
            (SortField::Deleted, SortDirection::Ascending) => {
                self.projections.sort_by(|left, right| {
                    left.deleted_time
                        .cmp(&right.deleted_time)
                        .then_with(|| left.id.cmp(&right.id))
                })
            }
            (SortField::Title, SortDirection::Ascending) => {
                self.projections.sort_by(|left, right| {
                    left.title_prefix
                        .to_ascii_lowercase()
                        .cmp(&right.title_prefix.to_ascii_lowercase())
                        .then_with(|| left.id.cmp(&right.id))
                })
            }
            (SortField::Title, SortDirection::Descending) => {
                self.projections.sort_by(|left, right| {
                    right
                        .title_prefix
                        .to_ascii_lowercase()
                        .cmp(&left.title_prefix.to_ascii_lowercase())
                        .then_with(|| left.id.cmp(&right.id))
                })
            }
        }
    }

    fn record_partial_commit(&mut self, committed_action: &str, error: &LibraryError) {
        self.partial_commit_message = Some(format!(
            "{committed_action}，但后续界面同步失败：{error}。资料库数据已提交；请重新打开资料库以恢复显示。"
        ));
    }

    fn record_reconciliation_partial_commit(
        &mut self,
        committed_action: &str,
        error: &LibraryError,
    ) {
        self.reconciliation_pending = Some(PendingReconciliation::CurrentRoute);
        self.record_partial_commit(committed_action, error);
    }

    fn record_create_note_reconciliation_partial_commit(
        &mut self,
        note: Note,
        destination_route: LibraryRoute,
        error: &LibraryError,
    ) {
        self.reconciliation_pending = Some(PendingReconciliation::CreateNote {
            note,
            destination_route,
        });
        self.record_partial_commit("笔记已创建", error);
    }

    /// The retained shell uses this typed model fact to freeze a stale
    /// session in the exact tick after a committed-but-unreconciled action.
    /// It is intentionally not inferred from presentation text: a localized
    /// warning must never become a mutation authority.
    pub(crate) fn reconciliation_pending(&self) -> bool {
        self.reconciliation_pending.is_some()
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

fn route_is_available(route: &LibraryRoute, index: &LibraryNavigationIndex) -> bool {
    match route {
        LibraryRoute::AllNotes | LibraryRoute::Trash => true,
        LibraryRoute::Notebook(id) => index.notebooks.iter().any(|notebook| notebook.id == *id),
        LibraryRoute::Stack(id) => index.stacks.iter().any(|stack| stack.id == *id),
        LibraryRoute::Tags(tag_ids) => tag_ids
            .iter()
            .all(|id| index.tags.iter().any(|tag| tag.id == *id)),
    }
}

fn note_organization_state_matches_route(
    note: &NoteOrganizationState,
    route: &LibraryRoute,
    index: &LibraryNavigationIndex,
) -> bool {
    match route {
        LibraryRoute::AllNotes => note.deleted_time.is_none(),
        LibraryRoute::Notebook(id) => {
            note.deleted_time.is_none()
                && note.notebook_id == *id
                && route_is_available(route, index)
        }
        LibraryRoute::Stack(id) => {
            note.deleted_time.is_none()
                && index.stacks.iter().any(|stack| stack.id == *id)
                && index.notebooks.iter().any(|notebook| {
                    notebook.id == note.notebook_id && notebook.stack_id.as_ref() == Some(id)
                })
        }
        LibraryRoute::Tags(tag_ids) => {
            note.deleted_time.is_none()
                && tag_ids.iter().all(|id| {
                    note.tag_ids.iter().any(|tag_id| tag_id == id)
                        && index.tags.iter().any(|tag| tag.id == *id)
                })
        }
        LibraryRoute::Trash => note.deleted_time.is_some(),
    }
}

#[cfg(test)]
mod image_flow_tests;
#[cfg(test)]
mod note_session_tests;
#[cfg(test)]
mod tests;
