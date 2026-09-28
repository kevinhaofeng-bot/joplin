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
            document: CanonicalDocument::parse_html(TABLE).unwrap(),
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
