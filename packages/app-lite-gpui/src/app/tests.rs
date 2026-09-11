use super::{AppAction, AppModel, AppStatus, ListViewMode, NoteSort, PaneState};
use app_lite_core::document::{Block, BlockStyle, Inline};
use app_lite_core::{
    CanonicalDocument, CreateNote, LibraryEvent, LibraryRepository, LibraryRoute,
    LibraryShellState, Note, NoteId, SaveNote,
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
