# Joplin Lite：Evernote 核心行为与 HTML 持久化设计

日期：2026-09-07
状态：已批准方向，进入分阶段实施
目标用户：单人自用，已有多年 Joplin/Evernote 使用习惯

## 1. 这次纠偏解决什么

Joplin Lite 的价值不是“顺手写一点文字”，而是用更轻、更稳、更容易找回内容的原生客户端，替代现有 Joplin 桌面端的核心日常路径，同时继续兼容 Joplin Server、Android 客户端和现有资料。

此前原生 MVP 用 `body_rtf` 保存格式，虽然实现快，但它把不可读的二进制缓存变成了格式事实源。这个选择只方便实现，不利于检查、索引、迁移、同步和长期维护，必须纠正。

本设计作出三项不可回退的决定：

1. 正文的唯一事实源改为 UTF-8 HTML 片段，并按 Joplin `markup_language = 2` 语义处理。
2. RTF 只允许作为旧版一次性迁移输入；新版本不再写入 RTF，迁移成功后清空旧值。
3. 参考 Evernote 的核心业务闭环和视觉层级，不复制它的 Electron、Web 编辑器、AI、任务、日历、协作或订阅功能。

## 2. 对 Evernote 11.32.5 的观察

### 2.1 本地安装与静态结构

在本机安装并校验了官方 `Evernote.app` 11.32.5，签名和公证有效。以下数字只代表本机这次测量：

- 应用包约 906 MB；
- 使用 Electron 37.6.0；
- 冷启动到欢迎页时存在主进程、GPU、网络和两个 renderer 等 6 个进程；
- 这 6 个进程当时合计 RSS 约 1.2 GiB。

本地 `app.asar` 的公开打包内容显示：

- `@evernote/common-editor` 自称 browser based editor；
- 编辑器建立在 ProseMirror、React 和 HTML/XML serializer 之上；
- DOM 根节点仍是 `en-note`，内部存在 ENML/XML、XHTML entity 和 serializer 处理；
- 本地层使用 SQLite/Conduit，并有离线搜索、附件搜索文本和同步状态迁移；
- 编辑器、存储、同步是分层的，而不是把显示结果直接当数据库。

结论：Evernote 值得借鉴的是结构化编辑、稳定的本地事实源、搜索权重和低摩擦工作流；不值得复制的是浏览器运行时和不断扩张的功能面。

### 2.2 只保留与笔记本质相关的行为

根据当前官方帮助和本地可见结构，Joplin Lite 只借鉴以下核心行为：

- 新建笔记立即进入正文，可持续输入，不弹出选择流程；
- 标题和正文独立，标题匹配在搜索中优先；
- 编辑时显示必要工具，选中文字时提供就地格式入口；
- 图片和附件在正文光标处出现，而不是脱离上下文堆在底部；
- 笔记列表提供标题、摘要和更新时间，选择态明确但不刺眼；
- 搜索覆盖标题、正文、标签、附件文本和 OCR，结果说明命中位置；
- 桌面端离线可创建、编辑和搜索，联网后再同步；
- 同步失败不阻塞写作，但状态、积压、错误和重试必须看得见。

明确忽略：AI 编辑、任务、日历、模板、团队空间、协作评论、订阅推广、公开发布、网页剪藏扩展、会议录音和社交功能。

## 3. 产品边界

### 3.1 私人可替代 MVP 必须具备

1. 新建、标题、正文、自动保存、软删除、退出重开恢复；
2. 默认所见即所得，支持段落、三级标题、粗体、斜体、下划线、列表、引用、代码、链接；
3. 图片粘贴和拖入可在光标处显示，普通文件可作为附件块插入；
4. 最少的笔记本和标签管理；
5. 标题优先的中文全文搜索，以及附件/OCR 文本入口；
6. Joplin Server 同步、离线队列、冲突副本和明确错误状态；
7. 与官方 Joplin Android 客户端互通；
8. 可读导出和数据库备份，任何时候都不把用户锁在私有二进制正文里。

### 3.2 暂不进入 MVP

- Markdown 源码编辑模式；
- 插件系统和主题市场；
- 块数据库、白板、看板、日程和任务中心；
- 多人权限、评论、共享空间；
- AI 写作、云端语义问答；
- 完整复刻 Evernote 每一种内容块和快捷命令。

## 4. 正文格式：可读的语义 HTML

### 4.1 唯一事实源

`notes.body` 保存 UTF-8 HTML fragment，不保存整页 `<html>` 外壳。新笔记和迁移后的笔记设置 `markup_language = 2`。`body_text` 是从 HTML 派生的纯文本搜索投影，可以随时重建，不是第二份正文。

示例：

```html
<h1>庭审提纲</h1>
<p>核对<strong>合同原件</strong>与付款记录。</p>
<p><img src=":/0123456789abcdef0123456789abcdef" alt="转账截图.jpg"></p>
```

它必须满足：

