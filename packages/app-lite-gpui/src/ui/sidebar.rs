//! Typed, metadata-only navigation for the LibraryShell's left column.
//!
//! Note cards deliberately do not appear here. The sidebar is constructed
//! from `LibraryNavigationIndex`, while the middle column remains the sole
//! consumer of `NoteProjection` rows owned by `AppModel`.

use super::LibraryShell;
use app_lite_core::{LibraryNavigationIndex, LibraryRoute};
use gpui::{
    App, Context, InteractiveElement, IntoElement, MouseButton, ParentElement, Styled,
    UniformListScrollHandle, Window, div, px, rgba, uniform_list,
};
use std::hash::{Hash, Hasher};

const SIDEBAR_ENTRY_HEIGHT: f32 = 30.0;

#[derive(Clone, Debug, PartialEq, Eq)]
struct RouteRow {
    route: LibraryRoute,
    label: String,
    selector: String,
    indent: u8,
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum SidebarEntry {
    Heading(&'static str),
    Route(RouteRow),
}

/// Builds the source-backed sidebar tree without touching a note projection.
/// Stack ownership remains on `Notebook::stack_id`, rather than being
/// reconstructed from whatever cards happen to be visible in the middle pane.
fn entries(index: &LibraryNavigationIndex) -> Vec<SidebarEntry> {
    let mut result = vec![SidebarEntry::Route(RouteRow {
        route: LibraryRoute::AllNotes,
        label: "全部笔记".into(),
        selector: "library-sidebar-route-all-notes".into(),
        indent: 0,
    })];

    result.push(SidebarEntry::Heading("笔记本"));
    for stack in &index.stacks {
        result.push(SidebarEntry::Route(RouteRow {
            route: LibraryRoute::Stack(stack.id.clone()),
            label: stack.title.clone(),
            selector: format!("library-sidebar-route-stack-{}", stack.id.as_str()),
            indent: 0,
        }));
        for notebook in index
            .notebooks
            .iter()
            .filter(|notebook| notebook.stack_id.as_ref() == Some(&stack.id))
        {
            result.push(SidebarEntry::Route(RouteRow {
                route: LibraryRoute::Notebook(notebook.id.clone()),
                label: notebook.title.clone(),
                selector: format!("library-sidebar-route-notebook-{}", notebook.id.as_str()),
                indent: 1,
            }));
        }
    }
    for notebook in index
        .notebooks
        .iter()
        .filter(|notebook| notebook.stack_id.is_none())
    {
        result.push(SidebarEntry::Route(RouteRow {
            route: LibraryRoute::Notebook(notebook.id.clone()),
            label: notebook.title.clone(),
            selector: format!("library-sidebar-route-notebook-{}", notebook.id.as_str()),
            indent: 0,
        }));
    }

    if !index.tags.is_empty() {
        result.push(SidebarEntry::Heading("标签"));
        for tag in &index.tags {
            result.push(SidebarEntry::Route(RouteRow {
                route: LibraryRoute::tags(vec![tag.id.clone()])
                    .expect("a single durable TagId is a valid route"),
                label: tag.title.clone(),
                selector: format!("library-sidebar-route-tag-{}", tag.id.as_str()),
                indent: 0,
            }));
        }
    }

    result.push(SidebarEntry::Heading(""));
    result.push(SidebarEntry::Route(RouteRow {
        route: LibraryRoute::Trash,
        label: "废纸篓".into(),
        selector: "library-sidebar-route-trash".into(),
        indent: 0,
    }));
    result
}

/// Renders an Evernote-style, source-backed sidebar. The host supplies the
/// only action bridge; this module owns row presentation but never invokes the
/// repository or changes a session directly.
///
/// Each source tree entry deliberately occupies the same 30px row geometry.
/// That lets GPUI's real `uniform_list` provide a bounded dynamic scroll area
/// for large notebook/tag trees, matching the donor nav rather than clipping a
/// fully materialized column at the window edge.
pub fn render<F>(
    width: u16,
    visible: bool,
    index: &LibraryNavigationIndex,
    selected_route: &LibraryRoute,
    scroll_handle: UniformListScrollHandle,
    cx: &mut Context<LibraryShell>,
    on_navigate: F,
) -> gpui::AnyElement
where
    F: Fn(LibraryRoute, &mut Window, &mut App) + Clone + 'static,
{
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
    if !visible {
        return pane.into_any_element();
    }

    let rows = entries(index);
    let selected_for_processor = selected_route.clone();
    let list = uniform_list(
        "library-sidebar-items",
        rows.len(),
        cx.processor(move |shell, range: std::ops::Range<usize>, _window, _cx| {
            #[cfg(test)]
            shell.record_sidebar_uniform_list_range_for_test(range.clone());
            #[cfg(not(test))]
            let _ = shell;
            range
                .filter_map(|index| rows.get(index).cloned())
                .map(|entry| render_entry(entry, &selected_for_processor, on_navigate.clone()))
                .collect::<Vec<_>>()
        }),
    )
    .size_full()
    .track_scroll(scroll_handle);
    pane.border_r_1()
        .border_color(rgba(0xe1e5e1ff))
        .p(px(8.0))
        .flex()
        .flex_col()
        .child(list)
        .into_any_element()
}

fn render_entry<F>(
    entry: SidebarEntry,
    selected_route: &LibraryRoute,
    on_navigate: F,
) -> gpui::AnyElement
where
    F: Fn(LibraryRoute, &mut Window, &mut App) + Clone + 'static,
{
    match entry {
        SidebarEntry::Heading(label) if !label.is_empty() => div()
            .h(px(SIDEBAR_ENTRY_HEIGHT))
            .w_full()
            .px(px(10.0))
            .flex()
            .items_center()
            .text_size(px(11.0))
            .text_color(rgba(0x718075ff))
            .child(label)
            .into_any_element(),
        SidebarEntry::Heading(_) => div()
            .h(px(SIDEBAR_ENTRY_HEIGHT))
            .w_full()
            .into_any_element(),
        SidebarEntry::Route(row) => {
            let selected = row.route == *selected_route;
            let route = row.route.clone();
            let dispatch = on_navigate.clone();
            let selector = row.selector.clone();
            let row_id = stable_row_id(&selector);
            let indentation = 10.0 + f32::from(row.indent) * 16.0;
            div()
                .id(("library-sidebar-route", row_id))
                .debug_selector(move || selector.clone())
                .h(px(SIDEBAR_ENTRY_HEIGHT))
                .w_full()
                .pl(px(indentation))
                .pr(px(8.0))
                .rounded(px(5.0))
                .flex()
                .items_center()
                .overflow_hidden()
                .text_size(px(13.0))
                .cursor_pointer()
                .bg(if selected {
                    rgba(0x00a82d19)
                } else {
                    rgba(0x00000000)
                })
                .hover(|row| row.bg(rgba(0x00a82d10)))
                .on_mouse_down(MouseButton::Left, move |_event, window, cx| {
                    dispatch(route.clone(), window, cx);
                })
                .child(
                    div()
                        .debug_selector(move || {
                            if selected {
                                "library-sidebar-selected-route".to_owned()
                            } else {
                                "library-sidebar-route-label".to_owned()
                            }
                        })
                        .child(row.label),
                )
                .into_any_element()
        }
    }
}

fn stable_row_id(value: &str) -> u64 {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    value.hash(&mut hasher);
    hasher.finish()
}

#[cfg(test)]
pub(crate) fn route_index_for_test(
    index: &LibraryNavigationIndex,
    route: &LibraryRoute,
) -> Option<usize> {
    entries(index).iter().position(|entry| {
        matches!(entry, SidebarEntry::Route(RouteRow { route: candidate, .. }) if candidate == route)
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use app_lite_core::{Notebook, NotebookId, Stack, StackId, Tag, TagId};

    fn id(value: &str) -> String {
        format!("{value:0>32}")
    }

    #[test]
    fn entries_keep_typed_routes_and_stack_ownership() {
        let stack = Stack {
            id: StackId::parse(id("1")).expect("stack id"),
            title: "项目".into(),
            revision: 1,
        };
        let notebook = Notebook {
            id: NotebookId::parse(id("2")).expect("notebook id"),
            title: "客户端".into(),
            stack_id: Some(stack.id.clone()),
            revision: 1,
            is_default: false,
        };
        let tag = Tag {
            id: TagId::parse(id("3")).expect("tag id"),
            title: "紧急".into(),
            revision: 1,
        };
        let entries = entries(&LibraryNavigationIndex {
            notebooks: vec![notebook.clone()],
            stacks: vec![stack.clone()],
            tags: vec![tag.clone()],
        });
        assert!(entries.iter().any(|entry| matches!(
            entry,
            SidebarEntry::Route(RouteRow { route: LibraryRoute::Stack(id), .. }) if id == &stack.id
        )));
        assert!(entries.iter().any(|entry| matches!(
            entry,
            SidebarEntry::Route(RouteRow { route: LibraryRoute::Notebook(id), indent: 1, .. }) if id == &notebook.id
        )));
        assert!(entries.iter().any(|entry| matches!(
            entry,
            SidebarEntry::Route(RouteRow { route: LibraryRoute::Tags(ids), .. }) if ids.contains(&tag.id)
        )));
    }
}
