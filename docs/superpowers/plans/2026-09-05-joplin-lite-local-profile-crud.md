# Joplin Lite Local Profile and CRUD Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use `superpowers:subagent-driven-development` and `superpowers:test-driven-development`. Implement one task at a time and stop at each review checkpoint.

**Goal:** Safely own an isolated Joplin Lite canonical profile and expose durable local Folder, Tag, Note, and Note-Tag operations through the existing versioned sidecar and Rust supervisor.

**Architecture:** The version-pinned Node sidecar remains the only writer of canonical Joplin data and delegates schema/mutations to this fork's official `@joplin/lib`. Rust remains the supervisor and stable domain boundary; it does not write canonical SQLite. Profile ownership uses a strict marker plus a Rust-acquired POSIX advisory lease inherited by the sidecar. No task may read a real profile, use a runtime network connection, or wire the sidecar into ordinary Tauri startup.

**Tech Stack:** Node.js, TypeScript 5.9, Jest 29, `@joplin/lib` 3.7, sqlite3 5.1, Rust 2024, Tokio 1, serde/serde_json, Cargo tests.

**Spec:** `docs/superpowers/specs/2026-09-05-joplin-lite-local-profile-crud-design.md`

## Global constraints

- Preserve every sidecar framing, redaction, lifecycle, fixture, and no-startup-side-effect guarantee from the frozen compatibility phase.
- Only `packages/app-lite-sync` may import the official Joplin database/models for canonical writes.
- Never inspect or write an existing user profile. Tests use an owned temporary parent and a child named exactly `com.kevinhao.joplin-lite`.
- Runtime and tests never access Joplin Server, WebDAV, localhost HTTP, DNS, or any other network endpoint. Dependency preparation may use Yarn's normal official package channel for sqlite3 prebuilds; document an offline compiler/header fallback.
- Do not add Tauri commands or start a sidecar from `run()`.
- Do not implement Resource binary CRUD, sync, E2EE, Data API, UI, editor, or search in this plan.
- All public inputs and outputs use explicit allowlists. Never expose a raw model, arbitrary fields, SQL, paths, content, secrets, or caught error text.
- Every production behavior is written test-first. Report the exact RED and GREEN command for each task.
- Every task ends with focused tests, workspace typecheck, `git diff --check`, a targeted sensitive-string scan, a focused commit, and a clean status apart from explicitly documented review artifacts.

---

### Task 0: Preserve the completed codec bootstrap gate

**Files:**

- Modify: `packages/app-lite-sync/package.json`
- Modify: `packages/app-lite-sync/README.md`
- Create or modify: focused script/contract test under `packages/app-lite-sync/` only if needed

**Status:** Completed by commit `ad3f3862b911145c11a549e8a9d61e535731eabe`.

**Interface:** The existing `verify:clean` builds the three ignored upstream TypeScript outputs needed by the codec-only sidecar, then runs its tests and typecheck. Task 2 extends this gate for sqlite3; do not mislabel the current command as a complete database bootstrap until then.

- [x] Add a failing contract proving the missing codec prerequisites.
- [x] Determine the minimum ordered codec build chain.
- [x] Add the non-recursive `verify:clean` script without changing root install behavior.
- [x] Keep existing `test`, `test-ci`, and `tsc` focused commands.
- [x] Document the codec bootstrap reason and cost.
- [x] Re-run from targeted-clean upstream JS state: 5 suites / 32 tests and tsc pass.
- [x] Commit the focused change.

Review checkpoint: confirm the chain is actually minimal, does not rely on renderer artifacts unless proven necessary, and does not hide a missing build behind old generated files.

---

### Task 1: Add profile path and ownership policy

**Files:**

- Create: `packages/app-lite-sync/src/profile/pathPolicy.ts`
- Create: `packages/app-lite-sync/src/profile/pathPolicy.test.ts`
- Create: `packages/app-lite-sync/src/profile/profileMarker.ts`
- Create: `packages/app-lite-sync/src/profile/profileMarker.test.ts`
- Modify: `packages/app-lite-sync/src/protocol.ts`

**Interfaces:**

```ts
export const PROFILE_DIRECTORY_NAME = 'com.kevinhao.joplin-lite';
export const PROFILE_FORMAT_VERSION = 1;

export type ValidatedProfilePaths = Readonly<{
  root: string;
  database: string;
  resources: string;
  indexes: string;
  logs: string;
  settings: string;
  marker: string;
  temp: string;
  cache: string;
}>;

export async function validateProfilePath(input: unknown): Promise<ValidatedProfilePaths>;
export async function claimProfile(paths: ValidatedProfilePaths): Promise<void>;
```

