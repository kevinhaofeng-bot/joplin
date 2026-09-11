//! Mutation-sensitive Task 4 acceptance tests.
//!
//! These intentionally exercise the actual `EntityInputHandler` bridge rather
//! than calling a save helper with invented strings.  A native input change
//! must become a journal, then a canonical snapshot, and survive a fresh
//! session without flattening the document.

use super::note_session::NoteSession;
use super::save_coordinator::{FlushReason, ManualSaveClock, SaveState};
use crate::native_editor::images::ResourceImport;
use crate::native_editor::model::{Affinity, BlockContent, DocPoint, DocumentError};
use app_lite_core::document::{Block, BlockStyle, Inline, Marks};
use app_lite_core::{
    CanonicalDocument, CreateNote, EditJournalEntry, LibraryError, LibraryRepository, Note,
    SaveNote,
};
use gpui::{AppContext, EntityInputHandler};
use rusqlite::Connection;
use serde_json::json;
use std::io::Cursor;
use std::sync::Arc;
use std::sync::mpsc::TryRecvError;
use std::time::Duration;

fn repository() -> (tempfile::TempDir, Arc<LibraryRepository>) {
    let profile = tempfile::tempdir().expect("temporary profile");
    let repository = Arc::new(
        LibraryRepository::open(profile.path().join("library.sqlite"))
            .expect("open temporary library"),
    );
    (profile, repository)
}

fn rich_document(text: &str) -> CanonicalDocument {
    CanonicalDocument::from_blocks(vec![Block::Paragraph {
        style: BlockStyle::default(),
        inlines: vec![Inline::Text {
            text: text.into(),
            marks: Marks {
                bold: true,
                italic: true,
                link: Some("https://example.test/中文".into()),
                ..Marks::default()
            },
        }],
    }])
}

fn create(repository: &LibraryRepository, title: &str, document: CanonicalDocument) -> Note {
    repository
        .create_note(CreateNote {
            title: title.into(),
            notebook_id: None,
            document,
        })
        .expect("create note")
}

fn structural_png(width: u32, height: u32) -> Vec<u8> {
    let image = image::RgbaImage::from_pixel(width, height, image::Rgba([0x2d, 0x86, 0x5f, 0xff]));
    let mut encoded = Cursor::new(Vec::new());
    image::DynamicImage::ImageRgba8(image)
        .write_to(&mut encoded, image::ImageFormat::Png)
        .expect("encode structural PNG fixture");
    encoded.into_inner()
}

fn session(
    note: Note,
    repository: Arc<LibraryRepository>,
    clock: Arc<ManualSaveClock>,
    cx: &mut gpui::VisualTestContext,
) -> gpui::Entity<NoteSession> {
    cx.new(|session_cx| {
        NoteSession::open(note, repository, clock, session_cx).expect("open native note session")
    })
}

/// Produces the full-replacement shape of a hostile/reordered v2 checkpoint.
/// Production creates deltas through `JournalPayload`; recovery must still
/// validate this equivalent wire object because SQLite can retain corruption
/// from a crashed or older process.
fn replace_all_recovery_payload(
    note: &Note,
    writer_token: &str,
    generation: i64,
    document: &CanonicalDocument,
) -> String {
    let body_html = document.to_canonical_html().as_str().to_owned();
    let resource_ids = document
        .resource_ids()
        .into_iter()
        .map(|resource_id| resource_id.as_str().to_owned())
        .collect::<Vec<_>>();
    serde_json::to_string(&json!({
        "version": 2,
        "note_id": note.id.as_str(),
        "expected_revision": note.revision,
        "writer_token": writer_token,
        "generation": generation,
        "title": {
            "start_byte": 0,
            "remove_bytes": 0,
            "insert": "",
        },
        "body": {
            "start_byte": 0,
            "remove_bytes": note.body_html.len(),
            "insert": body_html,
        },
        "resource_ids": resource_ids,
    }))
    .expect("serialize complete recovery payload")
}

fn append_title_via_entity_input(
    session: &gpui::Entity<NoteSession>,
    suffix: &str,
    cx: &mut gpui::VisualTestContext,
) {
    let title = session.read_with(cx, |session, _| session.title().clone());
    cx.update(|window, app| {
        title.update(app, |title, title_cx| {
            <crate::native_editor::chrome::TitleInput as EntityInputHandler>::replace_text_in_range(
                title, None, suffix, window, title_cx,
            );
        });
    });
}

fn append_body_via_entity_input(
    session: &gpui::Entity<NoteSession>,
    suffix: &str,
    cx: &mut gpui::VisualTestContext,
) {
    let editor = session.read_with(cx, |session, _| session.editor().clone());
    cx.update(|window, app| {
        editor.update(app, |editor, editor_cx| {
            let end = editor.document().flat_utf16_len();
            <crate::native_editor::core::EditorCore as EntityInputHandler>::replace_text_in_range(
                editor,
                Some(end..end),
                suffix,
                window,
                editor_cx,
            );
        });
    });
}

#[gpui::test]
async fn resource_insert_intent_rejects_a_note_identity_mismatch(cx: &mut gpui::TestAppContext) {
    // A picker can return after a rapid note switch. The saved target must
    // carry the originating NoteId rather than allowing its old DocPoint to
    // be reinterpreted in the newly mounted document.
    let cx = cx.add_empty_window();
    let (_profile, repository) = repository();
    let first_note = create(&repository, "来源", rich_document("first"));
    let second_note = create(&repository, "目标", rich_document("second"));
    let clock = Arc::new(ManualSaveClock::default());
    let first = session(first_note, Arc::clone(&repository), Arc::clone(&clock), cx);
    let second = session(second_note.clone(), Arc::clone(&repository), clock, cx);
    let intent = first
        .update(cx, |session, session_cx| {
            session.capture_resource_insert_intent(session_cx)
        })
        .expect("capture source note intent");
    let import = ResourceImport::from_bytes(
        b"%PDF-1.7\nidentity fixture\n%%EOF\n".to_vec(),
        "跨笔记.pdf",
        "application/pdf",
        "pdf",
    )
    .expect("attachment fixture");
    let error = match second.update(cx, |session, session_cx| {
        session.insert_resource(import, intent, session_cx)
    }) {
        Ok(_) => panic!("a saved intent cannot cross into another note"),
        Err(error) => error,
    };
    assert!(error.to_string().contains("不属于当前笔记"));
    let durable = repository
        .load_note(&second_note.id)
        .expect("load second note")
        .expect("second note remains");
    assert!(durable.resource_ids.is_empty());
    assert_eq!(durable.body_html, second_note.body_html);
}

#[gpui::test]
async fn durable_resource_commit_swaps_prevalidated_editor_when_legacy_apply_would_fail(
    cx: &mut gpui::TestAppContext,
) {
    // The old implementation called `editor.apply` after SQLite had already
    // committed. This fault is consumed only by that legacy path: the
    // resource must still leave DB and retained editor at the same revision.
    let cx = cx.add_empty_window();
    let (_profile, repository) = repository();
    let note = create(&repository, "原子资源", rich_document("正文"));
    let clock = Arc::new(ManualSaveClock::default());
    let session = session(note.clone(), Arc::clone(&repository), clock, cx);
    let intent = session
        .update(cx, |session, session_cx| {
            session.capture_resource_insert_intent(session_cx)
        })
        .expect("capture durable attachment intent");
    session.update(cx, |session, session_cx| {
        session.editor().update(session_cx, |editor, _| {
            editor.fail_next_apply_for_test(DocumentError::InvalidOperation(
                "legacy post-commit apply must not run".into(),
            ));
        });
    });
    let import = ResourceImport::from_bytes(
        b"%PDF-1.7\natomic fixture\n%%EOF\n".to_vec(),
        "原子证据.pdf",
        "application/pdf",
        "pdf",
    )
    .expect("valid attachment import");
    let inserted = session
        .update(cx, |session, session_cx| {
            session.insert_resource(import, intent, session_cx)
        })
        .expect("prevalidated durable swap must not replay legacy apply");
    let saved = repository
        .load_note(&note.id)
        .expect("load durable note")
        .expect("note exists");
    assert_eq!(saved.revision, inserted.note.revision);
    assert_eq!(saved.resource_ids, vec![inserted.resource_id.clone()]);
    let (body_resource, attachment_size, history_depth) = session.read_with(cx, |session, app| {
        let editor = session.editor().read(app);
        let resource = editor
            .document()
            .blocks()
            .iter()
            .find_map(|block| match &block.content {
                BlockContent::Attachment { resource_id, .. } => Some(resource_id.clone()),
                _ => None,
            });
        let size = resource.as_deref().and_then(|id| {
            editor
                .attachment_metadata(id)
                .map(|metadata| metadata.size())
        });
        (resource, size, editor.undo_depth())
    });
    assert_eq!(
        body_resource.as_deref(),
        Some(inserted.resource_id.as_str())
    );
    assert_eq!(attachment_size, Some(30));
    assert!(history_depth > 0, "swap preserves the inserted undo entry");
}

fn poll_and_drain(session: &gpui::Entity<NoteSession>, cx: &mut gpui::VisualTestContext) {
    session.update(cx, |session, session_cx| {
        session
            .poll(session_cx)
            .expect("dispatch due background work")
    });
    // Test timing is deterministic, but completion always uses the same
    // retained/background worker handoff as production.
    cx.run_until_parked();
}

fn flush_until_clean(
    session: &gpui::Entity<NoteSession>,
    reason: FlushReason,
    cx: &mut gpui::VisualTestContext,
) {
    let first = session.update(cx, |session, session_cx| session.flush(reason, session_cx));
    if first.is_err() {
        cx.run_until_parked();
        session.update(cx, |session, session_cx| {
            session
                .flush(reason, session_cx)
                .expect("completion-confirmed lifecycle flush")
        });
    }
}

#[gpui::test]
async fn deleting_a_persisted_image_then_undo_redo_journals_and_compacts_the_current_relation_set(
    cx: &mut gpui::TestAppContext,
) {
    // The durable relation vector belongs to the immutable document snapshot,
    // not to the session's opening note.  Removing a saved image through the
    // actual editor Backspace path, then undoing and redoing it, must leave a
    // crash journal that can recover the deletion and a later snapshot that
    // removes the obsolete note_resources row and card thumbnail.
    let cx = cx.add_empty_window();
    let (_profile, repository) = repository();
    let image = repository
        .import_resource(
            &crate::native_editor::images::ClipboardPayload::fixture_with_png_and_text("")
                .images
                .into_iter()
                .next()
                .expect("PNG fixture")
                .bytes,
            "已保存图片.png",
            "image/png",
            "png",
        )
        .expect("persist starting image");
    let note = create(
        &repository,
        "删除持久图片",
        CanonicalDocument::from_blocks(vec![
            Block::Paragraph {
                style: BlockStyle::default(),
                inlines: vec![Inline::Text {
                    text: "图片前".into(),
                    marks: Marks::default(),
                }],
            },
            Block::Image {
                resource_id: image.clone(),
                alt: "要删除的图片".into(),
                presentation: Default::default(),
            },
            Block::Paragraph {
                style: BlockStyle::default(),
                inlines: vec![Inline::Text {
                    text: "图片后".into(),
                    marks: Marks::default(),
                }],
            },
        ]),
    );
    let clock = Arc::new(ManualSaveClock::default());
    let active = session(
        note.clone(),
        Arc::clone(&repository),
        Arc::clone(&clock),
        cx,
    );
    let image_node = active.read_with(cx, |session, app| {
        session
            .editor()
            .read(app)
            .document()
            .blocks()
            .iter()
            .find_map(|block| {
                matches!(
                    &block.content,
                    BlockContent::Image { resource_id, .. } if resource_id == image.as_str()
                )
                .then_some(block.id)
            })
            .expect("mounted saved image")
    });
    active.update(cx, |session, session_cx| {
        session.editor().update(session_cx, |editor, editor_cx| {
            editor.select_for_workload(DocPoint::with_affinity(image_node, 0, Affinity::After));
            editor
                .backspace()
                .expect("Backspace removes selected image");
            editor.undo().expect("undo restores image");
            editor.redo().expect("redo removes image again");
            editor_cx.notify();
        });
    });
    assert!(active.read_with(cx, |session, app| {
        !session
            .editor()
            .read(app)
            .document()
            .blocks()
            .iter()
            .any(|block| {
                matches!(
                    &block.content,
                    BlockContent::Image { resource_id, .. } if resource_id == image.as_str()
                )
            })
    }));

    // The real entity observer records this semantic change before the
    // 100ms deadline starts. Drive that same observer edge explicitly before
    // advancing the deterministic clock.
    poll_and_drain(&active, cx);
    clock.advance(Duration::from_millis(100));
    poll_and_drain(&active, cx);
    assert!(
        repository
            .latest_edit_journal(&note.id)
            .expect("read deletion checkpoint")
            .is_some(),
        "the 100ms crash journal must carry the deleted document's resource order"
    );
    drop(active);

    let recovered_note = repository
        .load_note(&note.id)
        .expect("load still-uncompacted base")
        .expect("note exists");
    let recovered = session(
        recovered_note,
        Arc::clone(&repository),
        Arc::clone(&clock),
        cx,
    );
    assert!(
        recovered.read_with(cx, |session, app| {
            !session
                .editor()
                .read(app)
                .document()
                .blocks()
                .iter()
                .any(|block| {
                    matches!(
                        &block.content,
                        BlockContent::Image { resource_id, .. } if resource_id == image.as_str()
                    )
                })
        }),
        "crash recovery must apply the deletion journal rather than reject it as a new relation set"
    );

    flush_until_clean(&recovered, FlushReason::ManualSync, cx);
    drop(recovered);
    let saved = repository
        .load_note(&note.id)
        .expect("load compacted note")
        .expect("note remains");
    assert!(saved.resource_ids.is_empty());
    assert!(!saved.body_html.contains(image.as_str()));
    assert!(
        repository
            .latest_edit_journal(&note.id)
            .expect("journal compacts after snapshot")
            .is_none()
    );
    assert!(
        repository
            .list_notes(Default::default())
            .expect("projection after deletion")
            .iter()
            .find(|projection| projection.id == note.id)
            .expect("note projection")
            .selected_thumbnail_id
            .is_none(),
        "the last deleted image must not revive its old card thumbnail"
    );

    let restarted = session(saved, Arc::clone(&repository), clock, cx);
    assert!(
        restarted.read_with(cx, |session, app| {
            !session
                .editor()
                .read(app)
                .document()
                .blocks()
                .iter()
                .any(|block| {
                    matches!(
                        &block.content,
                        BlockContent::Image { resource_id, .. } if resource_id == image.as_str()
                    )
                })
        }),
        "a second restart must read the compacted relation set, not resurrect the image"
    );
}

