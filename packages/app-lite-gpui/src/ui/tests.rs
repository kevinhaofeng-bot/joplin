use super::*;
use crate::app::AppAction;
use crate::app::save_coordinator::ManualSaveClock;
use crate::components::{Copy, SelectAll};
use app_lite_core::document::{Block, BlockStyle, Inline, Marks};
use app_lite_core::{CanonicalDocument, CreateNote, LibraryShellState};
use gpui::{AppContext, EntityInputHandler, Modifiers, TestAppContext, VisualTestContext};
use std::sync::Arc;
use std::sync::mpsc::TryRecvError;
use std::time::Duration;

fn redraw(cx: &mut VisualTestContext) {
    cx.update(|window, app| window.draw(app).clear());
    cx.run_until_parked();
}

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
            marks: Default::default(),
        }],
    }])
}

fn mount_shell<'a>(
    repository: Arc<LibraryRepository>,
    cx: &'a mut TestAppContext,
) -> (Entity<LibraryShell>, &'a mut VisualTestContext) {
    let model_state = AppModel::open(repository).expect("open model");
    let model = cx.new(|_| model_state);
    cx.add_window_view(move |window, cx| LibraryShell::new(model, None, window, cx))
}

fn mount_shell_with_save_clock<'a>(
    repository: Arc<LibraryRepository>,
    clock: Arc<ManualSaveClock>,
    cx: &'a mut TestAppContext,
) -> (Entity<LibraryShell>, &'a mut VisualTestContext) {
    let model_state = AppModel::open(repository).expect("open model");
    let model = cx.new(|_| model_state);
    cx.add_window_view(move |window, cx| {
        LibraryShell::new_with_save_clock(model, None, clock, window, cx)
    })
}

#[gpui::test]
async fn mounted_empty_cta_uses_the_shell_action_reducer(cx: &mut TestAppContext) {
    let (_profile, repository) = repository();
    let (view, cx) = mount_shell(Arc::clone(&repository), cx);
    redraw(cx);

    let create = cx
        .debug_bounds("create-first-note")
        .expect("mounted empty-library CTA");
    cx.simulate_click(create.center(), Modifiers::default());
    redraw(cx);
    view.read_with(cx, |view, cx| {
        assert_eq!(view.model.read(cx).projections().len(), 1);
        assert!(view.model.read(cx).active_note().is_some());
        assert!(view.editor_surface.is_some());
    });
}

#[gpui::test]
async fn mounted_keyboard_actions_use_the_same_shell_reducer(cx: &mut TestAppContext) {
    cx.update(bind_library_keybindings);
    let (_profile, repository) = repository();
    let (view, cx) = mount_shell(repository, cx);
    redraw(cx);

    cx.simulate_keystrokes("cmd-n");
    redraw(cx);
    view.read_with(cx, |view, cx| {
        assert_eq!(view.model.read(cx).projections().len(), 1);
        assert!(view.model.read(cx).active_note().is_some());
    });

    for key in ["cmd-alt-s", "cmd-alt-l", "cmd-alt-v", "cmd-alt-o"] {
        cx.simulate_keystrokes(key);
        redraw(cx);
    }
    view.read_with(cx, |view, cx| {
        let model = view.model.read(cx);
        assert!(!model.panes().sidebar_visible);
        assert!(!model.panes().list_visible);
        assert_eq!(model.list_view_mode(), ListViewMode::Snippets);
        assert_eq!(model.sort(), crate::app::NoteSort::TitleAscending);
    });

    cx.simulate_keystrokes("cmd-shift-backspace");
    redraw(cx);
    view.read_with(cx, |view, cx| {
        assert!(view.model.read(cx).projections().is_empty());
        assert_eq!(view.model.read(cx).status(), &AppStatus::Ready);
    });
}

#[gpui::test]
async fn mounted_native_menu_actions_use_the_same_shell_reducer(cx: &mut TestAppContext) {
    let (_profile, repository) = repository();
    let (view, cx) = mount_shell(repository, cx);
    redraw(cx);

    // Native menus dispatch the action object directly, rather than a mouse
    // event or key binding. The focused LibraryShell must still receive it.
    cx.dispatch_action(crate::app::CreateNote);
    redraw(cx);
    view.read_with(cx, |view, cx| {
        assert_eq!(view.model.read(cx).projections().len(), 1);
        assert!(view.model.read(cx).active_note().is_some());
    });

    cx.dispatch_action(crate::app::TrashSelected);
    redraw(cx);
    view.read_with(cx, |view, cx| {
        assert!(view.model.read(cx).projections().is_empty());
        assert_eq!(view.model.read(cx).status(), &AppStatus::Ready);
    });
}

#[gpui::test]
async fn mounted_card_click_reaches_the_same_shell_action_reducer(cx: &mut TestAppContext) {
    let (_profile, repository) = repository();
    let stored = repository
        .create_note(CreateNote {
            title: "可点击卡片".into(),
            notebook_id: None,
            document: rich_document("卡片点击后的正文"),
        })
        .expect("create note");
    let (view, cx) = mount_shell(repository, cx);
    redraw(cx);
    let card = cx
        .debug_bounds("library-note-card")
        .expect("mounted virtual-list card");
    cx.simulate_click(card.center(), Modifiers::default());
    redraw(cx);
    view.read_with(cx, |view, cx| {
        assert_eq!(
            view.model.read(cx).navigation().selected_note_id(),
            Some(&stored.id)
        );
        assert!(view.editor_surface.is_some());
    });
}

#[gpui::test]
async fn mounted_toolbar_actions_notify_the_retained_shell_and_render_failures(
    cx: &mut TestAppContext,
) {
    let (_profile, repository) = repository();
    repository
        .create_note(CreateNote {
            title: "Zulu".into(),
            notebook_id: None,
            document: rich_document("first"),
        })
        .expect("first note");
    repository
        .create_note(CreateNote {
            title: "Alpha".into(),
            notebook_id: None,
            document: rich_document("second"),
        })
        .expect("second note");
    let (view, cx) = mount_shell(repository, cx);
    redraw(cx);

    let card = cx
        .debug_bounds("library-note-card")
        .expect("mounted note card");
    cx.simulate_click(card.center(), Modifiers::default());
    redraw(cx);

    for selector in ["library-cycle-view", "library-cycle-sort"] {
        let control = cx.debug_bounds(selector).expect("toolbar control");
        cx.simulate_click(control.center(), Modifiers::default());
        redraw(cx);
    }
    view.read_with(cx, |view, cx| {
        let model = view.model.read(cx);
        assert_eq!(model.list_view_mode(), ListViewMode::Snippets);
        assert_eq!(model.sort(), crate::app::NoteSort::TitleAscending);
        assert_eq!(model.projections()[0].title_prefix, "Alpha");
    });

    for selector in ["library-toggle-sidebar", "library-toggle-list"] {
        let control = cx.debug_bounds(selector).expect("toggle control");
        cx.simulate_click(control.center(), Modifiers::default());
        redraw(cx);
    }
    assert_eq!(
        f32::from(
            cx.debug_bounds("library-sidebar")
                .expect("hidden sidebar")
                .size
                .width,
        ),
        0.0
    );
    assert_eq!(
        f32::from(
            cx.debug_bounds("library-note-list")
                .expect("hidden list")
                .size
                .width,
        ),
        0.0
    );

    let trash = cx
        .debug_bounds("library-trash-selected")
        .expect("trash control");
    cx.simulate_click(trash.center(), Modifiers::default());
    redraw(cx);
    assert_eq!(
        view.read_with(cx, |view, cx| view.model.read(cx).projections().len()),
        1
    );

    let trash = cx
        .debug_bounds("library-trash-selected")
        .expect("trash control remains mounted");
    cx.simulate_click(trash.center(), Modifiers::default());
    redraw(cx);
    assert_eq!(
        view.read_with(cx, |view, cx| view.model.read(cx).projections().len()),
        0
    );

    let trash = cx
        .debug_bounds("library-trash-selected")
        .expect("trash control remains mounted for empty-state error");
    cx.simulate_click(trash.center(), Modifiers::default());
    redraw(cx);
    assert!(
        cx.debug_bounds("library-action-error").is_some(),
        "a reducer error must be visible in the mounted library shell"
    );
}

