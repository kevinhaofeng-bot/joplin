use std::{fmt, path::PathBuf, time::Duration};

use serde::{Deserialize, Serialize};
use serde_json::Value;

pub const PROTOCOL_VERSION: u32 = 1;
pub const MAX_FRAME_BYTES: usize = 8 * 1024 * 1024;
pub const STARTUP_TIMEOUT: Duration = Duration::from_secs(5);
pub const DEFAULT_REQUEST_TIMEOUT: Duration = Duration::from_secs(10);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SidecarState {
    Stopped,
    Starting,
    Ready,
    Stopping,
    Failed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SidecarErrorKind {
    SpawnFailed,
    Timeout,
    SidecarExited,
    ProtocolMismatch,
    InvalidResponse,
    FrameTooLarge,
    Io,
    ProfileInUse,
    ProfileLockRequired,
    ProfileInvalid,
    ProfileNotOwned,
    ProfileAlreadyOpen,
    ProfileNotOpen,
    ProfileOpenFailed,
    StorageError,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SidecarError {
    kind: SidecarErrorKind,
    message: &'static str,
}

impl SidecarError {
    pub fn new(kind: SidecarErrorKind, _detail: impl fmt::Display) -> Self {
        Self {
            kind,
            message: public_message(kind),
        }
    }

    pub fn kind(&self) -> SidecarErrorKind {
        self.kind
    }
    pub fn message(&self) -> &'static str {
        self.message
    }
}

impl fmt::Display for SidecarError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.message)
    }
}
impl std::error::Error for SidecarError {}

fn public_message(kind: SidecarErrorKind) -> &'static str {
    match kind {
        SidecarErrorKind::SpawnFailed => "无法启动兼容组件",
        SidecarErrorKind::Timeout => "兼容组件响应超时",
        SidecarErrorKind::SidecarExited => "兼容组件已退出",
        SidecarErrorKind::ProtocolMismatch => "兼容协议版本不匹配",
        SidecarErrorKind::InvalidResponse => "兼容组件响应无效",
        SidecarErrorKind::FrameTooLarge => "兼容组件响应过大",
        SidecarErrorKind::Io => "兼容组件通信失败",
        SidecarErrorKind::ProfileInUse => "资料库正在被使用",
        SidecarErrorKind::ProfileLockRequired => "资料库写入租约无效",
        SidecarErrorKind::ProfileInvalid => "资料库路径无效",
        SidecarErrorKind::ProfileNotOwned => "资料库不属于 Joplin Lite",
        SidecarErrorKind::ProfileAlreadyOpen => "资料库已经打开",
        SidecarErrorKind::ProfileNotOpen => "资料库尚未打开",
        SidecarErrorKind::ProfileOpenFailed => "无法打开资料库",
        SidecarErrorKind::StorageError => "无法保存资料库",
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SidecarCommand {
    pub executable: PathBuf,
    pub args: Vec<String>,
    pub current_dir: PathBuf,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct RequestFrame {
    pub id: String,
    pub protocol_version: u32,
    pub command: String,
    pub params: Value,
}

#[derive(Debug, Deserialize)]
pub struct ResponseFrame {
    pub id: String,
    pub ok: bool,
    pub result: Option<Value>,
    pub error: Option<ResponseError>,
}

#[derive(Debug, Deserialize)]
pub struct ResponseError {
    pub code: String,
    pub message: String,
}

impl ResponseFrame {
    pub fn id(&self) -> &str {
        &self.id
    }
}

pub(crate) fn request_frame(id: String, command: &str, params: Value) -> RequestFrame {
    RequestFrame {
        id,
        protocol_version: PROTOCOL_VERSION,
        command: command.to_owned(),
        params,
    }
}

pub fn decode_response(line: &str) -> Result<ResponseFrame, SidecarError> {
    let response: ResponseFrame = serde_json::from_str(line)
        .map_err(|_| SidecarError::new(SidecarErrorKind::InvalidResponse, "invalid response"))?;
    if response.id.is_empty()
        || (response.ok && (response.result.is_none() || response.error.is_some()))
        || (!response.ok && (response.error.is_none() || response.result.is_some()))
    {
        return Err(SidecarError::new(
            SidecarErrorKind::InvalidResponse,
            "invalid response",
        ));
    }
    Ok(response)
}

pub fn validate_response_id(
    response: &ResponseFrame,
    expected_id: &str,
) -> Result<(), SidecarError> {
    if response.id == expected_id {
        Ok(())
    } else {
        Err(SidecarError::new(
            SidecarErrorKind::InvalidResponse,
            "response id mismatch",
        ))
    }
}

pub fn validate_hello(response: &ResponseFrame) -> Result<(), SidecarError> {
    let result = response.result.as_ref().ok_or_else(|| {
        SidecarError::new(SidecarErrorKind::InvalidResponse, "missing hello result")
    })?;
    let version = result
        .get("protocolVersion")
        .and_then(Value::as_u64)
        .ok_or_else(|| SidecarError::new(SidecarErrorKind::InvalidResponse, "missing version"))?;
    if version == PROTOCOL_VERSION as u64 {
        Ok(())
    } else {
        Err(SidecarError::new(
            SidecarErrorKind::ProtocolMismatch,
            "protocol mismatch",
        ))
    }
}

pub(crate) fn classify_response_error(response: &ResponseFrame) -> SidecarError {
    match response.error.as_ref().map(|error| error.code.as_str()) {
        Some("PROFILE_IN_USE") => {
            SidecarError::new(SidecarErrorKind::ProfileInUse, "profile in use")
        }
        Some("PROFILE_LOCK_REQUIRED") => SidecarError::new(
            SidecarErrorKind::ProfileLockRequired,
            "profile lock required",
        ),
        Some("PROFILE_INVALID") => {
            SidecarError::new(SidecarErrorKind::ProfileInvalid, "profile invalid")
        }
        Some("PROFILE_NOT_OWNED") => {
            SidecarError::new(SidecarErrorKind::ProfileNotOwned, "profile not owned")
        }
        Some("PROFILE_ALREADY_OPEN") => {
            SidecarError::new(SidecarErrorKind::ProfileAlreadyOpen, "profile already open")
        }
        Some("PROFILE_NOT_OPEN") => {
            SidecarError::new(SidecarErrorKind::ProfileNotOpen, "profile not open")
        }
        Some("PROFILE_OPEN_FAILED") => {
            SidecarError::new(SidecarErrorKind::ProfileOpenFailed, "profile open failed")
        }
        Some("STORAGE_ERROR") => SidecarError::new(SidecarErrorKind::StorageError, "storage error"),
        Some("PROTOCOL_MISMATCH") => {
            SidecarError::new(SidecarErrorKind::ProtocolMismatch, "protocol mismatch")
        }
        _ => SidecarError::new(SidecarErrorKind::InvalidResponse, "sidecar failure"),
    }
}
