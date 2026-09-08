//! Compact, structured document model for the native editor spike.
//!
//! The model is intentionally independent from the donor Markdown editor.
//! Blocks are kept in a contiguous vector for the spike while positions use
//! stable node identities, allowing the transaction layer to own structural
//! edits and history to store inverse operations.

use std::cmp::Ordering;
use std::fmt;
use std::ops::Range;

use smallvec::SmallVec;
use unicode_segmentation::UnicodeSegmentation;

use super::transaction::{ApplyOutcome, Transaction, TransactionBatch};

/// Maximum nesting depth accepted by list transactions.
pub const MAX_LIST_DEPTH: u8 = 64;

/// Stable identity for a block in a document.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct NodeId(u64);

impl NodeId {
    pub(crate) const fn new_internal(raw: u64) -> Self {
        Self(raw)
    }

    pub const fn new(raw: u64) -> Self {
        Self(raw)
    }

    /// Construct an identity from a raw value.  Zero is reserved as an
    /// invalid/unallocated identity by [`Document`].
    pub const fn from_raw(raw: u64) -> Self {
        Self(raw)
    }

    pub const fn raw(self) -> u64 {
        self.0
    }
}

/// The structural role of a block.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum BlockKind {
    Paragraph,
    Heading { level: u8 },
    BulletItem { depth: u8 },
    OrderedItem { depth: u8 },
    CheckItem { depth: u8, checked: bool },
    Quote,
    Code,
    Image,
    Attachment,
    Divider,
}

impl BlockKind {
    pub(crate) fn estimated_bytes(&self) -> usize {
        std::mem::size_of::<Self>()
    }
}

/// Horizontal alignment of a block.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TextAlignment {
    Left,
    Center,
    Right,
}

impl Default for TextAlignment {
    fn default() -> Self {
        Self::Left
    }
}

/// Which side of a boundary a caret is visually associated with.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Affinity {
    Before,
    After,
}

/// A composable inline mark.  A `StyledRun` stores a set of these instead of
/// representing each mark as an overlapping range.
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Mark {
    Bold,
    Italic,
    Underline,
    Strike,
    Highlight,
    Link(String),
    InlineCode,
}

impl Mark {
    pub(crate) fn estimated_bytes(&self) -> usize {
        std::mem::size_of::<Self>()
            + match self {
                Self::Link(url) => url.len(),
                _ => 0,
            }
    }
}

/// A non-overlapping byte range and its normalized mark set.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StyledRun {
    pub range: Range<usize>,
    pub marks: SmallVec<[Mark; 4]>,
}

impl StyledRun {
    pub fn new(range: Range<usize>, marks: impl IntoIterator<Item = Mark>) -> Self {
        let mut marks: SmallVec<[Mark; 4]> = marks.into_iter().collect();
        normalize_marks(&mut marks);
        Self { range, marks }
    }
}

/// Content owned by one structural block.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum BlockContent {
    Text {
        text: String,
        styles: SmallVec<[StyledRun; 4]>,
    },
    Image {
        resource_id: String,
        natural_size: (u32, u32),
        display_width: Option<u32>,
    },
    Attachment {
        resource_id: String,
        filename: String,
        media_type: String,
    },
    Empty,
}

impl BlockContent {
    pub fn text(text: impl Into<String>) -> Self {
        Self::Text {
            text: text.into(),
            styles: SmallVec::new(),
        }
    }

    pub fn as_text(&self) -> Option<&str> {
        match self {
            Self::Text { text, .. } => Some(text),
            _ => None,
        }
    }

    pub fn styles(&self) -> Option<&[StyledRun]> {
        match self {
            Self::Text { styles, .. } => Some(styles),
            _ => None,
        }
    }
}

/// One document block.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Block {
    pub id: NodeId,
    pub kind: BlockKind,
    pub content: BlockContent,
    pub alignment: TextAlignment,
    pub revision: u64,
}

impl Block {
    fn text(id: NodeId, kind: BlockKind, text: String) -> Self {
        Self {
            id,
            kind,
            content: BlockContent::text(text),
            alignment: TextAlignment::Left,
            revision: 0,
        }
    }

    pub(crate) fn estimated_bytes(&self) -> usize {
        let content_bytes = match &self.content {
            BlockContent::Text { text, styles } => {
                text.len()
                    + styles
                        .iter()
                        .map(|run| {
                            run.range.len()
                                + run
                                    .marks
                                    .iter()
                                    .map(|mark| {
                                        std::mem::size_of::<Mark>()
                                            + match mark {
                                                Mark::Link(url) => url.len(),
                                                _ => 0,
                                            }
                                    })
                                    .sum::<usize>()
                        })
                        .sum::<usize>()
            }
            BlockContent::Image { resource_id, .. } => resource_id.len(),
            BlockContent::Attachment {
                resource_id,
                filename,
                media_type,
            } => resource_id.len() + filename.len() + media_type.len(),
            BlockContent::Empty => 0,
        };
        std::mem::size_of::<Self>() + content_bytes
    }
}

/// A stable position in a text block.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DocPoint {
    pub node_id: NodeId,
    pub utf8_offset: usize,
    pub affinity: Affinity,
}

impl DocPoint {
    pub const fn new(node_id: NodeId, utf8_offset: usize) -> Self {
        Self {
            node_id,
            utf8_offset,
            affinity: Affinity::After,
        }
    }

    pub const fn with_affinity(node_id: NodeId, utf8_offset: usize, affinity: Affinity) -> Self {
        Self {
            node_id,
            utf8_offset,
            affinity,
        }
    }
}

/// A directional selection.  Transactions normalize its endpoints by
/// document order when they need a range, while preserving the original
/// anchor/head for the returned caret state.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Selection {
    pub anchor: DocPoint,
    pub head: DocPoint,
}

impl Selection {
    pub const fn new(anchor: DocPoint, head: DocPoint) -> Self {
        Self { anchor, head }
    }

    pub const fn caret(point: DocPoint) -> Self {
        Self {
            anchor: point,
            head: point,
        }
    }

    pub fn is_caret(self) -> bool {
        self.anchor == self.head
    }
}

/// Errors raised before a transaction is committed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DocumentError {
    NodeNotFound(NodeId),
    InvalidUtf8Offset { node_id: NodeId, offset: usize },
    InvalidGraphemeOffset { node_id: NodeId, offset: usize },
    InvalidSelectionOrder,
    InvalidAffinity { node_id: NodeId, offset: usize },
    InvalidHeadingLevel(u8),
    InvalidListDepth(u8),
    InvalidBlockContent(NodeId),
    InvalidOperation(String),
    CannotRemoveLastNode,
    HistoryEmpty,
}

impl fmt::Display for DocumentError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NodeNotFound(id) => write!(f, "node {} was not found", id.raw()),
            Self::InvalidUtf8Offset { node_id, offset } => {
                write!(
                    f,
                    "offset {offset} is not a UTF-8 boundary in node {}",
                    node_id.raw()
                )
            }
            Self::InvalidGraphemeOffset { node_id, offset } => write!(
                f,
                "offset {offset} is not a grapheme boundary in node {}",
                node_id.raw()
            ),
            Self::InvalidSelectionOrder => write!(f, "selection endpoints are not ordered"),
            Self::InvalidAffinity { node_id, offset } => write!(
                f,
                "invalid affinity at offset {offset} in node {}",
                node_id.raw()
            ),
            Self::InvalidHeadingLevel(level) => write!(f, "heading level {level} is invalid"),
            Self::InvalidListDepth(depth) => write!(f, "list depth {depth} is invalid"),
            Self::InvalidBlockContent(id) => {
                write!(
                    f,
                    "block {} has content incompatible with its kind",
                    id.raw()
                )
            }
            Self::InvalidOperation(message) => f.write_str(message),
            Self::CannotRemoveLastNode => f.write_str("a document must retain one block"),
            Self::HistoryEmpty => f.write_str("history has no entry"),
        }
    }
}

impl std::error::Error for DocumentError {}

/// Semantic document data used by tests and round-trip checks.  Revision
/// counters and the allocation cursor intentionally do not participate.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SemanticSnapshot {
    pub blocks: Vec<Block>,
}

