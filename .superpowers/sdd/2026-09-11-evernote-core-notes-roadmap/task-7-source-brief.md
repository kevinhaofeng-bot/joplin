# Task 7 source-first brief: local search, suggestions, and find in note

Status: pre-implementation reverse-engineering evidence, 2026-09-12.

Scope: fast offline search for a personal note library: titles, canonical visible
body text, notebook/stack/tag/date/attachment filters, attachment filenames and
extracted text, recent queries, saved searches, contextual suggestions, and
find-in-note. Evernote AI/semantic-answer services, business spaces, tasks, and
calendars are deliberately outside this task.

## Evidence boundary

The installed Evernote 11.32.5 package exposes two complementary source levels:

- the reconstructed desktop `main.js` has readable module boundaries, exported
  symbols, SQLite migrations, SQL query construction, and control flow. Local
  variable names that remained minified are not treated as recovered source;
- `common-editor.ce.js.map` contains original `sourcesContent` for the editor's
  find commands, plugin state, match mapping, and UI. These TypeScript paths and
  symbols are direct source evidence;
- the targeted Boron renderer reconstruction preserves the global Search UI
  component graph and original CSS `sourcesContent`.

The Rust implementation must reproduce the observed contracts, not the Electron
or React mechanism. Where this product deliberately improves Chinese substring
search or bounds memory more tightly, the report must label that as an independent
product decision.

## Source-to-Rust behavior map

| Evernote source and observed behavior | Rust replication target | Required mutation-sensitive verification |
| --- | --- | --- |
| `main-readable/src/modules/56535__module-56535.js::SearchRepositoryImpl`: saved searches and `suggestedEntities` are local repository witnesses; semantic search, answers, and AI filter creation are separate remote methods. | `app-lite-core::search` owns an entirely local search path. Saved searches and offline suggestions ship without any AI dependency or network fallback. | Disable the network and search a populated profile. Results, history, filters, and saved searches remain complete; a test that routes ordinary search to any remote adapter fails. |
| `main-readable/src/modules/36175__module-36175.js` and migrations `91796`, `12876`: title metadata, note content, notebooks, tags, workspaces, and tasks use separate FTS5 tables maintained by insert/delete/update triggers. Later suggestion metadata tables use `tokenize="trigram"`. `41774` adds an explicit indexing queue/state and removes content/link rows when a note is deleted. | Keep authoritative notes/resources separate from derived FTS tables. Repository commits enqueue one replace-or-delete job in the same transaction; a bounded worker updates title/body and extracted-resource indexes. The planned dual `unicode61` + trigram strategy for mixed Latin/Chinese is a local product decision built on the source-backed separation. | Save, rename, trash, restore, and purge under forced interruption. Reopen and prove authoritative content is intact, queue work is idempotent, deleted text is absent, and Chinese substring plus Latin word queries return the expected stable NoteIds. |
| `main-readable/src/modules/83028__module-83028.js::searchNote`: parsed query AST is compiled into SQL; ordinary terms union title, body, task, attachment extracted text, and calendar matches. Relevance mode uses FTS5 `bm25` and sums scores by NoteId; normal results return only note projection metadata. Offset/limit are applied in SQL and the implementation enforces a hard result ceiling. | Query compilation unions title, body, attachment filename, and extracted attachment text, groups by NoteId, and returns `SearchHit { NoteListItem, snippet, matched_resource }`. Tasks/calendars are omitted. Paging and a hard ceiling are mandatory; canonical HTML and blob bytes never enter list hits. | A fixture where one note matches title+body ranks above one-field matches. SQL explain/projection tests reject body/blob hydration, duplicate NoteIds, unbounded result sets, and unstable ties. |
| `83028::implementedFieldOperators` plus its field-operator switch: title, notebook/ID, stack, tag/ID, created, updated, resource filename/MIME, link and other metadata operators are parsed distinctly; negation, prefix values, and `shouldMatchAll` have explicit branches. | MVP parser supports quoted phrases, negation, notebook, stack, tag intersections, created/updated ranges, trash, attachment presence, filename, and MIME. Unknown or malformed operators remain searchable text instead of silently producing an empty query. | Golden parser fixtures cover escaped quotes, Chinese punctuation, mixed Latin/Chinese, invalid filters, multiple AND tags, negative filters, and round-tripping a human-readable query string. Mutating an unknown token into a filter must fail the fixture. |
| `main-readable/src/modules/08905__module-8905.js::suggestEntities`: positive, negated, and valid field-operator nodes are separated; suggestions are independently built for tags, stacks, authors, titles, notebooks, and history, then unioned and ordered by updated descending and label ascending. Prefix `LIKE` augments some FTS paths. | A typed `SearchSuggestion` enum combines recent queries, recent notes, notebooks, stacks, and tags. The current route supplies an optional context filter, but suggestions never mutate the active route until committed. | Typing a prefix yields deterministic grouped suggestions; arrow navigation and commit use stable IDs. Cancelling restores the exact previous route/selection. Rename entities while the palette is open and prove selection follows ID rather than row/label. |
| `08905::D`, `17897__module-17897.js::addSearchHistory/deleteSearchHistory`, and migration `12876`: history is timestamped, deduplicated by upsert in the current schema, ordered newest first, prefix matched case-insensitively, and capped to 128 rows. Deletion removes stored prefixes as well as the full query. | `SearchHistory` is a bounded local table with normalized dedupe, recency ordering, prefix lookup, and explicit delete/clear operations. Empty or cancelled input is never recorded. | Insert 200 queries, repeat one with different case, delete a query/prefix, reopen, and verify the cap/order/dedupe contract without touching saved searches. |
| `main-readable/src/modules/48353__module-48353.js`, `59009__module-59009.js`, and `98189__module-98189.js`: attachment filename/MIME and extracted `searchText` have separate FTS tables with insert/delete/update triggers and a rebuild path. `83028` joins extracted attachment matches back to `parent_Note_id`. | Resource metadata is indexed immediately; OCR/PDF text is versioned derived data produced asynchronously. Search can identify both the note and matched resource. Extractor failure is visible and retryable but never blocks note save. | Insert image/PDF, search filename before extraction, then extracted text after worker completion. Replace/delete the resource and prove stale text disappears. Crash between extraction and index publication must resume without duplicate hits. |
| `renderer-readable/chunks/9435.js`, Search modules around CSS module `303150` and `747863`: global Search is a fixed overlay at `top: 15vh`, `min(80vw, 900px)` by `min(80vh, 660px)`, with a dim backdrop, 16/24 input typography, 270x28 category tabs, 32px filter rows, contextual pills, keyboard selection, and a horizontally scrolling category strip. | GPUI mounts one modal search palette above the existing library shell. It preserves AppModel route/NoteId state underneath, supports full keyboard navigation, and uses typed filters/pills rather than encoding UI state in display strings. AI-specific tabs/styling are omitted. | Mounted narrow/wide tests verify geometry, backdrop dismissal, focus return, arrow/Tab/Enter/Escape behavior, and that opening/closing Search neither remounts `NoteSession` nor changes editor history. |
| `common-editor.ce.js.map` original source `src/apps/peso/modules/find/commands/find.ts::execCommand`: find state is separate from document content; options include case handling, keeping results across updates, official-search grammar, and primary-highlight control. Empty input clears the active search. | `native_editor::find::FindState` is ephemeral presentation state outside canonical HTML and undo history. It owns query, case mode, ordered matches, primary index, and viewport decorations. | Open/find/clear around an edited note and assert canonical HTML, save generation, and undo depth are unchanged. Removing the ephemeral-state boundary must make the test fail. |
| Original `find/state.ts::FindPluginState`: matches are ordered, navigation is circular, primary position is mapped through document transactions, only changed text blocks are rescanned, and deleted matches disappear. Read-only embedded matches are counted separately. | Editor transactions map unaffected match ranges and rescan only affected blocks; current match stays stable when possible. Atomic image/attachment blocks can register filename/OCR/PDF matches without pretending they are editable body ranges. | Edit before/inside/after the primary match, undo, delete a matching block, and navigate next/previous. Verify correct mapped ranges, circular order, no full-document allocation on a one-block edit, and no replacement of read-only resource matches. |
| Original `find/commands/findnext.ts` and `findprev.ts`: next/previous navigation is circular and scrolls the selected accent into view; `find/commands/closeFindInNote.ts` closes only UI while preserving active highlights. `findInNoteUiPlugin.tsx` mounts the fixed panel outside the editable document so editor reconciliation cannot destroy it. | Cmd-F opens a retained GPUI overlay owned by the editor shell, not by a document block. Closing the panel may preserve highlights until explicit clear; focus returns to the editor. Primary-match scroll targets block geometry rather than changing selection or content. | Close/reopen preserves match count/index, explicit clear removes decorations, note switch tears down old find state, and rapid editor repaint/image hydration cannot dismiss or duplicate the panel. |
| Original `find/utils/index.ts::createNoteSearch/createFindInNote`: library-search highlighting respects word starts and special CJK handling, while in-note find matches arbitrary substrings. The source explicitly distinguishes the two grammars. The editor viewport optimization is activated for more than 500 matches. | Keep library-query semantics and literal in-note substring semantics separate. For more than 500 in-note matches, retain full ordered match metadata only in compact form and materialize decorations for the visible viewport plus a small overscan. | A note with over 500 matches reports the exact total, navigates to offscreen matches, and keeps visible decoration count bounded. Chinese literal find must match inside a text run even when library word search does not. |

