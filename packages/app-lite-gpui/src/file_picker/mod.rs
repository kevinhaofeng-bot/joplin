//! One-shot native picker protocol and background-only child supervision.
//! The native worker is verified independently before UI entry points switch.

mod adapter;
#[cfg(target_os = "macos")]
mod native;
mod protocol;
mod worker_path;
#[cfg(unix)]
mod supervisor;

pub use protocol::{PickerEvent, PickerRequest};
pub use adapter::bind_to_request;
pub use worker_path::is_helper_executable;
#[cfg(unix)]
pub use supervisor::supervise_picker;

pub fn prompt_for_paths(
    cx: &gpui::App,
    options: gpui::PathPromptOptions,
) -> futures::channel::oneshot::Receiver<anyhow::Result<Option<Vec<std::path::PathBuf>>>> {
    #[cfg(not(target_os = "macos"))]
    return cx.prompt_for_paths(options);
    #[cfg(target_os = "macos")]
    start_picker(cx, PickerRequest::Open {
        files: options.files,
        directories: options.directories,
        multiple: options.multiple,
        prompt: options.prompt.map(|s| s.to_string()),
    }, Ok)
}

pub fn prompt_for_new_path(
    cx: &gpui::App,
    directory: &std::path::Path,
    suggested_name: Option<&str>,
) -> futures::channel::oneshot::Receiver<anyhow::Result<Option<std::path::PathBuf>>> {
    #[cfg(not(target_os = "macos"))]
    return cx.prompt_for_new_path(directory, suggested_name);
    #[cfg(target_os = "macos")]
    {
        use std::os::unix::ffi::OsStrExt;
        start_picker(cx, PickerRequest::Save {
            directory: directory.as_os_str().as_bytes().to_vec(),
            suggested_name: suggested_name.map(str::to_owned),
        }, |mut paths| {
            if paths.len() != 1 {
                return Err("picker Save must return exactly one path".into());
            }
            Ok(paths.remove(0))
        })
    }
}

#[cfg(target_os = "macos")]
fn start_picker<T: Send + 'static>(
    cx: &gpui::App,
    request: PickerRequest,
    convert: impl FnOnce(Vec<std::path::PathBuf>) -> Result<T, String> + Send + 'static,
) -> futures::channel::oneshot::Receiver<anyhow::Result<Option<T>>> {
    let (sender, receiver) = futures::channel::oneshot::channel();
    cx.background_executor().spawn(async move {
        let result = (|| {
            let current = std::env::current_exe().map_err(|e| e.to_string())?;
            let executable = worker_path::picker_executable(&current)?;
            supervise_picker(|| {
                let mut command = std::process::Command::new(&executable);
                command.arg("--native-file-picker");
                command
            }, &request, || sender.is_canceled(), std::time::Duration::from_secs(10))?
                .map(convert).transpose()
        })().map_err(|reason| anyhow::anyhow!(
            "未选择文件，内容未改动，可重试：{reason}"
        ));
        let _ = sender.send(result);
    }).detach();
    receiver
}

/// Only the earliest CLI branch calls this: one request, one native panel,
/// no notes repository, GPUI Application, migration or background indexing.
pub fn run_child() -> i32 {
    #[cfg(target_os = "macos")]
    let parent = unsafe { libc::getppid() };
    let result = (|| {
        if std::env::args_os().count() != 2 {
            return Err("picker arguments: mode takes no additional arguments".into());
        }
        let request = protocol::read_request(std::io::stdin().lock())?;
        #[cfg(target_os = "macos")]
        {
            native::watch_parent(parent)?;
            return native::run(request);
        }
        #[cfg(not(target_os = "macos"))]
        {
            let _ = request;
            Err("native picker is supported only on macOS".into())
        }
    })();
    match result {
        Ok(()) => 0,
        Err(reason) => {
            let _ = protocol::emit_event(&PickerEvent::Failed(reason));
            2
        }
    }
}
