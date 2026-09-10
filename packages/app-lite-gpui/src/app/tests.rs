use super::{AppAction, AppModel, AppStatus, ListViewMode, NoteSort, PaneState};
use app_lite_core::document::{Block, BlockStyle, Inline};
use app_lite_core::{CanonicalDocument, CreateNote, LibraryRepository, LibraryShellState, NoteId};
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
fn create_note_reuses_the_repository_hydration_for_its_active_session() {
    // Catches AppModel::create_note selecting the returned ID through a second
    // load_note call after the repository already returned the complete Note.
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
    assert_eq!(loads.try_recv(), Ok(selected));
    assert_eq!(
        loads.try_recv(),
        Err(TryRecvError::Empty),
        "the complete Note returned by create_note must become the active session without a second hydration"
    );
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
