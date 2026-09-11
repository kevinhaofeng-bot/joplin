# Task 6 Stage B1：typed 侧栏与三栏资料库 MVP

状态：B1 复审修复完成；Fix round 2 已经独立复审并获得
**APPROVED（0 Critical / 0 Important / 0 Minor）**。本报告记录提交前验证快照。

本阶段只把已批准的 Stage A `LibraryRoute` / SQLite projection /
`NavigationState` 挂到实际 GPUI 三栏界面。没有改 native editor、`NoteSession`
保存、资源导入/水合、动画、CRUD、批量操作、快捷方式、最近项目或搜索。

## Evernote 源码 → Rust → mutation-sensitive 验证

| 已实际重读的 Evernote 源码、符号与观察行为 | Rust 实现 | mutation-sensitive 验证 |
| --- | --- | --- |
| `/Users/kevinhao/Projects/joplin-reconstruction/evernote-11.32.5/main-readable/src/modules/51113__module-51113.js::NavBarTreeItemType`、`SELECT_ALL_NOTES`、`SELECT_TAGS`、`SELECT_TRASH`：All Notes、Notebook/Stack、Tag、Trash 是 typed reducer actions，不由显示文字推断；nav 宽度与展开 state 独立。 | `packages/app-lite-core/src/domain.rs::LibraryNavigationIndex` 只包含 `Notebook`/`Stack`/`Tag` durable IDs；`repository.rs::LibraryRepository::list_navigation_index` 在一个只读 SQLite transaction 中读取组织元数据；`packages/app-lite-gpui/src/ui/sidebar.rs::{entries,render}` 为每种 row 构造 typed `LibraryRoute`，`ui/mod.rs` 只发送 `AppAction::NavigateTo`。 | `library_query_stage_a::navigation_index_exposes_only_typed_sidebar_metadata_in_stable_tree_order` 校验 Stack、Notebook→Stack 归属与 TagId；`ui::tests::mounted_typed_sidebar_routes_use_durable_ids_without_hydrating_cards` 实际点击 All Notes、Notebook、Stack、Tag、Trash，逐项断言 route 和 exact `NoteId` projection，且有真实 thumbnail fixture 下 `observe_note_loads` 与 `observe_resource_reads` 均为空。删除 typed route 或改成 label/index即 RED。 |
| `/Users/kevinhao/Projects/joplin-reconstruction/evernote-11.32.5/main-readable/src/modules/74300__module-74300.js::Notes/SET_NOTE_LIST_STATE`、`Notes/SELECT_NOTE_SAGA`、`Notes/SET_NOTE_LIST_WIDTH`：note-list projection、selected note identity 与 pane 宽度相互分离；list refresh 不能用 row index 重建编辑器。 | `packages/app-lite-gpui/src/app/mod.rs::{AppModel::navigation_index,projections}` 将 sidebar metadata 与唯一 note-card projection 明确分离；`ui/mod.rs::LibraryShell::render_note_list` 仍是所有卡片的唯一 `UniformList`/`NoteProjection` 消费者；sidebar 不直接查询 cards 或创建 session。 | `mounted_list_modes_and_pane_collapse_keep_one_projection_and_live_session` 逐项锁住同一 ordered projection、selected `NoteId`、`NoteSession::entity_id` 和 `EditorSurface::entity_id`。任何 mode/collapse 新建第二 query/session 或按 index 选择都会 RED。 |
| `/Users/kevinhao/Projects/joplin-reconstruction/evernote-11.32.5/main-readable/src/modules/76905__note-list-view-option.js::NOTE_LIST_VIEW_OPTION`：Cards、Snippets、List、Top List 是显式 presentation mode，不能各自持有不同 note list。 | `packages/app-lite-gpui/src/ui/note_card.rs::render` 在既有 `ListViewMode::{Cards,Snippets,Compact}` 下只改变 card hierarchy/height；`ui/note_list.rs` 仍将 mode 映射到 uniform-list fixed height。`TOP_LIST` 未实现，仍在后续 layout 范围。 | `mounted_list_modes_and_pane_collapse_keep_one_projection_and_live_session` 用真实 `library-cycle-view` 点击 Cards→Snippets→Compact→Cards，断言同一 projection 顺序/selection/session，且 `observe_note_loads` 没有新 hydration。删除 production mode selector 或绕开 reducer即 RED。 |
| `/Users/kevinhao/Projects/joplin-reconstruction/evernote-11.32.5/main-readable/src/modules/63570__module-63570.js`：Notebook/Stack 是独立 typed entities，Notebook 的 stack association 不是 note card 上的临时字段。`.../35422__module-35422.js`：Tag selection 保存 stable tag IDs，multi-tag predicate 是独立状态。 | `LibraryNavigationIndex` 保留 `Notebook::stack_id`，`sidebar::entries` 以 Stack route 加其 child Notebook route 组树；Tag row 形成一个 `LibraryRoute::Tags({TagId})`。多 tag AND SQL/state保持 Stage A 的 `LibraryRoute::Tags(BTreeSet<TagId>)`，本 MVP 尚无多选 tag 控件。 | core navigation-index test与 mounted sidebar test分别锁住 notebook/stack/tag durable IDs。Stage A 的 `route_compilation_filters_stack_tag_intersection_trash_and_page_without_body_reads` 继续锁住多 tag AND，不把它复制成 UI 第二 authority。 |
| `/Users/kevinhao/Projects/joplin-reconstruction/evernote-11.32.5/renderer-readable/chunks/9093.js`, module `633704`, `src/components/Nav/styles.css`：30px nav rows、13px row text、selected/hover surface、collapsed width token 60px；typed tree render由 stable identities提供。 | `ui/sidebar.rs::render` 使用 30px/13px、明确 selected/hover fill、可持久的现有 `PaneState` 宽度/visibility；保持 Task 3 已验证 shell width 范围，未改成 donor 的 244/400 或加入 motion。 | mounted sidebar route test真实读取 `library-sidebar-selected-route`；`mounted_list_modes_and_pane_collapse_keep_one_projection_and_live_session` 从三栏→两栏→一栏时断言不重建 session/surface。不做 300ms motion 是有意后续 isolated work，而不是 UI 默认透明行为。 |
| `/Users/kevinhao/Projects/joplin-reconstruction/evernote-11.32.5/renderer-readable/chunks/9435.js`, modules `706930`, `911674`, `610348`：DetailList 宽度独立于 nav/editor，并用 virtualization 处理大列表；card/list render不是完整 body hydration。 | 既有 `ui/mod.rs::LibraryShell::render_note_list` 继续用 `uniform_list + UniformListScrollHandle`；Stage B 没有替换这个 Task 3 已验证路径。sidebar 只消费 metadata index，card仍只消费 `NoteProjection`。 | `ui::tests::uniform_list_constructs_only_requested_ranges_and_reaches_1662_tail` 现增加 observer 断言：首次挂载 1,662 cards 时不 load body；既有 tail scroll/selection 和 bounded construction 仍通过。 |
| `/Users/kevinhao/Projects/joplin-reconstruction/evernote-11.32.5/renderer-readable/chunks/9093.js`, module `633704`, `src/components/Nav/styles.css::navigationTree/menuItem`：nav tree 处于动态 scroll area，30px typed rows 不因固定窗口高度而截断。 | `packages/app-lite-gpui/src/ui/sidebar.rs::{render,render_entry}` 以同一 `SIDEBAR_ENTRY_HEIGHT=30` 交给 GPUI 0.2.2 `uniform_list`；`LibraryShell::sidebar_scroll` 是 retained `UniformListScrollHandle`，所以侧栏仍只通过 `AppAction::NavigateTo`，但可请求任意 offscreen durable route。 | `ui::tests::mounted_sidebar_virtualizes_scale_fixture_and_reaches_tail_typed_routes` 在 760px 窗口创建默认+30 个 notebook、64 tags；先断言尾 Tag/Trash 不在 viewport、首帧和单次 processor range 有界，随后真实 `scroll_to_item` 后点击尾 Tag 与 Trash，逐项断言 typed route/selected styling，且 body/blob observers 为空。删掉 shared scroll wiring、改回 eager clipped column 或把 route 变为 label/index 都会 RED。 |
| `/Users/kevinhao/Projects/joplin-reconstruction/evernote-11.32.5/main-readable/src/modules/38218__module-38218.js::noteListQueryBuilder`：route query 是有界轻量 projection；正文/资源 bytes 不是 sidebar/list navigation 输入。此处的 SQLite authorizer 是独立 Rust 测试架构，不宣称 donor 有相同 hook。 | `packages/app-lite-core/src/repository.rs::{LibraryRepository::list_navigation_index,observe_next_navigation_index_query}` 在 test/test-support 构建给同一 read transaction 安装真实 rusqlite `AuthAction::Read` gate；release 不带 observer field/lock。三条 SQL仍只读 active stack/notebook/tag metadata。 | `library_query_stage_a::navigation_index_authorizer_rejects_body_blob_reads_and_soft_deleted_organization_rows` 建有 stack/notebook/tag 各一个 soft-deleted fixture，观察一次被丢弃的真实 navigation-index 结果；允许列集以外的任意 read（特别是 `notes.body_html/body_text/merge_state` 或 `resource_blobs.bytes`）立即 RED，删除任意 `deleted_time=0` predicate 也会暴露已删除 route。 |

