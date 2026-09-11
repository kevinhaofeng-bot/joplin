use super::*;
use crate::app::AppAction;
use crate::app::save_coordinator::ManualSaveClock;
use crate::components::{Copy, Paste, SelectAll};
use crate::native_editor::model::{Affinity, BlockKind, DocPoint, Mark};
use app_lite_core::document::{Block, BlockStyle, Inline, Marks};
use app_lite_core::{CanonicalDocument, CreateNote, LibraryRoute, LibraryShellState};
use gpui::{
    AppContext, ClipboardItem, EntityInputHandler, Image, ImageFormat, Modifiers, TestAppContext,
    VisualTestContext, point, px,
};
use std::io::Cursor;
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
use std::sync::mpsc::TryRecvError;
use std::sync::{Arc, Mutex};
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

/// A non-trivial real PNG for pointer tests. The common 1×1 clipboard fixture
/// is intentionally tiny, but its display rect cannot distinguish a left or
/// right image edge in a mounted high-DPI canvas.
fn structural_png(width: u32, height: u32) -> Vec<u8> {
    let image = image::RgbaImage::from_pixel(width, height, image::Rgba([0x1b, 0x7f, 0x46, 0xff]));
    let mut encoded = Cursor::new(Vec::new());
    image::DynamicImage::ImageRgba8(image)
        .write_to(&mut encoded, image::ImageFormat::Png)
        .expect("encode structural PNG fixture");
    encoded.into_inner()
}

fn mount_shell<'a>(
    repository: Arc<LibraryRepository>,
    cx: &'a mut TestAppContext,
) -> (Entity<LibraryShell>, &'a mut VisualTestContext) {
    let model_state = AppModel::open(repository).expect("open model");
    let model = cx.new(|_| model_state);
    cx.add_window_view(move |window, cx| LibraryShell::new(model, None, window, cx))
}

