# Velotype donor baseline

- Repository: https://github.com/manyougz/velotype
- Branch: dev
- Revision: ddd9f32588c8318173e9a48d588785d568d9ffd8
- Imported: 2026-09-09

## Retained until replacement passes

| Donor module | Reused mechanism | Replacement gate |
|---|---|---|
| `components/block/input.rs` | GPUI IME bridge and UTF-16 conversion | native editor IME tests |
| `components/block/element.rs` | text layout and hit testing | native layout tests |
| `editor/selection.rs` | cross-block selection behavior | native selection tests |
| `editor/history.rs` | undo behavior reference | inverse-transaction tests |
| `editor/file_drop.rs` | image paste/drop intake | image-boundary tests |
| `editor/render.rs` | viewport culling and spacing | long-document tests |

The donor Markdown editor remains buildable while `native_editor` is developed beside it.

## Upstream test note

The unchanged donor test command `cargo test --manifest-path packages/app-lite-gpui/Cargo.toml --all-targets` exits with signal 11 (`SIGSEGV: invalid memory reference`) after passing earlier tests. A serial focused rerun identifies the failing test as `editor::selection::tests::cross_block_cut_writes_markdown_deletes_range_and_undo_restores`. The donor source was not modified to make the import green.
