# Evernote 11.32.5 targeted implementation notes

Date: 2026-09-07  
Installed build: `11.32.5` / `20260830093737`  
Scope: read-only inspection of the installed application bundle and source maps. User note contents are out of scope.

## Evidence surface

- `/Applications/Evernote.app` is approximately 906 MB; `Contents/Resources/app.asar` is approximately 436 MB.
- `app.asar` contains `node_modules/@evernote/common-editor/ce.js.map`, source-map version 3, with 3,618 source entries.
- Relevant source paths include `changesplugin.ts`, `notelayoutplugin.ts`, `viewportoptimizationplugin.ts`, the explicit `flush.ts` command and `CompositionSafeInput/index.tsx`.
- The inspected editor package is `@evernote/common-editor` 183.272.12. Evidence is used as a behavioral/architectural reference only; source is not copied into this project.

## Mechanisms to reproduce natively

### Change and save lifecycle

- A dedicated transaction plugin counts user-generated document changes separately from explicit flush and state-reset events.
- Serialization transactions, uninitialized editor state, read-only state, non-user transactions and explicitly skipped changes do not produce normal user-change notifications.
- A real content change emits an eager `contentChanged` state immediately, then a settled state through a trailing 500 ms debounce with a 15-second maximum wait.
- Pending asynchronous operations can veto the settled state and reschedule it.
- The editor compares serialized ENML when document size alone cannot prove a real content change, avoiding phantom note writes.
- An explicit flush command is intended for the boundary immediately before final content is queried.

Native ruling: Task 3 uses immediate dirty state, a 300 ms coalesced SQLite save, canonical-HTML equality suppression, generation tokens for stale timers, pending-resource veto and explicit flush on note switch/new/delete/window close/image insert/format commands. The exact 500 ms web debounce is not copied because AppKit/SQLite latency differs; the two-stage lifecycle is the important mechanism.

### New-note creation and first focus

- The ordinary new-note command enters a dedicated creation transaction rather than constructing an unsaved editor-only document. The transaction calls the note-create mutation first and receives a stable note ID before selection and navigation proceed.
- Successful creation clears active search filters, marks the result as newly created, selects it in the appropriate note/notebook view and only then hands the document to the editor. Optional tag application and post-create actions are sequenced after the base note exists.
- An empty note carries a localized untitled label as creation fallback, while the editor still maintains title focus and body focus as separate state. Empty-note autofocus follows the persisted `cursorStartTitle` preference; the shipped default setup uses the body.
- Creation paths for pasted text, clipboard images and attachments reuse the same durable create-first contract with explicit initial ENML and resource input instead of inventing temporary editor-only formats.

Native ruling: keep creation local-first and recoverable: flush the current note, create an empty draft row with a stable ID, clear search, select/load the new row and focus the body so typing starts immediately. The visible `无标题笔记` string remains a card/title placeholder only; it must never overwrite an empty stored title or a title the user entered. Title and body focus remain independently controllable so a future preference can switch the first cursor target without changing creation or persistence semantics. Clipboard-image and attachment creation must continue through the same resource transaction rather than a special binary note-body format.

### Composition safety

- Evernote's controlled text input tracks composition start/end and does not overwrite the DOM value while composition is active.
- Composition end cleans and commits the final value; blur clears a stuck composition latch because macOS/Electron, dictation or autocorrect can lose the normal end event.
- External value changes use a minimal diff to retain selection rather than replacing the whole value.

Native ruling: AppKit `hasMarkedText()` is the source of truth. Marked text remains only in `NSTextView`; no semantic mutation or SQLite save occurs until commit. A minimal UTF-16 delta then updates the active Rust editor model. Blur/note-switch performs a controlled flush without serializing an intermediate composition.

### Note width and scrolling

- Layout state separates configured margins/alignment/full-width choice from measured `noteWidth` and `fullWidth`.
- A resize observer uses a 30 ms debounce with leading/trailing execution and a 100 ms maximum wait.
- The editor centers a bounded note when the host is wider than the configured measure and falls back to the available width when narrow.
- Bottom padding is 30% of viewport height so the final line can scroll to the visual middle and avoid floating controls.
- Resize notifications are dispatched only when measured widths actually change.

Native ruling: retain the approved 680 pt Chinese writing measure rather than copying Evernote's configurable width verbatim; calculate it from live AppKit geometry, debounce resize briefly, retain a large bottom scroll inset, and update only on actual geometry change.

### Viewport-bounded work

- A viewport plugin tracks document positions corresponding to the visible rectangle plus small coordinate padding.
- Position synchronization is throttled through idle work at 250 ms and reacts to scrolling, document changes and note-layout changes.
- During a document transaction, tracked viewport positions are remapped through the transaction mapping instead of discarded.

