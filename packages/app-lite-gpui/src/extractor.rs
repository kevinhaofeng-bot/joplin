//! One-shot, profile-free attachment text extraction child.
//!
//! D3b-1 intentionally exposes no scheduler here. The parent must obtain a
//! hash-verified descriptor from core and stream it to this process; this
//! module never resolves a profile path or touches SQLite.

use std::fs::File;
use std::io::{Read, Write};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

const MAX_INPUT_BYTES: usize = 20 * 1024 * 1024;
const MAX_OUTPUT_BYTES: usize = 1024 * 1024;
const MAX_PAGES: usize = 500;
const CHILD_TIMEOUT: Duration = Duration::from_secs(15);

#[derive(Debug, PartialEq, Eq)]
pub enum PdfChildError {
    TooLarge,
    Spawn,
    Timeout,
    OutputTooLarge,
    StderrTooLarge,
    Failed,
    Utf8,
    Io,
    Parse,
    Locked,
    NoSelectableText,
    Unsupported,
}

/// Background-only bridge for an already hash-verified core descriptor. It
/// never receives a profile path or materializes the PDF in parent memory.
pub fn run_pdf_child_for_verified_file(
    mut file: File,
    expected_size: i64,
) -> Result<String, PdfChildError> {
    let exe = std::env::current_exe().map_err(|_| PdfChildError::Spawn)?;
    run_pdf_child_for_verified_file_with_exe(file, expected_size, exe)
}

pub fn run_pdf_child_for_verified_file_with_exe(
    mut file: File,
    expected_size: i64,
    exe: std::path::PathBuf,
) -> Result<String, PdfChildError> {
    if !(0..=(MAX_INPUT_BYTES as i64)).contains(&expected_size) {
        return Err(PdfChildError::TooLarge);
    }
    if file.metadata().map_err(|_| PdfChildError::Io)?.len() != expected_size as u64 {
        return Err(PdfChildError::Io);
    }
    use std::io::Seek;
    file.rewind().map_err(|_| PdfChildError::Io)?;
    let mut child = Command::new(exe)
        .args(["--extract-resource-text", "--mime", "application/pdf"])
        .stdin(Stdio::from(file))
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|_| PdfChildError::Spawn)?;
    let mut stdout = child.stdout.take().ok_or(PdfChildError::Spawn)?;
    let mut stderr = child.stderr.take().ok_or(PdfChildError::Spawn)?;
    let out = std::thread::spawn(move || drain_bounded(&mut stdout, MAX_OUTPUT_BYTES));
    let err = std::thread::spawn(move || drain_bounded(&mut stderr, 4096));
    let start = Instant::now();
    loop {
        if let Some(status) = child.try_wait().map_err(|_| PdfChildError::Io)? {
            let out = out
                .join()
                .map_err(|_| PdfChildError::Io)?
                .map_err(|_| PdfChildError::Io)?;
            let err = err
                .join()
                .map_err(|_| PdfChildError::Io)?
                .map_err(|_| PdfChildError::Io)?;
            if out.1 {
                return Err(PdfChildError::OutputTooLarge);
            }
            if err.1 {
                return Err(PdfChildError::StderrTooLarge);
            }
            if !status.success() {
                return Err(classify_child_failure(&err.0));
            }
            return String::from_utf8(out.0).map_err(|_| PdfChildError::Utf8);
        }
        if start.elapsed() >= CHILD_TIMEOUT {
            let _ = child.kill();
            let _ = child.wait();
            let _ = out.join();
            let _ = err.join();
            return Err(PdfChildError::Timeout);
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}

fn classify_child_failure(stderr: &[u8]) -> PdfChildError {
    let marker = std::str::from_utf8(stderr).ok().and_then(|stderr| {
        let mut markers = stderr
            .lines()
            .filter_map(|line| line.strip_prefix("joplin-lite-extractor:"));
        let marker = markers.next()?;
        markers.next().is_none().then_some(marker)
    });
    match marker {
        Some("pdf-parse-failed") => PdfChildError::Parse,
        Some("pdf-locked") => PdfChildError::Locked,
        Some("pdf-no-selectable-text") => PdfChildError::NoSelectableText,
        Some("unsupported-mime") => PdfChildError::Unsupported,
        Some("input-too-large") | Some("output-limit") => PdfChildError::OutputTooLarge,
        _ => PdfChildError::Failed,
    }
}

fn drain_bounded(reader: &mut impl Read, limit: usize) -> std::io::Result<(Vec<u8>, bool)> {
    let mut kept = Vec::new();
    let mut buffer = [0_u8; 8192];
    let mut overflow = false;
    loop {
        let read = reader.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        let remaining = limit.saturating_sub(kept.len());
        let take = remaining.min(read);
        kept.extend_from_slice(&buffer[..take]);
        overflow |= take != read;
    }
    Ok((kept, overflow))
}

pub fn run_child(args: &[String]) -> i32 {
    let mut mime = None;
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--mime" => {
                i += 1;
                mime = args.get(i).map(String::as_str);
            }
            _ => return fail("invalid-arguments"),
        }
        i += 1;
    }
    if mime != Some("application/pdf") {
        return fail("unsupported-mime");
    }
    let mut input = Vec::new();
    if std::io::stdin()
        .take((MAX_INPUT_BYTES + 1) as u64)
        .read_to_end(&mut input)
        .is_err()
        || input.is_empty()
    {
        return fail("input-read-failed");
    }
    if input.len() > MAX_INPUT_BYTES {
        return fail("input-too-large");
    }
    #[cfg(target_os = "macos")]
    match pdf_text(&input) {
        Ok(text) => match std::io::stdout()
            .write_all(text.as_bytes())
            .and_then(|_| std::io::stdout().flush())
        {
            Ok(()) => 0,
            Err(_) => fail("output-write-failed"),
        },
        Err(kind) => fail(kind),
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = input;
        fail("platform-unsupported")
    }
}

