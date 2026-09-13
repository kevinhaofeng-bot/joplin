use std::{
    fs::{self, File},
    io::{Cursor, Read, Write},
};

use app_lite_core::{
    JexPrepareError, JexScanError, prepare_jex_source_archive,
    prepare_jex_source_archive_with_cancel,
};
use rusqlite::Connection;
use sha2::{Digest, Sha256};
use std::sync::{Arc, atomic::AtomicBool};
use tar::{Builder, Header};
use tempfile::{NamedTempFile, TempPath, tempdir};

const NOTE: &str = "11111111111111111111111111111111";
const FOLDER: &str = "22222222222222222222222222222222";
const RESOURCE: &str = "33333333333333333333333333333333";
const TAG: &str = "44444444444444444444444444444444";
const NOTE_TAG: &str = "55555555555555555555555555555555";

fn append(builder: &mut Builder<File>, path: &str, bytes: &[u8]) {
    let mut header = Header::new_gnu();
    header.set_size(bytes.len() as u64);
    header.set_mode(0o644);
    header.set_cksum();
    builder
        .append_data(&mut header, path, Cursor::new(bytes))
        .unwrap();
}

fn archive(entries: impl FnOnce(&mut Builder<File>)) -> TempPath {
    let path = NamedTempFile::new().unwrap().into_temp_path();
    let mut builder = Builder::new(File::create(&path).unwrap());
    entries(&mut builder);
    builder.finish().unwrap();
    path
}

fn truncated_resource_archive() -> TempPath {
    let path = NamedTempFile::new().unwrap().into_temp_path();
    let mut file = File::create(&path).unwrap();
    let metadata =
        format!("图.png\n\nid: {RESOURCE}\ntype_: 4\nmime: image/png\nfile_extension: png\n");
    let mut item_header = Header::new_gnu();
    item_header.set_path(format!("{RESOURCE}.md")).unwrap();
    item_header.set_size(metadata.len() as u64);
    item_header.set_mode(0o644);
    item_header.set_cksum();
    file.write_all(item_header.as_bytes()).unwrap();
    file.write_all(metadata.as_bytes()).unwrap();
    file.write_all(&vec![0; (512 - metadata.len() % 512) % 512])
        .unwrap();

    let mut resource_header = Header::new_gnu();
    resource_header
        .set_path(format!("resources/{RESOURCE}.png"))
        .unwrap();
    resource_header.set_size(32 * 1024);
    resource_header.set_mode(0o644);
    resource_header.set_cksum();
    file.write_all(resource_header.as_bytes()).unwrap();
    file.write_all(b"truncated resource bytes").unwrap();
    path
}

