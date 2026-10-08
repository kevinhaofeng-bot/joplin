use super::*;
use crate::app::AppAction;
use crate::native_editor::model::BlockContent;
use app_lite_core::document::{Block, BlockStyle, ImagePresentation, Inline, Marks};
use app_lite_core::{CanonicalDocument, CreateNote, LibraryRepository};
use gpui::{
    AppContext, Bounds, Entity, Modifiers, MouseButton, MouseDownEvent, MouseUpEvent, ScrollDelta,
    ScrollWheelEvent, TestAppContext, VisualTestContext, WindowBounds, WindowOptions, point, px,
    size,
};
use std::sync::{Arc, Mutex};

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

/// Build the production LibraryShell at the release-sized regression frame,
/// rather than using TestAppContext's maximized 1920x1080 convenience window.
fn mount_shell_at_900x700(
    repository: Arc<LibraryRepository>,
    cx: &mut TestAppContext,
) -> (Entity<LibraryShell>, VisualTestContext) {
    mount_shell_at_frame(repository, 900.0, 700.0, cx)
}

fn mount_shell_at_frame(
    repository: Arc<LibraryRepository>,
    width: f32,
    height: f32,
    cx: &mut TestAppContext,
) -> (Entity<LibraryShell>, VisualTestContext) {
    let model = cx.new(|_| AppModel::open(repository).expect("open model"));
    let window = cx.update(|app| {
        let bounds = Bounds::centered(None, size(px(width), px(height)), app);
        app.open_window(
            WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(bounds)),
                ..WindowOptions::default()
            },
            move |window, cx| cx.new(move |cx| LibraryShell::new(model, None, window, cx)),
        )
        .expect("open 900x700 LibraryShell")
    });
    let view = window.root(cx).expect("read LibraryShell root");
    let visual = VisualTestContext::from_window(window.into(), cx);
    visual.run_until_parked();
    (view, visual)
}

fn tiny_png() -> Vec<u8> {
    let mut encoded = std::io::Cursor::new(Vec::new());
    image::DynamicImage::ImageRgba8(image::RgbaImage::from_pixel(
        1,
        1,
        image::Rgba([0x3a, 0x85, 0x55, 0xff]),
    ))
    .write_to(&mut encoded, image::ImageFormat::Png)
    .expect("encode image fixture");
    encoded.into_inner()
}

#[gpui::test]
async fn mounted_typing_after_large_image_reveals_new_paragraph(cx: &mut TestAppContext) {
    assert_typing_after_large_image_reveals_new_paragraph(cx, 900.0, 700.0);
}

#[gpui::test]
async fn mounted_typing_after_large_image_reveals_unshaped_paragraph_in_wide_window(cx: &mut TestAppContext) {
    // The source image is capped at its natural width. Keep the viewport
    // shallow enough that its tail is also outside the one-viewport prefetch.
    assert_typing_after_large_image_reveals_new_paragraph(cx, 2320.0, 700.0);
}

fn assert_typing_after_large_image_reveals_new_paragraph(cx: &mut TestAppContext, width: f32, height: f32) {
    // Return/input must reveal their own selection, without a subsequent
    // navigation command, wheel, or reopening the note doing it for them.
    cx.update(|app| crate::components::init(app));
    let (_profile, repository) = repository();
    let resource_id = repository
        .import_resource(
            include_bytes!("../../assets/AppIcon-dock-v2.png"),
            "caret.png", "image/png", "png",
        )
        .unwrap();
    let note = repository.create_note(CreateNote {
        title: "图片后输入必须滚入".into(),
        notebook_id: None,
        document: CanonicalDocument::from_blocks(vec![
            Block::Paragraph {
                style: BlockStyle::default(),
                inlines: vec![Inline::Text { text: "图前".into(), marks: Marks::default() }],
            },
            Block::Image {
                resource_id,
                alt: "caret fixture".into(),
                presentation: ImagePresentation {
                    natural_size: Some((1254, 1254)), display_width: None,
                    alignment: None,
                },
                link: None,
            },
        ]),
    }).unwrap();
    let (view, mut cx) = mount_shell_at_frame(repository, width, height, cx);
    cx.update(|window, app| view.update(app, |shell, shell_cx| {
        shell.apply_action(AppAction::SelectNote(note.id.clone()), window, shell_cx);
        shell.apply_action(AppAction::ToggleSidebar, window, shell_cx);
        shell.apply_action(AppAction::ToggleNoteList, window, shell_cx);
    }));
    redraw(&mut cx);
    let editor = view.read_with(&cx, |shell, app| {
        shell.note_session.as_ref().unwrap().read(app).editor().clone()
    });
    cx.update(|window, app| {
        editor.update(app, |editor, editor_cx| {
            let end = editor.document_len();
            editor.select_document_range(end, end);
            editor_cx.notify();
        });
        crate::native_editor::surface::focus_editor(&editor, window, app);
    });
    redraw(&mut cx);
    cx.simulate_keystrokes("enter");
    redraw(&mut cx);
    cx.simulate_input("你好");
    for _ in 0..6 { redraw(&mut cx); }
    let (head_text, caret) = editor.read_with(&cx, |editor, _| {
        (
            editor.document().block(editor.selection().head.node_id)
                .unwrap().content.as_text().unwrap().to_owned(),
            editor.layout().caret_bounds_for_point(editor.selection().head),
        )
    });
    assert_eq!(head_text, "你好", "Return/input must actually edit a new image-tail paragraph");
    let metrics = view.read_with(&cx, |shell, app| {
        shell.editor_surface.as_ref().unwrap().read(app).scroll_metrics_for_test()
    });
    assert!(metrics.max_offset.height > px(200.0), "image fixture must overflow");
    assert!(metrics.offset.y < px(-200.0), "typing after the image must scroll automatically: {metrics:?}, caret={caret:?}");
    let caret = caret.expect("the input selection must be shaped after reveal");
    assert!(caret.top() >= metrics.viewport.top() && caret.bottom() <= metrics.viewport.bottom(),
        "typed caret must stay fully inside the body viewport: {metrics:?}, caret={caret:?}");
}

