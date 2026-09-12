# Native Find-in-Note C1 Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make Cmd-F find literal text in the current native note with live match count, visible highlights, circular previous/next navigation, and no document/save/undo mutation.

**Architecture:** The retained `EditorCore` owns ephemeral `FindState` keyed to its current `Document`; it is neither canonical HTML nor an `AppModel` route. The editor renderer paints only viewport-visible match rectangles behind glyphs. `LibraryShell` owns a compact, focusable GPUI panel outside the document canvas; it never creates a second editor or saves find state.

**Tech Stack:** Rust, GPUI, existing structured `Document`/`EditorCore`/`LayoutRegistry`/`EditorSurface`, existing UTF-16-safe `TitleInput` for the panel, `regex` for escaped Unicode-aware literal case-folded matching if needed.

**Spec:** `.superpowers/sdd/2026-09-11-evernote-core-notes-roadmap/task-7-source-brief.md` (approved Task 7 find rows) and this bounded C1 design. Original Evernote 11.32.5 source is under `/Users/kevinhao/Projects/joplin-reconstruction/evernote-11.32.5/common-editor-sourcemap/@evernote/common-editor/src/apps/peso/modules/find/`: `commands/find.ts`, `commands/findnext.ts`, `commands/findprev.ts`, `commands/closeFindInNote.ts`, `state.ts`, `plugin.ts`, `ui/findInNoteUiPlugin.tsx`, `ui/FindInNote.tsx`, `ui/keyBindings.ts`, `utils/index.ts`. Read the exact files when implementing; the source-backed behavior is ephemeral document-search state, literal in-note substring grammar distinct from library search, circular next/previous, close-UI preserving highlights, a shell-owned overlay, and viewport-bounded decorations. This is behavioral reconstruction, not a port of Electron/React code.

## Global Constraints

- Do not touch the user's personal Joplin/Evernote library, NAS, sync server, or live profile. Use disposable fixtures only.
- Do not change the stable Task 4 save/undo/IME bridge, Task 5 image/attachment ownership, Task 6 organization routes, or Task 7 SearchRoute/Cmd-K. Add find beside them.
- Find is literal within each text block (including Chinese substrings), not the library FTS grammar. Do not span image/attachment atoms or silently count alt/filename/OCR in C1.
- Default is case-insensitive; offer a case-sensitive toggle. UTF-8 byte ranges must remain valid for CJK, emoji, and combining sequences. Empty input clears matches, not note content.
- Opening/typing/navigating/closing find must not change canonical HTML, durable revision/save generation, selection, undo/redo depth, or active `NoteSession` identity. Document edits and undo/redo must refresh matches without losing editor focus/session.
- For >500 matches, keep an exact compact total/order but paint only visible viewport matches plus at most a small overscan; never allocate a full note text copy or 500+ GPUI decoration elements per frame.
- Do not claim replace, tag-chip find, embedded PDF/OCR find, IME physical-keyboard acceptance, or Task 7/M2 completion in this slice.
- Before each production behavior, add a real failing test and observe expected RED; then minimal GREEN. Run focused tests, full GPUI suite with only the documented unrelated donor skip, fmt/check/diff, Release build, and a disposable-profile UI smoke before slice acceptance.

## File ownership and interfaces

- Create `packages/app-lite-gpui/src/native_editor/find.rs`: `FindState` and compact `FindMatch { node_id: NodeId, utf8_range: Range<usize> }`; `set_query(&Document, &str, case_sensitive: bool)`, `reconcile(&Document)`, `next()`, `previous()`, `clear()`, `total()`, `primary()` and visible-match iteration. Keep one query string; no per-match text copies. Cache per-block revision or equivalent so unchanged blocks are not rescanned after a one-block edit.
- Modify `native_editor/mod.rs`, `core.rs`: one ephemeral `FindState` per retained editor and production methods that expose/update it without `Transaction` or history entry. Every successful document mutation/undo/redo/prepared durable install revalidates it. The C1 consumer needs `set_find_query`, `find_next`, `find_previous`, `find_summary`, and `clear_find` or equally small typed equivalents.
- Modify `native_editor/render.rs` and, only if necessary, `layout.rs`: gather highlight rectangles from visible laid-out text blocks, paint secondary and primary colors before glyphs without changing ordinary selection/caret painting. A primary offscreen match must still be navigable; shaping/reveal is owned by `EditorSurface`.
- Modify `native_editor/surface.rs`: expose a narrow `reveal_find_primary` using the retained scroll handle and real block/caret geometry. Do not estimate scroll by `index * height`.
- Modify `ui/mod.rs` (or extract a focused `ui/find.rs` if that makes ownership clearer): Cmd-F panel tied to current `NoteSession`, `TitleInput` query, count/current index, previous/next, case toggle, close, Escape, Cmd-G, Shift-Cmd-G. The panel is anchored to the editor pane outside the canvas. Closing preserves highlights; switching notes clears old find state and panel; reopening the same note can reuse prior query/results. If there is no active note, Cmd-F must not create one.
- Test `native_editor/tests.rs` or new `native_editor/find.rs` tests plus mounted `ui/tests.rs` cases. Avoid assertions on framework mocks or source text; assert real editor/document/paint/route outcomes.

