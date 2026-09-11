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

- `SaveCoordinator` writes a readable whole-snapshot journal after 100 ms,
  settles a canonical snapshot after 500 ms, and forces one after 15 s.  Its
  work tokens carry the session generation, and `Clean` is published only when
  the same generation has committed.
- `NoteSession` checks the expected revision before every snapshot, constructs
  its journal from title/body/resource relations, restores a compatible journal
  only, and delegates durable commit/event publication to the repository's
  post-transaction `flush_snapshot` path.
- `LibraryShell` owns the retained session and shared editable surface.  It
  flushes before switch/new/delete/manual-save, installs the GPUI
  `on_window_should_close` gate, and the app Quit route flushes every live
  shell before requesting platform quit.

## Exact Task 4 test accounting

This implementation adds **14 independent Task 4 tests**:

- 6 `app::note_session_tests` tests for input/restart, timing, hard deadline,
  crash recovery and every explicit boundary, IME composition, and stale work;
- 5 mounted `ui::tests` tests for real title/body/copy/save, stale A→B routing,
  window-close, reducer flush boundaries, and app-level quit;
- 2 codec tests for the complete structural/mark map and resource fail-closed
  behavior;
- 1 core canonical marker compatibility test for Quote/Code versus legacy HTML.

## Verification

- `cargo test --manifest-path packages/app-lite-core/Cargo.toml --no-fail-fast`
  — **PASS, 74 unit/integration tests**.
- `cargo test --manifest-path packages/app-lite-native/Cargo.toml --no-fail-fast`
  — **PASS, 225 tests**.
- `cargo test --quiet --manifest-path packages/app-lite-gpui/Cargo.toml --bin velotype -- --skip editor::selection::tests::cross_block_cut_writes_markdown_deletes_range_and_undo_restores`
  — **PASS, 1,061 passed; 1 exact pre-existing donor SIGSEGV test filtered**.
- `cargo check --all-targets --manifest-path packages/app-lite-gpui/Cargo.toml`
  — **PASS** (existing upstream warnings only).
- `cargo build --release --manifest-path packages/app-lite-gpui/Cargo.toml`
  — **PASS** at `781c520c2`.
- A fresh absolute `JOPLIN_LITE_PROFILE` release smoke created isolated
  `library.sqlite`, WAL, and SHM files; `PRAGMA integrity_check` returned
  **`ok`**.  The exact smoke child was then intentionally terminated.
- Release `--evernote-spike --fixture empty` — **PASS**: readiness and the
  five-number diagnostics contract were written; texture 0, layout 9,672,
  undo 1,233 bytes, transaction p95 6 μs, render-commit p95 8,711 μs.
- Release `--evernote-spike --fixture typical` — **PASS**: readiness and the
  diagnostics contract were written; texture 26,361,856, layout 1,009,056,
  undo 3,822 bytes, transaction p95 31 μs, render-commit p95 9,110 μs.  Both
  fixture gate sets passed with no child process and no WebKit link.
- `cargo fmt --check` for core and GPUI manifests, plus `git diff --check`
  — **PASS** after the final verification-record commit.

## Scope limits

Task 4 deliberately does not add resource/image import or durable resource
insertion.  The library now permits durable title/body editing; unsupported or
unrelated resource blocks remain visibly fail-closed rather than being silently
discarded.
