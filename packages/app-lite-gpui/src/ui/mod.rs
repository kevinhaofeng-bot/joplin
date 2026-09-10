pub mod note_card;
pub mod note_list;
pub mod sidebar;

use crate::app::{
    AppAction, AppModel, AppStatus, CreateNote, CycleListViewMode, CycleSort, ListViewMode,
    NoteSort, ToggleNoteList, ToggleSidebar, TrashSelected,
};
use crate::native_editor::codec::import_canonical;
use crate::native_editor::core::EditorCore;
use crate::native_editor::surface::{EditorSurface, EditorSurfaceMode};
use app_lite_core::{CanonicalDocument, LibraryRepository, Note, NoteId, NoteProjection};
use gpui::{
    App, AppContext, Context, Entity, InteractiveElement, IntoElement, KeyBinding, MouseButton,
    ParentElement, Render, ScrollStrategy, Styled, Subscription, Task, UniformListScrollHandle,
    Window, WindowBounds, WindowHandle, WindowOptions, div, px, rgba, size, uniform_list,
};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::mpsc::Receiver;
use std::time::Duration;

/// The product library shell. All user operations arrive at `apply_action`,
/// which is also the only place that mutates the model from UI callbacks.
pub struct LibraryShell {
    model: Entity<AppModel>,
    editor_surface: Option<Entity<EditorSurface>>,
    surface_note_id: Option<NoteId>,
    unsupported_document: Option<String>,
    startup_notice: Option<String>,
    note_list_scroll: UniformListScrollHandle,
    _model_observation: Subscription,
    // Held by the entity so GPUI cancels the receiver loop when this window is
    // destroyed. The task captures only a WeakEntity and never blocks on recv.
    _event_task: Task<()>,
    #[cfg(test)]
    rendered_note_range: Option<std::ops::Range<usize>>,
    #[cfg(test)]
    last_scroll_request: Option<usize>,
}

pub fn open_library_window(
    cx: &mut App,
    profile: PathBuf,
) -> Result<WindowHandle<LibraryShell>, String> {
    open_library_window_with_notice(cx, profile, None)
}

/// Open the local-library shell. `startup_notice` is deliberately rendered in
/// the window instead of stderr so platform file-open/import requests remain
/// truthful and visible to the person using the app.
pub(crate) fn open_library_window_with_notice(
    cx: &mut App,
    profile: PathBuf,
    startup_notice: Option<String>,
) -> Result<WindowHandle<LibraryShell>, String> {
    std::fs::create_dir_all(&profile).map_err(|error| format!("无法创建资料库目录: {error}"))?;
    let repository = Arc::new(
        LibraryRepository::open(profile.join("library.sqlite"))
            .map_err(|error| format!("无法打开资料库: {error}"))?,
    );
    // Construct all fallible product state before the window factory. A
    // corrupted profile must become a visible startup error, not a panic in a
    // deferred GPUI closure.
    let model_state =
        AppModel::open(repository).map_err(|error| format!("无法读取资料库: {error}"))?;
    let model = cx.new(|_| model_state);
    cx.bind_keys([
        KeyBinding::new("cmd-n", CreateNote, Some("LibraryShell")),
        KeyBinding::new("cmd-shift-backspace", TrashSelected, Some("LibraryShell")),
        KeyBinding::new("cmd-alt-s", ToggleSidebar, Some("LibraryShell")),
        KeyBinding::new("cmd-alt-l", ToggleNoteList, Some("LibraryShell")),
        KeyBinding::new("cmd-alt-v", CycleListViewMode, Some("LibraryShell")),
        KeyBinding::new("cmd-alt-o", CycleSort, Some("LibraryShell")),
    ]);
    let bounds = gpui::Bounds::centered(None, size(px(1160.0), px(760.0)), cx);
    cx.open_window(
        WindowOptions {
            window_bounds: Some(WindowBounds::Windowed(bounds)),
            ..WindowOptions::default()
        },
        move |_window, cx| {
            let model = model.clone();
            let startup_notice = startup_notice.clone();
            cx.new(move |cx| LibraryShell::new(model, startup_notice, cx))
        },
    )
    .map_err(|error| error.to_string())
}

