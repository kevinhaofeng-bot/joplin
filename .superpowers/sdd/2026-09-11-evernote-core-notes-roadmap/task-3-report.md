# Task 3 report — local notes library shell

## RED / GREEN

- RED: `app::tests` was introduced before `AppModel`, action types, navigation, and the repository settings API existed. Compilation failed for exactly those missing interfaces.
- RED: the nearest-survivor test deliberately supplied a visible order different from the repository refresh order. It failed because refresh cleared selection before replacement was chosen.
- GREEN: creation now commits through `LibraryRepository::create_note` before `NoteId` selection; list refresh uses only `NoteProjection`; the nearest visible `NoteId` is captured before refresh and selected only if it survives.

## Design

- The default route opens `JOPLIN_LITE_PROFILE` when supplied, otherwise an independent `Application Support/com.ArielKevin.Joplin Lite/library` profile. It no longer opens `sample_document()`; `--evernote-spike` remains its isolated measurement route.
- `AppModel` owns `Arc<LibraryRepository>`, `NavigationState`, note projections, the narrow active-session placeholder, panes and visible error status. Database changes are reached through `AppAction` only.
- Shell state is persisted through new repository `read_setting`/`write_setting` APIs. Pane widths/visibility and a valid selected ID restore on open; an invalid saved ID safely falls back with no body load.
- Cards hold only projection data. The list renders a bounded first viewport (100 cards), and repository authorizer observations prove startup/list reads do not select `notes.body_html`.
- The Task 3 editor entity is allocated but its UI truthfully states that durable save is not active. It does not show an “saved” state or silently publish temporary edits. Task 4 remains responsible for document codec, writable surface binding, and SaveCoordinator.

## Verification

- `cargo test --manifest-path packages/app-lite-gpui/Cargo.toml app::tests` — 49 passed (8 new app-state tests plus existing focused matches).
- `cargo test --manifest-path packages/app-lite-core/Cargo.toml` — 69 passed.
- `cargo test --manifest-path packages/app-lite-native/Cargo.toml` — 225 passed.
- `cargo check --all-targets --manifest-path packages/app-lite-gpui/Cargo.toml` — passed.
- `cargo test --manifest-path packages/app-lite-gpui/Cargo.toml -- --skip editor::selection::tests::cross_block_cut_writes_markdown_deletes_range_and_undo_restores` — 1,004 passed, 1 exact known donor test filtered.
- `cargo build --release --manifest-path packages/app-lite-gpui/Cargo.toml` — passed.
- `cargo fmt --manifest-path packages/app-lite-gpui/Cargo.toml -- --check` and `git diff --check` — passed.

## Manual verification and limits

No visual GUI smoke run was claimed: this execution environment did not provide a safe interactive GPUI window-observation loop. The temp-profile behavior is covered by real SQLite-backed state tests, including restart restore and the list-query authorizer. The native editable session surface and durable save are intentionally incomplete Task 4 work; the shell exposes that limit rather than claiming persistence.

## Fix round — 2026-09-11

This section supersedes the implementation claims above that described a 100-card
first viewport or an allocated-but-unrendered editor. The fixes are split across
`99e1dbe41`, `9dd460f03`, `ad64829b6`, `5a10b76f1`, and `a96f8fa7c`.

| Review finding | Code/test closure |
| --- | --- |
| C1 editor not rendered | `AppModel` retains the full selected `Note`; the library mounts the same `native_editor::surface::EditorSurface` as the spike and paints its read-only `EditorCore`. The one-way `CanonicalDocument -> Document` bridge covers paragraphs, H1-H3, all three list types, soft breaks, left/center/right alignment and rich marks/link. Unsupported generic indent or images fail closed with a visible notice and clear the previous surface. `body_text` is never an editor fallback. |
| C2 list capped at 100 | The list uses GPUI 0.2.2 `uniform_list` plus `UniformListScrollHandle`, fixed per-mode row heights and selected-item scroll-into-view. Mounted tests reach items 101 and 1,662 and assert only requested ranges construct cards. |
| I1/I3 reducer/redraw paths | CTA, card, toolbar, keyboard and native menu route through `LibraryShell::apply_action`, which updates/notifies `AppModel`; the retained shell observes it and redraws. Create, select, trash, side/list visibility, view mode and fixed enum sort all have real input paths. |
| I2/M1 shell persistence | Core owns typed, transactionally persisted `LibraryShellState`; pane parsing is strict all-or-default, `None` removes stale selection, and actual saved widths/visibility control mounted bounds. |
| I4 external events | `LibraryEvent` is consumed by a retained cancellable GPUI task using bounded `try_iter().take(128)` and a timer, with batch projection refreshes only. External create/trash and task cancellation on shell release have mounted tests. |
| I5 bootstrap regression | The default route restores preferences, i18n, theme, component key bindings, activation, a narrow native library menu, last-window quit and macOS URL intake. File/open requests become a visible no-import-yet notice; the old document editor/network/updater/workspace routes are not called. Default `--version` now identifies Joplin Lite. |
| I6/M2 visible failures | Profile override is non-empty and absolute; `ProjectDirs` failure has no cwd fallback. Fallible repository/model construction completes before normal window creation, startup failures use an error window, and action/partial-commit failures render visibly then clear after success. |
| I7 evidence quality | Tests are mutation-sensitive: codec semantics/fail-closed behavior, direct and command-catalogue read-only mutation matrices, mounted painting/copy/actions/error paths, persisted panes, virtual ranges, projection-only reads and event cancellation. |

### Exact test accounting

There are **35 new tests** relative to `f1b4b2d85`: 1 core repository-flow,
9 `AppModel`, 1 library-menu, 2 main/bootstrap, 3 codec, 2 command-catalogue,
1 direct read-only core, 1 spike shared-surface compatibility, and 15 mounted
library UI tests. The formerly quoted `app::tests` filter is not an app-only
count: its current 59 passes include 17 actual `src/app/tests.rs` tests and 42
name-matching `spike_app::tests`; it is retained only as a broad regression
command, not presented as 59 app tests.

### Fix-round verification

- `cargo test --manifest-path packages/app-lite-core/Cargo.toml` — 70 passed.
- `cargo test --manifest-path packages/app-lite-native/Cargo.toml` — 225 passed.
- `cargo test --manifest-path packages/app-lite-gpui/Cargo.toml app::tests::` — 59 passed; accounting is corrected above.
- `cargo test --manifest-path packages/app-lite-gpui/Cargo.toml ui::tests::` — 15 mounted tests passed.
- `cargo test --manifest-path packages/app-lite-gpui/Cargo.toml -- --skip editor::selection::tests::cross_block_cut_writes_markdown_deletes_range_and_undo_restores` — 1,038 passed, 1 explicitly named existing donor test filtered.
- `cargo check --all-targets --manifest-path packages/app-lite-gpui/Cargo.toml`, release build, formatter and diff checks — passed.
- Release default-route smoke with a fresh absolute `JOPLIN_LITE_PROFILE` created `library.sqlite` and returned `PRAGMA integrity_check = ok`.
- Release `--evernote-spike --fixture empty` produced a Task 7 ready marker and diagnostics (`texture_bytes=0`, render p95 present).

### Remaining deliberate limits

The library surface remains read-only until Task 4 adds the reverse codec and
durable `SaveCoordinator`. Image/resource resolution and durable image insertion
remain Task 5. These are visible limits, not silent fallback paths. The listed
Task 3 review findings now have code and test closure; the architecture still
requires an independent re-review before declaring final acceptance.
