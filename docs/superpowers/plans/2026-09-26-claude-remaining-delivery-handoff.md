# Claude Code 剩余交付施工交底

**Goal:** 接续现有 Evernote 核心笔记产品，Claude Code 施工，Codex 独立验收；不是重做编辑器。
**Architecture:** 保留 GPUI/Metal + Rust + SQLite WAL + 可读 canonical HTML + 内容寻址附件。
**Tech Stack:** 现有 Rust 工作区与 macOS 原生桥，不引入 WebKit/Node 运行时。
**Spec:** `docs/superpowers/specs/2026-09-11-evernote-core-notes-product-design.md`。

## 权威和现场

本交底更新 `2026-09-26-claude-evernote-product-delivery.md` 的基线和阶段状态，其任务0–7的产品范围、源码阅读要求和预算仍有效。旧审计中的“模块不存在”、376警告、旧测试崩溃不得未经核查重复当成现状。

工作目录 `/Users/kevinhao/Projects/joplin/.worktrees/joplin-lite-native-rust-mvp`，分支 `codex/joplin-lite-native-rust`，HEAD `3d16771bc3229309f992d8b4dd5a8f9683850800`。目前存在Codex未提交修复和用户文件：先记录 `git status --short` / `git diff --stat`，保留全部，不reset、不覆盖、不把所有脏文件一起提交。无需等待用户再次确认施工。Codex负责最终通过判定。

先读 `docs/research/replica-review-fixes-2026-09-26-codex.md` 的最新追加记录。原安装App、原Joplin库未变；只允许独立测试profile。不得以数据库备份通过宣称可读全库导出通过。

## 当前接受范围与未完成边界

- 服务器同hash并发上传锁、备份有界读取/取消、迁移按失败子块降级已修复。
- 原生标题/引用/列表含图可加载并回写父语义；组内Enter、跨组删除合并、原子图片边界插入、撤销已补回归。图片天然尺寸加载不制造保存版本。
- 最新GUI完整测试1355通过、0失败、1显式真实库测试默认忽略；日志 `/tmp/joplin-gpui-accepted.log`。core340项、server10项已通过（见修复记录）。存在编译警告，不宣称清零。
- 真实隔离导入1666笔记、31本、2组、64标签、425关系、4153资源/4127blob；全部资源hash一致。1386严格转换、280仍有警告。独立备份恢复11张表摘要和4127blob校验一致。
- 组内图片用户缩放宽度尚不能持久化；复杂父组内文件附件目前在变更前明确拒绝，避免产生不可保存脏状态。这是待实现能力，不能算完成，也不能隐藏按钮充数。
- 没有最终Release实机、中文IME全矩阵、安装升级、全产品RSS、NAS两客户端同步验收。之前生成的Release包早于最后事务修复，不能直接交付。

## 执行顺序（沿用旧计划，不重复已完成工作）

### 1. 先关闭剩余编辑保存边界

阅读现有 `packages/app-lite-core/src` canonical模型以及 `packages/app-lite-gpui/src/native_editor/{codec,model,transaction}.rs`，确认真实类型再改。参考解包Evernote image/list/attachment schema与交互代码，记录实际文件/符号→行为→本地实现→测试。

- [ ] 先补失败回归：含图标题/引用/列表图片缩放，保存、切换、重开宽度不丢；父块内插入普通附件，保存和撤销重做。
- [ ] 最小扩展canonical属性与原生映射；兼容已有HTML和旧资料库，不降级整篇、不破坏前后文字及资源顺序。
- [ ] 在现有 model/codec/note_session 测试中验证，不另建编辑器。覆盖左右对齐差异、图片前后输入、跨块删除和失败事务回滚。
- [ ] 写 `docs/research/replica-evidence/08-remaining-delivery.md`，逐项标记实现/单测/实机状态及阻断。

### 2. 关闭迁移与完整退出链

- [ ] 按旧计划任务2–3，先核查已有实现，只补缺口。280警告逐类别处理：链接图片、表格及未知HTML等不能通过屏蔽警告解决。保留原始审计字节。
- [ ] 实现/核验全库可读HTML/JSON/资源导出与入口，区别于SQLite全库备份。恢复到独立空目录，检查组织关系、正文、历史及资源hash。
- [ ] 只用 `/tmp/joplin-migration-current.MeMdVA/all_notebooks-1790430342/library.sqlite` 副本；不写原资料库。源JEX `/tmp/joplin-lite-t2-accept/src/all_notebooks.jex` 只读。

### 3. 产品业务与同步

- [ ] 按旧计划任务4–5核查生命周期、批量组织、浏览、OCR/PDF搜索与附件。已实现功能不得重写；有证据的失败才最小修复。
- [ ] 按任务6核查现有protocol/server/client/outbox，不能再次假定不存在。完成真实HTTP、离线两客户端、幂等、附件断点、冲突和恢复矩阵。OS文件锁单测不是同步验收。
- [ ] NAS接入前读取 `/Users/kevinhao/servers.md`，使用独立测试服务，不改现有生产服务。需要新权限时报告，不绕过。

### 4. 交付包与验收移交

- [ ] 按任务7生成新时间戳的Release包，不覆盖安装App、不运行清空dist的donor脚本。
- [ ] 验证全产品而非editor spike：RSS空闲120MiB/典型160MiB，输入p95<16ms、搜索50条<100ms，具体负载沿用原计划。
- [ ] 汇总同一构建的GUI/IME/图片/组织/导入导出/重启/同步证据。没有屏幕权限的项目明确留待Codex实机验收，不伪造通过。

## 每阶段验证和报告

从对应目录运行，失败先定位，不删断言绕过：

```sh
# packages/app-lite-gpui
cargo test --locked --bin velotype
JOPLIN_LITE_AUDIT_DATABASE=/tmp/joplin-migration-current.MeMdVA/all_notebooks-1790430342/library.sqlite cargo test --locked --bin velotype imported_real_copy_opens_and_round_trips_resources_in_native_editor -- --ignored --nocapture
# 工作树根
cargo test --manifest-path packages/app-lite-core/Cargo.toml --features test-support --tests
cargo test --manifest-path packages/app-lite-server/Cargo.toml --offline
git diff --check
```

每一阶段记录改动文件、Evernote源码证据、命令/退出码、日志位置、未通过项、提交与未提交边界。先做阶段1，再按顺序推进互不依赖工作；不得把自测称为Codex验收。风险性模型/协议变化先记录方案和兼容测试。若被额度/权限阻断，写明并停在可恢复状态，不用占位实现或假成功交差。不push、不覆盖用户安装、不修改本项目外配置。
