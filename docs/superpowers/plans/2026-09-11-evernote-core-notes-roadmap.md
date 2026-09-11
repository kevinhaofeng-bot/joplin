# Evernote Core Notes Replica Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Turn the existing GPUI editor spike into a complete, low-memory, personal Evernote-style notes application with local persistence, visual browsing, organization, search, multimodal attachments, migration, reliable NAS sync, and verified recovery.

**Architecture:** Preserve the existing GPUI `EditorCore`, but place it inside a real `AppModel` and `NoteSession`. Extract the validated SQLite/HTML/resource code from the retired AppKit client into a UI-independent Rust core crate, then add lightweight repository projections, background search indexing, durable sync outbox, and a single-process Rust NAS server. Canonical note bodies remain readable UTF-8 HTML and searchable text; merge metadata is never the only recoverable representation.

**Tech Stack:** Rust 2024, GPUI/Metal, rusqlite with bundled SQLite/FTS5/WAL, serde/serde_json, html5ever, SHA-256 content-addressed resources, macOS Vision/PDFKit through narrow Rust FFI adapters, Axum-based NAS service, Docker, restic.

**Spec:** `docs/superpowers/specs/2026-09-11-evernote-core-notes-product-design.md`

## Global Constraints

- Every task must re-read the relevant entries and unpacked source named in `docs/research/evernote-11.32.5-core-product-behavior-map.md` before implementation and again before review.
- Every task report must include an evidence crosswalk: Evernote unpacked source path and observed behavior → our independent implementation file/function → mutation-sensitive verification. A behavior without source evidence must be labeled as our own product/architecture decision, never as an Evernote reverse-engineering result.
- The current `packages/app-lite-gpui/src/native_editor` transaction, input, selection, layout, image-cache, undo-budget, and memory-measurement paths are the stable baseline; replace them only after an equivalent real Release regression passes.
- Do not initialize WebKit, Electron, Node, Tauri, or the legacy Joplin sidecar in the product path.
- The retired `NSTextView` UI remains untouched; only its UI-independent SQLite, HTML, resource, preview, backup, and migration logic may be extracted.
- Canonical note bodies are deterministic UTF-8 HTML in SQLite; Markdown is import/export input, not the internal feature ceiling.
- Every list query excludes `body_html`, merge-state blobs, and attachment bytes.
- Local save, background index, and remote sync are separate status pipelines and separate retry domains.
- An image insert updates the active `EditorCore`, resource relation, persisted note snapshot, and note-card projection from one application transaction; switching away and back is never a refresh mechanism.
- The first product milestone is not complete until create, edit, save, restart, select, switch, delete/restore, and image insertion work on real notes.
- Prioritize data-loss, IME, selection, image-boundary, list, persistence, and sync tests; cosmetic snapshot breadth may follow the MVP.
- Empty editor and typical editor retain the 80 MiB and 120 MiB RSS gates; the full library and typical full product retain the 120 MiB and 160 MiB RSS gates.
- Do not tag or call a milestone complete from compilation or unit tests alone; record a real Release application run and the exact user flow.

---

## Milestones

| Milestone | Product outcome | Tasks | Exit gate |
| --- | --- | --- | --- |
| M1: 日用纵切 | 真实资料库、新建、自动保存、列表切换、图片即时显示 | 1–5 | 重启不丢标题/正文/图片，三篇真实笔记可连续编辑切换 |
| M2: 找得到、理得清 | 三栏、笔记本/组、标签、回收站、排序、全库搜索 | 6–7 | 真实资料库可浏览、过滤、搜索、删除和恢复 |
| M3: 搬得进、拿得出 | ENEX/Joplin staging 导入、HTML/JSON/附件导出、OCR/PDF | 8 | 数量、哈希、抽样视觉和搜索命中通过迁移核验 |
| M4: 离线优先同步 | Rust NAS 服务、outbox、cursor、断点资源、冲突副本 | 9 | 两客户端断网编辑、恢复同步、断点续传和服务恢复通过 |
| M5: 可发布 | 恢复演练、内存/延迟、栏位动画、可观察状态、打包 | 10 | 全部核心完成定义与 Release 预算通过 |

## Planned File Structure

### UI-independent core

