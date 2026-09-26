# 08 剩余交付（交底 2026-09-26-claude-remaining-delivery-handoff）

- 实施：Claude Code；验收：Codex（本文件不填写验收结论）
- 交底基线 HEAD：`3d16771bc`；Codex 修复记录提交 `f7768ddec`、`0f3d03f57`
- 工作树：`/Users/kevinhao/Projects/joplin/.worktrees/joplin-lite-native-rust-mvp`，分支 `codex/joplin-lite-native-rust`
- 开工时未提交现场（保留，未纳入任何提交）：`docs/research/joplin-lite-native-pause-handoff-2026-09-13.md`、`docs/research/product-delivery-gaps-2026-09-26.md`、`docs/superpowers/plans/2026-09-26-claude-evernote-product-delivery.md`（均为未跟踪文档，无未提交代码）

## 阶段1：组内图片缩放持久化与父块内文件附件

### 读过的 Evernote 源码

根：`/Users/kevinhao/Projects/joplin-reconstruction/evernote-11.32.5/common-editor-sourcemap/@evernote/common-editor/src/apps/peso/modules/`

| 文件 | SHA256 | 符号 / 行段 | 行为 |
| --- | --- | --- | --- |
| `resource/image/imagecomponent.tsx` | `91637400…19e55a95` | `getWidth()` 449-452 | 宽度存于图片节点自身 `attrs.width`（`NNNpx`），缺省用天然宽度或 300 |
| `resource/image/ResizeHandle.tsx` | `49026a84…6d413ff0d7` | `MIN_WIDTH = 50`（17）；拖动计算 120-135；`onMouseDown`/`onDoubleClick` 234-235 | 拖动按比例计算、下限 50px、上限笔记可用宽度；双击手柄复位 |
| `resource/schema.ts` | `74d07643…9e4975c5` | `width` attr 44；图片/文件节点 `group: 'section tablecontent listblockcontent'` 1001、1046 | 图片与文件卡都是原子块，可作为列表项内容 |
| `list/schema.ts` | `23ebd1d5…bd276f19d` | `li.content: 'listblockcontent+'` 412 | 列表项可包含多个块（段落、图片、文件卡） |

行为对照：Evernote 的图片/文件卡是**块级原子节点**，可以位于列表项内；标题（`heading/schema.ts:102`）内容为行内文本，**Evernote 标题内不含图片或文件卡**。

- 采用：宽度存于图片节点本身；拖动只预览、松开提交一次；下限 50px、上限可用宽度（列表缩进计入）；双击复位天然尺寸；列表项内可有图片与文件卡。
- **独立设计（非 Evernote 行为）**：标题/引用内的图片与文件卡。原因：迁移的 Joplin 旧 HTML 中存在 `<h2>文字<img>文字</h2>` 等结构（真实副本首轮审计：标题内图 5 篇、引用内图 2 篇），为不降级整篇，本项目以“稀疏语义父组”在原位保留。序列化为 canonical `Inline::Image{display_width}`（`data-joplin-lite-display-width`）与新增 `Inline::Attachment`（`<a data-joplin-lite-inline-attachment ...>`）。

### 实现（均已提交）

| 提交 | 文件 | 作用 |
| --- | --- | --- |
| `6ec89e0f2` | `app-lite-core/src/document.rs`、`native_editor/codec.rs` 等 | `Inline::Image.display_width`；旧 HTML 解析为 `None`，打开不改正文；codec 组内图片读写宽度，取消原“拒绝导出” |
| `99405b34c` | `native_editor/{core,layout,render,surface}.rs` | 选中图片显示缩放手柄；拖动预览、松开一次可撤销提交；双击复位 |
| `3c260b9b3` | `document.rs`、`codec.rs`、`layout.rs`、`model.rs` | `Inline::Attachment`；codec 导入为组成员卡片并原位写回；模型不再在父组内拒绝 `InsertAttachment`；列表项内卡片带列表缩进；搜索文本含文件名；资源引用集合含组内附件 |
| 本次（待提交） | `app/image_flow_tests.rs`、`native_editor/model.rs`（测试）、`native_editor/core.rs`（注释） | 端到端与模型边界回归；修正过时注释 |

兼容性：旧文档从不含 `data-joplin-lite-inline-attachment` 标记；标记不完整时退回普通链接，不丢内容（`Projection::inline_attachment`）。普通段落中的行内附件在原生编辑器拆为块级卡片（与普通段落行内图片一致），文字与资源顺序保持。

