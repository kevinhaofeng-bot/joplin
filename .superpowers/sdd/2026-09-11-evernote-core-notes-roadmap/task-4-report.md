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

Broad verification after repair commit `b7a3dddda`:

- `cargo test --no-fail-fast` in `packages/app-lite-core` — **PASS, 78
  tests**.
- `cargo check --all-targets` in `packages/app-lite-gpui` — **PASS**.
- `cargo test --no-fail-fast` in `packages/app-lite-native` — **PASS, 225
  tests**.
- Full GPUI binary suite with only the pre-existing exact donor skip —
  **PASS, 1,078 tests**; 1 filtered donor test and no new skip.
- `cargo build --release` in `packages/app-lite-gpui` — **PASS**.
- A fresh absolute `JOPLIN_LITE_PROFILE` release smoke created isolated
  `library.sqlite`, WAL, and SHM files; `PRAGMA integrity_check` returned
  **`ok`**. The smoke child was intentionally terminated after initialization.
- Release `--evernote-spike --fixture empty` — **PASS**: ready marker plus
  diagnostics were emitted (texture 0, layout 9,672, undo 1,233,
  transaction p95 19 us, render-commit p95 16,001 us).
- Release `--evernote-spike --fixture typical` — **PASS**: ready marker plus
  diagnostics were emitted (texture 16,850,944, layout 1,009,056, undo 3,822,
  transaction p95 113 us, render-commit p95 16,001 us).
- `cargo fmt --check` for both manifests and `git diff --check` — **PASS**.
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

## Independent review repair round 3 — 2026-09-11

This narrow follow-up closes the recovered-journal writer ownership gap without
adding resource, import, or Task 5 work.

- **T4-R3-I1:** `PreparedNoteSession` now retains the validated crashed writer
  token and its exact base revision. Before any entity is constructed, it
  produces a fresh retained token and calls the core-owned
  `claim_edit_journal_ownership` transaction. That `BEGIN IMMEDIATE` CAS
  requires the exact old `(note_id, revision, writer_token)`, then changes the
  database owner and compact payload owner together while retaining sequence
  and generation. `ClaimedPreparedNoteSession` is a consumed type boundary:
  `from_prepared` cannot mount recovered content without the successful claim.
  A claim conflict becomes the existing visible document/session error rather
  than a superficially editable session that will fail at its 100 ms deadline.
- A v2 checkpoint is re-encoded with its new owner during claim; the migrated
  v4 version-1 payload follows the same path, turning its deterministic
  `legacy-v4-*` owner into a coherent v2 owner before continued input.
- The existing append CAS still rejects the former pre-crash writer after the
  transfer, so a delayed worker cannot reclaim the checkpoint.

Three new mutation-sensitive tests and one strengthened v4 end-to-end test
cover the repair:

- `recovered_checkpoint_claim_is_atomic_and_rejects_the_crashed_writer`
- `recovered_checkpoint_can_journal_new_chinese_input_then_snapshot_and_restart_exactly`
- `two_recovery_candidates_can_claim_one_checkpoint_and_old_writer_cannot_retake_it`
- strengthened `v4_revision_two_journal_migrates_and_prepare_recovers_chinese_styled_content`
  now continues real title/body input through 100 ms journal, 500 ms snapshot,
  journal compaction, and a second exact restart.

Targeted verification:

- `cargo test --no-fail-fast` in `packages/app-lite-core` — **PASS, 78 tests**.
- `cargo test app::note_session_tests:: -- --nocapture` in
  `packages/app-lite-gpui` — **PASS, 19 tests**.
- `cargo test ui::tests:: -- --nocapture` in `packages/app-lite-gpui` —
  **PASS, 28 tests**.

## Independent review repair round 4 — 2026-09-11

This narrowly closes the remaining recovery linearization critical without a
schema bump or any Task 5 resource/import work. Schema v5 already persists the
monotonic `edit_journal.sequence`; this round makes that existing sequence part
of the owner lease everywhere it must be.

