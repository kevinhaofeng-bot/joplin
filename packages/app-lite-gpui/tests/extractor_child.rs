use app_lite_core::document::Block;
use app_lite_core::{
    BlobHash, CanonicalDocument, CreateNote, DerivedTextFailure, DerivedTextStatus,
    LibraryRepository, SearchQuery,
};
use std::io::Write;
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};
#[path = "../src/extractor.rs"]
mod extractor;

fn child(mime: &str, input: &[u8]) -> std::process::Output {
    let mut command = Command::new(env!("CARGO_BIN_EXE_velotype"));
    let mut process = command
        .args(["--extract-resource-text", "--mime", mime])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    process.stdin.take().unwrap().write_all(input).unwrap();
    process.wait_with_output().unwrap()
}

// Match the production descriptor handoff. An unsupported MIME exits before
// reading stdin, so a large fixture sent through a writer pipe can fail in the
// harness with BrokenPipe instead of exposing the child's actual rejection.
fn child_from_fixture_descriptor(mime: &str, input: &[u8]) -> std::process::Output {
    let mut file = tempfile::NamedTempFile::new().unwrap();
    file.write_all(input).unwrap();
    file.flush().unwrap();
    Command::new(env!("CARGO_BIN_EXE_velotype"))
        .args(["--extract-resource-text", "--mime", mime])
        .stdin(Stdio::from(std::fs::File::open(file.path()).unwrap()))
        .output()
        .unwrap()
}

#[test]
fn plain_text_child_preserves_utf8_chinese_and_strips_only_the_bom() {
    let text = "TextNeedle164\n中文附件唯一词\t末行\r\n";
    for input in [
        text.as_bytes().to_vec(),
        [b"\xef\xbb\xbf".as_slice(), text.as_bytes()].concat(),
    ] {
        let output = child("text/plain", &input);
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert_eq!(String::from_utf8(output.stdout).unwrap(), text);
    }
}

