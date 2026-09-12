use super::*;
use crate::app::save_coordinator::ManualSaveClock;
use crate::components::QuitApplication;
use crate::library_menu;
use app_lite_core::{CanonicalDocument, CreateNote, LibraryRepository};
use gpui::{
    Entity, EntityInputHandler, Modifiers, Render, Styled, TestAppContext, VisualTestContext, div,
};
use std::sync::{Arc, mpsc};

/// Replaces a LibraryShell after its close guard has captured a weak handle.
/// It models a root replacement/teardown race: the old weak update must never
/// be interpreted as a safe clean close.
struct CloseGuardReplacementRoot;

impl Render for CloseGuardReplacementRoot {
    fn render(
        &mut self,
        _window: &mut gpui::Window,
        _cx: &mut gpui::Context<Self>,
    ) -> impl gpui::IntoElement {
        div().size_full()
    }
}

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

fn mount_shell<'a>(
    repository: Arc<LibraryRepository>,
    clock: Arc<ManualSaveClock>,
    cx: &'a mut TestAppContext,
) -> (Entity<LibraryShell>, &'a mut VisualTestContext) {
    let model = cx.new(|_| AppModel::open(repository).expect("open model"));
    cx.add_window_view(move |window, cx| {
        LibraryShell::new_with_save_clock(model, None, clock, window, cx)
    })
}

fn install_library_menu(cx: &mut TestAppContext) {
    cx.update(|app| {
        crate::components::init(app);
        let (_sender, receiver) = mpsc::channel();
        library_menu::init(
            app,
            tempfile::tempdir().expect("menu profile").keep(),
            receiver,
        );
    });
}

#[gpui::test]
async fn mounted_empty_library_menu_and_cmd_q_share_the_platform_quit_path(
    cx: &mut TestAppContext,
) {
    // Release repro: the default LibraryShell must route both the visible
    // native menu item and Cmd-Q through the one save-aware application quit
    // action, even when the fresh profile has no active NoteSession.
    install_library_menu(cx);
    let (_profile, repository) = repository();
    let (_view, cx) = mount_shell(repository, Arc::new(ManualSaveClock::default()), cx);
    redraw(cx);

    // This is the actual macOS shortcut path rather than a direct reducer
    // call. `components::init` above owns the platform Cmd-Q binding.
    cx.simulate_keystrokes("cmd-q");
    cx.run_until_parked();
    assert_eq!(
        cx.cx.read(library_menu::platform_quit_requests_for_test),
        1,
        "Cmd-Q on a clean, zero-note LibraryShell must reach the production platform-quit call"
    );
}

#[gpui::test]
async fn mounted_menu_quit_waits_for_dirty_note_then_finishes_the_same_request(
    cx: &mut TestAppContext,
) {
    // The first action must not quit with unsaved text, but it also must not
    // require a second Cmd-Q/menu click after the exact retained snapshot has
    // completed. Deleting the production continuation or bypassing its flush
    // gate makes either durable text or the platform-quit assertion fail.
    install_library_menu(cx);
    let (_profile, repository) = repository();
    let note = repository
        .create_note(CreateNote {
            title: "退出保存".into(),
            notebook_id: None,
            document: CanonicalDocument::default(),
        })
        .expect("create note");
    let (view, cx) = mount_shell(
        Arc::clone(&repository),
        Arc::new(ManualSaveClock::default()),
        cx,
    );
    redraw(cx);
    cx.update(|window, app| {
        view.update(app, |shell, shell_cx| {
            shell.apply_action(AppAction::SelectNote(note.id.clone()), window, shell_cx)
        });
    });
    redraw(cx);
    let surface = cx
        .debug_bounds("native-editor-surface")
        .expect("selected note surface");
    cx.simulate_click(surface.center(), Modifiers::default());
    cx.simulate_input("退出前的正文");

    // Dispatch the exact public action carried by the visible native menu.
    // The menu-specific test exercises extraction from the production menu;
    // this direct dispatch keeps the retained-save timing observable before
    // the executor runs the completion callback.
    cx.cx.update(|app| app.dispatch_action(&QuitApplication));
    assert_eq!(
        cx.cx.read(library_menu::platform_quit_requests_for_test),
        0,
        "the first request must wait for its exact dirty-session snapshot"
    );
    assert!(
        !repository
            .load_note(&note.id)
            .expect("load retained note")
            .expect("note retained")
            .body_text
            .contains("退出前的正文"),
        "the platform request may not happen before the current edit is durable"
    );

    cx.run_until_parked();
    assert!(
        repository
            .load_note(&note.id)
            .expect("load saved note")
            .expect("note retained")
            .body_text
            .contains("退出前的正文"),
        "the same menu/Cmd-Q request must first persist the exact body"
    );
    assert_eq!(
        cx.cx.read(library_menu::platform_quit_requests_for_test),
        1,
        "once the retained save completes, the original quit request must finish without a second click"
    );
}