#[gpui::test]
async fn undoing_a_durable_resource_deletion_keeps_the_original_id_authorized_for_the_next_snapshot(
    cx: &mut gpui::TestAppContext,
) {
    // A normal snapshot must replace note_resources from the current document
    // without shrinking the session's resource authorization set. Otherwise
    // Cmd-Z after a successful deletion snapshot resurrects a perfectly real
    // node in memory but the next save rejects it as a fabricated resource.
    let cx = cx.add_empty_window();
    let (_profile, repository) = repository();
    let image = repository
        .import_resource(
            &crate::native_editor::images::ClipboardPayload::fixture_with_png_and_text("")
                .images
                .into_iter()
                .next()
                .expect("PNG fixture")
                .bytes,
            "undoable.png",
            "image/png",
            "png",
        )
        .expect("persist starting image");
    let note = create(
        &repository,
        "持久删除后撤销",
        CanonicalDocument::from_blocks(vec![
            Block::Paragraph {
                style: BlockStyle::default(),
                inlines: vec![Inline::Text {
                    text: "左侧文本".into(),
                    marks: Marks::default(),
                }],
            },
            Block::Image {
                resource_id: image.clone(),
                alt: "可撤销图片".into(),
                presentation: Default::default(),
            },
            Block::Paragraph {
                style: BlockStyle::default(),
                inlines: vec![Inline::Text {
                    text: "右侧文本".into(),
                    marks: Marks::default(),
                }],
            },
        ]),
    );
    let clock = Arc::new(ManualSaveClock::default());
    let active = session(
        note.clone(),
        Arc::clone(&repository),
        Arc::clone(&clock),
        cx,
    );
    let image_node = active.read_with(cx, |session, app| {
        session
            .editor()
            .read(app)
            .document()
            .blocks()
            .iter()
            .find(|block| matches!(&block.content, BlockContent::Image { resource_id, .. } if resource_id == image.as_str()))
            .expect("mounted image")
            .id
    });
    active.update(cx, |session, session_cx| {
        session.editor().update(session_cx, |editor, editor_cx| {
            editor.select_for_workload(DocPoint::with_affinity(image_node, 0, Affinity::After));
            editor.backspace().expect("delete image");
            editor_cx.notify();
        });
    });
    poll_and_drain(&active, cx);
    flush_until_clean(&active, FlushReason::ManualSync, cx);
    assert!(
        repository
            .load_note(&note.id)
            .expect("load deleted snapshot")
            .expect("note")
            .resource_ids
            .is_empty()
    );

    active.update(cx, |session, session_cx| {
        session.editor().update(session_cx, |editor, editor_cx| {
            editor.undo().expect("undo durable deletion");
            editor_cx.notify();
        });
    });
    poll_and_drain(&active, cx);
    flush_until_clean(&active, FlushReason::ManualSync, cx);
    assert_eq!(
        repository
            .load_note(&note.id)
            .expect("load restored snapshot")
            .expect("note")
            .resource_ids,
        vec![image.clone()],
        "the same persisted resource ID must become durable again after Cmd-Z"
    );

    active.update(cx, |session, session_cx| {
        session.editor().update(session_cx, |editor, editor_cx| {
            editor.redo().expect("redo durable deletion");
            editor_cx.notify();
        });
    });
    poll_and_drain(&active, cx);
    flush_until_clean(&active, FlushReason::ManualSync, cx);
    let saved = repository
        .load_note(&note.id)
        .expect("load re-deleted snapshot")
        .expect("note");
    assert!(saved.resource_ids.is_empty());
    drop(active);
    let restarted = session(saved, Arc::clone(&repository), clock, cx);
    assert!(restarted.read_with(cx, |session, app| {
        !session.editor().read(app).document().blocks().iter().any(|block| {
            matches!(&block.content, BlockContent::Image { resource_id, .. } if resource_id == image.as_str())
        })
    }));
}

#[gpui::test]
async fn crash_journal_recovers_an_ordered_a_b_a_resource_deletion_without_losing_writer_ownership(
    cx: &mut gpui::TestAppContext,
) {
    // A deletion journal is allowed to shrink A,B,A to B,A, but not to
    // manufacture or reorder resources. This follows the exact recovery
    // ownership handoff, then compacts and restarts again from the database.
    let cx = cx.add_empty_window();
    let (_profile, repository) = repository();
    let bytes = crate::native_editor::images::ClipboardPayload::fixture_with_png_and_text("")
        .images
        .into_iter()
        .next()
        .expect("PNG fixture")
        .bytes;
    let a = repository
        .import_resource(&bytes, "a.png", "image/png", "png")
        .expect("persist A");
    let b = repository
        .import_resource(&bytes, "b.png", "image/png", "png")
        .expect("persist B");
    let note = create(
        &repository,
        "A B A 删除",
        CanonicalDocument::from_blocks(vec![
            Block::Paragraph {
                style: BlockStyle::default(),
                inlines: vec![Inline::Text {
                    text: "开始".into(),
                    marks: Marks::default(),
                }],
            },
            Block::Image {
                resource_id: a.clone(),
                alt: "first A".into(),
                presentation: Default::default(),
            },
            Block::Image {
                resource_id: b.clone(),
                alt: "B".into(),
                presentation: Default::default(),
            },
            Block::Image {
                resource_id: a.clone(),
                alt: "second A".into(),
                presentation: Default::default(),
            },
            Block::Paragraph {
                style: BlockStyle::default(),
                inlines: vec![Inline::Text {
                    text: "结束".into(),
                    marks: Marks::default(),
                }],
            },
        ]),
    );
    let clock = Arc::new(ManualSaveClock::default());
    let active = session(
        note.clone(),
        Arc::clone(&repository),
        Arc::clone(&clock),
        cx,
    );
    let first_a = active.read_with(cx, |session, app| {
        session
            .editor()
            .read(app)
            .document()
            .blocks()
            .iter()
            .find(|block| matches!(&block.content, BlockContent::Image { resource_id, alt, .. } if resource_id == a.as_str() && alt == "first A"))
            .expect("first A")
            .id
    });
    active.update(cx, |session, session_cx| {
        session.editor().update(session_cx, |editor, editor_cx| {
            editor.select_for_workload(DocPoint::with_affinity(first_a, 0, Affinity::After));
            editor.backspace().expect("delete only first A");
            editor_cx.notify();
        });
    });
    poll_and_drain(&active, cx);
    clock.advance(Duration::from_millis(100));
    poll_and_drain(&active, cx);
    let checkpoint = repository
        .latest_edit_journal(&note.id)
        .expect("read deletion checkpoint")
        .expect("checkpoint exists");
    drop(active);

    let recovered_note = repository
        .load_note(&note.id)
        .expect("load durable base")
        .expect("note exists");
    let recovered = session(
        recovered_note,
        Arc::clone(&repository),
        Arc::clone(&clock),
        cx,
    );
    let live_order = recovered.read_with(cx, |session, app| {
        session
            .editor()
            .read(app)
            .document()
            .blocks()
            .iter()
            .filter_map(|block| match &block.content {
                BlockContent::Image { resource_id, .. } => Some(resource_id.clone()),
                _ => None,
            })
            .collect::<Vec<_>>()
    });
    assert_eq!(
        live_order,
        vec![b.as_str().to_owned(), a.as_str().to_owned()]
    );
    let claimed = repository
        .latest_edit_journal(&note.id)
        .expect("read claimed checkpoint")
        .expect("claimed checkpoint remains until compaction");
    assert_eq!(claimed.sequence, checkpoint.sequence);
    assert_ne!(claimed.writer_token, checkpoint.writer_token);

    flush_until_clean(&recovered, FlushReason::ManualSync, cx);
    drop(recovered);
    let compacted = repository
        .load_note(&note.id)
        .expect("load compacted note")
        .expect("note exists");
    assert_eq!(compacted.resource_ids, vec![b.clone(), a.clone()]);
    assert!(
        repository
            .latest_edit_journal(&note.id)
            .expect("journal compacts")
            .is_none()
    );
    let restarted = session(compacted, Arc::clone(&repository), clock, cx);
    assert_eq!(
        restarted.read_with(cx, |session, app| {
            session
                .editor()
                .read(app)
                .document()
                .blocks()
                .iter()
                .filter_map(|block| match &block.content {
                    BlockContent::Image { resource_id, .. } => Some(resource_id.clone()),
                    _ => None,
                })
                .collect::<Vec<_>>()
        }),
        vec![b.as_str().to_owned(), a.as_str().to_owned()]
    );
}

#[gpui::test]
async fn crash_journal_recovers_historical_a_b_a_after_durable_deletion_then_undo(
    cx: &mut gpui::TestAppContext,
) {
    // A saved deletion legitimately changes the current relation from A,B,A
    // to B,A.  Cmd-Z can then restore the first A from the same note's
    // durable revision history.  A crash before the next snapshot must not
    // treat that recovered occurrence as a forged resource merely because it
    // is absent from the current B,A base relation.
    let cx = cx.add_empty_window();
    let (_profile, repository) = repository();
    let bytes = crate::native_editor::images::ClipboardPayload::fixture_with_png_and_text("")
        .images
        .into_iter()
        .next()
        .expect("PNG fixture")
        .bytes;
    let a = repository
        .import_resource(&bytes, "undo-a.png", "image/png", "png")
        .expect("persist A");
    let b = repository
        .import_resource(&bytes, "undo-b.png", "image/png", "png")
        .expect("persist B");
    let note = create(
        &repository,
        "A B A durable undo",
        CanonicalDocument::from_blocks(vec![
            Block::Image {
                resource_id: a.clone(),
                alt: "first A".into(),
                presentation: Default::default(),
            },
            Block::Image {
                resource_id: b.clone(),
                alt: "B".into(),
                presentation: Default::default(),
            },
            Block::Image {
                resource_id: a.clone(),
                alt: "second A".into(),
                presentation: Default::default(),
            },
        ]),
    );
    let clock = Arc::new(ManualSaveClock::default());
    let active = session(
        note.clone(),
        Arc::clone(&repository),
        Arc::clone(&clock),
        cx,
    );
    let first_a = active.read_with(cx, |session, app| {
        session
            .editor()
            .read(app)
            .document()
            .blocks()
            .iter()
            .find(|block| {
                matches!(&block.content, BlockContent::Image { resource_id, alt, .. } if resource_id == a.as_str() && alt == "first A")
            })
            .expect("first A")
            .id
    });
    active.update(cx, |session, session_cx| {
        session.editor().update(session_cx, |editor, editor_cx| {
            editor.select_for_workload(DocPoint::with_affinity(first_a, 0, Affinity::After));
            editor.backspace().expect("delete first A");
            editor_cx.notify();
        });
    });
    poll_and_drain(&active, cx);
    flush_until_clean(&active, FlushReason::ManualSync, cx);
    assert_eq!(
        repository
            .load_note(&note.id)
            .expect("load B,A durable snapshot")
            .expect("note exists")
            .resource_ids,
        vec![b.clone(), a.clone()]
    );

    active.update(cx, |session, session_cx| {
        session.editor().update(session_cx, |editor, editor_cx| {
            editor.undo().expect("undo restores first A");
            editor_cx.notify();
        });
    });
    poll_and_drain(&active, cx);
    clock.advance(Duration::from_millis(100));
    poll_and_drain(&active, cx);
    assert!(
        repository
            .latest_edit_journal(&note.id)
            .expect("read undo checkpoint")
            .is_some(),
        "the restored A,B,A must reach the production crash journal"
    );
    drop(active);

    let recovered = session(
        repository
            .load_note(&note.id)
            .expect("load B,A crash base")
            .expect("note exists"),
        Arc::clone(&repository),
        Arc::clone(&clock),
        cx,
    );
    let recovered_order = recovered.read_with(cx, |session, app| {
        session
            .editor()
            .read(app)
            .document()
            .blocks()
            .iter()
            .filter_map(|block| match &block.content {
                BlockContent::Image { resource_id, .. } => Some(resource_id.clone()),
                _ => None,
            })
            .collect::<Vec<_>>()
    });
    assert_eq!(
        recovered_order,
        vec![
            a.as_str().to_owned(),
            b.as_str().to_owned(),
            a.as_str().to_owned()
        ],
        "recovery must use same-note durable provenance rather than only B,A"
    );
    flush_until_clean(&recovered, FlushReason::ManualSync, cx);
    drop(recovered);

    let compacted = repository
        .load_note(&note.id)
        .expect("load compacted A,B,A")
        .expect("note exists");
    assert_eq!(
        compacted.resource_ids,
        vec![a.clone(), b.clone(), a.clone()]
    );
    assert!(
        repository
            .latest_edit_journal(&note.id)
            .expect("journal compacted after recovery snapshot")
            .is_none()
    );
    let second_restart = session(compacted, Arc::clone(&repository), clock, cx);
    let second_order = second_restart.read_with(cx, |session, app| {
        session
            .editor()
            .read(app)
            .document()
            .blocks()
            .iter()
            .filter_map(|block| match &block.content {
                BlockContent::Image { resource_id, .. } => Some(resource_id.clone()),
                _ => None,
            })
            .collect::<Vec<_>>()
    });
    assert_eq!(
        second_order,
        vec![
            a.as_str().to_owned(),
            b.as_str().to_owned(),
            a.as_str().to_owned()
        ],
        "the compacted durable relation must survive a second restart"
    );
}

