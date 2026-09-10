# Evernote 11.32.5 program and `main.js` reconstruction

Date: 2026-09-09 (reconstruction artifact updated 2026-09-11)

Installed build: `11.32.5` / `20260830093737`

Status: direct bundle inspection complete for the desktop orchestration, Conduit storage, local search and rich-text persistence paths

Scope: read-only inspection of the installed application, extracted package, source maps and aggregate local-store shape. This document records names, boundaries and behavior; it does not copy application source or account content.

## 1. What “reconstruct `main.js`” means

The minified `main.js` is not the Evernote editor. It is the Electron main-process bundle and owns the application shell:

- single-instance and application lifecycle;
- privileged protocols and deep links;
- main, login, popup and update windows;
- the renderer broker and IPC routing;
- creation and shutdown of the hidden Conduit worker;
- crash handling, update plumbing and final flush on quit.

The actual note editor is loaded by renderer chunks and `@evernote/common-editor`. Therefore a useful reconstruction has two deliverables:

1. recover the process, storage and message boundaries from `main.js` and Conduit packages;
2. recover editing transactions and behavior from renderer chunks and the complete common-editor source map.

Beautifying one 32 MiB file without recovering these boundaries would not explain the product.

## 2. Evidence and reproducibility

| Artifact | Exact bytes | SHA-256 |
| --- | ---: | --- |
| `Contents/Resources/app.asar` | 457,495,548 | `b39c8f4fa931ae5d9df546af22e606aea16450bd9502569c15c0d5a53e401fc4` |
| extracted `main.js` | 33,444,766 | `4c20465be7ddbecbebc5b35bf9a63d6244199006ac9e1e08a24b9f9875291bf1` |
| Terser-beautified `main.js` | 51,067,964 | `f2134bf7e083523a1eb4326d7d3ab694bfc14e9f9aeee9fc6f724d037278401e` |
| `@evernote/common-editor/ce.js.map` | 26,054,492 | `41e05a1bb99e58f04026f9bc2bf1f6ad61ac37be1da2e878c4b1fe11bd2aab05` |

Package metadata identifies:

- `evernote-client` 11.32.5;
- Electron Framework 37.6.0;
- `@evernote/common-editor` 183.272.12;
- `better-sqlite3` `^12.2.0`;
- React 17.0.2.

The common-editor map contains 3,618 `sources` and the same number of populated `sourcesContent` entries. The extracted dependency tree also contains 6,699 source-map files. By contrast, the top-level `main.js` has only a debug identifier and no adjacent source map.

The repository helper [`index-evernote-main.mjs`](../../packages/app-lite-gpui/scripts/index-evernote-main.mjs) makes the module boundary pass reproducible without storing Evernote code:

```bash
node packages/app-lite-gpui/scripts/index-evernote-main.mjs \
  /tmp/evernote-main-11.32.5.beautified.js
```

It emits module IDs, line spans, logger labels and selected architectural markers. On this build it identifies 2,843 Webpack modules. Two IDs are written by Terser as numeric exponent literals—`7e3` and `98e3`—and must be canonicalised to `7000` and `98000`; an earlier decimal-only pass incorrectly reported 2,841.

### 2.1 Readable source delivery

The full local reconstruction is generated outside the Git worktree at:

```text
/Users/kevinhao/Projects/joplin-reconstruction/evernote-11.32.5/main-readable
```

It contains:

- a 94-line readable CommonJS entry at `src/main.js`;
- 2,843 split and dependency-linked modules under `src/modules/`;
- 589 evidence-backed semantic module filenames, with the numeric module ID retained as a stable prefix;
- 8,571 dependency-graph edges and 8,659 verified relative `require()` links, including entry links;
- input/output hashes for every module in `manifest.json`;
- a sortable `module-map.csv`, `dependency-graph.json` and full checksum manifest;
- the complete Webcrack full-bundle reconstruction and bundle metadata under `diagnostics/` for lossless auditing.

Every delivered module parses as CommonJS or ESM with Babel's unambiguous mode. One Webcrack transform produced an illegal strict-mode/default-parameter combination in module `73180`; the reconstruction pipeline conservatively restores the original equivalent ordinary parameter plus explicit `undefined` default assignment. All 2,850 checksum entries pass after that repair.

The editor source map has also been extracted separately to:

```text
/Users/kevinhao/Projects/joplin-reconstruction/evernote-11.32.5/common-editor-sourcemap
```