#[test]
fn plain_text_child_rejects_invalid_utf8_binary_and_oversize_content() {
    for (input, marker) in [
        (vec![0xff, 0xfe, 0x41], "text-invalid-utf8"),
        (b"not\0text".to_vec(), "text-binary-input"),
        (vec![b'a'; 1024 * 1024 + 1], "input-too-large"),
    ] {
        let output = child("text/plain", &input);
        assert!(!output.status.success());
        assert!(output.stdout.is_empty());
        assert!(
            String::from_utf8_lossy(&output.stderr).contains(marker),
            "expected {marker}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
}

#[test]
fn plain_text_child_accepts_the_exact_byte_budget() {
    let input = vec![b'x'; 1024 * 1024];
    let output = child("text/plain", &input);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(output.stdout, input);
}

#[test]
fn plain_text_coordinator_indexes_generic_txt_without_changing_the_resource_and_reopens() {
    for (mime, extension, title) in [
        ("application/octet-stream", "txt", "opaque-name"),
        ("application/octet-stream", "bin", "fallback.TXT"),
        ("text/plain", "txt", "explicit.txt"),
    ] {
        let profile = tempfile::tempdir().unwrap();
        let db = profile.path().join("library.sqlite");
        let repository = LibraryRepository::open(&db).unwrap();
        let bytes = "TextNeedle164\n中文附件唯一词".as_bytes();
        let (resource, note) = associated_resource(&repository, bytes, title, mime, extension);
        let before = repository.resource_metadata(&resource).unwrap().unwrap();
        assert!(
            repository
                .search(SearchQuery::parse("TextNeedle164"))
                .unwrap()
                .is_empty()
        );
        assert_eq!(
            extractor::run_one_derived_text_pdf_job_with_exe(
                &repository,
                std::path::PathBuf::from(env!("CARGO_BIN_EXE_velotype"))
            )
            .unwrap(),
            extractor::DerivedTextCoordinatorOutcome::Indexed(resource.clone())
        );
        let after = repository.resource_metadata(&resource).unwrap().unwrap();
        assert_eq!(before.sha256, after.sha256);
        assert_eq!(before.mime, after.mime);
        assert_eq!(before.title, after.title);
        assert_eq!(
            repository.read_resource_bytes(&resource).unwrap().unwrap(),
            bytes
        );
        drop(repository);
        let reopened = LibraryRepository::open(&db).unwrap();
        for term in ["TextNeedle164", "中文附件唯一词"] {
            let hits = reopened.search(SearchQuery::parse(term)).unwrap();
            assert_eq!(hits.len(), 1, "{term} for {mime}/{extension}/{title}");
            assert_eq!(hits[0].note.id, note.id);
            assert_eq!(hits[0].matched_resource, Some(resource.clone()));
        }
    }
}

#[test]
fn plain_text_coordinator_preserves_known_mime_priority_and_rejects_large_txt_before_io() {
    for (bytes, mime, title, extension, failure) in [
        (
            b"TextNeedle164".to_vec(),
            "application/zip",
            "not-text.txt",
            "txt",
            None,
        ),
        (
            b"TextNeedle164".to_vec(),
            "application/octet-stream",
            "unknown.bin",
            "bin",
            None,
        ),
        (
            vec![b'a'; 1024 * 1024 + 1],
            "application/octet-stream",
            "large.txt",
            "txt",
            Some(DerivedTextFailure::TooLarge),
        ),
    ] {
        let profile = tempfile::tempdir().unwrap();
        let repository = LibraryRepository::open(profile.path().join("library.sqlite")).unwrap();
        let (resource, _) = associated_resource(&repository, &bytes, title, mime, extension);
        let opens = repository.observe_verified_resource_opens();
        let outcome = extractor::run_one_derived_text_pdf_job_with_exe(
            &repository,
            std::path::PathBuf::from(env!("CARGO_BIN_EXE_velotype")),
        )
        .unwrap();
        if let Some(failure) = failure {
            assert_eq!(
                outcome,
                extractor::DerivedTextCoordinatorOutcome::Failed(resource.clone(), failure.clone())
            );
            assert_eq!(
                repository.derived_text_status(&resource).unwrap(),
                Some(DerivedTextStatus::Failed {
                    failure,
                    attempts: 1
                })
            );
        } else {
            assert_eq!(outcome, extractor::DerivedTextCoordinatorOutcome::Idle);
            assert_eq!(repository.derived_text_status(&resource).unwrap(), None);
        }
        assert!(matches!(
            opens.recv_timeout(Duration::from_millis(20)),
            Err(std::sync::mpsc::RecvTimeoutError::Timeout)
        ));
        assert!(
            repository
                .search(SearchQuery::parse("TextNeedle164"))
                .unwrap()
                .is_empty()
        );
    }
}

#[test]
fn plain_text_coordinator_records_parse_failure_without_publishing_binary_text() {
    for bytes in [vec![0xff, 0xfe, 0x41], b"TextNeedle164\0hidden".to_vec()] {
        let profile = tempfile::tempdir().unwrap();
        let repository = LibraryRepository::open(profile.path().join("library.sqlite")).unwrap();
        let (resource, _) = associated_resource(
            &repository,
            &bytes,
            "bad.txt",
            "application/octet-stream",
            "txt",
        );
        assert_eq!(
            extractor::run_one_derived_text_pdf_job_with_exe(
                &repository,
                std::path::PathBuf::from(env!("CARGO_BIN_EXE_velotype"))
            )
            .unwrap(),
            extractor::DerivedTextCoordinatorOutcome::Failed(
                resource.clone(),
                DerivedTextFailure::Parse
            )
        );
        assert_eq!(
            repository.derived_text_status(&resource).unwrap(),
            Some(DerivedTextStatus::Failed {
                failure: DerivedTextFailure::Parse,
                attempts: 1
            })
        );
        assert!(
            repository
                .search(SearchQuery::parse("TextNeedle164"))
                .unwrap()
                .is_empty()
        );
    }
}

#[test]
fn plain_text_coordinator_cancellation_preserves_the_job_for_retry() {
    let profile = tempfile::tempdir().unwrap();
    let repository = LibraryRepository::open(profile.path().join("library.sqlite")).unwrap();
    let (resource, _) = associated_resource(
        &repository,
        b"TextNeedle164",
        "pending.txt",
        "application/octet-stream",
        "txt",
    );
    let cancelled = AtomicBool::new(true);
    assert_eq!(
        extractor::run_one_derived_text_pdf_job_with_exe_and_cancellation(
            &repository,
            std::path::PathBuf::from(env!("CARGO_BIN_EXE_velotype")),
            &cancelled
        )
        .unwrap(),
        extractor::DerivedTextCoordinatorOutcome::Cancelled
    );
    assert_eq!(
        repository.derived_text_status(&resource).unwrap(),
        Some(DerivedTextStatus::Pending { attempts: 0 })
    );
    cancelled.store(false, Ordering::Release);
    assert_eq!(
        extractor::run_one_derived_text_pdf_job_with_exe_and_cancellation(
            &repository,
            std::path::PathBuf::from(env!("CARGO_BIN_EXE_velotype")),
            &cancelled
        )
        .unwrap(),
        extractor::DerivedTextCoordinatorOutcome::Indexed(resource)
    );
}

#[test]
fn pdf_child_extracts_the_static_english_and_chinese_fixture() {
    let output = child(
        "application/pdf",
        include_bytes!("resources/extractor-fixture.pdf"),
    );
    assert!(
        output.status.success(),
        "{:?}",
        String::from_utf8_lossy(&output.stderr)
    );
    let text = String::from_utf8(output.stdout).unwrap();
    assert!(text.contains("English text"));
    assert!(text.contains("中文可选文字检索"));
}

#[test]
fn resource_text_child_rejects_unsupported_mime_and_corrupt_input() {
    let mime = child("application/octet-stream", b"not used");
    assert!(!mime.status.success());
    assert!(String::from_utf8_lossy(&mime.stderr).contains("unsupported-mime"));
    let corrupt = child("application/pdf", b"not a PDF");
    assert!(!corrupt.status.success());
    assert!(String::from_utf8_lossy(&corrupt.stderr).contains("pdf-parse-failed"));
}

#[test]
fn child_rejects_unknown_mime_without_waiting_for_stdin_eof() {
    let mut process = Command::new(env!("CARGO_BIN_EXE_velotype"))
        .args(["--extract-resource-text", "--mime", "image/gif"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("start child with an open stdin pipe");
    let _stdin = process.stdin.take().expect("hold stdin open");
    // Match the production runner's 15-second process budget. A debug
    // executable can take >1s to start alongside Vision/PDF workers; keep
    // stdin open to test EOF independence, not dynamic-loader throughput.
    let deadline = Instant::now() + Duration::from_secs(15);
    let status = loop {
        if let Some(status) = process.try_wait().expect("poll child") {
            break status;
        }
        if Instant::now() >= deadline {
            let _ = process.kill();
            let _ = process.wait();
            panic!("unsupported MIME child did not exit with stdin open within the process budget");
        }
        std::thread::sleep(Duration::from_millis(10));
    };
    assert!(!status.success());
    let output = process.wait_with_output().unwrap();
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("unsupported-mime"),
        "the child must reject MIME, not fail for an unrelated startup error"
    );
}

#[test]
fn image_child_extracts_real_english_and_chinese_fixtures() {
    for fixture in [
        (
            "ocr-english.png",
            "image/png",
            include_bytes!("resources/ocr-english.png").as_slice(),
            "Vision OCR English",
        ),
        (
            "ocr-chinese.png",
            "image/png",
            include_bytes!("resources/ocr-chinese.png").as_slice(),
            "中文视觉文字识别",
        ),
        (
            "ocr-english.jpg",
            "image/jpeg",
            include_bytes!("resources/ocr-english.jpg").as_slice(),
            "Vision OCR English",
        ),
    ] {
        let output = child(fixture.1, fixture.2);
        assert!(
            output.status.success(),
            "{}: {}",
            fixture.0,
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(
            String::from_utf8_lossy(&output.stdout).contains(fixture.3),
            "{}: {}",
            fixture.0,
            String::from_utf8_lossy(&output.stdout)
        );
    }
}

#[test]
fn image_child_rejects_corrupt_and_oversize_input() {
    let corrupt = child("image/png", b"not an image");
    assert!(!corrupt.status.success());
    assert!(String::from_utf8_lossy(&corrupt.stderr).contains("joplin-lite-extractor:image-"));
    let output = child("image/jpeg", &vec![b'x'; 20 * 1024 * 1024 + 1]);
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("input-too-large"));
}

#[test]
fn tiff_child_extracts_real_clipboard_bitmap_without_changing_the_payload() {
    let output = child_from_fixture_descriptor(
        "image/tiff",
        include_bytes!("resources/ocr-clipboard-tiff.tiff"),
    );
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let text = String::from_utf8(output.stdout).unwrap();
    assert!(text.contains("Colour fidelity"), "{text}");
}

#[test]
fn tiff_coordinator_classifies_indexes_and_reopens_the_original_resource() {
    let profile = tempfile::tempdir().unwrap();
    let db = profile.path().join("library.sqlite");
    let repository = LibraryRepository::open(&db).unwrap();
    let bytes = include_bytes!("resources/ocr-clipboard-tiff.tiff");
    let (resource, note) =
        associated_resource(&repository, bytes, "clipboard.tiff", "image/tiff", "tiff");
    assert_eq!(
        extractor::next_derived_text_work_kind(&repository).unwrap(),
        Some(extractor::DerivedTextWorkKind::Image),
        "TIFF must use the same background image pacing as PNG/JPEG"
    );
    let before = repository.resource_metadata(&resource).unwrap().unwrap();
    assert_eq!(
        extractor::run_one_derived_text_pdf_job_with_exe(
            &repository,
            std::path::PathBuf::from(env!("CARGO_BIN_EXE_velotype")),
        )
        .unwrap(),
        extractor::DerivedTextCoordinatorOutcome::Indexed(resource.clone())
    );
    assert_eq!(
        repository.read_resource_bytes(&resource).unwrap().unwrap(),
        bytes
    );
    let after = repository.resource_metadata(&resource).unwrap().unwrap();
    assert_eq!(
        (after.sha256, after.mime, after.file_extension),
        (before.sha256, before.mime, before.file_extension)
    );
    drop(repository);
    let reopened = LibraryRepository::open(&db).unwrap();
    let hits = reopened
        .search(SearchQuery::parse("\"Colour fidelity\""))
        .unwrap();
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].note.id, note.id);
    assert_eq!(hits[0].matched_resource, Some(resource));
}

#[test]
fn tiff_child_rejects_mislabeled_and_corrupt_images_before_publishing_text() {
    for bytes in [
        include_bytes!("resources/ocr-english.png").as_slice(),
        b"not a TIFF".as_slice(),
    ] {
        let output = child_from_fixture_descriptor("image/tiff", bytes);
        assert!(!output.status.success());
        assert!(output.stdout.is_empty());
        // ImageIO may provide a source with an unknown UTI rather than nil
        // for corrupt bytes. Both decode failure and type mismatch are Parse
        // at the production caller; neither is Unsupported or valid OCR.
        let mut file = tempfile::NamedTempFile::new().unwrap();
        file.write_all(bytes).unwrap();
        file.flush().unwrap();
        assert_eq!(
            extractor::run_resource_child_for_verified_file_with_exe_and_cancellation(
                std::fs::File::open(file.path()).unwrap(),
                bytes.len() as i64,
                "image/tiff",
                std::path::PathBuf::from(env!("CARGO_BIN_EXE_velotype")),
                &AtomicBool::new(false),
            ),
            Err(extractor::PdfChildError::Parse)
        );
    }
}

fn fail_fixture_job(
    repository: &LibraryRepository,
    resource: &app_lite_core::ResourceId,
    failure: DerivedTextFailure,
) -> app_lite_core::DerivedTextJob {
    let job = repository
        .take_derived_text_jobs(100)
        .unwrap()
        .into_iter()
        .find(|job| &job.resource_id == resource)
        .expect("live fixture job");
    assert!(repository.fail_derived_text(&job, failure).unwrap());
    job
}

#[test]
fn tiff_capability_recovery_indexes_an_old_failure_and_keeps_the_png_projection() {
    let profile = tempfile::tempdir().unwrap();
    let db = profile.path().join("library.sqlite");
    let repository = LibraryRepository::open(&db).unwrap();
    let (png, png_note) = associated_resource(
        &repository,
        include_bytes!("resources/ocr-english.png"),
        "kept.png",
        "image/png",
        "png",
    );
    let png_job = repository.take_derived_text_jobs(1).unwrap().pop().unwrap();
    assert!(
        repository
            .publish_derived_text(&png_job, "KeptPNGProjection269")
            .unwrap()
    );
    let bytes = include_bytes!("resources/ocr-clipboard-tiff.tiff");
    let (tiff, note) = associated_resource(&repository, bytes, "old.tiff", "image/tiff", "tiff");
    fail_fixture_job(&repository, &tiff, DerivedTextFailure::Unsupported);
    drop(repository);
    let reopened = LibraryRepository::open(&db).unwrap();
    let verified_opens = reopened.observe_verified_resource_opens();
    assert_eq!(
        extractor::next_derived_text_work_kind(&reopened).unwrap(),
        Some(extractor::DerivedTextWorkKind::Image),
        "new TIFF capability must recover the old unsupported identity"
    );
    assert!(
        verified_opens.try_recv().is_err(),
        "capability recovery may inspect metadata, not the blob"
    );
    assert_eq!(
        reopened.derived_text_status(&tiff).unwrap(),
        Some(DerivedTextStatus::Pending { attempts: 1 })
    );
    assert_eq!(
        extractor::run_one_derived_text_pdf_job_with_exe(
            &reopened,
            std::path::PathBuf::from(env!("CARGO_BIN_EXE_velotype"))
        )
        .unwrap(),
        extractor::DerivedTextCoordinatorOutcome::Indexed(tiff.clone())
    );
    assert_eq!(reopened.read_resource_bytes(&tiff).unwrap().unwrap(), bytes);
    let hits = reopened
        .search(SearchQuery::parse("\"Colour fidelity\""))
        .unwrap();
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].note.id, note.id);
    assert_eq!(hits[0].matched_resource, Some(tiff.clone()));
    assert_eq!(
        reopened.derived_text_status(&png).unwrap(),
        Some(DerivedTextStatus::Indexed { attempts: 0 })
    );
    assert_eq!(
        reopened
            .search(SearchQuery::parse("KeptPNGProjection269"))
            .unwrap()[0]
            .note
            .id,
        png_note.id
    );
    drop(reopened);
    let again = LibraryRepository::open(&db).unwrap();
    assert_eq!(
        extractor::next_derived_text_work_kind(&again).unwrap(),
        None
    );
    assert_eq!(
        again.derived_text_status(&tiff).unwrap(),
        Some(DerivedTextStatus::Indexed { attempts: 1 })
    );
}

