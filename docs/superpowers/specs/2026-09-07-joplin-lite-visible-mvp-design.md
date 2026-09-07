# Joplin Lite Native 可见 MVP 设计

## 目标

把现有原生工程构建从“能保存富文本的技术样机”提升为一眼可辨、日常可用的笔记软件。目标用户是从 Evernote 迁移到 Joplin 多年的单人用户：已有约 1,600 条长期笔记，主要任务只有快速新建、舒服书写、看到附件、从列表辨认并找回笔记。

Evernote 是业务与界面的主要原型，Byword 只补充长文写作区的克制感。核心窗口直接复刻 Evernote 的导航、卡片式笔记浏览和编辑纸张结构，但使用自己的名称、图标和实现；不复制 Evernote 的任务、日历、模板、AI、协作、订阅和插件等扩展功能。

## 当前问题

- 左栏是一次性创建全部行的 `NSStackView`，既不像成熟笔记列表，也不适合未来载入上千条笔记。
- 笔记行只有标题和纯摘要，没有更新时间和首图缩略图；选中态占用过多高饱和蓝色。
- 编辑区直接铺在窗口背景上，标题、格式条、正文和状态之间没有稳定的阅读层级。
- 永久可见的 B/I/U 文字按钮像开发样机；没有标题和列表等块级语义，用户很难感知这是所见即所得编辑器。
- 每次击键都在主线程立即执行 SQLite 事务，虽然当前数据量小，但会损害长笔记的输入手感。

## 选择

采用纯 AppKit 增量改造：`NSCollectionView` 虚拟化卡片、`NSTextView` 原生编辑、Rust `text-document` 编辑会话、应用自有 canonical `Document` 和 SQLite 中的规范 HTML。拒绝 WKWebView/ProseMirror 路线，因为它重新引入 WebKit 内存与焦点/剪贴板边界；也不在本轮重写成 SwiftUI/TextKit 2，因为会扩大风险且不增加用户价值。

## 从 Evernote 实现中实际采用的机制

本机 Evernote 11.32.5 的 `@evernote/common-editor` 源映射与只读本地 schema 显示，它没有把浏览器的富文本 DOM 直接当成笔记文件，而是使用 ProseMirror 语义树、命令事务、独立 ENML 解析/序列化器和资源记录。当前客户端还以 Yjs `internal_rteDoc` 承载实时编辑对象；SQLite 的 `Nodes_Note` 保存标题、摘要、内容散列、缩略图选择和同步状态，独立的离线正文表进入 FTS，附件表保存 MIME、大小、散列及父笔记关系。经典 ENML 仍是可读的序列化/兼容出口，但已不是当前客户端唯一的实时内部表示。我们的原生版本不复制其 Electron/React/ProseMirror/Yjs 技术栈，也不采用二进制正文，但明确复用以下架构原则：

- **语义模型先于视图**：正文由块、行内标记和资源节点构成；`NSTextView` 的字体、前缀和附件只是渲染投影。保存时从语义属性还原 Document，再由唯一序列化器生成规范 HTML。
- **命令是事务，不是直接改外观**：工具栏先查询当前选择的命令状态，再以一次编辑事务完成标题、列表、标记、对齐或缩进，形成一个清晰的撤销边界。列表按钮再次作用于同类列表时退回普通段落。
- **显示格式与存储格式分层**：同一套 schema 分别负责编辑显示和规范 HTML；清单方框、缺图提示等视图辅助字符不进入正文。
- **资源独立于正文**：图片正文节点只保存稳定资源标识和替代文字，二进制、MIME、散列和文件名由资源表管理。粘贴流程先持久化资源，再把资源节点作为同一编辑事务插入，失败不留下伪占位。
- **可靠保存有两个阶段**：每次真实用户更改立即进入 dirty 状态，同时合并短时间内的连续输入；只有规范 HTML 确实变化才写库。切换笔记、关闭窗口、图片插入和格式命令前强制 flush，异步图片处理未完成时不能误报“已保存”。
- **编辑上下文独立同步**：光标、选择、活动格式和 undo/redo 状态只在实际变化时刷新工具栏，避免每次输入重建整条工具栏。
- **版心由布局状态计算**：编辑区按可用宽度动态计算居中版心和左右留白，窗口缩放采用短 debounce；底部保留额外滚动空间，使最后一行也能滚到视线中央。
- **大文档只处理视口附近内容**：编辑区跟踪可见范围，卡片栏只创建可见卡片并只解码可见首图；任何全量工作都留在轻量文本/元数据层。