## Architecture rulings before implementation

1. Search indexes are disposable projections. Notes, canonical HTML, resource
   metadata, and original blobs remain authoritative and sufficient to rebuild.
2. A note save commits its search-queue replacement atomically, but extraction and
   FTS work run after local save on bounded background workers.
3. Global Search is `LibraryRoute::Search`, not a separate note-list implementation.
   It consumes the same projection, selection, virtualization, and thumbnail path
   established by Tasks 3, 5, and 6.
4. Local search never silently calls a remote or AI service. Semantic search can be
   reconsidered later only as an explicit optional feature, outside this roadmap.
5. Library search and find-in-note intentionally use different matching grammar.
   Search optimizes recall/ranking/filtering; Cmd-F is predictable literal text
   navigation inside the current note.
6. OCR/PDF text is versioned derived data tied to `ResourceId` and blob hash. A new
   extractor version queues re-extraction without mutating note content.
7. Search status is separate from local save and sync status. `已保存` cannot imply
   that extraction or indexing has completed.

## Implementation order

1. Finish and review Task 6 organization/query state, then freeze its stable
   `LibraryRoute` and projection contracts.
2. Add failing parser, history, title/body FTS, Chinese substring, filter, ranking,
   and bounded-paging tests.
3. Implement immediate title/body indexes and the idempotent indexing queue.
4. Mount the global Search palette on `LibraryRoute::Search` using the existing
   virtualized list and NoteId selection.
5. Implement Cmd-F match state, transaction mapping, viewport-bounded decorations,
   keyboard navigation, and shell-owned overlay.
6. Add filename/MIME search, then isolate OCR/PDF extraction as the second commit.
7. Run M2 acceptance with the real 1,662-note profile copy, record p50/p95 latency,
   resident memory, index size, pending/error counts, and exact build hash.
