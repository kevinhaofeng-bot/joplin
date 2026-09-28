# 外部粘贴修复报告（Claude，供 Codex 独立验收）

对应 `/tmp/joplin-paste-acceptance-request.md` 的三项，以及过程中的审查补充（解码后大小上限、无回应服务器的取消、切换笔记与重启后的恢复、唯一标记、保存失败时保留任务）。基于 `5c0bc59f6`。未推送；所有测试只使用临时资料库；没有安装或覆盖 App。

## 提交

| 提交 | 内容 |
| --- | --- |
| `e0dca81ec` | 非 base64 的 data URI 按字节做百分号解码（原先经 String 转换，非 UTF-8 字节被替换为 U+FFFD） |
| `cec6be99e` | data URI 的 10 MiB 上限按解码后的字节数计算，每种编码分别精确计数（原先对所有编码套用 base64 的估算，恰好 10 MiB 的图片也会被拒） |
| `6c53ea8f9` | core schema v13：`pasted_image_jobs` 表与接口（记录、列出、计次、移除）；v12→v13 只加表；迁移测试的导入不再依赖 test-support 特性 |
| `50b35c6b3` | 下载流水线：专用线程上的 tokio 运行时、有界并发、流式写入暂存文件、单张上限与总量预算、整批时限、可立即取消、可重试失败的分类、回环地址直连 |
| `659095680` | 粘贴立即插入；本地来源先入库；网络图片由窗口层统一获取，不受编辑会话限制；带唯一标记的链接作为持久形式；重新打开时还原占位；启动时恢复未完成任务；保存成功后才退役任务 |

## Evernote 映射

| Evernote（`common-editor-sourcemap/@evernote/common-editor/src/apps/peso/modules/resource/`） | 本地 |
| --- | --- |
| `schema.ts`（SHA256 `74d07643…3e4975c5`）562–571：外部 URL 图片与资源图片是同一种节点，以便在后台转换为资源而不破坏撤销历史 | 图片节点在整个会话中保持同一个占位 ID；存入后用会话级映射把它导出为真实资源 ID，撤销和重做不会遇到被改写的节点 |
| `resource.ts`（SHA256 `1e8d8e9e…8991caa3`）548–594：按节点当前位置 `setNodeAttribute('resource', …)`，并设置 `addToHistory=false` | 结果到达时，按图片节点当前所在的位置补齐尺寸（`repair_legacy_image_natural_sizes`，不进入撤销历史）并保存；与粘贴时的选区无关 |

## 行为

- 粘贴时立即在光标处插入（一次撤销）。data URI 和本地文件的图片先按预算入库。网络图片显示为加载中的占位。
- 网络图片由 `LibraryShell` 获取（`ui/pasted_image_fetches.rs`），切换或关闭笔记不会中断。
- 获取期间，笔记保存为指向原图的链接，链接目标为 `原URL#joplin-lite-pasted-image-<占位ID>`，并在 core 里写一条任务。
- 完成时：
  - 笔记正在编辑：在编辑器内存入资源并保存。
  - 笔记未打开：在一个事务里写入资源，并按标记把链接改回图片。
  - 只认标记，已有的同文字、同地址的普通链接不受影响。
- 重新打开有待定任务的笔记时，按标记还原成占位图。应用启动时恢复所有任务。
- 只有在保存成功、且已保存的正文里不再有该标记之后，任务才会删除；保存失败时任务保留。
- 永久失败（4xx、不是图片等），或累计 3 次未完成，就改成指向原图的普通链接，并给出提示。

## 测试与证据

