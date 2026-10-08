//! AppKit runs only on the one-shot worker's main thread. Objective-C
//! exceptions/crashes remain inside that worker, never unwind into the UI.
#![allow(deprecated)] // Same locked AppKit bridge as the existing GPUI platform.

use super::protocol::emit_event;
use super::{PickerEvent, PickerRequest};
use block::ConcreteBlock;
use cocoa::appkit::{
    NSApplication, NSApplicationActivationPolicy, NSModalResponse, NSOpenPanel, NSSavePanel,
};
use cocoa::base::{NO, YES, id, nil};
use cocoa::foundation::{NSArray, NSAutoreleasePool, NSProcessInfo, NSString, NSURL};
use objc::{class, msg_send, sel, sel_impl};
use std::ffi::{CStr, CString};
use std::os::raw::c_char;

/// The parent's Child guard cannot run after the parent itself exits. Keep a
/// small worker-only watchdog independent of AppKit initialization, which may
/// be blocked in a system service. It exits this worker, never signals a PID.
pub(super) fn watch_parent(parent: libc::pid_t) -> Result<(), String> {
    if parent <= 1 || unsafe { libc::getppid() } != parent {
        return Err("picker parent already exited".into());
    }
    std::thread::Builder::new()
        .name("picker-parent".into())
        .stack_size(128 * 1024)
        .spawn(move || loop {
            if unsafe { libc::getppid() } != parent {
                std::process::exit(2);
            }
            std::thread::sleep(std::time::Duration::from_millis(100));
        })
        .map(|_| ())
        .map_err(|error| format!("picker parent monitor: {error}"))
}

fn finish(result: Result<PickerEvent, String>) -> ! {
    let (event, status) = match result {
        Ok(event) => (event, 0),
        Err(reason) => (PickerEvent::Failed(reason), 2),
    };
    if let Err(reason) = emit_event(&event) {
        let _ = emit_event(&PickerEvent::Failed(reason));
        std::process::exit(2);
    }
    std::process::exit(status);
}

unsafe fn path_bytes(url: id) -> Result<Vec<u8>, String> {
    unsafe {
        if url == nil || url.isFileURL() != YES {
            return Err("picker returned a non-file URL".into());
        }
        let representation: *const c_char = msg_send![url, fileSystemRepresentation];
        if representation.is_null() {
            return Err("picker URL has no filesystem path".into());
        }
        let bytes = CStr::from_ptr(representation).to_bytes().to_vec();
        if bytes.first() != Some(&b'/') {
            return Err("picker returned an invalid path".into());
        }
        Ok(bytes)
    }
}

// Preserve the GPUI macOS 15 duplicate-extension workaround byte-for-byte
// in effect, without converting a selected filename to a lossy String.
fn normalize_save_extension(path: &mut Vec<u8>, major_version: u64) {
    if major_version < 15 {
        return;
    }
    let start = path
        .iter()
        .rposition(|byte| *byte == b'/')
        .map_or(0, |index| index + 1);
    let chunks: Vec<&[u8]> = path[start..].split(|byte| *byte == b'.').collect();
    if chunks.len() == 3 && chunks[1].starts_with(chunks[2]) {
        let length = start + chunks[0].len() + 1 + chunks[1].len();
        path.truncate(length);
    }
}

pub(super) fn run(request: PickerRequest) -> Result<(), String> {
    // Early CLI dispatch guarantees this is the process main thread. Use
    // public AppKit APIs only; no defaults, private entitlements or DB access.
    unsafe {
        let pool = NSAutoreleasePool::new(nil);
        let app = NSApplication::sharedApplication(nil);
        let accessory = NSApplicationActivationPolicy::NSApplicationActivationPolicyAccessory;
        // LSUIElement helpers already have this policy. AppKit returns NO for
        // an idempotent set, which is not an initialization failure.
        let current_policy: isize = msg_send![app, activationPolicy];
        if current_policy != accessory as isize && app.setActivationPolicy_(accessory) == NO
        {
            return Err("picker cannot set accessory activation policy".into());
        }
        app.finishLaunching();
        let (panel, save) = match request {
            PickerRequest::Open {
                files,
                directories,
                multiple,
                prompt,
            } => {
                let panel = NSOpenPanel::openPanel(nil);
                if panel == nil {
                    return Err("picker cannot create open panel".into());
                }
                panel.setCanChooseFiles_(if files { YES } else { NO });
                panel.setCanChooseDirectories_(if directories { YES } else { NO });
                panel.setAllowsMultipleSelection_(if multiple { YES } else { NO });
                panel.setCanCreateDirectories(YES);
                panel.setResolvesAliases_(NO);
                if let Some(prompt) = prompt {
                    let string = NSString::alloc(nil).init_str(&prompt).autorelease();
                    let _: () = msg_send![panel, setPrompt: string];
                }
                (panel, false)
            }
            PickerRequest::Save {
                directory,
                suggested_name,
            } => {
                let panel = NSSavePanel::savePanel(nil);
                if panel == nil {
                    return Err("picker cannot create save panel".into());
                }
                let directory =
                    CString::new(directory).map_err(|_| "picker invalid directory".to_string())?;
                let url: id = msg_send![class!(NSURL), fileURLWithFileSystemRepresentation: directory.as_ptr() isDirectory: YES relativeToURL: nil];
                if url == nil {
                    return Err("picker cannot create directory URL".into());
                }
                panel.setDirectoryURL(url);
                panel.setCanCreateDirectories(YES);
                if let Some(name) = suggested_name {
                    let string = NSString::alloc(nil).init_str(&name).autorelease();
                    let _: () = msg_send![panel, setNameFieldStringValue: string];
                }
                (panel, true)
            }
        };
        let completion = ConcreteBlock::new(move |response: NSModalResponse| {
            if response != NSModalResponse::NSModalResponseOk {
                finish(Ok(PickerEvent::Cancelled));
            }
            let selected = (|| {
                if save {
                    let mut bytes = path_bytes(panel.URL())?;
                    let info = NSProcessInfo::processInfo(nil);
                    normalize_save_extension(
                        &mut bytes,
                        info.operatingSystemVersion().majorVersion,
                    );
                    Ok(PickerEvent::Selected(vec![bytes]))
                } else {
                    let urls = panel.URLs();
                    if urls == nil || urls.count() == 0 {
                        return Err("picker returned empty selection".into());
                    }
                    let mut paths = Vec::new();
                    for index in 0..urls.count() {
                        paths.push(path_bytes(urls.objectAtIndex(index))?);
                    }
                    Ok(PickerEvent::Selected(paths))
                }
            })();
            finish(selected);
        })
        .copy();
        let _: () = msg_send![panel, beginWithCompletionHandler: completion];
        app.activateIgnoringOtherApps_(YES);
        // Never announce Ready before synchronous panel construction/config.
        emit_event(&PickerEvent::Ready)?;
        app.run();
        pool.drain();
        Err("picker event loop ended without a result".into())
    }
}
