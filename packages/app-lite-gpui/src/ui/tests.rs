use super::*;
use crate::app::AppAction;
use app_lite_core::document::{Block, BlockStyle, Inline};
use app_lite_core::{CanonicalDocument, CreateNote, LibraryShellState};
use gpui::{AppContext, Modifiers, TestAppContext, VisualTestContext};
use std::sync::Arc;
use std::time::Duration;

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

fn rich_document(text: &str) -> CanonicalDocument {
    CanonicalDocument::from_blocks(vec![Block::Paragraph {
        style: BlockStyle::default(),
        inlines: vec![Inline::Text {
            text: text.into(),
            marks: Default::default(),
        }],
    }])
}

fn mount_shell<'a>(
    repository: Arc<LibraryRepository>,
    cx: &'a mut TestAppContext,
) -> (Entity<LibraryShell>, &'a mut VisualTestContext) {
    let model_state = AppModel::open(repository).expect("open model");
    let model = cx.new(|_| model_state);
    cx.add_window_view(move |_window, cx| LibraryShell::new(model, None, cx))
}

#[gpui::test]
async fn mounted_empty_cta_uses_the_shell_action_reducer(cx: &mut TestAppContext) {
    let (_profile, repository) = repository();
    let (view, cx) = mount_shell(Arc::clone(&repository), cx);
    redraw(cx);

    let create = cx
        .debug_bounds("create-first-note")
        .expect("mounted empty-library CTA");
    cx.simulate_click(create.center(), Modifiers::default());
    redraw(cx);
    view.read_with(cx, |view, cx| {
        assert_eq!(view.model.read(cx).projections().len(), 1);
        assert!(view.model.read(cx).active_note().is_some());
        assert!(view.editor_surface.is_some());
    });
}

#[gpui::test]
async fn mounted_card_click_reaches_the_same_shell_action_reducer(cx: &mut TestAppContext) {
    let (_profile, repository) = repository();
    let stored = repository
        .create_note(CreateNote {
            title: "可点击卡片".into(),
            notebook_id: None,
            document: rich_document("卡片点击后的正文"),
        })
        .expect("create note");
    let (view, cx) = mount_shell(repository, cx);
    redraw(cx);
    let card = cx
        .debug_bounds("library-note-card")
        .expect("mounted virtual-list card");
    cx.simulate_click(card.center(), Modifiers::default());
    redraw(cx);
    view.read_with(cx, |view, cx| {
        assert_eq!(
            view.model.read(cx).navigation().selected_note_id(),
            Some(&stored.id)
        );
        assert!(view.editor_surface.is_some());
    });
}

#[gpui::test]
async fn persisted_pane_widths_and_visibility_control_real_rendered_bounds(
    cx: &mut TestAppContext,
) {
    let (_profile, repository) = repository();
    repository
        .write_library_shell_state(&LibraryShellState {
            sidebar_width: 260,
            list_width: 410,
            sidebar_visible: true,
            list_visible: true,
            selected_note_id: None,
        })
        .expect("persist panes");
    let (view, cx) = mount_shell(Arc::clone(&repository), cx);
    redraw(cx);
    let sidebar = cx.debug_bounds("library-sidebar").expect("sidebar bounds");
    let list = cx
        .debug_bounds("library-note-list")
        .expect("note-list bounds");
    assert_eq!(f32::from(sidebar.size.width), 260.0);
    assert_eq!(f32::from(list.size.width), 410.0);

    cx.update(|window, app| {
        view.update(app, |view, view_cx| {
            view.apply_action(AppAction::ToggleSidebar, window, view_cx);
            view.apply_action(AppAction::ToggleNoteList, window, view_cx);
        });
    });
    redraw(cx);
    let sidebar = cx.debug_bounds("library-sidebar").expect("sidebar bounds");
    assert_eq!(f32::from(sidebar.size.width), 0.0);
    let list = cx
        .debug_bounds("library-note-list")
        .expect("hidden note-list bounds");
    assert_eq!(f32::from(list.size.width), 0.0);
}

