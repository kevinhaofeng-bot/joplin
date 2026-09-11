# Task 5 独立正式审查

审查基线：`a91b842da97ee48db2f05e7bf0cacfd6e637a020`；被审查对象为 2026-09-11 当前共享脏工作树（尚无 Task 5 提交）。

结论：**CHANGES REQUESTED** — 1 Critical / 4 Important / 0 Minor。

## 行为权威与范围

本轮在检查 Rust 实现前，重新阅读了以下 Evernote 已解析实现：

- `common-editor/.../modules/clipboard/plugin.ts` 的 `state.apply`：保存完整 Selection，并把已插入范围逐 step map 到后续文档；
- `common-editor/.../modules/clipboard/commands/paste.ts` 的 `execCommand`：在精确 Selection 上做一次 replace/dispatch；
- `common-editor/.../modules/dragdrop/plugin.ts`：移动删除后使用 `tr.mapping.map(insertPos)`，最终只 dispatch 一次；
- `common-editor/.../modules/textbetweenblocks/plugin.ts::insertOrFocusParagraph` 与 `noteendparagraph/plugin.ts`：原子块间/尾部先聚焦现有段落，否则插入段落后聚焦；
- `common-editor/.../modules/resource/fileplugin.ts::handleClickOn/handleDoubleClickOn`：编辑态单击建立 NodeSelection；双击行为按 Neutron/Boron/其他客户端分支不同；本产品的“通用系统打开”是 Task 5 自己的产品策略，不是声称逐行复制其中某个 Evernote 分支；
- `common-editor/.../modules/resource/image/imagecomponent.tsx`：图片选中、稳定 extent、资源状态和 `selectedThumbnailHash` 均由当前 node view/规范状态驱动；
- `main-readable/diagnostics/deobfuscated-main.js` 的 `ENStagedBlobManager` 与 `AttachmentRepositoryImpl`：staging 独立持久、可重试错误保留 staged data、附件元数据与按需内容读取分离。

同时以 `task-5-brief.md`、roadmap Task 5、产品设计 7.2/7.3 和行为图谱“图片立即显示/图片前后输入/剪贴板/本地优先会话”为 acceptance authority。以下结论来自实际源码和测试，不采用实现报告自证。

## Critical

### T5-C1 — 删除、剪切或撤销已持久资源后，普通保存和崩溃恢复都拒绝新文档，资源会在重启后复活

- Rust 证据：`/Users/kevinhao/Projects/joplin/.worktrees/joplin-lite-native-rust-mvp/packages/app-lite-gpui/src/app/note_session.rs:754` 把 `resource_ids` 作为会话侧独立向量；`capture_committed_snapshot` 在 `:2151-2161` 每次仍复制该旧向量，而 Backspace/Delete/undo/redo 只改变 `EditorCore` 文档。随后 `encode_snapshot` 在 `:2183-2200` 从正文导出资源 ID 后要求与旧向量完全相等。崩溃日志恢复还有第二个同类限制：`JournalPayload::into_snapshot` 在 `:608-621` 要求 journal IDs 与 durable note IDs 完全相等。
- 重现逻辑：打开含图片 A 的已保存笔记，选中 A 后按 Backspace/Delete（或对刚提交的 A 执行 Undo）。画布立即删除 A，但 `self.resource_ids` 仍为 `[A]`；100 ms journal 和 500 ms snapshot 都在 `encode_snapshot` 报“正文资源关系与笔记不一致”。关闭/切换因此被保存屏障阻止；若崩溃发生在 journal 后，重启又因 journal `[ ] != note [A]` 拒绝恢复。若在删除 A 后插入附件 B，资源提交候选是 `[A,B]`、正文却只有 `[B]`，新事务也必失败。
- 为什么违反 Task 5：产品设计明确要求图片退格删除、跨块选择与撤销使用同一文档坐标，并且资源关系随规范快照持久化；Task 5 Step 2/3 要求这些动作与 note-resource order、snapshot 形成闭环。当前 UI 成功态与持久态不可一致，是核心数据行为阻断。
- 最小修复方向：从每次 immutable native document snapshot 导出新的有序 `resource_ids`；旧 durable 集合只用作允许引用的 fail-closed allowlist，不能再当成“结果必须完全相等”。journal 恢复应允许 payload 为 durable base 的有序多重子序列（每个重复 occurrence 不超过 base），禁止 journal 凭空新增资源，并继续要求正文 IDs 与 payload 精确相等。增加 `A,B,A` 删除/剪切/undo/redo → 100 ms journal → crash/reopen → 500 ms snapshot → 二次 reopen 的 NoteSession + mounted 测试，同时验证 journal ownership/expected revision。

## Important

### T5-I1 — 打开已有图片笔记仍在 GPUI 前台逐图反复校验和复制，切换大笔记会冻结窗口

