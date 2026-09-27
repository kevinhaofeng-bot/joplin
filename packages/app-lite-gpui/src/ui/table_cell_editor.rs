//! Editing one table cell (tables step 2, docs/research/table-model-proposal.md).
//!
//! The table stays one atom in the note. A double-clicked cell opens a small
//! editor of its own (same EditorCore, so IME, marks, links and undo work as
//! in the note); committing writes the cell back through one undoable
//! `ReplaceTable`. Tab commits and moves to the next cell (adding a row after
//! the last one, as Evernote's table keymap does), Escape discards.

use super::*;
use crate::native_editor::codec::{
    export_canonical_with_resources, import_canonical_with_resources,
};
use crate::native_editor::core::EditorCore;
use crate::native_editor::model::BlockContent;
use crate::native_editor::surface::EditorSurfaceMode;
use app_lite_core::CanonicalDocument;
use app_lite_core::document::{Block as CanonicalBlock, BlockStyle, Inline};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum TableStructureChange {
    InsertRowBelow,
    DeleteRow,
    InsertColumnRight,
    DeleteColumn,
}

pub(crate) struct TableCellEditor {
    pub(crate) node_id: crate::native_editor::model::NodeId,
    pub(crate) row: usize,
    pub(crate) column: usize,
    pub(crate) editor: Entity<EditorCore>,
    surface: Entity<EditorSurface>,
    resources: Vec<app_lite_core::ResourceId>,
    needs_focus: bool,
    error: Option<String>,
}

/// Cell content is inline only: paragraphs join with line breaks, images
/// stay images, anything block-shaped is refused rather than flattened.
fn cell_inlines(document: &CanonicalDocument) -> Result<Vec<Inline>, String> {
    let mut out = Vec::new();
    for block in document.blocks() {
        let inlines = match block {
            CanonicalBlock::Paragraph { inlines, .. }
            | CanonicalBlock::Heading { inlines, .. }
            | CanonicalBlock::Quote { inlines, .. }
            | CanonicalBlock::Code { inlines, .. } => inlines.clone(),
            CanonicalBlock::Image {
                resource_id,
                alt,
                presentation,
                link,
            } => vec![Inline::Image {
                resource_id: resource_id.clone(),
                alt: alt.clone(),
                display_width: presentation.display_width,
                link: link.clone(),
            }],
            _ => return Err("单元格只能包含文字、链接和图片。".into()),
        };
        if !out.is_empty() {
            out.push(Inline::SoftBreak);
        }
        out.extend(inlines);
    }
    Ok(out)
}

impl LibraryShell {
    pub(super) fn open_table_cell_editor(
        &mut self,
        node_id: crate::native_editor::model::NodeId,
        row: usize,
        column: usize,
        cx: &mut Context<Self>,
    ) {
        let Some(session) = self.note_session.as_ref() else {
            return;
        };
        let main = session.read(cx).editor().clone();
        let inlines = main.read_with(cx, |editor, _| {
            match &editor.document().block(node_id)?.content {
                BlockContent::Table(table) => {
                    table.cell_inlines(row, column).map(<[Inline]>::to_vec)
                }
                _ => None,
            }
        });
        let Some(inlines) = inlines else {
            return;
        };
        let canonical = CanonicalDocument::from_blocks(vec![CanonicalBlock::Paragraph {
            style: BlockStyle::default(),
            inlines,
        }]);
        let resources = canonical.resource_ids();
        let document = match import_canonical_with_resources(&canonical, &resources) {
            Ok(document) => document,
            Err(error) => {
                self.resource_notice = Some(format!("无法编辑该单元格：{error}"));
                cx.notify();
                return;
            }
        };
        let editor = cx.new(|cx| EditorCore::new(document, cx));
        let image_cache = self.image_cache.clone();
        let surface_editor = editor.clone();
        let surface = cx.new(move |cx| {
            EditorSurface::new(
                surface_editor,
                EditorSurfaceMode::Editable,
                Some(image_cache),
                cx,
            )
        });
        self.table_cell_editor = Some(TableCellEditor {
            node_id,
            row,
            column,
            editor,
            surface,
            resources,
            needs_focus: true,
            error: None,
        });
        cx.notify();
    }

