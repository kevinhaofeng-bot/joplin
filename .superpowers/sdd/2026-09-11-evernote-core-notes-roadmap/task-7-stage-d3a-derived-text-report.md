# Task 7 D3a: derived attachment text index acceptance

Date: 2026-09-13. Base: pushed `c76790cb7` (D2). Implementation:
`c2191214a` and `067397088`. This report accepts only the attachment-text
index/job contract. There is **no PDFKit/Vision extractor, OCR UI, or automatic
reading of attached PDF/image content** in these commits.

## Source-first mapping

| Readable Evernote 11.32.5 witness | Native implementation | Verification |
| --- | --- | --- |
| `main-readable/src/modules/83028__module-83028.js::k` unions attachment `searchText` back to a parent note separately from title/content. | `app-lite-core::LibraryRepository::search` unions current associated derived text with note and filename indexes, returning a stable matching `ResourceId` without loading canonical HTML or a blob. | Synthetic PDF-only text, Chinese 1/2/3-character and quoted Latin search; detach, Trash, hard delete and stale SHA/version cases. |
| `main-readable/src/modules/59009__module-59009.js` and `98189__module-98189.js` maintain attachment text FTS separately from metadata FTS, including deletion. | Schema v10 owns disposable per-resource unicode/trigram FTS plus a durable pending/indexed/failed queue. Resource hard delete removes FTS rows and jobs; v9 migration queues live associated image/PDF identities without reading blobs or rewriting note bodies. | v9→v10 migration/body sentinel; failure→retry and reopen; hard-delete and status assertions. |

The source establishes the search/index relationship, **not** Evernote's
underlying extraction algorithm. The native SHA-256/extractor-version compare,
1 MiB UTF-8 result cap, and single-worker pending model are local product
decisions. `take_derived_text_jobs` does not lease a job or open a blob; a later
worker must use `open_verified_resource_file` and publish only for the current
identity. A crash before publication leaves the job pending.

Independent review initially found one Important: changing extractor version
hid old results but did not requeue existing v10 attachments. `067397088`
closed it by keeping one version constant and an idempotent settings sentinel:
normal reopen only reads that sentinel; a version change requeues current
associated image/PDF resources within the same migration transaction. Old
indexed text is gated out until replacement publication. Re-review found
**0 Critical / 0 Important**. Two legacy migration tests that still expected
schema v9 were corrected to `SCHEMA_VERSION`.

Controller's final independent verification at `067397088`:

- `cargo test --manifest-path packages/app-lite-core/Cargo.toml --features test-support --quiet`: **155 passed / 0 failed**, across nine nonempty groups.
- `cargo test --manifest-path packages/app-lite-gpui/Cargo.toml --quiet --bin velotype -- --skip cross_block_cut_writes_markdown_deletes_range_and_undo_restores`: **1,308 passed / 0 failed / 1 known donor test filtered**.
- Both crate `cargo fmt --check`, `git diff --check`, and GPUI Release build: exit 0. Release SHA-256: `58a2e5085138851164eeb5fae8f3f5ecfcc44e533fb75cc9b4eacf8e615ce4a5`.

The first controller `test-support` run on `c2191214a` correctly failed in
two v9 hard-coded migration assertions; the results above are the rerun after
`067397088`, not a description of that failed run as passing.

Deferred M2 evidence: 1–2-character CJK queries use `LIKE` over potentially
large extracted text; real 1,662-note latency/RSS/index-size measurements are
needed. A dedicated SearchHit no-body/blob observer assertion and broader
multi-resource version-upgrade fixtures are useful hardening. No populated
Release search, real OCR/PDF extraction, or personal-library migration is
claimed. D3b's separate macOS extractor design boundary is in
`task-7-stage-d3b-macos-extractor-brief.md`.
