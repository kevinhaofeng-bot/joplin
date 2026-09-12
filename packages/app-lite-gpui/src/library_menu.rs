//! Narrow application-level bootstrap for the local-library product route.
//!
//! This intentionally does not reuse the old Velotype file-editor menu: its
//! file, updater, export and workspace actions would silently open a second
//! product. The common lifecycle/menu infrastructure is retained here with
//! library-native actions only.

use crate::app::save_coordinator::FlushReason;
use crate::app::{
    CreateNote, CycleListViewMode, CycleSort, SyncCurrent, ToggleNoteList, ToggleSidebar,
    TrashSelected,
};
use crate::components::QuitApplication;
use crate::file_url::parse_file_url;
use crate::ui::{self, LibraryShell};
use gpui::{AnyWindowHandle, App, Global, Menu, MenuItem, PromptLevel, Subscription, Task};
use std::path::PathBuf;
use std::sync::mpsc::Receiver;
use std::time::Duration;

#[derive(Default)]
struct LibraryMenuLifecycle {
    close_subscription: Option<Subscription>,
    open_url_task: Option<Task<()>>,
    quit_pending: bool,
    platform_quit_dispatched: bool,
    #[cfg(test)]
    platform_quit_requests: usize,
}

impl Global for LibraryMenuLifecycle {}

/// Install the process-lifecycle guard before any fallible profile or runtime
/// work. That makes a visible StartupErrorView behave like every normal window
/// when the person closes the last window.
pub(crate) fn install_last_window_quit(cx: &mut App) {
    if !cx.has_global::<LibraryMenuLifecycle>() {
        cx.set_global(LibraryMenuLifecycle::default());
    }
    if cx
        .global::<LibraryMenuLifecycle>()
        .close_subscription
        .is_some()
    {
        return;
    }
    let close_subscription = cx.on_window_closed(|cx| {
        if cx.windows().is_empty() {
            finish_platform_quit(cx);
        }
    });
    cx.global_mut::<LibraryMenuLifecycle>().close_subscription = Some(close_subscription);
}

#[cfg(test)]
impl LibraryMenuLifecycle {
    fn quit_requests_for_test(&self) -> usize {
        self.platform_quit_requests
    }
}

#[cfg(test)]
pub(crate) fn platform_quit_requests_for_test(cx: &App) -> usize {
    cx.global::<LibraryMenuLifecycle>().quit_requests_for_test()
}

/// Turns an asynchronous platform-open result into visible UI. Callers must
/// handle this result too: even a fallback window failure is not silently
/// treated as success.
fn present_open_request_result(cx: &mut App, result: Result<(), String>) -> Result<(), String> {
    match result {
        Ok(()) => {
            cx.activate(true);
            cx.refresh_windows();
            Ok(())
        }
        Err(error) => {
            let message = format!("无法处理打开请求：{error}");
            ui::open_startup_error_window(cx, message).map(|_| {
                cx.activate(true);
                cx.refresh_windows();
            })
        }
    }
}

pub(crate) fn init(cx: &mut App, profile: PathBuf, open_url_receiver: Receiver<Vec<String>>) {
    install_last_window_quit(cx);

    // Cmd-Q is bound by the shared application keymap to QuitApplication.
    // The Library route accepts it through the same lifecycle gate as the
    // visible menu item, rather than leaving a second shortcut path able to
    // skip the exact session flush.
    cx.on_action(|_: &QuitApplication, cx| request_quit_library(cx));
    cx.set_menus(vec![library_menu()]);

    // `Application::on_open_urls` has no App argument. The receiver bridge
    // transfers those platform callbacks back onto GPUI's foreground executor
    // and opens only a visible import-not-supported notice.
    let task = cx.spawn(async move |cx| {
        loop {
            cx.background_executor()
                .timer(Duration::from_millis(75))
                .await;
            let paths = open_url_receiver
                .try_iter()
                .take(32)
                .flatten()
                .filter_map(|url| parse_file_url(&url))
                .collect::<Vec<_>>();
            if paths.is_empty() {
                continue;
            }
            let notice = import_notice(&paths);
            let profile = profile.clone();
            match cx.update(|cx| {
                let opened =
                    ui::open_library_window_with_notice(cx, profile, Some(notice)).map(|_| ());
                present_open_request_result(cx, opened)
            }) {
                Ok(Ok(())) => {}
                Ok(Err(error)) => {
                    eprintln!("无法显示打开请求错误：{error}");
                }
                Err(error) => {
                    eprintln!("无法将打开请求交回前台：{error}");
                }
            }
        }
    });
    cx.global_mut::<LibraryMenuLifecycle>().open_url_task = Some(task);
}

