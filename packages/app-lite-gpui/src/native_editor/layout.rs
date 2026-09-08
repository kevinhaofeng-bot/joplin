//! Viewport-bounded layout projection for the structured native document.
//!
//! The implementation keeps document data outside the exact-layout cache. A
//! cache entry owns only the visible block geometry, shaped lines, and the
//! conservative selection geometry accounting needed to enforce the 16 MiB
//! limit.

use std::collections::{HashMap, HashSet, VecDeque};
use std::mem::size_of;
use std::ops::Range;

use gpui::{Bounds, Pixels, Point, ShapedLine, point, px, size};

use super::model::{Affinity, BlockContent, BlockKind, DocPoint, Document, NodeId, Selection};

pub const LAYOUT_CACHE_BUDGET_BYTES: usize = 16 * 1024 * 1024;
const DEFAULT_TEXT_HEIGHT: f32 = 24.0;
const DEFAULT_IMAGE_HEIGHT: f32 = 180.0;
const PREFETCH_VIEWPORTS: f32 = 1.0;
const FALLBACK_GLYPH_WIDTH: f32 = 8.0;
const CARET_WIDTH: f32 = 1.0;

/// Exact geometry for one block currently in or near the viewport.
#[derive(Clone, Debug)]
pub struct BlockLayout {
    pub node_id: NodeId,
    pub bounds: Bounds<Pixels>,
    pub text_lines: Vec<ShapedLine>,
    pub before: DocPoint,
    pub after: DocPoint,
}

/// A bounded exact-layout cache entry. The byte count includes shaped runs and
/// a conservative allowance for selection rectangles.
#[derive(Clone, Debug)]
pub struct CachedBlockLayout {
    pub layout: BlockLayout,
    pub bytes: usize,
    pub revision: u64,
    pub width: f32,
    pub selection_geometry_bytes: usize,
    pub(crate) is_image: bool,
    pub(crate) line_height: Pixels,
}

/// One document-wide layout registry. Blocks do not own a focus handle or an
/// input entity; all hit testing is routed through this registry and EditorCore.
pub struct LayoutRegistry {
    pub(crate) visible: Vec<BlockLayout>,
    pub(crate) estimated_heights: HashMap<NodeId, f32>,
    pub(crate) cache: HashMap<NodeId, CachedBlockLayout>,
    pub(crate) lru: VecDeque<NodeId>,
    pub(crate) budget_bytes: usize,
    pub(crate) used_bytes: usize,
    pub(crate) first_visible: usize,
    pub(crate) last_visible: usize,
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
            first_visible: 0,
            last_visible: 0,
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

    /// Build exact geometry for the viewport plus one viewport of prefetch on
    /// either side. This is the test/headless path; shaped lines are populated
    /// by `shape_visible_with_window` when a real GPUI window is available.
    pub fn layout_document(
        &mut self,
        document: &Document,
        viewport_top: f32,
        viewport_height: f32,
        width: f32,
    ) {
        self.refresh_estimates(document);
        let viewport_height = viewport_height.max(1.0);
        let viewport_bottom = viewport_top.max(0.0) + viewport_height;
        let prefetch_top = (viewport_top.max(0.0) - viewport_height * PREFETCH_VIEWPORTS).max(0.0);
        let prefetch_bottom = viewport_bottom + viewport_height * PREFETCH_VIEWPORTS;

        let mut y = 0.0;
        let mut first = None;
        let mut end = 0;
        for (index, block) in document.blocks().iter().enumerate() {
            let height = self.height_for(block.id);
            let bottom = y + height;
            if bottom >= prefetch_top && first.is_none() {
                first = Some(index);
            }
            if y <= prefetch_bottom {
                end = index.saturating_add(1);
            }
            y = bottom;
        }

        let first = first.unwrap_or_else(|| document.block_count().saturating_sub(1));
        let end = end.max(first).min(document.block_count());
        self.first_visible = first.min(end);
        self.last_visible = end;
        self.visible.clear();

        let allowed_ids: HashSet<NodeId> = document
            .blocks()
            .get(self.first_visible..self.last_visible)
            .unwrap_or_default()
            .iter()
            .map(|block| block.id)
            .collect();
        self.evict_outside(&allowed_ids);

        y = document
            .blocks()
            .iter()
            .take(self.first_visible)
            .map(|block| self.height_for(block.id))
            .sum();
        for block in document
            .blocks()
            .get(self.first_visible..self.last_visible)
            .unwrap_or_default()
        {
            let height = self.height_for(block.id);
            let bounds = Bounds::new(point(px(0.0), px(y)), size(px(width.max(1.0)), px(height)));
            let (before, after, is_image) = block_points(block);
            let layout = BlockLayout {
                node_id: block.id,
                bounds,
                text_lines: Vec::new(),
                before,
                after,
            };
            self.visible.push(layout.clone());
            self.insert_or_touch(block.revision, width, layout, 0, is_image);
            y += height;
        }
        self.enforce_budget();
    }