#[gpui::test]
async fn mounted_open_request_notice_is_visible_without_opening_an_old_editor(
    cx: &mut TestAppContext,
) {
    let (_profile, repository) = repository();
    let model = cx.new(|_| AppModel::open(repository).expect("open model"));
    let (_view, cx) = cx.add_window_view(move |window, cx| {
        LibraryShell::new(
            model,
            Some("暂不支持导入 /tmp/incoming.md；原文件未被读取或修改。".into()),
            window,
            cx,
        )
    });
    redraw(cx);
    assert!(cx.debug_bounds("library-startup-notice").is_some());
    assert!(cx.debug_bounds("native-editor-surface").is_none());
}

#[gpui::test]
async fn mounted_startup_error_is_visible_when_no_library_model_can_be_opened(
    cx: &mut TestAppContext,
) {
    let (_view, cx) = cx.add_window_view(|_window, _cx| StartupErrorView {
        message: "无法打开受保护的资料库目录".into(),
    });
    redraw(cx);
    assert!(
        cx.debug_bounds("library-startup-error").is_some(),
        "fallible bootstrap failures must be rendered in a window"
    );
}

#[gpui::test]
async fn persisted_pane_widths_and_visibility_control_real_rendered_bounds(
    cx: &mut TestAppContext,
) {
    let (_profile, repository) = repository();
    repository
        .write_library_shell_state(&LibraryShellState {
            sidebar_width: 260,
            list_width: 410,
            sidebar_visible: true,
            list_visible: true,
            selected_note_id: None,
        })
        .expect("persist panes");
    let (view, cx) = mount_shell(Arc::clone(&repository), cx);
    redraw(cx);
    let sidebar = cx.debug_bounds("library-sidebar").expect("sidebar bounds");
    let list = cx
        .debug_bounds("library-note-list")
        .expect("note-list bounds");
    assert_eq!(f32::from(sidebar.size.width), 260.0);
    assert_eq!(f32::from(list.size.width), 410.0);

    cx.update(|window, app| {
        view.update(app, |view, view_cx| {
            view.apply_action(AppAction::ToggleSidebar, window, view_cx);
            view.apply_action(AppAction::ToggleNoteList, window, view_cx);
        });
    });
    redraw(cx);
    let sidebar = cx.debug_bounds("library-sidebar").expect("sidebar bounds");
    assert_eq!(f32::from(sidebar.size.width), 0.0);
    let list = cx
        .debug_bounds("library-note-list")
        .expect("hidden note-list bounds");
    assert_eq!(f32::from(list.size.width), 0.0);
}

#[gpui::test]
async fn event_bridge_coalesces_external_projection_changes_without_loading_bodies(
    cx: &mut TestAppContext,
) {
    let (_profile, repository) = repository();
    let (view, cx) = mount_shell(Arc::clone(&repository), cx);
    redraw(cx);
    let ids = (0..3)
        .map(|index| {
            repository
                .create_note(CreateNote {
                    title: format!("外部创建 {index}"),
                    notebook_id: None,
                    document: rich_document("事件桥不应把这段正文当作列表字段读取"),
                })
                .expect("external create")
                .id
        })
        .collect::<Vec<_>>();
    let list_query = repository.observe_next_list_query();
    let body_loads = repository.observe_note_loads();
    cx.executor().advance_clock(Duration::from_millis(60));
    cx.run_until_parked();
    redraw(cx);
    view.read_with(cx, |view, cx| {
        let model = view.model.read(cx);
        assert_eq!(model.projections().len(), 3);
        assert!(
            model.active_note().is_none(),
            "event refresh must not hydrate a body"
        );
        assert_eq!(
            model.projection_event_refreshes_for_test(),
            1,
            "a burst must make one projection refresh, not one per event"
        );
    });
    let fields = list_query.recv().expect("one coalesced projection query");
    assert!(!fields.iter().any(|field| {
        matches!(
            field.as_str(),
            "notes.body_html" | "notes.body_text" | "notes.merge_state"
        )
    }));
    assert_eq!(body_loads.try_recv(), Err(TryRecvError::Empty));

    for id in &ids {
        repository.trash_note(id).expect("external trash");
    }
    cx.executor().advance_clock(Duration::from_millis(60));
    cx.run_until_parked();
    redraw(cx);
    view.read_with(cx, |view, cx| {
        let model = view.model.read(cx);
        assert!(model.projections().is_empty());
        assert_eq!(
            model.projection_event_refreshes_for_test(),
            2,
            "the trash burst must also refresh once"
        );
    });
    assert_eq!(body_loads.try_recv(), Err(TryRecvError::Empty));
}

#[gpui::test]
async fn queued_action_event_cannot_clear_partial_create_error_before_explicit_selection_recovery(
    cx: &mut TestAppContext,
) {
    // Catches refresh_projection_events unconditionally setting Ready after
    // the create action committed but its own refresh failed. The queued
    // repository event may repair projections, but it never retried the
    // action's selection/persistence phase; a real card selection must be
    // the explicit recovery that clears the warning.
    let (_profile, repository) = repository();
    let (view, cx) = mount_shell(Arc::clone(&repository), cx);
    redraw(cx);
    view.update(cx, |view, view_cx| {
        view.model.update(view_cx, |model, _| {
            model.fail_next_refresh_for_test(app_lite_core::LibraryError::NotFound);
        });
    });

    let create = cx
        .debug_bounds("create-first-note")
        .expect("empty-library create CTA");
    cx.simulate_click(create.center(), Modifiers::default());
    redraw(cx);
    assert!(
        cx.debug_bounds("library-action-error").is_some(),
        "the action's committed-create warning must first be visible"
    );

    let created = repository
        .list_notes(Default::default())
        .expect("read committed create")
        .into_iter()
        .next()
        .expect("one committed projection")
        .id;
    cx.executor().advance_clock(Duration::from_millis(60));
    cx.run_until_parked();
    redraw(cx);
    assert!(
        cx.debug_bounds("library-action-error").is_some(),
        "a queued projection event must not erase an unresolved action warning"
    );
    view.read_with(cx, |view, cx| {
        assert!(matches!(
            view.model.read(cx).status(),
            AppStatus::Error(message) if message.contains("笔记已创建")
                && message.contains("资料库数据已提交")
        ));
        assert_eq!(view.model.read(cx).projections().len(), 1);
        assert_eq!(view.model.read(cx).projection_event_refreshes_for_test(), 1);
    });

    let card = cx
        .debug_bounds("library-note-card")
        .expect("event refresh exposes the committed card for recovery");
    cx.simulate_click(card.center(), Modifiers::default());
    redraw(cx);
    view.read_with(cx, |view, cx| {
        assert_eq!(view.model.read(cx).status(), &AppStatus::Ready);
        assert_eq!(
            view.model.read(cx).navigation().selected_note_id(),
            Some(&created)
        );
    });
}

