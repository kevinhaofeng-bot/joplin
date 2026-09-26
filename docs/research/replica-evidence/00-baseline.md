# 00 基线冻结与测试阻断清理

- 任务：任务0（独立工程修复，不属于逆向成果）
- 实现提交：见本文件所在提交（只含 `packages/app-lite-gpui/src/native_editor/images.rs` 及本证据）
- 测试构建 SHA：工作树基于 `e6d044db856c0003dc0524880444eaa3866b8d12` + 该补丁
- Evernote 源码：不适用（测试宿主边界问题，与 Evernote 行为无关）

## 接手现场

`git status --short`（开工时）：

```text
 M packages/app-lite-gpui/src/native_editor/images.rs
?? docs/research/joplin-lite-native-pause-handoff-2026-09-13.md
?? docs/research/product-delivery-gaps-2026-09-26.md
?? docs/superpowers/plans/2026-09-26-claude-evernote-product-delivery.md
```

HEAD `e6d044db856c0003dc0524880444eaa3866b8d12`，与规划编写时一致。`images.rs` 差异归属：GPT-6 Sol 的剪贴板测试边界修复，原报告与 diff 见 `.superpowers/sdd/2026-09-11-evernote-core-notes-roadmap/clipboard-boundary-{report-2026-09-26.md,review.diff}`。历史交底 `joplin-lite-native-pause-handoff-2026-09-13.md` 保持未跟踪，未改动。

## 补丁审查（Claude）

差异仅两处 `cfg`：生产 macOS 的 `read_native_pasteboard()` 在 `cfg(test)` 下不编译，改用返回 `None` 的替身。核对结论：

- 切断的只有最终 AppKit `NSPasteboard` 读取。`ui/mod.rs::paste_resource_or_text` 仍调用真实 `resolve_clipboard_payload(native, cx.read_from_clipboard())`、`classify_clipboard`、`capture_resource_insert_intent`、`complete_paste_intent`（资源暂存、保存队列、journal fence）。
- 纯解析辅助（`NativeImageRead` 候选选择、`copy_bounded_native_image_bytes` 等）不受 cfg 影响，仍被单元测试覆盖。
- 生产二进制行为不变；`spike_app.rs` 的同名调用在测试下同样得到 `None`，spike 不属产品路径。
- 局限：**真实系统剪贴板粘贴未由此验证**，须在 Release 实机矩阵（任务1/7）中验证。

## 失败回归：修复前 / 修复后

修复前（`/tmp/joplin-lite-after-cleanup-full.log` 摘录，已复制到 `logs/00-gpui-full-before.excerpt.log`）：单线程全套在 `ui::tests::mounted_cmd_v_after_typing_queues_saved_point_until_journal_worker_finishes` 处 `cocoa-0.26.0/src/appkit.rs:791` panic → `SIGABRT`，全套中止。

修复后，本会话命令（`CARGO_TARGET_DIR=/Users/kevinhao/Projects/joplin/.shared-target`）：

```sh
cargo test --manifest-path packages/app-lite-core/Cargo.toml --tests
cd packages/app-lite-gpui && cargo test --locked --bin velotype -- --test-threads=1
```

| 套件 | 结果 | 日志 |
| --- | --- | --- |
| app-lite-core `--tests` | 248 passed / 0 failed，exit 0 | `logs/00-core-summary.log` |
| app-lite-gpui `--bin velotype --test-threads=1` | **1326 passed / 0 failed**，exit 0，82.57 s；原中止用例 `ok` | `logs/00-gpui-full-after.summary.log` |

仍有既有 `unexpected_cfgs`、dead code 警告，未处理（不在本任务范围）。

## Release 实机

本任务无产品行为变化，未做实机。系统剪贴板粘贴留待任务1实机矩阵。

- Claude 实施状态：提交待验收
- Codex 验收状态：未验收
