//! Mounted table cell editing: double-click opens the cell, "完成" saves,
//! Tab moves on (adding a row after the last cell), Escape discards.

use crate::app::save_coordinator::ManualSaveClock;
use crate::app::{AppAction, AppModel};
use crate::native_editor::model::BlockContent;
use crate::ui::LibraryShell;
use app_lite_core::{CanonicalDocument, CreateNote, LibraryRepository, SaveNote};
use gpui::{
    AppContext, Modifiers, MouseButton, MouseDownEvent, MouseUpEvent, TestAppContext,
    VisualTestContext, point, px,
};
use std::sync::Arc;

const TABLE: &str = "<p>前</p><table data-joplin-lite-table=\"true\"><tbody><tr><th>名</th><th>值</th></tr><tr><td>甲</td><td>一</td></tr></tbody></table>";

#[gpui::test]
async fn saving_open_cell_includes_keyboard_input_and_keeps_editor_open(cx: &mut TestAppContext) {
    let (_root, repository, note, view, cx) = mount(cx);
    let (_, bounds) = table_node(&view, cx);
    let position = point(bounds.left() + bounds.size.width * 0.75, bounds.bottom() - px(4.0));
    cx.simulate_event(MouseDownEvent {
        button: MouseButton::Left, position, modifiers: Default::default(),
        click_count: 2, first_mouse: false,
    });
    cx.simulate_event(MouseUpEvent {
        button: MouseButton::Left, position, modifiers: Default::default(), click_count: 2,
    });
    cx.run_until_parked();
    cx.update(|window, app| window.draw(app).clear());
    assert_eq!(open_cell(&view, cx), Some((1, 1)));
    cx.simulate_input("单元格新增");
    cx.run_until_parked();
    let body = saved_body(&view, cx, &repository, &note);
    assert!(body.contains("单元格新增"), "manual save lost the open cell's input: {body}");
    assert_eq!(open_cell(&view, cx), Some((1, 1)), "save should not dismiss the editing context");
    // A later save must include later input, not just the first cell snapshot.
    cx.simulate_input("继续");
    cx.run_until_parked();
    let body = saved_body(&view, cx, &repository, &note);
    assert!(body.contains("单元格新增继续"), "{body}");
    let revision = repository.load_note(&note).unwrap().unwrap().revision;
    let unchanged = saved_body(&view, cx, &repository, &note);
    assert_eq!(unchanged, body);
    assert_eq!(repository.load_note(&note).unwrap().unwrap().revision, revision,
        "saving an unchanged cell must not create another durable revision");
    cx.simulate_keystrokes("cmd-z");
    cx.run_until_parked();
    let undone = saved_body(&view, cx, &repository, &note);
    assert!(!undone.contains("继续"), "keyboard undo must affect the focused cell: {undone}");
    assert!(undone.contains("一"), "undo must preserve the pre-existing cell content");
    cx.simulate_keystrokes("cmd-shift-z");
    cx.run_until_parked();
    assert_eq!(saved_body(&view, cx, &repository, &note), body, "redo restores the saved cell content");
    // Leave and reopen through the shell, then read the durable document again.
    cx.update(|window, app| view.update(app, |shell, shell_cx| {
        shell.apply_action(AppAction::CreateNote, window, shell_cx);
    }));
    cx.run_until_parked();
    assert_eq!(open_cell(&view, cx), None);
    cx.update(|window, app| view.update(app, |shell, shell_cx| {
        shell.apply_action(AppAction::SelectNote(note.clone()), window, shell_cx);
    }));
    cx.run_until_parked();
    assert_eq!(repository.load_note(&note).unwrap().unwrap().body_html, body);
    assert!(view.read_with(cx, |shell, app| {
        shell.note_session.as_ref().unwrap().read(app).editor().read(app).document().blocks()
            .iter().any(|block| matches!(&block.content, BlockContent::Table(table)
                if table.rows.iter().flatten().any(|text| text.contains("单元格新增继续"))))
    }));
}

#[gpui::test]
async fn open_cell_composition_blocks_lifecycle_save(cx: &mut TestAppContext) {
    use crate::app::save_coordinator::FlushReason;
    use gpui::EntityInputHandler;
    let (_root, repository, note, view, cx) = mount(cx);
    let (node, _) = table_node(&view, cx);
    cx.update(|_, app| view.update(app, |shell, shell_cx| {
        shell.open_table_cell_editor(node, 1, 1, shell_cx);
    }));
    cx.run_until_parked();
    let editor = view.read_with(cx, |shell, _| shell.table_cell_editor.as_ref().unwrap().editor.clone());
    cx.update(|window, app| editor.update(app, |editor, editor_cx| {
        EntityInputHandler::replace_and_mark_text_in_range(editor, Some(0..0), "候选", Some(2..2), window, editor_cx);
    }));
    assert!(view.read_with(cx, |shell, app| shell.active_session_has_unsaved_changes(app)),
        "sync must treat the open cell's unconfirmed input as pending local work");
    let allowed = cx.update(|_, app| view.update(app, |shell, shell_cx| {
        shell.flush_for_lifecycle(FlushReason::WindowClose, shell_cx)
    }));
    assert!(!allowed, "unconfirmed cell composition must keep the window alive");
    cx.run_until_parked();
    assert_eq!(open_cell(&view, cx), Some((1, 1)));
    assert!(!repository.load_note(&note).unwrap().unwrap().body_html.contains("候选"));
    // Neither toolbar completion nor Tab may serialize provisional IME text.
    cx.update(|_, app| view.update(app, |shell, shell_cx| {
        shell.commit_table_cell_editor(false, shell_cx);
    }));
    assert_eq!(open_cell(&view, cx), Some((1, 1)), "Done must retain composing cell");
    cx.update(|window, app| window.draw(app).clear());
    cx.simulate_keystrokes("tab");
    cx.run_until_parked();
    assert_eq!(open_cell(&view, cx), Some((1, 1)), "Tab must not advance during composition");
    assert!(view.read_with(cx, |shell, app| {
        shell.note_session.as_ref().unwrap().read(app).editor().read(app).document().blocks()
            .iter().all(|block| !matches!(&block.content, BlockContent::Table(table)
                if table.rows.iter().flatten().any(|text| text.contains("候选"))))
    }), "provisional input must never enter the main note");
}

fn mount<'a>(
    cx: &'a mut TestAppContext,
) -> (
    tempfile::TempDir,
    Arc<LibraryRepository>,
    app_lite_core::NoteId,
    gpui::Entity<LibraryShell>,
    &'a mut VisualTestContext,
) {
    mount_with_clock(cx, Arc::new(ManualSaveClock::default()))
}

fn mount_with_clock<'a>(
    cx: &'a mut TestAppContext,
    clock: Arc<ManualSaveClock>,
) -> (
    tempfile::TempDir,
    Arc<LibraryRepository>,
    app_lite_core::NoteId,
    gpui::Entity<LibraryShell>,
    &'a mut VisualTestContext,
) {
    mount_body(cx, clock, TABLE)
}

fn mount_body<'a>(
    cx: &'a mut TestAppContext,
    clock: Arc<ManualSaveClock>,
    body: &str,
) -> (
    tempfile::TempDir,
    Arc<LibraryRepository>,
    app_lite_core::NoteId,
    gpui::Entity<LibraryShell>,
    &'a mut VisualTestContext,
) {
    cx.update(|app| crate::components::init(app));
    let root = tempfile::tempdir().unwrap();
    let repository = Arc::new(LibraryRepository::open(root.path().join("library.sqlite")).unwrap());
    let note = repository
        .create_note(CreateNote {
            title: "表格".into(),
            notebook_id: None,
            document: CanonicalDocument::from_blocks(Vec::new()),
        })
        .unwrap();
    repository
        .save_note(SaveNote {
            id: note.id.clone(),
            expected_revision: note.revision,
            title: "表格".into(),
            document: CanonicalDocument::parse_html(body).unwrap(),
            resource_ids: vec![],
            selected_thumbnail_id: None,
        })
        .unwrap();
    let model_repository = Arc::clone(&repository);
    let model = cx.new(move |_| AppModel::open(model_repository).unwrap());
    let (view, vcx) = cx.add_window_view(move |window, cx| {
        LibraryShell::new_with_save_clock(model.clone(), None, clock, window, cx)
    });
    let id = note.id.clone();
    vcx.update(|window, app| {
        view.update(app, |shell, shell_cx| {
            shell.apply_action(AppAction::SelectNote(id), window, shell_cx);
        })
    });
    vcx.run_until_parked();
    vcx.update(|window, app| window.draw(app).clear());
    vcx.run_until_parked();
    (root, repository, note.id, view, vcx)
}

#[gpui::test]
async fn open_cell_uses_automatic_journal_and_snapshot_then_cancel_restores_original(cx: &mut TestAppContext) {
    use std::time::Duration;
    let clock = Arc::new(ManualSaveClock::default());
    let (_root, repository, note, view, cx) = mount_with_clock(cx, clock.clone());
    let (node, _) = table_node(&view, cx);
    cx.update(|_, app| view.update(app, |shell, shell_cx| {
        shell.note_session.as_ref().unwrap().update(shell_cx, |session, _| session.enable_deadline_tasks_for_test());
        shell.open_table_cell_editor(node, 1, 1, shell_cx);
    }));
    cx.run_until_parked();
    cx.update(|window, app| window.draw(app).clear());
    cx.simulate_input("自动保存的单元格");
    cx.run_until_parked();
    clock.advance(Duration::from_millis(100));
    cx.executor().advance_clock(Duration::from_millis(100));
    cx.run_until_parked();
    let journal = repository.latest_edit_journal(&note).unwrap().expect("open cell input needs the same crash journal as ordinary text");
    assert!(journal.delta_utf8.contains("自动保存的单元格"));
    assert!(!repository.load_note(&note).unwrap().unwrap().body_html.contains("自动保存的单元格"), "this must test the journal before snapshot compaction");
    assert!(crate::app::note_session::NoteSession::prepare(repository.load_note(&note).unwrap().unwrap(), &repository).is_ok());
    clock.advance(Duration::from_millis(400));
    cx.executor().advance_clock(Duration::from_millis(400));
    cx.run_until_parked();
    assert!(repository.load_note(&note).unwrap().unwrap().body_html.contains("自动保存的单元格"));
    assert_eq!(open_cell(&view, cx), Some((1, 1)));
    cx.simulate_keystrokes("escape");
    cx.run_until_parked();
    assert_eq!(open_cell(&view, cx), None);
    clock.advance(Duration::from_millis(500));
    cx.executor().advance_clock(Duration::from_millis(500));
    cx.run_until_parked();
    let body = repository.load_note(&note).unwrap().unwrap().body_html;
    assert!(!body.contains("自动保存的单元格"), "cancel must durably restore the original cell: {body}");
    assert!(body.contains("<td>一</td>"));
}

fn table_node(
    view: &gpui::Entity<LibraryShell>,
    cx: &mut VisualTestContext,
) -> (
    crate::native_editor::model::NodeId,
    gpui::Bounds<gpui::Pixels>,
) {
    view.read_with(cx, |shell, app| {
        let editor = shell
            .note_session
            .as_ref()
            .unwrap()
            .read(app)
            .editor()
            .read(app);
        let block = editor
            .document()
            .blocks()
            .iter()
            .find(|block| matches!(block.content, BlockContent::Table(_)))
            .unwrap();
        (
            block.id,
            editor.layout().block_layout(block.id).unwrap().bounds,
        )
    })
}

#[gpui::test]
async fn open_cell_journal_recovers_after_owner_destruction_without_flush(cx: &mut TestAppContext) {
    use std::time::Duration;
    let clock = Arc::new(ManualSaveClock::default());
    let (root, repository, note, view, cx) = mount_with_clock(cx, clock.clone());
    let (node, _) = table_node(&view, cx);
    let old_session = view.read_with(cx, |shell, _| shell.note_session.as_ref().unwrap().downgrade());
    cx.update(|_, app| view.update(app, |shell, shell_cx| {
        shell.note_session.as_ref().unwrap().update(shell_cx, |session, _| session.enable_deadline_tasks_for_test());
        shell.open_table_cell_editor(node, 1, 1, shell_cx);
    }));
    cx.run_until_parked();
    cx.update(|window, app| window.draw(app).clear());
    cx.simulate_input("崩溃前的表格输入");
    cx.run_until_parked();
    clock.advance(Duration::from_millis(100));
    cx.executor().advance_clock(Duration::from_millis(100));
    cx.run_until_parked();
    assert!(repository.latest_edit_journal(&note).unwrap().is_some());
    assert!(!repository.load_note(&note).unwrap().unwrap().body_html.contains("崩溃前的表格输入"));
    // Destroy the mounted owners without dispatching close/save/cancel.
    cx.update(|window, _| window.remove_window());
    drop(view);
    cx.cx.update(|_| {}); // release zero-count GPUI owners at an app effect boundary
    cx.run_until_parked();
    assert!(old_session.upgrade().is_none(), "recovery must not read a surviving in-memory owner");
    drop(repository);
    let repository = Arc::new(LibraryRepository::open(root.path().join("library.sqlite")).unwrap());
    let base = repository.load_note(&note).unwrap().unwrap();
    assert!(!base.body_html.contains("崩溃前的表格输入"));
    let restart = cx.cx.add_empty_window();
    let recovered = restart.new(|cx| crate::app::note_session::NoteSession::open(base, repository.clone(), clock, cx).unwrap());
    assert!(recovered.read_with(restart, |session, app| session.editor().read(app).document().blocks()
        .iter().any(|block| matches!(&block.content, BlockContent::Table(table)
            if table.rows.iter().flatten().any(|text| text.contains("崩溃前的表格输入"))))));
}

fn open_cell(
    view: &gpui::Entity<LibraryShell>,
    cx: &mut VisualTestContext,
) -> Option<(usize, usize)> {
    view.read_with(cx, |shell, _| {
        shell
            .table_cell_editor
            .as_ref()
            .map(|cell| (cell.row, cell.column))
    })
}

fn type_in_cell(view: &gpui::Entity<LibraryShell>, cx: &mut VisualTestContext, text: &str) {
    let editor = view.read_with(cx, |shell, _| {
        shell.table_cell_editor.as_ref().unwrap().editor.clone()
    });
    cx.update(|_, app| {
        editor.update(app, |editor, _| {
            editor.select_all();
            editor.insert_text(text).unwrap();
        });
    });
}

fn saved_body(
    view: &gpui::Entity<LibraryShell>,
    cx: &mut VisualTestContext,
    repository: &LibraryRepository,
    note: &app_lite_core::NoteId,
) -> String {
    cx.update(|window, app| {
        view.update(app, |shell, shell_cx| {
            shell.apply_action(AppAction::ManualSync, window, shell_cx);
        })
    });
    cx.run_until_parked();
    repository.load_note(note).unwrap().unwrap().body_html
}

