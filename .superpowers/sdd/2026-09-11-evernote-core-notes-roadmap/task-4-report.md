# Task 4 report — durable native NoteSession

## Outcome

The default library route now mounts an editable, retained `NoteSession` for
the selected note.  It owns the real `TitleInput`, the shared native
`EditorCore`, generation-aware local-save state, and the durable repository
revision.  It does not flatten the native document to plain text.

## TDD evidence

The acceptance tests were written against the production `EntityInputHandler`
paths before the session/save implementation was completed:

- Chinese title and body input survives an explicit save and a fresh retained
  session, including bold, italic, link, and the edited text.
- A 100 ms readable JSON journal precedes a 500 ms settled canonical snapshot;
  continuously changing input reaches the 15 s hard snapshot deadline without
  wall-clock sleeps.
- A journal left before the snapshot is reconstructed on reopen.  Note switch,
  window close, quit, delete, and manual sync all compact the active generation
  or keep the UI alive with a visible local-save error.
- A real Chinese IME marked range is never journaled as a candidate.  A close
  attempt visibly blocks until the production input handler commits/unmarks it.
- A delayed session-A callback cannot serialize session-B's document after a
  retained-model selection change.

## Codec and resource boundary

The bidirectional codec maps Paragraph, H1–H3, bullet/ordered/check lists,
Quote, Code, Image, Attachment, Divider, alignment, soft breaks, and
Bold/Italic/Underline/Strike/Highlight/Link/InlineCode.  The canonical writer
uses explicit native markers for quote/code so legacy ordinary `<blockquote>`
and `<pre>` HTML keeps its pre-existing canonical interpretation.  Re-parsing
generated HTML produces the same semantic document.

Resource-bearing blocks are accepted only when their resource IDs are already
in the selected note relation.  Missing relations fail closed before a journal
or repository transaction.  Task 4 does not import, decode, or insert resource
bytes; Task 5 remains responsible for that transaction.

## Implementation

- `SaveCoordinator` schedules retained 100 ms/500 ms/15 s deadlines per
  dirty generation. Composition freezes due work; `Failed` is terminal until
  a new committed edit, never a periodic retry state.
- `NoteSession` captures an immutable unmarked native snapshot at every input
  commit boundary. Codec and SQLite work consume only that detached payload on
  GPUI's background executor, then re-check the generation before publishing
  `Failed` or `Clean`.
- Journal v2 stores a readable title/body splice instead of a second pretty
  HTML document. Core assigns a database-monotonic sequence and writer token,
  checks the expected note revision in an `IMMEDIATE` transaction, keeps one
  checkpoint per note, and recovery accepts only the exact durable base
  revision. A successful snapshot deletes all old journal rows in its commit.
- Lifecycle flush starts the same background snapshot and visibly blocks the
  boundary until its completion-confirmed `Clean` state; it does not pretend a
  synchronous foreground SQLite write succeeded.
- `LibraryShell` owns the retained session and shared editable surface.  It
  flushes before switch/new/delete/manual-save, installs the GPUI
  `on_window_should_close` gate, and the app Quit route flushes every live
  shell before requesting platform quit.

## Exact Task 4 test accounting

Relative to Task 4 baseline `17318ae229e1906ad9171ac440f781f472624fda`, this
implementation adds **26 independent mutation-sensitive tests**:

- 14 `app::note_session_tests` tests for native input/restart, 100/500/15 s,
  crash recovery, every explicit flush boundary, stale generations, and both
  title/body IME paths;
- 6 mounted `ui::tests` tests for real title/body/copy/save, slow-worker paint,
  stale A→B routing, window-close, reducer boundaries, quit, and the retained
  IME lifecycle notice;
- 2 repository/migration tests for v5 journal upgrade plus same-note
  two-window writer-token/sequence/revision safety;
- 1 scheduler state-machine test, 2 complete codec/resource fail-closed
  tests, and 1 Quote/Code compatibility test.

The independent-review repair contributes **12** of those 26: compact delta,
retained 100 ms timer, title/body dirty→IME, immutable background handoff,
two failure-state tests plus stale failure, slow-worker mounted paint, mounted
lifecycle notice, and the two v5 persistence tests.

