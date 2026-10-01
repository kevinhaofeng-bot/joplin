# Claude 施工报告（2026-09-28 起，供 Codex 独立复验）

依据 `docs/research/claude-next-delivery-handoff-2026-09-28.md`。本报告只记录施工方的实现、测试和自测证据，不代表 Codex 验收通过。没有推送，没有安装或替换正式应用，没有操作原资料库，也没有打扰 Codex 正在使用的隔离 UI 窗口和资料库（`/tmp/joplin-shortcuts-ui.*`）。

## 来源说明（适用于本报告各批）

每批都注明行为依据。“Evernote 依据”一栏只列出实际读过的解包文件和符号；没有读过源码的，一律标为本项目自有的缺陷修复，或自托管产品自有的功能。本地测试只证明实现符合这里写下的行为规则，不证明这些规则与 Evernote 一致。

| 批次 | 行为依据 | Evernote 依据 |
| --- | --- | --- |
| 第一批：文档首尾跳转 | 本项目自有缺陷修复。规则是 macOS 文本编辑的通用约定：Cmd-Up/Down（以及快捷键目录里的 Ctrl-Home/End）把插入点移到文档首尾，并让它可见 | 未读取 Evernote 源码；不声称与 Evernote 一致 |
| 第二批（一）：导入/恢复状态条布局 | 本项目自有 UI 缺陷修复。整库备份与恢复是自托管产品自有的功能 | 无；Evernote 没有对应的界面 |
| 第二批（二）：取消输入法组合后的撤销记录 | 本项目自有缺陷修复。规则是 macOS NSTextInputClient 的约定：取消组合（Escape 发出空的 setMarkedText）不改动文档，因此也不应留下撤销步骤 | 未读取 Evernote 源码 |
| 输入跟踪 | 本项目自有的诊断工具，只供测试资料使用 | 无 |
| 第三批：混合资源的列表与格式 | 按 Evernote 源码实现，见第三批的对照表 | `common-editor-sourcemap/@evernote/common-editor/src/apps/peso/modules/list/list.ts` 的 `insertOrToggleList`（62–101 行）与 `getListTypeAtRange`；段落/标题和行内格式依据的是 ProseMirror `setBlockType`/`addMark` 的通用语义，本批没有另读 Evernote 的格式命令文件 |
| 第三批补充：已分组资源单独切换列表类型 | 按 Evernote 源码实现 | 同上 `list.ts` `insertOrToggleList`：对已有 listItems 用 `setNodeMarkup` 改类型（Codex evidence32 复核同一文件） |
| 第四批：表格单元格内 Cmd+V | 本项目自有缺陷修复（粘贴目标跟随焦点）。聚焦上下文的依据由 Codex 在 evidence38 读取：`clipboard/commands/paste.ts` 98–191 行；我本人未另读该文件 | 单元格拒绝图片/文件**没有** Evernote 依据，是表格保真缺口，不是等价实现 |
| 第五批：批量移除/清空标签与数量文案 | 按 Evernote 行为实现 | 本人实际读取的只有本地化字符串：`main-readable/src/modules/64482__localization-catalog.js` 的 `Boron.notesMultiSelectionContext.title`（"已选择 {N} 条笔记"）、`...editTags`，以及 886/905 行 `AiCopilot.organizationPlan.op.addTagToNotes/removeTagFromNotes.title`（"向/从 {N} 笔记…标签"）。执行路径由 Codex 在 evidence38 读取：renderer `9435.js` 13155–13252 行（EDIT_TAGS 作用于整个选区） |
| 第六批：OCR 期间的图形内存 | 本项目自有性能缺陷修复 | 无 |
| 测试隔离：索引/OCR 测试闸门 | 本项目自有测试基础设施修复 | 无 |
| 第七批：删除笔记本 | 按 Evernote 用户契约实现 | `main-readable/src/modules/64482__localization-catalog.js` 4224 行与 `32150__localization-catalog.js` 4219 行 `ModalManager.deleteNotebook.confirmation`（“笔记本中的任何笔记都将被移动到回收站”）；Codex evidence41 另读 `63570__module-63570.js` 266–279 行 `expungeNotebook`。执行端未在解包文件中找到，无法继续追踪 |
| 第八批：列表视图与排序持久化 | 按 Evernote 行为实现 | `main-readable/src/modules/51244__get-string-user-setting.js` 153/183/297/309 行（GLOBAL/NOTEBOOKS/STACKS/TRASH_NOTE_VIEW_OPTIONS_KEY）、234/237 行（SEARCH_SORT_PREFERENCE_GLOBAL/_NOTEBOOK）；renderer `9435.js` 31827–31851（四个持久化存储）、33039–33048（先写全局再写当前上下文）、62691–62739（有上下文设置时用它，否则用全局） |
| 第九批：可读恢复的中文错误 | 本项目自有 UI 修复（可读导出是自托管功能） | 无 |
| 启动内存峰值 | 调查，未改代码 | 无 |
| 第十批：上下标数据通路 | 按 Evernote 源码实现 | `common-editor/.../textformatter/schema.ts` 346–349（解析 `sub`/`sup` 标签及 `vertical-align`）、614–633（两者 `excludes` 互斥，输出 `<sub>`/`<sup>`）；`textformatter/keymap.ts` 22–23（Ctrl-Cmd-= / Ctrl-Cmd--）；`64482__localization-catalog.js` 的 `FormattingBar.superscript`/`subscript` |
| 第十一批：光标处的输入样式（evidence54） | 按 Evernote 源码实现 | `utils/mark.ts` 220–247（`toggleMark`：选区中任一处有该格式即全部移除，否则全部添加；光标处改待输入格式）；`textformatter/commands/boolformat.ts`；`link/schema.ts:62` 与 `textformatter/schema.ts:380`（链接、代码 `inclusive: false`）；`paragraph/keymap.ts` 102–130 与 `list/keymap.ts` 428（回车带入样式）。光标格式的解析规则按 ProseMirror `ResolvedPos.marks()`，这是 Evernote 编辑器所用的库 |

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

## 第二批（二）：正文输入法（`eea1c0f99`）

交接中的现象（正文出现“8泥豪7”“nihaou”）在自动化中没有确定性复现。已覆盖的路径：

- `ui::tests::body_pinyin_composition_survives_autosave_and_commits_once`：挂载的资料库窗口中，正文组合输入期间触发自动保存，组合保持不变；提交后只写入一次“你好”，撤销一步即可恢复。
- `native_editor::tests::a_cancelled_composition_leaves_undo_as_it_found_it`：找到并修复了一处真实缺陷：取消组合后会留下一个空的撤销步骤。现在 `replace_and_mark_utf16` 遇到“组合被清空且原位置为空”时，会丢弃这一步（`History::discard_last_noop`）。

| 命令 | 结果 | 日志 |
| --- | --- | --- |
| 对照：去掉丢弃空步骤的处理 | 2 个测试失败 | `/tmp/joplin-claude-ime-cancel-red.log` |
| `cargo test -q --bin velotype`（提交前） | 退出 0；1454 通过、0 失败、2 忽略 | `/tmp/joplin-claude-batch2b-full.log` |

根因仍未确定。Codex 实机记录 `replica-evidence/30-real-ime-trace-observation.md`（诊断包、合成资料库）中：
- 标题和正文都按 n → ni → ni h → ni ha → ni hao 收到组合回调；
- 空格后只提交了一次“你好”，数据库中正文为 `<p>你好</p>`；
- 正文第一个 n 同时有 key_down 和组合回调，但没有多出 n。只看到 key_down 不能认定为按键泄漏。

这说明基本拼音路径正常，但不代表所有输入法问题都已修复。图片前后、切换笔记、工具栏、取消组合、长文等组合，仍需在干净的正式候选版上逐项实机验证。

## 输入跟踪（`386849488`，`c428952b4`）

只有启动时设置 `JOPLIN_LITE_INPUT_TRACE=<文件>`，才会记录正文和标题的输入法回调，以及到达正文按键处理的按键，每行一条 JSON；否则不记录。没有任何设置项或正式配置会开启它。因为日志含有输入的文字，新建的日志文件在 Unix 上权限为 0600（测试 `input_trace::tests::a_new_trace_file_is_private_to_its_owner`）。测试环境使用线程内的内存记录，不写文件（`ui::tests::input_trace_records_body_composition_and_keys_reaching_the_app`）。`c428952b4` 同时把 `history.rs` 中 `discard_next_redo` 的文档注释移回它所属的函数上方。

| 命令 | 结果 | 日志 |
| --- | --- | --- |
| `cargo test -q --bin velotype`（`386849488` 提交前） | 退出 0；1455 通过、0 失败、2 忽略 | `/tmp/joplin-claude-trace-full.log` |

在 `386849488` 打的包（`/tmp/joplin-claude-package-386849488.log`）已被 `c428952b4` 取代，不能作为候选。Codex 的诊断包在构建记录中标注为源码 dirty，同样不是交付候选。

## 第三批：混合资源的列表与格式（`98a5a995f`）

Codex 源码对照审计指出：Evernote 的 `insertOrToggleList` 会把选中的段落和资源一起转成列表，而我们的命令只要选区里有图片或附件，就会禁用列表、标题和行内格式。此前 evidence01 把这点记为“独立设计”，但用户没有接受它作为永久的产品限制，因此本批按源码修正。

### 来源到行为的对照

| Evernote / ProseMirror 行为 | 本项目行为 | 测试 |
| --- | --- | --- |
| `insertOrToggleList`：选区内的文字块用 `applyIndent(..., true)` 转成列表项，资源用 `applyIndent(tr, pos, listType, checked, false)` 包进各自的列表项 | 列表命令把每个独立的图片或附件变成一个列表项。它的形态与保存后的 `<li><img></li>` 重新打开时一致：一个空的列表行，和资源编为一组。与文字转换属于同一步撤销 | `a_list_takes_in_selected_resources_saves_reopens_undoes_and_toggles_back`（无序、有序；有序列表的资源项参与编号 1–4） |
| `getListTypeAtRange` 判断全选是否已是同类列表；是则移除列表 | 已在列表中的资源按其所在项的列表类型参与判断；全部相同时移除列表。只含资源的列表项会拆回独立资源，保存结果与原文一致 | 同上（重新打开后切换，以及撤销、重做、再切换） |
| ProseMirror `setBlockType` 跳过非文本块 | 段落和标题命令只改文字块，独立资源保持不变；选区里只有资源时命令禁用 | `text_styles_over_a_mixed_selection_change_only_its_text`（H1）、`a_resource_alone_takes_a_list_but_no_text_style` |
| ProseMirror `addMark` 只作用于文字 | 粗体、斜体等行内格式和链接只改选区中的文字；选区里只有资源时禁用 | `text_styles_over_a_mixed_selection_change_only_its_text`（粗体）、`a_resource_alone_takes_a_list_but_no_text_style` |
| — | 挂载的资料库窗口中，真实图片资源、列表命令、自动保存、撤销、重做后的数据库正文都正确 | `ui::tests::mounted_list_command_over_text_and_an_image_saves_and_undoes_durably` |

| 命令 | 结果 | 日志 |
| --- | --- | --- |
| 对照：命令层换回 HEAD 实现（保留新测试） | 3 个新命令测试失败 | `/tmp/joplin-claude-mixed-resource-red.log` |
| 对照：同上，运行挂载测试 | 失败：旧实现在图片处把列表拆成两段，保存为 `<ol><li>甲</li></ol><img …><ol><li>乙</li></ol>` | `/tmp/joplin-claude-mixed-resource-mounted-red.log` |
| `cargo test --bin velotype`（提交前） | 退出 0；1460 通过、0 失败、2 忽略 | `/tmp/joplin-claude-mixed-resource-full.log` |

### 仍与 Evernote 不同或未验证的地方（不标记为完成）

- 对齐命令（左、中、右）在选区含资源时仍然禁用。本批没有核对 Evernote 的对齐源码。
- 表格不属于 `insertOrToggleList` 处理的资源，列表命令会跳过表格。这是否与 Evernote 一致尚未核对。
- 文字加资源的列表项（例如 `<li>文字<img></li>`）移除列表后，会成为含图的段落组。保存结果正确，但重新打开时会拆成独立的文字块和图片块。
- 缩进和反缩进仍要求选区内每个块都是列表项。选区包含资源块时（即使资源已在列表项中），命令状态为禁用，因为资源块本身的类型不是列表。只选中该项的空列表行时可以缩进，但没有专门测试。这是尚存的差异。
- 带资源列表项的实机渲染（列表标记、编号、图片位置）、工具栏点击，以及复选框列表中资源项的勾选，都没有做视觉验证，留给 Codex 实机复核。

### 第三批补充：只选中列表项里的资源时切换列表类型（`2b415da9c`）

问题来自 Codex evidence32：在已有的无序列表中只选中某一项的图片或附件，再点有序列表或清单，命令显示为可用，实际却不起作用。原因是列表项的类型记录在该项的文字行上，而选区只覆盖了资源块。修复后，命令通过该项的文字行改变类型，与 `insertOrToggleList` 用 `setNodeMarkup` 改已有列表项的行为一致。

- 测试 `native_editor::commands::tests::a_selected_list_resource_alone_changes_its_own_item_type`：对图片和附件各跑一遍 UL → OL → 清单。检查保存结果（被选中的那一项单独成为新类型的列表，其他项不变）、保存后重新打开结果不变、每次命令只产生一个撤销步骤，两次撤销后回到原文。
- RED（修复前）：撤销步数为 0，即空操作。日志 `/tmp/joplin-claude-grouped-list-red.log`。
- 全量 `cargo test --bin velotype`：退出 0，1465 通过、0 失败、2 忽略。日志 `/tmp/joplin-claude-grouped-list-full.log`。

## 第四批：表格单元格内 Cmd+V（`5e170c03e`）

问题来自 Codex evidence33：单元格编辑器已打开并被点击聚焦时，按 Cmd+V，文字被写进了表格前的正文段落。原因是 `ui/mod.rs::paste_resource_or_text` 总是写入主笔记会话。修复后，只要单元格编辑器持有焦点，粘贴就进入单元格：

- 纯文字按文字粘贴；
- 本应用复制的片段和外部 HTML，按单元格支持的行内格式（粗体等）粘贴；
- 图片和文件被拒绝，并显示提示“单元格只能粘贴文字和链接；图片或文件请粘贴到正文”，不会进入正文；由粘贴产生的临时文件会被删除。

测试 `ui::table_cell_editor_tests::cmd_v_in_a_clicked_cell_pastes_into_that_cell_not_the_body` 走真实路径：双击单元格，模拟 `cmd-v` 按键，依次粘贴文字、带粗体的片段和图片，再按 Tab 前进，最后保存到数据库。它检查：单元格内容为 `一粘贴丙<strong>粗</strong>`，表格前的 `<p>前</p>` 没有改动，没有图片被加入，也没有资源进入插入队列。

| 命令 | 结果 | 日志 |
| --- | --- | --- |
| 对照：去掉路由判断 | 失败：图片被当作正文资源排队 | `/tmp/joplin-claude-cell-paste-red.log` |
| 全量（提交前） | 退出 0；1461 通过、0 失败、2 忽略 | `/tmp/joplin-claude-cell-paste-full.log` |

Codex 已在签名候选包 0fbec38e3 上实机验证原缺陷已修复（evidence38）：点击第 2 行第 2 列，通过真实剪贴板粘贴中文，Tab 后保存到正确的 `td`。

仍然存在的差距：单元格不能粘贴图片或文件，而 Evernote 表格单元格可以包含块级内容和资源。这是表格保真缺口，不能据此关闭表格或多媒体验收门。

## 第五批：批量移除、清空标签与数量文案（`0fbec38e3`）

问题来自 Codex evidence35：`MoveSelectedNote` 和 `AddTagToSelectedNote` 作用于整个选区，而 `SetSelectedNoteTags`（清空标签）和 `RemoveTagFromSelectedNote` 只处理当前一篇；多选时面板仍显示“移动当前笔记”。

修复内容：
- 仓储层新增三个方法：`remove_tag_from_notes`、`set_tags_for_notes`（都在一个事务中完成，某篇失败则整批回滚）和 `tag_counts_for_notes`。
- 两个动作改为作用于 `selected_note_ids()`。
- 多选时文案写明作用范围：“移动 N 篇笔记”“清空 N 篇笔记的标签”“为 N 篇添加 #T”“从 N 篇移除 #T”。只有部分笔记带有的标签，同时提供两个按钮：“为其余 K 篇添加”和“从 M 篇移除”。

| 测试 | 覆盖 |
| --- | --- |
| `organization_stage_c::batch_tag_removal_and_clearing_cover_every_selected_note_or_none` | 交集/并集计数；部分笔记带标签；某篇已在废纸篓时整批回滚；未选中的笔记不受影响；清空后标签过滤不再列出这些笔记 |
| `ui::tests::mounted_tag_buttons_act_on_every_selected_note` | 挂载面板，用鼠标依次点击：部分移除、全部移除、全部添加、清空。每次操作后多选保持为 2 篇，未选中的笔记保留标签 |
| `ui::tests::organization_tag_labels_name_the_selection_they_change` | 单选与多选下的全部文案 |

| 命令 | 结果 | 日志 |
| --- | --- | --- |
| 对照：两个动作只作用于选区第一篇 | 挂载测试失败 | `/tmp/joplin-claude-batch-tags-red.log` |
| gpui 全量 | 退出 0；1463 通过 | `/tmp/joplin-claude-batch-tags-full.log` |
| core 全量 | 退出 0；348 通过 | `/tmp/joplin-claude-batch-tags-core.log` |

