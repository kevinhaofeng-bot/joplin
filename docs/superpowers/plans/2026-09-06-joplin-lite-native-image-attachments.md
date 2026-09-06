# Joplin Lite Native Image Attachments Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add native inline image paste and Finder drag/drop with durable, deduplicated, Joplin-compatible local storage.

**Architecture:** Canonical note bodies contain Joplin resource markers while SHA-256-addressed files hold image bytes. Pure Rust modules own marker projection and durable resource storage; AppKit owns paste/drop callbacks and `NSTextAttachment` rendering. `body_rtf` remains a rebuildable cache without embedded image data.

**Tech Stack:** Rust 2024, rusqlite, sha2, AppKit through objc2/objc2-app-kit, SQLite FTS5, Bash/macOS bundle tools.

**Spec:** `docs/superpowers/specs/2026-09-06-joplin-lite-native-image-attachments-design.md`

## Global Constraints

- The shipped app must contain no WebKit, Electron, browser runtime, or resident Node.js helper.
- Local create, edit, resource import, and save must not depend on network or search success.
- Canonical image markup is exactly `![alt](:/<32 lowercase hex resource id>)`.
- Blob identity is the full lowercase SHA-256; resource identity is an independent 32-character lowercase hex ID.
- The accepted decoded/stored image limit is exactly 10 MiB per image.
- Blob paths must reject symbolic-link traversal and remain below `<profile>/resources/blobs`.
- `body_rtf` must not contain image bytes; FTS must not index binary, RTF, hashes, or blob paths.
- Do not touch the production Joplin profile, WebDAV target, NAS, Joplin Server, or server database.
- Every test and UI smoke run must use the app's isolated native profile or an explicit temporary profile.

---

## File map

- `packages/app-lite-native/src/body.rs`: parse/generate Joplin image markers and build plain search projections.
- `packages/app-lite-native/src/resource_store.rs`: validate image metadata, hash bytes, enforce safe paths, and atomically persist blobs.
- `packages/app-lite-native/src/core.rs`: migrate SQLite, store resource metadata and associations, and atomically update complete note content.
- `packages/app-lite-native/src/app.rs`: AppKit paste/drop bridge, attachment rendering, and editor save/load orchestration.
- `packages/app-lite-native/src/lib.rs`: expose the new pure Rust modules.
- `packages/app-lite-native/Cargo.toml`: add hashing and required AppKit feature gates.
- `packages/app-lite-native/scripts/check-attachment-contract.sh`: verify a bundled app still carries the native/icon/resource contract without web runtimes.
- `packages/app-lite-native/README.md`: document supported inputs, storage location, and known MVP limits.

### Task 1: Canonical body model and durable resource store

**Files:**
- Create: `packages/app-lite-native/src/body.rs`
- Create: `packages/app-lite-native/src/resource_store.rs`
- Modify: `packages/app-lite-native/src/lib.rs`
- Modify: `packages/app-lite-native/src/core.rs`
- Modify: `packages/app-lite-native/Cargo.toml`
- Test: inline `#[cfg(test)]` modules in `body.rs`, `resource_store.rs`, and `core.rs`

**Interfaces:**
- Consumes: existing `new_id()`, `CoreError`, `NoteRepository`, `Note`, and SQLite profile path validation.
- Produces: `markdown_marker(resource_id: &str, alt: &str) -> Result<String, BodyError>`, `extract_resource_ids(body: &str) -> Vec<String>`, `project_search_text(body: &str) -> String`, `ResourceStore::new(profile_root: PathBuf) -> Result<Self, ResourceError>`, `ResourceStore::put(&self, input: ResourceImport<'_>) -> Result<ResourceBlob, ResourceError>`, `NoteRepository::import_resource(ResourceImport<'_>) -> Result<StoredResource, CoreError>`, `NoteRepository::get_resource(&str) -> Result<Option<StoredResource>, CoreError>`, and `NoteRepository::update_note_content(&str, NoteContentUpdate) -> Result<Note, CoreError>`.

- [ ] **Step 1: Add failing canonical-body tests**

