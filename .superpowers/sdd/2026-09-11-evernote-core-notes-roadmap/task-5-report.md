# Task 5：图片与附件跨层事务 — 实现与证据报告

状态：实现、定向验证、独立 GPUI 全量验证和 fresh-profile Release M1 手工验收均已完成。首次实机显示发现的右侧编辑壳/标题区透出黑底已按 Evernote primary-surface token 修复并在本轮 fresh profile 重验；提交与推送在本报告写回后的最终验证通过后执行。

基线：`a91b842da97ee48db2f05e7bf0cacfd6e637a020`。本轮产物仍在共享脏工作树中。Task 5 只实现本地资源导入、持久关系、画布呈现、保存恢复和最小附件卡；没有扩展到 Task 6+ 的组织、搜索、同步或迁移功能。

## Release 实机回归：编辑壳黑底（2026-09-12）

在 fresh profile 的默认 light route，右侧 editor shell、现有 actions 行和 title 周围原先依赖透明窗口默认值；正文 surface 和左侧列表虽为白色，但 title 的深色文字可透出黑底而不可见。已将整条主编辑样式树显式设为 Evernote primary light surface。本轮 Step 5 完整临时 profile 手工流程已重验空资料库、标题、正文、图片和重启恢复，没有再次出现黑底。

## Brief 勾选表

- [x] Step 1：真实 picker completion 在同一 presentation cycle 更新 active document、layout/cache 与 selected card projection。
- [x] Step 2：图片前后输入、原子选择/删除、undo/redo、IME 相邻、长图/原子 gap 事件路径有 mutation-sensitive 覆盖。
- [x] Step 3：资源暂存、受保存 Selection 映射的插入、关系/thumbnail/snapshot/outbox 单事务提交、失败回滚/重试和单次投影可见。
- [x] Step 4：图片 inline；非图片为带文件名、mime、大小、状态和打开动作的 attachment card；字节不进入正文或 GPUI node state。
- [x] Step 5：M1 Release 手工验收完成。fresh temporary profile 中创建三篇中文笔记，完成截图 paste、Finder JPEG drop、PDF picker、图片前后继续输入、快速切换、进程退出与同 profile 重启；重启后标题、正文和图片首帧恢复，数据库关系与 blob 哈希逐一核验通过。

## Evernote 源码 → Rust 实现 → 突变敏感验证

下列“观察行为”均来自本轮实际重新阅读的解包源码；路径从解包根目录起均为绝对路径。每行右列是会在删除/绕过关键生产语句时失效的测试，而不是仅模拟内部 helper 的同名单元测试。

