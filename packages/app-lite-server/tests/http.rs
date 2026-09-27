//! Protocol v1 over real HTTP (docs/research/sync-client-design-v1.md §1):
//! the same store semantics as tests/contract.rs, reached through a bound
//! localhost socket, bearer auth and size limits.

use std::sync::Arc;

use app_lite_protocol::{
    Action, BlobStatus, EntityKind, EntityRef, OpResult, Operation, PROTOCOL_VERSION, PullRequest,
    PushRequest, SyncTransport, TransportError, client::HttpTransport,
};
use app_lite_server::{ServerStore, http::HttpServer};
use sha2::{Digest, Sha256};
use tempfile::tempdir;

const TOKEN: &str = "0123456789abcdef0123456789abcdef-test-token";
const DEVICE: &str = "dddddddddddddddddddddddddddddddd";

struct Running {
    _root: tempfile::TempDir,
    url: String,
    server: HttpServer,
}

fn start() -> Running {
    let root = tempdir().unwrap();
    let store = Arc::new(ServerStore::open(root.path()).unwrap());
    let server = HttpServer::bind("127.0.0.1:0", store, TOKEN.into()).unwrap();
    let url = format!("http://{}", server.local_addr());
    Running {
        _root: root,
        url,
        server,
    }
}

fn put(op_id: &str, id: &str, base_revision: u64, title: &str) -> Operation {
    Operation {
        op_id: op_id.into(),
        device_id: DEVICE.into(),
        entity: EntityRef {
            kind: EntityKind::Note,
            id: id.into(),
        },
        base_revision,
        action: Action::Put {
            payload: serde_json::json!({ "title": title }),
        },
    }
}

fn push(op: Operation) -> PushRequest {
    PushRequest {
        protocol: PROTOCOL_VERSION,
        device_id: DEVICE.into(),
        ops: vec![op],
    }
}

#[test]
fn requests_without_the_bearer_token_are_refused_without_side_effects() {
    let running = start();
    let op = put(&"1".repeat(32), &"a".repeat(32), 0, "甲");
    for token in ["", "wrong-token"] {
        let client = HttpTransport::new(&running.url, token);
        assert_eq!(
            client.push(&push(op.clone())),
            Err(TransportError::Unauthorized)
        );
    }
    let client = HttpTransport::new(&running.url, TOKEN);
    let pulled = client
        .pull(&PullRequest {
            protocol: PROTOCOL_VERSION,
            cursor: 0,
            limit: 10,
        })
        .unwrap();
    assert!(pulled.changes.is_empty(), "refused push must not apply");
    drop(running.server);
}

#[test]
fn push_is_idempotent_over_http_and_pull_returns_the_change() {
    let running = start();
    let client = HttpTransport::new(&running.url, TOKEN);
    let op = put(&"1".repeat(32), &"a".repeat(32), 0, "中文标题");
    let first = client.push(&push(op.clone())).unwrap();
    // The response was "lost": the same op is sent again and applies once.
    let second = client.push(&push(op)).unwrap();
    assert_eq!(first, second);
    assert!(matches!(
        first.results[0],
        OpResult::Accepted { revision: 1, .. }
    ));
    let pulled = client
        .pull(&PullRequest {
            protocol: PROTOCOL_VERSION,
            cursor: 0,
            limit: 10,
        })
        .unwrap();
    assert_eq!(pulled.changes.len(), 1);
    assert_eq!(
        pulled.changes[0].payload,
        Some(serde_json::json!({ "title": "中文标题" }))
    );
}

#[test]
fn interrupted_upload_resumes_over_http_and_reads_back_identical_bytes() {
    let running = start();
    let client = HttpTransport::new(&running.url, TOKEN);
    let bytes: Vec<u8> = (0..300_000u32).map(|i| (i % 251) as u8).collect();
    let sha = format!("{:x}", Sha256::digest(&bytes));
    let size = bytes.len() as u64;
    assert_eq!(client.blob_status(&sha), Ok(BlobStatus::Missing));
    assert_eq!(
        client.put_chunk(&sha, size, 0, &bytes[..100_000]),
        Ok(BlobStatus::Partial { bytes: 100_000 })
    );
    // A client that lost track of progress sends the wrong offset and is told
    // where to resume, instead of corrupting the upload.
    assert_eq!(
        client.put_chunk(&sha, size, 0, &bytes[..100_000]),
        Err(TransportError::OffsetMismatch { expected: 100_000 })
    );
    assert_eq!(
        client.blob_status(&sha),
        Ok(BlobStatus::Partial { bytes: 100_000 })
    );
    assert_eq!(
        client.put_chunk(&sha, size, 100_000, &bytes[100_000..]),
        Ok(BlobStatus::Complete)
    );
    let mut read = Vec::new();
    while (read.len() as u64) < size {
        let chunk = client
            .read_range(&sha, read.len() as u64, 64 * 1024)
            .unwrap();
        assert!(!chunk.is_empty());
        read.extend(chunk);
    }
    assert_eq!(read, bytes);
}

#[test]
fn oversized_or_malformed_requests_are_permanent_errors() {
    let running = start();
    let client = HttpTransport::new(&running.url, TOKEN);
    let big = "x".repeat(9 * 1024 * 1024);
    let op = Operation {
        action: Action::Put {
            payload: serde_json::json!({ "title": big }),
        },
        ..put(&"2".repeat(32), &"b".repeat(32), 0, "")
    };
    assert!(matches!(
        client.push(&push(op)),
        Err(TransportError::Permanent(_))
    ));
    let sha = "0".repeat(64);
    assert!(matches!(
        client.put_chunk(&sha, 5 * 1024 * 1024, 0, &vec![0u8; 5 * 1024 * 1024]),
        Err(TransportError::Permanent(_))
    ));
    assert!(matches!(
        client.blob_status("not-a-hash"),
        Err(TransportError::Permanent(_))
    ));
}

#[test]
fn an_unreachable_server_is_retryable() {
    let running = start();
    let url = running.url.clone();
    drop(running);
    let client = HttpTransport::new(&url, TOKEN);
    assert!(matches!(
        client.blob_status(&"0".repeat(64)),
        Err(TransportError::Retryable(_))
    ));
}
