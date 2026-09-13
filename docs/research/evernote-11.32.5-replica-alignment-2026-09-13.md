# Evernote 逆向与 Rust 复刻对齐审计（2026-09-13）

本页以 `codex/joplin-lite-native-rust` 的 v0.21 检查点为基线，并跟踪其后已通过限界复审的 ENEX C1b 只读扫描及 C2a 纯内容转换。它更新
`evernote-11.32.5-core-product-behavior-map.md` 第 4 节在 2026-09-11 写下的
「当前实现差距」快照；逆向资料、代码落地与真实产品验收是三种不同状态。

| 个人笔记核心链 | 实际阅读的 Evernote 证据 | 当前 Rust 落地与验证 | 仍缺什么 |
| --- | --- | --- | --- |
| 新建、标题、编辑、保存、图片 | `69451` create、`21288` select、common-editor `title`、`content/changesplugin`、`resource/image`、`textbetweenblocks` | `app-lite-core` 的 repository/规范 HTML/资源事务与 GPUI `AppModel`/`NoteSession`/原生编辑器；Task 5 三篇中文笔记及图片在隔离 Release 资料库重启后保留，见 `task-5-m1-release-acceptance-2026-09-12.md` | 该真实操作验收属于当时二进制；当前 HEAD 未对个人资料库重演完整 M1 流程。模型只有粗/斜/下划/删除/高亮/链接等有限 mark，尚无 Evernote 的字体、字号、颜色、上/下标与表格等完整样式保真，不能把基本编辑可用说成完美复刻 |
| 笔记列表、笔记本、标签、回收站、栏位 | `76905` list modes、Conduit NoteDAO simple projection、`74300` selected GUID reducer、Notebook/Tag mutators | GPUI typed route、轻量列表投影、保留 NoteId 的切换、组织操作、三/二/一栏；Task 6 报告有挂载测试及部分隔离 Release 操作 | 全部 Task 6/M2 的个人资料库浏览与性能门槛尚未签收；Evernote TOP_LIST、多标签层级等非首版能力也未照搬 |
| 全库搜索、笔记内查找、附件文字 | `83028` search AST/SQL、offline index queue、common-editor `find`、`34309`/`45897`/`59009` 资源搜索文字 | 本地 FTS、Cmd-K、Cmd-F、文件名、可选中文字 PDF 与图片 OCR；v0.19/v0.21 之前的隔离 Release 检索冒烟通过 | 以历史 1,662 篇规模作真实 Release 延迟/RSS 验收、建议与历史完整体验、HEIC/扫描 PDF OCR 和 M2 总验收未完成；中文短词策略与提示文案是本产品改进，不是 Evernote 原样算法 |
| Joplin 归档迁移 | Evernote `36364` 导入流程仅提供解析与 mutation 分离的行为参考 | v0.20/v0.21 的 `scan_jex_archive` 读 Joplin Raw/JEX，按 Joplin `BaseItem`/`urlUtils`/whiteboard 源码验证路径、资源和正文引用；27 个 golden 测试 | **没有 JEX 导入**、数据库 staging、正文转规范 HTML、关系/哈希核对、回滚与真实用户导出扫描；JEX 格式细节来自 Joplin 源码，不得称为 Evernote 逆向成果 |
| ENEX 导入与可读导出 | `36364` SAX 导入、`916042` ENML 清洗、`11354` ENEX 导出、`12524`/`18882` HTML 导出 | 源码已直接阅读；`b5ae370de`/`81b26d0c5` 将外层改为单遍分块扫描，>20 MiB 附件可计算 MD5/SHA-256，按笔记核对 `en-media`，并限制保留报告为 8 MiB；`5b689c17d..8d38cef16` 对支持的 ENML 子集转换到现有可编辑文档模型，并对表格/字体等未支持语义明确阻断；两段均经独立复审，最终 core 160/160。**只验收只读预检与纯转换** | 隔离 staging、原 ENML 留存、附件落库、可读导出与恢复尚未实现；不能把纯转换称为导入，也不能静默丢富文本或附件 |
| NAS 同步与恢复 | Conduit `MutationUpsyncActivity`、`RteSession`、`ResourceManager` | 本地 schema/outbox/cursor 的基础契约已在 core；普通写笔记不依赖网络 | Rust 客户端 transport、NAS server、冲突副本、断点资源和恢复演练仍属 Task 9/10，不能称为“同步完成” |