impl LibraryShell {
    fn new(
        model: Entity<AppModel>,
        startup_notice: Option<String>,
        cx: &mut Context<Self>,
    ) -> Self {
        let observation = cx.observe(&model, |shell, _, cx| {
            shell.sync_editor_surface(cx);
            cx.notify();
        });
        let event_receiver = model.read(cx).subscribe_library_events();
        let event_task = Self::spawn_event_bridge(event_receiver, cx);
        let mut shell = Self {
            model,
            editor_surface: None,
            surface_note_id: None,
            unsupported_document: None,
            startup_notice,
            note_list_scroll: UniformListScrollHandle::new(),
            _model_observation: observation,
            _event_task: event_task,
            #[cfg(test)]
            rendered_note_range: None,
            #[cfg(test)]
            last_scroll_request: None,
        };
        shell.sync_editor_surface(cx);
        shell
    }

    fn spawn_event_bridge(
        receiver: Receiver<app_lite_core::LibraryEvent>,
        cx: &mut Context<Self>,
    ) -> Task<()> {
        cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor()
                    .timer(Duration::from_millis(50))
                    .await;
                // `try_iter` is non-blocking; the explicit cap prevents a
                // noisy importer/sync burst from monopolizing a UI turn.
                let events = receiver.try_iter().take(128).collect::<Vec<_>>();
                if events.is_empty() {
                    continue;
                }
                let _ = this.update(cx, |shell, shell_cx| {
                    let refresh = shell.model.update(shell_cx, |model, model_cx| {
                        let refresh = model.refresh_projection_events(events);
                        model_cx.notify();
                        refresh
                    });
                    if matches!(refresh, Ok(true)) {
                        shell.sync_editor_surface(shell_cx);
                        shell.scroll_selected_into_view(shell_cx);
                    }
                    shell_cx.notify();
                });
            }
        })
    }

    /// The single UI reducer seam. It notifies the model even on errors, then
    /// lets the retained model observer redraw the shell. A direct card/button
    /// update of `AppModel` is deliberately impossible from this module.
    pub(crate) fn apply_action(
        &mut self,
        action: AppAction,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let result = self.model.update(cx, |model, model_cx| {
            let result = model.dispatch(action);
            model_cx.notify();
            result
        });
        if result.is_ok() {
            self.sync_editor_surface(cx);
            self.scroll_selected_into_view(cx);
        }
        // The action failure is retained in AppModel::status and rendered below.
        cx.notify();
    }

    fn create_note(&mut self, _: &CreateNote, window: &mut Window, cx: &mut Context<Self>) {
        self.apply_action(AppAction::CreateNote, window, cx);
    }

    fn trash_selected(&mut self, _: &TrashSelected, window: &mut Window, cx: &mut Context<Self>) {
        self.apply_action(AppAction::TrashSelected, window, cx);
    }

    fn toggle_sidebar(&mut self, _: &ToggleSidebar, window: &mut Window, cx: &mut Context<Self>) {
        self.apply_action(AppAction::ToggleSidebar, window, cx);
    }

    fn toggle_note_list(
        &mut self,
        _: &ToggleNoteList,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.apply_action(AppAction::ToggleNoteList, window, cx);
    }

    fn cycle_list_view_mode(
        &mut self,
        _: &CycleListViewMode,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let next = self
            .model
            .read_with(cx, |model, _| model.list_view_mode().next());
        self.apply_action(AppAction::SetListViewMode(next), window, cx);
    }

    fn cycle_sort(&mut self, _: &CycleSort, window: &mut Window, cx: &mut Context<Self>) {
        let next = self.model.read_with(cx, |model, _| model.sort().next());
        self.apply_action(AppAction::SetSort(next), window, cx);
    }

    fn sync_editor_surface(&mut self, cx: &mut Context<Self>) {
        let note = self
            .model
            .read_with(cx, |model, _| model.active_note().cloned());
        let next_id = note.as_ref().map(|note| note.id.clone());
        if self.surface_note_id == next_id {
            return;
        }

        // A switch (including a codec failure) first removes the old mounted
        // entity. It is never possible to show one note under another note's
        // selection/title.
        self.editor_surface = None;
        self.surface_note_id = next_id.clone();
        self.unsupported_document = None;

        let Some(note) = note else {
            return;
        };
        let native_document = import_note_body(&note);
        match native_document {
            Ok(document) => {
                let editor = cx.new(|cx| EditorCore::new_read_only(document, cx));
                self.editor_surface =
                    Some(cx.new(|cx| {
                        EditorSurface::new(editor, EditorSurfaceMode::ReadOnly, None, cx)
                    }));
            }
            Err(error) => self.unsupported_document = Some(error),
        }
    }

    fn scroll_selected_into_view(&mut self, cx: &mut Context<Self>) {
        let index = self.model.read_with(cx, |model, _| {
            model.navigation().selected_note_id().and_then(|selected| {
                model
                    .projections()
                    .iter()
                    .position(|projection| &projection.id == selected)
            })
        });
        if let Some(index) = index {
            #[cfg(test)]
            {
                self.last_scroll_request = Some(index);
            }
            self.note_list_scroll
                .scroll_to_item(index, ScrollStrategy::Center);
        }
    }

    fn render_note_list(
        &mut self,
        items: Vec<NoteProjection>,
        selected: Option<NoteId>,
        list_width: u16,
        visible: bool,
        mode: ListViewMode,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        if !visible {
            return div()
                .id("note-list-hidden")
                .debug_selector(|| "library-note-list".to_owned())
                .w(px(0.0))
                .h_full()
                .flex_none()
                .into_any_element();
        }
        let shell = cx.weak_entity();
        let processor_shell = shell.clone();
        let selected_for_processor = selected.clone();
        let item_count = items.len();
        let list = uniform_list(
            "library-note-list-items",
            item_count,
            cx.processor(move |_this, range: std::ops::Range<usize>, _window, _cx| {
                #[cfg(test)]
                {
                    _this.rendered_note_range = Some(range.clone());
                }
                note_list::record_constructed_items(range.len());
                range
                    .filter_map(|index| {
                        let projection = items.get(index)?.clone();
                        let id = projection.id.clone();
                        let selected = selected_for_processor.as_ref() == Some(&id);
                        let click_shell = processor_shell.clone();
                        Some(
                            div()
                                .id(("library-note-list-row", index))
                                .h(px(note_list::fixed_card_height(mode)))
                                .px(px(6.0))
                                .cursor_pointer()
                                .on_mouse_down(MouseButton::Left, move |_event, window, cx| {
                                    let _ = click_shell.update(cx, |shell, cx| {
                                        shell.apply_action(
                                            AppAction::SelectNote(id.clone()),
                                            window,
                                            cx,
                                        );
                                    });
                                })
                                .child(note_card::render(&projection, selected, mode)),
                        )
                    })
                    .collect::<Vec<_>>()
            }),
        )
        .size_full()
        .track_scroll(self.note_list_scroll.clone());
        div()
            .id("note-list")
            .debug_selector(|| "library-note-list".to_owned())
            .w(px(f32::from(list_width)))
            .h_full()
            .flex_none()
            .overflow_hidden()
            .py(px(8.0))
            .bg(rgba(0xffffffff))
            .border_r_1()
            .border_color(rgba(0xe1e5e1ff))
            .child(list)
            .into_any_element()
    }

    fn render_editor_panel(
        &self,
        items_empty: bool,
        note: Option<Note>,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        if items_empty {
            return div()
                .id("library-empty-state")
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
                        .debug_selector(|| "create-first-note".to_owned())
                        .px(px(16.0))
                        .py(px(10.0))
                        .rounded(px(7.0))
                        .bg(rgba(0x00a82dff))
                        .text_color(rgba(0xffffffff))
                        .cursor_pointer()
                        .on_mouse_down(
                            MouseButton::Left,
                            cx.listener(|shell, _event, window, cx| {
                                shell.apply_action(AppAction::CreateNote, window, cx)
                            }),
                        )
                        .child("新建第一篇笔记"),
                )
                .into_any_element();
        }
        if let Some(error) = &self.unsupported_document {
            let title = note.map_or_else(|| "无法显示笔记".to_owned(), |note| note.title);
            return div()
                .id("unsupported-native-document")
                .debug_selector(|| "unsupported-native-document".to_owned())
                .flex_1()
                .p(px(36.0))
                .child(div().text_size(px(24.0)).child(title))
                .child(
                    div()
                        .mt(px(18.0))
                        .text_color(rgba(0xa34838ff))
                        .child(format!("此笔记保持原样，暂不能在原生预览中显示：{error}")),
                )
                .into_any_element();
        }
        if let Some(surface) = &self.editor_surface {
            return div()
                .id("library-native-editor-pane")
                .flex_1()
                .min_w(px(1.0))
                .h_full()
                .child(surface.clone())
                .into_any_element();
        }
        div()
            .id("library-no-selection")
            .flex_1()
            .p(px(36.0))
            .text_color(rgba(0x718075ff))
            .child("选择一篇笔记以查看正文")
            .into_any_element()
    }
}

