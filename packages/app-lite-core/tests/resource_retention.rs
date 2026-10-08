#![cfg(feature = "test-support")]

// Spec 7.6/11.4: permanent note deletion is immediate, physical blob GC is
// not. These exercise real repository open/purge, SQLite and blob files.
use app_lite_core::document::Block;
use app_lite_core::{
    CanonicalDocument, CreateNote, LibraryError, LibraryRepository, RepositoryClock,
    RepositoryIdSource,
};
use rusqlite::Connection;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicI64, AtomicU64, Ordering};

const START: i64 = 1_700_000_000_000;
const SEVEN_DAYS: i64 = 604_800_000;

struct Clock(AtomicI64);
impl RepositoryClock for Clock {
    fn now_millis(&self) -> i64 {
        self.0.load(Ordering::SeqCst)
    }
}
struct Ids(AtomicU64);
impl RepositoryIdSource for Ids {
    fn next_id(&self) -> Result<String, LibraryError> {
        Ok(format!("{:032x}", self.0.fetch_add(1, Ordering::SeqCst)))
    }
}
fn open(path: &Path, clock: &Arc<Clock>, ids: &Arc<Ids>) -> LibraryRepository {
    LibraryRepository::open_with_sources(path, clock.clone(), ids.clone()).unwrap()
}
fn purge_owned_blob(repo: &LibraryRepository, bytes: &[u8]) -> app_lite_core::BlobHash {
    let resource = repo
        .import_resource(bytes, "retention.txt", "text/plain", "txt")
        .unwrap();
    let hash = repo.resource_metadata(&resource).unwrap().unwrap().sha256;
    let note = repo
        .create_note(CreateNote {
            title: "isolated retention fixture".into(),
            notebook_id: None,
            document: CanonicalDocument::from_blocks(vec![Block::Attachment {
                resource_id: resource.clone(),
                filename: "retention.txt".into(),
                media_type: "text/plain".into(),
            }]),
        })
        .unwrap();
    repo.trash_note(&note.id).unwrap();
    repo.purge_note(&note.id).unwrap();
    assert!(
        repo.load_note(&note.id).unwrap().is_none(),
        "permanent deletion remains immediate"
    );
    assert!(repo.resource_metadata(&resource).unwrap().is_none());
    hash
}

#[test]
fn orphan_blob_waits_seven_days_across_reopen_and_clock_rollback() {
    // Removing the created_time eligibility check must fail on the first
    // assertion, not only on a mocked cleanup callback.
    let profile = tempfile::tempdir().unwrap();
    let path = profile.path().join("library.sqlite");
    let clock = Arc::new(Clock(AtomicI64::new(START)));
    let ids = Arc::new(Ids(AtomicU64::new(1)));
    let repo = open(&path, &clock, &ids);
    let hash = purge_owned_blob(&repo, b"first retained blob");
    let blob = profile.path().join("resources/blobs").join(hash.as_str());
    assert!(
        blob.exists(),
        "fresh permanent purge must retain the orphan bytes"
    );
    drop(repo);
    for now in [START - 1, START, START + SEVEN_DAYS - 1] {
        clock.0.store(now, Ordering::SeqCst);
        let repo = open(&path, &clock, &ids);
        assert!(
            blob.exists(),
            "opening before expiry must not unlink the blob"
        );
        drop(repo);
    }
    clock.0.store(START + SEVEN_DAYS - 1, Ordering::SeqCst);
    let repo = open(&path, &clock, &ids);
    let younger = purge_owned_blob(&repo, b"younger retained blob");
    let younger_blob = profile
        .path()
        .join("resources/blobs")
        .join(younger.as_str());
    drop(repo);
    clock.0.store(START + SEVEN_DAYS, Ordering::SeqCst);
    let repo = open(&path, &clock, &ids);
    assert!(
        !blob.exists(),
        "expired unreferenced bytes must actually be collected"
    );
    assert!(
        younger_blob.exists(),
        "an eligible row must not age other queued blobs"
    );
    assert_eq!(
        Connection::open(&path)
            .unwrap()
            .query_row("SELECT count(*) FROM resource_gc_queue", [], |r| r
                .get::<_, i64>(0))
            .unwrap(),
        1
    );
    drop(repo);
}

#[test]
fn reimported_hash_survives_the_old_gc_deadline() {
    // A due queue entry cannot delete bytes that gained new metadata during
    // the grace period. Keep the existing IMMEDIATE transaction guard.
    let profile = tempfile::tempdir().unwrap();
    let path = profile.path().join("library.sqlite");
    let clock = Arc::new(Clock(AtomicI64::new(START)));
    let ids = Arc::new(Ids(AtomicU64::new(1)));
    let repo = open(&path, &clock, &ids);
    let hash = purge_owned_blob(&repo, b"same hash reused");
    let blob = profile.path().join("resources/blobs").join(hash.as_str());
    assert!(blob.exists(), "reimport must start from a retained orphan");
    let renewed = repo
        .import_resource(b"same hash reused", "renewed.txt", "text/plain", "txt")
        .unwrap();
    drop(repo);
    clock.0.store(START + SEVEN_DAYS, Ordering::SeqCst);
    let reopened = open(&path, &clock, &ids);
    assert_eq!(
        reopened.read_resource_bytes(&renewed).unwrap().unwrap(),
        b"same hash reused"
    );
    assert!(blob.exists());
    assert_eq!(
        Connection::open(&path)
            .unwrap()
            .query_row("SELECT count(*) FROM resource_gc_queue", [], |r| r
                .get::<_, i64>(0))
            .unwrap(),
        0
    );
}

#[test]
fn a_second_last_reference_purge_starts_a_new_grace_period() {
    // INSERT OR IGNORE on the old hash queue must not shorten the grace of
    // bytes that were reused and then became unreferenced a second time.
    let profile = tempfile::tempdir().unwrap();
    let path = profile.path().join("library.sqlite");
    let clock = Arc::new(Clock(AtomicI64::new(START)));
    let ids = Arc::new(Ids(AtomicU64::new(1)));
    let repo = open(&path, &clock, &ids);
    let hash = purge_owned_blob(&repo, b"purged, reused, purged again");
    let blob = profile.path().join("resources/blobs").join(hash.as_str());
    clock.0.store(START + 518_400_000, Ordering::SeqCst); // six days later
    assert_eq!(
        purge_owned_blob(&repo, b"purged, reused, purged again"),
        hash
    );
    drop(repo);
    clock.0.store(START + SEVEN_DAYS, Ordering::SeqCst);
    let repo = open(&path, &clock, &ids);
    assert!(
        blob.exists(),
        "the previous purge deadline must not collect a newly orphaned hash"
    );
    drop(repo);
    clock.0.store(START + 1_123_200_000 - 1, Ordering::SeqCst); // thirteen days - 1ms
    let repo = open(&path, &clock, &ids);
    assert!(blob.exists());
    drop(repo);
    clock.0.store(START + 1_123_200_000, Ordering::SeqCst);
    let _repo = open(&path, &clock, &ids);
    assert!(
        !blob.exists(),
        "seven days after the latest last-reference removal is due"
    );
}
