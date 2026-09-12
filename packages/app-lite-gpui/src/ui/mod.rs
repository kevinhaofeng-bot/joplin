mod card_thumbnail;
pub mod note_card;
pub mod note_list;
pub mod sidebar;

use self::card_thumbnail::{
    CARD_THUMBNAIL_CACHE_BUDGET, CARD_THUMBNAIL_PROXY_EDGE, CardThumbnailManager,
};
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
use app_lite_core::{
    LibraryEvent, LibraryRepository, LibraryRoute, Note, NoteId, NoteProjection, NotebookId,
    ResourceId, SearchHit, SearchQuery, StackId, TagId,
};
use gpui::{
    AnyWindowHandle, App, AppContext, Bounds, ClipboardItem, Context, DragMoveEvent,
    ElementInputHandler, Entity, ExternalPaths, FocusHandle, FontWeight, InteractiveElement,
    IntoElement, KeyBinding, KeyDownEvent, MouseButton, MouseDownEvent, MouseMoveEvent,
    MouseUpEvent, ParentElement, PathPromptOptions, Pixels, Render, ScrollHandle, ScrollStrategy,
    SharedString, StatefulInteractiveElement, Styled, Subscription, Task, TextRun,
    UniformListDecoration, UniformListScrollHandle, Window, WindowBounds, WindowHandle,
    WindowOptions, canvas, div, point, px, rgba, size, uniform_list,
};
use std::cell::RefCell;
use std::collections::{HashSet, VecDeque};
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::Arc;
#[cfg(test)]
use std::sync::Mutex;
use std::sync::atomic::AtomicBool;
#[cfg(test)]
use std::sync::atomic::{AtomicU32, AtomicUsize, Ordering};
use std::sync::mpsc::Receiver;
#[cfg(test)]
use std::sync::mpsc::{self, Sender};
use std::time::Duration;

// These are presentation-only library actions. They intentionally do not
// mirror an `AppAction`: the model remains the sole owner of durable note,
// route, sort and pane mutations, while the shell owns only menu visibility
// and the platform-picker handoff.
gpui::actions!(
    library_toolbar_adaptation,
    [
        ToggleLibraryToolbarMore,
        ToggleLibraryOrganizationPanel,
        OpenLibraryResourcePicker,
        ToggleSearchPalette,
    ]
);

#[derive(Clone, Debug, PartialEq, Eq)]
enum ShellSaveError {
    Automatic { generation: i64, message: String },
    Lifecycle { message: String },
}

/// Search indexing is a disposable, asynchronous projection.  It is never a
/// local-save error: the authoritative note transaction has already committed
/// when this state changes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum IndexingStatus {
    Idle,
    Pending,
    Failed(String),
}

/// Test-only async barrier placed before the synchronous core batch. It proves
/// that a running scheduler waits off the GPUI foreground executor.
#[cfg(test)]
struct IndexingWorkerGate {
    started: Sender<()>,
    release: futures::channel::oneshot::Receiver<()>,
}

/// Per-shell one-shot gate after a bounded search packet is read but before
/// the foreground continuation sees it. The OS thread is intentional: GPUI's
/// deterministic test executor must remain free to drive the save completion.
#[cfg(test)]
struct SearchCompletionGate {
    read: Sender<()>,
    release: futures::channel::oneshot::Receiver<()>,
}

#[cfg(test)]
static INDEXING_WORKER_GATE: Mutex<Option<IndexingWorkerGate>> = Mutex::new(None);

/// The GPUI test executor is deterministic rather than a real thread pool.
/// This narrow seam runs the *same shell scheduler worker call* on an OS
/// thread when a transaction-level test must hold SQLite open while the test
/// drives the mounted foreground window.
#[cfg(test)]
static INDEXING_WORKER_THREAD_FOR_TEST: AtomicBool = AtomicBool::new(false);

#[cfg(test)]
pub(crate) fn install_indexing_worker_gate_for_test(gate: IndexingWorkerGate) {
    *INDEXING_WORKER_GATE
        .lock()
        .expect("indexing worker gate mutex poisoned") = Some(gate);
}

#[cfg(test)]
pub(crate) fn run_indexing_worker_on_thread_for_test() {
    INDEXING_WORKER_THREAD_FOR_TEST.store(true, Ordering::Release);
}

/// A bounded coalescing buffer between the repository's unbounded sender and
/// the retained GPUI event task. The model refresh only distinguishes these
/// event *kinds*; keeping one representative ID per kind is therefore enough
/// to preserve its semantics while an IME/save/reconciliation barrier delays
/// installation of a full candidate.
#[derive(Default)]
struct PendingRepositoryEvents {
    note_created: Option<NoteId>,
    projection_changed: Option<NoteId>,
    note_trashed: Option<NoteId>,
    note_restored: Option<NoteId>,
    organization_changed: bool,
}

impl PendingRepositoryEvents {
    fn merge(&mut self, event: LibraryEvent) {
        match event {
            LibraryEvent::NoteCreated(id) => self.note_created = Some(id),
            LibraryEvent::NoteProjectionChanged(id) => self.projection_changed = Some(id),
            LibraryEvent::NoteTrashed(id) => self.note_trashed = Some(id),
            LibraryEvent::NoteRestored(id) => self.note_restored = Some(id),
            LibraryEvent::OrganizationChanged => self.organization_changed = true,
            // Search/sync queue notifications have no library projection
            // transition in this MVP and were intentionally ignored by the
            // previous bridge as well.
            LibraryEvent::SearchProjectionQueued(_) | LibraryEvent::SyncQueued(_) => {}
        }
    }

    fn drain(&mut self, receiver: &Receiver<LibraryEvent>) {
        for event in receiver.try_iter().take(128) {
            self.merge(event);
        }
    }

    fn is_empty(&self) -> bool {
        self.note_created.is_none()
            && self.projection_changed.is_none()
            && self.note_trashed.is_none()
            && self.note_restored.is_none()
            && !self.organization_changed
    }

    fn events(&self) -> Vec<LibraryEvent> {
        let mut events = Vec::with_capacity(5);
        if let Some(id) = &self.note_created {
            events.push(LibraryEvent::NoteCreated(id.clone()));
        }
        if let Some(id) = &self.projection_changed {
            events.push(LibraryEvent::NoteProjectionChanged(id.clone()));
        }
        if let Some(id) = &self.note_trashed {
            events.push(LibraryEvent::NoteTrashed(id.clone()));
        }
        if let Some(id) = &self.note_restored {
            events.push(LibraryEvent::NoteRestored(id.clone()));
        }
        if self.organization_changed {
            events.push(LibraryEvent::OrganizationChanged);
        }
        events
    }

    fn clear(&mut self) {
        *self = Self::default();
    }
}

/// An irreversible library command is deliberately only an in-shell intent
/// until the person confirms it. Its stable ID is retained here rather than a
/// row position so a changed route/card cannot redirect an old confirmation.
#[derive(Clone, Debug, PartialEq, Eq)]
enum PendingDestructiveAction {
    DeleteStack(StackId),
    DeleteNotebook(NotebookId),
    DeleteTag(TagId),
    PurgeNote(NoteId),
}

impl PendingDestructiveAction {
    fn action(&self) -> AppAction {
        match self {
            Self::DeleteStack(id) => AppAction::DeleteStack(id.clone()),
            Self::DeleteNotebook(id) => AppAction::DeleteNotebook(id.clone()),
            Self::DeleteTag(id) => AppAction::DeleteTag(id.clone()),
            Self::PurgeNote(id) => AppAction::PurgeNote(id.clone()),
        }
    }

    const fn label(&self) -> &'static str {
        match self {
            Self::DeleteStack(_) => "确认解散当前笔记本组？组内笔记本和笔记会保留。",
            Self::DeleteNotebook(_) => "确认删除当前笔记本？其中笔记会移至默认笔记本。",
            Self::DeleteTag(_) => "确认删除当前标签？笔记正文不会删除。",
            Self::PurgeNote(_) => "确认永久删除当前笔记？此操作不可撤销。",
        }
    }
}

/// A native panel completion is bound to the exact picker invocation that
/// captured its selection.  A window can recover, switch notes, or open a
/// second panel before the first AppKit callback arrives; accepting a bare
/// path would let the old callback consume the newer panel's saved intent.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct ResourcePickerToken(u64);

struct PendingResourcePicker {
    token: ResourcePickerToken,
    intent: InsertIntent,
}

struct ResourcePickerPrompt {
    window: WindowHandle<LibraryShell>,
    token: ResourcePickerToken,
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
/// `--color-text-fill-primary-enabled` resolves to `--colors-grey-8` in the
/// same exported Evernote token sheet.  Keeping title and empty-state text on
/// this explicit token prevents a dark host appearance from turning text into
/// an implicit, low-contrast default.
const EVERNOTE_LIGHT_PRIMARY_TEXT: u32 = 0x141414ff;
/// Evernote's secondary text color is based on `--colors-grey-40`.
const EVERNOTE_LIGHT_MUTED_TEXT: u32 = 0x696564ff;
/// `--color-surface-stroke-primary-enabled` resolves to `--colors-grey-95`.
const EVERNOTE_LIGHT_PRIMARY_STROKE: u32 = 0xf3f2f1ff;

/// Measured against the Chinese labels in the default three-pane window:
/// 540--580pt is compact, while an approximately 830pt editor keeps the
/// familiar full row. This is a local GPUI product decision, not an inferred
/// Evernote breakpoint.
const LIBRARY_TOOLBAR_COMPACT_WIDTH: f32 = 640.0;

fn library_toolbar_is_compact(available_editor_width: f32) -> bool {
    available_editor_width < LIBRARY_TOOLBAR_COMPACT_WIDTH
}

#[derive(Clone, Copy, Debug)]
enum LibraryPrimarySurface {
    Shell,
    MainEditor,
    Toolbar,
    Title,
    EditorPane,
    EmptyState,
    UnsupportedDocument,
    NoSelection,
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
            Self::EmptyState => 5,
            Self::UnsupportedDocument => 6,
            Self::NoSelection => 7,
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

/// The application toolbar follows normal editor-column flex layout, while
/// its compact More menu uses a window-layer backdrop so outside clicks cannot
/// reach the editor surface's capture-phase selection handler.
struct LibraryToolbarRender {
    toolbar: gpui::AnyElement,
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

/// `UniformList::render_items` is also called for measurement probes (usually
/// item zero). Decorations run only after UniformList has computed its real
/// prepaint `visible_range`, so this invisible observer is the first safe
/// place to reconcile card thumbnail residency. It intentionally produces no
/// visual or hit-test surface of its own.
struct CardThumbnailViewportDecoration {
    shell: gpui::WeakEntity<LibraryShell>,
    projections: Arc<Vec<NoteProjection>>,
    state: Rc<RefCell<CardThumbnailViewportState>>,
}

/// Shared by the short-lived UniformList decoration instances for one shell.
/// It avoids opening an entity update on every prepaint when the actual
/// visible thumbnail identities did not change, while an explicit worker
/// completion invalidates it so a newly adopted proxy is installed once.
#[derive(Default)]
struct CardThumbnailViewportState {
    last_resource_ids: Option<HashSet<ResourceId>>,
}

impl CardThumbnailViewportState {
    fn take_if_changed(
        &mut self,
        resource_ids: HashSet<ResourceId>,
    ) -> Option<HashSet<ResourceId>> {
        if self.last_resource_ids.as_ref() == Some(&resource_ids) {
            None
        } else {
            self.last_resource_ids = Some(resource_ids.clone());
            Some(resource_ids)
        }
    }

    fn invalidate(&mut self) {
        self.last_resource_ids = None;
    }