#[gpui::test]
async fn event_bridge_task_is_cancelled_immediately_when_the_shell_is_destroyed(
    cx: &mut TestAppContext,
) {
    let (_profile, repository) = repository();
    let (view, cx) = mount_shell(repository, cx);
    redraw(cx);
    let weak_shell = view.downgrade();
    let task_cancelled = view.update(cx, |view, _| {
        view.take_event_task_cancellation_receiver_for_test()
    });

    // Removing the window drops its root. Releasing our final Entity handle
    // queues GPUI's entity-release pass; do not advance the 50ms poll timer,
    // because delayed weak-update exit is not cancellation.
    cx.update(|window, _| window.remove_window());
    drop(view);
    assert!(
        weak_shell.upgrade().is_none(),
        "the window must release its shell"
    );
    // GPUI releases zero-count entities at the next app effect boundary, then
    // async-task schedules one final cancellation runnable. Drain both without
    // advancing the polling clock.
    cx.cx.update(|_| {});
    cx.run_until_parked();
    task_cancelled
        .try_recv()
        .expect("destroying the shell must cancel the retained event task immediately");
}

#[gpui::test]
async fn rich_body_mounts_the_native_canvas_and_never_uses_body_text_fallback(
    cx: &mut TestAppContext,
) {
    let (_profile, repository) = repository();
    let stored = repository
        .create_note(CreateNote {
            title: "富文本".into(),
            notebook_id: None,
            document: rich_document("来自 HTML 的粗体正文"),
        })
        .expect("create rich note");
    let mut forged = stored.clone();
    forged.body_text = "绝不能用作编辑器输入的 sentinel".into();
    let imported = import_note_body(&forged).expect("body_html importer");
    let probe = cx.new(|cx| EditorCore::new_read_only(imported, cx));
    let probe_text = cx.update(|app| probe.read(app).copy_all_plain_text());
    assert!(probe_text.contains("来自 HTML 的粗体正文"));
    assert!(!probe_text.contains("sentinel"));

    let (view, cx) = mount_shell(Arc::clone(&repository), cx);
    cx.update(|window, app| {
        view.update(app, |view, view_cx| {
            view.apply_action(AppAction::SelectNote(stored.id.clone()), window, view_cx)
        });
    });
    redraw(cx);
    assert!(cx.debug_bounds("native-editor-surface").is_some());
    view.read_with(cx, |view, cx| {
        let surface = view.editor_surface.as_ref().expect("mounted surface");
        assert_eq!(surface.read(cx).mode(), EditorSurfaceMode::Editable);
        assert!(
            surface
                .read(cx)
                .editor()
                .read(cx)
                .copy_all_plain_text()
                .contains("来自 HTML 的粗体正文")
        );
    });
}

#[gpui::test]
async fn library_canvas_shapes_and_executes_the_shared_paint_entity_path(cx: &mut TestAppContext) {
    // Catches a library route that mounts the right surface chrome but skips
    // either donor shaping or render::paint_entity inside its canvas callback.
    let (_profile, repository) = repository();
    let note = repository
        .create_note(CreateNote {
            title: "真实画布".into(),
            notebook_id: None,
            document: rich_document("必须由共享画布 shape 和 paint"),
        })
        .expect("create canvas fixture");
    let (view, cx) = mount_shell(repository, cx);
    redraw(cx);
    cx.update(|window, app| {
        view.update(app, |view, view_cx| {
            view.apply_action(AppAction::SelectNote(note.id.clone()), window, view_cx)
        });
    });
    crate::native_editor::render::reset_test_render_observations();
    redraw(cx);

    let editor = view.read_with(cx, |view, cx| {
        view.editor_surface
            .as_ref()
            .expect("selection mounts library surface")
            .read(cx)
            .editor()
            .clone()
    });
    let hook_counts = view.read_with(cx, |view, _| {
        (
            view.library_surface_paint_hooks_for_test
                .shape
                .load(std::sync::atomic::Ordering::Relaxed),
            view.library_surface_paint_hooks_for_test
                .paint
                .load(std::sync::atomic::Ordering::Relaxed),
        )
    });
    let shape_calls = cx.update(|_, app| editor.read(app).shape_calls_for_test());
    assert!(
        hook_counts.0 > 0,
        "library surface before-shape hook must run"
    );
    assert!(
        hook_counts.1 > 0,
        "library surface after-paint hook must run"
    );
    assert!(
        shape_calls > 0,
        "library canvas must shape the selected document"
    );
    assert!(
        crate::native_editor::render::test_paint_entity_calls() > 0,
        "library canvas must invoke the shared paint_entity path"
    );
}

#[gpui::test]
async fn mounted_editable_session_uses_real_title_and_body_input_then_persists(
    cx: &mut TestAppContext,
) {
    cx.update(|app| crate::components::init(app));
    let (_profile, repository) = repository();
    let stored = repository
        .create_note(CreateNote {
            title: "可编辑笔记".into(),
            notebook_id: None,
            document: CanonicalDocument::from_blocks(vec![Block::Paragraph {
                style: BlockStyle::default(),
                inlines: vec![Inline::Text {
                    text: "保留富文本样式".into(),
                    marks: Marks {
                        bold: true,
                        italic: true,
                        ..Marks::default()
                    },
                }],
            }]),
        })
        .expect("create note");
    let (view, cx) = mount_shell(Arc::clone(&repository), cx);
    redraw(cx);

    let card = cx
        .debug_bounds("library-note-card")
        .expect("mounted virtual-list card");
    cx.simulate_click(card.center(), Modifiers::default());
    redraw(cx);

    let title = cx
        .debug_bounds("library-note-title")
        .expect("mounted title EntityInputHandler canvas");
    cx.simulate_click(title.center(), Modifiers::default());
    cx.simulate_input("中文标题");
    let surface = cx
        .debug_bounds("native-editor-surface")
        .expect("mounted editable canvas");
    // Both edits pass through the production canvas/EntityInputHandler route;
    // no test-only mutation helper supplies the title or body string.
    cx.simulate_click(surface.center(), Modifiers::default());
    cx.simulate_input("中文正文");
    cx.dispatch_action(SelectAll);
    cx.dispatch_action(Copy);
    assert_eq!(
        cx.read_from_clipboard().and_then(|item| item.text()),
        Some("保留富文本样式中文正文".into()),
        "editable body keeps the mounted copy path available"
    );
    let save = cx
        .debug_bounds("library-sync-current")
        .expect("manual sync action");
    cx.simulate_click(save.center(), Modifiers::default());
    redraw(cx);

    let persisted = repository
        .load_note(&stored.id)
        .expect("load saved note")
        .expect("saved note");
    assert_eq!(persisted.title, "可编辑笔记中文标题");
    assert!(persisted.body_text.contains("中文正文"));
    let canonical = CanonicalDocument::parse_html(&persisted.body_html).expect("canonical save");
    assert!(format!("{canonical:?}").contains("bold: true"));
    assert!(format!("{canonical:?}").contains("italic: true"));
    view.read_with(cx, |view, cx| {
        let surface = view.editor_surface.as_ref().expect("mounted surface");
        assert_eq!(surface.read(cx).mode(), EditorSurfaceMode::Editable);
        assert!(
            surface
                .read(cx)
                .editor()
                .read(cx)
                .copy_all_plain_text()
                .contains("中文正文")
        );
        assert_eq!(
            view.model.read(cx).navigation().selected_note_id(),
            Some(&stored.id),
            "input/save actions must not mutate library selection"
        );
    });
}

