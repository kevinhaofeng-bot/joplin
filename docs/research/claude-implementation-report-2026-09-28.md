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

