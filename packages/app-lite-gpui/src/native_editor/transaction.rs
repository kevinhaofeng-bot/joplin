//! Transactions understood by the compact native-editor document model.
//!
//! This module deliberately contains no Markdown representation.  The
//! transaction layer is the boundary used by the native editor and keeps
//! structural operations (including images) atomic.

use std::mem::size_of;

use smallvec::SmallVec;

use super::model::{Affinity, Block, BlockKind, DocPoint, Mark, NodeId, Selection, TextAlignment};

/// A single validated edit to a [`Document`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Transaction {
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
    SetLink {
        selection: Selection,
        url: Option<String>,
    },
    SetAlignment {
        selection: Selection,
        alignment: TextAlignment,
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
    RemoveNode {
        node_id: NodeId,
    },
    SetImageDisplayWidth {
        node_id: NodeId,
        display_width: Option<u32>,
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
            | Self::SetLink { selection, .. }
            | Self::SetAlignment { selection, .. }
            | Self::IndentList { selection }
            | Self::OutdentList { selection }
            | Self::InsertImage { selection, .. } => Some(*selection),
            Self::SplitBlock { at } => Some(Selection::caret(*at)),
            Self::MergeBlocks { .. }
            | Self::RemoveNode { .. }
            | Self::SetImageDisplayWidth { .. }
            | Self::RestoreBlocks { .. } => None,
        }
    }

    /// A deterministic upper-bound estimate used for the history byte budget.
    pub fn estimated_bytes(&self) -> usize {
        let base = size_of::<Self>();
        match self {
            Self::InsertText { text, .. } => base.saturating_add(text.len()),
            Self::SetLink { url, .. } => base.saturating_add(url.as_ref().map_or(0, String::len)),
            Self::InsertImage { resource_id, .. } => base.saturating_add(resource_id.len()),
            Self::ToggleMark { mark, .. } => base.saturating_add(mark.estimated_bytes()),
            Self::SetBlockKind { kind, .. } => base.saturating_add(kind.estimated_bytes()),
            Self::RestoreBlocks { blocks, .. } => base.saturating_add(
                blocks
                    .iter()
                    .map(Block::estimated_bytes)
                    .fold(0usize, usize::saturating_add),
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

/// Result of one transaction or an atomically applied batch.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ApplyOutcome {
    pub selection: Selection,
    pub changed_nodes: SmallVec<[NodeId; 4]>,
    pub inverse: TransactionBatch,
    pub estimated_bytes: usize,
}

impl ApplyOutcome {
    pub(crate) fn empty(selection: Selection) -> Self {
        Self {
            selection,
            changed_nodes: SmallVec::new(),
            inverse: TransactionBatch::default(),
            estimated_bytes: 0,
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