    /// Writes the cell back; with `advance`, opens the next cell.
    pub(super) fn commit_table_cell_editor(&mut self, advance: bool, cx: &mut Context<Self>) {
        let Some(cell) = self.table_cell_editor.as_ref() else {
            return;
        };
        let exported = cell.editor.read_with(cx, |editor, _| {
            export_canonical_with_resources(editor.document(), Some(&cell.resources))
        });
        let inlines = match exported
            .map_err(|error| error.to_string())
            .and_then(|document| cell_inlines(&document))
        {
            Ok(inlines) => inlines,
            Err(message) => {
                if let Some(cell) = self.table_cell_editor.as_mut() {
                    cell.error = Some(message);
                }
                cx.notify();
                return;
            }
        };
        let (node_id, row, column) = (cell.node_id, cell.row, cell.column);
        let Some(session) = self.note_session.as_ref() else {
            self.table_cell_editor = None;
            cx.notify();
            return;
        };
        let main = session.read(cx).editor().clone();
        let next = main.update(cx, |editor, editor_cx| {
            let result = editor.set_table_cell(node_id, row, column, inlines);
            if result.is_ok() && advance {
                let (rows, columns) = match &editor.document().block(node_id).map(|b| &b.content) {
                    Some(BlockContent::Table(table)) => (table.rows.len(), table.column_count()),
                    _ => (0, 0),
                };
                let next = if column + 1 < columns {
                    Some((row, column + 1))
                } else if row + 1 < rows {
                    Some((row + 1, 0))
                } else {
                    editor
                        .insert_table_row(node_id, rows)
                        .ok()
                        .map(|()| (rows, 0))
                };
                editor_cx.notify();
                return result.map(|()| next);
            }
            editor_cx.notify();
            result.map(|()| None)
        });
        match next {
            Ok(next) => {
                self.table_cell_editor = None;
                if let Some((row, column)) = next {
                    self.open_table_cell_editor(node_id, row, column, cx);
                }
            }
            Err(error) => {
                if let Some(cell) = self.table_cell_editor.as_mut() {
                    cell.error = Some(format!("无法保存单元格：{error}"));
                }
            }
        }
        cx.notify();
    }

    /// Saves the open cell, then changes the table around it; the editor
    /// stays on a cell that still exists.
    pub(super) fn edit_table_structure(
        &mut self,
        change: TableStructureChange,
        cx: &mut Context<Self>,
    ) {
        let Some(cell) = self.table_cell_editor.as_ref() else {
            return;
        };
        let (node_id, row, column) = (cell.node_id, cell.row, cell.column);
        self.commit_table_cell_editor(false, cx);
        if self.table_cell_editor.is_some() {
            return; // the cell could not be saved; its error is shown
        }
        let Some(session) = self.note_session.as_ref() else {
            return;
        };
        let main = session.read(cx).editor().clone();
        let result = main.update(cx, |editor, editor_cx| {
            let result = match change {
                TableStructureChange::InsertRowBelow => editor.insert_table_row(node_id, row + 1),
                TableStructureChange::DeleteRow => editor.delete_table_row(node_id, row),
                TableStructureChange::InsertColumnRight => {
                    editor.insert_table_column(node_id, column + 1)
                }
                TableStructureChange::DeleteColumn => editor.delete_table_column(node_id, column),
            };
            editor_cx.notify();
            result
        });
        let (next_row, next_column) = match change {
            TableStructureChange::InsertRowBelow => (row + 1, column),
            TableStructureChange::DeleteRow => (row.saturating_sub(1), column),
            TableStructureChange::InsertColumnRight => (row, column + 1),
            TableStructureChange::DeleteColumn => (row, column.saturating_sub(1)),
        };
        match result {
            Ok(()) => self.open_table_cell_editor(node_id, next_row, next_column, cx),
            Err(_) => {
                self.open_table_cell_editor(node_id, row, column, cx);
                if let Some(cell) = self.table_cell_editor.as_mut() {
                    cell.error = Some("表格至少保留一行一列。".into());
                }
            }
        }
        cx.notify();
    }

    pub(super) fn insert_table_action(
        &mut self,
        _: &crate::app::InsertNoteTable,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(session) = self.note_session.as_ref() else {
            return;
        };
        let main = session.read(cx).editor().clone();
        let inserted = main.update(cx, |editor, editor_cx| {
            let result = editor.insert_table(3, 3);
            editor_cx.notify();
            result
        });
        match inserted {
            Ok(node_id) => self.open_table_cell_editor(node_id, 0, 0, cx),
            Err(error) => {
                self.resource_notice = Some(format!("无法插入表格：{error}"));
                cx.notify();
            }
        }
    }

    pub(super) fn cancel_table_cell_editor(&mut self, cx: &mut Context<Self>) {
        if self.table_cell_editor.take().is_some() {
            cx.notify();
        }
    }

