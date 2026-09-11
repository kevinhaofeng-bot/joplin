pub mod note_card;
pub mod note_list;
pub mod sidebar;

#[cfg(test)]
use crate::app::note_session::AttachmentOpener;
use crate::app::note_session::{InsertIntent, NoteSession, ResourceImportRequest};
use crate::app::save_coordinator::{FlushReason, SaveState, SystemSaveClock};
use crate::app::{
    AppAction, AppModel, AppStatus, CreateNote, CycleListViewMode, CycleSort, ListViewMode,
    NoteSort, SyncCurrent, ToggleNoteList, ToggleSidebar, TrashSelected,
};
use crate::components::Paste;
use crate::native_editor::chrome::{EVERNOTE_GREEN, TitleInput};
#[cfg(test)]
use crate::native_editor::codec::import_canonical;
#[cfg(test)]
use crate::native_editor::core::EditorCore;
use crate::native_editor::images::{
    BudgetedImageCache, ClipboardPayload, DECODED_IMAGE_CACHE_BUDGET, PasteIntent,
    classify_clipboard, classify_drop, read_native_pasteboard, resolve_clipboard_payload,
};
#[cfg(test)]
use crate::native_editor::model::BlockContent;
use crate::native_editor::model::Selection;
#[cfg(test)]
use crate::native_editor::surface::EditorSurfaceHooks;
use crate::native_editor::surface::focus_editor;
use crate::native_editor::surface::{EditorSurface, EditorSurfaceEvent, EditorSurfaceMode};
use crate::native_editor::toolbar::{
    EditorCommandChrome, EditorCommandChromeEvent, EditorCommandChromeHost,
    EditorCommandChromeRender,
};
#[cfg(test)]
use app_lite_core::CanonicalDocument;
use app_lite_core::{LibraryRepository, Note, NoteId, NoteProjection};
use gpui::{
    AnyWindowHandle, App, AppContext, Bounds, ClipboardItem, Context, DragMoveEvent,
    ElementInputHandler, Entity, ExternalPaths, FocusHandle, FontWeight, InteractiveElement,
    IntoElement, KeyBinding, KeyDownEvent, MouseButton, MouseDownEvent, MouseMoveEvent,
    MouseUpEvent, ParentElement, PathPromptOptions, Render, ScrollStrategy, SharedString, Styled,
    Subscription, Task, TextRun, UniformListScrollHandle, Window, WindowBounds, WindowHandle,
    WindowOptions, canvas, div, point, px, rgba, size, uniform_list,
};
use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::Arc;
#[cfg(test)]
use std::sync::atomic::{AtomicU32, AtomicUsize, Ordering};
use std::sync::mpsc::Receiver;
#[cfg(test)]
use std::sync::mpsc::{self, Sender};
use std::time::Duration;

#[derive(Clone, Debug, PartialEq, Eq)]
enum ShellSaveError {
    Automatic { generation: i64, message: String },
    Lifecycle { message: String },
}

/// A real platform completion whose saved document point must survive a
/// Task-4 worker. The request is only a path or clipboard-owned payload;
/// normalization, descriptor verification and staging happen later on the
/// retained resource worker. Keeping at most one request avoids turning rapid
/// paste events into an unbounded in-memory resource backlog.
struct QueuedResourceInsert {
    request: ResourceImportRequest,
    intent: InsertIntent,
    owned_temporary_paths: Vec<PathBuf>,
    window: WindowHandle<LibraryShell>,
}

const MAX_QUEUED_RESOURCE_INSERTS: usize = 1;

/// Evernote's exported `--color-background-fill-primary` and
/// `--color-surface-fill-primary-enabled` both resolve to
/// `--colors-grey-100` (`#fff`) in
/// `modules/content/commands/exportDesignTokens.generated.ts`.
///
/// Keep this as an opaque fill owned by the library route. The macOS window
/// background may be transparent or use a system appearance, so the editor
/// shell must not inherit it accidentally.
const EVERNOTE_LIGHT_PRIMARY_SURFACE: u32 = 0xffffffff;

#[derive(Clone, Copy)]
enum LibraryPrimarySurface {
    Shell,
    MainEditor,
    Toolbar,
    Title,
    EditorPane,
}

impl LibraryPrimarySurface {
    #[cfg(test)]
    const fn index(self) -> usize {
        match self {
            Self::Shell => 0,
            Self::MainEditor => 1,
            Self::Toolbar => 2,
            Self::Title => 3,
            Self::EditorPane => 4,
        }
    }
}

/// The editor pane belongs in the right-column flex flow, whereas Link/More
/// overlays are window-coordinate layers. Returning them separately keeps the
/// shared chrome's measured anchor geometry intact without recreating its
/// presentation logic in the library route.
struct LibraryEditorRender {
    pane: gpui::AnyElement,
    overlays: Vec<gpui::AnyElement>,
}

