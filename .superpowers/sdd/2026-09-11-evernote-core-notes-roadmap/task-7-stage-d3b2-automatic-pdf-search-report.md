# Task 7 D3b-2 C1/C2: automatic selectable-PDF search checkpoint

Date: 2026-09-13. Code checkpoint: `5f0aa4b7e` on
`codex/joplin-lite-native-rust`. This follows the D3b one-shot core checkpoint
`joplin-lite-native-v0.16.0-pdf-search-core-checkpoint`; it does not supersede
the stable title/body search path.

The source-first basis is the unpacked Evernote 11.32.5 attachment
`searchText`/`recognition` retrieval and separate local attachment-text index,
documented in `task-7-stage-d3b-macos-extractor-brief.md`. It establishes the
product behavior to reproduce, **not** Evernote's private extraction algorithm.
Our PDFKit child process is an independent macOS offline-first implementation.

## Accepted behavior

- An independent `LibraryShell` derived-text scheduler wakes at ordinary app
  startup and on `SearchProjectionQueued`. It claims at most one durable D3a
  job under a process-wide lock, runs the bounded PDF child away from the GPUI
  foreground executor, and never makes note save or quit wait for extraction.
- Successful SHA/version/live-association CAS publication emits
  `DerivedTextIndexed`. Open shells invalidate the active search route and
  schedule a fenced refresh. Existing title/body and filename indexing remain
  separate and intact.
- Closing a shell signals best-effort cancellation. If observed before the
  final publish/fail decision, the child is killed and reaped and the job stays
  Pending for a later window/startup. An already-started durable decision may
  finish; close does not synchronously wait for it.
- A macOS-only ignored smoke test launches the actual default GUI binary with
  a disposable absolute profile and real checked-in selectable PDF. An
  independent repository observes Indexed and finds `English` and
  `中文可选文字检索`, each with the expected note and ResourceId. The optional
  **test-only** `JOPLIN_LITE_LIVE_SMOKE_BIN` selects an absolute Release binary.
  This exercises the ordinary startup process and database search, not visual
  UI acceptance or a personal library.

## Controller verification

- Debug ordinary-app smoke: **1/1** passed. Informational RSS after indexing:
  parent 69,248 KiB; greatest observed direct child 21,808 KiB.
- Explicit Release ordinary-app smoke: **1/1** passed. Informational RSS after
  indexing: parent 58,848 KiB; greatest observed direct child 16,896 KiB.
  Child RSS comes from 10 ms polling and is **not** a strict peak. These are
  single small-fixture samples, not the app's overall memory budget.
- GPUI main binary: **1,311 passed / 0 failed / 1 filtered**. The filtered test
  is exactly the documented pre-existing donor SIGSEGV test
  `editor::selection::tests::cross_block_cut_writes_markdown_deletes_range_and_undo_restores`.
- Extractor integration: **13/13**. Core with `test-support`: all targets
  passed. `cargo fmt --check` and `git diff --check` passed.
- A fresh Release build on this source produced
  `/Users/kevinhao/Projects/joplin/.shared-target/release/velotype` at
  2026-09-13 07:16:43 +0800, SHA-256
  `06503b3a2cd29b5f953ba105ce4198e55b8505a657313b506b65ab3418ee03a2`.
  `otool -L` shows no direct PDFKit/Vision link; PDFKit loads only inside the
  child path. The old `f5c597...` hash was a stale Release build and is not
  evidence for this checkpoint.

## Outside this checkpoint

Image OCR, scanned-PDF OCR, UI visual/search-result acceptance, large personal
library performance, and strict peak-memory measurement remain open. The
timeout path of the ignored smoke reaps its GUI child; a briefly surviving PDF
grandchild is a minor test-harness cleanup issue, bounded by the product
child's 15-second timeout.