- Rust 证据：`/Users/kevinhao/Projects/joplin/.worktrees/joplin-lite-native-rust-mvp/packages/app-lite-gpui/src/ui/mod.rs:693-704` 在 retained shell/model observer 的同步 update 内直接调用 `NoteSession::prepare` 和 `from_prepared`。`/Users/kevinhao/Projects/joplin/.worktrees/joplin-lite-native-rust-mvp/packages/app-lite-gpui/src/app/note_session.rs:643-696` 对每张图片同步 `open_verified_resource_file` 并 inspect；`:935-963` 随后再次同步 open/verify，并调用 `register_durable_image_reader` 把整张压缩图复制到 session cache。`/Users/kevinhao/Projects/joplin/.worktrees/joplin-lite-native-rust-mvp/packages/app-lite-core/src/resource.rs:249-268` 说明一次 `open_verified` 本身就会把整个 blob 流过 SHA-256。
- 重现逻辑：创建/打开含多张接近 10 MiB 图片的笔记并从另一笔记切入。每张图片在首帧前经历至少一次全量 hash+inspect，随后又经历一次全量 hash+cache copy；这些都发生在 GPUI update 路径。现有 background gate 只覆盖新 picker/drop 的 stage/commit，不能杀死这个 reopen 路径。
- 为什么违反 Task 5：Task 5/产品设计要求资源 I/O、图片解码不阻塞编辑/切换，并要求典型多图笔记保持可交互；“不汇总为 Vec”不能替代“不得在 foreground 做 O(total bytes) I/O”。
- 最小修复方向：同步 prepare 只解析 canonical body 和资源 metadata，沿用已持久 natural size 挂载稳定 placeholder；按可见范围在 background executor 做 verify/materialize/decode。完成结果必须携带 note/session generation，旧 session drop 后不得注册或重建已删除 cache root。加 gated mounted reopen 测试：worker 未释放时新笔记已可 paint/输入，非可见图片不读，切换后旧完成被丢弃并清理。

### T5-I2 — 原生 Cmd-V 在 image-first 判型前先解析 HTML/RTF/text，并在 GPUI 前台同步把图片写磁盘

- Rust 证据：`/Users/kevinhao/Projects/joplin/.worktrees/joplin-lite-native-rust-mvp/packages/app-lite-gpui/src/ui/mod.rs:1116-1120` 在 Paste action 回调中同步调用 `read_native_pasteboard`。`/Users/kevinhao/Projects/joplin/.worktrees/joplin-lite-native-rust-mvp/packages/app-lite-gpui/src/native_editor/images.rs:2554-2565` 先读取 HTML、构造 `NSAttributedString` 解析 RTF、读取纯文本；直到 `:2566-2599` 才检查 image UTI；`:2600-2617` 又用 `NSData.writeToFile_atomically_` 在当前线程同步落临时盘。`classify_clipboard` 的后续优先级测试没有覆盖这些已经发生的前台成本。
- 重现逻辑：剪贴板同时提供大 RTF/HTML 表示和 8–10 MiB TIFF/PNG，按 Cmd-V。即使最终选中图片，主线程仍先完成 RTF/字符串解析和整图磁盘写入，之后才把路径交给已有 background ResourceStageJob；窗口可在 worker 启动前冻结。
- 为什么违反 Task 5：Evernote paste authority 是多表示 image/file 优先、保存 Selection、解析完成后单事务插入；Task 5 要求图片粘贴不打断写作且低内存后台处理。当前代码只在“判型结果”上 image-first，执行顺序并非 image-first。
- 最小修复方向：AppKit pasteboard 访问可保留主线程，但先 probe image UTI；只把 `NSData.length <= MAX_IMAGE_BYTES` 的 bytes 拷贝成 process-owned `ImagePayload(Vec<u8>)`，把格式校验、hash、fsync、SQLite 全交给已有 background ResourceStageJob。仅在无图片时解析 RTF/HTML/text。增加可注入 native representation 顺序/大 payload 测试，并证明 action 返回前无 filesystem write。

### T5-I3 — 同一资源提交返回规范 thumbnail `None` 时，AppModel 不会清除旧卡片缩略图

- Rust 证据：repository 已在资源/笔记同一事务内计算最终 `selected_thumbnail_id`，但 `/Users/kevinhao/Projects/joplin/.worktrees/joplin-lite-native-rust-mvp/packages/app-lite-gpui/src/app/mod.rs:390-415` 的 `apply_active_resource_commit` 只有 `inserted_thumbnail.is_some()` 才写 projection；`None` 被解释成“不更新”，而接口语义实际是“规范最终值为空”。
- 重现逻辑：笔记原来唯一图片 A 是卡片封面；删除 A 后在同一 dirty 会话插入 PDF/普通附件（T5-C1 修复后即成为合法路径）。资源事务返回 `selected_thumbnail_id=None`，但当前 presentation cycle 继续显示 A，直到异步 LibraryEvent reconciliation；若事件延迟/合并，旧封面持续更久。现有测试只覆盖 A→B 返回 `Some(B)` 和插第二图保留 `Some(A)`，没有 clear-to-None。
- 为什么违反 Task 5：产品设计要求当前编辑器和卡片缩略图由同一个提交结果驱动，不能等重选/异步刷新；Evernote 的 `selectedThumbnailHash` 是规范状态而非“可选 patch”。
- 最小修复方向：对命中的 active projection 无条件赋值 `projection.selected_thumbnail_id = committed.selected_thumbnail_id`；增加唯一封面删除 + 插非图资源的 mounted same-cycle 断言，并使测试在移除赋值或把 `None` 当 no-op 时失败。

### T5-I4 — 附件临时副本权限未收紧，且 `/usr/bin/open` 尚未接管路径时 session 可立即删除它

