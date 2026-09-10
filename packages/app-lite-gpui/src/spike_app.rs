//! Isolated native GPUI route for the Evernote-style editor spike.
//!
//! This module deliberately stops at the native editor entity. It does not
//! construct the ordinary workspace, updater, exporter, network client, or
//! sync services. The action names and default key bindings still come from
//! `components::actions` (the pinned Velotype donor path), while selection,
//! focus, transactions, and platform input remain owned by `EditorCore`.

use std::ops::Range;
use std::path::PathBuf;
use std::time::Instant;

use gpui::{
    App, AppContext, AsyncApp, AsyncWindowContext, Bounds, ClipboardItem, Context, DragMoveEvent,
    ElementInputHandler, Entity, EntityInputHandler, ExternalPaths, FocusHandle, FontWeight,
    InteractiveElement, IntoElement, KeyDownEvent, MouseButton, MouseDownEvent, MouseMoveEvent,
    MouseUpEvent, ParentElement, PathPromptOptions, Pixels, Point, Render, ScrollHandle,
    ShapedLine, SharedString, StatefulInteractiveElement, Styled, TextRun, UTF16Selection,
    WeakEntity, Window, WindowBounds, WindowHandle, WindowOptions, canvas, div, point, px, rgba,
    size, svg,
};
use unicode_segmentation::UnicodeSegmentation;

use crate::components::{
    BlockDown, BlockUp, BoldSelection, Copy, Cut, Delete, DeleteBack, End, FocusNext, FocusPrev,
    Home, IndentBlock, ItalicSelection, MoveLeft, MoveRight, Newline, OutdentBlock, Paste, Redo,
    SelectAll, SelectEnd, SelectHome, SelectLeft, SelectRight, UnderlineSelection, Undo,
    WordSelectLeft, WordSelectRight,
};
use crate::native_editor::chrome::{
    EVERNOTE_GREEN, TitleInput, ToolbarPlacement, editor_chrome_metrics, toolbar_placement,
};
use crate::native_editor::commands::{
    CommandArgument, CommandCatalogue, CommandDescriptor, CommandError, EditorCommand,
};
use crate::native_editor::core::EditorCore;
use crate::native_editor::diagnostics::{Diagnostics, FixedHistogram};
use crate::native_editor::fixtures::{
    FixtureKind, build_document, populate_typical_images, typical_image_count,
};
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

#[cfg(any(target_os = "macos", test))]
const SPIKE_MAXIMUM_DRAWABLE_COUNT: usize = 2;

#[cfg(any(target_os = "macos", test))]
fn validate_spike_drawable_pool_limit(actual: usize) -> Result<(), String> {
    if actual == SPIKE_MAXIMUM_DRAWABLE_COUNT {
        Ok(())
    } else {
        Err(format!(
            "Task 7 CAMetalLayer maximumDrawableCount must be {SPIKE_MAXIMUM_DRAWABLE_COUNT}; got {actual}"
        ))
    }
}

#[cfg(target_os = "macos")]
fn configure_spike_drawable_pool_limit(window: &Window) -> Result<(), String> {
    use cocoa::base::{id, nil};
    use objc::{msg_send, sel, sel_impl};
    use raw_window_handle::{HasWindowHandle, RawWindowHandle};

    let handle = HasWindowHandle::window_handle(window)
        .map_err(|error| format!("Task 7 could not read its native window handle: {error}"))?;
    let RawWindowHandle::AppKit(handle) = handle.as_raw() else {
        return Err("Task 7 expected an AppKit native window handle on macOS".into());
    };
    let native_view = handle.ns_view.as_ptr() as id;
    // SAFETY: GPUI invokes this only from its macOS main-thread window-build closure.
    // The borrowed Window keeps the GPUI-owned NSView alive, and the fixed macOS Metal
    // backend installs its CAMetalLayer as that view's backing layer.
    let layer: id = unsafe { msg_send![native_view, layer] };
    if layer == nil {
        return Err("Task 7 native window has no CAMetalLayer before its first draw".into());
    }

    // SAFETY: `layer` is the live CAMetalLayer established above. Both selectors are
    // standard CAMetalLayer accessors and execute synchronously on the same main thread.
    let actual = unsafe {
        let _: () = msg_send![layer, setMaximumDrawableCount: SPIKE_MAXIMUM_DRAWABLE_COUNT];
        msg_send![layer, maximumDrawableCount]
    };
    validate_spike_drawable_pool_limit(actual)
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SpikeLaunchOptions {
    pub fixture: FixtureKind,
    pub ready_file: PathBuf,
    pub diagnostics_file: PathBuf,
    pub run_id: String,
    pub binary_sha256: String,
}

impl SpikeLaunchOptions {
    fn ready_marker(&self) -> String {
        format!("task7-ready|{}|{}\n", self.run_id, self.binary_sha256)
    }
}

pub fn measurement_options_requested(args: &[String]) -> bool {
    args.iter().any(|arg| {
        matches!(
            arg.as_str(),
            "--fixture" | "--ready-file" | "--diagnostics-file"
        )
    })
}

/// Help/version are global options. They must be recognized before the main
/// parser consumes a following token as the value of `--fixture` or one of
/// the measurement paths.
pub fn global_help_or_version(args: &[String]) -> Option<&'static str> {
    args.iter().find_map(|arg| match arg.as_str() {
        "--help" | "-h" => Some("help"),
        "--version" | "-v" | "-V" => Some("version"),
        _ => None,
    })
}

pub fn parse_spike_options(args: &[String]) -> Result<SpikeLaunchOptions, String> {
    let mut spike = false;
    let mut fixture = None;
    let mut ready_file = None;
    let mut diagnostics_file = None;
    let mut index = 0;
    while index < args.len() {
        match args[index].as_str() {
            "--evernote-spike" if !spike => spike = true,
            "--evernote-spike" => return Err("duplicate --evernote-spike".into()),
            "--fixture" => {
                index += 1;
                let value = args
                    .get(index)
                    .ok_or_else(|| "--fixture requires empty|typical|long".to_owned())?;
                if fixture.is_some() {
                    return Err("duplicate --fixture".into());
                }
                fixture = Some(FixtureKind::parse(value)?);
            }
            "--ready-file" => {
                index += 1;
                let value = args
                    .get(index)
                    .ok_or_else(|| "--ready-file requires an absolute path".to_owned())?;
                let path = PathBuf::from(value);
                if !path.is_absolute() {
                    return Err("--ready-file must be absolute".into());
                }
                if ready_file.replace(path).is_some() {
                    return Err("duplicate --ready-file".into());
                }
            }
            "--diagnostics-file" => {
                index += 1;
                let value = args
                    .get(index)
                    .ok_or_else(|| "--diagnostics-file requires an absolute path".to_owned())?;
                let path = PathBuf::from(value);
                if !path.is_absolute() {
                    return Err("--diagnostics-file must be absolute".into());
                }
                if diagnostics_file.replace(path).is_some() {
                    return Err("duplicate --diagnostics-file".into());
                }
            }
            option => return Err(format!("unknown Task 7 option '{option}'")),
        }
        index += 1;
    }
    if !spike {
        return Err("--evernote-spike is required".into());
    }
    Ok(SpikeLaunchOptions {
        fixture: fixture.ok_or_else(|| "--fixture is required".to_owned())?,
        ready_file: ready_file.ok_or_else(|| "--ready-file is required".to_owned())?,
        diagnostics_file: diagnostics_file
            .ok_or_else(|| "--diagnostics-file is required".to_owned())?,
        run_id: std::env::var("TASK7_RUN_ID").unwrap_or_else(|_| "unbound-test".into()),
        binary_sha256: std::env::var("TASK7_BINARY_SHA256")
            .unwrap_or_else(|_| "unbound-test".into()),
    })
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
    open_with_options(cx, None)
}

