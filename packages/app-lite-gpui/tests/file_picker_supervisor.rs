#![cfg(unix)]

#[path = "../src/file_picker/mod.rs"]
mod file_picker;

use file_picker::{PickerRequest, supervise_picker};
use std::os::unix::ffi::OsStrExt;
use std::process::{Child, Command};
use std::time::{Duration, Instant};

fn request() -> PickerRequest {
    PickerRequest::Open {
        files: true,
        directories: false,
        multiple: true,
        prompt: None,
    }
}

struct Fixture {
    directory: tempfile::TempDir,
}
impl Fixture {
    fn new() -> Self {
        Self {
            directory: tempfile::tempdir().unwrap(),
        }
    }
    fn command(&self, body: &str) -> Command {
        let mut command = Command::new("/bin/sh");
        command
            .args([
                "-c",
                &format!("printf '%s\\n' \"$$\" >> \"$1\"; cat >/dev/null; {body}"),
                "fixture",
            ])
            .arg(self.directory.path().join("pids"));
        command
    }
    fn assert_reaped(&self, attempts: usize) {
        let pids = std::fs::read_to_string(self.directory.path().join("pids")).unwrap();
        let pids: Vec<i32> = pids.lines().map(|s| s.parse().unwrap()).collect();
        assert_eq!(pids.len(), attempts, "actual spawned attempts");
        for pid in pids {
            assert_eq!(
                unsafe { libc::kill(pid, 0) },
                -1,
                "child {pid} remains alive or zombie"
            );
            assert_eq!(
                std::io::Error::last_os_error().raw_os_error(),
                Some(libc::ESRCH)
            );
        }
    }
    fn run(&self, body: &str) -> Result<Option<Vec<std::path::PathBuf>>, String> {
        self.run_with_timeout(body, Duration::from_millis(500))
    }
    fn run_with_timeout(
        &self,
        body: &str,
        timeout: Duration,
    ) -> Result<Option<Vec<std::path::PathBuf>>, String> {
        supervise_picker(|| self.command(body), &request(), || false, timeout)
    }
}
struct OwnedProcess(Child);
impl Drop for OwnedProcess {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

#[test]
fn initialization_timeout_retries_once_reaps_both_and_leaves_unrelated_process_alive() {
    let fixture = Fixture::new();
    let mut unrelated = OwnedProcess(Command::new("/bin/sleep").arg("30").spawn().unwrap());
    let start = Instant::now();
    let error = fixture
        .run_with_timeout("exec /bin/sleep 30", Duration::from_millis(50))
        .unwrap_err();
    assert!(error.contains("initialization timeout"), "{error}");
    assert!(start.elapsed() < Duration::from_secs(2));
    fixture.assert_reaped(2);
    assert!(unrelated.0.try_wait().unwrap().is_none());
}

#[test]
fn ready_removes_initialization_deadline_without_restarting_user_selection() {
    let fixture = Fixture::new();
    let start = Instant::now();
    assert_eq!(
        fixture
            .run_with_timeout(
                "printf '\"Ready\"\\n'; sleep 0.2; printf '\"Cancelled\"\\n'",
                Duration::from_millis(50)
            )
            .unwrap(),
        None
    );
    assert!(start.elapsed() >= Duration::from_millis(180));
    fixture.assert_reaped(1);
}

#[test]
fn selected_preserves_raw_non_utf8_path_and_accepts_terminal_before_ready() {
    let fixture = Fixture::new();
    let paths = fixture
        .run("printf '{\"Selected\":[[47,116,109,112,47,255]]}\\n'")
        .unwrap()
        .unwrap();
    assert_eq!(paths.len(), 1);
    assert_eq!(
        paths[0].as_os_str().as_bytes(),
        &[47, 116, 109, 112, 47, 255]
    );
    fixture.assert_reaped(1);
}

#[test]
fn cancellation_is_terminal_not_a_failure_to_retry() {
    let fixture = Fixture::new();
    assert_eq!(fixture.run("printf '\"Cancelled\"\\n'").unwrap(), None);
    fixture.assert_reaped(1);
}

#[test]
fn malformed_json_is_an_error_without_retry() {
    let fixture = Fixture::new();
    assert!(
        fixture
            .run("printf 'not-json\\n'")
            .unwrap_err()
            .contains("protocol")
    );
    fixture.assert_reaped(1);
}

#[test]
fn eof_with_unterminated_json_is_not_a_selection_or_cancel() {
    let fixture = Fixture::new();
    assert!(
        fixture
            .run("printf '\"Cancelled\"'")
            .unwrap_err()
            .contains("unterminated")
    );
    fixture.assert_reaped(1);
}

#[test]
fn unexpected_exit_before_ready_is_retried_once_not_returned_as_cancel() {
    let fixture = Fixture::new();
    let error = fixture.run("exit 7").unwrap_err();
    assert!(error.contains('7'), "{error}");
    fixture.assert_reaped(2);
}

#[test]
fn unexpected_exit_after_ready_is_an_error_without_retry() {
    let fixture = Fixture::new();
    assert!(
        fixture
            .run("printf '\"Ready\"\\n'; exit 7")
            .unwrap_err()
            .contains('7')
    );
    fixture.assert_reaped(1);
}

#[test]
fn oversized_stdout_is_rejected_and_process_is_reaped() {
    let fixture = Fixture::new();
    let error = fixture
        .run("printf '\"Ready\"\\n'; head -c 1048577 /dev/zero; exec sleep 30")
        .unwrap_err();
    assert!(error.contains("stdout limit"), "{error}");
    fixture.assert_reaped(1);
}

#[test]
fn oversized_stderr_is_rejected_and_process_is_reaped() {
    let fixture = Fixture::new();
    let error = fixture
        .run("printf '\"Ready\"\\n'; head -c 65537 /dev/zero >&2; exec sleep 30")
        .unwrap_err();
    assert!(error.contains("stderr limit"), "{error}");
    fixture.assert_reaped(1);
}

#[test]
fn parent_cancellation_after_ready_reaps_own_child_without_retry() {
    let fixture = Fixture::new();
    let start = Instant::now();
    let error = supervise_picker(
        || fixture.command("printf '\"Ready\"\\n'; exec sleep 30"),
        &request(),
        || start.elapsed() > Duration::from_millis(120),
        Duration::from_millis(50),
    )
    .unwrap_err();
    assert!(error.contains("request cancelled"), "{error}");
    assert!(start.elapsed() < Duration::from_secs(2));
    fixture.assert_reaped(1);
}

#[test]
fn first_initialization_hang_then_second_selection_uses_exactly_two_children() {
    let fixture = Fixture::new();
    let paths = fixture.run("if [ \"$(wc -l < \"$1\")\" -eq 1 ]; then exec sleep 30; fi; printf '{\"Selected\":[[47,116,109,112,47,97]]}\\n'").unwrap().unwrap();
    assert_eq!(paths, vec![std::path::PathBuf::from("/tmp/a")]);
    fixture.assert_reaped(2);
}

#[test]
fn duplicate_terminal_cannot_deliver_a_second_selection() {
    let fixture = Fixture::new();
    let paths = fixture
        .run("printf '{\"Selected\":[[47,97]]}\\n{\"Selected\":[[47,98]]}\\n'; exec sleep 30")
        .unwrap()
        .unwrap();
    assert_eq!(paths, vec![std::path::PathBuf::from("/a")]);
    fixture.assert_reaped(1);
}

#[test]
fn invalid_empty_relative_and_nul_paths_fail_closed() {
    for payload in ["[]", "[[]]", "[[97]]", "[[47,97,0]]"] {
        let fixture = Fixture::new();
        assert!(
            fixture
                .run(&format!("printf '{{\"Selected\":{payload}}}\\n'"))
                .is_err()
        );
        fixture.assert_reaped(1);
    }
}

#[test]
fn worker_failure_is_an_error_not_a_cancellation_and_not_retried() {
    let fixture = Fixture::new();
    let error = fixture
        .run("printf '{\"Failed\":\"panel failed\"}\\n'")
        .unwrap_err();
    assert!(error.contains("panel failed"));
    fixture.assert_reaped(1);
}

#[test]
fn oversized_request_is_rejected_before_any_child_is_spawned() {
    let fixture = Fixture::new();
    let request = PickerRequest::Open {
        files: true,
        directories: false,
        multiple: false,
        prompt: Some("x".repeat(65536)),
    };
    let error = supervise_picker(
        || fixture.command("exec sleep 30"),
        &request,
        || false,
        Duration::from_millis(50),
    )
    .unwrap_err();
    assert!(error.contains("request limit"), "{error}");
    assert!(!fixture.directory.path().join("pids").exists());
}

#[test]
fn request_is_versioned_and_contains_raw_directory_bytes() {
    let fixture = Fixture::new();
    let request = PickerRequest::Save {
        directory: vec![47, 116, 109, 112, 47, 255],
        suggested_name: Some("中文 a".into()),
    };
    let body = "printf '%s\\n' \"$$\" >> \"$1\"; cat > \"$2\"; printf '\"Cancelled\"\\n'";
    assert_eq!(
        supervise_picker(
            || {
                let mut command = Command::new("/bin/sh");
                command
                    .args(["-c", body, "fixture"])
                    .arg(fixture.directory.path().join("pids"))
                    .arg(fixture.directory.path().join("request"));
                command
            },
            &request,
            || false,
            Duration::from_millis(500)
        )
        .unwrap(),
        None
    );
    let bytes = std::fs::read(fixture.directory.path().join("request")).unwrap();
    let value: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(
        value,
        serde_json::json!({"version":1,"request":{"Save":{"directory":[47,116,109,112,47,255],"suggested_name":"中文 a"}}})
    );
    fixture.assert_reaped(1);
}

/// Explicit manual driver: still uses production supervision, an actual
/// native worker, and real owned-PID checks, not injected lifecycle results.
#[cfg(target_os = "macos")]
#[test]
#[ignore = "manual isolated macOS native picker recovery"]
fn stopped_real_native_worker_is_reaped_and_second_worker_can_be_cancelled() {
    use std::cell::Cell;
    let executable = std::env::var_os("JOPLIN_LITE_PICKER_PROBE_EXE")
        .expect("explicit isolated probe executable required");
    let executable = std::path::PathBuf::from(executable);
    assert!(executable.is_absolute() && executable.is_file());
    let fixture = Fixture::new();
    let pid_file = fixture.directory.path().join("pids");
    let stopped = Cell::new(false);
    let start = Instant::now();
    let result = supervise_picker(
        || {
            let mut command = Command::new("/bin/sh");
            command
                .args([
                    "-c",
                    "printf '%s\\n' \"$$\" >> \"$1\"; exec \"$2\" --native-file-picker",
                    "fixture",
                ])
                .arg(&pid_file)
                .arg(&executable);
            command
        },
        &request(),
        || {
            if !stopped.get()
                && let Ok(pids) = std::fs::read_to_string(&pid_file)
                && let Some(pid) = pids
                    .lines()
                    .next()
                    .and_then(|text| text.parse::<i32>().ok())
            {
                let command = Command::new("/bin/ps")
                    .args(["-p", &pid.to_string(), "-o", "comm="])
                    .output()
                    .unwrap();
                if String::from_utf8_lossy(&command.stdout).trim() == executable.to_str().unwrap() {
                    assert_eq!(unsafe { libc::kill(pid, libc::SIGSTOP) }, 0);
                    stopped.set(true);
                    eprintln!("real native worker stopped before result: PID {pid}");
                }
            }
            // A bound for this manual test, never a production selection deadline.
            start.elapsed() > Duration::from_secs(120)
        },
        Duration::from_secs(2),
    )
    .unwrap();
    assert!(stopped.get(), "did not stop a verified native worker");
    assert_eq!(
        result, None,
        "second native panel must be explicitly Cancelled"
    );
    fixture.assert_reaped(2);
}
