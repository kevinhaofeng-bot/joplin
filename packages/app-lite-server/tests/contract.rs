//! Sync protocol v1 storage contract (docs/research/sync-protocol-v1.md),
//! exercised in-process against the server store. No network.

use app_lite_protocol::{
    Action, BlobStatus, EntityKind, EntityRef, OpResult, Operation, PROTOCOL_VERSION, PullRequest,
    PushRequest,
};
use app_lite_server::ServerStore;
use serde_json::json;
use sha2::{Digest, Sha256};

const DEVICE_A: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
const DEVICE_B: &str = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
const NOTE: &str = "11111111111111111111111111111111";
const OTHER: &str = "22222222222222222222222222222222";

fn op(n: u8, device: &str, id: &str, base: u64, action: Action) -> Operation {
    Operation {
        op_id: format!("{n:032x}"),
        device_id: device.into(),
        entity: EntityRef {
            kind: EntityKind::Note,
            id: id.into(),
        },
        base_revision: base,
        action,
    }
}

fn put(title: &str) -> Action {
    Action::Put {
        payload: json!({ "title": title }),
    }
}

fn push(store: &ServerStore, device: &str, ops: Vec<Operation>) -> Vec<OpResult> {
    store
        .push(PushRequest {
            protocol: PROTOCOL_VERSION,
            device_id: device.into(),
            ops,
        })
        .unwrap()
        .results
}

fn pull_all(store: &ServerStore, cursor: u64) -> Vec<app_lite_protocol::Change> {
    store
        .pull(PullRequest {
            protocol: PROTOCOL_VERSION,
            cursor,
            limit: 500,
        })
        .unwrap()
        .changes
}

fn store() -> (tempfile::TempDir, ServerStore) {
    let dir = tempfile::tempdir().unwrap();
    let store = ServerStore::open(dir.path()).unwrap();
    (dir, store)
}

#[test]
fn a_retried_op_after_a_lost_response_applies_exactly_once() {
    let (_dir, store) = store();
    let create = op(1, DEVICE_A, NOTE, 0, put("一"));
    let first = push(&store, DEVICE_A, vec![create.clone()]);
    let retry = push(&store, DEVICE_A, vec![create]);
    assert!(matches!(first[0], OpResult::Accepted { revision: 1, .. }));
    assert_eq!(first, retry, "the recorded result is returned again");
    assert_eq!(pull_all(&store, 0).len(), 1, "applied once");
}

#[test]
fn reusing_an_op_id_for_different_content_is_permanent() {
    let (_dir, store) = store();
    push(&store, DEVICE_A, vec![op(1, DEVICE_A, NOTE, 0, put("一"))]);
    let reused = push(
        &store,
        DEVICE_A,
        vec![op(1, DEVICE_A, NOTE, 0, put("不同"))],
    );
    assert!(
        matches!(reused[0], OpResult::Permanent { .. }),
        "{reused:?}"
    );
}

#[test]
fn unrelated_edits_from_two_offline_devices_converge_through_pull() {
    let (_dir, store) = store();
    push(&store, DEVICE_A, vec![op(1, DEVICE_A, NOTE, 0, put("甲"))]);
    push(&store, DEVICE_B, vec![op(2, DEVICE_B, OTHER, 0, put("乙"))]);
    let changes = pull_all(&store, 0);
    let ids: Vec<_> = changes.iter().map(|c| c.entity.id.as_str()).collect();
    assert_eq!(ids, vec![NOTE, OTHER]);
    assert!(changes.windows(2).all(|w| w[0].cursor < w[1].cursor));
}

#[test]
fn concurrent_edit_of_the_same_note_is_a_visible_conflict_not_an_overwrite() {
    let (_dir, store) = store();
    push(&store, DEVICE_A, vec![op(1, DEVICE_A, NOTE, 0, put("原"))]);
    let a = push(
        &store,
        DEVICE_A,
        vec![op(2, DEVICE_A, NOTE, 1, put("甲改"))],
    );
    let b = push(
        &store,
        DEVICE_B,
        vec![op(3, DEVICE_B, NOTE, 1, put("乙改"))],
    );
    assert!(matches!(a[0], OpResult::Accepted { revision: 2, .. }));
    match &b[0] {
        OpResult::Conflict {
            server_revision,
            server_payload,
            server_deleted,
            ..
        } => {
            assert_eq!(*server_revision, 2);
            assert!(!server_deleted);
            assert_eq!(server_payload.as_ref().unwrap()["title"], "甲改");
        }
        other => panic!("{other:?}"),
    }
    // The conflict is recorded too: retrying B's op yields the same answer.
    assert_eq!(
        push(
            &store,
            DEVICE_B,
            vec![op(3, DEVICE_B, NOTE, 1, put("乙改"))]
        ),
        b
    );
}

