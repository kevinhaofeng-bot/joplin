# GPUI Evernote Editor Spike Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Build a standalone macOS GPUI editor spike that directly reuses working Velotype code and proves Evernote-parity input, selection, list, toolbar, image-boundary, and memory behavior without WebKit or AppKit text editing.

**Architecture:** Import a pinned, runnable Velotype `dev` revision as the donor baseline under `packages/app-lite-gpui`, then add a new `native_editor` path alongside the donor Markdown editor. The new path owns one document model, selection, transaction engine, input handler, and viewport projection; donor code is retained until each replacement passes an Evernote-derived acceptance test.

**Tech Stack:** Rust 2024, GPUI 0.2 with Metal on macOS, pinned Velotype revision `ddd9f32588c8318173e9a48d588785d568d9ffd8`, `unicode-segmentation`, `smallvec`, GPUI test support, macOS `ps`/`vmmap`/Instruments.

**Spec:** `docs/superpowers/specs/2026-09-09-evernote-native-editor-core-design.md`

## Global Constraints

- Scope is the editor only; do not add sync, search, note lists, Joplin compatibility, CRDT, WebKit, Electron, Node, or Tauri.
- Begin from runnable Velotype code. Do not delete or rewrite a donor path until a focused replacement test passes.
- `EditorCore` is the sole owner of document focus, selection, IME composition, transactions, and undo.
- Blocks are document/render units, never independent text editors or focus islands.
- Every visible toolbar command must execute a document transaction or be visibly disabled.
- Images are structural nodes with stable before/after caret positions and bounded decoded-image caches.
- Empty-editor Release RSS must be at most 80 MiB; the typical 200-block/10-image fixture must be at most 120 MiB.
- Each adopted Evernote mechanism must be linked to an observation in `docs/research/evernote-11.32.5-targeted-reverse.md` and a regression test.

## Spec Coverage

| Approved specification requirement | Implementing tasks and release gate |
|---|---|
| Chinese IME composition and candidate replacement | Task 4, `ime_commit_preserves_utf16_selection` |
| Immediate text/image/text editing without switching or flashing | Task 3 structural insertion plus Task 6 `EN-IMG-01` |
| Cross-block selection, copy, cut, delete, and undo | Task 4, `cross_block_selection_includes_image_atom` and `editing_commands_cross_block_boundaries` |
| Inline marks, paragraph styles, lists, links, and alignment through one transaction system | Tasks 3 and 5, table-driven command tests |
| Selection-derived toolbar state with no dead controls | Task 5, `all_visible_commands_execute_or_are_disabled` |
| Cmd-A, arrows, Return, Backspace, Delete, cut, and undo across structural boundaries | Task 4, `editing_commands_cross_block_boundaries` |
| Viewport-only layout and hard image/undo/layout budgets | Tasks 4, 6, and 7, long fixture plus RSS gates |
| No WebKit or renderer child process | Task 7, `otool -L` and process-count gates |

## File Structure

- `packages/app-lite-gpui/`: exact pinned Velotype donor tree, then the independent GPUI spike.
- `packages/app-lite-gpui/UPSTREAM.md`: pinned revision, import command, retained donor modules, and replacement map.
- `packages/app-lite-gpui/src/native_editor/model.rs`: compact block/mark document model and stable positions.
- `packages/app-lite-gpui/src/native_editor/transaction.rs`: validated editor operations and change sets.
- `packages/app-lite-gpui/src/native_editor/history.rs`: byte-budgeted inverse-operation undo/redo.
- `packages/app-lite-gpui/src/native_editor/input.rs`: single GPUI `EntityInputHandler` and UTF-8/UTF-16 conversion.
- `packages/app-lite-gpui/src/native_editor/layout.rs`: block layout registry, viewport window, hit testing, and caret geometry.
- `packages/app-lite-gpui/src/native_editor/render.rs`: GPUI rendering for text, selection, caret, lists, and images.
- `packages/app-lite-gpui/src/native_editor/commands.rs`: shared toolbar/overflow command catalogue and query state.
- `packages/app-lite-gpui/src/native_editor/images.rs`: image metadata, async thumbnail decode, and LRU budget accounting.
- `packages/app-lite-gpui/src/native_editor/tests.rs`: model, transaction, IME, selection, list, and image regressions.
- `packages/app-lite-gpui/src/spike_app.rs`: Evernote-parity spike window and deterministic test fixtures.
- `packages/app-lite-gpui/scripts/measure-memory.sh`: stable RSS and process-count measurement.
- `docs/research/evernote-editor-behavior-matrix.md`: evidence-to-command-to-test traceability.

---

### Task 1: Import and freeze the working Velotype donor baseline

