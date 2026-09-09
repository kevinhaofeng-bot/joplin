use super::commands::{
    CommandArgument, CommandCatalogue, CommandError, EditorCommand, ToggleState,
};
use super::core::EditorCore;
use super::history::History;
use super::layout::{LAYOUT_CACHE_BUDGET_BYTES, LayoutRegistry, ordered_number_summary};
use super::render;
use crate::spike_app::{SpikeRouteContract, layout_for_viewport, route_contract};
use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;
use std::mem::size_of;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use gpui::{AppContext, Bounds, EntityInputHandler, FontStyle, FontWeight, TextStyle, point, px};
use smallvec::SmallVec;
use unicode_segmentation::UnicodeSegmentation;

use super::model::{
    Affinity, Block, BlockContent, BlockKind, DocPoint, Document, DocumentError, Mark, NodeId,
    Selection, StyledRun, TextAlignment, grapheme_resolution_counter,
    reset_grapheme_resolution_counter, reset_validation_grapheme_counter,
    validation_grapheme_counter,
};
use super::transaction::{ApplyOutcome, Transaction, TransactionBatch};

struct CountingAllocator;

thread_local! {
    // Keep the measurement state on the test worker that owns the real
    // selection call. A process-global counter lets another concurrently
    // running gpui test contaminate the observed scratch peak.
    static MEASURING_ALLOCATIONS: Cell<Option<usize>> = const { Cell::new(None) };
}

unsafe impl GlobalAlloc for CountingAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        MEASURING_ALLOCATIONS.with(|measuring| {
            if let Some(bytes) = measuring.get() {
                measuring.set(Some(bytes.saturating_add(layout.size())));
            }
        });
        unsafe { System.alloc(layout) }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        unsafe { System.dealloc(ptr, layout) };
    }
}

#[global_allocator]
static TEST_ALLOCATOR: CountingAllocator = CountingAllocator;

pub(crate) struct AllocationMeasurement;

impl AllocationMeasurement {
    pub(crate) fn begin() -> Self {
        MEASURING_ALLOCATIONS.with(|measuring| measuring.set(Some(0)));
        Self
    }

    pub(crate) fn bytes(&self) -> usize {
        MEASURING_ALLOCATIONS.with(|measuring| measuring.get().unwrap_or(0))
    }
}

impl Drop for AllocationMeasurement {
    fn drop(&mut self) {
        MEASURING_ALLOCATIONS.with(|measuring| measuring.set(None));
    }
}

fn apply_history_and_measure(
    document: &mut Document,
    history: &mut History,
    layout: &mut LayoutRegistry,
    before_selection: Selection,
    transaction: Transaction,
) -> (ApplyOutcome, usize) {
    let measurement = AllocationMeasurement::begin();
    let outcome = history
        .apply_with_selection(document, before_selection, transaction)
        .expect("history transaction");
    layout.invalidate_nodes_with_delta(
        document,
        &outcome.changed_nodes,
        outcome.structural,
        &outcome.structural_splices,
        &outcome.numbering_ranges,
    );
    (outcome, measurement.bytes())
}

fn ordered_fixture(count: usize) -> Vec<Block> {
    (0..count)
        .map(|index| Block {
            id: NodeId::new((index + 1) as u64),
            kind: BlockKind::OrderedItem { depth: 0 },
            content: BlockContent::text(format!("item-{index}")),
            alignment: TextAlignment::Left,
            revision: 0,
        })
        .collect()
}

fn assert_ordered_numbers_match(document: &Document, layout: &LayoutRegistry, context: &str) {
    let oracle = ordered_number_summary(document);
    for block in document.blocks() {
        assert_eq!(
            layout.ordered_number(block.id),
            oracle.get(&block.id).copied(),
            "{context}: stale number for {:?}",
            block.id
        );
    }
}

#[test]
fn paste_image_splits_paragraph_once() {
    let mut doc = Document::from_paragraph("前面的文字后面的文字");
    let paragraph = doc.first_node_id().unwrap();
    let at = "前面的文字".len();

    doc.apply(Transaction::InsertImage {
        selection: Selection::caret(DocPoint::new(paragraph, at)),
        resource_id: "fixture-image".into(),
        natural_size: (1600, 900),
    })
    .unwrap();

    assert_eq!(
        doc.block_kinds(),
        [BlockKind::Paragraph, BlockKind::Image, BlockKind::Paragraph]
    );
    assert_eq!(doc.text_at_index(0), Some("前面的文字"));
    assert_eq!(doc.text_at_index(2), Some("后面的文字"));
}

#[test]
fn list_commands_share_transaction_path() {
    let mut doc = Document::from_paragraphs(["一", "二"]);
    let selection = doc.select_all_text();
    let outcome = doc
        .apply(Transaction::SetBlockKind {
            selection,
            kind: BlockKind::BulletItem { depth: 0 },
        })
        .unwrap();
    assert_eq!(outcome.changed_nodes.len(), 2);
    assert!(
        doc.block_kinds()
            .iter()
            .all(|kind| matches!(kind, BlockKind::BulletItem { depth: 0 }))
    );
}

#[test]
fn inverse_transaction_restores_document_and_selection() {
    let mut doc = Document::from_paragraph("abc");
    let original = doc.semantic_snapshot();
    let mut history = History::new(1_000, 16 * 1024 * 1024);
    let end = doc.end_selection();
    history
        .apply(
            &mut doc,
            Transaction::InsertText {
                selection: end,
                text: "中文".into(),
            },
        )
        .unwrap();
    let restored_selection = history.undo(&mut doc).unwrap();
    assert_eq!(doc.semantic_snapshot(), original);
    assert_eq!(restored_selection, end);
}

#[test]
fn deterministic_operation_sequence_round_trips_through_inverse_history() {
    let mut doc = Document::from_paragraphs(["ab", "cd"]);
    let origin = doc.semantic_snapshot();
    let mut history = History::new(1_000, 16 * 1024 * 1024);

    let first = doc.first_node_id().unwrap();
    let first_end = doc.text_at_index(0).unwrap().len();
    history
        .apply(
            &mut doc,
            Transaction::InsertText {
                selection: Selection::caret(DocPoint::new(first, first_end)),
                text: "X".into(),
            },
        )
        .unwrap();
    doc.validate_invariants().unwrap();

    history
        .apply(
            &mut doc,
            Transaction::SplitBlock {
                at: DocPoint::new(first, 1),
            },
        )
        .unwrap();
    doc.validate_invariants().unwrap();

    let all_text = doc.select_all_text();
    history
        .apply(
            &mut doc,
            Transaction::SetBlockKind {
                selection: all_text,
                kind: BlockKind::BulletItem { depth: 0 },
            },
        )
        .unwrap();
    doc.validate_invariants().unwrap();

    let first_text_end = doc.text_at_index(0).unwrap().len();
    let mark_selection = Selection::new(
        DocPoint::with_affinity(first, 0, super::model::Affinity::Before),
        DocPoint::with_affinity(first, first_text_end, super::model::Affinity::After),
    );
    for mark in [Mark::Bold, Mark::Italic, Mark::Highlight] {
        history
            .apply(
                &mut doc,
                Transaction::ToggleMark {
                    selection: mark_selection,
                    mark,
                },
            )
            .unwrap();
        doc.validate_invariants().unwrap();
    }

    history
        .apply(
            &mut doc,
            Transaction::InsertImage {
                selection: Selection::caret(DocPoint::new(first, 1)),
                resource_id: "sequence-image".into(),
                natural_size: (320, 200),
            },
        )
        .unwrap();
    doc.validate_invariants().unwrap();

    let right = doc.blocks()[2].id;
    let second = doc.blocks().last().unwrap().id;
    let second_end = doc.text_at_index(doc.block_count() - 1).unwrap().len();
    history
        .apply(
            &mut doc,
            Transaction::DeleteRange {
                selection: Selection::new(
                    DocPoint::new(right, 0),
                    DocPoint::new(second, second_end),
                ),
            },
        )
        .unwrap();
    doc.validate_invariants().unwrap();

    let final_state = doc.semantic_snapshot();
    while history.undo_depth() > 0 {
        history.undo(&mut doc).unwrap();
        doc.validate_invariants().unwrap();
    }
    assert_eq!(doc.semantic_snapshot(), origin);
    while history.redo_depth() > 0 {
        history.redo(&mut doc).unwrap();
        doc.validate_invariants().unwrap();
    }
    assert_eq!(doc.semantic_snapshot(), final_state);
}

#[test]
fn invalid_transaction_is_atomic() {
    let mut doc = Document::from_paragraph("a😀b");
    let original = doc.semantic_snapshot();
    let original_revision = doc.revision();
    let original_selection = doc.end_selection();
    let mut history = History::new(1_000, 16 * 1024 * 1024);

    let node = doc.first_node_id().unwrap();
    let invalid_utf8 = history.apply(
        &mut doc,
        Transaction::InsertText {
            selection: Selection::caret(DocPoint::new(node, 2)),
            text: "x".into(),
        },
    );
    assert!(matches!(
        invalid_utf8,
        Err(DocumentError::InvalidUtf8Offset { .. })
            | Err(DocumentError::InvalidGraphemeOffset { .. })
    ));
    assert_eq!(doc.semantic_snapshot(), original);
    assert_eq!(doc.end_selection(), original_selection);
    assert_eq!(doc.revision(), original_revision);
    assert_eq!(history.undo_depth(), 0);

    let invalid_depth = doc.apply(Transaction::SetBlockKind {
        selection: doc.select_all_text(),
        kind: BlockKind::BulletItem { depth: u8::MAX },
    });
    assert!(matches!(
        invalid_depth,
        Err(DocumentError::InvalidListDepth(_))
    ));
    assert_eq!(doc.semantic_snapshot(), original);
    assert_eq!(doc.revision(), original_revision);
    assert_eq!(history.undo_depth(), 0);
}

#[test]
fn invalid_batch_rolls_back_prior_local_operations() {
    let mut doc = Document::from_paragraph("ab");
    let original = doc.semantic_snapshot();
    let original_revision = doc.revision();
    let node = doc.first_node_id().unwrap();
    let result = doc.apply_batch(TransactionBatch(vec![
        Transaction::InsertText {
            selection: Selection::caret(DocPoint::new(node, 2)),
            text: "x".into(),
        },
        Transaction::InsertText {
            selection: Selection::caret(DocPoint::new(node, 99)),
            text: "should-not-commit".into(),
        },
    ]));
    assert!(matches!(
        result,
        Err(DocumentError::InvalidUtf8Offset { .. })
            | Err(DocumentError::InvalidGraphemeOffset { .. })
    ));
    assert_eq!(doc.semantic_snapshot(), original);
    assert_eq!(doc.revision(), original_revision);
}

#[test]
fn over_budget_edit_cuts_stale_undo_history() {
    let mut doc = Document::from_paragraph("A");
    let mut history = History::new(1_000, 4_096);
    let node = doc.first_node_id().unwrap();
    let end = doc.end_selection();
    history
        .apply(
            &mut doc,
            Transaction::InsertText {
                selection: end,
                text: "x".into(),
            },
        )
        .unwrap();
    assert_eq!(history.undo_depth(), 1);

    history
        .apply(
            &mut doc,
            Transaction::InsertText {
                selection: Selection::caret(DocPoint::new(node, 2)),
                text: "y".repeat(5_000),
            },
        )
        .unwrap();
    assert_eq!(doc.text_at_index(0).unwrap().len(), 5_002);
    assert_eq!(history.undo_depth(), 0);
    assert!(matches!(
        history.undo(&mut doc),
        Err(DocumentError::HistoryEmpty)
    ));
}

#[test]
fn marks_and_links_preserve_residual_runs_at_selection_edges() {
    let mut doc = Document::from_paragraph("abcd");
    let node = doc.first_node_id().unwrap();
    let all = Selection::new(
        DocPoint::with_affinity(node, 0, Affinity::Before),
        DocPoint::with_affinity(node, 4, Affinity::After),
    );
    doc.apply(Transaction::ToggleMark {
        selection: all,
        mark: Mark::Bold,
    })
    .unwrap();
    let middle = Selection::new(DocPoint::new(node, 1), DocPoint::new(node, 3));
    doc.apply(Transaction::ToggleMark {
        selection: middle,
        mark: Mark::Italic,
    })
    .unwrap();
    doc.apply(Transaction::SetLink {
        selection: middle,
        url: Some("https://example.test".into()),
    })
    .unwrap();

    let styles = match &doc.blocks()[0].content {
        BlockContent::Text { styles, .. } => styles,
        _ => panic!("expected text block"),
    };
    assert_eq!(styles.len(), 3);
    assert_eq!(styles[0].range, 0..1);
    assert_eq!(styles[0].marks.as_slice(), [Mark::Bold].as_slice());
    assert_eq!(styles[1].range, 1..3);
    assert!(styles[1].marks.contains(&Mark::Bold));
    assert!(styles[1].marks.contains(&Mark::Italic));
    assert!(
        styles[1]
            .marks
            .contains(&Mark::Link("https://example.test".into()))
    );
    assert_eq!(styles[2].range, 3..4);
    assert_eq!(styles[2].marks.as_slice(), [Mark::Bold].as_slice());
    doc.validate_invariants().unwrap();
}

#[test]
fn cross_block_delete_shifts_suffix_styles_after_retained_prefix() {
    let mut doc = Document::from_paragraphs(["abc", "def"]);
    let second = doc.blocks()[1].id;
    doc.apply(Transaction::ToggleMark {
        selection: Selection::new(DocPoint::new(second, 1), DocPoint::new(second, 3)),
        mark: Mark::Bold,
    })
    .unwrap();
    let first = doc.first_node_id().unwrap();
    doc.apply(Transaction::DeleteRange {
        selection: Selection::new(DocPoint::new(first, 2), DocPoint::new(second, 1)),
    })
    .unwrap();
    assert_eq!(doc.text_at_index(0), Some("abef"));
    let styles = match &doc.blocks()[0].content {
        BlockContent::Text { styles, .. } => styles,
        _ => panic!("expected text block"),
    };
    assert_eq!(styles.len(), 1);
    assert_eq!(styles[0].range, 2..4);
    assert_eq!(styles[0].marks.as_slice(), [Mark::Bold].as_slice());
}

#[test]
fn image_before_after_points_and_cross_image_ranges_use_document_transactions() {
    let mut doc = Document::from_paragraph("ab");
    let paragraph = doc.first_node_id().unwrap();
    doc.apply(Transaction::InsertImage {
        selection: Selection::caret(DocPoint::new(paragraph, 1)),
        resource_id: "first".into(),
        natural_size: (100, 100),
    })
    .unwrap();
    let image = doc.blocks()[1].id;
    let before = DocPoint::with_affinity(image, 0, Affinity::Before);
    let after = DocPoint::with_affinity(image, 0, Affinity::After);
    doc.apply(Transaction::InsertImage {
        selection: Selection::new(before, after),
        resource_id: "replacement".into(),
        natural_size: (200, 100),
    })
    .unwrap();
    assert!(matches!(
        doc.blocks()[1].content,
        BlockContent::Image { .. }
    ));

    let left = doc.blocks()[0].id;
    let right = doc.blocks().last().unwrap().id;
    let right_end = doc.text_at_index(doc.block_count() - 1).unwrap().len();
    doc.apply(Transaction::DeleteRange {
        selection: Selection::new(DocPoint::new(left, 0), DocPoint::new(right, right_end)),
    })
    .unwrap();
    assert_eq!(doc.block_count(), 1);
    assert_eq!(doc.text_at_index(0), Some(""));
    doc.validate_invariants().unwrap();
}

#[test]
fn cross_image_input_replacement_consumes_structural_nodes() {
    let mut doc = Document::from_paragraph("ab");
    let paragraph = doc.first_node_id().unwrap();
    doc.apply(Transaction::InsertImage {
        selection: Selection::caret(DocPoint::new(paragraph, 1)),
        resource_id: "embedded".into(),
        natural_size: (100, 100),
    })
    .unwrap();
    let left = doc.blocks()[0].id;
    let right = doc.blocks().last().unwrap().id;
    let right_end = doc.text_at_index(doc.block_count() - 1).unwrap().len();
    doc.apply(Transaction::InsertText {
        selection: Selection::new(DocPoint::new(left, 0), DocPoint::new(right, right_end)),
        text: "replacement".into(),
    })
    .unwrap();
    assert_eq!(doc.block_count(), 1);
    assert_eq!(doc.text_at_index(0), Some("replacement"));
    doc.validate_invariants().unwrap();
}

#[test]
fn insertion_resolves_grapheme_seam_before_returning_cursor() {
    let mut doc = Document::from_paragraph("\u{301}");
    let node = doc.first_node_id().unwrap();
    let outcome = doc
        .apply(Transaction::InsertText {
            selection: Selection::caret(DocPoint::new(node, 0)),
            text: "a".into(),
        })
        .unwrap();
    assert_eq!(outcome.selection.head.utf8_offset, 3);
    doc.apply(Transaction::InsertText {
        selection: outcome.selection,
        text: "b".into(),
    })
    .unwrap();
    assert_eq!(doc.text_at_index(0), Some("a\u{301}b"));
}

#[test]
fn undo_redo_reprices_replacement_payloads_and_trims_to_budget() {
    let mut doc = Document::from_paragraph("a".repeat(4_000));
    let mut history = History::new(1_000, 6_000);
    let end = doc.end_selection();
    history
        .apply(
            &mut doc,
            Transaction::InsertText {
                selection: end,
                text: "x".into(),
            },
        )
        .unwrap();
    assert_eq!(history.undo_depth(), 1);
    history.undo(&mut doc).unwrap();
    assert_eq!(history.redo_depth(), 0);
    assert!(history.used_bytes() <= history.max_bytes());
}

#[test]
fn non_selection_transactions_restore_explicit_history_selection() {
    let mut doc = Document::from_paragraphs(["left", "right"]);
    let left = doc.blocks()[0].id;
    let right = doc.blocks()[1].id;
    let before = Selection::caret(DocPoint::new(left, 2));
    let mut history = History::new(1_000, 16 * 1024 * 1024);
    history
        .apply_with_selection(&mut doc, before, Transaction::MergeBlocks { left, right })
        .unwrap();
    assert_eq!(history.undo(&mut doc).unwrap(), before);

    let mut doc = Document::from_paragraphs(["left", "right"]);
    let left = doc.blocks()[0].id;
    let right = doc.blocks()[1].id;
    let before = Selection::caret(DocPoint::new(left, 1));
    let mut history = History::new(1_000, 16 * 1024 * 1024);
    history
        .apply_with_selection(&mut doc, before, Transaction::RemoveNode { node_id: right })
        .unwrap();
    assert_eq!(history.undo(&mut doc).unwrap(), before);

    let mut doc = Document::from_paragraph("text");
    let paragraph = doc.first_node_id().unwrap();
    doc.apply(Transaction::InsertImage {
        selection: Selection::caret(DocPoint::new(paragraph, 2)),
        resource_id: "width-image".into(),
        natural_size: (100, 100),
    })
    .unwrap();
    let image = doc.blocks()[1].id;
    let before = Selection::caret(DocPoint::new(paragraph, 1));
    let mut history = History::new(1_000, 16 * 1024 * 1024);
    history
        .apply_with_selection(
            &mut doc,
            before,
            Transaction::SetImageDisplayWidth {
                node_id: image,
                display_width: Some(80),
            },
        )
        .unwrap();
    assert_eq!(history.undo(&mut doc).unwrap(), before);
}

#[test]
fn small_edit_does_not_allocate_a_full_document_clone() {
    let mut doc = Document::from_paragraphs(
        (0..20_000).map(|index| format!("block-{index}-{}", "z".repeat(1_024))),
    );
    let node = doc.first_node_id().unwrap();
    let measurement = AllocationMeasurement::begin();
    doc.apply(Transaction::InsertText {
        selection: Selection::caret(DocPoint::new(node, 0)),
        text: "x".into(),
    })
    .unwrap();
    assert!(
        // Invariant validation and concurrently running tests allocate a
        // little noise; keep the limit well below the ~32 MiB full clone.
        measurement.bytes() < 24_000_000,
        "small edit allocated a full-document-sized candidate: {} bytes",
        measurement.bytes()
    );
}

#[test]
fn ordered_tail_edit_has_bounded_numbering_scratch_and_keeps_marker() {
    let blocks = (0..100_000)
        .map(|index| Block {
            id: NodeId::new((index + 1) as u64),
            kind: BlockKind::OrderedItem { depth: 0 },
            content: BlockContent::text(format!("ordered-item-{index}")),
            alignment: TextAlignment::Left,
            revision: 0,
        })
        .collect();
    let mut document = Document::from_blocks(blocks).expect("large ordered fixture");
    let tail = document.blocks().last().expect("tail item").clone();
    let mut layout = LayoutRegistry::new();
    layout.layout_document(&document, 0.0, 32.0, 120.0);
    assert_eq!(layout.ordered_number(tail.id), Some(100_000));
    let work_before = layout.ordered_number_work_count();

    let outcome = document
        .apply(Transaction::InsertText {
            selection: Selection::caret(DocPoint::with_affinity(
                tail.id,
                tail.content.as_text().expect("tail text").len(),
                Affinity::After,
            )),
            text: "!".into(),
        })
        .expect("tail inline edit");
    let measurement = AllocationMeasurement::begin();
    layout.invalidate_nodes_with_delta(
        &document,
        &outcome.changed_nodes,
        outcome.structural,
        &outcome.structural_splices,
        &outcome.numbering_ranges,
    );
    let scratch = measurement.bytes();
    let visited = layout
        .ordered_number_work_count()
        .saturating_sub(work_before);
    assert!(
        visited <= 32,
        "tail inline edit visited document-sized numbering state: {visited} nodes"
    );
    assert!(
        scratch < 512 * 1024,
        "tail inline edit allocated document-sized numbering scratch: {scratch} bytes"
    );
    assert_eq!(layout.ordered_number(tail.id), Some(100_000));
}

#[test]
fn ordered_tail_kind_depth_change_recomputes_locally_and_keeps_numbers_correct() {
    let blocks = (0..1_024)
        .map(|index| Block {
            id: NodeId::new((index + 1) as u64),
            kind: BlockKind::OrderedItem { depth: 0 },
            content: BlockContent::text(format!("ordered-item-{index}")),
            alignment: TextAlignment::Left,
            revision: 0,
        })
        .collect();
    let mut document = Document::from_blocks(blocks).expect("ordered fixture");
    let tail = document.blocks().last().expect("tail item").clone();
    let previous = document.blocks()[document.block_count() - 2].id;
    let mut layout = LayoutRegistry::new();
    layout.layout_document(&document, 0.0, 32.0, 120.0);
    assert_eq!(layout.ordered_number(previous), Some(1_023));
    assert_eq!(layout.ordered_number(tail.id), Some(1_024));
    let work_before = layout.ordered_number_work_count();

    let outcome = document
        .apply(Transaction::SetBlockKind {
            selection: Selection::new(
                DocPoint::with_affinity(tail.id, 0, Affinity::Before),
                DocPoint::with_affinity(
                    tail.id,
                    tail.content.as_text().expect("tail text").len(),
                    Affinity::After,
                ),
            ),
            kind: BlockKind::OrderedItem { depth: 1 },
        })
        .expect("tail depth change");
    assert!(outcome.structural);
    layout.invalidate_nodes_with_delta(
        &document,
        &outcome.changed_nodes,
        outcome.structural,
        &outcome.structural_splices,
        &outcome.numbering_ranges,
    );

    let visited = layout
        .ordered_number_work_count()
        .saturating_sub(work_before);
    assert!(
        visited <= 64,
        "tail kind/depth edit recomputed beyond its checkpoint: {visited} nodes"
    );
    assert_eq!(layout.ordered_number(previous), Some(1_023));
    assert_eq!(layout.ordered_number(tail.id), Some(1));
}