- `packages/app-lite-core/Cargo.toml`: reusable data/domain crate.
- `packages/app-lite-core/src/lib.rs`: public module surface only.
- `packages/app-lite-core/src/domain.rs`: entity IDs and Note/Notebook/Stack/Tag/Resource projections.
- `packages/app-lite-core/src/document.rs`: `native_editor::Document`-independent canonical HTML document DTO and codec boundary.
- `packages/app-lite-core/src/schema.rs`: forward-only SQLite migrations.
- `packages/app-lite-core/src/repository.rs`: atomic note/entity operations and library events.
- `packages/app-lite-core/src/query.rs`: lightweight list and organization queries.
- `packages/app-lite-core/src/resource.rs`: hardened SHA-256 blob store.
- `packages/app-lite-core/src/revision.rs`: edit journal, snapshots, local history.
- `packages/app-lite-core/src/search/query.rs`: search parser and filter AST.
- `packages/app-lite-core/src/search/index.rs`: FTS writes and search queue.
- `packages/app-lite-core/src/search/extract.rs`: OCR/PDF extract adapter interface.
- `packages/app-lite-core/src/import_export/enex.rs`: ENEX staging importer.
- `packages/app-lite-core/src/import_export/joplin.rs`: Joplin profile/JEX staging importer.
- `packages/app-lite-core/src/import_export/export.rs`: readable export bundle.

### GPUI application

- `packages/app-lite-gpui/src/app/mod.rs`: `AppModel` assembly.
- `packages/app-lite-gpui/src/app/actions.rs`: application-level typed commands.
- `packages/app-lite-gpui/src/app/navigation.rs`: route, filters, back/forward, selected NoteId.
- `packages/app-lite-gpui/src/app/note_session.rs`: active title/editor/save lifecycle.
- `packages/app-lite-gpui/src/app/save_coordinator.rs`: journal, settled snapshot, explicit flush.
- `packages/app-lite-gpui/src/app/layout.rs`: one/two/three-pane state and animation.
- `packages/app-lite-gpui/src/ui/sidebar.rs`: notebooks, stacks, tags, shortcuts, trash.
- `packages/app-lite-gpui/src/ui/note_list.rs`: virtualized note list and view modes.
- `packages/app-lite-gpui/src/ui/note_card.rs`: lightweight card rendering and thumbnail request.
- `packages/app-lite-gpui/src/ui/search.rs`: Cmd-K search field, filters, suggestions.
- `packages/app-lite-gpui/src/ui/status.rs`: local save/index/sync status.
- `packages/app-lite-gpui/src/native_editor/codec.rs`: GPUI document to/from core canonical document.
- `packages/app-lite-gpui/src/platform/macos_text_extract.rs`: Vision/PDFKit adapter.

### Sync

- `packages/app-lite-protocol/Cargo.toml`: client/server protocol crate.
- `packages/app-lite-protocol/src/lib.rs`: versioned request/response types and validation.
- `packages/app-lite-gpui/src/sync/engine.rs`: pull/push scheduler and state machine.
- `packages/app-lite-gpui/src/sync/outbox.rs`: repository-backed batching/rollup.
- `packages/app-lite-gpui/src/sync/resources.rs`: hash probe, chunk upload, Range download.
- `packages/app-lite-gpui/src/sync/conflict.rs`: merge result and conflict-note creation.
- `packages/app-lite-server/Cargo.toml`: NAS server crate.
- `packages/app-lite-server/src/main.rs`: config, lifecycle, shutdown.
- `packages/app-lite-server/src/api.rs`: authenticated sync routes.
- `packages/app-lite-server/src/store.rs`: SQLite entity/op/cursor transactions.
- `packages/app-lite-server/src/blobs.rs`: staged content-addressed resource publication.
- `infra/app-lite-server/docker-compose.yml`: private NAS deployment.
- `infra/app-lite-server/backup.sh`: consistent snapshot and restic hook.
- `infra/app-lite-server/restore-drill.sh`: isolated restore verifier.

---

### Task 1: Extract the validated Rust storage foundation

**Files:**
- Create: `packages/app-lite-core/Cargo.toml`
- Create: `packages/app-lite-core/src/lib.rs`
- Create: `packages/app-lite-core/src/document.rs`
- Create: `packages/app-lite-core/src/resource.rs`
- Create: `packages/app-lite-core/tests/document_roundtrip.rs`
- Create: `packages/app-lite-core/tests/resource_store.rs`
- Modify: `packages/app-lite-gpui/Cargo.toml`
- Reference: `packages/app-lite-native/src/html_body.rs`
- Reference: `packages/app-lite-native/src/resource_store.rs`