- [ ] Write path-policy RED tests for relative path, wrong basename, case-insensitive legacy component, symlink root, canonical parent into legacy, and symlink database/SQLite side-file/resources/tmp/cache/settings/marker.
- [ ] Implement path validation with `lstat`/`realpath`; missing expected children are allowed, but existing children must have the exact safe type.
- [ ] Write marker RED tests for exact empty Rust scaffold adoption, successful exclusive marker creation, wrong owner/version, unknown entry, non-empty scaffold, nonexistent/fully-empty root, existing markerless database, partial marker, and marker race.
- [ ] Implement strict marker parsing and exclusive final-file creation. Do not overwrite or follow a symlink.
- [ ] Repeat critical lstat checks immediately before and after SQLite open through a reusable guard; do not claim this closes every hostile local TOCTOU race.
- [ ] Ensure every public failure is `PROFILE_INVALID` or `PROFILE_NOT_OWNED` with the fixed spec message and no path leakage.
- [ ] Run focused tests and clean verification, then commit `feat: guard Joplin Lite profile ownership`.

Review checkpoint: independently inspect every filesystem mutation, symlink decision, marker race, partial-file behavior, and cleanup path before database work begins.

---

### Task 2: Add the OS profile lease and sqlite3 native bootstrap

**Files:**

- Modify: `packages/app-lite-sync/package.json`
- Modify: `packages/app-lite-sync/src/bootstrap.test.ts`
- Modify: `packages/app-lite-sync/README.md`
- Create: `packages/app-lite-sync/src/profile/inheritedLease.ts`
- Create: `packages/app-lite-sync/src/profile/inheritedLease.test.ts`
- Modify: `packages/app-lite/src-tauri/Cargo.toml`
- Modify: `packages/app-lite/src-tauri/Cargo.lock`
- Modify: `packages/app-lite/src-tauri/src/sync_sidecar/client.rs`
- Create: `packages/app-lite/src-tauri/src/sync_sidecar/profile_lease.rs`
- Modify: `packages/app-lite/src-tauri/src/sync_sidecar/mod.rs`

**Interfaces:** Preserve ordinary `SidecarClient::start` for codec-only use. Add a macOS-only profile launch path that validates `ProfilePaths`, opens the fixed sibling lease file without following symlinks, obtains Darwin/BSD `flock(fd, LOCK_EX | LOCK_NB)`, inherits the same open-file-description into the child, and retains the Rust descriptor until child cleanup.

- [ ] Add RED tests proving a direct `sqlite3` import is absent/unusable after `skip-build`, and add `sqlite3@5.1.6` as a direct sidecar dependency because workspace hoisting is limited.
- [ ] Add a narrowly scoped native bootstrap command for the sidecar's own sqlite3 package. Do not use project-wide `yarn rebuild sqlite3`, which can trigger unrelated workspace builds.
- [ ] Extend `verify:clean` and its test so a controlled removal of the sidecar sqlite3 `.node` binding is rebuilt before a real `:memory:` open, existing sidecar tests, and tsc. Document whether the official prebuild channel or local compiler/Node headers are used.
- [ ] Write Rust RED tests for `O_NOFOLLOW`, regular-file/mode validation, first lease success, second lease `PROFILE_IN_USE`, automatic kernel release after owner FD close, and no unlink/rename of the control file.
- [ ] Implement exactly BSD `flock`, not `fcntl(F_SETLK)`, with a maintained Darwin-capable wrapper (`rustix` or equivalent). Open the original with `O_CLOEXEC`; only the target sidecar's reviewed `pre_exec` may dup to a fixed high-numbered FD and clear CLOEXEC on that child copy, including the source==target case. Never pass a lock claim through shell text or leak the FD to unrelated concurrent spawns.
- [ ] Write Node RED tests for missing/invalid inherited FD, mismatched device/inode, wrong sibling path, and successful injected verification. Production trusts that only the Rust supervisor supplies the inherited locked FD; it verifies identity, not flock state. Unit tests may inject a fake lease only into `ProfileSession`.
- [ ] Neither side may call `LOCK_UN` or use an auto-unlock guard on the shared open-file-description. Add process tests that close/kill the Rust holder, Node holder, and both in controlled orders; a new client must acquire only after every inherited FD is closed.
- [ ] Keep error details redacted and map lease contention to `PROFILE_IN_USE`, invalid/missing inherited proof to terminal `PROFILE_LOCK_REQUIRED`.
- [ ] Commit `feat: lease isolated Joplin profiles`.