#[test]
fn structural_tail_return_merge_undo_redo_do_not_rebuild_numbering_for_100k_blocks() {
    let blocks = (0..100_000)
        .map(|index| Block {
            id: NodeId::new((index + 1) as u64),
            kind: BlockKind::Paragraph,
            content: BlockContent::text(format!("paragraph-{index}")),
            alignment: TextAlignment::Left,
            revision: 0,
        })
        .collect();
    let mut document = Document::from_blocks(blocks).expect("large plain fixture");
    let mut history = History::new(32, 4 * 1024 * 1024);
    let mut layout = LayoutRegistry::new();
    layout.layout_document(&document, 0.0, 32.0, 120.0);
    let work_before = layout.ordered_number_work_count();
    let tail = document.blocks().last().expect("tail paragraph").clone();
    let before_selection = document.end_selection();
    let splice_work_before = layout.ordered_splice_operation_count();
    let measurement = AllocationMeasurement::begin();
    let outcome = history
        .apply_with_selection(
            &mut document,
            before_selection,
            Transaction::SplitBlock {
                at: DocPoint::with_affinity(tail.id, 0, Affinity::Before),
            },
        )
        .expect("tail Return");
    assert_eq!(outcome.structural_splices.len(), 1);
    assert_eq!(outcome.structural_splices[0].start_index, 99_999);
    assert_eq!(outcome.structural_splices[0].removed.len(), 1);
    assert_eq!(outcome.structural_splices[0].inserted.len(), 2);
    layout.invalidate_nodes_with_delta(
        &document,
        &outcome.changed_nodes,
        outcome.structural,
        &outcome.structural_splices,
        &outcome.numbering_ranges,
    );
    let scratch = measurement.bytes();
    assert_eq!(document.block_count(), 100_001);
    assert!(
        layout
            .ordered_number_work_count()
            .saturating_sub(work_before)
            <= 256,
        "tail Return rebuilt numbering for the whole document"
    );
    let splice_work = layout
        .ordered_splice_operation_count()
        .saturating_sub(splice_work_before);
    assert_eq!(splice_work, 1, "tail Return uses one local tree splice");
    assert!(
        scratch < 512 * 1024,
        "tail Return splice scratch grew with the 100k-block suffix: {scratch} bytes"
    );

    let right = document.blocks().last().expect("split tail").id;
    let work_before = layout.ordered_number_work_count();
    let before_selection = document.end_selection();
    let splice_work_before = layout.ordered_splice_operation_count();
    let measurement = AllocationMeasurement::begin();
    let outcome = history
        .apply_with_selection(
            &mut document,
            before_selection,
            Transaction::MergeBlocks {
                left: tail.id,
                right,
            },
        )
        .expect("tail Backspace merge");
    assert_eq!(outcome.structural_splices.len(), 1);
    assert_eq!(outcome.structural_splices[0].start_index, 99_999);
    assert_eq!(outcome.structural_splices[0].removed.len(), 2);
    assert_eq!(outcome.structural_splices[0].inserted.len(), 1);
    layout.invalidate_nodes_with_delta(
        &document,
        &outcome.changed_nodes,
        outcome.structural,
        &outcome.structural_splices,
        &outcome.numbering_ranges,
    );
    let scratch = measurement.bytes();
    assert_eq!(document.block_count(), 100_000);
    assert!(
        layout
            .ordered_number_work_count()
            .saturating_sub(work_before)
            <= 256,
        "tail Backspace merge rebuilt numbering for the whole document"
    );
    let splice_work = layout
        .ordered_splice_operation_count()
        .saturating_sub(splice_work_before);
    assert_eq!(splice_work, 1, "tail Backspace uses one local tree splice");
    assert!(
        scratch < 512 * 1024,
        "tail Backspace splice scratch grew with the 100k-block suffix: {scratch} bytes"
    );

    let work_before = layout.ordered_number_work_count();
    let splice_work_before = layout.ordered_splice_operation_count();
    let measurement = AllocationMeasurement::begin();
    let outcome = history
        .undo_with_outcome(&mut document)
        .expect("undo merge");
    assert_eq!(outcome.structural_splices.len(), 1);
    assert_eq!(outcome.structural_splices[0].start_index, 99_999);
    assert_eq!(outcome.structural_splices[0].removed.len(), 1);
    assert_eq!(outcome.structural_splices[0].inserted.len(), 2);
    layout.invalidate_nodes_with_delta(
        &document,
        &outcome.changed_nodes,
        outcome.structural,
        &outcome.structural_splices,
        &outcome.numbering_ranges,
    );
    let scratch = measurement.bytes();
    assert_eq!(document.block_count(), 100_001);
    assert!(
        layout
            .ordered_number_work_count()
            .saturating_sub(work_before)
            <= 256,
        "undo merge rebuilt numbering for the whole document"
    );
    let splice_work = layout
        .ordered_splice_operation_count()
        .saturating_sub(splice_work_before);
    assert_eq!(splice_work, 1, "undo merge uses one local tree splice");
    assert!(
        scratch < 512 * 1024,
        "undo merge splice scratch grew with the 100k-block suffix: {scratch} bytes"
    );

    let work_before = layout.ordered_number_work_count();
    let splice_work_before = layout.ordered_splice_operation_count();
    let measurement = AllocationMeasurement::begin();
    let outcome = history
        .redo_with_outcome(&mut document)
        .expect("redo merge");
    assert_eq!(outcome.structural_splices.len(), 1);
    assert_eq!(outcome.structural_splices[0].start_index, 99_999);
    assert_eq!(outcome.structural_splices[0].removed.len(), 2);
    assert_eq!(outcome.structural_splices[0].inserted.len(), 1);
    layout.invalidate_nodes_with_delta(
        &document,
        &outcome.changed_nodes,
        outcome.structural,
        &outcome.structural_splices,
        &outcome.numbering_ranges,
    );
    let scratch = measurement.bytes();
    assert_eq!(document.block_count(), 100_000);
    assert!(
        layout
            .ordered_number_work_count()
            .saturating_sub(work_before)
            <= 256,
        "redo merge rebuilt numbering for the whole document"
    );
    let splice_work = layout
        .ordered_splice_operation_count()
        .saturating_sub(splice_work_before);
    assert_eq!(splice_work, 1, "redo merge uses one local tree splice");
    assert!(
        scratch < 512 * 1024,
        "redo merge splice scratch grew with the 100k-block suffix: {scratch} bytes"
    );
}

#[test]
fn batched_splices_and_restore_blocks_replay_in_order_through_history() {
    let mut document = Document::from_paragraphs(["aa", "bb", "cc"]);
    let first = document.blocks()[0].clone();
    let second = document.blocks()[1].clone();
    let mut history = History::new(32, 4 * 1024 * 1024);
    let mut layout = LayoutRegistry::new();
    layout.layout_document(&document, 0.0, 32.0, 120.0);

    let batch = TransactionBatch(vec![
        Transaction::SplitBlock {
            at: DocPoint::with_affinity(first.id, 1, Affinity::After),
        },
        Transaction::SplitBlock {
            at: DocPoint::with_affinity(second.id, 1, Affinity::After),
        },
    ]);
    let batch_selection = document.end_selection();
    let outcome = history
        .apply_batch_with_selection(&mut document, batch_selection, batch)
        .expect("batched splits");
    assert_eq!(outcome.structural_splices.len(), 2);
    assert_eq!(outcome.structural_splices[0].start_index, 0);
    assert_eq!(
        outcome.structural_splices[0].removed.as_slice(),
        &[first.id]
    );
    assert_eq!(outcome.structural_splices[0].inserted.len(), 2);
    assert_eq!(outcome.structural_splices[1].start_index, 2);
    assert_eq!(
        outcome.structural_splices[1].removed.as_slice(),
        &[second.id]
    );
    assert_eq!(outcome.structural_splices[1].inserted.len(), 2);
    layout.invalidate_nodes_with_delta(
        &document,
        &outcome.changed_nodes,
        outcome.structural,
        &outcome.structural_splices,
        &outcome.numbering_ranges,
    );
    assert_eq!(document.block_count(), 5);

    let replacement = Block {
        id: NodeId::new(10_000),
        kind: BlockKind::Paragraph,
        content: BlockContent::text("restored"),
        alignment: TextAlignment::Left,
        revision: 0,
    };
    let restore = Transaction::RestoreBlocks {
        index: 1,
        remove_count: 2,
        blocks: vec![replacement.clone()],
    };
    let restore_selection = document.end_selection();
    let outcome = history
        .apply_batch_with_selection(
            &mut document,
            restore_selection,
            TransactionBatch(vec![restore]),
        )
        .expect("RestoreBlocks splice");
    assert_eq!(outcome.structural_splices.len(), 1);
    assert_eq!(outcome.structural_splices[0].start_index, 1);
    assert_eq!(outcome.structural_splices[0].removed.len(), 2);
    assert_eq!(
        outcome.structural_splices[0].inserted.as_slice(),
        &[replacement.id]
    );
    layout.invalidate_nodes_with_delta(
        &document,
        &outcome.changed_nodes,
        outcome.structural,
        &outcome.structural_splices,
        &outcome.numbering_ranges,
    );
    assert_eq!(document.blocks()[1].id, replacement.id);

    let outcome = history
        .undo_with_outcome(&mut document)
        .expect("undo RestoreBlocks");
    assert_eq!(outcome.structural_splices.len(), 1);
    assert_eq!(outcome.structural_splices[0].start_index, 1);
    assert_eq!(outcome.structural_splices[0].removed.len(), 1);
    assert_eq!(outcome.structural_splices[0].inserted.len(), 2);
    layout.invalidate_nodes_with_delta(
        &document,
        &outcome.changed_nodes,
        outcome.structural,
        &outcome.structural_splices,
        &outcome.numbering_ranges,
    );
    assert_ne!(document.blocks()[1].id, replacement.id);

    let outcome = history
        .redo_with_outcome(&mut document)
        .expect("redo RestoreBlocks");
    assert_eq!(outcome.structural_splices.len(), 1);
    assert_eq!(outcome.structural_splices[0].start_index, 1);
    assert_eq!(outcome.structural_splices[0].removed.len(), 2);
    assert_eq!(outcome.structural_splices[0].inserted.len(), 1);
    layout.invalidate_nodes_with_delta(
        &document,
        &outcome.changed_nodes,
        outcome.structural,
        &outcome.structural_splices,
        &outcome.numbering_ranges,
    );
    assert_eq!(document.blocks()[1].id, replacement.id);
}

#[test]
fn restore_blocks_rejects_a_range_external_node_id_collision_atomically() {
    let mut document = Document::from_paragraphs(["left", "middle", "right"]);
    let original = document.clone();
    let outside = document.blocks()[0].clone();

    let result = document.apply(Transaction::RestoreBlocks {
        index: 1,
        remove_count: 1,
        blocks: vec![outside],
    });

    assert!(matches!(
        result,
        Err(DocumentError::InvalidOperation(message))
            if message.contains("duplicate a node id")
    ));
    assert_eq!(document, original);
    document.validate_invariants().expect("collision rollback");
}

#[test]
fn restore_blocks_rejects_zero_and_internal_duplicate_ids_atomically() {
    let mut document = Document::from_paragraphs(["left", "right"]);
    let original = document.clone();
    let zero = Block {
        id: NodeId::new(0),
        kind: BlockKind::Paragraph,
        content: BlockContent::text("zero"),
        alignment: TextAlignment::Left,
        revision: 0,
    };
    let result = document.apply(Transaction::RestoreBlocks {
        index: 1,
        remove_count: 1,
        blocks: vec![zero],
    });
    assert!(matches!(
        result,
        Err(DocumentError::InvalidOperation(message))
            if message.contains("duplicate or zero")
    ));
    assert_eq!(document, original);

    let duplicate = document.blocks()[0].clone();
    let result = document.apply(Transaction::RestoreBlocks {
        index: 1,
        remove_count: 1,
        blocks: vec![duplicate.clone(), duplicate],
    });
    assert!(matches!(
        result,
        Err(DocumentError::InvalidOperation(message))
            if message.contains("duplicate or zero")
    ));
    assert_eq!(document, original);
    document.validate_invariants().expect("restore validation");
}

#[test]
fn failed_history_batch_restores_selection_and_allocator_exactly() {
    let mut document = Document::from_paragraph("ab");
    let mut history = History::new(32, 4 * 1024 * 1024);
    let original = document.clone();
    let original_selection = document.end_selection();
    let node = document.first_node_id().expect("batch node");
    let result = history.apply_batch_with_selection(
        &mut document,
        original_selection,
        TransactionBatch(vec![
            Transaction::SplitBlock {
                at: DocPoint::new(node, 1),
            },
            Transaction::InsertText {
                selection: Selection::caret(DocPoint::new(node, 99)),
                text: "must-not-commit".into(),
            },
        ]),
    );
    assert!(matches!(
        result,
        Err(DocumentError::InvalidUtf8Offset { .. })
            | Err(DocumentError::InvalidGraphemeOffset { .. })
    ));
    assert_eq!(document, original);
    assert_eq!(document.end_selection(), original_selection);
    assert_eq!(history.undo_depth(), 0);
    assert_eq!(history.redo_depth(), 0);
    document
        .apply(Transaction::SplitBlock {
            at: DocPoint::new(node, 1),
        })
        .expect("allocator cursor remains reusable");
    assert_eq!(document.blocks()[1].id, NodeId::new(2));
}

#[test]
fn repeated_local_splices_stay_valid_without_order_buffer_capacity() {
    let mut document = Document::from_paragraph("seed");
    for _ in 0..8 {
        let tail = document.blocks().last().expect("tail block").clone();
        document
            .apply(Transaction::SplitBlock {
                at: DocPoint::with_affinity(
                    tail.id,
                    tail.content.as_text().expect("tail text").len(),
                    Affinity::After,
                ),
            })
            .expect("consume structural spare slot");
    }
    assert_eq!(document.block_count(), 9);
    document
        .validate_invariants()
        .expect("repeated structural insertions remain valid");
}

#[test]
fn block_sequence_index_lookup_uses_right_boundary() {
    let document = Document::from_paragraphs(["zero", "one", "last"]);
    assert_eq!(
        super::model::BlockSequence::item_overhead_bytes_for_test(),
        size_of::<Arc<Block>>(),
        "SumTree items retain one Arc pointer; Block payload stays shared"
    );
    assert_eq!(document.blocks()[0].content.as_text(), Some("zero"));
    assert_eq!(document.blocks()[1].content.as_text(), Some("one"));
    assert_eq!(
        document.blocks()[document.block_count() - 1]
            .content
            .as_text(),
        Some("last")
    );
}

#[test]
fn capacity_exhaustion_does_not_copy_a_100k_block_order_buffer() {
    let blocks = (0..100_000)
        .map(|index| Block {
            id: NodeId::new((index + 1) as u64),
            kind: BlockKind::Paragraph,
            content: BlockContent::text(format!("paragraph-{index}")),
            alignment: TextAlignment::Left,
            revision: 0,
        })
        .collect();
    let mut document = Document::from_blocks(blocks).expect("large fixture");
    let measurement = AllocationMeasurement::begin();
    for _ in 0..5 {
        let tail = document.blocks().last().expect("tail block").clone();
        document
            .apply(Transaction::SplitBlock {
                at: DocPoint::with_affinity(
                    tail.id,
                    tail.content.as_text().expect("tail text").len(),
                    Affinity::After,
                ),
            })
            .expect("tail split");
    }
    assert_eq!(document.block_count(), 100_005);
    let allocated = measurement.bytes();
    assert!(
        allocated < 2 * 1024 * 1024,
        "five tail splits allocated document-proportional order storage: {allocated} bytes"
    );
    document.validate_invariants().expect("100k tail splits");
}

#[derive(Clone)]
struct OracleMergeState {
    index: usize,
    left: Block,
    right: Block,
}

fn assert_sum_tree_matches_oracle(
    document: &Document,
    oracle: &[Block],
    layout: &LayoutRegistry,
    context: &str,
) {
    assert_eq!(document.block_count(), oracle.len(), "{context}: count");
    let mut ids = std::collections::HashSet::with_capacity(oracle.len());
    for (actual, expected) in document.blocks().iter().zip(oracle) {
        assert_eq!(actual.id, expected.id, "{context}: order");
        assert_eq!(actual.kind, expected.kind, "{context}: kind");
        assert_eq!(actual.content, expected.content, "{context}: content");
        assert_eq!(actual.alignment, expected.alignment, "{context}: alignment");
        assert!(
            ids.insert(actual.id),
            "{context}: duplicate {:?}",
            actual.id
        );
    }
    document
        .validate_invariants()
        .unwrap_or_else(|error| panic!("{context}: invariant {error}"));
    assert_ordered_numbers_match(document, layout, context);
}

fn run_mixed_sum_tree_fixture(block_count: usize) -> (usize, usize) {
    let blocks = (0..block_count)
        .map(|index| Block {
            id: NodeId::new((index + 1) as u64),
            kind: if index < 128 {
                BlockKind::OrderedItem { depth: 0 }
            } else {
                BlockKind::Paragraph
            },
            content: BlockContent::text(format!("row-{index}")),
            alignment: TextAlignment::Left,
            revision: 0,
        })
        .collect::<Vec<_>>();
    let mut oracle = blocks.clone();
    let mut document = Document::from_blocks(blocks).expect("mixed SumTree fixture");
    let mut history = History::new(64, 8 * 1024 * 1024);
    let mut layout = LayoutRegistry::new();
    layout.layout_document(&document, 0.0, 32.0, 120.0);
    assert_sum_tree_matches_oracle(&document, &oracle, &layout, "mixed initial");

    let mut max_allocation = 0usize;
    let mut max_number_work = 0usize;
    for cycle in 0..5 {
        let split_id = if cycle % 2 == 0 {
            oracle[block_count / 2].id
        } else {
            oracle.last().expect("tail fixture").id
        };
        let split_index = oracle
            .iter()
            .position(|block| block.id == split_id)
            .expect("split id in oracle");
        let before_undo = history.undo_depth();
        let before_work = layout.ordered_number_work_count();
        let before_operations = layout.ordered_splice_operation_count();
        let split_point = DocPoint::with_affinity(split_id, 1, Affinity::After);
        let (split_outcome, allocation) = apply_history_and_measure(
            &mut document,
            &mut history,
            &mut layout,
            Selection::caret(split_point),
            Transaction::SplitBlock { at: split_point },
        );
        max_allocation = max_allocation.max(allocation);
        max_number_work = max_number_work.max(
            layout
                .ordered_number_work_count()
                .saturating_sub(before_work),
        );
        assert_eq!(
            layout
                .ordered_splice_operation_count()
                .saturating_sub(before_operations),
            1,
            "split uses one SumTree splice"
        );
        assert_eq!(history.undo_depth(), before_undo + 1);
        assert_eq!(history.redo_depth(), 0);
        document
            .validate_selection(split_outcome.selection)
            .expect("split selection remains valid");
        assert_eq!(split_outcome.structural_splices.len(), 1);
        let split_splice = &split_outcome.structural_splices[0];
        assert_eq!(split_splice.start_index, split_index);
        let old = oracle.remove(split_index);
        let text = old.content.as_text().expect("plain fixture").to_owned();
        let (left_text, right_text) = text.split_at(1);
        let mut left = old.clone();
        left.content = BlockContent::text(left_text);
        let mut right = old;
        right.id = split_splice.inserted[1];
        right.content = BlockContent::text(right_text);
        oracle.splice(split_index..split_index, [left, right]);
        assert_sum_tree_matches_oracle(&document, &oracle, &layout, "after split");

        let right_id = split_splice.inserted[1];
        let merge_index = oracle
            .iter()
            .position(|block| block.id == split_id)
            .expect("merged left id in oracle");
        assert_eq!(oracle[merge_index + 1].id, right_id);
        let merge_state = OracleMergeState {
            index: merge_index,
            left: oracle[merge_index].clone(),
            right: oracle[merge_index + 1].clone(),
        };
        let before_undo = history.undo_depth();
        let before_work = layout.ordered_number_work_count();
        let before_operations = layout.ordered_splice_operation_count();
        let merge_selection = document.end_selection();
        let (merge_outcome, allocation) = apply_history_and_measure(
            &mut document,
            &mut history,
            &mut layout,
            merge_selection,
            Transaction::MergeBlocks {
                left: split_id,
                right: right_id,
            },
        );
        max_allocation = max_allocation.max(allocation);
        max_number_work = max_number_work.max(
            layout
                .ordered_number_work_count()
                .saturating_sub(before_work),
        );
        assert_eq!(
            layout
                .ordered_splice_operation_count()
                .saturating_sub(before_operations),
            1,
            "merge uses one SumTree splice"
        );
        assert_eq!(history.undo_depth(), before_undo + 1);
        assert_eq!(history.redo_depth(), 0);
        document
            .validate_selection(merge_outcome.selection)
            .expect("merge selection remains valid");
        let mut merged = merge_state.left.clone();
        let mut merged_text = merge_state
            .left
            .content
            .as_text()
            .expect("left plain fixture")
            .to_owned();
        merged_text.push_str(
            merge_state
                .right
                .content
                .as_text()
                .expect("right plain fixture"),
        );
        merged.content = BlockContent::text(merged_text);
        oracle.splice(merge_state.index..merge_state.index + 2, [merged]);
        assert_sum_tree_matches_oracle(&document, &oracle, &layout, "after merge");

        let before_undo = history.undo_depth();
        let before_work = layout.ordered_number_work_count();
        let before_operations = layout.ordered_splice_operation_count();
        let measurement = AllocationMeasurement::begin();
        let undo_outcome = history
            .undo_with_outcome(&mut document)
            .expect("undo merge");
        layout.invalidate_nodes_with_delta(
            &document,
            &undo_outcome.changed_nodes,
            undo_outcome.structural,
            &undo_outcome.structural_splices,
            &undo_outcome.numbering_ranges,
        );
        max_allocation = max_allocation.max(measurement.bytes());
        max_number_work = max_number_work.max(
            layout
                .ordered_number_work_count()
                .saturating_sub(before_work),
        );
        assert_eq!(
            layout
                .ordered_splice_operation_count()
                .saturating_sub(before_operations),
            1,
            "undo uses one SumTree splice"
        );
        assert_eq!(history.undo_depth(), before_undo - 1);
        assert_eq!(history.redo_depth(), 1);
        document
            .validate_selection(undo_outcome.selection)
            .expect("undo selection remains valid");
        oracle.splice(
            merge_state.index..merge_state.index + 1,
            [merge_state.left.clone(), merge_state.right.clone()],
        );
        assert_sum_tree_matches_oracle(&document, &oracle, &layout, "after undo");

        let before_undo = history.undo_depth();
        let before_work = layout.ordered_number_work_count();
        let before_operations = layout.ordered_splice_operation_count();
        let measurement = AllocationMeasurement::begin();
        let redo_outcome = history
            .redo_with_outcome(&mut document)
            .expect("redo merge");
        layout.invalidate_nodes_with_delta(
            &document,
            &redo_outcome.changed_nodes,
            redo_outcome.structural,
            &redo_outcome.structural_splices,
            &redo_outcome.numbering_ranges,
        );
        max_allocation = max_allocation.max(measurement.bytes());
        max_number_work = max_number_work.max(
            layout
                .ordered_number_work_count()
                .saturating_sub(before_work),
        );
        assert_eq!(
            layout
                .ordered_splice_operation_count()
                .saturating_sub(before_operations),
            1,
            "redo uses one SumTree splice"
        );
        assert_eq!(history.undo_depth(), before_undo + 1);
        assert_eq!(history.redo_depth(), 0);
        document
            .validate_selection(redo_outcome.selection)
            .expect("redo selection remains valid");
        let mut merged = merge_state.left.clone();
        let mut merged_text = merge_state
            .left
            .content
            .as_text()
            .expect("left plain fixture")
            .to_owned();
        merged_text.push_str(
            merge_state
                .right
                .content
                .as_text()
                .expect("right plain fixture"),
        );
        merged.content = BlockContent::text(merged_text);
        oracle.splice(merge_state.index..merge_state.index + 2, [merged]);
        assert_sum_tree_matches_oracle(&document, &oracle, &layout, "after redo");
    }

    println!(
        "mixed SumTree fixture {block_count}: max_path_allocation={max_allocation} max_number_work={max_number_work}"
    );
    assert!(
        max_allocation < 4 * 1024 * 1024,
        "mixed {block_count}-block paths allocated document-proportional storage: {max_allocation}"
    );
    assert!(
        max_number_work <= 512,
        "mixed {block_count}-block paths scanned too far: {max_number_work}"
    );
    (max_allocation, max_number_work)
}

