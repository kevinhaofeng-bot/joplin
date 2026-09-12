//! One-shot, profile-free attachment text extraction child.
//!
//! D3b-1 intentionally exposes no scheduler here. The parent must obtain a
//! hash-verified descriptor from core and stream it to this process; this
//! module never resolves a profile path or touches SQLite.

use std::fs::File;
use std::io::{Read, Write};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use app_lite_core::{
    DerivedTextFailure, DerivedTextJob, LibraryError, LibraryRepository, ResourceError, ResourceId,
};

const MAX_INPUT_BYTES: usize = 20 * 1024 * 1024;
const MAX_OUTPUT_BYTES: usize = 1024 * 1024;
const MAX_PAGES: usize = 500;
const CHILD_TIMEOUT: Duration = Duration::from_secs(15);
const MAX_IMAGE_EDGE: u32 = 2048;
const MAX_IMAGE_PIXELS: usize = 4_000_000;

#[derive(Debug, PartialEq, Eq)]
pub enum PdfChildError {
    TooLarge,
    Spawn,
    Cancelled,
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

/// A single durable PDF extraction outcome. This coordinator is deliberately
/// synchronous and background-only: a later worker owns scheduling it off the
/// GPUI foreground executor, never from a save or quit barrier.
#[derive(Debug, PartialEq, Eq)]
pub enum DerivedTextCoordinatorOutcome {
    Idle,
    /// The mounted worker was torn down. The durable job remains pending for
    /// a future library open; cancellation is never an extraction failure.
    Cancelled,
    Indexed(ResourceId),
    Failed(ResourceId, DerivedTextFailure),
    Stale(ResourceId),
}

/// Takes and processes at most one durable D3a job. Call this only from a
/// background worker; it can spend up to `CHILD_TIMEOUT` in the PDF child.
pub fn run_one_derived_text_pdf_job(
    repository: &LibraryRepository,
) -> Result<DerivedTextCoordinatorOutcome, LibraryError> {
    let cancelled = AtomicBool::new(false);
    run_one_derived_text_pdf_job_with_cancellation(repository, &cancelled)
}

/// Cancellable background-only variant used by the retained GPUI worker.
/// A pre-cancelled worker deliberately does not take a durable job, and a
/// later cancellation preserves the job's pending state for reopen/retry.
pub fn run_one_derived_text_pdf_job_with_cancellation(
    repository: &LibraryRepository,
    cancelled: &AtomicBool,
) -> Result<DerivedTextCoordinatorOutcome, LibraryError> {
    if cancelled.load(Ordering::Acquire) {
        return Ok(DerivedTextCoordinatorOutcome::Cancelled);
    }
    let job = match repository.take_derived_text_jobs(1)?.pop() {
        Some(job) => job,
        None => return Ok(DerivedTextCoordinatorOutcome::Idle),
    };
    if cancelled.load(Ordering::Acquire) {
        return Ok(DerivedTextCoordinatorOutcome::Cancelled);
    }
    let exe = match std::env::current_exe() {
        Ok(exe) => exe,
        Err(_) => {
            return record_derived_failure_or_cancel(
                repository,
                job,
                DerivedTextFailure::Unavailable,
                cancelled,
            );
        }
    };
    run_derived_text_pdf_job_with_exe_and_cancellation(repository, job, exe, cancelled)
}

pub fn run_one_derived_text_pdf_job_with_exe(
    repository: &LibraryRepository,
    exe: std::path::PathBuf,
) -> Result<DerivedTextCoordinatorOutcome, LibraryError> {
    let cancelled = AtomicBool::new(false);
    run_one_derived_text_pdf_job_with_exe_and_cancellation(repository, exe, &cancelled)
}

/// Narrow test/background seam for a specific executable with cancellation.
pub fn run_one_derived_text_pdf_job_with_exe_and_cancellation(
    repository: &LibraryRepository,
    exe: std::path::PathBuf,
    cancelled: &AtomicBool,
) -> Result<DerivedTextCoordinatorOutcome, LibraryError> {
    if cancelled.load(Ordering::Acquire) {
        return Ok(DerivedTextCoordinatorOutcome::Cancelled);
    }
    let job = match repository.take_derived_text_jobs(1)?.pop() {
        Some(job) => job,
        None => return Ok(DerivedTextCoordinatorOutcome::Idle),
    };
    if cancelled.load(Ordering::Acquire) {
        return Ok(DerivedTextCoordinatorOutcome::Cancelled);
    }
    run_derived_text_pdf_job_with_exe_and_cancellation(repository, job, exe, cancelled)
}

/// Processes a job identity already taken from D3a. Kept public as a narrow
/// test seam; callers normally use `run_one_derived_text_pdf_job`.
pub fn run_derived_text_pdf_job_with_exe(
    repository: &LibraryRepository,
    job: DerivedTextJob,
    exe: std::path::PathBuf,
) -> Result<DerivedTextCoordinatorOutcome, LibraryError> {
    let cancelled = AtomicBool::new(false);
    run_derived_text_pdf_job_with_exe_and_cancellation(repository, job, exe, &cancelled)
}

/// Processes a previously taken job while honouring a shell-lifetime
/// cancellation flag. It is intentionally not a failure path: if cancellation
/// is observed before the final publish/fail decision, the caller leaves D3a's
/// pending identity untouched when the shell closes. This is best-effort: a
/// concurrent durable decision already in progress may finish rather than
/// making window close wait on SQLite or a global cancellation lock.
pub fn run_derived_text_pdf_job_with_exe_and_cancellation(
    repository: &LibraryRepository,
    job: DerivedTextJob,
    exe: std::path::PathBuf,
    cancelled: &AtomicBool,
) -> Result<DerivedTextCoordinatorOutcome, LibraryError> {
    let resource_id = job.resource_id.clone();
    if cancelled.load(Ordering::Acquire) {
        return Ok(DerivedTextCoordinatorOutcome::Cancelled);
    }
    // Metadata is cheap and authoritative enough to reject a MIME we do not
    // support or a file outside this worker's parent-I/O budget. Do this
    // before `open_verified_resource_file`, whose SHA verification streams
    // the complete blob. The descriptor result is checked again below.
    let expected = match repository.resource_metadata(&resource_id)? {
        Some(resource) => resource,
        None => return Ok(DerivedTextCoordinatorOutcome::Stale(resource_id)),
    };
    if expected.sha256 != job.sha256 {
        return Ok(DerivedTextCoordinatorOutcome::Stale(resource_id));
    }
    if expected.mime != "application/pdf" {
        return record_derived_failure_or_cancel(
            repository,
            job,
            DerivedTextFailure::Unsupported,
            cancelled,
        );
    }
    if !(0..=(MAX_INPUT_BYTES as i64)).contains(&expected.size) {
        return record_derived_failure_or_cancel(
            repository,
            job,
            DerivedTextFailure::TooLarge,
            cancelled,
        );
    }
    if cancelled.load(Ordering::Acquire) {
        return Ok(DerivedTextCoordinatorOutcome::Cancelled);
    }
    let (resource, file) =
        match repository.open_verified_resource_file_with_limit(&resource_id, MAX_INPUT_BYTES) {
            Ok(Some(value)) => value,
            Ok(None) => return Ok(DerivedTextCoordinatorOutcome::Stale(resource_id)),
            Err(LibraryError::Resource(ResourceError::SizeLimitExceeded)) => {
                return record_derived_failure_or_cancel(
                    repository,
                    job,
                    DerivedTextFailure::TooLarge,
                    cancelled,
                );
            }
            Err(_) => {
                return record_derived_failure_or_cancel(
                    repository,
                    job,
                    DerivedTextFailure::Unavailable,
                    cancelled,
                );
            }
        };
    if resource.sha256 != expected.sha256
        || resource.mime != expected.mime
        || resource.size != expected.size
        || resource.sha256 != job.sha256
    {
        return Ok(DerivedTextCoordinatorOutcome::Stale(resource_id));
    }
    match run_pdf_child_for_verified_file_with_exe_and_cancellation(
        file,
        resource.size,
        exe,
        cancelled,
    ) {
        Ok(text) => {
            if cancelled.load(Ordering::Acquire) {
                return Ok(DerivedTextCoordinatorOutcome::Cancelled);
            }
            if repository.publish_derived_text(&job, &text)? {
                Ok(DerivedTextCoordinatorOutcome::Indexed(resource_id))
            } else {
                Ok(DerivedTextCoordinatorOutcome::Stale(resource_id))
            }
        }
        Err(PdfChildError::Cancelled) => Ok(DerivedTextCoordinatorOutcome::Cancelled),
        Err(error) => record_derived_failure_or_cancel(
            repository,
            job,
            derived_failure_for_pdf(error),
            cancelled,
        ),
    }
}

fn record_derived_failure(
    repository: &LibraryRepository,
    job: DerivedTextJob,
    failure: DerivedTextFailure,
) -> Result<DerivedTextCoordinatorOutcome, LibraryError> {
    let resource_id = job.resource_id.clone();
    if repository.fail_derived_text(&job, failure.clone())? {
        Ok(DerivedTextCoordinatorOutcome::Failed(resource_id, failure))
    } else {
        Ok(DerivedTextCoordinatorOutcome::Stale(resource_id))
    }
}

fn record_derived_failure_or_cancel(
    repository: &LibraryRepository,
    job: DerivedTextJob,
    failure: DerivedTextFailure,
    cancelled: &AtomicBool,
) -> Result<DerivedTextCoordinatorOutcome, LibraryError> {
    // See the coordinator contract above: this is the last non-blocking
    // cancellation fence before starting a durable failure decision.
    if cancelled.load(Ordering::Acquire) {
        return Ok(DerivedTextCoordinatorOutcome::Cancelled);
    }
    record_derived_failure(repository, job, failure)
}

fn derived_failure_for_pdf(error: PdfChildError) -> DerivedTextFailure {
    match error {
        PdfChildError::Unsupported => DerivedTextFailure::Unsupported,
        PdfChildError::TooLarge | PdfChildError::OutputTooLarge => DerivedTextFailure::TooLarge,
        PdfChildError::Timeout => DerivedTextFailure::Timeout,
        PdfChildError::Parse => DerivedTextFailure::Parse,
        PdfChildError::Locked => DerivedTextFailure::Locked,
        PdfChildError::NoSelectableText => DerivedTextFailure::NoSelectableText,
        PdfChildError::Spawn | PdfChildError::Io => DerivedTextFailure::Unavailable,
        PdfChildError::Cancelled => unreachable!("cancelled extraction is not a failure"),
        PdfChildError::StderrTooLarge | PdfChildError::Failed | PdfChildError::Utf8 => {
            DerivedTextFailure::Failed
        }
    }
}

/// Background-only bridge for an already hash-verified core descriptor. It
/// never receives a profile path or materializes the PDF in parent memory.
pub fn run_pdf_child_for_verified_file(
    file: File,
    expected_size: i64,
) -> Result<String, PdfChildError> {
    let exe = std::env::current_exe().map_err(|_| PdfChildError::Spawn)?;
    run_pdf_child_for_verified_file_with_exe(file, expected_size, exe)
}

pub fn run_pdf_child_for_verified_file_with_exe(
    file: File,
    expected_size: i64,
    exe: std::path::PathBuf,
) -> Result<String, PdfChildError> {
    let cancelled = AtomicBool::new(false);
    run_pdf_child_for_verified_file_with_exe_and_cancellation(file, expected_size, exe, &cancelled)
}

/// Same descriptor-safe child bridge, with a shell-lifetime cancellation
/// token. It only runs on a background executor.
pub fn run_pdf_child_for_verified_file_with_exe_and_cancellation(
    mut file: File,
    expected_size: i64,
    exe: std::path::PathBuf,
    cancelled: &AtomicBool,
) -> Result<String, PdfChildError> {
    if cancelled.load(Ordering::Acquire) {
        return Err(PdfChildError::Cancelled);
    }
    if !(0..=(MAX_INPUT_BYTES as i64)).contains(&expected_size) {
        return Err(PdfChildError::TooLarge);
    }
    if file.metadata().map_err(|_| PdfChildError::Io)?.len() != expected_size as u64 {
        return Err(PdfChildError::Io);
    }
    use std::io::Seek;
    file.rewind().map_err(|_| PdfChildError::Io)?;
    if cancelled.load(Ordering::Acquire) {
        return Err(PdfChildError::Cancelled);
    }
    let mut child = Command::new(exe)
        .args(["--extract-resource-text", "--mime", "application/pdf"])
        .stdin(Stdio::from(file))
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|_| PdfChildError::Spawn)?;
    let mut stdout = match child.stdout.take() {
        Some(stdout) => stdout,
        None => {
            let _ = stop_child(&mut child);
            return Err(PdfChildError::Spawn);
        }
    };
    let mut stderr = match child.stderr.take() {
        Some(stderr) => stderr,
        None => {
            let _ = stop_child(&mut child);
            return Err(PdfChildError::Spawn);
        }
    };
    let out = std::thread::spawn(move || drain_bounded(&mut stdout, MAX_OUTPUT_BYTES));
    let err = std::thread::spawn(move || drain_bounded(&mut stderr, 4096));
    let start = Instant::now();
    loop {
        if cancelled.load(Ordering::Acquire) {
            stop_and_join(&mut child, out, err);
            return Err(PdfChildError::Cancelled);
        }
        let status = match child.try_wait() {
            Ok(status) => status,
            Err(_) => {
                stop_and_join(&mut child, out, err);
                return Err(PdfChildError::Io);
            }
        };
        if let Some(status) = status {
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
            stop_and_join(&mut child, out, err);
            return Err(PdfChildError::Timeout);
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}

/// Stop a child before joining pipe readers, so they always observe EOF. A
/// successful `kill` is followed by `wait` to reap the child. If killing
/// reports an error, only accept a concurrently-exited child; do not fall
/// through to an unbounded `wait` on a process that might still be alive.
fn stop_child(child: &mut std::process::Child) -> bool {
    match child.kill() {
        Ok(()) => child.wait().is_ok(),
        Err(_) => matches!(child.try_wait(), Ok(Some(_))),
    }
}

fn stop_and_join(
    child: &mut std::process::Child,
    out: std::thread::JoinHandle<std::io::Result<(Vec<u8>, bool)>>,
    err: std::thread::JoinHandle<std::io::Result<(Vec<u8>, bool)>>,
) {
    // Do not join readers while the child could still own the write ends: that
    // would turn a failed kill into an unbounded UI-adjacent wait. When an OS
    // kill error races a child exit, `stop_child` confirms it was already
    // reaped; otherwise the handles detach rather than blocking this caller.
    if stop_child(child) {
        let _ = out.join();
        let _ = err.join();
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
    {
        let result = match mime {
            Some("application/pdf") => pdf_text(&input),
            Some("image/png") | Some("image/jpeg") => image_text(&input, mime.unwrap()),
            _ => Err("unsupported-mime"),
        };
        match result.and_then(|text| {
            match std::io::stdout()
                .write_all(text.as_bytes())
                .and_then(|_| std::io::stdout().flush())
            {
                Ok(()) => Ok(()),
                Err(_) => Err("output-write-failed"),
            }
        }) {
            Ok(()) => 0,
            Err(kind) => fail(kind),
        }
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = input;
        fail("platform-unsupported")
    }
}

#[cfg(target_os = "macos")]
fn image_text(bytes: &[u8], mime: &str) -> Result<String, &'static str> {
    use cocoa::base::{id, nil};
    use cocoa::foundation::{NSAutoreleasePool, NSString};
    use objc::{class, msg_send, sel, sel_impl};
    use objc2_core_foundation::{CFBoolean, CFData, CFDictionary, CFNumber, CFString, CFType};
    use objc2_core_graphics::CGImage;
    use objc2_image_io::{
        CGImageSource, kCGImageSourceCreateThumbnailFromImageAlways,
        kCGImageSourceCreateThumbnailWithTransform, kCGImageSourceShouldCache,
        kCGImageSourceThumbnailMaxPixelSize,
    };

    unsafe {
        // Vision must remain absent from the ordinary GUI process. ImageIO is
        // already used by the native image renderer, but this child creates no
        // editor cache and retains only the bounded thumbnail below.
        let vision = libc::dlopen(
            c"/System/Library/Frameworks/Vision.framework/Vision".as_ptr(),
            libc::RTLD_LAZY | libc::RTLD_LOCAL,
        );
        if vision.is_null() {
            return Err("vision-unavailable");
        }
        let _pool = NSAutoreleasePool::new(nil);
        let data = CFData::from_bytes(bytes);
        let source = CGImageSource::with_data(&data, None).ok_or("image-decode-failed")?;
        let image_type = source.r#type().map(|value| (&*value).to_string());
        let expected_type = match mime {
            "image/png" => "public.png",
            "image/jpeg" => "public.jpeg",
            _ => return Err("unsupported-mime"),
        };
        if image_type.as_deref() != Some(expected_type) {
            return Err(if image_type.as_deref() == Some("public.data") {
                "image-decode-failed"
            } else {
                "image-type-mismatch"
            });
        }
        if source.count() == 0 {
            return Err("image-decode-failed");
        }

        // 2000 is the largest square edge that also fits the 4M-pixel cap;
        // it is intentionally no greater than the public 2048px edge limit.
        let thumbnail_edge = MAX_IMAGE_EDGE.min((MAX_IMAGE_PIXELS as f64).sqrt() as u32);
        let create_thumbnail = CFBoolean::new(true);
        let transform = CFBoolean::new(true);
        let should_cache = CFBoolean::new(false);
        let max_pixel_size = CFNumber::new_i32(thumbnail_edge as i32);
        let keys: [&CFString; 4] = [
            kCGImageSourceCreateThumbnailFromImageAlways,
            kCGImageSourceCreateThumbnailWithTransform,
            kCGImageSourceShouldCache,
            kCGImageSourceThumbnailMaxPixelSize,
        ];
        let values: [&CFType; 4] = [
            create_thumbnail.as_ref(),
            transform.as_ref(),
            should_cache.as_ref(),
            max_pixel_size.as_ref(),
        ];
        let options =
            CFDictionary::<CFType, CFType>::from_slices(&keys.map(|key| key as &CFType), &values);
        let image = source
            .thumbnail_at_index(0, Some(options.as_ref()))
            .ok_or("image-decode-failed")?;
        let width = CGImage::width(Some(&image));
        let height = CGImage::height(Some(&image));
        let pixels = width.checked_mul(height).ok_or("image-limit")?;
        if width == 0
            || height == 0
            || width > MAX_IMAGE_EDGE as usize
            || height > MAX_IMAGE_EDGE as usize
            || pixels > MAX_IMAGE_PIXELS
        {
            return Err("image-limit");
        }

        let handler: id = msg_send![class!(VNImageRequestHandler), alloc];
        let handler: id = msg_send![handler, initWithCGImage: &*image options: nil];
        if handler == nil {
            return Err("image-decode-failed");
        }
        let request: id = msg_send![class!(VNRecognizeTextRequest), alloc];
        let request: id = msg_send![request, init];
        if request == nil {
            return Err("vision-unavailable");
        }
        let languages = [
            NSString::alloc(nil).init_str("en-US"),
            NSString::alloc(nil).init_str("zh-Hans"),
        ];
        let languages: id =
            msg_send![class!(NSArray), arrayWithObjects: languages.as_ptr() count: languages.len()];
        let _: () = msg_send![request, setRecognitionLanguages: languages];
        let _: () = msg_send![request, setUsesLanguageCorrection: true];
        if msg_send![request, respondsToSelector: sel!(setAutomaticallyDetectsLanguage:)] {
            let _: () = msg_send![request, setAutomaticallyDetectsLanguage: true];
        }
        let requests: id = msg_send![class!(NSArray), arrayWithObject: request];
        let mut error: id = nil;
        let succeeded: bool = msg_send![handler, performRequests: requests error: &mut error];
        if !succeeded {
            return Err("vision-failed");
        }
        let observations: id = msg_send![request, results];
        let count: usize = msg_send![observations, count];
        let mut text = String::new();
        for index in 0..count {
            let observation: id = msg_send![observations, objectAtIndex: index];
            let candidates: id = msg_send![observation, topCandidates: 1usize];
            let candidate_count: usize = msg_send![candidates, count];
            if candidate_count == 0 {
                continue;
            }
            let candidate: id = msg_send![candidates, objectAtIndex: 0usize];
            let value: id = msg_send![candidate, string];
            if value == nil {
                continue;
            }
            let utf8_len: usize = msg_send![value, lengthOfBytesUsingEncoding: 4usize];
            if text.len().saturating_add(utf8_len).saturating_add(1) > MAX_OUTPUT_BYTES {
                return Err("output-limit");
            }
            let pointer = NSString::UTF8String(value);
            if !pointer.is_null() {
                text.push_str(&std::ffi::CStr::from_ptr(pointer).to_string_lossy());
                text.push('\n');
            }
        }
        source.remove_cache_at_index(0);
        // See the PDF child: retaining a RTLD_LOCAL framework handle until
        // one-shot process exit avoids unloading live autorelease objects.
        let _ = vision;
        if text.trim().is_empty() {
            return Err("image-no-text");
        }
        Ok(text)
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
