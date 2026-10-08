//! Whole-library backup and restore-to-a-new-library.
//!
//! Backup flushes the open note, then snapshots the active library in the
//! background. Restore never replaces a library: it verifies the backup and
//! publishes it under `imported-libraries/`, reusing the "打开导入的资料库"
//! switch from the import flow.
//!
//! The readable export is a second, human-browsable format (HTML pages plus
//! original resource bytes); its restore follows the same new-library rule.

use super::library_import::{ImportPhase, LibraryJob, PendingLibraryImport};
use super::*;
use crate::app::{BackupLibrary, ExportLibraryReadable, RestoreLibrary, RestoreLibraryReadable};
use crate::library_profile::LibraryProfiles;
use app_lite_core::{
    BackupError, BackupReport, LibraryReadableReport, PublishedLibrary, ReadableExportError,
    backup_library, export_library_readable, restore_library_backup, restore_library_readable,
    unique_library_destination,
};
use std::sync::atomic::{AtomicBool, Ordering};

impl LibraryShell {
    fn begin_library_job(&mut self, job: LibraryJob, cx: &mut Context<Self>) -> Option<u64> {
        if self.pending_library_import.is_some() {
            return None;
        }
        if cx.try_global::<LibraryProfiles>().is_none() {
            self.library_import_notice = Some(ExportNotice::Error(
                "无法确定资料库位置，操作未开始。".into(),
            ));
            cx.notify();
            return None;
        }
        self.next_library_import_token = self.next_library_import_token.wrapping_add(1);
        let token = self.next_library_import_token;
        self.pending_library_import = Some(PendingLibraryImport::new(token, job));
        self.imported_library_ready = None;
        Some(token)
    }

    pub(super) fn backup_library_action(
        &mut self,
        _: &BackupLibrary,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.pending_library_import.is_some() {
            return;
        }
        if !self.flush_active_session(FlushReason::Export, cx) {
            self.library_import_notice = Some(ExportNotice::Error(
                "当前笔记尚未保存完成，暂不能备份；请稍后再试。".into(),
            ));
            cx.notify();
            return;
        }
        let Some(token) = self.begin_library_job(LibraryJob::Backup, cx) else {
            return;
        };
        self.library_import_notice = Some(ExportNotice::Status(
            "请选择一个尚不存在的备份目录。备份包含全部笔记（含回收站）、笔记本、标签、历史版本与全部附件。"
                .into(),
        ));
        cx.notify();
        #[cfg(not(test))]
        if let Some(handle) = window.window_handle().downcast::<LibraryShell>() {
            cx.defer(move |app| {
                prompt_for_new_directory(
                    handle,
                    token,
                    "Joplin-Lite-资料库备份",
                    LibraryShell::complete_library_backup_picker,
                    app,
                )
            });
        }
        #[cfg(test)]
        let _ = (window, token);
    }

    pub(super) fn restore_library_action(
        &mut self,
        _: &RestoreLibrary,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(token) = self.begin_library_job(LibraryJob::Restore, cx) else {
            return;
        };
        self.library_import_notice = Some(ExportNotice::Status(
            "请选择一个资料库备份目录。它将恢复为新的资料库，当前资料库不变。".into(),
        ));
        cx.notify();
        #[cfg(not(test))]
        if let Some(handle) = window.window_handle().downcast::<LibraryShell>() {
            cx.defer(move |app| {
                prompt_for_existing_directory(
                    handle,
                    token,
                    LibraryShell::complete_library_restore_picker,
                    app,
                )
            });
        }
        #[cfg(test)]
        let _ = (window, token);
    }

