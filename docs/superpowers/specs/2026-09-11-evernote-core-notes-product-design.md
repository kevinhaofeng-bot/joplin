# Evernote 核心笔记产品原生复刻设计

日期：2026-09-11
状态：已确认方向，作为产品级总设计
上位目标：以尽可能低的常驻内存，做一款个人使用、离线优先、同步可靠、编辑顺手的原生笔记软件；Evernote 11.32.5 的核心产品行为是主要参照。

## 1. 纠偏与当前事实

现有 GPUI 成果是编辑器技术样机，不是笔记软件 MVP。

代码证据很直接：`packages/app-lite-gpui/src/spike_app.rs` 明确声明只创建一个原生编辑器实体，并刻意不启动 workspace、network、sync 或普通应用服务；启动时标题固定为“会议记录”，正文来自 `sample_document()` 或性能 fixture。它已经验证了一部分重要底座：结构化块、统一选区与事务、中文输入法桥、图片节点、列表命令、撤销、视口布局和缓存预算。但它尚未形成以下闭环：

- 创建一篇具有稳定 ID 的真实笔记；
- 从 SQLite 加载并保存标题、正文与附件关系；
- 在真实笔记列表中选择、切换、移动、删除与恢复；
- 管理笔记本、标签、快捷入口与排序；
- 搜索标题、正文、图片/PDF 可检索文本及过滤条件；
- 在多台设备之间可靠同步并解释失败；
- 导入现有 Evernote/Joplin 资料并完整导出、备份与恢复。

因此，旧的“Evernote 式原生编辑器内核设计”降为本设计中的一个子系统规范。任何只展示单篇示例文档的构建都不得再称为产品 MVP。

## 2. 产品定义

这款产品的意义不是“另一个能写字的 Apple 记事本”，而是把 Evernote 最有价值的个人知识库体验保留下来，同时去掉当前 Evernote 的体积、团队功能和促销负担：

1. **随手捕获**：`Cmd-N` 后立即输入；粘贴或拖入图片、PDF、文件不会打断写作。
2. **可浏览**：卡片、摘要和缩略图让旧笔记可以凭视觉记忆被重新发现。
3. **可组织**：笔记本、单层笔记本组、标签、快捷入口、回收站和排序都是真实持久化对象。
4. **可找回**：本地全文搜索、条件过滤、笔记内查找，以及图片/PDF 的后台文字提取。
5. **可离线**：所有核心操作先在本机完成，网络只负责复制已经持久化的变更。
6. **可恢复**：正文始终有可读 UTF-8 快照；数据库、附件和同步状态均可校验、备份、导出和恢复。

## 3. 范围

### 3.1 产品核心，必须进入正式验收

| 能力 | 必须具备的行为 |
| --- | --- |
| 笔记生命周期 | 新建、自动保存、重命名、复制、移动、删除、回收站恢复、永久删除 |
| 富文本编辑 | 标题、段落、H1-H3、粗斜下删、高亮、链接、项目符号、编号、清单、缩进、对齐、撤销重做、中文 IME |
| 多模态内容 | 图片粘贴/拖放/文件选择、图片缩放与对齐、PDF/音视频/普通文件附件卡、Quick Look/系统打开 |
| 图文混排 | 图片前后都能落光标和输入；插入后当前编辑区立即显示；排版不闪跳；跨块选择与删除稳定 |
| 浏览 | 三栏、两栏、一栏；卡片/摘要/紧凑列表；缩略图；排序；选择状态；返回/前进；最近笔记 |
| 组织 | 笔记本、单层组、标签、快捷入口，批量移动和批量加标签 |
| 搜索 | 标题和正文全文检索、中文子串、引号短语、笔记本/标签/日期/回收站过滤、搜索历史、笔记内查找 |
| 索引增强 | 图片 OCR、PDF 文本提取进入本地索引；索引失败不阻塞保存与同步 |
| 本地数据 | SQLite WAL、内容寻址附件、可读正文快照、版本历史、崩溃恢复 |
| 同步 | 离线队列、幂等重试、增量拉取、断点附件、冲突可见、状态可解释、NAS 自托管 |
| 迁移与退出 | ENEX 导入、现有 Joplin 数据导入、HTML/JSON/附件目录导出、全库备份与恢复演练 |

