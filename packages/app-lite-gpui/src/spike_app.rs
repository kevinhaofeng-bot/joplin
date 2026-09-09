//! Isolated native GPUI route for the Evernote-style editor spike.
//!
//! This module deliberately stops at the native editor entity. It does not
//! construct the ordinary workspace, updater, exporter, network client, or
//! sync services. The action names and default key bindings still come from
//! `components::actions` (the pinned Velotype donor path), while selection,
//! focus, transactions, and platform input remain owned by `EditorCore`.

use std::ops::Range;

use gpui::{
    App, AppContext, Bounds, ClipboardItem, Context, ElementInputHandler, Entity,
    EntityInputHandler, FocusHandle, InteractiveElement, IntoElement, KeyDownEvent, MouseButton,
    MouseDownEvent, MouseMoveEvent, MouseUpEvent, ParentElement, Pixels, Point, Render,
    ScrollHandle, SharedString, StatefulInteractiveElement, Styled, TextRun, UTF16Selection,
    Window, WindowBounds, WindowHandle, WindowOptions, canvas, div, px, rgba, size,
};

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
use crate::native_editor::model::{
    Affinity, BlockKind, DocPoint, Document, DocumentError, Mark, Selection,
};
use crate::native_editor::transaction::Transaction;

gpui::actions!(evernote_spike, [SubmitLink, CancelLink]);

