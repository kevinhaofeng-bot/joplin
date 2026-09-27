//! Client sync engine (docs/research/sync-client-design-v1.md §2). Runs on a
//! background thread against any `SyncTransport`; only per-operation results
//! change local state. Mechanism after Evernote's `MutationUpsyncActivity`
//! (persistent queue, batches, per-item outcome); the protocol and conflict
//! policy are this project's own design.

use app_lite_protocol::{
    Action, EntityKind, EntityRef, MAX_PUSH_OPS, OpResult, Operation, PROTOCOL_VERSION,
    PushRequest, SyncTransport, TransportError,
};

use crate::{LibraryError, LibraryRepository, SyncFailure};

/// Stays under the protocol's 4 MiB push body with room for the envelope.
const MAX_PUSH_BATCH_BYTES: usize = 3 * 1024 * 1024;

#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct SyncReport {
    pub accepted: usize,
    pub conflicts: usize,
    /// Operations still waiting because the server or network was unavailable.
    pub retryable: usize,
    pub permanent: usize,
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

/// One upload pass. Transport failures that may succeed later are counted as
/// `retryable` and leave every in-flight op to be resent unchanged.
pub fn sync_once(
    repository: &LibraryRepository,
    transport: &dyn SyncTransport,
) -> Result<SyncReport, SyncError> {
    let device_id = repository.sync_device_id()?;
    let mut report = SyncReport::default();
    let inflight = repository.sync_prepare_inflight(MAX_PUSH_OPS)?;
    for batch in batches(&inflight) {
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
            Err(TransportError::Unauthorized) => return Err(SyncError::Unauthorized),
            Err(TransportError::Permanent(reason)) => return Err(SyncError::Rejected(reason)),
            Err(TransportError::OffsetMismatch { .. }) => {
                return Err(SyncError::Rejected("unexpected upload response".into()));
            }
        };
        if response.results.len() != batch.len() {
            return Err(SyncError::Rejected("push result count differs".into()));
        }
        for result in response.results {
            match result {
                OpResult::Accepted {
                    op_id, revision, ..
                } => {
                    repository.sync_record_accepted(&op_id, revision)?;
                    report.accepted += 1;
                }
                OpResult::Conflict { op_id, .. } => {
                    // Resolution arrives with the download half; keep the op.
                    repository.sync_record_permanent(&op_id, "conflict pending resolution")?;
                    report.conflicts += 1;
                }
                OpResult::Retryable { .. } => report.retryable += 1,
                OpResult::Permanent { op_id, reason } => {
                    repository.sync_record_permanent(&op_id, &reason)?;
                    report.permanent += 1;
                }
            }
        }
    }
    Ok(report)
}

pub fn sync_failures(repository: &LibraryRepository) -> Result<Vec<SyncFailure>, LibraryError> {
    repository.sync_failures()
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
