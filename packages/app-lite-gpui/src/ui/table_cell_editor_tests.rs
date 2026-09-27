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

fn mount<'a>(
    cx: &'a mut TestAppContext,
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
    let clock = Arc::new(ManualSaveClock::default());
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
