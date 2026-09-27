# 表格：canonical + 原生模型方案（草案，待 c7/Codex 确认后实施）

## 依据

Evernote 11.32.5 编辑器源码（`common-editor-sourcemap/@evernote/common-editor/src/apps/peso/modules/table/`）：

| 文件 | SHA256 | 行为 |
| --- | --- | --- |
| `schema.ts` | `8413c16b…92d9001` | `table` 内容为 `tr+`，`isolating`、`group: 'section'`；属性含宽度/背景色/`cols`（列宽）。`tr` 内容为 `(th \| td)*`。单元格内容 `tablecontent+`（块级），属性 `colspan`/`rowspan`（默认 1）、`colwidth`。 |
| `keymap.ts` | `c4430939…d9001` | Tab / Shift-Tab 在单元格间移动，末格 Tab 新增一行。 |
| `commands/` | — | 插入表格、增删行列、合并/拆分单元格、对齐、单元格颜色、等宽列、适应笔记宽度。 |

真实副本（c7 只读探针，81b996885）：76 篇笔记，共 290 张表。其中 1–2 列的表超过八成；231 张表不超过 5 行，3 张超过 100 行；对齐全部为 none；没有嵌套表格，也没有单元格内的块级列表。单元格里的内容有：`<br>` 803 处、链接 532、行内代码 78、图片 68、粗体 62。

## 首版范围（取舍）

做：
- canonical 表格块：若干行 × 若干单元格，第一行可以标为表头。单元格内容是行内序列（Text/marks/link、SoftBreak、Image）。`<br>` 就是 SoftBreak。
- 原生编辑：光标可以进入单元格并编辑文字、标记、链接；Tab / Shift-Tab 在单元格间移动，末格 Tab 新增一行；有命令可以插入表格、增删行列。
- 保存和重开都能逐字往返；复制出来是纯文本（单元格之间用 Tab，行之间用换行）。

暂不做（保留为已知边界，导入时不丢内容）：
- colspan/rowspan/合并单元格：真实副本里没有这类数据；解析时遇到则整表降级为现有的"保留原文"路径，并报告。
- 单元格内的块级内容（列表、标题、嵌套表格）：同上，降级并报告。
- 列宽、颜色、对齐：真实数据里没有；解析时忽略宽度，颜色和对齐不引入。
- 拖拽调列宽、单元格颜色面板。

## canonical（app-lite-core `document.rs`）

```rust
Block::Table { rows: Vec<TableRow>, header: bool }
pub struct TableRow { pub cells: Vec<TableCell> }
pub struct TableCell { pub inlines: Vec<Inline> }
```

- HTML 输出：`<table data-joplin-lite-table="true"><tbody><tr><th>…</th></tr><tr><td>…</td></tr></tbody></table>`；`header=true` 时第一行用 `th`。空单元格写 `<br data-joplin-lite-empty-item="true">`，沿用列表空项的惯例。
- 解析：`table > (thead|tbody|tfoot)? > tr > (th|td)`。满足下面任一条件就不生成 `Block::Table`，走现有的通用降级路径：有 colspan/rowspan ≠ 1、单元格内有块级元素、有嵌套 table。
- 行的列数不齐时，按最宽的行补空单元格；这一步只在规范化时做，并记为迁移警告。
- 搜索文本：单元格之间用 Tab，行之间用换行。`resource_ids` 包含单元格里的图片。
- 规模上限：每张表不超过 1000 行、64 列，超过的整表降级（防恶意输入）。

## 原生模型（app-lite-gpui）

有两种思路：

A. 表格作为一个原子 Block（`BlockKind::Table`，`BlockContent::Table { rows }`），单元格编辑用内部小光标状态：
   - 优点：不影响现有平坦块序列、列表编号、inline group。
   - 缺点：选区、IME、撤销、查找都要在表格内部再实现一套。

B. 单元格展开为平坦序列里的普通文本块，外加一张 table 侧表（与 inline_groups 同构）：`TableGroup { rows: Vec<Vec<NodeId>>, header }`；布局按网格排这些块：
   - 优点：光标、IME、marks、链接、图片（沿用组内原子图片）、撤销、查找全部复用现有文本块路径。Tab 只是移动到侧表中的下一个 NodeId。
   - 缺点：跨单元格选择和删除必须限制在单元格内（跨格的删除变成"清空所选单元格"，参照 Evernote 的 `isolating`）；布局要新增网格排版路径，并保证 SumTree 高度按行汇总。

建议选 B，理由：这个编辑器最难的是 IME、撤销、选区，这些已经在文本块上验证过，重写一套的风险远大于网格布局。两种方案的 canonical 层相同，所以 canonical 可以先落地并交给 c7 做导入映射，原生侧再分步实现：
1. 只读网格渲染：打开不丢内容，保存逐字回写，此时单元格不可编辑。
2. 单元格文本编辑，以及 Tab 导航。
3. 增删行列命令、插入表格。

每一步都先写失败测试：codec 往返、布局网格边界、IME 在单元格内提交、跨格删除被限制、>100 行表格的布局性能。

## 需要 c7 提供 / 确认

- 从迁移侧看，真实表格里有哪些"必须保真"的结构：表头行是 GFM 的第一行；单元格内图片是否都是 `:/resource`；单元格内是否有 HTML 实体、转义的管道符。
- 上面的降级条件在真实副本里是否为零（colspan/rowspan、块级内容）。