fn fail(kind: &str) -> i32 {
    eprintln!("joplin-lite-extractor:{kind}");
    2
}

#[cfg(target_os = "macos")]
fn pdf_text(bytes: &[u8]) -> Result<String, &'static str> {
    use cocoa::base::{id, nil};
    use cocoa::foundation::{NSAutoreleasePool, NSString};
    use objc::{class, msg_send, sel, sel_impl};
    unsafe {
        // Keep PDFKit out of the GUI image; only this child loads it.
        let pdfkit = libc::dlopen(
            c"/System/Library/Frameworks/PDFKit.framework/PDFKit".as_ptr(),
            libc::RTLD_LAZY | libc::RTLD_LOCAL,
        );
        if pdfkit.is_null() {
            return Err("pdfkit-unavailable");
        }
        let _pool = NSAutoreleasePool::new(nil);
        let data: id = msg_send![class!(NSData), dataWithBytes: bytes.as_ptr() length: bytes.len()];
        let document: id = msg_send![class!(PDFDocument), alloc];
        let document: id = msg_send![document, initWithData: data];
        if document == nil {
            return Err("pdf-parse-failed");
        }
        let locked: bool = msg_send![document, isLocked];
        if locked {
            let _: () = msg_send![document, release];
            return Err("pdf-locked");
        }
        let pages: usize = msg_send![document, pageCount];
        if pages == 0 {
            let _: () = msg_send![document, release];
            return Err("pdf-empty");
        }
        if pages > MAX_PAGES {
            let _: () = msg_send![document, release];
            return Err("page-limit");
        }
        let mut out = String::new();
        for index in 0..pages {
            // Drain Foundation temporaries per page instead of retaining a
            // whole long PDF's NSStrings until this one-shot child exits.
            let _page_pool = NSAutoreleasePool::new(nil);
            let page: id = msg_send![document, pageAtIndex: index];
            let value: id = msg_send![page, string];
            if value != nil {
                // UTF-8 byte length is checked before CStr/lossy conversion
                // allocates a Rust-owned copy; include a joining newline.
                let utf8_len: usize = msg_send![value, lengthOfBytesUsingEncoding: 4usize];
                if out.len().saturating_add(utf8_len).saturating_add(1) > MAX_OUTPUT_BYTES {
                    let _: () = msg_send![document, release];
                    return Err("output-limit");
                }
                let pointer = value.UTF8String();
                if !pointer.is_null() {
                    let value = std::ffi::CStr::from_ptr(pointer).to_string_lossy();
                    out.push_str(&value);
                    out.push('\n');
                }
            }
        }
        let _: () = msg_send![document, release];
        // Deliberately retain the RTLD_LOCAL handle. This one-shot child exits
        // immediately, whereas `dlclose` before the outer autorelease pool
        // drains could unload PDFKit while its Foundation objects still live.
        let _ = pdfkit;
        if out.trim().is_empty() {
            return Err("pdf-no-selectable-text");
        }
        Ok(out)
    }
}
