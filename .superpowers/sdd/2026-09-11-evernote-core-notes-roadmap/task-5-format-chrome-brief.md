# Task 5 格式 Chrome：默认资料库路由共享接入

状态：实现和定向自动验证完成；未提交、未推送。此检查点只把已验证的共享格式 Chrome 接到默认 `LibraryShell`，不改动 Task 5 的资源暂存、hydration 或保存核心。Task 5 Step 5 的 Release 手工验收仍未完成，**不得勾选**。

## 范围与不变量

- 默认资料库和 `--evernote-spike` 必须使用同一个 `EditorCommandChrome` 实体类型、同一 `CommandCatalogue`、同一 Link/More 逻辑；不得新增 library-only 按钮或 handler。
- Chrome 必须持有当前 `NoteSession` 已有的 `EditorCore`，在切笔记、无选择或 codec failure 时随 session/surface 一起释放。
- 格式命令仍进入 `EditorCore` 的正常 history；`NoteSession` 观察 mutation 并经已有 ManualSync/codec 保存。
- Chrome 的 InsertImage 只能发 typed request；`LibraryShell` 先捕获 saved `InsertIntent`，再由平台 picker completion 进入既有 staged durable resource path。Spike 保留自己的实验 picker adapter。
- Link 在打开时保留 selection，Apply/Cancel 回焦 editor；More/Link overlay 不能把鼠标事件穿透到 library `EditorSurface` 的 capture handler。

## TDD 接受测试

- [x] `mounted_default_route_mounts_the_shared_editor_command_chrome`：默认路由真实 mount，在 title 与 native body 之间的 shared Chrome 删除即 RED。
- [x] `mounted_library_shared_chrome_clicks_bold_and_list_with_one_history_entry`：真点击 Bold/列表，保持 selection，分别一条 history、一次 Undo。
- [x] `mounted_library_chrome_format_manual_sync_switch_and_reopen_round_trips_canonical_html`：Bold → ManualSync → 切换 → 重开同一 note 后 canonical `<strong>` 和 decoder mark 都存在。
- [x] `mounted_narrow_library_chrome_moves_list_command_into_shared_more_and_executes_it`：窄宽、折叠导航后的同一个 More row 执行列表；overlay 不穿透正文。
- [x] `mounted_library_shared_link_popover_keeps_selection_and_returns_editor_focus` 与 `mounted_library_note_switch_discards_the_previous_shared_link_and_more_overlays`：Link focus/selection 与 session switch teardown。
- [x] `mounted_library_chrome_insert_image_event_uses_the_saved_selection_durable_picker_route`：typed event → saved selection → 已有 picker completion/staged durable commit。

## 不在本检查点

- 不改变 resource hydration、writer ownership、journal、attachment handoff 或 Task 5 的数据库事务。
- 不实现新的格式命令语义；共享 `CommandCatalogue` 中未支持/不适用命令仍按现有 descriptor state 显式 disabled。
- 不执行或声称完成 fresh-profile Release 手工 M1 验收。