#[gpui::test]
async fn event_bridge_coalesces_external_projection_changes_without_loading_bodies(
    cx: &mut TestAppContext,
) {
    let (_profile, repository) = repository();
    let (view, cx) = mount_shell(Arc::clone(&repository), cx);
    redraw(cx);
    repository
        .create_note(CreateNote {
            title: "外部创建".into(),
            notebook_id: None,
            document: rich_document("事件桥不应把这段正文当作列表字段读取"),
        })
        .expect("external create");
    cx.executor().advance_clock(Duration::from_millis(60));
    cx.run_until_parked();
    redraw(cx);
    let id = view.read_with(cx, |view, cx| {
        let model = view.model.read(cx);
        assert_eq!(model.projections().len(), 1);
        assert!(
            model.active_note().is_none(),
            "event refresh must not hydrate a body"
        );
        model.projections()[0].id.clone()
    });

    repository.trash_note(&id).expect("external trash");
    cx.executor().advance_clock(Duration::from_millis(60));
    cx.run_until_parked();
    redraw(cx);
    view.read_with(cx, |view, cx| {
        assert!(view.model.read(cx).projections().is_empty());
    });
}

#[gpui::test]
async fn rich_body_mounts_the_native_canvas_and_never_uses_body_text_fallback(
    cx: &mut TestAppContext,
) {
    let (_profile, repository) = repository();
    let stored = repository
        .create_note(CreateNote {
            title: "富文本".into(),
            notebook_id: None,
            document: rich_document("来自 HTML 的粗体正文"),
        })
        .expect("create rich note");
    let mut forged = stored.clone();
    forged.body_text = "绝不能用作编辑器输入的 sentinel".into();
    let imported = import_note_body(&forged).expect("body_html importer");
    let probe = cx.new(|cx| EditorCore::new_read_only(imported, cx));
    let probe_text = cx.update(|app| probe.read(app).copy_all_plain_text());
    assert!(probe_text.contains("来自 HTML 的粗体正文"));
    assert!(!probe_text.contains("sentinel"));

    let (view, cx) = mount_shell(Arc::clone(&repository), cx);
    cx.update(|window, app| {
        view.update(app, |view, view_cx| {
            view.apply_action(AppAction::SelectNote(stored.id.clone()), window, view_cx)
        });
    });
    redraw(cx);
    assert!(cx.debug_bounds("native-editor-surface").is_some());
    view.read_with(cx, |view, cx| {
        let surface = view.editor_surface.as_ref().expect("mounted surface");
        assert_eq!(surface.read(cx).mode(), EditorSurfaceMode::ReadOnly);
        assert!(
            surface
                .read(cx)
                .editor()
                .read(cx)
                .copy_all_plain_text()
                .contains("来自 HTML 的粗体正文")
        );
    });
}

#[gpui::test]
async fn switching_to_an_unsupported_body_removes_the_previous_native_surface(
    cx: &mut TestAppContext,
) {
    let (_profile, repository) = repository();
    let supported = repository
        .create_note(CreateNote {
            title: "可显示正文".into(),
            notebook_id: None,
            document: rich_document("这篇正文必须先挂载画布"),
        })
        .expect("create supported note");
    let image = repository
        .import_image(
            b"task-5-will-decode-this",
            "future image",
            "image/png",
            "png",
        )
        .expect("create resource fixture");
    let unsupported = repository
        .create_note(CreateNote {
            title: "含图像正文".into(),
            notebook_id: None,
            document: CanonicalDocument::from_blocks(vec![Block::Paragraph {
                style: BlockStyle::default(),
                inlines: vec![Inline::Image {
                    resource_id: image,
                    alt: "Task 5 image".into(),
                }],
            }]),
        })
        .expect("create image note");
    let (view, cx) = mount_shell(repository, cx);
    cx.update(|window, app| {
        view.update(app, |view, view_cx| {
            view.apply_action(AppAction::SelectNote(supported.id.clone()), window, view_cx)
        });
    });
    redraw(cx);
    assert!(cx.debug_bounds("native-editor-surface").is_some());

    cx.update(|window, app| {
        view.update(app, |view, view_cx| {
            view.apply_action(
                AppAction::SelectNote(unsupported.id.clone()),
                window,
                view_cx,
            )
        });
    });
    redraw(cx);
    assert!(cx.debug_bounds("unsupported-native-document").is_some());
    view.read_with(cx, |view, _| {
        assert!(
            view.editor_surface.is_none(),
            "old note surface must be dropped"
        );
        assert_eq!(view.surface_note_id, Some(unsupported.id));
        assert!(
            view.unsupported_document
                .as_deref()
                .is_some_and(|message| message.contains("图片"))
        );
    });
}

