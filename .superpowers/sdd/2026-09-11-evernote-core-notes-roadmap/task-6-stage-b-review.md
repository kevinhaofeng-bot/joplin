# Task 6 Stage B1 独立审查

审查范围：Task 6 总基线 `30944e99d`；实际 Stage B1 dirty diff 位于当前
`HEAD 1c90069b5` 之上（`30944e99d..HEAD` 已包含已批准的 Stage A commits）。

结论：**CHANGES REQUESTED**

计数：**0 Critical / 2 Important / 0 Minor**。

本轮只修改本审查报告，没有修改产品实现、commit、tag 或 push。结论来自
`task-6-source-brief.md`、实际 Evernote reconstruction、当前 dirty diff、实际源码和
独立测试，不采用 `task-6-stage-b-report.md` 自证。

## Source-first 核对

- `/Users/kevinhao/Projects/joplin-reconstruction/evernote-11.32.5/main-readable/src/modules/51113__module-51113.js:39-48,90-118,122-167,184-266`
  确认 All Notes、Notebook/Stack、Tag、Trash 都是 typed tree item/action；nav width、
  expanded stacks 和 scroll location 是独立状态。当前 Rust route row 确实携带 durable
  `NotebookId`/`StackId`/`TagId`，不从中文 label 推断 route。
- `/Users/kevinhao/Projects/joplin-reconstruction/evernote-11.32.5/main-readable/src/modules/74300__module-74300.js:396-420,460-486,722-727`
  确认 note-list projection、selected note GUID 和 pane width 分离。当前三个 list mode
  都消费 `AppModel::projections`，selection 仍是 `NoteId`。
- `/Users/kevinhao/Projects/joplin-reconstruction/evernote-11.32.5/main-readable/src/modules/76905__note-list-view-option.js:4-15`
  明确列出 `CARDS`、`SNIPPETS`、`LIST`、`TOP_LIST`。B1 的 Cards/Snippets/Compact 是
  同一 projection path 的 presentation，不存在第二份 handler/query。
- `/Users/kevinhao/Projects/joplin-reconstruction/evernote-11.32.5/main-readable/src/modules/63570__module-63570.js:101-139,147-226`
  与 `35422__module-35422.js:105-132,137-260` 分别确认 Notebook/Stack 和 Tag selection
  是 stable typed identities；tag 普通 expansion 与 filtered expansion 分离。B1 当前只做
  flat single-tag route，multi-tag chooser/expansion/CRUD 明确仍不在本阶段。
- `/Users/kevinhao/Projects/joplin-reconstruction/evernote-11.32.5/renderer-readable/chunks/9093.js:22-170`
  的 module `633704` 保留 30px row、13px text、selected/hover、300ms transition，并明确
  nav 使用动态 scroll area。`renderer-readable/chunks/9435.js:404-505,37265-37460,
  40350-40372` 的 modules `706930`、`911674`、`610348` 确认 DetailList 独立宽度、
  hidden state 和 virtualized/card geometry；`9435.js:2574-2790` 的 card styles确认不同
  presentation 共享相同 selected/hover identity。

## Important

### T6B-I1 — 侧栏没有滚动路径，31 notebooks / 64 tags 规模下多数 typed routes 不可达

位置：

- `/Users/kevinhao/Projects/joplin/.worktrees/joplin-lite-native-rust-mvp/packages/app-lite-gpui/src/ui/sidebar.rs:109-123`
  将整个 sidebar 固定为 `h_full()` 并设为 `overflow_hidden()`，没有
  `overflow_y_scroll()`、scroll handle 或 virtual list。
- 同文件 `:124-184` 将全部 Stack、Notebook、Tag、Trash rows 直接追加到这个被裁剪的
  容器中；每个 route row 固定 30px，heading 还额外占高。
- `/Users/kevinhao/Projects/joplin/.worktrees/joplin-lite-native-rust-mvp/packages/app-lite-gpui/src/ui/tests.rs:178-317`
  的 mounted fixture 只有 1 stack、1 notebook、1 tag，全部 rows 可在测试窗口内出现，
  因而无法发现裁剪后的尾部 route 不可点击。

精确复现逻辑：按 roadmap 的真实规模建立 31 个 notebooks 和 64 个 tags，再以产品默认
760px 高窗口挂载。仅 95 个 metadata route rows 已需至少 2,850px，尚未计 headings、
stacks、padding 和 Trash。容器在约 760px 后裁剪且没有任何滚动输入；尾部 tags 和
Trash 的 bounds 位于 visible hit-test viewport 外，用户无法导航到这些 typed routes。

