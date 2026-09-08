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
| `components/block/element.rs` 的 `hard_line_ranges`、`line_index_for_offset`、`wrapped_line_for_y`、`position_for_offset`、`cursor_bounds_for_offset` | `layout.rs` 的行/光标几何和 `point_to_doc` | 按 native `BlockLayout` 和 GPUI `ShapedLine` 改写；命中多行时累加此前行的 UTF-8 字节偏移。 |
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

cargo test --manifest-path packages/app-lite-gpui/Cargo.toml --bin velotype -- \
  --skip editor::selection::tests::cross_block_cut_writes_markdown_deletes_range_and_undo_restores
788 passed, 0 failed, 1 skipped

cargo fmt --manifest-path packages/app-lite-gpui/Cargo.toml --all -- --check
PASS

git diff --check
PASS
```

完整 donor bin 并发运行曾有一次既有的 `validate_styles_scans_graphemes_once_per_text_on_transactions` 计数型测试瞬时比例失败；该测试单独重跑通过，随后按简报要求的精确 skip 命令完整通过。代码没有放宽该断言，也没有修改 donor 或计划文件。

## 剩余顾虑

Task 4 只允许修改 native editor 文件，因此 `paint_entity` 已提供真实 GPUI entity/input bridge，但尚未接入后续应用外壳的 measured-layout element。生产真机 IME 候选窗、真实主题字体 shaping、异步图片解码和 RSS/Metal 预算仍需后续 UI/性能验收；本报告不把 headless GREEN 误报为真机完成。