这些是后续 Task 2–4 的实现约束，不是调研备注；若代码绕过语义 Document 直接序列化 Cocoa 富文本、把附件塞进正文二进制、或每次击键重建工具栏/列表，都视为架构回归。

## Lapce/Floem 取舍

Lapce 与 Floem editor-core 是本轮明确检查过的 Rust 原生编辑器参考。它们的优势是 `Rope`/`RopeDelta` 大文本增量修改、revision 与 pristine 状态、撤销分组、选择/IME、可见行布局，以及不进入正文的 phantom text。我们采用 revision/pristine、单命令单 undo group 和 projection-only 辅助内容这三项设计。

但不直接依赖 Lapce/Floem editor-core：其 `Document` 事实源是纯文本 `Rope`，逐行 styling 与 phantom text 主要服务代码编辑，并不提供可持久化的标题、列表、链接、图片资源等富文本 schema。Floem 还会引入自定义 winit/wgpu 渲染和输入链，等于放弃已经可用的 AppKit 中文输入、系统文本服务、辅助功能和剪贴板集成。我们的组合保持为 Rust 语义 Document + AppKit `NSTextView` 投影；只有将来单条笔记规模证明 `NSTextStorage` 成为真实瓶颈时，才单独评估 Rope 增量存储，不以猜测替换稳定链。

## Velotype/GPUI 取舍

Velotype 是本轮明确审过源码的 Rust 原生编辑器参考，核对基线为 v0.7.2 / `ed65977be94f2f2703037fcb8b6cbab2e7579571`。它使用 GPUI 实现原生 block tree、块内 `EntityInputHandler`、UTF-16/UTF-8 输入位置转换、IME marked range、跨块选择/复制/删除、撤销快照、Markdown source mapping、图片/表格/代码块运行时和主题 token，证明全 Rust 自绘块编辑器并非概念演示。我们采用其文档树结构操作边界、跨块行为测试、未知语法保留和主题 token 分层作为实现参考。

本轮不 fork Velotype，也不引入 GPUI。它的产品边界是单个 Markdown 文件/工作区编辑器，不包含本项目的笔记库、缩略图列表、SQLite/FTS、资源资料库和可靠同步；其持久事实源仍是 canonical Markdown，而本项目明确选择可读、可索引的 canonical HTML。其跨块选择、焦点与输入法链和 GPUI block runtime 紧密耦合，并非可独立复用的富文本 crate；上游路线图仍列出“更完善的 IME 功能”和“内置图床”。对中文日常记录而言，替换 TextKit 会把已由系统解决的输入、选区、辅助功能和文本服务重新变成本项目责任。若 MVP 后真实测量证明 TextKit 无法满足已经批准的交互，再以独立原型比较 GPUI，不在当前资料格式或数据库上制造迁移。

## Obsidian 格式参考边界

Obsidian 只作为“日常数据库与可读导出可以并存”的产品参考，不作为 Markdown-first 约束。我们的正文可以导入导出 Markdown，但内部格式必须优先服务所见即所得、图片资源、列表/清单、稳定索引和同步事务；不能为了保持纯 Markdown 而牺牲用户可见效果或数据模型。

## Matrix Rich Text Editor 取舍

Element 的 Matrix Rich Text Editor 是本轮进一步检查的 Rust 富文本参考。其核心约 2.8 万行 Rust，已经实现 UTF-16 选区、DOM range 定位、粗体/斜体/下划线/删除线、链接、嵌套有序/无序列表、缩进、回车/退格边界、菜单 action state 和撤销/重做，并为浏览器及办公软件 HTML 粘贴准备了大量回归样本。这些正是 Task 2 最容易凭直觉写错的编辑算法；实现与测试必须对照其公开行为和反例，尤其是跨节点选区、局部链接、空列表项退出、嵌套列表残余和一条命令一个历史状态。

本轮不把 `wysiwyg` crate 直接作为运行时依赖，也不复制其源码。上游 README 明示项目仍处早期、次版本可能破坏 API 且可能出现崩溃；当前版本面向 Matrix 消息编辑器，没有笔记所需的图片资源节点、标题层级、清单状态、块对齐和高亮语义，直接接入会与本项目已经定义的 canonical HTML `Document` 形成第二套事实源。它采用 AGPLv3/商业双许可，虽然本项目同属开源路线，算法参考仍以 clean-room 行为对照和自行实现为界，避免无意引入额外来源义务。若未来上游稳定并补齐笔记语义，可重新评估替换内部范围变换层，而不是替换 AppKit 输入/排版层或 SQLite 正文模型。