| Evernote 源码路径、符号与观察行为 | 本产品 Rust 实现 | mutation-sensitive 验证与结果 |
| --- | --- | --- |
| `/Users/kevinhao/Projects/joplin-reconstruction/evernote-11.32.5/common-editor-sourcemap/@evernote/common-editor/src/apps/peso/modules/content/commands/exportDesignTokens.generated.ts`：`--colors-grey-100: #fff`；`--color-background-fill-primary` 与 `--color-surface-fill-primary-enabled` 都解析到它。主编辑区不能依赖宿主窗口的透明/外观默认值。 | `packages/app-lite-gpui/src/ui/mod.rs::EVERNOTE_LIGHT_PRIMARY_SURFACE`、`LibraryShell::evernote_primary_surface_fill`，并直接作为 `library-shell`、`library-main-editor-shell`、`library-actions`、`library-note-title`、`library-native-editor-pane` 五个生产 `Div::bg` 的参数。 | `ui::tests::mounted_default_editor_shell_paints_every_evernote_primary_surface` PASS；实际 mount/card-select/redraw 后，结构化 render seam 记录五个 `.bg` 参数均为 `#ffffffff`，并确认五个 selector 都在样式树中。变异证明：临时删除标题 `.bg(...)` 后同一测试 RED 为 `[0xffffffff, 0xffffffff, 0xffffffff, 0, 0xffffffff]`；恢复后 GREEN。 |
| `/Users/kevinhao/Projects/joplin-reconstruction/evernote-11.32.5/common-editor-sourcemap/@evernote/common-editor/src/apps/peso/modules/resource/image/imagecomponent.tsx::useRenderableUrl`：取得 renderable URL 后直接 `setNodeAttribute` 并 `dispatch`；标记 `user-generated=false`、`addToHistory=false`。资源 hash 改变会清旧 URL。`syncNaturalDims` 只在 fully rendered 后写真实 naturalWidth/naturalHeight，同样不进入用户历史。 | `packages/app-lite-gpui/src/app/note_session.rs::drain_image_hydration_requests/start_next_image_hydration/finish_image_hydration/apply_pending_legacy_image_repairs`；`native_editor/core.rs::request_image_hydration/repair_legacy_image_natural_sizes`；`native_editor/codec.rs::import_canonical_with_resources/export_canonical_with_resources`。持久格式已有尺寸的 hydration 只注册 source、不改 geometry；legacy `natural_size_known=false` 保留稳定 1024×768 fallback，成功可见 hydration 才作无 history 真实尺寸 repair。 | `mounted_persisted_images_paint_before_only_visible_blob_hydrates`、`visible_legacy_image_repairs_its_geometry_once_then_reopens_without_layout_jump`、`offscreen_legacy_image_keeps_unknown_geometry_until_visible_repair`、`task_four_codec_round_trips_every_structural_block_and_mark` 均 PASS。最后一项会杀死 `None` 跨 codec 的保留；offscreen 组合测试覆盖“普通文字保存/reopen 仍 None，首可见 decode 后才变真实竖图尺寸”。 |
| 同文件 `ImageView`：node attrs/resource 是渲染源；loading/loaded 分支切换时重新挂 `ResizeObserver`，保留图片区空间以避免跳变。 | `native_editor/render.rs::shape_visible` 和 `layout.rs::image_layout_size` 按文档持久 presentation 先画 placeholder extent；`images.rs::ImageStore`/`BudgetedImageCache` 只为 current resident materialize/decode。 | `mounted_picker_completion_immediately_updates_surface_cache_and_selected_card` 断言当前 frame 的 block、extent、cache 与 projection 同时变化；`mounted_persisted_images_paint_before_only_visible_blob_hydrates` 断言首帧可画且非可见原图不读；`hydration_scroll_coalesces_queued_work_to_the_latest_resident_image` 断言 active + latest resident 有界。均 PASS。 |
| `/Users/kevinhao/Projects/joplin-reconstruction/evernote-11.32.5/common-editor-sourcemap/@evernote/common-editor/src/apps/peso/modules/clipboard/plugin.ts::state.apply`：保存完整 Selection 并持续映射；`.../modules/clipboard/commands/paste.ts::execCommand`：收集多表示、解析/transform/normalize 后，以一次 replace/dispatch 结束。 | `app/note_session.rs::capture_resource_insert_intent/capture_resource_insert_intent_at/start_staged_resource_commit/finish_resource_commit` 保存 note identity、完整 directional Selection 和编辑 mutation mapping；`ui/mod.rs::complete_resource_picker_path/paste_resource_or_text` 与 picker/paste/drop 复用这条路径。 | `pending_resource_anchor` 系列、`mounted_finder_drop_completion_queues_through_the_same_saved_point_fence`、`mounted_resource_commit_keeps_live_typing_while_the_sqlite_worker_is_gated`、`mounted_resource_commit_failure_at_default_right_caret_retries_without_losing_suffix_text` 均 PASS。覆盖 prefix insert/delete、selection replacement、节点删除/会话不匹配、后台期间继续中文输入与 retry。 |
| `/Users/kevinhao/Projects/joplin-reconstruction/evernote-11.32.5/common-editor-sourcemap/@evernote/common-editor/src/apps/peso/modules/dragdrop/plugin.ts::handleDrop` 与 `.../dragdrop/dragdrop.ts::getDropInsertPos`：坐标解析 `$mouse`，复用 clipboard parse/process；先删除/映射 insert position，再在一个 transaction 最终 dispatch，保存后的 selection 通过 mapping 解析。 | `ui/mod.rs::on_external_paths_drop` → 同一 `ResourceImportRequest`/`NoteSession` staged flow；`native_editor/core.rs` 的 tracked `ResourceInsertAnchor` 和 transaction splice mapping；`app/note_session.rs::finish_resource_commit` 只在 canonical SQLite 成功后发布合并结果。 | `mounted_finder_drop_completion_queues_through_the_same_saved_point_fence`、`mounted_drop_defers_external_path_validation_to_the_retained_stage_worker`、`mounted_drop_skips_an_unsafe_first_path_and_commits_the_later_valid_candidate` 均 PASS。测试会在删除 saved selection 或使用 live caret 时失败。 |
| `/Users/kevinhao/Projects/joplin-reconstruction/evernote-11.32.5/common-editor-sourcemap/@evernote/common-editor/src/apps/peso/modules/textbetweenblocks/plugin.ts::insertOrFocusParagraph/textBetweenBlocksPlugin`：原子 section block 间 dead zone 优先聚焦相邻 text block，否则插 paragraph、TextSelection、scrollIntoView。`.../modules/noteendparagraph/plugin.ts`：末块下方也创建尾段。 | `native_editor/surface.rs::EditorSurface::on_mouse_down` 对 dead-zone 与 atom interior 分类；`layout.rs::atomic_dead_zone_hit`；`model.rs::Transaction::EnsureParagraph` 与 `core.rs::select_atomic_at`。图片/附件内容单击走完整 atomic selection；仅 gap/tail 生成段落。 | `mounted_atomic_gap_and_terminal_dead_zone_create_paragraphs_before_typing` 与 `mounted_text_image_text_uses_surface_keys_for_atomic_boundaries_and_history` 均 PASS。后者还断言 image NodeSelection、Backspace/Delete、undo/redo 和图片前后真实 key input。 |
| `/Users/kevinhao/Projects/joplin-reconstruction/evernote-11.32.5/common-editor-sourcemap/@evernote/common-editor/src/apps/peso/modules/resource/fileplugin.ts::handleResourceEvents`：编辑态单击 file node 设 `NodeSelection`；双击按客户端策略交给 open/save 路径，分离“选择”与“打开”。 | `native_editor/surface.rs` 发 typed `EditorSurfaceEvent::OpenAttachment`；`core.rs::select_atomic_at` 建完整 block selection；`app/note_session.rs::open_attachment/perform_attachment_open/system_attachment_opener` 物化经验证的副本并打开。 | `mounted_attachment_click_selects_the_whole_card_and_double_click_opens_a_verified_copy`、`mounted_attachment_open_lease_survives_a_switch_until_the_worker_returns`、`mounted_attachment_open_failure_is_visible_without_mutating_the_document` 均 PASS：包含高亮、注入 opener 的资源 ID/字节、0700/0600、切换竞态和失败 notice。 |
| `/Users/kevinhao/Projects/joplin-reconstruction/evernote-11.32.5/main-readable/diagnostics/deobfuscated-main.js::ENStagedBlobManager.stageBlobForUpload`（约 520765–520790）：先分配/stage blob，再在 blob-storage transaction 发布 staged metadata；`AttachmentRepositoryImpl.content/contentForEditor/fetchBytes`（约 550387–550446）：元数据、可编辑内容和 bytes fetch 分开，本地 staged content 可重试。 | `app-lite-core/src/repository.rs::stage_resource/stage_resource_reader/commit_staged_resource_snapshot`；`app/note_session.rs::perform_resource_stage/perform_resource_commit/finish_resource_commit`。blob stage 在最终 SQLite transaction 前不发布；成功一次提交 note snapshot、resources、note_resources、thumbnail、search/outbox 与单一 projection 事件。 | `mounted_lifecycle_switch_waits_for_a_gated_resource_stage_then_commits_it`、`mounted_resource_commit_keeps_live_typing_while_the_sqlite_worker_is_gated`、`mounted_resource_commit_failure_rolls_back_only_the_optimistic_atom_and_keeps_later_text`、`mounted_all_invalid_image_candidates_publish_no_resource_or_note_side_effects` 均 PASS。失败测试断言无 partial resource/relation/outbox/event，或 rollback 后正文/undo-redo 可恢复。 |
| 同一 `AttachmentRepositoryImpl`：attachment 的 metadata/content 分离，按需取得 bytes，而非把 bytes 塞进编辑节点。 | `native_editor/images.rs::ResourceImport/ResourceSource/ImageStore` 与 render attachment card；`BlockContent::Attachment` 仅保存 resource_id、filename、mime；`AttachmentMaterializationLease` 仅在用户双击时流式物化。 | `mounted_attachment_picker_paints_a_durable_card_without_reopening_note`、`mounted_attachment_click_selects_the_whole_card_and_double_click_opens_a_verified_copy` 均 PASS，断言 document/resource relation 与 card 对应且 card 不保留 bytes。 |