    /// `UniformList::render_items` is also used for the index-zero measuring
    /// probe. That probe may construct a Card whose bounded proxy is retained
    /// in the source LRU but is not in the actual prepaint viewport. Do not
    /// hand an `img` that source: GPUI would ask the image cache to decode it,
    /// the cache would reject it as non-visible on completion, and the next
    /// measurement frame would start the same decode again.
    fn contains(&self, resource_id: Option<&ResourceId>) -> bool {
        resource_id.is_some_and(|resource_id| {
            self.last_resource_ids
                .as_ref()
                .is_some_and(|resource_ids| resource_ids.contains(resource_id))
        })
    }
}

impl UniformListDecoration for CardThumbnailViewportDecoration {
    fn compute(
        &self,
        visible_range: std::ops::Range<usize>,
        _bounds: Bounds<gpui::Pixels>,
        _scroll_offset: gpui::Point<gpui::Pixels>,
        _item_height: gpui::Pixels,
        _item_count: usize,
        window: &mut Window,
        cx: &mut App,
    ) -> gpui::AnyElement {
        #[cfg(test)]
        let visible_range_for_test = visible_range.clone();
        let resource_ids = visible_range
            .filter_map(|index| {
                self.projections
                    .get(index)
                    .and_then(|projection| projection.selected_thumbnail_id.clone())
            })
            .collect::<HashSet<_>>();
        let resource_ids = self.state.borrow_mut().take_if_changed(resource_ids);
        if let Some(resource_ids) = resource_ids {
            let _ = self.shell.update(cx, |shell, shell_cx| {
                #[cfg(test)]
                {
                    shell.rendered_note_range = Some(visible_range_for_test);
                }
                shell.reconcile_card_thumbnail_residency(resource_ids, window, shell_cx);
            });
        }
        div().size_full().into_any_element()
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
    /// Cards own a separate, much smaller cache and task-owned source leases.
    /// It intentionally never participates in the active editor's image
    /// residency: scrolling the list must not evict or hydrate document
    /// images, and opening a document must not populate card textures.
    card_thumbnail_cache: Entity<BudgetedImageCache>,
    /// The exact card proxy sources last installed in `card_thumbnail_cache`.
    /// UniformList measures and paints a stable viewport more than once per
    /// frame; updating the cache for an unchanged source set would notify the
    /// window and turn those measurement passes into a decode/redraw loop.
    card_thumbnail_cache_resources: HashSet<gpui::Resource>,
    card_thumbnails: CardThumbnailManager,
    card_thumbnail_viewport_state: Rc<RefCell<CardThumbnailViewportState>>,
    _card_thumbnail_task: Option<Task<()>>,
    surface_note_id: Option<NoteId>,
    /// Organization commands may commit a new revision for the *same* NoteId
    /// (for example move or tag membership). The retained session owns an
    /// optimistic save fence for its original revision, so the model observer
    /// must rebuild it only after one of those successful external commits.
    /// Normal editor saves deliberately do not set this flag: their live
    /// session has already advanced its own fence and must retain history.
    remount_current_surface_after_organization_commit: bool,
    /// Saved before a native panel opens. Completion always uses this point,
    /// never an arbitrary caret that may have moved while the picker owned
    /// focus.
    pending_resource_insert: Option<PendingResourcePicker>,
    /// A native picker can complete after a committed organization action has
    /// frozen the retained session.  Its old tracked selection was discarded
    /// at the lock transition; retain only this one-shot reason so the late
    /// platform callback gets a truthful recovery error rather than silently
    /// looking like an ordinary picker expiry.
    last_resource_picker_cancelled_by_reconciliation: Option<ResourcePickerToken>,
    next_resource_picker_token: u64,
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
    /// A small real text-input owner for C1 organization actions. It shares
    /// the title input's UTF-16 bridge but is intentionally independent from
    /// the active note title/session so cancelling an organization menu can
    /// never dirty a document.
    organization_input: Entity<TitleInput>,
    search_input: Entity<TitleInput>,
    _search_input_observation: Option<Subscription>,
    search_palette_open: bool,
    search_palette_results: Vec<SearchHit>,
    search_palette_status: SearchPaletteStatus,
    search_palette_generation: Option<u64>,
    search_palette_selected: usize,
    search_palette_scroll: ScrollHandle,
    /// The palette is an overlay, not a navigation event.  Preserve the
    /// precise native owner that had focus so Escape/backdrop can return to
    /// title, body, or an auxiliary field without inventing an editor move.
    search_palette_return_focus: Option<FocusHandle>,
    search_palette_return_organization_panel_open: bool,
    _search_task: Option<Task<()>>,
    _history_search_task: Option<Task<()>>,
    _search_route_refresh_task: Option<Task<()>>,
    #[cfg(test)]
    search_completion_gate: Option<SearchCompletionGate>,
    history_search_notice: Option<String>,
    search_refresh_retry_available: bool,
    search_refresh_retry_history: Option<bool>,
    organization_panel_open: bool,
    pending_destructive_action: Option<PendingDestructiveAction>,
    /// Retained alongside the shell so the typed navigation tree can request
    /// an offscreen route without eagerly constructing every notebook/tag row.
    sidebar_scroll: UniformListScrollHandle,
    note_list_scroll: UniformListScrollHandle,
    /// Compact editor columns preserve high-frequency actions directly and
    /// house the secondary route/list mutations in this one transient menu.
    /// It owns no `AppAction` state and is closed on any model transition.
    toolbar_more_open: bool,
    _model_observation: Subscription,
    // Held by the entity so GPUI cancels the receiver loop when this window is
    // destroyed. The task captures only a WeakEntity and never blocks on recv.
    _event_task: Task<()>,
    /// One retained, cancellable worker drains the durable search queue. It
    /// owns no note body or second repository state; the SQLite queue remains
    /// the restart-safe authority.
    _indexing_task: Task<()>,
    indexing_status: IndexingStatus,
    #[cfg(test)]
    indexing_task_cancellation_receiver: Option<Receiver<()>>,
    #[cfg(test)]
    event_task_cancellation_receiver: Option<Receiver<()>>,
    #[cfg(test)]
    rendered_note_range: Option<std::ops::Range<usize>>,
    #[cfg(test)]
    last_scroll_request: Option<usize>,
    /// Test-only instrumentation is intentionally owned by this shell rather
    /// than a process-global Atomic: GPUI mounted tests can draw independent
    /// library windows concurrently.
    #[cfg(test)]
    sidebar_render_probe: SidebarRenderProbe,
    /// Per-shell rather than global because mounted tests create independent
    /// windows concurrently. It proves a stable Cards viewport does not
    /// continuously re-install the same cache residency.
    #[cfg(test)]
    card_thumbnail_cache_reconciliations: usize,
    #[cfg(test)]
    card_thumbnail_viewport_reconciliations: usize,
    #[cfg(test)]
    library_surface_paint_hooks_for_test: Arc<LibrarySurfacePaintHooks>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum SearchPaletteStatus {
    Idle,
    Pending,
    Empty,
    Error(String),
    Ready,
}

#[cfg(test)]
#[derive(Default)]
struct LibrarySurfacePaintHooks {
    shape: AtomicUsize,
    paint: AtomicUsize,
    /// Filled only by the production `Div::bg` argument expressions for the
    /// default-route shell. This lets the mounted regression test observe the
    /// actual structured style path rather than grep source text.
    primary_surface_fills: [AtomicU32; 8],
    primary_text: AtomicU32,
    muted_text: AtomicU32,
    primary_stroke: AtomicU32,
}

/// A structured, mounted-view record of the actual default-library palette
/// expressions. It deliberately observes production `Div::bg`/`text_color`
/// calls, so a missing fill or a black-on-black regression cannot be hidden by
/// an otherwise present debug selector.
#[cfg(test)]
#[derive(Clone, Debug)]
pub(crate) struct DefaultLightRoutePaintContract {
    backgrounds: [u32; 8],
    primary_text: u32,
    muted_text: u32,
    primary_stroke: u32,
}

#[cfg(test)]
impl DefaultLightRoutePaintContract {
    fn background(&self, surface: LibraryPrimarySurface) -> Option<u32> {
        let value = self.backgrounds[surface.index()];
        (value != 0).then_some(value)
    }

    pub(crate) fn is_opaque_and_contrasted(&self) -> bool {
        let backgrounds_are_opaque_primary = self
            .backgrounds
            .iter()
            .filter(|background| **background != 0)
            .all(|background| {
                *background == EVERNOTE_LIGHT_PRIMARY_SURFACE && (*background & 0xff) == 0xff
            });
        let text_is_legible = [self.primary_text, self.muted_text]
            .into_iter()
            .filter(|text| *text != 0)
            .all(|text| contrast_ratio(EVERNOTE_LIGHT_PRIMARY_SURFACE, text) >= 4.5);
        backgrounds_are_opaque_primary
            && text_is_legible
            && self.primary_stroke == EVERNOTE_LIGHT_PRIMARY_STROKE
    }
}

#[cfg(test)]
fn contrast_ratio(background: u32, foreground: u32) -> f32 {
    fn channel_luminance(channel: u8) -> f32 {
        let normalized = f32::from(channel) / 255.0;
        if normalized <= 0.04045 {
            normalized / 12.92
        } else {
            ((normalized + 0.055) / 1.055).powf(2.4)
        }
    }
    fn luminance(color: u32) -> f32 {
        let [red, green, blue, _] = color.to_be_bytes();
        0.2126 * channel_luminance(red)
            + 0.7152 * channel_luminance(green)
            + 0.0722 * channel_luminance(blue)
    }

    let light = luminance(background).max(luminance(foreground));
    let dark = luminance(background).min(luminance(foreground));
    (light + 0.05) / (dark + 0.05)
}

/// A per-mounted-shell observation of GPUI's actual sidebar uniform-list
/// requests. It is deliberately unavailable to production so the virtualized
/// sidebar has no cross-window mutable instrumentation.
#[cfg(test)]
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct SidebarRenderProbe {
    pub(crate) request_count: usize,
    pub(crate) largest_requested_range: usize,
    pub(crate) last_requested_range: Option<std::ops::Range<usize>>,
}

#[cfg(test)]
impl SidebarRenderProbe {
    fn record(&mut self, range: std::ops::Range<usize>) {
        self.request_count += 1;
        self.largest_requested_range = self.largest_requested_range.max(range.len());
        self.last_requested_range = Some(range);
    }
}

/// A mounted-view observation rather than a repository-only assertion. The
/// regression this protects was specifically a valid DB relation whose
/// already-mounted surface/card still displayed the preceding state.
#[cfg(test)]
#[derive(Clone, Debug)]
pub(crate) struct ImageFlowProbe {
    pub(crate) note_id: NoteId,
    /// Proves a lifecycle event retained the same stage/commit owner instead
    /// of merely repainting the same NoteId through a replacement session.
    pub(crate) session_entity_id: gpui::EntityId,
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
    cancelled: Arc<AtomicBool>,
    #[cfg(test)]
    cancellation_sender: Option<Sender<()>>,
}

impl EventTaskLifetime {
    #[cfg(test)]
    fn observed() -> (Self, Receiver<()>) {
        let (cancellation_sender, cancellation_receiver) = mpsc::channel();
        (
            Self {
                cancelled: Arc::new(AtomicBool::new(false)),
                cancellation_sender: Some(cancellation_sender),
            },
            cancellation_receiver,
        )
    }

    #[cfg(not(test))]
    fn unobserved() -> Self {
        Self {
            cancelled: Arc::new(AtomicBool::new(false)),
        }
    }

    fn cancellation_flag(&self) -> Arc<AtomicBool> {
        Arc::clone(&self.cancelled)
    }
}

impl Drop for EventTaskLifetime {
    fn drop(&mut self) {
        self.cancelled
            .store(true, std::sync::atomic::Ordering::Release);
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
        KeyBinding::new("cmd-k", ToggleSearchPalette, Some("LibraryShell")),
        // Compact-toolbar presentation commands deliberately remain scoped to
        // the library window. They do not introduce model mutations outside
        // the existing typed AppAction reducer.
        KeyBinding::new("cmd-alt-m", ToggleLibraryToolbarMore, Some("LibraryShell")),
        KeyBinding::new(
            "cmd-alt-g",
            ToggleLibraryOrganizationPanel,
            Some("LibraryShell"),
        ),
        KeyBinding::new("cmd-alt-i", OpenLibraryResourcePicker, Some("LibraryShell")),
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

    fn evernote_primary_text_fill(&self) -> gpui::Rgba {
        #[cfg(test)]
        self.library_surface_paint_hooks_for_test
            .primary_text
            .store(EVERNOTE_LIGHT_PRIMARY_TEXT, Ordering::Relaxed);
        rgba(EVERNOTE_LIGHT_PRIMARY_TEXT)
    }

    fn evernote_muted_text_fill(&self) -> gpui::Rgba {
        #[cfg(test)]
        self.library_surface_paint_hooks_for_test
            .muted_text
            .store(EVERNOTE_LIGHT_MUTED_TEXT, Ordering::Relaxed);
        rgba(EVERNOTE_LIGHT_MUTED_TEXT)
    }

    fn evernote_primary_stroke(&self) -> gpui::Rgba {
        #[cfg(test)]
        self.library_surface_paint_hooks_for_test
            .primary_stroke
            .store(EVERNOTE_LIGHT_PRIMARY_STROKE, Ordering::Relaxed);
        rgba(EVERNOTE_LIGHT_PRIMARY_STROKE)
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
        let organization_input = cx.new(|input_cx| TitleInput::new(String::new(), input_cx));
        let search_input = cx.new(|input_cx| TitleInput::new(String::new(), input_cx));
        let image_cache = BudgetedImageCache::new_entity_in_context(cx, DECODED_IMAGE_CACHE_BUDGET);
        let card_thumbnail_cache =
            BudgetedImageCache::new_entity_in_context(cx, CARD_THUMBNAIL_CACHE_BUDGET);
        let card_thumbnails = CardThumbnailManager::new(model.read(cx).repository());
        let observation = cx.observe(&model, |shell, _, cx| {
            // A model transition can replace the current route, selection, or
            // durable target. Never let a stale two-step intent survive it.
            shell.pending_destructive_action = None;
            shell.toolbar_more_open = false;
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
        window.on_window_should_close(cx, move |window, app| {
            match close_shell.update(app, |shell, shell_cx| {
                shell.flush_active_session(FlushReason::WindowClose, shell_cx)
            }) {
                Ok(allow_close) => allow_close,
                // A weak-session update error is not proof that the old
                // session was clean. Block close and leave a platform-visible
                // explanation instead of using the old fail-open default.
                Err(_) => {
                    let buttons = ["好"];
                    let _ = window.prompt(
                        gpui::PromptLevel::Critical,
                        "无法安全关闭 Joplin Lite",
                        Some("无法确认当前资料库会话是否已保存；请保持窗口打开后重试。"),
                        &buttons,
                        app,
                    );
                    false
                }
            }
        });
        let event_receiver = model.read(cx).subscribe_library_events();
        let indexing_receiver = model.read(cx).subscribe_library_events();
        #[cfg(test)]
        let (event_task_lifetime, event_task_cancellation_receiver) = EventTaskLifetime::observed();
        #[cfg(test)]
        let (indexing_task_lifetime, indexing_task_cancellation_receiver) =
            EventTaskLifetime::observed();
        #[cfg(not(test))]
        let event_task_lifetime = EventTaskLifetime::unobserved();
        #[cfg(not(test))]
        let indexing_task_lifetime = EventTaskLifetime::unobserved();
        let indexing_cancelled = indexing_task_lifetime.cancellation_flag();
        let event_task = Self::spawn_event_bridge(event_receiver, event_task_lifetime, cx);
        let indexing_task = Self::spawn_index_scheduler(
            model.read(cx).repository(),
            indexing_receiver,
            indexing_task_lifetime,
            indexing_cancelled,
            cx,
        );
        let search_input_observation = cx.observe(&search_input, |shell, _, cx| {
            if shell.search_palette_open {
                shell.schedule_search_from_input(cx);
            }
        });
        let mut shell = Self {
            model,
            note_session: None,
            _note_session_observation: None,
            editor_surface: None,
            command_chrome: None,
            _command_chrome_event_subscription: None,
            _editor_surface_event_subscription: None,
            image_cache,
            card_thumbnail_cache,
            card_thumbnail_cache_resources: HashSet::new(),
            card_thumbnails,
            card_thumbnail_viewport_state: Rc::new(RefCell::new(
                CardThumbnailViewportState::default(),
            )),
            _card_thumbnail_task: None,
            surface_note_id: None,
            remount_current_surface_after_organization_commit: false,
            pending_resource_insert: None,
            last_resource_picker_cancelled_by_reconciliation: None,
            next_resource_picker_token: 1,
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
            organization_input,
            search_input,
            _search_input_observation: Some(search_input_observation),
            search_palette_open: false,
            search_palette_results: Vec::new(),
            search_palette_status: SearchPaletteStatus::Idle,
            search_palette_generation: None,
            search_palette_selected: 0,
            search_palette_scroll: ScrollHandle::new(),
            search_palette_return_focus: None,
            search_palette_return_organization_panel_open: false,
            _search_task: None,
            _history_search_task: None,
            _search_route_refresh_task: None,
            #[cfg(test)]
            search_completion_gate: None,
            history_search_notice: None,
            search_refresh_retry_available: false,
            search_refresh_retry_history: None,
            organization_panel_open: false,
            pending_destructive_action: None,
            sidebar_scroll: UniformListScrollHandle::new(),
            note_list_scroll: UniformListScrollHandle::new(),
            toolbar_more_open: false,
            _model_observation: observation,
            _event_task: event_task,
            _indexing_task: indexing_task,
            indexing_status: IndexingStatus::Pending,
            #[cfg(test)]
            indexing_task_cancellation_receiver: Some(indexing_task_cancellation_receiver),
            #[cfg(test)]
            event_task_cancellation_receiver: Some(event_task_cancellation_receiver),
            #[cfg(test)]
            rendered_note_range: None,
            #[cfg(test)]
            last_scroll_request: None,
            #[cfg(test)]
            sidebar_render_probe: SidebarRenderProbe::default(),
            #[cfg(test)]
            card_thumbnail_cache_reconciliations: 0,
            #[cfg(test)]
            card_thumbnail_viewport_reconciliations: 0,
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

    #[cfg(test)]
    pub(crate) fn record_sidebar_uniform_list_range_for_test(
        &mut self,
        range: std::ops::Range<usize>,
    ) {
        self.sidebar_render_probe.record(range);
    }

    #[cfg(test)]
    pub(crate) fn sidebar_render_probe_for_test(&self) -> SidebarRenderProbe {
        self.sidebar_render_probe.clone()
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
            // An event that cannot yet cross the active-session lifecycle
            // boundary remains owned here. Consuming it directly from the
            // receiver and then remounting would discard dirty, marked-IME,
            // or staged-resource state without another chance to reconcile.
            let mut pending_events = PendingRepositoryEvents::default();
            loop {
                cx.background_executor()
                    .timer(Duration::from_millis(50))
                    .await;
                // Keep draining while a prior event batch waits behind a
                // lifecycle barrier. `PendingRepositoryEvents` coalesces to
                // five semantic kinds, so a long IME/resource wait cannot
                // turn the repository sender into an unbounded UI buffer.
                pending_events.drain(&receiver);
                if pending_events.is_empty() {
                    continue;
                }
                let events = pending_events.events();
                match this.update(cx, |shell, shell_cx| {
                    shell.reconcile_repository_events(&events, shell_cx)
                }) {
                    Ok(true) => pending_events.clear(),
                    Ok(false) => {
                        // Keep the exact batch for the next bounded timer
                        // turn. New sender-side events stay queued until the
                        // current lifecycle barrier has honestly completed.
                    }
                    Err(_) => {
                        // The shell owns the task, but stop promptly as well
                        // if an already-queued timer wakes after its entity
                        // died.
                        break;
                    }
                }
            }
        })
    }

    fn spawn_index_scheduler(
        repository: Arc<LibraryRepository>,
        receiver: Receiver<app_lite_core::LibraryEvent>,
        indexing_task_lifetime: EventTaskLifetime,
        indexing_cancelled: Arc<AtomicBool>,
        cx: &mut Context<Self>,
    ) -> Task<()> {
        #[cfg(test)]
        let mut worker_gate = INDEXING_WORKER_GATE
            .lock()
            .expect("indexing worker gate mutex poisoned")
            .take();
        #[cfg(test)]
        let worker_on_thread = INDEXING_WORKER_THREAD_FOR_TEST.swap(false, Ordering::AcqRel);
        cx.spawn(async move |this, cx| {
            let _indexing_task_lifetime = indexing_task_lifetime;
            // An open profile may already have durable work from a prior run.
            // Afterwards SearchProjectionQueued wakes one coalesced batch;
            // no GPUI state retains canonical text or creates per-note tasks.
            let mut scheduled = true;
            loop {
                if scheduled {
                    scheduled = false;
                    // `Context::spawn` is foreground-only. The SQLite/FTS
                    // batch must run on GPUI's actual background executor so
                    // it cannot monopolize typing, navigation, or Cmd-Q.
                    let worker_repository = Arc::clone(&repository);
                    let worker_cancelled = Arc::clone(&indexing_cancelled);
                    #[cfg(test)]
                    let gate = worker_gate.take();
                    #[cfg(test)]
                    let result = if worker_on_thread {
                        let thread_repository = Arc::clone(&worker_repository);
                        let thread_cancelled = Arc::clone(&worker_cancelled);
                        let (sender, receiver) = futures::channel::oneshot::channel();
                        std::thread::spawn(move || {
                            let _ = sender.send(
                                thread_repository
                                    .process_search_jobs_until_cancelled(&thread_cancelled),
                            );
                        });
                        receiver
                            .await
                            .unwrap_or(Err(app_lite_core::LibraryError::InvalidSnapshot))
                    } else {
                        cx.background_executor()
                            .spawn(async move {
                                #[cfg(test)]
                                if let Some(gate) = gate {
                                    let _ = gate.started.send(());
                                    if gate.release.await.is_err() {
                                        return Err(app_lite_core::LibraryError::InvalidSnapshot);
                                    }
                                }
                                worker_repository
                                    .process_search_jobs_until_cancelled(&worker_cancelled)
                            })
                            .await
                    };
                    #[cfg(not(test))]
                    let result = cx
                        .background_executor()
                        .spawn(async move {
                            worker_repository.process_search_jobs_until_cancelled(&worker_cancelled)
                        })
                        .await;
                    match result {
                        Ok(completed) => {
                            // A full batch means there may be more work, but
                            // yield before another bounded batch so typing and
                            // navigation retain their normal GPUI turns.
                            scheduled = completed == 100;
                            let status = if scheduled {
                                IndexingStatus::Pending
                            } else {
                                IndexingStatus::Idle
                            };
                            if this
                                .update(cx, |shell, shell_cx| {
                                    shell.indexing_status = status;
                                    // A note save emits its projection event
                                    // before Stage B1 has committed its FTS
                                    // row. Waiting for the queue to become
                                    // idle ensures an active SearchRoute is
                                    // refreshed from the new index, rather
                                    // than immediately reading stale matches.
                                    if !scheduled {
                                        shell.schedule_active_search_refresh(shell_cx);
                                    }
                                    shell_cx.notify();
                                })
                                .is_err()
                            {
                                break;
                            }
                        }
                        Err(error) => {
                            // Do not acknowledge or convert this into a save
                            // failure. The core worker leaves the exact queue
                            // identity durable for a later event or reopen.
                            if this
                                .update(cx, |shell, shell_cx| {
                                    shell.indexing_status =
                                        IndexingStatus::Failed(format!("本地索引待重试：{error}"));
                                    shell_cx.notify();
                                })
                                .is_err()
                            {
                                break;
                            }
                        }
                    }
                }

                cx.background_executor()
                    .timer(Duration::from_millis(50))
                    .await;
                let mut queued = false;
                for event in receiver.try_iter().take(128) {
                    if matches!(
                        event,
                        app_lite_core::LibraryEvent::SearchProjectionQueued(_)
                    ) {
                        queued = true;
                    }
                }
                if queued {
                    scheduled = true;
                    if this
                        .update(cx, |shell, shell_cx| {
                            if !matches!(shell.indexing_status, IndexingStatus::Failed(_)) {
                                shell.indexing_status = IndexingStatus::Pending;
                            }
                            shell_cx.notify();
                        })
                        .is_err()
                    {
                        break;
                    }
                }
            }
        })
    }

    /// Apply one repository batch only after the retained active session is
    /// at a lifecycle-safe point. A metadata event can alter the current
    /// note's revision, deleted state, or route membership; replacing its
    /// entity before a dirty/IME/resource operation flushes is data loss, not
    /// a harmless projection refresh.
    fn reconcile_repository_events(
        &mut self,
        events: &[app_lite_core::LibraryEvent],
        cx: &mut Context<Self>,
    ) -> bool {
        let reconciliation_pending = self
            .model
            .read_with(cx, |model, _| model.reconciliation_pending());
        let can_replace_active_session = self.model.read_with(cx, |model, _| {
            // If a metadata-only probe itself fails, remain conservative and
            // keep the lifecycle barrier. The later full refresh will expose
            // the same repository error without discarding the session.
            model
                .event_batch_requires_active_session_replacement(events)
                .unwrap_or(true)
        });
        // A committed-but-unreconciled session is already frozen. A durable
        // Trash preview is also non-writable, so neither can have title/body
        // changes that need a lifecycle flush before its candidate replaces
        // the old entity.
        if can_replace_active_session
            && !reconciliation_pending
            && !self.active_session_is_read_only(cx)
            && !self.flush_active_session(FlushReason::NoteSwitch, cx)
        {
            return false;
        }
        let refreshed = self
            .model
            .update(cx, |model, model_cx| {
                let result = model.refresh_projection_events(events.iter().cloned());
                model_cx.notify();
                result
            })
            .is_ok();
        // Consult the durable queue in the background coordinator. The
        // coalesced UI event packet intentionally does not retain every
        // SearchProjectionQueued event, so event shape cannot safely decide
        // whether FTS is already current. A queued row defers the reread to
        // the index worker's next Idle edge; an empty queue (tag rename) reads
        // immediately.
        if refreshed {
            self.schedule_active_search_refresh(cx);
        }
        refreshed
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
    pub(crate) fn stall_next_card_thumbnail_materialization_for_test(
        &mut self,
    ) -> futures::channel::oneshot::Sender<()> {
        self.card_thumbnails.stall_next_materialization_for_test()
    }

    #[cfg(test)]
    pub(crate) fn card_thumbnail_cache_reconciliations_for_test(&self) -> usize {
        self.card_thumbnail_cache_reconciliations
    }

    #[cfg(test)]
    pub(crate) fn card_thumbnail_viewport_reconciliations_for_test(&self) -> usize {
        self.card_thumbnail_viewport_reconciliations
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

    #[cfg(test)]
    pub(crate) fn default_light_route_paint_contract_for_test(
        &self,
    ) -> DefaultLightRoutePaintContract {
        DefaultLightRoutePaintContract {
            backgrounds: std::array::from_fn(|index| {
                self.library_surface_paint_hooks_for_test
                    .primary_surface_fills[index]
                    .load(Ordering::Relaxed)
            }),
            primary_text: self
                .library_surface_paint_hooks_for_test
                .primary_text
                .load(Ordering::Relaxed),
            muted_text: self
                .library_surface_paint_hooks_for_test
                .muted_text
                .load(Ordering::Relaxed),
            primary_stroke: self
                .library_surface_paint_hooks_for_test
                .primary_stroke
                .load(Ordering::Relaxed),
        }
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
    pub(crate) fn indexing_status_for_test(&self) -> IndexingStatus {
        self.indexing_status.clone()
    }

    #[cfg(test)]
    pub(crate) fn set_resource_notice_for_test(&mut self, notice: impl Into<String>) {
        self.resource_notice = Some(notice.into());
    }

    #[cfg(test)]
    fn install_search_completion_gate_for_test(&mut self, gate: SearchCompletionGate) {
        assert!(
            self.search_completion_gate.replace(gate).is_none(),
            "each mounted shell owns at most one search completion gate"
        );
    }

    #[cfg(test)]
    fn take_indexing_task_cancellation_receiver_for_test(&mut self) -> Receiver<()> {
        self.indexing_task_cancellation_receiver
            .take()
            .expect("indexing task cancellation receiver is taken only once per test")
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
        let session_entity_id = session.entity_id();
        let card_thumbnail_id = self.model.read_with(cx, |model, _| {
            model
                .projections()
                .iter()
                .find(|projection| projection.id == note_id)
                .and_then(|projection| projection.selected_thumbnail_id.clone())
        });
        ImageFlowProbe {
            note_id,
            session_entity_id,
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
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let _ = self.apply_action_with_result(action, window, cx);
    }

    /// Runs the one shell reducer and reports only whether its durable model
    /// action completed. This is deliberately private: callers which need a
    /// post-success UI effect (the shared organization title field) must not
    /// build a second mutation path around `AppModel::dispatch`.
    fn apply_action_with_result(
        &mut self,
        action: AppAction,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        // A committed repository mutation is waiting for one coherent
        // candidate. Do not let a same-card click, a second destructive menu
        // action, or a shortcut mutate the stale retained revision and clear
        // its truthful warning. The queued repository event owns recovery.
        if self
            .model
            .read_with(cx, |model, _| model.reconciliation_pending())
        {
            self.set_active_session_reconciliation_lock(true, cx);
            cx.notify();
            return false;
        }
        if let AppAction::NavigateBack | AppAction::NavigateForward = action {
            let forward = matches!(action, AppAction::NavigateForward);
            if self.model.read_with(cx, |model, _| {
                model.pending_history_search_query(forward).is_some()
            }) {
                if let Some(reason) = self.flush_reason_for_action(&action)
                    && !self.flush_active_session(reason, cx)
                {
                    return false;
                }
                return self.schedule_history_search(forward, cx);
            }
        }
        // Restore and purge are the two lifecycle transitions which make a
        // Trash detail legal again or remove it forever. A deliberately
        // read-only Trash session has no pending durable mutation, and a
        // stale Failed state must not strand the user's only recovery path.
        let bypass_read_only_trash_flush = self.active_session_is_durable_read_only(cx)
            && matches!(
                action,
                AppAction::RestoreNote(_)
                    | AppAction::RestoreSelected
                    | AppAction::PurgeNote(_)
                    | AppAction::PurgeSelected
            );
        if !bypass_read_only_trash_flush
            && let Some(reason) = self.flush_reason_for_action(&action)
            && !self.flush_active_session(reason, cx)
        {
            return false;
        }
        let remount_current_surface = self.action_can_change_active_session_revision(&action);
        if remount_current_surface {
            // Set this before the model notification so even an eager GPUI
            // observer cannot reconcile the old fence first. A failed reducer
            // result below clears it before any future model transition.
            self.remount_current_surface_after_organization_commit = true;
        }
        let result = self.model.update(cx, |model, model_cx| {
            let result = model.dispatch(action);
            model_cx.notify();
            result
        });
        if result.is_err() {
            self.remount_current_surface_after_organization_commit = false;
            if self
                .model
                .read_with(cx, |model, _| model.reconciliation_pending())
            {
                // `model_cx.notify` is intentionally asynchronous relative
                // to this reducer. Close the mutation window in this same UI
                // turn instead of waiting for the retained observer.
                self.set_active_session_reconciliation_lock(true, cx);
            }
        }
        // The retained model observer owns surface synchronization, deferred
        // scrolling, and shell invalidation. Keeping this reducer to model
        // mutation plus notification makes every user and event route share
        // the exact same visible-state bridge.
        result.is_ok()
    }

    /// The organization panel owns one shared `TitleInput`. Clear it only
    /// after the authoritative reducer reports success: validation, flush, or
    /// committed-but-unreconciled failures retain the user's title for a
    /// visible correction/retry instead of silently losing it.
    fn submit_organization_create(
        &mut self,
        action: AppAction,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        debug_assert!(matches!(
            action,
            AppAction::CreateNotebook { .. }
                | AppAction::CreateStack { .. }
                | AppAction::CreateTag { .. }
        ));
        if self.apply_action_with_result(action, window, cx) {
            self.organization_input.update(cx, |input, input_cx| {
                input.select_all();
                input.delete_forward();
                input_cx.notify();
            });
        }
    }

    fn active_session_is_read_only(&self, cx: &App) -> bool {
        self.note_session
            .as_ref()
            .is_some_and(|session| session.read_with(cx, |session, _| session.is_read_only()))
    }

    fn active_session_is_durable_read_only(&self, cx: &App) -> bool {
        self.note_session.as_ref().is_some_and(|session| {
            session.read_with(cx, |session, _| session.is_durable_read_only())
        })
    }

    fn resource_mutation_block_message(&self, operation: &str, cx: &App) -> String {
        if self.active_session_is_durable_read_only(cx) {
            format!("废纸篓中的笔记为只读；请先恢复后再{operation}")
        } else {
            format!("资料库已提交，正在恢复界面；暂不可{operation}")
        }
    }

    fn editor_surface_mode_for_session(session: &NoteSession) -> EditorSurfaceMode {
        if session.is_durable_read_only() {
            EditorSurfaceMode::ReadOnly
        } else if session.is_reconciliation_locked() {
            EditorSurfaceMode::RecoveryLocked
        } else {
            EditorSurfaceMode::Editable
        }
    }

    /// Keep title input, core and canvas in one temporary access state while
    /// the AppModel waits for a full committed-action candidate. This mirrors
    /// the session's hard core gate; it is not a second mutation authority.
    fn set_active_session_reconciliation_lock(&mut self, locked: bool, cx: &mut Context<Self>) {
        let Some(session) = self.note_session.clone() else {
            return;
        };
        let was_locked = session.read_with(cx, |session, _| session.is_reconciliation_locked());
        if locked && !was_locked {
            self.invalidate_resource_intents_for_reconciliation_lock(&session, cx);
        }
        let _ = session.update(cx, |session, session_cx| {
            session.set_reconciliation_locked(locked, session_cx);
        });
        let mode = session.read_with(cx, |session, _| {
            Self::editor_surface_mode_for_session(session)
        });
        if let Some(surface) = self.editor_surface.clone() {
            let _ = surface.update(cx, |surface, surface_cx| {
                surface.set_mode(mode);
                surface_cx.notify();
            });
        }
    }

    /// Captured external-resource intents are revision-scoped.  A
    /// committed-but-unreconciled organization action freezes the old packet,
    /// so leave neither a native picker callback nor a queued save-fence
    /// completion able to resolve its old anchor after recovery unlocks.
    fn invalidate_resource_intents_for_reconciliation_lock(
        &mut self,
        session: &Entity<NoteSession>,
        cx: &mut Context<Self>,
    ) {
        let mut invalidated = false;
        if let Some(pending) = self.pending_resource_insert.take() {
            Self::discard_resource_insert_intent(session, pending.intent, cx);
            self.last_resource_picker_cancelled_by_reconciliation = Some(pending.token);
            invalidated = true;
        }
        if let Some(intent) = self.pending_drop_intent.take() {
            Self::discard_resource_insert_intent(session, intent, cx);
            invalidated = true;
        }
        while let Some(queued) = self.queued_resource_inserts.pop_front() {
            cleanup_owned_temporary_paths(&queued.owned_temporary_paths);
            Self::discard_resource_insert_intent(session, queued.intent, cx);
            invalidated = true;
        }
        self.resource_queue_completion_scheduled = false;
        if invalidated {
            self.resource_notice = Some(
                "资料库已提交，正在恢复界面；已取消尚未完成的资源插入，请恢复后重新选择".to_owned(),
            );
            cx.notify();
        }
    }

    fn flush_reason_for_action(&self, action: &AppAction) -> Option<FlushReason> {
        let active_id = self.surface_note_id.clone();
        match action {
            AppAction::CreateNote => active_id.map(|_| FlushReason::NoteSwitch),
            AppAction::SelectNote(id) if active_id.as_ref() != Some(id) => {
                active_id.map(|_| FlushReason::NoteSwitch)
            }
            // Route membership is verified by the later, projection-backed
            // AppModel candidate. A metadata preflight is useful for passing
            // a likely-valid NoteId, but cannot atomically cover the interval
            // before that candidate reads SQLite. Always cross the existing
            // retained-session boundary first: otherwise a concurrent
            // move/tag/trash can turn an equal NoteId into an unmount and
            // silently drop the old dirty document.
            AppAction::NavigateTo { .. } => active_id.map(|_| FlushReason::NoteSwitch),
            AppAction::NavigateBack | AppAction::NavigateForward => {
                active_id.map(|_| FlushReason::NoteSwitch)
            }
            AppAction::TrashSelected => active_id.map(|_| FlushReason::Delete),
            AppAction::TrashNote(id) if active_id.as_ref() == Some(id) => Some(FlushReason::Delete),
            AppAction::MoveSelectedNote(_)
            | AppAction::SetSelectedNoteTags(_)
            | AppAction::AddTagToSelectedNote(_)
            | AppAction::RemoveTagFromSelectedNote(_)
            | AppAction::DeleteStack(_)
            | AppAction::DeleteNotebook(_)
            | AppAction::DeleteTag(_) => active_id.map(|_| FlushReason::ManualSync),
            AppAction::RestoreNote(id) | AppAction::PurgeNote(id)
                if active_id.as_ref() == Some(id) =>
            {
                Some(FlushReason::ManualSync)
            }
            AppAction::RestoreSelected | AppAction::PurgeSelected => {
                active_id.map(|_| FlushReason::ManualSync)
            }
            AppAction::ManualSync => active_id.map(|_| FlushReason::ManualSync),
            _ => None,
        }
    }

    fn action_can_change_active_session_revision(&self, action: &AppAction) -> bool {
        if self.surface_note_id.is_none() {
            return false;
        }
        matches!(
            action,
            AppAction::MoveSelectedNote(_)
                | AppAction::SetSelectedNoteTags(_)
                | AppAction::AddTagToSelectedNote(_)
                | AppAction::RemoveTagFromSelectedNote(_)
                | AppAction::DeleteNotebook(_)
                | AppAction::DeleteTag(_)
        )
    }

    fn flush_active_session(&mut self, reason: FlushReason, cx: &mut Context<Self>) -> bool {
        let Some(session) = self.note_session.clone() else {
            return true;
        };
        match session.update(cx, |session, session_cx| session.flush(reason, session_cx)) {
            Ok(_) => {
                // A save worker can finish immediately before the retained
                // session observer gets its next turn. Consume its complete
                // transaction-built Note at this explicit boundary as well,
                // so an organization action in the very same UI turn never
                // prepares a replacement session from an older active body.
                self.consume_saved_note_outcome(&session, cx);
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

    fn active_session_has_unsaved_changes(&self, cx: &App) -> bool {
        self.note_session
            .as_ref()
            .is_some_and(|session| !matches!(session.read(cx).save_state(), SaveState::Clean))
    }

    fn active_session_save_fence(&self, cx: &App) -> Option<(NoteId, i64, i64)> {
        self.note_session.as_ref().map(|session| {
            let note_id = self.surface_note_id.clone().unwrap_or_else(|| {
                self.model
                    .read_with(cx, |model, _| model.active_session_note_id().cloned())
                    .expect("retained session has an active note")
            });
            let session = session.read(cx);
            (
                note_id,
                session.save_generation(),
                session.expected_revision(),
            )
        })
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

    /// A failed lifecycle flush can either still have retained work in flight
    /// or be a visible terminal error. The application-level Quit coordinator
    /// retries only the first case; retrying the second would turn a failed
    /// save into a non-responsive quit command.
    pub(crate) fn lifecycle_flush_pending(&self) -> bool {
        self.save_pending
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

    fn toggle_library_toolbar_more(
        &mut self,
        _: &ToggleLibraryToolbarMore,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.toggle_toolbar_more(window, cx);
    }

    fn toggle_search_palette(
        &mut self,
        _: &ToggleSearchPalette,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.toggle_search_palette_visibility(window, cx);
    }

    fn toggle_library_organization_panel(
        &mut self,
        _: &ToggleLibraryOrganizationPanel,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.toggle_organization_panel_visibility(window, cx);
    }

    fn open_library_resource_picker(
        &mut self,
        _: &OpenLibraryResourcePicker,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // Capture the document point synchronously, then launch AppKit only
        // after this action update returns. This shares the existing Task-5
        // picker token/completion path without re-entering the current window
        // borrow from a keyboard handler.
        match self.prompt_for_resource_picker(window, cx) {
            Ok(prompt) => {
                #[cfg(not(test))]
                window.defer(cx, move |_window, app| {
                    prompt_for_resource_path(prompt, app)
                });
                #[cfg(test)]
                let _ = prompt;
            }
            Err(error) => {
                self.resource_notice = Some(error);
                cx.notify();
            }
        }
    }

    fn sync_editor_surface(&mut self, cx: &mut Context<Self>) {
        let note = self
            .model
            .read_with(cx, |model, _| model.active_note().cloned());
        let reconciliation_pending = self
            .model
            .read_with(cx, |model, _| model.reconciliation_pending());
        let next_id = note.as_ref().map(|note| note.id.clone());
        let next_revision = note.as_ref().map(|note| note.revision);
        let mounted_session_revision = self
            .note_session
            .as_ref()
            .map(|session| session.read_with(cx, |session, _| session.expected_revision()));
        if self.surface_note_id == next_id
            && mounted_session_revision == next_revision
            && !self.remount_current_surface_after_organization_commit
        {
            self.set_active_session_reconciliation_lock(reconciliation_pending, cx);
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
        self.remount_current_surface_after_organization_commit = false;
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
                if reconciliation_pending {
                    let _ = session.update(cx, |session, session_cx| {
                        session.set_reconciliation_locked(true, session_cx);
                    });
                }
                let (editor, surface_mode, durable_read_only) =
                    session.read_with(cx, |session, _| {
                        (
                            session.editor().clone(),
                            Self::editor_surface_mode_for_session(session),
                            session.is_durable_read_only(),
                        )
                    });
                // A temporary reconciliation lock keeps the same shared
                // Chrome mounted in its disabled state, so the eventual
                // candidate unlock does not need a second toolbar authority.
                // Durable Trash previews intentionally have no formatter.
                let command_chrome = (!durable_read_only).then(|| {
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
                    command_chrome
                });
                self._note_session_observation =
                    Some(cx.observe(&session, |shell, session, cx| {
                        shell.consume_saved_note_outcome(&session, cx);
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
                        // A native menu/Cmd-Q request may have started this
                        // session's exact background snapshot. Defer the
                        // cross-window completion check until this entity
                        // update has returned; it will either finish the same
                        // request or leave the visible lifecycle error intact.
                        cx.defer(crate::library_menu::retry_pending_quit);
                        cx.notify();
                    }));
                #[cfg(test)]
                let before_shape = Arc::clone(&self.library_surface_paint_hooks_for_test);
                #[cfg(test)]
                let after_paint = Arc::clone(&self.library_surface_paint_hooks_for_test);
                let image_cache = self.image_cache.clone();
                let surface = cx.new(move |cx| {
                    #[cfg(test)]
                    let mut surface =
                        EditorSurface::new(editor, surface_mode, Some(image_cache.clone()), cx);
                    #[cfg(not(test))]
                    let surface = EditorSurface::new(editor, surface_mode, Some(image_cache), cx);
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
                self.command_chrome = command_chrome;
                self.note_session = Some(session);
            }
            Err(error) => self.unsupported_document = Some(error.to_string()),
        }
    }

    /// Start the same saved-selection completion seam used by the native
    /// picker, pasteboard and drop routes.  Keeping this tiny state mutation
    /// synchronous makes cancellation harmless and lets the OS panel live
    /// outside the GPUI view borrow.
    pub(crate) fn begin_resource_picker(
        &mut self,
        cx: &mut Context<Self>,
    ) -> Result<ResourcePickerToken, String> {
        if self.active_session_is_read_only(cx) {
            return Err(self.resource_mutation_block_message("插入资源", cx));
        }
        let Some(session) = self.note_session.clone() else {
            return Err("请先选择一篇笔记再插入资源".to_owned());
        };
        let token = ResourcePickerToken(self.next_resource_picker_token);
        self.next_resource_picker_token = self.next_resource_picker_token.wrapping_add(1);
        self.pending_resource_insert = Some(PendingResourcePicker {
            token,
            intent: Self::capture_resource_insert_intent(&session, None, cx)?,
        });
        self.resource_notice = None;
        cx.notify();
        Ok(token)
    }

    /// Complete a path chosen by the platform picker. This is deliberately
    /// public within the crate so mounted tests exercise exactly the route
    /// called after the asynchronous native panel settles.
    fn complete_resource_picker_path_for_token(
        &mut self,
        token: ResourcePickerToken,
        path: PathBuf,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Result<(), String> {
        let Some(pending) = self.pending_resource_insert.as_ref() else {
            return Err(
                if self.last_resource_picker_cancelled_by_reconciliation == Some(token) {
                    "资料库已提交，正在恢复界面；已取消本次资源插入，请恢复后重新选择".to_owned()
                } else {
                    "资源选择已过期；请重新打开选择器".to_owned()
                },
            );
        };
        if pending.token != token {
            return Err(
                if self.last_resource_picker_cancelled_by_reconciliation == Some(token) {
                    "资料库已提交，正在恢复界面；已取消本次资源插入，请恢复后重新选择".to_owned()
                } else {
                    "资源选择已过期；请重新打开选择器".to_owned()
                },
            );
        }
        let intent = self
            .pending_resource_insert
            .take()
            .expect("token matched a pending picker")
            .intent;
        self.complete_resource_request(
            ResourceImportRequest::Path(path),
            intent,
            Vec::new(),
            window,
            cx,
        )
    }

    #[cfg(test)]
    pub(crate) fn complete_resource_picker_path(
        &mut self,
        path: PathBuf,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Result<(), String> {
        let token = self
            .pending_resource_insert
            .as_ref()
            .map(|pending| pending.token)
            .ok_or_else(|| "资源选择已过期；请重新打开选择器".to_owned())?;
        self.complete_resource_picker_path_for_token(token, path, window, cx)
    }

    #[cfg(test)]
    pub(crate) fn complete_resource_picker_path_for_token_for_test(
        &mut self,
        token: ResourcePickerToken,
        path: PathBuf,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Result<(), String> {
        self.complete_resource_picker_path_for_token(token, path, window, cx)
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
        if self.active_session_is_read_only(cx) {
            cleanup_owned_temporary_paths(&owned_temporary_paths);
            if let Some(session) = self.note_session.as_ref() {
                Self::discard_resource_insert_intent(session, intent, cx);
            }
            return Err(self.resource_mutation_block_message("插入资源", cx));
        }
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

    /// A settled normal save is a complete `Note` returned by the same SQLite
    /// transaction.  Install it into the model before any organization
    /// action can prepare a candidate from `active_session`; otherwise a
    /// metadata-only organization refresh can remount a stale body and lose
    /// text that was already durable.
    fn consume_saved_note_outcome(
        &mut self,
        session: &Entity<NoteSession>,
        cx: &mut Context<Self>,
    ) {
        let saved = session.update(cx, |session, _| session.take_saved_note_outcome());
        let Some(note) = saved else {
            return;
        };
        let _ = self.model.update(cx, |model, model_cx| {
            model.apply_active_note_snapshot(note);
            model_cx.notify();
        });
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
        if self.active_session_is_read_only(cx) {
            cleanup_owned_temporary_paths(&owned_temporary_paths);
            if let Some(session) = self.note_session.as_ref() {
                Self::discard_resource_insert_intent(session, intent, cx);
            }
            return Err(self.resource_mutation_block_message("插入资源", cx));
        }
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
        if session.read_with(cx, |session, _| session.is_read_only()) {
            self.invalidate_resource_intents_for_reconciliation_lock(&session, cx);
            self.resource_notice = Some(self.resource_mutation_block_message("插入资源", cx));
            cx.notify();
            return;
        }
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
    ) -> Result<ResourcePickerPrompt, String> {
        let token = self.begin_resource_picker(cx)?;
        let window = window
            .window_handle()
            .downcast::<LibraryShell>()
            .ok_or_else(|| "无法关联资源选择器到当前资料库窗口".to_owned())?;
        Ok(ResourcePickerPrompt { window, token })
    }

    /// The shared Chrome already captured the selection before it emitted its
    /// typed request.  This only packages that exact pending token for the
    /// native panel; it never captures a second, unrelated anchor.
    fn pending_resource_picker_prompt(
        &self,
        window: &mut Window,
    ) -> Result<ResourcePickerPrompt, String> {
        let token = self
            .pending_resource_insert
            .as_ref()
            .map(|pending| pending.token)
            .ok_or_else(|| "资源选择已过期；请重新打开选择器".to_owned())?;
        let window = window
            .window_handle()
            .downcast::<LibraryShell>()
            .ok_or_else(|| "无法关联资源选择器到当前资料库窗口".to_owned())?;
        Ok(ResourcePickerPrompt { window, token })
    }

    fn cancel_resource_picker(&mut self, token: ResourcePickerToken, cx: &mut Context<Self>) {
        if self
            .pending_resource_insert
            .as_ref()
            .is_some_and(|pending| pending.token == token)
            && let Some(pending) = self.pending_resource_insert.take()
            && let Some(session) = self.note_session.as_ref()
        {
            Self::discard_resource_insert_intent(session, pending.intent, cx);
        }
        cx.notify();
    }

    /// Native `Paste` is intentionally handled by the library shell, not by
    /// `EditorCore`'s text input handler: an image/file must reach the same
    /// durable resource transaction as picker and Finder drop, while ordinary
    /// text keeps the existing EntityInputHandler path.
    fn paste_resource_or_text(&mut self, _: &Paste, window: &mut Window, cx: &mut Context<Self>) {
        if self.active_session_is_read_only(cx) {
            self.resource_notice = Some(self.resource_mutation_block_message("编辑", cx));
            cx.notify();
            return;
        }
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
        if self.active_session_is_read_only(cx) {
            if let Some(session) = self.note_session.as_ref() {
                Self::discard_resource_insert_intent(session, saved_intent, cx);
            }
            return Err(self.resource_mutation_block_message("编辑", cx));
        }
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
        if self.active_session_is_read_only(cx) {
            return;
        }
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
        if self.active_session_is_read_only(cx) {
            self.resource_notice = Some(self.resource_mutation_block_message("拖入资源", cx));
            cx.notify();
            return;
        }
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
        if self.active_session_is_read_only(cx) {
            if let Some(session) = self.note_session.as_ref() {
                Self::discard_resource_insert_intent(session, intent, cx);
            }
            return Err(self.resource_mutation_block_message("拖入资源", cx));
        }
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

    /// Reconcile Cards-only thumbnail residency from the uniform-list's
    /// bounded construction range. This does no filesystem work: resource
    /// verification/copying is started below on the retained background task,
    /// while the GPUI thread only swaps IDs and requests small cache proxies.
    fn reconcile_card_thumbnail_residency(
        &mut self,
        resource_ids: impl IntoIterator<Item = app_lite_core::ResourceId>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        #[cfg(test)]
        {
            self.card_thumbnail_viewport_reconciliations = self
                .card_thumbnail_viewport_reconciliations
                .saturating_add(1);
        }
        self.card_thumbnails.reconcile_desired(resource_ids);
        self.sync_card_thumbnail_cache_residency(window, cx);
        self.start_next_card_thumbnail_materialization(cx);
    }

    /// Tear down card-specific resources only when the Cards presentation or
    /// its list itself disappears. A Cards viewport with no image keys is
    /// deliberately reconciled through `reconcile_card_thumbnail_residency`:
    /// it should release decoded cache entries while retaining the small,
    /// bounded source LRU for a nearby image card.
    fn leave_card_thumbnail_residency(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.card_thumbnail_viewport_state.borrow_mut().invalidate();
        self.card_thumbnails.leave_cards();
        self.sync_card_thumbnail_cache_residency(window, cx);
    }

    /// Install a new visible source set only when the final UniformList range
    /// or a worker completion actually changed it. `Entity::update` itself is
    /// observable by GPUI, so an unconditional cache update here causes
    /// render -> deferred range -> cache update -> render even when no source
    /// or request changed.
    fn sync_card_thumbnail_cache_residency(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let resources = self.card_thumbnails.visible_resources();
        let next_resources = resources.iter().cloned().collect::<HashSet<_>>();
        if next_resources == self.card_thumbnail_cache_resources {
            return;
        }
        self.card_thumbnail_cache.update(cx, |cache, cache_cx| {
            cache.set_visible_resources(resources.iter());
            for resource in &resources {
                // Every Card source is already a bounded 192px proxy. Tell
                // the cache its natural edge so it never promotes a 192px
                // PNG to the quantized 256px tier on later paints.
                cache.request_edge_with_natural_max(
                    resource,
                    CARD_THUMBNAIL_PROXY_EDGE,
                    Some(CARD_THUMBNAIL_PROXY_EDGE),
                );
            }
            cache.evict_offscreen(window, cache_cx);
        });
        self.card_thumbnail_cache_resources = next_resources;
        #[cfg(test)]
        {
            self.card_thumbnail_cache_reconciliations =
                self.card_thumbnail_cache_reconciliations.saturating_add(1);
        }
    }

    fn start_next_card_thumbnail_materialization(&mut self, cx: &mut Context<Self>) {
        if self._card_thumbnail_task.is_some() {
            return;
        }
        let Some(job) = self.card_thumbnails.next_job() else {
            return;
        };
        let worker = cx
            .background_executor()
            .spawn(async move { CardThumbnailManager::materialize(job).await });
        self._card_thumbnail_task = Some(cx.spawn(async move |this, cx| {
            let completion = worker.await;
            let _ = this.update(cx, |shell, shell_cx| {
                shell._card_thumbnail_task = None;
                shell.card_thumbnails.finish(completion);
                // The desired viewport may be unchanged while this worker
                // added its first source. Ask the next actual-prepaint
                // decoration to install that source exactly once.
                shell
                    .card_thumbnail_viewport_state
                    .borrow_mut()
                    .invalidate();
                shell.start_next_card_thumbnail_materialization(shell_cx);
                // A successful source is adopted only in this live-shell
                // callback. The next frame registers it with the card cache;
                // a dropped shell never recreates a stale editor cache root.
                shell_cx.notify();
            });
        }));
    }

    fn render_note_list(
        &mut self,
        items: Vec<NoteProjection>,
        selected: Option<NoteId>,
        list_width: u16,
        visible: bool,
        mode: ListViewMode,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        if !visible {
            self.leave_card_thumbnail_residency(window, cx);
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
        let viewport_state_for_processor = Rc::clone(&self.card_thumbnail_viewport_state);
        let selected_for_processor = selected.clone();
        let items = Arc::new(items);
        let items_for_processor = Arc::clone(&items);
        let item_count = items.len();
        if !matches!(mode, ListViewMode::Cards) || item_count == 0 {
            self.leave_card_thumbnail_residency(window, cx);
        }
        let list = uniform_list(
            "library-note-list-items",
            item_count,
            cx.processor(move |_this, range: std::ops::Range<usize>, _window, _cx| {
                // The actual card residency side effect lives exclusively in
                // the prepaint decoration below. Retain this legacy probe for
                // scale tests that inspect UniformList construction ranges;
                // it is test-only and never schedules I/O or cache work.
                #[cfg(test)]
                {
                    _this.rendered_note_range = Some(range.clone());
                }
                note_list::record_constructed_items(range.len());
                range
                    .filter_map(|index| {
                        let projection = items_for_processor.get(index)?.clone();
                        let id = projection.id.clone();
                        let selected = selected_for_processor.as_ref() == Some(&id);
                        let thumbnail_is_actually_visible = !matches!(mode, ListViewMode::Cards)
                            || viewport_state_for_processor
                                .borrow()
                                .contains(projection.selected_thumbnail_id.as_ref());
                        let thumbnail_source = thumbnail_is_actually_visible
                            .then(|| {
                                _this
                                    .card_thumbnails
                                    .source_for(projection.selected_thumbnail_id.as_ref())
                            })
                            .flatten();
                        let materialization_failed = _this
                            .card_thumbnails
                            .failed(projection.selected_thumbnail_id.as_ref());
                        let card_thumbnail_cache = _this.card_thumbnail_cache.clone();
                        let cache_failed = thumbnail_source.as_ref().is_some_and(|source| {
                            card_thumbnail_cache
                                .read(_cx)
                                .failed_resource(&gpui::Resource::from(source.clone()))
                        });
                        let thumbnail_failed = materialization_failed || cache_failed;
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
                                .child(note_card::render(
                                    &projection,
                                    selected,
                                    mode,
                                    thumbnail_source,
                                    thumbnail_failed,
                                    &card_thumbnail_cache,
                                )),
                        )
                    })
                    .collect::<Vec<_>>()
            }),
        )
        .size_full()
        .track_scroll(self.note_list_scroll.clone());
        let list = if matches!(mode, ListViewMode::Cards) && item_count > 0 {
            list.with_decoration(CardThumbnailViewportDecoration {
                shell,
                projections: items,
                state: Rc::clone(&self.card_thumbnail_viewport_state),
            })
        } else {
            list
        };
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
        let (title, read_only) = session.read_with(cx, |session, _| {
            (session.title().clone(), session.is_read_only())
        });
        let title_text_color = self.evernote_primary_text_fill();
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
                        color: title_text_color.into(),
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
                if !read_only && focus.is_focused(window) {
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
                .border_b_1()
                .border_color(self.evernote_primary_stroke())
                .text_color(self.evernote_primary_text_fill())
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
        let read_only = session.read_with(cx, |session, _| session.is_read_only());
        let key = event.keystroke.key.as_str();
        let modifiers = event.keystroke.modifiers;
        let secondary = modifiers.secondary();
        if TitleInput::moves_focus_to_body_for(key) {
            focus_editor(&editor, window, cx);
            cx.stop_propagation();
            return;
        }
        if read_only
            && (matches!(key, "backspace" | "delete") || (secondary && matches!(key, "x" | "v")))
        {
            // Do not let a title-level destructive shortcut bubble into an
            // unrelated parent handler. Selection movement and Cmd-C below
            // remain intentionally available for a retained Trash preview.
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

    fn toggle_organization_panel(
        &mut self,
        _event: &MouseDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.toggle_organization_panel_visibility(window, cx);
    }

    fn toggle_organization_panel_visibility(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.organization_panel_open = !self.organization_panel_open;
        if self.organization_panel_open {
            self.toolbar_more_open = false;
            self.organization_input
                .read(cx)
                .focus_handle()
                .focus(window);
        } else {
            self.pending_destructive_action = None;
            self.focus_active_editor_or_shell(window, cx);
        }
        cx.notify();
    }

    fn focus_active_editor_or_shell(&self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(session) = self.note_session.as_ref() {
            let editor = session.read_with(cx, |session, _| session.editor().clone());
            focus_editor(&editor, window, cx);
        } else {
            self.focus_handle.focus(window);
        }
    }

    fn fallback_focus_before_search_palette(&self, cx: &App) -> FocusHandle {
        if self.organization_panel_open {
            return self.organization_input.read(cx).focus_handle().clone();
        }
        if let Some(session) = self.note_session.as_ref() {
            let (title, editor) = session.read_with(cx, |session, _| {
                (session.title().clone(), session.editor().clone())
            });
            let _ = title;
            return editor.read(cx).focus_handle().clone();
        }
        self.focus_handle.clone()
    }

    fn toggle_search_palette_visibility(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.search_palette_open = !self.search_palette_open;
        if self.search_palette_open {
            self.search_palette_return_focus = window
                .focused(cx)
                .or_else(|| Some(self.fallback_focus_before_search_palette(cx)));
            self.search_palette_return_organization_panel_open = self.organization_panel_open;
            self.organization_panel_open = false;
            self.toolbar_more_open = false;
            self.search_input.read(cx).focus_handle().focus(window);
            self.schedule_search_from_input(cx);
        } else {
            self._search_task = None;
            self.search_palette_status = SearchPaletteStatus::Idle;
            self.search_palette_results.clear();
            self.search_palette_selected = 0;
            self.organization_panel_open = self.search_palette_return_organization_panel_open;
            self.search_palette_return_organization_panel_open = false;
            if let Some(focus) = self.search_palette_return_focus.take() {
                // A backdrop pointer event can claim focus after its handler
                // returns. Defer the sole restoration until the end of this
                // window turn: restoring a Link field synchronously would
                // let the same Escape event reach its underlying handler and
                // cancel the popover we are returning to.
                window.defer(cx, move |window, _app| focus.focus(window));
            } else {
                self.focus_active_editor_or_shell(window, cx);
            }
        }
        cx.notify();
    }

    fn schedule_search_from_input(&mut self, cx: &mut Context<Self>) {
        let query = self.search_input.read(cx).text().trim().to_owned();
        self._search_task = None;
        self.search_palette_generation = None;
        self.search_palette_results.clear();
        self.search_palette_selected = 0;
        if query.is_empty() {
            self.search_palette_status = SearchPaletteStatus::Idle;
            cx.notify();
            return;
        }
        let (repository, generation) = self.model.update(cx, |model, _| {
            (model.repository(), model.begin_search(query.clone()))
        });
        self.search_palette_generation = Some(generation);
        self.search_palette_status = SearchPaletteStatus::Pending;
        let query_for_worker = query.clone();
        let task = cx.spawn(async move |this, cx| {
            let result = cx
                .background_executor()
                .spawn(async move {
                    let mut parsed = SearchQuery::parse(&query_for_worker);
                    // 500 is an explicit local product cap, not an assertion
                    // about Evernote's distinct offset/window contract.
                    parsed
                        .set_page(0, SearchQuery::MAX_PAGE_SIZE)
                        .expect("bounded search page");
                    repository.search(parsed)
                })
                .await;
            let _ = this.update(cx, |shell, shell_cx| {
                // A field may have been edited, dismissed, or superseded while
                // SQLite was working. Only the exact retained generation gets
                // transient preview ownership.
                if !shell.search_palette_open
                    || shell.search_input.read(shell_cx).text().trim() != query
                    || shell.search_palette_generation != Some(generation)
                {
                    return;
                }
                match result {
                    Ok(hits) if hits.is_empty() => {
                        shell.search_palette_status = SearchPaletteStatus::Empty;
                    }
                    Ok(hits) => {
                        shell.search_palette_results = hits;
                        shell.search_palette_selected = 0;
                        shell.search_palette_status = SearchPaletteStatus::Ready;
                    }
                    Err(error) => {
                        shell.search_palette_status = SearchPaletteStatus::Error(error.to_string());
                    }
                }
                // `generation` is deliberately carried through to the commit
                // path. Previews never update the AppModel/card list.
                let _ = generation;
                shell_cx.notify();
            });
        });
        self._search_task = Some(task);
        cx.notify();
    }

    /// Back/Forward never advances into SearchRoute until this worker has a
    /// bounded local packet to install. This preserves the retained editor
    /// underneath the palette and prevents the generic list reducer from
    /// treating a search destination as All Notes.
    fn schedule_history_search(&mut self, forward: bool, cx: &mut Context<Self>) -> bool {
        let Some((repository, query, expected_current, expected_target)) =
            self.model.read_with(cx, |model, _| {
                model
                    .pending_history_search(forward)
                    .map(|(query, target)| {
                        (
                            model.repository(),
                            query,
                            model.navigation().snapshot(),
                            target,
                        )
                    })
            })
        else {
            return false;
        };
        self._history_search_task = None;
        let expected_session_fence = self.active_session_save_fence(cx);
        self.history_search_notice = Some("正在恢复本地搜索结果…".into());
        let query_for_worker = query.clone();
        #[cfg(test)]
        let completion_gate = self.search_completion_gate.take();
        let task = cx.spawn(async move |this, cx| {
            #[cfg(test)]
            let result = if let Some(gate) = completion_gate {
                let thread_repository = Arc::clone(&repository);
                let thread_query = query_for_worker.clone();
                let (sender, receiver) = futures::channel::oneshot::channel();
                std::thread::spawn(move || {
                    let result = (|| {
                        let mut parsed = SearchQuery::parse(&thread_query);
                        parsed
                            .set_page(0, SearchQuery::MAX_PAGE_SIZE)
                            .expect("bounded history search page");
                        if thread_repository.has_pending_search_jobs()? {
                            Ok(None)
                        } else {
                            thread_repository.search(parsed).map(Some)
                        }
                    })();
                    let _ = gate.read.send(());
                    let result = if futures::executor::block_on(gate.release).is_ok() {
                        result
                    } else {
                        Err(app_lite_core::LibraryError::InvalidSnapshot)
                    };
                    let _ = sender.send(result);
                });
                receiver
                    .await
                    .unwrap_or(Err(app_lite_core::LibraryError::InvalidSnapshot))
            } else {
                cx.background_executor()
                    .spawn(async move {
                        let mut parsed = SearchQuery::parse(&query_for_worker);
                        parsed
                            .set_page(0, SearchQuery::MAX_PAGE_SIZE)
                            .expect("bounded history search page");
                        if repository.has_pending_search_jobs()? {
                            Ok(None)
                        } else {
                            repository.search(parsed).map(Some)
                        }
                    })
                    .await
            };
            #[cfg(not(test))]
            let result = cx
                .background_executor()
                .spawn(async move {
                    let mut parsed = SearchQuery::parse(&query_for_worker);
                    parsed
                        .set_page(0, SearchQuery::MAX_PAGE_SIZE)
                        .expect("bounded history search page");
                    if repository.has_pending_search_jobs()? {
                        Ok(None)
                    } else {
                        repository.search(parsed).map(Some)
                    }
                })
                .await;
            let _ = this.update(cx, |shell, shell_cx| {
                if shell.active_session_save_fence(shell_cx) != expected_session_fence {
                    // An automatic save can finish while SQLite is searching,
                    // returning the session to Clean before this callback.
                    // The generation fence catches that otherwise invisible
                    // change and prevents committing a pre-save packet.
                    shell.retry_history_search(forward, shell_cx);
                    return;
                }
                let outcome = match result {
                    Ok(Some(hits)) => {
                        let active_would_change = shell.model.read_with(shell_cx, |model, _| {
                            let target_stays_selected = expected_target
                                .selected_note_id
                                .as_ref()
                                .is_some_and(|id| hits.iter().any(|hit| hit.note.id == *id));
                            model.active_session_note_id()
                                != expected_target.selected_note_id.as_ref()
                                || !target_stays_selected
                        });
                        if active_would_change {
                            let saved_after_packet =
                                shell.active_session_has_unsaved_changes(shell_cx);
                            if !shell.flush_active_session(FlushReason::NoteSwitch, shell_cx) {
                                shell.retry_history_search(forward, shell_cx);
                                return;
                            }
                            if saved_after_packet {
                                // The FTS packet was calculated before this
                                // lifecycle save. Do not advance history with
                                // a result that can omit the freshly saved
                                // note; query again after the durable queue
                                // has had a chance to drain.
                                shell.retry_history_search(forward, shell_cx);
                                return;
                            }
                        }
                        shell.model.update(shell_cx, |model, _| {
                            model.commit_history_search_results(
                                forward,
                                &query,
                                &expected_current,
                                &expected_target,
                                hits,
                            )
                        })
                    }
                    Ok(None) => {
                        shell.history_search_notice = Some("正在等待本地索引更新…".into());
                        shell.retry_history_search(forward, shell_cx);
                        return;
                    }
                    Err(error) => Err(error),
                };
                shell.history_search_notice = match outcome {
                    Ok(true) => None,
                    Ok(false) => {
                        // This packet lost its exact history fence. Only ask
                        // the model for the currently pending target; never
                        // revive the captured (possibly opposite) target.
                        if shell.model.read_with(shell_cx, |model, _| {
                            model.pending_history_search(forward).is_some()
                        }) {
                            shell.retry_history_search(forward, shell_cx);
                            return;
                        }
                        None
                    }
                    Err(error) => {
                        shell.search_refresh_retry_available = true;
                        shell.search_refresh_retry_history = Some(forward);
                        Some(format!("无法恢复搜索结果：{error}"))
                    }
                };
                shell_cx.notify();
            });
        });
        self._history_search_task = Some(task);
        cx.notify();
        true
    }

    fn retry_history_search(&mut self, forward: bool, cx: &mut Context<Self>) {
        let task = cx.spawn(async move |this, cx| {
            cx.background_executor()
                // Keep an IME composition or a transient local SQLite error
                // from turning into a 20Hz background-query loop. The route
                // and retained session remain visible while the next bounded
                // attempt is deferred.
                .timer(Duration::from_millis(500))
                .await;
            let _ = this.update(cx, |shell, shell_cx| {
                let _ = shell.schedule_history_search(forward, shell_cx);
            });
        });
        self._history_search_task = Some(task);
        cx.notify();
    }

    /// Repository events while SearchRoute is active must not synchronously
    /// fall back to an All Notes list. Recompute the existing typed packet in
    /// the background, fenced to this exact route snapshot.
    fn schedule_active_search_refresh(&mut self, cx: &mut Context<Self>) {
        let Some((repository, query, expected_snapshot, expected_generation)) =
            self.model.read_with(cx, |model, _| {
                model
                    .pending_search_refresh()
                    .map(|(query, snapshot, generation)| {
                        (model.repository(), query, snapshot, generation)
                    })
            })
        else {
            return;
        };
        self._search_route_refresh_task = None;
        let expected_session_fence = self.active_session_save_fence(cx);
        let query_for_worker = query.clone();
        #[cfg(test)]
        let completion_gate = self.search_completion_gate.take();
        let task = cx.spawn(async move |this, cx| {
            #[cfg(test)]
            let result = if let Some(gate) = completion_gate {
                let thread_repository = Arc::clone(&repository);
                let thread_query = query_for_worker.clone();
                let (sender, receiver) = futures::channel::oneshot::channel();
                std::thread::spawn(move || {
                    let result = (|| {
                        let mut parsed = SearchQuery::parse(&thread_query);
                        parsed
                            .set_page(0, SearchQuery::MAX_PAGE_SIZE)
                            .expect("bounded SearchRoute refresh page");
                        if thread_repository.has_pending_search_jobs()? {
                            Ok(None)
                        } else {
                            thread_repository.search(parsed).map(Some)
                        }
                    })();
                    let _ = gate.read.send(());
                    let result = if futures::executor::block_on(gate.release).is_ok() {
                        result
                    } else {
                        Err(app_lite_core::LibraryError::InvalidSnapshot)
                    };
                    let _ = sender.send(result);
                });
                receiver
                    .await
                    .unwrap_or(Err(app_lite_core::LibraryError::InvalidSnapshot))
            } else {
                cx.background_executor()
                    .spawn(async move {
                        let mut parsed = SearchQuery::parse(&query_for_worker);
                        parsed
                            .set_page(0, SearchQuery::MAX_PAGE_SIZE)
                            .expect("bounded SearchRoute refresh page");
                        if repository.has_pending_search_jobs()? {
                            Ok(None)
                        } else {
                            repository.search(parsed).map(Some)
                        }
                    })
                    .await
            };
            #[cfg(not(test))]
            let result = cx
                .background_executor()
                .spawn(async move {
                    let mut parsed = SearchQuery::parse(&query_for_worker);
                    parsed
                        .set_page(0, SearchQuery::MAX_PAGE_SIZE)
                        .expect("bounded SearchRoute refresh page");
                    if repository.has_pending_search_jobs()? {
                        Ok(None)
                    } else {
                        repository.search(parsed).map(Some)
                    }
                })
                .await;
            let _ = this.update(cx, |shell, shell_cx| {
                if shell.active_session_save_fence(shell_cx) != expected_session_fence {
                    // See history completion above: Clean is not proof that
                    // the editor did not save while this packet was in flight.
                    // Leave the core refresh pending for the new durable FTS
                    // queue/Idle edge rather than publishing stale cards.
                    return;
                }
                if let Ok(Some(hits)) = result {
                    let active_would_disappear = shell.model.read_with(shell_cx, |model, _| {
                        model
                            .active_session_note_id()
                            .is_some_and(|id| !hits.iter().any(|hit| hit.note.id == *id))
                    });
                    if active_would_disappear {
                        let saved_after_packet = shell.active_session_has_unsaved_changes(shell_cx);
                        if !shell.flush_active_session(FlushReason::NoteSwitch, shell_cx) {
                            // Composition/dirty-save ownership stays with the
                            // retained editor. Keep the pending core fence and
                            // retry after a short UI turn rather than unmounting
                            // a session that acquired new input while FTS ran.
                            shell.retry_active_search_refresh(shell_cx);
                            return;
                        }
                        if saved_after_packet {
                            // A successful flush may have queued a newer FTS
                            // row. Discard this pre-save packet; the durable
                            // queue/refresh-generation path will schedule the
                            // next truthful read.
                            return;
                        }
                    }
                    let committed = shell.model.update(shell_cx, |model, _| {
                        model.commit_search_refresh(
                            &query,
                            &expected_snapshot,
                            expected_generation,
                            hits,
                        )
                    });
                    if matches!(committed, Ok(true)) {
                        shell.history_search_notice = None;
                        shell.search_refresh_retry_available = false;
                        shell.search_refresh_retry_history = None;
                    } else if let Err(error) = committed {
                        shell.history_search_notice = Some(format!("本地搜索更新失败：{error}"));
                        shell.search_refresh_retry_available = true;
                        shell.search_refresh_retry_history = None;
                    } else if shell.model.read_with(shell_cx, |model, _| {
                        model.pending_search_refresh().is_some()
                    }) {
                        // A stale completion belongs to an older route/generation.
                        // Reissue only the latest retained SearchRoute request.
                        shell.schedule_active_search_refresh(shell_cx);
                    }
                } else if let Err(error) = result {
                    // A search refresh is not a save failure, but silently
                    // leaving an old packet visible is misleading. Keep the
                    // route intact and surface the error; a later repository
                    // event/index Idle edge is the bounded retry trigger.
                    shell.history_search_notice = Some(format!("本地搜索更新失败：{error}"));
                    shell.search_refresh_retry_available = true;
                    shell.search_refresh_retry_history = None;
                }
                shell_cx.notify();
            });
        });
        self._search_route_refresh_task = Some(task);
        cx.notify();
    }

    fn retry_active_search_refresh(&mut self, cx: &mut Context<Self>) {
        let task = cx.spawn(async move |this, cx| {
            cx.background_executor()
                .timer(Duration::from_millis(500))
                .await;
            let _ = this.update(cx, |shell, shell_cx| {
                shell.schedule_active_search_refresh(shell_cx);
            });
        });
        self._search_route_refresh_task = Some(task);
        cx.notify();
    }

    fn commit_search_result(&mut self, index: usize, window: &mut Window, cx: &mut Context<Self>) {
        let Some(hit) = self.search_palette_results.get(index).cloned() else {
            return;
        };
        let query = self.search_input.read(cx).text().trim().to_owned();
        if query.is_empty() {
            return;
        }
        // Establish the existing lifecycle boundary before selecting a result.
        // A dirty/marked-IME session must remain authoritative over a palette
        // click just as it does for a normal card selection.
        if !self.flush_active_session(FlushReason::NoteSwitch, cx) {
            return;
        }
        let Some(generation) = self.search_palette_generation else {
            return;
        };
        let packet = self.search_palette_results.clone();
        let committed = self.model.update(cx, |model, _| {
            model.commit_search_results(generation, query, packet, Some(hit.note.id.clone()))
        });
        match committed {
            Ok(true) => {
                self.search_palette_open = false;
                self.search_palette_results.clear();
                self.search_palette_selected = 0;
                self.search_palette_status = SearchPaletteStatus::Idle;
                self.search_palette_generation = None;
                self.focus_active_editor_or_shell(window, cx);
            }
            Ok(false) => {
                self.search_palette_status =
                    SearchPaletteStatus::Error("搜索结果已过期，请重新输入".into());
            }
            Err(error) => {
                self.search_palette_status = SearchPaletteStatus::Error(error.to_string())
            }
        }
        cx.notify();
    }

    fn toggle_toolbar_more(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.toolbar_more_open = !self.toolbar_more_open;
        if self.toolbar_more_open {
            self.organization_panel_open = false;
            self.pending_destructive_action = None;
            if let Some(chrome) = self.command_chrome.clone() {
                let _ = chrome.update(cx, |chrome, chrome_cx| {
                    chrome.dismiss_overlay(window, chrome_cx)
                });
            }
        }
        self.focus_active_editor_or_shell(window, cx);
        cx.notify();
    }

    fn dismiss_toolbar_more(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        if !self.toolbar_more_open {
            return false;
        }
        self.toolbar_more_open = false;
        self.focus_active_editor_or_shell(window, cx);
        cx.notify();
        true
    }

    fn apply_toolbar_more_action(
        &mut self,
        action: AppAction,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.toolbar_more_open = false;
        self.apply_action(action, window, cx);
    }

    fn request_destructive_confirmation(
        &mut self,
        pending: PendingDestructiveAction,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.pending_destructive_action = Some(pending);
        cx.notify();
    }

    fn cancel_destructive_confirmation(
        &mut self,
        _event: &MouseDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.pending_destructive_action = None;
        self.focus_handle.focus(window);
        cx.notify();
    }

    fn destructive_confirmation_is_current(
        &self,
        pending: &PendingDestructiveAction,
        cx: &App,
    ) -> bool {
        self.model.read_with(cx, |model, _| match pending {
            PendingDestructiveAction::DeleteStack(id) => model
                .navigation_index()
                .stacks
                .iter()
                .any(|candidate| candidate.id == *id),
            PendingDestructiveAction::DeleteNotebook(id) => model
                .navigation_index()
                .notebooks
                .iter()
                .any(|candidate| candidate.id == *id),
            PendingDestructiveAction::DeleteTag(id) => model
                .navigation_index()
                .tags
                .iter()
                .any(|candidate| candidate.id == *id),
            PendingDestructiveAction::PurgeNote(id) => {
                model.navigation().route() == &LibraryRoute::Trash
                    && model.navigation().selected_note_id() == Some(id)
                    && model
                        .projections()
                        .iter()
                        .any(|projection| projection.id == *id)
            }
        })
    }

    fn confirm_destructive_action(
        &mut self,
        _event: &MouseDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(pending) = self.pending_destructive_action.take() else {
            return;
        };
        if !self.destructive_confirmation_is_current(&pending, cx) {
            self.resource_notice = Some("待确认的目标已变化；请重新打开操作。".to_owned());
            self.focus_handle.focus(window);
            cx.notify();
            return;
        }
        self.focus_handle.focus(window);
        self.apply_action(pending.action(), window, cx);
    }

    fn render_destructive_confirmation(&self, cx: &mut Context<Self>) -> Option<gpui::AnyElement> {
        let pending = self.pending_destructive_action.as_ref()?;
        Some(
            div()
                .id("library-organization-destructive-confirmation")
                .debug_selector(|| "library-organization-destructive-confirmation".to_owned())
                .p(px(7.0))
                .rounded(px(5.0))
                .border_1()
                .border_color(rgba(0xe7c7c1ff))
                .bg(rgba(0xfff8f6ff))
                .text_color(rgba(0x7b3025ff))
                .flex()
                .flex_col()
                .gap(px(6.0))
                .child(pending.label())
                .child(
                    div()
                        .flex()
                        .gap(px(6.0))
                        .child(
                            div()
                                .id("library-organization-confirm-destructive")
                                .debug_selector(|| {
                                    "library-organization-confirm-destructive".to_owned()
                                })
                                .px(px(7.0))
                                .py(px(4.0))
                                .rounded(px(4.0))
                                .bg(rgba(0xa34838ff))
                                .text_size(px(11.0))
                                .text_color(rgba(0xffffffff))
                                .cursor_pointer()
                                .on_mouse_down(
                                    MouseButton::Left,
                                    cx.listener(Self::confirm_destructive_action),
                                )
                                .child("确认"),
                        )
                        .child(
                            div()
                                .id("library-organization-cancel-destructive")
                                .debug_selector(|| {
                                    "library-organization-cancel-destructive".to_owned()
                                })
                                .px(px(7.0))
                                .py(px(4.0))
                                .rounded(px(4.0))
                                .bg(rgba(0xf1f4f1ff))
                                .text_size(px(11.0))
                                .text_color(rgba(0x36413aff))
                                .cursor_pointer()
                                .on_mouse_down(
                                    MouseButton::Left,
                                    cx.listener(Self::cancel_destructive_confirmation),
                                )
                                .child("取消"),
                        ),
                )
                .into_any_element(),
        )
    }

    fn render_organization_input(&self, cx: &mut Context<Self>) -> gpui::AnyElement {
        let input = self.organization_input.clone();
        let paint_input = input.clone();
        let canvas_input = input.clone();
        let canvas = canvas(
            move |bounds, _window, cx| {
                let _ = canvas_input.update(cx, |input, _| input.record_bounds(bounds));
                canvas_input.clone()
            },
            move |bounds, entity, window, cx| {
                let (text, selection, focus) = entity.read_with(cx, |input, _| {
                    (
                        SharedString::from(input.text().to_owned()),
                        input.selection().clone(),
                        input.focus_handle().clone(),
                    )
                });
                let line = window.text_system().shape_line(
                    text,
                    px(14.0),
                    &[TextRun {
                        len: entity.read(cx).text().len(),
                        font: window.text_style().font(),
                        color: rgba(0x172033ff).into(),
                        background_color: None,
                        underline: None,
                        strikethrough: None,
                    }],
                    None,
                );
                entity.update(cx, |input, _| input.record_layout(bounds, line.clone()));
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
                        ElementInputHandler::new(bounds, paint_input.clone()),
                        cx,
                    );
                }
            },
        )
        .w_full()
        .h(px(26.0));
        div()
            .id("library-organization-input")
            .debug_selector(|| "library-organization-input".to_owned())
            .w(px(230.0))
            .h(px(28.0))
            .px(px(7.0))
            .rounded(px(5.0))
            .bg(rgba(0xffffffff))
            .border_1()
            .border_color(rgba(0xcbd5e1ff))
            .key_context("LibraryOrganizationInput")
            .track_focus(input.read(cx).focus_handle())
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(Self::on_organization_input_mouse_down),
            )
            .on_mouse_move(cx.listener(Self::on_organization_input_mouse_move))
            .on_mouse_up(
                MouseButton::Left,
                cx.listener(Self::on_organization_input_mouse_up),
            )
            .on_key_down(cx.listener(Self::on_organization_input_key_down))
            .child(canvas)
            .into_any_element()
    }

    fn on_organization_input_mouse_down(
        &mut self,
        event: &MouseDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if event.button != MouseButton::Left {
            cx.propagate();
            return;
        }
        self.organization_input.update(cx, |input, input_cx| {
            input.begin_pointer_selection(event.position, event.modifiers.shift);
            input.focus_handle().focus(window);
            input_cx.notify();
        });
        cx.stop_propagation();
    }

    fn on_organization_input_mouse_move(
        &mut self,
        event: &MouseMoveEvent,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if event.pressed_button != Some(MouseButton::Left) {
            cx.propagate();
            return;
        }
        self.organization_input.update(cx, |input, input_cx| {
            if input.extend_pointer_selection(event.position).is_some() {
                input_cx.notify();
            }
        });
        cx.stop_propagation();
    }

    fn on_organization_input_mouse_up(
        &mut self,
        _event: &MouseUpEvent,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.organization_input
            .update(cx, |input, _| input.end_pointer_selection());
        cx.stop_propagation();
    }

    fn on_organization_input_key_down(
        &mut self,
        event: &KeyDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let input = self.organization_input.clone();
        let key = event.keystroke.key.as_str();
        let modifiers = event.keystroke.modifiers;
        let secondary = modifiers.secondary();
        if key == "escape" {
            self.organization_panel_open = false;
            self.pending_destructive_action = None;
            self.focus_handle.focus(window);
            cx.notify();
            cx.stop_propagation();
            return;
        }
        if key == "enter" {
            let title = input.read(cx).text().to_owned();
            let stack_id = self
                .model
                .read_with(cx, |model, _| match model.navigation().route() {
                    LibraryRoute::Stack(id) => Some(id.clone()),
                    _ => None,
                });
            self.submit_organization_create(
                AppAction::CreateNotebook { title, stack_id },
                window,
                cx,
            );
            cx.stop_propagation();
            return;
        }
        let handled = match key {
            "backspace" => {
                input.update(cx, |input, input_cx| {
                    input.delete_backward();
                    input_cx.notify();
                });
                true
            }
            "delete" => {
                input.update(cx, |input, input_cx| {
                    input.delete_forward();
                    input_cx.notify();
                });
                true
            }
            "left" => {
                input.update(cx, |input, input_cx| {
                    input.move_horizontal(false, modifiers.shift);
                    input_cx.notify();
                });
                true
            }
            "right" => {
                input.update(cx, |input, input_cx| {
                    input.move_horizontal(true, modifiers.shift);
                    input_cx.notify();
                });
                true
            }
            "home" => {
                input.update(cx, |input, input_cx| {
                    input.move_to_edge(false, modifiers.shift);
                    input_cx.notify();
                });
                true
            }
            "end" => {
                input.update(cx, |input, input_cx| {
                    input.move_to_edge(true, modifiers.shift);
                    input_cx.notify();
                });
                true
            }
            "a" if secondary => {
                input.update(cx, |input, input_cx| {
                    input.select_all();
                    input_cx.notify();
                });
                true
            }
            "c" if secondary => {
                cx.write_to_clipboard(ClipboardItem::new_string(
                    input.read(cx).selected_text().to_owned(),
                ));
                true
            }
            "x" if secondary => {
                let selected = input.read(cx).selected_text().to_owned();
                if !selected.is_empty() {
                    cx.write_to_clipboard(ClipboardItem::new_string(selected));
                    input.update(cx, |input, input_cx| {
                        input.delete_forward();
                        input_cx.notify();
                    });
                }
                true
            }
            "v" if secondary => {
                input.update(cx, |input, input_cx| input.paste_from_clipboard(input_cx));
                true
            }
            _ => false,
        };
        if handled {
            cx.stop_propagation();
        }
    }

    /// Small C1 organization menu. All controls dispatch typed `AppAction`
    /// values through the same shell reducer as cards/menus/keys; this view
    /// owns neither SQLite nor a parallel selected-note state.
    fn render_organization_panel(&mut self, cx: &mut Context<Self>) -> Option<gpui::AnyElement> {
        if !self.organization_panel_open {
            return None;
        }
        let (route, index, selected_tag_ids, has_selected_note) =
            self.model.read_with(cx, |model, _| {
                (
                    model.navigation().route().clone(),
                    model.navigation_index().clone(),
                    model
                        .active_note()
                        .map(|note| note.tag_ids.clone())
                        .unwrap_or_default(),
                    model.navigation().selected_note_id().is_some(),
                )
            });
        let stack_for_new_notebook = match &route {
            LibraryRoute::Stack(id) => Some(id.clone()),
            _ => None,
        };
        let is_trash_route = route == LibraryRoute::Trash;
        let input = self.render_organization_input(cx);
        let create_notebook_stack = stack_for_new_notebook.clone();
        let create_notebook = div()
            .id("library-organization-create-notebook")
            .debug_selector(|| "library-organization-create-notebook".to_owned())
            .px(px(7.0))
            .py(px(4.0))
            .rounded(px(4.0))
            .bg(rgba(0x00a82dff))
            .text_size(px(11.0))
            .text_color(rgba(0xffffffff))
            .cursor_pointer()
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |shell, _event, window, cx| {
                    let title = shell.organization_input.read(cx).text().to_owned();
                    shell.submit_organization_create(
                        AppAction::CreateNotebook {
                            title,
                            stack_id: create_notebook_stack.clone(),
                        },
                        window,
                        cx,
                    );
                }),
            )
            .child("新建笔记本");
        let create_stack = div()
            .id("library-organization-create-stack")
            .debug_selector(|| "library-organization-create-stack".to_owned())
            .px(px(7.0))
            .py(px(4.0))
            .rounded(px(4.0))
            .bg(rgba(0xf1f4f1ff))
            .text_size(px(11.0))
            .text_color(rgba(0x36413aff))
            .cursor_pointer()
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|shell, _event, window, cx| {
                    let title = shell.organization_input.read(cx).text().to_owned();
                    shell.submit_organization_create(AppAction::CreateStack { title }, window, cx);
                }),
            )
            .child("新建组");
        let create_tag = div()
            .id("library-organization-create-tag")
            .debug_selector(|| "library-organization-create-tag".to_owned())
            .px(px(7.0))
            .py(px(4.0))
            .rounded(px(4.0))
            .bg(rgba(0xf1f4f1ff))
            .text_size(px(11.0))
            .text_color(rgba(0x36413aff))
            .cursor_pointer()
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|shell, _event, window, cx| {
                    let title = shell.organization_input.read(cx).text().to_owned();
                    shell.submit_organization_create(AppAction::CreateTag { title }, window, cx);
                }),
            )
            .child("新建标签");

        let rename_current: Option<gpui::AnyElement> = match route.clone() {
            LibraryRoute::Stack(id) => Some(
                div()
                    .id("library-organization-rename-current")
                    .debug_selector(|| "library-organization-rename-current".to_owned())
                    .px(px(7.0))
                    .py(px(4.0))
                    .rounded(px(4.0))
                    .bg(rgba(0xf1f4f1ff))
                    .text_size(px(11.0))
                    .cursor_pointer()
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |shell, _event, window, cx| {
                            let title = shell.organization_input.read(cx).text().to_owned();
                            shell.apply_action(
                                AppAction::RenameStack {
                                    id: id.clone(),
                                    title,
                                },
                                window,
                                cx,
                            );
                        }),
                    )
                    .child("重命名当前")
                    .into_any_element(),
            ),
            LibraryRoute::Notebook(id) => Some(
                div()
                    .id("library-organization-rename-current")
                    .debug_selector(|| "library-organization-rename-current".to_owned())
                    .px(px(7.0))
                    .py(px(4.0))
                    .rounded(px(4.0))
                    .bg(rgba(0xf1f4f1ff))
                    .text_size(px(11.0))
                    .cursor_pointer()
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |shell, _event, window, cx| {
                            let title = shell.organization_input.read(cx).text().to_owned();
                            shell.apply_action(
                                AppAction::RenameNotebook {
                                    id: id.clone(),
                                    title,
                                },
                                window,
                                cx,
                            );
                        }),
                    )
                    .child("重命名当前")
                    .into_any_element(),
            ),
            LibraryRoute::Tags(tag_ids) if tag_ids.len() == 1 => {
                let id = tag_ids.iter().next().expect("one tag").clone();
                Some(
                    div()
                        .id("library-organization-rename-current")
                        .debug_selector(|| "library-organization-rename-current".to_owned())
                        .px(px(7.0))
                        .py(px(4.0))
                        .rounded(px(4.0))
                        .bg(rgba(0xf1f4f1ff))
                        .text_size(px(11.0))
                        .cursor_pointer()
                        .on_mouse_down(
                            MouseButton::Left,
                            cx.listener(move |shell, _event, window, cx| {
                                let title = shell.organization_input.read(cx).text().to_owned();
                                shell.apply_action(
                                    AppAction::RenameTag {
                                        id: id.clone(),
                                        title,
                                    },
                                    window,
                                    cx,
                                );
                            }),
                        )
                        .child("重命名当前")
                        .into_any_element(),
                )
            }
            _ => None,
        };
        let delete_current: Option<gpui::AnyElement> = match route.clone() {
            LibraryRoute::Stack(id) => Some(
                library_destructive_action_button(
                    "library-organization-delete-current-stack",
                    "解散当前笔记本组".to_owned(),
                    PendingDestructiveAction::DeleteStack(id),
                    cx,
                )
                .into_any_element(),
            ),
            LibraryRoute::Notebook(id) => Some(
                library_destructive_action_button(
                    "library-organization-delete-current-notebook",
                    "删除当前笔记本".to_owned(),
                    PendingDestructiveAction::DeleteNotebook(id),
                    cx,
                )
                .into_any_element(),
            ),
            LibraryRoute::Tags(tag_ids) if tag_ids.len() == 1 => Some(
                library_destructive_action_button(
                    "library-organization-delete-current-tag",
                    "删除当前标签".to_owned(),
                    PendingDestructiveAction::DeleteTag(
                        tag_ids.into_iter().next().expect("one tag"),
                    ),
                    cx,
                )
                .into_any_element(),
            ),
            _ => None,
        };

        let mut move_targets = div()
            .id("library-organization-move-targets")
            .debug_selector(|| "library-organization-move-targets".to_owned())
            .flex()
            .flex_wrap()
            .gap(px(4.0));
        for notebook in &index.notebooks {
            let id = notebook.id.clone();
            let selector = format!("library-organization-move-note-{}", id.as_str());
            move_targets = move_targets.child(
                div()
                    .id(SharedString::from(selector.clone()))
                    .debug_selector(move || selector.clone())
                    .px(px(6.0))
                    .py(px(3.0))
                    .rounded(px(4.0))
                    .bg(rgba(0xf1f4f1ff))
                    .text_size(px(10.0))
                    .cursor_pointer()
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |shell, _event, window, cx| {
                            shell.apply_action(AppAction::MoveSelectedNote(id.clone()), window, cx)
                        }),
                    )
                    .child(format!("移至 {}", notebook.title)),
            );
        }

        let mut tag_targets = div()
            .id("library-organization-tag-targets")
            .debug_selector(|| "library-organization-tag-targets".to_owned())
            .flex()
            .flex_wrap()
            .gap(px(4.0));
        for tag in &index.tags {
            let id = tag.id.clone();
            let selected = selected_tag_ids.contains(&id);
            let selector = format!("library-organization-tag-{}", id.as_str());
            let action = if selected {
                AppAction::RemoveTagFromSelectedNote(id.clone())
            } else {
                AppAction::AddTagToSelectedNote(id.clone())
            };
            tag_targets = tag_targets.child(
                div()
                    .id(SharedString::from(selector.clone()))
                    .debug_selector(move || selector.clone())
                    .px(px(6.0))
                    .py(px(3.0))
                    .rounded(px(4.0))
                    .bg(if selected {
                        rgba(0x00a82d20)
                    } else {
                        rgba(0xf1f4f1ff)
                    })
                    .text_size(px(10.0))
                    .cursor_pointer()
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |shell, _event, window, cx| {
                            shell.apply_action(action.clone(), window, cx)
                        }),
                    )
                    .child(if selected {
                        format!("移除 #{}", tag.title)
                    } else {
                        format!("添加 #{}", tag.title)
                    }),
            );
        }

        let trash_controls: Option<gpui::AnyElement> =
            (is_trash_route && has_selected_note).then(|| {
                div()
                    .id("library-organization-trash-controls")
                    .debug_selector(|| "library-organization-trash-controls".to_owned())
                    .flex()
                    .gap(px(6.0))
                    .child(library_action_button(
                        "library-organization-restore-selected",
                        "恢复当前笔记".to_owned(),
                        AppAction::RestoreSelected,
                        cx,
                    ))
                    .child(library_destructive_action_button(
                        "library-organization-purge-selected",
                        "永久删除".to_owned(),
                        self.model.read_with(cx, |model, _| {
                            PendingDestructiveAction::PurgeNote(
                                model
                                    .navigation()
                                    .selected_note_id()
                                    .expect("Trash controls require a selected typed note")
                                    .clone(),
                            )
                        }),
                        cx,
                    ))
                    .into_any_element()
            });
        let destructive_confirmation = self.render_destructive_confirmation(cx);

        let mut panel = div()
            .id("library-organization-panel")
            .debug_selector(|| "library-organization-panel".to_owned())
            .mx(px(12.0))
            .mb(px(8.0))
            .p(px(8.0))
            .rounded(px(6.0))
            .border_1()
            .border_color(rgba(0xd9e1d9ff))
            .bg(rgba(0xfafcfaff))
            .text_color(rgba(0x36413aff))
            .text_size(px(11.0))
            .flex()
            .flex_col()
            .gap(px(7.0))
            .max_h(px(228.0))
            .overflow_y_scroll()
            .child(div().text_color(rgba(0x718075ff)).child("组织"))
            .child(
                div()
                    .flex()
                    .flex_wrap()
                    .items_center()
                    .gap(px(6.0))
                    .child(input)
                    .child(create_notebook)
                    .child(create_stack)
                    .child(create_tag),
            )
            .children(rename_current)
            .children(delete_current)
            .children(destructive_confirmation);
        if has_selected_note && !is_trash_route {
            panel = panel
                .child(div().text_color(rgba(0x718075ff)).child("移动当前笔记"))
                .child(move_targets)
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap(px(6.0))
                        .child(div().text_color(rgba(0x718075ff)).child("标签"))
                        .child(library_action_button(
                            "library-organization-clear-tags",
                            "清空标签".to_owned(),
                            AppAction::SetSelectedNoteTags(Vec::new()),
                            cx,
                        )),
                )
                .child(tag_targets);
        }
        Some(panel.children(trash_controls).into_any_element())
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
        if self.search_palette_open && event.keystroke.key == "escape" {
            self.toggle_search_palette_visibility(window, cx);
            cx.stop_propagation();
            return;
        }
        if event.keystroke.key != "escape" {
            return;
        }
        if self.dismiss_toolbar_more(window, cx) {
            cx.stop_propagation();
            return;
        }
        if self.organization_panel_open {
            self.organization_panel_open = false;
            self.pending_destructive_action = None;
            self.focus_active_editor_or_shell(window, cx);
            cx.notify();
            cx.stop_propagation();
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

    fn on_search_key_down(
        &mut self,
        event: &KeyDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let input = self.search_input.clone();
        let modifiers = event.keystroke.modifiers;
        let secondary = modifiers.secondary();
        let handled = match event.keystroke.key.as_str() {
            "escape" => {
                self.toggle_search_palette_visibility(window, cx);
                true
            }
            "enter" => {
                // macOS may deliver Enter while the native input bridge still
                // owns marked CJK composition. It finalizes that composition,
                // never opens a transient result underneath it.
                if input.read(cx).marked_range().is_none() {
                    self.commit_search_result(self.search_palette_selected, window, cx);
                }
                true
            }
            "down" => {
                if !self.search_palette_results.is_empty() {
                    self.search_palette_selected = (self.search_palette_selected + 1)
                        .min(self.search_palette_results.len() - 1);
                    self.reveal_search_palette_selection();
                    cx.notify();
                }
                true
            }
            "up" => {
                self.search_palette_selected = self.search_palette_selected.saturating_sub(1);
                self.reveal_search_palette_selection();
                cx.notify();
                true
            }
            "backspace" => {
                input.update(cx, |input, cx| {
                    input.delete_backward();
                    cx.notify();
                });
                true
            }
            "delete" => {
                input.update(cx, |input, cx| {
                    input.delete_forward();
                    cx.notify();
                });
                true
            }
            "left" => {
                input.update(cx, |input, cx| {
                    input.move_horizontal(false, modifiers.shift);
                    cx.notify();
                });
                true
            }
            "right" => {
                input.update(cx, |input, cx| {
                    input.move_horizontal(true, modifiers.shift);
                    cx.notify();
                });
                true
            }
            "home" => {
                input.update(cx, |input, cx| {
                    input.move_to_edge(false, modifiers.shift);
                    cx.notify();
                });
                true
            }
            "end" => {
                input.update(cx, |input, cx| {
                    input.move_to_edge(true, modifiers.shift);
                    cx.notify();
                });
                true
            }
            "a" if secondary => {
                input.update(cx, |input, cx| {
                    input.select_all();
                    cx.notify();
                });
                true
            }
            "v" if secondary => {
                input.update(cx, |input, input_cx| input.paste_from_clipboard(input_cx));
                true
            }
            _ => false,
        };
        if handled {
            cx.stop_propagation();
        }
    }

    fn reveal_search_palette_selection(&self) {
        // Search rows are direct tracked children, so GPUI can use their
        // painted bounds instead of inventing a fixed line height.
        self.search_palette_scroll
            .scroll_to_item(self.search_palette_selected);
    }

    fn render_search_palette(
        &self,
        viewport_width: f32,
        cx: &mut Context<Self>,
    ) -> Option<gpui::AnyElement> {
        if !self.search_palette_open {
            return None;
        }
        let input = self.search_input.clone();
        let canvas_input = input.clone();
        let paint_input = input.clone();
        let input_canvas = canvas(
            move |bounds, _window, cx| {
                let _ = canvas_input.update(cx, |input, _| input.record_bounds(bounds));
                canvas_input.clone()
            },
            move |bounds, entity, window, cx| {
                let (text, selection, focus) = entity.read_with(cx, |input, _| {
                    (
                        SharedString::from(input.text().to_owned()),
                        input.selection().clone(),
                        input.focus_handle().clone(),
                    )
                });
                let line = window.text_system().shape_line(
                    text,
                    px(17.0),
                    &[TextRun {
                        len: entity.read(cx).text().len(),
                        font: window.text_style().font(),
                        color: rgba(0x202420ff).into(),
                        background_color: None,
                        underline: None,
                        strikethrough: None,
                    }],
                    None,
                );
                entity.update(cx, |input, _| input.record_layout(bounds, line.clone()));
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
                    window.paint_quad(gpui::fill(
                        Bounds::new(
                            point(
                                bounds.left() + line.x_for_index(selection.start),
                                bounds.top(),
                            ),
                            size(px(1.0), bounds.size.height),
                        ),
                        rgba(EVERNOTE_GREEN),
                    ));
                }
                if focus.is_focused(window) {
                    window.handle_input(
                        &focus,
                        ElementInputHandler::new(bounds, paint_input.clone()),
                        cx,
                    );
                }
            },
        )
        .w_full()
        .h(px(34.0));
        let status = match &self.search_palette_status {
            SearchPaletteStatus::Idle => "输入关键词、短语或 tag:标签".to_owned(),
            SearchPaletteStatus::Pending => "正在搜索本地资料库…".to_owned(),
            SearchPaletteStatus::Empty => "没有找到匹配笔记".to_owned(),
            SearchPaletteStatus::Ready
                if self.search_palette_results.len() == SearchQuery::MAX_PAGE_SIZE =>
            {
                format!(
                    "结果达到 {} 条显示上限；请缩小关键词（Enter 打开当前选中项）",
                    SearchQuery::MAX_PAGE_SIZE
                )
            }
            SearchPaletteStatus::Ready => format!(
                "{} 条本地结果（上下键选择，Enter 打开）",
                self.search_palette_results.len()
            ),
            SearchPaletteStatus::Error(message) => format!("搜索失败：{message}"),
        };
        let mut results_scroll = div()
            .id("library-search-results-scroll")
            .flex()
            .flex_col()
            .h(px(420.0))
            .overflow_y_scroll()
            .track_scroll(&self.search_palette_scroll);
        // The packet is explicitly capped at 500. Mount the complete bounded
        // list so native wheel scrolling can reach every result as well as
        // keyboard navigation; no separate result authority is introduced.
        for (index, hit) in self.search_palette_results.iter().enumerate() {
            let title = hit.note.title_prefix.clone();
            let snippet = hit.snippet.clone();
            results_scroll = results_scroll.child(
                div()
                    .id(SharedString::from(format!("library-search-result-{index}")))
                    .cursor_pointer()
                    .p(px(8.0))
                    .rounded(px(5.0))
                    .bg(if index == self.search_palette_selected {
                        rgba(0x00a82d20)
                    } else {
                        rgba(0x00000000)
                    })
                    .hover(|style| style.bg(rgba(0x00a82d14)))
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |shell, _event, window, cx| {
                            shell.commit_search_result(index, window, cx)
                        }),
                    )
                    .child(div().text_size(px(14.0)).child(title))
                    .child(
                        div()
                            .text_size(px(11.0))
                            .text_color(rgba(0x718075ff))
                            .child(snippet),
                    ),
            );
        }
        let palette_width = (viewport_width - 16.0).max(1.0).min(620.0);
        let palette_left = (viewport_width - palette_width) / 2.0;
        Some(
            div()
                .id("library-search-backdrop")
                .debug_selector(|| "library-search-backdrop".to_owned())
                .absolute()
                .inset_0()
                .bg(rgba(0x10181033))
                .occlude()
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(|shell, _event, window, cx| {
                        cx.stop_propagation();
                        shell.toggle_search_palette_visibility(window, cx)
                    }),
                )
                .child(
                    div()
                        .id("library-search-palette")
                        .debug_selector(|| "library-search-palette".to_owned())
                        .absolute()
                        .top(px(72.0))
                        .left(px(palette_left))
                        .w(px(palette_width))
                        .max_h(px(600.0))
                        .p(px(14.0))
                        .rounded(px(10.0))
                        .bg(rgba(0xffffffff))
                        .border_1()
                        .border_color(rgba(0xd9e1d9ff))
                        .on_mouse_down(
                            MouseButton::Left,
                            cx.listener(|_shell, _event, _window, cx| cx.stop_propagation()),
                        )
                        .child(
                            div()
                                .id("library-search-input")
                                .border_b_1()
                                .border_color(rgba(0xd9e1d9ff))
                                .on_key_down(cx.listener(Self::on_search_key_down))
                                .track_focus(input.read(cx).focus_handle())
                                .child(input_canvas),
                        )
                        .child(
                            div()
                                .pt(px(8.0))
                                .text_size(px(12.0))
                                .text_color(rgba(0x718075ff))
                                .child(status),
                        )
                        .child(results_scroll),
                )
                .into_any_element(),
        )
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
                    .debug_selector(|| "library-empty-state".to_owned())
                    .flex_1()
                    .flex()
                    .flex_col()
                    .items_center()
                    .justify_center()
                    .gap(px(14.0))
                    .bg(self.evernote_primary_surface_fill(LibraryPrimarySurface::EmptyState))
                    .text_color(self.evernote_primary_text_fill())
                    .child(div().text_size(px(22.0)).child("从第一篇笔记开始"))
                    .child(
                        div()
                            .text_color(self.evernote_muted_text_fill())
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
                    .bg(self
                        .evernote_primary_surface_fill(LibraryPrimarySurface::UnsupportedDocument))
                    .text_color(self.evernote_primary_text_fill())
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
                    // The document surface is its own scroll owner. In a
                    // vertical flex column its automatic min-content height
                    // otherwise expands this pane to a tall image/PDF's full
                    // document height, leaving ScrollHandle::max_offset at
                    // zero and making the following attachment unreachable.
                    .min_h(px(0.0))
                    .h_full()
                    .relative()
                    .flex()
                    .flex_col()
                    .bg(self.evernote_primary_surface_fill(LibraryPrimarySurface::EditorPane))
                    .text_color(self.evernote_primary_text_fill())
                    .can_drop(|dragged, _window, _cx| dragged.is::<ExternalPaths>())
                    .on_drag_move::<ExternalPaths>(cx.listener(Self::on_external_paths_drag_move))
                    .on_drop::<ExternalPaths>(cx.listener(Self::on_external_paths_drop))
                    .children(title)
                    .child(toolbar)
                    .child(
                        div()
                            .flex_1()
                            .min_w(px(1.0))
                            .min_h(px(0.0))
                            .child(surface.clone()),
                    )
                    .into_any_element(),
                overlays,
            };
        }
        LibraryEditorRender::plain(
            div()
                .id("library-no-selection")
                .debug_selector(|| "library-no-selection".to_owned())
                .flex_1()
                .p(px(36.0))
                .bg(self.evernote_primary_surface_fill(LibraryPrimarySurface::NoSelection))
                .text_color(self.evernote_muted_text_fill())
                .child("选择一篇笔记以查看正文")
                .into_any_element(),
        )
    }
}

impl LibraryShell {
    fn render_library_toolbar(
        &mut self,
        available_editor_width: f32,
        editor_left: f32,
        content_mask: Bounds<Pixels>,
        route: &LibraryRoute,
        mode: ListViewMode,
        sort: NoteSort,
        cx: &mut Context<Self>,
    ) -> LibraryToolbarRender {
        let compact = library_toolbar_is_compact(available_editor_width);
        // A resize to an ample editor column must not revive a stale compact
        // overlay when the next narrow layout is entered.
        if !compact {
            self.toolbar_more_open = false;
        }
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
        let mut actions = Vec::new();
        if compact {
            actions.push(
                library_primary_action_button(
                    "library-create-note",
                    "新建".to_owned(),
                    AppAction::CreateNote,
                    cx,
                )
                .into_any_element(),
            );
            actions.push(self.render_search_toolbar_button(cx).into_any_element());
            if !matches!(route, LibraryRoute::Trash) {
                actions.push(library_resource_picker_button(cx).into_any_element());
            }
            actions.extend([
                library_action_button(
                    "library-toggle-sidebar",
                    "侧栏".to_owned(),
                    AppAction::ToggleSidebar,
                    cx,
                )
                .into_any_element(),
                library_action_button(
                    "library-toggle-list",
                    "笔记列表".to_owned(),
                    AppAction::ToggleNoteList,
                    cx,
                )
                .into_any_element(),
                library_action_button(
                    "library-sync-current",
                    "保存".to_owned(),
                    AppAction::ManualSync,
                    cx,
                )
                .into_any_element(),
                self.render_organization_toolbar_toggle(cx)
                    .into_any_element(),
                self.render_toolbar_more_trigger(cx),
            ]);
        } else {
            actions.push(self.render_search_toolbar_button(cx).into_any_element());
            if !matches!(route, LibraryRoute::Trash) {
                actions.push(library_resource_picker_button(cx).into_any_element());
            }
            actions.extend([
                library_primary_action_button(
                    "library-create-note",
                    "新建".to_owned(),
                    AppAction::CreateNote,
                    cx,
                )
                .into_any_element(),
                library_action_button(
                    "library-trash-selected",
                    "移至废纸篓".to_owned(),
                    AppAction::TrashSelected,
                    cx,
                )
                .into_any_element(),
                library_action_button(
                    "library-toggle-sidebar",
                    "侧栏".to_owned(),
                    AppAction::ToggleSidebar,
                    cx,
                )
                .into_any_element(),
                library_action_button(
                    "library-toggle-list",
                    "笔记列表".to_owned(),
                    AppAction::ToggleNoteList,
                    cx,
                )
                .into_any_element(),
                library_action_button(
                    "library-cycle-view",
                    format!("视图：{mode_label}"),
                    AppAction::SetListViewMode(mode.next()),
                    cx,
                )
                .into_any_element(),
                library_action_button(
                    "library-cycle-sort",
                    sort_label.to_owned(),
                    AppAction::SetSort(sort.next()),
                    cx,
                )
                .into_any_element(),
                library_action_button(
                    "library-sync-current",
                    "保存".to_owned(),
                    AppAction::ManualSync,
                    cx,
                )
                .into_any_element(),
                self.render_organization_toolbar_toggle(cx)
                    .into_any_element(),
            ]);
        }
        let toolbar = div()
            .id("library-actions")
            .debug_selector(|| "library-actions".to_owned())
            .w_full()
            .min_w(px(0.0))
            .h(px(42.0))
            .flex_none()
            .flex()
            .items_center()
            .gap(px(8.0))
            .px(px(12.0))
            .overflow_hidden()
            .bg(self.evernote_primary_surface_fill(LibraryPrimarySurface::Toolbar))
            .text_color(self.evernote_primary_text_fill())
            .border_b_1()
            .border_color(self.evernote_primary_stroke())
            .children(actions)
            .into_any_element();
        let overlays = self.render_toolbar_more_overlays(
            compact,
            available_editor_width,
            editor_left,
            content_mask,
            route,
            mode,
            sort,
            cx,
        );
        LibraryToolbarRender { toolbar, overlays }
    }

    fn render_organization_toolbar_toggle(
        &self,
        cx: &mut Context<Self>,
    ) -> gpui::Stateful<gpui::Div> {
        div()
            .id("library-toggle-organization")
            .debug_selector(|| "library-toggle-organization".to_owned())
            .flex_shrink_0()
            .px(px(8.0))
            .py(px(5.0))
            .rounded(px(5.0))
            .bg(if self.organization_panel_open {
                rgba(0x00a82d20)
            } else {
                rgba(0xf1f4f1ff)
            })
            .text_size(px(12.0))
            .text_color(rgba(0x36413aff))
            .cursor_pointer()
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(Self::toggle_organization_panel),
            )
            .child("组织")
    }

    fn render_toolbar_more_trigger(&self, cx: &mut Context<Self>) -> gpui::AnyElement {
        div()
            .id("library-toolbar-more")
            .debug_selector(|| "library-toolbar-more".to_owned())
            .flex_shrink_0()
            .px(px(8.0))
            .py(px(5.0))
            .rounded(px(5.0))
            .bg(if self.toolbar_more_open {
                rgba(0x00a82d20)
            } else {
                rgba(0xf1f4f1ff)
            })
            .text_size(px(12.0))
            .text_color(rgba(0x36413aff))
            .cursor_pointer()
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|shell, _event, window, cx| shell.toggle_toolbar_more(window, cx)),
            )
            .child(if self.toolbar_more_open {
                "更多 ▴"
            } else {
                "更多 ▾"
            })
            .into_any_element()
    }

