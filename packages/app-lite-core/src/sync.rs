//! Client sync engine (docs/research/sync-client-design-v1.md §2). Runs on a
//! background thread against any `SyncTransport`; only per-operation results
//! change local state. Mechanism after Evernote's `MutationUpsyncActivity`
//! (persistent queue, batches, per-item outcome); the protocol and conflict
//! policy are this project's own design.

use std::io::{self, Read, Seek, SeekFrom};

use app_lite_protocol::{
    Action, BlobStatus, EntityKind, EntityRef, MAX_PULL_LIMIT, MAX_PUSH_OPS, OpResult, Operation,
    PROTOCOL_VERSION, PullRequest, PushRequest, SyncTransport, TransportError,
};

use crate::{LibraryError, LibraryRepository, SyncFailure};

/// Stays under the protocol's 4 MiB push body with room for the envelope.
const MAX_PUSH_BATCH_BYTES: usize = 3 * 1024 * 1024;
/// Attachment transfer unit; the HTTP layer allows 4 MiB.
const BLOB_CHUNK_BYTES: u64 = 1024 * 1024;

#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct SyncReport {
    pub accepted: usize,
    /// Remote changes applied locally.
    pub pulled: usize,
    /// Remote changes skipped as malformed; listed by `sync_failures`.
    pub skipped: usize,
    pub conflicts: usize,
    /// Operations still waiting because the server or network was unavailable.
    pub retryable: usize,
    pub permanent: usize,
    /// The server had been restored from an older backup; local state was
    /// reconciled against it from the start.
    pub server_restored: bool,
}

