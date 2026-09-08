//! The single owner of native-editor focus, selection, IME composition,
//! transactions, history, and document-wide commands.

use std::ops::Range;

use gpui::{
    Bounds, Context, EntityInputHandler, FocusHandle, Pixels, Point, UTF16Selection, Window,
};
use unicode_segmentation::UnicodeSegmentation;

#[cfg(test)]
use gpui::TestAppContext;

use super::history::History;
use super::input;
use super::layout::LayoutRegistry;
use super::model::{
    Affinity, Block, BlockContent, BlockKind, DocPoint, Document, DocumentError, NodeId, Selection,
    TextAlignment,
};
use super::transaction::{ApplyOutcome, Transaction, TransactionBatch};

pub struct MarkedText {
    pub node_id: NodeId,
    pub utf8_range: Range<usize>,
}

pub struct EditorCore {
    pub(crate) focus: FocusHandle,
    document: Document,
    selection: Selection,
    preferred_x: Option<Pixels>,
    marked: Option<MarkedText>,
    composition_base: Option<Selection>,
    last_input_error: Option<DocumentError>,
    history: History,
    pub(crate) layout: LayoutRegistry,
}

impl EditorCore {
    /// Construct the production editor entity. The focus handle is allocated
    /// from the entity context exactly once and is then retained by this
    /// editor for every paint/input callback.
    pub fn new(document: Document, cx: &mut Context<Self>) -> Self {
        Self::from_document_with_focus(document, cx.focus_handle())
    }

    #[cfg(test)]
    pub fn for_test(text: &str, cx: &mut gpui::TestAppContext) -> Self {
        let document = Document::from_paragraph(text);
        Self::from_document(document, cx)
    }

    #[cfg(test)]
    pub fn fixture_text_image_text(
        left: &str,
        resource_id: &str,
        right: &str,
        cx: &mut gpui::TestAppContext,
    ) -> Self {
        let mut document = Document::from_paragraph(left);
        let paragraph = document.first_node_id().expect("fixture paragraph");
        document
            .apply(Transaction::InsertImage {
                selection: document.end_selection(),
                resource_id: resource_id.to_owned(),
                natural_size: (1600, 900),
            })
            .expect("fixture image insertion");
        let right_node = document
            .blocks()
            .last()
            .expect("fixture trailing paragraph")
            .id;
        document
            .apply(Transaction::InsertText {
                selection: Selection::caret(DocPoint::new(right_node, 0)),
                text: right.to_owned(),
            })
            .expect("fixture trailing text");
        debug_assert_eq!(document.blocks()[0].id, paragraph);
        Self::from_document(document, cx)
    }

    #[cfg(test)]
    pub fn fixture_text_image_list(
        left: &str,
        resource_id: &str,
        right: &str,
        cx: &mut gpui::TestAppContext,
    ) -> Self {
        let mut document = Document::from_paragraph(left);
        document
            .apply(Transaction::InsertImage {
                selection: document.end_selection(),
                resource_id: resource_id.to_owned(),
                natural_size: (1600, 900),
            })
            .expect("fixture image insertion");
        let right_node = document
            .blocks()
            .last()
            .expect("fixture trailing paragraph")
            .id;
        document
            .apply(Transaction::InsertText {
                selection: Selection::caret(DocPoint::new(right_node, 0)),
                text: right.to_owned(),
            })
            .expect("fixture trailing text");
        let right_len = document
            .block(right_node)
            .and_then(|block| block.content.as_text())
            .map_or(0, str::len);
        document
            .apply(Transaction::SetBlockKind {
                selection: Selection::new(
                    DocPoint::with_affinity(right_node, 0, Affinity::Before),
                    DocPoint::with_affinity(right_node, right_len, Affinity::After),
                ),
                kind: BlockKind::BulletItem { depth: 0 },
            })
            .expect("fixture list conversion");
        Self::from_document(document, cx)
    }

    #[cfg(test)]
    fn from_document(document: Document, cx: &mut TestAppContext) -> Self {
        let selection = document.end_selection();
        let focus = cx.update(|app| app.focus_handle());
        Self::from_parts(document, selection, focus)
    }

    fn from_document_with_focus(document: Document, focus: FocusHandle) -> Self {
        let selection = document.end_selection();
        Self::from_parts(document, selection, focus)
    }

    fn from_parts(document: Document, selection: Selection, focus: FocusHandle) -> Self {
        Self {
            focus,
            document,
            selection,
            preferred_x: None,
            marked: None,
            composition_base: None,
            last_input_error: None,
            history: History::new(1_000, 16 * 1024 * 1024),
            layout: LayoutRegistry::new(),
        }
    }

    pub fn focus_handle(&self) -> &FocusHandle {
        &self.focus
    }

    pub fn document(&self) -> &Document {
        &self.document
    }

    pub fn selection(&self) -> Selection {
        self.selection
    }

