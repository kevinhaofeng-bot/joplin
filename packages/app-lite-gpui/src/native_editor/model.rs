//! Compact, structured document model for the native editor spike.
//!
//! The model is intentionally independent from the donor Markdown editor.
//! Blocks are kept in a persistent GPUI `SumTree` sequence while positions use
//! stable node identities, allowing the transaction layer to own structural
//! edits and history to store inverse operations.

#[cfg(test)]
use std::cell::Cell;
use std::cmp::Ordering;
use std::collections::BTreeMap;
use std::fmt;
use std::ops::{Index, Range};
use std::sync::Arc;
#[cfg(test)]
use std::sync::atomic::{AtomicUsize, Ordering as AtomicOrdering};

use loro_fractional_index::FractionalIndex;
use smallvec::SmallVec;
use sum_tree::{Bias, ContextLessSummary, Dimension, Item, KeyedItem, SumTree, TreeMap};
use unicode_segmentation::UnicodeSegmentation;

use super::input;
use super::transaction::{
    ApplyOutcome, InsertedTextSpan, StructuralSplice, Transaction, TransactionBatch,
};

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

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord)]
struct BlockCount(usize);

#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct BlockSummary {
    count: usize,
    utf8_len: usize,
    utf16_len: usize,
    last_key: Option<BlockKey>,
}

impl ContextLessSummary for BlockSummary {
    fn zero() -> Self {
        Self::default()
    }

    fn add_summary(&mut self, summary: &Self) {
        let had_items = self.count > 0;
        self.count = self.count.saturating_add(summary.count);
        if had_items && summary.count > 0 {
            self.utf8_len = self.utf8_len.saturating_add(1);
            self.utf16_len = self.utf16_len.saturating_add(1);
        }
        self.utf8_len = self.utf8_len.saturating_add(summary.utf8_len);
        self.utf16_len = self.utf16_len.saturating_add(summary.utf16_len);
        if summary.count > 0 {
            self.last_key = summary.last_key.clone();
        }
    }
}

impl<'a> Dimension<'a, BlockSummary> for BlockCount {
    fn zero(_: ()) -> Self {
        Self(0)
    }

    fn add_summary(&mut self, summary: &'a BlockSummary, _: ()) {
        self.0 = self.0.saturating_add(summary.count);
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct BlockPosition {
    count: usize,
    utf8: usize,
    utf16: usize,
    key: Option<BlockKey>,
}

impl<'a> Dimension<'a, BlockSummary> for BlockPosition {
    fn zero(_: ()) -> Self {
        Self::default()
    }

    fn add_summary(&mut self, summary: &'a BlockSummary, _: ()) {
        if self.count > 0 && summary.count > 0 {
            self.utf8 = self.utf8.saturating_add(1);
            self.utf16 = self.utf16.saturating_add(1);
        }
        self.count = self.count.saturating_add(summary.count);
        self.utf8 = self.utf8.saturating_add(summary.utf8_len);
        self.utf16 = self.utf16.saturating_add(summary.utf16_len);
        if summary.count > 0 {
            self.key = summary.last_key.clone();
        }
    }
}

struct BlockKeyTarget(BlockKey);

impl<'a> sum_tree::SeekTarget<'a, BlockSummary, BlockPosition> for BlockKeyTarget {
    fn cmp(&self, cursor_location: &BlockPosition, _: ()) -> Ordering {
        cursor_location
            .key
            .as_ref()
            .map_or(Ordering::Greater, |key| std::cmp::Ord::cmp(&self.0, key))
    }
}

struct FlatUtf8Target(usize);

impl<'a> sum_tree::SeekTarget<'a, BlockSummary, BlockPosition> for FlatUtf8Target {
    fn cmp(&self, cursor_location: &BlockPosition, _: ()) -> Ordering {
        std::cmp::Ord::cmp(&self.0, &cursor_location.utf8)
    }
}

struct FlatUtf16Target(usize);

impl<'a> sum_tree::SeekTarget<'a, BlockSummary, BlockPosition> for FlatUtf16Target {
    fn cmp(&self, cursor_location: &BlockPosition, _: ()) -> Ordering {
        std::cmp::Ord::cmp(&self.0, &cursor_location.utf16)
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq, PartialOrd, Ord)]
struct BlockKey(FractionalIndex);

impl<'a> Dimension<'a, BlockSummary> for BlockKey {
    fn zero(_: ()) -> Self {
        Self::default()
    }

    fn add_summary(&mut self, summary: &'a BlockSummary, _: ()) {
        if let Some(key) = &summary.last_key {
            *self = key.clone();
        }
    }
}

/// A cheap-clone item stored in the document-order sequence.
///
/// GPUI's `SumTree` copies items while constructing persistent prefix/suffix
/// trees. Keeping the payload behind `Arc` makes those copies retain only one
/// block reference; a mutation clones just the affected block payload.
#[derive(Clone, Debug, PartialEq, Eq)]
struct BlockItem {
    block: Arc<Block>,
    key: BlockKey,
}

impl BlockItem {
    fn new(block: Block, key: BlockKey) -> Self {
        Self {
            block: Arc::new(block),
            key,
        }
    }
}

impl Item for BlockItem {
    type Summary = BlockSummary;

    fn summary(&self, _: ()) -> Self::Summary {
        let (utf8_len, utf16_len) = block_flat_lengths(&self.block);
        BlockSummary {
            count: 1,
            utf8_len,
            utf16_len,
            last_key: Some(self.key.clone()),
        }
    }
}

impl KeyedItem for BlockItem {
    type Key = BlockKey;

    fn key(&self) -> Self::Key {
        self.key.clone()
    }
}

/// Compact document-order read surface backed directly by GPUI's persistent
/// B+ tree. Range collection is intentionally explicit and local; callers do
/// not receive a hidden full-document materialization.
#[derive(Clone, Debug)]
pub struct BlockSequence {
    tree: SumTree<BlockItem>,
    node_keys: TreeMap<NodeId, FractionalIndex>,
    /// Keys ordered by document position for navigation that skips arbitrary
    /// structural atoms. The node-id map above remains the identity index;
    /// these maps deliberately use the existing fractional key as their
    /// order dimension instead of rebuilding a positional shadow vector.
    editable_keys: TreeMap<FractionalIndex, NodeId>,
    text_keys: TreeMap<FractionalIndex, NodeId>,
}

impl PartialEq for BlockSequence {
    fn eq(&self, other: &Self) -> bool {
        self.iter().eq(other.iter())
    }
}

impl Eq for BlockSequence {}

pub struct BlockSequenceIter<'a> {
    inner: sum_tree::Iter<'a, BlockItem>,
}

impl<'a> Iterator for BlockSequenceIter<'a> {
    type Item = &'a Block;

    fn next(&mut self) -> Option<Self::Item> {
        #[cfg(test)]
        BLOCK_SEQUENCE_VISITS.with(|visits| visits.set(visits.get().saturating_add(1)));
        self.inner.next().map(|item| item.block.as_ref())
    }
}

/// Borrowing iterator over one indexed range. The cursor seeks to the range
/// boundary in the SumTree; callers can fold command/layout state without
/// cloning the selected blocks.
pub struct BlockSequenceRangeIter<'a, 'cx> {
    cursor: sum_tree::Cursor<'a, 'cx, BlockItem, BlockCount>,
    end: usize,
}

impl<'a, 'cx> Iterator for BlockSequenceRangeIter<'a, 'cx> {
    type Item = &'a Block;

    fn next(&mut self) -> Option<Self::Item> {
        if self.cursor.start().0 >= self.end {
            return None;
        }
        #[cfg(test)]
        BLOCK_SEQUENCE_VISITS.with(|visits| visits.set(visits.get().saturating_add(1)));
        let item = self.cursor.item().map(|item| item.block.as_ref());
        self.cursor.next();
        item
    }
}

impl<'a> IntoIterator for &'a BlockSequence {
    type Item = &'a Block;
    type IntoIter = BlockSequenceIter<'a>;

    fn into_iter(self) -> Self::IntoIter {
        self.iter()
    }
}

impl BlockSequence {
    fn from_blocks(blocks: Vec<Block>) -> Self {
        let keys = FractionalIndex::generate_n_evenly(None, None, blocks.len())
            .expect("initial document keys have unbounded space");
        let mut items = Vec::with_capacity(blocks.len());
        let mut node_entries = Vec::with_capacity(blocks.len());
        let mut editable_entries = Vec::new();
        let mut text_entries = Vec::new();
        for (block, key) in blocks.into_iter().zip(keys) {
            node_entries.push((block.id, key.clone()));
            if is_navigation_block(&block) {
                editable_entries.push((key.clone(), block.id));
            }
            if block.content.as_text().is_some() {
                text_entries.push((key.clone(), block.id));
            }
            items.push(BlockItem::new(block, BlockKey(key)));
        }
        // Document order is not a valid ordering for this identity map: a
        // restored document may legally contain non-monotonic NodeIds. Sort
        // the entries by the map key before using GPUI's ordered bulk
        // constructor, preserving lookup semantics without persistent
        // one-entry-at-a-time construction.
        node_entries.sort_unstable_by_key(|(node_id, _)| *node_id);
        let node_keys = TreeMap::from_ordered_entries(node_entries);
        let pure_text =
            items.len() == editable_entries.len() && editable_entries.len() == text_entries.len();
        let editable_keys = TreeMap::from_ordered_entries(editable_entries);
        let text_keys = if pure_text {
            editable_keys.clone()
        } else {
            TreeMap::from_ordered_entries(text_entries)
        };
        Self {
            tree: SumTree::from_iter(items, ()),
            node_keys,
            editable_keys,
            text_keys,
        }
    }