## Verification

- `cargo test --manifest-path packages/app-lite-core/Cargo.toml --no-fail-fast`
  — **PASS, 76 unit/integration tests**.
- `cargo test --manifest-path packages/app-lite-native/Cargo.toml --no-fail-fast`
  — **PASS, 225 tests** in a standalone macOS run.
- `cargo test --quiet --manifest-path packages/app-lite-gpui/Cargo.toml --bin velotype -- --skip editor::selection::tests::cross_block_cut_writes_markdown_deletes_range_and_undo_restores`
  — **PASS, 1,071 passed; 1 exact pre-existing donor SIGSEGV test filtered**.
- `cargo test --manifest-path packages/app-lite-gpui/Cargo.toml 'app::note_session_tests::'`
  — **PASS, 14/14**; `ui::tests::` — **PASS, 26/26**; core `migration`
  — **PASS, 9/9**; core `repository_flow` — **PASS, 6/6**.
- `cargo check --all-targets --manifest-path packages/app-lite-gpui/Cargo.toml`
  — **PASS** (existing upstream warnings only).
- `cargo build --release --manifest-path packages/app-lite-gpui/Cargo.toml`
  — **PASS** at repair commit `74efe992c`.
- A fresh absolute `JOPLIN_LITE_PROFILE` release smoke created isolated
  `library.sqlite`, WAL, and SHM files; `PRAGMA integrity_check` returned
  **`ok`**.  The exact smoke child was then intentionally terminated.
- Release `--evernote-spike --fixture empty` — **PASS**: readiness and the
  five-number diagnostics contract were written; texture 0, layout 9,672,
  undo 1,233 bytes, transaction p95 6 μs, render-commit p95 8,450 μs.
- Release `--evernote-spike --fixture typical` — **PASS**: readiness and the
  diagnostics contract were written; texture 26,361,856, layout 1,009,056,
  undo 3,822 bytes, transaction p95 29 μs, render-commit p95 9,136 μs.  Both
  fixture readiness/diagnostics gates passed. This Task 4 smoke does not
  replace Task 7's longer RSS/child-process measurement evidence.
- `cargo fmt --check` for core and GPUI manifests, plus `git diff --check`
  — **PASS** after the final verification-record commit below.

## Independent review repair — 2026-09-11

- **T4-C1:** marked title/body input freezes due work. The previous committed
  `NativeSessionSnapshot` is immutable and detached before codec/SQLite work;
  dirty→IME tests advance 100 ms, 500 ms, and 15 s, cancel candidates, and
  verify neither journal nor durable snapshot contains them.
- **T4-I1:** every codec/repository result reaches `finish_save`; a current
  generation error becomes `Failed`, has no timer retry, remains visible, and
  a stale worker error cannot poison a later corrected generation.
- **T4-I2:** schema v5 adds `expected_revision`, `writer_token`, global
  `sequence`, and its index after legacy columns/rebuilds. `BEGIN IMMEDIATE`
  CAS rejects stale appends; recovery selects only the matching revision;
  snapshot compaction removes all old rows.
- **T4-I3:** journal v2 is a compact readable delta and all codec/SQLite work
  is inside a retained GPUI background task. A mounted slow-worker gate proves
  the editor's real `paint` callback advances while the journal is stalled.
- **T4-I4:** a lifecycle IME blocker is separate from automatic-save state.
  It survives clean ticks and clears only after composition resolves and the
  requested flush succeeds.

## Scope limits

Task 4 deliberately does not add resource/image import or durable resource
insertion.  The library now permits durable title/body editing; unsupported or
unrelated resource blocks remain visibly fail-closed rather than being silently
discarded.

## Independent review repair round 2 — 2026-09-11

This round closes the second review's in-flight boundary, legacy migration,
deadline, ownership, and stale-warning findings without adding Task 5 resource
work.

- **T4-R2-C1:** a `FlushBarrier` records the exact local generation and durable
  base revision requested by a lifecycle boundary. `Journaling` and
  `Snapshotting` are never reported as an old successful `last_saved` result.
  The retained production worker finishes the current job, then dispatches the
  barrier's exact snapshot; only the matching generation/revision completion
  clears it. Repeated switch, delete, close, quit, and manual-save attempts
  stay visibly blocked until then.
