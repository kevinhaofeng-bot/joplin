//! GPUI rendering for the document-wide native editor surface.
//!
//! Rendering consumes the registry only. It does not create focus handles or
//! mutate document state. The paint order is deliberately explicit: surfaces,
//! selection geometry, glyphs/images, then the caret.

use gpui::{
    App, BorderStyle, Bounds, Corners, ElementInputHandler, Entity, Pixels, TextAlign, Window,
    WrappedLine, fill, outline, point, px, rgba,
};

use super::core::EditorCore;
use super::layout::BlockLayout;
use super::model::Selection;

#[derive(Clone)]
struct RenderBlock {
    layout: BlockLayout,
    text_lines: Vec<WrappedLine>,
    is_image: bool,
    line_height: Option<Pixels>,
}

#[derive(Clone)]
struct RenderSnapshot {
    blocks: Vec<RenderBlock>,
    selection_rects: Vec<Bounds<Pixels>>,
    selection: Selection,
    caret_bounds: Option<Bounds<Pixels>>,
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
        for (line_index, line) in block.text_lines.iter().enumerate() {
            line.paint(
                point(
                    block.layout.bounds.left(),
                    block.layout.bounds.top()
                        + block
                            .text_lines
                            .iter()
                            .take(line_index)
                            .map(|line| line.size(line_height).height)
                            .fold(px(0.0), |top, height| top + height),
                ),
                line_height,
                TextAlign::Left,
                Some(block.layout.bounds),
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