为何违反 B1：B1 的目的不是只在内存中构造 typed rows，而是让 sidebar 的“所有 route”
真实可用；roadmap Step 4 和 source brief 还明确给出 31-notebook/64-tag fixture。不可达的
route 是资料库导航功能缺失，不是视觉 polish。Evernote donor 的 nav 则将树放在可滚动
区域中。

最小修复方向：保留 metadata-only `entries` 和唯一 `AppAction::NavigateTo` bridge，在
固定 sidebar shell 内增加真实纵向 scroll container（数据规模继续扩大时可使用 lazy/
uniform tree）。增加 mutation-sensitive mounted scale test：创建至少 31 notebooks、
64 tags，确认最末 TagId 和 Trash 初始不在 viewport；真实 scroll 到尾部后点击二者，
断言 exact typed route/projection/selected styling，同时 body-load/resource-read observers
保持空。删除 scroll wiring 时该测试必须 RED。

### T6B-I2 — “metadata-only / active-only”验证不拦原始 SQLite body/blob 读取，也未覆盖 deleted organization rows

位置：

- 生产 `/Users/kevinhao/Projects/joplin/.worktrees/joplin-lite-native-rust-mvp/packages/app-lite-core/src/repository.rs:818-858`
  当前目视正确：三条 SQL 只读取 stack/notebook/tag metadata，并都过滤
  `deleted_time = 0`。
- `/Users/kevinhao/Projects/joplin/.worktrees/joplin-lite-native-rust-mvp/packages/app-lite-core/tests/library_query_stage_a.rs:439-488`
  只比较返回的 organization IDs/order/stack ownership；没有安装 SQLite authorizer/read
  observer，也没有创建 soft-deleted stack/notebook/tag。
- `/Users/kevinhao/Projects/joplin/.worktrees/joplin-lite-native-rust-mvp/packages/app-lite-gpui/src/ui/tests.rs:222-223,288-316`
  的 `observe_note_loads` 只观测 `LibraryRepository::load_note`，`observe_resource_reads`
  只观测 ResourceStore blob 打开；它们不会捕获 `list_navigation_index` 内部直接执行的
  `SELECT notes.body_html/body_text/merge_state` 或 `SELECT resource_blobs.bytes`。

可存活 mutation：在 `list_navigation_index` 返回同样 metadata 前额外 query 任一 note
body/blob column并丢弃结果，所有新增 B1 tests仍会 GREEN；删除 stacks/notebooks/tags
的 `deleted_time = 0` predicate也不会被当前 fixture触发。测试注释声称这些 mutation 会
RED，与实际覆盖不符。

为何违反 B1：metadata-only 是本阶段最重要的低内存/隐私读取边界，且父审查要求测试
必须 mutation-sensitive。生产当前正确不足以替代一个能阻止后续 join/prefetch 回归的
真实列访问 gate。

最小修复方向：为 `list_navigation_index` 增加与 `observe_next_list_query` 同等级的
SQLite authorizer seam，断言实际 `Read` columns 仅限 stacks/notebooks/tags 的允许集合，
并明确拒绝 `notes.body_html`、`notes.body_text`、`notes.merge_state`、
`resource_blobs.bytes`；fixture 再软删除各一种 organization entity并断言均不返回。
用临时 mutation（额外 body read、删除任一 active predicate）证明测试先 RED 再恢复。

## 已通过且未发现实现缺陷

- `LibraryNavigationIndex` 与 `NoteProjection` 类型分离；当前三条 production SQL没有读取
  notes/body/resource blobs，并在同一 SQLite snapshot 中返回 stable typed metadata。
- sidebar 的实际 mouse path 只构造 `AppAction::NavigateTo { route, selected_note_id: None }`
  并走 `LibraryShell::apply_action`；Notebook/Stack/Tag/Trash 的正常 mounted click均恢复
  exact route 和 exact projection。
- Cards/Snippets/Compact 只改变 fixed row height/child hierarchy；同一 ordered projections、
  selected `NoteId` 和 single card click bridge保持不变。
- `LibraryShell::sync_editor_surface` 以 selected note ID fencing；mode change 和三栏→两栏→
  一栏 toggle 后 `NoteSession::entity_id`、`EditorSurface::entity_id` 保持不变。新增 mounted
  identity test对此是 mutation-sensitive 的。