    /// Shape only the already-capped viewport window using GPUI's text system.
    /// This adapts the donor's `shape_line` path while keeping the cache owner
    /// at editor level.
    pub fn shape_visible_with_window(
        &mut self,
        document: &Document,
        viewport_top: f32,
        viewport_height: f32,
        width: f32,
        window: &mut gpui::Window,
    ) {
        self.layout_document(document, viewport_top, viewport_height, width);
        let style = window.text_style();
        let font_size = style.font_size.to_pixels(window.rem_size());
        let line_height = window.line_height();
        let visible_ids: Vec<NodeId> = self.visible.iter().map(|layout| layout.node_id).collect();
        for node_id in visible_ids {
            let Some(block) = document.block(node_id) else {
                continue;
            };
            let Some(text) = block.content.as_text() else {
                continue;
            };
            let lines = text
                .split('\n')
                .map(|line| {
                    let text = gpui::SharedString::from(line.to_owned());
                    window.text_system().shape_line(
                        text.clone(),
                        font_size,
                        &[gpui::TextRun {
                            len: text.len(),
                            font: style.font(),
                            color: gpui::black(),
                            background_color: None,
                            underline: None,
                            strikethrough: None,
                        }],
                        None,
                    )
                })
                .collect::<Vec<_>>();
            let selection_geometry_bytes = lines.len().saturating_mul(size_of::<Bounds<Pixels>>());
            if let Some(layout) = self
                .visible
                .iter_mut()
                .find(|layout| layout.node_id == node_id)
            {
                layout.text_lines = lines;
                let layout = layout.clone();
                self.insert_or_touch(
                    block.revision,
                    width,
                    layout,
                    selection_geometry_bytes,
                    block.kind == BlockKind::Image,
                );
                if let Some(cached) = self.cache.get_mut(&node_id) {
                    cached.line_height = line_height;
                }
            }
        }
        self.enforce_budget();
    }

    /// Register a caller-shaped layout. This is useful for a renderer that
    /// already has shaped lines from GPUI's measured-layout callback.
    pub fn register_exact(
        &mut self,
        revision: u64,
        width: f32,
        layout: BlockLayout,
        selection_geometry_bytes: usize,
    ) {
        self.insert_or_touch(revision, width, layout, selection_geometry_bytes, false);
        self.enforce_budget();
    }

    pub(crate) fn clear_exact_cache(&mut self) {
        self.visible.clear();
        self.cache.clear();
        self.lru.clear();
        self.used_bytes = 0;
    }

    pub fn block_layout(&self, node_id: NodeId) -> Option<&BlockLayout> {
        self.cache.get(&node_id).map(|entry| &entry.layout)
    }

    pub fn is_image(&self, node_id: NodeId) -> bool {
        self.cache.get(&node_id).is_some_and(|entry| entry.is_image)
    }

    /// Return the measured line height captured with the exact block layout.
    /// Render and input geometry must use this value instead of rebuilding a
    /// fixed default from the document model.
    pub fn line_height(&self, node_id: NodeId) -> Option<Pixels> {
        self.cache.get(&node_id).map(|entry| entry.line_height)
    }

    /// Map a pointer to the document position represented by the visible
    /// layout. Images intentionally expose explicit before/after points.
    pub fn point_to_doc(&mut self, point: Point<Pixels>) -> Option<DocPoint> {
        let index = self
            .visible
            .iter()
            .position(|layout| contains(layout.bounds, point))?;
        let layout = self.visible[index].clone();
        let Some(cached) = self.cache.get(&layout.node_id) else {
            return Some(layout.after);
        };
        if cached.layout.text_lines.is_empty() {
            if layout.before.node_id == layout.node_id
                && layout.before.affinity == Affinity::Before
                && layout.after.affinity == Affinity::After
                && is_image_layout(&layout, cached)
            {
                return Some(image_side(layout.bounds, point, layout.node_id));
            }
            let text_len = layout.after.utf8_offset;
            let local_x = (point.x - layout.bounds.left()).max(px(0.0));
            let offset = approximate_text_offset(text_len, local_x);
            return Some(DocPoint::with_affinity(
                layout.node_id,
                offset,
                Affinity::After,
            ));
        }
        if layout.before.node_id == layout.node_id && is_image_layout(&layout, cached) {
            return Some(image_side(layout.bounds, point, layout.node_id));
        }
        let Some(text) = cached.layout.text_lines.first() else {
            return Some(layout.after);
        };
        let line_height = cached.line_height;
        let line_index = ((point.y - layout.bounds.top()).to_f64() as f32 / f32::from(line_height))
            .floor()
            .max(0.0) as usize;
        let line = cached.layout.text_lines.get(line_index).unwrap_or(text);
        let local_x = point.x - layout.bounds.left();
        let line_start = cached
            .layout
            .text_lines
            .iter()
            .take(line_index)
            .map(ShapedLine::len)
            .sum::<usize>();
        let offset = line_start.saturating_add(line.closest_index_for_x(local_x));
        Some(DocPoint::with_affinity(
            layout.node_id,
            offset.min(layout.after.utf8_offset),
            Affinity::After,
        ))
    }