    pub fn layout(&self) -> &LayoutRegistry {
        &self.layout
    }

    pub fn visible_text(&self) -> String {
        self.document_text()
    }

    pub fn document_len(&self) -> usize {
        self.document_text().len()
    }

    pub fn marked_text(&self) -> Option<&str> {
        let marked = self.marked.as_ref()?;
        let text = self.document.block(marked.node_id)?.content.as_text()?;
        let range = marked.utf8_range.clone();
        text.get(range)
    }

    pub fn input_error(&self) -> Option<&DocumentError> {
        self.last_input_error.as_ref()
    }

    pub fn set_caret_utf8(&mut self, offset: usize) {
        if let Some(block) = self.document.block(self.selection.head.node_id)
            && block.kind == BlockKind::Image
        {
            self.selection = Selection::caret(DocPoint::with_affinity(
                block.id,
                0,
                if offset == 0 {
                    self.selection.head.affinity
                } else {
                    Affinity::After
                },
            ));
            self.preferred_x = None;
            self.clear_composition();
            return;
        }
        let Some((node_id, text)) = self.active_text() else {
            return;
        };
        let offset = resolve_grapheme_offset(text, offset, Affinity::After);
        self.selection =
            Selection::caret(DocPoint::with_affinity(node_id, offset, Affinity::After));
        self.preferred_x = None;
        self.clear_composition();
    }

    pub fn select_document_range(&mut self, start: usize, end: usize) {
        let length = self.document_len();
        let start = start.min(length);
        let end = end.min(length);
        self.selection = if start == end {
            Selection::caret(self.point_for_document_offset_with_affinity(start, Affinity::After))
        } else {
            Selection::new(
                self.point_for_document_offset_with_affinity(start, Affinity::Before),
                self.point_for_document_offset_with_affinity(end, Affinity::After),
            )
        };
        self.preferred_x = None;
        self.clear_composition();
    }

    pub fn command_a(&mut self) {
        let Some(first) = self.document.blocks().first() else {
            return;
        };
        let Some(last) = self.document.blocks().last() else {
            return;
        };
        let (before, _) = block_points(first);
        let (_, after) = block_points(last);
        self.selection = Selection::new(before, after);
        self.preferred_x = None;
        self.clear_composition();
    }

    pub fn select_all(&mut self) {
        self.command_a();
    }

    pub fn copy_plain_text(&self) -> String {
        if self.selection.is_caret() {
            return String::new();
        }
        let text = self.document_text();
        let start = self.flat_offset_for_point(self.selection.anchor);
        let end = self.flat_offset_for_point(self.selection.head);
        let (start, end) = if start <= end {
            (start, end)
        } else {
            (end, start)
        };
        text.get(start.min(text.len())..end.min(text.len()))
            .unwrap_or_default()
            .to_owned()
    }

    pub fn cut_selection(&mut self) -> Result<String, DocumentError> {
        let copied = self.copy_plain_text();
        self.delete_selection()?;
        Ok(copied)
    }

    pub fn delete_selection(&mut self) -> Result<(), DocumentError> {
        let selection = self.selection;
        let outcome = self.apply_with_selection(Transaction::DeleteRange { selection })?;
        self.selection = outcome.selection;
        self.clear_composition();
        Ok(())
    }

    pub fn paste_plain_text(&mut self, text: &str) -> Result<(), DocumentError> {
        let outcome = self.apply_with_selection(Transaction::InsertText {
            selection: self.selection,
            text: text.to_owned(),
        })?;
        self.selection = outcome.selection;
        self.clear_composition();
        Ok(())
    }

    pub fn paste_text(&mut self, text: &str) -> Result<(), DocumentError> {
        self.paste_plain_text(text)
    }

    pub fn insert_text(&mut self, text: &str) -> Result<(), DocumentError> {
        self.paste_plain_text(text)
    }

    pub fn apply(&mut self, transaction: Transaction) -> Result<ApplyOutcome, DocumentError> {
        let outcome =
            self.history
                .apply_with_selection(&mut self.document, self.selection, transaction)?;
        self.selection = outcome.selection;
        self.preferred_x = None;
        self.clear_composition();
        self.layout.clear_exact_cache();
        Ok(outcome)
    }

    fn apply_with_selection(
        &mut self,
        transaction: Transaction,
    ) -> Result<ApplyOutcome, DocumentError> {
        let outcome =
            self.history
                .apply_with_selection(&mut self.document, self.selection, transaction)?;
        self.preferred_x = None;
        self.layout.clear_exact_cache();
        Ok(outcome)
    }

    pub fn undo(&mut self) -> Result<(), DocumentError> {
        self.selection = self.history.undo(&mut self.document)?;
        self.preferred_x = None;
        self.clear_composition();
        self.layout.clear_exact_cache();
        Ok(())
    }

