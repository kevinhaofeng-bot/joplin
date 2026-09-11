# Task 5 格式工具栏与白色主表面独立审查

审查基线：`a91b842da97ee48db2f05e7bf0cacfd6e637a020` 至当前共享 dirty worktree（2026-09-12）。

结论：**CHANGES REQUESTED**

计数：**0 Critical / 1 Important / 1 Minor**。

本报告只审查新增共享格式 Chrome、默认资料库接入、Spike 迁移和白色主表面；没有修改生产代码。Task 5 Step 5 的 fresh temporary-profile Release 人工验收仍未完成，本轮自动测试不能替代该 M1 门槛。

## Source-first 核对

| 行为权威 | 当前生产实现 | 独立核对 |
| --- | --- | --- |
| `/Users/kevinhao/Projects/joplin-reconstruction/evernote-11.32.5/common-editor-sourcemap/@evernote/common-editor/src/apps/peso/commands.ts:1-40,43-46,75-101,104-117,135-149` 集中聚合 clipboard/history/heading/list/paragraph/link/resource/textformatter 命令；`docs/research/evernote-11.32.5-targeted-reverse.md:77-85` 要求 toolbar/overflow 共用一个 action catalogue 和 selection-derived state。 | `/Users/kevinhao/Projects/joplin/.worktrees/joplin-lite-native-rust-mvp/packages/app-lite-gpui/src/native_editor/toolbar.rs:366-395,639-679,1069-1184` 的一个 `EditorCommandChrome` 持有一个 `CommandCatalogue`，primary/More 都调用同一 `render_command_button`/`execute_command`；`spike_app.rs:440-456` 与 `ui/mod.rs:871-889` 构造同一实体类型。 | Library 与 Spike 没有第二份格式 mutation handler；每个 enabled descriptor 都落入 `CommandCatalogue::execute`，disabled descriptor 不安装 mouse handler。定向 mounted 测试和 GPUI 全量均通过。见 T7805-M1：placement/group 元数据仍有重复 truth，但当前命令集合的实际 handler 是共用的。 |
| `.../modules/textformatter/commands/boolformat.ts:14-41,48-58`：拒绝结构 selection；一次 transaction toggle mark；command 后回焦；状态从当前 selection/stored marks 查询而不是 UI bool。 | `native_editor/commands.rs:305-443,447-510` 从 `EditorCore` selection/document/history 得 enabled/toggle，并通过一个 transaction/history 执行；`native_editor/toolbar.rs:639-679` 执行后回焦。 | `mounted_library_shared_chrome_clicks_bold_and_list_with_one_history_entry` 真点击 Bold/list，selection 不变、各增加一条 history、一次 Undo 恢复；`all_visible_commands_execute_or_are_disabled` 覆盖 descriptor 唯一性和 enabled 命令真实 mutation。 |
| 同一 `boolformat.ts::execCommand` 的文档 transaction 语义要求格式进入正常保存链，而不是 Chrome 局部状态。 | Chrome 更新的是当前 `NoteSession` 已持有的 `EditorCore`；editor notify 进入既有 Task 4 journal/snapshot coordinator。 | `mounted_library_chrome_format_manual_sync_switch_and_reopen_round_trips_canonical_html` 真点击 Bold → ManualSync → 切换并重建 session，SQLite canonical HTML 含 `<strong>`，重解码仍为 `Mark::Bold`。该测试也会在 editor notify/save 接线被移除时失败。 |
| `targeted-reverse.md:80-85`：toolbar/popover 保留 selection；Link 输入临时取得焦点；Apply/Cancel 回焦；selection change、Escape、outside click 必须确定性关闭 stale menu，popover 约束在可见窗口。 | Link input 是真实 `EntityInputHandler`，Apply/Cancel 共用 Chrome；More/Link panel 本身有 window-bound placement。 | Apply 路径和 note-switch teardown 测试为绿，但 outside click、selection-change、More Escape 没有实现完整，见 T7805-I1。 |
| `commands.ts:104-117` 只表明 resource 是 typed command；Library 的 SQLite staging/picker policy 是本产品独立安全边界。 | `native_editor/toolbar.rs:647-659` 只 emit `RequestInsertImage`；`ui/mod.rs:879-889,993-1022,1944-1995` 由 Library 先捕获 `InsertIntent`，再启动 native picker，completion 继续走 `NoteSession` staged resource transaction；Spike 仍使用自己的实验 adapter。 | `mounted_library_chrome_insert_image_event_uses_the_saved_selection_durable_picker_route` 真点击按钮后检查 pending intent、当前 mounted image/cache、恰一条 DB relation 和 canonical `<img>`；既有 Task 5 saved-selection/fencing tests 继续由全量覆盖。 |
| `.../modules/content/commands/exportDesignTokens.generated.ts:172,222-226`：`--color-background-fill-primary` 和 `--color-surface-fill-primary-enabled` 均解析到 `--colors-grey-100: #fff`。 | `ui/mod.rs:75-104,315-327,1657-1672,1921,2072-2083,2129-2159` 对 shell/main editor/actions/title/editor pane 显式传入 opaque `0xffffffff`。 | `mounted_default_editor_shell_paints_every_evernote_primary_surface` 每次 render 先把五个 observation slot 清零，且每个 slot 只在对应生产 `.bg(self.evernote_primary_surface_fill(...))` 的实参求值时写入；删掉任一生产 `.bg` 会留下 `0`，因此不是只 grep 源码的弱测试。它仍不是 fresh-profile Release 目视验收。 |