// The heading and two-row primary strip occupy this fixed leading region of
// the scroll column.  The editor still gets the live scroll offset and the
// remaining window height below it; keeping the offset explicit prevents the
// layout registry from shaping the whole document on every scroll tick.
const SPIKE_EDITOR_TOP: f32 = 118.0;

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
                cx.new(|_| SpikeView {
                    editor,
                    catalogue: CommandCatalogue::default(),
                    scroll_handle: ScrollHandle::new(),
                    more_open: false,
                    link_popover: None,
                    pointer_anchor: None,
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
    last_bounds: Option<Bounds<Pixels>>,
}

impl LinkPopover {
    fn new(initial: String, cx: &mut Context<Self>) -> Self {
        let end = initial.len();
        Self {
            focus: cx.focus_handle(),
            text: initial,
            selection: end..end,
            reversed: false,
            last_bounds: None,
        }
    }

    fn replace_range(&mut self, range: Option<Range<usize>>, text: &str) {
        let byte_range = range.unwrap_or_else(|| self.selection.clone());
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
        None
    }

    fn unmark_text(&mut self, _window: &mut Window, _cx: &mut Context<Self>) {}

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
        let start = range_utf8
            .as_ref()
            .map(|range| range.start)
            .unwrap_or(self.selection.start);
        self.replace_range(range_utf8, new_text);
        if let Some(selected_range) = new_selected_range {
            let selected_range =
                crate::native_editor::input::utf16_range_to_utf8_in(new_text, &selected_range);
            self.selection = start + selected_range.start..start + selected_range.end;
        }
        cx.notify();
    }

    fn bounds_for_range(
        &mut self,
        _range_utf16: Range<usize>,
        _element_bounds: Bounds<Pixels>,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<Bounds<Pixels>> {
        self.last_bounds
    }

    fn character_index_for_point(
        &mut self,
        point: Point<Pixels>,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<usize> {
        let bounds = self.last_bounds?;
        let x = (point.x - bounds.left()).max(px(0.0));
        let font_size = px(14.0);
        // The bridge only needs a stable UTF-16 index.  The next platform
        // replacement will still be converted against the exact field text.
        let approx = (f32::from(x) / f32::from(font_size)).floor().max(0.0) as usize;
        Some(crate::native_editor::input::utf8_to_utf16_in(
            &self.text,
            self.text
                .char_indices()
                .map(|(index, _)| index)
                .chain([self.text.len()])
                .nth(approx)
                .unwrap_or(self.text.len()),
        ))
    }
}

pub(crate) struct SpikeView {
    editor: Entity<EditorCore>,
    catalogue: CommandCatalogue,
    scroll_handle: ScrollHandle,
    more_open: bool,
    link_popover: Option<Entity<LinkPopover>>,
    pointer_anchor: Option<DocPoint>,
}

impl SpikeView {
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
        match event.keystroke.key.as_str() {
            "enter" => {
                cx.stop_propagation();
                self.submit_link(&SubmitLink, window, cx);
            }
            "escape" => {
                cx.stop_propagation();
                self.cancel_link(&CancelLink, window, cx);
            }
            _ => {}
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
            .on_mouse_down(MouseButton::Left, |_event, _window, cx| {
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

    fn render_more_menu(&self, left: f32, cx: &mut Context<Self>) -> Option<gpui::AnyElement> {
        if !self.more_open {
            return None;
        }
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
                .top(px(SPIKE_EDITOR_TOP))
                .left(px(left))
                .w(px(680.0))
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
        self.pointer_anchor = self.editor.update(cx, |editor, _cx| {
            editor.begin_pointer_selection(event.position)
        });
        focus_editor(&self.editor, window, cx);
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
        _cx: &mut Context<Self>,
    ) {
        self.pointer_anchor = None;
    }

    fn on_root_mouse_down(
        &mut self,
        _event: &MouseDownEvent,
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
        if dismissed {
            focus_editor(&self.editor, window, cx);
            cx.notify();
        }
    }

    fn render_editor_surface(
        &self,
        layout: SpikeLayout,
        content_height: f32,
        viewport_top: f32,
        viewport_height: f32,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        let editor = self.editor.clone();
        let width = layout.content_width;
        let canvas_editor = editor.clone();
        let surface = div()
            .id("spike-editor-surface")
            .key_context("BlockEditor")
            .track_focus(editor.read(cx).focus_handle())
            .w(px(width))
            .h(px(content_height))
            .rounded(px(7.0))
            .bg(rgba(0xffffffff))
            .capture_any_mouse_down(cx.listener(Self::on_surface_mouse_down))
            .on_mouse_move(cx.listener(Self::on_surface_mouse_move))
            .on_mouse_up(MouseButton::Left, cx.listener(Self::on_surface_mouse_up))
            .on_mouse_up_out(MouseButton::Left, cx.listener(Self::on_surface_mouse_up));

        let canvas = canvas(
            move |bounds, window, cx| {
                let _ = canvas_editor.update(cx, |editor, editor_cx| {
                    let previous_height = editor.layout().total_height();
                    editor.shape_visible_with_window(viewport_top, viewport_height, width, window);
                    editor.translate_layout(f32::from(bounds.origin.x), f32::from(bounds.origin.y));
                    let measured_height = editor.layout().total_height();
                    if (measured_height - previous_height).abs() > 0.01 {
                        editor_cx.notify();
                    }
                });
                canvas_editor.clone()
            },
            move |bounds, entity, window, cx| {
                let _ = crate::native_editor::render::paint_entity(entity, bounds, window, cx);
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
        let scroll_top = (-f32::from(self.scroll_handle.offset().y)).max(0.0);
        let surface_viewport_height = (viewport_height - SPIKE_EDITOR_TOP).max(1.0);
        let measured_height = self.editor.read(cx).layout().total_height();
        let content_height = measured_height.max(surface_viewport_height * 0.65);
        let viewport_top = (scroll_top - SPIKE_EDITOR_TOP).max(0.0);

        let primary_buttons = self
            .catalogue
            .primary_descriptors()
            .into_iter()
            .map(|descriptor| self.render_command_button(descriptor, false, cx))
            .collect::<Vec<_>>();
        let more_trigger = self.render_more_trigger(cx);
        let more_menu = self.render_more_menu(layout.left_inset, cx);
        let editor_surface = self.render_editor_surface(
            layout,
            content_height,
            viewport_top,
            surface_viewport_height,
            cx,
        );

        let toolbar = div()
            .id("evernote-native-spike-primary-toolbar")
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
        // Clipboard images are deliberately ignored until Task 6; text is
        // routed through the same transaction/history path as platform input.
        if let Some(text) = cx.read_from_clipboard().and_then(|item| item.text()) {
            run_editor_result(&paste_editor, window, cx, |editor| {
                editor.paste_plain_text(&text)
            });
        }
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

    fn redraw(cx: &mut VisualTestContext) {
        cx.update(|window, app| window.draw(app).clear());
        cx.run_until_parked();
    }

    fn build_view(window: &mut Window, cx: &mut Context<SpikeView>) -> SpikeView {
        let editor = cx.new(|cx| EditorCore::new(Document::from_paragraphs(["alpha", "beta"]), cx));
        editor.read(cx).focus_handle().focus(window);
        SpikeView {
            editor,
            catalogue: CommandCatalogue::default(),
            scroll_handle: ScrollHandle::new(),
            more_open: false,
            link_popover: None,
            pointer_anchor: None,
        }
    }

    fn build_long_view(window: &mut Window, cx: &mut Context<SpikeView>) -> SpikeView {
        let editor =
            cx.new(|cx| EditorCore::new(Document::from_paragraph("wrap ".repeat(360)), cx));
        editor.read(cx).focus_handle().focus(window);
        SpikeView {
            editor,
            catalogue: CommandCatalogue::default(),
            scroll_handle: ScrollHandle::new(),
            more_open: false,
            link_popover: None,
            pointer_anchor: None,
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
}