That tree contains all 3,618 embedded source files, including the original TypeScript/TSX for editor transactions, composition-safe input, lists, clipboard, layout, viewport optimization and explicit flush commands.

The self-contained delivery archive is:

```text
/Users/kevinhao/Projects/joplin-reconstruction/evernote-11.32.5-readable-source.tar.gz
SHA-256 aa04f75a3e9a4e1f89d3fa664e453eed987645802cc9a3210a81c87c69c15150
```

It contains 6,474 files: the readable main-process tree, all extracted common-editor sources, the exact original inputs and a delivery-wide checksum manifest.

## 3. Obfuscation assessment

The bundle uses normal production Webpack/Terser transformation:

- numeric module IDs;
- shortened local identifiers;
- minified control flow before beautification;
- vendor code, product code and translations merged into a common module table.

There is no observed encrypted code container, bytecode VM, runtime string decryptor or control-flow-flattening layer. Meaningful class names, logger labels, method names, URLs, IPC action names and package structure remain readable. This is inconvenient minification, not a barrier that requires speculative deobfuscation.

The largest module, `9695`, is roughly 290,000 beautified lines and is predominantly localisation data. This matters because the 32 MiB input must not be mistaken for 32 MiB of desktop orchestration logic.

## 4. Reconstructed process architecture

```text
Electron main process
  entry + ApplicationController (module 62264)
    ├─ window controllers / tab shell / menus
    ├─ typed renderer broker (module 4529)
    └─ Conduit Electron main bridge
          │ MessagePort + legacy typed IPC
          ▼
Hidden Conduit worker BrowserWindow
    ConduitCore
      ├─ BetterSQLite3 graph, metadata, FTS and mutation queues
      ├─ per-note Yjs document storage
      ├─ resource/file staging
      └─ remote services and RTE/WebSocket synchronization

Visible tab shell and renderer
    React application
      ├─ @evernote/common-editor / ProseMirror
      ├─ Yjs editor connector
      └─ lightweight note/list/search projections from Conduit
```

This is not “a web page that directly opens SQLite.” The main process deliberately keeps the visible renderer away from the database and runs the data layer in a separate hidden worker.

## 5. Entry point and recovered main modules

The final entry imports bootstrap modules `33448`, `23302`, `38666` and `46670`, then loads environment, settings, logging, Electron, the Conduit bridge, the system-audio capture initializer and `ApplicationController`. Module `72879` is the audio-capture initializer; an earlier name-only pass incorrectly described it as Sentry initialization.

Its visible sequence is:

1. set the Windows application ID when applicable;
2. acquire the single-instance lock;
3. refuse production remote-debugging startup;
4. configure `electron.main` logging;
5. load settings and apply a pending update;
6. initialise handlers and the broker;
7. create `ApplicationController`;
8. bind `ready`, `before-quit` and `activate`.

Key recovered modules:

| Module | Recovered responsibility |
| ---: | --- |
| `62264` | application lifecycle and top-level orchestration |
| `58919` | main-window creation, restore, crash handling and renderer bootstrap |
| `61978` | tab manager and visible renderer lifecycle |
| `4529` | typed broker bridge between WebContents and the main process |
| `54987` | application menu construction |
| `95913` | login window controller |
| `14827` | user-data move window controller |
| `20653` | force-update window controller |
| `32683` | popup-note window controller |
| `54141` | common window-controller base |

### 5.1 Application lifecycle

`ApplicationController` registers `app`, `en-cache` and `en-html` as privileged schemes, and recognises `evernote` and `en` deep links. Its ready path is explicitly split:

```text
onAppReadyPreConduit
  -> initDataLayer / initConduit
  -> onAppReadyPostConduit
```

The pre-Conduit stage establishes localisation, spellcheck, telemetry, feature rollout, protocols, middleware, logging, IPC handlers, updates and fatal-error plumbing. `initDataLayer` delegates to Conduit. The post-Conduit stage creates window controllers, observes authentication, installs shortcuts/notifications/tray behavior and attaches power/folder watchers.

Shutdown is equally deliberate: normal quit is prevented, managers and windows are asked to flush, Conduit is deinitialised, error telemetry is drained, and a five-second hard-exit guard prevents an indefinitely hung shutdown. The important product mechanism is that application quit is a persistence boundary, not merely a window close.

### 5.2 Main window and tab shell