## 本轮直接重读的导入/导出细节

- `main-readable/src/modules/36364__note-import-mutation-import-enex-file-import-file-from-url.js`：SAX 解析 ENEX；每篇笔记积累标题、时间、标签、ENML 内容与附件，然后调用 `noteImport` mutation；有位置进度和取消。其资源 `data` 会从 base64 解码并写临时文件。我们的阶段边界应保留「先检查/转换、后写入」，但不复制它一次持有完整资源字节的内存行为。
- `renderer-readable/chunks/9093.js::916042`：导入前的 ENML sanitizer 去注释，解析 XML，并使用 tag/attribute allowlist；`en-media` 的 `hash`、`type` 与 `en-todo` 的 checked 是有效内容，不可当成普通文本丢弃。
- `common-editor-sourcemap/.../modules/resource/resource.ts::getAttributeResourceFromElement`：编辑器以 `en-media` hash 寻找资源；缺少客户端资源资料时保留 hash+MIME 的 fallback。这解释了为什么内容和附件元数据必须交叉核对并报告悬空引用。
- `main-readable/src/modules/11354__enex-exporter.js`：导出按 note 写标题、创建/更新时间、标签、note-attributes、CDATA ENML 和 base64 资源；`getNoteInfoForExport` 还取得 attachment 列表。可读 HTML/JSON+原附件仍是本产品的恢复出口，不应让 ENEX 成为唯一备份格式。
- 对照开源 Joplin `packages/lib/import-enex.ts::parseNotes/processNoteResource` 与 `packages/lib/import-enex-html-gen.ts::enexXmlToHtml_`：它按 note 批次处理，将资源 `<data>` 流写临时文件、base64 解码，再以实际字节的 MD5 对应正文 `<en-media hash>`；HTML 路径保留图片/附件及 checklist 的区分。它自己的预处理注释明确说整体载入 1GB+ ENEX 会耗尽内存，因此我们的 Rust 路径不得整体读取归档。这里属于 Joplin 可借鉴实现，不属于 Evernote 逆向成果。
- Rust `CanonicalDocument` 目前只有段落、H1–H3、列表/清单、引文、代码、图片、附件和分隔线，没有表格块。ENML 表格、未知样式若直接投影为当前规范 HTML 会有保真风险；应先保留原 ENML，并在正式导入前用明确的“受支持/不支持”清单阻断静默损失。
- `main-readable/src/modules/68232__sync-manager.js::ENSyncManager`：任务队列区分初始下行与后台活动，暂停/恢复、鉴权变更和队列持久状态各有边界；`66578__n-sync-event-manager.js::NSyncEventManager`：连接信息保存于 sync state，重连退避，接收/处理/可见是不同的完成状态，暂存资源还有 finalize 阶段。我们只借鉴“持久队列、游标、资源发布与可解释状态”的机制；个人 NAS 不复制 Evernote 的鉴权平台、云端预建 datastore、协同会话复杂度。当前 Rust `schema.rs` 已有 `sync_outbox`、`sync_cursor`、`sync_conflicts` 表，但没有传输、服务端或双端重试验收。

## 执行裁定

下一段 Task 8 进入隔离资料库 staging；C1b 只读预检及 C2a 纯转换都还不是导入器。staging 应保留 ENML 原文以便审计，逐篇将已验证的同笔记附件映射到 C2a 转换，遇不支持结构保留原文并明确报告、停止无损承诺，不以「能搜索到文字」冒充图文迁移成功。正式资料库不进入这一轮。同步从 Task 9 独立推进，不能混入导入事务。

导入 staging 还有一个现成仓储契约要显式处理：`LibraryRepository::create_note` 会在写笔记的同一事务插入 `sync_outbox`。Task 8 计划规定迁入资料仅在用户启用新服务器时建立明确的初始同步基线；后续不能简单循环调用普通 `create_note`，否则虽然目前没有网络传输，仍会制造一批语义不明的待同步操作。隔离 staging 必须有专门的导入事务/基线规则，并通过断言检查 outbox、索引与资源关系。
