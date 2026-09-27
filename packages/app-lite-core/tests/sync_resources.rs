//! Attachments across devices (docs/research/sync-client-design-v1.md §2):
//! resumable blob upload, verified download, delete versus reference.

use std::cell::Cell;

use app_lite_core::document::{Block, BlockStyle, Inline};
use app_lite_core::{CanonicalDocument, CreateNote, LibraryRepository, ResourceId, sync};
use app_lite_protocol::{
    BlobStatus, PullRequest, PullResponse, PushRequest, PushResponse, SyncTransport, TransportError,
};
use app_lite_server::ServerStore;
use sha2::{Digest, Sha256};
use tempfile::{TempDir, tempdir};

fn with_image(prefix: &str, resource: ResourceId) -> CanonicalDocument {
    CanonicalDocument::from_blocks(vec![Block::Paragraph {
        style: BlockStyle::default(),
        inlines: vec![
            Inline::Text {
                text: prefix.into(),
                marks: Default::default(),
            },
            Inline::Image {
                resource_id: resource,
                alt: "图".into(),
                display_width: None,
                link: None,
            },
        ],
    }])
}

fn client() -> (TempDir, LibraryRepository) {
    let root = tempdir().unwrap();
    let repo = LibraryRepository::open(root.path().join("library.sqlite")).unwrap();
    (root, repo)
}

/// 2.5 MiB of non-repeating bytes: three upload chunks.
fn picture() -> Vec<u8> {
    let mut bytes = Vec::with_capacity(2_621_440);
    let mut state = 0x9e37_79b9_u32;
    while bytes.len() < 2_621_440 {
        state ^= state << 13;
        state ^= state >> 17;
        state ^= state << 5;
        bytes.extend_from_slice(&state.to_le_bytes());
    }
    bytes
}

/// Lets `allowed` chunks through, then fails uploads as a dropped connection.
struct FlakyUpload<'a> {
    inner: &'a ServerStore,
    allowed: Cell<usize>,
}

impl SyncTransport for FlakyUpload<'_> {
    fn push(&self, request: &PushRequest) -> Result<PushResponse, TransportError> {
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
        if self.allowed.get() == 0 {
            return Err(TransportError::Retryable("connection dropped".into()));
        }
        self.allowed.set(self.allowed.get() - 1);
        SyncTransport::put_chunk(self.inner, sha256, size, offset, bytes)
    }
    fn read_range(&self, sha256: &str, offset: u64, len: u64) -> Result<Vec<u8>, TransportError> {
        SyncTransport::read_range(self.inner, sha256, offset, len)
    }
}

#[test]
fn an_attachment_reaches_the_other_device_byte_for_byte_after_an_interrupted_upload() {
    let server_root = tempdir().unwrap();
    let store = ServerStore::open(server_root.path()).unwrap();
    let (_a_root, a) = client();
    let bytes = picture();
    let sha = format!("{:x}", Sha256::digest(&bytes));
    let resource = a
        .import_resource(&bytes, "照片.png", "image/png", "png")
        .unwrap();
    let note = a
        .create_note(CreateNote {
            title: "带图".into(),
            notebook_id: None,
            document: with_image("看图", resource.clone()),
        })
        .unwrap();
    let flaky = FlakyUpload {
        inner: &store,
        allowed: Cell::new(1),
    };
    let report = sync::sync_once(&a, &flaky).unwrap();
    assert!(report.retryable > 0);
    assert_eq!(
        store.blob_status(&sha).unwrap(),
        BlobStatus::Partial { bytes: 1_048_576 },
        "one chunk made it"
    );
    // Nothing referencing the incomplete blob was published.
    let (_b_root, b) = client();
    sync::sync_once(&b, &store).unwrap();
    assert!(b.load_note(&note.id).unwrap().is_none());

    sync::sync_once(&a, &store).unwrap();
    assert_eq!(store.blob_status(&sha).unwrap(), BlobStatus::Complete);
    assert_eq!(a.outbox_count().unwrap(), 0);
    sync::sync_once(&b, &store).unwrap();
    let received = b.load_note(&note.id).unwrap().expect("note arrives");
    assert_eq!(received.resource_ids, vec![resource.clone()]);
    let copied = b
        .read_resource_bytes(&resource)
        .unwrap()
        .expect("bytes arrive");
    assert_eq!(format!("{:x}", Sha256::digest(&copied)), sha);
    let metadata = b.resource_metadata(&resource).unwrap().unwrap();
    assert_eq!(metadata.title, "照片.png");
}