#[test]
fn mixed_sum_tree_paths_scale_on_10k_and_100k_fixtures() {
    let (small_allocation, small_work) = run_mixed_sum_tree_fixture(10_000);
    let (large_allocation, large_work) = run_mixed_sum_tree_fixture(100_000);
    assert!(
        large_allocation <= 2 * small_allocation.max(1) + 512 * 1024,
        "100k path allocation grew with document size: 10k={small_allocation}, 100k={large_allocation}"
    );
    assert!(
        large_work <= small_work.saturating_add(256),
        "100k numbering work grew with document size: 10k={small_work}, 100k={large_work}"
    );
}

#[test]
fn max_node_id_cannot_be_allocated_again() {
    let block = Block {
        id: NodeId::new(u64::MAX),
        kind: BlockKind::Paragraph,
        content: BlockContent::text("max"),
        alignment: TextAlignment::Left,
        revision: 0,
    };
    let mut document = Document::from_blocks(vec![block.clone()]).expect("max id is readable");
    let original = document.clone();
    let result = document.apply(Transaction::SplitBlock {
        at: DocPoint::with_affinity(block.id, 1, Affinity::After),
    });
    assert!(
        result.is_err(),
        "fresh id exhaustion must fail before mutation"
    );
    assert_eq!(document, original);

    // The image insertion path needs two fresh identities.  Starting after
    // this block makes the first allocation valid and the second exhausted;
    // that failure must happen before deleting or splitting the paragraph.
    let paragraph_block = Block {
        id: NodeId::new(u64::MAX - 2),
        kind: BlockKind::Paragraph,
        content: BlockContent::text("before"),
        alignment: TextAlignment::Left,
        revision: 0,
    };
    let mut document = Document::from_blocks(vec![paragraph_block]).expect("near-max id is valid");
    let original = document.clone();
    let paragraph = document.blocks()[0].clone();
    let result = document.apply(Transaction::InsertImage {
        selection: Selection::caret(DocPoint::with_affinity(paragraph.id, 3, Affinity::After)),
        resource_id: "exhausted-image".into(),
        natural_size: (100, 100),
    });
    assert!(
        result.is_err(),
        "image insertion must preflight both fresh ids"
    );
    assert_eq!(document, original);
}

#[test]
fn same_id_restore_reports_non_structural_metadata() {
    let mut document = Document::from_paragraph("before");
    let original = document.blocks()[0].clone();
    let replacement = Block {
        id: original.id,
        kind: original.kind.clone(),
        content: BlockContent::text("after"),
        alignment: original.alignment,
        revision: original.revision,
    };
    let outcome = document
        .apply(Transaction::RestoreBlocks {
            index: 0,
            remove_count: 1,
            blocks: vec![replacement],
        })
        .expect("same-id content restore");
    assert!(!outcome.structural);
    assert!(outcome.structural_splices.is_empty());
}

#[test]
fn restore_at_checkpoint_boundary_round_trips_numbering() {
    restore_checkpoint_boundary_fixture(65, 64);
    restore_checkpoint_boundary_fixture(129, 128);
}

fn restore_checkpoint_boundary_fixture(block_count: usize, index: usize) {
    let mut oracle = ordered_fixture(block_count);
    let mut document = Document::from_blocks(oracle.clone()).expect("checkpoint fixture");
    let mut history = History::new(32, 4 * 1024 * 1024);
    let mut layout = LayoutRegistry::new();
    layout.layout_document(&document, 0.0, 32.0, 120.0);
    assert_sum_tree_matches_oracle(&document, &oracle, &layout, "initial restore fixture");

    let removed = oracle[index].clone();
    let before_selection = document.end_selection();
    let remove = history
        .apply_with_selection(
            &mut document,
            before_selection,
            Transaction::RestoreBlocks {
                index,
                remove_count: 1,
                blocks: Vec::new(),
            },
        )
        .expect("remove checkpoint item");
    layout.invalidate_nodes_with_delta(
        &document,
        &remove.changed_nodes,
        remove.structural,
        &remove.structural_splices,
        &remove.numbering_ranges,
    );
    oracle.remove(index);
    assert_eq!(document.block_count(), block_count - 1);
    assert_sum_tree_matches_oracle(&document, &oracle, &layout, "after restore remove");

    let undone = history
        .undo_with_outcome(&mut document)
        .expect("undo remove to 65");
    layout.invalidate_nodes_with_delta(
        &document,
        &undone.changed_nodes,
        undone.structural,
        &undone.structural_splices,
        &undone.numbering_ranges,
    );
    oracle.insert(index, removed.clone());
    assert_eq!(document.blocks()[index].id, removed.id);
    assert_eq!(layout.ordered_number(removed.id), Some(index + 1));
    assert_sum_tree_matches_oracle(&document, &oracle, &layout, "after restore undo");

    let redone = history
        .redo_with_outcome(&mut document)
        .expect("redo restore remove");
    layout.invalidate_nodes_with_delta(
        &document,
        &redone.changed_nodes,
        redone.structural,
        &redone.structural_splices,
        &redone.numbering_ranges,
    );
    oracle.remove(index);
    assert_eq!(document.block_count(), block_count - 1);
    assert_sum_tree_matches_oracle(&document, &oracle, &layout, "after restore redo");
}

#[test]
fn plain_middle_splice_and_same_id_restore_stay_local() {
    const PREFIX_COUNT: usize = 64;
    const MIDDLE_COUNT: usize = 100_000;
    const SUFFIX_COUNT: usize = 64;
    const MIDDLE_INDEX: usize = PREFIX_COUNT + MIDDLE_COUNT / 2;

    let mut blocks = Vec::with_capacity(PREFIX_COUNT + MIDDLE_COUNT + SUFFIX_COUNT);
    for index in 0..PREFIX_COUNT {
        blocks.push(Block {
            id: NodeId::new((index + 1) as u64),
            kind: BlockKind::OrderedItem { depth: 0 },
            content: BlockContent::text(format!("prefix-{index}")),
            alignment: TextAlignment::Left,
            revision: 0,
        });
    }
    for index in 0..MIDDLE_COUNT {
        blocks.push(Block {
            id: NodeId::new((PREFIX_COUNT + index + 1) as u64),
            kind: BlockKind::Paragraph,
            content: BlockContent::text(format!("middle-{index}")),
            alignment: TextAlignment::Left,
            revision: 0,
        });
    }
    for index in 0..SUFFIX_COUNT {
        blocks.push(Block {
            id: NodeId::new((PREFIX_COUNT + MIDDLE_COUNT + index + 1) as u64),
            kind: BlockKind::OrderedItem { depth: 0 },
            content: BlockContent::text(format!("suffix-{index}")),
            alignment: TextAlignment::Left,
            revision: 0,
        });
    }
    let mut document = Document::from_blocks(blocks).expect("plain middle fixture");
    let middle = document.blocks()[MIDDLE_INDEX].id;
    let suffix_ids = document
        .blocks()
        .iter_range(PREFIX_COUNT + MIDDLE_COUNT..document.block_count())
        .map(|block| block.id)
        .collect::<Vec<_>>();
    let mut history = History::new(32, 4 * 1024 * 1024);
    let mut layout = LayoutRegistry::new();
    layout.layout_document(&document, 0.0, 32.0, 120.0);

    let assert_suffix = |document: &Document, layout: &LayoutRegistry| {
        let oracle = ordered_number_summary(document);
        for node_id in &suffix_ids {
            assert_eq!(
                layout.ordered_number(*node_id),
                oracle.get(node_id).copied(),
                "suffix numbering changed for {node_id:?}"
            );
        }
    };
    assert_suffix(&document, &layout);

    let split_point = DocPoint::with_affinity(middle, 1, Affinity::After);
    let work_before = layout.ordered_number_work_count();
    let (outcome, allocation) = apply_history_and_measure(
        &mut document,
        &mut history,
        &mut layout,
        Selection::caret(split_point),
        Transaction::SplitBlock { at: split_point },
    );
    assert_eq!(outcome.structural_splices.len(), 1);
    assert!(
        layout
            .ordered_number_work_count()
            .saturating_sub(work_before)
            <= 256,
        "plain middle Return scanned the distant suffix"
    );
    assert!(
        allocation < 24 * 1024 * 1024,
        "plain middle Return transaction/delta/layout allocation grew with the document: {allocation}"
    );
    assert_suffix(&document, &layout);

    let right = document.blocks()[MIDDLE_INDEX + 1].id;
    let merge_selection = Selection::caret(DocPoint::with_affinity(right, 0, Affinity::Before));
    let work_before = layout.ordered_number_work_count();
    let (outcome, allocation) = apply_history_and_measure(
        &mut document,
        &mut history,
        &mut layout,
        merge_selection,
        Transaction::MergeBlocks {
            left: middle,
            right,
        },
    );
    assert_eq!(outcome.structural_splices.len(), 1);
    assert!(
        layout
            .ordered_number_work_count()
            .saturating_sub(work_before)
            <= 256,
        "plain middle Backspace merge scanned the distant suffix"
    );
    assert!(
        allocation < 24 * 1024 * 1024,
        "plain middle Backspace transaction/delta/layout allocation grew with the document: {allocation}"
    );
    assert_suffix(&document, &layout);

    let work_before = layout.ordered_number_work_count();
    let measurement = AllocationMeasurement::begin();
    let outcome = history
        .undo_with_outcome(&mut document)
        .expect("undo middle merge");
    layout.invalidate_nodes_with_delta(
        &document,
        &outcome.changed_nodes,
        outcome.structural,
        &outcome.structural_splices,
        &outcome.numbering_ranges,
    );
    let allocation = measurement.bytes();
    assert_eq!(outcome.structural_splices.len(), 1);
    assert!(
        layout
            .ordered_number_work_count()
            .saturating_sub(work_before)
            <= 256,
        "plain middle undo scanned the distant suffix"
    );
    assert!(
        allocation < 24 * 1024 * 1024,
        "plain middle undo transaction/delta/layout allocation grew with the document: {allocation}"
    );
    assert_suffix(&document, &layout);

    let work_before = layout.ordered_number_work_count();
    let measurement = AllocationMeasurement::begin();
    let outcome = history
        .redo_with_outcome(&mut document)
        .expect("redo middle merge");
    layout.invalidate_nodes_with_delta(
        &document,
        &outcome.changed_nodes,
        outcome.structural,
        &outcome.structural_splices,
        &outcome.numbering_ranges,
    );
    let allocation = measurement.bytes();
    assert_eq!(outcome.structural_splices.len(), 1);
    assert!(
        layout
            .ordered_number_work_count()
            .saturating_sub(work_before)
            <= 256,
        "plain middle redo scanned the distant suffix"
    );
    assert!(
        allocation < 24 * 1024 * 1024,
        "plain middle redo transaction/delta/layout allocation grew with the document: {allocation}"
    );
    assert_suffix(&document, &layout);

    let inline_selection = Selection::new(
        DocPoint::with_affinity(middle, 0, Affinity::Before),
        DocPoint::with_affinity(middle, 1, Affinity::After),
    );
    let (outcome, _) = apply_history_and_measure(
        &mut document,
        &mut history,
        &mut layout,
        inline_selection,
        Transaction::ToggleMark {
            selection: inline_selection,
            mark: Mark::Bold,
        },
    );
    assert!(outcome.structural_splices.is_empty());
    let work_before = layout.ordered_number_work_count();
    let measurement = AllocationMeasurement::begin();
    let outcome = history
        .undo_with_outcome(&mut document)
        .expect("undo inline mark");
    layout.invalidate_nodes_with_delta(
        &document,
        &outcome.changed_nodes,
        outcome.structural,
        &outcome.structural_splices,
        &outcome.numbering_ranges,
    );
    let allocation = measurement.bytes();
    assert!(outcome.structural_splices.is_empty());
    assert!(
        layout
            .ordered_number_work_count()
            .saturating_sub(work_before)
            <= 4,
        "same-ID inline undo scanned the distant suffix"
    );
    assert!(
        allocation < 24 * 1024 * 1024,
        "same-ID inline undo transaction/delta/layout allocation grew with the document: {allocation}"
    );
    assert_suffix(&document, &layout);

    let (outcome, _) = apply_history_and_measure(
        &mut document,
        &mut history,
        &mut layout,
        inline_selection,
        Transaction::SetLink {
            selection: inline_selection,
            url: Some("https://example.test".into()),
        },
    );
    assert!(outcome.structural_splices.is_empty());
    let work_before = layout.ordered_number_work_count();
    let measurement = AllocationMeasurement::begin();
    let outcome = history
        .undo_with_outcome(&mut document)
        .expect("undo inline link");
    layout.invalidate_nodes_with_delta(
        &document,
        &outcome.changed_nodes,
        outcome.structural,
        &outcome.structural_splices,
        &outcome.numbering_ranges,
    );
    let allocation = measurement.bytes();
    assert!(outcome.structural_splices.is_empty());
    assert!(
        layout
            .ordered_number_work_count()
            .saturating_sub(work_before)
            <= 4,
        "same-ID link undo scanned the distant suffix"
    );
    assert!(
        allocation < 24 * 1024 * 1024,
        "same-ID link undo transaction/delta/layout allocation grew with the document: {allocation}"
    );
    assert_suffix(&document, &layout);

    let (outcome, _) = apply_history_and_measure(
        &mut document,
        &mut history,
        &mut layout,
        inline_selection,
        Transaction::SetAlignment {
            selection: inline_selection,
            alignment: TextAlignment::Center,
        },
    );
    assert!(outcome.structural_splices.is_empty());
    let work_before = layout.ordered_number_work_count();
    let measurement = AllocationMeasurement::begin();
    let outcome = history
        .undo_with_outcome(&mut document)
        .expect("undo inline alignment");
    layout.invalidate_nodes_with_delta(
        &document,
        &outcome.changed_nodes,
        outcome.structural,
        &outcome.structural_splices,
        &outcome.numbering_ranges,
    );
    let allocation = measurement.bytes();
    assert!(outcome.structural_splices.is_empty());
    assert!(
        layout
            .ordered_number_work_count()
            .saturating_sub(work_before)
            <= 4,
        "same-ID alignment undo scanned the distant suffix"
    );
    assert!(
        allocation < 24 * 1024 * 1024,
        "same-ID alignment undo transaction/delta/layout allocation grew with the document: {allocation}"
    );
    assert_suffix(&document, &layout);

    let composition_selection =
        Selection::caret(DocPoint::with_affinity(middle, 1, Affinity::After));
    let (outcome, _) = apply_history_and_measure(
        &mut document,
        &mut history,
        &mut layout,
        composition_selection,
        Transaction::InsertText {
            selection: composition_selection,
            text: "provisional".into(),
        },
    );
    assert!(outcome.structural_splices.is_empty());
    let work_before = layout.ordered_number_work_count();
    let measurement = AllocationMeasurement::begin();
    let outcome = history
        .replace_last_with(&mut document, composition_selection, |_restored| {
            Ok(Transaction::InsertText {
                selection: composition_selection,
                text: "replacement".into(),
            })
        })
        .expect("replace provisional composition");
    layout.invalidate_nodes_with_delta(
        &document,
        &outcome.changed_nodes,
        outcome.structural,
        &outcome.structural_splices,
        &outcome.numbering_ranges,
    );
    let allocation = measurement.bytes();
    assert!(outcome.structural_splices.is_empty());
    assert!(
        layout
            .ordered_number_work_count()
            .saturating_sub(work_before)
            <= 4,
        "same-ID IME restore scanned the distant suffix"
    );
    assert!(
        allocation < 24 * 1024 * 1024,
        "same-ID IME restore transaction/delta/layout allocation grew with the document: {allocation}"
    );
    assert_suffix(&document, &layout);
}

#[test]
fn checkpoint_vec_repairs_63_64_65_boundaries() {
    let mut document =
        Document::from_blocks(ordered_fixture(63)).expect("63-item checkpoint fixture");
    let mut layout = LayoutRegistry::new();
    layout.layout_document(&document, 0.0, 32.0, 120.0);
    assert_ordered_numbers_match(&document, &layout, "initial 63-item checkpoint");

    for expected_count in [64, 65] {
        let tail = document.blocks().last().expect("boundary tail").clone();
        let outcome = document
            .apply(Transaction::SplitBlock {
                at: DocPoint::with_affinity(
                    tail.id,
                    tail.content.as_text().expect("boundary text").len(),
                    Affinity::After,
                ),
            })
            .expect("boundary split");
        layout.invalidate_nodes_with_delta(
            &document,
            &outcome.changed_nodes,
            outcome.structural,
            &outcome.structural_splices,
            &outcome.numbering_ranges,
        );
        assert_eq!(document.block_count(), expected_count);
        assert_ordered_numbers_match(&document, &layout, "63/64/65 checkpoint boundary");
    }
}

#[test]
fn checkpoint_vec_repairs_new_stride_before_kind_updates() {
    let mut document =
        Document::from_blocks(ordered_fixture(64)).expect("checkpoint growth fixture");
    let mut layout = LayoutRegistry::new();
    layout.layout_document(&document, 0.0, 32.0, 120.0);
    let tail = document.blocks().last().expect("ordered tail").clone();
    let split = document
        .apply(Transaction::SplitBlock {
            at: DocPoint::with_affinity(
                tail.id,
                tail.content.as_text().unwrap().len(),
                Affinity::After,
            ),
        })
        .expect("split at the 64 boundary");
    layout.invalidate_nodes_with_delta(
        &document,
        &split.changed_nodes,
        split.structural,
        &split.structural_splices,
        &split.numbering_ranges,
    );
    let new_tail = document.blocks().last().expect("new ordered tail").clone();
    assert_eq!(layout.ordered_number(new_tail.id), Some(65));

    let selection = Selection::new(
        DocPoint::with_affinity(new_tail.id, 0, Affinity::Before),
        DocPoint::with_affinity(
            new_tail.id,
            new_tail.content.as_text().unwrap().len(),
            Affinity::After,
        ),
    );
    let indented = document
        .apply(Transaction::IndentList { selection })
        .expect("indent new checkpoint tail");
    layout.invalidate_nodes_with_delta(
        &document,
        &indented.changed_nodes,
        indented.structural,
        &indented.structural_splices,
        &indented.numbering_ranges,
    );
    assert_ordered_numbers_match(&document, &layout, "indent after 64/65 growth");

    let outdented = document
        .apply(Transaction::OutdentList { selection })
        .expect("outdent new checkpoint tail");
    layout.invalidate_nodes_with_delta(
        &document,
        &outdented.changed_nodes,
        outdented.structural,
        &outdented.structural_splices,
        &outdented.numbering_ranges,
    );
    assert_ordered_numbers_match(&document, &layout, "outdent after 64/65 growth");
}

#[test]
fn checkpoint_vec_grows_and_truncates_across_multiple_strides() {
    let mut document = Document::from_blocks(ordered_fixture(127)).expect("multi-stride fixture");
    let mut history = History::new(32, 4 * 1024 * 1024);
    let mut layout = LayoutRegistry::new();
    layout.layout_document(&document, 0.0, 32.0, 120.0);

    assert_ordered_numbers_match(&document, &layout, "initial multi-stride fixture");

    // 127 -> 128 keeps the existing two checkpoints; 128 -> 129 creates a
    // new checkpoint at index 128 and must seed it before the next kind edit.
    let first_tail = document.blocks().last().expect("first tail").clone();
    let before_selection = document.end_selection();
    let split = history
        .apply_with_selection(
            &mut document,
            before_selection,
            Transaction::SplitBlock {
                at: DocPoint::with_affinity(
                    first_tail.id,
                    first_tail.content.as_text().unwrap().len(),
                    Affinity::After,
                ),
            },
        )
        .expect("Return at 128");
    layout.invalidate_nodes_with_delta(
        &document,
        &split.changed_nodes,
        split.structural,
        &split.structural_splices,
        &split.numbering_ranges,
    );
    assert_eq!(document.block_count(), 128);
    assert_ordered_numbers_match(&document, &layout, "127 to 128");

    let second_tail = document.blocks().last().expect("second tail").clone();
    let before_selection = document.end_selection();
    let split = history
        .apply_with_selection(
            &mut document,
            before_selection,
            Transaction::SplitBlock {
                at: DocPoint::with_affinity(
                    second_tail.id,
                    second_tail.content.as_text().unwrap().len(),
                    Affinity::After,
                ),
            },
        )
        .expect("Return across the second stride");
    layout.invalidate_nodes_with_delta(
        &document,
        &split.changed_nodes,
        split.structural,
        &split.structural_splices,
        &split.numbering_ranges,
    );
    assert_eq!(document.block_count(), 129);
    assert_ordered_numbers_match(&document, &layout, "128 to 129");

    let tail = document.blocks().last().expect("numbered tail").clone();
    let tail_selection = Selection::new(
        DocPoint::with_affinity(tail.id, 0, Affinity::Before),
        DocPoint::with_affinity(
            tail.id,
            tail.content.as_text().unwrap().len(),
            Affinity::After,
        ),
    );
    let indented = document
        .apply(Transaction::IndentList {
            selection: tail_selection,
        })
        .expect("indent after checkpoint growth");
    layout.invalidate_nodes_with_delta(
        &document,
        &indented.changed_nodes,
        indented.structural,
        &indented.structural_splices,
        &indented.numbering_ranges,
    );
    assert_ordered_numbers_match(&document, &layout, "indent after second stride");

    let outdented = document
        .apply(Transaction::OutdentList {
            selection: tail_selection,
        })
        .expect("outdent after checkpoint growth");
    layout.invalidate_nodes_with_delta(
        &document,
        &outdented.changed_nodes,
        outdented.structural,
        &outdented.structural_splices,
        &outdented.numbering_ranges,
    );
    assert_ordered_numbers_match(&document, &layout, "outdent after second stride");

    let left = document.blocks()[127].id;
    let right = document.blocks()[128].id;
    let before_selection = document.end_selection();
    let merged = history
        .apply_with_selection(
            &mut document,
            before_selection,
            Transaction::MergeBlocks { left, right },
        )
        .expect("merge truncates the last checkpoint");
    layout.invalidate_nodes_with_delta(
        &document,
        &merged.changed_nodes,
        merged.structural,
        &merged.structural_splices,
        &merged.numbering_ranges,
    );
    assert_eq!(document.block_count(), 128);
    assert_ordered_numbers_match(&document, &layout, "truncate to 128");

    let undone = history
        .undo_with_outcome(&mut document)
        .expect("undo merge");
    layout.invalidate_nodes_with_delta(
        &document,
        &undone.changed_nodes,
        undone.structural,
        &undone.structural_splices,
        &undone.numbering_ranges,
    );
    assert_eq!(document.block_count(), 129);
    assert_ordered_numbers_match(&document, &layout, "undo to 129");

    let redone = history
        .redo_with_outcome(&mut document)
        .expect("redo merge");
    layout.invalidate_nodes_with_delta(
        &document,
        &redone.changed_nodes,
        redone.structural,
        &redone.structural_splices,
        &redone.numbering_ranges,
    );
    assert_eq!(document.block_count(), 128);
    assert_ordered_numbers_match(&document, &layout, "redo to 128");
}