## 关键完整性与性能契约

- **一次可见提交：** `LibraryRepository::commit_staged_resource_snapshot` 是资源 metadata、正文、顺序 relation、thumbnail、search/outbox 的单 SQLite transaction；commit 前不发资源/同步/投影事件。`mounted_all_invalid_image_candidates_publish_no_resource_or_note_side_effects` 和 repository transaction tests 保护该边界。
- **删除/Undo/journal：** `capture_committed_snapshot` 每次从 immutable document 导出当前有序 resource IDs；`JournalPayload::into_snapshot` 用 repository 的同 note durable provenance 限制 occurrence，不信任 journal 自报。`deleting_a_persisted_image_then_undo_redo_journals_and_compacts_the_current_relation_set`、`crash_journal_recovers_historical_a_b_a_after_durable_deletion_then_undo` 与 foreign-ID negative test 均 PASS。
- **低内存与前台响应：** native UTI 仅搬运受总 30 MiB 预算限制的压缩候选，单图 10 MiB；HTML data URI 保留 bounded encoded descriptor，path only 在 worker 做 no-follow/type/size 检查。`native_image_uti_seam_*`、`mounted_paste_tries_a_later_native_image_candidate_on_the_retained_worker`、`mounted_html_data_uri_stays_encoded_until_the_retained_stage_worker`、`mounted_drop_defers_external_path_validation_to_the_retained_stage_worker` 均 PASS。
- **附件 handoff 安全：** `AttachmentMaterializationLease` 使用独立、不可跟随 shared parent 的 0700 root 和 0600 leaf；`/usr/bin/open.status()` 成功前不发布完成，活会话持有 lease，drop session 的弱 completion 自动清理。`durable_image_materialization_uses_private_directory_and_leaf_modes` 也覆盖 ImageStore materialization 的同等 mode 约束。