**Files:**
- Create: `packages/app-lite-gpui/**`
- Create: `packages/app-lite-gpui/UPSTREAM.md`
- Verify: `packages/app-lite-gpui/src/components/block/input.rs`
- Verify: `packages/app-lite-gpui/src/editor/selection.rs`
- Verify: `packages/app-lite-gpui/src/editor/file_drop.rs`

**Interfaces:**
- Consumes: Velotype `dev` revision `ddd9f32588c8318173e9a48d588785d568d9ffd8`.
- Produces: a runnable donor executable and an immutable reference point for later native-editor replacements.

- [ ] **Step 1: Import the pinned donor tree without hand-retyping it**

Run from the repository root:

```bash
git subtree add --prefix packages/app-lite-gpui https://github.com/manyougz/velotype.git ddd9f32588c8318173e9a48d588785d568d9ffd8 --squash
```

Expected: `packages/app-lite-gpui/Cargo.toml`, `src/`, `assets/`, tests, and lockfile exist, and the subtree commit records the upstream revision.

- [ ] **Step 2: Build the donor before changing it**

Run:

```bash
cargo build --manifest-path packages/app-lite-gpui/Cargo.toml --release
```

Expected: PASS and produce `packages/app-lite-gpui/target/release/velotype`.

- [ ] **Step 3: Run the donor editor tests**

Run:

```bash
cargo test --manifest-path packages/app-lite-gpui/Cargo.toml --all-targets
```

Expected: all existing library tests PASS. If an upstream test fails unchanged, record the exact test and failure in `UPSTREAM.md`; do not modify it merely to make the import green.

- [ ] **Step 4: Record the reuse map**

Create `packages/app-lite-gpui/UPSTREAM.md` with this content:

```markdown
# Velotype donor baseline

- Repository: https://github.com/manyougz/velotype
- Branch: dev
- Revision: ddd9f32588c8318173e9a48d588785d568d9ffd8
- Imported: 2026-09-09

## Retained until replacement passes

| Donor module | Reused mechanism | Replacement gate |
|---|---|---|
| `components/block/input.rs` | GPUI IME bridge and UTF-16 conversion | native editor IME tests |
| `components/block/element.rs` | text layout and hit testing | native layout tests |
| `editor/selection.rs` | cross-block selection behavior | native selection tests |
| `editor/history.rs` | undo behavior reference | inverse-transaction tests |
| `editor/file_drop.rs` | image paste/drop intake | image-boundary tests |
| `editor/render.rs` | viewport culling and spacing | long-document tests |

The donor Markdown editor remains buildable while `native_editor` is developed beside it.
```

- [ ] **Step 5: Commit the baseline record**

```bash
git add packages/app-lite-gpui/UPSTREAM.md
git commit -m "Document Velotype donor baseline"
```

---

### Task 2: Turn Evernote reverse-engineering results into executable acceptance cases

**Files:**
- Create: `docs/research/evernote-editor-behavior-matrix.md`
- Create: `packages/app-lite-gpui/src/native_editor/mod.rs`
- Create: `packages/app-lite-gpui/src/native_editor/acceptance.rs`
- Modify: `packages/app-lite-gpui/src/main.rs`

**Interfaces:**
- Consumes: `docs/research/evernote-11.32.5-targeted-reverse.md`.
- Produces: `AcceptanceCase` and a stable list of parity gates used by every later task.

- [ ] **Step 1: Write the behavior matrix before editor changes**

Create `docs/research/evernote-editor-behavior-matrix.md`:

```markdown
# Evernote editor behavior matrix

| ID | Evernote mechanism | User-visible sequence | Required result | Automated gate |
|---|---|---|---|---|
| EN-IME-01 | CompositionSafeInput | Type `中华人民共和国` with macOS Pinyin, revise a candidate, commit | no lost, duplicated, or reordered text | `ime_commit_preserves_utf16_selection` |
| EN-SEL-01 | mapped structured selection | drag from paragraph through image into next paragraph | one continuous selection; copy preserves document order | `cross_block_selection_includes_image_atom` |
| EN-IMG-01 | resource node | place caret mid-paragraph and paste an image | paragraph splits into text/image/text; both caret boundaries work immediately | `paste_image_splits_paragraph_once` |
| EN-LIST-01 | transaction commands | select two paragraphs and click bullets, numbered list, checklist | each conversion is immediate and undoable | `list_commands_share_transaction_path` |
| EN-CMD-01 | shared command catalogue | open More, execute a visible command, undo | command runs once and produces one undo entry | `toolbar_and_overflow_execute_same_command` |
| EN-CMD-02 | command state derived from selection | traverse every primary and More command on a text selection | every visible command executes a transaction or is visibly disabled; active/mixed state matches the selection | `all_visible_commands_execute_or_are_disabled` |
| EN-KEY-01 | one document-level input surface | use arrows, Return, Backspace, Delete, Cmd-A, cut, and undo across text/image/list boundaries | no focus island; selection and document order remain valid | `editing_commands_cross_block_boundaries` |
| EN-VIEW-01 | viewport-bounded work | load 10,000 blocks and edit the last visible block | only the viewport window is laid out | `long_document_layout_is_bounded` |
```

