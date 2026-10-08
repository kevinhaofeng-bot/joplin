//! Reusable GPUI shell around the document-wide native editor canvas.
//!
//! The spike and library routes deliberately share the shaping/painting helper
//! below.  The library owns this `EditorSurface` entity, while the spike keeps
//! its measurement callbacks around the same canvas helper.

use super::commands::{CommandArgument, CommandCatalogue, EditorCommand};
use super::core::{AtomicBlockHit, EditorCore};
use super::find::FindMatch;
use super::images::BudgetedImageCache;
use super::model::{BlockKind, DocPoint, Selection};
use super::render;
use crate::components::{
    BlockDown, BlockUp, BoldSelection, Copy, Cut, Delete, DeleteBack, End, FocusNext, FocusPrev,
    Home, ItalicSelection, JumpToBottom, JumpToTop, MoveLeft, MoveRight, Newline, PageDown, PageUp,
    Redo, SelectAll, SelectEnd, SelectHome, SelectLeft, SelectRight, SubscriptSelection,
    SuperscriptSelection, UnderlineSelection, Undo, WordSelectLeft, WordSelectRight,
};
use gpui::{
    App, ClipboardItem, Context, Entity, EventEmitter, InteractiveElement, IntoElement,
    KeyDownEvent, MouseButton, MouseDownEvent, MouseMoveEvent, MouseUpEvent, ParentElement, Pixels,
    Render, ScrollHandle, StatefulInteractiveElement, Styled, Subscription, Window, canvas, div,
    px, rgba,
};
use std::sync::Arc;

/// The ordinary library route is a white Evernote primary surface. The canvas
/// owns its text shaping separately, but its host rectangle must stay opaque
/// and carry the primary surface stroke rather than exposing the macOS window
/// backing between title/chrome/body layers.
const LIGHT_EDITOR_SURFACE_BACKGROUND: u32 = 0xffffffff;
const LIGHT_EDITOR_SURFACE_BORDER: u32 = 0xf3f2f1ff;
const LIGHT_EDITOR_SURFACE_FOREGROUND: u32 = 0x141414ff;

#[cfg(test)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct EditorSurfaceLightContract {
    background: u32,
    border: u32,
    foreground: u32,
}

/// Test-only geometry from the retained scroll owner.  Keeping this at the
/// surface boundary lets mounted LibraryShell tests distinguish an unbounded
/// flex child from a document/layout problem without exposing a second
/// production scrolling API.
#[cfg(test)]
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct EditorSurfaceScrollMetrics {
    pub(crate) viewport: gpui::Bounds<Pixels>,
    pub(crate) max_offset: gpui::Size<Pixels>,
    pub(crate) offset: gpui::Point<Pixels>,
}

#[cfg(test)]
impl EditorSurfaceLightContract {
    pub(crate) fn is_opaque_and_contrasted(self) -> bool {
        self.background == LIGHT_EDITOR_SURFACE_BACKGROUND
            && self.border == LIGHT_EDITOR_SURFACE_BORDER
    }

    pub(crate) fn has_primary_foreground(self) -> bool {
        self.foreground == LIGHT_EDITOR_SURFACE_FOREGROUND
    }
}

macro_rules! bind_selection_action {
    ($surface:ident, $editor:expr, $action:ty, $method:ident) => {{
        let action_editor = $editor.clone();
        $surface = $surface.on_action(move |_action: &$action, window, cx| {
            let _ = action_editor.update(cx, |editor, editor_cx| {
                editor.$method();
                editor_cx.notify();
            });
            focus_editor(&action_editor, window, cx);
        });
    }};
}

// Structural editing actions are intentionally sent to `EditorCore` rather
// than reimplemented in the GPUI surface. `EditorCore::ensure_editable` is
// the single read-only gate, so the spike and library keep one real action
// route while a Task 3 read-only surface remains side-effect free.
macro_rules! bind_result_action {
    ($surface:ident, $editor:expr, $action:ty, $method:ident) => {{
        let action_editor = $editor.clone();
        $surface = $surface.on_action(move |_action: &$action, window, cx| {
            let _ = action_editor.update(cx, |editor, editor_cx| {
                let result = editor.$method();
                editor_cx.notify();
                result
            });
            focus_editor(&action_editor, window, cx);
        });
    }};
}

macro_rules! bind_format_action {
    ($surface:ident, $editor:expr, $action:ty, $command:expr) => {{
        let action_editor = $editor.clone();
        $surface = $surface.on_action(move |_action: &$action, window, cx| {
            let _ = action_editor.update(cx, |editor, editor_cx| {
                let result =
                    CommandCatalogue::new().execute($command, CommandArgument::None, editor);
                editor_cx.notify();
                result
            });
            focus_editor(&action_editor, window, cx);
        });
    }};
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EditorSurfaceMode {
    Editable,
    /// A durable Trash preview. Its explanatory copy may truthfully tell the
    /// person that restoring the note is the path back to editing.
    ReadOnly,
    /// A normal note whose durable organization mutation committed, but whose
    /// full route/session candidate has not been installed yet. It is just as
    /// non-mutating as Trash, but must not pretend the note was deleted.
    RecoveryLocked,
}

impl EditorSurfaceMode {
    pub const fn is_read_only(self) -> bool {
        !matches!(self, Self::Editable)
    }
}

type SurfacePaintCallback = Arc<dyn Fn(&mut Window, &mut App)>;

/// Route-specific work that must surround the shared canvas paint. The spike
/// uses this to retain its measurement accounting without cloning the canvas
/// implementation; the library leaves both callbacks as no-ops.
#[derive(Clone)]
pub struct EditorSurfaceHooks {
    before_shape: SurfacePaintCallback,
    after_paint: SurfacePaintCallback,
}

impl EditorSurfaceHooks {
    pub fn new(
        before_shape: impl Fn(&mut Window, &mut App) + 'static,
        after_paint: impl Fn(&mut Window, &mut App) + 'static,
    ) -> Self {
        Self {
            before_shape: Arc::new(before_shape),
            after_paint: Arc::new(after_paint),
        }
    }
}

impl Default for EditorSurfaceHooks {
    fn default() -> Self {
        Self::new(|_, _| {}, |_, _| {})
    }
}

/// A resource action requested by the one shared canvas.  The surface owns
/// hit testing and structural selection; the library shell owns repository
/// access and platform effects, keeping profile paths and opener policy out
/// of the native editor.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum EditorSurfaceEvent {
    OpenLink {
        url: String,
    },
    OpenAttachment {
        resource_id: String,
    },
    DismissFindInNote,
    /// Copy or cut with full structure and resources, which only the owner
    /// (with the library) can put on the clipboard.
    Clipboard {
        cut: bool,
    },
    /// Double-click on a table cell: the shell opens its cell editor.
    EditTableCell {
        node_id: super::model::NodeId,
        row: usize,
        column: usize,
    },
}