#[test]
fn delete_versus_edit_never_silently_loses_the_edit() {
    let (_dir, store) = store();
    push(&store, DEVICE_A, vec![op(1, DEVICE_A, NOTE, 0, put("原"))]);
    let delete = push(
        &store,
        DEVICE_A,
        vec![op(2, DEVICE_A, NOTE, 1, Action::Delete)],
    );
    let edit = push(
        &store,
        DEVICE_B,
        vec![op(3, DEVICE_B, NOTE, 1, put("乙改"))],
    );
    assert!(matches!(delete[0], OpResult::Accepted { revision: 2, .. }));
    assert!(
        matches!(
            edit[0],
            OpResult::Conflict {
                server_deleted: true,
                ..
            }
        ),
        "{edit:?}"
    );
}

#[test]
fn pull_pages_by_monotonic_cursor_and_survives_reopen() {
    let dir = tempfile::tempdir().unwrap();
    {
        let store = ServerStore::open(dir.path()).unwrap();
        let ops = (1..=5)
            .map(|n| op(n, DEVICE_A, &format!("{n:032x}"), 0, put("x")))
            .collect();
        push(&store, DEVICE_A, ops);
    }
    let store = ServerStore::open(dir.path()).unwrap();
    let page = store
        .pull(PullRequest {
            protocol: PROTOCOL_VERSION,
            cursor: 0,
            limit: 2,
        })
        .unwrap();
    assert_eq!(page.changes.len(), 2);
    assert!(page.has_more);
    let rest = pull_all(&store, page.next_cursor);
    assert_eq!(rest.len(), 3);
    assert!(rest[0].cursor > page.next_cursor);
}

#[test]
fn invalid_ids_or_non_object_payloads_are_permanent_errors() {
    let (_dir, store) = store();
    let mut bad_id = op(1, DEVICE_A, "not-hex", 0, put("x"));
    bad_id.entity.id = "not-hex".into();
    let bad_payload = op(
        2,
        DEVICE_A,
        NOTE,
        0,
        Action::Put {
            payload: json!("string"),
        },
    );
    let results = push(&store, DEVICE_A, vec![bad_id, bad_payload]);
    assert!(
        results
            .iter()
            .all(|r| matches!(r, OpResult::Permanent { .. }))
    );
    assert!(pull_all(&store, 0).is_empty());
}

#[test]
fn interrupted_blob_upload_resumes_and_publishes_only_a_verified_whole() {
    let (_dir, store) = store();
    let bytes: Vec<u8> = (0..100_000u32).map(|n| (n % 251) as u8).collect();
    let sha = format!("{:x}", Sha256::digest(&bytes));
    let size = bytes.len() as u64;
    assert_eq!(store.blob_status(&sha).unwrap(), BlobStatus::Missing);
    store.put_chunk(&sha, size, 0, &bytes[..40_000]).unwrap();
    assert_eq!(
        store.blob_status(&sha).unwrap(),
        BlobStatus::Partial { bytes: 40_000 }
    );
    assert!(
        store.read_range(&sha, 0, 10).is_err(),
        "partial is invisible"
    );
    // A client resuming at the wrong offset is rejected.
    assert!(store.put_chunk(&sha, size, 10, &bytes[10..20]).is_err());
    store
        .put_chunk(&sha, size, 40_000, &bytes[40_000..])
        .unwrap();
    assert_eq!(store.blob_status(&sha).unwrap(), BlobStatus::Complete);
    assert_eq!(store.read_range(&sha, 99_990, 10).unwrap(), bytes[99_990..]);
    // Re-uploading a complete blob is a no-op success, not a duplicate.
    store.put_chunk(&sha, size, 0, &bytes).unwrap();

    let wrong = format!("{:x}", Sha256::digest(b"something else"));
    assert!(store.put_chunk(&wrong, 4, 0, b"abcd").is_err());
    assert_eq!(store.blob_status(&wrong).unwrap(), BlobStatus::Missing);
}