Codex 已在签名候选包上实机验证批量添加、清空、移除，重启后数据库中的结果保留（evidence38）。

尚未验证的性能风险：多选时 `selected_note_tag_counts` 在渲染组织面板的过程中同步执行 SQL，每篇笔记一次查询。大量选中时的耗时还没有测量，不能据此称为流畅。

## 第六批：OCR 积压期间的图形内存（`e049fdd60`）

问题来自 Codex evidence36：真实 1666 篇的库副本中，物理占用约 630 MB，其中 IOAccelerator（图形）约 526 MB。

### 测量方法

以下全部使用合成资料库，没有打开任何由用户笔记派生的副本。

- 生成器：`/tmp/joplin-claude-memory-evidence/memfixture-main.rs`，命令为 `memfixture <profile> <笔记数> <每几篇配一张图> <宽> <高>`，另可用环境变量 `TOP_IMAGES=K` 让最新的 K 篇带图。
- 被测二进制：
  - 修复前：0fbec38e3 的 release 构建，sha256 `74cd4d7ed93a8321afd506dfd6d34b71a04a4fa89feca035adbbdfb8ac49a27c`；
  - 修复后：去掉每任务重绘后的 release 构建，sha256 `82bfa684027c72f6c9d67ca4d0ce37e4ffff019d6a00612e1c97b64ce08dfbc0`。
- 两份二进制和原始 footprint/vmmap 输出都保存在 `/tmp/joplin-claude-memory-evidence/`。
- 窗口大小为默认的 1160×789。

### 观察到的事实

1. 空库、1666 篇纯文字库、带图库都有 16 个 32 MiB 的 IOAccelerator（graphics）区域。区别只在于它们是否算作 dirty：空库和纯文字库中，dirty 为 7.6 MB，另有 514 MB 计为可回收。
2. 同一带图夹具（1666 篇，每 4 篇一张 2400×1600 的 PNG，共 417 张），做 OCR 前后的受控对照，3 轮结果一致：
   - OCR 待处理时，dirty 为 530 MB，可回收为 0；
   - OCR 全部完成后重新打开，dirty 为 7.6 MB，可回收为 514 MB。
3. 修复前的时间线（`timeline-before-0fbec38e3.txt`）：从 10 s 到 210 s，待处理任务从 407 降到 1，这期间 dirty 一直是 530 MB；队列清空后 10 秒内降到 16 MB，约 370 s 时 514 MB 重新计为可回收。
4. 用注入库记录 Metal 分配（仅用于诊断，不属于产品）：待处理期间与完成之后，主进程显式创建的纹理和缓冲区完全相同，都是两个 2 MiB instance buffer、两张 1024² 图集和窗口 drawable。因此 dirty 的 512 MiB 不是应用显式创建的纹理。注入库本身会改变结果，所以带队列 hook 的那组数据已作废，不作为证据。
5. ImageIO 缩略图（同样的参数）在独立 Swift 进程中不产生任何 IOAccelerator 占用，因此排除了它。
6. 缩放逻辑：派生文本调度器在每个任务完成后都对整个窗口 `notify()`，积压期间约每秒两次重绘；而界面上没有任何内容对应单个任务。
7. 修复后的时间线（`timeline-after-fix.txt`）：同一夹具、同一台机器，从 407 个待处理到 0，dirty 始终为 12 MB，514 MB 一直计为可回收。

### 结论与边界

去掉每任务一次的重绘后，积压期间的 530 MB 图形 dirty 不再出现，这一点已在同一夹具上受控复现。驱动为什么在持续重绘时把这 512 MiB 保持为不可清除，没有做驱动层面的验证；报告和代码注释只陈述测量结果。

仍需 Codex 完成：
- 在 1666 篇真实库的隔离副本上复测；
- 在真实 UI 中、不强制重绘的情况下，证明搜索结果仍能自动刷新（现有测试调用了 `redraw(cx)`，只能证明事件路由，不能证明自发重绘）。

回归测试 `ui::index_scheduler_tests::a_finished_derived_job_does_not_redraw_the_library`：统计挂载后 shell 收到的通知次数，派生任务完成后应为 0。对照中恢复每任务 notify 后，次数为 1，测试失败（`/tmp/joplin-claude-derived-redraw-red.log`）。

### 测试隔离（`38a79bee0`，`3b9496844`）

Codex 的独立运行发现 index_scheduler 测试间歇失败或挂起。我把 e049 之前的代码放回去做基线对照，结果同样失败，并在 `mounted_scheduler_close_finishes_only_active_index_transaction` 挂起。挂起时的堆栈（`/tmp/joplin-claude-baseline-hang-sample.txt`）与 Codex 捕获到的一致，说明这是既有的测试竞争，不是 e049 引入的。

原因：索引 worker 闸门、它的“在独立线程运行”标志，以及 OCR 槽位都是进程全局变量。测试并行运行时，一个测试挂载窗口可能拿走另一个测试的闸门，或者占住另一个测试的 OCR 槽位。

修复：在测试中把这三者改为按线程保存，并在调度器构造时就解析出该线程的 OCR 槽位。正式应用中仍然只有一个进程级 OCR 槽位，没有改动。

修复后结果：
- `ui::index_scheduler_tests` 连续 12 次并行运行，12/12 通过、无挂起；
- 全量运行 4 次，均为 1465 通过、0 失败，最后一次日志为 `/tmp/joplin-claude-lock-capture-full.log`。

另有一个未解释的现象：修复前的某一次全量运行中，`uniform_list_constructs_only_requested_ranges_and_reaches_1662_tail` 失败过一次，失败日志被随后的运行覆盖，之后单独运行 5 次、全量运行多次均通过。原因未查明，这里如实记录。

### 可供独立打包的源码检查点

`3b9496844`。此前的提交依次为 5e170c03e、0fbec38e3、e049fdd60、2b415da9c、38a79bee0。

## 第七批：删除笔记本会把笔记移到废纸篓（`f1751565e`）

问题来自 Codex evidence41：Evernote 删除笔记本时会确认“笔记本中的任何笔记都将被移动到回收站”，而我们把这些笔记移到了默认笔记本。

现在 `delete_notebook` 在同一事务中完成以下几件事：
- 删除笔记本；
- 把其中仍在使用的笔记放进废纸篓，保留附件、历史，也保留它们原来的笔记本 id；
- 按“移到废纸篓”加入同步队列。

另外：
- 已经在废纸篓中的笔记不改动；
- 从废纸篓恢复这些笔记时，由于原笔记本已不存在，笔记会进入默认笔记本（使用已有的恢复逻辑）；
- 默认笔记本仍然不能删除，解散笔记本组的行为不变；
- 确认框文字改为“确认删除当前笔记本？笔记本中的任何笔记都将被移动到废纸篓。”

| 测试 | 覆盖 |
| --- | --- |
| `organization_stage_c::deleting_a_notebook_moves_its_notes_to_trash_whole_and_reversibly` | 带附件与历史的笔记；已在废纸篓的笔记保持原删除时间；其他笔记本不受影响；默认笔记本不可删；用 SQLite 触发器在中途制造失败后整体回滚；重新打开；同步队列中有 trash 操作；恢复后进入默认笔记本，附件和历史都在 |
| `sync_two_clients::a_notebook_deleted_on_one_device_leaves_its_notes_in_trash_on_both` | A 删除笔记本后同步到 B：三篇笔记在 B 上都在废纸篓，附件可读；A 的历史不变。B 恢复其中一篇并同步回 A：该笔记在 A 上进入默认笔记本；笔记本没有复活；两端没有任何未删除的笔记指向不存在的笔记本 |
| `ui::tests::mounted_organization_panel_renames_and_deletes_the_active_notebook_by_id` | 挂载面板中先取消确认：什么都没变（笔记本和笔记都在）。再次删除并确认：笔记进入废纸篓，界面回到“全部笔记”，没有选中项，也没有编辑会话 |

| 命令 | 结果 | 日志 |
| --- | --- | --- |
| 对照：换回旧版 `repository.rs` | 本地两项测试失败 | `/tmp/joplin-claude-notebook-trash-red.log` |
| 对照：同上，运行同步测试 | 失败：B 上的笔记没有进入废纸篓 | `/tmp/joplin-claude-notebook-trash-sync-red.log` |
| gpui / core 全量 | 1465 通过 / 350 通过，均为 0 失败 | `/tmp/joplin-claude-notebook-trash-full.log`，`/tmp/joplin-claude-notebook-trash-core.log` |

仍需验证：签名包中的原生删除确认、废纸篓显示和恢复（Codex 正在隔离 profile 中实机验收）。

## 第八批：列表视图与排序在重启后保留（`d431a9bbf`）

问题来自 evidence43：列表视图和排序只存在内存中，正常退出再打开后恢复为默认。

- **视图：** 保存一个全局值，另外为笔记本、笔记本组、废纸篓各自保存自己的值。切换视图时，同时更新全局值和当前上下文的值；某个上下文没有自己的值时，使用全局值。这对应 Evernote `9435.js` 33039–33048 行的写法。
- **排序：** 按路由保存。其中“全部笔记”相当于 Evernote 的全局排序，“笔记本”相当于按笔记本保存的排序。笔记本组、标签和废纸篓的排序也会保存；Evernote 是否保存这三类的排序尚未核实。
- **存储：** 设置键为 `library-shell.list-view` 和 `library-shell.note-sorts`，写在各自资料库的 settings 表中。
- **写入顺序：** 先把设置写入数据库，成功后才更新界面状态。写入失败时视图和排序都保持原样，并返回错误。
- **兼容：** 读不懂的旧值或损坏的值会被忽略，按默认值打开，不会阻止资料库打开。

| 测试 | 覆盖 |
| --- | --- |
| `list_view_and_sort_survive_reopening_the_library_with_the_selection` | 重启后：全局视图、选中笔记和“全部笔记”的排序都保留；笔记本、笔记本组、废纸篓各自的视图，以及笔记本和废纸篓各自的排序都保留；没有自己设置的笔记本使用全局视图 |
| `a_list_setting_that_cannot_be_saved_changes_nothing` | 用 SQLite 触发器让写入失败：视图、排序和列表顺序都不变；移除触发器后可以正常保存 |
| `old_or_unreadable_list_settings_open_with_defaults` | 设置不存在、为旧格式、JSON 损坏或出现未知值时都按默认值打开 |
| `list_settings_belong_to_their_own_library` | 两个资料库的设置互不影响 |

| 命令 | 结果 | 日志 |
| --- | --- | --- |
| 对照：app 相关文件换回 HEAD（不持久化） | 2 项失败 | `/tmp/joplin-claude-list-prefs-red.log` |
| 对照：先改内存再写入 | 写入失败测试失败 | `/tmp/joplin-claude-list-prefs-order-red.log` |
| 全量 | 1469 通过、0 失败 | `/tmp/joplin-claude-list-prefs-full.log` |

补充：排序变更时，`prepare_navigation_commit` 会先写一次窗格和选中笔记的状态。排序不会改变这两项，所以这次写入与原值相同；即使随后排序写入失败，也不会留下不一致的状态。

Codex 在 d431a9bbf 构建了独立候选包（sha256 `d8378b55c7430fd9eb8cf0d789f1a922ea003be13825c04e6f0975fa7b17d65d`），正在实机验证设置和删除行为。

## 第九批：可读恢复失败时的中文说明（`18b57b1cd`，`8abbebee2` 修正措辞）

问题来自 evidence39：选择一个不是导出包的文件夹恢复时，提示中直接出现英文系统错误（`readable export I/O failed: No such file or directory`）。

现在提示会用中文说明原因，并在末尾以“（详情：…）”附上原始错误，便于排查。核心校验逻辑没有改动。

| 情况 | 提示原因 |
| --- | --- |
| 文件夹里没有 `manifest.json` | 所选文件夹不是可读导出包，请选择用“导出整个资料库为可读 HTML…”生成的文件夹 |
| 有 manifest，但缺少所需的文件或文件夹 | 导出包不完整，可能已被移动或删改 |
| 任一 JSON 数据文件无法解析（manifest 或历史文件） | 导出包中的数据文件无法解析，可能已损坏 |
| 只有格式或版本不符时 | 导出包的格式或版本不受支持 |
| 其他清单校验失败（正文、历史摘要、附件关系不一致等）或附件校验失败 | 导出包未通过完整性校验（正文、历史或附件记录不一致），可能已被修改或损坏 |

以上各种情况的提示结尾都保留“未创建新资料库，当前资料库未改动”。是否存在 `manifest.json` 在后台任务中检查，不占用界面线程。

测试：
- `mounted_readable_restore_of_a_non_bundle_leaves_no_new_library`：在原有断言上增加对中文原因和“详情”的检查；
- `mounted_readable_restore_of_a_damaged_bundle_says_what_is_wrong`：先真实导出一份，再分别写坏 manifest、删除 `notes` 目录，然后执行恢复并检查提示。

| 命令 | 结果 | 日志 |
| --- | --- | --- |
| 对照：换回旧版 `library_backup.rs` | 2 项失败 | `/tmp/joplin-claude-restore-message-red.log` |
| 全量 | 1470 通过 | `/tmp/joplin-claude-restore-message-full.log` |

## 启动内存峰值调查（evidence44，未改代码）

Codex 在已完成索引的 1666 篇隔离副本上测得空闲占用 93.7M，但峰值为 615.4M。原始数据见 `/tmp/joplin-claude-memory-evidence/startup-peak-2026-09-28.txt`。

每 0.5 秒采样一次图形占用，结果如下：

| 被测程序 | 启动时 | 之后 | 峰值 |
| --- | --- | --- | --- |
| 本应用（18b57b1cd release），空库 | 前 3.5 s 为 526 MB dirty | 降为 11 MB；约 10 s 处又短暂回到 530 MB | 613.3M |
| 本应用，417 张图、OCR 已完成的库 | 前 2 s 为 526 MB | 降为 7.6 MB | 612.5M |
| GPUI 0.2.2 自带的 `examples/hello_world.rs`，未改动，release 构建，完全不含本应用代码 | 前 2–3 s 为 524 MB dirty（两轮一致） | 降为约 10 MB，514 MB 计为可回收 | 576.4M / 573.3M |

hello_world 进程中同样有 16 个 32 MiB 的 IOAccelerator（graphics）区域。GPUI 的 `metal_renderer.rs` 没有显式分配 32 MiB：它只创建一个命令队列（252 行），`maximum_drawable_count` 为 3（146 行），instance buffer 为 2 MiB，图集纹理至少 1024²。

结论与边界：
- 约 575 MB 的启动峰值在 GPUI/Metal 的最小程序中就会出现，本应用在此之上只多出约 40 MB。
- 这块内存在连续绘制时处于 dirty 状态，停止绘制约一秒后转为可回收。
- 驱动为什么保留这 512 MiB 没有做驱动层验证。在不修改 GPUI 或 Metal 使用方式的前提下，本应用无法直接消除这一峰值。
- 因此不能宣称已满足启动峰值门槛。这是框架与驱动层面的限制，需要用户或 Codex 决定：接受并如实记录，还是投入修改 GPUI 渲染器。
- 仍未测量：全新 OCR 队列、连续编辑、快速切换、图片缩放等交互场景下的占用。
- 证据文件的 sha256：本应用 `342c4427593664fa766aa2525bced53cacea82e514bb5d85bd90669e448c5b34`，hello_world `a4283292a15a471a726d72bf76020c01dd9632f67901442ad7e8327ed0bb4d96`；hello_world 源码保存在 `/tmp/joplin-claude-memory-evidence/gpuihello-src`。

### 当前源码检查点

`18b57b1cd`（本节报告另行提交）。

### 第九批修正（`8abbebee2`）

Codex 复核发现 18b57b1cd 的措辞有两处不准确：

- JSON 解析错误也可能来自历史文件，不一定是 manifest.json；
- `InvalidManifest` 还包括正文、历史摘要、附件关系不一致等情况，不只是版本不符。

修正后：JSON 错误统一称“数据文件无法解析”；只有 manifest 的格式或版本不符时才说“格式或版本不受支持”；其余情况称“未通过完整性校验”。

测试增加了两个场景：历史文件被改写，以及 manifest 版本改为 999。历史被改写时，提示中不得出现“格式或版本”。

对照：用 18b57b1cd 的措辞运行新测试，失败于写坏 manifest 的场景（`/tmp/joplin-claude-restore-message-narrow-red.log`）。全量测试 1470 通过（`/tmp/joplin-claude-restore-message-narrow-full.log`）。

## 第十批：上下标的数据通路（`9704f7221`，以及 `7acfa24af` 中的编辑器部分）

核心格式新增上下标：`Marks.script: Option<Script>`，用类型保证上标和下标互斥。

- 存储为 `<sup>`/`<sub>`；嵌套时内层生效；粘贴的 HTML 中 `vertical-align: super/sub` 会被识别；ENML 和 JEX HTML 导入会保留它们，不再拒绝或压平。
- 旧 HTML 的解析和序列化结果不变。反向兼容的风险：旧版应用读到新数据时会保留文字，但丢失上下标。
- 编辑器侧：新增 `Mark::Superscript`/`Subscript`；编解码双向映射；切换其中一个会移除另一个；`from_blocks` 拒绝同时含两者的片段，而 `StyledRun::new` 会规范化为只保留上标；“更多”菜单中加入“上标”“下标”，快捷键 Ctrl-Cmd-= 和 Ctrl-Cmd--。

