# Codex 接手修复记录

基线：`3d16771bc`。用户授权直接修复，未更换框架、未更新正式安装应用、未部署服务器、未修改原资料库。实现差异目前未提交。

## 审查问题处理

### P1 同附件并发上传

旧版12个独立ServerStore同时上传同hash实际失败（文件不存在）；部分上传重开后改size亦被旧版接受。新增回归先失败再通过。

`put_chunk` 现在使用稳定的每hash锁文件和OS独占锁，覆盖检查、追加、校验、发布整个过程；不同store/进程共享锁。锁inode不删除，避免新旧锁失配。声明size持久化在同一锁文件，重开后也不能改变。并发和size回归通过，服务端完整测试退出0（契约10/10）。此项不等于HTTP/客户端/NAS同步完成。

### P2 恢复无限读取与取消

备份和恢复统一通过有界复制helper，以声明size+1探测越界，额外字节不写入目标；循环读取前后检查取消。恢复先核对已打开文件长度，再复制和hash复核。新增限量Reader及中途取消测试通过；完整备份恢复既有测试通过。helper测试最初为缺函数编译失败，不冒称已在旧实现运行复现无限读取。

### P1 整篇迁移降级

Markdown复用整篇解析的事件（保留引用式链接解析），按顶层块转换；仅失败块降级，不再把周围可表示的标题/标记/图片一起转成源文本。HTML复用现有严格树解析，按子树尝试保留格式；不支持的容器只失去该层表达。修正HTML void img/br与XML内部结束标签检查冲突，继续由自己的栈核对其余结束标签。

分隔线导致整篇粗体和图片失效的回归，旧版实际失败，修复后通过。Markdown内嵌HTML显示源标签的回归亦先失败，再连接HTML局部转换后通过。原文仍由现有staging audit保存；不支持结构仍报告警告。未知markup、解析预算、无法解析的HTML不伪装成功。

## 实际验证与版本边界

- 最终core：`cargo test --manifest-path packages/app-lite-core/Cargo.toml --features test-support --tests`，退出0，338项；日志 `/tmp/joplin-codex-fixes-core-final2.log`。
- 服务端：`cargo test --offline`，退出0，10项契约；日志 `/tmp/joplin-codex-fixes-server.log`。
- GUI：1338/1338、退出0，日志 `/tmp/joplin-codex-fixes-gpui.log`；在最后“Markdown内嵌HTML局部转换”小补丁之前运行，最终小补丁由core全套覆盖，不能称同一最终构建的GUI全套证明。
- 格式与`git diff --check`通过；生产检查日志 `/tmp/joplin-codex-fixes-production-final.log`（需结合退出码，现有警告未清零）。
- Release真实副本探针：只读 `/tmp/joplin-lite-t2-accept/src/all_notebooks.jex`，输出独立 `/tmp/joplin-codex-migration.APjLz0/all_notebooks-1790429120`；1666篇、31笔记本、2组、64标签、425关系、4153资源、4127不同blob，重新hash无不符，源blob缺失/多余均0。该轮在最后Markdown内嵌HTML补丁之前完成。
- 与旧导入库对比：包含strong的笔记283→505，包含img的笔记221→407。这是结构保留计数，不是视觉验收。

## 未关闭的产品边界

376篇仍有转换警告，局部表格/引用/代码/不支持行内结构仍可能降级；不能称全部保真。原先整篇降级的机制已改，但迁移产品验收仍不通过，待逐类补齐和视觉核验。未运行本轮系统剪贴板/GUI实机、安装升级、NAS或内存验收。新增隔离导入目录含用户副本，保留供验收，未清理。

## 继续修复：语义块与编辑器可打开性（同日后续，未提交）

上文338项/376篇为前一轮快照，不是最新计数。

