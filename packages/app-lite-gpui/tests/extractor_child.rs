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
fn pdf_child_rejects_wrong_mime_and_corrupt_input() {
    let mime = child("image/png", b"not used");
    assert!(!mime.status.success());
    assert!(String::from_utf8_lossy(&mime.stderr).contains("unsupported-mime"));
    let corrupt = child("application/pdf", b"not a PDF");
    assert!(!corrupt.status.success());
    assert!(String::from_utf8_lossy(&corrupt.stderr).contains("pdf-parse-failed"));
}

#[test]
fn pdf_child_rejects_input_above_its_stream_budget() {
    let output = child("application/pdf", &vec![b'x'; 20 * 1024 * 1024 + 1]);
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("input-too-large"));
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
fn coordinator_marks_non_pdf_jobs_unsupported_without_launching_the_pdf_child() {
    let profile = tempfile::tempdir().unwrap();
    let repository = LibraryRepository::open(profile.path().join("library.sqlite")).unwrap();
    let (image, _) = associated_resource(
        &repository,
        b"not decoded in D3b-2B",
        "future-vision.png",
        "image/png",
        "png",
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
