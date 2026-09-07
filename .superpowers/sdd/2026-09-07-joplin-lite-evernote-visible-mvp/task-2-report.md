# Task 2 report: native semantic editor session

## Result

Implemented `NativeEditorSession` and the explicit `Document` ↔ pinned
`text-document = 1.12.1` adapter in `packages/app-lite-native/src/native_editor.rs`.
Persistence continues to use the existing canonical `Document` serializer;
there is no `TextDocument::to_html()` call and no image byte/base64 insertion
into `TextDocument`. Image anchors retain only resource name/id, alt text and
positive dimensions required by the upstream anchor API; pixel data remains in
`ResourceStore`.

The session owns the only active `TextDocument` undo stack. A process-scoped
`DocumentBackend` is shared by sessions, so opening/switching notes does not
start one event-pump thread per note. AppKit projection includes semantic font
sizes, paragraph alignment/indent, list/checklist display prefixes, link
attributes, attachments, and marked missing-resource placeholders. Projection
prefixes and missing-image labels carry private attributes and are not
canonical text.

Implemented behavior includes:

- heading levels 1–3, paragraph/list/checklist blocks, nested indent, block
  alignment and paragraph indent commands;
- bold, italic, underline, strikeout, highlight, clear and link commands;
- `http`, `https` and `mailto` link validation with control-character and
  malformed-authority rejection;
- checked UTF-16 range conversion which rejects surrogate-splitting ranges;
- committed text deltas through `TextCursor`, including CJK, emoji, combining
  text and cross-block edits;
- active/inactive/mixed inline selection state;
- checklist toggle through the semantic marker, never by inserting a glyph;
- one-owner undo/redo revision tracking;
- `NSTextView`'s undo manager is disabled; Edit > Undo/Redo calls the session,
  then reprojects and persists the canonical document, so there is no second
  AppKit history;
- AppKit write-back uses the live semantic session. Plain typing is applied as
  a delta so existing heading/list/image anchors survive; attachment edits use
  the existing safe AppKit decoder as a guarded compatibility fallback;
- IME marked text is left in TextKit and is not synchronized until commit;
- title-only saves preserve the loaded semantic session and do not flatten the
  body.

## RED/GREEN and gates

Focused RED was established before the implementation by adding the new
`native_editor` API/tests while the module/dependency boundary was absent;
the first compile failed on the missing module. GREEN is now recorded by:

```text
cargo test --manifest-path packages/app-lite-native/Cargo.toml --locked native_editor
4 passed
cargo check --manifest-path packages/app-lite-native/Cargo.toml --locked
passed
cargo clippy --manifest-path packages/app-lite-native/Cargo.toml --all-targets --locked -- -D warnings
passed
cargo test --manifest-path packages/app-lite-native/Cargo.toml --locked
64 library tests + 31 app tests + 7 lifecycle tests passed
cargo fmt --manifest-path packages/app-lite-native/Cargo.toml -- --check
passed after formatting
git diff --check
passed
cargo build --manifest-path packages/app-lite-native/Cargo.toml --release --locked
passed; release binary 6,100,304 bytes after the undo-owner integration
```

The initial release build also passed:

```text
cargo build --manifest-path packages/app-lite-native/Cargo.toml --release --locked
binary: 6,080,432 bytes before the focused undo-owner integration
```

The lockfile grows from the previous 88-package tree to 243 packages because
the upstream editor crate includes its clean-architecture document IO and
formatting components. `default-features = false` does not remove those
components in 1.12.1 (the only optional feature is PDF), so no feature change
was made. A separate headless spike measured the pinned model's release
binary at about 5.6 MB and idle peak RSS about 7.6 MB; the full AppKit binary
is 6.08 MB. No GUI was launched in this task.

## Known limits

The existing MVP toolbar still owns its AppKit visual gesture implementation;
the semantic command API is now available and formatting actions update the
session before persistence. Full native toolbar/list UI wiring and richer
Return/list behavior remain follow-up work. The upstream public image anchor
API requires positive dimensions, so a live anchor uses `1×1` until AppKit
projection supplies the actual resource dimensions; no image bytes are copied
into the text model. If the semantic session cannot be built, the write-back
gate refuses to overwrite the body rather than falling back to a lossy body
codec.

## Fix 1 review findings: RED/GREEN evidence (2026-09-07)

### RED: reproduce both Important findings

Before the fix, the following new regressions were added to the focused tests:

```text
cargo test --locked --manifest-path packages/app-lite-native/Cargo.toml native_editor
running 7 tests
FAILED renderer_underlying_text_matches_addressable_text_without_projection_prefixes
  left:  "标题😀\n☑ [图片：图] item\n"
  right: "标题😀\n￼ item"
FAILED renderer_distinguishes_heading_levels
  assertion failed: h1 > h2 && h2 > h3
FAILED renderer_projects_all_inline_marks
  missing highlight projection
```

The IME boundary regression was initially assertion-bearing but had no
production decision helper, so the first run failed at the missing symbol:

