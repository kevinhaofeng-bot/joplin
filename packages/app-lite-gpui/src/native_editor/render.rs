//! GPUI rendering for the document-wide native editor surface.
//!
//! Rendering consumes the registry only. It does not create focus handles or
//! mutate document state. The paint order is deliberately explicit: surfaces,
//! selection geometry, glyphs/images, then the caret.

use gpui::{
    App, BorderStyle, Bounds, Corners, ElementInputHandler, Entity, FontWeight, ImageCache, Pixels,
    Resource, SharedString, TextRun, Window, WrappedLine, fill, outline, point, px, rgba,
};
#[cfg(test)]
use std::cell::RefCell;

use super::core::EditorCore;
use super::images::{BudgetedImageCache, proxy_max_edge_for_viewport};
use super::layout::{BlockLayout, ordered_number_summary};
use super::model::{BlockKind, Document, NodeId, Selection};

const PREFETCH_MAX_EDGE: u32 = 512;

pub(crate) fn image_proxy_max_edge_for_bounds(width: f32, height: f32, scale_factor: f32) -> u32 {
    proxy_max_edge_for_viewport(width.max(height), scale_factor)
}

fn natural_max_edge(natural_size: (u32, u32)) -> Option<u32> {
    (natural_size.0 > 0 && natural_size.1 > 0).then(|| natural_size.0.max(natural_size.1))
}

#[derive(Clone)]
struct RenderBlock {
    layout: BlockLayout,
    text_lines: Vec<WrappedLine>,
    is_image: bool,
    shaped_background_run_count: usize,
    line_height: Option<Pixels>,
    marker: Option<String>,
    image_resource: Option<Resource>,
    image_resource_id: Option<String>,
    image_natural_max_edge: Option<u32>,
    attachment: Option<AttachmentRenderInfo>,
}

/// The card contains presentation metadata only. Resource bytes stay in the
/// descriptor-safe core store and are materialized only if the user asks to
/// open the attachment.
#[derive(Clone)]
struct AttachmentRenderInfo {
    resource_id: String,
    filename: String,
    media_type: String,
    size: Option<u64>,
    available: bool,
}

#[derive(Clone)]
struct RenderSnapshot {
    blocks: Vec<RenderBlock>,
    find_highlights: Vec<FindHighlightGeometry>,
    selection_rects: Vec<Bounds<Pixels>>,
    selection: Selection,
    caret_bounds: Option<Bounds<Pixels>>,
}

/// Geometry is collected from the already-shaped visible block layouts. It is
/// never a second document traversal or a retained element per match.
#[derive(Clone, Debug)]
pub(crate) struct FindHighlightGeometry {
    pub(crate) bounds: Bounds<Pixels>,
    pub(crate) primary: bool,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct ImageResidency {
    visible_indices: Vec<usize>,
    prefetch_indices: Vec<usize>,
}

impl ImageResidency {
    fn resident_indices(&self) -> Vec<usize> {
        self.visible_indices
            .iter()
            .chain(self.prefetch_indices.iter())
            .copied()
            .collect()
    }

    fn is_resident(&self, index: usize) -> bool {
        self.visible_indices.contains(&index) || self.prefetch_indices.contains(&index)
    }

