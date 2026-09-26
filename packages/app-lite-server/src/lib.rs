//! Joplin Lite personal sync server: storage for protocol v1
//! (docs/research/sync-protocol-v1.md). Transport-free; the HTTP layer and
//! NAS deployment come after the protocol review gate.

use std::{
    fs::{self, File, OpenOptions},
    io::{self, Read, Seek, SeekFrom, Write},
    path::{Path, PathBuf},
    sync::Mutex,
};

use app_lite_protocol::{
    Action, BlobStatus, Change, EntityKind, EntityRef, MAX_PAYLOAD_BYTES, MAX_PULL_LIMIT,
    MAX_PUSH_OPS, OpResult, Operation, PROTOCOL_VERSION, PullRequest, PullResponse, PushRequest,
    PushResponse, valid_id, valid_sha256,
};
use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};
use sha2::{Digest, Sha256};
use thiserror::Error;

pub const MAX_BLOB_BYTES: u64 = 512 * 1024 * 1024;

#[derive(Debug, Error)]
pub enum ServerError {
    #[error("unsupported protocol version {0}")]
    Protocol(u32),
    #[error("request too large or malformed: {0}")]
    BadRequest(&'static str),
    #[error("upload offset mismatch, expected {expected}")]
    OffsetMismatch { expected: u64 },
    #[error("uploaded bytes do not match the declared SHA-256")]
    HashMismatch,
    #[error("blob not found")]
    NotFound,
    #[error("storage: {0}")]
    Storage(#[from] rusqlite::Error),
    #[error("io: {0}")]
    Io(#[from] io::Error),
    #[error("json: {0}")]
    Json(#[from] serde_json::Error),
}

pub struct ServerStore {
    connection: Mutex<Connection>,
    blobs: PathBuf,
    uploads: PathBuf,
}

impl ServerStore {
    pub fn open(root: &Path) -> Result<Self, ServerError> {
        let blobs = root.join("blobs");
        let uploads = root.join("uploads");
        fs::create_dir_all(&blobs)?;
        fs::create_dir_all(&uploads)?;
        let connection = Connection::open(root.join("sync.sqlite"))?;
        connection.pragma_update(None, "journal_mode", "WAL")?;
        connection.pragma_update(None, "synchronous", "FULL")?;
        connection.execute_batch(
            "CREATE TABLE IF NOT EXISTS ops (op_id TEXT PRIMARY KEY NOT NULL, content_hash TEXT NOT NULL, result TEXT NOT NULL);
             CREATE TABLE IF NOT EXISTS entities (kind TEXT NOT NULL, id TEXT NOT NULL, revision INTEGER NOT NULL, deleted INTEGER NOT NULL, payload TEXT, PRIMARY KEY(kind, id));
             CREATE TABLE IF NOT EXISTS changes (cursor INTEGER PRIMARY KEY AUTOINCREMENT, kind TEXT NOT NULL, id TEXT NOT NULL, revision INTEGER NOT NULL, deleted INTEGER NOT NULL, payload TEXT, op_id TEXT NOT NULL, device_id TEXT NOT NULL);",
        )?;
        Ok(Self {
            connection: Mutex::new(connection),
            blobs,
            uploads,
        })
    }

    pub fn push(&self, request: PushRequest) -> Result<PushResponse, ServerError> {
        if request.protocol != PROTOCOL_VERSION {
            return Err(ServerError::Protocol(request.protocol));
        }
        if request.ops.len() > MAX_PUSH_OPS || !valid_id(&request.device_id) {
            return Err(ServerError::BadRequest("too many ops or invalid device id"));
        }
        let mut results = Vec::with_capacity(request.ops.len());
        for op in &request.ops {
            results.push(self.apply(&request.device_id, op)?);
        }
        Ok(PushResponse { results })
    }

    fn apply(&self, device_id: &str, op: &Operation) -> Result<OpResult, ServerError> {
        let content_hash = format!("{:x}", Sha256::digest(serde_json::to_vec(op)?));
        let mut connection = self.connection.lock().expect("server mutex poisoned");
        let tx = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        if let Some((hash, result)) = tx
            .query_row(
                "SELECT content_hash, result FROM ops WHERE op_id = ?1",
                [&op.op_id],
                |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
            )
            .optional()?
        {
            return Ok(if hash == content_hash {
                serde_json::from_str(&result)?
            } else {
                OpResult::Permanent {
                    op_id: op.op_id.clone(),
                    reason: "op_id reused for different content".into(),
                }
            });
        }
        let result = match validate(device_id, op) {
            Err(reason) => OpResult::Permanent {
                op_id: op.op_id.clone(),
                reason: reason.into(),
            },
            Ok(payload) => {
                let current = tx
                    .query_row(
                        "SELECT revision, deleted, payload FROM entities WHERE kind = ?1 AND id = ?2",
                        params![op.entity.kind.as_str(), op.entity.id],
                        |row| {
                            Ok((
                                row.get::<_, i64>(0)? as u64,
                                row.get::<_, i64>(1)? != 0,
                                row.get::<_, Option<String>>(2)?,
                            ))
                        },
                    )
                    .optional()?;
                let (revision, deleted, stored) = current.unwrap_or((0, false, None));
                if op.base_revision != revision {
                    OpResult::Conflict {
                        op_id: op.op_id.clone(),
                        server_revision: revision,
                        server_deleted: deleted,
                        server_payload: stored.map(|p| serde_json::from_str(&p)).transpose()?,
                    }
                } else {
                    let next = revision + 1;
                    let is_delete = payload.is_none();
                    tx.execute(
                        "INSERT INTO entities (kind, id, revision, deleted, payload) VALUES (?1, ?2, ?3, ?4, ?5)
                         ON CONFLICT(kind, id) DO UPDATE SET revision = excluded.revision, deleted = excluded.deleted, payload = excluded.payload",
                        params![op.entity.kind.as_str(), op.entity.id, next as i64, is_delete, payload],
                    )?;
                    tx.execute(
                        "INSERT INTO changes (kind, id, revision, deleted, payload, op_id, device_id) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                        params![op.entity.kind.as_str(), op.entity.id, next as i64, is_delete, payload, op.op_id, device_id],
                    )?;
                    OpResult::Accepted {
                        op_id: op.op_id.clone(),
                        revision: next,
                        cursor: tx.last_insert_rowid() as u64,
                    }
                }
            }
        };
        tx.execute(
            "INSERT INTO ops (op_id, content_hash, result) VALUES (?1, ?2, ?3)",
            params![op.op_id, content_hash, serde_json::to_string(&result)?],
        )?;
        tx.commit()?;
        Ok(result)
    }

    pub fn pull(&self, request: PullRequest) -> Result<PullResponse, ServerError> {
        if request.protocol != PROTOCOL_VERSION {
            return Err(ServerError::Protocol(request.protocol));
        }
        let limit = request.limit.clamp(1, MAX_PULL_LIMIT);
        let connection = self.connection.lock().expect("server mutex poisoned");
        let mut statement = connection.prepare(
            "SELECT cursor, kind, id, revision, deleted, payload, op_id, device_id FROM changes WHERE cursor > ?1 ORDER BY cursor LIMIT ?2",
        )?;
        let mut changes = statement
            .query_map(params![request.cursor as i64, limit as i64 + 1], |row| {
                Ok((
                    row.get::<_, i64>(0)? as u64,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, i64>(3)? as u64,
                    row.get::<_, i64>(4)? != 0,
                    row.get::<_, Option<String>>(5)?,
                    row.get::<_, String>(6)?,
                    row.get::<_, String>(7)?,
                ))
            })?
            .map(|row| {
                let (cursor, kind, id, revision, deleted, payload, op_id, device_id) = row?;
                Ok(Change {
                    cursor,
                    entity: EntityRef {
                        kind: EntityKind::parse(&kind)
                            .ok_or(ServerError::BadRequest("stored kind"))?,
                        id,
                    },
                    revision,
                    deleted,
                    payload: payload.map(|p| serde_json::from_str(&p)).transpose()?,
                    op_id,
                    device_id,
                })
            })
            .collect::<Result<Vec<_>, ServerError>>()?;
        let has_more = changes.len() > limit;
        changes.truncate(limit);
        let next_cursor = changes
            .last()
            .map_or(request.cursor, |change| change.cursor);
        Ok(PullResponse {
            changes,
            next_cursor,
            has_more,
        })
    }

    pub fn blob_status(&self, sha256: &str) -> Result<BlobStatus, ServerError> {
        if !valid_sha256(sha256) {
            return Err(ServerError::BadRequest("invalid sha256"));
        }
        if self.blobs.join(sha256).is_file() {
            return Ok(BlobStatus::Complete);
        }
        Ok(match fs::metadata(self.part(sha256)) {
            Ok(metadata) => BlobStatus::Partial {
                bytes: metadata.len(),
            },
            Err(_) => BlobStatus::Missing,
        })
    }

    /// Appends one chunk at exactly the received length (resumable). The blob
    /// becomes visible only after the whole file matches `sha256`.
    pub fn put_chunk(
        &self,
        sha256: &str,
        size: u64,
        offset: u64,
        bytes: &[u8],
    ) -> Result<BlobStatus, ServerError> {
        if !valid_sha256(sha256) || size == 0 || size > MAX_BLOB_BYTES {
            return Err(ServerError::BadRequest("invalid blob identity or size"));
        }
        // Keep this inode: unlinking a lock file would let a new request lock
        // a different inode while an older request still owns the old lock.
        // An OS lock covers separate ServerStore instances/processes too.
        let mut lock = OpenOptions::new()
            .create(true)
            .read(true)
            .write(true)
            .truncate(false)
            .open(self.uploads.join(format!("{sha256}.lock")))?;
        lock.lock()?;
        let mut declared = String::new();
        lock.read_to_string(&mut declared)?;
        if declared.is_empty() {
            write!(lock, "{size}")?;
            lock.sync_all()?;
            File::open(&self.uploads)?.sync_all()?;
        } else if declared.parse::<u64>().ok() != Some(size) {
            return Err(ServerError::BadRequest("upload size changed"));
        }
        if self.blobs.join(sha256).is_file() {
            return Ok(BlobStatus::Complete);
        }
        let part = self.part(sha256);
        let received = fs::metadata(&part).map(|m| m.len()).unwrap_or(0);
        if offset != received {
            return Err(ServerError::OffsetMismatch { expected: received });
        }
        if received + bytes.len() as u64 > size {
            return Err(ServerError::BadRequest("chunk exceeds declared size"));
        }
        {
            let mut file = OpenOptions::new().create(true).append(true).open(&part)?;
            file.write_all(bytes)?;
            file.sync_all()?;
        }
        let received = received + bytes.len() as u64;
        if received < size {
            return Ok(BlobStatus::Partial { bytes: received });
        }
        let mut digest = Sha256::new();
        io::copy(&mut File::open(&part)?, &mut digest)?;
        if format!("{:x}", digest.finalize()) != sha256 {
            fs::remove_file(&part)?;
            return Err(ServerError::HashMismatch);
        }
        fs::rename(&part, self.blobs.join(sha256))?;
        File::open(&self.blobs)?.sync_all()?;
        Ok(BlobStatus::Complete)
    }

    pub fn read_range(&self, sha256: &str, offset: u64, len: u64) -> Result<Vec<u8>, ServerError> {
        if !valid_sha256(sha256) {
            return Err(ServerError::BadRequest("invalid sha256"));
        }
        let mut file = File::open(self.blobs.join(sha256)).map_err(|_| ServerError::NotFound)?;
        file.seek(SeekFrom::Start(offset))?;
        let mut out = Vec::new();
        file.take(len.min(16 * 1024 * 1024)).read_to_end(&mut out)?;
        Ok(out)
    }

    fn part(&self, sha256: &str) -> PathBuf {
        self.uploads.join(format!("{sha256}.part"))
    }
}

/// Returns the payload to store (None for delete) or a permanent reason.
fn validate(device_id: &str, op: &Operation) -> Result<Option<String>, &'static str> {
    if op.device_id != device_id {
        return Err("op device differs from request device");
    }
    if !valid_id(&op.op_id) || !valid_id(&op.entity.id) {
        return Err("invalid op or entity id");
    }
    match &op.action {
        Action::Delete => Ok(None),
        Action::Put { payload } => {
            if !payload.is_object() {
                return Err("payload must be a JSON object");
            }
            let text = serde_json::to_string(payload).map_err(|_| "payload not serializable")?;
            if text.len() > MAX_PAYLOAD_BYTES {
                return Err("payload too large");
            }
            Ok(Some(text))
        }
    }
}
