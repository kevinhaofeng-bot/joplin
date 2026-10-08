use super::protocol::{OUTPUT_LIMIT, STDERR_LIMIT, encode_request};
use super::{PickerEvent, PickerRequest};
use std::ffi::OsString;
use std::io::{Read, Write};
use std::os::fd::AsRawFd;
use std::os::unix::ffi::OsStringExt;
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

/// Owns only this invocation's child, never a process group or system service.
/// Every exit path kills an outstanding worker and waits to reap it.
struct OwnedChild(Child);
impl Drop for OwnedChild {
    fn drop(&mut self) {
        if matches!(self.0.try_wait(), Ok(Some(_))) {
            return;
        }
        if self.0.kill().is_ok() {
            let _ = self.0.wait();
        } else {
            // A kill can race an ordinary exit. try_wait also reaps it.
            let _ = self.0.try_wait();
        }
    }
}

struct Failure {
    message: String,
    retry_initialization: bool,
}
impl From<String> for Failure {
    fn from(message: String) -> Self {
        Self {
            message,
            retry_initialization: false,
        }
    }
}

fn nonblocking(pipe: &impl AsRawFd) -> Result<(), String> {
    let fd = pipe.as_raw_fd();
    // Pipes are owned for the entire fcntl calls, and flags retain all existing bits.
    let flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
    if flags < 0 || unsafe { libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK) } < 0 {
        return Err(format!(
            "picker pipe setup: {}",
            std::io::Error::last_os_error()
        ));
    }
    Ok(())
}

/// Bounded work per poll preserves cancellation/deadline responsiveness even
/// when the worker floods a pipe. No reader thread can be left blocked on EOF.
fn read_available(
    pipe: &mut impl Read,
    buffer: &mut Vec<u8>,
    total: &mut usize,
    limit: usize,
    name: &str,
) -> Result<(bool, bool), String> {
    let mut chunk = [0u8; 4096];
    for _ in 0..16 {
        match pipe.read(&mut chunk) {
            Ok(0) => return Ok((true, false)),
            Ok(size) => {
                if size > limit.saturating_sub(*total) {
                    return Err(format!("picker {name} limit exceeded"));
                }
                *total += size;
                buffer.extend_from_slice(&chunk[..size]);
            }
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                return Ok((false, false));
            }
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(error) => return Err(format!("picker {name} read: {error}")),
        }
    }
    Ok((false, true))
}

fn selected_paths(paths: Vec<Vec<u8>>) -> Result<Vec<PathBuf>, String> {
    if paths.is_empty() {
        return Err("picker protocol: empty selection".into());
    }
    paths
        .into_iter()
        .map(|bytes| {
            if bytes.is_empty() || bytes.contains(&0) {
                return Err("picker protocol: invalid path bytes".into());
            }
            let path = PathBuf::from(OsString::from_vec(bytes));
            if !path.is_absolute() {
                return Err("picker protocol: relative path".into());
            }
            Ok(path)
        })
        .collect()
}

fn attempt(
    mut command: Command,
    request: &[u8],
    cancelled: &impl Fn() -> bool,
    timeout: Duration,
) -> Result<Option<Vec<PathBuf>>, Failure> {
    let mut child = OwnedChild(
        command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|e| format!("picker spawn: {e}"))?,
    );
    let input = child
        .0
        .stdin
        .take()
        .ok_or_else(|| "picker missing stdin".to_string())?;
    let mut output = child
        .0
        .stdout
        .take()
        .ok_or_else(|| "picker missing stdout".to_string())?;
    let mut error_output = child
        .0
        .stderr
        .take()
        .ok_or_else(|| "picker missing stderr".to_string())?;
    nonblocking(&input)?;
    nonblocking(&output)?;
    nonblocking(&error_output)?;
    let mut input = Some(input);
    let mut written = 0;
    let mut pending = Vec::new();
    let mut diagnostics = Vec::new();
    let (mut total_out, mut total_err) = (0, 0);
    let mut ready = false;
    let mut exited = None;
    let start = Instant::now();
    loop {
        if cancelled() {
            return Err("picker request cancelled".to_string().into());
        }
        if let Some(pipe) = input.as_mut() {
            match pipe.write(&request[written..]) {
                Ok(0) => return Err("picker stdin write returned zero".to_string().into()),
                Ok(size) => written += size,
                Err(e)
                    if e.kind() == std::io::ErrorKind::WouldBlock
                        || e.kind() == std::io::ErrorKind::Interrupted => {}
                Err(e) => return Err(format!("picker stdin write: {e}").into()),
            }
            if written == request.len() {
                input = None;
                // Close stdin to frame the one bounded request by EOF.
            }
        }
        let (stdout_eof, more_stdout) = read_available(
            &mut output,
            &mut pending,
            &mut total_out,
            OUTPUT_LIMIT,
            "stdout",
        )?;
        let (_, more_stderr) = read_available(
            &mut error_output,
            &mut diagnostics,
            &mut total_err,
            STDERR_LIMIT,
            "stderr",
        )?;
        while let Some(end) = pending.iter().position(|byte| *byte == b'\n') {
            let line: Vec<u8> = pending.drain(..=end).collect();
            let event: PickerEvent =
                serde_json::from_slice(&line).map_err(|e| format!("picker event protocol: {e}"))?;
            match event {
                PickerEvent::Ready if !ready => ready = true,
                PickerEvent::Ready => {
                    return Err("picker protocol: duplicate Ready".to_string().into());
                }
                PickerEvent::Selected(paths) => return Ok(Some(selected_paths(paths)?)),
                PickerEvent::Cancelled => return Ok(None),
                PickerEvent::Failed(reason) => {
                    return Err(format!("picker worker failed: {reason}").into());
                }
            }
        }
        if stdout_eof && !pending.is_empty() {
            return Err("picker protocol: unterminated event at EOF"
                .to_string()
                .into());
        }
        if let Some(status) = exited {
            // One additional read after noticing exit drains bytes written
            // between the previous read and try_wait. Do not require EOF:
            // descendants could still hold a pipe, but we own only this Child.
            if more_stdout || more_stderr {
                continue;
            }
            if !pending.is_empty() {
                return Err("picker protocol: unterminated event after exit"
                    .to_string()
                    .into());
            }
            return Err(Failure {
                message: format!(
                    "picker exited without result ({status}): {}",
                    String::from_utf8_lossy(&diagnostics)
                ),
                retry_initialization: !ready,
            });
        }
        exited = child
            .0
            .try_wait()
            .map_err(|e| format!("picker wait: {e}"))?;
        if exited.is_some() {
            continue;
        }
        if !ready && start.elapsed() >= timeout {
            return Err(Failure {
                message: "picker initialization timeout".into(),
                retry_initialization: true,
            });
        }
        std::thread::sleep(Duration::from_millis(2));
    }
}

/// Run on the background executor only. Ready removes the initialization
/// deadline; a person's selection remains open until completion/cancellation.
pub fn supervise_picker(
    mut make_command: impl FnMut() -> Command,
    request: &PickerRequest,
    cancelled: impl Fn() -> bool,
    initialization_timeout: Duration,
) -> Result<Option<Vec<PathBuf>>, String> {
    let bytes = encode_request(request)?;
    for index in 0..2 {
        if cancelled() {
            return Err("picker request cancelled".into());
        }
        match attempt(make_command(), &bytes, &cancelled, initialization_timeout) {
            Ok(result) => return Ok(result),
            Err(failure) if index == 0 && failure.retry_initialization => continue,
            Err(failure) => return Err(failure.message),
        }
    }
    unreachable!("two attempts always terminate")
}
