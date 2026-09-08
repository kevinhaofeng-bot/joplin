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

## Fix round 1：findings 回归与修复

### 修复内容

- 超预算的已提交编辑现在明确切断旧 undo 链，避免旧 inverse 覆盖超预算编辑后的文档；undo/redo 每次替换 forward/inverse 后重新计算 entry 字节数并按条数/字节预算裁剪。
- ToggleMark/SetLink 对相交 run 切出选区左、右残余；跨块删除及 InsertText/InsertImage 替换将 suffix 平移 `start_offset - end_offset`，保留 prefix 长度。
- Image/Attachment/Divider 现在接受 offset 0 的 Before/After `DocPoint`；文本/结构节点范围可消费中间图片，图片替换、文本替换和跨图片删除均由统一事务处理。
- 所有事务 outcome selection 在提交前验证；拼接后的 caret/merge/delete 按 affinity 解析到 grapheme 边界，样式边界在拼接 grapheme 上重新规范化。
- `History::apply_with_selection` 接受 MergeBlocks、RemoveNode、SetImageDisplayWidth 等无 selection_hint 事务的真实编辑前选区；History 同步跟踪最近选区。
- `Document::apply_batch` 改为受影响事务 inverse 组成的局部 rollback journal；不再为每次按键深克隆完整 Document。失败事务自身也先用局部 inverse 回滚，再回滚批次前序操作。

### Findings 覆盖测试

`packages/app-lite-gpui/src/native_editor/tests.rs` 新增/扩展：

- `over_budget_edit_cuts_stale_undo_history`
- `marks_and_links_preserve_residual_runs_at_selection_edges`
- `cross_block_delete_shifts_suffix_styles_after_retained_prefix`
- `image_before_after_points_and_cross_image_ranges_use_document_transactions`
- `cross_image_input_replacement_consumes_structural_nodes`
- `insertion_resolves_grapheme_seam_before_returning_cursor`
- `undo_redo_reprices_replacement_payloads_and_trims_to_budget`
- `non_selection_transactions_restore_explicit_history_selection`（覆盖 MergeBlocks、RemoveNode、SetImageDisplayWidth）
- `small_edit_does_not_allocate_a_full_document_clone`
- `invalid_batch_rolls_back_prior_local_operations`

### Fix round TDD 证据

RED 使用 detached 基线 `94607b671`，只在临时工作树为新增测试加了未实现语义的 `apply_with_selection` 编译桥，未修改本工作树：

```text
cargo test --manifest-path packages/app-lite-gpui/Cargo.toml native_editor::tests -- --nocapture
running 15 tests
test result: FAILED. 6 passed; 9 failed; 0 ignored; 0 measured; 753 filtered out
失败覆盖：over_budget、marks/link residual、suffix style、image range/input、grapheme seam、history selection、replacement budget、full-document allocation。
```

GREEN 聚焦验证：

```text
cargo test --manifest-path packages/app-lite-gpui/Cargo.toml native_editor::tests -- --nocapture
running 15 tests
test result: ok. 15 passed; 0 failed; 0 ignored; 0 measured; 753 filtered out
```

### Fix round 完整验证

```text
cargo test --manifest-path packages/app-lite-gpui/Cargo.toml --all-targets --no-run
Finished test profile; all test and bench executables generated; exit 0

cargo test --manifest-path packages/app-lite-gpui/Cargo.toml --bin velotype -- --skip editor::selection::tests::cross_block_cut_writes_markdown_deletes_range_and_undo_restores
test result: ok. 767 passed; 0 failed; 0 ignored; 0 measured; 1 filtered out

cargo fmt --manifest-path packages/app-lite-gpui/Cargo.toml -- --check
git diff --check
通过；exit 0
```

### Fix round 自审与顾虑

- 旧报告中“候选文档 clone”是初始实现状态；本轮已改为局部 inverse journal，`semantic_snapshot()` 仍仅作为测试语义比较辅助，不进入事务提交路径。
- 分配回归测试在 20,000 个约 1 KiB block 上执行，20 MiB 阈值留出 invariant 校验和并行测试噪声；旧完整 clone 基线约 32.6 MiB，本轮 focused 与 bin 回归均通过。
- 结构事务若调用方没有当前选区，应通过 `apply_with_selection` 传入编辑前选区；History 仅在没有显式选区且没有最近选区时保留文档末尾 fallback，未扩展 UI 状态。