    pub fn len(&self) -> usize {
        self.tree.summary().count
    }

    pub fn is_empty(&self) -> bool {
        self.tree.is_empty()
    }

    pub fn iter(&self) -> BlockSequenceIter<'_> {
        BlockSequenceIter {
            inner: self.tree.iter(),
        }
    }

    pub fn iter_range(&self, range: Range<usize>) -> BlockSequenceRangeIter<'_, '_> {
        assert!(range.start <= range.end && range.end <= self.len());
        let mut cursor = self.tree.cursor::<BlockCount>(());
        cursor.seek(&BlockCount(range.start), Bias::Right);
        BlockSequenceRangeIter {
            cursor,
            end: range.end,
        }
    }

    pub fn get(&self, index: usize) -> Option<&Block> {
        self.item_at(index).map(|item| item.block.as_ref())
    }

    pub fn first(&self) -> Option<&Block> {
        self.tree.first().map(|item| item.block.as_ref())
    }

    pub fn last(&self) -> Option<&Block> {
        self.tree.last().map(|item| item.block.as_ref())
    }

    fn item_at(&self, index: usize) -> Option<&BlockItem> {
        let (_, _, item) = self
            .tree
            .find::<BlockCount, _>((), &BlockCount(index), Bias::Right);
        item
    }

    fn key_at(&self, index: usize) -> Option<BlockKey> {
        self.item_at(index).map(|item| item.key.clone())
    }

    fn key_for_node(&self, node_id: NodeId) -> Option<FractionalIndex> {
        self.node_keys.get(&node_id).cloned()
    }

    fn first_text(&self) -> Option<&Block> {
        self.text_keys
            .first()
            .and_then(|(_, node_id)| self.block_by_node_id(*node_id))
    }

    fn last_text(&self) -> Option<&Block> {
        self.text_keys
            .last()
            .and_then(|(_, node_id)| self.block_by_node_id(*node_id))
    }

    fn previous_navigation_block(&self, node_id: NodeId) -> Option<&Block> {
        let index = self.index_of_node(node_id)?;
        let key = self.key_at(index.checked_sub(1)?)?;
        self.editable_keys
            .closest(&key.0)
            .and_then(|(_, previous_id)| self.block_by_node_id(*previous_id))
    }

    fn next_navigation_block(&self, node_id: NodeId) -> Option<&Block> {
        let key = self.key_for_node(node_id)?;
        self.editable_keys
            .iter_from(&key)
            .find_map(|(candidate, candidate_id)| {
                (candidate > &key).then(|| self.block_by_node_id(*candidate_id))
            })
            .flatten()
    }

    fn previous_text_block(&self, node_id: NodeId) -> Option<&Block> {
        let index = self.index_of_node(node_id)?;
        let key = self.key_at(index.checked_sub(1)?)?;
        self.text_keys
            .closest(&key.0)
            .and_then(|(_, previous_id)| self.block_by_node_id(*previous_id))
    }

    fn next_text_block(&self, node_id: NodeId) -> Option<&Block> {
        let key = self.key_for_node(node_id)?;
        self.text_keys
            .iter_from(&key)
            .find_map(|(candidate, candidate_id)| {
                (candidate > &key).then(|| self.block_by_node_id(*candidate_id))
            })
            .flatten()
    }

    fn block_by_node_id(&self, node_id: NodeId) -> Option<&Block> {
        let key = self.node_keys.get(&node_id)?;
        let key = BlockKey(key.clone());
        let (_, _, item) = self
            .tree
            .find::<BlockPosition, _>((), &BlockKeyTarget(key), Bias::Left);
        item.filter(|item| item.block.id == node_id)
            .map(|item| item.block.as_ref())
    }

    fn contains_node(&self, node_id: NodeId) -> bool {
        self.node_keys.get(&node_id).is_some()
    }

    fn index_of_node(&self, node_id: NodeId) -> Option<usize> {
        let key = self.node_keys.get(&node_id)?;
        let key = BlockKey(key.clone());
        let (position, _, item) =
            self.tree
                .find::<BlockPosition, _>((), &BlockKeyTarget(key), Bias::Left);
        item.filter(|item| item.block.id == node_id)
            .map(|_| position.count)
    }

    fn position_of_node(&self, node_id: NodeId) -> Option<(BlockPosition, &Block)> {
        let key = self.node_keys.get(&node_id)?;
        let key = BlockKey(key.clone());
        let (position, _, item) =
            self.tree
                .find::<BlockPosition, _>((), &BlockKeyTarget(key), Bias::Left);
        item.filter(|item| item.block.id == node_id)
            .map(|item| (position, item.block.as_ref()))
    }

    fn position_for_utf8(&self, offset: usize) -> Option<(BlockPosition, &Block)> {
        let total = self.tree.summary().utf8_len;
        if self.is_empty() || offset >= total {
            return None;
        }
        let (position, _, item) =
            self.tree
                .find::<BlockPosition, _>((), &FlatUtf8Target(offset), Bias::Left);
        item.map(|item| (position, item.block.as_ref()))
    }

    fn position_for_utf16(&self, offset: usize) -> Option<(BlockPosition, &Block)> {
        let total = self.tree.summary().utf16_len;
        if self.is_empty() || offset >= total {
            return None;
        }
        let (position, _, item) =
            self.tree
                .find::<BlockPosition, _>((), &FlatUtf16Target(offset), Bias::Left);
        item.map(|item| (position, item.block.as_ref()))
    }

    fn flat_utf8_len(&self) -> usize {
        self.tree.summary().utf8_len
    }

    fn flat_utf16_len(&self) -> usize {
        self.tree.summary().utf16_len
    }

    fn position_start_utf8(position: &BlockPosition) -> usize {
        position
            .utf8
            .saturating_add(usize::from(position.count > 0))
    }

    fn position_start_utf16(position: &BlockPosition) -> usize {
        position
            .utf16
            .saturating_add(usize::from(position.count > 0))
    }

    fn flat_utf8_offset_for_node(
        &self,
        node_id: NodeId,
        local_offset: usize,
        affinity: Affinity,
    ) -> Option<usize> {
        let (position, block) = self.position_of_node(node_id)?;
        let local_offset = match &block.content {
            BlockContent::Text { text, .. } => local_offset.min(text.len()),
            _ => usize::from(affinity == Affinity::After) * block_flat_lengths(block).0,
        };
        Some(Self::position_start_utf8(&position).saturating_add(local_offset))
    }

    fn flat_utf8_point(&self, offset: usize, affinity: Affinity) -> Option<DocPoint> {
        let total = self.flat_utf8_len();
        if self.is_empty() {
            return None;
        }
        if offset == 0 {
            return self
                .first()
                .map(|block| DocPoint::with_affinity(block.id, 0, Affinity::Before));
        }
        if offset >= total {
            return self.last().map(|block| match &block.content {
                BlockContent::Text { text, .. } => {
                    DocPoint::with_affinity(block.id, text.len(), Affinity::After)
                }
                _ => DocPoint::with_affinity(block.id, 0, Affinity::After),
            });
        }
        let (position, block) = self.position_for_utf8(offset)?;
        let local = offset
            .saturating_sub(Self::position_start_utf8(&position))
            .min(block_flat_lengths(block).0);
        Some(match &block.content {
            BlockContent::Text { text, .. } => DocPoint::with_affinity(
                block.id,
                resolve_grapheme_offset(text, local, affinity),
                affinity,
            ),
            _ => DocPoint::with_affinity(
                block.id,
                0,
                if local < block_flat_lengths(block).0 / 2 {
                    Affinity::Before
                } else {
                    Affinity::After
                },
            ),
        })
    }

    fn flat_utf16_offset(&self, offset: usize) -> usize {
        let total = self.flat_utf16_len();
        if self.is_empty() {
            return 0;
        }
        if offset >= total {
            return self.flat_utf8_len();
        }
        let Some((position, block)) = self.position_for_utf16(offset) else {
            return self.flat_utf8_len();
        };
        let local = offset
            .saturating_sub(Self::position_start_utf16(&position))
            .min(block_flat_lengths(block).1);
        let local_utf8 = match &block.content {
            BlockContent::Text { text, .. } => input::utf16_to_utf8_in(text, local),
            _ => input::utf16_to_utf8_in("\u{fffc}", local),
        };
        Self::position_start_utf8(&position).saturating_add(local_utf8)
    }

    fn flat_utf8_utf16_offset(&self, offset: usize) -> usize {
        let total = self.flat_utf8_len();
        if self.is_empty() {
            return 0;
        }
        if offset >= total {
            return self.flat_utf16_len();
        }
        let Some((position, block)) = self.position_for_utf8(offset) else {
            return self.flat_utf16_len();
        };
        let local = offset
            .saturating_sub(Self::position_start_utf8(&position))
            .min(block_flat_lengths(block).0);
        let local_utf16 = match &block.content {
            BlockContent::Text { text, .. } => input::utf8_to_utf16_in(text, local),
            _ => input::utf8_to_utf16_in("\u{fffc}", local),
        };
        Self::position_start_utf16(&position).saturating_add(local_utf16)
    }

