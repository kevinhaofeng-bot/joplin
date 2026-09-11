# Task 5 格式 Chrome：共享接入证据报告

状态：默认 `LibraryShell` 已接入和 Spike 同一个 `EditorCommandChrome`，本轮只改 UI composition 与 typed picker adapter。Release 截图确认此前启动的是陈旧 local target；当前源码/共享 release 的浅色 route 已补齐，但 Task 5 fresh temporary-profile Release 手工验收**待重新执行**，提交与推送继续禁止。

## Evernote 源码 → Rust → mutation-sensitive 验证

| Evernote 源码路径、符号与实际观察行为 | 本产品 Rust 实现 | mutation-sensitive 验证与结果 |
| --- | --- | --- |
| `/Users/kevinhao/Projects/joplin-reconstruction/evernote-11.32.5/common-editor-sourcemap/@evernote/common-editor/src/apps/peso/commands.ts`：集中 import clipboard、history、heading、list、paragraph、link、resource、textformatter 等命令模块；命令入口不是每个 toolbar 各造一套 mutation。 | `packages/app-lite-gpui/src/native_editor/toolbar.rs::EditorCommandChrome` 只持有一个 `CommandCatalogue`；`EditorCommandChromeHost::{Spike,Library}` 只决定 selector/placement，`execute_command` 对两 host 共用。`spike_app.rs::new_spike_command_chrome` 与 `ui/mod.rs::LibraryShell::sync_editor_surface` 都构造这一同一组件。 | `ui::tests::mounted_default_route_mounts_the_shared_editor_command_chrome` PASS：实际 card-select/redraw 后断言 `library-editor-command-chrome`/toolbar/Bold 存在且位置介于 title/body；移除 Library mount 则 selector/结构断言 RED。`spike_app::tests::` 44/44 PASS，保留 Spike toolbar/More/Link 行为而没有第二份 state。 |
| `/Users/kevinhao/Projects/joplin-reconstruction/evernote-11.32.5/common-editor-sourcemap/@evernote/common-editor/src/apps/peso/modules/textformatter/commands/boolformat.ts::execCommand`：拒绝 `NestedSelection`/`NodeSelection`；在一个 ProseMirror transaction 中 toggle mark、`dispatch(tr)`，然后若 editor 未 focus 则 `view.focus()`。`queryCommandValue` 从 current selection/ranges 得 active/mixed，不存 toolbar 本地 bool。 | `native_editor/toolbar.rs::EditorCommandChrome::execute_command` → `native_editor/commands.rs::CommandCatalogue::execute` → `EditorCore::apply`；state 从 `CommandCatalogue::state(editor)` 导出，Chrome 不存格式 toggle。每次 command 后 `focus_editor`。 | `ui::tests::mounted_library_shared_chrome_clicks_bold_and_list_with_one_history_entry` PASS：真实按钮点击 Bold 和 Bulleted list，selection 不变、each `undo_depth + 1`、一次 Cmd-Z 恢复。删除真实 Chrome click/改为 UI local state 会使 document/history 断言 RED。 |
| 同一 `boolformat.ts::execCommand` 的 transaction/dispatch 语义：格式结果属于文档而非 toolbar；焦点不是保存替代品。 | `app/note_session.rs` 的既有 editor observation 接到同一个 core；`ui/mod.rs::LibraryShell::apply_action(AppAction::ManualSync)` 沿 Task 4 的 save coordinator，`native_editor/codec.rs` 写/读 canonical HTML。 | `ui::tests::mounted_library_chrome_format_manual_sync_switch_and_reopen_round_trips_canonical_html` PASS：真 Bold → UI ManualSync → 选择另一 note → 重建原 note session，持久 `body_html` 含 `<strong>` 且重解码 mark 仍为 Bold。 |
| `docs/research/evernote-11.32.5-targeted-reverse.md::Toolbar command state and focus preservation`：toolbar 与 overflow menu 是一个 action catalogue 的两种 placement；popovers 保存 editor selection，普通 mouse-down 不应把 selection 改成点击位置，关闭后回 editor focus。 | `native_editor/toolbar.rs::toolbar_placement/render_for_host/render_more_menu/render_link_popover`；`LibraryShell::render_editor_panel` 仅放置 returned toolbar/overlays。More 和 Link 面板 `.occlude()`，防止 library `EditorSurface` 的 capture-phase pointer handler 在点击 command/input 时移动正文 selection；可用宽度由实际编辑列（扣除 visible sidebar/list）传入，而非整窗宽度。 | `ui::tests::mounted_narrow_library_chrome_moves_list_command_into_shared_more_and_executes_it` PASS：400pt、折叠导航时 Bulleted list 先不在 primary，进入同一 More 后能执行且 More state 关闭。该测曾 RED，根因是 popup click 穿透 native surface；没有 `.occlude()` 时 caret 被改到正文末端、命令不执行。 |
| 同一 targeted reverse 的明确行为：selection 改变、Escape 和 outside click 都必须确定性关闭陈旧菜单；toolbar/普通 popover 的 pointer-down 不得清除正文 selection。 | `native_editor/toolbar.rs::EditorCommandChrome::render_for_host/overlay_backdrop_id/dismiss_overlay` 在任何 More 或 Link overlay 打开时，先返回透明、全窗口、`.occlude()` 的 backdrop；再按层级返回 menu/panel，使内部控制仍在 backdrop 上方。backdrop stop propagation 后只调用共享 `dismiss_overlay` 并返 `EditorCore` focus。`ui/mod.rs::LibraryShell::on_shell_key_down` 与 `spike_app.rs::SpikeView::on_root_key_down` 只是将 Escape 路由到同一 shared API，不复制 overlay state。 | 三条 mounted 默认 Library 路径先 RED 后 GREEN：`mounted_library_shared_more_outside_click_dismisses_without_mutating_selection_or_history`、`mounted_library_shared_link_outside_click_dismisses_without_mutating_selection_or_history`、`mounted_library_shared_chrome_escape_dismisses_more_and_link_without_editor_mutation`。均从真实 surface 外点/Escape 断言 overlay 关闭、selection/history 不变且 editor refocus；删除 backdrop `.occlude()` 或 Library host Escape route 会 RED。Spike 另有 `spike_shared_more_escape_dismisses_without_selection_or_history_mutation`：真实 More→Escape，断言关闭/selection/history/refocus；临时删除 `SpikeView` root `.on_key_down(cx.listener(Self::on_root_key_down))` 后该测试按预期 RED，再恢复 GREEN。Spike `spike_app::tests::` 46/46 PASS。 |
| 同一 targeted reverse 的“selection changes close stale menus”规则；Link 是独立 temporary input owner，URL 输入本身不能伪造 document selection change。 | `native_editor/toolbar.rs::EditorCommandChrome::last_editor_selection` 与其 `EditorCore` observer：每次 core notify 都比较完整 directional `Selection`；只有实质变化才关闭 `more_open`。它不改 `link_popover`，因 Link 保留原正文选择、输入只更新 `LinkPopover` 自己。 | `ui::tests::mounted_library_shared_more_closes_on_real_editor_selection_change_without_history_or_link_input_corruption` 先 RED 后 GREEN：真实 `right` 键经默认 Library `EditorSurface` 改 selection 后关闭 More、无 history；随后的 URL input 保持 Link open/selection。`spike_app::tests::spike_shared_more_closes_on_real_editor_selection_change_without_history` 同样通过 Spike 的真实 surface key route。删除 selection comparison 或改为所有 notify 都关闭，会分别 RED。 |
| 同一 reverse source：Link/More 的 overlay 是当前 editor session 的暂态，不可跨 note 展示。 | `ui/mod.rs::LibraryShell::sync_editor_surface` 先置空 `_command_chrome_event_subscription` 与 `command_chrome`，再释放 old surface/session；新 session 创建 new Chrome。 | `ui::tests::mounted_library_shared_link_popover_keeps_selection_and_returns_editor_focus` PASS：opening Link 保留 selection，URL field 获取焦点，Apply 生成一个 Link history entry 并返 editor focus。`mounted_library_note_switch_discards_the_previous_shared_link_and_more_overlays` PASS：switch 后新 Chrome 无 Link/More state；若保留旧 entity/state 则 RED。 |
| `/Users/kevinhao/Projects/joplin-reconstruction/evernote-11.32.5/common-editor-sourcemap/@evernote/common-editor/src/apps/peso/commands.ts` 的 resource command 聚合只表明资源操作是 typed editor command；它不规定本产品的 SQLite staging/picker policy。 | `native_editor/toolbar.rs::EditorCommandChromeEvent::RequestInsertImage` 是唯一 shared typed boundary。`ui/mod.rs::LibraryShell` subscription 先 `begin_resource_picker` 捕获 `InsertIntent`；`dispatch_library_resource_picker` 仅在生产启动 native picker；`complete_resource_picker_path` 继续走原有 `NoteSession` staged durable import。 | `ui::tests::mounted_library_chrome_insert_image_event_uses_the_saved_selection_durable_picker_route` PASS：真 Insert image click 先观测 `pending_resource_insert`，随后调用 production picker completion seam，worker 完成后当前 mounted doc/cache 有 image、DB 恰一条 relation、canonical HTML 有 `<img>`；不调用 Spike direct insertion。 |

