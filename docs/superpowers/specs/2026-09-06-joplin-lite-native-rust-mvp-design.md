# Joplin Lite Native Rust MVP Design

## Goal

Build a macOS-native note application whose shipped process contains no WebKit,
Electron, browser runtime, or resident Node.js helper. Creating and editing a
note is the product's primary reliability boundary: it must remain available
while offline and when future search, attachment, or sync work fails.

The existing Tauri 0.10 application remains a frozen compatibility reference.
The native application is added in parallel under `packages/app-lite-native`.

## Product contract

- `Command-N` and the New Note button create a local draft immediately.
- The body editor receives focus without a dialog or notebook chooser.
- Typing is persisted locally without an explicit Save command.
- A restart restores the note and its latest title and body.
- Empty abandoned drafts are removed rather than accumulated in the list.
- Search, network, and future sync failures never block create or edit.
- Chinese IME composition, selection, undo/redo, copy/paste, and drag/drop use
  the native text system rather than a custom web editor.
- Delete is explicit and reversible in the data model through `deleted_time`.

## Architecture

### Native shell

Rust calls AppKit through `objc2`, `objc2-foundation`, and `objc2-app-kit`.
The window uses native split views, native list controls, and `NSTextView`.
There is no `WKWebView` and no JavaScript application layer.

### Rust core

The core owns `Notebook`, `Note`, and repository operations. UI callbacks call
typed Rust methods rather than passing JSON messages. IDs use 32 lowercase hex
characters so later Joplin Server mapping does not require an ID migration.

### Storage

`rusqlite` opens an app-owned SQLite database in Application Support, enables
WAL, and runs explicit schema migrations. Notes store rich text in a native
round-trippable representation plus extracted plain text for title generation
and search. Deleted notes retain a deletion timestamp for later sync semantics.

SQLite FTS5 provides local full-text search. Search indexes are downstream of
the source note row: an indexing error is reported but cannot make note creation
or saving fail.

### Future compatibility seams

The shipped MVP has no Node process. Later phases add a Rust `reqwest` Joplin
Server client, JEX import, resource storage, and HTML/Joplin rich-text mapping.
Those additions remain behind repository and sync interfaces and cannot replace
the stable local write path.

## MVP interface

The initial window has three native areas:

1. A compact notebook sidebar with All Notes and Notes.
2. A note list with New Note, Search, note title, and modified time.
3. A title field and native rich-text body editor.

The interface should be quiet and familiar. It must not expose sync setup,
Markdown modes, plugin management, or advanced configuration in this MVP.

## Verification gates

- Core test: create, edit, soft-delete, restart/reopen, and FTS query.
- Draft test: blank abandoned drafts are cleaned while non-empty drafts remain.
- UI smoke: launch the signed `.app`, create with button and `Command-N`, type
  Chinese and formatted text, quit, relaunch, and observe persisted content.
- Process inspection: the app has no child Node process and no WebKit-related
  process attributable to it.
- Measure release RSS after idle and after opening/editing a representative
  note; record the values without comparing debug builds.
- `cargo test`, `cargo fmt --check`, `cargo clippy -- -D warnings`, release
  build, and ad-hoc signature verification pass.

