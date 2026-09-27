//! Sync acceptance probe against a running app-lite-server:
//!
//!   APP_LITE_SERVER_TOKEN=… sync_drill <profile> <server-url>
//!
//! Runs sync passes until nothing is left to upload or download, then checks
//! every attachment's bytes against its SHA-256 and prints entity counts, so
//! two profiles can be compared. Prints only counts, timings and outcomes.

use std::time::Instant;

use app_lite_core::{LibraryRepository, ResourceId, library_counts, sync};
use app_lite_protocol::client::HttpTransport;
use rusqlite::{Connection, OpenFlags};
use sha2::{Digest, Sha256};

fn main() {
    let mut args = std::env::args().skip(1);
    let profile = std::path::PathBuf::from(args.next().expect("profile"));
    let url = args.next().expect("server url");
    let token = std::env::var("APP_LITE_SERVER_TOKEN").expect("APP_LITE_SERVER_TOKEN");
    std::fs::create_dir_all(&profile).unwrap();
    let repository = LibraryRepository::open(profile.join("library.sqlite")).unwrap();
    let transport = HttpTransport::new(&url, &token);
    let started = Instant::now();
    let mut total = sync::SyncReport::default();
    for pass in 1.. {
        let report = sync::sync_once(&repository, &transport).expect("sync pass");
        println!(
            "pass {pass}: accepted {} pulled {} conflicts {} retryable {} permanent {} skipped {}",
            report.accepted,
            report.pulled,
            report.conflicts,
            report.retryable,
            report.permanent,
            report.skipped
        );
        total.accepted += report.accepted;
        total.pulled += report.pulled;
        total.conflicts += report.conflicts;
        total.permanent += report.permanent;
        total.skipped += report.skipped;
        let idle = report.accepted == 0 && report.pulled == 0 && report.retryable == 0;
        if idle || pass >= 200 {
            break;
        }
    }
    println!("sync.elapsed_ms {}", started.elapsed().as_millis());
    println!(
        "sync.total accepted {} pulled {} conflicts {} permanent {} skipped {}",
        total.accepted, total.pulled, total.conflicts, total.permanent, total.skipped
    );
    println!("sync.pending {}", repository.sync_pending_count().unwrap());
    for failure in repository.sync_failures().unwrap() {
        println!(
            "sync.failure {} {} {}",
            failure.entity_type, failure.entity_id, failure.reason
        );
    }
    drop(repository);
    println!("counts {:?}", library_counts(&profile).unwrap());

    let repository = LibraryRepository::open(profile.join("library.sqlite")).unwrap();
    let db = Connection::open_with_flags(
        profile.join("library.sqlite"),
        OpenFlags::SQLITE_OPEN_READ_ONLY,
    )
    .unwrap();
    let mut statement = db.prepare("SELECT id, sha256 FROM resources").unwrap();
    let mut rows = statement.query([]).unwrap();
    let (mut checked, mut mismatched) = (0, 0);
    while let Some(row) = rows.next().unwrap() {
        let id = ResourceId::new(row.get::<_, String>(0).unwrap()).unwrap();
        let expected: String = row.get(1).unwrap();
        let bytes = repository
            .read_resource_bytes(&id)
            .unwrap()
            .unwrap_or_default();
        checked += 1;
        if format!("{:x}", Sha256::digest(&bytes)) != expected {
            mismatched += 1;
        }
    }
    println!("resources.rehashed {checked} mismatched {mismatched}");
    // A digest of note content (id, title, body, notebook, tags) comparable
    // across profiles without printing any of it.
    let mut digest = Sha256::new();
    let mut notes = db
        .prepare(
            "SELECT n.id, n.title, n.body_html, n.notebook_id, n.deleted_time,
                    (SELECT group_concat(tag_id, ',') FROM (SELECT tag_id FROM note_tags WHERE note_id=n.id ORDER BY position, tag_id))
             FROM notes n ORDER BY n.id",
        )
        .unwrap();
    let mut rows = notes.query([]).unwrap();
    while let Some(row) = rows.next().unwrap() {
        for index in 0..6 {
            let value = format!("{:?}", row.get::<_, rusqlite::types::Value>(index).unwrap());
            digest.update((value.len() as u64).to_le_bytes());
            digest.update(value.as_bytes());
        }
    }
    println!("notes.content_digest {:x}", digest.finalize());
}