## Important

### T7805-I1 — Shared overlay 只保护了面板矩形；Library 的 outside click 会穿透正文，More 也不会关闭

位置：

- `/Users/kevinhao/Projects/joplin/.worktrees/joplin-lite-native-rust-mvp/packages/app-lite-gpui/src/native_editor/toolbar.rs:386`：editor observation 只 `notify`，selection 变化不会清理 `more_open`。
- `/Users/kevinhao/Projects/joplin/.worktrees/joplin-lite-native-rust-mvp/packages/app-lite-gpui/src/native_editor/toolbar.rs:1123-1147`：`.occlude()` 只覆盖 220pt More 菜单自身的 hitbox，没有 outside backdrop。
- `/Users/kevinhao/Projects/joplin/.worktrees/joplin-lite-native-rust-mvp/packages/app-lite-gpui/src/native_editor/toolbar.rs:1233-1255`：Link 虽有全屏取消层，但该层只是普通 `on_mouse_down` hitbox，缺少 `.occlude()`；后方 surface 仍被 GPUI 判为 hovered。
- `/Users/kevinhao/Projects/joplin/.worktrees/joplin-lite-native-rust-mvp/packages/app-lite-gpui/src/ui/mod.rs:2129-2163`：Library root 只是挂载 overlays，没有调用 shared `dismiss_overlay`，也没有像 Spike 那样停用 surface pointer handler。
- 对照 `/Users/kevinhao/Projects/joplin/.worktrees/joplin-lite-native-rust-mvp/packages/app-lite-gpui/src/spike_app.rs:872-891,974-987,1121-1123,1187-1204`：Spike 在 overlay 打开时移除自己的 capture handler，并由 root outside click 调用 `dismiss_overlay`；所以报告所称“Library/Spike 同一 Link/More 逻辑”并不成立。
- GPUI 0.2.2 语义证据：`/Users/kevinhao/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/gpui-0.2.2/src/window.rs:499-565,755-795` 明确说明普通 hitbox 不影响后方 hitbox，只有 `BlockMouse`/`.occlude()` 才截断；`EditorSurface` 在 `/Users/kevinhao/Projects/joplin/.worktrees/joplin-lite-native-rust-mvp/packages/app-lite-gpui/src/native_editor/surface.rs:187-259,340-346` 使用 capture-phase mouse-down，因此会先于普通 backdrop 的 bubble handler改动 selection。

复现逻辑：

