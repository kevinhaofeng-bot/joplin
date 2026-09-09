//! GPUI rendering for the document-wide native editor surface.
//!
//! Rendering consumes the registry only. It does not create focus handles or
//! mutate document state. The paint order is deliberately explicit: surfaces,
//! selection geometry, glyphs/images, then the caret.

use gpui::{
    App, BorderStyle, Bounds, Corners, ElementInputHandler, Entity, ImageCache, Pixels, Resource,
    SharedString, TextRun, Window, WrappedLine, fill, outline, point, px, rgba,
};
#[cfg(test)]
use std::cell::RefCell;

use super::core::EditorCore;
use super::images::BudgetedImageCache;
use super::layout::{BlockLayout, ordered_number_summary};
use super::model::{BlockKind, Document, NodeId, Selection};

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

#[cfg(test)]
#[derive(Default)]
struct TestRenderObservations {
    snapshot_clone_peak: usize,
    shaped_background_paints: usize,
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
    paint_snapshot(&snapshot, None, None, window, cx)
}

fn paint_snapshot(
    snapshot: &RenderSnapshot,
    image_cache: Option<Entity<BudgetedImageCache>>,
    editor: Option<Entity<EditorCore>>,
    window: &mut Window,
    cx: &mut App,
) -> gpui::Result<()> {
    if let Some(cache) = image_cache.as_ref() {
        cache.update(cx, |cache, _| cache.begin_frame());
    }
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
            let image = block.image_resource.as_ref().and_then(|resource| {
                image_cache.as_ref().and_then(|cache| {
                    cache.update(cx, |cache, cx| {
                        cache.mark_visible(resource);
                        cache.load(resource, window, cx)
                    })
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
    let focus_handle = entity.read(cx).focus_handle().clone();
    if focus_handle.is_focused(window) {
        window.handle_input(
            &focus_handle,
            ElementInputHandler::new(bounds, entity.clone()),
            cx,
        );
    }
    let snapshot = entity.read_with(cx, |editor, _cx| snapshot(editor));
    paint_snapshot(&snapshot, image_cache, Some(entity), window, cx)
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
    use crate::native_editor::model::{
        Affinity, BlockKind, DocPoint, Mark, Selection, TextAlignment,
    };
    use crate::native_editor::transaction::Transaction;

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