## 明确的独立产品设计

- GPUI `EditorCommandChrome` 和 `EditorCommandChromeHost` 是本产品为避免 Spike/default route 双实现作出的独立结构化抽取；Evernote 源码提供“单 command catalogue、selection-derived state、focus preservation”行为依据，不声称复刻其 React/ProseMirror UI。
- Library 的 native picker adapter 是独立 macOS/Task 5 safety policy：event 先保存 full Selection，再打开 OS panel，completion 才进入 staged durable import。测试不弹 OS panel，而调用同一 completion seam；这不是另一条插图实现。
- library 实际编辑列宽、GPUI window-overlay `.occlude()` 是本产品处理分栏布局/capture pointer 的必要适配。特别是全窗口透明 backdrop 是 GPUI capture-phase 的独立实现选择：Evernote 证据规定 selection-preserving 的 outside/Escape 语义，不声称其 React/ProseMirror 使用相同 hit-test API；其正确性由上表 mounted route 测试而非 source-string 比对证明。

## 本轮范围评估：descriptor placement/group 真值

独立复审指出 `CommandDescriptor::{primary,group}`、`chrome.rs::PRIMARY_ORDER` 与
`toolbar.rs::toolbar_group` 目前有三处 placement/group 表述。静态核对显示当前
`PRIMARY_ORDER` 的 15 个 command 与 descriptor 的 `primary=true` 过滤顺序相同；但
`toolbar_group` 将 Insert image 与 Undo/Redo 分成不同视觉组，而 descriptor 将三者都
标为 group 0。这是一个真实的 Minor 真值漂移，不把它误报为本轮已关闭。

