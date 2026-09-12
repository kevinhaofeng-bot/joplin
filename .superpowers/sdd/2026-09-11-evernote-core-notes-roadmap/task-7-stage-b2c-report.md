# Task 7 Stage B2c report

Status: bounded corrective implementation only. This report does **not** claim
that B2 is accepted.

## Changes

- The async history and active SearchRoute fences now include `(NoteId,
  save_generation, expected_revision)`. `expected_revision` advances only on a
  durable note/resource commit, catching Dirty-to-Clean completion that keeps
  the same save generation.
- Search rows are direct children of the tracked GPUI scroll container;
  Up/Down uses `ScrollHandle::scroll_to_item`, hence uses measured child bounds
  rather than a made-up row height. The mounted regression exercises 500 real,
  uniquely-indexed notes with wrapped title/snippet, confirms the tail painted
  bounds intersect the viewport, then clicks that tail row.
- Palette opening captures `Window::focused(&App)` and preserves whether the
  organization panel was open; closing restores both the panel and exact handle.
- Search refresh errors from queue/FTS and `commit_search_refresh` are distinct
  search notices with a visible Retry control. Stale `Ok(false)` only schedules
  the latest pending SearchRoute request; it does not paint an error after the
  route has changed. History failures remember their Back/Forward direction so
  Retry reissues the matching history lookup.
- Ran `cargo fmt`, including the prior core formatting failure at
  `packages/app-lite-core/src/repository.rs:2807`.

## Evernote source -> Rust -> test

| Source evidence | Rust path | Verification |
| --- | --- | --- |
| `renderer-readable/chunks/9435.js` search overlay behavior; source brief line 40 | `packages/app-lite-gpui/src/ui/mod.rs` palette rows/focus/Retry | `ui::tests::search_palette_keyboard_reveals_a_wrapping_tail_row_in_the_real_scroll_viewport`; `ui::tests::search_palette_escape_restores_an_open_organization_input_and_its_panel` |
| `main-readable/src/modules/36175__module-36175.js` plus `41774` durable FTS queue; source brief line 34 | `packages/app-lite-gpui/src/ui/mod.rs` durable-revision fence; `packages/app-lite-core/src/repository.rs` queue query | core suite and GPUI suite listed below |
| `83028__module-83028.js::searchNote`; source brief lines 35--36 | retained bounded `SearchHit`/`AppModel::projections` packet, no foreground fallback | 500 real `NoteId` mounted regression and full GPUI suite |

## Commands and results

- `cargo fmt --manifest-path packages/app-lite-gpui/Cargo.toml` — success.
- `cargo fmt --manifest-path packages/app-lite-core/Cargo.toml` — success.
- `cargo test --manifest-path packages/app-lite-core/Cargo.toml --features test-support` — success.
- `cargo test --manifest-path packages/app-lite-gpui/Cargo.toml --bin velotype -- --skip cross_block_cut_writes_markdown_deletes_range_and_undo_restores` — `1292 passed; 0 failed; 1 filtered out` (before the final Retry stale-route tightening).
- `cargo test --manifest-path packages/app-lite-gpui/Cargo.toml --bin velotype 'ui::tests::search_palette_' -- --nocapture` — `3 passed; 0 failed`; this is fresh after the final Retry stale-route tightening.
- `cargo check --manifest-path packages/app-lite-gpui/Cargo.toml` — success after final tightening.
- `cargo build --manifest-path packages/app-lite-gpui/Cargo.toml --release` — final rebuild in progress at report write time.

## Limits / remaining evidence

- No personal library was opened or accessed.
- No macOS physical IME verification was performed; the existing real marked-text mounted test remains automated coverage only.
- Disposable-profile Release smoke must use the final rebuilt binary under the Cargo metadata target directory, record its SHA, and verify normal exit. It was not claimed complete here.
- The same-generation Dirty-to-Clean fence is implemented at the controller boundary; the dedicated mounted interleaving regression remains a required independent-review target.

Commit hash: `2f20771bdcf3379ae23818b265acc38bc3eac1c6` (the report amendment that records the final targeted result is pending).
