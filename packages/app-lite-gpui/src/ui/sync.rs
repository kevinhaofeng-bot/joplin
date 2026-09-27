//! Sync with a personal server (docs/research/sync-client-design-v1.md).
//!
//! The server address and token live in `sync.json` next to the library
//! (mode 0600), written by "同步设置…". "立即同步" saves the open note, runs
//! one pass on the background executor, then rereads the open note so the
//! editor never keeps a revision the sync replaced. The status line always
//! says whether work is only saved locally or confirmed by the server.
//!
//! Once configured, sync also runs on its own (`AutoSync`): 5 s after local
//! changes are saved (Evernote schedules its background upsync 5 s out), and
//! every 5 minutes to pick up remote changes. It never runs while the open
//! note has unsaved input, backs off while the server is unreachable, and
//! pauses after a credential or protocol error until "立即同步" succeeds.
//!
//! Changes the server refused, or remote changes that could not be read,
//! stay listed under "同步问题…" until they resolve. A refused upload is
//! retried only when the person presses "重试": that sends the item's
//! current local state once.

use super::*;
use crate::app::{OpenSyncSettings, ShowSyncFailures, SyncNow};
use crate::library_profile::LibraryProfiles;
use app_lite_core::sync::{SyncError, SyncReport, retry_failure, sync_once};
use app_lite_core::{SyncConflict, SyncFailure};
use app_lite_protocol::client::HttpTransport;
use std::path::Path;
use std::time::{Duration, Instant};

const AUTO_SYNC_TICK: Duration = Duration::from_secs(5);
const AUTO_SYNC_PULL_INTERVAL: Duration = Duration::from_secs(5 * 60);
const AUTO_SYNC_FIRST_BACKOFF: Duration = Duration::from_secs(30);
const AUTO_SYNC_MAX_BACKOFF: Duration = Duration::from_secs(5 * 60);

#[derive(Default)]
pub(crate) struct AutoSync {
    task: Option<Task<()>>,
    running_automatic: bool,
    last_finished: Option<Instant>,
    pub(super) retry_at: Option<Instant>,
    pub(super) backoff: Option<Duration>,
    /// Set by a credential or protocol error; cleared by a manual sync.
    pub(super) paused: bool,
    /// Latest head the server announced; a sync is due while it differs
    /// from the local cursor.
    pub(super) announced_head: Option<u64>,
    #[cfg(test)]
    attempts: usize,
}

