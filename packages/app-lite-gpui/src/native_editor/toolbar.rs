//! Shared Evernote-style formatting command chrome.
//!
//! The measurement spike and the ordinary library route deliberately mount
//! this same retained command owner.  It is the only GPUI layer that turns a
//! toolbar click into a [`CommandCatalogue`] execution, owns More/Link
//! presentation state, and restores the document focus after chrome input.
//! The editor itself remains the sole owner of selection, history, and
//! document mutations.

use std::ops::Range;
use std::sync::Arc;

use gpui::{
    AnyWindowHandle, App, AppContext, Bounds, ClipboardItem, Context, ElementInputHandler, Entity,
    EntityInputHandler, EventEmitter, FocusHandle, InteractiveElement, IntoElement, KeyDownEvent,
    MouseButton, MouseDownEvent, ParentElement, Pixels, Point, SharedString,
    StatefulInteractiveElement, Styled, Subscription, TextRun, UTF16Selection, Window, canvas, div,
    point, px, rgba, size, svg,
};
use unicode_segmentation::UnicodeSegmentation;

use super::chrome::{EVERNOTE_GREEN, ToolbarPlacement, toolbar_placement};
use super::commands::{
    CommandArgument, CommandCatalogue, CommandDescriptor, EditorCommand, ToggleState,
};
use super::core::EditorCore;
use super::model::{Mark, Selection};
use super::surface::focus_editor;

gpui::actions!(editor_command_chrome, [SubmitLink, CancelLink]);

/// The only host-specific branch in the shared chrome.  Button behaviour,
/// command state, More placement and Link focus handling stay identical.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EditorCommandChromeHost {
    Spike,
    Library,
}

impl EditorCommandChromeHost {
    const fn root_id(self) -> &'static str {
        match self {
            Self::Spike => "evernote-native-spike-command-chrome",
            Self::Library => "library-editor-command-chrome",
        }
    }

    const fn toolbar_id(self) -> &'static str {
        match self {
            Self::Spike => "evernote-native-spike-primary-toolbar",
            Self::Library => "library-editor-command-toolbar",
        }
    }

    const fn more_trigger_id(self) -> &'static str {
        match self {
            Self::Spike => "evernote-native-spike-more-trigger",
            Self::Library => "library-editor-command-more-trigger",
        }
    }

    const fn more_menu_id(self) -> &'static str {
        match self {
            Self::Spike => "evernote-native-spike-more-menu",
            Self::Library => "library-editor-command-more-menu",
        }
    }

    const fn overlay_backdrop_id(self) -> &'static str {
        match self {
            Self::Spike => "evernote-native-spike-command-overlay-backdrop",
            Self::Library => "library-editor-command-overlay-backdrop",
        }
    }
}

/// A host consumes this typed request with its own platform adapter.  The
/// shared chrome never learns about profile paths, staged resource workers, or
/// a picker implementation.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum EditorCommandChromeEvent {
    RequestInsertImage { window: AnyWindowHandle },
}

/// The primary row belongs in a host's normal flow; menus/popovers are
/// returned separately so each host can attach them to its established window
/// overlay root without reimplementing the command UI.
pub struct EditorCommandChromeRender {
    pub toolbar: gpui::AnyElement,
    pub overlays: Vec<gpui::AnyElement>,
}

/// Small real text-input owner for Link.  The document selection remains in
/// `EditorCore`; only this URL field owns platform text focus while open.
pub(crate) struct LinkPopover {
    pub(crate) focus: FocusHandle,
    anchor: Option<Point<Pixels>>,
    pub(crate) text: String,
    pub(crate) selection: Range<usize>,
    pub(crate) reversed: bool,
    marked: Option<Range<usize>>,
    last_bounds: Option<Bounds<Pixels>>,
    last_layout: Option<gpui::ShapedLine>,
    pub(crate) invalid: bool,
}

impl LinkPopover {
    pub(crate) fn new(initial: String, cx: &mut Context<Self>) -> Self {
        let end = initial.len();
        Self {
            focus: cx.focus_handle(),
            anchor: None,
            text: initial,
            selection: end..end,
            reversed: false,
            marked: None,
            last_bounds: None,
            last_layout: None,
            invalid: false,
        }
    }

    fn anchored_at(mut self, anchor: Point<Pixels>) -> Self {
        self.anchor = Some(anchor);
        self
    }

    fn replace_range(&mut self, range: Option<Range<usize>>, text: &str) {
        let byte_range = range
            .or_else(|| self.marked.clone())
            .unwrap_or_else(|| self.selection.clone());
        let start = byte_range.start.min(self.text.len());
        let end = byte_range.end.min(self.text.len()).max(start);
        if !self.text.is_char_boundary(start) || !self.text.is_char_boundary(end) {
            return;
        }
        self.text.replace_range(start..end, text);
        let caret = start.saturating_add(text.len());
        self.selection = caret..caret;
        self.reversed = false;
    }

    fn ordered_selection(&self) -> (usize, usize) {
        (self.selection.start, self.selection.end)
    }

