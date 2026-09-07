# Joplin Lite Native Evernote 可见 MVP Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Build a recognizably Evernote-style, fully native macOS note browser and WYSIWYG editor without reintroducing WebKit, live RTF, or unrelated Evernote features.

**Architecture:** Keep the pure Rust HTML `Document` as the canonical storage boundary, use `text-document` as the active rich-editing transaction model, and project it through a focused AppKit adapter. Replace the eager stack of note buttons with a reusable `NSCollectionView` card browser; let `app.rs` coordinate a three-region shell, responsive toolbar, focus mode, delayed autosave and existing persistence/image services.

**Tech Stack:** Rust 2024, rusqlite/SQLite FTS5, html5ever, text-document 1.12.1, objc2 0.6, objc2-app-kit 0.3, AppKit `NSTextView`/`NSCollectionView`, Bash release contracts.

**Spec:** `docs/superpowers/specs/2026-09-07-joplin-lite-visible-mvp-design.md`

**Evernote implementation evidence:** The installed Evernote 11.32.5 bundle exposes source maps for `@evernote/common-editor` 183.272.12. Its relevant core is a semantic ProseMirror document, transaction-based command/query APIs, separate DOM/ENML parsers and serializers, resource nodes keyed by stable identity, immediate `contentChanged` plus a debounced settled notification, explicit flush, context-change de-thrashing, computed centered note width, and viewport-bounded work. Tasks 2–4 must translate those mechanisms into native AppKit rather than imitate only the pixels.

**Existing editor-core evidence:** Pin the independent MPL-2.0 `text-document` 1.12.1 crate as the active editor transaction model; its Rope, cursor mutations, block/list/image model, find/replace, undo/redo and change events replace hand-written range/history algorithms. Its HTML exporter is not a persistence boundary: the verified 1.12.1 exporter flattens nested list indent, so canonical saves must traverse flow/format snapshots into this project's `Document` and use the existing serializer. Element's Matrix Rich Text Editor remains a behavioral reference for UTF-16 and list edge cases; Lapce/Floem remains a reference for revision/pristine and projection-only auxiliary content.

**Rust stack ruling:** Keep AppKit/TextKit for this macOS MVP; `teksilo-preview-ui` is only a previewer and full Teksilo 0.9.x would replace the proven macOS input chain with a pre-1.0 winit/wgpu stack. Reuse its stable lower layer `text-document`, but keep canonical HTML ownership in this project. Select Loro for the follow-on native sync phase, behind the canonical `Document` transaction boundary. Do not add Loro to Tasks 1–5 or make its binary snapshot/oplog the only note body. SQLite canonical HTML/FTS remains readable materialized state; Joplin Server remains an undisturbed migration/recovery rail until a separate Loro multi-replica and NAS-resource gate passes.

## Global Constraints

- Evernote is the UI/workflow prototype; Byword only informs the centered long-form writing measure.
- No Tauri, WKWebView, WebKit, JavaScriptCore, Electron, Node, helper process, or downloaded web font.
- `notes.body` remains the only canonical body; `body_text` and resource associations are derived.
- Normal runtime never creates, saves or loads RTF. Keep the one-time legacy decoder isolated and frozen.
- Normal runtime keeps readable canonical HTML and derived FTS even after Loro sync is introduced; a CRDT snapshot/oplog may never be the only copy of note content.
- Do not show AI, task, calendar, reminder, template, sharing, collaboration or plugin controls.
- Every visible formatting/insert command works; future commands stay absent.
- Editor commands mutate the active `text-document::TextDocument` in one undo transaction and expose query state for toolbar refresh; UI controls must not directly mutate fonts as the source of truth.
- Save only when canonical HTML differs from the last persisted HTML. Dirty status changes immediately; coalescing may delay the write, never the status.
- Preserve title persistence, Command-N, undo/redo, Finder/bitmap paste, drag/drop, resource safety, search, soft delete and migration backup.
- Never access an official Joplin profile from normal product code.
- Luna owns product code; root owns architecture, review, real-window verification and release.

## File map

- `src/html_body.rs`: platform-neutral document types, safe parsing, deterministic HTML, search/resource projections.
- `src/native_editor.rs`: `text-document` ↔ canonical `Document` adapter, UTF-16 bridge, AppKit projection and editor session.
- `src/note_preview.rs`: preview title, snippet, first image and card metric projection.
- `src/native_note_browser.rs`: reusable `NSCollectionViewItem` card views and flow layout.
- `src/app.rs`: window/app delegate, three-region shell, commands, selection and delayed-save orchestration.