## Teksilo 与 text-document 取舍

`teksilo-preview-ui` 只是 Teksilo 控件目录的三栏预览器，不是应用 GUI 底座。完整 Teksilo 确实提供纯 Rust retained tree、AccessKit、winit/wgpu 和现成 `RichTextEditor`，但当前版本仍为 0.9.x，官方明确声明 0.x 会有破坏性变化，生产部署也主要限于作者自己的应用。它的 40 多个 crate、自绘输入/渲染链和 JetBrains Int UI 默认风格都不是当前 macOS 单人笔记 MVP 的必要成本，因此本轮不迁移 AppKit 外壳，也不依赖 `teksilo-preview-ui` 或 `teksilo-widgets`。

采用其已经独立稳定到 1.x 的 MPL-2.0 `text-document` 作为**瞬时富文本编辑模型**。它提供 Rope 文本、块/列表/图片锚点、格式区间、cursor mutation、查找替换、复合 undo/redo 与增量事件，能替换本项目原计划自行编写的大部分范围变换和历史算法。应用自有 canonical `Document` 仍是持久化边界；`text-document::TextDocument` 只存在于打开笔记的编辑会话内，不能成为第二种磁盘正文格式。

实测门禁已经确认：中文、emoji、链接格式、查找、undo/redo 以及 `jln-resource://sha256/...` 外部图片引用可正常导入并往返；但 `text-document` 1.12.1 的全量 HTML exporter 会把内部已有 indent 的嵌套列表序列化成同级 `<li>`。因此禁止用 `TextDocument::to_html()` 直接写 `notes.body`，也禁止把图片资源 bytes/base64 放入其 resource table。Task 2 必须实现显式双向 adapter：canonical `Document` ↔ `TextDocument` flow/format snapshot；保存仍统一走现有白名单 serializer。嵌套层级、清单 marker、对齐、缩进和图片资源 id 均由 adapter 显式映射并做等价往返测试。

AppKit `NSTextView`/TextKit 继续负责系统级输入、IME marked text、选区、拼写、无障碍和排版；活动 `TextDocument` 负责已提交文字与语义命令。IME 组合态只停留在 TextKit 投影中，composition commit 后再以最小 UTF-16 delta 转换为 Unicode scalar range 写入 `TextDocument`；格式命令直接作用于 `TextCursor`，再增量刷新 AppKit 投影。AppKit 自身不再作为持久语义来源，两个 undo 栈不得同时接管同一次编辑。

因此编辑器组合固定为：AppKit `NSTextView`/TextKit 负责原生交互；`text-document` 负责活动编辑事务；应用自有 Rust `Document` + `html5ever` 负责安全、确定的持久语义与 HTML；SQLite/FTS5 负责事务与索引；Teksilo RichTextEditor、Matrix RTE、Lapce/Floem 和 Evernote 提供实现与行为参考。选择现成组件的标准是减少产品风险，而不是追求依赖数量。

## Iced、Slint、Loro 与 cosmic-text 取舍

用户提出的四项 Rust 技术已经按当前官方实现重新核对。本项目作出以下决定：

