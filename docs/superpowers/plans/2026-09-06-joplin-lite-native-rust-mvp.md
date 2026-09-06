# Joplin Lite Native Rust MVP Implementation Plan

## Task 1: Native package and tested local core

- Add `packages/app-lite-native` as an independent Cargo package.
- Define Joplin-compatible note IDs, note/notebook models, and typed errors.
- Add SQLite migrations, WAL configuration, CRUD, soft deletion, draft cleanup,
  and FTS5 search.
- Write focused tests for the complete note lifetime and restart persistence.

## Task 2: AppKit note creation path

- Implement the native AppKit application and window from Rust.
- Build the sidebar, note list, title field, and `NSTextView` editor.
- Wire the New Note button and `Command-N` to the same Rust action.
- Focus the editor immediately and make edits persist automatically.
- Keep local create/edit independent from search indexing and future sync state.

## Task 3: Native editor behavior and polish

- Enable native rich-text, undo/redo, copy/paste, spell checking, and standard
  macOS text behavior.
- Generate an untitled note's display title from the first non-empty body line.
- Refresh list selection/title/modified time without disrupting editor focus.
- Add explicit soft delete and a useful empty state.

## Task 4: Bundle and product-level verification

- Add a reproducible release `.app` bundle step and ad-hoc signing.
- Run the core, formatting, lint, and release gates.
- Exercise real create/edit/quit/relaunch/delete behavior.
- Inspect the process tree and linked frameworks for WebKit and Node.
- Record app size and release RSS in the package README.

## Deferred after MVP acceptance

- Joplin Server sync in Rust, including conflicts and resource transport.
- JEX import and exact compatibility rehearsal against the existing library.
- Image/file attachments and full HTML/Joplin rich-text conversion.
- Tags, notebook management, OCR, and mobile-specific work.