将 placement 顺序、separator 计数和 responsive width cost 全部改为 descriptor 驱动是
一个独立的视觉策略收敛：它会改变现有 primary-row separator/窄宽阈值，需配套明确的
layout contract，而非和这次 capture/backdrop 安全修复混改。因此本轮保持稳定的格式
可见顺序，只记录为下一条小范围 Chrome layout 工作；More/Link outside、Escape 和
selection-change 状态机已由上表 mutation-sensitive tests 单独闭合。

## 本轮定向验证

```text
RUSTFLAGS='-Awarnings' cargo check --tests --manifest-path packages/app-lite-gpui/Cargo.toml
# PASS

RUSTFLAGS='-Awarnings' cargo test --quiet --manifest-path packages/app-lite-gpui/Cargo.toml ui::tests::mounted_library -- --nocapture
# PASS: 9 tests

RUSTFLAGS='-Awarnings' cargo test --quiet --manifest-path packages/app-lite-gpui/Cargo.toml ui::tests:: -- --nocapture
# PASS: 49 tests

RUSTFLAGS='-Awarnings' cargo test --quiet --manifest-path packages/app-lite-gpui/Cargo.toml spike_app::tests:: -- --nocapture
# PASS: 46 tests

RUSTFLAGS='-Awarnings' cargo test --quiet --manifest-path packages/app-lite-gpui/Cargo.toml \
  --bin velotype -- --skip editor::selection::tests::cross_block_cut_writes_markdown_deletes_range_and_undo_restores
# PASS: 1,159 passed, 0 failed, 1 exact documented donor test filtered

cargo fmt --manifest-path packages/app-lite-gpui/Cargo.toml -- --check
cargo fmt --manifest-path packages/app-lite-core/Cargo.toml -- --check
git diff --check
# PASS
```

Task 5 Step 5 的 fresh temporary-profile Release 人工验收待重新执行；完整更正记录见 `task-5-report.md` 的“M1 Release 实机验收”一节。自动测试没有替代该门槛，提交与推送仍须等待新的 shared-target Release 目视验收通过。
