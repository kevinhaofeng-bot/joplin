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