/// One mounted document canvas. Its editor is the only input-handler owner;
/// ordinary text elements never mirror or flatten document data.
pub struct EditorSurface {
    editor: Entity<EditorCore>,
    mode: EditorSurfaceMode,
    scroll_handle: ScrollHandle,
    pointer_anchor: Option<DocPoint>,
    image_cache: Option<Entity<BudgetedImageCache>>,
    hooks: EditorSurfaceHooks,
    find_panel_open: bool,
    // The library owns a nested document scroll area. The spike already owns
    // a larger scroll column (title, toolbar, body), so it mounts the same
    // surface as an embedded canvas and keeps that established scroll owner.
    embedded_frame: Option<(f32, f32)>,
    /// Copy and cut go to the subscriber as `EditorSurfaceEvent::Clipboard`
    /// instead of placing plain text here.
    clipboard_to_owner: bool,
    accepts_pointer_input: bool,
    // A distant result first seeks through the height index. The next canvas
    // passes shape that viewport and re-centre on the exact match until its
    // geometry stops moving (wrapped text, image sizes published later).
    pending_find_reveal: Option<FindRevealRequest>,
    /// Frames left in which to bring the caret or restored selected atom
    /// into view, as blocks newly in view are measured.
    pending_caret_reveal: u8,
    /// Layout/hydration notifications must not pull a manually scrolled
    /// viewport back to the old insertion point. Only a changed caret asks
    /// to reveal the next edit position.
    observed_selection: Selection,
    observed_document_revision: u64,
    _editor_subscription: Subscription,
    #[cfg(test)]
    light_surface_paint_for_test: EditorSurfaceLightContract,
}

/// One Find reveal (a query or a Next/Previous): it lasts while that primary
/// and that document revision do, and ends once two passes in a row find the
/// match centred with no image size still to arrive. A user scroll ends it
/// early.
#[derive(Clone)]
struct FindRevealRequest {
    found: FindMatch,
    revision: u64,
    passes: u8,
    // These passes run before the frame's paint, which is where images newly
    // in view ask for their size; so one settled pass is confirmed by the
    // next, after a paint has seen the revealed viewport.
    settled_once: bool,
}

/// Bounds the passes of one request, so a geometry that never settles
/// cannot keep taking the scroll position.
const MAX_FIND_REVEAL_PASSES: u8 = 24;

impl EventEmitter<EditorSurfaceEvent> for EditorSurface {}

impl EditorSurface {
    pub fn new(
        editor: Entity<EditorCore>,
        mode: EditorSurfaceMode,
        image_cache: Option<Entity<BudgetedImageCache>>,
        cx: &mut Context<Self>,
    ) -> Self {
        Self::new_with_layout(editor, mode, image_cache, None, true, cx)
    }

    /// Build the very same retained surface for the measurement spike. The
    /// parent spike keeps its existing drag/drop/popup policy, while this
    /// entity remains the one canvas, paint, focus and input-owner boundary.
    pub fn new_embedded(
        editor: Entity<EditorCore>,
        mode: EditorSurfaceMode,
        image_cache: Option<Entity<BudgetedImageCache>>,
        cx: &mut Context<Self>,
    ) -> Self {
        Self::new_with_layout(editor, mode, image_cache, Some((1.0, 480.0)), false, cx)
    }

    fn new_with_layout(
        editor: Entity<EditorCore>,
        mode: EditorSurfaceMode,
        image_cache: Option<Entity<BudgetedImageCache>>,
        embedded_frame: Option<(f32, f32)>,
        accepts_pointer_input: bool,
        cx: &mut Context<Self>,
    ) -> Self {
        let observed_selection = editor.read(cx).selection();
        let observed_document_revision = editor.read(cx).document().revision();
        let subscription = cx.observe(&editor, |surface, editor, cx| {
            let editor = editor.read(cx);
            let selection = editor.selection();
            let revision = editor.document().revision();
            let restored_atom =
                revision != surface.observed_document_revision && selection_is_atom(editor);
            if selection != surface.observed_selection
                && surface.embedded_frame.is_none()
                && (restored_atom
                    || (selection.is_caret()
                        && editor
                            .document()
                            .block(selection.head.node_id)
                            .is_some_and(|block| block.content.as_text().is_some())))
            {
                // Evernote paragraph/keymap.ts insertParagraph explicitly
                // requests scrollIntoView. Observe the shared editor here
                // so Return, paste and native IME input use the same route.
                // Its bundled prosemirror-history also requests it after
                // restoring a NodeSelection. Ordinary pointer selections and
                // hydration notifications must not steal the user's scroll.
                surface.pending_caret_reveal = 6;
            }
            surface.observed_selection = selection;
            surface.observed_document_revision = revision;
            cx.notify();
        });
        Self {
            editor,
            mode,
            scroll_handle: ScrollHandle::new(),
            pointer_anchor: None,
            image_cache,
            hooks: EditorSurfaceHooks::default(),
            find_panel_open: false,
            embedded_frame,
            clipboard_to_owner: false,
            accepts_pointer_input,
            pending_find_reveal: None,
            pending_caret_reveal: 0,
            observed_selection,
            observed_document_revision,
            _editor_subscription: subscription,
            #[cfg(test)]
            light_surface_paint_for_test: EditorSurfaceLightContract {
                background: 0,
                border: 0,
                foreground: 0,
            },
        }
    }