impl Render for LibraryShell {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let (items, selected, panes, status, mode, sort, active_note) =
            self.model.read_with(cx, |model, _| {
                (
                    model.projections().to_vec(),
                    model.navigation().selected_note_id().cloned(),
                    model.panes(),
                    model.status().clone(),
                    model.list_view_mode(),
                    model.sort(),
                    model.active_note().cloned(),
                )
            });
        let status_text = match status {
            AppStatus::Ready => String::new(),
            AppStatus::Error(error) => format!("资料库错误：{error}"),
        };
        let editor = self.render_editor_panel(items.is_empty(), active_note, cx);
        let note_list = self.render_note_list(
            items.clone(),
            selected,
            panes.list_width,
            panes.list_visible,
            mode,
            cx,
        );
        let mode_label = match mode {
            ListViewMode::Cards => "卡片",
            ListViewMode::Snippets => "摘要",
            ListViewMode::Compact => "紧凑",
        };
        let sort_label = match sort {
            NoteSort::UpdatedDescending => "按更新时间",
            NoteSort::TitleAscending => "按标题 A-Z",
            NoteSort::TitleDescending => "按标题 Z-A",
        };
        let toolbar = div()
            .id("library-actions")
            .h(px(42.0))
            .flex_none()
            .flex()
            .items_center()
            .gap(px(8.0))
            .px(px(12.0))
            .border_b_1()
            .border_color(rgba(0xe1e5e1ff))
            .children([
                library_action_button(
                    "library-create-note",
                    "新建".to_owned(),
                    AppAction::CreateNote,
                    cx,
                ),
                library_action_button(
                    "library-trash-selected",
                    "移至废纸篓".to_owned(),
                    AppAction::TrashSelected,
                    cx,
                ),
                library_action_button(
                    "library-toggle-sidebar",
                    "侧栏".to_owned(),
                    AppAction::ToggleSidebar,
                    cx,
                ),
                library_action_button(
                    "library-toggle-list",
                    "笔记列表".to_owned(),
                    AppAction::ToggleNoteList,
                    cx,
                ),
                library_action_button(
                    "library-cycle-view",
                    format!("视图：{mode_label}"),
                    AppAction::SetListViewMode(mode.next()),
                    cx,
                ),
                library_action_button(
                    "library-cycle-sort",
                    sort_label.to_owned(),
                    AppAction::SetSort(sort.next()),
                    cx,
                ),
            ]);
        div()
            .id("library-shell")
            .size_full()
            .relative()
            .flex()
            .key_context("LibraryShell")
            .on_action(cx.listener(Self::create_note))
            .on_action(cx.listener(Self::trash_selected))
            .on_action(cx.listener(Self::toggle_sidebar))
            .on_action(cx.listener(Self::toggle_note_list))
            .on_action(cx.listener(Self::cycle_list_view_mode))
            .on_action(cx.listener(Self::cycle_sort))
            .child(sidebar::render(panes.sidebar_width, panes.sidebar_visible))
            .child(note_list)
            .child(
                div()
                    .flex_1()
                    .min_w(px(1.0))
                    .h_full()
                    .flex()
                    .flex_col()
                    .child(toolbar)
                    .child(editor),
            )
            .child(
                div()
                    .absolute()
                    .bottom(px(10.0))
                    .right(px(14.0))
                    .text_size(px(11.0))
                    .text_color(rgba(0xa34838ff))
                    .child(status_text),
            )
            .children(self.startup_notice.as_ref().map(|notice| {
                div()
                    .id("library-startup-notice")
                    .absolute()
                    .top(px(52.0))
                    .right(px(16.0))
                    .max_w(px(440.0))
                    .p(px(12.0))
                    .rounded(px(7.0))
                    .bg(rgba(0xfff6dfff))
                    .text_color(rgba(0x725c19ff))
                    .child(notice.clone())
            }))
    }
}

