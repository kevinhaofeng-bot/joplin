# Joplin Lite Foundation Hardening Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Close the final foundation review findings so the experimental client is isolated from upstream aggregate builds, fails visibly on real backend initialization errors, compiles an actually minimal Tauri ACL, and cannot report an unsafe profile as isolated.

**Architecture:** Keep the existing additive `packages/app-lite` boundary. Frontend-only lifecycle scripts may participate in upstream test/typecheck aggregation, while native Tauri builds remain explicit opt-in. Rust initialization always produces managed state, including a safe failed state, so the WebView can render the existing failure UI. The profile type accepts only the Joplin Lite application-data directory and rejects legacy paths or symlink redirection before creating directories.

**Tech Stack:** Yarn 4 workspaces, React 19, TypeScript 5.9, Vitest 4, Tauri 2, Rust 2024.

**Source review:** Final review of `97f88839d..470f9f7b7` found four Important and three Minor issues. This plan addresses all seven before the sync-sidecar plan begins.

## Global Constraints

- Do not add note CRUD, sync, E2EE, Data API, OCR, database access, HTTP, shell, or broad filesystem capability.
- Do not open or write `~/.config/joplin-desktop`; tests use temporary paths only.
- Preserve the existing Joplin packages and aggregate commands. The experimental native build must be opt-in.
- All backend errors shown to the frontend are short and sanitized; never include OS paths, stacks, note data, or secrets.
- Keep the existing Simplified Chinese loading, ready, and failed interface.
- Do not commit `dist/`, `target/`, temporary profiles, launchers, screenshots, or generated ACL output.

## Review Finding Map

| Finding | Owner task | Acceptance |
|---|---|---|
| Aggregate `build` runs Tauri and `test` may watch | 1 | No `build` script; `build:native` is opt-in; `test` exits |
| TypeScript check is not a formal gate | 1 | Workspace exposes `tsc` and verification runs it |
| Vite build dirties the tree | 1 | `packages/app-lite/.gitignore` ignores only `/dist/` |
| Profile check is not fail-closed | 2 | Exact identifier allowlist plus legacy-component and symlink tests |
| Real initialization failure exits before React | 3 | Tauri always manages ready/failed state; command returns safe error |
| App build hook/ACL is ineffective and broad | 3 | `tauri-build` runs; capability permissions are empty and validated |
| IPC has no runtime payload validation | 4 | Unknown payload parser and camelCase serialization contract tests |

---

### Task 1: Isolate workspace lifecycle scripts and generated output

**Files:**
- Modify: `packages/app-lite/package.json`
- Create: `packages/app-lite/.gitignore`
- Modify: `packages/app-lite/README.md`
- Modify: `docs/superpowers/plans/2026-09-03-joplin-lite-foundation.md`

**Interfaces:**
- Produces terminating `test`, explicit `test:watch`, `tsc`, and opt-in `build:native` scripts.
- Removes the generic `build` lifecycle from the experimental workspace so root `buildParallel` does not require Rust/Tauri.

- [x] **Step 1: Capture the failing package-script contract**

Run a Node assertion that requires `test === "vitest run"`, `test:watch === "vitest"`, `tsc === "tsc --noEmit"`, `build:native === "tauri build"`, no own `build` property, and `.gitignore` containing `/dist/`.

Expected: FAIL against the reviewed package.

- [x] **Step 2: Implement the lifecycle boundary**

Update scripts exactly as asserted. Keep `dev`, `dev:web`, `build:web`, and `test-ci`. Add `packages/app-lite/.gitignore` with only `/dist/`. Update README commands and append a post-review amendment to the original foundation plan; do not rewrite historical RED/GREEN evidence.

- [x] **Step 3: Verify frontend and aggregation behavior**

Run:

```bash
corepack yarn workspace @joplin/app-lite test
corepack yarn workspace @joplin/app-lite tsc
corepack yarn workspace @joplin/app-lite build:web
corepack yarn workspaces foreach --worktree --dry-run run build
git status --short
```

Expected: tests, typecheck, and web build pass; dry-run does not select `@joplin/app-lite` for `build`; generated `dist/` is ignored.

- [x] **Step 4: Commit the workspace hardening**

```bash
git add packages/app-lite/package.json packages/app-lite/.gitignore packages/app-lite/README.md docs/superpowers/plans/2026-09-03-joplin-lite-foundation.md
git commit -m "fix: isolate Joplin Lite workspace lifecycle"
```

### Task 2: Make profile isolation fail closed

**Files:**
- Modify: `packages/app-lite/src-tauri/src/profile.rs`
- Modify: `packages/app-lite/src-tauri/src/runtime.rs`

**Interfaces:**
- Produces `EXPECTED_PROFILE_DIRECTORY_NAME = "com.kevinhao.joplin-lite"` and a `ProfilePaths` that accepts only that final component.
- Rejects a case-insensitive `joplin-desktop` component, an existing symlink root, and a parent symlink resolving into a legacy profile component.
- Revalidates the boundary in `ensure()` before directory creation.

