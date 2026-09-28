//! What a table cell shows is its content, laid out once: marks, line
//! breaks, images and attachments are painted, rows are as tall as what is
//! painted in them, and a click lands in the cell it was painted in
//! (Evernote's table cells hold rich `tablecontent+`, table/schema.ts 219).

use crate::app::{AppAction, AppModel};
use crate::native_editor::core::AtomicBlockHit;
use crate::native_editor::model::BlockContent;
use crate::native_editor::render::{TestTableCellPaint, take_test_table_cell_paints};
use crate::ui::LibraryShell;
use app_lite_core::{CanonicalDocument, CreateNote, LibraryRepository, ResourceId, SaveNote};
use gpui::{AppContext, TestAppContext, VisualTestContext, point, px, size};
use std::sync::Arc;

#[gpui::test]
async fn table_command_click_follows_link_without_editing(cx: &mut TestAppContext) {
    let (fixture, cx) = mount(cx, false);
    let paints = paint_until_images_load(cx);
    let link = cell(&paints, 2, 1);
    let position = point(link.cell.left() + px(12.0), link.lines[1].0 + px(5.0));
    let editor = fixture.view.read_with(cx, |shell, app| {
        shell.note_session.as_ref().unwrap().read(app).editor().clone()
    });
    let revision = editor.read_with(cx, |editor, _| editor.document().revision());
    // A plain CJK line, padding and whitespace must not inherit a later link.
    for position in [
        point(link.cell.left() + px(12.0), link.lines[0].0 + px(5.0)),
        point(link.cell.right() - px(2.0), link.lines[0].0 + px(5.0)),
        point(link.cell.left() + px(1.0), link.lines[1].0 + px(5.0)),
    ] {
        cx.simulate_event(gpui::MouseDownEvent {
            button: gpui::MouseButton::Left, position,
            modifiers: gpui::Modifiers { platform: true, ..Default::default() },
            click_count: 1, first_mouse: false,
        });
        cx.run_until_parked();
        assert_eq!(cx.opened_url(), None);
    }
    // Every wrapped visual line after the explicit break resolves the same URL.
    assert!(link.lines.len() >= 3, "fixture must wrap the long link");
    for (top, _) in link.lines.iter().skip(1) {
        let target = editor.read_with(cx, |editor, _| editor.layout().table_link_at(
            point(link.cell.left() + px(12.0), *top + px(5.0))));
        assert_eq!(target.as_deref(), Some("https://example.com/a/very/long/path/that/keeps/going/and/going/without/any/spaces/at/all"));
    }
    for command in [false, true] {
        let modifiers = gpui::Modifiers { platform: command, ..Default::default() };
        cx.simulate_event(gpui::MouseDownEvent {
            button: gpui::MouseButton::Left, position, modifiers,
            click_count: 1, first_mouse: false,
        });
        cx.simulate_event(gpui::MouseUpEvent {
            button: gpui::MouseButton::Left, position, modifiers, click_count: 1,
        });
        cx.run_until_parked();
        if !command { assert_eq!(cx.opened_url(), None); }
    }
    assert_eq!(cx.opened_url().as_deref(), Some("https://example.com/a/very/long/path/that/keeps/going/and/going/without/any/spaces/at/all"));
    assert_eq!(editor.read_with(cx, |editor, _| editor.document().revision()), revision);
}

fn png(width: u32, height: u32) -> Vec<u8> {
    let mut bytes = Vec::new();
    image::DynamicImage::new_rgb8(width, height)
        .write_to(
            &mut std::io::Cursor::new(&mut bytes),
            image::ImageFormat::Png,
        )
        .unwrap();
    bytes
}

struct Fixture {
    _root: tempfile::TempDir,
    view: gpui::Entity<LibraryShell>,
    image: ResourceId,
    attachment: ResourceId,
    /// Holds the image's load until sent (when mounted with `stall_image`).
    release_image: Option<futures::channel::oneshot::Sender<()>>,
}

fn mount<'a>(
    cx: &'a mut TestAppContext,
    stall_image: bool,
) -> (Fixture, &'a mut VisualTestContext) {
    mount_with_size(cx, stall_image, None)
}