- Rust 证据：`/Users/kevinhao/Projects/joplin/.worktrees/joplin-lite-native-rust-mvp/packages/app-lite-gpui/src/app/note_session.rs:280-315` 用默认 `create_dir_all`/`OpenOptions` 创建可能含敏感附件的目录和文件，没有 Unix 0700/0600；`:246-254` 使用 PATH 中的 `open` 且只 `.spawn()`，子进程/LaunchServices 是否已经接受文件未知就报告成功。成功结果 `AttachmentOpenSuccess` 在 `:242-244` 不携带/持有路径。`/Users/kevinhao/Projects/joplin/.worktrees/joplin-lite-native-rust-mvp/packages/app-lite-gpui/src/native_editor/images.rs:778-801` 证明副本并非永久泄漏：它位于每 EditorCore UUID root，并在 ImageStore drop 时删整树；问题是默认权限与过早清理竞态。
- 重现逻辑：双击附件后立即切换笔记。background opener 的 `spawn()` 已返回，UI 收到 success；切换销毁旧 EditorCore，ImageStore `remove_dir_all`，而 `/usr/bin/open` 子进程可能尚未 stat/提交 LaunchServices 请求，导致用户看到“已交给系统打开”但没有程序拿到文件。默认 umask 下内部目录/文件通常为 0755/0644，也不符合对私密笔记附件显式最小权限的安全契约。
- 为什么违反 Task 5：附件卡双击必须可靠触发产品定义的系统打开，临时物化必须安全、按 session 清理；测试注入的 opener 同步读取后返回，不能覆盖生产 `.spawn()` 交接窗口。
- 最小修复方向：每 session root/attachments 目录显式 0700、temp/final 文件 0600，目录和叶子继续 no-follow/descriptor-bound；background worker 使用绝对 `/usr/bin/open` 并 `.status()` 等到 handoff 成功后再发布 success。路径至少保留到 opener 返回，之后切换再由 session drop 清理。增加权限、symlink、opener-return 前切换不删/return 后切换清理、非零退出可见错误测试。

## 已确认关闭的预审项

- 资源 stage/commit 的生产入口现在确实由 NoteSession retained background task 驱动，不再走旧 `import_resource` + `flush_snapshot_note` 两阶段 UI 路径。
- stage/commit 已加入 lifecycle blocker；`mounted_lifecycle_switch_waits_for_a_gated_resource_stage_then_commits_it` 曾在共享修改竞态前真实失败，修复后重跑通过。`finish_resource_stage` 的错误分支也会清理 lifecycle token。
- `ResourceBlob` 已回到纯 content-addressed 值；staging 的 entity ID 走 repository allocator。旧 deterministic collision 测试和新增跨 connection stage→commit race/zero-event/zero-outbox 测试均通过。
- InsertIntent 保存完整 Selection；prefix edit、range replacement、split/merge/remove、undo/redo 的 mutation-sensitive anchor 测试通过。附件单击完整原子选择、双击发 typed open event 并由 session 验证物化的 mounted 路径也真实通过。
- 原子块间和末尾 dead-zone 的 mounted test 通过，不再复现相邻附件下边界吞点击。

## 独立验证

- `cargo test --manifest-path packages/app-lite-core/Cargo.toml`：PASS，85 tests（53 unit + 4 document + 13 migration + 7 repository flow + 8 resource store）。
- `cargo test --manifest-path packages/app-lite-core/Cargo.toml --features test-support resource_entity_ids_retry_database_collisions -- --nocapture`：PASS。
- `cargo test --manifest-path packages/app-lite-core/Cargo.toml --features test-support staged_resource_ids_skip_durable_rows_and_a_commit_race_publishes_nothing -- --nocapture`：PASS。
- `cargo test --manifest-path packages/app-lite-gpui/Cargo.toml image_flow_tests -- --nocapture`：PASS，8/8。
- `cargo test --manifest-path packages/app-lite-gpui/Cargo.toml pending_resource_anchor -- --nocapture`：PASS，4/4。
- `cargo test --manifest-path packages/app-lite-gpui/Cargo.toml mounted_attachment_click_selects_the_whole_card_and_double_click_opens_a_verified_copy -- --nocapture`：PASS，1/1。
- `cargo test --manifest-path packages/app-lite-gpui/Cargo.toml mounted_atomic_gap_and_terminal_dead_zone_create_paragraphs_before_typing -- --nocapture`：PASS，1/1。
- `cargo test --manifest-path packages/app-lite-gpui/Cargo.toml -- --nocapture`：本轮较早快照运行了大量测试后以 signal 11 结束；与既往已记录 donor-heavy GPUI 全量 SIGSEGV 表现相同，未把它计作新的 Task 5 finding，但该结果不能替代 standalone/Release acceptance。

目前不能批准。必须先关闭 T5-C1 与全部 Important，再以稳定快照重跑 mutation-sensitive tests、全量 core/test-support、GPUI 精确定向和 Release M1 smoke。

## Fresh review 追加阻断：已落盘删除后的 Undo 仍不能安全跨崩溃恢复

### T5-C2 — durable deletion 之后 Undo 恢复的资源 occurrence 会被下一次 crash recovery 当成“凭空新增”而拒绝