    pub fn redo(&mut self) -> Result<(), DocumentError> {
        self.selection = self.history.redo(&mut self.document)?;
        self.preferred_x = None;
        self.clear_composition();
        self.layout.clear_exact_cache();
        Ok(())
    }

    pub fn undo_depth(&self) -> usize {
        self.history.undo_depth()
    }

    pub fn redo_depth(&self) -> usize {
        self.history.redo_depth()
    }

    pub fn replace_and_mark_utf16(
        &mut self,
        range_utf16: Option<Range<usize>>,
        new_text: &str,
        new_selected_range_utf16: Option<Range<usize>>,
    ) -> Result<(), DocumentError> {
        if let Some(range) = range_utf16.as_ref() {
            self.validate_input_range(range)?;
        }
        let updating_composition = self.marked.is_some();
        let visible_selection = self.selection_for_input_range(range_utf16.as_ref());
        let composition_base = self.composition_base.unwrap_or(visible_selection);
        if updating_composition {
            self.selection = self.history.undo(&mut self.document)?;
            self.layout.clear_exact_cache();
        }
        let outcome = self.apply_with_selection(Transaction::InsertText {
            selection: if updating_composition {
                composition_base
            } else {
                visible_selection
            },
            text: new_text.to_owned(),
        })?;
        let node_id = outcome.selection.head.node_id;
        let end = outcome.selection.head.utf8_offset;
        let start = end.saturating_sub(new_text.len());
        let selected_relative = new_selected_range_utf16
            .as_ref()
            .map(|range| input::utf16_range_to_utf8_in(new_text, range))
            .unwrap_or(new_text.len()..new_text.len());
        self.selection = Selection::new(
            DocPoint::with_affinity(
                node_id,
                start.saturating_add(selected_relative.start),
                Affinity::Before,
            ),
            DocPoint::with_affinity(
                node_id,
                start.saturating_add(selected_relative.end),
                Affinity::After,
            ),
        );
        self.marked = (!new_text.is_empty()).then_some(MarkedText {
            node_id,
            utf8_range: start..end,
        });
        self.composition_base = (!new_text.is_empty()).then_some(composition_base);
        Ok(())
    }

    pub fn commit_marked_text(&mut self, text: &str) -> Result<(), DocumentError> {
        let Some(marked) = self.marked.as_ref() else {
            return self.paste_plain_text(text);
        };
        let base_selection = self.composition_base.unwrap_or_else(|| {
            Selection::new(
                DocPoint::with_affinity(marked.node_id, marked.utf8_range.start, Affinity::Before),
                DocPoint::with_affinity(marked.node_id, marked.utf8_range.end, Affinity::After),
            )
        });
        self.selection = self.history.undo(&mut self.document)?;
        self.layout.clear_exact_cache();
        self.clear_composition();
        if text.is_empty() {
            return Ok(());
        }
        let outcome = self.apply_with_selection(Transaction::InsertText {
            selection: base_selection,
            text: text.to_owned(),
        })?;
        self.selection = outcome.selection;
        Ok(())
    }

    pub fn move_to_image_before(&mut self) {
        if let Some(block) = self
            .document
            .blocks()
            .iter()
            .find(|block| block.kind == BlockKind::Image)
        {
            self.selection =
                Selection::caret(DocPoint::with_affinity(block.id, 0, Affinity::Before));
            self.marked = None;
        }
    }

    pub fn move_to_image_after(&mut self) {
        if let Some(block) = self
            .document
            .blocks()
            .iter()
            .find(|block| block.kind == BlockKind::Image)
        {
            self.selection =
                Selection::caret(DocPoint::with_affinity(block.id, 0, Affinity::After));
            self.marked = None;
        }
    }

    pub fn caret_is_before_image(&self) -> bool {
        self.caret_image_affinity() == Some(Affinity::Before)
    }

    pub fn caret_is_after_image(&self) -> bool {
        self.caret_image_affinity() == Some(Affinity::After)
    }

    pub fn move_left(&mut self) {
        if !self.selection.is_caret() {
            self.selection = Selection::caret(self.ordered_selection().0);
            self.preferred_x = None;
            return;
        }
        let point = self.selection.head;
        let Some(index) = self.block_index(point.node_id) else {
            return;
        };
        let next = if self.document.blocks()[index].kind == BlockKind::Image {
            if point.affinity == Affinity::After {
                DocPoint::with_affinity(point.node_id, 0, Affinity::Before)
            } else {
                self.previous_block_point(index)
            }
        } else if point.utf8_offset > 0 {
            let text = self.document.blocks()[index]
                .content
                .as_text()
                .unwrap_or_default();
            let offset = previous_grapheme_boundary(text, point.utf8_offset);
            DocPoint::with_affinity(point.node_id, offset, Affinity::After)
        } else {
            self.previous_block_point(index)
        };
        self.selection = Selection::caret(next);
        self.preferred_x = None;
    }