**Interfaces:**
- Produces: `CanonicalDocument`, `CanonicalHtml`, `SearchText`, `ResourceId`, `ResourceStore::put`, `ResourceStore::read`.
- Consumes: the existing validated HTML parser/serializer and hardened resource filesystem rules.

- [ ] **Step 1: Write extraction-preservation tests**

Create fixtures for headings, inline marks, nested lists, checks, image references, invalid HTML, duplicate resource references, symlinked resource roots, interrupted temporary writes, and 100,000-character text. The key assertions are:

```rust
let parsed = CanonicalDocument::parse_html(input)?;
let html = parsed.to_canonical_html();
assert_eq!(CanonicalDocument::parse_html(&html)?, parsed);
assert_eq!(parsed.search_text(), expected_visible_text);
assert_eq!(parsed.resource_ids(), expected_resource_order);
```

- [ ] **Step 2: Run the focused tests and verify the new crate is absent**

Run: `cargo test --manifest-path packages/app-lite-core/Cargo.toml`

Expected: FAIL because the manifest and public interfaces do not exist.

- [ ] **Step 3: Move, do not rewrite, the proven codec and blob logic**

Extract only the pure logic from `html_body.rs` and `resource_store.rs`. Keep atomic temp-file publication, `fsync`, SHA-256 addressing, symlink refusal, size limits, canonical escaping, invalid-node stripping, visible-text projection, and resource-order tests. Remove AppKit types and Joplin-specific names from the public API.

- [ ] **Step 4: Prove equivalence and wire the path dependency**

Run:

```bash
cargo test --manifest-path packages/app-lite-core/Cargo.toml
cargo test --manifest-path packages/app-lite-native/Cargo.toml html_body
cargo test --manifest-path packages/app-lite-native/Cargo.toml resource_store
cargo check --manifest-path packages/app-lite-gpui/Cargo.toml
```

Expected: all PASS; the GPUI binary still launches its unchanged editor spike.

- [ ] **Step 5: Commit**

```bash
git add packages/app-lite-core packages/app-lite-gpui/Cargo.toml packages/app-lite-gpui/Cargo.lock
git commit -m "Extract native notes core storage"
```

### Task 2: Build the complete local library schema and repository

**Files:**
- Create: `packages/app-lite-core/src/domain.rs`
- Create: `packages/app-lite-core/src/schema.rs`
- Create: `packages/app-lite-core/src/repository.rs`
- Create: `packages/app-lite-core/src/query.rs`
- Create: `packages/app-lite-core/src/revision.rs`
- Create: `packages/app-lite-core/tests/repository_flow.rs`
- Create: `packages/app-lite-core/tests/migration.rs`
- Modify: `packages/app-lite-core/src/lib.rs`
- Reference: `packages/app-lite-native/src/core.rs`

**Interfaces:**
- Produces: `LibraryRepository::open`, `create_note`, `load_note`, `save_note`, `list_notes`, `trash_note`, `restore_note`, `purge_note`, `create_notebook`, `create_stack`, `create_tag`, `move_notes`, `set_note_tags`, `append_edit_journal`, `flush_snapshot`.
- Produces event type:

```rust
pub enum LibraryEvent {
    NoteCreated(NoteId),
    NoteProjectionChanged(NoteId),
    NoteTrashed(NoteId),
    NoteRestored(NoteId),
    OrganizationChanged,
    SearchProjectionQueued(NoteId),
    SyncQueued(EntityRef),
}
```

- [ ] **Step 1: Write the end-to-end repository test first**

The test creates a default notebook, three notes, two tags and one image resource; it saves a title/body change, checks the lightweight list projection, moves a note, trashes/restores it, closes the repository, reopens it, and verifies all IDs, relationships, HTML, text, timestamps and resource order.

- [ ] **Step 2: Write migration atomicity tests**

Cover a clean database, the existing app-lite-native schema v3, failure in the middle of a forward migration, a source database replaced by symlink, and a second open after successful migration. A failed migration must leave the original schema version and data readable.

- [ ] **Step 3: Implement schema v4 and typed transactions**

Create notes, notebooks, stacks, tags, note_tags, resource tables, note_revisions, edit_journal, search_queue, sync_outbox, sync_cursor, sync_conflicts, shortcuts, search_history and settings. Use `PRAGMA journal_mode=WAL`, `foreign_keys=ON`, `user_version=4`, opaque 32-hex IDs, and monotonic per-entity revisions.

- [ ] **Step 4: Implement lightweight projections**

