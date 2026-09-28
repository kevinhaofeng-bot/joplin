# 图文混合选区独立复核（2026-09-28）

基线：`98a5a995fcaf01546683751e446716c4d681f1f6`，复核时 HEAD `9c3a4b6ba`。

## 已执行

独立运行：

```sh
CARGO_TARGET_DIR=/Users/kevinhao/Projects/joplin/.shared-target cargo test --manifest-path packages/app-lite-gpui/Cargo.toml --bin velotype native_editor::commands::tests -- --nocapture
```

结果：7 passed / 0 failed / 1455 filtered out。包含混合资源转 UL/OL、保存重开、撤销重做、混合选区文本样式、单资源转清单。不是整款软件验收通过。

后续独立全量运行同一 bin（不带过滤）：1460 passed / 0 failed / 2 ignored，退出 0，14.67 秒。日志 `/tmp/joplin-independent-full-authorized-20260928.log`。初轮沙箱内 1429 passed / 31 failed / 2 ignored，失败涉及本机测试网络权限；获准在沙箱外重跑后全部通过。初轮日志 `/tmp/joplin-independent-full-20260928.log` 保留。此处仅是主程序测试集，不包含独立集成测试或最终安装包实机验收；通过也不排除下述未覆盖的分组资源选区缺陷。

重新读取 Evernote 11.32.5 sourcemap 的 `common-editor/src/apps/peso/modules/list/list.ts`：`insertOrToggleList` 对资源创建列表项，并对已有 listItems 执行 `setNodeMarkup` 切换类型。

## 待修复核点：已经分组的资源单独切换列表类型

代码路径显示遗漏（尚未用新增回归测试或实机复现）：

- `selected_block_indices` 仅返回实际选区节点范围，不扩展到 InlineGroup 的文本行。
- `selected_items` 对单独选中的已分组图片返回 `Grouped { kind }`。
- `apply_list_command` 创建样式事务只处理 `SelectedItem::Text`；创建新列表行只处理 `Standalone`。
- 因此单独选择已有 UL 内图片再按 OL / 清单，状态可用但事务为空；原资源列表类型不变。现有测试只覆盖第一次转换与全选转换，没有覆盖这一链条。

请 Claude 增加针对已分组图片和附件的 UL → OL → 清单回归测试，检查保存重开、单步撤销，以及选区仅覆盖资源的情况，再修复。不得通过禁用按钮代替真实类型切换。

## 施工状态

已按用户要求向 screen `26019.joplin-lite` 发送继续指令；该次请求明确返回 weekly limit，尚未恢复施工。禁止把指令送达描述为正在施工。代码修改仍由 Claude 负责；Codex 继续独立验收。
