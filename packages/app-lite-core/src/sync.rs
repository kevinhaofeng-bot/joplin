//! Client sync engine (docs/research/sync-client-design-v1.md §2). Runs on a
//! background thread against any `SyncTransport`; only per-operation results
//! change local state. Mechanism after Evernote's `MutationUpsyncActivity`
//! (persistent queue, batches, per-item outcome); the protocol and conflict
//! policy are this project's own design.

use std::io::{Read, Seek, SeekFrom};
use std::sync::atomic::{AtomicU64, Ordering};

use app_lite_protocol::{
    Action, BlobStatus, EntityKind, EntityRef, MAX_PULL_LIMIT, MAX_PUSH_OPS, OpResult, Operation,
    PROTOCOL_VERSION, PullRequest, PushRequest, SyncTransport, TransportError,
};

use crate::{LibraryError, LibraryRepository, SyncFailure};

/// Largest push body and attachment range per request. One request must
/// finish within the transport's 120 s timeout on a weak link (1 MiB at
/// ~20 KB/s is ~50 s).
const MAX_PUSH_BATCH_BYTES: usize = 1024 * 1024;
const MAX_BLOB_CHUNK_BYTES: u64 = 1024 * 1024;
const MIN_PUSH_BATCH_BYTES: usize = 32 * 1024;
const MIN_BLOB_CHUNK_BYTES: u64 = 64 * 1024;

/// One request size that follows the link, like TCP's congestion window:
/// a request lost in transit halves it and sets the ceiling there; a
/// delivered one doubles it back up to the ceiling, then raises the
/// ceiling by an eighth. A link that drops sooner than a full-size request
/// takes (a phone moving between cells) otherwise resends the same request
/// forever; without the ceiling the size oscillates between one that fits
/// and one that does not.
#[derive(Debug)]
struct Adaptive {
    current: AtomicU64,
    ceiling: AtomicU64,
    min: u64,
    max: u64,
}

impl Adaptive {
    const fn new(min: u64, max: u64) -> Self {
        Self {
            current: AtomicU64::new(max),
            ceiling: AtomicU64::new(max),
            min,
            max,
        }
    }
    fn get(&self) -> u64 {
        self.current.load(Ordering::Relaxed)
    }
    fn lost(&self) {
        let next = (self.get() / 2).max(self.min);
        self.ceiling.store(next, Ordering::Relaxed);
        self.current.store(next, Ordering::Relaxed);
    }
    fn delivered(&self) {
        let ceiling = self.ceiling.load(Ordering::Relaxed);
        let current = self.get();
        if current < ceiling {
            self.current
                .store((current * 2).min(ceiling), Ordering::Relaxed);
        } else {
            let raised = (ceiling + ceiling / 8).clamp(self.min, self.max);
            self.ceiling.store(raised, Ordering::Relaxed);
            self.current.store(raised, Ordering::Relaxed);
        }
    }
}

/// Request sizes per library, kept for the life of the process.
#[derive(Debug)]
pub(crate) struct TransferBudget {
    push_bytes: Adaptive,
    chunk_bytes: Adaptive,
    /// Changes per pull page; one page of large notes can itself outlast
    /// the link.
    pull_changes: Adaptive,
}

impl Default for TransferBudget {
    fn default() -> Self {
        Self {
            push_bytes: Adaptive::new(MIN_PUSH_BATCH_BYTES as u64, MAX_PUSH_BATCH_BYTES as u64),
            chunk_bytes: Adaptive::new(MIN_BLOB_CHUNK_BYTES, MAX_BLOB_CHUNK_BYTES),
            pull_changes: Adaptive::new(1, MAX_PULL_LIMIT as u64),
        }
    }
}

impl TransferBudget {
    fn push_bytes(&self) -> usize {
        self.push_bytes.get() as usize
    }
    fn chunk_bytes(&self) -> u64 {
        self.chunk_bytes.get()
    }
    fn pull_changes(&self) -> usize {
        self.pull_changes.get() as usize
    }
    fn push_lost(&self) {
        self.push_bytes.lost();
    }
    fn push_delivered(&self) {
        self.push_bytes.delivered();
    }
    fn chunk_lost(&self) {
        self.chunk_bytes.lost();
    }
    fn chunk_delivered(&self) {
        self.chunk_bytes.delivered();
    }
    fn pull_lost(&self) {
        self.pull_changes.lost();
    }
    fn pull_delivered(&self) {
        self.pull_changes.delivered();
    }
}

#[cfg(test)]
mod tests {
    use super::Adaptive;

