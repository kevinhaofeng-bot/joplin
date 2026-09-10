# GPUI Evernote Editor Visible MVP Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Replace the GPUI editor's developer-spike chrome with a visually credible, immediately usable Evernote-style note editor while preserving the approved native transaction, input, image, layout, and memory architecture.

**Architecture:** Keep `EditorCore` as the sole body editor, selection owner, and transaction entry. Add a small pure-Rust chrome/presentation module for visual tokens, responsive toolbar placement, and the separately focused note-title field; the GPUI shell renders that state and continues to route every body command through the existing shared `CommandCatalogue`.

**Tech Stack:** Rust 2024, GPUI 0.2.2, Velotype donor assets/render primitives, `raw-window-handle` AppKit bridge already in use, native macOS path prompt, Metal rendering.

**Spec:** `docs/superpowers/specs/2026-09-09-evernote-native-editor-core-design.md`

## Global Constraints

- The current `c33f6dced` GPUI editor transaction, selection, IME, image, history, layout-cache, decoded-image-cache, two-drawable, and precompiled-shader paths are the stable baseline; visual work must layer on them.
- Evernote 11.32.5 is the visual and behavioral reference; use `docs/research/evernote-11.32.5-targeted-reverse.md` and the user-supplied/current local screenshots as evidence.
- No AppKit text editor, WKWebView, WebKit, JavaScriptCore, Electron, Node, helper process, Markdown fact source, or per-block input widget.
- Keep the body at a centered maximum width of 680 pt and bottom scroll padding at 30% of the viewport.
- The default window is 1200×820 pt. The editor header may use up to 1060 pt, but title and body share the same centered 680-pt left edge.
- The toolbar is one row. It never wraps into two rows; commands that do not fit move into the shared More menu.
- Every visible control either executes its real production command or is visibly disabled. No decorative dead button is permitted.
- Use new project-owned monochrome SVG toolbar glyphs. Do not reuse Evernote application assets.
- Evernote green `#00A82D` is the only saturated accent; text, borders, disabled states, and backgrounds remain neutral.
- Preserve all `--evernote-spike` measurement arguments, deterministic fixtures, ready/diagnostics publication, and existing tests. Do not modify memory thresholds or hide/retry/sleep the window to manufacture a pass.
- Task 7 remains STOP/not promotable because its one-shot long-fixture physical-footprint gate is timing-dependent; this plan improves the experimental MVP and creates no release tag.
- Product code is implemented by the delegated Terra agent because Luna was ineffective; root owns architecture, review, real-window verification, and any push/tag decision.

## File Structure

- `packages/app-lite-gpui/src/native_editor/chrome.rs`: pure visual tokens, title-edit state, responsive toolbar placement, and command presentation metadata.
- `packages/app-lite-gpui/src/native_editor/commands.rs`: existing command catalogue plus the image-insert command and typed path argument.
- `packages/app-lite-gpui/src/native_editor/mod.rs`: exports the new chrome module.
- `packages/app-lite-gpui/src/spike_app.rs`: production GPUI title/header/toolbar/popover/path-prompt rendering and focus routing.
- `packages/app-lite-gpui/src/main.rs`: embeds the new project-owned SVG assets.
- `packages/app-lite-gpui/assets/icon/editor/*.svg`: monochrome 20×20 toolbar icons.
- `docs/research/evernote-editor-behavior-matrix.md`: records the implementing commit and real Release result only after verification.

---

### Task 1: Build the Evernote-style GPUI editor surface

**Files:**
- Create: `packages/app-lite-gpui/src/native_editor/chrome.rs`
- Create: `packages/app-lite-gpui/assets/icon/editor/image.svg`
- Create: `packages/app-lite-gpui/assets/icon/editor/undo.svg`
- Create: `packages/app-lite-gpui/assets/icon/editor/redo.svg`
- Create: `packages/app-lite-gpui/assets/icon/editor/bold.svg`
- Create: `packages/app-lite-gpui/assets/icon/editor/italic.svg`
- Create: `packages/app-lite-gpui/assets/icon/editor/underline.svg`
- Create: `packages/app-lite-gpui/assets/icon/editor/highlight.svg`
- Create: `packages/app-lite-gpui/assets/icon/editor/bulleted-list.svg`
- Create: `packages/app-lite-gpui/assets/icon/editor/ordered-list.svg`
- Create: `packages/app-lite-gpui/assets/icon/editor/checklist.svg`
- Create: `packages/app-lite-gpui/assets/icon/editor/link.svg`
- Create: `packages/app-lite-gpui/assets/icon/editor/align-left.svg`
- Create: `packages/app-lite-gpui/assets/icon/editor/align-center.svg`
- Create: `packages/app-lite-gpui/assets/icon/editor/align-right.svg`
- Create: `packages/app-lite-gpui/assets/icon/editor/indent.svg`
- Create: `packages/app-lite-gpui/assets/icon/editor/outdent.svg`
- Create: `packages/app-lite-gpui/assets/icon/editor/strike.svg`
- Modify: `packages/app-lite-gpui/src/native_editor/commands.rs`
- Modify: `packages/app-lite-gpui/src/native_editor/mod.rs`
- Modify: `packages/app-lite-gpui/src/main.rs`
- Modify: `packages/app-lite-gpui/src/spike_app.rs`
- Modify: `docs/research/evernote-editor-behavior-matrix.md`
- Test: inline tests in `chrome.rs`, `commands.rs`, and `spike_app.rs`

