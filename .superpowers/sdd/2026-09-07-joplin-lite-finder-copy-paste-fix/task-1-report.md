# Task 1 报告：剪贴板表示选择与回归验证

## status

实现完成；真实 Finder 端到端验证未完成，见 concerns。

## 实现

- 将 AppDelegate 的剪贴板读取提取为可注入 `NSPasteboard` 的生产 helper。
- `public.file-url` 现在先于 PNG/TIFF/JPEG 读取；单个本地 PNG/JPEG regular file 继续复用现有安全文件读取、扩展名和编码校验。
- file URL 无效、远程、promised、多项、非图片、符号链接/FIFO/目录或超过 10 MiB 时直接拒绝，不回退到 Finder 图标 bitmap。
- 无 file URL 的 PNG/TIFF/JPEG 像素剪贴板路径保持不变；插入事务、canonical/RTF/undo/drop 未改动。
- 版本升至 0.3.1，README 增加 Finder 文件粘贴验收说明。

## RED → GREEN 证据

先运行真实 `NSPasteboard` 注入测试，旧顺序失败：断言观察到返回 Finder PNG 图标字节，而不是 file URL 指向的 JPEG 字节；修复读取优先级后通过。

新增回归覆盖：

- file URL + 不同 PNG/TIFF 预览时返回原始文件名、MIME 和原始字节；
- 无效 remote file URL 拒绝且不回退到 icon；
- 无 file URL 的 PNG 像素剪贴板继续导入。

## 测试

- `cargo fmt --all --check`：通过
- `cargo test`：48 个测试通过（13 lib、28 app、7 lifecycle；无失败）
- `cargo clippy --all-targets -- -D warnings`：通过
- `scripts/check-attachment-contract.sh`：通过
- `scripts/check-icon-contract.sh --bundle ...`：通过
- `codesign --verify --deep --strict --verbose=2`：通过
- bundle `CFBundleShortVersionString`/`CFBundleVersion`：0.3.1

## concerns

- 尝试用签名 App + 隔离 profile 做 Finder Cmd-C 文件 → 正文 Cmd-V → 重启验证时，桌面 CUA 对同 bundle 的重新启动没有保留 `JOPLIN_LITE_NATIVE_DATA_DIR`，并接管/重新启动到官方 profile。为遵守 brief 的非目标，未继续进行 Cmd-C/Cmd-V，也没有宣称真实 Finder E2E 通过。
- 该排查期间 CUA 曾对被接管窗口执行一次“新建笔记”点击；随后已停止相关进程。请在交付前核查官方 profile 是否产生空白草稿。

