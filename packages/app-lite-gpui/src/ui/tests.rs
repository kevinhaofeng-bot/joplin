use super::*;
use crate::app::AppAction;
use crate::app::save_coordinator::ManualSaveClock;
use crate::components::{Copy, Paste, SelectAll};
use crate::native_editor::model::{Affinity, BlockKind, DocPoint, Mark};
use app_lite_core::document::{Block, BlockStyle, ImagePresentation, Inline, Marks};
use app_lite_core::{
    CanonicalDocument, CreateNote, LibraryRoute, LibraryShellState, ResourceId, SaveNote,
};
use gpui::{
    AppContext, ClipboardItem, EntityInputHandler, Image, ImageFormat, KeyDownEvent, Keystroke,
    Modifiers, TestAppContext, VisualTestContext, point, px,
};
use rusqlite::{Connection, params};
use std::io::Cursor;
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
use std::sync::mpsc::{self, TryRecvError};
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

fn scale_document(index: usize, resource_ids: &[ResourceId]) -> CanonicalDocument {
    let body = format!("规模正文 {index:04} 不得由列表投影读取。").repeat(192);
    let mut blocks = vec![Block::Paragraph {
        style: BlockStyle::default(),
        inlines: vec![Inline::Text {
            text: body,
            marks: Default::default(),
        }],
    }];
    blocks.extend(
        resource_ids
            .iter()
            .enumerate()
            .map(|(position, resource_id)| Block::Image {
                resource_id: resource_id.clone(),
                alt: format!("规模图片 {index:04}-{position}"),
                presentation: ImagePresentation {
                    natural_size: Some((16, 12)),
                    display_width: Some(16),
                },
            }),
    );
    CanonicalDocument::from_blocks(blocks)
}

const SCALE_NOTE_COUNT: usize = 1_662;
const SCALE_NOTEBOOK_COUNT: usize = 31;
const SCALE_TAG_COUNT: usize = 64;
const SCALE_RESOURCE_COUNT: usize = 4_238;

/// Create the Task-6 scale fixture in one SQLite transaction after publishing
/// one real, verified PNG blob through the repository. The remaining resource
/// rows deliberately share that valid content-addressed blob: this makes the
/// fixture exercise all 4,238 real `resources`/`note_resources` SQL joins and
/// the eventual visible-card reader without turning a projection-scale test
/// into 4,238 filesystem writes.
///
/// The fixture is intentionally not a second application authority. It is
/// installed before `AppModel::open`, and every assertion below goes through
/// the ordinary repository/query and mounted `LibraryShell` paths.
fn install_projection_scale_fixture(
    profile: &tempfile::TempDir,
    repository: &LibraryRepository,
) -> (
    Vec<app_lite_core::Notebook>,
    Vec<app_lite_core::Tag>,
    app_lite_core::NoteId,
) {
    let mut notebooks = vec![repository.default_notebook().expect("default notebook")];
    for index in 0..(SCALE_NOTEBOOK_COUNT - 1) {
        notebooks.push(
            repository
                .create_notebook(&format!("规模笔记本 {index:02}"), None)
                .expect("create scale notebook"),
        );
    }
    let tags = (0..SCALE_TAG_COUNT)
        .map(|index| {
            repository
                .create_tag(&format!("规模标签 {index:02}"))
                .expect("create scale tag")
        })
        .collect::<Vec<_>>();

    let seed_resource = repository
        .import_resource(
            &structural_png(16, 12),
            "scale-thumbnail-seed.png",
            "image/png",
            "png",
        )
        .expect("publish one verified scale thumbnail blob");
    let seed_metadata = repository
        .resource_metadata(&seed_resource)
        .expect("read seed resource metadata")
        .expect("seed resource metadata exists");
    // Keep one selected item fully repository-created. It contains the seed
    // image so every note, including the retained live session below, has a
    // canonical resource block and a real relation.
    let selected_note_id = repository
        .create_note(CreateNote {
            title: "规模会话选择".into(),
            notebook_id: Some(notebooks[0].id.clone()),
            document: scale_document(SCALE_NOTE_COUNT - 1, std::slice::from_ref(&seed_resource)),
        })
        .expect("create selected scale note")
        .id;

    let mut database = Connection::open(profile.path().join("library.sqlite"))
        .expect("open scale fixture database");
    database
        .execute_batch("PRAGMA foreign_keys = ON;")
        .expect("enable scale fixture foreign keys");
    let transaction = database
        .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
        .expect("borrow scale fixture transaction");
    let mut insert_resource = transaction
        .prepare(
            "INSERT INTO resources
             (id, sha256, title, mime, file_extension, size, created_time, updated_time, deleted_time, revision)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, 1, 1, 0, 1)",
        )
        .expect("prepare scale resource metadata insert");
    let mut scale_resources = Vec::with_capacity(SCALE_RESOURCE_COUNT - 1);
    for index in 0..(SCALE_RESOURCE_COUNT - 1) {
        let resource_id = ResourceId::new(format!("a100000000000000000000000000{:04x}", index))
            .expect("scale resource ID is canonical");
        insert_resource
            .execute(params![
                resource_id.as_str(),
                seed_metadata.sha256.as_str(),
                format!("规模资源 {index:04}"),
                seed_metadata.mime.as_str(),
                seed_metadata.file_extension.as_str(),
                seed_metadata.size,
            ])
            .expect("insert scale resource metadata");
        scale_resources.push(resource_id);
    }
    drop(insert_resource);

    let mut insert_note = transaction
        .prepare(
            "INSERT INTO notes
             (id, title, body_html, body_text, snippet, notebook_id, selected_thumbnail_id, created_time, updated_time, deleted_time, revision)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, 1, 1, 0, 1)",
        )
        .expect("prepare scale note insert");
    let mut insert_relation = transaction
        .prepare(
            "INSERT INTO note_resources (note_id, position, resource_id, is_associated)
             VALUES (?1, ?2, ?3, 1)",
        )
        .expect("prepare scale relation insert");
    let mut insert_revision = transaction
        .prepare(
            "INSERT INTO note_revisions
             (note_id, revision, title, body_html, body_text, created_time)
             VALUES (?1, 1, ?2, ?3, ?4, 1)",
        )
        .expect("prepare scale note revision insert");
    let mut insert_tag = transaction
        .prepare("INSERT INTO note_tags (note_id, tag_id, position) VALUES (?1, ?2, 0)")
        .expect("prepare scale tag insert");
    insert_tag
        .execute(params![selected_note_id.as_str(), tags[0].id.as_str()])
        .expect("associate the retained scale session with a real filter tag");
    let mut resource_cursor = 0;
    for index in 0..(SCALE_NOTE_COUNT - 1) {
        let note_id = format!("b100000000000000000000000000{:04x}", index);
        let notebook = &notebooks[index % notebooks.len()];
        let notes_remaining = SCALE_NOTE_COUNT - 1 - index;
        let resources_remaining = scale_resources.len() - resource_cursor;
        let resource_count = resources_remaining.div_ceil(notes_remaining);
        let note_resource_ids = &scale_resources[resource_cursor..resource_cursor + resource_count];
        let document = scale_document(index, note_resource_ids);
        let body_html = document.to_canonical_html().as_str().to_owned();
        let body_text = document.search_text().as_str().to_owned();
        let snippet = body_text.chars().take(160).collect::<String>();
        let selected_thumbnail_id = note_resource_ids
            .first()
            .expect("every scale note receives one or more resources");
        let title = format!("规模笔记 {index:04}");
        insert_note
            .execute(params![
                note_id.as_str(),
                &title,
                &body_html,
                &body_text,
                &snippet,
                notebook.id.as_str(),
                selected_thumbnail_id.as_str(),
            ])
            .expect("insert scale note");
        for (position, resource_id) in note_resource_ids.iter().enumerate() {
            insert_relation
                .execute(params![
                    note_id.as_str(),
                    position as i64,
                    resource_id.as_str()
                ])
                .expect("insert scale note resource relation");
        }
        insert_revision
            .execute(params![note_id.as_str(), &title, &body_html, &body_text])
            .expect("insert canonical scale note revision");
        insert_tag
            .execute(params![
                note_id.as_str(),
                tags[index % tags.len()].id.as_str()
            ])
            .expect("insert scale note tag relation");
        resource_cursor += resource_count;
    }
    assert_eq!(resource_cursor, scale_resources.len());
    drop((insert_note, insert_relation, insert_revision, insert_tag));
    transaction.commit().expect("commit scale fixture");
    (notebooks, tags, selected_note_id)
}

fn assert_projection_scale_fixture_is_canonical_and_fully_associated(database: &Connection) {
    let unattached_resources: i64 = database
        .query_row(
            "SELECT count(*) FROM resources r
             WHERE NOT EXISTS (
                 SELECT 1 FROM note_resources nr WHERE nr.resource_id = r.id
             )",
            [],
            |row| row.get(0),
        )
        .expect("count unattached scale resources");
    assert_eq!(
        unattached_resources, 0,
        "every scale resource metadata row must affect an attachment-count/thumbnail join"
    );
    let notes_with_resources: i64 = database
        .query_row(
            "SELECT count(DISTINCT note_id) FROM note_resources",
            [],
            |row| row.get(0),
        )
        .expect("count scale notes with resources");
    assert_eq!(
        notes_with_resources, SCALE_NOTE_COUNT as i64,
        "the scale fixture must distribute resource pressure across every projection row"
    );

    let notes = {
        let mut statement = database
            .prepare(
                "SELECT id, title, body_html, body_text, snippet, selected_thumbnail_id
                 FROM notes ORDER BY id",
            )
            .expect("prepare canonical scale note scan");
        statement
            .query_map([], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, String>(4)?,
                    row.get::<_, Option<String>>(5)?,
                ))
            })
            .expect("scan canonical scale notes")
            .collect::<Result<Vec<_>, _>>()
            .expect("collect canonical scale notes")
    };
    assert_eq!(notes.len(), SCALE_NOTE_COUNT);

    let mut relation_statement = database
        .prepare(
            "SELECT resource_id FROM note_resources
             WHERE note_id = ?1 ORDER BY position, resource_id",
        )
        .expect("prepare scale relation scan");
    let mut revision_statement = database
        .prepare(
            "SELECT title, body_html, body_text FROM note_revisions
             WHERE note_id = ?1 AND revision = 1",
        )
        .expect("prepare scale revision scan");
    for (note_id, title, body_html, body_text, snippet, selected_thumbnail_id) in notes {
        let document = CanonicalDocument::parse_html(&body_html)
            .expect("scale fixture bodies must remain canonical HTML");
        assert_eq!(
            document.to_canonical_html().as_str(),
            body_html,
            "scale body for {note_id} must round-trip canonically"
        );
        assert_eq!(
            document.search_text().as_str(),
            body_text,
            "stored body text for {note_id} must be derived from its canonical body"
        );
        assert_eq!(
            body_text.chars().take(160).collect::<String>(),
            snippet,
            "list preview for {note_id} must be the durable body preview prefix"
        );
        let canonical_resource_ids = document
            .resource_ids()
            .iter()
            .map(|resource_id| resource_id.as_str().to_owned())
            .collect::<Vec<_>>();
        let related_resource_ids = relation_statement
            .query_map([note_id.as_str()], |row| row.get::<_, String>(0))
            .expect("query scale note relations")
            .collect::<Result<Vec<_>, _>>()
            .expect("collect scale note relations");
        assert_eq!(
            canonical_resource_ids, related_resource_ids,
            "canonical resource blocks and note_resources must agree for {note_id}"
        );
        assert_eq!(
            selected_thumbnail_id.as_deref(),
            canonical_resource_ids.first().map(String::as_str),
            "the selected thumbnail for {note_id} must be a visible first canonical image"
        );
        let revision = revision_statement
            .query_row([note_id.as_str()], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                ))
            })
            .expect("each scale note keeps its revision-one durable snapshot");
        assert_eq!(revision, (title, body_html, body_text));
    }
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
async fn mounted_cmd_f_opens_a_retained_cjk_find_panel(cx: &mut TestAppContext) {
    cx.update(bind_library_keybindings);
    let (_profile, repository) = repository();
    let note = repository
        .create_note(CreateNote {
            title: "查找会话".into(),
            notebook_id: None,
            document: rich_document("会议记录：会议继续。"),
        })
        .expect("create find fixture");
    let other_note = repository
        .create_note(CreateNote {
            title: "另一篇笔记".into(),
            notebook_id: None,
            document: rich_document("没有同一查找词"),
        })
        .expect("create second find fixture");
    let original_html = note.body_html.clone();
    let (view, cx) = mount_shell(Arc::clone(&repository), cx);
    cx.update(|window, app| {
        view.update(app, |shell, shell_cx| {
            shell.apply_action(AppAction::SelectNote(note.id.clone()), window, shell_cx);
        });
    });
    redraw(cx);

    cx.simulate_keystrokes("cmd-f");
    redraw(cx);

    assert!(
        cx.debug_bounds("library-find-in-note-panel").is_some(),
        "Cmd-F must mount the in-note panel inside the retained editor column"
    );
    // Sidebar + note list retain their explicit minimum widths, so 900px
    // leaves the editor at its narrow real-world column rather than hiding it.
    cx.simulate_resize(gpui::size(px(900.0), px(720.0)));
    redraw(cx);
    let controls = [
        "library-find-in-note-case",
        "library-find-in-note-previous",
        "library-find-in-note-next",
        "library-find-in-note-close",
    ]
    .map(|selector| {
        cx.debug_bounds(selector)
            .expect("narrow panel control is mounted")
    });
    for (index, bounds) in controls.iter().enumerate() {
        for other in controls.iter().skip(index + 1) {
            assert!(
                bounds.right() <= other.left()
                    || other.right() <= bounds.left()
                    || bounds.bottom() <= other.top()
                    || other.bottom() <= bounds.top(),
                "narrow-panel controls need distinct click targets"
            );
        }
    }
    let pane = cx
        .debug_bounds("library-main-editor-shell")
        .expect("editor pane is mounted");
    let panel = cx
        .debug_bounds("library-find-in-note-panel")
        .expect("find panel stays in the editor pane");
    assert!(
        panel.left() >= pane.left()
            && panel.right() <= pane.right()
            && panel.top() >= pane.top()
            && panel.bottom() <= pane.bottom(),
        "900px window keeps the wrapped Find controls inside the editor pane: panel={panel:?}, pane={pane:?}"
    );
    let input_bounds = cx
        .debug_bounds("library-find-in-note-input")
        .expect("find input viewport is mounted");
    let canvas_bounds = cx
        .debug_bounds("library-find-in-note-input-canvas")
        .expect("find input canvas is mounted");
    assert_eq!(input_bounds.size.width, canvas_bounds.size.width);
    let find_input = view.read_with(cx, |shell, _| shell.find_input.clone());
    let long_query = "长".repeat(80);
    cx.simulate_input(&long_query);
    redraw(cx);
    let visible_input = cx
        .debug_bounds("library-find-in-note-input")
        .expect("find input viewport is mounted");
    find_input.update(cx, |input, input_cx| {
        input.move_horizontal(false, true);
        input_cx.notify();
    });
    redraw(cx);
    let candidate_bounds = cx.update(|window, app| {
        find_input.update(app, |input, input_cx| {
            <TitleInput as EntityInputHandler>::bounds_for_range(
                input,
                79..80,
                visible_input,
                window,
                input_cx,
            )
        })
    });
    let candidate_bounds = candidate_bounds.expect("IME candidate range has visible bounds");
    assert!(
        candidate_bounds.left() >= visible_input.left()
            && candidate_bounds.right() <= visible_input.right(),
        "long-query IME candidate remains inside the translated input viewport"
    );
    let offscreen_candidate_bounds = cx.update(|window, app| {
        find_input.update(app, |input, input_cx| {
            <TitleInput as EntityInputHandler>::bounds_for_range(
                input,
                0..1,
                visible_input,
                window,
                input_cx,
            )
        })
    });
    let offscreen_candidate_bounds =
        offscreen_candidate_bounds.expect("offscreen IME range bounds");
    assert!(
        offscreen_candidate_bounds.left() >= visible_input.left()
            && offscreen_candidate_bounds.right() <= visible_input.right(),
        "offscreen-left IME range is clamped without inverted bounds"
    );
    find_input.update(cx, |input, input_cx| {
        input.select_all();
        input_cx.notify();
    });
    cx.simulate_input("会议");
    redraw(cx);
    let (editor, undo_depth) = view.read_with(cx, |shell, app| {
        let editor = shell
            .note_session
            .as_ref()
            .unwrap()
            .read(app)
            .editor()
            .clone();
        assert_eq!(
            editor.read(app).find_summary().total,
            2,
            "CJK input is found literally"
        );
        assert_eq!(editor.read(app).find_summary().primary_index, Some(0));
        (editor.clone(), editor.read(app).undo_depth())
    });
    cx.simulate_keystrokes("cmd-g");
    redraw(cx);
    assert_eq!(
        editor.read_with(cx, |editor, _| editor.find_summary().primary_index),
        Some(1)
    );
    cx.simulate_keystrokes("cmd-shift-g");
    redraw(cx);
    assert_eq!(
        editor.read_with(cx, |editor, _| editor.find_summary().primary_index),
        Some(0)
    );

    cx.update(|window, app| {
        find_input.update(app, |input, input_cx| {
            input.select_all();
            <TitleInput as EntityInputHandler>::replace_and_mark_text_in_range(
                input,
                None,
                "无",
                Some(0..1),
                window,
                input_cx,
            );
        });
    });
    assert!(find_input.read_with(cx, |input, _| input.marked_range().is_some()));
    assert_eq!(
        editor.read_with(cx, |editor, _| editor.find_summary().total),
        2,
        "IME marked text must not replace the live query or scroll"
    );
    // Closing and reopening must not turn an in-flight native composition
    // into a committed find query merely because the panel remounts.
    cx.simulate_keystrokes("escape");
    redraw(cx);
    cx.simulate_keystrokes("cmd-f");
    redraw(cx);
    assert!(find_input.read_with(cx, |input, _| input.marked_range().is_some()));
    assert_eq!(
        editor.read_with(cx, |editor, _| editor.find_summary().total),
        2,
        "Escape → Cmd-F retains provisional composition"
    );
    cx.simulate_keystrokes("cmd-k");
    redraw(cx);
    cx.simulate_keystrokes("cmd-f");
    redraw(cx);
    assert!(find_input.read_with(cx, |input, _| input.marked_range().is_some()));
    assert_eq!(
        editor.read_with(cx, |editor, _| editor.find_summary().total),
        2,
        "Cmd-K → Cmd-F retains provisional composition"
    );
    cx.update(|window, app| {
        view.update(app, |shell, shell_cx| {
            shell.toggle_find_case_sensitive(shell_cx);
            shell.toggle_find_in_note_visibility(window, shell_cx);
        });
    });
    view.read_with(cx, |shell, app| {
        assert!(
            !shell.find_case_sensitive,
            "Aa leaves provisional IME alone"
        );
        assert!(
            shell.find_input.read(app).marked_range().is_some(),
            "repeated Cmd-F cannot select away marked composition"
        );
    });
    assert_eq!(
        editor.read_with(cx, |editor, _| editor.find_summary().total),
        2,
        "central refresh guard keeps the committed find query while marked"
    );
    cx.update(|window, app| {
        find_input.update(app, |input, input_cx| {
            <TitleInput as EntityInputHandler>::unmark_text(input, window, input_cx);
        });
    });
    assert_eq!(
        editor.read_with(cx, |editor, _| editor.find_summary().total),
        0
    );
    cx.update(|window, app| {
        find_input.update(app, |input, input_cx| {
            input.select_all();
            <TitleInput as EntityInputHandler>::replace_text_in_range(
                input, None, "会议", window, input_cx,
            );
        });
    });
    assert_eq!(
        editor.read_with(cx, |editor, _| editor.find_summary().total),
        2
    );

    cx.update(|window, app| focus_editor(&editor, window, app));
    cx.simulate_keystrokes("escape");
    redraw(cx);
    view.read_with(cx, |shell, _| assert!(!shell.find_panel_open));
    assert_eq!(
        editor.read_with(cx, |editor, _| editor.find_summary().total),
        2
    );
    assert_eq!(
        editor.read_with(cx, |editor, _| editor.undo_depth()),
        undo_depth
    );
    assert_eq!(
        repository.load_note(&note.id).unwrap().unwrap().body_html,
        original_html,
        "find stays outside saved HTML"
    );

    cx.simulate_keystrokes("cmd-f");
    redraw(cx);
    view.read_with(cx, |shell, app| {
        assert_eq!(
            shell.find_input.read(app).text(),
            "会议",
            "same note reuses its query"
        );
    });
    cx.simulate_keystrokes("cmd-k");
    redraw(cx);
    assert!(
        cx.debug_bounds("library-search-palette").is_some(),
        "Cmd-K stays global search"
    );
    view.read_with(cx, |shell, _| {
        assert!(
            !shell.find_panel_open,
            "Cmd-K owns the visible input instead of leaving Find under its backdrop"
        )
    });
    cx.simulate_keystrokes("cmd-f");
    redraw(cx);
    view.read_with(cx, |shell, _| {
        assert!(
            !shell.search_palette_open,
            "Cmd-F removes the obscuring global palette"
        );
        assert!(shell.find_panel_open);
    });
    cx.simulate_input("g");
    redraw(cx);
    view.read_with(cx, |shell, app| {
        assert_eq!(shell.find_input.read(app).text(), "g")
    });
    cx.simulate_keystrokes("escape");
    redraw(cx);
    view.read_with(cx, |shell, _| assert!(!shell.find_panel_open));
    cx.update(|window, app| {
        assert!(
            editor.read(app).focus_handle().is_focused(window),
            "Find → Cmd-K → Cmd-F → Escape restores the original editor focus"
        );
    });
    redraw(cx);

    cx.update(|window, app| {
        view.update(app, |shell, shell_cx| {
            shell.apply_action(
                AppAction::SelectNote(other_note.id.clone()),
                window,
                shell_cx,
            );
        });
    });
    redraw(cx);
    assert_eq!(
        editor.read_with(cx, |editor, _| editor.find_summary().total),
        0
    );
    view.read_with(cx, |shell, app| {
        assert!(!shell.find_panel_open);
        assert!(shell.find_input.read(app).text().is_empty());
    });
}

