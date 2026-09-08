use super::history::History;
use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicUsize, Ordering};

use super::model::{
    Affinity, BlockContent, BlockKind, DocPoint, Document, DocumentError, Mark, Selection,
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
        // little noise; the limit remains below the ~32 MiB full clone.
        ALLOCATED_BYTES.load(Ordering::Relaxed) < 20_000_000,
        "small edit allocated a full-document-sized candidate: {} bytes",
        ALLOCATED_BYTES.load(Ordering::Relaxed)
    );
}