- **Iced 不作为 GUI 底座。** Iced 0.14 的 `text_editor` 是以纯字符串/行和编辑 action 为中心的多行输入部件，不是持久富文本编辑器；默认运行时还会引入 winit 与 wgpu/tiny-skia 自绘链。它适合真正需要 Windows/Linux 同构界面的应用，但替换 AppKit 后，macOS 中文输入、文本服务、辅助功能、拖放和附件命中测试都会重新成为本项目的维护责任。
- **Slint 不作为 GUI 底座。** Slint 1.17 的 `TextEdit` 仍以单个字符串和整块字体属性为主；官方 StyledText 工作明确只覆盖富文本显示，不覆盖用户编辑，完整 rich text editor 追踪项仍未完成。它的 winit + FemtoVG/Skia/software renderer 更适合嵌入式或自绘跨平台 UI，不适合本轮要求的 Evernote 级 macOS 写作手感。
- **cosmic-text 不替换 TextKit。** cosmic-text 0.19 已经是优秀的 Rust 字形塑形、双向文本、换行、字体回退、点击定位和基础编辑引擎，也能正确覆盖简体中文与彩色 emoji；但它不是完整的原生控件、富文档 schema、输入法客户端、拼写/无障碍系统或附件编辑器。在 macOS 上接入它会重复 CoreText/TextKit 已经可靠提供的能力。若未来开发 Linux 客户端，可把它作为那一端的文字布局候选，而不改变共享 Rust `Document`。
- **Loro 选为原生客户端下一阶段同步核心。** Loro 1.x 是 MIT 许可的 Rust local-first CRDT，已有富文本 marks、稳定光标、可移动树/列表、增量更新、快照、校验、undo manager 和 Rust/Swift 绑定，正面解决断网编辑、乱序/重复更新与多设备合并。它比把 Yjs 带回 WebKit 更符合本项目方向。

Loro 不进入当前可见 MVP 的 GUI/编辑热路径，避免推迟用户验收；Task 2 仍以 deterministic `Document` transaction 为边界，使后续 Loro adapter 能消费同一组语义操作。可见 MVP 通过后，单独实现并验证以下同步结构：

1. 一个 `library:<vault-id>` Loro room 保存笔记/笔记本/标签的目录、删除墓碑、版本和资源清单；每条笔记使用 `note:<note-id>` room 保存标题、块顺序、块属性与富文本 marks。
2. 图片和其他二进制不进入 CRDT 文本，继续按 SHA-256 存储；Loro 只同步资源标识、MIME、大小和引用。NAS 提供幂等的哈希 blob 上传/下载与垃圾回收保留期。
3. SQLite `notes.body` 中的规范 HTML 与 `body_text`/FTS 始终是可读、可搜索的物化当前态。Loro snapshot/oplog 允许是二进制，但只能作为带版本和校验的同步元数据；导入更新后必须在一个本地事务中重新物化 HTML、资源关联和 FTS。损坏或缺失 Loro 元数据时，可从 HTML 重建新同步历史，不能丢正文。
4. NAS 可复用 Loro protocol v1 的 Rust WebSocket server 和 SQLite persistence 起步，但官方协议明确不处理 collection-level synchronization，因此资料库清单、鉴权、设备撤销、资源传输、备份和可观测重试仍由本项目薄层负责，不能把“用了 CRDT”误报为“同步已经可靠”。
5. 现有 Joplin Server/PostgreSQL 部署继续作为迁移与恢复轨道，不在 MVP 期间拆除。Loro 原生同步必须先通过双/三副本中文与 emoji 并发编辑、格式/列表冲突、离线重连、重复乱序帧、快照压缩、资源中断续传、1,600 笔记规模和 NAS 恢复演练，才允许成为默认同步；官方 Joplin 客户端不理解 Loro 历史，因此长期兼容方式是受控导入导出/迁移桥，而不是两个协议同时双向写同一资料库。

这一决定保留了真正有价值的现成能力：macOS 端使用系统原生编辑栈，跨设备合并使用 Rust CRDT；两者通过本项目可读的语义文档层连接，而不是让 GUI 框架或同步日志反过来定义笔记格式。

## 信息架构

窗口采用 Evernote 式三栏骨架，默认尺寸 1380×820 pt；窄窗口允许导航栏压缩，但不删掉新建和笔记浏览：

1. **导航栏（176–208 pt）**：搜索、绿色“新建笔记”和“笔记”入口；底部只放本地资料状态。没有实现的任务、日历、模板、协作和 AI 不出现。
2. **笔记浏览栏（360–400 pt）**：顶部显示“笔记”和数量，下方为双列虚拟化卡片。
3. **编辑区**：浅灰工作区上放一张白色圆角编辑纸张，正文宽度限制在约 680 pt；标题、时间、格式区和正文左边缘严格对齐。

笔记卡片宽约 168–184 pt、高度按 1:1.35 的稳定节奏排列，包含最多两行标题、两行摘要、可选首图缩略图和短日期。无图时文字自然占用卡片空间，不显示伪缩略图；只有附件而无可预览图片时可显示克制的附件符号。普通卡片使用细边框和极浅底色，不堆阴影；选中卡片使用细绿色描边，与左栏绿色新建按钮形成唯一强调色。

## 编辑体验

