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

归属：`6ec89e0f2`、`99405b34c`、`3c260b9b3` 由并行会话“Claude Evernote 产品交付”提交；本会话补端到端回归与本证据（`56c48d6aa`）。该会话其后提交 `d3127d4d3`（空列表项回车退出、缩放后保持选中），不在本文件范围。

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

并行施工说明：同一工作树另有会话“Claude Evernote 产品交付”在改 `app-lite-gpui`（当时 `native_editor/toolbar.rs` 含其未提交的调试打印）。上表 GUI 全套编译进了这些未提交改动（仅 stderr 输出差异），不是纯净 `56c48d6aa` 构建；Codex 验收需在干净检出上重跑。两次变异核验期间曾临时改动 `codec.rs`/`model.rs` 数十秒，已还原。此后分工：该会话负责 `app-lite-gpui`，本会话只改 `app-lite-core` 迁移代码、审计示例与本文件。

编译警告仍存在（GUI 测试构建输出 149 行 `warning`），未清零、不宣称清零。真实副本审计比较资源顺序与非空白文字，不代替排版/表格视觉验收。

### 未通过 / 未做

- Release 实机：未做。组内图片拖动、附件卡片在列表缩进下的视觉、IME 在卡片前后的组合输入均只有挂载测试证据，留待 Codex 实机验收。
- 普通段落内的行内附件打开后拆为块级卡片：保存后不再是行内排版（与段落内行内图片同策略），属有意降级，未宣称保持原排版。
- 引用内附件：由同一父组机制支持，但本次未新增引用专门测试（端到端只测了标题与列表项）。

- Claude 实施状态：阶段1提交待验收（实机未做）
- Codex 验收状态：未验收

## 阶段2：迁移警告（进行中，未完成）

依据：Joplin JEX 格式适配，属本项目适配，不是 Evernote 行为；图片只存宽度参照 `resource/image/imagecomponent.tsx:449-452`。审计用只读探针 `examples/migration_fidelity_audit.rs`（`SQLITE_OPEN_READ_ONLY`，只输出计数；`JOPLIN_LITE_AUDIT_KINDS=1` 额外输出每篇的结构特征组合），库为 `/tmp/joplin-migration-current.MeMdVA/all_notebooks-1790430342/library.sqlite`（隔离副本，不是原库）。日志在 `/tmp/joplin-stage2-claude/`。

| 步骤 | 提交 | 严格通过 / 警告 |
| --- | --- | --- |
| 基线 | — | 1386 / 280 |
| 行内资源链接 `[标签](:/id)` 转为行内附件卡片或图片，标签与文件名不同时保留标签文字 | `c15c16d18` | 1421 / 245 |
| HTML 块只有被严格 HTML 转换器接受时才转换；行内 `<br>` 转换行；`<img width>` 保存为显示宽度（height 不保存）；无属性的 `<html>`/`<body>` 空包装块跳过（Joplin 显示为空，原字节仍在审计表） | `becdff6d6` | 1422 / 244 |
| `<html>` 后被 CommonMark 吞入同一 HTML 块的文字：剥离无属性 `<html>/<body>` 标签后，以文字开头的剩余部分包 `<div>`、以标签开头的直接交严格 HTML 转换器；两者混合仍阻断。源码换行沿用转换器约定保留为软换行（Joplin 浏览器会折叠为空格，这是可见差异，不丢文字） | `81b90cd7d` | 1442 / 224 |
| 自链接资源图片 `[![alt](:/id)](:/id)` 去掉冗余链接；链到别处的仍阻断 | `6bff4936c` | 1442 / 224（真实副本 0 增益：这些笔记同时有其他阻断） |
跳过空包装块、剥离包装标签都是判断，需要 Codex 确认。审计曾发现我自己写的包装块检测会在中文字符的字节边界上 panic，已修复并加回归。

### Codex 验收基线后的新鲜隔离导入（HEAD `becdff6d6`）

Codex 已独立核验 `becdff6d6`：core 345 通过；GUI 1362 通过、1 忽略；只读审计 1422 严格 / 244 警告。这不等于整体产品验收。

| 命令（目录） | 结果 | 日志 |
| --- | --- | --- |
| `cargo run -q --release --locked --example import_verify -- /tmp/joplin-lite-t2-accept/src/all_notebooks.jex /tmp/joplin-stage2-import.GsdfVk/imports`（`packages/app-lite-core`） | 退出0，91.1 s。源 SHA256 `8544080e…74906f500`（副本，原件只读未改）；1666 笔记/31 笔记本/2 组/64 标签/425 关系/4153 资源/4127 blob；**发布 blob 重新哈希 0 不符，源 blob 缺失 0、多余 0**；降级 244（与只读审计一致）；扫描 `unresolved_refs=1`（与“Internal link…”那 1 篇对应） | `/tmp/joplin-stage2-import.GsdfVk/verify.log` |
| `JOPLIN_LITE_AUDIT_DATABASE=/tmp/joplin-stage2-import.GsdfVk/imports/all_notebooks-1790457765/library.sqlite cargo test --locked --bin velotype imported_real_copy_opens_and_round_trips_resources_in_native_editor -- --ignored --nocapture`（`packages/app-lite-gpui`） | 退出0；`real-copy notes=1666, failure_categories={}`（原生打开后导出，比较资源顺序和非空白文字） | `/tmp/joplin-stage2-claude/native-audit-becdff6d6.log` |
| `mkdir -p /tmp/joplin-stage2-import.GsdfVk/backup && cargo run -q --release --locked --example backup_verify -- /tmp/joplin-stage2-import.GsdfVk/imports/all_notebooks-1790457765 /tmp/joplin-stage2-import.GsdfVk/backup`（`packages/app-lite-core`） | 退出0；备份→空目录恢复后计数一致、blob 集合一致，11 张内容表摘要全部相等，恢复后 4127 个 blob 重新哈希；备份/恢复共 66 s | `/tmp/joplin-stage2-import.GsdfVk/backup-verify.log` |

说明：第一次运行 `backup_verify` 因工作目录不存在报 `TargetParentMissing`（退出101）；创建目录后重跑通过，属于调用方式问题，不是产品缺陷。原生审计编译时工作树里有另一会话未提交的 `app-lite-gpui/src/native_editor/toolbar.rs`，审计入口与它无关，但仍不是纯净构建。原生审计只比较资源顺序和文字，不代替排版、表格的视觉验收。

| 多段落引用 → 相邻多个引用块（只含段落时）；含列表或标题的引用仍阻断 | `cd9405a04` | 1444 / 222 |
| 同类嵌套 Markdown 列表 → 列表项 `indent` = 嵌套层级（原生 depth，上限 8，依赖另一会话 `24d5a632c`）；异类嵌套、超 8 层、项内第二个块仍阻断。`[![alt](:/img)](url)` 与 `<a href><img></a>` → 图片 `link`（依赖另一会话 `ef8517cf1`）；链到资源或片段的仍阻断 | `6af36711c` | 1500 / 166 |
| 非图片资源用图片语法 `![名](:/pdf)` → 原位附件卡片（alt 与文件名不同时保留为文字）；在外链内时仍阻断 | `451f556a8` | 1502 / 164 |
| H4–H6（依赖另一会话 `c1275c5ea`）；顶层有序列表起始号写入 `start`（依赖 `27878ea83`，u32 溢出仍阻断）；嵌套子列表从 ≠1 开始仍阻断（展平后只能有一个 start） | `56342c126` | 1522 / 144 |
| GFM 表格 → canonical 表格（依赖另一会话 `5455bcb25`）：首行为表头；单元格保留标记、链接、`<br>`、转义管道、资源图片；超 1000 行/64 列或单元格含外链图片仍阻断；降级路径里失败的表格显示可读源码而不被当 HTML 拆散 | `22c9c233a` | 1581 / 85 |