1. 在默认 Library route 选择一段正文，打开 More，然后点击菜单矩形之外、正文 canvas 内的位置。
2. 点击坐标不在 More 的唯一 `.occlude()` hitbox 内，`EditorSurface::on_mouse_down` 会把 selection 改成该点 caret；Library 没有 root dismissal，`more_open` 仍为 `true`。用户看到的是绑定旧打开上下文的 stale menu。
3. 打开 Link 后点击 popover 外的正文也会命中全屏普通 backdrop和后方 surface。GPUI 先执行 surface capture，正文 selection 被改变，再在 bubble phase取消 Link；这直接违反 brief 的“More/Link overlay 不能把鼠标事件穿透到 library EditorSurface capture handler”。
4. 用键盘移动 selection 或在 More 打开时按 Escape，同样没有 shared stale-dismiss 路径；`_editor_subscription` 只触发 redraw。

为何阻断：`task-5-format-chrome-brief.md:7,11,18-19` 要求 Library/Spike 共用同一 Link/More 逻辑并阻止 overlay 穿透；`targeted-reverse.md:80-85` 又明确要求 selection change、Escape、outside click 确定性关闭 stale menu。当前测试 `ui/tests.rs:513-617` 只点击 More **内部** row，所以只能证明菜单自身 `.occlude()`；`ui/tests.rs:806-930` 只点击 Link **内部** Apply，所以完全绕过有缺陷的 outside backdrop。

最小修复方向：让 shared `EditorCommandChrome::render_for_host` 自己提供覆盖整个 window/content-mask、真正 `.occlude()` 的 More/Link backdrop，并由它调用 `dismiss_overlay`；或者为两个 host 统一提供同一个 pointer gate/outside/Escape adapter，Library 不得缺席。selection observation 也应按 pinned behavior 关闭 stale More（拖选期间需要按既有 rule 延后）。不要只在 Library 再造一套菜单 handler。

必须新增的 mutation-sensitive mounted 验收：

- Library：选择真实 text range → 打开 More → 点击菜单外的真实 `native-editor-surface` 坐标；断言 surface 没收到该 overlay click、selection 未被意外改写、More 已关闭。
- Library：打开 Link → 点击 full-screen backdrop；断言 popover 关闭、editor selection 保持、editor 回焦。临时删除 backdrop `.occlude()` 必须 RED。
- Library 与 Spike：More 打开后由真实 keyboard/selection route 触发 selection change 和 Escape，断言同一 shared Chrome 都关闭且没有 history entry。

## Minor

### T7805-M1 — 当前命令 handler 共用，但 placement/group 仍有三份可漂移 truth

位置：

- `/Users/kevinhao/Projects/joplin/.worktrees/joplin-lite-native-rust-mvp/packages/app-lite-gpui/src/native_editor/commands.rs:63-71,108-140,282-303` 已在 `CommandDescriptor` 声明 `group`/`primary`，并提供 `primary_descriptors`。
- `/Users/kevinhao/Projects/joplin/.worktrees/joplin-lite-native-rust-mvp/packages/app-lite-gpui/src/native_editor/chrome.rs:449-518` 又维护独立 `PRIMARY_ORDER` 和 `OVERFLOW_PRIORITY`。
- `/Users/kevinhao/Projects/joplin/.worktrees/joplin-lite-native-rust-mvp/packages/app-lite-gpui/src/native_editor/toolbar.rs:1160-1184,1261-1274` 再维护一份 `toolbar_group`；它甚至把 InsertImage 与 Undo/Redo 分组，而 descriptor 当前把三者都标为 group 0。仓库内 `CommandDescriptor.group` 和 `primary_descriptors` 没有生产消费者。

当前命令集合恰好被测试锁住，因此这是可维护性/未来漂移风险而非当前用户可见失败。最小方向是让 descriptor 表成为 primary order、group boundary、overflow membership/rank 的唯一来源；responsive width cost可以保留为纯 presentation 数据，但不要再复制 membership/group。

## 已验证通过且未发现回归的部分