**尚未完成（不能标记为通过）：**
- 基线的上移、下移还没有绘制，现在显示为普通文字；
- 命中测试、光标和选区与视觉效果的一致性待渲染完成后验证；
- 表格单元格的绘制尚未处理上下标；
- 跨设备同步测试（A → 服务器 → B → 编辑 → A → 重新打开）尚未补。

测试：
- core：`superscript_and_subscript_round_trip_and_exclude_each_other`、`evernote_superscript_and_subscript_are_kept`，并把原先断言 `<sub>` 被拒绝的用例改为 `<q>`；
- gpui：编解码往返测试扩展到上下标；`superscript_and_subscript_exclude_each_other_and_undo_in_one_step`；`a_run_never_keeps_both_script_marks`（构造函数规范化与原始片段拒绝两个约定都覆盖）。

## 第十一批：光标处的输入样式（evidence54，`7acfa24af`）

**根因：** 输入后光标的 affinity 被设为 `After`，而旧的 `insertion_marks` 在 `After` 时只看光标之后开始的片段。所以在粗体片段末尾连续输入时，第二次输入不继承格式。这不只影响待输入样式，在已有粗体文字末尾连续打字也会从第二个字起丢失粗体。

**修复：** 光标处的格式改按 ProseMirror `ResolvedPos.marks()` 的规则解析：
- 位于片段内部时，取该片段的格式；
- 位于边界时，取前一段文字的格式（位于块首时取后一段）；
- 链接和代码是非 inclusive 的格式，除非两侧都有，否则不延续。

同一批中其余按 Evernote 源码实现的内容：
- **格式切换：** 选区中任一处带有该格式，就在整个选区移除，否则整体添加，跨段落同样适用（此前是逐段判断）。
- **光标处的待输入格式：** 光标折叠时切换格式，只记录下一次输入要用的格式，不修改文档。它由 `EditorCore` 管理：光标移到其他位置（包括只改变 affinity）即清除；输入法组合的更新、提交、取消过程中都保留；同一位置插入时采用。所有选区赋值都统一经过 `set_selection`。
- **回车：** 在段落末尾回车，或在列表项开头之后回车，新块会带入前面的 inclusive 格式（或待输入格式）。标题不带入。

**两条旧测试改为 Evernote 语义：**
- `collapsed_caret_state_and_insertion_share_one_mark_rule`：在粗体文字之前的块首位置、以及粗体文字末尾（affinity 为 After），都延续粗体；
- `journal_checkpoint...`：在链接之后输入的字不在链接内，但保留粗体和斜体，日志记录的仍是一小段局部变化。

**新增测试：**
- `pending_bold_keeps_applying_to_separate_typing_events`：复现 evidence54，分多次独立输入；
- `pending_bold_on_then_off_covers_separate_typing_events`：开、输入两次、关、再输入两次；
- `a_pending_mark_ends_when_the_caret_moves_away_and_back`；
- `a_pending_mark_survives_ime_updates_commit_and_cancel`；
- `enter_carries_inclusive_typing_style_into_the_new_paragraph`：带入粗体，不带入链接；
- `links_and_code_do_not_extend_at_their_edges`：两种 affinity 下，在左右边界都不延续，在片段内部延续；
- `a_mark_toggle_removes_it_everywhere_when_any_selected_text_has_it`：区分 ANY 与 ALL 两种规则；
- 挂载测试 `mounted_cmd_b_in_an_empty_body_bolds_every_later_typing_event`：真实按键 Cmd-B，分两次输入“Bold中文928”和“CONTINUE”，再 Cmd-B 后输入 x、y，数据库中为 `<p><strong>Bold中文928CONTINUE</strong>xy</p>`。

| 命令 | 结果 | 日志 |
| --- | --- | --- |
| 修复前的复现测试 | 前 13 字节为粗体，CONTINUE 为普通文字 | `/tmp/joplin-claude-pending-bold-red.log` |
| 对照：临时换回旧 affinity 规则 | 3 项新测试全部失败 | `/tmp/joplin-claude-pending-bold-control.log` |
| 全量 gpui / core / native | 1480 / 352 / 225 通过，均为 0 失败 | `/tmp/joplin-claude-pending-style-full2.log` 等 |

**过程说明：** 对照是在共享的 `.shared-target` 中临时替换源码后运行的。Codex 在同一秒（22:15:37）的独立运行因此用到了对照版本，日志 `/tmp/joplin-codex-resume-pending-bold.log` 中的失败值与对照完全一致。源码随即恢复，复查 `/tmp/joplin-claude-pending-bold-recheck.log` 通过。之后的对照改用独立副本和单独的 `CARGO_TARGET_DIR`，并在 Codex 测试或打包期间冻结源码。

**已知与 Evernote 不同的地方：**
- 待输入格式只在光标折叠且位于文字块时生效；
- 输入法取消后，待输入格式仍保留；Evernote 在这种情况下的行为尚未做实机对照；
- 真实拼音输入法下的行为仍需实机验证。

**可供构建的源码检查点：** `7acfa24af`。

## 第十二批：上下标的真实字号与基线（`4b8fbe350`、`63a35dfc7`、`bf02a5381`）

Codex 审查否定了此前的方案：保留正常字号的前进宽度、只绘制缩小的字形，会导致间距和点击命中都不对。现在改为在整形阶段就使用上下标的真实几何，折行、光标、选区和点击命中都跟随实际绘制的字形。

**为什么要改 GPUI：** GPUI 0.2.2 对整行只接受一个字号。因此 vendored 了 GPUI（`packages/app-lite-gpui/vendor/gpui`，通过 `[patch.crates-io]` 引用；原样导入与修改分开提交；改动说明见 `vendor/gpui/JOPLIN-PATCHES.md`）。

**GPUI 的改动：**
- 新增 `RunScript { size_permille, rise_permille }`，`TextRun`、`FontRun`、`ShapedRun` 都带上它；
- macOS 上每个 run 的 CTFont 按各自字号创建，CoreText 返回真实的前进宽度和行宽，行的 ascent/descent 计入基线偏移；
- 每个字形按其字符所在的文字范围确定上下标：真实 CoreText 测试证实，相邻的上标和下标会被合并成同一个 CTRun；
- 绘制时每个字形使用所在 run 的字号和基线；可见性判断的边界计入了偏移。

**App 的改动：** `layout::script_for` 设置为 0.833 倍字号，上标上移 0.333，下标下移 0.2。这对应浏览器显示 Evernote 所写 `<sup>`/`<sub>` 时的默认效果（font-size: smaller、vertical-align: super/sub）。正文和表格单元格都已接通。

**测试：**
- 真实 CoreText 测试：`test_layout_line_script_run_uses_its_own_size_and_baseline`（数字前进宽度约为 0.833 倍，行宽变小，ascent 增加）；`test_layout_line_adjacent_scripts_keep_their_own_script_per_glyph`（拉丁和中文两组，上标与下标紧邻）。对照中改回“按 CTRun 第一个字形”的旧逻辑后，后者失败：下标字符被当成上标（`/tmp/joplin-claude-adjacent-script-control.log`）。
- App 层几何测试 `superscript_is_shaped_smaller_so_spacing_hits_and_wrapping_follow_it`：上标、下标的前进宽度比例都约为 0.833，折行更晚，按缩小后的位置点击命中正确。
- `table_cells_shape_superscript_at_its_own_size`：单元格的 run 带有 script，折行行数少于普通文字。
- 这两项 App 测试的负对照在独立的 git worktree 和独立的构建目录中运行，均失败（`/tmp/joplin-claude-script-shaping-control.log`）。
- vendored GPUI lib 测试 71 通过；App 全量 1482 通过、0 失败。

**已知差异与仍需验证：**
- 下划线和删除线仍画在行的主基线上，跨越上下标时是连续的。如果删除线只作用于上标文字本身，浏览器会穿过上标字形，我们没有这样做；
- Linux 和 Windows 的整形忽略 script（产品只在 macOS 发布）；
- 实际显示效果、中文与拉丁混排、输入法下的光标位置，都需要 Codex 实机验证；
- 跨设备同步测试（A → 服务器 → B → 编辑 → A → 重新打开）尚未补。

**构建说明：** 单独检出 `63a35dfc7` 时 App 编译不过，因为 App 的 `TextRun` 字面量在 `bf02a5381` 中才补上字段。构建请以 `bf02a5381` 为检查点。

### 第十二批补充：审查项的处理与复跑命令

**文字可见性判断（裁剪）：** 已在 `63a35dfc7` 修复。`vendor/gpui/src/text_system/line.rs` 中 `max_glyph_bounds` 的原点改为 `glyph_origin + run_rise`，上移或下移的字形按它实际所在的位置判断是否可见。这一项没有单独的测试，因为测试平台不会光栅化字形。

**装饰线的基线和合并（有意为之，已写入文档，未实机对照）：**
- 本应用序列化时的嵌套顺序是 `a → mark → s → strong → em → u → sup/sub → code`（app-lite-core `document.rs::serialize_text`），所以 `<u>`/`<s>` 总是 `<sup>`/`<sub>` 的祖先。
- 按 CSS Text Decoration 的规则，祖先元素的装饰线按祖先盒子的位置绘制，并延伸到 vertical-align 偏移的后代。因此浏览器（也就是 Evernote 的 DOM）显示我们写出的 HTML 时，下划线和删除线在主基线上连续。
- GPUI 的 `DecorationRun` 只比较颜色和样式，不比较 script，所以跨上下标时合并为连续的一段，画在主基线上，与上述效果一致。
- 未覆盖的情况：删除线仅作用于上标本身（即 `<sup><s>…</s></sup>`），这种写法我们不会产生。
- 尚未用 Evernote 实机截图对照。

**复跑命令（源码冻结在 `bf02a5381`，HEAD `dd2114dfc` 之后只改了文档）：**

```sh
cd packages/app-lite-gpui/vendor/gpui
CARGO_TARGET_DIR=<独立目录> cargo test --lib --features test-support platform::mac::text_system -- --nocapture
CARGO_TARGET_DIR=<独立目录> cargo test --lib --features test-support
cd ../..
cargo test --bin velotype superscript_is_shaped_smaller table_cells_shape_superscript
```

| 测试 | 结果 | 日志 |
| --- | --- | --- |
| 真实 CoreText（mac text_system，5 项，含两个新的上下标测试） | exit 0，5 通过 | `/tmp/joplin-claude-coretext-script-tests.log`（开头记有提交号和命令） |
| vendored GPUI lib 全部测试 | exit 0，71 通过、1 忽略 | `/tmp/joplin-claude-gpui-vendor-lib-full.log` |
| 相邻上下标对照（改回按 CTRun 分配） | 失败：下标字符被当成上标 | `/tmp/joplin-claude-adjacent-script-control.log` |
| App 几何测试（测试平台的模拟整形） | 通过；对照失败 | `/tmp/joplin-claude-script-shaping-full.log`，`/tmp/joplin-claude-script-shaping-control.log` |

App 全量测试使用的是测试平台的模拟整形，只能说明整条流程确实使用了缩小后的几何，不能证明 CoreText 的实际效果。CoreText 的效果由上面两个 mac 测试直接验证，最终视觉效果以 Codex 的实机验收为准。

## 第十三批：上下标内的装饰线按 Evernote 的嵌套绘制（`4aba287cb`、`eeeb07fd6`）

**更正：** 第十二批补充中“装饰线在主基线上连续，与 Evernote 一致”的结论是错的。当时的依据是我们自己写出的 HTML 嵌套，不能证明与 Evernote 一致。

Codex 读取的源码（我已复核）是 common-editor `apps/peso/schema.ts` 121–153 行。其中 marks 的注册顺序注释为“the order here is significant”：`fontfamily, code, bold, fontname, fontsize, lineheight, highlight, italic, subscript, superscript, link, forecolor, baseforecolor, strikethrough, underline`。ProseMirror 按注册顺序嵌套，越靠前越在外层，所以在 Evernote 的 DOM 中，链接、删除线、下划线都在 `<sup>`/`<sub>` 之内，跟随上下标文字的位置和字号绘制，并在上下标与普通文字的交界处断开。Codex 在 evidence56 中另外记录了 ProseMirror 自带的序列化器和 Peso HTMLSerializer 的源码。

**保存（core `serialize_text`）：**
- 带上下标的文字按 `mark, strong, em, sup/sub, a, s, u, code` 的顺序嵌套；
- 不带上下标的文字保持原来的顺序 `a, mark, s, strong, em, u, code`，逐字节不变，因此现有笔记不会被整体改写，也不会引起同步变化；
- 两种顺序都能读回同样的格式。

**绘制（vendored GPUI）：**
- `DecorationRun` 带上 script，文字在上下标与普通之间切换时，装饰线分段断开；
- `paint_line` 在分段的 script 改变时结束当前线段，即使线的样式相同也不接续；
- `script_decoration_shift` 在 GPUI 原有的定位公式上叠加上下标的缩放和基线升降。

**测试：**
- core：`script_text_nests_link_and_decorations_inside_like_evernote`（新顺序、旧顺序读回、无上下标时 HTML 不变）；`sync_two_clients::superscript_and_subscript_survive_a_round_trip_through_another_device`（A → 服务器 → B → 在 B 编辑 → A → 重新打开）。
- GPUI：`script_decoration_tests::lines_follow_the_script_texts_own_baseline`、`decoration_runs_break_where_script_text_starts_and_ends`（真实的 `shape_line`）。

**对照（均在隔离副本或 worktree 中运行）：**
- core 用旧顺序运行新测试，失败，输出为 `<a><s><u><sup>`（`/tmp/joplin-claude-decoration-order-control.log`）；
- GPUI 去掉 script 比较并把偏移设为零，两项都失败（`/tmp/joplin-claude-decoration-gpui-control.log`）。

**全量：** core 354、vendored GPUI 73（另 1 项忽略）、App 1482，均为 0 失败。

**仍然不同或未验证：**
- Evernote 把 `code` 放在最外层，粗体在高亮之外，链接在上下标之内而颜色在链接之内。本批只改了与上下标相关的嵌套，其他顺序差异不影响格式本身，但会影响导出 HTML 在浏览器中的层叠效果，尚未处理；
- `paint_line` 在 script 边界结束线段的逻辑没有自动测试（测试平台不绘制）。线的位置函数和分段断开两部分已分别测试，最终效果以实机验收为准；
- 新包的原生视觉验收仍未完成（Codex 的 CUA 连接中断）。

**源码检查点：** `eeeb07fd6`。


## 第十四批：文字颜色（`08adbf71b`、`5dfcf65b9`）与纯表格笔记的初始选区（`6975b88ed`）

### Evernote 源码对应

- 颜色数据：common-editor `textformatter/schema.ts` 470–554 行的 forecolor mark，保存为 `<span style="color: …">`；深色模式下设置的颜色另写 `--inversion-type-color: simple`。
- 命令语义：`textformatter/commands/forecolor.ts`。
  - 有选区时，替换选区内的颜色；
  - 光标处的颜色存为待用样式，作用于下一次输入；
  - 传 `null` 时恢复默认颜色；
  - 选区内颜色不一时，查询结果为混合。
- 合法颜色：`utils/color.ts` 的 `isValidColor` 使用 `color-string`，其 `node_modules/color-string/index.js` 156–174 行支持逗号分隔的 `hwb()`。
- 色板：`apps/peso/defs.ts` 50–61 行 `forecolorPalette.light` 的 14 个颜色，顺序与值都一致。
- 空文档的选区：`selection/commands/setselectiontoend.ts` 调用 ProseMirror 的 `Selection.atEnd`，找不到文字位置时落在已有的原子节点上，不会指向不存在的节点（evidence60）。

### 实现

**core（`08adbf71b`）**
- `TextColor` 保存 sRGB 与 alpha，解析交给 `cssparser` 0.29.6：CSS Color 3 的全部关键字、3/4/6/8 位十六进制、数字或百分比形式的 `rgb()`/`rgba()`、`hsl()`/`hsla()`。
- 逗号形式的 `hwb(h[deg], w%, b%[, a])` 由 cssparser 的 tokenizer 读取，再用它的 `hwb_to_rgb` 转换，没有自写颜色换算。
- `currentcolor`、带多余内容的值和非颜色值都不会成为颜色。
- 保存形式：不透明为 `#rrggbb`；半透明为 `rgba(r, g, b, a)`，alpha 保留三位小数，不做压平。
- ENML、JEX 和粘贴都保留颜色；JEX 中 span 除 `color` 与 `--inversion-type-color` 外的样式照旧拦截。

**gpui（`5dfcf65b9`）**
- `Transaction::SetTextColor`：光标处设置时进入待用样式，并与粗体共用同一套生命周期。审查中发现光标颜色原先没有记录 `pending_marks_at`，光标移开后颜色不会清除，已补上。
- 更多菜单中的“字体颜色”打开色板，内容为“默认”加 14 色；当前颜色有描边；选色后焦点回到编辑器。
- 正文和表格都按 alpha 绘制颜色。
- 更多菜单限高 300px，加入这一行后“Outdent list”落到可见区域之外。挂载测试改为先滚动再点击，没有放宽菜单。

**纯表格笔记（`6975b88ed`）**
- 原因：`Document::end_selection` 在没有文字块时返回 `NodeId(0)`，History 在修改前验证选区时拒绝，因此单元格的编辑无法保存。
- 修复：没有文字块时，选区落在最后一个已有块的 offset 0。`validate_point` 本来就允许非文字块的 offset 0。History 验证没有放宽，也不插入可见段落。

### 测试

