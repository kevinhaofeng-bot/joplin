# Task 6 Stage A 独立审查

审查基线：`30944e99d` 至 2026-09-12 当前共享 dirty worktree。

结论：**CHANGES REQUESTED**

计数：**0 Critical / 2 Important / 0 Minor**。

本轮只审查 core route/query、`AppModel` typed navigation/history 和 `ui/mod.rs` 的排序标签；没有修改实现代码。报告以 `task-6-source-brief.md`、实际 donor reconstruction、dirty diff 和真实测试结果为依据，不采用 `task-6-stage-a-report.md` 自证。

## Source-first 抽查结论

- `/Users/kevinhao/Projects/joplin-reconstruction/evernote-11.32.5/main-readable/src/modules/51113__module-51113.js:90-118,122-167,184-266` 确认 nav item/action 是 typed，width、stack expansion、scroll location 是独立 reducer state；Rust `LibraryRoute`/`NavigationState` 没有以 label 或 row index 代替 ID。
- `/Users/kevinhao/Projects/joplin-reconstruction/evernote-11.32.5/main-readable/src/modules/74300__module-74300.js:396-420,460-486,722-727` 确认 list projection state、selected note GUID 和 list width 是不同状态；Rust selection/history snapshot 使用 `NoteId`，projection refresh正常路径不新增 history。
- `/Users/kevinhao/Projects/joplin-reconstruction/evernote-11.32.5/main-readable/src/modules/74643__module-74643.js:46-58` 与 `35172__module-35172.js:46-58` 分别确认 All Notes 为 Updated desc、Trash 为 Deleted desc，并拥有各自 sort reducer；Rust default 与 per-route map 的正常路径实现相符。
- `/Users/kevinhao/Projects/joplin-reconstruction/evernote-11.32.5/main-readable/src/modules/38218__module-38218.js:11-85` 确认 notebook/stack container、tag `GROUP BY ... HAVING count = selected_tag_count` AND 交集、trash/active 隔离、label no-case sort 和 SQL offset/limit；当前 Rust SQL逐项目视相符。
- `/Users/kevinhao/Projects/joplin-reconstruction/evernote-11.32.5/main-readable/src/modules/54193__module-54193.js:327-328,531-545,899-904,980-990` 确认 Back/Forward flags 与包含 typed view/note/notebook/stack IDs 的 `NAVIGATE_TO` 分离。GPUI 采用本地 cursor history 是合理独立机制。

## Important

### T6A-I1 — Navigation/sort 在 fallible query/hydration 前提交，失败后会留下互相矛盾的 route、history、projection 与 active session

位置：

- `/Users/kevinhao/Projects/joplin/.worktrees/joplin-lite-native-rust-mvp/packages/app-lite-gpui/src/app/mod.rs:150-177`：`SetSort` 先 `set_sort_for_route`，然后才调用可失败的 `refresh_list`。
- `/Users/kevinhao/Projects/joplin/.worktrees/joplin-lite-native-rust-mvp/packages/app-lite-gpui/src/app/mod.rs:467-490`：`NavigateTo` 先 push history/替换 current route，Back/Forward 先移动 cursor/应用 snapshot，之后才 query projection。
- `/Users/kevinhao/Projects/joplin/.worktrees/joplin-lite-native-rust-mvp/packages/app-lite-gpui/src/app/mod.rs:493-508`：query 成功后，目标 selection 还要经过可失败的 `load_note`/shell-state persistence；这些失败也不会回滚前面已经提交的 route/history/projection。
- `/Users/kevinhao/Projects/joplin/.worktrees/joplin-lite-native-rust-mvp/packages/app-lite-gpui/src/app/navigation.rs:44-68,152-167,170-172`：history truncate/cursor move、route/selection 和 route sort 均为立即原地 mutation，没有 prepare/commit 或 rollback boundary。

精确复现逻辑（已有 seam 足够）：

