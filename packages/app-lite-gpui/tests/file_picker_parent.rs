#![cfg(target_os = "macos")]

use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

struct Launcher(Child);
impl Drop for Launcher {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

struct Worker(libc::pid_t);
impl Drop for Worker {
    fn drop(&mut self) {
        // This PID came from our launcher's $!, never from a name search.
        let expected = format!("{} --native-file-picker", env!("CARGO_BIN_EXE_velotype"));
        let command = Command::new("/bin/ps").args(["-p", &self.0.to_string(), "-o", "command="]).output();
        if command.is_ok_and(|output| output.status.success()
            && String::from_utf8_lossy(&output.stdout).trim() == expected) {
            unsafe { libc::kill(self.0, libc::SIGKILL); }
        }
    }
}

fn alive(pid: libc::pid_t) -> bool {
    unsafe { libc::kill(pid, 0) == 0 }
}

// Without a worker-side parent-lifetime guard this creates an orphan panel.
// It must be explicitly run on the isolated desktop; default ignored is NOT
// counted as a pass, and the guard kills only this test's worker on failure.
#[test]
#[ignore = "real AppKit worker; requires the authorized isolated desktop"]
fn native_picker_worker_exits_when_its_launcher_dies() {
    let directory = tempfile::tempdir().unwrap();
    let input = directory.path().join("request.json");
    let output = directory.path().join("events.jsonl");
    let diagnostics = directory.path().join("stderr.txt");
    let pid_file = directory.path().join("worker.pid");
    let profile = directory.path().join("must-not-exist");
    std::fs::write(&input, r#"{"version":1,"request":{"Open":{"files":true,"directories":false,"multiple":false,"prompt":"生命周期验收"}}}"#).unwrap();
    let mut launcher = Launcher(Command::new("/bin/sh")
        .args(["-c", r#""$1" --native-file-picker < "$2" > "$3" 2> "$4" & printf '%s\n' "$!" > "$5"; wait"#, "picker-parent-test"])
        .arg(env!("CARGO_BIN_EXE_velotype"))
        .args([&input, &output, &diagnostics, &pid_file])
        .env("JOPLIN_LITE_PROFILE", &profile)
        .stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null())
        .spawn().unwrap());
    let start = Instant::now();
    let worker = loop {
        if let Ok(pid) = std::fs::read_to_string(&pid_file)
            && let Ok(pid) = pid.trim().parse::<libc::pid_t>()
            && pid > 1 {
            break Worker(pid);
        }
        assert!(start.elapsed() < Duration::from_secs(3), "launcher did not record its worker PID");
        std::thread::sleep(Duration::from_millis(10));
    };
    // Allow earliest CLI dispatch to run, without relying on a possibly
    // blocked system panel ever reaching Ready.
    std::thread::sleep(Duration::from_millis(300));
    assert!(alive(worker.0), "worker exited before testing parent loss");
    launcher.0.kill().unwrap();
    launcher.0.wait().unwrap();
    let start = Instant::now();
    while alive(worker.0) && start.elapsed() < Duration::from_secs(3) {
        std::thread::sleep(Duration::from_millis(10));
    }
    let survived = alive(worker.0);
    eprintln!("parent exited; worker {} survived={survived}", worker.0);
    assert!(!profile.exists(), "picker initialized a notes library");
    assert!(!survived, "native picker orphaned its worker after parent exit");
}
