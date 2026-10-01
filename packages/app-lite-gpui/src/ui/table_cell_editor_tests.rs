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
    open_cell_at_end(&view, cx, 1, 1);
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
    assert!(
        editor.read_with(cx, |editor, _| editor.image_source_path(&image).is_some()),
        "the cell shows the stored image"
    );
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