fn mount_shell_with_save_clock<'a>(
    repository: Arc<LibraryRepository>,
    clock: Arc<ManualSaveClock>,
    cx: &'a mut TestAppContext,
) -> (Entity<LibraryShell>, &'a mut VisualTestContext) {
    let model_state = AppModel::open(repository).expect("open model");
    let model = cx.new(|_| model_state);
    cx.add_window_view(move |window, cx| {
        LibraryShell::new_with_save_clock(model, None, clock, window, cx)
    })
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
async fn mounted_keyboard_actions_use_the_same_shell_reducer(cx: &mut TestAppContext) {
    cx.update(bind_library_keybindings);
    let (_profile, repository) = repository();
    let (view, cx) = mount_shell(repository, cx);
    redraw(cx);

    cx.simulate_keystrokes("cmd-n");
    redraw(cx);
    view.read_with(cx, |view, cx| {
        assert_eq!(view.model.read(cx).projections().len(), 1);
        assert!(view.model.read(cx).active_note().is_some());
    });

    for key in ["cmd-alt-s", "cmd-alt-l", "cmd-alt-v", "cmd-alt-o"] {
        cx.simulate_keystrokes(key);
        redraw(cx);
    }
    view.read_with(cx, |view, cx| {
        let model = view.model.read(cx);
        assert!(!model.panes().sidebar_visible);
        assert!(!model.panes().list_visible);
        assert_eq!(model.list_view_mode(), ListViewMode::Snippets);
        assert_eq!(model.sort(), crate::app::NoteSort::TitleAscending);
    });

    cx.simulate_keystrokes("cmd-shift-backspace");
    redraw(cx);
    view.read_with(cx, |view, cx| {
        assert!(view.model.read(cx).projections().is_empty());
        assert_eq!(view.model.read(cx).status(), &AppStatus::Ready);
    });
}

#[gpui::test]
async fn mounted_native_menu_actions_use_the_same_shell_reducer(cx: &mut TestAppContext) {
    let (_profile, repository) = repository();
    let (view, cx) = mount_shell(repository, cx);
    redraw(cx);

    // Native menus dispatch the action object directly, rather than a mouse
    // event or key binding. The focused LibraryShell must still receive it.
    cx.dispatch_action(crate::app::CreateNote);
    redraw(cx);
    view.read_with(cx, |view, cx| {
        assert_eq!(view.model.read(cx).projections().len(), 1);
        assert!(view.model.read(cx).active_note().is_some());
    });

    cx.dispatch_action(crate::app::TrashSelected);
    redraw(cx);
    view.read_with(cx, |view, cx| {
        assert!(view.model.read(cx).projections().is_empty());
        assert_eq!(view.model.read(cx).status(), &AppStatus::Ready);
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
async fn mounted_typed_sidebar_routes_use_durable_ids_without_hydrating_cards(
    cx: &mut TestAppContext,
) {
    // Mutation-sensitive: replacing sidebar rows with labels/indexes, routing
    // them around AppModel::NavigateTo, or loading a body while only changing
    // a route makes a typed route, exact projection, or observer assertion
    // below fail.
    let (_profile, repository) = repository();
    let stack = repository.create_stack("项目组").expect("create stack");
    let notebook = repository
        .create_notebook("客户端", Some(&stack.id))
        .expect("create notebook");
    let tag = repository.create_tag("紧急").expect("create tag");
    let cover = repository
        .import_resource(
            &structural_png(48, 32),
            "导航缩略图.png",
            "image/png",
            "png",
        )
        .expect("store thumbnail fixture");
    let scoped = repository
        .create_note(CreateNote {
            title: "项目卡片".into(),
            notebook_id: Some(notebook.id.clone()),
            document: CanonicalDocument::from_blocks(vec![Block::Image {
                resource_id: cover,
                alt: "卡片缩略图只应作为 projection key".into(),
                presentation: Default::default(),
            }]),
        })
        .expect("create scoped note");
    repository
        .set_note_tags(&scoped.id, &[tag.id.clone()])
        .expect("tag scoped note");
    let trashed = repository
        .create_note(CreateNote {
            title: "废纸篓卡片".into(),
            notebook_id: None,
            document: rich_document("废纸篓正文也不是导航字段"),
        })
        .expect("create trash note");
    repository.trash_note(&trashed.id).expect("trash note");

    let body_loads = repository.observe_note_loads();
    let resource_reads = repository.observe_resource_reads();
    let (view, cx) = mount_shell(Arc::clone(&repository), cx);
    redraw(cx);
    let notebook_selector: &'static str = Box::leak(
        format!("library-sidebar-route-notebook-{}", notebook.id.as_str()).into_boxed_str(),
    );
    let stack_selector: &'static str =
        Box::leak(format!("library-sidebar-route-stack-{}", stack.id.as_str()).into_boxed_str());
    let tag_selector: &'static str =
        Box::leak(format!("library-sidebar-route-tag-{}", tag.id.as_str()).into_boxed_str());
    for selector in [
        "library-sidebar-route-all-notes",
        notebook_selector,
        stack_selector,
        tag_selector,
        "library-sidebar-route-trash",
    ] {
        assert!(
            cx.debug_bounds(selector).is_some(),
            "the mounted typed sidebar must expose {selector}"
        );
    }

    let assertions = [
        (
            notebook_selector,
            LibraryRoute::Notebook(notebook.id.clone()),
            vec![scoped.id.clone()],
        ),
        (
            stack_selector,
            LibraryRoute::Stack(stack.id.clone()),
            vec![scoped.id.clone()],
        ),
        (
            tag_selector,
            LibraryRoute::tags(vec![tag.id.clone()]).expect("single-tag route"),
            vec![scoped.id.clone()],
        ),
        (
            "library-sidebar-route-trash",
            LibraryRoute::Trash,
            vec![trashed.id.clone()],
        ),
    ];
    for (selector, route, ids) in assertions {
        let hit = cx.debug_bounds(selector).expect("mounted sidebar row");
        cx.simulate_click(hit.center(), Modifiers::default());
        redraw(cx);
        assert!(
            cx.debug_bounds("library-sidebar-selected-route").is_some(),
            "the clicked typed route must paint the sidebar's selected state"
        );
        view.read_with(cx, |shell, app| {
            let model = shell.model.read(app);
            assert_eq!(model.navigation().route(), &route);
            assert_eq!(
                model
                    .projections()
                    .iter()
                    .map(|projection| projection.id.clone())
                    .collect::<Vec<_>>(),
                ids,
                "the clicked route must use the same typed projection authority"
            );
            assert!(
                model.active_note().is_none(),
                "route navigation without a selected NoteId must not hydrate a body"
            );
        });
        assert_eq!(body_loads.try_recv(), Err(TryRecvError::Empty));
        assert_eq!(resource_reads.try_recv(), Err(TryRecvError::Empty));
    }

    let all_notes = cx
        .debug_bounds("library-sidebar-route-all-notes")
        .expect("mounted All Notes row");
    cx.simulate_click(all_notes.center(), Modifiers::default());
    redraw(cx);
    view.read_with(cx, |shell, app| {
        let model = shell.model.read(app);
        assert_eq!(model.navigation().route(), &LibraryRoute::AllNotes);
        assert_eq!(
            model
                .projections()
                .iter()
                .map(|projection| projection.id.clone())
                .collect::<Vec<_>>(),
            vec![scoped.id.clone()],
            "All Notes is a typed route, not a label-only reset of the current cards"
        );
    });
    assert_eq!(body_loads.try_recv(), Err(TryRecvError::Empty));
    assert_eq!(resource_reads.try_recv(), Err(TryRecvError::Empty));
}

#[gpui::test]
async fn mounted_sidebar_virtualizes_scale_fixture_and_reaches_tail_typed_routes(
    cx: &mut TestAppContext,
) {
    // Mutation-sensitive: replacing the true scrollable uniform list with an
    // eager h_full/overflow_hidden column leaves the tail rows either mounted
    // outside the viewport or unreachable after the retained scroll request.
    // Routing either tail row by label/index instead of its durable ID also
    // changes the exact route assertions below.
    let (_profile, repository) = repository();
    for index in 0..30 {
        let title = format!("规模笔记本 {index:02}");
        repository
            .create_notebook(&title, None)
            .expect("create sidebar notebook fixture");
    }
    let tags = (0..64)
        .map(|index| {
            let title = format!("规模标签 {index:03}");
            repository
                .create_tag(&title)
                .expect("create sidebar tag fixture")
        })
        .collect::<Vec<_>>();
    let tail_tag = tags.last().expect("tail tag").clone();
    let tail_tag_route = LibraryRoute::tags(vec![tail_tag.id.clone()]).expect("tail tag route");
    let tail_tag_selector: &'static str =
        Box::leak(format!("library-sidebar-route-tag-{}", tail_tag.id.as_str()).into_boxed_str());

    let body_loads = repository.observe_note_loads();
    let resource_reads = repository.observe_resource_reads();
    let (view, cx) = mount_shell(repository, cx);
    cx.simulate_resize(gpui::size(px(1280.0), px(760.0)));
    redraw(cx);

    let initial_sidebar_probe = view.read_with(cx, |shell, app| {
        assert_eq!(
            shell.model.read(app).navigation_index().notebooks.len(),
            31,
            "the scale fixture includes the default plus thirty extra notebooks"
        );
        assert_eq!(
            shell.model.read(app).navigation_index().tags.len(),
            64,
            "the scale fixture includes every durable tag"
        );
        assert!(
            shell.sidebar_scroll.is_scrollable(),
            "a 760px library sidebar must expose a real scroll viewport"
        );
        shell.sidebar_render_probe_for_test()
    });
    assert!(
        initial_sidebar_probe.request_count > 0,
        "the mounted shell must receive a real uniform-list request"
    );
    assert!(
        initial_sidebar_probe.largest_requested_range < 80,
        "first sidebar draw eagerly constructed {} rows in one request",
        initial_sidebar_probe.largest_requested_range
    );
    assert!(
        cx.debug_bounds(&tail_tag_selector).is_none(),
        "the tail tag must not be falsely interactable before the virtual sidebar requests it"
    );
    assert!(
        cx.debug_bounds("library-sidebar-route-trash").is_none(),
        "Trash must initially remain below the 760px viewport in this scale fixture"
    );

    let (tail_tag_index, trash_index) = view.read_with(cx, |shell, app| {
        let index = shell.model.read(app).navigation_index();
        (
            sidebar::route_index_for_test(index, &tail_tag_route).expect("tail tag row"),
            sidebar::route_index_for_test(index, &LibraryRoute::Trash).expect("trash row"),
        )
    });
    view.update(cx, |shell, _| {
        shell
            .sidebar_scroll
            .scroll_to_item(tail_tag_index, gpui::ScrollStrategy::Center);
    });
    redraw(cx);
    let tail_tag_sidebar_probe =
        view.read_with(cx, |shell, _| shell.sidebar_render_probe_for_test());
    assert!(
        tail_tag_sidebar_probe.request_count > initial_sidebar_probe.request_count,
        "the retained sidebar handle must request a new range for the tail tag"
    );
    assert!(
        tail_tag_sidebar_probe
            .last_requested_range
            .as_ref()
            .is_some_and(|range| range.contains(&tail_tag_index)),
        "the single shell must request the tail-tag range rather than construct every row"
    );
    assert!(
        tail_tag_sidebar_probe.largest_requested_range < 80,
        "one sidebar request constructed {} rows instead of a bounded viewport range",
        tail_tag_sidebar_probe.largest_requested_range
    );
    let tail_tag_bounds = cx
        .debug_bounds(&tail_tag_selector)
        .expect("tail tag is interactable after the true scroll request");
    cx.simulate_click(tail_tag_bounds.center(), Modifiers::default());
    redraw(cx);
    view.read_with(cx, |shell, app| {
        assert_eq!(shell.model.read(app).navigation().route(), &tail_tag_route);
    });
    assert!(
        cx.debug_bounds("library-sidebar-selected-route").is_some(),
        "tail Tag navigation must paint the selected typed route"
    );

    view.update(cx, |shell, _| {
        shell
            .sidebar_scroll
            .scroll_to_item(trash_index, gpui::ScrollStrategy::Bottom);
    });
    redraw(cx);
    let trash_sidebar_probe = view.read_with(cx, |shell, _| shell.sidebar_render_probe_for_test());
    assert!(
        trash_sidebar_probe.request_count > tail_tag_sidebar_probe.request_count,
        "the same shell must request a separate bounded range for Trash"
    );
    assert!(
        trash_sidebar_probe
            .last_requested_range
            .as_ref()
            .is_some_and(|range| range.contains(&trash_index)),
        "the single shell must request the Trash range rather than reuse a process-global count"
    );
    assert!(
        trash_sidebar_probe.largest_requested_range < 80,
        "one sidebar processor request constructed {} rows instead of a bounded viewport range",
        trash_sidebar_probe.largest_requested_range
    );
    let trash_bounds = cx
        .debug_bounds("library-sidebar-route-trash")
        .expect("Trash is interactable after the true scroll request");
    cx.simulate_click(trash_bounds.center(), Modifiers::default());
    redraw(cx);
    view.read_with(cx, |shell, app| {
        assert_eq!(
            shell.model.read(app).navigation().route(),
            &LibraryRoute::Trash
        );
        assert!(shell.model.read(app).active_note().is_none());
    });
    assert!(
        cx.debug_bounds("library-sidebar-selected-route").is_some(),
        "tail Trash navigation must paint the selected typed route"
    );
    assert_eq!(body_loads.try_recv(), Err(TryRecvError::Empty));
    assert_eq!(resource_reads.try_recv(), Err(TryRecvError::Empty));
}

#[gpui::test]
async fn mounted_list_modes_and_pane_collapse_keep_one_projection_and_live_session(
    cx: &mut TestAppContext,
) {
    // Mutation-sensitive: a second list authority, a card mode that reloads
    // notes, or collapse rebuilding the session/editor makes identity/order
    // or no-hydration assertions below fail.
    let (_profile, repository) = repository();
    let selected = repository
        .create_note(CreateNote {
            title: "第一篇".into(),
            notebook_id: None,
            document: rich_document("已挂载编辑器必须穿过三种列表模式"),
        })
        .expect("create selected note");
    for title in ["第二篇", "第三篇"] {
        repository
            .create_note(CreateNote {
                title: title.into(),
                notebook_id: None,
                document: rich_document("卡片正文永远不是列表数据"),
            })
            .expect("create list note");
    }
    let (view, cx) = mount_shell(Arc::clone(&repository), cx);
    redraw(cx);
    cx.update(|window, app| {
        view.update(app, |shell, shell_cx| {
            shell.apply_action(AppAction::SelectNote(selected.id.clone()), window, shell_cx)
        });
    });
    redraw(cx);

    let (projection_ids, session_id, surface_id) = view.read_with(cx, |shell, app| {
        (
            shell
                .model
                .read(app)
                .projections()
                .iter()
                .map(|projection| projection.id.clone())
                .collect::<Vec<_>>(),
            shell
                .note_session
                .as_ref()
                .expect("mounted NoteSession")
                .entity_id(),
            shell
                .editor_surface
                .as_ref()
                .expect("mounted EditorSurface")
                .entity_id(),
        )
    });
    let body_loads = repository.observe_note_loads();
    for (selector, mode) in [
        ("library-note-card-snippets", ListViewMode::Snippets),
        ("library-note-card-compact", ListViewMode::Compact),
        ("library-note-card-cards", ListViewMode::Cards),
    ] {
        let control = cx
            .debug_bounds("library-cycle-view")
            .expect("real list-mode control");
        cx.simulate_click(control.center(), Modifiers::default());
        redraw(cx);
        assert!(
            cx.debug_bounds(selector).is_some(),
            "the mounted card mode must change the real card presentation"
        );
        view.read_with(cx, |shell, app| {
            let model = shell.model.read(app);
            assert_eq!(model.list_view_mode(), mode);
            assert_eq!(
                model
                    .projections()
                    .iter()
                    .map(|projection| projection.id.clone())
                    .collect::<Vec<_>>(),
                projection_ids,
                "all visual modes must consume the same ordered projections"
            );
            assert_eq!(model.navigation().selected_note_id(), Some(&selected.id));
            assert_eq!(
                shell
                    .note_session
                    .as_ref()
                    .expect("retained session")
                    .entity_id(),
                session_id,
                "changing list presentation must not remount NoteSession"
            );
            assert_eq!(
                shell
                    .editor_surface
                    .as_ref()
                    .expect("retained surface")
                    .entity_id(),
                surface_id,
                "changing list presentation must not recreate EditorCore's surface"
            );
        });
        assert_eq!(body_loads.try_recv(), Err(TryRecvError::Empty));
    }

    for action in [AppAction::ToggleSidebar, AppAction::ToggleNoteList] {
        cx.update(|window, app| {
            view.update(app, |shell, shell_cx| {
                shell.apply_action(action, window, shell_cx)
            });
        });
        redraw(cx);
        view.read_with(cx, |shell, _| {
            assert_eq!(
                shell
                    .note_session
                    .as_ref()
                    .expect("retained session")
                    .entity_id(),
                session_id,
                "three/two/one-column collapse must retain the live NoteSession"
            );
            assert_eq!(
                shell
                    .editor_surface
                    .as_ref()
                    .expect("retained surface")
                    .entity_id(),
                surface_id,
                "three/two/one-column collapse must retain the existing EditorCore"
            );
        });
    }
    assert_eq!(body_loads.try_recv(), Err(TryRecvError::Empty));
}

#[gpui::test]
async fn mounted_default_editor_shell_paints_every_evernote_primary_surface(
    cx: &mut TestAppContext,
) {
    let (_profile, repository) = repository();
    let note = repository
        .create_note(CreateNote {
            title: "白色编辑壳".into(),
            notebook_id: None,
            document: rich_document("标题和正文都必须落在明确的白色主表面上。"),
        })
        .expect("create selected note");
    let (view, cx) = mount_shell(repository, cx);

    redraw(cx);
    let card = cx
        .debug_bounds("library-note-card")
        .expect("mounted note card");
    cx.simulate_click(card.center(), Modifiers::default());
    redraw(cx);

    assert_eq!(
        view.read_with(cx, |shell, app| {
            shell
                .model
                .read(app)
                .navigation()
                .selected_note_id()
                .cloned()
        }),
        Some(note.id),
        "the production card path must mount the actual title and editor pane before style assertions"
    );

    assert_eq!(
        view.read_with(cx, |shell, _| shell
            .rendered_primary_surface_fills_for_test()),
        [0xffffffff; 5],
        "the mounted default route must pass Evernote's #fff primary fill through every root, main editor, toolbar, title, and editor-pane .bg call"
    );
    for selector in [
        "library-shell",
        "library-main-editor-shell",
        "library-actions",
        "library-note-title",
        "library-native-editor-pane",
    ] {
        assert!(
            cx.debug_bounds(selector).is_some(),
            "the rendered primary-surface style tree must include {selector}"
        );
    }
}

#[gpui::test]
async fn mounted_default_route_mounts_the_shared_editor_command_chrome(cx: &mut TestAppContext) {
    let (_profile, repository) = repository();
    repository
        .create_note(CreateNote {
            title: "共享格式栏".into(),
            notebook_id: None,
            document: rich_document("默认资料库必须挂载与 spike 相同的格式命令组件。"),
        })
        .expect("create selected note");
    let (_view, cx) = mount_shell(repository, cx);

    redraw(cx);
    let card = cx
        .debug_bounds("library-note-card")
        .expect("mounted selected-note card");
    cx.simulate_click(card.center(), Modifiers::default());
    redraw(cx);

    assert!(
        cx.debug_bounds("library-editor-command-chrome").is_some(),
        "the default library route must mount the shared command chrome between its title and body"
    );
    assert!(
        cx.debug_bounds("library-editor-command-toolbar").is_some(),
        "the mounted shared chrome must expose its primary command row"
    );
    assert!(
        cx.debug_bounds("Bold").is_some(),
        "a visible formatting command must come from the mounted shared chrome rather than a second library-only toolbar"
    );
    let title = cx
        .debug_bounds("library-note-title")
        .expect("mounted library title input");
    let chrome = cx
        .debug_bounds("library-editor-command-chrome")
        .expect("mounted shared Chrome");
    let surface = cx
        .debug_bounds("native-editor-surface")
        .expect("mounted shared editor body");
    assert!(
        title.bottom() <= chrome.top() && chrome.bottom() <= surface.top(),
        "the production shared Chrome must sit between title and document body; title={title:?}, chrome={chrome:?}, surface={surface:?}"
    );
}

#[gpui::test]
async fn mounted_library_shared_chrome_clicks_bold_and_list_with_one_history_entry(
    cx: &mut TestAppContext,
) {
    // This is deliberately a mounted command-row test, not a direct
    // `CommandCatalogue` test. Removing the LibraryShell shared-Chrome mount
    // or routing either button through a second handler makes its hit targets
    // disappear and the mutation/history assertions fail.
    cx.update(|app| crate::components::init(app));
    let (_profile, repository) = repository();
    let note = repository
        .create_note(CreateNote {
            title: "共享格式命令".into(),
            notebook_id: None,
            document: rich_document("第一段"),
        })
        .expect("create formatted note");
    let (view, cx) = mount_shell(Arc::clone(&repository), cx);
    cx.simulate_resize(gpui::size(px(1400.0), px(820.0)));
    redraw(cx);
    let card = cx
        .debug_bounds("library-note-card")
        .expect("mounted note card");
    cx.simulate_click(card.center(), Modifiers::default());
    redraw(cx);

    let (editor, selected) = view.update(cx, |shell, shell_cx| {
        let editor = shell
            .note_session
            .as_ref()
            .expect("active library session")
            .read(shell_cx)
            .editor()
            .clone();
        let selected = editor.update(shell_cx, |editor, editor_cx| {
            let block = editor.document().blocks().first().expect("text block");
            let end = block.content.as_text().expect("text content").len();
            let selected = Selection::new(
                DocPoint::with_affinity(block.id, 0, Affinity::Before),
                DocPoint::with_affinity(block.id, end, Affinity::After),
            );
            editor.set_selection_for_test(selected);
            editor_cx.notify();
            selected
        });
        (editor, selected)
    });

    let bold_history = editor.read_with(cx, |editor, _| editor.undo_depth());
    let bold = cx.debug_bounds("Bold").expect("shared Bold button");
    cx.simulate_click(bold.center(), Modifiers::default());
    redraw(cx);
    editor.read_with(cx, |editor, _| {
        assert_eq!(editor.selection(), selected, "Bold must preserve selection");
        assert_eq!(
            editor.undo_depth(),
            bold_history + 1,
            "Bold is one transaction"
        );
        assert!(
            editor.document().blocks()[0]
                .content
                .styles()
                .expect("styled text")
                .iter()
                .any(|run| run.marks.contains(&Mark::Bold)),
            "the real library Chrome Bold click must mutate the active EditorCore"
        );
    });
    cx.simulate_keystrokes("cmd-z");
    redraw(cx);
    editor.read_with(cx, |editor, _| {
        assert_eq!(editor.undo_depth(), bold_history, "one Undo reverts Bold");
        assert!(
            editor.document().blocks()[0]
                .content
                .styles()
                .expect("styled text")
                .iter()
                .all(|run| !run.marks.contains(&Mark::Bold)),
            "Undo must restore the unformatted document"
        );
    });

    let list_history = editor.read_with(cx, |editor, _| editor.undo_depth());
    let list = cx
        .debug_bounds("Bulleted list")
        .expect("wide shared toolbar exposes Bulleted list");
    cx.simulate_click(list.center(), Modifiers::default());
    redraw(cx);
    editor.read_with(cx, |editor, _| {
        assert_eq!(
            editor.selection(),
            selected,
            "list command preserves selection"
        );
        assert_eq!(
            editor.undo_depth(),
            list_history + 1,
            "list is one transaction"
        );
        assert!(
            matches!(
                editor.document().blocks()[0].kind,
                BlockKind::BulletItem { depth: 0 }
            ),
            "the shared Bulleted list button must mutate the same active EditorCore"
        );
    });
    cx.simulate_keystrokes("cmd-z");
    redraw(cx);
    editor.read_with(cx, |editor, _| {
        assert!(
            matches!(editor.document().blocks()[0].kind, BlockKind::Paragraph),
            "one Undo must restore the paragraph after the list command"
        );
    });

    assert!(
        repository.load_note(&note.id).expect("load note").is_some(),
        "the test must stay on the default durable library route"
    );
}

#[gpui::test]
async fn mounted_library_chrome_format_manual_sync_switch_and_reopen_round_trips_canonical_html(
    cx: &mut TestAppContext,
) {
    // The command must enter the existing Task-4 NoteSession observation
    // path: a durable canonical snapshot is required before a different
    // session decodes the note again. This rejects a library-only visual
    // toggle that never reaches EditorCore history/save coordination.
    cx.update(|app| crate::components::init(app));
    let (_profile, repository) = repository();
    let first = repository
        .create_note(CreateNote {
            title: "格式需要保存".into(),
            notebook_id: None,
            document: rich_document("可持久粗体"),
        })
        .expect("create first note");
    let second = repository
        .create_note(CreateNote {
            title: "切换目标".into(),
            notebook_id: None,
            document: rich_document("另一篇正文"),
        })
        .expect("create second note");
    let (view, cx) = mount_shell(Arc::clone(&repository), cx);
    cx.simulate_resize(gpui::size(px(1400.0), px(820.0)));
    redraw(cx);

    cx.update(|window, app| {
        view.update(app, |shell, shell_cx| {
            shell.apply_action(AppAction::SelectNote(first.id.clone()), window, shell_cx)
        });
    });
    redraw(cx);
    let editor = view.read_with(cx, |shell, app| {
        shell
            .note_session
            .as_ref()
            .expect("first session")
            .read(app)
            .editor()
            .clone()
    });
    editor.update(cx, |editor, editor_cx| {
        let block = editor.document().blocks().first().expect("text block");
        let selection = Selection::new(
            DocPoint::with_affinity(block.id, 0, Affinity::Before),
            DocPoint::with_affinity(
                block.id,
                block.content.as_text().expect("text").len(),
                Affinity::After,
            ),
        );
        editor.set_selection_for_test(selection);
        editor_cx.notify();
    });
    let bold = cx.debug_bounds("Bold").expect("shared Bold button");
    cx.simulate_click(bold.center(), Modifiers::default());
    redraw(cx);

    let save = cx
        .debug_bounds("library-sync-current")
        .expect("manual sync action");
    cx.simulate_click(save.center(), Modifiers::default());
    cx.run_until_parked();
    redraw(cx);
    let persisted = repository
        .load_note(&first.id)
        .expect("load manually synced note")
        .expect("first note remains");
    assert!(
        persisted.body_html.contains("<strong>可持久粗体</strong>"),
        "ManualSync must persist the Chrome transaction as canonical rich HTML: {}",
        persisted.body_html
    );

    // Switching away destroys the first session/chrome. Switching back must
    // construct a new shared Chrome over a freshly decoded editor rather than
    // retaining the old in-memory formatting state.
    for target in [second.id.clone(), first.id.clone()] {
        cx.update(|window, app| {
            view.update(app, |shell, shell_cx| {
                shell.apply_action(AppAction::SelectNote(target.clone()), window, shell_cx)
            });
        });
        redraw(cx);
    }
    view.read_with(cx, |shell, app| {
        let reloaded = shell
            .note_session
            .as_ref()
            .expect("reopened first session")
            .read(app)
            .editor()
            .read(app);
        assert!(
            reloaded.document().blocks()[0]
                .content
                .styles()
                .expect("reloaded text styles")
                .iter()
                .any(|run| run.marks.contains(&Mark::Bold)),
            "switch/reopen must decode the saved canonical Bold mark"
        );
        assert!(
            shell.command_chrome.is_some(),
            "the reopened session must own the same shared formatting component"
        );
    });
}

#[gpui::test]
async fn mounted_narrow_library_chrome_moves_list_command_into_shared_more_and_executes_it(
    cx: &mut TestAppContext,
) {
    // The default route must use the same placement catalogue as Spike: this
    // checks a command that becomes overflow-only at a narrow width, then
    // drives its actual More row instead of a direct catalogue call.
    cx.update(|app| crate::components::init(app));
    let (_profile, repository) = repository();
    let note = repository
        .create_note(CreateNote {
            title: "窄窗 More".into(),
            notebook_id: None,
            document: rich_document("窄宽列表"),
        })
        .expect("create note");
    let (view, cx) = mount_shell(Arc::clone(&repository), cx);
    cx.simulate_resize(gpui::size(px(400.0), px(820.0)));
    redraw(cx);
    // At this deliberately narrow app width the two library navigation panes
    // would consume the editor column entirely. A person can collapse them
    // through the existing reducer; then the measured *editor* width (not
    // the full window width) drives the shared placement calculation.
    cx.update(|window, app| {
        view.update(app, |shell, shell_cx| {
            shell.apply_action(AppAction::ToggleSidebar, window, shell_cx);
            shell.apply_action(AppAction::ToggleNoteList, window, shell_cx);
        });
    });
    redraw(cx);
    cx.update(|window, app| {
        view.update(app, |shell, shell_cx| {
            shell.apply_action(AppAction::SelectNote(note.id.clone()), window, shell_cx)
        });
    });
    redraw(cx);
    let editor = view.read_with(cx, |shell, app| {
        shell
            .note_session
            .as_ref()
            .expect("mounted session")
            .read(app)
            .editor()
            .clone()
    });
    editor.update(cx, |editor, editor_cx| {
        let block = editor.document().blocks().first().expect("text block");
        editor.set_selection_for_test(Selection::caret(DocPoint::with_affinity(
            block.id,
            0,
            Affinity::Before,
        )));
        editor_cx.notify();
    });
    assert!(
        cx.debug_bounds("Bulleted list").is_none(),
        "at 400pt the list command must leave the primary row rather than be silently dropped"
    );
    let more = cx
        .debug_bounds("library-editor-command-more-trigger")
        .expect("narrow shared More trigger");
    cx.simulate_click(more.center(), Modifiers::default());
    redraw(cx);
    let more_open = view.read_with(cx, |shell, app| {
        shell
            .command_chrome
            .as_ref()
            .expect("shared Chrome remains mounted")
            .read(app)
            .more_open_for_test()
    });
    assert!(
        more_open,
        "the actual narrow More trigger must reach the shared retained Chrome state"
    );
    assert!(
        cx.debug_bounds("library-editor-command-more-menu")
            .is_some(),
        "the library must render the shared More overlay, not a local fallback menu"
    );
    let list = cx
        .debug_bounds("Bulleted list")
        .expect("More exposes the overflow list command");
    cx.simulate_click(list.center(), Modifiers::default());
    redraw(cx);
    editor.read_with(cx, |editor, _| {
        assert!(
            matches!(
                editor.document().blocks()[0].kind,
                BlockKind::BulletItem { depth: 0 }
            ),
            "the narrow More row must execute against the current library EditorCore"
        );
    });
    view.read_with(cx, |shell, app| {
        assert!(
            !shell
                .command_chrome
                .as_ref()
                .expect("shared Chrome remains mounted")
                .read(app)
                .more_open_for_test(),
            "a successful shared More command closes the single retained overlay"
        );
    });
}

#[gpui::test]
async fn mounted_library_note_switch_discards_the_previous_shared_link_and_more_overlays(
    cx: &mut TestAppContext,
) {
    // Link and More are retained state on the shared entity. A note change
    // must remove that entity with its surface/session, otherwise a visible
    // overlay could format the old editor while the title/body show a new
    // note.
    cx.update(|app| crate::components::init(app));
    let (_profile, repository) = repository();
    let first = repository
        .create_note(CreateNote {
            title: "链接来源".into(),
            notebook_id: None,
            document: rich_document("选中链接文字"),
        })
        .expect("create first note");
    let second = repository
        .create_note(CreateNote {
            title: "切换目标".into(),
            notebook_id: None,
            document: rich_document("另一个正文"),
        })
        .expect("create second note");
    let (view, cx) = mount_shell(repository, cx);
    cx.simulate_resize(gpui::size(px(1400.0), px(820.0)));
    redraw(cx);

    let select = |id: NoteId, cx: &mut VisualTestContext| {
        cx.update(|window, app| {
            view.update(app, |shell, shell_cx| {
                shell.apply_action(AppAction::SelectNote(id), window, shell_cx)
            });
        });
        redraw(cx);
    };
    select(first.id.clone(), cx);
    view.update(cx, |shell, shell_cx| {
        let editor = shell
            .note_session
            .as_ref()
            .expect("first session")
            .read(shell_cx)
            .editor()
            .clone();
        editor.update(shell_cx, |editor, editor_cx| {
            let block = editor.document().blocks().first().expect("text block");
            editor.set_selection_for_test(Selection::new(
                DocPoint::with_affinity(block.id, 0, Affinity::Before),
                DocPoint::with_affinity(
                    block.id,
                    block.content.as_text().expect("text").len(),
                    Affinity::After,
                ),
            ));
            editor_cx.notify();
        });
        shell_cx.notify();
    });
    redraw(cx);
    let link = cx.debug_bounds("Link").expect("shared Link command");
    cx.simulate_click(link.center(), Modifiers::default());
    redraw(cx);
    view.read_with(cx, |shell, app| {
        assert!(
            shell
                .command_chrome
                .as_ref()
                .expect("shared Chrome")
                .read(app)
                .has_link_popover(),
            "the Link hit target must open state owned by the active shared Chrome"
        );
    });

    select(second.id.clone(), cx);
    view.read_with(cx, |shell, app| {
        let chrome = shell
            .command_chrome
            .as_ref()
            .expect("second Chrome")
            .read(app);
        assert!(
            !chrome.has_open_overlay() && !chrome.has_link_popover(),
            "switching notes must discard the old Link popover with its session"
        );
    });

    let more = cx
        .debug_bounds("library-editor-command-more-trigger")
        .expect("shared More trigger for second note");
    cx.simulate_click(more.center(), Modifiers::default());
    redraw(cx);
    view.read_with(cx, |shell, app| {
        assert!(
            shell
                .command_chrome
                .as_ref()
                .expect("second Chrome")
                .read(app)
                .more_open_for_test(),
            "the second session must own its own More state"
        );
    });
    select(first.id.clone(), cx);
    view.read_with(cx, |shell, app| {
        assert!(
            !shell
                .command_chrome
                .as_ref()
                .expect("reopened first Chrome")
                .read(app)
                .has_open_overlay(),
            "switching back must not resurrect the other note's More menu"
        );
    });
}

#[gpui::test]
async fn mounted_library_chrome_insert_image_event_uses_the_saved_selection_durable_picker_route(
    cx: &mut TestAppContext,
) {
    // The shared button is allowed to request an image, but it must not call
    // the spike's direct path insertion. The event subscription must first
    // capture the LibraryShell saved Selection; this test then invokes the
    // same native-picker completion seam that stages one durable resource.
    cx.update(|app| crate::components::init(app));
    let (profile, repository) = repository();
    let note = repository
        .create_note(CreateNote {
            title: "Chrome 插图".into(),
            notebook_id: None,
            document: rich_document("资源前正文"),
        })
        .expect("create note");
    let picker_path = profile.path().join("chrome-picker.png");
    std::fs::write(&picker_path, structural_png(7, 5)).expect("write picker PNG");
    let (view, cx) = mount_shell(Arc::clone(&repository), cx);
    cx.simulate_resize(gpui::size(px(1400.0), px(820.0)));
    redraw(cx);
    cx.update(|window, app| {
        view.update(app, |shell, shell_cx| {
            shell.apply_action(AppAction::SelectNote(note.id.clone()), window, shell_cx)
        });
    });
    redraw(cx);

    let insert_image = cx
        .debug_bounds("Insert image")
        .expect("shared Insert image command");
    cx.simulate_click(insert_image.center(), Modifiers::default());
    redraw(cx);
    assert!(
        view.read_with(cx, |shell, _| shell.pending_resource_insert.is_some()),
        "the typed Chrome event must synchronously capture the LibraryShell saved Selection before presenting a picker"
    );

    cx.update(|window, app| {
        view.update(app, |shell, shell_cx| {
            shell
                .complete_resource_picker_path(picker_path.clone(), window, shell_cx)
                .expect("the Chrome event must use the existing staged picker completion")
        });
    });
    cx.run_until_parked();
    redraw(cx);
    let probe = view.read_with(cx, |shell, app| shell.image_flow_probe_for_test(app));
    assert!(
        probe.has_image_block && probe.cache_has_resource,
        "the durable picker completion must update the current mounted document/cache without a note switch"
    );
    let persisted = repository
        .load_note(&note.id)
        .expect("load chrome-inserted note")
        .expect("note remains");
    assert_eq!(
        persisted.resource_ids.len(),
        1,
        "the event route must publish exactly one ordered note-resource relation"
    );
    assert!(
        persisted.body_html.contains("<img"),
        "the staged durable snapshot must contain the inserted image atom"
    );
}

#[gpui::test]
async fn mounted_library_shared_link_popover_keeps_selection_and_returns_editor_focus(
    cx: &mut TestAppContext,
) {
    // Link is the Chrome exception to ordinary editor focus. It gets a real
    // input owner temporarily, but the selected document range remains on
    // the same EditorCore and focus comes back after Apply or Cancel.
    cx.update(|app| crate::components::init(app));
    let (_profile, repository) = repository();
    let note = repository
        .create_note(CreateNote {
            title: "链接格式".into(),
            notebook_id: None,
            document: rich_document("需要链接的文字"),
        })
        .expect("create note");
    let (view, cx) = mount_shell(repository, cx);
    cx.simulate_resize(gpui::size(px(1400.0), px(820.0)));
    redraw(cx);
    cx.update(|window, app| {
        view.update(app, |shell, shell_cx| {
            shell.apply_action(AppAction::SelectNote(note.id.clone()), window, shell_cx)
        });
    });
    redraw(cx);
    let (editor, selected, history_before) = view.update(cx, |shell, shell_cx| {
        let editor = shell
            .note_session
            .as_ref()
            .expect("mounted note session")
            .read(shell_cx)
            .editor()
            .clone();
        let selected = editor.update(shell_cx, |editor, editor_cx| {
            let block = editor.document().blocks().first().expect("text block");
            let selected = Selection::new(
                DocPoint::with_affinity(block.id, 0, Affinity::Before),
                DocPoint::with_affinity(
                    block.id,
                    block.content.as_text().expect("text").len(),
                    Affinity::After,
                ),
            );
            editor.set_selection_for_test(selected);
            editor_cx.notify();
            selected
        });
        let history = editor.read(shell_cx).undo_depth();
        (editor, selected, history)
    });

    let link = cx.debug_bounds("Link").expect("shared Link button");
    cx.simulate_click(link.center(), Modifiers::default());
    redraw(cx);
    view.read_with(cx, |shell, app| {
        let chrome = shell
            .command_chrome
            .as_ref()
            .expect("shared Chrome")
            .read(app);
        assert!(chrome.has_link_popover(), "Link opens the shared popover");
        assert_eq!(
            editor.read(app).selection(),
            selected,
            "opening Link never collapses the document range"
        );
    });
    cx.update(|window, app| {
        view.read_with(app, |shell, app| {
            let popover = shell
                .command_chrome
                .as_ref()
                .expect("shared Chrome")
                .read(app)
                .link_popover_for_test()
                .expect("Link popover entity");
            assert!(
                popover.read(app).focus.is_focused(window),
                "the real URL field must own focus while it is open"
            );
        });
    });
    cx.simulate_input("https://example.com");
    redraw(cx);
    let apply = cx
        .debug_bounds("evernote-link-apply")
        .expect("shared Link Apply button");
    cx.simulate_click(apply.center(), Modifiers::default());
    redraw(cx);
    view.read_with(cx, |shell, app| {
        assert!(
            !shell
                .command_chrome
                .as_ref()
                .expect("shared Chrome")
                .read(app)
                .has_link_popover(),
            "Apply closes the shared Link popover"
        );
        let editor = editor.read(app);
        assert_eq!(
            editor.selection(),
            selected,
            "Apply restores the original document selection"
        );
        assert_eq!(
            editor.undo_depth(),
            history_before + 1,
            "Link Apply is one editor transaction"
        );
        assert!(
            editor.document().blocks()[0]
                .content
                .styles()
                .expect("link styles")
                .iter()
                .any(|run| run.marks.iter().any(|mark| {
                    matches!(mark, Mark::Link(url) if url == "https://example.com")
                })),
            "Apply mutates the active editor rather than a Chrome-local URL state"
        );
    });
    cx.update(|window, app| {
        view.read_with(app, |_shell, app| {
            assert!(
                editor.read(app).focus_handle().is_focused(window),
                "Apply returns keyboard focus to the active library editor"
            );
        });
    });
}

#[gpui::test]
async fn mounted_library_shared_more_outside_click_dismisses_without_mutating_selection_or_history(
    cx: &mut TestAppContext,
) {
    // The Library surface has a capture-phase pointer handler. The shared
    // Chrome therefore needs a real window-layer backdrop rather than merely
    // relying on the More panel's own rectangle to receive the click. If the
    // backdrop goes away, this body click collapses the range before More can
    // close and this mounted test fails.
    cx.update(|app| crate::components::init(app));
    let (_profile, repository) = repository();
    repository
        .create_note(CreateNote {
            title: "More 外点".into(),
            notebook_id: None,
            document: rich_document("保留这段选中的正文"),
        })
        .expect("create note");
    let (view, cx) = mount_shell(repository, cx);
    cx.simulate_resize(gpui::size(px(1400.0), px(820.0)));
    redraw(cx);
    let card = cx
        .debug_bounds("library-note-card")
        .expect("mounted selected-note card");
    cx.simulate_click(card.center(), Modifiers::default());
    redraw(cx);

    let (editor, selected, history_before) = view.update(cx, |shell, shell_cx| {
        let editor = shell
            .note_session
            .as_ref()
            .expect("mounted note session")
            .read(shell_cx)
            .editor()
            .clone();
        let selected = editor.update(shell_cx, |editor, editor_cx| {
            let block = editor.document().blocks().first().expect("text block");
            let selected = Selection::new(
                DocPoint::with_affinity(block.id, 0, Affinity::Before),
                DocPoint::with_affinity(
                    block.id,
                    block.content.as_text().expect("text").len(),
                    Affinity::After,
                ),
            );
            editor.set_selection_for_test(selected);
            editor_cx.notify();
            selected
        });
        let history_before = editor.read(shell_cx).undo_depth();
        (editor, selected, history_before)
    });
    let more = cx
        .debug_bounds("library-editor-command-more-trigger")
        .expect("shared More trigger");
    cx.simulate_click(more.center(), Modifiers::default());
    redraw(cx);
    assert!(
        view.read_with(cx, |shell, app| {
            shell
                .command_chrome
                .as_ref()
                .expect("shared Chrome")
                .read(app)
                .more_open_for_test()
        }),
        "the shared More overlay must be open before its outside click"
    );

    let surface = cx
        .debug_bounds("native-editor-surface")
        .expect("mounted body surface");
    let outside_menu = point(surface.left() + px(10.0), surface.bottom() - px(10.0));
    cx.simulate_click(outside_menu, Modifiers::default());
    redraw(cx);

    view.read_with(cx, |shell, app| {
        assert!(
            !shell
                .command_chrome
                .as_ref()
                .expect("shared Chrome")
                .read(app)
                .has_open_overlay(),
            "an outside body click must close More"
        );
        let editor = editor.read(app);
        assert_eq!(
            editor.selection(),
            selected,
            "the backdrop must occlude the body capture handler and preserve selection"
        );
        assert_eq!(
            editor.undo_depth(),
            history_before,
            "closing More is presentation-only and cannot add history"
        );
    });
    cx.update(|window, app| {
        assert!(
            editor.read(app).focus_handle().is_focused(window),
            "outside dismissal returns focus to the document"
        );
    });
}

#[gpui::test]
async fn mounted_library_shared_link_outside_click_dismisses_without_mutating_selection_or_history(
    cx: &mut TestAppContext,
) {
    // Link's URL field owns focus, but its transparent full-window dismissal
    // layer must still occlude the surface's capture listener. Ordinary
    // pointer hit-testing alone is insufficient because the body listener
    // runs in capture phase.
    cx.update(|app| crate::components::init(app));
    let (_profile, repository) = repository();
    repository
        .create_note(CreateNote {
            title: "链接外点".into(),
            notebook_id: None,
            document: rich_document("保留链接范围"),
        })
        .expect("create note");
    let (view, cx) = mount_shell(repository, cx);
    cx.simulate_resize(gpui::size(px(1400.0), px(820.0)));
    redraw(cx);
    let card = cx
        .debug_bounds("library-note-card")
        .expect("mounted selected-note card");
    cx.simulate_click(card.center(), Modifiers::default());
    redraw(cx);

    let (editor, selected, history_before) = view.update(cx, |shell, shell_cx| {
        let editor = shell
            .note_session
            .as_ref()
            .expect("mounted note session")
            .read(shell_cx)
            .editor()
            .clone();
        let selected = editor.update(shell_cx, |editor, editor_cx| {
            let block = editor.document().blocks().first().expect("text block");
            let selected = Selection::new(
                DocPoint::with_affinity(block.id, 0, Affinity::Before),
                DocPoint::with_affinity(
                    block.id,
                    block.content.as_text().expect("text").len(),
                    Affinity::After,
                ),
            );
            editor.set_selection_for_test(selected);
            editor_cx.notify();
            selected
        });
        let history_before = editor.read(shell_cx).undo_depth();
        (editor, selected, history_before)
    });
    let link = cx.debug_bounds("Link").expect("shared Link command");
    cx.simulate_click(link.center(), Modifiers::default());
    redraw(cx);
    assert!(
        view.read_with(cx, |shell, app| {
            shell
                .command_chrome
                .as_ref()
                .expect("shared Chrome")
                .read(app)
                .has_link_popover()
        }),
        "Link must open before testing its outside dismissal"
    );

    let surface = cx
        .debug_bounds("native-editor-surface")
        .expect("mounted body surface");
    let outside_popover = point(surface.left() + px(10.0), surface.bottom() - px(10.0));
    cx.simulate_click(outside_popover, Modifiers::default());
    redraw(cx);

    view.read_with(cx, |shell, app| {
        assert!(
            !shell
                .command_chrome
                .as_ref()
                .expect("shared Chrome")
                .read(app)
                .has_open_overlay(),
            "an outside body click must close Link"
        );
        let editor = editor.read(app);
        assert_eq!(
            editor.selection(),
            selected,
            "Link dismissal must not let the body capture handler replace the saved range"
        );
        assert_eq!(
            editor.undo_depth(),
            history_before,
            "Link dismissal cannot create a formatting transaction"
        );
    });
    cx.update(|window, app| {
        assert!(
            editor.read(app).focus_handle().is_focused(window),
            "outside Link dismissal returns focus to the document"
        );
    });
}

#[gpui::test]
async fn mounted_library_shared_chrome_escape_dismisses_more_and_link_without_editor_mutation(
    cx: &mut TestAppContext,
) {
    // Escape is routed by the host only to the shared chrome's one dismiss
    // API. This proves the More path is not a library-local menu and that
    // both overlay kinds return the same editor selection/focus/history.
    cx.update(|app| crate::components::init(app));
    let (_profile, repository) = repository();
    repository
        .create_note(CreateNote {
            title: "Escape 覆盖层".into(),
            notebook_id: None,
            document: rich_document("选择在 Escape 后仍应存在"),
        })
        .expect("create note");
    let (view, cx) = mount_shell(repository, cx);
    cx.simulate_resize(gpui::size(px(1400.0), px(820.0)));
    redraw(cx);
    let card = cx
        .debug_bounds("library-note-card")
        .expect("mounted selected-note card");
    cx.simulate_click(card.center(), Modifiers::default());
    redraw(cx);

    let (editor, selected, history_before) = view.update(cx, |shell, shell_cx| {
        let editor = shell
            .note_session
            .as_ref()
            .expect("mounted note session")
            .read(shell_cx)
            .editor()
            .clone();
        let selected = editor.update(shell_cx, |editor, editor_cx| {
            let block = editor.document().blocks().first().expect("text block");
            let selected = Selection::new(
                DocPoint::with_affinity(block.id, 0, Affinity::Before),
                DocPoint::with_affinity(
                    block.id,
                    block.content.as_text().expect("text").len(),
                    Affinity::After,
                ),
            );
            editor.set_selection_for_test(selected);
            editor_cx.notify();
            selected
        });
        let history_before = editor.read(shell_cx).undo_depth();
        (editor, selected, history_before)
    });

    let more = cx
        .debug_bounds("library-editor-command-more-trigger")
        .expect("shared More trigger");
    cx.simulate_click(more.center(), Modifiers::default());
    redraw(cx);
    cx.simulate_keystrokes("escape");
    redraw(cx);
    view.read_with(cx, |shell, app| {
        assert!(
            !shell
                .command_chrome
                .as_ref()
                .expect("shared Chrome")
                .read(app)
                .has_open_overlay(),
            "Escape must close the shared More overlay"
        );
        let editor = editor.read(app);
        assert_eq!(editor.selection(), selected, "Escape preserves selection");
        assert_eq!(
            editor.undo_depth(),
            history_before,
            "Escape adds no history"
        );
    });
    cx.update(|window, app| {
        assert!(editor.read(app).focus_handle().is_focused(window));
    });

    let link = cx.debug_bounds("Link").expect("shared Link command");
    cx.simulate_click(link.center(), Modifiers::default());
    redraw(cx);
    cx.simulate_keystrokes("escape");
    redraw(cx);
    view.read_with(cx, |shell, app| {
        assert!(
            !shell
                .command_chrome
                .as_ref()
                .expect("shared Chrome")
                .read(app)
                .has_open_overlay(),
            "Escape must close the shared Link overlay"
        );
        let editor = editor.read(app);
        assert_eq!(editor.selection(), selected, "Escape preserves selection");
        assert_eq!(
            editor.undo_depth(),
            history_before,
            "Escape adds no history"
        );
    });
    cx.update(|window, app| {
        assert!(editor.read(app).focus_handle().is_focused(window));
    });
}

#[gpui::test]
async fn mounted_library_shared_more_closes_on_real_editor_selection_change_without_history_or_link_input_corruption(
    cx: &mut TestAppContext,
) {
    // This runs through the production key route after a real More click.
    // A Chrome observer that merely redraws on EditorCore notify leaves a
    // stale menu above the new selection. Conversely, typing into the Link
    // field must not look like an EditorCore selection mutation and close the
    // pinned popover.
    cx.update(|app| crate::components::init(app));
    let (_profile, repository) = repository();
    repository
        .create_note(CreateNote {
            title: "选择变更关闭 More".into(),
            notebook_id: None,
            document: rich_document("从这里向右移动选择"),
        })
        .expect("create note");
    let (view, cx) = mount_shell(repository, cx);
    cx.simulate_resize(gpui::size(px(1400.0), px(820.0)));
    redraw(cx);
    let card = cx
        .debug_bounds("library-note-card")
        .expect("mounted selected-note card");
    cx.simulate_click(card.center(), Modifiers::default());
    redraw(cx);

    let (editor, before_selection, history_before) = view.update(cx, |shell, shell_cx| {
        let editor = shell
            .note_session
            .as_ref()
            .expect("mounted note session")
            .read(shell_cx)
            .editor()
            .clone();
        let before_selection = editor.update(shell_cx, |editor, editor_cx| {
            let block = editor.document().blocks().first().expect("text block");
            let selection =
                Selection::caret(DocPoint::with_affinity(block.id, 0, Affinity::Before));
            editor.set_selection_for_test(selection);
            editor_cx.notify();
            selection
        });
        let history_before = editor.read(shell_cx).undo_depth();
        (editor, before_selection, history_before)
    });
    let more = cx
        .debug_bounds("library-editor-command-more-trigger")
        .expect("shared More trigger");
    cx.simulate_click(more.center(), Modifiers::default());
    redraw(cx);
    assert!(
        view.read_with(cx, |shell, app| {
            shell
                .command_chrome
                .as_ref()
                .expect("shared Chrome")
                .read(app)
                .more_open_for_test()
        }),
        "More is open before the real editor navigation"
    );

    cx.simulate_keystrokes("right");
    redraw(cx);
    view.read_with(cx, |shell, app| {
        assert!(
            !shell
                .command_chrome
                .as_ref()
                .expect("shared Chrome")
                .read(app)
                .more_open_for_test(),
            "a changed EditorCore selection must dismiss stale More state"
        );
        let editor = editor.read(app);
        assert_ne!(
            editor.selection(),
            before_selection,
            "the test must reach the real surface keyboard selection route"
        );
        assert_eq!(
            editor.undo_depth(),
            history_before,
            "selection navigation and stale-menu dismissal cannot add history"
        );
    });

    // Link is intentionally disabled for a bare caret. Give its real command
    // a selected range so this test exercises the focused URL input rather
    // than treating a present-but-disabled toolbar hit target as an open
    // popover.
    let link_selection = editor.update(cx, |editor, editor_cx| {
        let block = editor.document().blocks().first().expect("text block");
        let selection = Selection::new(
            DocPoint::with_affinity(block.id, 0, Affinity::Before),
            DocPoint::with_affinity(block.id, 1, Affinity::After),
        );
        editor.set_selection_for_test(selection);
        editor_cx.notify();
        selection
    });
    redraw(cx);
    let link = cx.debug_bounds("Link").expect("shared Link command");
    cx.simulate_click(link.center(), Modifiers::default());
    redraw(cx);
    assert!(
        view.read_with(cx, |shell, app| {
            shell
                .command_chrome
                .as_ref()
                .expect("shared Chrome")
                .read(app)
                .has_link_popover()
        }),
        "the enabled Link command must open its real URL input before typing"
    );
    cx.simulate_input("https://selection-stays.example");
    redraw(cx);
    view.read_with(cx, |shell, app| {
        assert_eq!(
            editor.read(app).selection(),
            link_selection,
            "typing in Link must not mutate the saved EditorCore selection"
        );
        let chrome = shell
            .command_chrome
            .as_ref()
            .expect("shared Chrome")
            .read(app);
        assert!(
            chrome.has_link_popover(),
            "URL-field input must not falsely close the Link popover whose document selection is pinned"
        );
        let editor = editor.read(app);
        assert_ne!(editor.selection(), before_selection);
        assert_eq!(editor.undo_depth(), history_before);
    });
}

#[gpui::test]
async fn mounted_toolbar_actions_notify_the_retained_shell_and_render_failures(
    cx: &mut TestAppContext,
) {
    let (_profile, repository) = repository();
    repository
        .create_note(CreateNote {
            title: "Zulu".into(),
            notebook_id: None,
            document: rich_document("first"),
        })
        .expect("first note");
    repository
        .create_note(CreateNote {
            title: "Alpha".into(),
            notebook_id: None,
            document: rich_document("second"),
        })
        .expect("second note");
    let (view, cx) = mount_shell(repository, cx);
    redraw(cx);

    let card = cx
        .debug_bounds("library-note-card")
        .expect("mounted note card");
    cx.simulate_click(card.center(), Modifiers::default());
    redraw(cx);

    for selector in ["library-cycle-view", "library-cycle-sort"] {
        let control = cx.debug_bounds(selector).expect("toolbar control");
        cx.simulate_click(control.center(), Modifiers::default());
        redraw(cx);
    }
    view.read_with(cx, |view, cx| {
        let model = view.model.read(cx);
        assert_eq!(model.list_view_mode(), ListViewMode::Snippets);
        assert_eq!(model.sort(), crate::app::NoteSort::TitleAscending);
        assert_eq!(model.projections()[0].title_prefix, "Alpha");
    });

    for selector in ["library-toggle-sidebar", "library-toggle-list"] {
        let control = cx.debug_bounds(selector).expect("toggle control");
        cx.simulate_click(control.center(), Modifiers::default());
        redraw(cx);
    }
    assert_eq!(
        f32::from(
            cx.debug_bounds("library-sidebar")
                .expect("hidden sidebar")
                .size
                .width,
        ),
        0.0
    );
    assert_eq!(
        f32::from(
            cx.debug_bounds("library-note-list")
                .expect("hidden list")
                .size
                .width,
        ),
        0.0
    );

    let trash = cx
        .debug_bounds("library-trash-selected")
        .expect("trash control");
    cx.simulate_click(trash.center(), Modifiers::default());
    redraw(cx);
    assert_eq!(
        view.read_with(cx, |view, cx| view.model.read(cx).projections().len()),
        1
    );

    let trash = cx
        .debug_bounds("library-trash-selected")
        .expect("trash control remains mounted");
    cx.simulate_click(trash.center(), Modifiers::default());
    redraw(cx);
    assert_eq!(
        view.read_with(cx, |view, cx| view.model.read(cx).projections().len()),
        0
    );

    let trash = cx
        .debug_bounds("library-trash-selected")
        .expect("trash control remains mounted for empty-state error");
    cx.simulate_click(trash.center(), Modifiers::default());
    redraw(cx);
    assert!(
        cx.debug_bounds("library-action-error").is_some(),
        "a reducer error must be visible in the mounted library shell"
    );
}

#[gpui::test]
async fn mounted_open_request_notice_is_visible_without_opening_an_old_editor(
    cx: &mut TestAppContext,
) {
    let (_profile, repository) = repository();
    let model = cx.new(|_| AppModel::open(repository).expect("open model"));
    let (_view, cx) = cx.add_window_view(move |window, cx| {
        LibraryShell::new(
            model,
            Some("暂不支持导入 /tmp/incoming.md；原文件未被读取或修改。".into()),
            window,
            cx,
        )
    });
    redraw(cx);
    assert!(cx.debug_bounds("library-startup-notice").is_some());
    assert!(cx.debug_bounds("native-editor-surface").is_none());
}

#[gpui::test]
async fn mounted_startup_error_is_visible_when_no_library_model_can_be_opened(
    cx: &mut TestAppContext,
) {
    let (_view, cx) = cx.add_window_view(|_window, _cx| StartupErrorView {
        message: "无法打开受保护的资料库目录".into(),
    });
    redraw(cx);
    assert!(
        cx.debug_bounds("library-startup-error").is_some(),
        "fallible bootstrap failures must be rendered in a window"
    );
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
    let ids = (0..3)
        .map(|index| {
            repository
                .create_note(CreateNote {
                    title: format!("外部创建 {index}"),
                    notebook_id: None,
                    document: rich_document("事件桥不应把这段正文当作列表字段读取"),
                })
                .expect("external create")
                .id
        })
        .collect::<Vec<_>>();
    let list_query = repository.observe_next_list_query();
    let body_loads = repository.observe_note_loads();
    cx.executor().advance_clock(Duration::from_millis(60));
    cx.run_until_parked();
    redraw(cx);
    view.read_with(cx, |view, cx| {
        let model = view.model.read(cx);
        assert_eq!(model.projections().len(), 3);
        assert!(
            model.active_note().is_none(),
            "event refresh must not hydrate a body"
        );
        assert_eq!(
            model.projection_event_refreshes_for_test(),
            1,
            "a burst must make one projection refresh, not one per event"
        );
    });
    let fields = list_query.recv().expect("one coalesced projection query");
    assert!(!fields.iter().any(|field| {
        matches!(
            field.as_str(),
            "notes.body_html" | "notes.body_text" | "notes.merge_state"
        )
    }));
    assert_eq!(body_loads.try_recv(), Err(TryRecvError::Empty));

    for id in &ids {
        repository.trash_note(id).expect("external trash");
    }
    cx.executor().advance_clock(Duration::from_millis(60));
    cx.run_until_parked();
    redraw(cx);
    view.read_with(cx, |view, cx| {
        let model = view.model.read(cx);
        assert!(model.projections().is_empty());
        assert_eq!(
            model.projection_event_refreshes_for_test(),
            2,
            "the trash burst must also refresh once"
        );
    });
    assert_eq!(body_loads.try_recv(), Err(TryRecvError::Empty));
}

#[gpui::test]
async fn queued_action_event_cannot_clear_partial_create_error_before_explicit_selection_recovery(
    cx: &mut TestAppContext,
) {
    // Catches refresh_projection_events unconditionally setting Ready after
    // the create action committed but its own refresh failed. The queued
    // repository event may repair projections, but it never retried the
    // action's selection/persistence phase; a real card selection must be
    // the explicit recovery that clears the warning.
    let (_profile, repository) = repository();
    let (view, cx) = mount_shell(Arc::clone(&repository), cx);
    redraw(cx);
    view.update(cx, |view, view_cx| {
        view.model.update(view_cx, |model, _| {
            model.fail_next_refresh_for_test(app_lite_core::LibraryError::NotFound);
        });
    });

    let create = cx
        .debug_bounds("create-first-note")
        .expect("empty-library create CTA");
    cx.simulate_click(create.center(), Modifiers::default());
    redraw(cx);
    assert!(
        cx.debug_bounds("library-action-error").is_some(),
        "the action's committed-create warning must first be visible"
    );

    let created = repository
        .list_notes(Default::default())
        .expect("read committed create")
        .into_iter()
        .next()
        .expect("one committed projection")
        .id;
    cx.executor().advance_clock(Duration::from_millis(60));
    cx.run_until_parked();
    redraw(cx);
    assert!(
        cx.debug_bounds("library-action-error").is_some(),
        "a queued projection event must not erase an unresolved action warning"
    );
    view.read_with(cx, |view, cx| {
        assert!(matches!(
            view.model.read(cx).status(),
            AppStatus::Error(message) if message.contains("笔记已创建")
                && message.contains("资料库数据已提交")
        ));
        assert_eq!(view.model.read(cx).projections().len(), 1);
        assert_eq!(view.model.read(cx).projection_event_refreshes_for_test(), 1);
    });

    let card = cx
        .debug_bounds("library-note-card")
        .expect("event refresh exposes the committed card for recovery");
    cx.simulate_click(card.center(), Modifiers::default());
    redraw(cx);
    view.read_with(cx, |view, cx| {
        assert_eq!(view.model.read(cx).status(), &AppStatus::Ready);
        assert_eq!(
            view.model.read(cx).navigation().selected_note_id(),
            Some(&created)
        );
    });
}

#[gpui::test]
async fn event_bridge_task_is_cancelled_immediately_when_the_shell_is_destroyed(
    cx: &mut TestAppContext,
) {
    let (_profile, repository) = repository();
    let (view, cx) = mount_shell(repository, cx);
    redraw(cx);
    let weak_shell = view.downgrade();
    let task_cancelled = view.update(cx, |view, _| {
        view.take_event_task_cancellation_receiver_for_test()
    });

    // Removing the window drops its root. Releasing our final Entity handle
    // queues GPUI's entity-release pass; do not advance the 50ms poll timer,
    // because delayed weak-update exit is not cancellation.
    cx.update(|window, _| window.remove_window());
    drop(view);
    assert!(
        weak_shell.upgrade().is_none(),
        "the window must release its shell"
    );
    // GPUI releases zero-count entities at the next app effect boundary, then
    // async-task schedules one final cancellation runnable. Drain both without
    // advancing the polling clock.
    cx.cx.update(|_| {});
    cx.run_until_parked();
    task_cancelled
        .try_recv()
        .expect("destroying the shell must cancel the retained event task immediately");
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
        assert_eq!(surface.read(cx).mode(), EditorSurfaceMode::Editable);
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
async fn library_canvas_shapes_and_executes_the_shared_paint_entity_path(cx: &mut TestAppContext) {
    // Catches a library route that mounts the right surface chrome but skips
    // either donor shaping or render::paint_entity inside its canvas callback.
    let (_profile, repository) = repository();
    let note = repository
        .create_note(CreateNote {
            title: "真实画布".into(),
            notebook_id: None,
            document: rich_document("必须由共享画布 shape 和 paint"),
        })
        .expect("create canvas fixture");
    let (view, cx) = mount_shell(repository, cx);
    redraw(cx);
    cx.update(|window, app| {
        view.update(app, |view, view_cx| {
            view.apply_action(AppAction::SelectNote(note.id.clone()), window, view_cx)
        });
    });
    crate::native_editor::render::reset_test_render_observations();
    redraw(cx);

    let editor = view.read_with(cx, |view, cx| {
        view.editor_surface
            .as_ref()
            .expect("selection mounts library surface")
            .read(cx)
            .editor()
            .clone()
    });
    let hook_counts = view.read_with(cx, |view, _| {
        (
            view.library_surface_paint_hooks_for_test
                .shape
                .load(std::sync::atomic::Ordering::Relaxed),
            view.library_surface_paint_hooks_for_test
                .paint
                .load(std::sync::atomic::Ordering::Relaxed),
        )
    });
    let shape_calls = cx.update(|_, app| editor.read(app).shape_calls_for_test());
    assert!(
        hook_counts.0 > 0,
        "library surface before-shape hook must run"
    );
    assert!(
        hook_counts.1 > 0,
        "library surface after-paint hook must run"
    );
    assert!(
        shape_calls > 0,
        "library canvas must shape the selected document"
    );
    assert!(
        crate::native_editor::render::test_paint_entity_calls() > 0,
        "library canvas must invoke the shared paint_entity path"
    );
}

#[gpui::test]
async fn mounted_persisted_images_paint_before_only_visible_blob_hydrates(cx: &mut TestAppContext) {
    // Opening a persisted multi-image note must not synchronously stream every
    // original blob through the GPUI session. The actual shared surface first
    // paints stable image atoms, asks EditorCore for only its resident image,
    // then the retained session worker materializes that one verified source.
    // Removing the residency queue (or restoring prepare/from_prepared's eager
    // loops) makes either the offscreen source or its verified-open observer
    // appear here.
    let (_profile, repository) = repository();
    let first_bytes = structural_png(1200, 675);
    let second_bytes = structural_png(675, 1200);
    let first = repository
        .import_resource(&first_bytes, "visible.png", "image/png", "png")
        .expect("store visible image");
    let second = repository
        .import_resource(&second_bytes, "offscreen.png", "image/png", "png")
        .expect("store offscreen image");
    let first_hash = repository
        .resource_metadata(&first)
        .expect("read visible metadata")
        .expect("visible metadata")
        .sha256;
    let second_hash = repository
        .resource_metadata(&second)
        .expect("read offscreen metadata")
        .expect("offscreen metadata")
        .sha256;
    let note = repository
        .create_note(CreateNote {
            title: "可见图片按需加载".into(),
            notebook_id: None,
            document: CanonicalDocument::from_blocks(vec![
                Block::Image {
                    resource_id: first.clone(),
                    alt: "首屏".into(),
                    presentation: Default::default(),
                },
                Block::Paragraph {
                    style: BlockStyle::default(),
                    inlines: vec![Inline::Text {
                        text: "撑开首屏与下一张图片的实际正文\n".repeat(220),
                        marks: Marks::default(),
                    }],
                },
                Block::Image {
                    resource_id: second.clone(),
                    alt: "远处".into(),
                    presentation: Default::default(),
                },
            ]),
        })
        .expect("create persisted image note");
    let opens = repository.observe_verified_resource_opens();
    let (view, cx) = mount_shell(Arc::clone(&repository), cx);
    redraw(cx);
    cx.update(|window, app| {
        view.update(app, |shell, shell_cx| {
            shell.apply_action(AppAction::SelectNote(note.id.clone()), window, shell_cx)
        });
    });
    redraw(cx);
    redraw(cx);

    assert!(
        cx.debug_bounds("native-editor-surface").is_some(),
        "the note switch must paint the shared surface before any nonvisible hydration"
    );
    let (visible_materialized, offscreen_materialized) = view.read_with(cx, |shell, app| {
        let session = shell.note_session.as_ref().expect("mounted session");
        let editor = session.read(app).editor().read(app);
        (
            editor
                .image_source_path(first.as_str())
                .is_some_and(|path| path.is_file()),
            editor
                .image_source_path(second.as_str())
                .is_some_and(|path| path.is_file()),
        )
    });
    assert!(
        visible_materialized,
        "the resident first image must eventually materialize without a SelectNote/reopen"
    );
    assert!(
        !offscreen_materialized,
        "the offscreen original must remain unread and unmaterialized after the first paint"
    );
    let mut opened = Vec::new();
    loop {
        match opens.try_recv() {
            Ok(hash) => opened.push(hash),
            Err(TryRecvError::Empty | TryRecvError::Disconnected) => break,
        }
    }
    assert!(
        !opened.is_empty() && opened.iter().all(|hash| hash == &first_hash),
        "only the visible image may cross the verified descriptor boundary on first paint; opened={opened:?}"
    );
    assert!(
        !opened.iter().any(|hash| hash == &second_hash),
        "the offscreen image must not be verified/read until it becomes resident"
    );
}

#[gpui::test]
async fn mounted_missing_persisted_image_fails_only_after_paint_and_keeps_the_note_surface(
    cx: &mut TestAppContext,
) {
    // A bad persisted blob is a node-level presentation failure, not a codec
    // failure for the entire note. Bare preparation intentionally avoids the
    // descriptor; the real shared surface must request its visible atom,
    // report a visible notice, and preserve the surrounding editable body.
    let (profile, repository) = repository();
    let resource = repository
        .import_resource(
            &structural_png(800, 400),
            "missing-after-sync.png",
            "image/png",
            "png",
        )
        .expect("persist image metadata and blob");
    let note = repository
        .create_note(CreateNote {
            title: "局部图片故障".into(),
            notebook_id: None,
            document: CanonicalDocument::from_blocks(vec![
                Block::Paragraph {
                    style: BlockStyle::default(),
                    inlines: vec![Inline::Text {
                        text: "图片前文字".into(),
                        marks: Marks::default(),
                    }],
                },
                Block::Image {
                    resource_id: resource.clone(),
                    alt: "丢失的图片".into(),
                    presentation: Default::default(),
                },
                Block::Paragraph {
                    style: BlockStyle::default(),
                    inlines: vec![Inline::Text {
                        text: "图片后文字".into(),
                        marks: Marks::default(),
                    }],
                },
            ]),
        })
        .expect("create persisted note");
    let metadata = repository
        .resource_metadata(&resource)
        .expect("load metadata")
        .expect("metadata exists");
    std::fs::remove_file(
        profile
            .path()
            .join("resources/blobs")
            .join(metadata.sha256.as_str()),
    )
    .expect("simulate missing local blob");

    let (view, cx) = mount_shell(Arc::clone(&repository), cx);
    redraw(cx);
    cx.update(|window, app| {
        view.update(app, |shell, shell_cx| {
            shell.apply_action(AppAction::SelectNote(note.id.clone()), window, shell_cx)
        });
    });
    redraw(cx);
    redraw(cx);

    assert!(
        cx.debug_bounds("native-editor-surface").is_some(),
        "one missing image must never replace the full native document with unsupported UI"
    );
    let (failed_image, warning, surrounding_text) = view.read_with(cx, |shell, app| {
        let session = shell.note_session.as_ref().expect("mounted session");
        let editor = session.read(app).editor().read(app);
        (
            editor.image_state(resource.as_str())
                == Some(crate::native_editor::images::ImageNodeState::Failed),
            shell.resource_notice_for_test(),
            editor
                .document()
                .blocks()
                .iter()
                .filter_map(|block| block.content.as_text())
                .collect::<String>(),
        )
    });
    assert!(
        failed_image,
        "the requested missing atom must paint as failed"
    );
    assert!(
        warning.is_some_and(|message| message.contains("图片")),
        "the shell must surface the node-level failure instead of only logging it"
    );
    assert_eq!(surrounding_text, "图片前文字图片后文字");
}

#[gpui::test]
async fn mounted_editable_session_uses_real_title_and_body_input_then_persists(
    cx: &mut TestAppContext,
) {
    cx.update(|app| crate::components::init(app));
    let (_profile, repository) = repository();
    let stored = repository
        .create_note(CreateNote {
            title: "可编辑笔记".into(),
            notebook_id: None,
            document: CanonicalDocument::from_blocks(vec![Block::Paragraph {
                style: BlockStyle::default(),
                inlines: vec![Inline::Text {
                    text: "保留富文本样式".into(),
                    marks: Marks {
                        bold: true,
                        italic: true,
                        ..Marks::default()
                    },
                }],
            }]),
        })
        .expect("create note");
    let (view, cx) = mount_shell(Arc::clone(&repository), cx);
    redraw(cx);

    let card = cx
        .debug_bounds("library-note-card")
        .expect("mounted virtual-list card");
    cx.simulate_click(card.center(), Modifiers::default());
    redraw(cx);

    let title = cx
        .debug_bounds("library-note-title")
        .expect("mounted title EntityInputHandler canvas");
    cx.simulate_click(title.center(), Modifiers::default());
    cx.simulate_input("中文标题");
    let surface = cx
        .debug_bounds("native-editor-surface")
        .expect("mounted editable canvas");
    // Both edits pass through the production canvas/EntityInputHandler route;
    // no test-only mutation helper supplies the title or body string.
    cx.simulate_click(surface.center(), Modifiers::default());
    cx.simulate_input("中文正文");
    cx.dispatch_action(SelectAll);
    cx.dispatch_action(Copy);
    assert_eq!(
        cx.read_from_clipboard().and_then(|item| item.text()),
        Some("保留富文本样式中文正文".into()),
        "editable body keeps the mounted copy path available"
    );
    let save = cx
        .debug_bounds("library-sync-current")
        .expect("manual sync action");
    cx.simulate_click(save.center(), Modifiers::default());
    redraw(cx);

    let persisted = repository
        .load_note(&stored.id)
        .expect("load saved note")
        .expect("saved note");
    assert_eq!(persisted.title, "可编辑笔记中文标题");
    assert!(persisted.body_text.contains("中文正文"));
    let canonical = CanonicalDocument::parse_html(&persisted.body_html).expect("canonical save");
    assert!(format!("{canonical:?}").contains("bold: true"));
    assert!(format!("{canonical:?}").contains("italic: true"));
    view.read_with(cx, |view, cx| {
        let surface = view.editor_surface.as_ref().expect("mounted surface");
        assert_eq!(surface.read(cx).mode(), EditorSurfaceMode::Editable);
        assert!(
            surface
                .read(cx)
                .editor()
                .read(cx)
                .copy_all_plain_text()
                .contains("中文正文")
        );
        assert_eq!(
            view.model.read(cx).navigation().selected_note_id(),
            Some(&stored.id),
            "input/save actions must not mutate library selection"
        );
    });
}

#[gpui::test]
async fn mounted_text_image_text_uses_surface_keys_for_atomic_boundaries_and_history(
    cx: &mut TestAppContext,
) {
    // Keep this one compact, mounted matrix on the actual shared canvas. It
    // deliberately does not call the editor's delete/history methods: an
    // unbound `EditorSurface` action would leave every direct-model test
    // green while these ordinary keyboard paths remain broken.
    cx.update(|app| crate::components::init(app));
    let (_profile, repository) = repository();
    let image = structural_png(800, 400);
    let image_id = repository
        .import_resource(&image, "边界图.png", "image/png", "png")
        .expect("import durable image");
    let note = repository
        .create_note(CreateNote {
            title: "图片边界事件路径".into(),
            notebook_id: None,
            document: CanonicalDocument::from_blocks(vec![
                Block::Paragraph {
                    style: BlockStyle::default(),
                    inlines: vec![Inline::Text {
                        text: "前".into(),
                        marks: Marks::default(),
                    }],
                },
                Block::Image {
                    resource_id: image_id.clone(),
                    alt: "边界图".into(),
                    presentation: Default::default(),
                },
                Block::Paragraph {
                    style: BlockStyle::default(),
                    inlines: vec![Inline::Text {
                        text: "后".into(),
                        marks: Marks::default(),
                    }],
                },
            ]),
        })
        .expect("create text-image-text note");
    let (view, cx) = mount_shell(Arc::clone(&repository), cx);
    redraw(cx);
    cx.update(|window, app| {
        view.update(app, |shell, shell_cx| {
            shell.apply_action(AppAction::SelectNote(note.id.clone()), window, shell_cx)
        });
    });
    redraw(cx);
    let image_node = view.read_with(cx, |shell, app| {
        shell
            .note_session
            .as_ref()
            .expect("mounted session")
            .read(app)
            .editor()
            .read(app)
            .document()
            .blocks()[1]
            .id
    });

    let bounds = |cx: &mut VisualTestContext| {
        view.read_with(cx, |shell, app| {
            let session = shell.note_session.as_ref().expect("mounted session");
            let editor = session.read(app).editor().read(app);
            let blocks = editor.document().blocks();
            (
                editor
                    .layout()
                    .block_layout(blocks[0].id)
                    .expect("leading text layout")
                    .bounds,
                editor
                    .layout()
                    .block_layout(blocks[1].id)
                    .expect("image layout")
                    .bounds,
                editor
                    .layout()
                    .block_layout(blocks[2].id)
                    .expect("trailing text layout")
                    .bounds,
            )
        })
    };
    let image_count = |cx: &mut VisualTestContext| {
        view.read_with(cx, |shell, app| {
            let session = shell.note_session.as_ref().expect("mounted session");
            session
                .read(app)
                .editor()
                .read(app)
                .document()
                .blocks()
                .iter()
                .filter(|block| {
                    matches!(
                        block.content,
                        crate::native_editor::model::BlockContent::Image { .. }
                    )
                })
                .count()
        })
    };

    // Input on each real side of the atom must remain in its respective
    // text block, rather than flattening the image into a temporary UI row.
    let (leading, _, _) = bounds(cx);
    cx.simulate_click(
        point(leading.right() - px(2.0), leading.center().y),
        Modifiers::default(),
    );
    cx.simulate_input("甲");
    redraw(cx);
    let (_, _, trailing) = bounds(cx);
    cx.simulate_click(
        point(trailing.left() + px(2.0), trailing.center().y),
        Modifiers::default(),
    );
    cx.simulate_input("乙");
    redraw(cx);
    view.read_with(cx, |shell, app| {
        let session = shell.note_session.as_ref().expect("mounted session");
        let editor = session.read(app).editor().read(app);
        assert!(
            editor.document().blocks()[0]
                .content
                .as_text()
                .is_some_and(|text| text.contains('甲')),
            "left-side surface input must stay in the paragraph before the image"
        );
        assert!(
            editor.document().blocks()[2]
                .content
                .as_text()
                .is_some_and(|text| text.contains('乙')),
            "right-side surface input must stay in the paragraph after the image"
        );
    });

    // Backspace after and Delete before are two different keyboard actions
    // around an atomic block. Both must remove the same structural resource
    // and both must round-trip through the *surface* undo/redo actions.
    let (_, image_bounds, _) = bounds(cx);
    let image_hit = point(image_bounds.right() - px(2.0), image_bounds.center().y);
    view.read_with(cx, |shell, app| {
        let editor = shell
            .note_session
            .as_ref()
            .expect("mounted session")
            .read(app)
            .editor()
            .read(app);
        assert_eq!(
            editor.layout().atomic_block_at(image_hit),
            Some(image_node),
            "the stored image geometry must use the same coordinate space as EditorSurface mouse hits; image_bounds={image_bounds:?}, hit={image_hit:?}"
        );
    });
    cx.simulate_click(image_hit, Modifiers::default());
    // GPUI delivers the surface mouse handler during the next presentation
    // pass. Keep the assertion after that pass, just like the attachment
    // interaction test, so it observes the production event result rather
    // than the queued input event.
    redraw(cx);
    view.read_with(cx, |shell, app| {
        let selection = shell
            .note_session
            .as_ref()
            .expect("mounted session")
            .read(app)
            .editor()
            .read(app)
            .selection();
        assert!(
            !selection.is_caret()
                && selection.anchor.node_id == image_node
                && selection.head.node_id == image_node,
            "a click on an inline image must create one full atomic selection; selection={selection:?}, expected_image={image_node:?}"
        );
    });
    cx.simulate_keystrokes("backspace");
    redraw(cx);
    assert_eq!(
        image_count(cx),
        0,
        "Backspace after image must remove the atom"
    );
    cx.simulate_keystrokes("cmd-z");
    redraw(cx);
    assert_eq!(
        image_count(cx),
        1,
        "surface Undo restores the image relation"
    );
    cx.simulate_keystrokes("cmd-shift-z");
    redraw(cx);
    assert_eq!(image_count(cx), 0, "surface Redo reapplies atomic deletion");
    cx.simulate_keystrokes("cmd-z");
    redraw(cx);

    let (_, image_bounds, _) = bounds(cx);
    cx.simulate_click(
        point(image_bounds.left() + px(2.0), image_bounds.center().y),
        Modifiers::default(),
    );
    redraw(cx);
    cx.simulate_keystrokes("delete");
    redraw(cx);
    assert_eq!(
        image_count(cx),
        0,
        "Delete before image must remove the atom"
    );
    cx.simulate_keystrokes("cmd-z");
    redraw(cx);

    // Manual Sync gives this mounted history sequence a durable check: the
    // same image relation and text edits must survive the real save
    // coordinator rather than only the canvas's undo stack. Focused
    // EditorCore/EntityInputHandler tests cover cross-block selection and
    // IME mapping without creating a second, synthetic GPUI drag harness.
    cx.update(|window, app| {
        view.update(app, |shell, shell_cx| {
            shell.apply_action(AppAction::ManualSync, window, shell_cx);
        });
    });
    cx.run_until_parked();
    redraw(cx);
    let persisted = repository
        .load_note(&note.id)
        .expect("load mounted event-path save")
        .expect("note remains");
    assert_eq!(persisted.resource_ids, vec![image_id]);
    assert!(persisted.body_text.contains('甲'));
    assert!(persisted.body_text.contains('乙'));
}

#[gpui::test]
async fn mounted_atomic_gap_and_terminal_dead_zone_create_paragraphs_before_typing(
    cx: &mut TestAppContext,
) {
    // This drives the actual shared EditorSurface mouse route.  It catches a
    // canvas that leaves image/attachment gaps as fake after-caret positions
    // until the first input event, which produces an observable layout jump
    // and breaks adjacent IME composition.
    cx.update(|app| crate::components::init(app));
    let (_profile, repository) = repository();
    let first = repository
        .import_resource(
            b"%PDF-1.7\nfirst\n%%EOF",
            "first.pdf",
            "application/pdf",
            "pdf",
        )
        .expect("import first attachment");
    let second = repository
        .import_resource(
            b"%PDF-1.7\nsecond\n%%EOF",
            "second.pdf",
            "application/pdf",
            "pdf",
        )
        .expect("import second attachment");
    let stored = repository
        .create_note(CreateNote {
            title: "原子块间隙".into(),
            notebook_id: None,
            document: CanonicalDocument::from_blocks(vec![
                Block::Attachment {
                    resource_id: first,
                    filename: "first.pdf".into(),
                    media_type: "application/pdf".into(),
                },
                Block::Attachment {
                    resource_id: second,
                    filename: "second.pdf".into(),
                    media_type: "application/pdf".into(),
                },
            ]),
        })
        .expect("create adjacent attachment note");
    let (view, cx) = mount_shell(repository, cx);
    redraw(cx);
    let card = cx.debug_bounds("library-note-card").expect("mounted card");
    cx.simulate_click(card.center(), Modifiers::default());
    redraw(cx);

    let (first_bounds, second_bounds) = view.read_with(cx, |shell, app| {
        let session = shell.note_session.as_ref().expect("mounted session");
        let editor = session.read(app).editor().read(app);
        (
            editor
                .layout()
                .block_layout(editor.document().blocks()[0].id)
                .expect("first attachment layout")
                .bounds,
            editor
                .layout()
                .block_layout(editor.document().blocks()[1].id)
                .expect("second attachment layout")
                .bounds,
        )
    });
    // This is exactly the shared-canvas seam: no text input has happened
    // yet, and both resource cards are already painted/laid out.
    cx.simulate_click(
        point(first_bounds.left() + px(18.0), first_bounds.bottom()),
        Modifiers::default(),
    );
    redraw(cx);
    let inserted_between = view.read_with(cx, |shell, app| {
        let session = shell
            .note_session
            .as_ref()
            .expect("session remains mounted");
        let editor = session.read(app).editor().read(app);
        editor.document().blocks().len() == 3
            && matches!(
                editor.document().blocks()[1].kind,
                crate::native_editor::model::BlockKind::Paragraph
            )
            && editor.document().blocks()[1].content.as_text() == Some("")
            && editor.selection().head.node_id == editor.document().blocks()[1].id
    });
    assert!(
        inserted_between,
        "the resource gap must materialize a focused paragraph before typing"
    );

    // A click in the blank tail below the last atomic card is the same
    // semantic operation, not a later first-character fallback.
    cx.simulate_click(
        point(
            second_bounds.left() + px(18.0),
            second_bounds.bottom() + px(30.0),
        ),
        Modifiers::default(),
    );
    redraw(cx);
    view.read_with(cx, |shell, app| {
        let session = shell
            .note_session
            .as_ref()
            .expect("session remains mounted");
        let editor = session.read(app).editor().read(app);
        let tail = editor
            .document()
            .blocks()
            .last()
            .expect("terminal paragraph");
        assert_eq!(tail.kind, crate::native_editor::model::BlockKind::Paragraph);
        assert_eq!(tail.content.as_text(), Some(""));
        assert_eq!(editor.selection().head.node_id, tail.id);
        assert_eq!(shell.surface_note_id, Some(stored.id.clone()));
    });
}

#[gpui::test]
async fn mounted_editor_keeps_painting_while_a_slow_background_journal_waits(
    cx: &mut TestAppContext,
) {
    // The gate is inside the background SaveJob immediately before its
    // codec/SQLite work.  If the save path ever moves back into a foreground
    // entity callback, this mounted draw cannot make progress while the gate
    // is held.
    cx.update(|app| crate::components::init(app));
    let (_profile, repository) = repository();
    let note = repository
        .create_note(CreateNote {
            title: "慢写入仍可绘制".into(),
            notebook_id: None,
            document: rich_document("初始正文"),
        })
        .expect("create note");
    let clock = Arc::new(ManualSaveClock::default());
    let (view, cx) = mount_shell_with_save_clock(Arc::clone(&repository), Arc::clone(&clock), cx);
    redraw(cx);
    cx.update(|window, app| {
        view.update(app, |shell, shell_cx| {
            shell.apply_action(AppAction::SelectNote(note.id.clone()), window, shell_cx)
        });
    });
    redraw(cx);

    let session = view.read_with(cx, |shell, _| {
        shell
            .note_session
            .as_ref()
            .expect("mounted session")
            .clone()
    });
    let release = session.update(cx, |session, _| {
        session.enable_deadline_tasks_for_test();
        session.stall_next_background_save_for_test()
    });
    let surface = cx
        .debug_bounds("native-editor-surface")
        .expect("mounted editable canvas");
    cx.simulate_click(surface.center(), Modifiers::default());
    cx.simulate_input(" 后台慢写");

    let paints_before = view.read_with(cx, |shell, _| {
        shell
            .library_surface_paint_hooks_for_test
            .paint
            .load(std::sync::atomic::Ordering::Relaxed)
    });
    clock.advance(Duration::from_millis(100));
    cx.executor().advance_clock(Duration::from_millis(100));
    cx.run_until_parked();
    assert!(matches!(
        session.read_with(cx, |session, _| session.save_state()),
        crate::app::save_coordinator::SaveState::Journaling
    ));

    redraw(cx);
    let paints_after = view.read_with(cx, |shell, _| {
        shell
            .library_surface_paint_hooks_for_test
            .paint
            .load(std::sync::atomic::Ordering::Relaxed)
    });
    assert!(
        paints_after > paints_before,
        "a mounted editor must keep painting while worker codec/SQLite is slow"
    );
    assert!(
        repository.latest_edit_journal(&note.id).unwrap().is_none(),
        "the background gate must still be holding the journal job"
    );

    release.send(()).expect("release slow background save");
    cx.run_until_parked();
    let state_after_release = session.read_with(cx, |session, _| session.save_state());
    let journal_after_release = repository
        .latest_edit_journal(&note.id)
        .expect("read journal after release");
    let durable_after_release = repository
        .load_note(&note.id)
        .expect("read note after release")
        .expect("note after release");
    assert!(
        journal_after_release.is_some()
            || (durable_after_release.revision > note.revision
                && durable_after_release.body_text.contains("后台慢写")),
        "releasing the worker should complete a durable retained save; state={state_after_release:?}, journal={journal_after_release:?}, durable={durable_after_release:?}"
    );
}

#[gpui::test]
async fn mounted_cmd_v_after_typing_queues_saved_point_until_journal_worker_finishes(
    cx: &mut TestAppContext,
) {
    // This is the real library Paste action, not a direct EditorCore image
    // transaction. It catches the common path where Cmd-V lands immediately
    // after typing and the 100ms journal worker already owns the generation.
    cx.update(|app| crate::components::init(app));
    let (_profile, repository) = repository();
    let note = repository
        .create_note(CreateNote {
            title: "保存中的粘贴".into(),
            notebook_id: None,
            document: rich_document("正文"),
        })
        .expect("create note");
    let clock = Arc::new(ManualSaveClock::default());
    let (view, cx) = mount_shell_with_save_clock(Arc::clone(&repository), Arc::clone(&clock), cx);
    redraw(cx);
    cx.update(|window, app| {
        view.update(app, |shell, shell_cx| {
            shell.apply_action(AppAction::SelectNote(note.id.clone()), window, shell_cx)
        });
    });
    redraw(cx);

    let session = view.read_with(cx, |shell, _| {
        shell
            .note_session
            .as_ref()
            .expect("mounted session")
            .clone()
    });
    let release = session.update(cx, |session, _| {
        session.enable_deadline_tasks_for_test();
        session.stall_next_background_save_for_test()
    });
    let surface = cx
        .debug_bounds("native-editor-surface")
        .expect("mounted editable canvas");
    cx.simulate_click(surface.center(), Modifiers::default());
    cx.simulate_input(" 刚输入");
    clock.advance(Duration::from_millis(100));
    cx.executor().advance_clock(Duration::from_millis(100));
    cx.run_until_parked();
    assert!(matches!(
        session.read_with(cx, |session, _| session.save_state()),
        crate::app::save_coordinator::SaveState::Journaling
    ));

    let png = crate::native_editor::images::ClipboardPayload::fixture_with_png_and_text("")
        .images
        .into_iter()
        .next()
        .expect("PNG fixture");
    cx.write_to_clipboard(ClipboardItem::new_image(&Image::from_bytes(
        ImageFormat::Png,
        png.bytes,
    )));
    cx.dispatch_action(Paste);
    assert_eq!(
        view.read_with(cx, |shell, _| shell.queued_resource_inserts.len()),
        1,
        "Cmd-V during a writer must retain one real resource completion"
    );
    assert!(
        repository
            .load_note(&note.id)
            .expect("read base while worker gated")
            .expect("note exists")
            .resource_ids
            .is_empty(),
        "no placeholder or half-associated resource may become visible before worker completion"
    );

    // Move the live caret after the paste event. The deferred completion must
    // honor the point captured by Cmd-V, not this later user selection.
    cx.simulate_keystrokes("home");
    release.send(()).expect("release journal worker");
    cx.run_until_parked();
    redraw(cx);

    let (queued, image_after_text, document, resource_ids) = view.read_with(cx, |shell, app| {
        let session = shell
            .note_session
            .as_ref()
            .expect("session remains mounted");
        let editor = session.read(app).editor().read(app);
        (
            shell.queued_resource_inserts.len(),
            matches!(
                editor
                    .document()
                    .blocks()
                    .get(1)
                    .map(|block| &block.content),
                Some(crate::native_editor::model::BlockContent::Image { .. })
            ) && editor
                .document()
                .blocks()
                .first()
                .and_then(|block| block.content.as_text())
                .is_some_and(|text| text.contains("正文 刚输入")),
            format!("{:?}", editor.document().blocks()),
            repository
                .load_note(&note.id)
                .expect("load durable inserted note")
                .expect("note exists")
                .resource_ids,
        )
    });
    assert_eq!(queued, 0);
    assert!(
        image_after_text,
        "deferred Cmd-V must use its saved end point rather than the later Home caret: {document}"
    );
    assert_eq!(resource_ids.len(), 1);
}

#[gpui::test]
async fn mounted_finder_drop_completion_queues_through_the_same_saved_point_fence(
    cx: &mut TestAppContext,
) {
    // GPUI 0.2.2 does not expose a public non-empty ExternalPaths constructor
    // for tests. The call below is the production post-hit-test completion
    // used by on_drop, with a real file and the same captured DocPoint.
    cx.update(|app| crate::components::init(app));
    let (profile, repository) = repository();
    let note = repository
        .create_note(CreateNote {
            title: "保存中的拖放".into(),
            notebook_id: None,
            document: rich_document("拖放正文"),
        })
        .expect("create note");
    let path = profile.path().join("finder-drop.png");
    let bytes = crate::native_editor::images::ClipboardPayload::fixture_with_png_and_text("")
        .images
        .into_iter()
        .next()
        .expect("PNG fixture")
        .bytes;
    std::fs::write(&path, bytes).expect("write Finder fixture");
    let clock = Arc::new(ManualSaveClock::default());
    let (view, cx) = mount_shell_with_save_clock(Arc::clone(&repository), Arc::clone(&clock), cx);
    redraw(cx);
    cx.update(|window, app| {
        view.update(app, |shell, shell_cx| {
            shell.apply_action(AppAction::SelectNote(note.id.clone()), window, shell_cx)
        });
    });
    redraw(cx);
    let session = view.read_with(cx, |shell, _| {
        shell
            .note_session
            .as_ref()
            .expect("mounted session")
            .clone()
    });
    let release = session.update(cx, |session, _| {
        session.enable_deadline_tasks_for_test();
        session.stall_next_background_save_for_test()
    });
    let surface = cx
        .debug_bounds("native-editor-surface")
        .expect("mounted editable canvas");
    cx.simulate_click(surface.center(), Modifiers::default());
    cx.simulate_input(" 刚输入");
    let saved_drop_point = session
        .update(cx, |session, session_cx| {
            session.capture_resource_insert_intent(session_cx)
        })
        .expect("capture saved Finder-drop selection");
    clock.advance(Duration::from_millis(100));
    cx.executor().advance_clock(Duration::from_millis(100));
    cx.run_until_parked();

    cx.update(|window, app| {
        view.update(app, |shell, shell_cx| {
            shell
                .complete_resource_drop_paths(
                    vec![path.clone()],
                    saved_drop_point,
                    window,
                    shell_cx,
                )
                .expect("Finder completion must queue instead of reject during save")
        });
    });
    assert_eq!(
        view.read_with(cx, |shell, _| shell.queued_resource_inserts.len()),
        1
    );
    cx.simulate_keystrokes("home");
    release.send(()).expect("release journal worker");
    cx.run_until_parked();
    redraw(cx);

    let (image_after_text, resource_count) = view.read_with(cx, |shell, app| {
        let session = shell
            .note_session
            .as_ref()
            .expect("session remains mounted");
        let editor = session.read(app).editor().read(app);
        (
            matches!(
                editor
                    .document()
                    .blocks()
                    .get(1)
                    .map(|block| &block.content),
                Some(crate::native_editor::model::BlockContent::Image { .. })
            ) && editor
                .document()
                .blocks()
                .first()
                .and_then(|block| block.content.as_text())
                .is_some_and(|text| text.contains("拖放正文 刚输入")),
            repository
                .load_note(&note.id)
                .expect("load dropped note")
                .expect("note exists")
                .resource_ids
                .len(),
        )
    });
    assert!(image_after_text);
    assert_eq!(resource_count, 1);
}

#[gpui::test]
async fn mounted_inflight_worker_blocks_switch_delete_close_and_quit_until_completion(
    cx: &mut TestAppContext,
) {
    // Both attempts travel through the mounted lifecycle seam. The second
    // call used to see `Journaling`, return the old `last_saved`, and let a
    // close/switch/delete/quit tear down a session whose only durable write
    // was still stopped in a worker.
    cx.update(|app| crate::components::init(app));
    let (_profile, repository) = repository();
    let note = repository
        .create_note(CreateNote {
            title: "in-flight 生命周期".into(),
            notebook_id: None,
            document: rich_document("基线"),
        })
        .expect("create note");
    let other = repository
        .create_note(CreateNote {
            title: "目标笔记".into(),
            notebook_id: None,
            document: rich_document("另一篇"),
        })
        .expect("create alternate note");
    let clock = Arc::new(ManualSaveClock::default());
    let (view, cx) = mount_shell_with_save_clock(Arc::clone(&repository), Arc::clone(&clock), cx);
    redraw(cx);
    cx.update(|window, app| {
        view.update(app, |shell, shell_cx| {
            shell.apply_action(AppAction::SelectNote(note.id.clone()), window, shell_cx)
        });
    });
    redraw(cx);

    let session = view.read_with(cx, |shell, _| {
        shell
            .note_session
            .as_ref()
            .expect("mounted session")
            .clone()
    });
    let release = session.update(cx, |session, _| {
        session.enable_deadline_tasks_for_test();
        session.stall_next_background_save_for_test()
    });
    let surface = cx
        .debug_bounds("native-editor-surface")
        .expect("mounted editable surface");
    cx.simulate_click(surface.center(), Modifiers::default());
    cx.simulate_input(" 未完成保存");
    clock.advance(Duration::from_millis(100));
    cx.executor().advance_clock(Duration::from_millis(100));
    cx.run_until_parked();
    assert!(matches!(
        session.read_with(cx, |session, _| session.save_state()),
        crate::app::save_coordinator::SaveState::Journaling
    ));

    assert!(
        !view.update(cx, |shell, shell_cx| {
            shell.flush_for_lifecycle(FlushReason::WindowClose, shell_cx)
        }),
        "the first boundary queues an exact snapshot behind the journal"
    );
    let editor = session.read_with(cx, |session, _| session.editor().clone());
    cx.update(|window, app| {
        editor.update(app, |editor, editor_cx| {
            let end = editor.document().flat_utf16_len();
            <EditorCore as EntityInputHandler>::replace_text_in_range(
                editor,
                Some(end..end),
                " 后续 generation",
                window,
                editor_cx,
            );
        });
    });

    for _ in 0..2 {
        cx.update(|window, app| {
            view.update(app, |shell, shell_cx| {
                shell.apply_action(AppAction::SelectNote(other.id.clone()), window, shell_cx)
            });
        });
        assert_eq!(
            view.read_with(cx, |shell, shell_cx| {
                shell
                    .model
                    .read(shell_cx)
                    .navigation()
                    .selected_note_id()
                    .cloned()
            }),
            Some(note.id.clone()),
            "a repeated switch must not tear down the gated session"
        );
        cx.update(|window, app| {
            view.update(app, |shell, shell_cx| {
                shell.apply_action(AppAction::TrashSelected, window, shell_cx)
            });
        });
        assert!(
            repository
                .load_note(&note.id)
                .unwrap()
                .expect("first note")
                .deleted_time
                .is_none(),
            "a repeated delete must stay behind the exact save completion"
        );
        for reason in [FlushReason::WindowClose, FlushReason::Quit] {
            assert!(
                !view.update(cx, |shell, shell_cx| {
                    shell.flush_for_lifecycle(reason, shell_cx)
                }),
                "{reason:?} must remain blocked until the exact worker completion"
            );
        }
    }
    assert_eq!(
        repository
            .load_note(&note.id)
            .unwrap()
            .expect("note")
            .revision,
        note.revision,
        "a blocked lifecycle boundary must not claim the old revision is saved"
    );

    release.send(()).expect("release worker");
    cx.run_until_parked();
    assert!(view.update(cx, |shell, shell_cx| {
        shell.flush_for_lifecycle(FlushReason::WindowClose, shell_cx)
    }));
    let completion_confirmed = repository
        .load_note(&note.id)
        .unwrap()
        .expect("completion-confirmed note");
    assert!(
        completion_confirmed.body_text.contains("后续 generation"),
        "the lifecycle barrier must wait for the newest captured generation, not the old journal: {completion_confirmed:?}"
    );
}

#[gpui::test]
async fn mounted_corrected_generation_clears_only_its_automatic_save_error(
    cx: &mut TestAppContext,
) {
    // This is a mounted surface plus the real editor command path: an
    // unsupported nested list fails its automatic checkpoint, then a user
    // undo creates a newer valid generation. The old automatic warning must
    // disappear only after that newer generation has durably snapshotted.
    use crate::native_editor::commands::{CommandArgument, CommandCatalogue, EditorCommand};

    cx.update(|app| crate::components::init(app));
    let (_profile, repository) = repository();
    let note = repository
        .create_note(CreateNote {
            title: "自动错误恢复".into(),
            notebook_id: None,
            document: rich_document("正文"),
        })
        .expect("create note");
    let clock = Arc::new(ManualSaveClock::default());
    let (view, cx) = mount_shell_with_save_clock(Arc::clone(&repository), Arc::clone(&clock), cx);
    redraw(cx);
    cx.update(|window, app| {
        view.update(app, |shell, shell_cx| {
            shell.apply_action(AppAction::SelectNote(note.id.clone()), window, shell_cx)
        });
    });
    redraw(cx);
    let session = view.read_with(cx, |shell, _| {
        shell
            .note_session
            .as_ref()
            .expect("mounted session")
            .clone()
    });
    session.update(cx, |session, _| session.enable_deadline_tasks_for_test());
    let editor = session.read_with(cx, |session, _| session.editor().clone());
    editor.update(cx, |editor, editor_cx| {
        let commands = CommandCatalogue::new();
        commands
            .execute(EditorCommand::BulletList, CommandArgument::None, editor)
            .expect("real list command");
        commands
            .execute(EditorCommand::IndentList, CommandArgument::None, editor)
            .expect("real unsupported nested list command");
        editor_cx.notify();
    });
    clock.advance(Duration::from_millis(100));
    cx.executor().advance_clock(Duration::from_millis(100));
    cx.run_until_parked();
    redraw(cx);
    assert!(
        cx.debug_bounds("library-save-error").is_some(),
        "the actual codec failure must be visible"
    );

    editor.update(cx, |editor, editor_cx| {
        editor.undo().expect("undo unsupported nesting");
        editor_cx.notify();
    });
    clock.advance(Duration::from_millis(100));
    cx.executor().advance_clock(Duration::from_millis(100));
    cx.run_until_parked();
    clock.advance(Duration::from_millis(400));
    cx.executor().advance_clock(Duration::from_millis(400));
    cx.run_until_parked();
    redraw(cx);
    redraw(cx);

    let final_state = session.read_with(cx, |session, _| session.save_state());
    let final_error = view.read_with(cx, |shell, _| shell.save_error.clone());
    assert!(
        final_error.is_none(),
        "a newer successful automatic generation must clear only its own error; state={final_state:?}, error={final_error:?}"
    );
    let saved = repository.load_note(&note.id).unwrap().expect("saved note");
    assert!(saved.body_html.contains("<ul>"));
    assert_eq!(saved.revision, note.revision + 1);
}

#[gpui::test]
async fn stale_delayed_session_save_cannot_overwrite_the_newly_selected_note(
    cx: &mut TestAppContext,
) {
    // This models the hardest real ordering: A has a delayed save queued,
    // selection changes to B through the retained-model observer, and the
    // old callback wakes afterwards. A stale callback may save A's own
    // generation, but it must never borrow the newly mounted B surface.
    cx.update(|app| crate::components::init(app));
    let (_profile, repository) = repository();
    let first = repository
        .create_note(CreateNote {
            title: "会话 A".into(),
            notebook_id: None,
            document: rich_document("A 原文"),
        })
        .expect("create A");
    let second = repository
        .create_note(CreateNote {
            title: "会话 B".into(),
            notebook_id: None,
            document: rich_document("B 原文"),
        })
        .expect("create B");
    let clock = Arc::new(ManualSaveClock::default());
    let (view, cx) = mount_shell_with_save_clock(Arc::clone(&repository), Arc::clone(&clock), cx);
    redraw(cx);
    cx.update(|window, app| {
        view.update(app, |view, view_cx| {
            view.apply_action(AppAction::SelectNote(first.id.clone()), window, view_cx)
        });
    });
    redraw(cx);
    let first_surface = cx
        .debug_bounds("native-editor-surface")
        .expect("A mounts editable canvas");
    cx.simulate_click(first_surface.center(), Modifiers::default());
    cx.simulate_input(" A 延迟编辑");
    let stale_session = view.read_with(cx, |view, _| {
        view.note_session
            .as_ref()
            .expect("A session retained")
            .clone()
    });

    // Deliberately bypass the shell reducer's normal flush. This represents a
    // platform selection/update racing the already-scheduled A callback and
    // catches any implementation that makes a session consult global active
    // selection when it finally serializes.
    cx.update(|_window, app| {
        let model = view.read(app).model.clone();
        model.update(app, |model, model_cx| {
            model
                .dispatch(AppAction::SelectNote(second.id.clone()))
                .expect("select B through model seam");
            model_cx.notify();
        });
    });
    redraw(cx);
    assert_eq!(
        view.read_with(cx, |view, _| view.surface_note_id.clone()),
        Some(second.id.clone()),
        "retained observer must mount B before the stale work fires"
    );

    clock.advance(Duration::from_millis(500));
    stale_session.update(cx, |session, session_cx| {
        session
            .poll(session_cx)
            .expect("delayed A work is contained")
    });
    cx.run_until_parked();

    let saved_first = repository
        .load_note(&first.id)
        .expect("load A")
        .expect("A exists");
    let saved_second = repository
        .load_note(&second.id)
        .expect("load B")
        .expect("B exists");
    assert!(saved_first.body_text.contains("A 延迟编辑"));
    assert_eq!(saved_second.body_text, "B 原文");
    assert_eq!(saved_second.revision, second.revision);
}

#[gpui::test]
async fn mounted_window_close_flushes_the_current_edit_before_teardown(cx: &mut TestAppContext) {
    cx.update(|app| crate::components::init(app));
    let (_profile, repository) = repository();
    let note = repository
        .create_note(CreateNote {
            title: "关闭前保存".into(),
            notebook_id: None,
            document: rich_document("关闭前正文"),
        })
        .expect("create note");
    let (view, cx) = mount_shell(Arc::clone(&repository), cx);
    redraw(cx);
    cx.update(|window, app| {
        view.update(app, |view, view_cx| {
            view.apply_action(AppAction::SelectNote(note.id.clone()), window, view_cx)
        });
    });
    redraw(cx);
    let surface = cx
        .debug_bounds("native-editor-surface")
        .expect("mounted session surface");
    cx.simulate_click(surface.center(), Modifiers::default());
    cx.simulate_input(" 已编辑");

    // `simulate_close` invokes GPUI's platform should-close callback, exactly
    // like Cmd-W/the titlebar control. `remove_window` is a programmatic
    // teardown primitive and intentionally bypasses that callback.
    assert!(
        !cx.simulate_close(),
        "the first platform close must remain blocked while its real background snapshot runs"
    );
    cx.run_until_parked();
    assert!(
        cx.simulate_close(),
        "retrying the platform close after the exact worker completion permits teardown"
    );

    let saved = repository
        .load_note(&note.id)
        .expect("load after close")
        .expect("note remains");
    assert!(saved.body_text.contains("已编辑"));
}

#[gpui::test]
async fn mounted_action_boundaries_flush_before_switch_new_manual_sync_and_delete(
    cx: &mut TestAppContext,
) {
    cx.update(|app| crate::components::init(app));
    let (_profile, repository) = repository();
    let first = repository
        .create_note(CreateNote {
            title: "切换前".into(),
            notebook_id: None,
            document: rich_document("A"),
        })
        .expect("create A");
    let second = repository
        .create_note(CreateNote {
            title: "新建前".into(),
            notebook_id: None,
            document: rich_document("B"),
        })
        .expect("create B");
    let (view, cx) = mount_shell(Arc::clone(&repository), cx);
    redraw(cx);

    cx.update(|window, app| {
        view.update(app, |shell, shell_cx| {
            shell.apply_action(AppAction::SelectNote(first.id.clone()), window, shell_cx)
        });
    });
    redraw(cx);
    let first_surface = cx.debug_bounds("native-editor-surface").expect("A surface");
    cx.simulate_click(first_surface.center(), Modifiers::default());
    cx.simulate_input(" 已编辑");

    // Card/keyboard routes end here too: the shared reducer must compact A
    // before it changes retained selection to B.
    cx.update(|window, app| {
        view.update(app, |shell, shell_cx| {
            shell.apply_action(AppAction::SelectNote(second.id.clone()), window, shell_cx)
        });
    });
    assert_eq!(
        view.read_with(cx, |shell, shell_cx| {
            shell
                .model
                .read(shell_cx)
                .navigation()
                .selected_note_id()
                .cloned()
        }),
        Some(first.id.clone()),
        "the first switch click must wait for its retained background flush"
    );
    cx.run_until_parked();
    cx.update(|window, app| {
        view.update(app, |shell, shell_cx| {
            shell.apply_action(AppAction::SelectNote(second.id.clone()), window, shell_cx)
        });
    });
    redraw(cx);
    assert!(
        repository
            .load_note(&first.id)
            .expect("load A")
            .expect("A exists")
            .body_text
            .contains("已编辑")
    );

    let second_surface = cx.debug_bounds("native-editor-surface").expect("B surface");
    cx.simulate_click(second_surface.center(), Modifiers::default());
    cx.simulate_input(" 待新建");
    let create = cx
        .debug_bounds("library-create-note")
        .expect("mounted create action");
    cx.simulate_click(create.center(), Modifiers::default());
    assert_eq!(
        view.read_with(cx, |shell, shell_cx| {
            shell
                .model
                .read(shell_cx)
                .navigation()
                .selected_note_id()
                .cloned()
        }),
        Some(second.id.clone()),
        "the first create click must wait for the active note snapshot"
    );
    cx.run_until_parked();
    redraw(cx);
    let create = cx
        .debug_bounds("library-create-note")
        .expect("mounted create action after save");
    cx.simulate_click(create.center(), Modifiers::default());
    redraw(cx);
    assert!(
        repository
            .load_note(&second.id)
            .expect("load B")
            .expect("B exists")
            .body_text
            .contains("待新建")
    );

    let created_id = view.read_with(cx, |shell, shell_cx| {
        shell
            .model
            .read(shell_cx)
            .active_note()
            .expect("new note selected")
            .id
            .clone()
    });
    let created_surface = cx
        .debug_bounds("native-editor-surface")
        .expect("new note surface");
    cx.simulate_click(created_surface.center(), Modifiers::default());
    cx.simulate_input(" 手动保存后删除");
    let save = cx
        .debug_bounds("library-sync-current")
        .expect("manual-save action");
    cx.simulate_click(save.center(), Modifiers::default());
    cx.run_until_parked();
    redraw(cx);
    let save = cx
        .debug_bounds("library-sync-current")
        .expect("manual-save action after background completion");
    cx.simulate_click(save.center(), Modifiers::default());
    redraw(cx);
    assert!(
        repository
            .load_note(&created_id)
            .expect("load manually saved note")
            .expect("new note exists")
            .body_text
            .contains("手动保存后删除")
    );

    let created_surface = cx
        .debug_bounds("native-editor-surface")
        .expect("new note remains selected");
    cx.simulate_click(created_surface.center(), Modifiers::default());
    cx.simulate_input(" 删除前最后一笔");
    let trash = cx
        .debug_bounds("library-trash-selected")
        .expect("mounted trash action");
    cx.simulate_click(trash.center(), Modifiers::default());
    assert!(
        repository
            .load_note(&created_id)
            .expect("load retained note while delete saves")
            .expect("retained row")
            .deleted_time
            .is_none(),
        "the first trash click must not delete before the retained worker completes"
    );
    cx.run_until_parked();
    redraw(cx);
    let trash = cx
        .debug_bounds("library-trash-selected")
        .expect("mounted trash action after save");
    cx.simulate_click(trash.center(), Modifiers::default());
    redraw(cx);
    let deleted = repository
        .load_note(&created_id)
        .expect("load trashed note")
        .expect("trashed row retained");
    assert!(deleted.body_text.contains("删除前最后一笔"));
    assert!(deleted.deleted_time.is_some());
}

#[gpui::test]
async fn mounted_quit_lifecycle_flushes_the_current_edit_before_the_platform_request(
    cx: &mut TestAppContext,
) {
    cx.update(|app| crate::components::init(app));
    let (_profile, repository) = repository();
    let note = repository
        .create_note(CreateNote {
            title: "退出前保存".into(),
            notebook_id: None,
            document: rich_document("退出前正文"),
        })
        .expect("create note");
    let (view, cx) = mount_shell(Arc::clone(&repository), cx);
    redraw(cx);
    cx.update(|window, app| {
        view.update(app, |shell, shell_cx| {
            shell.apply_action(AppAction::SelectNote(note.id.clone()), window, shell_cx)
        });
    });
    redraw(cx);
    let surface = cx
        .debug_bounds("native-editor-surface")
        .expect("mounted session");
    cx.simulate_click(surface.center(), Modifiers::default());
    cx.simulate_input(" 已编辑");

    assert!(view.read_with(cx, |shell, shell_cx| {
        shell.note_session.as_ref().is_some_and(|session| {
            !matches!(
                session.read(shell_cx).save_state(),
                crate::app::save_coordinator::SaveState::Clean
            )
        })
    }));
    cx.cx.update(|app| {
        assert!(
            app.windows()
                .iter()
                .any(|window| window.downcast::<LibraryShell>().is_some())
        );
    });

    // Native-menu dispatch owns an App callback, not a nested window update.
    // Use the same context here so its root-window handle can be updated.
    cx.cx
        .update(|app| crate::library_menu::request_quit_library(app));
    assert!(
        !repository
            .load_note(&note.id)
            .expect("load while quit is waiting")
            .expect("note retained")
            .body_text
            .contains("已编辑"),
        "the first quit request must not claim the in-flight snapshot is durable"
    );
    cx.run_until_parked();
    cx.cx
        .update(|app| crate::library_menu::request_quit_library(app));
    assert!(
        repository
            .load_note(&note.id)
            .expect("load after quit request")
            .expect("note retained")
            .body_text
            .contains("已编辑")
    );
}

#[gpui::test]
async fn mounted_ime_lifecycle_warning_survives_clean_ticks_until_commit_and_successful_flush(
    cx: &mut TestAppContext,
) {
    // Removing the retained lifecycle-error branch in `poll_active_session`
    // used to make the next clean 50ms tick hide this blocker while macOS was
    // still presenting the candidate. This is a mounted UI test over the real
    // EntityInputHandler, not a hand-written save-state transition.
    let (_profile, repository) = repository();
    let note = repository
        .create_note(CreateNote {
            title: "组合提示".into(),
            notebook_id: None,
            document: rich_document("正文"),
        })
        .expect("create note");
    let (view, cx) = mount_shell(Arc::clone(&repository), cx);
    redraw(cx);
    cx.update(|window, app| {
        view.update(app, |shell, shell_cx| {
            shell.apply_action(AppAction::SelectNote(note.id.clone()), window, shell_cx)
        });
    });
    redraw(cx);
    let editor = view.read_with(cx, |shell, shell_cx| {
        shell
            .note_session
            .as_ref()
            .expect("retained note session")
            .read(shell_cx)
            .editor()
            .clone()
    });
    let end = cx.update(|_window, app| editor.read(app).document().flat_utf16_len());
    cx.update(|window, app| {
        editor.update(app, |editor, editor_cx| {
            <EditorCore as EntityInputHandler>::replace_and_mark_text_in_range(
                editor,
                Some(end..end),
                "候选",
                Some((end + 2)..(end + 2)),
                window,
                editor_cx,
            );
        });
    });
    assert!(!view.update(cx, |shell, shell_cx| {
        shell.flush_for_lifecycle(FlushReason::WindowClose, shell_cx)
    }));
    redraw(cx);
    assert!(cx.debug_bounds("library-save-error").is_some());

    // Drive the same automatic polling entry point that normally runs on the
    // timer. The warning must remain while composition has not resolved.
    view.update(cx, |shell, shell_cx| shell.poll_active_session(shell_cx));
    redraw(cx);
    assert!(
        cx.debug_bounds("library-save-error").is_some(),
        "a clean tick must not erase an unresolved IME lifecycle blocker"
    );

    cx.update(|window, app| {
        editor.update(app, |editor, editor_cx| {
            <EditorCore as EntityInputHandler>::unmark_text(editor, window, editor_cx);
        });
    });
    assert!(!view.update(cx, |shell, shell_cx| {
        shell.flush_for_lifecycle(FlushReason::WindowClose, shell_cx)
    }));
    cx.run_until_parked();
    assert!(view.update(cx, |shell, shell_cx| {
        shell.flush_for_lifecycle(FlushReason::WindowClose, shell_cx)
    }));
    redraw(cx);
    let warning = view.read_with(cx, |shell, _| shell.save_error.clone());
    assert!(
        warning.is_none(),
        "successful flush should clear warning: {warning:?}"
    );
    assert!(
        repository
            .load_note(&note.id)
            .unwrap()
            .expect("note")
            .body_text
            .contains("候选")
    );
}

#[gpui::test]
async fn selecting_an_unsupported_resource_note_never_reads_blob_bytes(cx: &mut TestAppContext) {
    // Catches accidental Task-5-style blob hydration while Task 3 only needs
    // metadata/canonical parsing to fail closed on image notes.
    let (_profile, repository) = repository();
    let image = repository
        .import_image(b"resource observer fixture", "image", "image/png", "png")
        .expect("create resource blob");
    let note = repository
        .create_note(CreateNote {
            title: "资源未加载".into(),
            notebook_id: None,
            document: CanonicalDocument::from_blocks(vec![Block::Paragraph {
                style: BlockStyle::default(),
                inlines: vec![Inline::Image {
                    resource_id: image,
                    alt: "Task 5 owns decode".into(),
                }],
            }]),
        })
        .expect("create unsupported note");
    let resource_reads = repository.observe_resource_reads();
    let (view, cx) = mount_shell(repository, cx);
    redraw(cx);
    cx.update(|window, app| {
        view.update(app, |view, view_cx| {
            view.apply_action(AppAction::SelectNote(note.id.clone()), window, view_cx)
        });
    });
    redraw(cx);

    assert!(cx.debug_bounds("unsupported-native-document").is_some());
    assert_eq!(resource_reads.try_recv(), Err(TryRecvError::Empty));
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
    let body_loads = repository.observe_note_loads();
    let (view, cx) = mount_shell(repository, cx);
    note_list::reset_constructed_items_for_test();
    redraw(cx);
    let initial_constructed = note_list::constructed_items_for_test();
    assert!(
        initial_constructed < 128,
        "uniform list eagerly built {initial_constructed} cards"
    );
    assert_eq!(
        body_loads.try_recv(),
        Err(TryRecvError::Empty),
        "mounting a 1,662-card list must stay a projection-only operation until a NoteId is explicitly selected"
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
    let tail_card = cx
        .debug_bounds("library-selected-note-card")
        .expect("tail card must be interactable after scroll-to-item");
    cx.simulate_click(tail_card.center(), Modifiers::default());
    redraw(cx);
    view.read_with(cx, |view, cx| {
        assert_eq!(
            view.model.read(cx).navigation().selected_note_id(),
            Some(&tail),
            "mounted tail card click must use the same NoteId reducer"
        );
    });
    assert!(
        note_list::constructed_items_for_test() < 320,
        "tail scroll built the whole library"
    );
}

#[gpui::test]
async fn restored_tail_selection_scrolls_on_its_first_mounted_draw(cx: &mut TestAppContext) {
    // Catches LibraryShell::new synchronizing its surface but forgetting the
    // deferred UniformListScrollHandle request that makes a restored tail
    // selection visible before the first user interaction.
    let (_profile, repository) = repository();
    for index in 0..1_662 {
        repository
            .create_note(CreateNote {
                title: format!("restored tail {index:04}"),
                notebook_id: None,
                document: CanonicalDocument::default(),
            })
            .expect("create restored-selection fixture");
    }
    let tail = AppModel::open(Arc::clone(&repository))
        .expect("inspect projections")
        .projections()
        .last()
        .expect("tail projection")
        .id
        .clone();
    repository
        .write_library_shell_state(&LibraryShellState {
            selected_note_id: Some(tail.clone()),
            ..LibraryShellState::default()
        })
        .expect("persist restored tail selection");

    let (view, cx) = mount_shell(repository, cx);
    note_list::reset_constructed_items_for_test();
    redraw(cx);

    view.read_with(cx, |view, cx| {
        let model = view.model.read(cx);
        let tail_index = model
            .projections()
            .iter()
            .position(|projection| projection.id == tail)
            .expect("restored note remains in projection");
        assert_eq!(model.navigation().selected_note_id(), Some(&tail));
        assert_eq!(view.last_scroll_request, Some(tail_index));
        assert!(
            view.rendered_note_range
                .as_ref()
                .is_some_and(|range| range.contains(&tail_index)),
            "first library draw must construct the restored tail card"
        );
    });
    assert!(
        note_list::constructed_items_for_test() < 160,
        "restoring a tail selection must stay virtualized"
    );
}

#[gpui::test]
async fn retained_model_observer_syncs_and_scrolls_an_independent_model_change(
    cx: &mut TestAppContext,
) {
    // Catches removal of the retained observer or its scroll side effect. This
    // intentionally bypasses LibraryShell::apply_action, so no UI callback
    // can mask a missing model-notification bridge.
    let (_profile, repository) = repository();
    let note = repository
        .create_note(CreateNote {
            title: "observer seam".into(),
            notebook_id: None,
            document: rich_document("observer must mount this body"),
        })
        .expect("create observer fixture");
    let (view, cx) = mount_shell(repository, cx);
    redraw(cx);

    cx.update(|_window, app| {
        let model = view.read(app).model.clone();
        model.update(app, |model, model_cx| {
            model
                .dispatch(AppAction::SelectNote(note.id.clone()))
                .expect("dispatch independent selection");
            model_cx.notify();
        });
    });
    redraw(cx);

    view.read_with(cx, |view, cx| {
        assert_eq!(
            view.model.read(cx).navigation().selected_note_id(),
            Some(&note.id)
        );
        assert_eq!(view.surface_note_id, Some(note.id.clone()));
        assert!(view.editor_surface.is_some());
        assert_eq!(view.last_scroll_request, Some(0));
    });
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
    let tail_card = cx
        .debug_bounds("library-selected-note-card")
        .expect("the mounted 101st card must be clickable");
    cx.simulate_click(tail_card.center(), Modifiers::default());
    redraw(cx);
    view.read_with(cx, |view, cx| {
        assert_eq!(
            view.model.read(cx).navigation().selected_note_id(),
            Some(&tail),
            "the 101st card must dispatch selection through LibraryShell"
        );
    });
}

#[gpui::test]
async fn mounted_attachment_click_selects_the_whole_card_and_double_click_opens_a_verified_copy(
    cx: &mut TestAppContext,
) {
    // This is intentionally an actual EditorSurface pointer route, not a
    // direct editor mutation. Removing the atomic hit handling or the typed
    // surface event makes the assertions below fail even though the card may
    // still paint.
    cx.update(|app| crate::components::init(app));
    let (_profile, repository) = repository();
    let resource_id = repository
        .import_resource(
            b"%PDF-1.7\nattachment opener fixture\n%%EOF",
            "证据.pdf",
            "application/pdf",
            "pdf",
        )
        .expect("import durable attachment");
    let note = repository
        .create_note(CreateNote {
            title: "附件交互".into(),
            notebook_id: None,
            document: CanonicalDocument::from_blocks(vec![Block::Attachment {
                resource_id: resource_id.clone(),
                filename: "证据.pdf".into(),
                media_type: "application/pdf".into(),
            }]),
        })
        .expect("create attachment note");
    let successor = repository
        .create_note(CreateNote {
            title: "关闭附件会话".into(),
            notebook_id: None,
            document: rich_document("successor"),
        })
        .expect("create successor note");
    let before = repository
        .load_note(&note.id)
        .expect("load before open")
        .expect("durable note before open");
    let (view, cx) = mount_shell(Arc::clone(&repository), cx);
    redraw(cx);
    cx.update(|window, app| {
        view.update(app, |shell, shell_cx| {
            shell.apply_action(AppAction::SelectNote(note.id.clone()), window, shell_cx)
        });
    });
    redraw(cx);
    let card = cx
        .debug_bounds("library-selected-note-card")
        .expect("mounted selected attachment note card");
    cx.simulate_click(card.center(), Modifiers::default());
    redraw(cx);

    let opened = Arc::new(Mutex::new(Vec::new()));
    let opener: Arc<dyn Fn(&std::path::Path) -> Result<(), String> + Send + Sync> = {
        let opened = Arc::clone(&opened);
        Arc::new(move |path| {
            opened
                .lock()
                .expect("opener result mutex")
                .push(path.to_path_buf());
            Ok(())
        })
    };
    view.update(cx, |shell, shell_cx| {
        shell.set_attachment_opener_for_test(opener, shell_cx);
        shell_cx.notify();
    });
    let (attachment_bounds, attachment_node) = view.read_with(cx, |shell, app| {
        let session = shell.note_session.as_ref().expect("mounted note session");
        let editor = session.read(app).editor().read(app);
        let block = editor
            .document()
            .blocks()
            .iter()
            .find(|block| matches!(block.content, BlockContent::Attachment { .. }))
            .expect("attachment block");
        (
            editor
                .layout()
                .block_layout(block.id)
                .expect("attachment layout")
                .bounds,
            block.id,
        )
    });
    let hit = point(
        attachment_bounds.left() + px(18.0),
        attachment_bounds.top() + px(18.0),
    );

    cx.simulate_click(hit, Modifiers::default());
    redraw(cx);
    view.read_with(cx, |shell, app| {
        let session = shell.note_session.as_ref().expect("mounted note session");
        let editor = session.read(app).editor().read(app);
        let selection = editor.selection();
        assert!(
            !selection.is_caret()
                && selection.anchor.node_id == attachment_node
                && selection.head.node_id == attachment_node,
            "a single card click must select the complete atomic attachment, not merely place a caret"
        );
    });

    // Platform double-click is a second mouse-down carrying click_count=2.
    // The surface emits a typed event; the shell then starts its retained
    // worker, so no desktop opener runs from the draw/input callback itself.
    cx.simulate_event(gpui::MouseDownEvent {
        button: gpui::MouseButton::Left,
        position: hit,
        modifiers: Modifiers::default(),
        click_count: 2,
        first_mouse: false,
    });
    cx.simulate_event(gpui::MouseUpEvent {
        button: gpui::MouseButton::Left,
        position: hit,
        modifiers: Modifiers::default(),
        click_count: 2,
    });
    cx.run_until_parked();
    redraw(cx);

    let opened = opened.lock().expect("opener result mutex");
    assert_eq!(
        opened.len(),
        1,
        "double-click must call the injected opener once"
    );
    assert_eq!(
        opened[0]
            .extension()
            .and_then(|extension| extension.to_str()),
        Some("pdf")
    );
    assert!(
        std::fs::read(&opened[0])
            .expect("verified temporary copy remains readable")
            .starts_with(b"%PDF-1.7"),
        "the opener receives a materialized, verified copy rather than a profile path"
    );
    let materialized_path = opened[0].clone();
    #[cfg(unix)]
    {
        assert_eq!(
            std::fs::metadata(materialized_path.parent().expect("attachment lease root"))
                .expect("attachment lease root metadata")
                .permissions()
                .mode()
                & 0o777,
            0o700,
            "the controlled attachment lease directory must not be traversable by other users"
        );
        assert_eq!(
            std::fs::metadata(&materialized_path)
                .expect("materialized attachment metadata")
                .permissions()
                .mode()
                & 0o777,
            0o600,
            "the opener handoff file must not become world-readable in the shared temp directory"
        );
    }
    drop(opened);
    let after = repository
        .load_note(&note.id)
        .expect("load after open")
        .expect("durable note after open");
    assert_eq!(
        after.revision, before.revision,
        "opening must not save a document mutation"
    );
    assert_eq!(
        after.body_html, before.body_html,
        "opening must not change the document"
    );

    // The opener owns its OS hand-off, but the session owns the controlled
    // cache path. Switching away drops the old editor/surface and must not
    // retain an attachment copy in the global temp parent.
    cx.update(|window, app| {
        view.update(app, |shell, shell_cx| {
            shell.apply_action(
                AppAction::SelectNote(successor.id.clone()),
                window,
                shell_cx,
            )
        });
    });
    redraw(cx);
    assert!(
        !materialized_path.exists(),
        "switching the retained session must release the attachment temporary copy"
    );
}

#[gpui::test]
async fn mounted_attachment_open_lease_survives_a_switch_until_the_worker_returns(
    cx: &mut TestAppContext,
) {
    // A session switch can happen after the verified copy is ready but before
    // the platform opener has returned. The worker, not the old editor cache,
    // owns that lease: deleting the session must neither erase the file under
    // the opener nor leave it orphaned after the weak completion can no longer
    // upgrade the old NoteSession.
    cx.update(|app| crate::components::init(app));
    let (_profile, repository) = repository();
    let resource_id = repository
        .import_resource(
            b"%PDF-1.7\nattachment handoff fixture\n%%EOF",
            "交接.pdf",
            "application/pdf",
            "pdf",
        )
        .expect("import durable attachment");
    let note = repository
        .create_note(CreateNote {
            title: "附件交接".into(),
            notebook_id: None,
            document: CanonicalDocument::from_blocks(vec![Block::Attachment {
                resource_id: resource_id.clone(),
                filename: "交接.pdf".into(),
                media_type: "application/pdf".into(),
            }]),
        })
        .expect("create attachment note");
    let successor = repository
        .create_note(CreateNote {
            title: "后继笔记".into(),
            notebook_id: None,
            document: rich_document("successor"),
        })
        .expect("create successor note");
    let (view, cx) = mount_shell(Arc::clone(&repository), cx);
    redraw(cx);
    cx.update(|window, app| {
        view.update(app, |shell, shell_cx| {
            shell.apply_action(AppAction::SelectNote(note.id.clone()), window, shell_cx)
        });
    });
    redraw(cx);

    let opener: Arc<dyn Fn(&std::path::Path) -> Result<(), String> + Send + Sync> =
        Arc::new(|_| Ok(()));
    let (release, materialized) = view.update(cx, |shell, shell_cx| {
        shell.set_attachment_opener_for_test(opener, shell_cx);
        shell.stall_next_attachment_open_for_test(shell_cx)
    });
    view.update(cx, |shell, shell_cx| {
        shell.open_attachment_resource(resource_id.as_str().to_owned(), shell_cx);
    });
    cx.run_until_parked();
    let materialized_path = materialized
        .recv_timeout(Duration::from_secs(1))
        .expect("the retained worker must expose its verified handoff only after materializing it");
    assert!(
        materialized_path.is_file(),
        "the worker-owned handoff must remain readable while the opener is gated"
    );
    #[cfg(unix)]
    {
        assert_eq!(
            std::fs::metadata(materialized_path.parent().expect("lease root"))
                .expect("lease root metadata")
                .permissions()
                .mode()
                & 0o777,
            0o700,
            "the independent worker lease root must be private"
        );
        assert_eq!(
            std::fs::metadata(&materialized_path)
                .expect("lease leaf metadata")
                .permissions()
                .mode()
                & 0o777,
            0o600,
            "the independent worker lease leaf must be private"
        );
    }

    // This is deliberately a normal reducer-driven note switch rather than a
    // direct entity release. It catches a session-owned image-cache root being
    // removed while the asynchronous platform handoff still owns the file.
    cx.update(|window, app| {
        view.update(app, |shell, shell_cx| {
            shell.apply_action(
                AppAction::SelectNote(successor.id.clone()),
                window,
                shell_cx,
            )
        });
    });
    redraw(cx);
    assert!(
        materialized_path.is_file(),
        "switching notes before opener return must not delete the worker-owned handoff"
    );

    release
        .send(())
        .expect("release attachment opener worker after switch");
    cx.run_until_parked();
    redraw(cx);
    assert!(
        !materialized_path.exists(),
        "a completion whose old session is gone must release its handoff lease instead of orphaning it"
    );
}

#[gpui::test]
async fn mounted_attachment_open_failure_is_visible_without_mutating_the_document(
    cx: &mut TestAppContext,
) {
    cx.update(|app| crate::components::init(app));
    let (_profile, repository) = repository();
    let resource_id = repository
        .import_resource(
            b"%PDF-1.7\nfailed opener fixture\n%%EOF",
            "失败.pdf",
            "application/pdf",
            "pdf",
        )
        .expect("import durable attachment");
    let note = repository
        .create_note(CreateNote {
            title: "附件打开失败".into(),
            notebook_id: None,
            document: CanonicalDocument::from_blocks(vec![Block::Attachment {
                resource_id,
                filename: "失败.pdf".into(),
                media_type: "application/pdf".into(),
            }]),
        })
        .expect("create attachment note");
    let before = repository
        .load_note(&note.id)
        .expect("load before failed open")
        .expect("durable note before failed open");
    let (view, cx) = mount_shell(Arc::clone(&repository), cx);
    redraw(cx);
    let card = cx
        .debug_bounds("library-note-card")
        .expect("mounted attachment note card");
    cx.simulate_click(card.center(), Modifiers::default());
    redraw(cx);
    let failed_handoff_path = Arc::new(Mutex::new(None));
    let failing_opener: Arc<dyn Fn(&std::path::Path) -> Result<(), String> + Send + Sync> = {
        let failed_handoff_path = Arc::clone(&failed_handoff_path);
        Arc::new(move |path| {
            *failed_handoff_path
                .lock()
                .expect("failed handoff path mutex") = Some(path.to_path_buf());
            Err("系统默认应用拒绝打开测试附件".to_owned())
        })
    };
    view.update(cx, |shell, shell_cx| {
        shell.set_attachment_opener_for_test(failing_opener, shell_cx);
        shell_cx.notify();
    });
    let attachment_bounds = view.read_with(cx, |shell, app| {
        let session = shell.note_session.as_ref().expect("mounted note session");
        let editor = session.read(app).editor().read(app);
        let block = editor
            .document()
            .blocks()
            .iter()
            .find(|block| matches!(block.content, BlockContent::Attachment { .. }))
            .expect("attachment block");
        editor
            .layout()
            .block_layout(block.id)
            .expect("attachment layout")
            .bounds
    });
    let hit = point(
        attachment_bounds.left() + px(18.0),
        attachment_bounds.top() + px(18.0),
    );
    cx.simulate_event(gpui::MouseDownEvent {
        button: gpui::MouseButton::Left,
        position: hit,
        modifiers: Modifiers::default(),
        click_count: 2,
        first_mouse: false,
    });
    cx.simulate_event(gpui::MouseUpEvent {
        button: gpui::MouseButton::Left,
        position: hit,
        modifiers: Modifiers::default(),
        click_count: 2,
    });
    cx.run_until_parked();
    redraw(cx);
    assert!(
        cx.debug_bounds("library-resource-notice").is_some(),
        "open failure must become visible feedback rather than a stderr-only error"
    );
    let failed_handoff_path = failed_handoff_path
        .lock()
        .expect("failed handoff path mutex")
        .clone()
        .expect("the failing opener must receive the verified private handoff");
    assert!(
        !failed_handoff_path.exists(),
        "a nonzero/injected opener failure must drop the worker lease instead of leaving a readable handoff behind"
    );
    let after = repository
        .load_note(&note.id)
        .expect("load after failed open")
        .expect("durable note after failed open");
    assert_eq!(after.revision, before.revision);
    assert_eq!(after.body_html, before.body_html);
}
