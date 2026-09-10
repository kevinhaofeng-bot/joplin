use gpui::{InteractiveElement, IntoElement, ParentElement, Styled, div, px, rgba};

pub fn render(width: u16, visible: bool) -> impl IntoElement {
    let width = if visible { f32::from(width) } else { 0.0 };
    let mut pane = div()
        .w(px(width))
        .min_w(px(0.0))
        .max_w(px(width))
        .debug_selector(|| "library-sidebar".to_owned())
        .h_full()
        .flex_none()
        .overflow_hidden()
        .bg(rgba(0xf7f8f7ff))
        .text_color(rgba(0x36413aff));
    if visible {
        pane = pane
            .border_r_1()
            .border_color(rgba(0xe1e5e1ff))
            .p(px(20.0))
            .child("笔记")
            .child(
                div()
                    .mt(px(20.0))
                    .text_size(px(13.0))
                    .text_color(rgba(0x718075ff))
                    .child("全部笔记"),
            );
    }
    pane
}
