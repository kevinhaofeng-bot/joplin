use super::{AppAction, AppModel, AppStatus, ListViewMode, NoteSort, PaneState};
use app_lite_core::document::{Block, BlockStyle, Inline};
use app_lite_core::{
    CanonicalDocument, CreateNote, LibraryEvent, LibraryRepository, LibraryRoute,
    LibraryShellState, Note, NoteId, NotebookId, SaveNote,
};
use std::sync::Arc;
use std::sync::mpsc::TryRecvError;

fn repository() -> (tempfile::TempDir, Arc<LibraryRepository>) {
    let profile = tempfile::tempdir().expect("temporary profile");
    let repository = Arc::new(
        LibraryRepository::open(profile.path().join("library.sqlite"))
            .expect("open temporary library"),
    );
    (profile, repository)
}

fn create(repository: &LibraryRepository, title: &str) -> NoteId {
    repository
        .create_note(CreateNote {
            title: title.into(),
            notebook_id: None,
            document: CanonicalDocument::default(),
        })
        .expect("create note")
        .id
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct NavigationCommitProbe {
    snapshot: super::NavigationSnapshot,
    sort: NoteSort,
    can_navigate_back: bool,
    can_navigate_forward: bool,
    history_len: usize,
    projection_ids: Vec<NoteId>,
    active_note: Option<Note>,
}

fn navigation_commit_probe(model: &AppModel) -> NavigationCommitProbe {
    NavigationCommitProbe {
        snapshot: model.navigation().snapshot(),
        sort: model.sort(),
        can_navigate_back: model.navigation().can_navigate_back(),
        can_navigate_forward: model.navigation().can_navigate_forward(),
        history_len: model.navigation().history_len_for_test(),
        projection_ids: model
            .projections()
            .iter()
            .map(|projection| projection.id.clone())
            .collect(),
        active_note: model.active_note().cloned(),
    }
}

fn assert_navigation_commit_unchanged(model: &AppModel, before: &NavigationCommitProbe) {
    assert_eq!(navigation_commit_probe(model), *before);
}

fn navigation_fixture() -> (
    tempfile::TempDir,
    Arc<LibraryRepository>,
    app_lite_core::Notebook,
    NoteId,
    NoteId,
) {
    let (profile, repository) = repository();
    let notebook = repository
        .create_notebook("项目", None)
        .expect("create notebook");
    let all_note = create(&repository, "所有笔记");
    let notebook_note = repository
        .create_note(CreateNote {
            title: "项目笔记".into(),
            notebook_id: Some(notebook.id.clone()),
            document: CanonicalDocument::default(),
        })
        .expect("create notebook note")
        .id;
    (profile, repository, notebook, all_note, notebook_note)
}

#[test]
fn create_note_persists_before_it_becomes_selected() {
    let (_profile, repository) = repository();
    let mut model = AppModel::open(Arc::clone(&repository)).expect("open model");

    model
        .dispatch(AppAction::CreateNote)
        .expect("create action");

    let id = model
        .navigation()
        .selected_note_id()
        .cloned()
        .expect("selection");
    assert!(repository.load_note(&id).expect("load").is_some());
    assert_eq!(model.active_session_note_id(), Some(&id));
}

#[test]
fn create_note_installs_the_transaction_snapshot_without_post_commit_hydration() {
    // Catches create_note returning only an ID and hydrating it after commit.
    // The repository must return its transaction-built complete Note so a
    // later I/O fault cannot disguise an already-committed create as failure.
    let (_profile, repository) = repository();
    let loads = repository.observe_note_loads();
    let mut model = AppModel::open(Arc::clone(&repository)).expect("open model");

    model
        .dispatch(AppAction::CreateNote)
        .expect("create action");

    let selected = model
        .navigation()
        .selected_note_id()
        .cloned()
        .expect("created note becomes selected after refresh");
    assert_eq!(
        loads.try_recv(),
        Err(TryRecvError::Empty),
        "create must install its transaction snapshot without any post-commit load_note"
    );
    assert_eq!(model.active_session_note_id(), Some(&selected));
}

#[test]
fn create_note_from_trash_prepares_all_notes_with_the_new_editable_note_in_its_projection() {
    // A fresh ordinary note cannot remain selected under a Trash query. This
    // catches the old `clear_search + refresh_list(Trash)` path which paired
    // a new editable session with cards that could not contain it and showed
    // destructive Trash controls for the wrong lifecycle.
    let (_profile, repository) = repository();
    let trashed = create(&repository, "废纸篓中的旧笔记");
    repository.trash_note(&trashed).expect("trash fixture note");
    let mut model = AppModel::open(Arc::clone(&repository)).expect("open model");
    model
        .dispatch(AppAction::NavigateTo {
            route: LibraryRoute::Trash,
            selected_note_id: Some(trashed),
        })
        .expect("navigate Trash");

    model.dispatch(AppAction::CreateNote).expect("create note");
    let selected = model
        .navigation()
        .selected_note_id()
        .cloned()
        .expect("new note selected");
    assert_eq!(model.navigation().route(), &LibraryRoute::AllNotes);
    assert!(
        model
            .projections()
            .iter()
            .any(|projection| projection.id == selected)
    );
    let active = model.active_note().expect("new editable active note");
    assert_eq!(active.id, selected);
    assert!(active.deleted_time.is_none());
}

#[test]
fn create_note_from_notebook_route_uses_that_notebook_as_its_durable_container() {
    // Mutation-sensitive: the visible nested Notebook route must cross the
    // AppModel/repository boundary as a typed NotebookId. Replacing the
    // route-aware input with `notebook_id: None` silently creates this note
    // in the default notebook instead.
    let (_profile, repository) = repository();
    let stack = repository.create_stack("归档组").expect("create stack");
    let notebook = repository
        .create_notebook("In stack notebook", Some(&stack.id))
        .expect("create nested notebook");
    let default = repository.default_notebook().expect("default notebook");
    assert_ne!(
        notebook.id, default.id,
        "fixture must distinguish containers"
    );
    let existing = repository
        .create_note(CreateNote {
            title: "组内原笔记".into(),
            notebook_id: Some(notebook.id.clone()),
            document: CanonicalDocument::default(),
        })
        .expect("create nested fixture note");
    let mut model = AppModel::open(Arc::clone(&repository)).expect("open model");
    model
        .dispatch(AppAction::NavigateTo {
            route: LibraryRoute::Notebook(notebook.id.clone()),
            selected_note_id: Some(existing.id),
        })
        .expect("navigate nested notebook");

    model.dispatch(AppAction::CreateNote).expect("create note");

    let created = model
        .navigation()
        .selected_note_id()
        .cloned()
        .expect("created note selected");
    let durable = repository
        .load_note(&created)
        .expect("load durable created note")
        .expect("created note remains durable");
    assert_eq!(durable.notebook_id, notebook.id);
    assert_eq!(
        model.navigation().route(),
        &LibraryRoute::Notebook(notebook.id.clone()),
        "the created NoteId must remain legal in the same notebook projection"
    );
    assert_eq!(model.active_note(), Some(&durable));
    assert!(
        model
            .projections()
            .iter()
            .any(|projection| projection.id == created),
        "the selected created note must occur in the mounted route's cards"
    );
}

#[test]
fn create_note_from_stack_route_uses_the_selected_child_notebook_as_its_container() {
    // A Stack is a typed navigation/filter entity, not a notes container.
    // When a selected child note supplies a valid container, it is the least
    // surprising durable target and avoids an accidental default-notebook
    // write.
    let (_profile, repository) = repository();
    let stack = repository.create_stack("当前组").expect("create stack");
    let selected_child = repository
        .create_notebook("In stack notebook", Some(&stack.id))
        .expect("create selected child notebook");
    let existing = repository
        .create_note(CreateNote {
            title: "组内当前笔记".into(),
            notebook_id: Some(selected_child.id.clone()),
            document: CanonicalDocument::default(),
        })
        .expect("create selected stack note");
    let mut model = AppModel::open(Arc::clone(&repository)).expect("open model");
    model
        .dispatch(AppAction::NavigateTo {
            route: LibraryRoute::Stack(stack.id.clone()),
            selected_note_id: Some(existing.id),
        })
        .expect("navigate stack");

    model.dispatch(AppAction::CreateNote).expect("create note");

    let created = model
        .navigation()
        .selected_note_id()
        .cloned()
        .expect("created note selected");
    let durable = repository
        .load_note(&created)
        .expect("load durable created note")
        .expect("created note remains durable");
    assert_eq!(durable.notebook_id, selected_child.id);
    assert_eq!(model.navigation().route(), &LibraryRoute::Stack(stack.id));
    assert!(
        model
            .projections()
            .iter()
            .any(|projection| projection.id == created)
    );
}

#[test]
fn create_note_from_stack_with_one_child_uses_that_child_without_a_selected_note() {
    // The Stack route is still actionable before a card is selected when it
    // has exactly one legal Notebook container. This must not force people to
    // create a throwaway note merely to establish context.
    let (_profile, repository) = repository();
    let stack = repository.create_stack("单笔记本组").expect("create stack");
    let child = repository
        .create_notebook("唯一子笔记本", Some(&stack.id))
        .expect("create only child");
    let mut model = AppModel::open(Arc::clone(&repository)).expect("open model");
    model
        .dispatch(AppAction::NavigateTo {
            route: LibraryRoute::Stack(stack.id.clone()),
            selected_note_id: None,
        })
        .expect("navigate stack");

    model.dispatch(AppAction::CreateNote).expect("create note");

    let created = model
        .navigation()
        .selected_note_id()
        .cloned()
        .expect("created note selected");
    assert_eq!(
        repository
            .load_note(&created)
            .expect("load created note")
            .expect("durable created note")
            .notebook_id,
        child.id
    );
    assert_eq!(model.navigation().route(), &LibraryRoute::Stack(stack.id));
}

#[test]
fn create_note_from_an_empty_stack_fails_without_falling_back_to_the_default_notebook() {
    // An empty Stack has no typed Notebook container. Treating it as `None`
    // would create a surprising default-notebook note and make the Stack
    // projection/selection incoherent.
    let (_profile, repository) = repository();
    let stack = repository.create_stack("空组").expect("create empty stack");
    let default = repository.default_notebook().expect("default notebook");
    let mut model = AppModel::open(Arc::clone(&repository)).expect("open model");
    model
        .dispatch(AppAction::NavigateTo {
            route: LibraryRoute::Stack(stack.id),
            selected_note_id: None,
        })
        .expect("navigate empty stack");

    assert!(model.dispatch(AppAction::CreateNote).is_err());
    assert!(model.projections().is_empty());
    assert!(
        repository
            .list_notes(app_lite_core::ListQuery::for_route(LibraryRoute::AllNotes))
            .expect("read all notes")
            .is_empty()
    );
    assert_eq!(
        repository.default_notebook().expect("reload default").id,
        default.id
    );
}

#[test]
fn create_note_from_an_ambiguous_stack_requires_a_concrete_child_notebook() {
    // More than one child gives a Stack no unambiguous concrete container
    // until the user selects a note/Notebook. Picking the first row would
    // make a sort change silently change where a new note is persisted.
    let (_profile, repository) = repository();
    let stack = repository.create_stack("多笔记本组").expect("create stack");
    repository
        .create_notebook("甲", Some(&stack.id))
        .expect("create first child");
    repository
        .create_notebook("乙", Some(&stack.id))
        .expect("create second child");
    let mut model = AppModel::open(Arc::clone(&repository)).expect("open model");
    model
        .dispatch(AppAction::NavigateTo {
            route: LibraryRoute::Stack(stack.id),
            selected_note_id: None,
        })
        .expect("navigate stack");

    assert!(model.dispatch(AppAction::CreateNote).is_err());
    assert!(
        repository
            .list_notes(app_lite_core::ListQuery::for_route(LibraryRoute::AllNotes))
            .expect("read all notes")
            .is_empty()
    );
}

#[test]
fn pending_notebook_create_recovery_retains_its_typed_container_route() {
    // The SQLite create may commit before its first route/projection candidate
    // completes. Retrying from only a cached Note would regress a valid
    // Notebook creation back to All Notes, so the pending fact must retain the
    // original typed container route until the full candidate commits.
    let (_profile, repository) = repository();
    let notebook = repository
        .create_notebook("候选恢复笔记本", None)
        .expect("create notebook");
    let mut model = AppModel::open(Arc::clone(&repository)).expect("open model");
    model
        .dispatch(AppAction::NavigateTo {
            route: LibraryRoute::Notebook(notebook.id.clone()),
            selected_note_id: None,
        })
        .expect("navigate notebook");
    model.fail_next_refresh_for_test(app_lite_core::LibraryError::NotFound);

    assert!(model.dispatch(AppAction::CreateNote).is_err());
    let created = repository
        .list_notes(app_lite_core::ListQuery::for_route(LibraryRoute::Notebook(
            notebook.id.clone(),
        )))
        .expect("read committed notebook note")
        .into_iter()
        .next()
        .expect("create committed before candidate failure")
        .id;
    assert!(model.reconciliation_pending());

    model
        .refresh_projection_events([LibraryEvent::NoteCreated(created.clone())])
        .expect("queued event retries full candidate");

    assert!(!model.reconciliation_pending());
    assert_eq!(
        model.navigation().route(),
        &LibraryRoute::Notebook(notebook.id)
    );
    assert_eq!(model.navigation().selected_note_id(), Some(&created));
    assert_eq!(model.active_session_note_id(), Some(&created));
}

#[test]
fn creating_note_clears_search_context() {
    let (_profile, repository) = repository();
    let mut model = AppModel::open(repository).expect("open model");
    model.set_search_query(Some("遗留查询".into()));

    model
        .dispatch(AppAction::CreateNote)
        .expect("create action");

    assert_eq!(model.navigation().search_query(), None);
}

#[test]
fn selection_uses_note_id_and_survives_sort_refresh() {
    let (_profile, repository) = repository();
    let first = create(&repository, "first");
    let selected = create(&repository, "selected");
    let mut model = AppModel::open(repository).expect("open model");
    model
        .dispatch(AppAction::SelectNote(selected.clone()))
        .expect("select");

    model.set_projection_for_test(vec![selected.clone(), first]);
    model.refresh_list().expect("refresh");

    assert_eq!(model.navigation().selected_note_id(), Some(&selected));
}

#[test]
fn trashing_selected_note_selects_nearest_surviving_card() {
    let (_profile, repository) = repository();
    let first = create(&repository, "first");
    let selected = create(&repository, "selected");
    let third = create(&repository, "third");
    let mut model = AppModel::open(repository).expect("open model");
    model.set_projection_for_test(vec![first, selected.clone(), third.clone()]);
    model
        .dispatch(AppAction::SelectNote(selected.clone()))
        .expect("select");

    model
        .dispatch(AppAction::TrashNote(selected))
        .expect("trash");

    assert_eq!(model.navigation().selected_note_id(), Some(&third));
}

#[test]
fn trashing_first_selected_note_selects_the_next_card() {
    let (_profile, repository) = repository();
    let first = create(&repository, "first");
    let second = create(&repository, "second");
    let third = create(&repository, "third");
    let mut model = AppModel::open(repository).expect("open model");
    model.set_projection_for_test(vec![first.clone(), second.clone(), third]);
    model
        .dispatch(AppAction::SelectNote(first))
        .expect("select first");

    model
        .dispatch(AppAction::TrashSelected)
        .expect("trash first");

    assert_eq!(model.navigation().selected_note_id(), Some(&second));
}

#[test]
fn trashing_last_selected_note_selects_the_previous_card() {
    let (_profile, repository) = repository();
    let first = create(&repository, "first");
    let second = create(&repository, "second");
    let third = create(&repository, "third");
    let mut model = AppModel::open(repository).expect("open model");
    model.set_projection_for_test(vec![first, second.clone(), third.clone()]);
    model
        .dispatch(AppAction::SelectNote(third))
        .expect("select last");

    model
        .dispatch(AppAction::TrashSelected)
        .expect("trash last");

    assert_eq!(model.navigation().selected_note_id(), Some(&second));
}

#[test]
fn failed_trash_keeps_the_existing_selected_session_intact() {
    let (_profile, repository) = repository();
    let selected = create(&repository, "selected");
    let missing = NoteId::parse("ffffffffffffffffffffffffffffffff").expect("valid opaque id");
    let mut model = AppModel::open(Arc::clone(&repository)).expect("open model");
    model
        .dispatch(AppAction::SelectNote(selected.clone()))
        .expect("select note");

    assert!(model.dispatch(AppAction::TrashNote(missing)).is_err());

    assert_eq!(model.navigation().selected_note_id(), Some(&selected));
    assert_eq!(model.active_session_note_id(), Some(&selected));
    assert_eq!(
        repository
            .list_notes(Default::default())
            .expect("read unaffected library")
            .len(),
        1
    );
    assert!(matches!(model.status(), AppStatus::Error(_)));
}

#[test]
fn restart_restores_panes_and_a_valid_note_selection() {
    let (_profile, repository) = repository();
    let selected = create(&repository, "selected");
    let mut first = AppModel::open(Arc::clone(&repository)).expect("open first");
    first.set_panes(PaneState::new(260, 410));
    first
        .dispatch(AppAction::SelectNote(selected.clone()))
        .expect("select");
    first.persist_shell_state().expect("persist");
    drop(first);

    let resumed = AppModel::open(repository).expect("resume");
    assert_eq!(resumed.panes(), PaneState::new(260, 410));
    assert_eq!(resumed.navigation().selected_note_id(), Some(&selected));
}

#[test]
fn set_panes_normalizes_invalid_live_dimensions_before_persisting() {
    // Catches a resize/UI caller leaving an invalid 0px or oversized pane in
    // AppModel memory even though the repository rejects it later.
    let (_profile, repository) = repository();
    let mut model = AppModel::open(repository).expect("open model");

    model.set_panes(PaneState {
        sidebar_width: 0,
        list_width: u16::MAX,
        sidebar_visible: true,
        list_visible: true,
    });

    assert_eq!(model.panes(), PaneState::new(220, 360));
}

#[test]
fn invalid_saved_selection_falls_back_without_loading_a_body() {
    let (_profile, repository) = repository();
    let stale = NoteId::parse("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa").expect("valid opaque ID");
    repository
        .write_library_shell_state(&LibraryShellState {
            selected_note_id: Some(stale),
            ..LibraryShellState::default()
        })
        .expect("write stale setting");
    let observed = repository.observe_next_list_query();

    let resumed = AppModel::open(Arc::clone(&repository)).expect("resume");

    assert_eq!(resumed.navigation().selected_note_id(), None);
    assert_eq!(
        repository
            .read_library_shell_state()
            .expect("stale setting should be cleared")
            .selected_note_id,
        None,
        "an invalid restored selection must not be retried on every launch"
    );
    let fields = observed.recv().expect("list observation");
    assert!(!fields.iter().any(|field| {
        matches!(
            field.as_str(),
            "notes.body_html" | "notes.body_text" | "notes.merge_state"
        )
    }));
}

#[test]
fn opening_library_hydrates_only_projections_not_note_bodies() {
    let (_profile, repository) = repository();
    create(&repository, "one");
    create(&repository, "two");
    let observed = repository.observe_next_list_query();

    let model = AppModel::open(repository).expect("open model");

    assert_eq!(model.projections().len(), 2);
    let fields = observed.recv().expect("list observation");
    assert!(!fields.iter().any(|field| {
        matches!(
            field.as_str(),
            "notes.body_html" | "notes.body_text" | "notes.merge_state"
        )
    }));
}

#[test]
fn projection_actions_never_hydrate_a_body_and_an_explicit_selection_loads_once() {
    let (_profile, repository) = repository();
    let first = create(&repository, "Alpha");
    create(&repository, "Zulu");
    let loads = repository.observe_note_loads();
    let mut model = AppModel::open(Arc::clone(&repository)).expect("open model");

    model
        .dispatch(AppAction::SetSort(NoteSort::TitleDescending))
        .expect("sort projections");
    model
        .dispatch(AppAction::SetListViewMode(ListViewMode::Compact))
        .expect("change card presentation");
    assert_eq!(loads.try_recv(), Err(TryRecvError::Empty));

    model
        .dispatch(AppAction::SelectNote(first.clone()))
        .expect("explicit selection");
    assert_eq!(loads.try_recv(), Ok(first.clone()));
    assert_eq!(
        loads.try_recv(),
        Err(TryRecvError::Empty),
        "one selection must not eagerly hydrate additional note bodies"
    );

    model
        .dispatch(AppAction::SelectNote(first))
        .expect("reselect current session");
    assert_eq!(
        loads.try_recv(),
        Err(TryRecvError::Empty),
        "reselecting the retained session must not issue a second body load"
    );
}

#[test]
fn default_product_model_has_no_sample_document() {
    let (_profile, repository) = repository();
    let model = AppModel::open(repository).expect("open model");
    assert!(model.projections().is_empty());
    assert_eq!(model.active_session_note_id(), None);
}

#[test]
fn selected_session_retains_the_loaded_note_not_just_its_id() {
    let (_profile, repository) = repository();
    let stored = repository
        .create_note(CreateNote {
            title: "完整会话".into(),
            notebook_id: None,
            document: CanonicalDocument::from_blocks(vec![Block::Paragraph {
                style: BlockStyle::default(),
                inlines: vec![Inline::Text {
                    text: "来自 canonical HTML 的正文".into(),
                    marks: Default::default(),
                }],
            }]),
        })
        .expect("create rich note");
    let id = stored.id.clone();
    let mut model = AppModel::open(repository).expect("open model");

    model
        .dispatch(AppAction::SelectNote(id.clone()))
        .expect("select");

    let note = model.active_note().expect("loaded note retained");
    assert_eq!(note.id, id);
    assert_eq!(note.title, "完整会话");
    assert_eq!(note.body_html, stored.body_html);
    assert_eq!(note.body_text, "来自 canonical HTML 的正文");
}

#[test]
fn active_resource_commit_clears_a_removed_thumbnail_without_waiting_for_event_refresh() {
    // This starts with an image cover, then commits an attachment-only
    // snapshot. The SQLite transaction is the canonical same-frame outcome:
    // deleting the unconditional projection assignment in
    // `apply_active_resource_commit` leaves the old cover visible until an
    // unrelated LibraryEvent happens to refresh the list.
    let (_profile, repository) = repository();
    let old_cover = repository
        .import_resource(
            &crate::native_editor::images::ClipboardPayload::fixture_with_png_and_text("")
                .images
                .into_iter()
                .next()
                .expect("PNG fixture")
                .bytes,
            "old-cover.png",
            "image/png",
            "png",
        )
        .expect("persist old cover");
    let note = repository
        .create_note(CreateNote {
            title: "cover removed".into(),
            notebook_id: None,
            document: CanonicalDocument::from_blocks(vec![Block::Image {
                resource_id: old_cover.clone(),
                alt: "old cover".into(),
                presentation: Default::default(),
            }]),
        })
        .expect("create covered note");
    let mut model = AppModel::open(Arc::clone(&repository)).expect("open model");
    model
        .dispatch(AppAction::SelectNote(note.id.clone()))
        .expect("select covered note");
    assert_eq!(
        model
            .projections()
            .iter()
            .find(|projection| projection.id == note.id)
            .expect("active card")
            .selected_thumbnail_id,
        Some(old_cover),
        "the fixture must begin with the prior image cover"
    );

    let staged = repository
        .stage_resource(
            b"%PDF-1.7\nattachment-only snapshot\n%%EOF\n",
            "proof.pdf",
            "application/pdf",
            "pdf",
        )
        .expect("stage attachment");
    let attachment = staged.resource_id().clone();
    let committed = repository
        .commit_staged_resource_snapshot(
            SaveNote {
                id: note.id.clone(),
                expected_revision: note.revision,
                title: note.title.clone(),
                document: CanonicalDocument::from_blocks(vec![Block::Attachment {
                    resource_id: attachment.clone(),
                    filename: "proof.pdf".into(),
                    media_type: "application/pdf".into(),
                }]),
                resource_ids: vec![attachment],
                selected_thumbnail_id: None,
            },
            None,
            &staged,
        )
        .expect("one atomic attachment snapshot");
    assert!(committed.selected_thumbnail_id.is_none());

    model.apply_active_resource_commit(committed.note, committed.selected_thumbnail_id);

    assert!(
        model
            .projections()
            .iter()
            .find(|projection| projection.id == note.id)
            .expect("active card after commit")
            .selected_thumbnail_id
            .is_none(),
        "the current card must clear its old cover before any later event polling"
    );
}

#[test]
fn sort_and_view_actions_keep_selection_note_id_based_and_clear_old_errors() {
    let (_profile, repository) = repository();
    let zulu = create(&repository, "Zulu");
    let alpha = create(&repository, "Alpha");
    let mut model = AppModel::open(repository).expect("open model");
    model
        .dispatch(AppAction::SelectNote(zulu.clone()))
        .expect("select");
    assert!(
        model
            .dispatch(AppAction::SelectNote(
                NoteId::parse("ffffffffffffffffffffffffffffffff").unwrap()
            ))
            .is_err()
    );
    assert!(matches!(model.status(), AppStatus::Error(_)));

    model
        .dispatch(AppAction::SetSort(NoteSort::TitleAscending))
        .expect("sort");
    model
        .dispatch(AppAction::SetListViewMode(ListViewMode::Compact))
        .expect("view mode");

    assert_eq!(model.status(), &AppStatus::Ready);
    assert_eq!(model.navigation().selected_note_id(), Some(&zulu));
    assert_eq!(model.projections()[0].id, alpha);
    assert_eq!(model.list_view_mode(), ListViewMode::Compact);
    assert_eq!(model.sort(), NoteSort::TitleAscending);
}

#[test]
fn trash_selected_action_clears_selection_and_removes_stale_restart_setting() {
    let (_profile, repository) = repository();
    let only = create(&repository, "唯一笔记");
    let mut model = AppModel::open(Arc::clone(&repository)).expect("open model");
    model.dispatch(AppAction::SelectNote(only)).expect("select");

    model
        .dispatch(AppAction::TrashSelected)
        .expect("trash selected");
    assert_eq!(model.navigation().selected_note_id(), None);
    drop(model);

    assert_eq!(
        AppModel::open(repository)
            .expect("restart")
            .navigation()
            .selected_note_id(),
        None
    );
}

#[test]
fn committed_create_failure_is_truthful_and_never_creates_a_ghost_selection() {
    let (_profile, repository) = repository();
    let mut model = AppModel::open(Arc::clone(&repository)).expect("open model");
    model.fail_next_refresh_for_test(app_lite_core::LibraryError::NotFound);

    assert!(model.dispatch(AppAction::CreateNote).is_err());
    assert_eq!(
        repository
            .list_notes(Default::default())
            .expect("read committed create")
            .len(),
        1,
        "the repository create committed before refresh failed"
    );
    assert_eq!(model.navigation().selected_note_id(), None);
    assert!(matches!(
        model.status(),
        AppStatus::Error(message) if message.contains("笔记已创建")
            && message.contains("资料库数据已提交")
    ));
}

#[test]
fn queued_create_reconciliation_reloads_the_latest_durable_note_after_an_external_revision() {
    // The create transaction returned a complete rev-1 Note, but its first
    // candidate failed. A later repository writer may advance that same note
    // before the queued NoteCreated event retries. Recovery must not pair a
    // rev-2 card with the stale rev-1 active session cached by the create
    // transaction.
    let (_profile, repository) = repository();
    let mut model = AppModel::open(Arc::clone(&repository)).expect("open model");
    model.fail_next_refresh_for_test(app_lite_core::LibraryError::NotFound);
    assert!(model.dispatch(AppAction::CreateNote).is_err());

    let created = repository
        .list_notes(Default::default())
        .expect("read committed create")
        .into_iter()
        .next()
        .expect("one committed note")
        .id;
    let rev_one = repository
        .load_note(&created)
        .expect("load created note")
        .expect("created note survives");
    let latest = repository
        .save_note(SaveNote {
            id: rev_one.id.clone(),
            expected_revision: rev_one.revision,
            title: "external revision two".into(),
            document: CanonicalDocument::parse_html(&rev_one.body_html)
                .expect("created body remains canonical"),
            resource_ids: rev_one.resource_ids.clone(),
            selected_thumbnail_id: None,
        })
        .expect("external writer advances the durable note");
    assert!(latest.revision > rev_one.revision);

    repository.fail_next_note_load_for_test(app_lite_core::LibraryError::NotFound);
    assert!(
        model
            .refresh_projection_events([LibraryEvent::NoteCreated(created.clone())])
            .is_err(),
        "a changed durable revision needs a fallible latest-note load"
    );
    assert!(model.reconciliation_pending());
    assert!(model.active_note().is_none());
    assert!(matches!(
        model.status(),
        AppStatus::Error(message) if message.contains("笔记已创建")
            && message.contains("资料库数据已提交")
    ));

    model
        .refresh_projection_events([LibraryEvent::NoteCreated(created.clone())])
        .expect("queued create event reconciles against durable metadata");

    assert_eq!(model.navigation().route(), &LibraryRoute::AllNotes);
    assert_eq!(model.navigation().selected_note_id(), Some(&created));
    assert_eq!(model.active_note(), Some(&latest));
    assert_eq!(model.status(), &AppStatus::Ready);
    assert!(
        !model.reconciliation_pending(),
        "only a full latest-note candidate may clear the committed-create warning"
    );
}

#[test]
fn queued_create_reconciliation_does_not_mount_an_externally_trashed_note_as_editable() {
    // The typed create target can race a different repository writer that
    // trashes the newly-created note before the queued NoteCreated recovery.
    // A permanent pending warning is not a valid recovery state, and neither
    // is mounting the deleted full Note under All Notes as an editable session.
    let (_profile, repository) = repository();
    let mut model = AppModel::open(Arc::clone(&repository)).expect("open model");
    model.fail_next_refresh_for_test(app_lite_core::LibraryError::NotFound);
    assert!(model.dispatch(AppAction::CreateNote).is_err());
    let created = repository
        .list_notes(Default::default())
        .expect("read committed create")
        .into_iter()
        .next()
        .expect("one committed note")
        .id;

    repository
        .trash_note(&created)
        .expect("external writer trashes the pending create");
    model
        .refresh_projection_events([LibraryEvent::NoteCreated(created.clone())])
        .expect("the queued event deterministically reconciles the trashed target");

    assert_eq!(model.navigation().route(), &LibraryRoute::AllNotes);
    assert_eq!(model.navigation().selected_note_id(), None);
    assert!(model.active_note().is_none());
    assert!(model.projections().iter().all(|row| row.id != created));
    assert_eq!(model.status(), &AppStatus::Ready);
    assert!(!model.reconciliation_pending());
}

#[test]
fn queued_create_reconciliation_recovers_after_an_external_purge_without_a_stuck_lock() {
    // A purge removes the metadata row altogether. Recovery must not infer
    // that a missing row means the create transaction never committed and
    // leave the shell frozen forever waiting for an event that may never
    // recur.
    let (_profile, repository) = repository();
    let mut model = AppModel::open(Arc::clone(&repository)).expect("open model");
    model.fail_next_refresh_for_test(app_lite_core::LibraryError::NotFound);
    assert!(model.dispatch(AppAction::CreateNote).is_err());
    let created = repository
        .list_notes(Default::default())
        .expect("read committed create")
        .into_iter()
        .next()
        .expect("one committed note")
        .id;

    repository.trash_note(&created).expect("trash before purge");
    repository
        .purge_note(&created)
        .expect("external writer purges pending create");
    model
        .refresh_projection_events([LibraryEvent::NoteTrashed(created)])
        .expect("the queued event releases the obsolete typed recovery target");

    assert_eq!(model.navigation().route(), &LibraryRoute::AllNotes);
    assert_eq!(model.navigation().selected_note_id(), None);
    assert!(model.active_note().is_none());
    assert_eq!(model.status(), &AppStatus::Ready);
    assert!(!model.reconciliation_pending());
}

#[test]
fn committed_trash_failure_is_truthful_and_leaves_recovery_possible() {
    let (_profile, repository) = repository();
    let note = create(&repository, "待删除");
    let mut model = AppModel::open(Arc::clone(&repository)).expect("open model");
    model
        .dispatch(AppAction::SelectNote(note.clone()))
        .expect("select note");
    model.fail_next_refresh_for_test(app_lite_core::LibraryError::NotFound);

    assert!(model.dispatch(AppAction::TrashSelected).is_err());
    assert!(
        repository
            .list_notes(Default::default())
            .expect("read committed trash")
            .is_empty(),
        "the repository trash committed before refresh failed"
    );
    assert!(matches!(
        model.status(),
        AppStatus::Error(message) if message.contains("移至废纸篓")
            && message.contains("资料库数据已提交")
    ));
}

#[test]
fn committed_trash_successor_hydration_failure_keeps_the_entire_old_packet_coherent() {
    // Trash commits before a deterministic adjacent-card selection can load
    // its full Note.  Publishing the refreshed list first leaves active A
    // selected even though A is no longer a card; the recovery lock then
    // merely hides that incoherence.  The candidate must hydrate B and
    // persist its selection before changing any live route/list/session
    // field.
    let (_profile, repository) = repository();
    let active = create(&repository, "待删除 A");
    let successor = create(&repository, "相邻 B");
    let mut model = AppModel::open(Arc::clone(&repository)).expect("open model");
    model.set_projection_for_test(vec![active.clone(), successor.clone()]);
    model
        .dispatch(AppAction::SelectNote(active.clone()))
        .expect("select active A");
    let before = navigation_commit_probe(&model);

    repository.fail_next_note_load_for_test(app_lite_core::LibraryError::NotFound);
    assert!(model.dispatch(AppAction::TrashSelected).is_err());

    assert_navigation_commit_unchanged(&model, &before);
    assert!(model.reconciliation_pending());
    assert!(matches!(
        model.status(),
        AppStatus::Error(message) if message.contains("笔记已移至废纸篓")
            && message.contains("资料库数据已提交")
    ));
    assert!(
        repository
            .load_note(&active)
            .expect("read committed trash")
            .expect("soft-deleted row remains")
            .deleted_time
            .is_some(),
        "the truthful warning must not disguise the committed Trash mutation"
    );
}

#[test]
fn committed_sole_note_trash_shell_persist_failure_keeps_the_old_packet_coherent() {
    // The no-successor branch changes selection to None, so it is the easy
    // place to accidentally clear active_session before the shell-state
    // write fails.  Candidate/persist/commit must leave the old packet whole
    // until the queued NoteTrashed recovery can reconcile it.
    let (_profile, repository) = repository();
    let only = create(&repository, "唯一待删除笔记");
    let mut model = AppModel::open(Arc::clone(&repository)).expect("open model");
    model
        .dispatch(AppAction::SelectNote(only.clone()))
        .expect("select only note");
    let before = navigation_commit_probe(&model);

    model.fail_next_shell_state_persist_for_test(app_lite_core::LibraryError::NotFound);
    assert!(model.dispatch(AppAction::TrashSelected).is_err());

    assert_navigation_commit_unchanged(&model, &before);
    assert!(model.reconciliation_pending());
    assert!(matches!(
        model.status(),
        AppStatus::Error(message) if message.contains("笔记已移至废纸篓")
            && message.contains("资料库数据已提交")
    ));
    assert!(
        repository
            .load_note(&only)
            .expect("read committed trash")
            .expect("soft-deleted row remains")
            .deleted_time
            .is_some()
    );
}

#[test]
fn navigate_to_refresh_failure_keeps_the_entire_live_navigation_commit() {
    // Mutation-sensitive: committing the typed route/history before the
    // candidate query returns produces a Trash/notebook route paired with the
    // old cards and editor session.
    let (_profile, repository, notebook, all_note, notebook_note) = navigation_fixture();
    let mut model = AppModel::open(repository).expect("open model");
    model
        .dispatch(AppAction::SelectNote(all_note))
        .expect("select All Notes session");
    let before = navigation_commit_probe(&model);

    model.fail_next_refresh_for_test(app_lite_core::LibraryError::NotFound);
    assert!(
        model
            .dispatch(AppAction::NavigateTo {
                route: LibraryRoute::Notebook(notebook.id),
                selected_note_id: Some(notebook_note),
            })
            .is_err()
    );

    assert_navigation_commit_unchanged(&model, &before);
}

#[test]
fn navigation_commit_keeps_membership_authority_when_sidebar_requests_the_current_note() {
    // The sidebar may carry its current typed NoteId, but it never decides
    // membership itself.  The repository-backed candidate must retain a
    // matching note, clear an excluded note, and preserve the resulting
    // snapshots for native Back/Forward.
    let (_profile, repository) = repository();
    let source = repository
        .create_notebook("原笔记本", None)
        .expect("create source notebook");
    let destination = repository
        .create_notebook("目标笔记本", None)
        .expect("create destination notebook");
    let selected = repository
        .create_note(CreateNote {
            title: "当前笔记".into(),
            notebook_id: Some(source.id),
            document: CanonicalDocument::default(),
        })
        .expect("create selected note")
        .id;
    let target = repository
        .create_note(CreateNote {
            title: "目标笔记".into(),
            notebook_id: Some(destination.id.clone()),
            document: CanonicalDocument::default(),
        })
        .expect("create target note")
        .id;
    let mut model = AppModel::open(repository).expect("open model");
    model
        .dispatch(AppAction::SelectNote(selected.clone()))
        .expect("select current note");

    model
        .dispatch(AppAction::NavigateTo {
            route: LibraryRoute::Notebook(destination.id.clone()),
            selected_note_id: Some(selected.clone()),
        })
        .expect("navigate with the sidebar's current selection request");
    assert_eq!(model.navigation().selected_note_id(), None);
    assert_eq!(model.active_session_note_id(), None);
    assert_eq!(
        model
            .projections()
            .iter()
            .map(|projection| projection.id.clone())
            .collect::<Vec<_>>(),
        vec![target],
        "the destination projection, not the sidebar label, determines membership"
    );
    let history_len = model.navigation().history_len_for_test();

    model
        .dispatch(AppAction::NavigateBack)
        .expect("Back restores the prior selected snapshot");
    assert_eq!(model.navigation().route(), &LibraryRoute::AllNotes);
    assert_eq!(model.navigation().selected_note_id(), Some(&selected));
    assert_eq!(model.active_session_note_id(), Some(&selected));
    model
        .dispatch(AppAction::NavigateForward)
        .expect("Forward restores the excluded destination snapshot");
    assert_eq!(
        model.navigation().route(),
        &LibraryRoute::Notebook(destination.id)
    );
    assert_eq!(model.navigation().selected_note_id(), None);
    assert_eq!(model.navigation().history_len_for_test(), history_len);
}

#[test]
fn sidebar_selection_preflight_uses_typed_organization_metadata_without_note_bodies() {
    let (_profile, repository) = repository();
    let stack = repository.create_stack("项目组").expect("create stack");
    let notebook = repository
        .create_notebook("项目笔记本", Some(&stack.id))
        .expect("create notebook");
    let tag = repository.create_tag("验收").expect("create tag");
    let selected = repository
        .create_note(CreateNote {
            title: "当前笔记".into(),
            notebook_id: Some(notebook.id.clone()),
            document: CanonicalDocument::default(),
        })
        .expect("create selected note")
        .id;
    repository
        .set_note_tags(&selected, &[tag.id.clone()])
        .expect("tag selected note");
    let mut model = AppModel::open(Arc::clone(&repository)).expect("open model");
    model
        .dispatch(AppAction::SelectNote(selected.clone()))
        .expect("select note");

    let observed = repository.observe_next_note_organization_state_query();
    assert_eq!(
        model
            .sidebar_selected_note_for_route(&LibraryRoute::Notebook(notebook.id.clone()))
            .expect("Notebook preflight"),
        Some(selected.clone())
    );
    let fields = observed.recv().expect("organization metadata observation");
    assert!(
        !fields.iter().any(|field| {
            matches!(
                field.as_str(),
                "notes.body_html"
                    | "notes.body_text"
                    | "notes.merge_state"
                    | "resource_blobs.bytes"
            )
        }),
        "sidebar lifecycle preflight must not hydrate a canonical body or blob: {fields:?}"
    );
    let all_notes_index_read = repository.observe_next_navigation_index_query();
    assert_eq!(
        model
            .sidebar_selected_note_for_route(&LibraryRoute::AllNotes)
            .expect("All Notes preflight"),
        Some(selected.clone())
    );
    assert_eq!(
        all_notes_index_read.try_recv(),
        Err(TryRecvError::Empty),
        "All Notes membership is note-local and must not duplicate a full sidebar-index query"
    );
    assert_eq!(
        model
            .sidebar_selected_note_for_route(&LibraryRoute::Stack(stack.id))
            .expect("Stack preflight"),
        Some(selected.clone())
    );
    assert_eq!(
        model
            .sidebar_selected_note_for_route(
                &LibraryRoute::tags(vec![tag.id]).expect("single tag route"),
            )
            .expect("Tag preflight"),
        Some(selected.clone())
    );
    let trash_index_read = repository.observe_next_navigation_index_query();
    assert_eq!(
        model
            .sidebar_selected_note_for_route(&LibraryRoute::Trash)
            .expect("Trash preflight"),
        None
    );
    assert_eq!(
        trash_index_read.try_recv(),
        Err(TryRecvError::Empty),
        "Trash membership is note-local and must not duplicate a full sidebar-index query"
    );
}

#[test]
fn excluded_sidebar_navigation_shell_state_failure_keeps_the_live_packet_unchanged() {
    let (_profile, repository) = repository();
    let source = repository
        .create_notebook("原笔记本", None)
        .expect("create source notebook");
    let destination = repository
        .create_notebook("排除目标", None)
        .expect("create destination notebook");
    let selected = repository
        .create_note(CreateNote {
            title: "当前笔记".into(),
            notebook_id: Some(source.id),
            document: CanonicalDocument::default(),
        })
        .expect("create selected note")
        .id;
    let mut model = AppModel::open(repository).expect("open model");
    model
        .dispatch(AppAction::SelectNote(selected.clone()))
        .expect("select current note");
    let before = navigation_commit_probe(&model);

    model.fail_next_shell_state_persist_for_test(app_lite_core::LibraryError::NotFound);
    assert!(
        model
            .dispatch(AppAction::NavigateTo {
                route: LibraryRoute::Notebook(destination.id),
                selected_note_id: Some(selected),
            })
            .is_err()
    );

    assert_navigation_commit_unchanged(&model, &before);
}

#[test]
fn back_refresh_failure_keeps_the_entire_live_navigation_commit() {
    // Mutation-sensitive: moving the history cursor before the candidate
    // query makes a failed Back action silently abandon the selected notebook
    // session.
    let (_profile, repository, notebook, all_note, notebook_note) = navigation_fixture();
    let mut model = AppModel::open(repository).expect("open model");
    model
        .dispatch(AppAction::SelectNote(all_note))
        .expect("select All Notes session");
    model
        .dispatch(AppAction::NavigateTo {
            route: LibraryRoute::Notebook(notebook.id),
            selected_note_id: Some(notebook_note),
        })
        .expect("navigate notebook");
    let before = navigation_commit_probe(&model);

    model.fail_next_refresh_for_test(app_lite_core::LibraryError::NotFound);
    assert!(model.dispatch(AppAction::NavigateBack).is_err());

    assert_navigation_commit_unchanged(&model, &before);
}

#[test]
fn forward_refresh_failure_keeps_the_entire_live_navigation_commit() {
    // Mutation-sensitive: the Forward cursor/route may change only after its
    // target query and selected note preparation both succeed.
    let (_profile, repository, notebook, all_note, notebook_note) = navigation_fixture();
    let mut model = AppModel::open(repository).expect("open model");
    model
        .dispatch(AppAction::SelectNote(all_note.clone()))
        .expect("select All Notes session");
    model
        .dispatch(AppAction::NavigateTo {
            route: LibraryRoute::Notebook(notebook.id),
            selected_note_id: Some(notebook_note),
        })
        .expect("navigate notebook");
    model
        .dispatch(AppAction::NavigateBack)
        .expect("back to All Notes");
    assert_eq!(model.navigation().selected_note_id(), Some(&all_note));
    let before = navigation_commit_probe(&model);

    model.fail_next_refresh_for_test(app_lite_core::LibraryError::NotFound);
    assert!(model.dispatch(AppAction::NavigateForward).is_err());

    assert_navigation_commit_unchanged(&model, &before);
}

#[test]
fn sort_refresh_failure_keeps_the_entire_live_navigation_commit() {
    // Mutation-sensitive: changing route-scoped sort before the query returns
    // would leave the sort label semantically ahead of the old card order.
    let (_profile, repository, _notebook, all_note, _notebook_note) = navigation_fixture();
    let mut model = AppModel::open(repository).expect("open model");
    model
        .dispatch(AppAction::SelectNote(all_note))
        .expect("select All Notes session");
    let before = navigation_commit_probe(&model);

    model.fail_next_refresh_for_test(app_lite_core::LibraryError::NotFound);
    assert!(
        model
            .dispatch(AppAction::SetSort(NoteSort::TitleAscending))
            .is_err()
    );

    assert_navigation_commit_unchanged(&model, &before);
}

#[test]
fn target_hydration_failure_keeps_the_entire_live_navigation_commit() {
    // Mutation-sensitive: a candidate projection is not enough. Its selected
    // note must hydrate before the model publishes the target route/session.
    let (_profile, repository, notebook, all_note, notebook_note) = navigation_fixture();
    let mut model = AppModel::open(Arc::clone(&repository)).expect("open model");
    model
        .dispatch(AppAction::SelectNote(all_note))
        .expect("select All Notes session");
    let before = navigation_commit_probe(&model);

    repository.fail_next_note_load_for_test(app_lite_core::LibraryError::NotFound);
    assert!(
        model
            .dispatch(AppAction::NavigateTo {
                route: LibraryRoute::Notebook(notebook.id),
                selected_note_id: Some(notebook_note),
            })
            .is_err()
    );

    assert_navigation_commit_unchanged(&model, &before);
}

#[test]
fn typed_route_history_keeps_route_scoped_sorts_and_never_records_projection_refreshes() {
    // Mutation-sensitive: replacing typed snapshots with a row index/global
    // sort, appending history from refresh_projection_events, or failing to
    // truncate Forward after a new navigation changes these assertions.
    let (_profile, repository) = repository();
    let notebook = repository
        .create_notebook("项目", None)
        .expect("create notebook");
    let tag = repository.create_tag("紧急").expect("create tag");
    let notebook_note = repository
        .create_note(CreateNote {
            title: "项目笔记".into(),
            notebook_id: Some(notebook.id.clone()),
            document: CanonicalDocument::default(),
        })
        .expect("create notebook note");
    let tagged_note = create(&repository, "标签笔记");
    repository
        .set_note_tags(&tagged_note, &[tag.id.clone()])
        .expect("set tag");

    let mut model = AppModel::open(Arc::clone(&repository)).expect("open model");
    model
        .dispatch(AppAction::NavigateTo {
            route: LibraryRoute::Notebook(notebook.id.clone()),
            selected_note_id: Some(notebook_note.id.clone()),
        })
        .expect("navigate notebook");
    model
        .dispatch(AppAction::SetSort(NoteSort::TitleAscending))
        .expect("set notebook sort");
    let notebook_snapshot = model.navigation().snapshot();
    assert_eq!(notebook_snapshot.route, LibraryRoute::Notebook(notebook.id));
    assert_eq!(notebook_snapshot.selected_note_id, Some(notebook_note.id));

    model
        .dispatch(AppAction::NavigateTo {
            route: LibraryRoute::tags(vec![tag.id]).expect("tag route"),
            selected_note_id: Some(tagged_note.clone()),
        })
        .expect("navigate tag");
    let tag_snapshot = model.navigation().snapshot();
    let tag_sort = model.sort();
    assert!(model.navigation().can_navigate_back());
    assert!(!model.navigation().can_navigate_forward());

    model
        .dispatch(AppAction::NavigateBack)
        .expect("back to notebook");
    assert_eq!(model.navigation().snapshot(), notebook_snapshot);
    assert_eq!(model.sort(), NoteSort::TitleAscending);
    assert!(model.navigation().can_navigate_forward());
    let history_len = model.navigation().history_len_for_test();
    model
        .refresh_projection_events([LibraryEvent::OrganizationChanged])
        .expect("projection refresh");
    assert_eq!(
        model.navigation().history_len_for_test(),
        history_len,
        "a repository projection refresh must not become browser-style navigation"
    );
    assert!(
        model.navigation().can_navigate_forward(),
        "refresh must preserve the existing Forward branch"
    );

    model
        .dispatch(AppAction::NavigateForward)
        .expect("Forward restores the typed tag snapshot");
    assert_eq!(model.navigation().snapshot(), tag_snapshot);
    assert_eq!(model.navigation().selected_note_id(), Some(&tagged_note));
    assert_eq!(model.active_session_note_id(), Some(&tagged_note));
    assert_eq!(
        model
            .projections()
            .iter()
            .map(|projection| projection.id.clone())
            .collect::<Vec<_>>(),
        vec![tagged_note.clone()],
        "Forward must restore the target route projection, not merely a history flag"
    );
    assert_eq!(model.sort(), tag_sort);

    model
        .dispatch(AppAction::NavigateBack)
        .expect("Back before branching");
    assert_eq!(model.navigation().snapshot(), notebook_snapshot);

    model
        .dispatch(AppAction::NavigateTo {
            route: LibraryRoute::Trash,
            selected_note_id: None,
        })
        .expect("new navigation truncates forward");
    assert!(!model.navigation().can_navigate_forward());
    assert_eq!(model.sort(), NoteSort::DeletedDescending);
    model
        .dispatch(AppAction::NavigateBack)
        .expect("back restores notebook route state");
    assert_eq!(model.navigation().snapshot(), notebook_snapshot);
    assert_eq!(model.sort(), NoteSort::TitleAscending);
}

#[test]
fn organization_actions_commit_route_projection_selection_and_session_together() {
    // Mutation-sensitive: a moved selected note no longer belongs in its
    // current notebook route. Publishing only the new navigation index or
    // only the card projection would leave the retained session mounted on a
    // visibly absent/wrong row.
    let (_profile, repository) = repository();
    let work = repository
        .create_notebook("工作", None)
        .expect("create work notebook");
    let tag = repository.create_tag("紧急").expect("create tag");
    let note = repository
        .create_note(CreateNote {
            title: "当前笔记".into(),
            notebook_id: Some(work.id.clone()),
            document: CanonicalDocument::default(),
        })
        .expect("create scoped note");
    repository
        .add_note_tag(&note.id, &tag.id)
        .expect("tag current note");

    let mut model = AppModel::open(Arc::clone(&repository)).expect("open model");
    model
        .dispatch(AppAction::NavigateTo {
            route: LibraryRoute::Notebook(work.id.clone()),
            selected_note_id: Some(note.id.clone()),
        })
        .expect("navigate notebook");
    assert_eq!(model.active_session_note_id(), Some(&note.id));

    model
        .dispatch(AppAction::MoveSelectedNote(
            repository.default_notebook().expect("default notebook").id,
        ))
        .expect("move selected note");
    assert_eq!(model.navigation().route(), &LibraryRoute::Notebook(work.id));
    assert!(model.projections().is_empty());
    assert_eq!(model.navigation().selected_note_id(), None);
    assert_eq!(model.active_session_note_id(), None);

    model
        .dispatch(AppAction::NavigateTo {
            route: LibraryRoute::tags(vec![tag.id.clone()]).expect("typed tag route"),
            selected_note_id: Some(note.id.clone()),
        })
        .expect("navigate tag");
    model
        .dispatch(AppAction::DeleteTag(tag.id.clone()))
        .expect("delete active tag");
    assert_eq!(model.navigation().route(), &LibraryRoute::AllNotes);
    assert_eq!(model.navigation().selected_note_id(), Some(&note.id));
    assert_eq!(model.active_session_note_id(), Some(&note.id));
    assert!(model.navigation_index().tags.is_empty());
}

#[test]
fn destroying_the_active_stack_falls_back_without_losing_the_selected_note() {
    // Destroying a Stack is not deleting its child notebooks or notes. When
    // the current typed Stack route disappears, the candidate commit must
    // move only the route to All Notes while retaining the selected durable
    // NoteId and the session that already owns that same note.
    let (_profile, repository) = repository();
    let stack = repository.create_stack("当前组").expect("create stack");
    let notebook = repository
        .create_notebook("当前笔记本", Some(&stack.id))
        .expect("create child notebook");
    let note = repository
        .create_note(CreateNote {
            title: "当前笔记".into(),
            notebook_id: Some(notebook.id.clone()),
            document: CanonicalDocument::default(),
        })
        .expect("create note");
    let mut model = AppModel::open(Arc::clone(&repository)).expect("open model");
    model
        .dispatch(AppAction::NavigateTo {
            route: LibraryRoute::Stack(stack.id.clone()),
            selected_note_id: Some(note.id.clone()),
        })
        .expect("navigate stack");

    model
        .dispatch(AppAction::DeleteStack(stack.id.clone()))
        .expect("destroy active stack");
    assert_eq!(model.navigation().route(), &LibraryRoute::AllNotes);
    assert_eq!(model.navigation().selected_note_id(), Some(&note.id));
    assert_eq!(model.active_session_note_id(), Some(&note.id));
    assert!(
        model
            .navigation_index()
            .stacks
            .iter()
            .all(|candidate| candidate.id != stack.id)
    );
    assert_eq!(
        model
            .navigation_index()
            .notebooks
            .iter()
            .find(|candidate| candidate.id == notebook.id)
            .expect("child notebook remains")
            .stack_id,
        None
    );
    assert_eq!(
        repository
            .load_note(&note.id)
            .expect("load note")
            .expect("note remains")
            .notebook_id,
        notebook.id
    );
}

#[test]
fn destructive_organization_commits_never_leave_tombstoned_routes_in_back_or_forward_history() {
    // Stable route IDs are correct only while their navigation entities still
    // exist. A delete/disband must sanitize *every* saved history snapshot,
    // not just replace the route painted in the current frame.
    let (_profile, repository) = repository();
    let notebook = repository
        .create_notebook("将删除的笔记本", None)
        .expect("create notebook");
    let tag = repository.create_tag("将删除的标签").expect("create tag");
    let stack = repository.create_stack("将解散的组").expect("create stack");
    let stacked_notebook = repository
        .create_notebook("组内笔记本", Some(&stack.id))
        .expect("create stacked notebook");
    let note = repository
        .create_note(CreateNote {
            title: "历史选择".into(),
            notebook_id: Some(notebook.id.clone()),
            document: CanonicalDocument::default(),
        })
        .expect("create note")
        .id;
    repository
        .set_note_tags(&note, &[tag.id.clone()])
        .expect("set tag");

    let mut model = AppModel::open(Arc::clone(&repository)).expect("open notebook history");
    for route in [
        LibraryRoute::Notebook(notebook.id.clone()),
        LibraryRoute::tags(vec![tag.id.clone()]).expect("tag route"),
        LibraryRoute::Notebook(notebook.id.clone()),
    ] {
        model
            .dispatch(AppAction::NavigateTo {
                route,
                selected_note_id: Some(note.clone()),
            })
            .expect("navigate history fixture");
    }
    model
        .dispatch(AppAction::DeleteNotebook(notebook.id.clone()))
        .expect("delete notebook");
    while model.navigation().can_navigate_back() {
        model.dispatch(AppAction::NavigateBack).expect("back");
        assert_ne!(
            model.navigation().route(),
            &LibraryRoute::Notebook(notebook.id.clone()),
            "Back cannot revive a deleted notebook route"
        );
    }
    while model.navigation().can_navigate_forward() {
        model.dispatch(AppAction::NavigateForward).expect("forward");
        assert_ne!(
            model.navigation().route(),
            &LibraryRoute::Notebook(notebook.id.clone()),
            "Forward cannot revive a deleted notebook route"
        );
    }

    let mut tag_model = AppModel::open(Arc::clone(&repository)).expect("open tag history");
    let tag_route = LibraryRoute::tags(vec![tag.id.clone()]).expect("tag route");
    for route in [tag_route.clone(), LibraryRoute::AllNotes, tag_route] {
        tag_model
            .dispatch(AppAction::NavigateTo {
                route,
                selected_note_id: Some(note.clone()),
            })
            .expect("navigate tag history fixture");
    }
    tag_model
        .dispatch(AppAction::DeleteTag(tag.id.clone()))
        .expect("delete tag");
    while tag_model.navigation().can_navigate_back() {
        tag_model.dispatch(AppAction::NavigateBack).expect("back");
        assert!(
            !matches!(tag_model.navigation().route(), LibraryRoute::Tags(ids) if ids.contains(&tag.id)),
            "Back cannot revive a deleted tag route"
        );
    }
    while tag_model.navigation().can_navigate_forward() {
        tag_model
            .dispatch(AppAction::NavigateForward)
            .expect("forward");
        assert!(
            !matches!(tag_model.navigation().route(), LibraryRoute::Tags(ids) if ids.contains(&tag.id)),
            "Forward cannot revive a deleted tag route"
        );
    }

    let mut stack_model = AppModel::open(Arc::clone(&repository)).expect("open stack history");
    for route in [
        LibraryRoute::Stack(stack.id.clone()),
        LibraryRoute::AllNotes,
        LibraryRoute::Stack(stack.id.clone()),
    ] {
        stack_model
            .dispatch(AppAction::NavigateTo {
                route,
                selected_note_id: None,
            })
            .expect("navigate stack history fixture");
    }
    stack_model
        .dispatch(AppAction::DeleteStack(stack.id.clone()))
        .expect("disband stack");
    assert_eq!(
        repository
            .list_navigation_index()
            .expect("index after disband")
            .notebooks
            .iter()
            .find(|candidate| candidate.id == stacked_notebook.id)
            .expect("child notebook retained")
            .stack_id,
        None
    );
    while stack_model.navigation().can_navigate_back() {
        stack_model.dispatch(AppAction::NavigateBack).expect("back");
        assert_ne!(
            stack_model.navigation().route(),
            &LibraryRoute::Stack(stack.id.clone()),
            "Back cannot revive a disbanded stack route"
        );
    }
    while stack_model.navigation().can_navigate_forward() {
        stack_model
            .dispatch(AppAction::NavigateForward)
            .expect("forward");
        assert_ne!(
            stack_model.navigation().route(),
            &LibraryRoute::Stack(stack.id.clone()),
            "Forward cannot revive a disbanded stack route"
        );
    }
}

#[test]
fn committed_stack_destroy_refresh_failure_keeps_every_live_field_unchanged() {
    // The repository disband can commit before a navigation-index candidate
    // fails. That is a durable fact, but no stale intermediate Stack route,
    // projection, selection, or active session may be partially published.
    let (_profile, repository) = repository();
    let stack = repository.create_stack("待失败解散").expect("create stack");
    let notebook = repository
        .create_notebook("child", Some(&stack.id))
        .expect("create child");
    let note = repository
        .create_note(CreateNote {
            title: "selected".into(),
            notebook_id: Some(notebook.id.clone()),
            document: CanonicalDocument::default(),
        })
        .expect("create selected note");
    let mut model = AppModel::open(Arc::clone(&repository)).expect("open model");
    model
        .dispatch(AppAction::NavigateTo {
            route: LibraryRoute::Stack(stack.id.clone()),
            selected_note_id: Some(note.id.clone()),
        })
        .expect("navigate stack");
    let before = navigation_commit_probe(&model);
    model.fail_next_refresh_for_test(app_lite_core::LibraryError::NotFound);

    assert!(
        model
            .dispatch(AppAction::DeleteStack(stack.id.clone()))
            .is_err()
    );
    assert_navigation_commit_unchanged(&model, &before);
    assert!(
        repository
            .list_navigation_index()
            .expect("committed index")
            .stacks
            .iter()
            .all(|candidate| candidate.id != stack.id)
    );
    assert!(matches!(
        model.status(),
        AppStatus::Error(message) if message.contains("笔记本组已解散")
            && message.contains("资料库数据已提交")
    ));
}

#[test]
fn failed_organization_action_keeps_every_live_navigation_commit_unchanged() {
    // A repository error must not optimistically clear the route/session or
    // publish a sidebar refresh. This catches a reducer that mutates the
    // active model before its target notebook has been validated.
    let (_profile, repository) = repository();
    let selected = create(&repository, "selected");
    let mut model = AppModel::open(repository).expect("open model");
    model
        .dispatch(AppAction::SelectNote(selected.clone()))
        .expect("select note");
    let before = navigation_commit_probe(&model);
    let missing = NotebookId::parse("f".repeat(32)).expect("opaque notebook id");

    assert!(
        model
            .dispatch(AppAction::MoveSelectedNote(missing))
            .is_err()
    );

    assert_navigation_commit_unchanged(&model, &before);
}

#[test]
fn committed_organization_refresh_failure_keeps_the_live_session_coherent() {
    // Mutation-sensitive: the SQLite tag relation commits before the sidebar
    // candidate refresh. If that refresh fails, publishing any part of the
    // candidate would leave the old editor session paired with a new route or
    // projection. The durable fact must be reported truthfully, while every
    // live navigation/session field remains at its prior snapshot.
    let (_profile, repository) = repository();
    let selected = create(&repository, "selected");
    let tag = repository.create_tag("已提交标签").expect("create tag");
    let mut model = AppModel::open(Arc::clone(&repository)).expect("open model");
    model
        .dispatch(AppAction::SelectNote(selected.clone()))
        .expect("select note");
    let before = navigation_commit_probe(&model);

    model.fail_next_refresh_for_test(app_lite_core::LibraryError::NotFound);
    assert!(
        model
            .dispatch(AppAction::AddTagToSelectedNote(tag.id.clone()))
            .is_err()
    );

    assert_navigation_commit_unchanged(&model, &before);
    assert_eq!(
        repository
            .load_note(&selected)
            .expect("read committed note")
            .expect("note remains")
            .tag_ids,
        vec![tag.id],
        "the committed relation must not be disguised as a rollback"
    );
    assert!(matches!(
        model.status(),
        AppStatus::Error(message) if message.contains("笔记标签已更新")
            && message.contains("资料库数据已提交")
    ));
}

#[test]
fn committed_trash_failure_keeps_its_warning_until_note_trashed_event_commits_a_full_candidate() {
    // Mutation-sensitive: Trash commits before the first All Notes candidate
    // can fail. A repeated click on the retained old card may not clear that
    // warning, and the queued NoteTrashed event (not merely
    // OrganizationChanged) must finally replace the entire live packet.
    let (_profile, repository) = repository();
    let selected = create(&repository, "待协调的废纸篓笔记");
    let mut model = AppModel::open(Arc::clone(&repository)).expect("open model");
    model
        .dispatch(AppAction::SelectNote(selected.clone()))
        .expect("select active note");
    let before = navigation_commit_probe(&model);

    model.fail_next_refresh_for_test(app_lite_core::LibraryError::NotFound);
    assert!(model.dispatch(AppAction::TrashSelected).is_err());
    assert_navigation_commit_unchanged(&model, &before);
    assert!(model.reconciliation_pending());
    assert!(matches!(
        model.status(),
        AppStatus::Error(message) if message.contains("资料库数据已提交")
    ));

    model
        .dispatch(AppAction::SelectNote(selected.clone()))
        .expect("same retained card remains a non-recovery action");
    assert!(model.reconciliation_pending());
    assert!(matches!(
        model.status(),
        AppStatus::Error(message) if message.contains("资料库数据已提交")
    ));

    model
        .refresh_projection_events([LibraryEvent::NoteTrashed(selected.clone())])
        .expect("the real trash event prepares the complete recovery candidate");
    assert!(!model.reconciliation_pending());
    assert_eq!(model.active_session_note_id(), None);
    assert_eq!(model.navigation().selected_note_id(), None);
    assert!(
        model
            .projections()
            .iter()
            .all(|projection| projection.id != selected),
        "the coherent All Notes candidate removes the trashed card before Ready"
    );
    assert_eq!(model.status(), &AppStatus::Ready);
}

#[test]
fn active_organization_candidate_uses_metadata_without_rehydrating_the_saved_body() {
    // This exercises AppModel::prepare_organization_commit through its real
    // typed reducer. Keeping the repository helper test alone would not
    // catch this branch being changed back to `load_note`: that would hydrate
    // body_html/body_text and replace the retained session unnecessarily.
    let (_profile, repository) = repository();
    let selected = create(&repository, "已保存正文不应重载");
    let tag = repository.create_tag("只读元数据").expect("create tag");
    let mut model = AppModel::open(Arc::clone(&repository)).expect("open model");
    model
        .dispatch(AppAction::SelectNote(selected.clone()))
        .expect("select active note before installing observers");

    let metadata_reads = repository.observe_next_note_organization_state_query();
    let note_loads = repository.observe_note_loads();
    let resource_reads = repository.observe_resource_reads();

    model
        .dispatch(AppAction::AddTagToSelectedNote(tag.id.clone()))
        .expect("typed active-note organization mutation");

    let reads = metadata_reads
        .try_recv()
        .expect("production candidate must run exactly one metadata query");
    let forbidden = [
        "notes.body_html",
        "notes.body_text",
        "notes.merge_state",
        "resource_blobs.bytes",
    ];
    assert!(
        !reads
            .iter()
            .any(|column| forbidden.contains(&column.as_str())),
        "the active candidate may not hydrate body/blob columns: {reads:?}"
    );
    assert_eq!(
        metadata_reads.try_recv(),
        Err(TryRecvError::Disconnected),
        "the one-shot production metadata probe must be consumed exactly once by the candidate"
    );
    assert_eq!(
        note_loads.try_recv(),
        Err(TryRecvError::Empty),
        "the already-mounted selected note must not call load_note during an organization refresh"
    );
    assert_eq!(
        resource_reads.try_recv(),
        Err(TryRecvError::Empty),
        "metadata reconciliation must not read resource bytes"
    );
    assert_eq!(
        model.active_note().expect("active note").tag_ids,
        vec![tag.id],
        "the retained active session still receives the committed metadata"
    );
}

#[test]
fn organization_event_reconciliation_keeps_partial_warning_until_the_full_candidate_updates_revision()
 {
    // The durable tag mutation may commit before its first candidate read
    // fails. A queued OrganizationChanged must then use the exact same full
    // candidate packet (index, cards, selected metadata/session) rather than
    // publishing only a refreshed card list. Re-clicking the same card is not
    // that recovery packet and may not erase the committed-but-unreconciled
    // warning.
    let (_profile, repository) = repository();
    let selected = create(&repository, "事件恢复中的当前笔记");
    let tag = repository.create_tag("事件标签").expect("create tag");
    let mut model = AppModel::open(Arc::clone(&repository)).expect("open model");
    model
        .dispatch(AppAction::SelectNote(selected.clone()))
        .expect("select active note");
    let before = navigation_commit_probe(&model);
    let original_revision = before.active_note.as_ref().expect("active note").revision;

    model.fail_next_refresh_for_test(app_lite_core::LibraryError::NotFound);
    assert!(
        model
            .dispatch(AppAction::AddTagToSelectedNote(tag.id.clone()))
            .is_err()
    );
    assert_navigation_commit_unchanged(&model, &before);
    assert!(matches!(
        model.status(),
        AppStatus::Error(message) if message.contains("资料库数据已提交")
    ));
    let committed_revision = repository
        .load_note(&selected)
        .expect("load committed note")
        .expect("selected note remains")
        .revision;
    assert!(committed_revision > original_revision);

    // The real event bridge can race another transient candidate failure;
    // that failure must leave every live packet field and the warning intact.
    model.fail_next_refresh_for_test(app_lite_core::LibraryError::NotFound);
    assert!(
        model
            .refresh_projection_events([LibraryEvent::OrganizationChanged])
            .is_err()
    );
    assert_navigation_commit_unchanged(&model, &before);
    assert!(matches!(
        model.status(),
        AppStatus::Error(message) if message.contains("资料库数据已提交")
    ));

    model
        .dispatch(AppAction::SelectNote(selected.clone()))
        .expect("same-card selection remains a harmless navigation request");
    assert_navigation_commit_unchanged(&model, &before);
    assert!(matches!(
        model.status(),
        AppStatus::Error(message) if message.contains("资料库数据已提交")
    ));

    model
        .refresh_projection_events([LibraryEvent::OrganizationChanged])
        .expect("the later complete candidate recovers the event");
    assert_eq!(model.active_session_note_id(), Some(&selected));
    assert_eq!(
        model
            .active_note()
            .expect("reconciled active note")
            .revision,
        committed_revision,
        "the recovery packet must replace stale active metadata before Ready"
    );
    assert_eq!(
        model.active_note().expect("active note").tag_ids,
        vec![tag.id],
        "the active session metadata must match the durable organization revision"
    );
    assert_eq!(model.status(), &AppStatus::Ready);
}

#[test]
fn trash_route_restore_and_purge_are_typed_and_do_not_revive_a_wrong_session() {
    let (_profile, repository) = repository();
    let restored = create(&repository, "restore me");
    let purged = create(&repository, "purge me");
    repository
        .trash_note(&restored)
        .expect("trash restore fixture");
    repository.trash_note(&purged).expect("trash purge fixture");
    let mut model = AppModel::open(Arc::clone(&repository)).expect("open model");

    model
        .dispatch(AppAction::NavigateTo {
            route: LibraryRoute::Trash,
            selected_note_id: Some(restored.clone()),
        })
        .expect("navigate trash");
    model
        .dispatch(AppAction::RestoreNote(restored.clone()))
        .expect("restore note");
    assert_eq!(model.navigation().route(), &LibraryRoute::Trash);
    assert!(model.projections().iter().all(|row| row.id != restored));
    assert_ne!(model.active_session_note_id(), Some(&restored));

    model
        .dispatch(AppAction::SelectNote(purged.clone()))
        .expect("select remaining trash note");
    model
        .dispatch(AppAction::PurgeNote(purged.clone()))
        .expect("permanently delete trashed note");
    assert!(
        repository
            .load_note(&purged)
            .expect("load purged")
            .is_none()
    );
    assert!(model.projections().is_empty());
    assert_eq!(model.active_session_note_id(), None);
}