#[gpui::test]
async fn mounted_editor_keeps_painting_while_a_slow_background_journal_waits(
    cx: &mut TestAppContext,
) {
    // The gate is inside the background SaveJob immediately before its
    // codec/SQLite work.  If the save path ever moves back into a foreground
    // entity callback, this mounted draw cannot make progress while the gate
    // is held.
    cx.update(|app| crate::components::init(app));
    let (_profile, repository) = repository();
    let note = repository
        .create_note(CreateNote {
            title: "慢写入仍可绘制".into(),
            notebook_id: None,
            document: rich_document("初始正文"),
        })
        .expect("create note");
    let clock = Arc::new(ManualSaveClock::default());
    let (view, cx) = mount_shell_with_save_clock(Arc::clone(&repository), Arc::clone(&clock), cx);
    redraw(cx);
    cx.update(|window, app| {
        view.update(app, |shell, shell_cx| {
            shell.apply_action(AppAction::SelectNote(note.id.clone()), window, shell_cx)
        });
    });
    redraw(cx);

    let session = view.read_with(cx, |shell, _| {
        shell
            .note_session
            .as_ref()
            .expect("mounted session")
            .clone()
    });
    let release = session.update(cx, |session, _| {
        session.enable_deadline_tasks_for_test();
        session.stall_next_background_save_for_test()
    });
    let surface = cx
        .debug_bounds("native-editor-surface")
        .expect("mounted editable canvas");
    cx.simulate_click(surface.center(), Modifiers::default());
    cx.simulate_input(" 后台慢写");

    let paints_before = view.read_with(cx, |shell, _| {
        shell
            .library_surface_paint_hooks_for_test
            .paint
            .load(std::sync::atomic::Ordering::Relaxed)
    });
    clock.advance(Duration::from_millis(100));
    cx.executor().advance_clock(Duration::from_millis(100));
    cx.run_until_parked();
    assert!(matches!(
        session.read_with(cx, |session, _| session.save_state()),
        crate::app::save_coordinator::SaveState::Journaling
    ));

    redraw(cx);
    let paints_after = view.read_with(cx, |shell, _| {
        shell
            .library_surface_paint_hooks_for_test
            .paint
            .load(std::sync::atomic::Ordering::Relaxed)
    });
    assert!(
        paints_after > paints_before,
        "a mounted editor must keep painting while worker codec/SQLite is slow"
    );
    assert!(
        repository.latest_edit_journal(&note.id).unwrap().is_none(),
        "the background gate must still be holding the journal job"
    );

    release.send(()).expect("release slow background save");
    cx.run_until_parked();
    let state_after_release = session.read_with(cx, |session, _| session.save_state());
    let journal_after_release = repository
        .latest_edit_journal(&note.id)
        .expect("read journal after release");
    let durable_after_release = repository
        .load_note(&note.id)
        .expect("read note after release")
        .expect("note after release");
    assert!(
        journal_after_release.is_some()
            || (durable_after_release.revision > note.revision
                && durable_after_release.body_text.contains("后台慢写")),
        "releasing the worker should complete a durable retained save; state={state_after_release:?}, journal={journal_after_release:?}, durable={durable_after_release:?}"
    );
}

#[gpui::test]
async fn mounted_inflight_worker_blocks_switch_delete_close_and_quit_until_completion(
    cx: &mut TestAppContext,
) {
    // Both attempts travel through the mounted lifecycle seam. The second
    // call used to see `Journaling`, return the old `last_saved`, and let a
    // close/switch/delete/quit tear down a session whose only durable write
    // was still stopped in a worker.
    cx.update(|app| crate::components::init(app));
    let (_profile, repository) = repository();
    let note = repository
        .create_note(CreateNote {
            title: "in-flight 生命周期".into(),
            notebook_id: None,
            document: rich_document("基线"),
        })
        .expect("create note");
    let other = repository
        .create_note(CreateNote {
            title: "目标笔记".into(),
            notebook_id: None,
            document: rich_document("另一篇"),
        })
        .expect("create alternate note");
    let clock = Arc::new(ManualSaveClock::default());
    let (view, cx) = mount_shell_with_save_clock(Arc::clone(&repository), Arc::clone(&clock), cx);
    redraw(cx);
    cx.update(|window, app| {
        view.update(app, |shell, shell_cx| {
            shell.apply_action(AppAction::SelectNote(note.id.clone()), window, shell_cx)
        });
    });
    redraw(cx);

    let session = view.read_with(cx, |shell, _| {
        shell
            .note_session
            .as_ref()
            .expect("mounted session")
            .clone()
    });
    let release = session.update(cx, |session, _| {
        session.enable_deadline_tasks_for_test();
        session.stall_next_background_save_for_test()
    });
    let surface = cx
        .debug_bounds("native-editor-surface")
        .expect("mounted editable surface");
    cx.simulate_click(surface.center(), Modifiers::default());
    cx.simulate_input(" 未完成保存");
    clock.advance(Duration::from_millis(100));
    cx.executor().advance_clock(Duration::from_millis(100));
    cx.run_until_parked();
    assert!(matches!(
        session.read_with(cx, |session, _| session.save_state()),
        crate::app::save_coordinator::SaveState::Journaling
    ));

    assert!(
        !view.update(cx, |shell, shell_cx| {
            shell.flush_for_lifecycle(FlushReason::WindowClose, shell_cx)
        }),
        "the first boundary queues an exact snapshot behind the journal"
    );
    let editor = session.read_with(cx, |session, _| session.editor().clone());
    cx.update(|window, app| {
        editor.update(app, |editor, editor_cx| {
            let end = editor.document().flat_utf16_len();
            <EditorCore as EntityInputHandler>::replace_text_in_range(
                editor,
                Some(end..end),
                " 后续 generation",
                window,
                editor_cx,
            );
        });
    });

    for _ in 0..2 {
        cx.update(|window, app| {
            view.update(app, |shell, shell_cx| {
                shell.apply_action(AppAction::SelectNote(other.id.clone()), window, shell_cx)
            });
        });
        assert_eq!(
            view.read_with(cx, |shell, shell_cx| {
                shell
                    .model
                    .read(shell_cx)
                    .navigation()
                    .selected_note_id()
                    .cloned()
            }),
            Some(note.id.clone()),
            "a repeated switch must not tear down the gated session"
        );
        cx.update(|window, app| {
            view.update(app, |shell, shell_cx| {
                shell.apply_action(AppAction::TrashSelected, window, shell_cx)
            });
        });
        assert!(
            repository
                .load_note(&note.id)
                .unwrap()
                .expect("first note")
                .deleted_time
                .is_none(),
            "a repeated delete must stay behind the exact save completion"
        );
        for reason in [FlushReason::WindowClose, FlushReason::Quit] {
            assert!(
                !view.update(cx, |shell, shell_cx| {
                    shell.flush_for_lifecycle(reason, shell_cx)
                }),
                "{reason:?} must remain blocked until the exact worker completion"
            );
        }
    }
    assert_eq!(
        repository
            .load_note(&note.id)
            .unwrap()
            .expect("note")
            .revision,
        note.revision,
        "a blocked lifecycle boundary must not claim the old revision is saved"
    );

    release.send(()).expect("release worker");
    cx.run_until_parked();
    assert!(view.update(cx, |shell, shell_cx| {
        shell.flush_for_lifecycle(FlushReason::WindowClose, shell_cx)
    }));
    let completion_confirmed = repository
        .load_note(&note.id)
        .unwrap()
        .expect("completion-confirmed note");
    assert!(
        completion_confirmed.body_text.contains("后续 generation"),
        "the lifecycle barrier must wait for the newest captured generation, not the old journal: {completion_confirmed:?}"
    );
}

