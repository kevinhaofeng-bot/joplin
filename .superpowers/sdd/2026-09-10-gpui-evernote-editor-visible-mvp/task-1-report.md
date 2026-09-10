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