    fn cursor_offset(&self) -> usize {
        if self.reversed {
            self.selection.start
        } else {
            self.selection.end
        }
    }

    fn anchor_offset(&self) -> usize {
        if self.reversed {
            self.selection.end
        } else {
            self.selection.start
        }
    }

    fn set_selection(&mut self, anchor: usize, focus: usize) {
        self.selection = anchor.min(focus)..anchor.max(focus);
        self.reversed = !self.selection.is_empty() && focus < anchor;
    }

    fn collapse(&mut self, offset: usize) {
        self.selection = offset..offset;
        self.reversed = false;
    }

    pub(crate) fn move_horizontal(&mut self, right: bool, extend: bool) {
        let (start, end) = self.ordered_selection();
        let head = self.cursor_offset();
        if !extend && start != end {
            self.collapse(if right { end } else { start });
            return;
        }
        let target = if right {
            next_char_boundary(&self.text, head)
        } else {
            previous_char_boundary(&self.text, head)
        };
        if extend {
            self.set_selection(self.anchor_offset(), target);
        } else {
            self.collapse(target);
        }
    }

    pub(crate) fn move_to_edge(&mut self, end: bool, extend: bool) {
        let target = if end { self.text.len() } else { 0 };
        if extend {
            self.set_selection(self.anchor_offset(), target);
        } else {
            self.collapse(target);
        }
    }

    pub(crate) fn delete_backward(&mut self) {
        let (start, end) = self.ordered_selection();
        if start != end {
            self.replace_range(Some(start..end), "");
        } else if start > 0 {
            self.replace_range(Some(previous_char_boundary(&self.text, start)..start), "");
        }
        self.marked = None;
    }

    fn delete_forward(&mut self) {
        let (start, end) = self.ordered_selection();
        if start != end {
            self.replace_range(Some(start..end), "");
        } else if start < self.text.len() {
            self.replace_range(Some(start..next_char_boundary(&self.text, start)), "");
        }
        self.marked = None;
    }

    fn select_all(&mut self) {
        self.set_selection(0, self.text.len());
    }

    pub(crate) fn selected_text(&self) -> String {
        self.text
            .get(self.selection.start..self.selection.end)
            .unwrap_or_default()
            .to_owned()
    }

    fn paste_from_clipboard(&mut self, cx: &mut Context<Self>) {
        if let Some(text) = cx.read_from_clipboard().and_then(|item| item.text()) {
            self.replace_range(None, &text);
            self.marked = None;
            cx.notify();
        }
    }
}

fn previous_char_boundary(text: &str, offset: usize) -> usize {
    text.grapheme_indices(true)
        .map(|(index, _)| index)
        .take_while(|index| *index < offset.min(text.len()))
        .last()
        .unwrap_or(0)
}

fn next_char_boundary(text: &str, offset: usize) -> usize {
    text.grapheme_indices(true)
        .map(|(index, _)| index)
        .find(|index| *index > offset.min(text.len()))
        .unwrap_or(text.len())
}