The main window has a minimum size of 840 by 480. Its WebPreferences include `nodeIntegration=false`, `contextIsolation=true`, `webSecurity=true` and spellcheck. A preload script owns the tab shell boundary.

The controller loads a splash, then `app://evernote/boronTabShell.html`. The actual application renderer is `app://evernote/boronMain.html` and is bootstrapped through `MainWindowTabManager`. The window is shown only after preparation and restores persisted geometry and route. Arbitrary navigation is rejected; renderer crashes and unresponsive states have explicit recovery prompts.

`boronMain.js` itself is only a thin loader. Feature code lives in asynchronous renderer chunks, including large chunks such as `9089.js`, `8340.js` and `2402.js`. This is the first hard boundary showing why `main.js` alone cannot reveal editor behavior.

### 5.3 Broker and IPC

The main broker uses typed actions including `HELLO`, `GOODBYE`, `PUBLISH`, `SUBSCRIBE`, `REGISTER`, `CALL` and `MESSAGE`. It tracks per-WebContents callbacks, subscriptions and active/UI-publish state, then releases these resources with the window.

Conduit has a newer MessageChannel/MessagePort path with schema validation and a legacy typed IPC path for data and RTE/Yjs messages. Requests carry IDs, have up to three attempts and a 45-second timeout. Recently answered or timed-out request IDs are bounded by a 10,000-entry LRU. Main-to-worker bridging preserves the originating WebContents so worker responses return to the correct renderer.

The useful lesson is not the particular Electron API. It is the strict ownership and bounded-request contract between UI, data worker and remote synchronization.

## 6. Conduit worker reconstruction

The Conduit main bridge creates a hidden, taskbar-free `BrowserWindow` running `boronConduitWorker/en-conduit-electron-worker.html`. The worker enables Node integration, disables background throttling and is retried at most five times during initialization.

The main process passes:

- application version, device ID, user agent and service headers;
- a host-scoped SQLite path under `conduit-storage`;
- a host-scoped document path under `conduit-fs`;
- offline-content strategy `EVERYTHING`;
- offline search enabled with note-content indexing;
- a 10-second offline-search polling interval;
- worker and error-reporting configuration.

The worker owns the real Conduit core: BetterSQLite3, GraphDB, offline search, resources, rich-text sessions, plugins, file service and remote service clients. It sends initial documents and subsequent RTE updates back through IPC.

The BetterSQLite3 driver is synchronous inside this isolated worker, but higher layers expose async operations and an event-loop yield balancer. This avoids blocking the visible renderer while retaining the predictable performance of SQLite's synchronous API.

## 7. Local data model: confirmed correction

The installed current client does **not** primarily persist each rich note as a readable ENML/XML file.

It uses a split local representation:

| Layer | Responsibility |
| --- | --- |
| SQLite graph tables | note identity, label, snippet, timestamps, content hash/size, untitled state, thumbnail hash, attachment/task counts and sync entities |
| SQLite FTS5 tables | note metadata/title, extracted note body and attachment-search text |
| per-note `.dat` files | Yjs document state and separate YDoc metadata |
| resource storage | attachment bytes and staged uploads |
| ENML/content conversion | readable/searchable/export/service projection derived from the rich document |

`NoteDocumentStorage` stores note document bytes below `conduit-fs/.../rte/Note/internal_rteDoc/` and YDoc state under `internal_YDocState/`. Paths are sharded by note ID. Reads and writes use a keyed mutex. The document LRU is intentionally tiny—two notes—while the metadata cache is bounded at 100.

A read-only aggregate validation of the active local store found:

- 1,003 note rows;
- 1,003 offline note-content search rows;
- 1,003 rich-text document files, approximately 12 MiB;
- 1,003 YDoc metadata files, approximately 3.9 MiB;
- an empty search-index queue at inspection time.

No note text, title, account ID or user identifier was copied into this report.

This corrects the earlier loose assumption that ENML is the confirmed durable body stored at rest by the current desktop client. ENML remains important, but here it is a conversion and interoperability projection around a Yjs-backed live document.

Our product should borrow the separation, not blindly copy the binary-only truth. The GPUI application still needs a locally materialized readable/indexable representation so recovery, export and independent inspection do not depend on replaying an opaque collaboration document.

## 8. Rich-text write and synchronization path

The editor-to-storage path reconstructed from Conduit and common-editor is:

```text
ProseMirror transaction
  -> Yjs document update
  -> binary update over renderer IPC
  -> Conduit RTE session applies update
  -> local full Yjs state write
  -> update broadcast to other renderers
  -> metadata/resource/task optimistic mutations
  -> WebSocket stream or queued rteUpdateContent fallback
```

Important timing and recovery behavior:

- `LifecycleProvider` loads local state first and falls back to remote state;
- every accepted Yjs update can persist a complete `Y.encodeStateAsUpdate` snapshot;
- this desktop configuration passes `rteUpdateSaveDebounce: null`, resulting in immediate local document writes rather than a delayed editor-only draft;
- editor session metadata/resource/task flush is coalesced at one second;
- `rteUpdateContent` fallback is rolled up by note ID with a two-second buffer when realtime transport is unavailable or unsynchronized;
- a sync-step recovery attempt is throttled to five seconds when pending Yjs structs reveal divergence;
- WebSocket retry backs off to at most 32 seconds;
- session destruction performs an immediate final write when content changed.

The title is part of the YDoc, not merely a separate uncontrolled text field. A title mutation updates the Y title and then goes through the rich-text update path. Separate materialized note metadata still allows lists to render without opening the full document.

The more general lesson for the GPUI editor is a two-phase truth:

1. the input transaction must become locally durable immediately and atomically;
2. remote delivery can coalesce and retry without holding the editor hostage.

## 9. Offline search path

Offline indexing is not a full-document scan on every query. A background activity:

1. pulls the oldest pending notes in batches of 100;
2. reads their Yjs document bytes;
3. applies them to a Y.Doc;
4. converts the document to content/ENML;
5. extracts text and note links with a SAX pass;
6. transactionally replaces the note-content and link projections;
7. yields roughly 10 ms between notes.

The installed desktop configuration polls this queue every 10 seconds. Searches then query materialized FTS indexes and lightweight note fields rather than parsing editor documents. This is a major part of the perceived responsiveness that must survive the move to Rust.

## 10. What is and is not recovered from `main.js`

Recovered from `main.js` and Conduit:

- lifecycle ordering and quit flush;
- window/tab ownership;
- broker and IPC boundaries;
- hidden data-worker topology;
- database/document/resource separation;
- optimistic mutation and search projection responsibilities;
- renderer crash and worker initialization recovery.

Not recoverable from `main.js` alone:

- ProseMirror schema and commands;
- list split/merge/backspace semantics;
- composition-safe input behavior;
- mixed-selection formatting state;
- clipboard priority and sanitisation;
- image node selection, resize and adjacent-block behavior;
- toolbar command catalogue and active/mixed state.

Those are in renderer chunks and `@evernote/common-editor`; that source-map pass is the next editor-specific layer, not an optional appendix.

## 11. Consequences for the Rust/GPUI architecture

Mechanisms to adopt:

- one explicit application lifecycle with synchronous final persistence boundaries;
- one UI-independent editor transaction core;
- cheap note-card projections instead of opening documents for lists;
- separate structured document, readable/searchable projection and sync journal;
- immediate local durability followed by coalesced remote delivery;
- SQLite FTS over materialized text;
- bounded caches, batches, retries and resource budgets;
- separate resource byte storage and hash-addressed upload state;
- typed messages between the editor/UI and background persistence/sync actors.

Mechanisms not to adopt:

- Electron, WebContents, hidden BrowserWindow or Node integration;
- ProseMirror/Yjs as runtime dependencies merely because Evernote uses them;
- Evernote's complete collaboration/task/AI surface;
- Yjs binary state as the only independently recoverable representation.

The GPUI equivalent can keep this architecture in one native process using Rust actors/channels and bounded worker threads. Process count is not the essential Evernote lesson; ownership, isolation, materialized projections and durable queues are.

## 12. Next targeted reconstruction passes

1. Index renderer chunks and map their dynamic imports back to note-editor features.
2. Use the common-editor source map to recover the ProseMirror schema, command catalogue and plugin order.
3. Trace list commands through transaction creation, normalization and selection restoration.
4. Trace rich clipboard precedence for ENML/HTML/plain text/files/images.
5. Trace image-node insertion, loading, selection, resize and adjacent text positions.
6. Trace composition-safe input and explicit flush behavior into the GPUI acceptance matrix.

Each pass must end in a recorded behavior, a GPUI design consequence and an executable regression case. Merely collecting class names is not considered reverse-engineering progress.
