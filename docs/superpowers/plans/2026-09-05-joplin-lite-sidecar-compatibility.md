# Joplin Lite Sidecar Compatibility Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Build a network-free compatibility seam that lets Rust Core supervise a version-pinned `@joplin/lib` Node sidecar and prove official Joplin item serialization against checked-in fixtures.

**Architecture:** `packages/app-lite-sync` owns protocol validation and the only adapter to official Joplin item codecs. Rust owns a lazy, injected child-process client with strict frame, timeout, state, and redaction boundaries; neither side opens a real profile or network connection in this phase.

**Tech Stack:** Node.js, TypeScript 5.9, Jest 29, ts-jest 29, `@joplin/lib` 3.7, Rust 2024, Tokio 1, tokio-util 0.7, serde/serde_json, Cargo tests.

**Spec:** `docs/superpowers/specs/2026-09-05-joplin-lite-sidecar-compatibility-design.md`

## Global Constraints

- `packages/app-lite-sync` is the only new Node workspace; it must use this fork's `@joplin/lib` 3.7 series and must not copy official serialization logic.
- Do not open or write `~/.config/joplin-desktop`, any user profile, or any real note/resource.
- Do not connect to Joplin Server, WebDAV, localhost HTTP, or any other network address.
- Protocol version is exactly `1`; stdin/stdout are UTF-8 NDJSON; stdout contains protocol frames only and diagnostics use stderr.
- Maximum request or response frame size is exactly `8 * 1024 * 1024` bytes; resource binary content never appears in a frame.
- Rust startup handshake timeout is exactly `5 seconds`; normal request timeout defaults to exactly `10 seconds` and is injectable in tests.
- Errors exposed across the boundary are stable and redacted; they must not contain raw frames, fixture bodies, paths, tokens, passwords, or keys.
- The ordinary Joplin Lite application startup must not launch the sidecar or register a new Tauri command in this phase.
- All production behavior is developed test-first. Every task report must record the failing command/output before implementation and the passing command/output after implementation.

---

### Task 1: Add the protocol workspace and pure frame contract

**Files:**
- Create: `packages/app-lite-sync/package.json`
- Create: `packages/app-lite-sync/tsconfig.json`
- Create: `packages/app-lite-sync/jest.config.js`
- Create: `packages/app-lite-sync/src/protocol.ts`
- Create: `packages/app-lite-sync/src/protocol.test.ts`

**Interfaces:**
- Consumes: root Yarn workspace discovery through `packages/*`.
- Produces: `PROTOCOL_VERSION = 1`, `MAX_FRAME_BYTES = 8 * 1024 * 1024`, `RequestFrame`, `SuccessFrame`, `FailureFrame`, `ResponseFrame`, `ProtocolError`, `parseRequestFrame(line)`, `successFrame(id, result)`, and `failureFrame(id, code, message)`.

- [ ] **Step 1: Create the workspace test harness and failing protocol tests**

Use this workspace contract:

```json
{
  "name": "@joplin/app-lite-sync",
  "version": "0.1.0",
  "private": true,
  "scripts": {
    "test": "jest --config jest.config.js --runInBand",
    "test-ci": "yarn test",
    "tsc": "tsc --noEmit --project tsconfig.json",
    "start:stdio": "node -r ts-node/register/transpile-only src/main.ts"
  },
  "dependencies": {
    "@joplin/lib": "~3.7"
  },
  "devDependencies": {
    "@types/jest": "29.5.14",
    "@types/node": "18.19.130",
    "jest": "29.7.0",
    "ts-jest": "29.4.11",
    "ts-node": "10.9.2",
    "typescript": "5.9.3"
  }
}
```

`tsconfig.json` extends `../../tsconfig.json`, includes `src/**/*.ts`, excludes `node_modules`, and sets `noEmit: true`. `jest.config.js` extends `../../jest.config.base.js`, uses Node environment, transforms TypeScript with `ts-jest`, and matches `**/*.test.ts`.