    pub(super) fn export_library_readable_action(
        &mut self,
        _: &ExportLibraryReadable,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.pending_library_import.is_some() {
            return;
        }
        if !self.flush_active_session(FlushReason::Export, cx) {
            self.library_import_notice = Some(ExportNotice::Error(
                "当前笔记尚未保存完成，暂不能导出；请稍后再试。".into(),
            ));
            cx.notify();
            return;
        }
        let Some(token) = self.begin_library_job(LibraryJob::ReadableExport, cx) else {
            return;
        };
        self.library_import_notice = Some(ExportNotice::Status(
            "请选择一个尚不存在的导出目录。导出为可直接用浏览器阅读的 HTML，含全部笔记（含回收站）、历史版本与附件原文件。"
                .into(),
        ));
        cx.notify();
        #[cfg(not(test))]
        if let Some(handle) = window.window_handle().downcast::<LibraryShell>() {
            cx.defer(move |app| {
                prompt_for_new_directory(
                    handle,
                    token,
                    "Joplin-Lite-可读导出",
                    LibraryShell::complete_library_readable_export_picker,
                    app,
                )
            });
        }
        #[cfg(test)]
        let _ = (window, token);
    }

    pub(super) fn restore_library_readable_action(
        &mut self,
        _: &RestoreLibraryReadable,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(token) = self.begin_library_job(LibraryJob::ReadableRestore, cx) else {
            return;
        };
        self.library_import_notice = Some(ExportNotice::Status(
            "请选择一个可读导出目录。它将恢复为新的资料库，当前资料库不变。".into(),
        ));
        cx.notify();
        #[cfg(not(test))]
        if let Some(handle) = window.window_handle().downcast::<LibraryShell>() {
            cx.defer(move |app| {
                prompt_for_existing_directory(
                    handle,
                    token,
                    LibraryShell::complete_library_readable_restore_picker,
                    app,
                )
            });
        }
        #[cfg(test)]
        let _ = (window, token);
    }

    fn complete_library_readable_export_picker(
        &mut self,
        token: u64,
        selection: Result<Option<PathBuf>, String>,
        cx: &mut Context<Self>,
    ) {
        let Some((target, _cancel)) =
            self.take_picker(token, LibraryJob::ReadableExport, selection, cx)
        else {
            return;
        };
        // A late edit after the picker opened would make the export refuse
        // ("unsaved edits must be flushed"); flush again at the real boundary.
        if !self.flush_active_session(FlushReason::Export, cx) {
            self.pending_library_import = None;
            self.library_import_notice = Some(ExportNotice::Error(
                "当前笔记尚未保存完成，暂不能导出；请稍后再试。".into(),
            ));
            cx.notify();
            return;
        }
        let repository = self.model.read(cx).repository();
        self.library_import_notice = Some(ExportNotice::Status(format!(
            "正在后台导出整个资料库到 {}…",
            target.display()
        )));
        cx.notify();
        cx.spawn(async move |this, cx| {
            let job_target = target.clone();
            let result = cx
                .background_executor()
                .spawn(async move { export_library_readable(&repository, &job_target) })
                .await;
            let _ = this.update(cx, |shell, shell_cx| {
                shell.finish_library_readable_export(token, target, result, shell_cx);
            });
        })
        .detach();
    }

    fn complete_library_readable_restore_picker(
        &mut self,
        token: u64,
        selection: Result<Option<PathBuf>, String>,
        cx: &mut Context<Self>,
    ) {
        let Some((bundle, _cancel)) =
            self.take_picker(token, LibraryJob::ReadableRestore, selection, cx)
        else {
            return;
        };
        let Some(imports_dir) = cx
            .try_global::<LibraryProfiles>()
            .map(LibraryProfiles::imports_dir)
        else {
            self.pending_library_import = None;
            return;
        };
        self.library_import_notice = Some(ExportNotice::Status(
            "正在后台校验可读导出并恢复为新资料库…".into(),
        ));
        cx.notify();
        cx.spawn(async move |this, cx| {
            let result = cx
                .background_executor()
                .spawn(async move {
                    let result = restore_readable_into_new_library(&imports_dir, &bundle);
                    let has_manifest = result.is_err() && bundle.join("manifest.json").is_file();
                    (result, has_manifest)
                })
                .await;
            let _ = this.update(cx, |shell, shell_cx| {
                shell.finish_library_readable_restore(token, result, shell_cx);
            });
        })
        .detach();
    }

