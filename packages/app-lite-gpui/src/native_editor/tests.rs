use super::history::History;
use super::model::{BlockKind, DocPoint, Document, DocumentError, Mark, Selection};
use super::transaction::Transaction;

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