In `protocol.test.ts`, cover these exact behaviors:

```ts
expect(parseRequestFrame('{"id":"r1","protocolVersion":1,"command":"hello","params":{}}')).toEqual({
  id: 'r1', protocolVersion: 1, command: 'hello', params: {},
});
expect(() => parseRequestFrame('{')).toThrow(expect.objectContaining({ code: 'INVALID_REQUEST' }));
expect(() => parseRequestFrame('{"id":"r1","protocolVersion":2,"command":"hello","params":{}}'))
  .toThrow(expect.objectContaining({ code: 'PROTOCOL_MISMATCH' }));
expect(() => parseRequestFrame('{"id":"","protocolVersion":1,"command":"hello","params":{}}'))
  .toThrow(expect.objectContaining({ code: 'INVALID_REQUEST' }));
expect(() => parseRequestFrame('x'.repeat(MAX_FRAME_BYTES + 1)))
  .toThrow(expect.objectContaining({ code: 'FRAME_TOO_LARGE' }));
expect(failureFrame('r1', 'INVALID_REQUEST', '请求格式无效')).not.toHaveProperty('stack');
```

- [ ] **Step 2: Run the focused test and verify RED**

Run:

```bash
corepack yarn install --mode=skip-build
corepack yarn workspace @joplin/app-lite-sync test src/protocol.test.ts
```

Expected: FAIL because `src/protocol.ts` does not exist or does not export the required contract. A dependency-install failure is not an acceptable RED state.

- [ ] **Step 3: Implement the minimal pure protocol contract**

Use these exact public shapes:

```ts
export const PROTOCOL_VERSION = 1 as const;
export const MAX_FRAME_BYTES = 8 * 1024 * 1024;

export type RequestFrame = {
  id: string;
  protocolVersion: typeof PROTOCOL_VERSION;
  command: string;
  params: Record<string, unknown>;
};

export type SuccessFrame = { id: string; ok: true; result: unknown };
export type FailureFrame = {
  id: string;
  ok: false;
  error: { code: string; message: string };
};
export type ResponseFrame = SuccessFrame | FailureFrame;

export class ProtocolError extends Error {
  public constructor(public readonly code: string, message: string) {
    super(message);
    this.name = 'ProtocolError';
  }
}
```

`parseRequestFrame` measures UTF-8 bytes with `Buffer.byteLength`, parses without echoing input in any thrown message, accepts only a non-array object, requires a non-empty string `id` and `command`, requires an object/non-array `params`, and rejects every version other than `1`. Use the fixed messages `请求格式无效`, `协议版本不匹配`, and `协议帧过大`.

- [ ] **Step 4: Run focused and workspace verification**

Run:

```bash
corepack yarn workspace @joplin/app-lite-sync test src/protocol.test.ts
corepack yarn workspace @joplin/app-lite-sync tsc
```

Expected: all protocol tests PASS and TypeScript exits 0 without errors.

- [ ] **Step 5: Commit**

```bash
git add packages/app-lite-sync yarn.lock
git commit -m "feat: define Joplin sidecar protocol"
```

---

### Task 2: Bind official Joplin codecs to fixtures and the stdio server

**Files:**
- Create: `packages/app-lite-sync/fixtures/v1/manifest.json`
- Create: `packages/app-lite-sync/fixtures/v1/note.txt`
- Create: `packages/app-lite-sync/fixtures/v1/folder.txt`
- Create: `packages/app-lite-sync/fixtures/v1/resource.txt`
- Create: `packages/app-lite-sync/fixtures/v1/tag.txt`
- Create: `packages/app-lite-sync/fixtures/v1/note-tag.txt`
- Create: `packages/app-lite-sync/fixtures/v1/master-key.txt`
- Create: `packages/app-lite-sync/fixtures/v1/revision.txt`
- Create: `packages/app-lite-sync/src/codec.ts`
- Create: `packages/app-lite-sync/src/codec.test.ts`
- Create: `packages/app-lite-sync/src/handler.ts`
- Create: `packages/app-lite-sync/src/handler.test.ts`
- Create: `packages/app-lite-sync/src/server.ts`
- Create: `packages/app-lite-sync/src/server.test.ts`
- Create: `packages/app-lite-sync/src/main.ts`