    /// Adapt the donor `range_bounds` algorithm for unwrapped native lines.
    pub fn range_bounds(&self, node_id: NodeId, range: Range<usize>) -> Option<Bounds<Pixels>> {
        let layout = self.cache.get(&node_id)?.layout.clone();
        let line_height = self.cache.get(&node_id)?.line_height;
        if self.is_image(node_id) {
            return Some(layout.bounds);
        }
        if range.start >= range.end {
            return self.caret_bounds(node_id, range.start);
        }
        let text_len = layout.after.utf8_offset;
        let start = range.start.min(text_len);
        let end = range.end.min(text_len);
        if layout.text_lines.is_empty() {
            let left = layout.bounds.left() + px(start as f32 * FALLBACK_GLYPH_WIDTH);
            let right = layout.bounds.left() + px(end as f32 * FALLBACK_GLYPH_WIDTH);
            return Some(Bounds::from_corners(
                point(left, layout.bounds.top()),
                point(right.max(left + px(CARET_WIDTH)), layout.bounds.bottom()),
            ));
        }
        let mut union = None;
        let mut line_start = 0usize;
        for (line_index, line) in layout.text_lines.iter().enumerate() {
            let line_end = line_start.saturating_add(line.len());
            let segment_start = start.max(line_start);
            let segment_end = end.min(line_end);
            if segment_start < segment_end {
                let start_x = line.x_for_index(segment_start - line_start);
                let end_x = line.x_for_index(segment_end - line_start);
                let bounds = Bounds::from_corners(
                    point(
                        layout.bounds.left() + start_x,
                        layout.bounds.top() + line_height * line_index as f32,
                    ),
                    point(
                        layout.bounds.left() + end_x.max(start_x + px(CARET_WIDTH)),
                        layout.bounds.top() + line_height * (line_index as f32 + 1.0),
                    ),
                );
                union = Some(match union {
                    Some(previous) => union_bounds(previous, bounds),
                    None => bounds,
                });
            }
            line_start = line_end;
        }
        union.or_else(|| self.caret_bounds(node_id, start))
    }

    pub fn caret_bounds(&self, node_id: NodeId, offset: usize) -> Option<Bounds<Pixels>> {
        let layout = self.cache.get(&node_id)?.layout.clone();
        let line_height = self.cache.get(&node_id)?.line_height;
        if self.is_image(node_id) {
            let x = if offset == 0 {
                layout.bounds.left()
            } else {
                layout.bounds.right()
            };
            return Some(Bounds::new(
                point(x, layout.bounds.top()),
                size(px(CARET_WIDTH), layout.bounds.size.height),
            ));
        }
        let clamped = offset.min(layout.after.utf8_offset);
        if layout.text_lines.is_empty() {
            return Some(Bounds::new(
                point(
                    layout.bounds.left() + px(clamped as f32 * FALLBACK_GLYPH_WIDTH),
                    layout.bounds.top(),
                ),
                size(px(CARET_WIDTH), layout.bounds.size.height),
            ));
        }
        let mut line_start = 0usize;
        for (line_index, line) in layout.text_lines.iter().enumerate() {
            if clamped <= line_start.saturating_add(line.len()) {
                return Some(Bounds::new(
                    point(
                        layout.bounds.left() + line.x_for_index(clamped - line_start),
                        layout.bounds.top() + line_height * line_index as f32,
                    ),
                    size(px(CARET_WIDTH), line_height),
                ));
            }
            line_start = line_start.saturating_add(line.len());
        }
        Some(Bounds::new(
            point(layout.bounds.right(), layout.bounds.bottom() - line_height),
            size(px(CARET_WIDTH), line_height),
        ))
    }

