# Task 7 Stage C1 / Task 1 — native find engine report

Implementer commit: `d8297b14b` (local only). This is the in-memory engine/paint slice, not the Cmd-F UI or Task 7 acceptance.

| Reconstructed Evernote 11.32.5 evidence | Rust behavior | Test evidence reported by implementer |
| --- | --- | --- |
| `common-editor-sourcemap/.../find/state.ts`, `commands/find.ts`, `findnext.ts`, `findprev.ts`: ephemeral match state, literal in-note grammar, circular primary navigation | `native_editor/find.rs` caches compact UTF-8 match ranges per `NodeId + block revision`; `EditorCore` exposes fallible query, summary, next/previous/clear without document transactions | `find_literal_matches_text_blocks_without_mutating_editor_state`; `find_reconciles_changed_blocks_across_input_undo_and_redo` |
| `find/plugin.ts`: above 500 results, decorate only viewport/primary neighborhood | renderer obtains match rectangles only for shaped visible blocks, behind glyphs and separate from selection | `find_paints_only_visible_match_geometry_for_long_notes` with 600 matches |
| `find/utils/index.ts::scrollToAccent` and next/prev commands: navigate to selected visible accent | retained `EditorSurface::reveal_find_primary` performs height-index seek then exact range correction after shaping, without changing selection | mounted wrapped-prefix+image remote-hit viewport-intersection regression reported by implementer |

The engine limits query input to 4096 UTF-8 bytes; an oversized query returns `FindError::QueryTooLong` while leaving old results intact. This is an independent product safety bound, not an Evernote claim. Matcher compilation is cached per query/case setting. Ordinary edits update the changed block's matches and total, with structural edits rebuilding order. Successful editor mutation, prepared durable commit, undo/redo, IME replacement, and replay paths reconcile find state.

Implementer reported RED compilation failure before `set_find_query/find_summary/find_matches`, then GREEN; highlight RED before `render::find_highlights_for_test`, then GREEN. Reported focused `find_` 6 passed; complete GPUI exact-donor-skip 1,302 passed / 0 failed / 1 filtered; fmt check, diff check, and Release build passed with existing warnings. Controller independent verification and task review remain separate gates. No personal profile, push, tag, deployment, replacement UI, OCR/PDF, or Cmd-F panel in this commit.
