//! GPUI rendering for the document-wide native editor surface.
//!
//! Rendering consumes the registry only. It does not create focus handles or
//! mutate document state. The paint order is deliberately explicit: surfaces,
//! selection geometry, glyphs/images, then the caret.

use gpui::{
    App, BorderStyle, Bounds, Corners, ElementInputHandler, Entity, Pixels, SharedString, TextRun,
    Window, WrappedLine, fill, outline, point, px, rgba,
};

use super::core::EditorCore;
use super::layout::BlockLayout;
use super::model::{BlockKind, Document, NodeId, Selection};

#[derive(Clone)]
struct RenderBlock {
    layout: BlockLayout,
    text_lines: Vec<WrappedLine>,
    is_image: bool,
    line_height: Option<Pixels>,
    marker: Option<String>,
}

#[derive(Clone)]
struct RenderSnapshot {
    blocks: Vec<RenderBlock>,
    selection_rects: Vec<Bounds<Pixels>>,
    selection: Selection,
    caret_bounds: Option<Bounds<Pixels>>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum TextPaintPass {
    Background,
    Glyphs,
}

const fn text_paint_passes() -> [TextPaintPass; 2] {
    [TextPaintPass::Background, TextPaintPass::Glyphs]
}

fn snapshot(editor: &EditorCore) -> RenderSnapshot {
    let layout = editor.layout();
    let ordered_numbers = ordered_list_numbers(editor.document());
    let blocks = layout
        .visible()
        .iter()
        .map(|block| {
            let kind = editor
                .document()
                .block(block.node_id)
                .map(|block| block.kind.clone())
                .unwrap_or(BlockKind::Paragraph);
            RenderBlock {
                layout: block.clone(),
                text_lines: layout
                    .block_layout(block.node_id)
                    .map(|cached| cached.text_lines.clone())
                    .unwrap_or_default(),
                is_image: layout.is_image(block.node_id),
                line_height: layout.line_height(block.node_id),
                marker: list_marker(&kind, &ordered_numbers, block.node_id),
            }
        })
        .collect();
    RenderSnapshot {
        blocks,
        selection_rects: layout.selection_rects(editor.selection()),
        selection: editor.selection(),
        caret_bounds: editor
            .selection()
            .is_caret()
            .then(|| layout.caret_bounds_for_point(editor.selection().head))
            .flatten(),
    }
}

pub fn paint(editor: &EditorCore, window: &mut Window, cx: &mut App) -> gpui::Result<()> {
    let snapshot = snapshot(editor);
    paint_snapshot(&snapshot, window, cx)
}

fn paint_snapshot(
    snapshot: &RenderSnapshot,
    window: &mut Window,
    cx: &mut App,
) -> gpui::Result<()> {
    // 1. Block surfaces.
    for block in &snapshot.blocks {
        let mut quad = fill(block.layout.bounds, rgba(0x00000000));
        if block.is_image {
            quad.corner_radii = Corners::all(px(6.0));
        }
        window.paint_quad(quad);
    }

    // 2. Selection rectangles. Image atoms use the same geometry but with a
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

    // 3. Glyphs/images.
    for block in &snapshot.blocks {
        if block.is_image {
            let mut quad = fill(block.layout.bounds, rgba(0x9aa4b233));
            quad.corner_radii = Corners::all(px(6.0));
            window.paint_quad(quad);
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
        for (line_index, line) in block.text_lines.iter().enumerate() {
            let origin = point(
                text_bounds.left(),
                block.layout.bounds.top()
                    + block
                        .text_lines
                        .iter()
                        .take(line_index)
                        .map(|line| line.size(line_height).height)
                        .fold(px(0.0), |top, height| top + height),
            );
            for pass in text_paint_passes() {
                match pass {
                    TextPaintPass::Background => line.paint_background(
                        origin,
                        line_height,
                        block.layout.text_align,
                        Some(text_bounds),
                        window,
                        cx,
                    )?,
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

fn list_marker(
    kind: &BlockKind,
    ordered_numbers: &std::collections::HashMap<NodeId, usize>,
    node_id: NodeId,
) -> Option<String> {
    match kind {
        BlockKind::BulletItem { .. } => Some("•".to_owned()),
        BlockKind::CheckItem { checked, .. } => Some(if *checked { "☑" } else { "☐" }.to_owned()),
        BlockKind::OrderedItem { .. } => Some(format!(
            "{}.",
            ordered_numbers.get(&node_id).copied().unwrap_or(1)
        )),
        _ => None,
    }
}

/// Compute ordered markers once in document order. A child sequence only
/// advances its own depth; returning to a parent keeps the parent's counter,
/// so `[0, 1, 0]` renders `1., 1., 2.` instead of resetting the parent.
fn ordered_list_numbers(document: &Document) -> std::collections::HashMap<NodeId, usize> {
    let mut numbers = std::collections::HashMap::new();
    let mut counters: Vec<usize> = Vec::new();
    for block in document.blocks() {
        let BlockKind::OrderedItem { depth } = block.kind else {
            counters.clear();
            continue;
        };
        let depth = depth as usize;
        counters.truncate(depth + 1);
        while counters.len() <= depth {
            counters.push(0);
        }
        counters[depth] = counters[depth].saturating_add(1).max(1);
        numbers.insert(block.id, counters[depth]);
    }
    numbers
}

/// Paint an editor entity and install GPUI's real input bridge for the same
/// entity. The entity is the sole `EntityInputHandler` owner; individual
/// blocks are never registered with the platform input system.
pub fn paint_entity(
    entity: Entity<EditorCore>,
    bounds: Bounds<Pixels>,
    window: &mut Window,
    cx: &mut App,
) -> gpui::Result<()> {
    let focus_handle = entity.read(cx).focus_handle().clone();
    if focus_handle.is_focused(window) {
        window.handle_input(
            &focus_handle,
            ElementInputHandler::new(bounds, entity.clone()),
            cx,
        );
    }
    let snapshot = entity.read_with(cx, |editor, _cx| snapshot(editor));
    paint_snapshot(&snapshot, window, cx)
}

/// A small pure description useful to tests and to a future measured-layout
/// element. It preserves the same four-layer ordering as `paint`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RenderLayer {
    BlockSurface,
    Selection,
    GlyphsOrImage,
    Caret,
}

pub fn render_order() -> [RenderLayer; 4] {
    [
        RenderLayer::BlockSurface,
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
    use crate::native_editor::model::{Affinity, BlockKind, DocPoint, Selection};
    use crate::native_editor::transaction::Transaction;

    #[gpui::test]
    fn nested_ordered_items_continue_the_parent_sequence(cx: &mut gpui::TestAppContext) {
        let mut editor = EditorCore::for_test_paragraphs(["parent one", "child", "parent two"], cx);
        let blocks = editor.document().blocks().to_vec();
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
            list_marker(&BlockKind::OrderedItem { depth: 0 }, &numbers, blocks[0].id),
            Some("1.".into())
        );
        assert_eq!(
            list_marker(&BlockKind::OrderedItem { depth: 1 }, &numbers, blocks[1].id),
            Some("1.".into())
        );
        assert_eq!(
            list_marker(&BlockKind::OrderedItem { depth: 0 }, &numbers, blocks[2].id),
            Some("2.".into())
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