impl LibraryEditorRender {
    fn plain(pane: gpui::AnyElement) -> Self {
        Self {
            pane,
            overlays: Vec::new(),
        }
    }
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
    /// The one shared formatting owner for the active session's existing
    /// `EditorCore`. Its Link/More state must disappear with the session so a
    /// switch can never present controls for the previous note.
    command_chrome: Option<Entity<EditorCommandChrome>>,
    _command_chrome_event_subscription: Option<Subscription>,
    /// The mounted canvas emits typed attachment-open intents. Keeping this
    /// subscription with the shell prevents a previous note's surface from
    /// delivering a stale resource id after switch/destruction.
    _editor_surface_event_subscription: Option<Subscription>,
    /// One bounded decoded-texture cache is retained for the full library
    /// window. It is passed to every mounted shared surface, so a committed
    /// resource invalidates the current canvas rather than waiting for a note
    /// switch to construct another cache.
    image_cache: Entity<BudgetedImageCache>,
    surface_note_id: Option<NoteId>,
    /// Saved before a native panel opens. Completion always uses this point,
    /// never an arbitrary caret that may have moved while the picker owned
    /// focus.
    pending_resource_insert: Option<InsertIntent>,
    queued_resource_inserts: VecDeque<QueuedResourceInsert>,
    resource_queue_completion_scheduled: bool,
    pending_drop_intent: Option<InsertIntent>,
    resource_notice: Option<String>,
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
    /// Filled only by the production `Div::bg` argument expressions for the
    /// default-route shell. This lets the mounted regression test observe the
    /// actual structured style path rather than grep source text.
    primary_surface_fills: [AtomicU32; 5],
}

/// A mounted-view observation rather than a repository-only assertion. The
/// regression this protects was specifically a valid DB relation whose
/// already-mounted surface/card still displayed the preceding state.
#[cfg(test)]
#[derive(Clone, Debug)]
pub(crate) struct ImageFlowProbe {
    pub(crate) note_id: NoteId,
    pub(crate) has_image_block: bool,
    pub(crate) measured_height: f32,
    pub(crate) cache_has_resource: bool,
    pub(crate) cache_is_settled: bool,
    pub(crate) card_thumbnail_id: Option<app_lite_core::ResourceId>,
    pub(crate) image_resource_id: Option<app_lite_core::ResourceId>,
    pub(crate) has_attachment_card: bool,
    pub(crate) attachment_resource_id: Option<app_lite_core::ResourceId>,
    pub(crate) attachment_size: Option<u64>,
    /// The mounted image atom's actual layout geometry. Resource failure
    /// tests use this to drive the shared surface's NodeSelection route before
    /// dispatching Backspace, rather than inventing a model-side deletion.
    pub(crate) image_block_bounds: Option<Bounds<gpui::Pixels>>,
    /// An actual text-block hit target in the same coordinates consumed by
    /// EditorSurface. Resource failure tests use this rather than the canvas
    /// center so their suffix input cannot accidentally replace the atom.
    pub(crate) text_block_bounds: Option<Bounds<gpui::Pixels>>,
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
    /// This is deliberately the direct argument of each default-route
    /// `Div::bg` call below. It provides an explicit, source-backed light
    /// surface even when a native window supplies no opaque backing color.
    fn evernote_primary_surface_fill(&self, surface: LibraryPrimarySurface) -> gpui::Rgba {
        #[cfg(test)]
        self.library_surface_paint_hooks_for_test
            .primary_surface_fills[surface.index()]
        .store(EVERNOTE_LIGHT_PRIMARY_SURFACE, Ordering::Relaxed);
        #[cfg(not(test))]
        let _ = surface;
        rgba(EVERNOTE_LIGHT_PRIMARY_SURFACE)
    }

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
        let image_cache = BudgetedImageCache::new_entity_in_context(cx, DECODED_IMAGE_CACHE_BUDGET);
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
            command_chrome: None,
            _command_chrome_event_subscription: None,
            _editor_surface_event_subscription: None,
            image_cache,
            surface_note_id: None,
            pending_resource_insert: None,
            queued_resource_inserts: VecDeque::new(),
            resource_queue_completion_scheduled: false,
            pending_drop_intent: None,
            resource_notice: None,
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
    pub(crate) fn stall_next_resource_import_for_test(
        &mut self,
        cx: &mut Context<Self>,
    ) -> futures::channel::oneshot::Sender<()> {
        let session = self
            .note_session
            .as_ref()
            .expect("resource worker test requires an active session")
            .clone();
        session.update(cx, |session, _| {
            session.stall_next_resource_stage_for_test()
        })
    }

    #[cfg(test)]
    pub(crate) fn stall_next_resource_commit_for_test(
        &mut self,
        cx: &mut Context<Self>,
    ) -> futures::channel::oneshot::Sender<()> {
        let session = self
            .note_session
            .as_ref()
            .expect("resource worker test requires an active session")
            .clone();
        session.update(cx, |session, _| {
            session.stall_next_resource_commit_for_test()
        })
    }

    #[cfg(test)]
    pub(crate) fn stall_next_attachment_open_for_test(
        &mut self,
        cx: &mut Context<Self>,
    ) -> (
        futures::channel::oneshot::Sender<()>,
        std::sync::mpsc::Receiver<PathBuf>,
    ) {
        let session = self
            .note_session
            .as_ref()
            .expect("attachment open test requires an active session")
            .clone();
        session.update(cx, |session, _| {
            session.stall_next_attachment_open_for_test()
        })
    }

    #[cfg(test)]
    pub(crate) fn fail_next_resource_commit_for_test(
        &mut self,
        message: impl Into<String>,
        cx: &mut Context<Self>,
    ) {
        let session = self
            .note_session
            .as_ref()
            .expect("resource worker test requires an active session")
            .clone();
        session.update(cx, |session, _| {
            session.fail_next_resource_commit_for_test(message);
        });
    }

    #[cfg(test)]
    pub(crate) fn resource_flow_body_text_for_test(&self, cx: &App) -> String {
        let session = self
            .note_session
            .as_ref()
            .expect("mounted resource flow requires an active note");
        session.read_with(cx, |session, app| {
            session.editor().read(app).copy_all_plain_text()
        })
    }

    #[cfg(test)]
    pub(crate) fn resource_notice_for_test(&self) -> Option<String> {
        self.resource_notice.clone()
    }