#[gpui::test]
async fn crash_journal_rejects_a_resource_outside_same_note_durable_provenance(
    cx: &mut gpui::TestAppContext,
) {
    // The journal wire list is hostile input. A resource that happens to
    // exist globally but has never appeared in this note's durable revision
    // history must not become associated merely by forging a recovery delta.
    let _cx = cx.add_empty_window();
    let (_profile, repository) = repository();
    let bytes = crate::native_editor::images::ClipboardPayload::fixture_with_png_and_text("")
        .images
        .into_iter()
        .next()
        .expect("PNG fixture")
        .bytes;
    let associated = repository
        .import_resource(&bytes, "associated.png", "image/png", "png")
        .expect("persist associated resource");
    let foreign = repository
        .import_resource(&bytes, "foreign.png", "image/png", "png")
        .expect("persist globally valid but unassociated resource");
    let note = create(
        &repository,
        "durable provenance",
        CanonicalDocument::from_blocks(vec![Block::Image {
            resource_id: associated.clone(),
            alt: "associated".into(),
            presentation: Default::default(),
        }]),
    );
    let forged_document = CanonicalDocument::from_blocks(vec![Block::Image {
        resource_id: foreign.clone(),
        alt: "forged foreign".into(),
        presentation: Default::default(),
    }]);
    let writer_token = "forged-same-base-writer";
    let forged_delta = replace_all_recovery_payload(&note, writer_token, 1, &forged_document);
    repository
        .append_edit_journal(EditJournalEntry {
            note_id: note.id.clone(),
            expected_revision: note.revision,
            writer_token: writer_token.into(),
            sequence: 0,
            generation: 1,
            delta_utf8: forged_delta,
        })
        .expect("write hostile checkpoint exactly as crash recovery reads it");

    let error = match NoteSession::prepare(
        repository
            .load_note(&note.id)
            .expect("load durable base")
            .expect("note exists"),
        repository.as_ref(),
    ) {
        Ok(_) => panic!("foreign resource cannot be authorized by journal self-report"),
        Err(error) => error,
    };
    assert!(
        error.to_string().contains("持久历史"),
        "recovery must name the same-note provenance boundary: {error}"
    );
    assert_eq!(
        repository
            .load_note(&note.id)
            .expect("read unmodified durable note")
            .expect("note exists")
            .resource_ids,
        vec![associated],
        "rejecting hostile recovery must not expose the foreign relation"
    );
}

#[gpui::test]
async fn crash_journal_recovers_a_same_note_resource_reorder_within_durable_counts(
    cx: &mut gpui::TestAppContext,
) {
    // Resource order is document state, not authorization state. A durable
    // B,A base may legally recover an A,B reorder as long as every occurrence
    // came from the same note's committed history; the old current-order
    // subsequence check falsely rejected this.
    let cx = cx.add_empty_window();
    let (_profile, repository) = repository();
    let bytes = crate::native_editor::images::ClipboardPayload::fixture_with_png_and_text("")
        .images
        .into_iter()
        .next()
        .expect("PNG fixture")
        .bytes;
    let a = repository
        .import_resource(&bytes, "reorder-a.png", "image/png", "png")
        .expect("persist A");
    let b = repository
        .import_resource(&bytes, "reorder-b.png", "image/png", "png")
        .expect("persist B");
    let note = create(
        &repository,
        "B A reorder base",
        CanonicalDocument::from_blocks(vec![
            Block::Image {
                resource_id: b.clone(),
                alt: "B".into(),
                presentation: Default::default(),
            },
            Block::Image {
                resource_id: a.clone(),
                alt: "A".into(),
                presentation: Default::default(),
            },
        ]),
    );
    let reordered = CanonicalDocument::from_blocks(vec![
        Block::Image {
            resource_id: a.clone(),
            alt: "A".into(),
            presentation: Default::default(),
        },
        Block::Image {
            resource_id: b.clone(),
            alt: "B".into(),
            presentation: Default::default(),
        },
    ]);
    let writer_token = "same-note-reorder-writer";
    repository
        .append_edit_journal(EditJournalEntry {
            note_id: note.id.clone(),
            expected_revision: note.revision,
            writer_token: writer_token.into(),
            sequence: 0,
            generation: 1,
            delta_utf8: replace_all_recovery_payload(&note, writer_token, 1, &reordered),
        })
        .expect("write same-note reordered crash checkpoint");

    let clock = Arc::new(ManualSaveClock::default());
    let recovered = session(
        repository
            .load_note(&note.id)
            .expect("load B,A durable base")
            .expect("note exists"),
        Arc::clone(&repository),
        clock,
        cx,
    );
    let order = recovered.read_with(cx, |session, app| {
        session
            .editor()
            .read(app)
            .document()
            .blocks()
            .iter()
            .filter_map(|block| match &block.content {
                BlockContent::Image { resource_id, .. } => Some(resource_id.clone()),
                _ => None,
            })
            .collect::<Vec<_>>()
    });
    assert_eq!(order, vec![a.as_str().to_owned(), b.as_str().to_owned()]);
}

#[gpui::test]
async fn crash_journal_rejects_same_note_resource_occurrence_amplification(
    cx: &mut gpui::TestAppContext,
) {
    // A historical A occurrence authorizes at most one A. A forged A,A body
    // is not made safe merely because the resource is real and same-note.
    let _cx = cx.add_empty_window();
    let (_profile, repository) = repository();
    let bytes = crate::native_editor::images::ClipboardPayload::fixture_with_png_and_text("")
        .images
        .into_iter()
        .next()
        .expect("PNG fixture")
        .bytes;
    let a = repository
        .import_resource(&bytes, "one-a.png", "image/png", "png")
        .expect("persist A");
    let note = create(
        &repository,
        "one A provenance",
        CanonicalDocument::from_blocks(vec![Block::Image {
            resource_id: a.clone(),
            alt: "only A".into(),
            presentation: Default::default(),
        }]),
    );
    let amplified = CanonicalDocument::from_blocks(vec![
        Block::Image {
            resource_id: a.clone(),
            alt: "first A".into(),
            presentation: Default::default(),
        },
        Block::Image {
            resource_id: a.clone(),
            alt: "forged second A".into(),
            presentation: Default::default(),
        },
    ]);
    let writer_token = "same-note-amplification-writer";
    repository
        .append_edit_journal(EditJournalEntry {
            note_id: note.id.clone(),
            expected_revision: note.revision,
            writer_token: writer_token.into(),
            sequence: 0,
            generation: 1,
            delta_utf8: replace_all_recovery_payload(&note, writer_token, 1, &amplified),
        })
        .expect("write hostile amplified checkpoint");
    let error = match NoteSession::prepare(
        repository
            .load_note(&note.id)
            .expect("load durable note")
            .expect("note exists"),
        repository.as_ref(),
    ) {
        Ok(_) => panic!("one durable occurrence cannot authorize two recovered occurrences"),
        Err(error) => error,
    };
    assert!(error.to_string().contains("放大了出现次数"));
}

#[gpui::test]
async fn chinese_title_and_body_entity_input_round_trip_after_restart(
    cx: &mut gpui::TestAppContext,
) {
    let cx = cx.add_empty_window();
    let (_profile, repository) = repository();
    let note = create(&repository, "原题", rich_document("保留样式"));
    let clock = Arc::new(ManualSaveClock::default());
    let active = session(
        note.clone(),
        Arc::clone(&repository),
        Arc::clone(&clock),
        cx,
    );

    append_title_via_entity_input(&active, "中文标题", cx);
    append_body_via_entity_input(&active, "和正文", cx);
    flush_until_clean(&active, FlushReason::ManualSync, cx);
    drop(active);

    let reloaded = repository
        .load_note(&note.id)
        .expect("load saved note")
        .expect("saved note exists");
    assert_eq!(reloaded.title, "原题中文标题");
    let restarted = session(reloaded, Arc::clone(&repository), clock, cx);
    let (title, document) = restarted.read_with(cx, |session, session_cx| {
        (
            session.title().read(session_cx).text().to_owned(),
            session
                .editor()
                .read(session_cx)
                .document()
                .semantic_snapshot(),
        )
    });
    assert_eq!(title, "原题中文标题");
    assert!(format!("{document:?}").contains("Bold"));
    assert!(format!("{document:?}").contains("Italic"));
    assert!(format!("{document:?}").contains("https://example.test/中文"));
    assert!(format!("{document:?}").contains("和正文"));
}

#[gpui::test]
async fn opening_missing_persisted_image_keeps_other_body_editable_until_surface_requests_it(
    cx: &mut gpui::TestAppContext,
) {
    // Session preparation owns only canonical geometry and resource IDs. A
    // missing blob must not reject surrounding text or trigger a descriptor
    // read before the mounted renderer marks this atom resident; the mounted
    // companion test covers that later per-image failed placeholder.
    let cx = cx.add_empty_window();
    let (profile, repository) = repository();
    let resource = repository
        .import_resource(
            &crate::native_editor::images::ClipboardPayload::fixture_with_png_and_text("")
                .images
                .into_iter()
                .next()
                .expect("PNG fixture")
                .bytes,
            "missing.png",
            "image/png",
            "png",
        )
        .expect("persist image metadata and bytes");
    let note = create(
        &repository,
        "局部图片故障",
        CanonicalDocument::from_blocks(vec![
            Block::Paragraph {
                style: BlockStyle::default(),
                inlines: vec![Inline::Text {
                    text: "图片前文字".into(),
                    marks: Marks::default(),
                }],
            },
            Block::Image {
                resource_id: resource.clone(),
                alt: "丢失的图片".into(),
                presentation: Default::default(),
            },
            Block::Paragraph {
                style: BlockStyle::default(),
                inlines: vec![Inline::Text {
                    text: "图片后文字".into(),
                    marks: Marks::default(),
                }],
            },
        ]),
    );
    let metadata = repository
        .resource_metadata(&resource)
        .expect("load resource metadata")
        .expect("resource metadata exists");
    std::fs::remove_file(
        profile
            .path()
            .join("resources/blobs")
            .join(metadata.sha256.as_str()),
    )
    .expect("simulate a missing local blob after a valid sync record");

    let active = session(
        note,
        Arc::clone(&repository),
        Arc::new(ManualSaveClock::default()),
        cx,
    );
    let (image_state, warning, surrounding_text) = active.read_with(cx, |session, app| {
        let editor = session.editor().read(app);
        (
            editor.image_state(resource.as_str()),
            session.resource_load_warning().map(str::to_owned),
            editor
                .document()
                .blocks()
                .iter()
                .filter_map(|block| block.content.as_text())
                .collect::<String>(),
        )
    });
    assert_eq!(
        image_state, None,
        "bare session construction must not eagerly open or decode a missing blob"
    );
    assert!(
        warning.is_none(),
        "no renderer request means no false load notice"
    );
    assert_eq!(surrounding_text, "图片前文字图片后文字");

    append_body_via_entity_input(&active, "仍可编辑", cx);
    assert!(
        active.read_with(cx, |session, app| session
            .editor()
            .read(app)
            .document()
            .blocks()
            .iter()
            .filter_map(|block| block.content.as_text())
            .collect::<String>()
            .contains("仍可编辑")),
        "a deferred image load must not turn the rest of the note into an unsupported document"
    );
}