    fn text_for_utf8_range(&self, range: Range<usize>) -> Option<String> {
        let total = self.flat_utf8_len();
        let start = range.start.min(total);
        let end = range.end.min(total);
        if start > end {
            return None;
        }
        if start == end {
            return Some(String::new());
        }
        let (start_position, start_block) = self.position_for_utf8(start).or_else(|| {
            self.last()
                .and_then(|block| self.position_of_node(block.id))
        })?;
        let (end_position, end_block) = if end < total {
            self.position_for_utf8(end)?
        } else {
            let block = self.last()?;
            self.position_of_node(block.id)?
        };
        let start_index = start_position.count;
        let end_index = end_position.count;
        let mut result = String::new();
        for index in start_index..=end_index {
            let block = self.get(index)?;
            let position = self.position_of_node(block.id)?.0;
            let block_start = Self::position_start_utf8(&position);
            let block_len = block_flat_lengths(block).0;
            let local_start = if index == start_index {
                start.saturating_sub(block_start).min(block_len)
            } else {
                0
            };
            let local_end = if index == end_index {
                end.saturating_sub(block_start).min(block_len)
            } else {
                block_len
            };
            if local_start < local_end {
                match &block.content {
                    BlockContent::Text { text, .. } => {
                        result.push_str(text.get(local_start..local_end)?);
                    }
                    _ => result.push('\u{fffc}'),
                }
            }
            if index < end_index {
                let separator = block_start.saturating_add(block_len);
                if start <= separator && separator < end {
                    result.push('\n');
                }
            }
        }
        // Keep the variables meaningful in debug builds and make the range
        // boundary contract explicit: both endpoints must resolve to the
        // blocks traversed above.
        let _ = (start_block.id, end_block.id);
        Some(result)
    }

    /// Materialize only the requested local range for a transaction inverse
    /// or a neighboring-block calculation.
    pub fn collect_range(&self, range: Range<usize>) -> Vec<Block> {
        self.iter_range(range).cloned().collect()
    }

    /// Replace one local range using GPUI's production cursor splice path:
    /// persistent prefix + local replacement tree + persistent suffix.
    pub fn splice<I>(&mut self, range: Range<usize>, replacement: I) -> Vec<Block>
    where
        I: IntoIterator<Item = Block>,
    {
        assert!(range.start <= range.end && range.end <= self.len());
        let removed = self.collect_range(range.clone());
        let replacement = replacement.into_iter().collect::<Vec<_>>();
        let old_items = (range.start..range.end)
            .map(|index| self.item_at(index).expect("splice range item").key.clone())
            .collect::<Vec<_>>();
        let preserve_keys = replacement.len() == old_items.len()
            && replacement
                .iter()
                .zip(&removed)
                .all(|(replacement, removed)| replacement.id == removed.id);
        let keys = if preserve_keys {
            old_items
        } else {
            let lower = if range.start == 0 {
                None
            } else {
                self.key_at(range.start - 1)
            };
            let upper = self.key_at(range.end);
            FractionalIndex::generate_n_evenly(
                lower.as_ref().map(|key| &key.0),
                upper.as_ref().map(|key| &key.0),
                replacement.len(),
            )
            .expect("fractional index space exhausted between adjacent blocks")
            .into_iter()
            .map(BlockKey)
            .collect()
        };
        let replacement_items = replacement
            .into_iter()
            .zip(keys)
            .map(|(block, key)| BlockItem::new(block, key))
            .collect::<Vec<_>>();
        for block in &removed {
            if let Some(key) = self.key_for_node(block.id) {
                if is_navigation_block(block) {
                    self.editable_keys.remove(&key);
                }
                if block.content.as_text().is_some() {
                    self.text_keys.remove(&key);
                }
            }
            self.node_keys.remove(&block.id);
        }
        for item in &replacement_items {
            self.node_keys.insert(item.block.id, item.key.0.clone());
            if is_navigation_block(item.block.as_ref()) {
                self.editable_keys.insert(item.key.0.clone(), item.block.id);
            }
            if item.block.content.as_text().is_some() {
                self.text_keys.insert(item.key.0.clone(), item.block.id);
            }
        }
        let replacement = SumTree::from_iter(replacement_items, ());
        let mut cursor = self.tree.cursor::<BlockCount>(());
        let mut new_tree = cursor.slice(&BlockCount(range.start), Bias::Right);
        cursor.seek_forward(&BlockCount(range.end), Bias::Right);
        new_tree.append(replacement, ());
        new_tree.append(cursor.suffix(), ());
        drop(cursor);
        self.tree = new_tree;
        removed
    }

    pub fn insert(&mut self, index: usize, block: Block) {
        self.splice(index..index, [block]);
    }

    pub fn remove(&mut self, index: usize) -> Block {
        self.splice(index..index.saturating_add(1), std::iter::empty())
            .into_iter()
            .next()
            .expect("remove index is valid")
    }

    pub fn replace(&mut self, index: usize, block: Block) {
        self.splice(index..index.saturating_add(1), [block]);
    }

    #[cfg(test)]
    pub(crate) fn item_overhead_bytes_for_test() -> usize {
        std::mem::size_of::<BlockItem>()
    }
}

impl Index<usize> for BlockSequence {
    type Output = Block;

    fn index(&self, index: usize) -> &Self::Output {
        self.get(index).expect("block index is in range")
    }
}

/// Compact structured document.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Document {
    blocks: BlockSequence,
    next_id: u64,
    revision: u64,
}

#[derive(Clone, Copy, Debug)]
struct SelectionBounds {
    start_index: usize,
    start_offset: usize,
    start_affinity: Affinity,
    end_index: usize,
    end_offset: usize,
    end_affinity: Affinity,
}

/// Local pre-transaction facts used to publish an explicit structural splice
/// after the mutation has committed. The plan owns only the affected IDs;
/// it never snapshots the document order.
#[derive(Clone, Debug, Default)]
struct StructuralPlan {
    start_index: usize,
    removed: SmallVec<[NodeId; 4]>,
    inserted_count: usize,
}