#[gpui::test]
async fn mounted_window_dispatched_quit_defers_the_dirty_flush_until_after_menu_window_update(
    cx: &mut TestAppContext,
) {
    // Regression for the native macOS bridge. `Window::dispatch_action` runs
    // the global action from inside a window update. Calling
    // `WindowHandle<LibraryShell>::update` immediately from that handler
    // therefore fails with GPUI's "window not found" re-entrancy guard. The
    // production quit request must defer its save-aware flush until that
    // action update has returned; treating the error as Ready would lose this
    // body while still requesting a platform quit.
    //
    // Deleting the production `cx.defer` boundary makes the durable assertion
    // below fail: the old implementation asked the platform to quit while the
    // repository still held the pre-edit body.
    install_library_menu(cx);
    let (_profile, repository) = repository();
    let note = repository
        .create_note(CreateNote {
            title: "窗口菜单退出".into(),
            notebook_id: None,
            document: CanonicalDocument::default(),
        })
        .expect("create note");
    let (view, cx) = mount_shell(
        Arc::clone(&repository),
        Arc::new(ManualSaveClock::default()),
        cx,
    );
    redraw(cx);
    cx.update(|window, app| {
        view.update(app, |shell, shell_cx| {
            shell.apply_action(AppAction::SelectNote(note.id.clone()), window, shell_cx)
        });
    });
    redraw(cx);
    let surface = cx
        .debug_bounds("native-editor-surface")
        .expect("selected note surface");
    cx.simulate_click(surface.center(), Modifiers::default());
    cx.simulate_input("原生菜单必须保存这段正文");

    // This is intentionally VisualTestContext::dispatch_action rather than
    // App::dispatch_action: it follows Window::dispatch_action, the exact
    // deferred native-menu route that exercises the re-entrant window stack.
    cx.dispatch_action(QuitApplication);

    assert!(
        repository
            .load_note(&note.id)
            .expect("load durable note")
            .expect("note retained")
            .body_text
            .contains("原生菜单必须保存这段正文"),
        "a window-dispatched QuitApplication must save before it asks the platform to exit"
    );
    assert_eq!(
        cx.cx.read(library_menu::platform_quit_requests_for_test),
        1,
        "the same deferred request completes only after its exact body is durable"
    );
}

#[gpui::test]
async fn mounted_window_close_blocks_when_its_retained_shell_update_is_unavailable(
    cx: &mut TestAppContext,
) {
    // Mutation-sensitive: the prior `unwrap_or(true)` let an old close guard
    // authorize a platform close after its weak LibraryShell was gone. Replacing
    // the root makes that exact update fail; the production guard must return
    // false and surface a critical explanation rather than risk a dirty write.
    let (_profile, repository) = repository();
    let (view, mut visual) = mount_shell(repository, Arc::new(ManualSaveClock::default()), cx);
    redraw(&mut visual);
    visual.update(|window, app| {
        window.replace_root(app, |_window, _cx| CloseGuardReplacementRoot);
    });
    drop(view);

    assert!(
        !visual.simulate_close(),
        "a failed retained-shell update must block the close boundary"
    );
    let (title, detail) = visual
        .cx
        .pending_prompt()
        .expect("the blocked close must be visible to the user");
    assert!(title.contains("无法安全关闭"));
    assert!(detail.contains("会话"));
}