    pub fn editor(&self) -> &Entity<EditorCore> {
        &self.editor
    }

    /// The window area the note scrolls in.
    #[cfg(test)]
    pub(crate) fn scroll_viewport_for_test(&self) -> gpui::Bounds<Pixels> {
        self.scroll_handle.bounds()
    }

    pub fn mode(&self) -> EditorSurfaceMode {
        self.mode
    }

    /// Title forward-Tab enters the first body position and reveals it using
    /// the same retained viewport path as document-start keyboard navigation.
    pub fn focus_document_start(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.pending_find_reveal = None;
        jump_editor_to_document_edge(&self.editor, &self.scroll_handle, false, window, cx);
    }

    /// The shell retains find visibility, while the focused canvas owns the
    /// Escape event that must not fall through to its command chrome.
    pub fn set_find_panel_open(&mut self, open: bool) {
        self.find_panel_open = open;
        if !open {
            self.pending_find_reveal = None;
        }
    }

    /// Reveal the current find result without changing the editor selection.
    /// Visible text uses exact match geometry. An offscreen result first uses
    /// the retained height index, then the next canvas pass corrects to the
    /// freshly shaped text range (important for wrapped text and images above
    /// the result).
    pub fn reveal_find_primary(&mut self, cx: &mut Context<Self>) -> bool {
        let found = self.editor.read(cx).find_primary().cloned();
        // A match in a wide table: its column first comes into the table's
        // own horizontal view.
        if let Some((node_id, cell)) = found
            .as_ref()
            .and_then(|found| found.cell.map(|cell| (found.node_id, cell)))
        {
            self.editor.update(cx, |editor, editor_cx| {
                editor.reveal_find_table_column(node_id, cell.column);
                editor_cx.notify();
            });
        }
        let Some(found) = found else {
            self.pending_find_reveal = None;
            return false;
        };
        let editor = self.editor.read(cx);
        let target = editor.layout().find_match_bounds(&found).or_else(|| {
            editor
                .layout()
                .bounds_for_node(editor.document(), found.node_id)
        });
        let Some(target) = target else {
            self.pending_find_reveal = None;
            return false;
        };
        let revision = editor.document().revision();
        center_scroll_bounds(&self.scroll_handle, target);
        // The first rect is not yet final: the next passes check it again
        // with freshly measured layout.
        self.pending_find_reveal = Some(FindRevealRequest {
            found,
            revision,
            passes: 0,
            settled_once: false,
        });
        cx.notify();
        true
    }

    #[cfg(test)]
    pub(crate) fn find_reveal_pending_for_test(&self) -> bool {
        self.pending_find_reveal.is_some()
    }

    #[cfg(test)]
    pub(crate) fn scroll_metrics_for_test(&self) -> EditorSurfaceScrollMetrics {
        EditorSurfaceScrollMetrics {
            viewport: self.scroll_handle.bounds(),
            max_offset: self.scroll_handle.max_offset(),
            offset: self.scroll_handle.offset(),
        }
    }

    /// The retained shell may temporarily freeze an otherwise editable
    /// surface while reconciling a committed metadata mutation. This changes
    /// only event routing; the shared EditorCore remains the authority and
    /// independently rejects mutations through its recovery lock.
    pub fn set_mode(&mut self, mode: EditorSurfaceMode) {
        self.mode = mode;
    }

    /// Update the spike's measured body frame without taking over its outer
    /// scroll container. This is intentionally a presentation detail; both
    /// modes keep the same EditorCore and `paint_entity` path.
    pub(crate) fn route_clipboard_to_owner(&mut self) {
        self.clipboard_to_owner = true;
    }

    pub fn set_embedded_frame(&mut self, width: f32, height: f32) {
        self.embedded_frame = Some((width.max(1.0), height.max(1.0)));
    }

    pub fn set_paint_hooks(&mut self, hooks: EditorSurfaceHooks) {
        self.hooks = hooks;
    }

    fn light_surface_background(&mut self) -> gpui::Rgba {
        #[cfg(test)]
        {
            self.light_surface_paint_for_test.background = LIGHT_EDITOR_SURFACE_BACKGROUND;
        }
        rgba(LIGHT_EDITOR_SURFACE_BACKGROUND)
    }

    fn light_surface_border(&mut self) -> gpui::Rgba {
        #[cfg(test)]
        {
            self.light_surface_paint_for_test.border = LIGHT_EDITOR_SURFACE_BORDER;
        }
        rgba(LIGHT_EDITOR_SURFACE_BORDER)
    }

    fn light_surface_foreground(&mut self) -> gpui::Rgba {
        #[cfg(test)]
        {
            self.light_surface_paint_for_test.foreground = LIGHT_EDITOR_SURFACE_FOREGROUND;
        }
        rgba(LIGHT_EDITOR_SURFACE_FOREGROUND)
    }

    #[cfg(test)]
    pub(crate) fn light_surface_contract_for_test(&self) -> EditorSurfaceLightContract {
        self.light_surface_paint_for_test
    }

    fn trace_pointer_route(
        &self,
        route: &str,
        event: &MouseDownEvent,
        window: &Window,
        cx: &Context<Self>,
    ) {
        super::input_trace::record("body", "pointer_route", || {
            let editor = self.editor.read(cx);
            serde_json::json!({
                "route": route,
                "x": f32::from(event.position.x),
                "y": f32::from(event.position.y),
                "click_count": event.click_count,
                "shift": event.modifiers.shift,
                "command": event.modifiers.platform,
                "undo_depth": editor.undo_depth(),
                "redo_depth": editor.redo_depth(),
                "revision": editor.document().revision(),
                "selection": format!("{:?}", editor.selection()),
                "focused": format!("{:?}", window.focused(cx)),
            })
        });
    }