#[gpui::test]
async fn uniform_list_constructs_only_requested_ranges_and_reaches_1662_tail(
    cx: &mut TestAppContext,
) {
    let (_profile, repository) = repository();
    for index in 0..1_662 {
        repository
            .create_note(CreateNote {
                title: format!("note {index:04}"),
                notebook_id: None,
                document: CanonicalDocument::default(),
            })
            .expect("create projection fixture");
    }
    let (view, cx) = mount_shell(repository, cx);
    note_list::reset_constructed_items_for_test();
    redraw(cx);
    let initial_constructed = note_list::constructed_items_for_test();
    assert!(
        initial_constructed < 128,
        "uniform list eagerly built {initial_constructed} cards"
    );
    view.read_with(cx, |view, _| {
        assert!(
            view.note_list_scroll.is_scrollable(),
            "mounted uniform list must have a finite viewport"
        );
    });

    let (tail, tail_index) = view.read_with(cx, |view, cx| {
        let model = view.model.read(cx);
        (
            model
                .projections()
                .last()
                .expect("tail projection")
                .id
                .clone(),
            model.projections().len() - 1,
        )
    });
    cx.update(|window, app| {
        view.update(app, |view, view_cx| {
            view.apply_action(AppAction::SelectNote(tail.clone()), window, view_cx)
        });
    });
    view.read_with(cx, |view, app| {
        assert_eq!(
            view.model.read_with(app, |model, _| model
                .navigation()
                .selected_note_id()
                .cloned()),
            Some(tail.clone()),
            "selection action must succeed before scrolling"
        );
        assert_eq!(view.last_scroll_request, Some(tail_index));
    });
    redraw(cx);
    view.read_with(cx, |view, cx| {
        assert!(
            view.rendered_note_range
                .as_ref()
                .is_some_and(|range| range.contains(&tail_index)),
            "tail card was not constructed after scroll"
        );
        assert_eq!(
            view.model.read(cx).navigation().selected_note_id(),
            Some(&tail)
        );
    });
    assert!(
        note_list::constructed_items_for_test() < 320,
        "tail scroll built the whole library"
    );
}

#[gpui::test]
async fn uniform_list_does_not_truncate_the_101st_projection(cx: &mut TestAppContext) {
    let (_profile, repository) = repository();
    for index in 0..101 {
        repository
            .create_note(CreateNote {
                title: format!("threshold {index:03}"),
                notebook_id: None,
                document: CanonicalDocument::default(),
            })
            .expect("create threshold fixture");
    }
    let (view, cx) = mount_shell(repository, cx);
    redraw(cx);
    let tail = view.read_with(cx, |view, cx| {
        view.model
            .read(cx)
            .projections()
            .last()
            .expect("101st projection")
            .id
            .clone()
    });
    cx.update(|window, app| {
        view.update(app, |view, view_cx| {
            view.apply_action(AppAction::SelectNote(tail.clone()), window, view_cx)
        });
    });
    redraw(cx);
    view.read_with(cx, |view, cx| {
        assert_eq!(
            view.model.read(cx).navigation().selected_note_id(),
            Some(&tail)
        );
        assert!(
            view.rendered_note_range
                .as_ref()
                .is_some_and(|range| range.contains(&100)),
            "the 101st card must be mounted after its NoteId is selected"
        );
    });
}
