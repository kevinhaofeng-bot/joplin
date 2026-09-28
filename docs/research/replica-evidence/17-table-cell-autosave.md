# 单元格自动保存与组合输入边界

2026-09-28，Codex；基于 f749c2378 的未提交增量。不是整体验收通过。

## 实现与根因

独立单元格缓冲现在观察已确认的内容变化，将其送入主文档既有保存协调器，不另建保存系统。沿用100ms日志、500ms正文快照；Escape恢复打开时的单元格内容并走同一保存链。

同步此前仅查看主NoteSession是否Clean，忽略子编辑器的组合输入。新增待确认输入/单元格错误检查。完成和Tab此前另外导出子文档，会提交候选字并关闭；现复用persist_table_cell_draft，组合输入保持原单元格，不污染主文档，未改变内容时不重复提交。

## 验证证据

- `/tmp/joplin-cell-auto-red.log`：旧路径不产生自动恢复日志。
- `/tmp/joplin-cell-auto-green.log`：100ms有日志、正文仍旧；500ms正文更新；Escape后恢复打开前内容。
- `/tmp/joplin-cell-crash-recovery.log`：销毁窗口和原NoteSession、确认弱引用失效、重开数据库和新会话，从仅有日志恢复输入。不是物理杀进程或断电证明。
- `/tmp/joplin-cell-ime-sync-red.log` → `/tmp/joplin-cell-ime-sync-green.log`：同步待保存状态漏判的失败/通过。
- `/tmp/joplin-cell-ime-done-red.log`：完成按钮错误关闭组合输入单元格。
- `/tmp/joplin-cell-autosave-all.log`：8项挂载测试通过，包含完成、Tab、组合输入、自动保存、恢复、撤销、结构操作。不是真实系统输入法实机证据。
- 完整 `cargo test` 退出0：1438主测试、17集成测试通过，合计4项忽略；日志 `/tmp/joplin-cell-autosave-full.log`。既有编译告警仍在；`git diff --check`通过。

## 产品边界

新Release隔离候选已构建且整包ad-hoc签名校验通过（非公证）：`/tmp/joplin-codex-table-candidate/20260928T065129Z-f749c2378/Joplin Lite.app`。构建基线f749c2378加未提交增量，二进制SHA256 `985188147341f34cda9dbfac5726873ae148e10d7f1d8b515215793836e6c2d9`；日志`/tmp/joplin-cell-autosave-package.log`，退出0。未安装或覆盖正式App，新包尚未进行实机矩阵。

后续启动核查：新候选通过 `JOPLIN_LITE_PROFILE=/tmp/joplin-cell-live.tX1U0L/profile` 启动，PID34027，SQLite notes数为0（全新隔离库）。运行约62秒时RSS57280KiB、CPU0%，仅为空库静置样本，不是典型负载预算证明。独立重验签名成功、二进制哈希与上述一致。CUA选择新包时再次Transport closed，js_reset同样失败；旧窗口成功截图不能替代本包操作验证。测试进程保留，下一轮沿用，不因控制失败重复启动。

仍以证据16所述Evernote同一文档树为对照；本补丁修复独立缓冲的安全性，不意味着弹窗编辑已经达到Evernote内联编辑体验。合并单元格、块内容保真、最新包实机、NAS及完整产品验收仍未关闭。屏幕控制本轮已能重新读取隔离测试App；尚不能以此宣称新构建实机通过。