    pub fn move_right(&mut self) {
        if !self.selection.is_caret() {
            self.selection = Selection::caret(self.ordered_selection().1);
            self.preferred_x = None;
            return;
        }
        let point = self.selection.head;
        let Some(index) = self.block_index(point.node_id) else {
            return;
        };
        let block = &self.document.blocks()[index];
        let next = if block.kind == BlockKind::Image {
            if point.affinity == Affinity::Before {
                DocPoint::with_affinity(point.node_id, 0, Affinity::After)
            } else {
                self.next_block_point(index)
            }
        } else {
            let text = block.content.as_text().unwrap_or_default();
            if point.utf8_offset < text.len() {
                DocPoint::with_affinity(
                    point.node_id,
                    next_grapheme_boundary(text, point.utf8_offset),
                    Affinity::After,
                )
            } else {
                self.next_block_point(index)
            }
        };
        self.selection = Selection::caret(next);
        self.preferred_x = None;
    }

    pub fn move_home(&mut self) {
        if !self.selection.is_caret() {
            self.selection = Selection::caret(self.ordered_selection().0);
        }
        let point = self.selection.head;
        if let Some(boundary) = self.layout.visual_line_boundary(point, false) {
            self.selection = Selection::caret(boundary);
        } else if let Some(block) = self.document.block(point.node_id) {
            if block.kind == BlockKind::Image {
                self.selection =
                    Selection::caret(DocPoint::with_affinity(block.id, 0, Affinity::Before));
            } else if let Some(text) = block.content.as_text() {
                self.selection = Selection::caret(DocPoint::with_affinity(
                    block.id,
                    resolve_grapheme_offset(text, 0, Affinity::Before),
                    Affinity::Before,
                ));
            }
        }
        self.preferred_x = None;
        self.marked = None;
    }

    pub fn move_end(&mut self) {
        if !self.selection.is_caret() {
            self.selection = Selection::caret(self.ordered_selection().1);
        }
        let point = self.selection.head;
        if let Some(boundary) = self.layout.visual_line_boundary(point, true) {
            self.selection = Selection::caret(boundary);
        } else if let Some(block) = self.document.block(point.node_id) {
            if block.kind == BlockKind::Image {
                self.selection =
                    Selection::caret(DocPoint::with_affinity(block.id, 0, Affinity::After));
            } else if let Some(text) = block.content.as_text() {
                self.selection = Selection::caret(DocPoint::with_affinity(
                    block.id,
                    text.len(),
                    Affinity::After,
                ));
            }
        }
        self.preferred_x = None;
        self.marked = None;
    }

    pub fn home(&mut self) {
        self.move_home();
    }

    pub fn end(&mut self) {
        self.move_end();
    }

    pub fn move_up(&mut self) {
        self.move_vertical(-1);
    }

    pub fn move_down(&mut self) {
        self.move_vertical(1);
    }

    fn move_vertical(&mut self, direction: isize) {
        if !self.selection.is_caret() {
            self.selection = Selection::caret(if direction < 0 {
                self.ordered_selection().0
            } else {
                self.ordered_selection().1
            });
            self.preferred_x = None;
            return;
        }
        let point = self.selection.head;
        let preferred_x = self.preferred_x.or_else(|| self.layout.caret_x(point));
        if let Some(target) = self.layout.visual_move(point, direction, self.preferred_x) {
            let x = self.preferred_x.or_else(|| self.layout.caret_x(point));
            self.selection = Selection::caret(target);
            self.preferred_x = x;
            return;
        }
        let Some(index) = self.block_index(point.node_id) else {
            return;
        };
        let target = if direction < 0 {
            (0..index).rev().find_map(|candidate| {
                let block = self.document.blocks().get(candidate)?;
                self.layout
                    .visual_edge_point(block.id, preferred_x, true)
                    .or_else(|| self.text_point_at_index(candidate, point.utf8_offset))
            })
        } else {
            ((index + 1)..self.document.block_count()).find_map(|candidate| {
                let block = self.document.blocks().get(candidate)?;
                self.layout
                    .visual_edge_point(block.id, preferred_x, false)
                    .or_else(|| self.text_point_at_index(candidate, point.utf8_offset))
            })
        };
        if let Some(target) = target {
            self.selection = Selection::caret(target);
            self.preferred_x = preferred_x;
        }
    }

    fn text_point_at_index(&self, index: usize, offset: usize) -> Option<DocPoint> {
        let block = self.document.blocks().get(index)?;
        let text = block.content.as_text()?;
        Some(DocPoint::with_affinity(
            block.id,
            nearest_grapheme_boundary(text, offset),
            Affinity::After,
        ))
    }

