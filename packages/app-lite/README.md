# Joplin Lite foundation

Joplin Lite is an isolated macOS Tauri foundation. It currently starts a
three-pane shell and reports only the location of its own application profile.

## Local commands

Run these commands from the repository root:

```bash
corepack yarn install
corepack yarn workspace @joplin/app-lite test
cargo test --manifest-path packages/app-lite/src-tauri/Cargo.toml
corepack yarn workspace @joplin/app-lite build:web
corepack yarn workspace @joplin/app-lite dev
```

On a fresh clone where dependency installation must avoid package build scripts
until the local native toolchain is ready, `corepack yarn install --mode=skip-build`
can prepare the dependency tree. This is a development
environment convenience only: it does not replace the commands above or the
test, web-build, Rust-test, formatting, and Clippy verification gates.

## Safety boundary

This foundation uses the Tauri application-data directory for
`com.kevinhao.joplin-lite`. It does **not** open the official Joplin profile,
read notes, start sync, expose the Data API, or migrate data. It does not yet
implement note CRUD, E2EE, OCR, or access to the real Joplin profile.

The next plan is the **compatibility-fixture and sync-sidecar plan**. That work
must keep this isolated, no-existing-data boundary until its own compatibility
and migration acceptance criteria are met.
