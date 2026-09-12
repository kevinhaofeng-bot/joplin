# Task 7 D3b-2: connect real selectable-PDF extraction to local search

Status: bounded next implementation cut after the isolated D3b-1 child passes
real-fixture tests and a Release `otool -L` check. D3a is the durable queue and
CAS index authority. D3b-1 is a one-shot PDFKit child, not an automatic worker.

The unpacked Evernote 11.32.5 `34309__module-34309.js::searchText` gets
attachment text through `quasar.queries.attachment.resourceSearchTextAndRecognition`;
`50177__module-50177.js` requests `searchText`/`recognition` in a GraphQL
response. Local PDFKit extraction here is an independent offline-first choice,
while the separate attachment-text index and association back to a note follow
the observed Evernote search/data relationship.

## Required cut

1. Start one bounded background extraction coordinator for the active local
   library, never on GPUI's foreground executor or in a note-save/quit barrier.
   It takes a D3a PDF job, opens the exact resource via
   `LibraryRepository::open_verified_resource_file`, verifies current SHA/MIME
   and rejects files above 20 MiB before child launch. Pass that verified file
   descriptor as the child stdin (or copy it with a fixed buffer); never pass
   a profile path or unverified resource pathname.
2. Spawn the same binary in `--extract-resource-text --mime application/pdf`
   mode with at most one child at a time. Bound execution time (initial target
   15 s), stdout (1 MiB + sentinel) and stderr (4 KiB); consume both pipes
   without blocking the child, then check exit code and UTF-8. Kill and reap
   timed-out/oversized children. Avoid a whole-input parent allocation.
3. Publish only via D3a `publish_derived_text` compare-and-swap after a clean
   result; classify unsupported/parse/locked/timeout/unavailable failures via
   `fail_derived_text` without marking the note save as failed. A crash before
   publication leaves the durable job pending. Do not retry permanent failures
   in an uncontrolled loop. Image jobs remain explicit unsupported/for later
   Vision D3b-3 rather than blocking PDF jobs forever. Advance the extractor
   identity from the D3a placeholder to a real PDFKit version so v10 reopen
   requeues any previously indexed synthetic/old-version rows; a later Vision
   implementation must advance it again or use per-MIME version identity.
4. Search must be able to return the real PDF text on the next query/reopen,
   with a stable matching resource identity; preserve search history/route,
   title/body/filename behavior and all current editor/save flows.

## Minimum proof

Use the checked-in small selectable-text PDF fixture, not synthetic injected
text: import/associate it on a disposable profile, run the actual coordinator,
and search for both English and Chinese words after publication. Check corrupt,
oversized and stale-SHA cases; assert a foreground save/quit action does not
wait for a running child. Verify an idle GUI binary is not linked to PDFKit,
and record parent idle/child peak RSS before making a memory claim. Keep
scanned-PDF OCR and image Vision separate; a PDF with no selectable text must
show a failed/unsupported status rather than a false search hit.