---

### Task 1: Extend canonical HTML semantics

**Files:**
- Modify: `packages/app-lite-native/src/html_body.rs`
- Modify: `packages/app-lite-native/src/core.rs`
- Test: inline tests in both files

**Produces:**

```rust
pub enum HeadingLevel { One, Two, Three }
pub enum Alignment { Left, Center, Right }
pub struct BlockStyle { pub alignment: Alignment, pub indent: u8 }
pub enum ListKind { Unordered, Ordered, Checklist }
pub struct ListItem { pub checked: Option<bool>, pub style: BlockStyle, pub inlines: Vec<Inline> }
pub enum Block {
    Paragraph { style: BlockStyle, inlines: Vec<Inline> },
    Heading { level: HeadingLevel, style: BlockStyle, inlines: Vec<Inline> },
    List { kind: ListKind, items: Vec<ListItem> },
}
pub struct Marks {
    pub bold: bool,
    pub italic: bool,
    pub underline: bool,
    pub strikethrough: bool,
    pub highlight: bool,
    pub link: Option<String>,
}
```

Links accept only absolute `https://`, `http://` and `mailto:` URLs. Indent clamps to `0..=8`. Canonical block attributes are only `data-align="center|right"` and `data-indent="1..8"`; arbitrary CSS is discarded. Checklist HTML is `<ul data-type="checklist"><li data-checked="false">…</li></ul>`.

- [ ] **Step 1: Add RED tests.** Construct all block/mark variants and assert exact canonical HTML. Parse adversarial schemes, event attributes, arbitrary CSS, malformed nesting, depth bombs and oversized input; ensure safe visible output or typed rejection without panic.
- [ ] **Step 2: Run RED.** `cargo test --locked --manifest-path packages/app-lite-native/Cargo.toml html_body` must fail for missing variants.
- [ ] **Step 3: Implement types and normalization.** Retain the iterative html5ever projection and limits; merge text only when complete marks match; merge only adjacent same-kind lists; preserve empty blocks/items.
- [ ] **Step 4: Implement projections.** `search_text` separates blocks/list items with one newline and excludes UI prefixes/HTML. `resource_ids` preserves document order and duplicates.
- [ ] **Step 5: Run gates.** Run fmt check, full locked tests, Clippy with `-D warnings`, and `git diff --check`.
- [ ] **Step 6: Commit.** Commit only product files as `Expand native note semantics`.

---

### Task 2: Build the native editor session and AppKit adapter

**Files:**
- Create: `packages/app-lite-native/src/native_editor.rs`
- Modify: `packages/app-lite-native/src/app.rs`
- Modify: `packages/app-lite-native/Cargo.toml`
- Test: inline AppKit tests in `native_editor.rs`

**Consumes:** Task 1 canonical `Document` types. Pin `text-document = "=1.12.1"`; do not add the full Teksilo framework.

**Produces:**

```rust
pub enum BlockCommand { Paragraph, Heading(HeadingLevel), UnorderedList, OrderedList, Checklist }
pub enum InlineCommand { Bold, Italic, Underline, Strikethrough, Highlight, Clear }
pub enum ParagraphCommand { Align(Alignment), IncreaseIndent, DecreaseIndent }
pub struct NativeEditorSession { /* active text_document::TextDocument + revision */ }
pub struct RenderedDocument { pub attributed: Retained<NSMutableAttributedString>, pub missing_resources: usize }
pub fn session_from_document(document: &Document) -> Result<NativeEditorSession, EditorCodecError>;
pub fn document_from_session(session: &NativeEditorSession) -> Result<Document, EditorCodecError>;
pub fn render_session<F>(session: &NativeEditorSession, load: F, width: f64) -> RenderedDocument where F: FnMut(&str) -> Option<StoredResource>;
pub fn apply_committed_text_delta(session: &mut NativeEditorSession, range: NSRange, replacement: &str) -> Result<(), EditorCodecError>;
pub fn apply_link(session: &mut NativeEditorSession, selection: NSRange, url: Option<&str>) -> Result<(), EditorCodecError>;
pub fn toggle_checklist_at_utf16_location(session: &mut NativeEditorSession, location: usize) -> bool;
```