`list_notes(ListQuery)` selects only id, title prefix, snippet, updated/deleted time, notebook ID, selected thumbnail ID and attachment count. Add a SQL-observation test proving the query does not select `body_html`, `merge_state`, or resource bytes.

- [ ] **Step 5: Run tests and commit**

```bash
cargo test --manifest-path packages/app-lite-core/Cargo.toml repository_flow
cargo test --manifest-path packages/app-lite-core/Cargo.toml migration
git add packages/app-lite-core
git commit -m "Build local notes library repository"
```

### Task 3: Replace the spike shell with a real library application

**Files:**
- Create: `packages/app-lite-gpui/src/app/mod.rs`
- Create: `packages/app-lite-gpui/src/app/actions.rs`
- Create: `packages/app-lite-gpui/src/app/navigation.rs`
- Create: `packages/app-lite-gpui/src/ui/mod.rs`
- Create: `packages/app-lite-gpui/src/ui/sidebar.rs`
- Create: `packages/app-lite-gpui/src/ui/note_list.rs`
- Create: `packages/app-lite-gpui/src/ui/note_card.rs`
- Modify: `packages/app-lite-gpui/src/main.rs`
- Preserve: `packages/app-lite-gpui/src/spike_app.rs` as a measurement-only route until M1 passes.
- Test: `packages/app-lite-gpui/src/app/tests.rs`

**Interfaces:**
- Consumes: `Arc<LibraryRepository>` and `LibraryEvent`.
- Produces: `AppModel`, `NavigationState`, `ListViewMode`, `SelectNote`, `CreateNote`, `TrashNote`, `ToggleSidebar`, `ToggleNoteList`.

- [ ] **Step 1: Write application-state tests**

Assert `Cmd-N` calls repository creation before selection, search context clears on new note, selecting by NoteId survives sorting, deleting the selected note chooses the nearest surviving note, and restart restores pane widths plus the last valid selected NoteId without hydrating every body.

- [ ] **Step 2: Implement `AppModel` and typed actions**

`AppModel` owns repository, navigation, list projection, optional active `NoteSession`, pane state and status. GPUI controls dispatch application actions; controls do not mutate repository fields directly.

- [ ] **Step 3: Build the first real two-pane UI**

Render a virtualized card list and the existing editor surface. Replace fixed sample cards and `sample_document()` in the default product route. Empty library shows one purposeful action: “新建第一篇笔记”. The `--evernote-spike` route remains available only for isolated performance fixtures.

- [ ] **Step 4: Verify the vertical shell**

Run:

```bash
cargo test --manifest-path packages/app-lite-gpui/Cargo.toml app::tests
cargo build --release --manifest-path packages/app-lite-gpui/Cargo.toml
```

Launch the Release binary against a temporary profile; create three empty notes, sort, select, quit and reopen. Confirm list selection is restored and only the selected note body is loaded.

- [ ] **Step 5: Commit**

```bash
git add packages/app-lite-gpui/src/app packages/app-lite-gpui/src/ui packages/app-lite-gpui/src/main.rs
git commit -m "Replace editor spike with notes library shell"
```

### Task 4: Connect title and EditorCore to durable NoteSession saving

**Files:**
- Create: `packages/app-lite-gpui/src/app/note_session.rs`
- Create: `packages/app-lite-gpui/src/app/save_coordinator.rs`
- Create: `packages/app-lite-gpui/src/native_editor/codec.rs`
- Modify: `packages/app-lite-gpui/src/native_editor/mod.rs`
- Modify: `packages/app-lite-gpui/src/native_editor/core.rs`
- Modify: `packages/app-lite-gpui/src/native_editor/chrome.rs`
- Test: `packages/app-lite-gpui/src/app/note_session_tests.rs`

**Interfaces:**
- Produces:

```rust
pub struct NoteSession {
    pub note_id: NoteId,
    pub editor: Entity<EditorCore>,
    pub title: Entity<TitleInput>,
    pub save: SaveCoordinator,
}

pub enum SaveState { Clean, Dirty, Journaling, Snapshotting, Failed(SaveError) }

pub async fn flush(&mut self, reason: FlushReason) -> Result<SavedRevision, SaveError>;
```

- [ ] **Step 1: Write the title/body/restart failure test**

Type a Chinese title and body through the real `EntityInputHandler`, advance the save clock, destroy the session, reopen the repository and assert the exact title, body, styles and selection-safe content. Add a stale-timer test where session A's delayed save fires after selecting session B and prove it cannot overwrite B.

- [ ] **Step 2: Write crash-journal and explicit-flush tests**