pub(crate) fn open_with_options(
    cx: &mut App,
    options: Option<SpikeLaunchOptions>,
) -> WindowHandle<SpikeView> {
    let bounds = Bounds::centered(None, size(px(1200.0), px(820.0)), cx);
    let fixture = options.as_ref().map(|options| options.fixture);
    let options_for_window = options.clone();
    let handle = cx
        .open_window(
            WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(bounds)),
                ..WindowOptions::default()
            },
            move |window, cx| {
                #[cfg(target_os = "macos")]
                configure_spike_drawable_pool_limit(window).unwrap_or_else(|error| {
                    panic!("Task 7 must configure its CAMetalLayer before the first draw: {error}")
                });
                let document = fixture.map_or_else(sample_document, build_document);
                let editor = cx.new(|cx| EditorCore::new(document, cx));
                if fixture == Some(FixtureKind::Typical) {
                    editor
                        .update(cx, |editor, _| populate_typical_images(editor))
                        .expect("typical fixture image insertion");
                    debug_assert_eq!(typical_image_count(), 10);
                }
                let image_cache = BudgetedImageCache::new_entity(cx, DECODED_IMAGE_CACHE_BUDGET);
                let title = cx.new(|cx| TitleInput::new("会议记录".into(), cx));
                cx.new(|_| SpikeView {
                    editor,
                    title,
                    image_cache: Some(image_cache),
                    catalogue: CommandCatalogue::default(),
                    scroll_handle: ScrollHandle::new(),
                    more_open: false,
                    link_popover: None,
                    pointer_anchor: None,
                    drop_point: None,
                    more_trigger_bounds: None,
                    measurement: options_for_window
                        .map(MeasurementRuntime::new)
                        .map(Box::new),
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

#[derive(Clone)]
struct MeasurementRuntime {
    options: SpikeLaunchOptions,
    first_frame_painted: bool,
    workload_started: bool,
    workload_complete: bool,
    viewport_shift_index: usize,
    viewport_reset_requested: bool,
    viewport_reset_completed: bool,
    viewport_complete: bool,
    viewport_frames_requested: u32,
    viewport_frames_completed: u32,
    viewport_frame_pending: bool,
    viewport_reset_frames_requested: u32,
    viewport_reset_frames_completed: u32,
    viewport_reset_frame_pending: bool,
    render_sample_count: u32,
    viewport_reset_paint_count: u32,
    ready_written: bool,
    render_start: Option<Instant>,
    transaction_histogram: FixedHistogram,
    render_histogram: FixedHistogram,
}

impl MeasurementRuntime {
    fn new(options: SpikeLaunchOptions) -> Self {
        Self {
            options,
            first_frame_painted: false,
            workload_started: false,
            workload_complete: false,
            viewport_shift_index: 0,
            viewport_reset_requested: false,
            viewport_reset_completed: false,
            viewport_complete: false,
            viewport_frames_requested: 0,
            viewport_frames_completed: 0,
            viewport_frame_pending: false,
            viewport_reset_frames_requested: 0,
            viewport_reset_frames_completed: 0,
            viewport_reset_frame_pending: false,
            render_sample_count: 0,
            viewport_reset_paint_count: 0,
            ready_written: false,
            render_start: None,
            transaction_histogram: FixedHistogram::default(),
            render_histogram: FixedHistogram::default(),
        }
    }

    fn ready_prerequisites(&self, cache_settled: bool) -> bool {
        self.workload_complete
            && self.viewport_complete
            && self.viewport_frames_requested == 120
            && self.viewport_frames_completed == 120
            && self.viewport_reset_frames_requested == 1
            && self.viewport_reset_frames_completed == 1
            && self.viewport_reset_completed
            && self.render_sample_count == 120
            && self.viewport_reset_paint_count == 1
            && cache_settled
    }

    fn request_viewport_frame(&mut self) {
        self.viewport_frames_requested = self.viewport_frames_requested.saturating_add(1);
        self.viewport_frame_pending = true;
    }

    fn request_viewport_reset_frame(&mut self) {
        self.viewport_reset_frames_requested =
            self.viewport_reset_frames_requested.saturating_add(1);
        self.viewport_reset_frame_pending = true;
    }

    fn complete_viewport_frame(&mut self) -> (bool, bool) {
        let shift_frame = self.viewport_frame_pending;
        let reset_frame = self.viewport_reset_frame_pending;
        if self.viewport_frame_pending {
            self.viewport_frames_completed = self.viewport_frames_completed.saturating_add(1);
            self.viewport_frame_pending = false;
        }
        if self.viewport_reset_frame_pending {
            self.viewport_reset_frames_completed =
                self.viewport_reset_frames_completed.saturating_add(1);
            self.viewport_reset_frame_pending = false;
            self.viewport_reset_completed = true;
        }
        (shift_frame, reset_frame)
    }

    fn record_paint(&mut self, shift_frame: bool, reset_frame: bool, elapsed_us: u64) {
        if shift_frame {
            self.render_histogram.observe_us(elapsed_us);
            self.render_sample_count = self.render_sample_count.saturating_add(1);
        }
        if reset_frame {
            self.viewport_reset_paint_count = self.viewport_reset_paint_count.saturating_add(1);
        }
    }
}

fn measurement_workload_needs_frame(runtime: Option<&MeasurementRuntime>) -> bool {
    runtime.is_some_and(|runtime| !runtime.workload_complete)
}

/// Small real text-input owner for the link popover.  The editor selection
/// never moves into this entity; only the URL field owns focus while the
/// popover is open.  This follows the pinned donor's `ElementInputHandler`
/// bridge so AppKit/IME replacement edits the field rather than a fake label.
struct LinkPopover {
    focus: FocusHandle,
    anchor: Option<Point<Pixels>>,
    text: String,
    selection: Range<usize>,
    reversed: bool,
    marked: Option<Range<usize>>,
    last_bounds: Option<Bounds<Pixels>>,
    last_layout: Option<ShapedLine>,
    invalid: bool,
}

impl LinkPopover {
    fn new(initial: String, cx: &mut Context<Self>) -> Self {
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
    title: Entity<TitleInput>,
    image_cache: Option<Entity<BudgetedImageCache>>,
    catalogue: CommandCatalogue,
    scroll_handle: ScrollHandle,
    more_open: bool,
    link_popover: Option<Entity<LinkPopover>>,
    pointer_anchor: Option<DocPoint>,
    drop_point: Option<DocPoint>,
    more_trigger_bounds: Option<Bounds<Pixels>>,
    measurement: Option<Box<MeasurementRuntime>>,
}

impl SpikeView {
    fn render_title_input(&self, width: f32, cx: &mut Context<Self>) -> gpui::AnyElement {
        let title = self.title.clone();
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
        div()
            .id("evernote-note-title")
            .debug_selector(|| "evernote-note-title".to_owned())
            .w(px(width))
            .h(px(40.0))
            .key_context("EvernoteTitle")
            .track_focus(self.title.read(cx).focus_handle())
            .on_mouse_down(MouseButton::Left, cx.listener(Self::on_title_mouse_down))
            .on_mouse_move(cx.listener(Self::on_title_mouse_move))
            .on_mouse_up(MouseButton::Left, cx.listener(Self::on_title_mouse_up))
            .on_key_down(cx.listener(Self::on_title_key_down))
            .child(canvas)
            .into_any_element()
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
        self.title.update(cx, |title, title_cx| {
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
        self.title.update(cx, |title, title_cx| {
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
        self.title
            .update(cx, |title, _| title.end_pointer_selection());
        cx.stop_propagation();
    }

    fn on_title_key_down(
        &mut self,
        event: &KeyDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let key = event.keystroke.key.as_str();
        let modifiers = event.keystroke.modifiers;
        let secondary = modifiers.secondary();
        if TitleInput::moves_focus_to_body_for(key) {
            focus_editor(&self.editor, window, cx);
            cx.stop_propagation();
            return;
        }
        let handled = match key {
            "backspace" => {
                self.title.update(cx, |title, title_cx| {
                    title.delete_backward();
                    title_cx.notify();
                });
                true
            }
            "delete" => {
                self.title.update(cx, |title, title_cx| {
                    title.delete_forward();
                    title_cx.notify();
                });
                true
            }
            "left" => {
                self.title.update(cx, |title, title_cx| {
                    title.move_horizontal(false, modifiers.shift);
                    title_cx.notify();
                });
                true
            }
            "right" => {
                self.title.update(cx, |title, title_cx| {
                    title.move_horizontal(true, modifiers.shift);
                    title_cx.notify();
                });
                true
            }
            "home" => {
                self.title.update(cx, |title, title_cx| {
                    title.move_to_edge(false, modifiers.shift);
                    title_cx.notify();
                });
                true
            }
            "end" => {
                self.title.update(cx, |title, title_cx| {
                    title.move_to_edge(true, modifiers.shift);
                    title_cx.notify();
                });
                true
            }
            "a" if secondary => {
                self.title.update(cx, |title, title_cx| {
                    title.select_all();
                    title_cx.notify();
                });
                true
            }
            "c" if secondary => {
                let text = self.title.read(cx).selected_text().to_owned();
                cx.write_to_clipboard(ClipboardItem::new_string(text));
                true
            }
            "x" if secondary => {
                let text = self.title.read(cx).selected_text().to_owned();
                if !text.is_empty() {
                    cx.write_to_clipboard(ClipboardItem::new_string(text));
                    self.title.update(cx, |title, title_cx| {
                        title.delete_forward();
                        title_cx.notify();
                    });
                }
                true
            }
            "v" if secondary => {
                self.title
                    .update(cx, |title, title_cx| title.paste_from_clipboard(title_cx));
                true
            }
            _ => false,
        };
        if handled {
            cx.stop_propagation();
        }
    }

    fn start_measurement(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(runtime) = self.measurement.as_mut() else {
            return;
        };
        if runtime.workload_started {
            return;
        }
        runtime.workload_started = true;
        let options = runtime.options.clone();
        let editor = self.editor.clone();
        let weak_view: WeakEntity<Self> = cx.entity().downgrade();
        window
            .spawn(cx, async move |cx: &mut AsyncWindowContext| {
                match cx.update(|window, app| {
                    run_measurement_workload(&editor, options.fixture, window, app)
                }) {
                    Ok(Some(result)) => {
                        let _ = cx.update(|window, app| {
                            let delivered = weak_view.update(app, |view, view_cx| {
                                view.deliver_measurement_workload(result, window, view_cx)
                            });
                            if delivered.is_ok_and(|advanced| advanced) {
                                window.refresh();
                            }
                        });
                        // `on_next_frame` only owns the following entity
                        // notification; the async delivery must explicitly ask
                        // GPUI to drive the dirty window into that frame.
                        let _ = cx.refresh();
                    }
                    Ok(None) => eprintln!(
                        "Task 7 measurement workload failed to produce a validated report"
                    ),
                    Err(error) => eprintln!(
                        "Task 7 measurement workload could not update its owning window: {error}"
                    ),
                }
            })
            .detach();
    }

    fn measurement_workload_finished(&mut self, report: WorkloadReport) {
        let Some(runtime) = self.measurement.as_mut() else {
            return;
        };
        if report.apply_undo_pairs != 500
            || report.changed_node_assertions != 500
            || report.local_restoration_assertions != 500
            || report.full_document_verifications != 1
        {
            return;
        }
        runtime.transaction_histogram = report.transaction_histogram;
        runtime.workload_complete = true;
    }

    fn deliver_measurement_workload(
        &mut self,
        report: WorkloadReport,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        self.measurement_workload_finished(report);
        let workload_complete = self
            .measurement
            .as_ref()
            .is_some_and(|runtime| runtime.workload_complete);
        if workload_complete {
            self.advance_measurement_frame(window, cx);
        }
        workload_complete
    }

    /// Drive each viewport shift through a separate GPUI animation frame. A
    /// synchronous loop of `ScrollHandle::set_offset` calls would be coalesced
    /// by GPUI and would not measure the production layout+paint lifecycle.
    fn advance_measurement_frame(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(runtime) = self.measurement.as_ref() else {
            return;
        };
        if !runtime.workload_complete || runtime.viewport_complete {
            return;
        }
        let reset_requested = runtime.viewport_reset_requested;
        let shift_index = runtime.viewport_shift_index;
        if shift_index < 120 {
            let total_height = self.editor.read(cx).layout().total_height();
            let max_top = (total_height - 520.0).max(0.0);
            let fraction = shift_index as f32 / 119.0;
            let top = max_top * fraction;
            if let Some(runtime) = self.measurement.as_mut() {
                runtime.render_start = Some(Instant::now());
            }
            self.scroll_handle.set_offset(point(px(0.0), px(-top)));
            if let Some(runtime) = self.measurement.as_mut() {
                runtime.viewport_shift_index += 1;
                runtime.request_viewport_frame();
            }
            cx.on_next_frame(window, |_, _, cx| cx.notify());
            cx.notify();
        } else if !reset_requested {
            if let Some(runtime) = self.measurement.as_mut() {
                runtime.render_start = Some(Instant::now());
            }
            self.scroll_handle.set_offset(point(px(0.0), px(0.0)));
            if let Some(runtime) = self.measurement.as_mut() {
                runtime.viewport_reset_requested = true;
                runtime.request_viewport_reset_frame();
            }
            cx.on_next_frame(window, |_, _, cx| cx.notify());
            cx.notify();
        } else {
            if let Some(runtime) = self.measurement.as_mut()
                && runtime.viewport_reset_completed
            {
                runtime.viewport_complete = true;
            }
            cx.notify();
        }
    }

    fn on_external_paths_drop(
        &mut self,
        paths: &ExternalPaths,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let paths = paths.paths().to_vec();
        let drop_point = self.drop_point.take();
        run_editor_result(&self.editor, window, cx, |editor| {
            apply_drop_paths_at(editor, &paths, drop_point)
        });
        focus_editor(&self.editor, window, cx);
    }

    fn on_external_paths_drag_move(
        &mut self,
        event: &DragMoveEvent<ExternalPaths>,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let point = self.editor.update(cx, |editor, _| {
            editor.point_from_layout(event.event.position)
        });
        if self.drop_point != point {
            self.drop_point = point;
            cx.notify();
        }
    }

    fn retry_image_by_id(
        &mut self,
        resource_id: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let retry_path = self.editor.update(cx, |editor, _| {
            editor
                .retry_image_resource(resource_id)
                .then(|| editor.image_source_path(resource_id).map(PathBuf::from))
                .flatten()
        });
        if let Some(path) = retry_path {
            if let Some(cache) = self.image_cache.as_ref() {
                cache.update(cx, |cache, cache_cx| {
                    cache.invalidate(&gpui::Resource::from(path), window, cache_cx);
                });
            }
            cx.notify();
        }
    }

    fn open_link_popover(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.open_link_popover_at(None, window, cx);
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
        let popover = cx.new(|cx| {
            let popover = LinkPopover::new(initial, cx);
            match anchor {
                Some(anchor) => popover.anchored_at(anchor),
                None => popover,
            }
        });
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
        let estimated_height = if invalid { 144.0 } else { 126.0 };
        let anchor = anchor.unwrap_or_else(|| point(content_mask.left(), content_mask.top()));
        let left = f32::from(anchor.x)
            .max(mask_left)
            .min((mask_right - width).max(mask_left));
        let below = f32::from(anchor.y) + 16.0;
        let top = if below + estimated_height <= mask_bottom {
            below
        } else {
            (f32::from(anchor.y) - estimated_height - 16.0).max(mask_top)
        };
        let canvas_popover = popover.clone();
        let paint_popover = popover.clone();
        let click_popover = popover.clone();
        let view = cx.entity();
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
        .h(px(40.0));
        div()
            .id("evernote-link-popover")
            .debug_selector(|| "evernote-link-popover".to_owned())
            .absolute()
            .top(px(top))
            .left(px(left))
            .w(px(width))
            .flex()
            .flex_col()
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
            .on_action(cx.listener(Self::submit_link))
            .on_action(cx.listener(Self::cancel_link))
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
                    .child(
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
                                let _ = view.update(cx, |view, view_cx| {
                                    view.cancel_link(&CancelLink, window, view_cx)
                                });
                            })
                            .child("取消"),
                    )
                    .child({
                        let view = cx.entity();
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
                                let _ = view.update(cx, |view, view_cx| {
                                    view.submit_link(&SubmitLink, window, view_cx)
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
        let editor_ref = self.editor.read(cx);
        let state = self.catalogue.state(descriptor.command, editor_ref);
        let command = descriptor.command;
        let editor = self.editor.clone();
        let catalogue = self.catalogue;
        let view = cx.entity();
        let background = match state.toggle {
            crate::native_editor::commands::ToggleState::On
            | crate::native_editor::commands::ToggleState::Mixed => rgba(EVERNOTE_GREEN),
            crate::native_editor::commands::ToggleState::Off => rgba(0x00000000),
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
                        if matches!(
                            state.toggle,
                            crate::native_editor::commands::ToggleState::On
                                | crate::native_editor::commands::ToggleState::Mixed
                        ) {
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
                        if matches!(
                            state.toggle,
                            crate::native_editor::commands::ToggleState::On
                                | crate::native_editor::commands::ToggleState::Mixed
                        ) {
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
            button = button
                .cursor_pointer()
                // A div is deliberately used instead of a focusable button:
                // pointer-down preserves the editor's selection and focus just
                // like the donor block toolbar.
                .on_mouse_down(
                    MouseButton::Left,
                    move |event: &MouseDownEvent, window: &mut Window, cx: &mut App| {
                        cx.stop_propagation();
                        if command == EditorCommand::InsertImage {
                            if let Some(window_handle) =
                                window.window_handle().downcast::<SpikeView>()
                            {
                                prompt_for_image_path(window_handle, cx);
                            }
                        } else if command == EditorCommand::Link {
                            let _ = view.update(cx, |view, view_cx| {
                                view.open_link_popover_at(Some(event.position), window, view_cx)
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
                let _ = view.update(cx, |view, view_cx| {
                    view.more_open = !view.more_open;
                    focus_editor(&view.editor, window, view_cx);
                    view_cx.notify();
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
                    .find(|d| d.command == command)
            })
            .map(|descriptor| self.render_command_button(descriptor, true, cx))
            .collect::<Vec<_>>();
        Some(
            div()
                .id("evernote-native-spike-more-menu")
                .debug_selector(|| "evernote-native-spike-more-menu".to_owned())
                .absolute()
                .top(px(top.max(mask_top)))
                .left(px(left))
                .w(px(menu_width))
                .max_h(px(300.0_f32.min(max_height.max(1.0))))
                .overflow_y_scroll()
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
        if self.more_open || self.link_popover.is_some() {
            cx.propagate();
            return;
        }
        if !event.modifiers.shift {
            let failed_resource = self.editor.update(cx, |editor, _| {
                let resource_id = editor.image_resource_at_layout(event.position)?;
                (editor.image_state(&resource_id)
                    == Some(crate::native_editor::images::ImageNodeState::Failed))
                .then_some(resource_id)
            });
            if let Some(resource_id) = failed_resource {
                self.retry_image_by_id(&resource_id, window, cx);
                focus_editor(&self.editor, window, cx);
                cx.stop_propagation();
                return;
            }
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
        if self.link_popover.is_some() {
            // The modal backdrop owns Link's outside-click dismissal. Do not
            // compete with its mounted input or action controls.
            cx.propagate();
            return;
        }
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
        let measurement_view = cx.entity();
        let paint_measurement_view = measurement_view.clone();
        let mut surface = div()
            .id("spike-editor-surface")
            .debug_selector(|| "spike-editor-surface".to_owned())
            .key_context("BlockEditor")
            .track_focus(editor.read(cx).focus_handle())
            .w(px(width))
            .h(px(content_height))
            .rounded(px(7.0))
            .bg(rgba(0xffffffff))
            .can_drop(|dragged, _window, _cx| dragged.is::<ExternalPaths>())
            .on_drag_move::<ExternalPaths>(cx.listener(Self::on_external_paths_drag_move))
            .on_drop::<ExternalPaths>(cx.listener(Self::on_external_paths_drop))
            .on_key_down(cx.listener(Self::on_surface_key_down))
            .on_mouse_move(cx.listener(Self::on_surface_mouse_move))
            .on_mouse_up(MouseButton::Left, cx.listener(Self::on_surface_mouse_up))
            .on_mouse_up_out(MouseButton::Left, cx.listener(Self::on_surface_mouse_up));

        if self.link_popover.is_none() {
            surface = surface.capture_any_mouse_down(cx.listener(Self::on_surface_mouse_down));
        }

        let canvas = canvas(
            move |bounds, window, cx| {
                let _ = measurement_view.update(cx, |view, _| {
                    if let Some(runtime) = view.measurement.as_mut() {
                        if runtime.render_start.is_none() {
                            runtime.render_start = Some(Instant::now());
                        }
                    }
                });
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
                let fallback_started = Instant::now();
                let _ = paint_measurement_view.update(cx, |view, view_cx| {
                    let (first_frame, render_started, shift_frame, reset_frame) = {
                        let Some(runtime) = view.measurement.as_mut() else {
                            return;
                        };
                        let (shift_frame, reset_frame) = runtime.complete_viewport_frame();
                        if reset_frame {
                            runtime.viewport_reset_completed = true;
                        }
                        let render_started = runtime.render_start.take();
                        let first_frame = !runtime.first_frame_painted;
                        runtime.first_frame_painted = true;
                        (first_frame, render_started, shift_frame, reset_frame)
                    };
                    let elapsed = render_started
                        .unwrap_or(fallback_started)
                        .elapsed()
                        .as_micros()
                        .min(u128::from(u64::MAX)) as u64;
                    if let Some(runtime) = view.measurement.as_mut() {
                        runtime.record_paint(shift_frame, reset_frame, elapsed);
                    }
                    if first_frame {
                        view.start_measurement(window, view_cx);
                    }
                    view.advance_measurement_frame(window, view_cx);
                    let cache_settled = view
                        .image_cache
                        .as_ref()
                        .is_none_or(|cache| cache.read(view_cx).is_settled());
                    let ready_prerequisites = view
                        .measurement
                        .as_ref()
                        .is_some_and(|runtime| runtime.ready_prerequisites(cache_settled));
                    let ready_written = view
                        .measurement
                        .as_ref()
                        .is_some_and(|runtime| runtime.ready_written);
                    if ready_prerequisites && !ready_written {
                        let Some(runtime) = view.measurement.as_ref() else {
                            return;
                        };
                        let transaction_p95 = runtime.transaction_histogram.p95_us();
                        let render_p95 = runtime.render_histogram.p95_us();
                        let diagnostics_path = runtime.options.diagnostics_file.clone();
                        let ready_path = runtime.options.ready_file.clone();
                        let diagnostics = Diagnostics::from_values(
                            view.image_cache
                                .as_ref()
                                .map_or(0, |cache| cache.read(view_cx).peak_accounted_bytes()),
                            view.editor.read(view_cx).layout_peak_accounted_bytes(),
                            view.editor.read(view_cx).history_used_bytes(),
                            transaction_p95,
                            render_p95,
                        );
                        if let Err(error) = diagnostics.write_atomic(&diagnostics_path) {
                            eprintln!("failed to write Task 7 diagnostics: {error}");
                            return;
                        }
                        let marker = runtime.options.ready_marker();
                        if let Err(error) =
                            Diagnostics::write_atomic_bytes(&ready_path, marker.as_bytes())
                        {
                            eprintln!("failed to write Task 7 ready marker: {error}");
                            let _ = std::fs::remove_file(&ready_path);
                            return;
                        }
                        if let Some(runtime) = view.measurement.as_mut() {
                            runtime.ready_written = true;
                        }
                        view_cx.notify();
                    }
                    if measurement_workload_needs_frame(view.measurement.as_deref()) {
                        window.request_animation_frame();
                    }
                });
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
        let chrome = editor_chrome_metrics(viewport_width, viewport_height);
        // The title and document are one writing column at every width. The
        // legacy shell computed the body from a separate 64pt inset, which
        // shifted the document 8pt left of the title below 696pt.
        let mut layout = layout_for_viewport(viewport_width, viewport_height);
        layout.content_width = chrome.body_width;
        let measured_height = self.editor.read(cx).layout().total_height();
        let content_mask = window.content_mask().bounds;
        let content_height = measured_height.max(f32::from(content_mask.size.height) * 0.65);

        let placement = toolbar_placement(chrome.header_width);
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
            .h(px(32.0))
            .child(more_measure)
            .child(more_trigger)
            .into_any_element();
        let more_menu =
            self.render_more_menu(self.more_trigger_bounds, content_mask, &placement, cx);
        let editor_surface = self.render_editor_surface(layout, content_height, cx);

        let toolbar = div()
            .id("evernote-native-spike-primary-toolbar")
            .debug_selector(|| "evernote-native-spike-primary-toolbar".to_owned())
            .relative()
            .w(px(chrome.header_width))
            .flex()
            .flex_none()
            .h(px(chrome.toolbar_height))
            .items_center()
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
                    .w(px(chrome.header_width))
                    .pt(px(26.0))
                    .pb(px(12.0))
                    .text_size(px(13.0))
                    .text_color(rgba(0x667085ff))
                    .child("本地资料库  ›  笔记"),
            )
            .child(
                div()
                    .w(px(chrome.header_width))
                    .pl(px(chrome.body_left_in_header))
                    .child(self.render_title_input(chrome.body_width, cx)),
            )
            .child(
                div()
                    .w(px(chrome.header_width))
                    .pl(px(chrome.body_left_in_header))
                    .pt(px(4.0))
                    .pb(px(18.0))
                    .text_size(px(13.0))
                    .text_color(rgba(0x667085ff))
                    .child("更新 刚刚"),
            )
            .child(toolbar)
            .child(editor_surface)
            .pb(px(chrome.bottom_padding));

        let mut root = div()
            .id("evernote-native-spike")
            .debug_selector(|| "evernote-native-spike".to_owned())
            .size_full()
            .relative()
            .bg(rgba(0xf6f5f1ff))
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
            let view = cx.entity();
            root = root
                .child(
                    div()
                        .absolute()
                        .top(px(0.0))
                        .left(px(0.0))
                        .size_full()
                        .on_mouse_down(MouseButton::Left, move |_event, window, cx| {
                            cx.stop_propagation();
                            let _ = view.update(cx, |view, view_cx| {
                                view.cancel_link(&CancelLink, window, view_cx);
                            });
                        }),
                )
                .child(self.render_link_popover(popover, content_mask, cx));
        }
        root
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

struct WorkloadReport {
    transaction_histogram: FixedHistogram,
    apply_undo_pairs: u32,
    changed_node_assertions: u32,
    local_restoration_assertions: u32,
    full_document_verifications: u32,
}

fn run_measurement_workload(
    editor: &Entity<EditorCore>,
    _fixture: FixtureKind,
    window: &mut Window,
    cx: &mut App,
) -> Option<WorkloadReport> {
    const VIEWPORT_HEIGHT: f32 = 520.0;
    const CONTENT_WIDTH: f32 = 680.0;
    editor.update(cx, |editor, _| {
        editor.shape_visible_with_window(0.0, VIEWPORT_HEIGHT, CONTENT_WIDTH, window);
    });
    run_edit_undo_pairs(editor, 500, true, cx)
}

/// Uses GPUI's native single-file prompt. Cancellation is deliberately a
/// no-op: the editor is neither changed nor moved through history.
fn prompt_for_image_path(window_handle: WindowHandle<SpikeView>, cx: &mut App) {
    let prompt = cx.prompt_for_paths(PathPromptOptions {
        files: true,
        directories: false,
        multiple: false,
        prompt: Some("选择图片".into()),
    });
    cx.spawn(async move |cx| {
        let completion = match prompt.await {
            Ok(Ok(Some(paths))) => paths
                .into_iter()
                .next()
                .map(ImagePickerCompletion::Selected)
                .unwrap_or(ImagePickerCompletion::Cancelled),
            _ => ImagePickerCompletion::Cancelled,
        };
        deliver_image_picker_completion(window_handle, completion, cx);
    })
    .detach();
}

enum ImagePickerCompletion {
    Cancelled,
    Selected(PathBuf),
}

/// Delivers a settled platform picker result to the exact spike window that
/// opened it. If that window has closed while the native panel was visible,
/// `WindowHandle::update` fails harmlessly and the completion is discarded.
fn deliver_image_picker_completion(
    window_handle: WindowHandle<SpikeView>,
    completion: ImagePickerCompletion,
    cx: &mut AsyncApp,
) {
    let _ = cx.update(move |app| {
        let _ = window_handle.update(app, |view, window, view_cx| {
            complete_image_picker_in_view(view, completion, window, view_cx);
        });
    });
}

/// Route the platform picker completion through the owning shell entity.
/// EditorCore owns the transaction itself, but SpikeView owns the canvas
/// dimensions and scroll extent which must be relaid out after an image node
/// changes the document's measured height.
fn complete_image_picker_in_view(
    view: &mut SpikeView,
    completion: ImagePickerCompletion,
    window: &mut Window,
    cx: &mut Context<SpikeView>,
) {
    if let Err(error) = complete_image_picker(&view.editor, view.catalogue, completion, window, cx)
    {
        eprintln!("spike image command failed: {error}");
    }
    cx.notify();
}

/// This is the selected-image half of the completion seam. Cancellation is
/// represented explicitly by `ImagePickerCompletion::Cancelled` and leaves
/// focus untouched; a selected image is a real editor transaction and
/// deliberately returns focus to the body for continued writing.
fn apply_selected_image_path(
    editor: &Entity<EditorCore>,
    catalogue: CommandCatalogue,
    path: PathBuf,
    window: &mut Window,
    cx: &mut App,
) -> Result<(), CommandError> {
    editor.update(cx, |editor, editor_cx| -> Result<(), CommandError> {
        catalogue.execute(
            EditorCommand::InsertImage,
            CommandArgument::ImagePath(path),
            editor,
        )?;
        editor_cx.notify();
        Ok(())
    })?;
    focus_editor(editor, window, cx);
    Ok(())
}

fn complete_image_picker(
    editor: &Entity<EditorCore>,
    catalogue: CommandCatalogue,
    completion: ImagePickerCompletion,
    window: &mut Window,
    cx: &mut App,
) -> Result<(), CommandError> {
    match completion {
        ImagePickerCompletion::Cancelled => Ok(()),
        ImagePickerCompletion::Selected(path) => {
            apply_selected_image_path(editor, catalogue, path, window, cx)
        }
    }
}

fn run_edit_undo_pairs(
    editor: &Entity<EditorCore>,
    iterations: u32,
    visible_only: bool,
    cx: &mut App,
) -> Option<WorkloadReport> {
    let baseline = editor.read(cx).document().semantic_snapshot();
    let baseline_block_count = editor.read(cx).document().block_count();
    let mut report = WorkloadReport {
        transaction_histogram: FixedHistogram::default(),
        apply_undo_pairs: 0,
        changed_node_assertions: 0,
        local_restoration_assertions: 0,
        full_document_verifications: 0,
    };

    for _ in 0..iterations {
        let outcome = editor.update(cx, |editor, _| {
            let point = visible_only
                .then(|| {
                    editor.layout().visible().iter().find_map(|layout| {
                        editor
                            .document()
                            .block(layout.node_id)
                            .and_then(|block| block.content.as_text().map(|_| block.id))
                    })
                })
                .flatten();
            let point = point.or_else(|| {
                editor
                    .document()
                    .blocks()
                    .iter()
                    .find_map(|block| block.content.as_text().map(|_| block.id))
            })?;
            editor.select_for_workload(DocPoint::with_affinity(point, 0, Affinity::After));
            let selection = editor.selection();
            let before_content = editor
                .document()
                .block(point)
                .map(|block| block.content.clone())?;
            let started = Instant::now();
            let outcome = editor
                .apply(Transaction::InsertText {
                    selection,
                    text: "x".into(),
                })
                .ok()?;
            let elapsed = started.elapsed().as_micros().min(u128::from(u64::MAX)) as u64;
            report.transaction_histogram.observe_us(elapsed);
            if outcome.changed_nodes.len() != 1 {
                return None;
            }
            if outcome.changed_nodes[0] != point {
                return None;
            }
            report.changed_node_assertions = report.changed_node_assertions.saturating_add(1);
            editor.undo().ok()?;
            let restored_content = editor
                .document()
                .block(point)
                .map(|block| block.content.clone());
            if restored_content.as_ref() != Some(&before_content)
                || editor.document().block_count() != baseline_block_count
                || editor.selection() != selection
            {
                return None;
            }
            report.local_restoration_assertions =
                report.local_restoration_assertions.saturating_add(1);
            Some(())
        });
        outcome?;
        report.apply_undo_pairs = report.apply_undo_pairs.saturating_add(1);
    }

    if editor.read(cx).document().semantic_snapshot() != baseline {
        return None;
    }
    report.full_document_verifications = report.full_document_verifications.saturating_add(1);
    Some(report)
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
    apply_paste_intent_at(editor, intent, None)
}

fn apply_paste_intent_at(
    editor: &mut EditorCore,
    intent: PasteIntent,
    drop_point: Option<DocPoint>,
) -> Result<(), DocumentError> {
    match intent {
        PasteIntent::Image { payload } => {
            if let Some(point) = drop_point {
                editor.insert_image_payload_at(payload, Selection::caret(point))
            } else {
                editor.insert_image_payload(payload)
            }
        }
        PasteIntent::File { path, cleanup: _ } => {
            let result = if let Some(point) = drop_point {
                editor.insert_image_path_at(&path, Selection::caret(point))
            } else {
                editor.insert_image_path(&path)
            };
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
    let owned_temporary_files = payload.temporary_files.clone();
    let result = apply_paste_intent(editor, classify_clipboard(payload));
    cleanup_owned_temporary_files(&owned_temporary_files);
    result
}

fn cleanup_owned_temporary_files(paths: &[PathBuf]) {
    for path in paths {
        if let Err(error) = std::fs::remove_file(path)
            && error.kind() != std::io::ErrorKind::NotFound
        {
            eprintln!("failed to remove owned temporary pasteboard image {path:?}: {error}");
        }
    }
}

fn apply_drop_paths(editor: &mut EditorCore, paths: &[PathBuf]) -> Result<(), DocumentError> {
    apply_paste_intent(editor, classify_drop(paths))
}

fn apply_drop_paths_at(
    editor: &mut EditorCore,
    paths: &[PathBuf],
    drop_point: Option<DocPoint>,
) -> Result<(), DocumentError> {
    apply_paste_intent_at(editor, classify_drop(paths), drop_point)
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
    use crate::native_editor::fixtures::typical_image_payload;
    use crate::native_editor::images::{ImagePayload, proxy_max_edge_for_viewport};
    use gpui::{
        AppContext, ImageCache, Modifiers, Resource, TestAppContext, VisualContext,
        VisualTestContext, point,
    };
    use std::mem::size_of;
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };

    #[test]
    fn drawable_pool_contract_rejects_gpui_default_count() {
        assert!(validate_spike_drawable_pool_limit(2).is_ok());
        assert_eq!(
            validate_spike_drawable_pool_limit(3).expect_err("GPUI's three drawable default"),
            "Task 7 CAMetalLayer maximumDrawableCount must be 2; got 3"
        );
    }

    #[test]
    fn task7_cli_requires_strict_fixture_and_absolute_output_paths() {
        assert!(!measurement_options_requested(&["--evernote-spike".into()]));
        assert!(measurement_options_requested(&[
            "--evernote-spike".into(),
            "--fixture".into(),
        ]));
        let options = parse_spike_options(&[
            "--evernote-spike".into(),
            "--fixture".into(),
            "typical".into(),
            "--ready-file".into(),
            "/tmp/task7-ready".into(),
            "--diagnostics-file".into(),
            "/tmp/task7-diagnostics".into(),
        ])
        .expect("complete Task 7 CLI should parse");
        assert_eq!(options.fixture, FixtureKind::Typical);
        assert!(parse_spike_options(&["--evernote-spike".into()]).is_err());
        assert!(
            parse_spike_options(&[
                "--evernote-spike".into(),
                "--fixture".into(),
                "nope".into(),
                "--ready-file".into(),
                "/tmp/r".into(),
                "--diagnostics-file".into(),
                "/tmp/d".into(),
            ])
            .is_err()
        );
        assert!(
            parse_spike_options(&[
                "--evernote-spike".into(),
                "--fixture".into(),
                "empty".into(),
                "--ready-file".into(),
                "relative-ready".into(),
                "--diagnostics-file".into(),
                "/tmp/d".into(),
            ])
            .is_err()
        );
        for token in ["--help", "-h", "--version", "-v", "-V"] {
            let args = vec!["--evernote-spike".into(), "--fixture".into(), token.into()];
            assert_eq!(
                global_help_or_version(&args),
                Some(if token == "--help" || token == "-h" {
                    "help"
                } else {
                    "version"
                }),
                "global option must win over fixture value parsing: {token}"
            );
        }
        assert_eq!(
            global_help_or_version(&[
                "--ready-file".into(),
                "/tmp/ready".into(),
                "--diagnostics-file".into(),
                "--version".into(),
            ]),
            Some("version")
        );
    }

    #[gpui::test]
    fn typical_fixture_populates_two_hundred_text_or_list_blocks_and_ten_images(
        cx: &mut TestAppContext,
    ) {
        let editor = cx.new(|cx| EditorCore::new(build_document(FixtureKind::Typical), cx));
        editor
            .update(cx, |editor, _| populate_typical_images(editor))
            .expect("typical fixture images should insert through production path");
        let (text_or_list_count, image_count, resources, dimensions) =
            editor.update(cx, |editor, _| {
                let document = editor.document();
                let text_or_list_count = document
                    .blocks()
                    .into_iter()
                    .filter(|block| block.content.as_text().is_some_and(|text| !text.is_empty()))
                    .count();
                let mut resources = std::collections::BTreeSet::new();
                let image_count = document
                    .blocks()
                    .into_iter()
                    .filter_map(|block| match &block.content {
                        crate::native_editor::model::BlockContent::Image {
                            resource_id, ..
                        } => {
                            resources.insert(resource_id.clone());
                            Some(())
                        }
                        _ => None,
                    })
                    .count();
                let dimensions = resources
                    .iter()
                    .map(|resource_id| {
                        let metadata = editor
                            .image_metadata(resource_id)
                            .expect("production ImageStore metadata");
                        (metadata.natural_width, metadata.natural_height)
                    })
                    .collect::<Vec<_>>();
                (text_or_list_count, image_count, resources, dimensions)
            });
        assert_eq!(text_or_list_count, 200);
        assert_eq!(image_count, typical_image_count());
        assert_eq!(resources.len(), typical_image_count());
        assert!(dimensions.iter().all(|size| *size == (1600, 900)));
        assert_eq!(
            dimensions.len() * 1600 * 900 * 4,
            typical_image_count() * 1600 * 900 * 4,
            "production texture authority input dimensions"
        );
    }

    #[gpui::test]
    fn long_fixture_runs_exactly_five_hundred_real_edit_undo_pairs(cx: &mut TestAppContext) {
        let cx = cx.add_empty_window();
        let editor = cx.new(|cx| EditorCore::new(build_document(FixtureKind::Long), cx));
        let report = cx
            .update(|_, app| run_edit_undo_pairs(&editor, 500, false, app))
            .expect("long fixture workload should restore its baseline");
        assert_eq!(report.apply_undo_pairs, 500);
        assert_eq!(report.changed_node_assertions, 500);
        assert_eq!(report.local_restoration_assertions, 500);
        assert_eq!(report.full_document_verifications, 1);
        assert_eq!(report.transaction_histogram.sample_count(), 500);
    }

    #[gpui::test]
    fn typical_fixture_loads_each_1600x900_resource_through_production_cache(
        cx: &mut TestAppContext,
    ) {
        let root =
            std::env::temp_dir().join(format!("task7-typical-cache-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&root).expect("typical cache fixture directory");
        let paths = (0..typical_image_count())
            .map(|index| {
                let path = root.join(format!("fixture-{index}.png"));
                std::fs::write(&path, typical_image_payload(index).bytes)
                    .expect("typical fixture PNG");
                path
            })
            .collect::<Vec<_>>();
        let cache =
            cx.update(|app| BudgetedImageCache::new_entity(app, DECODED_IMAGE_CACHE_BUDGET));
        let window = cx.add_empty_window();
        // This direct cache test supplies the same 680pt editor content width
        // used by the spike's image blocks. The real render path supplies its
        // block width before each cache load.
        let requested_edge = proxy_max_edge_for_viewport(680.0, 1.0);
        let expected_minimum = requested_edge as usize * (requested_edge as usize * 900 / 1600) * 4;
        let expected_proxy_upper_bound =
            (requested_edge as usize + 63) * (requested_edge as usize + 63) * 4;
        for path in &paths {
            let resource = Resource::from(path.clone());
            window.update(|window, app| {
                cache.update(app, |cache, entity_cx| {
                    cache.set_visible_resources([&resource]);
                    cache.request_edge(&resource, requested_edge);
                    cache.evict_offscreen(window, entity_cx);
                    assert!(cache.load(&resource, window, entity_cx).is_none());
                });
            });
            window.run_until_parked();
            window.update(|window, app| {
                cache.update(app, |cache, entity_cx| {
                    let image = cache
                        .load(&resource, window, entity_cx)
                        .expect("production cache should return decoded image")
                        .expect("typical fixture image should decode");
                    let size = image.size(0);
                    let width = u32::from(size.width);
                    let height = u32::from(size.height);
                    assert!(
                        width.max(height) <= requested_edge.saturating_add(63),
                        "production proxy retained at {}x{} for request {}",
                        width,
                        height,
                        requested_edge
                    );
                    assert!(cache.used_bytes() >= expected_minimum);
                    assert!(cache.used_bytes() <= DECODED_IMAGE_CACHE_BUDGET);
                    assert!(cache.is_settled());
                });
            });
        }
        window.update(|window, app| {
            cache.update(app, |cache, entity_cx| {
                cache.set_visible_resources(std::iter::empty());
                cache.evict_offscreen(window, entity_cx);
                assert_eq!(cache.used_bytes(), 0);
                assert_eq!(cache.len(), 0);
                assert!(cache.peak_accounted_bytes() > 0);
                assert!(cache.peak_accounted_bytes() <= expected_proxy_upper_bound);
                assert!(cache.is_settled());
            });
        });
        let _ = std::fs::remove_dir_all(root);
    }

    #[gpui::test]
    fn production_paint_entity_uses_translated_content_mask_for_image_residency(
        cx: &mut TestAppContext,
    ) {
        cx.update(|cx| components::init(cx));
        crate::native_editor::render::reset_test_image_residency_observation();
        let cache =
            cx.update(|app| BudgetedImageCache::new_entity(app, DECODED_IMAGE_CACHE_BUDGET));
        let view_cache = cache.clone();
        let (view, cx) = cx.add_window_view(move |window, cx| {
            let editor = cx.new(|cx| EditorCore::new(build_document(FixtureKind::Empty), cx));
            editor
                .update(cx, |editor, _| populate_typical_images(editor))
                .expect("production image fixture should insert");
            editor.read(cx).focus_handle().focus(window);
            SpikeView {
                editor,
                title: cx.new(|cx| TitleInput::new("会议记录".into(), cx)),
                image_cache: Some(view_cache.clone()),
                catalogue: CommandCatalogue::default(),
                scroll_handle: ScrollHandle::new(),
                more_open: false,
                link_popover: None,
                pointer_anchor: None,
                drop_point: None,
                more_trigger_bounds: None,
                measurement: None,
            }
        });

        cx.simulate_resize(size(px(1080.0), px(720.0)));
        redraw(cx);

        let observation = crate::native_editor::render::test_image_residency_observation()
            .expect("real canvas paint_entity path should record image residency");
        assert!(!observation.image_bounds.is_empty());
        assert!(
            !observation.visible_bounds.is_empty(),
            "at least one translated image must intersect the real content mask"
        );
        assert!(observation.prefetch_bounds.len() <= 2);
        for bounds in &observation.visible_bounds {
            assert!(
                bounds.left() < observation.content_mask.right()
                    && bounds.right() > observation.content_mask.left()
                    && bounds.top() < observation.content_mask.bottom()
                    && bounds.bottom() > observation.content_mask.top(),
                "visible image and content mask must be in the same translated coordinate space"
            );
        }
        for bounds in &observation.prefetch_bounds {
            assert!(
                bounds.bottom() <= observation.content_mask.top()
                    || bounds.top() >= observation.content_mask.bottom(),
                "prefetch image must be outside the actual content mask"
            );
        }
        let resident_count = observation.visible_indices.len() + observation.prefetch_indices.len();
        let settled_entries = cx.update(|_, app| cache.read(app).len());
        assert!(
            settled_entries <= resident_count,
            "paint_entity must not decode remote images outside visible+adjacent prefetch"
        );
        let _ = view;
    }

    #[test]
    fn viewport_readiness_requires_all_one_hundred_twenty_commits_and_reset() {
        let options = SpikeLaunchOptions {
            fixture: FixtureKind::Empty,
            ready_file: PathBuf::from("/tmp/task7-ready"),
            diagnostics_file: PathBuf::from("/tmp/task7-diagnostics"),
            run_id: "test-run".into(),
            binary_sha256: "test-sha".into(),
        };
        let mut runtime = MeasurementRuntime::new(options);
        runtime.workload_complete = true;
        runtime.render_sample_count = 120;
        runtime.viewport_complete = true;
        assert!(!runtime.ready_prerequisites(true));
        for _ in 0..120 {
            runtime.request_viewport_frame();
            runtime.complete_viewport_frame();
        }
        runtime.request_viewport_reset_frame();
        runtime.complete_viewport_frame();
        runtime.record_paint(false, true, 1);
        assert!(runtime.ready_prerequisites(true));
        assert_eq!(runtime.viewport_frames_requested, 120);
        assert_eq!(runtime.viewport_frames_completed, 120);
        assert_eq!(runtime.viewport_reset_frames_requested, 1);
        assert_eq!(runtime.viewport_reset_frames_completed, 1);
        runtime.viewport_frames_completed -= 1;
        assert!(!runtime.ready_prerequisites(true));

        // The final restored-state paint is separate from the 120 viewport
        // shift paints.  A runtime that accepts 121 shift samples has not
        // proved the exact lifecycle contract and must stay unready.
        runtime.viewport_frames_completed = 120;
        runtime.render_sample_count = 121;
        assert!(!runtime.ready_prerequisites(true));

        // A reset paint cannot satisfy the shift-sample contract by itself.
        runtime.render_sample_count = 120;
        runtime.viewport_reset_paint_count = 2;
        assert!(!runtime.ready_prerequisites(true));
    }

    #[test]
    fn viewport_paint_accounting_records_only_shift_samples_in_render_histogram() {
        let options = SpikeLaunchOptions {
            fixture: FixtureKind::Empty,
            ready_file: PathBuf::from("/tmp/task7-ready"),
            diagnostics_file: PathBuf::from("/tmp/task7-diagnostics"),
            run_id: "test-run".into(),
            binary_sha256: "test-sha".into(),
        };
        let mut runtime = MeasurementRuntime::new(options);
        for _ in 0..120 {
            runtime.record_paint(true, false, 10);
        }
        runtime.record_paint(false, true, 20);
        assert_eq!(runtime.render_sample_count, 120);
        assert_eq!(runtime.render_histogram.sample_count(), 120);
        assert_eq!(runtime.viewport_reset_paint_count, 1);
        assert!(!runtime.ready_prerequisites(true));
    }

    fn assert_long_async_measurement_delivery_drives_every_production_viewport_paint(
        cx: &mut TestAppContext,
    ) {
        cx.update(|app| components::init(app));
        let output_root =
            std::env::temp_dir().join(format!("task7-async-delivery-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&output_root).expect("Task 7 async delivery output directory");
        let options = SpikeLaunchOptions {
            fixture: FixtureKind::Long,
            ready_file: output_root.join("ready"),
            diagnostics_file: output_root.join("diagnostics.json"),
            run_id: "test-run".into(),
            binary_sha256: "test-sha".into(),
        };
        let cache =
            cx.update(|app| BudgetedImageCache::new_entity(app, DECODED_IMAGE_CACHE_BUDGET));
        let (view, cx) = cx.add_window_view(move |window, app| {
            let editor = app.new(|app| EditorCore::new(build_document(FixtureKind::Long), app));
            editor.read(app).focus_handle().focus(window);
            SpikeView {
                editor,
                title: app.new(|app| TitleInput::new("会议记录".into(), app)),
                image_cache: Some(cache.clone()),
                catalogue: CommandCatalogue::default(),
                scroll_handle: ScrollHandle::new(),
                more_open: false,
                link_popover: None,
                pointer_anchor: None,
                drop_point: None,
                more_trigger_bounds: None,
                measurement: Some(Box::new(MeasurementRuntime::new(options))),
            }
        });

        cx.simulate_resize(size(px(1080.0), px(720.0)));
        // TestApp may paint implicitly while creating the window or flushing
        // effects. Drive the real canvas path explicitly, then check that
        // asynchronous delivery has left a pending shift before the following
        // explicit paint loop can make the measurement ready.
        cx.update(|window, app| {
            window.draw(app).clear();
        });
        // Drive only the spawned async workload task. `run_until_parked`
        // would recursively consume the queued next-frame notifications in
        // TestApp, which would no longer distinguish asynchronous delivery
        // from the explicit production paints below.
        let spawned_task_was_pending = cx.executor().tick();

        let delivered = cx.update(|_, app| {
            let runtime = view
                .read(app)
                .measurement
                .as_ref()
                .expect("measurement runtime");
            (
                runtime.workload_started,
                runtime.workload_complete,
                runtime.viewport_shift_index,
                runtime.viewport_frames_requested,
                runtime.viewport_frames_completed,
                runtime.ready_written,
            )
        });
        assert!(
            delivered.0,
            "production canvas paint must start Window::spawn"
        );
        assert!(
            spawned_task_was_pending || delivered.1,
            "Window::spawn must either remain queued or deliver the workload"
        );
        assert!(
            delivered.1,
            "async workload must be delivered to its live window"
        );
        assert!(
            delivered.2 >= 1,
            "late async delivery must start the first viewport shift on its owning window"
        );
        assert!(delivered.3 >= 1);
        assert!(
            delivered.4 < delivered.3,
            "the next manually driven canvas paint must have a pending production shift"
        );
        assert!(
            !delivered.5 && !output_root.join("ready").exists(),
            "delivery may initiate a frame but must not make readiness true before a paint"
        );

        // The 120 shift paints do not include the final restored-position
        // paint, so readiness may only become true after paint 121.
        for _ in delivered.4..120 {
            cx.update(|window, app| {
                window.draw(app).clear();
            });
        }
        let before_reset_paint = cx.update(|_, app| {
            let runtime = view
                .read(app)
                .measurement
                .as_ref()
                .expect("measurement runtime");
            (
                runtime.viewport_frames_completed,
                runtime.viewport_frames_requested,
                runtime.viewport_reset_frames_completed,
                runtime.viewport_reset_frames_requested,
                runtime.ready_written,
            )
        });
        assert_eq!(before_reset_paint.0, 120);
        assert_eq!(before_reset_paint.1, 120);
        assert_eq!(before_reset_paint.2, 0);
        assert_eq!(before_reset_paint.3, 1);
        assert!(!before_reset_paint.4);

        cx.update(|window, app| {
            window.draw(app).clear();
        });
        let settled = cx.update(|_, app| {
            let runtime = view
                .read(app)
                .measurement
                .as_ref()
                .expect("measurement runtime");
            (
                runtime.viewport_frames_completed,
                runtime.viewport_frames_requested,
                runtime.viewport_reset_frames_completed,
                runtime.viewport_reset_frames_requested,
                runtime.viewport_complete,
                runtime.ready_written,
            )
        });
        assert_eq!(settled.0, 120);
        assert_eq!(settled.1, 120);
        assert_eq!(settled.2, 1);
        assert_eq!(settled.3, 1);
        assert!(settled.4 && settled.5);
        assert!(output_root.join("ready").is_file());
        assert!(output_root.join("diagnostics.json").is_file());
        std::fs::remove_dir_all(output_root).expect("Task 7 async delivery output cleanup");
    }

    #[gpui::test]
    fn long_async_measurement_delivery_drives_every_production_viewport_paint(
        _cx: &mut TestAppContext,
    ) {
        // Window::spawn can complete synchronously in TestApp while the
        // initiating canvas draw is still on the call stack. This test-thread
        // stack configuration contains that nested test-harness callback chain;
        // it does not configure a production executor or scheduler.
        std::thread::Builder::new()
            .name("task7-async-delivery-test".into())
            .stack_size(32 * 1024 * 1024)
            .spawn(|| {
                let mut cx = TestAppContext::single();
                assert_long_async_measurement_delivery_drives_every_production_viewport_paint(
                    &mut cx,
                );
            })
            .expect("Task 7 async delivery test thread should start")
            .join()
            .expect("Task 7 async delivery test thread should pass");
    }

    #[test]
    fn measurement_frame_drive_stays_pending_until_workload_delivery() {
        let options = SpikeLaunchOptions {
            fixture: FixtureKind::Empty,
            ready_file: PathBuf::from("/tmp/task7-ready"),
            diagnostics_file: PathBuf::from("/tmp/task7-diagnostics"),
            run_id: "test-run".into(),
            binary_sha256: "test-sha".into(),
        };
        let mut runtime = MeasurementRuntime::new(options);

        assert!(measurement_workload_needs_frame(Some(&runtime)));
        runtime.workload_started = true;
        assert!(measurement_workload_needs_frame(Some(&runtime)));

        runtime.workload_complete = true;
        assert!(!measurement_workload_needs_frame(Some(&runtime)));
        assert!(!measurement_workload_needs_frame(None));
    }

    fn redraw(cx: &mut VisualTestContext) {
        cx.update(|window, app| window.draw(app).clear());
        cx.run_until_parked();
    }

    fn valid_png_bytes() -> Vec<u8> {
        ClipboardPayload::fixture_with_png_and_text("")
            .images
            .into_iter()
            .next()
            .expect("fixture image")
            .bytes
    }

    fn exif_jpeg_bytes(orientation: u32) -> Vec<u8> {
        assert!((1..=8).contains(&orientation));
        let mut encoded = std::io::Cursor::new(Vec::new());
        image::DynamicImage::ImageRgb8(image::ImageBuffer::from_fn(4, 2, |x, y| {
            image::Rgb([(x * 50) as u8, (y * 100) as u8, 10])
        }))
        .write_to(&mut encoded, image::ImageFormat::Jpeg)
        .expect("JPEG fixture should encode");
        let mut exif = b"Exif\0\0MM\0*\0\0\0\x08\0\x01\x01\x12\0\x03\0\0\0\x01".to_vec();
        exif.extend_from_slice(&[0, orientation as u8, 0, 0, 0, 0, 0, 0]);
        let segment_len = u16::try_from(exif.len() + 2).expect("small EXIF fixture");
        let mut segment = vec![0xff, 0xe1, (segment_len >> 8) as u8, segment_len as u8];
        segment.extend_from_slice(&exif);
        let mut jpeg = encoded.into_inner();
        jpeg.splice(2..2, segment);
        jpeg
    }

    fn build_view(window: &mut Window, cx: &mut Context<SpikeView>) -> SpikeView {
        let editor = cx.new(|cx| EditorCore::new(Document::from_paragraphs(["alpha", "beta"]), cx));
        editor.read(cx).focus_handle().focus(window);
        SpikeView {
            editor,
            title: cx.new(|cx| TitleInput::new("会议记录".into(), cx)),
            image_cache: None,
            catalogue: CommandCatalogue::default(),
            scroll_handle: ScrollHandle::new(),
            more_open: false,
            link_popover: None,
            pointer_anchor: None,
            drop_point: None,
            more_trigger_bounds: None,
            measurement: None,
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
        let drop_target = editor
            .document()
            .blocks()
            .first()
            .expect("drop target block")
            .id;
        editor.set_caret_utf8(editor.copy_all_plain_text().len());
        apply_drop_paths_at(
            &mut editor,
            std::slice::from_ref(&path),
            Some(DocPoint::new(drop_target, 0)),
        )
        .expect("drop action should honor prospective drag point");
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

        let unselected_native_temp = std::env::temp_dir().join(format!(
            "joplin-lite-task6-unselected-temp-{}.png",
            std::process::id()
        ));
        std::fs::write(&unselected_native_temp, valid_png_bytes())
            .expect("owned native temporary should be writable");
        let merged_mixed_payload = resolve_clipboard_payload(
            Some(ClipboardPayload {
                temporary_files: vec![unselected_native_temp.clone()],
                ..Default::default()
            }),
            Some(ClipboardPayload {
                images: vec![ImagePayload::new(gpui::ImageFormat::Png, valid_png_bytes())],
                text: Some("图像占位符".into()),
                ..Default::default()
            }),
        )
        .expect("mixed native and GPUI clipboard payload should resolve");
        apply_clipboard_payload(&mut editor, merged_mixed_payload)
            .expect("image representation should be selected");
        assert!(
            !unselected_native_temp.exists(),
            "unselected owned temporary must be collected after image selection"
        );

        let rejected_native_temp = std::env::temp_dir().join(format!(
            "joplin-lite-task6-rejected-temp-{}.png",
            std::process::id()
        ));
        std::fs::write(&rejected_native_temp, valid_png_bytes())
            .expect("owned rejected temporary should be writable");
        apply_clipboard_payload(
            &mut editor,
            ClipboardPayload {
                temporary_files: vec![rejected_native_temp.clone()],
                html: Some("<img src=\"https://example.invalid/not-owned.png\">".into()),
                ..Default::default()
            },
        )
        .expect("unsupported HTML should be a non-error fallback");
        assert!(!rejected_native_temp.exists());

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

    #[gpui::test]
    fn production_external_paths_drop_handler_consumes_prospective_point(cx: &mut TestAppContext) {
        let (view, cx) = cx.add_window_view(build_view);
        view.update(cx, |view, view_cx| {
            let first = view.editor.read(view_cx).document().blocks()[0].id;
            view.editor.update(view_cx, |editor, editor_cx| {
                editor.set_selection_for_test(Selection::caret(DocPoint::with_affinity(
                    first,
                    0,
                    Affinity::Before,
                )));
                editor_cx.notify();
            });
        });
        let (target, old_caret) = view.read_with(cx, |view, app| {
            let target = view.editor.read(app).document().blocks()[1].id;
            let old_caret = view.editor.read(app).selection().head;
            (target, old_caret)
        });
        let prospective = DocPoint::new(target, 0);
        assert_ne!(old_caret.node_id, prospective.node_id);

        // GPUI 0.2.2 has no public constructor for a non-empty ExternalPaths
        // and no test-platform file-drag injection API. Exercise the real
        // production handler with the real ExternalPaths event payload type;
        // the path action below uses the same prospective point with a real
        // managed PNG fixture.
        cx.update(|window, app| {
            view.update(app, |view, view_cx| {
                view.drop_point = Some(prospective);
                view.on_external_paths_drop(&ExternalPaths::default(), window, view_cx);
                assert!(view.drop_point.is_none());
            });
        });

        let path = std::env::temp_dir().join(format!(
            "joplin-lite-task6-round2b-drop-{}.png",
            std::process::id()
        ));
        std::fs::write(&path, valid_png_bytes()).expect("real drop fixture");
        view.update(cx, |view, view_cx| {
            view.editor.update(view_cx, |editor, _| {
                apply_drop_paths_at(editor, std::slice::from_ref(&path), Some(prospective))
                    .expect("production drop action should insert at prospective point");
            });
        });
        let has_image = view.read_with(cx, |view, app| {
            view.editor
                .read(app)
                .document()
                .blocks()
                .iter()
                .any(|block| {
                    matches!(
                        block.content,
                        crate::native_editor::model::BlockContent::Image { .. }
                    )
                })
        });
        assert!(has_image);
        let _ = std::fs::remove_file(path);
    }

    #[gpui::test]
    fn production_clipboard_prefers_native_exif_paths_and_decodes_transformed_geometry(
        cx: &mut TestAppContext,
    ) {
        for orientation in [6, 8] {
            let source_path = std::env::temp_dir().join(format!(
                "joplin-lite-task6-exif-{orientation}-{}.jpg",
                std::process::id()
            ));
            let bytes = exif_jpeg_bytes(orientation);
            std::fs::write(&source_path, &bytes).expect("EXIF fixture should be writable");
            let mut editor = EditorCore::for_test("前后", cx);
            let merged = resolve_clipboard_payload(
                Some(ClipboardPayload {
                    file_urls: vec![source_path.clone()],
                    temporary_files: vec![source_path.clone()],
                    ..Default::default()
                }),
                Some(ClipboardPayload {
                    images: vec![ImagePayload::new(gpui::ImageFormat::Jpeg, bytes.clone())],
                    ..Default::default()
                }),
            )
            .expect("native and GPUI representations should merge");
            assert!(matches!(
                classify_clipboard(merged.clone()),
                PasteIntent::File { .. }
            ));
            apply_clipboard_payload(&mut editor, merged)
                .expect("native EXIF path should insert through production paste");

            let resource_id = editor
                .document()
                .blocks()
                .iter()
                .find_map(|block| match &block.content {
                    crate::native_editor::model::BlockContent::Image { resource_id, .. } => {
                        Some(resource_id.clone())
                    }
                    _ => None,
                })
                .expect("clipboard paste should commit an image node");
            let metadata = editor
                .image_metadata(&resource_id)
                .expect("image metadata should be retained");
            assert_eq!((metadata.natural_width, metadata.natural_height), (2, 4));
            let managed_path = editor
                .image_source_path(&resource_id)
                .expect("managed path should be retained")
                .to_path_buf();
            assert_eq!(std::fs::read(&managed_path).expect("managed bytes"), bytes);
            let proxy = BudgetedImageCache::decode_resource_bounded(
                &gpui::Resource::from(managed_path),
                DECODED_IMAGE_CACHE_BUDGET,
            )
            .expect("ImageIO should decode the transformed EXIF proxy");
            let size = proxy.size(0);
            assert_eq!((u32::from(size.width), u32::from(size.height)), (2, 4));
            assert!(
                !source_path.exists(),
                "owned clipboard temp must be reclaimed"
            );
        }
    }

    #[gpui::test]
    fn failed_image_click_retries_only_the_target_resource(cx: &mut TestAppContext) {
        let (view, cx) = cx.add_window_view(build_view);
        let resource_ids = view.update(cx, |view, view_cx| {
            view.editor.update(view_cx, |editor, _| {
                for _ in 0..2 {
                    editor
                        .insert_image_payload(ImagePayload::new(
                            gpui::ImageFormat::Png,
                            valid_png_bytes(),
                        ))
                        .expect("fixture image should insert");
                }
                let ids = editor
                    .document()
                    .blocks()
                    .into_iter()
                    .filter_map(|block| match &block.content {
                        crate::native_editor::model::BlockContent::Image {
                            resource_id, ..
                        } => Some(resource_id.clone()),
                        _ => None,
                    })
                    .collect::<Vec<_>>();
                for id in &ids {
                    assert!(editor.mark_image_failed(id));
                }
                ids
            })
        });
        let target = resource_ids.first().expect("target image").clone();
        cx.update(|window, app| {
            view.update(app, |view, view_cx| {
                view.retry_image_by_id(&target, window, view_cx);
            });
        });
        view.read_with(cx, |view, app| {
            assert_eq!(
                view.editor.read(app).image_state(&target),
                Some(crate::native_editor::images::ImageNodeState::Loading)
            );
            assert_eq!(
                view.editor
                    .read(app)
                    .image_state(resource_ids.get(1).expect("second image")),
                Some(crate::native_editor::images::ImageNodeState::Failed)
            );
        });
    }

    fn build_long_view(window: &mut Window, cx: &mut Context<SpikeView>) -> SpikeView {
        let editor =
            cx.new(|cx| EditorCore::new(Document::from_paragraph("wrap ".repeat(360)), cx));
        editor.read(cx).focus_handle().focus(window);
        SpikeView {
            editor,
            title: cx.new(|cx| TitleInput::new("会议记录".into(), cx)),
            image_cache: None,
            catalogue: CommandCatalogue::default(),
            scroll_handle: ScrollHandle::new(),
            more_open: false,
            link_popover: None,
            pointer_anchor: None,
            drop_point: None,
            more_trigger_bounds: None,
            measurement: None,
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
            title: cx.new(|cx| TitleInput::new("会议记录".into(), cx)),
            image_cache: None,
            catalogue: CommandCatalogue::default(),
            scroll_handle: ScrollHandle::new(),
            more_open: false,
            link_popover: None,
            pointer_anchor: None,
            drop_point: None,
            more_trigger_bounds: None,
            measurement: None,
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
            title: cx.new(|cx| TitleInput::new("会议记录".into(), cx)),
            image_cache: None,
            catalogue: CommandCatalogue::default(),
            scroll_handle: ScrollHandle::new(),
            more_open: false,
            link_popover: None,
            pointer_anchor: None,
            drop_point: None,
            more_trigger_bounds: None,
            measurement: None,
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
            title: cx.new(|cx| TitleInput::new("会议记录".into(), cx)),
            image_cache: None,
            catalogue: CommandCatalogue::default(),
            scroll_handle: ScrollHandle::new(),
            more_open: false,
            link_popover: None,
            pointer_anchor: None,
            drop_point: None,
            more_trigger_bounds: None,
            measurement: None,
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
            title: cx.new(|cx| TitleInput::new("会议记录".into(), cx)),
            image_cache: None,
            catalogue: CommandCatalogue::default(),
            scroll_handle: ScrollHandle::new(),
            more_open: false,
            link_popover: None,
            pointer_anchor: None,
            drop_point: None,
            more_trigger_bounds: None,
            measurement: None,
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
            title: cx.new(|cx| TitleInput::new("会议记录".into(), cx)),
            image_cache: None,
            catalogue: CommandCatalogue::default(),
            scroll_handle: ScrollHandle::new(),
            more_open: false,
            link_popover: None,
            pointer_anchor: None,
            drop_point: None,
            more_trigger_bounds: None,
            measurement: None,
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
                view.link_popover.is_some(),
                "invalid submit must stay open without mutating"
            );
            assert_eq!(
                view.editor.read(cx).document().semantic_snapshot(),
                before_cancel.0
            );
            assert_eq!(view.editor.read(cx).undo_depth(), before_cancel.1);
            assert_eq!(view.editor.read(cx).selection(), before_cancel.2);
        });

        cx.update(|window, app| {
            view.update(app, |view, view_cx| {
                view.cancel_link(&CancelLink, window, view_cx)
            });
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
    async fn shell_visible_link_buttons_keep_invalid_open_and_dispatch_cancel_or_apply(
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
        let invalid_baseline = view.read_with(cx, |view, cx| {
            let editor = view.editor.read(cx);
            (
                editor.document().semantic_snapshot(),
                editor.undo_depth(),
                editor.selection(),
            )
        });
        let link = cx.debug_bounds("Link").expect("visible Link command");
        cx.simulate_click(link.center(), Modifiers::default());
        redraw(cx);
        view.read_with(cx, |view, _| assert!(view.link_popover.is_some()));
        let apply = cx
            .debug_bounds("evernote-link-apply")
            .expect("Apply must be a real hit target");
        let panel = cx
            .debug_bounds("evernote-link-popover")
            .expect("mounted Link panel before Apply");
        assert!(
            apply.left() >= panel.left()
                && apply.right() <= panel.right()
                && apply.top() >= panel.top()
                && apply.bottom() <= panel.bottom(),
            "mounted Apply bounds must belong to the Link panel; apply={apply:?}, panel={panel:?}"
        );
        cx.simulate_input("not-a-url");
        view.read_with(cx, |view, cx| {
            assert_eq!(
                view.link_popover.as_ref().unwrap().read(cx).text,
                "not-a-url",
                "mounted URL field must receive the invalid text before Apply"
            );
        });
        redraw(cx);
        cx.simulate_click(apply.center(), Modifiers::default());
        redraw(cx);
        view.read_with(cx, |view, cx| {
            assert!(
                view.link_popover.is_some(),
                "invalid Apply keeps the popover open; history={}, document_changed={}",
                view.editor.read(cx).undo_depth(),
                view.editor.read(cx).document().semantic_snapshot() != invalid_baseline.0
            );
            let popover = view.link_popover.as_ref().unwrap().read(cx);
            assert!(
                popover.invalid,
                "mounted Apply must invoke URL validation; text={:?}, selection={:?}",
                popover.text, popover.selection,
            );
            let editor = view.editor.read(cx);
            assert_eq!(editor.document().semantic_snapshot(), invalid_baseline.0);
            assert_eq!(editor.undo_depth(), invalid_baseline.1);
            assert_eq!(editor.selection(), invalid_baseline.2);
        });
        cx.update(|window, app| {
            view.read_with(app, |view, app| {
                assert!(
                    view.link_popover
                        .as_ref()
                        .unwrap()
                        .read(app)
                        .focus
                        .is_focused(window),
                    "invalid Apply must retain URL-field focus"
                );
            });
        });
        assert!(
            cx.debug_bounds("evernote-link-invalid-error").is_some(),
            "invalid mounted Apply must show the Chinese URL error"
        );
        let cancel = cx
            .debug_bounds("evernote-link-cancel")
            .expect("Cancel remains clickable after invalid validation");
        cx.simulate_click(cancel.center(), Modifiers::default());
        redraw(cx);
        cx.update(|window, app| {
            view.read_with(app, |view, app| {
                assert!(view.link_popover.is_none());
                assert!(view.editor.read(app).focus_handle().is_focused(window));
            });
        });

        let (before_apply_history, selected_range) = view.read_with(cx, |view, cx| {
            (
                view.editor.read(cx).undo_depth(),
                view.editor.read(cx).selected_text_ranges(),
            )
        });
        let link = cx.debug_bounds("Link").expect("Link remains mounted");
        cx.simulate_click(link.center(), Modifiers::default());
        redraw(cx);
        cx.simulate_input("https://example.com");
        let apply = cx
            .debug_bounds("evernote-link-apply")
            .expect("Apply remains clickable after reopening");
        cx.simulate_click(apply.center(), Modifiers::default());
        redraw(cx);
        cx.update(|window, app| {
            view.read_with(app, |view, app| {
                assert!(view.link_popover.is_none());
                assert!(view.editor.read(app).focus_handle().is_focused(window));
                assert_eq!(view.editor.read(app).undo_depth(), before_apply_history + 1);
                let (node_id, range) = selected_range
                    .first()
                    .expect("link test has a real selected text range");
                let styles = view
                    .editor
                    .read(app)
                    .document()
                    .block(*node_id)
                    .and_then(|block| block.content.styles())
                    .expect("Apply creates a styled text block");
                assert!(styles.iter().any(|run| {
                    run.range.start <= range.start
                        && run.range.end >= range.end
                        && run.marks.iter().any(
                            |mark| matches!(mark, Mark::Link(url) if url == "https://example.com"),
                        )
                }));
            });
        });
    }

    #[gpui::test]
    async fn shell_760pt_primary_and_more_partition_has_no_missing_command(
        cx: &mut TestAppContext,
    ) {
        cx.update(|cx| components::init(cx));
        let (_view, cx) = cx.add_window_view(build_view);
        cx.simulate_resize(size(px(760.0), px(820.0)));
        redraw(cx);
        let placement = toolbar_placement(editor_chrome_metrics(760.0, 820.0).header_width);
        assert_eq!(
            placement.overflow,
            vec![
                EditorCommand::Link,
                EditorCommand::AlignLeft,
                EditorCommand::AlignCenter,
                EditorCommand::AlignRight,
            ]
        );
        for command in &placement.primary {
            let descriptor = CommandCatalogue::default()
                .descriptors()
                .iter()
                .find(|descriptor| descriptor.command == *command)
                .unwrap();
            let bounds = cx
                .debug_bounds(descriptor.label)
                .unwrap_or_else(|| panic!("{} primary", descriptor.label));
            assert_eq!(
                bounds.size,
                size(px(32.0), px(32.0)),
                "{} hit target",
                descriptor.label
            );
        }
        let more = cx
            .debug_bounds("evernote-native-spike-more-trigger")
            .expect("More trigger at 760pt");
        cx.simulate_click(more.center(), Modifiers::default());
        redraw(cx);
        for command in &placement.overflow {
            let descriptor = CommandCatalogue::default()
                .descriptors()
                .iter()
                .find(|descriptor| descriptor.command == *command)
                .unwrap();
            assert!(
                cx.debug_bounds(descriptor.label).is_some(),
                "{} overflow",
                descriptor.label
            );
        }
    }

    #[gpui::test]
    async fn shell_more_rows_have_chinese_labels_and_full_line_hit_targets_at_wide_and_narrow_widths(
        cx: &mut TestAppContext,
    ) {
        cx.update(|cx| components::init(cx));
        let (_view, cx) = cx.add_window_view(build_view);
        let catalogue = CommandCatalogue::default();

        for width in [1200.0, 760.0] {
            cx.simulate_resize(size(px(width), px(820.0)));
            redraw(cx);
            let trigger = cx
                .debug_bounds("evernote-native-spike-more-trigger")
                .expect("mounted More trigger");
            cx.simulate_click(trigger.center(), Modifiers::default());
            redraw(cx);
            let menu = cx
                .debug_bounds("evernote-native-spike-more-menu")
                .expect("mounted More menu");
            assert_eq!(menu.size.width, px(220.0), "{width}pt menu width");
            assert!(menu.size.height <= px(300.0), "{width}pt menu scroll cap");

            let placement = toolbar_placement(editor_chrome_metrics(width, 820.0).header_width);
            let mut commands = placement.overflow;
            commands.extend(
                catalogue
                    .more_descriptors()
                    .into_iter()
                    .map(|descriptor| descriptor.command),
            );
            for command in commands {
                let descriptor = catalogue
                    .descriptors()
                    .iter()
                    .find(|descriptor| descriptor.command == command)
                    .expect("catalogue descriptor");
                let row = cx
                    .debug_bounds(descriptor.label)
                    .unwrap_or_else(|| panic!("{} More row", descriptor.label));
                let chinese = cx
                    .debug_bounds(descriptor.label_zh)
                    .unwrap_or_else(|| panic!("{} Chinese More label", descriptor.label));
                let clipped_by_scroll_edge =
                    row.top() == menu.top() || row.bottom() == menu.bottom();
                assert!(
                    row.size.width >= menu.size.width - px(18.0)
                        && (row.size.height >= px(36.0) || clipped_by_scroll_edge),
                    "{} must be a full menu-row hit target; row={row:?}, menu={menu:?}",
                    descriptor.label,
                );
                assert!(
                    chinese.left() >= row.left()
                        && chinese.right() <= row.right()
                        && chinese.top() >= row.top()
                        && chinese.bottom() <= row.bottom(),
                    "{} Chinese label belongs to its command row",
                    descriptor.label
                );
            }

            let trigger = cx
                .debug_bounds("evernote-native-spike-more-trigger")
                .expect("More trigger remains mounted");
            cx.simulate_click(trigger.center(), Modifiers::default());
            redraw(cx);
        }
    }

    #[gpui::test]
    async fn shell_link_popover_anchors_to_clicked_trigger_and_stays_inside_content_mask(
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
        redraw(cx);
        view.read_with(cx, |view, cx| {
            assert!(
                !view.editor.read(cx).selection().is_caret(),
                "Link trigger requires a real text selection"
            );
        });
        let wide_link = cx.debug_bounds("Link").expect("wide Link trigger");
        cx.simulate_click(wide_link.center(), Modifiers::default());
        view.read_with(cx, |view, _| {
            assert!(
                view.link_popover.is_some(),
                "wide Link hit target must open its panel"
            )
        });
        redraw(cx);
        let wide_panel = cx
            .debug_bounds("evernote-link-popover")
            .expect("wide Link panel");
        let wide_mask = cx
            .debug_bounds("evernote-native-spike")
            .expect("wide content mask root");
        assert!(wide_panel.left() >= wide_mask.left() && wide_panel.right() <= wide_mask.right());
        assert!(wide_panel.top() >= wide_mask.top() && wide_panel.bottom() <= wide_mask.bottom());
        assert!(
            wide_panel.left() <= wide_link.right() + px(1.0)
                && wide_panel.right() >= wide_link.left() - px(1.0),
            "wide panel must be anchored beside its actual Link trigger"
        );
        cx.update(|window, app| {
            view.update(app, |view, view_cx| {
                view.cancel_link(&CancelLink, window, view_cx)
            });
        });

        cx.simulate_resize(size(px(760.0), px(820.0)));
        redraw(cx);
        let more = cx
            .debug_bounds("evernote-native-spike-more-trigger")
            .expect("narrow More trigger");
        cx.simulate_click(more.center(), Modifiers::default());
        redraw(cx);
        let narrow_link = cx.debug_bounds("Link").expect("narrow Link row");
        cx.simulate_click(narrow_link.center(), Modifiers::default());
        redraw(cx);
        let narrow_panel = cx
            .debug_bounds("evernote-link-popover")
            .expect("narrow Link panel");
        let narrow_mask = cx
            .debug_bounds("evernote-native-spike")
            .expect("narrow content mask root");
        assert!(
            narrow_panel.left() >= narrow_mask.left()
                && narrow_panel.right() <= narrow_mask.right()
                && narrow_panel.top() >= narrow_mask.top()
                && narrow_panel.bottom() <= narrow_mask.bottom()
        );
        assert!(
            narrow_panel.left() <= narrow_link.right() + px(1.0)
                && narrow_panel.right() >= narrow_link.left() - px(1.0),
            "narrow panel must be anchored beside its actual Link row"
        );
    }

    #[gpui::test]
    async fn shell_title_click_edit_and_return_focus_to_body(cx: &mut TestAppContext) {
        cx.update(|cx| components::init(cx));
        let (view, cx) = cx.add_window_view(build_view);
        cx.simulate_resize(size(px(760.0), px(820.0)));
        redraw(cx);
        let title = cx
            .debug_bounds("evernote-note-title")
            .expect("mounted title input");
        let surface = cx
            .debug_bounds("spike-editor-surface")
            .expect("mounted body");
        assert_eq!(
            title.left(),
            surface.left(),
            "narrow title/body left edges align"
        );
        cx.simulate_click(
            point(title.left() + px(1.0), title.center().y),
            Modifiers::default(),
        );
        cx.simulate_input("新");
        view.read_with(cx, |view, cx| {
            assert_eq!(view.title.read(cx).text(), "新会议记录")
        });
        cx.simulate_keystrokes("end backspace");
        view.read_with(cx, |view, cx| {
            assert_eq!(view.title.read(cx).text(), "新会议记")
        });
        cx.simulate_keystrokes("end shift-left");
        cx.simulate_input("录");
        view.read_with(cx, |view, cx| {
            assert_eq!(view.title.read(cx).text(), "新会议录");
        });
        cx.simulate_keystrokes("end shift-left");
        cx.write_to_clipboard(ClipboardItem::new_string("本".into()));
        cx.simulate_keystrokes("cmd-v");
        view.read_with(cx, |view, cx| {
            assert_eq!(view.title.read(cx).text(), "新会议本");
        });
        cx.simulate_keystrokes("end shift-left");
        cx.update(|window, app| {
            view.update(app, |view, view_cx| {
                view.title.update(view_cx, |title, title_cx| {
                    <TitleInput as EntityInputHandler>::replace_and_mark_text_in_range(
                        title,
                        None,
                        "候",
                        Some(1..1),
                        window,
                        title_cx,
                    );
                    <TitleInput as EntityInputHandler>::replace_text_in_range(
                        title, None, "后", window, title_cx,
                    );
                });
            });
        });
        view.read_with(cx, |view, cx| {
            assert_eq!(view.title.read(cx).text(), "新会议后");
        });
        cx.write_to_clipboard(ClipboardItem::new_string("剪贴板保留".into()));
        cx.simulate_keystrokes("cmd-x");
        assert_eq!(
            cx.read_from_clipboard().and_then(|item| item.text()),
            Some("剪贴板保留".into()),
            "Cmd-X with an empty title selection must not replace clipboard"
        );
        view.read_with(cx, |view, cx| {
            assert_eq!(view.title.read(cx).text(), "新会议后");
        });
        cx.simulate_keystrokes("enter");
        cx.update(|window, app| {
            view.read_with(app, |view, app| {
                assert!(view.editor.read(app).focus_handle().is_focused(window));
                assert!(!view.title.read(app).focus_handle().is_focused(window));
            });
        });
    }

    #[gpui::test]
    async fn shell_picker_cancel_preserves_title_focus_but_selection_restores_body_focus(
        cx: &mut TestAppContext,
    ) {
        cx.update(|cx| components::init(cx));
        let (view, cx) = cx.add_window_view(build_view);
        redraw(cx);
        let window_handle = cx
            .window_handle()
            .downcast::<SpikeView>()
            .expect("picker completion must retain the owning spike window");
        cx.update(|window, app| {
            view.read_with(app, |view, app| {
                view.title.read(app).focus_handle().focus(window)
            });
        });
        let cancelled_baseline = view.read_with(cx, |view, cx| {
            let editor = view.editor.read(cx);
            (
                editor.document().semantic_snapshot(),
                editor.undo_depth(),
                editor.selection(),
            )
        });
        cx.update(|_, app| {
            app.spawn(async move |mut app| {
                deliver_image_picker_completion(
                    window_handle.clone(),
                    ImagePickerCompletion::Cancelled,
                    &mut app,
                );
            })
            .detach();
        });
        cx.run_until_parked();
        cx.update(|window, app| {
            view.read_with(app, |view, view_cx| {
                assert!(view.title.read(view_cx).focus_handle().is_focused(window));
                let editor = view.editor.read(view_cx);
                assert_eq!(editor.document().semantic_snapshot(), cancelled_baseline.0);
                assert_eq!(editor.undo_depth(), cancelled_baseline.1);
                assert_eq!(editor.selection(), cancelled_baseline.2);
            });
        });
        let selected_baseline = view.read_with(cx, |view, cx| {
            let editor = view.editor.read(cx);
            (editor.document().block_count(), editor.undo_depth())
        });
        let path = std::env::temp_dir().join(format!(
            "joplin-lite-picker-focus-{}.png",
            std::process::id()
        ));
        std::fs::write(&path, valid_png_bytes()).expect("temporary picker image");
        let selected_path = path.clone();
        cx.update(|_, app| {
            app.spawn(async move |mut app| {
                deliver_image_picker_completion(
                    window_handle,
                    ImagePickerCompletion::Selected(selected_path),
                    &mut app,
                );
            })
            .detach();
        });
        cx.run_until_parked();
        let _ = std::fs::remove_file(path);
        cx.update(|window, app| {
            view.read_with(app, |view, app| {
                let editor = view.editor.read(app);
                assert!(editor.focus_handle().is_focused(window));
                assert!(!view.title.read(app).focus_handle().is_focused(window));
                assert_eq!(editor.undo_depth(), selected_baseline.1 + 1);
                assert!(editor.document().block_count() > selected_baseline.0);
                assert!(editor.document().blocks().iter().any(|block| matches!(
                    block.content,
                    crate::native_editor::model::BlockContent::Image { .. }
                )));
                assert!(
                    editor
                        .document()
                        .block(editor.selection().head.node_id)
                        .is_some()
                );
            });
        });
    }

    #[gpui::test]
    async fn shell_picker_selection_immediately_paints_the_inserted_image_in_the_current_surface(
        cx: &mut TestAppContext,
    ) {
        cx.update(|cx| components::init(cx));
        let image_cache =
            cx.update(|app| BudgetedImageCache::new_entity(app, DECODED_IMAGE_CACHE_BUDGET));
        let view_cache = image_cache.clone();
        let (view, cx) = cx.add_window_view(move |window, cx| {
            let editor = cx.new(|cx| EditorCore::new(sample_document(), cx));
            editor.read(cx).focus_handle().focus(window);
            SpikeView {
                editor,
                title: cx.new(|cx| TitleInput::new("会议记录".into(), cx)),
                image_cache: Some(view_cache.clone()),
                catalogue: CommandCatalogue::default(),
                scroll_handle: ScrollHandle::new(),
                more_open: false,
                link_popover: None,
                pointer_anchor: None,
                drop_point: None,
                more_trigger_bounds: None,
                measurement: None,
            }
        });
        cx.simulate_resize(size(px(760.0), px(500.0)));
        redraw(cx);
        let surface_before = cx
            .debug_bounds("spike-editor-surface")
            .expect("mounted editor surface");
        crate::native_editor::render::reset_test_image_residency_observation();
        let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("assets/showcase/1.png");
        assert!(
            path.is_file(),
            "the manual-acceptance picker image must exist"
        );

        let window_handle = cx
            .window_handle()
            .downcast::<SpikeView>()
            .expect("picker callback must retain its owning spike window");
        let parent_notifications = Arc::new(AtomicUsize::new(0));
        let observed_parent_notifications = parent_notifications.clone();
        let _parent_observer = cx.update(|_, app| {
            app.observe(&view, move |_, _| {
                observed_parent_notifications.fetch_add(1, Ordering::Relaxed);
            })
        });
        cx.update(|_, app| {
            app.spawn(async move |mut app| {
                deliver_image_picker_completion(
                    window_handle,
                    ImagePickerCompletion::Selected(path),
                    &mut app,
                );
            })
            .detach();
        });
        cx.run_until_parked();

        let observation = crate::native_editor::render::test_image_residency_observation()
            .expect("picker completion must schedule the current surface for painting");
        assert_eq!(observation.image_bounds.len(), 1);
        assert_eq!(observation.visible_indices.len(), 1);
        assert!(
            observation.visible_bounds[0].top() < observation.content_mask.bottom(),
            "the inserted image must belong to the current surface's visible render snapshot"
        );
        let resource_id = view.read_with(cx, |view, cx| {
            view.editor
                .read(cx)
                .document()
                .blocks()
                .iter()
                .find_map(|block| match &block.content {
                    crate::native_editor::model::BlockContent::Image { resource_id, .. } => {
                        Some(resource_id.clone())
                    }
                    _ => None,
                })
                .expect("picker transaction must create an image block")
        });
        assert_eq!(
            view.read_with(cx, |view, cx| view
                .editor
                .read(cx)
                .image_state(&resource_id)),
            Some(crate::native_editor::images::ImageNodeState::Loaded),
            "the current surface must repaint after its cache decode, not leave the inserted image loading forever"
        );
        assert!(
            view.read_with(cx, |view, _| view.scroll_handle.max_offset().height) > px(0.0),
            "the enclosing scroll surface must grow after an async picker image insertion"
        );
        assert!(
            cx.debug_bounds("spike-editor-surface")
                .expect("mounted editor surface after picker completion")
                .size
                .height
                > surface_before.size.height,
            "the current surface must be relaid out instead of clipping the inserted image at its old height"
        );
        assert!(
            parent_notifications.load(Ordering::Relaxed) > 0,
            "the typed completion entry must notify the owning SpikeView after the visible surface contract holds"
        );
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
    async fn shell_narrow_single_row_toolbar_aligns_writing_column_and_keeps_live_membership(
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
        let undo = cx.debug_bounds("Undo").expect("narrow primary action");
        let more = cx
            .debug_bounds("evernote-native-spike-more-trigger")
            .expect("narrow overflow trigger");
        assert_eq!(
            toolbar.size.height,
            px(44.0),
            "primary toolbar owns one exact 44pt row"
        );
        assert_eq!(
            undo.center().y,
            more.center().y,
            "overflow keeps primary actions and More on one flex row"
        );
        let title = cx
            .debug_bounds("evernote-note-title")
            .expect("narrow title should be mounted");
        assert_eq!(
            title.left(),
            surface.left(),
            "title and body share a left writing edge below the wide breakpoint"
        );
        assert!(
            surface.top() > px(118.0),
            "surface origin must include the single-row chrome rather than a fixed shell offset"
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