#[gpui::test]
async fn opening_many_images_keeps_all_original_blobs_unverified_until_surface_residency(
    cx: &mut gpui::TestAppContext,
) {
    // `hydrate_persisted_images` used to call `read_resource_bytes` for every
    // block, retaining all source `Vec`s in PreparedNoteSession. Bare session
    // construction now proves an even stronger contract: it does not cross
    // either the allocating *or* verified descriptor boundary. The mounted
    // visible-only test drives the eventual real surface request separately.
    let cx = cx.add_empty_window();
    let (_profile, repository) = repository();
    let bytes = crate::native_editor::images::ClipboardPayload::fixture_with_png_and_text("")
        .images
        .into_iter()
        .next()
        .expect("PNG fixture")
        .bytes;
    let first = repository
        .import_resource(&bytes, "first.png", "image/png", "png")
        .expect("first durable image");
    let second = repository
        .import_resource(&bytes, "second.png", "image/png", "png")
        .expect("second durable image");
    let note = create(
        &repository,
        "流式图片",
        CanonicalDocument::from_blocks(vec![
            Block::Image {
                resource_id: first.clone(),
                alt: "一".into(),
                presentation: Default::default(),
            },
            Block::Image {
                resource_id: second.clone(),
                alt: "二".into(),
                presentation: Default::default(),
            },
        ]),
    );
    let reads = repository.observe_resource_reads();
    let opens = repository.observe_verified_resource_opens();

    let active = session(
        note,
        Arc::clone(&repository),
        Arc::new(ManualSaveClock::default()),
        cx,
    );

    let (first_cached, second_cached, first_bytes, second_bytes) =
        active.read_with(cx, |session, app| {
            let editor = session.editor().read(app);
            (
                editor
                    .image_source_path(first.as_str())
                    .is_some_and(|path| path.is_file()),
                editor
                    .image_source_path(second.as_str())
                    .is_some_and(|path| path.is_file()),
                editor.image_bytes(first.as_str()).is_none(),
                editor.image_bytes(second.as_str()).is_none(),
            )
        });
    assert!(!first_cached && !second_cached);
    assert!(first_bytes && second_bytes);
    assert!(matches!(reads.try_recv(), Err(TryRecvError::Empty)));
    assert!(matches!(opens.try_recv(), Err(TryRecvError::Empty)));
}

#[gpui::test]
async fn dropped_session_hydration_worker_never_recreates_its_image_cache_root(
    cx: &mut gpui::TestAppContext,
) {
    // The retained worker may already hold an open verified descriptor when
    // its editor/session is destroyed. It must write only to a task-owned
    // sibling staging directory; otherwise a late materialization recreates
    // the dropped ImageStore root after ImageStore::drop removed it.
    let cx = cx.add_empty_window();
    let (_profile, repository) = repository();
    let bytes = crate::native_editor::images::ClipboardPayload::fixture_with_png_and_text("")
        .images
        .into_iter()
        .next()
        .expect("PNG fixture")
        .bytes;
    let image = repository
        .import_resource(&bytes, "late.png", "image/png", "png")
        .expect("persist image");
    let note = create(
        &repository,
        "late hydration",
        CanonicalDocument::from_blocks(vec![Block::Image {
            resource_id: image.clone(),
            alt: "late".into(),
            presentation: Default::default(),
        }]),
    );
    let active = session(
        note,
        Arc::clone(&repository),
        Arc::new(ManualSaveClock::default()),
        cx,
    );
    let root = active.read_with(cx, |session, app| {
        session.editor().read(app).image_materialization_root()
    });
    assert!(
        !root.exists(),
        "a metadata-only open must not pre-create the editor image cache root"
    );
    let release = active.update(cx, |session, _| {
        session.stall_next_image_hydration_for_test()
    });
    active.update(cx, |session, session_cx| {
        session.editor().update(session_cx, |editor, editor_cx| {
            assert!(editor.request_image_hydration([image.as_str().to_owned()]));
            editor_cx.notify();
        });
    });
    cx.run_until_parked();

    drop(active);
    release
        .send(())
        .expect("release the retained worker after drop");
    cx.run_until_parked();
    assert!(
        !root.exists(),
        "a late worker completion must never resurrect a dropped session cache root"
    );
}

#[gpui::test]
async fn hydration_scroll_coalesces_queued_work_to_the_latest_resident_image(
    cx: &mut gpui::TestAppContext,
) {
    // A gated first materialization simulates a slow visible image while the
    // user rapidly scrolls across other image atoms. The worker may finish
    // the already-active first image, but must not drain every stale viewport
    // after it: only the final resident image may cross `open_verified`.
    let cx = cx.add_empty_window();
    let (_profile, repository) = repository();
    let first = repository
        .import_resource(&structural_png(1200, 675), "first.png", "image/png", "png")
        .expect("first durable image");
    let stale = repository
        .import_resource(&structural_png(675, 1200), "stale.png", "image/png", "png")
        .expect("stale durable image");
    let final_image = repository
        .import_resource(&structural_png(800, 800), "final.png", "image/png", "png")
        .expect("final durable image");
    let hashes = [first.clone(), stale.clone(), final_image.clone()].map(|resource| {
        repository
            .resource_metadata(&resource)
            .expect("read durable metadata")
            .expect("metadata exists")
            .sha256
    });
    let note = create(
        &repository,
        "bounded hydration",
        CanonicalDocument::from_blocks(vec![
            Block::Image {
                resource_id: first.clone(),
                alt: "first".into(),
                presentation: Default::default(),
            },
            Block::Image {
                resource_id: stale.clone(),
                alt: "stale".into(),
                presentation: Default::default(),
            },
            Block::Image {
                resource_id: final_image.clone(),
                alt: "final".into(),
                presentation: Default::default(),
            },
        ]),
    );
    let opens = repository.observe_verified_resource_opens();
    let active = session(
        note,
        Arc::clone(&repository),
        Arc::new(ManualSaveClock::default()),
        cx,
    );
    let release = active.update(cx, |session, _| {
        session.stall_next_image_hydration_for_test()
    });
    for resources in [
        vec![first.as_str().to_owned()],
        vec![stale.as_str().to_owned()],
        vec![final_image.as_str().to_owned()],
    ] {
        active.update(cx, |session, session_cx| {
            session.editor().update(session_cx, |editor, editor_cx| {
                editor.request_image_hydration(resources);
                editor_cx.notify();
            });
        });
        cx.run_until_parked();
    }
    release.send(()).expect("release first hydration");
    cx.run_until_parked();

    let opened = opens.try_iter().collect::<Vec<_>>();
    assert!(
        opened
            .iter()
            .all(|hash| hash == &hashes[0] || hash == &hashes[2]),
        "an offscreen stale viewport must be pruned before it starts a verified read; opened={opened:?}"
    );
    assert!(
        opened.iter().any(|hash| hash == &hashes[0]),
        "the initially active resident remains allowed to finish"
    );
    assert!(
        opened.iter().any(|hash| hash == &hashes[2]),
        "the final resident request must replace stale queued work"
    );
    assert!(
        !opened.iter().any(|hash| hash == &hashes[1]),
        "the intermediate viewport must never be opened after scrolling away"
    );
}

#[gpui::test]
async fn visible_legacy_image_repairs_its_geometry_once_then_reopens_without_layout_jump(
    cx: &mut gpui::TestAppContext,
) {
    // Old canonical HTML has no natural-size attributes. It gets a stable
    // fallback first frame, then the first visible descriptor inspection may
    // repair only that legacy node outside History. The ordinary snapshot
    // persists the repaired geometry so a second open does not jump again.
    let cx = cx.add_empty_window();
    let (_profile, repository) = repository();
    let image = repository
        .import_resource(
            &structural_png(675, 1200),
            "legacy-vertical.png",
            "image/png",
            "png",
        )
        .expect("persist vertical image");
    let note = create(
        &repository,
        "legacy geometry",
        CanonicalDocument::from_blocks(vec![Block::Image {
            resource_id: image.clone(),
            alt: "vertical".into(),
            presentation: Default::default(),
        }]),
    );
    let clock = Arc::new(ManualSaveClock::default());
    let active = session(
        note.clone(),
        Arc::clone(&repository),
        Arc::clone(&clock),
        cx,
    );
    let initial_size = active.read_with(cx, |session, app| {
        session
            .editor()
            .read(app)
            .document()
            .blocks()
            .iter()
            .find_map(|block| match &block.content {
                BlockContent::Image { natural_size, .. } => Some(*natural_size),
                _ => None,
            })
            .expect("legacy image atom")
    });
    assert_eq!(
        initial_size,
        (1024, 768),
        "legacy fallback is stable before I/O"
    );

    active.update(cx, |session, session_cx| {
        session.editor().update(session_cx, |editor, editor_cx| {
            assert!(editor.request_image_hydration([image.as_str().to_owned()]));
            editor_cx.notify();
        });
    });
    cx.run_until_parked();
    let (repaired_size, undo_depth) = active.read_with(cx, |session, app| {
        let editor = session.editor().read(app);
        (
            editor
                .document()
                .blocks()
                .iter()
                .find_map(|block| match &block.content {
                    BlockContent::Image { natural_size, .. } => Some(*natural_size),
                    _ => None,
                })
                .expect("repaired image atom"),
            editor.undo_depth(),
        )
    });
    assert_eq!(repaired_size, (675, 1200));
    assert_eq!(
        undo_depth, 0,
        "loader geometry repair must not manufacture undo history"
    );

    flush_until_clean(&active, FlushReason::ManualSync, cx);
    drop(active);
    let saved = repository
        .load_note(&note.id)
        .expect("load repaired note")
        .expect("note exists");
    assert!(
        saved
            .body_html
            .contains("data-joplin-lite-natural-width=\"675\""),
        "the ordinary semantic snapshot must persist the one-time legacy repair"
    );
    let reopened = session(saved, Arc::clone(&repository), clock, cx);
    let reopened_size = reopened.read_with(cx, |session, app| {
        session
            .editor()
            .read(app)
            .document()
            .blocks()
            .iter()
            .find_map(|block| match &block.content {
                BlockContent::Image { natural_size, .. } => Some(*natural_size),
                _ => None,
            })
            .expect("reopened image atom")
    });
    assert_eq!(
        reopened_size,
        (675, 1200),
        "new canonical presentation must reserve the same extent before a second hydration"
    );
}

#[gpui::test]
async fn offscreen_legacy_image_keeps_unknown_geometry_until_visible_repair(
    cx: &mut gpui::TestAppContext,
) {
    // A legacy image may remain outside the viewport while the user edits
    // nearby text. That ordinary save must preserve the absence of durable
    // dimensions: exporting the display fallback as a real presentation would
    // permanently turn a vertical image into 4:3 before it ever hydrates.
    let cx = cx.add_empty_window();
    let (_profile, repository) = repository();
    let image = repository
        .import_resource(
            &structural_png(675, 1200),
            "legacy-offscreen.png",
            "image/png",
            "png",
        )
        .expect("persist vertical legacy image");
    let note = create(
        &repository,
        "legacy offscreen",
        CanonicalDocument::from_blocks(vec![
            Block::Image {
                resource_id: image.clone(),
                alt: "vertical".into(),
                presentation: Default::default(),
            },
            Block::Paragraph {
                style: BlockStyle::default(),
                inlines: vec![Inline::Text {
                    text: "正文".into(),
                    marks: Marks::default(),
                }],
            },
        ]),
    );
    let clock = Arc::new(ManualSaveClock::default());
    let active = session(
        note.clone(),
        Arc::clone(&repository),
        Arc::clone(&clock),
        cx,
    );

    // No render residency request is made before this explicit text edit.
    append_body_via_entity_input(&active, "仍在屏外保存", cx);
    flush_until_clean(&active, FlushReason::ManualSync, cx);
    drop(active);

    let after_text_save = repository
        .load_note(&note.id)
        .expect("load ordinary text snapshot")
        .expect("note exists");
    assert!(
        !after_text_save
            .body_html
            .contains("data-joplin-lite-natural-width"),
        "an offscreen legacy image must remain unknown until a real visible decode repairs it"
    );

    let reopened = session(
        after_text_save,
        Arc::clone(&repository),
        Arc::clone(&clock),
        cx,
    );
    // This is the first simulated scroll/residency request. Only now may the
    // worker inspect bytes and turn the legacy fallback into durable geometry.
    reopened.update(cx, |session, session_cx| {
        session.editor().update(session_cx, |editor, editor_cx| {
            assert!(editor.request_image_hydration([image.as_str().to_owned()]));
            editor_cx.notify();
        });
    });
    cx.run_until_parked();
    flush_until_clean(&reopened, FlushReason::ManualSync, cx);
    drop(reopened);

    let repaired = repository
        .load_note(&note.id)
        .expect("load repaired snapshot")
        .expect("note exists");
    assert!(
        repaired
            .body_html
            .contains("data-joplin-lite-natural-width=\"675\"")
    );
    assert!(
        repaired
            .body_html
            .contains("data-joplin-lite-natural-height=\"1200\"")
    );

    let reopened_again = session(repaired, Arc::clone(&repository), clock, cx);
    let natural_size = reopened_again.read_with(cx, |session, app| {
        session
            .editor()
            .read(app)
            .document()
            .blocks()
            .iter()
            .find_map(|block| match &block.content {
                BlockContent::Image { natural_size, .. } => Some(*natural_size),
                _ => None,
            })
            .expect("repaired image atom")
    });
    assert_eq!(natural_size, (675, 1200));
}

