//! Isolated native GPUI route for the Evernote-style editor spike.
//!
//! This module deliberately stops at the native editor entity. It does not
//! construct the ordinary workspace, updater, exporter, network client, or
//! sync services. The action names and default key bindings still come from
//! `components::actions` (the pinned Velotype donor path), while selection,
//! focus, transactions, and platform input remain owned by `EditorCore`.

use std::ops::Range;
use std::path::PathBuf;

use gpui::{
    App, AppContext, Bounds, ClipboardItem, Context, ElementInputHandler, Entity,
    EntityInputHandler, ExternalPaths, FocusHandle, InteractiveElement, IntoElement, KeyDownEvent,
    MouseButton, MouseDownEvent, MouseMoveEvent, MouseUpEvent, ParentElement, Pixels, Point,
    Render, ScrollHandle, ShapedLine, SharedString, StatefulInteractiveElement, Styled, TextRun,
    UTF16Selection, Window, WindowBounds, WindowHandle, WindowOptions, canvas, div, point, px,
    rgba, size,
};
use unicode_segmentation::UnicodeSegmentation;

use crate::components::{
    BlockDown, BlockUp, BoldSelection, Copy, Cut, Delete, DeleteBack, End, FocusNext, FocusPrev,
    Home, IndentBlock, ItalicSelection, MoveLeft, MoveRight, Newline, OutdentBlock, Paste, Redo,
    SelectAll, SelectEnd, SelectHome, SelectLeft, SelectRight, UnderlineSelection, Undo,
    WordSelectLeft, WordSelectRight,
};
use crate::native_editor::commands::{
    CommandArgument, CommandCatalogue, CommandDescriptor, EditorCommand,
};
use crate::native_editor::core::EditorCore;
use crate::native_editor::images::{
    BudgetedImageCache, ClipboardPayload, DECODED_IMAGE_CACHE_BUDGET, PasteIntent,
    classify_clipboard, classify_drop, read_native_pasteboard, resolve_clipboard_payload,
};
use crate::native_editor::model::{
    Affinity, BlockKind, DocPoint, Document, DocumentError, Mark, Selection,
};
use crate::native_editor::transaction::Transaction;

gpui::actions!(evernote_spike, [SubmitLink, CancelLink]);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SpikeRouteContract {
    pub window_count: u8,
    pub uses_native_editor_entity: bool,
    pub uses_real_input_bridge: bool,
    pub initializes_donor_services: bool,
    pub initializes_web_runtime: bool,
}

/// A stable contract for the release-spike route. Keeping this pure makes the
/// no-service boundary testable without booting a platform window.
pub const fn route_contract() -> SpikeRouteContract {
    SpikeRouteContract {
        window_count: 1,
        uses_native_editor_entity: true,
        uses_real_input_bridge: true,
        initializes_donor_services: false,
        initializes_web_runtime: false,
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SpikeLayout {
    pub content_width: f32,
    pub left_inset: f32,
    pub right_inset: f32,
    pub bottom_padding: f32,
}

pub fn layout_for_viewport(width: f32, height: f32) -> SpikeLayout {
    let content_width = (width - 64.0).max(1.0).min(680.0);
    let horizontal_space = (width - content_width).max(0.0);
    SpikeLayout {
        content_width,
        left_inset: horizontal_space / 2.0,
        right_inset: horizontal_space / 2.0,
        bottom_padding: height.max(0.0) * 0.30,
    }
}

fn surface_viewport(surface: Bounds<Pixels>, content_mask: Bounds<Pixels>) -> (f32, f32) {
    let top = surface.top().max(content_mask.top());
    let bottom = surface.bottom().min(content_mask.bottom());
    (
        f32::from((top - surface.top()).max(px(0.0))),
        f32::from((bottom - top).max(px(1.0))),
    )
}

/// Open exactly one native GPUI window for the spike. The caller is expected
/// to have installed only `components::init`, not the ordinary app services.
pub(crate) fn open(cx: &mut App) -> WindowHandle<SpikeView> {
    let bounds = Bounds::centered(None, size(px(1080.0), px(720.0)), cx);
    let handle = cx
        .open_window(
            WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(bounds)),
                ..WindowOptions::default()
            },
            move |_window, cx| {
                let editor = cx.new(|cx| EditorCore::new(sample_document(), cx));
                let image_cache = BudgetedImageCache::new_entity(cx, DECODED_IMAGE_CACHE_BUDGET);
                cx.new(|_| SpikeView {
                    editor,
                    image_cache: Some(image_cache),
                    catalogue: CommandCatalogue::default(),
                    scroll_handle: ScrollHandle::new(),
                    more_open: false,
                    link_popover: None,
                    pointer_anchor: None,
                    more_trigger_bounds: None,
                })
            },
        )
        .expect("native Evernote spike window should open");

    handle
        .update(cx, |view, window, cx| {
            window.activate_window();
            focus_editor(&view.editor, window, cx);
        })
        .expect("native Evernote spike window should be updateable");
    handle
}

/// Small real text-input owner for the link popover.  The editor selection
/// never moves into this entity; only the URL field owns focus while the
/// popover is open.  This follows the pinned donor's `ElementInputHandler`
/// bridge so AppKit/IME replacement edits the field rather than a fake label.
struct LinkPopover {
    focus: FocusHandle,
    text: String,
    selection: Range<usize>,
    reversed: bool,
    marked: Option<Range<usize>>,
    last_bounds: Option<Bounds<Pixels>>,
    last_layout: Option<ShapedLine>,
}

