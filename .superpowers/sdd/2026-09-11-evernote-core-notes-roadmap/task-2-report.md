# Task 2 — Local library schema and repository

## Scope and baseline

- Baseline: `7f29a7e5420f986e65a5da3d3e7ccf32e7b068b8`.
- Only `packages/app-lite-core` and the GPUI lockfile needed to resolve its new path dependency were changed. No `app-lite-native` or GPUI product source was changed.

## TDD record

1. Added the public module/API shells plus black-box `repository_flow` and `migration` tests before repository behavior.
2. Ran the focused tests while `LibraryRepository::open` returned `NotImplemented`: `repository_flow` failed 2/2 and `migration` failed 4/4 for the absent repository/migration behavior.
3. Implemented the smallest SQLite v4 repository path, then expanded the tests for actual SQL-column observation, post-commit events, journal compaction, all required schema tables, native v3 compatibility, deterministic mid-migration rollback, and path symlink refusal.
4. Re-ran the complete core suite after every green/refactor pass.

## Delivered

- Typed opaque 32-lowercase-hex IDs and domain DTOs for notes, notebooks, stacks, tags, resources, snapshots, and sync entity references.
- SQLite v4 forward migration with WAL, foreign keys, atomic transactions, v3 canonical-HTML conversion, default notebook creation, and all planned persistence tables.
- `LibraryRepository` CRUD/organization/trash/restore/purge, ordered resource/tag relationships, edit journal and transactional snapshot compaction, content-addressed image import, local revisions, search queue, sync outbox, and commit-after event publication.
- Lightweight `ListQuery` projection only; SQLite authorizer observation records columns read by the actual prepared list statement and rejects body HTML, merge state, and attachment bytes.
- Migration behavior distinguishes imported v3 state (revision 1, no outbox) from normal local mutations (queue/outbox in the same transaction).

## Fresh verification

- `cargo test --manifest-path packages/app-lite-core/Cargo.toml` — 64 tests passed.
- `cargo test --manifest-path packages/app-lite-native/Cargo.toml` — 225 tests passed.
- `cargo check --manifest-path packages/app-lite-gpui/Cargo.toml` — passed (pre-existing GPUI warnings only).
- `cargo test --manifest-path packages/app-lite-gpui/Cargo.toml -- --skip editor::selection::tests::cross_block_cut_writes_markdown_deletes_range_and_undo_restores` — 996 passed, 1 filtered.
- `cargo fmt --manifest-path packages/app-lite-core/Cargo.toml -- --check` and `git diff --check` — passed.

## Concerns / intentionally deferred

- This is only Task 2’s durable core contract, not an MVP application: GPUI `AppModel`, editor-session timing/coordinator, search workers, attachment UI, import/export, and transport remain owned by later tasks.
- `edit_journal` stores deterministic UTF-8 deltas and is compacted atomically by `flush_snapshot`; timing/scheduling and replay orchestration are deliberately deferred to Task 4.
- The schema contains durable search/sync contracts; Task 7 and Task 9 own FTS workers and network transport respectively.

## Fix round 1 — review closure (baseline `079044e504cfa323844817fb473da4d4608dd31c`)

### TDD record

1. Added black-box `repository_review_regressions` coverage before the repair. Its first focused run was RED in six independent ways: legacy RTF opened instead of failing closed; `A,B,A` resources violated the relation key; stale snapshot saves overwrote a newer revision; organization changes did not enqueue search; a persisted thumbnail was recomputed; and purge created no durable tombstone.
2. Added migration/association/ID-clock regression cases while closing those failures: rich RTF including bold/italic/underline/image overlay/draft/deleted metadata; v3 canonical `A,B,A`; stale association rollback; true FK behavior; deterministic per-repository clock rollback; and deterministic entity-ID collision retries.
3. The final focused review suite is GREEN (11 tests), followed by the complete core/native/GPUI gates below.

### Review-item mapping

