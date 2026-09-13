# Task 8 Stage A: read-only JEX preflight checkpoint

Date: 2026-09-13. Branch: `codex/joplin-lite-native-rust`. Scope is
**archive preflight only**—not an
importer, profile switch, readable export, personal-library migration, or M3.

## Source-to-implementation crosswalk

- Unpacked Evernote 11.32.5 `36364__note-import-mutation-import-enex-file-import-file-from-url.js`
  streams ENEX with progress/cancellation before each note import mutation;
  `64997__import-file.js` dispatches ENEX separately. We preserve that
  parse-versus-mutation boundary. Read-only preflight and staging are our
  independent safety policy, not a copied Evernote algorithm.
- Joplin `InteropService_Exporter_Jex.ts` archives Raw export;
  `InteropService_Exporter_Raw.ts` writes root item `.md` files and resource
  files. `BaseItem.ts::serialize/unserialize` determines the property trailer,
  including property-only NoteTag items. `Resource.filename` and
  `mime-utils.ts::toFileExtension` determine the resource path. A checked-in
  770-row, 23 KB Rust-side table is mechanically generated from Joplin's
  `mime-utils-types.ts`, including its two appended entries, by
  `scripts/generate-joplin-mime-extensions.mjs`; it is not parsed from TS at
  runtime.

## What the scanner does

`app-lite-core::scan_jex_archive` opens only the specified tar file. It
does not extract, mutate SQLite, read the live Joplin profile, or open the
app's own profile. It records deterministic source IDs/counts for notes,
folders, resource metadata, tags and note-tag relations. Resource bytes are
streamed through a 64 KiB buffer for size and SHA-256. It diagnoses missing
resource files, orphan physical files and note-tag relations, duplicate
paths/IDs, encrypted or unsupported items, unknown filenames, zero-byte
resources, and resources exceeding the current 10 MiB image / 50 MiB other
ResourceStore limits.

Archive processing is capped at 50,000 entries, 4 MiB per metadata item and
512 MiB per resource. Unsafe paths, non-regular files, malformed items and
duplicate file paths fail closed. `tar::Archive::entries().raw(true)` exposes
GNU/PAX extension headers for rejection **before** tar's default unbounded
extension-payload read; a synthetic 16 MiB declared longname header regression
tests that boundary. Large resources are hashed incrementally, not held in RAM.

## Review and verification

Independent review found and then closed one Critical tar-extension memory
issue and four Important diagnostics/compatibility issues. The final scoped
review found **0 Critical / 0 Important**; JEX golden tests **17/17** and
generated-table unit test **1/1** passed. Controller independently ran core
`test-support` **177/177**, core/GPUI fmt and diff checks. Generator rerun
left the checked-in table unchanged. GPUI full suite first had one known
intermittent mounted background-scheduler timing failure unrelated to this
core-only cut; the isolated test then passed, and a second full run passed
**1,314/0/1 exact pre-existing donor test filtered**. The scheduler flake
has not been called fixed. GUI Cargo.lock was regenerated to include the new
`tar` core dependency and committed.

The release binary rebuilt successfully after the JEX dependency change
(SHA-256 `8917fdcf5cea106562cd180e2254c336cdf7bd08eca1901fb15b88f63decb7a3`).
Two ignored, disposable-profile release smoke tests for actual image OCR and
selectable-PDF indexing passed **2/2**. These exercise the existing derived
search path, not JEX import or a personal profile.

## Explicit limits

`is_clean()` means the scanned archive structure/metadata relationships and
currently supported ResourceStore sizes passed this preflight. It does **not**
parse note-body internal `:/<id>` links, preserve rich document content,
guarantee that import will succeed, or prove the JEX matches the user's live
Joplin profile. No user archive or personal data was scanned. The next Task 8
slice must parse canonical note bodies, verify internal links and stage into
an isolated profile before any live-profile replacement can be considered.
