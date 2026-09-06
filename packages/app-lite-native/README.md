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

## 当前功能（0.2.0 MVP）

- 原生笔记列表、实时搜索和软删除：低摩擦地接近 Evernote 的列表使用感
- 新建笔记、标题与正文编辑、自动保存；标题独立保存，不从正文首行覆盖
- RTF 所见即所得正文：B、I、U、清除样式，选区格式和重启恢复
- Byword 式 720pt 舒适写作画布、动态浅色/深色系统颜色和完整空态引导
- SQLite 本地存储及基础全文搜索
- 图片附件 MVP：正文视图支持 PNG/JPEG/TIFF 粘贴；Finder 拖入支持普通本地 PNG/JPEG
- 图片单文件上限为 10 MiB；拖入仅接受真实本地 regular file（拒绝目录、符号链接、FIFO、远程 URL、promised file 和非图片）
- 图片 blob 保存在 profile 的 `resources/<sha256 前缀>/<sha256>.<扩展名>`，正文使用规范 marker：`![alt](:/<32 位 resource id>)`
- 数据目录和 `notes.sqlite` 的符号链接防护；RTF 导出失败时优先保存正文，避免丢失最新文字

新建行为契约是：每次点击“新建笔记”都会创建一条新的草稿笔记，清空搜索条件、切换到该笔记并把焦点放到正文；空白草稿会在下次启动时清理。删除只做软删除，不物理移除历史行。

## 0.2.0 MVP 验收

- 真实原生界面验证了新建按钮、`Command-N`、`Command-V` 粘贴、`Command-Z` 撤销和 `Shift-Command-Z` 重做，以及中文标题/正文自动保存、标题独立保存、退出重开恢复、搜索、软删除和 RTF B/I/U/清除样式。
- 18 项测试通过（11 个原生客户端单元测试、7 个核心生命周期集成测试），并通过 `cargo fmt --check` 和 `cargo clippy --all-targets -- -D warnings`。
- ad-hoc 签名的 `.app` 通过 `codesign --verify --deep --strict`。
- App 包体约 2.4 MB；空资料库稳定后 RSS 约 46–49 MB，载入并编辑笔记后一次复测约 57 MB。
- 进程没有子进程；release 二进制的动态链接中没有 WebKit 或 JavaScriptCore。

## 当前限制

这是聚焦写作体验的本地 MVP：尚无 Joplin Server 同步、JEX 导入导出、笔记本/标签管理、加密或更高级编辑功能。附件导入目前只覆盖上述 PNG/JPEG/TIFF 粘贴与 PNG/JPEG Finder 拖入；不支持 GIF、SVG、HEIC、PDF、目录或远程资源。当前客户端不能替代完整 Joplin 桌面端。