## 明确标为独立产品架构决定

以下不是声称逐行复刻 Evernote；它们是本地 Rust MVP 为 Task 4/5 数据安全、macOS API 与低内存目标作出的可验证设计。

- `JournalPayload` writer token/sequence、historical durable provenance、100 ms journal / 500 ms settled / 15 s hard-cap，以及 lifecycle flush barrier：独立 crash-recovery 架构；Evernote source 仅提供 Selection/transaction 连续性的行为参照。
- 使用 `/usr/bin/open.status()` 的通用 macOS opener：独立产品策略。`fileplugin.ts` 的实际行为按 Neutron/Boron/其他 client 分支不同；本实现仅借鉴其 atomic selection 与 typed open 分离，不声称复制任一 client 分支。
- 30 MiB native candidate 总预算、10 MiB inline image 上限、active + latest-resident hydration queue、0700/0600 private materialization：独立低内存/私密笔记安全策略。它们的正确性由上述 worker、queue 和 mode 测试验证。
- canonical `selected_thumbnail_id` 作为事务 outcome 并在同一 frame 更新 card projection：独立的本地 repository/UI contract；不把 thumbnail 解释为编辑器 reload 的副作用。

## M1 Release 实机验收（2026-09-12）

验收使用新编译的 Release 二进制 `/Users/kevinhao/Projects/joplin/.shared-target/release/velotype` 和全新临时 profile `/tmp/joplin-lite-m1-profile.7zu18l`，没有复用开发资料库。实际完成并观察到：

