//! Publishing a verified import staging profile as a new, separate library.
//! The active profile is never opened for writing, merged into or replaced.

use std::{
    collections::BTreeMap,
    fs,
    io::{self, Write},
    path::Path,
    sync::atomic::AtomicBool,
};

use app_lite_core::{
    LibraryRepository, PublishError, library_counts, publish_staged_library,
    publish_staged_library_with_hook, stage_enex_file,
};
use base64::{Engine as _, engine::general_purpose::STANDARD};
use md5::Md5;
use sha2::{Digest, Sha256};
use tempfile::{NamedTempFile, tempdir};

fn resource(bytes: &[u8], mime: &str, filename: &str) -> String {
    format!(
        "<resource><data encoding=\"base64\">{}</data><mime>{mime}</mime><resource-attributes><file-name>{filename}</file-name></resource-attributes></resource>",
        STANDARD.encode(bytes)
    )
}

/// Two notes, two distinct tags (one shared), one image shared by both
/// notes, rich HTML (list, check, link, bold) and two notes sharing one blob.
fn fixture() -> NamedTempFile {
    let image = b"shared image bytes";
    let hash = format!("{:x}", Md5::digest(image));
    let first = format!(
        "<en-note><div><b>粗</b>前<en-media hash=\"{hash}\" type=\"image/png\"/>后</div><ul><li><en-todo checked=\"true\"/>已办</li></ul><div><a href=\"https://example.com\">链接</a></div></en-note>"
    );
    let second =
        format!("<en-note><div><en-media hash=\"{hash}\" type=\"image/png\"/></div></en-note>");
    let xml = format!(
        "<en-export><note><title>第一篇</title><created>20260913T010203Z</created><updated>20260913T040506Z</updated><tag>共同</tag><tag>甲</tag><content><![CDATA[{first}]]></content>{}</note><note><title>第二篇</title><created>20260912T010203Z</created><updated>20260912T040506Z</updated><tag>共同</tag><content><![CDATA[{second}]]></content>{}</note></en-export>",
        resource(image, "image/png", "图.png"),
        resource(image, "image/png", "图2.png"),
    );
    let mut file = NamedTempFile::new().unwrap();
    file.write_all(xml.as_bytes()).unwrap();
    file
}

/// SHA-256 of every regular file under `root`, keyed by relative path.
fn tree_hashes(root: &Path) -> BTreeMap<String, String> {
    let mut out = BTreeMap::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        for entry in fs::read_dir(&dir).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                stack.push(path);
            } else {
                let rel = path.strip_prefix(root).unwrap().display().to_string();
                out.insert(
                    rel,
                    format!("{:x}", Sha256::digest(fs::read(&path).unwrap())),
                );
            }
        }
    }
    out
}

fn active_profile(root: &Path) -> std::path::PathBuf {
    let active = root.join("active");
    fs::create_dir(&active).unwrap();
    let repo = LibraryRepository::open(active.join("library.sqlite")).unwrap();
    drop(repo);
    active
}

#[test]
fn publishes_verified_stage_as_new_library_with_exact_counts_relations_and_hashes() {
    let root = tempdir().unwrap();
    let active = active_profile(root.path());
    let active_before = tree_hashes(&active);
    let staging = root.path().join(".staging");
    fs::create_dir(&staging).unwrap();
    let staged = stage_enex_file(fixture().path(), &staging).unwrap();
    let staged_path = staged.profile_path().to_path_buf();
    let destination = root.path().join("imported-1");

    let published = publish_staged_library(staged, &destination, &AtomicBool::new(false)).unwrap();

    assert_eq!(published.path, destination);
    assert!(!staged_path.exists(), "staging directory moved, not copied");
    let counts = published.counts;
    assert_eq!(counts.notes, 2);
    assert_eq!(counts.trashed_notes, 0);
    assert_eq!(counts.tags, 2);
    assert_eq!(counts.note_tags, 3);
    assert_eq!(counts.resources, 2);
    assert_eq!(
        counts.blobs, 1,
        "same bytes share one content-addressed blob"
    );
    assert_eq!(library_counts(&destination).unwrap(), counts);

    let db = rusqlite::Connection::open(destination.join("library.sqlite")).unwrap();
    let blob = format!("{:x}", Sha256::digest(b"shared image bytes"));
    let stored: Vec<String> = db
        .prepare("SELECT sha256 FROM resource_blobs")
        .unwrap()
        .query_map([], |row| row.get(0))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    assert_eq!(stored, vec![blob]);
    assert_eq!(
        tree_hashes(&active),
        active_before,
        "active profile untouched"
    );
}

