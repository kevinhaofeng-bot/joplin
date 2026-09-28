# 打开的表格单元格保存边界

2026-09-28，Codex独立实现与验收，整款软件仍未验收完成。

## 根因和修复

现有单元格编辑器使用独立EditorCore；只有“完成”或Tab才调用set_table_cell。主笔记的flush_active_session未合并打开的单元格，因此真实键盘输入后手动保存，数据库仍保留旧单元格。失败测试` saving_open_cell_includes_keyboard_input_and_keeps_editor_open`见`/tmp/joplin-open-cell-save-red.log`，当时数据库正文仍为“甲/一”，没有“单元格新增”。

新增persist_table_cell_draft，在统一flush_active_session开始处把单元格当前canonical内容写入主文档，再走原有NoteSession保存边界。没有关闭单元格或抢焦点；内容相同时不制造额外事务；组合输入尚未确认、转换失败或目标无效时阻止该生命周期动作并保留编辑内容。保留既有完成/Tab/Escape路径，没有重写资源提交、主文档保存或同步协议。

## 验证

- 真实鼠标双击打开单元格，simulate_input通过已挂载输入处理器输入中文；这不是直接调用insert_text，也不是物理中文IME实机验证。
- 手动保存后数据库包含新内容，单元格仍打开；后续输入和保存有效，重复保存不产生额外数据库revision。
- Cmd+Z后保存去掉新增输入，保留原内容；Cmd+Shift+Z恢复相同HTML。
- 新建另一笔记、重新选择原笔记后，数据库与重新挂载的表格都包含保存内容。
- 通过平台输入协议设置未确认组合文本，WindowClose保存边界返回false，单元格保留，候选文字不落库。
- 定向6项通过：`/tmp/joplin-open-cell-boundary.log`；补充撤销/重做的定向通过：`/tmp/joplin-open-cell-undo.log`。

## 集成测试启动阈值误报

首次完整回归主测试1436通过，但`child_rejects_unknown_mime_without_waiting_for_stdin_eof`再次超过1秒（`/tmp/joplin-open-cell-full.log`）。额外诊断保持stdin打开，超时后观察进程：44.69ms后自行退出，stderr明确为unsupported-mime，见`/tmp/joplin-extractor-deadline-diagnostic.log`。8并发独立探针也均在stdin打开时正确退出，见`/tmp/joplin-unknown-mime-startup-probe.json`。诊断轮在沙箱内还出现两项Vision资源加载失败，不能记作绿色回归。

因此测试预算对齐生产原有15秒子进程预算，不再用1秒动态加载耗时推断EOF死锁；保持stdin句柄直到退出，增加unsupported-mime原因断言，超时明确kill+wait回收。生产超时和提取逻辑均未修改。最终完整回归日志`/tmp/joplin-open-cell-final-full.log`，进程退出0：1436主测试、17集成测试通过，4项忽略。`git diff --check`通过，既有编译warnings未清零。

## 与Evernote目标的关系和剩余差距

解析出的table/schema.ts:219将tablecontent置于同一文档树；我们的单元格弹出编辑器是独立缓冲，正是需额外生命周期桥接的实现差异。此次修复的是保存不丢内容的必要条件，不是复刻完成。

单元格尚未点击完成或触发显式保存边界时的自动保存、崩溃恢复和异步远端更新保护仍须验收；原生内联编辑体验、完整块内容、合并单元格及属性保真未关闭。最新签名候选062501Z不包含本次保存边界修复，不能拿它验证本次结果。未操作正式App或原资料库。