#[gpui::test]
async fn mounted_wheel_paints_text_entering_view_between_large_images(cx: &mut TestAppContext) {
    let _paint_scope = crate::native_editor::render::observe_test_text_paints();
    // A wheel must repaint text newly entering the clip without a selection
    // change. Merely retaining the text in SQLite/layout is not acceptance.
    cx.update(|app| crate::components::init(app));
    let (_profile, repository) = repository();
    let resource_id = repository
        .import_resource(
            include_bytes!("../../assets/AppIcon-dock-v2.png"),
            "wheel.png",
            "image/png",
            "png",
        )
        .unwrap();
    let paragraph = |text: &str| Block::Paragraph {
        style: BlockStyle::default(),
        inlines: vec![Inline::Text {
            text: text.into(),
            marks: Marks::default(),
        }],
    };
    let image = || Block::Image {
        resource_id: resource_id.clone(),
        alt: "scroll fixture".into(),
        presentation: ImagePresentation {
            natural_size: Some((1254, 1254)),
            display_width: None,
            alignment: None,
        },
        link: None,
    };
    let note = repository
        .create_note(CreateNote {
            title: "滚轮图间文字".into(),
            notebook_id: None,
            document: CanonicalDocument::from_blocks(vec![
                paragraph("图前"),
                image(),
                paragraph("中文测试"),
                paragraph("你好图片"),
                image(),
                image(),
                paragraph("世界"),
            ]),
        })
        .unwrap();
    let before = repository.load_note(&note.id).unwrap().unwrap();
    let (view, mut cx) = mount_shell_at_900x700(Arc::clone(&repository), cx);
    cx.update(|window, app| {
        view.update(app, |shell, shell_cx| {
            shell.apply_action(AppAction::SelectNote(note.id.clone()), window, shell_cx);
            shell.apply_action(AppAction::ToggleSidebar, window, shell_cx);
            shell.apply_action(AppAction::ToggleNoteList, window, shell_cx);
        })
    });
    redraw(&mut cx);
    let editor = view.read_with(&cx, |shell, app| {
        shell
            .note_session
            .as_ref()
            .unwrap()
            .read(app)
            .editor()
            .clone()
    });
    let between_nodes = editor.read_with(&cx, |editor, _| {
        editor
            .document()
            .blocks()
            .iter()
            .filter_map(|block| {
                matches!(block.content.as_text(), Some("中文测试" | "你好图片")).then_some(block.id)
            })
            .collect::<Vec<_>>()
    });
    assert_eq!(between_nodes.len(), 2);
    let initial = view.read_with(&cx, |shell, app| {
        shell
            .editor_surface
            .as_ref()
            .unwrap()
            .read(app)
            .scroll_metrics_for_test()
    });
    let mut checked = false;
    for _ in 0..12 {
        crate::native_editor::render::take_test_text_paints();
        cx.simulate_event(ScrollWheelEvent {
            position: initial.viewport.center(),
            delta: ScrollDelta::Pixels(point(px(0.0), px(-120.0))),
            ..Default::default()
        });
        redraw(&mut cx);
        let metrics = view.read_with(&cx, |shell, app| {
            shell
                .editor_surface
                .as_ref()
                .unwrap()
                .read(app)
                .scroll_metrics_for_test()
        });
        let bounds = editor.read_with(&cx, |editor, _| {
            between_nodes
                .iter()
                .filter_map(|id| {
                    editor
                        .layout()
                        .block_layout(*id)
                        .map(|layout| layout.bounds)
                })
                .collect::<Vec<_>>()
        });
        if bounds.len() != 2
            || bounds
                .iter()
                .any(|b| b.top() < metrics.viewport.top() || b.bottom() > metrics.viewport.bottom())
        {
            continue;
        }
        let paints = crate::native_editor::render::take_test_text_paints();
        for expected in ["中文测试", "你好图片"] {
            assert!(
                paints.iter().any(|paint| {
                    paint.editor == editor.entity_id()
                        && paint.text == expected
                        && paint.bounds.top() >= paint.content_mask.top()
                        && paint.bounds.bottom() <= paint.content_mask.bottom()
                }),
                "newly visible text must reach the glyph painter after wheel alone: expected={expected}, metrics={metrics:?}, layout={bounds:?}, paints={paints:?}"
            );
        }
        assert!(metrics.offset.y < px(0.0), "fixture must really scroll");
        checked = true;
        break;
    }
    assert!(
        checked,
        "wheel fixture must reach both intervening paragraphs"
    );
    let after = repository.load_note(&note.id).unwrap().unwrap();
    assert_eq!(after.body_html, before.body_html);
    assert_eq!(after.revision, before.revision);
}

