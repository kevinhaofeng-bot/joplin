//! Sync with a personal server (docs/research/sync-client-design-v1.md).
//!
//! The server address and token live in `sync.json` next to the library
//! (mode 0600), written by "同步设置…". "立即同步" saves the open note, runs
//! one pass on the background executor, then rereads the open note so the
//! editor never keeps a revision the sync replaced. The status line always
//! says whether work is only saved locally or confirmed by the server.
//!
//! Changes the server refused, or remote changes that could not be read,
//! stay listed under "同步问题…" until they resolve. A refused upload is
//! retried only when the person presses "重试": that sends the item's
//! current local state once.

use super::*;
use crate::app::{OpenSyncSettings, ShowSyncFailures, SyncNow};
use crate::library_profile::LibraryProfiles;
use app_lite_core::SyncFailure;
use app_lite_core::sync::{SyncError, SyncReport, retry_failure, sync_once};
use app_lite_protocol::client::HttpTransport;
use std::path::Path;

pub(super) const SYNC_CONFIG_FILE: &str = "sync.json";

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum ShellSyncStatus {
    Unconfigured,
    Idle,
    Running,
    Succeeded { report: SyncReport },
    Failed(String),
}

#[derive(serde::Deserialize, serde::Serialize)]
struct SyncConfig {
    server_url: String,
    token: String,
}

fn load_config(profile: &Path) -> Result<Option<SyncConfig>, String> {
    let path = profile.join(SYNC_CONFIG_FILE);
    let bytes = match std::fs::read(&path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(format!("无法读取同步设置：{error}")),
    };
    let config: SyncConfig =
        serde_json::from_slice(&bytes).map_err(|error| format!("同步设置格式有误：{error}"))?;
    if config.server_url.trim().is_empty() || config.token.trim().is_empty() {
        return Ok(None);
    }
    Ok(Some(config))
}

/// Creates a template the person fills in; never overwrites an existing file.
fn write_template(profile: &Path) -> std::io::Result<std::path::PathBuf> {
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;
    let path = profile.join(SYNC_CONFIG_FILE);
    match std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&path)
    {
        Ok(mut file) => {
            let template = SyncConfig {
                server_url: String::new(),
                token: String::new(),
            };
            file.write_all(&serde_json::to_vec_pretty(&template).expect("serializes"))?;
            file.sync_all()?;
        }
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
        Err(error) => return Err(error),
    }
    Ok(path)
}

fn failure_message(error: &SyncError) -> String {
    match error {
        SyncError::Unauthorized => "同步失败：服务器拒绝了凭据，请检查同步设置中的 token。".into(),
        SyncError::Rejected(reason) => format!("同步失败：服务器拒绝了请求（{reason}）。"),
        SyncError::Library(error) => format!("同步失败：本地资料库错误（{error}）。"),
    }
}

fn entity_label(entity_type: &str) -> &'static str {
    match entity_type {
        "note" => "笔记",
        "notebook" => "笔记本",
        "stack" => "笔记本组",
        "tag" => "标签",
        "resource" => "附件",
        _ => "项目",
    }
}

fn failure_reason(failure: &SyncFailure) -> String {
    if !failure.can_retry {
        return format!(
            "服务器上的这条修改无法读取，已跳过；本机内容未受影响（{}）。",
            failure.reason
        );
    }
    if failure.reason.contains("payload too large") {
        return "内容超过服务器单项上限（4 MiB）。缩短或拆分后点“重试”。".into();
    }
    format!("服务器拒绝了这项修改（{}）。", failure.reason)
}

/// One row of the "同步问题" list, as shown.
pub(crate) fn failure_row_text(failure: &SyncFailure) -> String {
    let name = failure
        .title
        .as_deref()
        .filter(|title| !title.trim().is_empty())
        .map_or_else(|| "（已不在本机）".to_owned(), |title| format!("“{title}”"));
    format!(
        "{}{name}：{}",
        entity_label(&failure.entity_type),
        failure_reason(failure)
    )
}

impl LibraryShell {
    fn active_profile(&self, cx: &App) -> Option<std::path::PathBuf> {
        cx.try_global::<LibraryProfiles>()
            .map(|profiles| profiles.active.clone())
    }

