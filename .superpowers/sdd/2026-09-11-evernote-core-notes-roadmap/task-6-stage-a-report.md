# Task 6 Stage A：路由、导航状态与轻量列表查询

状态：实现完成；Fix round 1 已经独立复审并获得
**APPROVED（0 Critical / 0 Important / 0 Minor）**。本报告记录的是提交前验证快照。

范围严格限制为 core/state/query 基础：typed route、每 route 的 sort、NoteId
selection/history 与 SQLite projection query。没有改 sidebar/list UI composition、pane
animation、thumbnail residency、搜索、shortcut 或组织 mutation UI；Task 1–5 已验证的
editor/session/resource 保存路径保持不动。

## Evernote 源码 → Rust → mutation-sensitive 验证

| Evernote 已实际重读的源文件、符号与观察行为 | Rust Stage A 实现 | mutation-sensitive 验证与结果 |
| --- | --- | --- |
| `/Users/kevinhao/Projects/joplin-reconstruction/evernote-11.32.5/main-readable/src/modules/51113__module-51113.js::NavBarTreeItemType` 与 `SELECT_ALL_NOTES` / `SELECT_TRASH` / `SELECT_SHORTCUT_SOURCE`：导航项和 route action 是 typed；`navBarWidth`、expanded stacks、scroll location 是互相独立的 reducer state。 | `packages/app-lite-core/src/query.rs::LibraryRoute` 用 `AllNotes`、`Notebook(NotebookId)`、`Stack(StackId)`、canonical `Tags(BTreeSet<TagId>)`、`Trash` 表达唯一 query route。`packages/app-lite-gpui/src/app/navigation.rs::NavigationState` 单独持有 route、selected `NoteId`、route sort 和 history；没有把显示名称或 row index 作为状态。 | `app::tests::typed_route_history_keeps_route_scoped_sorts_and_never_records_projection_refreshes`：Notebook 和 Tag route 都由 stable IDs 构建；Back 后同一 Notebook/NoteId 恢复。删除 typed `NavigateTo` snapshot 或改用 index 会使 route/selection assertions 失败。 |
| `/Users/kevinhao/Projects/joplin-reconstruction/evernote-11.32.5/main-readable/src/modules/74300__module-74300.js::initialState` / reducer `Notes/SET_NOTE_LIST_STATE`、`Notes/SELECT_NOTE_SAGA`、`Notes/SET_NOTE_LIST_WIDTH`：list projection state 与 selectedNoteGuid 不同；list state merge 不能通过 refresh 改变 selected identity。 | `NavigationState::select` 仅替换当前 snapshot 的 `Option<NoteId>`；`AppModel::prepare_navigation_commit` 先在 clone candidate 上查询 projection、hydrate target note、写必要 selection persistence，均成功后 `commit_navigation` 才一次交换 navigation/projection/active session；`refresh_projection_events` 只调用 route-derived projection query，绝不调用 `NavigationHistory::push`。 | `navigate_to_refresh_failure_keeps_the_entire_live_navigation_commit`、`back_refresh_failure_keeps_the_entire_live_navigation_commit`、`forward_refresh_failure_keeps_the_entire_live_navigation_commit`、`sort_refresh_failure_keeps_the_entire_live_navigation_commit` 与 `target_hydration_failure_keeps_the_entire_live_navigation_commit` 各注入真实 seam，断言 route/selection/history flags+length/projection IDs/完整 active note 都不变。删除 candidate/commit 边界会 RED。 |
| `/Users/kevinhao/Projects/joplin-reconstruction/evernote-11.32.5/main-readable/src/modules/76905__note-list-view-option.js::NOTE_LIST_VIEW_OPTION`：`CARDS`、`SNIPPETS`、`LIST`、`TOP_LIST` 是显式 presentation modes，不是不同 note query authority。 | Stage A 保留已有 `ListViewMode::{Cards,Snippets,Compact}` 和单一 `ListQuery`/`NoteProjection`。不实现 `TOP_LIST` 或任何 UI layout；之后 mode mount 仍只能消费这一个 projection path。 | 既有 `app::tests::projection_actions_never_hydrate_a_body_and_an_explicit_selection_loads_once` 与新 query observer test共同保证 presentation/sort 不读取正文。 |
| `/Users/kevinhao/Projects/joplin-reconstruction/evernote-11.32.5/main-readable/src/modules/74643__module-74643.js::initialState`：All Notes 默认 `Updated` descending；`.../35172__module-35172.js::initialState`：Trash 默认 `Deleted` descending，各自有 `SET_SORT` reducer。 | `query.rs::{SortField,SortDirection,SortSpec}`；`LibraryRoute::default_sort` 明确分配 All/Notebook/Stack/Tags 的 Updated desc 与 Trash 的 Deleted desc。`NavigationState::route_sorts: BTreeMap<LibraryRoute, SortSpec>` 避免全局 sort 泄漏。 | `library_query_stage_a::each_route_starts_with_its_source_backed_updated_or_deleted_sort` 用注入递增时钟分别执行 All、Notebook、Stack、Tag AND 与 Trash query，断言每路由的实际默认 SQL 顺序；app history test设 Notebook title sort、进 Trash、Back 后断言 Notebook title sort 仍在。全局 sort 或错误 default 会 RED。 |
| `/Users/kevinhao/Projects/joplin-reconstruction/evernote-11.32.5/main-readable/src/modules/38218__module-38218.js::noteListQueryBuilder`：projection 只 select id/label/snippet/timestamps/container；Notebook 匹配 parent，Stack 用 notebooks subquery，tag 多选用 `GROUP BY Note_id HAVING count = selected_tag_count` 的 AND 交集；Trash/normal 的 deleted predicate 相斥；label `noCaseColumn` sort；`offset/limit` 在 SQL。 | `app-lite-core/src/query.rs::compile_note_list_query` 编译唯一 projection SQL；`LibraryRepository::list_notes` 只执行该编译结果。`ListQuery::paged` 强制 `1..=500` page，包含 offset/limit；可预留的既有 `for_route` bootstrap 用于 Task 3 当前 1,662 项 virtual-list compatibility，不是新 viewport API。 | `route_compilation_filters_stack_tag_intersection_trash_and_page_without_body_reads` 覆盖 Stack、两 tag AND、Trash isolation 与 authorizer 无正文/blob读取；`notebook_route_offset_and_page_bounds_are_exact` 断言真实 Notebook predicate、offset 精确第二卡、相邻页无重叠、0/501/SQLite signed overflow 拒绝；`title_tie_break_uses_id_not_insert_row_order` 用反插入顺序 ID fixture锁住 `n.id ASC`。把 HAVING 改 OR、删 notebook/deleted predicate、删 OFFSET 或 tie-break、读 body 都会 RED。 |
| `/Users/kevinhao/Projects/joplin-reconstruction/evernote-11.32.5/main-readable/src/modules/54193__module-54193.js::initialState`、`App/SET_CAN_NAVIGATE_BACK`、`App/SET_CAN_NAVIGATE_FORWARD`、`navigateTo`：back/forward flags 与 typed `view` / note / notebook / stack identifiers 分离。 | `app/navigation.rs::{NavigationSnapshot,NavigationHistory}` 实现纯 Rust cursor history；`AppAction::{NavigateTo,NavigateBack,NavigateForward}` 是唯一应用 reducer 入口，snapshots 包含 `LibraryRoute + Option<NoteId>`。新导航先截断 forward，projection refresh 不可写入 history。 | `typed_route_history_keeps_route_scoped_sorts_and_never_records_projection_refreshes` 现在真实 dispatch Forward，并断言 Tag route、selected `NoteId`、target projection、active session 和 route sort 都恢复；再 Back 后新 Navigate 截断 Forward。删除 `navigate_forward` 的 target apply 或 truncate 会 RED。 |