    fn finish_library_readable_export(
        &mut self,
        token: u64,
        target: PathBuf,
        result: Result<LibraryReadableReport, ReadableExportError>,
        cx: &mut Context<Self>,
    ) {
        if !self.finish_pending(token) {
            return;
        }
        self.library_import_notice = Some(match result {
            Ok(report) => ExportNotice::Status(format!(
                "可读导出完成：{} 篇笔记、{} 个附件文件，位于 {}。用浏览器打开其中的 index.html 即可阅读。",
                report.note_count,
                report.resource_count,
                target.display()
            )),
            Err(error) => ExportNotice::Error(format!("可读导出未完成：{error}。资料库未改动。")),
        });
        cx.notify();
    }

    fn finish_library_readable_restore(
        &mut self,
        token: u64,
        (result, has_manifest): (
            Result<(PathBuf, LibraryReadableReport), ReadableExportError>,
            bool,
        ),
        cx: &mut Context<Self>,
    ) {
        if !self.finish_pending(token) {
            return;
        }
        self.library_import_notice = Some(match result {
            Ok((path, report)) => {
                let message = format!(
                    "恢复完成：{} 篇笔记、{} 个附件，已保存为新资料库 {}。当前资料库未改动。",
                    report.note_count,
                    report.resource_count,
                    path.display()
                );
                self.imported_library_ready = Some(path);
                ExportNotice::Status(message)
            }
            Err(error) => ExportNotice::Error(readable_restore_error_message(&error, has_manifest)),
        });
        cx.notify();
    }

    /// Returns the cancel flag when `token`/`job` are the pending picker.
    fn take_picker(
        &mut self,
        token: u64,
        job: LibraryJob,
        selection: Result<Option<PathBuf>, String>,
        cx: &mut Context<Self>,
    ) -> Option<(PathBuf, Arc<AtomicBool>)> {
        let pending = self.pending_library_import.as_mut()?;
        if pending.token != token || pending.job != job || pending.phase != ImportPhase::Picking {
            return None;
        }
        match selection {
            Ok(Some(path)) => {
                pending.phase = ImportPhase::Working;
                Some((path, pending.cancel.clone()))
            }
            Ok(None) => {
                self.pending_library_import = None;
                self.library_import_notice = None;
                cx.notify();
                None
            }
            Err(error) => {
                self.pending_library_import = None;
                self.library_import_notice =
                    Some(ExportNotice::Error(format!("无法选择目录：{error}")));
                cx.notify();
                None
            }
        }
    }

    pub(super) fn complete_library_backup_picker(
        &mut self,
        token: u64,
        selection: Result<Option<PathBuf>, String>,
        cx: &mut Context<Self>,
    ) {
        let Some((target, cancel)) = self.take_picker(token, LibraryJob::Backup, selection, cx)
        else {
            return;
        };
        let Some(profile) = cx
            .try_global::<LibraryProfiles>()
            .map(|profiles| profiles.active.clone())
        else {
            self.pending_library_import = None;
            return;
        };
        self.library_import_notice = Some(ExportNotice::Status(format!(
            "正在后台备份整个资料库到 {}…",
            target.display()
        )));
        cx.notify();
        cx.spawn(async move |this, cx| {
            let job_target = target.clone();
            let result = cx
                .background_executor()
                .spawn(async move { backup_library(&profile, &job_target, &cancel) })
                .await;
            let _ = this.update(cx, |shell, shell_cx| {
                shell.finish_library_backup(token, target, result, shell_cx);
            });
        })
        .detach();
    }

    pub(super) fn complete_library_restore_picker(
        &mut self,
        token: u64,
        selection: Result<Option<PathBuf>, String>,
        cx: &mut Context<Self>,
    ) {
        let Some((backup, cancel)) = self.take_picker(token, LibraryJob::Restore, selection, cx)
        else {
            return;
        };
        let Some(imports_dir) = cx
            .try_global::<LibraryProfiles>()
            .map(LibraryProfiles::imports_dir)
        else {
            self.pending_library_import = None;
            return;
        };
        self.library_import_notice = Some(ExportNotice::Status(
            "正在后台校验备份并恢复为新资料库…".into(),
        ));
        cx.notify();
        cx.spawn(async move |this, cx| {
            let result = cx
                .background_executor()
                .spawn(async move {
                    std::fs::create_dir_all(&imports_dir).map_err(BackupError::Io)?;
                    let destination = unique_library_destination(&imports_dir, &backup)
                        .map_err(BackupError::Io)?;
                    restore_library_backup(&backup, &destination, &cancel)
                })
                .await;
            let _ = this.update(cx, |shell, shell_cx| {
                shell.finish_library_restore(token, result, shell_cx);
            });
        })
        .detach();
    }

