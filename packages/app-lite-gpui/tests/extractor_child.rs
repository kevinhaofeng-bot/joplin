use std::io::Write;
use std::process::{Command, Stdio};

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
