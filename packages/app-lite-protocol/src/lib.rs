//! Wire types for Joplin Lite sync protocol v1 (docs/research/sync-protocol-v1.md).
//! Shared by the client and the NAS server; no I/O here.

use serde::{Deserialize, Serialize};

pub const PROTOCOL_VERSION: u32 = 1;
pub const MAX_PUSH_OPS: usize = 100;
pub const MAX_PULL_LIMIT: usize = 500;
pub const MAX_PAYLOAD_BYTES: usize = 4 * 1024 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EntityKind {
    Note,
    Notebook,
    Stack,
    Tag,
    Resource,
}

impl EntityKind {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Note => "note",
            Self::Notebook => "notebook",
            Self::Stack => "stack",
            Self::Tag => "tag",
            Self::Resource => "resource",
        }
    }
    pub fn parse(value: &str) -> Option<Self> {
        Some(match value {
            "note" => Self::Note,
            "notebook" => Self::Notebook,
            "stack" => Self::Stack,
            "tag" => Self::Tag,
            "resource" => Self::Resource,
            _ => return None,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct EntityRef {
    pub kind: EntityKind,
    pub id: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Action {
    Put { payload: serde_json::Value },
    Delete,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Operation {
    pub op_id: String,
    pub device_id: String,
    pub entity: EntityRef,
    /// Revision the client last saw; 0 creates.
    pub base_revision: u64,
    pub action: Action,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PushRequest {
    pub protocol: u32,
    pub device_id: String,
    pub ops: Vec<Operation>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum OpResult {
    Accepted {
        op_id: String,
        revision: u64,
        cursor: u64,
    },
    Conflict {
        op_id: String,
        server_revision: u64,
        server_deleted: bool,
        server_payload: Option<serde_json::Value>,
    },
    Retryable {
        op_id: String,
        reason: String,
        retry_after_ms: u64,
    },
    Permanent {
        op_id: String,
        reason: String,
    },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PushResponse {
    pub results: Vec<OpResult>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PullRequest {
    pub protocol: u32,
    /// Exclusive: changes with a larger cursor are returned.
    pub cursor: u64,
    pub limit: usize,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Change {
    pub cursor: u64,
    pub entity: EntityRef,
    pub revision: u64,
    pub deleted: bool,
    pub payload: Option<serde_json::Value>,
    pub op_id: String,
    pub device_id: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PullResponse {
    pub changes: Vec<Change>,
    pub next_cursor: u64,
    pub has_more: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum BlobStatus {
    Complete,
    Partial { bytes: u64 },
    Missing,
}

/// 32 lowercase hex characters (device, op and entity ids).
pub fn valid_id(value: &str) -> bool {
    value.len() == 32
        && value
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

/// 64 lowercase hex characters.
pub fn valid_sha256(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

/// How a sync call failed, as far as the caller needs to decide what to do.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TransportError {
    /// Network failure, server busy or 5xx: send the same request again later.
    Retryable(String),
    /// The request itself is invalid (4xx): resending it cannot succeed.
    Permanent(String),
    /// Missing or wrong credentials; nothing was applied.
    Unauthorized,
    /// A chunk was not at the end of the server's partial upload.
    OffsetMismatch { expected: u64 },
}

/// One sync server, in-process or over HTTP. Every per-operation outcome is
/// still in `PushResponse`; an `Ok` push is not by itself a success.
pub trait SyncTransport {
    fn push(&self, request: &PushRequest) -> Result<PushResponse, TransportError>;
    fn pull(&self, request: &PullRequest) -> Result<PullResponse, TransportError>;
    fn blob_status(&self, sha256: &str) -> Result<BlobStatus, TransportError>;
    fn put_chunk(
        &self,
        sha256: &str,
        size: u64,
        offset: u64,
        bytes: &[u8],
    ) -> Result<BlobStatus, TransportError>;
    fn read_range(&self, sha256: &str, offset: u64, len: u64) -> Result<Vec<u8>, TransportError>;
}

#[cfg(feature = "client")]
pub mod client;