1. 在 All Notes 选择 note A，记录 route、projection IDs、active session 和 history。
2. 调用 `fail_next_refresh_for_test(LibraryError::NotFound)`，再 dispatch `NavigateTo { route: Trash, selected_note_id: None }`。
3. action 返回错误，但 `navigation.route()` 已为 Trash、selected 已为 None、history 已多一个 Trash entry；projection 仍是旧 All Notes rows，`active_session` 仍是 A。一个 model 同时声称“Trash/无选择”和“展示 active rows/持有 A editor”。
4. 对 Back/Forward 注入同一失败，cursor 和 route同样已经移动而 projection/session 没有移动；下一次 Back/Forward 会从错误 cursor 继续。对 `SetSort` 注入失败则 sort label/state 已变而 rows 保持旧顺序。
5. 即使 list query 成功，目标 `load_note` 或 selection persistence 失败仍会留下新 route/projection/selected ID 配旧 active note，存在错误 card identity 下继续编辑旧 note 的风险。

为何违反 Stage A：typed navigation 的核心价值是 route + selected `NoteId` + projection/session 作为一个一致 snapshot；错误可见并不能使已部分变更的纯 UI 状态变得诚实。正常路径测试没有覆盖这一点，尽管生产同文件已有可直接利用的 `next_refresh_failure` seam。

最小修复方向：在 clone/candidate `NavigationState` 上执行 Navigate/Back/Forward/SetSort，使用 candidate route/sort先查询到局部 projection，并在需要时先解析目标 selected note；所有 fallible query/hydration/persistence成功后再一次替换 model 的 navigation/projection/session。或者保存完整 pre-action state并在任何错误上严格回滚，但不要复用当前会逐步 mutation 的 `refresh_list`。新增四组 mutation-sensitive regressions：NavigateTo、Back、Forward、SetSort 各注入 refresh failure，断言 route/sort/history flags+length、projection IDs、selected ID、active session 全部保持 action 前状态；另覆盖目标 hydration failure。

### T6A-I2 — 新测试没有兑现报告声称的 notebook、offset、stable tie-break、全部 route defaults 与真实 Forward mutation coverage

位置与可存活 mutation：

- `/Users/kevinhao/Projects/joplin/.worktrees/joplin-lite-native-rust-mvp/packages/app-lite-core/tests/library_query_stage_a.rs:67-150` 创建了两个 notebooks，却只查询 Stack/Tags/Trash/All Notes page，从未执行 `LibraryRoute::Notebook`。删除 `/Users/kevinhao/Projects/joplin/.worktrees/joplin-lite-native-rust-mvp/packages/app-lite-core/src/query.rs:194-197` 的 `n.notebook_id = ?` predicate，当前 Stage A tests仍不会因 notebook结果集合错误而 RED。
- 同一测试 `library_query_stage_a.rs:137-141` 请求 offset 1/limit 1，但只断言 `len() == 1`，没有断言返回哪个 ID。删除 `OFFSET` 或恒定使用 offset 0 仍返回一行并通过；它只能证明 LIMIT，不证明 SQL pagination 的 offset语义或无重叠 page。
- `library_query_stage_a.rs:152-171` 声称删除 `n.id` tie-break 会 RED，但 fixture 的 ID 由递增 source按插入顺序产生。SQLite 对相同 NOCASE key 的未指定顺序在当前表上恰为插入 rowid 顺序，也正好是 ID 升序；删掉 `, n.id ASC` 可以继续得到相同结果。需要让相同 title key 的 NoteId 顺序与插入顺序相反，才能真正锁住 tie-break。
- `library_query_stage_a.rs:172-186` 只拒绝 `MAX_PAGE_SIZE + 1`，未覆盖契约下界 0；也未覆盖 `usize -> i64` offset overflow。将 `limit == 0` guard 删除，现有测试仍 GREEN。
- `library_query_stage_a.rs:189-215` 名称/报告称“each route”，实际只查询 All Notes 和 Trash。Notebook/Stack/Tags 的 Updated desc default 均未锁住；把其中任一路由 default 改为 Deleted/Title 不会触发该测试。
- `/Users/kevinhao/Projects/joplin/.worktrees/joplin-lite-native-rust-mvp/packages/app-lite-gpui/src/app/tests.rs:571-612` 只 Back 到 Notebook并检查 `can_navigate_forward`，随后直接新导航来测试 forward truncation；它从未 dispatch `NavigateForward`。删除/破坏 `NavigationState::navigate_forward` 的 `apply_history_snapshot`，当前新增 app test仍 GREEN。

为何阻断：`task-6-source-brief.md` 明确要求 SQL fixtures覆盖 notebook、stable tie-break 和 bounded pagination，并要求 Back/Forward恢复 typed route/filters/selected ID；父任务还特别要求判断 mutation tests是否真实会红。当前生产 SQL目视大体正确，但测试无法保护多个核心分支，`task-6-stage-a-report.md` 对 mutation sensitivity 的结论过度。