- [ ] **Step 2: Write the failing acceptance registry test**

Create `src/native_editor/acceptance.rs`:

```rust
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum AcceptanceCase {
    ImeComposition,
    CrossBlockSelection,
    ImageBoundary,
    ListCommands,
    SharedCommandCatalogue,
    AllVisibleCommands,
    EditingAcrossBlockBoundaries,
    ViewportBoundedLayout,
}

pub const REQUIRED_CASES: [AcceptanceCase; 8] = [
    AcceptanceCase::ImeComposition,
    AcceptanceCase::CrossBlockSelection,
    AcceptanceCase::ImageBoundary,
    AcceptanceCase::ListCommands,
    AcceptanceCase::SharedCommandCatalogue,
    AcceptanceCase::AllVisibleCommands,
    AcceptanceCase::EditingAcrossBlockBoundaries,
    AcceptanceCase::ViewportBoundedLayout,
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn registry_contains_every_documented_gate() {
        assert_eq!(REQUIRED_CASES.len(), 8);
    }
}
```

Declare `mod native_editor;` in `src/main.rs` and `pub mod acceptance;` in `src/native_editor/mod.rs`.

- [ ] **Step 3: Run the registry test**

Run:

```bash
cargo test --manifest-path packages/app-lite-gpui/Cargo.toml registry_contains_every_documented_gate
```

Expected: PASS.

- [ ] **Step 4: Commit the evidence matrix**

```bash
git add docs/research/evernote-editor-behavior-matrix.md packages/app-lite-gpui/src/main.rs packages/app-lite-gpui/src/native_editor
git commit -m "Define Evernote editor acceptance gates"
```

---

### Task 3: Add the compact document model and transaction engine beside Markdown

**Files:**
- Create: `packages/app-lite-gpui/src/native_editor/model.rs`
- Create: `packages/app-lite-gpui/src/native_editor/transaction.rs`
- Create: `packages/app-lite-gpui/src/native_editor/history.rs`
- Modify: `packages/app-lite-gpui/src/native_editor/mod.rs`
- Test: `packages/app-lite-gpui/src/native_editor/tests.rs`

**Interfaces:**
- Consumes: only Rust standard collections, `smallvec`, and `unicode-segmentation`; it must not consume donor Markdown types.
- Produces: `Document`, `NodeId`, `Block`, `BlockKind`, `DocPoint`, `Selection`, `Mark`, `Transaction`, `ApplyOutcome`, and `History`.

- [ ] **Step 1: Write failing tests for structural image insertion, list conversion, and undo**

Create `src/native_editor/tests.rs` with these tests:

```rust
use super::history::History;
use super::model::{BlockKind, DocPoint, Document, Selection};
use super::transaction::Transaction;

#[test]
fn paste_image_splits_paragraph_once() {
    let mut doc = Document::from_paragraph("前面的文字后面的文字");
    let paragraph = doc.first_node_id().unwrap();
    let at = "前面的文字".len();

    doc.apply(Transaction::InsertImage {
        selection: Selection::caret(DocPoint::new(paragraph, at)),
        resource_id: "fixture-image".into(),
        natural_size: (1600, 900),
    }).unwrap();

    assert_eq!(doc.block_kinds(), [BlockKind::Paragraph, BlockKind::Image, BlockKind::Paragraph]);
    assert_eq!(doc.text_at_index(0), Some("前面的文字"));
    assert_eq!(doc.text_at_index(2), Some("后面的文字"));
}

#[test]
fn list_commands_share_transaction_path() {
    let mut doc = Document::from_paragraphs(["一", "二"]);
    let selection = doc.select_all_text();
    let outcome = doc.apply(Transaction::SetBlockKind {
        selection,
        kind: BlockKind::BulletItem { depth: 0 },
    }).unwrap();
    assert_eq!(outcome.changed_nodes.len(), 2);
    assert!(doc.block_kinds().iter().all(|kind| matches!(kind, BlockKind::BulletItem { depth: 0 })));
}

#[test]
fn inverse_transaction_restores_document_and_selection() {
    let mut doc = Document::from_paragraph("abc");
    let original = doc.semantic_snapshot();
    let mut history = History::new(1_000, 16 * 1024 * 1024);
    let end = doc.end_selection();
    history.apply(&mut doc, Transaction::InsertText {
        selection: end,
        text: "中文".into(),
    }).unwrap();
    let restored_selection = history.undo(&mut doc).unwrap();
    assert_eq!(doc.semantic_snapshot(), original);
    assert_eq!(restored_selection, end);
}
```

