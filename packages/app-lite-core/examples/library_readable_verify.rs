//! Whole-library readable export acceptance probe:
//! `cargo run --example library_readable_verify -- <profile> <work_dir>`
//!
//! Exports the profile as readable HTML/JSON/resources, restores it into a new
//! empty profile and compares the logical rows of every carried table and the
//! re-hashed blob files. Prints only counts, timings and equality flags.

use std::{path::Path, time::Instant};

use app_lite_core::{LibraryRepository, export_library_readable, restore_library_readable};
use rusqlite::{Connection, OpenFlags};
use sha2::{Digest, Sha256};

// Same table list as tests/library_readable_export.rs `rows()`.
const TABLES: [(&str, &str); 10] = [
    (
        "stacks",
        "SELECT id,title,revision,created_time,updated_time,deleted_time FROM stacks ORDER BY id",
    ),
    (
        "notebooks",
        "SELECT id,title,IFNULL(stack_id,''),is_default,revision,created_time,updated_time,deleted_time FROM notebooks ORDER BY id",
    ),
    (
        "tags",
        "SELECT id,title,revision,created_time,updated_time,deleted_time FROM tags ORDER BY id",
    ),
    (
        "note_tags",
        "SELECT note_id,tag_id,position FROM note_tags ORDER BY note_id,tag_id",
    ),
    (
        "notes",
        "SELECT id,title,body_html,body_text,notebook_id,IFNULL(selected_thumbnail_id,''),created_time,updated_time,deleted_time,revision FROM notes ORDER BY id",
    ),
    (
        "note_resources",
        "SELECT note_id,position,resource_id,is_associated FROM note_resources ORDER BY note_id,position",
    ),
    (
        "note_revisions",
        "SELECT note_id,revision,title,body_html,body_text,created_time FROM note_revisions ORDER BY note_id,revision",
    ),
    (
        "resources",
        "SELECT id,sha256,title,mime,file_extension,size,created_time,updated_time,deleted_time,revision FROM resources ORDER BY id",
    ),
    (
        "resource_blobs",
        "SELECT sha256 FROM resource_blobs ORDER BY sha256",
    ),
    (
        "shortcuts",
        "SELECT id,entity_type,entity_id,position,created_time FROM shortcuts ORDER BY id",
    ),
];

fn digest(profile: &Path, sql: &str) -> (usize, String) {
    let db = Connection::open_with_flags(
        profile.join("library.sqlite"),
        OpenFlags::SQLITE_OPEN_READ_ONLY,
    )
    .unwrap();
    let mut statement = db.prepare(sql).unwrap();
    let columns = statement.column_count();
    let mut rows = statement.query([]).unwrap();
    let mut hash = Sha256::new();
    let mut count = 0;
    while let Some(row) = rows.next().unwrap() {
        count += 1;
        for index in 0..columns {
            let value = format!("{:?}", row.get::<_, rusqlite::types::Value>(index).unwrap());
            hash.update((value.len() as u64).to_le_bytes());
            hash.update(value.as_bytes());
        }
    }
    (count, format!("{:x}", hash.finalize()))
}

fn main() {
    let mut args = std::env::args().skip(1);
    let profile = std::path::PathBuf::from(args.next().expect("profile"));
    let work = std::path::PathBuf::from(args.next().expect("work dir"));
    let bundle = work.join("readable-export");
    let restored = work.join("restored");

    let repository = LibraryRepository::open(profile.join("library.sqlite")).unwrap();
    let started = Instant::now();
    let report = export_library_readable(&repository, &bundle).expect("export");
    println!(
        "export.elapsed_ms {} notes {} resources {}",
        started.elapsed().as_millis(),
        report.note_count,
        report.resource_count
    );
    drop(repository);

    std::fs::create_dir(&restored).unwrap();
    let started = Instant::now();
    let report = restore_library_readable(&bundle, &restored).expect("restore");
    println!(
        "restore.elapsed_ms {} notes {} resources {}",
        started.elapsed().as_millis(),
        report.note_count,
        report.resource_count
    );

    let mut all_equal = true;
    for (table, sql) in TABLES {
        let source = digest(&profile, sql);
        let target = digest(&restored, sql);
        all_equal &= source == target;
        println!("table {table} rows {} equal {}", source.0, source == target);
    }
    let restored_repository = LibraryRepository::open(restored.join("library.sqlite")).unwrap();
    let mut rehashed = 0;
    let db = Connection::open_with_flags(
        restored.join("library.sqlite"),
        OpenFlags::SQLITE_OPEN_READ_ONLY,
    )
    .unwrap();
    let mut statement = db.prepare("SELECT id,sha256 FROM resources").unwrap();
    let mut rows = statement.query([]).unwrap();
    while let Some(row) = rows.next().unwrap() {
        let id = app_lite_core::ResourceId::new(row.get::<_, String>(0).unwrap()).unwrap();
        let expected: String = row.get(1).unwrap();
        let bytes = restored_repository
            .read_resource_bytes(&id)
            .unwrap()
            .expect("restored resource bytes");
        assert_eq!(format!("{:x}", Sha256::digest(&bytes)), expected);
        rehashed += 1;
    }
    println!("resources_rehashed {rehashed}");
    println!("all_tables_equal {all_equal}");
    assert!(all_equal);
}