/// Compact structured document.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Document {
    blocks: Vec<Block>,
    next_id: u64,
    revision: u64,
}

impl Document {
    pub fn new() -> Self {
        Self::from_paragraph("")
    }

    pub fn from_paragraph(text: impl Into<String>) -> Self {
        Self::from_paragraphs([text.into()])
    }

    pub fn from_paragraphs<I, S>(paragraphs: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        let mut next_id = 1u64;
        let mut blocks = Vec::new();
        for paragraph in paragraphs {
            let id = NodeId::new_internal(next_id);
            next_id = next_id.saturating_add(1);
            blocks.push(Block::text(id, BlockKind::Paragraph, paragraph.into()));
        }
        if blocks.is_empty() {
            blocks.push(Block::text(
                NodeId::new_internal(next_id),
                BlockKind::Paragraph,
                String::new(),
            ));
            next_id = next_id.saturating_add(1);
        }
        let document = Self {
            blocks,
            next_id,
            revision: 0,
        };
        debug_assert!(document.validate_invariants().is_ok());
        document
    }

    pub fn from_blocks(blocks: Vec<Block>) -> Result<Self, DocumentError> {
        let next_id = blocks
            .iter()
            .map(|block| block.id.raw())
            .max()
            .unwrap_or(0)
            .saturating_add(1);
        let document = Self {
            blocks,
            next_id: next_id.max(1),
            revision: 0,
        };
        document.validate_invariants()?;
        Ok(document)
    }

    pub fn blocks(&self) -> &[Block] {
        &self.blocks
    }

    pub fn block(&self, node_id: NodeId) -> Option<&Block> {
        self.blocks.iter().find(|block| block.id == node_id)
    }

    pub fn block_at_index(&self, index: usize) -> Option<&Block> {
        self.blocks.get(index)
    }

    pub fn block_count(&self) -> usize {
        self.blocks.len()
    }

    pub fn revision(&self) -> u64 {
        self.revision
    }

    pub fn first_node_id(&self) -> Option<NodeId> {
        self.blocks.first().map(|block| block.id)
    }

    pub fn block_kinds(&self) -> Vec<BlockKind> {
        self.blocks.iter().map(|block| block.kind.clone()).collect()
    }

    pub fn text_at_index(&self, index: usize) -> Option<&str> {
        self.blocks
            .get(index)
            .and_then(|block| block.content.as_text())
    }

    pub fn select_all_text(&self) -> Selection {
        let first = self
            .blocks
            .iter()
            .find(|block| matches!(block.content, BlockContent::Text { .. }));
        let last = self
            .blocks
            .iter()
            .rev()
            .find(|block| matches!(block.content, BlockContent::Text { .. }));
        match (first, last) {
            (Some(first), Some(last)) => {
                let first_point = DocPoint::with_affinity(first.id, 0, Affinity::Before);
                let last_offset = last.content.as_text().map_or(0, str::len);
                let last_point = DocPoint::with_affinity(last.id, last_offset, Affinity::After);
                Selection::new(first_point, last_point)
            }
            _ => Selection::caret(DocPoint::new(NodeId::new_internal(0), 0)),
        }
    }

    pub fn end_selection(&self) -> Selection {
        if let Some(block) = self
            .blocks
            .iter()
            .rev()
            .find(|block| matches!(block.content, BlockContent::Text { .. }))
        {
            return Selection::caret(DocPoint::with_affinity(
                block.id,
                block.content.as_text().map_or(0, str::len),
                Affinity::After,
            ));
        }
        Selection::caret(DocPoint::new(NodeId::new_internal(0), 0))
    }

    pub fn semantic_snapshot(&self) -> SemanticSnapshot {
        let mut blocks = self.blocks.clone();
        for block in &mut blocks {
            block.revision = 0;
        }
        SemanticSnapshot { blocks }
    }

    /// Apply one operation atomically.
    pub fn apply(&mut self, transaction: Transaction) -> Result<ApplyOutcome, DocumentError> {
        self.apply_batch(TransactionBatch(vec![transaction]))
    }

    /// Apply all operations in a batch atomically.  Each successful operation
    /// contributes its local inverse to a rollback journal; no full-document
    /// clone is made for a keystroke transaction.  The journal is replayed if
    /// a later operation fails.
    pub fn apply_batch(&mut self, batch: TransactionBatch) -> Result<ApplyOutcome, DocumentError> {
        if batch.is_empty() {
            return Ok(ApplyOutcome::empty(self.end_selection()));
        }

        let mut changed_nodes: SmallVec<[NodeId; 4]> = SmallVec::new();
        let mut inverse_batches = Vec::new();
        let mut selection = self.end_selection();
        let mut estimated_bytes = 0usize;
        let initial_revision = self.revision;

        for transaction in batch.0 {
            let outcome = match self.apply_transaction(transaction) {
                Ok(outcome) => outcome,
                Err(error) => {
                    self.rollback_journal(inverse_batches, initial_revision);
                    return Err(error);
                }
            };
            selection = outcome.selection;
            for node_id in outcome.changed_nodes {
                push_unique(&mut changed_nodes, node_id);
            }
            estimated_bytes = estimated_bytes.saturating_add(outcome.estimated_bytes);
            inverse_batches.push(outcome.inverse);
        }

        let rollback_batches = inverse_batches.clone();
        let mut inverse = Vec::new();
        for batch in inverse_batches.into_iter().rev() {
            inverse.extend(batch.0);
        }

        if let Err(error) = self.validate_invariants() {
            self.rollback_journal(rollback_batches, initial_revision);
            return Err(error);
        }
        Ok(ApplyOutcome {
            selection,
            changed_nodes,
            inverse: TransactionBatch(inverse),
            estimated_bytes,
        })
    }

    fn rollback_journal(&mut self, inverse_batches: Vec<TransactionBatch>, revision: u64) {
        for inverse in inverse_batches.into_iter().rev() {
            for transaction in inverse.0.into_iter().rev() {
                let _ = self.apply_transaction(transaction);
            }
        }
        self.revision = revision;
    }

    /// Check all structural and inline invariants without changing anything.
    pub fn validate_invariants(&self) -> Result<(), DocumentError> {
        let mut ids = std::collections::HashSet::new();
        for block in &self.blocks {
            if block.id.raw() == 0 || !ids.insert(block.id) {
                return Err(DocumentError::InvalidOperation(
                    "document contains a duplicate or zero node id".into(),
                ));
            }
            validate_kind(&block.kind)?;
            match (&block.kind, &block.content) {
                (BlockKind::Image, BlockContent::Image { .. })
                | (BlockKind::Attachment, BlockContent::Attachment { .. })
                | (BlockKind::Divider, BlockContent::Empty) => {}
                (
                    BlockKind::Paragraph
                    | BlockKind::Heading { .. }
                    | BlockKind::BulletItem { .. }
                    | BlockKind::OrderedItem { .. }
                    | BlockKind::CheckItem { .. }
                    | BlockKind::Quote
                    | BlockKind::Code,
                    BlockContent::Text { text, styles },
                ) => validate_styles(block.id, text, styles)?,
                _ => return Err(DocumentError::InvalidBlockContent(block.id)),
            }
        }
        Ok(())
    }

    fn node_index(&self, node_id: NodeId) -> Result<usize, DocumentError> {
        self.blocks
            .iter()
            .position(|block| block.id == node_id)
            .ok_or(DocumentError::NodeNotFound(node_id))
    }

    fn new_node_id(&mut self) -> NodeId {
        let id = NodeId::new_internal(self.next_id.max(1));
        self.next_id = self.next_id.saturating_add(1).max(1);
        id
    }