    pub(super) fn render_table_cell_editor(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<gpui::AnyElement> {
        let cell = self.table_cell_editor.as_mut()?;
        if cell.needs_focus {
            cell.needs_focus = false;
            crate::native_editor::surface::focus_editor(&cell.editor, window, cx);
        }
        let title = format!(
            "编辑单元格（第 {} 行，第 {} 列）",
            cell.row + 1,
            cell.column + 1
        );
        let error = cell.error.clone();
        let surface = cell.surface.clone();
        Some(
            div()
                .id("table-cell-editor")
                .debug_selector(|| "table-cell-editor".to_owned())
                .absolute()
                .top(px(120.0))
                .left(px(320.0))
                .w(px(520.0))
                .p(px(12.0))
                .rounded(px(8.0))
                .bg(rgba(0xffffffff))
                .border_1()
                .border_color(rgba(0xc9d3ccff))
                .shadow_lg()
                .flex()
                .flex_col()
                .gap(px(8.0))
                .capture_key_down(cx.listener(|shell, event: &KeyDownEvent, _window, cx| {
                    match event.keystroke.key.as_str() {
                        "tab" if !event.keystroke.modifiers.shift => {
                            shell.commit_table_cell_editor(true, cx);
                            cx.stop_propagation();
                        }
                        "escape" => {
                            shell.cancel_table_cell_editor(cx);
                            cx.stop_propagation();
                        }
                        _ => {}
                    }
                }))
                .child(
                    div()
                        .text_size(px(12.0))
                        .text_color(rgba(0x536f59ff))
                        .child(title),
                )
                .child(div().h(px(160.0)).w_full().child(surface))
                .children(error.map(|message| {
                    div()
                        .text_size(px(11.0))
                        .text_color(rgba(0xa34838ff))
                        .child(message)
                }))
                .child(
                    div()
                        .flex()
                        .gap(px(8.0))
                        .items_center()
                        .child(
                            div()
                                .id("table-cell-editor-done")
                                .px(px(10.0))
                                .py(px(4.0))
                                .rounded(px(4.0))
                                .bg(rgba(0x00a82dff))
                                .text_color(rgba(0xffffffff))
                                .text_size(px(12.0))
                                .cursor_pointer()
                                .on_mouse_down(
                                    MouseButton::Left,
                                    cx.listener(|shell, _event, _window, cx| {
                                        shell.commit_table_cell_editor(false, cx);
                                    }),
                                )
                                .child("完成"),
                        )
                        .child(
                            div()
                                .id("table-cell-editor-cancel")
                                .px(px(10.0))
                                .py(px(4.0))
                                .rounded(px(4.0))
                                .bg(rgba(0xf1f4f1ff))
                                .text_size(px(12.0))
                                .cursor_pointer()
                                .on_mouse_down(
                                    MouseButton::Left,
                                    cx.listener(|shell, _event, _window, cx| {
                                        shell.cancel_table_cell_editor(cx);
                                    }),
                                )
                                .child("取消"),
                        )
                        .child(
                            div()
                                .text_size(px(11.0))
                                .text_color(rgba(0x8a978dff))
                                .child("Tab 保存并编辑下一格，Esc 放弃修改"),
                        ),
                )
                .child(
                    div().flex().gap(px(6.0)).children(
                        [
                            (
                                "table-insert-row",
                                "下方插入行",
                                TableStructureChange::InsertRowBelow,
                            ),
                            (
                                "table-delete-row",
                                "删除此行",
                                TableStructureChange::DeleteRow,
                            ),
                            (
                                "table-insert-column",
                                "右侧插入列",
                                TableStructureChange::InsertColumnRight,
                            ),
                            (
                                "table-delete-column",
                                "删除此列",
                                TableStructureChange::DeleteColumn,
                            ),
                        ]
                        .into_iter()
                        .map(|(id, label, change)| {
                            div()
                                .id(id)
                                .px(px(8.0))
                                .py(px(3.0))
                                .rounded(px(4.0))
                                .bg(rgba(0xf1f4f1ff))
                                .text_size(px(11.0))
                                .cursor_pointer()
                                .on_mouse_down(
                                    MouseButton::Left,
                                    cx.listener(move |shell, _event, _window, cx| {
                                        shell.edit_table_structure(change, cx);
                                    }),
                                )
                                .child(label)
                        }),
                    ),
                )
                .into_any_element(),
        )
    }
}