    pub fn selection_rects(&self, selection: Selection) -> Vec<Bounds<Pixels>> {
        let mut order = HashMap::new();
        for (index, layout) in self.visible.iter().enumerate() {
            order.insert(layout.node_id, index);
        }
        let Some(anchor_index) = order.get(&selection.anchor.node_id).copied() else {
            return Vec::new();
        };
        let Some(head_index) = order.get(&selection.head.node_id).copied() else {
            return Vec::new();
        };
        let forward = (anchor_index, selection.anchor.utf8_offset)
            <= (head_index, selection.head.utf8_offset);
        let (start, end) = if forward {
            (selection.anchor, selection.head)
        } else {
            (selection.head, selection.anchor)
        };
        let start_index = order[&start.node_id];
        let end_index = order[&end.node_id];
        let mut rects = Vec::new();
        for index in start_index..=end_index {
            let layout = &self.visible[index];
            if self.is_image(layout.node_id) {
                rects.push(layout.bounds);
                continue;
            }
            let start_offset = if index == start_index {
                start.utf8_offset
            } else {
                0
            };
            let end_offset = if index == end_index {
                end.utf8_offset
            } else {
                layout.after.utf8_offset
            };
            if let Some(bounds) = self.range_bounds(layout.node_id, start_offset..end_offset) {
                rects.push(bounds);
            }
        }
        rects
    }

    fn refresh_estimates(&mut self, document: &Document) {
        let ids: HashSet<NodeId> = document.blocks().iter().map(|block| block.id).collect();
        self.estimated_heights.retain(|id, _| ids.contains(id));
        for block in document.blocks() {
            let height = match &block.content {
                BlockContent::Text { text, .. } => {
                    DEFAULT_TEXT_HEIGHT * text.matches('\n').count().saturating_add(1) as f32
                }
                BlockContent::Image {
                    natural_size: (width, height),
                    display_width,
                    ..
                } => display_width
                    .map(|display_width| {
                        (display_width as f32 * *height as f32 / *width as f32).max(1.0)
                    })
                    .unwrap_or(DEFAULT_IMAGE_HEIGHT),
                _ => DEFAULT_TEXT_HEIGHT,
            };
            self.estimated_heights.insert(block.id, height.max(1.0));
        }
    }

    fn height_for(&self, node_id: NodeId) -> f32 {
        self.estimated_heights
            .get(&node_id)
            .copied()
            .unwrap_or(DEFAULT_TEXT_HEIGHT)
    }

    fn insert_or_touch(
        &mut self,
        revision: u64,
        width: f32,
        layout: BlockLayout,
        selection_geometry_bytes: usize,
        is_image: bool,
    ) {
        let node_id = layout.node_id;
        if let Some(previous) = self.cache.remove(&node_id) {
            self.used_bytes = self.used_bytes.saturating_sub(previous.bytes);
        }
        self.lru.retain(|id| *id != node_id);
        let bytes = estimate_cache_bytes(&layout, selection_geometry_bytes);
        if bytes > self.budget_bytes {
            return;
        }
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
                line_height: px(DEFAULT_TEXT_HEIGHT),
            },
        );
        self.lru.push_back(node_id);
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

fn estimate_cache_bytes(layout: &BlockLayout, selection_geometry_bytes: usize) -> usize {
    let shaped_bytes = layout
        .text_lines
        .iter()
        .map(|line| {
            let runs = line
                .runs
                .iter()
                .map(|run| size_of_val(run) + run.glyphs.len() * size_of::<gpui::ShapedGlyph>())
                .sum::<usize>();
            size_of::<ShapedLine>()
                .saturating_add(line.text.len())
                .saturating_add(runs)
        })
        .sum::<usize>();
    size_of::<CachedBlockLayout>()
        .saturating_add(shaped_bytes)
        .saturating_add(selection_geometry_bytes.max(size_of::<Bounds<Pixels>>() * 2))
}

fn contains(bounds: Bounds<Pixels>, point: Point<Pixels>) -> bool {
    point.x >= bounds.left()
        && point.x <= bounds.right()
        && point.y >= bounds.top()
        && point.y <= bounds.bottom()
}

fn image_side(bounds: Bounds<Pixels>, point: Point<Pixels>, node_id: NodeId) -> DocPoint {
    let relative_y = point.y - bounds.top();
    let relative_x = point.x - bounds.left();
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

fn approximate_text_offset(text_len: usize, x: Pixels) -> usize {
    let target = (f32::from(x) / FALLBACK_GLYPH_WIDTH).round() as usize;
    target.min(text_len).checked_sub(0).unwrap_or(0)
}

fn union_bounds(a: Bounds<Pixels>, b: Bounds<Pixels>) -> Bounds<Pixels> {
    Bounds::from_corners(
        point(a.left().min(b.left()), a.top().min(b.top())),
        point(a.right().max(b.right()), a.bottom().max(b.bottom())),
    )
}

fn is_image_layout(_layout: &BlockLayout, cached: &CachedBlockLayout) -> bool {
    cached.is_image
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
        assert!(layout.used_bytes() <= 16 * 1024 * 1024);
        assert!(layout.exact_cache_len() < document.block_count());
        let ids = layout.exact_cache_ids().collect::<HashSet<_>>();
        assert!(ids.len() <= layout.visible_range().len());
    }
}