```text
cargo test --locked --manifest-path packages/app-lite-native/Cargo.toml ime_marked_text_skips_semantic_sync_and_save_until_commit
error[E0425]: cannot find function `should_sync_editor_change` in module `super`
```

The RED tests demonstrated the renderer string mismatch, list/checklist and
missing-image projection errors, missing heading distinction, missing inline
mark projection, and the absent marked-text gate before any root-cause fix was
kept.

### GREEN: focused and full gates

```text
cargo test --locked --manifest-path packages/app-lite-native/Cargo.toml native_editor
9 passed; 0 failed

cargo test --locked --manifest-path packages/app-lite-native/Cargo.toml ime_marked_text_skips_semantic_sync_and_save_until_commit
1 passed; 0 failed

cargo test --locked --manifest-path packages/app-lite-native/Cargo.toml
69 library tests + 32 app tests + 7 lifecycle tests passed

cargo fmt --manifest-path packages/app-lite-native/Cargo.toml -- --check
passed

cargo clippy --manifest-path packages/app-lite-native/Cargo.toml --all-targets --locked -- -D warnings
passed

git diff --check
passed
```

### Fix design

- `load_note`, semantic refresh, undo/redo and selection formatting now all
  project through `render_session`; the old lossy document renderer is no
  longer a production call site.
- The renderer emits exactly `TextDocument::to_addressable_text()`: no
  literal list/checklist prefix, no trailing newline, and every image (loaded
  or missing) is one U+FFFC `NSTextAttachment` sentinel. `NSTextList` and
  paragraph styles provide list markers and preserve alignment/indent.
- H1/H2/H3 use distinct semantic font sizes. B/I/U/strike/link and highlight
  are projected as AppKit attributes. `text-document` 1.12.1 does not carry
  character background color through its public format DTO, so highlight
  ranges are a semantic-session sidecar synchronized with the single
  text-document undo/redo owner; they are restored when converting back to
  `Document` and covered by undo/redo tests.
- Consecutive list items are created as one native list run before per-item
  marker/indent mapping, avoiding flattened boundaries while retaining image
  anchor positions.
- `textDidChange`, the direct semantic sync helper, and autosave all stop when
  `NSTextView.hasMarkedText()` is true. The committed composition resumes the
  ordinary UTF-16 delta path, so SQLite is not touched by intermediate IME
  marked text.
- No `TextDocument::to_html()` call, WebKit path, GUI launch, or image
  bytes/base64 insertion into the semantic text model was added.

## Fix 1 second-round review: RED/GREEN evidence (2026-09-07)

### RED: four Important findings reproduced before the second-round fixes

The new regressions were first run against the inherited product code (the
review document and research notes were not changed):

```text
cargo test --lib native_editor::tests::renderer_reuses_ordered_list_and_keeps_empty_list_carrier_without_text -- --exact
FAILED: first and second ordered paragraphs had different NSTextList pointers

cargo test --lib native_editor::tests::list_indent_commands_round_trip_through_list_format -- --exact
FAILED: list indent after IncreaseIndent was 0, expected 1

cargo test --bin joplin-lite-native app::tests::semantic_sync_equal_text_is_noop_and_attachment_sentinel_is_rejected -- --exact
FAILED: equal text classified as Rebuild, expected Noop

cargo test --lib native_editor::tests::second_round_history_is_empty_on_load_and_one_record_per_mixed_command -- --exact
FAILED before the fix at the load-history assertion: construction formatting
left an undoable entry.
```

The RED probes also cover the newly discovered attachment boundary: a lone
removed U+FFFC is routed to an explicit image-anchor delete, while an inserted
or mixed attachment sentinel remains rejected. A committed image-delete test
was added before the final GREEN run and verifies canonical removal plus undo
restoration.

### GREEN: focused and locked gates

```text
cargo test --locked --manifest-path packages/app-lite-native/Cargo.toml native_editor::tests:: -- --nocapture
14 passed; 0 failed

cargo test --locked --manifest-path packages/app-lite-native/Cargo.toml app::tests:: -- --nocapture
35 passed; 0 failed

cargo test --locked --manifest-path packages/app-lite-native/Cargo.toml
75 library tests + 35 app tests + 7 lifecycle tests passed

cargo fmt --manifest-path packages/app-lite-native/Cargo.toml -- --check
passed

cargo clippy --locked --manifest-path packages/app-lite-native/Cargo.toml \
  --all-targets -- -D warnings
passed

git diff --check
passed
```

### Second-round design and scope

- `NativeEditorSession` keeps `TextDocument` as the sole incremental history
  owner. Construction calls `clear_undo_redo()` and applies a 200-entry undo
  limit. Every visible semantic command starts with `break_undo_merge()` and
  `begin_edit_block()`, then ends with `end_edit_block()` and a sealed merge;
  the bounded highlight sidecar records the matching pre-command range state,
  so mixed text/highlight/list commands undo and redo in order without an
  unbounded full-document snapshot stack. Highlight-only commands also merge a
  real background-color format operation so they have a native model undo
  entry; transparent background is treated as not highlighted on readback.
