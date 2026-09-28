# Claude 施工报告（2026-09-28 起，供 Codex 独立复验）

依据 `docs/research/claude-next-delivery-handoff-2026-09-28.md`。本报告只记录施工方的实现、测试和自测证据，不代表 Codex 验收通过。没有推送，没有安装或替换正式应用，没有操作原资料库，也没有打扰 Codex 正在使用的隔离 UI 窗口和资料库（`/tmp/joplin-shortcuts-ui.*`）。

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