    #[cfg(test)]
    pub(crate) fn rendered_primary_surface_fills_for_test(&self) -> [u32; 5] {
        std::array::from_fn(|index| {
            self.library_surface_paint_hooks_for_test
                .primary_surface_fills[index]
                .load(Ordering::Relaxed)
        })
    }

    /// Test-only platform handoff.  It enters the same classifier and saved
    /// selection completion route as the native `Paste` action, while keeping
    /// tests independent from the host pasteboard.
    #[cfg(test)]
    pub(crate) fn complete_clipboard_payload_for_test(
        &mut self,
        payload: ClipboardPayload,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Result<(), String> {
        let session = self
            .note_session
            .clone()
            .ok_or_else(|| "请先选择一篇笔记再粘贴资源".to_owned())?;
        let intent = Self::capture_resource_insert_intent(&session, None, cx)?;
        self.complete_paste_intent(classify_clipboard(payload), intent, window, cx)
    }

    /// Test-only Finder completion handoff.  The production drop handler
    /// captures a hit-tested selection first; this helper captures the same
    /// current selection so a test can prove that classification does not
    /// synchronously probe an external path before staging begins.
    #[cfg(test)]
    pub(crate) fn complete_drop_paths_for_test(
        &mut self,
        paths: Vec<PathBuf>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Result<(), String> {
        let session = self
            .note_session
            .clone()
            .ok_or_else(|| "请先选择一篇笔记再拖入资源".to_owned())?;
        let intent = Self::capture_resource_insert_intent(&session, None, cx)?;
        self.complete_resource_drop_paths(paths, intent, window, cx)
    }

    #[cfg(test)]
    pub(crate) fn save_error_for_test(&self) -> Option<String> {
        self.save_error
            .as_ref()
            .map(|error| error.message().to_owned())
    }

    #[cfg(test)]
    pub(crate) fn image_flow_probe_for_test(&self, cx: &App) -> ImageFlowProbe {
        let session = self
            .note_session
            .as_ref()
            .expect("mounted resource flow requires an active note");
        let (
            measured_height,
            image_resource_id,
            attachment_resource_id,
            attachment_size,
            image_block_bounds,
            text_block_bounds,
        ) = session.read_with(cx, |session, app| {
            let editor = session.editor().read(app);
            let resource_id = editor.document().blocks().iter().find_map(|block| {
                if let BlockContent::Image { resource_id, .. } = &block.content {
                    app_lite_core::ResourceId::new(resource_id.clone()).ok()
                } else {
                    None
                }
            });
            let attachment_resource_id = editor.document().blocks().iter().find_map(|block| {
                if let BlockContent::Attachment { resource_id, .. } = &block.content {
                    app_lite_core::ResourceId::new(resource_id.clone()).ok()
                } else {
                    None
                }
            });
            let attachment_size = attachment_resource_id.as_ref().and_then(|id| {
                editor
                    .attachment_metadata(id.as_str())
                    .map(|metadata| metadata.size())
            });
            let image_block_bounds = editor
                .document()
                .blocks()
                .iter()
                .find(|block| matches!(block.content, BlockContent::Image { .. }))
                .and_then(|block| editor.layout().block_layout(block.id))
                .map(|layout| layout.bounds);
            let text_block_bounds = editor
                .document()
                .blocks()
                .iter()
                .find(|block| block.content.as_text().is_some())
                .and_then(|block| editor.layout().block_layout(block.id))
                .map(|layout| layout.bounds);
            (
                editor.layout().total_height(),
                resource_id,
                attachment_resource_id,
                attachment_size,
                image_block_bounds,
                text_block_bounds,
            )
        });
        let note_id = self
            .surface_note_id
            .clone()
            .expect("active session has a note id");
        let card_thumbnail_id = self.model.read_with(cx, |model, _| {
            model
                .projections()
                .iter()
                .find(|projection| projection.id == note_id)
                .and_then(|projection| projection.selected_thumbnail_id.clone())
        });
        ImageFlowProbe {
            note_id,
            has_image_block: image_resource_id.is_some(),
            measured_height,
            cache_has_resource: self.image_cache.read(cx).len() > 0,
            cache_is_settled: self.image_cache.read(cx).is_settled(),
            card_thumbnail_id,
            image_resource_id,
            has_attachment_card: attachment_resource_id.is_some(),
            attachment_resource_id,
            attachment_size,
            image_block_bounds,
            text_block_bounds,
        }
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
                // A retryable staged resource deliberately remains `Dirty`
                // while its retained SQLite task owns the same lifecycle
                // barrier. The barrier, rather than the coordinator's
                // transient work enum, is the honest indication that this
                // visible error should clear when the exact completion lands.
                self.save_pending = session.read(cx).flush_confirmation_pending();
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
        self._command_chrome_event_subscription = None;
        self.command_chrome = None;
        self._editor_surface_event_subscription = None;
        self.editor_surface = None;
        self.surface_note_id = next_id.clone();
        self.unsupported_document = None;
        self.pending_resource_insert = None;
        self.pending_drop_intent = None;
        self.discard_queued_resource_inserts();
        self.resource_notice = None;

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
                if let Some(warning) = session.read_with(cx, |session, _| {
                    session.resource_load_warning().map(str::to_owned)
                }) {
                    self.resource_notice = Some(warning);
                }
                let editor = session.read(cx).editor().clone();
                let chrome_editor = editor.clone();
                let command_chrome = cx.new(move |chrome_cx| {
                    EditorCommandChrome::new(
                        chrome_editor,
                        EditorCommandChromeHost::Library,
                        dispatch_library_resource_picker,
                        chrome_cx,
                    )
                });
                self._command_chrome_event_subscription = Some(cx.subscribe(
                    &command_chrome,
                    |shell, _chrome, event, shell_cx| match event {
                        EditorCommandChromeEvent::RequestInsertImage { .. } => {
                            if let Err(error) = shell.begin_resource_picker(shell_cx) {
                                shell.resource_notice = Some(format!("资源未插入：{error}"));
                                shell_cx.notify();
                            }
                        }
                    },
                ));
                self._note_session_observation =
                    Some(cx.observe(&session, |shell, session, cx| {
                        shell.consume_resource_import_outcome(&session, cx);
                        shell.consume_attachment_open_outcome(&session, cx);
                        // Persisted-image hydration is intentionally driven
                        // after the first surface paint. Its per-node failure
                        // therefore arrives on this retained-session
                        // observation rather than during `prepare`; surface a
                        // truthful notice without replacing the whole note
                        // with an unsupported-document state.
                        if let Some(warning) =
                            session.read(cx).resource_load_warning().map(str::to_owned)
                        {
                            shell.resource_notice = Some(warning);
                        }
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
                            SaveState::Clean
                                if shell.save_pending
                                    && !session.read(cx).flush_confirmation_pending() =>
                            {
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
                        // A paste/drop that arrived while Task 4 owned an
                        // immutable writer payload must resume only from this
                        // retained-session notification. The queued intent is
                        // the original saved DocPoint, never the live caret.
                        shell.schedule_queued_resource_completion(cx);
                        cx.notify();
                    }));
                #[cfg(test)]
                let before_shape = Arc::clone(&self.library_surface_paint_hooks_for_test);
                #[cfg(test)]
                let after_paint = Arc::clone(&self.library_surface_paint_hooks_for_test);
                let image_cache = self.image_cache.clone();
                let surface = cx.new(move |cx| {
                    #[cfg(test)]
                    let mut surface = EditorSurface::new(
                        editor,
                        EditorSurfaceMode::Editable,
                        Some(image_cache.clone()),
                        cx,
                    );
                    #[cfg(not(test))]
                    let surface = EditorSurface::new(
                        editor,
                        EditorSurfaceMode::Editable,
                        Some(image_cache),
                        cx,
                    );
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
                });
                self._editor_surface_event_subscription = Some(cx.subscribe(
                    &surface,
                    |shell, _surface, event, shell_cx| match event {
                        EditorSurfaceEvent::OpenAttachment { resource_id } => {
                            shell.open_attachment_resource(resource_id.clone(), shell_cx);
                        }
                    },
                ));
                self.editor_surface = Some(surface);
                self.command_chrome = Some(command_chrome);
                self.note_session = Some(session);
            }
            Err(error) => self.unsupported_document = Some(error.to_string()),
        }
    }

    /// Start the same saved-selection completion seam used by the native
    /// picker, pasteboard and drop routes.  Keeping this tiny state mutation
    /// synchronous makes cancellation harmless and lets the OS panel live
    /// outside the GPUI view borrow.
    pub(crate) fn begin_resource_picker(&mut self, cx: &mut Context<Self>) -> Result<(), String> {
        let Some(session) = self.note_session.clone() else {
            return Err("请先选择一篇笔记再插入资源".to_owned());
        };
        self.pending_resource_insert =
            Some(Self::capture_resource_insert_intent(&session, None, cx)?);
        self.resource_notice = None;
        cx.notify();
        Ok(())
    }

    /// Complete a path chosen by the platform picker. This is deliberately
    /// public within the crate so mounted tests exercise exactly the route
    /// called after the asynchronous native panel settles.
    pub(crate) fn complete_resource_picker_path(
        &mut self,
        path: PathBuf,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Result<(), String> {
        let Some(intent) = self.pending_resource_insert.take() else {
            return Err("资源选择已过期；请重新打开选择器".to_owned());
        };
        self.complete_resource_request(
            ResourceImportRequest::Path(path),
            intent,
            Vec::new(),
            window,
            cx,
        )
    }

    /// Complete the one production resource transaction shared by picker,
    /// pasteboard and Finder drop. If a Task-4 writer owns the current
    /// immutable generation, retain the source and its captured DocPoint
    /// until that exact writer completes instead of consulting a later caret.
    fn complete_resource_request(
        &mut self,
        request: ResourceImportRequest,
        intent: InsertIntent,
        owned_temporary_paths: Vec<PathBuf>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Result<(), String> {
        let Some(session) = self.note_session.clone() else {
            cleanup_owned_temporary_paths(&owned_temporary_paths);
            return Err("笔记已关闭，未插入资源".to_owned());
        };
        if session.read_with(cx, |session, _| session.resource_insert_is_fenced()) {
            return self.enqueue_resource_insert(
                request,
                intent,
                owned_temporary_paths,
                window,
                cx,
            );
        }
        self.start_resource_import(request, intent, owned_temporary_paths, window, cx)
    }

    fn capture_resource_insert_intent(
        session: &Entity<NoteSession>,
        selection: Option<Selection>,
        cx: &mut Context<Self>,
    ) -> Result<InsertIntent, String> {
        session
            .update(cx, |session, session_cx| match selection {
                Some(selection) => session.capture_resource_insert_intent_at(selection, session_cx),
                None => session.capture_resource_insert_intent(session_cx),
            })
            .map_err(|error| error.to_string())
    }

    fn discard_resource_insert_intent(
        session: &Entity<NoteSession>,
        intent: InsertIntent,
        cx: &mut Context<Self>,
    ) {
        session.update(cx, |session, session_cx| {
            session.discard_resource_insert_intent(intent, session_cx);
        });
    }

    fn consume_resource_import_outcome(
        &mut self,
        session: &Entity<NoteSession>,
        cx: &mut Context<Self>,
    ) {
        let outcome = session.update(cx, |session, _| session.take_resource_import_outcome());
        let Some(outcome) = outcome else {
            return;
        };
        match outcome {
            Ok(inserted) => {
                self.resource_notice = inserted.presentation_warning.clone();
                let _ = self.model.update(cx, |model, model_cx| {
                    model.apply_active_resource_commit(
                        inserted.note,
                        inserted.selected_thumbnail_id,
                    );
                    model_cx.notify();
                });
            }
            Err(error) => {
                self.resource_notice = Some(format!("资源未插入：{error}"));
            }
        }
    }

    /// The surface already made the attachment a full atomic selection. This
    /// shell boundary owns the only platform-facing side effect and delegates
    /// descriptor resolution/materialization to the retained session worker.
    fn open_attachment_resource(&mut self, resource_id: String, cx: &mut Context<Self>) {
        let Some(session) = self.note_session.clone() else {
            self.resource_notice = Some("笔记已关闭，未执行附件打开操作".to_owned());
            cx.notify();
            return;
        };
        if let Err(error) = session.update(cx, |session, session_cx| {
            session.open_attachment(resource_id, session_cx)
        }) {
            self.resource_notice = Some(format!("附件未打开：{error}"));
        }
        cx.notify();
    }

    fn consume_attachment_open_outcome(
        &mut self,
        session: &Entity<NoteSession>,
        cx: &mut Context<Self>,
    ) {
        let outcome = session.update(cx, |session, _| session.take_attachment_open_outcome());
        let Some(outcome) = outcome else {
            return;
        };
        match outcome {
            Ok(opened) => {
                self.resource_notice = Some(format!(
                    "已交给系统默认应用打开附件 {}",
                    opened.resource_id.as_str()
                ));
            }
            Err(error) => {
                self.resource_notice = Some(format!("附件未打开：{error}"));
            }
        }
        cx.notify();
    }

    #[cfg(test)]
    pub(crate) fn set_attachment_opener_for_test(
        &mut self,
        opener: AttachmentOpener,
        cx: &mut Context<Self>,
    ) {
        if let Some(session) = self.note_session.as_ref() {
            let _ = session.update(cx, |session, _| {
                session.set_attachment_opener_for_test(opener);
            });
        }
    }

    fn start_resource_import(
        &mut self,
        request: ResourceImportRequest,
        intent: InsertIntent,
        owned_temporary_paths: Vec<PathBuf>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Result<(), String> {
        let Some(session) = self.note_session.clone() else {
            cleanup_owned_temporary_paths(&owned_temporary_paths);
            return Err("笔记已关闭，未插入资源".to_owned());
        };
        let started = session
            .update(cx, |session, session_cx| {
                session.start_resource_import(
                    request,
                    intent.clone(),
                    owned_temporary_paths,
                    session_cx,
                )
            })
            .map_err(|error| error.to_string());
        match started {
            Ok(()) => {
                // The retained session worker publishes its exact outcome via
                // the existing session observation. Do not sync-load or
                // update the model here: that would reintroduce a foreground
                // SQLite path and a second visible projection event.
                focus_editor(
                    &session.read_with(cx, |session, _| session.editor().clone()),
                    window,
                    cx,
                );
                cx.notify();
                Ok(())
            }
            Err(error) => {
                Self::discard_resource_insert_intent(&session, intent, cx);
                return Err(error);
            }
        }
    }

    fn enqueue_resource_insert(
        &mut self,
        request: ResourceImportRequest,
        intent: InsertIntent,
        owned_temporary_paths: Vec<PathBuf>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Result<(), String> {
        if self.queued_resource_inserts.len() >= MAX_QUEUED_RESOURCE_INSERTS {
            cleanup_owned_temporary_paths(&owned_temporary_paths);
            if let Some(session) = self.note_session.as_ref() {
                Self::discard_resource_insert_intent(session, intent, cx);
            }
            return Err("上一项资源仍在等待保存完成；请稍后再试".to_owned());
        }
        let window = window
            .window_handle()
            .downcast::<LibraryShell>()
            .ok_or_else(|| "无法关联资源插入到当前资料库窗口".to_owned())?;
        self.queued_resource_inserts
            .push_back(QueuedResourceInsert {
                request,
                intent,
                owned_temporary_paths,
                window,
            });
        self.resource_notice = Some("正在保存刚才的编辑；资源会插入到原来的位置".to_owned());
        cx.notify();
        Ok(())
    }

    fn schedule_queued_resource_completion(&mut self, cx: &mut Context<Self>) {
        if self.resource_queue_completion_scheduled || self.queued_resource_inserts.is_empty() {
            return;
        }
        let Some(window) = self
            .queued_resource_inserts
            .front()
            .map(|queued| queued.window.clone())
        else {
            return;
        };
        self.resource_queue_completion_scheduled = true;
        cx.defer(move |app| {
            let _ = window.update(app, |shell, window, shell_cx| {
                shell.resource_queue_completion_scheduled = false;
                shell.complete_next_queued_resource(window, shell_cx);
            });
        });
    }

    fn complete_next_queued_resource(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(session) = self.note_session.clone() else {
            self.discard_queued_resource_inserts();
            return;
        };
        if session.read_with(cx, |session, _| session.resource_insert_is_fenced()) {
            return;
        }
        let Some(queued) = self.queued_resource_inserts.pop_front() else {
            return;
        };
        if let Err(error) = self.start_resource_import(
            queued.request,
            queued.intent,
            queued.owned_temporary_paths,
            window,
            cx,
        ) {
            self.resource_notice = Some(format!("资源未插入：{error}"));
            cx.notify();
        }
        self.schedule_queued_resource_completion(cx);
    }

    fn discard_queued_resource_inserts(&mut self) {
        while let Some(queued) = self.queued_resource_inserts.pop_front() {
            cleanup_owned_temporary_paths(&queued.owned_temporary_paths);
        }
        self.resource_queue_completion_scheduled = false;
    }

    fn prompt_for_resource_picker(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Result<WindowHandle<LibraryShell>, String> {
        self.begin_resource_picker(cx)?;
        window
            .window_handle()
            .downcast::<LibraryShell>()
            .ok_or_else(|| "无法关联资源选择器到当前资料库窗口".to_owned())
    }

    fn cancel_resource_picker(&mut self, cx: &mut Context<Self>) {
        if let Some(intent) = self.pending_resource_insert.take()
            && let Some(session) = self.note_session.as_ref()
        {
            Self::discard_resource_insert_intent(session, intent, cx);
        }
        cx.notify();
    }

    /// Native `Paste` is intentionally handled by the library shell, not by
    /// `EditorCore`'s text input handler: an image/file must reach the same
    /// durable resource transaction as picker and Finder drop, while ordinary
    /// text keeps the existing EntityInputHandler path.
    fn paste_resource_or_text(&mut self, _: &Paste, window: &mut Window, cx: &mut Context<Self>) {
        let payload = resolve_clipboard_payload(
            read_native_pasteboard(),
            cx.read_from_clipboard().map(ClipboardPayload::from_gpui),
        );
        let Some(payload) = payload else {
            return;
        };
        let Some(session) = self.note_session.clone() else {
            self.resource_notice = Some("请先选择一篇笔记再粘贴资源".to_owned());
            cx.notify();
            return;
        };
        let intent = match Self::capture_resource_insert_intent(&session, None, cx) {
            Ok(intent) => intent,
            Err(error) => {
                self.resource_notice = Some(format!("无法保存粘贴位置：{error}"));
                cx.notify();
                return;
            }
        };
        if let Err(error) =
            self.complete_paste_intent(classify_clipboard(payload), intent, window, cx)
        {
            self.resource_notice = Some(format!("粘贴未完成：{error}"));
            cx.notify();
        }
    }

    fn complete_paste_intent(
        &mut self,
        intent: PasteIntent,
        saved_intent: InsertIntent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Result<(), String> {
        match intent {
            PasteIntent::Image { payload } => self.complete_resource_request(
                ResourceImportRequest::Image(payload),
                saved_intent,
                Vec::new(),
                window,
                cx,
            ),
            PasteIntent::ImageCandidates { candidates } => self.complete_resource_request(
                ResourceImportRequest::ImageCandidates(candidates),
                saved_intent,
                Vec::new(),
                window,
                cx,
            ),
            PasteIntent::EncodedImage { payload } => self.complete_resource_request(
                ResourceImportRequest::EncodedImage(payload),
                saved_intent,
                Vec::new(),
                window,
                cx,
            ),
            PasteIntent::File { path, cleanup } => {
                let request = ResourceImportRequest::Path(path.clone());
                let owned_temporary_paths = cleanup.then_some(path).into_iter().collect();
                self.complete_resource_request(
                    request,
                    saved_intent,
                    owned_temporary_paths,
                    window,
                    cx,
                )
            }
            PasteIntent::FileCandidates {
                paths,
                cleanup_paths,
            } => self.complete_resource_request(
                ResourceImportRequest::Paths(paths),
                saved_intent,
                cleanup_paths,
                window,
                cx,
            ),
            PasteIntent::Text { text } => {
                let Some(session) = self.note_session.clone() else {
                    return Err("笔记已关闭，未粘贴文本".to_owned());
                };
                Self::discard_resource_insert_intent(&session, saved_intent, cx);
                let editor = session.read_with(cx, |session, _| session.editor().clone());
                editor
                    .update(cx, |editor, editor_cx| {
                        let result = editor.paste_plain_text(&text);
                        editor_cx.notify();
                        result
                    })
                    .map_err(|error| error.to_string())?;
                focus_editor(&editor, window, cx);
                Ok(())
            }
            PasteIntent::Unsupported => {
                if let Some(session) = self.note_session.as_ref() {
                    Self::discard_resource_insert_intent(session, saved_intent, cx);
                }
                self.resource_notice = Some("剪贴板中没有可安全插入的资源".to_owned());
                cx.notify();
                Ok(())
            }
        }
    }

    fn on_external_paths_drag_move(
        &mut self,
        event: &DragMoveEvent<ExternalPaths>,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(session) = self.note_session.clone() else {
            return;
        };
        let editor = session.read_with(cx, |session, _| session.editor().clone());
        let point = editor.update(cx, |editor, _| {
            editor.point_from_layout(event.event.position)
        });
        if let Some(intent) = self.pending_drop_intent.take() {
            Self::discard_resource_insert_intent(&session, intent, cx);
        }
        self.pending_drop_intent =
            match Self::capture_resource_insert_intent(&session, point.map(Selection::caret), cx) {
                Ok(intent) => Some(intent),
                Err(error) => {
                    self.resource_notice = Some(format!("无法记录拖放位置：{error}"));
                    cx.notify();
                    None
                }
            };
    }

    fn on_external_paths_drop(
        &mut self,
        paths: &ExternalPaths,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let intent = match self.pending_drop_intent.take() {
            Some(intent) => Ok(intent),
            None => self
                .note_session
                .as_ref()
                .ok_or_else(|| "请先选择一篇笔记再拖入资源".to_owned())
                .and_then(|session| Self::capture_resource_insert_intent(session, None, cx)),
        };
        let intent = match intent {
            Ok(intent) => intent,
            Err(error) => {
                self.resource_notice = Some(error);
                cx.notify();
                return;
            }
        };
        if let Err(error) =
            self.complete_resource_drop_paths(paths.paths().to_vec(), intent, window, cx)
        {
            self.resource_notice = Some(format!("拖放未完成：{error}"));
            cx.notify();
        }
    }

    /// Production completion seam for the platform `ExternalPaths` event.
    /// GPUI's test backend cannot manufacture a non-empty `ExternalPaths`, so
    /// mounted tests invoke this same post-hit-test method with a real Finder
    /// fixture path rather than fabricating an editor transaction.
    pub(crate) fn complete_resource_drop_paths(
        &mut self,
        paths: Vec<PathBuf>,
        intent: InsertIntent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Result<(), String> {
        self.complete_paste_intent(classify_drop(&paths), intent, window, cx)
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
                .bg(self.evernote_primary_surface_fill(LibraryPrimarySurface::Title))
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

    /// Escape belongs to the shared command Chrome, not to a LibraryShell
    /// duplicate of its Link/More state. The focused editor surface bubbles
    /// this unhandled key to the retained shell; the Chrome then closes either
    /// overlay and restores its one active `EditorCore` focus handle.
    fn on_shell_key_down(
        &mut self,
        event: &KeyDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if event.keystroke.key != "escape" {
            return;
        }
        let Some(chrome) = self.command_chrome.clone() else {
            return;
        };
        let dismissed = chrome.update(cx, |chrome, chrome_cx| {
            chrome.dismiss_overlay(window, chrome_cx)
        });
        if dismissed {
            cx.stop_propagation();
        }
    }

    fn render_editor_panel(
        &mut self,
        items_empty: bool,
        note: Option<Note>,
        available_width: f32,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> LibraryEditorRender {
        if items_empty {
            return LibraryEditorRender::plain(
                div()
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
                    .into_any_element(),
            );
        }
        if let Some(error) = &self.unsupported_document {
            let title = note.map_or_else(|| "无法显示笔记".to_owned(), |note| note.title);
            return LibraryEditorRender::plain(
                div()
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
                    .into_any_element(),
            );
        }
        if let Some(surface) = &self.editor_surface {
            let title = self.render_note_title(cx);
            let EditorCommandChromeRender { toolbar, overlays } = self
                .command_chrome
                .as_ref()
                .map(|chrome| {
                    chrome.update(cx, |chrome, chrome_cx| {
                        chrome.render_for_host(
                            available_width.max(1.0),
                            window.content_mask().bounds,
                            chrome_cx,
                        )
                    })
                })
                .unwrap_or_else(|| EditorCommandChromeRender {
                    toolbar: div().into_any_element(),
                    overlays: Vec::new(),
                });
            return LibraryEditorRender {
                pane: div()
                    .id("library-native-editor-pane")
                    .debug_selector(|| "library-native-editor-pane".to_owned())
                    .flex_1()
                    .min_w(px(1.0))
                    .h_full()
                    .relative()
                    .flex()
                    .flex_col()
                    .bg(self.evernote_primary_surface_fill(LibraryPrimarySurface::EditorPane))
                    .can_drop(|dragged, _window, _cx| dragged.is::<ExternalPaths>())
                    .on_drag_move::<ExternalPaths>(cx.listener(Self::on_external_paths_drag_move))
                    .on_drop::<ExternalPaths>(cx.listener(Self::on_external_paths_drop))
                    .children(title)
                    .child(toolbar)
                    .child(div().flex_1().min_w(px(1.0)).child(surface.clone()))
                    .into_any_element(),
                overlays,
            };
        }
        LibraryEditorRender::plain(
            div()
                .id("library-no-selection")
                .flex_1()
                .p(px(36.0))
                .text_color(rgba(0x718075ff))
                .child("选择一篇笔记以查看正文")
                .into_any_element(),
        )
    }
}

/// Ask the native platform for one file, then return to the exact library
/// window which captured the insertion point. No picker task holds the shell
/// strongly, so closing a window while the panel is open simply discards its
/// eventual completion.
fn prompt_for_resource_path(window_handle: WindowHandle<LibraryShell>, cx: &mut App) {
    let prompt = cx.prompt_for_paths(PathPromptOptions {
        files: true,
        directories: false,
        multiple: false,
        prompt: Some("插入图片或附件".into()),
    });
    cx.spawn(async move |cx| {
        let completion = match prompt.await {
            Ok(Ok(Some(paths))) => paths.into_iter().next(),
            Ok(Ok(None)) | Ok(Err(_)) | Err(_) => None,
        };
        let _ = cx.update(move |app| {
            let _ = window_handle.update(app, |shell, window, shell_cx| {
                if let Some(path) = completion {
                    if let Err(error) = shell.complete_resource_picker_path(path, window, shell_cx)
                    {
                        shell.resource_notice = Some(format!("资源未插入：{error}"));
                        shell_cx.notify();
                    }
                } else {
                    shell.cancel_resource_picker(shell_cx);
                }
            });
        });
    })
    .detach();
}

/// The shared command Chrome only emits an intent; the library owns the
/// platform picker and its Task-5 saved-selection/durable-import policy.
///
/// The typed event is emitted first by `EditorCommandChrome`. Its retained
/// subscription captures the current `InsertIntent` synchronously, then this
/// host adapter opens the native panel. Tests intentionally exercise the
/// completion seam rather than an operating-system panel, so the test build
/// leaves panel presentation to the injected completion path.
fn dispatch_library_resource_picker(window: AnyWindowHandle, cx: &mut App) {
    #[cfg(not(test))]
    if let Some(window) = window.downcast::<LibraryShell>() {
        prompt_for_resource_path(window, cx);
    }

    #[cfg(test)]
    {
        let _ = (window, cx);
    }
}

/// AppKit's image paste bridge may create a private temporary file. It is not
/// part of the document or resource store, so clean only the exact paths the
/// bridge marked as owned after the durable import either succeeds or fails.
fn cleanup_owned_temporary_paths(paths: &[PathBuf]) {
    for path in paths {
        // The durable transaction has already reported its outcome. A stale
        // AppKit temp file is recoverable OS cleanup, not a reason to falsely
        // report that a committed resource failed.
        let _ = std::fs::remove_file(path);
    }
}

impl Render for LibraryShell {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        #[cfg(test)]
        for fill in &self
            .library_surface_paint_hooks_for_test
            .primary_surface_fills
        {
            fill.store(0, Ordering::Relaxed);
        }
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
        let LibraryEditorRender {
            pane: editor_pane,
            overlays: editor_overlays,
        } = self.render_editor_panel(
            items.is_empty(),
            active_note,
            f32::from(window.bounds().size.width)
                - if panes.sidebar_visible {
                    f32::from(panes.sidebar_width)
                } else {
                    0.0
                }
                - if panes.list_visible {
                    f32::from(panes.list_width)
                } else {
                    0.0
                },
            window,
            cx,
        );
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
            NoteSort::DeletedDescending => "按删除时间",
            NoteSort::TitleAscending => "按标题 A-Z",
            NoteSort::TitleDescending => "按标题 Z-A",
        };
        let toolbar = div()
            .id("library-actions")
            .debug_selector(|| "library-actions".to_owned())
            .h(px(42.0))
            .flex_none()
            .flex()
            .items_center()
            .gap(px(8.0))
            .px(px(12.0))
            .bg(self.evernote_primary_surface_fill(LibraryPrimarySurface::Toolbar))
            .border_b_1()
            .border_color(rgba(0xe1e5e1ff))
            .children([
                library_resource_picker_button(cx),
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
        let mut root = div()
            .id("library-shell")
            .debug_selector(|| "library-shell".to_owned())
            .size_full()
            .relative()
            .flex()
            .bg(self.evernote_primary_surface_fill(LibraryPrimarySurface::Shell))
            .key_context("LibraryShell")
            .track_focus(&self.focus_handle)
            .on_action(cx.listener(Self::create_note))
            .on_action(cx.listener(Self::trash_selected))
            .on_action(cx.listener(Self::toggle_sidebar))
            .on_action(cx.listener(Self::toggle_note_list))
            .on_action(cx.listener(Self::cycle_list_view_mode))
            .on_action(cx.listener(Self::cycle_sort))
            .on_action(cx.listener(Self::sync_current))
            .on_action(cx.listener(Self::paste_resource_or_text))
            .on_key_down(cx.listener(Self::on_shell_key_down))
            .child(sidebar::render(panes.sidebar_width, panes.sidebar_visible))
            .child(note_list)
            .child(
                div()
                    .id("library-main-editor-shell")
                    .debug_selector(|| "library-main-editor-shell".to_owned())
                    .flex_1()
                    .min_w(px(1.0))
                    .h_full()
                    .flex()
                    .flex_col()
                    .bg(self.evernote_primary_surface_fill(LibraryPrimarySurface::MainEditor))
                    .child(toolbar)
                    .child(editor_pane),
            );
        for overlay in editor_overlays {
            root = root.child(overlay);
        }
        root.children(status_message.map(|message| {
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
        .children(self.resource_notice.as_ref().map(|notice| {
            div()
                .id("library-resource-notice")
                .debug_selector(|| "library-resource-notice".to_owned())
                .absolute()
                .bottom(px(58.0))
                .right(px(14.0))
                .max_w(px(520.0))
                .text_size(px(11.0))
                .text_color(rgba(0xa34838ff))
                .child(notice.clone())
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

fn library_resource_picker_button(cx: &mut Context<LibraryShell>) -> gpui::Stateful<gpui::Div> {
    let shell = cx.weak_entity();
    div()
        .id("library-insert-resource")
        .debug_selector(|| "library-insert-resource".to_owned())
        .px(px(8.0))
        .py(px(5.0))
        .rounded(px(5.0))
        .bg(rgba(0xf1f4f1ff))
        .text_size(px(12.0))
        .text_color(rgba(0x36413aff))
        .cursor_pointer()
        .on_mouse_down(MouseButton::Left, move |_event, window, app| {
            let result = shell.update(app, |shell, shell_cx| {
                shell.prompt_for_resource_picker(window, shell_cx)
            });
            match result {
                Ok(Ok(window_handle)) => prompt_for_resource_path(window_handle, app),
                Ok(Err(error)) => {
                    let _ = shell.update(app, |shell, shell_cx| {
                        shell.resource_notice = Some(error);
                        shell_cx.notify();
                    });
                }
                Err(_) => {}
            }
        })
        .child("插入资源")
}

#[cfg(test)]
fn import_note_body(note: &Note) -> Result<crate::native_editor::model::Document, String> {
    CanonicalDocument::parse_html(&note.body_html)
        .map_err(|error| format!("无法解析已保存的正文: {error}"))
        .and_then(|document| import_canonical(&document).map_err(|error| error.to_string()))
}

#[cfg(test)]
mod tests;
