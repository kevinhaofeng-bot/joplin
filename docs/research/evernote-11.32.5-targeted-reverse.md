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

## Explicitly excluded Evernote scope

AI editing, collaboration, calendar integration, tasks, meeting recording, transcription, templates, advertising/promotions, rich web cards, PDF/spreadsheet viewers, arbitrary fonts/colors and other expansion features are not product requirements. Their presence in the bundle is evidence of Evernote's current size, not a backlog for Joplin Lite Native.

## Next targeted passes

1. Task 3: toolbar command/query state, focus transitions, responsive overflow and save-state presentation.
2. Task 4: note-card data flow, thumbnail selection, visible-item reuse and selection preservation across filtering/reorder.
3. Task 5: compare the real native window against supplied Evernote screenshots and verify that implemented mechanisms, not copied pixels, produce the intended experience.