### 3.2 核心之后再评估

- 基础提醒；
- 多窗口或固定标签页；
- 移动端原生客户端；
- 浏览器剪藏扩展。

这些能力不能阻塞核心笔记应用完成，也不能预先污染数据模型。

### 3.3 明确不做

- AI 写作、AI 搜索、转录、自动摘要；
- 团队空间、权限、分享、协同评论与聊天；
- 日历、任务管理、模板市场、插件平台；
- 为兼容官方 Joplin 而保留 Node sidecar、Electron、WebDAV 或 Joplin 内部同步协议；
- 把 Markdown 当作正文能力上限；
- 把 CRDT 二进制状态当作唯一可恢复正文。

## 4. 技术方案选择

### 4.1 选择：GPUI + Rust 产品内核 + SQLite + 自托管 Rust 同步服务

继续使用已经证明能工作的 GPUI/Metal 路线，并把现有编辑器内核提升为完整应用的一部分。理由：

- 当前 GPUI 编辑器已经拥有真实输入桥、结构化块、图片节点和硬内存预算，保留它比再次重写正确率更高；
- GPUI 可以直接绘制 Evernote 式三栏、卡片、弹层和动画，不受 `NSTextView` 单一长字符串模型限制；
- 不引入 WebKit、Chromium 或常驻 Node 运行时；
- 旧 Rust 原生客户端中已经验证的 SQLite、规范 HTML、FTS、内容寻址附件、备份和迁移代码可以提炼复用，而不是丢弃。

### 4.2 未选择的两条路线

**Makepad 全量重写**：渲染和动画自由度高，但会再次重写输入、选区、块布局和图片生命周期；现在没有理由放弃已验证的 GPUI 内核。

**WKWebView + ProseMirror/Lexical**：编辑正确率的现成上限高，但重新引入 WebKit 进程、DOM/CSS 与 Rust/JS 桥，直接违背本项目最初的体积和内存目标。它保留为失败止损方案，不进入当前主线。

Iced、Slint、Xilem、`cosmic-text` 独立渲染和 teksilo-preview-ui 不再作为主框架候选。它们的可借鉴实现仍可用于具体控件或排版问题，但不触发第四次底座迁移。

## 5. 总体架构

```text
GPUI application shell
  ├─ Sidebar / Note list / Search / Inspector / Status
  ├─ NoteSession (one active editable note)
  │    ├─ TitleInput
  │    ├─ EditorCore
  │    └─ SaveCoordinator
  ├─ LibraryService
  │    ├─ NoteRepository (SQLite WAL)
  │    ├─ ResourceStore (SHA-256 blobs)
  │    ├─ SearchIndex + OCR/PDF workers
  │    └─ Revision / Import / Export services
  └─ SyncEngine
       ├─ durable outbox + pull cursor
       ├─ per-note merge state loaded on demand
       └─ resumable resource transfer

NAS Docker
  └─ Rust sync server
       ├─ authenticated HTTP/WebSocket API
       ├─ SQLite WAL entity/op store
       ├─ content-addressed blob directory
       └─ integrity scan + restic backup hooks
```

### 5.1 代码边界

新增 `packages/app-lite-core` Rust crate，承接并扩展旧 `app-lite-native` 中已经验证的纯数据代码：

- `domain`：Note、Notebook、Stack、Tag、Resource、Revision、SyncOp；
- `document`：GPUI `Document` 与规范 HTML 的双向 codec；
- `repository`：schema、query、transaction、migration；
- `resource`：内容寻址 blob 与引用关系；
- `search`：查询解析、FTS 投影、索引队列；
- `import_export`：ENEX、Joplin、HTML/JSON 导入导出；
- `sync`：协议类型、outbox 和 merge 状态。