fn mount_with_size<'a>(
    cx: &'a mut TestAppContext,
    stall_image: bool,
    dimensions: Option<(f32, f32)>,
) -> (Fixture, &'a mut VisualTestContext) {
    cx.update(|app| crate::components::init(app));
    let root = tempfile::tempdir().unwrap();
    let repository = Arc::new(LibraryRepository::open(root.path().join("library.sqlite")).unwrap());
    let image = repository
        .import_resource(&png(160, 80), "图.png", "image/png", "png")
        .unwrap();
    let attachment = repository
        .import_resource(b"%PDF-1.4 fixture", "报告.pdf", "application/pdf", "pdf")
        .unwrap();
    let html = format!(
        concat!(
            "<table data-joplin-lite-table=\"true\"><tbody>",
            "<tr><th>格式</th><th>内容</th></tr>",
            "<tr><td>普通<strong>粗体</strong><em>斜体</em><u>下划线</u><s>删除</s><mark>高亮</mark>",
            "<a href=\"https://example.com/\">链接</a></td>",
            "<td>上一行<br>下一行<img src=\":/{image}\" alt=\"图\">",
            "<a data-joplin-lite-inline-attachment=\"true\" href=\":/{attachment}\" data-filename=\"报告.pdf\" data-media-type=\"application/pdf\">报告.pdf</a></td></tr>",
            "<tr><td><strong>WWWWWWWWWWWWWWWWWWWWWWWWWWWWWWWWWWWW</strong> 中文混排😀😀 emoji</td>",
            "<td>普通中文<br><a href=\"https://example.com/a/very/long/path/that/keeps/going/and/going/without/any/spaces/at/all\">https://example.com/a/very/long/path/that/keeps/going/and/going/without/any/spaces/at/all</a></td></tr>",
            "</tbody></table>"
        ),
        image = image.as_str(),
        attachment = attachment.as_str(),
    );
    let note = repository
        .create_note(CreateNote {
            title: "表格".into(),
            notebook_id: None,
            document: CanonicalDocument::from_blocks(Vec::new()),
        })
        .unwrap();
    let document = CanonicalDocument::parse_html(&html).unwrap();
    repository
        .save_note(SaveNote {
            id: note.id.clone(),
            expected_revision: note.revision,
            title: "表格".into(),
            resource_ids: document.resource_ids(),
            document,
            selected_thumbnail_id: None,
        })
        .unwrap();
    let model_repository = Arc::clone(&repository);
    let model = cx.new(move |_| AppModel::open(model_repository).unwrap());
    let (view, vcx) =
        cx.add_window_view(move |window, cx| LibraryShell::new(model, None, window, cx));
    if let Some((width, height)) = dimensions {
        vcx.simulate_resize(size(px(width), px(height)));
    }
    let id = note.id.clone();
    vcx.update(|window, app| {
        view.update(app, |shell, shell_cx| {
            shell.stall_images_in_next_session_for_test = stall_image;
            shell.apply_action(AppAction::SelectNote(id), window, shell_cx);
        })
    });
    let release_image =
        vcx.update(|_, app| view.update(app, |shell, _| shell.image_load_release_for_test.take()));
    assert_eq!(release_image.is_some(), stall_image);
    vcx.run_until_parked();
    (
        Fixture {
            _root: root,
            view,
            image,
            attachment,
            release_image,
        },
        vcx,
    )
}

#[gpui::test]
async fn table_hidden_column_image_loads_only_when_scrolled_into_view(cx: &mut TestAppContext) {
    let (fixture, cx) = mount_with_size(cx, false, Some((640.0, 900.0)));
    let editor = fixture.view.read_with(cx, |shell, app| {
        shell.note_session.as_ref().unwrap().read(app).editor().clone()
    });
    let paints = paint(cx);
    let bounds = editor.read_with(cx, |editor, _| editor.layout().visible()[0].bounds);
    assert!(cell(&paints, 1, 1).cell.left() >= bounds.right(), "image column must be offscreen");
    assert!(!editor.read_with(cx, |editor, _| editor.image_source_path(fixture.image.as_str()).is_some()),
        "an invisible table image must not materialize its original blob");
    cx.simulate_event(gpui::ScrollWheelEvent {
        position: point(bounds.left() + px(10.0), bounds.top() + px(10.0)),
        delta: gpui::ScrollDelta::Pixels(point(px(-1000.0), px(0.0))),
        touch_phase: gpui::TouchPhase::Moved,
        modifiers: Default::default(),
    });
    cx.run_until_parked();
    let paints = paint_until_images_load(cx);
    assert!(editor.read_with(cx, |editor, _| editor.image_source_path(fixture.image.as_str()).is_some()));
    assert!(!cell(&paints, 1, 1).images.is_empty());
}