#[gpui::test]
async fn journal_is_readable_at_100ms_and_snapshot_waits_for_500ms_settle(
    cx: &mut gpui::TestAppContext,
) {
    let cx = cx.add_empty_window();
    let (_profile, repository) = repository();
    let note = create(&repository, "计时", CanonicalDocument::default());
    let clock = Arc::new(ManualSaveClock::default());
    let active = session(
        note.clone(),
        Arc::clone(&repository),
        Arc::clone(&clock),
        cx,
    );

    append_body_via_entity_input(&active, "可恢复", cx);
    clock.advance(Duration::from_millis(99));
    poll_and_drain(&active, cx);
    assert!(
        repository
            .latest_edit_journal(&note.id)
            .expect("read journal")
            .is_none()
    );

    clock.advance(Duration::from_millis(1));
    poll_and_drain(&active, cx);
    let journal = repository
        .latest_edit_journal(&note.id)
        .expect("read journal")
        .expect("100ms journal");
    assert!(journal.delta_utf8.contains("可恢复"));
    assert!(journal.delta_utf8.contains("\"body\""));
    assert!(
        !journal.delta_utf8.contains("\"body_html\""),
        "v2 recovery payload must be a delta rather than a repeated HTML snapshot"
    );
    assert_eq!(
        repository
            .load_note(&note.id)
            .expect("load")
            .expect("note")
            .revision,
        note.revision,
        "journal alone must not pretend the settled snapshot committed"
    );

    clock.advance(Duration::from_millis(399));
    poll_and_drain(&active, cx);
    assert_eq!(
        repository
            .load_note(&note.id)
            .expect("load")
            .expect("note")
            .revision,
        note.revision
    );

    clock.advance(Duration::from_millis(1));
    poll_and_drain(&active, cx);
    assert_eq!(
        repository
            .load_note(&note.id)
            .expect("load")
            .expect("note")
            .revision,
        note.revision + 1
    );
    assert!(
        repository
            .latest_edit_journal(&note.id)
            .expect("read journal")
            .is_none()
    );
    assert_eq!(
        active.read_with(cx, |session, _| session.save_state()),
        SaveState::Clean
    );
}

#[gpui::test]
async fn journal_checkpoint_is_a_compact_readable_delta_not_a_second_full_document(
    cx: &mut gpui::TestAppContext,
) {
    let cx = cx.add_empty_window();
    let (_profile, repository) = repository();
    let original_body = "旧".repeat(8_192);
    let note = create(&repository, "紧凑日志", rich_document(&original_body));
    let clock = Arc::new(ManualSaveClock::default());
    let active = session(
        note.clone(),
        Arc::clone(&repository),
        Arc::clone(&clock),
        cx,
    );

    append_body_via_entity_input(&active, "新", cx);
    clock.advance(Duration::from_millis(100));
    poll_and_drain(&active, cx);
    let journal = repository
        .latest_edit_journal(&note.id)
        .expect("read compact checkpoint")
        .expect("checkpoint");
    assert!(journal.delta_utf8.contains("\"version\":2"));
    assert!(journal.delta_utf8.contains("\"insert\":\"新\""));
    assert!(!journal.delta_utf8.contains("\"body_html\""));
    assert!(
        journal.delta_utf8.len() * 20 < note.body_html.len(),
        "one-character edit journal unexpectedly scaled with the full HTML body"
    );
}

#[gpui::test]
async fn retained_deadline_task_dispatches_the_100ms_checkpoint_without_foreground_polling(
    cx: &mut gpui::TestAppContext,
) {
    // This is intentionally not `session.poll()`: it drives the actual GPUI
    // retained timer and then the same background SaveJob that production
    // uses. Restoring a 50ms shell tick or removing the deadline task leaves
    // this journal absent at the exact 100ms boundary.
    let cx = cx.add_empty_window();
    let (_profile, repository) = repository();
    let note = create(&repository, "定时器", rich_document("正文"));
    let clock = Arc::new(ManualSaveClock::default());
    let active = session(
        note.clone(),
        Arc::clone(&repository),
        Arc::clone(&clock),
        cx,
    );
    active.update(cx, |session, _| session.enable_deadline_tasks_for_test());
    append_body_via_entity_input(&active, "可恢复", cx);

    clock.advance(Duration::from_millis(99));
    cx.executor().advance_clock(Duration::from_millis(99));
    assert!(repository.latest_edit_journal(&note.id).unwrap().is_none());

    clock.advance(Duration::from_millis(1));
    cx.executor().advance_clock(Duration::from_millis(1));
    cx.run_until_parked();
    let journal = repository
        .latest_edit_journal(&note.id)
        .expect("read direct deadline journal")
        .expect("exact 100ms timer checkpoint");
    assert!(journal.delta_utf8.contains("可恢复"));
    assert!(
        matches!(
            active.read_with(cx, |session, _| session.save_state()),
            SaveState::Dirty
        ),
        "a journal alone must not publish Clean"
    );
}

#[gpui::test]
async fn retained_journal_deadline_is_anchored_to_the_first_unjournaled_edit(
    cx: &mut gpui::TestAppContext,
) {
    // Continuous typing must not keep postponing the crash-recovery
    // checkpoint. Each input below lands before the previous 100ms deadline;
    // only a timer anchored to the first unjournaled edit can publish a
    // readable checkpoint before the 15s hard-snapshot ceiling.
    let cx = cx.add_empty_window();
    let (_profile, repository) = repository();
    let note = create(&repository, "连续日志", rich_document("基线"));
    let clock = Arc::new(ManualSaveClock::default());
    let active = session(
        note.clone(),
        Arc::clone(&repository),
        Arc::clone(&clock),
        cx,
    );
    active.update(cx, |session, _| session.enable_deadline_tasks_for_test());

    for _ in 0..12 {
        append_body_via_entity_input(&active, "续", cx);
        clock.advance(Duration::from_millis(50));
        cx.executor().advance_clock(Duration::from_millis(50));
        cx.run_until_parked();
    }

    let checkpoint = repository
        .latest_edit_journal(&note.id)
        .expect("read continuous-input checkpoint")
        .expect("first 100ms deadline must not be reset by later input");
    assert!(checkpoint.delta_utf8.contains("续"));
    assert!(checkpoint.sequence > 0);
}

#[gpui::test]
async fn continuous_edits_force_a_snapshot_at_fifteen_seconds_without_sleeping(
    cx: &mut gpui::TestAppContext,
) {
    let cx = cx.add_empty_window();
    let (_profile, repository) = repository();
    let note = create(&repository, "持续", CanonicalDocument::default());
    let clock = Arc::new(ManualSaveClock::default());
    let active = session(
        note.clone(),
        Arc::clone(&repository),
        Arc::clone(&clock),
        cx,
    );

    for _ in 0..37 {
        append_title_via_entity_input(&active, "续", cx);
        clock.advance(Duration::from_millis(400));
        poll_and_drain(&active, cx);
    }
    assert_eq!(
        repository
            .load_note(&note.id)
            .expect("load")
            .expect("note")
            .revision,
        note.revision,
        "15 seconds have not elapsed yet"
    );
    append_title_via_entity_input(&active, "终", cx);
    clock.advance(Duration::from_millis(200));
    poll_and_drain(&active, cx);
    assert_eq!(
        repository
            .load_note(&note.id)
            .expect("load")
            .expect("note")
            .revision,
        note.revision + 1,
        "hard maximum must snapshot even while input remains active"
    );
}

#[gpui::test]
async fn journal_recovery_reconstructs_unsnapshotted_input_and_all_flush_reasons_compact_it(
    cx: &mut gpui::TestAppContext,
) {
    let cx = cx.add_empty_window();
    let (_profile, repository) = repository();
    let note = create(&repository, "恢复", rich_document("原正文"));
    let clock = Arc::new(ManualSaveClock::default());
    let active = session(
        note.clone(),
        Arc::clone(&repository),
        Arc::clone(&clock),
        cx,
    );
    append_title_via_entity_input(&active, "后的标题", cx);
    append_body_via_entity_input(&active, "后的正文", cx);
    clock.advance(Duration::from_millis(100));
    poll_and_drain(&active, cx);
    drop(active);

    let base = repository
        .load_note(&note.id)
        .expect("base note")
        .expect("base exists");
    let recovered = session(base, Arc::clone(&repository), Arc::clone(&clock), cx);
    assert_eq!(
        recovered.read_with(cx, |session, session_cx| session
            .title()
            .read(session_cx)
            .text()
            .to_owned()),
        "恢复后的标题"
    );
    assert!(
        recovered
            .read_with(cx, |session, session_cx| session
                .editor()
                .read(session_cx)
                .visible_text())
            .contains("后的正文")
    );

    for reason in [
        FlushReason::NoteSwitch,
        FlushReason::WindowClose,
        FlushReason::Quit,
        FlushReason::Delete,
        FlushReason::ManualSync,
    ] {
        flush_until_clean(&recovered, reason, cx);
        assert!(
            repository
                .latest_edit_journal(&note.id)
                .expect("read journal")
                .is_none()
        );
    }
}

#[gpui::test]
async fn recovered_checkpoint_can_journal_new_chinese_input_then_snapshot_and_restart_exactly(
    cx: &mut gpui::TestAppContext,
) {
    // Catches a recovered session that displays a valid checkpoint but starts
    // its next 100ms journal with an unrelated writer token. The real
    // repository CAS must let the recovered owner append, compact at the
    // settled deadline, and survive another crash/restart without flattening
    // the recovered rich text.
    let cx = cx.add_empty_window();
    let (_profile, repository) = repository();
    let note = create(&repository, "初始标题", rich_document("加粗斜体基线"));
    let clock = Arc::new(ManualSaveClock::default());
    let crashed = session(
        note.clone(),
        Arc::clone(&repository),
        Arc::clone(&clock),
        cx,
    );
    append_title_via_entity_input(&crashed, "第一次恢复", cx);
    append_body_via_entity_input(&crashed, "第一次正文", cx);
    clock.advance(Duration::from_millis(100));
    poll_and_drain(&crashed, cx);
    let before_restart = repository
        .latest_edit_journal(&note.id)
        .expect("read first crash checkpoint")
        .expect("first checkpoint exists");
    assert!(before_restart.delta_utf8.contains("第一次正文"));
    drop(crashed);

    let recovered_base = repository
        .load_note(&note.id)
        .expect("load durable base after crash")
        .expect("base exists");
    let recovered = session(
        recovered_base,
        Arc::clone(&repository),
        Arc::clone(&clock),
        cx,
    );
    append_title_via_entity_input(&recovered, "继续中文", cx);
    append_body_via_entity_input(&recovered, "继续正文", cx);
    clock.advance(Duration::from_millis(100));
    poll_and_drain(&recovered, cx);
    let continued_checkpoint = repository
        .latest_edit_journal(&note.id)
        .expect("read continued checkpoint")
        .expect("continued input must reach the 100ms journal");
    assert!(continued_checkpoint.delta_utf8.contains("继续正文"));
    assert_ne!(
        continued_checkpoint.writer_token, before_restart.writer_token,
        "recovery must claim rather than share the crashed writer token"
    );
    assert!(matches!(
        recovered.read_with(cx, |session, _| session.save_state()),
        SaveState::Dirty
    ));

    clock.advance(Duration::from_millis(400));
    poll_and_drain(&recovered, cx);
    assert!(
        repository
            .latest_edit_journal(&note.id)
            .expect("read compacted journal")
            .is_none(),
        "the settled snapshot must compact the owned recovery journal"
    );
    drop(recovered);

    let exact = repository
        .load_note(&note.id)
        .expect("load durable continued snapshot")
        .expect("continued note exists");
    let restarted = session(exact, Arc::clone(&repository), clock, cx);
    let (title, visible, semantic) = restarted.read_with(cx, |session, session_cx| {
        (
            session.title().read(session_cx).text().to_owned(),
            session.editor().read(session_cx).visible_text(),
            session
                .editor()
                .read(session_cx)
                .document()
                .semantic_snapshot(),
        )
    });
    assert_eq!(title, "初始标题第一次恢复继续中文");
    assert!(visible.contains("第一次正文"));
    assert!(visible.contains("继续正文"));
    let semantic = format!("{semantic:?}");
    assert!(semantic.contains("Bold"));
    assert!(semantic.contains("Italic"));
    assert!(semantic.contains("https://example.test/中文"));
}

