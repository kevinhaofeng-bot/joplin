# Task 3：紧凑原生文档模型、事务与历史

## 实现内容

- 新增 `native_editor::model`：稳定 `NodeId`、连续 `Vec<Block>`、`BlockKind`、`BlockContent`、`DocPoint`、`Selection`、`Affinity`、可组合 `Mark` 和非重叠 `StyledRun`。
- 新增 `native_editor::transaction`：统一 `Transaction`、`TransactionBatch` 和 `ApplyOutcome`；文本插入/删除、分裂/合并、列表/标记/链接/对齐、图片节点、节点删除和图片宽度均走同一事务入口。
- 新增 `native_editor::history`：保存前向操作与局部反向操作，限制条数和总字节数；撤销/重做返回选择，并在结构操作重做时保留稳定节点身份。
- `Document::apply`/`apply_batch` 在候选文档上完成节点身份、UTF-8、grapheme、选择、标题级别、列表深度、样式规范化和结构验证，成功后才替换活动文档。
- 图片是 `BlockKind::Image` + `BlockContent::Image` 结构节点；`InsertImage` 原子地产生文本/图片/文本三段，不写入替代字符。
- `tests.rs` 包含简报三项基础测试、确定性事务序列（插入、分裂、列表、组合 marks、图片、跨块删除、undo/redo）和 `invalid_transaction_is_atomic`。

## TDD RED/GREEN 证据

RED：

```text
cargo test --manifest-path packages/app-lite-gpui/Cargo.toml native_editor::tests
error[E0432]: unresolved import `super::history`
error[E0432]: unresolved import `super::model`
error[E0432]: unresolved import `super::transaction`
```

GREEN：

```text
cargo test --manifest-path packages/app-lite-gpui/Cargo.toml native_editor::tests
running 5 tests
test result: ok. 5 passed; 0 failed
```

最终聚焦验证还执行了 `cargo fmt --manifest-path packages/app-lite-gpui/Cargo.toml -- --check` 和 `git diff --check`，均通过。

## 完整回归

简报指定命令：

```text
cargo test --manifest-path packages/app-lite-gpui/Cargo.toml --all-targets -- \
  --skip editor::selection::tests::cross_block_cut_writes_markdown_deletes_range_and_undo_restores
```

该命令的测试目标通过：`757 passed; 0 failed; 1 filtered out`；随后 `--all-targets` 将同一个 `--skip` 参数传给 `harness = false` 的 Criterion bench，bench 以 `unexpected argument found` 退出。这是 Cargo/bench 参数转发问题，不是 donor 测试失败。

为排除该命令行问题，执行了：

```text
cargo test --manifest-path packages/app-lite-gpui/Cargo.toml --bin velotype -- \
  --skip editor::selection::tests::cross_block_cut_writes_markdown_deletes_range_and_undo_restores
```

结果：`757 passed; 0 failed; 1 filtered out`。简报要求跳过的 donor SIGSEGV 测试未修改、未扩大 skip。

## 文件

- `packages/app-lite-gpui/Cargo.toml`
- `packages/app-lite-gpui/Cargo.lock`
- `packages/app-lite-gpui/src/native_editor/mod.rs`
- `packages/app-lite-gpui/src/native_editor/model.rs`
- `packages/app-lite-gpui/src/native_editor/transaction.rs`
- `packages/app-lite-gpui/src/native_editor/history.rs`
- `packages/app-lite-gpui/src/native_editor/tests.rs`

## Self-review

- 新代码只在 `native_editor` 增量实现；未引用 donor Markdown 类型，也未改变普通 donor 编辑器路径或 8 个验收门槛。
- 事务错误在候选文档上发生，活动文档的语义快照、revision 和历史深度保持不变；成功事务才增加 revision。
- `StyledRun` 以字节范围存储但要求 UTF-8/grapheme 边界，mark 集合排序去重，相邻同集合 run 合并。
- 历史受 1000 条/16 MiB 构造参数约束；局部 `RestoreBlocks` 只保存受影响连续块作为反向操作，不保存完整文档快照。
- 结构性 undo/redo 使用局部反向结果重建 forward 操作，避免重新分配后续操作依赖的 NodeId。

## 顾虑

- 简报的 `--all-targets -- --skip` 命令会把测试过滤器传给 Criterion bench；bin 目标等价回归已通过，但若 CI 直接采用简报命令，应将 donor 测试目标与 bench 目标分开传参。
- 新 native API 当前没有接入 UI、同步、CRDT 或持久化，按本任务范围保留给后续任务。
