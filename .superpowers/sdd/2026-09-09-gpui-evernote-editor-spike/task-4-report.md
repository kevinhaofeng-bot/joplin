# Task 4：统一 GPUI 编辑器输入面报告

日期：2026-09-09

## 结果

Task 4 已完成。新增 `EditorCore`、文档级 `EntityInputHandler`、统一布局缓存和渲染入口。编辑块不创建输入焦点；全文只有 `EditorCore` 持有 `FocusHandle`、`Selection`、IME marked range、事务历史和 `LayoutRegistry`。

## TDD 证据

### RED

先追加简报指定的三个 `#[gpui::test]`，按简报逐条运行：

```text
cargo test --manifest-path packages/app-lite-gpui/Cargo.toml ime_commit_preserves_utf16_selection
cargo test --manifest-path packages/app-lite-gpui/Cargo.toml cross_block_selection_includes_image_atom
cargo test --manifest-path packages/app-lite-gpui/Cargo.toml editing_commands_cross_block_boundaries
```

三个命令均以编译失败结束，错误为 `unresolved import super::core::EditorCore`；此时 `core.rs` 尚未创建，满足 RED 前置条件。

### GREEN

最终 native editor focused suite：

```text
cargo test --manifest-path packages/app-lite-gpui/Cargo.toml native_editor::tests
31 passed, 0 failed
```

覆盖内容包括指定的三个核心测试、跨块全文 UTF-16/IME 查询与替换、图片前后命中、所有编辑命令跨段落/图片/列表边界、10,000 块布局和 16 MiB 驱逐。

## Donor 复用映射

实现保留 donor 的边界语义和几何算法，再接入 Task 3 的 `Document`/`Selection`/`Transaction`，没有修改 donor 文件。

| donor 算法/函数 | native 适配位置 | 适配说明 |
| --- | --- | --- |
| `components/block/runtime/mod.rs` 的 `utf16_to_utf8_in`、`utf8_to_utf16_in`、范围转换 | `native_editor/input.rs` | 原有 UTF-16/UTF-8 端点算法原样迁移为模块级 helper，并补充 CJK、surrogate、越界 clamp 测试。 |
| `components/block/input.rs` 的 `text_for_range`、`selected_text_range`、`marked_text_range`、`unmark_text` | `native_editor/core.rs` 的 `EntityInputHandler for EditorCore` | 保留 GPUI 输入协议；坐标改为全文 UTF-16，换行和 `U+FFFC` 图片 atom 也进入同一映射。 |
| `components/block/input.rs` 的 `replace_text_in_range`、`replace_and_mark_text_in_range` | `core.rs` | 所有替换先转 `DocPoint`，再经 `Transaction` 和 `History`；组合输入只保存 `MarkedText`，不绕过文档模型。 |
| `components/block/input.rs` 的 `bounds_for_range`、`character_index_for_point` | `core.rs` + `layout.rs` | 范围走跨块 selection geometry，坐标命中走 `LayoutRegistry::point_to_doc`，最后反转回全文 UTF-16。 |
| `components/block/element.rs` 的 `hard_line_ranges`、`line_index_for_offset`、`wrapped_line_for_y`、`position_for_offset`、`cursor_bounds_for_offset` | `layout.rs` 的行/光标几何和 `point_to_doc` | 按 native `BlockLayout` 和 GPUI `WrappedLine` 改写；命中多行时累加此前行的 UTF-8 字节偏移。 |
| `components/block/element.rs` 的 `range_bounds`、`range_segment_bounds`、`aligned_line_left`、`point_inside_bounds` | `LayoutRegistry::range_bounds`、`selection_rects`、`contains` | 文本范围、图片 atom 和跨块选择都由同一个 registry 计算。 |
| `editor/selection.rs` 的 `normalized_cross_block_selection`、端点文档序、`sync_cross_block_selection_visuals` | `EditorCore::ordered_selection`、全文 flat offset、`LayoutRegistry::selection_rects` | 保留 anchor/head 正反向和跨块视觉顺序；native 剪贴板输出是 plain text，不引入 Markdown 事实源。 |
| `editor/selection.rs` 的 `delete_cross_block_selection` | `EditorCore::delete_selection` / `Transaction::DeleteRange` | 图片作为结构 atom 一并删除，撤销走同一 `History` 逆事务。 |

## 架构与输入坐标

- `EditorCore::from_document` 只调用一次 `cx.update(|app| app.focus_handle())`；之后通过 `focus_handle()` 借用/clone 同一个 handle。块、布局项和图片节点没有 focus handle。
- `render::paint_entity` 在 paint 阶段使用真实 `Entity<EditorCore>`：聚焦时调用 `ElementInputHandler::new(bounds, entity.clone())`，并把该 entity 注册给 GPUI `Window`。
- `text_for_range`、`selected_text_range`、`marked_text_range`、两个 replace 方法、`bounds_for_range`、`character_index_for_point` 均以全文可读串为坐标源：块间换行和图片 `U+FFFC` 不再被误当作当前活动块内偏移。
- `LayoutRegistry::point_to_doc` 对图片返回明确的 `Before`/`After`；文本位置按 shaped line 和真实缓存 `line_height` 映射。上下移动会把目标 offset 吸附到合法 grapheme 边界。

## 布局、缓存与渲染

- `LayoutRegistry::layout_document` 只为 viewport 加上下各一个 viewport 的预取窗口建立精确 block layout；远端仅保留估算高度。
- 默认 exact-layout 缓存预算是生产路径常量 `16 * 1024 * 1024`。`used_bytes` 对 `CachedBlockLayout`、shaped text/runs/glyphs 和 selection geometry 做保守计数；LRU 在每次布局后执行驱逐，超预算项不会留在缓存中。
- `long_document_layout_is_bounded` 覆盖 10,000 个块，`layout_cache_evicts_before_16_mib` 以大文本块验证真实预算与窗口边界，不使用测试专用替代缓存路径。
- `render.rs` 顺序固定为：block surfaces → selection rectangles → glyphs/images → caret；图片选区使用圆角几何，图片前后光标使用 registry 的竖直 caret geometry。

## 验收命令

```text
cargo test --manifest-path packages/app-lite-gpui/Cargo.toml --all-targets --no-run
PASS

cargo check --manifest-path packages/app-lite-gpui/Cargo.toml --bin velotype --offline
PASS

cargo test --manifest-path packages/app-lite-gpui/Cargo.toml --bin velotype -- \
  --skip editor::selection::tests::cross_block_cut_writes_markdown_deletes_range_and_undo_restores
804 passed, 0 failed, 1 skipped

cargo fmt --manifest-path packages/app-lite-gpui/Cargo.toml --all -- --check
PASS

git diff --check
PASS
```