#[test]
fn ordered_middle_splice_does_not_touch_the_distant_sequences() {
    let mut blocks = Vec::new();
    for index in 0..64 {
        blocks.push(Block {
            id: NodeId::new((index + 1) as u64),
            kind: BlockKind::OrderedItem { depth: 0 },
            content: BlockContent::text(format!("left-{index}")),
            alignment: TextAlignment::Left,
            revision: 0,
        });
    }
    blocks.push(Block {
        id: NodeId::new(65),
        kind: BlockKind::Paragraph,
        content: BlockContent::text("boundary"),
        alignment: TextAlignment::Left,
        revision: 0,
    });
    for index in 0..64 {
        blocks.push(Block {
            id: NodeId::new((index + 66) as u64),
            kind: BlockKind::OrderedItem { depth: 0 },
            content: BlockContent::text(format!("right-{index}")),
            alignment: TextAlignment::Left,
            revision: 0,
        });
    }
    blocks.extend((0..100_000).map(|index| Block {
        id: NodeId::new((index + 130) as u64),
        kind: BlockKind::Paragraph,
        content: BlockContent::text(format!("distant-suffix-{index}")),
        alignment: TextAlignment::Left,
        revision: 0,
    }));
    let mut document = Document::from_blocks(blocks).expect("ordered sequence fixture");
    let mut layout = LayoutRegistry::new();
    layout.layout_document(&document, 0.0, 32.0, 120.0);
    let left_tail = document.blocks()[63].id;
    let right_head = document.blocks()[65].id;
    assert_eq!(layout.ordered_number(left_tail), Some(64));
    assert_eq!(layout.ordered_number(right_head), Some(1));
    let work_before = layout.ordered_number_work_count();

    let outcome = document
        .apply(Transaction::SplitBlock {
            at: DocPoint::with_affinity(document.blocks()[32].id, 2, Affinity::After),
        })
        .expect("middle Return");
    layout.invalidate_nodes_with_delta(
        &document,
        &outcome.changed_nodes,
        outcome.structural,
        &outcome.structural_splices,
        &outcome.numbering_ranges,
    );
    assert_eq!(
        document.blocks()[64].kind,
        BlockKind::OrderedItem { depth: 0 }
    );
    assert_eq!(layout.ordered_number(left_tail), Some(65));
    assert_eq!(layout.ordered_number(right_head), Some(1));
    assert!(
        layout
            .ordered_number_work_count()
            .saturating_sub(work_before)
            <= 320,
        "middle splice visited the distant suffix"
    );
}

#[test]
fn changed_index_63_converges_at_the_first_clean_checkpoint_after_boundary_64() {
    let mut blocks = (0..64)
        .map(|index| Block {
            id: NodeId::new((index + 1) as u64),
            kind: BlockKind::OrderedItem { depth: 0 },
            content: BlockContent::text(format!("item-{index}")),
            alignment: TextAlignment::Left,
            revision: 0,
        })
        .collect::<Vec<_>>();
    blocks.push(Block {
        id: NodeId::new(65),
        kind: BlockKind::Paragraph,
        content: BlockContent::text("boundary"),
        alignment: TextAlignment::Left,
        revision: 0,
    });
    blocks.extend((0..20_000).map(|index| Block {
        id: NodeId::new((index + 66) as u64),
        kind: BlockKind::Paragraph,
        content: BlockContent::text(format!("suffix-{index}")),
        alignment: TextAlignment::Left,
        revision: 0,
    }));
    let mut document = Document::from_blocks(blocks).expect("checkpoint fixture");
    let mut layout = LayoutRegistry::new();
    layout.layout_document(&document, 0.0, 32.0, 120.0);
    let changed = document.blocks()[63].clone();
    let work_before = layout.ordered_number_work_count();
    let outcome = document
        .apply(Transaction::SetBlockKind {
            selection: Selection::new(
                DocPoint::with_affinity(changed.id, 0, Affinity::Before),
                DocPoint::with_affinity(
                    changed.id,
                    changed.content.as_text().unwrap().len(),
                    Affinity::After,
                ),
            ),
            kind: BlockKind::Paragraph,
        })
        .expect("index-63 boundary change");
    layout.invalidate_nodes_with_delta(
        &document,
        &outcome.changed_nodes,
        outcome.structural,
        &outcome.structural_splices,
        &outcome.numbering_ranges,
    );
    assert_eq!(layout.ordered_number(changed.id), None);
    assert!(
        layout
            .ordered_number_work_count()
            .saturating_sub(work_before)
            <= 128,
        "clean checkpoint at boundary 64 did not stop the suffix scan"
    );
}

#[test]
fn structural_affinity_keeps_images_outside_asymmetric_cross_node_ranges() {
    fn with_image() -> Document {
        let mut doc = Document::from_paragraph("ab");
        let paragraph = doc.first_node_id().unwrap();
        doc.apply(Transaction::InsertImage {
            selection: Selection::caret(DocPoint::new(paragraph, 1)),
            resource_id: "boundary-image".into(),
            natural_size: (100, 100),
        })
        .unwrap();
        doc
    }

    let mut doc = with_image();
    let left = doc.blocks()[0].id;
    let image = doc.blocks()[1].id;
    doc.apply(Transaction::DeleteRange {
        selection: Selection::new(
            DocPoint::new(left, 0),
            DocPoint::with_affinity(image, 0, Affinity::Before),
        ),
    })
    .unwrap();
    assert_eq!(doc.blocks()[1].id, image);
    assert_eq!(doc.text_at_index(0), Some(""));

    let mut doc = with_image();
    let image = doc.blocks()[1].id;
    let right = doc.blocks()[2].id;
    doc.apply(Transaction::DeleteRange {
        selection: Selection::new(
            DocPoint::with_affinity(image, 0, Affinity::After),
            DocPoint::new(right, 1),
        ),
    })
    .unwrap();
    assert_eq!(doc.blocks()[1].id, image);
    assert_eq!(doc.text_at_index(2), Some(""));

    let mut doc = with_image();
    let left = doc.blocks()[0].id;
    let image = doc.blocks()[1].id;
    doc.apply(Transaction::InsertText {
        selection: Selection::new(
            DocPoint::new(left, 0),
            DocPoint::with_affinity(image, 0, Affinity::Before),
        ),
        text: "x".into(),
    })
    .unwrap();
    assert_eq!(doc.text_at_index(0), Some("x"));
    assert_eq!(doc.blocks()[1].id, image);

    let mut doc = with_image();
    let image = doc.blocks()[1].id;
    let right = doc.blocks()[2].id;
    doc.apply(Transaction::InsertText {
        selection: Selection::new(
            DocPoint::with_affinity(image, 0, Affinity::After),
            DocPoint::new(right, 1),
        ),
        text: "y".into(),
    })
    .unwrap();
    assert_eq!(doc.blocks()[1].id, image);
    assert_eq!(doc.text_at_index(2), Some("y"));

    let mut doc = with_image();
    let left = doc.blocks()[0].id;
    let image = doc.blocks()[1].id;
    doc.apply(Transaction::InsertImage {
        selection: Selection::new(
            DocPoint::new(left, 0),
            DocPoint::with_affinity(image, 0, Affinity::Before),
        ),
        resource_id: "insert-before-image".into(),
        natural_size: (80, 80),
    })
    .unwrap();
    assert!(doc.blocks().iter().any(|block| block.id == image));

    let mut doc = with_image();
    let image = doc.blocks()[1].id;
    let right = doc.blocks()[2].id;
    doc.apply(Transaction::InsertImage {
        selection: Selection::new(
            DocPoint::with_affinity(image, 0, Affinity::After),
            DocPoint::new(right, 1),
        ),
        resource_id: "insert-after-image".into(),
        natural_size: (80, 80),
    })
    .unwrap();
    assert!(doc.blocks().iter().any(|block| block.id == image));
}

#[test]
fn failed_batch_restores_document_revision_and_next_node_id_exactly() {
    let mut doc = Document::from_paragraph("ab");
    let original = doc.clone();
    let node = doc.first_node_id().unwrap();
    let result = doc.apply_batch(TransactionBatch(vec![
        Transaction::SplitBlock {
            at: DocPoint::new(node, 1),
        },
        Transaction::InsertText {
            selection: Selection::caret(DocPoint::new(node, 99)),
            text: "must-not-commit".into(),
        },
    ]));
    assert!(matches!(
        result,
        Err(DocumentError::InvalidUtf8Offset { .. })
            | Err(DocumentError::InvalidGraphemeOffset { .. })
    ));
    assert_eq!(doc, original);

    doc.apply(Transaction::SplitBlock {
        at: DocPoint::new(node, 1),
    })
    .unwrap();
    assert_eq!(doc.blocks()[1].id, NodeId::new(2));
}

#[test]
fn replacement_uses_the_raw_grapheme_seam_before_resolving_the_cursor() {
    let mut doc = Document::from_paragraph("a\n\u{301}");
    let node = doc.first_node_id().unwrap();
    doc.apply(Transaction::InsertText {
        selection: Selection::new(DocPoint::new(node, 1), DocPoint::new(node, 2)),
        text: "b".into(),
    })
    .unwrap();
    assert_eq!(doc.text_at_index(0), Some("ab\u{301}"));
}

#[test]
fn apply_with_selection_rejects_invalid_before_selection_atomically() {
    let mut doc = Document::from_paragraphs(["left", "right"]);
    let original = doc.clone();
    let left = doc.blocks()[0].id;
    let right = doc.blocks()[1].id;
    let mut history = History::new(1_000, 16 * 1024 * 1024);
    let invalid_before = Selection::caret(DocPoint::new(left, 999));

    let result = history.apply_with_selection(
        &mut doc,
        invalid_before,
        Transaction::MergeBlocks { left, right },
    );
    assert!(matches!(
        result,
        Err(DocumentError::InvalidUtf8Offset { .. })
            | Err(DocumentError::InvalidGraphemeOffset { .. })
    ));
    assert_eq!(doc, original);
    assert_eq!(history.undo_depth(), 0);
    assert_eq!(history.redo_depth(), 0);
    assert_eq!(history.used_bytes(), 0);
    assert!(matches!(
        history.undo(&mut doc),
        Err(DocumentError::HistoryEmpty)
    ));
}

#[test]
fn style_normalization_uses_near_linear_grapheme_resolution() {
    let run_count = 256;
    let mut doc = Document::from_paragraph("a".repeat(run_count));
    let node = doc.first_node_id().unwrap();
    for offset in 0..run_count {
        doc.apply(Transaction::ToggleMark {
            selection: Selection::new(DocPoint::new(node, offset), DocPoint::new(node, offset + 1)),
            mark: if offset % 2 == 0 {
                Mark::Bold
            } else {
                Mark::Italic
            },
        })
        .unwrap();
    }
    reset_grapheme_resolution_counter();
    doc.apply(Transaction::InsertText {
        selection: Selection::caret(DocPoint::new(node, 0)),
        text: "x".into(),
    })
    .unwrap();
    assert!(
        grapheme_resolution_counter() < 32,
        "style normalization repeatedly rescanned graphemes: {} calls",
        grapheme_resolution_counter()
    );
}

#[test]
fn adjacent_image_empty_seams_have_consistent_transaction_semantics() {
    fn with_adjacent_images() -> (Document, NodeId, NodeId) {
        let mut doc = Document::from_paragraph("a");
        let paragraph = doc.first_node_id().unwrap();
        doc.apply(Transaction::InsertImage {
            selection: Selection::caret(DocPoint::new(paragraph, 1)),
            resource_id: "image-a".into(),
            natural_size: (100, 100),
        })
        .unwrap();
        let image_a = doc.blocks()[1].id;
        doc.apply(Transaction::InsertImage {
            selection: Selection::caret(DocPoint::with_affinity(image_a, 0, Affinity::After)),
            resource_id: "image-b".into(),
            natural_size: (100, 100),
        })
        .unwrap();
        let image_b = doc.blocks()[2].id;
        (doc, image_a, image_b)
    }

    fn seam(image_a: NodeId, image_b: NodeId) -> Selection {
        Selection::new(
            DocPoint::with_affinity(image_a, 0, Affinity::After),
            DocPoint::with_affinity(image_b, 0, Affinity::Before),
        )
    }

    let (mut doc, image_a, image_b) = with_adjacent_images();
    let original_ids: Vec<_> = doc.blocks().iter().map(|block| block.id).collect();
    let outcome = doc
        .apply(Transaction::DeleteRange {
            selection: seam(image_a, image_b),
        })
        .unwrap();
    assert!(outcome.changed_nodes.is_empty());
    assert_eq!(
        doc.blocks()
            .iter()
            .map(|block| block.id)
            .collect::<Vec<_>>(),
        original_ids
    );

    let (mut doc, image_a, image_b) = with_adjacent_images();
    doc.apply(Transaction::InsertText {
        selection: seam(image_a, image_b),
        text: "middle".into(),
    })
    .unwrap();
    assert_eq!(doc.blocks()[1].id, image_a);
    assert_eq!(doc.text_at_index(2), Some("middle"));
    assert_eq!(doc.blocks()[3].id, image_b);

    let (mut doc, image_a, image_b) = with_adjacent_images();
    doc.apply(Transaction::InsertImage {
        selection: seam(image_a, image_b),
        resource_id: "image-middle".into(),
        natural_size: (100, 100),
    })
    .unwrap();
    assert_eq!(doc.blocks()[1].id, image_a);
    assert!(matches!(doc.blocks()[2].kind, BlockKind::Image));
    assert_eq!(doc.blocks()[3].id, image_b);
}

#[test]
fn validate_styles_scans_graphemes_once_per_text_on_transactions() {
    fn transaction_validation_steps(run_count: usize) -> usize {
        let text = "a".repeat(run_count);
        let styles: SmallVec<[StyledRun; 4]> = (0..run_count)
            .map(|offset| {
                StyledRun::new(
                    offset..offset + 1,
                    [if offset % 2 == 0 {
                        Mark::Bold
                    } else {
                        Mark::Italic
                    }],
                )
            })
            .collect();
        let node = NodeId::new(1);
        let block = Block {
            id: node,
            kind: BlockKind::Paragraph,
            content: BlockContent::Text { text, styles },
            alignment: TextAlignment::Left,
            revision: 0,
        };
        let mut doc = Document::from_blocks(vec![block]).unwrap();
        reset_validation_grapheme_counter();
        doc.apply(Transaction::InsertText {
            selection: Selection::caret(DocPoint::new(node, 0)),
            text: "x".into(),
        })
        .unwrap();
        validation_grapheme_counter()
    }

    let smaller = transaction_validation_steps(256);
    let larger = transaction_validation_steps(512);
    assert!(smaller > 0);
    assert!(
        larger < smaller.saturating_mul(3),
        "validate_styles grapheme traversal grew super-linearly: {smaller} -> {larger}"
    );
}

#[test]
fn replacement_preserves_style_isolation_across_combining_grapheme_seam() {
    fn styled_document() -> (Document, NodeId) {
        let mut doc = Document::from_paragraph("a\n\u{301}");
        let node = doc.first_node_id().unwrap();
        doc.apply(Transaction::ToggleMark {
            selection: Selection::new(DocPoint::new(node, 0), DocPoint::new(node, 1)),
            mark: Mark::Bold,
        })
        .unwrap();
        doc.apply(Transaction::ToggleMark {
            selection: Selection::new(DocPoint::new(node, 2), DocPoint::new(node, 4)),
            mark: Mark::Italic,
        })
        .unwrap();
        (doc, node)
    }

    fn assert_run_marks(block: &Block, start: usize, end: usize, expected: &[Mark]) {
        let BlockContent::Text { styles, .. } = &block.content else {
            panic!("expected a text block");
        };
        let run = styles
            .iter()
            .find(|run| run.range.start == start && run.range.end == end)
            .unwrap_or_else(|| panic!("missing styled run {start}..{end}: {styles:?}"));
        assert_eq!(run.marks.as_slice(), expected);
    }

    let (mut doc, node) = styled_document();
    doc.apply(Transaction::InsertImage {
        selection: Selection::new(DocPoint::new(node, 1), DocPoint::new(node, 2)),
        resource_id: "isolated-image".into(),
        natural_size: (100, 100),
    })
    .unwrap();
    assert_run_marks(&doc.blocks()[0], 0, 1, &[Mark::Bold]);
    assert_run_marks(&doc.blocks()[2], 0, 2, &[Mark::Italic]);

    let (mut doc, node) = styled_document();
    doc.apply(Transaction::InsertText {
        selection: Selection::new(DocPoint::new(node, 1), DocPoint::new(node, 2)),
        text: "x".into(),
    })
    .unwrap();
    assert_run_marks(&doc.blocks()[0], 0, 1, &[Mark::Bold]);
    // The combining mark joins the inserted `x` into one grapheme; the
    // resulting grapheme remains Italic without importing Bold from the left.
    assert_run_marks(&doc.blocks()[0], 1, 4, &[Mark::Italic]);
}

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
    let ids = layout
        .exact_cache_ids()
        .collect::<std::collections::HashSet<_>>();
    assert!(ids.len() <= layout.visible_range().len());
}

#[test]
fn image_hit_testing_exposes_before_and_after_document_points() {
    let mut document = Document::from_paragraph("甲");
    document
        .apply(Transaction::InsertImage {
            selection: document.end_selection(),
            resource_id: "image".into(),
            natural_size: (1600, 900),
        })
        .unwrap();
    let image_id = document.blocks()[1].id;
    let mut layout = LayoutRegistry::new();
    layout.layout_document(&document, 0.0, 640.0, 680.0);
    let image = layout
        .visible()
        .iter()
        .find(|block| block.node_id == image_id)
        .cloned();
    let image = image.expect("image should be in the viewport window");
    let before = layout
        .point_to_doc(point(
            image.bounds.left() + px(2.0),
            image.bounds.top() + px(2.0),
        ))
        .expect("image hit should resolve");
    let after = layout
        .point_to_doc(point(
            image.bounds.right() - px(2.0),
            image.bounds.bottom() - px(2.0),
        ))
        .expect("image hit should resolve");
    assert_eq!(before.node_id, image_id);
    assert_eq!(before.affinity, Affinity::Before);
    assert_eq!(after.node_id, image_id);
    assert_eq!(after.affinity, Affinity::After);
}

#[gpui::test]
fn ime_commit_preserves_utf16_selection(cx: &mut gpui::TestAppContext) {
    let mut editor = EditorCore::for_test("前后", cx);
    editor.set_caret_utf8("前".len());
    editor
        .replace_and_mark_utf16(None, "中华", Some(0..2))
        .unwrap();
    assert_eq!(editor.visible_text(), "前中华后");
    assert_eq!(editor.marked_text(), Some("中华"));
    editor.commit_marked_text("中国").unwrap();
    assert_eq!(editor.visible_text(), "前中国后");
    assert_eq!(editor.marked_text(), None);
}

#[gpui::test]
fn ime_internal_selection_does_not_shrink_marked_range(cx: &mut gpui::TestAppContext) {
    let mut editor = EditorCore::for_test("前后", cx);
    editor.set_caret_utf8("前".len());
    editor
        .replace_and_mark_utf16(None, "中华", Some(2..2))
        .unwrap();
    assert_eq!(editor.visible_text(), "前中华后");
    assert_eq!(editor.marked_text(), Some("中华"));
    editor.commit_marked_text("中国").unwrap();
    assert_eq!(editor.visible_text(), "前中国后");
    assert_eq!(editor.undo_depth(), 1);
    editor.undo().unwrap();
    assert_eq!(editor.visible_text(), "前后");
}

#[gpui::test]
fn ime_partial_selection_and_candidate_updates_are_one_undo(cx: &mut gpui::TestAppContext) {
    let mut editor = EditorCore::for_test("abcd", cx);
    editor.select_document_range(1, 3);
    editor
        .replace_and_mark_utf16(Some(1..3), "中", Some(1..1))
        .unwrap();
    editor
        .replace_and_mark_utf16(None, "中华", Some(2..2))
        .unwrap();
    assert_eq!(editor.visible_text(), "a中华d");
    assert_eq!(editor.marked_text(), Some("中华"));
    editor.commit_marked_text("中国").unwrap();
    assert_eq!(editor.visible_text(), "a中国d");
    assert_eq!(editor.undo_depth(), 1);
    editor.undo().unwrap();
    assert_eq!(editor.visible_text(), "abcd");
}

#[gpui::test]
async fn entity_input_commit_maps_original_selection_only_once(cx: &mut gpui::TestAppContext) {
    let mut cx = cx.add_empty_window();
    let entity = cx.new(|cx| EditorCore::new(Document::from_paragraph("abcd"), cx));
    let original_selection = cx.update(|window, cx| {
        entity.update(cx, |editor, editor_cx| {
            editor.select_document_range(1, 3);
            let original_selection = editor.selection();
            <EditorCore as EntityInputHandler>::replace_and_mark_text_in_range(
                editor,
                Some(1..3),
                "X",
                Some(1..1),
                window,
                editor_cx,
            );
            <EditorCore as EntityInputHandler>::replace_text_in_range(
                editor, None, "Y", window, editor_cx,
            );
            assert_eq!(editor.visible_text(), "aYd");
            assert_eq!(editor.undo_depth(), 1);
            original_selection
        })
    });
    cx.update(|_, cx| {
        entity.update(cx, |editor, _| editor.undo().unwrap());
    });
    assert_eq!(
        entity.read_with(cx, |editor, _| editor.visible_text()),
        "abcd"
    );
    assert_eq!(
        entity.read_with(cx, |editor, _| editor.selection()),
        original_selection
    );
}

#[gpui::test]
async fn entity_input_candidate_range_is_remapped_before_restore(cx: &mut gpui::TestAppContext) {
    let mut cx = cx.add_empty_window();
    let entity = cx.new(|cx| EditorCore::new(Document::from_paragraph("ab"), cx));

    cx.update(|window, cx| {
        entity.update(cx, |editor, editor_cx| {
            editor.set_caret_utf8(1);
            <EditorCore as EntityInputHandler>::replace_and_mark_text_in_range(
                editor,
                None,
                "候选",
                Some(2..2),
                window,
                editor_cx,
            );
            let candidate_selection = editor.selection();
            assert_eq!(editor.visible_text(), "a候选b");
            <EditorCore as EntityInputHandler>::replace_and_mark_text_in_range(
                editor,
                Some(1..3),
                "更新",
                Some(1..1),
                window,
                editor_cx,
            );
            assert_eq!(editor.visible_text(), "a更新b");
            assert_eq!(editor.marked_text(), Some("更新"));
            assert_eq!(editor.undo_depth(), 1);

            let before_invalid = (
                editor.visible_text(),
                editor.selection(),
                editor.marked_text().map(str::to_owned),
                editor.undo_depth(),
            );
            <EditorCore as EntityInputHandler>::replace_and_mark_text_in_range(
                editor,
                Some(99..100),
                "错误",
                Some(1..1),
                window,
                editor_cx,
            );
            assert_eq!(
                (
                    editor.visible_text(),
                    editor.selection(),
                    editor.marked_text().map(str::to_owned),
                    editor.undo_depth(),
                ),
                before_invalid
            );
            assert!(editor.input_error().is_some());
            assert!(candidate_selection.is_caret());
        });
    });
}

