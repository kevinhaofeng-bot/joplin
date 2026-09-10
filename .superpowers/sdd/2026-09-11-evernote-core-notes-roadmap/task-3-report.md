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

## Fix round 2 — 2026-09-11

This round addresses every item from the independent `Fix round` re-review.
The product/test implementation is `884573693` (`Close Task 3 review gaps`);
the small follow-up also adds a positive resource-read-observer proof so the
projection's zero-read assertion cannot pass after its real notification is
deleted.

| Re-review finding | Closure and mutation-sensitive evidence |
| --- | --- |
| R-I1 restored tail not visible | `LibraryShell::new` issues the selected-item `UniformListScrollHandle` request immediately after initial surface sync. `restored_tail_selection_scrolls_on_its_first_mounted_draw` pre-writes the 1,662nd selected `NoteId`, mounts once, and requires that tail range/card on the first draw. |
| R-I2 observer was bypassable | `apply_action` and the event bridge now only reduce/mutate `AppModel` and call `model_cx.notify()`. The retained observer is the sole normal path for surface replacement, deferred scroll and shell notification. `retained_model_observer_syncs_and_scrolls_an_independent_model_change` mutates/notifies the retained model outside an input callback and requires both mounted surface replacement and tail scroll, so deleting the observer or model notify turns it red. |
| R-I3 split read snapshot | `read_library_shell_state` reads panes and selection inside one deferred SQLite read transaction. A two-repository interleave hook commits a full next generation between its two reads and proves the first result is entirely old while the next is entirely new. |
| R-I4 Cmd-N double hydrate | `AppModel::create_note` now consumes the complete `Note` returned by `repository.create_note`, refreshes projection successfully first, then creates the selected active session without a second `load_note`. The load observer test requires exactly one selected ID and one hydration. |
| R-I5 early error lifecycle / dropped URL errors | The last-window quit subscription installs before every fallible profile/runtime branch, including startup-error creation. The URL bridge turns open failure into a visible `StartupErrorView` and preserves its `Result`. Mounted tests close the only startup error window and verify quit, and verify a forced URL-open failure paints an error window. |
| R-I6 retained spike strong cycle | Retained surface hooks capture `WeakEntity<SpikeView>` and tolerate failed upgrades. The typical-fixture close test holds weak parent/surface/editor/cache handles, closes the window, drains GPUI releases, and requires every handle to expire. |
| R-I7 evidence gaps | Test-only library surface hooks plus core shape and `paint_entity` counters prove the shared canvas really shapes and paints. The mounted readonly test drives real surface click, IME input, clipboard paste/key handling and copy, then checks document semantic snapshot, history depths/bytes, image-store count and selection are unchanged. Resource observers now have both a positive exact-hash test and the unsupported-image library zero-byte-read test. |
| R-M1 shell-state boundaries | `LibraryShellState::validate`/`try_new`, writer validation and live `PaneState` normalization reject or repair zero/out-of-range dimensions before rendering/persisting. Generic settings APIs reject reserved shell keys; raw SQLite remains the narrow corruption seam for recovery testing. |
| R-M2 formatting | Rustfmt was applied to both manifests and both formatter checks now pass. |

### Exact Fix-round-2 test accounting

This round adds **11 new independent tests**: one cross-connection snapshot
test; two `AppModel` tests (Cmd-N one-hydrate and live pane normalization); two
library-menu lifecycle/error-window tests; one spike destruction test; four
mounted UI tests (first-draw restored tail, independent retained observer,
shared canvas shape/paint, resource zero-read); and one positive resource-read
observer test. It also strengthens the existing mounted readonly test with real
mouse, IME, key and clipboard delivery plus document/history/image-store
invariants, and strengthens the existing persistence corruption test to use the
typed writer/reserved-key boundary.

### Fix-round-2 verification

- `cargo test --quiet --manifest-path packages/app-lite-core/Cargo.toml` — **PASS, 72 tests**.
- `cargo test --quiet --manifest-path packages/app-lite-native/Cargo.toml` — **PASS, 225 tests**.
- `cargo test --quiet --manifest-path packages/app-lite-gpui/Cargo.toml --bin velotype app::tests::` — **PASS, 62 focused matches**.
- `cargo test --quiet --manifest-path packages/app-lite-gpui/Cargo.toml --bin velotype ui::tests::` — **PASS, 19 mounted shell tests**.
- Exact codec and read-only filters — **PASS** (3 codec and 4 readonly matches); `spike_app::tests::` — **PASS, 43 matches**.
- Full GPUI suite with only the existing exact donor skip: `cargo test --quiet --manifest-path packages/app-lite-gpui/Cargo.toml --bin velotype -- --skip editor::selection::tests::cross_block_cut_writes_markdown_deletes_range_and_undo_restores` — **PASS, 1,047 passed, 1 filtered**.
- `cargo check --all-targets --manifest-path packages/app-lite-gpui/Cargo.toml` and `cargo build --release --manifest-path packages/app-lite-gpui/Cargo.toml` — **PASS**.
- `cargo fmt --manifest-path packages/app-lite-core/Cargo.toml -- --check`, `cargo fmt --manifest-path packages/app-lite-gpui/Cargo.toml -- --check`, and `git diff --check` — **PASS**.
- Fresh absolute `JOPLIN_LITE_PROFILE` release smoke stayed alive for eight seconds, created only the isolated `library.sqlite` + WAL/SHM, and returned `PRAGMA integrity_check = ok`; the child process was then intentionally terminated.
- Release spike contracts: `empty` wrote `task7-ready` and diagnostics with `texture_bytes=0`, `layout_cache_bytes=9,672`, `undo_bytes=1,233`, `render_commit_p95_us=9,091`; `typical` wrote `task7-ready` and diagnostics with `texture_bytes=26,361,856`, `layout_cache_bytes=1,009,056`, `undo_bytes=3,822`, `render_commit_p95_us=9,175`. Each smoke child was intentionally terminated after both artifacts existed.

