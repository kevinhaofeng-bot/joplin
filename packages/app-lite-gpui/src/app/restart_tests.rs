//! A restart must restore the real typed destination, not just a selected ID.
use super::{AppAction, AppModel};
use app_lite_core::{CanonicalDocument, CreateNote, LibraryRepository, LibraryRoute, NoteId};
use std::sync::{Arc, mpsc::TryRecvError};

fn fixture(kind: &str) -> (tempfile::TempDir, Arc<LibraryRepository>, LibraryRoute, NoteId) {
    let root = tempfile::tempdir().unwrap();
    let repository = Arc::new(LibraryRepository::open(root.path().join("library.sqlite")).unwrap());
    let stack = repository.create_stack("重开276组").unwrap();
    let notebook = repository.create_notebook("重开276本", Some(&stack.id)).unwrap();
    let first_tag = repository.create_tag("重开276标签A").unwrap();
    let second_tag = repository.create_tag("重开276标签B").unwrap();
    let note = repository.create_note(CreateNote {
        title: "重开276独有".into(),
        notebook_id: Some(notebook.id.clone()),
        document: CanonicalDocument::default(),
    }).unwrap();
    repository.set_note_tags(&note.id, &[first_tag.id.clone(), second_tag.id.clone()]).unwrap();
    let route = match kind {
        "notebook" => LibraryRoute::Notebook(notebook.id),
        "stack" => LibraryRoute::Stack(stack.id),
        "tags" => LibraryRoute::tags(vec![first_tag.id,second_tag.id]).unwrap(),
        "trash" => {
            repository.trash_note(&note.id).unwrap();
            LibraryRoute::Trash
        },
        _ => panic!("unknown hand-written route fixture"),
    };
    (root,repository,route,note.id)
}

fn check_browse_restore(kind: &str, selected: bool) {
    let (root,repository,route,id) = fixture(kind);
    let selection = selected.then_some(id.clone());
    let mut model = AppModel::open(Arc::clone(&repository)).unwrap();
    model.dispatch(AppAction::NavigateTo {
        route: route.clone(), selected_note_id: selection.clone(),
    }).unwrap();
    assert_eq!(model.navigation().route(),&route);
    drop(model);
    drop(repository);
    let reopened = Arc::new(LibraryRepository::open(root.path().join("library.sqlite")).unwrap());
    let loads = reopened.observe_note_loads();
    let model = AppModel::open(reopened).unwrap();
    assert_eq!(model.navigation().route(),&route,"startup must retain the actual filter");
    assert_eq!(model.navigation().selected_note_id(),selection.as_ref());
    assert_eq!(model.active_session_note_id(),selection.as_ref());
    assert_eq!(model.projections().iter().map(|n|n.id.clone()).collect::<Vec<_>>(),vec![id.clone()]);
    assert!(!model.navigation().can_navigate_back(),"restoring is not a new user navigation");
    if selected { assert_eq!(loads.try_recv(),Ok(id)); }
    assert_eq!(loads.try_recv(),Err(TryRecvError::Empty),"never hydrate unselected cards");
}

#[test]
fn restart_276_notebook_with_selection() { check_browse_restore("notebook",true); }
#[test]
fn restart_276_notebook_without_selection() { check_browse_restore("notebook",false); }
#[test]
fn restart_276_stack_with_selection() { check_browse_restore("stack",true); }
#[test]
fn restart_276_stack_without_selection() { check_browse_restore("stack",false); }
#[test]
fn restart_276_tag_intersection_with_selection() { check_browse_restore("tags",true); }
#[test]
fn restart_276_tag_intersection_without_selection() { check_browse_restore("tags",false); }
#[test]
fn restart_276_trash_with_selection() { check_browse_restore("trash",true); }
#[test]
fn restart_276_trash_without_selection() { check_browse_restore("trash",false); }

#[test]
fn restart_276_same_selected_note_route_write_failure_leaves_old_packet_intact() {
    let (_root,repository,route,id) = fixture("tags");
    let mut model = AppModel::open(repository).unwrap();
    model.dispatch(AppAction::SelectNote(id.clone())).unwrap();
    let before = model.navigation().snapshot();
    let before_ids = model.projections().iter().map(|n|n.id.clone()).collect::<Vec<_>>();
    model.fail_next_shell_state_persist_for_test(app_lite_core::LibraryError::NotFound);
    let result = model.dispatch(AppAction::NavigateTo { route,selected_note_id:Some(id.clone()) });
    assert!(result.is_err(),"a filter change must cross the durable boundary even with the same selected ID");
    assert_eq!(model.navigation().snapshot(),before);
    assert_eq!(model.projections().iter().map(|n|n.id.clone()).collect::<Vec<_>>(),before_ids);
    assert_eq!(model.active_session_note_id(),Some(&id));
}

#[test]
fn restart_276_search_is_restored_by_existing_background_refresh_without_history_write() {
    let (root,repository,_route,id) = fixture("notebook");
    repository.process_search_jobs().unwrap();
    let query = "重开276独有";
    let mut model = AppModel::open(Arc::clone(&repository)).unwrap();
    model.dispatch(AppAction::SelectNote(id.clone())).unwrap();
    let generation = model.begin_search(query);
    let hits = repository.search(app_lite_core::SearchQuery::parse(query)).unwrap();
    assert_eq!(hits.len(),1);
    assert!(model.commit_search_results(generation,query.into(),hits,Some(id.clone())).unwrap());
    let history = repository.list_recent_searches("",10).unwrap();
    drop(model);
    drop(repository);
    let reopened = Arc::new(LibraryRepository::open(root.path().join("library.sqlite")).unwrap());
    let loads = reopened.observe_note_loads();
    let mut model = AppModel::open(Arc::clone(&reopened)).unwrap();
    assert_eq!(model.navigation().search_query(),Some(query));
    assert!(model.projections().is_empty(),"FTS must remain outside synchronous startup");
    assert_eq!(model.active_session_note_id(),None);
    assert_eq!(loads.try_recv(),Err(TryRecvError::Empty));
    let (pending,snapshot,generation) = model.pending_search_refresh().expect("one startup refresh");
    assert_eq!(pending,query);
    let hits = reopened.search(app_lite_core::SearchQuery::parse(query)).unwrap();
    assert!(model.commit_search_refresh(query,&snapshot,generation,hits).unwrap());
    assert_eq!(model.active_session_note_id(),Some(&id));
    assert_eq!(loads.try_recv(),Ok(id));
    assert_eq!(loads.try_recv(),Err(TryRecvError::Empty));
    assert!(!model.navigation().can_navigate_back());
    assert!(model.pending_search_refresh().is_none());
    assert_eq!(reopened.list_recent_searches("",10).unwrap(),history,"startup isn't another successful user search");
}

#[test]
fn restart_276_unavailable_container_falls_back_without_phantom_history_or_wrong_note() {
    let (_root,repository,_route,id) = fixture("notebook");
    repository.write_library_shell_state(&app_lite_core::LibraryShellState {
        location: app_lite_core::LibraryShellLocation::Browse(LibraryRoute::Notebook(
            app_lite_core::NotebookId::parse("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa").unwrap(),
        )),
        selected_note_id: Some(id.clone()), ..Default::default()
    }).unwrap();
    let model = AppModel::open(Arc::clone(&repository)).unwrap();
    assert_eq!(model.navigation().route(),&LibraryRoute::AllNotes);
    assert_eq!(model.active_session_note_id(),Some(&id));
    assert!(!model.navigation().can_navigate_back());
    assert_eq!(repository.read_library_shell_state().unwrap().location,app_lite_core::LibraryShellLocation::default());
}