#[gpui::test]
async fn entity_input_explicit_candidate_range_updates_composition_base(
    cx: &mut gpui::TestAppContext,
) {
    let mut cx = cx.add_empty_window();
    let entity = cx.new(|cx| EditorCore::new(Document::from_paragraph("abcd"), cx));
    let original_selection = cx.update(|window, cx| {
        entity.update(cx, |editor, editor_cx| {
            editor.set_caret_utf8(1);
            let original_selection = editor.selection();
            <EditorCore as EntityInputHandler>::replace_and_mark_text_in_range(
                editor,
                None,
                "X",
                Some(1..1),
                window,
                editor_cx,
            );
            assert_eq!(editor.visible_text(), "aXbcd");
            <EditorCore as EntityInputHandler>::replace_and_mark_text_in_range(
                editor,
                Some(0..2),
                "Y",
                Some(1..1),
                window,
                editor_cx,
            );
            assert_eq!(editor.visible_text(), "Ybcd");
            <EditorCore as EntityInputHandler>::replace_text_in_range(
                editor, None, "Z", window, editor_cx,
            );
            assert_eq!(editor.visible_text(), "Zbcd");
            assert_eq!(editor.undo_depth(), 1);
            original_selection
        })
    });
    cx.update(|_, cx| {
        entity.update(cx, |editor, _| editor.undo().unwrap());
    });
    assert_eq!(
        entity.read_with(cx, |editor, _| editor.visible_text()),
        "abcd"
    );
    assert_eq!(
        entity.read_with(cx, |editor, _| editor.selection()),
        original_selection
    );
}

#[gpui::test]
async fn entity_input_commit_restores_reverse_selection_affinities(cx: &mut gpui::TestAppContext) {
    let mut cx = cx.add_empty_window();
    let entity = cx.new(|cx| EditorCore::new(Document::from_paragraph("abcd"), cx));
    let original_selection = cx.update(|window, cx| {
        entity.update(cx, |editor, editor_cx| {
            let node = editor.document().first_node_id().expect("paragraph");
            let original_selection = Selection::new(
                DocPoint::with_affinity(node, 3, Affinity::Before),
                DocPoint::with_affinity(node, 1, Affinity::After),
            );
            editor.set_selection_for_test(original_selection);
            <EditorCore as EntityInputHandler>::replace_and_mark_text_in_range(
                editor,
                None,
                "X",
                Some(1..1),
                window,
                editor_cx,
            );
            <EditorCore as EntityInputHandler>::replace_text_in_range(
                editor, None, "Y", window, editor_cx,
            );
            assert_eq!(editor.visible_text(), "aYd");
            assert_eq!(editor.undo_depth(), 1);
            original_selection
        })
    });
    cx.update(|_, cx| {
        entity.update(cx, |editor, _| editor.undo().unwrap());
    });
    assert_eq!(
        entity.read_with(cx, |editor, _| editor.selection()),
        original_selection
    );
}

#[gpui::test]
async fn entity_input_marked_endpoints_keep_actual_candidate_interval(
    cx: &mut gpui::TestAppContext,
) {
    let mut cx = cx.add_empty_window();
    let entity = cx.new(|cx| EditorCore::new(Document::from_paragraph("a"), cx));
    let original_selection =
        cx.update(|_, cx| entity.read_with(cx, |editor, _| editor.selection()));
    cx.update(|window, cx| {
        entity.update(cx, |editor, editor_cx| {
            editor.set_caret_utf8(1);
            <EditorCore as EntityInputHandler>::replace_and_mark_text_in_range(
                editor,
                None,
                "\u{301}",
                Some(0..0),
                window,
                editor_cx,
            );
            assert_eq!(editor.visible_text(), "a\u{301}");
            assert!(editor.selection().is_caret());
            assert_eq!(editor.selection().head.utf8_offset, 3);
            assert_eq!(
                <EditorCore as EntityInputHandler>::marked_text_range(editor, window, editor_cx,),
                Some(0..2)
            );
            <EditorCore as EntityInputHandler>::replace_and_mark_text_in_range(
                editor,
                Some(0..2),
                "\u{308}",
                Some(0..0),
                window,
                editor_cx,
            );
            assert_eq!(editor.visible_text(), "a\u{308}");
            assert!(editor.selection().is_caret());
            assert_eq!(
                <EditorCore as EntityInputHandler>::marked_text_range(editor, window, editor_cx,),
                Some(0..2)
            );
            <EditorCore as EntityInputHandler>::replace_text_in_range(
                editor, None, "b", window, editor_cx,
            );
            assert_eq!(editor.visible_text(), "ab");
            assert_eq!(editor.undo_depth(), 1);
        });
    });
    cx.update(|_, cx| {
        entity.update(cx, |editor, _| editor.undo().unwrap());
    });
    assert_eq!(entity.read_with(cx, |editor, _| editor.visible_text()), "a");
    assert_eq!(
        entity.read_with(cx, |editor, _| editor.selection()),
        original_selection
    );
}

#[gpui::test]
async fn entity_input_actual_span_survives_right_grapheme_join(cx: &mut gpui::TestAppContext) {
    let mut cx = cx.add_empty_window();
    let entity = cx.new(|cx| EditorCore::new(Document::from_paragraph("\u{301}"), cx));
    let original_selection = cx.update(|window, cx| {
        entity.update(cx, |editor, editor_cx| {
            editor.set_caret_utf8(0);
            let original_selection = editor.selection();
            <EditorCore as EntityInputHandler>::replace_and_mark_text_in_range(
                editor,
                None,
                "a",
                Some(1..1),
                window,
                editor_cx,
            );
            assert_eq!(editor.visible_text(), "a\u{301}");
            <EditorCore as EntityInputHandler>::replace_text_in_range(
                editor, None, "b", window, editor_cx,
            );
            assert_eq!(editor.visible_text(), "b\u{301}");
            assert_eq!(editor.undo_depth(), 1);
            original_selection
        })
    });

    cx.update(|_, cx| {
        entity.update(cx, |editor, _| editor.undo().unwrap());
    });
    assert_eq!(
        entity.read_with(cx, |editor, _| editor.visible_text()),
        "\u{301}"
    );
    assert_eq!(
        entity.read_with(cx, |editor, _| editor.selection()),
        original_selection
    );
}

#[gpui::test]
async fn entity_input_explicit_replacement_outside_expanded_public_mark(
    cx: &mut gpui::TestAppContext,
) {
    let mut cx = cx.add_empty_window();
    let entity = cx.new(|cx| EditorCore::new(Document::from_paragraph("aQ"), cx));
    let original_selection = cx.update(|window, cx| {
        entity.update(cx, |editor, editor_cx| {
            editor.set_caret_utf8(1);
            let original_selection = editor.selection();
            <EditorCore as EntityInputHandler>::replace_and_mark_text_in_range(
                editor,
                None,
                "\u{301}",
                Some(0..0),
                window,
                editor_cx,
            );
            assert_eq!(editor.visible_text(), "a\u{301}Q");
            <EditorCore as EntityInputHandler>::replace_text_in_range(
                editor,
                Some(2..3),
                "b",
                window,
                editor_cx,
            );
            assert_eq!(editor.visible_text(), "ab");
            assert_eq!(editor.undo_depth(), 1);
            original_selection
        })
    });

    cx.update(|_, cx| {
        entity.update(cx, |editor, _| editor.undo().unwrap());
    });
    assert_eq!(
        entity.read_with(cx, |editor, _| editor.visible_text()),
        "aQ"
    );
    assert_eq!(
        entity.read_with(cx, |editor, _| editor.selection()),
        original_selection
    );
}

#[gpui::test]
async fn entity_input_explicit_replacement_superset_stays_explicit(cx: &mut gpui::TestAppContext) {
    let mut cx = cx.add_empty_window();
    let entity = cx.new(|cx| EditorCore::new(Document::from_paragraph("aQ"), cx));
    let original_selection = cx.update(|window, cx| {
        entity.update(cx, |editor, editor_cx| {
            editor.set_caret_utf8(1);
            let original_selection = editor.selection();
            <EditorCore as EntityInputHandler>::replace_and_mark_text_in_range(
                editor,
                None,
                "\u{301}",
                Some(0..0),
                window,
                editor_cx,
            );
            assert_eq!(editor.visible_text(), "a\u{301}Q");
            <EditorCore as EntityInputHandler>::replace_text_in_range(
                editor,
                Some(0..3),
                "b",
                window,
                editor_cx,
            );
            assert_eq!(editor.visible_text(), "b");
            assert_eq!(editor.undo_depth(), 1);
            original_selection
        })
    });

    cx.update(|_, cx| {
        entity.update(cx, |editor, _| editor.undo().unwrap());
    });
    assert_eq!(
        entity.read_with(cx, |editor, _| editor.visible_text()),
        "aQ"
    );
    assert_eq!(
        entity.read_with(cx, |editor, _| editor.selection()),
        original_selection
    );
}

#[gpui::test]
async fn entity_input_explicit_replacement_subset_stays_explicit(cx: &mut gpui::TestAppContext) {
    let mut cx = cx.add_empty_window();
    let entity = cx.new(|cx| EditorCore::new(Document::from_paragraph("aQ"), cx));
    let original_selection = cx.update(|window, cx| {
        entity.update(cx, |editor, editor_cx| {
            editor.set_caret_utf8(1);
            let original_selection = editor.selection();
            <EditorCore as EntityInputHandler>::replace_and_mark_text_in_range(
                editor,
                None,
                "\u{301}",
                Some(0..0),
                window,
                editor_cx,
            );
            assert_eq!(editor.visible_text(), "a\u{301}Q");
            <EditorCore as EntityInputHandler>::replace_text_in_range(
                editor,
                Some(0..1),
                "b",
                window,
                editor_cx,
            );
            assert_eq!(editor.visible_text(), "bQ");
            assert_eq!(editor.undo_depth(), 1);
            original_selection
        })
    });

    cx.update(|_, cx| {
        entity.update(cx, |editor, _| editor.undo().unwrap());
    });
    assert_eq!(
        entity.read_with(cx, |editor, _| editor.visible_text()),
        "aQ"
    );
    assert_eq!(
        entity.read_with(cx, |editor, _| editor.selection()),
        original_selection
    );
}

#[gpui::test]
async fn entity_input_explicit_replacement_overlap_stays_explicit(cx: &mut gpui::TestAppContext) {
    let mut cx = cx.add_empty_window();
    let entity = cx.new(|cx| EditorCore::new(Document::from_paragraph("aQ"), cx));
    let original_selection = cx.update(|window, cx| {
        entity.update(cx, |editor, editor_cx| {
            editor.set_caret_utf8(1);
            let original_selection = editor.selection();
            <EditorCore as EntityInputHandler>::replace_and_mark_text_in_range(
                editor,
                None,
                "\u{301}",
                Some(0..0),
                window,
                editor_cx,
            );
            assert_eq!(editor.visible_text(), "a\u{301}Q");
            <EditorCore as EntityInputHandler>::replace_text_in_range(
                editor,
                Some(1..3),
                "b",
                window,
                editor_cx,
            );
            assert_eq!(editor.visible_text(), "ab");
            assert_eq!(editor.undo_depth(), 1);
            original_selection
        })
    });

    cx.update(|_, cx| {
        entity.update(cx, |editor, _| editor.undo().unwrap());
    });
    assert_eq!(
        entity.read_with(cx, |editor, _| editor.visible_text()),
        "aQ"
    );
    assert_eq!(
        entity.read_with(cx, |editor, _| editor.selection()),
        original_selection
    );
}

#[test]
fn apply_batch_insert_then_delete_clears_inserted_span() {
    let mut doc = Document::from_paragraph("a");
    let node = doc.first_node_id().unwrap();
    let outcome = doc
        .apply_batch(TransactionBatch(vec![
            Transaction::InsertText {
                selection: Selection::caret(DocPoint::new(node, 0)),
                text: "x".into(),
            },
            Transaction::DeleteRange {
                selection: Selection::new(
                    DocPoint::with_affinity(node, 0, Affinity::Before),
                    DocPoint::with_affinity(node, 2, Affinity::After),
                ),
            },
        ]))
        .unwrap();

    assert_eq!(doc.text_at_index(0), Some(""));
    assert_eq!(outcome.inserted_span, None);
}

#[test]
fn apply_batch_insert_then_remove_clears_removed_inserted_span() {
    let mut doc = Document::from_paragraphs(["a", "z"]);
    let node = doc.first_node_id().unwrap();
    let outcome = doc
        .apply_batch(TransactionBatch(vec![
            Transaction::InsertText {
                selection: Selection::caret(DocPoint::new(node, 0)),
                text: "x".into(),
            },
            Transaction::RemoveNode { node_id: node },
        ]))
        .unwrap();

    assert_eq!(doc.text_at_index(0), Some("z"));
    assert_eq!(outcome.inserted_span, None);
}

#[test]
fn apply_batch_final_insert_reports_final_inserted_span() {
    let mut doc = Document::from_paragraph("a");
    let node = doc.first_node_id().unwrap();
    let outcome = doc
        .apply_batch(TransactionBatch(vec![
            Transaction::InsertText {
                selection: Selection::caret(DocPoint::new(node, 0)),
                text: "x".into(),
            },
            Transaction::InsertText {
                selection: Selection::caret(DocPoint::new(node, 1)),
                text: "y".into(),
            },
        ]))
        .unwrap();

    assert_eq!(doc.text_at_index(0), Some("xya"));
    assert_eq!(
        outcome.inserted_span.map(|span| (span.node_id, span.range)),
        Some((node, 1..2))
    );
}

#[gpui::test]
async fn empty_hard_lines_hit_test_by_y(cx: &mut gpui::TestAppContext) {
    let mut cx = cx.add_empty_window();
    let document = Document::from_paragraph("\n\n");
    let node = document.first_node_id().expect("paragraph");
    let mut layout = LayoutRegistry::new();
    cx.update(|window, _| {
        layout.shape_visible_with_window(&document, 0.0, 1_000.0, 680.0, window);
    });

    let (bounds, line_heights) = {
        let cached = layout.block_layout(node).expect("empty paragraph layout");
        let line_height = layout.line_height(node).expect("line height");
        assert_eq!(cached.text_lines.len(), 3);
        (
            cached.bounds,
            cached
                .text_lines
                .iter()
                .map(|line| line.size(line_height).height)
                .collect::<Vec<_>>(),
        )
    };
    let mut line_top = bounds.top();
    for (expected, line_height) in line_heights.into_iter().enumerate() {
        let hit = layout
            .point_to_doc(point(
                bounds.left() + bounds.size.width / 4.0,
                line_top + line_height / 2.0,
            ))
            .expect("empty hard-line hit should resolve");
        assert_eq!(hit.node_id, node);
        assert_eq!(hit.utf8_offset, expected);
        line_top += line_height;
    }
}

#[gpui::test]
async fn crlf_visual_end_never_publishes_invalid_grapheme_offset(cx: &mut gpui::TestAppContext) {
    let mut cx = cx.add_empty_window();
    let mut editor = EditorCore::for_test("a\r\nb", cx);
    editor.set_caret_utf8(0);
    cx.update(|window, _| {
        let document = editor.document().clone();
        editor
            .layout
            .shape_visible_with_window(&document, 0.0, 1_000.0, 680.0, window);
    });

    editor.move_end();
    assert_ne!(editor.selection().head.utf8_offset, 2);
    editor.insert_text("X").unwrap();
    assert!(editor.input_error().is_none());
}

#[gpui::test]
async fn measured_reflow_shapes_every_final_member_without_fixed_pass_hole(
    cx: &mut gpui::TestAppContext,
) {
    let mut cx = cx.add_empty_window();
    let document = Document::from_paragraphs((0..32).map(|index| format!("row-{index}")));
    let mut layout = LayoutRegistry::new();
    cx.update(|window, _| {
        let mut tall = window.text_style();
        tall.line_height = px(1000.0).into();
        layout.shape_visible_with_style(&document, 0.0, 100_000.0, 680.0, tall, window);
        layout.shape_visible_with_style(
            &document,
            120.0,
            120.0,
            680.0,
            window.text_style(),
            window,
        );
    });
    let final_range = layout.visible_range();
    let final_blocks = document.blocks().iter_range(final_range.clone());
    assert!(final_range.start < final_range.end);
    for block in final_blocks {
        let cached = layout
            .cache
            .get(&block.id)
            .expect("final viewport/prefetch member must be cached");
        assert!(
            !cached.layout.text_lines.is_empty(),
            "final member {} was exposed without shaping",
            block.id.raw()
        );
    }
}

#[gpui::test]
async fn measured_reflow_uses_incremental_height_sum_tree_for_large_document(
    cx: &mut gpui::TestAppContext,
) {
    let mut cx = cx.add_empty_window();
    let document = Document::from_paragraphs((0..20_000).map(|index| format!("row-{index}")));
    let mut layout = LayoutRegistry::new();
    cx.update(|window, _| {
        let mut tall = window.text_style();
        tall.line_height = px(1000.0).into();
        layout.shape_visible_with_style(&document, 0.0, 100_000.0, 680.0, tall, window);
    });
    let work_before = layout.height_index_work_count();
    cx.update(|window, _| {
        layout.shape_visible_with_style(
            &document,
            120.0,
            120.0,
            680.0,
            window.text_style(),
            window,
        );
    });
    let index_work = layout.height_index_work_count().saturating_sub(work_before);
    assert!(
        index_work < 4_096,
        "height index work scaled with full-document convergence waves: {index_work}"
    );
    let final_range = layout.visible_range();
    for block in document.blocks().iter_range(final_range.clone()) {
        assert!(
            layout
                .cache
                .get(&block.id)
                .is_some_and(|cached| !cached.layout.text_lines.is_empty()),
            "final member {} was exposed without shaping",
            block.id.raw()
        );
    }
}

#[gpui::test]
async fn shaped_cache_budget_accounts_dynamic_selection_geometry_scratch(
    cx: &mut gpui::TestAppContext,
) {
    let mut cx = cx.add_empty_window();
    let text = "word ".repeat(12_000);
    let document = Document::from_paragraph(text);
    let node = document.first_node_id().expect("paragraph");
    let mut layout = LayoutRegistry::new();
    cx.update(|window, _| {
        layout.shape_visible_with_window(&document, 0.0, 100_000.0, 80.0, window);
    });
    let cached = layout.cache.get(&node).expect("long paragraph cache");
    let selection = Selection::new(cached.layout.before, cached.layout.after);
    let rects = layout.selection_rects(selection);
    assert!(
        rects.len() > 8,
        "soft wrapping must exercise geometry growth"
    );
    let output_storage = rects
        .len()
        .saturating_mul(size_of::<Bounds<gpui::Pixels>>())
        .saturating_add(size_of::<Vec<Bounds<gpui::Pixels>>>());
    assert!(
        cached.selection_geometry_bytes >= output_storage,
        "selection geometry reserve {} is below final output storage {}",
        cached.selection_geometry_bytes,
        output_storage
    );
    assert!(layout.used_bytes() <= layout.budget_bytes());
    assert!(layout.peak_accounted_bytes() <= layout.budget_bytes());
}

#[gpui::test]
async fn selection_geometry_real_path_peak_covers_allocator(cx: &mut gpui::TestAppContext) {
    let mut cx = cx.add_empty_window();
    let text = "wide-ascii ".repeat(12_000);
    let document = Document::from_paragraph(text);
    let node = document.first_node_id().expect("paragraph");
    let mut baseline = LayoutRegistry::new();
    cx.update(|window, _| {
        baseline.shape_visible_with_window(&document, 0.0, 100_000.0, 160.0, window);
    });
    let baseline_cached = baseline
        .cache
        .get(&node)
        .expect("baseline shaped paragraph");
    let admission_budget = baseline_cached
        .bytes
        .saturating_add(256 * 1024)
        .min(LAYOUT_CACHE_BUDGET_BYTES - 1);
    assert!(admission_budget < LAYOUT_CACHE_BUDGET_BYTES);
    let mut layout = LayoutRegistry::with_budget(admission_budget);
    cx.update(|window, _| {
        layout.shape_visible_with_window(&document, 0.0, 100_000.0, 160.0, window);
    });
    let cached = layout
        .cache
        .get(&node)
        .expect("near-budget shaped paragraph");
    let selection = Selection::new(cached.layout.before, cached.layout.after);
    let measurement = AllocationMeasurement::begin();
    let rects = layout.selection_rects(selection);
    let observed = measurement.bytes();
    assert!(
        rects.len() > 100,
        "selection must span many soft-wrapped rows"
    );
    let margin = 64 * 1024;
    assert!(
        observed <= cached.selection_geometry_bytes.saturating_add(margin),
        "selection scratch allocation {observed} exceeded reserved {} plus margin {margin}",
        cached.selection_geometry_bytes
    );
    assert!(layout.used_bytes() <= layout.budget_bytes());
    assert!(layout.peak_accounted_bytes() <= layout.budget_bytes());
}

#[gpui::test]
async fn entity_input_repeated_candidates_do_not_clone_document_or_history(
    cx: &mut gpui::TestAppContext,
) {
    let mut cx = cx.add_empty_window();
    let document = Document::from_paragraphs(
        (0..20_000).map(|index| format!("block-{index}-{}", "z".repeat(1_024))),
    );
    let first_id = document.blocks().first().expect("first block").id;
    let last_id = document.blocks().last().expect("last block").id;
    let entity = cx.new(|cx| EditorCore::new(document, cx));
    let (first_revision, last_revision) = entity.read_with(cx, |editor, _| {
        (
            editor
                .document()
                .block(first_id)
                .expect("first block")
                .revision,
            editor
                .document()
                .block(last_id)
                .expect("last block")
                .revision,
        )
    });
    let measurement = AllocationMeasurement::begin();
    cx.update(|window, cx| {
        entity.update(cx, |editor, editor_cx| {
            editor.set_caret_utf8(0);
            <EditorCore as EntityInputHandler>::replace_and_mark_text_in_range(
                editor,
                None,
                "x",
                Some(1..1),
                window,
                editor_cx,
            );
            for text in ["y", "z", "w"] {
                <EditorCore as EntityInputHandler>::replace_and_mark_text_in_range(
                    editor,
                    None,
                    text,
                    Some(1..1),
                    window,
                    editor_cx,
                );
            }
            assert_eq!(editor.undo_depth(), 1);
        });
    });
    let allocated = measurement.bytes();
    let (block_count, first_after, last_after, undo_depth) = entity.read_with(cx, |editor, _| {
        (
            editor.document().block_count(),
            editor
                .document()
                .block(first_id)
                .expect("first block")
                .revision,
            editor
                .document()
                .block(last_id)
                .expect("last block")
                .revision,
            editor.undo_depth(),
        )
    });
    assert!(
        allocated < 32_000_000,
        "repeated IME candidates cloned the whole note/history: {allocated} bytes"
    );
    assert_eq!(block_count, 20_000);
    assert_eq!(first_after, first_revision);
    assert!(last_after > last_revision);
    assert_eq!(undo_depth, 1);
}

#[gpui::test]
fn return_replaces_selection_and_splits_as_one_undo(cx: &mut gpui::TestAppContext) {
    let mut editor = EditorCore::for_test("abcd", cx);
    let node = editor.document().first_node_id().expect("paragraph");
    editor.select_document_range(1, 3);
    editor.insert_paragraph_break().unwrap();
    assert_eq!(editor.document().block_count(), 2);
    assert_eq!(editor.document().text_at_index(0), Some("a"));
    assert_eq!(editor.document().text_at_index(1), Some("d"));
    assert_eq!(editor.undo_depth(), 1);
    editor.undo().unwrap();
    assert_eq!(editor.document().block_count(), 1);
    assert_eq!(editor.document().text_at_index(0), Some("abcd"));
    assert_eq!(editor.selection().head.node_id, node);
}

