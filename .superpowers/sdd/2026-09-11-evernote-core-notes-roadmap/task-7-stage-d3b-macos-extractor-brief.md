# Task 7 D3b: bounded macOS PDF/image text extractor

Status: design boundary for the stage after D3a, 2026-09-13. No extractor is
implemented or accepted by this brief.

Apple's PDFKit provides per-page [`PDFPage.string`](https://developer.apple.com/documentation/pdfkit/pdfpage/string?changes=_8&language=objc)
for selectable PDF text. Vision provides [`VNRecognizeTextRequest`](https://developer.apple.com/documentation/vision/vnrecognizetextrequest?changes=_1&language=objc)
and [`VNImageRequestHandler`](https://developer.apple.com/documentation/vision/vnimagerequesthandler?changes=_9_5)
for image OCR. These platform APIs are implementation candidates; the app's
document model, index, job identity, save and sync remain Rust-owned.

The parent GUI should claim at most one durable D3a job, open the blob through
`LibraryRepository::open_verified_resource_file` (hash-verified descriptor),
and stream it with a fixed buffer to an on-demand **Rust child mode of the same
binary** before publishing a compare-and-swap result. The child starts before
GPUI initialization, never receives a profile path, reads only stdin plus a
validated MIME/size budget, emits bounded UTF-8 text or typed error over
stdout/stderr, and exits after one job. This isolates PDFKit/Vision transient
allocations from the main app's steady RSS and makes child failure retryable;
it is a hypothesis requiring measured parent/child peak RSS, not a performance
claim.

For PDF, spool the bounded verified stream into a 0600 short-lived file, open
it with PDFKit, and read pages one at a time under autorelease pools. Enforce
page, input-byte, output-byte and execution-time budgets; report truncation,
password-protection and parse failure explicitly. A scanned page with no
selectable text is not automatically searchable until a separate PDF-page OCR
cut is implemented. For PNG/JPEG, use the existing ImageIO thumbnail route to
make an oriented ≤2048px-edge, ≤4-million-pixel `CGImage` before Vision; do not
hand compressed attacker-controlled dimensions directly to full-resolution
OCR. Keep one child/job at a time; don't run extraction in note-save or GPUI
foreground callbacks.

Acceptance requires real (not injected) PDF-with-text and Chinese/English
image fixtures, wrong/oversized/corrupt blob failures, cancellation/restart,
hash/version stale-result rejection, no foreground save wait, parent idle RSS
and child peak RSS, and visible Search results after reopen. Any unmeasured
memory or scanned-PDF gap remains explicit. No network/AI OCR service is
silently substituted.
