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

## 当前功能

- 原生笔记列表、实时搜索和软删除
- 新建笔记、标题与正文编辑、自动保存
- RTF 正文保存与恢复
- SQLite 本地存储及基础全文搜索

新建行为契约是：每次点击“新建笔记”都会创建一条新的草稿笔记，清空搜索条件、切换到该笔记并把焦点放到正文；空白草稿会在下次启动时清理。删除只做软删除，不物理移除历史行。

## 0.1.1 MVP 验收

- 真实原生界面验证了新建按钮、`Command-N`、`Command-V` 粘贴、`Command-Z` 撤销和 `Shift-Command-Z` 重做，以及中文标题/正文自动保存、退出重开恢复、搜索和软删除。
- 干净 release 构建通过 5 项核心集成测试、`cargo fmt --check` 和 `cargo clippy -- -D warnings`。
- ad-hoc 签名的 `.app` 通过 `codesign --verify --deep --strict`。
- App 包体约 2.3 MB；空资料库稳定后 RSS 约 46–49 MB，载入并编辑笔记后一次复测约 57 MB。
- 进程没有子进程；release 二进制的动态链接中没有 WebKit 或 JavaScriptCore。

## 当前限制

这是基础列表 UI 的本地 MVP：尚无 Joplin Server 同步、JEX 导入导出、附件、笔记本/标签管理、加密或高级编辑功能。当前客户端不能替代完整 Joplin 桌面端。
