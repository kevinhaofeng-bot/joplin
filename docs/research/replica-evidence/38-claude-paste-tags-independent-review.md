# Claude表格粘贴及批量标签修复独立审查

2026-09-28。审查提交：`5e170c03e`、`0fbec38e3`。源码修改归Claude；本记录归Codex。

## 实际复跑

使用共享target、当前源码：

- GPUI bin velotype，过滤 `cmd_v_in_a_clicked_cell`：1 passed，0 failed，1464 filtered。
- core integration organization_stage_c，过滤 `batch_tag_removal`：1 passed，0 failed，13 filtered。
- GPUI bin velotype，过滤 `mounted_tag_buttons_act`：1 passed，0 failed，1464 filtered。

日志：`/tmp/joplin-independent-cell-paste-review.log`、`/tmp/joplin-independent-batch-tags-core.log`、`/tmp/joplin-independent-batch-tags-ui.log`。

## 代码与断言范围

粘贴：根Paste处理先检测单元格焦点，再路由到单元格编辑器；测试模拟点击单元格、Cmd+V纯文字与粗体片段、Tab后保存，断言正文前段未改、单元格内容正确。测试也确认图片被显式拒绝而不是误插正文。这是文字粘贴修复，不是完整Evernote表格体验；图片/文件单元格支持仍缺失，已反馈Claude。

标签：移除和清空改为使用全部selected_note_ids，仓储以单事务处理；测试覆盖包含废纸篓成员时全批回滚、部分携带标签、全选中清空、未选中保留、标签筛选结果。GPUI测试点击实际渲染按钮并确认多选不丢失，但初始多选由typed action构造，不是原生Cmd+点击输入链。

UI增加多选作用数量文案及部分携带标签的添加/移除入口。`selected_note_tag_counts` 在面板render期间同步逐笔查询SQL是新增性能观察项：目前没有证据认定超标，但应在大规模多选时测量，不能只依据小fixture断言不卡顿。

## 结论

源代码方向与两项已知缺陷一致，三项定向回归独立通过；尚未对包含新提交的签名Release做实机复验。因此仅为代码/测试层通过，不关闭用户层验收，不关闭表格多媒体或性能门。原型逆向映射仍需施工方给出精确文件和行为对应，不以测试中的Evernote注释充当源码阅读证明。

## 原型代码回读补充

本轮实际读取本机解包11.32.5：

- `renderer-readable/chunks/9435.js` 13155–13252：多选菜单的EDIT_TAGS把整个notes集合r交给编辑标签动作；RESTORE传guids集合s，EXPUNGE传noteGuids集合s，MOVE和DELETE均传notes集合r。这为“操作对象应覆盖多选集合”的复刻规则提供代码证据，但此片段本身不证明标签后端事务或部分标签的UI细节。
- `common-editor-sourcemap/@evernote/common-editor/src/apps/peso/modules/clipboard/commands/paste.ts` 98–191：先检查焦点，再处理输入框或文档光标位置；按selection/caret上下文解析剪贴板，传入资源信息，经过transformPastedSlice及handlePaste，最终替换当前选择区。这支持按实际焦点路由粘贴的原则，不支持将所有Paste投向正文。该片段不是“拒绝表格图片”的依据，也不单独证明表格完整能力。

以上路径均相对 `/Users/kevinhao/Projects/joplin-reconstruction/evernote-11.32.5/`。这里只记录行为理解和对应关系，没有将专有实现代码粘入产品源文件。当前Rust修复采取不同内部实现，最终仍以用户可见行为实测评估是否达到目标。

## 独立签名候选包

使用既有package-notes-macos.sh于20260928T102536Z构建：`/tmp/joplin-review-paste-tags/20260928T102536Z-0fbec38e3/Joplin Lite.app`，0.7.2 build16351。构建前后HEAD均0fbec38e395b0bc4a63c4579dd64eac53cd29d26，core/GPUI源码无脏改。二进制SHA256 `982c8d5d50372685c6c7a9d7e1449258ed8aea3aaf0f21e05897d9daaec0f4b3`，ad-hoc签名校验通过、未公证。日志 `/tmp/joplin-review-paste-tags-build.log`。

旧测试进程93218正常退出；新候选使用 `/tmp/joplin-current-ui.Od2T2b/library` 合成隔离库启动以进行复验。未替换 /Applications，签名成功不等于交付验收通过。

## 新候选实机文字粘贴复验

PID97805、窗口26691。点击既有表格笔记，双击第二行第二列，目视浮层标题确认坐标正确。一次焦点守卫发现其他测试进程抢焦点而拒绝操作；重新激活指定进程后再执行，不向其他应用发送输入。

将“新版单元格粘贴通过”放入临时系统剪贴板，以真实Cmd+V粘贴、Tab保存并转下一格，之后恢复原剪贴板全部项目。数据库确认笔记550c2db7bddb9c467bd95657602e9484的第二行第二列为该文本，表格前段仍是原“表格验收甲单元格粘贴丙”，另一篇df07...的body未变化。截图 `fixed-table-before.png`、`fixed-cell-open.png`、`fixed-cell-pasted.png` 位于隔离目录。

此原始文字粘贴误路由缺陷的真实输入与保存分支通过。尚未据此确认图片单元格、跨单元格富文本、重启后的新内容或批量标签原生多选流程。

## 新候选原生批量标签复验

同一PID97805。退出空的下一格浮层，以普通点击第一篇、Cmd+点击第二篇建立真实双选。组织面板两张卡片均高亮；显示“移动2篇笔记”“清空2篇笔记的标签”，初始一篇有验收标签，界面提供“为其余1篇添加”和“从1篇移除”。截图 `fixed-tags-selected.png`。

点击为其余1篇添加后，SQL确认两篇均有同一标签；截图 `fixed-tags-both.png`。随后点击“清空2篇笔记的标签”，SQL确认note_tags=0，两篇笔记仍在、正文长度分别207和216，与此前内容一致；截图 `fixed-tags-cleared.png`。原始“清空只影响当前一篇”的缺陷在真实Cmd多选、可见按钮与落库链通过。批量单标签移除的实机分支、未选中对照及重启仍需补充（其仓储/GPUI测试已通过），不泛化为全部组织功能已验收。

### 批量移除及重启补验

在双选保持的状态点击“为2篇添加”，SQL确认2条关系；目视按钮变为“从2篇移除 #验收标签”（`fixed-tags-remove-before.png`），点击后SQL确认note_tags=0、notes=2。正常退出97805并以同一候选重新启动99822，重启后SQL仍为0条标签、2篇笔记；550c...的`<td>新版单元格粘贴通过</td>`仍在。批量单标签移除及进程重启后的数据库持久化通过。未选中对照仍只有自动化测试证据，重启后表格新文本的目视重开未在此步执行。