#[test]
fn refuses_existing_destination_and_leaves_it_and_active_unchanged() {
    let root = tempdir().unwrap();
    let active = active_profile(root.path());
    let active_before = tree_hashes(&active);
    let staging = root.path().join(".staging");
    fs::create_dir(&staging).unwrap();
    let staged = stage_enex_file(fixture().path(), &staging).unwrap();
    let staged_path = staged.profile_path().to_path_buf();

    let error = publish_staged_library(staged, &active, &AtomicBool::new(false)).unwrap_err();

    assert!(
        matches!(error, PublishError::DestinationExists),
        "{error:?}"
    );
    assert_eq!(tree_hashes(&active), active_before);
    assert!(
        !staged_path.exists(),
        "failed publish removes its staging copy"
    );
}

#[test]
fn cancel_before_publish_creates_nothing() {
    let root = tempdir().unwrap();
    let staging = root.path().join(".staging");
    fs::create_dir(&staging).unwrap();
    let staged = stage_enex_file(fixture().path(), &staging).unwrap();
    let destination = root.path().join("imported-1");

    let error = publish_staged_library(staged, &destination, &AtomicBool::new(true)).unwrap_err();

    assert!(matches!(error, PublishError::Cancelled), "{error:?}");
    assert!(!destination.exists());
    assert_eq!(fs::read_dir(&staging).unwrap().count(), 0);
}

#[test]
fn corrupted_staged_blob_is_rejected_before_publish() {
    let root = tempdir().unwrap();
    let staging = root.path().join(".staging");
    fs::create_dir(&staging).unwrap();
    let staged = stage_enex_file(fixture().path(), &staging).unwrap();
    let blobs = staged.profile_path().join("resources").join("blobs");
    let blob = fs::read_dir(&blobs)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .find(|path| path.is_file())
        .expect("one staged blob");
    fs::write(&blob, b"tampered bytes!!!!").unwrap();
    let destination = root.path().join("imported-1");

    let error = publish_staged_library(staged, &destination, &AtomicBool::new(false)).unwrap_err();

    assert!(
        matches!(error, PublishError::BlobMismatch { .. }),
        "{error:?}"
    );
    assert!(!destination.exists());
}

#[test]
fn failure_just_before_rename_leaves_no_half_published_library() {
    let root = tempdir().unwrap();
    let active = active_profile(root.path());
    let active_before = tree_hashes(&active);
    let staging = root.path().join(".staging");
    fs::create_dir(&staging).unwrap();
    let staged = stage_enex_file(fixture().path(), &staging).unwrap();
    let destination = root.path().join("imported-1");

    let error =
        publish_staged_library_with_hook(staged, &destination, &AtomicBool::new(false), || {
            Err(io::Error::other("injected disk full"))
        })
        .unwrap_err();

    assert!(matches!(error, PublishError::Io(_)), "{error:?}");
    assert!(!destination.exists());
    assert_eq!(fs::read_dir(&staging).unwrap().count(), 0);
    assert_eq!(tree_hashes(&active), active_before);
}

#[test]
fn import_library_file_stages_and_publishes_enex_into_a_new_named_library() {
    let root = tempdir().unwrap();
    let imports = root.path().join("imported-libraries");
    let source_dir = tempdir().unwrap();
    let source = source_dir.path().join("我的笔记.enex");
    fs::copy(fixture().path(), &source).unwrap();
    let source_before = fs::read(&source).unwrap();

    let first = app_lite_core::import_library_file(
        &source,
        &imports,
        &std::sync::Arc::new(AtomicBool::new(false)),
    )
    .unwrap();
    let second = app_lite_core::import_library_file(
        &source,
        &imports,
        &std::sync::Arc::new(AtomicBool::new(false)),
    )
    .unwrap();

    assert_eq!(
        fs::read(&source).unwrap(),
        source_before,
        "source is read-only"
    );
    assert_ne!(
        first.library.path, second.library.path,
        "never reuses a target"
    );
    for outcome in [&first, &second] {
        assert!(outcome.library.path.starts_with(&imports));
        assert!(
            outcome
                .library
                .path
                .file_name()
                .unwrap()
                .to_string_lossy()
                .starts_with("我的笔记-")
        );
        assert_eq!(outcome.library.counts.notes, 2);
        assert!(outcome.degraded.is_empty());
    }
    let leftovers: Vec<_> = fs::read_dir(imports.join(".staging")).unwrap().collect();
    assert!(leftovers.is_empty(), "no staging leftovers");
}

#[test]
fn import_library_file_rejects_unknown_extension_without_creating_a_library() {
    let root = tempdir().unwrap();
    let imports = root.path().join("imported-libraries");
    let source = root.path().join("notes.txt");
    fs::write(&source, b"x").unwrap();

    let error = app_lite_core::import_library_file(
        &source,
        &imports,
        &std::sync::Arc::new(AtomicBool::new(false)),
    )
    .unwrap_err();

    assert!(
        matches!(error, app_lite_core::ImportLibraryError::UnsupportedFormat),
        "{error:?}"
    );
    assert!(
        !imports.exists()
            || fs::read_dir(&imports)
                .unwrap()
                .all(|e| e.unwrap().file_name() == ".staging")
    );
}

