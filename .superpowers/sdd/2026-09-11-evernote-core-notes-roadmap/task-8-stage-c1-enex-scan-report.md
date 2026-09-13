# Task 8 / Stage C1 ENEX scan report

## Scope and result

Implemented `app_lite_core::scan_enex_file`: a read-only ENEX evidence scan.
It has no repository, profile, SQLite, blob-store, or GPUI dependency, so it
cannot mutate a live library. This is not staging, import, export, or sync.

The report preserves note ordinal/title/raw timestamps/repeated tags, ENML byte
size and SHA-256, nested `en-media` MD5/type references, resource occurrence
metadata, decoded-byte MD5/SHA-256/size, duplicate MD5s, unresolved media,
unreferenced real resources, MIME mismatches, and `table` as an unsupported
fidelity construct. Resource occurrences are deliberately not deduplicated.

## Source to Rust mapping

| Source evidence | C1 behavior |
| --- | --- |
| Evernote reconstructed `36364__note-import-mutation-import-enex-file-import-file-from-url.js` uses SAX input, sanitizes content, then invokes its mutation | C1 keeps only SAX-style evidence collection; it intentionally contains no sanitizer, mutation, or profile write. |
| Renderer module `916042` allowlists `en-media` `hash`/`type` and `en-todo` `checked` | The ENML scanner recognizes `en-media` at any nesting depth and retains hash/type. `en-todo` remains retained in the content digest; no canonicalization/import is claimed. |
| Evernote `getAttributeResourceFromElement` lowercases `en-media` hash and falls back to its `type` | References are normalized to lowercase MD5 while retaining the declared MIME for mismatch evidence. |
| Joplin `import-enex.ts::processNoteResource` writes base64 to a temporary file, decodes it, then uses decoded-byte MD5 for `en-media` identity | C1 decodes only bounded resources, calculates decoded-byte MD5 separately from SHA-256, and correlates media by MD5. |
| Joplin `import-enex-html-gen.ts::enexXmlToHtml_` finds nested `en-media` and retains unmatched attachments | C1 separately reports unresolved media and unreferenced real resource occurrences; neither is silently discarded. |

## TDD evidence

1. RED: `cargo test --test enex_scan` failed with unresolved `EnexScanError`,
   `EnexScanReport`, and `scan_enex_file` symbols before implementation.
2. GREEN: added the scanner and golden fixture; `cargo test --test enex_scan`
   passed all four tests.
3. RED: added the internal-entity safety assertion; it failed because the
   scanner accepted an internal `<!ENTITY>` declaration.
4. GREEN: rejecting internal entity declarations made that test pass. The
   ordinary external ENEX DOCTYPE remains accepted inertly and is never fetched.

The golden tests cover Chinese title, raw dates, repeated tags, rich/nested
ENML (`en-todo` and `en-media` inside `<i>/<div>`), table detection, inline
image plus unreferenced PDF, duplicate bytes/MD5, orphan media, MIME mismatch,
invalid and non-base64 data, malformed nesting, internal entity declaration,
and a generated 21 MiB data field.

## Commands and results

```text
cargo test --test enex_scan
4 passed; 0 failed

cargo test --quiet
all app-lite-core unit, integration, and doc tests passed (including 67 unit,
4 document roundtrip, 4 ENEX, 27 JEX, 15 migration, 8 organization, 7
repository-flow, and 7 resource-store tests)

git diff --check
no whitespace errors
```

## Limits and explicit follow-up gate

### Re-review status: NEEDS_CONTEXT (not C1 acceptance)

This report must not be read as claiming successful large-attachment scanning.
The current generated 21 MiB fixture proves only the rejection path:
`NeedsContext`, not decoded-byte MD5/SHA-256/size evidence. That does **not**
meet the later review requirement to scan a >20 MiB resource successfully.

Although the syntax-reader preflight receives text/CDATA in bounded chunks,
the current public scanner still subsequently uses `quick-xml`. Its unchecked
attribute-value, character-reference, comment, processing-instruction, and
DOCTYPE-system-id callbacks could be materialized by that second parser.
Therefore the current C1 scanner remains restricted to the bounded small-file
contract and is **not approved as a production large-ENEX scanner**.

To clear this gate, replace outer `quick-xml` parsing entirely with one
`xml-syntax-reader::Visitor` state machine: cap every retained metadata and
attribute accumulator, reject/ignore bounded comment/PI/DTD bodies without
passing them to another parser, and incrementally base64-decode each `data`
callback using at most a carry of three base64 characters. The decoder must
update MD5, SHA-256, and byte count per decoded chunk. A generated >20 MiB
fixture must then succeed and assert both digests, byte count, and an
observable fixed maximum callback/buffer size. Until that implementation and
test exist, the correct status is `NEEDS_CONTEXT`.

### Review correction: bounded outer syntax preflight

The initial raw-byte preflight was replaced after independent review: it could
misread CDATA/comment/quoted contexts, arbitrarily rejected a legal tag over
256 bytes, and `quick-xml` could still materialize a long non-data text event.
C1 now uses `xml-syntax-reader` with a fixed 64 KiB input buffer before the
existing semantic scan. Its visitor receives text, CDATA, DOCTYPE, and
attribute-value chunks at buffer boundaries; C1 keeps its own element stack,
rejects mismatched nesting/entities/internal DTD entities/non-UTF-8 XML, and
enforces field limits before `quick-xml` is invoked. The semantic parser sees
only input whose data field is at most 4 MiB, content at most 4 MiB, and other
text at most 16 KiB.

A `<data>` text payload over 4 MiB returns `EnexScanError::NeedsContext`
before `quick-xml` parsing, including the generated 21 MiB test fixture. C1
**does not claim >20 MiB streaming decode/import success**. Later staging
needs a proven incremental base64 decoder (for example, an isolated temporary
file plus incremental MD5/SHA-256) before it can accept large attachments.

The new mutation-sensitive tests also prove that a 128 KiB ordinary resource
is accepted (not constrained by the 16 KiB metadata limit), a media reference
in note A cannot be satisfied by same-MD5 bytes in note B, literal `<data>`
inside ENML CDATA is ordinary ENML, a 300-byte legal attribute is accepted,
malformed ENML is rejected by a strict event stack, and an oversized non-data
CDATA field is rejected by the outer bounded visitor.

ENML content is retained only up to 4 MiB and ordinary metadata fields up to
16 KiB; note/resource occurrence counts are capped at 50,000. The syntax
reader itself has a documented 1,000-byte atomic XML-name cap; tags with normal
long attribute values are streamed and accepted, but a name beyond that cap is
rejected rather than buffered without bound. Tables are reported rather than
flattened because `CanonicalDocument` has no table model.
