# Task 7 D3a: derived attachment text index contract

Status: bounded implementation brief, 2026-09-13. Start from pushed
`c76790cb7` (D2 accepted). D3a is the storage/search/job seam required for
later actual PDFKit/Vision extraction; **do not claim PDF/OCR is working** from
injected-text tests alone.

## Source/technology boundary

Evernote 11.32.5 readable `83028__module-83028.js::k` unions
`AttachmentSearchText_FTS` through Attachment back to parent Note ID, separate
from note-title/content search. `59009__module-59009.js` and
`98189__module-98189.js` create/rename an attachment `searchText` FTS with
insert/delete/update maintenance and cascade cleanup. That establishes the
derived-text/attachment identity relationship, not an extraction algorithm.
Apple [PDFPage.string](https://developer.apple.com/documentation/pdfkit/pdfpage/string?changes=_8&language=objc)
and [VNRecognizeTextRequest](https://developer.apple.com/documentation/vision/vnrecognizetextrequest?changes=_1&language=objc)
are candidates for the separate macOS extractor stage; no Apple framework
bridge is required in D3a.

## Required D3a cut

1. Add schema v10 disposable per-ResourceId extracted-text index (unicode61 +
   trigram), tied to immutable blob SHA-256 and extractor-version identity.
   Add bounded durable pending/failure state for live associated PDF/images;
   legacy v9 migration must enqueue existing eligible resources without
   reading resource bytes or rewriting note bodies. Resource hard-delete must
   remove derived text/status; detach and soft delete must never leak a hit.
2. Expose a repository API for a bounded pending batch and an atomic
   compare-and-publish extraction result or classified failure. A result for
   stale/missing/deleted/unassociated resource or wrong hash/version must not
   revive search text. Failure/retry must be visible as typed status, not hidden
   behind `已保存`, and must never prevent saving note content.
3. Ordinary terms search the union of existing note title/body, current
   filename FTS, and live associated extracted-text FTS. Keep Latin word,
   Chinese substring, quote and negation semantics and the 500-row page cap.
   Report a stable matching ResourceId for extracted-text-only hits; query
   must not hydrate canonical HTML, source blob bytes, or full extracted text
   into every card row.
4. Preserve v9 filename/filter behavior, source-first evidence, and current
   single-user sync/save paths. No actual PDF/vision parsing, network service,
   content-vector/AI search, or UI OCR controls in this cut.

## Verification gate

Use a synthetic extracted-text result (not fake user-facing OCR) to prove RED
then GREEN for PDF-only searchable text, Chinese 1/2/3-character search,
quoted Latin phrase, detach/soft-delete/purge/stale hash, failure→retry,
reopen/migration queue, exact resource provenance, and no body/blob read in
SearchHit. Keep tests focused: one or two lifecycle fixtures plus the
migration case are preferable to combinatorial test expansion. Full core,
GPUI exact donor-skip, fmt/diff and Release compile follow independent review.
The later D3b extractor must cap per-job input/output, run off the GUI thread
with bounded concurrency, and use the verified ResourceStore descriptor. For
minimal app baseline RSS, prefer an on-demand Rust helper process; evaluate its
actual peak before adopting it. M2 still requires real 1,662-note latency/RSS
and a populated Release UI search check.
