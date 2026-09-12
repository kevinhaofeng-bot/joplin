use std::io::Write;
use std::process::{Command, Stdio};
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
