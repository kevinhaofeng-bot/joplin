# Release 浅色路由与 macOS 打包独立复审

日期：2026-09-12

结论：**APPROVED**

计数：**0 Critical / 0 Important / 1 Minor**。

本复审只检查当前共享脏工作树的浅色默认 Library route、Spike 回归与 macOS bundle
脚本，没有修改产品代码、commit、tag 或 push。Task 5 Step 5 的 fresh-profile Release
目视/交互验收仍是单独门槛；自动测试与 bundle 哈希一致不能替代该门槛。

## 已验证结论

### 1. 陈旧 binary 根因成立，当前 Cargo target 是 shared target

- 当前工作目录执行
  `cargo metadata --manifest-path packages/app-lite-gpui/Cargo.toml --no-deps --format-version 1`
  返回的 `target_directory` 是
  `/Users/kevinhao/Projects/joplin/.shared-target`。
- 陈旧
  `/Users/kevinhao/Projects/joplin/.worktrees/joplin-lite-native-rust-mvp/packages/app-lite-gpui/target/release/velotype`
  为 7,786,048 bytes，修改时间 2026-09-11 13:04:27，SHA-256
  `edf18d89191744715b25f373694d093d49550a8692393a6c51970d95a3bacbac`；它没有
  `library-main-editor-shell` 或 `library-editor-command-toolbar` 标识。
- fresh release
  `/Users/kevinhao/Projects/joplin/.shared-target/release/velotype` 为 8,017,760 bytes，
  修改时间 2026-09-12 05:34:58，SHA-256
  `f16023353356fe8d0f2f25a73f179c1cdacdcba63ed0540c08a022892a46330f`；它包含
  `library-main-editor-shell`、`library-editor-command-toolbar`、`library-empty-state` 与
  `library-no-selection`。当前运行进程的 executable 也来自 shared-target 绝对路径。
- 独立 fresh `cargo build --release` 成功；Cargo判定该 shared-target artifact已是当前源码的
  fresh产物，因此未进行无意义重链接。旧 package-local binary的时间、大小、哈希和标识
  均与之不同，原黑底截图启动陈旧 local target而非当前 Cargo产物的根因得到佐证。

### 2. 打包脚本不会回落到旧 package-local target

- `/Users/kevinhao/Projects/joplin/.worktrees/joplin-lite-native-rust-mvp/packages/app-lite-gpui/scripts/create_macos_app_dist.sh:17-29,31-45`
  以绝对 manifest执行 release build，再用同一环境的 `cargo metadata` 取得
  `target_directory`，只从 `$TARGET_DIR/release/velotype` 复制；目标为空或不可执行时
  fail closed。它不再引用 `$PROJECT_ROOT/target/release/velotype`。
- 实际设置含空格的
  `CARGO_TARGET_DIR='/tmp/joplin lite release target'`（指向现有 shared target的隔离测试别名）
  后，metadata保留完整空格路径，脚本成功 build并生成 `.app`。源 binary与
  `dist/Velotype.app/Contents/MacOS/velotype` 的 SHA-256完全相同，均为上述
  `f160...630f`；引号边界与 custom target路径验证通过。
- `bash -n` 通过。当前 sed抽取对题设要求的普通含空格 Unix路径有效；本轮没有发现会让
  它在 build成功后误拷陈旧 local binary的路径。

### 3. Library所有要求状态显式浅色，Spike未被全局改写

- `/Users/kevinhao/Projects/joplin/.worktrees/joplin-lite-native-rust-mvp/packages/app-lite-gpui/src/ui/mod.rs:80-119,421-457,1762-1855,2037-2157,2323-2414`
  以 `#fff` primary surface、`#141414` primary text、`#696564` muted text及
  `#f3f2f1` stroke作为真实 GPUI样式参数。空资料库、存在卡片但无选择、选中笔记三态，
  以及 shell、main editor、actions toolbar、title和editor pane均有明确不透明背景/深色
  前景；title自绘 `TextRun`也直接使用primary text，不依赖native window继承色。
- `/Users/kevinhao/Projects/joplin/.worktrees/joplin-lite-native-rust-mvp/packages/app-lite-gpui/src/native_editor/toolbar.rs:72-114,515-538,1263-1355`
  仍使用同一个 `EditorCommandChrome`。只有 `EditorCommandChromeHost::Library` 给实际toolbar
  增加white/background/stroke/text；Spike host保留原透明chrome合同，没有复制handler或
  全局强制light toolbar。
- `/Users/kevinhao/Projects/joplin/.worktrees/joplin-lite-native-rust-mvp/packages/app-lite-gpui/src/native_editor/surface.rs:24-49,220-248,369-421`
  的shared surface始终保持既有white canvas；只在非embedded Library host增加stroke和
  foreground，embedded Spike不新增边框/宿主text style。正文glyph本身在
  `native_editor/layout.rs::styled_text_runs` 继续显式shape为black，因此可读性不依赖父级
  `Div::text_color`。
