use crate::app::ListViewMode;
use crate::native_editor::images::BudgetedImageCache;
use app_lite_core::{NoteId, NoteProjection};
use gpui::{
    AnyElement, Entity, InteractiveElement, IntoElement, ObjectFit, ParentElement, Styled,
    StyledImage, StyledText, div, img, px, rgba,
};
#[cfg(test)]
use gpui::{
    App, Bounds, Element, ElementId, GlobalElementId, InspectorElementId, LayoutId, Pixels, Window,
};
#[cfg(test)]
use std::cell::RefCell;
#[cfg(test)]
use std::collections::HashMap;
use std::hash::{Hash, Hasher};
use std::path::PathBuf;
#[cfg(test)]
use std::rc::Rc;

const CARD_PADDING: f32 = 12.0;
const CARD_THUMBNAIL_SIZE: f32 = 76.0;
const CARD_TITLE_LINE_HEIGHT: f32 = 20.0;
const CARD_SNIPPET_LINE_HEIGHT: f32 = 16.0;
const CARD_SNIPPET_LINES: usize = 2;
const CARD_TIME_LINE_HEIGHT: f32 = 16.0;

pub const fn fixed_height(mode: ListViewMode) -> f32 {
    match mode {
        ListViewMode::Cards => 112.0,
        ListViewMode::Snippets => 88.0,
        ListViewMode::Compact => 56.0,
    }
}

pub fn render(
    projection: &NoteProjection,
    selected: bool,
    mode: ListViewMode,
    thumbnail_source: Option<PathBuf>,
    thumbnail_failed: bool,
    thumbnail_cache: &Entity<BudgetedImageCache>,
) -> AnyElement {
    let title = if projection.title_prefix.trim().is_empty() {
        "无标题笔记"
    } else {
        &projection.title_prefix
    };
    let time = relative_time(projection.updated_time);
    let mode_selector = match mode {
        ListViewMode::Cards => "library-note-card-cards",
        ListViewMode::Snippets => "library-note-card-snippets",
        ListViewMode::Compact => "library-note-card-compact",
    };
    let card = div()
        .id(("note-card", stable_element_id(projection.id.as_str())))
        .debug_selector(|| "library-note-card".to_owned())
        .w_full()
        .h(px(fixed_height(mode)))
        .overflow_hidden()
        .p(px(CARD_PADDING))
        .rounded(px(8.0))
        .bg(if selected {
            rgba(0x00a82d19)
        } else {
            rgba(0x00000000)
        })
        .hover(|card| card.bg(rgba(0x00a82d10)))
        .text_color(rgba(0x202720ff));

    match mode {
        ListViewMode::Cards => {
            // A note without a thumbnail key is intentionally text-first.
            // The neutral 76pt reservation is only a loading affordance for
            // an actual selected-thumbnail identity, never a decorative empty
            // square on ordinary notes.
            if projection.selected_thumbnail_id.is_none() {
                return card
                    .child(cards_text_column(
                        title,
                        &projection.snippet,
                        &time,
                        projection.id.as_str(),
                        mode_selector,
                    ))
                    .into_any_element();
            }
            // A projection thumbnail is an identity, never embedded card
            // bytes. The list shell supplies a managed source only after its
            // background verified-reader task has completed; until then this
            // fixed-size neutral block reserves the exact card geometry.
            let thumbnail = match (thumbnail_failed, thumbnail_source) {
                (true, _) => div()
                    .id((
                        "note-card-thumbnail-error",
                        stable_element_id(projection.id.as_str()),
                    ))
                    .debug_selector(|| "library-note-card-thumbnail-error".to_owned())
                    .w(px(CARD_THUMBNAIL_SIZE))
                    .h(px(CARD_THUMBNAIL_SIZE))
                    .flex_none()
                    .rounded(px(6.0))
                    .bg(rgba(0xfff3e8ff))
                    .text_color(rgba(0x9a4a14ff))
                    .text_size(px(10.0))
                    .p(px(8.0))
                    .child("缩略图无法加载")
                    .child(
                        div()
                            .mt(px(4.0))
                            .text_size(px(9.0))
                            .text_color(rgba(0x8a674dff))
                            .child("离开后重试"),
                    )
                    .into_any_element(),
                (false, Some(source)) => div()
                    .id((
                        "note-card-thumbnail",
                        stable_element_id(projection.id.as_str()),
                    ))
                    .debug_selector(|| "library-note-card-thumbnail".to_owned())
                    .w(px(CARD_THUMBNAIL_SIZE))
                    .h(px(CARD_THUMBNAIL_SIZE))
                    .flex_none()
                    .overflow_hidden()
                    .rounded(px(6.0))
                    .bg(rgba(0xf0f3f0ff))
                    .child(
                        img(source)
                            .size_full()
                            .object_fit(ObjectFit::Cover)
                            .image_cache(thumbnail_cache),
                    )
                    .into_any_element(),
                (false, None) => div()
                    .id((
                        "note-card-thumbnail-placeholder",
                        stable_element_id(projection.id.as_str()),
                    ))
                    .debug_selector(|| "library-note-card-thumbnail-placeholder".to_owned())
                    .w(px(CARD_THUMBNAIL_SIZE))
                    .h(px(CARD_THUMBNAIL_SIZE))
                    .flex_none()
                    .rounded(px(6.0))
                    .bg(rgba(0xf0f3f0ff))
                    .into_any_element(),
            };
            card.flex()
                .items_center()
                .gap(px(10.0))
                .child(thumbnail)
                .child(
                    div()
                        .flex_1()
                        .h_full()
                        .min_h(px(0.0))
                        .min_w(px(0.0))
                        .overflow_hidden()
                        .child(cards_text_column(
                            title,
                            &projection.snippet,
                            &time,
                            projection.id.as_str(),
                            mode_selector,
                        )),
                )
                .into_any_element()
        }
        ListViewMode::Snippets => card
            .child(snippets_text_column(
                title,
                &projection.snippet,
                &time,
                projection.id.as_str(),
                mode_selector,
            ))
            .into_any_element(),
        ListViewMode::Compact => {
            let card = card.child(
                div()
                    .debug_selector(move || mode_selector.to_owned())
                    .text_size(px(15.0))
                    .font_weight(gpui::FontWeight::SEMIBOLD)
                    .child(snippet_text(
                        "title",
                        projection.id.as_str(),
                        title.to_owned(),
                    )),
            );
            card.into_any_element()
        }
    }
}