#[derive(Debug, thiserror::Error)]
pub enum SyncError {
    #[error("library: {0}")]
    Library(#[from] LibraryError),
    #[error("sync server refused the credentials")]
    Unauthorized,
    #[error("sync server rejected the request: {0}")]
    Rejected(String),
}

/// One sync pass: download, then upload. An upload conflict triggers another
/// download (which turns the local edit into a conflict copy) and an upload
/// of that copy. Transport failures that may succeed later are counted as
/// `retryable`; every in-flight op is then resent unchanged next time.
pub fn sync_once(
    repository: &LibraryRepository,
    transport: &dyn SyncTransport,
) -> Result<SyncReport, SyncError> {
    let device_id = repository.sync_device_id()?;
    let mut report = SyncReport::default();
    if !pull(repository, transport, &mut report)? {
        return Ok(report);
    }
    let conflicts = report.conflicts;
    push(repository, transport, &device_id, &mut report)?;
    if report.conflicts > conflicts && pull(repository, transport, &mut report)? {
        push(repository, transport, &device_id, &mut report)?;
    }
    Ok(report)
}

/// Returns false when the server was unreachable.
fn pull(
    repository: &LibraryRepository,
    transport: &dyn SyncTransport,
    report: &mut SyncReport,
) -> Result<bool, SyncError> {
    let mut forgot_server_state = false;
    match server_still_has_anchor(repository, transport) {
        Ok(true) => {}
        Ok(false) => {
            repository.sync_forget_server_state()?;
            forgot_server_state = true;
            report.server_restored = true;
        }
        Err(TransportError::Retryable(_)) => {
            report.retryable += 1;
            return Ok(false);
        }
        Err(error) => return Err(fatal(error)),
    }
    loop {
        let request = PullRequest {
            protocol: PROTOCOL_VERSION,
            cursor: repository.sync_cursor()?,
            limit: MAX_PULL_LIMIT,
        };
        let response = match transport.pull(&request) {
            Ok(response) => response,
            Err(TransportError::Retryable(_)) => {
                report.retryable += 1;
                return Ok(false);
            }
            Err(error) => return Err(fatal(error)),
        };
        if response.next_cursor < request.cursor {
            // The server was restored from an older backup. Start over from
            // its beginning: equal content is adopted, differing local
            // content becomes a conflict copy, and what the server lost is
            // uploaded again.
            if forgot_server_state {
                return Err(SyncError::Rejected("server cursor moved backwards".into()));
            }
            repository.sync_forget_server_state()?;
            forgot_server_state = true;
            report.server_restored = true;
            continue;
        }
        for change in &response.changes {
            let Some(resource) = downloadable(change) else {
                continue;
            };
            if repository.sync_has_blob(resource.sha256.as_str())? {
                continue;
            }
            match download(repository, transport, &resource) {
                Ok(()) => {}
                Err(TransportError::Retryable(_)) => {
                    report.retryable += 1;
                    return Ok(false);
                }
                Err(error) => return Err(fatal(error)),
            }
        }
        let applied = repository.sync_apply_page(&response.changes, response.next_cursor)?;
        if let Some(last) = response.changes.last() {
            repository.sync_raise_anchor(last.cursor, &last.op_id)?;
        }
        report.pulled += applied.applied;
        report.skipped += applied.skipped;
        report.conflicts += applied.conflicts;
        if !response.has_more {
            return Ok(true);
        }
    }
}

/// A server restored from an older backup reuses cursor numbers, so the
/// change at our highest known cursor must still be the same op.
fn server_still_has_anchor(
    repository: &LibraryRepository,
    transport: &dyn SyncTransport,
) -> Result<bool, TransportError> {
    let Some((cursor, op_id)) = repository
        .sync_anchor()
        .map_err(|error| TransportError::Permanent(error.to_string()))?
    else {
        return Ok(true);
    };
    let response = transport.pull(&PullRequest {
        protocol: PROTOCOL_VERSION,
        cursor: cursor.saturating_sub(1),
        limit: 1,
    })?;
    Ok(response
        .changes
        .first()
        .is_some_and(|change| change.cursor == cursor && change.op_id == op_id))
}

fn fatal(error: TransportError) -> SyncError {
    match error {
        TransportError::Unauthorized => SyncError::Unauthorized,
        TransportError::Permanent(reason) => SyncError::Rejected(reason),
        TransportError::Retryable(reason) => SyncError::Rejected(reason),
        TransportError::OffsetMismatch { .. } => {
            SyncError::Rejected("unexpected upload response".into())
        }
    }
}

/// Uploads until nothing is left, nothing more is accepted, or the server
/// becomes unreachable.
fn push(
    repository: &LibraryRepository,
    transport: &dyn SyncTransport,
    device_id: &str,
    report: &mut SyncReport,
) -> Result<(), SyncError> {
    loop {
        let (accepted, retryable) = (report.accepted, report.retryable);
        if !push_round(repository, transport, device_id, report)? {
            return Ok(());
        }
        if report.accepted == accepted || report.retryable > retryable {
            return Ok(());
        }
    }
}

/// One prepared set of at most `MAX_PUSH_OPS` ops; false when it was empty.
fn push_round(
    repository: &LibraryRepository,
    transport: &dyn SyncTransport,
    device_id: &str,
    report: &mut SyncReport,
) -> Result<bool, SyncError> {
    let device_id = device_id.to_owned();
    let inflight = repository.sync_prepare_inflight(MAX_PUSH_OPS)?;
    if inflight.is_empty() {
        return Ok(false);
    }
    for batch in batches(&inflight) {
        // An op referencing an incomplete blob is never published; later
        // batches may hold notes that use it, so stop this pass entirely.
        for op in batch {
            match upload_blob(repository, transport, op) {
                Ok(()) => {}
                Err(TransportError::Retryable(_)) => {
                    report.retryable += 1;
                    return Ok(true);
                }
                Err(error) => return Err(fatal(error)),
            }
        }
        let ops = batch
            .iter()
            .map(|op| operation(op, &device_id))
            .collect::<Result<Vec<_>, _>>()?;
        let request = PushRequest {
            protocol: PROTOCOL_VERSION,
            device_id: device_id.clone(),
            ops,
        };
        let response = match transport.push(&request) {
            Ok(response) => response,
            Err(TransportError::Retryable(_)) => {
                report.retryable += batch.len();
                continue;
            }
            Err(error) => return Err(fatal(error)),
        };
        if response.results.len() != batch.len() {
            return Err(SyncError::Rejected("push result count differs".into()));
        }
        for result in response.results {
            match result {
                OpResult::Accepted {
                    op_id,
                    revision,
                    cursor,
                } => {
                    repository.sync_record_accepted(&op_id, revision)?;
                    repository.sync_raise_anchor(cursor, &op_id)?;
                    report.accepted += 1;
                }
                // The next download resolves it (conflict copy, remote wins).
                OpResult::Conflict { .. } => report.conflicts += 1,
                OpResult::Retryable { .. } => report.retryable += 1,
                OpResult::Permanent { op_id, reason } => {
                    repository.sync_record_permanent(&op_id, &reason)?;
                    report.permanent += 1;
                }
            }
        }
    }
    Ok(true)
}

pub fn sync_failures(repository: &LibraryRepository) -> Result<Vec<SyncFailure>, LibraryError> {
    repository.sync_failures()
}

/// Person-triggered: the next sync sends the entity's current state once.
pub fn retry_failure(repository: &LibraryRepository, op_id: &str) -> Result<bool, LibraryError> {
    repository.sync_retry_failure(op_id)
}

fn batches(ops: &[crate::SyncInflight]) -> Vec<&[crate::SyncInflight]> {
    let mut batches = Vec::new();
    let mut start = 0;
    let mut bytes = 0;
    for (index, op) in ops.iter().enumerate() {
        if index > start && bytes + op.action_json.len() > MAX_PUSH_BATCH_BYTES {
            batches.push(&ops[start..index]);
            start = index;
            bytes = 0;
        }
        bytes += op.action_json.len();
    }
    if start < ops.len() {
        batches.push(&ops[start..]);
    }
    batches
}

fn operation(op: &crate::SyncInflight, device_id: &str) -> Result<Operation, LibraryError> {
    let kind = EntityKind::parse(&op.entity_type).ok_or(LibraryError::InvalidSnapshot)?;
    let action: Action =
        serde_json::from_str(&op.action_json).map_err(|_| LibraryError::InvalidSnapshot)?;
    Ok(Operation {
        op_id: op.op_id.clone(),
        device_id: device_id.to_owned(),
        entity: EntityRef {
            kind,
            id: op.entity_id.clone(),
        },
        base_revision: op.base_revision.max(0) as u64,
        action,
    })
}

fn downloadable(
    change: &app_lite_protocol::Change,
) -> Option<crate::repository::RemoteResourceRef> {
    if change.entity.kind != EntityKind::Resource || change.deleted {
        return None;
    }
    crate::repository::RemoteResourceRef::parse(change.payload.as_ref()?)
}

/// Streams a blob from the server into the local store, verifying its hash.
fn download(
    repository: &LibraryRepository,
    transport: &dyn SyncTransport,
    resource: &crate::repository::RemoteResourceRef,
) -> Result<(), TransportError> {
    let mut reader = RangeReader {
        transport,
        sha256: resource.sha256.as_str(),
        size: resource.size as u64,
        offset: 0,
        buffer: Vec::new(),
        position: 0,
        failure: None,
    };
    let stored = repository.sync_store_blob(
        &mut reader,
        resource.size,
        &resource.title,
        &resource.mime,
        &resource.file_extension,
    );
    if let Some(failure) = reader.failure {
        return Err(failure);
    }
    match stored {
        Ok(sha) if sha == resource.sha256 => Ok(()),
        Ok(_) => Err(TransportError::Permanent(
            "downloaded attachment does not match its hash".into(),
        )),
        Err(error) => Err(TransportError::Retryable(error.to_string())),
    }
}

struct RangeReader<'a> {
    transport: &'a dyn SyncTransport,
    sha256: &'a str,
    size: u64,
    offset: u64,
    buffer: Vec<u8>,
    position: usize,
    failure: Option<TransportError>,
}