#[test]
fn tiff_capability_recovery_is_once_only_and_does_not_retry_parse_or_gif() {
    let profile = tempfile::tempdir().unwrap();
    let repository = LibraryRepository::open(profile.path().join("library.sqlite")).unwrap();
    let bytes = include_bytes!("resources/ocr-clipboard-tiff.tiff");
    let (tiff, _) = associated_resource(&repository, bytes, "once.tiff", "image/tiff", "tiff");
    fail_fixture_job(&repository, &tiff, DerivedTextFailure::Unsupported);
    let (bad, _) = associated_resource(&repository, b"corrupt", "bad.tiff", "image/tiff", "tiff");
    fail_fixture_job(&repository, &bad, DerivedTextFailure::Parse);
    let (gif, _) = associated_resource(&repository, b"gif", "other.gif", "image/gif", "gif");
    fail_fixture_job(&repository, &gif, DerivedTextFailure::Unsupported);
    assert_eq!(
        extractor::next_derived_text_work_kind(&repository).unwrap(),
        Some(extractor::DerivedTextWorkKind::Image)
    );
    // A newly supported MIME can still fail. A committed capability marker
    // must not turn that deterministic failure into a scheduler retry loop.
    fail_fixture_job(&repository, &tiff, DerivedTextFailure::Unsupported);
    assert_eq!(
        extractor::next_derived_text_work_kind(&repository).unwrap(),
        None
    );
    for (resource, failure, attempts) in [
        (tiff, DerivedTextFailure::Unsupported, 2),
        (bad, DerivedTextFailure::Parse, 1),
        (gif, DerivedTextFailure::Unsupported, 1),
    ] {
        assert_eq!(
            repository.derived_text_status(&resource).unwrap(),
            Some(DerivedTextStatus::Failed { failure, attempts })
        );
    }
}

