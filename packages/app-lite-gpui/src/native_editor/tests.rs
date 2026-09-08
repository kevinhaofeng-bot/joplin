use super::core::EditorCore;
use super::history::History;
use super::layout::{LAYOUT_CACHE_BUDGET_BYTES, LayoutRegistry};
use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use gpui::{AppContext, Bounds, EntityInputHandler, FontStyle, FontWeight, TextStyle, point, px};
use smallvec::SmallVec;

use super::model::{
    Affinity, Block, BlockContent, BlockKind, DocPoint, Document, DocumentError, Mark, NodeId,
    Selection, StyledRun, TextAlignment, grapheme_resolution_counter,
    reset_grapheme_resolution_counter, reset_validation_grapheme_counter,
    validation_grapheme_counter,
};
use super::transaction::{Transaction, TransactionBatch};

struct CountingAllocator;

static ALLOCATED_BYTES: AtomicUsize = AtomicUsize::new(0);

unsafe impl GlobalAlloc for CountingAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        ALLOCATED_BYTES.fetch_add(layout.size(), Ordering::Relaxed);
        unsafe { System.alloc(layout) }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        unsafe { System.dealloc(ptr, layout) };
    }
}

#[global_allocator]
static TEST_ALLOCATOR: CountingAllocator = CountingAllocator;

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
    ALLOCATED_BYTES.store(0, Ordering::Relaxed);
    doc.apply(Transaction::InsertText {
        selection: Selection::caret(DocPoint::new(node, 0)),
        text: "x".into(),
    })
    .unwrap();
    assert!(
        // Invariant validation and concurrently running tests allocate a
        // little noise; keep the limit well below the ~32 MiB full clone.
        ALLOCATED_BYTES.load(Ordering::Relaxed) < 24_000_000,
        "small edit allocated a full-document-sized candidate: {} bytes",
        ALLOCATED_BYTES.load(Ordering::Relaxed)
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
    let target_top_estimate = document.blocks()[..140]
        .iter()
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
    let expected_top = document.blocks()[..140]
        .iter()
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
            .get(final_range.clone())
            .is_some_and(|blocks| blocks.iter().any(|block| block.id == *id))
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