- core：
  - `document_roundtrip::text_colour_round_trips_as_evernote_writes_it_and_rejects_non_colours`：rebeccapurple、百分比 rgb、hsl/hsla、8 位十六进制 alpha、`rgba(…, 0.5)`、transparent、`!important`、四种逗号 hwb（含负色相、deg、alpha、w+b≥1 的灰色）。每种都覆盖 parse_html、粘贴和保存后重读。拒绝项包括 currentcolor、hwb 缺参数、hwb 后跟多余内容、`turn` 单位以及注入值。
  - `enml_convert`：hsl、`<font color>` 关键字、rgba alpha、hwb 导入；currentcolor 和 `url()` 被丢弃。
  - `jex_html_conversion`、`sync_two_clients::text_colour_survives_a_round_trip_through_another_device`（与上下标共用往返辅助函数）。
- gpui：
  - `text_color_sets_replaces_resets_and_carries_at_the_caret`：替换颜色、混合状态、一步撤销、默认清除、光标移开再回来清除、分次输入续用光标颜色。
  - `font_color_palette_colours_and_resets_the_selection`：挂载 chrome，依次点击 More → 字体颜色 → 色块。选区保持不变；Cmd‑Z 和 Cmd‑Shift‑Z 后颜色与选区都正确；再点“默认”清除颜色。
  - `cell_text_colour_keeps_alpha_and_saves_a_new_colour`：纯表格笔记，没有前置段落，也没有注入选区。保留半透明蓝，给另一段加 hwb 绿后保存；主编辑器撤销、重做后分别保存；再用新的 NoteSession 重新打开，结果一致。
  - `an_atom_only_document_starts_on_an_existing_block_and_edits_with_history`：仅表格、仅图片、图片加附件三种文档。初始选区落在最后一块上，编辑、撤销、重做、导出后重新打开，结果都一致。

### 对照（隔离副本，独立 `CARGO_TARGET_DIR`，未触碰共享源码）

- 修复前，纯表格用例在共享工作区失败，未保存颜色：`/tmp/joplin-claude-table-only-red.log`。
- 去掉 `end_selection` 修复：atom-only 和纯表格两项都失败。
- 去掉光标颜色的 `pending_marks_at`：颜色测试在“光标移开再回来清除”一步失败（此前的版本没有这一步，所以对照没有失败，已补上）。
- 去掉逗号 hwb 分支：core 颜色测试失败（`/tmp/joplin-claude-colour-hwb-negative-control.log`；同一日志中后面的 gpui 编译失败是因为隔离副本当时缺少 fixture，不计入结果）。
- 以上两项对照的脚本与输出：`/tmp/joplin-claude-colour-table-negative-controls.{sh,log}`。

### 原子提交与逐个验证

- `08adbf71b` core 与两个 Cargo.lock（新增 cssparser 0.29.6 及其依赖，离线解析）。
- `5dfcf65b9` gpui 颜色部分。由于 model.rs 和 tests.rs 中同时有纯表格修复，这一提交只通过索引暂存，没有改动共享工作区文件。在把暂存内容导出的隔离目录中完整运行：1484 通过、0 失败、2 忽略，另有 17 通过，退出码 0（`/tmp/joplin-claude-colour-intermediate-5dfcf65b9-gpui.log`）。
- `6975b88ed` 纯表格修复加两项回归。
- 最终 HEAD `6975b88ed` 全量（均 `--offline --locked`，退出码 0）：
  - App：1486 通过、0 失败、2 忽略，另有 17 通过（`/tmp/joplin-claude-colour-gpui-full.log`）；
  - core：33 组共 357 通过、0 失败、1 忽略（`/tmp/joplin-claude-colour-core-full.log`）。

复跑命令：在 `packages/app-lite-gpui` 与 `packages/app-lite-core` 下分别运行 `cargo test --offline --locked`；单项加过滤器，例如 `cargo test --offline --locked --bin velotype -- cell_text_colour atom_only text_color font_color`。

### rustfmt

只整理了我改动的部分。`table_cell_editor_tests.rs` 在 HEAD 已有 16 处 rustfmt 差异，保持原样。`lib.rs` 的检查会连带 `sync_store.rs` 中原有的差异，同样未改动。

### 仍未完成或未验证

- 打开表格单元格时，Library 工具栏和色板仍然作用于主编辑器，没有接到单元格编辑器。单元格颜色目前只经过命令层和保存通路的验证，界面上还不能直接给单元格文字选色。这是界面接线缺口，本批没有改设计。
- 深色模式反色：编辑器只有浅色主题。`--inversion-type-color` 只保存和往返，不参与绘制。
- 颜色写在链接之内，与 Evernote 相同；code 和高亮的层叠顺序差异仍按第十三批所述，未处理。
- 另一个已有行为：只有一张图片的文档，全选后按删除返回 `CannotRemoveLastNode`，而 Evernote 会留下空段落。本批没有修改，留作后续问题。
- 挂载测试环境不是真实 GPU 窗口。色板外观、alpha 绘制效果和纯表格笔记的实机操作，都需要 Codex 做原生验收。
- 图标打包（evidence57）按指示暂缓。


## 第十五批：按路由区分的空列表（`b97a61b02`）、废纸篓菜单（`9664485e2`）、Dock 图标打包（`d249b330f`）

### Evernote 源码对应（空列表）

以只读方式解析 `/Applications/Evernote.app/Contents/Resources/app.asar`，只把引用相关文案键的 renderer 文件取到 scratchpad。安装的 App 未作改动。中文文案取自 `main-readable/src/modules/64482__localization-catalog.js`。

| Evernote 列表组件 | 条件 | 文案键（中文） | 本项目 |
|---|---|---|---|
| AllNotesNoteList（7873.js / 9106.js） | 空且无筛选 | `AllNotes.empty.title`（创建第一条笔记） | 所有笔记：保留原首次使用空态“从第一篇笔记开始 / 新建第一篇笔记”（`library-empty-state`） |
| AllNotesNoteList | 空且有筛选 | `Search.results.noResults` + `noResultsPrompt`（未找到笔记 / 尝试使用不同的关键词或筛选条件。） | 搜索：`search-empty-state`，无新建按钮 |
| NotebookNoteList（1503.js，`EMPTY_LIST` / `FILTERED_LIST` 三态） | 空 | `Notebook.empty.title` / `.text`（一切从笔记开始 / 点击侧边栏中的+新建笔记按钮创建笔记。） | 笔记本：`notebook-empty-state`，按钮“新建笔记”，笔记建在该笔记本中 |
| StackNoteList（7468.js） | 空 | 同一个 Notebook.empty 组件（module 416978） | 笔记本组：同样的文案，但没有按钮。在组内新建需要选择笔记本（`create_note` 对多个子笔记本会明确报错） |
| TrashNoteList（2195.js） | 空 | `Trash.empty.header`（废纸篓是空的）+ `Trash.empty.description` | 废纸篓：`trash-empty-state`，无新建按钮；说明文字为改写（见下） |
| 标签 | Evernote 的标签视图是笔记列表上的筛选 | 按筛选后为空处理：未找到笔记 | 标签：`filtered-empty-state`，无新建按钮 |

与 Evernote 的差异：
- 废纸篓说明改写为“当废纸篓中有笔记时，可以在这里还原或删除它们。”Evernote 原文让用户点“…”，而本项目的列表没有该按钮。
- 所有笔记沿用已验收的首次使用文案，没有改为 Evernote 的“创建第一条笔记”。
- 只有废纸篓中有笔记、而其余地方为空时，“所有笔记”仍显示“资料库为空”。这与事实不符。要改正，需要在渲染时知道废纸篓数量，而当前导航索引不含该数据，因此留作后续问题，没有在渲染中查询 SQL。

### 废纸篓菜单（evidence31）

- 问题：没有选中笔记时，原生“移至废纸篓”菜单仍会执行，显示 `requested entity was not found`。
- 修复：`TrashSelected` 的处理器只在满足两个条件时注册：有选中笔记，且当前不在废纸篓。
- GPUI 按 `is_action_available` 验证原生菜单项（`vendor/gpui/src/platform/app_menu.rs` 238 行），因此菜单项会被禁用，Cmd‑Shift‑Backspace 也不再触发。
- 右键菜单本来只对选中的笔记出现，未改动。

### 测试

- `ui::tests::an_empty_list_shows_what_its_route_means_not_an_empty_library`：挂载 LibraryShell，资料库中有一篇真实笔记。
  - 所有笔记：不显示空态；
  - 废纸篓、空笔记本、空笔记本组、未使用的标签：各自的空态、标题和按钮都符合预期；
  - 真实搜索提交（`begin_search` + `commit_search_results`）无结果：显示搜索空态；
  - 点击空笔记本的“新建笔记”：空态消失，笔记建在该笔记本中。
  - GPUI 的 `Frame::clear` 不清 `debug_bounds`，旧帧的选择器会残留，无法据此判断某元素是否已消失。因此测试读取每帧实际渲染的空态记录（仅测试构建有），并用 `debug_bounds` 确认该元素确实绘制出来。
- `ui::tests::move_to_trash_is_unavailable_without_a_note_to_move`：
  - 废纸篓无选择、所有笔记无选择：动作不可用，按快捷键后状态仍为 Ready；
  - 所有笔记中有选中笔记：动作可用。
- 真正的空资料库仍由原有的 `mounted_default_light_route_keeps_every_editor_state_opaque_and_contrasted` 覆盖 `library-empty-state`。

**对照**（隔离副本，独立 `CARGO_TARGET_DIR`，脚本与输出为 `/tmp/joplin-claude-empty-state-negative-controls.{sh,log}`）：
- 让所有路由都回到资料库空态：路由测试失败，输出正是 evidence53 的症状——废纸篓显示“从第一篇笔记开始”并带新建按钮；
- 无条件注册移入废纸篓：可用性测试失败。

**全量与中间提交：**
- 中间提交 `b97a61b02` 只经索引暂存，没有改动共享工作区文件。导出为独立目录后单独运行：1487 通过、0 失败、2 忽略，另有 17 通过（`/tmp/joplin-claude-empty-state-intermediate-gpui.log`）。
- 工作区全量结果见 `/tmp/joplin-claude-empty-state-gpui-full.log`。该次运行之后我对 `mod.rs` 和 `tests.rs` 只做了 rustfmt 格式调整，最终 HEAD 的全量结果见下文。

### Dock 图标（evidence57）

- 源图：Codex 生成的 `assets/AppIcon-dock-v2.png`（1254px，带 alpha，SHA256 `1c75fcaf…a3b4`），已原样提交，文件未改动。旧素材也未改动。
- `scripts/make-app-icon.sh`：
  - 要求源图为正方形且不小于 1024px；
  - 用 `sips` 生成 16/32/128/256/512 pt 的 1x 与 2x，共 10 个 representation；
  - 用 `iconutil` 打包。
  - 各尺寸都由同一张简化、非绿色的图缩放而来，所以无论 Dock 选用哪个尺寸，显示的都是这只老鼠。
- `scripts/package-notes-macos.sh`：
  - 加入 `Contents/Resources/AppIcon.icns` 和 `CFBundleIconFile=AppIcon`；
  - 签名后核对 plist；
  - BUILD-INFO 记录 `icon_source_sha256` 与 `icon_icns_sha256`。
  - 仍只写入新的输出目录：不安装、不重置图标缓存、不写 `CFBundleDocumentTypes`。
- `scripts/test-app-icon.sh`：
  - 生成后再用 `iconutil` 解回 iconset，检查 10 个文件的像素尺寸、四角透明（alpha=0）、可见像素中没有绿色；
  - 负控：绿色图必须不通过颜色检查，非正方形源必须被拒绝且不生成 icns。
  - 结果：退出码 0；各尺寸 green=0，四角 alpha 为 0；icns SHA256 `37f8129b…54b2`（`/tmp/joplin-claude-icon-test.log`）。

**隔离打包：** 在 HEAD `d249b330f` 运行 `scripts/package-notes-macos.sh /tmp/joplin-claude-icon-candidate`，退出码 0（`/tmp/joplin-claude-icon-package.log`）。
- 输出：`/tmp/joplin-claude-icon-candidate/20260930T202805Z-d249b330f/Joplin Lite.app`，`worktree_dirty_for_app_sources: no`；
- binary SHA256 `100ecc5c…aaa2`，icns SHA256 与测试生成的一致，为 `37f8129b…54b2`；
- 签名为 ad-hoc，未公证。

**对打包结果的独立核验**（`/tmp/joplin-claude-icon-bundle-verify.log`）：
- plist 中 `CFBundleIconFile=AppIcon`，没有 `CFBundleDocumentTypes`，`plutil -lint` 通过；
- `codesign --verify --deep --strict` 通过，Sealed Resources files=1（即 icns）；
- 从包内 icns 解出的 10 个 representation，尺寸、四角透明、无绿色都符合要求。

小尺寸对照图 `/tmp/joplin-claude-icon-sizes-16-32-64-128.png`（最近邻放大）：深灰底、象牙白老鼠。16px 时只剩双耳与头形，但仍可辨认。

**未做：**
- 没有启动 App，因为启动可能打开真实资料库；
- 没有安装，`/Applications` 下没有 Joplin Lite；
- 没有重置图标缓存。
- 因此 Dock、Launchpad、Finder 的实际显示，以及 Dock 在高分屏上选用哪个尺寸，都需要 Codex 在隔离环境中实机核验。

**最终 HEAD `d249b330f` 全量**（均 `--offline --locked`，退出码 0）：
- App：1488 通过、0 失败、2 忽略，另有 17 通过（`/tmp/joplin-claude-batch15-gpui-full.log`）；
- core：33 组共 357 通过、0 失败、1 忽略（`/tmp/joplin-claude-batch15-core-full.log`）；
- 图标：`scripts/test-app-icon.sh` 退出码 0（`/tmp/joplin-claude-icon-test.log`）。

### 仍未完成或未验证

- 空列表、菜单禁用状态和图标都没有做实机视觉验收。
- 只有废纸篓中有笔记、而其余地方为空时，“所有笔记”的“资料库为空”文案不准确（见上）。
- 废纸篓中有选中笔记时，“移至废纸篓”现在不可用，但原生菜单没有对应的“还原 / 永久删除”项，这两个操作仍只在右键菜单中。
- 图标仍是 ad-hoc 签名，未公证。最终安装包，以及从旧版本升级，属于后续安装关卡。
- 其余交付关卡没有推进：迁移保真（85 篇降级、严格 1581 篇）、多模态、NAS 常驻、启动内存峰值、最终安装包。


## 第十六批：迁移保真（`8bb589151`、`86383e2dc`、`3d6ceaea0`）

### 隔离副本与基线

- 此前的隔离副本 `/tmp/joplin-migration-current.MeMdVA` 已被清理，本轮重新建立。
- 源文件为 `/Users/kevinhao/JoplinBackup/default/all_notebooks.jex`，只读取，不修改。它已被 Joplin 自动备份更新（mtime 2026‑09‑30 22:27），SHA256 为 `320f684f…c6cf`，与证据中的 `8544080e…` 不同，共 1667 篇（比原来多 1 篇）。因此本轮基线是新导出，数字不能与 1581/85 直接比较。
- 复制到 `/tmp/joplin-claude-migration.pLtZsa/src/`，设为只读，复制前后 SHA256 一致（`/tmp/joplin-claude-migration-source-sha-before.txt`）。
- HEAD `92c2f734e` 下的隔离导入（`import-verify.log`）：
  - 1667 笔记、31 笔记本、2 组、64 标签、4153 资源、4127 blob；
  - blob 重新哈希 0 不符，源 blob 缺失 0、多余 0；
  - 降级 85，首个原因的分布与 evidence47 相同。
- 只读审计：严格通过 1582、警告 85。

所有输出只含计数、source_id 和结构形态，不包含正文或属性值。

### 行内 HTML（`86383e2dc`，探针 `8bb589151`）

新增探针模式 `JOPLIN_LITE_AUDIT_HTML_SHAPES=1`：对每篇 Raw HTML 笔记只输出标签名、属性名和 style 中的 CSS 属性名。21 篇 Raw HTML 笔记的实际形态：
- 13 篇只有 `<a id>` 锚点；
- 约 8 篇有 Joplin 缩放的行内 `<img src=":/id" width height>`；
- 各 1 篇 `<sup>`、`<del>`；
- 2 篇是大段粘贴的富 HTML（带 p 样式、onclick 等）。

**Evernote 对应：**
- `textformatter/schema.ts` 320–349：b/strong、i/em、u、s/strike/del、mark、sup/sub 解析为格式标记；
- `link/schema.ts` 43–57：只把 `a[href]` 当作链接，所以只有 id 或 name 的锚点保留文字、不加链接。

**实现**（`jex_body.rs`）：
- Markdown 行内循环识别成对的行内 HTML，无属性的格式标签转为对应标记，标签之间的 Markdown 照常解析；
- `<img>` 交给严格 HTML 转换器，沿用它的属性白名单、width 像素值和已验证资源检查；
- 其他标签或属性、错误嵌套、未闭合的标签仍阻断，不会被丢弃。

**测试：**
- `inline_html_maps_to_evernote_marks_anchors_and_resized_images`：正向映射、canonical 往返、资源出现顺序，另有 8 个仍应阻断的样例；
- 更新了两个旧测试。`上<sup>标</sup>` 原本被断言阻断（`becdff6d6` 写这项测试时还没有上标），现在改为转换；降级路径测试原用表格单元格中的 `<b>`，现在它不再降级，所以改用不受支持的 `<span class>`。
- 负控：`inline_html` 一律返回 false 时两项失败（`/tmp/joplin-claude-inline-html-negative-control.{sh,log}`）。