- [ ] **Step 2: Run the tests to verify missing model failures**

Run:

```bash
cargo test --manifest-path packages/app-lite-gpui/Cargo.toml native_editor::tests
```

Expected: FAIL because `model`, `transaction`, and `history` are not defined.

- [ ] **Step 3: Implement the compact model interfaces**

Add `smallvec = "1.15.2"` under `[dependencies]` in `Cargo.toml`, reusing the version already present in the donor lockfile.

Use these public shapes in `model.rs`:

```rust
use smallvec::SmallVec;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct NodeId(u64);

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum BlockKind {
    Paragraph,
    Heading { level: u8 },
    BulletItem { depth: u8 },
    OrderedItem { depth: u8 },
    CheckItem { depth: u8, checked: bool },
    Quote,
    Code,
    Image,
    Attachment,
    Divider,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TextAlignment { Left, Center, Right }

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Mark { Bold, Italic, Underline, Strike, Highlight, Link(String), InlineCode }

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MarkSpan { pub range: std::ops::Range<usize>, pub mark: Mark }

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum BlockContent {
    Text { text: String, marks: SmallVec<[MarkSpan; 4]> },
    Image { resource_id: String, natural_size: (u32, u32), display_width: Option<u32> },
    Attachment { resource_id: String, filename: String, media_type: String },
    Empty,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Block {
    pub id: NodeId,
    pub kind: BlockKind,
    pub content: BlockContent,
    pub alignment: TextAlignment,
    pub revision: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DocPoint { pub node_id: NodeId, pub utf8_offset: usize }

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Selection { pub anchor: DocPoint, pub head: DocPoint }

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Document { blocks: Vec<Block>, next_id: u64, revision: u64 }
```

Implement all constructors and accessors used by the tests. Keep nodes in a contiguous `Vec<Block>` for the spike; use stable `NodeId` values for external positions.

- [ ] **Step 4: Implement validated transactions and bounded history**

Use these interfaces in `transaction.rs` and `history.rs`:

```rust
pub enum Transaction {
    InsertText { selection: Selection, text: String },
    Delete { selection: Selection },
    SplitBlock { at: DocPoint },
    SetBlockKind { selection: Selection, kind: BlockKind },
    ToggleMark { selection: Selection, mark: Mark },
    SetAlignment { selection: Selection, alignment: TextAlignment },
    InsertImage { selection: Selection, resource_id: String, natural_size: (u32, u32) },
}

pub struct ApplyOutcome {
    pub selection: Selection,
    pub changed_nodes: SmallVec<[NodeId; 4]>,
    pub inverse: TransactionBatch,
    pub estimated_bytes: usize,
}

pub struct TransactionBatch(pub Vec<Transaction>);

pub struct History {
    max_entries: usize,
    max_bytes: usize,
    used_bytes: usize,
    undo: VecDeque<HistoryEntry>,
    redo: VecDeque<HistoryEntry>,
}
```

`Document::apply` must validate node identity, UTF-8 boundary, selection order, heading level, and list depth before mutating. Apply to a cloned set of affected blocks, then replace those blocks only after every operation succeeds.

- [ ] **Step 5: Run focused and donor regressions**

Run:

```bash
cargo test --manifest-path packages/app-lite-gpui/Cargo.toml native_editor::tests
cargo test --manifest-path packages/app-lite-gpui/Cargo.toml --all-targets
```

Expected: new tests PASS and all donor tests remain PASS.

- [ ] **Step 6: Commit the model**

```bash
git add packages/app-lite-gpui/src/native_editor packages/app-lite-gpui/Cargo.toml
git commit -m "Add compact native document transactions"
```

---

### Task 4: Reuse GPUI input and selection code behind one EditorCore

**Files:**
- Create: `packages/app-lite-gpui/src/native_editor/input.rs`
- Create: `packages/app-lite-gpui/src/native_editor/layout.rs`
- Create: `packages/app-lite-gpui/src/native_editor/render.rs`
- Create: `packages/app-lite-gpui/src/native_editor/core.rs`
- Modify: `packages/app-lite-gpui/src/native_editor/mod.rs`
- Modify: `packages/app-lite-gpui/src/native_editor/tests.rs`

