use super::*;
use crate::app::AppAction;
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

fn open_organization_panel(cx: &mut VisualTestContext) {
    let toggle = cx
        .debug_bounds("library-toggle-organization")
        .expect("organization panel toggle");
    cx.simulate_click(toggle.center(), Modifiers::default());
    redraw(cx);
}

fn focus_and_type_organization_title(cx: &mut VisualTestContext, title: &str) {
    let input = cx
        .debug_bounds("library-organization-input")
        .expect("real organization EntityInputHandler");
    cx.simulate_click(input.center(), Modifiers::default());
    cx.simulate_input(title);
    redraw(cx);
}

fn organization_input_text(view: &Entity<LibraryShell>, cx: &VisualTestContext) -> String {
    view.read_with(cx, |shell, app| {
        shell.organization_input.read(app).text().to_owned()
    })
}

#[gpui::test]
async fn mounted_organization_create_buttons_clear_the_shared_input_only_after_success(
    cx: &mut TestAppContext,
) {
    // A real release regression: leaving the shared TitleInput populated made
    // the next create concatenate titles (for example, "NotebookStack").
    // The three visible create controls must use one success-only clear path,
    // rather than keeping independent button-local form state.
    let (_profile, repository) = repository();
    let (view, cx) = mount_shell(Arc::clone(&repository), cx);
    redraw(cx);
    open_organization_panel(cx);

    focus_and_type_organization_title(cx, "第一个笔记本");
    let create_notebook = cx
        .debug_bounds("library-organization-create-notebook")
        .expect("create notebook button");
    cx.simulate_click(create_notebook.center(), Modifiers::default());
    redraw(cx);
    assert_eq!(
        organization_input_text(&view, cx),
        "",
        "a successful notebook create must clear the shared input before the next create"
    );

    focus_and_type_organization_title(cx, "第二个组");
    let create_stack = cx
        .debug_bounds("library-organization-create-stack")
        .expect("create stack button");
    cx.simulate_click(create_stack.center(), Modifiers::default());
    redraw(cx);
    assert_eq!(organization_input_text(&view, cx), "");

    focus_and_type_organization_title(cx, "第三个标签");
    let create_tag = cx
        .debug_bounds("library-organization-create-tag")
        .expect("create tag button");
    cx.simulate_click(create_tag.center(), Modifiers::default());
    redraw(cx);
    assert_eq!(organization_input_text(&view, cx), "");

    view.read_with(cx, |shell, app| {
        let index = shell.model.read(app).navigation_index();
        assert!(
            index
                .notebooks
                .iter()
                .any(|notebook| notebook.title == "第一个笔记本")
        );
        assert!(index.stacks.iter().any(|stack| stack.title == "第二个组"));
        assert!(index.tags.iter().any(|tag| tag.title == "第三个标签"));
    });
}

#[gpui::test]
async fn mounted_organization_create_failure_preserves_input_then_enter_uses_the_same_success_clear(
    cx: &mut TestAppContext,
) {
    // Whitespace is rejected by the real repository validator. Its text must
    // remain available for correction; then the same input's Enter path must
    // clear only after the repository accepted the corrected title.
    let (_profile, repository) = repository();
    let (view, cx) = mount_shell(Arc::clone(&repository), cx);
    redraw(cx);
    open_organization_panel(cx);

    focus_and_type_organization_title(cx, "   ");
    let create_notebook = cx
        .debug_bounds("library-organization-create-notebook")
        .expect("create notebook button");
    cx.simulate_click(create_notebook.center(), Modifiers::default());
    redraw(cx);
    assert_eq!(
        organization_input_text(&view, cx),
        "   ",
        "a rejected create must retain the exact title for correction"
    );
    view.read_with(cx, |shell, app| {
        assert!(
            shell
                .model
                .read(app)
                .navigation_index()
                .notebooks
                .iter()
                .all(|notebook| notebook.title != "   ")
        );
    });

    let input = cx
        .debug_bounds("library-organization-input")
        .expect("organization input remains mounted after the rejection");
    cx.simulate_click(input.center(), Modifiers::default());
    cx.simulate_keystrokes("cmd-a");
    cx.simulate_input("通过 Enter 创建");
    cx.simulate_keystrokes("enter");
    redraw(cx);
    assert_eq!(
        organization_input_text(&view, cx),
        "",
        "the Enter route must share the success-only input clear"
    );
    view.read_with(cx, |shell, app| {
        assert!(
            shell
                .model
                .read(app)
                .navigation_index()
                .notebooks
                .iter()
                .any(|notebook| notebook.title == "通过 Enter 创建")
        );
    });
}
