use gpui::{IntoElement, ParentElement, Styled, div, px, rgba};

pub fn render(visible: bool) -> impl IntoElement {
    let width = if visible { 220.0 } else { 0.0 };
    div()
        .w(px(width))
        .h_full()
        .flex_none()
        .p(px(20.0))
        .bg(rgba(0xf7f8f7ff))
        .border_r_1()
        .border_color(rgba(0xe1e5e1ff))
        .text_color(rgba(0x36413aff))
        .child("笔记")
        .child(
            div()
                .mt(px(20.0))
                .text_size(px(13.0))
                .text_color(rgba(0x718075ff))
                .child("全部笔记"),
        )
}