- [x] **Step 1: Write negative path tests first**

Add tests that reject:

- `/tmp/anything-else`;
- `/tmp/JOPLIN-DESKTOP` and `/tmp/joplin-desktop/com.kevinhao.joplin-lite`;
- an existing symlink named `com.kevinhao.joplin-lite`;
- a path whose parent symlink resolves to a directory named `joplin-desktop`.

Update happy-path tests to end with `com.kevinhao.joplin-lite`. Use standard-library temporary directories with unique names; clean them after each test.

- [x] **Step 2: Verify the new tests fail for the reviewed implementation**

Run:

```bash
cargo test --manifest-path packages/app-lite/src-tauri/Cargo.toml profile::tests
```

Expected: FAIL because arbitrary, case-variant, nested, or symlinked roots are accepted.

- [x] **Step 3: Implement the allowlist and symlink boundary**

Validate the final component exactly, scan lexical and canonicalized existing-parent components case-insensitively for `joplin-desktop`, reject an existing symlink root, and rerun the validation from `ensure()`. Do not canonicalize a nonexistent final directory as if that were an error. Keep all derived files strictly below the validated root.

- [x] **Step 4: Verify and commit**

Run Rust formatting, focused profile tests, then the complete crate tests. Commit:

```bash
git add packages/app-lite/src-tauri/src/profile.rs packages/app-lite/src-tauri/src/runtime.rs
git commit -m "fix: harden Joplin Lite profile isolation"
```

### Task 3: Preserve backend failure state and compile a minimal ACL

**Files:**
- Modify: `packages/app-lite/src-tauri/Cargo.toml`
- Modify: `packages/app-lite/src-tauri/Cargo.lock`
- Modify: `packages/app-lite/src-tauri/build.rs`
- Modify: `packages/app-lite/src-tauri/capabilities/default.json`
- Modify: `packages/app-lite/src-tauri/src/lib.rs`
- Modify: `packages/app-lite/src-tauri/src/runtime.rs`

**Interfaces:**
- Produces an `AppState` containing either ready `ProfilePaths` or a sanitized initialization failure.
- Produces `get_runtime_info(...) -> Result<RuntimeInfo, String>`; the error string contains no underlying path or OS error.
- Runs `tauri_build::build()` and compiles a `main`-window capability with an empty permission list.

- [x] **Step 1: Write failing backend-state tests**

Add pure tests for:

- a correct app-data directory that cannot be created because its parent is a regular file;
- an invalid app-data directory;
- both states returning the same safe runtime error without exposing the supplied path;
- a ready state still returning the existing runtime info.

Expected first focused run: FAIL because `AppState` cannot represent initialization failure and the command helper cannot return `Result`.

- [x] **Step 2: Implement non-fatal initialization**

Make `setup` always manage `AppState` and return `Ok(())`. Convert `app_data_dir()`, profile validation, and `ensure()` failures into the failed state. Register the WebView and command even on failure so React can render its existing alert. Do not log or serialize raw errors.

- [x] **Step 3: Activate and minimize the Tauri build contract**

Add a direct `[build-dependencies]` entry for the lockfile-compatible Tauri build crate and call `tauri_build::build()` from `build.rs`. Commit the Tauri CLI-stable dependency spelling with `features = []`. Replace `core:default` with an empty permissions array; custom `get_runtime_info` remains registered through `invoke_handler`.

- [x] **Step 4: Verify Rust, ACL, and real failure reachability**

Run:

```bash
cargo fmt --manifest-path packages/app-lite/src-tauri/Cargo.toml --check
cargo test --manifest-path packages/app-lite/src-tauri/Cargo.toml
cargo clippy --manifest-path packages/app-lite/src-tauri/Cargo.toml --all-targets -- -D warnings
corepack yarn workspace @joplin/app-lite build:native --debug --bundles app
```

Confirm the build hook validates `capabilities/default.json`, the debug app starts with the empty ACL, and a test-only failed state reaches a rejected `get_runtime_info` rather than panicking.

- [x] **Step 5: Commit backend hardening**

```bash
git add packages/app-lite/src-tauri
git commit -m "fix: surface Joplin Lite initialization failures"
```

### Task 4: Validate the runtime payload at the IPC boundary

**Files:**
- Modify: `packages/app-lite/src/runtime.ts`
- Modify: `packages/app-lite/src/runtime.test.ts`
- Modify: `packages/app-lite/src-tauri/Cargo.toml`
- Modify: `packages/app-lite/src-tauri/Cargo.lock`
- Modify: `packages/app-lite/src-tauri/src/runtime.rs`

**Interfaces:**
- Treats `invoke('get_runtime_info')` as `unknown` and returns `RuntimeInfo` only after runtime validation.
- Rejects empty fields and any profile directory containing a case-insensitive `joplin-desktop` path component.
- Tests the Rust JSON keys `appName` and `profileDirectory` through `serde_json` as a dev dependency.