#[gpui::test]
async fn mounted_title_tab_reveals_body_start_after_scrolling_to_the_end(cx: &mut TestAppContext) {
    cx.update(|app| crate::components::init(app));
    let (_profile, repository) = repository();
    let note = repository
        .create_note(CreateNote {
            title: "标题Tab长文165".into(),
            notebook_id: None,
            document: CanonicalDocument::from_blocks(
                (0..100)
                    .map(|index| Block::Paragraph {
                        style: BlockStyle::default(),
                        inlines: vec![Inline::Text {
                            text: format!("长文段落{index} 原文"),
                            marks: Marks::default(),
                        }],
                    })
                    .collect(),
            ),
        })
        .unwrap();
    let (view, mut cx) = mount_shell_at_900x700(repository, cx);
    cx.update(|window, app| {
        view.update(app, |shell, shell_cx| {
            shell.apply_action(AppAction::SelectNote(note.id.clone()), window, shell_cx)
        })
    });
    redraw(&mut cx);
    let editor = view.read_with(&cx, |shell, app| {
        shell
            .note_session
            .as_ref()
            .unwrap()
            .read(app)
            .editor()
            .clone()
    });
    cx.update(|window, app| crate::native_editor::surface::focus_editor(&editor, window, app));
    cx.simulate_keystrokes("cmd-down");
    redraw(&mut cx);
    let offset = view.read_with(&cx, |shell, app| {
        shell
            .editor_surface
            .as_ref()
            .unwrap()
            .read(app)
            .scroll_metrics_for_test()
            .offset
            .y
    });
    assert!(
        offset < px(-200.0),
        "fixture must really be scrolled below the first paragraph"
    );
    let title = cx.debug_bounds("library-note-title").unwrap();
    cx.simulate_click(title.center(), Modifiers::default());
    cx.simulate_keystrokes("tab");
    redraw(&mut cx);
    let metrics = view.read_with(&cx, |shell, app| {
        shell
            .editor_surface
            .as_ref()
            .unwrap()
            .read(app)
            .scroll_metrics_for_test()
    });
    assert_eq!(
        metrics.offset.y,
        px(0.0),
        "title Tab must reveal the body start immediately, before typing"
    );
    cx.simulate_input("X");
    redraw(&mut cx);
    assert!(
        editor
            .read_with(&cx, |editor, _| editor.copy_all_plain_text())
            .starts_with("X长文段落0 原文")
    );
}

