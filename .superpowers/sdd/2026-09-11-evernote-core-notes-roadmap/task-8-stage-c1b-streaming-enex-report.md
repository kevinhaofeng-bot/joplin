# Task 8 Stage C1b — streaming ENEX scanner report

Status: **DONE_WITH_CONCERNS**. This is a read-only synthetic-fixture scanner,
not ENEX import, staging, export, sync, or M3 acceptance. No personal profile,
live database, GUI, network fetch, or output file is touched by the scanner.

## Source → Rust → test

| Directly re-read source | Implemented contract | Test |
| --- | --- | --- |
| Evernote reconstructed `36364__note-import-mutation-import-enex-file-import-file-from-url.js`: SAX input, per-note content/resources, then mutation | One `xml-syntax-reader::Visitor` outer pass retains bounded note metadata and resource evidence; no mutation dependency | Chinese ENML/resource golden fixture; large-resource scan |
| Renderer `9093.js::916042`: `en-media` `hash`/`type`, `en-todo` `checked` belong to ENML; sanitizer is a separate import step | Bounded ENML inspection retains nested media references and table warning; no claim of sanitizing/canonicalizing | nested `en-media`, `en-todo`, table fixture |
| Evernote common-editor `resource.ts::getAttributeResourceFromElement`: lowercases hash and uses MIME fallback | Lowercase MD5 reference, MIME mismatch evidence; matching scoped to note ordinal | cross-note same-MD5, mismatch and unresolved tests |
| Joplin `import-enex.ts::processNoteResource`: base64 default from DTD, decoded-byte MD5 identity; `import-enex-html-gen.ts::enexXmlToHtml_`: unmatched attachments remain | Default base64, decoded MD5 + SHA-256 + size in digest-only streaming state; duplicate occurrences and unreferenced attachments preserved | 128 KiB, >20 MiB, duplicate/orphan tests |

## RED → GREEN

1. Replaced the old 21 MiB encoded-input `NeedsContext` assertion with a 22,020,096-byte decoded-resource success assertion. RED: `cargo test --test enex_scan streams_a_resource_over_20_mib` failed with `NeedsContext { resource_ordinal: 1, limit: 4194304 }`. GREEN: single-pass visitor and quartet decoder passed.
2. Added oversized attribute/comment/PI/DOCTYPE and duplicate semantic-field test. RED: duplicate title after a resource was accepted. GREEN: per-note/per-resource field sets passed.
3. Added aggregate attribute/tag test. RED: 300 attributes were accepted. GREEN: per-element 256-attribute and per-note 1024-tag limits passed.
4. Existing eight tests remain green; added padding/whitespace callback-boundary and adversarial metadata tests. Final `cargo test --test enex_scan`: 11 passed. Final full `cargo test --quiet`: core suites passed (67 unit; 4 document; 11 ENEX; 27 JEX; 15 migration; 8 organization; 7 repository flow; 7 resource store; doc tests).

## Boundaries and observations

The sole outer parser reads with 64 KiB capacity. The base64 decoder retains only a four-byte quartet, three-byte decoded output, and digest states; it filters ASCII whitespace, rejects malformed padding/trailing data, and caps decoded bytes at `ResourceStore::MAX_RESOURCE_BYTES` (50 MiB). ENML is retained to 4 MiB before the inner quick-xml pass. Individual metadata text/attributes, comment, PI, and DOCTYPE buffers stop at 16 KiB. XML depth stops at 64; note/resource occurrences stop at 50,000. Internal DTD subsets and non-predefined entities are rejected; an external canonical ENEX DOCTYPE is inert and accepted.

`/usr/bin/time -l cargo test --release --test enex_scan streams_a_resource_over_20_mib -- --nocapture` ran the 22 MiB decoded fixture in 0.38 s with `maximum resident set size` 37,126,144 bytes. This is one macOS Release test-process sample, not a general peak-RSS guarantee. The test itself constructs two 22 MiB expected-digest vectors sequentially; the observed process peak therefore includes test allocations, not only scanner state. The generated fixture used 448 writes of 64 KiB base64 blocks plus wrapper writes.

Remaining concerns: the scanner does not preserve unsupported ENEX metadata for import, does not canonicalize ENML, and rejects some valid-but-unmodeled structures fail-closed. An ENEX resource above 50 MiB is explicitly rejected to match the current future ResourceStore limit. The retained `NeedsContext` variant and `MAX_ENEX_DATA_BASE64_BYTES` symbol are legacy API compatibility only; the latter now names callback capacity, not an attachment limit. No real personal export was scanned under this stage's scope.

Commands: `cargo fmt`, focused RED/GREEN commands above, `cargo test --test enex_scan`, `cargo test --quiet`, Release RSS sample, and `git diff --check` (clean).

## Independent review round 1 correction

The review of `b5ae370de` requested two Important corrections, and the controller
identified two more fail-closed/aggregate-memory cases. No part of the JEX or
editor baseline was changed.

RED tests observed before the correction:

1. A 300-byte valid element/attribute/PI target name did not return the new
   name-bound error. The companion 17 KiB name fixtures assert rejection even
   when the syntax reader's own atomic-name cap fires before a visitor callback.
2. `<div/>` in `<content>` was accepted as ENML without an `en-note` root.
   The same test covers bare text, a custom entity, and two roots.
3. `<note>` inside `<note-attributes>` panicked on `Option::unwrap()` in
   `close()`; the test uses `catch_unwind` and requires a structured error.
4. Two notes with 600 large tags each were accepted despite unbounded total
   retained metadata; 400 media references multiplied by 400 same-MD5
   mismatched resources also produced an unbounded derived report.

GREEN: names are capped at 256 bytes before conversion to owned `String`, and
PI targets are checked at callback start. ENML inspection requires exactly one
`en-note` root and validates text entity expansion through `quick-xml`'s
`unescape`; unknown entities fail closed. Attribute containers now allow only
explicit leaf metadata fields, so semantic `note`/`resource` cannot nest there.
An 8 MiB aggregate report-retained budget charges metadata, entity overhead,
media references and derived mismatch/unresolved/duplicate rows before each
append. This budget limits the scanner's report, not the size of a valid ENEX
source; sources over it receive `ReportTooLarge` with no partial report.

Focused `cargo test --test enex_scan` after the correction: **16 passed**,
including the original 22 MiB decoded-resource success case. Final
`cargo test --quiet`: 67 unit, 4 document, 16 ENEX, 27 JEX, 15 migration,
8 organization, 7 repository flow and 7 resource-store tests passed; empty
integration/doc groups also passed. `git diff --check` was clean. This remains
synthetic-fixture-only, read-only Stage C1b, pending independent re-review.