#[gpui::test]
async fn table_below_viewport_image_loads_when_note_viewport_grows(cx: &mut TestAppContext) {
    let (fixture, cx) = mount_with_size(cx, false, Some((1200.0, 220.0)));
    let editor = fixture.view.read_with(cx, |shell, app| {
        shell.note_session.as_ref().unwrap().read(app).editor().clone()
    });
    let paints = paint(cx);
    let viewport = fixture.view.read_with(cx, |shell, app| {
        shell.editor_surface.as_ref().unwrap().read(app).scroll_metrics_for_test().viewport
    });
    assert!(cell(&paints, 1, 1).images[0].1.top() >= viewport.bottom(),
        "fixture image {:?} must be below the actual note viewport: {viewport:?}", cell(&paints, 1, 1).images[0].1);
    assert!(!editor.read_with(cx, |editor, _| editor.image_source_path(fixture.image.as_str()).is_some()));
    cx.simulate_resize(size(px(1200.0), px(900.0)));
    cx.run_until_parked();
    let paints = paint_until_images_load(cx);
    assert!(cell(&paints, 1, 1).images.iter().any(|(_, _, loaded)| *loaded));
}

/// Draws one frame and returns what each table cell painted.
fn paint(cx: &mut VisualTestContext) -> Vec<TestTableCellPaint> {
    take_test_table_cell_paints();
    cx.update(|window, app| window.draw(app).clear());
    let paints = take_test_table_cell_paints();
    cx.run_until_parked();
    paints
}

/// Draws frames until the cell images have their pixels: the first frame
/// asks for them, loading and decoding each take a turn of the executor.
fn paint_until_images_load(cx: &mut VisualTestContext) -> Vec<TestTableCellPaint> {
    for _ in 0..5 {
        let paints = paint(cx);
        let images: Vec<_> = paints.iter().flat_map(|paint| &paint.images).collect();
        if !images.is_empty() && images.iter().all(|(_, _, loaded)| *loaded) {
            return paints;
        }
    }
    panic!(
        "table images did not load within five frames: {:#?}",
        paint(cx)
    );
}

fn cell(paints: &[TestTableCellPaint], row: usize, column: usize) -> &TestTableCellPaint {
    paints
        .iter()
        .rev()
        .find(|paint| paint.row == row && paint.column == column)
        .unwrap_or_else(|| panic!("cell {row},{column} was not painted: {paints:#?}"))
}

#[gpui::test]
async fn table_cells_paint_their_marks_line_breaks_images_and_attachments(cx: &mut TestAppContext) {
    let (fixture, cx) = mount(cx, false);
    let paints = paint_until_images_load(cx);

    let marked = cell(&paints, 1, 0);
    let run = |text: &str| {
        marked
            .runs
            .iter()
            .find(|run| run.text == text)
            .unwrap_or_else(|| panic!("{text:?} not painted as its own run: {:#?}", marked.runs))
            .clone()
    };
    assert!(run("粗体").bold);
    assert!(run("斜体").italic);
    assert!(run("下划线").underline);
    assert!(run("删除").strikethrough);
    assert!(run("高亮").highlight);
    assert!(run("链接").link);
    assert!(!run("普通").bold);

    let rich = cell(&paints, 1, 1);
    assert!(rich.runs.iter().any(|run| run.text.contains("上一行")));
    assert!(rich.runs.iter().any(|run| run.text.contains("下一行")));
    assert!(
        rich.lines.len() >= 2,
        "the line break starts a new line: {rich:#?}"
    );
    let (image, image_bounds, loaded) = rich
        .images
        .first()
        .cloned()
        .unwrap_or_else(|| panic!("the cell image is painted: {rich:#?}"));
    assert_eq!(image, fixture.image.as_str());
    assert!(loaded, "its pixels, once loaded");
    assert!(rich.cell.contains(&image_bounds.origin), "{rich:#?}");
    assert!(image_bounds.bottom() <= rich.cell.bottom());
    assert!(image_bounds.size.width > px(0.0) && image_bounds.size.height > px(0.0));
    assert_eq!(
        rich.attachments.first().map(|(id, _)| id.as_str()),
        Some(fixture.attachment.as_str()),
        "{rich:#?}"
    );
    let header = cell(&paints, 0, 1);
    assert!(header.runs.iter().all(|run| run.bold), "{header:#?}");
}

