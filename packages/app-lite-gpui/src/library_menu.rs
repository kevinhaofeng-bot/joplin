//! Narrow application-level bootstrap for the local-library product route.
//!
//! This intentionally does not reuse the old Velotype file-editor menu: its
//! file, updater, export and workspace actions would silently open a second
//! product. The common lifecycle/menu infrastructure is retained here with
//! library-native actions only.

use crate::app::{
    CreateNote, CycleListViewMode, CycleSort, ToggleNoteList, ToggleSidebar, TrashSelected,
};
use crate::file_url::parse_file_url;
use crate::ui;
use gpui::{App, Global, Menu, MenuItem, Subscription, Task, actions};
use std::path::PathBuf;
use std::sync::mpsc::Receiver;
use std::time::Duration;

actions!(library_menu, [QuitLibrary]);

#[derive(Default)]
struct LibraryMenuLifecycle {
    close_subscription: Option<Subscription>,
    open_url_task: Option<Task<()>>,
}

impl Global for LibraryMenuLifecycle {}

pub(crate) fn init(cx: &mut App, profile: PathBuf, open_url_receiver: Receiver<Vec<String>>) {
    cx.set_global(LibraryMenuLifecycle::default());
    let close_subscription = cx.on_window_closed(|cx| {
        if cx.windows().is_empty() {
            cx.quit();
        }
    });
    cx.global_mut::<LibraryMenuLifecycle>().close_subscription = Some(close_subscription);

    cx.on_action(|_: &QuitLibrary, cx| cx.quit());
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
            let _ = cx.update(|cx| {
                let _ = ui::open_library_window_with_notice(cx, profile, Some(notice));
                cx.activate(true);
                cx.refresh_windows();
            });
        }
    });
    cx.global_mut::<LibraryMenuLifecycle>().open_url_task = Some(task);
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
    use super::import_notice;
    use std::path::PathBuf;

    #[test]
    fn import_notice_is_visible_and_promises_no_source_mutation() {
        let notice = import_notice(&[PathBuf::from("/tmp/one.md")]);
        assert!(notice.contains("暂不支持导入"));
        assert!(notice.contains("未被读取或修改"));
    }
}