Cover a change before the 500 ms snapshot, reconstruction from `edit_journal`, the 15-second maximum snapshot, note switch, window close, quit, delete and manual sync. Every boundary must await the exact session generation or surface a blocking local-save error.

- [ ] **Step 3: Implement deterministic document codec**

Map Paragraph, Heading, BulletItem, OrderedItem, CheckItem, Quote, Code, Image, Attachment and Divider plus Bold/Italic/Underline/Strike/Highlight/Link/InlineCode to the canonical core document. Reject invalid nesting before the repository transaction; parsing the generated HTML must reproduce the same semantic document.

- [ ] **Step 4: Implement SaveCoordinator**

Journal compact readable operations within 100 ms, snapshot after 500 ms settled time, force a snapshot at 15 seconds, and compare generation/revision before publishing Clean. `LibraryEvent::NoteProjectionChanged` is emitted only after the snapshot transaction commits.

- [ ] **Step 5: Run the real flow and commit**

```bash
cargo test --manifest-path packages/app-lite-gpui/Cargo.toml note_session
cargo test --manifest-path packages/app-lite-core/Cargo.toml repository_flow
git add packages/app-lite-gpui/src/app packages/app-lite-gpui/src/native_editor
git commit -m "Persist GPUI note editing sessions"
```

### Task 5: Make images and attachments a complete cross-layer transaction

**Files:**
- Modify: `packages/app-lite-gpui/src/native_editor/images.rs`
- Modify: `packages/app-lite-gpui/src/native_editor/transaction.rs`
- Modify: `packages/app-lite-gpui/src/native_editor/core.rs`
- Modify: `packages/app-lite-gpui/src/app/note_session.rs`
- Modify: `packages/app-lite-gpui/src/ui/note_card.rs`
- Create: `packages/app-lite-gpui/src/ui/attachment_card.rs`
- Test: `packages/app-lite-gpui/src/app/image_flow_tests.rs`

**Interfaces:**
- Consumes: `ResourceStore::put`, `LibraryRepository::associate_resource`, current `Selection`.
- Produces: `NoteSession::insert_resource(ResourceImport, InsertIntent)` and `ThumbnailKey { resource_id, pixel_size, scale }`.

- [x] **Step 1: Reproduce the reported stale-image bug as a test**

Mount the real AppModel, insert an image through the picker completion path, and assert in the same presentation cycle that: the active document contains the Image block; editor measured extent grows; the image cache has a pending/loaded key; and the selected card projection references the resource. The test must not select another note to pass.

- [x] **Step 2: Lock image-boundary behavior**

Add real event-path tests for typing before and after an image, inserting two adjacent images, selecting across text-image-text, Backspace/Delete at both boundaries, undo/redo, IME composition beside an image, and long-image reflow. No sequence may hide typed text or move the document on alternating frames.

- [x] **Step 3: Implement one insert transaction**

Import and validate the resource, insert at the saved DocPoint, update note-resource order, flush the note snapshot, emit one projection event, then schedule decode. On failure before association, roll back resource metadata; the content-addressed blob may remain for safe reuse/GC.

- [x] **Step 4: Add attachment cards**

Images render inline. PDF, audio, video and other files render a filename/mime/size card with system preview/open actions. Attachment bytes never enter the document string or GPUI element state.

- [x] **Step 5: Perform M1 acceptance and commit**

In a fresh temporary profile, create three notes; type Chinese titles/body; paste a screenshot; drag a JPEG; insert a PDF; type before and after images; switch rapidly; quit and reopen. Verify exact persistence and immediate display.

```bash
git add packages/app-lite-gpui/src packages/app-lite-core
git commit -m "Complete durable image and attachment flow"
```

M1 is complete only after this real Release flow passes. At this point the product becomes an actual minimal notes application; Tasks 6–10 expand it to the full core target.

### Task 6: Add Evernote-style organization, navigation, and pane behavior

**Files:**
- Modify: `packages/app-lite-gpui/src/ui/sidebar.rs`
- Modify: `packages/app-lite-gpui/src/ui/note_list.rs`
- Modify: `packages/app-lite-gpui/src/ui/note_card.rs`
- Create: `packages/app-lite-gpui/src/app/layout.rs`
- Modify: `packages/app-lite-gpui/src/app/navigation.rs`
- Modify: `packages/app-lite-core/src/query.rs`
- Test: `packages/app-lite-gpui/src/app/organization_tests.rs`