**效果：** 严格通过 1582 → 1594，降级 85 → 73。原 21 篇的去向：
- 12 篇完全通过；
- 7 篇 HTML 解决后，暴露出下一个原因：外链图片 +4、链接 title +2、不安全链接 +1；
- 2 篇行内 `<img>` 指向的资源不是 png/jpeg/gif/webp，仍阻断；
- 1 篇大段富 HTML 仍阻断。

`<font color>` 和 `<span style=color>` 在 Markdown 行内 HTML 中仍阻断：本批没有加颜色映射，涉及的笔记同时还有其他阻断。

### 代码块语言（`3d6ceaea0`）

动手前在隔离副本中做了“假设放行”实验（`/tmp/joplin-claude-migration-whatif*.{sh,log}`），只在副本中进行，不进入产品代码：放行代码块语言可净增 3 篇，放行链接和图片 title 可净增 4 篇，两者同时放行可净增 7 篇。

**Evernote 对应：** `codeblock/schema.ts` 12 行定义了 `syntaxLanguage`。

**实现：**
- canonical `Block::Code.language`，序列化为 `<pre data-language>`，只接受 highlight.js 风格的名称（1–32 个字符，范围 `A-Za-z0-9+#._-`）；
- 围栏的 info 字符串是单个语言名时即转为该语言；
- 多词 info 字符串、`{.rust}` 写法，以及 Joplin 会渲染为图表、乐谱、剧本的 mermaid/abc/fountain 围栏，仍阻断，不会退化成代码显示；
- 粘贴的 HTML 不设置语言；
- 原生编辑器仿照列表起始号，用按块索引的旁表保存语言。编辑块内文字后保存，语言仍在；含行内图片的代码块（分组）也保留。后者是新测试第一次运行就发现的缺陷，已修复。

**测试：**
- core：`fenced_code_keeps_its_language_as_evernote_syntax_language`、`code_block_language_round_trips_and_only_a_language_name_is_kept`（含注入值与空值被丢弃、粘贴不设语言）；
- gpui：`code_block_language_survives_editing_and_saves_with_its_block`，codec 全结构往返夹具改为带 `rust`；
- 负控：导入时丢掉语言，core 测试失败；codec 不写入旁表，两项 gpui 测试失败（`/tmp/joplin-claude-code-language-negative-controls.{sh,log}`）。

**效果：** 严格通过 1594 → 1596，降级 73 → 71。实验预测 +3，差的 1 篇属于应继续阻断的围栏（mermaid 或多词 info）。

### 验证（均为隔离副本）

| 项目 | 结果 | 日志 |
|---|---|---|
| 新转换器重新导入（`86383e2dc`） | 降级 73，blob 全部核对无误 | `/tmp/joplin-claude-migration.pLtZsa/import-inline-html.log` |
| 同上，原生往返 | 1667 篇，`failure_categories={}` | `/tmp/joplin-claude-migration-native-roundtrip.log` |
| 重新导入（`3d6ceaea0`） | 降级 71，blob 全部核对无误 | `/tmp/joplin-claude-migration.pLtZsa/import-code-language.log` |
| 同上，原生往返 | 1667 篇，`failure_categories={}` | `/tmp/joplin-claude-code-language-native-roundtrip.log` |
| core 全量 | 33 组共 360 通过、0 失败、1 忽略 | `/tmp/joplin-claude-code-language-core-full.log` |
| App 全量 | 1489 通过、0 失败、2 忽略，另有 17 通过 | `/tmp/joplin-claude-code-language-gpui-full.log` |

原生往返只比较资源顺序和非空白文字，不代替排版与视觉验收。

### 剩余 71 篇（首个原因）与判断

| 首个原因 | 篇数 | 判断 |
|---|---|---|
| 行内构造（主要是 `$…$` 行内数学） | 17 | Evernote 没有行内公式节点，只有块级 formulablock（`$$…$$`，`formulablock/doubleDollarSignTypeBehind.ts`）；`inlineMathSuggestionsEnabled` 是计算建议，不是 LaTeX。按已定原则保留源码、不展平，继续标为降级 |
| 外链图片 | 17 | 需要抓取外部资源，按约束不无条件抓取 |
| 链接 title / 图片 title | 8 + 1 | Evernote link 有 `title` 属性（`link/schema.ts` 11）。需要把链接标记改为带 title 的结构，会贯穿编辑器的链接命令，改动面大；实验表明可净增 4 篇 |
| 引用内含列表或标题 | 7 | Evernote quoteblock 的内容为 `(p \| todolist \| ol \| ul \| h)+`（`quoteblock/schema.ts` 14）。canonical Quote 只能容纳行内内容，是确实存在的模型缺口；修复需要容器型引用，原生编辑器也要支持 |
| 链接图片双目标 / 不安全链接 / 非标准图片类型等 | 各 1–4 | 逐项保持阻断，都有明确原因 |
| mermaid 围栏（包含在代码语言等类别中） | — | Evernote 有 `mermaidblock`，需要新增节点类型与渲染 |

### 另发现的 ENEX 缺口（不在本次 JEX 审计范围内）

Evernote 的 ENML 用 `<en-codeblock>` 或 `--en-codeblock:true` 表示代码块（`codeblock/schema.ts` 99–140），但 `enml.rs` 没有识别，ENEX 导入的代码块很可能变成普通段落。这需要单独修复并补 ENEX 测试，本批没有改动。

### 边界

- 原资料库与原 JEX 只读取，未修改；
- 没有启动 App，没有安装；
- 迁移的视觉验收（图文、列表、表格、附件、长文）仍未做；
- 71 篇降级的可接受性，由 Codex 结合原文逐篇判断。


## 第十七批：ENEX 代码块（`2f848a980`）与容器型引用（`39cd18525`）

### ENEX 代码块（`2f848a980`）

**Evernote 对应**（common-editor `codeblock/schema.ts`）：
- parseENML 的两条规则：
  - `div[style*="codeblock"]`，并且满足以下之一：`--en-codeblock` 为 true；或 `white-space` 为 `pre`/`pre-wrap`/`pre-line` 且字体族含 `monospace`；
  - `<pre>`。
- 内容由 `getContent` 取纯文本并按行拆分（`plaintext` 节点 `marks: ''`，不带格式）。
- `syntaxLanguage` 由 `--en-syntaxLanguage` 给出。`utils/schema.ts` 的 `getAttributesFromStyle` 也接受单横线的 `-en-`。

**修复前：** 多行代码块报 `unsupported inline <div>`，整篇阻断；单个 div 形式的代码块变成普通段落，代码块语义被静默丢失。

**实现**（`enml.rs`）：
- 按上述规则识别代码块；
- 子 div、`<br>` 按行分隔，空行和缩进都保留（canonical 模型用 NBSP 表示需要保留的空格）；
- 行内格式按 Evernote 规则去掉；
- 语言必须是单个语言名，否则阻断；
- 代码块内的表格、列表等块结构阻断。
- 有意与 Evernote 不同的一点：Evernote 会把代码块里的 en-media 压成纯文本，这里保留资源，不静默丢弃。

**测试：**
- `enml_convert::evernote_code_blocks_keep_language_lines_and_resources`：
  - 正向：双横线、单横线、false 加等宽字体、`<pre>`、语言、行结构与缩进、格式去除、资源保留；
  - 负向：style 中没有 codeblock 字样的等宽 div 仍是段落；多词语言、表格、列表阻断。
- `enex_stage::evernote_code_block_imports_with_language_and_attachment`：ENEX 整条导入链路，笔记不降级，`note_resources` 有 1 条。
- 负控：关闭识别后测试失败（`/tmp/joplin-claude-enml-codeblock-negative-control.{sh,log}`）。
- core 全量 361 通过。
- JEX 迁移不走 ENML 路径，只读审计仍为 1596/71（`audit-enml-codeblock.log`）。没有可用的真实 ENEX 源，所以没有 ENEX 迁移计数。

### 容器型引用（`39cd18525`）

**Evernote 对应：**
- `quoteblock/schema.ts` 14 行：`content: '( p | todolist | ol | ul | h ) +'`，quoteblock 不能包含 quoteblock、代码块或表格；
- `quoteblock.ts`：加引用时把选中的 p/列表/标题整体包进去，在引用内再次切换则把整块引用解开；
- 样式来自 `ce.css`：`blockquote { border-left: var(--spacing-0-25) solid var(--color-icon-fill-tertiary-enabled); padding-left: var(--spacing-2) }`，浅色主题取值为 2px、`#4e4d4c`（`body` 下为 grey‑30）、16px。

**canonical（core）：**
- `BlockStyle.quoted` 标记位于引用内的标题或列表项；引用内的段落仍用 `Block::Quote`；代码块等不能带这个标记，规范化时清除。
- 序列化：一段连续的引用块中只要含有标题或列表，就整体写成 `<blockquote data-joplin-lite-quote-container="true">`，子元素为 p/h/ul/ol。只有段落的引用保持原来的逐段形式，逐字节不变。
- 列表不会跨越引用边界，内外混合的列表会拆开；引用内外的同类列表不会合并。
- 解析时，容器子元素按常规解析，结束时统一标记。

**JEX：** 引用中的段落、标题、列表组成容器；嵌套引用、代码块、表格、分隔线、块级媒体仍阻断。

**原生编辑器：**
- `Block.quoted` 与 alignment 一样，随拆分、合并、粘贴、撤销和重做传递；
- 引用内的标题或列表项改变类型时仍留在引用中：在空列表项上按回车或退格，会变成引用段落；
- 引用段落设为普通段落时离开引用，与原来一致；
- 引用块向右缩进 18px，并画 2px 竖线，相邻的引用块连成一条。

**测试：**
- core：
  - `quote_container_round_trips_and_holds_only_quoteblock_content`：往返；只含段落的容器写回逐段形式；容器内的代码块移到引用外；混合列表拆开；
  - `quote_with_lists_and_headings_becomes_one_quote_container`；
  - 原测试中“引用内含列表或标题必须阻断”的两个样例，现在是本次支持的内容，改为代码块、嵌套引用、分隔线、表格四个仍应阻断的样例。
- gpui：
  - `quote_container_edits_keep_lists_and_headings_inside_the_quote`：导入时各块的 `quoted` 正确；引用列表中回车产生引用内的新项；空项回车变为引用段落；撤销和重做；重开；布局左移 18px；
  - `ui::tests::quote_container_note_edits_save_and_reopen`：挂载 LibraryShell，编辑后经 ManualSync 写入资料库，再用新的 NoteSession 重开，结构一致。
- 负控（`/tmp/joplin-claude-quote-negative-controls.{sh,log}`）：JEX 不标记引用、原生改类型时丢掉引用、codec 导入时丢掉 `quoted`，三种情况下对应测试都失败。

**迁移（隔离副本，源 SHA256 `320f684f…c6cf` 未变）：**

| 项目 | 结果 | 日志 |
|---|---|---|
| 只读审计 | 严格通过 1596 → 1598，警告 71 → 69 | `/tmp/joplin-claude-migration.pLtZsa/audit-quote.log` |
| 新导入 | 降级 69，blob 全部核对无误 | `/tmp/joplin-claude-migration.pLtZsa/import-quote.log` |
| 原生往返 | 1667 篇，`failure_categories={}` | `/tmp/joplin-claude-quote-native-roundtrip.log` |

原来 7 篇“引用内含非段落结构”中，2 篇完全通过。剩下 5 篇在隔离副本中按子块类型诊断（只输出类型名）：3 篇是引用中嵌套引用，Evernote 同样不支持；2 篇仍报原来的原因。

**全量**（最终 HEAD `39cd18525`，均 `--offline --locked`，退出码 0）：App 1491 通过、0 失败、2 忽略，另有 17 通过（`/tmp/joplin-claude-batch17-gpui-full.log`）；core 33 组共 364 通过、0 失败、1 忽略（`/tmp/joplin-claude-batch17-core-full.log`）。完成后再次核对，原 JEX 的 SHA256 未变。

### 边界与剩余

- 编辑器没有“引用”切换命令，这是此前就存在的缺口；Evernote 的整块包裹和解开还没有对应的交互。
- ENML 中的 `<blockquote>`、粘贴的 blockquote 里含列表的情况，本批都没有改。
- 引用样式只有浅色主题；两段相邻的独立引用会合并成一段显示。
- 视觉和实机验收仍需 Codex 完成。

## 第十八批：引用切换（`bd58556a1`）与 ENML、粘贴中的引用（`5d867fbc8`）

### 引用切换（`bd58556a1`）

**Evernote 对应**（common-editor `quoteblock/commands/quoteblock.ts`、`quoteblock/quoteblock.ts` `insertQuoteblockAtSelection`）：
- `queryCommandValue`：选区的任一范围在引用内（`isRangeInQuoteblock`）时，命令为开启状态；
- 已在引用内：把选区触及的每一段引用整体解开，内容放回原处（`tr.replaceWith(..., node.content)`）；选区折叠时，光标移到原引用的开头（`firstExtractedPos`）；
- 不在引用内：选区扩展到整块，内容必须是 `( p | todolist | ol | ul | h )+`（`validContent`），否则返回 false；包裹后选中全部被包裹的内容（`newAnchor = pos + 2`，`newHead = newAnchor + size - 2`）；选区为空时包裹当前块。

**实现：**
- `Transaction::SetQuote { selection, quote }`：解开时向两侧扩展到整段引用，包裹时扩展到整块及其行内分组；
- 段落与引用段落互换，标题和列表设置或清除 `quoted`；
- 一次操作只产生一步撤销（`RestoreBlocks`）；
- 它属于 `changes_parent` 操作，所以带图片的引用段落（行内分组）会整组包裹或解开；
- 选区按上面的 Evernote 规则设置。

**命令与界面：**
- `EditorCommand::Quote` 放在更多菜单中，显示“引用”；
- 选区内有代码块、表格或独立的块级媒体时，命令禁用；
- 引用段落改成标题或列表时仍留在引用中；改成普通段落时离开引用，与原来一致。

**编号：** 保存时，引用内的列表和引用外的列表是两个独立列表，所以原生编辑器的有序编号也在引用边界处从 1 重新开始（`layout::crosses_quote_boundary`）。

**测试：**
- `ui::tests::toolbar_quote_wraps_and_unwraps_whole_quotes_with_history_ime_and_save`：挂载 LibraryShell，通过真实的更多菜单点击（带滚动）依次验证：
  - 包裹标题、段落和列表，检查导出和选区；
  - 光标在引用内时解开整段引用，光标回到开头；
  - Cmd‑Z / Cmd‑Shift‑Z；
  - IME 组合进行中点击，组合文字先提交再被包裹，从光标包裹后选中整块；
  - ManualSync 保存后与数据库一致，用新的 NoteSession 重开后一致。
- `native_editor::tests::quote_toggle_takes_only_quoteblock_content_and_restarts_numbering`：含代码块时命令禁用；带行内图片的引用段落（分组）整体解开并重新包裹；有序列表前两项被引用后的编号为 1, 2, 1。
- 负控（`/tmp/joplin-claude-quote-toggle-negative-controls.{sh,log}`）：
  - 解开时只处理选中的块：挂载测试失败；
  - 去掉编号在引用边界的重新计数：边界测试失败；
  - 包裹后不设置 Evernote 选区：第一次对照没有失败，因为测试中包裹前后的选区恰好相同，测试不够严。补上“从光标包裹后选中整块”的断言后重跑，挂载测试失败。

### ENML 与粘贴中的引用（`5d867fbc8`）

**Evernote 对应：** `quoteblock/schema.ts` 的 `parseENML` 与 `parseClipboard` 都按 `blockquote` 解析，内容为 `( p | todolist | ol | ul | h )+`。

**ENML：** 此前 `<blockquote>` 会让整篇阻断。现在转为引用容器：
- div 作为段落，保留行内图片；
- 带 en-todo 的 div 转为待办；
- 标题、列表保留；
- 代码块、表格、嵌套引用阻断。

**粘贴：** 此前含块级子元素（p/div/ul/ol/h）的 blockquote 会被压成一个引用段落，列表和标题丢失。现在转为同样的容器：
- 引用中的多个段落保持分开，原测试的预期 `quoted<br>second` 相应改为两个引用段落；
- 只含行内文字的 blockquote 仍是一个引用段落。

**测试：**
- `enml_convert::evernote_blockquote_imports_as_a_quote_container`：正向，以及代码块、表格、嵌套引用三种阻断；修改前该用例失败，报 `unsupported block <blockquote>`；
- `document_roundtrip::pasted_blockquote_with_lists_and_headings_becomes_a_quote_container`；
- `ui::tests::pasted_blockquote_with_a_list_lands_as_a_quote_container_and_saves`：挂载 LibraryShell，走剪贴板粘贴流程，保存后数据库中为容器结构；
- 负控：去掉 ENML 分支、关闭粘贴容器判断，对应测试都失败（`/tmp/joplin-claude-quote-paths-negative-controls.{sh,log}`）。

**迁移：** JEX 不经过这两条路径，只读审计仍为 1598/69（`audit-quote-paths.log`）。在 HEAD `5d867fbc8` 上：
- 重新导入：降级 69，blob 全部核对无误（`/tmp/joplin-claude-migration.pLtZsa/import-quote-toggle.log`）；
- 原生往返：1667 篇，`failure_categories={}`（`/tmp/joplin-claude-quote-toggle-native-roundtrip.log`）。