完整 donor bin 并发运行曾有一次既有的 `validate_styles_scans_graphemes_once_per_text_on_transactions` 计数型测试瞬时比例失败；该测试单独重跑通过，随后按简报要求的精确 skip 命令完整通过。代码没有放宽该断言，也没有修改 donor 或计划文件。

## 剩余顾虑

Task 4 只允许修改 native editor 文件，因此 `paint_entity` 已提供真实 GPUI entity/input bridge，但尚未接入后续应用外壳的 measured-layout element。生产真机 IME 候选窗、真实主题字体 shaping、异步图片解码和 RSS/Metal 预算仍需后续 UI/性能验收；本报告不把 headless GREEN 误报为真机完成。

## Fix round 1（审查 findings 逐条闭环）

审查基线为 `f4fe91842`。本轮先以 focused 回归制造 RED，再逐项接入 donor 算法并保持 Task 3 的 `Document`/`Transaction`/`History` 边界。生产构造器、组合输入、视觉行、异构块边界和 grapheme 入口均没有绕过模型直接改块。

### RED/GREEN 记录

- Critical-1 的普通生产命令在基线因 `TestAppContext` 进入生产构造路径而 RED；新增 `EditorCore::new(Document, &mut Context<Self>)` 后 `cargo check --bin velotype --offline` GREEN。
- A 组 focused 回归先覆盖 `ime_internal_selection_does_not_shrink_marked_range`、`ime_partial_selection_and_candidate_updates_are_one_undo`、`return_replaces_selection_and_splits_as_one_undo`、`entity_input_replace_notifies_and_exposes_errors`；基线分别暴露 marked range、历史粒度和静默错误，修复后 GREEN。
- B 组先以 `shaped_cache_reuses_static_blocks_and_invalidates_changed_revision` 的缺失 `shape_count` 编译错误 RED，继而覆盖真实 `shape_text`/`WrappedLine`、硬换行、跨视口分段、图片 affinity、预算驱逐和高度回流；全部 GREEN。
- C 组新增视觉行、图片 Home/End、异构 Backspace/Delete、公开/fallback grapheme 入口回归；实现前视觉移动停在同一块、图片跳到首个文本块、异构边界无操作、组合字符入口落在非法 byte 位置，修复后全部 GREEN。

最终本地 focused native suite：

```text
cargo test --manifest-path packages/app-lite-gpui/Cargo.toml native_editor --offline
53 passed, 0 failed
```

### Findings 对照

| finding | 修复与证据 |
| --- | --- |
| Critical-1 生产构造 | `TestAppContext` 仅留在 `#[cfg(test)]` fixture；生产实体由 `EditorCore::new` 从 `Context<Self>::focus_handle()` 构造。普通 `cargo check --bin velotype --offline` 单独通过。 |
| Important-1 IME marked/selected | `MarkedText` 始终覆盖完整 inserted UTF-8 range；`new_selected_range` 只映射组合候选内部 selection；`composition_base` 用于候选更新/commit。 |
| Important-2 历史原子性 | `History::apply_batch_with_selection` 让 IME commit 和 Return（DeleteRange+SplitBlock）各为一个 undo entry；候选中间更新先撤销 provisional entry。 |
| Important-3 notify/error | 两个 `EntityInputHandler` 替换回调在成功和失败路径都 `cx.notify()`；`last_input_error` 可读，越界 UTF-16 range 明确报告 `DocumentError`。 |
| Important-4 软换行/真实高度 | `shape_visible_with_window` 直接调用 GPUI `shape_text(...).into_vec()`，保留 `WrappedLine`/真实 line height，实际高度回写 prefix index 并推动后续 block。 |
| Important-5 硬换行偏移 | donor hard-line ranges 的换行 byte 语义由 line_start_offset/line_position_for_offset 直接按借用 WrappedLine 长度复现；命中、caret、range segment 共用同一 hard-line start。 |
| Important-6 跨视口选择 | `selection_rects` 用全文 document order 与 visible block 求交，并逐 WrappedLine/soft row 返回 segment，不生成跨行 union selection。 |
| Important-7 图片 affinity | `Before < After` 进入 `point_key`；纯 caret 不产生 atom selection；图片 caret 按 affinity 左/右定位并采用邻接文本 line height；反向 Before/After range 仍命中图片。 |
| Important-8 真实 16 MiB | cache bytes 计入 `WrappedLine`、layout、text、wrap boundaries、runs、glyphs 和 selection geometry；visible 只留 geometry，拒绝/驱逐 shaped entry 后不留第二份 text lines；真实 128 KiB shaped block + 极小 budget 回归通过。 |
| Important-9 命中/扫描 | cache key 比较 `(revision,width,style_revision,line_height)` 并统计 shape reuse；prefix height index 支持 viewport 定位；静止视口不重 shape，10,000 块限制 visible/cache 和扫描量。 |
| Important-10 视觉导航 | LayoutRegistry 新增 `visual_move`、`visual_line_boundary`、`visual_edge_point`，用 donor WrappedLine position/closest-index 保持 preferred x；Home/End 限定当前视觉 row，图片不 fallback 到首个文本块。 |
| Important-11 异构边界 | Backspace 在 paragraph↔list/heading 边界按 donor 语义降级或先转换后合并；Delete forward 先将右块转换为左块 metadata 再 MergeBlocks；每个边界动作经单一 History batch。 |
| Important-12 合法位置/错误 | `set_caret_utf8`、全文 UTF-16 endpoint、vertical target、fallback hit 均按 affinity 吸附 grapheme；预算拒绝 shaped cache 时 hit 使用已验证 before/after endpoint，不凭像素制造 byte offset。 |
| Minor-1 图片选区样式 | render 生产路径通过 `image_selection_outline` + `outline` 绘制圆角 outline，不再 fill atom。 |
| Minor-2 register_exact | `register_exact` 同步 visible geometry、以 `before == after` 推断 image、保留 measured bounds/selection bytes，并使用缓存 line height。 |

### 本轮 donor 复用映射