- Evernote 行为权威：`common-editor/.../modules/clipboard/plugin.ts::state.apply` 会把已保存的完整 Selection/插入范围持续 map 到后续事务；`modules/dragdrop/plugin.ts` 同样通过 `tr.mapping.map` 保持资源原子块身份。结合 Task 4/5 的本地 journal 恢复契约，这意味着一次已经正常 snapshot 的资源删除并不会使同一 retained session 的 Undo 历史失效；Undo 恢复出来的原 occurrence 必须和普通文字 Undo 一样能够先 journal、再在崩溃重启后恢复。
- Rust 证据：`/Users/kevinhao/Projects/joplin/.worktrees/joplin-lite-native-rust-mvp/packages/app-lite-gpui/src/app/note_session.rs:687-696` 有意让 `resource_ids` allowlist 在会话内单调保留已删除资源，因此运行中的 Undo 能重新导出原 ID；但一次成功 snapshot 会在 `:1653`（资源事务）或 `:2321`（普通 snapshot completion）把 `journal_base` 更新为已经缩短的当前 snapshot。下一次 Undo 后，`JournalPayload::from_snapshots` 会把恢复后的资源顺序写进 100 ms journal；重启时 `JournalPayload::into_snapshot` 却在 `:650-653` 只允许 journal IDs 是当前 durable `note.resource_ids` 的 occurrence-bounded 有序子序列。对 durable `B,A` 而言，Undo 恢复的 `A,B,A` 必然被拒绝。
- 精确复现：初始 durable 正文资源为 `A,B,A`；删除首个 `A` 并等待 500 ms snapshot（或 ManualSync），确认数据库变为 `B,A`；在同一未重开的 session 执行 Undo，正文回到 `A,B,A`；只推进 100 ms 让 journal 成功，不让 500 ms snapshot 完成；模拟崩溃并用当前数据库 note + journal 重开。现实现会报“编辑日志引用了当前持久化基线之外的资源或改变了资源顺序”，而不是恢复 Undo 后的 `A,B,A`。
- 为什么违反 Task 5：Task 5 Step 2 明确要求资源删除、Undo/Redo 和其他文档事务共用同一历史；Task 4 journal 是 Task 5 写作会话的崩溃安全边界。当前实现把“同一 session 已授权的历史恢复”与“journal 凭空制造资源”混为一谈，导致一次合法 Undo 在 100–500 ms 崩溃窗口丢失，并可能使整篇 note 无法挂载。
- 现有测试为何不足：`deleting_a_persisted_image_then_undo_redo_journals_and_compacts_the_current_relation_set` 在每个动作之后都直接 ManualSync，没有在 Undo journal 与 snapshot 之间重启；`crash_journal_recovers_an_ordered_a_b_a_resource_deletion_without_losing_writer_ownership` 只测 `A,B,A -> B,A` 的收缩方向，恰好满足 subsequence 检查。两者都不会杀死当前错误分支。
- 最小修复方向：把 retained session 对历史资源 occurrence 的授权以可严格验证的形式跨 journal/restart 保存（例如 journal 携带并校验 durable authorization baseline，或数据库保留可验证的 tombstoned note-resource occurrence）；恢复前仍需验证 ID 格式、资源行/字节关系、正文顺序与授权 occurrence，不能简单取消 fail-closed 检查。新增 mutation-sensitive 组合测试：`A,B,A` → 删除首 `A` → exact snapshot → Undo → exact 100 ms journal → drop/reopen → `A,B,A` → exact 500 ms snapshot → 二次 reopen；同时断言 writer token/sequence ownership、expected revision、canonical HTML 与 `note_resources` occurrence 顺序一致，并加一个伪造未授权 ID 的反例证明放宽不会失守。

## Fresh review checkpoint：持久图片 hydration 已关闭原 T5-I1

- `NoteSession::prepare` 现在只从 canonical document 生成几何描述符，不再调用 repository 读 blob；只有 renderer 通过 `EditorCore::request_image_hydration` 报告 visible/prefetch resource 后，`NoteSession` 才启动 retained background worker。
- 生产队列为一个 active + 一个 latest pending；快速滚动时中间 viewport 不会累积。worker 先写入 task-owned sibling staging，只有存活的 exact session 回调才 O(1) adopt；丢弃 session 的迟到完成不会重建已清理的 editor root。
- canonical `ImagePresentation` 现在持久 natural size/display width；新格式图片的 hydration 只改 presentation cache，不改 document geometry。legacy 无尺寸图先用稳定 fallback，首次 visible hydration 后以无 History 的内部几何修复触发普通 snapshot；二次重开从 canonical attrs 直接使用真实竖图尺寸。
- 独立通过：`mounted_persisted_images_paint_before_only_visible_blob_hydrates`、`opening_many_images_keeps_all_original_blobs_unverified_until_surface_residency`、`hydration_scroll_coalesces_queued_work_to_the_latest_resident_image`、`dropped_session_hydration_worker_never_recreates_its_image_cache_root`、`visible_legacy_image_repairs_its_geometry_once_then_reopens_without_layout_jump`、`mounted_missing_persisted_image_fails_only_after_paint_and_keeps_the_note_surface`。这些测试分别对 verified-open 次数/顺序、队列淘汰、drop 后目录、undo depth、canonical attrs 和第二次重开尺寸做了 mutation-sensitive 断言。

## Fresh review 新增 Important

### T5-I5 — 所谓“后续合法 image UTI fallback”只测了非空字节，首个格式伪装候选仍会阻断真正可解码候选

