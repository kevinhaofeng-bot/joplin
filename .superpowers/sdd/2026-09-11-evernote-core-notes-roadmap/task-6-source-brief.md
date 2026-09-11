# Task 6 source-first brief: organization, navigation, and list behavior

Status: pre-implementation reverse-engineering evidence, 2026-09-12.

Scope: the personal-note core only: All Notes, notebooks, stacks, tags, shortcuts,
recent notes, trash, sorting, list modes, pane persistence, and NoteId-based
selection. Business spaces, sharing, tasks, calendars, templates, and paywalls are
deliberately outside this task.

## Evidence boundary

The installed Evernote 11.32.5 package provides two different evidence levels:

- `common-editor.ce.js.map` contains original `sourcesContent`, so editor evidence
  can be tied to original TypeScript symbols.
- the desktop `main.js` has no source map. Its complete 2,843-module graph has been
  split and made readable, but minified local identifiers cannot be restored.
  Module IDs, exported names, strings, action types, SQL, and control flow below
  are direct evidence; inferred filenames are navigation aids, not original paths.

Task 6 must not claim that a GPUI visual component is a line-for-line port of an
unavailable Evernote renderer component. State transitions, query semantics, and
persisted layout behavior are source-backed. Exact native visuals and the 200 ms
motion budget are our product implementation, checked against the running app.

## Source-to-Rust behavior map

| Evernote source and observed behavior | Rust replication target | Required mutation-sensitive verification |
| --- | --- | --- |
| `main-readable/src/modules/51113__module-51113.js`: `NavBarTreeItemType` enumerates Shortcuts, RecentNotes, Notes, Notebooks, NotebookOrStack, Tags, Tag, and Trash; reducer state keeps `navBarWidth`, expanded stack IDs, and scroll location separately. Width constants are 244 default/min, 400 max, with collapsed variants. Route actions are typed (`SELECT_ALL_NOTES`, `SELECT_TAGS`, `SELECT_TRASH`, `SELECT_SHORTCUT_SOURCE`) rather than inferred from labels. | `LibraryRoute`, `NavigationState`, and persisted `PaneLayout`; stable entity IDs, not display names, identify selection and expansion. | Rename a notebook/tag while it is selected; route and editor remain attached to the same IDs. Collapse/expand and restart; widths/expansions restore without recreating `NoteSession`. |
| `main-readable/src/modules/74300__module-74300.js`: note-list state and selected note are distinct; `SET_NOTE_LIST_STATE` merges list projection state while `SELECT_NOTE_SAGA` changes `selectedNoteGuid`; list width is independent persisted state; multi-selection is an ID map. | One `AppModel` reducer owns route, projection, selected `NoteId`, multiselection, and pane widths. List refresh never derives editor identity from row index. | Reorder/filter/delete rows around the selected note and deliver a stale list completion; selection remains on the same `NoteId` or follows an explicit deterministic fallback. |
| `main-readable/src/modules/76905__note-list-view-option.js`: `CARDS`, `SNIPPETS`, `LIST`, and `TOP_LIST` are explicit modes. | MVP exposes `Cards`, `Snippets`, and `Compact` over the same projection/query. `TOP_LIST` is intentionally deferred because it adds a horizontal split without improving the personal core. | Switch all supported modes on a 1,662-note fixture; identical query ordering/selection, no body hydration, and no duplicate thumbnail requests. |
| `main-readable/src/modules/74643__module-74643.js` and `35172__module-35172.js`: All Notes defaults to Updated descending; Trash defaults to Deleted descending. Each route owns sort state rather than one accidental global order. | `SortSpec` is route-scoped and persisted. Defaults match the source-backed behavior. | Change notebook sort, visit Trash and All Notes, then return; each route retains its own sort and selected ID. |
| `main-readable/src/modules/38218__module-38218.js::noteListQueryBuilder`: notebook filter matches parent notebook; stack filter subqueries all notebooks in the stack; tag filters use `GROUP BY Note_id HAVING count = selected_tag_count` (AND/intersection); trash uses `deleted IS NOT NULL`, normal routes use `deleted IS NULL`; label sorting is case-insensitive; pagination is applied in SQL. | `app-lite-core::query` compiles one bounded projection query for every route. Stack expansion stays a navigation concern; stack filtering stays a SQL concern. Multi-tag selection is intersection, not union. | SQL-level fixtures cover notebook, stack, multi-tag intersection, trash isolation, case-insensitive title order, stable tie-breaks, and bounded pagination. Explain/query tests reject body/blob hydration. |
| `main-readable/src/modules/35422__module-35422.js`: tag state separates ordinary expanded IDs from filtered expanded IDs; selected tag IDs can be multiple; filtering, rename, move, delete, remove-from-all-notes, and note-history navigation are typed operations. `55616__show-tag-context-menu.js` disables remove-from-all-notes when reference count is zero and confirms destructive operations through action state. | Hierarchical tags use stable parent IDs. Filtering does not overwrite ordinary expansion state. Bulk tag edits and destructive operations go through repository transactions and explicit confirmation state. | Filter the tag tree, expand matches, clear the filter, and prove normal expansion is unchanged. Rename/move/delete and bulk add/remove preserve note relations atomically; cancelled confirmation is a no-op. |
| `main-readable/src/modules/63570__module-63570.js`: notebook operations include create, rename, expunge, create/destroy stack, add/remove notebook to/from stack, selection, sort, default notebook, and recents. `32965__new-note-new-notebook.js` creates in selected stack when one is active, otherwise creates a floating notebook. | Notebook/stack mutations are typed repository commands. Creation context chooses current stack when meaningful, while a floating notebook remains valid. | Create/rename/move notebook, destroy a stack without deleting its notebooks, and bulk-move notes. Each operation publishes one coherent library event and preserves valid selection. |
| `main-readable/src/modules/98783__module-98783.js` plus `95680__shortcut-action-get-shortcut-action-get-shortcut-action-for-multiple-notes.js`: shortcuts are separate ordered references to notes/notebooks/tags; add/remove logic is based on stable source IDs; trashed notes cannot be added; expansion state for shortcut stacks/tags is independent. | `Shortcut` rows reference typed entities and have explicit order. Pinning never copies a note or changes its notebook/tag membership. | Reorder and restart; shortcuts preserve order. Delete/restore source entities; dangling display is impossible. Multi-note pin/unpin is all-or-nothing and rejects trash. |
| `main-readable/src/modules/35172__module-35172.js` and `15502__show-nav-trash-context-menu.js`: Trash selection is a route; empty-trash first enumerates trash note IDs and emits one explicit operation. Online-only enablement is Evernote service policy, not a local-data requirement. | Move-to-trash, restore, permanent delete, and empty trash are separate repository commands. Local personal use must work offline; permanent deletion also obeys resource occurrence/GC rules from Tasks 2 and 5. | Offline trash/restore works. Empty trash is explicit, atomic at the command boundary, leaves non-trash notes untouched, and journal/resource recovery passes after forced interruption. |
| `00951__note-menu-actions.js`: move, edit tags, and shortcut actions handle single- and multi-selection through the same dispatched operations. | Context menu, keyboard command, and future drag/drop all call the same typed application commands. | Invoke bulk move/tag/pin through two UI entry points and assert identical repository events and one undoable selection transition where applicable. |
| `main-readable/src/modules/54193__module-54193.js`: application state exposes separate `canNavigateBack`/`canNavigateForward` flags and `NAVIGATE_TO` carries typed view plus note/notebook/stack identifiers. `62264__application-controller.js` delegates Back/Forward to the active Electron `webContents` history. | A pure GPUI app has no browser history to borrow, so `NavigationHistory` records typed route snapshots and selected `NoteId`. This is a source-backed UX contract with a native Rust mechanism, not an imitation of Electron internals. | Navigate All Notes → notebook → tag → note; Back/Forward restores route, filters, and selected IDs. A new navigation after Back truncates the forward branch. Repository/list refreshes do not create history entries. |

