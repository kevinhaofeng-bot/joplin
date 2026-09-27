//! Uploads the server rejected for good: parked and listed, never resent on
//! their own, retried from current local state only when the person asks.

use std::cell::Cell;

use app_lite_core::document::{Block, BlockStyle, Inline};
use app_lite_core::{CanonicalDocument, CreateNote, LibraryRepository, NoteId, SaveNote, sync};
use app_lite_protocol::{
    BlobStatus, MAX_PAYLOAD_BYTES, PullRequest, PullResponse, PushRequest, PushResponse,
    SyncTransport, TransportError,
};
use app_lite_server::ServerStore;
use tempfile::{TempDir, tempdir};

fn text(value: &str) -> CanonicalDocument {
    CanonicalDocument::from_blocks(vec![Block::Paragraph {
        style: BlockStyle::default(),
        inlines: vec![Inline::Text {
            text: value.into(),
            marks: Default::default(),
        }],
    }])
}

/// Counts every op sent for `watched`.
struct Counting<'a> {
    inner: &'a ServerStore,
    watched: String,
    sent: Cell<usize>,
}

impl SyncTransport for Counting<'_> {
    fn push(&self, request: &PushRequest) -> Result<PushResponse, TransportError> {
        let watched = request
            .ops
            .iter()
            .filter(|op| op.entity.id == self.watched)
            .count();
        self.sent.set(self.sent.get() + watched);
        SyncTransport::push(self.inner, request)
    }
    fn pull(&self, request: &PullRequest) -> Result<PullResponse, TransportError> {
        SyncTransport::pull(self.inner, request)
    }
    fn blob_status(&self, sha256: &str) -> Result<BlobStatus, TransportError> {
        SyncTransport::blob_status(self.inner, sha256)
    }
    fn put_chunk(
        &self,
        sha256: &str,
        size: u64,
        offset: u64,
        bytes: &[u8],
    ) -> Result<BlobStatus, TransportError> {
        SyncTransport::put_chunk(self.inner, sha256, size, offset, bytes)
    }
    fn read_range(&self, sha256: &str, offset: u64, len: u64) -> Result<Vec<u8>, TransportError> {
        SyncTransport::read_range(self.inner, sha256, offset, len)
    }
}

struct Setup {
    _roots: (TempDir, TempDir),
    repo: LibraryRepository,
    store: ServerStore,
    note: NoteId,
}

/// A note too large for the server's payload limit.
fn oversized_note() -> Setup {
    let server_root = tempdir().unwrap();
    let store = ServerStore::open(server_root.path()).unwrap();
    let root = tempdir().unwrap();
    let repo = LibraryRepository::open(root.path().join("library.sqlite")).unwrap();
    let note = repo
        .create_note(CreateNote {
            title: "超大笔记".into(),
            notebook_id: None,
            document: text(&"字".repeat(MAX_PAYLOAD_BYTES / 3 + 1)),
        })
        .unwrap()
        .id;
    Setup {
        _roots: (server_root, root),
        repo,
        store,
        note,
    }
}

fn shorten(repo: &LibraryRepository, id: &NoteId) {
    let note = repo.load_note(id).unwrap().unwrap();
    repo.save_note(SaveNote {
        id: id.clone(),
        expected_revision: note.revision,
        title: note.title,
        document: text("改短了"),
        resource_ids: vec![],
        selected_thumbnail_id: None,
    })
    .unwrap();
}

#[test]
fn a_rejected_upload_is_listed_parked_and_uploads_current_content_after_a_retry() {
    let setup = oversized_note();
    let transport = Counting {
        inner: &setup.store,
        watched: setup.note.as_str().to_owned(),
        sent: Cell::new(0),
    };
    let first = sync::sync_once(&setup.repo, &transport).unwrap();
    assert_eq!(first.permanent, 1);
    assert_eq!(transport.sent.get(), 1);
    let failures = sync::sync_failures(&setup.repo).unwrap();
    assert_eq!(failures.len(), 1);
    assert_eq!(failures[0].entity_id, setup.note.as_str());
    assert_eq!(failures[0].title.as_deref(), Some("超大笔记"));
    assert!(failures[0].can_retry);
    assert!(
        failures[0].reason.contains("too large"),
        "{:?}",
        failures[0]
    );

    // Neither another sync nor a local edit resends it on its own; other
    // work still uploads.
    let other = setup
        .repo
        .create_note(CreateNote {
            title: "别的".into(),
            notebook_id: None,
            document: text("x"),
        })
        .unwrap();
    shorten(&setup.repo, &setup.note);
    let second = sync::sync_once(&setup.repo, &transport).unwrap();
    assert_eq!(transport.sent.get(), 1, "a parked op is not resent");
    assert_eq!((second.accepted, second.permanent), (1, 0));
    assert!(
        setup.repo.sync_pending_count().unwrap() > 0,
        "still local only"
    );
    assert_eq!(sync::sync_failures(&setup.repo).unwrap(), failures);
    assert!(setup.repo.outbox_count().unwrap() > 0);

    assert!(sync::retry_failure(&setup.repo, &failures[0].op_id).unwrap());
    let third = sync::sync_once(&setup.repo, &transport).unwrap();
    assert_eq!((third.accepted, third.permanent), (1, 0));
    assert!(sync::sync_failures(&setup.repo).unwrap().is_empty());
    assert_eq!(setup.repo.sync_pending_count().unwrap(), 0);

    let root = tempdir().unwrap();
    let b = LibraryRepository::open(root.path().join("library.sqlite")).unwrap();
    sync::sync_once(&b, &setup.store).unwrap();
    let received = b.load_note(&setup.note).unwrap().unwrap();
    assert!(received.body_html.contains("改短了"));
    assert!(b.load_note(&other.id).unwrap().is_some());
}

#[test]
fn a_retry_that_fails_again_is_listed_again_after_one_attempt() {
    let setup = oversized_note();
    let transport = Counting {
        inner: &setup.store,
        watched: setup.note.as_str().to_owned(),
        sent: Cell::new(0),
    };
    sync::sync_once(&setup.repo, &transport).unwrap();
    let before = sync::sync_failures(&setup.repo).unwrap();
    assert!(sync::retry_failure(&setup.repo, &before[0].op_id).unwrap());
    assert!(
        !sync::retry_failure(&setup.repo, &before[0].op_id).unwrap(),
        "an op already retried is gone"
    );
    let report = sync::sync_once(&setup.repo, &transport).unwrap();
    assert_eq!(report.permanent, 1);
    assert_eq!(transport.sent.get(), 2, "exactly one more attempt");
    let after = sync::sync_failures(&setup.repo).unwrap();
    assert_eq!(after.len(), 1);
    assert_ne!(after[0].op_id, before[0].op_id, "a fresh op, not a replay");
    sync::sync_once(&setup.repo, &transport).unwrap();
    assert_eq!(transport.sent.get(), 2);
}