#[gpui::test]
async fn mounted_arrow_down_keeps_ordinary_text_selection_authoritative_before_atomic_fallback(
    cx: &mut TestAppContext,
) {
    // This is deliberately a long *text* document, not an image fixture. It
    // catches a regression where the new atomic-boundary fallback scrolls on
    // every ArrowDown and takes over ordinary vertical caret movement.
    cx.update(|app| crate::components::init(app));
    let (_profile, repository) = repository();
    repository
        .create_note(CreateNote {
            title: "普通文本向下移动".into(),
            notebook_id: None,
            document: CanonicalDocument::from_blocks(vec![Block::Paragraph {
                style: BlockStyle::default(),
                inlines: vec![Inline::Text {
                    text: "正常文本行 ".repeat(600),
                    marks: Marks::default(),
                }],
            }]),
        })
        .expect("create text navigation fixture");
    let (view, mut cx) = mount_shell_at_900x700(Arc::clone(&repository), cx);
    redraw(&mut cx);
    let card = cx
        .debug_bounds("library-note-card")
        .expect("text fixture is listed in the production shell");
    cx.simulate_click(card.center(), Modifiers::default());
    redraw(&mut cx);
    cx.update(|window, app| {
        view.update(app, |shell, shell_cx| {
            shell.apply_action(AppAction::ToggleSidebar, window, shell_cx);
            shell.apply_action(AppAction::ToggleNoteList, window, shell_cx);
        });
    });
    redraw(&mut cx);

    let (text_bounds, metrics_before_focus) = view.read_with(&cx, |shell, app| {
        let surface = shell
            .editor_surface
            .as_ref()
            .expect("text note mounts surface")
            .read(app);
        let session = shell
            .note_session
            .as_ref()
            .expect("text note mounts session");
        let editor = session.read(app).editor().read(app);
        (
            editor
                .layout()
                .block_layout(editor.document().blocks()[0].id)
                .expect("large text paragraph is shaped")
                .bounds,
            surface.scroll_metrics_for_test(),
        )
    });
    assert!(
        metrics_before_focus.max_offset.height > px(0.0),
        "the text fixture must have a real scroll range so unconditional fallback scrolling is observable"
    );
    cx.simulate_click(
        point(text_bounds.left() + px(24.0), text_bounds.top() + px(14.0)),
        Modifiers::default(),
    );
    redraw(&mut cx);
    let (selection_before, offset_before) = view.read_with(&cx, |shell, app| {
        let surface = shell
            .editor_surface
            .as_ref()
            .expect("text surface remains mounted")
            .read(app);
        let editor = shell
            .note_session
            .as_ref()
            .expect("text session remains mounted")
            .read(app)
            .editor()
            .read(app);
        (editor.selection(), surface.scroll_metrics_for_test().offset)
    });
    cx.simulate_keystrokes("down");
    redraw(&mut cx);
    let (selection_after, offset_after) = view.read_with(&cx, |shell, app| {
        let surface = shell
            .editor_surface
            .as_ref()
            .expect("text surface remains mounted")
            .read(app);
        let editor = shell
            .note_session
            .as_ref()
            .expect("text session remains mounted")
            .read(app)
            .editor()
            .read(app);
        (editor.selection(), surface.scroll_metrics_for_test().offset)
    });
    assert_ne!(
        selection_after, selection_before,
        "ordinary text ArrowDown must keep EditorCore's vertical selection movement"
    );
    assert_eq!(
        offset_after, offset_before,
        "a successful ordinary-text selection movement must not take the atomic-boundary scroll fallback"
    );
}