**Interfaces:**
- Consumes: Task 1 protocol exports and official `BaseItem`, `Note`, `Folder`, `Resource`, `Tag`, `NoteTag`, `MasterKey`, and `Revision` classes.
- Produces: `registerItemClasses()`, `decodeItem(raw): Promise<Record<string, unknown>>`, `encodeItem(item): Promise<string>`, `handleRequest(request): Promise<{ response: ResponseFrame; shouldExit: boolean }>`, and `runServer(input, output): Promise<void>`.

- [ ] **Step 1: Add the seven artificial fixtures and failing codec tests**

Use only these deterministic IDs:

```text
note       11111111111111111111111111111111
folder     22222222222222222222222222222222
resource   33333333333333333333333333333333
tag        44444444444444444444444444444444
note-tag   55555555555555555555555555555555
master-key 66666666666666666666666666666666
revision   77777777777777777777777777777777
```

Use timestamps `1788566400000` and `1788566460000`. The Note fixture title is `Fixture note`, its body contains both `第二行中文` and `![fixture](:/33333333333333333333333333333333)`, and it has `is_todo: 1`. The Resource fixture uses `mime: image/png`, `file_extension: png`, and `size: 128`. The NoteTag links the fixed note and tag IDs. The Revision targets the fixed note with `item_type: 1`, `title_diff: []`, `body_diff: []`, and `metadata_diff: {}`. The MasterKey fixture uses test-only content `fixture-master-key-metadata`, never a real key.

`manifest.json` maps each filename to its expected `type_` and the exact fields compared after round trip:

```json
{
  "note.txt": ["id", "parent_id", "title", "body", "is_todo", "created_time", "updated_time", "type_"],
  "folder.txt": ["id", "parent_id", "title", "created_time", "updated_time", "type_"],
  "resource.txt": ["id", "title", "mime", "file_extension", "size", "created_time", "updated_time", "type_"],
  "tag.txt": ["id", "title", "created_time", "updated_time", "type_"],
  "note-tag.txt": ["id", "note_id", "tag_id", "created_time", "updated_time", "type_"],
  "master-key.txt": ["id", "content", "created_time", "updated_time", "type_"],
  "revision.txt": ["id", "item_id", "item_type", "title_diff", "body_diff", "metadata_diff", "created_time", "updated_time", "type_"]
}
```

The test loads every manifest entry, calls `decodeItem`, `encodeItem`, then `decodeItem` again, and compares every named field. Add two rejection tests: an item ID `../../escape` and a Resource with `file_extension: ../png` both reject with a fixed `INVALID_ITEM` error whose message contains neither malicious input.

- [ ] **Step 2: Run codec tests and verify RED**

Run:

```bash
corepack yarn workspace @joplin/app-lite-sync test src/codec.test.ts
```

Expected: FAIL because `codec.ts` is missing. Confirm fixture parsing reached the missing production interface; fix fixture syntax errors before accepting RED.

- [ ] **Step 3: Implement the official-codec adapter only**

`registerItemClasses()` calls `BaseItem.loadClass` once for all seven official model classes. `decodeItem` delegates to `BaseItem.unserialize`. `encodeItem` resolves the class through `BaseItem.itemClass(item)` and delegates to that class's ordinary `serialize`; it must not call `serializeForSync` in this phase.

Map every official parse/validation failure to:

```ts
throw new ProtocolError('INVALID_ITEM', 'Joplin 项目格式无效');
```