**Interfaces:**
- Produces: `LibraryRoute::{AllNotes, Notebook, Stack, Tag, Shortcuts, Recent, Trash, Search}`, `PaneMode::{Three, Two, One}`, `ListViewMode::{Cards, Snippets, Compact}`.

- [ ] **Step 1: Write navigation and organization tests**

Cover notebook/stack/tag creation and rename, multi-note move/tagging, list filters, sort order, trash/restore/permanent-delete rules, shortcut pinning, recent notes, back/forward, and selection retention by NoteId.

- [ ] **Step 2: Implement the three-pane shell**

Build the Evernote-style sidebar, virtualized list and editor. Pane widths persist; sidebars collapse without destroying the NoteSession. All list modes use the same query and selection model.

- [ ] **Step 3: Add pane motion without extra work**

Animate width and opacity for 200 ms ease-out. During animation, freeze thumbnail requests to the final visible range, reuse the existing editor layout entity, and issue one settled viewport relayout after the final width.

- [ ] **Step 4: Verify real library scale**

Generate 1,662 notes, 31 notebooks, 64 tags and 4,238 resource metadata rows. Scroll each list mode, change filters, collapse/expand panes, and confirm list queries never hydrate bodies.

- [ ] **Step 5: Commit**

```bash
git add packages/app-lite-gpui/src/app packages/app-lite-gpui/src/ui packages/app-lite-core/src/query.rs
git commit -m "Add Evernote style library navigation"
```

### Task 7: Build local search, suggestions, find-in-note, and indexing

**Files:**
- Create: `packages/app-lite-core/src/search/mod.rs`
- Create: `packages/app-lite-core/src/search/query.rs`
- Create: `packages/app-lite-core/src/search/index.rs`
- Create: `packages/app-lite-core/src/search/extract.rs`
- Create: `packages/app-lite-gpui/src/ui/search.rs`
- Create: `packages/app-lite-gpui/src/native_editor/find.rs`
- Create: `packages/app-lite-gpui/src/platform/macos_text_extract.rs`
- Test: `packages/app-lite-core/tests/search.rs`
- Test: `packages/app-lite-gpui/src/app/search_tests.rs`

**Interfaces:**
- Produces:

```rust
pub enum SearchFilter {
    Notebook(String), Stack(String), Tag(String), Created(DateRange),
    Updated(DateRange), Trash(bool), HasAttachment(bool),
}
pub struct SearchQuery { pub terms: Vec<SearchTerm>, pub filters: Vec<SearchFilter> }
pub struct SearchHit { pub note: NoteListItem, pub snippet: String, pub matched_resource: Option<ResourceId> }
```

- [ ] **Step 1: Write parser and query tests**

Cover quoted phrases, escaped quotes, Chinese substring, mixed Latin/Chinese, every supported filter, invalid filters treated as text, tag intersections, pagination, search-history prefix deletion and trash isolation.

- [ ] **Step 2: Implement dual FTS and incremental indexing**

Use `unicode61` for Latin word queries and FTS5 trigram for Chinese/no-space text. Saving a note updates its immediate title/body projection and replaces its `search_queue` row. The worker takes the oldest 100 rows, commits each batch, yields between documents, and retains failed rows with classified error state.

- [ ] **Step 3: Add OCR and PDF extraction adapters**

The core trait consumes a local resource path and returns UTF-8 text plus extractor version. The macOS adapter calls Vision for images and PDFKit for text PDFs on a background executor. Extraction result joins the search index but never alters note body or blocks local save.

- [ ] **Step 4: Implement Cmd-K and Cmd-F**

Cmd-K focuses the global search surface with recent notes, recent queries, notebook and tag suggestions; selecting a hit navigates without discarding the query route. Cmd-F decorates only current-note matches; more than 500 matches limits decorations to the viewport and keeps a primary match cursor.

- [ ] **Step 5: Run M2 search acceptance and commit**

```bash
cargo test --manifest-path packages/app-lite-core/Cargo.toml search
cargo test --manifest-path packages/app-lite-gpui/Cargo.toml search_tests
git add packages/app-lite-core/src/search packages/app-lite-gpui/src/ui/search.rs packages/app-lite-gpui/src/native_editor/find.rs packages/app-lite-gpui/src/platform
git commit -m "Add offline library and note search"
```

### Task 8: Import the real library and guarantee readable export