### 剩余警告的真实分布（HEAD `6bff4936c`，只读探针）

用临时诊断打印（只打印事件变体名，运行后已还原，源文件无残留）确认：块级 `Table` 57 篇；行内 `InlineMath` 12；列表项内嵌套 `List` 7；列表项内 `CodeBlock`/`BlockQuote` 各 1；`DefinitionList` 1。按特征组合统计：带链接图片 51 篇（组合约 85 篇）、表格 30 篇（组合更多）、外链图片 11、有序列表起始号 11、数学 11、H4 及以上 5、HTML 属性/表格 11。

需要原生编辑器新增表示的类别（表格、带链接图片、列表嵌套深度、H4+、列表起始号）已发方案给负责 `app-lite-gpui` 的会话，本会话不改 gpui。另外发现一个疑似产品缺陷，已转交该会话确认：原生模型允许列表缩进（depth 1），但 `codec.rs:664` 导出遇 depth≠0 返回 `UnsupportedListDepth`，导入也固定为 depth 0。canonical 的 `ListItem.style.indent` 已存在，可以直接映射。

## 全库可读导出与恢复（区别于 SQLite 备份）

依据：Evernote `main-readable/src/modules/11354__enex-exporter.js`（SHA256 见 03 号证据）的 `k()` 在读不到附件时只记日志、跳过该附件；本实现有意不同：任一资源缺失或字节不符都会中止导出或恢复。ENEX 不含笔记本组层级，这里的组/笔记本/标签/回收站/历史结构属于本项目独立设计。

新增 `app-lite-core/src/import_export/library_readable_export.rs`：`export_library_readable`、`restore_library_readable`，格式 `app-lite-library-readable-export` v1，与原有选区格式 v2（`export_readable_selection`）并存，原格式不变。

- 导出内容：`index.html`（按组 → 笔记本 → 笔记列出，另有回收站一节，显示标签）、`readable/<id>.html` 浏览页（资源用相对链接）、`notes/<id>.html` 规范正文、`history/<id>.json` 全部历史修订、`resources/<sha>--<id>--<名>` 原始字节、`manifest.json`（组、笔记本、标签、笔记与关系、正文/历史摘要、资源元数据、快捷方式）。
- 不导出：同步身份、待发 outbox、未刷新的编辑日志（存在时拒绝导出，要求先 flush）、本机视图设置。
- 恢复流程：先校验全部 ID、关系、资源哈希与大小、正文/历史摘要和规范性；在同级临时目录重建；做逐项核对以及 `integrity_check` 和外键检查；全部通过才移到目标空目录，失败时目标目录保持为空。
- 为复用做的改动：`readable_export.rs` 中有界读取、防符号链接打开、哈希复制等 helper 改为 `pub(super)`；抽出 `open_bundle_root`；`write_html_escaped` 允许 `?Sized`。这些只是可见性和结构调整，行为不变，原选区导出的测试全部通过。

实现中发现并修正的问题：
1. 移动、打标签、移入回收站会递增 `notes.revision` 但不写历史行，所以历史修订号不连续。不变式改为：修订号严格递增，最后一条的标题和正文等于当前值，且修订号不超过当前修订号。
2. 恢复时如果和资源 ID 重命名一起写 `deleted_time`，会先触发 `resource_filename_search_update` 插入搜索行，导致唯一约束冲突。改为先重命名，再单独写 `deleted_time`。夹具加了软删除资源和快捷方式；变异核验去掉单独写入这一步后，测试失败。

测试：`tests/library_readable_export.rs` 共 2 项。
- 全库往返：比较 10 张表的逻辑行（组、笔记本、标签及顺序、笔记含回收站、资源关系、历史、资源含软删除、blob、快捷方式），检查 outbox 为 0、恢复后能搜到。
- 失败时关闭：目标已存在、恢复目标非空；篡改资源、正文、历史或版本号都会被拒绝，目标目录保持为空。

先失败证据：接口不存在时编译失败（E0432）。core 全套 349 通过、0 失败，日志 `/tmp/joplin-stage2-claude/core-readable.log`。

真实副本验证（隔离导入库，不是原库）：

| 命令（`packages/app-lite-core`） | 结果 | 日志 |
| --- | --- | --- |
| `mkdir -p /tmp/joplin-stage2-import.GsdfVk/readable && cargo run -q --release --locked --example library_readable_verify -- /tmp/joplin-stage2-import.GsdfVk/imports/all_notebooks-1790457765 /tmp/joplin-stage2-import.GsdfVk/readable` | 退出0，总 114.8 s；导出 55.1 s、恢复 44.3 s；1666 笔记、4153 资源。stacks 2、notebooks 31、tags 64、note_tags 425、notes 1666、note_resources 4591、note_revisions 1666、resources 4153、resource_blobs 4127、shortcuts 0，**逐表逻辑行摘要全部相等**；恢复后 4153 个资源逐一读出重新哈希，全部一致 | `/tmp/joplin-stage2-import.GsdfVk/readable-verify.log` |

日志中有一行 `restored_blob_dir_entries 0`，是探针自身的统计错误（blob 不在 `blobs/` 目录下），已从探针删除，不影响上面的表摘要和重新哈希结论。

未完成：
- 界面入口（“导出整个资料库为可读 HTML…”“从可读导出恢复到新资料库…”）已由另一会话接线，见下文 C（`dd13ee9fc`）；本会话未做实机验证。
- 可读页面的浏览效果没有在浏览器里实际查看。
- 导出期间没有取消接口；备份（`library_backup`）已有取消，这里还没有。

## ENEX 缺失附件（与 JEX 处理对齐）

依据：Evernote `main-readable/src/modules/11354__enex-exporter.js` 的 `k()`（86-146 行）在读不到附件时只记日志、跳过该附件，所以 Evernote 自己导出的 ENEX 可能含有没有数据的 `en-media`。旧实现遇到这种情况会拒绝整个导入（02 号证据列为未解决项）。

`0cddd7182`：
- 该位置写入可见占位 `[附件缺失：<类型> MD5 <哈希>]`（块级为段落，行内为文字）。
- 笔记记入降级报告，原因 `Attachment data missing from ENEX: <md5…>`。
- 同一笔记的其余格式和附件照常保留；导入后回读校验使用同一缺失集合。

测试：
- 新增 `missing_attachment_data_leaves_a_visible_placeholder_and_a_report`（粗体、另一张图片保留，占位可见，报告含 MD5）。先失败证据：旧代码返回 `MissingResource`。
- 原“整库拒绝缺失附件”的断言按新策略删除。原子性测试 `second_note_failure_discards_previously_staged_note_and_attachment` 改用第二篇的非法时间戳（`InvalidDate`，在创建笔记后硬失败）作为触发，仍然断言失败后不留暂存目录、不改兄弟 profile。

core 全套 351 通过，日志 `/tmp/joplin-stage2-claude/core-enex.log`。

## 中间 HEAD `fb3de66cc` 的新鲜隔离导入与原生审计

| 命令 | 结果 | 日志 |
| --- | --- | --- |
| `import_verify`（同上参数，输出 `/tmp/joplin-stage2-import2.lFpuSu/imports`） | 退出0，86.0 s；计数与上轮相同；降级 224（与只读审计一致）；发布 blob 0 不符，源 blob 缺失 0、多余 0 | `/tmp/joplin-stage2-import2.lFpuSu/verify.log` |
| 原生加载/回写审计（同上命令，指向 `/tmp/joplin-stage2-import2.lFpuSu/imports/all_notebooks-1790458774/library.sqlite`） | 退出0；`real-copy notes=1666, failure_categories={}`；运行时 `app-lite-gpui` 无未提交改动（`gpui-dirty.txt` 为空） | `/tmp/joplin-stage2-import2.lFpuSu/native-audit.log` |

