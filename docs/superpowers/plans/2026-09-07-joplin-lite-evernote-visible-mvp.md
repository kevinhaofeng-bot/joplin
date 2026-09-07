# Joplin Lite Native Evernote 可见 MVP Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Build a recognizably Evernote-style, fully native macOS note browser and WYSIWYG editor without reintroducing WebKit, live RTF, or unrelated Evernote features.

**Architecture:** Extend the pure Rust HTML Document model first, then project it through a focused AppKit editor codec. Replace the eager stack of note buttons with a reusable `NSCollectionView` card browser; let `app.rs` coordinate a three-region shell, responsive toolbar, focus mode, delayed autosave and existing persistence/image services.

**Tech Stack:** Rust 2024, rusqlite/SQLite FTS5, html5ever, objc2 0.6, objc2-app-kit 0.3, AppKit `NSTextView`/`NSCollectionView`, Bash release contracts.

**Spec:** `docs/superpowers/specs/2026-09-07-joplin-lite-visible-mvp-design.md`

## Global Constraints

- Evernote is the UI/workflow prototype; Byword only informs the centered long-form writing measure.
- No Tauri, WKWebView, WebKit, JavaScriptCore, Electron, Node, helper process, or downloaded web font.
- `notes.body` remains the only canonical body; `body_text` and resource associations are derived.
- Normal runtime never creates, saves or loads RTF. Keep the one-time legacy decoder isolated and frozen.
- Do not show AI, task, calendar, reminder, template, sharing, collaboration or plugin controls.
- Every visible formatting/insert command works; future commands stay absent.
- Preserve title persistence, Command-N, undo/redo, Finder/bitmap paste, drag/drop, resource safety, search, soft delete and migration backup.
- Never access an official Joplin profile from normal product code.
- Luna owns product code; root owns architecture, review, real-window verification and release.

## File map

- `src/html_body.rs`: platform-neutral document types, safe parsing, deterministic HTML, search/resource projections.
- `src/native_editor.rs`: macOS attributed-string codec, semantic attributes, list/checklist projection and formatting helpers.
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

### Task 2: Build the native editor codec

**Files:**
- Create: `packages/app-lite-native/src/native_editor.rs`
- Modify: `packages/app-lite-native/src/app.rs`
- Modify: `packages/app-lite-native/Cargo.toml`
- Test: inline AppKit tests in `native_editor.rs`

**Consumes:** Task 1 Document types.

**Produces:**

```rust
pub enum BlockCommand { Paragraph, Heading(HeadingLevel), UnorderedList, OrderedList, Checklist }
pub enum InlineCommand { Bold, Italic, Underline, Strikethrough, Highlight, Clear }
pub enum ParagraphCommand { Align(Alignment), IncreaseIndent, DecreaseIndent }
pub struct RenderedDocument { pub attributed: Retained<NSMutableAttributedString>, pub missing_resources: usize }
pub fn document_from_editor(source: &NSAttributedString) -> Result<Document, EditorCodecError>;
pub fn render_document<F>(document: &Document, load: F, width: f64) -> RenderedDocument where F: FnMut(&str) -> Option<StoredResource>;
```

- [ ] **Step 1: Move existing codec and tests without behavior changes.** Keep the legacy RTF decoder in `app.rs`, calling the same new codec; do not duplicate it.
- [ ] **Step 2: Add RED round-trip tests.** Cover H1/H2/H3, bullet/ordered/checklist, strike/highlight/link, alignment/indent, emoji UTF-16 ranges, empty blocks and images.
- [ ] **Step 3: Implement semantic attributes.** Define app-owned attributes for block/list/checklist/alignment/indent and tagged projection prefixes. Render native 11/13/17/22/30 pt roles and strip projection prefixes on save.
- [ ] **Step 4: Implement mutations.** Block commands operate on full paragraphs; inline commands on exact UTF-16 ranges. Applying the active list kind again returns to paragraphs. Clear removes inline semantics but never attachments/block type.
- [ ] **Step 5: Implement Return behavior.** Return continues a nonempty list item; Return on an empty item exits the list. Checklist prefix is `☐`/`☑` and remains toggleable without entering canonical text.
- [ ] **Step 6: Preserve undo boundaries.** One visible command creates one text-storage edit/undo group; `app.rs` saves once afterward.
- [ ] **Step 7: Run gates and commit.** Run fmt/full tests/Clippy/diff-check; commit `Add native semantic editor`.

---

### Task 3: Rebuild the Evernote editor shell

**Files:**
- Modify: `packages/app-lite-native/src/app.rs`
- Modify: `packages/app-lite-native/Cargo.toml`
- Test: inline layout/state tests in `app.rs`

- [ ] **Step 1: Add RED layout tests.** Cover 1380×820, 1100×700, browser-collapsed and full-focus layouts; assert three non-overlapping regions, 680-pt document measure, fixed toolbar and bottom save state.
- [ ] **Step 2: Build the three-region hierarchy.** Use 176–208 pt navigation, 360–400 pt browser and flexible editor workspace. Put a white/dynamic 12-pt-radius editor sheet on a warm system background. Default 1380×820, minimum 1100×700.
- [ ] **Step 3: Build the fixed responsive toolbar.** Add working image insert, undo/redo, block popup, B/I/U, highlight, bullet, ordered and checklist controls. Narrow mode moves working link/alignment/indent/strike/clear commands into `More`.
- [ ] **Step 4: Add image and link input.** `NSOpenPanel` accepts one local PNG/JPEG and routes through existing validation. A native link sheet accepts only Task 1 schemes and applies to nonempty selection.
- [ ] **Step 5: Add collapse/focus controls.** Double-chevron toggles navigation+browser; diagonal arrow toggles the browser only. Keep the editor measure centered.
- [ ] **Step 6: Add 300 ms autosave coalescing.** Track dirty generations; ignore stale timers. Flush before note switch/new/delete/window close/image insert/format actions. Failure keeps dirty state and visible text for retry.
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
- [ ] **Step 4: Load visible thumbnails only.** Decode the first visible resource to a 2×56-pt target; cache by resource id/size with a fixed cap. Missing/corrupt images become text-only and never mutate HTML.
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
