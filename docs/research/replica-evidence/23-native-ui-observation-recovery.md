# 隔离窗口观察通道恢复

2026-09-28。CUA仍Transport closed；改用macOS原生只读查询，未更改辅助功能授权或系统保护。

测试包：`/tmp/joplin-codex-table-candidate/20260928T065129Z-f749c2378/Joplin Lite.app`，资料库仅合成5篇笔记：`/tmp/joplin-nas-acceptance.utBFH6/mac-lan-fresh`。本轮PID58768、主窗口26027（后续操作前必须重新确认编号，不能沿用旧PID）。

- System Events按unix id查询窗口成功，返回1160×789窗口。
- CoreGraphics仅筛选该PID的窗口列表，取得主窗口编号。
- screencapture -x -l 26027成功，截图`/tmp/joplin-native-ui-window.png`显示三栏笔记列表和五篇合成笔记，没有截取其他应用。
- AX直接窗口点击未打开笔记；CGEvent postToPid点击后截图`/tmp/joplin-native-ui-click.png`仅出现列表hover，正文仍提示选择笔记。不能当作笔记打开、焦点输入或编辑通过。
- 后续ps确认PID已不存在，exec会话72056退出0；日志只有restorable-state系统警告，无崩溃堆栈。退出原因不明，不能归为已确认应用缺陷或用户关闭。

结论：恢复了单窗口观察能力，输入通道与完整实机闭环尚未验证。当前旧包不含同步设置施工，不用于新功能验收。

## 后续：鼠标和键盘路径已验证

改用`open -n --env JOPLIN_LITE_PROFILE=...`标准启动同一隔离包。每次操作在同一exec中用System Events设置frontmost并AXRaise，随后验证前台PID再发送CGEvent到cghidEventTap；不能依赖跨工具调用保留焦点。AX直接click仅返回window，不能证明真实点击。

PID60327/窗口26074成功打开合成文件笔记（附件卡片实际出现），再点击新建，列表出现无标题笔记。截图`/tmp/joplin-native-ui-hid-raised.png`和`/tmp/joplin-native-ui-new.png`。该实例后来不在进程列表，原因未明。

PID60861/窗口26096继续同一合成库，再次新建。初次键盘尝试因前台守卫未满足而未输入；重新在同一AppleScript内AXRaise、检查frontmost后输入`Acceptance title`、Return、`First paragraph`、Return、`Second paragraph`、Cmd+S。只读SQLite确认标题精确为Acceptance title，body_text为上述两段换行文本。该结果证明此旧签名包的真实新建、ASCII标题/正文输入及保存路径；不证明中文IME、样式、图文混排或新版本设置通过。

## 实际样式操作

同一进程测试笔记：Cmd+A选中两段正文后，Cmd+B未使body_html发生变化（仍两个p），源码rg未发现该快捷键绑定，列为待修/新包复核项。点击真实粗体B按钮则界面文字变粗、B绿色选中，自动保存body_html为两个p内含strong。截图`/tmp/joplin-native-ui-toolbar-bold.png`。

随后点击编辑器“更多”→“项目符号列表”，界面出现两个圆点列表项，body_html为`<ul><li><strong>First paragraph</strong></li><li><strong>Second paragraph</strong></li></ul>`。截图`/tmp/joplin-native-ui-format-menu.png`、`/tmp/joplin-native-ui-ul.png`。验证的是旧签名候选的真实鼠标路径和持久化，编号列表、撤销重做、图片混排与IME尚未包含。
