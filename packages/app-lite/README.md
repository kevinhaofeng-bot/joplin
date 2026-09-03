# Joplin Lite foundation

Joplin Lite is an isolated macOS Tauri foundation. It currently starts a
three-pane shell and reports only the location of its own application profile.

## Local commands

In a supported full-repository environment, run these commands from the
repository root:

```bash
corepack yarn install
corepack yarn workspace @joplin/app-lite test
cargo test --manifest-path packages/app-lite/src-tauri/Cargo.toml
corepack yarn workspace @joplin/app-lite build:web
corepack yarn workspace @joplin/app-lite dev
```

For this Node 23 partial clone, installation must first use:

```bash
corepack yarn install --mode=skip-build
```

Do not use ordinary `corepack yarn install` in this environment: its
repository-wide postinstall fails under Node 23 TypeScript strip-only
semantics, and `wasm-pack` also times out while downloading from GitHub. The
skip-build installation only prepares dependencies; it does not satisfy any
acceptance gate. Run the app-lite test, web build, Rust test, Rust formatting,
and Clippy verification commands individually afterward.

## Safety boundary

This foundation uses the Tauri application-data directory for
`com.kevinhao.joplin-lite`. It does **not** open the official Joplin profile,
read notes, start sync, expose the Data API, or migrate data. It does not yet
implement note CRUD, E2EE, OCR, or access to the real Joplin profile.

The next plan is the **compatibility-fixture and sync-sidecar plan**. That work
must keep this isolated, no-existing-data boundary until its own compatibility
and migration acceptance criteria are met.