- C1: v3 rows with `markup_language=1` now return `LegacyRtfMigrationRequired` before any transaction or schema write. The raw RTF, row metadata, `user_version=3`, and absent outbox are asserted. Task 8 remains responsible for a backed-up RTF conversion.
- C2: `note_resources` now keys each occurrence by `(note_id, position)` and indexes `resource_id`; snapshot and v3 migration tests prove exact `A,B,A` ordering after reopen.
- I1/I2: move, tag, trash, and restore write search queue/outbox within their data transaction, emit only after commit, use live-notebook restore fallback, and are protected by SQLite foreign keys with PRAGMA readback and behavior checks.
- I3/I4: `SaveNote` carries `expected_revision` and returns typed `StaleRevision`; v3 imports get revision-1 history plus migration search bootstrap but no outbox, while v4 reopen returns without logical writes.
- I5/I6: resource-store safety preflight happens before migration; purge is trash-only, records a durable tombstone, and retains independent revision audit history.
- I7/I11: production IDs use OS CSPRNG 128-bit values; per-repository ID/clock seams are deterministic and isolated to tests. Database collisions retry before exposure, and note `updated_time` is `max(now, previous + 1)` for identical or backward clocks.
- I8/I9: selected thumbnails are persisted and read only if still a live associated image (otherwise ordered fallback); `DeletionScope` explicitly selects Active, Trash, or All.
- I10/M3: `AssociateResource` applies body/resource occurrence/revision/search/outbox as one snapshot transaction; stale association rolls back without a visible partial relation. Resource metadata exposes typed `BlobHash`.
- M1/M2: WAL and foreign-key PRAGMAs are read back; migration and SQLite errors retain typed source errors rather than becoming opaque strings.

### Fix-round verification

- `cargo test --manifest-path packages/app-lite-core/Cargo.toml` — 76 tests passed.
- `cargo test --manifest-path packages/app-lite-native/Cargo.toml` — 225 tests passed.
- `cargo check --manifest-path packages/app-lite-gpui/Cargo.toml` — passed (existing warnings only).
- `cargo test --manifest-path packages/app-lite-gpui/Cargo.toml -- --skip editor::selection::tests::cross_block_cut_writes_markdown_deletes_range_and_undo_restores` — 996 passed, 1 filtered.
- `cargo fmt --manifest-path packages/app-lite-core/Cargo.toml -- --check` and `git diff --check` — passed.

### Remaining concern

- Fail-closed legacy RTF is intentional until Task 8 can perform its dedicated, backed-up conversion; no RTF parser or AppKit/GPUI dependency was introduced into `app-lite-core`.

## Fix round 2 — migration safety closure (baseline `486024a3a64cc1c17eb2f67ffdc81105f11713ec`)

### TDD record

1. Added and ran deterministic RED cases for legacy refusal changing DELETE/WAL/profile entries, purge losing its search deletion instruction, associated-resource cleanup deleting its upload operation, outbox-ID collision, and purged-ID reuse.
2. Each failed against the previous implementation for its named observable condition; the focused `test-support` suite is GREEN with 16 tests after the corresponding changes.

### Review mapping

- C1/I5: migration now starts with `BEGIN IMMEDIATE`; the RTF gate runs while that lock is held and before resource preflight/schema mutation/WAL. Resource preflight runs under that same lock, profile identity is checked around SQLite and before commit, and WAL is established only after a successful migration.
- I1: migration/default IDs, durable entity IDs, and sync-outbox operation IDs use the per-repository allocator; outbox insertion retries real uniqueness failures, and notes reserve IDs named by tombstones or retained revisions.
- I2/I3: `search_queue` delete jobs outlive a purged note and purge emits `SearchProjectionQueued` after commit. Resource rollback only removes the resource outbox when its unassociated row was actually deleted.
- I4: profile binding holds a `O_DIRECTORY|O_NOFOLLOW` descriptor; resources are opened relative to it and pathname identity mismatches reject before schema publication.
- M1/M2: clock/ID injection is behind non-default `test-support`; entropy variants retain `getrandom::Error` as their typed source.

### Fix-round verification

- `cargo test --manifest-path packages/app-lite-core/Cargo.toml` — passed.
- `cargo test --manifest-path packages/app-lite-core/Cargo.toml --features test-support` — passed (16 review regressions active).
- `cargo test --manifest-path packages/app-lite-native/Cargo.toml` — 225 passed.

## Fix round 2 proof completion

- Added a per-repository, `test-support`-only open-phase hook. A channel-controlled two-connection test stops immediately after the locked legacy gate; the second v3 writer receives an SQLite write error and no RTF row can cross into the HTML migration.
- Added real rename/replacement tests at both profile-bind→SQLite-open and SQLite-open→resource-bind gaps. Both return `InvalidDatabasePath`, leave the original v3 database at version 3, and do not bind resources in the replacement directory.
- Added a `migrate_schema` v4-fast-path authorizer test that denies and records INSERT/UPDATE/DELETE/DDL/REINDEX actions. The actual fast path succeeds with an empty write record; a same-value update would therefore turn it RED.
- Expanded v3 refusal and mid-migration rollback checks to snapshot `sqlite_master`, `table_info`, every legacy source field including RTF/markup/draft/deleted state, `journal_mode`, `user_version`, and profile entries. The new snapshot test exposed an empty `resources` directory surviving an aborted migration; preflight-created trees are now cleaned through the bound profile fd unless the transaction commits.
