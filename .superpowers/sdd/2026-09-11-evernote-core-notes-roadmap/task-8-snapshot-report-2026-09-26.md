# Task 8 consistent snapshot and durable metadata report (2026-09-26)

Status: DONE for the scoped snapshot/metadata continuation; awaiting controller review. No commit or push.

## Baseline and changed files

- Started from `a338ae328` with controller-owned uncommitted `readable_export.rs` and `export_restore.rs` fixes intact. Their history-only resource union, bounded restore reads, descriptor-safe bundle access, and failure cleanup remain present.
- Changed `packages/app-lite-core/src/repository.rs`: connection-owned deferred read transaction, reusable connection-level note/history queries, dedicated notebook/resource durable metadata DTOs, and hash-addressed blob open without a fresh database query.
- Changed `packages/app-lite-core/src/import_export/readable_export.rs`: collect one bounded metadata manifest in the read transaction; write HTML and copy verified resource bytes after lock release; preserve and verify notebook/resource durable fields; version 2 format; remove partial post-copy rechecks. Public export/restore signatures remain unchanged.
- Changed `packages/app-lite-core/tests/export_restore.rs`: durable metadata reopen check, realistic incomplete v1 refusal, and default-notebook stack refusal. Added private deterministic concurrency and lock-release tests in `readable_export.rs`.
- Re-read reconstructed Evernote `11354__enex-exporter.js`: its note loop emits title, created/updated dates, content, and attachments. This bundle's durable notebook/resource timestamps and retained history are independent correctness guarantees, not claims about ENEX history coverage.

## Snapshot and memory boundary

The first default-notebook query establishes the SQLite snapshot on the repository's existing connection. The same transaction reads selected note rows, tag/resource relations, selected thumbnails, retained revisions, and every resource metadata row referenced by current or historical HTML. Pending edit journals still fail closed. The transaction returns an owned manifest, then releases both SQLite transaction and Rust mutex before any HTML file write or blob open/copy. Blob reads use the captured SHA-256 and size. Missing or altered bytes abort publication; later resource metadata cannot enter the manifest.

Collection enforces 10,000 selected notes, 50,000 distinct resources, 4 MiB current/revision HTML and text, 10,000 revisions and 16 MiB retained history per note, plus an accumulating 32 MiB manifest string budget. The bounded serializer also enforces the actual 32 MiB JSON size including escapes. A default notebook in a Stack is refused, since this narrow format has no Stack graph. No library-wide backup claim is made.

## Durable compatibility

Version 2 includes default notebook `revision`, `created_time`, `updated_time` and resource `created_time`, `updated_time`. Restore applies those exact values and checks them through a reopened repository before publishing the target profile. Validation reads the tiny format/version header before the full v2 structure; a genuine v1 manifest without these fields reports `incompatible bundle version 1; expected 2` and leaves the requested target empty. Missing fields are never invented.

## RED / GREEN evidence

- RED: `cargo test --test export_restore readable_export_restores_durable_notebook_and_resource_metadata -- --exact` failed: restored notebook was `(Renamed default, 1, 0, 0)` instead of revision 2 and timestamps 111/222.
- RED: `cargo test --test export_restore readable_restore_rejects_old_incomplete_bundle_version -- --exact` failed because old version 1 restored successfully.
- RED: `cargo test --test export_restore readable_export_rejects_default_notebook_inside_a_stack -- --exact` failed because export accepted a grouped default notebook.
- GREEN: these focused tests pass. A barrier test commits notebook and note/history mutations on a second WAL connection after the first snapshot read; output retains the coherent earlier notebook/note/revision/resource set. Its second seam writes through the original repository handle before blob copy, proving the Rust lock has ended. A failed-selection test immediately saves through the same repository, proving transaction/lock release on error.
- Final `cargo test --quiet`: 243 passed, 0 failed. `cargo fmt --check` and `git diff --check`: passed.

## Remaining boundary

The export is limited to explicitly selected active, untagged notes in an ungrouped default notebook. It cannot replace a whole-profile backup. Concurrent removal of a captured blob may fail the export; the temporary bundle is then removed and the target is not published. No migration path is provided for experimental version 1 bundles because their missing durable fields cannot be reconstructed reliably.

## Independent review round 1: bounded blob copy

The reviewer found that the first implementation opened blobs through unbounded `open_verified`, then copied to EOF. A blob growing after the metadata snapshot could consume unbounded read/write I/O before the size mismatch was detected. `open_readable_export_blob` now passes the captured resource size to the existing `ResourceStore::open_verified_with_limit`; the export copy additionally reads at most `captured size + 1` bytes and rejects any extra byte before writing it. `ResourceStore` and its stable callers were not changed. A source blob that grows or changes may abort the export without publishing the target.

- RED: `cargo test --test export_restore readable_export_rejects_a_blob_physically_larger_than_captured_metadata -- --exact` failed: the old path reported hash corruption rather than the typed physical size-limit error.
- RED: `cargo test --lib growing_reader_stops_after_declared_size_plus_one_byte` failed when the old copy loop attempted a tenth read from a controlled endlessly growing reader whose declared size was 8 bytes.
- GREEN: both targeted tests pass. The former matches `ResourceError::SizeLimitExceeded` and confirms no bundle publication; the latter reads exactly 9 bytes before rejecting. `cargo test --lib import_export::readable_export::tests` passed 9/9, and `cargo test --test export_restore` passed 16/16. `cargo fmt --check` and `git diff --check` passed. No second full core suite was run for this scoped review fix; the prior 243/243 result above predates it.