#[gpui::test]
async fn entity_input_replace_notifies_and_exposes_errors(cx: &mut gpui::TestAppContext) {
    let cx = cx.add_empty_window();
    let entity = cx.new(|cx| EditorCore::new(Document::from_paragraph("a"), cx));
    let notifications = Arc::new(AtomicUsize::new(0));
    let observed = Arc::clone(&notifications);
    let _subscription = cx.update(|_, cx| {
        cx.observe(&entity, move |_, _| {
            observed.fetch_add(1, Ordering::Relaxed);
        })
    });

    cx.update(|window, cx| {
        entity.update(cx, |editor, editor_cx| {
            <EditorCore as EntityInputHandler>::replace_text_in_range(
                editor,
                Some(0..1),
                "b",
                window,
                editor_cx,
            );
        });
    });
    cx.run_until_parked();
    assert!(notifications.load(Ordering::Relaxed) > 0);
    assert_eq!(entity.read_with(cx, |editor, _| editor.visible_text()), "b");

    let invalid_entity = cx.new(|cx| EditorCore::new(Document::from_paragraph("a\u{301}"), cx));
    cx.update(|window, cx| {
        invalid_entity.update(cx, |editor, editor_cx| {
            <EditorCore as EntityInputHandler>::replace_text_in_range(
                editor,
                Some(99..100),
                "x",
                window,
                editor_cx,
            );
        });
    });
    assert!(invalid_entity.read_with(cx, |editor, _| editor.input_error().is_some()));
}

#[gpui::test]
fn cross_block_selection_includes_image_atom(cx: &mut gpui::TestAppContext) {
    let mut editor = EditorCore::fixture_text_image_text("甲", "image", "乙", cx);
    editor.select_document_range(0, editor.document_len());
    assert_eq!(editor.copy_plain_text(), "甲\n\u{fffc}\n乙");
    editor.delete_selection().unwrap();
    assert_eq!(editor.document().block_kinds(), [BlockKind::Paragraph]);
    editor.undo().unwrap();
    assert_eq!(editor.copy_plain_text(), "甲\n\u{fffc}\n乙");
}

#[gpui::test]
fn editing_commands_cross_block_boundaries(cx: &mut gpui::TestAppContext) {
    let mut editor = EditorCore::fixture_text_image_list("甲", "image", "乙", cx);
    let left_id = editor.document().blocks()[0].id;
    let image_id = editor.document().blocks()[1].id;
    let list_id = editor.document().blocks()[2].id;
    editor.command_a();
    assert_eq!(editor.copy_plain_text(), "甲\n\u{fffc}\n乙");
    editor.cut_selection().unwrap();
    assert_eq!(editor.document().block_kinds(), [BlockKind::Paragraph]);
    editor.undo().unwrap();
    editor.redo().unwrap();
    assert_eq!(editor.document().block_kinds(), [BlockKind::Paragraph]);
    editor.undo().unwrap();
    editor.move_to_image_after();
    editor.move_left();
    assert!(editor.caret_is_before_image());
    editor.move_right();
    assert!(editor.caret_is_after_image());
    editor.move_up();
    assert_eq!(editor.selection().head.node_id, left_id);
    editor.move_end();
    assert_eq!(editor.selection().head.utf8_offset, "甲".len());
    editor.move_down();
    assert_eq!(editor.selection().head.node_id, list_id);
    editor.move_home();
    assert_eq!(editor.selection().head.utf8_offset, 0);
    editor.move_up();
    assert_eq!(editor.selection().head.node_id, left_id);
    editor.move_to_image_after();
    editor.insert_paragraph_break().unwrap();
    editor.undo().unwrap();
    editor.backspace().unwrap();
    editor.undo().unwrap();
    editor.move_to_image_before();
    editor.delete_forward().unwrap();
    editor.undo().unwrap();
    editor.command_a();
    editor.paste_plain_text("跨块").unwrap();
    assert_eq!(editor.visible_text(), "跨块");
    editor.undo().unwrap();
    editor.redo().unwrap();
    assert_eq!(editor.visible_text(), "跨块");
    editor.undo().unwrap();
    assert_eq!(editor.document().blocks()[1].id, image_id);
    editor.command_a();
    assert_eq!(editor.copy_plain_text(), "甲\n\u{fffc}\n乙");
}

#[gpui::test]
async fn entity_input_uses_document_wide_utf16_coordinates(cx: &mut gpui::TestAppContext) {
    let mut cx = cx.add_empty_window();
    let editor = EditorCore::fixture_text_image_list("甲", "image", "乙", &mut cx);
    let entity = cx.new(|_| editor);

    cx.update(|window, cx| {
        entity.update(cx, |editor, editor_cx| {
            let document = editor.document().clone();
            editor.layout.layout_document(&document, 0.0, 640.0, 680.0);
            let mut actual_range = None;
            let text = <EditorCore as EntityInputHandler>::text_for_range(
                editor,
                0..5,
                &mut actual_range,
                window,
                editor_cx,
            )
            .expect("full document input range");
            assert_eq!(text, "甲\n\u{fffc}\n乙");
            assert_eq!(actual_range, Some(0..5));

            editor.select_document_range(0, editor.document_len());
            let selected = <EditorCore as EntityInputHandler>::selected_text_range(
                editor, false, window, editor_cx,
            )
            .expect("document selection");
            assert_eq!(selected.range, 0..5);
            assert!(!selected.reversed);

            let image = editor
                .layout
                .visible()
                .iter()
                .find(|block| editor.layout.is_image(block.node_id))
                .cloned()
                .expect("image layout");
            let image_after = <EditorCore as EntityInputHandler>::character_index_for_point(
                editor,
                point(
                    image.bounds.right() - px(2.0),
                    image.bounds.bottom() - px(2.0),
                ),
                window,
                editor_cx,
            )
            .expect("image hit index");
            assert_eq!(image_after, 3);

            let bounds = <EditorCore as EntityInputHandler>::bounds_for_range(
                editor,
                0..5,
                Bounds::default(),
                window,
                editor_cx,
            )
            .expect("cross-block range bounds");
            assert!(bounds.size.width > px(0.0));
            assert!(bounds.size.height > px(0.0));

            <EditorCore as EntityInputHandler>::replace_and_mark_text_in_range(
                editor,
                Some(1..4),
                "中新",
                Some(1..3),
                window,
                editor_cx,
            );
            assert_eq!(editor.visible_text(), "甲中新乙");
            assert_eq!(editor.marked_text(), Some("中新"));
            assert_eq!(
                <EditorCore as EntityInputHandler>::marked_text_range(editor, window, editor_cx,),
                Some(1..3)
            );
            editor.undo().unwrap();
            let document = editor.document().clone();
            editor.layout.layout_document(&document, 0.0, 640.0, 680.0);
            <EditorCore as EntityInputHandler>::replace_text_in_range(
                editor,
                Some(1..4),
                "新",
                window,
                editor_cx,
            );
            assert_eq!(editor.visible_text(), "甲新乙");
            assert_eq!(editor.document().block_kinds(), [BlockKind::Paragraph]);
        });
    });
}

#[gpui::test]
async fn entity_input_platform_commit_replaces_candidate_as_one_undo(
    cx: &mut gpui::TestAppContext,
) {
    let mut cx = cx.add_empty_window();
    let entity = cx.new(|cx| EditorCore::new(Document::from_paragraph("前后"), cx));

    cx.update(|window, cx| {
        entity.update(cx, |editor, editor_cx| {
            editor.set_caret_utf8("前".len());
            <EditorCore as EntityInputHandler>::replace_and_mark_text_in_range(
                editor,
                None,
                "候选",
                Some(2..2),
                window,
                editor_cx,
            );
            assert!(
                editor.selection().is_caret(),
                "internal IME caret must be collapsed"
            );
            <EditorCore as EntityInputHandler>::replace_text_in_range(
                editor, None, "最终", window, editor_cx,
            );
        });
    });

    assert_eq!(
        entity.read_with(cx, |editor, _| editor.visible_text()),
        "前最终后"
    );
    assert_eq!(entity.read_with(cx, |editor, _| editor.undo_depth()), 1);
    cx.update(|_, cx| {
        entity.update(cx, |editor, _| editor.undo().unwrap());
    });
    assert_eq!(
        entity.read_with(cx, |editor, _| editor.visible_text()),
        "前后"
    );
}

#[gpui::test]
async fn entity_input_explicit_commit_range_and_navigation_cancel_composition(
    cx: &mut gpui::TestAppContext,
) {
    let mut cx = cx.add_empty_window();
    let entity = cx.new(|cx| EditorCore::new(Document::from_paragraph("abcd"), cx));

    cx.update(|window, cx| {
        entity.update(cx, |editor, editor_cx| {
            editor.set_caret_utf8(2);
            <EditorCore as EntityInputHandler>::replace_and_mark_text_in_range(
                editor,
                None,
                "候选",
                Some(2..2),
                window,
                editor_cx,
            );
            editor.move_home();
            <EditorCore as EntityInputHandler>::replace_and_mark_text_in_range(
                editor,
                None,
                "新",
                Some(1..1),
                window,
                editor_cx,
            );
            assert_eq!(editor.visible_text(), "新ab候选cd");
            <EditorCore as EntityInputHandler>::replace_text_in_range(
                editor,
                Some(0..1),
                "首",
                window,
                editor_cx,
            );
        });
    });
    assert_eq!(
        entity.read_with(cx, |editor, _| editor.visible_text()),
        "首ab候选cd"
    );
}

#[gpui::test]
async fn nonzero_viewport_reflow_keeps_document_block_index(cx: &mut gpui::TestAppContext) {
    let mut cx = cx.add_empty_window();
    let paragraphs = (0..240)
        .map(|index| {
            if index == 140 {
                "甲".repeat(600)
            } else {
                format!("block-{index}")
            }
        })
        .collect::<Vec<_>>();
    let document = Document::from_paragraphs(paragraphs);
    let target = document.blocks()[140].id;
    let mut layout = LayoutRegistry::new();
    layout.layout_document(&document, 0.0, 120.0, 32.0);
    let target_top_estimate = document
        .blocks()
        .iter_range(0..140)
        .map(|block| {
            layout
                .estimated_heights
                .get(&block.id)
                .copied()
                .unwrap_or(24.0)
        })
        .sum::<f32>();
    cx.update(|window, _| {
        layout.shape_visible_with_window(&document, target_top_estimate + 1.0, 120.0, 32.0, window);
    });
    assert!(layout.visible_range().start > 0);
    let target_layout = layout
        .block_layout(target)
        .expect("target must be prefetched");
    let expected_top = document
        .blocks()
        .iter_range(0..140)
        .map(|block| {
            layout
                .estimated_heights
                .get(&block.id)
                .copied()
                .unwrap_or(24.0)
        })
        .sum::<f32>();
    assert!((f32::from(target_layout.bounds.top()) - expected_top).abs() < 0.1);
    assert!(target_layout.bounds.size.height > px(24.0));
}

#[gpui::test]
async fn measured_reflow_recomputes_viewport_and_prefetch_membership(
    cx: &mut gpui::TestAppContext,
) {
    let mut cx = cx.add_empty_window();
    let document = Document::from_paragraphs((0..32).map(|index| format!("row-{index}")));
    let mut layout = LayoutRegistry::new();
    let (tall_end, tall_ids, expanded_ids) = cx.update(|window, _| {
        layout.shape_visible_with_window(&document, 0.0, 120.0, 680.0, window);

        let mut tall = window.text_style();
        tall.line_height = px(100.0).into();
        layout.shape_visible_with_style(&document, 0.0, 120.0, 680.0, tall, window);
        let tall_end = layout.visible_range().end;
        let tall_ids = layout
            .exact_cache_ids()
            .collect::<std::collections::HashSet<_>>();

        layout.shape_visible_with_window(&document, 0.0, 120.0, 680.0, window);
        assert!(layout.visible_range().end > tall_end);
        let expanded_ids = layout
            .exact_cache_ids()
            .collect::<std::collections::HashSet<_>>();
        (tall_end, tall_ids, expanded_ids)
    });
    let final_range = layout.visible_range();
    assert!(final_range.end > tall_end);
    assert!(expanded_ids.difference(&tall_ids).next().is_some());
    assert!(tall_ids.iter().all(|id| {
        document
            .blocks()
            .iter_range(final_range.clone())
            .any(|block| block.id == *id)
    }));
    assert!(expanded_ids.difference(&tall_ids).all(|id| {
        layout
            .block_layout(*id)
            .is_some_and(|block| !block.text_lines.is_empty())
    }));
}

#[gpui::test]
async fn removing_image_invalidates_old_geometry_before_next_layout(cx: &mut gpui::TestAppContext) {
    let mut cx = cx.add_empty_window();
    let editor = EditorCore::fixture_text_image_text("甲", "image", "乙", &mut cx);
    let entity = cx.new(|_| editor);
    let (image_id, image_center) = cx.update(|window, cx| {
        entity.update(cx, |editor, _| {
            let document = editor.document().clone();
            editor
                .layout
                .shape_visible_with_window(&document, 0.0, 640.0, 680.0, window);
            let image = editor
                .layout
                .visible()
                .iter()
                .find(|block| editor.layout.is_image(block.node_id))
                .expect("image layout")
                .clone();
            (
                image.node_id,
                point(
                    image.bounds.left() + image.bounds.size.width / 2.0,
                    image.bounds.top() + image.bounds.size.height / 2.0,
                ),
            )
        })
    });
    cx.update(|_, cx| {
        entity.update(cx, |editor, _| {
            editor
                .apply(Transaction::RemoveNode { node_id: image_id })
                .unwrap();
            let stale_hit = editor.layout.point_to_doc(image_center);
            assert!(stale_hit.is_none_or(|point| point.node_id != image_id));
            assert!(!editor.layout.exact_cache_ids().any(|id| id == image_id));
            assert!(
                !editor
                    .layout
                    .visible()
                    .iter()
                    .any(|block| block.node_id == image_id)
            );
        });
    });
}

#[gpui::test]
async fn hard_line_selection_uses_accumulated_y_and_entity_caret_bounds(
    cx: &mut gpui::TestAppContext,
) {
    let mut cx = cx.add_empty_window();
    let entity = cx.new(|cx| EditorCore::new(Document::from_paragraph("abc\ndef"), cx));
    cx.update(|window, cx| {
        entity.update(cx, |editor, editor_cx| {
            let document = editor.document().clone();
            editor
                .layout
                .shape_visible_with_window(&document, 0.0, 240.0, 680.0, window);
            let node = document.first_node_id().unwrap();
            let line_height = editor.layout.line_height(node).unwrap();
            let rects = editor.layout.selection_rects(Selection::new(
                DocPoint::with_affinity(node, 4, Affinity::Before),
                DocPoint::with_affinity(node, 7, Affinity::After),
            ));
            assert!(!rects.is_empty());
            assert!(rects.iter().all(|rect| rect.top() >= px(0.0) + line_height));
            let bounds = <EditorCore as EntityInputHandler>::bounds_for_range(
                editor,
                4..4,
                Bounds::default(),
                window,
                editor_cx,
            )
            .expect("collapsed range must expose caret bounds to IME");
            assert!(bounds.size.height >= line_height);
        });
    });
}

#[gpui::test]
async fn editor_core_invalidates_only_changed_shaped_node(cx: &mut gpui::TestAppContext) {
    let mut cx = cx.add_empty_window();
    let entity =
        cx.new(|cx| EditorCore::new(Document::from_paragraphs(["first", "second", "third"]), cx));
    let (changed, unchanged, initial_shapes) = cx.update(|window, cx| {
        entity.update(cx, |editor, _| {
            let document = editor.document().clone();
            editor
                .layout
                .shape_visible_with_window(&document, 0.0, 240.0, 680.0, window);
            (
                document.blocks()[1].id,
                document.blocks()[0].id,
                editor.layout.shape_count(),
            )
        })
    });
    cx.update(|window, cx| {
        entity.update(cx, |editor, _| {
            editor
                .apply(Transaction::InsertText {
                    selection: Selection::caret(DocPoint::with_affinity(
                        changed,
                        "second".len(),
                        Affinity::After,
                    )),
                    text: "!".into(),
                })
                .unwrap();
            let document = editor.document().clone();
            editor
                .layout
                .shape_visible_with_window(&document, 0.0, 240.0, 680.0, window);
            assert!(editor.layout.block_layout(unchanged).is_some());
            assert_eq!(editor.layout.shape_count(), initial_shapes + 1);
        });
    });
}

#[gpui::test]
fn editor_core_keeps_one_stable_focus_owner(cx: &mut gpui::TestAppContext) {
    let editor = EditorCore::for_test("唯一焦点", cx);
    let first = editor.focus_handle() as *const _;
    let second = editor.focus_handle() as *const _;
    assert_eq!(first, second);
}

#[gpui::test]
async fn shaped_layout_wraps_and_counts_hard_newline_bytes(cx: &mut gpui::TestAppContext) {
    let cx = cx.add_empty_window();
    let document = Document::from_paragraph("甲乙丙丁戊己庚辛\n壬癸");
    let node = document.first_node_id().expect("text block");
    let mut layout = LayoutRegistry::new();
    cx.update(|window, _| {
        layout.shape_visible_with_window(&document, 0.0, 240.0, 32.0, window);
    });
    let block = layout.block_layout(node).expect("shaped block");
    assert!(block.bounds.size.height > px(24.0));
    let line_height = layout.line_height(node).expect("measured line height");
    let first_hard_line_height = block.text_lines[0].size(line_height).height;
    let second_line = point(
        block.bounds.left() + px(1.0),
        block.bounds.top() + first_hard_line_height + px(1.0),
    );
    let hit = layout
        .point_to_doc(second_line)
        .expect("second hard line hit");
    assert!(hit.utf8_offset >= "甲乙丙丁戊己庚辛\n".len());
}

#[gpui::test]
async fn shaped_wrapped_height_reflows_following_blocks(cx: &mut gpui::TestAppContext) {
    let cx = cx.add_empty_window();
    let document = Document::from_paragraphs(["甲乙丙丁戊己庚辛", "tail"]);
    let first = document.blocks()[0].id;
    let second = document.blocks()[1].id;
    let mut layout = LayoutRegistry::new();
    cx.update(|window, _| {
        layout.shape_visible_with_window(&document, 0.0, 240.0, 32.0, window);
    });
    let first_bounds = layout
        .block_layout(first)
        .expect("first shaped block")
        .bounds;
    let second_bounds = layout
        .block_layout(second)
        .expect("following shaped block")
        .bounds;
    assert!(first_bounds.size.height > px(24.0));
    assert!(second_bounds.top() >= first_bounds.bottom());
}

#[gpui::test]
async fn cross_viewport_selection_keeps_visible_segments(_cx: &mut gpui::TestAppContext) {
    let document = Document::from_paragraphs((0..200).map(|index| format!("block-{index}")));
    let first = document.blocks().first().expect("first block").id;
    let visible = document.blocks()[100].id;
    let mut layout = LayoutRegistry::new();
    layout.layout_document(&document, 2_400.0, 96.0, 680.0);
    let selection = Selection::new(
        DocPoint::with_affinity(first, 0, Affinity::Before),
        DocPoint::with_affinity(visible, "block-100".len(), Affinity::After),
    );
    let rects = layout.selection_rects(selection);
    assert!(
        !rects.is_empty(),
        "visible portion of offscreen selection vanished"
    );
}

#[test]
fn image_caret_has_affinity_without_selecting_the_atom() {
    let mut document = Document::from_paragraph("甲");
    document
        .apply(Transaction::InsertImage {
            selection: document.end_selection(),
            resource_id: "image".into(),
            natural_size: (320, 200),
        })
        .unwrap();
    let image = document.blocks()[1].id;
    let mut layout = LayoutRegistry::new();
    layout.layout_document(&document, 0.0, 240.0, 680.0);
    assert!(
        layout
            .selection_rects(Selection::caret(DocPoint::with_affinity(
                image,
                0,
                Affinity::Before,
            )))
            .is_empty()
    );
    let reverse = Selection::new(
        DocPoint::with_affinity(image, 0, Affinity::After),
        DocPoint::with_affinity(image, 0, Affinity::Before),
    );
    assert_eq!(layout.selection_rects(reverse).len(), 1);
}

#[gpui::test]
async fn shaped_cache_reuses_static_blocks_and_invalidates_changed_revision(
    cx: &mut gpui::TestAppContext,
) {
    let cx = cx.add_empty_window();
    let mut document = Document::from_paragraphs(["first", "second", "third"]);
    let changed = document.blocks()[1].id;
    let mut layout = LayoutRegistry::new();
    cx.update(|window, _| {
        layout.shape_visible_with_window(&document, 0.0, 240.0, 680.0, window);
    });
    let first_shape_count = layout.shape_count();
    cx.update(|window, _| {
        layout.shape_visible_with_window(&document, 0.0, 240.0, 680.0, window);
    });
    assert_eq!(layout.shape_count(), first_shape_count);
    document
        .apply(Transaction::InsertText {
            selection: Selection::caret(DocPoint::new(changed, "second".len())),
            text: "!".into(),
        })
        .unwrap();
    cx.update(|window, _| {
        layout.shape_visible_with_window(&document, 0.0, 240.0, 680.0, window);
    });
    assert_eq!(layout.shape_count(), first_shape_count + 1);
}

#[gpui::test]
async fn shaped_layout_budget_does_not_keep_rejected_visible_text(cx: &mut gpui::TestAppContext) {
    let cx = cx.add_empty_window();
    let document = Document::from_paragraph("x".repeat(128 * 1024));
    let mut layout = LayoutRegistry::with_budget(1_024);
    cx.update(|window, _| {
        layout.shape_visible_with_window(&document, 0.0, 240.0, 680.0, window);
    });
    assert!(layout.used_bytes() <= layout.budget_bytes());
    assert!(
        layout
            .visible()
            .iter()
            .all(|block| block.text_lines.is_empty())
    );
    assert!(layout.exact_cache_len() == 0 || layout.exact_cache_ids().count() == 1);
}

#[test]
fn register_exact_updates_visible_geometry_and_image_metadata() {
    let document = Document::from_paragraph("text");
    let block = document.blocks()[0].clone();
    let (before, after) = (
        DocPoint::with_affinity(block.id, 0, Affinity::Before),
        DocPoint::with_affinity(block.id, 4, Affinity::After),
    );
    let layout = super::layout::BlockLayout {
        node_id: block.id,
        bounds: Bounds::new(point(px(0.0), px(0.0)), gpui::size(px(100.0), px(36.0))),
        text_inset: px(0.0),
        text_align: gpui::TextAlign::Left,
        text_lines: Vec::new(),
        before,
        after,
    };
    let mut registry = LayoutRegistry::new();
    registry.register_exact(1, 100.0, layout, 64, false, px(36.0));
    assert_eq!(registry.visible().len(), 1);
    assert_eq!(registry.line_height(block.id), Some(px(36.0)));

    let mut image_document = Document::from_paragraph("text");
    image_document
        .apply(Transaction::InsertImage {
            selection: image_document.end_selection(),
            resource_id: "measured-image".into(),
            natural_size: (100, 80),
        })
        .unwrap();
    let image = image_document.blocks()[1].clone();
    registry.register_exact(
        image.revision,
        100.0,
        super::layout::BlockLayout {
            node_id: image.id,
            bounds: Bounds::new(point(px(0.0), px(36.0)), gpui::size(px(100.0), px(88.0))),
            text_inset: px(0.0),
            text_align: gpui::TextAlign::Left,
            text_lines: Vec::new(),
            before: DocPoint::with_affinity(image.id, 0, Affinity::Before),
            after: DocPoint::with_affinity(image.id, 0, Affinity::After),
        },
        128,
        true,
        px(42.0),
    );
    assert!(registry.is_image(image.id));
    assert_eq!(registry.line_height(image.id), Some(px(42.0)));
}