    fn on_mouse_down(
        &mut self,
        event: &MouseDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if event.button != MouseButton::Left {
            cx.propagate();
            return;
        }
        self.trace_pointer_route("entered", event, window, cx);
        if !self.mode.is_read_only() {
            let on_handle = self.editor.update(cx, |editor, editor_cx| {
                let on_handle = editor
                    .image_resize_handle_bounds()
                    .is_some_and(|handle| handle.contains(&event.position));
                if on_handle {
                    if event.click_count >= 2 {
                        let _ = editor.restore_selected_image_natural_width();
                    } else {
                        editor.begin_image_resize(event.position);
                    }
                    editor_cx.notify();
                }
                on_handle
            });
            if on_handle {
                self.trace_pointer_route("resize_handle", event, window, cx);
                self.pointer_anchor = None;
                focus_editor(&self.editor, window, cx);
                cx.stop_propagation();
                return;
            }
        }
        // Evernote list/plugin.ts: a click on a checklist box ticks it and
        // does not move the caret.
        if !self.mode.is_read_only() && !event.modifiers.shift {
            let ticked = self.editor.update(cx, |editor, editor_cx| {
                let node = editor.check_marker_at(event.position)?;
                let ticked = editor.toggle_check(node).is_ok();
                editor_cx.notify();
                Some(ticked)
            });
            if ticked.is_some() {
                self.trace_pointer_route("check_marker", event, window, cx);
                self.pointer_anchor = None;
                focus_editor(&self.editor, window, cx);
                cx.stop_propagation();
                return;
            }
        }
        // Evernote table/plugin.ts lets Cmd/Ctrl-click links bypass cell selection.
        if event.modifiers.platform || event.modifiers.control {
            if let Some(url) = self.editor.read(cx).layout().table_link_at(event.position) {
                self.trace_pointer_route("table_link", event, window, cx);
                self.pointer_anchor = None;
                cx.emit(EditorSurfaceEvent::OpenLink { url });
                cx.stop_propagation();
                return;
            }
        }
        // Images and attachment cards are structural atoms in editing mode.
        // A click must produce a full NodeSelection-style range, not an
        // ambiguous before/after caret. The attachment's double-click is
        // intentionally only a typed request: resolving a verified descriptor
        // and launching the system application belongs to the retained
        // library session.
        // Resolve structural insertion seams before testing the inclusive card
        // bounds. `contains` intentionally includes a block's bottom edge for
        // painting/hit-testing, but that edge is also the wrapper dead-zone
        // between two section-level atoms (or below a terminal atom).  The
        // donor editor materializes/focuses a paragraph for that background
        // seam; only an actual atom-content hit becomes a NodeSelection.
        let activate_dead_zone = !self.mode.is_read_only()
            && !event.modifiers.shift
            && self
                .editor
                .read(cx)
                .layout()
                .atomic_dead_zone_hit(event.position);
        let atomic = if !event.modifiers.shift && !activate_dead_zone {
            self.editor.update(cx, |editor, editor_cx| {
                let hit = editor.select_atomic_at(event.position);
                if hit.is_some() {
                    editor_cx.notify();
                }
                hit
            })
        } else {
            None
        };
        if let Some(hit) = atomic {
            self.trace_pointer_route("atomic", event, window, cx);
            self.pointer_anchor = None;
            focus_editor(&self.editor, window, cx);
            if event.click_count >= 2 {
                match hit {
                    AtomicBlockHit::Attachment { resource_id } => {
                        cx.emit(EditorSurfaceEvent::OpenAttachment { resource_id });
                    }
                    AtomicBlockHit::Table {
                        attachment: Some(resource_id),
                        ..
                    } => {
                        cx.emit(EditorSurfaceEvent::OpenAttachment { resource_id });
                    }
                    AtomicBlockHit::Table {
                        node_id,
                        row,
                        column,
                        attachment: None,
                    } if !self.mode.is_read_only() => {
                        cx.emit(EditorSurfaceEvent::EditTableCell {
                            node_id,
                            row,
                            column,
                        });
                    }
                    _ => {}
                }
            }
            cx.stop_propagation();
            return;
        }
        self.pointer_anchor = self.editor.update(cx, |editor, editor_cx| {
            // The narrow gap between two atomic blocks (and the area below a
            // terminal one) is a real insertion target. Materialize the
            // paragraph before focus/IME sees it; a click inside the card or
            // image itself remains an atomic selection for resource actions.
            let anchor = editor.begin_pointer_selection(event.position, event.modifiers.shift);
            if activate_dead_zone {
                if editor.activate_atomic_dead_zone().is_err() {
                    // A stale/offscreen geometry hit must remain harmless:
                    // fall back to ordinary pointer selection rather than
                    // swallowing focus or inventing a partial document edit.
                }
            }
            editor_cx.notify();
            if activate_dead_zone {
                Some(editor.selection().anchor)
            } else {
                anchor
            }
        });
        self.trace_pointer_route(
            if activate_dead_zone {
                "dead_zone"
            } else {
                "text_selection"
            },
            event,
            window,
            cx,
        );
        focus_editor(&self.editor, window, cx);
        cx.stop_propagation();
    }

    fn on_mouse_move(
        &mut self,
        event: &MouseMoveEvent,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.editor.read(cx).is_resizing_image() {
            let _ = self.editor.update(cx, |editor, editor_cx| {
                editor.update_image_resize(event.position);
                editor_cx.notify();
            });
            return;
        }
        if !event.dragging() {
            return;
        }
        let Some(anchor) = self.pointer_anchor else {
            return;
        };
        let _ = self.editor.update(cx, |editor, editor_cx| {
            editor.update_pointer_selection(anchor, event.position);
            editor_cx.notify();
        });
    }

    fn on_mouse_up(&mut self, event: &MouseUpEvent, _window: &mut Window, cx: &mut Context<Self>) {
        self.pointer_anchor = None;
        if self.editor.read(cx).is_resizing_image() {
            let _ = self.editor.update(cx, |editor, editor_cx| {
                if editor.finish_image_resize(event.position).is_err() {
                    editor.cancel_image_resize();
                }
                editor_cx.notify();
            });
        }
        cx.notify();
    }

