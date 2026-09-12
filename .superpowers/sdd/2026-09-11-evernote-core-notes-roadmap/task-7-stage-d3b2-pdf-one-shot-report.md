# Task 7 D3b-2A/B: bounded, one-shot PDF-to-search bridge

Date: 2026-09-13. Accepted code through `2b92b5982`, after the D3b-1 child
checkpoint `7d8e8af99`. This is an internal capability checkpoint, **not**
automatic attachment indexing or a completed search UX.

The source-first basis remains the unpacked Evernote 11.32.5 attachment
`searchText`/`recognition` retrieval and separate local attachment-text index,
as recorded in `task-7-stage-d3b-macos-extractor-brief.md`. PDFKit extraction
in a local subprocess is our independent offline-first implementation choice;
the Evernote source did not establish its server extraction algorithm.

The parent now takes one durable D3a job, preflights current SHA/MIME/declared
size, then opens a content-hash-verified descriptor with a 20 MiB physical
file ceiling. The new core bounded-open variant checks `fstat` before hashing
and counts bytes during fixed-buffer hashing, while the pre-existing unbounded
open path remains available to other stable consumers. The parent rechecks
returned metadata and descriptor length, sends only the descriptor to the
same-binary PDF child, drains bounded output/error pipes, enforces a 15-second
timeout, and attempts kill/reap on error paths. Clean UTF-8 text is published
only through D3a's SHA/version/live-association CAS. Non-PDF, corrupt,
oversized, locked, no-selectable-text and timeout outcomes persist typed
failure states without making a note save fail. Extractor identity is now
`pdfkit-selectable-text-v1`; the existing v10 settings sentinel requeues live
resources after a version change.

The integration test imports the checked-in real selectable PDF into a
disposable library, associates it with a note whose title/filename do not
contain the target words, runs the actual child, then finds both `English` and
`中文可选文字检索` as attachment matches with the expected ResourceId. Additional
tests cover corrupt PDF, metadata and physical-file oversize, a stale SHA,
and unsupported image. The physical-file test grows a blob beyond 20 MiB
while its database metadata remains small, then verifies `TooLarge` before
hashing the entire file. The independent final review found **0 Critical / 0
Important** for this one-shot slice after closing process-cleanup, typed
stderr and both pre-hash budget findings.

Controller's independent checks on the accepted code: extractor integration
**10/10**, core `test-support` all targets pass, GPUI main **1,308 passed / 0
failed / 1 exact documented donor SIGSEGV test filtered**, both crate format
checks, and `git diff --check`. A default unfiltered GPUI run hit that same
pre-existing donor SIGSEGV; it was not described as a passing run. The Release
build completed during concurrent development, so its binary hash is not
treated as proof of this exact commit.

Still missing: GUI/background scheduler and event-driven refresh, image OCR,
scanned-PDF OCR, real idle-parent/peak-child RSS, and a live app smoke. The
one-shot coordinator must **not** be invoked on the GPUI foreground executor
or from save/quit barriers. A hostile external in-place rewrite of a blob
after descriptor hash verification is a later TOCTOU-hardening case; ordinary
ResourceStore publication uses temporary-file replacement.