## 阶段2 最终验证（HEAD `0cddd7182`，其后 `e171bbbf0` 只改导出侧历史大小检查）

| 命令 | 结果 | 日志 |
| --- | --- | --- |
| `import_verify`：JEX 副本 → `/tmp/joplin-stage2-import3.4Ivyn1/imports` | 退出0，89.1 s；1666/31/2/64/425/4153/4127；**降级 222**（与只读审计 1444/222 一致）；发布 blob 0 不符，源 blob 缺失 0、多余 0 | `/tmp/joplin-stage2-import3.4Ivyn1/verify.log` |
| `import_verify`：3 个 ENEX 副本（SHA256 `69d3d2c6…`、`df9b6210…`、`0576faba…`，与 02 号证据相同）→ `/tmp/joplin-stage2-import3.4Ivyn1/enex` | 3 个都退出0；各 1 篇；降级 0/0/1（与 02 号证据一致，那 1 篇是状态冲突的重复复选框）；这批 ENEX 没有附件，不能证明缺附件这条路径，该路径只有单元测试证据 | `/tmp/joplin-stage2-import3.4Ivyn1/enex.log` |
| 原生加载/回写审计 → `/tmp/joplin-stage2-import3.4Ivyn1/imports/all_notebooks-1790459274/library.sqlite` | 退出0；`real-copy notes=1666, failure_categories={}`；运行时 `app-lite-gpui` 无未提交改动 | `/tmp/joplin-stage2-import3.4Ivyn1/native-audit.log` |
| `cargo test --manifest-path packages/app-lite-core/Cargo.toml --features test-support --tests`（HEAD `e171bbbf0`） | 退出0；351 通过、0 失败 | `/tmp/joplin-stage2-claude/core-final.log` |
| `cargo test --manifest-path packages/app-lite-server/Cargo.toml --offline` | 退出0；10 通过 | `/tmp/joplin-stage2-claude/server-final.log` |
| `git diff --check` | 退出0 | — |

本阶段没有重跑 GUI 全套：本会话没有改 `app-lite-gpui`，GUI 仍以 Codex 在 `becdff6d6` 的 1362 通过为最近一次全套结果。core 改动经 GUI 的原生审计间接覆盖。

### 阶段2 未通过 / 未完成

- 222 篇仍降级（13.3%）。主要是表格、带链接图片、列表嵌套深度、H4 及以上、有序列表起始号、数学公式、外链图片，都需要原生编辑器新增表示（表格、图片链接等），已发方案给负责 `app-lite-gpui` 的会话，尚未实现。降级笔记的文字和附件都保留，原始字节在审计表中。
- 疑似产品缺陷未确认：嵌套列表在编辑器中能缩进，但 codec 导出拒绝 depth≠0，可能导致缩进后无法保存。已转交该会话。
- 全库可读导出/恢复没有界面入口，由另一会话接线；导出没有取消；可读页面没有在浏览器里实际查看。
- 以下项都属于 Codex 或实机验收，本会话没做：迁移的视觉验收（Release 实机、表格/图文/长文/中文特殊字符的排版）、“从菜单导入 → 搜索 → 重开”的实机流程。
- `jex_qualification` 的预检报告仍按旧规则把 source_url、is_todo、混合文件夹列为阻断（02 号证据的遗留项），导入流程和验收都不用它，没有改。

- Claude 实施状态：阶段2 进行中（core 部分已提交，待验收）
- Codex 验收状态：未验收

## 与另一会话协作完成的原生支持（A/B）

- A（另一会话 `24d5a632c`）：确认“列表缩进后自动保存失败”属实并修复——codec 双向映射列表项 depth ↔ canonical `ListItem.style.indent`，原生 `MAX_LIST_DEPTH` 64→8 与 `data-indent` 上限一致；段落缩进仍拒绝。
- B（另一会话 `ef8517cf1`）：canonical `Inline::Image`/`Block::Image` 与原生 `BlockContent::Image` 新增 `link: Option<String>`，序列化 `<a href><img></a>`，经 `valid_link` 过滤；本版不做 Cmd 点击打开链接。该会话在 `jex_body.rs`/`jex_html.rs` 只做了补 `link: None` 的机械修改。
- 协作过程：B 期间本会话按约定暂停修改 `document.rs`/`jex_body.rs`/`jex_html.rs`；本会话唯一未提交文件 `tests/jex_body_conversion.rs`（嵌套列表 RED 测试）未被纳入对方提交。

本会话 `6af36711c` 的导入映射与测试：
- `jex_body.rs`：`list_items()` 递归展开同类嵌套列表；紧凑列表项的行内文字在嵌套列表开始处结束。Markdown 图片在外链内时取链接 mark 作为图片 `link`。
- `jex_html.rs`：`image()` 接收外层链接，写入 `link`。
- 测试：`same_kind_nested_markdown_list_keeps_levels_as_item_indent`（先失败：旧代码对 `- 一\n  - 二` 报阻断）；`linked_resource_image_keeps_its_external_link`（先失败：`LinkedImage`）。原“嵌套列表必阻断”“外链包图片必阻断”两条旧断言按新能力删除或改为“链到另一资源仍阻断”。core 全套 355 通过，日志 `/tmp/joplin-stage2-claude/core-links-lists.log`。

### `6af36711c` 的新鲜隔离导入与原生审计

| 命令 | 结果 | 日志 |
| --- | --- | --- |
| `import_verify`：JEX 副本 → `/tmp/joplin-stage2-import4.1wdHFf/imports` | 退出0，90.3 s；1666/31/2/64/425/4153/4127；**降级 166**（与只读审计 1500/166 一致）；发布 blob 0 不符，源 blob 缺失 0、多余 0 | `/tmp/joplin-stage2-import4.1wdHFf/verify.log` |
| `sqlite3 -readonly …/library.sqlite "select count(*) from notes where body_html like '%<a href=%><img%'"` 与 `… like '%<li data-indent=%'` | 含带链接图片的笔记 242 篇；含缩进列表项的笔记 13 篇（说明新结构确实进入了正文） | `/tmp/joplin-stage2-import4.1wdHFf/structure-counts.txt` |
| 原生加载/回写审计 → `/tmp/joplin-stage2-import4.1wdHFf/imports/all_notebooks-1790460896/library.sqlite` | 退出0；`real-copy notes=1666, failure_categories={}`，上述 242/13 篇均能原生打开并回写 | `/tmp/joplin-stage2-import4.1wdHFf/native-audit.log` |

注意：这次原生审计的构建包含另一会话尚未提交的 C 菜单改动（`app/actions.rs`、`library_menu.rs`、`ui/library_backup.rs`、`ui/library_import.rs`、`ui/library_import_tests.rs`、`ui/mod.rs`，见 `/tmp/joplin-stage2-import4.1wdHFf/gpui-dirty.txt`），不是纯净构建。审计入口不经过菜单，但 Codex 仍需在干净检出上复核。`451f556a8` 这一步（+2 篇）只重跑了只读审计和 core 全套（356 通过，`/tmp/joplin-stage2-claude/core-nonimage.log`），没有另做新鲜导入。

剩余 164 篇降级，已按收益发给负责 `app-lite-gpui` 的会话排期：表格 64、H4–H6 14、有序列表起始号 12、行内数学约 12、外链图片约 11（是否联网抓取涉及隐私，建议保持降级，由用户决定）。

## C：全库可读导出/恢复的菜单入口（另一会话 `dd13ee9fc`）

