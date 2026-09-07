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
