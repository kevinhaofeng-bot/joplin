pub mod note_card;
pub mod note_list;
pub mod sidebar;

use crate::app::note_session::NoteSession;
use crate::app::save_coordinator::{FlushReason, SaveState, SystemSaveClock};
use crate::app::{
    AppAction, AppModel, AppStatus, CreateNote, CycleListViewMode, CycleSort, ListViewMode,
    NoteSort, SyncCurrent, ToggleNoteList, ToggleSidebar, TrashSelected,
};
use crate::native_editor::chrome::{EVERNOTE_GREEN, TitleInput};
#[cfg(test)]
use crate::native_editor::codec::import_canonical;
#[cfg(test)]
use crate::native_editor::core::EditorCore;
#[cfg(test)]
use crate::native_editor::surface::EditorSurfaceHooks;
use crate::native_editor::surface::focus_editor;
use crate::native_editor::surface::{EditorSurface, EditorSurfaceMode};
#[cfg(test)]
use app_lite_core::CanonicalDocument;
use app_lite_core::{LibraryRepository, Note, NoteId, NoteProjection};
use gpui::{
    App, AppContext, Bounds, ClipboardItem, Context, ElementInputHandler, Entity, FocusHandle,
    FontWeight, InteractiveElement, IntoElement, KeyBinding, KeyDownEvent, MouseButton,
    MouseDownEvent, MouseMoveEvent, MouseUpEvent, ParentElement, Render, ScrollStrategy,
    SharedString, Styled, Subscription, Task, TextRun, UniformListScrollHandle, Window,
    WindowBounds, WindowHandle, WindowOptions, canvas, div, point, px, rgba, size, uniform_list,
};
use std::path::PathBuf;
use std::sync::Arc;
#[cfg(test)]
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc::Receiver;
#[cfg(test)]
use std::sync::mpsc::{self, Sender};
use std::time::Duration;

#[derive(Clone, Debug, PartialEq, Eq)]
enum ShellSaveError {
    Automatic { generation: i64, message: String },
    Lifecycle { message: String },
}

impl ShellSaveError {
    fn message(&self) -> &str {
        match self {
            Self::Automatic { message, .. } | Self::Lifecycle { message } => message,
        }
    }
}

/// The product library shell. All user operations arrive at `apply_action`,
/// which is also the only place that mutates the model from UI callbacks.
pub struct LibraryShell {
    model: Entity<AppModel>,
    note_session: Option<Entity<NoteSession>>,
    _note_session_observation: Option<Subscription>,
    editor_surface: Option<Entity<EditorSurface>>,
    surface_note_id: Option<NoteId>,
    unsupported_document: Option<String>,
    save_error: Option<ShellSaveError>,
    /// True only for a lifecycle boundary that has started a background
    /// snapshot. IME/canonical failures stay visible until an explicit
    /// successful boundary, whereas this transient blocker clears on the
    /// completion-confirmed Clean transition.
    save_pending: bool,
    startup_notice: Option<String>,
    save_clock: Arc<dyn crate::app::save_coordinator::SaveClock>,
    focus_handle: FocusHandle,
    note_list_scroll: UniformListScrollHandle,
    _model_observation: Subscription,
    // Held by the entity so GPUI cancels the receiver loop when this window is
    // destroyed. The task captures only a WeakEntity and never blocks on recv.
    _event_task: Task<()>,
    #[cfg(test)]
    event_task_cancellation_receiver: Option<Receiver<()>>,
    #[cfg(test)]
    rendered_note_range: Option<std::ops::Range<usize>>,
    #[cfg(test)]
    last_scroll_request: Option<usize>,
    #[cfg(test)]
    library_surface_paint_hooks_for_test: Arc<LibrarySurfacePaintHooks>,
}

#[cfg(test)]
#[derive(Default)]
struct LibrarySurfacePaintHooks {
    shape: AtomicUsize,
    paint: AtomicUsize,
}

/// A task-local lifetime guard. In production it is deliberately zero-sized;
/// tests attach a receiver so they can prove that dropping the retained `Task`
/// cancels the future immediately instead of merely letting a later weak-entity
/// update notice the closed window.
struct EventTaskLifetime {
    #[cfg(test)]
    cancellation_sender: Option<Sender<()>>,
}

