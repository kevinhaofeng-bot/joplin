# GPUI 会话交底（2026-09-27，交给 joplin-c7 或后继会话）

本会话按用户指示在完成手头任务后停手。此前本会话负责 `packages/app-lite-gpui`，另一会话（joplin-c7）负责 core 迁移代码与同步。本文件把 GPUI 侧的状态、未完成项和操作经验交出去。上位计划仍是 `docs/superpowers/plans/2026-09-26-claude-remaining-delivery-handoff.md`。

## 1. 已提交（HEAD `5455bcb25` 时）

| 提交 | 内容 |
| --- | --- |
| `53e8192da` `d3127d4d3` | Cmd-N 后直接输入标题；空列表项回车时先减缩进、再变回段落；图片缩放后保持选中 |
| `24d5a632c` | 列表项缩进可保存（depth ↔ `ListItem.style.indent`，`MAX_LIST_DEPTH`=8） |
| `ef8517cf1` | 图片链接 `link: Option<String>`（canonical 与原生都有） |
| `dd13ee9fc` | 菜单"导出整个资料库为可读 HTML…"和"从可读导出恢复到新资料库…" |
| `c1275c5ea` | 4–6 级标题 |
| `27878ea83` | 有序列表起始号：`Block::List.start`，原生侧用 `Document` 侧表记录，由 `NumberingKind::OrderedFrom` 负责编号 |
| `c834043b4` | D5 回归测试：退出列表后输入不多出空段落 |
| `5455bcb25` | 表格：canonical `Block::Table { rows, header }`（`TableRow`/`TableCell`，单元格内只放行内内容）；原生 `BlockKind::Table` + `BlockContent::Table(Arc<TableContent>)` 是只读原子块，按网格画出纯文本（不换行），保存时原样写回 canonical 块 |
| `8f1d3fe05` `e676ca229` | 证据小节（08 文档）与表格方案 `docs/research/table-model-proposal.md` |

测试（`5455bcb25` 前的最后一次全量）：GPUI 1372 通过、0 失败、1 忽略；core 362 通过、0 失败；两个 crate 的 fmt 均通过。均为工作树内自测，不是纯净检出，也不是 Codex 验收。

## 2. 未完成（按优先级）

1. 表格后续：
   - 目前没有表格原子的选中/删除/撤销测试，也没有实机显示检查。这些走的是通用原子路径，理论上与附件卡片相同，但没有验证。
   - 单元格文字不换行，超宽部分被裁掉；单元格里的图片只显示 alt 或"[图片]"，不显示图像本身。
   - 编辑单元格：方案 B（单元格拆成平坦文本块 + TableGroup 侧表）尚未开始，见方案文档。
   - c7 的 GFM 表格导入映射要等 canonical 就绪才能接，现在已就绪（字段见上）。落地后请对真实副本跑一次原生加载/回写审计，确认含表格的笔记都能打开。
2. GUI 缺陷：
   - D2：连续多次输入法提交，每次提交各占一个撤销步（单次提交能被一次撤销，实机已确认），Evernote 会把连续输入合并。
   - 缩放手柄在右边缘被裁掉一半；列表标记在左边缘被裁切。
   - D5 在 HEAD 上实机不复现，已关闭。"更多"菜单点不开也不是缺陷，见下面第 4 节。
3. 实机矩阵（未完成）：拼音 IME 全矩阵、附件卡片与 Quick Look、中文搜索 UI、批量组织、菜单导入/备份/恢复（含可读导出）、重启后复查、表格显示。
4. 同步 GUI 接线：c7 负责 protocol/server/core 同步引擎（`93f6271f4` 起）；`app-lite-gpui` 里的客户端接线和状态 UI 原本归本会话，现在无人负责，需由接手方承担。NAS 部署需要用户在对话里明确同意。
5. 阶段 4：新时间戳的 Release 包、全产品 RSS/延迟测量、证据汇总。

## 3. 协作约定（沿用）

- 改 `packages/app-lite-core/src/document.rs`（以及机械修改 `jex_body.rs`/`jex_html.rs`）之前，先通知另一会话锁定文件，提交后再释放。
- 不 `git add .`；不纳入别人未提交的文件。提交时附 `Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>`。
- 不 push，不覆盖已安装的 App，不运行 donor 的 `create_macos_app_dist.sh`，不碰原始 Joplin/Evernote 资料库。
- 共享构建目录 `CARGO_TARGET_DIR=/Users/kevinhao/Projects/joplin/.shared-target`。GPUI 测试命令是 `cargo test --offline --bin velotype`（二进制名为 velotype）；core 需要加 `--features test-support`。
- 偶发测试（与改动无关）：`mounted_scheduler_close_finishes_only_active_index_transaction` 在高负载下可能卡死（hook 里的 `recv()` 没有超时），`mounted_scheduler_advances_a_derived_job_saved_after_open` 偶发失败。两者单独运行都能通过。

## 4. 实机操作经验

- 打包：`CARGO_TARGET_DIR=… bash packages/app-lite-gpui/scripts/package-notes-macos.sh`，产物在 `dist-notes/<时间戳>-<sha>/Joplin Lite.app`。最近一个包是 `20260927T022600Z-e676ca229`，路径也写在 `/tmp/joplin-lite-accept-0927/current-app.txt`。
- 启动（隔离 profile）：`JOPLIN_LITE_PROFILE=/tmp/joplin-lite-accept-0927/profile "<app>/Contents/MacOS/joplin-lite" > log 2>&1 &`。目前这个包仍在运行（该 profile），接手前请用 `pgrep -f 022600Z` 查到后正常退出。
- **Claude 桌面窗口会盖住屏幕右侧，但 computer-use 截图看不到它**：先用 `osascript … set position of window 1 to {0, 40}` 把 Joplin 窗口移到左边，否则右侧的点击会落到 Claude 窗口上。
- computer-use 的点击被拦截时，改用 `cliclick c:x,y`，坐标单位是点：0.6 缩放截图的像素 × 2.1，或截图坐标系 × 1.2595。按键用 `osascript -e 'tell application "System Events" to key code N'`（36 回车、49 空格、119 End）；Cmd-Z 用 `keystroke "z" using command down`。
- 输入法是鼠须管，候选栏可能不显示（`killall Squirrel` 可恢复）。`cliclick t:yi` 加空格可以上屏"以"。
- 验证保存结果：`sqlite3 "file:/tmp/joplin-lite-accept-0927/profile/library.sqlite?mode=ro" "select body_html from notes where title like '…%'"`。

## 5. 状态

- Claude 实施状态：上表提交待验收；表格只做了第 1 步（只读显示和原样保存）；实机矩阵未完成。
- Codex 验收状态：未验收。
