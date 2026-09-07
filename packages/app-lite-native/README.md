# Joplin Lite Native

这是一个 macOS 原生 Rust/AppKit MVP。它使用 AppKit 的 `NSStackView` 笔记列表和原生文本编辑器，不使用 Tauri、WebKit、Node.js 或 Electron。

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

脚本会生成并 ad-hoc 签名 `packages/app-lite-native/dist/Joplin Lite Native.app`，可用 Finder 打开或执行：

```sh
open "packages/app-lite-native/dist/Joplin Lite Native.app"
```

## 当前功能（0.4.0 原生 HTML）

- 原生笔记列表、实时搜索和软删除：低摩擦地接近 Evernote 的列表使用感
- 新建笔记、标题与正文编辑、自动保存；标题独立保存，不从正文首行覆盖
- 所见即所得正文：粗体（B）、斜体（I）、下划线（U）、清除样式，选区格式和重启恢复
- 正文支持段落、显式软换行、CJK/emoji 和段落内图文顺序；编辑器仍是原生 AppKit 文本编辑器
- Byword 式 720pt 舒适写作画布、动态浅色/深色系统颜色和完整空态引导
- SQLite 本地存储及基础全文搜索；搜索使用从正文派生的 `body_text` 索引
- 图片附件：正文视图支持 PNG/JPEG/TIFF 粘贴；Finder 拖入支持普通本地 PNG/JPEG
- 图片单文件上限为 10 MiB；拖入仅接受真实本地 regular file（拒绝目录、符号链接、FIFO、远程 URL、promised file 和非图片）
- 图片 blob 保存在 `<profile>/resources/blobs/<sha256>`，正文使用带本地 resource id 和 alt 的语义 `<img>` 节点
- 数据目录和 `notes.sqlite` 的符号链接防护；缺失图片在编辑器中保留可恢复的图片引用

### 正文存储格式

SQLite `notes.body` 是 UTF-8 规范语义 HTML，也是正文的唯一事实来源。它不是给用户手写的 HTML，而是由编辑器的 Document 模型生成的稳定、可读、可索引表示；标签和属性只表达当前支持的语义，不保存 Cocoa 的字体、颜色、布局等偶然属性。

`body_text` 只是从语义 HTML 派生的搜索/列表索引，不是第二份正文。`body_rtf` 只作为旧版本数据库的空兼容列保留；启动 0.4.0 时，应用会在自己的 profile 内对旧数据执行一次性迁移，成功后写入 HTML 并清空该列。正常编辑、保存和加载不会再生成或读取 RTF。

新建行为契约是：每次点击“新建笔记”都会创建一条新的草稿笔记，清空搜索条件、切换到该笔记并把焦点放到正文；空白草稿会在下次启动时清理。删除只做软删除，不物理移除历史行。

## 0.4.0 验收范围

- 真实原生界面验证了新建按钮、`Command-N`、`Command-V` 粘贴、`Command-Z` 撤销和 `Shift-Command-Z` 重做，以及中文标题/正文自动保存、标题独立保存、退出重开恢复、搜索、软删除和 B/I/U/清除样式。
- 自动化测试覆盖 HTML tree builder、规范化序列化、正文搜索投影、图片资源关联、旧数据迁移和原生编辑器编解码，并通过 `cargo fmt --check` 和 `cargo clippy --all-targets -- -D warnings`。
- ad-hoc 签名的 `.app` 通过 `codesign --verify --deep --strict`。
- App 包体约 2.4 MB；空资料库稳定后 RSS 约 46–49 MB，载入并编辑笔记后一次复测约 57 MB。
- 进程没有子进程；release 二进制的动态链接中没有 WebKit 或 JavaScriptCore。

## 0.3.1 Finder 粘贴行为（保留）

- PNG/JPEG 图片粘贴、10-MiB 上限、真实编码校验、blob 持久化与重启恢复已覆盖自动化测试和签名临时 profile smoke。
- Finder JPEG 跨应用拖入、退出重开与附件恢复已完成签名临时 profile 端到端验收。
- Finder 复制单个本地 PNG/JPEG 文件后，粘贴优先读取原始 file URL 的文件名和字节；Finder 同时提供的图标 PNG/TIFF 预览不会被误存为 `clipboard.png`。无 file URL 的 PNG/TIFF/JPEG 像素剪贴板仍按原路径导入；无效、远程、promised、多文件或超限 file URL 会拒绝且不回退到图标预览。

## 当前限制

这是聚焦写作体验的本地 MVP：尚无 Joplin Server 同步、JEX 导入导出、笔记本/标签管理、加密或更高级编辑功能。0.4.0 的格式范围限于 B/I/U、段落、软换行、PNG/JPEG/TIFF 粘贴和 PNG/JPEG Finder 拖入；不支持 GIF、SVG、HEIC、PDF、目录或远程资源，也不提供表格、列表、复杂布局或任意 HTML 编辑。当前客户端不能替代完整 Joplin 桌面端。
