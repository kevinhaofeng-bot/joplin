use super::*;

// Transparent test paint observer around the same production StyledText.
fn recent_text(part: &str, index: usize, text: String) -> gpui::AnyElement {
    // Same display-only whitespace policy as existing search_result_text:
    // GPUI's soft line clamp alone does not cap explicit paragraph breaks.
    // Do not normalize the persisted title/body or the repository projection.
    let text = text.split_whitespace().collect::<Vec<_>>().join(" ");
    #[cfg(test)]
    { return super::note_card::observed_text_for_test(format!("library-recent-note-{part}-{index}"), text); }
    #[cfg(not(test))]
    { let _ = (part, index); gpui::StyledText::new(text).into_any_element() }
}

impl LibraryShell {
    pub(super) fn reload_recent_notes(&mut self, cx: &mut Context<Self>) {
        match self.model.read(cx).repository().list_recent_notes(16) {
            Ok(notes) => { self.recent_notes = notes; self.recent_note_error = None; }
            Err(error) => { self.recent_notes.clear(); self.recent_note_error = Some(format!("最近笔记不可用：{error}")); }
        }
    }

    pub(super) fn open_recent_note(&mut self, index: usize, window: &mut Window, cx: &mut Context<Self>) {
        let Some(id) = self.recent_notes.get(index).map(|note| note.id.clone()) else { return; };
        if !self.apply_action_with_result(AppAction::OpenRecentNote(id), window, cx) {
            self.search_palette_status = SearchPaletteStatus::Error("暂时无法打开该最近笔记，请先处理保存状态，或重新打开列表".into());
            cx.notify();
            return;
        }
        self.search_palette_open = false;
        self._search_task = None;
        self.search_palette_generation = None;
        self.search_palette_results.clear();
        self.recent_notes.clear();
        self.recent_searches.clear();
        self.recent_note_error = None;
        self.recent_search_error = None;
        self.search_palette_selected = 0;
        self.search_palette_status = SearchPaletteStatus::Idle;
        self.search_palette_return_focus = None;
        self.search_palette_return_organization_panel_open = false;
        // Just like a committed search result, the next keystroke must reach
        // the exact newly mounted session in this same foreground turn.
        self.sync_editor_surface(cx);
        self.focus_active_editor_or_shell(window, cx);
        cx.notify();
    }

    pub(super) fn render_recent_note_rows(&self, cx: &mut Context<Self>) -> Vec<gpui::AnyElement> {
        let mut rows = Vec::new();
        if let Some(error) = &self.recent_note_error {
            rows.push(div().p(px(8.0)).text_size(px(12.0)).text_color(rgba(0x9c382fff)).child(error.clone()).into_any_element());
        }
        if self.recent_notes.is_empty() { return rows; }
        rows.push(div().py(px(10.0)).text_size(px(12.0)).text_color(rgba(0x718075ff)).child("最近笔记").into_any_element());
        for (index, note) in self.recent_notes.iter().enumerate() {
            let selected = self.search_palette_selected == self.search_palette_results.len() + index;
            let title = if note.title_prefix.trim().is_empty() { "无标题笔记".to_owned() } else { note.title_prefix.clone() };
            rows.push(div().id(SharedString::from(format!("library-recent-note-{}", note.id.as_str())))
                .debug_selector(move || format!("library-recent-note-{index}"))
                .flex().flex_col().h(px(58.0)).flex_none().overflow_hidden().p(px(8.0)).rounded(px(5.0)).cursor_pointer()
                .bg(if selected { rgba(0x00a82d20) } else { rgba(0x00000000) })
                .hover(|style| style.bg(rgba(0x00a82d14)))
                .on_mouse_down(MouseButton::Left, cx.listener(move |shell, _, window, cx| {
                    window.prevent_default(); cx.stop_propagation();
                    shell.open_recent_note(index, window, cx);
                }))
                .child(div().min_w_0().h(px(20.0)).flex_none().text_size(px(14.0)).line_height(px(20.0)).line_clamp(1).text_ellipsis().child(recent_text("title", index, title)))
                .child(div().min_w_0().h(px(18.0)).flex_none().text_size(px(12.0)).line_height(px(18.0)).line_clamp(1).text_color(rgba(0x718075ff)).text_ellipsis().child(recent_text("snippet", index, note.snippet.clone())))
                .into_any_element());
        }
        rows
    }
}
