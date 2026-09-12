use super::*;
use app_lite_core::{LibraryRepository, LibraryRoute};
use gpui::{Entity, Modifiers, TestAppContext, VisualTestContext};
use std::sync::Arc;

fn redraw(cx: &mut VisualTestContext) {
    cx.update(|window, app| window.draw(app).clear());
    cx.run_until_parked();
}

fn repository() -> (tempfile::TempDir, Arc<LibraryRepository>) {
    let profile = tempfile::tempdir().expect("temporary profile");
    let repository = Arc::new(
        LibraryRepository::open(profile.path().join("library.sqlite"))
            .expect("open temporary library"),
    );
    (profile, repository)
}

fn mount_shell<'a>(
    repository: Arc<LibraryRepository>,
    cx: &'a mut TestAppContext,
) -> (Entity<LibraryShell>, &'a mut VisualTestContext) {
    let model = cx.new(|_| AppModel::open(repository).expect("open model"));
    cx.add_window_view(move |window, cx| LibraryShell::new(model, None, window, cx))
}

#[gpui::test]
async fn mounted_nested_notebook_new_button_uses_the_clicked_notebook_as_its_durable_container(
    cx: &mut TestAppContext,
) {
    // This is the Release repro: click the visible nested Notebook row, then
    // the production New button. Removing AppModel's typed destination or
    // reverting it to `notebook_id: None` makes the durable assertion below
    // fail because RepositoryCreateNote legitimately chooses Default.
    let (_profile, repository) = repository();
    let stack = repository.create_stack("发布组").expect("create stack");
    let notebook = repository
        .create_notebook("In stack notebook", Some(&stack.id))
        .expect("create nested notebook");
    let default = repository.default_notebook().expect("default notebook");
    assert_ne!(notebook.id, default.id, "fixture needs distinct containers");

    let (view, cx) = mount_shell(Arc::clone(&repository), cx);
    redraw(cx);
    let route_selector: &'static str = Box::leak(
        format!("library-sidebar-route-notebook-{}", notebook.id.as_str()).into_boxed_str(),
    );
    let nested_notebook = cx
        .debug_bounds(route_selector)
        .expect("nested Notebook route must be mounted in the real sidebar");
    cx.simulate_click(nested_notebook.center(), Modifiers::default());
    redraw(cx);
    let create = cx
        .debug_bounds("library-create-note")
        .expect("production library New button");
    cx.simulate_click(create.center(), Modifiers::default());
    redraw(cx);

    let created = view.read_with(cx, |shell, app| {
        let model = shell.model.read(app);
        assert_eq!(
            model.navigation().route(),
            &LibraryRoute::Notebook(notebook.id.clone()),
            "the shell must retain a route where the created selection is legal"
        );
        let created = model
            .navigation()
            .selected_note_id()
            .cloned()
            .expect("new note selected by the shared shell reducer");
        assert_eq!(model.active_session_note_id(), Some(&created));
        assert!(shell.note_session.is_some());
        assert!(shell.editor_surface.is_some());
        created
    });
    assert_eq!(
        repository
            .load_note(&created)
            .expect("load durable note")
            .expect("new note remains durable")
            .notebook_id,
        notebook.id,
        "the actual UI click must not create in the default notebook"
    );
}