`packages/app-lite-gpui` 只拥有界面、交互状态、打开笔记会话和缓存。它不得拼 SQL、直接写资源目录或另建一套搜索逻辑。

新增 `packages/app-lite-server` 作为个人 NAS 同步服务。协议共享类型放在 `packages/app-lite-protocol`，客户端和服务端使用同一套序列化与版本验证。

旧 `packages/app-lite-native` 的 `NSTextView` 界面不再继续开发；其 `core.rs`、`html_body.rs`、`resource_store.rs`、`note_preview.rs` 的验证成果迁入 `app-lite-core` 后保留回归测试。

## 6. 数据模型与正文格式

### 6.1 三层表达

一篇笔记同时有三种职责不同的表达：

1. **编辑表达**：打开时是 `native_editor::Document`，由稳定 NodeId、块、行内样式和单一 Selection/Transaction 管理。
2. **可读快照**：SQLite `notes.body_html TEXT` 保存确定性、规范化的 UTF-8 HTML 子集；图片和附件只保存 `note-resource://<id>` 引用。
3. **检索投影**：`notes.body_text TEXT` 和 `notes.snippet TEXT` 保存去标记可见文本，进入本地 FTS 与卡片列表。

这三层在同一个保存事务中更新。HTML 是正文恢复与导出的底线；搜索不需要每次解析正文；编辑器也不被 Markdown 语法限制。

### 6.2 同步合并状态

正文同步可以使用 Loro 保存每篇笔记的合并状态，但必须遵守：

- 只为当前打开或正在合并的少量笔记加载；
- `loro_state BLOB` 是同步加速与合并元数据，不是唯一正文；
- HTML 快照和纯文本投影与 merge state 在同一事务中落盘；
- merge state 丢失或损坏时，可以从 HTML 建立新的同步基线并保留冲突副本；
- 在实现 Loro 前，先用版本前置条件和冲突副本交付可靠单用户同步；不得让 CRDT 阻塞本地产品闭环。

### 6.3 核心表

- `notes`：标题、HTML、纯文本、摘要、笔记本、缩略图、时间、删除状态、版本；
- `notebooks`、`stacks`：笔记本与单层组；
- `tags`、`note_tags`：标签和多对多关系；
- `resource_blobs`、`resources`、`note_resources`：去重 blob、展示元数据和有序引用；
- `note_revisions`：有界的本地历史；
- `edit_journal`：崩溃前尚未压成完整快照的可读增量；
- `search_queue`、`notes_fts_latin`、`notes_fts_cjk`：后台索引与双通道查询；
- `sync_outbox`、`sync_cursor`、`sync_conflicts`：持久化同步状态；
- `shortcuts`、`search_history`、`settings`：产品状态。

数据库使用 WAL、外键、显式 schema version 和事务迁移。列表查询永远只取轻量投影，不加载 `body_html`、merge state 或附件字节。

## 7. 关键产品流程

### 7.1 新建笔记

1. flush 当前会话；
2. SQLite 事务创建稳定 NoteId、默认笔记本关系和空 HTML 快照；
3. 清除与新笔记不相容的搜索过滤；
4. 更新列表投影并选中新笔记；
5. 创建 NoteSession，按偏好聚焦标题或正文；
6. 第一笔输入进入普通事务、崩溃日志和自动保存流程。

界面上的“无标题笔记”只是空标题占位，不得写回并覆盖真实标题。

### 7.2 编辑与保存

