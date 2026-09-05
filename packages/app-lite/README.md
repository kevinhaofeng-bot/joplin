# Joplin Lite foundation

Joplin Lite is an isolated macOS Tauri foundation. It currently starts a
three-pane shell and reports only the location of its own application profile.

## Local commands

In a supported full-repository environment, run these commands from the
repository root:

```bash
corepack yarn install
corepack yarn workspace @joplin/app-lite test
corepack yarn workspace @joplin/app-lite test:watch
corepack yarn workspace @joplin/app-lite tsc
cargo test --manifest-path packages/app-lite/src-tauri/Cargo.toml
corepack yarn workspace @joplin/app-lite build:web
corepack yarn workspace @joplin/app-lite dev
corepack yarn workspace @joplin/app-lite build:native
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

`test` is the terminating command used by workspace aggregation; use
`test:watch` only for interactive development. `build:web` is likewise the
frontend-only build. Native Tauri packaging is deliberately opt-in through
`build:native`: this experimental workspace has no generic `build` script, so
the repository's aggregate build workflows do not require Rust or Tauri.

## Safety boundary

This foundation uses the Tauri application-data directory for
`com.kevinhao.joplin-lite`. It does **not** open the official Joplin profile,
read notes, start sync, expose the Data API, or migrate data. It does not yet
implement note CRUD, E2EE, OCR, or access to the real Joplin profile.

The compatibility-fixture and sync-sidecar work is implemented while keeping
this isolated, no-existing-data boundary. Its compatibility and migration
acceptance criteria remain prerequisites for any real-data operation.

The compatibility sidecar now exists as a supervised, testable boundary, but
the application does not start it yet. This phase uses no real Joplin profile,
network, server, credentials, or user data. Local canonical profile creation
and Note/Folder/Tag CRUD are the next plan. Final packaging will bundle a
fixed Node runtime rather than depend on the user's system Node installation.
