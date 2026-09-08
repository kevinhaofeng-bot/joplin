//! GPUI rendering for the document-wide native editor surface.
//!
//! Rendering consumes the registry only. It does not create focus handles or
//! mutate document state. The paint order is deliberately explicit: surfaces,
//! selection geometry, glyphs/images, then the caret.

use gpui::{
    App, Bounds, Corners, ElementInputHandler, Entity, Pixels, ShapedLine, Window, fill, point, px,
    rgba,
};

use super::core::EditorCore;
use super::layout::BlockLayout;
use super::model::{DocPoint, Selection};

#[derive(Clone)]
struct RenderBlock {
    layout: BlockLayout,
    text_lines: Vec<ShapedLine>,
    is_image: bool,
    line_height: Option<Pixels>,
}

#[derive(Clone)]
struct RenderSnapshot {
    blocks: Vec<RenderBlock>,
    selection_rects: Vec<Bounds<Pixels>>,
    selection: Selection,
}

fn snapshot(editor: &EditorCore) -> RenderSnapshot {
    let layout = editor.layout();
    let blocks = layout
        .visible()
        .iter()
        .map(|block| RenderBlock {
            layout: block.clone(),
            text_lines: layout
                .block_layout(block.node_id)
                .map(|cached| cached.text_lines.clone())
                .unwrap_or_default(),
            is_image: layout.is_image(block.node_id),
            line_height: layout.line_height(block.node_id),
        })
        .collect();
    RenderSnapshot {
        blocks,
        selection_rects: layout.selection_rects(editor.selection()),
        selection: editor.selection(),
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
        let mut quad = fill(*rect, rgba(0x4f8cff55));
        if is_image {
            quad.corner_radii = Corners::all(px(6.0));
        }
        window.paint_quad(quad);
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
        for (line_index, line) in block.text_lines.iter().enumerate() {
            line.paint(
                point(
                    block.layout.bounds.left(),
                    block.layout.bounds.top() + line_height * line_index as f32,
                ),
                line_height,
                window,
                cx,
            )?;
        }
    }

    // 4. Caret. Image before/after positions use the same vertical caret
    // geometry as paragraph text through LayoutRegistry::caret_bounds.
    if snapshot.selection.is_caret() {
        let point = snapshot.selection.head;
        if let Some(bounds) = caret_bounds(snapshot, point) {
            window.paint_quad(fill(bounds, rgba(0x2167dfff)));
        }
    }
    Ok(())
}

fn caret_bounds(snapshot: &RenderSnapshot, caret: DocPoint) -> Option<Bounds<Pixels>> {
    let block = snapshot
        .blocks
        .iter()
        .find(|block| block.layout.node_id == caret.node_id)?;
    if block.is_image {
        let x = if caret.utf8_offset == 0 {
            block.layout.bounds.left()
        } else {
            block.layout.bounds.right()
        };
        return Some(Bounds::new(
            point(x, block.layout.bounds.top()),
            gpui::size(px(1.0), block.layout.bounds.size.height),
        ));
    }
    let line_height = block.line_height.unwrap_or(px(24.0));
    let offset = caret.utf8_offset.min(block.layout.after.utf8_offset);
    if block.text_lines.is_empty() {
        return Some(Bounds::new(
            point(
                block.layout.bounds.left() + px(offset as f32 * 8.0),
                block.layout.bounds.top(),
            ),
            gpui::size(px(1.0), block.layout.bounds.size.height),
        ));
    }
    let mut line_start = 0usize;
    for (line_index, line) in block.text_lines.iter().enumerate() {
        if offset <= line_start.saturating_add(line.len()) {
            return Some(Bounds::new(
                point(
                    block.layout.bounds.left() + line.x_for_index(offset - line_start),
                    block.layout.bounds.top() + line_height * line_index as f32,
                ),
                gpui::size(px(1.0), line_height),
            ));
        }
        line_start = line_start.saturating_add(line.len());
    }
    Some(Bounds::new(
        point(
            block.layout.bounds.right(),
            block.layout.bounds.bottom() - line_height,
        ),
        gpui::size(px(1.0), line_height),
    ))
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