    #[test]
    fn a_lost_request_sets_a_ceiling_that_growth_then_approaches_slowly() {
        let size = Adaptive::new(64, 1024);
        size.lost();
        assert_eq!(size.get(), 512);
        size.delivered();
        assert_eq!(size.get(), 576, "above the ceiling only by an eighth");
        size.lost();
        size.lost();
        assert_eq!(size.get(), 144);
        size.delivered();
        assert_eq!(size.get(), 162);
        for _ in 0..100 {
            size.delivered();
        }
        assert_eq!(size.get(), 1024, "bounded by the maximum");
        for _ in 0..20 {
            size.lost();
        }
        assert_eq!(size.get(), 64, "bounded by the minimum");
    }
}

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
    let (conflicts, accepted) = (report.conflicts, report.accepted);
    push(repository, transport, &device_id, &mut report)?;
    if report.conflicts > conflicts {
        if pull(repository, transport, &mut report)? {
            push(repository, transport, &device_id, &mut report)?;
        }
    } else if report.accepted > accepted {
        // Moves the cursor past our own uploads (skipped as already known),
        // so the server's change notification for them reads as seen.
        pull(repository, transport, &mut report)?;
    }
    if report.retryable == 0 {
        repository.sync_record_success()?;
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
            limit: repository.sync_budget().pull_changes(),
        };
        let response = match transport.pull(&request) {
            Ok(response) => {
                repository.sync_budget().pull_delivered();
                response
            }
            Err(TransportError::Retryable(_)) => {
                repository.sync_budget().pull_lost();
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
    let budget = repository.sync_budget();
    for batch in batches(&inflight, budget.push_bytes()) {
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
            Ok(response) => {
                budget.push_delivered();
                response
            }
            // Later batches may hold notes that reference what this one
            // carried (an attachment, a notebook): sending them first would
            // publish references the server cannot resolve yet.
            Err(TransportError::Retryable(_)) => {
                budget.push_lost();
                report.retryable += batch.len();
                return Ok(true);
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

fn batches(ops: &[crate::SyncInflight], limit: usize) -> Vec<&[crate::SyncInflight]> {
    let mut batches = Vec::new();
    let mut start = 0;
    let mut bytes = 0;
    for (index, op) in ops.iter().enumerate() {
        if index > start && bytes + op.action_json.len() > limit {
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
/// Resumes after the bytes an earlier interrupted download kept, fetching
/// ranges sized by the transfer budget; the finished part is published only
/// if its hash matches.
fn download(
    repository: &LibraryRepository,
    transport: &dyn SyncTransport,
    resource: &crate::repository::RemoteResourceRef,
) -> Result<(), TransportError> {
    let retryable = |error: LibraryError| TransportError::Retryable(error.to_string());
    let budget = repository.sync_budget();
    let size = resource.size as u64;
    let mut offset = repository
        .sync_download_part_len(&resource.sha256)
        .map_err(retryable)?
        .min(size);
    while offset < size {
        let len = budget.chunk_bytes().min(size - offset);
        match transport.read_range(resource.sha256.as_str(), offset, len) {
            Ok(bytes) if !bytes.is_empty() && bytes.len() as u64 <= len => {
                repository
                    .sync_append_download(&resource.sha256, offset, &bytes)
                    .map_err(retryable)?;
                offset += bytes.len() as u64;
                budget.chunk_delivered();
            }
            Ok(_) => return Err(TransportError::Retryable("short attachment read".into())),
            Err(TransportError::Retryable(reason)) => {
                budget.chunk_lost();
                return Err(TransportError::Retryable(reason));
            }
            Err(error) => return Err(error),
        }
    }
    match repository.sync_publish_download(resource) {
        Ok(sha) if sha == resource.sha256 => Ok(()),
        Ok(_) => Err(TransportError::Permanent(
            "downloaded attachment does not match its hash".into(),
        )),
        Err(error) => Err(retryable(error)),
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
    let budget = repository.sync_budget();
    let mut chunk = vec![0; MAX_BLOB_CHUNK_BYTES as usize];
    while offset < size {
        let len = budget.chunk_bytes().min(size - offset) as usize;
        file.seek(SeekFrom::Start(offset))
            .and_then(|_| file.read_exact(&mut chunk[..len]))
            .map_err(|error| TransportError::Retryable(error.to_string()))?;
        match transport.put_chunk(resource.sha256.as_str(), size, offset, &chunk[..len]) {
            Ok(status) => {
                budget.chunk_delivered();
                match status {
                    BlobStatus::Complete => return Ok(()),
                    BlobStatus::Partial { bytes } => offset = bytes,
                    BlobStatus::Missing => offset = 0,
                }
            }
            Err(TransportError::OffsetMismatch { expected }) => offset = expected,
            Err(TransportError::Retryable(reason)) => {
                // The server keeps what arrived; the next attempt resumes there.
                budget.chunk_lost();
                return Err(TransportError::Retryable(reason));
            }
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