    fn apply_transaction(
        &mut self,
        transaction: Transaction,
    ) -> Result<ApplyOutcome, DocumentError> {
        let original_revision = self.revision;
        let (selection, changed_nodes, inverse) = match transaction {
            Transaction::InsertText { selection, text } => {
                self.apply_insert_text(selection, text)?
            }
            Transaction::DeleteRange { selection } => self.apply_delete_range(selection)?,
            Transaction::SplitBlock { at } => self.apply_split_block(at)?,
            Transaction::MergeBlocks { left, right } => self.apply_merge_blocks(left, right)?,
            Transaction::SetBlockKind { selection, kind } => {
                self.apply_set_block_kind(selection, kind)?
            }
            Transaction::ToggleMark { selection, mark } => {
                self.apply_toggle_mark(selection, mark)?
            }
            Transaction::SetLink { selection, url } => self.apply_set_link(selection, url)?,
            Transaction::SetAlignment {
                selection,
                alignment,
            } => self.apply_set_alignment(selection, alignment)?,
            Transaction::IndentList { selection } => self.apply_indent_list(selection)?,
            Transaction::OutdentList { selection } => self.apply_outdent_list(selection)?,
            Transaction::InsertImage {
                selection,
                resource_id,
                natural_size,
            } => self.apply_insert_image(selection, resource_id, natural_size)?,
            Transaction::RemoveNode { node_id } => self.apply_remove_node(node_id)?,
            Transaction::SetImageDisplayWidth {
                node_id,
                display_width,
            } => self.apply_set_image_width(node_id, display_width)?,
            Transaction::RestoreBlocks {
                index,
                remove_count,
                blocks,
            } => self.apply_restore_blocks(index, remove_count, blocks)?,
        };

        let mut changed_nodes = changed_nodes;
        if changed_nodes.is_empty() {
            return Ok(ApplyOutcome::empty(selection));
        }

        if let Err(error) = self.validate_selection(selection) {
            self.rollback_journal(vec![inverse], original_revision);
            return Err(error);
        }
        self.revision = self.revision.saturating_add(1);
        for node_id in &changed_nodes {
            if let Some(block) = self.blocks.iter_mut().find(|block| block.id == *node_id) {
                block.revision = self.revision;
            }
        }
        if let Err(error) = self.validate_invariants() {
            self.rollback_journal(vec![inverse], original_revision);
            return Err(error);
        }
        let estimated_bytes = inverse.estimated_bytes();
        Ok(ApplyOutcome {
            selection,
            changed_nodes: {
                // Keep the helper's deterministic insertion order while
                // ensuring callers cannot observe duplicate IDs.
                let mut unique = SmallVec::new();
                for node_id in changed_nodes.drain(..) {
                    push_unique(&mut unique, node_id);
                }
                unique
            },
            inverse,
            estimated_bytes,
        })
    }

    fn validate_selection(&self, selection: Selection) -> Result<(), DocumentError> {
        self.validate_point(selection.anchor)?;
        self.validate_point(selection.head)?;
        let _ = self.selection_bounds(selection)?;
        Ok(())
    }

    fn validate_point(&self, point: DocPoint) -> Result<usize, DocumentError> {
        let index = self.node_index(point.node_id)?;
        let block = &self.blocks[index];
        if let Some(text) = block.content.as_text() {
            if point.utf8_offset > text.len() || !text.is_char_boundary(point.utf8_offset) {
                return Err(DocumentError::InvalidUtf8Offset {
                    node_id: point.node_id,
                    offset: point.utf8_offset,
                });
            }
            if !is_grapheme_boundary(text, point.utf8_offset) {
                return Err(DocumentError::InvalidGraphemeOffset {
                    node_id: point.node_id,
                    offset: point.utf8_offset,
                });
            }
        } else if point.utf8_offset != 0 {
            return Err(DocumentError::InvalidUtf8Offset {
                node_id: point.node_id,
                offset: point.utf8_offset,
            });
        }
        // Both affinities are valid at a grapheme boundary.  Keeping this
        // explicit check here makes affinity part of the validated position
        // contract and leaves room for visual-line constraints later.
        match point.affinity {
            Affinity::Before | Affinity::After => {}
        }
        Ok(index)
    }

    /// Return the normalized (start index, start offset, end index, end
    /// offset) range.  Anchor/head direction is preserved by `Selection`, but
    /// range operations always work from the earlier endpoint.
    fn selection_bounds(
        &self,
        selection: Selection,
    ) -> Result<(usize, usize, usize, usize), DocumentError> {
        let anchor_index = self.validate_point(selection.anchor)?;
        let head_index = self.validate_point(selection.head)?;
        let anchor = (
            anchor_index,
            selection.anchor.utf8_offset,
            affinity_order(selection.anchor.affinity),
        );
        let head = (
            head_index,
            selection.head.utf8_offset,
            affinity_order(selection.head.affinity),
        );
        match anchor.cmp(&head) {
            Ordering::Less | Ordering::Equal => Ok((
                anchor_index,
                selection.anchor.utf8_offset,
                head_index,
                selection.head.utf8_offset,
            )),
            Ordering::Greater => Ok((
                head_index,
                selection.head.utf8_offset,
                anchor_index,
                selection.anchor.utf8_offset,
            )),
        }
    }

    fn text_range_for_block(
        &self,
        index: usize,
        start_index: usize,
        start_offset: usize,
        end_index: usize,
        end_offset: usize,
    ) -> Option<(usize, usize)> {
        if index < start_index || index > end_index {
            None
        } else if start_index == end_index {
            Some((start_offset, end_offset))
        } else if index == start_index {
            self.blocks[index]
                .content
                .as_text()
                .map(|text| (start_offset, text.len()))
        } else if index == end_index {
            Some((0, end_offset))
        } else {
            self.blocks[index]
                .content
                .as_text()
                .map(|text| (0, text.len()))
        }
    }

    fn ensure_text_blocks(
        &self,
        start_index: usize,
        end_index: usize,
    ) -> Result<(), DocumentError> {
        for block in &self.blocks[start_index..=end_index] {
            if !matches!(block.content, BlockContent::Text { .. }) {
                return Err(DocumentError::InvalidBlockContent(block.id));
            }
        }
        Ok(())
    }

    fn ensure_editable_range(
        &self,
        start_index: usize,
        end_index: usize,
    ) -> Result<(), DocumentError> {
        let start = &self.blocks[start_index];
        let end = &self.blocks[end_index];
        if start_index == end_index {
            return Ok(());
        }
        if !is_text_block(start) && !is_structural_block(start)
            || !is_text_block(end) && !is_structural_block(end)
        {
            return Err(DocumentError::InvalidOperation(
                "range endpoints are not editable blocks".into(),
            ));
        }
        Ok(())
    }

    fn apply_insert_text(
        &mut self,
        selection: Selection,
        text: String,
    ) -> Result<(Selection, SmallVec<[NodeId; 4]>, TransactionBatch), DocumentError> {
        let (start_index, start_offset, end_index, end_offset) =
            self.selection_bounds(selection)?;
        self.ensure_editable_range(start_index, end_index)?;

        if start_index == end_index && !is_text_block(&self.blocks[start_index]) {
            let original = self.blocks[start_index].clone();
            if selection.is_caret() {
                if text.is_empty() {
                    return Ok((selection, SmallVec::new(), TransactionBatch::default()));
                }
                let inserted = Block::text(self.new_node_id(), BlockKind::Paragraph, text.clone());
                let inserted_id = inserted.id;
                let insert_at = match selection.anchor.affinity {
                    Affinity::Before => start_index,
                    Affinity::After => start_index.saturating_add(1),
                };
                self.blocks.insert(insert_at, inserted);
                let mut changed_nodes = SmallVec::new();
                push_unique(&mut changed_nodes, inserted_id);
                let after = Selection::caret(DocPoint::with_affinity(
                    inserted_id,
                    text.len(),
                    Affinity::After,
                ));
                return Ok((
                    after,
                    changed_nodes,
                    TransactionBatch(vec![Transaction::RestoreBlocks {
                        index: insert_at,
                        remove_count: 1,
                        blocks: Vec::new(),
                    }]),
                ));
            }

            let block_id = original.id;
            let replacement = Block::text(block_id, BlockKind::Paragraph, text.clone());
            self.blocks[start_index] = replacement;
            let mut changed_nodes = SmallVec::new();
            push_unique(&mut changed_nodes, block_id);
            let after = Selection::caret(DocPoint::with_affinity(
                block_id,
                text.len(),
                Affinity::After,
            ));
            return Ok((
                after,
                changed_nodes,
                TransactionBatch(vec![Transaction::RestoreBlocks {
                    index: start_index,
                    remove_count: 1,
                    blocks: vec![original],
                }]),
            ));
        }

        let originals = self.blocks[start_index..=end_index].to_vec();
        let original_ids = originals.iter().map(|block| block.id).collect::<Vec<_>>();
        let is_empty = start_index == end_index && start_offset == end_offset;

        if text.is_empty() && is_empty {
            return Ok((selection, SmallVec::new(), TransactionBatch::default()));
        }

        let insertion_offset = if !is_empty {
            self.delete_range_mut(start_index, start_offset, end_index, end_offset)?
        } else {
            start_offset
        };

        let block_id = self.blocks[start_index].id;
        let inserted_len = text.len();
        let block = &mut self.blocks[start_index];
        let (old_text, old_styles) = text_parts(&block.content)?;
        let mut new_text = String::with_capacity(old_text.len() + inserted_len);
        new_text.push_str(&old_text[..insertion_offset]);
        new_text.push_str(&text);
        new_text.push_str(&old_text[insertion_offset..]);
        let mut new_styles = insert_styles(old_styles, insertion_offset, inserted_len);
        normalize_styles_for_text(&new_text, &mut new_styles);
        block.content = BlockContent::Text {
            text: new_text,
            styles: new_styles,
        };

        let mut changed_nodes = SmallVec::new();
        for node_id in original_ids {
            push_unique(&mut changed_nodes, node_id);
        }
        push_unique(&mut changed_nodes, block_id);
        let after_offset = {
            let text = self.blocks[start_index]
                .content
                .as_text()
                .expect("insert text retains a text block");
            resolve_grapheme_offset(
                text,
                insertion_offset.saturating_add(inserted_len),
                Affinity::After,
            )
        };
        let after = Selection::caret(DocPoint::with_affinity(
            block_id,
            after_offset,
            Affinity::After,
        ));
        let inverse = TransactionBatch(vec![Transaction::RestoreBlocks {
            index: start_index,
            remove_count: 1,
            blocks: originals,
        }]);
        Ok((after, changed_nodes, inverse))
    }