#[test]
fn tiff_capability_recovery_parks_detached_jobs_and_rejects_stale_identities() {
    let profile = tempfile::tempdir().unwrap();
    let db = profile.path().join("library.sqlite");
    let repository = LibraryRepository::open(&db).unwrap();
    let bytes = include_bytes!("resources/ocr-clipboard-tiff.tiff");
    let (detached, note) =
        associated_resource(&repository, bytes, "detached.tiff", "image/tiff", "tiff");
    fail_fixture_job(&repository, &detached, DerivedTextFailure::Unsupported);
    let empty = repository
        .save_note(app_lite_core::SaveNote {
            id: note.id.clone(),
            expected_revision: note.revision,
            title: note.title,
            document: CanonicalDocument::default(),
            resource_ids: vec![],
            selected_thumbnail_id: None,
        })
        .unwrap();
    let mut stale = Vec::new();
    for label in ["hash", "version", "deleted"] {
        let (resource, _) = associated_resource(
            &repository,
            bytes,
            &format!("{label}.tiff"),
            "image/tiff",
            "tiff",
        );
        fail_fixture_job(&repository, &resource, DerivedTextFailure::Unsupported);
        stale.push(resource);
    }
    // Corrupt only disposable fixture metadata, never a mounted/user library.
    let connection = rusqlite::Connection::open(&db).unwrap();
    connection
        .execute(
            "UPDATE derived_text_jobs SET sha256=?2 WHERE resource_id=?1",
            rusqlite::params![stale[0].as_str(), "0".repeat(64)],
        )
        .unwrap();
    connection.execute("UPDATE derived_text_jobs SET extractor_version='obsolete-fixture' WHERE resource_id=?1", [stale[1].as_str()]).unwrap();
    connection
        .execute(
            "UPDATE resources SET deleted_time=1 WHERE id=?1",
            [stale[2].as_str()],
        )
        .unwrap();
    drop(connection);
    assert_eq!(
        extractor::next_derived_text_work_kind(&repository).unwrap(),
        None
    );
    assert_eq!(
        repository.derived_text_status(&detached).unwrap(),
        Some(DerivedTextStatus::Pending { attempts: 1 }),
        "history-only data must park until it becomes a live attachment again"
    );
    for resource in stale {
        assert_eq!(
            repository.derived_text_status(&resource).unwrap(),
            Some(DerivedTextStatus::Failed {
                failure: DerivedTextFailure::Unsupported,
                attempts: 1
            })
        );
    }
    assert!(
        repository
            .search(SearchQuery::parse("\"Colour fidelity\""))
            .unwrap()
            .is_empty()
    );
    repository
        .save_note(app_lite_core::SaveNote {
            id: empty.id,
            expected_revision: empty.revision,
            title: empty.title,
            document: CanonicalDocument::from_blocks(vec![Block::Attachment {
                resource_id: detached.clone(),
                filename: "detached.tiff".into(),
                media_type: "image/tiff".into(),
            }]),
            resource_ids: vec![detached.clone()],
            selected_thumbnail_id: None,
        })
        .unwrap();
    assert_eq!(
        extractor::next_derived_text_work_kind(&repository).unwrap(),
        Some(extractor::DerivedTextWorkKind::Image)
    );
    assert_eq!(
        extractor::run_one_derived_text_pdf_job_with_exe(
            &repository,
            std::path::PathBuf::from(env!("CARGO_BIN_EXE_velotype"))
        )
        .unwrap(),
        extractor::DerivedTextCoordinatorOutcome::Indexed(detached)
    );
}

