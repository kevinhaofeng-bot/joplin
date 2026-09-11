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