- [ ] **Step 0: Prove the dependency boundary before integration.** Add focused tests showing Chinese/emoji scalar↔UTF-16 conversion, external `jln-resource://sha256/...` image identity, nested list indent and checklist markers. Add a regression proving why `TextDocument::to_html()` is forbidden for persistence: 1.12.1 flattens nested lists even though flow snapshots retain indent. No product save path may call it.
- [ ] **Step 1: Move the existing AppKit codec without behavior changes.** Keep the legacy RTF decoder isolated in `app.rs`; do not duplicate or generalize it. The last known working title/image/paste path remains available until the new session passes all gates.
- [ ] **Step 2: Implement explicit canonical adapters.** Map Task 1 blocks/inlines/resources to `text-document` flow/format structures and back without routing through HTML. Cover H1/H2/H3, bullet/ordered/checklist, nested indent, strike/highlight/link, alignment, empty blocks and images. Image nodes store only the existing resource URI/id and dimensions; never insert resource bytes/base64 into `TextDocument`.
- [ ] **Step 3: Implement the AppKit projection.** Render native 11/13/17/22/30 pt roles, paragraph styles, attachments and tagged projection-only list/checklist prefixes. TextKit marked text remains ephemeral; only committed composition deltas enter the active model. Missing-image labels and `☐`/`☑` never enter canonical text.
- [ ] **Step 4: Implement the UTF-16 delta bridge.** Convert AppKit `NSRange` to Unicode scalar positions with checked boundaries; apply committed insertion/deletion through `TextCursor`, then incrementally refresh the affected projection. Reject ranges splitting a surrogate pair. Add Chinese, emoji, combining-mark and cross-block tests.
- [ ] **Step 5: Implement mutations and query state through `TextCursor`.** Block commands operate on full blocks; inline commands on exact converted ranges. Applying the active list kind again returns to paragraphs. Clear removes inline semantics but never attachments/block type. Expose inactive/active/mixed states and refresh the toolbar only when context changes.
- [ ] **Step 6: Implement Return/list behavior and one undo owner.** Return continues a nonempty list item; Return on an empty item exits the list. Group one visible command in one `text-document` composite edit. Disable or bridge the competing AppKit undo registration so one edit never appears in two stacks; Command-Z and toolbar undo must exercise the same model history.
- [ ] **Step 7: Close the Task 1 write-back gate atomically.** Until `document_from_session` and AppKit projection pass all semantic round-trips, title changes/autosave may not overwrite a body with the temporary paragraph-only codec. Switch load/edit/save together in one commit and retain a safe refusal path for unsupported sessions.
- [ ] **Step 8: Port behavioral edge cases, not source.** Add independently written cases equivalent to Matrix RTE's partial-link selection, empty-list exit, nested remnants and action-state transitions, plus `text-document`-specific nested-list export and dual-undo regressions.
- [ ] **Step 9: Run gates and commit.** Run fmt/full tests/Clippy/diff-check; commit `Add native semantic editor session`.

---

### Task 3: Rebuild the Evernote editor shell

**Files:**
- Modify: `packages/app-lite-native/src/app.rs`
- Modify: `packages/app-lite-native/Cargo.toml`
- Test: inline layout/state tests in `app.rs`

- [ ] **Step 1: Add RED layout tests.** Cover 1380×820, 1100×700, browser-collapsed and full-focus layouts; assert three non-overlapping regions, 680-pt document measure, fixed toolbar and bottom save state.
- [ ] **Step 2: Build the three-region hierarchy.** Use 176–208 pt navigation, 360–400 pt browser and flexible editor workspace. Put a white/dynamic 12-pt-radius editor sheet on a warm system background. Default 1380×820, minimum 1100×700. Compute the centered 680-pt measure from live width and retain enough bottom scroll inset for the final line to reach the visual middle.
- [ ] **Step 3: Build the fixed responsive toolbar.** Add working image insert, undo/redo, block popup, B/I/U, highlight, bullet, ordered and checklist controls. Narrow mode moves working link/alignment/indent/strike/clear commands into `More`.
- [ ] **Step 4: Add image and link input.** `NSOpenPanel` accepts one local PNG/JPEG and routes through existing validation. A native link sheet accepts only Task 1 schemes and applies to nonempty selection.
- [ ] **Step 5: Add collapse/focus controls.** Double-chevron toggles navigation+browser; diagonal arrow toggles the browser only. Keep the editor measure centered.
- [ ] **Step 6: Add 300 ms autosave coalescing.** Mark dirty synchronously, track dirty generations and ignore stale timers. Compare newly serialized canonical HTML with the last persisted HTML to skip phantom writes. Flush before note switch/new/delete/window close/image insert/format actions. Image insertion first persists the resource and then inserts the semantic node as one undoable edit; failure leaves neither a fake node nor a false saved state. Failure keeps dirty state and visible text for retry.
- [ ] **Step 7: Run gates and commit.** Run fmt/full tests/Clippy/diff-check; commit `Rebuild native editor shell`.