据该会话报告：菜单“导出整个资料库为可读 HTML…”“从可读导出恢复到新资料库…”已接上；导出前 flush，选择器返回时再 flush 一次；恢复到 `imported-libraries/` 下的新目录，失败时删除该目录，成功后走“打开导入的资料库”；新增两条挂载测试，GPUI 1368/1368。本会话没有改动或复核其实现，只在下方纯净构建中跑了全套。该会话提示：`mounted_scheduler_close_finishes_only_active_index_transaction` 在高负载下偶发卡死（钩子里 `recv()` 无超时），单独运行能通过。

## 表格结构清单（供表格方案评估，`81b996885`）

`JOPLIN_LITE_AUDIT_TABLES=1 cargo run -q --release --locked --example migration_fidelity_audit /tmp/joplin-migration-current.MeMdVA/all_notebooks-1790430342/library.sqlite`，只读，只输出计数：

- 含表格笔记 76 篇，表格 290 张。
- 列数：1 列 126、2 列 109、3 列 24、4 列 10、5 列 6、6–7 列 6、≥9 列 9。八成以上是 1–2 列，更像 Evernote 式“单格框/两栏”排版。
- 行数：1–5 行 231、6–20 行 42、21–100 行 14、>100 行 3。
- 列对齐：691 列全部未设置。
- 单元格内：`<br>` 803、链接 532、行内代码 78、图片 68、粗体 62、斜体 7、行内数学 6、HTML `<a>` 14；GFM 单元格只能放行内内容，所以没有单元格内的块级内容。

已把这份清单发给负责 `app-lite-gpui` 的会话作为表格方案输入；方案确认前双方都不改代码。H4–H6 与有序列表起始号由该会话改 `document.rs`（已锁定 `document.rs`、`jex_body.rs`、`jex_html.rs`、enml），完成后由本会话接导入映射（约 26 篇）。

## 纯净构建回归（HEAD `81b996885`，含另一会话 A/B/C）

为避免并行会话未提交改动混入，用 `git worktree add --detach <草稿区>/clean-81b996885 81b996885` 单独检出，`CARGO_TARGET_DIR` 指向独立目录从零编译；运行时检出无任何未提交改动。日志目录 `/tmp/joplin-stage2-claude/clean-81b996885/`。

| 命令（在该检出内） | 结果 | 日志 |
| --- | --- | --- |
| `cargo test --locked --bin velotype`（`packages/app-lite-gpui`） | 退出0；1368 通过、0 失败、1 忽略；未遇到偶发卡死 | `gpui.log` |
| 原生加载/回写审计 → `/tmp/joplin-stage2-import4.1wdHFf/imports/all_notebooks-1790460896/library.sqlite` | 退出0；`real-copy notes=1666, failure_categories={}` | `audit.log` |
| `cargo test --manifest-path packages/app-lite-core/Cargo.toml --features test-support --tests` | 退出0；356 通过、0 失败 | `core.log` |
| `cargo test --manifest-path packages/app-lite-server/Cargo.toml --offline` | 退出0；10 通过 | `server.log` |

编译警告仍存在（GUI 测试构建输出 150 行 `warning`），未清零。临时检出用后已 `git worktree remove`。这是本会话自测，不代替 Codex 验收。

## GPUI 侧（负责 `app-lite-gpui` 的会话）：实机发现、H4–H6、有序列表起始号

### 提交

| 提交 | 作用 | 先失败证据 |
| --- | --- | --- |
| `53e8192da` | Cmd-N 后可直接输入标题 | `typing_right_after_cmd_n_goes_into_the_new_notes_title` |
| `d3127d4d3` | 空列表项回车：嵌套项先减少缩进，顶层项变回段落，整个动作一步可撤销（对应 Evernote `list/keymap.ts` handleEnter）；图片缩放后保持选中 | `enter_on_an_empty_list_item_outdents_or_leaves_the_list` |
| `24d5a632c` | 列表项缩进可保存（见上方 A） | `indented_list_items_save_as_canonical_indent_and_reopen_at_their_depth` 等 2 条，失败原因为 `UnsupportedListDepth` |
| `ef8517cf1` | 图片链接（见上方 B） | 变异核验：把 link 固定为 None，两条往返测试失败 |
| `dd13ee9fc` | 可读导出/恢复的菜单入口；恢复失败会删除新建的空目录 | 两条挂载测试，未实现时编译即失败 |
| `c1275c5ea` | canonical `HeadingLevel::{Four,Five,Six}`，原生可保存 4–6 级标题 | `h4_to_h6_round_trip_as_headings`（编译失败）、`h4_to_h6_open_as_native_headings_and_save_unchanged`（`UnsupportedBlockKind Heading(4)`） |
| `27878ea83` | `Block::List.start`：只在有序列表且 ≠1 时存在，旧正文逐字不变；相邻列表只有后者没有 start 时才合并；原生侧用 Document 侧表（键为首项 NodeId）记录，全量与增量编号共用 `numbering_kind_for`，导出时在该项处新开列表 | codec 测试断言起始号往返失败；core 测试做变异核验 |

测试调整：有两条"保存失败要可见"的测试原本拿"缩进列表"和"4 级标题"当作无法保存的结构。这两者现在都能保存了，所以改为插入一张不属于本笔记的图片（`MissingResource`）来触发失败，被测的失败路径不变。

### 实机检查（Release 包，隔离 profile `/tmp/joplin-lite-accept-0927/profile`）

- 编辑器工具栏"更多"曾经点不开。排查结论：Claude 桌面窗口叠在 Joplin 窗口右侧，点击落到了 Claude 窗口上（computer-use 截图会隐藏 Claude 自身窗口）。把 Joplin 窗口挪开后，"更多"正常弹出；选中图片时列表和对齐项正确置灰。不是产品缺陷，未改代码。
- 仍未关闭的 GUI 问题：D2 输入撤销按字粒度；D5 退出列表后再输入会多出空段落（出现过，还没有分步复现）；缩放手柄在右边缘被裁掉一半；列表标记在左边缘被裁切。
- 还没做的实机项：真实拼音 IME 全矩阵、附件卡片与 Quick Look、中文搜索 UI、批量组织、菜单导入/备份/恢复（含新增的可读导出）、重启后复查。

### 偶发测试

- `ui::index_scheduler_tests::mounted_scheduler_close_finishes_only_active_index_transaction`：高负载下全套运行时卡死一次（测试 hook 里的 `recv()` 没有超时），卡了约 9 分钟后手动结束；单独运行 3/3 通过，重跑全套通过。
- `ui::index_scheduler_tests::mounted_scheduler_advances_a_derived_job_saved_after_open`：全套运行中失败一次（Pending ≠ Failed），单独运行 3/3 通过。
- 两者都与本节改动无关，未修改，只记录。

### 命令与结果（HEAD `27878ea83`，工作树内，非纯净检出）

- `cargo test --offline --bin velotype`（`packages/app-lite-gpui`）：1370 通过、0 失败、1 忽略。
- `cargo test --offline --features test-support`（`packages/app-lite-core`）：358 通过、0 失败。
- 两个 crate 的 `cargo fmt --check` 均通过。

- 表格：方案草案在 `docs/research/table-model-proposal.md`，等迁移侧确认后再实施。
- Claude 实施状态：以上提交待验收；实机矩阵未完成
- Codex 验收状态：未验收

## H4–H6 与有序列表起始号的导入映射（`56342c126`）

- 修复一次编译阻断：我在未提交的 `jex_body.rs:565` 写了 `start.and_then(Result::ok)`，这里的 `Result` 解析成了本文件的别名 `Result<T, JexBodyFidelityBlocker>`，而实际需要的是 `TryFromIntError`，于是报 E0631（Codex 复现日志 `/tmp/joplin-codex-review-27878-dirty-core.log`，cargo test 退出 101）。改为 `std::result::Result::ok`。
- 新增测试 `deep_headings_and_ordered_list_start_convert`。先失败证据：旧代码对 `####` 报 `UnsupportedHeading`。
- 修正我自己写错的用例：按 CommonMark，从非 1 开始的有序列表不能打断段落，所以 `1. 一\n   5. 五` 的第二行是续行文字，不是子列表。改为 `1. 一\n\n   5. 五` 后，用临时探针确认它按“Ordered list start number is not representable”阻断，临时文件已删除。
- 按新能力删除旧断言“`####` 必阻断”。
- core 全套 359 通过，日志 `/tmp/joplin-stage2-claude/core-h4-start.log`。