- 标题 30 pt、正文 17 pt，正文行距和段间距由统一段落样式控制；中文长文保持约 45–65 个汉字的舒适行宽。
- 编辑纸张顶部用轻量路径/标题栏承载独立标题，下面只显示短更新时间；“已保存”沉到纸张右下角，失败才使用红色。
- 按用户提供的 Evernote 截图，在标题路径下方固定一行格式工具条；窗口变窄时把低频命令收进“更多”，不使用遮挡正文的常驻浮动条。
- 第一层提供：插入图片、撤销、重做、正文/标题 1/标题 2/标题 3、粗体、斜体、下划线、高亮、项目符号、编号列表和清单。“更多”只列已经实现的链接、左/中/右对齐、增加/减少缩进、删除线和清除格式。所有可见按钮必须真实工作，不放 AI、分享、提醒、协作或其他未来功能占位。
- 正文为空时显示“开始写作”，而不是把教学说明塞进画布。
- 新建后正文立即获得焦点；标题可独立编辑和保存；`Command-N`、撤销/重做、Finder 图片复制粘贴和拖入保持不变。
- 自动保存改为短延迟合并（目标 250–400 ms），切换笔记、关闭窗口、插入图片及显式格式操作前强制落盘。界面即时更新，数据库失败时保留编辑内容并显示明确状态。

## 文档语义

`notes.body` 继续是唯一事实来源，使用 UTF-8 规范 HTML：

- 段落：`<p>`
- 标题：`<h1>`、`<h2>`、`<h3>`
- 项目符号：`<ul><li>`
- 编号列表：`<ol><li>`
- 清单：`<ul data-type="checklist"><li data-checked="true|false">`
- 行内：`<strong>`、`<em>`、`<u>`、`<s>`、`<mark>`、仅允许 `https://`、`http://`、`mailto:` 的 `<a>`、`<br>`、本地资源 `<img>`
- 段落对齐与缩进使用白名单化的规范属性，不接受任意 CSS。

Document 模型显式表示段落、三级标题、两类列表项和清单状态。AppKit 渲染使用语义字体与段落样式，并用应用自有属性保存块类型；保存时只提取受支持语义，不序列化字体家族、字号、任意颜色或 Cocoa 二进制对象。列表前缀和清单控件属于视图投影，不进入正文文本节点；回车延续列表，空列表项回车退出列表。

旧 0.3.x RTF 迁移逻辑保持冻结，只产生段落、行内 B/I/U 和图片，不臆测标题或列表。

## 缩略图与性能

从规范 HTML 中只取第一张本地图片作为候选缩略图。笔记浏览栏使用 `NSCollectionView` 的可复用卡片，只为可见卡片解码缩略图；缩略图按 resource id 和目标尺寸缓存。损坏或缺失资源退化为无图卡片，不能阻塞列表或改写正文。

搜索结果复用同一列表，不创建第二套 UI。首批载入 1,600 条仅查询笔记元数据和正文投影，不预解码全部附件。

## 视觉系统

- 单一系统字体家族，使用 11/13/17/22/30 pt 五档和 Regular/Medium/Semibold 三档。
- 4 pt 基础间距：4、8、12、16、24、32、48。
- 颜色主要来自系统动态色；浅暖灰导航/浏览背景、白色纸张、Evernote 式绿色作为唯一强调色。颜色只表达新建、选择、焦点和错误。
- 不使用渐变、玻璃效果、装饰阴影、胶囊按钮堆叠或大图标。图标优先 SF Symbols，并保留文本工具提示和键盘可达性。

## 验收

1. 打开应用两秒内能分辨新建、笔记列表和写作区域；空资料库也有明确下一步。
2. 新建后可输入中文/emoji，应用标题、B/I/U、高亮、删除线、三级标题、项目符号、编号、清单、链接、对齐与缩进；保存重启后视觉和语义一致，编辑器不显示 HTML 标签。
3. 含首图笔记在列表出现正确缩略图；无图、缺图、损坏图正常退化。
4. 1,600 条合成笔记滚动卡片时不预创建 1,600 个卡片控件，不提前解码所有图片。
5. 图片粘贴/拖入、旧 RTF 原子迁移、搜索、标题保存、撤销重做和无 WebKit 契约不回归。
6. 发布前只让用户看到通过真实窗口验收的构建；工程迁移窗口和中间包不再作为产品验收版本弹出。