## Stage A 独立产品决定

- Evernote desktop 的 Back/Forward 最终委托活 Electron `webContents` history；GPUI 没有可安全复用的浏览器 history。因此 `NavigationHistory` 是本产品的 native Rust 机制，复制的是 typed snapshot/Back-Forward UX contract，不宣称重写 Electron internals。
- `MAX_PAGE_SIZE = 500` 是本产品的 bounded list-page contract。当前 Task 3 virtual list 仍通过 legacy `ListQuery::for_route` 获取其完整 lightweight projection，避免在尚未实现 incremental page merge 的 Stage A 改坏 1,662-tail route；Task 6 UI 阶段必须迁到 `ListQuery::paged`，不能将 unbounded bootstrap 当成 viewport API。
- Title ordering使用 SQLite `COLLATE NOCASE` 和 stable `n.id ASC`。这是与 donor `noCaseColumn` 语义对齐的本地 SQLite implementation，不声称其底层 collation 实现完全相同。

## 实际验证

```text
RUSTFLAGS='-Awarnings' cargo test --quiet --manifest-path packages/app-lite-core/Cargo.toml --features test-support --test library_query_stage_a -- --nocapture
# PASS: 5 tests

RUSTFLAGS='-Awarnings' cargo test --quiet --manifest-path packages/app-lite-core/Cargo.toml
# PASS: 85 tests

RUSTFLAGS='-Awarnings' cargo test --quiet --manifest-path packages/app-lite-core/Cargo.toml --features test-support
# PASS: 116 tests

RUSTFLAGS='-Awarnings' cargo test --quiet --manifest-path packages/app-lite-gpui/Cargo.toml app::tests:: -- --nocapture
# PASS: 72 tests（含 candidate/prepare/commit 5 条失败原子性测试与真实 Forward 恢复）

RUSTFLAGS='-Awarnings' cargo check --tests --manifest-path packages/app-lite-gpui/Cargo.toml
# PASS
cargo fmt --manifest-path packages/app-lite-core/Cargo.toml -- --check
# PASS
cargo fmt --manifest-path packages/app-lite-gpui/Cargo.toml -- --check
# PASS
git diff --check
# PASS
```

## 明确未做

- Sidebar/list route rendering、Cards/Snippets/Compact per-route persistence、pane resize/motion、scroll restoration、shortcut/recent UI、notebook/tag CRUD UI、bulk commands、search、Task 7+。
- 没有将 route/sort/history 写入 `LibraryShellState`：持久化 schema/UI restore 属 Task 6 后续 organization/pane stage；本包只提供 stable typed state/query foundation。

## 独立复审 Fix round 1

- **T6A-I1 已关闭：** `NavigateTo`、Back、Forward 和 `SetSort` 均不再在 live model 上预先改变 route/history/sort。候选 query、目标 `load_note` 和 selection shell-state 写入全部在 `PreparedNavigationCommit` 前完成；任何错误只更新可见 action error，不改变原 live navigation/projection/editor snapshot。`fail_next_note_load_for_test` 只在 core 的 `test` / `test-support` 编译条件下导出，release 不含此 seam。
- **T6A-I2 已关闭：** query fixtures现在精确锁住 Notebook、offset/page、0/501/signed overflow、反插入顺序的 NOCASE tie-break，及 All/Notebook/Stack/Tags/Trash 五种默认排序；history fixture现在执行真正 Forward 再验证 branch truncate。
