//! Native readable-export handoff for one captured library note.

use super::*;
use app_lite_core::{ReadableExportError, ReadableExportReport, export_readable_selection};
use std::path::Path;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ExportPhase {
    SavingBeforePicker,
    Picking,
    SavingBeforeJob,
    Working,
}

pub(super) struct PendingReadableExport {
    pub(super) token: u64,
    note_id: NoteId,
    pub(super) window: WindowHandle<LibraryShell>,
    destination: Option<PathBuf>,
    phase: ExportPhase,
}

pub(super) enum ExportNotice {
    Status(String),
    Error(String),
}

impl ExportNotice {
    pub(super) fn message(&self) -> &str {
        match self {
            Self::Status(message) | Self::Error(message) => message,
        }
    }

    pub(super) fn is_error(&self) -> bool {
        matches!(self, Self::Error(_))
    }
}

impl LibraryShell {
    pub(super) fn export_current_note(
        &mut self,
        _: &ExportCurrentNote,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.pending_readable_export.is_some() {
            return;
        }
        let Some(note_id) = self.surface_note_id.clone() else {
            self.readable_export_notice = Some(ExportNotice::Error(
                "请先打开一篇笔记，再导出当前笔记。".into(),
            ));
            cx.notify();
            return;
        };
        let Some(window) = window.window_handle().downcast::<LibraryShell>() else {
            self.readable_export_notice = Some(ExportNotice::Error(
                "无法关联当前资料库窗口，请重新打开窗口后重试。".into(),
            ));
            cx.notify();
            return;
        };
        self.next_readable_export_token = self.next_readable_export_token.wrapping_add(1);
        self.pending_readable_export = Some(PendingReadableExport {
            token: self.next_readable_export_token,
            note_id,
            window,
            destination: None,
            phase: ExportPhase::SavingBeforePicker,
        });
        self.readable_export_notice = Some(ExportNotice::Status(
            "正在保存当前笔记，随后选择导出目录…".into(),
        ));
        self.advance_readable_export_after_session_notification(cx);
    }

    pub(super) fn cancel_readable_export_after_note_change(&mut self, cx: &mut Context<Self>) {
        let Some(pending) = self.pending_readable_export.as_ref() else {
            return;
        };
        if pending.phase != ExportPhase::Working
            && self.surface_note_id.as_ref() != Some(&pending.note_id)
        {
            self.pending_readable_export = None;
            self.readable_export_notice = Some(ExportNotice::Status(
                "笔记已切换；原导出请求已取消。".into(),
            ));
            cx.notify();
        }
    }

    pub(super) fn abort_readable_export_for_resource_failure(
        &mut self,
        error: &str,
        cx: &mut Context<Self>,
    ) {
        if self
            .pending_readable_export
            .as_ref()
            .is_some_and(|pending| {
                matches!(
                    pending.phase,
                    ExportPhase::SavingBeforePicker
                        | ExportPhase::Picking
                        | ExportPhase::SavingBeforeJob
                )
            })
        {
            self.pending_readable_export = None;
            self.readable_export_notice = Some(ExportNotice::Error(format!(
                "导出未开始：待插入资源保存失败：{error}。请先处理该资源后重试。"
            )));
            cx.notify();
        }
    }

    pub(super) fn advance_readable_export_after_session_notification(
        &mut self,
        cx: &mut Context<Self>,
    ) {
        let Some(pending) = self.pending_readable_export.as_ref() else {
            return;
        };
        if !matches!(
            pending.phase,
            ExportPhase::SavingBeforePicker | ExportPhase::SavingBeforeJob
        ) {
            return;
        }
        if self.surface_note_id.as_ref() != Some(&pending.note_id) {
            self.cancel_readable_export_after_note_change(cx);
            return;
        }
        if !self.flush_active_session(FlushReason::Export, cx) {
            if self.save_pending {
                self.readable_export_notice = Some(ExportNotice::Status(
                    "正在等待当前笔记和资源保存完成…".into(),
                ));
            } else {
                let detail = self
                    .save_error
                    .as_ref()
                    .map(|error| error.message())
                    .unwrap_or("保存未完成")
                    .to_owned();
                self.pending_readable_export = None;
                self.readable_export_notice = Some(ExportNotice::Error(format!(
                    "导出未开始：{detail}。请先解决保存错误后重试。"
                )));
            }
            cx.notify();
            return;
        }
        if !self.queued_resource_inserts.is_empty() {
            self.readable_export_notice =
                Some(ExportNotice::Status("正在等待待插入资源保存完成…".into()));
            cx.notify();
            return;
        }
        let pending = self
            .pending_readable_export
            .as_mut()
            .expect("checked above");
        if let Some(destination) = pending.destination.clone() {
            pending.phase = ExportPhase::Working;
            let token = pending.token;
            let note_id = pending.note_id.clone();
            let repository = self.model.read_with(cx, |model, _| model.repository());
            self.readable_export_notice = Some(ExportNotice::Status(format!(
                "正在后台导出 1 篇笔记到 {}…",
                destination.display()
            )));
            cx.notify();
            cx.spawn(async move |this, cx| {
                let job_path = destination.clone();
                let result = cx
                    .background_executor()
                    .spawn(async move {
                        export_readable_selection(repository.as_ref(), &[note_id], &job_path)
                    })
                    .await;
                let _ = this.update(cx, |shell, shell_cx| {
                    shell.finish_readable_export(token, destination, result, shell_cx);
                });
            })
            .detach();
        } else {
            pending.phase = ExportPhase::Picking;
            let token = pending.token;
            let window = pending.window;
            self.readable_export_notice = Some(ExportNotice::Status(
                "请选择一个尚不存在的导出目录；本次仅导出当前默认笔记本中未加标签的笔记。".into(),
            ));
            cx.notify();
            #[cfg(not(test))]
            cx.defer(move |app| prompt_for_readable_export_path(window, token, app));
            #[cfg(test)]
            let _ = (window, token);
        }
    }