- 每个用户事务立即把会话标为 dirty；
- 100 ms 内把紧凑 UTF-8 增量写入 `edit_journal`；
- 500 ms settled debounce 生成规范 HTML、body_text、snippet 和资源引用；
- 连续编辑最长 15 秒必须生成一次完整快照；
- 图片解码、OCR 等后台操作不能阻塞文本快照；尚未稳定的资源导入可以暂缓“已保存”状态；
- 切换笔记、关闭窗口、退出、删除、移动和显式同步前必须 flush；
- 保存完成只能在数据库事务、资源引用和编辑器 generation 一致时显示。

### 7.3 图片与附件

图片插入是一笔跨层事务：先安全导入 blob 和资源元数据，再在当前 Selection 插入 Image block，最后提交 note-resource 关系并触发当前编辑器与列表投影重绘。当前编辑区和卡片缩略图必须由同一个提交结果驱动，不能出现“缩略图先有、正文要切换后才出现”。

相邻原子块之间永远存在可点击的插入位置；点击图片前后、按方向键跨越图片、退格删除、跨块选择和撤销都使用同一文档坐标系。图片尺寸、标题、对齐、环绕和缩略图选择是节点属性，不塞进相邻文本行。

PDF、音频、视频和普通文件先以附件卡呈现，可通过系统预览或默认程序打开。转录和 AI 识别不属于核心。

### 7.4 浏览与导航

- 三栏：侧栏 / 虚拟化笔记列表 / 编辑器；
- 两栏：笔记列表 / 编辑器；
- 一栏：编辑器；
- 栏位切换使用 180–220 ms ease-out 抽拉，动画期间不重新解码缩略图、不重建编辑器、不保存中间宽度；
- 列表支持卡片、摘要、紧凑三种密度，默认卡片；
- 卡片只持有 ID、标题、摘要、时间和一个缩略图键；
- 选择、搜索、排序后以 NoteId 保持选中项，不以数组下标保持；
- 返回/前进保存导航目标与过滤条件，不复制完整文档。

### 7.5 搜索

`Cmd-K` 聚焦全库搜索，`Cmd-F` 只查当前笔记。全库查询解析普通词、引号短语和以下过滤：

- `notebook:`、`stack:`、`tag:`；
- `created:`、`updated:`；
- `is:trash`、`has:attachment`。

拉丁文本使用 `unicode61` FTS；中文和无空格语言使用 trigram 索引。保存事务立即更新标题和正文投影；OCR/PDF 文本通过 `search_queue` 后台追加。索引队列按最旧更新时间分批处理，每批最多 100 篇，每篇之间主动让出执行权；索引失败可重试且不会撤销用户内容。

搜索结果仍使用轻量卡片查询，并返回命中摘要和匹配附件缩略图。清除搜索历史时同时清除严格前缀，避免残留建议。

### 7.6 组织与回收站

笔记本、组和标签的增删改移动都先本地事务，再写入 outbox。删除笔记默认只是设置 `deleted_at` 并从普通 FTS 移除；恢复时回到原笔记本，原笔记本已删除则进入默认笔记本。永久删除只允许在回收站触发，并先记录可审计 tombstone；资源 blob 只有在无任何笔记或历史引用且超过保留期后才回收。

## 8. 同步设计

### 8.1 原则

- 本地操作永远不等待网络；
- 每一项待同步操作先和本地实体修改在同一个 SQLite 事务中持久化；
- 每个操作有全局唯一 `op_id`，服务端按 `op_id` 幂等；
- pull 使用单调 cursor，断线后从已提交 cursor 继续；
- 失败分为认证、网络、可重试服务端、永久数据错误和冲突，界面显示最后成功时间与具体状态；
- 任何重试都不得丢弃或重复用户可见修改。

### 8.2 服务端

个人版服务端使用单 Rust 进程、SQLite WAL 和内容寻址 blob 目录，运行于 NAS Docker。服务端数据库只由容器内进程访问，不把 SQLite 文件放在 SMB/NFS 客户端挂载上。