#[gpui::test]
async fn cell_text_colour_keeps_alpha_and_saves_a_new_colour(cx: &mut TestAppContext) {
    use crate::native_editor::commands::{CommandArgument, CommandCatalogue, EditorCommand};
    use crate::native_editor::model::{DocPoint, Selection};
    let table = "<table data-joplin-lite-table=\"true\"><tbody><tr><td><span style=\"color: rgba(24, 133, 226, 0.502)\">蓝</span>白</td></tr></tbody></table>";
    let (_root, repository, note, view, cx) =
        mount_body(cx, Arc::new(ManualSaveClock::default()), table);
    let (node, _) = table_node(&view, cx);
    cx.update(|_, app| {
        view.update(app, |shell, shell_cx| {
            shell.open_table_cell_editor(node, 0, 0, shell_cx)
        })
    });
    cx.run_until_parked();
    let editor = view.read_with(cx, |shell, _| {
        shell.table_cell_editor.as_ref().unwrap().editor.clone()
    });
    cx.update(|_, app| {
        editor.update(app, |editor, _| {
            let block = editor.document().first_node_id().unwrap();
            editor.set_selection_for_test(Selection::new(
                DocPoint::new(block, "蓝".len()),
                DocPoint::new(block, "蓝白".len()),
            ));
            CommandCatalogue::new()
                .execute(
                    EditorCommand::TextColor,
                    CommandArgument::TextColor(app_lite_core::TextColor::parse(
                        "hwb(120, 0%, 50%)",
                    )),
                    editor,
                )
                .unwrap();
        })
    });
    cx.update(|_, app| {
        view.update(app, |shell, shell_cx| {
            shell.commit_table_cell_editor(false, shell_cx)
        })
    });
    cx.run_until_parked();
    let coloured = "<table data-joplin-lite-table=\"true\"><tbody><tr><td><span style=\"color: rgba(24, 133, 226, 0.502)\">蓝</span><span style=\"color: #008000\">白</span></td></tr></tbody></table>";
    assert_eq!(saved_body(&view, cx, &repository, &note), coloured);
    // The table-only note now also undoes, redoes and reopens its cell edit.
    let main = view.read_with(cx, |shell, app| {
        shell
            .note_session
            .as_ref()
            .unwrap()
            .read(app)
            .editor()
            .clone()
    });
    cx.update(|_, app| main.update(app, |editor, _| editor.undo().unwrap()));
    assert_eq!(saved_body(&view, cx, &repository, &note), table);
    cx.update(|_, app| main.update(app, |editor, _| editor.redo().unwrap()));
    assert_eq!(saved_body(&view, cx, &repository, &note), coloured);
    let base = repository.load_note(&note).unwrap().unwrap();
    let reopened = cx.new(|cx| {
        crate::app::note_session::NoteSession::open(
            base,
            repository.clone(),
            Arc::new(ManualSaveClock::default()),
            cx,
        )
        .unwrap()
    });
    let html = reopened.read_with(cx, |session, app| {
        crate::native_editor::codec::export_canonical(session.editor().read(app).document())
            .unwrap()
            .to_canonical_html()
    });
    assert_eq!(html.as_str(), coloured);
}

#[gpui::test]
async fn double_click_opens_the_cell_and_done_saves_it(cx: &mut TestAppContext) {
    let (_root, repository, note, view, cx) = mount(cx);
    let (_, bounds) = table_node(&view, cx);
    // Second row, second column of a 2x2 grid.
    let target = point(
        bounds.left() + bounds.size.width * 0.75,
        bounds.bottom() - px(4.0),
    );
    for count in [1, 2] {
        cx.simulate_event(MouseDownEvent {
            button: MouseButton::Left,
            position: target,
            modifiers: Modifiers::default(),
            click_count: count,
            first_mouse: false,
        });
        cx.simulate_event(MouseUpEvent {
            button: MouseButton::Left,
            position: target,
            modifiers: Modifiers::default(),
            click_count: count,
        });
    }
    cx.run_until_parked();
    assert_eq!(open_cell(&view, cx), Some((1, 1)));
    type_in_cell(&view, cx, "改后");
    cx.update(|_, app| {
        view.update(app, |shell, shell_cx| {
            shell.commit_table_cell_editor(false, shell_cx)
        })
    });
    cx.run_until_parked();
    assert_eq!(open_cell(&view, cx), None);
    let body = saved_body(&view, cx, &repository, &note);
    assert!(body.contains("<td>甲</td><td>改后</td>"), "{body}");
}

#[gpui::test]
async fn tab_in_the_last_cell_adds_a_row_and_escape_discards(cx: &mut TestAppContext) {
    let (_root, repository, note, view, cx) = mount(cx);
    let (node, _) = table_node(&view, cx);
    cx.update(|_, app| {
        view.update(app, |shell, shell_cx| {
            shell.open_table_cell_editor(node, 1, 1, shell_cx)
        })
    });
    type_in_cell(&view, cx, "末格");
    cx.update(|_, app| {
        view.update(app, |shell, shell_cx| {
            shell.commit_table_cell_editor(true, shell_cx)
        })
    });
    cx.run_until_parked();
    assert_eq!(open_cell(&view, cx), Some((2, 0)), "a new row, first cell");
    type_in_cell(&view, cx, "不要");
    cx.update(|_, app| {
        view.update(app, |shell, shell_cx| {
            shell.cancel_table_cell_editor(shell_cx)
        })
    });
    cx.run_until_parked();
    let body = saved_body(&view, cx, &repository, &note);
    assert!(body.contains("<td>末格</td>"), "{body}");
    assert!(!body.contains("不要"), "{body}");
    assert_eq!(body.matches("<tr>").count(), 3, "{body}");
}

#[gpui::test]
async fn row_and_column_buttons_change_the_table_around_the_open_cell(cx: &mut TestAppContext) {
    use crate::ui::table_cell_editor::TableStructureChange;
    let (_root, repository, note, view, cx) = mount(cx);
    let (node, _) = table_node(&view, cx);
    cx.update(|_, app| {
        view.update(app, |shell, shell_cx| {
            shell.open_table_cell_editor(node, 0, 0, shell_cx)
        })
    });
    for change in [
        TableStructureChange::InsertRowBelow,
        TableStructureChange::InsertColumnRight,
    ] {
        cx.update(|_, app| {
            view.update(app, |shell, shell_cx| {
                shell.edit_table_structure(change, shell_cx)
            })
        });
        cx.run_until_parked();
    }
    assert_eq!(open_cell(&view, cx), Some((1, 1)));
    let body = saved_body(&view, cx, &repository, &note);
    assert_eq!(body.matches("<tr>").count(), 3, "{body}");
    assert_eq!(body.matches("<th>").count(), 3, "{body}");
    cx.update(|_, app| {
        view.update(app, |shell, shell_cx| {
            shell.edit_table_structure(TableStructureChange::DeleteRow, shell_cx);
            shell.edit_table_structure(TableStructureChange::DeleteColumn, shell_cx);
        })
    });
    cx.run_until_parked();
    let body = saved_body(&view, cx, &repository, &note);
    assert_eq!(body.matches("<tr>").count(), 2, "{body}");
    assert_eq!(body.matches("<th>").count(), 2, "{body}");
}

#[gpui::test]
async fn insert_table_from_the_menu_opens_its_first_cell(cx: &mut TestAppContext) {
    let (_root, repository, note, view, cx) = mount(cx);
    cx.dispatch_action(crate::app::InsertNoteTable);
    cx.run_until_parked();
    assert_eq!(open_cell(&view, cx), Some((0, 0)));
    type_in_cell(&view, cx, "新表头");
    cx.update(|_, app| {
        view.update(app, |shell, shell_cx| {
            shell.commit_table_cell_editor(false, shell_cx)
        })
    });
    cx.run_until_parked();
    let body = saved_body(&view, cx, &repository, &note);
    assert_eq!(body.matches("data-joplin-lite-table").count(), 2, "{body}");
    assert!(body.contains("<th>新表头</th>"), "{body}");
}

#[gpui::test]
async fn cmd_v_in_a_clicked_cell_pastes_into_that_cell_not_the_body(cx: &mut TestAppContext) {
    let (_root, repository, note, view, cx) = mount(cx);
    let (_, bounds) = table_node(&view, cx);
    let target = point(
        bounds.left() + bounds.size.width * 0.75,
        bounds.bottom() - px(4.0),
    );
    for count in [1, 2] {
        cx.simulate_event(MouseDownEvent {
            button: MouseButton::Left,
            position: target,
            modifiers: Modifiers::default(),
            click_count: count,
            first_mouse: false,
        });
        cx.simulate_event(MouseUpEvent {
            button: MouseButton::Left,
            position: target,
            modifiers: Modifiers::default(),
            click_count: count,
        });
    }
    cx.run_until_parked();
    cx.update(|window, app| window.draw(app).clear());
    assert_eq!(open_cell(&view, cx), Some((1, 1)));

    cx.write_to_clipboard(gpui::ClipboardItem::new_string("粘贴丙".to_owned()));
    cx.simulate_keystrokes("cmd-v");
    cx.run_until_parked();
    let fragment = crate::native_editor::images::ClipboardFragment {
        version: crate::native_editor::images::CLIPBOARD_FRAGMENT_VERSION,
        html: "<p><strong>粗</strong></p>".into(),
        plain: "粗".into(),
        resources: Vec::new(),
        open_start: true,
        open_end: true,
    };
    cx.write_to_clipboard(gpui::ClipboardItem::new_string_with_json_metadata(
        "粗".into(),
        fragment,
    ));
    cx.simulate_keystrokes("cmd-v");
    cx.run_until_parked();
    let png = crate::native_editor::images::ClipboardPayload::fixture_with_png_and_text("")
        .images
        .into_iter()
        .next()
        .unwrap();
    cx.write_to_clipboard(gpui::ClipboardItem::new_image(&gpui::Image::from_bytes(
        gpui::ImageFormat::Png,
        png.bytes,
    )));
    cx.simulate_keystrokes("cmd-v");
    cx.run_until_parked();
    assert!(
        view.read_with(cx, |shell, _| shell.queued_resource_inserts.is_empty()),
        "an image pasted into a cell is never queued for the body"
    );
    cx.simulate_keystrokes("tab");
    cx.run_until_parked();
    assert_eq!(open_cell(&view, cx), Some((2, 0)), "Tab still moves on after a paste");

    let body = saved_body(&view, cx, &repository, &note);
    // resource.ts insertResourceAtPosition at the end of the cell's text:
    // the image after it and an empty paragraph for the caret (the cell's
    // paragraphs are separated by line breaks in its saved content).
    let image = body
        .split(":/")
        .nth(1)
        .map(|rest| &rest[..32])
        .unwrap_or_default();
    assert!(
        body.contains(&format!(
            "<td>甲</td><td>一粘贴丙<strong>粗</strong><br><img src=\":/{image}\" alt=\"\"><br></td>"
        )),
        "{body}"
    );
    assert!(
        body.starts_with("<p>前</p><table"),
        "the body before the table is untouched: {body}"
    );
    assert_eq!(body.matches("<img").count(), 1, "{body}");
}

/// Opens a cell with the caret after its text.
fn open_cell_at_end(
    view: &gpui::Entity<LibraryShell>,
    cx: &mut VisualTestContext,
    row: usize,
    column: usize,
) -> gpui::Entity<crate::native_editor::core::EditorCore> {
    use crate::native_editor::model::{DocPoint, Selection};
    let (node, _) = table_node(view, cx);
    cx.update(|_, app| {
        view.update(app, |shell, shell_cx| {
            shell.open_table_cell_editor(node, row, column, shell_cx)
        })
    });
    cx.run_until_parked();
    cx.update(|window, app| window.draw(app).clear());
    let editor = view.read_with(cx, |shell, _| {
        shell.table_cell_editor.as_ref().unwrap().editor.clone()
    });
    cx.update(|_, app| {
        editor.update(app, |editor, _| {
            let block = editor.document().blocks().last().unwrap().clone();
            let end = block.content.as_text().unwrap().len();
            editor.set_selection_for_test(Selection::caret(DocPoint::new(block.id, end)));
        })
    });
    editor
}

fn paste_png(cx: &mut VisualTestContext) {
    let png = crate::native_editor::images::ClipboardPayload::fixture_with_png_and_text("")
        .images
        .into_iter()
        .next()
        .unwrap();
    cx.write_to_clipboard(gpui::ClipboardItem::new_image(&gpui::Image::from_bytes(
        gpui::ImageFormat::Png,
        png.bytes,
    )));
    cx.simulate_keystrokes("cmd-v");
}

fn cell_image_ids(editor: &crate::native_editor::core::EditorCore) -> Vec<String> {
    editor
        .document()
        .blocks()
        .iter()
        .filter_map(|block| match &block.content {
            BlockContent::Image { resource_id, .. } => Some(resource_id.clone()),
            _ => None,
        })
        .collect()
}

fn assert_cell_image_source_decodes(
    editor: &gpui::Entity<crate::native_editor::core::EditorCore>,
    resource_id: &str,
    cx: &mut VisualTestContext,
) {
    let (source, root) = editor.read_with(cx, |editor, _| {
        (
            editor
                .image_source_path(resource_id)
                .map(std::path::Path::to_path_buf),
            editor.image_materialization_root(),
        )
    });
    let source = source.expect("the open cell needs its own image source");
    assert!(
        source.is_file(),
        "the open cell's source must exist, not be an unavailable placeholder: {source:?}"
    );
    assert!(
        source.starts_with(root),
        "the source must belong to the open cell editor"
    );
    image::open(&source).expect("the open cell's persisted image source must decode");
}

fn main_cell_text(
    view: &gpui::Entity<LibraryShell>,
    cx: &mut VisualTestContext,
    row: usize,
    column: usize,
) -> Vec<app_lite_core::document::Inline> {
    view.read_with(cx, |shell, app| {
        let editor = shell
            .note_session
            .as_ref()
            .unwrap()
            .read(app)
            .editor()
            .read(app);
        editor
            .document()
            .blocks()
            .iter()
            .find_map(|block| match &block.content {
                BlockContent::Table(table) => table.cell_inlines(row, column).map(<[_]>::to_vec),
                _ => None,
            })
            .unwrap()
    })
}

// table/schema.ts 219: a cell holds tablecontent+, and image is tablecontent
// (resource/schema.ts 1001), so resource.ts insertResourceAtPosition puts a
// pasted image into the cell's own content at the caret.
#[gpui::test]
async fn reopened_cell_image_has_a_decodable_editor_owned_source(cx: &mut TestAppContext) {
    let (_root, repository, note, view, cx) = mount(cx);
    let editor = open_cell_at_end(&view, cx, 1, 1);
    paste_png(cx);
    cx.run_until_parked();
    let image = editor
        .read_with(cx, |editor, _| cell_image_ids(editor))
        .pop()
        .expect("inserted cell image");
    let body = saved_body(&view, cx, &repository, &note);
    assert!(body.contains(&format!("<img src=\":/{image}\"")));
    view.update(cx, |shell, shell_cx| {
        shell.commit_table_cell_editor(false, shell_cx)
    });
    cx.run_until_parked();
    let reopened_cell = open_cell_at_end(&view, cx, 1, 1);
    cx.run_until_parked();
    assert_ne!(editor.entity_id(), reopened_cell.entity_id());
    assert_cell_image_source_decodes(&reopened_cell, &image, cx);
}

