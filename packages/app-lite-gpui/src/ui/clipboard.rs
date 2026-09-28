//! Copy and cut from the note editor with full structure, and the system
//! clipboard write behind them. Evernote's copy response carries `html`,
//! `plain` and `resources`, and a selection of resources only also puts
//! the resource files on the clipboard (`clipboard/commands/copy.ts`
//! `resourceCopy`; boron `clipboard-utils` `setNativeFilesForCopy` →
//! `writeFilePaths`). Here the plain text, the HTML, this app's fragment and,
//! for resources only, the exported files go on the macOS pasteboard.

use super::*;
use crate::app::note_session::ClipboardExport;
#[cfg(all(target_os = "macos", not(test)))]
use crate::native_editor::images::FRAGMENT_PASTEBOARD_TYPE;

impl LibraryShell {
    pub(super) fn copy_from_editor(&mut self, cut: bool, cx: &mut Context<Self>) {
        let Some(session) = self.note_session.clone() else {
            return;
        };
        let export = match session.read_with(cx, |session, app| session.copy_selection(app)) {
            Ok(Some(export)) => export,
            Ok(None) => return,
            Err(error) => {
                self.resource_notice = Some(format!("复制未完成：{error}"));
                cx.notify();
                return;
            }
        };
        write_clipboard_export(&export, cx);
        if !cut {
            return;
        }
        if self.active_session_is_read_only(cx) {
            self.resource_notice = Some(self.resource_mutation_block_message("剪切", cx));
            cx.notify();
            return;
        }
        let editor = session.read_with(cx, |session, _| session.editor().clone());
        let deleted = editor.update(cx, |editor, editor_cx| {
            let result = editor.delete_selection();
            editor_cx.notify();
            result
        });
        if let Err(error) = deleted {
            self.resource_notice = Some(format!("剪切未完成：{error}"));
            cx.notify();
        }
    }
}

impl LibraryShell {
    /// Formatted content from another app lands at the caret at once; its
    /// images load behind it and a failure is reported when it happens.
    pub(super) fn paste_external_html(
        &mut self,
        html: &str,
        text: Option<&str>,
        session: Entity<NoteSession>,
        cx: &mut Context<Self>,
    ) -> Result<(), String> {
        let Ok(pasted) = app_lite_core::CanonicalDocument::parse_pasted_html(html) else {
            let editor = session.read_with(cx, |session, _| session.editor().clone());
            return editor
                .update(cx, |editor, editor_cx| {
                    let result = editor.paste_plain_text(text.unwrap_or_default());
                    editor_cx.notify();
                    result
                })
                .map_err(|error| error.to_string());
        };
        let outcome = session
            .update(cx, |session, session_cx| {
                session.paste_external_html(pasted, session_cx)
            })
            .map_err(|error| error.to_string())?;
        let jobs = session.update(cx, |session, _| session.take_new_pasted_image_jobs());
        self.fetch_pasted_images(jobs, cx);
        if outcome.unavailable > 0 {
            self.resource_notice = Some(format!(
                "{} 张图片未能获取，已保留为说明文字",
                outcome.unavailable
            ));
            cx.notify();
        }
        Ok(())
    }
}

#[cfg(all(target_os = "macos", not(test)))]
fn write_clipboard_export(export: &ClipboardExport, _cx: &mut App) {
    use cocoa::appkit::{NSFilenamesPboardType, NSPasteboard, NSPasteboardTypeString};
    use cocoa::base::{id, nil};
    use cocoa::foundation::{NSArray, NSAutoreleasePool, NSString};
    let fragment = serde_json::to_string(&export.fragment).expect("fragment serializes");
    unsafe {
        let _pool = NSAutoreleasePool::new(nil);
        let ns = |text: &str| -> id { NSString::alloc(nil).init_str(text).autorelease() };
        let pasteboard = NSPasteboard::generalPasteboard(nil);
        let html_type = ns("public.html");
        let fragment_type = ns(FRAGMENT_PASTEBOARD_TYPE);
        let offer_files = export.resource_only && !export.files.is_empty();
        let mut types = vec![NSPasteboardTypeString, html_type, fragment_type];
        if offer_files {
            types.push(NSFilenamesPboardType);
        }
        pasteboard.clearContents();
        pasteboard.declareTypes_owner(NSArray::arrayWithObjects(nil, &types), nil);
        pasteboard.setString_forType(ns(&export.plain), NSPasteboardTypeString);
        pasteboard.setString_forType(ns(&export.html), html_type);
        pasteboard.setString_forType(ns(&fragment), fragment_type);
        if offer_files {
            let paths: Vec<id> = export
                .files
                .iter()
                .map(|path| ns(&path.to_string_lossy()))
                .collect();
            pasteboard.setPropertyList_forType(
                NSArray::arrayWithObjects(nil, &paths),
                NSFilenamesPboardType,
            );
        }
    }
}

/// Tests and other platforms: GPUI's clipboard, with the fragment as its
/// JSON metadata; tests also see what the pasteboard would have held.
#[cfg(any(test, not(target_os = "macos")))]
fn write_clipboard_export(export: &ClipboardExport, cx: &mut App) {
    cx.write_to_clipboard(ClipboardItem::new_string_with_json_metadata(
        export.plain.clone(),
        export.fragment.clone(),
    ));
    #[cfg(test)]
    LAST_EXPORT.with(|last| {
        *last.borrow_mut() = Some(RecordedExport {
            plain: export.plain.clone(),
            html: export.html.clone(),
            files: export.files.clone(),
            resource_only: export.resource_only,
        })
    });
}

#[cfg(test)]
#[derive(Clone, Debug)]
pub(crate) struct RecordedExport {
    pub plain: String,
    pub html: String,
    pub files: Vec<std::path::PathBuf>,
    pub resource_only: bool,
}

#[cfg(test)]
thread_local! {
    static LAST_EXPORT: std::cell::RefCell<Option<RecordedExport>> =
        const { std::cell::RefCell::new(None) };
}

#[cfg(test)]
pub(crate) fn last_clipboard_export_for_test() -> Option<RecordedExport> {
    LAST_EXPORT.with(|last| last.borrow().clone())
}
