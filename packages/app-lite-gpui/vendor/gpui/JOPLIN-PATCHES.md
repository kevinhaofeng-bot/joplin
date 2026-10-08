# Vendored GPUI 0.2.2

Copied from the crates.io `gpui` 0.2.2 package (src, build.rs, resources,
README, license); example and test target declarations were removed from
Cargo.toml because their sources are not vendored.

Local changes are listed here, each in its own commit after the unmodified
import.

## Releasing shared Metal atlas image tiles

`platform/mac/metal_atlas.rs::remove` kept a removed image's key while other
images shared its texture, and did not release its allocator rectangle. A
second removal could release the sibling's texture, and reinserting the key
returned the old pixels instead of uploading the new image. Repeated temporary
images also consumed new textures despite available released space.

The key is now removed once before looking up its texture, and the exact tile
allocation is deallocated before decrementing the live-key count. Existing
whole-texture release and free-list behavior is retained. This is a local GPUI
platform correction, not an Evernote implementation or an editor/data-model
change. It is currently uncommitted together with its tests.

Three tests in `platform::mac::metal_atlas::tests` use an actual Metal device,
upload literal BGRA pixels and read them back: rebuilding a removed shared key,
repeated removal preserving its sibling, and twelve insert/remove cycles
reusing texture space while an anchor image stays live. All three failed on
the original implementation, then passed after the correction. The full
vendored library suite passed 91 tests with one ignored. Product-suite and
ordinary-App acceptance are tracked separately in the delivery evidence.

## Per-run script size and baseline

Superscript and subscript need text shaped smaller and off the baseline, with
their real advances, so that wrapping, caret, selection and hit testing follow
what is drawn. GPUI 0.2.2 shapes a line at one font size.

- `RunScript { size_permille, rise_permille }` (text_system/line_layout.rs),
  in thousandths of the line's font size so runs stay hashable for the layout
  cache. `TextRun`, `FontRun` and `ShapedRun` carry `script: Option<RunScript>`;
  a script change starts a new font run.
- macOS (`platform/mac/text_system.rs::layout_line`): each run's CTFont is made
  at its own size, so CoreText returns the real advances and line width; the
  line's ascent and descent include the shifted runs. CoreText may join
  adjacent spans whose fonts are equal (superscript beside subscript), so every
  glyph takes the script of the span its character came from.
- Painting (`text_system/line.rs::paint_line`): glyphs paint at their run's
  size and baseline, and the visibility check uses the shifted bounds.
- Underline and strikethrough follow the script text, as Evernote nests them
  inside `<sup>`/`<sub>` (common-editor `apps/peso/schema.ts` marks order):
  `DecorationRun` carries the script, decoration runs split where the script
  changes, a line ends there even if its style is the same, and
  `script_decoration_shift` places it relative to the script text's own size
  and baseline.
- The test platform's `NoopTextSystem` advances script runs at their size, so
  app tests can check the geometry. Linux and Windows shaping ignore the
  script (`script: None`); Joplin Lite ships on macOS.

Tests: `platform::mac::text_system::tests::test_layout_line_script_run_uses_its_own_size_and_baseline`
and `..._adjacent_scripts_keep_their_own_script_per_glyph` (real CoreText);
`text_system::line::script_decoration_tests` (line placement and where
decoration runs break).

## Nearest boundary in a line's last glyph

`LineLayout::closest_index_for_x` (text_system/line_layout.rs) compared x
only with glyph starts. Past the last glyph's start no glyph matched, so it
returned the line end even in that glyph's left half (one single-byte
character excepted). A click, an up/down move or a table drop in the left
half of a line's last character put the caret after it.

- Past the last glyph's start, it now compares the distance to that start
  with the distance to the line end (`width`) and returns the nearer,
  ties to the start, as the loop already does between glyphs. The
  one-byte special case is covered by the same comparison and removed.
- Evernote's editor resolves points with prosemirror-view `posAtCoords`
  (`dragdrop/plugin.ts` uses it for drops). For a text node,
  `findOffsetInText` (prosemirror-view `dist/index.js` in the extracted
  common-editor) takes the character under the point and returns the
  offset after it when x is at or past the middle of its rectangle, else
  before it: the nearer boundary. At an exact midpoint prosemirror goes
  right and GPUI stays left; that tie is unchanged here.

Tests (real CoreText, `platform::mac::text_system::tests`):
`test_closest_index_in_the_last_glyphs_left_half_is_its_start` (ASCII, one
and several CJK glyphs, a trailing space, é, an emoji, a ZWJ family, a
flag), `test_closest_index_with_style_runs_and_combining_marks`,
`test_closest_index_on_wrapped_rows` (non-final and final row) and
`test_closest_index_controls_that_already_hold`.
