//! Isolated native GPUI route for the Evernote-style editor spike.
//!
//! This module deliberately stops at the native editor entity. It does not
//! construct the ordinary workspace, updater, exporter, network client, or
//! sync services. The action names and default key bindings still come from
//! `components::actions` (the pinned Velotype donor path), while selection,
//! focus, transactions, and platform input remain owned by `EditorCore`.

use gpui::{
    App, AppContext, Bounds, Context, Entity, InteractiveElement, IntoElement, MouseButton,
    MouseDownEvent, ParentElement, Render, ScrollHandle, StatefulInteractiveElement, Styled,
    Window, WindowBounds, WindowHandle, WindowOptions, canvas, div, px, rgba, size,
};

use crate::components::{
    BlockDown, BoldSelection, Delete, DeleteBack, End, FocusNext, FocusPrev, Home, IndentBlock,
    ItalicSelection, MoveLeft, MoveRight, Newline, OutdentBlock, Redo, SelectAll,
    UnderlineSelection, Undo,
};
use crate::native_editor::commands::{
    CommandArgument, CommandCatalogue, CommandDescriptor, EditorCommand,
};
use crate::native_editor::core::EditorCore;
use crate::native_editor::model::{
    Affinity, BlockKind, DocPoint, Document, DocumentError, Selection,
};
use crate::native_editor::transaction::Transaction;

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

pub(crate) struct SpikeView {
    editor: Entity<EditorCore>,
    catalogue: CommandCatalogue,
    scroll_handle: ScrollHandle,
}

impl SpikeView {
    fn render_command_button(
        &self,
        descriptor: &'static CommandDescriptor,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        let editor_ref = self.editor.read(cx);
        let state = self.catalogue.state(descriptor.command, editor_ref);
        let command = descriptor.command;
        let editor = self.editor.clone();
        let catalogue = self.catalogue;
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
                        let argument = if command == EditorCommand::Link {
                            CommandArgument::LinkUrl("https://example.com".to_owned())
                        } else {
                            CommandArgument::None
                        };
                        run_command(&editor, catalogue, command, argument, window, cx);
                    },
                );
        }
        button.into_any_element()
    }

    fn render_editor_surface(
        &self,
        layout: SpikeLayout,
        content_height: f32,
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
            .on_mouse_down(MouseButton::Left, move |_event, window, cx| {
                // The donor element routes pointer-down through the focused
                // block. The native spike has one document-wide owner, so the
                // equivalent is to restore that same focus handle here.
                focus_editor(&editor, window, cx);
            });

        let canvas = canvas(
            move |bounds, window, cx| {
                let _ = canvas_editor.update(cx, |editor, _| {
                    editor.shape_visible_with_window(0.0, content_height, width, window);
                    editor.translate_layout(f32::from(bounds.origin.x), f32::from(bounds.origin.y));
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
        let block_count = self.editor.read(cx).document().block_count();
        let content_height = (block_count as f32 * 34.0 + 96.0).max(viewport_height * 0.65);

        let primary_buttons = self
            .catalogue
            .primary_descriptors()
            .into_iter()
            .map(|descriptor| self.render_command_button(descriptor, cx))
            .collect::<Vec<_>>();
        let more_buttons = self
            .catalogue
            .more_descriptors()
            .into_iter()
            .map(|descriptor| self.render_command_button(descriptor, cx))
            .collect::<Vec<_>>();
        let editor_surface = self.render_editor_surface(layout, content_height, cx);

        div()
            .id("evernote-native-spike")
            .size_full()
            .bg(rgba(0xf1f3f6ff))
            .child(
                div()
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
                    .child(
                        div()
                            .id("evernote-native-spike-primary-toolbar")
                            .w(px(layout.content_width))
                            .flex()
                            .flex_wrap()
                            .children(primary_buttons),
                    )
                    .child(
                        div()
                            .id("evernote-native-spike-more-toolbar")
                            .w(px(layout.content_width))
                            .flex()
                            .flex_wrap()
                            .children(more_buttons),
                    )
                    .child(editor_surface)
                    // Keep the final line scrollable into the visual center,
                    // matching the brief without adding a product-shell
                    // scrollbar or donor workspace service.
                    .pb(px(layout.bottom_padding)),
            )
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
    bind_mutation_action!(surface, editor, BlockDown, move_down);
    bind_mutation_action!(surface, editor, SelectAll, select_all);

    surface = surface.on_action(move |_action: &FocusPrev, window, _cx| {
        window.focus_prev();
    });
    surface = surface.on_action(move |_action: &FocusNext, window, _cx| {
        window.focus_next();
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