- Evernote 行为权威：`common-editor/.../modules/clipboard/commands/paste.ts::execCommand` 把 pasteboard 解析成可插入的 slice/content 后才在已保存 Selection 上执行一次 replace/dispatch；`modules/clipboard/plugin.ts::state.apply` 保持该选区映射。不可把“UTI 宣称它是图片”当成“已成功解码的图片”。
- Rust 证据：`/Users/kevinhao/Projects/joplin/.worktrees/joplin-lite-native-rust-mvp/packages/app-lite-gpui/src/native_editor/images.rs:2517-2526` 在第一个 `NativeImageRead::Payload` 立即停止；`:2591-2609` 只检查 NSData 非空、不超 10 MiB 并 copy，完全没有证明 bytes 与宣称 format 匹配。真正解码验证延迟到 background `ResourceImport::from_image_payload`，一旦失败，后续 UTI 候选已经被丢弃。
- 现有测试的自证缺口：`native_image_uti_seam_uses_a_later_valid_image_after_a_rejected_representation` 把 `fixture_png_bytes()` 包成 `ImageFormat::Tiff`；它对不合法 TIFF 断言 `Payload`，并不走真实 background normalize。因此删掉或破坏格式验证也不会使该测试失败。
- 精确重现：提供非空但损坏的 `public.png` NSData，同一 pasteboard 同时提供真实可解码的 `public.tiff`。前台把损坏 PNG 标为 Payload 后停止；background 拒绝 PNG，整次粘贴失败，不会尝试 TIFF。
- 为何违反 Task 5：产品要求 image-first 且失败可见，并要求格式验证不占用 GPUI 前台。当前在“前台不解码”与“后续合法候选可 fallback”之间只实现了前者。
- 最小修复方向：AppKit 主线程只按 UTI 顺序复制受总预算约束的 owned candidates；`ResourceStageJob` 在 background 按顺序调用真实 `ResourceImport::from_image_payload`，选第一个可解码候选。添加端到端 seam test：损坏 PNG + 真 TIFF → 实际插入 TIFF；全损坏/全超限 → 显式失败且无 resource/note/outbox/event；破坏真实 normalize 时测试必须变红。

### T5-I6 — Finder/file-URL 路径检查和 HTML data-image 解码仍在 GPUI action 前台执行

- Evernote 行为权威：`common-editor/.../modules/dragdrop/plugin.ts` 的 drop 路径先保存/映射插入位置，异步内容就绪后才在 `tr.mapping.map(insertPos)` 所得位置 dispatch；`modules/clipboard/commands/paste.ts::execCommand` 同样先解析内容再以保存 Selection 单次插入。这些规则不要求 GPUI 输入回调去阻塞等待外部文件系统。
- Rust 证据：`/Users/kevinhao/Projects/joplin/.worktrees/joplin-lite-native-rust-mvp/packages/app-lite-gpui/src/native_editor/images.rs:571` 在 clipboard file URLs 上同步调用 `Path::is_file`；`:631-635` 的 native/GPUI representation merge 通过 `is_supported_image_path`（`:684-686`）又做一次前台 `is_file`；`:658-662` 在 Finder drop 同样对每个路径同步 `is_file`。这些都由 `LibraryShell::paste_resource_or_text` / `on_external_paths_drop` 的 GPUI update 直接调用。对离线网络卷、故障 FUSE 或正在唤醒的外盘，这个 metadata syscall 可卡住绘制/输入线程，而真正的 no-follow 描述符验证本来已在 background `ResourceImport::from_path`。`:577-596` 还在前台对整段 HTML 做 lower-case 副本并对 data URI 直接 base64 decode，且 decode 前没有 `MAX_IMAGE_BYTES` 上界。
- 精确重现：把 Finder 路径放在延迟/不响应的 mounted volume，或粘贴含数十 MiB data-image 的 HTML；调用 drop/Paste 后，在 ResourceStageJob 被 spawn 之前当前 window 无法先 draw。
- 为何违反 Task 5：Step 3 的资源导入/验证是一条后台 cross-layer transaction，产品目标是粘贴/拖入不打断写作。当前虽然 hash/fsync/SQLite 已后台化，但前置分类仍能在 worker 创建前不受控阻塞/分配。
- 最小修复方向：前台对 file URL/drop 只传送原始 PathBuf，把 exists/type/symlink/size/open/fstat 全部交给 `ResourceStageJob`；HTML data URI 传送受长度上界的 encoded descriptor/string range，在 background 解码和验证。添加 production-path gated test：classifier/descriptor gate 未释放时 window 可先 draw/输入，释放后在原 tracked Selection 插入；超限 data URI 不分配/不落盘/不产生 DB 侧效应。

## Fresh review checkpoint：T5-C2 已关闭

- 修复没有相信 journal 自报的 allowlist。`LibraryRepository::durable_resource_provenance_for_recovery` 只从 exact note 的当前 canonical body 和 `note_revisions <= expected_revision` 取出历史资源，对每个 ID 计算历史最大 occurrence，且要求对应未删除 resource row 仍存在。`JournalPayload::into_snapshot` 仍先验证 note/revision/writer token、delta、canonical HTML 和 payload relation 精确一致，最后才用该 durable provenance 限制 occurrence；因此可恢复同 session 历史中的 `A,B,A`，但不能借全局已存在的 foreign resource 自授权。
- 独立通过：`crash_journal_recovers_historical_a_b_a_after_durable_deletion_then_undo`、`crash_journal_rejects_a_resource_outside_same_note_durable_provenance`、`crash_journal_recovers_an_ordered_a_b_a_resource_deletion_without_losing_writer_ownership`。正向测试实际执行 `A,B,A -> B,A` snapshot、Undo、100 ms journal、drop/recovery ownership claim、snapshot compaction 和二次 reopen；反向测试向真 SQLite journal 写入一个“全局存在但从未关联此 note”的 resource ID，恢复严格失败且 durable note 不变。

## 2026-09-11 23:30 fresh checkpoint（取代此前 finding 计数）