fn library_menu() -> Menu {
    Menu {
        name: "Joplin Lite".into(),
        items: vec![
            MenuItem::action("新建笔记", CreateNote),
            MenuItem::action("移至废纸篓", TrashSelected),
            MenuItem::separator(),
            MenuItem::action("显示/隐藏侧栏", ToggleSidebar),
            MenuItem::action("显示/隐藏笔记列表", ToggleNoteList),
            MenuItem::action("切换列表视图", CycleListViewMode),
            MenuItem::action("切换排序", CycleSort),
            MenuItem::action("保存当前笔记", SyncCurrent),
            MenuItem::separator(),
            MenuItem::action("退出 Joplin Lite", QuitApplication),
        ],
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum QuitFlushStatus {
    Ready,
    Pending,
    Blocked,
}

const QUIT_BLOCKED_TITLE: &str = "无法安全退出 Joplin Lite";

fn present_quit_blocked_prompt(window: &mut gpui::Window, detail: &str, cx: &mut App) {
    let buttons = ["好"];
    let _ = window.prompt(
        PromptLevel::Critical,
        QUIT_BLOCKED_TITLE,
        Some(detail),
        &buttons,
        cx,
    );
}

/// A live window whose root cannot be safely inspected must keep the process
/// alive. Prefer attaching the explanation to that exact window; if even the
/// generic window update fails, open a dedicated visible error window rather
/// than silently translating the failure into a clean quit.
fn report_quit_blocked_window(window: AnyWindowHandle, detail: &str, cx: &mut App) {
    let detail = detail.to_owned();
    if window
        .update(cx, |_, window, cx| {
            present_quit_blocked_prompt(window, &detail, cx);
        })
        .is_err()
    {
        let _ = ui::open_quit_safety_error_window(cx, detail);
    }
}

/// Ask every mounted library shell to finish the exact generation it owns.
/// A process exit can proceed only when every shell is already durable.
fn flush_library_windows_for_quit(cx: &mut App) -> QuitFlushStatus {
    let mut pending = false;
    let windows = cx.windows();
    for window in windows {
        let Some(shell) = window.downcast::<LibraryShell>() else {
            // StartupErrorView has no library model/session or persistence
            // authority, so it is the sole non-Library root that can safely
            // participate in a clean application quit. Every other root is
            // fail-closed: a future route must explicitly join this protocol.
            if window.downcast::<ui::StartupErrorView>().is_some() {
                continue;
            }
            report_quit_blocked_window(
                window,
                "检测到无法确认保存状态的窗口；已取消退出以保护本地数据。",
                cx,
            );
            return QuitFlushStatus::Blocked;
        };
        let status = match shell.update(cx, |shell, _window, shell_cx| {
            if shell.flush_for_lifecycle(FlushReason::Quit, shell_cx) {
                QuitFlushStatus::Ready
            } else if shell.lifecycle_flush_pending() {
                QuitFlushStatus::Pending
            } else {
                QuitFlushStatus::Blocked
            }
        }) {
            Ok(status) => status,
            // `WindowHandle::update` also fails while GPUI has the same
            // window borrowed for action dispatch.  That is not evidence that
            // the window owns no dirty session: treating it as Ready was the
            // native-menu data-loss path.  Every inability to inspect a live
            // shell therefore blocks process exit; the request may retry only
            // after a later retained-session notification or a new user quit
            // request.
            Err(_) => {
                report_quit_blocked_window(
                    window,
                    "无法确认当前笔记是否已保存；请保持窗口打开后重试。",
                    cx,
                );
                QuitFlushStatus::Blocked
            }
        };
        match status {
            QuitFlushStatus::Ready => {}
            QuitFlushStatus::Pending => pending = true,
            QuitFlushStatus::Blocked => return QuitFlushStatus::Blocked,
        }
    }
    if pending {
        QuitFlushStatus::Pending
    } else {
        QuitFlushStatus::Ready
    }
}

fn finish_platform_quit(cx: &mut App) {
    if cx.has_global::<LibraryMenuLifecycle>() {
        let lifecycle = cx.global_mut::<LibraryMenuLifecycle>();
        if lifecycle.platform_quit_dispatched {
            return;
        }
        lifecycle.platform_quit_dispatched = true;
        lifecycle.quit_pending = false;
        #[cfg(test)]
        {
            lifecycle.platform_quit_requests += 1;
        }
    }
    cx.quit();
}

/// The app menu can receive either the visible Quit item or Cmd-Q while a
/// library editor has focus. Both actions establish one retained request;
/// unfinished work remains safe and completes the same request on notify.
pub(crate) fn request_quit_library(cx: &mut App) {
    install_last_window_quit(cx);
    cx.global_mut::<LibraryMenuLifecycle>().quit_pending = true;
    // A native menu item reaches this global handler from
    // `Window::dispatch_action`, while that same root window is temporarily
    // taken out of GPUI's window map.  Defer the cross-window flush until the
    // action update has returned; otherwise `WindowHandle::update` reports
    // "window not found" and must never be mistaken for a clean session.
    cx.defer(retry_pending_quit);
}

/// Retry a previously accepted quit request after a retained NoteSession has
/// emitted a real completion. A failed flush clears this pending state and
/// leaves the session/window open with its visible lifecycle error.
pub(crate) fn retry_pending_quit(cx: &mut App) {
    if !cx.has_global::<LibraryMenuLifecycle>() || !cx.global::<LibraryMenuLifecycle>().quit_pending
    {
        return;
    }
    match flush_library_windows_for_quit(cx) {
        QuitFlushStatus::Ready => finish_platform_quit(cx),
        QuitFlushStatus::Pending => {}
        QuitFlushStatus::Blocked => {
            cx.global_mut::<LibraryMenuLifecycle>().quit_pending = false;
        }
    }
}

pub(crate) fn import_notice(paths: &[PathBuf]) -> String {
    let joined = paths
        .iter()
        .take(3)
        .map(|path| path.display().to_string())
        .collect::<Vec<_>>()
        .join("、");
    let suffix = (paths.len() > 3).then_some("等文件").unwrap_or("文件");
    format!("暂不支持导入 {joined}{suffix}；原文件未被读取或修改。")
}

#[cfg(test)]
mod tests {
    use super::{LibraryMenuLifecycle, import_notice, library_menu};
    use crate::app::AppModel;
    use crate::app::save_coordinator::ManualSaveClock;
    use crate::components::QuitApplication;
    use crate::ui::{self, LibraryShell, StartupErrorView};
    use app_lite_core::LibraryRepository;
    use gpui::{AppContext, Entity, Render, Styled, TestAppContext, VisualTestContext, div};
    use std::path::PathBuf;
    use std::sync::{Arc, mpsc};

    /// Deliberately not a LibraryShell or StartupErrorView. The production
    /// menu must never infer "no unsaved data" merely because a live window
    /// root has not been classified.
    struct UnknownQuitRoot;

    impl Render for UnknownQuitRoot {
        fn render(
            &mut self,
            _window: &mut gpui::Window,
            _cx: &mut gpui::Context<Self>,
        ) -> impl gpui::IntoElement {
            div().size_full()
        }
    }

    #[test]
    fn import_notice_is_visible_and_promises_no_source_mutation() {
        let notice = import_notice(&[PathBuf::from("/tmp/one.md")]);
        assert!(notice.contains("暂不支持导入"));
        assert!(notice.contains("未被读取或修改"));
    }

    #[test]
    fn visible_library_quit_menu_uses_the_standard_cmd_q_action() {
        // The macOS menu bridge dispatches the typed action object stored in
        // this tree. Reverting just the menu item to a private action makes
        // this structural action-path assertion fail before any platform
        // callback can be invoked.
        let menu = library_menu();
        assert!(menu.items.iter().any(|item| {
            matches!(
                item,
                gpui::MenuItem::Action { name, action, .. }
                    if name == "退出 Joplin Lite" && action.as_any().is::<QuitApplication>()
            )
        }));
    }

    #[gpui::test]
    async fn visible_library_menu_quit_action_dispatches_once_for_an_empty_profile(
        cx: &mut TestAppContext,
    ) {
        // Unlike the TestPlatform's unavailable native menu callback, this
        // extracts the exact action stored in the production menu tree and
        // sends it through App::dispatch_action — the same GPUI entrypoint
        // the macOS menu bridge uses. The close after platform quit models
        // NSApplication's ensuing last-window callback and must not request
        // process termination twice.
        let profile = tempfile::tempdir().expect("temporary profile");
        let repository = Arc::new(
            LibraryRepository::open(profile.path().join("library.sqlite"))
                .expect("open temporary library"),
        );
        cx.update(|app| {
            crate::components::init(app);
            let (_sender, receiver) = mpsc::channel();
            super::init(app, profile.path().to_path_buf(), receiver);
        });
        let model = cx.new(|_| AppModel::open(Arc::clone(&repository)).expect("open model"));
        let (_view, mut visual) = cx.add_window_view(move |window, cx| {
            LibraryShell::new_with_save_clock(
                model,
                None,
                Arc::new(ManualSaveClock::default()),
                window,
                cx,
            )
        });
        visual.update(|window, app| window.draw(app).clear());
        let action = library_menu()
            .items
            .into_iter()
            .find_map(|item| match item {
                gpui::MenuItem::Action { name, action, .. } if name == "退出 Joplin Lite" => {
                    Some(action)
                }
                _ => None,
            })
            .expect("visible Quit action in the production menu tree");

        visual.cx.update(|app| app.dispatch_action(action.as_ref()));
        visual.run_until_parked();
        assert_eq!(
            visual.cx.read(super::platform_quit_requests_for_test),
            1,
            "the stored visible menu action must reach the real platform-quit helper"
        );

        visual.update(|window, _app| window.remove_window());
        visual.cx.update(|_| {});
        assert_eq!(
            visual.cx.read(super::platform_quit_requests_for_test),
            1,
            "the last-window callback after a process quit must not dispatch a second platform quit"
        );
    }

    #[gpui::test]
    async fn menu_quit_blocks_and_reports_an_unknown_live_window_root(cx: &mut TestAppContext) {
        // Mutation-sensitive: changing the production unknown-root arm back
        // to `continue` makes this call finish the platform quit with a live
        // window whose persistence authority is unknown. A safe menu must
        // leave that window alone and show an error instead.
        let profile = tempfile::tempdir().expect("temporary profile");
        cx.update(|app| {
            crate::components::init(app);
            let (_sender, receiver) = mpsc::channel();
            super::init(app, profile.path().to_path_buf(), receiver);
        });
        let (_unknown, mut visual) = cx.add_window_view(|_window, _cx| UnknownQuitRoot);
        visual.update(|window, app| window.draw(app).clear());

        visual
            .cx
            .update(|app| app.dispatch_action(&QuitApplication));
        visual.run_until_parked();

        assert_eq!(
            visual.cx.read(super::platform_quit_requests_for_test),
            0,
            "an unclassified live root must block process exit rather than silently discard its state"
        );
        let (title, detail) = visual
            .cx
            .pending_prompt()
            .expect("the blocked quit must be visible to the user");
        assert!(title.contains("无法安全退出"));
        assert!(detail.contains("窗口"));
    }

    #[gpui::test]
    async fn menu_quit_allows_the_explicit_startup_error_root(cx: &mut TestAppContext) {
        // The whitelist is intentionally narrow: a startup-error window owns
        // no LibraryShell/session, so it may close cleanly. Widening this to
        // arbitrary non-library roots would make the preceding unknown-root
        // safety contract meaningless.
        let profile = tempfile::tempdir().expect("temporary profile");
        cx.update(|app| {
            crate::components::init(app);
            let (_sender, receiver) = mpsc::channel();
            super::init(app, profile.path().to_path_buf(), receiver);
            ui::open_startup_error_window(app, "启动前故障".into())
                .expect("open known no-session root");
            app.dispatch_action(&QuitApplication);
        });
        cx.run_until_parked();

        assert_eq!(
            cx.read(super::platform_quit_requests_for_test),
            1,
            "the explicit StartupErrorView whitelist must not strand a no-session application"
        );
        assert!(
            !cx.has_pending_prompt(),
            "a known StartupErrorView is not an unknown persistence authority"
        );
    }

    #[gpui::test]
    async fn reentrant_same_window_shell_update_blocks_quit_instead_of_counting_as_ready(
        cx: &mut TestAppContext,
    ) {
        // `Window::dispatch_action` invokes the global action while its root
        // is temporarily absent from GPUI's window map. Exercise that same
        // nested-update shape directly: an inability to inspect this live
        // LibraryShell is a blocked quit, never evidence of a clean profile.
        // Replacing the production `Err(_) => Blocked` arm with `Ready`
        // makes this assertion fail and would reintroduce the native-menu
        // data-loss path.
        let profile = tempfile::tempdir().expect("temporary profile");
        let repository = Arc::new(
            LibraryRepository::open(profile.path().join("library.sqlite"))
                .expect("open temporary library"),
        );
        cx.update(|app| {
            crate::components::init(app);
            let (_sender, receiver) = mpsc::channel();
            super::init(app, profile.path().to_path_buf(), receiver);
        });
        let model = cx.new(|_| AppModel::open(repository).expect("open model"));
        let (_view, mut visual) = cx.add_window_view(move |window, cx| {
            LibraryShell::new_with_save_clock(
                model,
                None,
                Arc::new(ManualSaveClock::default()),
                window,
                cx,
            )
        });
        visual.update(|window, app| window.draw(app).clear());

        let status = visual.update(|_window, app| super::flush_library_windows_for_quit(app));

        assert_eq!(
            status,
            super::QuitFlushStatus::Blocked,
            "a same-window nested shell update must fail closed"
        );
        assert_eq!(
            visual.cx.read(super::platform_quit_requests_for_test),
            0,
            "the blocked inspection must not reach the platform quit helper"
        );
        assert!(
            visual
                .cx
                .windows()
                .into_iter()
                .any(|window| window.downcast::<StartupErrorView>().is_some()),
            "when the borrowed window cannot accept an in-place prompt, the user must still see the dedicated quit-safety error window"
        );
    }

    #[gpui::test]
    async fn startup_error_window_still_quits_when_it_is_the_last_window(cx: &mut TestAppContext) {
        // Catches installing the last-window lifecycle after profile/runtime
        // setup: an early StartupErrorView would otherwise leave the process
        // alive with no visible windows.
        cx.update(super::install_last_window_quit);
        let error = cx.update(|app| {
            ui::open_startup_error_window(app, "无法解析资料库目录".into())
                .expect("open visible startup error")
        });
        assert_eq!(
            cx.read(|app| app
                .global::<LibraryMenuLifecycle>()
                .quit_requests_for_test()),
            0
        );

        let _ = error.update(cx, |_view, window, _| window.remove_window());
        cx.update(|_| {});

        assert_eq!(
            cx.read(|app| app
                .global::<LibraryMenuLifecycle>()
                .quit_requests_for_test()),
            1,
            "closing the only startup error window must ask the app to quit"
        );
    }

    #[gpui::test]
    async fn failed_url_open_becomes_a_visible_startup_error(cx: &mut TestAppContext) {
        // Catches an open-url bridge that discards ui::open_library_window's
        // Result on a background task and leaves the user with no explanation.
        cx.update(super::install_last_window_quit);
        cx.update(|app| {
            super::present_open_request_result(app, Err("模拟资料库打开失败".into()))
                .expect("failure is rendered in a fallback window");
        });
        let error_window = cx
            .windows()
            .into_iter()
            .find_map(|window| window.downcast::<StartupErrorView>())
            .expect("URL open failure creates a visible StartupErrorView");
        let mut visual = VisualTestContext::from_window(error_window.into(), cx);
        visual.update(|window, app| window.draw(app).clear());
        assert!(visual.debug_bounds("library-startup-error").is_some());
    }
}
