# Task 8 Stage A: read-only JEX migration scan

Date: 2026-09-13. This is a bounded precursor to Task 8, **not** import,
staging, export, personal-library migration, or M3 acceptance. Do not open or
change the user's live Joplin profile in this cut.

## Source crosswalk

- Unpacked Evernote 11.32.5
  `main-readable/src/modules/36364__note-import-mutation-import-enex-file-import-file-from-url.js`
  uses a SAX stream, reports progress/cancellation, parses each note plus its
  resources, and submits an import mutation. Its `64997__import-file.js`
  dispatches ENEX separately from ordinary files. We adopt the behavioral
  separation of read/parse and mutation; the read-only preflight and atomic
  staging policy are our independent safety choice.
- Joplin's checked-out
  `packages/lib/services/interop/InteropService_Exporter_Jex.ts` creates a
  portable tar from Raw export. `InteropService_Exporter_Raw.ts` emits one
  serialized root `.md` item per entity plus `resources/<id>.<extension>`.
  `InteropService_Importer_Jex.ts` extracts it then runs
  `InteropService_Importer_Raw.ts`, which remaps item IDs and relationships,
  and warns on absent resources. `BaseItem.ts::serialize/unserialize` puts
  title/body first and typed property lines after a blank separator; Joplin
  `ModelType` values 1/2/4/5/6 denote note/folder/resource/tag/note-tag.

## Narrow deliverable

Implement a public, streaming, **read-only** `scan_jex_archive(path)` in
`app-lite-core::import_export`. It should return deterministic counts and
source IDs for notes, folders, resource metadata, tags, note-tag relations,
physical resource files, duplicate archive paths/IDs, and missing resource
files. Record unsupported item types and encrypted items explicitly; do not
silently count them as imported. Keep all counts/diagnostics bounded and
avoid reading a full archive or resource blob into memory. Preserve original
source bytes and never mutate the current profile. Reject unsafe tar paths,
symlink/hardlink/device entries, malformed IDs/metadata and duplicate file
paths with an actionable error. A resource file must be streamed for byte
count and SHA-256, not decoded into RAM.

Build synthetic golden JEX fixtures in temporary files: complete note with
folder/tag/resource relation, missing resource, duplicate path, path escape,
unsupported type/encrypted item, and a large streamed resource. Exact report
contract may be chosen in code, but tests must distinguish a clean scan from
incomplete/unsupported data. Keep the API compatible with later
`stage_import`/`verify_staged_import` rather than calling Joplin's live DB.

## Gates and exclusions

Run focused core tests, full core `test-support`, both crate fmt checks and
`git diff --check`. Independently review before promotion. Do not claim
the user's 31 notebooks/64 tags/4,238 resources match until a later
explicit, read-only scan of a user export. ENEX parser, conversion to
CanonicalDocument, UI progress, commit/rollback, readable export, personal
data access, and sync are deliberately out of scope here.