最小修复方向：

- 精确断言 Notebook route 只返回该 notebook 的 active notes；同时含 other notebook 与 trashed note。
- 对排序后的已知 IDs 请求 offset 1/limit 1并断言确切第二项，再取相邻 page 断言无重叠；分别拒绝 limit 0、501 和超出 SQLite signed range 的 offset。
- 使用 ID 顺序与插入顺序相反的 test ID source创建两个 NOCASE-equal titles，删除 SQL `n.id ASC` 时必须 RED。
- 对 All/Notebook/Stack/Tags 分别构造不同 updated_time，逐 route断言 Updated desc；Trash 单独断言 Deleted desc。
- 真正执行 Forward，断言 route、selected NoteId、projection IDs、active session 和 route sort恢复；再 Back 后新 Navigate 验证 branch truncate。

## 已通过且本轮未发现实现缺陷

- `LibraryRoute::tags` 使用 `BTreeSet<TagId>` canonicalize重复/顺序；SQL用 `HAVING count(DISTINCT nt.tag_id) = selected_count`，当前 two-tag AND fixture正确排除 only-one-tag note。
- Stack query、Trash isolation、`COLLATE NOCASE` 主排序、`n.id ASC` 明确 tie-break、SQL `LIMIT ? OFFSET ?` 和参数化 IDs 的生产实现均目视正确。
- projection SQL只读取 card metadata、资源关系和 MIME；独立 authorizer test未观察到 `notes.body_html`、`notes.body_text`、`notes.merge_state` 或 `resource_blobs.bytes`。
- `ListQuery::for_route` 保留 unbounded lightweight bootstrap，没有把 legacy Task 3 route截成100/500项；独立复跑 1,662-tail 与 101st-item mounted tests均通过。它仍应只作为过渡 compatibility path，Stage 6 viewport merge完成后迁到 `paged`。
- route sort正常成功路径为 per-route `BTreeMap<LibraryRoute, SortSpec>`，All Notes/Trash source-backed defaults正确；projection event refresh不会 push history。
- `ui/mod.rs` 只新增 `DeletedDescending` 中文标签，没有扩大 Stage A UI composition。

## 独立验证

```text
RUSTFLAGS='-Awarnings' cargo test --quiet --manifest-path packages/app-lite-core/Cargo.toml \
  --features test-support --test library_query_stage_a -- --nocapture
# PASS: 3/3

RUSTFLAGS='-Awarnings' cargo test --quiet --manifest-path packages/app-lite-core/Cargo.toml
# PASS: 85 tests across all targets

RUSTFLAGS='-Awarnings' cargo test --quiet --manifest-path packages/app-lite-core/Cargo.toml --features test-support
# PASS: 114 tests across all targets

RUSTFLAGS='-Awarnings' cargo test --quiet --manifest-path packages/app-lite-gpui/Cargo.toml app::tests:: -- --nocapture
# PASS: 67/67

cargo test --quiet --manifest-path packages/app-lite-gpui/Cargo.toml \
  uniform_list_constructs_only_requested_ranges_and_reaches_1662_tail -- --nocapture
# PASS: 1/1

cargo test --quiet --manifest-path packages/app-lite-gpui/Cargo.toml \
  uniform_list_does_not_truncate_the_101st_projection -- --nocapture
# PASS: 1/1

RUSTFLAGS='-Awarnings' cargo check --tests --manifest-path packages/app-lite-gpui/Cargo.toml
# PASS

cargo fmt --manifest-path packages/app-lite-core/Cargo.toml -- --check
cargo fmt --manifest-path packages/app-lite-gpui/Cargo.toml -- --check
git diff --check
# PASS
```

---

## Fix round 1 独立复审（2026-09-12，superseding verdict）

结论：**APPROVED**

计数：**0 Critical / 0 Important / 0 Minor**。

本节取代上面的首次 verdict。原 T6A-I1、T6A-I2 均已由生产结构和 mutation-sensitive tests关闭；本轮未发现新的 Stage A 阻断。

### T6A-I1 已关闭

