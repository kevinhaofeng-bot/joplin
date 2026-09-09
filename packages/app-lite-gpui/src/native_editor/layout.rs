//! Viewport-bounded layout projection for the structured native document.
//!
//! The registry owns the measured `WrappedLine` values. `visible` contains
//! only viewport geometry, so an evicted shaped block cannot remain alive in a
//! second renderer-owned vector.

use std::cmp::Ordering;
use std::collections::{HashMap, HashSet, VecDeque};
use std::mem::size_of;
use std::ops::Range;

use gpui::{
    Bounds, Font, FontStyle, FontWeight, Hsla, Pixels, Point, ShapedGlyph, SharedString,
    StrikethroughStyle, TextAlign, TextRun, TextStyle, UnderlineStyle, WrapBoundary, WrappedLine,
    WrappedLineLayout, point, px, rgba, size,
};
use smallvec::SmallVec;
use sum_tree::{Bias, ContextLessSummary, Dimension, Item, KeyedItem, SeekTarget, SumTree};
use unicode_segmentation::UnicodeSegmentation;

use super::model::{
    Affinity, BlockContent, BlockKind, DocPoint, Document, Mark, NodeId, Selection, TextAlignment,
};
use super::transaction::StructuralSplice;

pub const LAYOUT_CACHE_BUDGET_BYTES: usize = 16 * 1024 * 1024;
const DEFAULT_TEXT_HEIGHT: f32 = 24.0;
const DEFAULT_IMAGE_HEIGHT: f32 = 180.0;
const PREFETCH_VIEWPORTS: f32 = 1.0;
const FALLBACK_GLYPH_WIDTH: f32 = 8.0;
const CARET_WIDTH: f32 = 1.0;
const LIST_MARKER_WIDTH: f32 = 22.0;
const LIST_DEPTH_INDENT: f32 = 20.0;
const NUMBERING_CHECKPOINT_STRIDE: usize = 64;

#[derive(Clone)]
struct BlockVisualStyle {
    font: Font,
    font_size: Pixels,
    line_height: Pixels,
    text_inset: Pixels,
    text_align: TextAlign,
}

fn block_visual_style(
    block: &super::model::Block,
    style: &TextStyle,
    rem_size: Pixels,
) -> BlockVisualStyle {
    let mut font = style.font();
    let mut font_size = style.font_size.to_pixels(rem_size);
    let mut line_height = style.line_height_in_pixels(rem_size);
    match block.kind {
        BlockKind::Heading { level: 1 } => {
            font_size = px(30.0);
            line_height = px(40.0);
            font.weight = FontWeight::BOLD;
        }
        BlockKind::Heading { level: 2 } => {
            font_size = px(25.0);
            line_height = px(34.0);
            font.weight = FontWeight::BOLD;
        }
        BlockKind::Heading { level: 3 } => {
            font_size = px(21.0);
            line_height = px(29.0);
            font.weight = FontWeight::BOLD;
        }
        BlockKind::Heading { .. } => {
            font_size = px(18.0);
            line_height = px(26.0);
            font.weight = FontWeight::BOLD;
        }
        _ => {}
    }
    let depth = list_depth(&block.kind);
    let text_inset = depth.map(|_| px(LIST_MARKER_WIDTH)).unwrap_or_default();
    BlockVisualStyle {
        font,
        font_size,
        line_height,
        text_inset,
        text_align: match block.alignment {
            TextAlignment::Left => TextAlign::Left,
            TextAlignment::Center => TextAlign::Center,
            TextAlignment::Right => TextAlign::Right,
        },
    }
}

fn list_depth(kind: &BlockKind) -> Option<u8> {
    match kind {
        BlockKind::BulletItem { depth }
        | BlockKind::OrderedItem { depth }
        | BlockKind::CheckItem { depth, .. } => Some(*depth),
        _ => None,
    }
}

fn block_bounds(width: f32, block: &super::model::Block) -> Bounds<Pixels> {
    let left = list_depth(&block.kind)
        .map(|depth| depth as f32 * LIST_DEPTH_INDENT)
        .unwrap_or(0.0);
    Bounds::new(
        point(px(left), px(0.0)),
        size(px((width - left).max(1.0)), px(DEFAULT_TEXT_HEIGHT)),
    )
}

fn styled_text_runs(
    block: &super::model::Block,
    text: &SharedString,
    base_font: &Font,
) -> Vec<TextRun> {
    let Some(styles) = block.content.styles() else {
        return vec![TextRun {
            len: text.len(),
            font: base_font.clone(),
            color: gpui::black(),
            background_color: None,
            underline: None,
            strikethrough: None,
        }];
    };
    let mut boundaries = vec![0, text.len()];
    for run in styles {
        boundaries.push(run.range.start.min(text.len()));
        boundaries.push(run.range.end.min(text.len()));
    }
    boundaries.sort_unstable();
    boundaries.dedup();
    let underline_thickness = px(1.0);
    let mut shaped = Vec::new();
    let mut style_index = 0usize;
    for window in boundaries.windows(2) {
        let start = window[0];
        let end = window[1];
        if start >= end {
            continue;
        }
        while style_index + 1 < styles.len() && styles[style_index].range.end <= start {
            style_index += 1;
        }
        let marks = styles
            .get(style_index)
            .filter(|run| run.range.start <= start && start < run.range.end)
            .map(|run| run.marks.as_slice())
            .unwrap_or(&[]);
        let mut font = base_font.clone();
        if marks.iter().any(|mark| matches!(mark, Mark::Bold)) {
            font.weight = FontWeight::BOLD;
        }
        if marks.iter().any(|mark| matches!(mark, Mark::Italic)) {
            font.style = FontStyle::Italic;
        }
        let link = marks.iter().any(|mark| matches!(mark, Mark::Link(_)));
        let underline = marks.iter().any(|mark| matches!(mark, Mark::Underline)) || link;
        let strike = marks.iter().any(|mark| matches!(mark, Mark::Strike));
        let color = if link { gpui::blue() } else { gpui::black() };
        shaped.push(TextRun {
            len: end - start,
            font,
            color,
            background_color: marks
                .iter()
                .any(|mark| matches!(mark, Mark::Highlight))
                .then(|| Hsla::from(rgba(0xffd84d66))),
            underline: underline.then_some(UnderlineStyle {
                color: Some(color),
                thickness: underline_thickness,
                wavy: false,
            }),
            strikethrough: strike.then_some(StrikethroughStyle {
                color: Some(color),
                thickness: underline_thickness,
            }),
        });
    }
    if shaped.is_empty() {
        vec![TextRun {
            len: text.len(),
            font: base_font.clone(),
            color: gpui::black(),
            background_color: None,
            underline: None,
            strikethrough: None,
        }]
    } else {
        shaped
    }
}

/// Count the decoration runs that GPUI's `shape_text` retains for each hard
/// line. `shape_text` starts a fresh `SmallVec<[DecorationRun; 32]>` for every
/// hard line and merges adjacent source runs only when all decoration fields
/// match. The newline itself is consumed after the line and is therefore not
/// part of that line's decoration run count.
fn decoration_run_counts_per_hard_line(text: &str, runs: &[TextRun]) -> SmallVec<[usize; 4]> {
    let mut counts = SmallVec::new();
    let mut run_index = 0usize;
    let mut run_remaining = 0usize;
    let mut lines = text.split('\n').peekable();

    while let Some(line) = lines.next() {
        let mut count = 0usize;
        let mut previous_run: Option<&TextRun> = None;

        let mut remaining_line = line.len();
        while remaining_line > 0 {
            while run_index < runs.len() && run_remaining == 0 {
                run_remaining = runs[run_index].len;
                if run_remaining == 0 {
                    run_index += 1;
                }
            }
            let Some(run) = runs.get(run_index) else {
                break;
            };
            let same_decoration = previous_run.is_some_and(|previous| {
                previous.color == run.color
                    && previous.underline == run.underline
                    && previous.strikethrough == run.strikethrough
                    && previous.background_color == run.background_color
            });
            if !same_decoration {
                count = count.saturating_add(1);
            }
            previous_run = Some(run);
            let consumed = remaining_line.min(run_remaining);
            remaining_line -= consumed;
            run_remaining -= consumed;
            if run_remaining == 0 {
                run_index += 1;
            }
        }

        counts.push(count);

        // Match GPUI's `shape_text`: after shaping a hard line it consumes
        // exactly one newline byte from the current source run, advancing to
        // the next run only when that run is exhausted.
        if lines.peek().is_some() {
            while run_index < runs.len() && run_remaining == 0 {
                run_remaining = runs[run_index].len;
                if run_remaining == 0 {
                    run_index += 1;
                }
            }
            if run_remaining > 0 {
                run_remaining -= 1;
                if run_remaining == 0 {
                    run_index += 1;
                }
            }
        }
    }

    counts
}

fn decoration_spill_bytes(line_run_counts: &[usize]) -> usize {
    line_run_counts
        .iter()
        .map(|count| {
            if *count > 32 {
                count
                    .checked_next_power_of_two()
                    .unwrap_or(usize::MAX)
                    .saturating_mul(size_of::<gpui::DecorationRun>())
            } else {
                0
            }
        })
        .sum()
}

/// Exact geometry for one block currently in or near the viewport.
///
/// The field is intentionally named `text_lines` for the Task 4 public
/// shape, but the values are the donor's real `WrappedLine` objects so hard
/// newlines and soft wraps share one coordinate system.
#[derive(Clone, Debug)]
pub struct BlockLayout {
    pub node_id: NodeId,
    pub bounds: Bounds<Pixels>,
    /// Horizontal space reserved for a list marker. Text shaping and all
    /// selection/caret geometry use this same inset.
    pub text_inset: Pixels,
    pub text_align: TextAlign,
    pub text_lines: Vec<WrappedLine>,
    pub before: DocPoint,
    pub after: DocPoint,
}

/// All inputs that can change the shaped geometry of a block. Keep the full
/// GPUI `Font` rather than a short numeric style revision: family, OpenType
/// features, fallbacks, weight, and italic/oblique style are all part of the
/// shaping identity.
#[derive(Clone, Debug, Eq, PartialEq, Hash)]
struct ShapeKey {
    block_revision: u64,
    width_bits: u32,
    font: Font,
    font_size_bits: u64,
    line_height_bits: u64,
    wrap_mode: u8,
}

impl ShapeKey {
    fn new(
        block_revision: u64,
        width: f32,
        font: Font,
        font_size: Pixels,
        line_height: Pixels,
    ) -> Self {
        Self {
            block_revision,
            width_bits: width.to_bits(),
            font,
            font_size_bits: font_size.to_f64().to_bits(),
            line_height_bits: line_height.to_f64().to_bits(),
            // The native editor currently uses GPUI's normal wrapping mode;
            // keep the discriminator so a future wrap-policy change cannot
            // accidentally reuse old shaped lines.
            wrap_mode: 0,
        }
    }

    fn geometry(block_revision: u64, width: f32, line_height: Pixels) -> Self {
        Self::new(
            block_revision,
            width,
            gpui::font(".SystemUIFont"),
            px(0.0),
            line_height,
        )
    }
}