| donor 位置/算法 | native 文件映射 |
| --- | --- |
| `components/block/input.rs:116-179` 的 EntityInputHandler 查询/替换协议 | `native_editor/core.rs` 的全文 `text_for_range`、`selected_text_range`、`marked_text_range`、replace/unmark；候选内部 selection 与 marked range 分离。 |
| `components/block/runtime/mod.rs:1539-1615,1726-1747` 的 composition base/marked 更新路径 | `EditorCore::replace_and_mark_utf16` / `commit_marked_text`；provisional transaction 经 History undo/reapply 合并成一次用户动作。 |
| `editor/selection.rs:1031-1052` 的输入 selection 更新 | `selection_for_input_range` + `point_for_document_offset_with_affinity`；整篇换行和 U+FFFC 都纳入 UTF-16 flat text。 |
| `editor/history.rs:77-137`、`editor/mod.rs:295-298`、`editor/tests.rs:2566-2668` 的捕获/非合并 undo 语义 | native `History::apply_batch_with_selection`；Return 和 IME commit 保留单条非合并 entry。 |
| `components/block/element.rs:242-263,279-312,353-512,938-985` 的 hard-line、WrappedLine、position/closest/range segment | `native_editor/layout.rs` 的 `line_start_offset`/`line_position_for_offset`、`wrapped_row_offsets`（仅视觉导航）、`visual_move`、`visual_line_boundary`、`range_segment_bounds_for_line`、`point_to_doc`。 |
| `editor/selection.rs:364-493` 的跨 viewport selection 端点/可见绘制 | `LayoutRegistry::point_key` + `selection_rects`，只输出可见逐行 segment。 |
| donor 的图片 Before/After affinity 与 caret geometry | `layout.rs::image_side`、`caret_bounds_for_point`、`visual_edge_point`；`render.rs` 只消费 registry geometry。 |
| donor `components/block/runtime/mod.rs:1879-2020`、`interactions.rs:663-704`、`events.rs:2167-2228` 的 preferred-x 视觉移动 | `LayoutRegistry::visual_move/visual_edge_point` + `EditorCore::preferred_x`，跨块入口使用同一 x。 |
| donor `interactions.rs:433-564`、`events.rs:1775-1817,2260-2335` 的异构 Backspace/Delete/降级/合并事件 | `EditorCore::backspace/delete_forward` 组装 `SetBlockKind`/`SetAlignment`/`MergeBlocks` `TransactionBatch`，不直接编辑块。 |
| donor `components/block/runtime/mod.rs:2149-2163` 的 `previous_boundary`/`next_boundary` grapheme 语义 | native `resolve_grapheme_offset`、`snap_grapheme_offset` 及所有公开位置/fallback/vertical 入口。 |

本轮只改 native editor 实现与测试（另因历史原子边界需要扩展 native `history.rs`），没有修改 donor、计划/spec、验收矩阵或控制器 ledger。剩余顾虑仍是 headless 测试不能替代真机 IME 候选窗、主题字体/Metal/RSS 和异步图片解码验收。

## Fix round 2（合并 findings 闭环）

本轮先把审查要求转成生产入口回归，再修实现；没有以测试 helper 或直接 `Document::apply` 绕过 `EditorCore`。初始 RED 为 54/58（平台 IME 内部 caret、composition undo 粒度、硬换行 selection/caret bounds、changed-node 缓存失效与非零 viewport reflow）；结构边界 fixture、空硬行、16 MiB/selection reserve、图片 affinity、`register_exact` 和完整字体 key 也随后各自加入 RED。最终 focused native suite 为 63/63 GREEN。

### Round 2 findings 与实现/证据

| finding | 生产修复与回归 |
| --- | --- |
| 1. 平台 IME composition 与 undo | `EntityInputHandler::replace_and_mark_text_in_range`、`replace_text_in_range` 经过 `EditorCore` 的 provisional `History::undo_with_outcome`/最终事务；marked range 与内部 selected range 分离，commit 只产生一个 undo entry。`entity_input_platform_commit_replaces_candidate_as_one_undo`、`entity_input_explicit_commit_range_and_navigation_cancel_composition` 通过。 |
| 2. 非零 viewport reflow | `LayoutRegistry::reflow_visible` 以 `document_order[node_id]` 定位，而不是把 visible slice index 当全篇 index；`nonzero_viewport_reflow_keeps_document_block_index` 真实 `shape_text` 后通过。 |
| 3. hard-line selection/caret geometry | `range_segment_bounds` 累加此前 `WrappedLine` 的真实高度；IME collapsed range 走 `caret_bounds_for_point`；硬换行 selection y 与 caret height 回归通过。 |
| 4. image affinity contraction | `EditorCore::doc_point_key` 与 `LayoutRegistry::point_key` 都保留 `Before < After`；纯 caret 不绘制 atom selection，反向 affinity range 才绘制 image outline。`editor_core_orders_same_image_affinity_for_contraction` 通过。 |
| 5. changed-node cache invalidation/扫描 | 事务 outcome 的 `changed_nodes` 只删除受影响 shaped entry；`Document::revision()`、宽度和 order revision 复用估算/高度/order 索引，静态 viewport 不重复扫描全文。`editor_core_invalidates_only_changed_shaped_node` 与 10,000 块边界回归通过。 |
| 6. 真实 hard cache budget | `estimate_cache_bytes` 计入 `CachedBlockLayout`/`BlockLayout`、`WrappedLine`、`Arc` 所属 layout payload、text、runs/glyph capacity、wrap boundaries、外层 line capacity 和 selection row reserve；每次插入/精确注册后立即 enforce LRU，超 budget entry 不保留。`shaped_cache_budget_is_hard_during_multi_block_shaping`、16 MiB 和 10,000 块回归通过。 |
| 7. 空 hard line visual navigation | `wrapped_row_offsets` 对空 `WrappedLine` 返回 `[0, 0]`，保留一个视觉行；`visual_down_preserves_empty_hard_line_as_one_row` 通过。 |
| 8. structural edge semantics | 首块 Backspace 是 clean no-op；非空 list 起点 Backspace 经 `SetBlockKind` transaction 降级且保留 caret；异构 forward Delete 仍经 metadata transaction + merge。`structural_edges_preserve_caret_after_backspace_downgrade` 及原异构边界回归通过。 |
| 9. measured `register_exact` | API 要求调用方显式传入 `is_image` 与 measured `line_height`，不再从 `before == after` 猜图片；文本 36px、图片 42px 非默认高度回归通过。 |
| 10. complete shaping key | 新增生产 `LayoutRegistry::shape_visible_with_style(TextStyle, Window)`；`shape_visible_with_window` 委托该入口。`ShapeKey` 包含 block revision、width、完整 GPUI `Font`（family/features/fallbacks/weight/style）、font size、line height 和 wrap discriminator。字体 family/weight/style/size 分项改变均触发真实 reshaping 回归。 |