```rust
#[test]
fn marker_round_trip_and_projection_are_joplin_compatible() {
    let id = "0123456789abcdef0123456789abcdef";
    let marker = markdown_marker(id, "庭审截图 [1]").unwrap();
    assert_eq!(marker, "![庭审截图 \\[1\\]](:/0123456789abcdef0123456789abcdef)");
    let body = format!("证据如下\n\n{marker}\n\n结论");
    assert_eq!(extract_resource_ids(&body), vec![id]);
    let projected = project_search_text(&body);
    assert!(projected.contains("庭审截图 [1]"));
    assert!(!projected.contains("0123456789abcdef"));
    assert!(!projected.contains('\u{fffc}'));
}
```

- [ ] **Step 2: Run the focused test and verify the missing module failure**

Run: `cd packages/app-lite-native && cargo test marker_round_trip_and_projection_are_joplin_compatible -- --exact`

Expected: compilation fails because `body` and its functions do not exist.

- [ ] **Step 3: Implement strict marker parsing and plain-text projection**

```rust
pub fn markdown_marker(resource_id: &str, alt: &str) -> Result<String, BodyError> {
    validate_resource_id(resource_id)?;
    let escaped = alt.replace('\\', "\\\\").replace('[', "\\[").replace(']', "\\]");
    Ok(format!("![{escaped}](:/{resource_id})"))
}
```

Use a single parser shared by `extract_resource_ids` and
`project_search_text`; accept only lowercase 32-hex IDs and unescape the alt
text for search. Invalid lookalike markup remains ordinary text.

- [ ] **Step 4: Add failing migration, deduplication, and association tests**

```rust
#[test]
fn importing_same_bytes_reuses_blob_and_survives_reopen() {
    let temp = tempfile::tempdir().unwrap();
    let db = temp.path().join("notes.sqlite");
    let repo = NoteRepository::open(&db).unwrap();
    let first = repo.import_resource(png_import(TINY_PNG, "first.png")).unwrap();
    let second = repo.import_resource(png_import(TINY_PNG, "copy.png")).unwrap();
    assert_ne!(first.id, second.id);
    assert_eq!(first.sha256, second.sha256);
    assert_eq!(count_rows(&repo, "resource_blobs"), 1);
    drop(repo);
    let reopened = NoteRepository::open(&db).unwrap();
    assert_eq!(reopened.get_resource(&first.id).unwrap().unwrap().sha256, first.sha256);
}
```

Also assert old-note migration copies `body` to `body_text`, a complete
`NoteContentUpdate` replaces associations without deleting blobs, and search
matches image alt text but not digest/path text.

- [ ] **Step 5: Run the resource tests and confirm their first failing reason**

Run: `cd packages/app-lite-native && cargo test resource -- --nocapture`

Expected: compilation fails because resource types and repository methods are absent.

- [ ] **Step 6: Implement blob persistence and schema migration**

```rust
pub const MAX_IMAGE_BYTES: usize = 10 * 1024 * 1024;

pub struct ResourceImport<'a> {
    pub bytes: &'a [u8],
    pub title: &'a str,
    pub mime: &'a str,
    pub file_extension: &'a str,
}

pub struct NoteContentUpdate {
    pub title: String,
    pub body: String,
    pub body_text: String,
    pub body_rtf: Vec<u8>,
    pub resource_ids: Vec<String>,
}
```

Use `sha2::{Digest, Sha256}` and lowercase hex output. Create a temporary
sibling with `create_new(true)`, call `sync_all`, and rename it to the digest
path. Reject empty data, over-limit data, MIME/extensions outside PNG/JPEG, any
unsafe component, and a resource root/blob that is a symbolic link. Migrate
with `PRAGMA user_version`, add `body_text`, create the three resource tables,
backfill existing notes, and rebuild FTS on `title, body_text`.

- [ ] **Step 7: Run all core tests and lint the Rust implementation**

Run: `cd packages/app-lite-native && cargo fmt --check && cargo test && cargo clippy --all-targets -- -D warnings`