#[gpui::test]
async fn mounted_find_panel_shows_a_complete_ten_thousand_match_count(cx: &mut TestAppContext) {
    cx.update(bind_library_keybindings);
    let (_profile, repository) = repository();
    let body = "命中 ".repeat(10_000);
    let note = repository
        .create_note(CreateNote {
            title: "大量命中".into(),
            notebook_id: None,
            document: rich_document(&body),
        })
        .expect("create 10k find fixture");
    let (view, cx) = mount_shell(Arc::clone(&repository), cx);
    cx.update(|window, app| {
        view.update(app, |shell, shell_cx| {
            shell.apply_action(AppAction::SelectNote(note.id.clone()), window, shell_cx);
        });
    });
    redraw(cx);
    cx.simulate_keystrokes("cmd-f");
    cx.simulate_input("命中");
    redraw(cx);
    view.read_with(cx, |shell, app| {
        let editor = shell
            .note_session
            .as_ref()
            .unwrap()
            .read(app)
            .editor()
            .clone();
        assert_eq!(editor.read(app).find_summary().total, 10_000);
    });
    let summary = cx
        .debug_bounds("library-find-in-note-summary")
        .expect("visible find summary");
    assert!(summary.size.width >= px(90.0));
}

#[gpui::test]
async fn cmd_k_palette_mounts_above_the_retained_editor_without_changing_session(
    cx: &mut TestAppContext,
) {
    let (_profile, repository) = repository();
    let note = repository
        .create_note(CreateNote {
            title: "可搜索".into(),
            notebook_id: None,
            document: rich_document("本地关键词"),
        })
        .expect("create note");
    let (view, cx) = mount_shell(repository, cx);
    cx.update(|window, app| {
        view.update(app, |shell, shell_cx| {
            shell.apply_action(AppAction::SelectNote(note.id.clone()), window, shell_cx);
        });
    });
    redraw(cx);
    let before = view.read_with(cx, |shell, _| shell.note_session.clone().expect("session"));
    cx.dispatch_action(ToggleSearchPalette);
    redraw(cx);
    assert!(cx.debug_bounds("library-search-palette").is_some());
    view.read_with(cx, |shell, _| {
        assert!(shell.search_palette_open);
        assert_eq!(shell.note_session.as_ref(), Some(&before));
    });
}

#[gpui::test]
async fn search_palette_escape_and_backdrop_restore_the_original_focus_and_session(
    cx: &mut TestAppContext,
) {
    cx.update(bind_library_keybindings);
    let (_profile, repository) = repository();
    let note = repository
        .create_note(CreateNote {
            title: "焦点笔记".into(),
            notebook_id: None,
            document: rich_document("保持会话"),
        })
        .expect("create note");
    let (view, cx) = mount_shell(repository, cx);
    cx.update(|window, app| {
        view.update(app, |shell, shell_cx| {
            shell.apply_action(AppAction::SelectNote(note.id.clone()), window, shell_cx);
        });
    });
    redraw(cx);
    let (session_id, title) = view.read_with(cx, |shell, app| {
        let session = shell.note_session.as_ref().unwrap();
        (session.entity_id(), session.read(app).title().clone())
    });
    cx.update(|window, app| title.read(app).focus_handle().focus(window));
    cx.update(|window, app| {
        view.update(app, |shell, shell_cx| {
            shell.toggle_search_palette_visibility(window, shell_cx);
        });
    });
    redraw(cx);
    assert!(cx.debug_bounds("library-search-palette").is_some());
    cx.simulate_keystrokes("escape");
    redraw(cx);
    view.read_with(cx, |shell, app| {
        assert!(!shell.search_palette_open);
        assert_eq!(shell.note_session.as_ref().unwrap().entity_id(), session_id);
        let _ = app;
    });
    assert!(cx.update(|window, app| title.read(app).focus_handle().is_focused(window)));

    cx.update(|window, app| {
        view.update(app, |shell, shell_cx| {
            shell.toggle_search_palette_visibility(window, shell_cx);
        });
    });
    redraw(cx);
    let backdrop = cx.debug_bounds("library-search-backdrop").unwrap();
    // The palette is centered inside its backdrop; click an exposed corner,
    // not its center, so this exercises the actual backdrop handler.
    cx.simulate_click(
        point(backdrop.left() + px(4.0), backdrop.top() + px(4.0)),
        Modifiers::default(),
    );
    redraw(cx);
    view.read_with(cx, |shell, app| {
        assert!(!shell.search_palette_open);
        assert_eq!(shell.note_session.as_ref().unwrap().entity_id(), session_id);
        let _ = app;
    });
    assert!(cx.update(|window, app| title.read(app).focus_handle().is_focused(window)));
}

#[gpui::test]
async fn search_palette_preserves_link_popover_focus_on_escape_and_backdrop(
    cx: &mut TestAppContext,
) {
    cx.update(|app| crate::components::init(app));
    let (_profile, repository) = repository();
    let note = repository
        .create_note(CreateNote {
            title: "链接搜索焦点".into(),
            notebook_id: None,
            document: rich_document("需要链接的文字"),
        })
        .unwrap();
    let (view, cx) = mount_shell(repository, cx);
    cx.simulate_resize(gpui::size(px(1400.0), px(820.0)));
    redraw(cx);
    cx.update(|window, app| {
        view.update(app, |shell, shell_cx| {
            shell.apply_action(AppAction::SelectNote(note.id.clone()), window, shell_cx)
        });
    });
    redraw(cx);
    let (session_id, editor, selection, undo_depth) = view.update(cx, |shell, shell_cx| {
        let session = shell.note_session.as_ref().unwrap().clone();
        let editor = session.read(shell_cx).editor().clone();
        let selection = editor.update(shell_cx, |editor, editor_cx| {
            let block = editor.document().blocks().first().unwrap();
            let selection = Selection::new(
                DocPoint::with_affinity(block.id, 0, Affinity::Before),
                DocPoint::with_affinity(
                    block.id,
                    block.content.as_text().unwrap().len(),
                    Affinity::After,
                ),
            );
            editor.set_selection_for_test(selection);
            editor_cx.notify();
            selection
        });
        (
            session.entity_id(),
            editor.clone(),
            selection,
            editor.read(shell_cx).undo_depth(),
        )
    });
    let link = cx.debug_bounds("Link").unwrap();
    cx.simulate_click(link.center(), Modifiers::default());
    redraw(cx);
    let chrome_before = view.read_with(cx, |shell, _| {
        shell.command_chrome.as_ref().unwrap().entity_id()
    });
    for dismiss_with_backdrop in [false, true] {
        let focus = view.read_with(cx, |shell, app| {
            let popover = shell
                .command_chrome
                .as_ref()
                .unwrap()
                .read(app)
                .link_popover_for_test()
                .unwrap();
            popover.read(app).focus.clone()
        });
        cx.update(|window, app| {
            assert!(focus.is_focused(window));
            view.update(app, |shell, shell_cx| {
                shell.toggle_search_palette_visibility(window, shell_cx)
            });
        });
        redraw(cx);
        view.read_with(cx, |shell, app| {
            let chrome = shell.command_chrome.as_ref().unwrap();
            assert!(chrome.read(app).has_link_popover(), "opening Cmd-K lost Link: session={:?}/{:?}, chrome={:?}/{:?}, surface={:?}, active_revision={:?}, session_revision={:?}", session_id, shell.note_session.as_ref().unwrap().entity_id(), chrome_before, chrome.entity_id(), shell.surface_note_id, shell.model.read(app).active_note().map(|note| note.revision), shell.note_session.as_ref().map(|session| session.read(app).expected_revision()));
        });
        if dismiss_with_backdrop {
            let backdrop = cx.debug_bounds("library-search-backdrop").unwrap();
            cx.simulate_click(
                point(backdrop.left() + px(4.0), backdrop.top() + px(4.0)),
                Modifiers::default(),
            );
        } else {
            cx.simulate_keystrokes("escape");
        }
        redraw(cx);
        view.read_with(cx, |shell, app| {
            assert!(
                shell
                    .command_chrome
                    .as_ref()
                    .unwrap()
                    .read(app)
                    .has_link_popover(),
                "Link disappeared after {} dismissal",
                if dismiss_with_backdrop {
                    "backdrop"
                } else {
                    "Escape"
                }
            );
            assert_eq!(shell.note_session.as_ref().unwrap().entity_id(), session_id);
            assert_eq!(editor.read(app).selection(), selection);
            assert_eq!(editor.read(app).undo_depth(), undo_depth);
        });
        assert!(cx.update(|window, _| focus.is_focused(window)));
    }
}