#[test]
fn spools_shuffled_jex_raw_items_and_large_resource_in_owned_child() {
    // Mutation caught: tar-order dependence, CRLF normalization in audit,
    // a 16 KiB whole-resource buffer, or a source-only report without files.
    let body = format!("前文\r\n![图](:/{RESOURCE})\r\n{}", "中文".repeat(24_000));
    let note = format!(
        "标题\r\n\r\n{body}\r\n\r\nid: {NOTE}\r\ntype_: 1\r\nparent_id: {FOLDER}\r\nmarkup_language: 1\r\n"
    );
    let folder = format!("笔记本\n\nid: {FOLDER}\ntype_: 2\n");
    let metadata =
        format!("图.png\n\nid: {RESOURCE}\ntype_: 4\nmime: image/png\nfile_extension: png\n");
    let tag = format!("中文标签\n\nid: {TAG}\ntype_: 5\n");
    let relation = format!("id: {NOTE_TAG}\ntype_: 6\nnote_id: {NOTE}\ntag_id: {TAG}\n");
    let blob = vec![b'x'; 32 * 1024 + 7];
    let source = archive(|builder| {
        append(builder, &format!("resources/{RESOURCE}.png"), &blob);
        append(builder, &format!("{NOTE_TAG}.md"), relation.as_bytes());
        append(builder, &format!("{TAG}.md"), tag.as_bytes());
        append(builder, &format!("{RESOURCE}.md"), metadata.as_bytes());
        append(builder, &format!("{NOTE}.md"), note.as_bytes());
        append(builder, &format!("{FOLDER}.md"), folder.as_bytes());
    });
    let parent = tempdir().unwrap();
    let prepared = prepare_jex_source_archive(&source, parent.path()).unwrap();
    let child = prepared.staging_path().to_path_buf();
    assert!(child.starts_with(fs::canonicalize(parent.path()).unwrap()));
    assert!(!child.join("library.sqlite").exists());
    assert_eq!(prepared.report().metadata_items.len(), 5);
    assert_eq!(prepared.report().physical_resource_files.len(), 1);
    assert_eq!(prepared.report().archive_entry_count, 6);
    for (id, kind, path, bytes) in [
        (NOTE, 1, format!("{NOTE}.md"), note.as_bytes()),
        (FOLDER, 2, format!("{FOLDER}.md"), folder.as_bytes()),
        (RESOURCE, 4, format!("{RESOURCE}.md"), metadata.as_bytes()),
        (TAG, 5, format!("{TAG}.md"), tag.as_bytes()),
        (NOTE_TAG, 6, format!("{NOTE_TAG}.md"), relation.as_bytes()),
    ] {
        let evidence = prepared
            .report()
            .metadata_items
            .iter()
            .find(|item| item.source_id == id)
            .unwrap();
        assert_eq!(evidence.archive_path, path);
        assert_eq!(evidence.item_type, kind);
        assert_eq!(evidence.byte_count, bytes.len() as u64);
        assert_eq!(evidence.raw_sha256, format!("{:x}", Sha256::digest(bytes)));
        assert_eq!(prepared.raw_item(id).unwrap().unwrap().raw_bytes, bytes);
    }
    let item = prepared.raw_item(NOTE).unwrap().unwrap();
    assert_eq!(item.raw_bytes, note.as_bytes());
    assert_eq!(item.archive_path, format!("{NOTE}.md"));
    assert_eq!(
        item.raw_sha256,
        format!("{:x}", Sha256::digest(note.as_bytes()))
    );
    assert_eq!(
        item.canonical_note_body_sha256,
        Some(format!(
            "{:x}",
            Sha256::digest(body.replace("\r\n", "\n").as_bytes())
        ))
    );
    let db = Connection::open(prepared.spool_database_path()).unwrap();
    let reopened: Vec<u8> = db
        .query_row(
            "SELECT raw_bytes FROM jex_source_items WHERE source_id=?1",
            [NOTE],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(reopened, note.as_bytes());
    let (resource, mut file) = prepared.open_verified_resource(RESOURCE).unwrap().unwrap();
    assert_eq!(resource.archive_path, format!("resources/{RESOURCE}.png"));
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes).unwrap();
    assert_eq!(bytes, blob);
    assert_eq!(resource.sha256, format!("{:x}", Sha256::digest(&blob)));
    drop(db);
    drop(prepared);
    assert!(!child.exists());
    assert_eq!(fs::read_dir(parent.path()).unwrap().count(), 0);
}