- **T4-R4-C1:** `JournalOwnership` is the exact durable lease
  `(writer_token, sequence)`, not a broad session permission. A recovered
  `PreparedNoteSession` retains the validated journal sequence alongside its
  base revision and old token. Its `BEGIN IMMEDIATE` claim matches
  `note_id + expected_revision + old_writer_token + exact_sequence` in both
  the read and update CAS. Thus a prepare of J1 followed by a same-token J2
  cannot claim or overwrite J2.
- Every `flush_snapshot` carries the captured optional lease through the
  retained background job. In one `BEGIN IMMEDIATE` transaction it first
  checks the note revision, then requires either no journal row or an exact
  current `(expected_revision, writer_token, sequence)` match before any note
  or resource mutation. It deletes only that exact row. A former worker that
  was queued before a recovery claim therefore fails closed: it cannot advance
  the note revision or remove the new owner's checkpoint. The new owner can
  journal a new sequence and snapshot it normally.

Two new mutation-sensitive, real retained-worker/gated tests cover the two
required reverse orderings:

- `prepared_recovery_rejects_a_gated_same_owner_checkpoint_replacement`:
  prepare J1, release the crashed writer's gated J2, then assert the stale J1
  claim conflicts and the J2 bytes remain durable.
- `claimed_recovery_rejects_a_gated_former_owner_snapshot_and_allows_the_new_owner`:
  queue A's snapshot, claim J1 as B, publish B J2, release A, assert A enters
  `Failed` without changing the note or deleting J2, then assert B's own
  leased snapshot succeeds and compacts J2.

Verification after repair commit `e8e151359`:

- `cargo test app::note_session_tests:: -- --nocapture` in
  `packages/app-lite-gpui` — **PASS, 21 tests**.
- `cargo test ui::tests:: -- --nocapture` in `packages/app-lite-gpui` —
  **PASS, 28 tests**.
- `cargo test --no-fail-fast` in `packages/app-lite-core` — **PASS, 78
  tests**.
- `cargo check --all-targets` in `packages/app-lite-gpui` — **PASS**.
- `cargo test --no-fail-fast` in `packages/app-lite-native` — **PASS, 225
  tests**.
- Full GPUI binary suite with only the existing exact donor skip — **PASS,
  1,080 tests**; 1 filtered donor test and no new skip.
- `cargo build --release` in `packages/app-lite-gpui` — **PASS**.
- Fresh absolute `JOPLIN_LITE_PROFILE` release smoke created isolated
  `library.sqlite`, WAL, and SHM files; `PRAGMA integrity_check` returned
  **`ok`**. The smoke child was intentionally terminated after initialization.
- Release `--evernote-spike --fixture empty` — **PASS**: exact readiness marker
  and diagnostics contract (texture 0, layout 9,672, undo 1,233,
  transaction p95 6 us, render-commit p95 8,917 us).
- Release `--evernote-spike --fixture typical` — **PASS**: exact readiness
  marker and diagnostics contract (texture 26,361,856, layout 1,009,056,
  undo 3,822, transaction p95 31 us, render-commit p95 9,177 us).
- `cargo fmt --check` for core and GPUI manifests plus `git diff --check` —
  **PASS** after the final report commit below.

## Independent review repair round 5 — 2026-09-11

This narrow compatibility repair closes the remaining v4 multi-checkpoint
recovery gap without a schema bump or any Task 5 resource/import work.

- **T4-R5-I1:** the v4-to-v5 journal backfill now parses and validates every
  legacy version-1 payload inside the migration transaction, then groups rows
  independently by note. For each live base revision it keeps exactly one
  replayable checkpoint, choosing deterministically by
  `(created_time, generation, rowid)`. It safely removes lower-revision rows
  that are already superseded by the durable note; a future revision, missing
  note, malformed payload, or note-ID mismatch remains fail-closed. This
  prevents a later timestamp on a stale base from winning and never deletes a
  valid checkpoint belonging to another note.