---

### Task 1: Ephemeral native find state and visible painting

**Files:** create `native_editor/find.rs`; modify `native_editor/mod.rs`, `core.rs`, `render.rs`, optionally `layout.rs`, `surface.rs`; test `native_editor/tests.rs` and local find/render tests.

**Interfaces:** Consumes `Document::blocks()`, `BlockContent::as_text()`, `Block::revision`, `DocPoint`, `LayoutRegistry::visible()`/`selection_rects()`, and retained `EditorCore`. Produces the small editor find methods and `EditorSurface::reveal_find_primary` for Task 2.

- [ ] **Step 1: RED for literal matching and non-mutation.** Add a fixture with paragraphs `"上海上海 café"`, `"SHANGHAI 上海"`, and an image/attachment atom. Assert query `"上海"` yields literal CJK substring positions and exact count, case-insensitive `"shanghai"` finds the Latin text, case-sensitive does not, and empty query clears. Capture `document.revision()`, HTML codec output, selection, undo/redo depths before/after query and next/previous; assert unchanged. Run focused `cargo test --manifest-path packages/app-lite-gpui/Cargo.toml find_ -- --nocapture` and record an expected missing-behavior failure before code.
- [ ] **Step 2: GREEN for compact match state.** Implement `FindState` over text blocks only. A match records only stable `NodeId` and UTF-8 range; use literal escaped matching with valid byte boundaries. `next` and `previous` wrap at ends and preserve primary as much as possible across a one-block edit. Cache each block's revision or equivalent to avoid rescanning unchanged blocks. Reconcile on the actual core commit/undo/redo paths, not during every paint.
- [ ] **Step 3: RED for visual and long-note behavior.** Add a viewport fixture with >500 occurrences across many blocks. Assert total is exact, initial paint materializes only visible highlight geometry, navigation to an offscreen primary reveals its actual block, and a one-block edit rescans only the changed block (a test-only observer may count scanned blocks, but assert the real result as well). Ensure ordinary selection color/caret remains unchanged. Run focused test and observe expected RED.
- [ ] **Step 4: GREEN for visible paint and reveal.** Use `LayoutRegistry`'s real visible block geometry; paint matched rectangles behind glyphs, distinguish primary, and reveal using retained `ScrollHandle`/block geometry. Do not flatten all text or create one UI child per match. Run focused tests, `cargo fmt --manifest-path packages/app-lite-gpui/Cargo.toml --check`, then commit this independently testable engine slice.

### Task 2: Retained Cmd-F shell panel and keyboard flow

**Files:** modify `ui/mod.rs` and focused UI tests (optionally create `ui/find.rs`); use Task 1 editor APIs. No app-core schema or network changes.

**Interfaces:** Consumes current `NoteSession::editor()`/`EditorSurface`, `TitleInput` UTF-16 input, and Task 1 `find_summary`/navigation/reveal. Produces only ephemeral shell panel state; existing `LibraryRoute::Search` and Cmd-K remain independent.

- [ ] **Step 1: RED for mounted behavior.** In a real disposable repository mounted shell, create two notes, type/find a CJK substring in note A, assert Cmd-F focuses the panel, count/current index are visible, Enter/Cmd-G/Shift-Cmd-G wrap and reveal the selected actual match, Escape/close preserves highlights and returns focus, reopening restores the query, and switching to note B clears A's panel/highlights without changing either note's HTML/save/undo. A second assertion proves Cmd-K still opens its independent global palette. Observe focused RED.
- [ ] **Step 2: GREEN for GPUI panel.** Bind Cmd-F/Find Next/Find Previous to the shell and panel. Use one retained `TitleInput` and observation; keep panel outside the editable canvas and positioned inside the editor column. Show a truthful `x / total` or `无结果` label, a case toggle, previous/next and close controls with non-overlapping hit targets at default and narrow widths. Preserve the same session/editor/focus owner on close; defer focus restoration when necessary to avoid Escape reaching the underlying editor/link overlay.
- [ ] **Step 3: RED/GREEN edge cases.** Empty note/no selection, no active note, read-only Trash preview, query change during IME marked composition, rapid note switch, undo while panel open, image ahead of offscreen text hit, and >500 matches need mounted or core regressions only where they cover a real branch. Do not add a fake keyboard path that bypasses GPUI action routing.
- [ ] **Step 4: Verify and report.** Run focused tests, full relevant GPUI suite with only `cross_block_cut_writes_markdown_deletes_range_and_undo_restores` skipped, both crate fmt checks, `git diff --check`, and GPUI Release build. Controller independently repeats final gates and opens only a disposable Release profile. Commit code and write source→behavior→test crosswalk plus unresolved limitations; do not tag/push/deploy this slice without a fresh release decision.