Native ruling: Task 4 virtualizes note cards with `NSCollectionView`, decodes only visible thumbnails and retains a bounded cache. Long-note viewport optimization remains a measured follow-up; it must not replace TextKit pre-emptively.

### Toolbar command state and focus preservation

- The toolbar and its overflow menu are not two independent implementations. A single action catalogue supplies execution and active-state queries; toolbar items only add placement, shortcut hints and presentation.
- The visible order is a declarative sequence of grouped tool slots. Separators are emitted only between nonempty groups, while the More entry remains reachable at the end.
- Heading and list controls query the current selection and change their icon or active state from editor state instead of maintaining a second UI-only toggle state.
- Toolbar and ordinary popover mouse-down events preserve the editor selection. Focusable controls inside a popover are treated separately so their text fields can receive focus without letting the outer toolbar steal or clear the note selection.
- After a command, focus returns to the editor. Clicking an already-open menu button closes it without executing its side effect again, avoiding a false extra undo entry.
- Selection-driven refresh is scheduled for the next presentation frame and suppressed while the pointer is still dragging a selection. Selection changes, Escape and outside clicks close stale menus deterministically.
- Popovers are positioned from the selection/control anchor with edge flipping and inset shifting rather than assuming enough room above or to the right.

Native ruling: Task 3 defines one Rust command descriptor table used by both the fixed toolbar and More menu. Each descriptor owns availability, active/mixed state and execution; AppKit controls never keep independent formatting truth. Save and restore the current `NSRange` around toolbar/popover interaction, keep text-field focus only while entering a link, and return focus to the body after applying a command. Reuse commands in overflow rather than duplicating handlers, omit empty separators, close a menu without reapplying its command, and constrain native popovers to the visible window.

### Typography hierarchy

- The editor's current default heading styles are deliberately restrained: H1 is 30 px, H2 is 24 px, H3 is 18 px, and all three use weight 600.
- The note title is also 30 px / weight 600 with a 1.3333 line height. The title remains visually dominant through placement, whitespace, metadata separation and editor structure rather than an oversized body-heading scale.
- The prior H1/H2 defaults were 25/20 px, which confirms that heading scale is treated as a tunable product token rather than content structure being coupled to an arbitrarily large platform font.

Native ruling: keep the native note title at 30 pt semibold, use approximately 30/24/18 pt semibold for H1/H2/H3, and preserve a 17 pt body. Heading attributes must be scoped to text blocks and must never constrain attachment line height. Visual hierarchy comes from rhythm and spacing as much as size.

### Image nodes and surrounding flow

- An image is a dedicated resource node view with persisted natural width and height plus an optional explicit display width. The display height is derived from aspect ratio rather than inherited from surrounding text.
- The rendered image uses `max-width: 100%` and automatic height. The resource node owns loading, conversion, error, selection and resize states without storing image bytes in note markup.
- Spacing rules explicitly cover paragraph-to-image, heading-to-image, image-to-paragraph and image-to-image sibling transitions. An image following a heading is not placed inside the heading's line box.
- Image alignment and text wrapping are separate attributes. Alignment removes wrapping state, and wrapping removes alignment state, avoiding two competing presentation truths.

Native ruling: canonical HTML remains readable and keeps `:/resource-id` references, but the editor projection must present images as independent attachment blocks. Natural dimensions and the available writing measure determine display size. A heading command may not absorb a following image, and a pasted image must terminate the current text block before inserting its resource block. This directly guards against the observed thin-strip image regression when an attachment inherited H1 paragraph geometry.

### Editor model, readable format and collaboration state

- The shipped editor identifies itself as `@evernote/common-editor` 183.272.12 and directly ships ProseMirror model, state, transform and view modules.
- The same bundle contains Yjs, Y protocols, awareness handling and explicit sync-step commands. It also contains separate XML-tree and Yjs-tree handlers plus a dedicated ENML serializer.
- This is concrete evidence that Evernote does not force one representation to serve editing, synchronization and durable interchange equally. Structured editor state, collaborative state and readable serialized note content are distinct layers with conversion boundaries.

Native ruling: do not transplant ProseMirror or Yjs into the native client merely because Evernote uses them. Preserve the same separation of concerns with native components: AppKit/TextKit is the editing projection, normalized readable HTML is the durable body format in SQLite, and Loro may carry structured change history and convergence metadata for sync. CRDT bytes must not become the only recoverable representation of a note; the current HTML and searchable text projection remain locally materialized and exportable.

### Toolbar visual system and overflow

