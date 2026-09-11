# Task 5：图片与附件跨层事务 — 实现与证据报告

状态：实现与定向/全量自动验证完成；fresh-profile Release M1 手工验收**待重新执行**。本轮实机截图发现右侧编辑壳/标题区黑底后，已核实当时运行的是工作树中陈旧的 local `target/release/velotype`，不是 Cargo 当前共享 target 产物。源码、共享 Chrome/body surface 和分发脚本均已修正；在新 `.shared-target/release/velotype` 上重新目视验收前，不得把 Step 5 或发布状态标为完成。

基线：`a91b842da97ee48db2f05e7bf0cacfd6e637a020`。本轮产物仍在共享脏工作树中。Task 5 只实现本地资源导入、持久关系、画布呈现、保存恢复和最小附件卡；没有扩展到 Task 6+ 的组织、搜索、同步或迁移功能。

## Release 实机回归：编辑壳黑底（2026-09-12）

在 fresh profile 的默认 light route，右侧 editor shell、现有 actions 行和 title 周围会透出黑底；正文 surface 和左侧列表虽为白色，但 title 的深色文字不可见。进程检查证明截图时实际启动的是 `packages/app-lite-gpui/target/release/velotype`（修改时间为前一天 13:04），而仓库 `.cargo/config.toml` 让 Cargo 将新 Release 写到 `/Users/kevinhao/Projects/joplin/.shared-target/release/velotype`。陈旧 local 二进制不含 `library-main-editor-shell` 或 shared Chrome 的当前样式字符串。

生产修复将默认 route 的 shell、主编辑列、actions、title、editor pane、empty/no-selection/unsupported states、shared Library Chrome 和 native body surface 都设为显式不透明 primary surface，并为 title/empty-state 文本使用 Evernote primary/secondary text token；body surface 也有 primary stroke。`scripts/create_macos_app_dist.sh` 现在通过 `cargo metadata` 解析实际 target directory，避免 build 后重新打包陈旧 local binary。自动验证已通过，但完整临时 profile 手工流程必须以新 shared-target Release 重新执行。

## Brief 勾选表

- [x] Step 1：真实 picker completion 在同一 presentation cycle 更新 active document、layout/cache 与 selected card projection。
- [x] Step 2：图片前后输入、原子选择/删除、undo/redo、IME 相邻、长图/原子 gap 事件路径有 mutation-sensitive 覆盖。
- [x] Step 3：资源暂存、受保存 Selection 映射的插入、关系/thumbnail/snapshot/outbox 单事务提交、失败回滚/重试和单次投影可见。
- [x] Step 4：图片 inline；非图片为带文件名、mime、大小、状态和打开动作的 attachment card；字节不进入正文或 GPUI node state。
- [ ] Step 5：M1 Release 手工验收待重新执行。必须从 `cargo metadata` 指向的 shared-target Release 或修复后的 `.app` 启动全新临时 profile，重新完成三篇中文笔记、截图 paste、Finder JPEG drop、PDF picker、图片边界输入、快速切换与重启；在目视确认右侧 shell/title/chrome/body 全程浅色且可读前不得勾选。

## Evernote 源码 → Rust 实现 → 突变敏感验证

下列“观察行为”均来自本轮实际重新阅读的解包源码；路径从解包根目录起均为绝对路径。每行右列是会在删除/绕过关键生产语句时失效的测试，而不是仅模拟内部 helper 的同名单元测试。