    pub(super) fn sync_now_action(&mut self, _: &SyncNow, _: &mut Window, cx: &mut Context<Self>) {
        if self.sync_status == ShellSyncStatus::Running {
            return;
        }
        let Some(profile) = self.active_profile(cx) else {
            self.sync_status = ShellSyncStatus::Failed("无法确定资料库位置，未同步。".into());
            cx.notify();
            return;
        };
        let config = match load_config(&profile) {
            Ok(Some(config)) => config,
            Ok(None) => {
                self.sync_status = ShellSyncStatus::Unconfigured;
                cx.notify();
                return;
            }
            Err(message) => {
                self.sync_status = ShellSyncStatus::Failed(message);
                cx.notify();
                return;
            }
        };
        if !self.flush_active_session(FlushReason::ManualSync, cx) {
            self.sync_status =
                ShellSyncStatus::Failed("当前笔记尚未保存完成，暂不能同步；请稍后再试。".into());
            cx.notify();
            return;
        }
        self.sync_status = ShellSyncStatus::Running;
        cx.notify();
        let repository = self.model.read_with(cx, |model, _| model.repository());
        cx.spawn(async move |this, cx| {
            let result = cx
                .background_executor()
                .spawn(async move {
                    let transport = HttpTransport::new(&config.server_url, &config.token);
                    sync_once(&repository, &transport)
                })
                .await;
            let _ = this.update(cx, |shell, shell_cx| shell.finish_sync(result, shell_cx));
        })
        .detach();
    }

    fn finish_sync(&mut self, result: Result<SyncReport, SyncError>, cx: &mut Context<Self>) {
        self.sync_status = match result {
            Ok(report) => ShellSyncStatus::Succeeded { report },
            Err(error) => ShellSyncStatus::Failed(failure_message(&error)),
        };
        // Remote changes may have replaced the open note's revision: reread
        // it and remount the editor at that revision.
        self.remount_current_surface_after_organization_commit = true;
        let reload = self.model.update(cx, |model, model_cx| {
            let result = model.reload_after_sync();
            model_cx.notify();
            result
        });
        if let Err(error) = reload {
            self.sync_status = ShellSyncStatus::Failed(format!("同步后刷新失败：{error}"));
        }
        self.sync_editor_surface(cx);
        cx.notify();
    }

    pub(super) fn open_sync_settings_action(
        &mut self,
        _: &OpenSyncSettings,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(profile) = self.active_profile(cx) else {
            return;
        };
        match write_template(&profile) {
            Ok(path) => {
                self.sync_status = ShellSyncStatus::Unconfigured;
                #[cfg(not(test))]
                {
                    let _ = std::process::Command::new("open")
                        .arg("-R")
                        .arg(&path)
                        .spawn();
                }
                #[cfg(test)]
                let _ = path;
            }
            Err(error) => {
                self.sync_status = ShellSyncStatus::Failed(format!("无法创建同步设置：{error}"));
            }
        }
        cx.notify();
    }

    pub(super) fn sync_failures(&self, cx: &App) -> Vec<SyncFailure> {
        self.model
            .read_with(cx, |model, _| model.repository())
            .sync_failures()
            .unwrap_or_default()
    }

    pub(super) fn show_sync_failures_action(
        &mut self,
        _: &ShowSyncFailures,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.sync_failures_open = true;
        cx.notify();
    }

    pub(super) fn toggle_sync_failures(&mut self, cx: &mut Context<Self>) {
        self.sync_failures_open = !self.sync_failures_open && !self.sync_failures(cx).is_empty();
        cx.notify();
    }

    /// Marks one refused upload for a single new attempt, then syncs.
    pub(super) fn retry_sync_failure(
        &mut self,
        op_id: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.sync_status == ShellSyncStatus::Running {
            return;
        }
        let repository = self.model.read_with(cx, |model, _| model.repository());
        match retry_failure(&repository, op_id) {
            Ok(true) => self.sync_now_action(&SyncNow, window, cx),
            Ok(false) => cx.notify(),
            Err(error) => {
                self.sync_status = ShellSyncStatus::Failed(format!("无法重试：{error}"));
                cx.notify();
            }
        }
    }