    fn on_key_down(&mut self, event: &KeyDownEvent, _window: &mut Window, cx: &mut Context<Self>) {
        // A key here reached the app, not the input method.
        super::input_trace::record("body", "key_down", || {
            let modifiers = &event.keystroke.modifiers;
            serde_json::json!({
                "key": event.keystroke.key,
                "key_char": event.keystroke.key_char,
                "cmd": modifiers.platform,
                "ctrl": modifiers.control,
                "alt": modifiers.alt,
                "shift": modifiers.shift,
                "held": event.is_held,
                "composing": self.editor.read(cx).marked_text().is_some(),
            })
        });
        if self.find_panel_open && matches!(event.keystroke.key.as_str(), "escape" | "esc") {
            cx.emit(EditorSurfaceEvent::DismissFindInNote);
            cx.stop_propagation();
            return;
        }
        if !event.keystroke.modifiers.shift {
            return;
        }
        let modifiers = &event.keystroke.modifiers;
        if cfg!(target_os = "macos")
            && modifiers.platform
            && !modifiers.control
            && !modifiers.alt
            && matches!(event.keystroke.key.as_str(), "up" | "down")
        {
            let to_end = event.keystroke.key == "down";
            let _ = self.editor.update(cx, |editor, editor_cx| {
                editor.select_document_edge(to_end);
                editor_cx.notify();
            });
            self.pending_caret_reveal = 3;
            cx.notify();
            cx.stop_propagation();
            return;
        }
        let operation = match event.keystroke.key.as_str() {
            "up" => Some(EditorCore::select_up as fn(&mut EditorCore)),
            "down" => Some(EditorCore::select_down as fn(&mut EditorCore)),
            _ => None,
        };
        let Some(operation) = operation else {
            return;
        };
        let _ = self.editor.update(cx, |editor, editor_cx| {
            operation(editor);
            editor_cx.notify();
        });
        cx.stop_propagation();
    }
}

