//! The single owner of native-editor focus, selection, IME composition,
//! transactions, history, and document-wide commands.

use std::io::Cursor;
use std::ops::Range;
use std::path::Path;

use gpui::{
    Bounds, Context, EntityInputHandler, FocusHandle, ImageFormat, Pixels, Point, UTF16Selection,
    Window,
};
use image::ImageReader;
use unicode_segmentation::UnicodeSegmentation;
use usvg::{Options, Tree};
use uuid::Uuid;

#[cfg(test)]
use gpui::TestAppContext;

use super::history::History;
use super::images::{ImageMetadata, ImagePayload, ImageStore, image_format_from_path};
use super::input;
use super::layout::LayoutRegistry;
use super::model::{
    Affinity, Block, BlockContent, BlockKind, DocPoint, Document, DocumentError, NodeId, Selection,
    TextAlignment, insertion_marks,
};
use super::transaction::{ApplyOutcome, Transaction, TransactionBatch};

fn image_dimensions(payload: &ImagePayload) -> Option<(u32, u32)> {
    if payload.format == gpui::ImageFormat::Svg {
        let tree = Tree::from_data(&payload.bytes, &Options::default()).ok()?;
        let size = tree.size();
        return Some((size.width().ceil() as u32, size.height().ceil() as u32));
    }
    let reader = ImageReader::new(Cursor::new(&payload.bytes))
        .with_guessed_format()
        .ok()?;
    let (width, height) = reader.into_dimensions().ok()?;
    Some((width, height))
}

fn image_dimensions_from_path(path: &Path, format: ImageFormat) -> Option<(u32, u32)> {
    if format == ImageFormat::Svg {
        let bytes = std::fs::read(path).ok()?;
        let tree = Tree::from_data(&bytes, &Options::default()).ok()?;
        let size = tree.size();
        return Some((size.width().ceil() as u32, size.height().ceil() as u32));
    }
    let reader = ImageReader::open(path).ok()?.with_guessed_format().ok()?;
    let dimensions = reader.into_dimensions().ok()?;
    #[cfg(target_os = "macos")]
    let orientation = image_orientation_from_path(path).unwrap_or(1);
    #[cfg(not(target_os = "macos"))]
    let orientation = 1;
    Some(oriented_dimensions(dimensions, orientation))
}

fn oriented_dimensions((width, height): (u32, u32), orientation: u32) -> (u32, u32) {
    if (5..=8).contains(&orientation) {
        (height, width)
    } else {
        (width, height)
    }
}

#[cfg(target_os = "macos")]
fn image_orientation_from_path(path: &Path) -> Option<u32> {
    use objc2_core_foundation::{CFDictionary, CFNumber, CFString, CFType, CFURL};
    use objc2_image_io::{CGImageSource, kCGImagePropertyOrientation};
    let url = CFURL::from_file_path(path)?;
    let source = unsafe { CGImageSource::with_url(&url, None) }?;
    let properties = unsafe { source.properties_at_index(0, None) }?;
    let properties: &CFDictionary<CFString, CFType> = unsafe {
        &*(properties.as_ref() as *const CFDictionary as *const CFDictionary<CFString, CFType>)
    };
    properties
        .get(unsafe { kCGImagePropertyOrientation })
        .and_then(|value| value.downcast_ref::<CFNumber>().and_then(CFNumber::as_i32))
        .map(|value| value as u32)
}

#[derive(Clone)]
pub struct MarkedText {
    pub node_id: NodeId,
    /// The grapheme-expanded span exposed to the platform as the marked
    /// range. This is deliberately separate from the provisional bytes that
    /// the IME transaction actually inserted.
    pub utf8_range: Range<usize>,
    /// The exact candidate interval in the candidate-containing document.
    /// Combining marks can make this narrower than `utf8_range`; replacement
    /// and inverse-history remapping must use this interval, never the public
    /// grapheme-safe span.
    pub actual_utf8_range: Range<usize>,
}

/// A document-flat UTF-8 range whose endpoints have not yet been expanded to
/// model grapheme boundaries. Platform input ranges use this representation
/// until candidate coordinates have been inverse-mapped to the base document.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct RawDocumentRange {
    start: usize,
    end: usize,
    start_affinity: Affinity,
    end_affinity: Affinity,
}

impl RawDocumentRange {
    fn from_utf8(range: Range<usize>) -> Self {
        let (start_affinity, end_affinity) = if range.start == range.end {
            (Affinity::After, Affinity::After)
        } else {
            (Affinity::Before, Affinity::After)
        };
        Self {
            start: range.start,
            end: range.end,
            start_affinity,
            end_affinity,
        }
    }

    #[cfg(test)]
    fn from_utf16(text: &str, range: &Range<usize>) -> Self {
        Self::from_utf8(input::utf16_range_to_utf8_in(text, range))
    }

    fn with_affinities(
        start: usize,
        end: usize,
        start_affinity: Affinity,
        end_affinity: Affinity,
    ) -> Self {
        Self {
            start,
            end,
            start_affinity,
            end_affinity,
        }
    }

    fn offsets(self) -> ((usize, Affinity), (usize, Affinity)) {
        (
            (self.start, self.start_affinity),
            (self.end, self.end_affinity),
        )
    }

    fn as_range(self) -> Range<usize> {
        self.start..self.end
    }
}

pub struct EditorCore {
    pub(crate) focus: FocusHandle,
    access: EditorAccess,
    document: Document,
    selection: Selection,
    preferred_x: Option<Pixels>,
    marked: Option<MarkedText>,
    composition_base: Option<Selection>,
    /// Document-wide UTF-8 range in the base document before the current
    /// provisional marked text was inserted.  GPUI may report the next IME
    /// range in the candidate-containing document, so the original range
    /// must not be recomputed from those candidate coordinates.
    composition_base_range: Option<Range<usize>>,
    last_input_error: Option<DocumentError>,
    history: History,
    image_store: ImageStore,
    pub(crate) layout: LayoutRegistry,
    layout_offset: (f32, f32),
    #[cfg(test)]
    shape_calls: usize,
}

/// Capability boundary for the single editor core.  The default constructor
/// remains editable for the performance spike; the real library uses
/// `ReadOnly` until Task 4 can durably save every mutation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EditorAccess {
    Editable,
    ReadOnly,
}

impl EditorCore {
    /// Construct the production editor entity. The focus handle is allocated
    /// from the entity context exactly once and is then retained by this
    /// editor for every paint/input callback.
    pub fn new(document: Document, cx: &mut Context<Self>) -> Self {
        Self::from_document_with_focus(document, cx.focus_handle(), EditorAccess::Editable)
    }

    pub fn new_read_only(document: Document, cx: &mut Context<Self>) -> Self {
        Self::from_document_with_focus(document, cx.focus_handle(), EditorAccess::ReadOnly)
    }

    #[cfg(test)]
    pub fn for_test(text: &str, cx: &mut gpui::TestAppContext) -> Self {
        let document = Document::from_paragraph(text);
        Self::from_document(document, cx)
    }

