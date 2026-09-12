# Task 7 C1 Task 2: source-to-native find panel

Final local code HEAD: `ce2efae5566ff769652168d595cce0b161295bf4`.

| Original Evernote 11.32.5 source read | Reconstructed native behavior | Verification |
| --- | --- | --- |
| `common-editor-sourcemap/@evernote/common-editor/src/apps/peso/modules/find/ui/findInNoteUiPlugin.tsx` mounts the panel outside `view.dom` so document reconciliation cannot remove it. | `LibraryShell::render_find_in_note_panel` owns one retained GPUI panel outside `EditorSurface` and the canonical document. | Mounted Cmd-F panel survives editor refresh; core and mounted tests prove find does not alter saved HTML/undo. |
| `find/ui/keyBindings.ts` separates Cmd-F/G/Shift-G from the global search route. | Cmd-F opens in-note literal find; Cmd-G/Shift-Cmd-G navigate. Cmd-K keeps the independent global palette; handoff is mutually visible with original focus restoration. | Mounted Cmd-F/Cmd-K→Cmd-F and Find→Cmd-K→Cmd-F→Escape paths; final Release shows Cmd-K→Cmd-F. |
| `find/state.ts`, `find/commands/findnext.ts`, `findprev.ts`, `closeFindInNote.ts` keep match state outside the document, navigate circularly, and close UI while retaining highlights. | `FindState` stays ephemeral on retained `EditorCore`; shell close hides panel but not matches; note switch clears old find. | Native 10 focused engine tests plus mounted CJK/10,000-match cases; last-match wrap and no HTML/save/undo mutation. |
| `find/ui/FindInNote.tsx` uses a distinct panel query/summary and case option. | `TitleInput` backs the query; live exact count, case switch, visible horizontal query viewport, translated IME bounds and narrow-column wrap are native GPUI controls. | Mounted 900px bounds, long query, marked Escape/reopen, marked Cmd-K→Cmd-F, and actual 10,000-match summary bounds. |

Deliberate limits: literal text-block find only; attachment filename/OCR/PDF find is not presented as editable body matches. Search suggestions/history and OCR/PDF extraction remain later Task 7 work. No Evernote Electron/React code or resources were copied into the Rust app; behavior was mapped from the listed source files.
