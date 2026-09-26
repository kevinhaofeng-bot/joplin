//! Import acceptance probe: `cargo run --example import_verify -- <source.enex|.jex> <imports_dir>`
//!
//! Runs the product import on a *copy* of a library export, then checks the
//! published library independently of the importer: source scan counts, and
//! every published blob re-hashed from disk against the source set. Prints
//! only counts, IDs and hashes — never note titles or bodies.

use std::{
    collections::BTreeSet,
    fs,
    path::Path,
    sync::{Arc, atomic::AtomicBool},
};

use app_lite_core::{import_library_file, scan_enex_file, scan_jex_archive};
use sha2::{Digest, Sha256};

fn sha256_file(path: &Path) -> String {
    let mut digest = Sha256::new();
    let mut file = fs::File::open(path).unwrap();
    std::io::copy(&mut file, &mut digest).unwrap();
    format!("{:x}", digest.finalize())
}

fn main() {
    let mut args = std::env::args().skip(1);
    let source = std::path::PathBuf::from(args.next().expect("source path"));
    let imports = std::path::PathBuf::from(args.next().expect("imports dir"));
    println!("source_sha256 {}", sha256_file(&source));

    let is_jex = source.extension().is_some_and(|ext| ext == "jex");
    let source_blobs: BTreeSet<String>;
    if is_jex {
        let scan = scan_jex_archive(&source).expect("scan jex");
        println!("scan.counts {:?}", scan.counts);
        println!("scan.is_clean {}", scan.is_clean());
        println!(
            "scan.blockers missing_files={} unresolved_refs={} unsupported_items={} encrypted={} store_compat={} dup_ids={} orphan_rel={} orphan_files={}",
            scan.missing_resource_files.len(),
            scan.unresolved_note_body_internal_references.len(),
            scan.unsupported_items.len(),
            scan.encrypted_item_ids.len(),
            scan.store_compatibility_blockers.len(),
            scan.duplicate_item_ids.len(),
            scan.orphan_note_tag_relations.len(),
            scan.orphan_physical_resource_files.len(),
        );
        source_blobs = scan
            .physical_resource_files
            .iter()
            .map(|file| file.sha256.clone())
            .collect();
    } else {
        let scan = scan_enex_file(&source).expect("scan enex");
        println!("scan.counts {:?}", scan.counts);
        println!(
            "scan.unresolved_media {}",
            scan.unresolved_media_references.len()
        );
        source_blobs = scan
            .resources
            .iter()
            .map(|resource| resource.sha256.clone())
            .collect();
    }
    println!("source.distinct_blobs {}", source_blobs.len());

    let started = std::time::Instant::now();
    let outcome = match import_library_file(&source, &imports, &Arc::new(AtomicBool::new(false))) {
        Ok(outcome) => outcome,
        Err(error) => {
            println!("IMPORT_FAILED {error}");
            std::process::exit(2);
        }
    };
    println!("import.elapsed_ms {}", started.elapsed().as_millis());
    println!("import.path {}", outcome.library.path.display());
    println!("import.counts {:?}", outcome.library.counts);
    println!("import.degraded {}", outcome.degraded.len());
    for line in &outcome.degraded {
        // Source IDs / ordinals and the construct name only.
        println!("  degraded {line}");
    }

    let db = rusqlite::Connection::open_with_flags(
        outcome.library.path.join("library.sqlite"),
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
    )
    .unwrap();
    let rows: Vec<(String, String, i64)> = db
        .prepare("SELECT sha256, relative_path, size FROM resource_blobs")
        .unwrap()
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    let mut bad = 0;
    let mut published = BTreeSet::new();
    for (sha256, relative, size) in &rows {
        let path = outcome.library.path.join(relative);
        let actual = sha256_file(&path);
        let len = fs::metadata(&path).unwrap().len() as i64;
        if &actual != sha256 || len != *size {
            bad += 1;
            println!("  BLOB_MISMATCH {sha256}");
        }
        published.insert(actual);
    }
    println!("verify.published_blobs {} mismatched {bad}", rows.len());
    let missing: Vec<_> = source_blobs.difference(&published).collect();
    let extra: Vec<_> = published.difference(&source_blobs).collect();
    println!(
        "verify.source_blobs_missing {} extra {}",
        missing.len(),
        extra.len()
    );
    for sha in missing.iter().take(20) {
        println!("  missing {sha}");
    }
}
