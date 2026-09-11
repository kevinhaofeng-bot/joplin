//! Mutation-sensitive Task 4 acceptance tests.
//!
//! These intentionally exercise the actual `EntityInputHandler` bridge rather
//! than calling a save helper with invented strings.  A native input change
//! must become a journal, then a canonical snapshot, and survive a fresh
//! session without flattening the document.

use super::note_session::NoteSession;
use super::save_coordinator::{FlushReason, ManualSaveClock, SaveState};
use app_lite_core::document::{Block, BlockStyle, Inline, Marks};
use app_lite_core::{
    CanonicalDocument, CreateNote, EditJournalEntry, LibraryError, LibraryRepository, Note,
    SaveNote,
};
use gpui::{AppContext, EntityInputHandler};
use rusqlite::Connection;
use std::sync::Arc;
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