#[test]
fn a_remote_attachment_delete_does_not_remove_one_still_used_here() {
    let server_root = tempdir().unwrap();
    let store = ServerStore::open(server_root.path()).unwrap();
    let (_a_root, a) = client();
    let resource = a
        .import_resource(b"shared picture", "共享.png", "image/png", "png")
        .unwrap();
    let note = a
        .create_note(CreateNote {
            title: "原笔记".into(),
            notebook_id: None,
            document: with_image("原", resource.clone()),
        })
        .unwrap();
    sync::sync_once(&a, &store).unwrap();
    let (_b_root, b) = client();
    sync::sync_once(&b, &store).unwrap();
    // B starts using the same attachment in a note A has not seen yet.
    let other = b
        .create_note(CreateNote {
            title: "B 的笔记".into(),
            notebook_id: None,
            document: with_image("B", resource.clone()),
        })
        .unwrap();
    // A deletes its note for good; the attachment is unreferenced on A.
    a.trash_note(&note.id).unwrap();
    a.purge_note(&note.id).unwrap();
    sync::sync_once(&a, &store).unwrap();
    sync::sync_once(&b, &store).unwrap();
    sync::sync_once(&a, &store).unwrap();
    for (device, name) in [(&a, "A"), (&b, "B")] {
        let bytes = device.read_resource_bytes(&resource).unwrap();
        assert_eq!(
            bytes.as_deref(),
            Some(&b"shared picture"[..]),
            "{name} keeps it"
        );
        assert!(device.load_note(&other.id).unwrap().is_some(), "{name}");
    }
}

/// A link that drops every second attachment range, or any request larger
/// than `largest` bytes, counting the attachment bytes it delivered.
struct ShortLivedLink<'a> {
    inner: &'a ServerStore,
    largest: usize,
    drop_every_other_range: bool,
    ranges: Cell<usize>,
    delivered: Cell<u64>,
}

impl ShortLivedLink<'_> {
    fn new(
        inner: &ServerStore,
        largest: usize,
        drop_every_other_range: bool,
    ) -> ShortLivedLink<'_> {
        ShortLivedLink {
            inner,
            largest,
            drop_every_other_range,
            ranges: Cell::new(0),
            delivered: Cell::new(0),
        }
    }
}

impl SyncTransport for ShortLivedLink<'_> {
    fn push(&self, request: &PushRequest) -> Result<PushResponse, TransportError> {
        if serde_json::to_vec(request).unwrap().len() > self.largest {
            return Err(TransportError::Retryable("link dropped".into()));
        }
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
        if bytes.len() > self.largest {
            return Err(TransportError::Retryable("link dropped".into()));
        }
        SyncTransport::put_chunk(self.inner, sha256, size, offset, bytes)
    }
    fn read_range(&self, sha256: &str, offset: u64, len: u64) -> Result<Vec<u8>, TransportError> {
        self.ranges.set(self.ranges.get() + 1);
        if len as usize > self.largest
            || (self.drop_every_other_range && self.ranges.get() % 2 == 0)
        {
            return Err(TransportError::Retryable("link dropped".into()));
        }
        let bytes = SyncTransport::read_range(self.inner, sha256, offset, len)?;
        self.delivered
            .set(self.delivered.get() + bytes.len() as u64);
        Ok(bytes)
    }
}

fn sync_until_idle(repository: &LibraryRepository, transport: &dyn SyncTransport) -> usize {
    for pass in 1..=200 {
        let report = sync::sync_once(repository, transport).unwrap();
        if report.retryable == 0 && repository.sync_pending_count().unwrap() == 0 {
            return pass;
        }
    }
    panic!("never converged");
}

#[test]
fn an_interrupted_download_resumes_where_it_stopped() {
    let server_root = tempdir().unwrap();
    let store = ServerStore::open(server_root.path()).unwrap();
    let (_a_root, a) = client();
    let bytes = picture();
    let resource = a
        .import_resource(&bytes, "照片.png", "image/png", "png")
        .unwrap();
    a.create_note(CreateNote {
        title: "带图".into(),
        notebook_id: None,
        document: with_image("看图", resource.clone()),
    })
    .unwrap();
    sync::sync_once(&a, &store).unwrap();

    let (_b_root, b) = client();
    let link = ShortLivedLink::new(&store, usize::MAX, true);
    let passes = sync_until_idle(&b, &link);
    assert!(passes > 1, "the link did drop");
    assert_eq!(b.read_resource_bytes(&resource).unwrap().unwrap(), bytes);
    assert_eq!(
        link.delivered.get(),
        bytes.len() as u64,
        "no byte fetched twice"
    );
}

#[test]
fn transfers_shrink_until_a_link_that_cannot_carry_large_requests_gets_everything_through() {
    let server_root = tempdir().unwrap();
    let store = ServerStore::open(server_root.path()).unwrap();
    let (_a_root, a) = client();
    let bytes = picture();
    let resource = a
        .import_resource(&bytes, "照片.png", "image/png", "png")
        .unwrap();
    for index in 0..40 {
        a.create_note(CreateNote {
            title: format!("笔记 {index}"),
            notebook_id: None,
            document: with_image(&"字".repeat(3000), resource.clone()),
        })
        .unwrap();
    }
    let link = ShortLivedLink::new(&store, 200 * 1024, false);
    sync_until_idle(&a, &link);
    let (_b_root, b) = client();
    sync_until_idle(&b, &link);
    assert_eq!(b.read_resource_bytes(&resource).unwrap().unwrap(), bytes);
    assert!(
        sync::sync_failures(&b).unwrap().is_empty(),
        "nothing skipped on the receiving side"
    );
    assert_eq!(b.list_notes(Default::default()).unwrap().len(), 40);
}