- 人可以直接读取；
- SQLite、命令行和其他程序可以解析、索引和导出；
- 图片和附件只引用 32 位资源 ID，二进制仍保存在内容哈希 blob 中；
- 不允许 data URL、base64 图片、RTF、序列化对象或平台归档进入正文；
- 标题仍是独立字段，不从正文第一行推断或覆盖。

### 4.2 首批允许的元素

首批白名单为：

- 块：`p`、`h1`、`h2`、`h3`、`ul`、`ol`、`li`、`blockquote`、`pre`、`hr`；
- 行内：`strong`、`em`、`u`、`s`、`code`、`a`、`br`、`img`；
- 属性：链接的 `href`，图片的 `src`/`alt`，以及少量由我们定义且可移除的 `data-joplin-lite-*` 属性。

不把字体家族、任意字号、任意颜色和复杂 CSS 作为首批语义。粘贴进来的富文本先规范化；未知标签保留其可见文本，危险 URL、脚本、事件属性和远程内嵌内容全部丢弃。应用不会执行 HTML。

### 4.3 结构模型与原生编辑器

AppKit `NSTextView` 继续负责输入法、光标、选区、撤销、拼写和无障碍；Rust 负责结构模型、HTML 解析/序列化和数据事务。

```text
HTML fragment
    ↓ parse + sanitize
Rust document model
    ↓ render
NSAttributedString / NSTextAttachment
    ↓ user edits
Rust document model
    ↓ deterministic serialize
HTML fragment
```

保存不能调用 Cocoa 的整段 RTF 或通用 HTML 导出作为事实源。序列化必须由我们控制，输出稳定、紧凑、可测试的语义 HTML。相同文档模型重复保存应产生相同字节。

### 4.4 图片与附件

- 图片节点使用 `<img src=":/<resource-id>" alt="<原文件名>">`；
- 文件附件后续使用普通链接或明确的附件块，但仍只引用资源 ID；
- Finder 文件复制优先读取原始 file URL，不能退化成 Finder 提供的文件图标预览；
- 资源入库、正文变更和搜索投影在同一数据库事务中提交；
- blob 使用 SHA-256 寻址，提交前校验真实编码、大小和 regular-file 安全约束；
- 保存正文不会重复写入图片二进制。

## 5. 从 RTF 一次性迁移

### 5.1 数据安全边界

升级到 HTML 前先使用 SQLite backup API 创建带版本和时间戳的数据库快照。资源 blob 不改名、不移动。

迁移流程：

1. 读取旧 `body`、`body_text`、`body_rtf` 和资源引用；
2. 用旧版已经验证的 AppKit 路径只读解析 RTF，并按旧 `body` marker 恢复附件位置；
3. 转成 Rust document model，再序列化成规范 HTML；
4. 重新解析生成的 HTML；
5. 核对标题、可见文本、资源 ID 顺序和数量；
6. 全部笔记核验成功后，在一个事务中写入 HTML、`markup_language = 2`、重建 `body_text` 并清空 `body_rtf`；
7. 任意一篇失败则整体回滚，保留旧数据库和备份，不做部分迁移。

迁移不改变 `created_time` 和用户可见的 `updated_time`。新版代码不得再创建非空 `body_rtf`。旧列先保留为空，经过一个稳定版本后再单独删除，避免把高风险 schema 变化和内容转换绑在一次升级里。

### 5.2 失败处理

迁移失败时明确显示失败笔记 ID 和可复制的脱敏原因，不记录正文内容。应用不静默覆盖，也不假装升级成功。旧版可执行文件和迁移前备份仍可恢复使用。

## 6. Evernote 式核心交互，Byword 式克制界面

### 6.1 信息结构

桌面宽屏采用三栏，但默认保持安静：

1. 160–190 pt 导航栏：全部笔记、笔记本、标签、废纸篓，低频区可折叠；
2. 260–320 pt 笔记列表：搜索、新建、标题、两行摘要、更新时间；
3. 编辑区：最大阅读宽度约 720 pt，标题、轻量工具栏、正文和状态。

窗口较窄时先收起导航栏，再让笔记列表变成可切换面板；正文宽度和光标位置不能跳动。选中笔记使用低饱和强调色，不用大面积高亮卡片。主画布使用接近纸张的系统背景、清晰排版和足够留白，不加渐变、玻璃、装饰卡片或营销元素。

### 6.2 新建与保存

- 新建按钮和 `Command-N` 都立即创建本地草稿并聚焦正文；
- 标题框始终可直接编辑，保存后不能回退成“无标题笔记”；
- 输入采用短延迟自动保存，切换笔记、关闭窗口和退出前强制刷盘；
- 状态只显示“正在保存 / 已保存 / 保存失败”，正常时降低视觉权重；
- 保存失败保留编辑内容和恢复副本，不禁用继续输入。

### 6.3 格式工具

