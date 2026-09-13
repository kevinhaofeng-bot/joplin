# Task 7 D3b-4: attachment search provenance checkpoint

Date: 2026-09-13. Candidate HEAD: `04d2b36b1` on
`codex/joplin-lite-native-rust`. The previous stable checkpoint is
`joplin-lite-native-v0.18.0-image-ocr-search-checkpoint` at `28b94983a`.

## Source-first boundary

The unpacked Evernote 11.32.5 bundle's `34309__module-34309.js::searchText`
requests `resourceSearchTextAndRecognition`; `45897__note-content-fetch.js`
fetches attachment recognition/search text and `47391` persists it. Migration
`59009` supplies local attachment-text FTS. These support the product behavior
of resource-owned searchable text. They do not prescribe the native result
label; `匹配附件：<文件名>` is our own restrained, generic UX choice because a
matched resource can come from filename/MIME filters, filename terms, or
extracted PDF/OCR text. It must not imply that every term was OCR-recognized.

## Bounded implementation

- `LibraryRepository::search` keeps its 500-hit page bound. A CTE selects the
  existing projection and matched `ResourceId`; one outer `LEFT JOIN` reads
  that resource's title. It does not read canonical note HTML/body or blob
  bytes, and it does not perform per-card resource queries.
- When an attachment matches, `SearchHit.snippet` and its `NoteProjection`
  snippet carry the same source line. The existing Cmd-K palette consumes the
  former and SearchRoute cards consume the latter. No-resource matches keep
  the old note snippet.
- Independent review caught a mixed-hit regression in the first commit:
  `filename:invoice meeting` showed only the attachment source, dropping
  the body summary. The second commit preserves body context first and then
  appends the source on another line. A filename-only note stays compact.
  The combined string is bounded to 160 Unicode scalar values, with a
  UTF-8-safe filename limit and short extension retained.

## Verification and limits

- Controller independently ran core `test-support` all targets (161 tests),
  extractor integration **17/17** including real English/Chinese Vision
  fixtures, and GPUI main binary **1,314 passed / 0 failed / 1 exact known
  donor test filtered**. Core/GPUI fmt and `git diff --check` passed after a
  test-only formatting correction. Independent code re-review ended **0
  Critical / 0 Important**.
- Fresh Release executable SHA-256:
  `4a8057d4617562b9eb9e22901441b8165d299c6a545cd8feaf16257473d3a9ee`.
  The explicit ignored normal-GUI startup smoke on disposable profiles passed
  **2/2**: selectable PDF and real PNG OCR became searchable automatically.
  An initial attempt used a wrong target path and failed before launching;
  Cargo metadata resolved the shared target and the correct run passed.
- Mounted UI tests prove Cmd-K result packet and Enter→SearchRoute projection
  propagation, not pixels. The screen automation transport was unavailable;
  no visual screenshot acceptance or personal-library test is claimed.
  Snippets density has a one-line clamp and may show only the body half of a
  mixed result; the full palette and two-line Cards density are the current
  intended presentation. HEIC, scanned-PDF OCR, 1,662-note search latency,
  overall strict memory measurement and broader Evernote-core/M2 acceptance
  remain open.