- 空资料库默认 light route 为完整白色主 surface；新建三篇笔记后，标题分别为“第一篇中文笔记”“第二篇中文笔记”“第三篇多媒体验收”，标题和正文作为不同输入区即时保存。
- 在第三篇笔记正文先输入中文，随后从 macOS 剪贴板粘贴 PNG；图片在当前编辑区同一轮立即显示，不需要切换笔记。图片后继续输入“图片后面的文字也能稳定输入，不闪跳。”，文字即时显示，未观察到交替帧跳动。
- 从 Finder 将带 EXIF 方向的 JPEG 拖到编辑区，图片即时显示；通过原生 picker 选择 PDF。提交后 canonical `body_html` 顺序为首段、PNG、后续中文、JPEG、PDF attachment、尾段，`note_resources.position` 为 `0,1,2`。
- 以约 150 ms 间隔在三张笔记卡间往返六次，回到多媒体笔记时首张图片立即出现；退出 Release 进程后用同一 profile 重启，默认恢复上次选中的第一篇笔记，再点回第三篇时标题、正文和首张图片立即恢复。
- SQLite `PRAGMA integrity_check` 返回 `ok`。三篇笔记 revision 为 `4/3/7`；资源表和关系表均为 3 条。PNG、JPEG、PDF 的存储 blob SHA-256 分别为 `a65c3b0a3fb42a04f8c7232a556d9b7b8dbe4ba515766c6e00cb21d08eff840e`、`11a8c656ba8c3cd2b4d1bd349c254b3ea197d6550e3a62046a80987dbc8cb0f9`、`b0d6283e9330ac99ed1765b244713a7f10a393572017674a067baf7d519c5b0e`，逐一等于源文件哈希。

这组手工证据闭合的是 Task 5/M1 日用纵切；它不宣称 Task 6–10 的组织、搜索、迁移、NAS 同步和最终性能打磨已经完成。

## 已执行验证

以下为本轮实际命令/独立复跑结果：

```text
RUSTFLAGS='-Awarnings' cargo test --quiet --manifest-path packages/app-lite-core/Cargo.toml
# PASS: 85 tests

RUSTFLAGS='-Awarnings' cargo test --quiet --manifest-path packages/app-lite-core/Cargo.toml --features test-support
# PASS: 111 tests

RUSTFLAGS='-Awarnings' cargo check --tests --manifest-path packages/app-lite-gpui/Cargo.toml
# PASS

RUSTFLAGS='-Awarnings' cargo test --quiet --manifest-path packages/app-lite-gpui/Cargo.toml \
  --bin velotype -- --skip editor::selection::tests::cross_block_cut_writes_markdown_deletes_range_and_undo_restores
# independent rerun PASS: 1159 passed, 0 failed, 1 exact documented donor test filtered

cargo fmt --manifest-path packages/app-lite-gpui/Cargo.toml -- --check
cargo fmt --manifest-path packages/app-lite-core/Cargo.toml -- --check
git diff --check
# PASS
```

Step 5 的 fresh-profile Release 人工流程已按上节完成；最终提交仍以本轮重新执行的 core、GPUI、Release build、fmt 和 diff 检查全部通过为门槛。

黑底修复后的额外定向验证：