/// Rows are measured from the shaped content: bold wide letters, CJK,
/// emoji, a long unbroken link, narrow columns, and an image before and
/// after it loads. Nothing spills past its row, no row is padded with blank
/// lines, and a click on any painted line hits that cell.
#[gpui::test]
async fn table_rows_fit_their_painted_content_and_clicks_hit_the_painted_cell(
    cx: &mut TestAppContext,
) {
    let (mut fixture, cx) = mount(cx, true);
    let editor = fixture.view.read_with(cx, |shell, app| {
        shell
            .note_session
            .as_ref()
            .unwrap()
            .read(app)
            .editor()
            .clone()
    });
    // `note_right`: where the note ends; clicks go only inside it.
    let check = |paints: &[TestTableCellPaint],
                 note_right: gpui::Pixels,
                 label: &str,
                 cx: &mut VisualTestContext| {
        let rows = paints.iter().map(|paint| paint.row).max().unwrap() + 1;
        for row in 0..rows {
            let cells: Vec<_> = paints.iter().filter(|paint| paint.row == row).collect();
            let row_bottom = cells[0].cell.bottom();
            let tallest = cells
                .iter()
                .map(|paint| paint.content_bottom)
                .fold(px(0.0), |a, b| if a > b { a } else { b });
            for paint in &cells {
                assert!(
                    paint.content_bottom <= row_bottom + px(0.5),
                    "{label}: cell {},{} spills below its row: {paint:#?}",
                    paint.row,
                    paint.column
                );
            }
            assert!(
                row_bottom - tallest <= px(12.0),
                "{label}: row {row} is taller than its content ({row_bottom:?} vs {tallest:?})"
            );
            for paint in &cells {
                let clickable =
                    (note_right.min(paint.cell.right()) - paint.cell.left()).max(px(0.0));
                if clickable <= px(0.0) {
                    continue;
                }
                for (top, height) in &paint.lines {
                    let inset = px(10.0).min(clickable / 2.0);
                    let at = point(paint.cell.left() + inset, *top + *height / 2.0);
                    let hit = cx.update(|_, app| {
                        editor.update(app, |editor, _| editor.select_atomic_at(at))
                    });
                    assert!(
                        matches!(hit, Some(AtomicBlockHit::Table { row, column, .. }) if row == paint.row && column == paint.column),
                        "{label}: a click on a painted line of cell {},{} hit {hit:?}",
                        paint.row,
                        paint.column
                    );
                }
            }
        }
    };
    let everywhere = px(100_000.0);
    let before_image = paint(cx);
    assert!(
        cell(&before_image, 1, 1)
            .images
            .iter()
            .all(|(_, _, loaded)| !*loaded),
        "the first frame lays the image out before it loads"
    );
    check(&before_image, everywhere, "wide, image loading", cx);
    let _ = fixture.release_image.take().unwrap().send(());
    let after_image = paint_until_images_load(cx);
    check(&after_image, everywhere, "wide, image loaded", cx);
    assert!(
        cell(&after_image, 1, 1)
            .images
            .iter()
            .any(|(_, _, loaded)| *loaded),
        "{:#?}",
        cell(&after_image, 1, 1)
    );
    // A narrow window with only the note showing: narrow columns.
    cx.update(|window, app| {
        fixture.view.update(app, |shell, shell_cx| {
            shell.apply_action(AppAction::ToggleSidebar, window, shell_cx);
            shell.apply_action(AppAction::ToggleNoteList, window, shell_cx);
        })
    });
    cx.simulate_resize(size(px(560.0), px(900.0)));
    cx.run_until_parked();
    let narrow = paint_until_images_load(cx);
    let column = cell(&narrow, 2, 0).cell.size.width;
    assert!(
        column > px(150.0) && column < px(320.0),
        "a narrow but usable column: {column:?}"
    );
    check(&narrow, everywhere, "narrow", cx);
    assert!(
        narrow
            .iter()
            .any(|paint| paint.row == 2 && paint.lines.len() >= 3),
        "the long bold text and link wrap in narrow columns: {narrow:#?}"
    );
    // Sidebar and list back in a window too small for them: the note is
    // squeezed to almost nothing. Content still stays inside its rows and a
    // click inside a cell still lands in that cell.
    cx.update(|window, app| {
        fixture.view.update(app, |shell, shell_cx| {
            shell.apply_action(AppAction::ToggleSidebar, window, shell_cx);
            shell.apply_action(AppAction::ToggleNoteList, window, shell_cx);
        })
    });
    cx.simulate_resize(size(px(520.0), px(900.0)));
    cx.run_until_parked();
    let squeezed = paint(cx);
    let note =
        cx.update(|_, app| editor.read_with(app, |editor, _| editor.layout().visible()[0].bounds));
    assert!(
        note.size.width < px(20.0),
        "the note really is squeezed: {note:?}"
    );
    for paint in &squeezed {
        assert!(
            paint.cell.size.width >= px(64.0),
            "columns keep a usable width: {paint:#?}"
        );
    }
    // Rows still hold their content; only what is inside the note can be
    // clicked, and it hits the cell it shows.
    check(&squeezed, note.right(), "squeezed", cx);
}

