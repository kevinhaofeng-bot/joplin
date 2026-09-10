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