- 独立 `spike_app::tests` 46/46通过；共享Chrome、overlay、selection/history与embedded
  surface既有路径未回归。

### 4. mounted contrast gate连接生产样式且对mutation敏感

- `/Users/kevinhao/Projects/joplin/.worktrees/joplin-lite-native-rust-mvp/packages/app-lite-gpui/src/ui/tests.rs:665-762`
  真正mount/redraw生产 `LibraryShell`：分别覆盖empty、no-selection和点击真实card后的
  selected state；selected态还直接读取retained shared Chrome与native EditorSurface的
  样式调用记录，并逐个要求Shell/MainEditor/Toolbar/Title/EditorPane为opaque white。
- probe由生产 `.bg` / `.text_color` / `.border_color` 参数helper在draw时写入，测试没有直接
  调range renderer或重造另一套palette。
- 独立复制两个crate到 `/tmp/task6-release-contrast-mutation`，只在临时副本删除empty-state
  的生产 `.bg(self.evernote_primary_surface_fill(...))`，使用独立 target运行同一exact test；
  它按预期于 `ui/tests.rs:682` RED：实际值 `None`，期望opaque white。共享产品源码上的同一
  test随后1/1 GREEN。这证明测试能捕获具体production style调用被删除。

## Minor

### REL-M1 — bundle资源路径仍依赖调用者cwd

位置：

- `/Users/kevinhao/Projects/joplin/.worktrees/joplin-lite-native-rust-mvp/packages/app-lite-gpui/scripts/create_macos_app_dist.sh:36-37`

脚本已为manifest、dist、README和release binary使用绝对 `PROJECT_ROOT`，但Info.plist和icns
仍写成 `resources/macos/...`。从package root执行完全正常；从worktree root以
`./packages/app-lite-gpui/scripts/create_macos_app_dist.sh` 调用时，独立复现为build成功后
`cp: resources/macos/Info.plist: No such file or directory`。随后从package root重跑已恢复完整
bundle，且bundle binary哈希仍与shared target一致。

这不会误拷旧binary或生成可启动的错误包，因 `set -e` 会在缺资源时退出；题设要求的
custom target和含空格路径也已通过。因此判为非阻断Minor。最小修复是将两项源路径改为
`$PROJECT_ROOT/resources/macos/...`，并顺手把第3行usage中的旧脚本名修正为当前文件名。

## 独立验证

```text
RUSTFLAGS='-Awarnings' cargo test --quiet --manifest-path packages/app-lite-gpui/Cargo.toml \
  --bin velotype mounted_default_light_route_keeps_every_editor_state_opaque_and_contrasted \
  -- --nocapture
# PASS: 1/1

RUSTFLAGS='-Awarnings' cargo test --quiet --manifest-path packages/app-lite-gpui/Cargo.toml \
  --bin velotype ui::tests:: -- --test-threads=8
# PASS: 53/53

RUSTFLAGS='-Awarnings' cargo test --quiet --manifest-path packages/app-lite-gpui/Cargo.toml \
  app::tests -- --nocapture
# PASS: 72/72

RUSTFLAGS='-Awarnings' cargo test --quiet --manifest-path packages/app-lite-gpui/Cargo.toml \
  --bin velotype spike_app::tests -- --nocapture
# PASS: 46/46

RUSTFLAGS='-Awarnings' cargo test --quiet --manifest-path packages/app-lite-gpui/Cargo.toml \
  --bin velotype -- \
  --skip editor::selection::tests::cross_block_cut_writes_markdown_deletes_range_and_undo_restores
# PASS: 1,170 passed / 0 failed / 1 exact documented donor test filtered

RUSTFLAGS='-Awarnings' cargo check --quiet \
  --manifest-path packages/app-lite-gpui/Cargo.toml --tests
RUSTFLAGS='-Awarnings' cargo check --quiet --release \
  --manifest-path packages/app-lite-gpui/Cargo.toml
RUSTFLAGS='-Awarnings' cargo build --quiet --release \
  --manifest-path packages/app-lite-gpui/Cargo.toml
cargo fmt --manifest-path packages/app-lite-core/Cargo.toml -- --check
cargo fmt --manifest-path packages/app-lite-gpui/Cargo.toml -- --check
git diff --check
bash -n packages/app-lite-gpui/scripts/create_macos_app_dist.sh
# PASS

(cd packages/app-lite-gpui && \
CARGO_TARGET_DIR='/tmp/joplin lite release target' \
  ./scripts/create_macos_app_dist.sh)
# PASS; bundle/source SHA-256 identical
```

---

## Release Minor fix 最终复审（2026-09-12，superseding conclusion）

结论：**APPROVED**

计数：**0 Critical / 0 Important / 0 Minor**。

本节取代上面的首次结论。原 `REL-M1` 已关闭；本轮只更新本审查报告，没有修改产品
代码、commit、tag 或 push。

### REL-M1 已关闭：bundle资源与binary均不再依赖caller cwd

