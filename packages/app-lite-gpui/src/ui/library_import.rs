//! Import an Evernote (.enex) or Joplin (.jex) file into a new library.
//!
//! The source is only read and the active library is never written: the
//! core stages, verifies and publishes a separate profile under
//! `imported-libraries/`. Switching to it is a separate, explicit step that
//! first flushes the open note.

use super::*;
use crate::app::{CancelLibraryImport, ImportLibrary, OpenImportedLibrary};
use crate::library_profile::{LibraryProfiles, record_active};
use app_lite_core::{ImportLibraryError, ImportLibraryOutcome, PublishError, import_library_file};
use std::sync::atomic::{AtomicBool, Ordering};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ImportPhase {
    Picking,
    Working,
}

pub(super) struct PendingLibraryImport {
    token: u64,
    phase: ImportPhase,
    cancel: Arc<AtomicBool>,
}

impl LibraryShell {
    pub(super) fn import_library(
        &mut self,
        _: &ImportLibrary,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.pending_library_import.is_some() {
            return;
        }
        if cx.try_global::<LibraryProfiles>().is_none() {
            self.library_import_notice = Some(ExportNotice::Error(
                "无法确定资料库位置，导入未开始。".into(),
            ));
            cx.notify();
            return;
        }
        self.next_library_import_token = self.next_library_import_token.wrapping_add(1);
        let token = self.next_library_import_token;
        self.pending_library_import = Some(PendingLibraryImport {
            token,
            phase: ImportPhase::Picking,
            cancel: Arc::new(AtomicBool::new(false)),
        });
        self.imported_library_ready = None;
        self.library_import_notice = Some(ExportNotice::Status(
            "请选择 .enex（Evernote）或 .jex（Joplin）文件。原文件只读；内容导入为新资料库，当前资料库不变。"
                .into(),
        ));
        cx.notify();
        #[cfg(not(test))]
        if let Some(handle) = window.window_handle().downcast::<LibraryShell>() {
            cx.defer(move |app| prompt_for_import_source(handle, token, app));
        }
        #[cfg(test)]
        let _ = window;
    }

    pub(super) fn complete_library_import_picker(
        &mut self,
        token: u64,
        selection: Result<Option<PathBuf>, String>,
        cx: &mut Context<Self>,
    ) {
        let Some(pending) = self.pending_library_import.as_mut() else {
            return;
        };
        if pending.token != token || pending.phase != ImportPhase::Picking {
            return;
        }
        let source = match selection {
            Ok(Some(source)) => source,
            Ok(None) => {
                self.pending_library_import = None;
                self.library_import_notice = None;
                cx.notify();
                return;
            }
            Err(error) => {
                self.pending_library_import = None;
                self.library_import_notice =
                    Some(ExportNotice::Error(format!("无法选择导入文件：{error}")));
                cx.notify();
                return;
            }
        };
        let Some(imports_dir) = cx
            .try_global::<LibraryProfiles>()
            .map(LibraryProfiles::imports_dir)
        else {
            self.pending_library_import = None;
            return;
        };
        pending.phase = ImportPhase::Working;
        let cancel = pending.cancel.clone();
        let name = source
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_default();
        self.library_import_notice = Some(ExportNotice::Status(format!(
            "正在后台导入 {name}：校验、暂存并生成新资料库…"
        )));
        cx.notify();
        cx.spawn(async move |this, cx| {
            let result = cx
                .background_executor()
                .spawn(async move { import_library_file(&source, &imports_dir, &cancel) })
                .await;
            let _ = this.update(cx, |shell, shell_cx| {
                shell.finish_library_import(token, result, shell_cx);
            });
        })
        .detach();
    }

    pub(super) fn cancel_library_import(
        &mut self,
        _: &CancelLibraryImport,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(pending) = self.pending_library_import.as_ref() else {
            return;
        };
        pending.cancel.store(true, Ordering::Relaxed);
        if pending.phase == ImportPhase::Picking {
            self.pending_library_import = None;
            self.library_import_notice = None;
        } else {
            self.library_import_notice = Some(ExportNotice::Status("正在取消导入…".into()));
        }
        cx.notify();
    }

