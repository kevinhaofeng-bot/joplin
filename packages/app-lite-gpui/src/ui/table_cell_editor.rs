//! Editing one table cell (tables step 2, docs/research/table-model-proposal.md).
//!
//! The table stays one atom in the note. A double-clicked cell opens a small
//! editor of its own (same EditorCore, so IME, marks, links and undo work as
//! in the note); committing writes the cell back through one undoable
//! `ReplaceTable`. Tab commits and moves to the next cell (adding a row after
//! the last one, as Evernote's table keymap does), Escape discards.

use super::*;
use crate::native_editor::codec::{
    export_canonical_with_resources, import_canonical_with_resources, table_cell_inlines,
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

const CELL_IMAGE_CACHE_BUDGET: usize = 16 * 1024 * 1024;

pub(crate) struct TableCellEditor {
    pub(crate) node_id: crate::native_editor::model::NodeId,
    pub(crate) row: usize,
    pub(crate) column: usize,
    pub(crate) editor: Entity<EditorCore>,
    surface: Entity<EditorSurface>,
    resources: Vec<app_lite_core::ResourceId>,
    original_inlines: Vec<Inline>,
    _observation: Subscription,
    // Copy and cut from this cell's surface; dropped with the cell.
    _surface_events: Subscription,
    needs_focus: bool,
    pub(crate) error: Option<String>,
    // Cleared when this cell closes, for resource inserts still in flight.
    open: std::sync::Arc<std::sync::atomic::AtomicBool>,
    // Revision of the note's table block as this cell last wrote or read it:
    // any other change to the table (rows added or removed) moves it on.
    table_revision: std::sync::Arc<std::sync::atomic::AtomicU64>,
    // Where the cell's editing area was painted, for drops.
    pub(crate) bounds: std::rc::Rc<std::cell::Cell<Option<gpui::Bounds<gpui::Pixels>>>>,
    // The cell surface's own decode cache, for tests that check what the
    // cell itself painted.
    #[cfg(test)]
    pub(crate) image_cache: Entity<crate::native_editor::images::BudgetedImageCache>,
}

impl TableCellEditor {
    pub(crate) fn surface_id(&self) -> gpui::EntityId {
        self.surface.entity_id()
    }

    #[cfg(test)]
    pub(crate) fn surface_for_test(&self) -> Entity<EditorSurface> {
        self.surface.clone()
    }
}

impl Drop for TableCellEditor {
    fn drop(&mut self) {
        self.open.store(false, std::sync::atomic::Ordering::Release);
    }
}

impl LibraryShell {
    /// The open cell's caret (or `selection` in it) as a resource insert
    /// target; None without an open cell.
    pub(super) fn capture_cell_resource_insert_intent(
        &mut self,
        selection: Option<crate::native_editor::model::Selection>,
        cx: &mut Context<Self>,
    ) -> Option<Result<InsertIntent, String>> {
        let cell = self.table_cell_editor.as_ref()?;
        let editor = cell.editor.clone();
        let target = crate::app::note_session::CellTargetParts {
            open: cell.open.clone(),
            table_revision: cell.table_revision.clone(),
            place: (cell.node_id, cell.row, cell.column),
        };
        let session = self.note_session.clone()?;
        Some(
            session
                .update(cx, |session, session_cx| {
                    session.capture_table_cell_resource_insert_intent(
                        &editor, selection, target, session_cx,
                    )
                })
                .map_err(|error| error.to_string()),
        )
    }

    /// Whether `focused` (what had the focus when a button or menu acted)
    /// is the open cell's editor.
    pub(super) fn table_cell_is(&self, focused: Option<&gpui::FocusHandle>, cx: &App) -> bool {
        self.table_cell_editor.as_ref().is_some_and(|cell| {
            focused.is_some_and(|focused| cell.editor.read(cx).focus_handle() == focused)
        })
    }

    /// The open cell's point under `position`, if the drop lands in it.
    pub(super) fn table_cell_drop_point(
        &mut self,
        position: gpui::Point<gpui::Pixels>,
        cx: &mut Context<Self>,
    ) -> Option<Option<crate::native_editor::model::DocPoint>> {
        let cell = self.table_cell_editor.as_ref()?;
        if !cell
            .bounds
            .get()
            .is_some_and(|bounds| bounds.contains(&position))
        {
            return None;
        }
        let editor = cell.editor.clone();
        Some(editor.update(cx, |editor, _| editor.point_from_layout(position)))
    }

    pub(super) fn table_cell_has_pending_input(&self, cx: &App) -> bool {
        self.table_cell_editor.as_ref().is_some_and(|cell| {
            cell.editor.read(cx).marked_text().is_some() || cell.error.is_some()
        })
    }

    /// Include an open cell draft in the note's existing durable save barrier
    /// without closing the cell or stealing its keyboard focus.
    pub(super) fn persist_table_cell_draft(&mut self, cx: &mut Context<Self>) -> Result<(), String> {
        let Some(cell) = self.table_cell_editor.as_ref() else { return Ok(()); };
        // The note's allowlist carries resources the cell gained since it
        // opened (a pasted image), published before the cell changed.
        let mut allowed = cell.resources.clone();
        if let Some(session) = self.note_session.as_ref() {
            allowed.extend(session.read(cx).resource_allowlist().iter().cloned());
        }
        let inlines = cell.editor.read_with(cx, |editor, _| {
            if editor.marked_text().is_some() {
                return Err("单元格中仍有未确认的输入法组合文本，请先确认或取消输入".into());
            }
            export_canonical_with_resources(editor.document(), Some(&allowed))
                .map_err(|error| error.to_string())
                .and_then(|document| table_cell_inlines(&document))
        })?;
        let (node_id, row, column) = (cell.node_id, cell.row, cell.column);
        let table_revision = cell.table_revision.clone();
        let session = self.note_session.as_ref().ok_or("单元格所属笔记已经关闭")?;
        let main = session.read(cx).editor().clone();
        main.update(cx, |editor, editor_cx| {
            let block = editor.document().block(node_id);
            let current = block.and_then(|block| match &block.content {
                BlockContent::Table(table) => table.cell_inlines(row, column),
                _ => None,
            });
            if current == Some(inlines.as_slice()) { return Ok(()); }
            // Rows added or removed elsewhere: these coordinates may name
            // another cell now, so this cell's text is not written there.
            if block.map(|block| block.revision)
                != Some(table_revision.load(std::sync::atomic::Ordering::Acquire))
            {
                return Err(
                    "表格已在别处改变，单元格内容未写入；请按 Esc 放弃后重新打开".to_owned(),
                );
            }
            editor.set_table_cell(node_id, row, column, inlines)
                .map_err(|error| error.to_string())?;
            if let Some(block) = editor.document().block(node_id) {
                table_revision.store(block.revision, std::sync::atomic::Ordering::Release);
            }
            editor_cx.notify();
            Ok(())
        })
    }

    pub(super) fn table_cell_has_focus(&self, window: &Window, cx: &App) -> bool {
        self.table_cell_editor
            .as_ref()
            .is_some_and(|cell| cell.editor.read(cx).focus_handle().is_focused(window))
    }

    /// Paste into the focused cell. Images go through the note's resource
    /// chain into this cell's caret (table/schema.ts 219: a cell holds
    /// tablecontent, which includes image); text, marks, links and images
    /// already in this library paste directly.
    pub(super) fn paste_into_table_cell(
        &mut self,
        intent: PasteIntent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(editor) = self
            .table_cell_editor
            .as_ref()
            .map(|cell| cell.editor.clone())
        else {
            return;
        };
        let (request, temporary_paths) = match intent {
            PasteIntent::Image { payload } => (ResourceImportRequest::Image(payload), Vec::new()),
            PasteIntent::ImageCandidates { candidates } => (
                ResourceImportRequest::ImageCandidates(candidates),
                Vec::new(),
            ),
            PasteIntent::EncodedImage { payload } => {
                (ResourceImportRequest::EncodedImage(payload), Vec::new())
            }
            PasteIntent::File { path, cleanup } => (
                ResourceImportRequest::Path(path.clone()),
                cleanup.then_some(path).into_iter().collect(),
            ),
            PasteIntent::FileCandidates {
                paths,
                cleanup_paths,
            } => (ResourceImportRequest::Paths(paths), cleanup_paths),
            intent => {
                self.paste_content_into_table_cell(intent, &editor, cx);
                crate::native_editor::surface::focus_editor(&editor, window, cx);
                return;
            }
        };
        let captured = self
            .capture_cell_resource_insert_intent(None, cx)
            .ok_or_else(|| "单元格已关闭".to_owned())
            .and_then(|captured| captured);
        let result = match captured {
            Ok(intent) => {
                self.complete_resource_request(request, intent, temporary_paths, window, cx)
            }
            Err(error) => {
                cleanup_owned_temporary_paths(&temporary_paths);
                Err(error)
            }
        };
        if let Err(error) = result {
            self.resource_notice = Some(format!("粘贴未完成：{error}"));
            cx.notify();
        }
        crate::native_editor::surface::focus_editor(&editor, window, cx);
    }

    fn paste_content_into_table_cell(
        &mut self,
        intent: PasteIntent,
        editor: &Entity<EditorCore>,
        cx: &mut Context<Self>,
    ) {
        let mut available = Vec::new();
        let mut unavailable = Vec::new();
        let (formatted, plain) = match intent {
            PasteIntent::Text { text } => (None, Some(text)),
            PasteIntent::Fragment { fragment } if fragment.resources.is_empty() => (
                CanonicalDocument::parse_html(&fragment.html).ok(),
                Some(fragment.plain),
            ),
            PasteIntent::Fragment { fragment } => {
                let Some(session) = self.note_session.clone() else {
                    return;
                };
                match session.update(cx, |session, _| session.fragment_resources(&fragment)) {
                    Ok((html, here, missing)) => {
                        available = here;
                        unavailable = missing;
                        (
                            CanonicalDocument::parse_html(&html).ok(),
                            Some(fragment.plain),
                        )
                    }
                    Err(error) => {
                        self.resource_notice = Some(format!("粘贴未完成：{error}"));
                        cx.notify();
                        return;
                    }
                }
            }
            PasteIntent::Html { html, text } => {
                match CanonicalDocument::parse_pasted_html(&html) {
                    Ok(pasted) if pasted.images.is_empty() => (Some(pasted.document), text),
                    Ok(_) => (None, None),
                    Err(_) => (None, text),
                }
            }
            _ => (None, None),
        };
        let blocks = formatted
            .and_then(|document| table_cell_inlines(&document).ok())
            .map(|mut inlines| {
                inlines.retain(|inline| {
                    !matches!(
                        inline,
                        Inline::Image { resource_id, .. } if unavailable.contains(resource_id)
                    )
                });
                CanonicalDocument::from_blocks(vec![CanonicalBlock::Paragraph {
                    style: BlockStyle::default(),
                    inlines,
                }])
            })
            .and_then(|document| import_canonical_with_resources(&document, &available).ok())
            .map(|document| document.blocks().iter().cloned().collect::<Vec<_>>());
        if blocks.is_none() && plain.is_none() {
            self.resource_notice =
                Some("单元格暂不能粘贴网页中的图片；请复制图片本身后再粘贴".into());
            cx.notify();
            return;
        }
        if !available.is_empty() {
            if let Some(session) = self.note_session.as_ref() {
                session.update(cx, |session, _| session.allow_resources(&available));
            }
        }
        let result = editor.update(cx, |editor, editor_cx| {
            let result = match (blocks, plain) {
                (Some(blocks), _) => editor
                    .paste_blocks(&crate::native_editor::core::CopiedBlocks {
                        blocks,
                        open_start: true,
                        open_end: true,
                    })
                    .map(|_| ()),
                (None, Some(text)) => editor.paste_plain_text(&text),
                (None, None) => unreachable!("checked above"),
            };
            editor_cx.notify();
            result
        });
        if let Err(error) = result {
            self.resource_notice = Some(format!("粘贴未完成：{error}"));
        } else if !unavailable.is_empty() {
            self.resource_notice = Some(format!(
                "{} 个图片或附件未粘贴：本资料库中没有，复制时也未能导出其文件",
                unavailable.len()
            ));
        }
        cx.notify();
    }

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
            let block = editor.document().block(node_id)?;
            match &block.content {
                BlockContent::Table(table) => table
                    .cell_inlines(row, column)
                    .map(|inlines| (inlines.to_vec(), block.revision)),
                _ => None,
            }
        });
        let Some((inlines, revision)) = inlines else {
            return;
        };
        let original_inlines = inlines.clone();
        let canonical = CanonicalDocument::from_blocks(
            crate::native_editor::codec::table_cell_blocks(&inlines),
        );
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
        let observation = cx.observe(&editor, |shell, changed, shell_cx| {
            let current = shell.table_cell_editor.as_ref().is_some_and(|cell| {
                cell.editor.entity_id() == changed.entity_id()
            });
            if !current || changed.read(shell_cx).marked_text().is_some() {
                return;
            }
            // Images the cell painted without a source of its own come from
            // the library through the session, into this editor's store.
            if let (Some(session), Some(open)) = (
                shell.note_session.clone(),
                shell
                    .table_cell_editor
                    .as_ref()
                    .map(|cell| cell.open.clone()),
            ) {
                session.update(shell_cx, |session, session_cx| {
                    session.drain_cell_image_hydration_requests(&changed, &open, session_cx);
                });
            }
            let error = shell.persist_table_cell_draft(shell_cx).err();
            if let Some(cell) = shell.table_cell_editor.as_mut() {
                if cell.error != error {
                    cell.error = error;
                    shell_cx.notify();
                }
            }
        });
        // Its own cache: the note's surface replaces the shared cache's
        // visible set and requested edges on every paint, so a cell image
        // the note's table also shows would be evicted and reloaded by each
        // surface in turn without end.
        let image_cache = crate::native_editor::images::BudgetedImageCache::new_entity_in_context(
            cx,
            CELL_IMAGE_CACHE_BUDGET,
        );
        #[cfg(test)]
        let cell_image_cache = image_cache.clone();
        let surface_editor = editor.clone();
        let surface = cx.new(move |cx| {
            let mut surface = EditorSurface::new(
                surface_editor,
                EditorSurfaceMode::Editable,
                Some(image_cache),
                cx,
            );
            // Its copy needs the note's resources: plain text alone would
            // turn an image into U+FFFC.
            surface.route_clipboard_to_owner();
            surface
        });
        let surface_events = cx.subscribe(&surface, |shell, surface, event, shell_cx| {
            if let EditorSurfaceEvent::Clipboard { cut } = event {
                shell.copy_from_table_cell(surface.entity_id(), *cut, shell_cx);
            }
        });
        self.table_cell_editor = Some(TableCellEditor {
            node_id,
            row,
            column,
            editor,
            surface,
            resources,
            original_inlines,
            _observation: observation,
            _surface_events: surface_events,
            needs_focus: true,
            error: None,
            open: std::sync::Arc::new(std::sync::atomic::AtomicBool::new(true)),
            table_revision: std::sync::Arc::new(std::sync::atomic::AtomicU64::new(revision)),
            bounds: Default::default(),
            #[cfg(test)]
            image_cache: cell_image_cache,
        });
        cx.notify();
    }

    /// Writes the cell back; with `advance`, opens the next cell.
    pub(super) fn commit_table_cell_editor(&mut self, advance: bool, cx: &mut Context<Self>) {
        let Some(cell) = self.table_cell_editor.as_ref() else {
            return;
        };
        let (node_id, row, column) = (cell.node_id, cell.row, cell.column);
        if let Err(message) = self.persist_table_cell_draft(cx) {
            if let Some(cell) = self.table_cell_editor.as_mut() {
                cell.error = Some(message);
            }
            cx.notify();
            return;
        }
        let Some(session) = self.note_session.as_ref() else {
            self.table_cell_editor = None;
            cx.notify();
            return;
        };
        let main = session.read(cx).editor().clone();
        let next: Result<_, crate::native_editor::model::DocumentError> = main.update(cx, |editor, editor_cx| {
            if advance {
                let (rows, columns) = match &editor.document().block(node_id).map(|b| &b.content) {
                    Some(BlockContent::Table(table)) => (table.rows.len(), table.column_count()),
                    _ => (0, 0),
                };
                let next = if column + 1 < columns {
                    Some((row, column + 1))
                } else if row + 1 < rows {
                    Some((row + 1, 0))
                } else {
                    editor.insert_table_row(node_id, rows)?;
                    Some((rows, 0))
                };
                editor_cx.notify();
                return Ok(next);
            }
            editor_cx.notify();
            Ok(None)
        });
        match next {
            Ok(next) => {
                let closed = self
                    .table_cell_editor
                    .take()
                    .map(|cell| cell.editor.read(cx).focus_handle().clone());
                if let Some((row, column)) = next {
                    self.open_table_cell_editor(node_id, row, column, cx);
                } else {
                    self.focus_body_after_cell_close = closed;
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
        // Autosave protects the draft while the popup is open. Explicit
        // cancellation restores the opening snapshot through the same save
        // coordinator, rather than leaving the protected draft as final text.
        if let Some(cell) = self.table_cell_editor.as_ref() {
            let (node, row, column) = (cell.node_id, cell.row, cell.column);
            let original = cell.original_inlines.clone();
            if let Some(session) = self.note_session.as_ref() {
                let main = session.read(cx).editor().clone();
                let result = main.update(cx, |editor, editor_cx| {
                    let current = editor.document().block(node).and_then(|block| match &block.content {
                        BlockContent::Table(table) => table.cell_inlines(row, column),
                        _ => None,
                    });
                    if current == Some(original.as_slice()) { return Ok(()); }
                    let result = editor.set_table_cell(node, row, column, original);
                    if result.is_ok() { editor_cx.notify(); }
                    result
                });
                if let Err(error) = result {
                    if let Some(cell) = self.table_cell_editor.as_mut() {
                        cell.error = Some(format!("无法取消单元格编辑：{error}"));
                    }
                    cx.notify();
                    return;
                }
            }
        }
        if let Some(cell) = self.table_cell_editor.take() {
            self.focus_body_after_cell_close = Some(cell.editor.read(cx).focus_handle().clone());
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
        let bounds = cell.bounds.clone();
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
                // The popup floats over the note list and body: none of its
                // presses (Done, Cancel, structure buttons, the editing
                // area) may also reach a card or the body beneath it.
                .occlude()
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
                .can_drop(|dragged, _window, _cx| dragged.is::<ExternalPaths>())
                .on_drag_move::<ExternalPaths>(cx.listener(Self::on_external_paths_drag_move))
                .on_drop::<ExternalPaths>(cx.listener(Self::on_external_paths_drop))
                .child(
                    div().h(px(160.0)).w_full().relative().child(surface).child(
                        gpui::canvas(
                            move |area, _window, _cx| bounds.set(Some(area)),
                            |_, _, _, _| {},
                        )
                        .absolute()
                        .top_0()
                        .left_0()
                        .size_full(),
                    ),
                )
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
                                .debug_selector(|| "table-cell-editor-done".to_owned())
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
                                .debug_selector(|| "table-cell-editor-cancel".to_owned())
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
                                .debug_selector(move || id.to_owned())
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