Review checkpoint: verify the kernel lease, descriptor inheritance, kill/drop behavior, and cross-process contention. There must be no mtime/PID stale recovery or rename-based ABA window.

---

### Task 3: Open and close an official isolated Joplin database

**Files:**

- Create: `packages/app-lite-sync/src/profile/joplinRuntime.ts`
- Create: `packages/app-lite-sync/src/profile/profileSession.ts`
- Create: `packages/app-lite-sync/src/profile/profileSession.test.ts`
- Modify: `packages/app-lite-sync/src/handler.ts`
- Modify: `packages/app-lite-sync/src/handler.test.ts`
- Modify: `packages/app-lite-sync/src/server.ts`
- Modify: `packages/app-lite-sync/src/server.test.ts`
- Modify: `packages/app-lite-sync/package.json`
- Modify: `packages/app-lite/src-tauri/tests/sidecar_compatibility.rs`
- Create: `packages/app-lite/src-tauri/tests/local_profile_open.rs`

**Interfaces:**

```ts
export type ProfileState = 'closed' | 'open';

export interface ProfileSession {
  status(): { state: ProfileState; formatVersion: 1 };
  open(profilePath: unknown): Promise<{ state: 'open'; schemaVersion: number; formatVersion: 1 }>;
  flush(): Promise<void>;
  close(): Promise<void>;
}
```

Handler ownership changes from module-global implicit behavior to one injected/owned `ProfileSession` per `runServer`. Codec-only tests may use a session-free handler helper, but production server must not share an open database across processes/tests.

- [ ] Write RED tests for `profileStatus`, CRUD-before-open, first open with an injected valid lease, repeated open, Joplin initialization failure, shutdown close, and no profile path in responses/errors.
- [ ] Initialize `shimInit({ nodeSqlite: require('sqlite3'), appVersion: ... })`, a no-stdout logger and Node filesystem driver, the seven item classes, `Setting` constants, `JoplinDatabase`, `BaseModel`, `reg`, `loadKeychainServiceAndSettings([])`, and `BaseItem.revisionService_ = RevisionService.instance()` in the fixed spec order.
- [ ] Create only owned `resources`, `indexes`, `logs`, `tmp`, and `cache` locations below the validated root.
- [ ] Keep `Setting.autoSaveEnabled = false`; implement explicit `flush()` with `ItemChange.waitForAllSaved()` and `Setting.saveAll()`.
- [ ] After codec registration, explicitly rebind `BaseModel` and `reg` to the real database. Test both `hello→open→codec→CRUD` and `hello→codec→open→CRUD` orderings.
- [ ] Preserve classified, recoverable `PROFILE_INVALID`, `PROFILE_NOT_OWNED`, and `PROFILE_ALREADY_OPEN` responses and prove the client can continue with `profileStatus`/another valid request. Missing/invalid lease is terminal `PROFILE_LOCK_REQUIRED`. Only shim/database migration/Settings/runtime initialization failures become terminal `PROFILE_OPEN_FAILED`; close partial state and both lease FDs.
- [ ] On shutdown, stop new dispatch, flush, close DB, write one exact `{ stopped: true }` response, and naturally exit 0. On cleanup failure return terminal `STORAGE_ERROR`, still attempt remaining cleanup, and exit nonzero without accepting later frames.
- [ ] Put session cleanup in the server's `finally`, covering shutdown, EOF, input/output error, invalid fatal frame, and unhandled exception.
- [ ] Add real supervised-process persistence: existing Rust scaffold→lease→open, assert marker/database/schema exist, shutdown, start another supervised process, reopen, shutdown. Inspect only the test profile.
- [ ] Rejected profile tests require the profile root contents to remain byte-for-byte unchanged; the empty sibling lease control file may remain and is verified separately.
- [ ] Prove stdout remains NDJSON-only and no ordinary Tauri startup path changed.
- [ ] Commit `feat: open isolated Joplin profiles`.

Review checkpoint: compare the initialization sequence against current fork code, verify Note.save prerequisites, and ensure profile/session globals cannot cross test instances.

---

### Task 4: Implement Folder and Tag domain operations

**Files:**

- Create: `packages/app-lite-sync/src/domain/validation.ts`
- Create: `packages/app-lite-sync/src/domain/dto.ts`
- Create: `packages/app-lite-sync/src/domain/folderStore.ts`
- Create: `packages/app-lite-sync/src/domain/folderStore.test.ts`
- Create: `packages/app-lite-sync/src/domain/tagStore.ts`
- Create: `packages/app-lite-sync/src/domain/tagStore.test.ts`
- Modify: `packages/app-lite-sync/src/profile/profileSession.ts`
- Modify: `packages/app-lite-sync/src/handler.ts`
- Modify: `packages/app-lite-sync/src/handler.test.ts`