#[gpui::test]
async fn mounted_cmd_q_blocks_an_active_ime_candidate_then_saves_after_explicit_retry(
    cx: &mut TestAppContext,
) {
    // A macOS candidate is not a durable document mutation. Cmd-Q must keep
    // the window alive until composition resolves; only the next explicit
    // request after unmarking may snapshot and quit.
    install_library_menu(cx);
    let (_profile, repository) = repository();
    let note = repository
        .create_note(CreateNote {
            title: "退出 IME".into(),
            notebook_id: None,
            document: CanonicalDocument::default(),
        })
        .expect("create note");
    let (view, cx) = mount_shell(
        Arc::clone(&repository),
        Arc::new(ManualSaveClock::default()),
        cx,
    );
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
            .expect("selected session")
            .read(shell_cx)
            .editor()
            .clone()
    });
    let end = cx.update(|_window, app| editor.read(app).document().flat_utf16_len());
    cx.update(|window, app| {
        editor.update(app, |editor, editor_cx| {
            <crate::native_editor::core::EditorCore as EntityInputHandler>::replace_and_mark_text_in_range(
                editor,
                Some(end..end),
                "候选退出正文",
                Some((end + "候选退出正文".len())..(end + "候选退出正文".len())),
                window,
                editor_cx,
            );
        });
    });

    cx.cx.update(|app| app.dispatch_action(&QuitApplication));
    cx.run_until_parked();
    assert_eq!(
        cx.cx.read(library_menu::platform_quit_requests_for_test),
        0,
        "Cmd-Q must not exit while native composition is still active"
    );
    assert!(
        !repository
            .load_note(&note.id)
            .expect("load note")
            .expect("note exists")
            .body_text
            .contains("候选退出正文"),
        "an unresolved IME candidate cannot be claimed durable"
    );

    cx.update(|window, app| {
        editor.update(app, |editor, editor_cx| {
            <crate::native_editor::core::EditorCore as EntityInputHandler>::unmark_text(
                editor, window, editor_cx,
            );
        });
    });
    cx.cx.update(|app| app.dispatch_action(&QuitApplication));
    cx.run_until_parked();
    assert!(
        repository
            .load_note(&note.id)
            .expect("load saved note")
            .expect("note exists")
            .body_text
            .contains("候选退出正文"),
        "the explicit retry after commit must own the exact durable snapshot"
    );
    assert_eq!(cx.cx.read(library_menu::platform_quit_requests_for_test), 1);
}