    pub fn insert_paragraph_break(&mut self) -> Result<(), DocumentError> {
        let selection = self.selection;
        let point = if selection.is_caret() {
            selection.head
        } else {
            self.ordered_selection().0
        };
        let Some(index) = self.block_index(point.node_id) else {
            return Err(DocumentError::NodeNotFound(point.node_id));
        };
        let at = if self.document.blocks()[index].kind == BlockKind::Image {
            if point.affinity == Affinity::Before {
                self.previous_text_point(index)
            } else {
                self.next_text_point(index)
            }
        } else {
            point
        };
        let mut transactions = Vec::with_capacity(2);
        if !selection.is_caret() {
            transactions.push(Transaction::DeleteRange { selection });
        }
        transactions.push(Transaction::SplitBlock { at });
        let outcome = self.history.apply_batch_with_selection(
            &mut self.document,
            selection,
            TransactionBatch(transactions),
        )?;
        self.selection = outcome.selection;
        self.preferred_x = None;
        self.clear_composition();
        self.layout.clear_exact_cache();
        Ok(())
    }

    pub fn backspace(&mut self) -> Result<(), DocumentError> {
        if !self.selection.is_caret() {
            return self.delete_selection();
        }
        let point = self.selection.head;
        let Some(index) = self.block_index(point.node_id) else {
            return Ok(());
        };
        if self.document.blocks()[index].kind == BlockKind::Image {
            if point.affinity == Affinity::After {
                let outcome = self.apply_with_selection(Transaction::RemoveNode {
                    node_id: point.node_id,
                })?;
                self.selection = outcome.selection;
            }
            return Ok(());
        }
        let text = self.document.blocks()[index]
            .content
            .as_text()
            .unwrap_or_default();
        if point.utf8_offset > 0 {
            let start = previous_grapheme_boundary(text, point.utf8_offset);
            let selection = Selection::new(
                DocPoint::with_affinity(point.node_id, start, Affinity::Before),
                DocPoint::with_affinity(point.node_id, point.utf8_offset, Affinity::After),
            );
            let outcome = self.apply_with_selection(Transaction::DeleteRange { selection })?;
            self.selection = outcome.selection;
        } else if let Some(previous) = self.document.blocks().get(index.saturating_sub(1)) {
            if previous.kind == BlockKind::Image {
                let outcome = self.apply_with_selection(Transaction::RemoveNode {
                    node_id: previous.id,
                })?;
                self.selection = outcome.selection;
            } else if previous.content.as_text().is_some() {
                let current = &self.document.blocks()[index];
                if current.kind != BlockKind::Paragraph && previous.kind == BlockKind::Paragraph {
                    // Match the donor's list/heading boundary behavior:
                    // Backspace at the start downgrades the current structural
                    // block instead of silently stalling.
                    let selection = self.full_block_selection(index);
                    let mut transactions = vec![Transaction::SetBlockKind {
                        selection,
                        kind: BlockKind::Paragraph,
                    }];
                    if current.alignment != TextAlignment::Left {
                        transactions.push(Transaction::SetAlignment {
                            selection,
                            alignment: TextAlignment::Left,
                        });
                    }
                    let outcome = self.history.apply_batch_with_selection(
                        &mut self.document,
                        self.selection,
                        TransactionBatch(transactions),
                    )?;
                    self.selection = outcome.selection;
                    self.preferred_x = None;
                    self.layout.clear_exact_cache();
                } else {
                    let selection = self.full_block_selection(index);
                    let mut transactions = Vec::new();
                    if current.kind != previous.kind {
                        transactions.push(Transaction::SetBlockKind {
                            selection,
                            kind: previous.kind.clone(),
                        });
                    }
                    if current.alignment != previous.alignment {
                        transactions.push(Transaction::SetAlignment {
                            selection,
                            alignment: previous.alignment,
                        });
                    }
                    transactions.push(Transaction::MergeBlocks {
                        left: previous.id,
                        right: point.node_id,
                    });
                    let outcome = self.history.apply_batch_with_selection(
                        &mut self.document,
                        self.selection,
                        TransactionBatch(transactions),
                    )?;
                    self.selection = outcome.selection;
                    self.preferred_x = None;
                    self.layout.clear_exact_cache();
                }
            }
        }
        Ok(())
    }