impl Read for RangeReader<'_> {
    fn read(&mut self, out: &mut [u8]) -> io::Result<usize> {
        if self.position == self.buffer.len() {
            if self.offset >= self.size {
                return Ok(0);
            }
            let len = BLOB_CHUNK_BYTES.min(self.size - self.offset);
            match self.transport.read_range(self.sha256, self.offset, len) {
                Ok(bytes) if !bytes.is_empty() && bytes.len() as u64 <= len => {
                    self.offset += bytes.len() as u64;
                    self.buffer = bytes;
                    self.position = 0;
                }
                Ok(_) => {
                    self.failure = Some(TransportError::Retryable("short attachment read".into()));
                    return Err(io::Error::other("short attachment read"));
                }
                Err(error) => {
                    self.failure = Some(error);
                    return Err(io::Error::other("attachment download failed"));
                }
            }
        }
        let count = out.len().min(self.buffer.len() - self.position);
        out[..count].copy_from_slice(&self.buffer[self.position..self.position + count]);
        self.position += count;
        Ok(count)
    }
}

/// Makes sure the server holds the whole blob of an attachment op,
/// resuming from what it already received.
fn upload_blob(
    repository: &LibraryRepository,
    transport: &dyn SyncTransport,
    op: &crate::SyncInflight,
) -> Result<(), TransportError> {
    if op.entity_type != "resource" {
        return Ok(());
    }
    let Ok(Action::Put { payload }) = serde_json::from_str::<Action>(&op.action_json) else {
        return Ok(());
    };
    let Some(resource) = crate::repository::RemoteResourceRef::parse(&payload) else {
        return Err(TransportError::Permanent(
            "attachment payload is incomplete".into(),
        ));
    };
    let size = resource.size as u64;
    let mut offset = match transport.blob_status(resource.sha256.as_str())? {
        BlobStatus::Complete => return Ok(()),
        BlobStatus::Partial { bytes } => bytes,
        BlobStatus::Missing => 0,
    };
    let id = crate::ResourceId::new(&op.entity_id)
        .map_err(|_| TransportError::Permanent("invalid attachment id".into()))?;
    let (_, mut file) = repository
        .open_verified_resource_file(&id)
        .map_err(|error| TransportError::Retryable(error.to_string()))?
        .ok_or_else(|| TransportError::Permanent("attachment is no longer stored here".into()))?;
    let mut chunk = vec![0; BLOB_CHUNK_BYTES as usize];
    while offset < size {
        let len = BLOB_CHUNK_BYTES.min(size - offset) as usize;
        file.seek(SeekFrom::Start(offset))
            .and_then(|_| file.read_exact(&mut chunk[..len]))
            .map_err(|error| TransportError::Retryable(error.to_string()))?;
        match transport.put_chunk(resource.sha256.as_str(), size, offset, &chunk[..len]) {
            Ok(BlobStatus::Complete) => return Ok(()),
            Ok(BlobStatus::Partial { bytes }) => offset = bytes,
            Ok(BlobStatus::Missing) => offset = 0,
            Err(TransportError::OffsetMismatch { expected }) => offset = expected,
            Err(error) => return Err(error),
        }
    }
    match transport.blob_status(resource.sha256.as_str())? {
        BlobStatus::Complete => Ok(()),
        _ => Err(TransportError::Retryable(
            "attachment upload incomplete".into(),
        )),
    }
}