#[test]
fn verified_file_runner_executes_the_real_child_without_reading_a_profile_path() {
    let file = std::fs::File::open(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/resources/extractor-fixture.pdf"
    ))
    .unwrap();
    let text = extractor::run_pdf_child_for_verified_file_with_exe(
        file,
        14890,
        std::path::PathBuf::from(env!("CARGO_BIN_EXE_velotype")),
    )
    .unwrap();
    assert!(text.contains("English text") && text.contains("中文可选文字检索"));
}

#[test]
fn verified_file_runner_rejects_oversize_before_spawning_and_bad_pdf_after_child_exit() {
    let fixture = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/resources/extractor-fixture.pdf"
    );
    assert_eq!(
        extractor::run_pdf_child_for_verified_file_with_exe(
            std::fs::File::open(fixture).unwrap(),
            20 * 1024 * 1024 + 1,
            std::path::PathBuf::from(env!("CARGO_BIN_EXE_velotype"))
        ),
        Err(extractor::PdfChildError::TooLarge)
    );
    let bad = tempfile::NamedTempFile::new().unwrap();
    std::fs::write(bad.path(), b"not a PDF").unwrap();
    assert_eq!(
        extractor::run_pdf_child_for_verified_file_with_exe(
            std::fs::File::open(bad.path()).unwrap(),
            9,
            std::path::PathBuf::from(env!("CARGO_BIN_EXE_velotype"))
        ),
        Err(extractor::PdfChildError::Parse)
    );
}

