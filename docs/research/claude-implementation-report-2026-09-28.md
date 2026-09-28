# Claude 施工报告（2026-09-28 起，供 Codex 独立复验）

依据 `docs/research/claude-next-delivery-handoff-2026-09-28.md`。本报告只记录施工方的实现、测试和自测证据，不代表 Codex 验收通过。没有推送，没有安装或替换正式应用，没有操作原资料库，也没有打扰 Codex 正在使用的隔离 UI 窗口和资料库（`/tmp/joplin-shortcuts-ui.*`）。

## 来源说明（适用于本报告各批）

每批都注明行为依据。“Evernote 依据”一栏只列出实际读过的解包文件和符号；没有读过源码的，一律标为本项目自有的缺陷修复，或自托管产品自有的功能。本地测试只证明实现符合这里写下的行为规则，不证明这些规则与 Evernote 一致。

| 批次 | 行为依据 | Evernote 依据 |
| --- | --- | --- |
| 第一批：文档首尾跳转 | 本项目自有缺陷修复。规则是 macOS 文本编辑的通用约定：Cmd-Up/Down（以及快捷键目录里的 Ctrl-Home/End）把插入点移到文档首尾，并让它可见 | 未读取 Evernote 源码；不声称与 Evernote 一致 |
| 第二批（一）：导入/恢复状态条布局 | 本项目自有 UI 缺陷修复。整库备份与恢复是自托管产品自有的功能 | 无；Evernote 没有对应的界面 |

## 第一批：正文文档首尾跳转（`aae2684c1`）

接手 Sol 未提交的补丁（`components/actions.rs`、`native_editor/surface.rs`、`ui/tests.rs`），审查后做了以下调整再提交：

| 审查点 | 处理 |
| --- | --- |
| 新 handler 同时挂在内嵌的测量 spike 编辑面上 | spike 的外层自己绑定动作，但不处理 JumpToTop/Bottom，内嵌编辑面也不负责滚动。新的 handler 现在只挂在资料库的正式编辑面上（`embedded_frame.is_none()`），spike 保持原行为 |
| 跳到底部只按 `max_offset` 设偏移 | 屏外的块用的是估算高度，滚进视口时才被测量；编辑面的画布高度要到下一帧才更新。现在由编辑面在“排版后、绘制前”的钩子里跟随光标，最多 3 帧，直到光标完整进入视口（`pending_caret_reveal`，与已有的 `pending_find_reveal` 同一机制） |
| 滚到底部时最后一行被压住 1px | 编辑面有 1px 边框，但容器高度没有把它算进去。现在非内嵌编辑面的高度加上上下两条边框 |
| 格式 | 没有全文件 rustfmt。`surface.rs` 只整理了本次改动的行，另有 2 处原有的格式差异保持不动；`tests.rs` 在 HEAD 时格式干净，所以单独对它格式化，改动只落在新增的测试上 |

测试（均为真实按键派发，使用临时资料库）：

- `document_jump_shortcuts_move_body_caret_before_editing`（Sol）：Cmd-Down/Up、Ctrl-End/Home 把光标移到文档首尾，导航不产生撤销记录，Return 在末尾插入。
- `document_jumps_do_not_steal_title_search_or_settings_focus`（Sol）：焦点在标题、搜索面板、同步设置时，正文选区不变。
- `document_jumps_reveal_the_caret_in_a_long_note_and_save_nothing`（新增）：80 段的长笔记，Cmd-Down 后光标位于正文滚动视口之内（用编辑面的真实 `ScrollHandle::bounds()` 判断），Cmd-Up 同样；同步保存后正文和修订号都不变。

| 命令 | 结果 | 日志 |
| --- | --- | --- |
| 对照 1：跳转后不跟随光标 | 长笔记测试失败 | `/tmp/joplin-claude-batch1-red.log` |
| 对照 2：编辑面高度不含边框 | 长笔记测试失败（差 1px） | 同上 |
| `cargo test -q --bin velotype`（app-lite-gpui，提交前的工作树） | 退出 0；1451 通过、0 失败、2 忽略 | `/tmp/joplin-claude-batch1-full.log` |

未验：实机键盘（包括外接键盘上的 Home/End 映射）由 Codex 复核。

## 第二批（一）：导入/恢复状态条（`acc64fbe8`）

问题来自 Codex 的实机核验 `replica-evidence/26-native-backup-restore-review.md`：窗口为 1160×789 的三栏布局时，恢复成功提示越过窗口右边缘，“打开导入的资料库”按钮不可见。

原因有两层，自测都确认过：

1. 状态条是一个只锚定右侧的绝对定位横向 flex。长消息是不能收缩的文字子项，把按钮挤到了窗口之外（RED：按钮画在 x=1908，窗口宽 1160）。
2. 状态条下面的正文编辑面会截走点击。即使按钮回到可见位置，鼠标按下也到不了按钮（对照：去掉遮挡后点击无效，活动资料库不变；这与 Codex 独立运行的失败现象相同）。

修复：外层改为左右都锚定的整宽定位行，内容靠右，最宽 560px；消息放在自己的可收缩容器里换行，按钮单独一行、不收缩；状态框声明 `occlude`。

测试 `ui::library_import_tests::mounted_restore_status_keeps_its_open_button_on_screen_and_clickable`：
- 走真实的备份与恢复流程；备份目录用一个很长的中文名，让提示足够长。
- 在 1160×789 的三栏、两栏、一栏，以及 760×600 的三栏下，检查状态条和按钮都画在窗口之内。
- 用鼠标按下按钮（不是程序派发 `OpenImportedLibrary`），活动资料库切换为恢复出来的副本，旧窗口被替换。

| 命令 | 结果 | 日志 |
| --- | --- | --- |
| RED（修复前） | 按钮在窗口外 | `/tmp/joplin-claude-restore-status-red.log` |
| 对照：去掉 `occlude` | 点击无效，活动资料库未切换 | 同上 |
| `cargo test -q --bin velotype`（提交前） | 退出 0；1452 通过、0 失败、2 忽略 | `/tmp/joplin-claude-batch2a-full.log` |

格式：`mod.rs` 和测试文件在 HEAD 时是格式干净的，所以对这两个文件单独运行了 rustfmt。rustfmt 会顺着 `mod` 声明连带格式化其他子模块，这些无关改动已恢复为 HEAD 版本；`mod.rs` 中一处原有的未格式化代码也恢复了原样。提交只包含状态条和新增的测试。

未验：真实签名应用里点击“打开导入的资料库”、在恢复库里编辑、保存并重新打开，由 Codex 实机复核。