- Library/Spike 均挂载同一 `EditorCommandChrome` 类型，并绑定各自现有的同一个 `EditorCore`。
- enabled primary/More 命令进入同一 `CommandCatalogue::execute`；disabled 项不安装点击 handler；Link 无效 URL 留在 popover并显示错误。
- Bold/list 的真实 mounted click、selection 保留、单 history、Undo、ManualSync、canonical HTML、session 重建均通过。
- InsertImage 在 Library 没有回退到 Spike direct path；typed event 后仍使用 staged durable transaction。
- note switch 先销毁旧 Chrome subscription/entity，再销毁 surface/session；旧 Link/More 不会跨 note 复活。
- 窄宽由 `ui/mod.rs:2037-2050` 用 window width 减实际 visible sidebar/list persisted width得到，不是直接使用整窗宽；400pt mounted test 真正把 list command移动进 More并执行。
- 白色主表面有 Evernote design-token 依据，五个 production `.bg` 调用及 mounted test 均在当前快照存在。

## 独立验证

```text
git diff --check
# PASS

# 9 个新增/关键 mounted target（white surface、Library shared mount、Bold/list、
# canonical save/reopen、narrow More、session fence、durable InsertImage、Link focus、Spike shared mount）
# PASS: 9/9

cargo test --quiet --manifest-path packages/app-lite-gpui/Cargo.toml -- \
  --skip editor::selection::tests::cross_block_cut_writes_markdown_deletes_range_and_undo_restores
# PASS: 1153 passed / 0 failed / 1 exact documented donor test filtered; 15.25 s
```

编译仍输出既有 `objc` macro `unexpected cfg(cargo-clippy)`、unused/dead-code warnings；本轮没有把这些已知 warning 升格为格式 Chrome 阻断。由于 T7805-I1 尚未关闭，当前不能以全量绿或报告自证替代修复与新增 mounted interaction tests。

---

## T7805-I1 修复独立复审（2026-09-12，superseding verdict）

结论：**CHANGES REQUESTED**

计数：**0 Critical / 1 Important / 1 Minor**。

本节取代上面的首次 verdict。原 T7805-I1 的生产缺陷已经修正：共享 Chrome 现在提供真正阻断后方 hitbox 的全根节点 backdrop，Library 的 More/Link outside click、Escape、selection-change stale More 和 Link 输入保留 selection/history/focus 均由真实 mounted 路径证明；Spike 的 outside click 与 selection-change 路径也有真实 mounted 证明。剩余阻断是 Spike 独立 Escape host adapter 没有 mutation-sensitive 测试。Task 5 Release M1 fresh-profile 人工验收仍是单独门槛，本复审不以自动测试替代。

### 已关闭：原 T7805-I1 生产缺陷

- `/Users/kevinhao/Projects/joplin/.worktrees/joplin-lite-native-rust-mvp/packages/app-lite-gpui/src/native_editor/toolbar.rs:1258-1295`：`render_for_host` 在任一 overlay 打开时先挂载 `.absolute().size_full().occlude()` backdrop，再挂载 More menu/Link panel。由 sibling 插入顺序可见 panel 位于 backdrop 之上；backdrop 的 mouse-down stop propagation 并只调用同一 `dismiss_overlay`。这既阻断后方 `EditorSurface` capture handler，也没有遮住面板内部控件。
- `/Users/kevinhao/Projects/joplin/.worktrees/joplin-lite-native-rust-mvp/packages/app-lite-gpui/src/ui/mod.rs:2153-2188` 与 `/Users/kevinhao/Projects/joplin/.worktrees/joplin-lite-native-rust-mvp/packages/app-lite-gpui/src/spike_app.rs:1207-1225`：两个 host 都是 `.size_full().relative()` 根节点，且把 shared overlays 挂在正文/scroll 之后，故 backdrop 的 full-size 契约确实覆盖 host 根节点而不是仅覆盖 toolbar 行。
- `/Users/kevinhao/Projects/joplin/.worktrees/joplin-lite-native-rust-mvp/packages/app-lite-gpui/src/native_editor/toolbar.rs:398-410`：observer 比较完整 `Selection`，仅 selection 真变化时关闭 stale More；Link 的独立 URL input 不改 editor selection，因此不会被普通输入误关。
- `/Users/kevinhao/Projects/joplin/.worktrees/joplin-lite-native-rust-mvp/packages/app-lite-gpui/src/ui/mod.rs:1830-1851` 与 `/Users/kevinhao/Projects/joplin/.worktrees/joplin-lite-native-rust-mvp/packages/app-lite-gpui/src/spike_app.rs:997-1015`：两 host 的 Escape adapter 都只转发给 shared `dismiss_overlay`，不复制 overlay 状态；源码路径本身正确。
- `/Users/kevinhao/Projects/joplin/.worktrees/joplin-lite-native-rust-mvp/packages/app-lite-gpui/src/ui/tests.rs:938-1392`：Library 四组新 mounted tests 分别真实点击正文范围内的 backdrop、发送 Escape、用真实 `right` keyboard route 改 selection、向真实 Link input 输入 URL，并断言 overlay、完整 selection、undo depth 与 focus。特别是删除 backdrop `.occlude()` 会让 capture-phase surface 改写 selection，outside-click tests 会 RED；删除 Library root Escape binding 会让 More 保持打开，Escape test 会 RED；把 observer 改成每次 notify 都关闭则 Link 输入测试会 RED。
- `/Users/kevinhao/Projects/joplin/.worktrees/joplin-lite-native-rust-mvp/packages/app-lite-gpui/src/spike_app.rs:4437-4504`：Spike 的真实 outside click 和真实 `right` selection-change 路径均断言 shared More 关闭；后者还验证 selection 确实变化且 history 不增加。