#[cfg(unix)]
fn sleeping_child_script() -> (tempfile::TempDir, std::path::PathBuf, std::path::PathBuf) {
    use std::os::unix::fs::PermissionsExt;

    let directory = tempfile::tempdir().unwrap();
    let executable = directory.path().join("sleeping-child.sh");
    let pid_file = directory.path().join("child.pid");
    std::fs::write(
        &executable,
        format!(
            "#!/bin/sh\nprintf '%s' \"$$\" > '{}'\nexec sleep 30\n",
            pid_file.display()
        ),
    )
    .unwrap();
    std::fs::set_permissions(&executable, std::fs::Permissions::from_mode(0o755)).unwrap();
    (directory, executable, pid_file)
}

#[cfg(unix)]
fn wait_for_file(path: &std::path::Path) {
    let deadline = Instant::now() + Duration::from_secs(5);
    while !path.exists() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(10));
    }
    assert!(path.exists(), "child did not record its pid");
}

#[cfg(unix)]
fn process_is_alive(pid: &str) -> bool {
    Command::new("kill")
        .args(["-0", pid.trim()])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|status| status.success())
}

#[cfg(unix)]
fn assert_child_is_reaped(pid_file: &std::path::Path) {
    let pid = std::fs::read_to_string(pid_file).unwrap();
    let deadline = Instant::now() + Duration::from_secs(1);
    while process_is_alive(&pid) && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(10));
    }
    assert!(!process_is_alive(&pid), "cancelled child must be reaped");
}

#[cfg(unix)]
#[test]
fn cancellable_verified_file_runner_kills_and_reaps_a_running_child() {
    let (_directory, executable, pid_file) = sleeping_child_script();
    let cancelled = Arc::new(AtomicBool::new(false));
    let runner_cancelled = Arc::clone(&cancelled);
    let fixture = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/resources/extractor-fixture.pdf"
    );
    let runner = std::thread::spawn(move || {
        extractor::run_pdf_child_for_verified_file_with_exe_and_cancellation(
            std::fs::File::open(fixture).unwrap(),
            14890,
            executable,
            &runner_cancelled,
        )
    });
    wait_for_file(&pid_file);
    let cancelled_at = Instant::now();
    cancelled.store(true, Ordering::Release);
    assert_eq!(
        runner.join().unwrap(),
        Err(extractor::PdfChildError::Cancelled)
    );
    assert!(
        cancelled_at.elapsed() < Duration::from_secs(2),
        "cancellation must not wait for the child timeout"
    );
    assert_child_is_reaped(&pid_file);
}

fn associated_resource(
    repository: &LibraryRepository,
    bytes: &[u8],
    title: &str,
    mime: &str,
    extension: &str,
) -> (app_lite_core::ResourceId, app_lite_core::Note) {
    let resource = repository
        .import_resource(bytes, title, mime, extension)
        .expect("import durable fixture resource");
    let note = repository
        .create_note(CreateNote {
            title: "PDF extraction owner".into(),
            notebook_id: None,
            document: CanonicalDocument::from_blocks(vec![Block::Attachment {
                resource_id: resource.clone(),
                filename: title.into(),
                media_type: mime.into(),
            }]),
        })
        .expect("associate resource with live note");
    (resource, note)
}

#[test]
fn cancelled_coordinator_leaves_the_durable_job_pending_without_spawning() {
    let profile = tempfile::tempdir().unwrap();
    let repository = LibraryRepository::open(profile.path().join("library.sqlite")).unwrap();
    let (resource, _) = associated_resource(
        &repository,
        include_bytes!("resources/extractor-fixture.pdf"),
        "pending.pdf",
        "application/pdf",
        "pdf",
    );
    let cancelled = AtomicBool::new(true);

    assert_eq!(
        extractor::run_one_derived_text_pdf_job_with_exe_and_cancellation(
            &repository,
            std::path::PathBuf::from("must-not-spawn"),
            &cancelled,
        )
        .unwrap(),
        extractor::DerivedTextCoordinatorOutcome::Cancelled
    );
    assert_eq!(
        repository.derived_text_status(&resource).unwrap(),
        Some(DerivedTextStatus::Pending { attempts: 0 })
    );
}

