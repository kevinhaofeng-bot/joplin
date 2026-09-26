# 02 迁移闭环（ENEX、Joplin JEX）

- 任务：任务2
- 实现提交：`a287133a2`（发布新资料库）、`072890309`（菜单导入/切换）、`a831cf5cb`（真实 ENML 样式/复选框/tel）、`03838eb2b`（真实 JEX 规范化）
- 测试构建 SHA：`03838eb2b`

## 读过的 Evernote 源码

| 文件（`/Users/kevinhao/Projects/joplin-reconstruction/evernote-11.32.5/main-readable/src/modules/`） | SHA256 | 符号 / 行段 |
| --- | --- | --- |
| `36364__note-import-mutation-import-enex-file-import-file-from-url.js` | `6aa132e3…a1d305` | `L()`（importEnexFile）197-400：SAX 流式解析；`note` 结束即 `P.execute(L)` 写入在线账户；标签按 label 查找/创建并缓存；进度回调返回 false → 抛“importCanceled” |
| `64997__import-file.js` | `6f21423b…fbd4` | `k()`：按扩展名分派 ENEX / TXT / 其他文件（作为附件新建笔记） |

行为对照：输入文件 → 按扩展名分派 → 逐笔记解析 → **Evernote 直接写入活动账户**，取消时已导入的笔记保留 → 用户看到部分导入。

采用：按扩展名分派、流式解析、进度与取消、同名标签合并。
**独立设计（非 Evernote 行为）：**先在独立 staging 资料库完成全部导入与重开校验，再原子发布为新的资料库目录；活动资料库从不写入；取消/失败不留下半成品。Joplin JEX 格式适配属于本项目适配，不来自 Evernote。

## 实现

| 文件 | 作用 |
| --- | --- |
| `app-lite-core/src/import_export/import_commit.rs` | `publish_staged_library`（完整性、外键、逐 blob SHA-256+长度复核 → fsync → rename 到不存在的目标）、`library_counts`、`import_library_file`（扩展名分派、唯一命名、degraded 汇总） |
| `enex.rs` / `enml.rs` / `document.rs` | 不支持结构按笔记降级为可读文本并报告；Evernote 展示性属性、span 样式→标记、`<div><en-todo/>` 清单、`tel:` 链接 |
| `jex_body.rs` / `jex_stage*.rs` / `import_export.rs` | 同上降级；资源元数据规范化；笔记额外字段只保留在原始审计并计数；混合文件夹映射；空/片段链接保留文字 |
| `app-lite-gpui/src/ui/library_import.rs`、`library_profile.rs`、`library_menu.rs` | 菜单“导入 Evernote / Joplin 资料…”、后台导入、取消、结果与降级报告、“打开导入的资料库”（先 flush，再写 `active-library`、替换窗口） |
| `app-lite-core/examples/import_verify.rs` | 验收探针：扫描计数、导入、从磁盘逐 blob 重新哈希、与源 blob 集合比对；只输出计数/ID/哈希 |

## 测试（均先 RED 后 GREEN，RED 输出见会话）

新增：`tests/import_commit.rs` 7 项（发布计数与哈希、目标存在拒绝、发布前取消、损坏 blob 拒绝、改名前注入失败、按扩展名导入并唯一命名、未知扩展名拒绝）；`enex_stage::unsupported_enml_degrades_…`；`enml_convert` 4 项；`jex_note_only_stage` 2 项；`jex_resource_stage::real_library_resource_metadata_…`；`jex_folder_stage::folder_user_times_and_…`；`jex_body_conversion::empty_and_fragment_links_…`；GUI `ui::library_import_tests` 3 项；`library_profile` 2 项。

变异核验：取消不置位 → `mounted_import_cancel_…` 失败；`resolve_active` 恒返回 base → `records_and_resolves_…` 失败。

因策略变更而改写的旧测试（均在 ledger 记 Ruling）：整库因 Fidelity/缺附件/缺字段/混合文件夹/MIME 被拒的断言改为“降级或规范化并报告”；“后续笔记失败清理 staging”改用废纸篓笔记（仍为硬错误）。

最终：`cargo test --manifest-path packages/app-lite-core/Cargo.toml --tests` **265/265**；`cargo test --locked --bin velotype -- --test-threads=1` **1334/1334**。

## 真实资料副本验收（无正文/标题入报告）

副本目录 `/tmp/joplin-lite-t2-accept/src/`（`cp -p`，原文件只读、未改）。

| 源 | 修改时间 | SHA256（副本=原件） |
| --- | --- | --- |
| `~/JoplinBackup/default/all_notebooks.jex`（1,172,484,096 B） | 2026-09-25 22:15:53 | `8544080e…74906f500` |
| Dropbox `…/案件进展备份/` 3 个 `.enex` | 2019-03-15 | `69d3d2c6…`、`0576faba…`、`df9b6210…` |

JEX（`import_verify`，Release，88 s）：

| 项 | 源扫描 | 导入后 |
| --- | --- | --- |
| 笔记 | 1,666 | 1,666（回收站 0） |
| 文件夹 | 31（顶层 17、二级 14） | 2 个笔记本组 + 30 个笔记本（15 顶层 + 14 子 + 1 混合文件夹拆分）+ 默认 1 = 31 |
| 标签 / 关系 | 64 / 425 | 64 / 425 |
| 附件 | 4,153 条元数据、4,127 个不同 blob | 4,153 资源、4,127 blob；**从磁盘逐个重新哈希 0 不符；源 blob 缺失 0、多余 0** |
| 搜索 | — | FTS 行 1,666，search_queue 0，sync_outbox 0，`integrity_check` ok |

降级为可读文本的笔记：**376 / 1,666（22.6%）**，正文文字与全部附件保留（附件以卡片形式，在原位置信息丢失）。原因：Markdown 分隔线/表格/引用/代码块 180、图片外包链接 58、原始 HTML 42、附件与文字同段 20、其他行内结构 20、其余 ≤12。这些需要编辑器新增块类型，超出任务1/2“不扩大富文本能力”范围，列为已知缺口。另：笔记元数据（来源、排序、坐标、作者、source_url、待办状态）仅保留于原始审计表 `jex_stage_note_audit`。

ENEX：3/3 导入成功，各 1 篇；2 篇完整转换，1 篇含状态冲突的重复复选框按规则降级；附件 0。

## Release 实机

未做。屏幕控制权限此前被拒（任务1），菜单导入、进度、取消、切换资料库均只有挂载测试证据。**计划门：“Codex 独立从副本完成导入、搜索和重开”未满足，迁移不能称完成。**

## 未解决 / 下一步

- 22.6% 笔记降级（上）；ENEX 缺附件仍整库拒绝（与 JEX 的悬空链接处理不一致）。
- `jex_qualification`（预览报告）仍按旧规则把 source_url、is_todo、混合文件夹列为阻断；UI 和验收不使用它。
- Finder 打开 `.enex/.jex` 仍显示“暂不支持导入”，未接到导入流程。
- 同名 GIF/WebP 等内联图片能否在 GPUI 编辑器解码未验证（任务5）。

- Claude 实施状态：提交待验收（实机未做）
- Codex 验收状态：未验收
