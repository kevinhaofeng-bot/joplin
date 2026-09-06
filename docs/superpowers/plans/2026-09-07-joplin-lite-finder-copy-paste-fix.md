# Joplin Lite Native Finder 复制粘贴图片修复计划

## 目标

修复从 Finder 对图片文件执行复制、再在正文粘贴时插入 1024×1024 文件图标预览而非原始图片的问题，同时保持普通像素图片剪贴板粘贴和 Finder 拖放路径不回退。

## 已确认根因

`read_pasteboard_image` 当前依次读取 `public.png`、`public.tiff`、`public.jpeg`，最后才处理 `public.file-url`。Finder 复制文件时可同时发布真实文件 URL 与供其他应用显示的文件图标预览；当前顺序因此把预览 PNG/TIFF 当成正文图片。用户资料库中的错误资源已只读核实为标题 `clipboard.png`、`1024×1024`、306607 字节，而同一笔记内 Finder 拖入的真实文件保留原文件名 `IMG_7641.jpg`。

## Task 1：剪贴板表示选择与回归验证

只修改 `packages/app-lite-native`，具体要求：

1. 先写失败测试，构造同时含单个本地 `public.file-url` 和不同 PNG/TIFF 预览的 pasteboard；证明旧逻辑选择预览而非文件字节。
2. 把 pasteboard 解析提取为可注入 `NSPasteboard` 的生产 helper，AppDelegate 的通用剪贴板入口复用它。
3. pasteboard 存在 `public.file-url` 时，优先按现有拖放安全契约读取真实文件：仅单个本地 regular PNG/JPEG，拒绝 symlink、FIFO、目录、远程 URL、promised file、非图片、编码/扩展名不符和超过 10 MiB。
4. 文件 URL 存在但无效时不得回退到 Finder 提供的 PNG/TIFF 图标预览。
5. 不含文件 URL 的真正像素剪贴板继续按 PNG、TIFF→PNG、JPEG 顺序导入。
6. 成功插入仍须走既有 off-view candidate → repository commit → native insert 路径，不修改 canonical marker、资源存储、undo/redo 或拖放实现。
7. 版本提升为 `0.3.1`，更新 README 的粘贴验收说明。
8. 运行 `cargo fmt --check`、`cargo test`、`cargo clippy --all-targets -- -D warnings`、bundle contract、deep strict codesign；使用签名 App 和隔离 profile 做一次真实 Finder Cmd-C 文件 → 正文 Cmd-V → 退出重开验证，确认数据库保存原始文件名/字节摘要而不是 `clipboard.png` 图标预览。

## 非目标

- 本轮不实现 Markdown/RTF 存储迁移。
- 本轮不调整图文混排布局。
- 不访问或修改官方 Joplin profile、Joplin Server、NAS 或生产同步配置。