### Round 2 donor 复用映射

| donor 位置/算法 | round 2 native 适配 |
| --- | --- |
| `components/block/input.rs:116-179`、`runtime/mod.rs:1539-1615,1726-1747` 的 marked/composition/commit 协议 | `core.rs` 的 `MarkedText`、`composition_base`、`replace_and_mark_utf16`、`commit_marked_text_with_range`；事务与 `History` 保持单一撤销边界。 |
| `editor/history.rs:77-137` 与 `editor/tests.rs:2566-2668` 的 provisional undo/reapply 原子性 | `history.rs::undo_with_outcome`/`redo_with_outcome` 将 changed nodes 与 selection 一起返回，core 精确失效 layout。 |
| `components/block/element.rs:242-263,279-312,353-512,938-985` 的 `WrappedLine` 行高、range segment、closest-index | `layout.rs::ShapeKey`、`range_segment_bounds`、`wrapped_row_offsets`、`visual_move`/`visual_edge_point`；硬换行与空行共用真实 shaped line。 |
| `editor/selection.rs:364-493` 的全文端点/跨 viewport selection ordering | `core.rs::doc_point_key` 与 `layout.rs::point_key`；image Before/After affinity 和 offscreen anchor 均按全文顺序求交。 |
| donor image affinity/caret geometry | `layout.rs::image_side`、`caret_bounds_for_point`、`selection_rects`；register_exact 不再猜测 image kind。 |
| donor visual navigation `runtime/mod.rs:1879-2020`、`interactions.rs:663-704`、`events.rs:2167-2228` | `LayoutRegistry` 保留 preferred-x、空 hard row、跨 block edge row 的统一入口。 |
| donor structural boundary `interactions.rs:433-564`、`events.rs:1775-1817,2260-2335` | `core.rs` 通过 `TransactionBatch` 完成首块 no-op、list downgrade、异构 metadata merge，不直接改 `Document.blocks`。 |
| donor grapheme `runtime/mod.rs:2149-2163` | `resolve_grapheme_offset`/`snap_grapheme_offset` 覆盖公开 caret、vertical target、IME selected range 和 fallback hit。 |

Round 2 仍只修改 native editor 文件与本报告；没有修改 donor、plan/spec、验收矩阵、ledger 或 findings。Headless focused/compile gates 通过不等于真机 IME、主题字体和 Metal/RSS 预算已经验收，这些仍保留为后续 UI 集成顾虑。

## Fix round 3（六项复审 findings）

本轮先按 findings 中的真实生产序列追加 RED，再改 `EditorCore`/`LayoutRegistry`。新增回归没有直接调用 `Document::apply` 绕过入口：IME 通过真实 `EntityInputHandler` entity callback，删除图片通过 `EditorCore::apply`，布局通过真实 `shape_visible_with_window`/`shape_visible_with_style` 和 viewport/cache 状态。

### RED → GREEN

- RED focused 首次运行：62 个 native tests 中 57 通过、5 个失败，失败正对应首块 Backspace、IME 原始 selection 二次映射、显式 candidate range、测量后成员重算、删除图片后的旧几何；多块预算用例也先证明了各项可单独 admission。加入峰值观测断言后，缺失的 `peak_accounted_bytes` API 先以编译 RED 暴露，再接入生产 admission。
- GREEN focused：

```text
cargo test --manifest-path packages/app-lite-gpui/Cargo.toml \
  --bin velotype native_editor::tests:: --offline
62 passed, 0 failed
```

### Findings 对照

| finding | 生产修复与真实回归 |
| --- | --- |
| 1. 首块 Backspace | `backspace` 先处理文本 grapheme 或 image affinity，再仅对首块 offset 0 做 no-op；`first_block_backspace_deletes_text_but_offset_zero_is_noop` 覆盖 `abc@3 → ab` 和首块 `@0` 不变。 |
| 2. IME 原始 base selection 被二次映射 | `EditorCore` 增加文档级 `composition_base_range`，保存候选插入前的 flat UTF-8 range；候选/commit range 才经过 `remap_candidate_selection_after_undo`，原始 base 不再按候选坐标再减一次。真实 entity 序列 `abcd` 选 `bc`、mark `X`、commit `Y` 得到 `aYd`，一次 undo 恢复 `abcd` 与原 selection。 |
| 3. 显式 candidate 坐标和失败原子 | candidate UTF-16 range 先在候选文档解析，再按 marked span/base range 映射到 undo 后的 clone；`Document` 与 `History` clone 先完整试运行 undo+replace，成功后才提交，错误不会消费 provisional entry。collapsed 内部 selection 两端共用同一 grapheme boundary；真实 `ab → 候选 → range(1..3) 更新` 与 invalid range 保持状态/marked/history 不变回归通过。 |
| 4. measured reflow 成员集合 | 从 donor `WrappedLine` 实测高度更新 prefix 后，`rebuild_visible_window` 重新计算 viewport/prefetch、驱逐 moved-out cache/geometry，并以有界 fixed-point pass shape/register newly entering blocks。测试用 100px expansion→26px contraction，校验 final membership、new IDs 的 shaped lines、无 tall 状态外残留。 |
| 5. invalidated geometry | `invalidate_nodes` 同步从 `visible` 移除 changed/deleted NodeId，再清 cache/LRU；`point_to_doc` 的 before/after fallback 只适用于仍然有效的 unshaped geometry。真实 text-image-text shape→`EditorCore::apply(RemoveNode)`→next layout 前旧图片坐标查询不再返回已删 NodeId。 |
| 6. hard cache budget | `insert_shaped` 在 cache ownership 前按 `CachedBlockLayout`、`BlockLayout`、WrappedLine 外层 capacity、text/wrap Arc header、runs/glyph capacity、selection per-row reserve 计算 conservative cost，并先 LRU 腾挪空间；`used_bytes` admission 后始终不超 budget，`peak_accounted_bytes` 记录 post-admission high-water。预算回归用一个 64KiB budget：单块可放下，多块总和超过 budget，验证仍有 admission、发生 eviction 且 peak/used 均不超限。 |

### Round 3 donor/GPUI 复用映射