## 纯净构建回归 + 新鲜隔离导入 + 原生往返（HEAD `56342c126`）

做法：`git worktree add --detach` 单独检出，从零编译到独立 target 目录，检出里没有任何未提交改动（`worktree-dirty.txt` 为空）。日志目录 `/tmp/joplin-stage2-claude/clean-56342c126/`，用后已 `git worktree remove`。

| 命令（在该检出内） | 结果 | 日志 |
| --- | --- | --- |
| `import_verify`：只读 JEX 副本 → `/tmp/joplin-stage2-import5.sLTM2B/imports` | 退出0，91.3 s；1666/31/2/64/425/4153/4127；**降级 144**（与只读审计 1522/144 一致）；发布 blob 0 不符，源 blob 缺失 0、多余 0 | `/tmp/joplin-stage2-import5.sLTM2B/verify.log` |
| `sqlite3 -readonly` 统计正文结构 | 含 `<h4>`–`<h6>` 的笔记 19 篇；含 `<ol start=` 的笔记 14 篇 | `structure-counts.txt` |
| `cargo test --locked --bin velotype`（`app-lite-gpui`） | 退出0；1370 通过、0 失败、1 忽略 | `gpui.log` |
| 原生加载/回写审计 → `/tmp/joplin-stage2-import5.sLTM2B/imports/all_notebooks-1790475871/library.sqlite` | 退出0；`real-copy notes=1666, failure_categories={}`（上面 19/14 篇都能原生打开并回写） | `audit.log` |
| core `--features test-support --tests` | 退出0；359 通过 | `core.log` |
| server `--offline` | 退出0；10 通过 | `server.log` |

编译警告仍有 150 行，未清零。以上为本会话自测，不是 Codex 验收。

## 表格方案的迁移侧核查（`e06f214d7`，只读探针）

另一会话的方案草案在 `docs/research/table-model-proposal.md`。核查命令：`JOPLIN_LITE_AUDIT_TABLES=1 cargo run -q --release --locked --example migration_fidelity_audit <隔离副本>`，只输出计数。

- GFM 表格 290 张：colspan/rowspan、单元格块级内容、嵌套表格在语法上都不可能出现；超过 1000 行 0 张，超过 64 列 0 张。
- 内嵌原始 HTML 表格 7 张，全部在同一篇笔记里：rowspan 3 处、嵌套 table 3 处、单元格内块级元素 390 个。按方案这篇会整表降级；它现在本来就因原始 HTML 降级。
- 单元格里的转义管道 `\|` 26 处，GFM 解析为字面 `|`，canonical 需要保留；HTML 实体 2 处，解析器已解码成文字。
- 解析器会丢掉超出表头列数的单元格。按解析器重算后，丢失非空单元格的行为 0。我先前按 `|` 粗数得到的 445 是误报，已从探针移除。
- 单元格内图片 68 个：53 个指向资源，15 个是外链（这些笔记仍会因外链图片降级）。单元格内行内 HTML `<a>` 14 处，导入时仍按原始 HTML 阻断。

已回复另一会话：对方案 B 没有异议；canonical 层落地后，由本会话接 GFM 表格导入映射。

## 接手 GPUI 与表格导入（2026-09-27）

另一会话按用户指示停手，交底 `docs/superpowers/plans/2026-09-27-claude-gpui-session-handoff.md`（`24ec79d05`）；此后 `app-lite-gpui` 也由本会话负责。

### 集成回归（HEAD `c19db6275`，工作树无未提交改动）

`cargo test --offline --features test-support --tests`（core）退出0，367 通过；`cargo test --offline --bin velotype`（gpui）退出0，1372 通过、0 失败、1 忽略。日志 `/tmp/joplin-stage3-claude/integ-c19db6275/`。schema v11 对 GUI 无影响。

### GFM 表格导入（`22c9c233a`）

- 新增 `gfm_tables_convert_with_header_inline_content_and_resource_images`。先失败证据：旧代码报“Markdown block construct has no lossless canonical mapping”。
- 两条旧测试原来拿表格当“不支持”的夹具，改用定义列表；它们断言的行为（只降级失败块、阻断种类为 UnsupportedStructure）不变。
- 发现并修复一处降级质量回退：单元格含行内 HTML 的表格在降级路径被当作 HTML 解析，单元格被拆散。现在表格片段显示可读源码。
- core 全套 368 通过，日志 `/tmp/joplin-stage3-claude/core-tables.log`。

真实副本新鲜隔离导入与原生往返（HEAD `888a71c17`，含 `22c9c233a`；工作树无未提交改动）：

| 命令 | 结果 | 日志 |
| --- | --- | --- |
| `import_verify`：只读 JEX 副本 → `/tmp/joplin-stage3-import6.qGst4B/imports` | 退出0，92.6 s；1666/31/2/64/425/4153/4127；**降级 85**；发布 blob 0 不符，源 blob 缺失 0、多余 0 | `/tmp/joplin-stage3-import6.qGst4B/verify.log` |
| 统计含 `data-joplin-lite-table` 的笔记 | 75 篇 | `/tmp/joplin-stage3-claude/fresh-22c9c233a/tables.txt` |
| 原生加载/回写审计 | 退出0；`real-copy notes=1666, failure_categories={}`，75 篇含表格的笔记都能原生打开并原样回写 | `/tmp/joplin-stage3-claude/fresh-22c9c233a/audit.log` |

原生侧表格目前是只读原子块（另一会话方案第 1 步）：单元格不能编辑、文字不换行、单元格内图片只显示 alt。表格显示没有实机查看。

### 剩余 85 篇降级（只读探针）

特征组合里单独出现的：外链图片 11、原始 HTML 11、行内数学 11，另有 11 篇没有标记特征（原因分布见 `/tmp/joplin-stage3-claude/fidelity-after-tables.log`）。外链图片是否联网抓取需要用户决定；行内数学在 Evernote 核心里没有对应，保持降级。

## 同步、撤销合并、表格编辑（2026-09-27，`843498bc7`..`30540454f`）

### 同步

引擎、GUI 接线、真实规模本机演练与备份/恢复安全审查的完整证据在 `06-sync.md`（`40f9cca55`）。要点：

- 演练1 `/tmp/joplin-sync-drill.verqL1/drill.log` 的版本边界：HEAD `23e9d048e` 加当时未提交的 `infra/app-lite-server/` 与 `examples/sync_drill.rs`（其后原样提交为 `b55d1d87b`），**不含** `4bc60bdb6` 的修正（当时一次同步只传 100 条，所以跑了 61 轮）。结果：5916 个实体全部被接受；备份 4127 blob；从恢复出来的服务端全新拉取 5916 条；计数相同，4153 个附件重新哈希 0 不符，笔记内容摘要相同。
- `4bc60bdb6` 之后的演练与服务端回滚测试见 06-sync.md 同节。
- 所有演练只在本机临时目录进行。**NAS 未部署**：需要用户在本会话明确同意，并先读 `~/servers.md`；没有改动任何生产服务。

### 撤销合并（`917b711ec`）

依据 Evernote 使用的 prosemirror-history `newGroupDelay` 500 ms：500 ms 内相邻的连续输入和输入法提交合为一步撤销；换位置、删除、格式、结构操作都会断开合并。

### 表格编辑（`e3c5de65c`、`228dba2eb`、`30540454f`）

