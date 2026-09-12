# Task 7 Stage B2c report

Status: bounded corrective implementation only. This report does **not** claim
that B2 is accepted.

## Changes

- The async history and active SearchRoute fences now include `(NoteId,
  save_generation, expected_revision)`. `expected_revision` advances only on a
  durable note/resource commit, catching Dirty-to-Clean completion that keeps
  the same save generation.
- A per-`LibraryShell`, one-shot test gate holds only after the ordinary
  bounded packet read and before its foreground continuation. The test worker
  runs that same repository query on an OS thread solely because GPUI's
  deterministic executor cannot otherwise advance the concurrent save.
- A journal that outlives an already-fired settled timer re-arms the settled
  debounce only when no timer remains; ordinary fast journaling retains its
  original 500ms snapshot deadline.
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
| durable local revision plus search history restore | `ui/mod.rs` history coordinator and `app/note_session.rs` settled save | `ui::tests::history_search_discards_old_packet_after_same_generation_autosave` |
| search refresh failure/recovery | `ui/mod.rs` Retry click handler and `AppModel::commit_search_refresh` | `ui::tests::mounted_search_refresh_error_retry_click_keeps_old_cards_then_recovers` |

## Commands and results

- `cargo fmt --manifest-path packages/app-lite-gpui/Cargo.toml` — success.
- `cargo fmt --manifest-path packages/app-lite-core/Cargo.toml` — success.
- `cargo test --manifest-path packages/app-lite-core/Cargo.toml --features test-support` — success.
- `cargo test --manifest-path packages/app-lite-gpui/Cargo.toml --bin velotype -- --skip cross_block_cut_writes_markdown_deletes_range_and_undo_restores` — `1292 passed; 0 failed; 1 filtered out` (before the final Retry stale-route tightening).
- `cargo test --manifest-path packages/app-lite-gpui/Cargo.toml --bin velotype 'ui::tests::search_palette_' -- --nocapture` — `3 passed; 0 failed`; this is fresh after the final Retry stale-route tightening.
- `cargo check --manifest-path packages/app-lite-gpui/Cargo.toml` — success after final tightening.
- `cargo build --manifest-path packages/app-lite-gpui/Cargo.toml --release` — final rebuild in progress at report write time.
- `cargo fmt --manifest-path packages/app-lite-gpui/Cargo.toml` — success after the B2c follow-up.
- `cargo test --manifest-path packages/app-lite-gpui/Cargo.toml history_search_discards_old_packet_after_same_generation_autosave -- --nocapture` — `1 passed; 0 failed`.
- `cargo test --manifest-path packages/app-lite-gpui/Cargo.toml mounted_search_refresh_error_retry_click_keeps_old_cards_then_recovers -- --nocapture` — `1 passed; 0 failed`.

## Limits / remaining evidence

- No personal library was opened or accessed.
- No macOS physical IME verification was performed; the existing real marked-text mounted test remains automated coverage only.
- Disposable-profile Release smoke must use the final rebuilt binary under the Cargo metadata target directory, record its SHA, and verify normal exit. It was not claimed complete here.
- History Retry direction is wired and independently needs a fault-injected Back/Forward click regression; this follow-up adds the active-refresh Retry click regression, not a claim of complete B2 acceptance.
- Round 1 follow-up: stale history `Ok(false)` now rechecks and schedules only
  the latest same-direction pending target; Retry verifies that its history or
  active SearchRoute target is still schedulable, otherwise it replaces the
  button with an explicit changed-context notice. The unpacked Evernote source
  authority is `/Users/kevinhao/Projects/joplin-reconstruction/evernote-11.32.5`.
- The Round 1 mounted history regression uses a test-only AppModel seam only
  to install a formerly-valid, now-stale SearchRoute(A) history entry. Its
  query, commit-persistence fault, Retry click and successful same-direction
  restore use the production history coordinator; no notice or worker error is
  assigned by the test.
- The Cmd-K backdrop now occludes and stops its pointer event before it can
  reach the retained Link popover's full-window backdrop. The mounted Link
  regression proves both Escape and exposed-corner backdrop dismissal retain
  the Link popover and its focus handle, session entity, selection and undo.
  `toolbar_more` remains a mouse-only `div` without a `track_focus` handle;
  it is therefore deliberately not captured as a focus-restoration origin.

Base corrective commit: `9c9116713664156b9d78d78ea334dbe40172fac1`.
Follow-up implementation commit: `b72f58723cd51d0e3789141d71beac65854cda52`.

## Round1 exact verification

- `cargo fmt --manifest-path packages/app-lite-gpui/Cargo.toml` — success.
- `cargo test --manifest-path packages/app-lite-gpui/Cargo.toml mounted_search_refresh_error_retry_click_keeps_old_cards_then_recovers -- --nocapture` — `ui/tests.rs`, `1 passed; 0 failed`.
- `cargo test --manifest-path packages/app-lite-gpui/Cargo.toml mounted_history_retry_click_reuses_forward_after_real_commit_failure -- --nocapture` — `ui/tests.rs`, `1 passed; 0 failed`; covers fault-injected history commit failure, visible Retry, real Forward Retry click, and successful same-direction restore.
- `cargo test --manifest-path packages/app-lite-gpui/Cargo.toml search_palette_preserves_link_popover_focus_on_escape_and_backdrop -- --nocapture` — `ui/tests.rs`, `1 passed; 0 failed`; covers Link focus/visibility, Escape and exposed-corner backdrop dismissal, retained session, selection and undo.
- `cargo test --manifest-path packages/app-lite-gpui/Cargo.toml search_palette_escape_and_backdrop_restore_the_original_focus_and_session -- --nocapture` — `ui/tests.rs`, `1 passed; 0 failed`.
- `cargo test --manifest-path packages/app-lite-gpui/Cargo.toml search_palette_escape_restores_an_open_organization_input_and_its_panel -- --nocapture` — `ui/tests.rs`, `1 passed; 0 failed`.

Round1 commits: `37dd66571cabfa24742752bc91f7586ba826d06e`,
`db079032d5c9ff336a10ec1f7f80841b503fba29`, and
`5ea454258f8dde38d3ba92082e9349200b27540a`.

The latest complete suite, Release rebuild, and disposable-profile smoke remain
controller-owned final-head gates; this section intentionally does not claim
they were rerun here.