**全量**（HEAD `5d867fbc8`，均 `--offline --locked`，退出码 0）：
- App：1494 通过、0 失败、2 忽略（`/tmp/joplin-claude-batch18-gpui-full.log`）；
- core：33 组共 366 通过、0 失败、1 忽略（`/tmp/joplin-claude-batch18-core-full.log`）。

原 JEX 的 SHA256 仍为 `320f684f…c6cf`。

### 仍未完成或与 Evernote 不同

- Evernote quoteblock 的按键行为（`quoteblock/keymap.ts`）尚未实现：
  - 引用开头按退格解开整段引用；
  - 在引用后的段落开头按退格，把该段并入引用；
  - 在引用内的空段落上按回车或退格，把引用拆成两段；
  - 在引用前的段落末尾按 Delete，把引用的第一块拉出来。

  现在的行为：引用段落开头按退格只让该段离开引用；空列表项按回车变成引用段落。
- 本项目用扁平模型：新包裹的块若紧邻已有引用，会并成一段；Evernote 会生成两个相邻的 quoteblock。
- 旧格式中“段落内含图片”在原生编辑器里本来就拆成“文字块、块级图片、文字块”。在其中一个文字块上包裹时，只包裹该文字块；选区跨过这种独立的块级图片时命令禁用。Evernote 的图片始终在段落内。
- 没有“引用”的快捷键和工具栏图标（Evernote 在 `typebehind.ts` 中有输入 `>` 触发的规则，也未实现）。
- 视觉和实机验收仍需 Codex 完成。

## 第十九批：相邻引用边界（`631c34500`）与引用按键（`2cfa9bbdb`）

接续 evidence67 留下的未提交 core 改动（`BlockStyle.quote_start`），完成原生部分后，拆成两个独立提交。

### 相邻引用边界（`631c34500`）

**问题：** 两个相邻的 quoteblock 会合并成一段引用。Evernote 是树结构，两个 quoteblock 各自独立。

**最小表示：**
- canonical 用 `BlockStyle.quote_start`，原生用 `Block.quote_start`，含义是“此引用块开始一段新的引用，即使前一块也在引用中”；
- 前一块不在引用中时，这个标志一律清除；
- 不引入容器 ID，也不改动整体结构。

**canonical：**
- 一段连续的引用块在 `quote_start` 处拆成多个容器；
- 有多个容器，或任一容器含标题、列表时，逐个写成 `<blockquote data-joplin-lite-quote-container>`；否则保持原来的逐段写法，旧笔记逐字节不变；
- 旧的逐段写法仍视为一段引用（JEX 过去就是这样写多段落引用的）；
- 列表在边界处拆开，也不会跨边界合并；
- 每个容器元素、每个粘贴的 blockquote、每段 Markdown blockquote（CommonMark 中空行会结束 blockquote）都开始一段新引用。

**原生：**
- 标志随撤销、重做、粘贴传递；
- 拆分出的右半块、插入图片后的后段文字不继承标志，所以在引用中输入或回车不会把引用切开；
- 工具栏解开时只处理边界内的那一段；在已有引用旁包裹，会形成独立的一段，并让后面的引用保持独立；
- 有序编号在边界处从 1 重新开始；
- 新一段的竖线下移 4px，对应 ce.css 中 blockquote 的 `margin: var(--spacing-0-5) 0`。

**测试：**
- core：`adjacent_quote_containers_stay_apart`、`separate_markdown_quotes_stay_separate_quotes`；
- gpui：`adjacent_quoted_lists_are_numbered_and_saved_apart`；
- 挂载：`ui::tests::adjacent_quotes_stay_apart_through_toolbar_history_editing_and_save`，覆盖两段引用打开、解开其中一段、Cmd‑Z / Cmd‑Shift‑Z、在引用前后包裹、引用内回车、保存与重开。
- 负控（`/tmp/joplin-claude-quote-boundary-negative-controls.{sh,log}`）：原生导入丢掉边界、解开越过边界、包裹后与前一段合并、编号不在边界重新开始，四种情况下对应测试都失败。

**中间提交独立验证：** 该提交只经索引暂存。在导出的独立目录中运行：
- App 1496 通过、0 失败、2 忽略（`/tmp/joplin-claude-boundary-intermediate-gpui.log`）；
- core 368 通过（`/tmp/joplin-claude-boundary-intermediate-core.log`）。
- 格式化后的暂存版本另行复测，结果相同。

**迁移：** 只读审计 1598/69（`audit-boundary.log`）；新导入降级 69，blob 全部核对无误（`import-boundary.log`）；原生往返 1667 篇，`failure_categories={}`（`/tmp/joplin-claude-boundary-native-roundtrip.log`）。

### 引用按键（`2cfa9bbdb`）

**Evernote 对应**（common-editor `quoteblock/keymap.ts`）：

| Evernote 处理函数 | 源码条件 | 本项目 |
|---|---|---|
| `handleBackspaceAtStartOfQuoteblock`（Backspace，第一顺位） | `$cursor.nodeBefore == null`；直接父节点是 quoteblock（`$cursor.node(-1)`，列表项的父节点是 li，不触发）；是第一个子节点（`$cursor.start(-1) === $cursor.before()`）→ 整段替换为其内容，光标在开头 | 引用段落或引用内标题的 offset 0，且是容器开头 → `SetQuote` 解开整段，光标不动 |
| `handleBackspaceAfterQuoteblock`（第二顺位） | 光标在 `p`（不含标题、列表）开头，前一个兄弟节点是 quoteblock → 该 `p` 成为引用的最后一个子节点 | 不在引用内的普通段落 offset 0，前一块在引用中 → `SetBlockKind(Quote)` 并入，同时清除残留的边界标志 |
| `removeEmptyLineAndSplitQuoteblock`（Enter，以及 Backspace 第三顺位） | 空 `p`；是 quoteblock 的直接子节点；下标不为 0；若是最后一个子节点则只保留前段，否则为“引用 + 空 p + 引用” | 空引用段落且不是容器开头 → `SplitQuoteAt`：变为普通段落；若后面还有引用块，下一块写入边界 |
| `handleDelete` | `$cursor.pos === $cursor.end()`；`$cursor.after()` 处的兄弟节点是 quoteblock → 其第一个子节点并入当前块，其余保留 | 不在引用内的段落或标题在末尾，下一块是容器开头的引用段落或引用内标题 → `JoinQuoteHead` |

**与 Evernote 的差异：**
- 末尾空行：Evernote 会删除这一行，光标落到引用之后；这里改为把这一行变成引用后的空段落，看到的效果相同。
- 引用的第一个子节点是列表时，Evernote 的 Delete 会把一个列表切片并入当前段落；这里不处理这种情况，交给原有的 Delete。
- `Mod-Backspace` 没有单独绑定。

**实现：**
- 新增两个事务 `SplitQuoteAt` 和 `JoinQuoteHead`，各自只产生一步撤销，并更新结构计划、编号范围和替换范围；
- 解开复用 `SetQuote`，并入复用 `SetBlockKind`；
- 只在光标折叠、没有 IME 组合时处理；光标在图片分组中时保持默认行为。

**测试：**
- 挂载：`ui::tests::quote_keys_follow_evernote_quoteblock_keymap`，在 LibraryShell 中真实派发 Backspace、Enter、Delete，以及 Cmd‑Z / Cmd‑Shift‑Z，覆盖：
  - 首块开头退格解开整段，标题与列表保持，光标位置正确；撤销和重做；非首块不解开；
  - 引用后段落开头退格并入引用；撤销；
  - 内部空行回车拆分，在空段输入后按 Home 再退格，又并回前一段，后一段保持独立；
  - 内部空行退格同样拆分；
  - 段末 Delete 把引用首块（标题）的文字并入当前段落，光标位置正确；保存后与数据库一致，重开后一致。
- 原生：`quote_keys_keep_inline_images_and_leave_on_an_empty_last_line`：末尾空行回车离开引用；首块退格解开时，段落内的图片与列表保留，撤销后复原；光标在图片分组的后段时不触发引用规则。
- 负控（`/tmp/joplin-claude-quote-keymap-negative-controls.{sh,log}`）：分别关掉四条规则，对应测试都失败。

**全量（HEAD `2cfa9bbdb`，均 `--offline --locked`，退出码 0）：**
- App：1498 通过、0 失败、2 忽略（`/tmp/joplin-claude-keymap-gpui-full.log`）；
- core：33 组共 368 通过、0 失败、1 忽略（`/tmp/joplin-claude-keymap-core-full.log`）；
- 迁移：新导入降级 69，blob 全部核对无误（`import-keymap.log`）；原生往返 1667 篇，`failure_categories={}`（`/tmp/joplin-claude-keymap-native-roundtrip.log`）；
- 原 JEX 的 SHA256 仍为 `320f684f…c6cf`。

### 单独列出：段落内图片的分组差异

在 Evernote 中，图片始终是段落内的行内节点。本项目原生编辑器有两种表示：
- 引用段落、代码块等用“行内分组”，一组成员属于同一段落，引用切换和按键都把整组作为一个段落处理；
- 旧格式中的普通段落 `<p>文字<img>文字</p>`，打开时拆成“文字块、块级图片、文字块”三个独立的块（`codec.rs` 的 legacy flow，保存时可能写成块级图片）。

因此有以下差异：
- 在这种普通段落的文字处包裹引用时，只包裹那一个文字块；
- 选区跨过其中的块级图片时，引用命令禁用（块级图片不属于 quoteblock 内容）；
- 光标在分组的后段文字开头时，引用按键不触发，保持默认行为（Evernote 中那里不是段落开头）。

要彻底消除这些差异，需要把旧格式普通段落也改为行内分组。这影响图片导入与编辑的通用路径，本批没有改动。

### 边界

没有安装，没有推送，没有改动原资料库和 Codex 的文档。实机验收仍由 Codex 进行。

## 第二十批：图片插入位置（`e6ce2aaf1`）、待办勾选（`a52e4c4d3`）与两处更正

### 更正 1：第十九批中关于 Evernote 图片的说法有误

第十九批“单独列出”一节写了“在 Evernote 中，图片始终是段落内的行内节点”。这与源码相反：

- `paragraph/schema.ts` 213：`p.content = 'inline*'`；
- `resource/schema.ts` 992–1001 与 1037–1046：`image` 和 `file` 节点都是 `group: 'section tablecontent listblockcontent'`、`atom: true`，属于**块级**节点，不能放在 `p` 里；
- `list/schema.ts` 413：`li.content = 'listblockcontent+'`，列表项可以包含块级图片。

因此，普通段落中的“文字块、块级图片、文字块”与 Evernote 一致，并不是差异。标题和引用中的图片分组是 evidence 08 阶段 1 记录的**兼容扩展**：真实 Joplin 迁移数据中有标题内图片 5 篇、引用内图片 2 篇，以分组原位保留，并非 Evernote 的结构。

本轮我一度修改 codec 去掉这些分组，用户和 Codex 指出后，已用 `git checkout HEAD -- codec.rs` 撤回，没有提交，也没有留在差异中。这些分组保持不变。

### 更正 2：`e6ce2aaf1` 的提交说明过度声称

提交说明中写到“段中、末尾无后续、列表、引用、分组、表格沿用原有拆分，在这些情况下与 Evernote 一致或属于兼容路径”。其中列表、引用、选区替换和表格**没有逐项核对**，不能据此判定与 Evernote 一致。下表以本节为准。提交已被 Codex 冻结，所以不改写历史。

### 图片插入位置（`e6ce2aaf1`）

**依据：** Evernote 运行时代码 `resource/resource.ts` 298–412（`insertResourceAtPosition`、`insert`），不只是 schema。在顶层段落或标题中、光标折叠时：
- 空块：资源替换该块（`replaceRangeWith`）；
- 块首：资源插在块之前（`tr.insert($pos.before())`）；
- 块尾：资源插在块之后；
- 块中间：拆开；
- 插入后，光标进入下一个文本块的开头（`!$caret.parent.isTextblock` 分支）；后面没有文本块时才插入一个空段落。

**修复前的缺陷**（用临时诊断测试实测，诊断代码未提交）：
- 段首插图，图片上方多出一个空段落；
- 在空段落中插图，多出两个空段落；
- 段末插图且下一块是标题，多出一个空段落。

**实现：** `insert_beside_top_level_text`。只处理以下条件：图片或附件（不含表格），光标折叠，所在块是不在引用内、不在列表中、不属于分组的顶层段落或标题。光标总是落在插入前就已存在的块中，所以资源提交失败时仍能撤销插入并重放之后的输入。

**已核对一致的情形：**

| 情形 | 测试 |
|---|---|
| 段首 → 图在前，光标留在原段开头，接着输入的文字落在原段 | `image_insert_follows_evernote_resource_placement` |
| 空段后接文本 → 图替换空段，光标进入下一块 | 同上 |
| 空段在最后 → 图在前，保留空段供光标停留（视觉上等同 Evernote 的“替换后补空段”） | 同上 |
| 段末无后续 → 图在后，补一个空段 | 同上（沿用原有拆分，实测与 Evernote 一致） |
| 段中 → 拆开，光标在右段开头 | 同上（沿用原有拆分，实测与 Evernote 一致） |
| 段末后接标题 → 图在后，光标进入标题开头，不补空段 | 同上，以及挂载测试 `mounted_image_at_a_paragraph_end_goes_on_into_the_next_text_block`（真实图片选择器插入、立即显示、续输入、Cmd‑Z 两次 / Cmd‑Shift‑Z 两次、保存、以新仓库重开） |
| 每种情形：撤销、重做 | 同上 |
| 选区跨过图片时删除，图片随文字一起删除 | 同上 |
| 选区含独立块级图片时，引用命令禁用（quoteblock 内容不含 image） | 同上 |

**尚未核对或已知不一致：**
- **列表项内插图：不一致。** Evernote 会把图片放进当前 li，然后调用 `createNewListItemAfterCurrent` 新建一个空 li，光标进入新 li。实测本项目的图片成了两个列表项之间的独立块，不属于任何列表项，保存时列表会被拆成两段。尚未修复。
- **unsplittable 或资源祖先节点**（例如表格单元格、展开的音频等）的分支：未对照。
- **引用内、标题内插入：** 本项目把它们拆成两段并夹着图片；Evernote 的 `tr.insert` 在这些位置的实际拟合结果未核实，不声称一致。
- **非空选区替换：** 实测结果是用图片替换选区，与 `replaceSelectionWith` 的方向一致；但 `insertParagraphIfNeeded` 的细节未逐项对照。
- **表格插入：** 不在本次范围内，未对照。
- **测试调整：** 两项资源提交失败测试依赖“插图后光标在新建右段中”的旧行为。我改为先输入一段文字，在段末插图，使它们仍然覆盖不安全回滚的兜底路径，没有放宽任何断言。
- **负控：** 关掉新分支后，两项新测试都失败（`/tmp/joplin-claude-image-insert-negative-control.{sh,log}`）。

### 待办勾选（`a52e4c4d3`，evidence 69）

**问题：** 在原生编辑器中点击待办方框只会移动光标，数据库里 `checked` 仍为 false。只有旧编辑器（`editor/events.rs` 2097）有取反逻辑。

**Evernote 对应：** `list/plugin.ts` 中的 `handleTodoListMouseEvent`（约 860–890）和 `handleViewModeClickOn`（约 380–400）。点击落在 `.list-bullet-todo-container` 上时执行 `setNodeMarkup` 翻转 `checked`，保留选区；点击文字照常放置光标。

**实现：**
- `EditorCore::check_marker_at`：判断点击是否落在可见待办项（不含分组后续段）第一行的列表标记区域内，这正是 render.rs 绘制方框的位置；
- Surface 在放置光标之前先处理这次左键点击；
- `Transaction::ToggleCheck` 只产生一步撤销，保留选区，并加入分组更新，使带图片的待办项也能导出新状态。

**测试：** `ui::tests::clicking_a_checklist_box_ticks_it_undoably_and_saves`，在挂载的 Library 中依次验证：
- 点方框勾选，光标不动；
- 点文字不勾选；
- Cmd‑Z / Cmd‑Shift‑Z；
- 保存后数据库为 `data-checked="true"`；
- 重开后仍为勾选；
- 再点一次取消勾选。

**负控：** 去掉点击处理后，测试失败的表现正是 evidence 69 的症状：`[false, false]`（`/tmp/joplin-claude-checklist-negative-control.{sh,log}`）。

**仍需复核（不扩大范围）：**
- 待办文字居中或右对齐时，方框仍画在左侧；这需要对照 Evernote 的视觉规范，留作保真复核项；
- 输入法组合进行中点击方框：Evernote 对此有专门处理（PESO‑2349），这里没有单独实现。

### 保留的已知差异（重申）

- 引用的第一块是列表时按 Delete：Evernote 会把列表切片并入当前段，本项目交给普通 Delete 处理；
- `Mod-Backspace` 没有单独绑定 quoteblock 的处理函数。

### 全量（HEAD `a52e4c4d3`，均 `--offline --locked`，退出码 0）

- App：1501 通过、0 失败、2 忽略（`/tmp/joplin-claude-batch20-gpui-full.log`）；
- core：33 组共 368 通过、0 失败、1 忽略（`/tmp/joplin-claude-batch20-core-full.log`）。

没有安装，没有推送，没有改动原资料库，也没有碰 Codex 正在使用的隔离候选包和资料库。

## 第二十一批：列表项内插图（`685a05109`）

### Evernote 对应

源码根目录：`evernote-11.32.5/common-editor-sourcemap/@evernote/common-editor/src/apps/peso/modules/`。