    fn finish_library_backup(
        &mut self,
        token: u64,
        target: PathBuf,
        result: Result<BackupReport, BackupError>,
        cx: &mut Context<Self>,
    ) {
        if !self.finish_pending(token) {
            return;
        }
        self.library_import_notice = Some(match result {
            Ok(report) => ExportNotice::Status(format!(
                "备份完成：{} 篇笔记（另有 {} 篇在废纸篓）、{} 个笔记本、{} 个标签、{} 个附件文件，共 {:.1} MB，位于 {}。",
                report.counts.notes,
                report.counts.trashed_notes,
                report.counts.notebooks,
                report.counts.tags,
                report.blobs,
                report.bytes as f64 / 1_048_576.0,
                target.display()
            )),
            Err(BackupError::Cancelled) => {
                ExportNotice::Status("备份已取消；未生成备份目录。".into())
            }
            Err(error) => ExportNotice::Error(format!(
                "备份未完成：{error}。未生成备份目录，资料库未改动。"
            )),
        });
        cx.notify();
    }

    fn finish_library_restore(
        &mut self,
        token: u64,
        result: Result<PublishedLibrary, BackupError>,
        cx: &mut Context<Self>,
    ) {
        if !self.finish_pending(token) {
            return;
        }
        self.library_import_notice = Some(match result {
            Ok(library) => {
                let counts = library.counts;
                let message = format!(
                    "恢复完成：{} 篇笔记（另有 {} 篇在废纸篓）、{} 个笔记本、{} 个标签、{} 个附件，全部通过哈希校验，已保存为新资料库 {}。当前资料库未改动。",
                    counts.notes,
                    counts.trashed_notes,
                    counts.notebooks,
                    counts.tags,
                    counts.resources,
                    library.path.display()
                );
                self.imported_library_ready = Some(library.path);
                ExportNotice::Status(message)
            }
            Err(BackupError::Cancelled) => {
                ExportNotice::Status("恢复已取消；未创建新资料库。".into())
            }
            Err(error) => ExportNotice::Error(format!(
                "恢复未完成：{error}。未创建新资料库，当前资料库未改动。"
            )),
        });
        cx.notify();
    }

    fn finish_pending(&mut self, token: u64) -> bool {
        if self
            .pending_library_import
            .as_ref()
            .is_none_or(|pending| pending.token != token)
        {
            return false;
        }
        self.pending_library_import = None;
        true
    }

    #[cfg(test)]
    pub(crate) fn complete_library_readable_export_picker_for_test(
        &mut self,
        selection: Result<Option<PathBuf>, String>,
        cx: &mut Context<Self>,
    ) {
        let token = self.next_library_import_token;
        self.complete_library_readable_export_picker(token, selection, cx);
    }

    #[cfg(test)]
    pub(crate) fn complete_library_readable_restore_picker_for_test(
        &mut self,
        selection: Result<Option<PathBuf>, String>,
        cx: &mut Context<Self>,
    ) {
        let token = self.next_library_import_token;
        self.complete_library_readable_restore_picker(token, selection, cx);
    }

    #[cfg(test)]
    pub(crate) fn complete_library_backup_picker_for_test(
        &mut self,
        selection: Result<Option<PathBuf>, String>,
        cx: &mut Context<Self>,
    ) {
        let token = self.next_library_import_token;
        self.complete_library_backup_picker(token, selection, cx);
    }

    #[cfg(test)]
    pub(crate) fn complete_library_restore_picker_for_test(
        &mut self,
        selection: Result<Option<PathBuf>, String>,
        cx: &mut Context<Self>,
    ) {
        let token = self.next_library_import_token;
        self.complete_library_restore_picker(token, selection, cx);
    }
}