#[gpui::test]
async fn mounted_cmd_k_search_ignores_marked_enter_and_opens_the_thirteenth_result(
    cx: &mut TestAppContext,
) {
    cx.update(bind_library_keybindings);
    let (_profile, repository) = repository();
    let retained = repository
        .create_note(CreateNote {
            title: "保持编辑器会话".into(),
            notebook_id: None,
            document: rich_document("原笔记正文"),
        })
        .expect("create retained note");
    for index in 0..15 {
        repository
            .create_note(CreateNote {
                title: format!("needle result {index:02}"),
                notebook_id: None,
                document: rich_document("needle local body"),
            })
            .expect("create searchable fixture");
    }
    repository
        .process_search_jobs()
        .expect("index fixtures before mounted search");
    let (view, cx) = mount_shell(repository, cx);
    redraw(cx);
    cx.update(|window, app| {
        view.update(app, |shell, shell_cx| {
            shell.apply_action(AppAction::SelectNote(retained.id.clone()), window, shell_cx);
        });
    });
    redraw(cx);
    let session_before = view.read_with(cx, |shell, _| {
        shell
            .note_session
            .as_ref()
            .expect("retained session")
            .entity_id()
    });

    cx.simulate_keystrokes("cmd-k");
    redraw(cx);
    cx.simulate_input("needle");
    cx.run_until_parked();
    redraw(cx);
    let input = view.read_with(cx, |shell, _| shell.search_input.clone());
    cx.update(|window, app| {
        input.update(app, |input, input_cx| {
            let end = input.text().encode_utf16().count();
            <TitleInput as EntityInputHandler>::replace_and_mark_text_in_range(
                input,
                Some(end..end),
                "候选",
                Some(2..2),
                window,
                input_cx,
            );
        });
    });
    assert!(
        input.read_with(cx, |input, _| input.marked_range().is_some()),
        "test must install a real marked native-input range"
    );
    let marked_enter = KeyDownEvent {
        keystroke: Keystroke::parse("enter").expect("valid Enter"),
        is_held: false,
    };
    cx.update(|window, app| {
        view.update(app, |shell, shell_cx| {
            shell.on_search_key_down(&marked_enter, window, shell_cx);
        });
    });
    redraw(cx);
    view.read_with(cx, |shell, app| {
        assert!(
            shell.search_palette_open,
            "marked Enter must stay in the palette"
        );
        assert_eq!(shell.model.read(app).navigation().search_query(), None);
        assert_eq!(
            shell
                .note_session
                .as_ref()
                .expect("same session")
                .entity_id(),
            session_before,
            "opening/composing in search must not remount the editor"
        );
    });

    cx.update(|window, app| {
        input.update(app, |input, input_cx| {
            <TitleInput as EntityInputHandler>::unmark_text(input, window, input_cx);
            input.select_all();
            <TitleInput as EntityInputHandler>::replace_text_in_range(
                input, None, "needle", window, input_cx,
            );
        });
    });
    cx.run_until_parked();
    redraw(cx);
    for _ in 0..13 {
        cx.simulate_keystrokes("down");
    }
    let expected = view.read_with(cx, |shell, _| {
        assert!(
            shell.search_palette_results.len() >= 14,
            "all results are navigable"
        );
        assert_eq!(shell.search_palette_selected, 13);
        shell.search_palette_results[13].note.id.clone()
    });
    cx.simulate_keystrokes("enter");
    redraw(cx);
    view.read_with(cx, |shell, app| {
        assert!(!shell.search_palette_open);
        let model = shell.model.read(app);
        assert_eq!(model.navigation().search_query(), Some("needle"));
        assert_eq!(model.navigation().selected_note_id(), Some(&expected));
    });
}

#[gpui::test]
async fn search_palette_keyboard_reveals_a_wrapping_tail_row_in_the_real_scroll_viewport(
    cx: &mut TestAppContext,
) {
    cx.update(bind_library_keybindings);
    let (_profile, repository) = repository();
    for index in 0..SearchQuery::MAX_PAGE_SIZE {
        repository
            .create_note(CreateNote {
                title: format!(
                    "needle {index:03} 这是一条会在窄调色板中换行的很长标题，用于验证真实几何滚动"
                ),
                notebook_id: None,
                document: rich_document("needle 这是一段也会换行的摘要内容，以避免固定行高估算。"),
            })
            .expect("create wrapping search fixture");
    }
    for _ in 0..32 {
        if !repository
            .has_pending_search_jobs()
            .expect("inspect fixture search queue")
        {
            break;
        }
        repository
            .process_search_jobs()
            .expect("index one bounded fixture batch");
    }
    assert!(
        !repository
            .has_pending_search_jobs()
            .expect("fixture indexing drained"),
        "the mounted 500-row geometry test needs every real fixture hit indexed"
    );
    let (view, cx) = mount_shell(repository, cx);
    redraw(cx);
    cx.simulate_keystrokes("cmd-k");
    redraw(cx);
    cx.simulate_input("needle");
    cx.run_until_parked();
    redraw(cx);
    let tail_id = view.read_with(cx, |shell, _| {
        assert_eq!(
            shell.search_palette_results.len(),
            SearchQuery::MAX_PAGE_SIZE,
            "the bounded repository packet must mount all 500 real hits"
        );
        let unique = shell
            .search_palette_results
            .iter()
            .map(|hit| hit.note.id.clone())
            .collect::<std::collections::HashSet<_>>();
        assert_eq!(unique.len(), SearchQuery::MAX_PAGE_SIZE);
        shell.search_palette_results[SearchQuery::MAX_PAGE_SIZE - 1]
            .note
            .id
            .clone()
    });
    for _ in 0..(SearchQuery::MAX_PAGE_SIZE - 1) {
        cx.simulate_keystrokes("down");
    }
    redraw(cx);
    // `scroll_to_item` is applied during GPUI prepaint, then the resulting
    // child geometry is available on the following draw.
    redraw(cx);

    let (viewport, tail, offset) = view.read_with(cx, |shell, _| {
        assert_eq!(
            shell.search_palette_scroll.children_count(),
            SearchQuery::MAX_PAGE_SIZE,
            "the tracked scroll container must expose every bounded result as a direct child"
        );
        (
            shell.search_palette_scroll.bounds(),
            shell
                .search_palette_scroll
                .bounds_for_item(SearchQuery::MAX_PAGE_SIZE - 1)
                .expect("mounted tail row remains mouse reachable"),
            shell.search_palette_scroll.offset(),
        )
    });
    assert!(
        tail.bottom() + offset.y > viewport.top() && tail.top() + offset.y < viewport.bottom(),
        "the keyboard-selected wrapping tail row must intersect the painted scroll viewport"
    );
    view.read_with(cx, |shell, _| {
        assert_eq!(
            shell.search_palette_selected,
            SearchQuery::MAX_PAGE_SIZE - 1
        );
    });
    cx.simulate_click(
        point(tail.center().x, tail.center().y + offset.y),
        Modifiers::default(),
    );
    redraw(cx);
    view.read_with(cx, |shell, app| {
        assert!(
            !shell.search_palette_open,
            "the painted tail row is mouse clickable"
        );
        assert_eq!(
            shell.model.read(app).navigation().selected_note_id(),
            Some(&tail_id),
            "tail click must select the real tail NoteId"
        );
    });
}

#[gpui::test]
async fn search_palette_escape_restores_an_open_organization_input_and_its_panel(
    cx: &mut TestAppContext,
) {
    cx.update(bind_library_keybindings);
    let (_profile, repository) = repository();
    let (view, cx) = mount_shell(repository, cx);
    cx.update(|window, app| {
        view.update(app, |shell, shell_cx| {
            shell.organization_panel_open = true;
            shell
                .organization_input
                .read(shell_cx)
                .focus_handle()
                .focus(window);
        });
    });
    redraw(cx);
    cx.update(|window, app| {
        view.update(app, |shell, shell_cx| {
            shell.toggle_search_palette_visibility(window, shell_cx);
        });
    });
    redraw(cx);
    cx.simulate_keystrokes("escape");
    redraw(cx);
    view.read_with(cx, |shell, _| {
        assert!(
            shell.organization_panel_open,
            "closing the palette must restore the panel containing the prior focus"
        );
    });
    assert!(cx.update(|window, app| {
        view.read(app)
            .organization_input
            .read(app)
            .focus_handle()
            .is_focused(window)
    }));
}

#[gpui::test]
async fn history_search_discards_old_packet_after_same_generation_autosave(
    cx: &mut TestAppContext,
) {
    // This starts with a legal SearchRoute(A), returns to All Notes(A), then
    // an external revision removes the old FTS match. The mounted editor is
    // rehydrated at that revision and types the match back in. Thus the held
    // packet genuinely omits A; removing expected_revision from the fence
    // makes Forward clear the retained editor when this continuation resumes.
    let (_profile, repository) = repository();
    let note = repository
        .create_note(CreateNote {
            title: "旧搜索包围栏".into(),
            notebook_id: None,
            document: rich_document("needle initial"),
        })
        .expect("create note");
    repository
        .process_search_jobs()
        .expect("index initial match");
    let clock = Arc::new(ManualSaveClock::default());
    let (view, cx) = mount_shell_with_save_clock(Arc::clone(&repository), Arc::clone(&clock), cx);
    redraw(cx);
    cx.update(|window, app| {
        view.update(app, |shell, shell_cx| {
            shell.apply_action(AppAction::SelectNote(note.id.clone()), window, shell_cx);
            shell.model.update(shell_cx, |model, _| {
                let mut query = SearchQuery::parse("needle");
                query.set_page(0, SearchQuery::MAX_PAGE_SIZE).unwrap();
                let generation = model.begin_search("needle");
                model
                    .commit_search_results(
                        generation,
                        "needle".into(),
                        repository.search(query).expect("initial FTS packet"),
                        Some(note.id.clone()),
                    )
                    .expect("commit initial SearchRoute");
                model
                    .dispatch(AppAction::NavigateBack)
                    .expect("back to All Notes");
            });
        });
    });
    redraw(cx);

    let durable = repository.load_note(&note.id).unwrap().unwrap();
    repository
        .save_note(SaveNote {
            id: note.id.clone(),
            expected_revision: durable.revision,
            title: durable.title,
            document: rich_document("external content without match"),
            resource_ids: durable.resource_ids,
            selected_thumbnail_id: None,
        })
        .expect("external revision removes old FTS match");
    repository
        .process_search_jobs()
        .expect("index removed match");
    view.update(cx, |shell, shell_cx| {
        shell.model.update(shell_cx, |model, model_cx| {
            model
                .reload_active_session_for_test()
                .expect("rehydrate current All Notes session");
            model_cx.notify();
        });
    });
    redraw(cx);
    let session = view.read_with(cx, |shell, _| {
        shell.note_session.clone().expect("rehydrated session")
    });
    assert_eq!(
        session.read_with(cx, |session, _| session.expected_revision()),
        2,
        "the external revision is rehydrated before the mounted edit"
    );
    let save_release = session.update(cx, |session, _| {
        session.enable_deadline_tasks_for_test();
        session.stall_next_background_save_for_test()
    });
    let surface = cx.debug_bounds("native-editor-surface").unwrap();
    cx.simulate_click(surface.center(), Modifiers::default());
    cx.simulate_input(" needle restored by mounted editor");
    let (generation, revision, selection, undo_depth) = session.read_with(cx, |session, app| {
        let editor = session.editor().read(app);
        (
            session.save_generation(),
            session.expected_revision(),
            editor.selection(),
            editor.undo_depth(),
        )
    });
    let (read_sender, read_receiver) = mpsc::channel();
    let (release_sender, release_receiver) = futures::channel::oneshot::channel();
    view.update(cx, |shell, _| {
        shell.install_search_completion_gate_for_test(SearchCompletionGate {
            read: read_sender,
            release: release_receiver,
        });
    });
    view.read_with(cx, |shell, app| {
        assert_eq!(
            shell.model.read(app).pending_history_search_query(true),
            Some("needle".into()),
            "the original committed SearchRoute must remain the Forward target"
        );
    });
    view.update(cx, |shell, shell_cx| {
        assert!(
            shell.schedule_history_search(true, shell_cx),
            "the real history worker accepts the retained Forward target"
        );
    });
    cx.run_until_parked();
    read_receiver
        .recv_timeout(Duration::from_secs(2))
        .expect("old FTS packet read before save completion");
    clock.advance(Duration::from_millis(100));
    cx.executor().advance_clock(Duration::from_millis(100));
    cx.run_until_parked();
    save_release.send(()).expect("release journal save");
    cx.run_until_parked();
    clock.advance(Duration::from_millis(500));
    cx.executor().advance_clock(Duration::from_millis(500));
    cx.run_until_parked();
    let (after_generation, after_revision, state) = session.read_with(cx, |session, _| {
        (
            session.save_generation(),
            session.expected_revision(),
            session.save_state(),
        )
    });
    assert_eq!(
        after_generation, generation,
        "autosave stays in the dirty generation"
    );
    assert!(
        after_revision > revision,
        "same generation durable save advances revision; before={revision}, after={after_revision}, state={state:?}"
    );
    assert!(matches!(
        state,
        crate::app::save_coordinator::SaveState::Clean
    ));
    release_sender
        .send(())
        .expect("release stale query completion");
    cx.run_until_parked();
    redraw(cx);
    view.read_with(cx, |shell, app| {
        assert_eq!(shell.model.read(app).navigation().search_query(), None);
        assert_eq!(
            shell.model.read(app).navigation().selected_note_id(),
            Some(&note.id)
        );
        assert_eq!(
            shell.note_session.as_ref().unwrap().entity_id(),
            session.entity_id()
        );
        let editor = session.read(app).editor().read(app);
        assert_eq!(editor.selection(), selection);
        assert!(editor.undo_depth() >= undo_depth);
    });
}

#[gpui::test]
async fn mounted_search_refresh_error_retry_click_keeps_old_cards_then_recovers(
    cx: &mut TestAppContext,
) {
    let (_profile, repository) = repository();
    let note = repository
        .create_note(CreateNote {
            title: "retry packet".into(),
            notebook_id: None,
            document: rich_document("needle retry"),
        })
        .expect("create search note");
    repository.process_search_jobs().expect("index search note");
    let (view, cx) = mount_shell(Arc::clone(&repository), cx);
    redraw(cx);
    view.update(cx, |shell, shell_cx| {
        shell.model.update(shell_cx, |model, model_cx| {
            let mut query = SearchQuery::parse("needle");
            query.set_page(0, SearchQuery::MAX_PAGE_SIZE).unwrap();
            let generation = model.begin_search("needle");
            model
                .commit_search_results(
                    generation,
                    "needle".into(),
                    repository.search(query).expect("initial packet"),
                    Some(note.id.clone()),
                )
                .expect("commit SearchRoute");
            model_cx.notify();
        });
    });
    redraw(cx);
    repository
        .trash_note(&note.id)
        .expect("remove the refreshed hit");
    repository.process_search_jobs().expect("index removed hit");
    view.update(cx, |shell, shell_cx| {
        shell.model.update(shell_cx, |model, model_cx| {
            model
                .refresh_projection_events([LibraryEvent::NoteTrashed(note.id.clone())])
                .expect("mark SearchRoute refresh pending");
            model.fail_next_shell_state_persist_for_test(app_lite_core::LibraryError::NotFound);
            model_cx.notify();
        });
        shell.schedule_active_search_refresh(shell_cx);
    });
    cx.run_until_parked();
    redraw(cx);
    view.read_with(cx, |shell, app| {
        assert!(
            shell
                .history_search_notice
                .as_deref()
                .is_some_and(|notice| notice.contains("本地搜索更新失败")),
            "the actual failed commit must be visible"
        );
        assert_eq!(
            shell.model.read(app).navigation().search_query(),
            Some("needle")
        );
        assert_eq!(
            shell.model.read(app).projections().len(),
            1,
            "old coherent card remains"
        );
    });
    let retry = cx
        .debug_bounds("library-search-refresh-retry")
        .expect("visible Retry control");
    cx.simulate_click(retry.center(), Modifiers::default());
    cx.run_until_parked();
    redraw(cx);
    view.read_with(cx, |shell, app| {
        assert!(
            shell.history_search_notice.is_none(),
            "successful Retry clears notice"
        );
        assert!(
            shell.model.read(app).projections().is_empty(),
            "Retry installed new bounded packet"
        );
    });
}

#[gpui::test]
async fn mounted_history_retry_click_reuses_forward_after_real_commit_failure(
    cx: &mut TestAppContext,
) {
    let (_profile, repository) = repository();
    let note = repository
        .create_note(CreateNote {
            title: "history retained editor".into(),
            notebook_id: None,
            document: rich_document("ordinary body"),
        })
        .unwrap();
    let (view, cx) = mount_shell(Arc::clone(&repository), cx);
    redraw(cx);
    cx.update(|window, app| {
        view.update(app, |shell, shell_cx| {
            shell.apply_action(AppAction::SelectNote(note.id.clone()), window, shell_cx);
        });
    });
    redraw(cx);
    let session = view.read_with(cx, |shell, _| shell.note_session.clone().unwrap());
    view.update(cx, |shell, shell_cx| {
        shell.model.update(shell_cx, |model, model_cx| {
            model.install_stale_search_history_for_test("historical missing query".into());
            model.fail_next_shell_state_persist_for_test(app_lite_core::LibraryError::NotFound);
            model_cx.notify();
        });
        assert!(shell.schedule_history_search(true, shell_cx));
    });
    cx.run_until_parked();
    redraw(cx);
    view.read_with(cx, |shell, app| {
        assert_eq!(shell.search_refresh_retry_history, Some(true));
        assert!(
            shell
                .history_search_notice
                .as_deref()
                .is_some_and(|n| n.contains("无法恢复"))
        );
        assert_eq!(shell.model.read(app).navigation().search_query(), None);
        assert_eq!(
            shell.note_session.as_ref().unwrap().entity_id(),
            session.entity_id()
        );
    });
    let retry = cx.debug_bounds("library-search-refresh-retry").unwrap();
    cx.simulate_click(retry.center(), Modifiers::default());
    cx.run_until_parked();
    redraw(cx);
    view.read_with(cx, |shell, app| {
        assert!(shell.history_search_notice.is_none());
        assert_eq!(
            shell.model.read(app).navigation().search_query(),
            Some("historical missing query")
        );
        assert_eq!(shell.search_refresh_retry_history, None);
    });
}