- `resource/resource.ts` 389–398：`insert` 先用 `insertResourceAtPosition`（298–342）放置资源。如果 `inList($caret)` 为真（`list/list.ts` 153，向上查任一层是否为列表），就找到外层 `li`，在它之后调用 `createNewListItemAfterCurrent(tr, schema, liNode, insertPos, p.create())`，光标进入新段。
- `list/li.ts` 166–181：新 li 用 `li.create(null, content)` 创建，属性全部取默认值。`liAttrs`（`list/schema.ts` 50–）中 `checked` 默认为 `null`，即未勾选。
- `list/schema.ts` 413：`li.content = 'listblockcontent+'`。`resource/schema.ts` 886 中 image 的 group 含 `listblockcontent`，所以图片和拆开的两段都留在原 li 内。列表本身不属于 `listblockcontent`；`ul`/`ol`/`todolist` 的内容是 `(li | todolist | ol | ul)+`（359/377/395），嵌套列表是与 li 并列的兄弟节点，因此新 li 紧跟当前 li，排在它的嵌套子列表之前。

### 实现

在 `apply_insert_structural` 中：当插入点是普通顶层列表项（不是已有分组的成员）且插入的不是表格时，改走 `insert_into_list_item`：
- 原项成为分组 `[左段, 图片, 右段?]`，右段为空时省略；
- 紧随其后插入一个同类型、同缩进的空项（待办为 `checked: false`）；
- 光标位于新项开头；
- 逆操作批次为 `RestoreInlineGroups` 加 `RestoreBlocks`，撤销一步即可恢复原项。

### 已核对一致的情形

| 情形 | 保存的 HTML（`img` 为 `<img src=":/…" alt="">`，`空` 为空项标记） | 测试 |
|---|---|---|
| UL 项末尾 | `<ul><li>一{img}</li><li>{空}</li><li>二</li></ul>` | `list_item_takes_media_and_a_new_item_follows_like_evernote` |
| OL 项中间 | `<ol><li>甲{img}乙</li><li>{空}</li></ol>` | 同上 |
| 已勾选待办的开头 | `<li data-checked="true">{img}甲</li><li data-checked="false">{空}</li>` | 同上 |
| 嵌套项 | 新项带同样的 `data-indent="1"` | 同上 |
| 空项 | `<li>{img}</li><li>{空}</li>` | 同上 |
| 选区“甲[乙]丙” | `<ol><li>甲{img}丙</li><li>{空}</li></ol>` | 同上 |

每种情形都检查以下几点：
- 重开后形状不变；
- 接着输入的文字落在新项中；
- 布局顺序与文档顺序一致；
- 撤销两次恢复原文；
- 重做恢复插入后的结果。

挂载测试 `mounted_list_item_image_stays_in_its_item_and_rolls_back_safely`，用真实图片选择器插入，覆盖三种情况：
1. **立即显示与持久化：** 图片立即显示（布局中有图片块，缓存中有资源）。输入“续”后保存为 `<ul><li>一<img src=":/{id}" alt=""></li><li>续</li><li>二</li></ul>`，用新的 `LibraryRepository` 重开结果相同。
2. **提交失败、之后无输入：** 图片消失，保存后恢复为原列表。
3. **提交失败、之前已在新项中输入：** 图片和文字都保留，显示“手动同步可重试”。原因是撤销插入后，在已不存在的新项上重放输入会失败，于是转入现有的 fail-closed 路径。这是有意保留的安全行为，并非静默丢失。

### 负控

在隔离副本中把分支条件改为 `false` 后，两项新测试都失败（`/tmp/joplin-claude-list-image-negative-control.{sh,log}`）。

### 剩余差异（未改，不扩大范围）

- **已含图片的列表项（分组项）不新建 li。** 实测在 `<li>一<img a>后</li>` 的“后”末尾插图，得到 `<li>一<img a>后<img b></li><li>二</li>`：图片仍在 li 内，但没有像 `createNewListItemAfterCurrent` 那样新建 li。这条走的是 evidence08 阶段 1 的兼容分组路径，本批没有改动。
- **新项的属性。** Evernote 用 `li.create(null)` 和 `p.create()` 创建新项，对齐方式和 `checked` 都取默认值（`checked: null`，导出时不写该属性）。本项目的新项沿用右段的对齐和引用标记，待办导出为 `data-checked="false"`。视觉上，只有居中或右对齐的列表项才会有差别。
- **`StructuralPlan` 计数（观察项）。** `InsertImage` 的 plan 固定假设插入 3 块，而列表分支插入 4 块（或者右段为空时插入 3 块且结构不同）。诊断确认重新整形后布局顺序正确，所以没有改。
- **仍未对照的分支：** unsplittable 或资源祖先节点（resource.ts 343、365–375）、引用和标题内的插入、表格插入。
- **沿用上一批的已知差异：** 引用中以列表开头时按 Delete 的行为、`Mod-Backspace`、待办文字居中时方框仍在左侧、输入法组合期间点击方框。

### 全量（工作区等同 `685a05109`，均 `--offline --locked`，退出码 0）

- App：1503 通过、0 失败、2 忽略（`/tmp/joplin-claude-list-image-gpui-full.log`）；
- core：368 通过、0 失败（`/tmp/joplin-claude-list-image-core-full.log`）。

rustfmt 只处理了本批新增的代码；与 HEAD 相比，格式差异为 0。没有安装，没有推送，没有改动原资料库，也没有碰 Codex 的证据文档和 `replica-delivery-status.md`。

## 第二十二批：已含图片的列表项再插图、新项默认属性、StructuralPlan 计数（`be5eafa99`）

这一批处理第二十一批报告中的三项剩余差异。产品整体并未完成，见文末的剩余项。

### Evernote 对应

源码根目录：`evernote-11.32.5/common-editor-sourcemap/@evernote/common-editor/src/apps/peso/modules/`。

- `resource/resource.ts` 389–398：只要 `inList($caret)` 为真，就调用 `createNewListItemAfterCurrent`。是否执行不取决于 li 里是否已有资源，也不区分图片和文件。image 与 file 节点同属 `listblockcontent`（`resource/schema.ts` 886、1001、1046）。
- `list/li.ts` 166–181：新 li 由 `li.create(null, p.create())` 创建，li 和 p 的属性都取默认值。`liAttrs.style` 与 `checked` 默认为 `null`（`list/schema.ts` 50–）。对齐方式存放在 p 的 `textAlign` 上（`paragraph/schema.ts` 233–237），新建的 p 没有对齐。
- 新 li 插在整个当前 li 之后（`insertPos = $liNodePos.pos + liNode.nodeSize`），也就是本项目中整个分组的最后一个成员之后。

### 实现

- **分组项（`follow_media_with_list_item`）**：在分组更新包装层中，如果插入的是图片或附件，并且受影响的分组是列表项，就在包装层算出新的分组之后：
  - 如果拆分在分组末尾留下了空的新文字段，并且光标就在这里，就把它移出分组，作为新项；
  - 否则在分组最后一个成员之后插入一个新的空项，作为单独的拼接发布，并在逆操作批次的最前面加一条删除它的 `RestoreBlocks`；
  - 光标移到新项开头。新项的 ID 在执行原始事务之前就预留好，原始事务执行后不会再因为分配失败而产生半完成状态。
- **新项默认属性**：不论是普通项还是分组项，新项都为左对齐；待办为未勾选。引用容器内的列表保持 `quoted`，因为新 li 仍在同一个 quoteblock 里。
- **StructuralPlan**：媒体插入不再固定按 3 块计算，改为按提交后的块数差值得出 `inserted_count`（`inserted_from_delta`）。开头说明中提到的“现有 atom 旁插入计 1”这条路径，实际只插入 1 块，所以保持不变。分组项额外插入的新项另外发布一条拼接 `(index, 0, 1)`。

### 测试（先写，在实现前确认失败）

| 测试 | 覆盖内容 |
|---|---|
| `grouped_list_item_takes_more_media_and_a_new_item_follows` | 8 个用例：<br>• UL 分组项末尾 → `<li>一{a}后{b}</li><li>{空}</li><li>二</li>`<br>• OL 分组项首段中间 → `<li>甲{b}乙{a}丙</li><li>{空}</li>`<br>• 已勾选待办中、光标在图片之后 → `…甲{a}{b}</li><li data-checked="false">{空}</li>`<br>• 嵌套分组项开头（新项 `data-indent="1"`，排在下一项“二”之前）<br>• 图片后的段首（第二图路径）→ `一{a}{b}后`<br>• 分组内选区替换<br>• 引用容器内的分组列表项<br>• 引用容器内的普通列表项<br>每个用例都检查：导出、重开、输入落到新项、布局顺序、有序编号、一步撤销、重做 |
| `new_list_item_after_media_takes_default_alignment` | 居中的项（普通项末尾、普通项中间、分组项）插图后，新项不带 `data-align` |
| `heading_and_quote_media_groups_keep_their_compatible_shape` | 断言式兼容回归，取代临时诊断：`<h2>` 和旧式引用块 `data-joplin-lite-block-quote` 中含图的分组插图后，图片仍留在分组内，块数只多 2（不新建项），撤销后原样恢复。改动前后都通过 |
| `list_media_inserts_publish_exact_splices_in_a_large_list` | 详见下文“大列表” |
| 挂载测试 `mounted_grouped_list_item_takes_another_image_and_fails_closed` | 通过真实图片选择器操作：<br>• 先插一张图并保存，使该项成为分组；再插第二张图，图片立即显示（`cache_has_resource`）；输入“再”后保存为 `<ul><li>一<img b><img a></li><li>再</li><li>续</li><li>二</li></ul>`，用新仓库重开结果相同；<br>• 提交失败且之后无输入：保存结果与插图前的分组 HTML 逐字相同，没有被拍平；<br>• 在新项中输入后提交失败：文字保留，显示“手动同步可重试” |
| 已有测试 `attachment_inserted_inside_an_image_list_item_saves_in_place_and_undoes`（改了期望值，没有删除） | 按 resource.ts，附件同样会新建 li。期望值从“以 `后</li></ul>` 结尾”改为完整的精确 canonical：`<ul><li>图前<img …>图<a data-joplin-lite-inline-attachment…>合同.pdf</a>后</li><li>{空}</li></ul>`。图片、附件、文字都保留；重开和撤销的断言没有改动 |

### 大列表：拼接计数与增量成本

测试会依次执行以下步骤：
1. 在普通项中插图；
2. 在刚形成的分组项中间再插图；
3. 撤销两次；
4. 重做两次。

每一步都做四项检查：
- 把发布的拼接逐条重放到一份镜像 ID 序列上，结果必须与文档顺序完全一致，`removed` 也必须逐项对上；
- 增量布局的总高度与全新布局一致；
- 有序编号与参照实现一致；
- 精确的拼接形状：普通项为 `[(m, 1, 4)]`，分组项为 `[(m+2, 1, 3), (m+5, 0, 1)]`。

实测的布局工作量（`/tmp/joplin-claude-batch22-large-list-counts.log`），取每一步中的最大值：

| 列表 | 规模 | 高度索引 | 编号 |
|---|---|---|---|
| 无序 | 10k | 11 | 192 |
| 无序 | 100k | 11 | 192 |
| 有序 | 10k | 11 | 5078 |
| 有序 | 100k | 11 | 50086 |
| 有序，回车拆分基线 | 10k / 100k | — | 5073 / 50081 |

如实说明：
- **高度索引**和**无序列表的编号**不随规模增长。
- **有序列表的编号**随规模线性增长。原因是在中间插入一项后，后面每一项的序号都会变，这是原有的重新编号行为。普通回车拆分的成本完全相同（每种规模下只差 5）。断言只要求插图不超过同规模回车基线加 64，**不声称**整个操作是常数成本。
- 原有测试 `ordered_tail_edit_has_bounded_numbering_scratch_and_keeps_marker` 只覆盖尾部编辑。

### 负控（隔离副本，`/tmp/joplin-claude-batch22-negative-controls.{sh,log}`）

- **A：去掉分组项的新 li。** 分组、默认对齐、大列表、挂载四项测试失败；兼容测试仍通过。
- **B：恢复固定计数 3。** 大列表测试在第一步就失败（重放拼接无法还原文档顺序）；分组项测试也失败。
- **C：新项继承居中。** 默认对齐测试失败。

### 全量（工作区等同 `be5eafa99`，均 `--offline --locked`，退出码 0）

- App：1508 通过、0 失败、2 忽略（`/tmp/joplin-claude-batch22-gpui-full.log`）；
- core：368 通过、0 失败（`/tmp/joplin-claude-batch22-core-full.log`）。

rustfmt 只处理了本批改动；与 HEAD 相比，格式差异为 0。

### 剩余差异（未改动，不扩大范围）

- **新待办的 checked 属性**：Evernote 新 li 的 `checked` 为 `null`，导出时不写该属性；本模型只能用布尔值表示，所以导出为 `data-checked="false"`。显示效果相同。
- **标题和旧式引用块中的插图**：保留兼容分组形状。Evernote 中 image 不能放进 h 或 p，会在拟合时被移出；这里不改，以免对现有 canonical 笔记做破坏性转换。
- **跨多个分组的选区替换**：会在含新媒体的第一个列表分组之后新建项，没有单独与 Evernote 对照。
- **列表新项上的失败回滚**：仍依赖 fail-closed 的“手动同步可重试”路径，不会悄悄丢失内容，但也不会自动重放。
- **仍未对照的分支**：unsplittable 或资源祖先节点（resource.ts 343、365–375）、表格插入（下一份合同 evidence 72 处理表格单元格内插图）。
- **沿用之前的差异**：引用中以列表开头时的 Delete、`Mod-Backspace`、待办居中时方框位置、输入法组合期间点击方框。
- 产品整体尚未完成：迁移视觉验收、多模态、NAS 常驻、内存峰值、最终安装包等关卡都还没有进行。

没有安装，没有推送，没有改动原资料库，也没有碰 Codex 的证据文档和 `replica-delivery-status.md`。

## 第二十三批：表格单元格粘贴图片（evidence 72，`d98fb960c`）

本批实现 evidence 72 中“聚焦单元格粘贴图片”这一项，**不是完整的表格资源复刻**。剩余合同见文末。

### Evernote 对应

源码根目录：`evernote-11.32.5/common-editor-sourcemap/@evernote/common-editor/src/apps/peso/modules/`。

- `table/schema.ts` 219–226：th/td 的内容为 `tablecontent+`，并且是 isolating、unsplittable。
- `resource/schema.ts` 992–1004：image 是 atom，group 含 `tablecontent`，所以单元格可以直接容纳图片。
- `resource/resource.ts`：
  - 295–296：`validResourceInsertPos` 只要求父节点能容纳资源（td 能容纳 image），unsplittable 不影响单元格内的 p。
  - 298–341：`insertResourceAtPosition` 的分支照常适用于单元格内的 p：空段替换、段首插在前、段尾插在后、段中拆分。
  - 377–387：选区非空时用 `replaceSelectionWith` 替换选区。
  - 400–406：插入后光标进入下一个文本块，没有时补一个空 p。
  - 365–375：多单元格选区（CellSelection）、unsplittable 祖先节点的分支，**本批未实现**，见剩余项。

结论：单元格内插图后，td 的内容是 `[p一, image, p后]`。本项目单元格 canonical 用 `<br>` 分隔段落（`table_cell_inlines`），所以保存为 `<td>一<br><img …><br>后</td>`；在段尾插图时，用于放置光标的空 p 保存为结尾的 `<br>`。

### 实现（沿用原有资源链，不改粘为正文）

**意图捕获**（`paste_into_table_cell`）：图像、候选图像、编码图像、单个文件、多个候选文件，都在单元格编辑器中捕获锚点，生成 `InsertIntent { cell: CellInsertTarget }`，然后交给原有的 `complete_resource_request`。所以写入方栅栏、排队、暂存 worker、只读检查都与正文相同。

**单元格身份**（Codex 审查后补充）：
- `CellInsertTarget` 同时持有三样东西：编辑器的弱引用；一个 `open` 标志，在 `TableCellEditor` 被丢弃时清零，所以即使有人仍持有旧的实体也能识别；表格位置（节点、行、列）。
- 在写入任何内容之前，先校验正文中该单元格的 inline 与单元格编辑器的导出一致。这可以拦住“上方插入或删除了行，坐标仍然有效，但指向了另一格”的情况。

**插入**（`prepare_cell_resource_insert`）：
1. 先把资源加入会话的允许列表，再修改单元格。这样壳层观察者同步单元格草稿时，已经允许这个资源。
2. 单元格编辑器执行 `InsertImage`，在单元格历史中只占一步撤销，并以 `register_unavailable_image` 立即显示占位。
3. 用新的单元格内容构造正文的 `ReplaceTable` 预备提交，资源快照携带的就是这份正文。
4. 任一步失败，都回滚单元格并恢复允许列表。

**成功后**：正文和单元格编辑器都注册已物化的图片。

**失败时**（`rollback_cell_resource`）：
- 先对单元格执行原有的乐观回滚，会重放之后的输入。
- 回滚成功后，同步回正文表格，并调用 `forget_table_resource`，从正文历史中**所有**表格快照里剔除这张图。快照有两种形式：`ReplaceTable` 本身，以及它的逆操作——携带整个表格块的 `RestoreBlocks`。这样正文中逐步撤销或重做，都不会把未入库的图片带回来。
- 如果之后的输入无法重放（输入发生在插入时新建的段里），就保留暂存的资源和单元格目标，显示“手动同步可重试”。重试成功后，正文和单元格都会显示这张图。