### Remaining scope limits after round 2

There are no known remaining Task-3 Critical/Important/Minor findings in this
round. The library is deliberately read-only until Task 4; reverse conversion,
durable saving and writable editing are not claimed here. Resource/image decode
and durable insertion remain Task 5. Independent re-review remains the approval
authority.

## Fix round 3 — 2026-09-11

The third independent review found no Critical issues and two narrow Important
truthfulness gaps. This round closes only those gaps in
`5da1f9d1f` (`Preserve committed action warnings`); it does not broaden Task 4
or Task 5 scope.

| Third-review finding | Closure and mutation-sensitive evidence |
| --- | --- |
| T3-I1 queued events erased a partial-commit warning | `AppModel` now records the visible status origin (`Action`, `ProjectionEvent`, or neutral). `refresh_projection_events` can only replace a projection-origin status; it cannot turn an action/persistence error into `Ready`. A successful explicit create/select/trash recovery path clears the retained partial warning, while cosmetic actions do not. The mounted `queued_action_event_cannot_clear_partial_create_error_before_explicit_selection_recovery` test drives the real empty CTA, forces its post-commit refresh to fail, advances the retained event bridge by 60 ms, requires the visible committed-create warning to remain, then clicks the real recovered card and requires `Ready`. Deleting the action-origin guard restores the old false `Ready` and makes this test fail. |
| T3-I2 create could fail after commit while hydrating its return value | `LibraryRepository::create_note` now constructs the complete `Note` from the transaction-known title/body/snippet/default notebook/resource relations before committing, then publishes and returns that committed snapshot with no subsequent `load_note`. `create_note_does_not_consume_a_post_commit_complete_note_load_fault` arms a one-shot real `load_note` fault before create: create must publish `NoteCreated`, the explicitly later load must consume the fault, and the following load must still find the committed note. The prior post-commit hydration consumes that fault and turns an already committed create into `Err`. The strengthened `AppModel` load-observer test separately requires the transaction snapshot to install as the active session without a second hydration. |

### Exact Fix-round-3 test accounting

This round adds **2 independent mutation-sensitive tests**: one core
post-commit `load_note` fault/committed-event test and one mounted action →
event-poll → explicit-recovery composition test. It also strengthens the
existing AppModel create-session test to require zero post-commit hydrations.

### Fix-round-3 verification

- `cargo test --quiet --manifest-path packages/app-lite-core/Cargo.toml` — **PASS, 73 tests**.
- `cargo test --quiet --manifest-path packages/app-lite-native/Cargo.toml` — **PASS, 225 tests**.
- `cargo test --quiet --manifest-path packages/app-lite-gpui/Cargo.toml --bin velotype app::tests::` — **PASS, 62 focused matches**.
- `cargo test --quiet --manifest-path packages/app-lite-gpui/Cargo.toml --bin velotype ui::tests::` — **PASS, 20 mounted shell tests**.
- Exact `native_editor::codec::tests::`, `read_only`, and `spike_app::tests::` filters — **PASS, 3 / 4 / 43 matches**.
- Full GPUI suite with only the existing exact donor skip: `cargo test --quiet --manifest-path packages/app-lite-gpui/Cargo.toml --bin velotype -- --skip editor::selection::tests::cross_block_cut_writes_markdown_deletes_range_and_undo_restores` — **PASS, 1,048 passed, 1 filtered**.
- `cargo check --all-targets --manifest-path packages/app-lite-gpui/Cargo.toml` and `cargo build --release --manifest-path packages/app-lite-gpui/Cargo.toml` — **PASS**.
- `cargo fmt --check` for core and GPUI manifests, plus `git diff --check` — **PASS**.
- Fresh absolute `JOPLIN_LITE_PROFILE` release smoke stayed alive for eight seconds, created the isolated `library.sqlite` + WAL/SHM, and returned `PRAGMA integrity_check = ok`; it was then intentionally terminated.
- Release spike contracts: `empty` wrote `task7-ready` with `texture_bytes=0`, `layout_cache_bytes=9,672`, `undo_bytes=1,233`, `render_commit_p95_us=8,557`; `typical` wrote `task7-ready` with `texture_bytes=26,361,856`, `layout_cache_bytes=1,009,056`, `undo_bytes=3,822`, `render_commit_p95_us=12,623`. Each smoke child was intentionally terminated after both artifacts existed.

### Remaining scope limits after round 3

There are no known remaining findings from this third-review scope. The library
remains intentionally read-only until Task 4; reverse conversion, durable
saving and writable editing are still not claimed. Resource/image decode and
durable insertion remain Task 5.