#[allow(dead_code)]
fn is_table(content: &BlockContent) -> bool {
    matches!(content, BlockContent::Table(_))
}

#[gpui::test]
async fn table_horizontal_wheel_reveals_hidden_column_without_editing(cx: &mut TestAppContext) {
    let (fixture, cx) = mount(cx, false);
    cx.simulate_resize(size(px(640.0), px(900.0)));
    cx.run_until_parked();
    // The hidden image is intentionally not resident before scrolling.
    let before = paint(cx);
    let editor = fixture.view.read_with(cx, |shell, app| {
        shell.note_session.as_ref().unwrap().read(app).editor().clone()
    });
    let (bounds, revision) = cx.update(|_, app| {
        let editor = editor.read(app);
        (editor.layout().visible()[0].bounds, editor.document().revision())
    });
    assert!(bounds.size.width > px(20.0) && bounds.size.width < px(128.0), "{bounds:?}");
    let position = point(bounds.center().x, cell(&before, 0, 0).cell.center().y);
    cx.simulate_event(gpui::ScrollWheelEvent {
        position,
        delta: gpui::ScrollDelta::Pixels(point(px(-1000.0), px(0.0))),
        modifiers: gpui::Modifiers::default(),
        touch_phase: gpui::TouchPhase::Moved,
    });
    cx.run_until_parked();
    let after = paint_until_images_load(cx);
    let right = cell(&after, 0, 1).cell;
    assert!(right.center().x < bounds.right(), "right column must be reachable: {right:?} vs {bounds:?}");
    cx.update(|_, app| {
        editor.update(app, |editor, _| {
            assert_eq!(editor.document().revision(), revision, "scroll is not an edit");
            assert!(matches!(editor.select_atomic_at(right.center()), Some(AtomicBlockHit::Table { column: 1, .. })));
            assert!(editor.select_atomic_at(point(bounds.right() + px(10.0), right.center().y)).is_none(),
                "clipped content outside the note must not receive clicks");
        });
    });
    cx.simulate_event(gpui::ScrollWheelEvent {
        position,
        delta: gpui::ScrollDelta::Pixels(point(px(1000.0), px(0.0))),
        modifiers: gpui::Modifiers::default(),
        touch_phase: gpui::TouchPhase::Moved,
    });
    cx.run_until_parked();
    let restored = paint(cx);
    assert_eq!(cell(&restored, 0, 0).cell.left(), cell(&before, 0, 0).cell.left(),
        "reverse gesture clamps to the first column without overscroll");
}

/// Dropping the surface's table-attachment event branch must fail this:
/// a double click on the painted label opens verified bytes, not a cell editor.
#[gpui::test]
async fn table_attachment_double_click_opens_verified_bytes(cx: &mut TestAppContext) {
    let (fixture, cx) = mount(cx, false);
    let paints = paint_until_images_load(cx);
    let attachment_bounds = cell(&paints, 1, 1).attachments[0].1;
    let opened = Arc::new(std::sync::Mutex::new(Vec::new()));
    let observed = Arc::clone(&opened);
    fixture.view.update(cx, |shell, shell_cx| {
        shell.set_attachment_opener_for_test(
            Arc::new(move |path| {
                let bytes = std::fs::read(path).map_err(|error| error.to_string())?;
                observed.lock().unwrap().push(bytes);
                Ok(())
            }),
            shell_cx,
        );
    });
    let position = attachment_bounds.center();
    cx.simulate_event(gpui::MouseDownEvent {
        button: gpui::MouseButton::Left,
        position,
        modifiers: gpui::Modifiers::default(),
        click_count: 2,
        first_mouse: false,
    });
    cx.simulate_event(gpui::MouseUpEvent {
        button: gpui::MouseButton::Left,
        position,
        modifiers: gpui::Modifiers::default(),
        click_count: 2,
    });
    cx.run_until_parked();
    assert_eq!(*opened.lock().unwrap(), vec![b"%PDF-1.4 fixture".to_vec()]);
}
