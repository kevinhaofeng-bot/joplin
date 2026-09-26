//! Backup/restore acceptance probe: `cargo run --example backup_verify -- <profile> <work_dir>`
//!
//! Backs up a library profile, restores it into a new directory and compares
//! entity counts and the blob set. Prints only counts, sizes and timings.

use std::{collections::BTreeSet, path::Path, sync::atomic::AtomicBool, time::Instant};

use app_lite_core::{backup_library, library_counts, restore_library_backup};
use rusqlite::types::ValueRef;
use sha2::{Digest, Sha256};

// Compare logical rows rather than database-file bytes (restore intentionally
// resets sync state). Length/type framing prevents ambiguous concatenations.
fn content_digest(profile: &Path, table: &str) -> String {
    let db = rusqlite::Connection::open_with_flags(
        profile.join("library.sqlite"),
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
    )
    .unwrap();
    let mut statement = db
        .prepare(&format!("SELECT * FROM {table} ORDER BY 1, 2"))
        .unwrap();
    let columns = statement.column_count();
    let mut rows = statement.query([]).unwrap();
    let mut digest = Sha256::new();
    while let Some(row) = rows.next().unwrap() {
        digest.update(b"row");
        for index in 0..columns {
            match row.get_ref(index).unwrap() {
                ValueRef::Null => digest.update([0]),
                ValueRef::Integer(value) => {
                    digest.update([1]);
                    digest.update(value.to_le_bytes());
                }
                ValueRef::Real(value) => {
                    digest.update([2]);
                    digest.update(value.to_bits().to_le_bytes());
                }
                ValueRef::Text(bytes) | ValueRef::Blob(bytes) => {
                    digest.update([
                        if matches!(row.get_ref(index).unwrap(), ValueRef::Text(_)) {
                            3
                        } else {
                            4
                        },
                    ]);
                    digest.update((bytes.len() as u64).to_le_bytes());
                    digest.update(bytes);
                }
            }
        }
    }
    format!("{:x}", digest.finalize())
}

fn blob_set(profile: &Path) -> BTreeSet<String> {
    rusqlite::Connection::open_with_flags(
        profile.join("library.sqlite"),
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
    )
    .unwrap()
    .prepare("SELECT sha256 FROM resource_blobs")
    .unwrap()
    .query_map([], |row| row.get(0))
    .unwrap()
    .collect::<Result<_, _>>()
    .unwrap()
}

fn main() {
    let mut args = std::env::args().skip(1);
    let profile = std::path::PathBuf::from(args.next().expect("profile"));
    let work = std::path::PathBuf::from(args.next().expect("work dir"));
    let cancel = AtomicBool::new(false);
    let started = Instant::now();
    let report = backup_library(&profile, &work.join("backup"), &cancel).expect("backup");
    println!(
        "backup.elapsed_ms {} blobs {} bytes {}",
        started.elapsed().as_millis(),
        report.blobs,
        report.bytes
    );
    let started = Instant::now();
    let restored = restore_library_backup(&work.join("backup"), &work.join("restored"), &cancel)
        .expect("restore");
    println!("restore.elapsed_ms {}", started.elapsed().as_millis());
    let source = library_counts(&profile).unwrap();
    println!("source.counts {source:?}");
    println!("restored.counts {:?}", restored.counts);
    println!("counts_equal {}", source == restored.counts);
    assert_eq!(source, restored.counts, "restored entity counts differ");
    println!(
        "blob_sets_equal {}",
        blob_set(&profile) == blob_set(&restored.path)
    );
    assert_eq!(
        blob_set(&profile),
        blob_set(&restored.path),
        "restored blob set differs"
    );
    for table in [
        "notes",
        "notebooks",
        "stacks",
        "tags",
        "note_tags",
        "note_resources",
        "resources",
        "resource_blobs",
        "note_revisions",
        "shortcuts",
        "edit_journal",
    ] {
        let same = content_digest(&profile, table) == content_digest(&restored.path, table);
        println!("content_equal.{table} {same}");
        assert!(same, "restored content differs in {table}");
    }
    let mut verified = 0;
    for hash in blob_set(&restored.path) {
        let mut file =
            std::fs::File::open(restored.path.join("resources/blobs").join(&hash)).unwrap();
        let mut digest = Sha256::new();
        std::io::copy(&mut file, &mut digest).unwrap();
        assert_eq!(
            format!("{:x}", digest.finalize()),
            hash,
            "restored blob bytes differ"
        );
        verified += 1;
    }
    println!("restored_blobs_rehashed {verified}");
}