- 默认只显示 `Aa`、粗体、斜体、下划线、列表、链接和插入；
- 选中文本时可显示紧凑浮动工具条；
- `+` 只列核心插入项：图片、文件、分隔线、引用、代码块；
- 所见即所得是唯一默认编辑方式，不要求用户理解 Markdown；
- 图片可在光标处与文本前后排列，选中图片后再提供对齐、宽度和删除，不常驻复杂面板。

## 7. 搜索

搜索不是简单的 `LIKE` 过滤，而是本地可重建索引：

- 字段：标题、正文、标签、笔记本、附件文件名、附件提取文本、图片 OCR、PDF 页文本；
- 排序：标题精确/前缀命中最高，其次标签和正文，再结合更新时间轻量衰减；
- 中文：至少支持连续字符和前缀匹配，不依赖空格分词；
- 结果：显示标题、摘要、更新时间和命中来源；OCR/PDF 命中必须标明附件及页码；
- 输入时提供本地建议，回车进入完整结果；
- 索引损坏或升级可以重建，永远不影响正文保存。

首个可用版本先用 SQLite FTS5 加中文连续字符索引；数据量和评测证明需要后再引入 Tantivy。语义向量搜索属于多模态增强阶段，不能替代可靠的文字精确搜索。

## 8. 同步

主程序保持原生 Rust/AppKit，不引入 WebKit。为避免重写 Joplin 同步、E2EE 和冲突语义造成数据风险，第一阶段继续使用固定版本的官方 `@joplin/lib` sidecar，但改成按需同步进程：

- 本地编辑先提交，网络永远不在保存事务里；
- sidecar 只在同步或迁移时启动，任务完成后退出，不作为常驻 UI 运行时；
- Rust 持久化同步队列、最后成功时间、当前阶段和脱敏错误；
- 同一 canonical profile 只有 sidecar 写同步字段，避免双写；
- Joplin Server 是唯一可写同步目标，旧 WebDAV 只作为迁移期只读备份；
- 冲突保留双方笔记，显示设备和修改时间，不静默合并；
- 附件上传用资源 ID、大小和哈希核验，服务端未确认前不清理本地 blob。

待固定 fixtures、Android 双向互通、E2EE 和故障注入全部通过后，再逐模块评估 Rust 替换；“Rust 客户端”不等于必须立刻重写成熟同步协议。

## 9. 分阶段交付

### 0.3.1（已完成）

- 原生 Rust/AppKit 本地笔记 MVP；
- 标题、正文、自动保存、搜索、软删除；
- 图片粘贴/拖入和内容哈希 blob；
- 修复 Finder 复制图片时误存文件图标预览。

### 0.4：HTML 事实源

- 引入 Rust document model 和规范 HTML parser/serializer；
- 一次性 RTF 迁移和备份；
- 新保存不再写 RTF；
- 保留现有标题、格式、图片、撤销和重启能力。

### 0.5：核心界面与搜索

- 三栏/窄窗自适应结构；
- Evernote 式标题、摘要、时间和低干扰格式工具；
- 三级标题、列表、引用、代码、链接；
- 标题优先中文搜索和可解释命中摘要；
- 最少的笔记本、标签和废纸篓入口。

### 0.6：私人同步 MVP

- 连接已部署的 Joplin Server；
- 按需 sidecar、离线队列、重试、冲突和同步状态；
- 隔离资料库迁移、官方 Android 双向编辑和附件校验；
- 达到私人日常替代条件后，再由用户决定切换范围。

### 0.7：多模态搜索

- 图片 OCR；
- PDF 原生文本和逐页 OCR；
- 后台可暂停、按哈希增量重建的索引；
- 文字精确搜索稳定后再加入本地语义检索。

## 10. 验收底线

- HTML 文件可直接读取，数据库中抽查不得出现新的非空 RTF；
- HTML → document model → HTML 字节稳定，文本和资源引用不丢；
- 迁移前自动备份，故障注入后能回滚；
- 新建、标题、正文、B/I/U、图片、撤销、重启全部真实 UI 验收；
- 应用包和动态链接不含 WebKit、JavaScriptCore、Electron 或常驻 Node；
- 搜索对中文标题、正文和附件名有固定评测集；
- 同步上线前完成断网、超时、重复请求、服务重启和冲突测试；
- 用户数据的迁移与最终是否替换现有客户端，始终由用户决定。

## 11. 参考

- Evernote Note editor and editing toolbar overview: https://help.evernote.com/hc/en-us/articles/360022954093-Note-editor-and-editing-toolbar-overview
- Evernote Search overview: https://help.evernote.com/hc/en-us/articles/360040282613-Search-overview
- Evernote Access notes offline: https://help.evernote.com/hc/en-us/articles/209005917-Access-notes-offline
- Evernote Export as ENEX or HTML: https://help.evernote.com/hc/en-us/articles/209005557-Export-Notes-and-Notebooks-as-ENEX-or-HTML
- Joplin Rich Text editor and HTML note behavior: https://joplinapp.org/help/apps/rich_text_editor/
- Joplin import/export, including ENEX as HTML: https://joplinapp.org/help/apps/import_export/