- The toolbar renders icons at 24 px, uses one-pixel separators between non-empty command groups and indicates active state with the brand colour. Tooltips carry accessible labels and keyboard shortcuts.
- Heading and list controls remain pinned because their popovers represent structural choices. Other commands come from the same action catalogue and can move between the primary row and overflow without duplicating command logic.
- The More menu is a scrollable, bounded panel (roughly 200 px wide and 260 px maximum height). Empty command groups do not leave orphan separators.
- Pointer-down on toolbar chrome preserves the editor selection; menu form controls are the explicit exception. Popovers flip and shift at window edges instead of clipping.

Native ruling: retain the existing Rust command catalogue, replace one-character placeholder labels with consistent SF Symbols where available, keep a textual block-style control, provide AppKit tooltips and accessibility labels, and move width-constrained commands into the shared More menu. Destructive note deletion belongs in restrained editor chrome or overflow, never overlapped with save status.

### Note-card projection and thumbnail policy

- The note-card data contract is a lightweight projection: note identity, title, last-edited time, editor metadata, snippet, thumbnail URL and notebook identity. The full editor document is not the list item's rendering source.
- The shipped card typography is compact and consistent: 13 px semibold title with a 20 px line, 13/20 snippet text and 12/16 date metadata. Titles clamp to two lines, snippets clamp according to available card height, and long unbroken text wraps instead of widening the card.
- The regular thumbnail slot is a fixed 76 by 76 px container with a subtle border and `object-fit: cover`. The selected card uses a one-pixel outline; hover changes the surface over 150 ms rather than introducing motion or scale.
- Card content uses 16 px horizontal padding and reserves a stable 44 px footer area for date and indicators. Loading uses a structural skeleton rather than a blank or jumping card.
- The cards view is virtualized. The shipped styles explicitly compensate for a spacer emitted by the virtualizer, and local note storage carries cached snippets and a selected-thumbnail hash so list rendering does not parse every full note body.
- Search-result cards consume the same search-word array as the editor. The title, visible snippet and tag labels are highlighted without changing stored metadata.
- When the first snippet match occurs beyond the first 30 characters, the card normalizes whitespace and derives a 120-character context window beginning about 30 characters before the match, preferring a nearby word boundary and adding leading/trailing ellipses. A shallow match keeps the ordinary beginning-of-note snippet.

Native ruling: keep the current Rust `NotePreview` projection, `NSCollectionView` reuse and visible-only thumbnail decoding. Tune the visual card toward this compact information hierarchy: stable metadata/footer geometry, 13 pt title/snippet scale, restrained selected outline and fixed cropped thumbnail slot. Search may derive a query-specific display snippet from the already-materialized `body_text`, but that presentation field must remain separate from durable content and require no canonical-HTML parsing. Avoid turning the note browser into a gallery of oversized images; the image supports recognition while title, snippet and recency remain primary.

### Offline search and relevance

- The current local schema does not treat search as a scan of serialized note documents. It materializes separate FTS5 indexes for note metadata/title, extracted note content and attachment search text. The attachment index is fed by a plain searchable-text table rather than by filenames or binary payloads alone.
- A free-text term is queried across those independent sources and the matching note IDs are combined. In relevance mode each source supplies an SQLite `bm25(...)` score; if the same note matches more than one source, the scores are summed before the final sort. Lower BM25 score is the stronger match.
- The normal query sanitizer doubles embedded quotes, wraps a term as an FTS phrase and appends `*` for prefix matching unless the user explicitly entered an exact quoted term. This provides type-ahead-friendly prefix results without concatenating raw FTS syntax.
- The query parser also has field operators for title, created/updated time, notebook, tag and attachment filename/MIME. Those operators demonstrate a layered query model, but they are not all MVP requirements for a personal lightweight client.
- The shipped final schema uses ordinary FTS5 tokenization for the search indexes. A historical suggestion migration used trigram tokenization for title/notebook/tag suggestions, but a later tokenizer migration rebuilt the note indexes; the historical trigram choice must not be mistaken for the current general-search contract.

Native ruling: keep SQLite FTS5 and the readable `body_text` projection, but stop sorting every successful search only by recency. The MVP ranking should merge safe FTS prefix matches with literal substring matches needed for CJK, de-duplicate by note ID, and rank exact title, title prefix and title substring above body-only matches, with BM25 and recency as deterministic tie-breakers. Search results and note-card previews must use the same ordering. Attachment alt/caption text already present in `body_text` remains searchable; OCR and field-operator syntax are separate product increments, not excuses to delay useful relevance now.

### Search-result highlighting and reveal

