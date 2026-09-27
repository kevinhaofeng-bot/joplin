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