impl Render for EditorSurface {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        #[cfg(test)]
        {
            self.light_surface_paint_for_test = EditorSurfaceLightContract {
                background: 0,
                border: 0,
                foreground: 0,
            };
        }
        let editor = self.editor.clone();
        let measured_height = editor.read(cx).layout().total_height().max(480.0);
        let (content_width, content_height, embedded) = self
            .embedded_frame
            .map(|(width, height)| (width, height, true))
            // The library surface's 1px border sits outside its content.
            .unwrap_or((1.0, measured_height + 2.0, false));
        let readonly_notice = match self.mode {
            EditorSurfaceMode::Editable => None,
            EditorSurfaceMode::ReadOnly => Some(
                div()
                    .id("native-editor-surface-trash-readonly-notice")
                    .debug_selector(|| "native-editor-surface-trash-readonly-notice".to_owned())
                    .absolute()
                    .top(px(10.0))
                    .right(px(14.0))
                    .px(px(8.0))
                    .py(px(4.0))
                    .rounded(px(5.0))
                    .bg(rgba(0xfff3cdff))
                    .text_size(px(11.0))
                    .text_color(rgba(0x6f5200ff))
                    .child("废纸篓中的笔记为只读；恢复后可编辑"),
            ),
            EditorSurfaceMode::RecoveryLocked => Some(
                div()
                    .id("native-editor-surface-recovery-locked-notice")
                    .debug_selector(|| "native-editor-surface-recovery-locked-notice".to_owned())
                    .absolute()
                    .top(px(10.0))
                    .right(px(14.0))
                    .px(px(8.0))
                    .py(px(4.0))
                    .rounded(px(5.0))
                    .bg(rgba(0xfff3cdff))
                    .text_size(px(11.0))
                    .text_color(rgba(0x6f5200ff))
                    .child("资料库已提交，正在恢复界面；暂不可编辑"),
            ),
        };
        let before_shape = self.hooks.clone();
        let after_paint = self.hooks.clone();
        let pending_find_reveal = self.pending_find_reveal.take();
        let pending_caret_reveal = std::mem::take(&mut self.pending_caret_reveal);
        let caret_reveal_editor = editor.clone();
        let caret_reveal_scroll_handle = self.scroll_handle.clone();
        let caret_reveal_surface = cx.entity().downgrade();
        let find_reveal_editor = editor.clone();
        let find_reveal_scroll_handle = self.scroll_handle.clone();
        let find_reveal_surface = cx.entity().downgrade();
        let mut surface = div()
            .id("native-editor-surface")
            .debug_selector(|| "native-editor-surface".to_owned())
            .key_context("BlockEditor")
            .track_focus(editor.read(cx).focus_handle())
            .h(px(content_height))
            .rounded(px(7.0))
            .bg(self.light_surface_background());
        // The spike's established embedded canvas owns its surrounding page
        // chrome. Only the ordinary LibraryShell needs the explicit boundary
        // between an opaque editor pane and the document surface.
        if !embedded {
            surface = surface
                .border_1()
                .border_color(self.light_surface_border())
                .text_color(self.light_surface_foreground());
        }
        if embedded {
            surface = surface.w(px(content_width));
        } else {
            surface = surface.w_full();
        }
        if self.accepts_pointer_input {
            surface = surface
                .on_key_down(cx.listener(Self::on_key_down))
                .on_mouse_move(cx.listener(Self::on_mouse_move))
                .on_scroll_wheel(cx.listener(|this, event: &gpui::ScrollWheelEvent, _, cx| {
                    let delta = event.delta.pixel_delta(px(24.0));
                    // The user's own scroll wins over a Find reveal still settling.
                    this.pending_find_reveal = None;
                    this.pending_caret_reveal = 0;
                    if delta.x.abs() <= delta.y.abs() {
                        return;
                    }
                    let handled = this.editor.update(cx, |editor, editor_cx| {
                        let handled = editor.scroll_table_at(event.position, f32::from(delta.x));
                        if handled {
                            editor_cx.notify();
                        }
                        handled
                    });
                    if handled {
                        cx.stop_propagation();
                    }
                }))
                .on_mouse_up(MouseButton::Left, cx.listener(Self::on_mouse_up))
                .on_mouse_up_out(MouseButton::Left, cx.listener(Self::on_mouse_up))
                .capture_any_mouse_down(cx.listener(Self::on_mouse_down));
            // The library body shares the donor's BlockEditor shortcut
            // context but does not use the spike's parent action handlers.
            // Route formatting through the same catalogue as toolbar clicks.
            bind_format_action!(surface, editor, BoldSelection, EditorCommand::Bold);
            bind_format_action!(surface, editor, ItalicSelection, EditorCommand::Italic);
            bind_format_action!(
                surface,
                editor,
                UnderlineSelection,
                EditorCommand::Underline
            );
            bind_format_action!(
                surface,
                editor,
                SuperscriptSelection,
                EditorCommand::Superscript
            );
            bind_format_action!(
                surface,
                editor,
                SubscriptSelection,
                EditorCommand::Subscript
            );
        }
        // Preserve the donor's standard key context on the one shared
        // document entity. Mutation permissions are deliberately enforced in
        // `EditorCore`, not by leaving an alternate keyboard route unbound:
        // a read-only surface gets harmless `ReadOnly` errors, while the
        // editable library and spike use identical Delete/Undo behavior.
        bind_result_action!(surface, editor, Newline, insert_paragraph_break);
        bind_result_action!(surface, editor, DeleteBack, backspace);
        bind_result_action!(surface, editor, Delete, delete_forward);
        bind_result_action!(surface, editor, Undo, undo);
        bind_result_action!(surface, editor, Redo, redo);
        bind_selection_action!(surface, editor, MoveLeft, move_left);
        bind_selection_action!(surface, editor, MoveRight, move_right);
        bind_selection_action!(surface, editor, Home, move_home);
        bind_selection_action!(surface, editor, End, move_end);
        bind_selection_action!(surface, editor, SelectLeft, select_left);
        bind_selection_action!(surface, editor, SelectRight, select_right);
        bind_selection_action!(surface, editor, WordSelectLeft, select_word_left);
        bind_selection_action!(surface, editor, WordSelectRight, select_word_right);
        bind_selection_action!(surface, editor, SelectHome, select_home);
        bind_selection_action!(surface, editor, SelectEnd, select_end);
        bind_selection_action!(surface, editor, BlockUp, move_up);
        bind_selection_action!(surface, editor, BlockDown, move_down);
        // The shared shortcut catalogue maps Cmd-Up/Down and Ctrl-Home/End
        // to document jumps. A viewport-only jump leaves the insertion point
        // behind, so a subsequent Return or paste edits the wrong paragraph.
        // An embedded surface (the measurement spike) does not scroll
        // itself; it leaves these actions to its host as before.
        if self.embedded_frame.is_none() {
            for to_end in [false, true] {
                let jump_editor = editor.clone();
                let jump_handle = self.scroll_handle.clone();
                let jump_surface = cx.entity().downgrade();
                let jump = move |window: &mut Window, cx: &mut App| {
                    jump_editor_to_document_edge(&jump_editor, &jump_handle, to_end, window, cx);
                    let _ = jump_surface.update(cx, |surface, surface_cx| {
                        surface.pending_caret_reveal = 3;
                        surface_cx.notify();
                    });
                };
                surface = if to_end {
                    surface.on_action(move |_action: &JumpToBottom, window, cx| jump(window, cx))
                } else {
                    surface.on_action(move |_action: &JumpToTop, window, cx| jump(window, cx))
                };
            }
        }
        let focus_prev_editor = editor.clone();
        let focus_prev_handle = self.scroll_handle.clone();
        surface = surface.on_action(move |_action: &FocusPrev, window, cx| {
            move_editor_vertically_and_reveal(&focus_prev_editor, &focus_prev_handle, -1, cx);
            focus_editor(&focus_prev_editor, window, cx);
            window.refresh();
        });
        let focus_next_editor = editor.clone();
        let focus_next_handle = self.scroll_handle.clone();
        surface = surface.on_action(move |_action: &FocusNext, window, cx| {
            move_editor_vertically_and_reveal(&focus_next_editor, &focus_next_handle, 1, cx);
            focus_editor(&focus_next_editor, window, cx);
            window.refresh();
        });
        bind_selection_action!(surface, editor, SelectAll, select_all);
        // Page navigation belongs to this retained scroll owner, not to a
        // block-level selection action.  The other editor implementation has
        // the same viewport-sized step; keep this canvas focused while the
        // document itself remains untouched.
        let page_up_handle = self.scroll_handle.clone();
        surface = surface.on_action(move |_action: &PageUp, window, _cx| {
            scroll_handle_by_page(&page_up_handle, 1.0);
            window.refresh();
        });
        let page_down_handle = self.scroll_handle.clone();
        surface = surface.on_action(move |_action: &PageDown, window, _cx| {
            scroll_handle_by_page(&page_down_handle, -1.0);
            window.refresh();
        });
        let copy_editor = editor.clone();
        let copy_surface = cx.entity().downgrade();
        let to_owner = self.clipboard_to_owner;
        surface = surface.on_action(move |_action: &Copy, _window, cx| {
            if to_owner {
                let _ = copy_surface.update(cx, |_, surface_cx| {
                    surface_cx.emit(EditorSurfaceEvent::Clipboard { cut: false })
                });
                return;
            }
            let text = copy_editor.read(cx).copy_plain_text();
            if !text.is_empty() {
                cx.write_to_clipboard(ClipboardItem::new_string(text));
            }
        });
        let cut_editor = editor.clone();
        let cut_surface = cx.entity().downgrade();
        surface = surface.on_action(move |_action: &Cut, _window, cx| {
            if to_owner {
                let _ = cut_surface.update(cx, |_, surface_cx| {
                    surface_cx.emit(EditorSurfaceEvent::Clipboard { cut: true })
                });
                return;
            }
            let cut = cut_editor.update(cx, |editor, editor_cx| {
                let result = editor.cut_selection();
                editor_cx.notify();
                result
            });
            if let Ok(text) = cut
                && !text.is_empty()
            {
                cx.write_to_clipboard(ClipboardItem::new_string(text));
            }
        });
        let surface = surface.child(editor_canvas(
            editor,
            self.image_cache.clone(),
            content_height,
            move |window, cx| (before_shape.before_shape)(window, cx),
            move |_window, cx| {
                // An edit/jump can select text beyond the shaped viewport.
                // Seek with the retained height index first, then refine with
                // the actual caret once the next canvas pass shapes it.
                if pending_caret_reveal > 0 {
                    let editor = caret_reveal_editor.read(cx);
                    let atomic = selection_is_atom(editor);
                    // ProseMirror view.scrollToSelection uses the whole node
                    // rect for NodeSelection, not its adjacent caret line.
                    let caret = if atomic {
                        editor
                            .layout()
                            .visible()
                            .iter()
                            .find(|block| block.node_id == editor.selection().head.node_id)
                            .map(|block| block.bounds)
                    } else {
                        editor
                            .layout()
                            .caret_bounds_for_point(editor.selection().head)
                    };
                    let viewport = caret_reveal_scroll_handle.bounds();
                    let before = caret_reveal_scroll_handle.offset().y;
                    let target = caret.or_else(|| {
                        editor
                            .layout()
                            .bounds_for_node(editor.document(), editor.selection().head.node_id)
                            .map(|mut estimate| {
                                // Indexed boxes are document-local; shaped
                                // carets already include the canvas translation.
                                estimate.origin.y += viewport.top() + before + px(1.0);
                                // Seek into a long paragraph, not past its end;
                                // its exact insertion line is resolved next pass.
                                if !atomic {
                                    estimate.size.height = estimate.size.height.min(px(26.0));
                                }
                                estimate
                            })
                    });
                    if let Some(mut target) = target {
                        if atomic {
                            // An image taller than the viewport cannot fit.
                            // Reveal its top once, rather than alternating
                            // between its top and bottom on settling passes.
                            target.size.height = target
                                .size
                                .height
                                .min((viewport.size.height - px(2.0)).max(px(1.0)));
                        }
                        reveal_scroll_bounds(&caret_reveal_scroll_handle, target);
                    }
                    let moved = caret_reveal_scroll_handle.offset().y != before;
                    let settling = editor.image_geometry_settling();
                    if pending_caret_reveal > 1 && (caret.is_none() || moved || settling) {
                        let _ = caret_reveal_surface.update(cx, |surface, surface_cx| {
                            // Hydration will notify when geometry arrives;
                            // don't spin frames while waiting for image IO.
                            surface.pending_caret_reveal = if settling {
                                pending_caret_reveal
                            } else {
                                pending_caret_reveal - 1
                            };
                            if moved || !settling {
                                surface_cx.notify();
                            }
                        });
                    }
                }
                let Some(request) = pending_find_reveal.clone() else {
                    return;
                };
                // Closing Find, another primary, a switched note or an edit
                // ends the request; it never outlives what it was asked for.
                let editor = find_reveal_editor.read(cx);
                if editor.find_primary() != Some(&request.found)
                    || editor.document().revision() != request.revision
                {
                    return;
                }
                let found = &request.found;
                if let Some(cell) = found.cell {
                    let _ = find_reveal_editor.update(cx, |editor, _| {
                        editor.reveal_find_table_column(found.node_id, cell.column);
                    });
                }
                let editor = find_reveal_editor.read(cx);
                let target = editor.layout().find_match_bounds(found);
                let exact = target.is_some();
                let moved = match target {
                    Some(target) => center_scroll_bounds(&find_reveal_scroll_handle, target),
                    // The preceding pass may have measured wrapped text or an
                    // image and shifted the retained height index. Seek again
                    // until the primary's own text is shaped.
                    None => editor
                        .layout()
                        .bounds_for_node(editor.document(), found.node_id)
                        .is_some_and(|estimate| {
                            center_scroll_bounds(&find_reveal_scroll_handle, estimate)
                        }),
                };
                // An image still loading can change heights above the match
                // after this pass; its arrival repaints and re-checks.
                let settling = editor.image_geometry_settling();
                let settled = exact && !moved && !settling;
                if (settled && request.settled_once) || request.passes >= MAX_FIND_REVEAL_PASSES {
                    return;
                }
                let _ = find_reveal_surface.update(cx, |surface, surface_cx| {
                    surface.pending_find_reveal = Some(FindRevealRequest {
                        passes: request.passes + 1,
                        settled_once: settled,
                        ..request
                    });
                    // A loading image repaints this surface when its size
                    // arrives; otherwise ask for the next pass now.
                    if !settling {
                        surface_cx.notify();
                    }
                });
            },
            move |window, cx| (after_paint.after_paint)(window, cx),
        ));
        if embedded {
            return div()
                .relative()
                .child(surface)
                .children(readonly_notice)
                .into_any_element();
        }
        div()
            .id("native-editor-surface-scroll")
            .relative()
            .size_full()
            .overflow_y_scroll()
            .track_scroll(&self.scroll_handle)
            .child(surface)
            .children(readonly_notice)
            .into_any_element()
    }
}

