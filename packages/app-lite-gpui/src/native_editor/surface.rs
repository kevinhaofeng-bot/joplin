//! Reusable GPUI shell around the document-wide native editor canvas.
//!
//! The spike and library routes deliberately share the shaping/painting helper
//! below.  The library owns this `EditorSurface` entity, while the spike keeps
//! its measurement callbacks around the same canvas helper.

use super::core::EditorCore;
use super::images::BudgetedImageCache;
use super::model::DocPoint;
use super::render;
use crate::components::{
    BlockDown, BlockUp, Copy, End, FocusNext, FocusPrev, Home, MoveLeft, MoveRight, SelectAll,
    SelectEnd, SelectHome, SelectLeft, SelectRight, WordSelectLeft, WordSelectRight,
};
use gpui::{
    App, ClipboardItem, Context, Entity, InteractiveElement, IntoElement, KeyDownEvent,
    MouseButton, MouseDownEvent, MouseMoveEvent, MouseUpEvent, ParentElement, Pixels, Render,
    ScrollHandle, StatefulInteractiveElement, Styled, Subscription, Window, canvas, div, px, rgba,
};
use std::sync::Arc;

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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EditorSurfaceMode {
    Editable,
    ReadOnly,
}

impl EditorSurfaceMode {
    pub const fn is_read_only(self) -> bool {
        matches!(self, Self::ReadOnly)
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
}

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
        }
    }

    pub fn editor(&self) -> &Entity<EditorCore> {
        &self.editor
    }

    pub fn mode(&self) -> EditorSurfaceMode {
        self.mode
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
        self.pointer_anchor = self.editor.update(cx, |editor, editor_cx| {
            let anchor = editor.begin_pointer_selection(event.position, event.modifiers.shift);
            editor_cx.notify();
            anchor
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
        let editor = self.editor.clone();
        let measured_height = editor.read(cx).layout().total_height().max(480.0);
        let (content_width, content_height, embedded) = self
            .embedded_frame
            .map(|(width, height)| (width, height, true))
            .unwrap_or((1.0, measured_height, false));
        let readonly_notice = self.mode.is_read_only().then(|| {
            div()
                .absolute()
                .top(px(10.0))
                .right(px(14.0))
                .px(px(8.0))
                .py(px(4.0))
                .rounded(px(5.0))
                .bg(rgba(0xfff3cdff))
                .text_size(px(11.0))
                .text_color(rgba(0x6f5200ff))
                .child("只读预览：保存将在下一阶段启用")
        });
        let before_shape = self.hooks.clone();
        let after_paint = self.hooks.clone();
        let mut surface = div()
            .id("native-editor-surface")
            .debug_selector(|| "native-editor-surface".to_owned())
            .key_context("BlockEditor")
            .track_focus(editor.read(cx).focus_handle())
            .h(px(content_height))
            .rounded(px(7.0))
            .bg(rgba(0xffffffff));
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
        // These are deliberately the non-mutating half of the donor action
        // map. A Task 4 library surface must remain useful for keyboard
        // selection and copying, while all mutation entry points stay behind
        // `EditorCore::ensure_editable`. The editable spike can also receive
        // these handlers through the same mounted entity.
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
        bind_selection_action!(surface, editor, FocusPrev, move_up);
        bind_selection_action!(surface, editor, FocusNext, move_down);
        bind_selection_action!(surface, editor, SelectAll, select_all);
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
