# Joplin Lite 0.4 HTML Canonical Implementation Plan

> **For implementation agents:** Execute this plan task by task. Production code is delegated to a Luna agent; the root agent owns architecture, review, real-UI verification, tagging, and push.

**Goal:** Replace RTF persistence with deterministic UTF-8 HTML while preserving native editing, title saving, inline images, autosave, undo/redo, restart recovery, and Joplin HTML-note compatibility.

**Architecture:** A pure-Rust document model is the boundary between storage and AppKit. `notes.body` becomes a sanitized semantic HTML fragment and `markup_language` is `2`; `body_text` and note-resource rows are derived. AppKit maps `NSTextView`/`NSAttributedString` to and from the document model. Existing RTF is read once for an all-or-nothing migration after a SQLite backup, then cleared; no normal save path writes RTF.

**Tech stack:** Rust, AppKit via `objc2`, `rusqlite` with SQLite backup support, a pure-Rust HTML5 parser, existing SHA-256 resource store and FTS5.

**Governing design:** `docs/superpowers/specs/2026-09-07-joplin-lite-evernote-core-html-design.md`

---

## Stable baseline — do not disturb

- `Command-N`, New Note focus, title independence, debounced autosave, forced save on switch/quit.
- Native IME, selection, undo/redo, B/I/U, copy/paste and drag/drop.
- The verified 0.3.1 Finder file-copy rule: valid local file URL wins over icon bitmap representations; invalid file URL never falls back to the icon.
- Resource validation, 10 MiB limit, SHA-256 blob addressing, transactional resource association and soft delete.
- No WebKit, JavaScriptCore, Electron, Node child process or Tauri runtime in the shipped app.
- App-owned isolated profile protection; never read or modify official Joplin profile paths.

## Task 1: Add the pure-Rust HTML document model

**Files:**

- Create: `packages/app-lite-native/src/html_body.rs`
- Modify: `packages/app-lite-native/src/lib.rs`
- Modify: `packages/app-lite-native/Cargo.toml`
- Modify: `packages/app-lite-native/Cargo.lock`

**Step 1: Write failing tests**

Cover at least:

1. CJK, emoji, `&`, `<`, `>`, quotes and significant whitespace are escaped without text loss.
2. Paragraphs, empty paragraphs, B/I/U nesting and inline image order round-trip through `Document -> HTML -> Document`.
3. The same document serializes to identical HTML bytes on repeated saves.
4. `<img src=":/<32 lowercase hex>" alt="...">` yields one resource node and one resource ID; data URLs, remote URLs and invalid IDs do not.
5. Script/style/event attributes and `javascript:` links are discarded; unknown elements retain safe visible text.
6. Search projection contains visible text and image alt text but not resource IDs, tags or HTML syntax.
7. Empty semantic content serializes to `""`, so abandoned-draft cleanup remains correct.

Run the new test target and capture the RED failure before implementing.

**Step 2: Implement a small semantic model**

Use typed blocks and inlines rather than storing AppKit attributes or arbitrary HTML. Initial implementation must model:

- paragraphs;
- text runs with bold, italic and underline marks;
- soft breaks;
- image resources with validated ID and alt text.

Define the model so headings, lists, quotes, code and links can be added without changing the storage boundary, but do not implement unrelated 0.5 UI in this task.

**Step 3: Implement parse, sanitize, serialize and projection**

- Parse fragments with a maintained pure-Rust HTML5 parser; do not execute or load HTML.
- Accept the broader design whitelist when cheap, but only promise lossless editing for the 0.4 model above.
- Serialize deterministically using our own emitter, not a browser or Cocoa HTML exporter.
- Reject/downgrade unsafe URLs and unsupported binary/remote image representations.
- Centralize resource ID validation; do not maintain a second rule that can drift from the resource store.

**Step 4: Run focused and full Rust gates**

```sh
cargo test --locked --manifest-path packages/app-lite-native/Cargo.toml html_body
cargo fmt --all --check --manifest-path packages/app-lite-native/Cargo.toml
cargo test --locked --manifest-path packages/app-lite-native/Cargo.toml
cargo clippy --locked --manifest-path packages/app-lite-native/Cargo.toml --all-targets -- -D warnings
```

**Step 5: Commit**

```sh
git add packages/app-lite-native/src/html_body.rs packages/app-lite-native/src/lib.rs packages/app-lite-native/Cargo.toml packages/app-lite-native/Cargo.lock
git commit -m "Add canonical HTML document model"
```

## Task 2: Add the HTML schema and atomic legacy migration API

**Files:**

