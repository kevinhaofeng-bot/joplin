# 表格源码对照与剩余交付差距

## 本轮直接阅读依据

解析目录：`/Users/kevinhao/Projects/joplin-reconstruction/evernote-11.32.5/common-editor-sourcemap/@evernote/common-editor/src/apps/peso/modules/table/`。

- `schema.ts:85–95`：按列宽计算表格总宽度，DOM外层容器使用 `overflow-x: auto`，而非截断右侧列。
- `tableview/hooks/index.ts:180–191`：横向位移限定在0与 `scrollWidth-offsetWidth` 之间，返回当前与最大位移。
- `schema.ts:201–219`：单元格有跨度、宽度、背景色、文本/垂直对齐等属性，内容为 `tablecontent+`。这些是来源支持的能力，不应将当前子集称作完全复刻。

## 当前已验证的增量

Codex横向滚动增量（未提交）：新增真实水平滚轮用例，640px三栏窗口中正文58px，原实现右列在645px、正文右缘639px，失败日志 `/tmp/joplin-table-scroll-red.log`。LayoutRegistry现在保存每表格视图偏移并钳制边界；绘制与命中共用偏移，裁剪仍限于原正文范围。Surface只截获水平占优的表格滚轮事件，纵向事件继续原路径，不改变文档。相同用例通过（`/tmp/joplin-table-scroll-green.log`，0.42秒）。另补反向钳制和视口外不可点击断言，完整回归日志 `/tmp/joplin-table-scroll-full.log`，结果待取。可见滚动提示、横向滚动后的资源驻留预算与实际触控板验收仍待完成，不能称作整项已交付。

GPUI共享测量结果用于行高、绘制和命中；单元格粗斜体、下划线、删除线、高亮、链接外观、图片和附件标签已有自动化证据。实际鼠标双击表格附件经session校验取得正确文件字节的回归已补齐。资源提交失败恢复的两项回归已修复。详见12及完整测试日志。

## 仍然不符合目标的部分

横向滚动补丁完整回归退出0，日志 `/tmp/joplin-table-scroll-full.log`；自动化通过仍不关闭以下实机与保真差距。

1. 横向滚动及命中已通过自动化回归，隐藏列不可达的缺陷已修复；可见滚动提示、横向资源驻留预算和实际触控板验收仍未完成。
2. Cmd/Ctrl点击表格链接已接通外部地址打开，并有真实鼠标事件到平台URL接收的自动化证据；最新包实机、表格内内部笔记链接及完整链接交互仍待验收。
3. canonical表格模型当前以rows/header为主；合并单元格、列宽与单元格属性还需逐项对照迁移数据与既定交付要求，不得因为测试未覆盖而视作完成。
4. 单元格编辑仍须补跨保存、撤销与重新打开的实际键盘事件验收，不能仅依赖直接core调用。

## 下一修复顺序与边界

先补“窄编辑区中，多列表格可横向到达右侧列并正确命中”的失败事件用例，再增加每表格局部滚动偏移，绘制与命中使用同一偏移，限制边界，不影响正文纵向滚动，不修改文档或制造撤销步骤。之后验证图片驻留/附件位置在横向滚动后保持正确，并处理链接激活。保留目前已通过的富文本、附件安全打开和资源失败恢复路径。全部仅在隔离库验证。

Claude当前会话报告额度限制，Codex已接手；不得描述为Claude仍在后台施工。整款软件及表格完整复刻均未验收完成。

## 2026-09-28 12:11 核查

重新读取完整回归日志：主测试1431通过、2忽略；另一目标2忽略；集成测试17通过，无失败。`git diff --check`通过。以上为已有测试运行结果的复核，本次没有重跑测试。最新提交仍为`8fdcb4c6a`，表格及关联修复未提交，也未包含在此前隔离签名包中。下一步为补齐上述剩余项并构建新隔离包进行实机验收，不能用旧包验证新修复。

## 后续实际实现：表格链接