**Interfaces:** Use the exact FolderDto/TagDto and command shapes in the spec. Store methods receive already validated command DTOs and return only mapped public DTOs plus `created` where required.

- [ ] Add common RED tests for exact-key input allowlists, lower-case 32-hex IDs, finite timestamps, fixed redacted validation errors, and DTO field allowlists.
- [ ] Add Folder RED tests for root/child create, optional deterministic ID, idempotent replay, same-ID conflict, list excluding trash, update conflict, monotonic retained-item timestamp, user timestamp semantics, nonexistent parent, move cycle, and official recursive trash behavior under a frozen/future clock.
- [ ] Implement Folder operations through official `Folder` APIs; deterministic create uses `{ isNew: true, userSideValidation: true }`, update uses `isNew:false`, moves use `Folder.canNestUnder`, and trash uses `Folder.delete(..., { toTrash: true, deleteChildren: true })` without claiming an `old + 1` trash timestamp.
- [ ] Add Tag RED tests for trim/NFC, case-insensitive duplicate, deterministic-ID replay/conflict, list counts, stale update/delete, and delete removing note-tag relationships.
- [ ] Implement Tag operations through official `Tag.save`, `Tag.allWithNotes`/equivalent, and `Tag.untagAll`; normalize before replay comparison and use explicit `isNew:true/false`.
- [ ] Before every update/delete/trash, reload and compare `expectedUpdatedTime`; set `max(Date.now(), old + 1)` with `autoTimestamp: false` where save is used.
- [ ] Await `ItemChange.waitForAllSaved()` before every successful mutation response.
- [ ] Verify Folder/Tag mutations through reloaded canonical rows and official delete records/relationships. Do not invent ItemChange rows for model methods that do not create them.
- [ ] Commit `feat: add local folder and tag operations`.

Review checkpoint: inspect idempotency comparisons, folder recursion, cycle protection, tag deletion semantics, ItemChange durability, and absence of raw model fields.

---

### Task 5: Implement Note CRUD and explicit tag assignment

**Files:**

- Create: `packages/app-lite-sync/src/domain/noteStore.ts`
- Create: `packages/app-lite-sync/src/domain/noteStore.test.ts`
- Modify: `packages/app-lite-sync/src/domain/dto.ts`
- Modify: `packages/app-lite-sync/src/domain/tagStore.ts`
- Modify: `packages/app-lite-sync/src/handler.ts`
- Modify: `packages/app-lite-sync/src/handler.test.ts`
- Modify: `packages/app-lite-sync/src/server.test.ts`

**Interfaces:** Implement the exact NoteSummaryDto/NoteDetailDto, pagination response, Note commands, and `setNoteTags` command from the spec.

- [ ] Write Note RED tests for required active folder, optional deterministic ID with real `isNew:true` insertion, normalized idempotent replay, same-ID conflict, official validation, and fixed redaction.
- [ ] Write list/get RED tests proving stable order, default/max paging, `hasMore`, no body in summaries, body only in detail, tag IDs in detail, and deleted notes excluded.
- [ ] Write update/trash RED tests for stale expected time, strictly monotonic retained-item updated time, title/body/todo userUpdatedTime, move-only userUpdatedTime preservation, missing-key rejection, same-value `changed:false`, and official trash behavior under a frozen/future clock.
- [ ] Implement create/get/list/update/trash using official `Note.save`, `Note.load`, `Note.previews`/official query helpers, and `Note.delete(..., { toTrash: true })`; create/update explicitly use `isNew:true/false`.
- [ ] Do not return excerpts, raw Markdown analysis, encryption fields, arbitrary requested fields, or SQL rows.
- [ ] Write `setNoteTags` RED tests for duplicate input IDs, missing note, missing tag, stale note, replacement semantics, note timestamp bump, and no partial mutation after prevalidation failure.
- [ ] Prevalidate all tag IDs, call official `Tag.setNoteTagsByIds`, then save the note with a strictly newer updated_time while preserving user_updated_time so the concurrency token changes; identical normalized tag sets return `changed:false` without writes.
- [ ] Install a session-scoped promise/error tracker around `ItemChange.add` so discarded background rejections are caught without leaking details. Before each Note mutation, flush prior work and record `lastChangeId`; afterward require a new `changesSinceId(barrierId)` record with correct ID/type/change type for every affected note.
- [ ] Inject failure into a second UPDATE after an older UPDATE record already exists. Prove the old row is not accepted, the command emits terminal `STORAGE_ERROR`, content/path are redacted, cleanup runs, and the lease becomes acquirable.
- [ ] Extend the real stdio test through create Folder→Tag→Note→set tags→update→trash while checking exactly one response per request and bounded output.
- [ ] Commit `feat: add local note operations`.