#[gpui::test]
async fn image_pasted_into_a_cell_is_stored_and_shows_in_that_cell(cx: &mut TestAppContext) {
    let (root, repository, note, view, cx) = mount(cx);
    let editor = open_cell_at_end(&view, cx, 1, 1);
    paste_png(cx);
    cx.run_until_parked();
    cx.update(|window, app| window.draw(app).clear());
    assert!(view.read_with(cx, |shell, _| shell.queued_resource_inserts.is_empty()));
    let images = editor.read_with(cx, |editor, _| cell_image_ids(editor));
    assert_eq!(images.len(), 1, "the image is in the open cell");
    let image = images[0].clone();
    assert_cell_image_source_decodes(&editor, &image, cx);
    assert!(
        main_cell_text(&view, cx, 1, 1).iter().any(|inline| matches!(
            inline,
            app_lite_core::document::Inline::Image { resource_id, .. } if resource_id.as_str() == image
        )),
        "the table in the note holds it too"
    );
    cx.simulate_input("后");
    cx.run_until_parked();
    cx.simulate_keystrokes("tab");
    cx.run_until_parked();
    assert_eq!(open_cell(&view, cx), Some((2, 0)));
    let paints = (0..5).find_map(|_| {
        crate::native_editor::render::take_test_table_cell_paints();
        cx.update(|window, app| window.draw(app).clear());
        let paints = crate::native_editor::render::take_test_table_cell_paints();
        cx.run_until_parked();
        paints
            .iter()
            .rev()
            .find(|paint| paint.row == 1 && paint.column == 1)
            .filter(|paint| {
                paint
                    .images
                    .iter()
                    .any(|(id, _, loaded)| *id == image && *loaded)
            })
            .cloned()
    });
    assert!(paints.is_some(), "the table paints the image's pixels");

    let body = saved_body(&view, cx, &repository, &note);
    let expected = format!("<td>一<br><img src=\":/{image}\" alt=\"\"><br>后</td>");
    assert!(body.contains(&expected), "{body}");
    // Opening and closing the cell again changes nothing.
    let reopened_cell = open_cell_at_end(&view, cx, 1, 1);
    cx.run_until_parked();
    assert_cell_image_source_decodes(&reopened_cell, &image, cx);
    view.update(cx, |shell, shell_cx| {
        shell.commit_table_cell_editor(false, shell_cx)
    });
    assert_eq!(saved_body(&view, cx, &repository, &note), body);
    assert!(
        body.starts_with("<p>前</p><table"),
        "nothing lands in the body: {body}"
    );
    assert_eq!(body.matches("<img").count(), 1, "{body}");
    let id = app_lite_core::ResourceId::new(image.as_str()).unwrap();
    assert!(repository.resource_metadata(&id).unwrap().is_some());
    assert!(
        repository
            .open_verified_resource_file(&id)
            .unwrap()
            .is_some()
    );
    let reopened = LibraryRepository::open(root.path().join("library.sqlite")).unwrap();
    let saved = reopened.load_note(&note).unwrap().unwrap();
    assert_eq!(saved.body_html, body);
    assert!(crate::app::note_session::NoteSession::prepare(saved, &reopened).is_ok());
}

fn main_editor(
    view: &gpui::Entity<LibraryShell>,
    cx: &mut VisualTestContext,
) -> gpui::Entity<crate::native_editor::core::EditorCore> {
    view.read_with(cx, |shell, app| {
        shell
            .note_session
            .as_ref()
            .unwrap()
            .read(app)
            .editor()
            .clone()
    })
}

fn notice(view: &gpui::Entity<LibraryShell>, cx: &mut VisualTestContext) -> String {
    view.read_with(cx, |shell, _| shell.resource_notice.clone())
        .unwrap_or_default()
}

// The body's rule (NoteSession::finish_resource_commit): a failed commit
// takes the image out when nothing typed since depends on it.
#[gpui::test]
async fn failed_cell_image_commit_takes_the_image_out_of_cell_table_and_history(
    cx: &mut TestAppContext,
) {
    let (_root, repository, note, view, cx) = mount(cx);
    let editor = open_cell_at_end(&view, cx, 1, 1);
    let release = view.update(cx, |shell, shell_cx| {
        shell.stall_next_resource_commit_for_test(shell_cx)
    });
    view.update(cx, |shell, shell_cx| {
        shell.fail_next_resource_commit_for_test("单元格图片提交失败", shell_cx)
    });
    paste_png(cx);
    cx.run_until_parked();
    let image = editor
        .read_with(cx, |editor, _| cell_image_ids(editor))
        .pop()
        .expect("shown at once");
    release.send(()).unwrap();
    cx.run_until_parked();
    cx.update(|window, app| window.draw(app).clear());
    assert!(
        editor
            .read_with(cx, |editor, _| cell_image_ids(editor))
            .is_empty()
    );
    assert_eq!(
        main_cell_text(&view, cx, 1, 1),
        [app_lite_core::document::Inline::Text {
            text: "一".into(),
            marks: Default::default()
        }]
    );
    assert!(
        notice(&view, cx).contains("单元格图片提交失败"),
        "the failure stays visible: {}",
        notice(&view, cx)
    );
    cx.simulate_keystrokes("tab");
    cx.run_until_parked();
    // No undo step in the note brings back the image that was never stored.
    let main = main_editor(&view, cx);
    let referenced_on_the_way = |step: fn(&mut crate::native_editor::core::EditorCore) -> bool,
                                 cx: &mut VisualTestContext| {
        cx.update(|_, app| {
            main.update(app, |editor, _| {
                let mut referenced = Vec::new();
                while step(editor) {
                    referenced.push(editor.references_resource(&image));
                }
                referenced
            })
        })
    };
    let undo = referenced_on_the_way(|editor| editor.undo().is_ok(), cx);
    assert!(
        !undo.is_empty() && undo.iter().all(|seen| !seen),
        "{undo:?}"
    );
    let redo = referenced_on_the_way(|editor| editor.redo().is_ok(), cx);
    assert!(
        !redo.is_empty() && redo.iter().all(|seen| !seen),
        "{redo:?}"
    );
    let body = saved_body(&view, cx, &repository, &note);
    assert!(body.contains("<td>一</td>"), "{body}");
    assert!(!body.contains("<img"), "{body}");
}

// Typing in the paragraph the insert made cannot be replayed without it, so
// the image stays staged with a visible manual retry; cell and table agree,
// and the retry stores it and shows it in the cell.
#[gpui::test]
async fn failed_cell_image_commit_after_typing_keeps_both_and_retries(cx: &mut TestAppContext) {
    let (_root, repository, note, view, cx) = mount(cx);
    let editor = open_cell_at_end(&view, cx, 1, 1);
    let release = view.update(cx, |shell, shell_cx| {
        shell.stall_next_resource_commit_for_test(shell_cx)
    });
    view.update(cx, |shell, shell_cx| {
        shell.fail_next_resource_commit_for_test("单元格图片提交失败", shell_cx)
    });
    paste_png(cx);
    cx.run_until_parked();
    cx.simulate_input("后");
    cx.run_until_parked();
    release.send(()).unwrap();
    cx.run_until_parked();
    let image = editor
        .read_with(cx, |editor, _| cell_image_ids(editor))
        .pop()
        .expect("kept in the cell");
    assert!(
        notice(&view, cx).contains("手动同步可重试"),
        "{}",
        notice(&view, cx)
    );
    let in_table = main_cell_text(&view, cx, 1, 1);
    assert!(in_table.iter().any(|inline| matches!(inline, app_lite_core::document::Inline::Image { resource_id, .. } if resource_id.as_str() == image)));
    assert!(
        matches!(in_table.last(), Some(app_lite_core::document::Inline::Text { text, .. }) if text == "后")
    );
    let body = saved_body(&view, cx, &repository, &note);
    assert!(
        body.contains(&format!(
            "<td>一<br><img src=\":/{image}\" alt=\"\"><br>后</td>"
        )),
        "the retry stores it: {body}"
    );
    assert_cell_paints_image(&view, &editor, &image, cx);
}

/// The open cell's own surface painted the image's pixels: its private
/// decode cache (not the note's) holds the cell editor's source as loaded,
/// and the renderer recorded a paint_image of that image by this very cell
/// editor (the note's table paints its images on another path).
fn assert_cell_paints_image(
    view: &gpui::Entity<LibraryShell>,
    editor: &gpui::Entity<crate::native_editor::core::EditorCore>,
    resource_id: &str,
    cx: &mut VisualTestContext,
) {
    assert_cell_image_source_decodes(editor, resource_id, cx);
    let source = editor.read_with(cx, |editor, _| {
        editor
            .image_source_path(resource_id)
            .map(std::path::Path::to_path_buf)
            .expect("the cell's source")
    });
    let cache = view.read_with(cx, |shell, _| {
        let cell = shell.table_cell_editor.as_ref().expect("an open cell");
        assert_eq!(cell.editor.entity_id(), editor.entity_id(), "the open cell");
        cell.image_cache.clone()
    });
    let resource = gpui::Resource::from(source);
    let cell = editor.entity_id();
    crate::native_editor::render::take_test_image_paints();
    let painted = (0..5).any(|_| {
        draw(cx);
        let loaded = cache.read_with(cx, |cache, _| cache.loaded_success_for_test(&resource));
        let drawn = crate::native_editor::render::take_test_image_paints()
            .iter()
            .any(|paint| {
                paint.editor == cell
                    && paint.resource_id == resource_id
                    && paint.bounds.size.width > px(0.0)
                    && paint.bounds.size.height > px(0.0)
            });
        loaded && drawn
    });
    assert!(
        painted,
        "the cell's own editor must paint the image's pixels"
    );
}

#[gpui::test]
async fn cell_image_paints_in_the_cell_now_and_after_reopening(cx: &mut TestAppContext) {
    let (_root, repository, note, view, cx) = mount(cx);
    let editor = open_cell_at_end(&view, cx, 1, 1);
    paste_png(cx);
    draw(cx);
    let image = editor
        .read_with(cx, |editor, _| cell_image_ids(editor))
        .pop()
        .expect("inserted cell image");
    assert_cell_paints_image(&view, &editor, &image, cx);
    view.update(cx, |shell, shell_cx| {
        shell.commit_table_cell_editor(false, shell_cx)
    });
    saved_body(&view, cx, &repository, &note);
    let reopened = open_cell_at_end(&view, cx, 1, 1);
    assert_ne!(editor.entity_id(), reopened.entity_id());
    assert_cell_paints_image(&view, &reopened, &image, cx);
}

// While the cell's copy is in flight its placeholder keeps being painted;
// that must not ask for the image again on every frame.
#[gpui::test]
async fn cell_image_in_flight_does_not_repaint_in_a_loop(cx: &mut TestAppContext) {
    let (_root, repository, note, view, cx) = mount(cx);
    let editor = open_cell_at_end(&view, cx, 1, 1);
    paste_png(cx);
    draw(cx);
    let image = editor
        .read_with(cx, |editor, _| cell_image_ids(editor))
        .pop()
        .expect("inserted cell image");
    view.update(cx, |shell, shell_cx| {
        shell.commit_table_cell_editor(false, shell_cx)
    });
    saved_body(&view, cx, &repository, &note);
    let release = view.update(cx, |shell, shell_cx| {
        shell
            .note_session
            .as_ref()
            .unwrap()
            .update(shell_cx, |session, _| {
                session.stall_next_cell_image_hydration_for_test()
            })
    });
    let reopened = open_cell_at_end(&view, cx, 1, 1);
    for _ in 0..5 {
        draw(cx);
    }
    assert!(
        !has_cell_source(&reopened, &image, cx),
        "no source while the copy is held"
    );
    release.send(()).unwrap();
    draw(cx);
    assert_cell_paints_image(&view, &reopened, &image, cx);
}

#[gpui::test]
async fn cell_image_undo_redo_and_exact_selection(cx: &mut TestAppContext) {
    use crate::native_editor::model::{DocPoint, Selection};
    let table = "<table data-joplin-lite-table=\"true\"><tbody><tr><td>甲乙丙</td><td>一</td></tr></tbody></table>";
    let (_root, repository, note, view, cx) =
        mount_body(cx, Arc::new(ManualSaveClock::default()), table);
    let editor = open_cell_at_end(&view, cx, 0, 0);
    // The selected 乙 is replaced, as replaceSelectionWith does.
    cx.update(|_, app| {
        editor.update(app, |editor, _| {
            let id = editor.document().blocks()[0].id;
            editor.set_selection_for_test(Selection::new(
                DocPoint::new(id, "甲".len()),
                DocPoint::new(id, "甲乙".len()),
            ));
        })
    });
    paste_png(cx);
    cx.run_until_parked();
    cx.simulate_input("后");
    cx.run_until_parked();
    let image = editor
        .read_with(cx, |editor, _| cell_image_ids(editor))
        .pop()
        .unwrap();
    let inserted = format!("<td>甲<br><img src=\":/{image}\" alt=\"\"><br>后丙</td>");
    cx.simulate_keystrokes("cmd-z");
    cx.run_until_parked();
    cx.simulate_keystrokes("cmd-z");
    cx.run_until_parked();
    assert!(
        editor
            .read_with(cx, |editor, _| cell_image_ids(editor))
            .is_empty()
    );
    assert_eq!(
        main_cell_text(&view, cx, 0, 0),
        [app_lite_core::document::Inline::Text {
            text: "甲乙丙".into(),
            marks: Default::default()
        }]
    );
    cx.simulate_keystrokes("cmd-shift-z");
    cx.run_until_parked();
    cx.simulate_keystrokes("cmd-shift-z");
    cx.run_until_parked();
    view.update(cx, |shell, shell_cx| {
        shell.commit_table_cell_editor(false, shell_cx)
    });
    let body = saved_body(&view, cx, &repository, &note);
    assert!(body.contains(&inserted), "{body}");
}

#[gpui::test]
async fn cell_image_arriving_after_the_cell_closed_lands_nowhere(cx: &mut TestAppContext) {
    let (_root, repository, note, view, cx) = mount(cx);
    // Held here, so the closed cell's editor entity outlives the cell.
    let closed = open_cell_at_end(&view, cx, 1, 1);
    let release = view.update(cx, |shell, shell_cx| {
        shell.stall_next_resource_import_for_test(shell_cx)
    });
    paste_png(cx);
    cx.run_until_parked();
    cx.simulate_keystrokes("escape");
    cx.run_until_parked();
    assert_eq!(open_cell(&view, cx), None);
    // The same cell is open again, in a new editor, when the image is ready.
    let reopened = open_cell_at_end(&view, cx, 1, 1);
    release.send(()).unwrap();
    cx.run_until_parked();
    assert!(
        closed
            .read_with(cx, |editor, _| cell_image_ids(editor))
            .is_empty()
    );
    assert!(
        reopened
            .read_with(cx, |editor, _| cell_image_ids(editor))
            .is_empty()
    );
    assert!(view.read_with(cx, |shell, _| shell.resource_notice.is_some()));
    cx.simulate_keystrokes("escape");
    cx.run_until_parked();
    let body = saved_body(&view, cx, &repository, &note);
    assert!(!body.contains("<img"), "{body}");
    assert!(body.contains("<td>甲</td><td>一</td>"), "{body}");
}