- `/Users/kevinhao/Projects/joplin/.worktrees/joplin-lite-native-rust-mvp/packages/app-lite-gpui/src/app/mod.rs:479-559` 的 NavigateTo/Back/Forward 不再原地移动 live state，而是在 cloned `NavigationState` 上改变 route/history cursor，再由 `prepare_navigation_commit` 依次完成 candidate projection query、目标 Note hydration 和必要 selected-note shell-state persistence；只有这些 fallible步骤全部成功后，`commit_navigation` 才一次交换 navigation/projections/active session。
- `/Users/kevinhao/Projects/joplin/.worktrees/joplin-lite-native-rust-mvp/packages/app-lite-gpui/src/app/mod.rs:185-190` 的 SetSort 同样先修改 candidate并走同一 prepare/commit路径，查询失败不会留下新 sort 配旧 rows。
- `/Users/kevinhao/Projects/joplin/.worktrees/joplin-lite-native-rust-mvp/packages/app-lite-gpui/src/app/tests.rs:590-708` 的五个 failure tests分别覆盖 NavigateTo、Back、Forward、SetSort query failure及 target hydration failure。共享 `NavigationCommitProbe` 同时比较 typed snapshot、route sort、Back/Forward flags、history length、完整 projection ID顺序和完整 active `Note`；因此只回滚部分字段不能通过。
- hydration seam `/Users/kevinhao/Projects/joplin/.worktrees/joplin-lite-native-rust-mvp/packages/app-lite-core/src/repository.rs:332-339,553-559,686-696,883-896` 仅在 `cfg(test)`/`feature = "test-support"` 下存在；GPUI release dependency未启用该 feature，生产语义没有 test seam分叉。

### T6A-I2 已关闭

- `/Users/kevinhao/Projects/joplin/.worktrees/joplin-lite-native-rust-mvp/packages/app-lite-core/tests/library_query_stage_a.rs:217-315` 精确查询 Notebook route，fixture包含 other-notebook与trashed note；相邻 page断言具体 Alpha/Bravo IDs，删除 Notebook predicate、active predicate或 OFFSET都会 RED。limit 0、501 和 64-bit SQLite signed offset overflow均有具体 error断言。
- `/Users/kevinhao/Projects/joplin/.worktrees/joplin-lite-native-rust-mvp/packages/app-lite-core/tests/library_query_stage_a.rs:317-350` 使用“先插入较大 NoteId、后插入较小 NoteId”的 scripted source。NOCASE-equal titles期望按 ID而非 row/insertion order，删掉 `n.id ASC` 时会得到相反顺序并 RED。
- `/Users/kevinhao/Projects/joplin/.worktrees/joplin-lite-native-rust-mvp/packages/app-lite-core/tests/library_query_stage_a.rs:352-437` 实际执行 All Notes、Notebook、Stack、Tags、Trash 五种 route query，分别锁住 Updated desc/Deleted desc default，不再以测试名称替代 coverage。
- `/Users/kevinhao/Projects/joplin/.worktrees/joplin-lite-native-rust-mvp/packages/app-lite-gpui/src/app/tests.rs:711-812` 真实 dispatch Forward，并断言 Tag route snapshot、selected `NoteId`、唯一 target projection、active session和route sort；之后再次 Back并新建 Trash navigation，断言 Forward branch被截断。

### 独立验证

```text
RUSTFLAGS='-Awarnings' cargo test --quiet --manifest-path packages/app-lite-core/Cargo.toml \
  --features test-support --test library_query_stage_a -- --nocapture
# PASS: 5/5

RUSTFLAGS='-Awarnings' cargo test --quiet --manifest-path packages/app-lite-core/Cargo.toml
# PASS: 85 tests across all targets

RUSTFLAGS='-Awarnings' cargo test --quiet --manifest-path packages/app-lite-core/Cargo.toml --features test-support
# PASS: 116 tests across all targets

RUSTFLAGS='-Awarnings' cargo test --quiet --manifest-path packages/app-lite-gpui/Cargo.toml app::tests:: -- --nocapture
# PASS: 72/72

RUSTFLAGS='-Awarnings' cargo check --tests --manifest-path packages/app-lite-gpui/Cargo.toml
# PASS

cargo fmt --manifest-path packages/app-lite-core/Cargo.toml -- --check
cargo fmt --manifest-path packages/app-lite-gpui/Cargo.toml -- --check
git diff --check
# PASS
```