#[gpui::test]
async fn mounted_long_text_vertical_noops_do_not_move_the_scroll_viewport(cx: &mut TestAppContext) {
    // Regression for the first scroll repair: `EditorCore::move_vertical`
    // deliberately leaves Selection unchanged at ordinary first/last text
    // lines. A surface must not mistake that normal no-op for an image/file
    // boundary and move the viewport underneath a stable caret.
    cx.update(|app| crate::components::init(app));
    let (_profile, repository) = repository();
    let mut blocks = vec![Block::Paragraph {
        style: BlockStyle::default(),
        inlines: vec![Inline::Text {
            text: "首行".into(),
            marks: Marks::default(),
        }],
    }];
    for index in 0..96 {
        blocks.push(Block::Paragraph {
            style: BlockStyle::default(),
            inlines: vec![Inline::Text {
                text: format!("中间正文第 {index} 行，保持普通文本导航。"),
                marks: Marks::default(),
            }],
        });
    }
    blocks.push(Block::Paragraph {
        style: BlockStyle::default(),
        inlines: vec![Inline::Text {
            text: "末行".into(),
            marks: Marks::default(),
        }],
    });
    repository
        .create_note(CreateNote {
            title: "普通长文首尾导航".into(),
            notebook_id: None,
            document: CanonicalDocument::from_blocks(blocks),
        })
        .expect("create long text boundary fixture");
    let (view, mut cx) = mount_shell_at_900x700(Arc::clone(&repository), cx);
    redraw(&mut cx);
    let card = cx
        .debug_bounds("library-note-card")
        .expect("long text fixture is listed in the production shell");
    cx.simulate_click(card.center(), Modifiers::default());
    redraw(&mut cx);
    cx.update(|window, app| {
        view.update(app, |shell, shell_cx| {
            shell.apply_action(AppAction::ToggleSidebar, window, shell_cx);
            shell.apply_action(AppAction::ToggleNoteList, window, shell_cx);
        });
    });
    redraw(&mut cx);

    let (first_bounds, metrics_at_top) = view.read_with(&cx, |shell, app| {
        let surface = shell
            .editor_surface
            .as_ref()
            .expect("long text mounts surface")
            .read(app);
        let editor = shell
            .note_session
            .as_ref()
            .expect("long text mounts session")
            .read(app)
            .editor()
            .read(app);
        (
            editor
                .layout()
                .block_layout(editor.document().blocks()[0].id)
                .expect("first text block is shaped")
                .bounds,
            surface.scroll_metrics_for_test(),
        )
    });
    assert!(
        metrics_at_top.max_offset.height > px(0.0),
        "the fixture needs a real scroll range so a false fallback is observable"
    );
    cx.simulate_click(
        point(first_bounds.left() + px(18.0), first_bounds.center().y),
        Modifiers::default(),
    );
    cx.simulate_keystrokes("home");
    redraw(&mut cx);
    let top_wheel_point = point(
        metrics_at_top.viewport.left() + px(18.0),
        metrics_at_top.viewport.top() + px(18.0),
    );
    cx.simulate_event(ScrollWheelEvent {
        position: top_wheel_point,
        delta: ScrollDelta::Pixels(point(px(0.0), px(-180.0))),
        ..Default::default()
    });
    redraw(&mut cx);
    let (top_selection_before, top_metrics_before) = view.read_with(&cx, |shell, app| {
        let surface = shell
            .editor_surface
            .as_ref()
            .expect("surface remains mounted")
            .read(app);
        let editor = shell
            .note_session
            .as_ref()
            .expect("session remains mounted")
            .read(app)
            .editor()
            .read(app);
        (editor.selection(), surface.scroll_metrics_for_test())
    });
    assert!(
        top_metrics_before.offset.y < px(0.0)
            && top_metrics_before.offset.y > -top_metrics_before.max_offset.height,
        "move the viewport away from its clamp before testing a top text no-op: {top_metrics_before:?}"
    );
    cx.simulate_keystrokes("up");
    redraw(&mut cx);
    let (top_selection_after, top_metrics_after) = view.read_with(&cx, |shell, app| {
        let surface = shell
            .editor_surface
            .as_ref()
            .expect("surface remains mounted")
            .read(app);
        let editor = shell
            .note_session
            .as_ref()
            .expect("session remains mounted")
            .read(app)
            .editor()
            .read(app);
        (editor.selection(), surface.scroll_metrics_for_test())
    });
    assert_eq!(
        top_selection_after, top_selection_before,
        "ArrowUp at the ordinary first text line is a Core no-op"
    );
    assert_eq!(
        top_metrics_after.offset, top_metrics_before.offset,
        "an ordinary first-text no-op must not invoke the atomic scroll fallback"
    );

    for _ in 0..32 {
        cx.simulate_keystrokes("pagedown");
        redraw(&mut cx);
    }
    let (last_bounds, bottom_metrics_at_max) = view.read_with(&cx, |shell, app| {
        let surface = shell
            .editor_surface
            .as_ref()
            .expect("surface remains mounted")
            .read(app);
        let editor = shell
            .note_session
            .as_ref()
            .expect("session remains mounted")
            .read(app)
            .editor()
            .read(app);
        let last = editor
            .document()
            .blocks()
            .last()
            .expect("last text block remains in document");
        (
            editor
                .layout()
                .block_layout(last.id)
                .expect("PageDown shapes the final text block")
                .bounds,
            surface.scroll_metrics_for_test(),
        )
    });
    assert_eq!(
        bottom_metrics_at_max.offset.y, -bottom_metrics_at_max.max_offset.height,
        "PageDown must reach the actual bottom before the final-text no-op route"
    );
    cx.simulate_click(
        point(last_bounds.left() + px(18.0), last_bounds.center().y),
        Modifiers::default(),
    );
    cx.simulate_keystrokes("end");
    redraw(&mut cx);
    let bottom_wheel_point = point(
        bottom_metrics_at_max.viewport.left() + px(18.0),
        bottom_metrics_at_max.viewport.bottom() - px(18.0),
    );
    cx.simulate_event(ScrollWheelEvent {
        position: bottom_wheel_point,
        delta: ScrollDelta::Pixels(point(px(0.0), px(180.0))),
        ..Default::default()
    });
    redraw(&mut cx);
    let (bottom_selection_before, bottom_metrics_before) = view.read_with(&cx, |shell, app| {
        let surface = shell
            .editor_surface
            .as_ref()
            .expect("surface remains mounted")
            .read(app);
        let editor = shell
            .note_session
            .as_ref()
            .expect("session remains mounted")
            .read(app)
            .editor()
            .read(app);
        (editor.selection(), surface.scroll_metrics_for_test())
    });
    assert!(
        bottom_metrics_before.offset.y < px(0.0)
            && bottom_metrics_before.offset.y > -bottom_metrics_before.max_offset.height,
        "move the viewport away from its clamp before testing a bottom text no-op: {bottom_metrics_before:?}"
    );
    cx.simulate_keystrokes("down");
    redraw(&mut cx);
    let (bottom_selection_after, bottom_metrics_after) = view.read_with(&cx, |shell, app| {
        let surface = shell
            .editor_surface
            .as_ref()
            .expect("surface remains mounted")
            .read(app);
        let editor = shell
            .note_session
            .as_ref()
            .expect("session remains mounted")
            .read(app)
            .editor()
            .read(app);
        (editor.selection(), surface.scroll_metrics_for_test())
    });
    assert_eq!(
        bottom_selection_after, bottom_selection_before,
        "ArrowDown at the ordinary final text line is a Core no-op"
    );
    assert_eq!(
        bottom_metrics_after.offset, bottom_metrics_before.offset,
        "an ordinary final-text no-op must not invoke the atomic scroll fallback"
    );
}

