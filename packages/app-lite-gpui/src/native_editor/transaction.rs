//! Transactions understood by the compact native-editor document model.
//!
//! This module deliberately contains no Markdown representation.  The
//! transaction layer is the boundary used by the native editor and keeps
//! structural operations (including images) atomic.

use std::mem::size_of;
use std::ops::Range;

use smallvec::SmallVec;

use super::model::{
    Affinity, Block, BlockKind, DocPoint, InlineGroup, Mark, NodeId, Selection, TextAlignment,
};

/// A single validated edit to a [`Document`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Transaction {
    /// Local semantic-parent metadata inverse, paired with RestoreBlocks.
    RestoreInlineGroups {
        remove: Vec<NodeId>,
        groups: Vec<InlineGroup>,
    },
    InsertText {
        selection: Selection,
        text: String,
    },
    DeleteRange {
        selection: Selection,
    },
    SplitBlock {
        at: DocPoint,
    },
    MergeBlocks {
        left: NodeId,
        right: NodeId,
    },
    SetBlockKind {
        selection: Selection,
        kind: BlockKind,
    },
    ToggleMark {
        selection: Selection,
        mark: Mark,
    },
    /// Peso removeformat: keep links and document structure, remove selected
    /// text's presentation marks and reset its semantic paragraph attributes.
    /// One transaction, with one local inverse, rather than mark toggles.
    ClearFormatting {
        selection: Selection,
    },
    SetLink {
        selection: Selection,
        url: Option<String>,
    },
    /// Evernote forecolor: sets the colour of the selected text, or with
    /// `None` returns it to the default (textformatter/commands/forecolor.ts).
    SetTextColor {
        selection: Selection,
        color: Option<app_lite_core::TextColor>,
    },
    SetHighlightColor {
        selection: Selection,
        color: Option<app_lite_core::TextColor>,
    },
    SetAlignment {
        selection: Selection,
        alignment: TextAlignment,
    },
    /// Evernote's quoteblock toggle (common-editor quoteblock/quoteblock.ts
    /// insertQuoteblockAtSelection): `true` wraps the selected blocks in a
    /// quote, `false` unwraps every whole quote the selection touches.
    SetQuote {
        selection: Selection,
        quote: bool,
    },
    /// Evernote list/plugin.ts handleTodoListMouseEvent: a click on a
    /// checklist item's box flips `checked` (`setNodeMarkup`), leaving the
    /// selection where it was.
    ToggleCheck {
        node: NodeId,
        selection: Selection,
    },
    /// quoteblock/keymap.ts removeEmptyLineAndSplitQuoteblock: the empty
    /// quote paragraph `node` leaves its quote, splitting it if more follows.
    SplitQuoteAt {
        node: NodeId,
    },
    /// quoteblock/keymap.ts handleDelete: the first block of the quote after
    /// `left` joins `left`'s text; the rest of the quote stays.
    JoinQuoteHead {
        left: NodeId,
    },
    IndentList {
        selection: Selection,
    },
    OutdentList {
        selection: Selection,
    },
    InsertImage {
        selection: Selection,
        resource_id: String,
        natural_size: (u32, u32),
    },
    /// Insert a non-image resource as one atomic attachment block. Its bytes
    /// stay in the durable resource store; only display metadata is retained
    /// by the document model.
    InsertAttachment {
        selection: Selection,
        resource_id: String,
        filename: String,
        media_type: String,
    },
    /// Materialize the text insertion point represented by an atomic-block
    /// seam. Pointer hits in the gap between resources and below a terminal
    /// resource use this before focus/IME arrives, so the first character
    /// never has to create a paragraph as a side effect.
    EnsureParagraph {
        selection: Selection,
    },
    RemoveNode {
        node_id: NodeId,
    },
    SetImageDisplayWidth {
        node_id: NodeId,
        display_width: Option<u32>,
    },
    /// Insert a new table atom at the selection (between paragraphs).
    InsertTable {
        selection: Selection,
        table: std::sync::Arc<super::model::TableContent>,
    },
    /// Replace a table atom's whole content (a cell edit or a row/column
    /// change). The inverse restores the original block.
    ReplaceTable {
        node_id: NodeId,
        table: std::sync::Arc<super::model::TableContent>,
    },
    /// Internal presentation repair for a legacy durable image whose old
    /// canonical HTML lacked natural dimensions. `EditorCore` applies this
    /// directly to the model without adding a user-visible history entry.
    SetImageNaturalSize {
        node_id: NodeId,
        natural_size: (u32, u32),
    },
    /// Internal inverse operation that restores only the affected contiguous
    /// block range.  It is intentionally not a document snapshot: history
    /// stores the operation payload for the edit being undone, not the entire
    /// document.
    RestoreBlocks {
        index: usize,
        remove_count: usize,
        blocks: Vec<Block>,
    },
}