- Modify: `packages/app-lite-native/src/core.rs`
- Modify: `packages/app-lite-native/tests/core_lifecycle.rs`
- Modify: `packages/app-lite-native/Cargo.toml`
- Modify: `packages/app-lite-native/Cargo.lock`

**Step 1: Write failing repository tests**

Cover:

1. A fresh note explicitly stores `markup_language = 2`, HTML body, derived `body_text`, and empty `body_rtf`.
2. Schema upgrade adds `markup_language`; pre-0.4 rows are identified as legacy without changing title/timestamps/content.
3. A migration batch updates every legacy row in one transaction, sets language `2`, replaces body/body_text/resource associations and clears RTF.
4. If any row or resource validation fails, the entire batch rolls back.
5. Migration creates a valid SQLite backup before changing rows; reopening the backup yields the old body and RTF.
6. Normal create/update APIs cannot write a non-empty `body_rtf`.
7. FTS is rebuilt from HTML-derived `body_text`, not raw tags or resource IDs.

Run the focused tests and preserve the RED output.

**Step 2: Evolve the schema without deleting the old column**

- Increment `PRAGMA user_version` from 2 to 3.
- Fresh databases define `markup_language INTEGER NOT NULL DEFAULT 2`.
- Existing rows receive `markup_language = 1` until content migration succeeds.
- Keep `body_rtf` in schema for one release but make all normal write paths store an empty blob.

**Step 3: Change repository types**

- Add `markup_language` to `Note`.
- Remove `body_rtf` from normal `CreateNote`, `UpdateNote` and `NoteContentUpdate` inputs.
- Add an explicitly named legacy read DTO/API that exposes RTF only to startup migration.
- Add a single atomic `apply_html_migration` API accepting already-validated conversions.
- Keep note/resource association and FTS changes inside the same transaction.

**Step 4: Add the backup API**

Use SQLite's backup API, not a filesystem copy of a live WAL database. The backup name must be unique, remain inside the app-owned profile and include the source schema version. Refuse symlinks and pre-existing targets.

**Step 5: Run focused and full gates, then commit**

```sh
cargo test --locked --manifest-path packages/app-lite-native/Cargo.toml core
cargo test --locked --manifest-path packages/app-lite-native/Cargo.toml --test core_lifecycle
cargo fmt --all --check --manifest-path packages/app-lite-native/Cargo.toml
cargo test --locked --manifest-path packages/app-lite-native/Cargo.toml
cargo clippy --locked --manifest-path packages/app-lite-native/Cargo.toml --all-targets -- -D warnings
git add packages/app-lite-native/src/core.rs packages/app-lite-native/tests/core_lifecycle.rs packages/app-lite-native/Cargo.toml packages/app-lite-native/Cargo.lock
git commit -m "Add atomic HTML note migration"
```

## Task 3: Replace the AppKit RTF save/load path

**Files:**

- Modify: `packages/app-lite-native/src/app.rs`
- Modify: `packages/app-lite-native/src/body.rs` or delete it if all callers move cleanly
- Modify: `packages/app-lite-native/src/lib.rs`

**Step 1: Write failing AppKit codec tests**

Cover:

1. An attributed string containing CJK/emoji and mixed B/I/U converts to the expected document model and canonical HTML.
2. Canonical HTML renders back to equivalent visible text and traits.
3. A resource node renders as `NSTextAttachment` with the existing resource ID/alt attributes, and saving recovers the same node in the same position.
4. An unknown attachment downgrades to visible `[图片]` text and never serializes image bytes.
5. Current Finder paste tests still pass unchanged.
6. No normal save helper calls `RTFFromRange` or produces non-empty RTF.

Run the focused test and capture RED.

**Step 2: Implement document-to-AppKit rendering**

- Render semantic marks with native font traits/underline attributes.
- Render image nodes with the existing decoded image and bounded display-size code.
- Missing resources remain visible as a readable placeholder containing alt text; they are not silently removed.
- Preserve the native text system and existing undo manager.

**Step 3: Implement AppKit-to-document capture**

- Walk attributed runs and paragraph boundaries.
- Convert only the supported semantic attributes.
- Read known resource ID/alt attributes from attachments.
- Strip incidental font/color/layout attributes rather than persisting platform styling.
- Serialize through `html_body`, derive `body_text` and resource IDs there, and save once through the repository.

**Step 4: Remove RTF from normal runtime behavior**

- Delete `sanitized_rtf_from_editor`, `RtfSavePlan`, normal RTF load fallback and the “正文已保存，格式未保存” branch.
- Keep only a narrowly named `decode_legacy_rtf_for_migration` helper.
- New-note creation uses empty HTML and `markup_language = 2`.
- Load language `2` through the HTML document model.