**Files:**
- Create: `packages/app-lite-core/src/import_export/mod.rs`
- Create: `packages/app-lite-core/src/import_export/enex.rs`
- Create: `packages/app-lite-core/src/import_export/joplin.rs`
- Create: `packages/app-lite-core/src/import_export/export.rs`
- Create: `packages/app-lite-core/src/import_export/manifest.rs`
- Create: `packages/app-lite-gpui/src/ui/import_progress.rs`
- Test: `packages/app-lite-core/tests/enex_import.rs`
- Test: `packages/app-lite-core/tests/joplin_import.rs`
- Test: `packages/app-lite-core/tests/export_restore.rs`

**Interfaces:**
- Produces: `scan_import`, `stage_import`, `verify_staged_import`, `commit_staged_import`, `export_library`, `restore_export`.
- Produces `ImportReport` with counts for notes/notebooks/tags/resources, unsupported constructs, duplicate hashes, failures and source-to-destination IDs.

- [ ] **Step 1: Build small golden migration fixtures**

Include ENEX rich text, nested lists, checks, tags, dates, links, duplicate resources, image dimensions and a PDF; include a Joplin fixture with Markdown, HTML notes, resources, folders, tags and note-tag relations. Expected output includes exact canonical HTML, visible text and resource hashes.

- [ ] **Step 2: Implement scan and staging**

Scan never changes the live profile. Import into a temporary SQLite database and temporary blob root, preserve source IDs in the report, canonicalize bodies, build indexes, and compare all referenced resource hashes before commit.

- [ ] **Step 3: Implement atomic profile commit**

Close the live repository, fsync staging files, rename the existing profile to a timestamped backup, rename staging into place, reopen and run integrity checks. On any failure, restore the original profile and retain the report.

- [ ] **Step 4: Implement readable export and restore test**

Export one HTML file per note, a JSON manifest for metadata/relationships/revisions, and original resource files named by hash plus safe display name. Restore the export into an empty profile and compare every entity count, body HTML, search text, relationship and SHA-256.

- [ ] **Step 5: Run M3 acceptance and commit**

Import a copy of the user's actual Evernote/Joplin data. Compare total active/trash notes, 31 notebooks, 64 tags, 4,238 resources, large attachments and at least 50 stratified visual samples. Numbers that differ remain explicit failures with source IDs.

```bash
git add packages/app-lite-core/src/import_export packages/app-lite-core/tests packages/app-lite-gpui/src/ui/import_progress.rs
git commit -m "Add verified notes migration and export"
```

### Task 9: Deliver reliable local-first NAS synchronization

**Files:**
- Create: `packages/app-lite-protocol/Cargo.toml`
- Create: `packages/app-lite-protocol/src/lib.rs`
- Create: `packages/app-lite-gpui/src/sync/mod.rs`
- Create: `packages/app-lite-gpui/src/sync/engine.rs`
- Create: `packages/app-lite-gpui/src/sync/outbox.rs`
- Create: `packages/app-lite-gpui/src/sync/resources.rs`
- Create: `packages/app-lite-gpui/src/sync/conflict.rs`
- Create: `packages/app-lite-server/Cargo.toml`
- Create: `packages/app-lite-server/src/main.rs`
- Create: `packages/app-lite-server/src/api.rs`
- Create: `packages/app-lite-server/src/store.rs`
- Create: `packages/app-lite-server/src/blobs.rs`
- Create: `infra/app-lite-server/docker-compose.yml`
- Test: `packages/app-lite-server/tests/two_client_sync.rs`
- Test: `packages/app-lite-server/tests/resource_resume.rs`

**Interfaces:**
- Produces protocol messages `PushBatch`, `PushResult`, `PullPage`, `ChangeEnvelope`, `ResourceProbe`, `UploadChunk`, `SyncCursor`.
- Each mutation includes `protocol_version`, `device_id`, `op_id`, `entity`, `base_revision`, `payload`, `created_at`.

- [ ] **Step 1: Write protocol/idempotency tests**

Replay an identical op batch twice and assert one server change; interrupt a response after commit and retry; reject unsupported protocol versions before mutation; validate size limits, hashes, revisions and unknown fields; never echo credentials in errors.

- [ ] **Step 2: Implement client outbox and server op log**

Local entity transaction inserts outbox in the same commit. Client rolls up superseded title/body metadata operations without crossing a delete/restore boundary, sends bounded batches, and removes only acknowledged op IDs. Server commits entity change, op idempotency row and monotonic cursor together.

- [ ] **Step 3: Implement pull and conflicts**