    fn is_visible(&self, index: usize) -> bool {
        self.visible_indices.contains(&index)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum TextPaintPass {
    Background,
    Glyphs,
}

#[cfg(test)]
#[derive(Default)]
struct TestRenderObservations {
    snapshot_clone_peak: usize,
    shaped_background_paints: usize,
    paint_entity_calls: usize,
    attachment_card_paints: usize,
    image_residency: Option<TestImageResidencyObservation>,
}

#[cfg(test)]
#[derive(Clone, Debug)]
pub(crate) struct TestImageResidencyObservation {
    pub(crate) content_mask: Bounds<Pixels>,
    pub(crate) image_bounds: Vec<Bounds<Pixels>>,
    pub(crate) visible_bounds: Vec<Bounds<Pixels>>,
    pub(crate) prefetch_bounds: Vec<Bounds<Pixels>>,
    pub(crate) visible_indices: Vec<usize>,
    pub(crate) prefetch_indices: Vec<usize>,
}

#[cfg(test)]
thread_local! {
    static TEST_RENDER_OBSERVATIONS: RefCell<TestRenderObservations> =
        RefCell::new(TestRenderObservations::default());
}

#[cfg(test)]
pub(crate) fn reset_test_render_observations() {
    TEST_RENDER_OBSERVATIONS.with(|observations| {
        *observations.borrow_mut() = TestRenderObservations::default();
    });
}

#[cfg(test)]
pub(crate) fn test_snapshot_clone_peak() -> usize {
    TEST_RENDER_OBSERVATIONS.with(|observations| observations.borrow().snapshot_clone_peak)
}

#[cfg(test)]
pub(crate) fn test_highlight_background_paints() -> usize {
    TEST_RENDER_OBSERVATIONS.with(|observations| observations.borrow().shaped_background_paints)
}

#[cfg(test)]
pub(crate) fn test_paint_entity_calls() -> usize {
    TEST_RENDER_OBSERVATIONS.with(|observations| observations.borrow().paint_entity_calls)
}

#[cfg(test)]
pub(crate) fn test_attachment_card_paints() -> usize {
    TEST_RENDER_OBSERVATIONS.with(|observations| observations.borrow().attachment_card_paints)
}

#[cfg(test)]
pub(crate) fn reset_test_image_residency_observation() {
    TEST_RENDER_OBSERVATIONS.with(|observations| {
        observations.borrow_mut().image_residency = None;
    });
}

#[cfg(test)]
pub(crate) fn test_image_residency_observation() -> Option<TestImageResidencyObservation> {
    TEST_RENDER_OBSERVATIONS.with(|observations| observations.borrow().image_residency.clone())
}

#[cfg(test)]
pub(crate) fn snapshot_allocation_bytes_for_test(editor: &EditorCore) -> usize {
    let measurement = super::tests::AllocationMeasurement::begin();
    let _snapshot = snapshot(editor);
    measurement.bytes()
}

#[cfg(test)]
pub(crate) fn wrapped_line_clone_allocation_bytes_for_test(editor: &EditorCore) -> usize {
    let measurement = super::tests::AllocationMeasurement::begin();
    for block in editor.layout.visible() {
        if let Some(layout) = editor.layout.block_layout(block.node_id) {
            let _wrapped_lines = layout.text_lines.clone();
        }
    }
    measurement.bytes()
}

#[cfg(test)]
fn observe_snapshot_clone(cached: &super::layout::CachedBlockLayout) {
    // This is the explicit owned allocation performed by the real
    // `WrappedLine`/Vec clone in snapshot(), including spilled decoration
    // runs. Shared text/layout/glyph Arcs are intentionally not counted.
    TEST_RENDER_OBSERVATIONS.with(|observations| {
        let mut observations = observations.borrow_mut();
        observations.snapshot_clone_peak = observations
            .snapshot_clone_peak
            .max(cached.snapshot_clone_bytes);
    });
}

#[cfg(test)]
fn observe_image_residency(
    snapshot: &RenderSnapshot,
    content_mask: Bounds<Pixels>,
    residency: &ImageResidency,
) {
    TEST_RENDER_OBSERVATIONS.with(|observations| {
        observations.borrow_mut().image_residency = Some(TestImageResidencyObservation {
            content_mask,
            image_bounds: snapshot
                .blocks
                .iter()
                .filter(|block| block.is_image)
                .map(|block| block.layout.bounds)
                .collect(),
            visible_bounds: residency
                .visible_indices
                .iter()
                .filter_map(|index| snapshot.blocks.get(*index))
                .map(|block| block.layout.bounds)
                .collect(),
            prefetch_bounds: residency
                .prefetch_indices
                .iter()
                .filter_map(|index| snapshot.blocks.get(*index))
                .map(|block| block.layout.bounds)
                .collect(),
            visible_indices: residency.visible_indices.clone(),
            prefetch_indices: residency.prefetch_indices.clone(),
        });
    });
}

const fn text_paint_passes() -> [TextPaintPass; 2] {
    [TextPaintPass::Background, TextPaintPass::Glyphs]
}

fn snapshot(editor: &EditorCore) -> RenderSnapshot {
    let layout = editor.layout();
    let blocks = layout
        .visible()
        .iter()
        .map(|block| {
            let model_block = editor.document().block(block.node_id);
            let kind = model_block
                .map(|block| block.kind.clone())
                .unwrap_or(BlockKind::Paragraph);
            let image_resource = model_block.and_then(|block| match &block.content {
                super::model::BlockContent::Image { resource_id, .. } => editor
                    .image_source_path(resource_id)
                    .map(|path| Resource::from(path.to_path_buf())),
                _ => None,
            });
            let image_resource_id = model_block.and_then(|block| match &block.content {
                super::model::BlockContent::Image { resource_id, .. } => Some(resource_id.clone()),
                _ => None,
            });
            let image_natural_max_edge = model_block.and_then(|block| match &block.content {
                super::model::BlockContent::Image { natural_size, .. } => {
                    natural_max_edge(*natural_size)
                }
                _ => None,
            });
            let attachment = model_block.and_then(|block| match &block.content {
                super::model::BlockContent::Attachment {
                    resource_id,
                    filename,
                    media_type,
                } => {
                    let metadata = editor.attachment_metadata(resource_id);
                    Some(AttachmentRenderInfo {
                        resource_id: resource_id.clone(),
                        filename: filename.clone(),
                        media_type: media_type.clone(),
                        size: metadata.map(|metadata| metadata.size()),
                        available: metadata.is_some_and(|metadata| metadata.is_available()),
                    })
                }
                _ => None,
            });
            let shaped_background_run_count = layout
                .cache
                .get(&block.node_id)
                .map_or(0, |cached| cached.shaped_background_run_count);
            #[cfg(test)]
            if let Some(cached) = layout.cache.get(&block.node_id) {
                observe_snapshot_clone(cached);
            }
            RenderBlock {
                layout: block.clone(),
                text_lines: layout
                    .block_layout(block.node_id)
                    .map(|cached| cached.text_lines.clone())
                    .unwrap_or_default(),
                is_image: layout.is_image(block.node_id),
                shaped_background_run_count,
                line_height: layout.line_height(block.node_id),
                marker: list_marker(&kind, layout.ordered_number(block.node_id)),
                image_resource,
                image_resource_id,
                image_natural_max_edge,
                attachment,
            }
        })
        .collect();
    let find_highlights = if editor.find_summary().total == 0 {
        Vec::new()
    } else {
        layout
            .visible()
            .iter()
            .filter(|block| editor.find_has_matches_for_node(block.node_id))
            .flat_map(|block| {
                layout
                    .find_highlight_text_range(block.node_id)
                    .into_iter()
                    .flat_map(|visible_utf8_range| {
                        editor
                            .find_matches_for_node_in_range(block.node_id, visible_utf8_range)
                            .flat_map(|(found, primary)| {
                                layout
                                    .find_range_segment_bounds(
                                        found.node_id,
                                        found.utf8_range.clone(),
                                    )
                                    .into_iter()
                                    .filter(|bounds| {
                                        layout.intersects_find_highlight_viewport(*bounds)
                                    })
                                    .map(move |bounds| FindHighlightGeometry { bounds, primary })
                            })
                    })
            })
            .collect()
    };
    RenderSnapshot {
        blocks,
        find_highlights,
        selection_rects: layout.selection_rects(editor.selection()),
        selection: editor.selection(),
        caret_bounds: editor
            .selection()
            .is_caret()
            .then(|| layout.caret_bounds_for_point(editor.selection().head))
            .flatten(),
    }
}

#[cfg(test)]
pub(crate) fn find_highlights_for_test(editor: &EditorCore) -> Vec<FindHighlightGeometry> {
    snapshot(editor).find_highlights
}

fn bounds_intersect(left: Bounds<Pixels>, right: Bounds<Pixels>) -> bool {
    left.left() < right.right()
        && left.right() > right.left()
        && left.top() < right.bottom()
        && left.bottom() > right.top()
}

fn classify_image_residency(
    blocks: &[RenderBlock],
    content_mask: Bounds<Pixels>,
) -> ImageResidency {
    let mut residency = ImageResidency::default();
    let mut preceding_image = None;
    let mut following_image = None;

    for (index, block) in blocks.iter().enumerate() {
        if !block.is_image {
            continue;
        }
        if bounds_intersect(block.layout.bounds, content_mask) {
            residency.visible_indices.push(index);
            continue;
        }
        if block.layout.bounds.bottom() <= content_mask.top() {
            preceding_image = Some(index);
        } else if following_image.is_none() && block.layout.bounds.top() >= content_mask.bottom() {
            following_image = Some(index);
        }
    }

    if let Some(index) = preceding_image {
        residency.prefetch_indices.push(index);
    }
    if let Some(index) = following_image {
        residency.prefetch_indices.push(index);
    }
    residency
}

fn image_request_edge(block: &RenderBlock, scale_factor: f32, is_visible: bool) -> u32 {
    let edge = image_proxy_max_edge_for_bounds(
        f32::from(block.layout.bounds.size.width),
        f32::from(block.layout.bounds.size.height),
        scale_factor,
    );
    if is_visible {
        edge
    } else {
        edge.min(PREFETCH_MAX_EDGE)
    }
}

pub fn paint(editor: &EditorCore, window: &mut Window, cx: &mut App) -> gpui::Result<()> {
    let snapshot = snapshot(editor);
    let content_mask = window.content_mask().bounds;
    paint_snapshot(&snapshot, content_mask, None, None, window, cx)
}

fn paint_snapshot(
    snapshot: &RenderSnapshot,
    content_mask: Bounds<Pixels>,
    image_cache: Option<Entity<BudgetedImageCache>>,
    editor: Option<Entity<EditorCore>>,
    window: &mut Window,
    cx: &mut App,
) -> gpui::Result<()> {
    let residency = classify_image_residency(snapshot.blocks.as_slice(), content_mask);
    #[cfg(test)]
    observe_image_residency(snapshot, content_mask, &residency);
    // An unresolved persisted image remains an honest stable placeholder in
    // this frame. Rendering is the only layer with viewport residency, so it
    // asks the retained session for just visible/prefetch resource IDs instead
    // of letting note preparation stream every original blob on the GPUI
    // thread.
    if let Some(editor) = editor.as_ref() {
        let resident_ids = residency
            .resident_indices()
            .into_iter()
            .filter_map(|index| snapshot.blocks[index].image_resource_id.clone())
            .collect::<Vec<_>>();
        let _ = editor.update(cx, |editor, editor_cx| {
            if editor.request_image_hydration(resident_ids) {
                editor_cx.notify();
            }
        });
    }
    if let Some(cache) = image_cache.as_ref() {
        let resident_indices = residency.resident_indices();
        let resident_resources = resident_indices
            .iter()
            .filter_map(|index| snapshot.blocks[*index].image_resource.as_ref())
            .collect::<Vec<_>>();
        cache.update(cx, |cache, cache_cx| {
            cache.set_visible_resources(resident_resources.iter().copied());
            for index in resident_indices.iter().copied() {
                let block = &snapshot.blocks[index];
                if !block.is_image {
                    continue;
                }
                if let Some(resource) = block.image_resource.as_ref() {
                    let max_edge = image_request_edge(
                        block,
                        window.scale_factor(),
                        residency.is_visible(index),
                    );
                    cache.request_edge_with_natural_max(
                        resource,
                        max_edge,
                        block.image_natural_max_edge,
                    );
                }
            }
            cache.evict_offscreen(window, cache_cx);
        });
    }
    // 1. Block surfaces.
    for block in &snapshot.blocks {
        let mut quad = fill(block.layout.bounds, rgba(0x00000000));
        if block.is_image {
            quad.corner_radii = Corners::all(px(6.0));
        } else if block.attachment.is_some() {
            quad.corner_radii = Corners::all(px(7.0));
            quad.background = if block
                .attachment
                .as_ref()
                .is_some_and(|attachment| attachment.available)
            {
                rgba(0xf1f7f3ff).into()
            } else {
                rgba(0xfff3f1ff).into()
            };
        }
        window.paint_quad(quad);
    }

    // 2. Find rectangles stay behind selection and glyphs. The primary is a
    // more saturated accent without changing the editor's actual selection.
    for highlight in &snapshot.find_highlights {
        let color = if highlight.primary {
            rgba(0xf59e0b80)
        } else {
            rgba(0xfde68a99)
        };
        window.paint_quad(fill(highlight.bounds, color));
    }

    // 3. Selection rectangles. Image atoms use the same geometry but with a
    // rounded outline-like highlight rather than a text rectangle.
    for rect in &snapshot.selection_rects {
        let is_image = snapshot
            .blocks
            .iter()
            .any(|block| block.is_image && block.layout.bounds == *rect);
        if is_image {
            let mut quad = outline(
                image_selection_outline(*rect),
                rgba(0x4f8cffdd),
                BorderStyle::default(),
            );
            quad.corner_radii = Corners::all(px(6.0));
            window.paint_quad(quad);
        } else {
            window.paint_quad(fill(*rect, rgba(0x4f8cff55)));
        }
    }

    // 4. Glyphs/images.
    for (index, block) in snapshot.blocks.iter().enumerate() {
        if block.is_image {
            let image = residency
                .is_resident(index)
                .then(|| block.image_resource.as_ref())
                .flatten()
                .and_then(|resource| {
                    image_cache.as_ref().and_then(|cache| {
                        cache.update(cx, |cache, cx| cache.load(resource, window, cx))
                    })
                });
            if let Some(Ok(image)) = image {
                if let (Some(editor), Some(resource_id)) =
                    (editor.as_ref(), block.image_resource_id.as_deref())
                {
                    let _ = editor.update(cx, |editor, editor_cx| {
                        if editor.mark_image_loaded(resource_id) {
                            editor_cx.notify();
                        }
                    });
                }
                window.paint_image(block.layout.bounds, Corners::all(px(6.0)), image, 0, false)?;
            } else {
                if let (Some(editor), Some(resource_id), Some(Err(_))) = (
                    editor.as_ref(),
                    block.image_resource_id.as_deref(),
                    image.as_ref(),
                ) {
                    let _ = editor.update(cx, |editor, editor_cx| {
                        if editor.mark_image_failed(resource_id) {
                            editor_cx.notify();
                        }
                    });
                }
                let mut quad = fill(block.layout.bounds, rgba(0x9aa4b233));
                quad.corner_radii = Corners::all(px(6.0));
                window.paint_quad(quad);
            }
            continue;
        }
        if let Some(attachment) = block.attachment.as_ref() {
            paint_attachment_card(attachment, block.layout.bounds, window, cx)?;
            #[cfg(test)]
            TEST_RENDER_OBSERVATIONS.with(|observations| {
                observations.borrow_mut().attachment_card_paints += 1;
            });
            continue;
        }
        let line_height = block.line_height.unwrap_or_else(|| window.line_height());
        let text_bounds = Bounds::new(
            point(
                block.layout.bounds.left() + block.layout.text_inset,
                block.layout.bounds.top(),
            ),
            gpui::size(
                (block.layout.bounds.size.width - block.layout.text_inset).max(px(1.0)),
                block.layout.bounds.size.height,
            ),
        );
        let mut line_top = block.layout.bounds.top();
        for line in &block.text_lines {
            let origin = point(text_bounds.left(), line_top);
            for pass in text_paint_passes() {
                match pass {
                    TextPaintPass::Background => {
                        let result = line.paint_background(
                            origin,
                            line_height,
                            block.layout.text_align,
                            Some(text_bounds),
                            window,
                            cx,
                        );
                        #[cfg(test)]
                        if block.shaped_background_run_count > 0 && result.is_ok() {
                            #[cfg(test)]
                            TEST_RENDER_OBSERVATIONS.with(|observations| {
                                observations.borrow_mut().shaped_background_paints += 1;
                            });
                        }
                        result?;
                    }
                    TextPaintPass::Glyphs => line.paint(
                        origin,
                        line_height,
                        block.layout.text_align,
                        Some(text_bounds),
                        window,
                        cx,
                    )?,
                }
            }
            line_top += line.size(line_height).height;
        }
        if let Some(marker) = block.marker.as_deref() {
            let style = window.text_style();
            let marker_text: SharedString = marker.to_owned().into();
            let marker_line = window.text_system().shape_line(
                marker_text.clone(),
                style.font_size.to_pixels(window.rem_size()),
                &[TextRun {
                    len: marker_text.len(),
                    font: style.font(),
                    color: rgba(0x536174ff).into(),
                    background_color: None,
                    underline: None,
                    strikethrough: None,
                }],
                None,
            );
            marker_line.paint(
                point(block.layout.bounds.left(), block.layout.bounds.top()),
                line_height,
                window,
                cx,
            )?;
        }
    }

    // 4. Caret. Image before/after positions use the same vertical caret
    // geometry as paragraph text through LayoutRegistry::caret_bounds.
    if snapshot.selection.is_caret() {
        if let Some(bounds) = snapshot.caret_bounds {
            window.paint_quad(fill(bounds, rgba(0x2167dfff)));
        }
    }
    Ok(())
}

fn paint_attachment_card(
    attachment: &AttachmentRenderInfo,
    bounds: Bounds<Pixels>,
    window: &mut Window,
    cx: &mut App,
) -> gpui::Result<()> {
    let style = window.text_style();
    let mut title_font = style.font();
    title_font.weight = FontWeight::SEMIBOLD;
    let title: SharedString = attachment.filename.clone().into();
    let title_line = window.text_system().shape_line(
        title.clone(),
        px(14.0),
        &[TextRun {
            len: title.len(),
            font: title_font,
            color: rgba(0x25342bff).into(),
            background_color: None,
            underline: None,
            strikethrough: None,
        }],
        None,
    );
    title_line.paint(
        point(bounds.left() + px(12.0), bounds.top() + px(13.0)),
        px(20.0),
        window,
        cx,
    )?;

    let availability = if attachment.available {
        "已加载 · 双击打开"
    } else {
        "资源不可用"
    };
    let detail: SharedString = format!(
        "{} · {} · {}",
        attachment.media_type,
        attachment
            .size
            .map(format_attachment_size)
            .unwrap_or_else(|| "大小未知".to_owned()),
        availability,
    )
    .into();
    let detail_line = window.text_system().shape_line(
        detail.clone(),
        px(12.0),
        &[TextRun {
            len: detail.len(),
            font: style.font(),
            color: if attachment.available {
                rgba(0x637368ff).into()
            } else {
                rgba(0x9d4437ff).into()
            },
            background_color: None,
            underline: None,
            strikethrough: None,
        }],
        None,
    );
    detail_line.paint(
        point(bounds.left() + px(12.0), bounds.top() + px(40.0)),
        px(18.0),
        window,
        cx,
    )?;
    Ok(())
}

fn format_attachment_size(size: u64) -> String {
    const KIB: u64 = 1024;
    const MIB: u64 = 1024 * 1024;
    if size >= MIB {
        format!("{:.1} MB", size as f64 / MIB as f64)
    } else if size >= KIB {
        format!("{:.1} KB", size as f64 / KIB as f64)
    } else {
        format!("{size} B")
    }
}

fn list_marker(kind: &BlockKind, ordered_number: Option<usize>) -> Option<String> {
    match kind {
        BlockKind::BulletItem { .. } => Some("•".to_owned()),
        BlockKind::CheckItem { checked, .. } => Some(if *checked { "☑" } else { "☐" }.to_owned()),
        BlockKind::OrderedItem { .. } => Some(format!("{}.", ordered_number.unwrap_or(1))),
        _ => None,
    }
}

/// Compute ordered markers once in document order. A child sequence only
/// advances its own depth; returning to a parent keeps the parent's counter,
/// so `[0, 1, 0]` renders `1., 1., 2.` instead of resetting the parent.
fn ordered_list_numbers(document: &Document) -> std::collections::HashMap<NodeId, usize> {
    ordered_number_summary(document)
}

/// Paint an editor entity and install GPUI's real input bridge for the same
/// entity. The entity is the sole `EntityInputHandler` owner; individual
/// blocks are never registered with the platform input system.
pub fn paint_entity(
    entity: Entity<EditorCore>,
    bounds: Bounds<Pixels>,
    image_cache: Option<Entity<BudgetedImageCache>>,
    window: &mut Window,
    cx: &mut App,
) -> gpui::Result<()> {
    #[cfg(test)]
    TEST_RENDER_OBSERVATIONS.with(|observations| {
        observations.borrow_mut().paint_entity_calls += 1;
    });
    let focus_handle = entity.read(cx).focus_handle().clone();
    if focus_handle.is_focused(window) {
        window.handle_input(
            &focus_handle,
            ElementInputHandler::new(bounds, entity.clone()),
            cx,
        );
    }
    let snapshot = entity.read_with(cx, |editor, _cx| snapshot(editor));
    let content_mask = window.content_mask().bounds;
    paint_snapshot(
        &snapshot,
        content_mask,
        image_cache,
        Some(entity),
        window,
        cx,
    )
}

/// A small pure description useful to tests and to a future measured-layout
/// element. It preserves the same paint ordering as `paint`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RenderLayer {
    BlockSurface,
    FindHighlight,
    Selection,
    GlyphsOrImage,
    Caret,
}

pub fn render_order() -> [RenderLayer; 5] {
    [
        RenderLayer::BlockSurface,
        RenderLayer::FindHighlight,
        RenderLayer::Selection,
        RenderLayer::GlyphsOrImage,
        RenderLayer::Caret,
    ]
}

pub(crate) fn image_selection_outline(bounds: Bounds<gpui::Pixels>) -> Bounds<gpui::Pixels> {
    Bounds::from_corners(
        point(bounds.left() - px(2.0), bounds.top() - px(2.0)),
        point(bounds.right() + px(2.0), bounds.bottom() + px(2.0)),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::native_editor::model::{
        Affinity, BlockKind, DocPoint, Mark, Selection, TextAlignment,
    };
    use crate::native_editor::transaction::Transaction;

    #[test]
    fn image_proxy_uses_rendered_bounds_and_validates_natural_edge() {
        let portrait_height = 680.0 * 1600.0 / 900.0;
        assert_eq!(
            image_proxy_max_edge_for_bounds(680.0, portrait_height, 2.0),
            2418
        );
        assert_eq!(image_proxy_max_edge_for_bounds(680.0, 400.0, 2.0), 1360);
        assert_eq!(natural_max_edge((0, 1600)), None);
        assert_eq!(natural_max_edge((900, 1600)), Some(1600));
    }

    #[test]
    fn image_residency_uses_mask_and_only_adjacent_prefetch_images() {
        let blocks = [
            test_image_render_block(1, 0.0),
            test_image_render_block(2, 60.0),
            test_image_render_block(3, 120.0),
            test_image_render_block(4, 180.0),
            test_image_render_block(5, 240.0),
            test_image_render_block(6, 300.0),
            test_image_render_block(7, 500.0),
        ];
        let mask = Bounds::new(point(px(0.0), px(120.0)), gpui::size(px(100.0), px(130.0)));

        let residency = classify_image_residency(&blocks, mask);

        assert_eq!(residency.visible_indices, vec![2, 3, 4]);
        assert_eq!(residency.prefetch_indices, vec![1, 5]);
        assert!(!residency.resident_indices().contains(&6));
    }

    #[test]
    fn image_request_edge_keeps_retina_for_visible_and_caps_prefetch() {
        let block = test_image_render_block(1, 0.0);

        assert_eq!(image_request_edge(&block, 2.0, true), 200);
        assert_eq!(image_request_edge(&block, 2.0, false), 200);

        let wide = RenderBlock {
            layout: BlockLayout {
                bounds: Bounds::new(point(px(0.0), px(0.0)), gpui::size(px(680.0), px(382.5))),
                ..block.layout.clone()
            },
            ..block
        };
        assert_eq!(image_request_edge(&wide, 2.0, true), 1360);
        assert_eq!(image_request_edge(&wide, 2.0, false), PREFETCH_MAX_EDGE);
    }

    #[gpui::test]
    fn attachment_snapshot_exposes_filename_mime_and_durable_size(cx: &mut gpui::TestAppContext) {
        let mut editor = EditorCore::for_test("前", cx);
        let paragraph = editor.document().first_node_id().expect("paragraph");
        editor
            .apply(super::super::transaction::Transaction::InsertAttachment {
                selection: Selection::caret(DocPoint::with_affinity(
                    paragraph,
                    "前".len(),
                    Affinity::After,
                )),
                resource_id: "attachment".into(),
                filename: "证据.pdf".into(),
                media_type: "application/pdf".into(),
            })
            .expect("insert attachment");
        editor
            .register_attachment("attachment", 2_048)
            .expect("register metadata");
        let document = editor.document().clone();
        editor.layout.layout_document(&document, 0.0, 640.0, 680.0);

        let snapshot = snapshot(&editor);
        let card = snapshot
            .blocks
            .iter()
            .find_map(|block| block.attachment.as_ref())
            .expect("attachment card snapshot");
        assert_eq!(card.filename, "证据.pdf");
        assert_eq!(card.media_type, "application/pdf");
        assert_eq!(card.size, Some(2_048));
        assert!(card.available);
    }

    fn test_image_render_block(id: u64, top: f32) -> RenderBlock {
        let node_id = NodeId::new(id);
        RenderBlock {
            layout: BlockLayout {
                node_id,
                bounds: Bounds::new(point(px(0.0), px(top)), gpui::size(px(100.0), px(50.0))),
                text_inset: px(0.0),
                text_align: gpui::TextAlign::Left,
                text_lines: Vec::new(),
                before: super::super::model::DocPoint::new(node_id, 0),
                after: super::super::model::DocPoint::new(node_id, 0),
            },
            text_lines: Vec::new(),
            is_image: true,
            shaped_background_run_count: 0,
            line_height: None,
            marker: None,
            image_resource: None,
            image_resource_id: None,
            image_natural_max_edge: None,
            attachment: None,
        }
    }

    #[gpui::test]
    fn nested_ordered_items_continue_the_parent_sequence(cx: &mut gpui::TestAppContext) {
        let mut editor = EditorCore::for_test_paragraphs(["parent one", "child", "parent two"], cx);
        let blocks = editor
            .document()
            .blocks()
            .collect_range(0..editor.document().block_count());
        for (block, kind) in blocks.iter().zip([
            BlockKind::OrderedItem { depth: 0 },
            BlockKind::OrderedItem { depth: 1 },
            BlockKind::OrderedItem { depth: 0 },
        ]) {
            let text_len = block.content.as_text().map_or(0, str::len);
            editor
                .apply(Transaction::SetBlockKind {
                    selection: Selection::new(
                        DocPoint::with_affinity(block.id, 0, Affinity::Before),
                        DocPoint::with_affinity(block.id, text_len, Affinity::After),
                    ),
                    kind,
                })
                .expect("ordered item conversion should succeed");
        }
        let numbers = ordered_list_numbers(editor.document());
        assert_eq!(
            list_marker(
                &BlockKind::OrderedItem { depth: 0 },
                numbers.get(&blocks[0].id).copied(),
            ),
            Some("1.".into())
        );
        assert_eq!(
            list_marker(
                &BlockKind::OrderedItem { depth: 1 },
                numbers.get(&blocks[1].id).copied(),
            ),
            Some("1.".into())
        );
        assert_eq!(
            list_marker(
                &BlockKind::OrderedItem { depth: 0 },
                numbers.get(&blocks[2].id).copied(),
            ),
            Some("2.".into())
        );
    }

    #[gpui::test]
    fn mixed_child_types_do_not_reset_parent_ordered_numbers(cx: &mut gpui::TestAppContext) {
        let mut editor = EditorCore::for_test_paragraphs(
            [
                "parent one",
                "bullet child",
                "parent two",
                "check child",
                "parent three",
            ],
            cx,
        );
        let blocks = editor
            .document()
            .blocks()
            .collect_range(0..editor.document().block_count());
        for (block, kind) in blocks.iter().zip([
            BlockKind::OrderedItem { depth: 0 },
            BlockKind::BulletItem { depth: 1 },
            BlockKind::OrderedItem { depth: 0 },
            BlockKind::CheckItem {
                depth: 1,
                checked: false,
            },
            BlockKind::OrderedItem { depth: 0 },
        ]) {
            let text_len = block.content.as_text().map_or(0, str::len);
            editor
                .apply(Transaction::SetBlockKind {
                    selection: Selection::new(
                        DocPoint::with_affinity(block.id, 0, Affinity::Before),
                        DocPoint::with_affinity(block.id, text_len, Affinity::After),
                    ),
                    kind,
                })
                .expect("mixed child conversion should succeed");
        }
        let numbers = ordered_list_numbers(editor.document());
        assert_eq!(numbers.get(&blocks[0].id), Some(&1));
        assert_eq!(numbers.get(&blocks[2].id), Some(&2));
        assert_eq!(numbers.get(&blocks[4].id), Some(&3));
    }

    #[gpui::test]
    fn numbering_summary_reuses_scan_and_snapshot_reads_visible_nodes(
        cx: &mut gpui::TestAppContext,
    ) {
        let cx = cx.add_empty_window();
        let mut editor =
            EditorCore::for_test_paragraphs((0..256).map(|index| format!("ordered-{index}")), cx);
        let blocks = editor
            .document()
            .blocks()
            .collect_range(0..editor.document().block_count());
        for block in blocks {
            let text_len = block.content.as_text().map_or(0, str::len);
            editor
                .apply(Transaction::SetBlockKind {
                    selection: Selection::new(
                        DocPoint::with_affinity(block.id, 0, Affinity::Before),
                        DocPoint::with_affinity(block.id, text_len, Affinity::After),
                    ),
                    kind: BlockKind::OrderedItem { depth: 0 },
                })
                .expect("ordered fixture conversion");
        }
        cx.update(|window, _| {
            editor.shape_visible_with_window(0.0, 32.0, 120.0, window);
        });
        let scan_count = editor.layout().ordered_number_scan_count();
        assert_eq!(scan_count, editor.document().block_count());
        assert!(editor.layout().visible().len() < editor.document().block_count());
        let visible_ids = editor
            .layout()
            .visible()
            .iter()
            .map(|block| block.node_id)
            .collect::<std::collections::HashSet<_>>();
        let first = snapshot(&editor);
        let second = snapshot(&editor);
        assert!(
            first
                .blocks
                .iter()
                .all(|block| visible_ids.contains(&block.layout.node_id))
        );
        assert!(
            second
                .blocks
                .iter()
                .all(|block| visible_ids.contains(&block.layout.node_id))
        );
        assert_eq!(editor.layout().ordered_number_scan_count(), scan_count);
        cx.update(|window, _| {
            editor.shape_visible_with_window(0.0, 32.0, 120.0, window);
        });
        assert_eq!(editor.layout().ordered_number_scan_count(), scan_count);

        let inline_target = editor.document().blocks()[128].clone();
        editor
            .apply(Transaction::InsertText {
                selection: Selection::caret(DocPoint::with_affinity(
                    inline_target.id,
                    inline_target.content.as_text().unwrap().len(),
                    Affinity::After,
                )),
                text: "!".into(),
            })
            .expect("inline edit should succeed");
        cx.update(|window, _| {
            editor.shape_visible_with_window(0.0, 32.0, 120.0, window);
        });
        assert_eq!(
            editor.layout().ordered_number_scan_count(),
            scan_count,
            "text edits must reuse the structural numbering summary"
        );

        for mark in [
            Mark::Bold,
            Mark::Link("https://example.com".into()),
            Mark::Highlight,
        ] {
            editor
                .apply(Transaction::ToggleMark {
                    selection: Selection::new(
                        DocPoint::with_affinity(inline_target.id, 0, Affinity::Before),
                        DocPoint::with_affinity(inline_target.id, 1, Affinity::After),
                    ),
                    mark,
                })
                .expect("inline mark edit should succeed");
            cx.update(|window, _| {
                editor.shape_visible_with_window(0.0, 32.0, 120.0, window);
            });
            assert_eq!(
                editor.layout().ordered_number_scan_count(),
                scan_count,
                "mark and link edits must reuse the structural numbering summary"
            );
        }

        let marked = editor.document().blocks()[128].clone();
        editor
            .apply(Transaction::SetAlignment {
                selection: Selection::new(
                    DocPoint::with_affinity(marked.id, 0, Affinity::Before),
                    DocPoint::with_affinity(
                        marked.id,
                        marked.content.as_text().unwrap().len(),
                        Affinity::After,
                    ),
                ),
                alignment: TextAlignment::Center,
            })
            .expect("alignment edit should succeed");
        cx.update(|window, _| {
            editor.shape_visible_with_window(0.0, 32.0, 120.0, window);
        });
        assert_eq!(
            editor.layout().ordered_number_scan_count(),
            scan_count,
            "alignment edits must reuse the structural numbering summary"
        );

        let first = editor.document().blocks()[0].clone();
        let first_len = first.content.as_text().unwrap().len();
        editor
            .apply(Transaction::SetBlockKind {
                selection: Selection::new(
                    DocPoint::with_affinity(first.id, 0, Affinity::Before),
                    DocPoint::with_affinity(first.id, first_len, Affinity::After),
                ),
                kind: BlockKind::Paragraph,
            })
            .expect("structural kind edit should succeed");
        let last_id = editor.document().blocks().last().unwrap().id;
        cx.update(|window, _| {
            editor.shape_visible_with_window(0.0, 32.0, 120.0, window);
        });
        assert_eq!(
            editor.layout().ordered_number(last_id),
            Some(255),
            "a structural list boundary must update later ordered markers"
        );
    }

    #[test]
    fn highlight_background_paint_precedes_glyphs() {
        assert_eq!(
            text_paint_passes(),
            [TextPaintPass::Background, TextPaintPass::Glyphs]
        );
    }
}
