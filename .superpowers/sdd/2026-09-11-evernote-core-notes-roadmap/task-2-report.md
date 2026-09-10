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