**Interfaces:**
- Consumes: Task 3 `Document`, `Selection`, `Transaction`, and `History`; donor range/layout helpers from `components/block/input.rs`, `components/block/element.rs`, and `editor/selection.rs`.
- Produces: `EditorCore`, `LayoutRegistry`, `BlockLayout`, and one `EntityInputHandler` implementation.

- [ ] **Step 1: Add failing UTF-16 IME and cross-block selection tests**

Append to `src/native_editor/tests.rs`:

```rust
#[gpui::test]
fn ime_commit_preserves_utf16_selection(cx: &mut gpui::TestAppContext) {
    let mut editor = EditorCore::for_test("前后", cx);
    editor.set_caret_utf8("前".len());
    editor.replace_and_mark_utf16(None, "中华", Some(0..2)).unwrap();
    assert_eq!(editor.visible_text(), "前中华后");
    assert_eq!(editor.marked_text(), Some("中华"));
    editor.commit_marked_text("中国").unwrap();
    assert_eq!(editor.visible_text(), "前中国后");
    assert_eq!(editor.marked_text(), None);
}

#[gpui::test]
fn cross_block_selection_includes_image_atom(cx: &mut gpui::TestAppContext) {
    let mut editor = EditorCore::fixture_text_image_text("甲", "image", "乙", cx);
    editor.select_document_range(0, editor.document_len());
    assert_eq!(editor.copy_plain_text(), "甲\n\u{fffc}\n乙");
    editor.delete_selection().unwrap();
    assert_eq!(editor.document().block_kinds(), [BlockKind::Paragraph]);
    editor.undo().unwrap();
    assert_eq!(editor.copy_plain_text(), "甲\n\u{fffc}\n乙");
}

#[gpui::test]
fn editing_commands_cross_block_boundaries(cx: &mut gpui::TestAppContext) {
    let mut editor = EditorCore::fixture_text_image_list("甲", "image", "乙", cx);
    editor.command_a();
    assert_eq!(editor.copy_plain_text(), "甲\n\u{fffc}\n乙");
    editor.cut_selection().unwrap();
    assert_eq!(editor.document().block_kinds(), [BlockKind::Paragraph]);
    editor.undo().unwrap();
    editor.move_to_image_after();
    editor.move_left();
    assert!(editor.caret_is_before_image());
    editor.move_right();
    assert!(editor.caret_is_after_image());
    editor.insert_paragraph_break().unwrap();
    editor.undo().unwrap();
    editor.backspace().unwrap();
    editor.undo().unwrap();
    editor.move_to_image_before();
    editor.delete_forward().unwrap();
    editor.undo().unwrap();
    editor.command_a();
    assert_eq!(editor.copy_plain_text(), "甲\n\u{fffc}\n乙");
}
```

- [ ] **Step 2: Run tests to verify EditorCore is absent**

Run:

```bash
cargo test --manifest-path packages/app-lite-gpui/Cargo.toml ime_commit_preserves_utf16_selection
cargo test --manifest-path packages/app-lite-gpui/Cargo.toml cross_block_selection_includes_image_atom
cargo test --manifest-path packages/app-lite-gpui/Cargo.toml editing_commands_cross_block_boundaries
```

Expected: compilation FAIL because `EditorCore` does not exist.

- [ ] **Step 3: Implement a single editor-level input handler**

Define in `core.rs`:

```rust
pub struct EditorCore {
    pub(crate) focus: gpui::FocusHandle,
    document: Document,
    selection: Selection,
    marked: Option<MarkedText>,
    history: History,
    layout: LayoutRegistry,
}

pub struct MarkedText {
    pub node_id: NodeId,
    pub utf8_range: std::ops::Range<usize>,
}
```

Implement `gpui::EntityInputHandler for EditorCore` by adapting, not retyping from memory, the donor methods:

- `text_for_range`
- `selected_text_range`
- `marked_text_range`
- `unmark_text`
- `replace_text_in_range`
- `replace_and_mark_text_in_range`
- `bounds_for_range`
- `character_index_for_point`

Move the donor UTF-16 conversion helpers into `input.rs` with focused tests. All methods must resolve through `Selection` and `LayoutRegistry`; no block gets its own focus handle.

- [ ] **Step 4: Implement one layout registry for all block types**

Use these shapes in `layout.rs`:

```rust
pub struct BlockLayout {
    pub node_id: NodeId,
    pub bounds: gpui::Bounds<gpui::Pixels>,
    pub text_lines: Vec<gpui::ShapedLine>,
    pub before: DocPoint,
    pub after: DocPoint,
}

pub struct LayoutRegistry {
    visible: Vec<BlockLayout>,
    estimated_heights: std::collections::HashMap<NodeId, f32>,
    first_visible: usize,
    last_visible: usize,
}
```