/// Shared donor-backed canvas implementation. The spike supplies a paint hook
/// for measurement telemetry; the library supplies a no-op hook but otherwise
/// executes the exact same shape/translate/`paint_entity` path.
pub fn editor_canvas(
    editor: Entity<EditorCore>,
    image_cache: Option<Entity<BudgetedImageCache>>,
    height: f32,
    before_shape: impl Fn(&mut Window, &mut App) + 'static,
    after_shape: impl Fn(&mut Window, &mut App) + 'static,
    after_paint: impl Fn(&mut Window, &mut App) + 'static,
) -> impl IntoElement {
    let canvas_editor = editor.clone();
    canvas(
        move |bounds, window, cx| {
            before_shape(window, cx);
            let width = f32::from(bounds.size.width).max(1.0);
            let _ = canvas_editor.update(cx, |editor, editor_cx| {
                let previous_height = editor.layout().total_height();
                let (viewport_top, viewport_height) =
                    surface_viewport(bounds, window.content_mask().bounds);
                editor.shape_visible_with_window(viewport_top, viewport_height, width, window);
                editor.translate_layout(f32::from(bounds.origin.x), f32::from(bounds.origin.y));
                if (editor.layout().total_height() - previous_height).abs() > 0.01 {
                    editor_cx.notify();
                }
            });
            after_shape(window, cx);
            canvas_editor.clone()
        },
        move |bounds, entity, window, cx| {
            let _ = render::paint_entity(entity, bounds, image_cache.clone(), window, cx);
            after_paint(window, cx);
        },
    )
    .w_full()
    .h(px(height))
}

