# 05 多模态与搜索完整使用链

- 任务：任务5
- 实现提交：`a89acda94`（SQLite 文件锁丢失导致写入丢失）、`224840804`（搜索语法、日历日期、提取失败有界重试、探针）
- 测试构建 SHA：Release `velotype` sha256 `974dcd4541e354a1c7a14a48d4746dfddcf015232c1a041dff09a73dd36e6e32`（`a89acda94`，端到端验收用）

## 读过的 Evernote 源码

| 文件（`…/main-readable/src/modules/`） | SHA256 | 符号 / 行段 |
| --- | --- | --- |
| `51029__search.js` | `da68ccae…c09798473485978d10cc1abfe8a82d2199e101ab` | `u()` 分词（引号短语、`prefix:value`）；`y()` parsePlaintextQuery：`contains:`、`intrash`、notebook/stack/tag/author 等前缀转过滤器，其余为文本 |
| `21652__module-21652.js` | `6c54a1b1…471d51b4` | `SearchFilterValuePrefix`：`contains`、`intrash`、`stack` |
| `18766__module-18766.js` | `11b79d63…9e17eee5c` | `SuggestedType`（notebook/tag/stack/created/updated…）、`ContainsId.Attachment="attachment"` |
| `99128__search-event.js` | `cc0aa24c…bbc76ff` | 搜索事件埋点（只确认查询流向，无语法） |

采用：`contains:attachment`、`intrash:`、引号短语、前缀过滤。规格 7.5 的 `is:trash`、`has:attachment` 同时支持。
**证据缺口：**`created:day-N`、`YYYYMMDD` 语法在本重建中未找到解析实现（在 Conduit 服务端/离线引擎）；本产品实现为独立设计（本地日历日，`day-N` 从 N 天前本地零点起、开区间）。

## 发现并修复的缺陷（均先 RED）

1. **SQLite 文件锁被丢弃 → 已提交写入丢失（严重）。**`DatabaseFile::matches_profile_child` 为校验身份打开并关闭数据库文件的额外描述符，`DatabaseFile` 还整个生命周期持有一个。关闭任一描述符会释放本进程 SQLite 在该文件上的全部 POSIX 锁（sqlite.org/howtocorrupt.html §2.2）。另一进程以读写方式打开同一资料库时认为自己是最后连接，检查点后删除活动 WAL；应用随后提交进已被删除的 WAL，写入对其他连接永久丢失。实测：OCR 结果日志显示 `Indexed`，数据库中仍为 `pending`，运行中 `-wal/-shm` 消失。同进程第二窗口关闭时同样会丢掉第一窗口的锁。
   修复：身份用 `fstatat` 获取/复核，不再打开描述符；`DatabaseFile` 只保存 device/inode。
   回归：`repository_flow::another_process_cannot_delete_the_wal_while_the_repository_is_open`、`closing_a_second_repository_on_the_same_file_keeps_the_first_ones_locks`（修复前均 FAIL：WAL 被删）。
2. **规格语法 `is:trash`/`has:attachment` 被当作普通文本**，日期过滤只接受毫秒时间戳。回归 `search::filter_matrix_executes_spec_and_evernote_syntax_with_calendar_days`、`relative_day_filter_starts_at_local_midnight_n_days_ago`；变异核验（去掉 `is:` 别名、日期退回毫秒解析）均使矩阵失败。
3. **提取失败无重试入口**（核心 `retry_derived_text` 无 GUI 调用）。改为打开资料库时有界自动重试：`unavailable/timeout/failed` 且 attempts<3 回到 pending；`unsupported/parse/locked/no-selectable-text/too-large` 保持失败可见。回归 `repository_flow::reopening_requeues_transient_derived_text_failures_up_to_three_attempts`。

**测试流程缺陷：**`tests/search.rs` 等 6 个文件带 `#![cfg(feature = "test-support")]`，规划基线命令 `cargo test --tests` 不含该 feature，会**整文件跳过**。此前任务 0-4 报告的 core 计数不含这些文件；本任务起统一用 `--features test-support`，并确认之前的改动未破坏它们。

## 搜索矩阵（执行级，`parse_at` 固定 UTC+9）

标题/正文、中文子串、引号短语、`notebook:`、`stack:`、`tag:`、`created:YYYYMMDD`、`updated:A..B`、默认排除回收站、`is:trash`/`intrash:`、`has:attachment`/`contains:attachment` 全部命中预期集合。既有覆盖：附件文件名 FTS、`filename:`/`mime:`、否定、分页、中文短词、缩略图一致性（`tests/search.rs` 其余 19 项）。

## 端到端（Release，隔离 profile，无屏幕控制）

`multimodal_probe seed` 建库：截图（Pillow 渲染 “ZEPHYRQUARTZ 42”）、可选文字 PDF（“MARIGOLDVECTOR report”）、音频、视频、纯文本附件各一篇 → 启动 Release 应用（后台索引，无需交互），外部进程每 2 s 读写方式轮询 → 关闭 → 核心 API 搜索：

| 查询 | 结果 |
| --- | --- |
| 提取子进程直接运行 | PNG → `ZEPHYRQUARTZ 42`（Vision OCR）；PDF → `MARIGOLDVECTOR report`（PDFKit）；截断 PNG → `image-no-text`，退出码 2 |
| `ZEPHYRQUARTZ` | 截图笔记，`matched_resource=true` |
| `MARIGOLDVECTOR` | PDF 笔记，`matched_resource=true` |
| `has:attachment` | 5 篇全部 |
| `memo`（文件名） | 音频笔记 |
| 运行中文件 | `library.sqlite-wal/-shm` 保持存在（修复前被删除） |

音频/视频/纯文本不进入提取队列（`is_extractable_mime` 只含 PDF/PNG/JPEG）。

## 与用户全局约束的冲突（未改动）

图片 OCR 使用 macOS Vision 框架（仅在提取子进程中 dlopen），而用户全局规则为“RapidOCR 为唯一 OCR”。RapidOCR 是 Python 组件，本产品运行时禁止 sidecar；该实现为既有代码，不在本规划范围内更换。已记入 ledger，需用户裁定。

## Release 实机（界面）

未做（屏幕控制权限被拒）：附件卡外观、Quick Look/系统打开、中文 IME 查询、笔记内查找与全库搜索区分的界面行为、高命中高亮上限，只有既有自动化测试。

## 未解决

- WebP/GIF 等内联图片不进入 OCR；GPUI 能否解码未验证。
- 提取失败在界面中的逐附件展示与手动重试按钮未做（仅自动有界重试）。

- Claude 实施状态：提交待验收（界面实机未做）
- Codex 验收状态：未验收
