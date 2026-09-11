//! Mutation-sensitive Task 4 acceptance tests.
//!
//! These intentionally exercise the actual `EntityInputHandler` bridge rather
//! than calling a save helper with invented strings.  A native input change
//! must become a journal, then a canonical snapshot, and survive a fresh
//! session without flattening the document.

use super::note_session::NoteSession;
use super::save_coordinator::{FlushReason, ManualSaveClock, SaveState};
use app_lite_core::document::{Block, BlockStyle, Inline, Marks};
use app_lite_core::{CanonicalDocument, CreateNote, LibraryRepository, Note};
use gpui::{AppContext, EntityInputHandler};
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
    active.update(cx, |session, session_cx| {
        session
            .flush(FlushReason::ManualSync, session_cx)
            .expect("flush")
    });
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
    active.update(cx, |session, session_cx| {
        session.poll(session_cx).expect("poll")
    });
    assert!(
        repository
            .latest_edit_journal(&note.id)
            .expect("read journal")
            .is_none()
    );

    clock.advance(Duration::from_millis(1));
    active.update(cx, |session, session_cx| {
        session.poll(session_cx).expect("poll")
    });
    let journal = repository
        .latest_edit_journal(&note.id)
        .expect("read journal")
        .expect("100ms journal");
    assert!(journal.delta_utf8.contains("可恢复"));
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
    active.update(cx, |session, session_cx| {
        session.poll(session_cx).expect("poll")
    });
    assert_eq!(
        repository
            .load_note(&note.id)
            .expect("load")
            .expect("note")
            .revision,
        note.revision
    );

    clock.advance(Duration::from_millis(1));
    active.update(cx, |session, session_cx| {
        session.poll(session_cx).expect("poll")
    });
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
        active.update(cx, |session, session_cx| {
            session.poll(session_cx).expect("poll")
        });
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
    active.update(cx, |session, session_cx| {
        session.poll(session_cx).expect("poll")
    });
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
    active.update(cx, |session, session_cx| {
        session.poll(session_cx).expect("journal")
    });
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
        recovered.update(cx, |session, session_cx| {
            session.flush(reason, session_cx).expect("explicit flush")
        });
        assert!(
            repository
                .latest_edit_journal(&note.id)
                .expect("read journal")
                .is_none()
        );
    }
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
    active.update(cx, |session, session_cx| {
        session.poll(session_cx).expect("poll candidate")
    });
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
    active.update(cx, |session, session_cx| {
        session
            .flush(FlushReason::WindowClose, session_cx)
            .expect("flush committed IME text")
    });
    assert!(
        repository
            .load_note(&note.id)
            .expect("load")
            .expect("note")
            .body_html
            .contains("候选")
    );
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
