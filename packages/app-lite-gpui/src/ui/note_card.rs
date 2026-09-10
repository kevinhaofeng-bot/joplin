use app_lite_core::{NoteId, NoteProjection};
use gpui::{InteractiveElement, IntoElement, ParentElement, Styled, div, px, rgba};
use std::hash::{Hash, Hasher};

pub fn render(projection: &NoteProjection, selected: bool) -> impl IntoElement {
    let title = if projection.title_prefix.trim().is_empty() {
        "无标题笔记"
    } else {
        &projection.title_prefix
    };
    let time = relative_time(projection.updated_time);
    div()
        .id(("note-card", stable_element_id(projection.id.as_str())))
        .w_full()
        .min_h(px(88.0))
        .p(px(12.0))
        .mb(px(6.0))
        .rounded(px(8.0))
        .bg(if selected {
            rgba(0x00a82d19)
        } else {
            rgba(0x00000000)
        })
        .hover(|card| card.bg(rgba(0x00a82d10)))
        .text_color(rgba(0x202720ff))
        .child(
            div()
                .text_size(px(15.0))
                .font_weight(gpui::FontWeight::SEMIBOLD)
                .child(title.to_owned()),
        )
        .child(
            div()
                .mt(px(5.0))
                .text_size(px(12.0))
                .text_color(rgba(0x647064ff))
                .child(projection.snippet.clone()),
        )
        .child(
            div()
                .mt(px(8.0))
                .text_size(px(11.0))
                .text_color(rgba(0x8b958cff))
                .child(if projection.selected_thumbnail_id.is_some() {
                    format!("{time} · 有缩略图")
                } else {
                    time
                }),
        )
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