## 独立产品决定与明确未做项

- donor CSS 的 60px collapsed rail、300ms nav motion、400ms detail-list motion 与
  `TOP_LIST` 不在 B1 范围。当前折叠保留 Task 3 已验证的 persisted pane width/visibility，
  且只改变 presentation，不重建 editor/session；后续 motion 作为独立提交处理。
- Stage A 已拥有 canonical multi-tag AND route。B1 为每个 tag 渲染单 tag typed entry，
  不引入 multi-select chooser、tag filtering 或 tag CRUD。
- `LibraryNavigationIndex` 是 metadata-only input，不是第二 note query authority：所有
  card sort/list mode/selection仍由 `AppModel::projections + NavigationState` 决定。

## 复审修复与当前验证快照

- I1 根因是原 sidebar 仅 `h_full + overflow_hidden`，虽然拥有所有 typed entries，但没有一个 scroll owner；修复将完整 metadata tree 交给真实 `uniform_list`，不缩小 31-notebook/64-tag fixture。
- I2 根因是原 body/resource observers 只围绕 complete-note hydration 与 blob open，不能看见 `list_navigation_index` 的 SQLite direct reads；修复在 test/test-support 设真实 authorizer allow-list，release 不产生该测试开销。
- 第二轮并发隔离：原 `sidebar.rs` 的 `CONSTRUCTED_ITEMS` / `LARGEST_REQUESTED_RANGE` 是进程全局 `AtomicUsize`，并行 mounted shell 会把无关 draw 计入 scale fixture 的累计阈值，曾稳定复现 342。现在 `LibraryShell` 在 `cfg(test)` 下各自持有 `SidebarRenderProbe`，真实 `sidebar::render` 的 `uniform_list` processor 只向当前 host 记录 `request_count`、单次最大范围和最后请求范围；production 没有全局 mutable probe/Atomic。scale test 不再断言跨 draw 的累计总数，而是逐次确认本 shell 对 tail Tag 与 Trash 的实际 requested range 均包含目标且小于 80。临时删除 processor 的 `record_sidebar_uniform_list_range_for_test` 后，same exact target 在“mounted shell must receive a real uniform-list request”如预期 RED；恢复后 GREEN。
- `RUSTFLAGS='-Awarnings' cargo test --quiet --manifest-path packages/app-lite-core/Cargo.toml`：85 passed；`--features test-support`：118 passed。
- `RUSTFLAGS='-Awarnings' cargo test --quiet --manifest-path packages/app-lite-core/Cargo.toml --features test-support --test library_query_stage_a -- --nocapture`：7 passed。
- `RUSTFLAGS='-Awarnings' cargo test --quiet --manifest-path packages/app-lite-gpui/Cargo.toml app::tests -- --nocapture`：72 passed；`--bin velotype ui::tests`：52 passed。
- `RUSTFLAGS='-Awarnings' cargo test --quiet --manifest-path packages/app-lite-gpui/Cargo.toml --bin velotype ui::tests -- --test-threads=8`：52/52 PASS，连续三次；不再依赖串行 `--test-threads=1` 偶然规避串扰。
- 定向：`mounted_typed_sidebar_routes_use_durable_ids_without_hydrating_cards`、`mounted_sidebar_virtualizes_scale_fixture_and_reaches_tail_typed_routes`、`ui::sidebar::tests`、navigation-index SQL authorizer：各通过；后者对丢弃结果仍记录实际 SQLite `Read` columns。
- Authorizer mutation proof：本地临时加入并丢弃 `SELECT body_html FROM notes LIMIT 1` 后，`navigation_index_authorizer_rejects_body_blob_reads_and_soft_deleted_organization_rows` 如预期 RED，报告 `notes.body_html` 不在允许列集；移除该故意变异后相同 exact test 恢复 GREEN。
- `RUSTFLAGS='-Awarnings' cargo test --quiet --manifest-path packages/app-lite-gpui/Cargo.toml --bin velotype -- --skip editor::selection::tests::cross_block_cut_writes_markdown_deletes_range_and_undo_restores`：1,169 passed / 0 failed / 1 exact pre-existing donor skip。
- `cargo check --quiet --manifest-path packages/app-lite-gpui/Cargo.toml --tests`、两个 crate 的 `cargo fmt -- --check`、`git diff --check`：PASS（check 仅有既有 objc macro warning）。
- 本报告不会把 Task 6 或任何 Release step 标记完成；B1 已独立复审通过，但动画、CRUD/批量操作、shortcut/recent/search 与 Top List 仍明确不在本次范围。