- **T4-R2-C2:** the v4 table shape is detected before adding v5 defaults.
  Migration parses each version-1 payload in its transaction, validates its
  note ID and live base revision, backfills `expected_revision`, assigns a
  deterministic non-empty legacy writer token, and preserves a durable
  sequence. Invalid/mismatched payloads abort migration instead of becoming
  silently unrecoverable.
- **T4-R2-I1:** the 100 ms journal timer is anchored to the first committed
  edit not already checkpointed. Subsequent sub-100 ms input refreshes only
  the immutable captured payload; it cannot cancel the promised crash journal.
  The 500 ms settled timer remains an idle debounce and 15 s remains a hard
  snapshot limit.
- **T4-R2-I2:** journal replacement now verifies the existing writer token
  inside the same `BEGIN IMMEDIATE` transaction as the revision CAS. A late
  foreign writer fails visibly instead of deleting the checkpoint that won the
  same-base race.
- **T4-R2-I3:** `LibraryShell` distinguishes lifecycle blockers from an
  automatic error tagged with its failed generation. A later durable `Clean`
  generation clears only the automatic warning; lifecycle/IME errors remain
  until their explicit successful boundary.

Six new mutation-sensitive tests provide the direct regression coverage:

- `v4_legacy_journal_backfills_payload_revision_without_losing_recovery`
- `retained_journal_deadline_is_anchored_to_the_first_unjournaled_edit`
- `gated_older_writer_cannot_replace_a_newer_same_note_checkpoint`
- `v4_revision_two_journal_migrates_and_prepare_recovers_chinese_styled_content`
- `mounted_inflight_worker_blocks_switch_delete_close_and_quit_until_completion`
- `mounted_corrected_generation_clears_only_its_automatic_save_error`

The existing core two-writer test was also strengthened to assert the visible
ownership conflict and retained checkpoint. The mounted action, close, quit,
and IME tests now use the real asynchronous retained worker contract: first
boundary blocks, completion is drained, then retry succeeds.

Targeted verification before the broad release pass:

- `cargo test --tests -- --nocapture` in `packages/app-lite-core` — **PASS,
  77 tests**.
- `cargo test app::note_session_tests:: -- --nocapture` in
  `packages/app-lite-gpui` — **PASS, 17 tests**.
- `cargo test ui::tests:: -- --nocapture` in `packages/app-lite-gpui` —
  **PASS, 28 tests**.
- `cargo fmt` for both manifests and `git diff --check` — **PASS**.

Broad verification after repair commit `8f6debc032059c4cfdee61ae5f3f0a2a4edb36b1`:

- `cargo test --no-fail-fast` in `packages/app-lite-core` — **PASS, 77
  tests** (including the v4 revision-2 recovery migration).
- `cargo check --all-targets` in `packages/app-lite-gpui` — **PASS**.
- `cargo test --no-fail-fast` in `packages/app-lite-native` — **PASS, 225
  tests**.
- Full GPUI binary suite with only the pre-existing exact donor skip —
  **PASS, 1,076 tests**; 1 filtered donor test and no new skip.
- `cargo build --release` in `packages/app-lite-gpui` — **PASS**.
- A fresh absolute `JOPLIN_LITE_PROFILE` release smoke created an isolated
  `library.sqlite`, WAL, and SHM; `PRAGMA integrity_check` returned **`ok`**.
  The smoke child was intentionally terminated after initialization.
- Release `--evernote-spike --fixture empty` — **PASS**: ready marker plus
  diagnostics were emitted (texture 0, layout 9,672, undo 1,233,
  transaction p95 6 us, render-commit p95 9,185 us).
- Release `--evernote-spike --fixture typical` — **PASS**: ready marker plus
  diagnostics were emitted (texture 26,361,856, layout 1,009,056, undo 3,822,
  transaction p95 31 us, render-commit p95 9,169 us).
- `cargo fmt --check` for core and GPUI plus `git diff --check` — **PASS**.
