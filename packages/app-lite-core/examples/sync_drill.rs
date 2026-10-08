//! Sync acceptance probe against a running app-lite-server:
//!
//!   APP_LITE_SERVER_TOKEN=… sync_drill <profile> <server-url>
//!
//! `APP_LITE_SERVER_CA=<pem file>` additionally trusts that certificate
//! (an HTTPS server with a self-signed certificate).
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
    let certificate = std::env::var_os("APP_LITE_SERVER_CA")
        .map(|path| std::fs::read_to_string(path).expect("APP_LITE_SERVER_CA readable"));
    let transport = HttpTransport::with_trusted_certificate(&url, &token, certificate.as_deref())
        .expect("valid certificate");
    let started = Instant::now();
    let mut total = sync::SyncReport::default();
    let mut converged = false;
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
        converged = idle;
        if idle || pass >= 200 {
            break;
        }
    }
    println!("sync.elapsed_ms {}", started.elapsed().as_millis());
    println!(
        "sync.total accepted {} pulled {} conflicts {} permanent {} skipped {}",
        total.accepted, total.pulled, total.conflicts, total.permanent, total.skipped
    );
    let pending = repository.sync_pending_count().unwrap();
    let failures = repository.sync_failures().unwrap();
    println!("sync.pending {pending}");
    for failure in &failures {
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
    if !converged || pending != 0 || !failures.is_empty() || mismatched != 0 {
        eprintln!(
            "sync verification failed: converged={converged} pending={pending} failures={} mismatched={mismatched}",
            failures.len()
        );
        std::process::exit(1);
    }
}