| Evernote 源码路径、符号与观察行为 | 本产品 Rust 实现 | mutation-sensitive 验证与结果 |
| --- | --- | --- |
| `/Users/kevinhao/Projects/joplin-reconstruction/evernote-11.32.5/common-editor-sourcemap/@evernote/common-editor/src/apps/peso/modules/content/commands/exportDesignTokens.generated.ts`：`--colors-grey-100: #fff`；`--color-background-fill-primary` 与 `--color-surface-fill-primary-enabled` 都解析到它；`--color-text-fill-primary-enabled` 解析到 `--colors-grey-8: #141414`，primary stroke 解析到 `--colors-grey-95: #f3f2f1`。主编辑区不能依赖宿主窗口的透明/外观默认值。 | `packages/app-lite-gpui/src/ui/mod.rs::{EVERNOTE_LIGHT_PRIMARY_SURFACE,EVERNOTE_LIGHT_PRIMARY_TEXT,EVERNOTE_LIGHT_MUTED_TEXT,EVERNOTE_LIGHT_PRIMARY_STROKE}` 和 `LibraryShell::{evernote_primary_surface_fill,evernote_primary_text_fill,evernote_muted_text_fill}` 作为 shell/main/actions/title/pane 以及 empty/no-selection/unsupported state 的真实样式参数；`native_editor/toolbar.rs::EditorCommandChrome::render_for_host` 给 Library（不改变 Spike）显式 white toolbar/stroke；`native_editor/surface.rs::EditorSurface::render` 给 body surface 显式 white/**#141414 foreground**/stroke。 | `ui::tests::mounted_default_light_route_keeps_every_editor_state_opaque_and_contrasted` PASS：空态、无选择态、已选笔记、共享 Chrome 与 body surface 都经 mount/redraw 读取实际样式调用记录，且 primary/muted text 与 white surface 的对比度达标；body contract 还要求真实调用 #141414 foreground。变异证明：暂时将生产 `EVERNOTE_LIGHT_PRIMARY_SURFACE` 改为 `#000000` 后同一测试 RED；恢复 `#ffffff` 后 GREEN。既有五 surface test 继续 PASS。 |
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

## M1 Release 实机验收（2026-09-12，待重新执行）

此前记录的 fresh-profile 通过结论不再作为 Release 证据：04:56 的三张实机截图对应 PID 96389，命令为工作树相对路径 `packages/app-lite-gpui/target/release/velotype`；该文件修改时间为前一天 13:04，且 `strings` 不含当前 `library-main-editor-shell` 或 `library-editor-command-toolbar`。Cargo metadata 的实际 `target_directory` 是 `/Users/kevinhao/Projects/joplin/.shared-target`，并且修复后的分发脚本已经从该目录取 binary。

重新验收必须以新的绝对 profile 和以下二者之一启动：

- `/Users/kevinhao/Projects/joplin/.shared-target/release/velotype`；或
- 运行修复后的 `packages/app-lite-gpui/scripts/create_macos_app_dist.sh` 生成的 `.app`。

目视门槛：空态、无选择态和已选笔记三种状态中，`library-main-editor-shell`、actions、title、shared Chrome、正文周围都为连续浅色 primary surface；标题和正文文本可见，绿色 caret/selection/CTA 仍保留。随后才执行三篇中文笔记、PNG paste、JPEG Finder drop、PDF picker、图片前后输入、快速切换、退出/重开及 SQLite/hash 核验。完成前，本报告不声称 Step 5/M1 已闭合。

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

Step 5 的 fresh-profile Release 人工流程尚未因本次黑底回归重新完成；最终提交仍以重新执行的 core、GPUI、Release build、fmt、diff 和上节实机门槛全部通过为前提。

黑底修复后的额外定向验证：

```text
RUSTFLAGS='-Awarnings' cargo test --quiet --manifest-path packages/app-lite-gpui/Cargo.toml \
  ui::tests::mounted_default_editor_shell_paints_every_evernote_primary_surface -- --exact --nocapture
# PASS

RUSTFLAGS='-Awarnings' cargo test --quiet --manifest-path packages/app-lite-gpui/Cargo.toml \
  ui::tests::mounted_default_light_route_keeps_every_editor_state_opaque_and_contrasted -- --exact --nocapture
# PASS；空态、无选择、选中 note、shared Chrome、body surface 的实际样式调用均可读。
# 将生产 primary-surface token 临时改为 #000000 后此测试 RED；恢复 #ffffff 后 GREEN。

bash -n packages/app-lite-gpui/scripts/create_macos_app_dist.sh
cargo metadata --manifest-path packages/app-lite-gpui/Cargo.toml --no-deps --format-version 1
# PASS；脚本解析的 target directory 为 /Users/kevinhao/Projects/joplin/.shared-target，
# 不再从陈旧 packages/app-lite-gpui/target/release 复制 binary。

RUSTFLAGS='-Awarnings' cargo test --quiet --manifest-path packages/app-lite-gpui/Cargo.toml \
  ui::tests::mounted_card_click_reaches_the_same_shell_action_reducer -- --exact --nocapture
# PASS
```