#[gpui::test]
async fn chinese_ime_composition_blocks_lifecycle_flush_until_the_real_input_commit(
    cx: &mut gpui::TestAppContext,
) {
    let cx = cx.add_empty_window();
    let (_profile, repository) = repository();
    let note = create(&repository, "输入法", rich_document("原正文"));
    let clock = Arc::new(ManualSaveClock::default());
    let active = session(
        note.clone(),
        Arc::clone(&repository),
        Arc::clone(&clock),
        cx,
    );
    let editor = active.read_with(cx, |session, _| session.editor().clone());

    // This is the same provisional path macOS takes while a Chinese IME is
    // presenting candidates. It must neither journal a candidate nor let a
    // close boundary silently lose it.
    cx.update(|window, app| {
        editor.update(app, |editor, editor_cx| {
            let end = editor.document().flat_utf16_len();
            <crate::native_editor::core::EditorCore as EntityInputHandler>::replace_and_mark_text_in_range(
                editor,
                Some(end..end),
                "候选",
                Some((end + 2)..(end + 2)),
                window,
                editor_cx,
            );
        });
    });
    clock.advance(Duration::from_secs(16));
    poll_and_drain(&active, cx);
    assert!(
        repository
            .latest_edit_journal(&note.id)
            .expect("read journal")
            .is_none()
    );
    assert_eq!(
        repository
            .load_note(&note.id)
            .expect("load")
            .expect("note")
            .revision,
        note.revision
    );
    let error = active.update(cx, |session, session_cx| {
        session
            .flush(FlushReason::WindowClose, session_cx)
            .expect_err("uncommitted IME text must visibly block close")
    });
    assert!(error.to_string().contains("组合"));

    cx.update(|window, app| {
        editor.update(app, |editor, editor_cx| {
            <crate::native_editor::core::EditorCore as EntityInputHandler>::unmark_text(
                editor, window, editor_cx,
            );
        });
    });
    assert_eq!(
        active.read_with(cx, |session, _| session.save_state()),
        SaveState::Dirty,
        "the real input commit notification must reach the retained session"
    );
    flush_until_clean(&active, FlushReason::WindowClose, cx);
    assert!(
        repository
            .load_note(&note.id)
            .expect("load")
            .expect("note")
            .body_html
            .contains("候选")
    );
}

#[gpui::test]
async fn dirty_body_before_ime_candidate_never_serializes_the_provisional_text(
    cx: &mut gpui::TestAppContext,
) {
    let cx = cx.add_empty_window();
    let (_profile, repository) = repository();
    let note = create(&repository, "组合输入", rich_document("原正文"));
    let clock = Arc::new(ManualSaveClock::default());
    let active = session(
        note.clone(),
        Arc::clone(&repository),
        Arc::clone(&clock),
        cx,
    );

    // First make one ordinary, committed edit. This creates the generation
    // whose due timer used to read the editor again after IME composition had
    // replaced the live text with a provisional candidate.
    append_body_via_entity_input(&active, "已确认", cx);
    let editor = active.read_with(cx, |session, _| session.editor().clone());
    let candidate_start = cx.update(|_window, app| editor.read(app).document().flat_utf16_len());
    cx.update(|window, app| {
        editor.update(app, |editor, editor_cx| {
            <crate::native_editor::core::EditorCore as EntityInputHandler>::replace_and_mark_text_in_range(
                editor,
                Some(candidate_start..candidate_start),
                "候选",
                Some((candidate_start + 2)..(candidate_start + 2)),
                window,
                editor_cx,
            );
        });
    });

    clock.advance(Duration::from_millis(100));
    poll_and_drain(&active, cx);
    let journal = repository
        .latest_edit_journal(&note.id)
        .expect("read journal while marked");
    assert!(
        journal
            .as_ref()
            .is_none_or(|entry| !entry.delta_utf8.contains("候选")),
        "a timer scheduled before composition must never serialize live candidate text"
    );

    clock.advance(Duration::from_millis(400));
    poll_and_drain(&active, cx);
    clock.advance(Duration::from_secs(15));
    poll_and_drain(&active, cx);
    let durable = repository
        .load_note(&note.id)
        .expect("load durable note")
        .expect("note exists");
    assert!(
        !durable.body_text.contains("候选"),
        "all due paths must use the last committed immutable payload"
    );

    // Simulate the platform cancelling the candidate by replacing the marked
    // range with nothing. The cancellation must not resurrect the candidate
    // from either journal recovery or the previously due snapshot.
    cx.update(|window, app| {
        editor.update(app, |editor, editor_cx| {
            <crate::native_editor::core::EditorCore as EntityInputHandler>::replace_text_in_range(
                editor,
                Some(candidate_start..candidate_start + 2),
                "",
                window,
                editor_cx,
            );
        });
    });
    poll_and_drain(&active, cx);
    let after_cancel = repository
        .load_note(&note.id)
        .expect("reload after cancellation")
        .expect("note exists");
    assert!(after_cancel.body_text.contains("已确认"));
    assert!(!after_cancel.body_text.contains("候选"));
}

#[gpui::test]
async fn dirty_title_before_ime_candidate_never_serializes_the_provisional_text(
    cx: &mut gpui::TestAppContext,
) {
    // Title composition has its own EntityInputHandler and mark state; do not
    // let body-only coverage hide a scheduler that snapshots the title live.
    let cx = cx.add_empty_window();
    let (_profile, repository) = repository();
    let note = create(&repository, "原标题", rich_document("正文"));
    let clock = Arc::new(ManualSaveClock::default());
    let active = session(
        note.clone(),
        Arc::clone(&repository),
        Arc::clone(&clock),
        cx,
    );
    append_title_via_entity_input(&active, "已确认", cx);
    let title = active.read_with(cx, |session, _| session.title().clone());
    let candidate_start = cx.update(|_window, app| title.read(app).text().encode_utf16().count());
    cx.update(|window, app| {
        title.update(app, |title, title_cx| {
            <crate::native_editor::chrome::TitleInput as EntityInputHandler>::replace_and_mark_text_in_range(
                title,
                Some(candidate_start..candidate_start),
                "候选",
                Some((candidate_start + 2)..(candidate_start + 2)),
                window,
                title_cx,
            );
        });
    });

    for elapsed in [
        Duration::from_millis(100),
        Duration::from_millis(400),
        Duration::from_secs(15),
    ] {
        clock.advance(elapsed);
        poll_and_drain(&active, cx);
    }
    let journal = repository
        .latest_edit_journal(&note.id)
        .expect("read marked-title journal");
    assert!(
        journal
            .as_ref()
            .is_none_or(|entry| !entry.delta_utf8.contains("候选"))
    );
    let durable = repository.load_note(&note.id).unwrap().expect("note");
    assert!(!durable.title.contains("候选"));

    // Cancel through the real input handler. Its resulting semantic title is
    // the original committed edit, which must finally be durable rather than
    // leaving the session frozen after its old deadlines passed.
    cx.update(|window, app| {
        title.update(app, |title, title_cx| {
            <crate::native_editor::chrome::TitleInput as EntityInputHandler>::replace_text_in_range(
                title,
                Some(candidate_start..candidate_start + 2),
                "",
                window,
                title_cx,
            );
        });
    });
    poll_and_drain(&active, cx);
    let after_cancel = repository.load_note(&note.id).unwrap().expect("note");
    assert!(after_cancel.title.contains("已确认"));
    assert!(!after_cancel.title.contains("候选"));
}

#[gpui::test]
async fn background_journal_job_uses_the_unmarked_snapshot_captured_before_ime(
    cx: &mut gpui::TestAppContext,
) {
    // This intentionally dispatches the retained production worker instead
    // of the deterministic synchronous `poll` seam. If the worker rereads a
    // live entity, the candidate installed after dispatch leaks into SQLite.
    let cx = cx.add_empty_window();
    let (_profile, repository) = repository();
    let note = create(&repository, "后台快照", rich_document("原正文"));
    let clock = Arc::new(ManualSaveClock::default());
    let active = session(
        note.clone(),
        Arc::clone(&repository),
        Arc::clone(&clock),
        cx,
    );
    append_body_via_entity_input(&active, "已确认", cx);
    let editor = active.read_with(cx, |session, _| session.editor().clone());

    clock.advance(Duration::from_millis(100));
    active.update(cx, |session, session_cx| {
        session.dispatch_due_background_work_for_test(session_cx)
    });
    assert!(matches!(
        active.read_with(cx, |session, _| session.save_state()),
        SaveState::Journaling
    ));

    // The worker is now queued with the immutable committed document. Mutate
    // the live entity through the real IME handler before TestApp schedules
    // that worker, exactly the race that used to serialize a candidate.
    let candidate_start = cx.update(|_window, app| editor.read(app).document().flat_utf16_len());
    cx.update(|window, app| {
        editor.update(app, |editor, editor_cx| {
            <crate::native_editor::core::EditorCore as EntityInputHandler>::replace_and_mark_text_in_range(
                editor,
                Some(candidate_start..candidate_start),
                "候选",
                Some((candidate_start + 2)..(candidate_start + 2)),
                window,
                editor_cx,
            );
        });
    });
    cx.run_until_parked();

    let journal = repository
        .latest_edit_journal(&note.id)
        .expect("read background checkpoint")
        .expect("journal written by worker");
    assert!(journal.delta_utf8.contains("已确认"));
    assert!(
        !journal.delta_utf8.contains("候选"),
        "the worker must serialize its captured immutable snapshot"
    );
    assert!(matches!(
        active.read_with(cx, |session, _| session.save_state()),
        SaveState::Dirty
    ));
}

#[gpui::test]
async fn gated_older_writer_cannot_replace_a_newer_same_note_checkpoint(
    cx: &mut gpui::TestAppContext,
) {
    // A captures first, but its actual SQLite append is stopped in the
    // production worker. B uses an independently opened repository/session
    // and commits a same-revision checkpoint before A wakes. Writer identity
    // must participate in the database CAS so A cannot delete B merely by
    // completing later.
    let cx = cx.add_empty_window();
    let (profile, repository_a) = repository();
    let repository_b = Arc::new(
        LibraryRepository::open(profile.path().join("library.sqlite"))
            .expect("open independent second repository"),
    );
    let note = create(&repository_a, "并发 journal", rich_document("基线"));
    let note_a = repository_a
        .load_note(&note.id)
        .unwrap()
        .expect("load A note");
    let note_b = repository_b
        .load_note(&note.id)
        .unwrap()
        .expect("load B note");
    let clock_a = Arc::new(ManualSaveClock::default());
    let clock_b = Arc::new(ManualSaveClock::default());
    let a = session(note_a, Arc::clone(&repository_a), Arc::clone(&clock_a), cx);
    let b = session(note_b, Arc::clone(&repository_b), Arc::clone(&clock_b), cx);
    a.update(cx, |session, _| session.enable_deadline_tasks_for_test());
    b.update(cx, |session, _| session.enable_deadline_tasks_for_test());

    append_body_via_entity_input(&a, " A旧", cx);
    let release_a = a.update(cx, |session, _| {
        session.stall_next_background_save_for_test()
    });
    clock_a.advance(Duration::from_millis(100));
    a.update(cx, |session, session_cx| {
        session.dispatch_due_background_work_for_test(session_cx)
    });
    assert!(matches!(
        a.read_with(cx, |session, _| session.save_state()),
        SaveState::Journaling
    ));

    append_body_via_entity_input(&b, " B新", cx);
    clock_b.advance(Duration::from_millis(100));
    b.update(cx, |session, session_cx| {
        session.dispatch_due_background_work_for_test(session_cx)
    });
    cx.run_until_parked();
    let b_checkpoint = repository_b
        .latest_edit_journal(&note.id)
        .expect("read B checkpoint")
        .expect("B writes while A remains gated");
    assert!(b_checkpoint.delta_utf8.contains("B新"));

    release_a.send(()).expect("wake late A worker");
    cx.run_until_parked();
    assert!(matches!(
        a.read_with(cx, |session, _| session.save_state()),
        SaveState::Failed(ref error) if error.contains("writer")
    ));
    let recovered = repository_a
        .latest_edit_journal(&note.id)
        .expect("read surviving checkpoint")
        .expect("B checkpoint remains");
    assert_eq!(recovered.writer_token, b_checkpoint.writer_token);
    assert!(recovered.delta_utf8.contains("B新"));
    assert!(!recovered.delta_utf8.contains("A旧"));
}

