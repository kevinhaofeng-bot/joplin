# Evernote 11.32.5 核心笔记产品行为源码映射

日期：2026-09-11
用途：把可阅读源码中的产品机制绑定到本项目的 Rust 复刻规则与验收项，避免只凭截图或印象造轮子。

## 1. 证据范围

本轮阅读基于以下本机只读材料：

- `/Applications/Evernote.app/Contents/Resources/app.asar`；
- `/Users/kevinhao/Projects/joplin-reconstruction/evernote-11.32.5/main-readable`：2,843 个完整拆分模块，0 缺失；
- `/Users/kevinhao/Projects/joplin-reconstruction/evernote-11.32.5/common-editor-sourcemap`：3,618 个带 `sourcesContent` 的编辑器原始 TypeScript/TSX 文件；
- `app.asar!/node_modules/conduit-core/dist/**`：SQLite、笔记仓库、本地搜索、资源与同步队列的可读构建产物和 source map。

Evernote 11.32.5 的实现栈是 Electron 37.6、React 17、ProseMirror、Yjs、BetterSQLite3 与 Conduit 本地优先数据层。我们复刻产品机制和可观察行为，不照搬它的进程重量。

## 2. 产品行为映射

| 产品能力 | Evernote 源码证据 | 已读机制 | Rust 复刻决定 | 阶段 |
| --- | --- | --- | --- | --- |
| 新建笔记 | `main-readable/src/modules/69451__create-note.js`、`32965__new-note-new-notebook.js` | create mutation 先接收标题、容器、标签、初始内容并返回实体；成功后才选择和导航 | SQLite 事务先创建稳定 NoteId 和空正文，再选择、加载、聚焦；无临时“编辑器笔记” | R1 |
| 选择笔记 | `21288__select-note-middleware.js` | 普通选择被统一转换为导航；搜索视图保留搜索上下文 | `NavigationState` 按 NoteId、来源视图和过滤条件导航；选择不直接替换 UI 数组下标 | R1 |
| 选择笔记本 | `56175__select-notebook-middleware.js` | 笔记本选择携带 notebook/note/workspace 并进入统一导航 | 选择笔记本只改变 query + navigation，编辑器会话通过统一切换边界 flush | R2 |
| 笔记列表模式 | `76905__note-list-view-option.js` | 明确区分 CARDS、SNIPPETS、LIST、TOP_LIST | 交付卡片、摘要、紧凑三种密度；TOP_LIST 不进入首版 | R2 |
| 轻量列表查询 | `app.asar!/node_modules/conduit-core/dist/repositories/entities/note/utils/NoteDAO+ListQueryBuilder.js` | simple projection 只取 id、label、snippet、时间、父容器等；过滤、排序、分页在 SQL 完成 | `NoteListItem` 不含 `body_html`、merge state 或附件 bytes；虚拟化列表分页读取 | R1 |
| 列表缩略图 | `app.asar!/node_modules/conduit-core/dist/repositories/entities/note/NoteRepositoryImpl.js` | note projection 带 snippet/thumbnail；搜索命中附件可临时作为结果图，不改持久选图 | 持久 `selected_thumbnail_id` 与搜索命中 thumbnail 分开；卡片缓存独立于正文纹理 | R1/R3 |
| 列表订阅 | 同上 | query witness 让列表响应实体变化；一次性渲染查询避免长期保留 watcher；列表渲染不得触发正文下载 | Repository 发紧凑 `LibraryEvent`；卡片刷新不加载正文、不打开同步连接 | R1 |
| 标题事务 | `common-editor/.../modules/title/title.ts` | 标题限制和清洗通过 `SetMetaStep` 进入事务，而非失控的独立输入状态 | `TitleInput` 的提交进入与正文相同的 NoteSession generation/save coordinator | R1 |
| 编辑器生命周期 | `common-editor/.../modules/editor/plugin.ts` | initialized/editable/appActive/focus 是显式状态；blur 的瞬态误报被抑制 | NoteSession 显式管理 loading/ready/dirty/flushing/failed；切笔记不依赖焦点偶然事件 | R1 |
| 内容变更和已保存状态 | `common-editor/.../modules/content/changesplugin.ts`、`commands/flush.ts` | 用户变化立即通知未 settled；500 ms trailing debounce、15 s max；异步操作可阻止 settled；读取最终内容前显式 flush | 立即 dirty，100 ms 崩溃 journal，500 ms 完整快照，15 s 强制快照；切换/关闭/同步前 flush | R1 |
| 中文输入 | `common-editor/.../components/common/CompositionSafeInput/index.tsx`、`modules/sync/bindings/composition.ts` | composition 期间不回写受控值；外部更新用最小 diff 保持选区；blur 清 stuck latch；composition end 强制同步通知 | 保留 GPUI `EntityInputHandler` 唯一桥；marked text 不生成中间语义事务，commit 后一次入历史和保存 | R1 |
| 统一文档结构 | `common-editor/.../modules/editor/schema.ts` | 文档是 `section+` 结构；markholder 保持空位置样式；ENML 是投影 | GPUI `Document` 是打开时编辑模型；规范 HTML 是可读快照；二者通过确定性 codec 转换 | R1 |
| 图片前后输入 | `common-editor/.../modules/textbetweenblocks/plugin.ts`、`noteendparagraph/plugin.ts` | 相邻图片/文件/表格等原子块间的点击死区会插入或聚焦段落；末尾点击生成尾段 | 每个原子块边界都有 Before/After caret；点击死区、方向键和 Enter 使用同一 DocPoint 规则 | R1 |
| 图片立即显示 | `common-editor/.../modules/resource/image/imagecomponent.tsx` | image node view 自己管理 load/error/selection/resize；节点 attrs 变化直接驱动当前视图 | 资源导入、文档节点、资源关联和列表事件是一笔应用事务；当前编辑器先重绘，不靠重新选笔记 | R1 |
| 图片尺寸与对齐 | `common-editor/.../modules/resource/image/image.ts`、`ResizeHandle.tsx`、`imagecomponent.tsx` | 保存自然尺寸和可选显示宽度；按比例缩放；四角 handle；双击恢复自然尺寸；对齐与环绕互斥 | Image block 保存 natural size/display width/alignment/wrap；拖动只画预览，mouseup 产生一个可撤销事务 | R3 |
| 图片加载预算 | 同上 | 大图可默认附件视图或禁用预览；宽度受 note/cell 限制 | 导入保存原文件，显示用有硬预算的 proxy/texture；单张大图不能绕过缓存上限 | R1 |
| 剪贴板 | `common-editor/.../modules/clipboard/plugin.ts`、`commands/paste.ts` | HTML/plain/resources 多表示解析；保存选区、来源清洗、分阶段转换；Markdown fallback 设历史边界 | 按 image/file/html/plain 优先级分类；解析与资源导入完成后单事务插入并恢复选择；无占位符路径 | R1/R3 |
| 列表切换 | `common-editor/.../modules/list/commands/insertlist.ts` | OL/UL/todo 共用结构命令；可转换、解包、换类型；执行后回到编辑器 | 一个 list command family 修改 BlockKind/depth，不以字符串前缀模拟 | R1 |
| 列表键盘语义 | `common-editor/.../modules/list/keymap.ts`、`list.ts` | Enter 拆分非空项；空项 outdent/退出；Shift-Enter 保留 marks 软换行；项首 Backspace outdent/合并；todo 新项清 checked/value | 把这些序列写成 GPUI 编辑器行为矩阵；每个按键最多形成一个 history step | R1 |
| 全选 | `common-editor/.../modules/selection/commands/selectall.ts` | 支持覆盖结构文档的 AllSelection | 第一次 Cmd-A 选块内，第二次选全文的现状需与应用级复制、删除形成稳定规格 | R1 |
| 当前笔记查找 | `common-editor/.../modules/find/plugin.ts` | 匹配是 decoration；超过 500 个结果时只装饰视口附近；primary match 独立滚动 | `Cmd-F` 是 NoteSession 内查找；结果多时只为视口绘制高亮，不污染全库搜索状态 | R3 |
| 搜索输入语法 | `main-readable/src/modules/51029__search.js` | 保留引号和空格；解析 contains/tag/notebook/stack/space/author/trash 等 filter chip | 支持普通词、短语、notebook/stack/tag/date/trash/attachment；团队/作者/space 过滤移除 | R2 |
| 搜索交互 | `99128__search-event.js` | 搜索条、弹层、最近搜索、最近笔记、go-to 建议、过滤器和历史删除是独立事件 | `Cmd-K` 打开本地快速搜索；最近笔记、笔记本、标签建议共用一个 query service | R2 |
| 离线搜索 | `app.asar!/node_modules/conduit-core/dist/repositories/search/LegacyPlugin/offline/OfflineSearchEngine.js` | 本地解析、search、suggest、历史；删除查询时连严格前缀一起删除 | SQLite 双通道 FTS + 本地历史；首批结果不访问网络 | R2 |
| 索引队列 | `.../OfflineSearchNoteIndexingDAO.js`、`.../OfflineSearchIndexActivity.js` | 正文写入 replace queue row；默认批 100，最旧优先；提取可见文本和 note links；每篇 yield；事务替换索引 | 每次正文提交写 `search_queue`；批 100；OCR/PDF 同队列分类型处理，失败不回滚笔记 | R2/R3 |
| 笔记本 | `.../CoreEntityTypes/Mutators/NotebookMutators.js`、`41614__show-nav-notebooks-context-menu.js` | create/rename/delete/move notebook 走本地 mutator；导航有独立上下文菜单 | 本地事务创建、重命名、移动到 stack、删除；删除前明确笔记迁移规则 | R2 |
| 标签 | `.../CoreEntityTypes/Mutators/TagMutators.js`、`55616__show-tag-context-menu.js`、`60306__show-nav-tags-context-menu.js` | 标签增删改与 note membership 是独立实体关系 | `tags` + `note_tags` 多对多；批量应用；删除标签不删除笔记 | R2 |
| 回收站 | `15502__show-nav-trash-context-menu.js`、`35178__is-trash-item-selected...js`、`NoteMutators.js` | delete 是移入 trash；restore 与永久删除分开 | 默认软删除并移出普通 FTS；恢复保留原容器；永久删除只在回收站 | R2 |
| 本地正文存储 | `.../repositories/entities/note/local/NoteDocumentStorage.js` | 每笔记 keyed read/write mutex；正文 cache 2、metadata cache 100；写正文触发索引队列 | 单 NoteSession + keyed storage lock；列表不缓存正文；保存事务触发索引/outbox | R1 |
| 本地优先会话 | `.../ENConduitSync/Plugins/RTE/LifecycleProvider.js`、`RteSession.js` | 先读本地、远端 fallback；观察标题/资源/正文；批处理 optimistic 更新；destroy 前处理待办、离线 queue、最终 save | 打开永远读本地；网络不在打开路径；会话销毁必须 flush journal/snapshot/outbox 后再释放 | R4 |
| 持久同步队列 | `.../ENConduitSync/SyncManagement/MutationUpsyncActivity.js` | 冗余 mutation 可 rollup；批量发送；区分 success/retry/permanent failure；成功后清 retry 状态 | 同事务写 `sync_outbox`；按实体合并无意义中间修改；幂等批次与明确失败分类 | R4 |
| 资源上传 | `app.asar!/node_modules/conduit-core/dist/ResourceManager.js`、`59823__get-file-resources-to-upload.js` | stage/upload/finalize；失败有 fallback copy；资源 bytes 独立于笔记 body | SHA-256 查重、分块上传、完成后原子发布、Range 下载；正文只含资源 ID | R4 |
| 导入 | `36364__note-import-mutation-import-enex-file-import-file-from-url.js`、`64997__import-file.js` | ENEX/import 是独立 mutation/job，处理资源与笔记实体 | 先做只读扫描和计数，再 staging DB，校验后原子切换；支持 ENEX 和现有 Joplin 数据 | R3 |
| 导出 | `11354__enex-exporter.js`、`12524__single-html-exporter.js`、`18882__abstract-html-exporter.js`、`26944__export-notes-into-html-action.js` | ENEX、单 HTML、多 HTML、PDF 是不同 exporter，共用取消/资源抽取 | 首版交付 HTML 目录 + JSON manifest + 原附件；ENEX 作为互操作导出；PDF 是单笔记动作 | R3 |
| 导航历史/标签页 | `main-readable/src/modules/61978__main-window-tab-manager.js` | 500 ms 持久化 tab 状态；active 先恢复；back/forward 与过滤状态同存；warm preload 会占内存 | R2 只做 back/forward 和最近笔记；不 warm preload；多标签页进入核心后评估 |