- Live `textDidChange` now has an explicit Noop/Applied/Rejected result. Equal
  text is a no-op; an ordinary safe delta uses the semantic text command; a
  failed delta is fail-closed and never invokes the legacy attributed-string
  decoder. A lone removed U+FFFC calls `delete_image_anchor`, an explicit
  resource-aware semantic operation whose canonical model removal is undoable;
  inserted or mixed attachment sentinels are rejected. Rejected syncs do not
  enter `save_current_note()` and leave an error status for retry.
- Renderer list projection reuses one `NSTextList` instance for each
  consecutive compatible list run; the headless ordered probe observes marker
  1 and 2 from the shared list. Underlying text remains exactly addressable
  text. Middle empty blocks apply their paragraph/list style to their native
  newline carrier. A true zero-character terminal/solo block exposes an
  `EmptyBlockCarrier` sidecar; shared `install_rendered_document` consumption
  in both load and refresh applies it only for a collapsed caret at the exact
  carrier offset via `defaultParagraphStyle` and `typingAttributes`, without
  adding a fake canonical character.
- Paragraph alignment/indent commands keep block format semantics, while list
  indent changes use `ListFormat`; increase/decrease survives
  `document_from_session` and session reload.

## Fix 1 third-round review: RED/GREEN evidence (2026-09-07)

### RED: four independent blockers reproduced before the fixes

The new focused regressions were added and run against the inherited product
implementation before changing the corresponding production paths:

```text
cargo test --manifest-path packages/app-lite-native/Cargo.toml --lib \
  utf16_zero_offset_accepts_text_and_image_insertions
FAILED: NSRange(0, 0) on a non-empty document returned InvalidUtf16Range

cargo test --manifest-path packages/app-lite-native/Cargo.toml --lib \
  no_op_and_failed_commands_do_not_consume_history_or_revision
FAILED: the second Clear incremented revision (left 3, expected 2), so one
        no-op command consumed a visible history step

cargo test --manifest-path packages/app-lite-native/Cargo.toml --lib \
  empty_text_delta_is_not_a_visible_command
FAILED: the zero-length insertion at offset zero returned InvalidUtf16Range

cargo test --manifest-path packages/app-lite-native/Cargo.toml --lib \
  list_item_indents_round_trip_independently
FAILED: the second ordered item returned indent 0 instead of canonical indent 2
```

The adjacent-image RED condition is the range-only ambiguity: deleting either
image from the same old/new U+FFFC strings produces only a one-character
`NSRange`, with no resource identity. The regression therefore asserts the new
projection identity map for both deletion directions, rejects an identity
mismatch before mutating the session, and checks undo restoration. The
terminal empty-list carrier regression is exercised through the production
selection-refresh path, not by adding a canonical placeholder character.

### GREEN: focused and locked gates

```text
cargo test --manifest-path packages/app-lite-native/Cargo.toml --lib
80 passed; 0 failed

cargo test --manifest-path packages/app-lite-native/Cargo.toml --bin joplin-lite-native
36 passed; 0 failed

cargo test --locked --manifest-path packages/app-lite-native/Cargo.toml --all-targets
80 library + 36 app + 7 lifecycle tests passed; 0 failed

cargo clippy --locked --manifest-path packages/app-lite-native/Cargo.toml \
  --all-targets -- -D warnings
passed

cargo fmt --manifest-path packages/app-lite-native/Cargo.toml -- --check
passed

git diff --check
passed
```

### Third-round design and scope

- The live model still has exactly one incremental history owner: the
  `TextDocument` undo manager with a 200-entry limit. `run_edit_command` now
  stages the sidecar pre-state and only commits it, clears redo, and increments
  revision after a changed command succeeds. No-op deltas/Clear and bounded
  paragraph no-ops return before history creation. Failed commands restore the
  sidecar; a marked partial model edit is rolled back through the just-closed
  TextDocument edit block. No full-document snapshot stack was introduced.
- `utf16_range` seeds both endpoints at offset zero and scans only exact UTF-16
  boundaries, so insertion before ordinary text, emoji, or an image sentinel
  applies while a mid-surrogate boundary still rejects. Live replacement text
  containing U+FFFC remains rejected; image removal has its own semantic path.
- Per-item list nesting is stored in each item's `BlockFormat.indent`; the
  shared `ListFormat` is no longer rewritten once per item. Consecutive items
  retain one native list identity and ordered numbering, while
  `document_from_session` prefers the item block indent and round-trips mixed
  list/checklist/nesting semantics.
- Renderer output now carries each attachment's addressable UTF-16 offset and
  resource ID. AppKit live sync uses that stable projection map plus the actual
  edit range to delete exactly A or B from adjacent anchors, updates offsets for
  ordinary text deltas, and keeps inserted-image mappings synchronized from
  storage. Unknown/mismatched identity fails closed before semantic mutation.
- Empty terminal/solo blocks remain zero-character projections. Their native
  paragraph carrier is stored by the shared load/refresh install helper and is
  reapplied when a collapsed selection moves onto the exact carrier offset;
  canonical text, model history, and SQLite never receive a fake character.