#[gpui::test]
async fn mounted_long_image_before_pdf_has_a_scrollable_editor_viewport_and_page_down_reaches_pdf(
    cx: &mut TestAppContext,
) {
    // Release regression: a portrait 1218x2494 image followed by a PDF was
    // visibly taller than the editor pane, yet the retained scroll owner had
    // no usable range. The fixture pins the durable presentation dimensions
    // so image hydration/decode timing cannot hide a flex geometry failure.
    cx.update(|app| crate::components::init(app));
    let (_profile, repository) = repository();
    let image_id = repository
        .import_resource(&tiny_png(), "long.png", "image/png", "png")
        .expect("store image fixture");
    // Use the repository's genuine PDF fixture. The scroll test itself only
    // needs metadata, but its reached-card follow-up is also allowed to use
    // the production attachment opener rather than a malformed byte stub.
    let pdf_id = repository
        .import_resource(
            include_bytes!(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/../app-desktop/integration-tests/resources/small-pdf.pdf"
            )),
            "after-image.pdf",
            "application/pdf",
            "pdf",
        )
        .expect("store PDF fixture");
    let note = repository
        .create_note(CreateNote {
            title: "长图后的 PDF".into(),
            notebook_id: None,
            document: CanonicalDocument::from_blocks(vec![
                Block::Image {
                    resource_id: image_id,
                    alt: "1218x2494 portrait fixture".into(),
                    presentation: ImagePresentation {
                        natural_size: Some((1218, 2494)),
                        display_width: None,
                        alignment: None,
                    },
                    link: None,
                },
                Block::Attachment {
                    resource_id: pdf_id,
                    filename: "after-image.pdf".into(),
                    media_type: "application/pdf".into(),
                },
            ]),
        })
        .expect("create long-image fixture");
    let durable_before_navigation = repository
        .load_note(&note.id)
        .expect("load initial durable fixture")
        .expect("long-image fixture remains durable");
    let (view, mut cx) = mount_shell_at_900x700(Arc::clone(&repository), cx);
    redraw(&mut cx);
    let card = cx
        .debug_bounds("library-note-card")
        .expect("fixture note is listed in the production shell");
    cx.simulate_click(card.center(), Modifiers::default());
    redraw(&mut cx);

    // The release report reached the editor after selecting a note, then used
    // the expanded reading pane. Collapse only the navigation columns; this
    // must preserve the same LibraryShell/session while yielding the actual
    // 900px-wide editor viewport in which a 1218x2494 image is long.
    cx.update(|window, app| {
        view.update(app, |shell, shell_cx| {
            shell.apply_action(AppAction::ToggleSidebar, window, shell_cx);
            shell.apply_action(AppAction::ToggleNoteList, window, shell_cx);
        });
    });
    redraw(&mut cx);

    let (metrics, image_bounds, pdf_bounds, total_height, visible_range) =
        view.read_with(&cx, |shell, app| {
            let surface = shell
                .editor_surface
                .as_ref()
                .expect("selected note mounts EditorSurface")
                .read(app);
            let session = shell
                .note_session
                .as_ref()
                .expect("selected note has session");
            let editor = session.read(app).editor().read(app);
            let mut blocks = editor.document().blocks().iter();
            let image = blocks.next().expect("image block");
            let attachment = blocks.next().expect("attachment block");
            (
                surface.scroll_metrics_for_test(),
                editor
                    .layout()
                    .block_layout(image.id)
                    .expect("image layout")
                    .bounds,
                editor
                    .layout()
                    .block_layout(attachment.id)
                    .map(|layout| layout.bounds),
                editor.layout().total_height(),
                editor.layout().visible_range(),
            )
        });
    assert!(
        total_height > f32::from(metrics.viewport.size.height),
        "fixture must actually exceed the 900x700 editor viewport: total_height={total_height}, image={image_bounds:?}, viewport={:?}, visible_range={visible_range:?}",
        metrics.viewport,
    );
    assert!(
        pdf_bounds.is_none(),
        "the below-image PDF should remain outside the initial visible-only layout window"
    );
    assert!(
        metrics.max_offset.height > px(0.0),
        "a long document must produce a real nested scroll range; metrics={metrics:?}"
    );

    // This is a platform-level wheel route, not a direct ScrollHandle write.
    // The top of the visible image gives the captured editor surface focus
    // before the wheel event is sent to the production scroll container.
    let focus_point = point(
        image_bounds.left() + px(18.0),
        metrics.viewport.top() + px(18.0),
    );
    cx.simulate_click(focus_point, Modifiers::default());
    cx.simulate_event(ScrollWheelEvent {
        position: focus_point,
        delta: ScrollDelta::Pixels(point(px(0.0), px(-180.0))),
        ..Default::default()
    });
    redraw(&mut cx);
    let after_wheel = view.read_with(&cx, |shell, app| {
        shell
            .editor_surface
            .as_ref()
            .expect("surface remains mounted after wheel")
            .read(app)
            .scroll_metrics_for_test()
    });
    assert!(
        after_wheel.offset.y < px(0.0),
        "a wheel event over the document must advance the nested editor scroll handle: before={metrics:?}, after={after_wheel:?}"
    );

    // PageUp returns to the start; PageDown then advances by real viewport
    // pages. A missing EditorSurface action binding used to leave the PDF
    // permanently below the canvas even after keyboard navigation.
    cx.simulate_keystrokes("pageup");
    redraw(&mut cx);
    for _ in 0..4 {
        cx.simulate_keystrokes("pagedown");
        redraw(&mut cx);
    }
    let (after_pages, pdf_bounds, attachment_node) = view.read_with(&cx, |shell, app| {
        let surface = shell
            .editor_surface
            .as_ref()
            .expect("surface remains mounted after PageDown")
            .read(app);
        let session = shell
            .note_session
            .as_ref()
            .expect("session remains mounted");
        let editor = session.read(app).editor().read(app);
        let attachment = editor
            .document()
            .blocks()
            .iter()
            .find(|block| matches!(block.content, BlockContent::Attachment { .. }))
            .expect("PDF attachment remains in document");
        (
            surface.scroll_metrics_for_test(),
            editor
                .layout()
                .block_layout(attachment.id)
                .expect("PageDown shapes the reached PDF attachment")
                .bounds,
            attachment.id,
        )
    });
    assert!(
        after_pages.offset.y < after_wheel.offset.y,
        "PageDown must advance beyond the preceding wheel position: wheel={after_wheel:?}, pages={after_pages:?}"
    );
    assert!(
        pdf_bounds.bottom() > after_pages.viewport.top()
            && pdf_bounds.top() < after_pages.viewport.bottom(),
        "the PDF must become reachable in the actual editor viewport after PageDown: pdf={pdf_bounds:?}, viewport={:?}",
        after_pages.viewport
    );

    // Reaching the attachment must preserve the production pointer path as
    // well: a PDF that is merely painted but cannot be handed to the typed
    // opener would not fix the release regression. The injected opener keeps
    // this mounted test outside the desktop and proves the double-click is
    // delivered after keyboard scrolling, not by calling NoteSession directly.
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
    view.update(&mut cx, |shell, shell_cx| {
        shell.set_attachment_opener_for_test(opener, shell_cx);
        shell_cx.notify();
    });
    let attachment_hit = point(pdf_bounds.left() + px(18.0), pdf_bounds.top() + px(18.0));
    cx.simulate_event(MouseDownEvent {
        button: MouseButton::Left,
        position: attachment_hit,
        modifiers: Modifiers::default(),
        click_count: 2,
        first_mouse: false,
    });
    cx.simulate_event(MouseUpEvent {
        button: MouseButton::Left,
        position: attachment_hit,
        modifiers: Modifiers::default(),
        click_count: 2,
    });
    cx.run_until_parked();
    redraw(&mut cx);
    let opened = opened.lock().expect("opener result mutex");
    assert_eq!(
        opened.len(),
        1,
        "double-clicking the keyboard-reached PDF must invoke the typed attachment opener exactly once"
    );
    assert_eq!(
        opened[0]
            .extension()
            .and_then(|extension| extension.to_str()),
        Some("pdf"),
        "the opener must receive a materialized PDF, not the library blob"
    );
    drop(opened);
    view.read_with(&cx, |shell, app| {
        let session = shell
            .note_session
            .as_ref()
            .expect("session remains mounted");
        let editor = session.read(app).editor().read(app);
        let selection = editor.selection();
        assert!(
            !selection.is_caret()
                && selection.anchor.node_id == attachment_node
                && selection.head.node_id == attachment_node,
            "the reached attachment remains an atomic selection after its opener event"
        );
    });

    // ArrowDown is the ordinary focused-editor route. After an image atom it
    // first resolves the legitimate after-image selection, then must keep the
    // viewport moving rather than stranding the person on a no-op structural
    // boundary with the PDF forever below the fold.
    for _ in 0..4 {
        cx.simulate_keystrokes("pageup");
    }
    redraw(&mut cx);
    let (image_bounds_after_return, image_node) = view.read_with(&cx, |shell, app| {
        let session = shell
            .note_session
            .as_ref()
            .expect("session remains mounted");
        let editor = session.read(app).editor().read(app);
        let image = editor
            .document()
            .blocks()
            .iter()
            .find(|block| matches!(block.content, BlockContent::Image { .. }))
            .expect("image remains in document");
        (
            editor
                .layout()
                .block_layout(image.id)
                .expect("returning to top shapes the image")
                .bounds,
            image.id,
        )
    });
    assert_eq!(
        image_bounds_after_return.size, image_bounds.size,
        "scroll navigation must not rewrite the long image presentation geometry"
    );

    // Establish the actual image NodeSelection through the production pointer
    // route before testing ArrowDown. PageUp only changes the viewport; it
    // deliberately does not pretend to move the editor selection back above
    // the PDF that was opened earlier in this test.
    let image_hit = point(
        image_bounds_after_return.left() + px(18.0),
        image_bounds_after_return.top() + px(18.0),
    );
    cx.simulate_click(image_hit, Modifiers::default());
    redraw(&mut cx);
    let image_selection_before_arrow = view.read_with(&cx, |shell, app| {
        shell
            .note_session
            .as_ref()
            .expect("session remains mounted")
            .read(app)
            .editor()
            .read(app)
            .selection()
    });
    assert!(
        !image_selection_before_arrow.is_caret()
            && image_selection_before_arrow.anchor.node_id == image_node
            && image_selection_before_arrow.head.node_id == image_node,
        "ArrowDown regression must begin at a real image NodeSelection, not a stale PDF selection"
    );
    // GPUI applies the canvas translation during the next paint. Drive the
    // native key path one frame at a time, as the real event loop does,
    // instead of making 48 stale-layout dispatches in a single test turn.
    cx.simulate_keystrokes("down");
    redraw(&mut cx);
    let image_selection_after_first_arrow = view.read_with(&cx, |shell, app| {
        shell
            .note_session
            .as_ref()
            .expect("session remains mounted")
            .read(app)
            .editor()
            .read(app)
            .selection()
    });
    assert_ne!(
        image_selection_after_first_arrow, image_selection_before_arrow,
        "the first ArrowDown from an image must retain the ordinary EditorCore boundary selection movement before viewport fallback"
    );
    for _ in 1..48 {
        cx.simulate_keystrokes("down");
        redraw(&mut cx);
    }
    let (after_arrows, arrow_pdf_bounds) = view.read_with(&cx, |shell, app| {
        let surface = shell
            .editor_surface
            .as_ref()
            .expect("surface remains mounted after ArrowDown")
            .read(app);
        let session = shell
            .note_session
            .as_ref()
            .expect("session remains mounted");
        let editor = session.read(app).editor().read(app);
        let attachment = editor
            .document()
            .blocks()
            .iter()
            .find(|block| matches!(block.content, BlockContent::Attachment { .. }))
            .expect("PDF attachment remains in document");
        (
            surface.scroll_metrics_for_test(),
            editor
                .layout()
                .block_layout(attachment.id)
                .expect("ArrowDown reaches and shapes PDF attachment")
                .bounds,
        )
    });
    assert!(
        after_arrows.offset.y < px(0.0)
            && arrow_pdf_bounds.bottom() > after_arrows.viewport.top()
            && arrow_pdf_bounds.top() < after_arrows.viewport.bottom(),
        "ArrowDown must make the below-image PDF reachable without modifying the document: metrics={after_arrows:?}, pdf={arrow_pdf_bounds:?}"
    );

    // The scroll/selection routes are purely presentational. A real manual
    // sync after all three navigation paths must leave canonical content and
    // revision untouched rather than turning viewport travel into a document
    // edit or a geometry repair.
    cx.update(|window, app| {
        view.update(app, |shell, shell_cx| {
            shell.apply_action(AppAction::ManualSync, window, shell_cx);
        });
    });
    cx.run_until_parked();
    redraw(&mut cx);
    let durable_after_navigation = repository
        .load_note(&note.id)
        .expect("load fixture after manual sync")
        .expect("navigation must not delete fixture");
    assert_eq!(
        durable_after_navigation.revision, durable_before_navigation.revision,
        "scrolling and keyboard traversal must not manufacture a semantic revision"
    );
    assert_eq!(
        durable_after_navigation.body_html, durable_before_navigation.body_html,
        "manual sync after navigation must preserve the canonical long-image/PDF document"
    );
}