#[gpui::test]
async fn mounted_corrected_generation_clears_only_its_automatic_save_error(
    cx: &mut TestAppContext,
) {
    // This is a mounted surface plus the real editor command path: an
    // unsupported nested list fails its automatic checkpoint, then a user
    // undo creates a newer valid generation. The old automatic warning must
    // disappear only after that newer generation has durably snapshotted.
    use crate::native_editor::commands::{CommandArgument, CommandCatalogue, EditorCommand};

    cx.update(|app| crate::components::init(app));
    let (_profile, repository) = repository();
    let note = repository
        .create_note(CreateNote {
            title: "自动错误恢复".into(),
            notebook_id: None,
            document: rich_document("正文"),
        })
        .expect("create note");
    let clock = Arc::new(ManualSaveClock::default());
    let (view, cx) = mount_shell_with_save_clock(Arc::clone(&repository), Arc::clone(&clock), cx);
    redraw(cx);
    cx.update(|window, app| {
        view.update(app, |shell, shell_cx| {
            shell.apply_action(AppAction::SelectNote(note.id.clone()), window, shell_cx)
        });
    });
    redraw(cx);
    let session = view.read_with(cx, |shell, _| {
        shell
            .note_session
            .as_ref()
            .expect("mounted session")
            .clone()
    });
    session.update(cx, |session, _| session.enable_deadline_tasks_for_test());
    let editor = session.read_with(cx, |session, _| session.editor().clone());
    editor.update(cx, |editor, editor_cx| {
        let commands = CommandCatalogue::new();
        commands
            .execute(EditorCommand::BulletList, CommandArgument::None, editor)
            .expect("real list command");
        commands
            .execute(EditorCommand::IndentList, CommandArgument::None, editor)
            .expect("real unsupported nested list command");
        editor_cx.notify();
    });
    clock.advance(Duration::from_millis(100));
    cx.executor().advance_clock(Duration::from_millis(100));
    cx.run_until_parked();
    redraw(cx);
    assert!(
        cx.debug_bounds("library-save-error").is_some(),
        "the actual codec failure must be visible"
    );

    editor.update(cx, |editor, editor_cx| {
        editor.undo().expect("undo unsupported nesting");
        editor_cx.notify();
    });
    clock.advance(Duration::from_millis(100));
    cx.executor().advance_clock(Duration::from_millis(100));
    cx.run_until_parked();
    clock.advance(Duration::from_millis(400));
    cx.executor().advance_clock(Duration::from_millis(400));
    cx.run_until_parked();
    redraw(cx);
    redraw(cx);

    let final_state = session.read_with(cx, |session, _| session.save_state());
    let final_error = view.read_with(cx, |shell, _| shell.save_error.clone());
    assert!(
        final_error.is_none(),
        "a newer successful automatic generation must clear only its own error; state={final_state:?}, error={final_error:?}"
    );
    let saved = repository.load_note(&note.id).unwrap().expect("saved note");
    assert!(saved.body_html.contains("<ul>"));
    assert_eq!(saved.revision, note.revision + 1);
}

#[gpui::test]
async fn stale_delayed_session_save_cannot_overwrite_the_newly_selected_note(
    cx: &mut TestAppContext,
) {
    // This models the hardest real ordering: A has a delayed save queued,
    // selection changes to B through the retained-model observer, and the
    // old callback wakes afterwards. A stale callback may save A's own
    // generation, but it must never borrow the newly mounted B surface.
    cx.update(|app| crate::components::init(app));
    let (_profile, repository) = repository();
    let first = repository
        .create_note(CreateNote {
            title: "会话 A".into(),
            notebook_id: None,
            document: rich_document("A 原文"),
        })
        .expect("create A");
    let second = repository
        .create_note(CreateNote {
            title: "会话 B".into(),
            notebook_id: None,
            document: rich_document("B 原文"),
        })
        .expect("create B");
    let clock = Arc::new(ManualSaveClock::default());
    let (view, cx) = mount_shell_with_save_clock(Arc::clone(&repository), Arc::clone(&clock), cx);
    redraw(cx);
    cx.update(|window, app| {
        view.update(app, |view, view_cx| {
            view.apply_action(AppAction::SelectNote(first.id.clone()), window, view_cx)
        });
    });
    redraw(cx);
    let first_surface = cx
        .debug_bounds("native-editor-surface")
        .expect("A mounts editable canvas");
    cx.simulate_click(first_surface.center(), Modifiers::default());
    cx.simulate_input(" A 延迟编辑");
    let stale_session = view.read_with(cx, |view, _| {
        view.note_session
            .as_ref()
            .expect("A session retained")
            .clone()
    });

    // Deliberately bypass the shell reducer's normal flush. This represents a
    // platform selection/update racing the already-scheduled A callback and
    // catches any implementation that makes a session consult global active
    // selection when it finally serializes.
    cx.update(|_window, app| {
        let model = view.read(app).model.clone();
        model.update(app, |model, model_cx| {
            model
                .dispatch(AppAction::SelectNote(second.id.clone()))
                .expect("select B through model seam");
            model_cx.notify();
        });
    });
    redraw(cx);
    assert_eq!(
        view.read_with(cx, |view, _| view.surface_note_id.clone()),
        Some(second.id.clone()),
        "retained observer must mount B before the stale work fires"
    );

    clock.advance(Duration::from_millis(500));
    stale_session.update(cx, |session, session_cx| {
        session
            .poll(session_cx)
            .expect("delayed A work is contained")
    });
    cx.run_until_parked();

    let saved_first = repository
        .load_note(&first.id)
        .expect("load A")
        .expect("A exists");
    let saved_second = repository
        .load_note(&second.id)
        .expect("load B")
        .expect("B exists");
    assert!(saved_first.body_text.contains("A 延迟编辑"));
    assert_eq!(saved_second.body_text, "B 原文");
    assert_eq!(saved_second.revision, second.revision);
}