## 3. 最值得直接借鉴的工程原则

### 3.1 UI 不持有数据真相

Evernote 的按钮、列表、标题、编辑器和同步层都围绕事务、repository 和 navigation state 工作。我们的 GPUI 控件只派发 typed action；笔记、选区、过滤器、保存与同步状态分别只有一个权威来源。

### 3.2 列表与正文必须分离

Evernote 明确区分 simple list 与 detailed note，并限制正文缓存。我们的列表 projection 只含卡片所需字段；打开正文只保留一个完整 NoteSession。这是复刻“上千篇笔记仍然顺手”和降低内存的关键。

### 3.3 保存、索引、同步是三条可观察流水线

正文保存成功不等于索引完成，也不等于远端同步完成。三者分别显示状态、分别重试：

- 本地保存失败：阻止切换并明确报错；
- 索引失败：笔记仍安全，后台重试；
- 同步失败：笔记仍可编辑，outbox 保留并显示原因。

### 3.4 原子块边界是编辑器正确性的核心

图片前后能否继续输入，不是图片组件的小修补，而是文档坐标、命中测试、选择映射和事务共同决定。`textbetweenblocks`、`noteendparagraph`、list keymap 和 image node view 必须作为一组行为复刻，不能分散成 UI hack。

### 3.5 只吸收个人笔记核心