- Current journal ownership lookup in both snapshot and append is now scoped
  to the current `expected_revision`. After the supplied lease has been
  verified inside the same `BEGIN IMMEDIATE` snapshot transaction, compaction
  removes that note's same-or-lower-base residue while preserving a hypothetical
  future-base row. A recovered lifecycle flush therefore cannot strand a
  legacy foreign owner that blocks the next journal or snapshot.

The repair was developed RED first. Before the migration change, the real
multi-row v4 fixtures failed closed with `InvalidLegacyEditJournal` because a
valid stale-base legacy row was treated as fatal; before revision-scoped
ownership/compaction, the core stale-residue continuation failed with
`JournalOwnershipConflict`. All three tests are now green and would fail again
if their corresponding grouping, scope, or cleanup statements were removed:

- `v4_migration_keeps_one_latest_current_checkpoint_per_note_without_touching_other_notes`
  seeds J1/J2 for a revision-2 note, a later-timestamp stale revision-1 row,
  and another note's valid journal; only J2 and the other note survive.
- strengthened
  `journal_writer_token_owns_the_checkpoint_against_cross_window_replay`
  injects an old-base v5 residue, verifies a current lifecycle snapshot cleans
  it, then journals and snapshots a new current generation normally.
- `v4_multi_checkpoint_recovery_lifecycle_flush_compacts_then_continues_after_restart`
  uses real `EntityInputHandler` title/body input: recover J2, invoke a
  `WindowClose` flush before another edit, assert no same-note foreign journal
  remains, then complete the 100 ms journal, 500 ms snapshot, and exact styled
  second restart.

Implementation commit: `6a50a33ff Compact stale v4 journal checkpoints`.

Verification after that commit:

- Core targeted migration/repository flow suites — **PASS, 11/11 and 7/7**.
- `cargo test app::note_session_tests:: -- --nocapture` — **PASS, 22/22**.
- `cargo test ui::tests:: -- --nocapture` — **PASS, 28/28**.
- `cargo test --no-fail-fast` in `packages/app-lite-core` — **PASS, 79
  tests**.
- `cargo test --no-fail-fast` in `packages/app-lite-native` — **PASS, 225
  tests**.
- `cargo check --all-targets` in `packages/app-lite-gpui` — **PASS**.
- Full GPUI binary suite with exactly the pre-existing donor skip — **PASS,
  1,081 tests**; 1 filtered donor test and no new skip.
- `cargo build --release` in `packages/app-lite-gpui` — **PASS**.
- A fresh absolute `JOPLIN_LITE_PROFILE` release smoke created an isolated
  `library.sqlite`; `PRAGMA integrity_check` returned **`ok`**. The child was
  intentionally terminated after initialization.
- Release `--evernote-spike --fixture empty` — **PASS**: exact readiness
  marker plus five-number diagnostics (texture 0, layout 9,672, undo 1,233,
  transaction p95 5 us, render-commit p95 9,140 us).
- Release `--evernote-spike --fixture typical` — **PASS**: exact readiness
  marker plus five-number diagnostics (texture 26,361,856, layout 1,009,056,
  undo 3,822, transaction p95 31 us, render-commit p95 9,306 us).
- `cargo fmt --check` for core and GPUI manifests plus `git diff --check` —
  **PASS** after the report commit below.

## Independent review repair round 6 — 2026-09-11

This repair closes the final v4 corrupt-checkpoint migration data-loss path.
It is deliberately limited to Task 4 crash-journal compatibility; it does not
introduce Task 5 resource-byte import or editing behavior.