#[gpui::test]
async fn mounted_window_close_flushes_the_current_edit_before_teardown(cx: &mut TestAppContext) {
    cx.update(|app| crate::components::init(app));
    let (_profile, repository) = repository();
    let note = repository
        .create_note(CreateNote {
            title: "关闭前保存".into(),
            notebook_id: None,
            document: rich_document("关闭前正文"),
        })
        .expect("create note");
    let (view, cx) = mount_shell(Arc::clone(&repository), cx);
    redraw(cx);
    cx.update(|window, app| {
        view.update(app, |view, view_cx| {
            view.apply_action(AppAction::SelectNote(note.id.clone()), window, view_cx)
        });
    });
    redraw(cx);
    let surface = cx
        .debug_bounds("native-editor-surface")
        .expect("mounted session surface");
    cx.simulate_click(surface.center(), Modifiers::default());
    cx.simulate_input(" 已编辑");

    // `simulate_close` invokes GPUI's platform should-close callback, exactly
    // like Cmd-W/the titlebar control. `remove_window` is a programmatic
    // teardown primitive and intentionally bypasses that callback.
    assert!(
        !cx.simulate_close(),
        "the first platform close must remain blocked while its real background snapshot runs"
    );
    cx.run_until_parked();
    assert!(
        cx.simulate_close(),
        "retrying the platform close after the exact worker completion permits teardown"
    );

    let saved = repository
        .load_note(&note.id)
        .expect("load after close")
        .expect("note remains");
    assert!(saved.body_text.contains("已编辑"));
}

#[gpui::test]
async fn mounted_action_boundaries_flush_before_switch_new_manual_sync_and_delete(
    cx: &mut TestAppContext,
) {
    cx.update(|app| crate::components::init(app));
    let (_profile, repository) = repository();
    let first = repository
        .create_note(CreateNote {
            title: "切换前".into(),
            notebook_id: None,
            document: rich_document("A"),
        })
        .expect("create A");
    let second = repository
        .create_note(CreateNote {
            title: "新建前".into(),
            notebook_id: None,
            document: rich_document("B"),
        })
        .expect("create B");
    let (view, cx) = mount_shell(Arc::clone(&repository), cx);
    redraw(cx);

    cx.update(|window, app| {
        view.update(app, |shell, shell_cx| {
            shell.apply_action(AppAction::SelectNote(first.id.clone()), window, shell_cx)
        });
    });
    redraw(cx);
    let first_surface = cx.debug_bounds("native-editor-surface").expect("A surface");
    cx.simulate_click(first_surface.center(), Modifiers::default());
    cx.simulate_input(" 已编辑");

    // Card/keyboard routes end here too: the shared reducer must compact A
    // before it changes retained selection to B.
    cx.update(|window, app| {
        view.update(app, |shell, shell_cx| {
            shell.apply_action(AppAction::SelectNote(second.id.clone()), window, shell_cx)
        });
    });
    assert_eq!(
        view.read_with(cx, |shell, shell_cx| {
            shell
                .model
                .read(shell_cx)
                .navigation()
                .selected_note_id()
                .cloned()
        }),
        Some(first.id.clone()),
        "the first switch click must wait for its retained background flush"
    );
    cx.run_until_parked();
    cx.update(|window, app| {
        view.update(app, |shell, shell_cx| {
            shell.apply_action(AppAction::SelectNote(second.id.clone()), window, shell_cx)
        });
    });
    redraw(cx);
    assert!(
        repository
            .load_note(&first.id)
            .expect("load A")
            .expect("A exists")
            .body_text
            .contains("已编辑")
    );

    let second_surface = cx.debug_bounds("native-editor-surface").expect("B surface");
    cx.simulate_click(second_surface.center(), Modifiers::default());
    cx.simulate_input(" 待新建");
    let create = cx
        .debug_bounds("library-create-note")
        .expect("mounted create action");
    cx.simulate_click(create.center(), Modifiers::default());
    assert_eq!(
        view.read_with(cx, |shell, shell_cx| {
            shell
                .model
                .read(shell_cx)
                .navigation()
                .selected_note_id()
                .cloned()
        }),
        Some(second.id.clone()),
        "the first create click must wait for the active note snapshot"
    );
    cx.run_until_parked();
    redraw(cx);
    let create = cx
        .debug_bounds("library-create-note")
        .expect("mounted create action after save");
    cx.simulate_click(create.center(), Modifiers::default());
    redraw(cx);
    assert!(
        repository
            .load_note(&second.id)
            .expect("load B")
            .expect("B exists")
            .body_text
            .contains("待新建")
    );

    let created_id = view.read_with(cx, |shell, shell_cx| {
        shell
            .model
            .read(shell_cx)
            .active_note()
            .expect("new note selected")
            .id
            .clone()
    });
    let created_surface = cx
        .debug_bounds("native-editor-surface")
        .expect("new note surface");
    cx.simulate_click(created_surface.center(), Modifiers::default());
    cx.simulate_input(" 手动保存后删除");
    let save = cx
        .debug_bounds("library-sync-current")
        .expect("manual-save action");
    cx.simulate_click(save.center(), Modifiers::default());
    cx.run_until_parked();
    redraw(cx);
    let save = cx
        .debug_bounds("library-sync-current")
        .expect("manual-save action after background completion");
    cx.simulate_click(save.center(), Modifiers::default());
    redraw(cx);
    assert!(
        repository
            .load_note(&created_id)
            .expect("load manually saved note")
            .expect("new note exists")
            .body_text
            .contains("手动保存后删除")
    );

    let created_surface = cx
        .debug_bounds("native-editor-surface")
        .expect("new note remains selected");
    cx.simulate_click(created_surface.center(), Modifiers::default());
    cx.simulate_input(" 删除前最后一笔");
    let trash = cx
        .debug_bounds("library-trash-selected")
        .expect("mounted trash action");
    cx.simulate_click(trash.center(), Modifiers::default());
    assert!(
        repository
            .load_note(&created_id)
            .expect("load retained note while delete saves")
            .expect("retained row")
            .deleted_time
            .is_none(),
        "the first trash click must not delete before the retained worker completes"
    );
    cx.run_until_parked();
    redraw(cx);
    let trash = cx
        .debug_bounds("library-trash-selected")
        .expect("mounted trash action after save");
    cx.simulate_click(trash.center(), Modifiers::default());
    redraw(cx);
    let deleted = repository
        .load_note(&created_id)
        .expect("load trashed note")
        .expect("trashed row retained");
    assert!(deleted.body_text.contains("删除前最后一笔"));
    assert!(deleted.deleted_time.is_some());
}

