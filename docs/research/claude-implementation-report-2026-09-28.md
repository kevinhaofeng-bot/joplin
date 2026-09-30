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
