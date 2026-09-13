# Task 7 D3b-3: bounded image OCR to automatic attachment search

Date: 2026-09-13. Candidate code checkpoint: `1286dda0b` on
`codex/joplin-lite-native-rust`. The prior stable PDF-only checkpoint is
`joplin-lite-native-v0.17.0-automatic-pdf-search-checkpoint` at `967081a2a`.

## Source-first boundary

The unpacked Evernote 11.32.5 desktop bundle's
`34309__module-34309.js::searchText` calls remote
`resourceSearchTextAndRecognition`; `50177__module-50177.js` requests the
resource's `recognition` and `searchText` fields; and
`45897__note-content-fetch.js::downloadNoteAttachmentsRecognitionAndSearchText`
persists returned data via `47391__module-47391.js::setSearchText`. Migration
`59009` builds local attachment-search FTS. This establishes the separate
resource-derived-text index behavior we reproduce. It does **not** establish
Evernote's internal OCR algorithm. Local macOS Vision OCR is our independent
offline-first implementation choice.

## Accepted scope

- The existing same-binary, profile-free child now accepts verified PNG/JPEG
  stdin as well as selectable PDF. ImageIO forces an orientation-transformed
  thumbnail with a 2,000-pixel maximum edge and checks its decoded area is
  at most 4,000,000 pixels before handing that `CGImage` to Vision. Input is
  limited to 20 MiB and output to 1 MiB; the parent retains the existing
  15-second timeout, cancellation, bounded pipe drain and kill/reap behavior.
- The parent coordinator admits only PDF, PNG and JPEG. It preflights
  SHA/MIME/declared size, opens a physically bounded hash-verified descriptor,
  rechecks metadata, and sends the verified MIME to the child. Unknown image
  formats fail `Unsupported` without opening the blob. Results publish only
  through D3a's SHA/version/live-association CAS; an ordinary note save does
  not wait for OCR. The current no-text classification reuses
  `NoSelectableText` to mean no searchable text extracted; it is not yet
  user-facing terminology.
- The extractor identity is `pdfkit-vision-resource-text-v2` without a schema
  version change. The v10 settings sentinel requeues previously failed live
  image resources; selectable PDFs are also re-extracted once under the new
  identity. Existing title/body and filename search projections remain
  independent.
- Initial PDF jobs retain priority. Historical image jobs wait five seconds
  after window mount, run one at a time with 250 ms between jobs, and defer
  500 ms after the broad `SearchProjectionQueued` event (which also fires for
  ordinary text saves). Images order newest first; same-millisecond ties use
  job rowid descending. This protects the initial Cards viewport from a
  historical OCR backlog while letting a new image outrank older images.
- Real checked-in English/Chinese PNG and English JPEG child fixtures pass.
  The ordinary Debug and Release GUI startup smoke each imports English and
  Chinese PNG into a disposable profile, waits for both durable `Indexed`
  states, and confirms content-only search hits the correct note and
  ResourceId. This is a process/database acceptance test, **not** visual UI
  acceptance or personal-library testing.

## Regression discovered and closed

The first image-scheduler integration caused two previously stable mounted
Cards/scale tests to fail. The initial scheduler ran during the first
`run_until_parked` render and the observer saw extra image descriptor opens;
background OCR competing with the viewport is the supported diagnosis, though
the observer did not tag each open by task origin. We did not delete or weaken
those assertions.
The delayed/paced image scheduler restored both. A second review caught an
empty-queue 50 ms SQLite rescan after the delay expired; the scheduler now
arms the deadline only when it actually deferred a pending image. A final
one-shot test was made order-independent after image priority changed; the
separate queue-order test verifies PDF-first/new-image-first behavior.

## Controller verification on `1286dda0b`

- Core `test-support` all targets passed. Extractor integration **17/17**.
  GPUI main binary **1,312 passed / 0 failed / 1 filtered**; the filter is
  exactly the documented pre-existing donor SIGSEGV test
  `editor::selection::tests::cross_block_cut_writes_markdown_deletes_range_and_undo_restores`.
  Both crate format checks and `git diff --check` passed.
- Explicit ignored ordinary-GUI smoke, serialized: Debug **2/2** and fresh
  Release **2/2** (PDF plus real-image OCR). The Release build timestamp is
  2026-09-13 08:09:16 +0800, 9,424,736-byte executable, SHA-256
  `14d9a8a3de8833f0c7e26559a40490d63a5fd298f5e6ed79959495104e8d8376`.
  `otool -L` lists existing ImageIO, but no direct Vision or PDFKit link.
- Release smoke's informational, post-index parent RSS sample: PDF profile
  59,872 KiB; two-image OCR profile 82,496 KiB. Its 10 ms child polling is
  not a strict peak. Separately, macOS `/usr/bin/time -l` on the same Release
  binary measured maximum resident set size **132,726,784 bytes** for the
  Chinese PNG Vision child and **26,214,400 bytes** for the selectable-PDF
  child. These are single-fixture process maxima, not a large-library or
  whole-app performance claim.

## Open gaps

HEIC/WEBP/GIF OCR, scanned-PDF OCR, EXIF-rotation fixture verification,
visible search-result acceptance, large personal-library throughput and
strict overall app-memory measurement remain open. Vision's ~133 MB child
peak for this fixture is transient but material; the implementation should
not be described as low-memory without broader measurements. The ignored
smoke's timeout cleanup still permits a short-lived PDF/image grandchild
until the product child's 15-second timeout. No mounted fake-clock test yet
proves the exact 5 s/250 ms/500 ms boundaries; prolonged text-saving activity
or a sustained PDF backlog can intentionally defer image OCR.