- 双击单元格打开单元格编辑框：Enter/Tab 提交并前进，Esc 取消；增删行列、插入表格（菜单“插入表格”）。每次改动是一个 `ReplaceTable`/`InsertTable` 事务，可撤销，保存回 canonical `Block::Table`。
- `30540454f`：单元格文字按列宽换行，行高取该行最高单元格；布局、点击命中、绘制共用一份行高估算，绘制裁剪在单元格内。新增测试 `long_table_cells_wrap_into_taller_rows_that_hit_testing_agrees_with`。

表格仍有的限制：

- 单元格编辑框是嵌套的完整编辑器，行内格式、链接、图片都保留；框里的标题、引用、代码块提交时变回普通行内文字（块样式丢失）；列表、表格等其他块级结构拒绝提交并提示。多个段落以换行合并。
- 网格里的单元格只显示纯文字：行内格式不显示（打开编辑框才看得到），内容仍原样保存。
- 单元格内图片只显示 alt 文字，不显示图片。
- 不支持合并单元格、列宽调整、单元格对齐（canonical 本来也不表示这些）。
- 行高是不经排版的估算（全角 14.5 px、半角 8 px）：极端字体下可能多留空白，或文字被单元格裁掉。
- 表格显示和编辑都没有实机查看。

## 最终产品验证（HEAD `30540454f`，纯净检出）

做法：`git worktree add --detach /tmp/joplin-final-claude/wt-30540454f 30540454f`，独立 target 目录从零编译；检出里无未提交改动（`worktree-dirty.txt` 为空）。日志 `/tmp/joplin-final-claude/clean-30540454f/`，用后已 `git worktree remove`。

| 命令（在该检出内） | 结果 | 日志 |
| --- | --- | --- |
| core `cargo test --offline --locked --features test-support --tests` | 退出0；382 通过、0 失败 | `core.log` |
| server `cargo test --offline --locked` | 退出0；15 通过 | `server.log` |
| protocol `cargo test --offline --all-features` | 退出0；0 个测试（该 crate 没有自己的测试，HTTP 客户端由 core 的 `sync_http` 覆盖）。第一次带 `--locked` 失败退出101，因为该 crate 没有自己的 Cargo.lock，属环境问题 | `protocol.log` |
| gpui `cargo test --offline --locked --bin velotype` | 退出0；1383 通过、0 失败、1 忽略 | `gpui.log` |
| `import_verify`：只读 JEX 副本（SHA256 `8544080e…74906f500`）→ `/tmp/joplin-final-import.t7CAGW/imports` | 退出0，92.2 s；1666/31/2/64/425/4153/4127；**降级 85**；发布 blob 4127 个重新哈希 0 不符，源 blob 缺失 0、多余 0 | `/tmp/joplin-final-import.t7CAGW/verify.log` |
| 统计含 `data-joplin-lite-table` 的笔记 | 75 篇 | `tables.txt` |
| 原生加载/回写审计 | 退出0；`real-copy notes=1666, failure_categories={}` | `audit.log` |
| `scripts/package-notes-macos.sh /tmp/joplin-final-claude/dist-notes` | 退出0；`/tmp/joplin-final-claude/dist-notes/20260927T075224Z-30540454f/Joplin Lite.app`，0.7.2 (16250)，`worktree_dirty_for_app_sources: no`，binary SHA256 `25665299…f3f0069e` | `package.log` |
| `scripts/measure-product-memory.sh`，profile 为新鲜导入库的拷贝 `/tmp/joplin-final-claude/mem-profile`（1.2 GB），3 次、每次静置 10 s | 退出0；RSS 79.9 / 70.6 / 69.2 MiB | `memory/summary.txt` |

已安装的 App 和原资料库都没有被写入或替换。GPUI 编译警告 150 行，未清零。以上是本会话自测，**不是 Codex 验收**。

## 仍未完成 / 需要用户或 Codex

- **NAS 部署**：等待用户在本会话明确同意；部署前先读 `~/servers.md`，只用独立目录、端口和容器。
- **实机检查**：本会话没有操作界面的工具。表格显示与编辑、同步菜单与状态、撤销合并、输入法，需要 Codex 或用户用上面的 Release 包在隔离 profile 上实机检查。
- **同步**：自动同步、冲突副本列表、永久失败界面都已补上，见文末两节。
- **迁移**：85 篇降级，其中外链图片是否联网抓取需要用户决定；行内数学在 Evernote 核心里没有对应，保持降级。
- 表格限制见上节。

## 同步问题列表与手动重试（`2101461fd`、`1863f92f3`）

### Evernote 参照与独立设计

| 文件 | SHA256 | 看到的行为 |
| --- | --- | --- |
| `main-readable/src/modules/42665__module-42665.js` 第 215–260 行 | `0a965cac…f7336` | `MutationUpsyncActivity`：`processMutationUpsyncResults` 分出 success/retry/failed。只有 retry 放回队列（`unshiftUnsyncedMutations`）；failed 不再排队，只 `rebuildOptimisticGraph()` 并写日志，也就是本地的乐观修改回到服务端状态 |
| `renderer-readable/chunks/9435.js` 第 41196–41200 行 | `236728fa…d99e` | 笔记列表只有一个“未同步”小图标（`localChangeTimestamp > 0`，提示文案 `Note.snippet.unsyncIndicator.tooltip.unsynced`）。没找到失败列表 |

采用 Evernote 的做法：失败的操作不自动重发，可重试的错误留在队列里。
**独立设计（不是复刻）**：Evernote 会丢掉本地修改，这里不丢。被拒的上传挂起，连同原因列出，由用户点“重试”，从当前本地内容新生成一次上传。“同步问题”列表和重试按钮都是本项目自己的界面。

### 修复前的问题（先失败证据）

被服务器永久拒绝的操作留在 `sync_inflight`，之后每次同步都原样重发。同一次同步里如果有别的内容被接受，推送循环会再发一次。服务端按 op_id 缓存结果，所以永远得到同一个拒绝。另外，该实体一直占着 in-flight 位置，之后的本地修改永远不会上传。

新测试在只加接口、不加挂起逻辑时的结果（`/tmp/joplin-final-claude/sync-failures/red.log`）：第一次同步就发送了 2 次（期望 1）；重试后的测试里发送 3 次（期望 2）。

### 实现

- core `2101461fd`：
  - `sync_prepare_inflight` 跳过已记录失败的 op，这类 op 也不占批次名额。
  - `SyncFailure` 增加 `title`（本地标题）、`updated_time`、`can_retry`（被拒的上传为 true，跳过的下载为 false）。
  - `sync::retry_failure(op_id)` 在一个事务里删掉挂起的 op 和它的失败记录，保留 outbox，下次同步用当前本地内容生成新 op，只发一次。对下载侧失败或已不存在的项返回 false，不改动任何东西。
- GUI `1863f92f3`：
  - 状态栏常驻显示“N 项同步问题，点此查看”，数字来自持久化的记录，不是本次同步的报告；“仅保存在本机、待同步”仍单独显示。
  - 点击状态栏或菜单“同步问题…”打开列表。每行显示类型、本地标题、中文原因（超过 4 MiB 上限时提示缩短或拆分）。
  - 被拒的上传有“重试”按钮：标记后立即同步一次，同步进行中不显示按钮。下载侧失败说明无法重试、本机内容未受影响。
  - 界面从不自动清除或自动重试。

失败记录只在以下情况消失：重试后被接受；服务端对该实体的新版本到达（本机修改按既有规则成为冲突副本）；服务端从较早备份恢复后的重新对账（被拒的内容会重新上传，仍被拒时会重新列出）。

### 测试

