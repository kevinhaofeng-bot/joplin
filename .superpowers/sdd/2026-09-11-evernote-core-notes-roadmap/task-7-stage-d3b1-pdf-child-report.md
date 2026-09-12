# Task 7 D3b-1: isolated selectable-PDF child

Date: 2026-09-13. Base: pushed D3a foundation `5185c320d`.
Implementation: `418f2f7ce`, `39bafc550`, `7a0353d62`, `33a290e17`,
`7017c4823`, plus `5a3fb82f6` entry-point formatting. This is a **bounded
child-process component**, not automatic PDF indexing or D3b/M2 acceptance.

The Evernote 11.32.5 source relationship is recorded in
`task-7-stage-d3b-macos-extractor-brief.md`: its desktop code fetches
attachment `searchText`/`recognition` from a remote Quasar/GraphQL path, then
indexes that text locally. Our PDFKit extraction is an independent offline
single-user decision; it is not a claim to reproduce Evernote's extraction
algorithm.

The executable checks `--extract-resource-text` before GPUI and profile
initialization. Child mode receives only `--mime application/pdf` and stdin,
reads at most 20 MiB + sentinel, and exits after one PDF. macOS PDFKit is
loaded with `RTLD_LOCAL` inside child mode, not linked as a normal GUI
dependency. A PDF is read page-by-page with a per-page autorelease pool,
500-page cap and 1 MiB UTF-8 output cap. Unsupported MIME, corrupt/locked,
zero-page/no-selectable-text, input/output limit and output-write errors have
nonzero typed failures. The PDFKit handle intentionally remains until the
one-shot child exits, after a review found `dlclose` ahead of the outer pool
was unsafe.

The checked-in 14,890-byte, one-page Quartz PDF fixture contains actual
selectable English and Chinese text. Both implementer and controller ran the
child against it and observed `Joplin Lite PDF fixture English text` and
`中文可选文字检索`; the controller independently ran the bad-PDF nonzero case.
The 3 integration tests actually execute the child for the positive fixture,
bad MIME/corrupt PDF and >20 MiB input. Independent review of the final
code found **0 Critical / 0 Important** for this isolated component after
closing output, lifetime and pool findings.

At final `5a3fb82f6`, controller independently ran the child integration
suite **3/3**, the GPUI binary exact-known-donor-skip suite **1,308 passed /
0 failed / 1 filtered**, both crate formatting checks, diff check, and a
fresh GPUI Release build, all exit 0. The final Release binary SHA-256 is
`1a270b6f6e478ca88977031575f5208c841a2aaeac757fd7c8dc1a51b6f0b20c`.
`otool -L` on both rebuilt Debug and final Release shows AppKit but no direct
PDFKit/Vision dependency; the final Release child read the real English and
Chinese fixture successfully. This proves the link boundary, **not** measured
idle parent or child peak RSS.

Remaining boundaries are material: no parent coordinator, verified resource
descriptor handoff, timeout/kill/reap, CAS publication, image Vision OCR,
scanned-PDF OCR, UI extraction status, or live search refresh has been added.
The child currently uses a 20 MiB `Vec` and `NSData` copy rather than the
0600-spool design; `PDFPage.string` may allocate before the UTF-8 output cap,
so the future parent must enforce process time/memory policy. Locked/no-text,
multi-page, output-limit and broken-pipe fixtures have not yet been added.
No parent idle-RSS or child peak-RSS claim is made from linker inspection.