Expected: all commands exit 0; resource tests prove one blob for duplicate
bytes, durable reopen, exact markers, safe projection, and atomic association replacement.

- [ ] **Step 8: Commit Task 1**

```bash
git add packages/app-lite-native/Cargo.toml packages/app-lite-native/Cargo.lock packages/app-lite-native/src/body.rs packages/app-lite-native/src/resource_store.rs packages/app-lite-native/src/core.rs packages/app-lite-native/src/lib.rs
git commit -m "feat: add native image resource store"
```

### Task 2: AppKit inline attachment codec and image paste

**Files:**
- Modify: `packages/app-lite-native/src/app.rs`
- Modify: `packages/app-lite-native/Cargo.toml`
- Test: inline pure helper tests in `packages/app-lite-native/src/app.rs`

**Interfaces:**
- Consumes: all Task 1 interfaces, especially `markdown_marker`, `extract_resource_ids`, `ResourceImport`, `StoredResource`, and `NoteContentUpdate`.
- Produces: `AttachmentDescriptor { resource_id: String, alt: String }`, pure `editor_save_projection(segments: &[EditorSegment]) -> Result<EditorProjection, EditorCodecError>`, native `insert_image_data(bytes: &[u8], title: &str, mime: &str)`, and load-time `render_body_attachments(note: &Note)`.

- [ ] **Step 1: Add failing editor projection tests**

```rust
#[test]
fn editor_projection_preserves_attachment_order_without_binary_rtf() {
    let projection = editor_save_projection(&[
        EditorSegment::Text("前文\n".into()),
        EditorSegment::Attachment(AttachmentDescriptor {
            resource_id: "0123456789abcdef0123456789abcdef".into(),
            alt: "截图.png".into(),
        }),
        EditorSegment::Text("\n后文".into()),
    ]).unwrap();
    assert_eq!(projection.body, "前文\n![截图.png](:/0123456789abcdef0123456789abcdef)\n后文");
    assert_eq!(projection.resource_ids, vec!["0123456789abcdef0123456789abcdef"]);
    assert!(projection.body_text.contains("截图.png"));
}
```

- [ ] **Step 2: Run the focused test and verify the missing codec failure**

Run: `cd packages/app-lite-native && cargo test editor_projection_preserves_attachment_order_without_binary_rtf -- --exact`

Expected: compilation fails because editor codec types are absent.

- [ ] **Step 3: Implement attachment metadata, save projection, and reload rendering**

Represent each attachment with an attributed-string metadata attribute keyed
`com.kevinhao.joplin-lite.resource-id`; never infer identity from the image
filename. Build canonical body by enumerating text and attachment runs in
order. Make an attributed-string copy with markers substituted before calling
`RTFFromRange`, then assert in the test helper that no source image byte slice
is present in the resulting cache. On load, parse the canonical body and
replace valid marker ranges with `NSTextAttachment` images; missing resources
stay as visible marker text.

- [ ] **Step 4: Enable native image paste with ordinary-text fallback**

Add the required `objc2-app-kit` features: `NSBitmapImageRep`, `NSImage`,
`NSImageRep`, `NSPasteboard`, `NSPasteboardItem`, and `NSTextAttachment`.
Handle paste only when the pasteboard supplies PNG, TIFF, or one ordinary
PNG/JPEG file URL. Normalize TIFF to PNG with `NSBitmapImageRep`; validate
decoding with `NSImage::initWithData`; otherwise invoke normal `NSTextView`
paste. After `import_resource` succeeds, insert exactly one attachment through
the text system, register it with undo, and invoke the existing autosave path.

- [ ] **Step 5: Preserve readable inline geometry and failure semantics**

Scale only the displayed `NSTextAttachment` bounds so neither dimension
exceeds 640 points and the aspect ratio is retained. Keep original stored
bytes. Over-limit/invalid images must not change the attributed string,
selection, note row, or resource associations; set save status to
`图片未插入：格式不支持` or `图片未插入：超过 10 MB`.

- [ ] **Step 6: Run Rust tests and a signed paste smoke**

