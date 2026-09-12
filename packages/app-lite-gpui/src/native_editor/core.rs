//! The single owner of native-editor focus, selection, IME composition,
//! transactions, history, and document-wide commands.

use std::collections::{HashMap, HashSet};
use std::io::Cursor;
use std::ops::Range;
use std::path::{Path, PathBuf};

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

use super::find::{FindError, FindMatch, FindState, FindSummary};
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

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AttachmentMetadata {
    resource_id: String,
    size: u64,
    available: bool,
}

/// Opaque handle for a pending picker, paste, or drop insertion point.
///
/// The editor alone owns the mutable point behind this token. UI code may
/// retain the token while an asynchronous source is staged, but it cannot
/// retain a bare `DocPoint` that becomes stale as the document changes.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) struct ResourceInsertAnchor {
    token: u64,
    /// The document revision at capture time is carried with the handle so a
    /// session intent has an explicit generation provenance. Resolution is
    /// nevertheless based on the tracked mapping, not an unsafe equality
    /// check against a later live revision.
    captured_revision: u64,
}

impl ResourceInsertAnchor {
    pub(crate) const fn captured_revision(self) -> u64 {
        self.captured_revision
    }
}

#[derive(Clone, Copy, Debug)]
struct PendingResourceInsertAnchor {
    selection: Option<Selection>,
}

#[derive(Clone, Debug)]
struct ResourceAnchorReplacement {
    range: Range<usize>,
    pre_document_len: usize,
    anchors: Vec<(u64, (usize, Affinity), (usize, Affinity))>,
}

impl AttachmentMetadata {
    fn ready(resource_id: impl Into<String>, size: u64) -> Self {
        Self {
            resource_id: resource_id.into(),
            size,
            available: true,
        }
    }

    fn unavailable(resource_id: impl Into<String>) -> Self {
        Self {
            resource_id: resource_id.into(),
            size: 0,
            available: false,
        }
    }

    pub fn resource_id(&self) -> &str {
        &self.resource_id
    }

    pub const fn size(&self) -> u64 {
        self.size
    }

    pub const fn is_available(&self) -> bool {
        self.available
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
    /// Ephemeral find state follows committed document mutations but is never
    /// encoded into canonical HTML or represented in undo/redo history.
    find: FindState,
    image_store: ImageStore,
    /// Pure in-memory source state for retained durable images. Rendering
    /// consults this instead of probing `Path::is_file` every frame; the
    /// session updates it only after an atomic materialization handoff.
    materialized_image_ids: HashSet<String>,
    /// Requests emitted only for image atoms in the renderer's visible or
    /// prefetch residency set. A retained `NoteSession` drains this small
    /// queue and verifies/materializes the blob off the GPUI thread.
    pending_image_hydration: HashSet<String>,
    active_image_hydration: HashSet<String>,
    failed_image_hydration: HashSet<String>,
    /// Attachment bytes are never held by the editor. This small metadata map
    /// gives structural attachment nodes an honest filename/mime/size card;
    /// the note session materializes a verified source only for an explicit
    /// system-open request.
    attachment_metadata: HashMap<String, AttachmentMetadata>,
    /// Pending external insertion points only. This stays empty during normal
    /// typing, and when it is non-empty every successful transaction maps the
    /// few tracked points in place; no document clone or whole-note diff is
    /// needed for a keystroke.
    next_resource_insert_anchor: u64,
    pending_resource_insert_anchors: HashMap<u64, PendingResourceInsertAnchor>,
    pub(crate) layout: LayoutRegistry,
    layout_offset: (f32, f32),
    #[cfg(test)]
    shape_calls: usize,
    /// Test-only fault seam for the legacy post-commit `apply` path. Durable
    /// resource insertion must not call that path after SQLite has committed.
    #[cfg(test)]
    next_apply_error_for_test: Option<DocumentError>,
}

/// A fully validated editor mutation prepared before an external durable
/// transaction. It owns the resulting document and bounded inverse history,
/// so installation after SQLite succeeds is an infallible state swap rather
/// than a second mutable replay that can leave DB and session divergent.
pub(crate) struct PreparedEditorCommit {
    document: Document,
    history: History,
    outcome: ApplyOutcome,
    resource_anchor_mapping: Option<ResourceAnchorReplacement>,
}

impl PreparedEditorCommit {
    pub(crate) fn document(&self) -> &Document {
        &self.document
    }
}

/// Capability boundary for the single editor core.  The default constructor
/// remains editable for the performance spike; the real library uses
/// `ReadOnly` until Task 4 can durably save every mutation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EditorAccess {
    Editable,
    /// A committed library mutation is waiting for a complete model/session
    /// reconciliation. The old editor stays selectable/copyable but cannot
    /// accept input against an obsolete durable revision.
    RecoveryLocked,
    ReadOnly,
}