    pub fn delete_forward(&mut self) -> Result<(), DocumentError> {
        if !self.selection.is_caret() {
            return self.delete_selection();
        }
        let point = self.selection.head;
        let Some(index) = self.block_index(point.node_id) else {
            return Ok(());
        };
        if self.document.blocks()[index].kind == BlockKind::Image {
            if point.affinity == Affinity::Before {
                let outcome = self.apply_with_selection(Transaction::RemoveNode {
                    node_id: point.node_id,
                })?;
                self.selection = outcome.selection;
            }
            return Ok(());
        }
        let text = self.document.blocks()[index]
            .content
            .as_text()
            .unwrap_or_default();
        if point.utf8_offset < text.len() {
            let end = next_grapheme_boundary(text, point.utf8_offset);
            let selection = Selection::new(
                DocPoint::with_affinity(point.node_id, point.utf8_offset, Affinity::Before),
                DocPoint::with_affinity(point.node_id, end, Affinity::After),
            );
            let outcome = self.apply_with_selection(Transaction::DeleteRange { selection })?;
            self.selection = outcome.selection;
        } else if let Some(next) = self.document.blocks().get(index + 1) {
            if next.kind == BlockKind::Image {
                let outcome =
                    self.apply_with_selection(Transaction::RemoveNode { node_id: next.id })?;
                self.selection = outcome.selection;
            } else if next.content.as_text().is_some() {
                let current = &self.document.blocks()[index];
                let selection = self.full_block_selection(index + 1);
                let mut transactions = Vec::new();
                if next.kind != current.kind {
                    transactions.push(Transaction::SetBlockKind {
                        selection,
                        kind: current.kind.clone(),
                    });
                }
                if next.alignment != current.alignment {
                    transactions.push(Transaction::SetAlignment {
                        selection,
                        alignment: current.alignment,
                    });
                }
                transactions.push(Transaction::MergeBlocks {
                    left: point.node_id,
                    right: next.id,
                });
                let outcome = self.history.apply_batch_with_selection(
                    &mut self.document,
                    self.selection,
                    TransactionBatch(transactions),
                )?;
                self.selection = outcome.selection;
                self.preferred_x = None;
                self.layout.clear_exact_cache();
            }
        }
        Ok(())
    }

    fn document_text(&self) -> String {
        let mut result = String::new();
        for (index, block) in self.document.blocks().iter().enumerate() {
            if index > 0 {
                result.push('\n');
            }
            match &block.content {
                BlockContent::Text { text, .. } => result.push_str(text),
                BlockContent::Image { .. }
                | BlockContent::Attachment { .. }
                | BlockContent::Empty => result.push('\u{fffc}'),
            }
        }
        result
    }

    fn clear_composition(&mut self) {
        self.marked = None;
        self.composition_base = None;
    }

    fn active_text(&self) -> Option<(NodeId, &str)> {
        self.document
            .block(self.selection.head.node_id)
            .and_then(|block| block.content.as_text().map(|text| (block.id, text)))
            .or_else(|| {
                self.document
                    .blocks()
                    .iter()
                    .find_map(|block| block.content.as_text().map(|text| (block.id, text)))
            })
    }

    fn selection_for_input_range(&self, range_utf16: Option<&Range<usize>>) -> Selection {
        if let Some(range_utf16) = range_utf16 {
            let document_text = self.document_text();
            let range = input::utf16_range_to_utf8_in(&document_text, range_utf16);
            return self.selection_for_document_byte_range(range);
        }
        if let Some(marked) = self.marked.as_ref() {
            return Selection::new(
                DocPoint::with_affinity(marked.node_id, marked.utf8_range.start, Affinity::Before),
                DocPoint::with_affinity(marked.node_id, marked.utf8_range.end, Affinity::After),
            );
        }
        self.selection
    }

    fn validate_input_range(&self, range_utf16: &Range<usize>) -> Result<(), DocumentError> {
        let text = self.document_text();
        let document_len = input::utf8_range_to_utf16_in(&text, &(0..text.len())).end;
        if range_utf16.start > range_utf16.end || range_utf16.end > document_len {
            return Err(DocumentError::InvalidOperation(
                "input UTF-16 range is outside the document".into(),
            ));
        }
        Ok(())
    }

    fn ordered_selection(&self) -> (DocPoint, DocPoint) {
        let anchor_index = self.block_index(self.selection.anchor.node_id).unwrap_or(0);
        let head_index = self.block_index(self.selection.head.node_id).unwrap_or(0);
        let anchor = (anchor_index, self.selection.anchor.utf8_offset);
        let head = (head_index, self.selection.head.utf8_offset);
        if anchor <= head {
            (self.selection.anchor, self.selection.head)
        } else {
            (self.selection.head, self.selection.anchor)
        }
    }

    fn block_index(&self, node_id: NodeId) -> Option<usize> {
        self.document
            .blocks()
            .iter()
            .position(|block| block.id == node_id)
    }

    fn full_block_selection(&self, index: usize) -> Selection {
        let block = &self.document.blocks()[index];
        let (before, after) = block_points(block);
        Selection::new(before, after)
    }

    fn caret_image_affinity(&self) -> Option<Affinity> {
        if !self.selection.is_caret() {
            return None;
        }
        let block = self.document.block(self.selection.head.node_id)?;
        (block.kind == BlockKind::Image).then_some(self.selection.head.affinity)
    }

    fn previous_block_point(&self, index: usize) -> DocPoint {
        for candidate in self.document.blocks()[..index].iter().rev() {
            if candidate.kind == BlockKind::Image {
                return DocPoint::with_affinity(candidate.id, 0, Affinity::After);
            }
            if let Some(text) = candidate.content.as_text() {
                return DocPoint::with_affinity(candidate.id, text.len(), Affinity::After);
            }
        }
        self.selection.head
    }