API 提供：设备注册、push 操作批次、pull cursor、资源存在性检查、分块上传、Range 下载、同步状态和完整性检查。资源块携带 SHA-256；完成上传后原子发布。服务端按实体 revision 做前置条件检查，正文 merge 状态按需合并；无法自动合并时返回双方版本，客户端创建可见冲突副本。

备份由一致性 SQLite snapshot、blob 清单和 restic 组成。恢复演练必须在隔离目录启动第二个服务端并由新客户端完成全量 pull，不能只证明备份文件存在。

## 9. 从 Evernote 源码采用的机制

本设计不是仅凭截图模仿。具体源码证据和采纳决定记录在 `docs/research/evernote-11.32.5-core-product-behavior-map.md`。核心结论包括：

- 新建笔记先生成持久实体，再导航与聚焦；
- 笔记选择与笔记本选择统一转成导航状态；
- 笔记列表使用不包含全文正文的轻量投影；
- 缩略图与搜索命中附件可独立选择；
- 本地正文保存会触发后台离线索引队列；
- 同步变更先进入持久化 mutation queue，可合并、批量、重试并区分永久失败；
- 编辑会话销毁前 flush 本地内容和待同步变更；
- 标题是编辑事务的一部分；
- 图片前后死区可插入段落；
- 列表 Enter/Backspace/Shift-Enter 是结构化事务，不是给字符串加前缀；
- 搜索解析、搜索历史、视图模式和标签/笔记本操作都是独立产品层能力。

## 10. 内存与性能预算

编辑器子系统继续遵守原规范：空白编辑器 RSS 不超过 80 MiB，典型 200 块/10 图不超过 120 MiB，图片纹理、布局与撤销缓存各自有硬预算。

完整产品新增预算：

- 载入真实规模资料库但未打开大图笔记，稳定 RSS 不超过 120 MiB；
- 打开典型 200 块/10 图笔记并显示 50 个虚拟化卡片，稳定 RSS 不超过 160 MiB；
- 1,662 篇笔记、4,238 个资源的初始列表查询不读取任何 `body_html` 或附件字节；
- 可见卡片缩略图缓存 24 MiB，与正文图片 48 MiB 缓存分离；
- 默认只保留一个完整可编辑 NoteSession；切换后旧会话 flush 并释放；
- 空库到可输入首帧目标 800 ms 内，真实资料库到列表可交互目标 1.2 秒内；
- 普通键入到绘制 p95 小于 16 ms，本地保存不阻塞 UI 线程；
- 搜索首批 50 条结果目标 100 ms 内，后台 OCR 不参与首批阻塞。

任何预算只能通过 Instruments/footprint、真实 Release 包和可重复 fixture 验证。不得通过隐藏图片、禁用撤销、少载数据或关闭正确输入来制造结果。

## 11. 产品完成定义

只有以下真实流程全部通过，才称为核心 MVP：

1. 从空库创建三篇笔记，输入中文标题和图文混排正文，重启后内容、顺序和光标语义可恢复；
2. 创建笔记本与标签，移动/过滤/批量标记后重启仍一致；
3. 搜索标题、正文、中文片段、图片 OCR 和 PDF 文本，结果摘要和缩略图正确；
4. 删除、恢复、永久删除及资源保留期行为正确；
5. 导入用户现有资料库的副本，核对笔记/笔记本/标签/资源计数和抽样视觉内容；
6. 两台客户端离线分别修改，再上线同步；无冲突修改自动收敛，真正冲突生成可见副本；
7. 上传中断的大附件能续传，重复请求不产生重复资源；
8. NAS 服务停止时仍能完整编辑，恢复后 outbox 清空且最后同步时间更新；
9. 从独立备份恢复到空环境，新客户端能全量拉取并通过哈希检查；
10. Release 产品通过内存、延迟、中文 IME、粘贴、拖放、图文边界和栏位动画验收。

在这些门槛之前，单独的编辑器、数据库测试、同步 API 或漂亮截图都只是子系统进度，不是产品完成。