- 阅读解包源码 `common-editor-sourcemap/@evernote/common-editor/src/apps/peso/modules/codeblock/schema.ts`、`codeblock/serializer.ts`、`quoteblock/schema.ts`。确认代码块保留空白、引用包含段落/列表等结构；本轮仅连接现有本地模型能无损表示的单段引用、无语言代码块、分隔线，不宣称已覆盖Evernote全部结构。
- Markdown及HTML转换器接入现有 `Quote` / `Code` / `Divider`；HTML void元素加入hr。HTML保真检查仅将空格和canonical保护空格NBSP按相同显示字符比较，不折叠空白，不跳过结构和资源顺序复核。回归先失败，再通过。
- 新增只读 `migration_fidelity_audit` 示例，从已隔离资料库的原始审计字节重新严格转换；不写资料库、不输出笔记内容。最新1666篇中1386通过、280警告（上一轮1290通过、376警告）。日志 `/tmp/joplin-fidelity-audit-current.log`。表格、链接图片等仍有缺口，此计数不等于编辑器可用性。
- 普通段落内的历史行内图片，在native codec中转为原生图片块，保留前后文字标记、资源顺序、alt；打开本身不写库。后续编辑保存会采用块级表达，不宣称保持原行内排版。
- 增加显式运行的真实副本native加载/导出审计。第一次1666篇中17篇失败：标题内图5、列表内图10、引用内图2；该失败尚未关闭。正常单测不自动读取私人资料库，因此此测试默认ignore，须用 `JOPLIN_LITE_AUDIT_DATABASE` 显式执行，不能以普通测试通过遮盖它。
- 当前core全套340项退出0：`/tmp/joplin-core-current.log`；服务端10项退出0：`/tmp/joplin-server-current.log`。native模型扩展仍进行中，GUI全套和实机未重新验收。
- 最新全量隔离导入完成：`/tmp/joplin-migration-current.MeMdVA/all_notebooks-1790430342`，80.935秒；1666篇、31笔记本、2组、64标签、425关系、4153资源、4127不同blob，280警告。独立重新hash无不符，源blob缺失/多余均0。日志同目录上一级 `verify.log`。
- 最新真实副本备份/恢复完成：备份27.890秒，恢复30.733秒；4127 blobs、1,253,782,661字节。`backup_verify`增强为比较11张内容表的带类型/长度边界摘要（含正文、组织关系、修订和编辑日志），并独立重算恢复后的全部4127附件哈希；全部相同，进程退出0。备份与恢复目录均在 `/tmp/joplin-migration-current.MeMdVA`，日志 `backup-verify.log`。这是隔离副本验收，不是原资料库恢复或NAS演练。
- 正式安装App和原资料库未改。native图片扩展仍未完成验收，不得打包当前中间态。

### 原生图片后续验证（覆盖上文17篇失败快照）

- 标题、引用、列表内部图片采用稀疏语义父组：可视层复用完整图片块及按需读取，保存重建原父块内图片，不额外生成列表项。参考已读Evernote `modules/list/schema.ts:412` 的列表项多内容模型，以及 `list/keymap.ts:291` / `:424` 的Enter/ShiftEnter分工；分组事务是本项目独立实现。
- 结构事务只为受影响父组记录逆操作；补齐Enter拆父组、跨组合并唯一归属、整组样式/对齐/缩进、失败批事务回滚和ID回收。普通输入及IME不复制组元数据。
- 组内图片天然尺寸加载仅改变显示，不修改正文语义，不制造保存版本。挂载GPUI测试实测675×1200图片首次加载完成，flush后正文HTML与revision均不变。
- 最新完整GUI测试：1352通过、0失败、1显式资料库审计默认忽略，退出0，`/tmp/joplin-gpui-final-current.log`。随后对本轮新导入库显式执行ignored审计：1666篇、failure_categories为空，退出0，`/tmp/joplin-native-real-final.log`；该审计比较资源顺序及非空白文字，不能替代全部排版/表格保真检查。
- 两个旧UI拒绝路径测试原来用inline图片作“不支持”fixture；图片现在支持，fixture改为仍不支持的indent1，保留先拒绝后不读blob、切换清旧surface的保护断言。
- 边界：canonical Inline::Image仍没有用户缩放宽度字段，组内图片主动改宽暂不能持久化，会明确拒绝导出而非静默丢宽度；此限制未关闭。280篇转换警告仍在，整产品未验收。独立代码审查和Release构建正在进行，未提交、未推送、未覆盖安装App。

### 本轮代码验收关闭与Claude移交

独立审查发现组内原子插入、不同对齐父组跨选区删除会产生不可导出的状态。已修复新增图片成员归属、合并后父样式归一与完整逆操作；末尾图片新增文字继承父组样式/对齐。复杂父组文件附件在修改前明确拒绝，作为未实现能力移交，不称功能通过。

6项定向回归通过。最终完整GUI1355通过、0失败、1默认忽略，退出0，日志 `/tmp/joplin-gpui-accepted.log`；显式1666篇真实副本加载/回写审计退出0、failure_categories为空，日志 `/tmp/joplin-native-real-accepted.log`。`git diff --check`退出0。本轮代码修复验收结束；未做最终实机和安装验收，先前Release包早于最后修复不可交付。全部修改仍未提交。

剩余交付由Claude Code施工，Codex验收，执行交底为 `docs/superpowers/plans/2026-09-26-claude-remaining-delivery-handoff.md`。280篇警告、组内缩放及附件、全产品实机、NAS和资源预算均保留为未关闭门槛。