- 既有 GPUI `uniform_list` 仍能挂载/选择并 scroll 到第 1,662 项，初次 construction小于
  128 rows，且未触发 complete-note hydration；B1 没有回退 Task 1–5 的 editor/session/
  durable-resource生产路径。

## 独立验证

```text
RUSTFLAGS='-Awarnings' cargo test --quiet --manifest-path packages/app-lite-core/Cargo.toml
# PASS: 85 tests across all targets

RUSTFLAGS='-Awarnings' cargo test --quiet --manifest-path packages/app-lite-core/Cargo.toml \
  --features test-support
# PASS: 117 tests across all targets

cargo test --quiet --manifest-path packages/app-lite-core/Cargo.toml \
  --features test-support --test library_query_stage_a \
  navigation_index_exposes_only_typed_sidebar_metadata_in_stable_tree_order -- --nocapture
# PASS: 1/1

cargo test --quiet --manifest-path packages/app-lite-gpui/Cargo.toml --bin velotype \
  mounted_typed_sidebar_routes_use_durable_ids_without_hydrating_cards -- --nocapture
cargo test --quiet --manifest-path packages/app-lite-gpui/Cargo.toml --bin velotype \
  mounted_list_modes_and_pane_collapse_keep_one_projection_and_live_session -- --nocapture
cargo test --quiet --manifest-path packages/app-lite-gpui/Cargo.toml --bin velotype \
  uniform_list_constructs_only_requested_ranges_and_reaches_1662_tail -- --nocapture
# PASS: 1/1 each

RUSTFLAGS='-Awarnings' cargo test --quiet --manifest-path packages/app-lite-gpui/Cargo.toml \
  app::tests -- --nocapture
# PASS: 72/72

RUSTFLAGS='-Awarnings' cargo test --quiet --manifest-path packages/app-lite-gpui/Cargo.toml \
  --bin velotype ui::tests:: -- --nocapture
# PASS: 51/51

RUSTFLAGS='-Awarnings' cargo test --quiet --manifest-path packages/app-lite-gpui/Cargo.toml \
  --bin velotype -- \
  --skip editor::selection::tests::cross_block_cut_writes_markdown_deletes_range_and_undo_restores
# PASS: 1,168 passed / 0 failed / 1 exact pre-existing donor skip

RUSTFLAGS='-Awarnings' cargo check --quiet \
  --manifest-path packages/app-lite-gpui/Cargo.toml --tests
cargo fmt --manifest-path packages/app-lite-core/Cargo.toml -- --check
cargo fmt --manifest-path packages/app-lite-gpui/Cargo.toml -- --check
git diff --check
# PASS
```

---

## Fix round 1 独立复审（2026-09-12，superseding verdict）

结论：**CHANGES REQUESTED**

计数：**0 Critical / 1 Important / 0 Minor**。

本节取代首次 verdict。原 T6B-I1、T6B-I2 的生产缺陷已经关闭；但新 scale
verification 使用未隔离的进程全局计数器，在正常并行 test runner 下稳定串扰并使 UI
gate 非确定失败，因此本轮仍不能 APPROVE。复审期间只更新本报告，没有修改产品代码、
commit、tag 或 push。

### 原 T6B-I1 已关闭：真实 retained uniform-list scroll path可到达尾部 typed routes

- `/Users/kevinhao/Projects/joplin/.worktrees/joplin-lite-native-rust-mvp/packages/app-lite-gpui/src/ui/mod.rs:173-176,385-407,2100-2119`
  将 `UniformListScrollHandle` 保存在 `LibraryShell` 实体上，constructor只创建一次；每次
  render均把 clone交给同一个 sidebar，不会因 model redraw丢掉 offscreen scroll请求。
- `/Users/kevinhao/Projects/joplin/.worktrees/joplin-lite-native-rust-mvp/packages/app-lite-gpui/src/ui/sidebar.rs:102-155`
  将所有 30px fixed-height entries交给生产 GPUI 0.2.2 `uniform_list`，并真实调用
  `.track_scroll(scroll_handle)`；processor只构造 GPUI请求的 range，而不是预先构造完整树
  再裁剪。
- `/Users/kevinhao/Projects/joplin/.worktrees/joplin-lite-native-rust-mvp/packages/app-lite-gpui/src/ui/tests.rs:319-446`
  的 scale fixture确实为默认+30 notebooks与64 tags，resize到760px；首帧断言 tail Tag/
  Trash没有 mounted bounds，然后通过 retained production handle分别 scroll到两项，读取
  mounted bounds并真实 `simulate_click`，最终断言 exact `LibraryRoute::Tags(TagId)` 与
  `LibraryRoute::Trash`。删除 `uniform_list`/`track_scroll`/shared handle时这条行为链会 RED。