Adapt donor `range_bounds`, `closest_index_for_position`, cross-block endpoint ordering, and viewport culling. `point_to_doc` must return an image's `before` or `after` point based on the pointer x/y half, and text positions for shaped lines.

- [ ] **Step 5: Render selection and caret from the registry**

In `render.rs`, draw in this order: block surfaces, selection rectangles, glyphs/images, caret. The image atom selection uses a rounded outline; its before/after caret uses the same vertical caret geometry as adjacent paragraph text.

- [ ] **Step 6: Run input, selection, and donor tests**

Run:

```bash
cargo test --manifest-path packages/app-lite-gpui/Cargo.toml native_editor::tests
cargo test --manifest-path packages/app-lite-gpui/Cargo.toml --all-targets
```

Expected: all PASS.

- [ ] **Step 7: Commit the unified input surface**

```bash
git add packages/app-lite-gpui/src/native_editor
git commit -m "Add unified GPUI editor input surface"
```

---

### Task 5: Reuse the donor command routing for working Evernote toolbar and lists

**Files:**
- Create: `packages/app-lite-gpui/src/native_editor/commands.rs`
- Create: `packages/app-lite-gpui/src/spike_app.rs`
- Modify: `packages/app-lite-gpui/src/main.rs`
- Modify: `packages/app-lite-gpui/src/native_editor/core.rs`
- Modify: `packages/app-lite-gpui/src/native_editor/tests.rs`

**Interfaces:**
- Consumes: `EditorCore::apply(Transaction)`, `EditorCore::undo`, `EditorCore::redo`, and donor keybinding/action patterns.
- Produces: `EditorCommand`, `CommandDescriptor`, `CommandState`, and a visible `--evernote-spike` window.

- [ ] **Step 1: Add a failing shared-command test**

Append:

```rust
#[gpui::test]
fn toolbar_and_overflow_execute_same_command(cx: &mut gpui::TestAppContext) {
    let mut editor = EditorCore::for_test("第一行\n第二行", cx);
    editor.select_all();
    let catalogue = CommandCatalogue::default();
    catalogue.execute(EditorCommand::BulletList, &mut editor).unwrap();
    assert!(editor.document().block_kinds().iter().all(|kind| matches!(kind, BlockKind::BulletItem { .. })));
    assert_eq!(editor.undo_depth(), 1);
    catalogue.execute(EditorCommand::Undo, &mut editor).unwrap();
    assert_eq!(editor.document().block_kinds(), [BlockKind::Paragraph, BlockKind::Paragraph]);
}
```

Add `all_visible_commands_execute_or_are_disabled` as a table-driven GPUI test. Construct a fresh two-paragraph editor for each descriptor. Assert descriptor keys are unique; enabled formatting, paragraph/list, link, and non-current alignment commands change the document through exactly one history entry; current-state no-op commands report `On`; Undo/Redo report disabled until history permits them. No descriptor may report enabled and then return an unimplemented/no-op result.

- [ ] **Step 2: Run the test to verify commands are absent**

Run:

```bash
cargo test --manifest-path packages/app-lite-gpui/Cargo.toml toolbar_and_overflow_execute_same_command
```

Expected: compilation FAIL because command types do not exist.

- [ ] **Step 3: Implement the shared command catalogue**

Define:

```rust
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum EditorCommand {
    Undo, Redo, Paragraph, Heading1, Heading2, Heading3,
    Bold, Italic, Underline, Strike, Highlight,
    BulletList, OrderedList, CheckList, Link,
    AlignLeft, AlignCenter, AlignRight,
}

pub enum ToggleState { Off, On, Mixed }

pub struct CommandState { pub enabled: bool, pub toggle: ToggleState }

pub struct CommandDescriptor {
    pub command: EditorCommand,
    pub label: &'static str,
    pub group: u8,
    pub primary: bool,
}
```

Both primary toolbar and More menu iterate the same descriptor slice. `execute` translates commands into Task 3 transactions, including `SetAlignment`; list conversions across multiple blocks are one history entry. Toolbar pointer-down preserves the editor selection and focus, then returns focus after execution.

- [ ] **Step 4: Add a deterministic spike launch mode**

Add `--evernote-spike` parsing in `main.rs`. When set, `spike_app::open` must create one window containing the centered 680 pt editor, Evernote-order toolbar, sample text/list/image blocks, and no workspace/update/export UI. The ordinary donor launch remains intact for comparison.

- [ ] **Step 5: Run tests and launch the spike**

Run:

```bash
cargo test --manifest-path packages/app-lite-gpui/Cargo.toml native_editor::tests
cargo run --manifest-path packages/app-lite-gpui/Cargo.toml --release -- --evernote-spike
```