- **T4-R6-C1:** `app-lite-core` now owns the strict version-1 wire boundary in
  `journal::LegacyJournalPayload`. Its DTO rejects missing, wrongly typed, or
  unknown fields, verifies the exact version/note/revision/generation contract
  against the v4 SQL row, applies bounded title/body/resource-list wire ranges,
  and requires canonical HTML to round-trip byte-for-byte through
  `CanonicalDocument`.
- Every v4 row is validated before it is allowed to participate in stale/current
  routing or deterministic winner ranking. The payload resource list must equal
  the note's live associated-resource relation in durable order, and the parsed
  document's resource references must equal that same list. Invalid SQL IDs,
  negative timestamps, future/missing note revisions, and invalid legacy writer
  identities remain fail-closed.
- Version-1 journal payloads did not store a writer token. Migration derives the
  v5 lease token only after validating the opaque v4 journal ID with the legacy
  32-lowercase-hex rule; malformed IDs cannot become a v5 owner. The validator
  lives in core and has no GPUI dependency. GPUI v1 recovery now reuses that
  exact validator rather than retaining a weaker local decoder.
- The schema migration remains one transaction. A malformed row leaves
  `user_version`, v4 table shape, and every original journal row untouched;
  valid rows still compact deterministically only after the full row set is
  safe to interpret.

The repair was developed RED first. Before it, a valid readable J1 plus a
newer current-base J2 missing `body_html` migrated successfully, deleted J1,
and published v5 with only an unreadable checkpoint. It now returns
`InvalidLegacyEditJournal` with a byte-for-byte unchanged v4 snapshot.

Two new mutation-sensitive core migration test functions cover this boundary:

- `v4_migration_rejects_a_malformed_newer_checkpoint_without_writing_the_profile`
  is the reviewer reproduction: valid J1 plus later malformed J2 must preserve
  both rows and schema version 4.
- `v4_migration_rejects_every_unrecoverable_wire_field_without_mutating_legacy_rows`
  runs a field matrix for wrong title/body/resource types, SQL/payload
  generation mismatch, zero generation, noncanonical HTML, valid-but-unrelated
  resource references, and an ID that cannot derive a writer token. Each case
  starts with a readable J1 and asserts the exact raw v4 snapshot survives.
  Existing valid multi-row compaction and v4 recovery/continuation cases remain
  green.

Implementation commit: `63fae07e5 Validate legacy journal migration payloads`.

Verification after that commit:

- `cargo test --no-fail-fast` in `packages/app-lite-core` — **PASS, 81
  tests**; migration suite **13/13**.
- `cargo test --no-fail-fast` in `packages/app-lite-native` — **PASS, 225
  tests**.
- `cargo test app::note_session_tests:: -- --nocapture` — **PASS, 22/22**;
  the two v4 recovery/continuation tests are included.
- `cargo test ui::tests:: -- --nocapture` — **PASS, 28/28**.
- `cargo check --quiet --all-targets` in `packages/app-lite-gpui` — **PASS**.
- Full GPUI binary suite with exactly the existing donor skip — **PASS,
  1,081 tests**; 1 filtered donor test and no new skip.
- `cargo build --quiet --release` in `packages/app-lite-gpui` — **PASS**.
- Fresh absolute `JOPLIN_LITE_PROFILE` release smoke created an isolated
  `library.sqlite`; `PRAGMA integrity_check` returned **`ok`** and its log was
  empty. The smoke child was intentionally terminated after initialization.
- Release `--evernote-spike --fixture empty` — **PASS**: exact readiness marker,
  empty log, and diagnostics: texture 0, layout 9,672, undo 1,233,
  transaction p95 6 us, render-commit p95 8,874 us.
- Release `--evernote-spike --fixture typical` — **PASS**: exact readiness
  marker, empty log, and diagnostics: texture 26,361,856, layout 1,009,056,
  undo 3,822, transaction p95 32 us, render-commit p95 9,213 us.
- `cargo fmt --check --all` for core and GPUI manifests plus `git diff --check`
  — **PASS** after the final report commit below.