当前结论：**CHANGES REQUESTED — 0 Critical / 1 Important / 0 Minor**。T5-C1/T5-C2、T5-I1/T5-I2/T5-I3/T5-I5/T5-I6 均已由当前生产路径和 mutation-sensitive tests 关闭；唯一剩余阻断是 T5-I4 的附件临时副本权限与系统 handoff 生命周期。

### T5-I5 已关闭：真实 AppKit collector 与 worker-side decode 已贯通

- `/Users/kevinhao/Projects/joplin/.worktrees/joplin-lite-native-rust-mvp/packages/app-lite-gpui/src/native_editor/images.rs:2696` 的 `native_image_candidates_from` 是 `read_native_pasteboard` 在 `:2827` 实际调用的 production helper，而不是测试专用 collector。它按 `NATIVE_IMAGE_UTIS` 顺序探测全部支持类型，单项仍受 10 MiB 上限，总 owned compressed bytes 受 30 MiB 上限；超出剩余预算的候选会被拒绝但不会停止对后续较小 UTI 的探测。
- `classify_clipboard` 在 `:622-630` 保留有界候选序列；`ResourceImportRequest::normalize` 在 `/Users/kevinhao/Projects/joplin/.worktrees/joplin-lite-native-rust-mvp/packages/app-lite-gpui/src/app/note_session.rs:240-273` 只在 retained background stage worker 内逐候选调用真实 `ResourceImport::from_image_payload`，因此声明格式与实际签名不符会失败并继续下一项。
- production collector seam test 强制观察全部 UTI 查询与候选顺序；mounted test 使用坏 PNG、坏 TIFF、真 JPEG，最终断言 durable metadata 为 `image/jpeg`/`jpg`，不是只看“出现了一张图”。全损坏反例同时断言可见错误、live document 无 image、note revision/resource relation 不变、`resources`/`note_resources` 为零、outbox 不增加且无 LibraryEvent。
- 独立运行 `mounted_paste_tries_a_later_native_image_candidate_on_the_retained_worker`、`mounted_all_invalid_image_candidates_publish_no_resource_or_note_side_effects` 和 `native_image_uti_seam_`：均 PASS。

### T5-I6 已关闭：外部路径验证和 data-URI decode 已离开 GPUI action 前台

- `classify_clipboard` 与 `classify_drop` 已不调用 `Path::is_file`。它们只保留至多八个 `PathBuf`；`ResourceImportRequest::first_valid_path_candidate` 才在 retained worker 内按序执行 `ResourceImport::from_path` 的 `symlink_metadata`、no-follow open、descriptor identity/size 检查。缺失路径在 stage gate 未释放前仍能 paint 且不会被 UI callback 同步拒绝；symlink 首项加合法第二项会由 worker 跳过前者并提交后者。
- HTML image classifier 在 `/Users/kevinhao/Projects/joplin/.worktrees/joplin-lite-native-rust-mvp/packages/app-lite-gpui/src/native_editor/images.rs:773-817` 只做有界 case-insensitive descriptor scan，返回 `PasteIntent::EncodedImage`；base64 解码位于 `EncodedImagePayload::decode_bounded`（`:510-530`），由 `ResourceImportRequest::normalize` 在 background stage worker 调用。单元测试直接要求 classifier 返回 encoded variant 且保留原始 base64，并以超限 descriptor 反例杀死前台解码/无界输入；mounted gate 再证明同一 descriptor 通过真实 session worker 插入。
- 独立运行 `mounted_drop_defers_external_path_validation_to_the_retained_stage_worker`、`mounted_drop_skips_an_unsafe_first_path_and_commits_the_later_valid_candidate`、`html_data_uri_classifier_retains_bounded_encoded_bytes_for_the_worker`、`mounted_html_data_uri_stays_encoded_until_the_retained_stage_worker`：均 PASS。

### 当前唯一 Important：T5-I4 仍未修复

- 当前 `/Users/kevinhao/Projects/joplin/.worktrees/joplin-lite-native-rust-mvp/packages/app-lite-gpui/src/app/note_session.rs:337-345` 仍从 `PATH` 启动 `open` 并只等待 `.spawn()` 成功；`:371-406` 的 attachment 目录/leaf 仍使用默认权限；`:1937-1982` 仍把副本写进 EditorCore session root，success 只返回 ResourceId，不持有 materialized path/lease。
- 真实失败序列不只是“成功后永久泄漏”：若 background blocking worker 已开始，用户在 opener 接管之前切换笔记，旧 ImageStore 会删 session root；worker 既可能把已打开/即将打开的路径交给尚未完成的 LaunchServices，也可能在 root drop 后重新 `create_dir_all`，形成无人持有的 orphan。现有 mounted test 的 injected opener 同步读取并立刻返回，只验证“完成后切换删除”，无法覆盖 opener-return 前的 race。
- 最小验收不变：独立 worker-owned lease/unique private root，Unix root 0700、temp/final 0600，绝对 `/usr/bin/open` + `.status()`；活 session 只在成功 handoff 后收养 lease 并保持到切换，session 已 drop 时 completion/result 自清。测试必须 gate opener：切换后、opener return 前 path 仍可读；return 后 worker/session drop 清理；另断言权限、非零 opener 退出为可见错误、无 document mutation，且 symlink 目标不能被跟随。

## 2026-09-12 00:10 final-full-suite checkpoint（再次取代此前 finding 计数）

当前结论：**CHANGES REQUESTED — 0 Critical / 1 Important / 0 Minor**。原 T5-I4 已关闭，但最终 GPUI 全量回归揭示一个此前定向 hydration 测试没有覆盖的 legacy 图片持久化回归；因此当前仍不能批准。