**其他**：
- 正文回滚和重试判断资源是否仍在文档中时，改用 `references_resource`，会检查表格单元格。原先只看块级内容，会把表格中的图片误判为“已删除”。
- **同库 fragment**：沿用正文的同库校验，同库资源按 sha256 和已校验文件确认，跨库资源用导出的文件导入。抽取为 `fragment_resources`。既不在本库、复制时也没有导出文件的资源不会粘贴（不会只复制 ID），并给出提示。
- **单元格打开**：`table_cell_blocks` 是 `table_cell_inlines` 的精确反函数：独占一段的图片还原为图片块，其余部分还原为段落。这样打开再保存不会改变内容。修复前，重开后换行会翻倍。
- **单元格图片缓存**：改为单元格独有，预算 16 MiB。
- **表格持久化加载**：跳过正在提交，以及已失败、等待重试的资源。

### 挂起问题（Codex 采样 `/tmp/joplin-cell-test-spin-oct1.sample.txt`）

- **现象**：第一次运行单元格测试时，测试进程空转，CPU 约 395%，持续 7 分钟以上。该次运行由我在后台启动，管道过滤后只留下 `/tmp/joplin-claude-cell-image-spin-run1.log`，里面没有测试输出，这是我的失误。之后改为保留完整日志，并给每个用例加 60 秒闹钟。在单元格仍共用缓存的版本上，`image_pasted_into_…` 和原有的 `cmd_v_in_a_clicked_cell_…` 两项都以 `exit=142`（闹钟）终止，挂起复现。
- **根因**：正文和单元格编辑器的表面共用一个 `BudgetedImageCache`。每次绘制，`set_visible_resources` 会**整体替换**可见集合，并按各自的尺寸请求解码边长；随后 `evict_offscreen` 逐出“对自己不可见”的条目。同一张图在正文表格和单元格中同时显示时，两个表面轮流逐出对方的条目、轮流改边长，于是不停地重新加载、notify、重绘。
- **修复**：单元格编辑器使用独立的缓存实体。负控 E（改回共享缓存）在 60 秒时再次被闹钟终止（`exit=142`）。

### 测试（`ui/table_cell_editor_tests.rs`，挂载测试，真实 Cmd‑V 剪贴板）

| 测试 | 覆盖内容 |
|---|---|
| `image_pasted_into_a_cell_is_stored_and_shows_in_that_cell` | 不进入正文队列；图片在单元格编辑器中，也在正文表格中；接着输入“后”，按 Tab 换格；正文表格绘制出已加载像素的这张图；保存为 `<td>一<br><img …><br>后</td>`；全文只有 1 张图，表格之前的正文不变；资源元数据和已校验文件都存在；用新仓库和 `NoteSession::prepare` 能重开；再次打开并提交单元格，内容逐字不变 |
| `cmd_v_in_a_clicked_cell_pastes_into_that_cell_not_the_body`（原有测试，改了期望值，未删除） | 原测试断言拒绝图片，现在改为断言图片插入该单元格：`一粘贴丙<strong>粗</strong><br><img …><br>`；文字、fragment 粘贴和 Tab 换格的断言保持不变 |
| `failed_cell_image_commit_takes_the_image_out_of_cell_table_and_history` | 提交失败，之后没有输入：单元格和正文表格都恢复为“一”；失败提示可见；正文中**每一步**撤销和重做都不会引用这张图；保存结果中没有图片 |
| `failed_cell_image_commit_after_typing_keeps_both_and_retries` | 插图后输入“后”再失败：单元格和表格中的内容一致，都保留图片和“后”；提示“手动同步可重试”；手动同步后保存为 `<td>一<br><img …><br>后</td>`；单元格编辑器中这张图已物化 |
| `cell_image_undo_redo_and_exact_selection` | 选中“甲乙丙”中的“乙”后粘贴，图片替换选区；之后输入；Cmd‑Z 两次，单元格和表格都回到“甲乙丙”；Cmd‑Shift‑Z 两次后保存为 `<td>甲<br><img …><br>后丙</td>` |
| `cell_image_arriving_after_the_cell_closed_lands_nowhere` | 暂存期间按 Escape 关闭单元格，测试**继续持有旧实体**，然后用新实体重开同一格；图片完成后，新旧两个实体都没有图片，提示可见，正文没有图片 |
| `cell_image_refuses_a_moved_cell_a_switched_note_and_open_composition` | 三个场景：<br>• 单元格保持打开时在上方插入一行：拒绝，提示“单元格已移动或改变”；<br>• 回调到达时单元格中有未确认的输入法组合：拒绝，提示“输入法组合文本尚未确认”；<br>• 暂存期间切换笔记：切换会等待进行中的资源（与正文相同），图片落在原笔记的原单元格，另一篇笔记不受影响 |
| `tiff_pasted_into_a_cell_is_stored_as_an_image` | TIFF 剪贴板图像保存为图片 |
| `fragment_image_from_this_library_pastes_into_a_cell` | 同库 fragment 的图片进入单元格，保存为 `甲图<br><img …>`；只有 ID、没有文件的外库资源不粘贴，提示“1 个图片或附件未粘贴” |

### 负控（隔离副本，`/tmp/joplin-claude-batch23-negative-controls.{sh,log}`，每个用例有 60 秒闹钟）

| 控制 | 改动 | 结果 |
|---|---|---|
| A | 拒绝单元格插图 | 插图、TIFF、撤销与选区三项失败 |
| B | 忽略 open 标志 | 持有旧实体的迟到测试失败 |
| C | 去掉血统校验 | 移动单元格测试失败 |
| D | 不跳过正在提交的资源 | 两项失败测试的提示被持久化加载警告覆盖，失败 |
| E | 共享图片缓存 | 闹钟终止，挂起 |
| F | 单元格按单个段落打开 | 重开后换行翻倍，失败 |
| G | 历史不剔除资源 | 第一次运行时测试没有抓到（只检查了撤销到底和重做到底两个端点）。改为逐步检查后，测试抓到了一个**真实残留**：`ReplaceTable` 的逆操作 `RestoreBlocks` 中仍带着这张图，记录为 `[false, true, false]`。修复后单独重跑控制 G，失败（`/tmp/joplin-claude-batch23-negative-control-G.log`） |

### 全量（工作区等同 `d98fb960c`，均 `--offline --locked`，App 加 20 分钟上限，退出码 0）

- App：1516 通过、0 失败、2 忽略，16 秒（`/tmp/joplin-claude-batch23-gpui-full.log`）；
- core：368 通过、0 失败（`/tmp/joplin-claude-batch23-core-full.log`）。

改动的文件中没有新增编译警告（与第二十二批日志逐条对比）。rustfmt 只处理了本批的格式块：`core.rs` 在 HEAD 没有格式差异，所以整文件格式化；其余三个文件只应用与本批改动行相交的格式块。

### 剩余合同（明确未完成，不称完整表格复刻）

- **多单元格选区**（resource.ts 365–375，`isCellSelection` 多格）：本项目的单元格编辑器只编辑一格，没有对应路径。
- **表格附件块**：单元格中粘贴非图片文件，会明确拒绝并提示“单元格中只能插入图片；附件请插入正文”，已暂存的字节不入库。单元格内的附件卡片尚未实现。
- **列表、引用等复杂单元格结构**：未实现。
- **网页 HTML 中的图片粘贴到单元格**：仍明确拒绝，提示“单元格暂不能粘贴网页中的图片”。正文的远程图片抓取链没有接到单元格。
- **工具栏或菜单“插入图片”、Finder 拖放到单元格**：仍走正文意图，没有接到单元格。单元格打开时用工具栏插图，图片会进入正文的光标位置。这条需要单独处理。
- **`PasteIntent::EncodedImage`**：与其他图像共用同一个分派分支，但没有单独的挂载测试。TIFF 测试经由 gpui 剪贴板，走的是 Image 路径。
- **跨库但带导出文件的 fragment**：沿用正文的导入，单元格中未单独测试。
- **单元格已关闭时提交失败**：此时图片只在正文表格里，按正文的 fail-closed 规则保留暂存，可手动重试。这条路径没有单独测试。
- **重开同一格得到新实体时**：迟到的图片按设计拒绝，不写入新实体。这是保守选择，需要用户重新粘贴。
- **保存形状**：单元格中的段落用 `<br>` 表示，没有使用 Evernote ENML 的 `<div>`/`<en-media>` 结构。这沿用现有的单元格 canonical 约定，没有改变。
- **单元格图片缓存**：每个打开的单元格额外占用最多 16 MiB 解码预算，单元格关闭时释放。
- **实机验收**：尚未进行。Codex 报告的 CUA Transport closed 问题仍然存在。

没有安装，没有推送，没有改动原资料库、服务器或已安装的 App，也没有碰 Codex 的证据文档和 `replica-delivery-status.md`。

## 第二十四批：选择器与拖放路由到单元格、单元格血统（evidence 76，`5891fe44b`）

本批实现 evidence 76 的第 1–4 项。第 5 项“原生选择器能否真正显示”是独立关卡，本批只做了只读诊断，**不声称选择器原生通过**。

### Evernote 对应

源码根目录：`evernote-11.32.5/common-editor-sourcemap/@evernote/common-editor/src/apps/peso/modules/`。

- `resource/resource.ts` 345–387：
  - 有坐标时（`caretLocationPluginKey` 的 coords，经 `posAtCoords` 换算），在坐标处插入；
  - 否则用当前选区；
  - 多单元格选区和不可分割节点另有分支（365–375，本项目没有对应路径）。
- `dragdrop/plugin.ts`：
  - 444–447：落点由 `posAtCoords(eventCoords(event))` 决定；
  - 518–521：落在表格内时，节点直接 `replaceRangeWith` 进该单元格。
- `resource/schema.ts` 992–1004 和 `table/schema.ts` 219–226：见第二十三批。

### 实现与对应

| 入口 | Evernote 行为 | 本项目实现 | 测试 |
|---|---|---|---|
| 图片按钮、菜单、选择器 | 在当前选区处插入 | `begin_resource_picker`：单元格在上次绘制时持有键盘焦点，就捕获单元格光标；否则捕获正文光标。再次打开选择器时，丢弃先前未用的锚点。原先的锚点被覆盖后会遗留在登记表中 | `picker_from_a_focused_cell_inserts_into_that_cell`（取消后无变化；打开两次时旧令牌失效、新令牌插入单元格；保存后表格之前的正文不变，全文只有 1 张图） |
| 选择器完成前单元格已关闭 | — | 沿用 open 标志拒绝，提示可见 | `picker_completing_after_its_cell_closed_is_refused` |
| 拖放到已打开的单元格 | 以坐标为准，放进该格 | 记录单元格编辑区的绘制边界（不建 hitbox 的 prepaint canvas）。落点在边界内时，取单元格编辑器在该点的位置；面板本身也接收拖放 | `drop_into_the_open_cell_goes_there_and_on_a_closed_cell_is_refused` |
| 拖放到未打开的单元格 | 放进该格 | 本项目必须先打开单元格才能编辑其内容，所以拖到正文表格上时记录拒绝原因，松手时拒绝并提示“请先双击打开该单元格”。**修复前**，松手会退回正文光标插入，属于静默降级 | 同上（`record_drop_position` 后 `take_drop_intent` 为错误） |
| 其他拖放 | 坐标处 | 沿用正文的落点逻辑 | 原有正文拖放测试 |

**单元格血统**（evidence 74、76 第 3 项）：
- **旧做法的反例**：原先用“同一坐标下单元格内容相等”作为血统。在全是空单元格的表格中，于目标上方插入一行，坐标 (2,1) 指向了另一个同样为空的格，图片因此被插进错误的行。这是红灯阶段 `cell_image_…identical_cells` 实测到的情况。
- **新做法**：`TableCellEditor` 持有 `table_revision`，即表格块上次由本单元格写入或读取时的修订号。插入前比较正文表格块当前的修订号，不一致就拒绝，提示“单元格已移动或改变”。内容相等的检查保留，但只作为一致性校验，不再当作血统。
- **修订号会前进的三种情况**都是本单元格自己的修改：本单元格的同步（`persist_table_cell_draft`）、插图安装后的正文表格、失败回滚后的同步。所以单元格内的普通输入不会导致误拒（测试中在暂存期间输入“字”，图片照常插入）。
- **同一规则也用于单元格文字的同步**：表格已在别处改变时，单元格文字不写入可能已错位的格，单元格显示错误，用户按 Esc 放弃后重新打开即可（`cell_text_is_not_written_into_a_cell_that_moved_under_it`）。

**合同第 4 项的明确测试**：
- `encoded_image_and_foreign_fragment_with_its_file_paste_into_a_cell`：
  - data URI 图片经 `classify_clipboard` 成为 `EncodedImage`，进入单元格；
  - 外库 fragment 附带导出的文件，sha256 和大小都匹配，以本库的新 ID 导入单元格，原外库 ID 不出现在正文中，资源元数据存在。
- `failed_commit_after_the_cell_closed_keeps_the_table_image_for_retry`：点“完成”关闭单元格后提交失败。图片只留在正文表格中，提示“手动同步可重试”；重试后图片保存进正文，资源元数据存在。

### 红灯与负控

**红灯**（实现前，`/tmp/joplin-claude-batch24-red/`）：
- 失败的四项：选择器插入单元格、单元格关闭后的选择器、内容相同的结构变动、拖放进单元格。
- 实现前已通过的三项：编码图像、外库带文件的 fragment、单元格关闭后提交失败。这三项记录的是现有行为，作为明确回归保留。

**负控**（隔离副本，每个用例 60 秒闹钟）：

| 控制 | 改动 | 结果 |
|---|---|---|
| A | 选择器忽略单元格焦点 | 两项选择器测试失败 |
| B | 去掉修订号血统 | 内容相同的结构变动测试失败 |
| C | 拖放不识别单元格边界 | 拖放测试失败 |
| D | 拖到表格不拒绝 | 拖放测试失败 |
| E | 单元格同步不校验修订号 | 错位单元格写入测试失败 |
| F | 松手时忽略拒绝原因 | 拖放测试失败 |

日志：A–E 在 `/tmp/joplin-claude-batch24-negative-controls.{sh,log}`，F 在 `/tmp/joplin-claude-batch24-negative-control-F.{sh,log}`。

### 全量（工作区等同 `5891fe44b`，`--offline --locked`，20 分钟上限，退出码 0）

- App：1523 通过、0 失败、2 忽略，15 秒（`/tmp/joplin-claude-batch24-gpui-full.log`）；
- core：368 通过、0 失败（`/tmp/joplin-claude-batch24-core-full.log`；本批没有改动 core）。

改动的文件中没有新增警告（与第二十三批日志对比）。rustfmt 只应用与本批改动行相交的格式块。

### 原生选择器：只读诊断（不是修复，也不是通过）

- **产品分派已到达 AppKit**：PID 42086 在 23:40:26 的断言调用栈中，`+[NSSavePanel _createPanel]` → `-[NSOpenPanel init…]` → `-[NSSavePanel _initBridgeAndStuff]`，发起帧是 joplin-lite 主队列任务，也就是 GPUI `prompt_for_paths` 在前台执行器中调用 `NSOpenPanel::openPanel`，再 `beginWithCompletionHandler`。因此按钮分派并没有缺失。
- **断言前的系统日志**：面板服务 `com.apple.appkit.xpc.openAndSavePanelService` 在 23:40:24 记录了 `_LSBundleCreateNode … returned -43`（找不到文件）；之后客户端记录 “Advance to configuration phase semaphore timed out”，接着才是断言。
- **LaunchServices 登记**（`lsregister -dump`，只读）：临时验收身份 `com.arielkevin.joplinlite.acceptance.oct1` 和 `…acceptance.media1001` 都带有 `in-temp-dir` 标志。打包脚本没有沙盒 entitlements，签名是 ad-hoc。
- **推测，尚待 Codex 实机区分**：面板服务需要通过 LaunchServices 找到客户端 bundle。直接执行 `Contents/MacOS/joplin-lite`、bundle 位于临时目录、或者改过身份后重签，都可能让这一步查找失败。Codex 正在验证经 LaunchServices 启动的情形；最新签名包直接执行时同样会断言。
- 没有修改 GPUI 或打包脚本。cfg(test) 的选择器接缝不能证明 NSOpenPanel 能显示。

### 剩余合同（明确未完成）

- **原生选择器**：未通过，根因待实机区分（见上）。
- **拖放到未打开的单元格**：会拒绝并提示，而 Evernote 会直接放进该格。本项目要先打开单元格，并且需要拿到落点在单元格文字中的偏移，才能做到，尚未实现。
- **落在已打开单元格内的文字偏移**：取自单元格编辑器的 `point_from_layout`。测试中，单元格编辑器布局块的 y 坐标（约 174）与面板实际绘制位置（约 320–480）不一致，所以光标的精确落点尚未单独核实；只核实了图片进入了该单元格。这种坐标差异也可能影响单元格内的点击定位，需要单独排查。
- **焦点判断**：取自单元格上一次绘制时的焦点状态。焦点变化后如果尚未重绘就触发菜单，可能按旧焦点路由。
- **表格在别处改变后**：单元格只给出错误、拒绝写入，不会自动重新定位到原来的格。
- **沿用之前的缺项**：多单元格选区、表格附件、复杂单元格块、网页图片粘贴到单元格、解码缓存的实机测量。

没有安装，没有推送，没有改动原资料库、服务器、默认 profile 或已安装的 App，也没有碰 Codex 的 evidence 74–76 和 `replica-delivery-status.md`。
