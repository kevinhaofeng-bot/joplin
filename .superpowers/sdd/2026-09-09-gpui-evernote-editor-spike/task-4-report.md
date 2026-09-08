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
| Important-5 硬换行偏移 | `joined_line_text` 与 donor hard-line ranges 为每个 `WrappedLine` 插入换行 byte；命中、caret、range segment 共用同一 hard-line start。 |
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
| `components/block/element.rs:242-263,279-312,353-512,938-985` 的 hard-line、WrappedLine、position/closest/range segment | `native_editor/layout.rs` 的 `hard_line_ranges`、`wrapped_row_offsets`、`visual_move`、`visual_line_boundary`、`range_segment_bounds_for_line`、`point_to_doc`。 |
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