Expected: all tests PASS; the window opens with working paragraph style, undo/redo, inline marks, three list buttons, link, alignment, and More commands.

- [ ] **Step 6: Commit working commands**

```bash
git add packages/app-lite-gpui/src
git commit -m "Add working Evernote editor commands"
```

---

### Task 6: Reuse donor image intake and enforce immediate image-boundary editing

**Files:**
- Create: `packages/app-lite-gpui/src/native_editor/images.rs`
- Modify: `packages/app-lite-gpui/src/native_editor/core.rs`
- Modify: `packages/app-lite-gpui/src/native_editor/layout.rs`
- Modify: `packages/app-lite-gpui/src/native_editor/render.rs`
- Modify: `packages/app-lite-gpui/src/spike_app.rs`
- Modify: `packages/app-lite-gpui/src/native_editor/tests.rs`

**Interfaces:**
- Consumes: donor paste/drop file extraction and image decoding; `Transaction::InsertImage`.
- Produces: `ImageStore`, `ImageMetadata`, `TextureCache`, and the `EN-IMG-01` behavior.

- [ ] **Step 1: Add failing image layout-stability tests**

Append:

```rust
#[test]
fn image_placeholder_and_texture_have_identical_layout_height() {
    let metadata = ImageMetadata::new("fixture", 1600, 900);
    let width = 680.0;
    assert_eq!(metadata.display_height(width), 382.5);
    assert_eq!(metadata.placeholder_height(width), metadata.display_height(width));
}

#[test]
fn image_cache_evicts_before_exceeding_budget() {
    let mut cache = TextureCache::new(48 * 1024 * 1024);
    cache.insert_for_test("a", 32 * 1024 * 1024);
    cache.insert_for_test("b", 32 * 1024 * 1024);
    assert!(cache.used_bytes() <= cache.budget_bytes());
    assert!(!cache.contains("a"));
    assert!(cache.contains("b"));
}
```

- [ ] **Step 2: Run tests to verify image services are absent**

Run:

```bash
cargo test --manifest-path packages/app-lite-gpui/Cargo.toml image_placeholder_and_texture_have_identical_layout_height
cargo test --manifest-path packages/app-lite-gpui/Cargo.toml image_cache_evicts_before_exceeding_budget
```

Expected: compilation FAIL.

- [ ] **Step 3: Adapt the donor image code**

Reuse actual decoding/file-drop code from `components/markdown/image.rs`, `components/block/runtime/image.rs`, and `editor/file_drop.rs`. The new path reads dimensions first, commits `InsertImage` immediately, then decodes a viewport-sized texture asynchronously. Do not keep compressed bytes, CPU bitmap, and full GPU texture simultaneously after upload.

- [ ] **Step 4: Implement byte-budgeted cache accounting**

Define:

```rust
pub struct ImageMetadata {
    pub resource_id: String,
    pub natural_width: u32,
    pub natural_height: u32,
}

pub struct BudgetedImageCache {
    textures: TextureCache,
}

pub struct TextureCache {
    budget_bytes: usize,
    used_bytes: usize,
    entries: std::collections::HashMap<u64, CachedTexture>,
    lru: std::collections::VecDeque<u64>,
}

pub struct CachedTexture {
    item: Option<gpui::ImageCacheItem>,
    decoded_bytes: usize,
}
```

`TextureCache` owns deterministic byte accounting and LRU policy; `insert_for_test` installs an entry with `item: None` so the budget policy is testable without a GPUI window. Adapt GPUI 0.2.2's real `RetainAllImageCache` implementation rather than inventing a parallel renderer path: implement `gpui::ImageCache` for `BudgetedImageCache`, provide it as an `Entity<BudgetedImageCache>`, and wrap the editor subtree with `gpui::image_cache(...)`. Use GPUI's `ImageAssetLoader`, `ImageCacheItem`, `RenderImage`, `hash`, and `App::drop_image` lifecycle. When a load becomes `Loaded`, compute decoded bytes as the checked sum of `width * height * 4` across `RenderImage::frame_count()`; update LRU order on access. Eviction removes least-recently-used offscreen entries and calls `drop_image` for loaded images until the insertion fits. A single oversized image is downsampled before `RenderImage` creation rather than exempted. Keep compressed resource bytes only in `ImageStore`; the cache owns decoded/render images.

- [ ] **Step 5: Run image, editor, and donor tests**

Run:

```bash
cargo test --manifest-path packages/app-lite-gpui/Cargo.toml native_editor::tests
cargo test --manifest-path packages/app-lite-gpui/Cargo.toml --all-targets
```

Expected: all PASS.

- [ ] **Step 6: Manually execute EN-IMG-01**

