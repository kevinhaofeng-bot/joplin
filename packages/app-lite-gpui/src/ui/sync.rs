//! Sync with a personal server (docs/research/sync-client-design-v1.md).
//!
//! The server address and token live in `sync.json` next to the library
//! (mode 0600), written by the sync settings panel. "立即同步" saves the open note, runs
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
use std::io::{Read, Write};
use std::os::unix::fs::OpenOptionsExt;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::time::{Duration, Instant};

const AUTO_SYNC_TICK: Duration = Duration::from_secs(5);
const AUTO_SYNC_PULL_INTERVAL: Duration = Duration::from_secs(5 * 60);
const AUTO_SYNC_FIRST_BACKOFF: Duration = Duration::from_secs(30);
const AUTO_SYNC_MAX_BACKOFF: Duration = Duration::from_secs(5 * 60);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum SyncHealth {
    Unconfigured,
    Healthy,
    Recovering,
    Attention,
}

impl SyncHealth {
    fn label(self) -> &'static str {
        match self {
            Self::Unconfigured => "状态：未配置同步",
            Self::Healthy => "状态：正常",
            Self::Recovering => "状态：恢复中（会自动重试）",
            Self::Attention => "状态：需要处理",
        }
    }
}

#[derive(Default)]
pub(crate) struct AutoSync {
    task: Option<Task<()>>,
    running_automatic: bool,
    pub(super) last_finished: Option<Instant>,
    pub(super) retry_at: Option<Instant>,
    pub(super) backoff: Option<Duration>,
    /// Set by a credential or protocol error; cleared by a manual sync.
    pub(super) paused: bool,
    pub(super) config_unverified: bool,
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

#[derive(Clone, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
pub(super) struct SyncConfig {
    server_url: String,
    token: String,
    /// PEM certificate to trust besides the public roots, for an HTTPS
    /// server with a self-signed certificate.
    #[serde(default)]
    server_certificate_pem: Option<String>,
    #[serde(flatten)]
    extra: std::collections::BTreeMap<String, serde_json::Value>,
}

pub(super) struct SyncSettings {
    profile: std::path::PathBuf,
    pub(super) server_url: Entity<TitleInput>,
    pub(super) token: Entity<TitleInput>,
    pub(super) certificate_pem: Option<String>,
    certificate_label: Option<String>,
    pub(super) error: Option<String>,
    pub(super) picker_generation: u64,
}

impl SyncConfig {
    pub(super) fn transport(&self) -> Result<HttpTransport, app_lite_protocol::TransportError> {
        HttpTransport::with_trusted_certificate(
            &self.server_url,
            &self.token,
            self.server_certificate_pem.as_deref(),
        )
    }
}

fn read_config_file(profile: &Path) -> Result<Option<SyncConfig>, String> {
    let path = profile.join(SYNC_CONFIG_FILE);
    let bytes = match std::fs::read(&path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(format!("无法读取同步设置：{error}")),
    };
    let config: SyncConfig =
        serde_json::from_slice(&bytes).map_err(|error| format!("同步设置格式有误：{error}"))?;
    Ok(Some(config))
}

fn load_config(profile: &Path) -> Result<Option<SyncConfig>, String> {
    Ok(read_config_file(profile)?
        .filter(|config| !config.server_url.trim().is_empty() && !config.token.trim().is_empty()))
}

fn save_config(profile: &Path, config: &SyncConfig) -> Result<(), String> {
    let mut temporary = tempfile::NamedTempFile::new_in(profile)
        .map_err(|error| format!("无法创建同步设置临时文件：{error}"))?;
    temporary
        .as_file()
        .set_permissions(std::fs::Permissions::from_mode(0o600))
        .map_err(|error| format!("无法设置同步设置权限：{error}"))?;
    serde_json::to_writer_pretty(&mut temporary, config)
        .map_err(|error| format!("无法编码同步设置：{error}"))?;
    temporary
        .write_all(b"\n")
        .map_err(|error| format!("无法写入同步设置：{error}"))?;
    temporary
        .as_file()
        .sync_all()
        .map_err(|error| format!("无法同步同步设置：{error}"))?;
    temporary
        .persist(profile.join(SYNC_CONFIG_FILE))
        .map_err(|error| format!("无法保存同步设置：{}", error.error))?;
    std::fs::File::open(profile)
        .and_then(|dir| dir.sync_all())
        .map_err(|error| format!("无法同步资料库目录：{error}"))?;
    Ok(())
}

const UNTRUSTED_CERTIFICATE: &str =
    "同步失败：服务器的 TLS 证书不受信任。若使用自签名证书，请在“同步设置…”中导入服务器 PEM 证书。";

fn validate_sync_config(config: &SyncConfig) -> Result<(), String> {
    let url = url::Url::parse(&config.server_url)
        .map_err(|_| "服务器地址必须是完整的 HTTPS 地址。".to_owned())?;
    let loopback = url.host_str().is_some_and(|host| {
        host.eq_ignore_ascii_case("localhost")
            || host
                .trim_start_matches('[')
                .trim_end_matches(']')
                .parse::<std::net::IpAddr>()
                .is_ok_and(|ip| ip.is_loopback())
    });
    if url.scheme() != "https" && !(url.scheme() == "http" && loopback) {
        return Err("服务器地址必须使用 HTTPS；仅本机测试地址可使用 HTTP。".into());
    }
    if url.host_str().is_none() || !url.username().is_empty() || url.password().is_some() {
        return Err("服务器地址无效，请不要在地址中填写账号或密码。".into());
    }
    if config.token.trim().is_empty() {
        return Err("请输入访问令牌。".into());
    }
    config
        .transport()
        .map_err(|error| format!("证书文件无效：{error:?}"))?;
    Ok(())
}

pub(super) fn read_certificate_file(path: &Path) -> Result<String, String> {
    let file = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NONBLOCK)
        .open(path)
        .map_err(|error| format!("无法读取证书文件：{error}"))?;
    let metadata = file
        .metadata()
        .map_err(|error| format!("无法检查证书文件：{error}"))?;
    if !metadata.is_file() {
        return Err("请选择普通的 PEM 证书文件。".into());
    }
    if metadata.len() > 1024 * 1024 {
        return Err("证书文件超过 1 MiB。".into());
    }
    let mut bytes = Vec::new();
    file.take(1024 * 1024 + 1)
        .read_to_end(&mut bytes)
        .map_err(|error| format!("无法读取证书文件：{error}"))?;
    if bytes.len() > 1024 * 1024 {
        return Err("证书文件超过 1 MiB。".into());
    }
    let pem = String::from_utf8(bytes).map_err(|_| "证书文件必须是 UTF-8 PEM 文本。".to_owned())?;
    HttpTransport::with_trusted_certificate("https://localhost", "test", Some(&pem))
        .map_err(|error| format!("证书文件无效：{error:?}"))?;
    Ok(pem)
}