### T5-I4 已关闭：附件 handoff 与敏感临时文件权限形成生产闭环

- `/Users/kevinhao/Projects/joplin/.worktrees/joplin-lite-native-rust-mvp/packages/app-lite-gpui/src/app/note_session.rs:385-450` 的 `AttachmentMaterializationLease` 位于独立 worker-owned root，不再挂回 `ImageStore`/editor session root；shared parent symlink 会 fail-closed，leaf root 显式 0700，completion 对 live session 才收养 lease，旧 session 的 weak completion 失败时由 result drop 自动清理。
- `materialize_verified_attachment_reader`（同文件 `:546-577`）使用 create-new、`O_NOFOLLOW|O_CLOEXEC`、0600、固定缓冲、fsync、atomic rename；生产 `system_attachment_opener` 使用绝对 `/usr/bin/open` 并等待 `.status()` 成功后才发布 success。
- mounted surface 双击 success 测试证明 opener 收到 verified copy、0700/0600、note 无 mutation、成功后切换删除；gated worker 测试证明 opener 返回前 reducer-driven note switch 不会删除 path，返回后旧 session completion 自清；failure 测试证明 opener error 可见且文档/revision 不变。独立运行 `mounted_attachment_` 4/4、private image materialization mode 1/1 均 PASS。

### T5-I7 — legacy 图片在真正 hydration 前发生任意普通保存，会把假的 1024×768 fallback 永久写入 canonical HTML

- Evernote/产品行为权威：`resource/image/imagecomponent.tsx` 的 node view 以资源真实 natural width/height 建立稳定 extent；Task 5 要求 legacy 缺尺寸图片仅在首次后台 hydration 后做一次无历史 geometry repair，并由普通 snapshot 持久化，不能在资源尚未读取时把占位几何冒充真实事实。
- Rust 精确证据：`/Users/kevinhao/Projects/joplin/.worktrees/joplin-lite-native-rust-mvp/packages/app-lite-gpui/src/native_editor/codec.rs:203-220` 将 canonical `ImagePresentation.natural_size=None` 导入为 native `(1024,768)`；同文件 `:354-373` 又对任何 native Image 无条件导出 `natural_size: Some(*natural_size)`。`NoteSession::prepare_persisted_image_hydration` 虽在 `/Users/kevinhao/Projects/joplin/.worktrees/joplin-lite-native-rust-mvp/packages/app-lite-gpui/src/app/note_session.rs:951-982` 暂时另存 `needs_legacy_geometry_repair`，这个 flag 不在 immutable editor snapshot/codec 输出中；因此它无法保护 hydration 前的 journal/snapshot。
- 精确重现：打开一篇首屏外含 legacy 竖图、canonical 没有 width/height attrs 的笔记；不滚动到图片，先编辑标题或可见正文并等待 500 ms snapshot（或 ManualSync）。图片从未进入 visible hydration，但保存已经把 `Some((1024,768))` 写入 canonical HTML。重启后 `presentation.natural_size.is_none()` 为 false，`needs_legacy_geometry_repair` 不再设置；即便之后滚动并解码真实竖图，也不会执行 legacy repair，永久保持 4:3 错误 geometry。
- mutation-sensitive 证据：最终命令 `cargo test --quiet --manifest-path packages/app-lite-gpui/Cargo.toml --bin velotype -- --skip editor::selection::tests::cross_block_cut_writes_markdown_deletes_range_and_undo_restores` 得到 1140 PASS / 2 FAIL / 1 精确 donor skip。其中 `native_editor::codec::tests::task_four_codec_round_trips_every_structural_block_and_mark` 在 `/Users/kevinhao/Projects/joplin/.worktrees/joplin-lite-native-rust-mvp/packages/app-lite-gpui/src/native_editor/codec.rs:879-918` 直接显示 `None -> Some(1024,768)`；这不是可更新 expected 的陈旧测试，而是 Task 4 全结构 roundtrip 与 Task 5 legacy repair 的真实回归。
- 最小修复方向：让“natural size 尚未知”成为 native Image/immutable snapshot 可保留的 typed 状态，或在 snapshot codec 中按 exact node/resource 的 legacy flag 覆盖为 `None`；只有成功 hydration 的无历史 repair 才转成 `Some(real_size)`。新增 mounted 组合测试：offscreen legacy 竖图 → 先编辑文字并 exact snapshot → reopen 后 canonical 仍为 `None` → scroll 到图并完成 hydration → 一次无历史 repair + snapshot → 二次 reopen 为真实尺寸，且 undo depth、resource relation、save generation 无额外污染。

### 非阻断但必须修绿的陈旧测试

- `native_editor::images::tests::production_clipboard_classification_moves_large_image_bytes` 当前构造 16 MiB payload，却仍要求得到 `PasteIntent::Image`；产品硬上限 `MAX_IMAGE_BYTES` 为 10 MiB，生产拒绝正确。测试应改成“上限内大 payload 零额外复制地 move”并另断言超限明确 Unsupported，不能为让测试通过而放宽 intake 上限。

### 本 checkpoint 独立验证