    fn render_toolbar_more_overlays(
        &self,
        compact: bool,
        available_editor_width: f32,
        editor_left: f32,
        content_mask: Bounds<Pixels>,
        route: &LibraryRoute,
        mode: ListViewMode,
        sort: NoteSort,
        cx: &mut Context<Self>,
    ) -> Vec<gpui::AnyElement> {
        if !compact || !self.toolbar_more_open {
            return Vec::new();
        }
        let mask_left = f32::from(content_mask.left());
        let mask_right = f32::from(content_mask.right());
        let mask_top = f32::from(content_mask.top());
        let mask_bottom = f32::from(content_mask.bottom());
        let menu_width = 220.0_f32.min(available_editor_width.max(1.0));
        let left = (editor_left + available_editor_width - menu_width - 12.0)
            .max(mask_left)
            .min((mask_right - menu_width).max(mask_left));
        let max_height = (mask_bottom - (mask_top + 42.0)).max(1.0).min(280.0);
        let mut actions = Vec::new();
        if !matches!(route, LibraryRoute::Trash) {
            actions.push(
                library_toolbar_more_action_button(
                    "library-toolbar-more-trash",
                    "移至废纸篓".to_owned(),
                    AppAction::TrashSelected,
                    cx,
                )
                .into_any_element(),
            );
        }
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
        actions.extend([
            library_toolbar_more_action_button(
                "library-toolbar-more-view",
                format!("视图：{mode_label}"),
                AppAction::SetListViewMode(mode.next()),
                cx,
            )
            .into_any_element(),
            library_toolbar_more_action_button(
                "library-toolbar-more-sort",
                sort_label.to_owned(),
                AppAction::SetSort(sort.next()),
                cx,
            )
            .into_any_element(),
        ]);
        let shell = cx.entity();
        let backdrop = div()
            .id("library-toolbar-more-backdrop")
            .debug_selector(|| "library-toolbar-more-backdrop".to_owned())
            .absolute()
            .top(px(0.0))
            .left(px(0.0))
            .size_full()
            .occlude()
            .on_mouse_down(MouseButton::Left, move |_event, window, cx| {
                cx.stop_propagation();
                let _ = shell.update(cx, |shell, shell_cx| {
                    shell.dismiss_toolbar_more(window, shell_cx)
                });
            })
            .into_any_element();
        let menu = div()
            .id("library-toolbar-more-menu")
            .debug_selector(|| "library-toolbar-more-menu".to_owned())
            .absolute()
            .top(px(mask_top + 42.0))
            .left(px(left))
            .w(px(menu_width))
            .max_h(px(max_height))
            .overflow_y_scroll()
            .occlude()
            .p(px(8.0))
            .rounded(px(6.0))
            .bg(rgba(0xffffffff))
            .border_1()
            .border_color(rgba(0xc7d0ddff))
            .flex()
            .flex_col()
            .gap(px(4.0))
            .children(actions)
            .into_any_element();
        vec![backdrop, menu]
    }