pub(super) fn masked_sync_token(token: &str) -> String {
    // One ASCII glyph per UTF-8 byte keeps TitleInput's byte offsets aligned
    // with the rendered line without ever handing the token to the painter.
    "*".repeat(token.len())
}

pub(super) fn refused_reason_message(reason: &str) -> String {
    if reason.contains("certificate is not trusted") {
        UNTRUSTED_CERTIFICATE.into()
    } else {
        format!("同步失败：服务器拒绝了请求（{reason}）。")
    }
}

fn failure_message(error: &SyncError) -> String {
    match error {
        SyncError::Unauthorized => "同步失败：服务器拒绝了凭据，请检查同步设置中的 token。".into(),
        SyncError::Rejected(reason) => refused_reason_message(reason),
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
    if failure.waiting {
        return "其他设备的这项修改引用的附件还没到，附件到达后会自动补上，无需操作。".into();
    }
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
                    let transport = config.transport().map_err(|error| {
                        SyncError::Rejected(format!("同步设置中的证书无效：{error:?}"))
                    })?;
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
        self.check_link_environment();
        self.ensure_event_link(&config, cx);
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
        if matches!(&result, Ok(report) if report.retryable == 0) {
            self.auto_sync.config_unverified = false;
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
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(profile) = self.active_profile(cx) else {
            return;
        };
        let (config, error) = match read_config_file(&profile) {
            Ok(config) => (config, None),
            Err(error) => (None, Some(error)),
        };
        let server_url = cx.new(|input_cx| {
            TitleInput::new(
                config
                    .as_ref()
                    .map_or(String::new(), |config| config.server_url.clone()),
                input_cx,
            )
        });
        let token = cx.new(|input_cx| {
            TitleInput::new(
                config
                    .as_ref()
                    .map_or(String::new(), |config| config.token.clone()),
                input_cx,
            )
        });
        server_url.read(cx).focus_handle().focus(window);
        self.sync_settings = Some(SyncSettings {
            profile,
            server_url,
            token,
            certificate_pem: config
                .as_ref()
                .and_then(|config| config.server_certificate_pem.clone()),
            certificate_label: config.as_ref().and_then(|config| {
                config
                    .server_certificate_pem
                    .as_ref()
                    .map(|_| "已保存的证书".to_owned())
            }),
            error,
            picker_generation: self.next_sync_settings_generation,
        });
        self.next_sync_settings_generation += 1;
        cx.notify();
    }

    pub(super) fn cancel_sync_settings(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.sync_settings = None;
        self.focus_handle.focus(window);
        cx.notify();
    }

    pub(super) fn save_sync_settings(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(settings) = self.sync_settings.as_mut() else {
            return;
        };
        if self.sync_status == ShellSyncStatus::Running {
            settings.error = Some("正在同步，请等待本轮完成后再保存新设置。".into());
            cx.notify();
            return;
        }
        let mut config = match read_config_file(&settings.profile) {
            Ok(Some(config)) => config,
            Ok(None) => SyncConfig {
                server_url: String::new(),
                token: String::new(),
                server_certificate_pem: None,
                extra: Default::default(),
            },
            Err(error) => {
                settings.error = Some(error);
                cx.notify();
                return;
            }
        };
        config.server_url = settings
            .server_url
            .read(cx)
            .text()
            .trim()
            .trim_end_matches('/')
            .to_owned();
        config.token = settings.token.read(cx).text().trim().to_owned();
        config.server_certificate_pem = settings.certificate_pem.clone();
        let result =
            validate_sync_config(&config).and_then(|()| save_config(&settings.profile, &config));
        match result {
            Ok(()) => {
                self.sync_settings = None;
                self.auto_sync.paused = false;
                self.auto_sync.retry_at = None;
                self.auto_sync.backoff = None;
                self.auto_sync.last_finished = None;
                self.auto_sync.announced_head = None;
                self.auto_sync.config_unverified = true;
                self.sync_status = ShellSyncStatus::Idle;
                self.stop_event_link();
                self.focus_handle.focus(window);
            }
            Err(error) => settings.error = Some(error),
        }
        cx.notify();
    }

    fn clear_sync_certificate(
        &mut self,
        _: &MouseDownEvent,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(settings) = self.sync_settings.as_mut() {
            settings.picker_generation = self.next_sync_settings_generation;
            self.next_sync_settings_generation += 1;
            settings.certificate_pem = None;
            settings.certificate_label = None;
            settings.error = None;
            cx.notify();
        }
    }

    fn import_sync_certificate(
        &mut self,
        _: &MouseDownEvent,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(generation) = self.begin_sync_certificate_pick() else {
            return;
        };
        let prompt = cx.prompt_for_paths(PathPromptOptions {
            files: true,
            directories: false,
            multiple: false,
            prompt: Some("选择 PEM 服务器证书".into()),
        });
        cx.spawn(async move |this, cx| {
            let result = match prompt.await {
                Ok(Ok(Some(paths))) => paths.into_iter().next().map(|path| {
                    let pem = read_certificate_file(&path)?;
                    Ok((
                        pem,
                        path.file_name()
                            .unwrap_or_default()
                            .to_string_lossy()
                            .into_owned(),
                    ))
                }),
                Ok(Ok(None)) => None,
                Ok(Err(error)) => Some(Err(format!("无法选择证书：{error}"))),
                Err(error) => Some(Err(format!("无法选择证书：{error}"))),
            };
            if let Some(result) = result {
                let _ = this.update(cx, |shell, shell_cx| {
                    shell.complete_sync_certificate_pick(generation, result, shell_cx)
                });
            }
        })
        .detach();
    }

    pub(super) fn begin_sync_certificate_pick(&mut self) -> Option<u64> {
        let settings = self.sync_settings.as_mut()?;
        let generation = self.next_sync_settings_generation;
        self.next_sync_settings_generation += 1;
        settings.picker_generation = generation;
        Some(generation)
    }

    pub(super) fn complete_sync_certificate_pick(
        &mut self,
        generation: u64,
        result: Result<(String, String), String>,
        cx: &mut Context<Self>,
    ) {
        let Some(settings) = self
            .sync_settings
            .as_mut()
            .filter(|settings| settings.picker_generation == generation)
        else {
            return;
        };
        match result {
            Ok((pem, label)) => {
                settings.certificate_pem = Some(pem);
                settings.certificate_label = Some(label);
                settings.error = None;
            }
            Err(error) => settings.error = Some(error),
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

    /// Health grade and privacy-safe facts for the panel (after the
    /// reference clients' `0debug` snapshot).
    pub(super) fn sync_health(&self, cx: &App) -> (SyncHealth, Vec<String>) {
        use crate::ui::sync_events::LinkState;
        let repository = self.model.read_with(cx, |model, _| model.repository());
        let configured = self
            .active_profile(cx)
            .and_then(|profile| load_config(&profile).ok().flatten())
            .is_some();
        if !configured {
            return (SyncHealth::Unconfigured, Vec::new());
        }
        let now_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |elapsed| elapsed.as_millis() as i64);
        let age = |at: i64| Duration::from_millis((now_ms - at).max(0) as u64);
        let pending = repository.sync_pending_count().unwrap_or(0);
        let oldest = repository
            .sync_oldest_pending_time()
            .ok()
            .flatten()
            .map(age);
        let failures = self.sync_failures(cx);
        let waiting = failures.iter().filter(|failure| failure.waiting).count();
        let problems = failures.len() - waiting + self.sync_conflicts(cx).len();
        let link = self.event_link_state();
        let health = if self.auto_sync.paused
            || problems > 0
            || oldest.is_some_and(|age| age > Duration::from_secs(10 * 60))
        {
            SyncHealth::Attention
        } else if self.auto_sync.config_unverified
            || matches!(link, LinkState::Connecting | LinkState::Waiting)
            || waiting > 0
            || self.auto_sync.retry_at.is_some()
            || oldest.is_some_and(|age| age > Duration::from_secs(90))
        {
            SyncHealth::Recovering
        } else {
            SyncHealth::Healthy
        };
        let minutes = |duration: Duration| match duration.as_secs() {
            seconds if seconds < 60 => format!("{seconds} 秒"),
            seconds if seconds < 3600 => format!("{} 分钟", seconds / 60),
            seconds => format!("{} 小时", seconds / 3600),
        };
        let mut facts = vec![
            if self.auto_sync.config_unverified {
                "新同步设置尚未确认连接；最近一次成功记录属于旧设置".into()
            } else {
                match repository.sync_last_success_time().ok().flatten() {
                    Some(at) => format!("最近一次成功同步：{}前", minutes(age(at))),
                    None => "最近一次成功同步：尚无".into(),
                }
            },
            match oldest {
                Some(oldest) => format!("待同步 {pending} 项，最久已等 {}", minutes(oldest)),
                None => "待同步：无".into(),
            },
        ];
        if self.auto_sync.paused {
            facts.push("自动同步已暂停，修正后点“立即同步”".into());
        }
        facts.extend(self.event_link_facts());
        (health, facts)
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
            ShellSyncStatus::Unconfigured => {
                format!("未配置同步：打开“同步设置…”填写服务器地址与访问令牌{pending}")
            }
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
    fn render_sync_input(
        &self,
        id: &'static str,
        input: Entity<TitleInput>,
        secret: bool,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        let layout_input = input.clone();
        let paint_input = input.clone();
        let canvas = canvas(
            move |bounds, _window, cx| {
                let _ = layout_input.update(cx, |input, _| input.record_bounds(bounds));
                layout_input.clone()
            },
            move |bounds, entity, window, cx| {
                let (display, selection, focus) = entity.read_with(cx, |input, _| {
                    let display = if secret {
                        masked_sync_token(input.text())
                    } else {
                        input.text().to_owned()
                    };
                    (
                        display,
                        input.selection().clone(),
                        input.focus_handle().clone(),
                    )
                });
                let len = display.len();
                let line = window.text_system().shape_line(
                    SharedString::from(display),
                    px(14.0),
                    &[TextRun {
                        len,
                        font: window.text_style().font(),
                        color: rgba(0x172033ff).into(),
                        background_color: None,
                        underline: None,
                        strikethrough: None,
                    }],
                    None,
                );
                let scroll_x =
                    (line.x_for_index(selection.end) - bounds.size.width + px(4.0)).max(px(0.0));
                let layout_bounds =
                    Bounds::new(point(bounds.left() - scroll_x, bounds.top()), bounds.size);
                entity.update(cx, |input, _| {
                    input.record_layout(layout_bounds, line.clone())
                });
                if focus.is_focused(window) && !selection.is_empty() {
                    window.paint_quad(gpui::fill(
                        Bounds::from_corners(
                            point(
                                layout_bounds.left() + line.x_for_index(selection.start),
                                bounds.top(),
                            ),
                            point(
                                layout_bounds.left() + line.x_for_index(selection.end),
                                bounds.bottom(),
                            ),
                        ),
                        rgba(0x00a82d33),
                    ));
                }
                line.paint(layout_bounds.origin, bounds.size.height, window, cx)
                    .ok();
                if focus.is_focused(window) && selection.is_empty() {
                    window.paint_quad(gpui::fill(
                        Bounds::new(
                            point(
                                layout_bounds.left() + line.x_for_index(selection.start),
                                bounds.top(),
                            ),
                            size(px(1.0), bounds.size.height),
                        ),
                        rgba(EVERNOTE_GREEN),
                    ));
                }
                if focus.is_focused(window) {
                    window.handle_input(
                        &focus,
                        ElementInputHandler::new(bounds, paint_input.clone()),
                        cx,
                    );
                }
            },
        )
        .w_full()
        .h(px(28.0));
        let pointer_input = input.clone();
        let moving_input = input.clone();
        let up_input = input.clone();
        let key_input = input.clone();
        div()
            .id(id)
            .debug_selector(move || id.to_owned())
            .w_full()
            .h(px(32.0))
            .px(px(8.0))
            .overflow_hidden()
            .rounded(px(5.0))
            .bg(rgba(0xffffffff))
            .border_1()
            .border_color(rgba(0xcbd5e1ff))
            .key_context("SyncSettingsInput")
            .track_focus(input.read(cx).focus_handle())
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |_shell, event: &MouseDownEvent, window, cx| {
                    pointer_input.update(cx, |input, input_cx| {
                        input.begin_pointer_selection(event.position, event.modifiers.shift);
                        input.focus_handle().focus(window);
                        input_cx.notify();
                    });
                    cx.stop_propagation();
                }),
            )
            .on_mouse_move(
                cx.listener(move |_shell, event: &MouseMoveEvent, _window, cx| {
                    if event.pressed_button == Some(MouseButton::Left) {
                        moving_input.update(cx, |input, input_cx| {
                            if input.extend_pointer_selection(event.position).is_some() {
                                input_cx.notify();
                            }
                        });
                        cx.stop_propagation();
                    }
                }),
            )
            .on_mouse_up(
                MouseButton::Left,
                cx.listener(move |_shell, _event, _window, cx| {
                    up_input.update(cx, |input, _| input.end_pointer_selection());
                    cx.stop_propagation();
                }),
            )
            .on_key_down(cx.listener(move |shell, event: &KeyDownEvent, window, cx| {
                let key = event.keystroke.key.as_str();
                let shift = event.keystroke.modifiers.shift;
                let secondary = event.keystroke.modifiers.secondary();
                let handled = match key {
                    "escape" => {
                        shell.cancel_sync_settings(window, cx);
                        true
                    }
                    "enter" => {
                        shell.save_sync_settings(window, cx);
                        true
                    }
                    "tab" => {
                        if let Some(settings) = shell.sync_settings.as_ref() {
                            let next = if secret {
                                &settings.server_url
                            } else {
                                &settings.token
                            };
                            next.read(cx).focus_handle().focus(window);
                            cx.notify();
                        }
                        true
                    }
                    "backspace" => {
                        key_input.update(cx, |input, input_cx| {
                            input.delete_backward();
                            input_cx.notify();
                        });
                        true
                    }
                    "delete" => {
                        key_input.update(cx, |input, input_cx| {
                            input.delete_forward();
                            input_cx.notify();
                        });
                        true
                    }
                    "left" => {
                        key_input.update(cx, |input, input_cx| {
                            input.move_horizontal(false, shift);
                            input_cx.notify();
                        });
                        true
                    }
                    "right" => {
                        key_input.update(cx, |input, input_cx| {
                            input.move_horizontal(true, shift);
                            input_cx.notify();
                        });
                        true
                    }
                    "home" => {
                        key_input.update(cx, |input, input_cx| {
                            input.move_to_edge(false, shift);
                            input_cx.notify();
                        });
                        true
                    }
                    "end" => {
                        key_input.update(cx, |input, input_cx| {
                            input.move_to_edge(true, shift);
                            input_cx.notify();
                        });
                        true
                    }
                    "a" if secondary => {
                        key_input.update(cx, |input, input_cx| {
                            input.select_all();
                            input_cx.notify();
                        });
                        true
                    }
                    "v" if secondary => {
                        key_input
                            .update(cx, |input, input_cx| input.paste_from_clipboard(input_cx));
                        true
                    }
                    "c" if secondary && !secret => {
                        cx.write_to_clipboard(gpui::ClipboardItem::new_string(
                            key_input.read(cx).selected_text().to_owned(),
                        ));
                        true
                    }
                    "x" if secondary && !secret => {
                        let selected = key_input.read(cx).selected_text().to_owned();
                        if !selected.is_empty() {
                            cx.write_to_clipboard(gpui::ClipboardItem::new_string(selected));
                            key_input.update(cx, |input, input_cx| {
                                input.delete_forward();
                                input_cx.notify();
                            });
                        }
                        true
                    }
                    "c" | "x" if secondary => true,
                    _ => false,
                };
                if handled {
                    cx.stop_propagation();
                }
            }))
            .child(canvas)
            .into_any_element()
    }

    pub(super) fn render_sync_settings(&self, cx: &mut Context<Self>) -> Option<gpui::AnyElement> {
        let settings = self.sync_settings.as_ref()?;
        let server_input = self.render_sync_input(
            "sync-settings-server-url",
            settings.server_url.clone(),
            false,
            cx,
        );
        let token_input =
            self.render_sync_input("sync-settings-token", settings.token.clone(), true, cx);
        let cert_label = settings
            .certificate_label
            .clone()
            .unwrap_or_else(|| "未导入；公开可信证书无需导入".into());
        let mut panel = div()
            .id("sync-settings")
            .debug_selector(|| "sync-settings".to_owned())
            .absolute()
            .top(px(60.0))
            .right(px(24.0))
            .w(px(470.0))
            .occlude()
            .p(px(16.0))
            .rounded(px(9.0))
            .bg(rgba(0xffffffff))
            .border_1()
            .border_color(rgba(0xc9d3ccff))
            .shadow_lg()
            .flex()
            .flex_col()
            .gap(px(9.0))
            .text_size(px(12.0))
            .child(
                div()
                    .text_size(px(17.0))
                    .text_color(rgba(0x25342bff))
                    .child("同步设置"),
            )
            .child(div().child("服务器地址（HTTPS）"))
            .child(server_input)
            .child(div().child("访问令牌"))
            .child(token_input)
            .child(
                div()
                    .text_color(rgba(0x536f59ff))
                    .child("令牌已遮蔽。设置仅保存在当前资料库。"),
            )
            .child(div().child("自签名服务器证书（可选）"))
            .child(div().text_color(rgba(0x536f59ff)).child(cert_label))
            .child(
                div()
                    .flex()
                    .gap(px(8.0))
                    .child(
                        div()
                            .id("sync-settings-import-certificate")
                            .debug_selector(|| "sync-settings-import-certificate".to_owned())
                            .cursor_pointer()
                            .on_mouse_down(
                                MouseButton::Left,
                                cx.listener(Self::import_sync_certificate),
                            )
                            .child("导入 PEM 文件…"),
                    )
                    .child(
                        div()
                            .id("sync-settings-clear-certificate")
                            .debug_selector(|| "sync-settings-clear-certificate".to_owned())
                            .cursor_pointer()
                            .on_mouse_down(
                                MouseButton::Left,
                                cx.listener(Self::clear_sync_certificate),
                            )
                            .child("清除证书"),
                    ),
            )
            .child(
                div()
                    .text_color(rgba(0x536f59ff))
                    .child("保存只更新本机设置；连接与同步结果请看“立即同步”的状态。"),
            );
        if let Some(error) = &settings.error {
            panel = panel.child(
                div()
                    .id("sync-settings-error")
                    .debug_selector(|| "sync-settings-error".to_owned())
                    .text_color(rgba(0xa34838ff))
                    .child(error.clone()),
            );
        }
        Some(
            panel
                .child(
                    div()
                        .flex()
                        .justify_end()
                        .gap(px(16.0))
                        .child(
                            div()
                                .id("sync-settings-cancel")
                                .debug_selector(|| "sync-settings-cancel".to_owned())
                                .cursor_pointer()
                                .on_mouse_down(
                                    MouseButton::Left,
                                    cx.listener(|shell, _, window, cx| {
                                        shell.cancel_sync_settings(window, cx)
                                    }),
                                )
                                .child("取消"),
                        )
                        .child(
                            div()
                                .id("sync-settings-save")
                                .debug_selector(|| "sync-settings-save".to_owned())
                                .cursor_pointer()
                                .text_color(rgba(EVERNOTE_GREEN))
                                .on_mouse_down(
                                    MouseButton::Left,
                                    cx.listener(|shell, _, window, cx| {
                                        shell.save_sync_settings(window, cx)
                                    }),
                                )
                                .child("保存"),
                        ),
                )
                .into_any_element(),
        )
    }

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
        let (health, facts) = self.sync_health(cx);
        panel = panel
            .child(
                div()
                    .id("sync-health")
                    .debug_selector(|| "sync-health".to_owned())
                    .text_color(match health {
                        SyncHealth::Attention => rgba(0xa34838ff),
                        SyncHealth::Recovering => rgba(0x8d6a27ff),
                        _ => rgba(0x536f59ff),
                    })
                    .child(health.label()),
            )
            .child(
                div()
                    .text_size(px(11.0))
                    .text_color(rgba(0x536f59ff))
                    .child(self.event_link_text()),
            );
        for fact in facts {
            panel = panel.child(
                div()
                    .text_size(px(11.0))
                    .text_color(rgba(0x8a978dff))
                    .child(fact),
            );
        }
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
