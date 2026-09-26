# Task 8：可直接浏览的导出图文 HTML

基线：`5883f73ca`。本切片只改 `packages/app-lite-core/src/import_export/readable_export.rs` 和 `packages/app-lite-core/tests/export_restore.rs`；未改 GUI、编辑器、canonical serializer，也未接触个人笔记数据、提交或推送。

## 结果与边界

- 导出目录新增 `index.html`，标题经过 HTML 转义，链接指向 `readable/<note-id>.html`。
- 每条笔记新增独立 UTF-8 浏览页：内联最小 CSS，保留段落、粗体、列表、勾选列表、链接、行内/块图片及附件；仅块图片标记应用 `display:block`，行内图片保留行内排版。`data-align`、`data-indent` 与块图片 `data-joplin-lite-display-width` 转成浏览器可见排版，图片仍以 `max-width:100%` 适配窄屏。没有 JavaScript、远程资源或应用内 Web runtime。
- 使用已有 `html5ever` tokenizer 只处理真实 `img[src]` 与 `a[href]` 中的 `:/resource-id`，映射到 `../resources/...`；正文里长得像资源属性的字串、链接标签和其他 URL 不做全局替换。标题、正文文本与属性值写出时分别转义。缺失资源映射会让导出失败，临时目录不会发布。
- 浏览页逐条写入，资源仍以现有 64 KiB 缓冲复制和 SHA-256 核验；所有文件在原有临时目录中生成，最后统一目录重命名发布。
- `notes/<id>.html`、manifest v2、正文哈希、历史和资源身份不变。恢复继续只验证并读取 canonical `notes/`、`manifest.json`、`resources/`；`index.html` 与 `readable/` 即使被修改也不参与恢复。

## Evernote 源行为对照

| 本地重构源段 | 已观察行为 | 本实现关系 |
| --- | --- | --- |
| `26944__export-notes-into-html-action.js:337-365` | 生成 HTML index；目录项写链接并对标题调用 `escape` | 同样提供 index 与转义标题，但链接指向本实现的独立浏览页 |
| `18882__abstract-html-exporter-export-cancelled.js:71-99,111-142` | `resourceURLMap` 按资源 hash 对应相对本地路径；`outputStartFileData` 写 UTF-8 HTML | 使用 manifest 的资源 ID 到相对本地文件路径映射；输出 UTF-8；资源完整性仍由本实现现有 SHA-256 校验承担 |
| `12524__single-html-exporter.js:24-60` | 逐条提取笔记附件并调用内容导出 | 逐条生成派生笔记页，复用同一已核验资源文件；不采用 Evernote 的单文件结构 |

源文件位置：`/Users/kevinhao/Projects/joplin-reconstruction/evernote-11.32.5/main-readable/src/modules/`。Evernote 源行为只用于交叉核对；canonical/派生双文件及 fail-closed 恢复是本产品独立架构。

## RED / GREEN 与验证

- `readable_export_builds_escaped_browsable_pages_with_relative_resource_links`：RED 为 `index.html` 缺失；GREEN 覆盖中文及引号/HTML 标点标题、行内和块图片、PDF 相对 URL 指向原文件且 SHA-256 与 manifest 相符、粗体/列表/普通链接、正文中的字面 `src=":/..."`、canonical HTML 原样及浏览页损坏后恢复仍成功。
- `readable_page_applies_canonical_alignment_indent_and_image_display_width`：RED 为缺少 `data-align` CSS；GREEN 覆盖居中、右对齐、缩进 2/8（包括列表项）与 320px 图片显示宽度。
- 私有单元测试 `browser_projection_refuses_an_unmapped_resource_attribute` 验证缺失实际资源属性映射会失败，而字面文本已经写出且未改写。
- `cargo test --manifest-path packages/app-lite-core/Cargo.toml`：退出码 0，包含 `export_restore` 18/18；`cargo fmt --manifest-path packages/app-lite-core/Cargo.toml` 已执行。浏览器的实际视觉检查按简报留待下一步。

Review 修复轮 1：原页只有通用 `img{max-width:100%;height:auto}`，块图片紧接其他内容时不会独立占行。为 `readable_export_builds_escaped_browsable_pages_with_relative_resource_links` 增加行内图无块标记、块图片有块标记且 CSS 仅作用于块图的断言；RED 为缺少块图 CSS，加入 `img[data-joplin-lite-block-image="true"]{display:block}` 后 GREEN。重跑 `cargo test --manifest-path packages/app-lite-core/Cargo.toml --test export_restore`，18/18 通过；未动 canonical serializer。

现存 `packages/app-lite-gpui/Cargo.lock` 与 `progress.md` 的其他任务改动未处理。