#[cfg(unix)]
#[test]
fn cancelling_a_running_coordinator_reaps_the_child_and_keeps_the_job_pending() {
    let (_directory, executable, pid_file) = sleeping_child_script();
    let profile = tempfile::tempdir().unwrap();
    let repository =
        Arc::new(LibraryRepository::open(profile.path().join("library.sqlite")).unwrap());
    let (resource, _) = associated_resource(
        &repository,
        include_bytes!("resources/extractor-fixture.pdf"),
        "running.pdf",
        "application/pdf",
        "pdf",
    );
    let job = repository.take_derived_text_jobs(1).unwrap().pop().unwrap();
    let cancelled = Arc::new(AtomicBool::new(false));
    let runner_repository = Arc::clone(&repository);
    let runner_cancelled = Arc::clone(&cancelled);
    let runner = std::thread::spawn(move || {
        extractor::run_derived_text_pdf_job_with_exe_and_cancellation(
            &runner_repository,
            job,
            executable,
            &runner_cancelled,
        )
    });
    wait_for_file(&pid_file);
    let cancelled_at = Instant::now();
    cancelled.store(true, Ordering::Release);
    assert_eq!(
        runner.join().unwrap().unwrap(),
        extractor::DerivedTextCoordinatorOutcome::Cancelled
    );
    assert!(
        cancelled_at.elapsed() < Duration::from_secs(2),
        "coordinator cancellation must not wait for the child timeout"
    );
    assert_child_is_reaped(&pid_file);
    assert_eq!(
        repository.derived_text_status(&resource).unwrap(),
        Some(DerivedTextStatus::Pending { attempts: 0 }),
        "cancellation must not become a durable extraction failure"
    );
}

#[test]
fn pdf_coordinator_publishes_a_real_fixture_for_english_and_chinese_search() {
    let profile = tempfile::tempdir().unwrap();
    let repository = LibraryRepository::open(profile.path().join("library.sqlite")).unwrap();
    let (resource, note) = associated_resource(
        &repository,
        include_bytes!("resources/extractor-fixture.pdf"),
        "searchable.pdf",
        "application/pdf",
        "pdf",
    );

    assert_eq!(
        extractor::run_one_derived_text_pdf_job_with_exe(
            &repository,
            std::path::PathBuf::from(env!("CARGO_BIN_EXE_velotype")),
        )
        .unwrap(),
        extractor::DerivedTextCoordinatorOutcome::Indexed(resource.clone())
    );
    for term in ["English", "中文可选文字检索"] {
        let hits = repository.search(SearchQuery::parse(term)).unwrap();
        assert_eq!(hits.len(), 1, "{term}");
        assert_eq!(hits[0].note.id, note.id);
        assert_eq!(hits[0].matched_resource, Some(resource.clone()));
    }
}

#[test]
fn pdf_coordinator_records_parse_and_oversize_without_a_search_hit() {
    let profile = tempfile::tempdir().unwrap();
    let repository = LibraryRepository::open(profile.path().join("library.sqlite")).unwrap();
    let (corrupt, _) = associated_resource(
        &repository,
        b"not a PDF",
        "corrupt.pdf",
        "application/pdf",
        "pdf",
    );
    assert_eq!(
        extractor::run_one_derived_text_pdf_job_with_exe(
            &repository,
            std::path::PathBuf::from(env!("CARGO_BIN_EXE_velotype")),
        )
        .unwrap(),
        extractor::DerivedTextCoordinatorOutcome::Failed(
            corrupt.clone(),
            DerivedTextFailure::Parse,
        )
    );
    assert_eq!(
        repository.derived_text_status(&corrupt).unwrap(),
        Some(DerivedTextStatus::Failed {
            failure: DerivedTextFailure::Parse,
            attempts: 1,
        })
    );
    assert!(
        repository
            .search(SearchQuery::parse("not a PDF"))
            .unwrap()
            .is_empty()
    );

    let (oversize, _) = associated_resource(
        &repository,
        &vec![b'x'; 20 * 1024 * 1024 + 1],
        "oversize.pdf",
        "application/pdf",
        "pdf",
    );
    let verified_opens = repository.observe_verified_resource_opens();
    assert_eq!(
        extractor::run_one_derived_text_pdf_job_with_exe(
            &repository,
            std::path::PathBuf::from(env!("CARGO_BIN_EXE_velotype")),
        )
        .unwrap(),
        extractor::DerivedTextCoordinatorOutcome::Failed(
            oversize.clone(),
            DerivedTextFailure::TooLarge,
        )
    );
    assert_eq!(
        repository.derived_text_status(&oversize).unwrap(),
        Some(DerivedTextStatus::Failed {
            failure: DerivedTextFailure::TooLarge,
            attempts: 1,
        })
    );
    assert!(matches!(
        verified_opens.recv_timeout(std::time::Duration::from_millis(20)),
        Err(std::sync::mpsc::RecvTimeoutError::Timeout)
    ));
}

#[test]
fn pdf_coordinator_never_publishes_a_stale_job_identity() {
    let profile = tempfile::tempdir().unwrap();
    let repository = LibraryRepository::open(profile.path().join("library.sqlite")).unwrap();
    let (resource, _) = associated_resource(
        &repository,
        include_bytes!("resources/extractor-fixture.pdf"),
        "stale.pdf",
        "application/pdf",
        "pdf",
    );
    let mut stale = repository.take_derived_text_jobs(1).unwrap().pop().unwrap();
    stale.sha256 = BlobHash::new("0".repeat(64)).unwrap();

    assert_eq!(
        extractor::run_derived_text_pdf_job_with_exe(
            &repository,
            stale,
            std::path::PathBuf::from(env!("CARGO_BIN_EXE_velotype")),
        )
        .unwrap(),
        extractor::DerivedTextCoordinatorOutcome::Stale(resource.clone())
    );
    assert_eq!(
        repository.derived_text_status(&resource).unwrap(),
        Some(DerivedTextStatus::Pending { attempts: 0 })
    );
    assert!(
        repository
            .search(SearchQuery::parse("English"))
            .unwrap()
            .is_empty()
    );
}