Pull resumes from committed cursor. Non-overlapping entity changes apply automatically. Revision conflict returns both versions; the client preserves the local note and creates `标题（冲突 YYYY-MM-DD HHmm）` with remote/base provenance. Add Loro per-note merge only after the two-client memory and corruption-recovery tests show it improves real concurrent body edits; readable HTML remains in every revision.

- [ ] **Step 4: Implement resumable resources**

Probe by SHA-256, upload fixed chunks with offset/hash, persist received ranges, verify final size/hash, fsync, then atomically publish. Downloads support Range and temporary-file resume. A note may sync metadata before a large resource, but status remains “附件待同步” until all referenced hashes exist remotely.

- [ ] **Step 5: Deploy privately and run M4 acceptance**

Run the server as one Docker service with a bind-mounted local NAS directory, healthcheck, no public anonymous route, and TLS through the existing trusted private ingress. Test two clients editing offline, server restart mid-push, duplicate batch, interrupted 100 MiB upload, credential failure, disk-full simulation, retry recovery and a true same-note conflict.

```bash
git add packages/app-lite-protocol packages/app-lite-server packages/app-lite-gpui/src/sync infra/app-lite-server
git commit -m "Deliver local first NAS note sync"
```

### Task 10: Recovery, performance, visual polish, and release gate

**Files:**
- Create: `packages/app-lite-gpui/src/ui/status.rs`
- Modify: `packages/app-lite-gpui/src/app/layout.rs`
- Modify: `packages/app-lite-gpui/scripts/measure-memory.sh`
- Create: `packages/app-lite-gpui/scripts/measure-product-memory.sh`
- Create: `infra/app-lite-server/backup.sh`
- Create: `infra/app-lite-server/restore-drill.sh`
- Create: `docs/research/evernote-core-product-acceptance.md`
- Modify: `packages/app-lite-gpui/README.md`

**Interfaces:**
- Produces user-visible `SaveStatus`, `IndexStatus`, `SyncStatus`, `RecoveryStatus` with timestamps and actionable failure details.
- Produces machine-readable product performance and restore reports.

- [ ] **Step 1: Add three independent status surfaces**

Show local saving/saved/error, indexing pending/error, and sync offline/syncing/pending attachment/error/last success separately. A green “已保存” may only describe the local snapshot; it must not imply remote sync.

- [ ] **Step 2: Implement backup and isolated restore drill**

Create a consistent SQLite snapshot, blob manifest and restic backup without stopping normal local editing. Restore into a new directory and port, start a second server, connect a fresh client, full-pull, compare entity counts and every resource hash, then destroy only the isolated restore instance.

- [ ] **Step 3: Measure full-product memory and latency**

Use Release builds and the 1,662-note/4,238-resource fixture. Record physical footprint, RSS, Metal texture estimate, card cache, editor cache, undo bytes, first-interactive time, search p95, typing p95 and pane-animation frame time. Assert 120 MiB idle-library and 160 MiB typical-product stable RSS gates without disabling visible content.

- [ ] **Step 4: Run the complete product behavior matrix**

Execute every item in the spec's “产品完成定义” using macOS Pinyin, Finder drag/drop, screenshot clipboard, Preview/Photos copy, image/PDF/audio attachments, library search, note find, offline edits, NAS stop/restart, conflict, import, export and restore. Record PASS only with the exact build hash and evidence path.

- [ ] **Step 5: Update product identity, documentation, and commit**

Remove visible spike/Velotype/Joplin Lite labels from the release UI while retaining source acknowledgements in developer documentation. Update README with implemented capabilities, exact unmet items if any, measured package size/RSS and restore date.

```bash
git add packages/app-lite-gpui infra/app-lite-server docs/research/evernote-core-product-acceptance.md
git commit -m "Pass Evernote core product release gate"
```

## Execution Order and Review Gates

Tasks 1–5 are one M1 stream and must be completed before expanding editor cosmetics. Task 6 and Task 7 can proceed after M1, but merge through separate review gates because organization state and search indexing fail differently. Task 8 runs only against copies and staging profiles. Task 9 does not replace or delete any existing server or user data; it deploys in parallel until two-client sync and restore pass. Task 10 is the only task allowed to declare the product core complete.

Every task receives two reviews:

1. spec/behavior review against the Evernote source mapping and product design;
2. implementation review focused on data loss, stale selection/session state, unbounded caches, hidden body hydration, retry duplication and false status claims.

The first implementation checkpoint is Task 5, not Task 10: once M1 passes, the user receives a build that is finally a real local notes application and can evaluate daily writing while organization, migration and sync continue.