- 直接依据同一解析目录`plugin.ts:185–190`：Cmd/Ctrl点击锚点交给链接处理器，不应走单元格选择。
- 新增`table_command_click_follows_link_without_editing`，修复前真实点击没有打开URL，失败日志`/tmp/joplin-table-link-red.log`。修复保留每段文本的链接字节范围，使用绘制同源的字形测量命中；横向偏移与裁剪沿用表格现有布局。窗口层只对HTTP/HTTPS/mailto启动外部处理器。
- 测试覆盖普通点击不打开、Cmd点击实际打开正确地址、中文硬换行与长链接软折行命中、空白与内边距不误触、文档revision不变。绿色定向日志`/tmp/joplin-table-link-green.log`。
- 完整测试首次受沙箱禁止监听本机端口影响24项失败，见`/tmp/joplin-table-link-full.log`。带权限重跑主测试1432通过，但`child_rejects_unknown_mime_without_waiting_for_stdin_eof`一次触发1秒超时，见`/tmp/joplin-table-link-full-unrestricted.log`；单独重测通过（0.73秒）。未放宽超时或删除测试。再次完整运行退出0：1432主测试和17集成测试通过，4项忽略，见`/tmp/joplin-table-link-full-recheck.log`。这不证明偶发子进程启动超时已解决，保留待监测项。
- 屏幕控制重新尝试仍为`Transport closed`，没有操作正式资料库。新隔离包构建日志`/tmp/joplin-table-candidate-build.log`，启动时尚未完成，须检查实际退出状态和签名后才能使用。上述修改仍未提交。

下一性能检查的具体代码入口：`native_editor/render.rs`的`resident_ids`、`resident_resources`及表格`cache.load`循环，当前按整表可见性将表格所有图片加入驻留，尚未按单元格与横向/纵向视口交集过滤。不得仅凭单张图片测试通过即关闭多图表格内存门槛；下一步先用不可见单元格不发起加载的失败测试约束此处。

## 表格图片视口驻留修复

上述入口已修复为按每张图片的绘制范围、表格横向偏移、表格裁剪和正文视口交集筛选，筛选后的同一集合用于原图读取请求、解码缓存驻留及绘制加载。顶层图片的原预取路径不变。

- 新增真实窗口用例`table_hidden_column_image_loads_only_when_scrolled_into_view`：640px窗口右列完全不可见，修复前原图仍被materialize，见`/tmp/joplin-table-residency-red.log`。修复后不可见时没有原图路径，真实水平滚轮露出列后图片才加载显示，见`/tmp/joplin-table-residency-green.log`。
- `table_below_viewport_image_loads_when_note_viewport_grows`：用实际正文视口确认图片在下方，不提前读取，扩大窗口后显示，见`/tmp/joplin-table-vertical-residency.log`。最初360px高度fixture未满足“完全在视口下方”的前提，改为220px后验证前提和行为均通过；没有改动产品最小窗口约束。
- 完整回归首轮1433通过1失败：旧横向滚动测试在滚动前等待隐藏图片加载，与懒加载要求冲突。将等待移至滚入视口后，保留所有命中、边界和revision断言。最终完整回归退出0：1434主测试、17集成测试通过，4忽略；`/tmp/joplin-table-residency-full-recheck.log`。`git diff --check`通过。
- 本轮依据optimize技能做加载行为的前后验证；尚未取得整款应用RSS/footprint峰值预算证据，不能称内存验收完成。整张大表的文本测量和元数据遍历仍需预算验证。
- 旧候选`/tmp/joplin-codex-table-candidate/20260928T061707Z-8fdcb4c6a/Joplin Lite.app`已完成release构建、整包ad-hoc签名校验及`--version`（0.7.2），SHA256为`cdf34a861e38cf8dd6102cd9d344400d483a1266756d0b36f5404abd1950b85a`。该包包含链接修复但不包含本节驻留修复。屏幕控制及重置均返回Transport closed，实机验收未完成。

最新候选已完成：`/tmp/joplin-codex-table-candidate/20260928T062501Z-8fdcb4c6a/Joplin Lite.app`，包含驻留修复。构建日志`/tmp/joplin-table-residency-candidate-build.log`，进程退出0；重新独立验证整包签名与`--version`均退出0。SHA256为`0b23afda9d155800233ec6158eb6f4222d03ebab50ed12aea582f57145153e8c`。清单明确`worktree_dirty_for_app_sources: yes`；未提交、未推送、未替换正式App。

用该新包和新建隔离空库`/tmp/joplin-table-release-probe.jSupt3/profile`执行产品启动/内存脚本，1次启动、10秒稳定后RSS57.4MiB，脚本正常退出并仅结束自行启动的测试进程。机器M3 Max/48GiB/macOS14.7.2，原始证据在同目录`evidence`。这是空库启动基线，不是200块/10图负载、交互延迟、峰值内存或实机操作验收。
