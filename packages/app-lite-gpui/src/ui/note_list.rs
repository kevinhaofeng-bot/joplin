use crate::app::{AppAction, AppModel};
use crate::ui::note_card;
use app_lite_core::{NoteId, NoteProjection};
use gpui::{
    Entity, InteractiveElement, IntoElement, MouseButton, ParentElement,
    StatefulInteractiveElement, Styled, div, px, rgba,
};

/// Windowed card rendering deliberately constructs only the first viewport.
/// Scrolling/paging can advance this window without changing the projection contract.
pub fn render(
    items: &[NoteProjection],
    selected: Option<&NoteId>,
    model: Entity<AppModel>,
    visible: bool,
) -> impl IntoElement {
    let width = if visible { 360.0 } else { 0.0 };
    let mut list = div()
        .id("note-list")
        .w(px(width))
        .h_full()
        .flex_none()
        .overflow_y_scroll()
        .p(px(12.0))
        .bg(rgba(0xffffffff))
        .border_r_1()
        .border_color(rgba(0xe1e5e1ff));
    for projection in items.iter().take(100) {
        let id = projection.id.clone();
        let action_model = model.clone();
        list = list.child(
            div()
                .on_mouse_down(MouseButton::Left, move |_event, _window, cx| {
                    let _ = action_model.update(cx, |model, _| {
                        model.dispatch(AppAction::SelectNote(id.clone()))
                    });
                })
                .child(note_card::render(
                    projection,
                    selected == Some(&projection.id),
                )),
        );
    }
    list
}