/// The pointer-facing identity of a section-level resource node. The shared
/// surface uses this only to distinguish the attachment's platform open
/// request; both variants are selected as one complete document atom.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum AtomicBlockHit {
    Image,
    Attachment { resource_id: String },
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
            find: FindState::default(),
            image_store: ImageStore::default(),
            materialized_image_ids: HashSet::new(),
            pending_image_hydration: HashSet::new(),
            active_image_hydration: HashSet::new(),
            failed_image_hydration: HashSet::new(),
            attachment_metadata: HashMap::new(),
            next_resource_insert_anchor: 1,
            pending_resource_insert_anchors: HashMap::new(),
            layout: LayoutRegistry::new(),
            layout_offset: (0.0, 0.0),
            #[cfg(test)]
            shape_calls: 0,
            #[cfg(test)]
            next_apply_error_for_test: None,
        }
    }

    pub fn focus_handle(&self) -> &FocusHandle {
        &self.focus
    }

    pub fn access(&self) -> EditorAccess {
        self.access
    }

    pub fn is_read_only(&self) -> bool {
        self.access != EditorAccess::Editable
    }

    /// Temporarily freeze only an otherwise editable retained session while
    /// its owning LibraryShell waits for a full committed-action candidate.
    /// A durable Trash preview remains ReadOnly regardless of this flag.
    pub(crate) fn set_recovery_locked(&mut self, locked: bool) {
        self.access = match (self.access, locked) {
            (EditorAccess::Editable, true) => EditorAccess::RecoveryLocked,
            (EditorAccess::RecoveryLocked, false) => EditorAccess::Editable,
            (access, _) => access,
        };
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

    pub fn set_find_query(&mut self, query: &str, case_sensitive: bool) -> Result<(), FindError> {
        self.find.set_query(&self.document, query, case_sensitive)
    }

    pub fn find_next(&mut self) -> Option<&FindMatch> {
        self.find.next()
    }

    pub fn find_previous(&mut self) -> Option<&FindMatch> {
        self.find.previous()
    }

    pub fn find_summary(&self) -> FindSummary {
        self.find.summary()
    }

    pub fn find_matches(&self) -> impl Iterator<Item = &FindMatch> {
        self.find.matches()
    }

    pub fn find_primary(&self) -> Option<&FindMatch> {
        self.find.primary()
    }

    pub fn clear_find(&mut self) {
        self.find.clear();
    }

    pub(crate) fn find_matches_for_node(
        &self,
        node_id: NodeId,
    ) -> impl Iterator<Item = (&FindMatch, bool)> {
        self.find.matches_for_node(node_id)
    }

    #[cfg(test)]
    pub(crate) fn find_scanned_blocks_for_test(&self) -> usize {
        self.find.scanned_blocks_for_test()
    }

    #[cfg(test)]
    pub(crate) fn find_matcher_compiles_for_test(&self) -> usize {
        self.find.matcher_compiles_for_test()
    }

    /// Register a point that an asynchronous external resource completion
    /// must later resolve against the then-current document.  This is a tiny
    /// tracked selection, not a snapshot: only pending intents pay any
    /// per-transaction mapping cost.
    pub(crate) fn capture_resource_insert_anchor(
        &mut self,
        selection: Selection,
    ) -> Result<ResourceInsertAnchor, DocumentError> {
        self.document.validate_selection(selection)?;
        let token = self.next_resource_insert_anchor;
        self.next_resource_insert_anchor = self
            .next_resource_insert_anchor
            .checked_add(1)
            .ok_or_else(|| {
                DocumentError::InvalidOperation("resource insert anchor exhausted".into())
            })?;
        self.pending_resource_insert_anchors.insert(
            token,
            PendingResourceInsertAnchor {
                selection: Some(selection),
            },
        );
        Ok(ResourceInsertAnchor {
            token,
            captured_revision: self.document.revision(),
        })
    }

    /// Resolve without consuming. This is deliberately test-visible so
    /// structural mutations can prove that a pending intent is mapped before
    /// its asynchronous source completes.
    pub(crate) fn peek_resource_insert_anchor(
        &self,
        anchor: ResourceInsertAnchor,
    ) -> Result<Selection, DocumentError> {
        let selection = self
            .pending_resource_insert_anchors
            .get(&anchor.token)
            .and_then(|anchor| anchor.selection)
            .ok_or_else(|| {
                DocumentError::InvalidOperation(
                    "resource insertion position is no longer valid".into(),
                )
            })?;
        self.document.validate_selection(selection)?;
        Ok(selection)
    }

    /// Resolve and consume a pending external insertion point. A stale token
    /// is an explicit error rather than a fallback to the live caret.
    pub(crate) fn resolve_resource_insert_anchor(
        &mut self,
        anchor: ResourceInsertAnchor,
    ) -> Result<Selection, DocumentError> {
        let selection = self
            .pending_resource_insert_anchors
            .remove(&anchor.token)
            .and_then(|anchor| anchor.selection)
            .ok_or_else(|| {
                DocumentError::InvalidOperation(
                    "resource insertion position is no longer valid".into(),
                )
            })?;
        self.document.validate_selection(selection)?;
        Ok(selection)
    }

    pub(crate) fn discard_resource_insert_anchor(&mut self, anchor: ResourceInsertAnchor) {
        self.pending_resource_insert_anchors.remove(&anchor.token);
    }

    fn resource_anchor_replacement_for_transaction(
        &self,
        transaction: &Transaction,
    ) -> Option<ResourceAnchorReplacement> {
        self.resource_anchor_replacement_for_range(
            self.replacement_range_for_transaction(transaction)?,
        )
    }

    fn resource_anchor_replacement_for_batch(
        &self,
        batch: &TransactionBatch,
    ) -> Option<ResourceAnchorReplacement> {
        let range = batch
            .0
            .iter()
            .find_map(|transaction| self.replacement_range_for_transaction(transaction))?;
        self.resource_anchor_replacement_for_range(range)
    }

    fn resource_anchor_replacement_for_range(
        &self,
        range: Range<usize>,
    ) -> Option<ResourceAnchorReplacement> {
        if self.pending_resource_insert_anchors.is_empty() {
            return None;
        }
        let anchors = self
            .pending_resource_insert_anchors
            .iter()
            .filter_map(|(token, anchor)| {
                let selection = anchor.selection?;
                let anchor_offset = self.document.flat_offset_for_point(selection.anchor)?;
                let head_offset = self.document.flat_offset_for_point(selection.head)?;
                Some((
                    *token,
                    (anchor_offset, selection.anchor.affinity),
                    (head_offset, selection.head.affinity),
                ))
            })
            .collect::<Vec<_>>();
        (!anchors.is_empty()).then_some(ResourceAnchorReplacement {
            range,
            pre_document_len: self.document.flat_utf8_len(),
            anchors,
        })
    }

    /// Map every pending tracked selection through one document replacement.
    /// The operation stores only offsets for outstanding resource intents;
    /// normal typing retains the existing no-clone hot path.
    fn apply_resource_anchor_replacement(&mut self, mapping: Option<ResourceAnchorReplacement>) {
        let Some(mapping) = mapping else {
            return;
        };
        let post_len = self.document.flat_utf8_len();
        let removed_len = mapping.range.end.saturating_sub(mapping.range.start);
        let inserted_len =
            post_len.saturating_sub(mapping.pre_document_len.saturating_sub(removed_len));
        for (token, (anchor_offset, anchor_affinity), (head_offset, head_affinity)) in
            mapping.anchors
        {
            let mapped_anchor = map_resource_anchor_offset(
                anchor_offset,
                anchor_affinity,
                mapping.range.clone(),
                inserted_len,
            );
            let mapped_head = map_resource_anchor_offset(
                head_offset,
                head_affinity,
                mapping.range.clone(),
                inserted_len,
            );
            let selection = self
                .document
                .point_for_flat_utf8_offset(mapped_anchor, anchor_affinity)
                .zip(
                    self.document
                        .point_for_flat_utf8_offset(mapped_head, head_affinity),
                )
                .map(|(anchor, head)| Selection::new(anchor, head));
            if let Some(anchor) = self.pending_resource_insert_anchors.get_mut(&token) {
                anchor.selection = selection;
            }
        }
    }

    fn replacement_range_for_transaction(&self, transaction: &Transaction) -> Option<Range<usize>> {
        let selection_range = |selection: Selection| self.selection_flat_range(selection);
        match transaction {
            Transaction::InsertText { selection, .. }
            | Transaction::DeleteRange { selection }
            | Transaction::InsertImage { selection, .. }
            | Transaction::InsertAttachment { selection, .. }
            | Transaction::EnsureParagraph { selection } => Some(selection_range(*selection)),
            Transaction::SplitBlock { at } => {
                let offset = self.document.flat_offset_for_point(*at)?;
                Some(offset..offset)
            }
            Transaction::MergeBlocks { left, right } => {
                let left = self.document.block(*left)?;
                let right = self.document.block(*right)?;
                let left_end = self
                    .document
                    .flat_offset_for_point(DocPoint::with_affinity(
                        left.id,
                        left.content.as_text().map_or(0, str::len),
                        Affinity::After,
                    ))?;
                let right_start = self
                    .document
                    .flat_offset_for_point(DocPoint::with_affinity(
                        right.id,
                        0,
                        Affinity::Before,
                    ))?;
                Some(left_end..right_start)
            }
            Transaction::RemoveNode { node_id } => {
                let index = self.document.node_index(*node_id).ok()?;
                let (start, end) = if index + 1 < self.document.block_count() {
                    (
                        self.block_boundary_offset(index)?,
                        self.block_boundary_offset(index + 1)?,
                    )
                } else if index > 0 {
                    // Removing the terminal block also removes its preceding
                    // document separator, so an anchor in it falls back to
                    // the end of the prior stable text block.
                    (
                        self.block_end_offset(index - 1)?,
                        self.document.flat_utf8_len(),
                    )
                } else {
                    (0, self.document.flat_utf8_len())
                };
                Some(start..end)
            }
            Transaction::RestoreBlocks {
                index,
                remove_count,
                ..
            } => {
                let start = self
                    .block_boundary_offset(*index)
                    .unwrap_or_else(|| self.document.flat_utf8_len());
                let end_index = index.saturating_add(*remove_count);
                let end = self
                    .block_boundary_offset(end_index)
                    .unwrap_or_else(|| self.document.flat_utf8_len());
                Some(start..end)
            }
            Transaction::SetBlockKind { .. }
            | Transaction::ToggleMark { .. }
            | Transaction::SetLink { .. }
            | Transaction::SetAlignment { .. }
            | Transaction::IndentList { .. }
            | Transaction::OutdentList { .. }
            | Transaction::SetImageDisplayWidth { .. }
            | Transaction::SetImageNaturalSize { .. } => None,
        }
    }

    fn block_boundary_offset(&self, index: usize) -> Option<usize> {
        let block = self.document.block_at_index(index)?;
        self.document
            .flat_offset_for_point(DocPoint::with_affinity(block.id, 0, Affinity::Before))
    }

    fn block_end_offset(&self, index: usize) -> Option<usize> {
        let block = self.document.block_at_index(index)?;
        self.document.flat_offset_for_point(DocPoint::with_affinity(
            block.id,
            block.content.as_text().map_or(0, str::len),
            Affinity::After,
        ))
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
        let retried = self.image_store.retry_resource(resource_id);
        if retried {
            self.failed_image_hydration.remove(resource_id);
        }
        retried
    }

    pub fn image_source_path(&self, resource_id: &str) -> Option<&std::path::Path> {
        self.image_store.source_path_for_resource(resource_id)
    }

    /// Queue only residency-selected persisted images for a retained session
    /// worker. This performs no I/O; it records resource IDs that still occur
    /// in the live document. A failed request stays failed until retry/reopen
    /// rather than triggering a descriptor scan on every paint frame.
    pub(crate) fn request_image_hydration<I>(&mut self, resource_ids: I) -> bool
    where
        I: IntoIterator<Item = String>,
    {
        // Keep at most one not-yet-started request. A render can run many
        // times while a slow first descriptor is active; retaining every
        // historical viewport would eventually stream an entire long note
        // after one quick scroll. The active request is allowed to finish,
        // while this slot always represents the newest resident/preload set.
        let next = resource_ids.into_iter().find(|resource_id| {
            let still_present = self.document.blocks().iter().any(|block| {
                matches!(
                    &block.content,
                    BlockContent::Image { resource_id: candidate, .. }
                        if candidate == resource_id
                )
            });
            still_present
                && !self.materialized_image_ids.contains(resource_id)
                && !self.active_image_hydration.contains(resource_id)
                && !self.failed_image_hydration.contains(resource_id)
        });
        let previous = self.pending_image_hydration.clone();
        self.pending_image_hydration.clear();
        if let Some(resource_id) = next {
            self.pending_image_hydration.insert(resource_id);
        }
        self.pending_image_hydration != previous
    }

    /// Drain renderer-demanded IDs in document order. `active` prevents a
    /// second paint before completion from starting another verified read for
    /// the same resource.
    pub(crate) fn take_pending_image_hydration_requests(&mut self) -> Vec<String> {
        let Some(resource_id) = self.pending_image_hydration.drain().next() else {
            return Vec::new();
        };
        let still_present = self.document.blocks().iter().any(|block| {
            matches!(
                &block.content,
                BlockContent::Image { resource_id: candidate, .. }
                    if candidate == &resource_id
            )
        });
        if !still_present {
            return Vec::new();
        }
        self.active_image_hydration.insert(resource_id.clone());
        vec![resource_id]
    }

    /// Complete the worker handoff without mutating semantic document state.
    /// The source registration happens separately after a successful worker;
    /// failure keeps the atom a stable visible placeholder.
    pub(crate) fn finish_image_hydration_request(&mut self, resource_id: &str, success: bool) {
        self.active_image_hydration.remove(resource_id);
        if success {
            self.failed_image_hydration.remove(resource_id);
        } else {
            self.failed_image_hydration.insert(resource_id.to_owned());
        }
    }

    pub fn attachment_metadata(&self, resource_id: &str) -> Option<&AttachmentMetadata> {
        self.attachment_metadata.get(resource_id)
    }

    /// Register durable metadata only. Unlike images, attachments do not
    /// pre-copy or decode their bytes during note mount; opening one is an
    /// explicit, descriptor-safe operation owned by `NoteSession`.
    pub fn register_attachment(
        &mut self,
        resource_id: &str,
        size: u64,
    ) -> Result<(), DocumentError> {
        if resource_id.is_empty() {
            return Err(DocumentError::InvalidOperation(
                "an attachment resource id cannot be empty".into(),
            ));
        }
        self.attachment_metadata.insert(
            resource_id.to_owned(),
            AttachmentMetadata::ready(resource_id, size),
        );
        Ok(())
    }

    pub fn register_unavailable_attachment(
        &mut self,
        resource_id: &str,
    ) -> Result<(), DocumentError> {
        if resource_id.is_empty() {
            return Err(DocumentError::InvalidOperation(
                "an attachment resource id cannot be empty".into(),
            ));
        }
        self.attachment_metadata.insert(
            resource_id.to_owned(),
            AttachmentMetadata::unavailable(resource_id),
        );
        Ok(())
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
            && is_atomic_block_kind(&block.kind)
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
                self.materialized_image_ids.remove(&resource_id);
                return Err(error);
            }
        };
        self.materialized_image_ids.insert(resource_id);
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
                self.materialized_image_ids.remove(&resource_id);
                return Err(error);
            }
        };
        self.materialized_image_ids.insert(resource_id);
        self.selection = outcome.selection;
        Ok(())
    }

    /// Attach a successfully committed repository image to this editor's
    /// cache source. The document transaction itself is deliberately separate
    /// and already durable at this point; this method stores no compressed
    /// blob in `EditorCore` after success.
    pub fn register_durable_image(
        &mut self,
        resource_id: &str,
        natural_size: (u32, u32),
        bytes: &[u8],
        format: ImageFormat,
    ) -> Result<(), DocumentError> {
        self.register_durable_image_reader(
            resource_id,
            natural_size,
            std::io::Cursor::new(bytes),
            format,
        )
    }

    /// Attach a durable repository image from a verified stream. This is the
    /// library-session path: only the current image is copied into the
    /// session-private cache source, rather than retaining every persisted
    /// source byte in the prepared session.
    pub fn register_durable_image_reader<R: std::io::Read>(
        &mut self,
        resource_id: &str,
        natural_size: (u32, u32),
        reader: R,
        format: ImageFormat,
    ) -> Result<(), DocumentError> {
        if natural_size.0 == 0 || natural_size.1 == 0 {
            return Err(DocumentError::InvalidOperation(
                "a durable image requires non-zero dimensions".into(),
            ));
        }
        self.image_store
            .insert_durable_reader(
                ImageMetadata::new(resource_id, natural_size.0, natural_size.1),
                reader,
                format,
            )
            .map_err(|error| {
                DocumentError::InvalidOperation(format!(
                    "unable to materialize durable image cache source: {error}"
                ))
            })?;
        self.materialized_image_ids.insert(resource_id.to_owned());
        Ok(())
    }

    /// Return this editor's session-private destination for a background
    /// durable image materialization. The path is never exposed to UI code or
    /// a caller-controlled resource; only `ImageStore` validates and adopts
    /// the matching managed filename after the worker returns.
    pub(crate) fn image_materialization_root(&self) -> PathBuf {
        self.image_store.materialization_root()
    }

    /// Adopt a source that the resource worker already streamed, synced and
    /// atomically renamed inside this exact editor's private cache directory.
    /// No bytes are reread or copied on the GPUI callback.
    pub(crate) fn register_materialized_durable_image(
        &mut self,
        resource_id: &str,
        natural_size: (u32, u32),
        source: PathBuf,
        format: ImageFormat,
    ) -> Result<(), DocumentError> {
        if natural_size.0 == 0 || natural_size.1 == 0 {
            return Err(DocumentError::InvalidOperation(
                "a durable image requires non-zero dimensions".into(),
            ));
        }
        self.image_store
            .register_materialized_durable_source(
                ImageMetadata::new(resource_id, natural_size.0, natural_size.1),
                source,
                format,
            )
            .map_err(|error| {
                DocumentError::InvalidOperation(format!(
                    "unable to register durable image cache source: {error}"
                ))
            })?;
        self.pending_image_hydration.remove(resource_id);
        self.active_image_hydration.remove(resource_id);
        self.failed_image_hydration.remove(resource_id);
        self.materialized_image_ids.insert(resource_id.to_owned());
        Ok(())
    }

    /// Repair first-frame geometry for legacy image HTML only. This bypasses
    /// `History` intentionally: a resource loader must not manufacture a
    /// user undo step, yet the model revision/layout invalidation must still
    /// be real so `NoteSession` captures and persists the repaired geometry.
    pub(crate) fn repair_legacy_image_natural_sizes(
        &mut self,
        resource_id: &str,
        node_ids: &[NodeId],
        natural_size: (u32, u32),
    ) -> Result<bool, DocumentError> {
        // Hydration is presentation work, but a legacy geometry repair is a
        // semantic canonical-document mutation that will be snapshotted. It
        // must obey the same Trash/reconciliation capability as every other
        // document mutation.
        self.ensure_editable()?;
        if natural_size.0 == 0 || natural_size.1 == 0 {
            return Err(DocumentError::InvalidOperation(
                "legacy image natural size must be positive".into(),
            ));
        }
        let transactions = node_ids
            .iter()
            .copied()
            .filter(|node_id| {
                matches!(
                    self.document.block(*node_id).map(|block| &block.content),
                    Some(BlockContent::Image { resource_id: candidate, .. })
                        if candidate == resource_id
                )
            })
            .map(|node_id| Transaction::SetImageNaturalSize {
                node_id,
                natural_size,
            })
            .collect::<Vec<_>>();
        if transactions.is_empty() {
            return Ok(false);
        }
        let outcome = self.document.apply_batch(TransactionBatch(transactions))?;
        if outcome.changed_nodes.is_empty() {
            return Ok(false);
        }
        self.layout.invalidate_nodes_with_delta(
            &self.document,
            &outcome.changed_nodes,
            outcome.structural,
            &outcome.structural_splices,
            &outcome.numbering_ranges,
        );
        Ok(true)
    }

    /// Keep a canonical image atom in the live document when this device
    /// cannot hydrate its blob. The failed node has real selection/layout
    /// geometry and the renderer paints an explicit unavailable state instead
    /// of forcing the entire note through the unsupported-document route.
    pub fn register_unavailable_image(
        &mut self,
        resource_id: &str,
        natural_size: (u32, u32),
    ) -> Result<(), DocumentError> {
        if natural_size.0 == 0 || natural_size.1 == 0 {
            return Err(DocumentError::InvalidOperation(
                "an unavailable image requires non-zero fallback dimensions".into(),
            ));
        }
        self.image_store.insert_unavailable(ImageMetadata::new(
            resource_id,
            natural_size.0,
            natural_size.1,
        ));
        self.materialized_image_ids.remove(resource_id);
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
        #[cfg(test)]
        if let Some(error) = self.next_apply_error_for_test.take() {
            return Err(error);
        }
        let resource_anchor_mapping =
            self.resource_anchor_replacement_for_transaction(&transaction);
        let outcome =
            self.history
                .apply_with_selection(&mut self.document, self.selection, transaction)?;
        self.apply_resource_anchor_replacement(resource_anchor_mapping);
        self.selection = outcome.selection;
        self.preferred_x = None;
        self.clear_composition();
        self.find.reconcile(&self.document);
        self.layout.invalidate_nodes_with_delta(
            &self.document,
            &outcome.changed_nodes,
            outcome.structural,
            &outcome.structural_splices,
            &outcome.numbering_ranges,
        );
        Ok(outcome)
    }

    /// Produce the exact post-transaction document/history before a resource
    /// association is allowed to commit. This intentionally clones only for
    /// the infrequent cross-store resource boundary; ordinary typing keeps
    /// the existing in-place history path.
    pub(crate) fn prepare_durable_transaction(
        &self,
        transaction: Transaction,
    ) -> Result<PreparedEditorCommit, DocumentError> {
        self.ensure_editable()?;
        let mut document = self.document.clone();
        let mut history = self.history.clone();
        let resource_anchor_mapping =
            self.resource_anchor_replacement_for_transaction(&transaction);
        let outcome = history.apply_with_selection(&mut document, self.selection, transaction)?;
        Ok(PreparedEditorCommit {
            document,
            history,
            outcome,
            resource_anchor_mapping,
        })
    }

    /// Install a [`PreparedEditorCommit`] after the matching SQLite snapshot
    /// has succeeded. All fallible model/history work happened in
    /// `prepare_durable_transaction`, making this a non-fallible swap while
    /// preserving the exact undo entry, selection, and localized layout
    /// invalidation that an in-place `apply` would have produced.
    pub(crate) fn install_prepared_durable_commit(&mut self, prepared: PreparedEditorCommit) {
        let PreparedEditorCommit {
            document,
            history,
            outcome,
            resource_anchor_mapping,
        } = prepared;
        self.document = document;
        self.history = history;
        self.apply_resource_anchor_replacement(resource_anchor_mapping);
        self.selection = outcome.selection;
        self.preferred_x = None;
        self.clear_composition();
        self.last_input_error = None;
        self.find.reconcile(&self.document);
        self.layout.invalidate_nodes_with_delta(
            &self.document,
            &outcome.changed_nodes,
            outcome.structural,
            &outcome.structural_splices,
            &outcome.numbering_ranges,
        );
    }

    #[cfg(test)]
    pub(crate) fn fail_next_apply_for_test(&mut self, error: DocumentError) {
        self.next_apply_error_for_test = Some(error);
    }

    fn apply_with_selection(
        &mut self,
        transaction: Transaction,
    ) -> Result<ApplyOutcome, DocumentError> {
        self.ensure_editable()?;
        let resource_anchor_mapping =
            self.resource_anchor_replacement_for_transaction(&transaction);
        let outcome =
            self.history
                .apply_with_selection(&mut self.document, self.selection, transaction)?;
        self.apply_resource_anchor_replacement(resource_anchor_mapping);
        self.preferred_x = None;
        self.find.reconcile(&self.document);
        self.layout.invalidate_nodes_with_delta(
            &self.document,
            &outcome.changed_nodes,
            outcome.structural,
            &outcome.structural_splices,
            &outcome.numbering_ranges,
        );
        Ok(outcome)
    }

    /// The few editor commands that deliberately group several model
    /// operations into one undo entry still map outstanding resource intents
    /// once around the atomic batch. This keeps structural merge/split and
    /// list-boundary shortcuts on the same tracked-selection contract as the
    /// ordinary one-transaction paths.
    fn apply_batch_with_selection(
        &mut self,
        before_selection: Selection,
        batch: TransactionBatch,
    ) -> Result<ApplyOutcome, DocumentError> {
        self.ensure_editable()?;
        let resource_anchor_mapping = self.resource_anchor_replacement_for_batch(&batch);
        let outcome =
            self.history
                .apply_batch_with_selection(&mut self.document, before_selection, batch)?;
        self.apply_resource_anchor_replacement(resource_anchor_mapping);
        self.find.reconcile(&self.document);
        Ok(outcome)
    }

    pub fn undo(&mut self) -> Result<(), DocumentError> {
        self.ensure_editable()?;
        let resource_anchor_mapping = self
            .history
            .next_undo_batch_for_mapping()
            .as_ref()
            .and_then(|batch| self.resource_anchor_replacement_for_batch(batch));
        let outcome = self.history.undo_with_outcome(&mut self.document)?;
        self.apply_resource_anchor_replacement(resource_anchor_mapping);
        self.selection = outcome.selection;
        self.preferred_x = None;
        self.clear_composition();
        self.find.reconcile(&self.document);
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
        let resource_anchor_mapping = self
            .history
            .next_redo_batch_for_mapping()
            .as_ref()
            .and_then(|batch| self.resource_anchor_replacement_for_batch(batch));
        let outcome = self.history.redo_with_outcome(&mut self.document)?;
        self.apply_resource_anchor_replacement(resource_anchor_mapping);
        self.selection = outcome.selection;
        self.preferred_x = None;
        self.clear_composition();
        self.find.reconcile(&self.document);
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
            let resource_anchor_mapping =
                self.resource_anchor_replacement_for_range(marked_range.clone());
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
            self.apply_resource_anchor_replacement(resource_anchor_mapping);
            self.selection = outcome.selection;
            self.preferred_x = None;
            self.find.reconcile(&self.document);
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
            let resource_anchor_mapping =
                self.resource_anchor_replacement_for_range(marked_range.clone());
            let outcome = self.history.undo_with_outcome(&mut self.document)?;
            self.apply_resource_anchor_replacement(resource_anchor_mapping);
            self.selection = outcome.selection;
            self.preferred_x = None;
            self.find.reconcile(&self.document);
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
        let resource_anchor_mapping =
            self.resource_anchor_replacement_for_range(marked_range.clone());
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
        self.apply_resource_anchor_replacement(resource_anchor_mapping);
        self.selection = outcome.selection;
        self.preferred_x = None;
        self.find.reconcile(&self.document);
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
        let next = if is_atomic_block_kind(&self.document.blocks()[index].kind) {
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
        let next = if is_atomic_block_kind(&block.kind) {
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
            if is_atomic_block_kind(&block.kind) {
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
            if is_atomic_block_kind(&block.kind) {
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
        let at = if is_atomic_block_kind(&self.document.blocks()[index].kind) {
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
        let outcome = self.apply_batch_with_selection(selection, TransactionBatch(transactions))?;
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

    /// Materialize a paragraph at the currently selected atomic seam before
    /// input arrives. `EditorSurface` calls this only for a gap/below-tail
    /// pointer hit; a regular click on an image or attachment remains an
    /// atomic selection for resource interaction.
    pub(crate) fn activate_atomic_dead_zone(&mut self) -> Result<bool, DocumentError> {
        let outcome = self.apply(Transaction::EnsureParagraph {
            selection: self.selection,
        })?;
        Ok(!outcome.changed_nodes.is_empty())
    }

    /// Select an image or attachment as one structural atom. This mirrors the
    /// donor's NodeSelection behavior: a click inside rendered resource
    /// content never fabricates a text caret; Delete/Backspace can therefore
    /// delete the selected resource through the ordinary range path.
    pub(crate) fn select_atomic_at(&mut self, position: Point<Pixels>) -> Option<AtomicBlockHit> {
        let node_id = self.layout.atomic_block_at(position)?;
        let index = self.block_index(node_id)?;
        let hit = match &self.document.blocks()[index].content {
            BlockContent::Image { .. } => AtomicBlockHit::Image,
            BlockContent::Attachment { resource_id, .. } => AtomicBlockHit::Attachment {
                resource_id: resource_id.clone(),
            },
            _ => return None,
        };
        self.selection = self.full_block_selection(index);
        self.preferred_x = None;
        self.clear_composition();
        Some(hit)
    }

    /// Revert a resource atom whose optimistic cross-store commit failed
    /// without restoring an old whole-document snapshot over later input.
    ///
    /// `InsertImage`/`InsertAttachment` can replace a real text selection,
    /// so deleting the atom by node id would permanently discard that text.
    /// Instead we find the exact history entry for this staged resource,
    /// temporarily undo newer input, undo and discard the failed insertion,
    /// then redo the newer input.  All operations remain localized history
    /// transactions; the exceptional recovery path may clone once only to
    /// restore the live state if a later operation cannot be replayed.
    /// `Ok(false)` means the caller must retain the staged resource as a
    /// visible retryable item rather than risk losing a person's edits.
    pub(crate) fn rollback_failed_optimistic_resource(
        &mut self,
        resource_id: &str,
    ) -> Result<bool, DocumentError> {
        self.ensure_editable()?;
        let resource_is_live = self.document.blocks().iter().any(|block| {
            matches!(
                &block.content,
                BlockContent::Image { resource_id: id, .. }
                    | BlockContent::Attachment { resource_id: id, .. }
                    if id == resource_id
            )
        });
        if !resource_is_live {
            // A person may already have undone/deleted the atom before its
            // background worker reports. Never leave its redo entry capable
            // of recreating a resource that SQLite rejected.
            self.history.discard_redo();
            self.image_store.remove_resource(resource_id);
            self.materialized_image_ids.remove(resource_id);
            self.attachment_metadata.remove(resource_id);
            return Ok(true);
        }
        let Some(resource_depth) = self.history.undo_depth_for_resource_insert(resource_id) else {
            // The bounded history may have evicted a very old insertion while
            // a worker was stalled. Keep the staged source retryable instead
            // of approximating a destructive inverse from a raw point.
            return Ok(false);
        };
        let later_entries = self.history.undo_depth().saturating_sub(resource_depth);
        let later_forwards = self.history.forward_entries_after_depth(resource_depth);
        let saved_document = self.document.clone();
        let saved_history = self.history.clone();
        let saved_selection = self.selection;
        let saved_preferred_x = self.preferred_x;
        let replay = (|| -> Result<Selection, DocumentError> {
            for _ in 0..later_entries {
                self.history.undo_with_outcome(&mut self.document)?;
            }
            // This inverse restores every original selected block/span, not
            // merely the image node, which is essential for paste-over-text.
            let mut selection = self
                .history
                .undo_with_outcome(&mut self.document)?
                .selection;
            // `undo_with_outcome` transforms entries into redo-local inverse
            // batches. Those ranges refer to the resource-expanded document
            // and cannot simply be redone after its inverse. Drop that redo
            // suffix and replay the saved original transactions against the
            // restored text, one normal history entry at a time.
            self.history.discard_redo();
            for (before_selection, forward) in later_forwards {
                let before_selection =
                    self.rebase_history_before_selection(before_selection, &forward)?;
                selection = self
                    .history
                    .apply_batch_with_selection(&mut self.document, before_selection, forward)?
                    .selection;
            }
            Ok(selection)
        })();
        let selection = match replay {
            Ok(selection) => selection,
            Err(_) => {
                self.document = saved_document;
                self.history = saved_history;
                self.selection = saved_selection;
                self.preferred_x = saved_preferred_x;
                return Ok(false);
            }
        };
        self.selection = selection;
        self.preferred_x = None;
        self.clear_composition();
        self.find.reconcile(&self.document);
        // Replaying a compact history suffix has several intermediate
        // structural deltas. The exceptional path trades a single cache
        // rebuild for correctness instead of feeding stale intermediate
        // splice coordinates into the retained layout index.
        self.layout.clear_exact_cache();
        if !self.document.blocks().iter().any(|block| {
            matches!(
                &block.content,
                BlockContent::Image { resource_id: id, .. }
                    | BlockContent::Attachment { resource_id: id, .. }
                    if id == resource_id
            )
        }) {
            self.image_store.remove_resource(resource_id);
            self.materialized_image_ids.remove(resource_id);
            self.attachment_metadata.remove(resource_id);
        }
        Ok(true)
    }

    /// Map a history entry's old editor selection through a removed resource
    /// insertion. A normal text transaction carries its own exact selection;
    /// use that as the transaction-mapped replacement when the history
    /// bookkeeping selection pointed at a split-right node that the inverse
    /// has intentionally removed. Structural transactions without a valid
    /// mapped selection fail closed into NoteSession's staged-retry path.
    fn rebase_history_before_selection(
        &self,
        before_selection: Selection,
        forward: &TransactionBatch,
    ) -> Result<Selection, DocumentError> {
        if self.document.validate_selection(before_selection).is_ok() {
            return Ok(before_selection);
        }
        forward
            .0
            .iter()
            .find_map(Transaction::selection_hint)
            .filter(|selection| self.document.validate_selection(*selection).is_ok())
            .ok_or_else(|| {
                DocumentError::InvalidOperation(
                    "later edit cannot be safely mapped around failed resource insertion".into(),
                )
            })
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
        if is_atomic_block_kind(&self.document.blocks()[index].kind) {
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
                let outcome = self
                    .apply_batch_with_selection(self.selection, TransactionBatch(transactions))?;
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
            if is_atomic_block_kind(&previous.kind) {
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
                    let outcome = self.apply_batch_with_selection(
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
                    let outcome = self.apply_batch_with_selection(
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
        if is_atomic_block_kind(&self.document.blocks()[index].kind) {
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
            if is_atomic_block_kind(&next.kind) {
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
                let outcome = self
                    .apply_batch_with_selection(self.selection, TransactionBatch(transactions))?;
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
                if is_atomic_block_kind(&candidate.kind) {
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

/// Map one side of a tracked selection through an explicit replacement. The
/// affinity rule is the same one used for IME candidate remapping: a `Before`
/// endpoint stays at the beginning of a replacement, while an `After`
/// endpoint stays after its inserted content. This preserves range direction
/// and makes a picker selection behave like the original editor selection.
fn map_resource_anchor_offset(
    offset: usize,
    affinity: Affinity,
    range: Range<usize>,
    inserted_len: usize,
) -> usize {
    if offset < range.start || (offset == range.start && affinity == Affinity::Before) {
        offset
    } else if offset > range.end || (offset == range.end && affinity == Affinity::After) {
        offset
            .saturating_sub(range.end.saturating_sub(range.start))
            .saturating_add(inserted_len)
    } else if affinity == Affinity::Before {
        range.start
    } else {
        range.start.saturating_add(inserted_len)
    }
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

    fn unmark_text(&mut self, _window: &mut Window, cx: &mut Context<Self>) {
        // A read-only session never owns an IME composition. More
        // importantly, do not let a delayed platform unmark callback mutate
        // composition bookkeeping after Task 4 has deliberately disabled all
        // document-input paths.
        if self.is_read_only() {
            return;
        }
        let had_marked_text = self.marked.take().is_some();
        self.composition_base = None;
        self.composition_base_range = None;
        // A note session intentionally ignores provisional IME updates.
        // Unmarking is the production commit boundary it observes, so this
        // notification must not be omitted or the composed text can remain
        // forever dirty-but-unsaved until an unrelated repaint occurs.
        if had_marked_text {
            cx.notify();
        }
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

/// Image and attachment blocks both occupy one document coordinate.  Keeping
/// this predicate at the editor boundary prevents a rendered attachment card
/// from accidentally becoming a text-like, uneditable dead zone.
fn is_atomic_block_kind(kind: &BlockKind) -> bool {
    matches!(kind, BlockKind::Image | BlockKind::Attachment)
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