#[test]
fn coordinator_marks_unsupported_image_mime_without_opening_a_descriptor() {
    let profile = tempfile::tempdir().unwrap();
    let repository = LibraryRepository::open(profile.path().join("library.sqlite")).unwrap();
    let (image, _) = associated_resource(
        &repository,
        b"not decoded in D3b-2B",
        "unsupported.gif",
        "image/gif",
        "gif",
    );
    let verified_opens = repository.observe_verified_resource_opens();
    assert_eq!(
        extractor::run_one_derived_text_pdf_job_with_exe(
            &repository,
            std::path::PathBuf::from(env!("CARGO_BIN_EXE_velotype")),
        )
        .unwrap(),
        extractor::DerivedTextCoordinatorOutcome::Failed(
            image.clone(),
            DerivedTextFailure::Unsupported,
        )
    );
    assert_eq!(
        repository.derived_text_status(&image).unwrap(),
        Some(DerivedTextStatus::Failed {
            failure: DerivedTextFailure::Unsupported,
            attempts: 1,
        })
    );
    assert!(matches!(
        verified_opens.recv_timeout(std::time::Duration::from_millis(20)),
        Err(std::sync::mpsc::RecvTimeoutError::Timeout)
    ));
}

#[test]
fn coordinator_indexes_real_english_and_chinese_images_with_resource_provenance() {
    let profile = tempfile::tempdir().unwrap();
    let repository = LibraryRepository::open(profile.path().join("library.sqlite")).unwrap();
    let (english, english_note) = associated_resource(
        &repository,
        include_bytes!("resources/ocr-english.png"),
        "one.png",
        "image/png",
        "png",
    );
    let (chinese, chinese_note) = associated_resource(
        &repository,
        include_bytes!("resources/ocr-chinese.png"),
        "two.png",
        "image/png",
        "png",
    );
    let exe = std::path::PathBuf::from(env!("CARGO_BIN_EXE_velotype"));

    let mut indexed = Vec::new();
    for executable in [exe.clone(), exe] {
        match extractor::run_one_derived_text_pdf_job_with_exe(&repository, executable).unwrap() {
            extractor::DerivedTextCoordinatorOutcome::Indexed(resource) => indexed.push(resource),
            other => panic!("expected one of the two OCR resources to index, got {other:?}"),
        }
    }
    assert_eq!(indexed.len(), 2);
    assert!(indexed.contains(&english));
    assert!(indexed.contains(&chinese));
    for (term, resource, note, provenance) in [
        (
            "Vision OCR English",
            english,
            english_note,
            "匹配附件：one.png",
        ),
        (
            "中文视觉文字识别",
            chinese,
            chinese_note,
            "匹配附件：two.png",
        ),
    ] {
        let hits = repository.search(SearchQuery::parse(term)).unwrap();
        assert_eq!(hits.len(), 1, "{term}");
        assert_eq!(hits[0].note.id, note.id, "{term}");
        assert_eq!(hits[0].matched_resource, Some(resource), "{term}");
        assert!(
            hits[0].snippet.contains(term),
            "{term}: {:?}",
            hits[0].snippet
        );
        assert!(hits[0].snippet.ends_with(provenance), "{term}");
        assert_eq!(hits[0].note.snippet, hits[0].snippet, "{term}");
    }
}

#[test]
fn coordinator_rejects_a_physically_grown_blob_even_when_database_size_is_small() {
    let profile = tempfile::tempdir().unwrap();
    let repository = LibraryRepository::open(profile.path().join("library.sqlite")).unwrap();
    let (resource, _) = associated_resource(
        &repository,
        include_bytes!("resources/extractor-fixture.pdf"),
        "grown.pdf",
        "application/pdf",
        "pdf",
    );
    let metadata = repository.resource_metadata(&resource).unwrap().unwrap();
    assert!(metadata.size < 20 * 1024 * 1024);
    let mut blob = std::fs::OpenOptions::new()
        .append(true)
        .open(
            profile
                .path()
                .join("resources/blobs")
                .join(metadata.sha256.as_str()),
        )
        .unwrap();
    blob.write_all(&vec![b'x'; 20 * 1024 * 1024]).unwrap();
    blob.sync_all().unwrap();

    assert_eq!(
        extractor::run_one_derived_text_pdf_job_with_exe(
            &repository,
            std::path::PathBuf::from(env!("CARGO_BIN_EXE_velotype")),
        )
        .unwrap(),
        extractor::DerivedTextCoordinatorOutcome::Failed(
            resource.clone(),
            DerivedTextFailure::TooLarge,
        )
    );
    assert_eq!(
        repository.derived_text_status(&resource).unwrap(),
        Some(DerivedTextStatus::Failed {
            failure: DerivedTextFailure::TooLarge,
            attempts: 1,
        })
    );
}