#[gpui::test]
async fn cell_image_refuses_a_moved_cell_a_switched_note_and_open_composition(
    cx: &mut TestAppContext,
) {
    use gpui::EntityInputHandler;
    let (_root, repository, note, view, cx) = mount(cx);
    let (node, _) = table_node(&view, cx);
    // A row inserted above while the cell stays open: same coordinates,
    // another cell.
    let editor = open_cell_at_end(&view, cx, 1, 1);
    let release = view.update(cx, |shell, shell_cx| {
        shell.stall_next_resource_import_for_test(shell_cx)
    });
    paste_png(cx);
    cx.run_until_parked();
    let main = main_editor(&view, cx);
    cx.update(|_, app| main.update(app, |editor, _| editor.insert_table_row(node, 1).unwrap()));
    release.send(()).unwrap();
    cx.run_until_parked();
    assert!(
        editor
            .read_with(cx, |editor, _| cell_image_ids(editor))
            .is_empty()
    );
    assert!(
        notice(&view, cx).contains("单元格已移动或改变"),
        "{}",
        notice(&view, cx)
    );
    cx.update(|_, app| main.update(app, |editor, _| while editor.undo().is_ok() {}));
    cx.simulate_keystrokes("escape");
    cx.run_until_parked();

    // An input method composition open in the cell when the image is ready.
    let editor = open_cell_at_end(&view, cx, 1, 1);
    let release = view.update(cx, |shell, shell_cx| {
        shell.stall_next_resource_import_for_test(shell_cx)
    });
    paste_png(cx);
    cx.run_until_parked();
    cx.update(|window, app| {
        editor.update(app, |editor, editor_cx| {
            editor.replace_and_mark_text_in_range(None, "zhong", None, window, editor_cx);
        })
    });
    release.send(()).unwrap();
    cx.run_until_parked();
    assert!(
        editor
            .read_with(cx, |editor, _| cell_image_ids(editor))
            .is_empty()
    );
    assert!(
        notice(&view, cx).contains("输入法组合文本尚未确认"),
        "{}",
        notice(&view, cx)
    );
    cx.update(|window, app| {
        editor.update(app, |editor, editor_cx| {
            editor.replace_text_in_range(None, "", window, editor_cx);
        })
    });
    cx.simulate_keystrokes("escape");
    cx.run_until_parked();

    // Another note selected while the image is read: the switch waits for
    // the resource in flight (as for the body), which lands in the cell it
    // was pasted into, and the other note is untouched.
    let other = repository
        .create_note(CreateNote {
            title: "另一篇".into(),
            notebook_id: None,
            document: CanonicalDocument::parse_html("<p>别处</p>").unwrap(),
        })
        .unwrap();
    open_cell_at_end(&view, cx, 1, 1);
    let release = view.update(cx, |shell, shell_cx| {
        shell.stall_next_resource_import_for_test(shell_cx)
    });
    paste_png(cx);
    cx.run_until_parked();
    cx.update(|window, app| {
        view.update(app, |shell, shell_cx| {
            shell.apply_action(AppAction::SelectNote(other.id.clone()), window, shell_cx);
        })
    });
    cx.run_until_parked();
    let _ = release.send(());
    cx.run_until_parked();
    let other_body = saved_body(&view, cx, &repository, &other.id);
    assert!(!other_body.contains("<img"), "{other_body}");
    let body = repository.load_note(&note).unwrap().unwrap().body_html;
    let image = body
        .split(":/")
        .nth(1)
        .map(|rest| &rest[..32])
        .unwrap_or_default();
    assert!(
        body.contains(&format!(
            "<td>甲</td><td>一<br><img src=\":/{image}\" alt=\"\"><br></td>"
        )),
        "{body}"
    );
    assert_eq!(body.matches("<img").count(), 1, "{body}");
}

#[gpui::test]
async fn tiff_pasted_into_a_cell_is_stored_as_an_image(cx: &mut TestAppContext) {
    let (_root, repository, note, view, cx) = mount(cx);
    let editor = open_cell_at_end(&view, cx, 1, 0);
    let mut tiff = std::io::Cursor::new(Vec::new());
    image::RgbaImage::from_pixel(3, 2, image::Rgba([10, 20, 30, 255]))
        .write_to(&mut tiff, image::ImageFormat::Tiff)
        .unwrap();
    cx.write_to_clipboard(gpui::ClipboardItem::new_image(&gpui::Image::from_bytes(
        gpui::ImageFormat::Tiff,
        tiff.into_inner(),
    )));
    cx.simulate_keystrokes("cmd-v");
    cx.run_until_parked();
    let image = editor
        .read_with(cx, |editor, _| cell_image_ids(editor))
        .pop()
        .expect("a TIFF is an image");
    view.update(cx, |shell, shell_cx| {
        shell.commit_table_cell_editor(false, shell_cx)
    });
    let body = saved_body(&view, cx, &repository, &note);
    assert!(
        body.contains(&format!(
            "<td>甲<br><img src=\":/{image}\" alt=\"\"><br></td>"
        )),
        "{body}"
    );
}

#[gpui::test]
async fn fragment_image_from_this_library_pastes_into_a_cell(cx: &mut TestAppContext) {
    let (_root, repository, note, view, cx) = mount(cx);
    open_cell_at_end(&view, cx, 1, 1);
    paste_png(cx);
    cx.run_until_parked();
    cx.simulate_keystrokes("tab");
    cx.run_until_parked();
    let body = saved_body(&view, cx, &repository, &note);
    let image = body.split(":/").nth(1).unwrap()[..32].to_owned();
    let stored = repository
        .resource_metadata(&app_lite_core::ResourceId::new(image.as_str()).unwrap())
        .unwrap()
        .unwrap();
    let foreign = "c".repeat(32);
    let resource = |id: &str, sha256: &str| crate::native_editor::images::FragmentResource {
        id: id.to_owned(),
        sha256: sha256.to_owned(),
        title: stored.title.clone(),
        mime: stored.mime.clone(),
        file_extension: stored.file_extension.clone(),
        size: stored.size as u64,
        file: None,
    };
    let fragment = crate::native_editor::images::ClipboardFragment {
        version: crate::native_editor::images::CLIPBOARD_FRAGMENT_VERSION,
        html: format!(
            "<p>图<img src=\":/{image}\" alt=\"\"><img src=\":/{foreign}\" alt=\"\"></p>"
        ),
        plain: "图".into(),
        resources: vec![
            resource(&image, stored.sha256.as_str()),
            // From another library, without its file: only the id is known.
            resource(&foreign, &"d".repeat(64)),
        ],
        open_start: true,
        open_end: true,
    };
    cx.simulate_keystrokes("escape");
    cx.run_until_parked();
    let editor = open_cell_at_end(&view, cx, 1, 0);
    cx.write_to_clipboard(gpui::ClipboardItem::new_string_with_json_metadata(
        "图".into(),
        fragment,
    ));
    cx.simulate_keystrokes("cmd-v");
    cx.run_until_parked();
    assert_eq!(
        editor.read_with(cx, |editor, _| cell_image_ids(editor)),
        [image.clone()]
    );
    // A pasted image of this library shows in the cell from its own source.
    assert_cell_paints_image(&view, &editor, &image, cx);
    assert!(
        notice(&view, cx).contains("1 个图片或附件未粘贴"),
        "{}",
        notice(&view, cx)
    );
    cx.simulate_keystrokes("tab");
    cx.run_until_parked();
    let body = saved_body(&view, cx, &repository, &note);
    // An image is a block of the cell (p holds only inline content), so it
    // follows the pasted text on its own line.
    assert!(
        body.contains(&format!(
            "<td>甲图<br><img src=\":/{image}\" alt=\"\"></td>"
        )),
        "{body}"
    );
    assert!(!body.contains(&foreign), "{body}");
}

fn png_bytes() -> Vec<u8> {
    crate::native_editor::images::ClipboardPayload::fixture_with_png_and_text("")
        .images
        .into_iter()
        .next()
        .unwrap()
        .bytes
}

fn png_file(root: &tempfile::TempDir, name: &str) -> std::path::PathBuf {
    let path = root.path().join(name);
    std::fs::write(&path, png_bytes()).unwrap();
    path
}

fn draw(cx: &mut VisualTestContext) {
    cx.run_until_parked();
    cx.update(|window, app| window.draw(app).clear());
    cx.run_until_parked();
}

// The image button and menu open the chooser for whichever editor has the
// caret: with a cell focused, its caret is captured (resource.ts insert
// replaces that selection), never the body's older caret.
#[gpui::test]
async fn picker_from_a_focused_cell_inserts_into_that_cell(cx: &mut TestAppContext) {
    let (root, repository, note, view, cx) = mount(cx);
    let editor = open_cell_at_end(&view, cx, 1, 1);
    draw(cx);
    // Cancelled: nothing changes.
    let token = press_image_button(&view, cx);
    view.update(cx, |shell, shell_cx| {
        shell.cancel_resource_picker_for_test(token, shell_cx)
    });
    draw(cx);
    assert!(
        editor
            .read_with(cx, |editor, _| cell_image_ids(editor))
            .is_empty()
    );
    // Opened twice: only the latest chooser may complete.
    let stale = press_image_button(&view, cx);
    let token = press_image_button(&view, cx);
    let path = png_file(&root, "chosen.png");
    let stale_result = cx.update(|window, app| {
        view.update(app, |shell, shell_cx| {
            shell.complete_resource_picker_path_for_token_for_test(
                stale,
                path.clone(),
                window,
                shell_cx,
            )
        })
    });
    assert!(stale_result.is_err());
    cx.update(|window, app| {
        view.update(app, |shell, shell_cx| {
            shell.complete_resource_picker_path_for_token_for_test(
                token,
                path.clone(),
                window,
                shell_cx,
            )
        })
    })
    .unwrap();
    draw(cx);
    let image = editor
        .read_with(cx, |editor, _| cell_image_ids(editor))
        .pop()
        .expect("in the cell");
    view.update(cx, |shell, shell_cx| {
        shell.commit_table_cell_editor(false, shell_cx)
    });
    let body = saved_body(&view, cx, &repository, &note);
    assert!(
        body.contains(&format!(
            "<td>一<br><img src=\":/{image}\" alt=\"\"><br></td>"
        )),
        "{body}"
    );
    assert!(body.starts_with("<p>前</p><table"), "{body}");
    assert_eq!(body.matches("<img").count(), 1, "{body}");
}

#[gpui::test]
async fn picker_completing_after_its_cell_closed_is_refused(cx: &mut TestAppContext) {
    let (root, repository, note, view, cx) = mount(cx);
    open_cell_at_end(&view, cx, 1, 1);
    draw(cx);
    let token = press_image_button(&view, cx);
    cx.simulate_keystrokes("escape");
    draw(cx);
    let path = png_file(&root, "late.png");
    let _ = cx.update(|window, app| {
        view.update(app, |shell, shell_cx| {
            shell.complete_resource_picker_path_for_token_for_test(token, path, window, shell_cx)
        })
    });
    draw(cx);
    assert!(!notice(&view, cx).is_empty(), "refused visibly");
    let body = saved_body(&view, cx, &repository, &note);
    assert!(
        !body.contains("<img"),
        "neither the cell nor the body: {body}"
    );
}

// Identical (empty) cells: equal content at the same coordinates is not the
// same cell once a row was added or removed above it.
#[gpui::test]
async fn cell_insert_refuses_a_structural_change_between_identical_cells(cx: &mut TestAppContext) {
    let table = "<table data-joplin-lite-table=\"true\"><tbody><tr><th>名</th><th>值</th></tr><tr><td></td><td></td></tr><tr><td></td><td></td></tr><tr><td></td><td></td></tr></tbody></table>";
    let (_root, repository, note, view, cx) =
        mount_body(cx, Arc::new(ManualSaveClock::default()), table);
    let (node, _) = table_node(&view, cx);
    let main = main_editor(&view, cx);
    for change in ["insert above", "delete above"] {
        let editor = open_cell_at_end(&view, cx, 2, 1);
        let release = view.update(cx, |shell, shell_cx| {
            shell.stall_next_resource_import_for_test(shell_cx)
        });
        paste_png(cx);
        cx.run_until_parked();
        cx.update(|_, app| {
            main.update(app, |editor, _| match change {
                "insert above" => editor.insert_table_row(node, 1).unwrap(),
                _ => editor.delete_table_row(node, 1).unwrap(),
            })
        });
        release.send(()).unwrap();
        draw(cx);
        assert!(
            editor
                .read_with(cx, |editor, _| cell_image_ids(editor))
                .is_empty(),
            "{change}"
        );
        assert!(
            notice(&view, cx).contains("单元格已移动或改变"),
            "{change}: {}",
            notice(&view, cx)
        );
        assert!(!main.read_with(cx, |editor, _| editor.document().blocks().iter().any(|block| matches!(&block.content, BlockContent::Table(table) if crate::native_editor::table_layout::table_image_ids(table).next().is_some()))), "{change}");
        cx.simulate_keystrokes("escape");
        cx.run_until_parked();
    }
    // Typing in the cell meanwhile is that cell's own change: still inserted.
    let editor = open_cell_at_end(&view, cx, 2, 1);
    let release = view.update(cx, |shell, shell_cx| {
        shell.stall_next_resource_import_for_test(shell_cx)
    });
    paste_png(cx);
    cx.run_until_parked();
    cx.simulate_input("字");
    cx.run_until_parked();
    release.send(()).unwrap();
    draw(cx);
    assert_eq!(
        editor
            .read_with(cx, |editor, _| cell_image_ids(editor))
            .len(),
        1
    );
    view.update(cx, |shell, shell_cx| {
        shell.commit_table_cell_editor(false, shell_cx)
    });
    let body = saved_body(&view, cx, &repository, &note);
    assert_eq!(body.matches("<img").count(), 1, "{body}");
}

// Evernote dragdrop/plugin.ts takes the drop coordinate (posAtCoords): in
// an open cell it is that cell's point; on a table whose cell is not open
// there is no editable cell point here, so the drop is refused visibly.
/// The toolbar's image button, as clicked: it notes what has the focus,
/// hands the focus back to the note's editor, and the shell then routes the
/// chooser (`EditorCommandChromeEvent::RequestInsertImage`).
fn press_image_button(
    view: &gpui::Entity<LibraryShell>,
    cx: &mut VisualTestContext,
) -> super::ResourcePickerToken {
    let chrome = view.read_with(cx, |shell, _| {
        shell.command_chrome.clone().expect("toolbar")
    });
    cx.update(|window, app| {
        chrome.update(app, |chrome, chrome_cx| {
            chrome.execute_command_for_test(
                crate::native_editor::commands::EditorCommand::InsertImage,
                window,
                chrome_cx,
            )
        })
    });
    cx.run_until_parked();
    view.read_with(cx, |shell, _| {
        shell
            .pending_resource_insert
            .as_ref()
            .map(|pending| pending.token)
    })
    .unwrap_or_else(|| panic!("no chooser pending: {}", notice(view, cx)))
}