### 回归测试

提交 `3c260b9b3` 前已有（codec/model/core 级）：`inline_image_display_width_round_trips_and_old_html_has_none`、`grouped_inline_image_resize_saves_and_reopens_with_its_width`、`inline_group_atomic_image_and_attachment_insertions_are_saveable_and_reversible`、`mounted_image_resize_handle_commits_one_undoable_width_that_persists`（顶层图片）。

本次补齐交底要求而此前缺失的端到端路径：

| 测试 | 覆盖 |
| --- | --- |
| `app::image_flow_tests::mounted_grouped_image_resize_persists_across_switch_and_reopen` | 真实仓库种子 `<h2>前<img>后</h2>`；挂载后拖动组内图片手柄 → 120px；ManualSync 后正文仍以 `<h2>前<img` 开头且含宽度；切到另一篇再切回宽度不丢；以新 `LibraryRepository` 重开同一 sqlite 后宽度不丢 |
| `app::image_flow_tests::mounted_grouped_attachment_insert_saves_undoes_and_redoes` | 点击标题文字末尾后走真实选择器完成路径插入 PDF；保存正文为 `<h2>前<a data-joplin-lite-inline-attachment…` 且图片仍在、资源 2 个；Cmd-Z 后保存正文不含附件、标题结构不变；Cmd-Shift-Z 后保存正文与插入后逐字相同 |
| `native_editor::model::tests::inline_group_attachment_survives_typing_and_cross_parent_delete_with_exact_undo` | 列表项内卡片后输入中文、从列表项起跨到标题的范围删除（含卡片）；每步可导出，逆操作恢复语义快照与原 HTML |

先失败证据：实现先于这两个端到端测试提交，故以变异核验代替“修复前运行”：
- 把 codec 组成员导入宽度改为 `None` → `mounted_grouped_image_resize_persists_across_switch_and_reopen` 在“switching back keeps the width”断言失败（`image_flow_tests.rs:1943`）。
- 恢复旧的“父组内拒绝 `InsertAttachment`” → `mounted_grouped_attachment_insert_saves_undoes_and_redoes` 在正文前缀断言失败（`:1985`）。
- 两次变异后源文件均从备份还原，`git diff` 无残留。

已由 Codex 前轮覆盖、本次未重写：组内 Enter 拆父组、跨组合并唯一归属、不同对齐父组跨选区删除、失败批事务回滚、末尾图片后输入继承父样式/对齐（见 `replica-review-fixes-2026-09-26-codex.md`“本轮代码验收关闭”）。

### 命令与结果

日志目录 `/tmp/joplin-stage1-claude/`（`exit.txt` 汇总退出码）：

| 命令（所在目录） | 结果 | 日志 |
| --- | --- | --- |
| `cargo test --locked --bin velotype`（`packages/app-lite-gpui`） | 退出0；1360 通过、0 失败、1 忽略（显式真实库审计） | `gpui.log` |
| `JOPLIN_LITE_AUDIT_DATABASE=/tmp/joplin-migration-current.MeMdVA/all_notebooks-1790430342/library.sqlite cargo test --locked --bin velotype imported_real_copy_opens_and_round_trips_resources_in_native_editor -- --ignored --nocapture` | 退出0；`real-copy notes=1666, failure_categories={}` | `audit.log` |
| `cargo test --manifest-path packages/app-lite-core/Cargo.toml --features test-support --tests`（根） | 退出0；342 通过、0 失败 | `core.log` |
| `cargo test --manifest-path packages/app-lite-server/Cargo.toml --offline`（根） | 退出0；10 通过 | `server.log` |
| `git diff --check` | 退出0 | `diffcheck.log` |

编译警告仍存在（GUI 测试构建输出 149 行 `warning`），未清零、不宣称清零。真实副本审计比较资源顺序与非空白文字，不代替排版/表格视觉验收。

### 未通过 / 未做

- Release 实机：未做。组内图片拖动、附件卡片在列表缩进下的视觉、IME 在卡片前后的组合输入均只有挂载测试证据，留待 Codex 实机验收。
- 普通段落内的行内附件打开后拆为块级卡片：保存后不再是行内排版（与段落内行内图片同策略），属有意降级，未宣称保持原排版。
- 引用内附件：由同一父组机制支持，但本次未新增引用专门测试（端到端只测了标题与列表项）。

- Claude 实施状态：阶段1提交待验收（实机未做）
- Codex 验收状态：未验收
