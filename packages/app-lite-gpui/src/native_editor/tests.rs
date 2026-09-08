use super::core::EditorCore;
use super::history::History;
use super::layout::{LAYOUT_CACHE_BUDGET_BYTES, LayoutRegistry};
use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicUsize, Ordering};

use gpui::{AppContext, Bounds, EntityInputHandler, point, px};
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
            assert_eq!(editor.marked_text(), Some("新"));
            assert_eq!(
                <EditorCore as EntityInputHandler>::marked_text_range(editor, window, editor_cx,),
                Some(2..3)
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
fn editor_core_keeps_one_stable_focus_owner(cx: &mut gpui::TestAppContext) {
    let editor = EditorCore::for_test("唯一焦点", cx);
    let first = editor.focus_handle() as *const _;
    let second = editor.focus_handle() as *const _;
    assert_eq!(first, second);
}