#[gpui::test]
async fn mounted_quit_waits_for_two_dirty_library_windows_before_one_platform_exit(
    cx: &mut TestAppContext,
) {
    // One application quit must fence every LibraryShell, not just the active
    // window. Removing the per-window loop or returning Ready after the first
    // clean result makes one of these independently derived bodies disappear.
    install_library_menu(cx);
    let (_profile, repository) = repository();
    let first_note = repository
        .create_note(CreateNote {
            title: "第一窗口退出".into(),
            notebook_id: None,
            document: CanonicalDocument::default(),
        })
        .expect("create first note");
    let second_note = repository
        .create_note(CreateNote {
            title: "第二窗口退出".into(),
            notebook_id: None,
            document: CanonicalDocument::default(),
        })
        .expect("create second note");

    let first_model = cx.new(|_| AppModel::open(Arc::clone(&repository)).expect("open model"));
    let (first_view, mut first) = cx.add_window_view(move |window, app| {
        LibraryShell::new_with_save_clock(
            first_model,
            None,
            Arc::new(ManualSaveClock::default()),
            window,
            app,
        )
    });
    let second_model = first
        .cx
        .new(|_| AppModel::open(Arc::clone(&repository)).expect("open second model"));
    let mut second_context = first.cx.clone();
    let (second_view, mut second) = second_context.add_window_view(move |window, app| {
        LibraryShell::new_with_save_clock(
            second_model,
            None,
            Arc::new(ManualSaveClock::default()),
            window,
            app,
        )
    });
    redraw(&mut first);
    redraw(&mut second);

    first.update(|window, app| {
        first_view.update(app, |shell, shell_cx| {
            shell.apply_action(
                AppAction::SelectNote(first_note.id.clone()),
                window,
                shell_cx,
            )
        });
    });
    second.update(|window, app| {
        second_view.update(app, |shell, shell_cx| {
            shell.apply_action(
                AppAction::SelectNote(second_note.id.clone()),
                window,
                shell_cx,
            )
        });
    });
    redraw(&mut first);
    redraw(&mut second);
    let first_surface = first
        .debug_bounds("native-editor-surface")
        .expect("first surface");
    first.simulate_click(first_surface.center(), Modifiers::default());
    first.simulate_input("第一窗口未保存");
    let second_surface = second
        .debug_bounds("native-editor-surface")
        .expect("second surface");
    second.simulate_click(second_surface.center(), Modifiers::default());
    second.simulate_input("第二窗口未保存");

    first.cx.update(|app| app.dispatch_action(&QuitApplication));
    assert_eq!(
        first.cx.read(library_menu::platform_quit_requests_for_test),
        0,
        "the process must wait while either dirty shell still owns its exact snapshot"
    );
    first.run_until_parked();
    assert!(
        repository
            .load_note(&first_note.id)
            .expect("load first")
            .expect("first exists")
            .body_text
            .contains("第一窗口未保存")
    );
    assert!(
        repository
            .load_note(&second_note.id)
            .expect("load second")
            .expect("second exists")
            .body_text
            .contains("第二窗口未保存")
    );
    assert_eq!(
        first.cx.read(library_menu::platform_quit_requests_for_test),
        1,
        "all clean shells complete one idempotent platform quit"
    );
}

#[gpui::test]
async fn mounted_repeated_quit_waits_for_a_gated_worker_then_exits_once(cx: &mut TestAppContext) {
    // A second menu/Cmd-Q request during the same retained worker must neither
    // bypass the writer nor duplicate the platform exit after completion.
    install_library_menu(cx);
    let (_profile, repository) = repository();
    let note = repository
        .create_note(CreateNote {
            title: "慢退出".into(),
            notebook_id: None,
            document: CanonicalDocument::default(),
        })
        .expect("create note");
    let (view, cx) = mount_shell(
        Arc::clone(&repository),
        Arc::new(ManualSaveClock::default()),
        cx,
    );
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
            .expect("selected session")
            .clone()
    });
    let release = session.update(cx, |session, _| {
        session.enable_deadline_tasks_for_test();
        session.stall_next_background_save_for_test()
    });
    let surface = cx
        .debug_bounds("native-editor-surface")
        .expect("selected surface");
    cx.simulate_click(surface.center(), Modifiers::default());
    cx.simulate_input("后台 worker 退出正文");

    for _ in 0..2 {
        cx.cx.update(|app| app.dispatch_action(&QuitApplication));
    }
    cx.run_until_parked();
    assert_eq!(
        cx.cx.read(library_menu::platform_quit_requests_for_test),
        0,
        "neither duplicate request may exit while the exact writer is gated"
    );
    assert!(
        !repository
            .load_note(&note.id)
            .expect("load pending note")
            .expect("note exists")
            .body_text
            .contains("后台 worker 退出正文")
    );

    release.send(()).expect("release retained worker");
    cx.run_until_parked();
    assert!(
        repository
            .load_note(&note.id)
            .expect("load saved note")
            .expect("note exists")
            .body_text
            .contains("后台 worker 退出正文")
    );
    assert_eq!(
        cx.cx.read(library_menu::platform_quit_requests_for_test),
        1,
        "the pending requests converge on one platform exit after the worker finishes"
    );
}