- 独立定向运行该 test为 1/1 PASS；`largest_requested_range_for_test() < 80` 也证明单次
  production processor request有界。生产 route click仍统一走 `AppAction::NavigateTo`，
  body-load/resource-read observers为空。

### 原 T6B-I2 已关闭：真实 SQLite Read actions、soft delete和release cfg均正确

- `/Users/kevinhao/Projects/joplin/.worktrees/joplin-lite-native-rust-mvp/packages/app-lite-core/src/repository.rs:326-346,550-568,666-680,838-922`
  的 observer field、registration和 authorizer bookkeeping全部受
  `cfg(any(test, feature = "test-support"))` 控制。普通 `cargo tree -e normal -p velotype`
  只显示无 feature 的 `app-lite-core`，release `cargo check`通过；生产 binary不带该 seam。
- observer安装在 `list_navigation_index` 持有的真实 repository SQLite connection上，
  收集 rusqlite `AuthAction::Read { table_name, column_name }`，closure无论成功失败后都移除
  authorizer。三条生产 SQL只读取 active stacks/notebooks/tags metadata。
- `/Users/kevinhao/Projects/joplin/.worktrees/joplin-lite-native-rust-mvp/packages/app-lite-core/tests/library_query_stage_a.rs:492-602`
  创建 active与soft-deleted stack/notebook/tag并精确断言过滤；随后故意丢弃 navigation
  result，仍用真实 authorizer读集与 allow-list比较。
- 独立在临时 core copy中加入 `SELECT body_html FROM notes LIMIT 1`，测试以
  `navigation index read a non-metadata column: ... notes.body_html` RED；恢复后仅删除 stack
  `deleted_time = 0`，测试以 `soft-deleted stacks cannot become sidebar routes` RED。
  两个 mutation均证明测试不是只观察返回对象或 mock query。共享产品源码未作改动。

## Important

### T6B-R1-I1 — Scale test 的进程全局 Atomic累计量与其他并行 mounted tests串扰，正常 UI gate会随机失败

位置：

- `/Users/kevinhao/Projects/joplin/.worktrees/joplin-lite-native-rust-mvp/packages/app-lite-gpui/src/ui/sidebar.rs:232-265`
  用静态全局 `CONSTRUCTED_ITEMS` / `LARGEST_REQUESTED_RANGE` 记录所有 sidebar render，
  没有 test token、shell identity或 thread隔离。
- `/Users/kevinhao/Projects/joplin/.worktrees/joplin-lite-native-rust-mvp/packages/app-lite-gpui/src/ui/tests.rs:369-382,428-442`
  scale test在自己的 shell挂载后 reset全局计数，随后把整个进程中其他并行 test的 sidebar
  processor工作也计入 `< 320` cumulative assertion。

独立复现：

```text
RUSTFLAGS='-Awarnings' cargo test --quiet \
  --manifest-path packages/app-lite-gpui/Cargo.toml --bin velotype ui::tests:: -- --nocapture
# run A: 51 passed / 1 failed
# src/ui/tests.rs:438: two tail requests constructed 342 cumulative rows instead of staying bounded

# 再按同一命令循环复跑，第一次即再次得到相同 342 cumulative failure。

RUSTFLAGS='-Awarnings' cargo test --quiet \
  --manifest-path packages/app-lite-gpui/Cargo.toml --bin velotype ui::tests:: -- \
  --test-threads=1
# PASS: 52/52
```

这不是 production virtualization 超限：同一 scale test standalone 1/1、串行 UI 52/52，
且 single largest range保持有界；失败量来自同时运行的其他 mounted shells。但是正常
Cargo test默认并行执行，当前新增 gate本身不可靠，既会阻断无回归提交，也可能因调度
顺序偶然 GREEN（本轮 exact donor-skip全量即为1169/0/1）。报告不能用一次幸运 GREEN
替代可重复的 suite isolation。

最小修复方向：把 construction probe变为 test-owned/per-shell state，例如向 sidebar
processor注入带唯一 token的 `Arc<SidebarConstructionProbe>`，scale test只读取自己 shell的
range/cumulative counts；或只断言真实可见/mounted range与 per-render bounded request，
不要使用跨测试全局累计值。修复后至少并行重复运行 UI suite多次，并保留 standalone、
serial与 exact donor-skip证据。

