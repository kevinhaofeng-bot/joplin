# Task 3：Finder drag/drop、bundle contract 与图片附件 MVP

## RED / GREEN

- RED：先加入 `drag_file_reader_accepts_only_local_png_or_jpeg_images`，在实现 helper 前运行 targeted test；编译失败于缺失 `read_drag_image_file`，符合预期。
- GREEN：实现 `BodyTextView`（`NSTextView` 子类）并注册 `NSPasteboardTypeFileURL`；拖入只接受单个 local PNG/JPEG regular file，复用 `read_regular_image_file` 和既有 `insert_image_data`/repository importer；成功保存后 `performDragOperation:` 才返回 true。
- RED bundle contract：先创建最小 shell contract，随后扩展为图标、`CFBundleIconFile=AppIcon`、Electron/WebKit/JavaScriptCore/Node 禁带与 codesign strict 校验，并由 `bundle.sh` 末尾调用。

## 验证命令

以下命令均退出 0：

```text
cargo fmt --manifest-path packages/app-lite-native/Cargo.toml --check
cargo test --manifest-path packages/app-lite-native/Cargo.toml
cargo clippy --manifest-path packages/app-lite-native/Cargo.toml --all-targets -- -D warnings
bash packages/app-lite-native/scripts/bundle.sh
bash packages/app-lite-native/scripts/check-attachment-contract.sh "packages/app-lite-native/dist/Joplin Lite Native.app"
codesign --verify --deep --strict --verbose=2 "packages/app-lite-native/dist/Joplin Lite Native.app"
```

测试结果：13 个库测试、19 个 AppKit 单元测试、7 个生命周期集成测试全部通过。新增测试覆盖本地 PNG、错误扩展名，以及既有 regular-file、symlink、FIFO、10-MiB 上限拒绝路径。

## 签名 product smoke

- 临时 profile：`/tmp/joplin-lite-task3.2PjuKE/profile`（未使用默认 Joplin profile）。
- 使用签名 app 启动，创建临时笔记，输入标题与中文正文，粘贴 PNG、粘贴 JPEG，退出并重新启动；重启后两张图片均显示在正文中且顺序保持。
- 通过搜索框分别搜索中文正文和第二张图片的 alt 名称，均返回同一笔记。
- note ID：`000000000000000018d2c6bf3dd412b1`。
- 资源（按正文顺序）：
  - `000000000000000018d2c6c7266230cb`，`image/png`，blob SHA-256 `f4c8c545b071e85ab2d126c239e5347b0442fdf360722177bc2cb0487f2cb5e6`
  - `000000000000000018d2c6d29ffa259d`，`image/jpeg`，blob SHA-256 `73e1d589e868a7e458ab5894e925624a101b67da4d9f5c5fbf161fde579c7e8b`
- 进程树：签名 app 单进程，PID 50750，PPID 1，无子进程。
- release app 包大小：4.3M。
- smoke RSS：99,648 KiB（`ps` 读取，含两张图片后的运行状态）。
- contract 已检查 AppIcon.icns、Info.plist icon key、无 Electron/WebKit/JavaScriptCore/Node payload，并通过 deep strict codesign。

## Concerns

CUA 当前无法稳定合成 Finder 跨应用拖放：两次把 Finder JPEG 拖到正文均未产生可见导入，因此未把该动作冒充为 Finder 端到端成功。拖放 destination 代码已编译、注册并经过图片路径单元测试；本次真实签名持久化 smoke 使用了同一 importer 的 PNG/JPEG 粘贴路径。建议在人工 Finder 拖放或具备稳定跨应用 drag synthesis 的环境补做一次，并核对 `second.jpg` 资源 marker 与上述资源顺序。