- app-lite-core `--features test-support`：PASS，111 tests（54 + 4 + 20 + 7 + 18 + 8）。
- app-lite-native 全量：PASS，225 tests（133 + 85 + 7）。
- `durable_image_materialization_uses_private_directory_and_leaf_modes`：PASS，1/1。
- `mounted_attachment_`：PASS，4/4。
- GPUI bin 全量（只精确跳过已知 donor `cross_block_cut...`）：**FAIL，1140 PASS / 2 FAIL / 1 skip**；一个为上述 T5-I7，另一个为陈旧 16 MiB clipboard 测试。
- `git diff --check`：PASS。

## 2026-09-12 最终稳定快照复审（最终结论）

结论：**APPROVED — 0 Critical / 0 Important / 1 Minor**。本节取代此前所有阶段性 finding 计数；T5-I7 与全量回归阻断均已关闭。这里批准的是 Task 5 当前代码与自动化验证。按 roadmap/实现报告，M1 的 fresh-profile Release 人工流程仍必须单独完成，不能由本批准替代。

### T5-I7 已关闭：unknown natural size 只在 fully rendered 后转成真实持久尺寸

- 重新阅读 `/Users/kevinhao/Projects/joplin-reconstruction/evernote-11.32.5/common-editor-sourcemap/@evernote/common-editor/src/apps/peso/modules/resource/image/imagecomponent.tsx:168-203`：`useNaturalDims` 读取 node attrs；`syncNaturalDims` 只有在图片 fully rendered、DOM naturalWidth/naturalHeight 非零且确实变化时才写回 node，并显式设置 `user-generated=false`、`addToHistory=false`。因此“未知”不能由占位尺寸提前变成“已知”，真实解码完成后的修复也不能污染用户撤销历史。
- Rust 现在把这个边界编码成 typed state：`/Users/kevinhao/Projects/joplin/.worktrees/joplin-lite-native-rust-mvp/packages/app-lite-gpui/src/native_editor/model.rs:142-153` 的 Image 持有 `natural_size_known`；新插入图片在 `:416-429` 以真实 inspect 结果设为 true。legacy import 在 `/Users/kevinhao/Projects/joplin/.worktrees/joplin-lite-native-rust-mvp/packages/app-lite-gpui/src/native_editor/codec.rs:203-220` 设 false 并仅把 1024×768 当布局 fallback；export 在 `:355-374` 对 false 写回 `None`。`SetImageNaturalSize` 在 `/Users/kevinhao/Projects/joplin/.worktrees/joplin-lite-native-rust-mvp/packages/app-lite-gpui/src/native_editor/model.rs:3296-3326` 只有真实 hydration repair 才同时更新尺寸并把 known 设为 true；`EditorCore::repair_legacy_image_natural_sizes` 仍绕过 History，但走真实 document revision/layout invalidation。
- 新组合测试 `/Users/kevinhao/Projects/joplin/.worktrees/joplin-lite-native-rust-mvp/packages/app-lite-gpui/src/app/note_session_tests.rs:1635-1742` 实际覆盖：legacy 竖图保持非 resident → 先通过 EntityInputHandler 编辑正文并 ManualSync → reopen 后 canonical 仍没有尺寸 attrs → 首次 hydration 测出 675×1200 → 无历史 repair + snapshot → 二次 reopen 保持 675×1200。它会在移除 `natural_size_known`、export 无条件 Some、或提前把 fallback 标为已知时失败。
- Task 4 全结构/marks codec roundtrip 已恢复；原 16 MiB 陈旧 clipboard 测试现在拆成 `<= MAX_IMAGE_BYTES` 保留 owned-buffer move identity，以及 `> MAX_IMAGE_BYTES` 必须 Unsupported 且不得退回伴随文本，既不放宽 10 MiB 上限，也不弱化 image-first。

### Minor

#### T5-M1 — AppKit 文件列表仍在主线程复制全部路径，八项候选上限应用得太晚

- `/Users/kevinhao/Projects/joplin/.worktrees/joplin-lite-native-rust-mvp/packages/app-lite-gpui/src/native_editor/images.rs:2925-2934` 对 `NSFilenamesPboardType` 的全部 `files.count()` 构造 `PathBuf`；只有之后 `classify_clipboard` 才 `.take(MAX_CLIPBOARD_PATH_CANDIDATES)`。极端大量 Finder 项目会在 GPUI action 前台产生不必要的无界路径复制。
- 这不做 stat/open/read，真正路径验证和 blob I/O 仍在 retained worker，正常八项以内流程不受影响，因此为非阻断 Minor。
- 最小修复：collector 循环直接限制为八项，并加 AppKit snapshot seam 反例，证明第九项以后不被复制；不得把限制推回 worker 后才做。

### 最终独立验证

- `offscreen_legacy_image_keeps_unknown_geometry_until_visible_repair`：PASS，1/1。
- `task_four_codec_round_trips_every_structural_block_and_mark`：PASS，1/1。
- `production_clipboard_classification_`：PASS，2/2（上限内 move + 超限拒绝）。
- app-lite-core 默认 features：PASS，85/85。
- app-lite-core `--features test-support`：PASS，111/111。
- GPUI bin 全量，仅精确跳过已知 donor `editor::selection::tests::cross_block_cut_writes_markdown_deletes_range_and_undo_restores`：PASS，1144 passed / 0 failed / 1 filtered，15.63 s。
- GPUI/core `cargo fmt --check`、GPUI `cargo check --tests`、`git diff --check`：PASS。
- `task-5-report.md` 已逐项提供 Evernote source → Rust production path → mutation-sensitive test crosswalk，并明确把 `/usr/bin/open.status()`、journal/recovery、内存预算等本产品策略标为独立架构决定，没有冒充 Evernote 逐行复刻。