**Interfaces:**
- Consumes: existing `EditorCore`, `CommandCatalogue`, `CommandState`, `EditorCommand`, `ElementInputHandler`, `layout_for_viewport`, `ImageStore`, `BudgetedImageCache`, and native `PathPromptOptions`.
- Produces:

```rust
pub const EVERNOTE_GREEN: u32 = 0x00a82dff;
pub const NOTE_BODY_MAX_WIDTH: f32 = 680.0;
pub const EDITOR_HEADER_MAX_WIDTH: f32 = 1060.0;

#[derive(Clone, Debug, PartialEq)]
pub struct EditorChromeMetrics {
    pub header_width: f32,
    pub body_width: f32,
    pub body_left_in_header: f32,
    pub toolbar_height: f32,
    pub bottom_padding: f32,
}

pub fn editor_chrome_metrics(viewport_width: f32, viewport_height: f32) -> EditorChromeMetrics;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ToolbarPlacement {
    pub primary: Vec<EditorCommand>,
    pub overflow: Vec<EditorCommand>,
}

pub fn toolbar_placement(available_width: f32) -> ToolbarPlacement;

pub struct TitleInput {
    text: String,
    selection: std::ops::Range<usize>,
    marked_range: Option<std::ops::Range<usize>>,
    focus: gpui::FocusHandle,
}
```

`TitleInput` implements `EntityInputHandler`. Its internal offsets are UTF-8; platform ranges are converted with the existing checked UTF-16 helpers. Return or Down focuses the body. It is a separate note-title field and never becomes the first body block.

Extend the command boundary exactly as follows:

```rust
pub enum EditorCommand {
    InsertImage,
    // existing variants unchanged
}

pub enum CommandArgument {
    None,
    LinkUrl(String),
    ImagePath(std::path::PathBuf),
}

pub struct CommandDescriptor {
    pub command: EditorCommand,
    pub label: &'static str,
    pub label_zh: &'static str,
    pub icon_path: Option<&'static str>,
    pub group: u8,
    pub primary: bool,
}
```

`InsertImage` is enabled for an editable body selection and accepts only `ImagePath`; it calls the existing `EditorCore::insert_image_path`. Missing, unsupported, or mismatched input returns a structured error without changing the document or history. The toolbar opens one native single-file path prompt and forwards the selected path through this command. Paste and drag/drop remain on their existing production paths.

- [ ] **Step 1: Add RED chrome geometry and placement tests.** Add literal tests proving: 1200×820 produces a 1060-pt header, 680-pt body, 190-pt body offset within the header, a 44-pt toolbar, and 246-pt bottom padding; a 760-pt viewport produces 696-pt header, 680-pt body, and 8-pt body offset. Add a table-driven placement test proving the wide row contains `InsertImage, Undo, Redo, Paragraph, Bold, Italic, Underline, Highlight, BulletList, OrderedList, CheckList, Link, AlignLeft, AlignCenter, AlignRight` in that order, while a 696-pt row moves Link and all three alignments into overflow and still never duplicates or omits a command.

- [ ] **Step 2: Run the focused tests and capture RED.** Run `cargo test --manifest-path packages/app-lite-gpui/Cargo.toml --bin velotype chrome -- --nocapture` and the new command tests. The failures must be caused by missing chrome types/placement and missing `InsertImage`, not syntax or fixture errors. Record the command, expected break, and output in the task report.

- [ ] **Step 3: Implement the pure chrome module.** Implement the constants, literal geometry, and deterministic responsive placement. Use a width-cost table local to `chrome.rs`; preserve the structural controls and move lower-priority link/alignment commands into overflow before reducing body width. Never inspect rendered text to guess placement and never use `flex_wrap`.

- [ ] **Step 4: Add title-input RED tests, then implement the real input bridge.** Before production code, add tests for Chinese text, emoji, replacement of a selected UTF-16 range, marked-text commit/cancel, Return-to-body focus intent, and a surrogate-splitting rejection that leaves title and selection unchanged. Reuse `native_editor::input` conversions. Implement `TitleInput` as one GPUI input entity; do not use AppKit controls or a second body editor.