/// Keep the existing fixed 88pt Snippets row honest: after the 12pt outer
/// padding there are only 64pt left for a title, preview, and relative date.
/// The independent local density is one 20pt title line, one 16pt preview
/// line, and one 16pt footer; `justify_between` spends the remaining space
/// without letting a soft-wrapped title or preview overlap its neighbour.
///
/// Evernote's card CSS supplies the hierarchy (clamped title, clamped snippet,
/// and `snippetDate` footer). This exact fixed-row allocation is deliberately
/// a GPUI presentation adaptation, not a claim of pixel-identical donor CSS.
fn snippets_text_column(
    title: &str,
    snippet: &str,
    time: &str,
    note_id: &str,
    mode_selector: &str,
) -> AnyElement {
    let title_selector = snippet_text_selector("title", note_id);
    let snippet_selector = snippet_text_selector("snippet", note_id);
    let time_selector = snippet_text_selector("time", note_id);
    let mode_selector = mode_selector.to_owned();
    div()
        .w_full()
        .h_full()
        .min_h(px(0.0))
        .min_w(px(0.0))
        .flex()
        .flex_col()
        .justify_between()
        .overflow_hidden()
        .child(
            div()
                .debug_selector(move || title_selector.clone())
                .w_full()
                .h(px(CARD_TITLE_LINE_HEIGHT))
                .min_w(px(0.0))
                .overflow_hidden()
                .text_size(px(15.0))
                .line_height(px(CARD_TITLE_LINE_HEIGHT))
                .font_weight(gpui::FontWeight::SEMIBOLD)
                .line_clamp(1)
                .text_ellipsis()
                .child(snippet_text("title", note_id, title.to_owned())),
        )
        .child(
            div()
                .debug_selector(move || snippet_selector.clone())
                .w_full()
                .h(px(CARD_SNIPPET_LINE_HEIGHT))
                .min_w(px(0.0))
                .overflow_hidden()
                .text_size(px(12.0))
                .line_height(px(CARD_SNIPPET_LINE_HEIGHT))
                .line_clamp(1)
                .text_ellipsis()
                .text_color(rgba(0x647064ff))
                .child(snippet_text("snippet", note_id, snippet.to_owned())),
        )
        .child(
            div()
                .debug_selector(move || time_selector.clone())
                .w_full()
                .h(px(CARD_TIME_LINE_HEIGHT))
                .min_w(px(0.0))
                .overflow_hidden()
                .text_size(px(11.0))
                .line_height(px(CARD_TIME_LINE_HEIGHT))
                .line_clamp(1)
                .text_ellipsis()
                .text_color(rgba(0x8b958cff))
                .child(snippet_text("time", note_id, time.to_owned())),
        )
        .debug_selector(move || mode_selector.clone())
        .into_any_element()
}

