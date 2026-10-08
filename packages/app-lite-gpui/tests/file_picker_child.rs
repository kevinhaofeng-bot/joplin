#![cfg(target_os = "macos")]

use std::io::{Seek, Write};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

struct Reap(Option<Child>);
impl Drop for Reap {
    fn drop(&mut self) {
        if let Some(child) = self.0.as_mut() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

fn rejected(input: &[u8], extra: &[&str], marker: &str) {
    let directory = tempfile::tempdir().unwrap();
    let profile = directory.path().join("must-not-exist");
    let mut file = tempfile::tempfile().unwrap();
    file.write_all(input).unwrap();
    file.rewind().unwrap();
    let mut child = Reap(Some(
        Command::new(env!("CARGO_BIN_EXE_velotype"))
            .arg("--native-file-picker")
            .args(extra)
            .env("JOPLIN_LITE_PROFILE", &profile)
            .stdin(Stdio::from(file))
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap(),
    ));
    let start = Instant::now();
    while child.0.as_mut().unwrap().try_wait().unwrap().is_none() {
        assert!(
            start.elapsed() < Duration::from_secs(3),
            "invalid request opened a GUI or stalled"
        );
        std::thread::sleep(Duration::from_millis(2));
    }
    let output = child.0.take().unwrap().wait_with_output().unwrap();
    assert!(!output.status.success());
    assert!(!profile.exists(), "worker touched the notes profile");
    let event: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap_or_else(|error| {
        panic!(
            "expected picker failure event, got {:?}/{:?}: {error}",
            output.stdout, output.stderr
        )
    });
    let message = event
        .get("Failed")
        .and_then(|value| value.as_str())
        .expect("failure must be explicit, not cancel");
    assert!(message.contains(marker), "expected {marker}: {message}");
}

#[test]
fn native_picker_rejects_unknown_version_before_opening_a_profile_or_app() {
    rejected(br#"{"version":99,"request":{"Open":{"files":true,"directories":false,"multiple":false,"prompt":null}}}"#, &[], "version");
}

#[test]
fn native_picker_rejects_malformed_json_and_unknown_envelope_fields() {
    rejected(b"not-json", &[], "protocol");
    rejected(br#"{"version":1,"request":{"Open":{"files":true,"directories":false,"multiple":false,"prompt":null}},"extra":true}"#, &[], "protocol");
}

#[test]
fn native_picker_bounds_stdin_before_appkit_initialization() {
    rejected(&vec![b'x'; 65537], &[], "request limit");
}

#[test]
fn native_picker_rejects_unusable_options_before_appkit_initialization() {
    rejected(br#"{"version":1,"request":{"Open":{"files":false,"directories":false,"multiple":false,"prompt":null}}}"#, &[], "options");
    for directory in ["[]", "[97]", "[47,97,0]"] {
        rejected(format!("{{\"version\":1,\"request\":{{\"Save\":{{\"directory\":{directory},\"suggested_name\":null}}}}}}").as_bytes(), &[], "directory");
    }
}

#[test]
fn native_picker_rejects_additional_cli_arguments_without_opening_a_library() {
    rejected(b"", &["--unexpected"], "arguments");
}

#[test]
fn helper_relaunch_cannot_start_a_notes_writer_even_with_help_or_extractor_flags() {
    let directory = tempfile::tempdir().unwrap();
    let executable = directory.path().join("joplin-lite-picker");
    std::fs::copy(env!("CARGO_BIN_EXE_velotype"), &executable).unwrap();
    for args in [vec![], vec!["--help"], vec!["--extract-resource-text"]] {
        let profile = directory.path().join("must-not-exist");
        let mut child = Reap(Some(Command::new(&executable)
            .args(args).env("JOPLIN_LITE_PROFILE", &profile)
            .stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped())
            .spawn().unwrap()));
        let started = Instant::now();
        while child.0.as_mut().unwrap().try_wait().unwrap().is_none() {
            assert!(started.elapsed() < Duration::from_secs(3), "helper relaunch opened GUI or stalled");
            std::thread::sleep(Duration::from_millis(2));
        }
        let output = child.0.take().unwrap().wait_with_output().unwrap();
        assert_eq!(output.status.code(), Some(2));
        assert!(String::from_utf8_lossy(&output.stderr).contains("requires --native-file-picker"));
        assert!(output.stdout.is_empty());
        assert!(!profile.exists(), "helper relaunch touched notes profile");
    }
}

#[test]
#[ignore = "manual isolated macOS accessory helper panel initialization"]
fn accessory_helper_initializes_when_policy_is_already_accessory() {
    use std::io::BufRead;
    let directory = tempfile::tempdir().unwrap();
    let bundle = directory.path().join("Joplin Lite Picker Test.app");
    let macos = bundle.join("Contents/MacOS");
    std::fs::create_dir_all(&macos).unwrap();
    let executable = macos.join("joplin-lite-picker");
    let source = std::env::var_os("JOPLIN_LITE_PICKER_POLICY_PROBE_EXE")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| env!("CARGO_BIN_EXE_velotype").into());
    assert!(source.is_absolute() && source.is_file());
    std::fs::copy(source, &executable).unwrap();
    std::fs::write(bundle.join("Contents/Info.plist"), format!(
        "<?xml version=\"1.0\"?><plist version=\"1.0\"><dict>\
        <key>CFBundleExecutable</key><string>joplin-lite-picker</string>\
        <key>CFBundleIdentifier</key><string>com.arielkevin.joplinlite.acceptance.policytest.p{}</string>\
        <key>CFBundleName</key><string>Joplin Lite Picker Test</string>\
        <key>CFBundlePackageType</key><string>APPL</string>\
        <key>LSUIElement</key><true/></dict></plist>", std::process::id()
    )).unwrap();
    assert!(Command::new("/usr/bin/codesign").args(["--force", "--sign", "-"])
        .arg(&bundle).status().unwrap().success());
    let profile = directory.path().join("must-not-exist");
    let mut request = tempfile::tempfile().unwrap();
    request.write_all(r#"{"version":1,"request":{"Open":{"files":true,"directories":false,"multiple":false,"prompt":"验收原生策略"}}}"#.as_bytes()).unwrap();
    request.rewind().unwrap();
    let mut child = Reap(Some(Command::new(&executable).arg("--native-file-picker")
        .env("JOPLIN_LITE_PROFILE", &profile).stdin(Stdio::from(request))
        .stdout(Stdio::piped()).stderr(Stdio::null()).spawn().unwrap()));
    let stdout = child.0.as_mut().unwrap().stdout.take().unwrap();
    let (sender, receiver) = std::sync::mpsc::channel();
    let reader = std::thread::spawn(move || {
        let mut line = String::new();
        let result = std::io::BufReader::new(stdout).read_line(&mut line);
        let _ = sender.send((result, line));
    });
    let event = receiver.recv_timeout(Duration::from_secs(15));
    // Stop only the owned test worker; no click on other applications/panels.
    drop(child);
    reader.join().unwrap();
    let (read, line) = event.expect("accessory helper did not initialize its actual native panel");
    read.unwrap();
    assert_eq!(line.trim(), "\"Ready\"", "actual worker rejected its existing accessory policy");
    assert!(!profile.exists());
}
