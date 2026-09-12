//! One-shot, profile-free attachment text extraction child.
//!
//! D3b-1 intentionally exposes no scheduler here. The parent must obtain a
//! hash-verified descriptor from core and stream it to this process; this
//! module never resolves a profile path or touches SQLite.

use std::io::{Read, Write};

const MAX_INPUT_BYTES: usize = 20 * 1024 * 1024;
const MAX_OUTPUT_BYTES: usize = 1024 * 1024;
const MAX_PAGES: usize = 500;

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