---

### Task 4: Add Evernote thumbnail cards

**Files:**
- Create: `packages/app-lite-native/src/note_preview.rs`
- Create: `packages/app-lite-native/src/native_note_browser.rs`
- Modify: `packages/app-lite-native/src/lib.rs`
- Modify: `packages/app-lite-native/src/app.rs`
- Modify: `packages/app-lite-native/Cargo.toml`
- Test: inline tests in new modules and `app.rs`

**Produces:**

```rust
pub struct NotePreview { pub note_id: String, pub title: String, pub snippet: String, pub updated_label: String, pub first_image_id: Option<String> }
pub fn preview_from_note(note: &Note, now_ms: i64) -> NotePreview;
pub struct BrowserMetrics { pub columns: usize, pub card_width: f64, pub card_height: f64 }
pub fn browser_metrics(available_width: f64) -> BrowserMetrics;
```

- [ ] **Step 1: Add RED preview/geometry tests.** Assert body-text title fallback, tag-free snippet, first image order, Unicode-safe truncation, deterministic dates and exactly two columns at 360–400 pt.
- [ ] **Step 2: Implement preview projection.** Parse canonical HTML once per refresh. Parse failure returns text-only preview, never a fake resource id.
- [ ] **Step 3: Implement reusable cards.** Register one `NSCollectionViewItem` subclass with title/snippet/date/image; clear stale images during reuse. Use thin dynamic borders and a green selected outline, no shadow stack.
- [ ] **Step 4: Load visible thumbnails only.** Decode the first visible resource to a 2×56-pt target; cache by resource id/size with a fixed cap. Missing/corrupt images become text-only and never mutate HTML. Selection/context changes update only affected cards and toolbar state; they do not rebuild the collection.
- [ ] **Step 5: Remove eager stack rows.** Delete note-row/button arrays and tag-index selection. Search and refresh share the collection data source; selection uses note id across reorder/filter.
- [ ] **Step 6: Add minimal navigation.** Show search, green New Note, selected Notes and count only. Do not add dead Evernote sections.
- [ ] **Step 7: Prove virtualization.** A 1,600-note smoke retains 1,600 lightweight previews while live item views and image decodes stay bounded by visible cards plus reuse margin.
- [ ] **Step 8: Run gates and commit.** Run fmt/full tests/Clippy/diff-check; commit `Add Evernote note card browser`.

---

### Task 5: Verify and replace the release bundle

**Files:**
- Modify: `packages/app-lite-native/README.md`
- Modify: `packages/app-lite-native/scripts/check-attachment-contract.sh`
- Modify: design spec only for verified corrections

- [ ] **Step 1: Extend contracts.** Fail on forbidden runtimes/helpers, live RTF, eager `NSStackView` note rows, unsupported toolbar labels, bad version/icon/signature; combine all temp cleanup in one trap.
- [ ] **Step 2: Run automated release gates.** Run fmt, full locked tests, Clippy, bundle, icon contract, attachment contract, strict codesign and diff-check.
- [ ] **Step 3: Run fresh-profile UI acceptance.** Create a Chinese/emoji note; use every visible toolbar command; insert and Finder-paste an image between text; verify thumbnail, search, focus modes, autosave, undo/redo and restart. Query SQLite for readable canonical HTML, empty RTF, derived text and matching resource hash.
- [ ] **Step 4: Re-run migration and scale checks.** Migrate a copy of the six-note 0.3.1 profile, verify backup/metadata/image order and no second migration. Generate 1,600 temporary notes and measure first display, scrolling, thumbnail decode count, process tree, bundle size and idle/edited RSS.
- [ ] **Step 5: Review against the supplied Evernote screenshots.** Capture only the app window. Any crude placeholder, clipped Chinese label, dead button, wrong card hierarchy or visible HTML tag blocks release.
- [ ] **Step 6: Correct README and commit.** Replace old size/RSS/test-count claims with observed values; commit `Release Evernote style native MVP`.
- [ ] **Step 7: Tag/push only after root approval.** The proposed 0.4 tag was never created. Create one final annotated version tag only after the signed bundle passes product review.