#[test]
fn pending_repository_events_coalesce_multi_batch_receiver_bursts_to_a_fixed_packet() {
    // Mutation-sensitive: a lifecycle barrier can hold an event packet for
    // several timer turns. If the bridge stores every sender message rather
    // than semantic representatives, long IME/resource work turns the UI
    // task into an unbounded queue. The production buffer must retain only
    // the five projection-transition kinds while still draining every batch.
    let (sender, receiver) = mpsc::channel();
    for index in 0..384_u32 {
        sender
            .send(app_lite_core::LibraryEvent::NoteProjectionChanged(
                app_lite_core::NoteId::parse(format!("{index:032x}")).expect("opaque fixture id"),
            ))
            .expect("queue projection event");
    }
    sender
        .send(app_lite_core::LibraryEvent::OrganizationChanged)
        .expect("queue organization event");

    let mut pending = PendingRepositoryEvents::default();
    for _ in 0..4 {
        pending.drain(&receiver);
        assert!(
            pending.events().len() <= 5,
            "one shell may retain only its fixed semantic event packet"
        );
    }
    let events = pending.events();
    assert_eq!(
        events
            .iter()
            .filter(|event| matches!(event, app_lite_core::LibraryEvent::NoteProjectionChanged(_)))
            .count(),
        1,
        "all projection events coalesce to one representative while blocked"
    );
    assert!(
        events
            .iter()
            .any(|event| matches!(event, app_lite_core::LibraryEvent::OrganizationChanged))
    );
    assert_eq!(receiver.try_recv(), Err(TryRecvError::Empty));
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
async fn mounted_create_note_from_trash_moves_to_all_notes_before_mounting_an_editable_session(
    cx: &mut TestAppContext,
) {
    // Exercise the actual Cmd-N handler. A new ordinary note must never be
    // mounted below a Trash route/card list, where it would inherit Restore /
    // Purge controls despite being an editable live note.
    cx.update(bind_library_keybindings);
    let (_profile, repository) = repository();
    let trashed = repository
        .create_note(CreateNote {
            title: "废纸篓旧笔记".into(),
            notebook_id: None,
            document: rich_document("旧正文"),
        })
        .expect("create fixture");
    repository.trash_note(&trashed.id).expect("trash fixture");
    let (view, cx) = mount_shell(Arc::clone(&repository), cx);
    redraw(cx);
    cx.update(|window, app| {
        view.update(app, |shell, shell_cx| {
            shell.apply_action(
                AppAction::NavigateTo {
                    route: LibraryRoute::Trash,
                    selected_note_id: Some(trashed.id.clone()),
                },
                window,
                shell_cx,
            );
        });
    });
    redraw(cx);

    cx.simulate_keystrokes("cmd-n");
    redraw(cx);
    view.read_with(cx, |shell, app| {
        let model = shell.model.read(app);
        let selected = model
            .navigation()
            .selected_note_id()
            .expect("fresh note selected");
        assert_eq!(model.navigation().route(), &LibraryRoute::AllNotes);
        assert!(
            model
                .projections()
                .iter()
                .any(|projection| projection.id == *selected)
        );
        let session = shell.note_session.as_ref().expect("new session mounted");
        assert!(!session.read(app).is_read_only());
        assert_eq!(
            shell
                .editor_surface
                .as_ref()
                .expect("surface")
                .read(app)
                .mode(),
            EditorSurfaceMode::Editable
        );
        assert!(shell.command_chrome.is_some());
    });
    assert!(
        cx.debug_bounds("library-organization-restore-selected")
            .is_none()
            && cx
                .debug_bounds("library-organization-purge-selected")
                .is_none(),
        "the All Notes new-note route must not retain Trash controls"
    );
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
async fn mounted_organization_revision_replaces_the_same_note_session_before_later_saves(
    cx: &mut TestAppContext,
) {
    // A note/tag relation mutation increments the durable note revision. If
    // the library only compares NoteId while reconciling the mounted editor,
    // the old NoteSession would later snapshot against a stale revision and
    // fail. This uses the production shell reducer rather than patching the
    // model directly, so deleting the revision-aware mount guard makes the
    // assertion below fail.
    let (_profile, repository) = repository();
    let note = repository
        .create_note(CreateNote {
            title: "组织版本".into(),
            notebook_id: None,
            document: rich_document("关系更新后仍可保存正文"),
        })
        .expect("create note");
    let tag = repository.create_tag("持久标签").expect("create tag");
    let (view, cx) = mount_shell(Arc::clone(&repository), cx);
    redraw(cx);
    cx.update(|window, app| {
        view.update(app, |shell, shell_cx| {
            shell.apply_action(AppAction::SelectNote(note.id.clone()), window, shell_cx)
        });
    });
    redraw(cx);
    let before = view.read_with(cx, |shell, _| {
        shell
            .note_session
            .as_ref()
            .expect("mounted note session")
            .entity_id()
    });

    cx.update(|window, app| {
        view.update(app, |shell, shell_cx| {
            shell.apply_action(
                AppAction::AddTagToSelectedNote(tag.id.clone()),
                window,
                shell_cx,
            )
        });
    });
    redraw(cx);

    view.read_with(cx, |shell, app| {
        let model = shell.model.read(app);
        assert_eq!(model.navigation().selected_note_id(), Some(&note.id));
        assert_eq!(
            model.active_note().expect("active note").tag_ids,
            vec![tag.id.clone()]
        );
        assert_ne!(
            shell
                .note_session
                .as_ref()
                .expect("replacement session")
                .entity_id(),
            before,
            "a relation revision must remount the session that owns its save fence"
        );
    });
}

#[gpui::test]
async fn mounted_organization_flushes_dirty_text_then_remounts_and_persists_later_text_after_reopen(
    cx: &mut TestAppContext,
) {
    // This exercises the production action boundary rather than replacing an
    // AppModel field in a test: a dirty session must block the first typed
    // organization action, then the save fence, metadata commit/remount, and
    // a later 100/500 ms editor checkpoint must all survive reopening.
    cx.update(|app| crate::components::init(app));
    let (profile, repository) = repository();
    let path = profile.path().join("library.sqlite");
    let note = repository
        .create_note(CreateNote {
            title: "组织保存闭环".into(),
            notebook_id: None,
            document: rich_document("基线正文"),
        })
        .expect("create note");
    let tag = repository.create_tag("闭环标签").expect("create tag");
    let clock = Arc::new(ManualSaveClock::default());
    let (view, cx) = mount_shell_with_save_clock(Arc::clone(&repository), Arc::clone(&clock), cx);
    redraw(cx);
    cx.update(|window, app| {
        view.update(app, |shell, shell_cx| {
            shell.apply_action(AppAction::SelectNote(note.id.clone()), window, shell_cx)
        });
    });
    redraw(cx);
    let first_session = view.read_with(cx, |shell, _| {
        shell
            .note_session
            .as_ref()
            .expect("selected note session")
            .clone()
    });
    first_session.update(cx, |session, _| session.enable_deadline_tasks_for_test());
    let surface = cx
        .debug_bounds("native-editor-surface")
        .expect("mounted editable surface");
    cx.simulate_click(surface.center(), Modifiers::default());
    cx.simulate_input(" 第一段必须先落库");
    redraw(cx);

    cx.update(|window, app| {
        view.update(app, |shell, shell_cx| {
            shell.apply_action(
                AppAction::AddTagToSelectedNote(tag.id.clone()),
                window,
                shell_cx,
            )
        });
    });
    redraw(cx);
    assert!(
        cx.debug_bounds("library-save-error").is_some(),
        "a dirty active editor must block the first organization mutation until its save fence completes"
    );
    assert!(
        repository
            .load_note(&note.id)
            .expect("read blocked note")
            .expect("note remains")
            .tag_ids
            .is_empty(),
        "the blocked action cannot commit metadata ahead of the editor snapshot"
    );

    clock.advance(Duration::from_millis(100));
    cx.executor().advance_clock(Duration::from_millis(100));
    cx.run_until_parked();
    clock.advance(Duration::from_millis(400));
    cx.executor().advance_clock(Duration::from_millis(400));
    cx.run_until_parked();
    redraw(cx);
    assert!(
        repository
            .load_note(&note.id)
            .expect("read first durable text")
            .expect("note remains")
            .body_text
            .contains("第一段必须先落库"),
        "the exact save boundary must durably finish before retrying the action"
    );

    cx.update(|window, app| {
        view.update(app, |shell, shell_cx| {
            shell.apply_action(
                AppAction::AddTagToSelectedNote(tag.id.clone()),
                window,
                shell_cx,
            )
        });
    });
    redraw(cx);
    let remounted_session = view.read_with(cx, |shell, app| {
        let model = shell.model.read(app);
        assert_eq!(
            model.active_note().expect("active note").tag_ids,
            vec![tag.id.clone()]
        );
        shell
            .note_session
            .as_ref()
            .expect("organization commit remounts revision fence")
            .clone()
    });
    assert_ne!(
        remounted_session.entity_id(),
        first_session.entity_id(),
        "metadata revision must replace the retained session after the first snapshot"
    );
    remounted_session.update(cx, |session, _| session.enable_deadline_tasks_for_test());
    let surface = cx
        .debug_bounds("native-editor-surface")
        .expect("remounted editor surface");
    cx.simulate_click(surface.center(), Modifiers::default());
    cx.simulate_input(" 第二段在重建后保存");
    redraw(cx);

    clock.advance(Duration::from_millis(100));
    cx.executor().advance_clock(Duration::from_millis(100));
    cx.run_until_parked();
    assert!(
        repository
            .latest_edit_journal(&note.id)
            .expect("read 100ms journal")
            .is_some(),
        "the new session must journal its later generation before the settled snapshot"
    );
    clock.advance(Duration::from_millis(400));
    cx.executor().advance_clock(Duration::from_millis(400));
    cx.run_until_parked();
    redraw(cx);
    assert!(
        repository
            .latest_edit_journal(&note.id)
            .expect("journal after settled snapshot")
            .is_none(),
        "the 500ms settled snapshot clears its owned journal"
    );

    let reopened = LibraryRepository::open(path).expect("reopen durable library");
    let persisted = reopened
        .load_note(&note.id)
        .expect("load reopened note")
        .expect("note remains after reopen");
    assert!(persisted.body_text.contains("第一段必须先落库"));
    assert!(persisted.body_text.contains("第二段在重建后保存"));
    assert_eq!(persisted.tag_ids, vec![tag.id]);
    assert!(
        persisted.revision > note.revision,
        "every durable stage must leave the reopened CAS fence newer than the original note"
    );
}

#[gpui::test]
async fn mounted_organization_event_recovery_remounts_the_same_note_at_its_committed_revision(
    cx: &mut TestAppContext,
) {
    // The first candidate failure happens after SQLite committed the tag
    // relation. The next real 50ms repository event must rebuild the entire
    // active packet and remount the editor by revision, not merely refresh a
    // card with the same NoteId.
    let (_profile, repository) = repository();
    let note = repository
        .create_note(CreateNote {
            title: "事件重建会话".into(),
            notebook_id: None,
            document: rich_document("版本 N 正文"),
        })
        .expect("create note");
    let tag = repository.create_tag("事件重建标签").expect("create tag");
    let (view, cx) = mount_shell(Arc::clone(&repository), cx);
    redraw(cx);
    cx.update(|window, app| {
        view.update(app, |shell, shell_cx| {
            shell.apply_action(AppAction::SelectNote(note.id.clone()), window, shell_cx)
        });
    });
    redraw(cx);
    let before_session = view.read_with(cx, |shell, _| {
        shell
            .note_session
            .as_ref()
            .expect("mounted session")
            .entity_id()
    });
    view.update(cx, |shell, shell_cx| {
        shell.model.update(shell_cx, |model, _| {
            model.fail_next_refresh_for_test(app_lite_core::LibraryError::NotFound);
        });
    });
    cx.update(|window, app| {
        view.update(app, |shell, shell_cx| {
            shell.apply_action(
                AppAction::AddTagToSelectedNote(tag.id.clone()),
                window,
                shell_cx,
            )
        });
    });
    redraw(cx);
    view.read_with(cx, |shell, app| {
        assert_eq!(
            shell
                .note_session
                .as_ref()
                .expect("failed candidate retains old session")
                .entity_id(),
            before_session
        );
        assert!(matches!(
            shell.model.read(app).status(),
            AppStatus::Error(message) if message.contains("资料库数据已提交")
        ));
    });

    cx.executor().advance_clock(Duration::from_millis(60));
    cx.run_until_parked();
    redraw(cx);
    let committed = repository
        .load_note(&note.id)
        .expect("load durable tag relation")
        .expect("note remains");
    view.read_with(cx, |shell, app| {
        let model = shell.model.read(app);
        assert_eq!(model.status(), &AppStatus::Ready);
        assert_eq!(model.active_note(), Some(&committed));
        assert_ne!(
            shell
                .note_session
                .as_ref()
                .expect("event reconciliation remounts session")
                .entity_id(),
            before_session,
            "the same NoteId at a newer organization revision needs a fresh save fence"
        );
    });
}

#[gpui::test]
async fn mounted_committed_trash_refresh_failure_freezes_the_old_session_until_the_queued_event_reconciles(
    cx: &mut TestAppContext,
) {
    // A trash transaction can commit while its first candidate refresh fails.
    // The old All Notes packet is then intentionally retained for honest
    // recovery, but it must not remain a fake editable rev-N session: a
    // same-card click cannot clear the warning and a body key event cannot
    // create text that the later NoteTrashed event silently drops.
    let (_profile, repository) = repository();
    let note = repository
        .create_note(CreateNote {
            title: "待协调的废纸篓笔记".into(),
            notebook_id: None,
            document: rich_document("提交前正文"),
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
    let original_session = view.read_with(cx, |shell, _| {
        shell
            .note_session
            .as_ref()
            .expect("selected note has a session")
            .entity_id()
    });
    view.update(cx, |shell, shell_cx| {
        shell.model.update(shell_cx, |model, _| {
            model.fail_next_refresh_for_test(app_lite_core::LibraryError::NotFound);
        });
    });
    cx.update(|window, app| {
        view.update(app, |shell, shell_cx| {
            shell.apply_action(AppAction::TrashSelected, window, shell_cx)
        });
    });
    redraw(cx);

    let surface = cx
        .debug_bounds("native-editor-surface")
        .expect("the retained recovery surface remains visible");
    cx.simulate_click(surface.center(), Modifiers::default());
    cx.simulate_input("这段文本绝不能进入已删除旧会话");
    redraw(cx);
    view.read_with(cx, |shell, app| {
        let session = shell
            .note_session
            .as_ref()
            .expect("candidate failure retains the recovery session");
        assert_eq!(session.entity_id(), original_session);
        assert!(
            !session
                .read(app)
                .editor()
                .read(app)
                .document()
                .blocks()
                .iter()
                .filter_map(|block| block.content.as_text())
                .any(|text| text.contains("绝不能进入")),
            "a committed-but-unreconciled session must fail closed before the event remount"
        );
        assert!(matches!(
            shell.model.read(app).status(),
            AppStatus::Error(message) if message.contains("资料库数据已提交")
        ));
    });

    let card = cx
        .debug_bounds("library-selected-note-card")
        .expect("the retained old card is still clickable before recovery");
    cx.simulate_click(card.center(), Modifiers::default());
    redraw(cx);
    view.read_with(cx, |shell, app| {
        assert!(matches!(
            shell.model.read(app).status(),
            AppStatus::Error(message) if message.contains("资料库数据已提交")
        ));
    });

    // The actual retained event bridge, rather than a direct model helper,
    // owns the final route/projection/session transition.
    cx.executor().advance_clock(Duration::from_millis(60));
    cx.run_until_parked();
    redraw(cx);
    view.read_with(cx, |shell, app| {
        assert!(shell.note_session.is_none());
        assert_eq!(shell.model.read(app).status(), &AppStatus::Ready);
    });
    let trashed = repository
        .load_note(&note.id)
        .expect("read trashed note")
        .expect("Trash retains the durable row");
    assert!(trashed.deleted_time.is_some());
    assert!(!trashed.body_text.contains("绝不能进入"));
    assert!(
        repository
            .latest_edit_journal(&note.id)
            .expect("read journal")
            .is_none(),
        "the blocked old session cannot leave a crash-recovery write behind"
    );
}

#[gpui::test]
async fn mounted_external_organization_event_never_remounts_a_dirty_active_session_before_its_flush(
    cx: &mut TestAppContext,
) {
    // This is intentionally an external repository mutation, not a shell
    // action: an OrganizationChanged can arrive while the retained rev-N
    // editor has unsaved text. The bridge must retain/retry the event rather
    // than install rev N+1 and silently drop that text/session.
    let (_profile, repository) = repository();
    let note = repository
        .create_note(CreateNote {
            title: "外部组织事件".into(),
            notebook_id: None,
            document: rich_document("初始正文"),
        })
        .expect("create note");
    let tag = repository.create_tag("外部变更").expect("create tag");
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
        .expect("selected editor surface");
    cx.simulate_click(surface.center(), Modifiers::default());
    cx.simulate_input("本地未保存文本");
    redraw(cx);
    let before_session = view.read_with(cx, |shell, app| {
        let session = shell.note_session.as_ref().expect("active session");
        assert!(matches!(session.read(app).save_state(), SaveState::Dirty));
        session.entity_id()
    });

    repository
        .add_note_tag(&note.id, &tag.id)
        .expect("commit external metadata revision");
    cx.executor().advance_clock(Duration::from_millis(60));
    cx.run_until_parked();
    redraw(cx);

    view.read_with(cx, |shell, app| {
        let session = shell
            .note_session
            .as_ref()
            .expect("failed external flush must retain the old entity");
        assert_eq!(session.entity_id(), before_session);
        assert!(
            session
                .read(app)
                .editor()
                .read(app)
                .copy_all_plain_text()
                .contains("本地未保存文本")
        );
        assert!(matches!(
            session.read(app).save_state(),
            SaveState::Failed(_)
        ));
        assert!(
            shell.save_error.is_some(),
            "the lifecycle conflict must remain visible instead of remounting rev N+1"
        );
    });
}

#[gpui::test]
async fn mounted_external_trash_event_never_unloads_a_dirty_active_session_before_its_flush(
    cx: &mut TestAppContext,
) {
    // The destructive counterpart: NoteTrashed is also delivered through the
    // retained bridge, so it must keep the dirty session and its visible
    // failure rather than immediately replacing it with no active detail.
    let (_profile, repository) = repository();
    let note = repository
        .create_note(CreateNote {
            title: "外部废纸篓事件".into(),
            notebook_id: None,
            document: rich_document("初始正文"),
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
        .expect("selected editor surface");
    cx.simulate_click(surface.center(), Modifiers::default());
    cx.simulate_input("绝不能被外部删除事件吞掉");
    redraw(cx);
    let before_session = view.read_with(cx, |shell, app| {
        let session = shell.note_session.as_ref().expect("active session");
        assert!(matches!(session.read(app).save_state(), SaveState::Dirty));
        session.entity_id()
    });

    repository
        .trash_note(&note.id)
        .expect("commit external trash");
    cx.executor().advance_clock(Duration::from_millis(60));
    cx.run_until_parked();
    redraw(cx);

    view.read_with(cx, |shell, app| {
        let session = shell
            .note_session
            .as_ref()
            .expect("failed external trash flush must retain the old entity");
        assert_eq!(session.entity_id(), before_session);
        assert!(
            session
                .read(app)
                .editor()
                .read(app)
                .copy_all_plain_text()
                .contains("绝不能被外部删除事件吞掉")
        );
        assert!(matches!(
            session.read(app).save_state(),
            SaveState::Failed(_)
        ));
        assert!(shell.save_error.is_some());
    });
}

#[gpui::test]
async fn mounted_coalesced_external_trash_events_still_probe_the_dirty_active_note(
    cx: &mut TestAppContext,
) {
    // The bridge intentionally stores a bounded representative per event
    // kind.  Two same-kind NoteTrashed events in one 50ms batch therefore
    // retain B, not A.  That must not let a dirty active A skip its durable
    // metadata probe and get silently unmounted just because B was the final
    // representative.
    let (_profile, repository) = repository();
    let active = repository
        .create_note(CreateNote {
            title: "批量废纸篓 active A".into(),
            notebook_id: None,
            document: rich_document("A 的初始正文"),
        })
        .expect("create active note A");
    let other = repository
        .create_note(CreateNote {
            title: "批量废纸篓 B".into(),
            notebook_id: None,
            document: rich_document("B 的初始正文"),
        })
        .expect("create later representative B");
    let (view, cx) = mount_shell(Arc::clone(&repository), cx);
    redraw(cx);
    cx.update(|window, app| {
        view.update(app, |shell, shell_cx| {
            shell.apply_action(AppAction::SelectNote(active.id.clone()), window, shell_cx)
        });
    });
    redraw(cx);
    let surface = cx
        .debug_bounds("native-editor-surface")
        .expect("mounted active editor");
    cx.simulate_click(surface.center(), Modifiers::default());
    cx.simulate_input("A 的脏文本绝不能被 B 覆盖");
    redraw(cx);
    let before_session = view.read_with(cx, |shell, app| {
        let session = shell.note_session.as_ref().expect("dirty A session");
        assert!(matches!(session.read(app).save_state(), SaveState::Dirty));
        session.entity_id()
    });

    repository
        .trash_note(&active.id)
        .expect("external writer trashes active A first");
    repository
        .trash_note(&other.id)
        .expect("same batch trashes B last and overwrites the bounded representative");
    cx.executor().advance_clock(Duration::from_millis(60));
    cx.run_until_parked();
    redraw(cx);

    view.read_with(cx, |shell, app| {
        let session = shell
            .note_session
            .as_ref()
            .expect("lifecycle failure must retain dirty active A");
        assert_eq!(session.entity_id(), before_session);
        assert!(
            session
                .read(app)
                .editor()
                .read(app)
                .copy_all_plain_text()
                .contains("A 的脏文本绝不能被 B 覆盖")
        );
        assert!(matches!(
            session.read(app).save_state(),
            SaveState::Failed(_)
        ));
        assert!(shell.save_error.is_some());
    });
}

#[gpui::test]
async fn mounted_external_event_keeps_marked_ime_session_alive_until_composition_resolves(
    cx: &mut TestAppContext,
) {
    // A marked IME candidate is not a snapshot payload. An external event
    // therefore cannot replace the session underneath it; the retained bridge
    // must retry later instead of treating the last clean saved revision as
    // permission to unmount the composition owner.
    let (_profile, repository) = repository();
    let note = repository
        .create_note(CreateNote {
            title: "组合与外部事件".into(),
            notebook_id: None,
            document: rich_document("正文"),
        })
        .expect("create note");
    let tag = repository.create_tag("外部标签").expect("create tag");
    let (view, cx) = mount_shell(Arc::clone(&repository), cx);
    redraw(cx);
    cx.update(|window, app| {
        view.update(app, |shell, shell_cx| {
            shell.apply_action(AppAction::SelectNote(note.id.clone()), window, shell_cx)
        });
    });
    redraw(cx);
    let (editor, session_before) = view.read_with(cx, |shell, app| {
        let session = shell.note_session.as_ref().expect("active session");
        (session.read(app).editor().clone(), session.entity_id())
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
    redraw(cx);

    repository
        .add_note_tag(&note.id, &tag.id)
        .expect("commit external metadata revision");
    cx.executor().advance_clock(Duration::from_millis(60));
    cx.run_until_parked();
    redraw(cx);

    view.read_with(cx, |shell, app| {
        let session = shell
            .note_session
            .as_ref()
            .expect("IME event gate must retain the session");
        assert_eq!(session.entity_id(), session_before);
        assert!(session.read(app).editor().read(app).marked_text().is_some());
        assert!(
            shell.save_error.is_some(),
            "the visible lifecycle blocker must survive until IME resolves"
        );
    });
}

#[gpui::test]
async fn mounted_organization_panel_uses_real_input_and_typed_move_tag_controls(
    cx: &mut TestAppContext,
) {
    // Mutation-sensitive: this follows the actual panel focus/input/button
    // route. Replacing it with a model-only convenience action, or making a
    // sidebar label a second mutation authority, leaves one of the durable
    // create/move/tag assertions below false.
    let (_profile, repository) = repository();
    let destination = repository
        .create_notebook("迁移目标", None)
        .expect("create destination notebook");
    let tag = repository.create_tag("真实标签").expect("create tag");
    let note = repository
        .create_note(CreateNote {
            title: "组织面板笔记".into(),
            notebook_id: None,
            document: rich_document("不经卡片正文查询的组织更新"),
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

    let toggle = cx
        .debug_bounds("library-toggle-organization")
        .expect("organization menu trigger");
    cx.simulate_click(toggle.center(), Modifiers::default());
    redraw(cx);
    let input = cx
        .debug_bounds("library-organization-input")
        .expect("real organization EntityInputHandler");
    cx.simulate_click(input.center(), Modifiers::default());
    cx.simulate_input("来自面板的笔记本");
    redraw(cx);
    let create = cx
        .debug_bounds("library-organization-create-notebook")
        .expect("typed create notebook control");
    cx.simulate_click(create.center(), Modifiers::default());
    redraw(cx);
    view.read_with(cx, |shell, app| {
        assert!(
            shell
                .model
                .read(app)
                .navigation_index()
                .notebooks
                .iter()
                .any(|notebook| notebook.title == "来自面板的笔记本")
        );
    });

    let move_selector: &'static str = Box::leak(
        format!("library-organization-move-note-{}", destination.id.as_str()).into_boxed_str(),
    );
    let move_target = cx
        .debug_bounds(&move_selector)
        .expect("dynamic typed move target");
    cx.simulate_click(move_target.center(), Modifiers::default());
    redraw(cx);
    assert_eq!(
        repository
            .load_note(&note.id)
            .expect("read moved note")
            .expect("note remains")
            .notebook_id,
        destination.id,
        "the mounted move button must invoke the typed repository action"
    );

    let tag_selector: &'static str =
        Box::leak(format!("library-organization-tag-{}", tag.id.as_str()).into_boxed_str());
    let tag_target = cx
        .debug_bounds(&tag_selector)
        .expect("dynamic typed tag target");
    cx.simulate_click(tag_target.center(), Modifiers::default());
    redraw(cx);
    view.read_with(cx, |shell, app| {
        let model = shell.model.read(app);
        assert_eq!(model.navigation().selected_note_id(), Some(&note.id));
        assert_eq!(
            model.active_note().expect("active note").tag_ids,
            vec![tag.id.clone()]
        );
        assert!(matches!(model.status(), AppStatus::Ready));
    });
}

#[gpui::test]
async fn mounted_organization_panel_renames_and_deletes_the_active_notebook_by_id(
    cx: &mut TestAppContext,
) {
    // This goes through the actual input/menu controls rather than dispatching
    // repository helpers from the test. A delete/recreate rename would change
    // the durable ID; deleting the active route without the candidate fallback
    // would leave the selected editor on an invisible/wrong notebook.
    let (_profile, repository) = repository();
    let notebook = repository
        .create_notebook("原笔记本", None)
        .expect("create notebook");
    let note = repository
        .create_note(CreateNote {
            title: "笔记本中的当前笔记".into(),
            notebook_id: Some(notebook.id.clone()),
            document: rich_document("删除容器后必须回到默认笔记本"),
        })
        .expect("create scoped note");
    let default_notebook = repository.default_notebook().expect("default notebook").id;
    let (view, cx) = mount_shell(Arc::clone(&repository), cx);
    redraw(cx);
    cx.update(|window, app| {
        view.update(app, |shell, shell_cx| {
            shell.apply_action(
                AppAction::NavigateTo {
                    route: LibraryRoute::Notebook(notebook.id.clone()),
                    selected_note_id: Some(note.id.clone()),
                },
                window,
                shell_cx,
            )
        });
    });
    redraw(cx);

    let toggle = cx
        .debug_bounds("library-toggle-organization")
        .expect("organization menu trigger");
    cx.simulate_click(toggle.center(), Modifiers::default());
    redraw(cx);
    let input = cx
        .debug_bounds("library-organization-input")
        .expect("real organization title input");
    cx.simulate_click(input.center(), Modifiers::default());
    cx.simulate_input("改名后仍是同一个 ID");
    redraw(cx);
    let rename = cx
        .debug_bounds("library-organization-rename-current")
        .expect("typed rename control");
    cx.simulate_click(rename.center(), Modifiers::default());
    redraw(cx);
    view.read_with(cx, |shell, app| {
        let model = shell.model.read(app);
        let renamed = model
            .navigation_index()
            .notebooks
            .iter()
            .find(|candidate| candidate.id == notebook.id)
            .expect("same durable notebook is retained");
        assert_eq!(renamed.title, "改名后仍是同一个 ID");
        assert_eq!(
            model.navigation().route(),
            &LibraryRoute::Notebook(notebook.id.clone())
        );
    });

    let delete = cx
        .debug_bounds("library-organization-delete-current-notebook")
        .expect("typed delete notebook control");
    let (
        route_before_confirmation,
        selected_before_confirmation,
        history_before_confirmation,
        session_before_confirmation,
    ) = view.read_with(cx, |shell, app| {
        let model = shell.model.read(app);
        (
            model.navigation().route().clone(),
            model.navigation().selected_note_id().cloned(),
            model.navigation().history_len_for_test(),
            shell
                .note_session
                .as_ref()
                .expect("active notebook keeps its session before confirmation")
                .entity_id(),
        )
    });
    cx.simulate_click(delete.center(), Modifiers::default());
    redraw(cx);
    assert_eq!(
        repository
            .load_note(&note.id)
            .expect("read note before destructive confirmation")
            .expect("cancelable confirmation cannot delete a note")
            .notebook_id,
        notebook.id,
        "opening a destructive confirmation must not perform the notebook mutation"
    );
    let cancel = cx
        .debug_bounds("library-organization-cancel-destructive")
        .expect("delete click opens an explicit cancellation boundary");
    assert!(
        cx.debug_bounds("library-organization-confirm-destructive")
            .is_some(),
        "typed confirmation must be visible before repository dispatch"
    );
    cx.simulate_click(cancel.center(), Modifiers::default());
    redraw(cx);
    view.read_with(cx, |shell, app| {
        let model = shell.model.read(app);
        assert_eq!(model.navigation().route(), &route_before_confirmation);
        assert_eq!(
            model.navigation().selected_note_id(),
            selected_before_confirmation.as_ref()
        );
        assert_eq!(
            model.navigation().history_len_for_test(),
            history_before_confirmation,
            "cancel must not create or rewrite history"
        );
        assert_eq!(
            shell
                .note_session
                .as_ref()
                .expect("cancel retains the exact mounted session")
                .entity_id(),
            session_before_confirmation
        );
    });
    assert_eq!(
        repository
            .load_note(&note.id)
            .expect("read note after cancelled confirmation")
            .expect("cancel must be a database no-op")
            .notebook_id,
        notebook.id
    );

    let delete = cx
        .debug_bounds("library-organization-delete-current-notebook")
        .expect("delete control remains after cancellation");
    cx.simulate_click(delete.center(), Modifiers::default());
    redraw(cx);
    let confirm = cx
        .debug_bounds("library-organization-confirm-destructive")
        .expect("second destructive click reopens a typed confirmation");
    cx.simulate_click(confirm.center(), Modifiers::default());
    redraw(cx);
    assert_eq!(
        repository
            .load_note(&note.id)
            .expect("read rehomed note")
            .expect("note remains after container deletion")
            .notebook_id,
        default_notebook,
        "ordinary notebook deletion must rehome its note rather than trash or purge it"
    );
    view.read_with(cx, |shell, app| {
        let model = shell.model.read(app);
        assert_eq!(model.navigation().route(), &LibraryRoute::AllNotes);
        assert_eq!(model.navigation().selected_note_id(), Some(&note.id));
        assert_eq!(model.active_session_note_id(), Some(&note.id));
        assert!(
            model
                .navigation_index()
                .notebooks
                .iter()
                .all(|candidate| candidate.id != notebook.id)
        );
    });
}

#[gpui::test]
async fn mounted_organization_panel_disbands_the_active_stack_without_losing_its_note(
    cx: &mut TestAppContext,
) {
    // A stack delete control must be a genuine typed route action. The test
    // uses the mounted panel rather than invoking the repository, so removing
    // the production control or routing it through notebook deletion makes
    // the stack/route/note assertions below fail together.
    let (_profile, repository) = repository();
    let stack = repository.create_stack("当前组").expect("create stack");
    let notebook = repository
        .create_notebook("组内笔记本", Some(&stack.id))
        .expect("create child notebook");
    let note = repository
        .create_note(CreateNote {
            title: "组内笔记".into(),
            notebook_id: Some(notebook.id.clone()),
            document: rich_document("解散组不应删除正文或容器"),
        })
        .expect("create scoped note");
    let (view, cx) = mount_shell(Arc::clone(&repository), cx);
    redraw(cx);
    cx.update(|window, app| {
        view.update(app, |shell, shell_cx| {
            shell.apply_action(
                AppAction::NavigateTo {
                    route: LibraryRoute::Stack(stack.id.clone()),
                    selected_note_id: Some(note.id.clone()),
                },
                window,
                shell_cx,
            )
        });
    });
    redraw(cx);
    let surface = cx
        .debug_bounds("native-editor-surface")
        .expect("stack detail mounts the shared editor surface");
    cx.simulate_click(surface.center(), Modifiers::default());
    cx.simulate_input("会话与撤销历史必须保留");
    redraw(cx);
    let (session_before_disband, selection_before_disband, undo_before_disband) =
        view.read_with(cx, |shell, app| {
            let session = shell
                .note_session
                .as_ref()
                .expect("active stack note owns one retained session");
            let editor = session.read(app).editor().read(app);
            assert!(
                editor.undo_depth() > 0,
                "fixture must establish undo history"
            );
            (session.entity_id(), editor.selection(), editor.undo_depth())
        });
    assert!(
        !view.update(cx, |shell, shell_cx| {
            shell.flush_for_lifecycle(FlushReason::ManualSync, shell_cx)
        }),
        "the dirty typing fixture must use the production retained save worker"
    );
    cx.run_until_parked();
    assert!(view.update(cx, |shell, shell_cx| {
        shell.flush_for_lifecycle(FlushReason::ManualSync, shell_cx)
    }));
    redraw(cx);
    let toggle = cx
        .debug_bounds("library-toggle-organization")
        .expect("organization menu trigger");
    cx.simulate_click(toggle.center(), Modifiers::default());
    redraw(cx);
    let disband = cx
        .debug_bounds("library-organization-delete-current-stack")
        .expect("typed disband control");
    cx.simulate_click(disband.center(), Modifiers::default());
    redraw(cx);
    let confirm = cx
        .debug_bounds("library-organization-confirm-destructive")
        .expect("stack disband requires explicit confirmation");
    cx.simulate_click(confirm.center(), Modifiers::default());
    redraw(cx);

    assert_eq!(
        repository
            .load_note(&note.id)
            .expect("load retained note")
            .expect("note survives disband")
            .notebook_id,
        notebook.id
    );
    view.read_with(cx, |shell, app| {
        let model = shell.model.read(app);
        assert_eq!(model.navigation().route(), &LibraryRoute::AllNotes);
        assert_eq!(model.navigation().selected_note_id(), Some(&note.id));
        assert_eq!(model.active_session_note_id(), Some(&note.id));
        let session = shell
            .note_session
            .as_ref()
            .expect("same NoteId/revision must retain its editor session");
        let editor = session.read(app).editor().read(app);
        assert_eq!(session.entity_id(), session_before_disband);
        assert_eq!(editor.selection(), selection_before_disband);
        assert_eq!(editor.undo_depth(), undo_before_disband);
        assert!(
            model
                .navigation_index()
                .stacks
                .iter()
                .all(|candidate| candidate.id != stack.id)
        );
        assert_eq!(
            model
                .navigation_index()
                .notebooks
                .iter()
                .find(|candidate| candidate.id == notebook.id)
                .expect("notebook remains")
                .stack_id,
            None
        );
    });
}

#[gpui::test]
async fn mounted_tag_delete_waits_for_confirmation_and_targets_the_durable_id(
    cx: &mut TestAppContext,
) {
    let (_profile, repository) = repository();
    let tag = repository.create_tag("确认删除的标签").expect("create tag");
    let (view, cx) = mount_shell(Arc::clone(&repository), cx);
    redraw(cx);
    cx.update(|window, app| {
        view.update(app, |shell, shell_cx| {
            shell.apply_action(
                AppAction::NavigateTo {
                    route: LibraryRoute::Tags(std::collections::BTreeSet::from([tag.id.clone()])),
                    selected_note_id: None,
                },
                window,
                shell_cx,
            )
        });
    });
    redraw(cx);
    let toggle = cx
        .debug_bounds("library-toggle-organization")
        .expect("organization panel trigger");
    cx.simulate_click(toggle.center(), Modifiers::default());
    redraw(cx);
    let delete = cx
        .debug_bounds("library-organization-delete-current-tag")
        .expect("typed tag delete control");
    cx.simulate_click(delete.center(), Modifiers::default());
    redraw(cx);
    assert!(
        repository
            .list_navigation_index()
            .expect("read tags before confirmation")
            .tags
            .iter()
            .any(|candidate| candidate.id == tag.id),
        "opening tag confirmation must not write the durable tag row"
    );
    let confirm = cx
        .debug_bounds("library-organization-confirm-destructive")
        .expect("typed tag confirmation");
    cx.simulate_click(confirm.center(), Modifiers::default());
    redraw(cx);
    assert!(
        repository
            .list_navigation_index()
            .expect("read tags after confirmation")
            .tags
            .iter()
            .all(|candidate| candidate.id != tag.id),
        "confirmation must dispatch DeleteTag for the exact stable ID"
    );
}

#[gpui::test]
async fn mounted_organization_panel_exposes_typed_restore_and_purge_only_in_trash(
    cx: &mut TestAppContext,
) {
    let (_profile, repository) = repository();
    let restored = repository
        .create_note(CreateNote {
            title: "待恢复".into(),
            notebook_id: None,
            document: rich_document("废纸篓恢复必须走 typed action"),
        })
        .expect("create restore fixture");
    let purged = repository
        .create_note(CreateNote {
            title: "待彻底删除".into(),
            notebook_id: None,
            document: rich_document("普通路由绝不直接彻底删除"),
        })
        .expect("create purge fixture");
    repository
        .trash_note(&restored.id)
        .expect("trash restored note");
    repository
        .trash_note(&purged.id)
        .expect("trash purged note");
    let (view, cx) = mount_shell(Arc::clone(&repository), cx);
    redraw(cx);

    cx.update(|window, app| {
        view.update(app, |shell, shell_cx| {
            shell.apply_action(
                AppAction::NavigateTo {
                    route: LibraryRoute::Trash,
                    selected_note_id: Some(restored.id.clone()),
                },
                window,
                shell_cx,
            )
        });
    });
    redraw(cx);
    let toggle = cx
        .debug_bounds("library-toggle-organization")
        .expect("organization trigger in Trash");
    cx.simulate_click(toggle.center(), Modifiers::default());
    redraw(cx);
    assert!(
        cx.debug_bounds("library-organization-restore-selected")
            .is_some()
    );
    assert!(
        cx.debug_bounds("library-organization-purge-selected")
            .is_some()
    );
    assert!(
        cx.debug_bounds("library-organization-move-targets")
            .is_none()
            && cx
                .debug_bounds("library-organization-tag-targets")
                .is_none(),
        "Trash must not render Move/Tag controls that the typed repository correctly rejects"
    );
    let restore = cx
        .debug_bounds("library-organization-restore-selected")
        .expect("typed restore control");
    cx.simulate_click(restore.center(), Modifiers::default());
    redraw(cx);
    assert_eq!(
        repository
            .load_note(&restored.id)
            .expect("read restored note")
            .expect("restored note remains")
            .deleted_time,
        None
    );
    view.read_with(cx, |shell, app| {
        assert_ne!(
            shell.model.read(app).active_session_note_id(),
            Some(&restored.id),
            "Trash route must not keep a restored note mounted as its active session"
        );
    });

    cx.update(|window, app| {
        view.update(app, |shell, shell_cx| {
            shell.apply_action(
                AppAction::NavigateTo {
                    route: LibraryRoute::Trash,
                    selected_note_id: Some(purged.id.clone()),
                },
                window,
                shell_cx,
            )
        });
    });
    redraw(cx);
    // The menu remains an owned shell overlay across typed navigation, so its
    // controls are rebuilt against the newly selected Trash NoteId.
    let purge = cx
        .debug_bounds("library-organization-purge-selected")
        .expect("typed purge control");
    cx.simulate_click(purge.center(), Modifiers::default());
    redraw(cx);
    let confirm = cx
        .debug_bounds("library-organization-confirm-destructive")
        .expect("permanent deletion requires explicit confirmation");
    cx.simulate_click(confirm.center(), Modifiers::default());
    redraw(cx);
    assert!(
        repository
            .load_note(&purged.id)
            .expect("read purged note")
            .is_none()
    );
    view.read_with(cx, |shell, app| {
        assert_eq!(shell.model.read(app).active_session_note_id(), None);
    });
}

#[gpui::test]
async fn mounted_empty_trash_hides_recovery_and_destructive_controls(cx: &mut TestAppContext) {
    let (_profile, repository) = repository();
    let (view, cx) = mount_shell(repository, cx);
    redraw(cx);
    cx.update(|window, app| {
        view.update(app, |shell, shell_cx| {
            shell.apply_action(
                AppAction::NavigateTo {
                    route: LibraryRoute::Trash,
                    selected_note_id: None,
                },
                window,
                shell_cx,
            )
        });
    });
    redraw(cx);
    let toggle = cx
        .debug_bounds("library-toggle-organization")
        .expect("Trash organization trigger");
    cx.simulate_click(toggle.center(), Modifiers::default());
    redraw(cx);
    assert!(
        cx.debug_bounds("library-organization-restore-selected")
            .is_none()
            && cx
                .debug_bounds("library-organization-purge-selected")
                .is_none(),
        "an empty Trash must not expose dead recovery/destructive buttons"
    );
}

#[gpui::test]
async fn mounted_destructive_confirmation_fences_a_stale_trash_target(cx: &mut TestAppContext) {
    // A confirmation carries the typed NoteId it was opened for. Navigation
    // before approval must drop that intent rather than letting an old button
    // permanently delete a different, no-longer-selected card.
    let (_profile, repository) = repository();
    let first = repository
        .create_note(CreateNote {
            title: "第一个待永久删除".into(),
            notebook_id: None,
            document: rich_document("第一个正文"),
        })
        .expect("create first Trash note");
    let second = repository
        .create_note(CreateNote {
            title: "第二个待永久删除".into(),
            notebook_id: None,
            document: rich_document("第二个正文"),
        })
        .expect("create second Trash note");
    repository.trash_note(&first.id).expect("trash first note");
    repository
        .trash_note(&second.id)
        .expect("trash second note");

    let (view, cx) = mount_shell(Arc::clone(&repository), cx);
    redraw(cx);
    cx.update(|window, app| {
        view.update(app, |shell, shell_cx| {
            shell.apply_action(
                AppAction::NavigateTo {
                    route: LibraryRoute::Trash,
                    selected_note_id: Some(first.id.clone()),
                },
                window,
                shell_cx,
            )
        });
    });
    redraw(cx);
    let toggle = cx
        .debug_bounds("library-toggle-organization")
        .expect("Trash organization trigger");
    cx.simulate_click(toggle.center(), Modifiers::default());
    redraw(cx);
    let purge = cx
        .debug_bounds("library-organization-purge-selected")
        .expect("purge opens a typed confirmation");
    cx.simulate_click(purge.center(), Modifiers::default());
    redraw(cx);
    assert!(
        repository
            .load_note(&first.id)
            .expect("read first target before confirmation")
            .is_some(),
        "opening permanent-delete confirmation cannot write the old target"
    );
    let stale_confirm_bounds = cx
        .debug_bounds("library-organization-confirm-destructive")
        .expect("opened confirmation remains visibly tied to the first typed NoteId");

    cx.update(|window, app| {
        view.update(app, |shell, shell_cx| {
            shell.apply_action(
                AppAction::NavigateTo {
                    route: LibraryRoute::Trash,
                    selected_note_id: Some(second.id.clone()),
                },
                window,
                shell_cx,
            )
        });
    });
    redraw(cx);
    view.read_with(cx, |shell, app| {
        assert_eq!(
            shell.pending_destructive_action, None,
            "a selection/session change must fence the stale destructive intent"
        );
        assert_eq!(
            shell.model.read(app).navigation().selected_note_id(),
            Some(&second.id),
            "the actual route reducer must have reached the new typed NoteId"
        );
    });
    // Click where the old confirmation had been. If the retained intent or
    // overlay survived selection change, this would still dispatch a purge.
    cx.simulate_click(stale_confirm_bounds.center(), Modifiers::default());
    redraw(cx);
    assert!(
        repository
            .load_note(&first.id)
            .expect("read fenced first target")
            .is_some(),
        "switching cards must leave the previous pending target untouched"
    );
    assert!(
        repository
            .load_note(&second.id)
            .expect("read selected second target")
            .is_some(),
        "fencing confirmation must not accidentally delete the new selection"
    );
}

#[gpui::test]
async fn mounted_trash_detail_rejects_title_and_body_mutation_but_keeps_copy_available(
    cx: &mut TestAppContext,
) {
    // A trashed note remains a real retained detail view, but its repository
    // writer is intentionally unavailable. This exercises the same title
    // canvas and body EntityInputHandler that a person uses instead of
    // calling a session/model mutation helper directly.
    cx.update(|app| crate::components::init(app));
    let (_profile, repository) = repository();
    let note = repository
        .create_note(CreateNote {
            title: "已删除标题".into(),
            notebook_id: None,
            document: rich_document("已删除正文"),
        })
        .expect("create trash fixture");
    repository.trash_note(&note.id).expect("trash fixture");

    let (view, cx) = mount_shell(Arc::clone(&repository), cx);
    redraw(cx);
    cx.update(|window, app| {
        view.update(app, |shell, shell_cx| {
            shell.apply_action(
                AppAction::NavigateTo {
                    route: LibraryRoute::Trash,
                    selected_note_id: Some(note.id.clone()),
                },
                window,
                shell_cx,
            )
        });
    });
    redraw(cx);

    view.read_with(cx, |shell, app| {
        assert_eq!(
            shell.model.read(app).navigation().route(),
            &LibraryRoute::Trash
        );
    });

    let (title_before, body_before, generation_before) = view.read_with(cx, |shell, app| {
        let session = shell.note_session.as_ref().expect("mounted trash session");
        let surface = shell
            .editor_surface
            .as_ref()
            .expect("mounted trash surface");
        assert_eq!(surface.read(app).mode(), EditorSurfaceMode::ReadOnly);
        assert!(
            shell.command_chrome.is_none(),
            "Trash must not mount formatting mutations over a non-durable note"
        );
        assert!(
            shell.pending_resource_insert.is_none(),
            "Trash detail must not retain a resource insertion intent"
        );
        (
            session.read(app).title().read(app).text().to_owned(),
            session.read(app).editor().read(app).copy_all_plain_text(),
            session.read(app).save_generation(),
        )
    });
    let revision_before = repository
        .load_note(&note.id)
        .expect("read retained trashed note")
        .expect("trashed note exists")
        .revision;
    assert!(
        cx.debug_bounds("library-editor-command-chrome").is_none(),
        "the shared format Chrome must not expose mutation buttons in Trash"
    );
    let picker_error = view.update(cx, |shell, shell_cx| shell.begin_resource_picker(shell_cx));
    assert!(
        picker_error.is_err_and(|error| error.contains("只读")),
        "the production picker boundary cannot create a staged insert for a trashed note"
    );

    let title = cx
        .debug_bounds("library-note-title")
        .expect("Trash title remains visible for selection/copy");
    cx.simulate_click(title.center(), Modifiers::default());
    cx.simulate_input(" 不得写入");
    let surface = cx
        .debug_bounds("native-editor-surface")
        .expect("Trash body remains visible for selection/copy");
    cx.simulate_click(surface.center(), Modifiers::default());
    cx.simulate_input(" 不得写入");
    cx.simulate_keystrokes("backspace");
    cx.write_to_clipboard(ClipboardItem::new_string("粘贴也不得写入".to_owned()));
    cx.dispatch_action(Paste);
    cx.dispatch_action(SelectAll);
    cx.dispatch_action(Copy);
    redraw(cx);

    assert_eq!(
        cx.read_from_clipboard().and_then(|item| item.text()),
        Some(body_before.clone().into()),
        "read-only Trash keeps the production copy route available"
    );
    view.read_with(cx, |shell, app| {
        let session = shell.note_session.as_ref().expect("trash session retained");
        assert_eq!(session.read(app).title().read(app).text(), title_before);
        assert_eq!(
            session.read(app).editor().read(app).copy_all_plain_text(),
            body_before
        );
        assert_eq!(session.read(app).save_generation(), generation_before);
    });
    let stored = repository
        .load_note(&note.id)
        .expect("load trashed note")
        .expect("trashed note retained");
    assert_eq!(stored.title, title_before);
    assert_eq!(stored.revision, revision_before);
    assert_eq!(stored.body_text, body_before);
    assert!(stored.deleted_time.is_some());
}

#[gpui::test]
async fn mounted_trash_legacy_image_hydrates_for_presentation_without_repairing_or_saving_semantics(
    cx: &mut TestAppContext,
) {
    // A legacy image lacks natural geometry and may become visible in the
    // retained Trash preview. Hydration/materialization is presentation-only;
    // its normal editable-session geometry repair must not revise/journal a
    // deleted note merely because the renderer inspected its bytes.
    let (_profile, repository) = repository();
    let image = repository
        .import_resource(
            &structural_png(675, 1200),
            "trash-legacy.png",
            "image/png",
            "png",
        )
        .expect("persist legacy image");
    let note = repository
        .create_note(CreateNote {
            title: "废纸篓旧图片".into(),
            notebook_id: None,
            document: CanonicalDocument::from_blocks(vec![Block::Image {
                resource_id: image,
                alt: "vertical".into(),
                presentation: Default::default(),
            }]),
        })
        .expect("create legacy image note");
    repository.trash_note(&note.id).expect("trash fixture");
    let before = repository
        .load_note(&note.id)
        .expect("load trash note")
        .expect("trash note retained");
    assert!(
        !before.body_html.contains("data-joplin-lite-natural-width"),
        "fixture must use the legacy unknown-presentation wire form"
    );

    let (view, cx) = mount_shell(Arc::clone(&repository), cx);
    redraw(cx);
    cx.update(|window, app| {
        view.update(app, |shell, shell_cx| {
            shell.apply_action(
                AppAction::NavigateTo {
                    route: LibraryRoute::Trash,
                    selected_note_id: Some(note.id.clone()),
                },
                window,
                shell_cx,
            )
        });
    });
    redraw(cx);
    cx.run_until_parked();
    redraw(cx);

    let probe = view.read_with(cx, |shell, app| shell.image_flow_probe_for_test(app));
    assert!(
        probe.has_image_block && probe.cache_has_resource,
        "Trash still receives verified presentation hydration for its visible image"
    );
    view.read_with(cx, |shell, app| {
        let session = shell.note_session.as_ref().expect("Trash session retained");
        assert!(session.read(app).is_durable_read_only());
        assert_eq!(session.read(app).save_state(), SaveState::Clean);
    });
    let after = repository
        .load_note(&note.id)
        .expect("reload Trash note")
        .expect("Trash row retained");
    assert_eq!(after.revision, before.revision);
    assert_eq!(after.body_html, before.body_html);
    assert!(
        repository
            .latest_edit_journal(&note.id)
            .expect("read journal")
            .is_none(),
        "presentation hydration must not create a writable crash-recovery record"
    );
}

#[gpui::test]
async fn mounted_trash_restore_bypasses_a_failed_read_only_flush(cx: &mut TestAppContext) {
    // Restore is the recovery operation for the exact lifecycle in which a
    // stale save error is visible. It must not first ask the intentionally
    // read-only Trash session to snapshot into a repository row that forbids
    // deleted notes.
    let (_profile, repository) = repository();
    let note = repository
        .create_note(CreateNote {
            title: "失败后仍可恢复".into(),
            notebook_id: None,
            document: rich_document("保留内容"),
        })
        .expect("create trash restore fixture");
    repository.trash_note(&note.id).expect("trash fixture");
    let (view, cx) = mount_shell(Arc::clone(&repository), cx);
    redraw(cx);
    cx.update(|window, app| {
        view.update(app, |shell, shell_cx| {
            shell.apply_action(
                AppAction::NavigateTo {
                    route: LibraryRoute::Trash,
                    selected_note_id: Some(note.id.clone()),
                },
                window,
                shell_cx,
            )
        });
    });
    redraw(cx);

    let session = view.read_with(cx, |shell, _| {
        shell.note_session.as_ref().expect("trash session").clone()
    });
    session.update(cx, |session, session_cx| {
        session.force_save_failure_for_test("injected stale Trash snapshot");
        session_cx.notify();
    });
    redraw(cx);
    assert!(view.read_with(cx, |shell, _| shell.save_error_for_test().is_some()));

    let toggle = cx
        .debug_bounds("library-toggle-organization")
        .expect("Trash organization control");
    cx.simulate_click(toggle.center(), Modifiers::default());
    redraw(cx);
    let restore = cx
        .debug_bounds("library-organization-restore-selected")
        .expect("typed Trash restore action");
    cx.simulate_click(restore.center(), Modifiers::default());
    redraw(cx);

    assert_eq!(
        repository
            .load_note(&note.id)
            .expect("reload restored note")
            .expect("restored note remains")
            .deleted_time,
        None,
        "Restore must dispatch despite the stale read-only flush failure"
    );
    view.read_with(cx, |shell, app| {
        assert_ne!(
            shell.model.read(app).active_session_note_id(),
            Some(&note.id),
            "Trash route must tear down the old read-only session after Restore"
        );
    });
}

#[gpui::test]
async fn mounted_failed_organization_action_keeps_the_live_session_and_shows_one_error(
    cx: &mut TestAppContext,
) {
    // The shell sets its same-NoteId remount request before dispatch so an
    // eager model observer sees a successful external revision change. The
    // failure path must clear that request again: a nonexistent destination
    // cannot be allowed to tear down a healthy active editor/session.
    let (_profile, repository) = repository();
    let note = repository
        .create_note(CreateNote {
            title: "失败保持会话".into(),
            notebook_id: None,
            document: rich_document("错误不能换掉当前编辑器"),
        })
        .expect("create note");
    let (view, cx) = mount_shell(repository, cx);
    redraw(cx);
    cx.update(|window, app| {
        view.update(app, |shell, shell_cx| {
            shell.apply_action(AppAction::SelectNote(note.id.clone()), window, shell_cx)
        });
    });
    redraw(cx);
    let session_before = view.read_with(cx, |shell, _| {
        shell
            .note_session
            .as_ref()
            .expect("mounted session")
            .entity_id()
    });
    let missing =
        app_lite_core::NotebookId::parse("f".repeat(32)).expect("valid opaque missing notebook id");
    cx.update(|window, app| {
        view.update(app, |shell, shell_cx| {
            shell.apply_action(AppAction::MoveSelectedNote(missing), window, shell_cx)
        });
    });
    redraw(cx);

    assert!(
        cx.debug_bounds("library-action-error").is_some(),
        "a failed organization action must be visible rather than silently dropping its route error"
    );
    view.read_with(cx, |shell, app| {
        assert_eq!(
            shell.model.read(app).navigation().selected_note_id(),
            Some(&note.id)
        );
        assert_eq!(
            shell
                .note_session
                .as_ref()
                .expect("failed action retains session")
                .entity_id(),
            session_before,
            "failed organization navigation must not remount the active session"
        );
        assert!(matches!(
            shell.model.read(app).status(),
            AppStatus::Error(_)
        ));
    });
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
async fn mounted_default_light_route_keeps_every_editor_state_opaque_and_contrasted(
    cx: &mut TestAppContext,
) {
    // This is intentionally a mounted production-style assertion.  The
    // release regression left the body as a white island while an inherited
    // transparent right shell exposed macOS's black backing.  It must fail if
    // any concrete route state stops invoking its opaque surface/text token,
    // not merely if a source string changes.
    let (_empty_profile, empty_repository) = repository();
    let (empty_shell, cx) = mount_shell(empty_repository, cx);
    redraw(cx);
    assert!(cx.debug_bounds("library-empty-state").is_some());
    let empty = empty_shell.read_with(cx, |shell, _| {
        shell.default_light_route_paint_contract_for_test()
    });
    assert!(empty.is_opaque_and_contrasted());
    assert_eq!(
        empty.background(LibraryPrimarySurface::EmptyState),
        Some(EVERNOTE_LIGHT_PRIMARY_SURFACE),
        "the empty-state background must not inherit the opaque black macOS window backing"
    );

    let (_profile, repository) = repository();
    let note = repository
        .create_note(CreateNote {
            title: "浅色正文与标题".into(),
            notebook_id: None,
            document: rich_document("正文文本必须与白色编辑面板保持可读对比。"),
        })
        .expect("create selected note");
    let (view, cx) = mount_shell(repository, cx);
    redraw(cx);
    assert!(cx.debug_bounds("library-no-selection").is_some());
    let no_selection = view.read_with(cx, |shell, _| {
        shell.default_light_route_paint_contract_for_test()
    });
    assert!(no_selection.is_opaque_and_contrasted());
    assert_eq!(
        no_selection.background(LibraryPrimarySurface::NoSelection),
        Some(EVERNOTE_LIGHT_PRIMARY_SURFACE),
        "the no-selection panel must stay opaque before a card is selected"
    );

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
    );
    let selected = view.read_with(cx, |shell, app| {
        (
            shell.default_light_route_paint_contract_for_test(),
            shell
                .command_chrome
                .as_ref()
                .expect("selected library note owns the shared Chrome")
                .read(app)
                .library_surface_contract_for_test(),
            shell
                .editor_surface
                .as_ref()
                .expect("selected library note owns its native surface")
                .read(app)
                .light_surface_contract_for_test(),
        )
    });
    assert!(selected.0.is_opaque_and_contrasted());
    assert!(selected.1.is_opaque_and_contrasted());
    assert!(selected.2.is_opaque_and_contrasted());
    assert!(
        selected.2.has_primary_foreground(),
        "the Library body surface must explicitly request readable primary text rather than inheriting a native-window default"
    );
    for surface in [
        LibraryPrimarySurface::Shell,
        LibraryPrimarySurface::MainEditor,
        LibraryPrimarySurface::Toolbar,
        LibraryPrimarySurface::Title,
        LibraryPrimarySurface::EditorPane,
    ] {
        assert_eq!(
            selected.0.background(surface),
            Some(EVERNOTE_LIGHT_PRIMARY_SURFACE),
            "the selected editor's {surface:?} must use the opaque Evernote primary surface"
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
        title: "无法启动 Joplin Lite".into(),
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
async fn queued_create_event_retries_the_full_candidate_without_manual_card_selection(
    cx: &mut TestAppContext,
) {
    // A committed create whose first candidate fails must retain a typed
    // recovery target. The queued NoteCreated event retries the *complete*
    // All Notes + selected full-Note candidate; it is not merely a projection
    // repaint, and a second candidate failure cannot clear the warning.
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

    view.update(cx, |view, view_cx| {
        view.model.update(view_cx, |model, _| {
            model.fail_next_refresh_for_test(app_lite_core::LibraryError::NotFound);
        });
    });
    cx.executor().advance_clock(Duration::from_millis(60));
    cx.run_until_parked();
    redraw(cx);
    assert!(
        cx.debug_bounds("library-action-error").is_some(),
        "a failed queued full candidate must not erase the action warning"
    );
    view.read_with(cx, |view, cx| {
        assert!(matches!(
            view.model.read(cx).status(),
            AppStatus::Error(message) if message.contains("笔记已创建")
                && message.contains("资料库数据已提交")
        ));
        assert!(view.model.read(cx).reconciliation_pending());
        assert_eq!(view.model.read(cx).projection_event_refreshes_for_test(), 1);
    });

    // The retained event packet retries at the next bounded bridge tick. No
    // card click may be needed to recover the newly-created selection/session.
    cx.executor().advance_clock(Duration::from_millis(60));
    cx.run_until_parked();
    redraw(cx);
    view.read_with(cx, |view, cx| {
        assert_eq!(view.model.read(cx).status(), &AppStatus::Ready);
        assert_eq!(
            view.model.read(cx).navigation().selected_note_id(),
            Some(&created)
        );
        assert_eq!(view.model.read(cx).active_session_note_id(), Some(&created));
        assert!(
            !view.model.read(cx).reconciliation_pending(),
            "the warning clears only after the full candidate installs"
        );
    });
}

#[gpui::test]
async fn mounted_reconciliation_lock_has_a_distinct_notice_and_resource_error_from_trash(
    cx: &mut TestAppContext,
) {
    // A transient committed-but-unreconciled normal note is not a Trash
    // preview. Treating both as one ReadOnly UI state leaves people with the
    // false explanation that they must restore a live note before retrying.
    let (_profile, repository) = repository();
    let note = repository
        .create_note(CreateNote {
            title: "recovery lock".into(),
            notebook_id: None,
            document: rich_document("正文"),
        })
        .expect("create selected note");
    let tag = repository.create_tag("pending tag").expect("create tag");
    let (view, cx) = mount_shell(Arc::clone(&repository), cx);
    redraw(cx);
    cx.update(|window, app| {
        view.update(app, |shell, shell_cx| {
            shell.apply_action(AppAction::SelectNote(note.id.clone()), window, shell_cx);
        });
    });
    redraw(cx);
    view.update(cx, |view, view_cx| {
        view.model.update(view_cx, |model, _| {
            model.fail_next_refresh_for_test(app_lite_core::LibraryError::NotFound);
        });
    });
    cx.update(|window, app| {
        view.update(app, |shell, shell_cx| {
            shell.apply_action(
                AppAction::AddTagToSelectedNote(tag.id.clone()),
                window,
                shell_cx,
            );
        });
    });
    redraw(cx);

    assert!(
        cx.debug_bounds("native-editor-surface-recovery-locked-notice")
            .is_some(),
        "a temporary recovery fence must not paint the durable Trash notice"
    );
    let picker_error = view.update(cx, |shell, shell_cx| {
        shell
            .begin_resource_picker(shell_cx)
            .expect_err("the temporary lock blocks a new resource mutation")
    });
    assert!(picker_error.contains("正在恢复"));
    assert!(!picker_error.contains("废纸篓"));
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
async fn mounted_full_scale_fixture_keeps_query_modes_rows_and_thumbnail_work_bounded(
    cx: &mut TestAppContext,
) {
    // This is intentionally a single production-path scale gate rather than
    // three unrelated unit fixtures. It catches (1) a projection query that
    // starts loading canonical content, (2) a list-mode reducer that creates a
    // second query/session authority, and (3) a Cards renderer that turns all
    // 1,662 persisted thumbnail keys into foreground/resource work.
    let (profile, repository) = repository();
    let (_notebooks, _tags, selected_note_id) =
        install_projection_scale_fixture(&profile, repository.as_ref());
    let database = Connection::open(profile.path().join("library.sqlite"))
        .expect("open scale assertion database");
    let (note_rows, resource_rows, relation_rows): (i64, i64, i64) = database
        .query_row(
            "SELECT
               (SELECT count(*) FROM notes),
               (SELECT count(*) FROM resources),
               (SELECT count(*) FROM note_resources)",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .expect("count complete scale fixture");
    assert_eq!(note_rows, SCALE_NOTE_COUNT as i64);
    assert_eq!(resource_rows, SCALE_RESOURCE_COUNT as i64);
    assert_eq!(
        relation_rows, SCALE_RESOURCE_COUNT as i64,
        "every scale resource metadata row must participate in a real note_resources join"
    );
    assert_projection_scale_fixture_is_canonical_and_fully_associated(&database);
    drop(database);

    // These authorizers see actual SQLite Read opcodes even if future code
    // discards a loaded value before it reaches GPUI. They are deliberately
    // armed before AppModel::open, when both the typed navigation index and
    // the All Notes projection are queried for this 1,662-row route.
    let index_reads = repository.observe_next_navigation_index_query();
    let projection_reads = repository.observe_next_list_query();
    let body_loads = repository.observe_note_loads();
    let blob_reads = repository.observe_resource_reads();
    let verified_opens = repository.observe_verified_resource_opens();
    let model = cx.new(|_| AppModel::open(Arc::clone(&repository)).expect("open scale model"));
    let thumbnail_gate = Arc::new(Mutex::new(None));
    let gate_slot = Arc::clone(&thumbnail_gate);
    let (view, cx) = cx.add_window_view(move |window, cx| {
        let mut shell = LibraryShell::new(model, None, window, cx);
        *gate_slot.lock().expect("scale thumbnail gate poisoned") =
            Some(shell.stall_next_card_thumbnail_materialization_for_test());
        shell
    });
    let release_thumbnail_worker = thumbnail_gate
        .lock()
        .expect("scale thumbnail gate poisoned")
        .take()
        .expect("the first visible Card worker must be gateable");
    cx.simulate_resize(gpui::size(px(1_280.0), px(760.0)));
    redraw(cx);

    let forbidden_reads = [
        "notes.body_html",
        "notes.body_text",
        "notes.merge_state",
        "resource_blobs.bytes",
    ];
    let navigation_columns = index_reads
        .try_recv()
        .expect("mounted scale model must issue one navigation-index query");
    assert!(
        navigation_columns
            .iter()
            .all(|column| !forbidden_reads.contains(&column.as_str())),
        "sidebar scale metadata query loaded forbidden content: {navigation_columns:?}"
    );
    let projection_columns = projection_reads
        .try_recv()
        .expect("mounted scale model must issue one projection query");
    assert!(projection_columns.iter().any(|column| column == "notes.id"));
    assert!(
        projection_columns
            .iter()
            .all(|column| !forbidden_reads.contains(&column.as_str())),
        "1,662-row projection query loaded forbidden content: {projection_columns:?}"
    );
    assert_eq!(body_loads.try_recv(), Err(TryRecvError::Empty));
    assert_eq!(blob_reads.try_recv(), Err(TryRecvError::Empty));
    assert_eq!(verified_opens.try_recv(), Err(TryRecvError::Empty));

    let (projection_ids, cards_range, initial_desired, initial_queued) =
        view.read_with(cx, |shell, app| {
            let model = shell.model.read(app);
            assert_eq!(model.projections().len(), SCALE_NOTE_COUNT);
            assert_eq!(
                model.navigation_index().notebooks.len(),
                SCALE_NOTEBOOK_COUNT
            );
            assert_eq!(model.navigation_index().tags.len(), SCALE_TAG_COUNT);
            assert!(
                model.projections().iter().all(|projection| {
                    projection.attachment_count >= 1 && projection.selected_thumbnail_id.is_some()
                }),
                "every scale projection must expose a real associated image to the Cards query"
            );
            let range = shell
                .rendered_note_range
                .clone()
                .expect("Cards decoration must report the true visible range");
            (
                model
                    .projections()
                    .iter()
                    .map(|projection| projection.id.clone())
                    .collect::<Vec<_>>(),
                range,
                shell.card_thumbnails.desired_count_for_test(),
                shell.card_thumbnails.queued_count_for_test(),
            )
        });
    assert!(
        cards_range.len() < 128 && cards_range.end < SCALE_NOTE_COUNT,
        "the mounted Cards viewport must not construct all {SCALE_NOTE_COUNT} rows: {cards_range:?}"
    );
    assert!(
        initial_desired <= cards_range.len() && initial_desired < SCALE_NOTE_COUNT,
        "only the actual visible range may enter the Cards desired set: desired={initial_desired}, range={cards_range:?}"
    );
    assert!(
        initial_queued <= initial_desired && initial_queued < SCALE_NOTE_COUNT,
        "the thumbnail worker queue may not accumulate the full library: queued={initial_queued}, desired={initial_desired}"
    );

    // Select the one repository-created note after the query-only assertions.
    // It gives the mode switch a live editor/session identity to preserve,
    // while proving that the later mode operations do not trigger another
    // canonical-body load.
    cx.update(|window, app| {
        view.update(app, |shell, shell_cx| {
            shell.apply_action(
                AppAction::SelectNote(selected_note_id.clone()),
                window,
                shell_cx,
            );
        });
    });
    redraw(cx);
    assert_eq!(
        body_loads.try_recv(),
        Ok(selected_note_id.clone()),
        "only the explicit selected NoteId may hydrate its canonical document"
    );
    let (session_id, selected_before_modes) = view.read_with(cx, |shell, app| {
        (
            shell
                .note_session
                .as_ref()
                .expect("selected scale note must mount one session")
                .entity_id(),
            shell
                .model
                .read(app)
                .navigation()
                .selected_note_id()
                .cloned(),
        )
    });
    assert_eq!(selected_before_modes, Some(selected_note_id.clone()));

    // These are UI-only presentation changes. If any mode starts a fresh SQL
    // route query or a replacement NoteSession, the one-shot observer and the
    // entity identity assertions below fail.
    let mode_queries = repository.observe_next_list_query();
    for mode in [
        ListViewMode::Snippets,
        ListViewMode::Compact,
        ListViewMode::Cards,
    ] {
        cx.update(|window, app| {
            view.update(app, |shell, shell_cx| {
                shell.apply_action(AppAction::SetListViewMode(mode), window, shell_cx);
            });
        });
        redraw(cx);
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
                "{mode:?} must consume the original typed projection ordering"
            );
            assert_eq!(
                model.navigation().selected_note_id(),
                Some(&selected_note_id),
                "{mode:?} must retain selection by NoteId"
            );
            assert_eq!(
                shell
                    .note_session
                    .as_ref()
                    .expect("mode switch must retain the selected session")
                    .entity_id(),
                session_id,
                "{mode:?} must not recreate the selected NoteSession"
            );
        });
    }
    assert_eq!(
        mode_queries.try_recv(),
        Err(TryRecvError::Empty),
        "Cards/Snippets/Compact must not issue a second list query"
    );
    assert_eq!(
        body_loads.try_recv(),
        Err(TryRecvError::Empty),
        "mode changes must not hydrate another note body"
    );
    assert_eq!(
        blob_reads.try_recv(),
        Err(TryRecvError::Empty),
        "mode changes must not read resource blobs through the Vec API"
    );

    // Let the first actual Cards task run only after the bounded queue and
    // cross-mode assertions have been taken. A few deterministic redraws are
    // enough to observe at least one real descriptor-safe visible source;
    // they must remain bounded by the final viewport, never by resource row
    // count or a scrolling history.
    drop(release_thumbnail_worker);
    for _ in 0..8 {
        redraw(cx);
        if view.read_with(cx, |shell, _| {
            shell.card_thumbnails.ready_count_for_test() > 0
        }) {
            break;
        }
    }
    let (final_range, final_desired, final_ready, final_source_bytes, decoded_entries) = view
        .read_with(cx, |shell, app| {
            (
                shell
                    .rendered_note_range
                    .clone()
                    .expect("Cards range remains visible after mode return"),
                shell.card_thumbnails.desired_count_for_test(),
                shell.card_thumbnails.ready_count_for_test(),
                shell.card_thumbnails.source_bytes_for_test(),
                shell.card_thumbnail_cache.read(app).len(),
            )
        });
    assert!(final_desired <= final_range.len());
    assert!(
        final_ready > 0 && final_ready <= final_desired,
        "only final visible thumbnail keys may be materialized: ready={final_ready}, desired={final_desired}"
    );
    assert!(
        final_source_bytes <= super::card_thumbnail::CARD_THUMBNAIL_SOURCE_CACHE_BUDGET,
        "visible card proxy sources exceeded the explicit disk budget"
    );
    assert!(
        decoded_entries <= final_desired,
        "decoded card cache may not retain more entries than the actual visible thumbnail set"
    );
    let verified_open_count = verified_opens.try_iter().count();
    assert!(
        verified_open_count <= final_desired.saturating_add(1),
        "one final viewport may finish at most one already-started descriptor-safe worker plus its own visible resources: opens={verified_open_count}, desired={final_desired}, range={final_range:?}"
    );
}

#[gpui::test]
async fn mounted_full_scale_fixture_reaches_each_mode_tail_and_retains_filters_panes_session(
    cx: &mut TestAppContext,
) {
    // Mutation-sensitive: replacing a UniformList with an eager column makes
    // a tail range unreachable; a second mode/query authority, filter that
    // drops a still-valid NoteId, or pane collapse that remounts the editor
    // changes one of the durable identity assertions below.
    let (profile, repository) = repository();
    let (notebooks, tags, selected_note_id) =
        install_projection_scale_fixture(&profile, repository.as_ref());
    let database = Connection::open(profile.path().join("library.sqlite"))
        .expect("open scale route assertion database");
    assert_projection_scale_fixture_is_canonical_and_fully_associated(&database);
    drop(database);

    let forbidden_reads = [
        "notes.body_html",
        "notes.body_text",
        "notes.merge_state",
        "resource_blobs.bytes",
    ];
    let initial_query = repository.observe_next_list_query();
    let body_loads = repository.observe_note_loads();
    let blob_reads = repository.observe_resource_reads();
    let (view, cx) = mount_shell(Arc::clone(&repository), cx);
    cx.simulate_resize(gpui::size(px(1_280.0), px(760.0)));
    redraw(cx);
    let initial_columns = initial_query
        .try_recv()
        .expect("initial full-scale projection query");
    assert!(
        initial_columns
            .iter()
            .all(|column| !forbidden_reads.contains(&column.as_str())),
        "initial full-scale projection query read canonical content: {initial_columns:?}"
    );

    cx.update(|window, app| {
        view.update(app, |shell, shell_cx| {
            shell.apply_action(
                AppAction::SelectNote(selected_note_id.clone()),
                window,
                shell_cx,
            );
        });
    });
    redraw(cx);
    assert_eq!(
        body_loads.try_recv(),
        Ok(selected_note_id.clone()),
        "only the explicitly selected full-scale note may hydrate"
    );
    let (tail_index, session_id) = view.read_with(cx, |shell, app| {
        let model = shell.model.read(app);
        assert_eq!(model.projections().len(), SCALE_NOTE_COUNT);
        (
            model.projections().len() - 1,
            shell
                .note_session
                .as_ref()
                .expect("selected scale note mounts one live session")
                .entity_id(),
        )
    });
    let tail_card_selector: &'static str =
        Box::leak(format!("library-note-card-{tail_index}").into_boxed_str());

    for mode in [
        ListViewMode::Snippets,
        ListViewMode::Compact,
        ListViewMode::Cards,
    ] {
        cx.update(|window, app| {
            view.update(app, |shell, shell_cx| {
                shell.apply_action(AppAction::SetListViewMode(mode), window, shell_cx);
            });
        });
        redraw(cx);
        cx.update(|_, app| {
            view.update(app, |shell, _| {
                shell
                    .note_list_scroll
                    .scroll_to_item(tail_index, gpui::ScrollStrategy::Center);
            });
        });
        redraw(cx);
        let (range, desired, queued, source_bytes, retained_session) =
            view.read_with(cx, |shell, app| {
                (
                    shell
                        .rendered_note_range
                        .clone()
                        .expect("each list mode must report its actual viewport range"),
                    shell.card_thumbnails.desired_count_for_test(),
                    shell.card_thumbnails.queued_count_for_test(),
                    shell.card_thumbnails.source_bytes_for_test(),
                    shell
                        .note_session
                        .as_ref()
                        .expect("tail scrolling may not unmount the selected session")
                        .entity_id(),
                )
            });
        assert!(
            range.contains(&tail_index) && range.len() < 128,
            "{mode:?} must construct the actual bounded tail viewport: {range:?}"
        );
        assert!(
            cx.debug_bounds(tail_card_selector).is_some(),
            "{mode:?} must paint a real tail row after the retained scroll request"
        );
        assert_eq!(
            retained_session, session_id,
            "{mode:?} tail scrolling must preserve the live NoteSession"
        );
        if matches!(mode, ListViewMode::Cards) {
            assert!(
                desired <= range.len() && queued <= desired,
                "Cards tail thumbnail work must remain within the current viewport: desired={desired}, queued={queued}, range={range:?}"
            );
            assert!(
                source_bytes <= super::card_thumbnail::CARD_THUMBNAIL_SOURCE_CACHE_BUDGET,
                "Cards tail sources exceeded the explicit bounded cache budget"
            );
        } else {
            assert_eq!(
                desired, 0,
                "{mode:?} must leave Cards thumbnail residency rather than retaining invisible image work"
            );
        }
    }
    assert_eq!(body_loads.try_recv(), Err(TryRecvError::Empty));
    assert_eq!(blob_reads.try_recv(), Err(TryRecvError::Empty));

    let notebook_route = LibraryRoute::Notebook(notebooks[0].id.clone());
    let tag_route = LibraryRoute::tags(vec![tags[0].id.clone()]).expect("scale tag route");
    for (route, label) in [(notebook_route, "Notebook"), (tag_route, "Tag")] {
        let route_query = repository.observe_next_list_query();
        cx.update(|window, app| {
            view.update(app, |shell, shell_cx| {
                shell.apply_action(
                    AppAction::NavigateTo {
                        route: route.clone(),
                        selected_note_id: Some(selected_note_id.clone()),
                    },
                    window,
                    shell_cx,
                );
            });
        });
        redraw(cx);
        let columns = route_query
            .try_recv()
            .expect("every filter route must issue one observed projection query");
        assert!(
            columns
                .iter()
                .all(|column| !forbidden_reads.contains(&column.as_str())),
            "{label} scale filter query read canonical content: {columns:?}"
        );
        view.read_with(cx, |shell, app| {
            let model = shell.model.read(app);
            assert_eq!(model.navigation().route(), &route);
            assert_eq!(
                model.navigation().selected_note_id(),
                Some(&selected_note_id)
            );
            assert!(
                model
                    .projections()
                    .iter()
                    .any(|projection| projection.id == selected_note_id),
                "{label} filter must retain the valid selected NoteId in its projection"
            );
            assert_eq!(
                shell
                    .note_session
                    .as_ref()
                    .expect("valid filtered selection retains its session")
                    .entity_id(),
                session_id,
                "{label} filter may not remount a same-revision selected session"
            );
        });
    }

    for (action, sidebar_visible, list_visible) in [
        (AppAction::ToggleSidebar, false, true),
        (AppAction::ToggleNoteList, false, false),
        (AppAction::ToggleNoteList, false, true),
        (AppAction::ToggleSidebar, true, true),
    ] {
        cx.update(|window, app| {
            view.update(app, |shell, shell_cx| {
                shell.apply_action(action, window, shell_cx)
            });
        });
        redraw(cx);
        view.read_with(cx, |shell, app| {
            let model = shell.model.read(app);
            assert_eq!(model.panes().sidebar_visible, sidebar_visible);
            assert_eq!(model.panes().list_visible, list_visible);
            assert_eq!(
                model.navigation().selected_note_id(),
                Some(&selected_note_id)
            );
            assert_eq!(
                shell
                    .note_session
                    .as_ref()
                    .expect("three/two/one-pane transitions retain the live session")
                    .entity_id(),
                session_id,
                "pane transitions must not remount the selected editor/session"
            );
        });
    }
    assert_eq!(body_loads.try_recv(), Err(TryRecvError::Empty));
    assert_eq!(blob_reads.try_recv(), Err(TryRecvError::Empty));
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