## Important

### T7805-I2 — Spike 的 More→Escape 是独立 host 路由，但当前测试矩阵无法检测它被删掉

位置：

- `/Users/kevinhao/Projects/joplin/.worktrees/joplin-lite-native-rust-mvp/packages/app-lite-gpui/src/spike_app.rs:997-1015,1207-1215` 定义并挂载 Spike 专属 `on_root_key_down`。这不是 shared Chrome 内部路径；Library 有另一份 `/Users/kevinhao/Projects/joplin/.worktrees/joplin-lite-native-rust-mvp/packages/app-lite-gpui/src/ui/mod.rs:1830-1851,2153-2170` adapter。
- `/Users/kevinhao/Projects/joplin/.worktrees/joplin-lite-native-rust-mvp/packages/app-lite-gpui/src/spike_app.rs:3354-3456` 的现有 Link 测试不能覆盖该路径：它在 `simulate_keystrokes("escape")` 之前已经直接调用 `view.cancel_link(...)`，所以 Escape 到达时 popover 已关闭，删除 Spike root key binding 仍会 GREEN。
- 当前新增 Spike tests 只覆盖 `/Users/kevinhao/Projects/joplin/.worktrees/joplin-lite-native-rust-mvp/packages/app-lite-gpui/src/spike_app.rs:4437-4504` 的 outside click 和 selection-change，没有 More 打开后发送 Escape 的断言。Library 的 Escape test不能证明另一个 host 的 `.on_key_down` 仍挂载。

复现/Mutation：只删除 `spike_app.rs:1214` 的 `.on_key_down(cx.listener(Self::on_root_key_down))`（或使 `on_root_key_down` 对 Escape 提前返回）。现有 45 个 `spike_app::tests::` 仍没有测试会要求打开的 More 被 Escape 关闭；Library 的 6 个 shared tests也不经过 Spike root。由此实现者报告中“删除 host Escape route 会 RED”只对 Library 成立，不能支撑“Library/Spike 同一 Link/More 逻辑”的双 host acceptance。

为何阻断：首次审查已明确要求“Library 与 Spike：More 打开后真实 keyboard/selection route 触发 selection change 和 Escape”；本轮父任务也再次点名核查两 host 的 Escape。生产代码目视正确不能代替独立 host event wiring 的 mutation-sensitive 回归证据。

最小修复：新增一个 Spike mounted test：点击 `evernote-native-spike-more-trigger` → 断言 More mounted/open → `simulate_keystrokes("escape")` → 断言 More unmounted/closed、原完整 directional selection 不变、undo depth 不变、editor focus 恢复。该测试必须在删除 `SpikeView` root `.on_key_down` binding 时 RED。可同时修正现有 Link test，使 Escape 在 popover仍打开时发生，避免当前先 Cancel 后 Escape 的空断言。