impl EventTaskLifetime {
    #[cfg(test)]
    fn observed() -> (Self, Receiver<()>) {
        let (cancellation_sender, cancellation_receiver) = mpsc::channel();
        (
            Self {
                cancellation_sender: Some(cancellation_sender),
            },
            cancellation_receiver,
        )
    }

    #[cfg(not(test))]
    const fn unobserved() -> Self {
        Self {}
    }
}

impl Drop for EventTaskLifetime {
    fn drop(&mut self) {
        #[cfg(test)]
        if let Some(cancellation_sender) = self.cancellation_sender.take() {
            let _ = cancellation_sender.send(());
        }
    }
}

fn bind_library_keybindings(cx: &mut App) {
    cx.bind_keys([
        KeyBinding::new("cmd-n", CreateNote, Some("LibraryShell")),
        KeyBinding::new("cmd-shift-backspace", TrashSelected, Some("LibraryShell")),
        KeyBinding::new("cmd-alt-s", ToggleSidebar, Some("LibraryShell")),
        KeyBinding::new("cmd-alt-l", ToggleNoteList, Some("LibraryShell")),
        KeyBinding::new("cmd-alt-v", CycleListViewMode, Some("LibraryShell")),
        KeyBinding::new("cmd-alt-o", CycleSort, Some("LibraryShell")),
        KeyBinding::new("cmd-s", SyncCurrent, Some("LibraryShell")),
    ]);
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
    bind_library_keybindings(cx);
    let bounds = gpui::Bounds::centered(None, size(px(1160.0), px(760.0)), cx);
    cx.open_window(
        WindowOptions {
            window_bounds: Some(WindowBounds::Windowed(bounds)),
            ..WindowOptions::default()
        },
        move |window, cx| {
            let model = model.clone();
            let startup_notice = startup_notice.clone();
            cx.new(move |cx| LibraryShell::new(model, startup_notice, window, cx))
        },
    )
    .map_err(|error| error.to_string())
}

impl LibraryShell {
    fn new(
        model: Entity<AppModel>,
        startup_notice: Option<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        Self::new_with_save_clock(
            model,
            startup_notice,
            Arc::new(SystemSaveClock::default()),
            window,
            cx,
        )
    }