    fn finish_library_import(
        &mut self,
        token: u64,
        result: Result<ImportLibraryOutcome, ImportLibraryError>,
        cx: &mut Context<Self>,
    ) {
        if self
            .pending_library_import
            .as_ref()
            .is_none_or(|pending| pending.token != token)
        {
            return;
        }
        self.pending_library_import = None;
        self.library_import_notice = Some(match result {
            Ok(outcome) => {
                let counts = outcome.library.counts;
                let degraded = if outcome.degraded.is_empty() {
                    String::new()
                } else {
                    format!(
                        "其中 {} 篇含暂不支持的格式，已按纯文本导入（原始内容保留在导入记录中）：{}。",
                        outcome.degraded.len(),
                        outcome.degraded.join("；")
                    )
                };
                let message = format!(
                    "导入完成：{} 篇笔记（另有 {} 篇在废纸篓）、{} 个笔记本、{} 个标签、{} 个附件，已保存为新资料库 {}。当前资料库未改动。{degraded}",
                    counts.notes,
                    counts.trashed_notes,
                    counts.notebooks,
                    counts.tags,
                    counts.resources,
                    outcome.library.path.display()
                );
                self.imported_library_ready = Some(outcome.library.path);
                ExportNotice::Status(message)
            }
            Err(error) if is_cancelled(&error) => {
                ExportNotice::Status("导入已取消；未创建新资料库，当前资料库未改动。".into())
            }
            Err(error) => ExportNotice::Error(format!(
                "导入未完成：{error}。未创建新资料库，当前资料库未改动。"
            )),
        });
        cx.notify();
    }

    pub(super) fn open_imported_library(
        &mut self,
        _: &OpenImportedLibrary,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(path) = self.imported_library_ready.clone() else {
            return;
        };
        let Some(base) = cx
            .try_global::<LibraryProfiles>()
            .map(|profiles| profiles.base.clone())
        else {
            return;
        };
        if !self.flush_active_session(FlushReason::WindowClose, cx) {
            self.library_import_notice = Some(ExportNotice::Error(
                "当前笔记尚未保存完成，暂不能切换资料库；请稍后再试。".into(),
            ));
            cx.notify();
            return;
        }
        if let Err(error) = record_active(&base, &path) {
            self.library_import_notice = Some(ExportNotice::Error(format!(
                "无法记录要打开的资料库：{error}"
            )));
            cx.notify();
            return;
        }
        let previous = cx.global::<LibraryProfiles>().active.clone();
        cx.global_mut::<LibraryProfiles>().active = path.clone();
        match open_library_window(cx, path) {
            Ok(_) => window.remove_window(),
            Err(error) => {
                cx.global_mut::<LibraryProfiles>().active = previous.clone();
                let _ = record_active(&base, &previous);
                self.library_import_notice = Some(ExportNotice::Error(format!(
                    "无法打开导入的资料库：{error}"
                )));
                cx.notify();
            }
        }
    }

    pub(super) fn library_import_working(&self) -> bool {
        self.pending_library_import
            .as_ref()
            .is_some_and(|pending| pending.phase == ImportPhase::Working)
    }

    #[cfg(test)]
    pub(crate) fn complete_library_import_picker_for_test(
        &mut self,
        selection: Result<Option<PathBuf>, String>,
        cx: &mut Context<Self>,
    ) {
        let token = self.next_library_import_token;
        self.complete_library_import_picker(token, selection, cx);
    }

    #[cfg(test)]
    pub(crate) fn library_import_notice_for_test(&self) -> Option<String> {
        self.library_import_notice
            .as_ref()
            .map(|notice| notice.message().to_owned())
    }

    #[cfg(test)]
    pub(crate) fn imported_library_ready_for_test(&self) -> Option<PathBuf> {
        self.imported_library_ready.clone()
    }
}

fn is_cancelled(error: &ImportLibraryError) -> bool {
    matches!(
        error,
        ImportLibraryError::Publish(PublishError::Cancelled)
            | ImportLibraryError::Enex(app_lite_core::EnexStageError::Cancelled)
    )
}

#[cfg(not(test))]
fn prompt_for_import_source(window: WindowHandle<LibraryShell>, token: u64, cx: &mut App) {
    let prompt = cx.prompt_for_paths(gpui::PathPromptOptions {
        files: true,
        directories: false,
        multiple: false,
        prompt: Some("导入".into()),
    });
    cx.spawn(async move |cx| {
        let selection = match prompt.await {
            Ok(Ok(paths)) => Ok(paths.and_then(|paths| paths.into_iter().next())),
            Ok(Err(error)) => Err(error.to_string()),
            Err(error) => Err(error.to_string()),
        };
        let _ = cx.update(move |app| {
            let _ = window.update(app, |shell, _window, shell_cx| {
                shell.complete_library_import_picker(token, selection, shell_cx);
            });
        });
    })
    .detach();
}
