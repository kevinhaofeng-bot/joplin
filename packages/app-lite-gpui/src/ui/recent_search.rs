use super::*;
use gpui::EntityInputHandler;

impl LibraryShell {
    pub(super) fn reload_recent_searches(&mut self, cx: &mut Context<Self>) {
        let prefix = self.search_input.read(cx).text().trim();
        let limit = if self.recent_searches_expanded { 128 } else { 8 };
        match self.model.read(cx).repository().list_recent_searches(prefix, limit) {
            Ok(entries) => {
                self.recent_searches = entries;
                self.recent_search_error = None;
            }
            Err(error) => {
                self.recent_searches.clear();
                self.recent_search_error = Some(format!("最近搜索不可用：{error}"));
            }
        }
        self.search_palette_selected = self.search_palette_selected.min(
            (self.search_palette_results.len() + self.recent_notes.len() + self.recent_searches.len()).saturating_sub(1),
        );
    }

    fn delete_recent_search(&mut self, index: usize, window: &mut Window, cx: &mut Context<Self>) {
        let Some(entry) = self.recent_searches.get(index) else { return; };
        let result = self.model.read(cx).repository().delete_search_history(&entry.query);
        match result {
            Ok(()) => self.reload_recent_searches(cx),
            Err(error) => self.recent_search_error = Some(format!("删除历史失败：{error}")),
        }
        self.search_input.read(cx).focus_handle().focus(window);
        cx.notify();
    }

    fn clear_recent_searches(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        match self.model.read(cx).repository().clear_search_history() {
            Ok(()) => self.reload_recent_searches(cx),
            Err(error) => self.recent_search_error = Some(format!("清空历史失败：{error}")),
        }
        self.search_input.read(cx).focus_handle().focus(window);
        cx.notify();
    }

    fn reuse_recent_search(&mut self, index: usize, window: &mut Window, cx: &mut Context<Self>) {
        let Some(query) = self.recent_searches.get(index).map(|entry| entry.query.clone()) else { return; };
        self.search_input.update(cx, |input, input_cx| {
            input.select_all();
            input.replace_text_in_range(None, &query, window, input_cx);
            input.focus_handle().focus(window);
        });
        // Re-run even when the stored query equals the current input. Exact
        // generation fences on the existing worker discard superseded packets.
        self.schedule_search_from_input(cx);
        cx.notify();
    }

    pub(super) fn activate_search_palette_selection(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let index = self.search_palette_selected;
        if index < self.search_palette_results.len() {
            self.commit_search_result(index, window, cx);
        } else if index < self.search_palette_results.len() + self.recent_notes.len() {
            self.open_recent_note(index - self.search_palette_results.len(), window, cx);
        } else {
            self.reuse_recent_search(index - self.search_palette_results.len() - self.recent_notes.len(), window, cx);
        }
    }

    pub(super) fn render_recent_search_rows(&self, cx: &mut Context<Self>) -> Vec<gpui::AnyElement> {
        let mut rows = Vec::new();
        if let Some(message) = self.recent_search_error.as_ref() {
            rows.push(div().p(px(8.0)).text_size(px(12.0)).text_color(rgba(0x9c382fff))
                .child(message.clone()).into_any_element());
        }
        if self.recent_searches.is_empty() { return rows; }
        rows.push(div().flex().items_center().justify_between().py(px(10.0))
            .text_size(px(12.0)).text_color(rgba(0x718075ff)).child("最近搜索")
            .child(div().flex().gap(px(14.0))
                .child(div().id("library-recent-search-expand").cursor_pointer()
                    .child(if self.recent_searches_expanded { "收起" } else { "展开" })
                    .on_mouse_down(MouseButton::Left, cx.listener(|shell, _, window, cx| {
                        window.prevent_default(); cx.stop_propagation();
                        shell.recent_searches_expanded = !shell.recent_searches_expanded;
                        shell.reload_recent_searches(cx); cx.notify();
                    })))
                .child(div().id("library-recent-search-clear")
                    .debug_selector(|| "library-recent-search-clear".to_owned()).cursor_pointer()
                    .child("清空")
                    .on_mouse_down(MouseButton::Left, cx.listener(|shell, _, window, cx| {
                        window.prevent_default(); cx.stop_propagation();
                        shell.clear_recent_searches(window, cx);
                    }))))
            .into_any_element());
        for (index, entry) in self.recent_searches.iter().enumerate() {
            let selected = self.search_palette_selected == self.search_palette_results.len() + self.recent_notes.len() + index;
            rows.push(div().id(SharedString::from(format!("library-recent-search-{index}")))
                .debug_selector(move || format!("library-recent-search-{index}"))
                .flex().items_center().justify_between().p(px(8.0)).rounded(px(5.0))
                .cursor_pointer().bg(if selected { rgba(0x00a82d20) } else { rgba(0x00000000) })
                .hover(|style| style.bg(rgba(0x00a82d14)))
                .on_mouse_down(MouseButton::Left, cx.listener(move |shell, _, window, cx| {
                    window.prevent_default(); cx.stop_propagation();
                    shell.reuse_recent_search(index, window, cx);
                }))
                .child(div().flex_1().min_w_0().text_size(px(14.0)).text_ellipsis().child(entry.query.clone()))
                .child(div().id(SharedString::from(format!("library-recent-search-delete-{index}")))
                    .debug_selector(move || format!("library-recent-search-delete-{index}"))
                    .px(px(8.0)).text_size(px(12.0)).text_color(rgba(0x718075ff)).child("删除")
                    .on_mouse_down(MouseButton::Left, cx.listener(move |shell, _, window, cx| {
                        window.prevent_default(); cx.stop_propagation();
                        shell.delete_recent_search(index, window, cx);
                    })))
                .into_any_element());
        }
        rows
    }
}
