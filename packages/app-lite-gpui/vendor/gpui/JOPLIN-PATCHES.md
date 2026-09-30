# Vendored GPUI 0.2.2

Copied from the crates.io `gpui` 0.2.2 package (src, build.rs, resources,
README, license); example and test target declarations were removed from
Cargo.toml because their sources are not vendored.

Local changes are listed here, each in its own commit after the unmodified
import.

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
  Underline and strikethrough stay on the line's baseline and run across a
  script boundary.
- The test platform's `NoopTextSystem` advances script runs at their size, so
  app tests can check the geometry. Linux and Windows shaping ignore the
  script (`script: None`); Joplin Lite ships on macOS.

Tests: `platform::mac::text_system::tests::test_layout_line_script_run_uses_its_own_size_and_baseline`
and `..._adjacent_scripts_keep_their_own_script_per_glyph` (real CoreText).