本次修复后又以当前共享工作树复跑：`ui::tests` 53/53、`app::tests` 72/72、GPUI bin exact-donor-skip 1,170/0/1；`cargo check --tests`、两 crate 的 `cargo fmt -- --check`、`git diff --check` 与 `RUSTFLAGS='-Awarnings' cargo build --release --manifest-path packages/app-lite-gpui/Cargo.toml` 均 PASS。Release binary 已确认写入 Cargo metadata 的 `/Users/kevinhao/Projects/joplin/.shared-target/release/velotype`，并包含 `library-main-editor-shell` 和 `library-editor-command-toolbar`；这仍不替代下节规定的人工验收。

## 默认资料库共享格式 Chrome（2026-09-12）

默认 `LibraryShell` 现已在 `library-note-title` 与 `native-editor-surface` 间 mount 与 `--evernote-spike` 同一个 `native_editor::toolbar::EditorCommandChrome`。它持有 active `NoteSession` 的既有 `EditorCore`，并随 session/surface 在 selection 清空、切 note 或 codec failure 时一同释放；资料库顶层 actions 仍保持自己的职责。

本轮实际重新阅读的行为依据与完整交叉映射见 `task-5-format-chrome-report.md`：

- `/Users/kevinhao/Projects/joplin-reconstruction/evernote-11.32.5/common-editor-sourcemap/@evernote/common-editor/src/apps/peso/commands.ts` 的集中 command 模块；对应 Rust `EditorCommandChrome` + one `CommandCatalogue`，两 host 不复制 button/handler。
- 同解包路径的 `modules/textformatter/commands/boolformat.ts::execCommand/queryCommandValue` 的 single transaction、selection-derived active/mixed state 和 focus restore；对应 `EditorCommandChrome::execute_command` → `CommandCatalogue::execute` → `EditorCore` history。
- `docs/research/evernote-11.32.5-targeted-reverse.md::Toolbar command state and focus preservation` 的 toolbar/More 同 catalogue、popover preserve selection/return focus；对应 `render_for_host`、`render_more_menu`、`render_link_popover` 与 Library overlay placement。

新 mounted mutation-sensitive 结果均 PASS：`mounted_default_route_mounts_the_shared_editor_command_chrome`、`mounted_library_shared_chrome_clicks_bold_and_list_with_one_history_entry`、`mounted_library_chrome_format_manual_sync_switch_and_reopen_round_trips_canonical_html`、`mounted_narrow_library_chrome_moves_list_command_into_shared_more_and_executes_it`、`mounted_library_shared_link_popover_keeps_selection_and_returns_editor_focus`、`mounted_library_note_switch_discards_the_previous_shared_link_and_more_overlays`、`mounted_library_chrome_insert_image_event_uses_the_saved_selection_durable_picker_route`。最后一条断言 typed InsertImage event 先捕获 Library saved Selection，随后经已有 picker completion/staged durable import 发布 resource relation；没有调用 Spike 路径。

在该测试期间发现并修复一个共享 overlay 真回归：Library `EditorSurface` 自身有 capture-phase pointer handler，More/Link window overlays 没有 mouse occlusion 时，点击 More row 会先把正文 caret 移到末尾并使 command 无效。共享 `toolbar.rs` 的 menu/popover 现使用 GPUI `.occlude()`；这同样保护 Spike，而 Spike 的 44 项 toolbar/More/Link regression 仍全部通过。Library placement 也改为真实右侧编辑列宽，而非整窗宽度，以便 narrow layout 的同一 More catalogue 可靠计算。

本检查点随后运行完整 GPUI bin suite（仅跳过记录在案、未改动的 donor `editor::selection::tests::cross_block_cut_writes_markdown_deletes_range_and_undo_restores`）：**1,159 passed / 0 failed / 1 filtered**；`cargo check --tests`、两 crate 的 `cargo fmt -- --check` 和 `git diff --check` 均 PASS。

本节只记录自动化接线证据；Task 5 Brief Step 5 仍由上面的 fresh temporary-profile Release 实机流程闭合，当前状态为待重新验收，不能用绿色测试替代。