impl Transaction {
    /// Return the selection supplied by selection-based operations.
    pub fn selection_hint(&self) -> Option<Selection> {
        match self {
            Self::InsertText { selection, .. }
            | Self::DeleteRange { selection }
            | Self::SetBlockKind { selection, .. }
            | Self::ToggleMark { selection, .. }
            | Self::ClearFormatting { selection }
            | Self::SetLink { selection, .. }
            | Self::SetTextColor { selection, .. }
            | Self::SetHighlightColor { selection, .. }
            | Self::SetAlignment { selection, .. }
            | Self::SetQuote { selection, .. }
            | Self::ToggleCheck { selection, .. }
            | Self::IndentList { selection }
            | Self::OutdentList { selection }
            | Self::InsertImage { selection, .. }
            | Self::InsertAttachment { selection, .. }
            | Self::InsertTable { selection, .. }
            | Self::EnsureParagraph { selection } => Some(*selection),
            Self::SplitBlock { at } => Some(Selection::caret(*at)),
            Self::MergeBlocks { .. }
            | Self::SplitQuoteAt { .. }
            | Self::JoinQuoteHead { .. }
            | Self::RemoveNode { .. }
            | Self::SetImageDisplayWidth { .. }
            | Self::ReplaceTable { .. }
            | Self::SetImageNaturalSize { .. }
            | Self::RestoreBlocks { .. } => None,
            Self::RestoreInlineGroups { .. } => None,
        }
    }

    /// A deterministic upper-bound estimate used for the history byte budget.
    pub fn estimated_bytes(&self) -> usize {
        let base = size_of::<Self>();
        match self {
            Self::InsertText { text, .. } => base.saturating_add(text.len()),
            Self::SetLink { url, .. } => base.saturating_add(url.as_ref().map_or(0, String::len)),
            Self::InsertImage { resource_id, .. } => base.saturating_add(resource_id.len()),
            Self::InsertAttachment {
                resource_id,
                filename,
                media_type,
                ..
            } => base
                .saturating_add(resource_id.len())
                .saturating_add(filename.len())
                .saturating_add(media_type.len()),
            Self::ToggleMark { mark, .. } => base.saturating_add(mark.estimated_bytes()),
            Self::ReplaceTable { table, .. } | Self::InsertTable { table, .. } => base
                .saturating_add(
                    table
                        .rows
                        .iter()
                        .flatten()
                        .map(|cell| cell.len() * 2)
                        .fold(0usize, usize::saturating_add),
                ),
            Self::SetBlockKind { kind, .. } => base.saturating_add(kind.estimated_bytes()),
            Self::RestoreBlocks { blocks, .. } => base.saturating_add(
                blocks
                    .iter()
                    .map(Block::estimated_bytes)
                    .fold(0usize, usize::saturating_add),
            ),
            Self::RestoreInlineGroups { remove, groups } => base
                .saturating_add(remove.len() * size_of::<NodeId>())
                .saturating_add(
                    groups
                        .iter()
                        .map(|group| {
                            size_of::<InlineGroup>() + group.members.len() * size_of::<NodeId>()
                        })
                        .sum::<usize>(),
                ),
            _ => base,
        }
    }
}

/// A sequence of transactions applied atomically by [`Document::apply_batch`].
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TransactionBatch(pub Vec<Transaction>);

impl TransactionBatch {
    pub fn new(transactions: Vec<Transaction>) -> Self {
        Self(transactions)
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    pub fn len(&self) -> usize {
        self.0.len()
    }

    pub fn estimated_bytes(&self) -> usize {
        self.0
            .iter()
            .map(Transaction::estimated_bytes)
            .fold(0usize, usize::saturating_add)
    }
}

/// Exact text span written by an `InsertText` transaction in the resulting
/// document. This is captured before the model snaps the post-edit caret to a
/// grapheme boundary, so composition bookkeeping never has to reconstruct an
/// inserted interval from that public caret.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InsertedTextSpan {
    pub node_id: NodeId,
    pub range: Range<usize>,
}

/// An explicit document-order replacement emitted by the transaction layer.
///
/// `start_index` is relative to the document order immediately before this
/// splice.  A batch therefore carries splices in application order; layout
/// can replay them against its balanced order sequence without comparing the
/// whole document or inferring movement from `changed_nodes`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StructuralSplice {
    pub start_index: usize,
    pub removed: SmallVec<[NodeId; 4]>,
    pub inserted: SmallVec<[NodeId; 4]>,
    /// Revisions captured with the inserted identities. A structural batch
    /// is replayed against its intermediate sequence, so layout must not
    /// recover these items from the final document's ordinals.
    pub inserted_revisions: SmallVec<[u64; 4]>,
}

/// Result of one transaction or an atomically applied batch.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ApplyOutcome {
    pub selection: Selection,
    pub changed_nodes: SmallVec<[NodeId; 4]>,
    /// True when block order or list kind/depth may have changed. Inline
    /// edits leave this false so layout can invalidate numbering in O(1).
    pub structural: bool,
    /// Ordered structural replacements, in sequential transaction order.
    pub structural_splices: SmallVec<[StructuralSplice; 2]>,
    /// Local numbering invalidations for kind/depth edits that do not alter
    /// document order. These ranges are relative to this transaction's
    /// resulting document and remain separate from structural splices.
    pub numbering_ranges: SmallVec<[Range<usize>; 2]>,
    pub inverse: TransactionBatch,
    pub estimated_bytes: usize,
    pub inserted_span: Option<InsertedTextSpan>,
}

impl ApplyOutcome {
    pub(crate) fn empty(selection: Selection) -> Self {
        Self {
            selection,
            changed_nodes: SmallVec::new(),
            structural: false,
            structural_splices: SmallVec::new(),
            numbering_ranges: SmallVec::new(),
            inverse: TransactionBatch::default(),
            estimated_bytes: 0,
            inserted_span: None,
        }
    }
}

/// Create a point for a text boundary with explicit affinity.
pub(crate) fn point(node_id: NodeId, utf8_offset: usize, affinity: Affinity) -> DocPoint {
    DocPoint {
        node_id,
        utf8_offset,
        affinity,
    }
}