pub fn surface_viewport(
    surface: gpui::Bounds<Pixels>,
    content_mask: gpui::Bounds<Pixels>,
) -> (f32, f32) {
    let top = surface.top().max(content_mask.top());
    let bottom = surface.bottom().min(content_mask.bottom());
    (
        f32::from((top - surface.top()).max(px(0.0))),
        f32::from((bottom - top).max(px(1.0))),
    )
}

pub fn focus_editor(editor: &Entity<EditorCore>, window: &mut Window, cx: &mut App) {
    let focus_handle = editor.read(cx).focus_handle().clone();
    focus_handle.focus(window);
}

fn jump_editor_to_document_edge(
    editor: &Entity<EditorCore>,
    scroll_handle: &ScrollHandle,
    to_end: bool,
    window: &mut Window,
    cx: &mut App,
) {
    editor.update(cx, |editor, editor_cx| {
        let offset = if to_end { editor.document_len() } else { 0 };
        editor.select_document_range(offset, offset);
        editor_cx.notify();
    });
    let mut scroll_offset = scroll_handle.offset();
    scroll_offset.y = if to_end {
        -scroll_handle.max_offset().height.max(px(0.0))
    } else {
        px(0.0)
    };
    scroll_handle.set_offset(scroll_offset);
    focus_editor(editor, window, cx);
    window.refresh();
}

/// Move the retained nested editor viewport by exactly one current page.
/// GPUI offsets become increasingly negative toward the document end.
fn scroll_handle_by_page(scroll_handle: &ScrollHandle, direction: f32) {
    let page = scroll_handle.bounds().size.height.max(px(1.0));
    scroll_handle_by_pixels(scroll_handle, page * direction);
}

fn scroll_handle_by_pixels(scroll_handle: &ScrollHandle, delta_y: Pixels) {
    let max_y = scroll_handle.max_offset().height.max(px(0.0));
    let mut offset = scroll_handle.offset();
    offset.y = (offset.y + delta_y).min(px(0.0)).max(-max_y);
    scroll_handle.set_offset(offset);
}

/// Keep arrow-key traversal useful when an atomic image/attachment is the
/// only thing between the selection and the next text node. The ordinary
/// `EditorCore` movement remains authoritative; only a no-op at that
/// structural boundary advances the viewport by two visual lines so holding
/// Down can still reach the next atomic resource without inventing text.
fn move_editor_vertically_and_reveal(
    editor: &Entity<EditorCore>,
    scroll_handle: &ScrollHandle,
    direction: isize,
    cx: &mut App,
) {
    let (moved, caret, stopped_at_resource_atom) = editor.update(cx, |editor, editor_cx| {
        let before = editor.selection();
        if direction < 0 {
            editor.move_up();
        } else {
            editor.move_down();
        }
        let moved = editor.selection() != before;
        let caret = editor
            .layout()
            .caret_bounds_for_point(editor.selection().head);
        let stopped_at_resource_atom = !moved
            && editor.selection().is_caret()
            && editor
                .document()
                .block(editor.selection().head.node_id)
                .is_some_and(|block| {
                    matches!(
                        block.kind,
                        BlockKind::Image | BlockKind::Attachment | BlockKind::Table
                    )
                });
        editor_cx.notify();
        (moved, caret, stopped_at_resource_atom)
    });
    // Once movement stops at an atom boundary, that old caret must not pull
    // the viewport back to itself on every Down press. Only a genuine
    // selection move asks to reveal a caret; boundary repeats use the line
    // scroll fallback below.
    if moved && let Some(caret) = caret {
        reveal_scroll_bounds(scroll_handle, caret);
    }
    if stopped_at_resource_atom {
        let line_step = px(48.0) * if direction < 0 { 1.0 } else { -1.0 };
        scroll_handle_by_pixels(scroll_handle, line_step);
    }
}

/// Find's reveal, as Evernote's `scrollIntoView({block: 'center'})`: the
/// target's centre to the viewport's, or its top when it is taller than the
/// viewport, within the scroll range. Whether the offset moved.
fn center_scroll_bounds(scroll_handle: &ScrollHandle, target: gpui::Bounds<Pixels>) -> bool {
    let viewport = scroll_handle.bounds();
    let delta_y = if target.size.height >= viewport.size.height {
        viewport.top() - target.top()
    } else {
        viewport.center().y - target.center().y
    };
    let before = scroll_handle.offset().y;
    scroll_handle_by_pixels(scroll_handle, delta_y);
    (scroll_handle.offset().y - before).abs() > px(0.5)
}

fn selection_is_atom(editor: &EditorCore) -> bool {
    let selection = editor.selection();
    !selection.is_caret()
        && selection.anchor.node_id == selection.head.node_id
        && editor
            .document()
            .block(selection.head.node_id)
            .is_some_and(|block| block.content.as_text().is_none())
}

fn reveal_scroll_bounds(scroll_handle: &ScrollHandle, target: gpui::Bounds<Pixels>) {
    let viewport = scroll_handle.bounds();
    let delta_y = if target.top() < viewport.top() {
        viewport.top() - target.top()
    } else if target.bottom() > viewport.bottom() {
        viewport.bottom() - target.bottom()
    } else {
        px(0.0)
    };
    if delta_y != px(0.0) {
        scroll_handle_by_pixels(scroll_handle, delta_y);
    }
}