**Step 5: Implement startup migration orchestration**

Before draft cleanup and before opening the main window:

1. list all legacy rows;
2. decode each RTF read-only, overlay resource markers using their UTF-16 ranges and preserve marks;
3. build and validate canonical HTML, visible text and resource ID order;
4. request a SQLite backup;
5. submit one atomic migration batch;
6. on failure, leave the database untouched and show a concise recovery error without logging note content.

Do not claim support for arbitrary official Joplin profiles in this migration; it only upgrades the app-owned Lite database.

**Step 6: Run full gates and commit**

```sh
cargo fmt --all --check --manifest-path packages/app-lite-native/Cargo.toml
cargo test --locked --manifest-path packages/app-lite-native/Cargo.toml
cargo clippy --locked --manifest-path packages/app-lite-native/Cargo.toml --all-targets -- -D warnings
bash packages/app-lite-native/scripts/check-attachment-contract.sh packages/app-lite-native/dist/Joplin\ Lite\ Native.app
git add packages/app-lite-native/src/app.rs packages/app-lite-native/src/body.rs packages/app-lite-native/src/lib.rs
git commit -m "Persist native notes as HTML"
```

## Task 4: Documentation, contracts and release verification

**Files:**

- Modify: `packages/app-lite-native/README.md`
- Modify: `packages/app-lite-native/Cargo.toml`
- Modify: `packages/app-lite-native/Info.plist`
- Modify: `packages/app-lite-native/scripts/check-attachment-contract.sh`
- Modify: `docs/superpowers/specs/2026-09-07-joplin-lite-evernote-core-html-design.md` only if implementation exposes a real design correction

**Step 1: Update product truth**

- Version to 0.4.0.
- Replace RTF claims with UTF-8 semantic HTML and one-time migration language.
- State the exact supported formatting subset and current limits.
- Keep 0.3.1 Finder paste behavior documented.

**Step 2: Add release contracts**

The contract should fail if:

- normal runtime contains an RTF export/save symbol or a code path that writes non-empty RTF;
- the bundle introduces WebKit, JavaScriptCore, Electron, libnode or child helpers;
- the app/version/icon/binary/signature invariants regress.

Do not ban the narrowly scoped legacy RTF decoder required for migration.

**Step 3: Build and run automated release gates**

```sh
cargo fmt --all --check --manifest-path packages/app-lite-native/Cargo.toml
cargo test --locked --manifest-path packages/app-lite-native/Cargo.toml
cargo clippy --locked --manifest-path packages/app-lite-native/Cargo.toml --all-targets -- -D warnings
packages/app-lite-native/scripts/bundle.sh
bash packages/app-lite-native/scripts/check-icon-contract.sh --bundle packages/app-lite-native/dist/Joplin\ Lite\ Native.app
bash packages/app-lite-native/scripts/check-attachment-contract.sh packages/app-lite-native/dist/Joplin\ Lite\ Native.app
codesign --verify --deep --strict packages/app-lite-native/dist/Joplin\ Lite\ Native.app
```

**Step 4: Root-agent real UI verification**

Use a new isolated profile and confirm its actual `notes.sqlite` path with `lsof` before interacting:

1. Create a note, set a Chinese title, type CJK/emoji and apply B/I/U.
2. Finder-copy a JPEG whose clipboard also exposes icon previews, paste at the caret, add text before and after it.
3. Wait for autosave; query the isolated database and verify body is readable HTML, `markup_language = 2`, `body_rtf` is empty, and resource hash matches source.
4. Quit and relaunch the same isolated profile; visually verify title, formatting, mixed text/image order and image content.
5. Prepare a copy of a 0.3.1 profile with RTF formatting; launch 0.4, verify a backup was created and the migrated note matches before/after.
6. Inspect process tree, dynamic libraries, app size and idle/edited RSS.

**Step 5: Independent review**

Request separate spec-compliance and code-quality review. Any Critical or Important finding blocks tag/push. Minor gaps may be deferred only if they do not affect data loss, migration, HTML readability, image integrity or stable native editing.

**Step 6: Commit, tag and push after approval**

```sh
git add packages/app-lite-native/README.md packages/app-lite-native/Cargo.toml packages/app-lite-native/Info.plist packages/app-lite-native/scripts/check-attachment-contract.sh
git commit -m "Release native HTML notes 0.4.0"
git tag -a joplin-lite-native-v0.4.0-html-canonical -m "Joplin Lite Native 0.4.0 HTML canonical"
git push origin codex/joplin-lite-native-rust
git push origin joplin-lite-native-v0.4.0-html-canonical
```