    fn apply_delete_range(
        &mut self,
        selection: Selection,
    ) -> Result<(Selection, SmallVec<[NodeId; 4]>, TransactionBatch), DocumentError> {
        let (start_index, start_offset, end_index, end_offset) =
            self.selection_bounds(selection)?;
        self.ensure_editable_range(start_index, end_index)?;

        if start_index == end_index && !is_text_block(&self.blocks[start_index]) {
            if selection.is_caret() {
                return Ok((
                    Selection::caret(selection.anchor),
                    SmallVec::new(),
                    TransactionBatch::default(),
                ));
            }
            if self.blocks.len() <= 1 {
                return Err(DocumentError::CannotRemoveLastNode);
            }
            let removed = self.blocks.remove(start_index);
            let mut changed_nodes = SmallVec::new();
            push_unique(&mut changed_nodes, removed.id);
            return Ok((
                self.selection_near_index(start_index),
                changed_nodes,
                TransactionBatch(vec![Transaction::RestoreBlocks {
                    index: start_index,
                    remove_count: 0,
                    blocks: vec![removed],
                }]),
            ));
        }

        if start_index == end_index && start_offset == end_offset {
            return Ok((
                Selection::caret(DocPoint::with_affinity(
                    selection.anchor.node_id,
                    selection.anchor.utf8_offset,
                    selection.anchor.affinity,
                )),
                SmallVec::new(),
                TransactionBatch::default(),
            ));
        }

        let originals = self.blocks[start_index..=end_index].to_vec();
        let original_ids = originals.iter().map(|block| block.id).collect::<Vec<_>>();
        let result_offset =
            self.delete_range_mut(start_index, start_offset, end_index, end_offset)?;
        let node_id = self.blocks[start_index].id;
        let mut changed_nodes = SmallVec::new();
        for node_id in original_ids {
            push_unique(&mut changed_nodes, node_id);
        }
        push_unique(&mut changed_nodes, node_id);
        let after = Selection::caret(DocPoint::with_affinity(
            node_id,
            result_offset,
            Affinity::After,
        ));
        let inverse = TransactionBatch(vec![Transaction::RestoreBlocks {
            index: start_index,
            remove_count: 1,
            blocks: originals,
        }]);
        Ok((after, changed_nodes, inverse))
    }

    /// Delete a normalized range while retaining one merged start block.
    fn delete_range_mut(
        &mut self,
        start_index: usize,
        start_offset: usize,
        end_index: usize,
        end_offset: usize,
    ) -> Result<usize, DocumentError> {
        if start_index == end_index {
            let block = &mut self.blocks[start_index];
            let (text, styles) = text_parts(&block.content)?;
            let (new_text, new_styles) = delete_text(text, styles, start_offset, end_offset);
            let result_offset = resolve_grapheme_offset(&new_text, start_offset, Affinity::After);
            block.content = BlockContent::Text {
                text: new_text,
                styles: new_styles,
            };
            return Ok(result_offset);
        }

        let start_block = self.blocks[start_index].clone();
        let end_block = self.blocks[end_index].clone();
        let (prefix, prefix_styles) = match &start_block.content {
            BlockContent::Text { text, styles } => (
                text[..start_offset].to_owned(),
                clip_styles(styles, 0, start_offset, 0),
            ),
            _ => (String::new(), SmallVec::new()),
        };
        let (suffix, suffix_styles) = match &end_block.content {
            BlockContent::Text { text, styles } => (
                text[end_offset..].to_owned(),
                clip_styles(
                    styles,
                    end_offset,
                    text.len(),
                    prefix.len() as isize - end_offset as isize,
                ),
            ),
            _ => (String::new(), SmallVec::new()),
        };
        let mut merged_text = String::with_capacity(prefix.len() + suffix.len());
        merged_text.push_str(&prefix);
        merged_text.push_str(&suffix);
        let mut merged_styles = prefix_styles;
        merged_styles.extend(suffix_styles);
        normalize_styles_for_text(&merged_text, &mut merged_styles);
        let start_is_text = is_text_block(&start_block);
        let merged = Block {
            id: start_block.id,
            kind: if start_is_text {
                start_block.kind
            } else {
                BlockKind::Paragraph
            },
            content: BlockContent::Text {
                text: merged_text,
                styles: merged_styles,
            },
            alignment: if start_is_text {
                start_block.alignment
            } else {
                TextAlignment::Left
            },
            revision: start_block.revision,
        };
        let result_offset = resolve_grapheme_offset(
            merged.content.as_text().expect("merged block is text"),
            prefix.len(),
            Affinity::After,
        );
        self.blocks.splice(start_index..=end_index, [merged]);
        Ok(result_offset)
    }

    fn apply_split_block(
        &mut self,
        at: DocPoint,
    ) -> Result<(Selection, SmallVec<[NodeId; 4]>, TransactionBatch), DocumentError> {
        let index = self.validate_point(at)?;
        let original = self.blocks[index].clone();
        let (text, styles) = text_parts(&original.content)?;
        let (left_text, left_styles, right_text, right_styles) =
            split_text(text, styles, at.utf8_offset);
        let right_id = self.new_node_id();
        let left = Block {
            id: original.id,
            kind: original.kind.clone(),
            content: BlockContent::Text {
                text: left_text,
                styles: left_styles,
            },
            alignment: original.alignment,
            revision: original.revision,
        };
        let right = Block {
            id: right_id,
            kind: original.kind.clone(),
            content: BlockContent::Text {
                text: right_text,
                styles: right_styles,
            },
            alignment: original.alignment,
            revision: original.revision,
        };
        self.blocks.splice(index..=index, [left, right]);
        let mut changed_nodes = SmallVec::new();
        push_unique(&mut changed_nodes, original.id);
        push_unique(&mut changed_nodes, right_id);
        let selection = Selection::caret(DocPoint::with_affinity(right_id, 0, Affinity::Before));
        let inverse = TransactionBatch(vec![Transaction::RestoreBlocks {
            index,
            remove_count: 2,
            blocks: vec![original],
        }]);
        Ok((selection, changed_nodes, inverse))
    }