    #[cfg(test)]
    pub fn for_test_paragraphs<I, S>(paragraphs: I, cx: &mut gpui::TestAppContext) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        Self::from_document(Document::from_paragraphs(paragraphs), cx)
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
        Self::from_parts(document, selection, focus, EditorAccess::Editable)
    }

    fn from_document_with_focus(
        document: Document,
        focus: FocusHandle,
        access: EditorAccess,
    ) -> Self {
        let selection = document.end_selection();
        Self::from_parts(document, selection, focus, access)
    }

    fn from_parts(
        document: Document,
        selection: Selection,
        focus: FocusHandle,
        access: EditorAccess,
    ) -> Self {
        Self {
            focus,
            access,
            document,
            selection,
            preferred_x: None,
            marked: None,
            composition_base: None,
            composition_base_range: None,
            last_input_error: None,
            history: History::new(1_000, 16 * 1024 * 1024),
            image_store: ImageStore::default(),
            layout: LayoutRegistry::new(),
            layout_offset: (0.0, 0.0),
            #[cfg(test)]
            shape_calls: 0,
        }
    }

    pub fn focus_handle(&self) -> &FocusHandle {
        &self.focus
    }

    pub fn access(&self) -> EditorAccess {
        self.access
    }

    pub fn is_read_only(&self) -> bool {
        self.access == EditorAccess::ReadOnly
    }

    fn ensure_editable(&self) -> Result<(), DocumentError> {
        if self.is_read_only() {
            Err(DocumentError::ReadOnly)
        } else {
            Ok(())
        }
    }

    pub fn document(&self) -> &Document {
        &self.document
    }

    pub fn selection(&self) -> Selection {
        self.selection
    }

    pub fn image_metadata(&self, resource_id: &str) -> Option<&ImageMetadata> {
        self.image_store.metadata_for_resource(resource_id)
    }

    pub fn image_bytes(&self, resource_id: &str) -> Option<&[u8]> {
        self.image_store.compressed_for_resource(resource_id)
    }

    #[cfg(test)]
    pub(crate) fn image_resource_count_for_test(&self) -> usize {
        self.image_store.resource_count_for_test()
    }

    pub fn image_state(&self, resource_id: &str) -> Option<super::images::ImageNodeState> {
        self.image_store
            .id_for_resource(resource_id)
            .map(|id| self.image_store.node_state(id))
    }

    pub fn retry_image_resource(&mut self, resource_id: &str) -> bool {
        self.image_store.retry_resource(resource_id)
    }

    pub fn image_source_path(&self, resource_id: &str) -> Option<&std::path::Path> {
        self.image_store.source_path_for_resource(resource_id)
    }

    pub fn mark_image_loaded(&mut self, resource_id: &str) -> bool {
        self.image_store.finish_loaded_resource(resource_id)
    }

    pub fn mark_image_failed(&mut self, resource_id: &str) -> bool {
        self.image_store.finish_failed_resource(resource_id)
    }

    /// Return the document-order block interval covered by the active
    /// selection. Command state and the native toolbar use this same helper so
    /// they cannot accidentally maintain a second selection interpretation.
    pub(crate) fn selected_block_indices(&self) -> Option<(usize, usize)> {
        let (start, end) = self.ordered_selection();
        Some((
            self.block_index(start.node_id)?,
            self.block_index(end.node_id)?,
        ))
    }

    /// Return the non-empty text spans covered by the active selection. Image
    /// atoms are intentionally omitted: text commands must not pretend that a
    /// structural image selection is editable text.
    pub(crate) fn selected_text_ranges(&self) -> Vec<(NodeId, Range<usize>)> {
        let Some((start_index, end_index)) = self.selected_block_indices() else {
            return Vec::new();
        };
        let (start, end) = self.ordered_selection();
        let mut ranges = Vec::new();
        for index in start_index..=end_index {
            let Some(block) = self.document.blocks().get(index) else {
                continue;
            };
            let Some(text) = block.content.as_text() else {
                continue;
            };
            let range_start = if index == start_index {
                start.utf8_offset.min(text.len())
            } else {
                0
            };
            let range_end = if index == end_index {
                end.utf8_offset.min(text.len())
            } else {
                text.len()
            };
            if range_start < range_end {
                ranges.push((block.id, range_start..range_end));
            }
        }
        ranges
    }

    /// Query whether a mark is present in any and all of the selected text.
    /// The tuple is `(any, all)`; an empty selection uses the mark at the
    /// caret, preserving the familiar toolbar state while still allowing the
    /// command catalogue to disable operations that would otherwise be no-op.
    pub(crate) fn selection_mark_state(&self, mark: &super::model::Mark) -> (bool, bool) {
        let ranges = self.selected_text_ranges();
        if !ranges.is_empty() {
            let mut any = false;
            let mut all = true;
            for (node_id, range) in ranges {
                let Some(block) = self.document.block(node_id) else {
                    all = false;
                    continue;
                };
                let Some(styles) = block.content.styles() else {
                    all = false;
                    continue;
                };
                let (range_any, range_all) = mark_state_for_range(styles, range, mark);
                any |= range_any;
                all &= range_all;
            }
            return (any, all);
        }

        let Some(block) = self.document.block(self.selection.head.node_id) else {
            return (false, false);
        };
        let Some(styles) = block.content.styles() else {
            return (false, false);
        };
        let inherited = insertion_marks(
            styles,
            self.selection.head.utf8_offset,
            self.selection.head.affinity,
        );
        let has_mark = contains_mark(&inherited, mark);
        (has_mark, has_mark)
    }

    #[cfg(test)]
    pub(crate) fn set_selection_for_test(&mut self, selection: Selection) {
        self.selection = selection;
        self.preferred_x = None;
        self.clear_composition();
    }

    pub fn layout(&self) -> &LayoutRegistry {
        &self.layout
    }

    pub fn history_used_bytes(&self) -> usize {
        self.history.used_bytes()
    }

    pub fn layout_peak_accounted_bytes(&self) -> usize {
        self.layout.peak_accounted_bytes()
    }

    pub(crate) fn select_for_workload(&mut self, point: DocPoint) {
        self.selection = Selection::caret(point);
        self.preferred_x = None;
        self.clear_composition();
    }

    /// Shape the actual document through the donor-backed LayoutRegistry for
    /// the native spike window. Keeping this adapter on EditorCore preserves
    /// one document owner while allowing the GPUI view to remain a thin
    /// render/input bridge.
    pub(crate) fn shape_visible_with_window(
        &mut self,
        viewport_top: f32,
        viewport_height: f32,
        width: f32,
        window: &mut Window,
    ) {
        #[cfg(test)]
        {
            self.shape_calls += 1;
        }
        self.clear_layout_translation();
        let document = &self.document;
        self.layout.shape_visible_with_window(
            document,
            viewport_top,
            viewport_height,
            width,
            window,
        );
    }

    #[cfg(test)]
    pub(crate) fn shape_calls_for_test(&self) -> usize {
        self.shape_calls
    }

    pub(crate) fn translate_layout(&mut self, offset_x: f32, offset_y: f32) {
        let offset_x_pixels = gpui::px(offset_x);
        let offset_y_pixels = gpui::px(offset_y);
        for layout in &mut self.layout.visible {
            layout.bounds.origin.x += offset_x_pixels;
            layout.bounds.origin.y += offset_y_pixels;
        }
        for cached in self.layout.cache.values_mut() {
            cached.layout.bounds.origin.x += offset_x_pixels;
            cached.layout.bounds.origin.y += offset_y_pixels;
        }
        self.layout_offset = (offset_x, offset_y);
    }

    fn clear_layout_translation(&mut self) {
        let (offset_x, offset_y) = self.layout_offset;
        if offset_x == 0.0 && offset_y == 0.0 {
            return;
        }
        let offset_x_pixels = gpui::px(offset_x);
        let offset_y_pixels = gpui::px(offset_y);
        for layout in &mut self.layout.visible {
            layout.bounds.origin.x -= offset_x_pixels;
            layout.bounds.origin.y -= offset_y_pixels;
        }
        for cached in self.layout.cache.values_mut() {
            cached.layout.bounds.origin.x -= offset_x_pixels;
            cached.layout.bounds.origin.y -= offset_y_pixels;
        }
        self.layout_offset = (0.0, 0.0);
    }

    pub fn visible_text(&self) -> String {
        self.document_text()
    }

    pub fn document_len(&self) -> usize {
        self.document.flat_utf8_len()
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
        let start = self.flat_offset_for_point(self.selection.anchor);
        let end = self.flat_offset_for_point(self.selection.head);
        let (start, end) = if start <= end {
            (start, end)
        } else {
            (end, start)
        };
        self.document
            .text_for_utf8_range(start..end)
            .unwrap_or_default()
    }

    pub fn copy_all_plain_text(&self) -> String {
        self.document_text()
    }

    pub fn type_text(&mut self, text: &str) -> Result<(), DocumentError> {
        self.insert_text(text)
    }

    #[cfg(test)]
    pub fn insert_fixture_image(
        &mut self,
        resource_id: &str,
        natural_size: (u32, u32),
    ) -> Result<(), DocumentError> {
        self.apply(Transaction::InsertImage {
            selection: self.selection,
            resource_id: resource_id.to_owned(),
            natural_size,
        })
        .map(|_| ())
    }

    pub fn cut_selection(&mut self) -> Result<String, DocumentError> {
        self.ensure_editable()?;
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

    /// Commit an image node before any decode work. The compressed payload is
    /// handed to the shared ImageStore; the existing Transaction::InsertImage
    /// path owns selection, history, and structural paragraph splitting.
    pub fn insert_image_payload(&mut self, payload: ImagePayload) -> Result<(), DocumentError> {
        self.insert_image_payload_at(payload, self.selection)
    }

    pub fn insert_image_payload_at(
        &mut self,
        payload: ImagePayload,
        selection: Selection,
    ) -> Result<(), DocumentError> {
        self.ensure_editable()?;
        let resource_id = Uuid::new_v4().to_string();
        let (width, height) = image_dimensions(&payload).ok_or_else(|| {
            DocumentError::InvalidOperation("unsupported or invalid image payload".into())
        })?;
        let metadata = ImageMetadata::new(resource_id.clone(), width, height);
        self.image_store
            .insert_with_format(metadata, payload.bytes, payload.format);
        let outcome = self.apply_with_selection(Transaction::InsertImage {
            selection,
            resource_id: resource_id.clone(),
            natural_size: (width, height),
        });
        let outcome = match outcome {
            Ok(outcome) => outcome,
            Err(error) => {
                self.image_store.remove_resource(&resource_id);
                return Err(error);
            }
        };
        self.selection = outcome.selection;
        Ok(())
    }

    /// Insert a managed image directly from a file path. This is the
    /// production path for Finder drops and pasteboard temporary files: the
    /// source is copied by the filesystem and never materialized as a Rust
    /// `Vec` in the editor process.
    pub fn insert_image_path(&mut self, path: &Path) -> Result<(), DocumentError> {
        self.insert_image_path_at(path, self.selection)
    }

    pub fn insert_image_path_at(
        &mut self,
        path: &Path,
        selection: Selection,
    ) -> Result<(), DocumentError> {
        self.ensure_editable()?;
        let format = image_format_from_path(path).ok_or_else(|| {
            DocumentError::InvalidOperation("unsupported or invalid image path".into())
        })?;
        let (width, height) = image_dimensions_from_path(path, format).ok_or_else(|| {
            DocumentError::InvalidOperation("unsupported or invalid image path".into())
        })?;
        let resource_id = Uuid::new_v4().to_string();
        let metadata = ImageMetadata::new(resource_id.clone(), width, height);
        self.image_store
            .insert_from_path(metadata, path, format)
            .map_err(|error| {
                DocumentError::InvalidOperation(format!("image resource copy failed: {error}"))
            })?;
        let outcome = self.apply_with_selection(Transaction::InsertImage {
            selection,
            resource_id: resource_id.clone(),
            natural_size: (width, height),
        });
        let outcome = match outcome {
            Ok(outcome) => outcome,
            Err(error) => {
                self.image_store.remove_resource(&resource_id);
                return Err(error);
            }
        };
        self.selection = outcome.selection;
        Ok(())
    }

    pub fn paste_text(&mut self, text: &str) -> Result<(), DocumentError> {
        self.paste_plain_text(text)
    }

    pub fn insert_text(&mut self, text: &str) -> Result<(), DocumentError> {
        self.paste_plain_text(text)
    }

    pub fn apply(&mut self, transaction: Transaction) -> Result<ApplyOutcome, DocumentError> {
        self.ensure_editable()?;
        let outcome =
            self.history
                .apply_with_selection(&mut self.document, self.selection, transaction)?;
        self.selection = outcome.selection;
        self.preferred_x = None;
        self.clear_composition();
        self.layout.invalidate_nodes_with_delta(
            &self.document,
            &outcome.changed_nodes,
            outcome.structural,
            &outcome.structural_splices,
            &outcome.numbering_ranges,
        );
        Ok(outcome)
    }

    fn apply_with_selection(
        &mut self,
        transaction: Transaction,
    ) -> Result<ApplyOutcome, DocumentError> {
        self.ensure_editable()?;
        let outcome =
            self.history
                .apply_with_selection(&mut self.document, self.selection, transaction)?;
        self.preferred_x = None;
        self.layout.invalidate_nodes_with_delta(
            &self.document,
            &outcome.changed_nodes,
            outcome.structural,
            &outcome.structural_splices,
            &outcome.numbering_ranges,
        );
        Ok(outcome)
    }

    pub fn undo(&mut self) -> Result<(), DocumentError> {
        self.ensure_editable()?;
        let outcome = self.history.undo_with_outcome(&mut self.document)?;
        self.selection = outcome.selection;
        self.preferred_x = None;
        self.clear_composition();
        self.layout.invalidate_nodes_with_delta(
            &self.document,
            &outcome.changed_nodes,
            outcome.structural,
            &outcome.structural_splices,
            &outcome.numbering_ranges,
        );
        Ok(())
    }

    pub fn redo(&mut self) -> Result<(), DocumentError> {
        self.ensure_editable()?;
        let outcome = self.history.redo_with_outcome(&mut self.document)?;
        self.selection = outcome.selection;
        self.preferred_x = None;
        self.clear_composition();
        self.layout.invalidate_nodes_with_delta(
            &self.document,
            &outcome.changed_nodes,
            outcome.structural,
            &outcome.structural_splices,
            &outcome.numbering_ranges,
        );
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
        self.ensure_editable()?;
        if let Some(range) = range_utf16.as_ref() {
            self.validate_input_range(range)?;
        }
        let updating_composition = self.marked.is_some();
        let raw_input_range = self.raw_input_range(range_utf16.as_ref());
        let visible_selection = raw_input_range
            .map(|range| self.selection_for_raw_document_range(range))
            .unwrap_or_else(|| self.selection_for_input_range(None));
        let base_range = if updating_composition {
            self.composition_base_range.clone().unwrap_or_else(|| {
                self.selection_flat_range(self.composition_base.unwrap_or(visible_selection))
            })
        } else {
            self.selection_flat_range(visible_selection)
        };
        let outcome = if updating_composition {
            let marked = self.marked.clone().ok_or(DocumentError::HistoryEmpty)?;
            let candidate_range =
                self.candidate_raw_range_for_marked_range(raw_input_range, &marked);
            let candidate_offsets = candidate_range.offsets();
            let marked_range = self.actual_marked_flat_range(&marked);
            let history_before_selection = self.composition_base.unwrap_or(visible_selection);
            let replacement_text = new_text.to_owned();
            let mut actual_base_range = None;
            let outcome = self.history.replace_last_with(
                &mut self.document,
                history_before_selection,
                |restored_document| {
                    let (composition_selection, range) = remap_candidate_selection_after_inverse(
                        candidate_offsets,
                        marked_range.clone(),
                        base_range.clone(),
                        restored_document,
                    );
                    actual_base_range = Some(range);
                    Ok(Transaction::InsertText {
                        selection: composition_selection,
                        text: replacement_text.clone(),
                    })
                },
            )?;
            self.selection = outcome.selection;
            self.preferred_x = None;
            self.layout.invalidate_nodes_with_delta(
                &self.document,
                &outcome.changed_nodes,
                outcome.structural,
                &outcome.structural_splices,
                &outcome.numbering_ranges,
            );
            self.composition_base_range = actual_base_range.or(Some(base_range.clone()));
            outcome
        } else {
            self.apply_with_selection(Transaction::InsertText {
                selection: visible_selection,
                text: new_text.to_owned(),
            })?
        };
        let (node_id, start, end) = if let Some(inserted_span) = outcome.inserted_span {
            (
                inserted_span.node_id,
                inserted_span.range.start,
                inserted_span.range.end,
            )
        } else {
            debug_assert!(
                new_text.is_empty(),
                "non-empty InsertText composition outcome must expose its exact span"
            );
            let point = outcome.selection.head;
            (point.node_id, point.utf8_offset, point.utf8_offset)
        };
        let selected_relative = new_selected_range_utf16
            .as_ref()
            .map(|range| input::utf16_range_to_utf8_in(new_text, range))
            .unwrap_or(new_text.len()..new_text.len());
        let (selected_start, selected_end, marked_start, marked_end) = self
            .document
            .block(node_id)
            .and_then(|block| block.content.as_text())
            .map(|text| {
                let raw_start = start.min(text.len());
                let raw_end = end.min(text.len());
                let marked_start = resolve_grapheme_offset(text, raw_start, Affinity::Before);
                let marked_end = resolve_grapheme_offset(text, raw_end, Affinity::After);
                let selected_start_raw = raw_start
                    .saturating_add(selected_relative.start)
                    .min(text.len());
                if new_selected_range_utf16
                    .as_ref()
                    .is_none_or(|range| range.start == range.end)
                {
                    let selected =
                        resolve_grapheme_offset(text, selected_start_raw, Affinity::After);
                    (selected, selected, marked_start, marked_end)
                } else {
                    let selected_end_raw = raw_start
                        .saturating_add(selected_relative.end)
                        .min(text.len());
                    (
                        resolve_grapheme_offset(text, selected_start_raw, Affinity::Before),
                        resolve_grapheme_offset(text, selected_end_raw, Affinity::After),
                        marked_start,
                        marked_end,
                    )
                }
            })
            .unwrap_or((
                selected_relative.start.min(new_text.len()),
                selected_relative.end.min(new_text.len()),
                start,
                end,
            ));
        self.selection = if selected_start == selected_end {
            Selection::caret(DocPoint::with_affinity(
                node_id,
                selected_start,
                Affinity::After,
            ))
        } else {
            Selection::new(
                DocPoint::with_affinity(node_id, selected_start, Affinity::Before),
                DocPoint::with_affinity(node_id, selected_end, Affinity::After),
            )
        };
        self.marked = (!new_text.is_empty()).then_some(MarkedText {
            node_id,
            utf8_range: marked_start..marked_end,
            actual_utf8_range: start..end,
        });
        self.composition_base =
            (!new_text.is_empty()).then_some(self.composition_base.unwrap_or(visible_selection));
        if new_text.is_empty() {
            self.composition_base_range = None;
        } else if !updating_composition {
            self.composition_base_range = Some(base_range);
        }
        Ok(())
    }

    pub fn commit_marked_text(&mut self, text: &str) -> Result<(), DocumentError> {
        self.ensure_editable()?;
        self.commit_marked_text_with_range(None, text)
    }

    fn commit_marked_text_with_range(
        &mut self,
        range_utf16: Option<&Range<usize>>,
        text: &str,
    ) -> Result<(), DocumentError> {
        let Some(marked) = self.marked.clone() else {
            return self.paste_plain_text(text);
        };
        let marked_selection = self.actual_marked_selection(&marked);
        let raw_input_range = self.raw_input_range(range_utf16);
        let candidate_range = self.candidate_raw_range_for_marked_range(raw_input_range, &marked);
        let candidate_offsets = candidate_range.offsets();
        let marked_range = self.actual_marked_flat_range(&marked);
        let base_range = self.composition_base_range.clone().unwrap_or_else(|| {
            self.selection_flat_range(self.composition_base.unwrap_or(marked_selection))
        });

        if text.is_empty() {
            let outcome = self.history.undo_with_outcome(&mut self.document)?;
            self.selection = outcome.selection;
            self.preferred_x = None;
            self.layout.invalidate_nodes_with_delta(
                &self.document,
                &outcome.changed_nodes,
                outcome.structural,
                &outcome.structural_splices,
                &outcome.numbering_ranges,
            );
            self.clear_composition();
            return Ok(());
        }

        let history_before_selection = self.composition_base.unwrap_or(marked_selection);
        let replacement_text = text.to_owned();
        let outcome = self.history.replace_last_with(
            &mut self.document,
            history_before_selection,
            |restored_document| {
                let (base_selection, _) = remap_candidate_selection_after_inverse(
                    candidate_offsets,
                    marked_range.clone(),
                    base_range.clone(),
                    restored_document,
                );
                Ok(Transaction::InsertText {
                    selection: base_selection,
                    text: replacement_text.clone(),
                })
            },
        )?;
        self.selection = outcome.selection;
        self.preferred_x = None;
        self.layout.invalidate_nodes_with_delta(
            &self.document,
            &outcome.changed_nodes,
            outcome.structural,
            &outcome.structural_splices,
            &outcome.numbering_ranges,
        );
        self.clear_composition();
        Ok(())
    }

    fn selection_flat_range(&self, selection: Selection) -> Range<usize> {
        let (start, end) = self.ordered_points_for(selection);
        self.flat_offset_for_point(start)..self.flat_offset_for_point(end)
    }

    fn ordered_points_for(&self, selection: Selection) -> (DocPoint, DocPoint) {
        let anchor_key = self.doc_point_key(selection.anchor);
        let head_key = self.doc_point_key(selection.head);
        if anchor_key <= head_key {
            (selection.anchor, selection.head)
        } else {
            (selection.head, selection.anchor)
        }
    }

    fn doc_point_key(&self, point: DocPoint) -> (usize, usize, u8) {
        (
            self.block_index(point.node_id).unwrap_or(usize::MAX),
            point.utf8_offset,
            match point.affinity {
                Affinity::Before => 0,
                Affinity::After => 1,
            },
        )
    }

    pub fn move_to_image_before(&mut self) {
        let image_id = self
            .document
            .blocks()
            .iter()
            .find(|block| block.kind == BlockKind::Image)
            .map(|block| block.id);
        if let Some(image_id) = image_id {
            self.selection =
                Selection::caret(DocPoint::with_affinity(image_id, 0, Affinity::Before));
            self.clear_composition();
        }
    }

    pub fn move_to_image_after(&mut self) {
        let image_id = self
            .document
            .blocks()
            .iter()
            .find(|block| block.kind == BlockKind::Image)
            .map(|block| block.id);
        if let Some(image_id) = image_id {
            self.selection =
                Selection::caret(DocPoint::with_affinity(image_id, 0, Affinity::After));
            self.clear_composition();
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
            self.clear_composition();
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
        self.clear_composition();
    }

    pub fn move_right(&mut self) {
        if !self.selection.is_caret() {
            self.selection = Selection::caret(self.ordered_selection().1);
            self.preferred_x = None;
            self.clear_composition();
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
        self.clear_composition();
    }

    pub fn move_home(&mut self) {
        if !self.selection.is_caret() {
            self.selection = Selection::caret(self.ordered_selection().0);
        }
        let point = self.selection.head;
        if let Some(boundary) = self.layout.visual_line_boundary(point, false) {
            self.selection = Selection::caret(self.snap_layout_point(boundary));
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
        self.clear_composition();
    }

    pub fn move_end(&mut self) {
        if !self.selection.is_caret() {
            self.selection = Selection::caret(self.ordered_selection().1);
        }
        let point = self.selection.head;
        if let Some(boundary) = self.layout.visual_line_boundary(point, true) {
            self.selection = Selection::caret(self.snap_layout_point(boundary));
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
        self.clear_composition();
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

    /// Extend the document selection from its anchor while using the same
    /// movement primitives as the unmodified arrow actions.  Keeping this in
    /// `EditorCore` means Shift selection never creates a second focus or
    /// transaction owner in the GPUI route.
    pub fn select_left(&mut self) {
        self.extend_with(|editor| editor.move_left());
    }

    pub fn select_right(&mut self) {
        self.extend_with(|editor| editor.move_right());
    }

    pub fn select_up(&mut self) {
        self.extend_vertical(-1);
    }

    pub fn select_down(&mut self) {
        self.extend_vertical(1);
    }

    pub fn select_home(&mut self) {
        self.extend_with(|editor| editor.move_home());
    }

    pub fn select_end(&mut self) {
        self.extend_with(|editor| editor.move_end());
    }

    pub fn select_word_left(&mut self) {
        let anchor = self.selection.anchor;
        let head = self.selection.head;
        let Some(target) = self.word_boundary_target(head, false) else {
            return;
        };
        self.selection = Selection::new(anchor, target);
        self.preferred_x = None;
        self.clear_composition();
    }

    pub fn select_word_right(&mut self) {
        let anchor = self.selection.anchor;
        let head = self.selection.head;
        let Some(target) = self.word_boundary_target(head, true) else {
            return;
        };
        self.selection = Selection::new(anchor, target);
        self.preferred_x = None;
        self.clear_composition();
    }

    /// Find a word boundary in the current text block first, then continue to
    /// the nearest text block in document order when the local boundary is
    /// already exhausted. Structural blocks remain inside the resulting
    /// document selection, but never manufacture a byte offset of their own.
    fn word_boundary_target(&self, head: DocPoint, right: bool) -> Option<DocPoint> {
        let index = self.block_index(head.node_id)?;
        if let Some(text) = self.document.blocks()[index].content.as_text() {
            let offset = head.utf8_offset.min(text.len());
            let local = if right {
                next_word_boundary(text, offset)
            } else {
                previous_word_boundary(text, offset)
            };
            if local != offset {
                return Some(DocPoint::with_affinity(
                    head.node_id,
                    local,
                    if right {
                        Affinity::After
                    } else {
                        Affinity::Before
                    },
                ));
            }
        }

        if right {
            for candidate in (index + 1)..self.document.block_count() {
                let block = self.document.blocks().get(candidate)?;
                let Some(text) = block.content.as_text() else {
                    continue;
                };
                if text.is_empty() {
                    continue;
                }
                let target = next_word_boundary(text, 0);
                if target > 0 {
                    return Some(DocPoint::with_affinity(block.id, target, Affinity::After));
                }
            }
        } else {
            for candidate in (0..index).rev() {
                let block = self.document.blocks().get(candidate)?;
                let Some(text) = block.content.as_text() else {
                    continue;
                };
                if text.is_empty() {
                    continue;
                }
                let target = previous_word_boundary(text, text.len());
                if target < text.len() {
                    return Some(DocPoint::with_affinity(block.id, target, Affinity::Before));
                }
            }
        }
        None
    }

    /// Convert a shaped editor-surface point into the document's canonical
    /// point.  The layout registry owns hit geometry, including wrapped rows,
    /// list insets, alignment and image affinities.
    pub(crate) fn point_from_layout(&mut self, position: Point<Pixels>) -> Option<DocPoint> {
        let point = self.layout.point_to_doc(position)?;
        Some(self.snap_layout_point(point))
    }

    /// Return the stable resource id when a layout hit lands on an image
    /// block. The caller can use this for targeted retry actions without
    /// scanning every image in the document.
    pub(crate) fn image_resource_at_layout(&mut self, position: Point<Pixels>) -> Option<String> {
        let point = self.point_from_layout(position)?;
        self.document
            .block(point.node_id)
            .and_then(|block| match &block.content {
                BlockContent::Image { resource_id, .. } => Some(resource_id.clone()),
                _ => None,
            })
    }

    pub(crate) fn begin_pointer_selection(
        &mut self,
        position: Point<Pixels>,
        extend: bool,
    ) -> Option<DocPoint> {
        let point = self.point_from_layout(position)?;
        let anchor = if extend { self.selection.anchor } else { point };
        self.selection = if extend {
            Selection::new(anchor, point)
        } else {
            Selection::caret(point)
        };
        self.preferred_x = None;
        self.clear_composition();
        // Dragging must continue from the original anchor.  For a Shift
        // click that is the pre-existing selection anchor, not the point
        // where the extension click landed.
        Some(anchor)
    }

    pub(crate) fn update_pointer_selection(
        &mut self,
        anchor: DocPoint,
        position: Point<Pixels>,
    ) -> Option<Selection> {
        let head = self.point_from_layout(position)?;
        self.selection = Selection::new(anchor, head);
        self.preferred_x = None;
        self.clear_composition();
        Some(self.selection)
    }

    fn extend_with(&mut self, movement: impl FnOnce(&mut Self)) {
        let anchor = self.selection.anchor;
        let head = self.selection.head;
        self.selection = Selection::caret(head);
        movement(self);
        let target = self.selection.head;
        self.selection = Selection::new(anchor, target);
        self.preferred_x = None;
        self.clear_composition();
    }

    fn extend_vertical(&mut self, direction: isize) {
        let anchor = self.selection.anchor;
        let head = self.selection.head;
        self.selection = Selection::caret(head);
        self.move_vertical(direction);
        let target = self.selection.head;
        self.selection = Selection::new(anchor, target);
        self.clear_composition();
    }

    fn move_vertical(&mut self, direction: isize) {
        if !self.selection.is_caret() {
            self.selection = Selection::caret(if direction < 0 {
                self.ordered_selection().0
            } else {
                self.ordered_selection().1
            });
            self.preferred_x = None;
            self.clear_composition();
            return;
        }
        let point = self.selection.head;
        let preferred_x = self.preferred_x.or_else(|| self.layout.caret_x(point));
        if let Some(target) = self.layout.visual_move(point, direction, preferred_x) {
            let x = preferred_x.or_else(|| self.layout.caret_x(point));
            self.selection = Selection::caret(self.snap_layout_point(target));
            self.preferred_x = x;
            self.clear_composition();
            return;
        }
        let Some(_index) = self.block_index(point.node_id) else {
            return;
        };
        let block = if direction < 0 {
            self.document.previous_text_block(point.node_id)
        } else {
            self.document.next_text_block(point.node_id)
        };
        let target = block.and_then(|block| {
            self.layout
                .visual_edge_point(block.id, preferred_x, direction < 0)
                .map(|point| self.snap_layout_point(point))
                .or_else(|| {
                    block.content.as_text().map(|text| {
                        DocPoint::with_affinity(
                            block.id,
                            nearest_grapheme_boundary(text, point.utf8_offset),
                            Affinity::After,
                        )
                    })
                })
        });
        if let Some(target) = target {
            self.selection = Selection::caret(target);
            self.preferred_x = preferred_x;
            self.clear_composition();
        }
    }

    pub fn insert_paragraph_break(&mut self) -> Result<(), DocumentError> {
        self.ensure_editable()?;
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
        self.layout.invalidate_nodes_with_delta(
            &self.document,
            &outcome.changed_nodes,
            outcome.structural,
            &outcome.structural_splices,
            &outcome.numbering_ranges,
        );
        Ok(())
    }

    pub fn backspace(&mut self) -> Result<(), DocumentError> {
        self.ensure_editable()?;
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
        } else if index == 0 {
            // At the start of the first list item, Backspace exits the list
            // just like the donor editor. Keep the following items and their
            // depths intact instead of treating the document edge as a hard
            // no-op.
            let current = &self.document.blocks()[index];
            if matches!(
                current.kind,
                BlockKind::BulletItem { .. }
                    | BlockKind::OrderedItem { .. }
                    | BlockKind::CheckItem { .. }
            ) {
                let selection = self.selection;
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
                self.clear_composition();
                self.layout.invalidate_nodes_with_delta(
                    &self.document,
                    &outcome.changed_nodes,
                    outcome.structural,
                    &outcome.structural_splices,
                    &outcome.numbering_ranges,
                );
            } else {
                self.clear_composition();
            }
            return Ok(());
        } else if let Some(previous) = self.document.blocks().get(index - 1) {
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
                    let selection = self.selection;
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
                    self.clear_composition();
                    self.layout.invalidate_nodes_with_delta(
                        &self.document,
                        &outcome.changed_nodes,
                        outcome.structural,
                        &outcome.structural_splices,
                        &outcome.numbering_ranges,
                    );
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
                    self.clear_composition();
                    self.layout.invalidate_nodes_with_delta(
                        &self.document,
                        &outcome.changed_nodes,
                        outcome.structural,
                        &outcome.structural_splices,
                        &outcome.numbering_ranges,
                    );
                }
            }
        }
        Ok(())
    }

    pub fn delete_forward(&mut self) -> Result<(), DocumentError> {
        self.ensure_editable()?;
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
                self.clear_composition();
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
                self.clear_composition();
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
                self.clear_composition();
                self.layout.invalidate_nodes_with_delta(
                    &self.document,
                    &outcome.changed_nodes,
                    outcome.structural,
                    &outcome.structural_splices,
                    &outcome.numbering_ranges,
                );
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
        self.composition_base_range = None;
    }

    fn active_text(&self) -> Option<(NodeId, &str)> {
        self.document
            .block(self.selection.head.node_id)
            .and_then(|block| block.content.as_text().map(|text| (block.id, text)))
            .or_else(|| {
                self.document
                    .first_text_block()
                    .and_then(|block| block.content.as_text().map(|text| (block.id, text)))
            })
    }

    fn selection_for_input_range(&self, range_utf16: Option<&Range<usize>>) -> Selection {
        if let Some(range) = self.raw_input_range(range_utf16) {
            return self.selection_for_raw_document_range(range);
        }
        if let Some(marked) = self.marked.as_ref() {
            return Selection::new(
                DocPoint::with_affinity(marked.node_id, marked.utf8_range.start, Affinity::Before),
                DocPoint::with_affinity(marked.node_id, marked.utf8_range.end, Affinity::After),
            );
        }
        self.selection
    }

    fn raw_input_range(&self, range_utf16: Option<&Range<usize>>) -> Option<RawDocumentRange> {
        let range_utf16 = range_utf16?;
        Some(RawDocumentRange::from_utf8(
            self.document.utf16_range_to_utf8(range_utf16.clone()),
        ))
    }

    fn actual_marked_selection(&self, marked: &MarkedText) -> Selection {
        Selection::new(
            DocPoint::with_affinity(
                marked.node_id,
                marked.actual_utf8_range.start,
                Affinity::Before,
            ),
            DocPoint::with_affinity(
                marked.node_id,
                marked.actual_utf8_range.end,
                Affinity::After,
            ),
        )
    }

    fn actual_marked_flat_range(&self, marked: &MarkedText) -> Range<usize> {
        self.raw_marked_range(marked, &marked.actual_utf8_range)
            .as_range()
    }

    fn raw_marked_range(&self, marked: &MarkedText, range: &Range<usize>) -> RawDocumentRange {
        RawDocumentRange::with_affinities(
            flat_offset_for_point_in(
                &self.document,
                DocPoint::with_affinity(marked.node_id, range.start, Affinity::Before),
            ),
            flat_offset_for_point_in(
                &self.document,
                DocPoint::with_affinity(marked.node_id, range.end, Affinity::After),
            ),
            Affinity::Before,
            Affinity::After,
        )
    }

    /// Resolve the platform's candidate range against the actual provisional
    /// bytes without expanding raw endpoints to candidate grapheme
    /// boundaries. The public marked span may include a preceding grapheme
    /// when an IME inserts a combining mark, so only a platform range exactly
    /// equal to that expanded public span is mapped to `actual_utf8_range`
    /// before inverse mapping; every other explicit range stays raw.
    fn candidate_raw_range_for_marked_range(
        &self,
        requested: Option<RawDocumentRange>,
        marked: &MarkedText,
    ) -> RawDocumentRange {
        let actual = self.raw_marked_range(marked, &marked.actual_utf8_range);
        let Some(requested) = requested else {
            return actual;
        };
        let public = self.raw_marked_range(marked, &marked.utf8_range);
        // With an ordinary candidate the platform range may intentionally
        // include text immediately before/after the marked span (the donor
        // accepts that explicit replacement). Only the exact public span
        // itself is treated as the IME's expanded view of the candidate; an
        // explicit range outside it remains a real document range.
        if public != actual && requested.start == public.start && requested.end == public.end {
            return actual;
        }
        requested
    }

    fn validate_input_range(&self, range_utf16: &Range<usize>) -> Result<(), DocumentError> {
        let document_len = self.document.flat_utf16_len();
        if range_utf16.start > range_utf16.end || range_utf16.end > document_len {
            return Err(DocumentError::InvalidOperation(
                "input UTF-16 range is outside the document".into(),
            ));
        }
        Ok(())
    }

    fn ordered_selection(&self) -> (DocPoint, DocPoint) {
        if self.doc_point_key(self.selection.anchor) <= self.doc_point_key(self.selection.head) {
            (self.selection.anchor, self.selection.head)
        } else {
            (self.selection.head, self.selection.anchor)
        }
    }

    fn block_index(&self, node_id: NodeId) -> Option<usize> {
        self.document.node_index(node_id).ok()
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
        let Some(current) = self.document.block_at_index(index) else {
            return self.selection.head;
        };
        self.document
            .previous_navigation_block(current.id)
            .map(|candidate| {
                if candidate.kind == BlockKind::Image {
                    DocPoint::with_affinity(candidate.id, 0, Affinity::After)
                } else {
                    DocPoint::with_affinity(
                        candidate.id,
                        candidate.content.as_text().map_or(0, str::len),
                        Affinity::After,
                    )
                }
            })
            .unwrap_or(self.selection.head)
    }

    fn next_block_point(&self, index: usize) -> DocPoint {
        let Some(current) = self.document.block_at_index(index) else {
            return self.selection.head;
        };
        self.document
            .next_navigation_block(current.id)
            .map(|candidate| DocPoint::with_affinity(candidate.id, 0, Affinity::Before))
            .unwrap_or(self.selection.head)
    }

    fn previous_text_point(&self, index: usize) -> DocPoint {
        let Some(current) = self.document.block_at_index(index) else {
            return self.selection.head;
        };
        self.document
            .previous_text_block(current.id)
            .map(|block| {
                DocPoint::with_affinity(
                    block.id,
                    block.content.as_text().map_or(0, str::len),
                    Affinity::After,
                )
            })
            .unwrap_or(self.selection.head)
    }

    fn next_text_point(&self, index: usize) -> DocPoint {
        let Some(current) = self.document.block_at_index(index) else {
            return self.selection.head;
        };
        self.document
            .next_text_block(current.id)
            .map(|block| DocPoint::with_affinity(block.id, 0, Affinity::Before))
            .unwrap_or(self.selection.head)
    }

    fn flat_offset_for_point(&self, point: DocPoint) -> usize {
        flat_offset_for_point_in(&self.document, point)
    }

    fn selection_for_document_byte_range(&self, range: Range<usize>) -> Selection {
        self.selection_for_raw_document_range(RawDocumentRange::from_utf8(range))
    }

    fn selection_for_raw_document_range(&self, range: RawDocumentRange) -> Selection {
        if range.start == range.end {
            Selection::caret(
                self.point_for_document_offset_with_affinity(range.start, range.start_affinity),
            )
        } else {
            Selection::new(
                self.point_for_document_offset_with_affinity(range.start, range.start_affinity),
                self.point_for_document_offset_with_affinity(range.end, range.end_affinity),
            )
        }
    }

    fn point_for_document_offset_with_affinity(
        &self,
        offset: usize,
        affinity: Affinity,
    ) -> DocPoint {
        point_for_document_offset_in(&self.document, offset, affinity)
    }

    /// Layout operates on individual shaped hard lines, while the document
    /// model validates grapheme boundaries over the complete block text. A
    /// CRLF pair is one grapheme cluster even though shaping exposes the
    /// carriage return at the visual end of the preceding line, so final
    /// navigation points must be snapped against the model text here.
    fn snap_layout_point(&self, point: DocPoint) -> DocPoint {
        let Some(text) = self
            .document
            .block(point.node_id)
            .and_then(|block| block.content.as_text())
        else {
            return point;
        };
        let offset = point.utf8_offset.min(text.len());
        let affinity = if text.as_bytes().get(offset.saturating_sub(1)) == Some(&b'\r')
            && text.as_bytes().get(offset) == Some(&b'\n')
        {
            Affinity::Before
        } else {
            point.affinity
        };
        DocPoint::with_affinity(
            point.node_id,
            resolve_grapheme_offset(text, offset, affinity),
            affinity,
        )
    }
}

fn point_for_document_offset_in(
    document: &Document,
    offset: usize,
    affinity: Affinity,
) -> DocPoint {
    document
        .point_for_flat_utf8_offset(offset, affinity)
        .unwrap_or_else(|| document.end_selection().head)
}

fn flat_offset_for_point_in(document: &Document, point: DocPoint) -> usize {
    document
        .flat_offset_for_point(point)
        .unwrap_or_else(|| document.flat_utf8_len())
}

fn remap_candidate_selection_after_inverse(
    candidate_offsets: ((usize, Affinity), (usize, Affinity)),
    marked_range: Range<usize>,
    base_range: Range<usize>,
    restored_document: &Document,
) -> (Selection, Range<usize>) {
    let map_offset = |offset: usize, affinity: Affinity| {
        let marked_len = marked_range.end.saturating_sub(marked_range.start);
        let base_len = base_range.end.saturating_sub(base_range.start);
        if offset < marked_range.start
            || (offset == marked_range.start && affinity == Affinity::Before)
        {
            offset
        } else if offset > marked_range.end
            || (offset == marked_range.end && affinity == Affinity::After)
        {
            offset.saturating_sub(marked_len).saturating_add(base_len)
        } else if affinity == Affinity::Before {
            base_range.start
        } else {
            base_range.end
        }
    };
    let map_point = |(offset, affinity): (usize, Affinity)| {
        let mapped = map_offset(offset, affinity);
        point_for_document_offset_in(restored_document, mapped, affinity)
    };
    let selection = Selection::new(
        map_point(candidate_offsets.0),
        map_point(candidate_offsets.1),
    );
    let (start, end) = if flat_offset_for_point_in(restored_document, selection.anchor)
        <= flat_offset_for_point_in(restored_document, selection.head)
    {
        (selection.anchor, selection.head)
    } else {
        (selection.head, selection.anchor)
    };
    (
        selection,
        flat_offset_for_point_in(restored_document, start)
            ..flat_offset_for_point_in(restored_document, end),
    )
}

impl EntityInputHandler for EditorCore {
    fn text_for_range(
        &mut self,
        range_utf16: Range<usize>,
        actual_range: &mut Option<Range<usize>>,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<String> {
        let range = self.document.utf16_range_to_utf8(range_utf16);
        actual_range.replace(self.document.utf8_range_to_utf16(range.clone()));
        self.document.text_for_utf8_range(range)
    }

    fn selected_text_range(
        &mut self,
        _ignore_disabled_input: bool,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<UTF16Selection> {
        let anchor = self.flat_offset_for_point(self.selection.anchor);
        let head = self.flat_offset_for_point(self.selection.head);
        let (start, end, reversed) = if anchor <= head {
            (anchor, head, false)
        } else {
            (head, anchor, true)
        };
        let range = self.document.utf8_range_to_utf16(start..end);
        Some(UTF16Selection { range, reversed })
    }

    fn marked_text_range(
        &self,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<Range<usize>> {
        let marked = self.marked.as_ref()?;
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
        Some(self.document.utf8_range_to_utf16(start..end))
    }

    fn unmark_text(&mut self, _window: &mut Window, _cx: &mut Context<Self>) {
        // A read-only session never owns an IME composition. More
        // importantly, do not let a delayed platform unmark callback mutate
        // composition bookkeeping after Task 4 has deliberately disabled all
        // document-input paths.
        if self.is_read_only() {
            return;
        }
        self.marked = None;
        self.composition_base = None;
        self.composition_base_range = None;
    }

    fn replace_text_in_range(
        &mut self,
        range_utf16: Option<Range<usize>>,
        new_text: &str,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.is_read_only() {
            return;
        }
        if let Some(range) = range_utf16.as_ref()
            && let Err(error) = self.validate_input_range(range)
        {
            self.last_input_error = Some(error);
            cx.notify();
            return;
        }
        if self.marked.is_some() {
            match self.commit_marked_text_with_range(range_utf16.as_ref(), new_text) {
                Ok(()) => {
                    self.last_input_error = None;
                    cx.notify();
                }
                Err(error) => {
                    self.last_input_error = Some(error);
                    cx.notify();
                }
            }
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
        if self.is_read_only() {
            return;
        }
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
        let range = self.document.utf16_range_to_utf8(range_utf16);
        let selection = self.selection_for_document_byte_range(range);
        if selection.is_caret() {
            return self.layout.caret_bounds_for_point(selection.head);
        }
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
        let point = self.snap_layout_point(point);
        Some(
            self.document
                .utf8_to_utf16_offset(self.flat_offset_for_point(point)),
        )
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

fn mark_state_for_range(
    styles: &[super::model::StyledRun],
    range: Range<usize>,
    mark: &super::model::Mark,
) -> (bool, bool) {
    let mut cursor = range.start;
    let mut any = false;
    let mut all = true;
    for run in styles {
        if run.range.end <= range.start || run.range.start >= range.end {
            continue;
        }
        let segment_start = run.range.start.max(range.start);
        let segment_end = run.range.end.min(range.end);
        if segment_start > cursor {
            all = false;
        }
        if contains_mark(&run.marks, mark) {
            any = true;
        } else {
            all = false;
        }
        cursor = cursor.max(segment_end);
    }
    if cursor < range.end {
        all = false;
    }
    (any, all)
}

fn contains_mark(marks: &[super::model::Mark], mark: &super::model::Mark) -> bool {
    marks.iter().any(|candidate| match (candidate, mark) {
        (super::model::Mark::Link(_), super::model::Mark::Link(_)) => true,
        (candidate, mark) => candidate == mark,
    })
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

fn previous_word_boundary(text: &str, offset: usize) -> usize {
    let offset = offset.min(text.len());
    let mut boundary = 0;
    for (index, segment) in text.split_word_bound_indices() {
        if index >= offset {
            break;
        }
        boundary = index;
        if index.saturating_add(segment.len()) >= offset {
            break;
        }
    }
    boundary
}

fn next_word_boundary(text: &str, offset: usize) -> usize {
    let offset = offset.min(text.len());
    for (index, _) in text.split_word_bound_indices() {
        if index > offset {
            return index;
        }
    }
    text.len()
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

#[cfg(test)]
mod raw_document_range_tests {
    use super::RawDocumentRange;

    #[test]
    fn utf16_to_raw_range_preserves_surrogate_and_combining_endpoints() {
        let text = "甲😀乙";
        let surrogate_neighbor = RawDocumentRange::from_utf16(text, &(1..3));
        assert_eq!(
            (surrogate_neighbor.start, surrogate_neighbor.end),
            ("甲".len(), "甲😀".len())
        );

        let combining = "a\u{301}Q";
        let subset = RawDocumentRange::from_utf16(combining, &(0..1));
        assert_eq!((subset.start, subset.end), (0, 1));
        let overlap = RawDocumentRange::from_utf16(combining, &(1..3));
        assert_eq!((overlap.start, overlap.end), (1, combining.len()));
    }
}

#[cfg(test)]
mod image_orientation_tests {
    use super::oriented_dimensions;

    #[test]
    fn exif_rotated_dimensions_swap_for_both_orientation_directions() {
        assert_eq!(oriented_dimensions((4031, 3023), 6), (3023, 4031));
        assert_eq!(oriented_dimensions((3023, 4031), 8), (4031, 3023));
        assert_eq!(oriented_dimensions((4031, 3023), 1), (4031, 3023));
    }
}