impl StructuralPlan {
    fn finish(self, blocks: &BlockSequence) -> Option<StructuralSplice> {
        let end = self.start_index.saturating_add(self.inserted_count);
        if end > blocks.len() {
            return None;
        }
        let inserted: SmallVec<[(NodeId, u64); 4]> = blocks
            .iter_range(self.start_index..end)
            .map(|block| (block.id, block.revision))
            .collect();
        // A RestoreBlocks inverse is also used for inline edits and for
        // IME replacement. When it puts the exact same identities back in
        // the exact same order, the document order did not change. Keep the
        // operation on the local changed-node path so layout does not treat a
        // content-only restore as an order splice.
        if self.removed
            == inserted
                .iter()
                .map(|(node_id, _)| *node_id)
                .collect::<SmallVec<[NodeId; 4]>>()
        {
            return None;
        }
        Some(StructuralSplice {
            start_index: self.start_index,
            removed: self.removed,
            inserted: inserted.iter().map(|(node_id, _)| *node_id).collect(),
            inserted_revisions: inserted.iter().map(|(_, revision)| *revision).collect(),
        })
    }
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
            blocks: BlockSequence::from_blocks(blocks),
            next_id,
            revision: 0,
        };
        debug_assert!(document.validate_invariants().is_ok());
        document
    }

    pub fn from_blocks(blocks: Vec<Block>) -> Result<Self, DocumentError> {
        validate_block_slice(&blocks)?;
        let next_id = blocks
            .iter()
            .map(|block| block.id.raw())
            .max()
            .unwrap_or(0)
            .checked_add(1)
            .unwrap_or(u64::MAX);
        let document = Self {
            blocks: BlockSequence::from_blocks(blocks),
            next_id: next_id.max(1),
            revision: 0,
        };
        document.validate_invariants()?;
        Ok(document)
    }

    pub fn blocks(&self) -> &BlockSequence {
        &self.blocks
    }

    #[cfg(test)]
    pub(crate) fn blocks_mut_for_test(&mut self) -> &mut BlockSequence {
        &mut self.blocks
    }

    pub fn block(&self, node_id: NodeId) -> Option<&Block> {
        self.blocks.block_by_node_id(node_id)
    }

    pub fn block_at_index(&self, index: usize) -> Option<&Block> {
        self.blocks.get(index)
    }

    pub fn block_count(&self) -> usize {
        self.blocks.len()
    }

    pub(crate) fn order_key(&self, node_id: NodeId) -> Option<FractionalIndex> {
        self.blocks.key_for_node(node_id)
    }

    pub(crate) fn order_keys(&self) -> TreeMap<NodeId, FractionalIndex> {
        self.blocks.node_keys.clone()
    }

    pub(crate) fn first_text_block(&self) -> Option<&Block> {
        self.blocks.first_text()
    }

    pub(crate) fn last_text_block(&self) -> Option<&Block> {
        self.blocks.last_text()
    }

    pub(crate) fn previous_navigation_block(&self, node_id: NodeId) -> Option<&Block> {
        self.blocks.previous_navigation_block(node_id)
    }

    pub(crate) fn next_navigation_block(&self, node_id: NodeId) -> Option<&Block> {
        self.blocks.next_navigation_block(node_id)
    }

    pub(crate) fn previous_text_block(&self, node_id: NodeId) -> Option<&Block> {
        self.blocks.previous_text_block(node_id)
    }

    pub(crate) fn next_text_block(&self, node_id: NodeId) -> Option<&Block> {
        self.blocks.next_text_block(node_id)
    }

    pub(crate) fn flat_utf8_len(&self) -> usize {
        self.blocks.flat_utf8_len()
    }

    pub(crate) fn flat_utf16_len(&self) -> usize {
        self.blocks.flat_utf16_len()
    }

    pub(crate) fn flat_offset_for_point(&self, point: DocPoint) -> Option<usize> {
        self.blocks
            .flat_utf8_offset_for_node(point.node_id, point.utf8_offset, point.affinity)
    }

    pub(crate) fn point_for_flat_utf8_offset(
        &self,
        offset: usize,
        affinity: Affinity,
    ) -> Option<DocPoint> {
        self.blocks.flat_utf8_point(offset, affinity)
    }

    pub(crate) fn utf8_to_utf16_offset(&self, offset: usize) -> usize {
        self.blocks.flat_utf8_utf16_offset(offset)
    }

    pub(crate) fn utf16_to_utf8_offset(&self, offset: usize) -> usize {
        self.blocks.flat_utf16_offset(offset)
    }

    pub(crate) fn utf8_range_to_utf16(&self, range: Range<usize>) -> Range<usize> {
        self.utf8_to_utf16_offset(range.start)..self.utf8_to_utf16_offset(range.end)
    }

    pub(crate) fn utf16_range_to_utf8(&self, range: Range<usize>) -> Range<usize> {
        self.utf16_to_utf8_offset(range.start)..self.utf16_to_utf8_offset(range.end)
    }

    pub(crate) fn text_for_utf8_range(&self, range: Range<usize>) -> Option<String> {
        self.blocks.text_for_utf8_range(range)
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
        let first = self.first_text_block();
        let last = self.last_text_block();
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
        if let Some(block) = self.last_text_block() {
            return Selection::caret(DocPoint::with_affinity(
                block.id,
                block.content.as_text().map_or(0, str::len),
                Affinity::After,
            ));
        }
        Selection::caret(DocPoint::new(NodeId::new_internal(0), 0))
    }

    pub fn semantic_snapshot(&self) -> SemanticSnapshot {
        let mut blocks: Vec<Block> = self.blocks.iter().cloned().collect();
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
        let mut selection = None;
        let mut estimated_bytes = 0usize;
        let mut inserted_span = None;
        let mut structural = false;
        let mut structural_splices = SmallVec::new();
        let mut numbering_ranges = SmallVec::new();
        let initial_revision = self.revision;
        let initial_next_id = self.next_id;

        for transaction in batch.0 {
            let outcome = match self.apply_transaction(transaction) {
                Ok(outcome) => outcome,
                Err(error) => {
                    self.rollback_journal(inverse_batches, initial_revision, initial_next_id);
                    return Err(error);
                }
            };
            selection = Some(outcome.selection);
            structural |= outcome.structural;
            structural_splices.extend(outcome.structural_splices);
            numbering_ranges.extend(outcome.numbering_ranges);
            // The public outcome describes only the final operation in the
            // batch. A later non-insert operation must clear an earlier span
            // rather than publishing a range that may have moved or vanished.
            inserted_span = outcome.inserted_span;
            for node_id in outcome.changed_nodes {
                push_unique(&mut changed_nodes, node_id);
            }
            estimated_bytes = estimated_bytes.saturating_add(outcome.estimated_bytes);
            inverse_batches.push(outcome.inverse);
        }

        let selection = selection.expect("non-empty batch produced an outcome");
        let rollback_batches = inverse_batches.clone();
        let mut inverse = Vec::new();
        for batch in inverse_batches.into_iter().rev() {
            inverse.extend(batch.0);
        }

        // Every primitive validates the blocks it changes before publishing
        // its outcome, and structural RestoreBlocks validates the replacement
        // range plus any outside-range identity collision. Re-running a
        // document-wide HashSet/grapheme audit here would turn an otherwise
        // local batch into an allocation proportional to every block. The
        // rollback journal still covers the local validation failure path.
        if let Err(error) = self.validate_changed_nodes(&changed_nodes) {
            self.rollback_journal(rollback_batches, initial_revision, initial_next_id);
            return Err(error);
        }
        Ok(ApplyOutcome {
            selection,
            changed_nodes,
            structural,
            structural_splices,
            numbering_ranges,
            inverse: TransactionBatch(inverse),
            estimated_bytes,
            inserted_span,
        })
    }

    /// Replace one already-applied provisional operation without cloning the
    /// document. The inverse is applied and the replacement is then executed
    /// against the restored base document inside one rollback journal. If
    /// either the inverse, the caller's preflight closure, or the replacement
    /// fails, the original document revision, allocation cursor, blocks and
    /// contents are restored exactly.
    pub(crate) fn replace_after_inverse<F>(
        &mut self,
        inverse: TransactionBatch,
        before_selection: Selection,
        make_replacement: F,
    ) -> Result<(ApplyOutcome, Transaction), DocumentError>
    where
        F: FnOnce(&Document) -> Result<Transaction, DocumentError>,
    {
        let initial_revision = self.revision;
        let initial_next_id = self.next_id;
        let mut rollback_journal = Vec::new();
        let mut changed_nodes = SmallVec::new();
        let mut structural = false;
        let mut structural_splices = SmallVec::new();
        let mut numbering_ranges = SmallVec::new();

        for transaction in inverse.0 {
            let outcome = match self.apply_transaction(transaction) {
                Ok(outcome) => outcome,
                Err(error) => {
                    self.rollback_journal(rollback_journal, initial_revision, initial_next_id);
                    return Err(error);
                }
            };
            for node_id in outcome.changed_nodes.iter().copied() {
                push_unique(&mut changed_nodes, node_id);
            }
            structural |= outcome.structural;
            structural_splices.extend(outcome.structural_splices);
            numbering_ranges.extend(outcome.numbering_ranges);
            rollback_journal.push(outcome.inverse);
        }

        if let Err(error) = self.validate_selection(before_selection) {
            self.rollback_journal(rollback_journal, initial_revision, initial_next_id);
            return Err(error);
        }

        let replacement = match make_replacement(self) {
            Ok(replacement) => replacement,
            Err(error) => {
                self.rollback_journal(rollback_journal, initial_revision, initial_next_id);
                return Err(error);
            }
        };
        let replacement_for_return = replacement.clone();
        let outcome = match self.apply_transaction(replacement) {
            Ok(outcome) => outcome,
            Err(error) => {
                self.rollback_journal(rollback_journal, initial_revision, initial_next_id);
                return Err(error);
            }
        };
        for node_id in outcome.changed_nodes.iter().copied() {
            push_unique(&mut changed_nodes, node_id);
        }
        structural |= outcome.structural;
        structural_splices.extend(outcome.structural_splices);
        numbering_ranges.extend(outcome.numbering_ranges);
        Ok((
            ApplyOutcome {
                selection: outcome.selection,
                changed_nodes,
                structural,
                structural_splices,
                numbering_ranges,
                inverse: outcome.inverse,
                estimated_bytes: outcome.estimated_bytes,
                inserted_span: outcome.inserted_span,
            },
            replacement_for_return,
        ))
    }

    fn rollback_journal(
        &mut self,
        inverse_batches: Vec<TransactionBatch>,
        revision: u64,
        next_id: u64,
    ) {
        for inverse in inverse_batches.into_iter().rev() {
            self.restore_inverse_batch(inverse);
        }
        self.revision = revision;
        self.next_id = next_id;
    }

    fn restore_inverse_batch(&mut self, inverse: TransactionBatch) {
        for transaction in inverse.0.into_iter().rev() {
            match transaction {
                Transaction::RestoreBlocks {
                    index,
                    remove_count,
                    blocks,
                } => {
                    debug_assert!(
                        index <= self.blocks.len()
                            && remove_count <= self.blocks.len().saturating_sub(index)
                    );
                    self.blocks
                        .splice(index..index.saturating_add(remove_count), blocks);
                }
                other => {
                    unreachable!("transaction journal contains non-local inverse: {other:?}");
                }
            }
        }
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
            validate_block_invariants(block)?;
        }
        Ok(())
    }

    fn validate_changed_nodes(&self, changed_nodes: &[NodeId]) -> Result<(), DocumentError> {
        for node_id in changed_nodes {
            // A removed block is retained in changed_nodes so layout can
            // evict stale geometry; it has no live content left to validate.
            if let Some(block) = self.blocks.block_by_node_id(*node_id) {
                validate_block_invariants(block)?;
            }
        }
        Ok(())
    }

    pub(crate) fn node_index(&self, node_id: NodeId) -> Result<usize, DocumentError> {
        self.blocks
            .index_of_node(node_id)
            .ok_or(DocumentError::NodeNotFound(node_id))
    }

    fn new_node_id(&mut self) -> Result<NodeId, DocumentError> {
        let raw = self.next_id.max(1);
        if raw == u64::MAX {
            return Err(DocumentError::InvalidOperation(
                "node id allocator exhausted".into(),
            ));
        }
        let id = NodeId::new_internal(raw);
        if self.blocks.contains_node(id) {
            return Err(DocumentError::InvalidOperation(
                "node id allocator collided with a retained node".into(),
            ));
        }
        self.next_id = raw.saturating_add(1);
        Ok(id)
    }

    fn advance_next_id_for_blocks(&mut self, blocks: &[Block]) {
        if let Some(max_id) = blocks.iter().map(|block| block.id.raw()).max() {
            self.next_id = self.next_id.max(max_id.checked_add(1).unwrap_or(u64::MAX));
        }
    }

    fn structural_plan_for(&self, transaction: &Transaction) -> Option<StructuralPlan> {
        let mut plan = StructuralPlan::default();
        match transaction {
            Transaction::SplitBlock { at } => {
                let index = self.node_index(at.node_id).ok()?;
                plan.start_index = index;
                plan.removed.push(at.node_id);
                plan.inserted_count = 2;
                Some(plan)
            }
            Transaction::MergeBlocks { left, right } => {
                let left_index = self.node_index(*left).ok()?;
                let right_index = self.node_index(*right).ok()?;
                if right_index != left_index.saturating_add(1) {
                    return None;
                }
                plan.start_index = left_index;
                plan.removed.extend([*left, *right]);
                plan.inserted_count = 1;
                Some(plan)
            }
            Transaction::InsertText { selection, .. } => {
                if let Some(index) = self.adjacent_structural_seam(*selection).ok()? {
                    plan.start_index = index;
                    plan.inserted_count = 1;
                    return Some(plan);
                }
                let bounds = self.editable_selection_bounds(*selection).ok()?;
                if bounds.start_index == bounds.end_index {
                    if !is_text_block(&self.blocks[bounds.start_index]) && selection.is_caret() {
                        plan.start_index = match selection.anchor.affinity {
                            Affinity::Before => bounds.start_index,
                            Affinity::After => bounds.start_index.saturating_add(1),
                        };
                        plan.inserted_count = 1;
                        return Some(plan);
                    }
                    return None;
                }
                plan.start_index = bounds.start_index;
                plan.removed.extend(
                    self.blocks
                        .iter_range(bounds.start_index..bounds.end_index.saturating_add(1))
                        .map(|block| block.id),
                );
                plan.inserted_count = 1;
                Some(plan)
            }
            Transaction::DeleteRange { selection } => {
                if self.adjacent_structural_seam(*selection).ok()?.is_some() {
                    return None;
                }
                let bounds = self.editable_selection_bounds(*selection).ok()?;
                if bounds.start_index == bounds.end_index {
                    if selection.is_caret() || is_text_block(&self.blocks[bounds.start_index]) {
                        return None;
                    }
                    plan.start_index = bounds.start_index;
                    plan.removed.push(self.blocks[bounds.start_index].id);
                    return Some(plan);
                }
                plan.start_index = bounds.start_index;
                plan.removed.extend(
                    self.blocks
                        .iter_range(bounds.start_index..bounds.end_index.saturating_add(1))
                        .map(|block| block.id),
                );
                plan.inserted_count = 1;
                Some(plan)
            }
            Transaction::InsertImage { selection, .. } => {
                if let Some(index) = self.adjacent_structural_seam(*selection).ok()? {
                    plan.start_index = index;
                    plan.inserted_count = 1;
                    return Some(plan);
                }
                let bounds = self.editable_selection_bounds(*selection).ok()?;
                if bounds.start_index == bounds.end_index
                    && !is_text_block(&self.blocks[bounds.start_index])
                {
                    if !selection.is_caret() {
                        return None;
                    }
                    plan.start_index = match selection.anchor.affinity {
                        Affinity::Before => bounds.start_index,
                        Affinity::After => bounds.start_index.saturating_add(1),
                    };
                    plan.inserted_count = 1;
                    return Some(plan);
                }
                plan.start_index = bounds.start_index;
                plan.removed.extend(
                    self.blocks
                        .iter_range(bounds.start_index..bounds.end_index.saturating_add(1))
                        .map(|block| block.id),
                );
                plan.inserted_count = 3;
                Some(plan)
            }
            Transaction::RemoveNode { node_id } => {
                let index = self.node_index(*node_id).ok()?;
                plan.start_index = index;
                plan.removed.push(*node_id);
                Some(plan)
            }
            Transaction::RestoreBlocks {
                index,
                remove_count,
                blocks,
            } => {
                if *index > self.blocks.len()
                    || *remove_count > self.blocks.len().saturating_sub(*index)
                {
                    return None;
                }
                plan.start_index = *index;
                plan.removed.extend(
                    self.blocks
                        .iter_range(*index..index.saturating_add(*remove_count))
                        .map(|block| block.id),
                );
                plan.inserted_count = blocks.len();
                Some(plan)
            }
            _ => None,
        }
    }

    fn numbering_range_for(&self, transaction: &Transaction) -> Option<Range<usize>> {
        match transaction {
            Transaction::SetBlockKind { selection, .. }
            | Transaction::IndentList { selection }
            | Transaction::OutdentList { selection } => {
                let bounds = self.selection_bounds(*selection).ok()?;
                Some(bounds.0..bounds.2.saturating_add(1))
            }
            Transaction::RestoreBlocks {
                index,
                remove_count,
                blocks,
            } => {
                let end = index.saturating_add(*remove_count);
                if end > self.blocks.len() {
                    return None;
                }
                // Same-ID restores are not order edits, but a kind/depth
                // change still invalidates the local numbering sequence.
                // Compare only the supplied range; this keeps inline undo and
                // IME provisional restore O(changed_nodes).
                if *remove_count != blocks.len() {
                    return None;
                }
                let mut kind_changed = false;
                let mut old_blocks = self.blocks.iter_range(*index..end);
                for replacement in blocks {
                    let old = old_blocks.next()?;
                    if old.id != replacement.id {
                        return None;
                    }
                    kind_changed |= old.kind != replacement.kind;
                }
                kind_changed.then_some(*index..index.saturating_add(blocks.len()))
            }
            _ => None,
        }
    }

    fn apply_transaction(
        &mut self,
        transaction: Transaction,
    ) -> Result<ApplyOutcome, DocumentError> {
        let original_revision = self.revision;
        let original_next_id = self.next_id;
        let block_count_before = self.blocks.len();
        let structural_plan = self.structural_plan_for(&transaction);
        let numbering_range = self.numbering_range_for(&transaction);
        let (selection, changed_nodes, inverse, inserted_span) = match transaction {
            Transaction::InsertText { selection, text } => {
                self.apply_insert_text(selection, text)?
            }
            Transaction::DeleteRange { selection } => {
                let (selection, changed_nodes, inverse) = self.apply_delete_range(selection)?;
                (selection, changed_nodes, inverse, None)
            }
            Transaction::SplitBlock { at } => {
                let (selection, changed_nodes, inverse) = self.apply_split_block(at)?;
                (selection, changed_nodes, inverse, None)
            }
            Transaction::MergeBlocks { left, right } => {
                let (selection, changed_nodes, inverse) = self.apply_merge_blocks(left, right)?;
                (selection, changed_nodes, inverse, None)
            }
            Transaction::SetBlockKind { selection, kind } => {
                let (selection, changed_nodes, inverse) =
                    self.apply_set_block_kind(selection, kind)?;
                (selection, changed_nodes, inverse, None)
            }
            Transaction::ToggleMark { selection, mark } => {
                let (selection, changed_nodes, inverse) =
                    self.apply_toggle_mark(selection, mark)?;
                (selection, changed_nodes, inverse, None)
            }
            Transaction::SetLink { selection, url } => {
                let (selection, changed_nodes, inverse) = self.apply_set_link(selection, url)?;
                (selection, changed_nodes, inverse, None)
            }
            Transaction::SetAlignment {
                selection,
                alignment,
            } => {
                let (selection, changed_nodes, inverse) =
                    self.apply_set_alignment(selection, alignment)?;
                (selection, changed_nodes, inverse, None)
            }
            Transaction::IndentList { selection } => {
                let (selection, changed_nodes, inverse) = self.apply_indent_list(selection)?;
                (selection, changed_nodes, inverse, None)
            }
            Transaction::OutdentList { selection } => {
                let (selection, changed_nodes, inverse) = self.apply_outdent_list(selection)?;
                (selection, changed_nodes, inverse, None)
            }
            Transaction::InsertImage {
                selection,
                resource_id,
                natural_size,
            } => {
                let (selection, changed_nodes, inverse) =
                    self.apply_insert_image(selection, resource_id, natural_size)?;
                (selection, changed_nodes, inverse, None)
            }
            Transaction::RemoveNode { node_id } => {
                let (selection, changed_nodes, inverse) = self.apply_remove_node(node_id)?;
                (selection, changed_nodes, inverse, None)
            }
            Transaction::SetImageDisplayWidth {
                node_id,
                display_width,
            } => {
                let (selection, changed_nodes, inverse) =
                    self.apply_set_image_width(node_id, display_width)?;
                (selection, changed_nodes, inverse, None)
            }
            Transaction::RestoreBlocks {
                index,
                remove_count,
                blocks,
            } => {
                let (selection, changed_nodes, inverse) =
                    self.apply_restore_blocks(index, remove_count, blocks)?;
                (selection, changed_nodes, inverse, None)
            }
        };

        let mut changed_nodes = changed_nodes;
        if changed_nodes.is_empty() {
            return Ok(ApplyOutcome::empty(selection));
        }

        if let Err(error) = self.validate_selection(selection) {
            self.rollback_journal(vec![inverse], original_revision, original_next_id);
            return Err(error);
        }
        self.revision = self.revision.saturating_add(1);
        for node_id in &changed_nodes {
            let index = self.blocks.index_of_node(*node_id);
            if let Some(index) = index {
                let mut block = self.blocks[index].clone();
                block.revision = self.revision;
                self.blocks.replace(index, block);
            }
        }
        // Capture structural replacement data only after changed blocks carry
        // this transaction's committed revision. Each batch splice remains
        // local and explicit; layout replays it against its intermediate
        // order rather than reading final-document ordinals.
        let mut structural_splices = SmallVec::new();
        if let Some(splice) = structural_plan.and_then(|plan| plan.finish(&self.blocks)) {
            structural_splices.push(splice);
        }
        let mut numbering_ranges = SmallVec::new();
        if let Some(range) = numbering_range {
            numbering_ranges.push(range);
        }
        // The IME replacement path calls this primitive repeatedly while
        // restoring and reapplying one provisional block. Validate only the
        // blocks it touched here. Structural operations (including
        // RestoreBlocks) perform their range and node-id collision checks
        // before reaching this point; the batch boundary repeats only the
        // changed-node invariant checks needed for a multi-operation batch.
        if let Err(error) = self.validate_changed_nodes(&changed_nodes) {
            self.rollback_journal(vec![inverse], original_revision, original_next_id);
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
            structural: !structural_splices.is_empty()
                || !numbering_ranges.is_empty()
                || self.blocks.len() != block_count_before,
            structural_splices,
            numbering_ranges,
            inverse,
            estimated_bytes,
            inserted_span,
        })
    }

    pub(crate) fn validate_selection(&self, selection: Selection) -> Result<(), DocumentError> {
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
        let bounds = self.selection_bounds_with_affinity(selection)?;
        Ok((
            bounds.start_index,
            bounds.start_offset,
            bounds.end_index,
            bounds.end_offset,
        ))
    }

    fn selection_bounds_with_affinity(
        &self,
        selection: Selection,
    ) -> Result<SelectionBounds, DocumentError> {
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
            Ordering::Less | Ordering::Equal => Ok(SelectionBounds {
                start_index: anchor_index,
                start_offset: selection.anchor.utf8_offset,
                start_affinity: selection.anchor.affinity,
                end_index: head_index,
                end_offset: selection.head.utf8_offset,
                end_affinity: selection.head.affinity,
            }),
            Ordering::Greater => Ok(SelectionBounds {
                start_index: head_index,
                start_offset: selection.head.utf8_offset,
                start_affinity: selection.head.affinity,
                end_index: anchor_index,
                end_offset: selection.anchor.utf8_offset,
                end_affinity: selection.anchor.affinity,
            }),
        }
    }

    fn editable_selection_bounds(
        &self,
        selection: Selection,
    ) -> Result<SelectionBounds, DocumentError> {
        let mut bounds = self.selection_bounds_with_affinity(selection)?;
        if bounds.start_index < bounds.end_index {
            if !is_text_block(&self.blocks[bounds.start_index])
                && bounds.start_affinity == Affinity::After
            {
                bounds.start_index = bounds.start_index.saturating_add(1);
                bounds.start_offset = 0;
                bounds.start_affinity = Affinity::Before;
            }
            if bounds.start_index <= bounds.end_index
                && !is_text_block(&self.blocks[bounds.end_index])
                && bounds.end_affinity == Affinity::Before
            {
                bounds.end_index = bounds.end_index.saturating_sub(1);
                bounds.end_offset = self.blocks[bounds.end_index]
                    .content
                    .as_text()
                    .map_or(0, str::len);
                bounds.end_affinity = Affinity::After;
            }
        }
        if bounds.start_index > bounds.end_index {
            return Err(DocumentError::InvalidOperation(
                "selection leaves no editable block seam between structural nodes".into(),
            ));
        }
        Ok(bounds)
    }

    /// Return the insertion slot for the empty seam between two adjacent
    /// structural blocks.  An image's `After` point and the next image's
    /// `Before` point are a real document boundary even though no text block
    /// owns that span.
    fn adjacent_structural_seam(
        &self,
        selection: Selection,
    ) -> Result<Option<usize>, DocumentError> {
        let bounds = self.selection_bounds_with_affinity(selection)?;
        if bounds.start_index.saturating_add(1) != bounds.end_index
            || bounds.start_affinity != Affinity::After
            || bounds.end_affinity != Affinity::Before
            || !is_structural_block(&self.blocks[bounds.start_index])
            || !is_structural_block(&self.blocks[bounds.end_index])
        {
            return Ok(None);
        }
        Ok(Some(bounds.end_index))
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
        for block in self
            .blocks
            .iter_range(start_index..end_index.saturating_add(1))
        {
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
    ) -> Result<
        (
            Selection,
            SmallVec<[NodeId; 4]>,
            TransactionBatch,
            Option<InsertedTextSpan>,
        ),
        DocumentError,
    > {
        if let Some(insert_at) = self.adjacent_structural_seam(selection)? {
            if text.is_empty() {
                return Ok((
                    selection,
                    SmallVec::new(),
                    TransactionBatch::default(),
                    None,
                ));
            }
            let inserted = Block::text(self.new_node_id()?, BlockKind::Paragraph, text.clone());
            let inserted_id = inserted.id;
            self.blocks.insert(insert_at, inserted);
            let mut changed_nodes = SmallVec::new();
            push_unique(&mut changed_nodes, inserted_id);
            return Ok((
                Selection::caret(DocPoint::with_affinity(
                    inserted_id,
                    text.len(),
                    Affinity::After,
                )),
                changed_nodes,
                TransactionBatch(vec![Transaction::RestoreBlocks {
                    index: insert_at,
                    remove_count: 1,
                    blocks: Vec::new(),
                }]),
                Some(InsertedTextSpan {
                    node_id: inserted_id,
                    range: 0..text.len(),
                }),
            ));
        }
        let bounds = self.editable_selection_bounds(selection)?;
        let (start_index, start_offset, end_index, end_offset) = (
            bounds.start_index,
            bounds.start_offset,
            bounds.end_index,
            bounds.end_offset,
        );
        self.ensure_editable_range(start_index, end_index)?;

        if start_index == end_index && !is_text_block(&self.blocks[start_index]) {
            let original = self.blocks[start_index].clone();
            if selection.is_caret() {
                if text.is_empty() {
                    return Ok((
                        selection,
                        SmallVec::new(),
                        TransactionBatch::default(),
                        None,
                    ));
                }
                let inserted = Block::text(self.new_node_id()?, BlockKind::Paragraph, text.clone());
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
                    Some(InsertedTextSpan {
                        node_id: inserted_id,
                        range: 0..text.len(),
                    }),
                ));
            }

            let block_id = original.id;
            let replacement = Block::text(block_id, BlockKind::Paragraph, text.clone());
            self.blocks.replace(start_index, replacement);
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
                Some(InsertedTextSpan {
                    node_id: block_id,
                    range: 0..text.len(),
                }),
            ));
        }

        let originals = self
            .blocks
            .collect_range(start_index..end_index.saturating_add(1));
        let original_ids = originals.iter().map(|block| block.id).collect::<Vec<_>>();
        let is_empty = start_index == end_index && start_offset == end_offset;

        if text.is_empty() && is_empty {
            return Ok((
                selection,
                SmallVec::new(),
                TransactionBatch::default(),
                None,
            ));
        }

        let insertion_offset = if !is_empty {
            self.delete_range_mut(
                start_index,
                start_offset,
                end_index,
                end_offset,
                !text.is_empty(),
            )?
        } else {
            start_offset
        };

        let block_id = self.blocks[start_index].id;
        let inserted_len = text.len();
        let mut block = self.blocks[start_index].clone();
        let (old_text, old_styles) = text_parts(&block.content)?;
        let mut new_text = String::with_capacity(old_text.len() + inserted_len);
        new_text.push_str(&old_text[..insertion_offset]);
        new_text.push_str(&text);
        new_text.push_str(&old_text[insertion_offset..]);
        let mut new_styles = insert_styles(old_styles, insertion_offset, inserted_len);
        if is_empty {
            let inherited = insertion_marks(old_styles, insertion_offset, selection.head.affinity);
            if !inherited.is_empty() {
                new_styles.push(StyledRun {
                    range: insertion_offset..insertion_offset.saturating_add(inserted_len),
                    marks: inherited,
                });
                normalize_styles(&mut new_styles);
            }
        }
        normalize_styles_for_text(&new_text, &mut new_styles);
        block.content = BlockContent::Text {
            text: new_text,
            styles: new_styles,
        };
        self.blocks.replace(start_index, block);

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
        Ok((
            after,
            changed_nodes,
            inverse,
            Some(InsertedTextSpan {
                node_id: block_id,
                range: insertion_offset..insertion_offset.saturating_add(inserted_len),
            }),
        ))
    }

    fn apply_delete_range(
        &mut self,
        selection: Selection,
    ) -> Result<(Selection, SmallVec<[NodeId; 4]>, TransactionBatch), DocumentError> {
        if self.adjacent_structural_seam(selection)?.is_some() {
            return Ok((
                Selection::caret(selection.anchor),
                SmallVec::new(),
                TransactionBatch::default(),
            ));
        }
        let bounds = self.editable_selection_bounds(selection)?;
        let (start_index, start_offset, end_index, end_offset) = (
            bounds.start_index,
            bounds.start_offset,
            bounds.end_index,
            bounds.end_offset,
        );
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

        let originals = self
            .blocks
            .collect_range(start_index..end_index.saturating_add(1));
        let original_ids = originals.iter().map(|block| block.id).collect::<Vec<_>>();
        let raw_offset =
            self.delete_range_mut(start_index, start_offset, end_index, end_offset, false)?;
        let node_id = self.blocks[start_index].id;
        let result_offset = {
            let text = self.blocks[start_index]
                .content
                .as_text()
                .expect("delete range retains a text block");
            resolve_grapheme_offset(text, raw_offset, Affinity::After)
        };
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
        preserve_style_seam: bool,
    ) -> Result<usize, DocumentError> {
        if start_index == end_index {
            let mut block = self.blocks[start_index].clone();
            let (text, styles) = text_parts(&block.content)?;
            let (new_text, new_styles) =
                delete_text(text, styles, start_offset, end_offset, preserve_style_seam);
            block.content = BlockContent::Text {
                text: new_text,
                styles: new_styles,
            };
            self.blocks.replace(start_index, block);
            return Ok(start_offset);
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
        if !preserve_style_seam {
            normalize_styles_for_text(&merged_text, &mut merged_styles);
        }
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
        self.blocks
            .splice(start_index..end_index.saturating_add(1), [merged]);
        Ok(prefix.len())
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
        let right_id = self.new_node_id()?;
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
        self.blocks
            .splice(index..index.saturating_add(1), [left, right]);
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
        let mut merged_block = self.blocks[left_index].clone();
        merged_block.content = BlockContent::Text {
            text: merged_text,
            styles: merged_styles,
        };
        self.blocks
            .splice(left_index..right_index.saturating_add(1), [merged_block]);
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
        let originals = self
            .blocks
            .collect_range(start_index..end_index.saturating_add(1));
        let mut changed_nodes = SmallVec::new();
        let mut replacements = originals.clone();
        for block in &mut replacements {
            if block.kind != kind {
                block.kind = kind.clone();
                push_unique(&mut changed_nodes, block.id);
            }
        }
        if changed_nodes.is_empty() {
            return Ok((selection, changed_nodes, TransactionBatch::default()));
        }
        self.blocks
            .splice(start_index..end_index.saturating_add(1), replacements);
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
        let originals = self
            .blocks
            .collect_range(start_index..end_index.saturating_add(1));
        let mut replacements = originals.clone();
        let mut changed_nodes = SmallVec::new();
        for (offset, block) in replacements.iter_mut().enumerate() {
            let index = start_index.saturating_add(offset);
            let Some((range_start, range_end)) =
                self.text_range_for_block(index, start_index, start_offset, end_index, end_offset)
            else {
                continue;
            };
            if range_start == range_end {
                continue;
            }
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
        self.blocks
            .splice(start_index..end_index.saturating_add(1), replacements);
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
        let originals = self
            .blocks
            .collect_range(start_index..end_index.saturating_add(1));
        let mut replacements = originals.clone();
        let mut changed_nodes = SmallVec::new();
        for (offset, block) in replacements.iter_mut().enumerate() {
            let index = start_index.saturating_add(offset);
            let Some((range_start, range_end)) =
                self.text_range_for_block(index, start_index, start_offset, end_index, end_offset)
            else {
                continue;
            };
            if range_start == range_end {
                continue;
            }
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
        self.blocks
            .splice(start_index..end_index.saturating_add(1), replacements);
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
        let originals = self
            .blocks
            .collect_range(start_index..end_index.saturating_add(1));
        let mut replacements = originals.clone();
        let mut changed_nodes = SmallVec::new();
        for block in &mut replacements {
            if block.alignment != alignment {
                block.alignment = alignment;
                push_unique(&mut changed_nodes, block.id);
            }
        }
        if changed_nodes.is_empty() {
            return Ok((selection, changed_nodes, TransactionBatch::default()));
        }
        self.blocks
            .splice(start_index..end_index.saturating_add(1), replacements);
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
            for block in self
                .blocks
                .iter_range(start_index..end_index.saturating_add(1))
            {
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
        let originals = self
            .blocks
            .collect_range(start_index..end_index.saturating_add(1));
        let mut replacements = originals.clone();
        let mut changed_nodes = SmallVec::new();
        for block in &mut replacements {
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
        self.blocks
            .splice(start_index..end_index.saturating_add(1), replacements);
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
        if let Some(insert_at) = self.adjacent_structural_seam(selection)? {
            let image = Block {
                id: self.new_node_id()?,
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
        let bounds = self.editable_selection_bounds(selection)?;
        let (start_index, start_offset, end_index, end_offset) = (
            bounds.start_index,
            bounds.start_offset,
            bounds.end_index,
            bounds.end_offset,
        );
        self.ensure_editable_range(start_index, end_index)?;

        if start_index == end_index && !is_text_block(&self.blocks[start_index]) {
            let original = self.blocks[start_index].clone();
            if selection.is_caret() {
                let image = Block {
                    id: self.new_node_id()?,
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
            self.blocks.replace(
                start_index,
                Block {
                    id: image_id,
                    kind: BlockKind::Image,
                    content: BlockContent::Image {
                        resource_id,
                        natural_size,
                        display_width: None,
                    },
                    alignment: original.alignment,
                    revision: original.revision,
                },
            );
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

        let originals = self
            .blocks
            .collect_range(start_index..end_index.saturating_add(1));
        let original_ids = originals.iter().map(|block| block.id).collect::<Vec<_>>();
        let is_empty = start_index == end_index && start_offset == end_offset;
        // Reserve every fresh identity before deleting the selected range.
        // If the second allocation is exhausted, this operation must leave
        // the document unchanged rather than publishing a partial deletion.
        let image_id = self.new_node_id()?;
        let right_id = self.new_node_id()?;
        let insertion_offset = if !is_empty {
            self.delete_range_mut(start_index, start_offset, end_index, end_offset, true)?
        } else {
            start_offset
        };

        let original_block = self.blocks[start_index].clone();
        let (text, styles) = text_parts(&original_block.content)?;
        let (left_text, left_styles, right_text, right_styles) =
            split_text(text, styles, insertion_offset);
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
        self.blocks.splice(
            start_index..start_index.saturating_add(1),
            [left, image, right],
        );
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
        let mut updated = original.clone();
        let BlockContent::Image {
            display_width: current,
            ..
        } = &mut updated.content
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
        self.blocks.replace(index, updated);
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
        // Inverse ranges are normally one small contiguous run (for example
        // the provisional IME block). Do not allocate a set proportional to
        // the whole document just to validate that local replacement. The
        // pairwise check is deliberately bounded by the supplied replacement
        // range; only a replacement that introduces an id from outside that
        // range needs the full outside-range collision scan.
        let mut replacement_ids = SmallVec::<[NodeId; 4]>::new();
        for block in &blocks {
            if replacement_ids.contains(&block.id) {
                return Err(DocumentError::InvalidOperation(
                    "inverse block range would duplicate a node id".into(),
                ));
            }
            replacement_ids.push(block.id);
        }
        let current_range = self
            .blocks
            .collect_range(index..index.saturating_add(remove_count));
        let all_ids_are_replaced = blocks.iter().all(|replacement| {
            current_range
                .iter()
                .any(|current| current.id == replacement.id)
        });
        if !all_ids_are_replaced
            && blocks.iter().any(|replacement| {
                self.blocks.contains_node(replacement.id)
                    && !current_range
                        .iter()
                        .any(|current| current.id == replacement.id)
            })
        {
            return Err(DocumentError::InvalidOperation(
                "inverse block range would duplicate a node id".into(),
            ));
        }
        let replacement_count = blocks.len();
        let old_blocks = current_range;
        self.advance_next_id_for_blocks(&blocks);
        self.blocks
            .splice(index..index.saturating_add(remove_count), blocks);
        let mut changed_nodes = SmallVec::new();
        for block in &old_blocks {
            push_unique(&mut changed_nodes, block.id);
        }
        for node_id in replacement_ids {
            push_unique(&mut changed_nodes, node_id);
        }
        let selection = self.selection_near_index(index);
        let inverse = TransactionBatch(vec![Transaction::RestoreBlocks {
            index,
            remove_count: replacement_count,
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
            self.blocks.first_text().map_or_else(
                || Selection::caret(DocPoint::new(block.id, 0)),
                |candidate| {
                    Selection::caret(DocPoint::with_affinity(
                        candidate.id,
                        candidate.content.as_text().map_or(0, str::len),
                        Affinity::After,
                    ))
                },
            )
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

fn is_navigation_block(block: &Block) -> bool {
    block.content.as_text().is_some() || block.kind == BlockKind::Image
}

fn block_flat_lengths(block: &Block) -> (usize, usize) {
    match &block.content {
        BlockContent::Text { text, .. } => (text.len(), text.chars().map(char::len_utf16).sum()),
        _ => ('\u{fffc}'.len_utf8(), 1),
    }
}

fn is_structural_block(block: &Block) -> bool {
    matches!(
        (&block.kind, &block.content),
        (BlockKind::Image, BlockContent::Image { .. })
            | (BlockKind::Attachment, BlockContent::Attachment { .. })
            | (BlockKind::Divider, BlockContent::Empty)
    )
}

fn validate_block_invariants(block: &Block) -> Result<(), DocumentError> {
    validate_kind(&block.kind)?;
    match (&block.kind, &block.content) {
        (BlockKind::Image, BlockContent::Image { .. })
        | (BlockKind::Attachment, BlockContent::Attachment { .. })
        | (BlockKind::Divider, BlockContent::Empty) => Ok(()),
        (
            BlockKind::Paragraph
            | BlockKind::Heading { .. }
            | BlockKind::BulletItem { .. }
            | BlockKind::OrderedItem { .. }
            | BlockKind::CheckItem { .. }
            | BlockKind::Quote
            | BlockKind::Code,
            BlockContent::Text { text, styles },
        ) => validate_styles(block.id, text, styles),
        _ => Err(DocumentError::InvalidBlockContent(block.id)),
    }
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
    if styles.is_empty() {
        return Ok(());
    }
    let grapheme_boundaries = validation_grapheme_boundaries(text);
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
        if grapheme_boundaries.binary_search(&run.range.start).is_err()
            || grapheme_boundaries.binary_search(&run.range.end).is_err()
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
    #[cfg(test)]
    GRAPHEME_RESOLUTION_CALLS.fetch_add(1, AtomicOrdering::Relaxed);
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

#[cfg(test)]
static GRAPHEME_RESOLUTION_CALLS: AtomicUsize = AtomicUsize::new(0);

#[cfg(test)]
thread_local! {
    static BLOCK_SEQUENCE_VISITS: Cell<usize> = const { Cell::new(0) };
    static VALIDATION_GRAPHEME_STEPS: Cell<usize> = const { Cell::new(0) };
}

#[cfg(test)]
pub(crate) fn reset_block_sequence_visit_counter() {
    BLOCK_SEQUENCE_VISITS.with(|visits| visits.set(0));
}

#[cfg(test)]
pub(crate) fn block_sequence_visit_counter() -> usize {
    BLOCK_SEQUENCE_VISITS.with(Cell::get)
}

#[cfg(test)]
pub(crate) fn reset_grapheme_resolution_counter() {
    GRAPHEME_RESOLUTION_CALLS.store(0, AtomicOrdering::Relaxed);
}

#[cfg(test)]
pub(crate) fn grapheme_resolution_counter() -> usize {
    GRAPHEME_RESOLUTION_CALLS.load(AtomicOrdering::Relaxed)
}

#[cfg(test)]
pub(crate) fn reset_validation_grapheme_counter() {
    VALIDATION_GRAPHEME_STEPS.with(|steps| steps.set(0));
}

#[cfg(test)]
pub(crate) fn validation_grapheme_counter() -> usize {
    VALIDATION_GRAPHEME_STEPS.with(Cell::get)
}

/// Snap style boundaries out of a newly joined grapheme and rebuild the
/// non-overlapping run list by taking the union of marks per grapheme-safe
/// segment.  Joining text can make an old byte boundary cease to be a
/// grapheme boundary; a run must expand to cover that complete grapheme.
fn normalize_styles_for_text(text: &str, styles: &mut SmallVec<[StyledRun; 4]>) {
    if styles.is_empty() {
        return;
    }
    let grapheme_boundaries = grapheme_boundaries(text);
    let mut boundaries = vec![0, text.len()];
    let mut events = Vec::with_capacity(styles.len().saturating_mul(2));
    for run in styles.iter() {
        let start = snap_grapheme_offset(&grapheme_boundaries, run.range.start, Affinity::Before);
        let end = snap_grapheme_offset(&grapheme_boundaries, run.range.end, Affinity::After);
        if start >= end {
            continue;
        }
        boundaries.push(start);
        boundaries.push(end);
        for mark in &run.marks {
            events.push((start, true, mark.clone()));
            events.push((end, false, mark.clone()));
        }
    }
    boundaries.sort_unstable();
    boundaries.dedup();
    events.sort_by(|left, right| {
        left.0
            .cmp(&right.0)
            .then_with(|| left.1.cmp(&right.1))
            .then_with(|| left.2.cmp(&right.2))
    });
    let mut normalized = SmallVec::new();
    let mut active_marks: BTreeMap<Mark, usize> = BTreeMap::new();
    // As in donor `build_text_runs`, both sides are sorted and this cursor
    // only advances; the event sweep additionally unions overlapping marks.
    let mut event_idx = 0usize;
    for window in boundaries.windows(2) {
        let (start, end) = (window[0], window[1]);
        if start >= end {
            continue;
        }
        while event_idx < events.len() && events[event_idx].0 <= start {
            let (_, add, mark) = &events[event_idx];
            if *add {
                *active_marks.entry(mark.clone()).or_default() += 1;
            } else if let Some(count) = active_marks.get_mut(mark) {
                *count = count.saturating_sub(1);
                if *count == 0 {
                    active_marks.remove(mark);
                }
            }
            event_idx += 1;
        }
        let marks: SmallVec<[Mark; 4]> = active_marks.keys().cloned().collect();
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

fn grapheme_boundaries(text: &str) -> Vec<usize> {
    let mut boundaries = Vec::with_capacity(text.len().saturating_add(1));
    boundaries.push(0);
    for (start, _) in text.grapheme_indices(true).skip(1) {
        boundaries.push(start);
    }
    if boundaries.last().copied() != Some(text.len()) {
        boundaries.push(text.len());
    }
    boundaries
}

/// Build the reusable boundary index consumed by `validate_styles`.  Keeping
/// this scan separate from the normalization helper makes the validation hot
/// path observable in tests and ensures every run endpoint uses binary search
/// against one index rather than rescanning the text.
fn validation_grapheme_boundaries(text: &str) -> Vec<usize> {
    let mut boundaries = Vec::with_capacity(text.len().saturating_add(1));
    boundaries.push(0);
    for (start, _) in text.grapheme_indices(true) {
        #[cfg(test)]
        VALIDATION_GRAPHEME_STEPS.with(|steps| steps.set(steps.get().saturating_add(1)));
        if start != 0 {
            boundaries.push(start);
        }
    }
    if boundaries.last().copied() != Some(text.len()) {
        boundaries.push(text.len());
    }
    boundaries
}

fn snap_grapheme_offset(boundaries: &[usize], preferred: usize, affinity: Affinity) -> usize {
    let preferred = preferred.min(*boundaries.last().unwrap_or(&0));
    match boundaries.binary_search(&preferred) {
        Ok(offset) => boundaries[offset],
        Err(index) => match affinity {
            Affinity::Before => boundaries[index.saturating_sub(1)],
            Affinity::After => boundaries[index.min(boundaries.len().saturating_sub(1))],
        },
    }
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

/// Resolve the marks used by a collapsed-caret insertion.  Affinity is the
/// single boundary rule shared by command state and model insertion:
/// `Before` looks to the run ending at the seam, while `After` looks to the
/// run beginning at the seam.  Strict interior offsets belong to their
/// containing run.  This avoids a toolbar state that claims Bold while the
/// next inserted grapheme is unstyled.
pub(crate) fn insertion_marks(
    styles: &[StyledRun],
    offset: usize,
    affinity: Affinity,
) -> SmallVec<[Mark; 4]> {
    styles
        .iter()
        .find(|run| {
            (run.range.start < offset && offset < run.range.end)
                || (run.range.end == offset && affinity == Affinity::Before)
                || (run.range.start == offset && affinity == Affinity::After)
        })
        .map(|run| run.marks.clone())
        .unwrap_or_default()
}

fn delete_text(
    text: &str,
    styles: &[StyledRun],
    start: usize,
    end: usize,
    preserve_style_seam: bool,
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
    if !preserve_style_seam {
        normalize_styles_for_text(&new_text, &mut new_styles);
    }
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

#[cfg(test)]
mod tests {
    use super::{
        reset_validation_grapheme_counter, validation_grapheme_boundaries,
        validation_grapheme_counter,
    };
    use std::sync::{Arc, Barrier};
    use std::thread;

    #[test]
    fn validation_grapheme_counter_is_thread_local() {
        let ready = Arc::new(Barrier::new(2));
        let counted = Arc::new(Barrier::new(2));
        let handles = (0..2)
            .map(|_| {
                let ready = Arc::clone(&ready);
                let counted = Arc::clone(&counted);
                thread::spawn(move || {
                    reset_validation_grapheme_counter();
                    ready.wait();
                    let _ = validation_grapheme_boundaries("a");
                    counted.wait();
                    validation_grapheme_counter()
                })
            })
            .collect::<Vec<_>>();

        for handle in handles {
            assert_eq!(handle.join().unwrap(), 1);
        }
    }
}