## Minor（沿用）

### T7805-M1 — placement/group 仍有三份可漂移 truth

首次审查的 T7805-M1 未由本轮 overlay 修复触及，仍是非阻断维护性问题：descriptor、`chrome.rs` primary/overflow order 与 `toolbar_group` 尚未收敛为一个 declarative source。

## 独立验证

```text
cargo test --quiet --manifest-path packages/app-lite-gpui/Cargo.toml mounted_library_shared_ -- --nocapture
# PASS: 6 passed / 0 failed

cargo test --quiet --manifest-path packages/app-lite-gpui/Cargo.toml more_menu_dispatches_catalogue_command_and_restores_editor_focus -- --nocapture
# PASS: 1 passed / 0 failed（含 Spike outside click）

cargo test --quiet --manifest-path packages/app-lite-gpui/Cargo.toml spike_shared_more_closes_on_real_editor_selection_change_without_history -- --nocapture
# PASS: 1 passed / 0 failed

cargo test --quiet --manifest-path packages/app-lite-gpui/Cargo.toml editable_link_popover_uses_input_bridge_and_preserves_selection -- --nocapture
# PASS: 1 passed / 0 failed；但源码检查确认其 Escape 断言发生在直接 Cancel 之后，不能作为 Escape mutation gate

cargo test --quiet --manifest-path packages/app-lite-gpui/Cargo.toml --bin velotype -- \
  --skip editor::selection::tests::cross_block_cut_writes_markdown_deletes_range_and_undo_restores
# PASS: 1158 passed / 0 failed / 1 exact documented donor test filtered；14.67 s

git diff --check
# PASS
```

---

## T7805-I2 最终复审（2026-09-12，final superseding verdict）

结论：**APPROVED**

计数：**0 Critical / 0 Important / 1 Minor**。

上一节的唯一 Important 已关闭：

- `/Users/kevinhao/Projects/joplin/.worktrees/joplin-lite-native-rust-mvp/packages/app-lite-gpui/src/spike_app.rs:4506-4570` 新增 `spike_shared_more_escape_dismisses_without_selection_or_history_mutation`。测试通过真实 `evernote-native-spike-more-trigger` mounted hitbox 打开 More，先断言 menu state 确实 open，再发送 `simulate_keystrokes("escape")`，之后断言 shared More 已关闭、原完整 range selection 未变、undo depth 未变且 editor focus 恢复。
- 该测试明确依赖 `/Users/kevinhao/Projects/joplin/.worktrees/joplin-lite-native-rust-mvp/packages/app-lite-gpui/src/spike_app.rs:997-1015,1207-1215` 的 Spike root key adapter；More 保持 editor focus，Escape 没有 LinkPopover 自有 handler 可代为关闭，因此删除 root `.on_key_down(cx.listener(Self::on_root_key_down))` 会使 `chrome_more_open` 断言 RED。它补上了 Library/Spike 双 host event-wiring 矩阵的最后缺口。
- 原 T7805-I1 的 shared `.occlude()` backdrop、Library/Spike outside click、selection-change stale More、Library Escape、Link 输入不误关及 selection/history/focus 保留均保持通过；本次增量未发现回归。

剩余唯一 T7805-M1 是上文 placement/group 多份 declarative truth 的维护性风险，不影响当前用户可见行为，继续列为非阻断 Minor。

独立验证：

```text
cargo test --quiet --manifest-path packages/app-lite-gpui/Cargo.toml \
  spike_shared_more_escape_dismisses_without_selection_or_history_mutation -- --nocapture
# PASS: 1 passed / 0 failed

cargo test --quiet --manifest-path packages/app-lite-gpui/Cargo.toml --bin velotype -- \
  --skip editor::selection::tests::cross_block_cut_writes_markdown_deletes_range_and_undo_restores
# PASS: 1159 passed / 0 failed / 1 exact documented donor test filtered；14.87 s

git diff --check
# PASS
```

本批准仅针对 Task 5 格式 Chrome 的实现与自动化验收。Task 5 Release M1 的 fresh temporary-profile 人工验收仍未完成，仍是独立 release 门槛。