- Keyword-search completion keeps a dedicated `searchHighlightTerms` array. It combines the submitted query with any `highlightTerms` returned by the search page, removes duplicates, and clears the array when search is dismissed or navigation leaves a search-capable view.
- The note-editor host passes those terms into the common editor as presentation configuration. On editor initialization, the find plugin enters `noteSearchMode`, derives matches using Evernote search grammar rather than arbitrary substring matching, and creates inline decorations without changing the ProseMirror document.
- The highlighter treats CJK characters as word boundaries, expands Latin diacritics, ignores common English stop words, trims boundary punctuation and distinguishes note-search matching from ordinary in-note Find. Match ranges are calculated separately from the regular-expression span so boundary characters are not highlighted accidentally.
- Ordinary text matches render with a half-opacity yellow surface, a two-pixel yellow bottom edge and a small corner radius. A separately selected primary match uses orange. When a note is opened from search, the editor may scroll the first accent into the vertical centre; the primary-match navigation machinery remains distinct from durable content.
- Title, image caption, OCR/image overlays, rich-link chips and file/audio resource views reuse the same find state but register their own match geometry. This is how non-text resources participate without flattening the whole note into one display string.
- The find plugin can virtualize decorations after 500 matches and keeps the primary match plus its neighbours even outside the ordinary viewport decoration window. That threshold is a useful scale signal, not an MVP requirement for our shorter native notes.

Native ruling: keep the query in application UI state and derive UTF-16 ranges over the current TextKit projection. Apply search emphasis through `NSLayoutManager` temporary attributes only, reapply it after a render, note switch or accepted edit, and remove it immediately when search clears. Never add highlight attributes to `NSTextStorage`, the semantic editor session, canonical HTML, the undo stack or autosave generations. For the first native increment, use one restrained yellow emphasis for all matches and scroll the first body match into view when the selected result changes; primary-match navigation, OCR overlays and resource-specific highlighting remain separate increments.

### Durable sync queue and failure recovery

- Evernote's local mutation manager persists an optimistic mutation and its remote-pending copy in the same database transaction. On launch it reloads the remote-pending table first, then the optimistic table, so interrupted upsync work is not reconstructed from UI state or memory.
- Compatible consecutive mutations can be rolled up before upload. Normal upsync uses bounded batches (25 by default) and a bounded activity runtime (120 seconds by default); unfinished or retryable mutations are put back at the head of the pending queue before the activity yields.
- The sync activity queue itself is serialized into sync state. Failed activities are dehydrated, recreated with a typed retry delay and persisted again; successful or cancelled activities are removed transactionally. The connection backoff uses stepped delays of 3, 15, 15, 15, 30 and 60 seconds, with jitter after the first attempt.
- A successful upload is not immediately treated as a full round trip. The optimistic record remains until returned entity versions/deletion dependencies are observed, with a separate timeout and error path.
- Remote documents that fail validation or belong to a type unknown to the current client are stored in dedicated failed/unknown-document tables and replayed in bounded chunks on later runs. Corrupt local mutations are also moved to a quarantine table instead of being silently discarded.
- Attachment bytes are staged independently from the metadata mutation using a stable blob ID, resource hash and parent reference. A mutation retries while its file is still being staged; terminal upload failures attempt to preserve a fallback copy rather than losing the only local bytes.

Native ruling: Loro solves convergence of structured note changes, not delivery reliability. The native sync phase therefore needs a SQLite outbox and persisted activity journal around Loro updates, with stable idempotency IDs, atomic enqueue beside each local save, bounded coalescing/batches, capped backoff with jitter, explicit manual retry and restart recovery. Download failures and unknown protocol objects go to a replayable quarantine rather than blocking the entire account. Resources use a separate hash-addressed upload queue with resumable state and retain local bytes until both metadata and content are acknowledged. UI status must be derived from these durable records (last success, pending count, current phase and sanitized error), not from a transient spinner. Canonical HTML and searchable text remain materialized independently, so a damaged CRDT log or transport journal never becomes a damaged note.

## Explicitly excluded Evernote scope

AI editing, collaboration, calendar integration, tasks, meeting recording, transcription, templates, advertising/promotions, rich web cards, PDF/spreadsheet viewers, arbitrary fonts/colors and other expansion features are not product requirements. Their presence in the bundle is evidence of Evernote's current size, not a backlog for Joplin Lite Native.

## Next targeted passes

1. Task 3: apply the verified toolbar catalogue, command/query-state and focus-preservation mechanisms; continue targeted inspection only where native behavior remains ambiguous.
2. Task 4: note-card data flow, thumbnail selection, visible-item reuse and selection preservation across filtering/reorder.
3. Task 5: compare the real native window against supplied Evernote screenshots and verify that implemented mechanisms, not copied pixels, produce the intended experience.
