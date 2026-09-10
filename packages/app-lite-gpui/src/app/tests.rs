use super::{AppAction, AppModel, PaneState};
use app_lite_core::{CanonicalDocument, CreateNote, LibraryRepository, NoteId};
use std::sync::Arc;

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
fn invalid_saved_selection_falls_back_without_loading_a_body() {
    let (_profile, repository) = repository();
    repository
        .write_setting(
            "library-shell.selected-note-id",
            "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
        )
        .expect("write stale setting");
    let observed = repository.observe_next_list_query();

    let resumed = AppModel::open(Arc::clone(&repository)).expect("resume");

    assert_eq!(resumed.navigation().selected_note_id(), None);
    let fields = observed.recv().expect("list observation");
    assert!(!fields.iter().any(|field| field == "notes.body_html"));
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
    assert!(!fields.iter().any(|field| field == "notes.body_html"));
}

#[test]
fn default_product_model_has_no_sample_document() {
    let (_profile, repository) = repository();
    let model = AppModel::open(repository).expect("open model");
    assert!(model.projections().is_empty());
    assert_eq!(model.active_session_note_id(), None);
}
