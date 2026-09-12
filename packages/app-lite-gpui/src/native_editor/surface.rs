//! Reusable GPUI shell around the document-wide native editor canvas.
//!
//! The spike and library routes deliberately share the shaping/painting helper
//! below.  The library owns this `EditorSurface` entity, while the spike keeps
//! its measurement callbacks around the same canvas helper.

use super::core::{AtomicBlockHit, EditorCore};
use super::images::BudgetedImageCache;
use super::model::{BlockKind, DocPoint};
use super::render;
use crate::components::{
    BlockDown, BlockUp, Copy, Delete, DeleteBack, End, FocusNext, FocusPrev, Home, MoveLeft,
    MoveRight, Newline, PageDown, PageUp, Redo, SelectAll, SelectEnd, SelectHome, SelectLeft,
    SelectRight, Undo, WordSelectLeft, WordSelectRight,
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
    OpenAttachment { resource_id: String },
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
    // The library owns a nested document scroll area. The spike already owns
    // a larger scroll column (title, toolbar, body), so it mounts the same
    // surface as an embedded canvas and keeps that established scroll owner.
    embedded_frame: Option<(f32, f32)>,
    accepts_pointer_input: bool,
    _editor_subscription: Subscription,
    #[cfg(test)]
    light_surface_paint_for_test: EditorSurfaceLightContract,
}

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
        let subscription = cx.observe(&editor, |_, _, cx| cx.notify());
        Self {
            editor,
            mode,
            scroll_handle: ScrollHandle::new(),
            pointer_anchor: None,
            image_cache,
            hooks: EditorSurfaceHooks::default(),
            embedded_frame,
            accepts_pointer_input,
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

    pub fn mode(&self) -> EditorSurfaceMode {
        self.mode
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
            self.pointer_anchor = None;
            focus_editor(&self.editor, window, cx);
            if event.click_count >= 2
                && let AtomicBlockHit::Attachment { resource_id } = hit
            {
                cx.emit(EditorSurfaceEvent::OpenAttachment { resource_id });
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
        focus_editor(&self.editor, window, cx);
        cx.stop_propagation();
    }

    fn on_mouse_move(
        &mut self,
        event: &MouseMoveEvent,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
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

    fn on_mouse_up(&mut self, _event: &MouseUpEvent, _window: &mut Window, cx: &mut Context<Self>) {
        self.pointer_anchor = None;
        cx.notify();
    }

    fn on_key_down(&mut self, event: &KeyDownEvent, _window: &mut Window, cx: &mut Context<Self>) {
        if !event.keystroke.modifiers.shift {
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
            .unwrap_or((1.0, measured_height, false));
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
                .on_mouse_up(MouseButton::Left, cx.listener(Self::on_mouse_up))
                .on_mouse_up_out(MouseButton::Left, cx.listener(Self::on_mouse_up))
                .capture_any_mouse_down(cx.listener(Self::on_mouse_down));
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
        surface = surface.on_action(move |_action: &Copy, _window, cx| {
            let text = copy_editor.read(cx).copy_plain_text();
            if !text.is_empty() {
                cx.write_to_clipboard(ClipboardItem::new_string(text));
            }
        });
        let surface = surface.child(editor_canvas(
            editor,
            self.image_cache.clone(),
            content_height,
            move |window, cx| (before_shape.before_shape)(window, cx),
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
                    matches!(block.kind, BlockKind::Image | BlockKind::Attachment)
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