Never include the caught error message or input in the public error. Keep the caught value available only as an unlogged cause if supported by the runtime.

- [ ] **Step 4: Add failing handler and stdio tests**

Cover exact command behavior:

```ts
await expect(handleRequest(request('hello', {}))).resolves.toMatchObject({
  response: { ok: true, result: { protocolVersion: 1, joplinVersion: '3.7.0' } },
  shouldExit: false,
});
await expect(handleRequest(request('decodeItem', { raw: noteRaw }))).resolves.toMatchObject({
  response: { ok: true, result: { id: '11111111111111111111111111111111', type_: 1 } },
});
await expect(handleRequest(request('encodeItem', { item: decodedNote }))).resolves.toMatchObject({
  response: { ok: true },
});
await expect(handleRequest(request('shutdown', {}))).resolves.toMatchObject({ shouldExit: true });
await expect(handleRequest(request('not-real', { secret: 'must-not-leak' }))).resolves.toEqual({
  response: failureFrame('r1', 'UNKNOWN_COMMAND', '未知命令'), shouldExit: false,
});
```

For `runServer`, use in-memory `Readable`/`Writable` streams. Verify two request lines yield two response lines, stdout data is parseable JSON only, shutdown stops later input, malformed JSON yields `INVALID_REQUEST` with response `id: ""`, and a frame over `MAX_FRAME_BYTES` yields `FRAME_TOO_LARGE` with response `id: ""` without echoing its marker text.

- [ ] **Step 5: Run handler/server tests and verify RED**

Run:

```bash
corepack yarn workspace @joplin/app-lite-sync test src/handler.test.ts src/server.test.ts
```

Expected: FAIL because handler/server entry points do not exist.

- [ ] **Step 6: Implement handler, server, and minimal process entry**

`hello` reads the version from `packages/lib/package.json` through a typed JSON import or `require`, and returns capabilities in this exact order:

```ts
['decodeItem', 'encodeItem', 'shutdown']
```

`decodeItem` requires only a string `params.raw`; `encodeItem` requires only an object/non-array `params.item`; invalid params return `INVALID_REQUEST` and `请求格式无效`. `server.ts` uses `readline.createInterface({ input, crlfDelay: Infinity })`, parses each line, writes exactly one JSON response plus `\n`, and closes after responding to `shutdown`. It must measure each raw line before JSON parsing. `main.ts` calls `runServer(process.stdin, process.stdout)` and writes only a fixed `SIDECAR_FATAL` diagnostic to stderr on an unhandled error before setting `process.exitCode = 1`.

- [ ] **Step 7: Run the Node workspace gates**

Run:

```bash
corepack yarn workspace @joplin/app-lite-sync test
corepack yarn workspace @joplin/app-lite-sync tsc
printf '%s\n' '{"id":"smoke","protocolVersion":1,"command":"hello","params":{}}' '{"id":"stop","protocolVersion":1,"command":"shutdown","params":{}}' | corepack yarn workspace @joplin/app-lite-sync start:stdio
```

Expected: tests and typecheck exit 0; smoke stdout contains exactly two JSON response lines, the first reports protocol `1` and Joplin `3.7.0`, and the second acknowledges shutdown.

- [ ] **Step 8: Commit**

```bash
git add packages/app-lite-sync
git commit -m "feat: add Joplin compatibility sidecar"
```

---

### Task 3: Add the Rust supervisor client and real cross-language round trip

**Files:**
- Modify: `packages/app-lite/src-tauri/Cargo.toml`
- Modify: `packages/app-lite/src-tauri/Cargo.lock`
- Modify: `packages/app-lite/src-tauri/src/lib.rs`
- Create: `packages/app-lite/src-tauri/src/sync_sidecar/mod.rs`
- Create: `packages/app-lite/src-tauri/src/sync_sidecar/client.rs`
- Create: `packages/app-lite/src-tauri/src/sync_sidecar/protocol.rs`
- Create: `packages/app-lite/src-tauri/tests/sidecar_compatibility.rs`
- Modify: `packages/app-lite/README.md`