| donor 算法/路径 | native 适配 |
| --- | --- |
| `components/block/input.rs:116-179` 与 `runtime/mod.rs:1539-1615,1726-1747` 的 marked/commit provisional composition | `core.rs::composition_base_range`、全文 flat UTF-8 mapping、`remap_candidate_selection_after_undo`；clone 的 native `History` 试运行保持 GPUI callback 的失败原子语义。 |
| `runtime/mod.rs:2149-2163` 的 grapheme previous-boundary/backspace 入口 | `core.rs::backspace` 将首块 guard 放到 grapheme deletion 之后，继续沿用 `previous_grapheme_boundary`。 |
| `components/block/element.rs:242-263,279-312,353-512` 的 WrappedLine 实测高度与行几何 | `layout.rs::shape_visible_with_style` 的 bounded fixed-point membership rebuild、prefix reflow、cache eviction；仍使用 GPUI `shape_text(...).into_vec()`/`WrappedLine`。 |
| donor layout/cache 的可见窗口与保留成本边界 | `LayoutRegistry::rebuild_visible_window`、`make_room_for`、`estimate_cache_bytes`、`selection_geometry_reserve`、`peak_accounted_bytes`；visible 仍只保存 geometry，shaped lines 只在 cache 单一所有权中保留。 |

本轮仍只修改 `packages/app-lite-gpui/src/native_editor/{core.rs,layout.rs,tests.rs}` 与本报告；没有修改 donor、plan/spec、验收矩阵、控制器 ledger 或 findings。headless GREEN 仍不替代真机 IME 候选窗、主题字体和 Metal/RSS 预算验收。

## Fix round 4/5（六项复审 findings）

本轮严格先走真实生产入口 RED，再改 native 实现。新增回归均通过
`EntityInputHandler`、`EditorCore` 和 `LayoutRegistry` 的公开生产序列，未以
`Document::apply` 或测试专用替代路径绕过输入、历史、缓存或视口行为。

### RED → GREEN

基线 `e789f250f` 的精确回归先得到 62 个既有 native 测试通过、6 个新增测试失败：

| 测试 | RED 暴露的问题 | GREEN 修复/证据 |
| --- | --- | --- |
| `entity_input_explicit_candidate_range_updates_composition_base` | 显式候选范围更新后仍沿用旧 replacement base，最终得到 `Zabcd` 而非 `Zbcd` | `replace_last_with` 在恢复 provisional inverse 后重新映射 candidate/base flat offsets，并保存实际 `composition_base_range`；真实序列最终 `Zbcd`，一次 Undo 恢复原文档/selection。 |
| `entity_input_commit_restores_reverse_selection_affinities` | Undo 只恢复 normalized forward range，丢失 reverse anchor/head 与 affinity | History replacement 使用原始 `composition_base` 作为 `before_selection`；Undo 精确恢复方向与两端 affinity。 |
| `entity_input_marked_endpoints_use_final_grapheme_boundaries` | 插入后按候选字符串算 endpoint，组合字符相邻时发布非法 byte caret/marked range | marked、selected、collapsed caret 均在最终文档 block text 上经 `resolve_grapheme_offset`；`a + U+0301` 回归通过。 |
| `measured_reflow_shapes_every_final_member_without_fixed_pass_hole` | 固定 8 轮 reflow 后第九个最终成员进入窗口但没有 shaped lines | `shape_visible_with_style` 改为按 membership 稳定性循环，真实 `WrappedLine` 测量后继续 shape 新成员；最终 viewport/prefetch 成员同一调用均有 cache layout。 |
| `shaped_cache_budget_accounts_dynamic_selection_geometry_scratch` | wrap capacity 和同时存在的多组 selection geometry 未计入 admission/peak | `estimate_cache_bytes` 按 capacity 保守计 wrap storage，selection reserve 计三组同时存活的 bounds scratch；cache admission 前腾挪，`used_bytes` 与 observed peak 均不超 budget。 |
| `entity_input_repeated_candidates_do_not_clone_document_or_history` | 每次候选更新深拷贝 20,000 块文档/历史，分配明显随全文增长 | `History::replace_last_with` + `Document::replace_after_inverse` 只回放局部 inverse/replacement；无 `Document`/`History` clone。大文档真实 EntityInputHandler 重复候选保持 20,000 blocks、仅目标 block revision 变化、undo depth 1，分配门槛通过。 |

随后把热路径中逆 `RestoreBlocks` 的整篇 `HashSet` 分配去掉：已拥有的 replacement
块直接交给 `splice`，局部同 ID inverse 只做替换范围 pairwise 校验；只有引入外部
ID 的结构性恢复才扫描 outside range。`apply_transaction` 仅校验 changed nodes，普通
`apply_batch` 仍在批边界执行完整 `validate_invariants`，所以没有削弱普通事务的全局
审计。结构恢复仍保留 block-slice 校验、replacement 内重复校验与 outside ID 冲突校验。

最终 focused native suite：

```text
cargo test --manifest-path packages/app-lite-gpui/Cargo.toml \
  --bin velotype native_editor::tests:: --offline
68 passed, 0 failed
```

### Round 4 donor/GPUI 复用映射

| donor/既有 native 算法 | round 4 native 适配 |
| --- | --- |
| `components/block/input.rs:116-179`、`runtime/mod.rs:1539-1615,1726-1747` 的 marked/candidate/commit 协议 | `core.rs::replace_and_mark_utf16` 与 `commit_marked_text_with_range` 保持 marked range、candidate internal selection、composition base 分离；候选更新和 commit 走统一 `EntityInputHandler` 入口。 |
| `editor/history.rs:77-137`、`editor/mod.rs:295-298` 的 provisional undo/reapply 与单条 history 语义 | native `History::replace_last_with`、`Document::replace_after_inverse` 用局部 inverse journal 原子恢复，再替换同一 history entry；失败时恢复 revision、next id、selection、marked 前状态。 |
| `editor/selection.rs:1031-1052` 的全文 endpoint 映射与 donor grapheme boundary | `flat_offset_for_point_in`、`remap_candidate_selection_after_inverse` 保留 affinity；最终 block text 上复用 `resolve_grapheme_offset`，不在 candidate string 内提前截断。 |
| `components/block/element.rs:242-263,279-312,353-512,938-985` 的真实 `shape_text`/`WrappedLine` 及 measured reflow | `LayoutRegistry::shape_visible_with_style` 循环至 membership 稳定，cache 只保存实际 `WrappedLine` layout，不留固定 8 轮洞。 |
| donor cache/geometry 的 capacity 与 selection segment 约束 | `estimate_cache_bytes`、`selection_geometry_reserve`、`insert_shaped` admission/peak accounting；wrap boundary capacity 与三组同时存活 geometry 均进入硬预算。 |