Run: `cd packages/app-lite-native && cargo fmt --check && cargo test && cargo clippy --all-targets -- -D warnings && bash scripts/bundle.sh`

Expected: all commands exit 0. Launch the bundled app with an explicit
temporary profile, paste one generated PNG and ordinary Chinese text, undo and
redo the image insertion, quit and relaunch, and confirm the image and text are
still visible without any WebKit or Node child process.

- [ ] **Step 7: Commit Task 2**

```bash
git add packages/app-lite-native/Cargo.toml packages/app-lite-native/Cargo.lock packages/app-lite-native/src/app.rs
git commit -m "feat: paste inline images in native editor"
```

### Task 3: Finder drag/drop, bundle contract, and MVP handoff

**Files:**
- Modify: `packages/app-lite-native/src/app.rs`
- Create: `packages/app-lite-native/scripts/check-attachment-contract.sh`
- Modify: `packages/app-lite-native/scripts/bundle.sh`
- Modify: `packages/app-lite-native/README.md`

**Interfaces:**
- Consumes: Task 2 `insert_image_data` and the existing `ResourceTextView` paste behavior.
- Produces: `NSDraggingDestination` support for ordinary local PNG/JPEG URLs and a reproducible release verification command.

- [ ] **Step 1: Add a failing bundle contract**

```bash
#!/usr/bin/env bash
set -euo pipefail
APP_PATH="${1:?app path required}"
test -f "$APP_PATH/Contents/Resources/AppIcon.icns"
test ! -e "$APP_PATH/Contents/Frameworks/Electron Framework.framework"
if find "$APP_PATH" -type f \( -name node -o -name 'libnode*' \) -print -quit | grep -q .; then
  echo "bundled Node runtime is forbidden" >&2
  exit 1
fi
```

Run before implementation: `cd packages/app-lite-native && bash scripts/check-attachment-contract.sh dist/'Joplin Lite Native.app'`

Expected: exit 127 because the contract script does not exist.

- [ ] **Step 2: Implement `NSDraggingDestination` on the editor subclass**

Register `NSPasteboardTypeFileURL`. In `draggingEntered:` return copy only when
the dragging pasteboard resolves to one readable local PNG/JPEG no larger than
10 MiB; otherwise return none. In `performDragOperation:` move the caret to the
drop character index, call the same `insert_image_data` path as paste, and
return true only after the resource and attachment are saved. Promised files,
directories, remote URLs, and non-images remain rejected.

- [ ] **Step 3: Add the complete bundle contract and documentation**

Extend the failing shell check to validate `CFBundleIconFile=AppIcon`, the
presence of `AppIcon.icns`, absence of WebKit/JavaScriptCore/Node payloads, and
a successful `codesign --verify --deep --strict`. Call it at the end of
`bundle.sh`. Document PNG/JPEG/TIFF paste support, PNG/JPEG Finder drop, 10-MiB
limit, blob location, canonical marker format, and unsupported formats in the
README.

- [ ] **Step 4: Run release and real-product verification**

Run:

```bash
cd packages/app-lite-native
cargo fmt --check
cargo test
cargo clippy --all-targets -- -D warnings
bash scripts/bundle.sh
bash scripts/check-attachment-contract.sh "dist/Joplin Lite Native.app"
codesign --verify --deep --strict --verbose=2 "dist/Joplin Lite Native.app"
```

Expected: every command exits 0. In the signed app, create a note, type a real
title, paste a screenshot, drag a JPEG after it, paste Chinese text, quit,
relaunch, search for both the text and image alt name, and verify the same two
images appear in order. Record the temporary profile path, note ID, resource
IDs, blob SHA-256 values, process tree, app size, and release RSS without
including note content or credentials.

- [ ] **Step 5: Commit Task 3**

```bash
git add packages/app-lite-native/src/app.rs packages/app-lite-native/scripts/check-attachment-contract.sh packages/app-lite-native/scripts/bundle.sh packages/app-lite-native/README.md
git commit -m "feat: finish native image attachment MVP"
```