| 命令 | 结果 | 日志 |
| --- | --- | --- |
| core `cargo test --offline --features test-support --test sync_failures --test sync_two_clients --test sync_push --test sync_resources --test sync_http`（工作树内） | 退出0；2 + 8 + 5 + 2 + 3 通过 | `/tmp/joplin-final-claude/sync-failures/green.log` |
| 纯净检出 `1863f92f3`：core `--features test-support --tests` | 退出0；384 通过 | `/tmp/joplin-final-claude/clean-1863f92f3/core.log` |
| 纯净检出：`sync_drill` 示例编译 | 退出0 | `drill-build.log` |
| 纯净检出：server | 退出0；15 通过 | `server.log` |
| 纯净检出：gpui `--bin velotype` | 退出0；1384 通过、0 失败、1 忽略 | `gpui.log` |

新增或加强的测试：

- `sync_failures.rs::a_rejected_upload_is_listed_parked_and_uploads_current_content_after_a_retry`：真实服务端拒绝超过 4 MiB 的笔记。断言：列出标题；再次同步和本地修改都不会重发；其他笔记照常上传；重试后上传的是修改后的内容，另一台设备收到。
- `sync_failures.rs::a_retry_that_fails_again_is_listed_again_after_one_attempt`：未修复就重试时只多发 1 次，用新 op_id，再次列出，之后不再发送。
- `sync_two_clients.rs` 的畸形远端正文用例：下载侧失败 `can_retry=false`，重试返回 false，记录保留。
- 挂载 UI `ui::sync_tests::a_refused_upload_is_listed_and_retried_only_from_its_button`：本机 HTTP 服务端，真实点击状态栏和“重试”按钮，另一台设备收到修改后的内容。gpui 0.2.2 的 `debug_bounds` 不清除旧帧条目，所以“按钮已消失”用 shell 状态断言，不用 `debug_bounds().is_none()`。

以上都是本会话自测，不是 Codex 验收。没有实机点击，也没有接触 NAS 或生产服务。

## 同步收尾：未保存编辑不再丢失、自动同步、冲突副本列表（`2002ba400`..`8166eedd1`）

### 修复：同步结束时丢弃当前笔记的未保存编辑（`2002ba400`、`ad09c5636`）

同步结束后，编辑器总是从存储重新挂载。在同步进行中输入、还没来得及保存的文字会被静默丢弃。**即使这次同步根本没改动当前笔记**也会丢。手动同步会先保存，所以丢失发生在同步进行中的输入；自动同步会让这种情况变得常见，因此先修这个。

先失败证据：临时把 `finish_sync` 恢复为旧行为（无条件重新挂载）后跑新测试，两条都失败（`/tmp/joplin-final-claude/sync-failures/overtaken-red.log`）：

- 远端改了当前笔记：没有冲突副本，本机输入丢失。
- 远端只新增了别的笔记：编辑器里只剩“原文”，输入丢失。

修复：

- 只有当同步替换了编辑器所基于的版本时，才重新挂载编辑器。
- 此时如果有未保存的编辑，先用新增的核心方法 `save_overtaken_edit_as_conflict_copy` 另存为“标题（冲突副本）”：放在同一笔记本，复制标签；原笔记已被彻底删除时放进默认笔记本。这与拉取时“已保存但未同步的编辑”的处理一致：远端版本占用原 id，本机内容成为冲突副本。
- 如果另存失败，不替换编辑器，状态栏说明情况并请用户先复制。
- 正在输入、尚未确认的输入法候选文字不算编辑，不会被保存。

### 自动同步（`39bce21c6`）

| 参照 | 文件 / SHA256 | 内容 |
| --- | --- | --- |
| Evernote | `68232__sync-manager.js` `2870d3e0…9d7a`，`getNextActivity` | 有未同步修改且队列空闲时，安排一次 5 s 后的后台上传（`runAfter: now + MILLIS_IN_ONE_SECOND * 5`）；调度器最多等 10 s 再检查 |
| Evernote | `21551__module-21551.js` `a1d08fd6…811d` | `SHORT_RETRY_TIME` 30 s，`ACTIVITY_RECURRENCY_TIME` 5 min（常量出处；它们在 Evernote 里用于别的活动，这里只借用数值） |

实现：配置了同步后，每 5 s 检查一次：

- 有可发送的本地修改（待同步数减去挂起的被拒项）时同步。
- 距上次同步满 5 分钟时同步一次，拉取远端修改。**这是独立设计**：Evernote 下载靠服务端推送，我们的服务端没有推送。
- 当前笔记有未保存输入时不同步。
- 服务器不可达时退避：30 s，之后翻倍，最长 5 min。
- 凭据或协议错误后暂停自动同步，状态栏写明，直到用户手动“立即同步”。

同时修正状态文案：服务器不可达时以前显示“已同步：上传 0、下载 0…”，现在显示“未能连接同步服务器，稍后自动重试”，并保留“待同步”计数。

### 冲突副本列表（`4c1ddab43`、`8166eedd1`）

Evernote 参照 `32150__localization-catalog.js`（`5fff17c5…70b4`）：`Home.widgetDesc.scratchPad.conflicts`“{N} conflicting versions — Select the items you want to keep”，`Home.content.conflict.label`“Conflict - {NOTE_LABEL}”。

实现：

- core 的 `sync_conflicts()` 列出本机产生、尚未处理且副本仍在的冲突副本；`sync_resolve_conflict` 写 `resolved_time`（表中原有该列，无需迁移）。副本移入废纸篓也视为已处理。标记已处理不会删除副本笔记。
- 状态栏显示“N 个冲突副本待处理，点此查看”。同步问题面板里每条冲突副本有“打开”（选中副本）和“已处理”两个按钮。
- 限制：冲突记录只在产生副本的设备上有；其他设备收到的副本是普通笔记，不进这个列表。

### 测试

| 命令 | 结果 | 日志 |
| --- | --- | --- |
| 纯净检出 `8166eedd1`：core `--features test-support --tests` | 退出0；385 通过 | `/tmp/joplin-final-claude/clean-8166eedd1/core.log` |
| 纯净检出：server | 退出0；15 通过 | `server.log` |
| 纯净检出：gpui `--bin velotype` | 退出0；1389 通过、0 失败、1 忽略 | `gpui.log` |
| 纯净检出：`package-notes-macos.sh /tmp/joplin-final-claude/dist-notes` | 退出0；`20260927T101320Z-8166eedd1/Joplin Lite.app`，0.7.2 (16259)，`worktree_dirty_for_app_sources: no`，binary SHA256 `e800ae4f…e89443f` | `package.log` |
| 上面的包在导入库拷贝 `/tmp/joplin-final-claude/mem-profile` 上空闲内存，3 次各静置 10 s | 退出0；RSS 69.2 / 70.0 / 71.5 MiB（该拷贝未配置同步，自动同步不运行） | `memory/summary.txt` |

新增的挂载测试（本机真实 HTTP 服务端，测试时钟快进）：

- `an_unsaved_edit_overtaken_by_a_sync_is_kept_as_a_conflict_copy`：未保存编辑成为冲突副本，编辑器显示远端版本；状态栏计数；点“打开”选中副本；点“已处理”后从列表消失，副本仍在。
- `a_sync_that_leaves_the_open_note_alone_keeps_its_unsaved_edit`：同步没有改动当前笔记时，编辑器保留输入，之后正常保存。
- `sync_runs_on_its_own_after_local_changes_and_periodically_for_remote_ones`：5 s 内上传；没有待发内容时 60 s 内不同步；5 分钟后拉到远端新笔记。
- `automatic_sync_waits_for_unsaved_input`：有未保存输入时，30 s 内 0 次自动同步。
- `an_unreachable_server_backs_off_and_bad_credentials_pause_until_a_manual_sync`：退避间隔 30 s、60 s；token 错误后 10 分钟内 0 次尝试，状态栏写明已暂停；手动同步成功后恢复。