pub(super) const SYNC_CONFIG_FILE: &str = "sync.json";

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum ShellSyncStatus {
    Unconfigured,
    Idle,
    Running,
    Succeeded {
        report: SyncReport,
        /// Title of the conflict copy that kept the open note's unsaved edit.
        kept_edit: Option<String>,
    },
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
        self.auto_sync.paused = false;
        self.auto_sync.retry_at = None;
        self.auto_sync.backoff = None;
        self.reconnect_event_link_now();
        self.start_sync(config, false, cx);
    }

    fn start_sync(&mut self, config: SyncConfig, automatic: bool, cx: &mut Context<Self>) {
        self.sync_status = ShellSyncStatus::Running;
        self.auto_sync.running_automatic = automatic;
        #[cfg(test)]
        {
            self.auto_sync.attempts += usize::from(automatic);
        }
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

    pub(super) fn start_auto_sync(&mut self, cx: &mut Context<Self>) {
        self.auto_sync.task = Some(cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(AUTO_SYNC_TICK).await;
                if this
                    .update(cx, |shell, shell_cx| shell.auto_sync_tick(shell_cx))
                    .is_err()
                {
                    break;
                }
            }
        }));
    }

    pub(super) fn auto_sync_tick(&mut self, cx: &mut Context<Self>) {
        let config = (!self.auto_sync.paused)
            .then(|| self.active_profile(cx))
            .flatten()
            .and_then(|profile| load_config(&profile).ok().flatten());
        let Some(config) = config else {
            self.stop_event_link();
            return;
        };
        self.ensure_event_link(&config.server_url, &config.token, cx);
        if self.sync_status == ShellSyncStatus::Running
            || self.active_session_has_unsaved_changes(cx)
        {
            return;
        }
        let now = cx.background_executor().now();
        if self.auto_sync.retry_at.is_some_and(|at| now < at) {
            return;
        }
        let repository = self.model.read_with(cx, |model, _| model.repository());
        let parked = repository
            .sync_failures()
            .map(|failures| failures.iter().filter(|failure| failure.can_retry).count())
            .unwrap_or(0) as i64;
        let sendable = repository.sync_pending_count().unwrap_or(0) - parked;
        let remote_due = self.remote_changes_announced(cx);
        let pull_due = self
            .auto_sync
            .last_finished
            .is_none_or(|at| now.duration_since(at) >= AUTO_SYNC_PULL_INTERVAL);
        if sendable > 0 || pull_due || remote_due {
            self.start_sync(config, true, cx);
        }
    }

    pub(super) fn remote_changes_announced(&self, cx: &App) -> bool {
        self.auto_sync.announced_head.is_some_and(|head| {
            self.model
                .read_with(cx, |model, _| model.repository())
                .sync_cursor()
                .is_ok_and(|cursor| cursor != head)
        })
    }

    /// Schedules the next automatic attempt from how this one ended.
    fn note_auto_sync_outcome(&mut self, result: &Result<SyncReport, SyncError>, cx: &App) {
        let now = cx.background_executor().now();
        self.auto_sync.last_finished = Some(now);
        let unreachable = match result {
            Ok(report) => report.retryable > 0,
            Err(_) => {
                if self.auto_sync.running_automatic {
                    self.auto_sync.paused = true;
                }
                false
            }
        };
        if unreachable {
            let backoff = self
                .auto_sync
                .backoff
                .map_or(AUTO_SYNC_FIRST_BACKOFF, |previous| {
                    (previous * 2).min(AUTO_SYNC_MAX_BACKOFF)
                });
            self.auto_sync.backoff = Some(backoff);
            self.auto_sync.retry_at = Some(now + backoff);
        } else {
            self.auto_sync.backoff = None;
            self.auto_sync.retry_at = None;
        }
    }

    pub(super) fn finish_sync(
        &mut self,
        result: Result<SyncReport, SyncError>,
        cx: &mut Context<Self>,
    ) {
        self.note_auto_sync_outcome(&result, cx);
        let mut kept_edit = None;
        match self.open_note_overtaken_by_sync(cx) {
            Overtaken::No => {}
            Overtaken::Yes => {
                // Reread the open note and remount the editor at the
                // revision the sync installed.
                self.remount_current_surface_after_organization_commit = true;
            }
            Overtaken::KeptAs(title) => {
                self.remount_current_surface_after_organization_commit = true;
                kept_edit = Some(title);
            }
            Overtaken::Unkept(message) => {
                // Never replace an editor whose text could not be kept.
                self.sync_status = ShellSyncStatus::Failed(message);
                cx.notify();
                return;
            }
        }
        self.sync_status = match result {
            Ok(report) => ShellSyncStatus::Succeeded { report, kept_edit },
            Err(error) => ShellSyncStatus::Failed(failure_message(&error)),
        };
        let reload = self.model.update(cx, |model, model_cx| {
            let result = model.reload_after_sync();
            model_cx.notify();
            result
        });
        if let Err(error) = reload {
            self.sync_status = ShellSyncStatus::Failed(format!("同步后刷新失败：{error}"));
        }
        self.sync_editor_surface(cx);
        // Changes announced while this sync ran.
        if self.remote_changes_announced(cx) {
            self.auto_sync_tick(cx);
        }
        cx.notify();
    }

    /// Whether the sync replaced the revision the open editor is based on.
    /// An unsaved edit on top of it is kept as a conflict copy first, the
    /// same outcome a saved but unsynced edit gets from the pull.
    fn open_note_overtaken_by_sync(&mut self, cx: &mut Context<Self>) -> Overtaken {
        let Some(session) = self.note_session.clone() else {
            return Overtaken::No;
        };
        let (repository, note_id) = self.model.read_with(cx, |model, _| {
            (model.repository(), model.active_session_note_id().cloned())
        });
        let Some(note_id) = self.surface_note_id.clone().or(note_id) else {
            return Overtaken::No;
        };
        let based_on = session.read(cx).expected_revision();
        let stored = match repository.load_note(&note_id) {
            Ok(stored) => stored,
            Err(error) => return Overtaken::Unkept(format!("同步后无法读取当前笔记：{error}")),
        };
        if stored
            .as_ref()
            .is_some_and(|note| note.revision == based_on && note.deleted_time.is_none())
        {
            return Overtaken::No;
        }
        let edit = match session.update(cx, |session, session_cx| session.unsaved_edit(session_cx))
        {
            Ok(Some(edit)) => edit,
            Ok(None) => return Overtaken::Yes,
            Err(error) => {
                return Overtaken::Unkept(format!(
                    "当前笔记已被其他设备修改，但无法读取你未保存的编辑（{error}）；编辑器内容保留，请先复制再同步。"
                ));
            }
        };
        match repository.save_overtaken_edit_as_conflict_copy(&note_id, based_on, &edit.0, &edit.1)
        {
            Ok(copy) => Overtaken::KeptAs(copy.title),
            Err(error) => Overtaken::Unkept(format!(
                "当前笔记已被其他设备修改，未能把你未保存的编辑另存为冲突副本（{error}）；编辑器内容保留，请先复制再同步。"
            )),
        }
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

    pub(super) fn sync_conflicts(&self, cx: &App) -> Vec<SyncConflict> {
        self.model
            .read_with(cx, |model, _| model.repository())
            .sync_conflicts()
            .unwrap_or_default()
    }

    pub(super) fn toggle_sync_failures(&mut self, cx: &mut Context<Self>) {
        self.sync_failures_open = !self.sync_failures_open;
        cx.notify();
    }

    fn settle_conflict(&mut self, copy_id: &app_lite_core::NoteId, cx: &mut Context<Self>) {
        let repository = self.model.read_with(cx, |model, _| model.repository());
        if let Err(error) = repository.sync_resolve_conflict(copy_id) {
            self.sync_status = ShellSyncStatus::Failed(format!("无法标记冲突副本：{error}"));
        }
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
        let conflicts = self.sync_conflicts(cx).len();
        if failures > 0 {
            pending.push_str(&format!("；{failures} 项同步问题"));
        }
        if conflicts > 0 {
            pending.push_str(&format!("；{conflicts} 个冲突副本待处理"));
        }
        if failures + conflicts > 0 {
            pending.push_str("，点此查看");
        }
        match &self.sync_status {
            ShellSyncStatus::Unconfigured => format!(
                "未配置同步：在“同步设置…”生成的 {SYNC_CONFIG_FILE} 中填写服务器地址与 token{pending}"
            ),
            ShellSyncStatus::Idle => format!("尚未同步{pending}"),
            ShellSyncStatus::Running => "正在同步…".into(),
            ShellSyncStatus::Succeeded { report, .. }
                if report.retryable > 0 && report.accepted == 0 && report.pulled == 0 =>
            {
                let retry = if self.auto_sync.retry_at.is_some() {
                    "，稍后自动重试"
                } else {
                    ""
                };
                format!("未能连接同步服务器{retry}{pending}")
            }
            ShellSyncStatus::Succeeded { report, kept_edit } => {
                let mut text = format!("已同步：上传 {}、下载 {}", report.accepted, report.pulled);
                if report.server_restored {
                    text.push_str("；服务器曾从较早的备份恢复，已与本机重新对账");
                }
                if report.conflicts > 0 {
                    text.push_str(&format!("，{} 处冲突已保存为“冲突副本”", report.conflicts));
                }
                if let Some(copy) = kept_edit {
                    text.push_str(&format!(
                        "；当前笔记已被其他设备修改，你未保存的编辑另存为“{copy}”"
                    ));
                }
                if report.retryable > 0 {
                    text.push_str("，部分内容因网络中断未完成，下次同步继续");
                }
                text + &pending
            }
            ShellSyncStatus::Failed(message) if self.auto_sync.paused => {
                format!("{message}自动同步已暂停，修正后点“立即同步”{pending}")
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
                    .child(div().text_color(rgba(0x25342bff)).child("同步状态"))
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
        panel = panel.child(
            div()
                .text_size(px(11.0))
                .text_color(rgba(0x536f59ff))
                .child(self.event_link_text()),
        );
        let conflicts = self.sync_conflicts(cx);
        if failures.is_empty() && conflicts.is_empty() {
            panel = panel.child(div().text_color(rgba(0x536f59ff)).child("没有同步问题。"));
        }
        if !conflicts.is_empty() {
            panel = panel.child(
                div()
                    .text_size(px(11.0))
                    .text_color(rgba(0x8a978dff))
                    .child("冲突副本：两个版本都已保留。比较后删除不要的一份，或标记为已处理。"),
            );
        }
        for (index, conflict) in conflicts.into_iter().enumerate() {
            let original = conflict.original_title.as_deref().map_or_else(
                || "原笔记已不在本机".to_owned(),
                |title| format!("原笔记“{title}”"),
            );
            let open_id = conflict.copy_id.clone();
            let settle_id = conflict.copy_id.clone();
            panel = panel.child(
                div()
                    .flex()
                    .gap(px(8.0))
                    .items_start()
                    .child(
                        div()
                            .flex_1()
                            .text_color(rgba(0x8d6a27ff))
                            .child(format!("“{}”（{original}）", conflict.copy_title)),
                    )
                    .child(
                        div()
                            .id(("sync-conflict-open", index))
                            .debug_selector(move || format!("sync-conflict-open-{index}"))
                            .px(px(10.0))
                            .py(px(3.0))
                            .rounded(px(4.0))
                            .bg(rgba(0xf1f4f1ff))
                            .cursor_pointer()
                            .on_mouse_down(
                                MouseButton::Left,
                                cx.listener(move |shell, _event, window, cx| {
                                    shell.apply_action(
                                        AppAction::SelectNote(open_id.clone()),
                                        window,
                                        cx,
                                    );
                                }),
                            )
                            .child("打开"),
                    )
                    .child(
                        div()
                            .id(("sync-conflict-settle", index))
                            .debug_selector(move || format!("sync-conflict-settle-{index}"))
                            .px(px(10.0))
                            .py(px(3.0))
                            .rounded(px(4.0))
                            .bg(rgba(0xf1f4f1ff))
                            .cursor_pointer()
                            .on_mouse_down(
                                MouseButton::Left,
                                cx.listener(move |shell, _event, _window, cx| {
                                    shell.settle_conflict(&settle_id, cx);
                                }),
                            )
                            .child("已处理"),
                    ),
            );
        }
        if !failures.is_empty() {
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

enum Overtaken {
    No,
    Yes,
    KeptAs(String),
    Unkept(String),
}

#[cfg(test)]
impl LibraryShell {
    pub(super) fn auto_sync_attempts_for_test(&self) -> usize {
        self.auto_sync.attempts
    }
}
