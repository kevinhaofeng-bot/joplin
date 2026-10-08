//! Opt-in record of text input, for diagnosing input methods on a real
//! machine. With `JOPLIN_LITE_INPUT_TRACE=/path/to/file` set when the app
//! starts, every input-method call on the note body and title, and every key
//! that reaches the body's own key handling, plus toolbar command/history,
//! body pointer routes and search visibility/focus boundaries, is appended as one
//! JSON line. Off (and free) otherwise; nothing turns it on for a regular
//! profile. It records the typed text, so it is for test content only, and a
//! new trace file is readable by its owner only.

use std::io::Write as _;
use std::sync::{Mutex, OnceLock};
use std::time::{SystemTime, UNIX_EPOCH};

fn sink() -> Option<&'static Mutex<std::fs::File>> {
    static SINK: OnceLock<Option<Mutex<std::fs::File>>> = OnceLock::new();
    SINK.get_or_init(|| {
        let path = std::env::var_os("JOPLIN_LITE_INPUT_TRACE")?;
        open_private(std::path::Path::new(&path))
            .ok()
            .map(Mutex::new)
    })
    .as_ref()
}

/// The trace holds typed text: a new file is readable by its owner only.
fn open_private(path: &std::path::Path) -> std::io::Result<std::fs::File> {
    let mut options = std::fs::OpenOptions::new();
    options.create(true).append(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        options.mode(0o600);
    }
    options.open(path)
}

#[cfg(test)]
thread_local! {
    static TEST_LINES: std::cell::RefCell<Option<Vec<String>>> = const { std::cell::RefCell::new(None) };
}

/// Starts collecting this thread's trace lines in memory.
#[cfg(test)]
pub(crate) fn capture_for_test() {
    TEST_LINES.with(|lines| *lines.borrow_mut() = Some(Vec::new()));
}

#[cfg(test)]
pub(crate) fn take_for_test() -> Vec<String> {
    TEST_LINES.with(|lines| lines.borrow_mut().take().unwrap_or_default())
}

fn enabled() -> bool {
    #[cfg(test)]
    if TEST_LINES.with(|lines| lines.borrow().is_some()) {
        return true;
    }
    sink().is_some()
}

/// Records one event; `detail` is only built when tracing is on.
pub(crate) fn record(target: &str, event: &str, detail: impl FnOnce() -> serde_json::Value) {
    if !enabled() {
        return;
    }
    let millis = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_millis());
    let line = serde_json::json!({
        "ms": millis as u64,
        "target": target,
        "event": event,
        "detail": detail(),
    })
    .to_string();
    #[cfg(test)]
    if TEST_LINES.with(|lines| {
        lines
            .borrow_mut()
            .as_mut()
            .map(|lines| lines.push(line.clone()))
            .is_some()
    }) {
        return;
    }
    if let Some(sink) = sink()
        && let Ok(mut file) = sink.lock()
    {
        let _ = writeln!(file, "{line}");
    }
}

#[cfg(test)]
mod tests {
    #[cfg(unix)]
    #[test]
    fn a_new_trace_file_is_private_to_its_owner() {
        use std::os::unix::fs::PermissionsExt as _;
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("input-trace.jsonl");
        drop(super::open_private(&path).unwrap());
        let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
    }
}

pub(crate) fn range(range: Option<&std::ops::Range<usize>>) -> serde_json::Value {
    match range {
        Some(range) => serde_json::json!([range.start, range.end]),
        None => serde_json::Value::Null,
    }
}