#[test]
fn a_note_that_arrives_before_its_attachment_waits_for_it_instead_of_being_lost() {
    use app_lite_protocol::{Action, Operation, PROTOCOL_VERSION};
    // A publishes normally to one server...
    let first_root = tempdir().unwrap();
    let first = ServerStore::open(first_root.path()).unwrap();
    let (_a_root, a) = client();
    let bytes = picture();
    let resource = a
        .import_resource(&bytes, "照片.png", "image/png", "png")
        .unwrap();
    let note = a
        .create_note(CreateNote {
            title: "先到的笔记".into(),
            notebook_id: None,
            document: with_image("看图", resource.clone()),
        })
        .unwrap();
    sync::sync_once(&a, &first).unwrap();
    let changes = first
        .pull(PullRequest {
            protocol: PROTOCOL_VERSION,
            cursor: 0,
            limit: 100,
        })
        .unwrap()
        .changes;

    // ...and another device replays it to a second server with the note
    // ahead of its attachment.
    let second_root = tempdir().unwrap();
    let second = ServerStore::open(second_root.path()).unwrap();
    let device = "e".repeat(32);
    let replay = |kinds: &[&str]| {
        for (index, change) in changes
            .iter()
            .filter(|change| kinds.contains(&change.entity.kind.as_str()))
            .enumerate()
        {
            let op_id = format!("{:032x}", index + 1 + 100 * kinds.len());
            second
                .push(PushRequest {
                    protocol: PROTOCOL_VERSION,
                    device_id: device.clone(),
                    ops: vec![Operation {
                        op_id,
                        device_id: device.clone(),
                        entity: change.entity.clone(),
                        base_revision: 0,
                        action: Action::Put {
                            payload: change.payload.clone().unwrap(),
                        },
                    }],
                })
                .unwrap();
        }
    };
    replay(&["notebook", "note"]);
    let (_b_root, b) = client();
    sync::sync_once(&b, &second).unwrap();
    assert!(b.load_note(&note.id).unwrap().is_none());
    let waiting = sync::sync_failures(&b).unwrap();
    assert_eq!(waiting.len(), 1);
    assert!(waiting[0].waiting && !waiting[0].can_retry, "{waiting:?}");

    let sha = format!("{:x}", Sha256::digest(&bytes));
    let blob = SyncTransport::read_range(&first, &sha, 0, bytes.len() as u64).unwrap();
    second
        .put_chunk(&sha, bytes.len() as u64, 0, &blob)
        .unwrap();
    replay(&["resource"]);
    sync::sync_once(&b, &second).unwrap();
    let arrived = b
        .load_note(&note.id)
        .unwrap()
        .expect("applied once the attachment is here");
    assert_eq!(arrived.resource_ids, vec![resource]);
    assert!(sync::sync_failures(&b).unwrap().is_empty());
}

fn picture_seeded(seed: u32) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(2_621_440);
    let mut state = seed | 1;
    while bytes.len() < 2_621_440 {
        state ^= state << 13;
        state ^= state >> 17;
        state ^= state << 5;
        bytes.extend_from_slice(&state.to_le_bytes());
    }
    bytes
}

#[test]
fn attachments_finished_on_earlier_attempts_are_not_downloaded_again() {
    let server_root = tempdir().unwrap();
    let store = ServerStore::open(server_root.path()).unwrap();
    let (_a_root, a) = client();
    let pictures: Vec<(ResourceId, Vec<u8>)> = (0..3)
        .map(|seed| {
            let bytes = picture_seeded(2 * seed + 23);
            let id = a
                .import_resource(&bytes, &format!("图 {seed}.png"), "image/png", "png")
                .unwrap();
            (id, bytes)
        })
        .collect();
    let mut inlines = Vec::new();
    for (id, _) in &pictures {
        inlines.push(Inline::Image {
            resource_id: id.clone(),
            alt: "图".into(),
            display_width: None,
            link: None,
        });
    }
    a.create_note(CreateNote {
        title: "三张图".into(),
        notebook_id: None,
        document: CanonicalDocument::from_blocks(vec![Block::Paragraph {
            style: BlockStyle::default(),
            inlines,
        }]),
    })
    .unwrap();
    sync::sync_once(&a, &store).unwrap();

    let (_b_root, b) = client();
    let link = ShortLivedLink::new(&store, usize::MAX, true);
    sync_until_idle(&b, &link);
    for (id, bytes) in &pictures {
        assert_eq!(&b.read_resource_bytes(id).unwrap().unwrap(), bytes);
    }
    let total: u64 = pictures.iter().map(|(_, bytes)| bytes.len() as u64).sum();
    assert_eq!(link.delivered.get(), total, "no byte fetched twice");
}