Run the Release spike. Type Chinese before the caret, paste a Finder/Preview image mid-paragraph, immediately type before and after it, select across it, delete, and undo. Expected: image appears without switching documents, no text disappears, no layout flash occurs, and undo restores text/image/text in one step.

- [ ] **Step 7: Commit image behavior**

```bash
git add packages/app-lite-gpui/src/native_editor packages/app-lite-gpui/src/spike_app.rs packages/app-lite-gpui/Cargo.toml
git commit -m "Add stable native image block editing"
```

---

### Task 7: Measure memory and decide whether the GPUI spike is promotable

**Files:**
- Create: `packages/app-lite-gpui/scripts/measure-memory.sh`
- Create: `packages/app-lite-gpui/src/native_editor/fixtures.rs`
- Modify: `packages/app-lite-gpui/src/main.rs`
- Modify: `docs/research/evernote-editor-behavior-matrix.md`

**Interfaces:**
- Consumes: `--evernote-spike`, complete acceptance cases, and deterministic empty/typical/long fixtures.
- Produces: machine-readable memory results and an explicit promote/stop decision.

- [ ] **Step 1: Add deterministic fixture modes**

Support:

```text
--evernote-spike --fixture empty
--evernote-spike --fixture typical
--evernote-spike --fixture long
--ready-file /absolute/path
```

`empty` contains one empty paragraph. `typical` contains 200 text/list blocks and ten distinct 1600×900 generated image textures. `long` contains 10,000 text blocks. Write the ready file only after the first complete frame and texture queue settle.

- [ ] **Step 2: Create the memory measurement script**

Create `scripts/measure-memory.sh`:

```bash
#!/usr/bin/env bash
set -euo pipefail

binary=${1:?release binary path required}
fixture=${2:?fixture required}
ready_file=$(mktemp /tmp/app-lite-gpui-ready.XXXXXX)
rm -f "$ready_file"

"$binary" --evernote-spike --fixture "$fixture" --ready-file "$ready_file" &
app_pid=$!
trap 'kill "$app_pid" 2>/dev/null || true; rm -f "$ready_file"' EXIT

for _ in $(seq 1 300); do
  [[ -f "$ready_file" ]] && break
  sleep 0.1
done
[[ -f "$ready_file" ]]
sleep 30

rss_kib=$(ps -o rss= -p "$app_pid" | tr -d ' ')
child_count=$(ps -axo ppid= | awk -v pid="$app_pid" '$1 == pid { count++ } END { print count + 0 }')
webkit_linked=$(otool -L "$binary" | awk '/WebKit\.framework/ { found=1 } END { print found + 0 }')
printf '{"fixture":"%s","pid":%s,"rss_kib":%s,"child_processes":%s,"webkit_linked":%s}\n' \
  "$fixture" "$app_pid" "$rss_kib" "$child_count" "$webkit_linked"
```

- [ ] **Step 3: Build and measure all fixtures**

Run:

```bash
cargo build --manifest-path packages/app-lite-gpui/Cargo.toml --release
packages/app-lite-gpui/scripts/measure-memory.sh packages/app-lite-gpui/target/release/velotype empty
packages/app-lite-gpui/scripts/measure-memory.sh packages/app-lite-gpui/target/release/velotype typical
packages/app-lite-gpui/scripts/measure-memory.sh packages/app-lite-gpui/target/release/velotype long
```

Expected gates:

- `child_processes` equals `0` for every fixture;
- `webkit_linked` equals `0` for every fixture;
- `empty.rss_kib <= 81920`;
- `typical.rss_kib <= 122880`;
- `long.rss_kib - empty.rss_kib <= 40960`.

- [ ] **Step 4: Complete the behavior matrix**

For every matrix row, add the implementing commit, automated test result, manual result, and measured memory record. A failed row remains `FAIL`; do not change the requirement or mark it complete based only on compilation.

- [ ] **Step 5: Run the complete release gate**

Run:

```bash
cargo fmt --manifest-path packages/app-lite-gpui/Cargo.toml -- --check
cargo clippy --manifest-path packages/app-lite-gpui/Cargo.toml --all-targets -- -D warnings
cargo test --manifest-path packages/app-lite-gpui/Cargo.toml --all-targets
git diff --check
```

Expected: all PASS.

- [ ] **Step 6: Commit measured spike results**

```bash
git add packages/app-lite-gpui docs/research/evernote-editor-behavior-matrix.md
git commit -m "Verify GPUI Evernote editor spike"
```

The spike is promotable only if all six Evernote behavior rows pass and all memory/process gates pass. Promotion into the permanent application is a separate implementation plan; the old AppKit editor remains untouched until that plan is approved.