    pub(super) fn complete_readable_export_picker(
        &mut self,
        token: u64,
        selection: Result<Option<PathBuf>, String>,
        cx: &mut Context<Self>,
    ) {
        let Some(pending) = self.pending_readable_export.as_mut() else {
            return;
        };
        if pending.token != token || pending.phase != ExportPhase::Picking {
            return;
        }
        if self.surface_note_id.as_ref() != Some(&pending.note_id) {
            self.cancel_readable_export_after_note_change(cx);
            return;
        }
        match selection {
            Ok(None) => {
                self.pending_readable_export = None;
                self.readable_export_notice = None;
                cx.notify();
            }
            Err(error) => {
                self.pending_readable_export = None;
                self.readable_export_notice =
                    Some(ExportNotice::Error(format!("无法选择导出目录：{error}")));
                cx.notify();
            }
            Ok(Some(destination)) => {
                pending.destination = Some(destination);
                pending.phase = ExportPhase::SavingBeforeJob;
                self.readable_export_notice =
                    Some(ExportNotice::Status("正在确认笔记与资源已保存…".into()));
                self.advance_readable_export_after_session_notification(cx);
            }
        }
    }

    fn finish_readable_export(
        &mut self,
        token: u64,
        destination: PathBuf,
        result: Result<ReadableExportReport, ReadableExportError>,
        cx: &mut Context<Self>,
    ) {
        if self
            .pending_readable_export
            .as_ref()
            .is_none_or(|pending| pending.token != token || pending.phase != ExportPhase::Working)
        {
            return;
        }
        self.pending_readable_export = None;
        self.readable_export_notice = Some(match result {
            Ok(report) => ExportNotice::Status(format!(
                "导出完成：{}；{} 篇笔记，{} 个资源。此目录只包含本次选择，不能代替全库备份。",
                destination.display(),
                report.note_count,
                report.resource_count
            )),
            Err(error) => ExportNotice::Error(readable_export_error_message(&error, &destination)),
        });
        cx.notify();
    }
}

fn readable_export_error_message(error: &ReadableExportError, destination: &Path) -> String {
    match error {
        ReadableExportError::TargetExists => format!(
            "导出未完成：目标 {} 已存在。请选择一个新目录；现有内容未被覆盖。",
            destination.display()
        ),
        ReadableExportError::UnsupportedOrganization(_) =>
            "导出未完成：当前仅支持默认笔记本中未删除、未加标签的笔记，且默认笔记本不能属于笔记本组。请保留原笔记并选择受支持的笔记重试。".into(),
        other => format!("导出未完成：{other}。请检查目标目录与本地资源后重试。"),
    }
}

#[cfg(not(test))]
fn prompt_for_readable_export_path(window: WindowHandle<LibraryShell>, token: u64, cx: &mut App) {
    let default_dir = directories::UserDirs::new()
        .and_then(|dirs| dirs.document_dir().map(Path::to_path_buf))
        .unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")));
    // A stable, content-free name avoids leaking a note title to OS dialog
    // history, logs, and accidentally invalid filesystem path components.
    let prompt = crate::file_picker::prompt_for_new_path(cx, &default_dir, Some("Joplin-Lite-当前笔记导出"));
    let prompt = crate::file_picker::bind_to_request(cx, window.into(), prompt, move |app| {
        window.read_with(app, |shell, _| shell.pending_readable_export.as_ref().is_some_and(
            |pending| pending.token == token,
        )).unwrap_or(false)
    });
    cx.spawn(async move |cx| {
        let selection = match prompt.await {
            Ok(Ok(path)) => Ok(path),
            Ok(Err(error)) => Err(error.to_string()),
            Err(error) => Err(error.to_string()),
        };
        let _ = cx.update(move |app| {
            let _ = window.update(app, |shell, _window, shell_cx| {
                shell.complete_readable_export_picker(token, selection, shell_cx);
            });
        });
    })
    .detach();
}