/// Bounded exact-layout cache entry. Shaped runs, glyphs and selection
/// geometry are counted in `bytes`; no shaped line is retained in `visible`.
#[derive(Clone, Debug)]
pub struct CachedBlockLayout {
    pub layout: BlockLayout,
    pub bytes: usize,
    /// Bytes retained by the cache entry itself, excluding the per-frame
    /// snapshot clone. This is the retained side of the admission bound.
    pub(crate) retained_bytes: usize,
    /// Explicit upper bound for the owned buffers allocated by the real
    /// render snapshot clone, including spilled decoration runs.
    pub(crate) snapshot_clone_bytes: usize,
    pub revision: u64,
    pub width: f32,
    pub selection_geometry_bytes: usize,
    /// Upper bound for the GPUI decoration runs retained by the wrapped
    /// lines. `WrappedLine` keeps these in a private SmallVec, so retain the
    /// per-hard-line source run counts alongside the public shaped geometry
    /// for budgeting.
    pub(crate) decoration_run_count: usize,
    pub(crate) decoration_line_run_counts: SmallVec<[usize; 4]>,
    /// Background-bearing runs that survived into the shaped input. The
    /// renderer records a paint only after the real GPUI background pass.
    pub(crate) shaped_background_run_count: usize,
    pub(crate) is_image: bool,
    pub(crate) line_height: Pixels,
    shape_key: ShapeKey,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum NumberingKind {
    Ordered(u8),
    Bullet(u8),
    Check(u8),
    Boundary,
}

type NumberingCounter = (u8, usize);

/// Numbering state is retained only at sparse sequence checkpoints. A
/// checkpoint stores the active list depths, not a MAX_LIST_DEPTH-sized array
/// for every block; the ordinary shallow case stays inline in SmallVec.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct NumberingCursor {
    counters: SmallVec<[NumberingCounter; 4]>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct NumberingCheckpoint {
    cursor: NumberingCursor,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct OrderSummary {
    count: usize,
}

impl ContextLessSummary for OrderSummary {
    fn zero() -> Self {
        Self::default()
    }

    fn add_summary(&mut self, summary: &Self) {
        self.count = self.count.saturating_add(summary.count);
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct OrderItem {
    node_id: NodeId,
}

impl Item for OrderItem {
    type Summary = OrderSummary;

    fn summary(&self, _: ()) -> Self::Summary {
        OrderSummary { count: 1 }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord)]
struct OrderCount(usize);

impl<'a> Dimension<'a, OrderSummary> for OrderCount {
    fn zero(_: ()) -> Self {
        Self(0)
    }

    fn add_summary(&mut self, summary: &'a OrderSummary, _: ()) {
        self.0 = self.0.saturating_add(summary.count);
    }
}

fn numbering_kind(kind: &BlockKind) -> NumberingKind {
    match kind {
        BlockKind::OrderedItem { depth } => NumberingKind::Ordered(*depth),
        BlockKind::BulletItem { depth } => NumberingKind::Bullet(*depth),
        BlockKind::CheckItem { depth, .. } => NumberingKind::Check(*depth),
        _ => NumberingKind::Boundary,
    }
}

/// A compact, document-order height index. This follows GPUI's list
/// implementation: the tree stores one item per block and summarizes both
/// item count and measured height, so viewport seeks and localized height
/// replacements share the same balanced tree rather than rebuilding a full
/// prefix vector for every convergence wave.
#[derive(Clone, Debug, Default)]
struct HeightSummary {
    count: usize,
    height: f32,
    max_index: usize,
}

impl ContextLessSummary for HeightSummary {
    fn zero() -> Self {
        Self::default()
    }

    fn add_summary(&mut self, summary: &Self) {
        self.count = self.count.saturating_add(summary.count);
        self.height += summary.height;
        self.max_index = self.max_index.max(summary.max_index);
    }
}

#[derive(Clone, Debug)]
struct HeightItem {
    index: usize,
    node_id: NodeId,
    revision: u64,
    height: f32,
}

impl Item for HeightItem {
    type Summary = HeightSummary;

    fn summary(&self, _: ()) -> Self::Summary {
        HeightSummary {
            count: 1,
            height: self.height.max(1.0),
            max_index: self.index,
        }
    }
}

impl KeyedItem for HeightItem {
    type Key = Count;

    fn key(&self) -> Self::Key {
        Count(self.index)
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord)]
struct Count(usize);

impl<'a> Dimension<'a, HeightSummary> for Count {
    fn zero(_: ()) -> Self {
        Self(0)
    }

    fn add_summary(&mut self, summary: &'a HeightSummary, _: ()) {
        self.0 = summary.max_index;
    }
}

#[derive(Clone, Copy, Debug)]
struct HeightTarget(f32);

impl SeekTarget<'_, HeightSummary, HeightSummary> for HeightTarget {
    fn cmp(&self, cursor_location: &HeightSummary, _: ()) -> Ordering {
        self.0
            .partial_cmp(&cursor_location.height)
            .unwrap_or(Ordering::Equal)
    }
}

#[derive(Clone, Copy, Debug)]
struct CountTarget(usize);

impl SeekTarget<'_, HeightSummary, HeightSummary> for CountTarget {
    fn cmp(&self, cursor_location: &HeightSummary, _: ()) -> Ordering {
        Ord::cmp(&self.0, &cursor_location.count)
    }
}

/// One document-wide layout registry. Blocks do not own focus or input
/// entities; all hit testing, selection geometry and caret geometry route
/// through this registry.
pub struct LayoutRegistry {
    pub(crate) visible: Vec<BlockLayout>,
    pub(crate) estimated_heights: HashMap<NodeId, f32>,
    pub(crate) cache: HashMap<NodeId, CachedBlockLayout>,
    pub(crate) lru: VecDeque<NodeId>,
    pub(crate) budget_bytes: usize,
    pub(crate) used_bytes: usize,
    peak_accounted_bytes: usize,
    pub(crate) first_visible: usize,
    pub(crate) last_visible: usize,
    document_order: HashMap<NodeId, usize>,
    document_order_revision: Option<u64>,
    /// Document-order numbering summary. Inline revisions retain these maps;
    /// only list structure changes trigger a suffix update.
    ordered_numbers: HashMap<NodeId, usize>,
    ordered_kinds: HashMap<NodeId, NumberingKind>,
    ordered_tree: SumTree<OrderItem>,
    ordered_checkpoints: Vec<NumberingCheckpoint>,
    ordered_summary_valid: bool,
    ordered_structure_revision: Option<u64>,
    ordered_number_scan_count: usize,
    #[cfg(test)]
    ordered_number_work_count: usize,
    #[cfg(test)]
    ordered_splice_work_count: usize,
    estimate_revisions: HashMap<NodeId, u64>,
    estimate_width: f32,
    estimate_document_revision: Option<u64>,
    height_tree: SumTree<HeightItem>,
    height_document_revision: Option<u64>,
    height_index_work: usize,
    shape_count: usize,
    layout_scan_count: usize,
}

impl Default for LayoutRegistry {
    fn default() -> Self {
        Self::new()
    }
}

impl LayoutRegistry {
    pub fn new() -> Self {
        Self {
            visible: Vec::new(),
            estimated_heights: HashMap::new(),
            cache: HashMap::new(),
            lru: VecDeque::new(),
            budget_bytes: LAYOUT_CACHE_BUDGET_BYTES,
            used_bytes: 0,
            peak_accounted_bytes: 0,
            first_visible: 0,
            last_visible: 0,
            document_order: HashMap::new(),
            document_order_revision: None,
            ordered_numbers: HashMap::new(),
            ordered_kinds: HashMap::new(),
            ordered_tree: SumTree::new(()),
            ordered_checkpoints: Vec::new(),
            ordered_summary_valid: false,
            ordered_structure_revision: None,
            ordered_number_scan_count: 0,
            #[cfg(test)]
            ordered_number_work_count: 0,
            #[cfg(test)]
            ordered_splice_work_count: 0,
            estimate_revisions: HashMap::new(),
            estimate_width: 0.0,
            estimate_document_revision: None,
            height_tree: SumTree::new(()),
            height_document_revision: None,
            height_index_work: 0,
            shape_count: 0,
            layout_scan_count: 0,
        }
    }

    pub fn with_budget(budget_bytes: usize) -> Self {
        let mut registry = Self::new();
        registry.budget_bytes = budget_bytes;
        registry
    }

    pub fn budget_bytes(&self) -> usize {
        self.budget_bytes
    }

    pub fn used_bytes(&self) -> usize {
        self.used_bytes
    }

    /// Highest retained-cache total observed after an admission/eviction
    /// boundary. This is intentionally post-admission accounting: transient
    /// allocator activity cannot make the published cache usage exceed the
    /// hard budget.
    pub fn peak_accounted_bytes(&self) -> usize {
        self.peak_accounted_bytes
    }

    pub fn cache_bytes(&self) -> usize {
        self.used_bytes
    }

    pub fn exact_cache_len(&self) -> usize {
        self.cache.len()
    }

    pub fn exact_cache_ids(&self) -> impl Iterator<Item = NodeId> + '_ {
        self.cache.keys().copied()
    }

    pub fn visible(&self) -> &[BlockLayout] {
        &self.visible
    }

    pub fn visible_range(&self) -> Range<usize> {
        self.first_visible..self.last_visible
    }

    /// Current measured/estimated document extent from the same height index
    /// used by viewport seeking.  The spike scroll surface uses this value
    /// instead of a block-count guess, so wrapped paragraphs remain reachable.
    pub fn total_height(&self) -> f32 {
        self.height_tree.summary().height.max(0.0)
    }

    pub fn first_visible(&self) -> usize {
        self.first_visible
    }

    pub fn last_visible(&self) -> usize {
        self.last_visible
    }

    pub fn shape_count(&self) -> usize {
        self.shape_count
    }

    /// Number of blocks used to rebuild the height index. A static scroll
    /// reuses the index and therefore does not rescan every block's text.
    pub fn layout_scan_count(&self) -> usize {
        self.layout_scan_count
    }

    pub fn height_index_work_count(&self) -> usize {
        self.height_index_work
    }

    pub(crate) fn ordered_number(&self, node_id: NodeId) -> Option<usize> {
        self.ordered_numbers.get(&node_id).copied()
    }

    #[cfg(test)]
    pub(crate) fn ordered_number_scan_count(&self) -> usize {
        self.ordered_number_scan_count
    }

    #[cfg(test)]
    pub(crate) fn ordered_number_work_count(&self) -> usize {
        self.ordered_number_work_count
    }

    #[cfg(test)]
    pub(crate) fn ordered_splice_work_count(&self) -> usize {
        self.ordered_splice_work_count
    }

    /// Build exact geometry for the viewport plus one viewport of prefetch on
    /// either side. Height lookup uses a prefix index; the full text is only
    /// inspected when a block revision or width changes.
    pub fn layout_document(
        &mut self,
        document: &Document,
        viewport_top: f32,
        viewport_height: f32,
        width: f32,
    ) {
        let width = width.max(1.0);
        self.ensure_ordered_numbers(document);
        self.refresh_estimates(document, width);
        self.ensure_height_index(document);
        self.rebuild_visible_window(document, viewport_top, viewport_height, width);
    }

    fn rebuild_visible_window(
        &mut self,
        document: &Document,
        viewport_top: f32,
        viewport_height: f32,
        width: f32,
    ) {
        let viewport_height = viewport_height.max(1.0);
        let viewport_bottom = viewport_top.max(0.0) + viewport_height;
        let prefetch_top = (viewport_top.max(0.0) - viewport_height * PREFETCH_VIEWPORTS).max(0.0);
        let prefetch_bottom = viewport_bottom + viewport_height * PREFETCH_VIEWPORTS;
        let first = self
            .height_index_at_or_before(prefetch_top)
            .min(document.block_count());
        let end = self
            .height_index_after(prefetch_bottom)
            .min(document.block_count());
        self.first_visible = first.min(end);
        self.last_visible = end.max(self.first_visible).min(document.block_count());

        if self.document_order_revision != Some(document.revision()) {
            self.document_order.clear();
            self.document_order.extend(
                document
                    .blocks()
                    .iter()
                    .enumerate()
                    .map(|(index, block)| (block.id, index)),
            );
            self.document_order_revision = Some(document.revision());
        }

        let allowed_ids: HashSet<NodeId> = document
            .blocks()
            .get(self.first_visible..self.last_visible)
            .unwrap_or_default()
            .iter()
            .map(|block| block.id)
            .collect();
        self.evict_outside(&allowed_ids);
        self.visible.clear();
        for index in self.first_visible..self.last_visible {
            let Some(block) = document.blocks().get(index) else {
                continue;
            };
            let y = self.height_prefix(index);
            let height = self.height_prefix(index + 1) - y;
            let (before, after, is_image) = block_points(block);
            let mut bounds = block_bounds(width, block);
            bounds.origin.y = px(y);
            bounds.size.height = px(height.max(1.0));
            let visual = block_visual_style(block, &TextStyle::default(), px(16.0));
            let layout = BlockLayout {
                node_id: block.id,
                bounds,
                text_inset: visual.text_inset,
                text_align: visual.text_align,
                text_lines: Vec::new(),
                before,
                after,
            };
            self.visible.push(layout.clone());
            self.register_geometry(block.revision, width, layout, is_image);
        }
        self.enforce_budget();
    }

    /// Shape only the already-capped viewport window with the donor's
    /// `shape_text`/`WrappedLine` path using the active window style.
    pub fn shape_visible_with_window(
        &mut self,
        document: &Document,
        viewport_top: f32,
        viewport_height: f32,
        width: f32,
        window: &mut gpui::Window,
    ) {
        let style = window.text_style();
        self.shape_visible_with_style(
            document,
            viewport_top,
            viewport_height,
            width,
            style,
            window,
        );
    }

    /// Production shaping entry point with an explicit, complete GPUI text
    /// style. Keeping this separate from `Window::with_text_style` also makes
    /// prepaint/layout callers able to pass the exact style they used for
    /// measurement without mutating window phase state.
    pub fn shape_visible_with_style(
        &mut self,
        document: &Document,
        viewport_top: f32,
        viewport_height: f32,
        width: f32,
        style: TextStyle,
        window: &mut gpui::Window,
    ) {
        let width = width.max(1.0);
        self.layout_document(document, viewport_top, viewport_height, width);
        // A measured height can change which blocks belong to the viewport
        // and prefetch window. Rebuild membership after each shaping pass so
        // blocks entering after expansion/contraction are shaped in the same
        // production call, rather than waiting for a later frame. A large
        // estimated block can initially push several prefetched blocks out,
        // while contraction can reveal the same blocks in waves. The cache
        // key makes each member shape at most once per style, so this loop
        // converges when the measured membership reaches a fixed point.
        loop {
            let visible_ids: Vec<NodeId> =
                self.visible.iter().map(|layout| layout.node_id).collect();
            let mut estimates_changed = false;
            for node_id in visible_ids {
                let Some(block) = document.block(node_id) else {
                    continue;
                };
                let visual = block_visual_style(block, &style, window.rem_size());
                let Some(text) = block.content.as_text() else {
                    self.update_cache_metadata(
                        node_id,
                        block.revision,
                        width,
                        ShapeKey::new(
                            block.revision,
                            width,
                            visual.font.clone(),
                            visual.font_size,
                            visual.line_height,
                        ),
                        visual.line_height,
                    );
                    continue;
                };
                let shape_key = ShapeKey::new(
                    block.revision,
                    width,
                    visual.font.clone(),
                    visual.font_size,
                    visual.line_height,
                );
                let cache_hit = self.cache.get(&node_id).is_some_and(|cached| {
                    cached.shape_key == shape_key && !cached.layout.text_lines.is_empty()
                });
                if cache_hit {
                    continue;
                }
                self.shape_count = self.shape_count.saturating_add(1);
                let shared_text = SharedString::from(text.to_owned());
                let runs = styled_text_runs(block, &shared_text, &visual.font);
                let decoration_line_run_counts = decoration_run_counts_per_hard_line(text, &runs);
                let decoration_run_count = decoration_line_run_counts.iter().sum();
                let shaped_background_run_count = runs
                    .iter()
                    .filter(|run| run.background_color.is_some())
                    .count();
                let block_width = block_bounds(width, block).size.width;
                let text_width = (f32::from(block_width) - f32::from(visual.text_inset)).max(1.0);
                let lines = window
                    .text_system()
                    .shape_text(
                        shared_text,
                        visual.font_size,
                        &runs,
                        Some(px(text_width)),
                        None,
                    )
                    .map(|lines| lines.into_vec())
                    .unwrap_or_default();
                let measured_height = lines
                    .iter()
                    .map(|line| line.size(visual.line_height).height)
                    .fold(px(0.0), |height, line_height| height + line_height)
                    .max(visual.line_height);
                if self
                    .estimated_heights
                    .get(&node_id)
                    .is_none_or(|height| (*height - f32::from(measured_height)).abs() > 0.01)
                {
                    let measured_height = f32::from(measured_height);
                    self.estimated_heights.insert(node_id, measured_height);
                    self.update_height_index(node_id, measured_height, block.revision);
                    estimates_changed = true;
                }
                let Some(geometry) = self.visible.iter().find(|layout| layout.node_id == node_id)
                else {
                    continue;
                };
                let mut layout = geometry.clone();
                layout.text_inset = visual.text_inset;
                layout.text_align = visual.text_align;
                layout.text_lines = lines;
                self.insert_shaped(
                    shape_key,
                    layout,
                    false,
                    visual.line_height,
                    0,
                    decoration_run_count,
                    decoration_line_run_counts,
                    shaped_background_run_count,
                );
            }
            if !estimates_changed {
                break;
            }
            self.rebuild_visible_window(document, viewport_top, viewport_height, width);
        }
        self.enforce_budget();
    }

    /// Register a measured block as the single exact-layout source. If the
    /// block is in the viewport, update its geometry there as well.
    pub fn register_exact(
        &mut self,
        revision: u64,
        width: f32,
        layout: BlockLayout,
        selection_geometry_bytes: usize,
        is_image: bool,
        line_height: Pixels,
    ) {
        let node_id = layout.node_id;
        if let Some(visible) = self.visible.iter_mut().find(|item| item.node_id == node_id) {
            *visible = BlockLayout {
                text_lines: Vec::new(),
                ..layout.clone()
            };
        } else {
            self.visible.push(BlockLayout {
                text_lines: Vec::new(),
                ..layout.clone()
            });
        }
        self.insert_shaped(
            ShapeKey::geometry(revision, width, line_height),
            layout,
            is_image,
            line_height,
            selection_geometry_bytes,
            0,
            SmallVec::new(),
            0,
        );
        self.enforce_budget();
    }

    pub(crate) fn clear_exact_cache(&mut self) {
        self.visible.clear();
        self.cache.clear();
        self.lru.clear();
        self.used_bytes = 0;
        self.estimate_document_revision = None;
        self.height_document_revision = None;
        self.document_order_revision = None;
        self.ordered_numbers.clear();
        self.ordered_kinds.clear();
        self.ordered_tree = SumTree::new(());
        self.ordered_checkpoints.clear();
        self.ordered_summary_valid = false;
        self.ordered_structure_revision = None;
    }

    /// Invalidate only the model nodes reported by the transaction layer.
    /// Structural edits also invalidate the height/order index, while
    /// unaffected shaped lines remain retained for the next viewport pass.
    pub(crate) fn invalidate_nodes_with_delta(
        &mut self,
        document: &Document,
        changed_nodes: &[NodeId],
        structural: bool,
        structural_splices: &[StructuralSplice],
        numbering_ranges: &[Range<usize>],
    ) {
        let invalidated: HashSet<NodeId> = changed_nodes.iter().copied().collect();
        self.visible
            .retain(|layout| !invalidated.contains(&layout.node_id));
        for node_id in changed_nodes {
            if let Some(entry) = self.cache.remove(node_id) {
                self.used_bytes = self.used_bytes.saturating_sub(entry.bytes);
            }
            self.lru.retain(|cached| cached != node_id);
        }
        if !changed_nodes.is_empty() {
            self.height_document_revision = None;
            self.document_order.clear();
            self.document_order_revision = None;
            if structural && self.ordered_summary_valid {
                self.update_ordered_structure(
                    document,
                    changed_nodes,
                    structural_splices,
                    numbering_ranges,
                );
            }
            self.ordered_structure_revision = Some(document.revision());
        }
        self.enforce_budget();
    }

    pub fn block_layout(&self, node_id: NodeId) -> Option<&BlockLayout> {
        self.cache.get(&node_id).map(|entry| &entry.layout)
    }

    pub fn is_image(&self, node_id: NodeId) -> bool {
        self.cache.get(&node_id).is_some_and(|entry| entry.is_image)
    }

    pub fn line_height(&self, node_id: NodeId) -> Option<Pixels> {
        self.cache.get(&node_id).map(|entry| entry.line_height)
    }

    /// Map a pointer to the document position represented by the visible
    /// layout. Images expose explicit before/after affinity.
    pub fn point_to_doc(&mut self, position: Point<Pixels>) -> Option<DocPoint> {
        let layout = self
            .visible
            .iter()
            .find(|layout| contains(layout.bounds, position))
            .or_else(|| {
                self.visible.iter().min_by(|left, right| {
                    vertical_distance(left.bounds, position.y)
                        .partial_cmp(&vertical_distance(right.bounds, position.y))
                        .unwrap_or(Ordering::Equal)
                })
            })?
            .clone();
        let Some(cached) = self.cache.get(&layout.node_id) else {
            // A shaped block may be rejected by the byte budget.  The
            // geometry still carries validated document endpoints, so use
            // their affinity instead of manufacturing an arbitrary byte
            // offset from pixels.
            return Some(
                if position.x <= layout.bounds.left() + layout.bounds.size.width / 2.0 {
                    layout.before
                } else {
                    layout.after
                },
            );
        };
        // Once the nearest block is selected, pointer coordinates outside its
        // vertical extent are block-edge hits.  Clamping y to the last row
        // alone would still use the pointer's x and turn a below-tail click
        // into the first character of that row.
        if position.y > layout.bounds.bottom() {
            return Some(layout.after);
        }
        if position.y < layout.bounds.top() {
            return Some(layout.before);
        }
        if cached.is_image {
            return Some(image_side(layout.bounds, position, layout.node_id));
        }
        let line_height = cached.line_height;
        let clamped_x = position
            .x
            .max(layout.bounds.left())
            .min(layout.bounds.right());
        let clamped_y = position
            .y
            .max(layout.bounds.top())
            .min(layout.bounds.bottom());
        let relative_y = (clamped_y - layout.bounds.top()).max(px(0.0));
        let mut line_top = px(0.0);
        let mut hard_start = 0;
        for (line_index, line) in cached.layout.text_lines.iter().enumerate() {
            let height = line.size(line_height).height;
            if relative_y < line_top + height || line_index + 1 == cached.layout.text_lines.len() {
                let local_y = (relative_y - line_top).max(px(0.0));
                let offsets = wrapped_row_offsets(line);
                let row = row_index_for_y(offsets.len().saturating_sub(1), local_y, line_height);
                let row_start = offsets.get(row).copied().unwrap_or(0);
                let row_end = offsets.get(row + 1).copied().unwrap_or(line.len());
                let row_origin = wrapped_row_origin_x(&cached.layout, line, row_start, row_end);
                let local_x = (clamped_x - row_origin).max(px(0.0));
                // Resolve x in the unwrapped line after selecting the visual
                // row from y.  `WrappedLine::closest_index_for_position`
                // combines its own row origin with x and therefore cannot
                // represent the alignment/slack-adjusted origin we use for a
                // wrapped row here.
                let row_start_x = line.unwrapped_layout.x_for_index(row_start);
                let local = line
                    .unwrapped_layout
                    .closest_index_for_x(row_start_x + local_x)
                    .max(row_start)
                    .min(row_end);
                let affinity = if local >= row_end && row_end < line.len() {
                    Affinity::Before
                } else {
                    Affinity::After
                };
                let end = hard_start + line.len();
                return Some(DocPoint::with_affinity(
                    layout.node_id,
                    hard_start
                        + snap_grapheme_offset(line.text.as_ref(), local.min(line.len()), affinity)
                            .min(line.len())
                            .min(end.saturating_sub(hard_start)),
                    affinity,
                ));
            }
            line_top += height;
            hard_start = hard_start.saturating_add(line.len() + 1);
        }
        Some(layout.after)
    }

    /// Move a caret by one visual row using the donor's wrapped-line
    /// geometry. `preferred_x` is retained by the caller across successive
    /// vertical moves; when absent, the current caret's measured x is used.
    pub fn visual_move(
        &self,
        caret: DocPoint,
        direction: isize,
        preferred_x: Option<Pixels>,
    ) -> Option<DocPoint> {
        let cached = self.cache.get(&caret.node_id)?;
        if cached.is_image || cached.layout.text_lines.is_empty() {
            return None;
        }
        let (line_index, _hard_start, offset_in_line) =
            line_position_for_offset(&cached.layout.text_lines, caret.utf8_offset);
        let line = cached.layout.text_lines.get(line_index)?;
        let line_height = cached.line_height;
        let current_position =
            line.position_for_index(offset_in_line.min(line.len()), line_height)?;
        let current_x = preferred_x
            .or_else(|| {
                self.caret_bounds_for_point(caret)
                    .map(|bounds| bounds.left())
            })
            .unwrap_or(current_position.x);
        let row_offsets = wrapped_row_offsets(line);
        let current_row = row_index_for_offset(&row_offsets, offset_in_line, caret.affinity);

        let mut rows_before = 0usize;
        for candidate in cached.layout.text_lines.iter().take(line_index) {
            rows_before =
                rows_before.saturating_add(wrapped_row_offsets(candidate).len().saturating_sub(1));
        }
        let current_visual_row = rows_before.saturating_add(current_row);
        let target_visual_row = if direction < 0 {
            current_visual_row.checked_sub(1)?
        } else {
            current_visual_row.checked_add(1)?
        };

        let mut row_cursor = 0usize;
        for (target_line_index, target_line) in cached.layout.text_lines.iter().enumerate() {
            let target_offsets = wrapped_row_offsets(target_line);
            let row_count = target_offsets.len().saturating_sub(1);
            if target_visual_row < row_cursor.saturating_add(row_count) {
                let target_row = target_visual_row.saturating_sub(row_cursor);
                let row_start = target_offsets.get(target_row).copied().unwrap_or(0);
                let row_end = target_offsets
                    .get(target_row + 1)
                    .copied()
                    .unwrap_or(target_line.len());
                let row_origin =
                    wrapped_row_origin_x(&cached.layout, target_line, row_start, row_end);
                let row_x = (preferred_x.unwrap_or(current_x) - row_origin).max(px(0.0));
                let row_start_x = target_line.unwrapped_layout.x_for_index(row_start);
                let target_x = row_start_x + row_x;
                // Resolve the target row against the unwrapped line.  The
                // wrapped helper adds its own row origin and would interpret
                // the alignment-adjusted x as a second local offset.
                let local = target_line
                    .unwrapped_layout
                    .closest_index_for_x(target_x)
                    .max(row_start)
                    .min(row_end);
                let affinity = if local >= row_end && row_end < target_line.len() {
                    Affinity::Before
                } else {
                    Affinity::After
                };
                let target_start = line_start_offset(&cached.layout.text_lines, target_line_index);
                let target_line_len = target_line.len();
                let snapped = target_start
                    + snap_grapheme_offset(
                        target_line.text.as_ref(),
                        local.min(target_line_len),
                        affinity,
                    )
                    .min(target_line_len);
                return Some(DocPoint::with_affinity(caret.node_id, snapped, affinity));
            }
            row_cursor = row_cursor.saturating_add(row_count);
        }
        None
    }

    /// Return the start/end byte offset of the visual row containing a caret.
    pub fn visual_line_boundary(&self, caret: DocPoint, end: bool) -> Option<DocPoint> {
        let cached = self.cache.get(&caret.node_id)?;
        if cached.is_image || cached.layout.text_lines.is_empty() {
            return None;
        }
        let (line_index, hard_start, offset_in_line) =
            line_position_for_offset(&cached.layout.text_lines, caret.utf8_offset);
        let line = cached.layout.text_lines.get(line_index)?;
        let offsets = wrapped_row_offsets(line);
        let row = row_index_for_offset(&offsets, offset_in_line, caret.affinity);
        let local = if end {
            offsets.get(row + 1).copied().unwrap_or(line.len())
        } else {
            offsets.get(row).copied().unwrap_or(0)
        };
        let affinity = if end {
            if local < line.len() {
                Affinity::Before
            } else {
                Affinity::After
            }
        } else {
            Affinity::After
        };
        let offset = hard_start + local.min(line.len());
        Some(DocPoint::with_affinity(
            caret.node_id,
            hard_start
                + snap_grapheme_offset(
                    line.text.as_ref(),
                    offset.saturating_sub(hard_start),
                    affinity,
                )
                .min(line.len()),
            affinity,
        ))
    }

    /// Resolve a vertical transfer entering a neighboring block. The edge
    /// row and measured x are selected from the same shaped layout used for
    /// in-block movement; callers use `last_row` for Up and the first row for
    /// Down.
    pub fn visual_edge_point(
        &self,
        node_id: NodeId,
        preferred_x: Option<Pixels>,
        last_row: bool,
    ) -> Option<DocPoint> {
        let cached = self.cache.get(&node_id)?;
        if cached.is_image {
            return Some(DocPoint::with_affinity(
                node_id,
                0,
                if last_row {
                    Affinity::After
                } else {
                    Affinity::Before
                },
            ));
        }
        let (line_index, line) = if last_row {
            let index = cached.layout.text_lines.len().checked_sub(1)?;
            (index, cached.layout.text_lines.get(index)?)
        } else {
            (0, cached.layout.text_lines.first()?)
        };
        let offsets = wrapped_row_offsets(line);
        let row = if last_row {
            offsets.len().saturating_sub(2)
        } else {
            0
        };
        let row_start = offsets.get(row).copied().unwrap_or(0);
        let row_end = offsets.get(row + 1).copied().unwrap_or(line.len());
        let row_origin = wrapped_row_origin_x(&cached.layout, line, row_start, row_end);
        let row_x = (preferred_x.unwrap_or(row_origin) - row_origin).max(px(0.0));
        let row_start_x = line.unwrapped_layout.x_for_index(row_start);
        let target_x = row_start_x + row_x;
        let local = line
            .unwrapped_layout
            .closest_index_for_x(target_x)
            .max(row_start)
            .min(row_end);
        let affinity = if local >= row_end && row_end < line.len() {
            Affinity::Before
        } else {
            Affinity::After
        };
        let start = line_start_offset(&cached.layout.text_lines, line_index);
        let offset = start
            + snap_grapheme_offset(line.text.as_ref(), local.min(line.len()), affinity)
                .min(line.len());
        Some(DocPoint::with_affinity(node_id, offset, affinity))
    }

    pub fn range_bounds(&self, node_id: NodeId, range: Range<usize>) -> Option<Bounds<Pixels>> {
        self.range_segment_bounds(node_id, range)
            .into_iter()
            .reduce(union_bounds)
            .or_else(|| self.caret_bounds(node_id, 0))
    }

    pub fn range_segment_bounds(
        &self,
        node_id: NodeId,
        range: Range<usize>,
    ) -> Vec<Bounds<Pixels>> {
        let Some(cached) = self.cache.get(&node_id) else {
            return Vec::new();
        };
        if cached.is_image || range.start >= range.end {
            return Vec::new();
        }
        let mut segments = Vec::with_capacity(selection_segment_capacity(
            &cached.layout.text_lines,
            &range,
        ));
        append_range_segment_bounds(&cached.layout, cached.line_height, range, &mut segments);
        segments
    }

    pub fn caret_bounds(&self, node_id: NodeId, offset: usize) -> Option<Bounds<Pixels>> {
        self.caret_bounds_for_point(DocPoint::with_affinity(node_id, offset, Affinity::After))
    }

    pub fn caret_x(&self, caret: DocPoint) -> Option<Pixels> {
        self.cache.get(&caret.node_id)?;
        self.caret_bounds_for_point(caret)
            .map(|bounds| bounds.left())
    }

    pub fn caret_bounds_for_point(&self, caret: DocPoint) -> Option<Bounds<Pixels>> {
        let cached = self.cache.get(&caret.node_id)?;
        let layout = &cached.layout;
        if cached.is_image {
            let height = self
                .adjacent_text_line_height(caret.node_id)
                .unwrap_or(cached.line_height);
            let y = layout.bounds.top() + (layout.bounds.size.height - height) / 2.0;
            let x = if caret.affinity == Affinity::Before {
                layout.bounds.left()
            } else {
                layout.bounds.right()
            };
            return Some(Bounds::new(point(x, y), size(px(CARET_WIDTH), height)));
        }
        let (line_index, _hard_start, offset_in_line) =
            line_position_for_offset(&layout.text_lines, caret.utf8_offset);
        let line_top = layout
            .text_lines
            .iter()
            .take(line_index)
            .map(|line| line.size(cached.line_height).height)
            .fold(px(0.0), |top, height| top + height);
        let line = layout.text_lines.get(line_index)?;
        let offset_in_line = snap_grapheme_offset(
            line.text.as_ref(),
            offset_in_line.min(line.len()),
            caret.affinity,
        )
        .min(line.len());
        let offsets = wrapped_row_offsets(line);
        let row = row_index_for_offset(&offsets, offset_in_line, caret.affinity);
        let row_start = offsets.get(row).copied().unwrap_or(0);
        let row_end = offsets.get(row + 1).copied().unwrap_or(line.len());
        let row_origin = wrapped_row_origin_x(layout, line, row_start, row_end);
        // `position_for_index` resolves a wrap seam to the preceding row for
        // an exact boundary.  The affinity-selected row is authoritative for
        // caret geometry, so derive the x/y from that row's unwrapped span
        // instead of reusing the seam-ambiguous wrapped position.
        let row_x = line.unwrapped_layout.x_for_index(offset_in_line)
            - line.unwrapped_layout.x_for_index(row_start);
        let row_y = cached.line_height * row as f32;
        Some(Bounds::new(
            point(row_origin + row_x, layout.bounds.top() + line_top + row_y),
            size(px(CARET_WIDTH), cached.line_height),
        ))
    }

    /// Return one rectangle per visible line/wrapped row. Offscreen endpoints
    /// are ordered using the document-wide node index, so an offscreen anchor
    /// does not suppress the visible portion of a selection.
    pub fn selection_rects(&self, selection: Selection) -> Vec<Bounds<Pixels>> {
        if selection.is_caret() {
            return Vec::new();
        }
        let (start, end) = if self.point_key(selection.anchor) <= self.point_key(selection.head) {
            (selection.anchor, selection.head)
        } else {
            (selection.head, selection.anchor)
        };
        let start_key = self.point_key(start);
        let end_key = self.point_key(end);
        let mut rect_capacity = 0usize;
        for layout in &self.visible {
            let before_key = self.point_key(layout.before);
            let after_key = self.point_key(layout.after);
            if self.is_image(layout.node_id) {
                if start_key <= self.point_key(layout.before)
                    && end_key >= self.point_key(layout.after)
                {
                    rect_capacity = rect_capacity.saturating_add(1);
                }
                continue;
            }
            if end_key <= before_key || start_key >= after_key {
                continue;
            }
            let start_offset = if start.node_id == layout.node_id {
                start.utf8_offset
            } else {
                0
            };
            let end_offset = if end.node_id == layout.node_id {
                end.utf8_offset
            } else {
                layout.after.utf8_offset
            };
            if let Some(cached) = self.cache.get(&layout.node_id) {
                rect_capacity = rect_capacity.saturating_add(selection_segment_capacity(
                    &cached.layout.text_lines,
                    &(start_offset..end_offset),
                ));
            }
        }
        let mut rects = Vec::with_capacity(rect_capacity);
        for layout in &self.visible {
            let before_key = self.point_key(layout.before);
            let after_key = self.point_key(layout.after);
            if self.is_image(layout.node_id) {
                if start_key <= self.point_key(layout.before)
                    && end_key >= self.point_key(layout.after)
                {
                    rects.push(layout.bounds);
                }
                continue;
            }
            if end_key <= before_key || start_key >= after_key {
                continue;
            }
            let start_offset = if start.node_id == layout.node_id {
                start.utf8_offset
            } else {
                0
            };
            let end_offset = if end.node_id == layout.node_id {
                end.utf8_offset
            } else {
                layout.after.utf8_offset
            };
            if let Some(cached) = self.cache.get(&layout.node_id) {
                append_range_segment_bounds(
                    &cached.layout,
                    cached.line_height,
                    start_offset..end_offset,
                    &mut rects,
                );
            }
        }
        rects
    }

    fn point_key(&self, point: DocPoint) -> (usize, usize, u8) {
        (
            self.document_order
                .get(&point.node_id)
                .copied()
                .unwrap_or(usize::MAX),
            point.utf8_offset,
            match point.affinity {
                Affinity::Before => 0,
                Affinity::After => 1,
            },
        )
    }

    fn adjacent_text_line_height(&self, node_id: NodeId) -> Option<Pixels> {
        let index = self.document_order.get(&node_id).copied()?;
        self.document_order
            .iter()
            .find_map(|(candidate, candidate_index)| {
                (candidate_index.abs_diff(index) == 1)
                    .then(|| {
                        self.cache
                            .get(candidate)
                            .and_then(|entry| (!entry.is_image).then_some(entry.line_height))
                    })
                    .flatten()
            })
    }

    fn refresh_estimates(&mut self, document: &Document, width: f32) {
        let width_changed = self.estimate_width.to_bits() != width.to_bits();
        if !width_changed && self.estimate_document_revision == Some(document.revision()) {
            return;
        }
        if width_changed {
            self.estimate_revisions.clear();
            self.height_document_revision = None;
        }
        self.estimate_width = width;
        let ids: HashSet<NodeId> = document.blocks().iter().map(|block| block.id).collect();
        self.estimated_heights.retain(|id, _| ids.contains(id));
        self.estimate_revisions.retain(|id, _| ids.contains(id));
        for block in document.blocks() {
            if !width_changed && self.estimate_revisions.get(&block.id) == Some(&block.revision) {
                continue;
            }
            self.layout_scan_count = self.layout_scan_count.saturating_add(1);
            let height = match &block.content {
                BlockContent::Text { text, .. } => estimate_text_height(text, width, &block.kind),
                BlockContent::Image {
                    natural_size: (image_width, image_height),
                    display_width,
                    ..
                } => display_width
                    .map(|display_width| {
                        (display_width as f32 * *image_height as f32 / *image_width as f32).max(1.0)
                    })
                    .unwrap_or(DEFAULT_IMAGE_HEIGHT),
                _ => DEFAULT_TEXT_HEIGHT,
            };
            self.estimated_heights.insert(block.id, height.max(1.0));
            self.estimate_revisions.insert(block.id, block.revision);
        }
        self.estimate_document_revision = Some(document.revision());
    }

    fn ensure_ordered_numbers(&mut self, document: &Document) {
        if self.ordered_summary_valid
            && self.ordered_structure_revision == Some(document.revision())
        {
            return;
        }
        self.rebuild_ordered_numbers(document);
    }

    fn rebuild_ordered_numbers(&mut self, document: &Document) {
        self.ordered_numbers.clear();
        self.ordered_kinds.clear();
        self.ordered_checkpoints.clear();
        self.ordered_tree = SumTree::from_iter(
            document
                .blocks()
                .iter()
                .map(|block| OrderItem { node_id: block.id }),
            (),
        );
        let mut cursor = NumberingCursor::default();
        for (index, block) in document.blocks().iter().enumerate() {
            if index % NUMBERING_CHECKPOINT_STRIDE == 0 {
                self.ordered_checkpoints.push(NumberingCheckpoint {
                    cursor: cursor.clone(),
                });
            }
            let kind = numbering_kind(&block.kind);
            let number = advance_numbering(kind, &mut cursor);
            self.ordered_kinds.insert(block.id, kind);
            if let Some(number) = number {
                self.ordered_numbers.insert(block.id, number);
            }
        }
        self.ordered_summary_valid = true;
        self.ordered_structure_revision = Some(document.revision());
        self.ordered_number_scan_count = self
            .ordered_number_scan_count
            .saturating_add(document.block_count());
        #[cfg(test)]
        {
            self.ordered_number_work_count = self
                .ordered_number_work_count
                .saturating_add(document.block_count());
        }
    }

    /// Apply explicit order replacements and separate kind/depth ranges. The
    /// order tree is edited with the same cursor slice/append path as GPUI's
    /// list state, so a tail splice does not copy the suffix into a new Vec.
    fn update_ordered_structure(
        &mut self,
        document: &Document,
        _changed_nodes: &[NodeId],
        structural_splices: &[StructuralSplice],
        numbering_ranges: &[Range<usize>],
    ) {
        self.repair_ordered_checkpoints(document.block_count());
        let mut first = usize::MAX;
        let mut last = 0usize;
        let mut affected_ranges = SmallVec::<[Range<usize>; 4]>::new();
        if !structural_splices.is_empty() {
            for splice in structural_splices {
                first = first.min(splice.start_index);
                last = last.max(
                    splice
                        .start_index
                        .saturating_add(splice.inserted.len())
                        .max(splice.start_index.saturating_add(splice.removed.len())),
                );
                // The splice itself is the explicit invalidation boundary.
                // Mark only the resulting replacement span (or its start for
                // a pure removal); do not infer structural change from the
                // final order tree, whose shifted IDs would force a suffix
                // scan even when numbering state is unchanged.
                let affected_len = splice.inserted.len().max(1);
                let affected_start = splice.start_index.min(document.block_count());
                let affected_end = affected_start
                    .saturating_add(affected_len)
                    .min(document.block_count());
                affected_ranges.push(affected_start..affected_end);
                self.splice_order_tree(splice);
                for node_id in &splice.removed {
                    self.ordered_kinds.remove(node_id);
                    self.ordered_numbers.remove(node_id);
                }
            }
        }
        for range in numbering_ranges {
            first = first.min(range.start);
            last = last.max(range.end.saturating_sub(1));
            affected_ranges.push(range.clone());
        }
        if first != usize::MAX {
            self.update_ordered_numbers(document, first, last, &affected_ranges);
        }
    }

    /// Keep the sparse checkpoint vector aligned with the final document
    /// length after an order splice. New slots start empty and are populated
    /// by the incremental scan when it reaches that boundary; removed tail
    /// slots are discarded so a later update cannot resume from an invalid
    /// cursor. Existing cursor values remain available for convergence checks
    /// until the affected wave rewrites them.
    fn repair_ordered_checkpoints(&mut self, block_count: usize) {
        let checkpoint_count = block_count.div_ceil(NUMBERING_CHECKPOINT_STRIDE);
        #[cfg(test)]
        let previous_count = self.ordered_checkpoints.len();
        self.ordered_checkpoints.truncate(checkpoint_count);
        while self.ordered_checkpoints.len() < checkpoint_count {
            self.ordered_checkpoints
                .push(NumberingCheckpoint::default());
        }
        #[cfg(test)]
        {
            // Count checkpoint repair as delta work rather than hiding it
            // behind removed/inserted identity counts.
            self.ordered_splice_work_count = self
                .ordered_splice_work_count
                .saturating_add(previous_count.abs_diff(checkpoint_count));
        }
    }

    fn splice_order_tree(&mut self, splice: &StructuralSplice) {
        let end = splice.start_index.saturating_add(splice.removed.len());
        debug_assert!(end <= self.ordered_tree.summary().count);
        let mut cursor = self.ordered_tree.cursor::<OrderCount>(());
        let mut new_tree = cursor.slice(&OrderCount(splice.start_index), Bias::Right);
        cursor.seek_forward(&OrderCount(end), Bias::Right);
        new_tree.extend(
            splice
                .inserted
                .iter()
                .copied()
                .map(|node_id| OrderItem { node_id }),
            (),
        );
        new_tree.append(cursor.suffix(), ());
        drop(cursor);
        self.ordered_tree = new_tree;
        #[cfg(test)]
        {
            // Count the bounded sequence operations as well as the identities
            // written. `slice`, `seek_forward`, and `suffix` each traverse
            // the balanced tree; the counter is deliberately not just
            // removed.len() + inserted.len().
            self.ordered_splice_work_count = self.ordered_splice_work_count.saturating_add(
                splice
                    .removed
                    .len()
                    .saturating_add(splice.inserted.len())
                    .saturating_add(3),
            );
        }
    }

    fn update_ordered_numbers(
        &mut self,
        document: &Document,
        start: usize,
        last_changed: usize,
        affected_ranges: &[Range<usize>],
    ) {
        let checkpoint_start =
            (start / NUMBERING_CHECKPOINT_STRIDE).saturating_mul(NUMBERING_CHECKPOINT_STRIDE);
        let mut cursor = self
            .ordered_checkpoints
            .get(checkpoint_start / NUMBERING_CHECKPOINT_STRIDE)
            .map(|checkpoint| checkpoint.cursor.clone())
            .unwrap_or_default();
        let mut processed = 0usize;
        let mut changed_since_checkpoint = false;
        let mut checkpoint_changed_before = false;
        for (index, block) in document.blocks().iter().enumerate().skip(checkpoint_start) {
            let kind = numbering_kind(&block.kind);
            let old_kind = self.ordered_kinds.get(&block.id).copied();
            let old_number = self.ordered_numbers.get(&block.id).copied();
            let number = advance_numbering(kind, &mut cursor);
            if let Some(number) = number {
                self.ordered_numbers.insert(block.id, number);
            } else {
                self.ordered_numbers.remove(&block.id);
            }
            self.ordered_kinds.insert(block.id, kind);
            processed = processed.saturating_add(1);

            if affected_ranges.iter().any(|range| range.contains(&index))
                || old_kind != Some(kind)
                || old_number != number
            {
                changed_since_checkpoint = true;
            }
            let at_checkpoint_boundary = (index + 1) % NUMBERING_CHECKPOINT_STRIDE == 0;
            if at_checkpoint_boundary {
                let checkpoint_index = (index + 1) / NUMBERING_CHECKPOINT_STRIDE;
                let stable = index >= last_changed
                    // A checkpoint containing the changed item cannot be the
                    // convergence point: the item immediately after it may
                    // have shifted even when the aggregate cursor happens to
                    // be equal. The first *clean* checkpoint after that one
                    // is allowed to converge when its cursor matches.
                    && checkpoint_changed_before
                    && !changed_since_checkpoint
                    && self
                        .ordered_checkpoints
                        .get(checkpoint_index)
                        .is_some_and(|checkpoint| checkpoint.cursor == cursor);
                if let Some(checkpoint) = self.ordered_checkpoints.get_mut(checkpoint_index) {
                    checkpoint.cursor = cursor.clone();
                }
                if stable {
                    break;
                }
                checkpoint_changed_before = changed_since_checkpoint;
                changed_since_checkpoint = false;
            }
        }
        self.ordered_summary_valid = true;
        self.ordered_number_scan_count = self.ordered_number_scan_count.saturating_add(processed);
        #[cfg(test)]
        {
            self.ordered_number_work_count =
                self.ordered_number_work_count.saturating_add(processed);
        }
    }

    fn ensure_height_index(&mut self, document: &Document) {
        if self.height_document_revision == Some(document.revision())
            && self.height_tree.summary().count == document.block_count()
        {
            return;
        }
        let items = document
            .blocks()
            .iter()
            .enumerate()
            .map(|(index, block)| HeightItem {
                index,
                node_id: block.id,
                revision: block.revision,
                height: self.height_for(block.id),
            });
        self.height_tree = SumTree::from_iter(items, ());
        self.height_index_work = self
            .height_index_work
            .saturating_add(document.block_count());
        self.height_document_revision = Some(document.revision());
    }

    fn update_height_index(&mut self, node_id: NodeId, height: f32, revision: u64) {
        let Some(&index) = self.document_order.get(&node_id) else {
            return;
        };
        self.height_tree.insert_or_replace(
            HeightItem {
                index,
                node_id,
                revision,
                height: height.max(1.0),
            },
            (),
        );
        // This is a logical localized tree replacement, not a document scan.
        // Keep the observable counter conservative and independent of the
        // implementation's internal node fan-out.
        self.height_index_work = self.height_index_work.saturating_add(1);
    }

    fn height_prefix(&self, index: usize) -> f32 {
        let (start, _, _) =
            self.height_tree
                .find::<HeightSummary, _>((), &CountTarget(index), Bias::Right);
        start.height
    }

    fn height_index_at_or_before(&self, target: f32) -> usize {
        let (start, _, _) = self.height_tree.find::<HeightSummary, _>(
            (),
            &HeightTarget(target.max(0.0)),
            Bias::Left,
        );
        start.count
    }

    fn height_index_after(&self, target: f32) -> usize {
        let target = target.max(0.0);
        let (start, _, item) =
            self.height_tree
                .find::<HeightSummary, _>((), &HeightTarget(target), Bias::Right);
        if item.is_none() {
            return start.count;
        }
        if (start.height - target).abs() <= f32::EPSILON {
            start.count
        } else {
            start.count.saturating_add(1)
        }
    }

    fn height_for(&self, node_id: NodeId) -> f32 {
        self.estimated_heights
            .get(&node_id)
            .copied()
            .unwrap_or(DEFAULT_TEXT_HEIGHT)
    }

    fn register_geometry(
        &mut self,
        revision: u64,
        width: f32,
        layout: BlockLayout,
        is_image: bool,
    ) {
        let node_id = layout.node_id;
        if let Some(cached) = self.cache.get_mut(&node_id)
            && cached.revision == revision
            && cached.width.to_bits() == width.to_bits()
        {
            cached.layout.bounds = layout.bounds;
            cached.layout.text_inset = layout.text_inset;
            cached.layout.text_align = layout.text_align;
            cached.layout.before = layout.before;
            cached.layout.after = layout.after;
            cached.is_image = is_image;
            self.touch(node_id);
            return;
        }
        self.insert_shaped(
            ShapeKey::geometry(revision, width, px(DEFAULT_TEXT_HEIGHT)),
            layout,
            is_image,
            px(DEFAULT_TEXT_HEIGHT),
            0,
            0,
            SmallVec::new(),
            0,
        );
    }

    fn insert_shaped(
        &mut self,
        shape_key: ShapeKey,
        layout: BlockLayout,
        is_image: bool,
        default_line_height: Pixels,
        requested_selection_geometry_bytes: usize,
        decoration_run_count: usize,
        decoration_line_run_counts: SmallVec<[usize; 4]>,
        shaped_background_run_count: usize,
    ) {
        let node_id = layout.node_id;
        let revision = shape_key.block_revision;
        let width = f32::from_bits(shape_key.width_bits);
        let line_height = px(f64::from_bits(shape_key.line_height_bits) as f32);
        if let Some(previous) = self.cache.remove(&node_id) {
            self.used_bytes = self.used_bytes.saturating_sub(previous.bytes);
        }
        self.lru.retain(|id| *id != node_id);
        let selection_geometry_bytes = selection_geometry_reserve(
            &layout,
            requested_selection_geometry_bytes.max(size_of::<Bounds<Pixels>>() * 2),
        );
        let retained_bytes = estimate_retained_cache_bytes(
            &layout,
            selection_geometry_bytes,
            &decoration_line_run_counts,
        );
        let snapshot_clone_bytes =
            estimate_snapshot_clone_bytes(&layout, &decoration_line_run_counts);
        let bytes = estimate_cache_bytes(
            &layout,
            selection_geometry_bytes,
            &decoration_line_run_counts,
        );
        if bytes > self.budget_bytes {
            return;
        }
        self.make_room_for(bytes);
        self.peak_accounted_bytes = self
            .peak_accounted_bytes
            .max(self.used_bytes.saturating_add(bytes));
        self.used_bytes = self.used_bytes.saturating_add(bytes);
        self.cache.insert(
            node_id,
            CachedBlockLayout {
                layout,
                bytes,
                retained_bytes,
                snapshot_clone_bytes,
                revision,
                width,
                selection_geometry_bytes,
                decoration_run_count,
                decoration_line_run_counts,
                shaped_background_run_count,
                is_image,
                line_height: if line_height == px(0.0) {
                    default_line_height
                } else {
                    line_height
                },
                shape_key,
            },
        );
        self.lru.push_back(node_id);
        self.enforce_budget();
    }

    fn update_cache_metadata(
        &mut self,
        node_id: NodeId,
        revision: u64,
        width: f32,
        shape_key: ShapeKey,
        line_height: Pixels,
    ) {
        if let Some(entry) = self.cache.get_mut(&node_id) {
            entry.revision = revision;
            entry.width = width;
            entry.shape_key = shape_key;
            entry.line_height = line_height;
            self.touch(node_id);
        }
    }

    fn touch(&mut self, node_id: NodeId) {
        self.lru.retain(|id| *id != node_id);
        self.lru.push_back(node_id);
    }

    fn reflow_visible(&mut self) {
        let positions = self
            .visible
            .iter()
            .map(|visible| {
                let index = self
                    .document_order
                    .get(&visible.node_id)
                    .copied()
                    .unwrap_or_default();
                (
                    visible.node_id,
                    self.height_prefix(index),
                    self.height_prefix(index + 1),
                )
            })
            .collect::<Vec<_>>();
        for (node_id, y, next_y) in positions {
            let height = next_y - y;
            let Some(visible) = self
                .visible
                .iter_mut()
                .find(|visible| visible.node_id == node_id)
            else {
                continue;
            };
            visible.bounds.origin.y = px(y);
            visible.bounds.size.height = px(height.max(1.0));
            if let Some(cached) = self.cache.get_mut(&node_id) {
                cached.layout.bounds = visible.bounds;
            }
        }
    }

    fn recompute_used_bytes(&mut self) {
        self.used_bytes = self.cache.values().map(|entry| entry.bytes).sum();
    }

    fn make_room_for(&mut self, incoming_bytes: usize) {
        while self.used_bytes.saturating_add(incoming_bytes) > self.budget_bytes {
            let Some(node_id) = self.lru.pop_front() else {
                break;
            };
            if let Some(entry) = self.cache.remove(&node_id) {
                self.used_bytes = self.used_bytes.saturating_sub(entry.bytes);
            }
        }
    }

    fn evict_outside(&mut self, allowed_ids: &HashSet<NodeId>) {
        let stale: Vec<NodeId> = self
            .cache
            .keys()
            .copied()
            .filter(|id| !allowed_ids.contains(id))
            .collect();
        for node_id in stale {
            if let Some(entry) = self.cache.remove(&node_id) {
                self.used_bytes = self.used_bytes.saturating_sub(entry.bytes);
            }
        }
        self.lru.retain(|id| allowed_ids.contains(id));
    }

    fn enforce_budget(&mut self) {
        while self.used_bytes > self.budget_bytes {
            let Some(node_id) = self.lru.pop_front() else {
                break;
            };
            if let Some(entry) = self.cache.remove(&node_id) {
                self.used_bytes = self.used_bytes.saturating_sub(entry.bytes);
            }
        }
        self.peak_accounted_bytes = self.peak_accounted_bytes.max(self.used_bytes);
    }
}

/// Build the ordered-list number summary in document order.  Non-ordered
/// children at a deeper list depth end only their own child sequence, while a
/// same-level/non-list block ends the parent sequence as well.  This keeps a
/// mixed `[ordered(0), bullet(1), ordered(0)]` run numbered `1., 2.`.
pub(crate) fn ordered_number_summary(document: &Document) -> HashMap<NodeId, usize> {
    let mut numbers = HashMap::new();
    let mut counters = NumberingCursor::default();
    for block in document.blocks() {
        if let Some(number) = advance_numbering(numbering_kind(&block.kind), &mut counters) {
            numbers.insert(block.id, number);
        }
    }
    numbers
}

fn advance_numbering(kind: NumberingKind, counters: &mut NumberingCursor) -> Option<usize> {
    fn reset_from(counters: &mut NumberingCursor, depth: u8) {
        counters
            .counters
            .retain(|(entry_depth, _)| *entry_depth < depth);
    }

    fn value_at(counters: &NumberingCursor, depth: u8) -> Option<usize> {
        counters
            .counters
            .iter()
            .find_map(|(entry_depth, value)| (*entry_depth == depth).then_some(*value))
    }

    match kind {
        NumberingKind::Ordered(depth) => {
            reset_from(counters, depth.saturating_add(1));
            if let Some((_, value)) = counters
                .counters
                .iter_mut()
                .find(|(entry_depth, _)| *entry_depth == depth)
            {
                *value = value.saturating_add(1).max(1);
                Some(*value)
            } else {
                let value = value_at(counters, depth)
                    .unwrap_or(0)
                    .saturating_add(1)
                    .max(1);
                counters.counters.push((depth, value));
                Some(value)
            }
        }
        NumberingKind::Bullet(depth) | NumberingKind::Check(depth) => {
            if depth == 0 {
                counters.counters.clear();
            } else {
                reset_from(counters, depth);
            }
            None
        }
        NumberingKind::Boundary => {
            counters.counters.clear();
            None
        }
    }
}

fn block_points(block: &super::model::Block) -> (DocPoint, DocPoint, bool) {
    match &block.content {
        BlockContent::Text { text, .. } => (
            DocPoint::with_affinity(block.id, 0, Affinity::Before),
            DocPoint::with_affinity(block.id, text.len(), Affinity::After),
            false,
        ),
        _ => (
            DocPoint::with_affinity(block.id, 0, Affinity::Before),
            DocPoint::with_affinity(block.id, 0, Affinity::After),
            matches!(block.kind, BlockKind::Image),
        ),
    }
}

fn estimate_text_height(text: &str, width: f32, kind: &BlockKind) -> f32 {
    let inset = list_depth(kind)
        .map(|_| LIST_MARKER_WIDTH)
        .unwrap_or_default();
    let chars_per_row = ((width - inset).max(1.0) / FALLBACK_GLYPH_WIDTH)
        .floor()
        .max(1.0) as usize;
    let line_height = match kind {
        BlockKind::Heading { level: 1 } => 40.0,
        BlockKind::Heading { level: 2 } => 34.0,
        BlockKind::Heading { level: 3 } => 29.0,
        BlockKind::Heading { .. } => 26.0,
        _ => DEFAULT_TEXT_HEIGHT,
    };
    text.split('\n')
        .map(|line| line.len().div_ceil(chars_per_row).max(1))
        .sum::<usize>() as f32
        * line_height
}

fn estimate_retained_cache_bytes(
    layout: &BlockLayout,
    selection_geometry_bytes: usize,
    decoration_line_run_counts: &[usize],
) -> usize {
    const ARC_ALLOCATION_OVERHEAD: usize = size_of::<usize>() * 2;
    let shaped_bytes = layout
        .text_lines
        .iter()
        .map(|line| {
            let runs = line
                .runs()
                .iter()
                .map(|run| {
                    size_of_val(run)
                        .saturating_add(run.glyphs.capacity() * size_of::<ShapedGlyph>())
                })
                .sum::<usize>();
            let line_layout = size_of::<gpui::LineLayout>()
                .saturating_add(line.runs().len() * size_of::<gpui::ShapedRun>());
            size_of::<WrappedLine>()
                .saturating_add(size_of::<WrappedLineLayout>())
                .saturating_add(line.text.len().saturating_add(ARC_ALLOCATION_OVERHEAD))
                .saturating_add(
                    line.wrap_boundaries()
                        .len()
                        .checked_next_power_of_two()
                        .unwrap_or(usize::MAX)
                        .saturating_mul(size_of::<WrapBoundary>())
                        .saturating_add(ARC_ALLOCATION_OVERHEAD),
                )
                .saturating_add(line_layout)
                .saturating_add(runs)
        })
        .sum::<usize>();
    // Each hard line owns an independent `SmallVec<[DecorationRun; 32]>` in
    // GPUI. A line with at most 32 runs has no heap spill; a line above that
    // threshold allocates its own next-power-of-two backing store. Do not
    // subtract the inline 32 slots from the spilled capacity.
    let decoration_spill = decoration_spill_bytes(decoration_line_run_counts);
    let shaped_bytes = shaped_bytes.saturating_add(decoration_spill);
    let line_count_spill = if decoration_line_run_counts.len() > 4 {
        decoration_line_run_counts
            .len()
            .checked_next_power_of_two()
            .unwrap_or(usize::MAX)
            .saturating_mul(size_of::<usize>())
    } else {
        0
    };
    size_of::<CachedBlockLayout>()
        .saturating_add(size_of::<BlockLayout>())
        .saturating_add(layout.text_lines.capacity() * size_of::<WrappedLine>())
        .saturating_add(shaped_bytes)
        .saturating_add(line_count_spill)
        .saturating_add(selection_geometry_bytes)
}

fn estimate_snapshot_clone_bytes(
    layout: &BlockLayout,
    decoration_line_run_counts: &[usize],
) -> usize {
    // The production snapshot clones this Vec and each WrappedLine. GPUI
    // shares its text/layout Arcs; only these owned buffers are counted.
    const ARC_ALLOCATION_OVERHEAD: usize = size_of::<usize>() * 2;
    size_of::<BlockLayout>()
        .saturating_add(size_of::<Vec<WrappedLine>>())
        .saturating_add(layout.text_lines.capacity() * size_of::<WrappedLine>())
        .saturating_add(decoration_spill_bytes(decoration_line_run_counts))
        .saturating_add(ARC_ALLOCATION_OVERHEAD)
}

fn estimate_cache_bytes(
    layout: &BlockLayout,
    selection_geometry_bytes: usize,
    decoration_line_run_counts: &[usize],
) -> usize {
    estimate_retained_cache_bytes(layout, selection_geometry_bytes, decoration_line_run_counts)
        .saturating_add(estimate_snapshot_clone_bytes(
            layout,
            decoration_line_run_counts,
        ))
}

fn selection_geometry_reserve(layout: &BlockLayout, requested: usize) -> usize {
    // `selection_rects` computes the final output capacity from the selected
    // wrapped rows and streams every row boundary into that one vector. No
    // joined text, hard-line range vector, or per-line row-offset vector is
    // live on this path; account only for the output allocation and its
    // fixed Vec header before admitting the shaped block.
    let visual_rows = layout
        .text_lines
        .iter()
        .map(|line| line.wrap_boundaries().len().saturating_add(1))
        .sum::<usize>();
    let output_storage = visual_rows
        .saturating_mul(size_of::<Bounds<Pixels>>())
        .saturating_add(size_of::<Vec<Bounds<Pixels>>>());
    requested.max(output_storage)
}

fn contains(bounds: Bounds<Pixels>, position: Point<Pixels>) -> bool {
    position.x >= bounds.left()
        && position.x <= bounds.right()
        && position.y >= bounds.top()
        && position.y <= bounds.bottom()
}

fn vertical_distance(bounds: Bounds<Pixels>, y: Pixels) -> f32 {
    if y < bounds.top() {
        f32::from(bounds.top() - y)
    } else if y > bounds.bottom() {
        f32::from(y - bounds.bottom())
    } else {
        0.0
    }
}

fn row_index_for_y(row_count: usize, y: Pixels, line_height: Pixels) -> usize {
    if row_count == 0 {
        return 0;
    }
    ((f32::from(y) / f32::from(line_height.max(px(1.0))))
        .floor()
        .max(0.0) as usize)
        .min(row_count.saturating_sub(1))
}

fn image_side(bounds: Bounds<Pixels>, position: Point<Pixels>, node_id: NodeId) -> DocPoint {
    let relative_y = position.y - bounds.top();
    let relative_x = position.x - bounds.left();
    let before = relative_y < bounds.size.height / 2.0
        || (relative_y == bounds.size.height / 2.0 && relative_x < bounds.size.width / 2.0);
    DocPoint::with_affinity(
        node_id,
        0,
        if before {
            Affinity::Before
        } else {
            Affinity::After
        },
    )
}

fn union_bounds(a: Bounds<Pixels>, b: Bounds<Pixels>) -> Bounds<Pixels> {
    Bounds::from_corners(
        point(a.left().min(b.left()), a.top().min(b.top())),
        point(a.right().max(b.right()), a.bottom().max(b.bottom())),
    )
}

fn line_start_offset(lines: &[WrappedLine], line_index: usize) -> usize {
    lines
        .iter()
        .take(line_index)
        .enumerate()
        .fold(0, |offset, (index, line)| {
            offset.saturating_add(line.len() + usize::from(index < line_index))
        })
}

/// Resolve a document byte offset directly against the borrowed wrapped
/// lines. `WrappedLine` values already preserve hard-line boundaries, so the
/// newline byte is accounted for while walking without joining all lines into
/// a temporary document string or allocating range metadata.
fn line_position_for_offset(lines: &[WrappedLine], offset: usize) -> (usize, usize, usize) {
    if lines.is_empty() {
        return (0, 0, 0);
    }
    let mut start: usize = 0;
    for (index, line) in lines.iter().enumerate() {
        let end = start.saturating_add(line.len());
        if offset <= end || index + 1 == lines.len() {
            return (index, start, offset.min(end).saturating_sub(start));
        }
        start = end.saturating_add(1);
    }
    let index = lines.len().saturating_sub(1);
    let line = &lines[index];
    (index, start, line.len())
}

/// Count the geometry rows touched by a document-byte range without creating
/// the temporary row-offset vector used by visual navigation. The result is
/// an upper bound for the output rectangle capacity, so the real selection
/// path can allocate its final `Bounds` vector once.
fn selection_segment_capacity(lines: &[WrappedLine], range: &Range<usize>) -> usize {
    if range.start >= range.end {
        return 0;
    }
    if lines.is_empty() {
        return 1;
    }
    let (start_line, _, _) = line_position_for_offset(lines, range.start);
    let (end_line, _, _) = line_position_for_offset(lines, range.end);
    (start_line..=end_line)
        .filter_map(|line_index| lines.get(line_index))
        .map(wrapped_row_count)
        .sum::<usize>()
        .max(1)
}

fn wrap_boundary_offset(line: &WrappedLine, wrap_index: usize) -> Option<usize> {
    let boundary = line.wrap_boundaries().get(wrap_index)?;
    let run = line.runs().get(boundary.run_ix)?;
    let glyph = run.glyphs.get(boundary.glyph_ix)?;
    Some(glyph.index)
}

fn wrapped_row_offsets(line: &WrappedLine) -> Vec<usize> {
    if line.len() == 0 {
        // An empty hard line still occupies one visual row. Keep two equal
        // endpoints so row-count arithmetic does not erase it.
        return vec![0, 0];
    }
    let mut offsets = Vec::with_capacity(line.wrap_boundaries().len() + 2);
    offsets.push(0);
    for index in 0..line.wrap_boundaries().len() {
        if let Some(offset) = wrap_boundary_offset(line, index) {
            offsets.push(offset.min(line.len()));
        }
    }
    offsets.push(line.len());
    offsets.dedup();
    offsets
}

fn wrapped_row_count(line: &WrappedLine) -> usize {
    let mut count = 0usize;
    for_each_wrapped_row(line, |_, _| count = count.saturating_add(1));
    count
}

/// Visit the same row intervals as `wrapped_row_offsets`, but keep all
/// offsets in locals. Selection/caret geometry uses this path because its
/// output vector is already owned by the caller and no per-line scratch is
/// necessary.
fn for_each_wrapped_row(line: &WrappedLine, mut visit: impl FnMut(usize, usize)) {
    if line.len() == 0 {
        visit(0, 0);
        return;
    }
    let mut row_start = 0usize;
    for index in 0..line.wrap_boundaries().len() {
        let Some(offset) = wrap_boundary_offset(line, index) else {
            continue;
        };
        let row_end = offset.min(line.len());
        if row_end > row_start {
            visit(row_start, row_end);
            row_start = row_end;
        }
    }
    if row_start < line.len() {
        visit(row_start, line.len());
    }
}

fn row_index_for_offset(offsets: &[usize], offset: usize, affinity: Affinity) -> usize {
    if offsets.len() < 2 {
        return 0;
    }
    for row in 0..offsets.len().saturating_sub(1) {
        let start = offsets[row];
        let end = offsets[row + 1];
        if offset < end
            || (offset == end && (row + 1 == offsets.len() - 1 || affinity == Affinity::Before))
        {
            return row;
        }
        if offset == start && row > 0 && affinity == Affinity::After {
            return row;
        }
    }
    offsets.len().saturating_sub(2)
}

fn append_range_segment_bounds(
    layout: &BlockLayout,
    line_height: Pixels,
    range: Range<usize>,
    segments: &mut Vec<Bounds<Pixels>>,
) {
    if range.start >= range.end {
        return;
    }
    if layout.text_lines.is_empty() {
        let text_left = layout.bounds.left() + layout.text_inset;
        let left = text_left + px(range.start as f32 * FALLBACK_GLYPH_WIDTH);
        let right = text_left + px(range.end as f32 * FALLBACK_GLYPH_WIDTH);
        segments.push(Bounds::from_corners(
            point(left, layout.bounds.top()),
            point(right.max(left + px(CARET_WIDTH)), layout.bounds.bottom()),
        ));
        return;
    }
    let (start_line, _, start_offset) = line_position_for_offset(&layout.text_lines, range.start);
    let (end_line, _, end_offset) = line_position_for_offset(&layout.text_lines, range.end);
    let mut line_top = layout.bounds.top()
        + layout
            .text_lines
            .iter()
            .take(start_line)
            .map(|line| line.size(line_height).height)
            .fold(px(0.0), |top, height| top + height);
    for line_index in start_line..=end_line {
        let Some(line) = layout.text_lines.get(line_index) else {
            continue;
        };
        let line_start = if line_index == start_line {
            start_offset
        } else {
            0
        };
        let line_end = if line_index == end_line {
            end_offset
        } else {
            line.len()
        };
        range_segment_bounds_for_line(
            line,
            line_top,
            layout,
            line_height,
            line_start,
            line_end,
            segments,
        );
        line_top += line.size(line_height).height;
    }
}

fn text_bounds(layout: &BlockLayout) -> Bounds<Pixels> {
    Bounds::new(
        point(
            layout.bounds.left() + layout.text_inset,
            layout.bounds.top(),
        ),
        size(
            (layout.bounds.size.width - layout.text_inset).max(px(1.0)),
            layout.bounds.size.height,
        ),
    )
}

fn wrapped_row_origin_x(
    layout: &BlockLayout,
    line: &WrappedLine,
    row_start: usize,
    row_end: usize,
) -> Pixels {
    let text_bounds = text_bounds(layout);
    let row_width =
        line.unwrapped_layout.x_for_index(row_end) - line.unwrapped_layout.x_for_index(row_start);
    let slack = (line.width() - row_width).max(px(0.0));
    let line_left = match layout.text_align {
        TextAlign::Left => text_bounds.left(),
        TextAlign::Center => {
            text_bounds.left() + (text_bounds.size.width - line.width()).max(px(0.0)) / 2.0
        }
        TextAlign::Right => {
            text_bounds.left() + (text_bounds.size.width - line.width()).max(px(0.0))
        }
    };
    match layout.text_align {
        TextAlign::Left => line_left,
        TextAlign::Center => line_left + slack / 2.0,
        TextAlign::Right => line_left + slack,
    }
}

fn range_segment_bounds_for_line(
    line: &WrappedLine,
    line_top: Pixels,
    layout: &BlockLayout,
    line_height: Pixels,
    start_offset: usize,
    end_offset: usize,
    segments: &mut Vec<Bounds<Pixels>>,
) {
    let mut row_index = 0usize;
    for_each_wrapped_row(line, |row_start, row_end| {
        let segment_start = start_offset.max(row_start).min(row_end);
        let segment_end = end_offset.min(row_end).max(row_start);
        if segment_start >= segment_end {
            row_index = row_index.saturating_add(1);
            return;
        }
        let row_start_x = line.unwrapped_layout.x_for_index(row_start);
        let start_x = line.unwrapped_layout.x_for_index(segment_start) - row_start_x;
        let end_x = line.unwrapped_layout.x_for_index(segment_end) - row_start_x;
        let row_top = line_top + line_height * row_index as f32;
        let row_origin = wrapped_row_origin_x(layout, line, row_start, row_end);
        segments.push(Bounds::from_corners(
            point(row_origin + start_x, row_top),
            point(
                row_origin + end_x.max(start_x + px(CARET_WIDTH)),
                row_top + line_height,
            ),
        ));
        row_index = row_index.saturating_add(1);
    });
}

fn snap_grapheme_offset(text: &str, offset: usize, affinity: Affinity) -> usize {
    let offset = offset.min(text.len());
    if text.is_empty() || text.is_char_boundary(offset) {
        let mut boundaries = text
            .grapheme_indices(true)
            .map(|(start, _)| start)
            .chain([text.len()]);
        return match affinity {
            Affinity::Before => boundaries
                .filter(|boundary| *boundary <= offset)
                .last()
                .unwrap_or(0),
            Affinity::After => boundaries
                .find(|boundary| *boundary >= offset)
                .unwrap_or(text.len()),
        };
    }
    text.grapheme_indices(true)
        .map(|(start, _)| start)
        .find(|start| *start > offset)
        .unwrap_or(text.len())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::native_editor::model::{Affinity, DocPoint, Document, Selection, TextAlignment};
    use crate::native_editor::transaction::Transaction;

    #[test]
    fn long_document_layout_is_bounded() {
        let document = Document::from_paragraphs((0..10_000).map(|index| format!("block-{index}")));
        let mut layout = LayoutRegistry::new();
        layout.layout_document(&document, 4_000.0, 480.0, 680.0);
        assert!(layout.visible_range().len() < document.block_count());
        assert!(layout.exact_cache_len() <= layout.visible_range().len());
        assert!(layout.used_bytes() <= LAYOUT_CACHE_BUDGET_BYTES);
    }

    #[test]
    fn layout_cache_evicts_before_16_mib() {
        let document = Document::from_paragraphs((0..10_000).map(|_| "x".repeat(4_096)));
        let mut layout = LayoutRegistry::new();
        layout.layout_document(&document, 0.0, 240.0, 680.0);
        assert!(layout.used_bytes() <= LAYOUT_CACHE_BUDGET_BYTES);
        assert!(layout.exact_cache_len() < document.block_count());
        let ids = layout.exact_cache_ids().collect::<HashSet<_>>();
        assert!(ids.len() <= layout.visible_range().len());
    }

    #[gpui::test]
    async fn wrapped_hit_test_uses_row_local_y_and_clamps_tail(cx: &mut gpui::TestAppContext) {
        let cx = cx.add_empty_window();
        let document = Document::from_paragraph("0123456789".repeat(12));
        let node = document.first_node_id().expect("text block");
        let mut layout = LayoutRegistry::new();
        cx.update(|window, _| {
            layout.shape_visible_with_window(&document, 0.0, 1_000.0, 32.0, window);
        });
        let block = layout.block_layout(node).expect("wrapped block");
        let block_bounds = block.bounds;
        let text_inset = block.text_inset;
        let line = block.text_lines.first().expect("hard line");
        let offsets = wrapped_row_offsets(line);
        assert!(offsets.len() >= 4, "fixture must have at least three rows");
        let line_height = layout.line_height(node).expect("line height");
        let x = block_bounds.left() + text_inset + px(1.0);

        for row in 1..offsets.len() - 1 {
            let position = point(x, block_bounds.top() + line_height * (row as f32 + 0.5));
            let hit = layout.point_to_doc(position).expect("wrapped row hit");
            assert_eq!(hit.node_id, node);
            assert_eq!(
                hit.utf8_offset, offsets[row],
                "row {row} left edge must map to its exact wrap boundary"
            );
        }

        let below = point(x, block_bounds.bottom() + line_height * 4.0);
        let tail = layout
            .point_to_doc(below)
            .expect("below-tail hit should clamp");
        assert_eq!(tail.node_id, node);
        assert_eq!(tail.utf8_offset, document.text_at_index(0).unwrap().len());
    }

    #[gpui::test]
    async fn wrapped_seam_caret_affinity_uses_the_selected_row(cx: &mut gpui::TestAppContext) {
        let cx = cx.add_empty_window();
        for alignment in [
            TextAlignment::Left,
            TextAlignment::Center,
            TextAlignment::Right,
        ] {
            let mut document = Document::from_paragraph("0123456789".repeat(16));
            let node = document.first_node_id().expect("text block");
            let text_len = document.text_at_index(0).unwrap().len();
            document
                .apply(Transaction::SetAlignment {
                    selection: Selection::new(
                        DocPoint::with_affinity(node, 0, Affinity::Before),
                        DocPoint::with_affinity(node, text_len, Affinity::After),
                    ),
                    alignment,
                })
                .expect("alignment transaction");
            let mut layout = LayoutRegistry::new();
            cx.update(|window, _| {
                layout.shape_visible_with_window(&document, 0.0, 1_000.0, 96.0, window);
            });
            let block = layout.block_layout(node).expect("wrapped block");
            let line = block.text_lines.first().expect("hard line");
            let offsets = wrapped_row_offsets(line);
            let seam = offsets.get(1).copied().expect("wrap seam");
            let line_height = layout.line_height(node).expect("line height");
            let before = layout
                .caret_bounds_for_point(DocPoint::with_affinity(node, seam, Affinity::Before))
                .expect("before seam caret");
            let after = layout
                .caret_bounds_for_point(DocPoint::with_affinity(node, seam, Affinity::After))
                .expect("after seam caret");
            assert_eq!(before.top(), block.bounds.top());
            assert_eq!(after.top(), block.bounds.top() + line_height);
            assert!(
                after.left() >= block.bounds.left() - px(0.5)
                    && after.left() <= block.bounds.right() + px(0.5),
                "after seam caret must remain in the aligned block"
            );
            let next = layout
                .visual_move(
                    DocPoint::with_affinity(node, seam, Affinity::After),
                    1,
                    None,
                )
                .expect("vertical move after seam");
            let next_bounds = layout.caret_bounds_for_point(next).expect("next-row caret");
            assert!(next_bounds.top() > after.top());
        }
    }
}