Round 4/5 仍只修改 `packages/app-lite-gpui/src/native_editor/` 下的实现/测试与本报告；
没有修改 donor、plan/spec、验收矩阵、控制器 ledger 或 findings。Headless GREEN 仍不
替代真机候选窗、主题字体、Metal/RSS 和异步图片解码验收。

## Fix round 5/5（三项最终复审 findings）

本轮基线为 934ac18a9。先把三个 production-path 回归置为 RED，再按 donor/GPUI
已有协议修复；没有通过测试 helper、直接 Document::apply 或测试专用布局路径绕过
EntityInputHandler、EditorCore、LayoutRegistry。

### RED -> GREEN

初始精确回归为 76 个 native tests 中 73 个通过、3 个失败：

| 测试 | RED 暴露的问题 | GREEN 修复/证据 |
| --- | --- | --- |
| entity_input_marked_endpoints_keep_actual_candidate_interval | 组合标记相邻时把公开 grapheme span 当作候选实际区间，第二次候选/commit 顺序错误（左侧出现 combining mark）； | MarkedText 显式保存 public utf8_range 与 internal actual_utf8_range；候选更新、commit、inverse remap 只用 actual interval，公开 range 仍在最终文档上吸附 grapheme；二次候选、commit、一次 Undo 和原始 caret 均通过。 |
| measured_reflow_uses_incremental_height_sum_tree_for_large_document | 20,000 块在收敛波次反复重建全文前缀，index work 为 280,000（后续旧 keyed summary 仍为 20,014）； | 直接接入 gpui_sum_tree 0.2.2 的 Item/Summary/Dimension/SeekTarget/insert_or_replace；结构/宽度变化才全量建树，测量高度只更新单个路径，最终成员均已 shape，20k work 增量为有界局部值。 |
| selection_geometry_real_path_peak_covers_allocator | near-budget 长段在旧 scratch 组合下要么无法 admission，要么真实 selection_rects 分配峰值超过 reserve； | 删除 joined text、hard-line range Vec 和 per-line row-offset Vec；先按选中可见 wrapped rows 预留唯一最终 Bounds Vec，再流式写入。计数 allocator 改为当前测试线程的 scoped Cell，真实 selection 调用的 observed allocation 由 reserve + 64 KiB margin 覆盖，16 MiB accounted used/peak 仍不超。 |

最终 focused native suite：

    cargo test --manifest-path packages/app-lite-gpui/Cargo.toml \
      --bin velotype native_editor --offline -- --nocapture
    76 passed, 0 failed

### 三项 finding 对照

1. IME 公开/内部坐标分离：MarkedText::utf8_range 是平台可见、grapheme-safe
   marked span；actual_utf8_range 是候选文档中真实插入字节区间。
   candidate_selection_for_marked_range 只有在 public span 因组合字符扩大时才将平台范围
   约束到 actual interval，保留 donor 对普通显式 candidate range 的语义。这样
   "a" + combining mark + 第二次候选 + "b" 得到 "ab"，一次 Undo 恢复 "a" 和原方向/affinity
   selection。
2. 增量高度索引：LayoutRegistry::height_tree 以文档序 block 为 item，HeightSummary
   同时维护 count/height/max index；viewport seek 使用 tree find，实测高度使用
   insert_or_replace。height_index_work_count 的 20k 回归证明第二次 style/viewport
   shape 不按全文块数乘收敛波次增长，最终 visible/prefetch 集合中的每个 block 都有真实
   shaped layout。
3. 几何 scratch 与硬预算：selection_rects 两遍扫描可见布局，第一遍按实际 wrapped
   row 上界一次性分配唯一输出 Vec，第二遍调用无分配的
   append_range_segment_bounds/for_each_wrapped_row；hard-line 起点和行高在借用的
   WrappedLine 上累计。selection_geometry_reserve 只计最终 Bounds capacity 和 Vec
   header；回归通过真实 near-budget admission、真实 selection/caret geometry 和 scoped
   allocator 观测，而不是复算实现公式。

### Round 5 donor/GPUI 复用映射

| donor/GPUI 算法或路径 | round 5 native 适配 |
| --- | --- |
| components/block/input.rs:116-179、runtime/mod.rs:1539-1615,1726-1747 的 marked/candidate/commit 协议 | core.rs::MarkedText、candidate_selection_for_marked_range、replace_and_mark_utf16、commit_marked_text_with_range；公开 marked span 和内部 actual candidate interval 分开，失败仍由现有 transaction/history 原子边界处理。 |
| editor/selection.rs:1031-1052 的全文 flat endpoint/affinity 与 donor grapheme boundary | actual_marked_flat_range、selection_flat_range 和最终文档 grapheme resolution；不从 public expanded span 反推候选 inverse 坐标。 |
| GPUI gpui-0.2.2/src/elements/list.rs:1112-1184 的 Item/Summary/Count/Height/seek/splice 模式；底层 gpui_sum_tree-0.2.2/src/sum_tree.rs | layout.rs::HeightItem/HeightSummary/Count/HeightTarget/CountTarget/SumTree；Cargo 以 sum_tree = { package = "gpui_sum_tree", version = "0.2.2" } 直接声明可见依赖，并锁定到 donor 已有 0.2.2。 |
| components/block/element.rs:242-263,279-312,353-512,938-985 的 shape_text、WrappedLine measured height、position/closest/range segment | shape_visible_with_style 保留 shape_text(...).into_vec() 和真实 line height；line_start_offset/line_position_for_offset/for_each_wrapped_row/append_range_segment_bounds 直接消费借用的 wrapped lines，不创建 joined text 或 hard-line ranges。 |
| donor selection range geometry 与 GPUI Bounds capacity 语义 | selection_rects 先计算 wrapped-row capacity 后填充单一 Vec；selection_geometry_reserve 与 near-budget allocator regression 共同守住最终 output allocation，线程本地 measurement 避免并发测试互相污染。 |

### Round 5 验收命令

    cargo fmt --manifest-path packages/app-lite-gpui/Cargo.toml --all -- --check
    PASS

    cargo check --manifest-path packages/app-lite-gpui/Cargo.toml --bin velotype --offline
    PASS

    cargo test --manifest-path packages/app-lite-gpui/Cargo.toml --all-targets --no-run --offline
    PASS

    cargo test --manifest-path packages/app-lite-gpui/Cargo.toml --bin velotype --offline \
      --skip editor::selection::tests::cross_block_cut_writes_markdown_deletes_range_and_undo_restores
    827 passed, 0 failed, 1 filtered

    git diff --check
    PASS