    /// The production constructor supplies a monotonic clock above. Keeping
    /// the clock injectable lets the mounted acceptance tests advance exact
    /// journal/snapshot boundaries without sleeps or a test-only save path.
    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) fn new_with_save_clock(
        model: Entity<AppModel>,
        startup_notice: Option<String>,
        save_clock: Arc<dyn crate::app::save_coordinator::SaveClock>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let focus_handle = cx.focus_handle();
        focus_handle.focus(window);
        let observation = cx.observe(&model, |shell, _, cx| {
            shell.sync_editor_surface(cx);
            shell.scroll_selected_into_view(cx);
            cx.notify();
        });
        // GPUI asks this hook *before* it tears down the window. A failed
        // local snapshot leaves the retained shell alive with its visible
        // blocking error instead of allowing a close to discard a journaled
        // edit. The weak entity also means this callback cannot retain a
        // closed library window by itself.
        let close_shell = cx.weak_entity();
        window.on_window_should_close(cx, move |_window, app| {
            close_shell
                .update(app, |shell, shell_cx| {
                    shell.flush_active_session(FlushReason::WindowClose, shell_cx)
                })
                .unwrap_or(true)
        });
        let event_receiver = model.read(cx).subscribe_library_events();
        #[cfg(test)]
        let (event_task_lifetime, event_task_cancellation_receiver) = EventTaskLifetime::observed();
        #[cfg(not(test))]
        let event_task_lifetime = EventTaskLifetime::unobserved();
        let event_task = Self::spawn_event_bridge(event_receiver, event_task_lifetime, cx);
        let mut shell = Self {
            model,
            note_session: None,
            _note_session_observation: None,
            editor_surface: None,
            surface_note_id: None,
            unsupported_document: None,
            save_error: None,
            save_pending: false,
            startup_notice,
            save_clock,
            focus_handle,
            note_list_scroll: UniformListScrollHandle::new(),
            _model_observation: observation,
            _event_task: event_task,
            #[cfg(test)]
            event_task_cancellation_receiver: Some(event_task_cancellation_receiver),
            #[cfg(test)]
            rendered_note_range: None,
            #[cfg(test)]
            last_scroll_request: None,
            #[cfg(test)]
            library_surface_paint_hooks_for_test: Arc::new(LibrarySurfacePaintHooks::default()),
        };
        shell.sync_editor_surface(cx);
        // UniformListScrollHandle retains this request until its first
        // `track_scroll` mount, so a restored selection can reach a distant
        // card before the window has drawn for the first time.
        shell.scroll_selected_into_view(cx);
        shell
    }

    fn spawn_event_bridge(
        receiver: Receiver<app_lite_core::LibraryEvent>,
        event_task_lifetime: EventTaskLifetime,
        cx: &mut Context<Self>,
    ) -> Task<()> {
        cx.spawn(async move |this, cx| {
            // Keep the guard inside the cancellable future, not on the shell.
            // When `_event_task` drops, this guard drops in the same turn.
            let _event_task_lifetime = event_task_lifetime;
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
                if this
                    .update(cx, |shell, shell_cx| {
                        let _ = shell.model.update(shell_cx, |model, model_cx| {
                            let _ = model.refresh_projection_events(events);
                            model_cx.notify();
                        });
                    })
                    .is_err()
                {
                    // The shell owns the task, but stop promptly as well if
                    // an already-queued timer wakes after its entity died.
                    break;
                }
            }
        })
    }

    fn poll_active_session(&mut self, cx: &mut Context<Self>) {
        let Some(session) = self.note_session.clone() else {
            return;
        };
        let result = session.update(cx, |session, session_cx| session.poll(session_cx));
        match result {
            // A clean timer only says there is no currently-due automatic
            // work. It must never erase a lifecycle blocker while macOS still
            // owns marked IME text. `flush_active_session` is the explicit
            // successful boundary that clears this visible warning.
            Ok(()) => {}
            Err(error) => {
                self.save_error = Some(ShellSaveError::Automatic {
                    generation: session.read(cx).save_generation(),
                    message: format!("自动保存失败：{error}"),
                });
            }
        }
        cx.notify();
    }

    #[cfg(test)]
    fn take_event_task_cancellation_receiver_for_test(&mut self) -> Receiver<()> {
        self.event_task_cancellation_receiver
            .take()
            .expect("event task cancellation receiver is taken only once per test")
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
        if let Some(reason) = self.flush_reason_for_action(&action)
            && !self.flush_active_session(reason, cx)
        {
            return;
        }
        let _ = self.model.update(cx, |model, model_cx| {
            let _ = model.dispatch(action);
            model_cx.notify();
        });
        // The retained model observer owns surface synchronization, deferred
        // scrolling, and shell invalidation. Keeping this reducer to model
        // mutation plus notification makes every user and event route share
        // the exact same visible-state bridge.
    }

    fn flush_reason_for_action(&self, action: &AppAction) -> Option<FlushReason> {
        let active_id = self.surface_note_id.clone();
        match action {
            AppAction::CreateNote => active_id.map(|_| FlushReason::NoteSwitch),
            AppAction::SelectNote(id) if active_id.as_ref() != Some(id) => {
                active_id.map(|_| FlushReason::NoteSwitch)
            }
            AppAction::TrashSelected => active_id.map(|_| FlushReason::Delete),
            AppAction::TrashNote(id) if active_id.as_ref() == Some(id) => Some(FlushReason::Delete),
            AppAction::ManualSync => active_id.map(|_| FlushReason::ManualSync),
            _ => None,
        }
    }

    fn flush_active_session(&mut self, reason: FlushReason, cx: &mut Context<Self>) -> bool {
        let Some(session) = self.note_session.clone() else {
            return true;
        };
        match session.update(cx, |session, session_cx| session.flush(reason, session_cx)) {
            Ok(_) => {
                self.save_error = None;
                self.save_pending = false;
                cx.notify();
                true
            }
            Err(error) => {
                self.save_pending = matches!(
                    session.read(cx).save_state(),
                    SaveState::Journaling | SaveState::Snapshotting
                );
                self.save_error = Some(ShellSaveError::Lifecycle {
                    message: format!("无法在 {reason:?} 前保存当前笔记：{error}"),
                });
                cx.notify();
                false
            }
        }
    }

    /// Used by the native app lifecycle, whose close/quit callbacks operate
    /// outside ordinary element action dispatch. It shares the exact session
    /// boundary as note switches and the visible manual-save command.
    pub(crate) fn flush_for_lifecycle(
        &mut self,
        reason: FlushReason,
        cx: &mut Context<Self>,
    ) -> bool {
        self.flush_active_session(reason, cx)
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

    fn sync_current(&mut self, _: &SyncCurrent, window: &mut Window, cx: &mut Context<Self>) {
        self.apply_action(AppAction::ManualSync, window, cx);
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
        self.note_session = None;
        self._note_session_observation = None;
        self.editor_surface = None;
        self.surface_note_id = next_id.clone();
        self.unsupported_document = None;

        let Some(note) = note else {
            return;
        };
        let repository = self.model.read_with(cx, |model, _| model.repository());
        match NoteSession::prepare(note, repository.as_ref())
            .and_then(|prepared| prepared.claim_recovery_ownership(repository.as_ref()))
        {
            Ok(prepared) => {
                let save_clock = Arc::clone(&self.save_clock);
                let session = cx.new(move |session_cx| {
                    NoteSession::from_prepared(
                        prepared,
                        Arc::clone(&repository),
                        save_clock,
                        session_cx,
                    )
                });
                let editor = session.read(cx).editor().clone();
                self._note_session_observation =
                    Some(cx.observe(&session, |shell, session, cx| {
                        match session.read(cx).save_state() {
                            SaveState::Failed(error) => {
                                let message = format!("自动保存失败：{error}");
                                if shell.save_pending {
                                    shell.save_pending = false;
                                    shell.save_error = Some(ShellSaveError::Lifecycle { message });
                                } else {
                                    shell.save_error = Some(ShellSaveError::Automatic {
                                        generation: session.read(cx).save_generation(),
                                        message,
                                    });
                                }
                            }
                            SaveState::Clean if shell.save_pending => {
                                shell.save_pending = false;
                                shell.save_error = None;
                            }
                            SaveState::Clean => {
                                if let Some(ShellSaveError::Automatic { generation, .. }) =
                                    shell.save_error.as_ref()
                                    && session.read(cx).save_generation() > *generation
                                {
                                    shell.save_error = None;
                                }
                            }
                            _ => {}
                        }
                        cx.notify();
                    }));
                #[cfg(test)]
                let before_shape = Arc::clone(&self.library_surface_paint_hooks_for_test);
                #[cfg(test)]
                let after_paint = Arc::clone(&self.library_surface_paint_hooks_for_test);
                self.editor_surface = Some(cx.new(move |cx| {
                    #[cfg(test)]
                    let mut surface =
                        EditorSurface::new(editor, EditorSurfaceMode::Editable, None, cx);
                    #[cfg(not(test))]
                    let surface = EditorSurface::new(editor, EditorSurfaceMode::Editable, None, cx);
                    #[cfg(test)]
                    surface.set_paint_hooks(EditorSurfaceHooks::new(
                        move |_window, _app| {
                            before_shape.shape.fetch_add(1, Ordering::Relaxed);
                        },
                        move |_window, _app| {
                            after_paint.paint.fetch_add(1, Ordering::Relaxed);
                        },
                    ));
                    surface
                }));
                self.note_session = Some(session);
            }
            Err(error) => self.unsupported_document = Some(error.to_string()),
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
                                .debug_selector(move || {
                                    if selected {
                                        "library-selected-note-card".to_owned()
                                    } else {
                                        format!("library-note-card-{index}")
                                    }
                                })
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

    fn render_note_title(&self, cx: &mut Context<Self>) -> Option<gpui::AnyElement> {
        let session = self.note_session.as_ref()?.clone();
        let title = session.read_with(cx, |session, _| session.title().clone());
        let paint_title = title.clone();
        let canvas_title = title.clone();
        let canvas = canvas(
            move |bounds, _window, cx| {
                let _ = canvas_title.update(cx, |title, _| title.record_bounds(bounds));
                canvas_title.clone()
            },
            move |bounds, entity, window, cx| {
                let (text, selection, focus) = entity.read_with(cx, |title, _| {
                    (
                        SharedString::from(title.text().to_owned()),
                        title.selection().clone(),
                        title.focus_handle().clone(),
                    )
                });
                let style = window.text_style();
                let mut font = style.font();
                font.weight = FontWeight::SEMIBOLD;
                let line = window.text_system().shape_line(
                    text,
                    px(30.0),
                    &[TextRun {
                        len: entity.read(cx).text().len(),
                        font,
                        color: rgba(0x172033ff).into(),
                        background_color: None,
                        underline: None,
                        strikethrough: None,
                    }],
                    None,
                );
                entity.update(cx, |title, _| title.record_layout(bounds, line.clone()));
                if focus.is_focused(window) && !selection.is_empty() {
                    window.paint_quad(gpui::fill(
                        Bounds::from_corners(
                            point(
                                bounds.left() + line.x_for_index(selection.start),
                                bounds.top(),
                            ),
                            point(
                                bounds.left() + line.x_for_index(selection.end),
                                bounds.bottom(),
                            ),
                        ),
                        rgba(0x00a82d33),
                    ));
                }
                line.paint(bounds.origin, bounds.size.height, window, cx)
                    .ok();
                if focus.is_focused(window) && selection.is_empty() {
                    let x = line.x_for_index(selection.start);
                    window.paint_quad(gpui::fill(
                        Bounds::new(
                            point(bounds.left() + x, bounds.top()),
                            size(px(1.0), bounds.size.height),
                        ),
                        rgba(EVERNOTE_GREEN),
                    ));
                }
                if focus.is_focused(window) {
                    window.handle_input(
                        &focus,
                        ElementInputHandler::new(bounds, paint_title.clone()),
                        cx,
                    );
                }
            },
        )
        .w_full()
        .h(px(40.0));
        Some(
            div()
                .id("library-note-title")
                .debug_selector(|| "library-note-title".to_owned())
                .mx(px(26.0))
                .mt(px(22.0))
                .mb(px(8.0))
                .h(px(40.0))
                .key_context("LibraryNoteTitle")
                .track_focus(title.read(cx).focus_handle())
                .on_mouse_down(MouseButton::Left, cx.listener(Self::on_title_mouse_down))
                .on_mouse_move(cx.listener(Self::on_title_mouse_move))
                .on_mouse_up(MouseButton::Left, cx.listener(Self::on_title_mouse_up))
                .on_key_down(cx.listener(Self::on_title_key_down))
                .child(canvas)
                .into_any_element(),
        )
    }

    fn on_title_mouse_down(
        &mut self,
        event: &MouseDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if event.button != MouseButton::Left {
            cx.propagate();
            return;
        }
        let Some(session) = self.note_session.clone() else {
            return;
        };
        let title = session.read_with(cx, |session, _| session.title().clone());
        title.update(cx, |title, title_cx| {
            title.begin_pointer_selection(event.position, event.modifiers.shift);
            title.focus_handle().focus(window);
            title_cx.notify();
        });
        cx.stop_propagation();
    }

    fn on_title_mouse_move(
        &mut self,
        event: &MouseMoveEvent,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if event.pressed_button != Some(MouseButton::Left) {
            cx.propagate();
            return;
        }
        let Some(session) = self.note_session.clone() else {
            return;
        };
        let title = session.read_with(cx, |session, _| session.title().clone());
        title.update(cx, |title, title_cx| {
            if title.extend_pointer_selection(event.position).is_some() {
                title_cx.notify();
            }
        });
        cx.stop_propagation();
    }

    fn on_title_mouse_up(
        &mut self,
        _event: &MouseUpEvent,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(session) = self.note_session.clone() {
            let title = session.read_with(cx, |session, _| session.title().clone());
            title.update(cx, |title, _| title.end_pointer_selection());
        }
        cx.stop_propagation();
    }

    fn on_title_key_down(
        &mut self,
        event: &KeyDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(session) = self.note_session.clone() else {
            return;
        };
        let (title, editor) = session.read_with(cx, |session, _| {
            (session.title().clone(), session.editor().clone())
        });
        let key = event.keystroke.key.as_str();
        let modifiers = event.keystroke.modifiers;
        let secondary = modifiers.secondary();
        if TitleInput::moves_focus_to_body_for(key) {
            focus_editor(&editor, window, cx);
            cx.stop_propagation();
            return;
        }
        let handled = match key {
            "backspace" => {
                title.update(cx, |title, title_cx| {
                    title.delete_backward();
                    title_cx.notify();
                });
                true
            }
            "delete" => {
                title.update(cx, |title, title_cx| {
                    title.delete_forward();
                    title_cx.notify();
                });
                true
            }
            "left" => {
                title.update(cx, |title, title_cx| {
                    title.move_horizontal(false, modifiers.shift);
                    title_cx.notify();
                });
                true
            }
            "right" => {
                title.update(cx, |title, title_cx| {
                    title.move_horizontal(true, modifiers.shift);
                    title_cx.notify();
                });
                true
            }
            "home" => {
                title.update(cx, |title, title_cx| {
                    title.move_to_edge(false, modifiers.shift);
                    title_cx.notify();
                });
                true
            }
            "end" => {
                title.update(cx, |title, title_cx| {
                    title.move_to_edge(true, modifiers.shift);
                    title_cx.notify();
                });
                true
            }
            "a" if secondary => {
                title.update(cx, |title, title_cx| {
                    title.select_all();
                    title_cx.notify();
                });
                true
            }
            "c" if secondary => {
                let text = title.read(cx).selected_text().to_owned();
                cx.write_to_clipboard(ClipboardItem::new_string(text));
                true
            }
            "x" if secondary => {
                let text = title.read(cx).selected_text().to_owned();
                if !text.is_empty() {
                    cx.write_to_clipboard(ClipboardItem::new_string(text));
                    title.update(cx, |title, title_cx| {
                        title.delete_forward();
                        title_cx.notify();
                    });
                }
                true
            }
            "v" if secondary => {
                title.update(cx, |title, title_cx| title.paste_from_clipboard(title_cx));
                true
            }
            _ => false,
        };
        if handled {
            cx.stop_propagation();
        }
    }

    fn render_editor_panel(
        &mut self,
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
            let title = self.render_note_title(cx);
            return div()
                .id("library-native-editor-pane")
                .flex_1()
                .min_w(px(1.0))
                .h_full()
                .flex()
                .flex_col()
                .children(title)
                .child(div().flex_1().min_w(px(1.0)).child(surface.clone()))
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
        let status_message = match status {
            AppStatus::Ready => None,
            AppStatus::Error(error) => Some(format!("资料库错误：{error}")),
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
                library_action_button(
                    "library-sync-current",
                    "保存".to_owned(),
                    AppAction::ManualSync,
                    cx,
                ),
            ]);
        div()
            .id("library-shell")
            .size_full()
            .relative()
            .flex()
            .key_context("LibraryShell")
            .track_focus(&self.focus_handle)
            .on_action(cx.listener(Self::create_note))
            .on_action(cx.listener(Self::trash_selected))
            .on_action(cx.listener(Self::toggle_sidebar))
            .on_action(cx.listener(Self::toggle_note_list))
            .on_action(cx.listener(Self::cycle_list_view_mode))
            .on_action(cx.listener(Self::cycle_sort))
            .on_action(cx.listener(Self::sync_current))
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
            .children(status_message.map(|message| {
                div()
                    .id("library-action-error")
                    .debug_selector(|| "library-action-error".to_owned())
                    .absolute()
                    .bottom(px(10.0))
                    .right(px(14.0))
                    .text_size(px(11.0))
                    .text_color(rgba(0xa34838ff))
                    .child(message)
            }))
            .children(self.save_error.as_ref().map(|error| {
                div()
                    .id("library-save-error")
                    .debug_selector(|| "library-save-error".to_owned())
                    .absolute()
                    .bottom(px(34.0))
                    .right(px(14.0))
                    .max_w(px(520.0))
                    .text_size(px(11.0))
                    .text_color(rgba(0xa34838ff))
                    .child(error.message().to_owned())
            }))
            .children(self.startup_notice.as_ref().map(|notice| {
                div()
                    .id("library-startup-notice")
                    .debug_selector(|| "library-startup-notice".to_owned())
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
            .debug_selector(|| "library-startup-error".to_owned())
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
        .debug_selector(move || id.to_owned())
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

#[cfg(test)]
fn import_note_body(note: &Note) -> Result<crate::native_editor::model::Document, String> {
    CanonicalDocument::parse_html(&note.body_html)
        .map_err(|error| format!("无法解析已保存的正文: {error}"))
        .and_then(|document| import_canonical(&document).map_err(|error| error.to_string()))
}

#[cfg(test)]
mod tests;
