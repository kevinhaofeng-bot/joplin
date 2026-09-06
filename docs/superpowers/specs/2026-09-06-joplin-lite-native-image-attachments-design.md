# Joplin Lite Native Image Attachments Design

## Goal

Add image paste and Finder drag/drop to the native Rust/AppKit client without
turning the editor into a browser or making network availability part of the
save path. Images must render inline, survive a quit/relaunch cycle, remain
searchable by their alt text, and map cleanly to Joplin resources later.

## Product contract

- Pasting a PNG, JPEG, or macOS screenshot inserts it at the current caret.
- Dragging a normal PNG or JPEG file from Finder inserts it at the drop caret.
- Pasting ordinary text keeps the native AppKit paste behavior.
- Imported images render inline at a readable size and never exceed the text
  column width; their stored bytes are not destructively resized.
- A successful local import and note save does not depend on sync or search.
- Quit and relaunch restore every image at the same logical body position.
- Unsupported, unreadable, or larger-than-10-MiB images leave the note
  unchanged and show a concise failure status.
- Undo removes a newly inserted attachment from the editor. Physical resource
  deletion and garbage collection are outside this MVP.

## Canonical data model

The canonical note body is Joplin-compatible Markdown. An image position is
stored as `![alt](:/0123456789abcdef0123456789abcdef)`. The editor renders this
marker as an `NSTextAttachment`; the marker, not the attachment replacement
character, is persisted in `notes.body`.

Resources use independent 32-character lowercase hexadecimal IDs so their
identity has the same shape as Joplin resource IDs. Blob deduplication uses the
full lowercase SHA-256 of the final stored bytes; the resource ID is never a
truncated digest.

SQLite adds the following tables:

```sql
CREATE TABLE resource_blobs (
    sha256 TEXT PRIMARY KEY NOT NULL,
    size INTEGER NOT NULL CHECK(size >= 0),
    mime TEXT NOT NULL,
    relative_path TEXT NOT NULL,
    created_time INTEGER NOT NULL
);

CREATE TABLE resources (
    id TEXT PRIMARY KEY NOT NULL,
    title TEXT NOT NULL DEFAULT '',
    mime TEXT NOT NULL,
    file_extension TEXT NOT NULL DEFAULT '',
    size INTEGER NOT NULL CHECK(size >= 0),
    sha256 TEXT NOT NULL,
    created_time INTEGER NOT NULL,
    updated_time INTEGER NOT NULL,
    deleted_time INTEGER NOT NULL DEFAULT 0,
    FOREIGN KEY (sha256) REFERENCES resource_blobs(sha256)
);

CREATE INDEX resources_sha256 ON resources(sha256);

CREATE TABLE note_resources (
    note_id TEXT NOT NULL,
    resource_id TEXT NOT NULL,
    is_associated INTEGER NOT NULL DEFAULT 1,
    last_seen_time INTEGER NOT NULL,
    PRIMARY KEY(note_id, resource_id),
    FOREIGN KEY(note_id) REFERENCES notes(id),
    FOREIGN KEY(resource_id) REFERENCES resources(id)
);
```

`notes.body_text` is added as the search projection. Existing rows copy
`notes.body` into it during migration. FTS indexes `title` and `body_text`, not
RTF, resource paths, hashes, or image bytes.

## Blob storage and failure atomicity

Blobs live under `<profile>/resources/blobs/<sha256>`. The resource root and
every blob path must reject symbolic-link traversal using the same fail-closed
policy as the SQLite profile path.

Import validates size, MIME/extension, and AppKit decodability. TIFF clipboard
images are converted to PNG before hashing; PNG and JPEG file bytes are kept as
provided. The importer writes a temporary sibling file, flushes it, and renames
it atomically to the digest path. Existing digest files are reused.

The database transaction then inserts the blob row, resource metadata, note
body, search projection, and note-resource associations. If the blob write
fails, no database row is created. If the transaction fails, the newly written
unreferenced blob may remain but cannot appear in a note; later garbage
collection may reclaim it. Existing blobs are never deleted by an import
rollback.

## Editor serialization

`body_rtf` remains a reconstructible cache and must never embed image bytes.
Each attachment carries its resource ID and alt text in an AppKit-visible
representation. Saving walks the attributed string, replaces each attachment
with its exact Markdown marker, and derives `body_text` by replacing the marker
with its alt text. A separate attributed-string copy replaces attachments with
markers before producing RTF.

Loading starts from cached RTF when valid, otherwise from `notes.body`. It finds
resource markers in canonical body order, resolves metadata and blob bytes,
creates `NSImage` and `NSTextAttachment` values on the main thread, and replaces
the corresponding marker ranges. A missing or corrupt blob leaves the readable
Markdown marker visible and reports a non-fatal warning; it does not erase the
body.

## AppKit integration

A small `NSTextView` subclass owns `paste:` and the `NSDraggingDestination`
callbacks. It delegates accepted image bytes/file URLs to the app controller;
all other paste operations call the superclass implementation. AppKit UI and
attachment creation stay on the main thread. Hashing and disk writes may remain
synchronous for this 10-MiB-capped MVP, but the design keeps the importer behind
a Rust interface so it can move off-thread later.

Accepted first-release inputs are PNG, JPEG, clipboard TIFF, and ordinary local
`file://` PNG/JPEG URLs. WebP, AVIF, animated formats, promised files, PDFs, and
arbitrary binary attachments are explicitly out of scope.

## Repository interfaces

The core exposes typed operations rather than AppKit objects:

```rust
pub struct ResourceImport<'a> {
    pub bytes: &'a [u8],
    pub title: &'a str,
    pub mime: &'a str,
    pub file_extension: &'a str,
}

pub struct StoredResource {
    pub id: String,
    pub title: String,
    pub mime: String,
    pub file_extension: String,
    pub size: i64,
    pub sha256: String,
    pub blob_path: PathBuf,
}

pub fn import_resource(&self, input: ResourceImport<'_>)
    -> Result<StoredResource, CoreError>;
pub fn get_resource(&self, id: &str)
    -> Result<Option<StoredResource>, CoreError>;
pub fn update_note_content(&self, id: &str, input: NoteContentUpdate)
    -> Result<Note, CoreError>;
```

`NoteContentUpdate` includes title, canonical body, body text, RTF cache, and
the complete ordered set of currently referenced resource IDs. The repository
updates the note row and associations in one transaction.

## Sync seam

This phase does not contact the NAS or Joplin Server. The schema intentionally
matches Joplin's resource metadata and `note_resources` concepts. A later sync
adapter will serialize resource metadata as Joplin items and upload blob bytes
to `.resource/<resource-id>` through the official Joplin sync-target protocol.
The current local write path remains the authority and may not wait for that
adapter.

## Verification gates

- Core tests cover migration, SHA-256 deduplication, marker extraction, search
  projection, reopen persistence, associations, and rejected unsafe input.
- A real signed app accepts screenshot paste and Finder PNG/JPEG drag/drop.
- Ordinary text paste still works, and undo/redo remains native.
- After quit/relaunch, image count, resource IDs, body order, and rendered image
  availability are unchanged.
- `cargo fmt --check`, `cargo test`, `cargo clippy -- -D warnings`, release
  bundle, icon contract, and strict ad-hoc signature verification pass.
- The release process has no WebKit or Node child process attributable to it.

## Non-goals

- Joplin Server transport, credentials, locks, cursors, E2EE, or conflicts.
- Physical resource garbage collection.
- Arbitrary file attachments, OCR, annotation, image editing, or galleries.
- HTML import/export or perfect rich-text interoperability with all Joplin
  editor constructs.