    fn render_search_toolbar_button(&self, cx: &mut Context<Self>) -> gpui::Stateful<gpui::Div> {
        div()
            .id("library-open-search")
            .debug_selector(|| "library-open-search".to_owned())
            .flex_shrink_0()
            .px(px(8.0))
            .py(px(5.0))
            .rounded(px(5.0))
            .bg(rgba(0xf1f4f1ff))
            .text_size(px(12.0))
            .text_color(rgba(0x36413aff))
            .cursor_pointer()
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|shell, _event, window, cx| {
                    shell.toggle_search_palette_visibility(window, cx)
                }),
            )
            .child("搜索 ⌘K")
    }
}

/// Ask the native platform for one file, then return to the exact library
/// window which captured the insertion point. No picker task holds the shell
/// strongly, so closing a window while the panel is open simply discards its
/// eventual completion.
fn prompt_for_resource_path(prompt_request: ResourcePickerPrompt, cx: &mut App) {
    let ResourcePickerPrompt {
        window: window_handle,
        token,
    } = prompt_request;
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
                    if let Err(error) =
                        shell.complete_resource_picker_path_for_token(token, path, window, shell_cx)
                    {
                        shell.resource_notice = Some(format!("资源未插入：{error}"));
                        shell_cx.notify();
                    }
                } else {
                    shell.cancel_resource_picker(token, shell_cx);
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
        let prompt = window.update(cx, |shell, window, _| {
            shell.pending_resource_picker_prompt(window)
        });
        if let Ok(Ok(prompt)) = prompt {
            prompt_for_resource_path(prompt, cx);
        }
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
        {
            for fill in &self
                .library_surface_paint_hooks_for_test
                .primary_surface_fills
            {
                fill.store(0, Ordering::Relaxed);
            }
            self.library_surface_paint_hooks_for_test
                .primary_text
                .store(0, Ordering::Relaxed);
            self.library_surface_paint_hooks_for_test
                .muted_text
                .store(0, Ordering::Relaxed);
            self.library_surface_paint_hooks_for_test
                .primary_stroke
                .store(0, Ordering::Relaxed);
        }
        let (items, selected, panes, status, mode, sort, active_note, navigation_index, route) =
            self.model.read_with(cx, |model, _| {
                (
                    model.projections().to_vec(),
                    model.navigation().selected_note_id().cloned(),
                    model.panes(),
                    model.status().clone(),
                    model.list_view_mode(),
                    model.sort(),
                    model.active_note().cloned(),
                    model.navigation_index().clone(),
                    model.navigation().route().clone(),
                )
            });
        let status_message = match status {
            AppStatus::Ready => None,
            AppStatus::Error(error) => Some(format!("资料库错误：{error}")),
        };
        let editor_left_in_window = if panes.sidebar_visible {
            f32::from(panes.sidebar_width)
        } else {
            0.0
        } + if panes.list_visible {
            f32::from(panes.list_width)
        } else {
            0.0
        };
        // `Window::bounds` can include platform chrome and does not follow a
        // test/runtime content-mask resize until the next platform pass. The
        // toolbar must adapt to the actual flex column that will be painted,
        // not that outer window measurement.
        let content_mask = window.content_mask().bounds;
        let available_editor_width =
            (f32::from(content_mask.size.width) - editor_left_in_window).max(1.0);
        let editor_left = f32::from(content_mask.left()) + editor_left_in_window;
        let LibraryEditorRender {
            pane: editor_pane,
            overlays: editor_overlays,
        } = self.render_editor_panel(
            items.is_empty(),
            active_note,
            available_editor_width,
            window,
            cx,
        );
        let note_list = self.render_note_list(
            items.clone(),
            selected,
            panes.list_width,
            panes.list_visible,
            mode,
            window,
            cx,
        );
        let sidebar_shell = cx.weak_entity();
        let sidebar = sidebar::render(
            panes.sidebar_width,
            panes.sidebar_visible,
            &navigation_index,
            &route,
            self.sidebar_scroll.clone(),
            cx,
            move |route, window, app| {
                let _ = sidebar_shell.update(app, |shell, shell_cx| {
                    let selected_note_id = match shell.model.read_with(shell_cx, |model, _| {
                        model.sidebar_selected_note_for_route(&route)
                    }) {
                        Ok(selected_note_id) => selected_note_id,
                        Err(error) => {
                            shell.model.update(shell_cx, |model, model_cx| {
                                model.report_navigation_preflight_error(&error);
                                model_cx.notify();
                            });
                            return;
                        }
                    };
                    shell.apply_action(
                        AppAction::NavigateTo {
                            route,
                            selected_note_id,
                        },
                        window,
                        shell_cx,
                    );
                });
            },
        );
        let LibraryToolbarRender {
            toolbar,
            overlays: toolbar_overlays,
        } = self.render_library_toolbar(
            available_editor_width,
            editor_left,
            content_mask,
            &route,
            mode,
            sort,
            cx,
        );
        let organization_panel = self.render_organization_panel(cx);
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
            .on_action(cx.listener(Self::toggle_search_palette))
            .on_action(cx.listener(Self::toggle_library_toolbar_more))
            .on_action(cx.listener(Self::toggle_library_organization_panel))
            .on_action(cx.listener(Self::open_library_resource_picker))
            .on_action(cx.listener(Self::paste_resource_or_text))
            .on_key_down(cx.listener(Self::on_shell_key_down))
            .child(sidebar)
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
                    .text_color(self.evernote_primary_text_fill())
                    .child(toolbar)
                    .children(organization_panel)
                    .child(editor_pane),
            );
        for overlay in toolbar_overlays {
            root = root.child(overlay);
        }
        for overlay in editor_overlays {
            root = root.child(overlay);
        }
        let search_palette =
            self.render_search_palette(f32::from(window.viewport_size().width), cx);
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
        .children(match &self.indexing_status {
            // Index status has its own stable presentation slot: it never
            // reuses local-save semantics or overlaps resource notices.
            IndexingStatus::Pending => Some(
                div()
                    .id("library-indexing-status")
                    .debug_selector(|| "library-indexing-status".to_owned())
                    .absolute()
                    .bottom(px(58.0))
                    .right(px(14.0))
                    .max_w(px(520.0))
                    .text_size(px(11.0))
                    .text_color(rgba(0x536f59ff))
                    .child("正在建立本地搜索索引…"),
            ),
            IndexingStatus::Failed(message) => Some(
                div()
                    .id("library-indexing-status")
                    .debug_selector(|| "library-indexing-status".to_owned())
                    .absolute()
                    .bottom(px(58.0))
                    .right(px(14.0))
                    .max_w(px(520.0))
                    .text_size(px(11.0))
                    .text_color(rgba(0x8d6a27ff))
                    .child(message.clone()),
            ),
            IndexingStatus::Idle => None,
        })
        .children(self.resource_notice.as_ref().map(|notice| {
            div()
                .id("library-resource-notice")
                .debug_selector(|| "library-resource-notice".to_owned())
                .absolute()
                .bottom(px(82.0))
                .right(px(14.0))
                .max_w(px(520.0))
                .text_size(px(11.0))
                .text_color(rgba(0xa34838ff))
                .child(notice.clone())
        }))
        .children(self.history_search_notice.as_ref().map(|notice| {
            let retry_available = self.search_refresh_retry_available;
            let retry_history = self.search_refresh_retry_history;
            div()
                .id("library-history-search-status")
                .absolute()
                .bottom(px(106.0))
                .right(px(14.0))
                .max_w(px(520.0))
                .text_size(px(11.0))
                .text_color(rgba(0x536f59ff))
                .flex()
                .items_center()
                .gap(px(8.0))
                .child(notice.clone())
                .children(retry_available.then(|| {
                    div()
                        .id("library-search-refresh-retry")
                        .debug_selector(|| "library-search-refresh-retry".to_owned())
                        .cursor_pointer()
                        .px(px(6.0))
                        .py(px(3.0))
                        .rounded(px(4.0))
                        .bg(rgba(0x00a82d20))
                        .text_color(rgba(EVERNOTE_GREEN))
                        .on_mouse_down(
                            MouseButton::Left,
                            cx.listener(move |shell, _event, _window, cx| {
                                let scheduled = if let Some(forward) = retry_history {
                                    shell.schedule_history_search(forward, cx)
                                } else if shell.model.read_with(cx, |model, _| {
                                    model.pending_search_refresh().is_some()
                                }) {
                                    shell.schedule_active_search_refresh(cx);
                                    true
                                } else {
                                    false
                                };
                                shell.search_refresh_retry_available = false;
                                shell.search_refresh_retry_history = None;
                                shell.history_search_notice = if scheduled {
                                    Some("正在重试本地搜索更新…".into())
                                } else {
                                    Some("搜索上下文已变化，请重新打开搜索。".into())
                                };
                                cx.notify();
                            }),
                        )
                        .child("重试")
                }))
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
        .children(search_palette)
    }
}

/// A fallible bootstrap has no product model to render. Keep the error in a
/// minimal ordinary GPUI window rather than panicking inside a deferred window
/// factory or printing a message which can disappear when launched by Finder.
pub(crate) struct StartupErrorView {
    title: String,
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
            .child(div().text_size(px(23.0)).child(self.title.clone()))
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
    open_application_error_window(cx, "无法启动 Joplin Lite".to_owned(), message)
}

/// Fallback when a live window cannot even accept an in-place prompt. It is
/// deliberately separate from startup semantics so a blocked quit tells the
/// person what happened rather than pretending application launch failed.
pub(crate) fn open_quit_safety_error_window(
    cx: &mut App,
    message: String,
) -> Result<WindowHandle<StartupErrorView>, String> {
    open_application_error_window(cx, "无法安全退出 Joplin Lite".to_owned(), message)
}

fn open_application_error_window(
    cx: &mut App,
    title: String,
    message: String,
) -> Result<WindowHandle<StartupErrorView>, String> {
    let bounds = gpui::Bounds::centered(None, size(px(520.0), px(240.0)), cx);
    cx.open_window(
        WindowOptions {
            window_bounds: Some(WindowBounds::Windowed(bounds)),
            ..WindowOptions::default()
        },
        move |_window, cx| cx.new(|_| StartupErrorView { title, message }),
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
        .flex_shrink_0()
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

/// The compact and wide toolbars share this actual `CreateNote` route. The
/// green treatment makes the habitual writer action visually primary without
/// manufacturing a second create reducer.
fn library_primary_action_button(
    id: &'static str,
    label: String,
    action: AppAction,
    cx: &mut Context<LibraryShell>,
) -> gpui::Stateful<gpui::Div> {
    div()
        .id(id)
        .debug_selector(move || id.to_owned())
        .flex_shrink_0()
        .px(px(8.0))
        .py(px(5.0))
        .rounded(px(5.0))
        .bg(rgba(EVERNOTE_GREEN))
        .text_size(px(12.0))
        .text_color(rgba(0xffffffff))
        .cursor_pointer()
        .on_mouse_down(
            MouseButton::Left,
            cx.listener(move |shell, _event, window, cx| {
                shell.apply_action(action.clone(), window, cx)
            }),
        )
        .child(label)
}

/// The overflow owns only whether it is open. Choosing a row immediately
/// closes that transient layer then sends the same typed model action as the
/// wide direct button.
fn library_toolbar_more_action_button(
    id: &'static str,
    label: String,
    action: AppAction,
    cx: &mut Context<LibraryShell>,
) -> gpui::Stateful<gpui::Div> {
    div()
        .id(id)
        .debug_selector(move || id.to_owned())
        .w_full()
        .px(px(8.0))
        .py(px(6.0))
        .rounded(px(4.0))
        .bg(rgba(0xf7f9f7ff))
        .text_size(px(12.0))
        .text_color(rgba(0x36413aff))
        .cursor_pointer()
        .on_mouse_down(
            MouseButton::Left,
            cx.listener(move |shell, _event, window, cx| {
                cx.stop_propagation();
                shell.apply_toolbar_more_action(action.clone(), window, cx)
            }),
        )
        .child(label)
}

fn library_destructive_action_button(
    id: &'static str,
    label: String,
    pending: PendingDestructiveAction,
    cx: &mut Context<LibraryShell>,
) -> gpui::Stateful<gpui::Div> {
    div()
        .id(id)
        .debug_selector(move || id.to_owned())
        .flex_shrink_0()
        .px(px(8.0))
        .py(px(5.0))
        .rounded(px(5.0))
        .bg(rgba(0xfff1efff))
        .text_size(px(12.0))
        .text_color(rgba(0x8d3b30ff))
        .cursor_pointer()
        .on_mouse_down(
            MouseButton::Left,
            cx.listener(move |shell, _event, window, cx| {
                shell.request_destructive_confirmation(pending.clone(), window, cx)
            }),
        )
        .child(label)
}

fn library_resource_picker_button(cx: &mut Context<LibraryShell>) -> gpui::Stateful<gpui::Div> {
    let shell = cx.weak_entity();
    div()
        .id("library-insert-resource")
        .debug_selector(|| "library-insert-resource".to_owned())
        .flex_shrink_0()
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
                Ok(Ok(prompt_request)) => prompt_for_resource_path(prompt_request, app),
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
mod create_note_route_tests;
#[cfg(test)]
mod editor_scroll_tests;
#[cfg(test)]
mod index_scheduler_tests;
#[cfg(test)]
mod navigation_retention_tests;
#[cfg(test)]
mod note_card_tests;
#[cfg(test)]
mod organization_input_tests;
#[cfg(test)]
mod quit_lifecycle_tests;
#[cfg(test)]
mod tests;
#[cfg(test)]
mod toolbar_adaptation_tests;