#[gpui::test]
async fn mounted_quit_lifecycle_flushes_the_current_edit_before_the_platform_request(
    cx: &mut TestAppContext,
) {
    cx.update(|app| crate::components::init(app));
    let (_profile, repository) = repository();
    let note = repository
        .create_note(CreateNote {
            title: "退出前保存".into(),
            notebook_id: None,
            document: rich_document("退出前正文"),
        })
        .expect("create note");
    let (view, cx) = mount_shell(Arc::clone(&repository), cx);
    redraw(cx);
    cx.update(|window, app| {
        view.update(app, |shell, shell_cx| {
            shell.apply_action(AppAction::SelectNote(note.id.clone()), window, shell_cx)
        });
    });
    redraw(cx);
    let surface = cx
        .debug_bounds("native-editor-surface")
        .expect("mounted session");
    cx.simulate_click(surface.center(), Modifiers::default());
    cx.simulate_input(" 已编辑");

    assert!(view.read_with(cx, |shell, shell_cx| {
        shell.note_session.as_ref().is_some_and(|session| {
            !matches!(
                session.read(shell_cx).save_state(),
                crate::app::save_coordinator::SaveState::Clean
            )
        })
    }));
    cx.cx.update(|app| {
        assert!(
            app.windows()
                .iter()
                .any(|window| window.downcast::<LibraryShell>().is_some())
        );
    });

    // Native-menu dispatch owns an App callback, not a nested window update.
    // Use the same context here so its root-window handle can be updated.
    cx.cx
        .update(|app| crate::library_menu::request_quit_library(app));
    assert!(
        !repository
            .load_note(&note.id)
            .expect("load while quit is waiting")
            .expect("note retained")
            .body_text
            .contains("已编辑"),
        "the first quit request must not claim the in-flight snapshot is durable"
    );
    cx.run_until_parked();
    cx.cx
        .update(|app| crate::library_menu::request_quit_library(app));
    assert!(
        repository
            .load_note(&note.id)
            .expect("load after quit request")
            .expect("note retained")
            .body_text
            .contains("已编辑")
    );
}

#[gpui::test]
async fn mounted_ime_lifecycle_warning_survives_clean_ticks_until_commit_and_successful_flush(
    cx: &mut TestAppContext,
) {
    // Removing the retained lifecycle-error branch in `poll_active_session`
    // used to make the next clean 50ms tick hide this blocker while macOS was
    // still presenting the candidate. This is a mounted UI test over the real
    // EntityInputHandler, not a hand-written save-state transition.
    let (_profile, repository) = repository();
    let note = repository
        .create_note(CreateNote {
            title: "组合提示".into(),
            notebook_id: None,
            document: rich_document("正文"),
        })
        .expect("create note");
    let (view, cx) = mount_shell(Arc::clone(&repository), cx);
    redraw(cx);
    cx.update(|window, app| {
        view.update(app, |shell, shell_cx| {
            shell.apply_action(AppAction::SelectNote(note.id.clone()), window, shell_cx)
        });
    });
    redraw(cx);
    let editor = view.read_with(cx, |shell, shell_cx| {
        shell
            .note_session
            .as_ref()
            .expect("retained note session")
            .read(shell_cx)
            .editor()
            .clone()
    });
    let end = cx.update(|_window, app| editor.read(app).document().flat_utf16_len());
    cx.update(|window, app| {
        editor.update(app, |editor, editor_cx| {
            <EditorCore as EntityInputHandler>::replace_and_mark_text_in_range(
                editor,
                Some(end..end),
                "候选",
                Some((end + 2)..(end + 2)),
                window,
                editor_cx,
            );
        });
    });
    assert!(!view.update(cx, |shell, shell_cx| {
        shell.flush_for_lifecycle(FlushReason::WindowClose, shell_cx)
    }));
    redraw(cx);
    assert!(cx.debug_bounds("library-save-error").is_some());

    // Drive the same automatic polling entry point that normally runs on the
    // timer. The warning must remain while composition has not resolved.
    view.update(cx, |shell, shell_cx| shell.poll_active_session(shell_cx));
    redraw(cx);
    assert!(
        cx.debug_bounds("library-save-error").is_some(),
        "a clean tick must not erase an unresolved IME lifecycle blocker"
    );

    cx.update(|window, app| {
        editor.update(app, |editor, editor_cx| {
            <EditorCore as EntityInputHandler>::unmark_text(editor, window, editor_cx);
        });
    });
    assert!(!view.update(cx, |shell, shell_cx| {
        shell.flush_for_lifecycle(FlushReason::WindowClose, shell_cx)
    }));
    cx.run_until_parked();
    assert!(view.update(cx, |shell, shell_cx| {
        shell.flush_for_lifecycle(FlushReason::WindowClose, shell_cx)
    }));
    redraw(cx);
    let warning = view.read_with(cx, |shell, _| shell.save_error.clone());
    assert!(
        warning.is_none(),
        "successful flush should clear warning: {warning:?}"
    );
    assert!(
        repository
            .load_note(&note.id)
            .unwrap()
            .expect("note")
            .body_text
            .contains("候选")
    );
}

#[gpui::test]
async fn selecting_an_unsupported_resource_note_never_reads_blob_bytes(cx: &mut TestAppContext) {
    // Catches accidental Task-5-style blob hydration while Task 3 only needs
    // metadata/canonical parsing to fail closed on image notes.
    let (_profile, repository) = repository();
    let image = repository
        .import_image(b"resource observer fixture", "image", "image/png", "png")
        .expect("create resource blob");
    let note = repository
        .create_note(CreateNote {
            title: "资源未加载".into(),
            notebook_id: None,
            document: CanonicalDocument::from_blocks(vec![Block::Paragraph {
                style: BlockStyle::default(),
                inlines: vec![Inline::Image {
                    resource_id: image,
                    alt: "Task 5 owns decode".into(),
                }],
            }]),
        })
        .expect("create unsupported note");
    let resource_reads = repository.observe_resource_reads();
    let (view, cx) = mount_shell(repository, cx);
    redraw(cx);
    cx.update(|window, app| {
        view.update(app, |view, view_cx| {
            view.apply_action(AppAction::SelectNote(note.id.clone()), window, view_cx)
        });
    });
    redraw(cx);

    assert!(cx.debug_bounds("unsupported-native-document").is_some());
    assert_eq!(resource_reads.try_recv(), Err(TryRecvError::Empty));
}

#[gpui::test]
async fn switching_to_an_unsupported_body_removes_the_previous_native_surface(
    cx: &mut TestAppContext,
) {
    let (_profile, repository) = repository();
    let supported = repository
        .create_note(CreateNote {
            title: "可显示正文".into(),
            notebook_id: None,
            document: rich_document("这篇正文必须先挂载画布"),
        })
        .expect("create supported note");
    let image = repository
        .import_image(
            b"task-5-will-decode-this",
            "future image",
            "image/png",
            "png",
        )
        .expect("create resource fixture");
    let unsupported = repository
        .create_note(CreateNote {
            title: "含图像正文".into(),
            notebook_id: None,
            document: CanonicalDocument::from_blocks(vec![Block::Paragraph {
                style: BlockStyle::default(),
                inlines: vec![Inline::Image {
                    resource_id: image,
                    alt: "Task 5 image".into(),
                }],
            }]),
        })
        .expect("create image note");
    let (view, cx) = mount_shell(repository, cx);
    cx.update(|window, app| {
        view.update(app, |view, view_cx| {
            view.apply_action(AppAction::SelectNote(supported.id.clone()), window, view_cx)
        });
    });
    redraw(cx);
    assert!(cx.debug_bounds("native-editor-surface").is_some());

    cx.update(|window, app| {
        view.update(app, |view, view_cx| {
            view.apply_action(
                AppAction::SelectNote(unsupported.id.clone()),
                window,
                view_cx,
            )
        });
    });
    redraw(cx);
    assert!(cx.debug_bounds("unsupported-native-document").is_some());
    view.read_with(cx, |view, _| {
        assert!(
            view.editor_surface.is_none(),
            "old note surface must be dropped"
        );
        assert_eq!(view.surface_note_id, Some(unsupported.id));
        assert!(
            view.unsupported_document
                .as_deref()
                .is_some_and(|message| message.contains("图片"))
        );
    });
}

