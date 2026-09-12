use super::*;
use crate::app::AppAction;
use crate::app::save_coordinator::ManualSaveClock;
use app_lite_core::document::Block;
use app_lite_core::{
    CanonicalDocument, CreateNote, DerivedTextFailure, DerivedTextStatus, LibraryError,
    LibraryRepository, SaveNote, SearchIndexTestPhase, SearchQuery,
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
async fn mounted_scheduler_advances_a_derived_job_saved_after_open(cx: &mut TestAppContext) {
    let (_profile, repository) = repository();
    let note = repository
        .create_note(CreateNote {
            title: "PDF owner".into(),
            notebook_id: None,
            document: CanonicalDocument::default(),
        })
        .expect("create ordinary note");
    let resource = repository
        .import_resource(b"future vision source", "fixture.png", "image/png", "png")
        .expect("import selectable PDF fixture");
    let (_shell, cx) = mount_shell(Arc::clone(&repository), cx);
    redraw(cx);
    assert!(
        repository
            .take_derived_text_jobs(1)
            .expect("derived job queued")
            .len()
            == 0,
        "a stand-alone resource is not associated yet"
    );

    repository
        .save_note(SaveNote {
            id: note.id,
            expected_revision: note.revision,
            title: note.title,
            document: CanonicalDocument::from_blocks(vec![Block::Attachment {
                resource_id: resource.clone(),
                filename: "fixture.png".into(),
                media_type: "image/png".into(),
            }]),
            resource_ids: vec![resource.clone()],
            selected_thumbnail_id: None,
        })
        .expect("ordinary save associates fixture");
    cx.executor()
        .advance_clock(std::time::Duration::from_millis(50));
    redraw(cx);

    assert_eq!(
        repository
            .derived_text_status(&resource)
            .expect("derived status"),
        Some(DerivedTextStatus::Failed {
            failure: DerivedTextFailure::Unsupported,
            attempts: 1,
        }),
        "the mounted scheduler consumes the durable job without any foreground save path"
    );
}

#[gpui::test]
async fn mounted_derived_text_publish_refreshes_an_active_search_route(cx: &mut TestAppContext) {
    // This injects D3a output directly to exercise the mounted event-to-search
    // refresh seam. It is deliberately not a claim about the PDFKit child.
    let (_profile, repository) = repository();
    let resource = repository
        .import_resource(b"opaque source", "fixture.png", "image/png", "png")
        .expect("import opaque source");
    let note = repository
        .create_note(CreateNote {
            title: "derived search owner".into(),
            notebook_id: None,
            document: CanonicalDocument::from_blocks(vec![Block::Attachment {
                resource_id: resource.clone(),
                filename: "fixture.png".into(),
                media_type: "image/png".into(),
            }]),
        })
        .expect("create ordinary note");
    repository
        .process_search_jobs()
        .expect("finish ordinary title/body search work before opening shell");
    let job = repository
        .take_derived_text_jobs(1)
        .expect("take associated derived identity")
        .pop()
        .expect("derived job pending");
    let model = cx.new(|_| {
        let mut model = AppModel::open(Arc::clone(&repository)).expect("open model");
        let generation = model.begin_search("derived-only-needle");
        model
            .commit_search_results(generation, "derived-only-needle".into(), vec![], None)
            .expect("commit initially empty SearchRoute");
        model
    });
    // Hold the process-wide child gate until the synthetic D3a publish has
    // emitted. Otherwise the startup worker can consume this intentionally
    // pending image job before the mounted event path observes it.
    let derived_worker_guard = super::DERIVED_TEXT_WORKER_LOCK
        .lock()
        .expect("derived worker gate");
    let (view, cx) = cx.add_window_view(move |window, cx| {
        LibraryShell::new_with_save_clock(
            model,
            None,
            Arc::new(ManualSaveClock::default()),
            window,
            cx,
        )
    });
    let (history_before, snapshot_before) = view.read_with(cx, |shell, app| {
        let navigation = shell.model.read(app).navigation();
        (navigation.history_len_for_test(), navigation.snapshot())
    });
    assert!(
        repository
            .publish_derived_text(&job, "derived-only-needle")
            .expect("publish synthetic derived text")
    );
    drop(derived_worker_guard);
    assert_eq!(
        repository
            .search(SearchQuery::parse("derived-only-needle"))
            .expect("derived text reaches FTS")
            .len(),
        1
    );

    cx.executor()
        .advance_clock(std::time::Duration::from_millis(300));
    redraw(cx);

    view.read_with(cx, |shell, app| {
        let model = shell.model.read(app);
        assert_eq!(
            model.projections().len(),
            1,
            "active route receives derived hit"
        );
        assert_eq!(model.projections()[0].id, note.id);
        assert_eq!(
            model.navigation().history_len_for_test(),
            history_before,
            "derived refresh must not create history"
        );
        assert_eq!(
            model.navigation().snapshot(),
            snapshot_before,
            "derived refresh keeps the active typed route unchanged"
        );
    });
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

#[gpui::test]
async fn mounted_indexing_pending_and_failure_are_visible_without_covering_resource_notice(
    cx: &mut TestAppContext,
) {
    let (_profile, repository) = repository();
    repository
        .create_note(CreateNote {
            title: "状态提示".into(),
            notebook_id: None,
            document: CanonicalDocument::default(),
        })
        .expect("queue note");
    repository.fail_next_search_jobs_for_test(LibraryError::InvalidSnapshot);
    let (shell, cx) = mount_shell(repository, cx);

    // The first actual mounted draw sees the honest startup Pending state,
    // before the background worker returns its failure outcome.
    cx.update(|window, app| window.draw(app).clear());
    assert!(cx.debug_bounds("library-indexing-status").is_some());
    cx.run_until_parked();
    shell.update(cx, |shell, shell_cx| {
        shell.set_resource_notice_for_test("资源导入提示");
        shell_cx.notify();
    });
    cx.update(|window, app| window.draw(app).clear());
    let indexing = cx
        .debug_bounds("library-indexing-status")
        .expect("failed index status is visible");
    let resource = cx
        .debug_bounds("library-resource-notice")
        .expect("resource notice remains visible");
    assert!(
        resource.bottom() < indexing.top(),
        "separate bottom slots must not overlap"
    );
}

#[gpui::test]
async fn mounted_background_index_worker_does_not_block_note_switch_or_shell_teardown(
    cx: &mut TestAppContext,
) {
    let (_profile, repository) = repository();
    let first = repository
        .create_note(CreateNote {
            title: "索引工作中 A".into(),
            notebook_id: None,
            document: CanonicalDocument::default(),
        })
        .expect("first queued note");
    let second = repository
        .create_note(CreateNote {
            title: "索引工作中 B".into(),
            notebook_id: None,
            document: CanonicalDocument::default(),
        })
        .expect("second queued note");
    let (started_sender, started) = std::sync::mpsc::channel();
    let (release, release_receiver) = futures::channel::oneshot::channel();
    install_indexing_worker_gate_for_test(IndexingWorkerGate {
        started: started_sender,
        release: release_receiver,
    });
    let (view, cx) = mount_shell(Arc::clone(&repository), cx);
    cx.run_until_parked();
    started
        .try_recv()
        .expect("the scheduler batch is now waiting on the background executor");

    // This route executes on the mounted GPUI shell while the worker is
    // deliberately parked. The old foreground implementation could not reach
    // this action until its synchronous 100-note batch returned.
    cx.update(|window, app| {
        view.update(app, |shell, shell_cx| {
            shell.apply_action(AppAction::SelectNote(second.id.clone()), window, shell_cx);
        });
    });
    assert_eq!(
        view.read_with(cx, |shell, _| shell.surface_note_id.clone()),
        Some(second.id.clone())
    );

    let cancelled = view.update(cx, |shell, _| {
        shell.take_indexing_task_cancellation_receiver_for_test()
    });
    cx.update(|window, _| window.remove_window());
    drop(view);
    cx.cx.update(|_| {});
    cx.run_until_parked();
    cancelled
        .try_recv()
        .expect("teardown cancels the retained foreground waiter");
    assert!(
        repository
            .take_search_jobs(100)
            .expect("durable queue after cancelled worker")
            .iter()
            .any(|job| job.note_id == first.id),
        "cancelling before the barrier releases cannot acknowledge queued work"
    );
    drop(release);
}

#[gpui::test]
async fn mounted_note_switch_and_shell_close_survive_an_active_index_transaction(
    cx: &mut TestAppContext,
) {
    let (_profile, repository) = repository();
    let first = repository
        .create_note(CreateNote {
            title: "事务中的索引 A".into(),
            notebook_id: None,
            document: CanonicalDocument::default(),
        })
        .expect("first queued note");
    let second = repository
        .create_note(CreateNote {
            title: "前台选择 B".into(),
            notebook_id: None,
            document: CanonicalDocument::default(),
        })
        .expect("second queued note");
    // Let the mounted scheduler consume a controlled retryable failure first,
    // so this test owns the following real transaction instead of racing its
    // startup batch.
    repository.fail_next_search_jobs_for_test(LibraryError::InvalidSnapshot);
    let (view, cx) = mount_shell(Arc::clone(&repository), cx);
    redraw(cx);
    let (entered_sender, entered) = std::sync::mpsc::channel();
    let (release_sender, release) = std::sync::mpsc::channel();
    let release = Arc::new(std::sync::Mutex::new(release));
    let first_transaction = Arc::new(std::sync::atomic::AtomicBool::new(true));
    repository.set_search_index_transaction_hook_for_test(Arc::new(move |phase| {
        if phase != SearchIndexTestPhase::TransactionStarted
            || !first_transaction.swap(false, std::sync::atomic::Ordering::AcqRel)
        {
            return;
        }
        entered_sender
            .send(())
            .expect("signal active index transaction");
        release
            .lock()
            .expect("release mutex")
            .recv()
            .expect("release active index transaction");
    }));
    let worker_repository = Arc::clone(&repository);
    let worker = std::thread::spawn(move || worker_repository.process_search_jobs());
    entered
        .recv_timeout(std::time::Duration::from_secs(1))
        .expect("index transaction is active");

    cx.update(|window, app| {
        view.update(app, |shell, shell_cx| {
            shell.apply_action(AppAction::SelectNote(second.id.clone()), window, shell_cx);
        });
    });
    assert_eq!(
        view.read_with(cx, |shell, _| shell.surface_note_id.clone()),
        Some(second.id.clone()),
        "mounted selection uses the foreground connection while WAL indexing is active"
    );
    assert_eq!(
        view.read_with(cx, |shell, _| shell.save_error_for_test()),
        None,
        "a retryable indexing failure remains distinct from local-save status"
    );

    cx.update(|window, _| window.remove_window());
    drop(view);
    assert!(
        repository
            .take_search_jobs(100)
            .expect("queue identity remains readable after shell close")
            .iter()
            .any(|job| job.note_id == first.id),
        "closing the shell cannot acknowledge an in-flight queue identity"
    );
    release_sender.send(()).expect("release worker");
    assert_eq!(
        worker
            .join()
            .expect("worker thread")
            .expect("worker result"),
        2
    );
}

#[gpui::test]
async fn mounted_scheduler_close_finishes_only_active_index_transaction(cx: &mut TestAppContext) {
    let (_profile, repository) = repository();
    let first = repository
        .create_note(CreateNote {
            title: "当前原子索引".into(),
            notebook_id: None,
            document: CanonicalDocument::default(),
        })
        .expect("first queued note");
    let second = repository
        .create_note(CreateNote {
            title: "关窗后不得继续扫描".into(),
            notebook_id: None,
            document: CanonicalDocument::default(),
        })
        .expect("second queued note");
    let third = repository
        .create_note(CreateNote {
            title: "保留队列身份".into(),
            notebook_id: None,
            document: CanonicalDocument::default(),
        })
        .expect("third queued note");
    let queued_before_close = repository
        .take_search_jobs(100)
        .expect("capture deterministic queue order");
    let active_id = queued_before_close[0].note_id.clone();
    let remaining_ids = queued_before_close[1..]
        .iter()
        .map(|job| job.note_id.clone())
        .collect::<std::collections::BTreeSet<_>>();
    let (started_sender, started) = std::sync::mpsc::channel();
    let (release_sender, release) = std::sync::mpsc::channel();
    let (committed_sender, committed) = std::sync::mpsc::channel();
    let release = Arc::new(std::sync::Mutex::new(release));
    repository.set_search_index_transaction_hook_for_test(Arc::new(move |phase| match phase {
        SearchIndexTestPhase::TransactionStarted => {
            started_sender.send(()).expect("signal active transaction");
            release
                .lock()
                .expect("release mutex")
                .recv()
                .expect("release active transaction");
        }
        SearchIndexTestPhase::TransactionCommitted => {
            committed_sender
                .send(())
                .expect("signal committed transaction");
        }
    }));
    run_indexing_worker_on_thread_for_test();
    let (view, cx) = mount_shell(Arc::clone(&repository), cx);
    cx.run_until_parked();
    started
        .recv_timeout(std::time::Duration::from_secs(1))
        .expect("the shell scheduler entered BEGIN IMMEDIATE");

    let cancelled = view.update(cx, |shell, _| {
        shell.take_indexing_task_cancellation_receiver_for_test()
    });
    cx.update(|window, _| window.remove_window());
    drop(view);
    cx.cx.update(|_| {});
    cx.run_until_parked();
    cancelled
        .try_recv()
        .expect("closing the shell cancels its retained scheduler task");

    release_sender
        .send(())
        .expect("allow the active atomic transaction to finish");
    committed
        .recv_timeout(std::time::Duration::from_secs(1))
        .expect("the current transaction committed exactly once");
    let remaining = repository
        .take_search_jobs(100)
        .expect("remaining queue readable after scheduler close");
    assert_eq!(
        remaining.len(),
        2,
        "no later batch item may start after close"
    );
    assert_eq!(
        remaining
            .iter()
            .map(|job| job.note_id.clone())
            .collect::<std::collections::BTreeSet<_>>(),
        remaining_ids,
        "the exact later queue identities remain durable"
    );
    assert!(
        !remaining.iter().any(|job| job.note_id == active_id),
        "the active atomic note is acknowledged only after its committed projection"
    );
}