## Fix round 1 独立验证

```text
RUSTFLAGS='-Awarnings' cargo test --quiet --manifest-path packages/app-lite-core/Cargo.toml
# PASS: 85 tests across all targets

RUSTFLAGS='-Awarnings' cargo test --quiet --manifest-path packages/app-lite-core/Cargo.toml \
  --features test-support
# PASS: 118 tests across all targets

RUSTFLAGS='-Awarnings' cargo test --quiet --manifest-path packages/app-lite-core/Cargo.toml \
  --features test-support --test library_query_stage_a -- --nocapture
# PASS: 7/7 after fresh rebuild

RUSTFLAGS='-Awarnings' cargo test --quiet --manifest-path packages/app-lite-gpui/Cargo.toml \
  app::tests -- --nocapture
# PASS: 72/72

RUSTFLAGS='-Awarnings' cargo test --quiet --manifest-path packages/app-lite-gpui/Cargo.toml \
  --bin velotype mounted_sidebar_virtualizes_scale_fixture_and_reaches_tail_typed_routes \
  -- --nocapture
# PASS: 1/1 standalone

RUSTFLAGS='-Awarnings' cargo test --quiet --manifest-path packages/app-lite-gpui/Cargo.toml \
  --bin velotype ui::tests:: -- --nocapture
# FAIL: 51/52; global cumulative sidebar count = 342 (reproduced twice)

RUSTFLAGS='-Awarnings' cargo test --quiet --manifest-path packages/app-lite-gpui/Cargo.toml \
  --bin velotype ui::tests:: -- --test-threads=1
# PASS: 52/52

RUSTFLAGS='-Awarnings' cargo test --quiet --manifest-path packages/app-lite-gpui/Cargo.toml \
  --bin velotype -- \
  --skip editor::selection::tests::cross_block_cut_writes_markdown_deletes_range_and_undo_restores
# PASS: 1,169 passed / 0 failed / 1 exact pre-existing donor skip

RUSTFLAGS='-Awarnings' cargo check --quiet \
  --manifest-path packages/app-lite-gpui/Cargo.toml --tests
RUSTFLAGS='-Awarnings' cargo check --quiet --release \
  --manifest-path packages/app-lite-gpui/Cargo.toml
cargo fmt --manifest-path packages/app-lite-core/Cargo.toml -- --check
cargo fmt --manifest-path packages/app-lite-gpui/Cargo.toml -- --check
git diff --check
# PASS
```

---

## Fix round 2 最终独立复审（2026-09-12，superseding verdict）

结论：**APPROVED**

计数：**0 Critical / 0 Important / 0 Minor**。

本节取代此前全部 verdict。Fix round 1 唯一剩余的全局 Atomic test-isolation
Important 已关闭；本轮未发现新的 Task 6 Stage B1 阻断。复审期间只更新本报告，未修改
产品代码、未 commit、未打 tag、未 push。

### T6B-R1-I1 已关闭：sidebar range probe 已按 mounted shell 隔离

- `/Users/kevinhao/Projects/joplin/.worktrees/joplin-lite-native-rust-mvp/packages/app-lite-gpui/src/ui/mod.rs:173-191,207-224,437-464`
  将 retained `UniformListScrollHandle` 与 `SidebarRenderProbe` 都放在单个
  `LibraryShell` 实体上。probe 类型、字段、初始化、record/accessor 全受 `#[cfg(test)]`
  控制；它没有 `static`、`Atomic`、共享 `Arc` 或 process-global reset，因此两个并行窗口
  不会互相累计 request。
- `/Users/kevinhao/Projects/joplin/.worktrees/joplin-lite-native-rust-mvp/packages/app-lite-gpui/src/ui/sidebar.rs:107-158`
  的生产路径仍是 GPUI 0.2.2 `uniform_list` 加 retained `.track_scroll(scroll_handle)`；
  processor 仅在 test build 调用当前 host shell 的
  `record_sidebar_uniform_list_range_for_test`。普通/release build 的 `shell` 只是显式 no-op，
  没有隐藏的 mutable probe，也没有旧 `CONSTRUCTED_ITEMS` /
  `LARGEST_REQUESTED_RANGE` sidebar globals。
