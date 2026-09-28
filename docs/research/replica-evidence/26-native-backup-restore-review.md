# 实机备份恢复独立核验（2026-09-28）

## 基线与边界

- 签名隔离候选源码 `da3612738`；不是当前施工中的导航补丁。
- 测试资料库 `/tmp/joplin-shortcuts-ui.Y7gmgX`，只含合成测试数据；未操作正式资料库。
- 通过原生菜单“备份整个资料库…”及系统保存对话框生成备份，再通过“恢复到新资料库…”及系统打开对话框恢复。没有用脚本调用产品内部备份函数替代 UI。

## 数据结果：通过本样本核验

- 备份目录 `/tmp/joplin-shortcuts-ui.Y7gmgX/backup-native-20260928a`。
- 恢复目录 `/tmp/imported-libraries/backup-native-20260928a-1790584694`。
- 使用 sqlite3 JSON 查询并对完整行排序比较：notes 7、notebooks 1、resources 6、note_resources 6、note_revisions 21，恢复前后完整行相同。
- stacks、tags、note_tags 均为 0，只证明空集合保持，不证明带标签或堆栈恢复。
- 5 个实体附件逐个 SHA-256 比较，缺失/不一致 0。
- 截图 `backup-after-activate.png`、`restore-status.png` 位于上述隔离资料库根目录。

## UI 阻断：尚未通过完整恢复流程

在 1160×789 点窗口、三栏视图中，恢复成功提示被裁切到右侧窗口之外，“打开导入的资料库”按钮不可见。恢复成功不等于用户能顺畅打开恢复库。

代码定位：`packages/app-lite-gpui/src/ui/mod.rs` 的 `library-import-status`。横向 flex 同时包含长消息和操作按钮，虽然 max_w(560)，但消息没有独立收缩/换行容器，操作也没有显式保持可见。截图能够证实裁切；最终根因及修复由 Claude Code 确认。

验收要求：长中文恢复/导入成功消息下，打开与取消等操作不能被挤出边界；在窄编辑区及一/二/三栏可操作。实际点击打开恢复库、再次编辑保存和重开后再补充通过证据。不得仅通过程序派发 OpenImportedLibrary 绕过按钮可用性验收。

结论：本样本备份/恢复的数据完整性已核验；恢复后的打开和继续使用尚未通过，整款产品不放行。

## 修复后独立自动化复验

Claude 提交 `acc64fbe8`：状态消息换行、操作单独一行，并通过 occlude 避免下层正文抢走按钮点击。主代理在其施工中间态曾独立复现“可见但点击后未切换”；提交后的同名测试独立重跑通过。

命令 `cargo test --bin velotype mounted_restore_status_keeps_its_open_button_on_screen_and_clickable -- --nocapture`，退出 0，1 通过、0 失败。日志 `/tmp/joplin-restore-ui-committed-independent.log`。覆盖三/二/一栏、窄窗边界和点击打开后活动库改变，但属于挂载测试，不是签名包实机。新候选构建中，实机门仍未关闭。