fn complete_picker(
    view: &gpui::Entity<LibraryShell>,
    cx: &mut VisualTestContext,
    token: super::ResourcePickerToken,
    path: std::path::PathBuf,
) {
    cx.update(|window, app| {
        view.update(app, |shell, shell_cx| {
            shell.complete_resource_picker_path_for_token_for_test(token, path, window, shell_cx)
        })
    })
    .unwrap();
    draw(cx);
}

const TWO_LINES: &str = "<table data-joplin-lite-table=\"true\"><tbody><tr><td>甲乙丙<br>丁戊</td><td>一</td></tr></tbody></table>";

// Source offsets of the cell 甲乙丙<br>丁戊 (three bytes a character, one
// for the break) and where resource.ts insertResourceAtPosition puts the
// image: before the text at its start, splitting it elsewhere.
fn offset_cases(image: &str) -> [(usize, String); 5] {
    let img = format!("<img src=\":/{image}\" alt=\"\">");
    [
        (0, format!("{img}<br>甲乙丙<br>丁戊")),
        (3, format!("甲<br>{img}<br>乙丙<br>丁戊")),
        (9, format!("甲乙丙<br>{img}<br><br>丁戊")),
        (13, format!("甲乙丙<br>丁<br>{img}<br>戊")),
        (16, format!("甲乙丙<br>丁戊<br>{img}<br>")),
    ]
}

/// A point inside a glyph rather than on a boundary: right of a line's
/// start, past its end, else in the right half of the glyph before (GPUI's
/// closest-index rounds the left half of a line's last glyph to the line
/// end, so a boundary exactly there is ambiguous).
fn inside_glyph(position: gpui::Point<gpui::Pixels>, offset: usize) -> gpui::Point<gpui::Pixels> {
    let dx = match offset {
        0 | 10 => 1.0,
        9 | 16 => 3.0,
        _ => -2.0,
    };
    gpui::point(position.x + px(dx), position.y)
}

fn first_cell_html(view: &gpui::Entity<LibraryShell>, cx: &mut VisualTestContext) -> String {
    let inlines = main_cell_text(view, cx, 0, 0);
    let document = CanonicalDocument::from_blocks(vec![app_lite_core::document::Block::Table {
        rows: vec![app_lite_core::document::TableRow {
            cells: vec![app_lite_core::document::TableCell { inlines }],
        }],
        header: false,
    }]);
    let html = document.to_canonical_html().as_str().to_owned();
    let start = html.find("<td>").unwrap() + 4;
    html[start..html.rfind("</td>").unwrap()].to_owned()
}

// A click in the open cell puts the caret at the glyph boundary under it,
// and a drop there inserts at that same point (dragdrop/plugin.ts:
// posAtCoords of the drop event).
#[gpui::test]
async fn click_and_drop_in_the_open_cell_land_at_the_point_under_the_pointer(
    cx: &mut TestAppContext,
) {
    use crate::native_editor::model::DocPoint;
    for (offset, _) in offset_cases("") {
        let (root, _repository, _note, view, cx) =
            mount_body(cx, Arc::new(ManualSaveClock::default()), TWO_LINES);
        let editor = open_cell_at_end(&view, cx, 0, 0);
        draw(cx);
        let block = editor.read_with(cx, |editor, _| editor.document().blocks()[0].id);
        let position = editor
            .read_with(cx, |editor, _| editor.layout().caret_bounds(block, offset))
            .expect("caret geometry")
            .center();
        let position = inside_glyph(position, offset);
        cx.simulate_click(position, gpui::Modifiers::default());
        cx.run_until_parked();
        assert_eq!(
            editor.read_with(cx, |editor, _| editor.selection().head),
            DocPoint::new(block, offset),
            "click at {offset}"
        );
        view.update(cx, |shell, shell_cx| {
            shell.record_drop_position(position, shell_cx)
        });
        let intent = view
            .update(cx, |shell, shell_cx| shell.take_drop_intent(shell_cx))
            .unwrap();
        let path = png_file(&root, "dropped.png");
        cx.update(|window, app| {
            view.update(app, |shell, shell_cx| {
                shell.complete_resource_drop_paths(vec![path], intent, window, shell_cx)
            })
        })
        .unwrap();
        draw(cx);
        let image = editor
            .read_with(cx, |editor, _| cell_image_ids(editor))
            .pop()
            .expect("dropped");
        let expected = offset_cases(&image)
            .into_iter()
            .find(|(at, _)| *at == offset)
            .unwrap()
            .1;
        assert_eq!(first_cell_html(&view, cx), expected, "drop at {offset}");
    }
}

// The same drops onto the table with the cell not open: the cell opens with
// the caret at the dropped point and takes the image there.
#[gpui::test]
async fn drop_on_an_unopened_cell_opens_it_and_inserts_at_the_point(cx: &mut TestAppContext) {
    for (offset, _) in offset_cases("") {
        let (root, _repository, _note, view, cx) =
            mount_body(cx, Arc::new(ManualSaveClock::default()), TWO_LINES);
        draw(cx);
        let (node, _) = table_node(&view, cx);
        let position = main_editor(&view, cx)
            .read_with(cx, |editor, _| {
                editor.layout().table_source_position(node, 0, 0, offset)
            })
            .expect("glyph geometry");
        let position = inside_glyph(position, offset);
        view.update(cx, |shell, shell_cx| {
            shell.record_drop_position(position, shell_cx)
        });
        assert_eq!(open_cell(&view, cx), None, "a drag does not open cells");
        let intent = view
            .update(cx, |shell, shell_cx| shell.take_drop_intent(shell_cx))
            .unwrap();
        assert_eq!(open_cell(&view, cx), Some((0, 0)));
        let path = png_file(&root, "dropped.png");
        cx.update(|window, app| {
            view.update(app, |shell, shell_cx| {
                shell.complete_resource_drop_paths(vec![path], intent, window, shell_cx)
            })
        })
        .unwrap();
        draw(cx);
        let editor = view.read_with(cx, |shell, _| {
            shell.table_cell_editor.as_ref().unwrap().editor.clone()
        });
        let image = editor
            .read_with(cx, |editor, _| cell_image_ids(editor))
            .pop()
            .expect("dropped");
        let expected = offset_cases(&image)
            .into_iter()
            .find(|(at, _)| *at == offset)
            .unwrap()
            .1;
        assert_eq!(first_cell_html(&view, cx), expected, "drop at {offset}");
    }
}

// Over an image in a cell, the upper half is before it, the lower after.
#[gpui::test]
async fn drop_on_a_cell_image_goes_before_or_after_it_by_half(cx: &mut TestAppContext) {
    for before in [true, false] {
        let (root, repository, note, view, cx) = mount(cx);
        let editor = open_cell_at_end(&view, cx, 1, 1);
        paste_png(cx);
        cx.run_until_parked();
        let first = editor
            .read_with(cx, |editor, _| cell_image_ids(editor))
            .pop()
            .unwrap();
        view.update(cx, |shell, shell_cx| {
            shell.commit_table_cell_editor(false, shell_cx)
        });
        saved_body(&view, cx, &repository, &note);
        let image_bounds = (0..5)
            .find_map(|_| {
                crate::native_editor::render::take_test_table_cell_paints();
                cx.update(|window, app| window.draw(app).clear());
                let paints = crate::native_editor::render::take_test_table_cell_paints();
                cx.run_until_parked();
                paints
                    .iter()
                    .rev()
                    .find(|paint| paint.row == 1 && paint.column == 1)
                    .and_then(|paint| paint.images.first().map(|(_, bounds, _)| *bounds))
            })
            .expect("image painted");
        let y = if before {
            image_bounds.top() + image_bounds.size.height / 4.0
        } else {
            image_bounds.bottom() - image_bounds.size.height / 4.0
        };
        let position = gpui::point(image_bounds.center().x, y);
        view.update(cx, |shell, shell_cx| {
            shell.record_drop_position(position, shell_cx)
        });
        let intent = view
            .update(cx, |shell, shell_cx| shell.take_drop_intent(shell_cx))
            .unwrap();
        let path = png_file(&root, "second.png");
        std::fs::write(&path, {
            let mut bytes = std::io::Cursor::new(Vec::new());
            image::RgbaImage::from_pixel(5, 3, image::Rgba([200, 10, 10, 255]))
                .write_to(&mut bytes, image::ImageFormat::Png)
                .unwrap();
            bytes.into_inner()
        })
        .unwrap();
        cx.update(|window, app| {
            view.update(app, |shell, shell_cx| {
                shell.complete_resource_drop_paths(vec![path], intent, window, shell_cx)
            })
        })
        .unwrap();
        draw(cx);
        let ids = view
            .read_with(cx, |shell, app| {
                shell
                    .table_cell_editor
                    .as_ref()
                    .map(|cell| cell_image_ids(cell.editor.read(app)))
            })
            .expect("the cell opened");
        let second = ids
            .iter()
            .find(|id| **id != first)
            .expect("second image")
            .clone();
        let order = if before {
            [second, first]
        } else {
            [first, second]
        };
        assert_eq!(ids, order, "before={before}");
    }
}

// The button routes by what has the focus when it is pressed, not by the
// last paint: focus moved without a frame between still counts.
#[gpui::test]
async fn image_button_follows_the_focus_at_the_press_not_the_last_paint(cx: &mut TestAppContext) {
    let (root, repository, note, view, cx) = mount(cx);
    let cell = open_cell_at_end(&view, cx, 1, 1);
    draw(cx);
    let main = main_editor(&view, cx);
    // The body takes the focus; no frame is drawn before the press.
    cx.update(|window, app| main.read(app).focus_handle().focus(window));
    let token = press_image_button(&view, cx);
    complete_picker(&view, cx, token, png_file(&root, "body.png"));
    assert!(
        cell.read_with(cx, |editor, _| cell_image_ids(editor))
            .is_empty()
    );
    assert_eq!(
        main.read_with(cx, |editor, _| cell_image_ids(editor)).len(),
        1,
        "{}",
        notice(&view, cx)
    );
    // And back to the cell, again without a frame.
    cx.update(|window, app| cell.read(app).focus_handle().focus(window));
    let token = press_image_button(&view, cx);
    complete_picker(&view, cx, token, png_file(&root, "cell.png"));
    assert_eq!(
        cell.read_with(cx, |editor, _| cell_image_ids(editor)).len(),
        1,
        "{}",
        notice(&view, cx)
    );
    view.update(cx, |shell, shell_cx| {
        shell.commit_table_cell_editor(false, shell_cx)
    });
    let body = saved_body(&view, cx, &repository, &note);
    assert_eq!(body.matches("<img").count(), 2, "{body}");
}

#[gpui::test]
async fn encoded_image_and_foreign_fragment_with_its_file_paste_into_a_cell(
    cx: &mut TestAppContext,
) {
    use base64::Engine;
    use sha2::{Digest, Sha256};
    let (root, repository, note, view, cx) = mount(cx);
    let editor = open_cell_at_end(&view, cx, 1, 0);
    let html = format!(
        "<img src=\"data:image/png;base64,{}\">",
        base64::engine::general_purpose::STANDARD.encode(png_bytes())
    );
    let intent = crate::native_editor::images::classify_clipboard(
        crate::native_editor::images::ClipboardPayload {
            html: Some(html),
            ..Default::default()
        },
    );
    assert!(matches!(
        intent,
        crate::native_editor::images::PasteIntent::EncodedImage { .. }
    ));
    cx.update(|window, app| {
        view.update(app, |shell, shell_cx| {
            shell.paste_into_table_cell(intent, window, shell_cx)
        })
    });
    draw(cx);
    assert_eq!(
        editor
            .read_with(cx, |editor, _| cell_image_ids(editor))
            .len(),
        1,
        "{}",
        notice(&view, cx)
    );
    view.update(cx, |shell, shell_cx| {
        shell.commit_table_cell_editor(false, shell_cx)
    });
    draw(cx);

    // From another library, with the file its copy exported.
    let bytes = png_bytes();
    let foreign = "e".repeat(32);
    let fragment = crate::native_editor::images::ClipboardFragment {
        version: crate::native_editor::images::CLIPBOARD_FRAGMENT_VERSION,
        html: format!("<p><img src=\":/{foreign}\" alt=\"\"></p>"),
        plain: String::new(),
        resources: vec![crate::native_editor::images::FragmentResource {
            id: foreign.clone(),
            sha256: format!("{:x}", Sha256::digest(&bytes)),
            title: "外库.png".into(),
            mime: "image/png".into(),
            file_extension: "png".into(),
            size: bytes.len() as u64,
            file: Some(png_file(&root, "exported.png")),
        }],
        open_start: true,
        open_end: true,
    };
    let editor = open_cell_at_end(&view, cx, 1, 1);
    cx.write_to_clipboard(gpui::ClipboardItem::new_string_with_json_metadata(
        String::new(),
        fragment,
    ));
    cx.simulate_keystrokes("cmd-v");
    draw(cx);
    let imported = editor
        .read_with(cx, |editor, _| cell_image_ids(editor))
        .pop()
        .expect("imported");
    assert_ne!(imported, foreign, "imported under an id of this library");
    view.update(cx, |shell, shell_cx| {
        shell.commit_table_cell_editor(false, shell_cx)
    });
    let body = saved_body(&view, cx, &repository, &note);
    assert!(!body.contains(&foreign), "{body}");
    assert_eq!(body.matches("<img").count(), 2, "{body}");
    assert!(
        repository
            .resource_metadata(&app_lite_core::ResourceId::new(imported.as_str()).unwrap())
            .unwrap()
            .is_some()
    );
}

// Closed with "完成" while the commit runs: the image is then in the table
// only, so a failed commit keeps it staged for the manual retry.
#[gpui::test]
async fn failed_commit_after_the_cell_closed_keeps_the_table_image_for_retry(
    cx: &mut TestAppContext,
) {
    let (_root, repository, note, view, cx) = mount(cx);
    open_cell_at_end(&view, cx, 1, 1);
    let release = view.update(cx, |shell, shell_cx| {
        shell.stall_next_resource_commit_for_test(shell_cx)
    });
    view.update(cx, |shell, shell_cx| {
        shell.fail_next_resource_commit_for_test("关闭后提交失败", shell_cx)
    });
    paste_png(cx);
    cx.run_until_parked();
    view.update(cx, |shell, shell_cx| {
        shell.commit_table_cell_editor(false, shell_cx)
    });
    assert_eq!(open_cell(&view, cx), None);
    release.send(()).unwrap();
    draw(cx);
    assert!(
        notice(&view, cx).contains("手动同步可重试"),
        "{}",
        notice(&view, cx)
    );
    let in_table = main_cell_text(&view, cx, 1, 1);
    let image = in_table
        .iter()
        .find_map(|inline| match inline {
            app_lite_core::document::Inline::Image { resource_id, .. } => {
                Some(resource_id.as_str().to_owned())
            }
            _ => None,
        })
        .expect("still in the table");
    let body = saved_body(&view, cx, &repository, &note);
    assert!(
        body.contains(&format!("<img src=\":/{image}\"")),
        "the retry stores it: {body}"
    );
    assert!(
        repository
            .resource_metadata(&app_lite_core::ResourceId::new(image.as_str()).unwrap())
            .unwrap()
            .is_some()
    );
}