#[gpui::test]
async fn visual_navigation_uses_wrapped_rows_and_preserves_home_end_scope(
    cx: &mut gpui::TestAppContext,
) {
    let mut cx = cx.add_empty_window();
    let mut editor = EditorCore::for_test("abc\ndef", &mut cx);
    let document = editor.document().clone();
    cx.update(|window, _| {
        editor
            .layout
            .shape_visible_with_window(&document, 0.0, 240.0, 680.0, window);
    });
    editor.set_caret_utf8(2);
    editor.move_down();
    let moved = editor.selection().head;
    assert!(
        moved.utf8_offset >= 4,
        "down must enter the second visual line"
    );
    editor.move_home();
    assert_eq!(editor.selection().head.utf8_offset, 4);
    editor.move_end();
    assert_eq!(editor.selection().head.utf8_offset, 7);
}

#[gpui::test]
async fn wrapped_click_home_end_keep_row_affinity_for_all_alignments(
    cx: &mut gpui::TestAppContext,
) {
    let cx = cx.add_empty_window();
    for alignment in [
        TextAlignment::Left,
        TextAlignment::Center,
        TextAlignment::Right,
    ] {
        let mut editor = EditorCore::for_test(&"0123456789".repeat(16), cx);
        let node = editor.document().first_node_id().expect("text block");
        let text_len = editor.document().text_at_index(0).unwrap().len();
        editor
            .apply(Transaction::SetAlignment {
                selection: Selection::new(
                    DocPoint::with_affinity(node, 0, Affinity::Before),
                    DocPoint::with_affinity(node, text_len, Affinity::After),
                ),
                alignment,
            })
            .expect("alignment transaction");
        let document = editor.document().clone();
        cx.update(|window, _| {
            editor
                .layout
                .shape_visible_with_window(&document, 0.0, 800.0, 96.0, window);
        });
        let block_bounds = editor
            .layout()
            .block_layout(node)
            .expect("wrapped block")
            .bounds;
        let line = editor
            .layout()
            .block_layout(node)
            .expect("wrapped block")
            .text_lines
            .first()
            .expect("hard line");
        let seam = line
            .wrap_boundaries()
            .first()
            .map(|boundary| line.runs()[boundary.run_ix].glyphs[boundary.glyph_ix].index)
            .expect("soft-wrap seam");
        let line_height = editor.layout().line_height(node).expect("line height");

        let click = point(
            block_bounds.right() - px(1.0),
            block_bounds.top() + line_height / 2.0,
        );
        let clicked = editor
            .point_from_layout(click)
            .expect("production hit-test point");
        assert_eq!(clicked.utf8_offset, seam);
        assert_eq!(
            clicked.affinity,
            Affinity::Before,
            "clicking a non-final row's end must stay on that row"
        );
        editor.set_selection_for_test(Selection::caret(clicked));
        let clicked_bounds = editor
            .layout()
            .caret_bounds_for_point(clicked)
            .expect("clicked caret geometry");
        assert_eq!(clicked_bounds.top(), block_bounds.top());

        editor.move_home();
        let home = editor.selection().head;
        assert_eq!(home.utf8_offset, 0);
        assert_eq!(home.affinity, Affinity::After);
        assert_eq!(
            editor.layout().caret_bounds_for_point(home).unwrap().top(),
            block_bounds.top()
        );

        editor.set_selection_for_test(Selection::caret(clicked));
        editor.move_end();
        let end = editor.selection().head;
        assert_eq!(end.utf8_offset, seam);
        assert_eq!(
            end.affinity,
            Affinity::Before,
            "End on a non-final wrapped row must use Before affinity"
        );
        editor.move_down();
        let next = editor.selection().head;
        assert!(next.utf8_offset > seam);
        editor.move_up();
        assert_eq!(editor.selection().head.utf8_offset, seam);
        assert_eq!(editor.selection().head.affinity, Affinity::Before);

        // The start of the post-wrap row is a distinct affinity at the same
        // UTF-8 seam. Hit-testing must use y to choose that row, including
        // center/right alignment where the row has different slack.
        let row_start = DocPoint::with_affinity(node, seam, Affinity::After);
        let row_start_bounds = editor
            .layout()
            .caret_bounds_for_point(row_start)
            .expect("post-wrap row-start caret");
        let row_start_click = point(
            row_start_bounds.left(),
            block_bounds.top() + line_height * 1.5,
        );
        let row_start_hit = editor
            .point_from_layout(row_start_click)
            .expect("post-wrap row-start hit");
        assert_eq!(row_start_hit.utf8_offset, seam);
        assert_eq!(row_start_hit.affinity, Affinity::After);
        editor.set_selection_for_test(Selection::caret(row_start_hit));
        editor.move_home();
        assert_eq!(editor.selection().head, row_start);

        // A grapheme-shaped fixture exercises a seam whose adjacent text is
        // multi-code-point (emoji plus combining sequence), rather than only
        // ASCII byte boundaries.
        let grapheme_text = "🙂e\u{301}".repeat(24);
        let mut grapheme_editor = EditorCore::for_test(&grapheme_text, cx);
        let grapheme_node = grapheme_editor
            .document()
            .first_node_id()
            .expect("grapheme block");
        let grapheme_len = grapheme_editor
            .document()
            .text_at_index(0)
            .expect("grapheme text")
            .len();
        grapheme_editor
            .apply(Transaction::SetAlignment {
                selection: Selection::new(
                    DocPoint::with_affinity(grapheme_node, 0, Affinity::Before),
                    DocPoint::with_affinity(grapheme_node, grapheme_len, Affinity::After),
                ),
                alignment,
            })
            .expect("grapheme alignment transaction");
        let grapheme_document = grapheme_editor.document().clone();
        cx.update(|window, _| {
            grapheme_editor.layout.shape_visible_with_window(
                &grapheme_document,
                0.0,
                800.0,
                96.0,
                window,
            );
        });
        let grapheme_line = grapheme_editor
            .layout()
            .block_layout(grapheme_node)
            .expect("grapheme layout")
            .text_lines
            .first()
            .expect("grapheme hard line");
        let grapheme_seam = grapheme_line
            .wrap_boundaries()
            .first()
            .map(|boundary| grapheme_line.runs()[boundary.run_ix].glyphs[boundary.glyph_ix].index)
            .expect("grapheme soft-wrap seam");
        let grapheme_row_end = grapheme_line
            .wrap_boundaries()
            .get(1)
            .map(|boundary| grapheme_line.runs()[boundary.run_ix].glyphs[boundary.glyph_ix].index)
            .unwrap_or_else(|| grapheme_line.len());
        let grapheme_line_len = grapheme_line.len();
        assert!(
            grapheme_line
                .text
                .grapheme_indices(true)
                .any(|(offset, _)| offset == grapheme_seam),
            "the wrapped seam must be a grapheme boundary"
        );
        let grapheme_bounds = grapheme_editor
            .layout()
            .block_layout(grapheme_node)
            .expect("grapheme block layout")
            .bounds;
        let grapheme_height = grapheme_editor
            .layout()
            .line_height(grapheme_node)
            .expect("grapheme line height");
        let grapheme_start = DocPoint::with_affinity(grapheme_node, grapheme_seam, Affinity::After);
        let grapheme_start_x = grapheme_editor
            .layout()
            .caret_bounds_for_point(grapheme_start)
            .expect("grapheme row start")
            .left();
        let grapheme_hit = grapheme_editor
            .point_from_layout(point(
                grapheme_start_x,
                grapheme_bounds.top() + grapheme_height * 1.5,
            ))
            .expect("grapheme row-start hit");
        assert_eq!(grapheme_hit, grapheme_start);
        grapheme_editor.set_selection_for_test(Selection::caret(grapheme_hit));
        grapheme_editor.move_home();
        assert_eq!(grapheme_editor.selection().head, grapheme_start);
        grapheme_editor.move_end();
        let grapheme_end = grapheme_editor.selection().head;
        assert_eq!(grapheme_end.utf8_offset, grapheme_row_end);
        assert_eq!(
            grapheme_end.affinity,
            if grapheme_row_end < grapheme_line_len {
                Affinity::Before
            } else {
                Affinity::After
            }
        );
    }
}

#[gpui::test]
fn image_home_end_stay_on_the_image_atom(cx: &mut gpui::TestAppContext) {
    let mut editor = EditorCore::fixture_text_image_text("甲", "image", "乙", cx);
    let image = editor.document().blocks()[1].id;
    editor.move_to_image_before();
    editor.move_home();
    assert_eq!(editor.selection().head.node_id, image);
    editor.move_to_image_after();
    editor.move_end();
    assert_eq!(editor.selection().head.node_id, image);
}

#[gpui::test]
fn heterogeneous_backspace_downgrades_list_boundary(cx: &mut gpui::TestAppContext) {
    let mut editor = EditorCore::for_test("ab", cx);
    editor.insert_paragraph_break().unwrap();
    editor.insert_text("tail").unwrap();
    let second = editor.selection().head.node_id;
    let second_len = editor.document().text_at_index(1).unwrap().len();
    editor
        .apply(Transaction::SetBlockKind {
            selection: Selection::new(
                DocPoint::with_affinity(second, 0, Affinity::Before),
                DocPoint::with_affinity(second, second_len, Affinity::After),
            ),
            kind: BlockKind::BulletItem { depth: 0 },
        })
        .unwrap();
    editor.set_caret_utf8(0);
    let before = editor.undo_depth();
    editor.backspace().unwrap();
    assert_eq!(editor.document().blocks()[1].kind, BlockKind::Paragraph);
    assert_eq!(editor.undo_depth(), before + 1);
}

#[gpui::test]
fn heterogeneous_delete_forward_merges_into_left_kind(cx: &mut gpui::TestAppContext) {
    let mut editor = EditorCore::for_test("ab", cx);
    editor.insert_paragraph_break().unwrap();
    let second = editor.selection().head.node_id;
    let second_len = editor.document().text_at_index(1).unwrap().len();
    editor
        .apply(Transaction::SetBlockKind {
            selection: Selection::new(
                DocPoint::with_affinity(second, 0, Affinity::Before),
                DocPoint::with_affinity(second, second_len, Affinity::After),
            ),
            kind: BlockKind::BulletItem { depth: 0 },
        })
        .unwrap();
    let first = editor.document().blocks()[0].id;
    editor.set_caret_utf8("ab".len());
    // set_caret_utf8 follows the active second block; explicitly place the
    // caret at the paragraph/list seam for the forward-delete assertion.
    editor.select_document_range(2, 2);
    assert_eq!(editor.selection().head.node_id, first);
    editor.delete_forward().unwrap();
    assert_eq!(editor.document().block_count(), 1);
    assert_eq!(editor.document().text_at_index(0), Some("ab"));
    assert_eq!(editor.document().blocks()[0].kind, BlockKind::Paragraph);
}

#[gpui::test]
fn public_and_fallback_positions_snap_to_graphemes(cx: &mut gpui::TestAppContext) {
    let mut editor = EditorCore::for_test("a\u{301}b", cx);
    editor.set_caret_utf8(1);
    assert_eq!(editor.selection().head.utf8_offset, "a\u{301}".len());

    let document = editor.document().clone();
    let block = document.blocks()[0].clone();
    let mut layout = LayoutRegistry::new();
    layout.register_exact(
        block.revision,
        100.0,
        super::layout::BlockLayout {
            node_id: block.id,
            bounds: Bounds::new(point(px(0.0), px(0.0)), gpui::size(px(100.0), px(24.0))),
            text_inset: px(0.0),
            text_align: gpui::TextAlign::Left,
            text_lines: Vec::new(),
            before: DocPoint::with_affinity(block.id, 0, Affinity::Before),
            after: DocPoint::with_affinity(block.id, "a\u{301}b".len(), Affinity::After),
        },
        64,
        false,
        px(26.0),
    );
    let hit = layout
        .point_to_doc(point(px(8.0), px(4.0)))
        .expect("fallback hit should remain document-valid");
    assert_ne!(hit.utf8_offset, 1);
}

#[gpui::test]
fn editor_core_orders_same_image_affinity_for_contraction(cx: &mut gpui::TestAppContext) {
    let mut editor = EditorCore::fixture_text_image_text("甲", "image", "乙", cx);
    let image = editor.document().blocks()[1].id;
    let before_offset = "甲\n".len();
    let after_offset = before_offset + "\u{fffc}".len();
    editor.select_document_range(after_offset, before_offset);
    assert!(!editor.selection().is_caret());
    editor.move_left();
    assert_eq!(
        editor.selection().head,
        DocPoint::with_affinity(image, 0, Affinity::Before)
    );
    editor.select_document_range(after_offset, before_offset);
    editor.move_right();
    assert_eq!(
        editor.selection().head,
        DocPoint::with_affinity(image, 0, Affinity::After)
    );
}

#[gpui::test]
async fn visual_down_preserves_empty_hard_line_as_one_row(cx: &mut gpui::TestAppContext) {
    let mut cx = cx.add_empty_window();
    let mut editor = EditorCore::for_test("abc\n\ndef", &mut cx);
    let document = editor.document().clone();
    cx.update(|window, _| {
        editor
            .layout
            .shape_visible_with_window(&document, 0.0, 240.0, 680.0, window);
    });
    editor.set_caret_utf8(2);
    editor.move_down();
    assert_eq!(editor.selection().head.utf8_offset, "abc\n".len());
    editor.move_down();
    assert!(editor.selection().head.utf8_offset >= "abc\n\n".len());
}

#[gpui::test]
fn structural_edges_preserve_caret_after_backspace_downgrade(cx: &mut gpui::TestAppContext) {
    let mut first = EditorCore::for_test("first", cx);
    first.set_caret_utf8(0);
    let before = first.document().semantic_snapshot();
    let undo_depth = first.undo_depth();
    first.backspace().unwrap();
    assert_eq!(first.document().semantic_snapshot(), before);
    assert_eq!(first.undo_depth(), undo_depth);

    let mut editor = EditorCore::for_test("ab", cx);
    editor.insert_paragraph_break().unwrap();
    editor.insert_text("tail").unwrap();
    let second = editor.selection().head.node_id;
    let second_len = editor.document().text_at_index(1).unwrap().len();
    editor
        .apply(Transaction::SetBlockKind {
            selection: Selection::new(
                DocPoint::with_affinity(second, 0, Affinity::Before),
                DocPoint::with_affinity(second, second_len, Affinity::After),
            ),
            kind: BlockKind::BulletItem { depth: 0 },
        })
        .unwrap();
    editor.set_caret_utf8(0);
    editor.backspace().unwrap();
    assert_eq!(editor.document().text_at_index(1), Some("tail"));
    editor.insert_text("X").unwrap();
    assert_eq!(editor.document().text_at_index(1), Some("Xtail"));
}

#[gpui::test]
fn first_block_backspace_deletes_text_but_offset_zero_is_noop(cx: &mut gpui::TestAppContext) {
    let mut editor = EditorCore::for_test("abc", cx);
    editor.set_caret_utf8(3);
    editor.backspace().unwrap();
    assert_eq!(editor.visible_text(), "ab");

    editor.set_caret_utf8(0);
    let before = editor.document().semantic_snapshot();
    let undo_depth = editor.undo_depth();
    editor.backspace().unwrap();
    assert_eq!(editor.document().semantic_snapshot(), before);
    assert_eq!(editor.undo_depth(), undo_depth);
}

#[gpui::test]
async fn shaped_cache_budget_is_hard_during_multi_block_shaping(cx: &mut gpui::TestAppContext) {
    let mut cx = cx.add_empty_window();
    let budget = 64 * 1024;
    let single_document = Document::from_paragraph("single-".to_owned() + &"x".repeat(512));
    let mut single_layout = LayoutRegistry::with_budget(budget);
    cx.update(|window, _| {
        single_layout.shape_visible_with_window(&single_document, 0.0, 480.0, 160.0, window);
    });
    assert_eq!(single_layout.exact_cache_len(), 1);
    assert!(single_layout.used_bytes() > 0);
    assert!(single_layout.peak_accounted_bytes() <= budget);

    let document =
        Document::from_paragraphs((0..12).map(|index| format!("{index}-{}", "x".repeat(512))));
    assert!(
        single_layout
            .used_bytes()
            .saturating_mul(document.block_count())
            > budget
    );
    let mut layout = LayoutRegistry::with_budget(budget);
    let mut peak_used = 0;
    cx.update(|window, _| {
        layout.shape_visible_with_window(&document, 0.0, 10_000.0, 160.0, window);
        peak_used = peak_used.max(layout.used_bytes());
    });
    assert!(layout.exact_cache_len() > 0);
    assert!(layout.exact_cache_len() < document.block_count());
    assert!(layout.cache_bytes() <= layout.budget_bytes());
    assert!(layout.used_bytes() <= budget);
    assert!(peak_used <= budget);
    assert!(layout.peak_accounted_bytes() <= budget);
}

#[gpui::test]
async fn alternating_decorations_account_for_wrapped_line_clone_peak(
    cx: &mut gpui::TestAppContext,
) {
    let mut cx = cx.add_empty_window();
    let text = "x".repeat(768);
    let mut document = Document::from_paragraph(text.clone());
    let node = document.first_node_id().expect("decorated paragraph");
    let transactions: Vec<Transaction> = (0..text.len())
        .map(|offset| {
            let mark = match offset % 4 {
                0 => Mark::Bold,
                1 => Mark::Italic,
                2 => Mark::Underline,
                _ => Mark::Highlight,
            };
            Transaction::ToggleMark {
                selection: Selection::new(
                    DocPoint::with_affinity(node, offset, Affinity::Before),
                    DocPoint::with_affinity(node, offset + 1, Affinity::After),
                ),
                mark,
            }
        })
        .collect();
    document
        .apply_batch(TransactionBatch(transactions.clone()))
        .expect("alternating decorations should validate");

    let budget = 2 * 1024 * 1024;
    let mut layout = LayoutRegistry::with_budget(budget);
    cx.update(|window, _| {
        layout.shape_visible_with_window(&document, 0.0, 4_000.0, 680.0, window);
    });
    assert!(layout.used_bytes() <= budget);
    assert!(layout.peak_accounted_bytes() <= budget);
    let cached = layout.cache.get(&node).expect("decorated paragraph cache");
    assert!(
        cached.decoration_run_count > 32,
        "the fixture must exercise SmallVec decoration spill capacity"
    );
}

#[gpui::test]
async fn each_hard_line_accounts_for_its_own_decoration_spill_and_clone_peak(
    cx: &mut gpui::TestAppContext,
) {
    let mut cx = cx.add_empty_window();
    let line = "x".repeat(40);
    let text = format!("{line}\n{line}\n{line}");
    let mut document = Document::from_paragraph(text.clone());
    let node = document.first_node_id().expect("decorated paragraph");
    let mut transactions = Vec::new();
    let mut offset = 0;
    for (line_index, hard_line) in text.split('\n').enumerate() {
        for (column, _) in hard_line.bytes().enumerate() {
            let mark = if (column + line_index) % 2 == 0 {
                Mark::Bold
            } else {
                Mark::Highlight
            };
            transactions.push(Transaction::ToggleMark {
                selection: Selection::new(
                    DocPoint::with_affinity(node, offset, Affinity::Before),
                    DocPoint::with_affinity(node, offset + 1, Affinity::After),
                ),
                mark,
            });
            offset += 1;
        }
        offset += 1;
    }
    document
        .apply_batch(TransactionBatch(transactions.clone()))
        .expect("three hard-line decoration fixture");
    let mut layout = LayoutRegistry::with_budget(LAYOUT_CACHE_BUDGET_BYTES);
    cx.update(|window, _| {
        layout.shape_visible_with_window(&document, 0.0, 4_000.0, 680.0, window);
    });
    let cached = layout.cache.get(&node).expect("decorated paragraph cache");
    assert_eq!(cached.layout.text_lines.len(), 3);
    assert!(cached.decoration_run_count >= 120);
    assert_eq!(cached.decoration_line_run_counts.as_slice(), &[40, 40, 40]);
    assert!(
        cached
            .decoration_line_run_counts
            .iter()
            .all(|count| *count > 32)
    );
    let line_capacity = cached.layout.text_lines.capacity();
    let decoration_run_bytes = size_of::<gpui::DecorationRun>();
    let old_block_wide_capacity = cached
        .decoration_run_count
        .next_power_of_two()
        .saturating_mul(decoration_run_bytes);
    let old_retained_spill =
        old_block_wide_capacity.saturating_sub(32usize.saturating_mul(decoration_run_bytes));
    let corrected_spill = cached
        .decoration_line_run_counts
        .iter()
        .map(|count| {
            count
                .next_power_of_two()
                .saturating_mul(decoration_run_bytes)
        })
        .sum::<usize>();
    assert!(
        corrected_spill.saturating_mul(2)
            > old_retained_spill.saturating_add(old_block_wide_capacity),
        "fixture must distinguish per-line and block-wide capacities"
    );
    assert!(
        cached.snapshot_clone_bytes
            >= line_capacity.saturating_mul(size_of::<gpui::WrappedLine>())
                + size_of::<Vec<gpui::WrappedLine>>()
                + corrected_spill,
        "snapshot clone estimate omitted an independent hard-line spill"
    );

    // Exercise the real WrappedLine clone path, not only the accounting
    // formula. The isolated helper clones the exact per-visible-block Vec so
    // its allocator bytes include each spilled SmallVec backing store.
    let editor_transactions = transactions.clone();
    let mut editor = EditorCore::for_test(&text, &mut cx);
    for transaction in editor_transactions {
        editor.apply(transaction).expect("decorate editor fixture");
    }
    cx.update(|window, _| {
        editor.shape_visible_with_window(0.0, 4_000.0, 680.0, window);
    });
    let observed_wrapped_line_clone = render::wrapped_line_clone_allocation_bytes_for_test(&editor);
    let observed_snapshot = render::snapshot_allocation_bytes_for_test(&editor);
    assert!(observed_wrapped_line_clone > 0);
    assert!(
        cached.snapshot_clone_bytes >= observed_wrapped_line_clone,
        "snapshot clone estimate {estimate} missed real WrappedLine clone allocation {observed}",
        estimate = cached.snapshot_clone_bytes,
        observed = observed_wrapped_line_clone,
    );
    assert!(observed_snapshot >= observed_wrapped_line_clone);

    // Put admission strictly between the old block-wide estimate and the
    // corrected retained+clone total. The shaped block must be rejected even
    // though the old estimate would have admitted it.
    let old_underestimate = cached.bytes.saturating_sub(
        corrected_spill
            .saturating_mul(2)
            .saturating_sub(old_retained_spill.saturating_add(old_block_wide_capacity)),
    );
    let corrected_total = cached.bytes;
    let admission_budget = old_underestimate
        .saturating_add(corrected_total.saturating_sub(old_underestimate) / 2)
        .max(old_underestimate.saturating_add(1));
    assert!(old_underestimate < admission_budget);
    assert!(admission_budget < corrected_total);
    let mut constrained = LayoutRegistry::with_budget(admission_budget);
    cx.update(|window, _| {
        constrained.shape_visible_with_window(&document, 0.0, 4_000.0, 680.0, window);
    });
    assert_eq!(
        constrained.exact_cache_len(),
        0,
        "a budget between old and corrected decoration totals must reject the block"
    );
    assert!(constrained.used_bytes() <= admission_budget);
}