impl LinkPopover {
    fn new(initial: String, cx: &mut Context<Self>) -> Self {
        let end = initial.len();
        Self {
            focus: cx.focus_handle(),
            text: initial,
            selection: end..end,
            reversed: false,
            marked: None,
            last_bounds: None,
            last_layout: None,
        }
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

    fn move_horizontal(&mut self, right: bool, extend: bool) {
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

    fn move_to_edge(&mut self, end: bool, extend: bool) {
        let head = self.cursor_offset();
        let target = if end { self.text.len() } else { 0 };
        if extend {
            self.set_selection(self.anchor_offset(), target);
        } else if head != target {
            self.collapse(target);
        }
    }

    fn delete_backward(&mut self) {
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

    fn selected_text(&self) -> String {
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

pub(crate) struct SpikeView {
    editor: Entity<EditorCore>,
    image_cache: Option<Entity<BudgetedImageCache>>,
    catalogue: CommandCatalogue,
    scroll_handle: ScrollHandle,
    more_open: bool,
    link_popover: Option<Entity<LinkPopover>>,
    pointer_anchor: Option<DocPoint>,
    more_trigger_bounds: Option<Bounds<Pixels>>,
}

impl SpikeView {
    fn on_external_paths_drop(
        &mut self,
        paths: &ExternalPaths,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let paths = paths.paths().to_vec();
        run_editor_result(&self.editor, window, cx, |editor| {
            apply_drop_paths(editor, &paths)
        });
        focus_editor(&self.editor, window, cx);
    }

    fn open_link_popover(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.link_popover.is_some() {
            return;
        }
        let initial = self
            .editor
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
            .unwrap_or_default();
        let popover = cx.new(|cx| LinkPopover::new(initial, cx));
        popover.update(cx, |popover, _cx| popover.focus.focus(window));
        self.link_popover = Some(popover);
        cx.notify();
    }

    fn cancel_link(&mut self, _action: &CancelLink, window: &mut Window, cx: &mut Context<Self>) {
        self.link_popover = None;
        focus_editor(&self.editor, window, cx);
        cx.notify();
    }

    fn submit_link(&mut self, _action: &SubmitLink, window: &mut Window, cx: &mut Context<Self>) {
        let Some(popover) = self.link_popover.take() else {
            return;
        };
        let url = popover.read(cx).text.trim().to_owned();
        if !url.is_empty() {
            let _ = self.editor.update(cx, |editor, editor_cx| {
                let result = self.catalogue.execute(
                    EditorCommand::Link,
                    CommandArgument::LinkUrl(url),
                    editor,
                );
                editor_cx.notify();
                if let Err(error) = result {
                    eprintln!("spike link command failed: {error}");
                }
            });
        }
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
                self.submit_link(&SubmitLink, window, cx);
                true
            }
            "escape" => {
                self.cancel_link(&CancelLink, window, cx);
                true
            }
            "backspace" if secondary => {
                if let Some(popover) = self.link_popover.clone() {
                    popover.update(cx, |popover, _| popover.delete_backward());
                }
                cx.notify();
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

    fn render_link_popover(
        &self,
        popover: Entity<LinkPopover>,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        let focus = popover.read(cx).focus.clone();
        let canvas_popover = popover.clone();
        let paint_popover = popover.clone();
        let click_popover = popover.clone();
        let input_canvas = canvas(
            move |bounds, _window, cx| {
                let _ = canvas_popover.update(cx, |popover, _cx| {
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
        .h(px(28.0));
        div()
            .id("evernote-link-popover")
            .absolute()
            .top(px(94.0))
            .left(px(12.0))
            .w(px(360.0))
            .p(px(8.0))
            .rounded(px(6.0))
            .bg(rgba(0xffffffff))
            .border(px(1.0))
            .border_color(rgba(0xc7d0ddff))
            .key_context("EvernoteLinkPopover")
            .track_focus(&focus)
            .on_mouse_down(MouseButton::Left, move |event, window, cx| {
                let _ = click_popover.update(cx, |popover, popover_cx| {
                    if let Some(index) =
                        popover.character_index_for_point(event.position, window, popover_cx)
                    {
                        let index = crate::native_editor::input::utf16_range_to_utf8_in(
                            &popover.text,
                            &(index..index),
                        )
                        .start;
                        popover.collapse(index);
                        popover.focus.focus(window);
                        popover_cx.notify();
                    }
                });
                cx.stop_propagation();
            })
            .on_key_down(cx.listener(Self::on_link_key_down))
            .on_action(cx.listener(Self::submit_link))
            .on_action(cx.listener(Self::cancel_link))
            .child(input_canvas)
            .into_any_element()
    }

    fn render_command_button(
        &self,
        descriptor: &'static CommandDescriptor,
        from_more: bool,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        let editor_ref = self.editor.read(cx);
        let state = self.catalogue.state(descriptor.command, editor_ref);
        let command = descriptor.command;
        let editor = self.editor.clone();
        let catalogue = self.catalogue;
        let view = cx.entity();
        let background = match state.toggle {
            crate::native_editor::commands::ToggleState::On => rgba(0x2f6feb44),
            crate::native_editor::commands::ToggleState::Mixed => rgba(0xf2b84b55),
            crate::native_editor::commands::ToggleState::Off => rgba(0x00000010),
        };
        let foreground = if state.enabled {
            rgba(0x172033ff)
        } else {
            rgba(0x68738699)
        };
        let mut button = div()
            .id(descriptor.label)
            .debug_selector(|| descriptor.label.to_owned())
            .flex_shrink_0()
            .px(px(9.0))
            .py(px(6.0))
            .mr(px(5.0))
            .mb(px(5.0))
            .rounded(px(5.0))
            .bg(background)
            .text_size(px(12.0))
            .text_color(foreground)
            .child(descriptor.label);

        if state.enabled {
            button = button
                .cursor_pointer()
                // A div is deliberately used instead of a focusable button:
                // pointer-down preserves the editor's selection and focus just
                // like the donor block toolbar.
                .on_mouse_down(
                    MouseButton::Left,
                    move |_event: &MouseDownEvent, window: &mut Window, cx: &mut App| {
                        cx.stop_propagation();
                        if command == EditorCommand::Link {
                            let _ = view.update(cx, |view, view_cx| {
                                view.open_link_popover(window, view_cx)
                            });
                        } else {
                            run_command(
                                &editor,
                                catalogue,
                                command,
                                CommandArgument::None,
                                window,
                                cx,
                            );
                            if from_more {
                                let _ = view.update(cx, |view, view_cx| {
                                    view.more_open = false;
                                    view_cx.notify();
                                });
                            }
                        }
                    },
                );
        }
        button.into_any_element()
    }

    fn render_more_trigger(&self, cx: &mut Context<Self>) -> gpui::AnyElement {
        let view = cx.entity();
        div()
            .id("evernote-native-spike-more-trigger")
            .debug_selector(|| "evernote-native-spike-more-trigger".to_owned())
            .flex_shrink_0()
            .px(px(9.0))
            .py(px(6.0))
            .mr(px(5.0))
            .mb(px(5.0))
            .rounded(px(5.0))
            .bg(if self.more_open {
                rgba(0x2f6feb44)
            } else {
                rgba(0x00000010)
            })
            .text_size(px(12.0))
            .text_color(rgba(0x172033ff))
            .cursor_pointer()
            .on_mouse_down(MouseButton::Left, move |_event, window, cx| {
                cx.stop_propagation();
                let _ = view.update(cx, |view, view_cx| {
                    view.more_open = !view.more_open;
                    focus_editor(&view.editor, window, view_cx);
                    view_cx.notify();
                });
            })
            .child(if self.more_open {
                "More ▴"
            } else {
                "More ▾"
            })
            .into_any_element()
    }

    fn render_more_menu(
        &self,
        trigger_bounds: Option<Bounds<Pixels>>,
        content_mask: Bounds<Pixels>,
        cx: &mut Context<Self>,
    ) -> Option<gpui::AnyElement> {
        let trigger = trigger_bounds?;
        if !self.more_open {
            return None;
        }
        let mask_left = f32::from(content_mask.left());
        let mask_right = f32::from(content_mask.right());
        let mask_top = f32::from(content_mask.top());
        let mask_bottom = f32::from(content_mask.bottom());
        let trigger_left = f32::from(trigger.left()).max(mask_left);
        let available_width = (mask_right - trigger_left)
            .min(f32::from(content_mask.size.width))
            .max(1.0);
        let below_height = (mask_bottom - f32::from(trigger.bottom())).max(0.0);
        let above_height = (f32::from(trigger.top()) - mask_top).max(0.0);
        let (top, max_height) = if below_height >= above_height {
            (f32::from(trigger.bottom()), below_height)
        } else {
            (mask_top, above_height)
        };
        let buttons = self
            .catalogue
            .more_descriptors()
            .into_iter()
            .map(|descriptor| self.render_command_button(descriptor, true, cx))
            .collect::<Vec<_>>();
        Some(
            div()
                .id("evernote-native-spike-more-menu")
                .debug_selector(|| "evernote-native-spike-more-menu".to_owned())
                .absolute()
                .top(px(top.max(mask_top)))
                .left(px(trigger_left))
                .w(px(available_width.max(1.0)))
                .max_h(px(max_height.max(1.0)))
                .overflow_y_scroll()
                .p(px(8.0))
                .rounded(px(6.0))
                .bg(rgba(0xffffffff))
                .border(px(1.0))
                .border_color(rgba(0xc7d0ddff))
                .flex()
                .flex_wrap()
                .children(buttons)
                .into_any_element(),
        )
    }

    fn on_surface_mouse_down(
        &mut self,
        event: &MouseDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if event.button != MouseButton::Left {
            cx.propagate();
            return;
        }
        // The popup is rendered above the editor surface. Its command
        // buttons must get first refusal even though the surface owns a
        // capture-phase mouse listener; an editor click while the popup is
        // open is dismissed by the root listener instead of changing the
        // selection underneath the menu.
        if self.more_open {
            cx.propagate();
            return;
        }
        let extend = event.modifiers.shift;
        self.pointer_anchor = self.editor.update(cx, |editor, editor_cx| {
            let anchor = editor.begin_pointer_selection(event.position, extend);
            editor_cx.notify();
            anchor
        });
        if self.more_open {
            self.more_open = false;
        }
        if self.link_popover.is_some() {
            self.link_popover = None;
        }
        focus_editor(&self.editor, window, cx);
        cx.stop_propagation();
    }

    fn on_surface_key_down(
        &mut self,
        event: &KeyDownEvent,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
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
        let editor = self.editor.clone();
        let _ = editor.update(cx, |editor, editor_cx| {
            operation(editor);
            editor_cx.notify();
        });
        cx.stop_propagation();
    }

    fn on_surface_mouse_move(
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

    fn on_surface_mouse_up(
        &mut self,
        _event: &MouseUpEvent,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.pointer_anchor = None;
        cx.notify();
    }

    fn on_root_mouse_down(
        &mut self,
        event: &MouseDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let dismissed = self.more_open || self.link_popover.is_some();
        if self.more_open {
            self.more_open = false;
        }
        if self.link_popover.is_some() {
            self.link_popover = None;
        }
        if event.button == MouseButton::Left && event.modifiers.shift {
            self.pointer_anchor = self.editor.update(cx, |editor, editor_cx| {
                let anchor = editor.begin_pointer_selection(event.position, true);
                editor_cx.notify();
                anchor
            });
            focus_editor(&self.editor, window, cx);
        }
        if dismissed {
            focus_editor(&self.editor, window, cx);
            cx.notify();
        }
    }

    fn render_editor_surface(
        &self,
        layout: SpikeLayout,
        content_height: f32,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        let editor = self.editor.clone();
        let image_cache = self.image_cache.clone();
        let width = layout.content_width;
        let canvas_editor = editor.clone();
        let surface = div()
            .id("spike-editor-surface")
            .debug_selector(|| "spike-editor-surface".to_owned())
            .key_context("BlockEditor")
            .track_focus(editor.read(cx).focus_handle())
            .w(px(width))
            .h(px(content_height))
            .rounded(px(7.0))
            .bg(rgba(0xffffffff))
            .can_drop(|dragged, _window, _cx| dragged.is::<ExternalPaths>())
            .on_drop::<ExternalPaths>(cx.listener(Self::on_external_paths_drop))
            .capture_any_mouse_down(cx.listener(Self::on_surface_mouse_down))
            .on_key_down(cx.listener(Self::on_surface_key_down))
            .on_mouse_move(cx.listener(Self::on_surface_mouse_move))
            .on_mouse_up(MouseButton::Left, cx.listener(Self::on_surface_mouse_up))
            .on_mouse_up_out(MouseButton::Left, cx.listener(Self::on_surface_mouse_up));

        let canvas = canvas(
            move |bounds, window, cx| {
                let _ = canvas_editor.update(cx, |editor, editor_cx| {
                    let previous_height = editor.layout().total_height();
                    let mask = window.content_mask().bounds;
                    let (viewport_top, viewport_height) = surface_viewport(bounds, mask);
                    editor.shape_visible_with_window(
                        f32::from(viewport_top),
                        f32::from(viewport_height),
                        width,
                        window,
                    );
                    editor.translate_layout(f32::from(bounds.origin.x), f32::from(bounds.origin.y));
                    let measured_height = editor.layout().total_height();
                    if (measured_height - previous_height).abs() > 0.01 {
                        editor_cx.notify();
                    }
                });
                canvas_editor.clone()
            },
            move |bounds, entity, window, cx| {
                let _ = crate::native_editor::render::paint_entity(
                    entity,
                    bounds,
                    image_cache.clone(),
                    window,
                    cx,
                );
            },
        )
        .w(px(width))
        .h(px(content_height));

        let surface = bind_donor_actions(surface.child(canvas), &self.editor);
        surface.into_any_element()
    }
}

impl Render for SpikeView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let window_size = window.bounds().size;
        let viewport_width = f32::from(window_size.width.max(px(1.0)));
        let viewport_height = f32::from(window_size.height.max(px(1.0)));
        let layout = layout_for_viewport(viewport_width, viewport_height);
        let measured_height = self.editor.read(cx).layout().total_height();
        let content_mask = window.content_mask().bounds;
        let content_height = measured_height.max(f32::from(content_mask.size.height) * 0.65);

        let primary_buttons = self
            .catalogue
            .primary_descriptors()
            .into_iter()
            .map(|descriptor| self.render_command_button(descriptor, false, cx))
            .collect::<Vec<_>>();
        let more_trigger = self.render_more_trigger(cx);
        let measure_view = cx.entity();
        let more_measure = canvas(
            move |bounds, _window, cx| {
                let _ = measure_view.update(cx, |view, view_cx| {
                    if view.more_trigger_bounds != Some(bounds) {
                        view.more_trigger_bounds = Some(bounds);
                        view_cx.notify();
                    }
                });
                measure_view.clone()
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
            .child(more_measure)
            .child(more_trigger)
            .into_any_element();
        let more_menu = self.render_more_menu(self.more_trigger_bounds, content_mask, cx);
        let editor_surface = self.render_editor_surface(layout, content_height, cx);

        let toolbar = div()
            .id("evernote-native-spike-primary-toolbar")
            .debug_selector(|| "evernote-native-spike-primary-toolbar".to_owned())
            .relative()
            .w(px(layout.content_width))
            .flex()
            .flex_wrap()
            .children(primary_buttons)
            .child(more_trigger);
        let scroll = div()
            .id("evernote-native-spike-scroll")
            .size_full()
            .flex()
            .flex_col()
            .items_center()
            .overflow_y_scroll()
            .track_scroll(&self.scroll_handle)
            .child(
                div()
                    .w(px(layout.content_width))
                    .pt(px(28.0))
                    .pb(px(10.0))
                    .text_size(px(24.0))
                    .text_color(rgba(0x172033ff))
                    .child("Evernote editor spike"),
            )
            .child(toolbar)
            .child(editor_surface)
            .pb(px(layout.bottom_padding));

        let mut root = div()
            .id("evernote-native-spike")
            .size_full()
            .relative()
            .bg(rgba(0xf1f3f6ff))
            .on_mouse_down(MouseButton::Left, cx.listener(Self::on_root_mouse_down))
            // The root hitbox remains active while a drag leaves the editor
            // surface. This is GPUI's practical pointer-capture fallback for
            // the canvas, whose own hitbox no longer receives move events
            // once the pointer crosses its bounds.
            .on_mouse_move(cx.listener(Self::on_surface_mouse_move))
            .on_mouse_up(MouseButton::Left, cx.listener(Self::on_surface_mouse_up))
            .on_mouse_up_out(MouseButton::Left, cx.listener(Self::on_surface_mouse_up))
            .child(scroll);
        if let Some(menu) = more_menu {
            root = root.child(menu);
        }
        if let Some(popover) = self.link_popover.clone() {
            root = root.child(self.render_link_popover(popover, cx));
        }
        root
    }
}

fn sample_document() -> Document {
    let mut document = Document::from_paragraphs([
        "A quiet place for ideas, meetings, and the next useful thing.",
        "Capture the important detail before it gets away.",
        "Use the command strip to shape this note without leaving the page.",
    ]);
    let blocks = document.blocks();
    let first = blocks[1].id;
    let last = blocks[2].id;
    let selection = Selection::new(
        DocPoint::with_affinity(first, 0, Affinity::Before),
        DocPoint::with_affinity(
            last,
            blocks[2].content.as_text().map_or(0, str::len),
            Affinity::After,
        ),
    );
    document
        .apply(Transaction::SetBlockKind {
            selection,
            kind: BlockKind::BulletItem { depth: 0 },
        })
        .expect("sample list blocks should be valid");
    document
}

fn focus_editor(editor: &Entity<EditorCore>, window: &mut Window, cx: &mut App) {
    let focus_handle = editor.read(cx).focus_handle().clone();
    focus_handle.focus(window);
}

/// Shared production action seam for paste and Finder drops. The UI handlers
/// only provide platform payloads; image-first classification and structural
/// insertion happen here so both paths commit the same real image node.
fn apply_paste_intent(editor: &mut EditorCore, intent: PasteIntent) -> Result<(), DocumentError> {
    match intent {
        PasteIntent::Image { payload } => editor.insert_image_payload(payload),
        PasteIntent::File { path, cleanup } => {
            let result = editor.insert_image_path(&path);
            if cleanup
                && let Err(error) = std::fs::remove_file(&path)
                && error.kind() != std::io::ErrorKind::NotFound
            {
                eprintln!("failed to remove temporary pasteboard image {path:?}: {error}");
            }
            result
        }
        PasteIntent::Text { text } => editor.paste_plain_text(&text),
        PasteIntent::Unsupported => Ok(()),
    }
}

fn apply_clipboard_payload(
    editor: &mut EditorCore,
    payload: ClipboardPayload,
) -> Result<(), DocumentError> {
    apply_paste_intent(editor, classify_clipboard(payload))
}

fn apply_drop_paths(editor: &mut EditorCore, paths: &[PathBuf]) -> Result<(), DocumentError> {
    apply_paste_intent(editor, classify_drop(paths))
}

fn run_command(
    editor: &Entity<EditorCore>,
    catalogue: CommandCatalogue,
    command: EditorCommand,
    argument: CommandArgument,
    window: &mut Window,
    cx: &mut App,
) {
    let result = editor.update(cx, |editor, editor_cx| {
        let result = catalogue.execute(command, argument, editor);
        editor_cx.notify();
        result
    });
    if let Err(error) = result {
        eprintln!("spike command {command:?} failed: {error}");
    }
    focus_editor(editor, window, cx);
}

fn run_editor_result<F>(
    editor: &Entity<EditorCore>,
    window: &mut Window,
    cx: &mut App,
    operation: F,
) where
    F: FnOnce(&mut EditorCore) -> Result<(), DocumentError>,
{
    let result = editor.update(cx, |editor, editor_cx| {
        let result = operation(editor);
        editor_cx.notify();
        result
    });
    if let Err(error) = result {
        eprintln!("spike editor action failed: {error}");
    }
    focus_editor(editor, window, cx);
}

fn run_editor_mutation<F>(
    editor: &Entity<EditorCore>,
    window: &mut Window,
    cx: &mut App,
    operation: F,
) where
    F: FnOnce(&mut EditorCore),
{
    let _ = editor.update(cx, |editor, editor_cx| {
        operation(editor);
        editor_cx.notify();
    });
    focus_editor(editor, window, cx);
}

macro_rules! bind_result_action {
    ($surface:ident, $editor:expr, $action:ty, $method:ident) => {{
        let action_editor = $editor.clone();
        $surface = $surface.on_action(move |_action: &$action, window, cx| {
            run_editor_result(&action_editor, window, cx, |editor| editor.$method());
        });
    }};
}

macro_rules! bind_mutation_action {
    ($surface:ident, $editor:expr, $action:ty, $method:ident) => {{
        let action_editor = $editor.clone();
        $surface = $surface.on_action(move |_action: &$action, window, cx| {
            run_editor_mutation(&action_editor, window, cx, |editor| editor.$method());
        });
    }};
}

macro_rules! bind_command_action {
    ($surface:ident, $editor:expr, $action:ty, $command:expr, $argument:expr) => {{
        let action_editor = $editor.clone();
        $surface = $surface.on_action(move |_action: &$action, window, cx| {
            run_command(
                &action_editor,
                CommandCatalogue::default(),
                $command,
                $argument,
                window,
                cx,
            );
        });
    }};
}

/// Bind the pinned donor actions to the one native editor entity. This is the
/// same GPUI `key_context`/`track_focus`/`on_action` route used by the donor
/// block renderer; only the operation target changes from a block entity to
/// the document-wide `EditorCore`.
fn bind_donor_actions(
    mut surface: gpui::Stateful<gpui::Div>,
    editor: &Entity<EditorCore>,
) -> gpui::Stateful<gpui::Div> {
    bind_result_action!(surface, editor, Newline, insert_paragraph_break);
    bind_result_action!(surface, editor, DeleteBack, backspace);
    bind_result_action!(surface, editor, Delete, delete_forward);
    bind_mutation_action!(surface, editor, MoveLeft, move_left);
    bind_mutation_action!(surface, editor, MoveRight, move_right);
    bind_mutation_action!(surface, editor, Home, move_home);
    bind_mutation_action!(surface, editor, End, move_end);
    bind_mutation_action!(surface, editor, SelectLeft, select_left);
    bind_mutation_action!(surface, editor, SelectRight, select_right);
    bind_mutation_action!(surface, editor, WordSelectLeft, select_word_left);
    bind_mutation_action!(surface, editor, WordSelectRight, select_word_right);
    bind_mutation_action!(surface, editor, SelectHome, select_home);
    bind_mutation_action!(surface, editor, SelectEnd, select_end);
    bind_mutation_action!(surface, editor, BlockUp, move_up);
    bind_mutation_action!(surface, editor, BlockDown, move_down);
    bind_mutation_action!(surface, editor, FocusPrev, move_up);
    bind_mutation_action!(surface, editor, FocusNext, move_down);
    bind_mutation_action!(surface, editor, SelectAll, select_all);

    let copy_editor = editor.clone();
    surface = surface.on_action(move |_action: &Copy, _window, cx| {
        let text = copy_editor.read(cx).copy_plain_text();
        if !text.is_empty() {
            cx.write_to_clipboard(ClipboardItem::new_string(text));
        }
    });
    let cut_editor = editor.clone();
    surface = surface.on_action(move |_action: &Cut, window, cx| {
        let copied = cut_editor.update(cx, |editor, editor_cx| {
            let result = editor.cut_selection();
            editor_cx.notify();
            result
        });
        if let Ok(text) = copied
            && !text.is_empty()
        {
            cx.write_to_clipboard(ClipboardItem::new_string(text));
        }
        focus_editor(&cut_editor, window, cx);
    });
    let paste_editor = editor.clone();
    surface = surface.on_action(move |_action: &Paste, window, cx| {
        // GPUI 0.2.2 reads plain text before image UTTypes on macOS. The
        // narrow native bridge is therefore consulted first; non-macOS and
        // test platforms retain GPUI ClipboardItem extraction.
        let payload = resolve_clipboard_payload(
            read_native_pasteboard(),
            cx.read_from_clipboard().map(ClipboardPayload::from_gpui),
        );
        if let Some(payload) = payload {
            run_editor_result(&paste_editor, window, cx, |editor| {
                apply_clipboard_payload(editor, payload)
            });
        }
        focus_editor(&paste_editor, window, cx);
    });

    bind_command_action!(
        surface,
        editor,
        Undo,
        EditorCommand::Undo,
        CommandArgument::None
    );
    bind_command_action!(
        surface,
        editor,
        Redo,
        EditorCommand::Redo,
        CommandArgument::None
    );
    bind_command_action!(
        surface,
        editor,
        BoldSelection,
        EditorCommand::Bold,
        CommandArgument::None
    );
    bind_command_action!(
        surface,
        editor,
        ItalicSelection,
        EditorCommand::Italic,
        CommandArgument::None
    );
    bind_command_action!(
        surface,
        editor,
        UnderlineSelection,
        EditorCommand::Underline,
        CommandArgument::None
    );
    bind_command_action!(
        surface,
        editor,
        IndentBlock,
        EditorCommand::IndentList,
        CommandArgument::None
    );
    bind_command_action!(
        surface,
        editor,
        OutdentBlock,
        EditorCommand::OutdentList,
        CommandArgument::None
    );
    surface
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::components::{self, Copy, Cut};
    use gpui::{AppContext, Modifiers, TestAppContext, VisualTestContext, point};
    use std::mem::size_of;

    fn redraw(cx: &mut VisualTestContext) {
        cx.update(|window, app| window.draw(app).clear());
        cx.run_until_parked();
    }

    fn build_view(window: &mut Window, cx: &mut Context<SpikeView>) -> SpikeView {
        let editor = cx.new(|cx| EditorCore::new(Document::from_paragraphs(["alpha", "beta"]), cx));
        editor.read(cx).focus_handle().focus(window);
        SpikeView {
            editor,
            image_cache: None,
            catalogue: CommandCatalogue::default(),
            scroll_handle: ScrollHandle::new(),
            more_open: false,
            link_popover: None,
            pointer_anchor: None,
            more_trigger_bounds: None,
        }
    }

    #[gpui::test]
    fn production_paste_and_finder_drop_actions_insert_real_images(cx: &mut TestAppContext) {
        let mut editor = EditorCore::for_test("前后", cx);
        editor.set_caret_utf8("前".len());
        let payload = ClipboardPayload::fixture_with_png_and_text("图像占位符");
        apply_clipboard_payload(&mut editor, payload).expect("paste action should insert image");
        assert!(editor.copy_all_plain_text().contains('\u{fffc}'));
        assert!(!editor.copy_all_plain_text().contains("图像占位符"));
        assert!(editor.document().blocks().iter().any(|block| matches!(
            block.content,
            crate::native_editor::model::BlockContent::Image { .. }
        )));

        let path =
            std::env::temp_dir().join(format!("joplin-lite-task6-drop-{}.png", std::process::id()));
        let bytes = ClipboardPayload::fixture_with_png_and_text("drop")
            .images
            .into_iter()
            .next()
            .expect("fixture image")
            .bytes;
        std::fs::write(&path, bytes).expect("temporary PNG should be writable");
        editor.set_caret_utf8(0);
        apply_drop_paths(&mut editor, std::slice::from_ref(&path))
            .expect("drop action should insert image");
        let image_count = editor
            .document()
            .blocks()
            .iter()
            .filter(|block| {
                matches!(
                    block.content,
                    crate::native_editor::model::BlockContent::Image { .. }
                )
            })
            .count();
        assert_eq!(image_count, 2);
        let _ = std::fs::remove_file(path);

        let pasteboard_path = std::env::temp_dir().join(format!(
            "joplin-lite-task6-pasteboard-{}.png",
            std::process::id()
        ));
        let pasteboard_bytes = ClipboardPayload::fixture_with_png_and_text("pasteboard")
            .images
            .into_iter()
            .next()
            .expect("fixture image")
            .bytes;
        std::fs::write(&pasteboard_path, pasteboard_bytes)
            .expect("temporary pasteboard PNG should be writable");
        apply_clipboard_payload(
            &mut editor,
            ClipboardPayload {
                file_urls: vec![pasteboard_path.clone()],
                temporary_files: vec![pasteboard_path.clone()],
                ..Default::default()
            },
        )
        .expect("pasteboard path should insert image");
        assert!(
            !pasteboard_path.exists(),
            "temporary pasteboard file is cleaned up"
        );
    }

    fn build_long_view(window: &mut Window, cx: &mut Context<SpikeView>) -> SpikeView {
        let editor =
            cx.new(|cx| EditorCore::new(Document::from_paragraph("wrap ".repeat(360)), cx));
        editor.read(cx).focus_handle().focus(window);
        SpikeView {
            editor,
            image_cache: None,
            catalogue: CommandCatalogue::default(),
            scroll_handle: ScrollHandle::new(),
            more_open: false,
            link_popover: None,
            pointer_anchor: None,
            more_trigger_bounds: None,
        }
    }

    fn build_multiblock_tail_view(window: &mut Window, cx: &mut Context<SpikeView>) -> SpikeView {
        let document = Document::from_paragraphs([
            "first ".repeat(1_200),
            "second ".repeat(1_200),
            "third ".repeat(1_200),
            "fourth ".repeat(1_200),
            "fifth ".repeat(1_200),
            "tail ".repeat(1_200),
        ]);
        let editor = cx.new(|cx| EditorCore::new(document, cx));
        editor.read(cx).focus_handle().focus(window);
        SpikeView {
            editor,
            image_cache: None,
            catalogue: CommandCatalogue::default(),
            scroll_handle: ScrollHandle::new(),
            more_open: false,
            link_popover: None,
            pointer_anchor: None,
            more_trigger_bounds: None,
        }
    }

    fn build_word_cross_block_view(window: &mut Window, cx: &mut Context<SpikeView>) -> SpikeView {
        let mut document = Document::from_paragraph("alpha");
        let first = document.blocks()[0].id;
        document
            .apply(Transaction::InsertImage {
                selection: document.end_selection(),
                resource_id: "word-selection-image".into(),
                natural_size: (100, 100),
            })
            .expect("image fixture");
        let trailing = document.blocks().last().expect("trailing text").id;
        document
            .apply(Transaction::InsertText {
                selection: Selection::caret(DocPoint::with_affinity(trailing, 0, Affinity::Before)),
                text: "omega".into(),
            })
            .expect("trailing text fixture");
        debug_assert_eq!(document.blocks()[0].id, first);
        let editor = cx.new(|cx| EditorCore::new(document, cx));
        editor.read(cx).focus_handle().focus(window);
        SpikeView {
            editor,
            image_cache: None,
            catalogue: CommandCatalogue::default(),
            scroll_handle: ScrollHandle::new(),
            more_open: false,
            link_popover: None,
            pointer_anchor: None,
            more_trigger_bounds: None,
        }
    }

    fn build_multiblock_list_view(window: &mut Window, cx: &mut Context<SpikeView>) -> SpikeView {
        let mut document =
            Document::from_paragraphs(["bullet one", "bullet two", "ordered one", "ordered two"]);
        let blocks = document.blocks().collect_range(0..document.block_count());
        for (index, block) in blocks.iter().enumerate() {
            let text_len = block.content.as_text().map_or(0, str::len);
            let kind = if index < 2 {
                BlockKind::BulletItem { depth: 1 }
            } else {
                BlockKind::OrderedItem { depth: 1 }
            };
            document
                .apply(Transaction::SetBlockKind {
                    selection: Selection::new(
                        DocPoint::with_affinity(block.id, 0, Affinity::Before),
                        DocPoint::with_affinity(block.id, text_len, Affinity::After),
                    ),
                    kind,
                })
                .expect("list fixture conversion");
        }
        let editor = cx.new(|cx| EditorCore::new(document, cx));
        editor.read(cx).focus_handle().focus(window);
        SpikeView {
            editor,
            image_cache: None,
            catalogue: CommandCatalogue::default(),
            scroll_handle: ScrollHandle::new(),
            more_open: false,
            link_popover: None,
            pointer_anchor: None,
            more_trigger_bounds: None,
        }
    }

    fn build_wrapped_blocks_view(window: &mut Window, cx: &mut Context<SpikeView>) -> SpikeView {
        let document = Document::from_paragraphs([
            "0123456789".repeat(48),
            "short middle line".to_string(),
            "abcdefghij".repeat(48),
        ]);
        let editor = cx.new(|cx| EditorCore::new(document, cx));
        editor.read(cx).focus_handle().focus(window);
        SpikeView {
            editor,
            image_cache: None,
            catalogue: CommandCatalogue::default(),
            scroll_handle: ScrollHandle::new(),
            more_open: false,
            link_popover: None,
            pointer_anchor: None,
            more_trigger_bounds: None,
        }
    }

    fn build_decorated_view(window: &mut Window, cx: &mut Context<SpikeView>) -> SpikeView {
        let text = "x".repeat(768);
        let mut document = Document::from_paragraph(text.clone());
        let node = document.first_node_id().expect("decorated paragraph");
        for offset in 0..text.len() {
            let mark = match offset % 4 {
                0 => Mark::Bold,
                1 => Mark::Italic,
                2 => Mark::Underline,
                _ => Mark::Highlight,
            };
            document
                .apply(Transaction::ToggleMark {
                    selection: Selection::new(
                        DocPoint::with_affinity(node, offset, Affinity::Before),
                        DocPoint::with_affinity(node, offset + 1, Affinity::After),
                    ),
                    mark,
                })
                .expect("decorated fixture");
        }
        let editor = cx.new(|cx| EditorCore::new(document, cx));
        editor.read(cx).focus_handle().focus(window);
        SpikeView {
            editor,
            image_cache: None,
            catalogue: CommandCatalogue::default(),
            scroll_handle: ScrollHandle::new(),
            more_open: false,
            link_popover: None,
            pointer_anchor: None,
            more_trigger_bounds: None,
        }
    }

    #[gpui::test]
    async fn shell_event_path_routes_selection_clipboard_and_pointer_drag(cx: &mut TestAppContext) {
        cx.update(|cx| components::init(cx));
        let (view, cx) = cx.add_window_view(build_view);
        redraw(cx);
        cx.update(|window, app| {
            view.read_with(app, |view, app| {
                view.editor.read(app).focus_handle().focus(window)
            });
        });

        cx.simulate_keystrokes("home up shift-right");
        view.read_with(cx, |view, cx| {
            let selection = view.editor.read(cx).selection();
            assert!(
                !selection.is_caret(),
                "Shift+Right must reach EditorCore selection"
            );
        });
        cx.dispatch_action(Copy);
        assert_eq!(
            cx.read_from_clipboard().and_then(|item| item.text()),
            Some("a".into())
        );
        cx.dispatch_action(Cut);
        redraw(cx);
        view.read_with(cx, |view, cx| {
            assert_eq!(
                view.editor.read(cx).document().text_at_index(0),
                Some("lpha")
            );
        });

        view.update(cx, |view, cx| {
            view.editor
                .update(cx, |editor, _| editor.paste_plain_text("L").unwrap());
        });
        redraw(cx);

        cx.simulate_keystrokes("end down");
        view.read_with(cx, |view, cx| {
            let editor = view.editor.read(cx);
            assert_eq!(
                editor.selection().head.node_id,
                editor.document().blocks()[1].id,
                "donor FocusNext binding must move the caret vertically in the document"
            );
        });
        cx.simulate_keystrokes("up");

        cx.simulate_keystrokes("home up shift-down");
        view.read_with(cx, |view, cx| {
            assert!(
                !view.editor.read(cx).selection().is_caret(),
                "production Shift+Down must extend the document selection"
            );
        });

        let bounds = view.read_with(cx, |view, cx| {
            let editor = view.editor.read(cx);
            let first = editor.document().blocks()[0].id;
            editor
                .layout()
                .block_layout(first)
                .expect("first block should have rendered geometry")
                .bounds
        });
        let start = point(
            bounds.left() + bounds.size.width.min(px(20.0)),
            bounds.top() + px(8.0),
        );
        let end = point(bounds.right() - px(4.0), bounds.top() + px(8.0));
        cx.simulate_mouse_down(start, MouseButton::Left, Modifiers::default());
        cx.simulate_mouse_move(end, Some(MouseButton::Left), Modifiers::default());
        cx.simulate_mouse_up(end, MouseButton::Left, Modifiers::default());
        view.read_with(cx, |view, cx| {
            let dragged = view.editor.read(cx).selection();
            assert!(
                !dragged.is_caret(),
                "pointer drag must update one document selection"
            );
        });
    }

    #[gpui::test]
    async fn shell_word_selection_crosses_text_and_structural_blocks(cx: &mut TestAppContext) {
        cx.update(|cx| components::init(cx));
        let (view, cx) = cx.add_window_view(build_word_cross_block_view);
        redraw(cx);
        view.update(cx, |view, cx| {
            let first = view.editor.read(cx).document().blocks()[0].id;
            let first_end = view
                .editor
                .read(cx)
                .document()
                .text_at_index(0)
                .unwrap()
                .len();
            view.editor.update(cx, |editor, editor_cx| {
                editor.set_selection_for_test(Selection::caret(DocPoint::with_affinity(
                    first,
                    first_end,
                    Affinity::After,
                )));
                editor_cx.notify();
            });
        });
        cx.simulate_keystrokes("alt-shift-right");
        view.read_with(cx, |view, cx| {
            let editor = view.editor.read(cx);
            assert_eq!(
                editor.selection().anchor.node_id,
                editor.document().blocks()[0].id
            );
            assert_eq!(
                editor.selection().head.node_id,
                editor.document().blocks()[2].id
            );
            assert_eq!(editor.selection().head.utf8_offset, "omega".len());
        });

        view.update(cx, |view, cx| {
            let third = view.editor.read(cx).document().blocks()[2].id;
            view.editor.update(cx, |editor, editor_cx| {
                editor.set_selection_for_test(Selection::caret(DocPoint::with_affinity(
                    third,
                    0,
                    Affinity::Before,
                )));
                editor_cx.notify();
            });
        });
        cx.simulate_keystrokes("alt-shift-left");
        view.read_with(cx, |view, cx| {
            let editor = view.editor.read(cx);
            assert_eq!(
                editor.selection().anchor.node_id,
                editor.document().blocks()[2].id
            );
            assert_eq!(
                editor.selection().head.node_id,
                editor.document().blocks()[0].id
            );
            assert_eq!(editor.selection().head.utf8_offset, 0);
        });
    }

    #[gpui::test]
    async fn shell_pointer_capture_clamps_outside_surface_and_shift_clicks(
        cx: &mut TestAppContext,
    ) {
        cx.update(|cx| components::init(cx));
        let (view, cx) = cx.add_window_view(build_view);
        redraw(cx);
        let bounds = view.read_with(cx, |view, cx| {
            let first = view.editor.read(cx).document().blocks()[0].id;
            view.editor
                .read(cx)
                .layout()
                .block_layout(first)
                .expect("first block geometry")
                .bounds
        });
        let start = point(bounds.left() + px(2.0), bounds.top() + px(8.0));
        let outside = point(bounds.right() + px(120.0), bounds.top() + px(96.0));
        cx.simulate_mouse_down(start, MouseButton::Left, Modifiers::default());
        cx.simulate_mouse_move(outside, Some(MouseButton::Left), Modifiers::default());
        cx.simulate_mouse_up(outside, MouseButton::Left, Modifiers::default());
        view.read_with(cx, |view, cx| {
            assert!(!view.editor.read(cx).selection().is_caret());
        });

        cx.simulate_mouse_down(start, MouseButton::Left, Modifiers::default());
        cx.simulate_mouse_up(start, MouseButton::Left, Modifiers::default());
        cx.simulate_mouse_down(
            outside,
            MouseButton::Left,
            Modifiers {
                shift: true,
                ..Modifiers::default()
            },
        );
        cx.simulate_mouse_up(
            outside,
            MouseButton::Left,
            Modifiers {
                shift: true,
                ..Modifiers::default()
            },
        );
        view.read_with(cx, |view, cx| {
            assert!(
                !view.editor.read(cx).selection().is_caret(),
                "Shift-click must extend from the prior anchor"
            );
        });
    }

    #[gpui::test]
    async fn shell_shift_click_drag_preserves_original_anchor(cx: &mut TestAppContext) {
        cx.update(|cx| components::init(cx));
        let (view, cx) = cx.add_window_view(build_view);
        redraw(cx);
        let (first, second, first_id, second_id) = view.read_with(cx, |view, cx| {
            let editor = view.editor.read(cx);
            (
                editor
                    .layout()
                    .block_layout(editor.document().blocks()[0].id)
                    .unwrap()
                    .bounds,
                editor
                    .layout()
                    .block_layout(editor.document().blocks()[1].id)
                    .unwrap()
                    .bounds,
                editor.document().blocks()[0].id,
                editor.document().blocks()[1].id,
            )
        });
        let a = point(first.left() + px(2.0), first.top() + px(8.0));
        let b = point(second.right() - px(2.0), second.top() + px(8.0));
        let c = point(second.left() + px(20.0), second.top() + px(8.0));
        // These are hand-derived UTF-8 offsets for the literal fixture and
        // pointer positions, independent of the production hit-test helper.
        let expected_a = DocPoint::with_affinity(first_id, 0, Affinity::After);
        let expected_c = DocPoint::with_affinity(second_id, 2, Affinity::After);
        cx.simulate_mouse_down(a, MouseButton::Left, Modifiers::default());
        cx.simulate_mouse_up(a, MouseButton::Left, Modifiers::default());
        cx.simulate_mouse_down(
            b,
            MouseButton::Left,
            Modifiers {
                shift: true,
                ..Modifiers::default()
            },
        );
        cx.simulate_mouse_move(c, Some(MouseButton::Left), Modifiers::default());
        cx.simulate_mouse_up(c, MouseButton::Left, Modifiers::default());
        view.read_with(cx, |view, cx| {
            let editor = view.editor.read(cx);
            assert_eq!(
                editor.selection().anchor.node_id,
                editor.document().blocks()[0].id
            );
            assert_eq!(
                editor.selection().head.node_id,
                editor.document().blocks()[1].id
            );
            assert_eq!(editor.selection().anchor, expected_a);
            assert_eq!(editor.selection().head, expected_c);
        });
    }

    #[gpui::test]
    async fn shell_link_field_key_sequence_copies_and_deletes_graphemes(cx: &mut TestAppContext) {
        cx.update(|cx| components::init(cx));
        let (view, cx) = cx.add_window_view(build_view);
        redraw(cx);
        cx.update(|window, app| {
            view.read_with(app, |view, app| {
                view.editor.read(app).focus_handle().focus(window)
            });
        });
        cx.simulate_keystrokes("home up shift-right");
        cx.update(|window, app| {
            view.update(app, |view, view_cx| view.open_link_popover(window, view_cx));
        });
        redraw(cx);

        cx.simulate_input("a🙂b");
        cx.simulate_keystrokes("shift-left shift-left");
        cx.simulate_keystrokes("cmd-c");
        assert_eq!(
            cx.read_from_clipboard().and_then(|item| item.text()),
            Some("🙂b".into())
        );
        cx.simulate_keystrokes("backspace");
        view.read_with(cx, |view, cx| {
            let popover = view.link_popover.as_ref().expect("URL field remains open");
            assert_eq!(popover.read(cx).text, "a");
            assert_eq!(popover.read(cx).selected_text(), "");
        });
    }

    #[gpui::test]
    async fn shell_multiblock_toolbar_and_outdent_use_one_history_entry(cx: &mut TestAppContext) {
        cx.update(|cx| components::init(cx));
        let (view, cx) = cx.add_window_view(build_multiblock_list_view);
        redraw(cx);
        let ids = view.read_with(cx, |view, cx| {
            view.editor
                .read(cx)
                .document()
                .blocks()
                .iter()
                .map(|block| block.id)
                .collect::<Vec<_>>()
        });
        let first_selection = view.update(cx, |view, cx| {
            view.editor.update(cx, |editor, editor_cx| {
                let end = editor.document().text_at_index(1).unwrap().len();
                editor.set_selection_for_test(Selection::new(
                    DocPoint::with_affinity(ids[0], 0, Affinity::Before),
                    DocPoint::with_affinity(ids[1], end, Affinity::After),
                ));
                editor_cx.notify();
                editor.selection()
            })
        });
        let before_bold = view.read_with(cx, |view, cx| view.editor.read(cx).undo_depth());
        let bold = cx.debug_bounds("Bold").expect("Bold toolbar button");
        cx.simulate_click(bold.center(), Modifiers::default());
        redraw(cx);
        view.read_with(cx, |view, cx| {
            let editor = view.editor.read(cx);
            assert_eq!(editor.selection(), first_selection);
            assert_eq!(editor.undo_depth(), before_bold + 1);
            for id in &ids[..2] {
                assert!(
                    editor
                        .document()
                        .block(*id)
                        .unwrap()
                        .content
                        .styles()
                        .unwrap()
                        .iter()
                        .any(|run| run.marks.contains(&Mark::Bold))
                );
            }
        });

        let before_bullet_outdent =
            view.read_with(cx, |view, cx| view.editor.read(cx).undo_depth());
        // Open the actual measured More menu through its trigger, then use the
        // rendered Outdent command rather than calling the catalogue directly.
        view.update(cx, |view, cx| {
            view.editor.update(cx, |editor, editor_cx| {
                editor.set_selection_for_test(first_selection);
                editor_cx.notify();
            });
        });
        let trigger = cx
            .debug_bounds("evernote-native-spike-more-trigger")
            .expect("More trigger");
        cx.simulate_click(trigger.center(), Modifiers::default());
        redraw(cx);
        let outdent = cx.debug_bounds("Outdent list").expect("Outdent command");
        cx.simulate_click(outdent.center(), Modifiers::default());
        redraw(cx);
        view.read_with(cx, |view, cx| {
            let editor = view.editor.read(cx);
            assert_eq!(editor.undo_depth(), before_bullet_outdent + 1);
            assert!(
                editor
                    .document()
                    .blocks()
                    .iter_range(0..2)
                    .all(|block| matches!(block.kind, BlockKind::BulletItem { depth: 0 }))
            );
        });

        let before_ordered_outdent =
            view.read_with(cx, |view, cx| view.editor.read(cx).undo_depth());
        view.update(cx, |view, cx| {
            view.editor.update(cx, |editor, editor_cx| {
                let end = editor.document().text_at_index(3).unwrap().len();
                editor.set_selection_for_test(Selection::new(
                    DocPoint::with_affinity(ids[2], 0, Affinity::Before),
                    DocPoint::with_affinity(ids[3], end, Affinity::After),
                ));
                editor_cx.notify();
            });
        });
        let trigger = cx
            .debug_bounds("evernote-native-spike-more-trigger")
            .expect("More trigger remains mounted");
        cx.simulate_click(trigger.center(), Modifiers::default());
        redraw(cx);
        let outdent = cx
            .debug_bounds("Outdent list")
            .expect("Outdent command remains available");
        cx.simulate_click(outdent.center(), Modifiers::default());
        redraw(cx);
        view.read_with(cx, |view, cx| {
            let editor = view.editor.read(cx);
            assert_eq!(editor.undo_depth(), before_ordered_outdent + 1);
            assert!(
                editor
                    .document()
                    .blocks()
                    .iter_range(2..4)
                    .all(|block| matches!(block.kind, BlockKind::OrderedItem { depth: 0 }))
            );
        });
    }

    #[gpui::test]
    async fn editable_link_popover_uses_input_bridge_and_preserves_selection(
        cx: &mut TestAppContext,
    ) {
        cx.update(|cx| components::init(cx));
        let (view, cx) = cx.add_window_view(build_view);
        redraw(cx);
        cx.update(|window, app| {
            view.read_with(app, |view, app| {
                view.editor.read(app).focus_handle().focus(window)
            });
        });
        cx.simulate_keystrokes("home up shift-right");
        let original_selection = view.read_with(cx, |view, cx| view.editor.read(cx).selection());

        cx.update(|window, app| {
            view.update(app, |view, view_cx| view.open_link_popover(window, view_cx));
        });
        redraw(cx);
        view.read_with(cx, |view, cx| {
            assert!(
                view.link_popover.is_some(),
                "Link must open an editable popover"
            );
            assert_eq!(view.editor.read(cx).selection(), original_selection);
        });

        cx.simulate_input("https://example.com");
        view.read_with(cx, |view, cx| {
            let popover = view.link_popover.as_ref().expect("popover still focused");
            assert_eq!(popover.read(cx).text, "https://example.com");
        });
        cx.simulate_keystrokes("enter");
        redraw(cx);
        view.read_with(cx, |view, cx| {
            assert!(view.link_popover.is_none(), "Enter must submit and close the popover");
            assert_eq!(view.editor.read(cx).selection(), original_selection);
            let first = view.editor.read(cx).document().blocks()[0].id;
            let styles = view
                .editor
                .read(cx)
                .document()
                .block(first)
                .and_then(|block| block.content.styles())
                .expect("submitted link must create a styled text run");
            assert!(styles.iter().any(|run| {
                run.range == (0..1)
                    && run.marks.iter().any(|mark| {
                        matches!(mark, crate::native_editor::model::Mark::Link(url) if url == "https://example.com")
                    })
            }));
        });

        let before_cancel = view.read_with(cx, |view, cx| {
            (
                view.editor.read(cx).document().semantic_snapshot(),
                view.editor.read(cx).undo_depth(),
                view.editor.read(cx).selection(),
            )
        });
        cx.update(|window, app| {
            view.update(app, |view, view_cx| view.open_link_popover(window, view_cx));
        });
        redraw(cx);
        cx.simulate_input("not a url");
        cx.simulate_keystrokes("enter");
        redraw(cx);
        view.read_with(cx, |view, cx| {
            assert!(
                view.link_popover.is_none(),
                "invalid submit must close without mutating"
            );
            assert_eq!(
                view.editor.read(cx).document().semantic_snapshot(),
                before_cancel.0
            );
            assert_eq!(view.editor.read(cx).undo_depth(), before_cancel.1);
            assert_eq!(view.editor.read(cx).selection(), before_cancel.2);
        });

        cx.update(|window, app| {
            view.update(app, |view, view_cx| view.open_link_popover(window, view_cx));
        });
        redraw(cx);
        cx.simulate_keystrokes("enter");
        redraw(cx);
        view.read_with(cx, |view, cx| {
            assert!(
                view.link_popover.is_none(),
                "empty submit must close without mutating"
            );
            assert_eq!(
                view.editor.read(cx).document().semantic_snapshot(),
                before_cancel.0
            );
            assert_eq!(view.editor.read(cx).undo_depth(), before_cancel.1);
            assert_eq!(view.editor.read(cx).selection(), before_cancel.2);
        });

        cx.update(|window, app| {
            view.update(app, |view, view_cx| view.open_link_popover(window, view_cx));
        });
        redraw(cx);
        cx.simulate_keystrokes("escape");
        redraw(cx);
        view.read_with(cx, |view, cx| {
            assert!(
                view.link_popover.is_none(),
                "Escape must cancel and close the popover"
            );
            assert_eq!(
                view.editor.read(cx).document().semantic_snapshot(),
                before_cancel.0
            );
            assert_eq!(view.editor.read(cx).undo_depth(), before_cancel.1);
            assert_eq!(view.editor.read(cx).selection(), before_cancel.2);
        });
    }

    #[gpui::test]
    fn link_popover_ime_candidate_updates_replace_the_marked_range(cx: &mut TestAppContext) {
        let mut cx = cx.add_empty_window();
        let field = cx.new(|cx| LinkPopover::new(String::new(), cx));
        cx.update(|window, app| {
            field.update(app, |field, field_cx| {
                <LinkPopover as EntityInputHandler>::replace_and_mark_text_in_range(
                    field,
                    None,
                    "n",
                    Some(1..1),
                    window,
                    field_cx,
                );
                assert_eq!(field.text, "n");
                assert_eq!(
                    <LinkPopover as EntityInputHandler>::marked_text_range(field, window, field_cx),
                    Some(0..1)
                );
                <LinkPopover as EntityInputHandler>::replace_and_mark_text_in_range(
                    field,
                    None,
                    "ni",
                    Some(2..2),
                    window,
                    field_cx,
                );
                assert_eq!(field.text, "ni");
                assert_eq!(field.selection, 2..2);
                <LinkPopover as EntityInputHandler>::replace_text_in_range(
                    field, None, "你", window, field_cx,
                );
                assert_eq!(field.text, "你");
            });
        });
    }

    #[gpui::test]
    fn link_popover_reverse_selection_uses_sorted_range_and_graphemes(cx: &mut TestAppContext) {
        let mut cx = cx.add_empty_window();
        let field = cx.new(|cx| LinkPopover::new("a🙂e\u{301}bc".to_owned(), cx));
        cx.update(|_, app| {
            field.update(app, |field, _| {
                field.move_to_edge(false, false);
                field.move_to_edge(true, false);
                field.move_horizontal(false, true);
                field.move_horizontal(false, true);
                assert_eq!(field.selected_text(), "bc");
                assert!(field.reversed);
                field.delete_backward();
                assert_eq!(field.text, "a🙂e\u{301}");
                field.delete_backward();
                assert_eq!(field.text, "a🙂");
                field.delete_backward();
                assert_eq!(field.text, "a");
            });
        });
    }

    #[gpui::test]
    async fn more_menu_dispatches_catalogue_command_and_restores_editor_focus(
        cx: &mut TestAppContext,
    ) {
        cx.update(|cx| components::init(cx));
        let (view, cx) = cx.add_window_view(build_view);
        redraw(cx);
        cx.update(|window, app| {
            view.read_with(app, |view, app| {
                view.editor.read(app).focus_handle().focus(window)
            });
        });
        cx.simulate_keystrokes("home up shift-right");
        let original_selection = view.read_with(cx, |view, cx| view.editor.read(cx).selection());

        let bold = cx
            .debug_bounds("Bold")
            .expect("Bold must be on the primary strip");
        cx.simulate_click(bold.center(), Modifiers::default());
        redraw(cx);
        view.read_with(cx, |view, cx| {
            assert_eq!(view.editor.read(cx).selection(), original_selection);
            let first = view.editor.read(cx).document().blocks()[0].id;
            let styles = view
                .editor
                .read(cx)
                .document()
                .block(first)
                .and_then(|block| block.content.styles())
                .expect("primary command must use the document text path");
            assert!(
                styles
                    .iter()
                    .any(|run| run.marks.contains(&crate::native_editor::model::Mark::Bold))
            );
        });

        let trigger = cx
            .debug_bounds("evernote-native-spike-more-trigger")
            .expect("More trigger must be mounted");
        cx.simulate_click(trigger.center(), Modifiers::default());
        redraw(cx);
        view.read_with(cx, |view, cx| {
            assert!(view.more_open, "More click must open menu state");
            assert_eq!(view.editor.read(cx).selection(), original_selection);
        });
        assert!(
            cx.debug_bounds("evernote-native-spike-more-menu").is_some(),
            "More menu must be a mounted popup surface"
        );

        let heading = cx
            .debug_bounds("Heading 1")
            .expect("Heading 1 must be in More");
        view.read_with(cx, |view, cx| {
            assert!(
                view.catalogue
                    .state(EditorCommand::Heading1, view.editor.read(cx))
                    .enabled
            );
        });
        cx.simulate_mouse_down(heading.center(), MouseButton::Left, Modifiers::default());
        view.read_with(cx, |view, _cx| {
            assert!(
                !view.more_open,
                "More command pointer-down must dismiss the menu"
            );
        });
        cx.simulate_mouse_up(heading.center(), MouseButton::Left, Modifiers::default());
        redraw(cx);
        view.read_with(cx, |view, cx| {
            assert!(
                !view.more_open,
                "using a More command must dismiss the menu"
            );
            assert_eq!(view.editor.read(cx).selection(), original_selection);
            assert!(matches!(
                view.editor.read(cx).document().blocks()[0].kind,
                BlockKind::Heading { level: 1 }
            ));
        });
        cx.update(|window, app| {
            view.read_with(app, |view, app| {
                assert!(view.editor.read(app).focus_handle().is_focused(window));
            });
        });

        let trigger = cx
            .debug_bounds("evernote-native-spike-more-trigger")
            .expect("More trigger must remain mounted");
        cx.simulate_click(trigger.center(), Modifiers::default());
        redraw(cx);
        cx.simulate_click(point(px(8.0), px(8.0)), Modifiers::default());
        redraw(cx);
        view.read_with(cx, |view, _cx| {
            assert!(!view.more_open, "outside click must dismiss the More menu");
        });
    }

    #[gpui::test]
    async fn shell_more_menu_uses_measured_bounds_in_short_narrow_view(cx: &mut TestAppContext) {
        cx.update(|cx| components::init(cx));
        let (_view, cx) = cx.add_window_view(build_long_view);
        cx.simulate_resize(size(px(400.0), px(260.0)));
        redraw(cx);
        let trigger = cx
            .debug_bounds("evernote-native-spike-more-trigger")
            .expect("More trigger must be mounted in a narrow window");
        cx.simulate_click(trigger.center(), Modifiers::default());
        redraw(cx);
        let menu = cx
            .debug_bounds("evernote-native-spike-more-menu")
            .expect("More menu must be mounted after the measured trigger click");
        assert!(menu.left() >= px(0.0));
        assert!(menu.right() <= px(400.0));
        assert!(menu.top() >= px(0.0));
        assert!(menu.bottom() <= px(260.0));
        assert!(menu.top() >= trigger.bottom() || menu.bottom() <= trigger.top());
    }

    #[gpui::test]
    async fn shell_scroll_uses_measured_wrapped_extent_and_live_viewport(cx: &mut TestAppContext) {
        cx.update(|cx| components::init(cx));
        let (view, cx) = cx.add_window_view(build_long_view);
        redraw(cx);
        let (total_height, max_offset) = view.read_with(cx, |view, cx| {
            (
                view.editor.read(cx).layout().total_height(),
                view.scroll_handle.max_offset(),
            )
        });
        assert!(
            total_height > 400.0,
            "a wrapped block must contribute its measured line extent"
        );
        assert!(
            max_offset.height > px(0.0),
            "the scroll viewport must expose the measured block extent"
        );
        let before_top = view.read_with(cx, |view, cx| {
            view.editor
                .read(cx)
                .layout()
                .visible()
                .first()
                .map(|layout| layout.bounds.top())
        });

        view.update(cx, |view, cx| {
            view.scroll_handle
                .set_offset(gpui::point(px(0.0), -max_offset.height));
            cx.notify();
        });
        redraw(cx);
        view.read_with(cx, |view, cx| {
            assert!(view.scroll_handle.offset().y < px(0.0));
            let after_top = view
                .editor
                .read(cx)
                .layout()
                .visible()
                .first()
                .map(|layout| layout.bounds.top());
            assert!(
                after_top < before_top,
                "shaping must follow the live scroll origin instead of always starting at document y=0"
            );
        });
    }

    #[gpui::test]
    async fn shell_render_observes_highlight_paint_and_snapshot_clone_peak(
        cx: &mut TestAppContext,
    ) {
        cx.update(|cx| components::init(cx));
        crate::native_editor::render::reset_test_render_observations();
        let (view, cx) = cx.add_window_view(build_decorated_view);
        redraw(cx);
        view.read_with(cx, |view, cx| {
            let editor = view.editor.read(cx);
            let node = editor.document().first_node_id().expect("decorated node");
            let cached = editor
                .layout()
                .cache
                .get(&node)
                .expect("decorated block must be retained for the production paint");
            let retained_bytes = cached.retained_bytes;
            let snapshot_clone_bytes = cached.snapshot_clone_bytes;
            let line_capacity = cached.layout.text_lines.capacity();
            let decoration_run_count = cached.decoration_run_count;
            assert!(
                decoration_run_count > 32,
                "fixture must exercise alternating decoration runs"
            );
            assert_eq!(cached.bytes, retained_bytes + snapshot_clone_bytes);
            assert!(
                retained_bytes + snapshot_clone_bytes <= editor.layout().budget_bytes(),
                "retained cache plus one real snapshot clone must stay under the budget"
            );
            assert!(
                snapshot_clone_bytes >= line_capacity * size_of::<gpui::WrappedLine>(),
                "snapshot clone accounting must include its owned WrappedLine Vec"
            );
            assert!(
                snapshot_clone_bytes
                    > size_of::<gpui::WrappedLine>() * line_capacity
                        + size_of::<Vec<gpui::WrappedLine>>(),
                "decoration spill must be part of the clone peak, not just the line Vec"
            );
            let actual_snapshot_bytes =
                crate::native_editor::render::snapshot_allocation_bytes_for_test(editor);
            assert!(
                actual_snapshot_bytes > 0,
                "production snapshot clone must allocate measurable owned buffers"
            );
            assert!(
                retained_bytes + actual_snapshot_bytes <= editor.layout().budget_bytes(),
                "retained cache plus the measured production snapshot must stay under the budget"
            );
            assert!(
                actual_snapshot_bytes
                    >= line_capacity * size_of::<gpui::WrappedLine>()
                        + size_of::<Vec<gpui::WrappedLine>>(),
                "allocator observation must include the owned WrappedLine Vec"
            );
            assert!(
                actual_snapshot_bytes
                    >= line_capacity * size_of::<gpui::WrappedLine>()
                        + decoration_run_count.saturating_sub(32)
                            * size_of::<gpui::DecorationRun>(),
                "allocator observation must include spilled decoration runs"
            );
            assert!(editor.layout().used_bytes() <= editor.layout().budget_bytes());
        });
        assert!(
            crate::native_editor::render::test_highlight_background_paints() > 0,
            "production paint_entity must invoke background painting for Highlight runs"
        );
        assert!(
            crate::native_editor::render::test_snapshot_clone_peak() > 0,
            "production render snapshot must observe a non-empty retained-line clone"
        );
    }

    #[gpui::test]
    async fn shell_live_viewport_membership_reaches_exact_tail_line_click(cx: &mut TestAppContext) {
        cx.update(|cx| components::init(cx));
        let (view, cx) = cx.add_window_view(build_multiblock_tail_view);
        redraw(cx);
        let max_offset = view.read_with(cx, |view, _| view.scroll_handle.max_offset());
        view.update(cx, |view, cx| {
            view.scroll_handle
                .set_offset(point(px(0.0), -max_offset.height));
            cx.notify();
        });
        redraw(cx);

        let (target, target_bounds, line_height, visible_ids, expected_ids, tail_len) = view
            .read_with(cx, |view, cx| {
                let editor = view.editor.read(cx);
                let ids = editor
                    .document()
                    .blocks()
                    .iter()
                    .map(|block| block.id)
                    .collect::<Vec<_>>();
                let target = *ids.last().expect("tail paragraph");
                let target_bounds = editor
                    .layout()
                    .block_layout(target)
                    .expect("scrolled tail block remains shaped")
                    .bounds;
                let line_height = editor
                    .layout()
                    .line_height(target)
                    .expect("tail line height");
                let visible_ids = editor
                    .layout()
                    .visible()
                    .iter()
                    .map(|layout| layout.node_id)
                    .collect::<Vec<_>>();
                let expected_ids = ids[4..].to_vec();
                let tail_len = editor.document().text_at_index(5).expect("tail text").len();
                (
                    target,
                    target_bounds,
                    line_height,
                    visible_ids,
                    expected_ids,
                    tail_len,
                )
            });
        assert_eq!(
            visible_ids, expected_ids,
            "live viewport must include exactly the tail multi-block range"
        );
        assert!(
            !visible_ids.contains(&view.read_with(cx, |view, cx| {
                view.editor.read(cx).document().blocks()[0].id
            })),
            "the first offscreen node must be excluded"
        );
        let tail_click = point(
            target_bounds.right() - px(2.0),
            target_bounds.bottom() - line_height / 2.0,
        );
        cx.simulate_click(tail_click, Modifiers::default());
        redraw(cx);
        view.read_with(cx, |view, cx| {
            let editor = view.editor.read(cx);
            let selection = editor.selection();
            assert_eq!(selection.head.node_id, target);
            assert_eq!(
                selection.head.utf8_offset, tail_len,
                "a click in the exact tail row must reach the document end"
            );
            assert_eq!(selection.head.affinity, Affinity::After);
        });
    }

    #[gpui::test]
    async fn shell_consecutive_shift_vertical_moves_preserve_preferred_x(cx: &mut TestAppContext) {
        cx.update(|cx| components::init(cx));
        let (view, cx) = cx.add_window_view(build_wrapped_blocks_view);
        redraw(cx);
        let origin = view.update(cx, |view, cx| {
            let node = view.editor.read(cx).document().blocks()[0].id;
            let origin = DocPoint::with_affinity(node, 120, Affinity::After);
            view.editor.update(cx, |editor, editor_cx| {
                editor.set_selection_for_test(Selection::caret(origin));
                editor_cx.notify();
            });
            origin
        });
        let origin_x = view.read_with(cx, |view, cx| {
            view.editor
                .read(cx)
                .layout()
                .caret_x(origin)
                .expect("origin caret geometry")
        });
        for key in ["shift-down", "shift-down", "shift-up", "shift-up"] {
            cx.simulate_keystrokes(key);
            view.read_with(cx, |view, cx| {
                let editor = view.editor.read(cx);
                let step_x = editor
                    .layout()
                    .caret_x(editor.selection().head)
                    .expect("caret geometry after each vertical event");
                assert!(
                    f32::from(step_x - origin_x).abs() <= 0.5,
                    "{key} must preserve preferred screen x at every step: {origin_x:?} -> {step_x:?}"
                );
            });
        }
        view.read_with(cx, |view, cx| {
            let editor = view.editor.read(cx);
            let selection = editor.selection();
            assert_eq!(selection.anchor, origin);
            assert_eq!(selection.head, origin);
            let returned_x = editor
                .layout()
                .caret_x(selection.head)
                .expect("returned caret geometry");
            assert!(
                f32::from(returned_x - origin_x).abs() <= 0.5,
                "consecutive Shift+Down/Up must preserve preferred screen x: {origin_x:?} -> {returned_x:?}"
            );
        });
    }

    #[test]
    fn surface_viewport_is_the_intersection_with_the_content_mask() {
        let surface = Bounds::new(point(px(24.0), px(96.0)), size(px(336.0), px(900.0)));
        let mask = Bounds::new(point(px(0.0), px(12.0)), size(px(400.0), px(260.0)));
        assert_eq!(surface_viewport(surface, mask), (0.0, 176.0));

        let scrolled_surface = Bounds::new(point(px(24.0), px(-180.0)), size(px(336.0), px(900.0)));
        assert_eq!(surface_viewport(scrolled_surface, mask), (192.0, 260.0));
    }

    #[gpui::test]
    async fn shell_narrow_wrapped_toolbar_and_scrolled_title_keep_live_membership(
        cx: &mut TestAppContext,
    ) {
        cx.update(|cx| components::init(cx));
        let (view, cx) = cx.add_window_view(build_long_view);
        cx.simulate_resize(size(px(400.0), px(260.0)));
        redraw(cx);

        let toolbar = cx
            .debug_bounds("evernote-native-spike-primary-toolbar")
            .expect("narrow toolbar should be mounted");
        let surface = cx
            .debug_bounds("spike-editor-surface")
            .expect("editor surface should be mounted");
        assert!(
            toolbar.size.height > px(31.0),
            "narrow width must wrap the primary command strip"
        );
        assert!(
            surface.top() > px(118.0),
            "surface origin must include the wrapped toolbar rather than a fixed 118pt shell offset"
        );
        let before_top = view.read_with(cx, |view, cx| {
            view.editor
                .read(cx)
                .layout()
                .visible()
                .first()
                .map(|layout| layout.bounds.top())
                .expect("narrow surface should shape a visible first block")
        });
        assert!(before_top >= surface.top() - px(1.0));

        let max_offset = view.read_with(cx, |view, _| view.scroll_handle.max_offset());
        view.update(cx, |view, cx| {
            view.scroll_handle
                .set_offset(point(px(0.0), -max_offset.height.min(px(220.0))));
            cx.notify();
        });
        redraw(cx);
        view.read_with(cx, |view, cx| {
            let after_top = view
                .editor
                .read(cx)
                .layout()
                .visible()
                .first()
                .map(|layout| layout.bounds.top())
                .expect("scrolled surface should retain visible tail content");
            assert!(
                after_top < before_top,
                "title/toolbar scroll must move layout membership with the live scroll origin"
            );
        });
    }
}