#[gpui::test]
async fn cell_text_is_not_written_into_a_cell_that_moved_under_it(cx: &mut TestAppContext) {
    let table = "<table data-joplin-lite-table=\"true\"><tbody><tr><th>名</th><th>值</th></tr><tr><td></td><td></td></tr><tr><td></td><td></td></tr></tbody></table>";
    let (_root, repository, note, view, cx) =
        mount_body(cx, Arc::new(ManualSaveClock::default()), table);
    let (node, _) = table_node(&view, cx);
    let main = main_editor(&view, cx);
    open_cell_at_end(&view, cx, 2, 1);
    cx.update(|_, app| main.update(app, |editor, _| editor.insert_table_row(node, 1).unwrap()));
    cx.simulate_input("字");
    cx.run_until_parked();
    assert!(
        view.read_with(cx, |shell, _| shell
            .table_cell_editor
            .as_ref()
            .unwrap()
            .error
            .clone())
            .is_some_and(|error| error.contains("表格已在别处改变")),
        "the cell says why its text was not written"
    );
    assert!(!main.read_with(cx, |editor, _| editor.document().blocks().iter().any(|block| matches!(&block.content, BlockContent::Table(table) if table.rows.iter().flatten().any(|text| text.contains("字"))))));
    cx.simulate_keystrokes("escape");
    cx.run_until_parked();
    let body = saved_body(&view, cx, &repository, &note);
    assert!(!body.contains("字"), "{body}");
}

// Rows added between the drag over a cell and the drop: the measured point
// may name another, identical cell now, so the drop is refused.
#[gpui::test]
async fn drop_on_a_cell_whose_table_changed_since_the_drag_is_refused(cx: &mut TestAppContext) {
    let table = "<table data-joplin-lite-table=\"true\"><tbody><tr><td>同</td><td>同</td></tr><tr><td>同</td><td>同</td></tr></tbody></table>";
    let (_root, repository, note, view, cx) =
        mount_body(cx, Arc::new(ManualSaveClock::default()), table);
    draw(cx);
    let (node, _) = table_node(&view, cx);
    let main = main_editor(&view, cx);
    let position = main
        .read_with(cx, |editor, _| {
            editor.layout().table_source_position(node, 1, 1, 0)
        })
        .expect("cell geometry");
    let position = inside_glyph(position, 0);
    view.update(cx, |shell, shell_cx| {
        shell.record_drop_position(position, shell_cx)
    });
    cx.update(|_, app| main.update(app, |editor, _| editor.insert_table_row(node, 0).unwrap()));
    let refused = view.update(cx, |shell, shell_cx| shell.take_drop_intent(shell_cx));
    assert!(refused.is_err_and(|error| error.contains("表格在拖放过程中已改变")));
    assert_eq!(
        open_cell(&view, cx),
        None,
        "no cell opened for a stale point"
    );
    let body = saved_body(&view, cx, &repository, &note);
    assert!(!body.contains("<img"), "{body}");
}

/// Inserts one image into cell (1,1), saves and closes the cell.
fn stored_cell_image(
    view: &gpui::Entity<LibraryShell>,
    cx: &mut VisualTestContext,
    repository: &LibraryRepository,
    note: &app_lite_core::NoteId,
) -> String {
    let editor = open_cell_at_end(view, cx, 1, 1);
    paste_png(cx);
    draw(cx);
    let image = editor
        .read_with(cx, |editor, _| cell_image_ids(editor))
        .pop()
        .expect("inserted cell image");
    view.update(cx, |shell, shell_cx| {
        shell.commit_table_cell_editor(false, shell_cx)
    });
    saved_body(view, cx, repository, note);
    image
}

fn stall_cell_image_hydration(
    view: &gpui::Entity<LibraryShell>,
    cx: &mut VisualTestContext,
) -> futures::channel::oneshot::Sender<()> {
    view.update(cx, |shell, shell_cx| {
        shell
            .note_session
            .as_ref()
            .unwrap()
            .update(shell_cx, |session, _| {
                session.stall_next_cell_image_hydration_for_test()
            })
    })
}

fn has_cell_source(
    editor: &gpui::Entity<crate::native_editor::core::EditorCore>,
    image: &str,
    cx: &mut VisualTestContext,
) -> bool {
    editor
        .read_with(cx, |editor, _| {
            editor
                .image_source_path(image)
                .map(std::path::Path::to_path_buf)
        })
        .is_some_and(|source| source.is_file())
}

// A durable image's copy is still in flight when its cell closes, another
// cell opens, or the note is switched: the late result goes nowhere.
#[gpui::test]
async fn delayed_cell_image_hydration_lands_nowhere_after_close_or_switch(cx: &mut TestAppContext) {
    let (_root, repository, note, view, cx) = mount(cx);
    let image = stored_cell_image(&view, cx, &repository, &note);

    // Closed, with another cell open, when the copy lands.
    let release = stall_cell_image_hydration(&view, cx);
    let closed = open_cell_at_end(&view, cx, 1, 1);
    cx.simulate_keystrokes("escape");
    cx.run_until_parked();
    assert_eq!(open_cell(&view, cx), None);
    let other = open_cell_at_end(&view, cx, 1, 0);
    release.send(()).unwrap();
    draw(cx);
    assert!(
        !has_cell_source(&closed, &image, cx),
        "not into the closed cell"
    );
    assert!(
        !has_cell_source(&other, &image, cx),
        "not into another cell"
    );
    cx.simulate_keystrokes("escape");
    cx.run_until_parked();

    // Switched to another note while the copy is held.
    let other_note = repository
        .create_note(CreateNote {
            title: "另一篇".into(),
            notebook_id: None,
            document: CanonicalDocument::parse_html("<p>别处</p>").unwrap(),
        })
        .unwrap();
    let release = stall_cell_image_hydration(&view, cx);
    let left = open_cell_at_end(&view, cx, 1, 1);
    cx.update(|window, app| {
        view.update(app, |shell, shell_cx| {
            shell.apply_action(
                AppAction::SelectNote(other_note.id.clone()),
                window,
                shell_cx,
            );
        })
    });
    cx.run_until_parked();
    let _ = release.send(());
    draw(cx);
    assert!(
        !has_cell_source(&left, &image, cx),
        "not into the left note's cell"
    );

    // Back to the note: the same cell still shows its image.
    cx.update(|window, app| {
        view.update(app, |shell, shell_cx| {
            shell.apply_action(AppAction::SelectNote(note.clone()), window, shell_cx);
        })
    });
    draw(cx);
    let reopened = open_cell_at_end(&view, cx, 1, 1);
    assert_cell_paints_image(&view, &reopened, &image, cx);
}

// The stored bytes are missing when a cell opens: the failure is visible
// and the cell keeps its placeholder; once the bytes are back, reopening the
// cell shows the image.
#[gpui::test]
async fn cell_image_load_failure_is_visible_and_recovers_on_reopening(cx: &mut TestAppContext) {
    let (root, repository, note, view, cx) = mount(cx);
    let image = stored_cell_image(&view, cx, &repository, &note);
    let sha256 = repository
        .resource_metadata(&app_lite_core::ResourceId::new(image.as_str()).unwrap())
        .unwrap()
        .unwrap()
        .sha256;
    let blob = root.path().join("resources/blobs").join(sha256.as_str());
    let aside = root.path().join("blob-aside");
    std::fs::rename(&blob, &aside).expect("the stored blob");

    let failed = open_cell_at_end(&view, cx, 1, 1);
    for _ in 0..3 {
        draw(cx);
    }
    assert!(!has_cell_source(&failed, &image, cx));
    assert!(
        notice(&view, cx).contains("单元格图片") && notice(&view, cx).contains("暂不可用"),
        "the failure is shown: {}",
        notice(&view, cx)
    );
    cx.simulate_keystrokes("escape");
    cx.run_until_parked();

    std::fs::rename(&aside, &blob).unwrap();
    let recovered = open_cell_at_end(&view, cx, 1, 1);
    assert_cell_paints_image(&view, &recovered, &image, cx);
}

// The cell paints its placeholder while the resource is still being stored;
// when the commit lands the cell takes the image without being reopened.
#[gpui::test]
async fn cell_image_painted_during_its_commit_shows_once_stored(cx: &mut TestAppContext) {
    let (_root, _repository, _note, view, cx) = mount(cx);
    let editor = open_cell_at_end(&view, cx, 1, 1);
    let release = view.update(cx, |shell, shell_cx| {
        shell.stall_next_resource_commit_for_test(shell_cx)
    });
    paste_png(cx);
    for _ in 0..3 {
        draw(cx);
    }
    let image = editor
        .read_with(cx, |editor, _| cell_image_ids(editor))
        .pop()
        .expect("inserted cell image");
    assert!(!has_cell_source(&editor, &image, cx), "not stored yet");
    release.send(()).unwrap();
    draw(cx);
    assert_cell_paints_image(&view, &editor, &image, cx);
}

// The worker's copy cannot be moved into the cell's own directory: the
// failure is shown without a repaint loop, and reopening the cell (a fresh
// store) shows the image.
#[gpui::test]
async fn cell_image_adoption_failure_is_visible_and_retryable(cx: &mut TestAppContext) {
    let (_root, repository, note, view, cx) = mount(cx);
    let image = stored_cell_image(&view, cx, &repository, &note);
    let release = stall_cell_image_hydration(&view, cx);
    let cell = open_cell_at_end(&view, cx, 1, 1);
    draw(cx);
    let cell_root = cell.read_with(cx, |editor, _| editor.image_materialization_root());
    let _ = std::fs::remove_dir_all(&cell_root);
    std::fs::write(&cell_root, b"not a directory").unwrap();
    release.send(()).unwrap();
    for _ in 0..3 {
        draw(cx);
    }
    assert!(!has_cell_source(&cell, &image, cx));
    assert!(
        notice(&view, cx).contains("无法放入单元格缓存"),
        "the failure is shown: {}",
        notice(&view, cx)
    );

    cx.simulate_keystrokes("escape");
    cx.run_until_parked();
    let reopened = open_cell_at_end(&view, cx, 1, 1);
    assert_cell_paints_image(&view, &reopened, &image, cx);
    std::fs::remove_file(&cell_root).unwrap();
}

// Native acceptance reproduced Done selecting the note card underneath the
// floating cell editor. Calling commit_table_cell_editor directly misses the
// actual mouse propagation path: exercise the visible control over another row.
#[gpui::test]
async fn cell_done_mouse_click_does_not_select_the_underlying_note(cx: &mut TestAppContext) {
    let (_root, repository, note, view, cx) = mount(cx);
    cx.simulate_resize(gpui::size(px(1160.0), px(789.0)));
    draw(cx);
    for _ in 0..7 {
        cx.update(|window, app| view.update(app, |shell, shell_cx| {
            shell.apply_action(AppAction::CreateNote, window, shell_cx);
        }));
        draw(cx);
    }
    cx.update(|window, app| view.update(app, |shell, shell_cx| {
        shell.apply_action(AppAction::SelectNote(note.clone()), window, shell_cx);
    }));
    draw(cx);
    let session_id = view.read_with(cx, |shell, _| {
        shell.note_session.as_ref().unwrap().entity_id()
    });
    let _cell = open_cell_at_end(&view, cx, 1, 1);
    cx.simulate_input("完成按钮验收");
    draw(cx);
    assert!(saved_body(&view, cx, &repository, &note).contains("完成按钮验收"));
    draw(cx);
    let popup = cx.debug_bounds("table-cell-editor").expect("visible cell popup");
    let cell_bounds = view.read_with(cx, |shell, _| {
        shell.table_cell_editor.as_ref().unwrap().bounds.get().unwrap()
    });
    // Done is the first control immediately below the editing surface. Its
    // real geometry is derived from that surface, not a direct handler call.
    let position = point(popup.left() + px(35.0), cell_bounds.bottom() + px(18.0));
    assert!(popup.contains(&position), "click must land inside the popup");
    assert!([
        "library-note-card-0", "library-note-card-1", "library-note-card-2",
        "library-note-card-3", "library-note-card-4", "library-note-card-5",
        "library-note-card-6", "library-note-card-7",
    ].into_iter().any(|selector| {
        cx.debug_bounds(selector)
            .is_some_and(|bounds| bounds.contains(&position))
    }), "fixture must put a different note card underneath Done: {position:?}");
    cx.simulate_event(gpui::MouseMoveEvent {
        position, pressed_button: None, modifiers: Modifiers::default(),
    });
    draw(cx);
    cx.simulate_event(MouseDownEvent {
        position, button: MouseButton::Left, click_count: 1,
        first_mouse: false, modifiers: Modifiers::default(),
    });
    draw(cx);
    cx.simulate_event(MouseUpEvent {
        position, button: MouseButton::Left, click_count: 1,
        modifiers: Modifiers::default(),
    });
    cx.executor().advance_clock(std::time::Duration::from_millis(300));
    draw(cx);
    assert_eq!(open_cell(&view, cx), None, "real Done click must close the cell");
    view.read_with(cx, |shell, app| {
        assert_eq!(shell.model.read(app).navigation().selected_note_id(), Some(&note),
            "Done must not select a note underneath the floating editor");
        assert_eq!(shell.note_session.as_ref().unwrap().entity_id(), session_id,
            "Done must not replace the active note session");
    });
    assert!(saved_body(&view, cx, &repository, &note).contains("完成按钮验收"),
        "Done must preserve the edited cell in its own note");
}

/// Seven more notes so that note cards lie under the floating cell editor.
fn notes_under_the_popup(
    view: &gpui::Entity<LibraryShell>,
    note: &app_lite_core::NoteId,
    cx: &mut VisualTestContext,
) {
    cx.simulate_resize(gpui::size(px(1160.0), px(789.0)));
    draw(cx);
    for _ in 0..7 {
        cx.update(|window, app| {
            view.update(app, |shell, shell_cx| {
                shell.apply_action(AppAction::CreateNote, window, shell_cx);
            })
        });
        draw(cx);
    }
    cx.update(|window, app| {
        view.update(app, |shell, shell_cx| {
            shell.apply_action(AppAction::SelectNote(note.clone()), window, shell_cx);
        })
    });
    draw(cx);
}

fn card_under(cx: &mut VisualTestContext, position: gpui::Point<gpui::Pixels>) -> bool {
    [
        "library-note-card-0",
        "library-note-card-1",
        "library-note-card-2",
        "library-note-card-3",
        "library-note-card-4",
        "library-note-card-5",
        "library-note-card-6",
        "library-note-card-7",
    ]
    .into_iter()
    .any(|selector| {
        cx.debug_bounds(selector)
            .is_some_and(|bounds| bounds.contains(&position))
    })
}

