//! Client sync engine, upload half (docs/research/sync-client-design-v1.md §2):
//! outbox rollup, persisted in-flight operations and exactly-once retry.

use std::cell::Cell;

use app_lite_core::document::{Block, BlockStyle, Inline};
use app_lite_core::{CanonicalDocument, CreateNote, LibraryRepository, SaveNote, sync};
use app_lite_protocol::{
    BlobStatus, PROTOCOL_VERSION, PullRequest, PullResponse, PushRequest, PushResponse,
    SyncTransport, TransportError,
};
use app_lite_server::ServerStore;
use tempfile::tempdir;

fn text(value: &str) -> CanonicalDocument {
    CanonicalDocument::from_blocks(vec![Block::Paragraph {
        style: BlockStyle::default(),
        inlines: vec![Inline::Text {
            text: value.into(),
            marks: Default::default(),
        }],
    }])
}

fn server_entities(store: &ServerStore) -> Vec<(String, u64, Option<serde_json::Value>)> {
    let pulled = store
        .pull(PullRequest {
            protocol: PROTOCOL_VERSION,
            cursor: 0,
            limit: 500,
        })
        .unwrap();
    pulled
        .changes
        .into_iter()
        .map(|change| {
            (
                format!("{}:{}", change.entity.kind.as_str(), change.entity.id),
                change.revision,
                change.payload,
            )
        })
        .collect()
}

/// Applies the push on the server, then "loses" the response.
struct LosesPushResponses<'a> {
    inner: &'a ServerStore,
    remaining: Cell<usize>,
}

impl SyncTransport for LosesPushResponses<'_> {
    fn push(&self, request: &PushRequest) -> Result<PushResponse, TransportError> {
        let response = SyncTransport::push(self.inner, request)?;
        if self.remaining.get() > 0 {
            self.remaining.set(self.remaining.get() - 1);
            return Err(TransportError::Retryable("connection reset".into()));
        }
        Ok(response)
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

fn open(root: &std::path::Path) -> LibraryRepository {
    LibraryRepository::open(root.join("library.sqlite")).unwrap()
}

#[test]
fn device_id_is_stable_across_reopen() {
    let root = tempdir().unwrap();
    let first = open(root.path()).sync_device_id().unwrap();
    assert_eq!(first.len(), 32);
    assert_eq!(open(root.path()).sync_device_id().unwrap(), first);
}

#[test]
fn several_local_edits_upload_as_one_latest_state_and_clear_the_outbox() {
    let root = tempdir().unwrap();
    let server_root = tempdir().unwrap();
    let store = ServerStore::open(server_root.path()).unwrap();
    let repo = open(root.path());
    let note = repo
        .create_note(CreateNote {
            title: "草稿".into(),
            notebook_id: None,
            document: text("一"),
        })
        .unwrap();
    let saved = repo
        .save_note(SaveNote {
            id: note.id.clone(),
            expected_revision: note.revision,
            title: "定稿".into(),
            document: text("二"),
            resource_ids: vec![],
            selected_thumbnail_id: None,
        })
        .unwrap();
    let report = sync::sync_once(&repo, &store).unwrap();
    assert_eq!(report.retryable, 0);
    assert_eq!(repo.outbox_count().unwrap(), 0);
    let entities = server_entities(&store);
    let note_entry = entities
        .iter()
        .find(|(key, ..)| *key == format!("note:{}", note.id.as_str()))
        .expect("note uploaded");
    assert_eq!(
        note_entry.1, 1,
        "two local saves become one server revision"
    );
    let payload = note_entry.2.as_ref().unwrap();
    assert_eq!(payload["title"], "定稿");
    assert_eq!(payload["body_html"], saved.body_html);
    assert!(
        entities
            .iter()
            .any(|(key, ..)| key.starts_with("notebook:")),
        "the note's notebook is uploaded too"
    );
    // Nothing left to send: a second sync is a no-op on the server.
    sync::sync_once(&repo, &store).unwrap();
    assert_eq!(server_entities(&store).len(), entities.len());
}

#[test]
fn a_lost_push_response_is_resent_unchanged_and_applies_once_even_after_a_new_edit() {
    let root = tempdir().unwrap();
    let server_root = tempdir().unwrap();
    let store = ServerStore::open(server_root.path()).unwrap();
    let repo = open(root.path());
    let note = repo
        .create_note(CreateNote {
            title: "一版".into(),
            notebook_id: None,
            document: text("一"),
        })
        .unwrap();
    let lossy = LosesPushResponses {
        inner: &store,
        remaining: Cell::new(1),
    };
    let report = sync::sync_once(&repo, &lossy).unwrap();
    assert!(
        report.retryable > 0,
        "a lost response is reported as retryable"
    );
    assert!(
        repo.outbox_count().unwrap() > 0,
        "unacknowledged work stays queued"
    );
    // Edit while the first upload's outcome is unknown.
    repo.save_note(SaveNote {
        id: note.id.clone(),
        expected_revision: note.revision,
        title: "二版".into(),
        document: text("二"),
        resource_ids: vec![],
        selected_thumbnail_id: None,
    })
    .unwrap();
    // First sync resends the in-flight op (server replays its result), the
    // next one uploads the new edit on top of it.
    sync::sync_once(&repo, &store).unwrap();
    sync::sync_once(&repo, &store).unwrap();
    assert_eq!(repo.outbox_count().unwrap(), 0);
    let note_entry = server_entities(&store)
        .into_iter()
        .filter(|(key, ..)| *key == format!("note:{}", note.id.as_str()))
        .last()
        .unwrap();
    assert_eq!(
        note_entry.1, 2,
        "one revision per distinct upload, no duplicate"
    );
    assert_eq!(note_entry.2.unwrap()["title"], "二版");
    assert!(
        sync::sync_failures(&repo).unwrap().is_empty(),
        "no op_id reuse error"
    );
}

#[test]
fn a_note_created_and_purged_before_any_sync_sends_nothing() {
    let root = tempdir().unwrap();
    let server_root = tempdir().unwrap();
    let store = ServerStore::open(server_root.path()).unwrap();
    let repo = open(root.path());
    let note = repo
        .create_note(CreateNote {
            title: "临时".into(),
            notebook_id: None,
            document: text("x"),
        })
        .unwrap();
    repo.trash_note(&note.id).unwrap();
    repo.purge_note(&note.id).unwrap();
    sync::sync_once(&repo, &store).unwrap();
    assert!(
        server_entities(&store)
            .iter()
            .all(|(key, ..)| !key.starts_with("note:")),
        "a note the server never saw is not announced"
    );
    assert_eq!(repo.outbox_count().unwrap(), 0);
}

#[test]
fn one_sync_uploads_more_than_one_batch() {
    let root = tempdir().unwrap();
    let server_root = tempdir().unwrap();
    let store = ServerStore::open(server_root.path()).unwrap();
    let repo = open(root.path());
    for index in 0..250 {
        repo.create_note(CreateNote {
            title: format!("第{index}篇"),
            notebook_id: None,
            document: text("x"),
        })
        .unwrap();
    }
    let report = sync::sync_once(&repo, &store).unwrap();
    assert_eq!(repo.sync_pending_count().unwrap(), 0, "{report:?}");
    assert_eq!(repo.outbox_count().unwrap(), 0);
}