impl EntityInputHandler for LinkPopover {
    fn text_for_range(
        &mut self,
        range_utf16: Range<usize>,
        adjusted_range: &mut Option<Range<usize>>,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<String> {
        let range = crate::native_editor::input::utf16_range_to_utf8_in(&self.text, &range_utf16);
        adjusted_range.replace(crate::native_editor::input::utf8_range_to_utf16_in(
            &self.text, &range,
        ));
        self.text.get(range).map(str::to_owned)
    }

    fn selected_text_range(
        &mut self,
        _ignore_disabled_input: bool,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<UTF16Selection> {
        Some(UTF16Selection {
            range: crate::native_editor::input::utf8_range_to_utf16_in(&self.text, &self.selection),
            reversed: self.reversed,
        })
    }

    fn marked_text_range(
        &self,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<Range<usize>> {
        self.marked
            .as_ref()
            .map(|range| crate::native_editor::input::utf8_range_to_utf16_in(&self.text, range))
    }

    fn unmark_text(&mut self, _window: &mut Window, _cx: &mut Context<Self>) {
        self.marked = None;
    }

    fn replace_text_in_range(
        &mut self,
        range: Option<Range<usize>>,
        text: &str,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let range = range
            .map(|range| crate::native_editor::input::utf16_range_to_utf8_in(&self.text, &range));
        self.replace_range(range, text);
        self.marked = None;
        cx.notify();
    }

    fn replace_and_mark_text_in_range(
        &mut self,
        range: Option<Range<usize>>,
        new_text: &str,
        new_selected_range: Option<Range<usize>>,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let range_utf8 = range
            .as_ref()
            .map(|range| crate::native_editor::input::utf16_range_to_utf8_in(&self.text, range));
        let replacement = range_utf8.clone().or_else(|| self.marked.clone());
        let start = replacement
            .as_ref()
            .map(|range| range.start)
            .unwrap_or(self.selection.start);
        self.replace_range(replacement, new_text);
        if let Some(selected_range) = new_selected_range {
            let selected_range =
                crate::native_editor::input::utf16_range_to_utf8_in(new_text, &selected_range);
            self.selection = start + selected_range.start..start + selected_range.end;
            self.reversed = false;
        }
        self.marked = (!new_text.is_empty()).then_some(start..start + new_text.len());
        cx.notify();
    }

    fn bounds_for_range(
        &mut self,
        range_utf16: Range<usize>,
        _element_bounds: Bounds<Pixels>,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<Bounds<Pixels>> {
        let bounds = self.last_bounds?;
        let line = self.last_layout.as_ref()?;
        let range = crate::native_editor::input::utf16_range_to_utf8_in(&self.text, &range_utf16);
        Some(Bounds::from_corners(
            point(bounds.left() + line.x_for_index(range.start), bounds.top()),
            point(bounds.left() + line.x_for_index(range.end), bounds.bottom()),
        ))
    }

    fn character_index_for_point(
        &mut self,
        point: Point<Pixels>,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<usize> {
        let bounds = self.last_bounds?;
        let line = self.last_layout.as_ref()?;
        let x = (point.x - bounds.left()).max(px(0.0)).min(line.width);
        let index = line.closest_index_for_x(x).min(self.text.len());
        Some(crate::native_editor::input::utf8_to_utf16_in(
            &self.text, index,
        ))
    }
}

/// One retained owner for the primary row, More menu and Link popover.  The
/// command catalogue is intentionally held here rather than in either host so
/// a command cannot grow a second library-only handler or UI-only toggle bit.
pub struct EditorCommandChrome {
    editor: Entity<EditorCore>,
    /// The last document selection observed from the one shared editor. This
    /// deliberately tracks editor state rather than toolbar/popover input:
    /// More becomes stale when the user navigates the document, while typing
    /// in Link's separate URL field must leave the saved editor range alone.
    last_editor_selection: Selection,
    catalogue: CommandCatalogue,
    host: EditorCommandChromeHost,
    more_open: bool,
    link_popover: Option<Entity<LinkPopover>>,
    more_trigger_bounds: Option<Bounds<Pixels>>,
    insert_image_dispatch: Arc<dyn Fn(AnyWindowHandle, &mut App)>,
    _editor_subscription: Subscription,
}

impl EventEmitter<EditorCommandChromeEvent> for EditorCommandChrome {}

impl EditorCommandChrome {
    pub fn new(
        editor: Entity<EditorCore>,
        host: EditorCommandChromeHost,
        insert_image_dispatch: impl Fn(AnyWindowHandle, &mut App) + 'static,
        cx: &mut Context<Self>,
    ) -> Self {
        let last_editor_selection = editor.read(cx).selection();
        let subscription = cx.observe(&editor, |chrome, editor, cx| {
            let selection = editor.read(cx).selection();
            if selection != chrome.last_editor_selection {
                chrome.last_editor_selection = selection;
                // Link owns an independent text input while retaining the
                // original EditorCore selection. Only More is selection-bound
                // presentation state, so URL-field edits cannot falsely
                // dismiss the Link popover.
                chrome.more_open = false;
            }
            cx.notify();
        });
        Self {
            editor,
            last_editor_selection,
            catalogue: CommandCatalogue::new(),
            host,
            more_open: false,
            link_popover: None,
            more_trigger_bounds: None,
            insert_image_dispatch: Arc::new(insert_image_dispatch),
            _editor_subscription: subscription,
        }
    }

    pub fn editor(&self) -> &Entity<EditorCore> {
        &self.editor
    }

    pub fn has_open_overlay(&self) -> bool {
        self.more_open || self.link_popover.is_some()
    }

    pub fn has_link_popover(&self) -> bool {
        self.link_popover.is_some()
    }

    pub fn accepts_surface_pointer_input(&self) -> bool {
        !self.has_open_overlay()
    }

    pub fn dismiss_overlay(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        let dismissed = self.has_open_overlay();
        if dismissed {
            self.more_open = false;
            self.link_popover = None;
            focus_editor(&self.editor, window, cx);
            cx.notify();
        }
        dismissed
    }

    #[cfg(test)]
    pub(crate) fn more_open_for_test(&self) -> bool {
        self.more_open
    }

    #[cfg(test)]
    pub(crate) fn link_popover_for_test(&self) -> Option<Entity<LinkPopover>> {
        self.link_popover.clone()
    }

    #[cfg(test)]
    pub(crate) fn catalogue_for_test(&self) -> CommandCatalogue {
        self.catalogue
    }

    fn selected_link_url(&self, cx: &App) -> String {
        self.editor
            .read(cx)
            .selected_text_ranges()
            .into_iter()
            .find_map(|(node_id, range)| {
                self.editor
                    .read(cx)
                    .document()
                    .block(node_id)?
                    .content
                    .styles()?
                    .iter()
                    .find_map(|run| {
                        (run.range.start <= range.start && run.range.end >= range.end)
                            .then(|| {
                                run.marks.iter().find_map(|mark| match mark {
                                    Mark::Link(url) => Some(url.clone()),
                                    _ => None,
                                })
                            })
                            .flatten()
                    })
            })
            .unwrap_or_default()
    }

    fn open_link_popover_at(
        &mut self,
        anchor: Option<Point<Pixels>>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.link_popover.is_some() {
            return;
        }
        let initial = self.selected_link_url(cx);
        let popover = cx.new(|cx| {
            let popover = LinkPopover::new(initial, cx);
            match anchor {
                Some(anchor) => popover.anchored_at(anchor),
                None => popover,
            }
        });
        popover.update(cx, |popover, _| popover.focus.focus(window));
        self.link_popover = Some(popover);
        cx.notify();
    }

    pub fn open_link_popover(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.open_link_popover_at(None, window, cx);
    }

    pub fn cancel_link(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.link_popover = None;
        focus_editor(&self.editor, window, cx);
        cx.notify();
    }

    fn submit_link(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(popover) = self.link_popover.clone() else {
            return;
        };
        let url = popover.read(cx).text.trim().to_owned();
        let result = self.editor.update(cx, |editor, editor_cx| {
            let result =
                self.catalogue
                    .execute(EditorCommand::Link, CommandArgument::LinkUrl(url), editor);
            editor_cx.notify();
            result
        });
        if result.is_err() {
            popover.update(cx, |popover, popover_cx| {
                popover.invalid = true;
                popover_cx.notify();
            });
            cx.notify();
            return;
        }
        self.link_popover = None;
        focus_editor(&self.editor, window, cx);
        cx.notify();
    }

    fn on_link_key_down(
        &mut self,
        event: &KeyDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let key = event.keystroke.key.as_str();
        let modifiers = event.keystroke.modifiers;
        let secondary = modifiers.secondary();
        let handled = match key {
            "enter" => {
                self.submit_link(window, cx);
                true
            }
            "escape" => {
                self.cancel_link(window, cx);
                true
            }
            "backspace" => {
                if let Some(popover) = self.link_popover.clone() {
                    popover.update(cx, |popover, _| popover.delete_backward());
                }
                cx.notify();
                true
            }
            "delete" => {
                if let Some(popover) = self.link_popover.clone() {
                    popover.update(cx, |popover, _| popover.delete_forward());
                }
                cx.notify();
                true
            }
            "left" => {
                if let Some(popover) = self.link_popover.clone() {
                    popover.update(cx, |popover, _| {
                        popover.move_horizontal(false, modifiers.shift)
                    });
                }
                cx.notify();
                true
            }
            "right" => {
                if let Some(popover) = self.link_popover.clone() {
                    popover.update(cx, |popover, _| {
                        popover.move_horizontal(true, modifiers.shift)
                    });
                }
                cx.notify();
                true
            }
            "home" => {
                if let Some(popover) = self.link_popover.clone() {
                    popover.update(cx, |popover, _| {
                        popover.move_to_edge(false, modifiers.shift)
                    });
                }
                cx.notify();
                true
            }
            "end" => {
                if let Some(popover) = self.link_popover.clone() {
                    popover.update(cx, |popover, _| popover.move_to_edge(true, modifiers.shift));
                }
                cx.notify();
                true
            }
            "a" if secondary => {
                if let Some(popover) = self.link_popover.clone() {
                    popover.update(cx, |popover, _| popover.select_all());
                }
                cx.notify();
                true
            }
            "c" if secondary => {
                let text = self
                    .link_popover
                    .as_ref()
                    .map(|popover| popover.read(cx).selected_text())
                    .unwrap_or_default();
                if !text.is_empty() {
                    cx.write_to_clipboard(ClipboardItem::new_string(text));
                }
                true
            }
            "x" if secondary => {
                let text = self
                    .link_popover
                    .as_ref()
                    .map(|popover| popover.read(cx).selected_text())
                    .unwrap_or_default();
                if !text.is_empty() {
                    cx.write_to_clipboard(ClipboardItem::new_string(text));
                    if let Some(popover) = self.link_popover.clone() {
                        popover.update(cx, |popover, _| popover.delete_backward());
                    }
                    cx.notify();
                }
                true
            }
            "v" if secondary => {
                if let Some(popover) = self.link_popover.clone() {
                    popover.update(cx, |popover, popover_cx| {
                        popover.paste_from_clipboard(popover_cx)
                    });
                }
                true
            }
            _ => false,
        };
        if handled {
            cx.stop_propagation();
        }
    }

    fn execute_command(
        &mut self,
        command: EditorCommand,
        from_more: bool,
        anchor: Option<Point<Pixels>>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if command == EditorCommand::InsertImage {
            if from_more {
                self.more_open = false;
            }
            // The parent owns picker/staged resource policy; this typed event
            // is deliberately emitted before returning focus to the editor.
            let window_handle = window.window_handle();
            cx.emit(EditorCommandChromeEvent::RequestInsertImage {
                window: window_handle,
            });
            (self.insert_image_dispatch)(window_handle, cx);
            focus_editor(&self.editor, window, cx);
            cx.notify();
            return;
        }
        if command == EditorCommand::Link {
            if from_more {
                self.more_open = false;
            }
            self.open_link_popover_at(anchor, window, cx);
            return;
        }
        let _ = self.editor.update(cx, |editor, editor_cx| {
            let result = self
                .catalogue
                .execute(command, CommandArgument::None, editor);
            editor_cx.notify();
            result
        });
        if from_more {
            self.more_open = false;
        }
        focus_editor(&self.editor, window, cx);
        cx.notify();
    }

    fn render_link_popover(
        &self,
        popover: Entity<LinkPopover>,
        content_mask: Bounds<Pixels>,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        let focus = popover.read(cx).focus.clone();
        let invalid = popover.read(cx).invalid;
        let anchor = popover.read(cx).anchor;
        let mask_left = f32::from(content_mask.left());
        let mask_right = f32::from(content_mask.right());
        let mask_top = f32::from(content_mask.top());
        let mask_bottom = f32::from(content_mask.bottom());
        let width = 360.0_f32.min((mask_right - mask_left).max(1.0));
        let natural_height: f32 = if invalid { 134.0 } else { 118.0 };
        let panel_height = natural_height.min((mask_bottom - mask_top).max(1.0));
        let anchor = anchor.unwrap_or_else(|| point(content_mask.left(), content_mask.top()));
        let left = f32::from(anchor.x)
            .max(mask_left)
            .min((mask_right - width).max(mask_left));
        let below = f32::from(anchor.y) + 16.0;
        let preferred_top = if below + natural_height <= mask_bottom {
            below
        } else {
            f32::from(anchor.y) - natural_height - 16.0
        };
        let top = preferred_top
            .max(mask_top)
            .min((mask_bottom - panel_height).max(mask_top));
        let canvas_popover = popover.clone();
        let paint_popover = popover.clone();
        let click_popover = popover.clone();
        let chrome = cx.entity();
        let input_canvas = canvas(
            move |bounds, _window, cx| {
                let _ = canvas_popover.update(cx, |popover, _| {
                    popover.last_bounds = Some(bounds);
                });
                canvas_popover.clone()
            },
            move |bounds, entity, window, cx| {
                let (text, selection) = entity.read_with(cx, |popover, _| {
                    (
                        SharedString::from(popover.text.clone()),
                        popover.selection.clone(),
                    )
                });
                let focused = entity.read(cx).focus.is_focused(window);
                let style = window.text_style();
                let line = window.text_system().shape_line(
                    text.clone(),
                    style.font_size.to_pixels(window.rem_size()),
                    &[TextRun {
                        len: text.len(),
                        font: style.font(),
                        color: rgba(0x172033ff).into(),
                        background_color: None,
                        underline: None,
                        strikethrough: None,
                    }],
                    None,
                );
                entity.update(cx, |popover, _| {
                    popover.last_layout = Some(line.clone());
                });
                if focused && !selection.is_empty() {
                    let start = line.x_for_index(selection.start);
                    let end = line.x_for_index(selection.end);
                    window.paint_quad(gpui::fill(
                        Bounds::from_corners(
                            Point::new(bounds.left() + start, bounds.top()),
                            Point::new(bounds.left() + end, bounds.bottom()),
                        ),
                        rgba(0x4f8cff55),
                    ));
                }
                line.paint(bounds.origin, bounds.size.height, window, cx)
                    .ok();
                if focused && selection.is_empty() {
                    let x = line.x_for_index(selection.start);
                    window.paint_quad(gpui::fill(
                        Bounds::new(
                            Point::new(bounds.left() + x, bounds.top()),
                            size(px(1.0), bounds.size.height),
                        ),
                        rgba(0x2167dfff),
                    ));
                }
                let focus = entity.read(cx).focus.clone();
                if focus.is_focused(window) {
                    window.handle_input(
                        &focus,
                        ElementInputHandler::new(bounds, paint_popover.clone()),
                        cx,
                    );
                }
            },
        )
        .w_full()
        .h(px(40.0));
        div()
            .id("evernote-link-popover")
            .debug_selector(|| "evernote-link-popover".to_owned())
            .absolute()
            .top(px(top))
            .left(px(left))
            .w(px(width))
            .h(px(panel_height))
            .flex()
            .flex_col()
            .overflow_y_scroll()
            // See the More menu below: the Link panel is likewise a
            // window-layer interaction and must shield the library canvas's
            // capture listener while its input/button controls own focus.
            .occlude()
            .p(px(8.0))
            .rounded(px(6.0))
            .bg(rgba(0xffffffff))
            .border(px(1.0))
            .border_color(if invalid {
                rgba(0xd92d20ff)
            } else {
                rgba(0xc7d0ddff)
            })
            .key_context("EvernoteLinkPopover")
            .track_focus(&focus)
            .on_mouse_down(MouseButton::Left, move |_event, _window, cx| {
                cx.stop_propagation()
            })
            .on_key_down(cx.listener(Self::on_link_key_down))
            .on_action(cx.listener(|chrome, _action: &SubmitLink, window, cx| {
                chrome.submit_link(window, cx)
            }))
            .on_action(cx.listener(|chrome, _action: &CancelLink, window, cx| {
                chrome.cancel_link(window, cx)
            }))
            .child(
                div()
                    .text_size(px(12.0))
                    .text_color(rgba(0x475467ff))
                    .child("链接地址"),
            )
            .child(
                div()
                    .h(px(40.0))
                    .on_mouse_down(MouseButton::Left, move |event, window, cx| {
                        let handled = click_popover.update(cx, |popover, popover_cx| {
                            if let Some(index) = popover.character_index_for_point(
                                event.position,
                                window,
                                popover_cx,
                            ) {
                                let index = crate::native_editor::input::utf16_range_to_utf8_in(
                                    &popover.text,
                                    &(index..index),
                                )
                                .start;
                                popover.collapse(index);
                                popover.focus.focus(window);
                                popover_cx.notify();
                                true
                            } else {
                                false
                            }
                        });
                        if handled {
                            cx.stop_propagation();
                        } else {
                            cx.propagate();
                        }
                    })
                    .child(input_canvas),
            )
            .children(invalid.then(|| {
                div()
                    .id("evernote-link-invalid-error")
                    .debug_selector(|| "evernote-link-invalid-error".to_owned())
                    .pt(px(4.0))
                    .text_size(px(12.0))
                    .text_color(rgba(0xd92d20ff))
                    .child("请输入有效 URL")
            }))
            .child({
                div()
                    .pt(px(8.0))
                    .w_full()
                    .flex()
                    .justify_end()
                    .gap(px(8.0))
                    .child({
                        let chrome = chrome.clone();
                        div()
                            .id("evernote-link-cancel")
                            .debug_selector(|| "evernote-link-cancel".to_owned())
                            .w(px(64.0))
                            .h(px(32.0))
                            .flex()
                            .items_center()
                            .justify_center()
                            .rounded(px(5.0))
                            .cursor_pointer()
                            .hover(|this| this.bg(rgba(0xeff2f6ff)))
                            .on_mouse_down(MouseButton::Left, move |_event, window, cx| {
                                cx.stop_propagation();
                                let _ = chrome.update(cx, |chrome, chrome_cx| {
                                    chrome.cancel_link(window, chrome_cx)
                                });
                            })
                            .child("取消")
                    })
                    .child({
                        let chrome = chrome.clone();
                        div()
                            .id("evernote-link-apply")
                            .debug_selector(|| "evernote-link-apply".to_owned())
                            .w(px(64.0))
                            .h(px(32.0))
                            .flex()
                            .items_center()
                            .justify_center()
                            .rounded(px(5.0))
                            .bg(rgba(EVERNOTE_GREEN))
                            .text_color(rgba(0xffffffff))
                            .cursor_pointer()
                            .hover(|this| this.bg(rgba(0x008f26ff)))
                            .on_mouse_down(MouseButton::Left, move |_event, window, cx| {
                                cx.stop_propagation();
                                let _ = chrome.update(cx, |chrome, chrome_cx| {
                                    chrome.submit_link(window, chrome_cx)
                                });
                            })
                            .child("应用")
                    })
            })
            .into_any_element()
    }

    fn render_command_button(
        &self,
        descriptor: &'static CommandDescriptor,
        from_more: bool,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        let state = self
            .catalogue
            .state(descriptor.command, self.editor.read(cx));
        let command = descriptor.command;
        let chrome = cx.entity();
        let background = match state.toggle {
            ToggleState::On | ToggleState::Mixed => rgba(EVERNOTE_GREEN),
            ToggleState::Off => rgba(0x00000000),
        };
        let foreground = if state.enabled {
            rgba(0x172033ff)
        } else {
            rgba(0x68738699)
        };
        let icon = match descriptor.icon_path {
            Some(path) => svg()
                .path(path)
                .size(px(20.0))
                .text_color(foreground)
                .into_any_element(),
            None if from_more => div()
                .w(px(20.0))
                .text_size(px(13.0))
                .child("T")
                .into_any_element(),
            None => div()
                .text_size(px(13.0))
                .child(descriptor.label_zh)
                .into_any_element(),
        };
        let mut button = if from_more {
            div()
                .id(descriptor.label)
                .debug_selector(|| descriptor.label.to_owned())
                .w_full()
                .h(px(36.0))
                .flex_none()
                .px(px(8.0))
                .flex()
                .items_center()
                .gap(px(8.0))
                .rounded(px(5.0))
                .bg(background)
                .hover(move |this| {
                    this.bg(
                        if matches!(state.toggle, ToggleState::On | ToggleState::Mixed) {
                            rgba(0x008f26ff)
                        } else {
                            rgba(0x17203312)
                        },
                    )
                })
                .text_size(px(13.0))
                .text_color(foreground)
                .opacity(if state.enabled { 1.0 } else { 0.35 })
                .child(icon)
                .child(
                    div()
                        .debug_selector(|| descriptor.label_zh.to_owned())
                        .child(descriptor.label_zh),
                )
        } else {
            div()
                .id(descriptor.label)
                .debug_selector(|| descriptor.label.to_owned())
                .flex_shrink_0()
                .size(px(32.0))
                .flex()
                .items_center()
                .justify_center()
                .mr(px(4.0))
                .rounded(px(5.0))
                .bg(background)
                .hover(move |this| {
                    this.bg(
                        if matches!(state.toggle, ToggleState::On | ToggleState::Mixed) {
                            rgba(0x008f26ff)
                        } else {
                            rgba(0x17203312)
                        },
                    )
                })
                .text_size(px(12.0))
                .text_color(foreground)
                .opacity(if state.enabled { 1.0 } else { 0.35 })
                .child(icon)
        };
        if state.enabled {
            button = button.cursor_pointer().on_mouse_down(
                MouseButton::Left,
                move |event: &MouseDownEvent, window: &mut Window, cx: &mut App| {
                    cx.stop_propagation();
                    let _ = chrome.update(cx, |chrome, chrome_cx| {
                        chrome.execute_command(
                            command,
                            from_more,
                            Some(event.position),
                            window,
                            chrome_cx,
                        )
                    });
                },
            );
        }
        button.into_any_element()
    }

    fn render_more_trigger(&self, cx: &mut Context<Self>) -> gpui::AnyElement {
        let chrome = cx.entity();
        let id = self.host.more_trigger_id();
        div()
            .id(id)
            .debug_selector(move || id.to_owned())
            .flex_shrink_0()
            .h(px(32.0))
            .px(px(10.0))
            .mr(px(4.0))
            .rounded(px(5.0))
            .bg(if self.more_open {
                rgba(EVERNOTE_GREEN)
            } else {
                rgba(0x00000010)
            })
            .text_size(px(12.0))
            .text_color(rgba(0x172033ff))
            .cursor_pointer()
            .hover(|this| this.bg(rgba(0x17203318)))
            .on_mouse_down(MouseButton::Left, move |_event, window, cx| {
                cx.stop_propagation();
                let _ = chrome.update(cx, |chrome, chrome_cx| {
                    chrome.more_open = !chrome.more_open;
                    focus_editor(&chrome.editor, window, chrome_cx);
                    chrome_cx.notify();
                });
            })
            .child(if self.more_open {
                "更多 ▴"
            } else {
                "更多 ▾"
            })
            .into_any_element()
    }

    fn render_more_menu(
        &self,
        trigger_bounds: Option<Bounds<Pixels>>,
        content_mask: Bounds<Pixels>,
        placement: &ToolbarPlacement,
        cx: &mut Context<Self>,
    ) -> Option<gpui::AnyElement> {
        if !self.more_open {
            return None;
        }
        let trigger = trigger_bounds.unwrap_or_else(|| {
            Bounds::new(
                point(content_mask.left(), content_mask.top()),
                size(px(52.0), px(32.0)),
            )
        });
        let mask_left = f32::from(content_mask.left());
        let mask_right = f32::from(content_mask.right());
        let mask_top = f32::from(content_mask.top());
        let mask_bottom = f32::from(content_mask.bottom());
        let mask_width = (mask_right - mask_left).max(1.0);
        let menu_width = 220.0_f32.min(mask_width);
        let left = if mask_width >= 220.0 {
            f32::from(trigger.left())
                .max(mask_left)
                .min(mask_right - 220.0)
        } else {
            mask_left
        };
        let below_height = (mask_bottom - f32::from(trigger.bottom())).max(0.0);
        let above_height = (f32::from(trigger.top()) - mask_top).max(0.0);
        let (top, max_height) = if below_height >= above_height {
            (f32::from(trigger.bottom()), below_height)
        } else {
            (mask_top, above_height)
        };
        let mut commands = placement.overflow.clone();
        commands.extend(
            self.catalogue
                .more_descriptors()
                .into_iter()
                .map(|descriptor| descriptor.command),
        );
        let buttons = commands
            .into_iter()
            .filter_map(|command| {
                self.catalogue
                    .descriptors()
                    .iter()
                    .find(|descriptor| descriptor.command == command)
            })
            .map(|descriptor| self.render_command_button(descriptor, true, cx))
            .collect::<Vec<_>>();
        let id = self.host.more_menu_id();
        Some(
            div()
                .id(id)
                .debug_selector(move || id.to_owned())
                .absolute()
                .top(px(top.max(mask_top)))
                .left(px(left))
                .w(px(menu_width))
                .max_h(px(300.0_f32.min(max_height.max(1.0))))
                .overflow_y_scroll()
                // The ordinary library surface owns a capture-phase pointer
                // handler. This window-layer menu must occlude it so a More
                // row cannot move the document caret underneath the command
                // that was clicked. (The spike also has a parent gate, but
                // both hosts deliberately share this overlay.)
                .occlude()
                .p(px(8.0))
                .rounded(px(6.0))
                .bg(rgba(0xffffffff))
                .border(px(1.0))
                .border_color(rgba(0xc7d0ddff))
                .flex()
                .flex_col()
                .children(buttons)
                .into_any_element(),
        )
    }

    /// Build a shared command row and global-window overlays.  Hosts only
    /// choose where to place the returned slots; they never manufacture a
    /// second command/button handler.
    pub fn render_for_host(
        &mut self,
        width: f32,
        content_mask: Bounds<Pixels>,
        cx: &mut Context<Self>,
    ) -> EditorCommandChromeRender {
        let placement = toolbar_placement(width);
        let mut primary_buttons = Vec::new();
        let mut previous_group = None;
        for command in &placement.primary {
            let group = toolbar_group(*command);
            if previous_group.is_some_and(|previous| previous != group) {
                primary_buttons.push(
                    div()
                        .w(px(1.0))
                        .h(px(18.0))
                        .mx(px(4.0))
                        .bg(rgba(0xd0d5ddff))
                        .into_any_element(),
                );
            }
            if let Some(descriptor) = self
                .catalogue
                .descriptors()
                .iter()
                .find(|descriptor| descriptor.command == *command)
            {
                primary_buttons.push(self.render_command_button(descriptor, false, cx));
            }
            previous_group = Some(group);
        }
        let more_trigger = self.render_more_trigger(cx);
        let chrome = cx.entity();
        let more_measure = canvas(
            move |bounds, _window, cx| {
                let _ = chrome.update(cx, |chrome, chrome_cx| {
                    if chrome.more_trigger_bounds != Some(bounds) {
                        chrome.more_trigger_bounds = Some(bounds);
                        chrome_cx.notify();
                    }
                });
                chrome.clone()
            },
            move |_bounds, _view, _window, _cx| {},
        )
        .absolute()
        .top(px(0.0))
        .left(px(0.0))
        .w_full()
        .h_full();
        let more_trigger = div()
            .relative()
            .h(px(32.0))
            .child(more_measure)
            .child(more_trigger)
            .into_any_element();
        let toolbar_id = self.host.toolbar_id();
        let root_id = self.host.root_id();
        let toolbar = div()
            .id(root_id)
            .debug_selector(move || root_id.to_owned())
            .relative()
            .w(px(width.max(1.0)))
            .flex()
            .flex_none()
            .h(px(44.0))
            .items_center()
            .child(
                div()
                    .id(toolbar_id)
                    .debug_selector(move || toolbar_id.to_owned())
                    .w_full()
                    .h_full()
                    .flex()
                    .items_center()
                    .children(primary_buttons)
                    .child(more_trigger),
            )
            .into_any_element();
        let mut overlays = Vec::new();
        if self.has_open_overlay() {
            // `EditorSurface` owns a capture-phase pointer listener. A normal
            // transparent hitbox is too late to protect the document: the
            // surface can collapse its selection before a menu/popover gets
            // its outside-click callback. This window-layer occluder is put
            // *before* the visible menu/panel, so those controls remain above
            // it while every other pointer target deterministically dismisses
            // the one shared overlay and restores editor focus.
            let chrome = cx.entity();
            let backdrop_id = self.host.overlay_backdrop_id();
            overlays.push(
                div()
                    .id(backdrop_id)
                    .debug_selector(move || backdrop_id.to_owned())
                    .absolute()
                    .top(px(0.0))
                    .left(px(0.0))
                    .size_full()
                    .occlude()
                    .on_mouse_down(MouseButton::Left, move |_event, window, cx| {
                        cx.stop_propagation();
                        let _ = chrome.update(cx, |chrome, chrome_cx| {
                            chrome.dismiss_overlay(window, chrome_cx)
                        });
                    })
                    .into_any_element(),
            );
        }
        if let Some(menu) =
            self.render_more_menu(self.more_trigger_bounds, content_mask, &placement, cx)
        {
            overlays.push(menu);
        }
        if let Some(popover) = self.link_popover.clone() {
            overlays.push(self.render_link_popover(popover, content_mask, cx));
        }
        EditorCommandChromeRender { toolbar, overlays }
    }
}

fn toolbar_group(command: EditorCommand) -> u8 {
    match command {
        EditorCommand::InsertImage => 0,
        EditorCommand::Undo | EditorCommand::Redo => 1,
        EditorCommand::Paragraph => 2,
        EditorCommand::Bold
        | EditorCommand::Italic
        | EditorCommand::Underline
        | EditorCommand::Highlight => 3,
        EditorCommand::BulletList | EditorCommand::OrderedList | EditorCommand::CheckList => 4,
        EditorCommand::Link => 5,
        EditorCommand::AlignLeft | EditorCommand::AlignCenter | EditorCommand::AlignRight => 6,
        _ => 7,
    }
}