/// A real press and release: move, down, frame, (move to `to`), up.
fn pointer_press(
    cx: &mut VisualTestContext,
    from: gpui::Point<gpui::Pixels>,
    to: gpui::Point<gpui::Pixels>,
) {
    cx.simulate_event(gpui::MouseMoveEvent {
        position: from,
        pressed_button: None,
        modifiers: Modifiers::default(),
    });
    draw(cx);
    cx.simulate_event(MouseDownEvent {
        position: from,
        button: MouseButton::Left,
        click_count: 1,
        first_mouse: false,
        modifiers: Modifiers::default(),
    });
    draw(cx);
    if to != from {
        cx.simulate_event(gpui::MouseMoveEvent {
            position: to,
            pressed_button: Some(MouseButton::Left),
            modifiers: Modifiers::default(),
        });
        draw(cx);
    }
    cx.simulate_event(MouseUpEvent {
        position: to,
        button: MouseButton::Left,
        click_count: 1,
        modifiers: Modifiers::default(),
    });
    cx.executor()
        .advance_clock(std::time::Duration::from_millis(300));
    draw(cx);
}

// Every part of the floating cell editor keeps its pointer events: its
// buttons (saved or unsaved draft) and a drag in its editing area never
// select the note card, or move the body caret, underneath it.
#[gpui::test]
async fn cell_popup_controls_and_editing_area_do_not_click_through(cx: &mut TestAppContext) {
    let (_root, repository, note, view, cx) = mount(cx);
    notes_under_the_popup(&view, &note, cx);
    let session = view.read_with(cx, |shell, _| {
        shell.note_session.as_ref().unwrap().entity_id()
    });
    // (target, typed, saved): a clean Cancel changes nothing, so no save
    // fence can hide a click that reaches the card underneath.
    for (target, typed, saved) in [
        ("table-cell-editor-done", true, false),
        ("table-cell-editor-cancel", false, true),
        ("table-insert-row", true, true),
        ("editing area", true, true),
    ] {
        let _cell = open_cell_at_end(&view, cx, 1, 1);
        if typed {
            cx.simulate_input("穿透");
            draw(cx);
        }
        if saved {
            saved_body(&view, cx, &repository, &note);
            draw(cx);
        }
        let body_selection = main_editor(&view, cx).read_with(cx, |editor, _| editor.selection());
        let (from, to) = if target == "editing area" {
            let area = view.read_with(cx, |shell, _| {
                shell
                    .table_cell_editor
                    .as_ref()
                    .unwrap()
                    .bounds
                    .get()
                    .unwrap()
            });
            let y = area.top() + px(20.0);
            (
                point(area.left() + px(20.0), y),
                point(area.left() + px(60.0), y),
            )
        } else {
            let center = cx
                .debug_bounds(target)
                .unwrap_or_else(|| panic!("{target} is mounted"))
                .center();
            (center, center)
        };
        assert!(
            card_under(cx, from),
            "{target}: fixture must put a note card underneath {from:?}"
        );
        pointer_press(cx, from, to);
        view.read_with(cx, |shell, app| {
            assert_eq!(
                shell.model.read(app).navigation().selected_note_id(),
                Some(&note),
                "{target} must not select the note underneath"
            );
            assert_eq!(
                shell.note_session.as_ref().unwrap().entity_id(),
                session,
                "{target} must not replace the note session"
            );
            // A card reached through a dirty draft is refused by the save
            // fence, but its switch attempt still shows here.
            let save_error = format!("{:?}", shell.save_error);
            assert!(
                !save_error.contains("NoteSwitch"),
                "{target} must not reach a note card: {save_error}"
            );
        });
        assert_eq!(
            main_editor(&view, cx).read_with(cx, |editor, _| editor.selection()),
            body_selection,
            "{target} must not move the body caret underneath"
        );
        if open_cell(&view, cx).is_some() {
            cx.simulate_keystrokes("escape");
            draw(cx);
        }
    }
    // Closed, the list takes clicks again (one of the first two cards is
    // another note).
    saved_body(&view, cx, &repository, &note);
    draw(cx);
    let switched = ["library-note-card-0", "library-note-card-1"]
        .into_iter()
        .any(|selector| {
            let card = cx.debug_bounds(selector).expect("note card");
            pointer_press(cx, card.center(), card.center());
            view.read_with(cx, |shell, app| {
                shell.model.read(app).navigation().selected_note_id() != Some(&note)
            })
        });
    assert!(
        switched,
        "with the popup closed a card click selects its note"
    );
}

// Done or Tab during an IME composition never stores the provisional text;
// the popup stays open with its notice.
#[gpui::test]
async fn cell_done_or_tab_during_composition_keeps_the_popup_open(cx: &mut TestAppContext) {
    use gpui::EntityInputHandler;
    let (_root, repository, note, view, cx) = mount(cx);
    notes_under_the_popup(&view, &note, cx);
    let editor = open_cell_at_end(&view, cx, 1, 1);
    draw(cx);
    cx.update(|window, app| {
        editor.update(app, |editor, editor_cx| {
            editor.replace_and_mark_text_in_range(None, "ni", Some(2..2), window, editor_cx);
        })
    });
    draw(cx);
    assert!(editor.read_with(cx, |editor, _| editor.marked_text().is_some()));
    let done = cx
        .debug_bounds("table-cell-editor-done")
        .expect("Done")
        .center();
    pointer_press(cx, done, done);
    assert_eq!(open_cell(&view, cx), Some((1, 1)), "Done keeps the popup");
    cx.simulate_keystrokes("tab");
    draw(cx);
    assert_eq!(open_cell(&view, cx), Some((1, 1)), "Tab keeps the popup");
    view.read_with(cx, |shell, app| {
        assert_eq!(
            shell.model.read(app).navigation().selected_note_id(),
            Some(&note)
        );
        assert!(
            shell
                .table_cell_editor
                .as_ref()
                .unwrap()
                .error
                .as_deref()
                .is_some_and(|error| error.contains("输入法")),
            "the reason is shown"
        );
    });
    assert!(!saved_body(&view, cx, &repository, &note).contains("ni"));
}

/// A cell holding only a pasted (and stored) image; returns its editor and
/// the image's resource id.
fn cell_with_only_an_image(
    view: &gpui::Entity<LibraryShell>,
    cx: &mut VisualTestContext,
) -> (gpui::Entity<crate::native_editor::core::EditorCore>, String) {
    let editor = open_cell_at_end(view, cx, 1, 1);
    // The pasted image replaces the selected text.
    cx.update(|_, app| editor.update(app, |editor, _| editor.select_all()));
    paste_png(cx);
    draw(cx);
    let image = editor
        .read_with(cx, |editor, _| cell_image_ids(editor))
        .pop()
        .expect("pasted cell image");
    (editor, image)
}

/// What macOS gives the next paste after this app's copy: the fragment in
/// this app's native type, and only the plain string to GPUI.
fn as_native_copy(cx: &mut VisualTestContext) -> crate::native_editor::images::ClipboardFragment {
    let fragment = cx
        .read_from_clipboard()
        .map(crate::native_editor::images::ClipboardPayload::from_gpui)
        .and_then(|payload| payload.fragment)
        .expect("a structured copy");
    offer_native_copy(&fragment, cx);
    fragment
}

/// The same copy offered again for another paste.
fn offer_native_copy(
    fragment: &crate::native_editor::images::ClipboardFragment,
    cx: &mut VisualTestContext,
) {
    let native = crate::native_editor::images::native_fragment_payload(
        &serde_json::to_string(fragment).unwrap(),
    )
    .expect("native fragment");
    cx.write_to_clipboard(gpui::ClipboardItem::new_string(fragment.plain.clone()));
    crate::native_editor::images::set_next_native_pasteboard_for_test(native);
}

// 110: Cmd-C of an image in the cell editor put only U+FFFC on the
// clipboard. The cell's own Copy exports its selection with structure and
// the resource file, and it pastes back as the image, in the cell and in
// the body.
#[gpui::test]
async fn cell_copy_of_an_image_exports_it_and_pastes_it_back(cx: &mut TestAppContext) {
    let (_root, repository, note, view, cx) = mount(cx);
    let (editor, image) = cell_with_only_an_image(&view, cx);
    cx.update(|_, app| editor.update(app, |editor, _| editor.select_all()));
    cx.dispatch_action(crate::components::Copy);
    draw(cx);

    let export = crate::ui::clipboard::last_clipboard_export_for_test().expect("a cell copy");
    assert!(!export.plain.contains('\u{fffc}'), "{:?}", export.plain);
    assert!(export.resource_only, "an image alone");
    assert_eq!(export.files.len(), 1);
    assert_eq!(
        std::fs::read(&export.files[0]).unwrap(),
        repository
            .open_verified_resource_file(&app_lite_core::ResourceId::new(image.as_str()).unwrap())
            .unwrap()
            .map(|(_, mut file)| {
                let mut bytes = Vec::new();
                std::io::Read::read_to_end(&mut file, &mut bytes).unwrap();
                bytes
            })
            .unwrap(),
        "the exported file is the stored image"
    );
    let fragment = as_native_copy(cx);
    assert!(
        fragment.html.contains(&format!(":/{image}")),
        "{}",
        fragment.html
    );
    assert_eq!(
        fragment
            .resources
            .iter()
            .map(|resource| resource.id.as_str())
            .collect::<Vec<_>>(),
        [image.as_str()]
    );

    // Back into the same cell, after the image.
    cx.update(|_, app| {
        editor.update(app, |editor, _| {
            use crate::native_editor::model::{DocPoint, Selection};
            let block = editor.document().blocks().last().unwrap().clone();
            let end = block.content.as_text().map_or(1, |text| text.len());
            editor.set_selection_for_test(Selection::caret(DocPoint::new(block.id, end)));
        })
    });
    cx.dispatch_action(crate::components::Paste);
    draw(cx);
    assert_eq!(
        editor.read_with(cx, |editor, _| cell_image_ids(editor)),
        [image.clone(), image.clone()]
    );
    let body = saved_body(&view, cx, &repository, &note);
    assert_eq!(body.matches(&format!(":/{image}")).count(), 2, "{body}");

    // And into the body, out of the closed cell.
    offer_native_copy(&fragment, cx);
    cx.simulate_keystrokes("escape");
    draw(cx);
    let main = main_editor(&view, cx);
    let images_in_body =
        |cx: &mut VisualTestContext| main.read_with(cx, |editor, _| cell_image_ids(editor)).len();
    let before = images_in_body(cx);
    cx.update(|window, app| {
        main.update(app, |editor, _| editor.select_all());
        crate::native_editor::surface::focus_editor(&main, window, app);
    });
    cx.simulate_keystrokes("right");
    cx.dispatch_action(crate::components::Paste);
    draw(cx);
    assert_eq!(
        images_in_body(cx),
        before + 1,
        "the image lands in the body"
    );
}

// Cut copies the selected structure first, then deletes it from the open
// cell only; one Undo brings it back. A composition in progress refuses the
// cut and deletes nothing.
#[gpui::test]
async fn cell_cut_moves_only_the_cell_selection_and_undoes_in_one_step(cx: &mut TestAppContext) {
    let (_root, repository, note, view, cx) = mount(cx);
    let editor = open_cell_at_end(&view, cx, 1, 1);
    paste_png(cx);
    draw(cx);
    let image = editor
        .read_with(cx, |editor, _| cell_image_ids(editor))
        .pop()
        .expect("pasted cell image");
    let other_cell = main_cell_text(&view, cx, 1, 0);

    // Refused while composing, with the IME's selection over the composed
    // text: nothing deleted, nothing exported.
    let composing = cx.update(|window, app| {
        editor.update(app, |editor, editor_cx| {
            use gpui::EntityInputHandler;
            editor.replace_and_mark_text_in_range(None, "ni", Some(0..2), window, editor_cx);
            editor.marked_text().is_some() && !editor.selection().is_caret()
        })
    });
    if composing {
        cx.dispatch_action(crate::components::Cut);
        draw(cx);
        assert!(crate::ui::clipboard::last_clipboard_export_for_test().is_none());
        assert_eq!(
            editor.read_with(cx, |editor, _| cell_image_ids(editor)),
            [image.clone()],
            "a cut during composition deletes nothing"
        );
    }
    assert!(composing, "the fixture must cut during a real composition");
    // End the composition and take its text back out.
    cx.update(|window, app| {
        editor.update(app, |editor, editor_cx| {
            use gpui::EntityInputHandler;
            editor.unmark_text(window, editor_cx);
            editor.undo().unwrap();
        })
    });
    draw(cx);
    let has_text = |cx: &mut VisualTestContext, needle: &str| {
        editor.read_with(cx, |editor, _| {
            editor.document().blocks().iter().any(|block| {
                block
                    .content
                    .as_text()
                    .is_some_and(|text| text.contains(needle))
            })
        })
    };
    assert!(!has_text(cx, "ni"), "the composition is gone");
    assert_eq!(
        editor.read_with(cx, |editor, _| cell_image_ids(editor)),
        [image.clone()]
    );

    cx.update(|_, app| editor.update(app, |editor, _| editor.select_all()));
    let contents = |cx: &mut VisualTestContext| {
        editor.read_with(cx, |editor, _| {
            editor
                .document()
                .blocks()
                .iter()
                .map(|block| block.content.clone())
                .collect::<Vec<_>>()
        })
    };
    let before = contents(cx);
    cx.dispatch_action(crate::components::Cut);
    draw(cx);
    let export = crate::ui::clipboard::last_clipboard_export_for_test().expect("cut copies first");
    assert!(!export.plain.contains('\u{fffc}'), "{:?}", export.plain);
    let fragment = as_native_copy(cx);
    assert!(
        fragment.html.contains(&format!(":/{image}")),
        "{}",
        fragment.html
    );
    assert!(
        editor
            .read_with(cx, |editor, _| cell_image_ids(editor))
            .is_empty(),
        "the cell's selection is gone"
    );
    assert_eq!(
        main_cell_text(&view, cx, 1, 0),
        other_cell,
        "other cells untouched"
    );

    cx.simulate_keystrokes("cmd-z");
    draw(cx);
    assert_eq!(contents(cx), before, "one Undo restores the cut");
    cx.simulate_keystrokes("cmd-shift-z");
    draw(cx);
    assert!(
        editor
            .read_with(cx, |editor, _| cell_image_ids(editor))
            .is_empty()
    );

    // The cut image pastes into another cell and is saved with the note.
    cx.simulate_keystrokes("escape");
    draw(cx);
    let target = open_cell_at_end(&view, cx, 1, 0);
    // The first native offer deliberately replaced GPUI metadata with
    // plain text. Reoffer the fragment exported by the actual Cut rather
    // than trying to recover that removed metadata from GPUI again.
    offer_native_copy(&fragment, cx);
    cx.dispatch_action(crate::components::Paste);
    draw(cx);
    assert_eq!(
        target.read_with(cx, |editor, _| cell_image_ids(editor)),
        [image.clone()]
    );
    let body = saved_body(&view, cx, &repository, &note);
    assert!(body.contains(&format!(":/{image}")), "{body}");
}

