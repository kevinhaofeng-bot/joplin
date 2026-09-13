# Task 8 Stage B: JEX body-reference preflight checkpoint

Date: 2026-09-13. Branch: `codex/joplin-lite-native-rust`. This is still a
read-only archive preflight, **not** an importer or M3 migration acceptance.

## Source-to-implementation crosswalk

- The reconstructed Evernote import path
  `36364__note-import-mutation-import-enex-file-import-file-from-url.js`
  keeps parsing/progress separate from note mutation. Stage B preserves that
  boundary: no profile/database write is possible from `scan_jex_archive`.
- Checked-out Joplin `BaseItem.ts::serialize/unserialize` defines title, body
  and trailing property lines. `urlUtils.ts::extractResourceUrls` recognizes
  Markdown inline/reference links and HTML image/anchor attributes;
  `Note.ts::linkedItemIds` resolves IDs by item type. Whiteboard
  `parse.ts::parseWhiteboard` and `resolveRef.ts::resolveFileRef` add validated
  `jsoncanvas` file-node `:/<hex-id>` references. The Rust scanner follows
  these source forms and does not treat every `:/id` as a resource.

## Bounded behavior

The existing 4 MiB-per-item JEX scan now extracts canonical internal note-body
references, classifies those whose metadata is a Resource, exempts known
non-resource item IDs, and reports unknown IDs with the source note ID.
`is_clean` is false for unresolved references. A global 50,000 distinct
note/reference-pair budget is enforced **while each note is parsed**, before
later archive entries can accumulate unbounded sets. HTML is scanned without
repeated whole-remainder lowercase copies; tag closure respects quoted `>`.
Malformed Unicode IDs do not panic. The archive remains unextracted and no
personal profile or user export was opened.

## Review and verification

Independent review first rejected a late reference cap and false HTML/type
diagnostics, then caught a quoted-`>` HTML false-clean. All were fixed and the
final scoped review found **0 Critical / 0 Important**. JEX golden tests are
**27/27**. Controller independently ran core `test-support` **189/189**, GPUI
main binary **1,314 passed / 0 failed / 1 exact pre-existing donor test
filtered**, core/GPUI fmt and diff checks. Fresh Release build passed; binary
SHA-256 is
`fcc2345a82ab265ffdc7ecde5f582c8b532501b636c25e392ca9c0f8213a7031`.
Two ignored disposable-profile Release smoke tests for existing image OCR and
selectable-PDF search passed **2/2**; they do not exercise JEX import.

## Explicit limits

Only Joplin's recognized internal link forms are preflighted. The report is
not a full Markdown/HTML parser, a rich-content conversion, an ENEX scan, a
source library count, or proof that staging/commit/export will work. The next
slice is isolated staging with canonical body conversion, resource hash and
relationship verification; it must not replace the live profile before
round-trip and rollback gates pass.