/// A fallible bootstrap has no product model to render. Keep the error in a
/// minimal ordinary GPUI window rather than panicking inside a deferred window
/// factory or printing a message which can disappear when launched by Finder.
pub(crate) struct StartupErrorView {
    message: String,
}

impl Render for StartupErrorView {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .id("library-startup-error")
            .size_full()
            .p(px(32.0))
            .flex()
            .flex_col()
            .gap(px(12.0))
            .bg(rgba(0xffffffff))
            .child(div().text_size(px(23.0)).child("无法启动 Joplin Lite"))
            .child(
                div()
                    .text_color(rgba(0xa34838ff))
                    .child(self.message.clone()),
            )
            .child(
                div()
                    .text_color(rgba(0x718075ff))
                    .child("请检查资料库路径或系统目录权限后重试。"),
            )
    }
}

pub(crate) fn open_startup_error_window(
    cx: &mut App,
    message: String,
) -> Result<WindowHandle<StartupErrorView>, String> {
    let bounds = gpui::Bounds::centered(None, size(px(520.0), px(240.0)), cx);
    cx.open_window(
        WindowOptions {
            window_bounds: Some(WindowBounds::Windowed(bounds)),
            ..WindowOptions::default()
        },
        move |_window, cx| cx.new(|_| StartupErrorView { message }),
    )
    .map_err(|error| error.to_string())
}

fn library_action_button(
    id: &'static str,
    label: String,
    action: AppAction,
    cx: &mut Context<LibraryShell>,
) -> gpui::Stateful<gpui::Div> {
    div()
        .id(id)
        .px(px(8.0))
        .py(px(5.0))
        .rounded(px(5.0))
        .bg(rgba(0xf1f4f1ff))
        .text_size(px(12.0))
        .text_color(rgba(0x36413aff))
        .cursor_pointer()
        .on_mouse_down(
            MouseButton::Left,
            cx.listener(move |shell, _event, window, cx| {
                shell.apply_action(action.clone(), window, cx)
            }),
        )
        .child(label)
}

fn import_note_body(note: &Note) -> Result<crate::native_editor::model::Document, String> {
    CanonicalDocument::parse_html(&note.body_html)
        .map_err(|error| format!("无法解析已保存的正文: {error}"))
        .and_then(|document| import_canonical(&document).map_err(|error| error.to_string()))
}

#[cfg(test)]
mod tests;
