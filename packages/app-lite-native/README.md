# Joplin Lite Native

这是一个 macOS 原生 Rust/AppKit 0.7.2 RC。应用采用 Evernote 风格的三栏工作区：极简导航、可复用的笔记卡片浏览栏，以及带固定工具栏的原生所见即所得编辑器。正文不经过 WebKit、Tauri、Electron 或 Node.js。

## 运行

直接运行调试构建：

```sh
cargo run --manifest-path packages/app-lite-native/Cargo.toml
```

默认数据文件为 `~/Library/Application Support/com.kevinhao.joplin-lite-native/notes.sqlite`。也可以用 `JOPLIN_LITE_NATIVE_DATA_DIR` 指定一个 profile 目录；程序会在该目录下创建 `notes.sqlite`。

## 构建可安装 App

在 macOS 上执行：

```sh
packages/app-lite-native/scripts/bundle.sh
```

脚本会在 staging 目录生成并 ad-hoc 签名 `packages/app-lite-native/dist/Joplin Lite Native.app`，随后检查图标、版本、签名和原生依赖契约。

## 当前功能

- 原生三栏工作区：搜索、新建笔记、本地笔记数量、可复用双列卡片和固定编辑器纸张。
- 卡片列表只保留轻量笔记投影；首图缩略图按可见卡片异步下采样和有界缓存，1600 条笔记的测试资料库仍只创建屏幕附近的卡片视图。
- 搜索合并 FTS5 前缀结果与中文子串结果，按“标题精确、标题前缀、标题包含、正文命中”的相关度分层，再以 BM25、更新时间和笔记 ID 稳定排序；完整结果与卡片预览共用同一排序查询。
- 非空搜索会在卡片标题和摘要中以 view-only 黄色标出大小写不敏感的 query terms；正文深处命中时摘要向命中位置派生上下文窗口，清空搜索或复用卡片不会残留高亮。
- 空标题只在卡片标题槽显示“无标题笔记”占位符；持久化标题保持为空，正文首行只出现在摘要中。
- 主菜单“搜索笔记”提供 ⌘K 快捷键：恢复可见的资料库导航后聚焦现有搜索框并选中当前查询，后续输入仍复用 `searchNotes:`。
- 标题与正文独立编辑，支持中文、emoji、输入法组合文本、撤销/重做和延迟自动保存；保存失败会保留可见内容并允许重试。
- 语义富文本支持正文、H1/H2/H3、粗体、斜体、下划线、高亮、删除线、链接、对齐、缩进、项目符号、编号列表和清单；工具栏使用原生 SF Symbols，并随窗口宽度把次要命令收进“更多”。
- 图片支持 PNG/JPEG 像素剪贴板、TIFF 剪贴板（规范化为 PNG）和 Finder PNG/JPEG 文件拖入；图片在编辑投影中是独立块，不继承标题行高，缺失资源仍保持可恢复的语义锚点，正文不存储图片 bytes 或 base64。
- 0.7.2 收口了图片粘贴即时显示、图片独立块的真实 caret 与连续插图路径；固定工具栏的 SF Symbols 不与文字叠放，并使用多尺寸绿色笔记本小鼠图标。
- 正文持久化为规范 UTF-8 HTML；`body_text` 仅用于搜索索引，旧 RTF 只用于一次性迁移，正常编辑与保存不再写入 RTF。
- 数据目录、SQLite 文件和资源 blob 有符号链接防护；删除为软删除，搜索和卡片列表共用同一数据源。

## 构建与发布契约

```sh
cargo fmt --manifest-path packages/app-lite-native/Cargo.toml -- --check
cargo test --locked --manifest-path packages/app-lite-native/Cargo.toml --all-targets
cargo clippy --locked --manifest-path packages/app-lite-native/Cargo.toml --all-targets -- -D warnings
packages/app-lite-native/scripts/bundle.sh
```

发布脚本只允许单个主可执行文件、AppIcon 和系统框架依赖；不打包辅助进程、嵌入式 Web runtime 或额外脚本运行时。

## 当前限制

这是本地写作 MVP RC，尚未提供 Joplin Server 同步、JEX 导入导出、笔记本/标签管理、加密或协作能力。图片范围限于 PNG/JPEG（旧迁移仍兼容既有 RTF 中的受支持内容）；不支持 GIF、SVG、HEIC、PDF、远程资源、表格或任意 HTML 编辑。