/// Restores into a fresh directory under `imports_dir`; a failed restore
/// removes that directory so no empty library is left behind.
/// Says in plain words why a folder could not be restored; the underlying
/// error stays at the end for diagnosis.
fn readable_restore_error_message(error: &ReadableExportError, has_manifest: bool) -> String {
    let reason = match error {
        ReadableExportError::Io(io)
            if io.kind() == std::io::ErrorKind::NotFound && !has_manifest =>
        {
            "所选文件夹不是可读导出包（缺少 manifest.json）。请选择用“导出整个资料库为可读 HTML…”生成的文件夹"
        }
        ReadableExportError::Io(io) if io.kind() == std::io::ErrorKind::NotFound => {
            "导出包不完整：缺少所需的文件或文件夹，可能已被移动或删改"
        }
        ReadableExportError::Json(_) => "导出包中的数据文件无法解析，可能已损坏",
        ReadableExportError::InvalidManifest(reason)
            if reason.contains("format or version") || reason.contains("bundle version") =>
        {
            "导出包的格式或版本不受支持"
        }
        ReadableExportError::InvalidManifest(_) | ReadableExportError::ResourceVerification(_) => {
            "导出包未通过完整性校验（正文、历史或附件记录不一致），可能已被修改或损坏"
        }
        _ => "恢复过程出错",
    };
    format!("恢复未完成：{reason}。未创建新资料库，当前资料库未改动。（详情：{error}）")
}

fn restore_readable_into_new_library(
    imports_dir: &std::path::Path,
    bundle: &std::path::Path,
) -> Result<(PathBuf, LibraryReadableReport), ReadableExportError> {
    std::fs::create_dir_all(imports_dir)?;
    let destination = unique_library_destination(imports_dir, bundle)?;
    std::fs::create_dir(&destination)?;
    match restore_library_readable(bundle, &destination) {
        Ok(report) => Ok((destination, report)),
        Err(error) => {
            let _ = std::fs::remove_dir_all(&destination);
            Err(error)
        }
    }
}

type PickerCompletion =
    fn(&mut LibraryShell, u64, Result<Option<PathBuf>, String>, &mut Context<LibraryShell>);

#[cfg(not(test))]
fn prompt_for_new_directory(
    window: WindowHandle<LibraryShell>,
    token: u64,
    suggested_name: &str,
    complete: PickerCompletion,
    cx: &mut App,
) {
    let default_dir = directories::UserDirs::new()
        .and_then(|dirs| dirs.document_dir().map(std::path::Path::to_path_buf))
        .unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")));
    let prompt = crate::file_picker::prompt_for_new_path(cx, &default_dir, Some(suggested_name));
    let prompt = crate::file_picker::bind_to_request(cx, window.into(), prompt, move |app| {
        window.read_with(app, |shell, _| shell.pending_library_import.as_ref().is_some_and(
            |pending| pending.token == token && !pending.cancel.load(Ordering::Relaxed),
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
                complete(shell, token, selection, shell_cx);
            });
        });
    })
    .detach();
}

#[cfg(not(test))]
fn prompt_for_existing_directory(
    window: WindowHandle<LibraryShell>,
    token: u64,
    complete: PickerCompletion,
    cx: &mut App,
) {
    let prompt = crate::file_picker::prompt_for_paths(cx, gpui::PathPromptOptions {
        files: false,
        directories: true,
        multiple: false,
        prompt: Some("恢复".into()),
    });
    let prompt = crate::file_picker::bind_to_request(cx, window.into(), prompt, move |app| {
        window.read_with(app, |shell, _| shell.pending_library_import.as_ref().is_some_and(
            |pending| pending.token == token && !pending.cancel.load(Ordering::Relaxed),
        )).unwrap_or(false)
    });
    cx.spawn(async move |cx| {
        let selection = match prompt.await {
            Ok(Ok(paths)) => Ok(paths.and_then(|paths| paths.into_iter().next())),
            Ok(Err(error)) => Err(error.to_string()),
            Err(error) => Err(error.to_string()),
        };
        let _ = cx.update(move |app| {
            let _ = window.update(app, |shell, _window, shell_cx| {
                complete(shell, token, selection, shell_cx);
            });
        });
    })
    .detach();
}