源码中的 AI、任务、日历、协作、权限、团队空间和营销流程不进入路线图。它们会增加依赖、常驻状态和认知负担，却不改善个人笔记的捕获、组织、找回、同步与恢复。

## 4. 当前实现差距

| 层 | 当前真实状态 | 结论 |
| --- | --- | --- |
| GPUI 编辑内核 | 已有结构块、输入、选区、列表、图片、命令、撤销、布局和缓存测试 | 保留并继续按行为矩阵完善 |
| GPUI 产品壳 | `spike_app.rs` 单页示例，固定标题，内存文档 | 必须替换为真实 AppModel/NoteSession/LibraryView |
| Rust 本地存储 | 旧 AppKit crate 有 SQLite、规范 HTML、FTS、资源与备份代码 | 提炼到独立 core crate，废弃 NSTextView UI |
| 笔记本/标签 | 当前 GPUI 无；旧 schema 也未形成完整组织模型 | 新增正式实体和 repository |
| 全库搜索 | 旧 core 有基础 FTS，GPUI 未接入；无过滤/OCR/PDF | 扩展并接入统一 SearchService |
| 同步 | GPUI 无；旧 Node/Joplin sidecar 与新目标无关 | 新建 Rust 协议、客户端 outbox 和 NAS 服务 |
| 迁移 | 旧代码有 HTML/RTF 局部迁移，未形成用户全库导入 | ENEX/Joplin staging 导入作为产品发布门槛 |

## 5. 阅读边界与后续查证

本映射已经覆盖新版路线图所需的核心产品链。进入每个执行阶段时，只针对该阶段继续深读对应源码和交互，不再笼统声称“逆向 Evernote”：

- R1 深读 selection/navigation、编辑 session 和 image/list 边界；
- R2 深读 query parser、list projection、notebook/tag/trash mutators；
- R3 深读 importer/exporter、find、image resize 与附件视图；
- R4 深读 lifecycle、mutation rollup、resource transfer 和错误恢复。

每项采用结论必须落到测试名、真实 Release 操作序列和产品验收证据，不能仅在研究文档中打勾。