// A Clipboard event from a closed cell's surface reaches nothing: not the
// cell open now, not the body.
#[gpui::test]
async fn closed_cell_surface_clipboard_events_reach_nothing(cx: &mut TestAppContext) {
    let (_root, _repository, _note, view, cx) = mount(cx);
    let _first = open_cell_at_end(&view, cx, 1, 1);
    let old_surface = view.read_with(cx, |shell, _| {
        shell.table_cell_editor.as_ref().unwrap().surface_for_test()
    });
    cx.simulate_keystrokes("escape");
    draw(cx);
    let second = open_cell_at_end(&view, cx, 1, 0);
    cx.update(|_, app| second.update(app, |editor, _| editor.select_all()));
    let body_before = main_editor(&view, cx).read_with(cx, |editor, _| editor.document().clone());
    let cell_before = second.read_with(cx, |editor, _| editor.document().clone());
    cx.update(|_, app| {
        old_surface.update(app, |_, surface_cx| {
            surface_cx
                .emit(crate::native_editor::surface::EditorSurfaceEvent::Clipboard { cut: true })
        })
    });
    draw(cx);
    assert!(crate::ui::clipboard::last_clipboard_export_for_test().is_none());
    assert_eq!(
        second.read_with(cx, |editor, _| editor.document().clone()),
        cell_before
    );
    assert_eq!(
        main_editor(&view, cx).read_with(cx, |editor, _| editor.document().clone()),
        body_before
    );
}

// A cut deletes only once its copy is on the clipboard: if the pasteboard
// refuses the write, or a resource file cannot be exported, the selection
// stays, in the cell and in the body, and the failure is shown.
#[gpui::test]
async fn cut_deletes_nothing_when_its_copy_does_not_reach_the_clipboard(cx: &mut TestAppContext) {
    let (_root, _repository, _note, view, cx) = mount(cx);
    let (editor, image) = cell_with_only_an_image(&view, cx);
    let fail_write = || crate::ui::clipboard::fail_next_clipboard_write_for_test();
    let fail_export = || crate::app::note_session::fail_next_clipboard_file_export_for_test();
    for (name, fail) in [
        ("pasteboard write", &fail_write as &dyn Fn()),
        ("resource file export", &fail_export as &dyn Fn()),
    ] {
        view.update(cx, |shell, _| shell.resource_notice = None);
        cx.update(|_, app| editor.update(app, |editor, _| editor.select_all()));
        fail();
        cx.dispatch_action(crate::components::Cut);
        draw(cx);
        assert_eq!(
            editor.read_with(cx, |editor, _| cell_image_ids(editor)),
            [image.clone()],
            "{name}: the cell keeps its selection"
        );
        assert!(
            notice(&view, cx).contains("未完成"),
            "{name}: the failure is shown: {}",
            notice(&view, cx)
        );
    }

    // The body's cut keeps the same rule (the image stays in the body's
    // table, so its copy has a resource file to export).
    view.update(cx, |shell, shell_cx| {
        shell.commit_table_cell_editor(false, shell_cx)
    });
    draw(cx);
    let main = main_editor(&view, cx);
    let blocks = |cx: &mut VisualTestContext| {
        main.read_with(cx, |editor, _| {
            editor
                .document()
                .blocks()
                .iter()
                .map(|block| block.content.clone())
                .collect::<Vec<_>>()
        })
    };
    let before = blocks(cx);
    for (name, fail) in [
        ("pasteboard write", &fail_write as &dyn Fn()),
        ("resource file export", &fail_export as &dyn Fn()),
    ] {
        view.update(cx, |shell, _| shell.resource_notice = None);
        cx.update(|window, app| {
            main.update(app, |editor, _| editor.select_all());
            crate::native_editor::surface::focus_editor(&main, window, app);
        });
        fail();
        cx.dispatch_action(crate::components::Cut);
        draw(cx);
        assert_eq!(blocks(cx), before, "{name}: the body keeps its selection");
        assert!(
            notice(&view, cx).contains("未完成"),
            "{name}: the failure is shown: {}",
            notice(&view, cx)
        );
    }
}

// While the note is read-only a cell cut deletes nothing; copying is still
// allowed (Evernote's copy is active when uneditable).
#[gpui::test]
async fn cell_cut_is_refused_while_the_note_is_read_only(cx: &mut TestAppContext) {
    let (_root, _repository, _note, view, cx) = mount(cx);
    let (editor, image) = cell_with_only_an_image(&view, cx);
    view.update(cx, |shell, shell_cx| {
        let session = shell.note_session.clone().unwrap();
        session.update(shell_cx, |session, session_cx| {
            session.set_reconciliation_locked(true, session_cx)
        });
    });
    cx.update(|_, app| editor.update(app, |editor, _| editor.select_all()));
    cx.dispatch_action(crate::components::Cut);
    draw(cx);
    assert_eq!(
        editor.read_with(cx, |editor, _| cell_image_ids(editor)),
        [image.clone()]
    );
    assert!(!notice(&view, cx).is_empty(), "the refusal is shown");
    cx.dispatch_action(crate::components::Copy);
    draw(cx);
    let fragment = as_native_copy(cx);
    assert!(
        fragment.html.contains(&format!(":/{image}")),
        "{}",
        fragment.html
    );
}

// A cell's bold text and image copy together and paste into another note
// with the mark and the resource.
#[gpui::test]
async fn cell_copy_of_formatted_text_and_image_pastes_into_another_note(cx: &mut TestAppContext) {
    use crate::native_editor::commands::{CommandArgument, CommandCatalogue, EditorCommand};
    let (_root, repository, _note, view, cx) = mount(cx);
    let editor = open_cell_at_end(&view, cx, 1, 1);
    // Bold the cell's text as a user would: the edit notifies, so the cell's
    // draft is saved before the image goes in.
    cx.update(|_, app| {
        editor.update(app, |editor, editor_cx| {
            editor.select_all();
            CommandCatalogue::new()
                .execute(EditorCommand::Bold, CommandArgument::None, editor)
                .unwrap();
            let block = editor.document().blocks().last().unwrap().clone();
            let end = block.content.as_text().unwrap().len();
            editor.set_selection_for_test(crate::native_editor::model::Selection::caret(
                crate::native_editor::model::DocPoint::new(block.id, end),
            ));
            editor_cx.notify();
        })
    });
    draw(cx);
    paste_png(cx);
    draw(cx);
    let image = editor
        .read_with(cx, |editor, _| cell_image_ids(editor))
        .pop()
        .expect("pasted cell image");
    cx.update(|_, app| editor.update(app, |editor, _| editor.select_all()));
    cx.dispatch_action(crate::components::Copy);
    draw(cx);
    let fragment = as_native_copy(cx);
    assert!(
        fragment.html.contains("<strong>一</strong>"),
        "{}",
        fragment.html
    );
    assert!(
        fragment.html.contains(&format!(":/{image}")),
        "{}",
        fragment.html
    );

    let other = repository
        .create_note(CreateNote {
            title: "另一篇".into(),
            notebook_id: None,
            document: CanonicalDocument::parse_html("<p>乙</p>").unwrap(),
        })
        .unwrap();
    cx.simulate_keystrokes("escape");
    draw(cx);
    for _ in 0..50 {
        let switched = cx.update(|window, app| {
            view.update(app, |shell, shell_cx| {
                shell.apply_action_with_result(
                    AppAction::SelectNote(other.id.clone()),
                    window,
                    shell_cx,
                )
            })
        });
        if switched {
            break;
        }
        cx.run_until_parked();
    }
    draw(cx);
    let main = main_editor(&view, cx);
    cx.update(|window, app| {
        main.update(app, |editor, _| editor.select_all());
        crate::native_editor::surface::focus_editor(&main, window, app);
    });
    cx.simulate_keystrokes("right");
    offer_native_copy(&fragment, cx);
    cx.dispatch_action(crate::components::Paste);
    draw(cx);
    cx.update(|window, app| {
        view.update(app, |shell, shell_cx| {
            shell.apply_action(AppAction::ManualSync, window, shell_cx);
        })
    });
    cx.run_until_parked();
    let stored = repository.load_note(&other.id).unwrap().unwrap();
    assert!(
        stored.body_html.contains("<strong>一</strong>"),
        "{}",
        stored.body_html
    );
    assert!(
        stored.body_html.contains(&format!(":/{image}")),
        "{}",
        stored.body_html
    );
    assert_eq!(
        stored.resource_ids,
        vec![app_lite_core::ResourceId::new(image.as_str()).unwrap()]
    );
}

// Copied in one library's cell, pasted in another library: the image comes
// from the copy's exported file and is stored there with the same bytes.
#[gpui::test]
async fn cell_copy_pastes_its_image_into_another_library(cx: &mut TestAppContext) {
    let (_root, repository, _note, view, cx) = mount(cx);
    let (editor, image) = cell_with_only_an_image(&view, cx);
    let bytes = repository
        .read_resource_bytes(&app_lite_core::ResourceId::new(image.as_str()).unwrap())
        .unwrap()
        .unwrap();
    cx.update(|_, app| editor.update(app, |editor, _| editor.select_all()));
    cx.dispatch_action(crate::components::Copy);
    draw(cx);
    let fragment = as_native_copy(cx);
    assert!(
        fragment.resources[0]
            .file
            .as_ref()
            .is_some_and(|file| std::fs::read(file).unwrap() == bytes),
        "the copy carries the image's bytes"
    );

    let second_root = tempfile::tempdir().unwrap();
    let second =
        Arc::new(LibraryRepository::open(second_root.path().join("library.sqlite")).unwrap());
    let target = second
        .create_note(CreateNote {
            title: "资料库乙".into(),
            notebook_id: None,
            document: CanonicalDocument::parse_html("<p>乙</p>").unwrap(),
        })
        .unwrap();
    let second_model = cx.new(|_| AppModel::open(Arc::clone(&second)).unwrap());
    let mut app = cx.cx.clone();
    let (second_view, second_cx) =
        app.add_window_view(move |window, app| LibraryShell::new(second_model, None, window, app));
    draw(second_cx);
    second_cx.update(|window, app| {
        second_view.update(app, |shell, shell_cx| {
            shell.apply_action(AppAction::SelectNote(target.id.clone()), window, shell_cx);
        })
    });
    draw(second_cx);
    let main = main_editor(&second_view, second_cx);
    second_cx.update(|window, app| {
        main.update(app, |editor, _| editor.select_all());
        crate::native_editor::surface::focus_editor(&main, window, app);
    });
    second_cx.simulate_keystrokes("right");
    offer_native_copy(&fragment, second_cx);
    second_cx.dispatch_action(crate::components::Paste);
    draw(second_cx);
    draw(second_cx);
    second_cx.update(|window, app| {
        second_view.update(app, |shell, shell_cx| {
            shell.apply_action(AppAction::ManualSync, window, shell_cx);
        })
    });
    second_cx.run_until_parked();
    let stored = second.load_note(&target.id).unwrap().unwrap();
    assert_eq!(stored.resource_ids.len(), 1, "{}", stored.body_html);
    assert_eq!(
        second
            .read_resource_bytes(&stored.resource_ids[0])
            .unwrap()
            .unwrap(),
        bytes,
        "the same image, now stored in the other library"
    );
}

fn table_rows(view: &gpui::Entity<LibraryShell>, cx: &mut VisualTestContext) -> usize {
    main_editor(view, cx).read_with(cx, |editor, _| {
        editor
            .document()
            .blocks()
            .iter()
            .find_map(|block| match &block.content {
                BlockContent::Table(table) => Some(table.rows.len()),
                _ => None,
            })
            .expect("the note's table")
    })
}

// 114: after a structure button and Done (both real presses), the very next
// Cmd-Z belongs to the body and undoes that change; Cmd-Shift-Z redoes it.
// While the cell is open, Cmd-Z still undoes the cell's own typing.
#[gpui::test]
async fn closing_the_cell_by_mouse_returns_the_keyboard_to_the_body(cx: &mut TestAppContext) {
    let (_root, _repository, _note, view, cx) = mount(cx);
    let rows = table_rows(&view, cx);

    let cell = open_cell_at_end(&view, cx, 1, 1);
    draw(cx);
    cx.simulate_input("格");
    draw(cx);
    cx.simulate_keystrokes("cmd-z");
    draw(cx);
    assert!(
        !cell.read_with(cx, |editor, _| editor.copy_plain_text().contains('格')
            || editor.document().blocks().iter().any(|block| block
                .content
                .as_text()
                .is_some_and(|text| text.contains('格')))),
        "with the cell open, Cmd-Z undoes the cell's typing"
    );

    let insert = cx
        .debug_bounds("table-insert-row")
        .expect("insert row button")
        .center();
    pointer_press(cx, insert, insert);
    assert_eq!(table_rows(&view, cx), rows + 1, "the row was inserted");
    let done = cx
        .debug_bounds("table-cell-editor-done")
        .expect("Done")
        .center();
    pointer_press(cx, done, done);
    assert_eq!(open_cell(&view, cx), None);

    cx.simulate_keystrokes("cmd-z");
    draw(cx);
    assert_eq!(
        table_rows(&view, cx),
        rows,
        "Cmd-Z after Done undoes the insert"
    );
    cx.simulate_keystrokes("cmd-shift-z");
    draw(cx);
    assert_eq!(table_rows(&view, cx), rows + 1, "Cmd-Shift-Z redoes it");

    // Cancel hands the keyboard back the same way.
    let _cell = open_cell_at_end(&view, cx, 1, 1);
    draw(cx);
    let cancel = cx
        .debug_bounds("table-cell-editor-cancel")
        .expect("Cancel")
        .center();
    pointer_press(cx, cancel, cancel);
    assert_eq!(open_cell(&view, cx), None);
    cx.simulate_keystrokes("cmd-z");
    draw(cx);
    assert_eq!(
        table_rows(&view, cx),
        rows,
        "Cmd-Z after Cancel reaches the body"
    );
}

// The body's Cut during an IME composition (the IME's selection over the
// composed text) exports and deletes nothing, as in a cell.
#[gpui::test]
async fn body_cut_during_composition_exports_and_deletes_nothing(cx: &mut TestAppContext) {
    let (_root, _repository, _note, view, cx) = mount(cx);
    let main = main_editor(&view, cx);
    let composing = cx.update(|window, app| {
        crate::native_editor::surface::focus_editor(&main, window, app);
        main.update(app, |editor, editor_cx| {
            use gpui::EntityInputHandler;
            editor.replace_and_mark_text_in_range(None, "ni", Some(0..2), window, editor_cx);
            editor.marked_text().is_some() && !editor.selection().is_caret()
        })
    });
    assert!(composing, "the fixture must cut during a real composition");
    draw(cx);
    cx.dispatch_action(crate::components::Cut);
    draw(cx);
    assert!(
        crate::ui::clipboard::last_clipboard_export_for_test().is_none(),
        "nothing is exported"
    );
    assert!(
        main.read_with(cx, |editor, _| editor.marked_text().is_some()),
        "the composition is still there"
    );
    assert!(
        main.read_with(cx, |editor, _| editor.document().blocks().iter().any(
            |block| block
                .content
                .as_text()
                .is_some_and(|text| text.contains("ni"))
        )),
        "nothing was deleted"
    );
}