    fn apply_merge_blocks(
        &mut self,
        left_id: NodeId,
        right_id: NodeId,
    ) -> Result<(Selection, SmallVec<[NodeId; 4]>, TransactionBatch), DocumentError> {
        let left_index = self.node_index(left_id)?;
        let right_index = self.node_index(right_id)?;
        if right_index != left_index.saturating_add(1) {
            return Err(DocumentError::InvalidOperation(
                "MergeBlocks requires adjacent blocks in document order".into(),
            ));
        }
        let left_original = self.blocks[left_index].clone();
        let right_original = self.blocks[right_index].clone();
        if left_original.kind != right_original.kind
            || left_original.alignment != right_original.alignment
        {
            return Err(DocumentError::InvalidOperation(
                "MergeBlocks requires matching text block metadata".into(),
            ));
        }
        let (left_text, left_styles) = text_parts(&left_original.content)?;
        let (right_text, right_styles) = text_parts(&right_original.content)?;
        let left_len = left_text.len();
        let mut merged_text = String::with_capacity(left_len + right_text.len());
        merged_text.push_str(left_text);
        merged_text.push_str(right_text);
        let mut merged_styles: SmallVec<[StyledRun; 4]> = left_styles.iter().cloned().collect();
        merged_styles.extend(clip_styles(
            right_styles,
            0,
            right_text.len(),
            left_len as isize,
        ));
        let mut merged_text = String::with_capacity(left_len + right_text.len());
        merged_text.push_str(left_text);
        merged_text.push_str(right_text);
        normalize_styles_for_text(&merged_text, &mut merged_styles);
        self.blocks[left_index].content = BlockContent::Text {
            text: merged_text,
            styles: merged_styles,
        };
        self.blocks.remove(right_index);
        let mut changed_nodes = SmallVec::new();
        push_unique(&mut changed_nodes, left_id);
        push_unique(&mut changed_nodes, right_id);
        let merged_text = self.blocks[left_index]
            .content
            .as_text()
            .expect("merged blocks retain text content");
        let selection = Selection::caret(DocPoint::with_affinity(
            left_id,
            resolve_grapheme_offset(merged_text, left_len, Affinity::After),
            Affinity::After,
        ));
        let inverse = TransactionBatch(vec![Transaction::RestoreBlocks {
            index: left_index,
            remove_count: 1,
            blocks: vec![left_original, right_original],
        }]);
        Ok((selection, changed_nodes, inverse))
    }

    fn apply_set_block_kind(
        &mut self,
        selection: Selection,
        kind: BlockKind,
    ) -> Result<(Selection, SmallVec<[NodeId; 4]>, TransactionBatch), DocumentError> {
        self.validate_selection(selection)?;
        validate_kind(&kind)?;
        if !is_text_kind(&kind) {
            return Err(DocumentError::InvalidOperation(
                "SetBlockKind cannot synthesize structural media content".into(),
            ));
        }
        let (start_index, _, end_index, _) = self.selection_bounds(selection)?;
        self.ensure_text_blocks(start_index, end_index)?;
        let originals = self.blocks[start_index..=end_index].to_vec();
        let mut changed_nodes = SmallVec::new();
        for block in &mut self.blocks[start_index..=end_index] {
            if block.kind != kind {
                block.kind = kind.clone();
                push_unique(&mut changed_nodes, block.id);
            }
        }
        if changed_nodes.is_empty() {
            return Ok((selection, changed_nodes, TransactionBatch::default()));
        }
        let inverse = TransactionBatch(vec![Transaction::RestoreBlocks {
            index: start_index,
            remove_count: end_index - start_index + 1,
            blocks: originals,
        }]);
        Ok((selection, changed_nodes, inverse))
    }

    fn apply_toggle_mark(
        &mut self,
        selection: Selection,
        mark: Mark,
    ) -> Result<(Selection, SmallVec<[NodeId; 4]>, TransactionBatch), DocumentError> {
        self.validate_selection(selection)?;
        let (start_index, start_offset, end_index, end_offset) =
            self.selection_bounds(selection)?;
        self.ensure_text_blocks(start_index, end_index)?;
        if start_index == end_index && start_offset == end_offset {
            return Ok((selection, SmallVec::new(), TransactionBatch::default()));
        }
        let originals = self.blocks[start_index..=end_index].to_vec();
        let mut changed_nodes = SmallVec::new();
        for index in start_index..=end_index {
            let Some((range_start, range_end)) =
                self.text_range_for_block(index, start_index, start_offset, end_index, end_offset)
            else {
                continue;
            };
            if range_start == range_end {
                continue;
            }
            let block = &mut self.blocks[index];
            let (text, styles) = text_parts(&block.content)?;
            let updated = toggle_style_range(text, styles, range_start, range_end, &mark);
            if updated.as_slice() != styles {
                block.content = BlockContent::Text {
                    text: text.to_owned(),
                    styles: updated,
                };
                push_unique(&mut changed_nodes, block.id);
            }
        }
        if changed_nodes.is_empty() {
            return Ok((selection, changed_nodes, TransactionBatch::default()));
        }
        let inverse = TransactionBatch(vec![Transaction::RestoreBlocks {
            index: start_index,
            remove_count: end_index - start_index + 1,
            blocks: originals,
        }]);
        Ok((selection, changed_nodes, inverse))
    }

    fn apply_set_link(
        &mut self,
        selection: Selection,
        url: Option<String>,
    ) -> Result<(Selection, SmallVec<[NodeId; 4]>, TransactionBatch), DocumentError> {
        self.validate_selection(selection)?;
        let (start_index, start_offset, end_index, end_offset) =
            self.selection_bounds(selection)?;
        self.ensure_text_blocks(start_index, end_index)?;
        if start_index == end_index && start_offset == end_offset {
            return Ok((selection, SmallVec::new(), TransactionBatch::default()));
        }
        let originals = self.blocks[start_index..=end_index].to_vec();
        let mut changed_nodes = SmallVec::new();
        for index in start_index..=end_index {
            let Some((range_start, range_end)) =
                self.text_range_for_block(index, start_index, start_offset, end_index, end_offset)
            else {
                continue;
            };
            if range_start == range_end {
                continue;
            }
            let block = &mut self.blocks[index];
            let (text, styles) = text_parts(&block.content)?;
            let updated = set_link_range(text, styles, range_start, range_end, url.as_deref());
            if updated.as_slice() != styles {
                block.content = BlockContent::Text {
                    text: text.to_owned(),
                    styles: updated,
                };
                push_unique(&mut changed_nodes, block.id);
            }
        }
        if changed_nodes.is_empty() {
            return Ok((selection, changed_nodes, TransactionBatch::default()));
        }
        let inverse = TransactionBatch(vec![Transaction::RestoreBlocks {
            index: start_index,
            remove_count: end_index - start_index + 1,
            blocks: originals,
        }]);
        Ok((selection, changed_nodes, inverse))
    }

    fn apply_set_alignment(
        &mut self,
        selection: Selection,
        alignment: TextAlignment,
    ) -> Result<(Selection, SmallVec<[NodeId; 4]>, TransactionBatch), DocumentError> {
        self.validate_selection(selection)?;
        let (start_index, _, end_index, _) = self.selection_bounds(selection)?;
        let originals = self.blocks[start_index..=end_index].to_vec();
        let mut changed_nodes = SmallVec::new();
        for block in &mut self.blocks[start_index..=end_index] {
            if block.alignment != alignment {
                block.alignment = alignment;
                push_unique(&mut changed_nodes, block.id);
            }
        }
        if changed_nodes.is_empty() {
            return Ok((selection, changed_nodes, TransactionBatch::default()));
        }
        let inverse = TransactionBatch(vec![Transaction::RestoreBlocks {
            index: start_index,
            remove_count: end_index - start_index + 1,
            blocks: originals,
        }]);
        Ok((selection, changed_nodes, inverse))
    }

    fn apply_indent_list(
        &mut self,
        selection: Selection,
    ) -> Result<(Selection, SmallVec<[NodeId; 4]>, TransactionBatch), DocumentError> {
        self.apply_list_depth(selection, true)
    }

    fn apply_outdent_list(
        &mut self,
        selection: Selection,
    ) -> Result<(Selection, SmallVec<[NodeId; 4]>, TransactionBatch), DocumentError> {
        self.apply_list_depth(selection, false)
    }

