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
use crate::file_url::parse_file_url;
use crate::ui::{self, LibraryShell};
use gpui::{App, Global, Menu, MenuItem, Subscription, Task, actions};
use std::path::PathBuf;
use std::sync::mpsc::Receiver;
use std::time::Duration;

actions!(library_menu, [QuitLibrary]);

#[derive(Default)]
struct LibraryMenuLifecycle {
    close_subscription: Option<Subscription>,
    open_url_task: Option<Task<()>>,
    #[cfg(test)]
    quit_requests: usize,
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
            #[cfg(test)]
            {
                cx.global_mut::<LibraryMenuLifecycle>().quit_requests += 1;
            }
            cx.quit();
        }
    });
    cx.global_mut::<LibraryMenuLifecycle>().close_subscription = Some(close_subscription);
}

#[cfg(test)]
impl LibraryMenuLifecycle {
    fn quit_requests_for_test(&self) -> usize {
        self.quit_requests
    }
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

    cx.on_action(|_: &QuitLibrary, cx| request_quit_library(cx));
    cx.set_menus(vec![Menu {
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
            MenuItem::action("退出 Joplin Lite", QuitLibrary),
        ],
    }]);

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

/// The app menu can receive Cmd-Q while a library editor has focus. Ask each
/// mounted library shell to complete its exact current generation first; if a
/// snapshot fails, the shell stays open and renders the local-save error.
pub(crate) fn request_quit_library(cx: &mut App) {
    for window in cx.windows() {
        let Some(shell) = window.downcast::<LibraryShell>() else {
            continue;
        };
        let flushed = shell
            .update(cx, |shell, _window, shell_cx| {
                shell.flush_for_lifecycle(FlushReason::Quit, shell_cx)
            })
            .unwrap_or(false);
        if !flushed {
            return;
        }
    }
    cx.quit();
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
    use super::{LibraryMenuLifecycle, import_notice};
    use crate::ui::{self, StartupErrorView};
    use gpui::{TestAppContext, VisualTestContext};
    use std::path::PathBuf;

    #[test]
    fn import_notice_is_visible_and_promises_no_source_mutation() {
        let notice = import_notice(&[PathBuf::from("/tmp/one.md")]);
        assert!(notice.contains("暂不支持导入"));
        assert!(notice.contains("未被读取或修改"));
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