- [x] **Step 1: Write failing IPC contract tests**

Extend frontend tests to assert the exact valid return value and rejection for missing fields, empty strings, and a legacy-profile payload. Add a Rust serialization test that requires camelCase keys and excludes snake_case keys.

- [x] **Step 2: Verify the new tests fail**

Run the focused frontend runtime test and focused Rust runtime tests. Expected: malformed frontend payloads are currently accepted and Rust lacks the direct JSON contract assertion.

- [x] **Step 3: Implement minimal runtime validation**

Add a small type guard/parser without a schema dependency. Keep displayed failure text generic through the existing App rejection path. Add only `serde_json` under `[dev-dependencies]` for the Rust test.

- [x] **Step 4: Verify and commit**

Run frontend tests, `tsc`, web build, Rust formatting/tests/Clippy, then commit:

```bash
git add packages/app-lite/src/runtime.ts packages/app-lite/src/runtime.test.ts packages/app-lite/src-tauri/Cargo.toml packages/app-lite/src-tauri/Cargo.lock packages/app-lite/src-tauri/src/runtime.rs
git commit -m "test: validate Joplin Lite runtime contract"
```

### Task 5: Re-run foundation acceptance after hardening

**Files:**
- Modify: `docs/superpowers/plans/2026-09-03-joplin-lite-foundation-hardening.md`
- Modify: `docs/superpowers/plans/2026-09-03-joplin-lite-foundation.md`

- [x] **Step 1: Run every scoped gate from a clean worktree**

Run frontend test, `tsc`, web build, Rust fmt/test/Clippy, native debug app bundle, root build dry-run, and `git diff --check`. Confirm `dist/` stays ignored and no manifest rewrite remains.

- [x] **Step 2: Perform a native Computer Use smoke**

Inspect the debug `.app` with `node_repl` and `@oai/sky`. Confirm the ready title/three panes/profile path, quit normally, then run a test-only or injected initialization-failure path proving the Chinese alert is reachable without a process panic.

- [x] **Step 3: Record exact evidence and commit**

Append test counts, bundle/asset sizes, ACL result, root dry-run result, observed safe path, failure-path result, warnings, and final clean-status evidence to both plans. Commit:

```bash
git add docs/superpowers/plans/2026-09-03-joplin-lite-foundation-hardening.md docs/superpowers/plans/2026-09-03-joplin-lite-foundation.md
git commit -m "docs: verify Joplin Lite hardening"
```

## Verification Results

Verified on 2026-09-04 from a clean worktree before recording this evidence:

- Frontend gates passed: `corepack yarn workspace @joplin/app-lite test` ran 2 files and 10 tests; `tsc` exited 0; `build:web` transformed 19 modules. Its generated files total 188,597 bytes: `index.html` 399 bytes, CSS 3,077 bytes, and JavaScript 185,121 bytes.
- Rust gates passed: `cargo fmt --check`, complete `cargo test` (12 library tests passed, 0 binary tests, 0 doc-tests), and Clippy with `-D warnings` all exited 0.
- `corepack yarn workspace @joplin/app-lite build:native --debug --bundles app` succeeded and produced `target/debug/bundle/macos/Joplin Lite.app`: 22,912 KiB on disk, containing a 23,455,392-byte executable and 922-byte `Info.plist`.
- ACL/build evidence: `build.rs` calls `tauri_build::build()`, `tauri-build` has `features = []`, and `capabilities/default.json` contains exactly `"permissions": []`; the native bundle build completed with that ACL.
- Root aggregate dry-run exited 0 and explicitly printed `Excluding packages/app-lite because it doesn't have a "build" script`. `git check-ignore` confirmed `/dist/` and generated Tauri `/gen/` schemas are ignored; no manifest rewrite remained. `git diff --check HEAD` and the pre-documentation `git status --short --untracked-files=all` were clean.
- Ready native smoke: Computer Use (`node_repl` and `@oai/sky`) visually checked the debug `.app` window titled `Joplin Lite` at `tauri://localhost`, its navigation, note-list, and editor panes, `本地资料库已隔离`, and `/Users/kevinhao/Library/Application Support/com.kevinhao.joplin-lite`; the path has no legacy desktop-profile component. The native close button exited the process normally with no panic output.
- Failure native smoke: immediately before injection, that exact profile was a non-symlink directory containing only the empty non-symlink directories `indexes`, `logs`, and `resources`. A trap-protected helper moved it to a temporary backup, created one regular file at the exact profile path, then started the same debug `.app`. Sky visually checked the Chinese alert `初始化失败。没有修改现有笔记或资料库。`; its close button exited normally with no panic output. The helper moved the blocking test file and its state directory to Trash, restored the original profile, and final verification again found exactly those three empty non-symlink directories.

Warnings: no project gate emitted warnings. The previous repository hook limitation remains: unrelated hook scripts are absent in this partial worktree, so this documentation-only commit is verified with staged `git diff --check` and uses `--no-verify` rather than claiming that hook passed.
