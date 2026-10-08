//! Opt-in macOS input-context diagnostics for synthetic acceptance profiles.
//! Never enabled by normal startup. No text content is recorded here.

use std::{
    fs::{File, OpenOptions},
    io::Write,
    path::Path,
    sync::{Mutex, OnceLock},
    time::{SystemTime, UNIX_EPOCH},
};

fn open_private(path: &Path) -> std::io::Result<File> {
    use std::os::unix::fs::OpenOptionsExt;
    OpenOptions::new()
        .create(true)
        .append(true)
        .mode(0o600)
        .open(path)
}

fn sink() -> Option<&'static Mutex<File>> {
    static SINK: OnceLock<Option<Mutex<File>>> = OnceLock::new();
    SINK.get_or_init(|| {
        let path = std::env::var_os("JOPLIN_LITE_MAC_INPUT_TRACE")?;
        open_private(Path::new(&path)).ok().map(Mutex::new)
    })
    .as_ref()
}

fn record_to(sink: Option<&Mutex<File>>, event: &str, detail: impl FnOnce() -> serde_json::Value) {
    let Some(sink) = sink else {
        return;
    };
    let ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |time| time.as_millis());
    let line = serde_json::json!({"ms": ms as u64, "event": event, "detail": detail()});
    if let Ok(mut file) = sink.lock() {
        let _ = writeln!(file, "{line}");
    }
}

pub(super) fn record(event: &str, detail: impl FnOnce() -> serde_json::Value) {
    record_to(sink(), event, detail);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn disabled_trace_does_not_inspect_the_native_input_context() {
        record_to(None, "key_down", || panic!("must remain lazy when off"));
    }

    #[test]
    fn new_native_trace_file_is_private() {
        use std::os::unix::fs::PermissionsExt;
        let path = std::env::temp_dir().join(format!(
            "joplin-native-trace-test-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        drop(open_private(&path).unwrap());
        let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        std::fs::remove_file(&path).unwrap();
        assert_eq!(
            mode, 0o600,
            "diagnostic metadata must not be world readable"
        );
    }
}