Review checkpoint: inspect pagination determinism, body isolation, no-op/update rules, tag prevalidation, concurrency behavior, and every public response field.

---

### Task 6: Add typed Rust domain wrappers and restart integration

**Files:**

- Modify: `packages/app-lite/src-tauri/src/sync_sidecar/protocol.rs`
- Modify: `packages/app-lite/src-tauri/src/sync_sidecar/client.rs`
- Modify: `packages/app-lite/src-tauri/src/sync_sidecar/mod.rs`
- Create: `packages/app-lite/src-tauri/src/sync_sidecar/domain.rs`
- Create: `packages/app-lite/src-tauri/tests/local_profile_crud.rs`
- Modify: `packages/app-lite/README.md`
- Modify: `packages/app-lite-sync/README.md`

**Interfaces:** Add typed Rust request/response DTOs for profile, Folder, Tag, Note, pagination, and mutations. Keep the low-level generic `request` non-public outside the module if possible; callers use typed methods.

- [ ] Write RED tests mapping every known domain error code to a distinct fixed `SidecarErrorKind` and fixed Rust-owned Chinese message.
- [ ] Recoverable domain failures must leave `SidecarClient` in `Ready`; `PROFILE_LOCK_REQUIRED`, `PROFILE_OPEN_FAILED`, and `STORAGE_ERROR` must set `Failed` and reap. Unknown code or structurally invalid result is also fatal.
- [ ] Add serde DTO tests that reject missing/wrong-type fields and ignore no unexpected success shape; use `deny_unknown_fields` on protocol/domain result DTOs where compatible with the explicit contract.
- [ ] Implement typed methods for status/open, Folder, Tag, Note, tag assignment, and shutdown. No method accepts arbitrary field maps.
- [ ] Add a true process integration with an artificial temporary profile:

```text
start -> status(closed) -> open -> create folder -> create tag -> create note
-> set tags -> stale update gets CONFLICT and client stays Ready
-> valid update -> shutdown -> process exits
-> restart -> open same profile -> list/get exact persisted DTOs -> shutdown
```

- [ ] Add a second-process lock integration proving one owner and successful reopen after normal release.
- [ ] Harden shutdown tests: success requires exact stopped result, natural exit within the shared 5-second budget, and exit code 0. Cover success-frame-then-exit-1, success-frame-then-hang, STORAGE_ERROR-then-exit, EOF, and stdio failure cleanup.
- [ ] Assert no fixture content/path/secret appears in all public Rust errors.
- [ ] Re-run all existing Rust sidecar lifecycle regressions, including prompt shutdown with stdin open and drop/kill cleanup.
- [ ] Update READMEs with the implemented boundary and explicitly list sync/E2EE/resources/UI as still absent.
- [ ] Commit `feat: expose local Joplin CRUD to Rust`.

Review checkpoint: validate Rust does not own canonical SQL, only recoverable domain errors preserve Ready, terminal/protocol errors fail closed, OS lease FDs are always released, and restart proves disk persistence rather than in-memory state.

---

### Task 7: Whole-branch verification and milestone tag

**Files:**

- Modify only defects and documentation found by the whole-branch review
- Do not add planned next-stage features

- [ ] From a targeted-clean generated state, run the new single-command sidecar verification.
- [ ] Run all Node sidecar tests and tsc.
- [ ] Run app-lite frontend tests, tsc, and web build.
- [ ] Run Rust tests including both real-process integrations, `cargo fmt --check`, Clippy with warnings denied, and `cargo metadata --locked`.
- [ ] Build the native arm64 Tauri release executable and inspect its architecture.
- [ ] Run the no-startup-side-effect source/runtime gate: no sidecar launch, database creation, network connection, or Tauri command during ordinary startup.
- [ ] Run profile contamination checks, ignored-output checks, secret/URL/profile scans, `git diff --check`, and clean status.
- [ ] Obtain a whole-branch architecture/code review. Resolve every Critical and Important issue through Luna with new RED/GREEN evidence.
- [ ] Push `codex/joplin-lite-local-crud`, open/update a PR against `codex/joplin-lite-core-notes`, and create an annotated milestone tag only after all gates pass.

Expected tag name: `joplin-lite-v0.3.0-local-crud`.

The tag message must state that it is an isolated local CRUD milestone and does not claim production sync, E2EE, Resource binary support, Data API, UI/editor, search, or real-profile migration.