core 新增 `an_unsaved_edit_overtaken_by_a_remote_version_is_kept_as_a_conflict_copy`，并在并发编辑用例中覆盖列表与“已处理”。

以上是本会话自测，不是 Codex 验收。没有实机点击，没有部署 NAS，没有改动生产服务、原资料库或已安装的 App。

仍未完成：下载侧失败无法手动忽略，会一直列着；实机检查；NAS 部署（需要用户在本会话明确同意）；外链图片是否联网抓取（需要用户决定）；GPUI 编译警告约 150 行。

## 变更推送与弱网（`82cd06ec9`..`438083ddc`）

### 参照

| 来源 | 位置 | 采用了什么 |
| --- | --- | --- |
| Evernote NSync | `66578__n-sync-event-manager.js`（`310e012c…7336`）`createSyncEventSource`、`handleErrorEvent`；`59030__module-59030.js`（`c76c71df…b0`） | 用 Server-Sent Events 长连接（`/v1/connect`，Bearer）；重连带上次位置，回退 60 s 重放；退避 3/15/15/15/30/60 s 加 ≤1 s 抖动；401/403 走认证错误；暂停 60 s 后断开 |
| yuchbox | github.com/yuchting/yuchbox `c4e3d7c8`：`fetchMgr.java` 395–482、640–660，`fetchEmail.java` 1099–1122、1663–1712，客户端 `recvMain.java` 280–282 | 客户端发起 TLS 长连接，认证限时 10 s；可配置心跳（1/3/5/10/30 min）；推送后逐条确认，没确认的按 3 min×次数重发，发满 10 次放弃 |
| 用户的 stego-reader | `custom-protocol-tui` 分支 `572c12d9`：`server/tcp.js`、`docs/superpowers/specs/2026-06-24-matrix-removal-tcp-receipts-design.md`、`plans/2026-07-12-ios-connection-stability.md`、`specs/2026-07-20-delivery-receipt-hms-fallback-design.md`、`specs/2026-07-25-stego-zero-operation-reliability-design.md` | 半开连接检测（客户端 60 s PING、120 s 无 PONG 断开；服务端空闲 180 s 发 PING、300 s 销毁）；连接 10 s 和认证 10–15 s 超时；3→5→10→30→60 s 退避，只在认证成功后重置，认证失败停止重连；用代次防止同时存在两条连接；“连接在不代表送到了”；弱网帧 gzip |

本项目的取舍：推送只发“服务端最新游标变了”的**提示**，不带数据。数据仍走按游标的拉取（重复执行无副作用，服务端确认后才算同步）。所以丢失一条通知只影响延迟，不影响正确性，不需要 yuchbox 的逐条确认重发，也不需要 Evernote 的回退重放。

### 实现

- 协议 `82cd06ec9`：`GET /v1/events`。流程是 `hello{head}`，然后每次有变更发 `changed{head}`；空闲时每 25 s 发一行注释心跳；300 s 后发 `bye` 并关闭。
- 服务端 `82cd06ec9`：
  - 对照实验发现 tiny_http 没有单连接超时：4 个工作线程时，6 个在上传中途失联的客户端让服务端 120 s 无响应（`/tmp/joplin-final-claude/push/socket-timeout-red.log` 记录同类结论）。
  - 实测 macOS 上 accept 出来的连接**不继承**监听 socket 的超时，Linux 行为不同，不能依赖。
  - 因此改为自己的最小 HTTP/1.1 实现：每个连接读写超时 60 s；最多 64 个连接、32 条推送流；每个连接只处理一个请求；关闭前有上限地读掉剩余请求体，保证超大请求仍能读到 413；只接受带 Content-Length 的请求体。tiny_http 依赖已移除。
- 客户端推送流 `867bf57c2`：每次读取的超时为 2.5 个心跳（62 s），静默超过就判定为死连接。用“黑洞”代理（不关连接、不再转发任何字节）验证能判定。
- 游标 `78582275c`：上传有被接受的内容时，末尾再拉取一次，把游标推进到服务端最新位置。自己上传引起的通知因此不再触发多余的同步；自己的变更在拉取时跳过，不计入下载数。
- 界面连接控制器 `8a665831b`：
  - 同一时间只有一条连接，靠代次区分，旧的读取线程会自行退出。
  - `hello` 必须在 15 s 内到达；退避 3/5/10/30/60 s，从第二次起加 ≤1 s 抖动，只在收到 `hello` 后重置；收到 `bye` 立即重连；401 暂停自动同步；手动同步会跳过退避等待。
  - 只有服务端宣布的游标不等于本地游标时才同步；每 5 分钟的拉取保留作兜底。
  - 同步面板显示连接状态。
- 上传批量 `438083ddc`：从 3 MiB 降到 1 MiB。3 MiB 在约 20 KB/s 的弱网上要约 150 s，超过客户端 120 s 超时，这一批会永远重发；1 MiB 约 50 s。

### 测试与对照

| 命令 | 结果 | 日志 |
| --- | --- | --- |
| server `cargo test --offline` | 退出0；20 通过（新增 `tests/events.rs` 5 条，其中 2 条走故障代理） | `/tmp/joplin-final-claude/push/server-green.log` |
| 对照：socket 超时改为 60 s | 卡住的连接占满名额，新请求得不到服务，测试失败 | `socket-timeout-red.log` |
| core `--features test-support --tests` | 退出0；387 通过 | — |
| 对照：批量恢复为 3 MiB | 最大请求 2 462 866 字节，测试失败 | `batch-red.log` |
| gpui `--bin velotype` | 退出0；1392 通过、1 忽略；同步挂载测试连续 5 次通过 | `gpui.log` |

新增的挂载测试（本机真实服务端，测试时钟快进）：

- 另一台设备的修改靠通知在测试时钟不动的情况下到达；没有变化时 60 s 内不同步；5 分钟定时拉取仍在。
- 收到 `bye` 后立即重连；服务端消失后在 3 s、5 s（加抖动）时重连。
- 服务端连上 TCP 但不回 `hello`：14 s 时仍在连接中，15 s 时放弃，再过 3 s 重试。
- 另有退避序列的单元测试。

### Release 二进制本机演练（`438083ddc`，工作树无未提交改动）

脚本 `/tmp/joplin-final-claude/push/drill.sh`，目录 `/tmp/joplin-push-drill.*`（见 `drill-dir.txt`）。服务端是 Release 版 `app-lite-server`，监听随机本机端口；用 `curl -N` 挂住推送流。

| 步骤 | 结果 |
| --- | --- |
| A（新鲜导入库的拷贝）上传 | 4 轮，110.7 s；接受 5916 个实体；附件重新算哈希 4153 个、0 不符 |
| 推送流 | `hello {"head":0}`，60 条 `changed`，最后一条 `{"head":5916}` |
| B（全新空库）拉取 | 42.5 s，拉到 5916 个；计数与 A 相同；附件重新算哈希 0 不符；笔记内容摘要相同 `6b9bba86…1cdf` |
| 不带令牌访问 `/v1/events` | 401 |
| 服务端重启后空闲 55 s | `hello {"head":5916}`，之后 2 条 `: ping` |

### 仍未做

- 请求和响应的 gzip 压缩。
- 没有监听 Mac 睡眠唤醒和网络切换来立即重连；目前靠 62 s 静默判定加退避。
- 反向代理（Caddy 或 NAS 自带）下的实测，需要在 NAS 部署时做；服务端已发 `X-Accel-Buffering: no`。
- 客户端目前只支持明文 HTTP（ureq 未启用 TLS），公网访问要靠反向代理终止 TLS。
- 移动端的厂商推送（HMS/APNs）兜底，桌面端不需要。

以上是本会话自测，不是 Codex 验收。没有部署 NAS，没有改动生产服务、原资料库或已安装的 App。