    fn apply_list_depth(
        &mut self,
        selection: Selection,
        indent: bool,
    ) -> Result<(Selection, SmallVec<[NodeId; 4]>, TransactionBatch), DocumentError> {
        self.validate_selection(selection)?;
        let (start_index, _, end_index, _) = self.selection_bounds(selection)?;
        if indent {
            for block in &self.blocks[start_index..=end_index] {
                match block.kind {
                    BlockKind::BulletItem { depth }
                    | BlockKind::OrderedItem { depth }
                    | BlockKind::CheckItem { depth, .. }
                        if depth >= MAX_LIST_DEPTH =>
                    {
                        return Err(DocumentError::InvalidListDepth(depth.saturating_add(1)));
                    }
                    _ => {}
                }
            }
        }
        let originals = self.blocks[start_index..=end_index].to_vec();
        let mut changed_nodes = SmallVec::new();
        for block in &mut self.blocks[start_index..=end_index] {
            let depth = match &mut block.kind {
                BlockKind::BulletItem { depth }
                | BlockKind::OrderedItem { depth }
                | BlockKind::CheckItem { depth, .. } => depth,
                _ => continue,
            };
            if indent {
                *depth = depth.saturating_add(1);
                push_unique(&mut changed_nodes, block.id);
            } else if *depth > 0 {
                *depth -= 1;
                push_unique(&mut changed_nodes, block.id);
            }
        }
        if changed_nodes.is_empty() {
            return Ok((selection, changed_nodes, TransactionBatch::default()));
        }
        let inverse = TransactionBatch(vec![Transaction::RestoreBlocks {
            index: start_index,
            remove_count: end_index - start_index + 1,
            blocks: originals,
        }]);
        Ok((selection, changed_nodes, inverse))
    }

    fn apply_insert_image(
        &mut self,
        selection: Selection,
        resource_id: String,
        natural_size: (u32, u32),
    ) -> Result<(Selection, SmallVec<[NodeId; 4]>, TransactionBatch), DocumentError> {
        if resource_id.is_empty() {
            return Err(DocumentError::InvalidOperation(
                "an image resource id cannot be empty".into(),
            ));
        }
        if natural_size.0 == 0 || natural_size.1 == 0 {
            return Err(DocumentError::InvalidOperation(
                "an image natural size must be non-zero".into(),
            ));
        }
        let (start_index, start_offset, end_index, end_offset) =
            self.selection_bounds(selection)?;
        self.ensure_editable_range(start_index, end_index)?;

        if start_index == end_index && !is_text_block(&self.blocks[start_index]) {
            let original = self.blocks[start_index].clone();
            if selection.is_caret() {
                let image = Block {
                    id: self.new_node_id(),
                    kind: BlockKind::Image,
                    content: BlockContent::Image {
                        resource_id,
                        natural_size,
                        display_width: None,
                    },
                    alignment: TextAlignment::Left,
                    revision: 0,
                };
                let image_id = image.id;
                let insert_at = match selection.anchor.affinity {
                    Affinity::Before => start_index,
                    Affinity::After => start_index.saturating_add(1),
                };
                self.blocks.insert(insert_at, image);
                let mut changed_nodes = SmallVec::new();
                push_unique(&mut changed_nodes, image_id);
                return Ok((
                    Selection::caret(DocPoint::with_affinity(image_id, 0, Affinity::After)),
                    changed_nodes,
                    TransactionBatch(vec![Transaction::RestoreBlocks {
                        index: insert_at,
                        remove_count: 1,
                        blocks: Vec::new(),
                    }]),
                ));
            }

            let image_id = original.id;
            self.blocks[start_index] = Block {
                id: image_id,
                kind: BlockKind::Image,
                content: BlockContent::Image {
                    resource_id,
                    natural_size,
                    display_width: None,
                },
                alignment: original.alignment,
                revision: original.revision,
            };
            let mut changed_nodes = SmallVec::new();
            push_unique(&mut changed_nodes, image_id);
            return Ok((
                Selection::caret(DocPoint::with_affinity(image_id, 0, Affinity::After)),
                changed_nodes,
                TransactionBatch(vec![Transaction::RestoreBlocks {
                    index: start_index,
                    remove_count: 1,
                    blocks: vec![original],
                }]),
            ));
        }

        let originals = self.blocks[start_index..=end_index].to_vec();
        let original_ids = originals.iter().map(|block| block.id).collect::<Vec<_>>();
        let is_empty = start_index == end_index && start_offset == end_offset;
        let insertion_offset = if !is_empty {
            self.delete_range_mut(start_index, start_offset, end_index, end_offset)?
        } else {
            start_offset
        };

        let original_block = self.blocks[start_index].clone();
        let (text, styles) = text_parts(&original_block.content)?;
        let (left_text, left_styles, right_text, right_styles) =
            split_text(text, styles, insertion_offset);
        let image_id = self.new_node_id();
        let right_id = self.new_node_id();
        let left = Block {
            id: original_block.id,
            kind: original_block.kind.clone(),
            content: BlockContent::Text {
                text: left_text,
                styles: left_styles,
            },
            alignment: original_block.alignment,
            revision: original_block.revision,
        };
        let image = Block {
            id: image_id,
            kind: BlockKind::Image,
            content: BlockContent::Image {
                resource_id,
                natural_size,
                display_width: None,
            },
            alignment: TextAlignment::Left,
            revision: 0,
        };
        let right = Block {
            id: right_id,
            kind: original_block.kind.clone(),
            content: BlockContent::Text {
                text: right_text,
                styles: right_styles,
            },
            alignment: original_block.alignment,
            revision: original_block.revision,
        };
        self.blocks
            .splice(start_index..=start_index, [left, image, right]);
        let mut changed_nodes = SmallVec::new();
        for node_id in original_ids {
            push_unique(&mut changed_nodes, node_id);
        }
        push_unique(&mut changed_nodes, image_id);
        push_unique(&mut changed_nodes, right_id);
        let selection = Selection::caret(DocPoint::with_affinity(right_id, 0, Affinity::Before));
        let inverse = TransactionBatch(vec![Transaction::RestoreBlocks {
            index: start_index,
            remove_count: 3,
            blocks: originals,
        }]);
        Ok((selection, changed_nodes, inverse))
    }

    fn apply_remove_node(
        &mut self,
        node_id: NodeId,
    ) -> Result<(Selection, SmallVec<[NodeId; 4]>, TransactionBatch), DocumentError> {
        if self.blocks.len() <= 1 {
            return Err(DocumentError::CannotRemoveLastNode);
        }
        let index = self.node_index(node_id)?;
        let removed = self.blocks[index].clone();
        self.blocks.remove(index);
        let selection = self.selection_near_index(index);
        let mut changed_nodes = SmallVec::new();
        push_unique(&mut changed_nodes, node_id);
        let inverse = TransactionBatch(vec![Transaction::RestoreBlocks {
            index,
            remove_count: 0,
            blocks: vec![removed],
        }]);
        Ok((selection, changed_nodes, inverse))
    }

    fn apply_set_image_width(
        &mut self,
        node_id: NodeId,
        display_width: Option<u32>,
    ) -> Result<(Selection, SmallVec<[NodeId; 4]>, TransactionBatch), DocumentError> {
        if display_width == Some(0) {
            return Err(DocumentError::InvalidOperation(
                "image display width must be positive".into(),
            ));
        }
        let index = self.node_index(node_id)?;
        let original = self.blocks[index].clone();
        let BlockContent::Image {
            display_width: current,
            ..
        } = &mut self.blocks[index].content
        else {
            return Err(DocumentError::InvalidBlockContent(node_id));
        };
        if *current == display_width {
            return Ok((
                self.selection_near_index(index),
                SmallVec::new(),
                TransactionBatch::default(),
            ));
        }
        *current = display_width;
        let mut changed_nodes = SmallVec::new();
        push_unique(&mut changed_nodes, node_id);
        let inverse = TransactionBatch(vec![Transaction::RestoreBlocks {
            index,
            remove_count: 1,
            blocks: vec![original],
        }]);
        Ok((self.selection_near_index(index), changed_nodes, inverse))
    }