```text
RUSTFLAGS='-Awarnings' cargo test --quiet --manifest-path packages/app-lite-gpui/Cargo.toml \
  ui::tests::mounted_default_editor_shell_paints_every_evernote_primary_surface -- --exact --nocapture
# PASS；并已用临时删除标题生产 .bg(...) 的 mutation run 观察到预期 RED

RUSTFLAGS='-Awarnings' cargo test --quiet --manifest-path packages/app-lite-gpui/Cargo.toml \
  ui::tests::mounted_card_click_reaches_the_same_shell_action_reducer -- --exact --nocapture
# PASS
```

## 默认资料库共享格式 Chrome（2026-09-12）

默认 `LibraryShell` 现已在 `library-note-title` 与 `native-editor-surface` 间 mount 与 `--evernote-spike` 同一个 `native_editor::toolbar::EditorCommandChrome`。它持有 active `NoteSession` 的既有 `EditorCore`，并随 session/surface 在 selection 清空、切 note 或 codec failure 时一同释放；资料库顶层 actions 仍保持自己的职责。

本轮实际重新阅读的行为依据与完整交叉映射见 `task-5-format-chrome-report.md`：

- `/Users/kevinhao/Projects/joplin-reconstruction/evernote-11.32.5/common-editor-sourcemap/@evernote/common-editor/src/apps/peso/commands.ts` 的集中 command 模块；对应 Rust `EditorCommandChrome` + one `CommandCatalogue`，两 host 不复制 button/handler。
- 同解包路径的 `modules/textformatter/commands/boolformat.ts::execCommand/queryCommandValue` 的 single transaction、selection-derived active/mixed state 和 focus restore；对应 `EditorCommandChrome::execute_command` → `CommandCatalogue::execute` → `EditorCore` history。
- `docs/research/evernote-11.32.5-targeted-reverse.md::Toolbar command state and focus preservation` 的 toolbar/More 同 catalogue、popover preserve selection/return focus；对应 `render_for_host`、`render_more_menu`、`render_link_popover` 与 Library overlay placement。

新 mounted mutation-sensitive 结果均 PASS：`mounted_default_route_mounts_the_shared_editor_command_chrome`、`mounted_library_shared_chrome_clicks_bold_and_list_with_one_history_entry`、`mounted_library_chrome_format_manual_sync_switch_and_reopen_round_trips_canonical_html`、`mounted_narrow_library_chrome_moves_list_command_into_shared_more_and_executes_it`、`mounted_library_shared_link_popover_keeps_selection_and_returns_editor_focus`、`mounted_library_note_switch_discards_the_previous_shared_link_and_more_overlays`、`mounted_library_chrome_insert_image_event_uses_the_saved_selection_durable_picker_route`。最后一条断言 typed InsertImage event 先捕获 Library saved Selection，随后经已有 picker completion/staged durable import 发布 resource relation；没有调用 Spike 路径。

在该测试期间发现并修复一个共享 overlay 真回归：Library `EditorSurface` 自身有 capture-phase pointer handler，More/Link window overlays 没有 mouse occlusion 时，点击 More row 会先把正文 caret 移到末尾并使 command 无效。共享 `toolbar.rs` 的 menu/popover 现使用 GPUI `.occlude()`；这同样保护 Spike，而 Spike 的 44 项 toolbar/More/Link regression 仍全部通过。Library placement 也改为真实右侧编辑列宽，而非整窗宽度，以便 narrow layout 的同一 More catalogue 可靠计算。

本检查点随后运行完整 GPUI bin suite（仅跳过记录在案、未改动的 donor `editor::selection::tests::cross_block_cut_writes_markdown_deletes_range_and_undo_restores`）：**1,159 passed / 0 failed / 1 filtered**；`cargo check --tests`、两 crate 的 `cargo fmt -- --check` 和 `git diff --check` 均 PASS。

本节只记录自动化接线证据；Task 5 Brief Step 5 另由上面的 fresh temporary-profile Release 实机流程闭合，没有用绿色测试替代人工门槛。
