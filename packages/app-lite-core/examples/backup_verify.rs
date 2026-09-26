//! Backup/restore acceptance probe: `cargo run --example backup_verify -- <profile> <work_dir>`
//!
//! Backs up a library profile, restores it into a new directory and compares
//! entity counts and the blob set. Prints only counts, sizes and timings.

use std::{collections::BTreeSet, path::Path, sync::atomic::AtomicBool, time::Instant};

use app_lite_core::{backup_library, library_counts, restore_library_backup};

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
    println!(
        "blob_sets_equal {}",
        blob_set(&profile) == blob_set(&restored.path)
    );
}