#[gpui::test]
async fn uniform_list_constructs_only_requested_ranges_and_reaches_1662_tail(
    cx: &mut TestAppContext,
) {
    let (_profile, repository) = repository();
    for index in 0..1_662 {
        repository
            .create_note(CreateNote {
                title: format!("note {index:04}"),
                notebook_id: None,
                document: CanonicalDocument::default(),
            })
            .expect("create projection fixture");
    }
    let (view, cx) = mount_shell(repository, cx);
    note_list::reset_constructed_items_for_test();
    redraw(cx);
    let initial_constructed = note_list::constructed_items_for_test();
    assert!(
        initial_constructed < 128,
        "uniform list eagerly built {initial_constructed} cards"
    );
    view.read_with(cx, |view, _| {
        assert!(
            view.note_list_scroll.is_scrollable(),
            "mounted uniform list must have a finite viewport"
        );
    });

    let (tail, tail_index) = view.read_with(cx, |view, cx| {
        let model = view.model.read(cx);
        (
            model
                .projections()
                .last()
                .expect("tail projection")
                .id
                .clone(),
            model.projections().len() - 1,
        )
    });
    cx.update(|window, app| {
        view.update(app, |view, view_cx| {
            view.apply_action(AppAction::SelectNote(tail.clone()), window, view_cx)
        });
    });
    view.read_with(cx, |view, app| {
        assert_eq!(
            view.model.read_with(app, |model, _| model
                .navigation()
                .selected_note_id()
                .cloned()),
            Some(tail.clone()),
            "selection action must succeed before scrolling"
        );
        assert_eq!(view.last_scroll_request, Some(tail_index));
    });
    redraw(cx);
    view.read_with(cx, |view, cx| {
        assert!(
            view.rendered_note_range
                .as_ref()
                .is_some_and(|range| range.contains(&tail_index)),
            "tail card was not constructed after scroll"
        );
        assert_eq!(
            view.model.read(cx).navigation().selected_note_id(),
            Some(&tail)
        );
    });
    let tail_card = cx
        .debug_bounds("library-selected-note-card")
        .expect("tail card must be interactable after scroll-to-item");
    cx.simulate_click(tail_card.center(), Modifiers::default());
    redraw(cx);
    view.read_with(cx, |view, cx| {
        assert_eq!(
            view.model.read(cx).navigation().selected_note_id(),
            Some(&tail),
            "mounted tail card click must use the same NoteId reducer"
        );
    });
    assert!(
        note_list::constructed_items_for_test() < 320,
        "tail scroll built the whole library"
    );
}

#[gpui::test]
async fn restored_tail_selection_scrolls_on_its_first_mounted_draw(cx: &mut TestAppContext) {
    // Catches LibraryShell::new synchronizing its surface but forgetting the
    // deferred UniformListScrollHandle request that makes a restored tail
    // selection visible before the first user interaction.
    let (_profile, repository) = repository();
    for index in 0..1_662 {
        repository
            .create_note(CreateNote {
                title: format!("restored tail {index:04}"),
                notebook_id: None,
                document: CanonicalDocument::default(),
            })
            .expect("create restored-selection fixture");
    }
    let tail = AppModel::open(Arc::clone(&repository))
        .expect("inspect projections")
        .projections()
        .last()
        .expect("tail projection")
        .id
        .clone();
    repository
        .write_library_shell_state(&LibraryShellState {
            selected_note_id: Some(tail.clone()),
            ..LibraryShellState::default()
        })
        .expect("persist restored tail selection");

    let (view, cx) = mount_shell(repository, cx);
    note_list::reset_constructed_items_for_test();
    redraw(cx);

    view.read_with(cx, |view, cx| {
        let model = view.model.read(cx);
        let tail_index = model
            .projections()
            .iter()
            .position(|projection| projection.id == tail)
            .expect("restored note remains in projection");
        assert_eq!(model.navigation().selected_note_id(), Some(&tail));
        assert_eq!(view.last_scroll_request, Some(tail_index));
        assert!(
            view.rendered_note_range
                .as_ref()
                .is_some_and(|range| range.contains(&tail_index)),
            "first library draw must construct the restored tail card"
        );
    });
    assert!(
        note_list::constructed_items_for_test() < 160,
        "restoring a tail selection must stay virtualized"
    );
}

#[gpui::test]
async fn retained_model_observer_syncs_and_scrolls_an_independent_model_change(
    cx: &mut TestAppContext,
) {
    // Catches removal of the retained observer or its scroll side effect. This
    // intentionally bypasses LibraryShell::apply_action, so no UI callback
    // can mask a missing model-notification bridge.
    let (_profile, repository) = repository();
    let note = repository
        .create_note(CreateNote {
            title: "observer seam".into(),
            notebook_id: None,
            document: rich_document("observer must mount this body"),
        })
        .expect("create observer fixture");
    let (view, cx) = mount_shell(repository, cx);
    redraw(cx);

    cx.update(|_window, app| {
        let model = view.read(app).model.clone();
        model.update(app, |model, model_cx| {
            model
                .dispatch(AppAction::SelectNote(note.id.clone()))
                .expect("dispatch independent selection");
            model_cx.notify();
        });
    });
    redraw(cx);

    view.read_with(cx, |view, cx| {
        assert_eq!(
            view.model.read(cx).navigation().selected_note_id(),
            Some(&note.id)
        );
        assert_eq!(view.surface_note_id, Some(note.id.clone()));
        assert!(view.editor_surface.is_some());
        assert_eq!(view.last_scroll_request, Some(0));
    });
}

#[gpui::test]
async fn uniform_list_does_not_truncate_the_101st_projection(cx: &mut TestAppContext) {
    let (_profile, repository) = repository();
    for index in 0..101 {
        repository
            .create_note(CreateNote {
                title: format!("threshold {index:03}"),
                notebook_id: None,
                document: CanonicalDocument::default(),
            })
            .expect("create threshold fixture");
    }
    let (view, cx) = mount_shell(repository, cx);
    redraw(cx);
    let tail = view.read_with(cx, |view, cx| {
        view.model
            .read(cx)
            .projections()
            .last()
            .expect("101st projection")
            .id
            .clone()
    });
    cx.update(|window, app| {
        view.update(app, |view, view_cx| {
            view.apply_action(AppAction::SelectNote(tail.clone()), window, view_cx)
        });
    });
    redraw(cx);
    view.read_with(cx, |view, cx| {
        assert_eq!(
            view.model.read(cx).navigation().selected_note_id(),
            Some(&tail)
        );
        assert!(
            view.rendered_note_range
                .as_ref()
                .is_some_and(|range| range.contains(&100)),
            "the 101st card must be mounted after its NoteId is selected"
        );
    });
    let tail_card = cx
        .debug_bounds("library-selected-note-card")
        .expect("the mounted 101st card must be clickable");
    cx.simulate_click(tail_card.center(), Modifiers::default());
    redraw(cx);
    view.read_with(cx, |view, cx| {
        assert_eq!(
            view.model.read(cx).navigation().selected_note_id(),
            Some(&tail),
            "the 101st card must dispatch selection through LibraryShell"
        );
    });
}
