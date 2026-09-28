# 隔离窗口观察通道恢复

2026-09-28。CUA仍Transport closed；改用macOS原生只读查询，未更改辅助功能授权或系统保护。

测试包：`/tmp/joplin-codex-table-candidate/20260928T065129Z-f749c2378/Joplin Lite.app`，资料库仅合成5篇笔记：`/tmp/joplin-nas-acceptance.utBFH6/mac-lan-fresh`。本轮PID58768、主窗口26027（后续操作前必须重新确认编号，不能沿用旧PID）。

- System Events按unix id查询窗口成功，返回1160×789窗口。
- CoreGraphics仅筛选该PID的窗口列表，取得主窗口编号。
- screencapture -x -l 26027成功，截图`/tmp/joplin-native-ui-window.png`显示三栏笔记列表和五篇合成笔记，没有截取其他应用。
- AX直接窗口点击未打开笔记；CGEvent postToPid点击后截图`/tmp/joplin-native-ui-click.png`仅出现列表hover，正文仍提示选择笔记。不能当作笔记打开、焦点输入或编辑通过。
- 后续ps确认PID已不存在，exec会话72056退出0；日志只有restorable-state系统警告，无崩溃堆栈。退出原因不明，不能归为已确认应用缺陷或用户关闭。

结论：恢复了单窗口观察能力，输入通道与完整实机闭环尚未验证。当前旧包不含同步设置施工，不用于新功能验收。
