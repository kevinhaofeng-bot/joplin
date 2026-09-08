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
    Bounds, Font, Pixels, Point, ShapedGlyph, SharedString, TextRun, TextStyle, WrapBoundary,
    WrappedLine, WrappedLineLayout, point, px, size,
};
use sum_tree::{Bias, ContextLessSummary, Dimension, Item, KeyedItem, SeekTarget, SumTree};
use unicode_segmentation::UnicodeSegmentation;

use super::model::{Affinity, BlockContent, BlockKind, DocPoint, Document, NodeId, Selection};

pub const LAYOUT_CACHE_BUDGET_BYTES: usize = 16 * 1024 * 1024;
const DEFAULT_TEXT_HEIGHT: f32 = 24.0;
const DEFAULT_IMAGE_HEIGHT: f32 = 180.0;
const PREFETCH_VIEWPORTS: f32 = 1.0;
const FALLBACK_GLYPH_WIDTH: f32 = 8.0;
const CARET_WIDTH: f32 = 1.0;

/// Exact geometry for one block currently in or near the viewport.
///
/// The field is intentionally named `text_lines` for the Task 4 public
/// shape, but the values are the donor's real `WrappedLine` objects so hard
/// newlines and soft wraps share one coordinate system.
#[derive(Clone, Debug)]
pub struct BlockLayout {
    pub node_id: NodeId,
    pub bounds: Bounds<Pixels>,
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
    pub revision: u64,
    pub width: f32,
    pub selection_geometry_bytes: usize,
    pub(crate) is_image: bool,
    pub(crate) line_height: Pixels,
    shape_key: ShapeKey,
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
            let layout = BlockLayout {
                node_id: block.id,
                bounds: Bounds::new(point(px(0.0), px(y)), size(px(width), px(height.max(1.0)))),
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
        let font_size = style.font_size.to_pixels(window.rem_size());
        let line_height = style.line_height_in_pixels(window.rem_size());
        let font = style.font();
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
                let Some(text) = block.content.as_text() else {
                    self.update_cache_metadata(
                        node_id,
                        block.revision,
                        width,
                        ShapeKey::new(block.revision, width, font.clone(), font_size, line_height),
                        line_height,
                    );
                    continue;
                };
                let shape_key =
                    ShapeKey::new(block.revision, width, font.clone(), font_size, line_height);
                let cache_hit = self.cache.get(&node_id).is_some_and(|cached| {
                    cached.shape_key == shape_key && !cached.layout.text_lines.is_empty()
                });
                if cache_hit {
                    continue;
                }
                self.shape_count = self.shape_count.saturating_add(1);
                let shared_text = SharedString::from(text.to_owned());
                let runs = [TextRun {
                    len: shared_text.len(),
                    font: font.clone(),
                    color: gpui::black(),
                    background_color: None,
                    underline: None,
                    strikethrough: None,
                }];
                let lines = window
                    .text_system()
                    .shape_text(shared_text, font_size, &runs, Some(px(width)), None)
                    .map(|lines| lines.into_vec())
                    .unwrap_or_default();
                let measured_height = lines
                    .iter()
                    .map(|line| line.size(line_height).height)
                    .fold(px(0.0), |height, line_height| height + line_height)
                    .max(line_height);
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
                layout.text_lines = lines;
                self.insert_shaped(shape_key, layout, false, line_height, 0);
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
    }