#[gpui::test]
async fn shaped_cache_invalidates_font_family_weight_style_and_size(cx: &mut gpui::TestAppContext) {
    let mut cx = cx.add_empty_window();
    let document = Document::from_paragraph("cache-key");
    let mut layout = LayoutRegistry::new();
    cx.update(|window, _| {
        layout.shape_visible_with_window(&document, 0.0, 240.0, 680.0, window);
    });
    let first_shape_count = layout.shape_count();
    cx.update(|window, _| {
        let mut style: TextStyle = window.text_style();
        style.font_family = "Menlo".into();
        layout.shape_visible_with_style(&document, 0.0, 240.0, 680.0, style.clone(), window);
        assert_eq!(layout.shape_count(), first_shape_count + 1);

        style.font_weight = FontWeight::BOLD;
        layout.shape_visible_with_style(&document, 0.0, 240.0, 680.0, style.clone(), window);
        assert_eq!(layout.shape_count(), first_shape_count + 2);

        style.font_style = FontStyle::Italic;
        layout.shape_visible_with_style(&document, 0.0, 240.0, 680.0, style.clone(), window);
        assert_eq!(layout.shape_count(), first_shape_count + 3);

        style.font_size = px(28.0).into();
        layout.shape_visible_with_style(&document, 0.0, 240.0, 680.0, style, window);
    });
    assert_eq!(layout.shape_count(), first_shape_count + 4);
}

#[gpui::test]
fn toolbar_and_overflow_execute_same_command(cx: &mut gpui::TestAppContext) {
    let mut editor = EditorCore::for_test("第一行\n第二行", cx);
    editor.select_all();
    let catalogue = CommandCatalogue::default();
    catalogue
        .execute(
            EditorCommand::BulletList,
            CommandArgument::None,
            &mut editor,
        )
        .unwrap();
    assert!(
        editor
            .document()
            .block_kinds()
            .iter()
            .all(|kind| matches!(kind, BlockKind::BulletItem { .. }))
    );
    assert_eq!(editor.undo_depth(), 1);
    catalogue
        .execute(EditorCommand::Undo, CommandArgument::None, &mut editor)
        .unwrap();
    assert_eq!(editor.document().block_kinds(), [BlockKind::Paragraph]);
}

#[gpui::test]
fn all_visible_commands_execute_or_are_disabled(cx: &mut gpui::TestAppContext) {
    let catalogue = CommandCatalogue::default();
    let descriptors = catalogue.descriptors();
    let unique_commands = descriptors
        .iter()
        .map(|descriptor| descriptor.command)
        .collect::<std::collections::HashSet<_>>();
    assert_eq!(unique_commands.len(), descriptors.len());

    for descriptor in descriptors {
        let mut editor = EditorCore::for_test("第一行\n第二行", cx);
        editor.select_all();
        let before = editor.document().semantic_snapshot();
        let before_undo = editor.undo_depth();
        let argument = if descriptor.command == EditorCommand::Link {
            CommandArgument::LinkUrl("https://example.com".into())
        } else {
            CommandArgument::None
        };
        let state = catalogue.state(descriptor.command, &editor);
        let current_state_no_op = state.toggle == ToggleState::On
            && matches!(
                descriptor.command,
                EditorCommand::Paragraph
                    | EditorCommand::Heading1
                    | EditorCommand::Heading2
                    | EditorCommand::Heading3
                    | EditorCommand::BulletList
                    | EditorCommand::OrderedList
                    | EditorCommand::CheckList
                    | EditorCommand::AlignLeft
                    | EditorCommand::AlignCenter
                    | EditorCommand::AlignRight
            );
        if current_state_no_op {
            assert_eq!(
                state.toggle,
                ToggleState::On,
                "a current-state no-op must advertise its active state"
            );
            assert!(!state.enabled, "current-state no-op must be disabled");
            assert_eq!(editor.document().semantic_snapshot(), before);
            assert_eq!(editor.undo_depth(), before_undo);
            continue;
        }
        if !state.enabled {
            continue;
        }
        catalogue
            .execute(descriptor.command, argument, &mut editor)
            .unwrap_or_else(|error| {
                panic!(
                    "enabled visible command {:?} returned an error: {error:?}",
                    descriptor.command
                )
            });
        match descriptor.command {
            EditorCommand::Undo => {
                assert_ne!(editor.document().semantic_snapshot(), before);
                assert_eq!(editor.undo_depth(), before_undo.saturating_sub(1));
                assert_eq!(editor.redo_depth(), 1);
            }
            EditorCommand::Redo => {
                assert_ne!(editor.document().semantic_snapshot(), before);
                assert_eq!(editor.undo_depth(), before_undo + 1);
                assert_eq!(editor.redo_depth(), 0);
            }
            _ => {
                assert_ne!(
                    editor.document().semantic_snapshot(),
                    before,
                    "enabled visible command {:?} did not change the document",
                    descriptor.command
                );
                assert_eq!(editor.undo_depth(), before_undo + 1);
            }
        }
    }

    let mut editor = EditorCore::for_test("第一行\n第二行", cx);
    editor.select_all();
    catalogue
        .execute(
            EditorCommand::BulletList,
            CommandArgument::None,
            &mut editor,
        )
        .unwrap();
    assert!(catalogue.state(EditorCommand::IndentList, &editor).enabled);
    catalogue
        .execute(
            EditorCommand::IndentList,
            CommandArgument::None,
            &mut editor,
        )
        .unwrap();
    assert!(
        editor
            .document()
            .block_kinds()
            .iter()
            .all(|kind| matches!(kind, BlockKind::BulletItem { depth: 1 }))
    );
    assert_eq!(editor.undo_depth(), 2);
    assert_eq!(
        catalogue.state(EditorCommand::Undo, &editor).toggle,
        ToggleState::On
    );
    catalogue
        .execute(EditorCommand::Undo, CommandArgument::None, &mut editor)
        .unwrap();
    assert!(catalogue.state(EditorCommand::Redo, &editor).enabled);
}

#[gpui::test]
fn link_argument_validation_is_atomic_for_empty_and_invalid_urls(cx: &mut gpui::TestAppContext) {
    let catalogue = CommandCatalogue::default();
    let mut editor = EditorCore::for_test("linked text", cx);
    editor.select_all();
    let before = editor.document().semantic_snapshot();
    let before_undo = editor.undo_depth();

    assert_eq!(
        catalogue.execute(
            EditorCommand::Link,
            CommandArgument::LinkUrl("   ".into()),
            &mut editor,
        ),
        Err(CommandError::EmptyLinkUrl)
    );
    assert_eq!(
        catalogue.execute(
            EditorCommand::Link,
            CommandArgument::LinkUrl("not a url".into()),
            &mut editor,
        ),
        Err(CommandError::InvalidLinkUrl)
    );
    assert_eq!(editor.document().semantic_snapshot(), before);
    assert_eq!(editor.undo_depth(), before_undo);
}

#[gpui::test]
fn list_boundary_editing_preserves_structure(cx: &mut gpui::TestAppContext) {
    let catalogue = CommandCatalogue::default();
    let list_kinds = [
        BlockKind::BulletItem { depth: 0 },
        BlockKind::OrderedItem { depth: 0 },
        BlockKind::CheckItem {
            depth: 0,
            checked: false,
        },
    ];

    for list_kind in list_kinds {
        let mut editor = EditorCore::for_test_paragraphs(["一", "二", "三"], cx);
        editor.select_all();
        let original_selection = editor.selection();
        let original_document = editor.document().semantic_snapshot();
        catalogue
            .execute(
                match list_kind {
                    BlockKind::BulletItem { .. } => EditorCommand::BulletList,
                    BlockKind::OrderedItem { .. } => EditorCommand::OrderedList,
                    BlockKind::CheckItem { .. } => EditorCommand::CheckList,
                    _ => unreachable!(),
                },
                CommandArgument::None,
                &mut editor,
            )
            .unwrap();

        let ids = editor
            .document()
            .blocks()
            .iter()
            .map(|block| block.id)
            .collect::<Vec<_>>();
        let middle = ids[1];
        let middle_end = editor.document().text_at_index(1).unwrap().len();
        editor.set_selection_for_test(Selection::new(
            DocPoint::with_affinity(middle, 0, Affinity::Before),
            DocPoint::with_affinity(middle, middle_end, Affinity::After),
        ));
        catalogue
            .execute(
                EditorCommand::IndentList,
                CommandArgument::None,
                &mut editor,
            )
            .unwrap();

        editor.set_selection_for_test(Selection::caret(DocPoint::with_affinity(
            ids[0],
            0,
            Affinity::Before,
        )));
        editor.backspace().unwrap();
        assert!(matches!(
            editor.document().blocks()[0].kind,
            BlockKind::Paragraph
        ));
        assert_eq!(editor.document().blocks()[1].id, ids[1]);
        assert_eq!(editor.document().blocks()[2].id, ids[2]);

        editor.undo().unwrap();
        editor.undo().unwrap();
        editor.undo().unwrap();
        assert_eq!(editor.document().semantic_snapshot(), original_document);
        assert_eq!(editor.selection(), original_selection);
    }
}

#[gpui::test]
fn command_state_reports_on_off_and_mixed_from_real_selection(cx: &mut gpui::TestAppContext) {
    let catalogue = CommandCatalogue::default();
    let mut editor = EditorCore::for_test("ab\ncd", cx);
    let first = editor.document().blocks()[0].id;
    editor
        .apply(Transaction::ToggleMark {
            selection: Selection::new(
                DocPoint::with_affinity(first, 0, Affinity::Before),
                DocPoint::with_affinity(first, 2, Affinity::After),
            ),
            mark: Mark::Bold,
        })
        .unwrap();
    editor.select_all();
    let mixed = catalogue.state(EditorCommand::Bold, &editor);
    assert!(mixed.enabled);
    assert_eq!(mixed.toggle, ToggleState::Mixed);

    editor.select_document_range(0, 2);
    let on = catalogue.state(EditorCommand::Bold, &editor);
    assert!(on.enabled);
    assert_eq!(on.toggle, ToggleState::On);

    let mut fresh = EditorCore::for_test("ab", cx);
    fresh.select_all();
    let off = catalogue.state(EditorCommand::Bold, &fresh);
    assert!(off.enabled);
    assert_eq!(off.toggle, ToggleState::Off);

    assert!(catalogue.state(EditorCommand::Undo, &editor).enabled);
    editor.undo().unwrap();
    assert!(!catalogue.state(EditorCommand::Undo, &editor).enabled);
    assert!(catalogue.state(EditorCommand::Redo, &editor).enabled);
}

#[test]
fn spike_layout_centers_680_and_keeps_narrow_insets_and_bottom_padding() {
    let wide = layout_for_viewport(1_200.0, 800.0);
    assert_eq!(wide.content_width, 680.0);
    assert_eq!(wide.left_inset, 260.0);
    assert_eq!(wide.right_inset, 260.0);
    assert!((wide.bottom_padding - 240.0).abs() < 0.01);

    let narrow = layout_for_viewport(700.0, 600.0);
    assert_eq!(narrow.content_width, 636.0);
    assert_eq!(narrow.left_inset, 32.0);
    assert_eq!(narrow.right_inset, 32.0);
    assert!((narrow.bottom_padding - 180.0).abs() < 0.01);
}

#[test]
fn spike_route_uses_native_gpui_without_donor_services() {
    let contract: SpikeRouteContract = route_contract();
    assert_eq!(contract.window_count, 1);
    assert!(contract.uses_native_editor_entity);
    assert!(contract.uses_real_input_bridge);
    assert!(!contract.initializes_donor_services);
    assert!(!contract.initializes_web_runtime);
}

#[gpui::test]
fn mixed_max_depth_indent_is_disabled_before_atomic_execution(cx: &mut gpui::TestAppContext) {
    let catalogue = CommandCatalogue::default();
    let mut editor = EditorCore::for_test_paragraphs(["at-limit", "still-editable"], cx);
    let first = editor.document().blocks()[0].id;
    let second = editor.document().blocks()[1].id;
    editor
        .apply(Transaction::SetBlockKind {
            selection: Selection::new(
                DocPoint::with_affinity(first, 0, Affinity::Before),
                DocPoint::with_affinity(first, "at-limit".len(), Affinity::After),
            ),
            kind: BlockKind::BulletItem {
                depth: super::model::MAX_LIST_DEPTH,
            },
        })
        .unwrap();
    editor
        .apply(Transaction::SetBlockKind {
            selection: Selection::new(
                DocPoint::with_affinity(second, 0, Affinity::Before),
                DocPoint::with_affinity(second, "still-editable".len(), Affinity::After),
            ),
            kind: BlockKind::BulletItem { depth: 0 },
        })
        .unwrap();
    editor.select_all();

    let state = catalogue.state(EditorCommand::IndentList, &editor);
    assert!(
        !state.enabled,
        "an atomic mixed-depth indent must be disabled when any selected item is at MAX_LIST_DEPTH"
    );
    let before = editor.document().semantic_snapshot();
    let before_undo = editor.undo_depth();
    let result = catalogue.execute(
        EditorCommand::IndentList,
        CommandArgument::None,
        &mut editor,
    );
    assert!(
        result.is_err(),
        "direct execution must still reject the invalid batch"
    );
    assert_eq!(editor.document().semantic_snapshot(), before);
    assert_eq!(editor.undo_depth(), before_undo);
}

#[gpui::test]
fn collapsed_caret_state_and_insertion_share_affinity_boundary_rule(cx: &mut gpui::TestAppContext) {
    let catalogue = CommandCatalogue::default();
    let mut editor = EditorCore::for_test("ab", cx);
    let node = editor.document().blocks()[0].id;
    editor
        .apply(Transaction::ToggleMark {
            selection: Selection::new(
                DocPoint::with_affinity(node, 0, Affinity::Before),
                DocPoint::with_affinity(node, 1, Affinity::After),
            ),
            mark: Mark::Bold,
        })
        .unwrap();

    // Before the styled run is unstyled; after the run is styled. The same
    // answer must drive both the toolbar state and inserted-run inheritance.
    editor.set_selection_for_test(Selection::caret(DocPoint::with_affinity(
        node,
        0,
        Affinity::Before,
    )));
    assert_eq!(
        catalogue.state(EditorCommand::Bold, &editor).toggle,
        ToggleState::Off
    );
    editor.insert_text("X").unwrap();
    let styles = editor.document().blocks()[0]
        .content
        .styles()
        .expect("text styles");
    assert!(
        styles
            .iter()
            .any(|run| run.range == (1..2) && run.marks.contains(&Mark::Bold)),
        "the original styled run should remain attached after an unstyled insertion"
    );

    let mut editor = EditorCore::for_test("ab", cx);
    let node = editor.document().blocks()[0].id;
    editor
        .apply(Transaction::ToggleMark {
            selection: Selection::new(
                DocPoint::with_affinity(node, 0, Affinity::Before),
                DocPoint::with_affinity(node, 1, Affinity::After),
            ),
            mark: Mark::Bold,
        })
        .unwrap();
    editor.set_selection_for_test(Selection::caret(DocPoint::with_affinity(
        node,
        1,
        Affinity::Before,
    )));
    assert_eq!(
        catalogue.state(EditorCommand::Bold, &editor).toggle,
        ToggleState::On
    );
    editor.insert_text("X").unwrap();
    let styles = editor.document().blocks()[0]
        .content
        .styles()
        .expect("text styles");
    assert!(
        styles
            .iter()
            .any(|run| run.range == (0..2) && run.marks.contains(&Mark::Bold)),
        "insertion at the styled run's before-affinity seam must inherit Bold"
    );

    let mut editor = EditorCore::for_test("ab", cx);
    let node = editor.document().blocks()[0].id;
    editor
        .apply(Transaction::ToggleMark {
            selection: Selection::new(
                DocPoint::with_affinity(node, 0, Affinity::Before),
                DocPoint::with_affinity(node, 1, Affinity::After),
            ),
            mark: Mark::Bold,
        })
        .unwrap();
    editor.set_selection_for_test(Selection::caret(DocPoint::with_affinity(
        node,
        1,
        Affinity::After,
    )));
    assert_eq!(
        catalogue.state(EditorCommand::Bold, &editor).toggle,
        ToggleState::Off
    );
    editor.insert_text("Y").unwrap();
    let styles = editor.document().blocks()[0]
        .content
        .styles()
        .expect("text styles");
    assert!(
        styles
            .iter()
            .any(|run| run.range == (0..1) && run.marks.contains(&Mark::Bold)),
        "insertion at the styled run's after-affinity seam must remain unstyled"
    );
}

#[gpui::test]
async fn heading_and_list_layout_are_measured_as_distinct_render_geometry(
    cx: &mut gpui::TestAppContext,
) {
    let mut cx = cx.add_empty_window();
    let mut document = Document::from_paragraphs(["heading", "list item"]);
    let heading = document.blocks()[0].id;
    let list = document.blocks()[1].id;
    document
        .apply(Transaction::SetBlockKind {
            selection: Selection::new(
                DocPoint::with_affinity(heading, 0, Affinity::Before),
                DocPoint::with_affinity(heading, "heading".len(), Affinity::After),
            ),
            kind: BlockKind::Heading { level: 1 },
        })
        .unwrap();
    document
        .apply(Transaction::SetBlockKind {
            selection: Selection::new(
                DocPoint::with_affinity(list, 0, Affinity::Before),
                DocPoint::with_affinity(list, "list item".len(), Affinity::After),
            ),
            kind: BlockKind::BulletItem { depth: 2 },
        })
        .unwrap();

    let mut layout = LayoutRegistry::new();
    cx.update(|window, _| {
        layout.shape_visible_with_window(&document, 0.0, 240.0, 680.0, window);
    });
    let heading_height = layout
        .block_layout(heading)
        .expect("heading should be shaped")
        .bounds
        .size
        .height;
    let list_layout = layout.block_layout(list).expect("list should be shaped");
    assert!(
        heading_height > px(24.0),
        "heading style must affect measured line height"
    );
    assert!(
        list_layout.bounds.left() > px(0.0),
        "nested list marker indentation must participate in hit geometry"
    );
}

#[gpui::test]
fn shift_navigation_and_plain_clipboard_use_one_document_selection(cx: &mut gpui::TestAppContext) {
    let mut editor = EditorCore::for_test_paragraphs(["alpha", "beta"], cx);
    let first = editor.document().blocks()[0].id;
    let second = editor.document().blocks()[1].id;
    editor.set_selection_for_test(Selection::caret(DocPoint::with_affinity(
        first,
        "alpha".len(),
        Affinity::After,
    )));
    editor.select_left();
    editor.select_left();
    assert_eq!(editor.copy_plain_text(), "ha");
    let cut = editor.cut_selection().unwrap();
    assert_eq!(cut, "ha");
    assert_eq!(editor.document().text_at_index(0), Some("alp"));
    editor.paste_plain_text("XYZ").unwrap();
    assert_eq!(editor.document().text_at_index(0), Some("alpXYZ"));

    editor.set_selection_for_test(Selection::caret(DocPoint::with_affinity(
        first,
        "alpXYZ".len(),
        Affinity::After,
    )));
    editor.select_down();
    assert_eq!(editor.selection().head.node_id, second);
    assert!(!editor.selection().is_caret());
    editor.select_up();
    assert_eq!(editor.selection().head.node_id, first);
}

#[gpui::test]
async fn measured_total_height_includes_a_single_wrapped_block(cx: &mut gpui::TestAppContext) {
    let mut cx = cx.add_empty_window();
    let document = Document::from_paragraph("wrap ".repeat(240));
    let mut layout = LayoutRegistry::new();
    cx.update(|window, _| {
        layout.shape_visible_with_window(&document, 0.0, 48.0, 120.0, window);
    });
    assert!(
        layout.total_height() > 240.0,
        "measured wrap height must expose a scrollable extent rather than one guessed row"
    );
}

#[gpui::test]
async fn nested_list_wrapping_uses_one_inset_alignment_geometry(cx: &mut gpui::TestAppContext) {
    let mut cx = cx.add_empty_window();
    let mut document = Document::from_paragraph("nested ".repeat(80));
    let node = document.first_node_id().expect("paragraph");
    let text_len = document.text_at_index(0).expect("text").len();
    document
        .apply(Transaction::SetBlockKind {
            selection: Selection::new(
                DocPoint::with_affinity(node, 0, Affinity::Before),
                DocPoint::with_affinity(node, text_len, Affinity::After),
            ),
            kind: BlockKind::OrderedItem { depth: 2 },
        })
        .expect("nested ordered item conversion");
    document
        .apply(Transaction::SetAlignment {
            selection: Selection::new(
                DocPoint::with_affinity(node, 0, Affinity::Before),
                DocPoint::with_affinity(node, text_len, Affinity::After),
            ),
            alignment: TextAlignment::Center,
        })
        .expect("center alignment");
    let mut layout = LayoutRegistry::new();
    cx.update(|window, _| {
        layout.shape_visible_with_window(&document, 0.0, 4_000.0, 120.0, window);
    });
    let block = layout.block_layout(node).expect("nested block geometry");
    let text_width = (block.bounds.size.width - block.text_inset).max(px(1.0));
    assert!(
        block
            .text_lines
            .iter()
            .all(|line| line.width() <= text_width),
        "shaped rows must not spill into the list marker inset"
    );
    let rects = layout.selection_rects(Selection::new(block.before, block.after));
    assert!(!rects.is_empty());
    assert!(rects.iter().all(|rect| {
        rect.left() >= block.bounds.left() + block.text_inset - px(0.5)
            && rect.right() <= block.bounds.right() + px(0.5)
    }));
}

#[gpui::test]
async fn vertical_navigation_preserves_screen_x_across_list_inset_and_alignment(
    cx: &mut gpui::TestAppContext,
) {
    let mut cx = cx.add_empty_window();
    let mut editor = EditorCore::for_test_paragraphs(
        [
            "0123456789",
            "abcdefghijklmnopqrstuvwxabcdefghijklmnopqrstuvwx",
        ],
        &mut cx,
    );
    let first = editor.document().blocks()[0].id;
    let second = editor.document().blocks()[1].id;
    let first_len = editor.document().text_at_index(0).unwrap().len();
    let second_len = editor.document().text_at_index(1).unwrap().len();
    editor
        .apply(Transaction::SetAlignment {
            selection: Selection::new(
                DocPoint::with_affinity(first, 0, Affinity::Before),
                DocPoint::with_affinity(first, first_len, Affinity::After),
            ),
            alignment: TextAlignment::Right,
        })
        .expect("right-align first block");
    editor
        .apply(Transaction::SetBlockKind {
            selection: Selection::new(
                DocPoint::with_affinity(second, 0, Affinity::Before),
                DocPoint::with_affinity(second, second_len, Affinity::After),
            ),
            kind: BlockKind::OrderedItem { depth: 2 },
        })
        .expect("nested list conversion");
    editor
        .apply(Transaction::SetAlignment {
            selection: Selection::new(
                DocPoint::with_affinity(second, 0, Affinity::Before),
                DocPoint::with_affinity(second, second_len, Affinity::After),
            ),
            alignment: TextAlignment::Center,
        })
        .expect("center-align nested list");

    let document = editor.document().clone();
    cx.update(|window, _| {
        editor
            .layout
            .shape_visible_with_window(&document, 0.0, 800.0, 180.0, window);
    });
    let origin = DocPoint::with_affinity(first, first_len, Affinity::After);
    editor.set_selection_for_test(Selection::caret(origin));
    let before_x = editor.layout().caret_x(origin).expect("first caret x");
    editor.move_down();
    let moved = editor.selection().head;
    assert_eq!(moved.node_id, second, "down must enter the next block");
    let after_x = editor.layout().caret_x(moved).expect("nested caret x");
    assert!(
        f32::from(after_x - before_x).abs() <= 12.0,
        "vertical navigation must preserve screen x across inset/alignment: {before_x:?} -> {after_x:?}"
    );
}