#[test]
fn cancelled_jex_309_does_not_even_read_a_broken_archive() {
    // Break caught: checking the cancel flag only after JEX staging.
    let root = tempdir().unwrap();
    let source = root.path().join("broken.jex");
    fs::write(&source, b"not a tar archive").unwrap();
    let imports = root.path().join("imports");
    let result = app_lite_core::import_library_file(
        &source, &imports, &std::sync::Arc::new(AtomicBool::new(true)),
    );
    assert!(matches!(result, Err(app_lite_core::ImportLibraryError::Publish(PublishError::Cancelled))), "{result:?}");
    assert!(!imports.exists(), "a cancelled job must not begin spooling");
    assert_eq!(fs::read(&source).unwrap(), b"not a tar archive");
}

#[test]
fn cancelled_jex_309_stops_during_spool_without_building_a_repository() {
    // Break caught: losing the shared GUI cancellation flag in the JEX branch.
    // Real tar, files, worker and TempDir cleanup; no importer mocks or hooks.
    use std::sync::{Arc, atomic::Ordering};
    use std::time::{Duration, Instant};
    let root = tempdir().unwrap();
    let active = active_profile(root.path());
    let active_before = tree_hashes(&active);
    let source = root.path().join("cancel-working.jex");
    let mut builder = tar::Builder::new(fs::File::create(&source).unwrap());
    let mut append = |name: &str, bytes: &[u8]| {
        let mut header = tar::Header::new_gnu();
        header.set_mode(0o600);
        header.set_size(bytes.len() as u64);
        header.set_cksum();
        builder.append_data(&mut header, name, std::io::Cursor::new(bytes)).unwrap();
    };
    let resource_id = "33333333333333333333333333333333";
    append(&format!("resources/{resource_id}.bin"), &vec![0x71; 32 * 1024 * 1024]);
    append(&format!("{resource_id}.md"), format!("fixture.bin\n\nid: {resource_id}\ntype_: 4\nmime: application/octet-stream\nfile_extension: bin\n").as_bytes());
    for index in 1..=32 {
        let id = format!("{index:032x}");
        append(&format!("{id}.md"), format!("cancel fixture {index}\n\n正文 [file](:/{resource_id})\n\nid: {id}\ntype_: 1\nparent_id: \nmarkup_language: 1\ncreated_time: 2026-09-13T01:02:03.000Z\nupdated_time: 2026-09-13T01:02:03.000Z\nuser_created_time: 2026-09-13T01:02:03.000Z\nuser_updated_time: 2026-09-13T01:02:03.000Z\n").as_bytes());
    }
    builder.finish().unwrap();
    drop(builder);
    let source_before = format!("{:x}", Sha256::digest(fs::read(&source).unwrap()));
    let imports = root.path().join("imports");
    let cancel = Arc::new(AtomicBool::new(false));
    let worker_source = source.clone();
    let worker_imports = imports.clone();
    let worker_cancel = cancel.clone();
    let worker = std::thread::spawn(move || app_lite_core::import_library_file(
        &worker_source, &worker_imports, &worker_cancel,
    ));
    let deadline = Instant::now() + Duration::from_secs(30);
    let mut cancelled_during_spool = false;
    let mut built_repository_after_cancel = false;
    while !worker.is_finished() && Instant::now() < deadline {
        if let Ok(entries) = fs::read_dir(imports.join(".staging")) {
            for entry in entries.flatten() {
                let name = entry.file_name().to_string_lossy().into_owned();
                if !cancelled_during_spool && name.starts_with("jex-source-") {
                    let resources = entry.path().join("resources");
                    if fs::read_dir(resources).is_ok_and(|mut files| files.next().is_some()) {
                        cancel.store(true, Ordering::Relaxed);
                        cancelled_during_spool = true;
                    }
                }
                if cancelled_during_spool && name.starts_with("jex-stage-") {
                    built_repository_after_cancel = true;
                }
            }
        }
        std::thread::sleep(Duration::from_millis(1));
    }
    cancel.store(true, Ordering::Relaxed);
    let result = worker.join().unwrap();
    assert!(cancelled_during_spool, "must exercise real work, not pre-cancellation: {result:?}");
    assert!(!built_repository_after_cancel, "JEX continued into repository creation after cancellation");
    assert!(matches!(result, Err(app_lite_core::ImportLibraryError::Publish(PublishError::Cancelled))), "{result:?}");
    assert!(fs::read_dir(imports.join(".staging")).unwrap().next().is_none(), "owned temporary spool removed");
    assert!(fs::read_dir(&imports).unwrap().all(|e| e.unwrap().file_name() == ".staging"), "nothing published");
    assert_eq!(tree_hashes(&active), active_before);
    assert_eq!(format!("{:x}", Sha256::digest(fs::read(&source).unwrap())), source_before);
}