Round 5 修改范围为 native editor 实现/测试、本报告，以及为直接复用 GPUI sum tree
所需的 packages/app-lite-gpui/Cargo.toml/Cargo.lock；没有修改 donor、plan/spec、
验收矩阵、控制器 ledger 或 findings。剩余顾虑仍是 headless 测试不能替代真机 IME
候选窗、主题字体/Metal/RSS、异步图片解码和真实 UI 外壳集成验收。

## Task 4R 边界恢复（2026-09-09）

本轮严格按 brief 先写生产入口回归并运行 RED，再修改实现。四个新增测试均通过
真实 `EntityInputHandler`、`EditorCore`、`LayoutRegistry` 和 GPUI shape/window 路径；
没有通过 `Document::apply` 或测试专用布局路径绕过输入、历史、缓存、视口或预算。

### RED -> GREEN

基线 `6f794cfa3` 加入四个回归后先运行：

    cargo test --manifest-path packages/app-lite-gpui/Cargo.toml native_editor --offline -- --nocapture
    76 passed, 4 failed

| 测试 | RED 暴露的问题 | GREEN 修复/证据 |
| --- | --- | --- |
| `entity_input_actual_span_survives_right_grapheme_join` | 从执行后 caret 反推插入长度，把 `b` 放到右侧 combining mark 后，结果为 `\u{301}b` | transaction 在执行前记录精确 `InsertedTextSpan`，`ApplyOutcome` 贯穿 batch/model/history；marked span 从真实插入区间生成，结果为 `b\u{301}`，一次 Undo 恢复原文/selection。 |
| `entity_input_explicit_replacement_outside_expanded_public_mark` | 把显式 UTF-16 replacement range 错误夹到 expanded public marked span 内，`aQ` 的 `2..3` 替换后错误保留 `Q` | 明确区分 public marked range 与实际 candidate interval；真实平台 replacement range 优先，只有完整覆盖 expanded public span 的既有 seam 才回到 actual interval，结果为 `ab`，一次 Undo 恢复。 |
| `empty_hard_lines_hit_test_by_y` | 全空 hard lines 走 x fallback，第二、三行 y 命中都回到 offset 0 | `point_to_doc` 按每个 `WrappedLine` 的 y 和 hard-line 累计 offset 命中空行；`"\\n\\n"` 三行分别发布 0、1、2。 |
| `crlf_visual_end_never_publishes_invalid_grapheme_offset` | visual end 直接发布 CRLF 中间 byte offset 2 | layout point 在最终全文 grapheme 边界上吸附，并把 CRLF 中间点调整到合法 affinity；move_end/insert 不再发布 InvalidGraphemeOffset。 |

最终 focused native suite：

    cargo test --manifest-path packages/app-lite-gpui/Cargo.toml native_editor --offline -- --nocapture
    80 passed, 0 failed, 752 filtered out

四个新增 focused tests 也逐项独立运行并全部通过。完整 bin 验收（保留 brief 指定
的既有单项 skip）为：

    cargo test --manifest-path packages/app-lite-gpui/Cargo.toml --bin velotype --offline \
      -- --skip editor::selection::tests::cross_block_cut_writes_markdown_deletes_range_and_undo_restores
    831 passed, 0 failed, 1 filtered

### Task 4R donor/GPUI 复用映射

| donor/GPUI 路径 | native 复用/适配 |
| --- | --- |
| `components/block/input.rs` 的 `EntityInputHandler` replacement/marked/selection 协议 | `core.rs::replace_and_mark_utf16` 保留 replacement range 优先；公开 grapheme-safe range 与 actual candidate interval 分离，commit/candidate 更新仍走同一生产入口。 |
| GPUI 0.2.2 `examples/input.rs:300-349` | 复用“replacement range 优先于 marked range”以及 marked span 从执行前 `range.start..range.start + new_text.len()` 生成的语义；不从执行后 caret 反推。 |
| macOS `platform/mac/window.rs:2234-2272` | 复用平台 bridge 透传真实文档 `replacementRange` 的边界定义；显式范围不再被 public IME grapheme 扩张吞掉。 |
| `components/block/element.rs:243-312` 的 `hard_line_ranges`、`line_index_for_offset`、`wrapped_line_for_y` | `layout.rs::point_to_doc` 采用同一 hard-line/y 语义，空行也按实际行高和累计 offset 命中；不拼接全文字符串。 |
| native 既有 `gpui_sum_tree 0.2.2` `HeightItem`/`HeightSummary`/`SeekTarget`/`insert_or_replace`、selection streaming 与 scoped allocator gate | 本轮只在 transaction/IME 边界和 layout point 最终 grapheme 吸附处接入；sum-tree、两遍 selection geometry、硬 cache/selection 预算均未削弱或重写。 |

### Task 4R 验收命令

    cargo check --manifest-path packages/app-lite-gpui/Cargo.toml --bin velotype --offline
    PASS

    cargo test --manifest-path packages/app-lite-gpui/Cargo.toml --all-targets --no-run --offline
    PASS

    cargo fmt --manifest-path packages/app-lite-gpui/Cargo.toml --all -- --check
    PASS

    git diff --check
    PASS

修改仅涉及 native editor 实现/测试和本报告，没有开始 Task 5，也没有修改 brief、
findings、验收矩阵、donor 或既有预算/sum-tree/selection streaming gate。已知限制仍
与前轮相同：headless GREEN 不能替代真实 macOS IME 候选窗、主题字体、Metal/RSS、
异步图片解码和真实 UI 外壳集成验收。

## Task 4R fix round 1（2026-09-09）

针对独立审查的两个 Important findings，先在当前 `7e348b48d` 基线上加入生产路径
回归并运行 RED，再作最小修复；没有修改已批准的 sum-tree、selection streaming、
allocator/cache budget，也没有开始 Task 5。

### RED -> GREEN