#[gpui::test]
async fn two_recovery_candidates_can_claim_one_checkpoint_and_old_writer_cannot_retake_it(
    cx: &mut gpui::TestAppContext,
) {
    // Both candidates intentionally prepare from the same durable crashed
    // journal before either transfer begins. The real SQLite CAS must allow
    // exactly one future entity owner, while the old process token remains
    // unable to append after it wakes up.
    let cx = cx.add_empty_window();
    let (profile, repository_a) = repository();
    let repository_b = Arc::new(
        LibraryRepository::open(profile.path().join("library.sqlite"))
            .expect("open independent recovery repository"),
    );
    let note = create(&repository_a, "竞争恢复", rich_document("恢复基线"));
    let clock = Arc::new(ManualSaveClock::default());
    let crashed = session(
        note.clone(),
        Arc::clone(&repository_a),
        Arc::clone(&clock),
        cx,
    );
    append_body_via_entity_input(&crashed, "崩溃前内容", cx);
    clock.advance(Duration::from_millis(100));
    poll_and_drain(&crashed, cx);
    let crashed_checkpoint = repository_a
        .latest_edit_journal(&note.id)
        .expect("read crashed checkpoint")
        .expect("checkpoint exists");
    drop(crashed);

    let base_a = repository_a
        .load_note(&note.id)
        .expect("load candidate A base")
        .expect("A base exists");
    let base_b = repository_b
        .load_note(&note.id)
        .expect("load candidate B base")
        .expect("B base exists");
    let prepared_a = NoteSession::prepare(base_a, repository_a.as_ref())
        .expect("prepare first recovery candidate");
    let prepared_b = NoteSession::prepare(base_b, repository_b.as_ref())
        .expect("prepare second recovery candidate");

    let claimed_a = prepared_a
        .claim_recovery_ownership(repository_a.as_ref())
        .expect("first candidate claims the exact crashed owner");
    let losing_claim = match prepared_b.claim_recovery_ownership(repository_b.as_ref()) {
        Ok(_) => panic!("second candidate must not steal the already-transferred checkpoint"),
        Err(error) => error,
    };
    assert!(losing_claim.to_string().contains("writer"));
    let winner = repository_a
        .latest_edit_journal(&note.id)
        .expect("read claimed checkpoint")
        .expect("winner checkpoint remains");
    assert_ne!(winner.writer_token, crashed_checkpoint.writer_token);
    assert!(winner.delta_utf8.contains(&winner.writer_token));

    let old_writer = repository_b
        .append_edit_journal(EditJournalEntry {
            note_id: note.id.clone(),
            expected_revision: note.revision,
            writer_token: crashed_checkpoint.writer_token,
            sequence: 0,
            generation: crashed_checkpoint.generation + 1,
            delta_utf8: crashed_checkpoint.delta_utf8,
        })
        .expect_err("the pre-crash writer cannot reclaim a transferred checkpoint");
    assert!(matches!(old_writer, LibraryError::JournalOwnershipConflict));
    assert_eq!(
        repository_a
            .latest_edit_journal(&note.id)
            .expect("winner is still present")
            .expect("winner checkpoint")
            .writer_token,
        winner.writer_token
    );

    let _mounted_winner = cx.new(move |session_cx| {
        NoteSession::from_prepared(claimed_a, Arc::clone(&repository_a), clock, session_cx)
    });
}

#[gpui::test]
async fn prepared_recovery_rejects_a_gated_same_owner_checkpoint_replacement(
    cx: &mut gpui::TestAppContext,
) {
    // A restart candidate captures J1, then the crashed session's already
    // dispatched worker publishes J2 with the same owner before the claim can
    // begin. A claim tied only to token/revision would replace J2's durable
    // bytes with the stale J1 payload; it must instead reject the stale
    // prepared identity and leave J2 byte-for-byte intact.
    let cx = cx.add_empty_window();
    let (profile, repository_a) = repository();
    let repository_b = Arc::new(
        LibraryRepository::open(profile.path().join("library.sqlite"))
            .expect("open independent recovery repository"),
    );
    let note = create(&repository_a, "准备后替换", rich_document("持久化基线"));
    let clock_a = Arc::new(ManualSaveClock::default());
    let crashed = session(
        note.clone(),
        Arc::clone(&repository_a),
        Arc::clone(&clock_a),
        cx,
    );

    append_body_via_entity_input(&crashed, " J1恢复内容", cx);
    clock_a.advance(Duration::from_millis(100));
    poll_and_drain(&crashed, cx);
    let j1 = repository_a
        .latest_edit_journal(&note.id)
        .expect("read first checkpoint")
        .expect("J1 exists");

    let base_b = repository_b
        .load_note(&note.id)
        .expect("load restart base")
        .expect("restart base exists");
    let prepared = NoteSession::prepare(base_b, repository_b.as_ref())
        .expect("prepare the exact J1 recovery identity");

    append_body_via_entity_input(&crashed, " J2新检查点", cx);
    let release_j2 = crashed.update(cx, |session, _| {
        session.stall_next_background_save_for_test()
    });
    clock_a.advance(Duration::from_millis(100));
    crashed.update(cx, |session, session_cx| {
        session.dispatch_due_background_work_for_test(session_cx)
    });
    assert!(matches!(
        crashed.read_with(cx, |session, _| session.save_state()),
        SaveState::Journaling
    ));
    release_j2
        .send(())
        .expect("release the already-dispatched J2 worker");
    cx.run_until_parked();
    let j2 = repository_a
        .latest_edit_journal(&note.id)
        .expect("read newer checkpoint")
        .expect("J2 replaces J1 under the crashed owner");
    assert_eq!(j2.writer_token, j1.writer_token);
    assert!(j2.sequence > j1.sequence);
    assert!(j2.delta_utf8.contains("J2新检查点"));

    let claim = match prepared.claim_recovery_ownership(repository_b.as_ref()) {
        Ok(_) => panic!("a prepared J1 must not claim or overwrite newer J2"),
        Err(error) => error,
    };
    assert!(claim.to_string().contains("writer"));
    let surviving = repository_b
        .latest_edit_journal(&note.id)
        .expect("read checkpoint after rejected stale claim")
        .expect("newer checkpoint remains");
    assert_eq!(surviving.writer_token, j2.writer_token);
    assert_eq!(surviving.sequence, j2.sequence);
    assert_eq!(surviving.delta_utf8, j2.delta_utf8);
}

#[gpui::test]
async fn claimed_recovery_rejects_a_gated_former_owner_snapshot_and_allows_the_new_owner(
    cx: &mut gpui::TestAppContext,
) {
    // A stale snapshot job has captured A's J1 lease before it blocks in the
    // real background worker. B then claims J1 and publishes J2. Releasing A
    // must neither advance the note revision nor compact B's checkpoint; B's
    // own exact lease is the only one allowed to snapshot and clear J2.
    let cx = cx.add_empty_window();
    let (profile, repository_a) = repository();
    let repository_b = Arc::new(
        LibraryRepository::open(profile.path().join("library.sqlite"))
            .expect("open independent recovery repository"),
    );
    let note = create(&repository_a, "旧快照所有权", rich_document("持久化基线"));
    let clock_a = Arc::new(ManualSaveClock::default());
    let former_owner = session(
        note.clone(),
        Arc::clone(&repository_a),
        Arc::clone(&clock_a),
        cx,
    );

    append_body_via_entity_input(&former_owner, " A的J1", cx);
    clock_a.advance(Duration::from_millis(100));
    poll_and_drain(&former_owner, cx);
    let old_checkpoint = repository_a
        .latest_edit_journal(&note.id)
        .expect("read A checkpoint")
        .expect("A J1 exists");

    let release_old_snapshot = former_owner.update(cx, |session, _| {
        session.stall_next_background_save_for_test()
    });
    clock_a.advance(Duration::from_millis(400));
    former_owner.update(cx, |session, session_cx| {
        session.dispatch_due_background_work_for_test(session_cx)
    });
    assert!(matches!(
        former_owner.read_with(cx, |session, _| session.save_state()),
        SaveState::Snapshotting
    ));

    let base_b = repository_b
        .load_note(&note.id)
        .expect("load recovery base")
        .expect("recovery base exists");
    let clock_b = Arc::new(ManualSaveClock::default());
    let new_owner = session(base_b, Arc::clone(&repository_b), Arc::clone(&clock_b), cx);
    let claimed = repository_b
        .latest_edit_journal(&note.id)
        .expect("read claimed checkpoint")
        .expect("claim keeps J1 durable");
    assert_ne!(claimed.writer_token, old_checkpoint.writer_token);
    assert_eq!(claimed.sequence, old_checkpoint.sequence);

    append_body_via_entity_input(&new_owner, " B的J2", cx);
    clock_b.advance(Duration::from_millis(100));
    poll_and_drain(&new_owner, cx);
    let j2 = repository_b
        .latest_edit_journal(&note.id)
        .expect("read B checkpoint")
        .expect("B extends the claimed checkpoint");
    assert_eq!(j2.writer_token, claimed.writer_token);
    assert!(j2.sequence > claimed.sequence);
    assert!(j2.delta_utf8.contains("B的J2"));

    release_old_snapshot
        .send(())
        .expect("release the stale A snapshot worker");
    cx.run_until_parked();
    assert!(matches!(
        former_owner.read_with(cx, |session, _| session.save_state()),
        SaveState::Failed(ref error) if error.contains("writer")
    ));
    let still_base = repository_a
        .load_note(&note.id)
        .expect("load note after rejected stale snapshot")
        .expect("note remains");
    assert_eq!(still_base.revision, note.revision);
    let surviving = repository_a
        .latest_edit_journal(&note.id)
        .expect("read B checkpoint after old snapshot failure")
        .expect("B checkpoint must survive");
    assert_eq!(surviving.writer_token, j2.writer_token);
    assert_eq!(surviving.sequence, j2.sequence);
    assert_eq!(surviving.delta_utf8, j2.delta_utf8);

    clock_b.advance(Duration::from_millis(400));
    poll_and_drain(&new_owner, cx);
    let saved = repository_b
        .load_note(&note.id)
        .expect("load new-owner snapshot")
        .expect("new-owner snapshot exists");
    assert_eq!(saved.revision, note.revision + 1);
    assert!(saved.body_text.contains("B的J2"));
    assert!(
        repository_b
            .latest_edit_journal(&note.id)
            .expect("read compacted journal")
            .is_none(),
        "only the current owner may compact its own current checkpoint"
    );
}

#[gpui::test]
async fn v4_revision_two_journal_migrates_and_prepare_recovers_chinese_styled_content(
    cx: &mut gpui::TestAppContext,
) {
    // End-to-end release-profile fixture: the v4 SQL table has no v5 identity
    // columns, but its v1 payload carries revision two. Migration must
    // backfill that exact revision, then NoteSession::prepare must consume the
    // recovered Chinese rich text rather than silently opening the old base.
    let cx = cx.add_empty_window();
    let profile = tempfile::tempdir().expect("temporary release profile");
    let path = profile.path().join("library.sqlite");
    let repository = Arc::new(LibraryRepository::open(&path).expect("open profile"));
    let note = create(&repository, "旧标题", rich_document("旧正文"));
    repository
        .flush_snapshot(
            SaveNote {
                id: note.id.clone(),
                expected_revision: note.revision,
                title: "revision two base".into(),
                document: rich_document("持久化基线"),
                resource_ids: Vec::new(),
                selected_thumbnail_id: None,
            },
            None,
        )
        .expect("advance note to revision two");
    drop(repository);

    let connection = Connection::open(&path).expect("open v4 fixture sqlite");
    connection
        .execute_batch(
            "DROP TABLE edit_journal;
             CREATE TABLE edit_journal (
                 id TEXT PRIMARY KEY NOT NULL,
                 note_id TEXT NOT NULL,
                 generation INTEGER NOT NULL,
                 delta_utf8 TEXT NOT NULL,
                 created_time INTEGER NOT NULL
             );
             PRAGMA user_version = 4;",
        )
        .expect("seed v4 journal shape");
    let payload = format!(
        r#"{{"version":1,"note_id":"{}","expected_revision":2,"generation":9,"title":"迁移后中文标题","body_html":"<p><strong>中文加粗样式</strong>恢复正文</p>","resource_ids":[]}}"#,
        note.id.as_str()
    );
    connection
        .execute(
            "INSERT INTO edit_journal (id, note_id, generation, delta_utf8, created_time)
             VALUES (?1, ?2, 9, ?3, 1)",
            rusqlite::params!["e".repeat(32), note.id.as_str(), payload],
        )
        .expect("seed v1 checkpoint");
    drop(connection);

    let migrated = Arc::new(LibraryRepository::open(&path).expect("migrate v4 profile"));
    let durable = migrated
        .load_note(&note.id)
        .expect("load migrated durable base")
        .expect("note exists");
    assert_eq!(durable.revision, 2);
    let legacy_checkpoint = migrated
        .latest_edit_journal(&note.id)
        .expect("read migrated checkpoint")
        .expect("migrated checkpoint remains until snapshot");
    assert!(legacy_checkpoint.writer_token.starts_with("legacy-v4-"));
    let clock = Arc::new(ManualSaveClock::default());
    let restored = session(durable, Arc::clone(&migrated), Arc::clone(&clock), cx);
    let (title, body) = restored.read_with(cx, |session, session_cx| {
        (
            session.title().read(session_cx).text().to_owned(),
            session.editor().read(session_cx).visible_text(),
        )
    });
    assert_eq!(title, "迁移后中文标题");
    assert!(body.contains("中文加粗样式"));
    assert!(body.contains("恢复正文"));
    append_title_via_entity_input(&restored, "继续中文", cx);
    append_body_via_entity_input(&restored, "恢复后二次正文", cx);
    clock.advance(Duration::from_millis(100));
    poll_and_drain(&restored, cx);
    let continued_checkpoint = migrated
        .latest_edit_journal(&note.id)
        .expect("read continued migrated checkpoint")
        .expect("continued v4 recovery reaches the 100ms journal");
    assert_ne!(
        continued_checkpoint.writer_token,
        legacy_checkpoint.writer_token
    );
    assert!(continued_checkpoint.delta_utf8.contains("恢复后二次正文"));

    clock.advance(Duration::from_millis(400));
    poll_and_drain(&restored, cx);
    assert!(
        migrated
            .latest_edit_journal(&note.id)
            .expect("read compacted migrated journal")
            .is_none()
    );
    drop(restored);
    let exact = migrated
        .load_note(&note.id)
        .expect("load continued migrated snapshot")
        .expect("continued migrated note exists");
    let restarted = session(exact, Arc::clone(&migrated), clock, cx);
    let (title, visible, semantic) = restarted.read_with(cx, |session, session_cx| {
        (
            session.title().read(session_cx).text().to_owned(),
            session.editor().read(session_cx).visible_text(),
            session
                .editor()
                .read(session_cx)
                .document()
                .semantic_snapshot(),
        )
    });
    assert_eq!(title, "迁移后中文标题继续中文");
    assert!(visible.contains("恢复后二次正文"));
    let semantic = format!("{semantic:?}");
    assert!(semantic.contains("Bold"));
}