    fn apply_restore_blocks(
        &mut self,
        index: usize,
        remove_count: usize,
        blocks: Vec<Block>,
    ) -> Result<(Selection, SmallVec<[NodeId; 4]>, TransactionBatch), DocumentError> {
        if index > self.blocks.len() || remove_count > self.blocks.len().saturating_sub(index) {
            return Err(DocumentError::InvalidOperation(
                "inverse block range is outside the document".into(),
            ));
        }
        validate_block_slice(&blocks)?;
        let mut outside_ids = std::collections::HashSet::new();
        for (position, block) in self.blocks.iter().enumerate() {
            if position < index || position >= index.saturating_add(remove_count) {
                outside_ids.insert(block.id);
            }
        }
        let mut replacement_ids = std::collections::HashSet::new();
        for block in &blocks {
            if !replacement_ids.insert(block.id) || outside_ids.contains(&block.id) {
                return Err(DocumentError::InvalidOperation(
                    "inverse block range would duplicate a node id".into(),
                ));
            }
        }
        let old_blocks = self.blocks[index..index + remove_count].to_vec();
        self.blocks
            .splice(index..index + remove_count, blocks.clone());
        let mut changed_nodes = SmallVec::new();
        for block in old_blocks.iter().chain(blocks.iter()) {
            push_unique(&mut changed_nodes, block.id);
        }
        let selection = self.selection_near_index(index);
        let inverse = TransactionBatch(vec![Transaction::RestoreBlocks {
            index,
            remove_count: blocks.len(),
            blocks: old_blocks,
        }]);
        Ok((selection, changed_nodes, inverse))
    }

    fn selection_near_index(&self, index: usize) -> Selection {
        let block = self
            .blocks
            .get(index.min(self.blocks.len().saturating_sub(1)))
            .or_else(|| self.blocks.last());
        let Some(block) = block else {
            return Selection::caret(DocPoint::new(NodeId::new_internal(0), 0));
        };
        if let Some(text) = block.content.as_text() {
            Selection::caret(DocPoint::with_affinity(
                block.id,
                text.len(),
                Affinity::After,
            ))
        } else {
            self.blocks
                .iter()
                .find_map(|candidate| {
                    candidate.content.as_text().map(|text| {
                        Selection::caret(DocPoint::with_affinity(
                            candidate.id,
                            text.len(),
                            Affinity::After,
                        ))
                    })
                })
                .unwrap_or_else(|| Selection::caret(DocPoint::new(block.id, 0)))
        }
    }
}

fn is_text_kind(kind: &BlockKind) -> bool {
    matches!(
        kind,
        BlockKind::Paragraph
            | BlockKind::Heading { .. }
            | BlockKind::BulletItem { .. }
            | BlockKind::OrderedItem { .. }
            | BlockKind::CheckItem { .. }
            | BlockKind::Quote
            | BlockKind::Code
    )
}

fn is_text_block(block: &Block) -> bool {
    matches!(block.content, BlockContent::Text { .. }) && is_text_kind(&block.kind)
}

fn is_structural_block(block: &Block) -> bool {
    matches!(
        (&block.kind, &block.content),
        (BlockKind::Image, BlockContent::Image { .. })
            | (BlockKind::Attachment, BlockContent::Attachment { .. })
            | (BlockKind::Divider, BlockContent::Empty)
    )
}

fn validate_kind(kind: &BlockKind) -> Result<(), DocumentError> {
    match kind {
        BlockKind::Heading { level } if !(1..=6).contains(level) => {
            Err(DocumentError::InvalidHeadingLevel(*level))
        }
        BlockKind::BulletItem { depth }
        | BlockKind::OrderedItem { depth }
        | BlockKind::CheckItem { depth, .. }
            if *depth > MAX_LIST_DEPTH =>
        {
            Err(DocumentError::InvalidListDepth(*depth))
        }
        _ => Ok(()),
    }
}

fn validate_block_slice(blocks: &[Block]) -> Result<(), DocumentError> {
    let mut ids = std::collections::HashSet::new();
    for block in blocks {
        if block.id.raw() == 0 || !ids.insert(block.id) {
            return Err(DocumentError::InvalidOperation(
                "block range contains a duplicate or zero node id".into(),
            ));
        }
        validate_kind(&block.kind)?;
        match (&block.kind, &block.content) {
            (BlockKind::Image, BlockContent::Image { .. })
            | (BlockKind::Attachment, BlockContent::Attachment { .. })
            | (BlockKind::Divider, BlockContent::Empty) => {}
            (kind, BlockContent::Text { text, styles }) if is_text_kind(kind) => {
                validate_styles(block.id, text, styles)?;
            }
            _ => return Err(DocumentError::InvalidBlockContent(block.id)),
        }
    }
    Ok(())
}

fn validate_styles(node_id: NodeId, text: &str, styles: &[StyledRun]) -> Result<(), DocumentError> {
    let mut previous: Option<&StyledRun> = None;
    for run in styles {
        if run.range.start >= run.range.end || run.range.end > text.len() {
            return Err(DocumentError::InvalidOperation(format!(
                "styled run {:?} is outside node {}",
                run.range,
                node_id.raw()
            )));
        }
        if !text.is_char_boundary(run.range.start) || !text.is_char_boundary(run.range.end) {
            return Err(DocumentError::InvalidUtf8Offset {
                node_id,
                offset: run.range.start,
            });
        }
        if !is_grapheme_boundary(text, run.range.start)
            || !is_grapheme_boundary(text, run.range.end)
        {
            return Err(DocumentError::InvalidGraphemeOffset {
                node_id,
                offset: run.range.start,
            });
        }
        if run.marks.is_empty() || run.marks.windows(2).any(|window| window[0] >= window[1]) {
            return Err(DocumentError::InvalidOperation(
                "styled run marks are not sorted and deduplicated".into(),
            ));
        }
        if let Some(previous) = previous {
            if previous.range.end > run.range.start {
                return Err(DocumentError::InvalidOperation(
                    "styled runs overlap".into(),
                ));
            }
            if previous.range.end == run.range.start && previous.marks == run.marks {
                return Err(DocumentError::InvalidOperation(
                    "adjacent styled runs with equal marks must be merged".into(),
                ));
            }
        }
        previous = Some(run);
    }
    Ok(())
}

fn text_parts(content: &BlockContent) -> Result<(&str, &[StyledRun]), DocumentError> {
    match content {
        BlockContent::Text { text, styles } => Ok((text, styles)),
        _ => Err(DocumentError::InvalidOperation(
            "transaction requires text block content".into(),
        )),
    }
}

fn is_grapheme_boundary(text: &str, offset: usize) -> bool {
    if offset == 0 || offset == text.len() {
        return true;
    }
    text.grapheme_indices(true)
        .any(|(start, _)| start == offset)
}

fn affinity_order(affinity: Affinity) -> u8 {
    match affinity {
        Affinity::Before => 0,
        Affinity::After => 1,
    }
}

