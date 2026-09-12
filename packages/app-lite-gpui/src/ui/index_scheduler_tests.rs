use super::*;
use crate::app::save_coordinator::ManualSaveClock;
use app_lite_core::{
    CanonicalDocument, CreateNote, LibraryError, LibraryRepository, SaveNote, SearchQuery,
};
use gpui::{TestAppContext, VisualTestContext};
use std::sync::Arc;

fn repository() -> (tempfile::TempDir, Arc<LibraryRepository>) {
    let profile = tempfile::tempdir().expect("temporary profile");
    let repository = Arc::new(
        LibraryRepository::open(profile.path().join("library.sqlite"))
            .expect("open temporary library"),
    );
    (profile, repository)
}

fn mount_shell<'a>(
    repository: Arc<LibraryRepository>,
    cx: &'a mut TestAppContext,
) -> (Entity<LibraryShell>, &'a mut VisualTestContext) {
    let model = cx.new(|_| AppModel::open(repository).expect("open model"));
    cx.add_window_view(move |window, cx| {
        LibraryShell::new_with_save_clock(
            model,
            None,
            Arc::new(ManualSaveClock::default()),
            window,
            cx,
        )
    })
}

fn redraw(cx: &mut VisualTestContext) {
    cx.update(|window, app| window.draw(app).clear());
    cx.run_until_parked();
}

#[gpui::test]
async fn mounted_open_drains_existing_search_work_without_opening_search(cx: &mut TestAppContext) {
    // This is deliberately a mounted library path: Stage B1 must not depend
    // on a Search view or a direct core-worker invocation to publish a note
    // that was already queued when the profile opened.
    let (_profile, repository) = repository();
    repository
        .create_note(CreateNote {
            title: "启动索引".into(),
            notebook_id: None,
            document: CanonicalDocument::default(),
        })
        .expect("queue note before opening shell");
    assert!(
        repository
            .search(SearchQuery::parse("启动索引"))
            .expect("search before worker")
            .is_empty()
    );

    let (_shell, cx) = mount_shell(Arc::clone(&repository), cx);
    redraw(cx);

    assert_eq!(
        repository
            .search(SearchQuery::parse("启动索引"))
            .expect("search after mounted scheduler")
            .len(),
        1,
        "opening the mounted library must retain a background index scheduler"
    );
}

#[gpui::test]
async fn mounted_scheduler_coalesces_latest_save_then_removes_trash_and_purge_hits(
    cx: &mut TestAppContext,
) {
    let (_profile, repository) = repository();
    let note = repository
        .create_note(CreateNote {
            title: "旧标题".into(),
            notebook_id: None,
            document: CanonicalDocument::default(),
        })
        .expect("create queued note");
    let (_shell, cx) = mount_shell(Arc::clone(&repository), cx);
    redraw(cx);

    let first = repository
        .save_note(SaveNote {
            id: note.id.clone(),
            expected_revision: note.revision,
            title: "中间标题".into(),
            document: CanonicalDocument::default(),
            resource_ids: vec![],
            selected_thumbnail_id: None,
        })
        .expect("first save");
    repository
        .save_note(SaveNote {
            id: note.id.clone(),
            expected_revision: first.revision,
            title: "最终标题".into(),
            document: CanonicalDocument::default(),
            resource_ids: vec![],
            selected_thumbnail_id: None,
        })
        .expect("rapid newer save");
    cx.executor()
        .advance_clock(std::time::Duration::from_millis(50));
    cx.run_until_parked();
    assert!(
        repository
            .search(SearchQuery::parse("中间标题"))
            .expect("search")
            .is_empty()
    );
    assert_eq!(
        repository
            .search(SearchQuery::parse("最终标题"))
            .expect("search")
            .len(),
        1
    );

    repository
        .trash_note(&note.id)
        .expect("trash queues delete projection");
    cx.executor()
        .advance_clock(std::time::Duration::from_millis(50));
    cx.run_until_parked();
    assert!(
        repository
            .search(SearchQuery::parse("最终标题"))
            .expect("search")
            .is_empty()
    );
    repository
        .purge_note(&note.id)
        .expect("purge queues durable delete");
    cx.executor()
        .advance_clock(std::time::Duration::from_millis(50));
    cx.run_until_parked();
    assert!(
        repository
            .take_search_jobs(1)
            .expect("queue readable")
            .is_empty()
    );
}

#[gpui::test]
async fn mounted_scheduler_failure_keeps_queue_and_never_sets_local_save_error(
    cx: &mut TestAppContext,
) {
    let (_profile, repository) = repository();
    repository
        .create_note(CreateNote {
            title: "失败仍可保存".into(),
            notebook_id: None,
            document: CanonicalDocument::default(),
        })
        .expect("create queued note");
    repository.fail_next_search_jobs_for_test(LibraryError::InvalidSnapshot);
    let (shell, cx) = mount_shell(Arc::clone(&repository), cx);
    redraw(cx);
    shell.read_with(cx, |shell, _| {
        assert!(matches!(
            shell.indexing_status_for_test(),
            IndexingStatus::Failed(_)
        ));
        assert_eq!(shell.save_error_for_test(), None);
    });
    assert_eq!(
        repository
            .take_search_jobs(100)
            .expect("durable queue")
            .len(),
        1
    );
    drop(shell);
    let (_reopened, cx) = mount_shell(Arc::clone(&repository), cx);
    redraw(cx);
    assert_eq!(
        repository
            .search(SearchQuery::parse("失败仍可保存"))
            .expect("retry after reopen")
            .len(),
        1
    );
}

#[gpui::test]
async fn mounted_scheduler_task_is_cancelled_with_the_shell_without_losing_durable_work(
    cx: &mut TestAppContext,
) {
    let (_profile, repository) = repository();
    let note = repository
        .create_note(CreateNote {
            title: "关闭后仍待索引".into(),
            notebook_id: None,
            document: CanonicalDocument::default(),
        })
        .expect("queue note");
    // Hold the one startup worker failure so the queue remains durable while
    // the mounted shell is destroyed; a later reopen owns the retry.
    repository.fail_next_search_jobs_for_test(LibraryError::InvalidSnapshot);
    let (view, cx) = mount_shell(Arc::clone(&repository), cx);
    redraw(cx);
    let weak_shell = view.downgrade();
    let cancelled = view.update(cx, |shell, _| {
        shell.take_indexing_task_cancellation_receiver_for_test()
    });
    cx.update(|window, _| window.remove_window());
    drop(view);
    assert!(weak_shell.upgrade().is_none());
    cx.cx.update(|_| {});
    cx.run_until_parked();
    cancelled
        .try_recv()
        .expect("destroying the shell cancels the retained index worker");
    assert_eq!(
        repository
            .take_search_jobs(100)
            .expect("queue survives quit")[0]
            .note_id,
        note.id
    );
}