- [ ] **Step 5: Add image-command RED tests, then implement the typed path command.** Mutation target: replacing `InsertImage` execution with `Ok(())` must fail because document order, selection, and undo depth do not change. Test valid PNG/JPEG insertion between text, unsupported extension, argument mismatch, and undo restoration through the actual catalogue. Add the native one-file prompt in `spike_app.rs`; cancellation is a no-op that preserves focus and selection.

- [ ] **Step 6: Add and embed the project-owned SVG glyphs.** Each icon uses a 20×20 `viewBox`, `currentColor`, round joins/caps, and no embedded fonts, raster payload, brand name, or copied path data. Extend `VelotypeAssets::load` with the exact `icon/editor/*.svg` paths. Add an asset-load test that requests every descriptor icon through the real asset source and rejects missing or empty bytes.

- [ ] **Step 7: Replace the developer-spike chrome.** Default to a centered 1200×820 window. Render a warm neutral workspace, a white editor page, breadcrumb `本地资料库  ›  笔记`, editable 30-pt semibold title initialized to `会议记录`, metadata `更新 刚刚`, the 44-pt toolbar, and the existing body canvas. The header and toolbar use `EDITOR_HEADER_MAX_WIDTH`; title and body share the centered `NOTE_BODY_MAX_WIDTH` left edge. Remove visible strings `Evernote editor spike`, `Undo`, `Redo`, `Paragraph`, and other English button labels from the rendered surface.

- [ ] **Step 8: Render one-row icon controls from the shared catalogue.** Icons are 20 pt inside 32×32 hit targets; the paragraph control is the text button `正文`, and the overflow trigger is `更多`. Insert one-pixel separators only between nonempty groups. Disabled controls remain visible at 35% opacity; active and mixed state use `EVERNOTE_GREEN` without introducing blue/yellow toggle fills. Hover uses a neutral surface. Pointer-down preserves the body selection; after a body command, focus returns to the body.

- [ ] **Step 9: Rebuild the More and Link panels.** More is a single-column 220-pt-wide, at-most-300-pt-high scrollable menu anchored to the measured trigger and edge-flipped within the content mask. It uses the same descriptors and command execution path as the primary row. Link is a labelled popover with URL field plus `应用` and `取消`; invalid URL keeps the panel open and exposes a visible error line. Opening/closing a panel does not consume history, and clicking a command consumes exactly one history entry.

- [ ] **Step 10: Add production-shell regression tests.** Through `VisualTestContext`, assert the wide and narrow toolbar membership, exact one-row toolbar height, title/body left-edge alignment, rendered Chinese labels, title-to-body focus transfer, primary/overflow command equivalence, image prompt seam, popover edge containment, disabled state, and no English debug labels. Tests must assert mounted element bounds and real command/history effects, not source strings or test-only mocks.

- [ ] **Step 11: Run controller-reproducible gates.** Run:

```bash
cargo test --manifest-path packages/app-lite-gpui/Cargo.toml --bin velotype chrome -- --nocapture
cargo test --manifest-path packages/app-lite-gpui/Cargo.toml --bin velotype shell_ -- --nocapture
cargo test --manifest-path packages/app-lite-gpui/Cargo.toml --bin velotype -- --skip editor::selection::tests::cross_block_cut_writes_markdown_deletes_range_and_undo_restores
cargo test --manifest-path packages/app-lite-gpui/Cargo.toml --all-targets --no-run
cargo fmt --check --manifest-path packages/app-lite-gpui/Cargo.toml
git diff --check
cargo build --manifest-path packages/app-lite-gpui/Cargo.toml --release
```

The known project-wide `clippy -D warnings` baseline remains separately recorded; do not claim it passes unless this task actually removes every warning.

- [ ] **Step 12: Perform real-window acceptance and record evidence.** Launch the exact final Release binary with `--evernote-spike`; capture only its window. Verify title editing with macOS Pinyin, body editing, list conversion, image button insertion, paste, drag/drop, before/after-image typing, More, link, undo/redo, and narrow-window overflow. Compare against the local Evernote 11.32.5 editor hierarchy. Update the behavior matrix only with observed results; any failing sequence remains `FAIL` or `PENDING`, never inferred PASS.

- [ ] **Step 13: Self-review and commit.** Confirm the diff contains no Web runtime, copied Evernote asset, dead control, second body input surface, threshold change, or hidden measurement workaround. Commit product and research files as `Build Evernote style GPUI editor surface`. Do not tag or push; root decides after independent review and verification.