    pub(super) fn sync_status_text(&self, cx: &App) -> String {
        let pending = self
            .model
            .read_with(cx, |model, _| model.repository())
            .sync_pending_count()
            .unwrap_or(0);
        let failures = self.sync_failures(cx).len();
        let mut pending = if pending > 0 {
            format!("，{pending} 项仅保存在本机、待同步")
        } else {
            String::new()
        };
        if failures > 0 {
            pending.push_str(&format!("；{failures} 项同步问题，点此查看"));
        }
        match &self.sync_status {
            ShellSyncStatus::Unconfigured => format!(
                "未配置同步：在“同步设置…”生成的 {SYNC_CONFIG_FILE} 中填写服务器地址与 token{pending}"
            ),
            ShellSyncStatus::Idle => format!("尚未同步{pending}"),
            ShellSyncStatus::Running => "正在同步…".into(),
            ShellSyncStatus::Succeeded { report } => {
                let mut text = format!("已同步：上传 {}、下载 {}", report.accepted, report.pulled);
                if report.server_restored {
                    text.push_str("；服务器曾从较早的备份恢复，已与本机重新对账");
                }
                if report.conflicts > 0 {
                    text.push_str(&format!("，{} 处冲突已保存为“冲突副本”", report.conflicts));
                }
                if report.retryable > 0 {
                    text.push_str("，部分内容因网络中断未完成，下次同步继续");
                }
                text + &pending
            }
            ShellSyncStatus::Failed(message) => format!("{message}{pending}"),
        }
    }
}

impl LibraryShell {
    pub(super) fn render_sync_failures(&self, cx: &mut Context<Self>) -> Option<gpui::AnyElement> {
        if !self.sync_failures_open {
            return None;
        }
        let failures = self.sync_failures(cx);
        let running = self.sync_status == ShellSyncStatus::Running;
        let mut panel = div()
            .id("sync-failures")
            .debug_selector(|| "sync-failures".to_owned())
            .absolute()
            .bottom(px(130.0))
            .right(px(14.0))
            .w(px(460.0))
            .max_h(px(360.0))
            .overflow_y_scroll()
            .p(px(12.0))
            .rounded(px(8.0))
            .bg(rgba(0xffffffff))
            .border_1()
            .border_color(rgba(0xc9d3ccff))
            .shadow_lg()
            .flex()
            .flex_col()
            .gap(px(8.0))
            .text_size(px(12.0))
            .child(
                div()
                    .flex()
                    .justify_between()
                    .child(div().text_color(rgba(0x25342bff)).child("同步问题"))
                    .child(
                        div()
                            .id("sync-failures-close")
                            .debug_selector(|| "sync-failures-close".to_owned())
                            .cursor_pointer()
                            .text_color(rgba(0x536f59ff))
                            .on_mouse_down(
                                MouseButton::Left,
                                cx.listener(|shell, _event, _window, cx| {
                                    shell.sync_failures_open = false;
                                    cx.notify();
                                }),
                            )
                            .child("关闭"),
                    ),
            );
        if failures.is_empty() {
            panel = panel.child(div().text_color(rgba(0x536f59ff)).child("没有同步问题。"));
        } else {
            panel = panel.child(
                div()
                    .text_size(px(11.0))
                    .text_color(rgba(0x8a978dff))
                    .child("下列内容都已保存在本机，只是没有同步到服务器。"),
            );
        }
        for (index, failure) in failures.into_iter().enumerate() {
            let op_id = failure.op_id.clone();
            let row = div().flex().gap(px(8.0)).items_start().child(
                div()
                    .flex_1()
                    .text_color(rgba(0xa34838ff))
                    .child(failure_row_text(&failure)),
            );
            let row = if failure.can_retry && !running {
                row.child(
                    div()
                        .id(("sync-failure-retry", index))
                        .debug_selector(move || format!("sync-failure-retry-{index}"))
                        .px(px(10.0))
                        .py(px(3.0))
                        .rounded(px(4.0))
                        .bg(rgba(0xf1f4f1ff))
                        .cursor_pointer()
                        .on_mouse_down(
                            MouseButton::Left,
                            cx.listener(move |shell, _event, window, cx| {
                                shell.retry_sync_failure(&op_id, window, cx);
                            }),
                        )
                        .child("重试"),
                )
            } else {
                row
            };
            panel = panel.child(row);
        }
        Some(panel.into_any_element())
    }
}