/// The fixed 112pt Cards row has an 88pt content area after its outer
/// padding.  Constrain all text to that budget before `items_center` has a
/// chance to vertically centre a taller column across adjacent rows.
///
/// Evernote's source CSS establishes the hierarchy (semibold clamped title,
/// separately clamped snippet, and footer date); one-line title density here
/// is a local GPUI adaptation for a narrow single-column 76pt-thumbnail row.
fn cards_text_column(
    title: &str,
    snippet: &str,
    time: &str,
    note_id: &str,
    mode_selector: &str,
) -> AnyElement {
    let title_selector = card_text_selector("title", note_id);
    let snippet_selector = card_text_selector("snippet", note_id);
    let time_selector = card_text_selector("time", note_id);
    let mode_selector = mode_selector.to_owned();
    div()
        .h_full()
        .min_h(px(0.0))
        .min_w(px(0.0))
        .flex()
        .flex_col()
        .justify_between()
        .overflow_hidden()
        .child(
            div()
                .debug_selector(move || title_selector.clone())
                .text_size(px(13.0))
                .line_height(px(CARD_TITLE_LINE_HEIGHT))
                .font_weight(gpui::FontWeight::SEMIBOLD)
                .h(px(CARD_TITLE_LINE_HEIGHT))
                .line_clamp(1)
                .text_ellipsis()
                .child(card_text("title", note_id, title.to_owned())),
        )
        .child(
            div()
                .debug_selector(move || snippet_selector.clone())
                .h(px(CARD_SNIPPET_LINE_HEIGHT * CARD_SNIPPET_LINES as f32))
                .text_size(px(13.0))
                .line_height(px(CARD_SNIPPET_LINE_HEIGHT))
                .line_clamp(CARD_SNIPPET_LINES)
                .text_color(rgba(0x647064ff))
                .child(card_text("snippet", note_id, snippet.to_owned())),
        )
        .child(
            div()
                .debug_selector(move || time_selector.clone())
                .h(px(CARD_TIME_LINE_HEIGHT))
                .text_size(px(11.0))
                .line_height(px(CARD_TIME_LINE_HEIGHT))
                .truncate()
                .text_color(rgba(0x8b958cff))
                .child(card_text("time", note_id, time.to_owned())),
        )
        .debug_selector(move || mode_selector.clone())
        .into_any_element()
}

/// Build the production text element, with a test-only transparent observer.
///
/// Release builds remain the raw `StyledText` element. Test builds wrap that
/// same element only to observe its resulting shaped layout after paint; they
/// do not substitute a second text implementation or change its text budget.
fn card_text(part: &'static str, note_id: &str, text: String) -> AnyElement {
    #[cfg(test)]
    {
        return ObservedCardText::new(card_text_selector(part, note_id), text).into_any_element();
    }

    #[cfg(not(test))]
    {
        let _ = (part, note_id);
        StyledText::new(text).into_any_element()
    }
}

/// Build the pre-existing Snippets text element, with the same test-only
/// post-paint observer used by Cards. Release builds still use raw
/// `StyledText`; this helper only gives the mounted test access to the actual
/// shaped text that was already going to be painted.
fn snippet_text(part: &'static str, note_id: &str, text: String) -> AnyElement {
    #[cfg(test)]
    {
        return ObservedCardText::new(snippet_text_selector(part, note_id), text)
            .into_any_element();
    }

    #[cfg(not(test))]
    {
        let _ = (part, note_id);
        StyledText::new(text).into_any_element()
    }
}

/// Test-only thin wrapper around the production `StyledText` element.
#[cfg(test)]
struct ObservedCardText {
    text: StyledText,
    selector: String,
}

#[cfg(test)]
impl ObservedCardText {
    fn new(selector: String, text: String) -> Self {
        Self {
            text: StyledText::new(text),
            selector,
        }
    }
}

#[cfg(test)]
impl IntoElement for ObservedCardText {
    type Element = Self;

    fn into_element(self) -> Self::Element {
        self
    }
}

