//! Metadata-only shortcut controls and one retained-session save continuation.
//! The reducer remains the only mutation/navigation path.
use super::LibraryShell;
use crate::app::AppAction;
use crate::app::save_coordinator::SaveState;
use app_lite_core::{LibraryNavigationIndex, LibraryRoute, NoteId, ShortcutTarget};
use gpui::{
    App, Context, InteractiveElement, IntoElement, MouseButton, ParentElement, Styled, Window,
    WindowHandle, div, px, rgba,
};

pub(super) struct PendingShortcutAction {
    action: AppAction,
    source_session: gpui::EntityId,
    route: LibraryRoute,
    window: WindowHandle<LibraryShell>,
}

impl LibraryShell {
    pub(super) fn apply_shortcut_action(
        &mut self,
        action: AppAction,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        debug_assert!(matches!(
            &action,
            AppAction::AddShortcuts(_) | AppAction::RemoveShortcuts(_) | AppAction::OpenShortcut(_)
        ));
        let opens_note = matches!(&action, AppAction::OpenShortcut(_));
        if self.apply_action_with_result(action.clone(), window, cx) {
            if opens_note {
                self.sync_editor_surface(cx);
                self.focus_active_editor_or_shell(window, cx);
            }
            return;
        }
        if !self.save_pending {
            return;
        }
        let Some(session) = self.note_session.as_ref() else {
            return;
        };
        let source = session.read(cx);
        if source.title().read(cx).marked_range().is_some()
            || source.editor().read(cx).marked_text().is_some()
            || matches!(source.save_state(), SaveState::Failed(_))
            || self.model.read(cx).reconciliation_pending()
        {
            return;
        }
        let Some(window) = window.window_handle().downcast::<LibraryShell>() else {
            return;
        };
        self.pending_shortcut_action = Some(PendingShortcutAction {
            action,
            source_session: session.entity_id(),
            route: self.model.read(cx).navigation().route().clone(),
            window,
        });
        self.schedule_pending_shortcut_action(cx);
    }

    fn shortcut_source_is_current(&self, pending: &PendingShortcutAction, cx: &App) -> bool {
        let Some(session) = self.note_session.as_ref() else {
            return false;
        };
        let source = session.read(cx);
        session.entity_id() == pending.source_session
            && self.model.read(cx).navigation().route() == &pending.route
            && !self.model.read(cx).reconciliation_pending()
            && !matches!(source.save_state(), SaveState::Failed(_))
            && source.title().read(cx).marked_range().is_none()
            && source.editor().read(cx).marked_text().is_none()
    }

    pub(super) fn schedule_pending_shortcut_action(&mut self, cx: &mut Context<Self>) {
        if self.shortcut_action_completion_scheduled {
            return;
        }
        let Some(pending) = self.pending_shortcut_action.as_ref() else {
            return;
        };
        if !self.shortcut_source_is_current(pending, cx) {
            self.pending_shortcut_action = None;
            return;
        }
        let session = self
            .note_session
            .as_ref()
            .expect("source checked above")
            .read(cx);
        if self.save_pending
            || session.flush_confirmation_pending()
            || !matches!(session.save_state(), SaveState::Clean)
        {
            return;
        }
        let window = pending.window.clone();
        self.shortcut_action_completion_scheduled = true;
        cx.defer(move |app| {
            let _ = window.update(app, |shell, window, shell_cx| {
                shell.shortcut_action_completion_scheduled = false;
                let Some(pending) = shell.pending_shortcut_action.take() else {
                    return;
                };
                if !shell.shortcut_source_is_current(&pending, shell_cx) {
                    return;
                }
                // Fresh target membership/lifecycle is checked by AppModel,
                // after the original snapshot has actually been confirmed.
                shell.apply_shortcut_action(pending.action, window, shell_cx);
            });
        });
    }
}

pub(super) fn controls(
    route: &LibraryRoute,
    index: &LibraryNavigationIndex,
    notes: Vec<NoteId>,
    cx: &mut Context<LibraryShell>,
) -> gpui::AnyElement {
    let mut row = div().flex().flex_wrap().items_center().gap(px(6.0));
    if *route != LibraryRoute::Trash && !notes.is_empty() {
        let count = notes.len();
        let noun = if count == 1 {
            "当前笔记".to_owned()
        } else {
            format!("{count} 篇笔记")
        };
        row = row.child(toggle_button(
            "library-organization-shortcut-notes",
            noun,
            notes.into_iter().map(ShortcutTarget::Note).collect(),
            index,
            cx,
        ));
    }
    let route_target = match route {
        LibraryRoute::Notebook(id) => {
            Some(("当前笔记本", vec![ShortcutTarget::Notebook(id.clone())]))
        }
        LibraryRoute::Stack(id) => Some(("当前笔记本组", vec![ShortcutTarget::Stack(id.clone())])),
        LibraryRoute::Tags(ids) => Some((
            "当前标签",
            ids.iter().cloned().map(ShortcutTarget::Tag).collect(),
        )),
        _ => None,
    };
    if let Some((noun, targets)) = route_target {
        row = row.child(toggle_button(
            "library-organization-shortcut-route",
            noun.to_owned(),
            targets,
            index,
            cx,
        ));
    }
    row.into_any_element()
}

fn toggle_button(
    selector: &'static str,
    noun: String,
    targets: Vec<ShortcutTarget>,
    index: &LibraryNavigationIndex,
    cx: &mut Context<LibraryShell>,
) -> gpui::AnyElement {
    let remove = targets
        .iter()
        .all(|target| index.shortcuts.iter().any(|entry| &entry.target == target));
    let label = if remove {
        format!("移除{noun}快捷入口")
    } else {
        format!("添加{noun}快捷入口")
    };
    let action = if remove {
        AppAction::RemoveShortcuts(targets)
    } else {
        AppAction::AddShortcuts(targets)
    };
    div()
        .id(selector)
        .debug_selector(move || selector.to_owned())
        .px(px(7.0))
        .py(px(4.0))
        .rounded(px(4.0))
        .text_size(px(11.0))
        .bg(rgba(0x00a82d14))
        .text_color(rgba(0x28723eff))
        .cursor_pointer()
        .on_mouse_down(
            MouseButton::Left,
            cx.listener(move |shell, _event, window, cx| {
                window.prevent_default();
                shell.apply_shortcut_action(action.clone(), window, cx);
            }),
        )
        .child(label)
        .into_any_element()
}