## Architecture rulings before implementation

1. `LibraryRoute` is the sole route truth. Sidebar rows dispatch routes; they do
   not own independent query/filter state.
2. Selection is always `Option<NoteId>`. Row indexes are transient presentation
   data and must never cross an async boundary.
3. All list modes consume one lightweight `NoteListItem` projection. Title,
   snippet, timestamps, notebook ID, tag badges, attachment flag, and thumbnail
   key are allowed; canonical body and resource bytes are forbidden.
4. Notebook/tag/shortcut mutations are repository transactions that publish one
   event only after commit. UI optimistic state cannot become a second authority.
5. Collapsing a pane changes presentation width/opacity only. The active
   `NoteSession`, editor layout entity, image hydration state, and undo history
   survive the transition.
6. During the 200 ms pane animation, thumbnail residency is computed for the
   final viewport and frozen; one settled relayout is issued at completion.
7. Evernote service-only constraints (online Trash enablement, business spaces,
   shared ownership) are not copied into the offline personal product.

## Implementation order

1. Complete Task 5 Release acceptance and commit the stable M1 baseline.
2. Add failing core query/repository tests for the mapped semantics above.
3. Implement typed organization commands and `LibraryRoute` query compilation.
4. Mount the real sidebar/list modes without replacing the active editor/session.
5. Add pane persistence and motion as an isolated final change.
6. Run the 1,662-note / 31-notebook / 64-tag / 4,238-resource scale fixture,
   then perform a fresh Release visual/interaction pass.