#[cfg(test)]
impl Element for ObservedCardText {
    type RequestLayoutState = ();
    type PrepaintState = ();

    fn id(&self) -> Option<ElementId> {
        None
    }

    fn source_location(&self) -> Option<&'static std::panic::Location<'static>> {
        None
    }

    fn request_layout(
        &mut self,
        id: Option<&GlobalElementId>,
        inspector_id: Option<&InspectorElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (LayoutId, Self::RequestLayoutState) {
        self.text.request_layout(id, inspector_id, window, cx)
    }

    fn prepaint(
        &mut self,
        id: Option<&GlobalElementId>,
        inspector_id: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        request_layout: &mut Self::RequestLayoutState,
        window: &mut Window,
        cx: &mut App,
    ) -> Self::PrepaintState {
        self.text
            .prepaint(id, inspector_id, bounds, request_layout, window, cx)
    }

    fn paint(
        &mut self,
        id: Option<&GlobalElementId>,
        inspector_id: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        request_layout: &mut Self::RequestLayoutState,
        prepaint: &mut Self::PrepaintState,
        window: &mut Window,
        cx: &mut App,
    ) {
        self.text.paint(
            id,
            inspector_id,
            bounds,
            request_layout,
            prepaint,
            window,
            cx,
        );
        #[cfg(test)]
        record_card_text_paint(
            &self.selector,
            self.text.layout().wrapped_text(),
            self.text.layout().bounds(),
            self.text.layout().line_height(),
        );
    }
}

#[cfg(test)]
#[derive(Clone, Debug)]
pub(crate) struct PaintedCardText {
    pub painted_text: String,
    pub painted_line_count: usize,
    pub bounds: Bounds<Pixels>,
    pub line_height: Pixels,
}

#[cfg(test)]
pub(crate) type CardTextPaintProbe = Rc<RefCell<HashMap<String, PaintedCardText>>>;

#[cfg(test)]
thread_local! {
    static CARD_TEXT_PAINT_PROBE: RefCell<Option<CardTextPaintProbe>> = const { RefCell::new(None) };
}

#[cfg(test)]
pub(crate) struct CardTextPaintProbeScope {
    previous: Option<CardTextPaintProbe>,
}

#[cfg(test)]
impl Drop for CardTextPaintProbeScope {
    fn drop(&mut self) {
        CARD_TEXT_PAINT_PROBE.with(|active| {
            active.replace(self.previous.take());
        });
    }
}

#[cfg(test)]
pub(crate) fn observe_card_text_paints_for_test() -> (CardTextPaintProbe, CardTextPaintProbeScope) {
    let probe = Rc::new(RefCell::new(HashMap::new()));
    let previous = CARD_TEXT_PAINT_PROBE.with(|active| active.replace(Some(Rc::clone(&probe))));
    (probe, CardTextPaintProbeScope { previous })
}

#[cfg(test)]
fn record_card_text_paint(
    selector: &str,
    painted_text: String,
    bounds: Bounds<Pixels>,
    line_height: Pixels,
) {
    CARD_TEXT_PAINT_PROBE.with(|active| {
        let Some(probe) = active.borrow().as_ref().cloned() else {
            return;
        };
        let painted_line_count = if painted_text.is_empty() {
            0
        } else {
            painted_text.lines().count()
        };
        probe.borrow_mut().insert(
            selector.to_owned(),
            PaintedCardText {
                painted_text,
                painted_line_count,
                bounds,
                line_height,
            },
        );
    });
}

fn card_text_selector(part: &str, note_id: &str) -> String {
    format!("library-note-card-{part}-{note_id}")
}

pub(crate) fn snippet_text_selector(part: &str, note_id: &str) -> String {
    format!("library-note-snippet-{part}-{note_id}")
}

fn relative_time(updated: i64) -> String {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|value| value.as_millis() as i64)
        .unwrap_or(updated);
    let minutes = ((now - updated).max(0) / 60_000) as u64;
    if minutes < 1 {
        "刚刚".into()
    } else if minutes < 60 {
        format!("{minutes} 分钟前")
    } else if minutes < 1_440 {
        format!("{} 小时前", minutes / 60)
    } else {
        format!("{} 天前", minutes / 1_440)
    }
}

fn stable_element_id(value: &str) -> u64 {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    value.hash(&mut hasher);
    hasher.finish()
}

#[allow(dead_code)]
fn _note_id_is_stable(id: &NoteId) -> &str {
    id.as_str()
}
