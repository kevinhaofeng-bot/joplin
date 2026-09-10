# Task 1 report — Evernote-style GPUI editor surface

Status: DONE_WITH_CONCERNS

## Implementation

- Added `native_editor::chrome`: fixed writing-column metrics, responsive deterministic toolbar placement and a standalone GPUI `TitleInput` with checked UTF-16 boundaries.
- Extended the shared catalogue with `InsertImage`, typed `ImagePath`, Chinese labels, icon descriptors and atomic argument validation. The command calls `EditorCore::insert_image_path`.
- Replaced the spike shell with a 1200x820 warm-neutral/white-page hierarchy, breadcrumb, title, metadata, one-row icon toolbar, narrow More fallback, labelled link field and native single-file image picker.
- Added owned 20x20 SVG assets and embedded them via the real `VelotypeAssets` source.

## RED to GREEN evidence

1. `cargo test --manifest-path packages/app-lite-gpui/Cargo.toml --bin velotype chrome -- --nocapture` initially failed on missing `editor_chrome_metrics`, `toolbar_placement`, and `InsertImage`; after implementation, 18 matching tests pass.
2. `cargo test --manifest-path packages/app-lite-gpui/Cargo.toml --bin velotype insert_image_command -- --nocapture` initially failed on missing `InsertImage`/`ImagePath`; after implementation, both real path-insertion/undo and atomic-error tests pass.
3. `cargo test --manifest-path packages/app-lite-gpui/Cargo.toml --bin velotype title_input -- --nocapture` initially failed on missing `TitleInput`; after implementation, the CJK/emoji replacement, marked text, and surrogate-split rejection tests pass.

## Controller gates

- `chrome`: PASS, 18 tests.
- `shell_`: PASS, 12 tests.
- full bin suite with the specified one skip: PASS, 982 passed / 0 failed / 1 skipped.
- `--all-targets --no-run`: PASS.
- `cargo fmt --check`: PASS.
- `git diff --check`: PASS.
- `cargo build --manifest-path packages/app-lite-gpui/Cargo.toml --release`: PASS (optimized build completed in 2m09s).

## Real-window evidence

Release-window acceptance is PENDING: the final binary was launched with `--evernote-spike`, but this desktop session does not expose the standalone GPUI process to its accessibility automation surface, so no screenshot or interaction is represented as PASS. The behavior matrix has been updated accordingly.

## Self-review

- No Web runtime, copied Evernote asset, second body editor, or Task 7 drawable/memory threshold change was added.
- Image paste and drag/drop continue through their prior `EditorCore`/image-store paths; toolbar image selection only adds the typed native picker seam.
- More and the primary row resolve descriptors through the same catalogue. Invalid links now stay open with a visible error as required.

## Known concerns

- Real macOS Pinyin, native picker cancellation, drag/drop, and visual screenshot acceptance remain PENDING until the running Release build completes and the window can be automated.
- The repository's pre-existing warnings remain; no claim is made for `clippy -D warnings`.

## Round 1 review repairs

Status: DONE_WITH_CONCERNS

### Changes made

- Replaced the Link popover's inert `取消`/`应用` text with separately mounted GPUI hit targets (`evernote-link-cancel` and `evernote-link-apply`). Both route into the existing cancel/submit methods; invalid Apply retains the panel/error state, while valid Apply creates the actual catalogue transaction and restores body focus.
- Calculated toolbar placement once from `chrome.header_width` and passed that same placement into More. More now uses the shared overflow set and, when its trigger is near the right edge, clamps to `content_mask.right - 220`; it only becomes narrower when the mask itself is under 220pt.
- Completed the real title input path: shaped-line pointer placement/drag selection, grapheme-safe Left/Right/Home/End/Delete/Backspace, Cmd-A/C/X/V, 30pt semibold shaping, and Return/Down body-focus transfer. UTF-16 IME bridge validation remains checked at its platform boundary.
- Kept title and body on the same narrow writing-column left edge, changed the obsolete “wrapped toolbar” assertion to real one-row primary/More flex alignment, and rendered one-pixel separators only across occupied toolbar groups with neutral hover treatment.
- Split picker focus correctly through the prompt's shared completion seam: `Cancelled` is an explicit no-op that leaves prior focus alone; a real selected image transaction restores body focus through the active window handle.

### Round 1 RED to GREEN evidence

1. RED: `cargo test native_editor::chrome::tests::title_input_edits_by_grapheme_without_splitting_utf8_or_utf16 -- --exact` from `packages/app-lite-gpui` failed to compile because `TitleInput` had no `move_to_edge`, `delete_backward`, `move_horizontal`, `selected_text`, `delete_forward`, or `select_all`. GREEN: the same test passed, proving emoji and combining-grapheme edits do not split UTF-8/UTF-16 boundaries.
2. RED seam evidence: `cargo test shell_visible_link_buttons_keep_invalid_open_and_dispatch_cancel_or_apply -- --nocapture` initially failed at `Apply must be a real hit target`; after adding the mounted buttons it passed, including invalid retention, actual button cancellation, valid apply, and body focus.
3. GREEN shell seams: `cargo test --manifest-path Cargo.toml --bin velotype shell_ -- --nocapture` passed 16 tests, including 760pt primary+More membership, title click/edit/Return focus, title/body left alignment, picker focus divergence, More bounds, and live-scroll membership.

### Round 1 controller gates

- `cargo test --manifest-path Cargo.toml --bin velotype chrome -- --nocapture`: PASS, 19 tests.
- `cargo test --manifest-path Cargo.toml --bin velotype shell_ -- --nocapture`: PASS, 16 tests.
- `cargo test --manifest-path Cargo.toml --bin velotype -- --skip editor::selection::tests::cross_block_cut_writes_markdown_deletes_range_and_undo_restores`: PASS, 987 passed / 0 failed / 1 filtered out.
- `cargo test --manifest-path Cargo.toml --all-targets --no-run`: PASS.
- `cargo fmt --check --manifest-path Cargo.toml`: PASS.
- `git diff --check`: PASS.
- `cargo build --manifest-path Cargo.toml --release`: PASS (optimized release target; final LTO link completed in 2m14s).

### Round 1 real-window evidence

- Launched the exact release binary as `target/release/velotype --evernote-spike`; its foreground process remained live until explicitly stopped after the evidence attempt.
- The desktop automation surface still did not enumerate a `velotype` app. A direct automation lookup returned `Invalid app: velotype`; therefore no screenshot or native interaction has been marked PASS. These real-window items remain PENDING, not inferred from shell tests.

### Round 1 self-review

- Verified the Round 1 diff changes only `spike_app.rs` and `native_editor/chrome.rs`; Task 7 fixture counts, memory thresholds, drawable-pool logic, image paste/drop paths, and measurement code were not changed.
- More and primary resolve their command descriptors through the same catalogue placement; Link buttons call the production submit/cancel methods rather than test helpers. Picker selection calls the same typed `InsertImage` catalogue transaction as the prompt.