- `/Users/kevinhao/Projects/joplin/.worktrees/joplin-lite-native-rust-mvp/packages/app-lite-gpui/src/ui/tests.rs:319-468`
  的 760px fixture 仍创建默认+30 notebooks及64 tags。首帧确认尾 Tag/Trash未挂载且单次
  requested range `< 80`；随后分别用同一个 retained handle scroll到尾 Tag和Trash，断言
  `last_requested_range` 包含目标 index、request count在本 shell内递增、真实 mounted bounds
  可点击，并最终得到 exact `LibraryRoute::Tags(TagId)` / `LibraryRoute::Trash` 与 selected
  styling。body-load/resource-read observers仍为空。

### Mutation sensitivity 与并行稳定性

- 独立复制 `app-lite-core` / `app-lite-gpui` 到 `/tmp/task6-b1-probe-mutation`，使用独立
  `CARGO_TARGET_DIR=/tmp/task6-b1-probe-mutation-target`，只在该临时副本删除
  `sidebar.rs` processor 的 `record_sidebar_uniform_list_range_for_test` 调用。同一 exact
  scale test按预期于 production processor挂载后的首个 probe断言 RED：
  `src/ui/tests.rs:371: the mounted shell must receive a real uniform-list request`。共享产品源码
  未被修改；共享快照上的同一 test随后 1/1 GREEN。这证明 probe断言确实连接真实
  uniform-list processor，不是直接调用 range helper或报告自证。
- 正常并行 runner以 `--test-threads=8` 连续三轮运行完整 `ui::tests::`，每轮均
  **52 passed / 0 failed**（1.04s、1.09s、1.11s）；此前全局累计导致的342-row随机串扰
  未复现，且无需串行规避。

### Fix round 2 独立验证

```text
RUSTFLAGS='-Awarnings' cargo test --quiet --manifest-path packages/app-lite-core/Cargo.toml
# PASS: 85 tests across all targets

RUSTFLAGS='-Awarnings' cargo test --quiet --manifest-path packages/app-lite-core/Cargo.toml \
  --features test-support
# PASS: 118 tests across all targets

RUSTFLAGS='-Awarnings' cargo test --quiet --manifest-path packages/app-lite-core/Cargo.toml \
  --features test-support --test library_query_stage_a -- --nocapture
# PASS: 7/7

RUSTFLAGS='-Awarnings' cargo test --quiet --manifest-path packages/app-lite-gpui/Cargo.toml \
  app::tests -- --nocapture
# PASS: 72/72

RUSTFLAGS='-Awarnings' cargo test --quiet --manifest-path packages/app-lite-gpui/Cargo.toml \
  --bin velotype ui::tests:: -- --test-threads=8
# PASS: 52/52, repeated three consecutive times

RUSTFLAGS='-Awarnings' cargo test --quiet --manifest-path packages/app-lite-gpui/Cargo.toml \
  --bin velotype mounted_sidebar_virtualizes_scale_fixture_and_reaches_tail_typed_routes \
  -- --nocapture
RUSTFLAGS='-Awarnings' cargo test --quiet --manifest-path packages/app-lite-gpui/Cargo.toml \
  --bin velotype ui::sidebar::tests -- --nocapture
# PASS: 1/1 each

RUSTFLAGS='-Awarnings' CARGO_TARGET_DIR=/tmp/task6-b1-probe-mutation-target \
  cargo test --quiet \
  --manifest-path /tmp/task6-b1-probe-mutation/packages/app-lite-gpui/Cargo.toml \
  --bin velotype mounted_sidebar_virtualizes_scale_fixture_and_reaches_tail_typed_routes \
  -- --nocapture
# EXPECTED RED after deleting only the temporary copy's record call: 0 passed / 1 failed,
# ui/tests.rs:371 request_count remained zero

RUSTFLAGS='-Awarnings' cargo test --quiet --manifest-path packages/app-lite-gpui/Cargo.toml \
  --bin velotype -- \
  --skip editor::selection::tests::cross_block_cut_writes_markdown_deletes_range_and_undo_restores
# PASS: 1,169 passed / 0 failed / 1 exact pre-existing donor skip

RUSTFLAGS='-Awarnings' cargo check --quiet \
  --manifest-path packages/app-lite-gpui/Cargo.toml --tests
RUSTFLAGS='-Awarnings' cargo check --quiet --release \
  --manifest-path packages/app-lite-gpui/Cargo.toml
cargo fmt --manifest-path packages/app-lite-core/Cargo.toml -- --check
cargo fmt --manifest-path packages/app-lite-gpui/Cargo.toml -- --check
git diff --check
# PASS
```