fn resolve_grapheme_offset(text: &str, preferred: usize, affinity: Affinity) -> usize {
    let preferred = preferred.min(text.len());
    if is_grapheme_boundary(text, preferred) {
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

/// Snap style boundaries out of a newly joined grapheme and rebuild the
/// non-overlapping run list by taking the union of marks per grapheme-safe
/// segment.  Joining text can make an old byte boundary cease to be a
/// grapheme boundary; a run must expand to cover that complete grapheme.
fn normalize_styles_for_text(text: &str, styles: &mut SmallVec<[StyledRun; 4]>) {
    if styles.is_empty() {
        return;
    }
    let mut boundaries = vec![0, text.len()];
    for run in styles.iter() {
        boundaries.push(resolve_grapheme_offset(
            text,
            run.range.start,
            Affinity::Before,
        ));
        boundaries.push(resolve_grapheme_offset(
            text,
            run.range.end,
            Affinity::After,
        ));
    }
    boundaries.sort_unstable();
    boundaries.dedup();
    let source = styles.clone();
    let mut normalized = SmallVec::new();
    for window in boundaries.windows(2) {
        let (start, end) = (window[0], window[1]);
        if start >= end {
            continue;
        }
        let mut marks: SmallVec<[Mark; 4]> = SmallVec::new();
        for run in &source {
            let run_start = resolve_grapheme_offset(text, run.range.start, Affinity::Before);
            let run_end = resolve_grapheme_offset(text, run.range.end, Affinity::After);
            if run_start < end && run_end > start {
                marks.extend(run.marks.iter().cloned());
            }
        }
        normalize_marks(&mut marks);
        if !marks.is_empty() {
            normalized.push(StyledRun {
                range: start..end,
                marks,
            });
        }
    }
    normalize_styles(&mut normalized);
    *styles = normalized;
}

fn split_text(
    text: &str,
    styles: &[StyledRun],
    at: usize,
) -> (
    String,
    SmallVec<[StyledRun; 4]>,
    String,
    SmallVec<[StyledRun; 4]>,
) {
    let left_styles = clip_styles(styles, 0, at, 0);
    let right_styles = clip_styles(styles, at, text.len(), -(at as isize));
    (
        text[..at].to_owned(),
        left_styles,
        text[at..].to_owned(),
        right_styles,
    )
}

fn clip_styles(
    styles: &[StyledRun],
    start: usize,
    end: usize,
    shift: isize,
) -> SmallVec<[StyledRun; 4]> {
    let mut clipped = SmallVec::new();
    for run in styles {
        let clipped_start = run.range.start.max(start);
        let clipped_end = run.range.end.min(end);
        if clipped_start >= clipped_end {
            continue;
        }
        let shifted_start = (clipped_start as isize + shift).max(0) as usize;
        let shifted_end = (clipped_end as isize + shift).max(0) as usize;
        if shifted_start < shifted_end {
            clipped.push(StyledRun {
                range: shifted_start..shifted_end,
                marks: run.marks.clone(),
            });
        }
    }
    normalize_styles(&mut clipped);
    clipped
}

fn insert_styles(
    styles: &[StyledRun],
    offset: usize,
    inserted_len: usize,
) -> SmallVec<[StyledRun; 4]> {
    if inserted_len == 0 {
        return styles.iter().cloned().collect();
    }
    let mut shifted = SmallVec::new();
    for run in styles {
        let mut range = run.range.clone();
        if range.start >= offset {
            range.start = range.start.saturating_add(inserted_len);
            range.end = range.end.saturating_add(inserted_len);
        } else if range.end > offset {
            range.end = range.end.saturating_add(inserted_len);
        }
        shifted.push(StyledRun {
            range,
            marks: run.marks.clone(),
        });
    }
    normalize_styles(&mut shifted);
    shifted
}

fn delete_text(
    text: &str,
    styles: &[StyledRun],
    start: usize,
    end: usize,
) -> (String, SmallVec<[StyledRun; 4]>) {
    let mut new_text = String::with_capacity(text.len().saturating_sub(end - start));
    new_text.push_str(&text[..start]);
    new_text.push_str(&text[end..]);
    let deleted_len = end - start;
    let mut new_styles = SmallVec::new();
    for run in styles {
        let new_start = map_after_delete(run.range.start, start, end, deleted_len);
        let new_end = map_after_delete(run.range.end, start, end, deleted_len);
        if new_start < new_end {
            new_styles.push(StyledRun {
                range: new_start..new_end,
                marks: run.marks.clone(),
            });
        }
    }
    normalize_styles_for_text(&new_text, &mut new_styles);
    (new_text, new_styles)
}

fn map_after_delete(position: usize, start: usize, end: usize, deleted_len: usize) -> usize {
    if position <= start {
        position
    } else if position >= end {
        position.saturating_sub(deleted_len)
    } else {
        start
    }
}

fn toggle_style_range(
    text: &str,
    styles: &[StyledRun],
    start: usize,
    end: usize,
    mark: &Mark,
) -> SmallVec<[StyledRun; 4]> {
    let mut boundaries = vec![start, end];
    for run in styles {
        if run.range.end > start && run.range.start < end {
            boundaries.push(run.range.start.max(start));
            boundaries.push(run.range.end.min(end));
        }
    }
    boundaries.sort_unstable();
    boundaries.dedup();
    let segments = boundaries
        .windows(2)
        .filter_map(|window| (window[0] < window[1]).then_some((window[0], window[1])))
        .collect::<Vec<_>>();
    let remove = !segments.is_empty()
        && segments.iter().all(|(segment_start, segment_end)| {
            marks_for_segment(styles, *segment_start, *segment_end).contains(mark)
        });
    let mut updated = SmallVec::new();
    for (segment_start, segment_end) in segments {
        let mut marks = marks_for_segment(styles, segment_start, segment_end);
        if remove {
            marks.retain(|candidate| candidate != mark);
        } else if !marks.contains(mark) {
            marks.push(mark.clone());
        }
        normalize_marks(&mut marks);
        if !marks.is_empty() {
            updated.push(StyledRun {
                range: segment_start..segment_end,
                marks,
            });
        }
    }
    append_style_residuals(&mut updated, styles, start, end);
    normalize_styles(&mut updated);
    let _ = text;
    updated
}

fn set_link_range(
    text: &str,
    styles: &[StyledRun],
    start: usize,
    end: usize,
    url: Option<&str>,
) -> SmallVec<[StyledRun; 4]> {
    let mut boundaries = vec![start, end];
    for run in styles {
        if run.range.end > start && run.range.start < end {
            boundaries.push(run.range.start.max(start));
            boundaries.push(run.range.end.min(end));
        }
    }
    boundaries.sort_unstable();
    boundaries.dedup();
    let mut updated = SmallVec::new();
    let segments = boundaries
        .windows(2)
        .filter_map(|window| (window[0] < window[1]).then_some((window[0], window[1])))
        .collect::<Vec<_>>();
    for (segment_start, segment_end) in segments {
        let mut marks = marks_for_segment(styles, segment_start, segment_end);
        marks.retain(|mark| !matches!(mark, Mark::Link(_)));
        if let Some(url) = url {
            marks.push(Mark::Link(url.to_owned()));
        }
        normalize_marks(&mut marks);
        if !marks.is_empty() {
            updated.push(StyledRun {
                range: segment_start..segment_end,
                marks,
            });
        }
    }
    append_style_residuals(&mut updated, styles, start, end);
    normalize_styles(&mut updated);
    let _ = text;
    updated
}

fn append_style_residuals(
    updated: &mut SmallVec<[StyledRun; 4]>,
    styles: &[StyledRun],
    start: usize,
    end: usize,
) {
    for run in styles {
        if run.range.start < start {
            let left_end = run.range.end.min(start);
            if run.range.start < left_end {
                updated.push(StyledRun {
                    range: run.range.start..left_end,
                    marks: run.marks.clone(),
                });
            }
        }
        if run.range.end > end {
            let right_start = run.range.start.max(end);
            if right_start < run.range.end {
                updated.push(StyledRun {
                    range: right_start..run.range.end,
                    marks: run.marks.clone(),
                });
            }
        }
    }
}

fn marks_for_segment(styles: &[StyledRun], start: usize, end: usize) -> SmallVec<[Mark; 4]> {
    styles
        .iter()
        .find(|run| run.range.start <= start && end <= run.range.end)
        .map(|run| run.marks.clone())
        .unwrap_or_default()
}

fn normalize_marks(marks: &mut SmallVec<[Mark; 4]>) {
    marks.sort();
    marks.dedup();
}

fn normalize_styles(styles: &mut SmallVec<[StyledRun; 4]>) {
    for run in styles.iter_mut() {
        normalize_marks(&mut run.marks);
    }
    styles.retain(|run| run.range.start < run.range.end && !run.marks.is_empty());
    styles.sort_by(|left, right| {
        left.range
            .start
            .cmp(&right.range.start)
            .then(left.range.end.cmp(&right.range.end))
    });
    let mut merged: SmallVec<[StyledRun; 4]> = SmallVec::new();
    for run in styles.drain(..) {
        if let Some(previous) = merged.last_mut()
            && previous.range.end == run.range.start
            && previous.marks == run.marks
        {
            previous.range.end = run.range.end;
        } else {
            merged.push(run);
        }
    }
    *styles = merged;
}

fn push_unique(nodes: &mut SmallVec<[NodeId; 4]>, node_id: NodeId) {
    if !nodes.contains(&node_id) {
        nodes.push(node_id);
    }
}