    fn next_block_point(&self, index: usize) -> DocPoint {
        for candidate in self.document.blocks().iter().skip(index + 1) {
            if candidate.kind == BlockKind::Image {
                return DocPoint::with_affinity(candidate.id, 0, Affinity::Before);
            }
            if candidate.content.as_text().is_some() {
                return DocPoint::with_affinity(candidate.id, 0, Affinity::Before);
            }
        }
        self.selection.head
    }

    fn previous_text_point(&self, index: usize) -> DocPoint {
        self.document.blocks()[..index]
            .iter()
            .rev()
            .find_map(|block| {
                block
                    .content
                    .as_text()
                    .map(|text| DocPoint::with_affinity(block.id, text.len(), Affinity::After))
            })
            .unwrap_or(self.selection.head)
    }

    fn next_text_point(&self, index: usize) -> DocPoint {
        self.document
            .blocks()
            .iter()
            .skip(index + 1)
            .find_map(|block| {
                block
                    .content
                    .as_text()
                    .map(|_| DocPoint::with_affinity(block.id, 0, Affinity::Before))
            })
            .unwrap_or(self.selection.head)
    }

    fn flat_offset_for_point(&self, point: DocPoint) -> usize {
        let mut offset = 0;
        for (index, block) in self.document.blocks().iter().enumerate() {
            if index > 0 {
                offset += 1;
            }
            if block.id != point.node_id {
                offset += block_flat_len(block);
                continue;
            }
            return offset
                + match &block.content {
                    BlockContent::Text { text, .. } => point.utf8_offset.min(text.len()),
                    _ => usize::from(point.affinity == Affinity::After) * block_flat_len(block),
                };
        }
        offset
    }

    fn selection_for_document_byte_range(&self, range: Range<usize>) -> Selection {
        if range.start == range.end {
            Selection::caret(
                self.point_for_document_offset_with_affinity(range.start, Affinity::After),
            )
        } else {
            Selection::new(
                self.point_for_document_offset_with_affinity(range.start, Affinity::Before),
                self.point_for_document_offset_with_affinity(range.end, Affinity::After),
            )
        }
    }

    fn point_for_document_offset_with_affinity(
        &self,
        offset: usize,
        affinity: Affinity,
    ) -> DocPoint {
        if let Some(first) = self.document.blocks().first() {
            if offset == 0 {
                return DocPoint::with_affinity(first.id, 0, Affinity::Before);
            }
        }
        let full_len = self.document_text().len();
        if offset >= full_len {
            return self
                .document
                .blocks()
                .last()
                .map(block_points)
                .map(|(_, after)| after)
                .unwrap_or_else(|| DocPoint::new(NodeId::new(0), 0));
        }
        let mut cursor = 0;
        for (index, block) in self.document.blocks().iter().enumerate() {
            if index > 0 {
                cursor += 1;
            }
            let len = block_flat_len(block);
            if offset <= cursor + len {
                match &block.content {
                    BlockContent::Text { text, .. } => {
                        let local = resolve_grapheme_offset(
                            text,
                            (offset - cursor).min(text.len()),
                            affinity,
                        );
                        return DocPoint::with_affinity(block.id, local, affinity);
                    }
                    _ => {
                        return DocPoint::with_affinity(
                            block.id,
                            0,
                            if offset - cursor < len / 2 {
                                Affinity::Before
                            } else {
                                Affinity::After
                            },
                        );
                    }
                }
            }
            cursor += len;
        }
        self.document.end_selection().head
    }
}

