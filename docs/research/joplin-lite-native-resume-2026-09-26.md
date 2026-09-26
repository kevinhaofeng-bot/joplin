# 2026-09-26 恢复推进记录

用户「继续推进」已解除 2026-09-13 的暂停。旧暂停交底保留为历史现场；本记录是后续进展入口。

最新入口验收见 [原生导出与图片修复记录](native-export-ui-acceptance-2026-09-26.md)：实机导出 1 篇/2 资源、取消与重开通过，图片按钮修复通过实机；浏览器本地文件受工具策略阻止，未绕过，未标为验收。全套测试 SIGSEGV 正在独立定位，不能声称整款产品完成。

后续：`6daa9d7e6` 已提交原生导出与图片按钮修复；`1f70bbde2` 修复了旧 selection 测试退出时的 use-after-free（仅测试一行）。独立 selection 12/12 通过；修复后整套检查进入后续 Cmd-V 测试，在 Cocoa 空指针 SIGABRT 中止，详见 [清理崩溃诊断](selection-test-cleanup-fix-2026-09-26.md)。下一步定位该粘贴测试边界；未推送、未标记全套通过。

## 基线与本轮范围

- 工作树：`/Users/kevinhao/Projects/joplin/.worktrees/joplin-lite-native-rust-mvp`，分支 `codex/joplin-lite-native-rust`，恢复时 HEAD `a338ae328`。
- 恢复时仅旧暂停交底为未跟踪文件，没有遗留未提交代码；本轮不改稳定编辑器、个人资料库、个人备份或服务器。
- 当前仍是 Task 8 可读选区导出/隔离恢复实验，不是全库备份或真实迁移验收。原复审 1 Critical / 5 Important / 2 Minor 逐项处理。
- 前两项由 Luna 实现，主代理核对代码、源行为和独立回归。用户随后明确调整编码代理为 GPT-6 Sol；Luna 已中断，恢复 I/O 切片改由 Sol 接手，避免同时修改。半小时进度 heartbeat 已通过应用恢复为 ACTIVE。

## 已核验：历史独有附件

旧实现只收集最新 `note.resource_ids`，保留历史 HTML 却漏掉历史独有资源。新增回归先在旧代码失败：A 图片改为 B PDF 后导出只有一个资源；A 的 blob 缺失/损坏仍导出成功；从 manifest 删除历史 A 后恢复仍成功。

修改后复用 `CanonicalDocument::resource_ids`，收集当前与全部保留历史正文的资源并集，沿用元数据、原字节 SHA-256 核对与临时目录发布。恢复逐历史正文检查引用存在于已验证 manifest；最新正文的附件关系保持原样，不把历史附件加进最新正文。

主代理独立验证：`cargo test --manifest-path packages/app-lite-core/Cargo.toml --test export_restore --quiet` 9/9；core `--tests --quiet` 229/229；`cargo fmt --check` 与 `git diff --check` 通过。这是合成隔离资料库验证，不是个人资料库或 GUI 验收。

## 源行为对照

本轮重读 `/Users/kevinhao/Projects/joplin-reconstruction/evernote-11.32.5/main-readable/src/modules/11354__enex-exporter.js` 的逐笔记元数据、内容、附件导出及资源读取错误分支。借鉴其将内容与资源一起导出的职责；本产品的「历史资源并集」「任一引用资源缺失即拒绝整个包」是独立完整性要求，不宣称 Evernote 的 ENEX 原样实现了这些保证。

## 已核验：导出与恢复容量限制

新增回归先证实旧 exporter 会发布超过 4 MiB HTML 恢复限制的正文。现已在发布前检查笔记数、资源数、当前与历史 HTML/text 限制；累积清单字符串字节下界采用 checked_add，超过 32 MiB 提前拒绝；最终使用 64 KiB BufWriter 加有界 writer 流式写 pretty JSON，精确计入转义与缩进开销，flush/sync 后才发布。

主代理最终独立运行 core `cargo test --tests --quiet` **233/233 通过**（包含导出恢复 10 项、有界 writer/budget 3 项），fmt 和 diff 检查通过。该项只关闭容量不一致与序列化分配问题，不保证所有剩余条件下的恢复成功。