**Interfaces:**
- Consumes: Task 1 protocol version/frame limit and Task 2 `start:stdio` process.
- Produces: public Rust module `sync_sidecar`; `SidecarCommand { executable, args, current_dir }`; `SidecarState`; `SidecarErrorKind`; `SidecarError`; `SidecarClient::start(command, request_timeout)`, `request(command, params)`, `state()`, and `shutdown()`.

- [ ] **Step 1: Add failing pure protocol/state tests**

Add direct dependencies:

```toml
serde_json = "1"
tokio = { version = "1", features = ["io-util", "macros", "process", "rt-multi-thread", "time"] }
tokio-util = { version = "0.7", features = ["codec"] }
futures-util = { version = "0.3", features = ["sink"] }
```

Move the existing `serde_json = "1"` entry from `[dev-dependencies]` to `[dependencies]`; do not leave a duplicate declaration.

Define these exact public enums:

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SidecarState { Stopped, Starting, Ready, Stopping, Failed }

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SidecarErrorKind {
    SpawnFailed, Timeout, SidecarExited, ProtocolMismatch,
    InvalidResponse, FrameTooLarge, Io,
}
```

Tests must verify response deserialization accepts success/failure frames, rejects a mismatched `id`, rejects a hello result whose protocol version is not `1`, and maps all public messages to fixed Chinese text without including a supplied secret marker.

- [ ] **Step 2: Run the focused Rust test and verify RED**

Run:

```bash
cargo test --manifest-path packages/app-lite/src-tauri/Cargo.toml sync_sidecar::protocol
```

Expected: FAIL because `sync_sidecar` does not exist.

- [ ] **Step 3: Implement frame types and redacted error mapping**

Use `serde(rename_all = "camelCase")` for request fields and tagged/untagged response structures matching Task 1. Set Rust constants to the same literal values:

```rust
pub const PROTOCOL_VERSION: u32 = 1;
pub const MAX_FRAME_BYTES: usize = 8 * 1024 * 1024;
pub const STARTUP_TIMEOUT: Duration = Duration::from_secs(5);
pub const DEFAULT_REQUEST_TIMEOUT: Duration = Duration::from_secs(10);
```

`SidecarError` stores only `kind` and a fixed public message. The mapping is: spawn `无法启动兼容组件`, timeout `兼容组件响应超时`, EOF/exit `兼容组件已退出`, version `兼容协议版本不匹配`, malformed/ID mismatch `兼容组件响应无效`, oversize `兼容组件响应过大`, and I/O `兼容组件通信失败`.

- [ ] **Step 4: Add failing child-process lifecycle tests**

Use `node -e` as an injected fake process; never use a shell. Add async tests for:

- valid hello then echo request moves `Starting -> Ready` and correlates `request-2`;
- hello protocol version `2` fails startup and leaves `Failed`;
- a silent child exceeds an injected `100 ms` request timeout and leaves `Failed`;
- a child that exits after hello maps the next request to `SidecarExited`;
- a response line of `MAX_FRAME_BYTES + 1` bytes maps to `FrameTooLarge`;
- explicit shutdown reaches `Stopped` and the child is reaped;
- dropping a live client relies on `Command::kill_on_drop(true)` and does not detach the child.

Each fake script reads NDJSON from stdin and emits deterministic frames; it must not open a socket or touch a profile.

- [ ] **Step 5: Run lifecycle tests and verify RED**

Run:

```bash
cargo test --manifest-path packages/app-lite/src-tauri/Cargo.toml sync_sidecar::client
```

Expected: FAIL because `SidecarClient` is not implemented.

- [ ] **Step 6: Implement the minimal serial Tokio client**

`SidecarCommand` is built from separate executable/argument values and passes them to `tokio::process::Command`; do not invoke `sh -c` or concatenate a command string. Pipe stdin/stdout, set stderr to null for the client boundary, and set `kill_on_drop(true)`.

Use `tokio_util::codec::LinesCodec::new_with_max_length(MAX_FRAME_BYTES)` for both directions. `start` sets `Starting`, spawns, sends `hello` as `request-1`, wraps it in `tokio::time::timeout(STARTUP_TIMEOUT, ...)`, verifies response ID and protocol version, then sets `Ready`. `request` allows only `Ready`, increments an internal counter, sends one frame, waits under the configured timeout, and on timeout/EOF/codec/protocol error terminates the child and sets `Failed`. `shutdown` sets `Stopping`, sends `shutdown`, waits up to `5 seconds`, kills only if needed, waits to reap, then sets `Stopped`.

Export the module from `lib.rs`, but do not construct it in `run()` and do not add it to `tauri::generate_handler!`.

- [ ] **Step 7: Add the real Node-sidecar integration test and verify RED**

`tests/sidecar_compatibility.rs` resolves the repository root from `CARGO_MANIFEST_DIR`, reads only `packages/app-lite-sync/fixtures/v1/note.txt`, and starts this injected command without a shell:

```rust
SidecarCommand {
    executable: PathBuf::from("corepack"),
    args: vec![
        "yarn".into(), "workspace".into(), "@joplin/app-lite-sync".into(),
        "start:stdio".into(),
    ],
    current_dir: repo_root,
}
```

The test calls `decodeItem`, asserts the fixed Note ID, calls `encodeItem`, calls `decodeItem` again, asserts title `Fixture note` and body marker `第二行中文`, then shuts down and asserts `Stopped`.

Run:

```bash
cargo test --manifest-path packages/app-lite/src-tauri/Cargo.toml --test sidecar_compatibility
```

Expected before final integration wiring: FAIL at startup or missing response. Do not accept a missing `corepack` or missing dependency as RED; the actual sidecar process must start far enough to expose the missing integration behavior.

- [ ] **Step 8: Complete integration wiring and document the boundary**

Make the smallest changes needed for the real test to pass. Update `packages/app-lite/README.md` to state:

- the compatibility sidecar exists but is not started by the application;
- it uses no real profile or network in this phase;
- local canonical profile creation and CRUD are the next plan;
- final app packaging will bundle a fixed Node runtime rather than depend on the user's system Node.

- [ ] **Step 9: Run full phase verification**

Run in this order:

```bash
corepack yarn workspace @joplin/app-lite-sync test
corepack yarn workspace @joplin/app-lite-sync tsc
corepack yarn workspace @joplin/app-lite test
corepack yarn workspace @joplin/app-lite tsc
cargo test --manifest-path packages/app-lite/src-tauri/Cargo.toml
cargo fmt --manifest-path packages/app-lite/src-tauri/Cargo.toml -- --check
cargo clippy --manifest-path packages/app-lite/src-tauri/Cargo.toml --all-targets -- -D warnings
corepack yarn workspace @joplin/app-lite build:web
git diff --check
```

Expected: all commands exit 0; existing frontend has 10 or more passing tests, existing Rust suite has 12 or more passing tests plus the new sidecar tests, and no formatting or lint errors are reported.

Run the no-side-effect source gate:

```bash
if rg -n "\.config/joplin-desktop|https?://|WebDAV|JOPLIN_ADMIN_PASSWORD|token" packages/app-lite-sync packages/app-lite/src-tauri/src/sync_sidecar packages/app-lite/src-tauri/tests/sidecar_compatibility.rs; then
  exit 1
fi
```

Expected: no matches and exit 0.

- [ ] **Step 10: Commit**

```bash
git add packages/app-lite/src-tauri packages/app-lite/README.md
git commit -m "feat: supervise Joplin sync sidecar"
```