| 项目 | 结果 | 日志 |
| --- | --- | --- |
| RED：非 UTF-8 data URI 被破坏 | 失败（得到 U+FFFD 字节） | `/tmp/joplin-acceptance-fix/data-uri-red.log` |
| RED：解码后大小上限（恰好 10 MiB 的 base64） | 失败 | `/tmp/joplin-acceptance-fix/data-uri-limit-red.log` |
| RED：下载期间移动光标并输入，粘贴落点错误 | 失败（`插入粘贴文字 / 图 / 开头`） | `/tmp/joplin-acceptance-fix/paste-selection-red.log` |
| RED：21×10 MiB 同时驻留内存；3 个挂起服务器串行等待 | 失败（210 MiB；20 s 仍未返回） | `/tmp/joplin-acceptance-fix/paste-download-red.log` |
| 对照：忽略取消信号 | 无回应服务器用例失败 | `/tmp/joplin-acceptance-fix/silent-cancel-red.log` |
| 对照：标记退化为普通 URL；去掉会话外完成路径 | 切换、重启、身份用例失败 | `/tmp/joplin-acceptance-fix/pasted-image-durability-red.log` |
| `cargo test -q --bin velotype`（app-lite-gpui，`659095680` 的工作树） | 1426 通过、0 失败、2 忽略（其一为子进程测试的子进程部分） | `/tmp/joplin-acceptance-fix/gpui-paste-durable.log` |
| `cargo test -q --features test-support`（app-lite-core） | 400 通过、0 失败 | `/tmp/joplin-acceptance-fix/core-paste-durable.log` |
| `cargo test -q`（app-lite-core，默认特性） | 344 通过、0 失败 | `/tmp/joplin-acceptance-fix/core-paste-durable-default.log` |

主要新测试：

- `net::pasted_images`：
  - 解码：`a_percent_encoded_data_uri_keeps_the_image_bytes`、`the_image_size_limit_applies_to_decoded_bytes_in_every_encoding`（base64、带换行的 base64、百分号编码三种，恰好等于上限、多 1 字节，以及接近上限的真实图片）。
  - 预算：`a_paste_stages_images_to_files_within_its_total_budget`。
  - 时限与并行：`stalled_servers_share_one_deadline_and_run_in_parallel`。
  - 逐个送达：`results_arrive_one_by_one_and_a_cancelled_paste_stops`。
  - 取消：`cancelling_abandons_a_silent_server_and_frees_its_slot_and_files`（服务器始终不回应，分别测取消和丢弃句柄；槽位与暂存目录在有界时间内回收）。
- core：
  - 迁移：`v12_library_upgrades_by_adding_only_the_pasted_image_job_table`（旧正文、修订号、笔记本不变，重复打开幂等）；更新后的 v11 测试。
  - 生命周期：`pasted_image_jobs_are_kept_counted_and_follow_their_note`（去重、计次、备份与恢复、移入废纸篓时保留、永久删除时清除、重新打开后一致）。
- 挂载界面：
  - `mounted_external_paste_stays_where_it_was_pasted_while_images_download`：门控服务器，下载期间移动光标并输入；检查落点、资源、撤销与重做、持久化 HTML。
  - `mounted_pasted_images_survive_switching_notes_before_they_arrive`：切走；切走再切回。
  - `mounted_pasted_web_image_arrives_after_a_restart`：窗口关闭后资料库引用计数回到 1，关闭后从 profile 重新打开；目标笔记不被打开，只靠启动时恢复完成。
  - `mounted_pasted_image_job_outlives_a_failed_save`：用 SQLite 触发器让保存真正失败，任务保留；关闭并重新打开后完成，正文里不留标记。
  - `mounted_pasted_images_land_by_identity_not_by_matching_text`：已有相同的普通链接；两张图以相反顺序完成；覆盖打开与关闭两种情况。
  - `pasted_web_image_survives_the_app_process_dying`：子进程在下载中途 `process::exit`；父进程重新打开后恢复并完成。

## 已知边界

- 连续三次未完成后放弃，图片保留为指向原图的链接。
- 在编辑器里存入后、保存前进程崩溃：崩溃恢复日志写的是真实资源 ID；任务在下次运行时找不到标记，会自行结束。
- 打包脚本仍未包含产品图标（Codex 已记为最终交付缺口）。
- 实机验证（真实浏览器、Word、网络环境与代理）仍需 Codex 完成；挂载测试不能代替实机。

## 隔离验收包

| 项目 | 值 |
| --- | --- |
| 包路径 | `/tmp/joplin-final-claude/dist-notes/20260928T052752Z-659095680/Joplin Lite.app` |
| 源码提交 | `65909568090c6d8170ba9ed185dbab802e23098a`（`worktree_dirty_for_app_sources: no`） |
| 可执行文件 SHA256 | `c9a1d531acea26b0d3520edf33ea5c31ee0a97e9d01670fc3503356997edd618`（已用 `shasum -a 256` 复核） |
| 版本 | 0.7.2 (16296) |
| 构建日志 | `/tmp/joplin-acceptance-fix/package-659095680.log` |

没有安装到 /Applications，没有启动，也没有打开任何真实资料库。