- `/Users/kevinhao/Projects/joplin/.worktrees/joplin-lite-native-rust-mvp/packages/app-lite-gpui/scripts/create_macos_app_dist.sh:9-11,17-29,31-40`
  现在以脚本自身位置计算绝对 `PROJECT_ROOT` 和
  `MACOS_RESOURCES_DIR="$PROJECT_ROOT/resources/macos"`。manifest、dist、README、
  `Info.plist`、`velotype.icns` 与release binary均来自该authority；不存在剩余的
  `resources/macos/...` caller-relative copy。
- 独立从worktree root调用绝对脚本、从
  `packages/app-lite-gpui/scripts/` 目录调用相对脚本、以及从 `/tmp` cwd配合
  `CARGO_TARGET_DIR='/tmp/joplin lite release final target'` 调用绝对脚本，三次均成功。
  第一轮无相同 `RUSTFLAGS` cache key，实际完成一次1m28s fresh release link；后两次正确
  复用同一artifact，没有回落package-local旧binary。
- 每次脚本调用后均在 `set -e` gate中执行 `cmp -s`、SHA-256、`plutil -lint` 和icon
  non-empty检查；若任一失败，命令不会继续到下一调用上下文。最终Cargo metadata source
  `/Users/kevinhao/Projects/joplin/.shared-target/release/velotype` 与
  `dist/Velotype.app/Contents/MacOS/velotype` 的SHA-256均为
  `b2c11962c8e5c6dcd8bc57df3a9c67d7f3189218e8a575fdc4a84647d694513c`。
  `Contents/Info.plist` 经 `plutil` 返回 `OK`，`CFBundleExecutable=velotype`；
  `Contents/Resources/velotype.icns` 存在且为795,337 bytes。

### 新增实机证据与真实profile一致

- 已逐张目视
  `/tmp/joplin-lite-b1-correct-empty.png`、`title2.png`、`body.png`、`two-pane.png`、
  `one-pane.png`、`three.png`、`relaunch.png`。七张均为3456×2234 RGBA PNG；空态、
  title/body、三栏/两栏/一栏和重开态均没有暴露黑色native backing，深色正文/标题、
  light shared Chrome及绿色caret/selection与报告描述相符。
- 按同一 `b1-correct` 前缀和05:42–05:46时间链定位到实际fresh profile
  `/tmp/joplin-lite-b1-correct.0T4Fp3`：目录和DB birth分别为05:42:47/05:42:59；
  `library.sqlite-shm` 在05:45:28重新打开，早于05:45:39的relaunch截图；DB checkpoint
  时间05:46:47。
- 对该profile的真实 `library.sqlite` 查询得到唯一active note：title精确为
  `Release smoke title`，`body_text`精确为 `Release body text`，canonical HTML为
  `<p>Release body text</p>`，revision 3；`edit_journal`为空，settings的last selection
  指向同一NoteId，`PRAGMA integrity_check` 为 `ok`。因此截图中的重开可见内容确已落到
  snapshot，不是只存在于尚未保存的retained UI。
- 当前机器另有一个自02:36持续运行的旧M1验收进程，profile是
  `/tmp/joplin-lite-m1-profile.7zu18l`；它与本组05:42新建的`b1-correct`截图/profile无关，
  不能拿其DB否定或替代本轮证据。以文件birth、SHM reopen、截图与DB内容关联后，
  `/Users/kevinhao/Projects/joplin/.worktrees/joplin-lite-native-rust-mvp/.superpowers/sdd/2026-09-11-evernote-core-notes-roadmap/task-6-stage-b-report.md:48-67`
  的fresh-profile补充记录成立。

### 最终验证

```text
# worktree root
./packages/app-lite-gpui/scripts/create_macos_app_dist.sh
# PASS

# scripts directory
(cd packages/app-lite-gpui/scripts && ./create_macos_app_dist.sh)
# PASS

# arbitrary cwd + custom target path containing spaces
(cd /tmp && CARGO_TARGET_DIR='/tmp/joplin lite release final target' \
  /Users/kevinhao/Projects/joplin/.worktrees/joplin-lite-native-rust-mvp/packages/app-lite-gpui/scripts/create_macos_app_dist.sh)
# PASS

cmp -s /Users/kevinhao/Projects/joplin/.shared-target/release/velotype \
  packages/app-lite-gpui/dist/Velotype.app/Contents/MacOS/velotype
shasum -a 256 /Users/kevinhao/Projects/joplin/.shared-target/release/velotype \
  packages/app-lite-gpui/dist/Velotype.app/Contents/MacOS/velotype
plutil -lint packages/app-lite-gpui/dist/Velotype.app/Contents/Info.plist
test -s packages/app-lite-gpui/dist/Velotype.app/Contents/Resources/velotype.icns
# PASS; hashes identical, plist OK, icon present

sqlite3 /tmp/joplin-lite-b1-correct.0T4Fp3/library.sqlite 'PRAGMA integrity_check;'
# ok

cargo fmt --manifest-path packages/app-lite-core/Cargo.toml -- --check
cargo fmt --manifest-path packages/app-lite-gpui/Cargo.toml -- --check
git diff --check
# PASS
```