## 剩余边界与接续

### GPT-6 Sol 接手：恢复 I/O（本轮）

- RED：独立子进程限制为 48 个文件句柄时，旧实现恢复 64 个资源报 `Too many open files`。限制未作用于系统或主测试进程。
- 改为固定 root/notes/resources 目录描述符；资源逐个安全打开、校验和关闭。通过单组件 `openat`、`O_NOFOLLOW`、文件 `O_NONBLOCK` 和已打开文件元数据检查，替代路径 lstat 后重新打开。沿用本项目 `resource.rs` 的 Unix 安全模式，未改稳定 ResourceStore。
- manifest/HTML 使用上限加一的限量读取；资源声明大小先限制为 50 MiB，再按声明大小限量核对。恢复重新打开资源后，核对实际导入 SHA-256/长度；发生变化时丢弃临时恢复库，目标为空，不声称外部源不可变。
- 新增固定目录后替换路径、无限增长流、验证后同长度资源改写、各层符号链接拒绝测试。真实 macOS 测试发现末尾 `/` 或 `/.` 可绕过根路径 O_NOFOLLOW，先 RED，现已在打开前剥离这些尾缀并回归。
- 主代理首轮全测确实在尾缀 symlink 用例失败，未当作通过；修复冻结后重新独立运行 core `cargo test --tests --quiet`，**238/238 通过**（其中导出恢复 12 项）。最终 fmt/diff 检查通过。

### GPT-6 Sol：一致源快照与 durable 元数据（独立审查通过）

- 在已有仓库连接上开启一个 SQLite deferred read transaction，统一读取默认笔记本、选定笔记、关系、封面、历史及资源元数据；返回有界清单后释放事务和 mutex，再复制捕获哈希对应的 blob。
- 导出格式升级为实验 v2，保留 notebook revision/created/updated 与 resource created/updated；恢复重新打开仓库核对。真正缺少新字段的 v1 明确拒绝，不补造数据。默认笔记本若属于 Stack 则拒绝，避免悄悄压平组织结构。
- 确定性并发测试在第一次读取后通过第二连接改笔记本和正文，验证输出保持同一旧快照；复制前通过原 repository 写设置，证明数据库锁已释放。失败路径也验证后续可写。
- 代理最终及主代理独立 `cargo test --quiet` 均 **243/243 通过**，主代理 `cargo fmt --check` 通过。累计差异已交独立 GPT-6 Sol 审查；未将测试通过当作审查通过。

独立审查另发现导出源 blob 的物理读取/复制没有按记录大小限量；Sol 改为复用 limited-open 并只复制至 size+1，超限不发布。主代理独立 9/9 单元及 16/16 集成通过，复审确认 1 Important 已关闭、无新增问题。该完整性切片可保存本地检查点，不代表全 Task 8 完成。

下一切片是独立 HTML 浏览页，再接原生导出入口及隔离 Release GUI 验收。FTS 恢复查询验证仍待补。I/O 修复不扩大为对同一用户权限恶意改写整个目标 staging/profile 的完整安全保证。

完整性修复已保存本地提交 `5883f73ca`，旧暂停交底仍未跟踪。未推送、未打可用备份标签。

## 可浏览导出页（代码审查通过，实景验收待做）

新增 `index.html` 与 `readable/<id>.html`，使用现有 html5ever 对实际图片/附件属性映射相对本地资源路径，正文中的同名字串不改写；支持对齐、缩进、清单、块图片独立占行及显示宽度。canonical 文件和恢复路径不改。

主代理独立导出恢复测试 18/18、fmt/diff 通过。独立审查的唯一 Important（块图片缺独占行规则）已修复并复审关闭；主代理再跑该确切回归 1/1 通过。浏览器实测留在原生入口完成后一起验收，不能仅从 HTML 断言声称视觉通过。

接下来实现原生 `导出当前笔记…`，并修正产品构建未继承 core vendored XML parser 的依赖路径。基线菜单测试 8/8 通过，但仍有既有 objc cfg/dead-code warnings。真实迁移、全库导出 GUI、NAS 双端同步仍未完成。
