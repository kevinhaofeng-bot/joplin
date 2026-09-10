use crate::native_editor::commands::EditorCommand;
use gpui::{
    Bounds, Context, EntityInputHandler, FocusHandle, Pixels, Point, ShapedLine, UTF16Selection,
    Window,
};
use std::ops::Range;
use unicode_segmentation::UnicodeSegmentation;

pub const EVERNOTE_GREEN: u32 = 0x00a82dff;
pub const NOTE_BODY_MAX_WIDTH: f32 = 680.0;
pub const EDITOR_HEADER_MAX_WIDTH: f32 = 1060.0;

#[derive(Clone, Debug, PartialEq)]
pub struct EditorChromeMetrics {
    pub header_width: f32,
    pub body_width: f32,
    pub body_left_in_header: f32,
    pub toolbar_height: f32,
    pub bottom_padding: f32,
}

pub fn editor_chrome_metrics(viewport_width: f32, viewport_height: f32) -> EditorChromeMetrics {
    let header_width = (viewport_width - 64.0)
        .max(1.0)
        .min(EDITOR_HEADER_MAX_WIDTH);
    let body_width = NOTE_BODY_MAX_WIDTH.min((header_width - 16.0).max(1.0));
    EditorChromeMetrics {
        header_width,
        body_width,
        body_left_in_header: (header_width - body_width) / 2.0,
        toolbar_height: 44.0,
        bottom_padding: viewport_height.max(0.0) * 3.0 / 10.0,
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ToolbarPlacement {
    pub primary: Vec<EditorCommand>,
    pub overflow: Vec<EditorCommand>,
}

/// The note title is a distinct GPUI input entity. Its offsets are byte
/// offsets internally; platform input remains UTF-16 at this narrow bridge.
pub struct TitleInput {
    text: String,
    selection: Range<usize>,
    marked_range: Option<Range<usize>>,
    focus: FocusHandle,
    last_bounds: Option<Bounds<Pixels>>,
    last_layout: Option<ShapedLine>,
    pointer_anchor: Option<usize>,
}

impl TitleInput {
    pub fn new(text: String, cx: &mut Context<Self>) -> Self {
        let end = text.len();
        Self {
            text,
            selection: end..end,
            marked_range: None,
            focus: cx.focus_handle(),
            last_bounds: None,
            last_layout: None,
            pointer_anchor: None,
        }
    }

    pub fn text(&self) -> &str {
        &self.text
    }

    pub fn selection(&self) -> &Range<usize> {
        &self.selection
    }

    pub fn marked_range(&self) -> Option<&Range<usize>> {
        self.marked_range.as_ref()
    }

    pub fn focus_handle(&self) -> &FocusHandle {
        &self.focus
    }

    pub fn moves_focus_to_body_for(key: &str) -> bool {
        matches!(key, "enter" | "down")
    }

    pub fn selected_text(&self) -> &str {
        let range = ordered_range(&self.selection);
        self.text.get(range).unwrap_or_default()
    }

    pub fn select_all(&mut self) {
        self.selection = 0..self.text.len();
        self.marked_range = None;
    }

    pub fn collapse(&mut self, index: usize) {
        let index = grapheme_boundary_at_or_before(&self.text, index);
        self.selection = index..index;
        self.marked_range = None;
    }

    pub fn move_to_edge(&mut self, end: bool, extend: bool) {
        let index = if end { self.text.len() } else { 0 };
        if extend {
            self.selection.end = index;
        } else {
            self.selection = index..index;
        }
        self.marked_range = None;
    }

    pub fn move_horizontal(&mut self, right: bool, extend: bool) {
        let ordered = ordered_range(&self.selection);
        let index = if extend {
            self.selection.end
        } else if !ordered.is_empty() {
            if right { ordered.end } else { ordered.start }
        } else {
            self.selection.end
        };
        let next = if right {
            next_grapheme_boundary(&self.text, index)
        } else {
            previous_grapheme_boundary(&self.text, index)
        };
        if extend {
            self.selection.end = next;
        } else {
            self.selection = next..next;
        }
        self.marked_range = None;
    }

    pub fn delete_backward(&mut self) {
        let range = ordered_range(&self.selection);
        let range = if range.is_empty() {
            previous_grapheme_boundary(&self.text, range.start)..range.start
        } else {
            range
        };
        self.replace_byte_range(range);
    }

    pub fn delete_forward(&mut self) {
        let range = ordered_range(&self.selection);
        let range = if range.is_empty() {
            range.end..next_grapheme_boundary(&self.text, range.end)
        } else {
            range
        };
        self.replace_byte_range(range);
    }

    pub fn paste_from_clipboard(&mut self, cx: &mut Context<Self>) {
        if let Some(text) = cx.read_from_clipboard().and_then(|item| item.text()) {
            self.replace_utf16(None, &text);
            cx.notify();
        }
    }

    fn replace_byte_range(&mut self, range: Range<usize>) {
        if range.start > range.end
            || !self.text.is_char_boundary(range.start)
            || !self.text.is_char_boundary(range.end)
        {
            return;
        }
        self.text.replace_range(range.clone(), "");
        self.selection = range.start..range.start;
        self.marked_range = None;
    }

    pub fn record_layout(&mut self, bounds: Bounds<Pixels>, line: ShapedLine) {
        self.last_bounds = Some(bounds);
        self.last_layout = Some(line);
    }

    pub fn record_bounds(&mut self, bounds: Bounds<Pixels>) {
        self.last_bounds = Some(bounds);
    }

    pub fn begin_pointer_selection(&mut self, point: Point<Pixels>, extend: bool) -> Option<usize> {
        let index = self.byte_index_for_point(point)?;
        let anchor = if extend { self.selection.start } else { index };
        self.selection = anchor.min(index)..anchor.max(index);
        self.pointer_anchor = Some(anchor);
        self.marked_range = None;
        Some(index)
    }

    pub fn extend_pointer_selection(&mut self, point: Point<Pixels>) -> Option<usize> {
        let index = self.byte_index_for_point(point)?;
        let anchor = self.pointer_anchor.unwrap_or(self.selection.start);
        self.selection = anchor.min(index)..anchor.max(index);
        self.marked_range = None;
        Some(index)
    }

    pub fn end_pointer_selection(&mut self) {
        self.pointer_anchor = None;
    }

    fn byte_index_for_point(&self, point: Point<Pixels>) -> Option<usize> {
        let bounds = self.last_bounds?;
        let line = self.last_layout.as_ref()?;
        let x = (point.x - bounds.left()).max(gpui::px(0.0)).min(line.width);
        Some(line.closest_index_for_x(x).min(self.text.len()))
    }

    fn checked_utf16_range(&self, range: &Range<usize>) -> Option<Range<usize>> {
        if range.start > range.end
            || !is_utf16_boundary(&self.text, range.start)
            || !is_utf16_boundary(&self.text, range.end)
        {
            return None;
        }
        Some(crate::native_editor::input::utf16_range_to_utf8_in(
            &self.text, range,
        ))
    }

    fn replace_utf16(&mut self, range: Option<Range<usize>>, replacement: &str) -> bool {
        let range = match range {
            Some(range) => match self.checked_utf16_range(&range) {
                Some(range) => range,
                None => return false,
            },
            None => self
                .marked_range
                .clone()
                .unwrap_or_else(|| self.selection.clone()),
        };
        if !self.text.is_char_boundary(range.start) || !self.text.is_char_boundary(range.end) {
            return false;
        }
        self.text.replace_range(range.clone(), replacement);
        let caret = range.start + replacement.len();
        self.selection = caret..caret;
        self.marked_range = None;
        true
    }

    fn replace_and_mark_utf16(
        &mut self,
        range: Option<Range<usize>>,
        replacement: &str,
        selected_range: Option<Range<usize>>,
    ) -> bool {
        let range = match range {
            Some(range) => match self.checked_utf16_range(&range) {
                Some(range) => range,
                None => return false,
            },
            None => self
                .marked_range
                .clone()
                .unwrap_or_else(|| self.selection.clone()),
        };
        let selected = match selected_range {
            Some(range)
                if range.start <= range.end
                    && is_utf16_boundary(replacement, range.start)
                    && is_utf16_boundary(replacement, range.end) =>
            {
                Some(crate::native_editor::input::utf16_range_to_utf8_in(
                    replacement,
                    &range,
                ))
            }
            Some(_) => return false,
            None => None,
        };
        if !self.text.is_char_boundary(range.start) || !self.text.is_char_boundary(range.end) {
            return false;
        }
        let start = range.start;
        self.text.replace_range(range, replacement);
        self.selection = selected
            .map(|range| start + range.start..start + range.end)
            .unwrap_or_else(|| start + replacement.len()..start + replacement.len());
        self.marked_range = (!replacement.is_empty()).then_some(start..start + replacement.len());
        true
    }

    fn unmark(&mut self) {
        self.marked_range = None;
    }
}

fn ordered_range(range: &Range<usize>) -> Range<usize> {
    range.start.min(range.end)..range.start.max(range.end)
}

fn grapheme_boundary_at_or_before(text: &str, index: usize) -> usize {
    let index = index.min(text.len());
    UnicodeSegmentation::grapheme_indices(text, true)
        .map(|(start, _)| start)
        .chain(std::iter::once(text.len()))
        .take_while(|boundary| *boundary <= index)
        .last()
        .unwrap_or(0)
}

fn previous_grapheme_boundary(text: &str, index: usize) -> usize {
    grapheme_boundary_at_or_before(text, index.saturating_sub(1))
}

fn next_grapheme_boundary(text: &str, index: usize) -> usize {
    let index = index.min(text.len());
    UnicodeSegmentation::grapheme_indices(text, true)
        .map(|(start, _)| start)
        .chain(std::iter::once(text.len()))
        .find(|boundary| *boundary > index)
        .unwrap_or(text.len())
}

fn is_utf16_boundary(text: &str, offset: usize) -> bool {
    let mut units = 0;
    if offset == 0 {
        return true;
    }
    for character in text.chars() {
        units += character.len_utf16();
        if units == offset {
            return true;
        }
        if units > offset {
            return false;
        }
    }
    false
}

impl EntityInputHandler for TitleInput {
    fn text_for_range(
        &mut self,
        range_utf16: Range<usize>,
        adjusted_range: &mut Option<Range<usize>>,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<String> {
        let range = self.checked_utf16_range(&range_utf16)?;
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
            reversed: false,
        })
    }

    fn marked_text_range(
        &self,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<Range<usize>> {
        self.marked_range
            .as_ref()
            .map(|range| crate::native_editor::input::utf8_range_to_utf16_in(&self.text, range))
    }

    fn unmark_text(&mut self, _window: &mut Window, cx: &mut Context<Self>) {
        self.unmark();
        cx.notify();
    }

    fn replace_text_in_range(
        &mut self,
        range: Option<Range<usize>>,
        text: &str,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.replace_utf16(range, text) {
            cx.notify();
        }
    }

    fn replace_and_mark_text_in_range(
        &mut self,
        range: Option<Range<usize>>,
        text: &str,
        selected_range: Option<Range<usize>>,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.replace_and_mark_utf16(range, text, selected_range) {
            cx.notify();
        }
    }

    fn bounds_for_range(
        &mut self,
        _range_utf16: Range<usize>,
        element_bounds: Bounds<Pixels>,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<Bounds<Pixels>> {
        Some(element_bounds)
    }

    fn character_index_for_point(
        &mut self,
        point: gpui::Point<Pixels>,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<usize> {
        self.byte_index_for_point(point)
            .map(|index| crate::native_editor::input::utf8_to_utf16_in(&self.text, index))
    }
}

const PRIMARY_ORDER: [EditorCommand; 15] = [
    EditorCommand::InsertImage,
    EditorCommand::Undo,
    EditorCommand::Redo,
    EditorCommand::Paragraph,
    EditorCommand::Bold,
    EditorCommand::Italic,
    EditorCommand::Underline,
    EditorCommand::Highlight,
    EditorCommand::BulletList,
    EditorCommand::OrderedList,
    EditorCommand::CheckList,
    EditorCommand::Link,
    EditorCommand::AlignLeft,
    EditorCommand::AlignCenter,
    EditorCommand::AlignRight,
];

/// Hand-measured hit target, separator, and inter-group spacing costs.  They
/// deliberately describe controls, rather than inspecting shaped labels, so
/// responsive placement is stable across fonts and locales.
fn toolbar_width_cost(command: EditorCommand) -> f32 {
    match command {
        EditorCommand::Paragraph => 78.0,
        EditorCommand::InsertImage | EditorCommand::Link => 58.0,
        EditorCommand::AlignLeft | EditorCommand::AlignCenter | EditorCommand::AlignRight => 62.0,
        _ => 50.0,
    }
}

fn toolbar_total_width(commands: &[EditorCommand]) -> f32 {
    let command_width = commands
        .iter()
        .copied()
        .map(toolbar_width_cost)
        .sum::<f32>();
    let separators = 5.0 * 9.0;
    let more_trigger = 52.0;
    command_width + separators + more_trigger
}

pub fn toolbar_placement(available_width: f32) -> ToolbarPlacement {
    let mut primary = PRIMARY_ORDER.to_vec();
    let mut overflow = Vec::new();
    // These are intentionally the first commands to leave: the core writing
    // controls retain a single row before the writing column is narrowed.
    const OVERFLOW_PRIORITY: [EditorCommand; 11] = [
        EditorCommand::AlignRight,
        EditorCommand::AlignCenter,
        EditorCommand::AlignLeft,
        EditorCommand::Link,
        EditorCommand::CheckList,
        EditorCommand::OrderedList,
        EditorCommand::BulletList,
        EditorCommand::Highlight,
        EditorCommand::Underline,
        EditorCommand::Italic,
        EditorCommand::Bold,
    ];
    for command in OVERFLOW_PRIORITY {
        if toolbar_total_width(&primary) <= available_width.max(0.0) {
            break;
        }
        if let Some(index) = primary.iter().position(|candidate| *candidate == command) {
            primary.remove(index);
            overflow.push(command);
        }
    }
    overflow.reverse();
    ToolbarPlacement { primary, overflow }
}

#[cfg(test)]
mod tests {
    use super::{TitleInput, editor_chrome_metrics, toolbar_placement};
    use crate::native_editor::commands::EditorCommand;
    use gpui::AppContext;

    #[test]
    fn chrome_metrics_keep_the_evernote_writing_column_centered() {
        let wide = editor_chrome_metrics(1200.0, 820.0);
        assert_eq!(wide.header_width, 1060.0);
        assert_eq!(wide.body_width, 680.0);
        assert_eq!(wide.body_left_in_header, 190.0);
        assert_eq!(wide.toolbar_height, 44.0);
        assert_eq!(wide.bottom_padding, 246.0);

        let narrow = editor_chrome_metrics(760.0, 820.0);
        assert_eq!(narrow.header_width, 696.0);
        assert_eq!(narrow.body_width, 680.0);
        assert_eq!(narrow.body_left_in_header, 8.0);
    }

    #[test]
    fn toolbar_moves_only_low_priority_actions_to_overflow_without_losing_commands() {
        let wide = toolbar_placement(1060.0);
        assert_eq!(
            wide.primary,
            vec![
                EditorCommand::InsertImage,
                EditorCommand::Undo,
                EditorCommand::Redo,
                EditorCommand::Paragraph,
                EditorCommand::Bold,
                EditorCommand::Italic,
                EditorCommand::Underline,
                EditorCommand::Highlight,
                EditorCommand::BulletList,
                EditorCommand::OrderedList,
                EditorCommand::CheckList,
                EditorCommand::Link,
                EditorCommand::AlignLeft,
                EditorCommand::AlignCenter,
                EditorCommand::AlignRight,
            ]
        );
        let narrow = toolbar_placement(696.0);
        assert_eq!(
            narrow.overflow,
            vec![
                EditorCommand::Link,
                EditorCommand::AlignLeft,
                EditorCommand::AlignCenter,
                EditorCommand::AlignRight,
            ]
        );
        let mut all = narrow.primary.clone();
        all.extend(narrow.overflow);
        assert_eq!(all.len(), 15);
        all.sort_by_key(|command| format!("{command:?}"));
        all.dedup();
        assert_eq!(
            all.len(),
            15,
            "responsive placement must not duplicate commands"
        );
    }

    #[gpui::test]
    fn title_input_replaces_utf16_selection_and_preserves_ime_marked_text(
        cx: &mut gpui::TestAppContext,
    ) {
        let title = cx.new(|cx| TitleInput::new("会议😀记录".into(), cx));
        title.update(cx, |title, _| {
            assert!(title.replace_utf16(Some(2..4), "你好"));
            assert_eq!(title.text(), "会议你好记录");
            assert!(title.replace_and_mark_utf16(None, "候", Some(0..1)));
            assert!(title.marked_range().is_some());
            title.unmark();
            assert!(title.marked_range().is_none());
            assert_eq!(title.text(), "会议你好候记录");
        });
    }

    #[gpui::test]
    fn title_input_rejects_a_surrogate_split_without_changing_title_or_selection(
        cx: &mut gpui::TestAppContext,
    ) {
        let title = cx.new(|cx| TitleInput::new("😀标题".into(), cx));
        title.update(cx, |title, _| {
            let baseline = (title.text().to_owned(), title.selection().clone());
            assert!(!title.replace_utf16(Some(1..1), "x"));
            assert_eq!(
                (title.text().to_owned(), title.selection().clone()),
                baseline
            );
            assert!(TitleInput::moves_focus_to_body_for("enter"));
            assert!(TitleInput::moves_focus_to_body_for("down"));
        });
    }

    #[gpui::test]
    fn title_input_edits_by_grapheme_without_splitting_utf8_or_utf16(
        cx: &mut gpui::TestAppContext,
    ) {
        let title = cx.new(|cx| TitleInput::new("甲🙂e\u{301}乙".into(), cx));
        title.update(cx, |title, _| {
            title.move_to_edge(true, false);
            title.delete_backward();
            assert_eq!(title.text(), "甲🙂e\u{301}");
            title.delete_backward();
            assert_eq!(title.text(), "甲🙂");
            title.move_horizontal(false, true);
            assert_eq!(title.selected_text(), "🙂");
            title.delete_forward();
            assert_eq!(title.text(), "甲");
            title.select_all();
            assert_eq!(title.selected_text(), "甲");
        });
    }
}