#[test]
fn refuses_preflight_blockers_and_existing_profile_parent_without_touching_sibling() {
    // Mutation caught: creating a child before clean preflight or accepting a
    // live library directory as a staging parent.
    let parent = tempdir().unwrap();
    let sibling = parent.path().join("sibling");
    fs::create_dir(&sibling).unwrap();
    fs::write(sibling.join("keep.bin"), b"untouched").unwrap();
    let missing = archive(|builder| {
        append(
            builder,
            &format!("{RESOURCE}.md"),
            format!("id: {RESOURCE}\ntype_: 4\nmime: image/png\n").as_bytes(),
        );
    });
    assert!(matches!(
        prepare_jex_source_archive(&missing, parent.path()),
        Err(JexPrepareError::PreflightBlocked { report }) if report.missing_resource_files.len() == 1
    ));
    let encrypted = archive(|builder| {
        append(
            builder,
            &format!("{NOTE}.md"),
            format!("id: {NOTE}\ntype_: 1\nencryption_applied: 1\n").as_bytes(),
        );
    });
    assert!(matches!(
        prepare_jex_source_archive(&encrypted, parent.path()),
        Err(JexPrepareError::PreflightBlocked { report }) if report.encrypted_item_ids == vec![NOTE]
    ));
    let unsupported = archive(|builder| {
        append(
            builder,
            &format!("{NOTE}.md"),
            format!("id: {NOTE}\ntype_: 9\n").as_bytes(),
        );
    });
    assert!(matches!(
        prepare_jex_source_archive(&unsupported, parent.path()),
        Err(JexPrepareError::PreflightBlocked { report }) if report.unsupported_items.len() == 1
    ));
    let unsafe_item = archive(|builder| {
        append(builder, "unexpected.txt", b"bad");
    });
    assert!(matches!(
        prepare_jex_source_archive(&unsafe_item, parent.path()),
        Err(JexPrepareError::Scan(_))
    ));
    assert_eq!(fs::read(sibling.join("keep.bin")).unwrap(), b"untouched");
    assert_eq!(fs::read_dir(parent.path()).unwrap().count(), 1);

    let live = tempdir().unwrap();
    fs::write(live.path().join("library.sqlite"), b"sentinel").unwrap();
    assert!(matches!(
        prepare_jex_source_archive(&missing, live.path()),
        Err(JexPrepareError::InvalidStagingParent)
    ));
    assert_eq!(
        fs::read(live.path().join("library.sqlite")).unwrap(),
        b"sentinel"
    );
}

#[test]
fn rejects_truncated_resource_tar_without_creating_a_stage_child() {
    // Mutation caught: treating a damaged resource as an empty/partial file,
    // or leaving a stage child behind when preflight returns a typed error.
    let source = truncated_resource_archive();
    let parent = tempdir().unwrap();
    let sentinel = parent.path().join("sibling");
    fs::create_dir(&sentinel).unwrap();
    fs::write(sentinel.join("keep.bin"), b"unchanged").unwrap();
    let before = fs::read_dir(parent.path())
        .unwrap()
        .map(|entry| entry.unwrap().file_name())
        .collect::<Vec<_>>();

    assert!(matches!(
        prepare_jex_source_archive(&source, parent.path()),
        Err(JexPrepareError::Scan(JexScanError::Io(_)))
    ));
    assert_eq!(fs::read(sentinel.join("keep.bin")).unwrap(), b"unchanged");
    let after = fs::read_dir(parent.path())
        .unwrap()
        .map(|entry| entry.unwrap().file_name())
        .collect::<Vec<_>>();
    assert_eq!(after, before);
}

#[test]
fn raw_item_lookup_preserves_uppercase_source_id_bytes() {
    // Mutation caught: normalizing the source key but accidentally replacing
    // its source ID in the audit record or returning a false verification error.
    let id = "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA";
    let raw = format!("标题\n\n内容\n\nid: {id}\ntype_: 1\n");
    let source = archive(|builder| append(builder, &format!("{id}.md"), raw.as_bytes()));
    let parent = tempdir().unwrap();
    let prepared = prepare_jex_source_archive(&source, parent.path()).unwrap();
    let exact = prepared.raw_item(id).unwrap().unwrap();
    assert_eq!(exact.source_id, id);
    assert_eq!(exact.raw_bytes, raw.as_bytes());
    assert_eq!(
        prepared.raw_item(&id.to_ascii_lowercase()).unwrap(),
        Some(exact)
    );
}

#[test]
fn cancelled_prepare_returns_no_handle_or_child() {
    // Mutation caught: returning a successful owned spool despite cancellation.
    let source = archive(|builder| {
        append(
            builder,
            &format!("{NOTE}.md"),
            format!("id: {NOTE}\ntype_: 1\n").as_bytes(),
        );
    });
    let parent = tempdir().unwrap();
    let cancel = Arc::new(AtomicBool::new(true));
    assert!(matches!(
        prepare_jex_source_archive_with_cancel(&source, parent.path(), cancel),
        Err(JexPrepareError::Cancelled)
    ));
    assert_eq!(fs::read_dir(parent.path()).unwrap().count(), 0);
}
