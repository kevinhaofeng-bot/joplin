pub mod note_card;
pub mod note_list;
pub mod sidebar;

use crate::app::{AppAction, AppModel, AppStatus, CreateNote};
use crate::native_editor::{core::EditorCore, model::Document};
use app_lite_core::LibraryRepository;
use gpui::{
    App, AppContext, Context, Entity, InteractiveElement, IntoElement, KeyBinding, ParentElement,
    Render, Styled, Window, WindowBounds, WindowHandle, WindowOptions, div, px, rgba, size,
};
use std::path::PathBuf;
use std::sync::Arc;

pub struct LibraryShell {
    model: Entity<AppModel>,
    // Task 3 allocates the real editor core but intentionally does not attach a
    // save coordinator. The visible warning below makes that boundary explicit.
    #[allow(dead_code)]
    editor: Entity<EditorCore>,
}

pub fn open_library_window(
    cx: &mut App,
    profile: PathBuf,
) -> Result<WindowHandle<LibraryShell>, String> {
    std::fs::create_dir_all(&profile).map_err(|error| format!("无法创建资料库目录: {error}"))?;
    let repository = Arc::new(
        LibraryRepository::open(profile.join("library.sqlite"))
            .map_err(|error| format!("无法打开资料库: {error}"))?,
    );
    cx.bind_keys([KeyBinding::new("cmd-n", CreateNote, Some("LibraryShell"))]);
    let bounds = gpui::Bounds::centered(None, size(px(1160.0), px(760.0)), cx);
    cx.open_window(
        WindowOptions {
            window_bounds: Some(WindowBounds::Windowed(bounds)),
            ..WindowOptions::default()
        },
        move |_window, cx| {
            let model = cx.new(|_| {
                AppModel::open(repository)
                    .expect("repository was opened before window construction")
            });
            let editor = cx.new(|cx| EditorCore::new(Document::new(), cx));
            cx.new(|_| LibraryShell { model, editor })
        },
    )
    .map_err(|error| error.to_string())
}

impl LibraryShell {
    fn create_note(&mut self, _: &CreateNote, _window: &mut Window, cx: &mut Context<Self>) {
        let _ = self
            .model
            .update(cx, |model, _| model.dispatch(AppAction::CreateNote));
        cx.notify();
    }
}

impl Render for LibraryShell {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let (items, selected, panes, status) = self.model.read_with(cx, |model, _| {
            (
                model.projections().to_vec(),
                model.navigation().selected_note_id().cloned(),
                model.panes(),
                model.status().clone(),
            )
        });
        let editor = if items.is_empty() {
            let model = self.model.clone();
            div()
                .flex_1()
                .flex()
                .flex_col()
                .items_center()
                .justify_center()
                .gap(px(14.0))
                .child(div().text_size(px(22.0)).child("从第一篇笔记开始"))
                .child(
                    div()
                        .text_color(rgba(0x718075ff))
                        .child("资料库为空，所有内容都将保存在此设备。"),
                )
                .child(
                    div()
                        .id("create-first-note")
                        .px(px(16.0))
                        .py(px(10.0))
                        .rounded(px(7.0))
                        .bg(rgba(0x00a82dff))
                        .text_color(rgba(0xffffffff))
                        .cursor_pointer()
                        .on_mouse_down(gpui::MouseButton::Left, move |_event, _window, cx| {
                            let _ =
                                model.update(cx, |model, _| model.dispatch(AppAction::CreateNote));
                        })
                        .child("新建第一篇笔记"),
                )
        } else {
            let label = selected
                .as_ref()
                .and_then(|id| items.iter().find(|item| &item.id == id))
                .map(|item| {
                    if item.title_prefix.is_empty() {
                        "无标题笔记".to_owned()
                    } else {
                        item.title_prefix.clone()
                    }
                })
                .unwrap_or_else(|| "选择一篇笔记".into());
            div().flex_1().p(px(36.0)).child(div().text_size(px(26.0)).text_color(rgba(0x202720ff)).child(label)).child(div().mt(px(18.0)).text_color(rgba(0x718075ff)).child("编辑器已接入原生会话边界；自动保存将在下一阶段启用。为避免静默丢失，本阶段不把编辑误报为已保存。"))
        };
        let status_text = match status {
            AppStatus::Ready => String::new(),
            AppStatus::Error(error) => format!("资料库错误：{error}"),
        };
        div()
            .size_full()
            .flex()
            .key_context("LibraryShell")
            .on_action(cx.listener(Self::create_note))
            .child(sidebar::render(panes.sidebar_visible))
            .child(note_list::render(
                &items,
                selected.as_ref(),
                self.model.clone(),
                panes.list_visible,
            ))
            .child(editor)
            .child(
                div()
                    .absolute()
                    .bottom(px(10.0))
                    .right(px(14.0))
                    .text_size(px(11.0))
                    .text_color(rgba(0xa34838ff))
                    .child(status_text),
            )
    }
}