## Release fresh-profile 实机补充（2026-09-12）

这不是 Evernote 行为复刻，而是本产品的发布交付/验收证据。此前黑色 editor
shell 截图运行的是陈旧 worktree-local `target/release/velotype`；正确的 Cargo
metadata target 产物已在 fresh profile 上重新验收。以下截图均来自该正确产物：

- `/tmp/joplin-lite-b1-correct-empty.png`：三栏空态，sidebar/list/editor 的右侧主面连续浅色，空态标题和 CTA 可见。
- `/tmp/joplin-lite-b1-correct-title2.png`、`/tmp/joplin-lite-b1-correct-body.png`：`Release smoke title` 和 `Release body text` 都以深色绘制在白色 title/chrome/body 上，绿色 caret/selection 仍保留。
- `/tmp/joplin-lite-b1-correct-two-pane.png`、`/tmp/joplin-lite-b1-correct-one-pane.png`、`/tmp/joplin-lite-b1-correct-three.png`：三栏、两栏、一栏之间切换不暴露黑色窗口 backing，格式栏与正文持续可读。
- `/tmp/joplin-lite-b1-correct-relaunch.png`：重开后 note card、标题和正文仍可见；同次 fresh-profile SQLite 检查确认上述标题/正文精确存在且 `PRAGMA integrity_check` 为 `ok`。

本轮 Release review 的路径回归也已关闭：
`packages/app-lite-gpui/scripts/create_macos_app_dist.sh` 的
`MACOS_RESOURCES_DIR="$PROJECT_ROOT/resources/macos"` 是唯一 bundle resource
authority，`Info.plist` 和 `velotype.icns` 不再依赖 caller cwd。实际从 worktree
root、`scripts/` 目录、以及带空格的 `CARGO_TARGET_DIR` 三种上下文运行脚本均成功；
每次均用 `cmp -s` 和 SHA-256 比较 bundle 的
`Contents/MacOS/velotype` 与该次 Cargo metadata/fresh release binary，结果完全相同，
且 `plutil -lint` 与 icon presence 都通过。该脚本修复不改变 B1 的 route/query/session
权威，也不将后续 Task 6 范围标为完成。