#[gpui::test]
async fn v4_multi_checkpoint_recovery_lifecycle_flush_compacts_then_continues_after_restart(
    cx: &mut gpui::TestAppContext,
) {
    // Catches a real v4 crash profile where J1 and J2 share a live base
    // revision. The session must recover J2, compact every superseded legacy
    // row on a lifecycle boundary before any new input, then accept a fresh
    // 100 ms journal / 500 ms snapshot and restart exactly. A stale revision
    // with a later timestamp is deliberately included so it cannot win just
    // because it was the final v4 append.
    let cx = cx.add_empty_window();
    let profile = tempfile::tempdir().expect("temporary release profile");
    let path = profile.path().join("library.sqlite");
    let repository = Arc::new(LibraryRepository::open(&path).expect("open profile"));
    let note = create(&repository, "旧标题", rich_document("旧正文"));
    repository
        .flush_snapshot(
            SaveNote {
                id: note.id.clone(),
                expected_revision: note.revision,
                title: "revision two base".into(),
                document: rich_document("持久化基线"),
                resource_ids: Vec::new(),
                selected_thumbnail_id: None,
            },
            None,
        )
        .expect("advance note to revision two");
    drop(repository);

    let connection = Connection::open(&path).expect("open v4 fixture sqlite");
    connection
        .execute_batch(
            "DROP TABLE edit_journal;
             CREATE TABLE edit_journal (
                 id TEXT PRIMARY KEY NOT NULL,
                 note_id TEXT NOT NULL,
                 generation INTEGER NOT NULL,
                 delta_utf8 TEXT NOT NULL,
                 created_time INTEGER NOT NULL
             );
             PRAGMA user_version = 4;",
        )
        .expect("seed v4 journal shape");
    for (id, revision, generation, created_time, title, body_html) in [
        (
            "a".repeat(32),
            2_i64,
            3_i64,
            100_i64,
            "迁移 J1",
            "<p>迁移 J1</p>",
        ),
        (
            "b".repeat(32),
            2_i64,
            9_i64,
            200_i64,
            "迁移 J2 最新标题",
            "<p><strong>迁移 J2 最新正文</strong></p>",
        ),
        (
            "c".repeat(32),
            1_i64,
            99_i64,
            999_i64,
            "过期标题不得恢复",
            "<p>过期正文不得恢复</p>",
        ),
    ] {
        let payload = format!(
            r#"{{"version":1,"note_id":"{}","expected_revision":{},"generation":{},"title":"{}","body_html":"{}","resource_ids":[]}}"#,
            note.id.as_str(),
            revision,
            generation,
            title,
            body_html
        );
        connection
            .execute(
                "INSERT INTO edit_journal (id, note_id, generation, delta_utf8, created_time)
                 VALUES (?1, ?2, ?3, ?4, ?5)",
                rusqlite::params![id, note.id.as_str(), generation, payload, created_time],
            )
            .expect("seed v4 checkpoint");
    }
    drop(connection);

    let migrated = Arc::new(LibraryRepository::open(&path).expect("migrate v4 profile"));
    let durable = migrated
        .load_note(&note.id)
        .expect("load migrated base")
        .expect("note exists");
    assert_eq!(durable.revision, 2);
    let clock = Arc::new(ManualSaveClock::default());
    let restored = session(durable, Arc::clone(&migrated), Arc::clone(&clock), cx);
    let (title, body) = restored.read_with(cx, |session, session_cx| {
        (
            session.title().read(session_cx).text().to_owned(),
            session.editor().read(session_cx).visible_text(),
        )
    });
    assert_eq!(title, "迁移 J2 最新标题");
    assert!(body.contains("迁移 J2 最新正文"));
    assert!(!body.contains("过期正文"));

    // This is deliberately before another input event. The old implementation
    // removed only J2 here, leaving J1/stale rows to become foreign owners.
    flush_until_clean(&restored, FlushReason::WindowClose, cx);
    let after_lifecycle: i64 = Connection::open(&path)
        .expect("inspect compacted migrated journal")
        .query_row(
            "SELECT count(*) FROM edit_journal WHERE note_id = ?1",
            [note.id.as_str()],
            |row| row.get(0),
        )
        .expect("count note journals");
    assert_eq!(
        after_lifecycle, 0,
        "no legacy foreign owner may survive J2's snapshot"
    );

    append_title_via_entity_input(&restored, "继续编辑", cx);
    append_body_via_entity_input(&restored, "并保存到第二次重启", cx);
    clock.advance(Duration::from_millis(100));
    poll_and_drain(&restored, cx);
    let continued = migrated
        .latest_edit_journal_for_revision(&note.id, Some(3))
        .expect("read current-base continued journal")
        .expect("continued input must journal after lifecycle compaction");
    assert!(continued.delta_utf8.contains("继续编辑"));
    assert!(continued.delta_utf8.contains("第二次重启"));
    clock.advance(Duration::from_millis(400));
    poll_and_drain(&restored, cx);
    assert!(
        migrated
            .latest_edit_journal(&note.id)
            .expect("read compacted continuation")
            .is_none(),
        "the fresh owner snapshots and compacts normally"
    );

    drop(restored);
    let restarted_note = migrated
        .load_note(&note.id)
        .expect("load continued note")
        .expect("continued note exists");
    assert_eq!(restarted_note.revision, 4);
    let restarted = session(restarted_note, migrated, clock, cx);
    let (restarted_title, restarted_body, semantic) =
        restarted.read_with(cx, |session, session_cx| {
            (
                session.title().read(session_cx).text().to_owned(),
                session.editor().read(session_cx).visible_text(),
                format!(
                    "{:?}",
                    session
                        .editor()
                        .read(session_cx)
                        .document()
                        .semantic_snapshot()
                ),
            )
        });
    assert_eq!(restarted_title, "迁移 J2 最新标题继续编辑");
    assert!(restarted_body.contains("迁移 J2 最新正文"));
    assert!(restarted_body.contains("第二次重启"));
    assert!(semantic.contains("Bold"));
}

#[test]
fn stale_timer_generation_cannot_snapshot_a_note_selected_after_its_session() {
    use super::save_coordinator::{SaveCoordinator, SaveWork};

    let clock = Arc::new(ManualSaveClock::default());
    let mut coordinator = SaveCoordinator::new(clock.clone());
    let first_generation = coordinator.mark_dirty();
    clock.advance(Duration::from_millis(100));
    let stale_work = SaveWork::Journal {
        generation: first_generation,
    };
    assert_eq!(coordinator.due_work(), Some(stale_work));

    let second_generation = coordinator.mark_dirty();
    assert!(second_generation > first_generation);
    assert!(
        !coordinator.begin(stale_work),
        "a captured timer may never begin work for a newer edit generation"
    );
}

#[gpui::test]
async fn stale_repository_snapshot_failure_enters_failed_once_without_timer_retry(
    cx: &mut gpui::TestAppContext,
) {
    // This uses a real second durable writer, not a repository mock. Removing
    // `SaveCoordinator::fail` makes the retained session remain Snapshotting
    // instead of truthfully blocking later timer work.
    let cx = cx.add_empty_window();
    let (_profile, repository) = repository();
    let note = create(&repository, "竞争", rich_document("原正文"));
    let clock = Arc::new(ManualSaveClock::default());
    let active = session(
        note.clone(),
        Arc::clone(&repository),
        Arc::clone(&clock),
        cx,
    );
    append_body_via_entity_input(&active, "本窗口", cx);
    clock.advance(Duration::from_millis(100));
    poll_and_drain(&active, cx);
    let active_ownership = repository
        .latest_edit_journal(&note.id)
        .expect("read active checkpoint")
        .expect("active checkpoint exists")
        .ownership();

    repository
        .flush_snapshot(
            app_lite_core::SaveNote {
                id: note.id.clone(),
                expected_revision: note.revision,
                title: note.title.clone(),
                document: rich_document("另一窗口"),
                resource_ids: Vec::new(),
                selected_thumbnail_id: None,
            },
            Some(active_ownership),
        )
        .expect("external committed revision");
    clock.advance(Duration::from_millis(400));
    poll_and_drain(&active, cx);
    let failed = active.read_with(cx, |session, _| session.save_state());
    assert!(
        matches!(failed, SaveState::Failed(ref message) if message.contains("stale note revision"))
    );

    clock.advance(Duration::from_secs(1));
    poll_and_drain(&active, cx);
    assert_eq!(
        active.read_with(cx, |session, _| session.save_state()),
        failed
    );
    assert_eq!(
        repository
            .load_note(&note.id)
            .expect("load")
            .expect("note")
            .body_text,
        "另一窗口"
    );
}

#[gpui::test]
async fn unsupported_codec_save_failure_enters_failed_without_writing_a_journal(
    cx: &mut gpui::TestAppContext,
) {
    // A nested list is a real native-editor command path that Task 4's
    // canonical codec intentionally rejects. The entity observer must expose
    // the failure rather than letting a later 50ms tick retry or persist a
    // lossy payload.
    use crate::native_editor::commands::{CommandArgument, CommandCatalogue, EditorCommand};

    let cx = cx.add_empty_window();
    let (_profile, repository) = repository();
    let note = create(&repository, "不支持", rich_document("正文"));
    let clock = Arc::new(ManualSaveClock::default());
    let active = session(
        note.clone(),
        Arc::clone(&repository),
        Arc::clone(&clock),
        cx,
    );
    let editor = active.read_with(cx, |session, _| session.editor().clone());
    editor.update(cx, |editor, editor_cx| {
        let commands = CommandCatalogue::new();
        commands
            .execute(EditorCommand::BulletList, CommandArgument::None, editor)
            .expect("make a list through the production command path");
        commands
            .execute(EditorCommand::IndentList, CommandArgument::None, editor)
            .expect("make an unsupported nested list");
        editor_cx.notify();
    });
    // Codec work runs at the scheduled save boundary, not from the input
    // observer itself. Drive that real boundary so this asserts the terminal
    // failure path rather than an implementation-detail timing shortcut.
    clock.advance(Duration::from_millis(100));
    poll_and_drain(&active, cx);
    let state = active.read_with(cx, |session, _| session.save_state());
    assert!(
        matches!(state, SaveState::Failed(ref message) if message.contains("尚未支持的嵌套级别"))
    );
    clock.advance(Duration::from_secs(20));
    poll_and_drain(&active, cx);
    assert_eq!(
        active.read_with(cx, |session, _| session.save_state()),
        state
    );
    assert!(repository.latest_edit_journal(&note.id).unwrap().is_none());
    assert_eq!(
        repository
            .load_note(&note.id)
            .unwrap()
            .expect("note")
            .revision,
        note.revision
    );
}

#[gpui::test]
async fn stale_background_codec_failure_cannot_poison_a_newer_fixed_generation(
    cx: &mut gpui::TestAppContext,
) {
    // Completion must recheck its generation. A worker that captured an
    // unsupported nested list can finish after the user has undone that one
    // change; it must not publish Failed over the new valid document.
    use crate::native_editor::commands::{CommandArgument, CommandCatalogue, EditorCommand};

    let cx = cx.add_empty_window();
    let (_profile, repository) = repository();
    let note = create(&repository, "代际", rich_document("正文"));
    let clock = Arc::new(ManualSaveClock::default());
    let active = session(
        note.clone(),
        Arc::clone(&repository),
        Arc::clone(&clock),
        cx,
    );
    let editor = active.read_with(cx, |session, _| session.editor().clone());
    editor.update(cx, |editor, editor_cx| {
        let commands = CommandCatalogue::new();
        commands
            .execute(EditorCommand::BulletList, CommandArgument::None, editor)
            .expect("list command");
        commands
            .execute(EditorCommand::IndentList, CommandArgument::None, editor)
            .expect("unsupported nested list");
        editor_cx.notify();
    });
    clock.advance(Duration::from_millis(100));
    active.update(cx, |session, session_cx| {
        session.dispatch_due_background_work_for_test(session_cx)
    });

    // This is a real user-visible correction while the old worker is queued.
    editor.update(cx, |editor, editor_cx| {
        editor.undo().expect("undo nested indent");
        editor_cx.notify();
    });
    cx.run_until_parked();
    assert!(matches!(
        active.read_with(cx, |session, _| session.save_state()),
        SaveState::Dirty
    ));

    clock.advance(Duration::from_millis(100));
    poll_and_drain(&active, cx);
    clock.advance(Duration::from_millis(400));
    poll_and_drain(&active, cx);
    assert_eq!(
        active.read_with(cx, |session, _| session.save_state()),
        SaveState::Clean
    );
    assert!(
        repository
            .load_note(&note.id)
            .unwrap()
            .expect("note")
            .body_html
            .contains("<ul>"),
        "the corrected non-nested list must be durable"
    );
}
