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
            "--mime" => { i += 1; mime = args.get(i).map(String::as_str); }
            _ => return fail("invalid-arguments"),
        }
        i += 1;
    }
    if mime != Some("application/pdf") { return fail("unsupported-mime"); }
    let mut input = Vec::new();
    if std::io::stdin().take((MAX_INPUT_BYTES + 1) as u64).read_to_end(&mut input).is_err() || input.is_empty() { return fail("input-read-failed"); }
    if input.len() > MAX_INPUT_BYTES { return fail("input-too-large"); }
    #[cfg(target_os = "macos")]
    match pdf_text(&input) {
        Ok(text) => { let _ = std::io::stdout().write_all(text.as_bytes()); 0 }
        Err(kind) => fail(kind),
    }
    #[cfg(not(target_os = "macos"))]
    { let _ = input; fail("platform-unsupported") }
}

fn fail(kind: &str) -> i32 { eprintln!("joplin-lite-extractor:{kind}"); 2 }

#[cfg(target_os = "macos")]
fn pdf_text(bytes: &[u8]) -> Result<String, &'static str> {
    use cocoa::base::{id, nil};
    use cocoa::foundation::{NSAutoreleasePool, NSString};
    use objc::{class, msg_send, sel, sel_impl};
    #[link(name = "PDFKit", kind = "framework")]
    unsafe extern "C" {}
    unsafe {
        let _pool = NSAutoreleasePool::new(nil);
        let data: id = msg_send![class!(NSData), dataWithBytes: bytes.as_ptr() length: bytes.len()];
        let document: id = msg_send![class!(PDFDocument), alloc];
        let document: id = msg_send![document, initWithData: data];
        if document == nil { return Err("pdf-parse-failed"); }
        let pages: usize = msg_send![document, pageCount];
        if pages > MAX_PAGES { let _: () = msg_send![document, release]; return Err("page-limit"); }
        let mut out = String::new();
        for index in 0..pages {
            let page: id = msg_send![document, pageAtIndex: index];
            let value: id = msg_send![page, string];
            if value != nil {
                let pointer = value.UTF8String();
                if !pointer.is_null() {
                    let value = std::ffi::CStr::from_ptr(pointer).to_string_lossy();
                    if out.len().saturating_add(value.len()) > MAX_OUTPUT_BYTES { let _: () = msg_send![document, release]; return Err("output-limit"); }
                    out.push_str(&value);
                    out.push('\n');
                }
            }
        }
        let _: () = msg_send![document, release];
        Ok(out)
    }
}