impl EntityInputHandler for EditorCore {
    fn text_for_range(
        &mut self,
        range_utf16: Range<usize>,
        actual_range: &mut Option<Range<usize>>,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<String> {
        let document_text = self.document_text();
        let range = input::utf16_range_to_utf8_in(&document_text, &range_utf16);
        actual_range.replace(input::utf8_range_to_utf16_in(&document_text, &range));
        Some(document_text.get(range)?.to_owned())
    }

    fn selected_text_range(
        &mut self,
        _ignore_disabled_input: bool,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<UTF16Selection> {
        let document_text = self.document_text();
        let anchor = self.flat_offset_for_point(self.selection.anchor);
        let head = self.flat_offset_for_point(self.selection.head);
        let (start, end, reversed) = if anchor <= head {
            (anchor, head, false)
        } else {
            (head, anchor, true)
        };
        let range = input::utf8_range_to_utf16_in(&document_text, &(start..end));
        Some(UTF16Selection { range, reversed })
    }

    fn marked_text_range(
        &self,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<Range<usize>> {
        let marked = self.marked.as_ref()?;
        let document_text = self.document_text();
        let start = self.flat_offset_for_point(DocPoint::with_affinity(
            marked.node_id,
            marked.utf8_range.start,
            Affinity::Before,
        ));
        let end = self.flat_offset_for_point(DocPoint::with_affinity(
            marked.node_id,
            marked.utf8_range.end,
            Affinity::After,
        ));
        Some(input::utf8_range_to_utf16_in(&document_text, &(start..end)))
    }

    fn unmark_text(&mut self, _window: &mut Window, _cx: &mut Context<Self>) {
        self.marked = None;
        self.composition_base = None;
    }

    fn replace_text_in_range(
        &mut self,
        range_utf16: Option<Range<usize>>,
        new_text: &str,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(range) = range_utf16.as_ref()
            && let Err(error) = self.validate_input_range(range)
        {
            self.last_input_error = Some(error);
            cx.notify();
            return;
        }
        let selection = self.selection_for_input_range(range_utf16.as_ref());
        match self.apply_with_selection(Transaction::InsertText {
            selection,
            text: new_text.to_owned(),
        }) {
            Ok(outcome) => {
                self.selection = outcome.selection;
                self.clear_composition();
                self.last_input_error = None;
                cx.notify();
            }
            Err(error) => {
                self.last_input_error = Some(error);
                cx.notify();
            }
        }
    }

    fn replace_and_mark_text_in_range(
        &mut self,
        range_utf16: Option<Range<usize>>,
        new_text: &str,
        new_selected_range_utf16: Option<Range<usize>>,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match self.replace_and_mark_utf16(range_utf16, new_text, new_selected_range_utf16) {
            Ok(()) => {
                self.last_input_error = None;
                cx.notify();
            }
            Err(error) => {
                self.last_input_error = Some(error);
                cx.notify();
            }
        }
    }

    fn bounds_for_range(
        &mut self,
        range_utf16: Range<usize>,
        _bounds: Bounds<Pixels>,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<Bounds<Pixels>> {
        let document_text = self.document_text();
        let range = input::utf16_range_to_utf8_in(&document_text, &range_utf16);
        let selection = self.selection_for_document_byte_range(range);
        self.layout
            .selection_rects(selection)
            .into_iter()
            .reduce(union_bounds)
    }

    fn character_index_for_point(
        &mut self,
        point: Point<Pixels>,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<usize> {
        let point = self.layout.point_to_doc(point)?;
        let document_text = self.document_text();
        Some(input::utf8_to_utf16_in(
            &document_text,
            self.flat_offset_for_point(point),
        ))
    }
}

fn block_points(block: &Block) -> (DocPoint, DocPoint) {
    match &block.content {
        BlockContent::Text { text, .. } => (
            DocPoint::with_affinity(block.id, 0, Affinity::Before),
            DocPoint::with_affinity(block.id, text.len(), Affinity::After),
        ),
        _ => (
            DocPoint::with_affinity(block.id, 0, Affinity::Before),
            DocPoint::with_affinity(block.id, 0, Affinity::After),
        ),
    }
}

fn block_flat_len(block: &Block) -> usize {
    match &block.content {
        BlockContent::Text { text, .. } => text.len(),
        _ => '\u{fffc}'.len_utf8(),
    }
}

fn union_bounds(a: Bounds<Pixels>, b: Bounds<Pixels>) -> Bounds<Pixels> {
    Bounds::from_corners(
        Point::new(a.left().min(b.left()), a.top().min(b.top())),
        Point::new(a.right().max(b.right()), a.bottom().max(b.bottom())),
    )
}

fn previous_grapheme_boundary(text: &str, offset: usize) -> usize {
    text.grapheme_indices(true)
        .map(|(start, _)| start)
        .take_while(|start| *start < offset)
        .last()
        .unwrap_or(0)
}

fn nearest_grapheme_boundary(text: &str, offset: usize) -> usize {
    let offset = offset.min(text.len());
    if offset == text.len() {
        return offset;
    }
    text.grapheme_indices(true)
        .map(|(start, _)| start)
        .take_while(|start| *start <= offset)
        .last()
        .unwrap_or(0)
}

fn next_grapheme_boundary(text: &str, offset: usize) -> usize {
    text.grapheme_indices(true)
        .map(|(start, _)| start)
        .find(|start| *start > offset)
        .unwrap_or(text.len())
}

fn resolve_grapheme_offset(text: &str, preferred: usize, affinity: Affinity) -> usize {
    let preferred = preferred.min(text.len());
    if preferred == 0 || preferred == text.len() {
        return preferred;
    }
    if text
        .grapheme_indices(true)
        .any(|(start, _)| start == preferred)
    {
        return preferred;
    }
    let mut previous = 0;
    for (start, _) in text.grapheme_indices(true) {
        if start >= preferred {
            return match affinity {
                Affinity::Before => previous,
                Affinity::After => start,
            };
        }
        previous = start;
    }
    match affinity {
        Affinity::Before => previous,
        Affinity::After => text.len(),
    }
}