    /// Invalidate only the model nodes reported by the transaction layer.
    /// Structural edits also invalidate the height/order index, while
    /// unaffected shaped lines remain retained for the next viewport pass.
    pub(crate) fn invalidate_nodes(&mut self, changed_nodes: &[NodeId]) {
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
            .find(|layout| contains(layout.bounds, position))?
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
        if cached.is_image {
            return Some(image_side(layout.bounds, position, layout.node_id));
        }
        let line_height = cached.line_height;
        let relative_y = (position.y - layout.bounds.top()).max(px(0.0));
        let mut line_top = px(0.0);
        let mut hard_start = 0;
        for (line_index, line) in cached.layout.text_lines.iter().enumerate() {
            let height = line.size(line_height).height;
            if relative_y < line_top + height || line_index + 1 == cached.layout.text_lines.len() {
                let local_y = (relative_y - line_top).max(px(0.0));
                let local_x = (position.x - layout.bounds.left()).max(px(0.0));
                let local = line
                    .closest_index_for_position(point(local_x, local_y), line_height)
                    .unwrap_or_else(|offset| offset);
                let end = hard_start + line.len();
                return Some(DocPoint::with_affinity(
                    layout.node_id,
                    hard_start
                        + snap_grapheme_offset(
                            line.text.as_ref(),
                            local.min(line.len()),
                            Affinity::After,
                        )
                        .min(line.len())
                        .min(end.saturating_sub(hard_start)),
                    Affinity::After,
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
                let target_y = line_height * (target_row as f32 + 0.5);
                let x = preferred_x.unwrap_or(current_position.x);
                let local = target_line
                    .closest_index_for_position(point(x, target_y), line_height)
                    .unwrap_or_else(|offset| offset)
                    .min(target_line.len());
                let target_start = line_start_offset(&cached.layout.text_lines, target_line_index);
                let target_line_len = target_line.len();
                let snapped = target_start
                    + snap_grapheme_offset(
                        target_line.text.as_ref(),
                        local.min(target_line_len),
                        Affinity::After,
                    )
                    .min(target_line_len);
                return Some(DocPoint::with_affinity(
                    caret.node_id,
                    snapped,
                    Affinity::After,
                ));
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
        let offset = hard_start + local.min(line.len());
        Some(DocPoint::with_affinity(
            caret.node_id,
            hard_start
                + snap_grapheme_offset(
                    line.text.as_ref(),
                    offset.saturating_sub(hard_start),
                    if end {
                        Affinity::After
                    } else {
                        Affinity::Before
                    },
                )
                .min(line.len()),
            if end {
                Affinity::After
            } else {
                Affinity::Before
            },
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
        let y = cached.line_height * (row as f32 + 0.5);
        let local = line
            .closest_index_for_position(
                point(preferred_x.unwrap_or(px(0.0)), y),
                cached.line_height,
            )
            .unwrap_or_else(|offset| offset)
            .min(line.len());
        let start = line_start_offset(&cached.layout.text_lines, line_index);
        let offset = start
            + snap_grapheme_offset(line.text.as_ref(), local.min(line.len()), Affinity::After)
                .min(line.len());
        Some(DocPoint::with_affinity(node_id, offset, Affinity::After))
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
        let block_left = self.cache.get(&caret.node_id)?.layout.bounds.left();
        self.caret_bounds_for_point(caret)
            .map(|bounds| bounds.left() - block_left)
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
        let position = line.position_for_index(offset_in_line, cached.line_height)?;
        Some(Bounds::new(
            point(
                layout.bounds.left() + position.x,
                layout.bounds.top() + line_top + position.y,
            ),
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
                BlockContent::Text { text, .. } => estimate_text_height(text, width),
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
        );
    }

    fn insert_shaped(
        &mut self,
        shape_key: ShapeKey,
        layout: BlockLayout,
        is_image: bool,
        default_line_height: Pixels,
        requested_selection_geometry_bytes: usize,
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
        let bytes = estimate_cache_bytes(&layout, selection_geometry_bytes);
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
                revision,
                width,
                selection_geometry_bytes,
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

fn estimate_text_height(text: &str, width: f32) -> f32 {
    let chars_per_row = (width / FALLBACK_GLYPH_WIDTH).floor().max(1.0) as usize;
    text.split('\n')
        .map(|line| line.len().div_ceil(chars_per_row).max(1))
        .sum::<usize>() as f32
        * DEFAULT_TEXT_HEIGHT
}

fn estimate_cache_bytes(layout: &BlockLayout, selection_geometry_bytes: usize) -> usize {
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
                // `WrappedLine` stores several small vectors inline, but a
                // backend may spill their capacity. Reserve a conservative
                // per-line spill allowance instead of counting only lengths.
                .saturating_add(ARC_ALLOCATION_OVERHEAD * 2)
        })
        .sum::<usize>();
    size_of::<CachedBlockLayout>()
        .saturating_add(size_of::<BlockLayout>())
        .saturating_add(layout.text_lines.capacity() * size_of::<WrappedLine>())
        .saturating_add(shaped_bytes)
        .saturating_add(selection_geometry_bytes)
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
        let left = layout.bounds.left() + px(range.start as f32 * FALLBACK_GLYPH_WIDTH);
        let right = layout.bounds.left() + px(range.end as f32 * FALLBACK_GLYPH_WIDTH);
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
            layout.bounds.left(),
            line_height,
            line_start,
            line_end,
            segments,
        );
        line_top += line.size(line_height).height;
    }
}

fn range_segment_bounds_for_line(
    line: &WrappedLine,
    line_top: Pixels,
    line_left: Pixels,
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
        segments.push(Bounds::from_corners(
            point(line_left + start_x, row_top),
            point(
                line_left + end_x.max(start_x + px(CARET_WIDTH)),
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
    use crate::native_editor::model::Document;

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
}