| 测试 | RED 暴露的问题 | GREEN 修复/契约 |
| --- | --- | --- |
| `entity_input_explicit_replacement_superset_stays_explicit` | `Some(0..3)` 严格 superset 被 containment 规则误判为 public marked span，结果为 `abQ` 而非 `b` | 只有 explicit range 与 expanded public marked range **精确相等**时才映射到 actual candidate interval；strict superset/subset/overlap/disjoint 均保留真实文档坐标。结果为 `b`，一次 Undo 恢复 `aQ` 与原 selection。 |
| `apply_batch_insert_then_delete_clears_inserted_span` | insert 后 delete 的 batch 仍发布已删除区间 | `apply_batch` 每一步都覆盖 `inserted_span`，final Delete 的 None 清除 earlier span。 |
| `apply_batch_insert_then_remove_clears_removed_inserted_span` | insert 后 RemoveNode 的 batch 仍发布已不存在 NodeId | 同一 final-operation 契约；RemoveNode 的 None 不会传播已删除 node 的 span。 |
| `apply_batch_final_insert_reports_final_inserted_span` | 守住 batch 最终 InsertText 的 exact span 语义 | final InsertText 返回最后一次执行时记录的 node/range，回归确认 `x` 后插入 `y` 返回 `node, 1..2`。 |

RED focused 结果为 superset `1 failed`，batch 套件 `1 passed / 2 failed`；失败均对应
finding，未以放宽断言或绕过 production path 处理。

修复后的 focused 结果：

    cargo test --manifest-path packages/app-lite-gpui/Cargo.toml native_editor --offline -- --nocapture
    84 passed, 0 failed, 752 filtered out

### ApplyOutcome batch contract

`ApplyOutcome.inserted_span` 现在明确表示“仅当 batch 的最后一项 transaction 是
`InsertText` 时，才描述该最后操作在最终文档中的精确写入区间；否则为 `None`”。
这样不会尝试把 earlier InsertText 的 span 通过任意后续删除、移除节点或结构替换
重映射，也不会向 composition caller 发布 stale NodeId/range；单项 `Document::apply`
继续得到相同的 InsertText exact span。

### Fix round 1 验收命令

    cargo check --manifest-path packages/app-lite-gpui/Cargo.toml --bin velotype --offline
    PASS

    cargo test --manifest-path packages/app-lite-gpui/Cargo.toml --all-targets --no-run --offline
    PASS

    cargo test --manifest-path packages/app-lite-gpui/Cargo.toml --bin velotype --offline \
      --skip editor::selection::tests::cross_block_cut_writes_markdown_deletes_range_and_undo_restores
    835 passed, 0 failed, 1 filtered

    cargo fmt --manifest-path packages/app-lite-gpui/Cargo.toml --all -- --check
    PASS

    git diff --check
    PASS

Donor/GPUI 复用映射和已知 headless 限制沿用 Task 4R 主报告：本轮只收紧
`candidate_selection_for_marked_range` 的 explicit precedence，并落实
`apply_batch` 的 final-operation span 契约；没有回退或弱化既有 donor gate、sum-tree、
selection streaming、预算或测试。

## Task 4R fix round 2（2026-09-09）

针对独立审查指出的 raw UTF-16 endpoint 丢失问题，先补 subset/overlap production
回归并运行 RED，再按 raw-coordinate 契约修复；没有修改 Task 4R 已批准的
`inserted_span`、sum-tree、selection streaming、allocator、layout 或 cache-budget 路径，
也没有开始 Task 5。

### RED -> GREEN

在共同生产前缀（`aQ`、caret byte 1、combining acute）下新增：

| UTF-16 range | 关系 | RED | GREEN |
| --- | --- | --- | --- |
| `0..2` | exact expanded public span | 既有 exact seam 回归保留 | `abQ` |
| `0..3` | strict superset | 既有 round-1 回归保留 | `b` |
| `0..1` | strict subset | 错误得到 `abQ`，应为 `bQ` | `bQ` |
| `1..3` | overlap | 错误得到 `b`，应为 `ab` | `ab` |
| `2..3` | disjoint | 既有 disjoint 回归保留 | `ab` |

RED 结果：`entity_input_explicit_replacement_subset_stays_explicit` 与
`entity_input_explicit_replacement_overlap_stays_explicit` 各自独立运行均失败，
分别暴露 `abQ`/`b` 的错误结果。修复后五种关系的生产 EntityInputHandler 回归全部
通过，且每个 case 的一次 Undo 都恢复原始 `aQ` 与原始 selection。

### Raw endpoint contract

新增 typed `RawDocumentRange`，明确区分平台 raw UTF-8 flat coordinates 与合法模型
`DocPoint`：

1. UTF-16 range 在 `replace_and_mark_utf16`/commit 入口只转换一次为 raw UTF-8 range，
   并复用同一值计算可见 selection 与 candidate mapping。
2. raw range 直接和 raw public marked range 比较；只有 exact expanded-public seam 才
   使用 actual candidate interval。
3. subset、overlap、superset、disjoint 等其他 explicit range 保持 raw endpoint/affinity，
   先经 candidate-to-base inverse mapping，再在恢复的 base document 上构造并吸附合法
   `DocPoint`。
4. 新增 `native_editor::core::raw_document_range_tests`，用 CJK 邻接 surrogate pair
   与 combining endpoint 验证 UTF-16→raw 边界未提前 grapheme-expand；该 pure mapping
   test 通过。

### Fix round 2 验收命令

    cargo test --manifest-path packages/app-lite-gpui/Cargo.toml native_editor --offline -- --nocapture
    87 passed, 0 failed, 752 filtered out

    cargo check --manifest-path packages/app-lite-gpui/Cargo.toml --bin velotype --offline
    PASS

    cargo test --manifest-path packages/app-lite-gpui/Cargo.toml --all-targets --no-run --offline
    PASS

    cargo test --manifest-path packages/app-lite-gpui/Cargo.toml --bin velotype --offline \
      --skip editor::selection::tests::cross_block_cut_writes_markdown_deletes_range_and_undo_restores
    838 passed, 0 failed, 1 filtered

    cargo fmt --manifest-path packages/app-lite-gpui/Cargo.toml --all -- --check
    PASS

    git diff --check
    PASS

本轮 donor/GPUI 复用映射仍是 input UTF-16 conversion、GPUI replacement precedence、
macOS bridge 真实 replacementRange 与 donor grapheme/hard-line 语义；新增 raw helper
只守住原始 endpoint 到 inverse mapping 的生命周期，没有更改已批准的 geometry、
sum-tree、selection streaming、预算或缓存行为。已知限制仍为 headless 测试不能替代
真实 macOS IME 候选窗、主题字体、Metal/RSS、异步图片解码及真实 UI 外壳集成验收。
