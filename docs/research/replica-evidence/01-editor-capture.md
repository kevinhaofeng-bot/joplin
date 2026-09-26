# 01 现有编辑与捕获体验核验

- 任务：任务1
- 实现提交：`20c5314df`（基于 `ffc16f1ae`）
- 测试构建 SHA：Release `velotype` sha256 `004c31467f33e267c1a3ae602885e5f7b0321b1f5bdcbb5b37d38eefbf04342e`（`20c5314df`）

## 读过的 Evernote 源码

根：`/Users/kevinhao/Projects/joplin-reconstruction/evernote-11.32.5/common-editor-sourcemap/@evernote/common-editor/src/apps/peso/modules/`

| 文件 | SHA256 | 符号 / 行段 |
| --- | --- | --- |
| `list/commands/insertlist.ts` | `afcf886c…c472a9` | `execCommand` 全文 1-104：命令名→列表类型，iOS 组合输入保存 |
| `list/list.ts` | `9b422fc3…9c28b0` | `insertListAtSelection` 37-60、`insertOrToggleList` 62-100、`removeList` |
| `title/title.ts` | `666f6bfb…987d7a` | `formatTitle`（控制字符→空格+trim）、`EDAM_NOTE_TITLE_LEN_MAX=255` |
| `textbetweenblocks/plugin.ts` | `8d2c906d…acab9` | `isSectionLevelBlockNode`、`insertOrFocusParagraph`、死区点击 `handleClickOn/handleClick` |
| `content/changesplugin.ts` | `c94b4215…87d7a` | 仅定位 `shouldSkipChange` 元数据用途 |
| `clipboard/plugin.ts`、`clipboard/commands/paste.ts` | `5eb67f22…cbe7b`、`b429185d…b0789` | 已哈希，本轮未改粘贴逻辑 |
| `noteendparagraph/plugin.ts`、`list/keymap.ts`、`resource/image/imagecomponent.tsx` | `3294d230…a7cde`、`fce3c127…37f`、`91637400…b95` | 已哈希，本轮无对应失败 |

## 行为对照

输入（点击 UL/OL/清单）→ 分支：`getListTypeAtRange` 与目标类型相同 → `removeList`（转回段落）；否则 `insertOrToggleList`：段落逐个包成列表项，已有列表项 `setNodeMarkup` 换类型、保留嵌套 → 一个 transaction（一次撤销）→ 用户可见：再次点击已高亮的列表按钮即取消列表。

我们的对应：`native_editor/commands.rs::apply_list_command`，经新增 `EditorCore::apply_batch_keeping_selection` 逐块 `SetBlockKind`，一次撤销，选区不变。

**修复前实际失败（均先 RED）：**

1. 列表按钮高亮（`toggle=On`）但 `enabled=false`，执行也只是再设同一类型 → 无法取消列表。
2. UL→OL/清单一律写成 `depth: 0`，嵌套被压平。

**有意差异：**清单→清单外的类型转换不保留勾选状态以外的属性；混合选择（部分为目标列表）按 Evernote 规则统一转换，不取消；图片等非文本块在选区内时命令仍禁用（沿用原状态逻辑，Evernote 会把资源包进列表项，本产品图片不属于列表项，属独立设计）。

## 测试

| 名称 | 修复前 | 修复后 |
| --- | --- | --- |
| `native_editor::tests::same_list_command_on_matching_list_selection_toggles_back_to_paragraphs` | FAIL：`[BulletItem{0},BulletItem{0}] != [Paragraph,Paragraph]` | ok |
| `native_editor::tests::switching_list_type_preserves_item_depth` | FAIL：`OrderedItem{depth:0}` ≠ `{depth:1}` | ok |
| `app::image_flow_tests::mounted_picker_completion_after_switching_notes_inserts_nowhere`（Review Focus 1：异步选择器期间切笔记不串写、取消不留节点/资源行） | 首次即通过（既有行为） | ok |

变异核验：临时删除切换笔记时 `pending_resource_insert = None;`，该测试**仍通过**——插入 intent 绑定原 session，存在第二道防护；因此本测试证明行为安全，但不单独钉住那一行。

命令（`CARGO_TARGET_DIR=/Users/kevinhao/Projects/joplin/.shared-target`，`packages/app-lite-gpui`）：

```sh
cargo test --locked --bin velotype -- list          # 70 passed
cargo test --locked --bin velotype -- --test-threads=1   # 1329 passed, 0 failed
```

既有覆盖（未新增，直接引用）：中文 IME 组合/提交/候选（`native_editor::tests::ime_*`、`entity_input_*`、`ui::tests::mounted_external_event_keeps_marked_ime_session_alive_until_composition_resolves`、`mounted_ime_lifecycle_warning_survives_clean_ticks_until_commit_and_successful_flush`）、图片前后落点（`image_before_after_points_and_cross_image_ranges_use_document_transactions`）、格式切换保存重开（`mounted_library_chrome_format_manual_sync_switch_and_reopen_round_trips_canonical_html`）、中文标题正文重启（`chinese_title_and_body_entity_input_round_trip_after_restart`）。

## Release 实机

已构建 Release 并以隔离 profile `/tmp/joplin-lite-t1-accept/profile` 启动 `/tmp/joplin-lite-t1-accept/JoplinLiteT1Test.app`。**请求屏幕控制权限被用户拒绝**，未绕过、未重试。因此以下全部**未实机验证**：按钮逐个行为记录、选两段→UL→OL→撤销→重开、首图前/两图间/末图后中文输入、系统剪贴板粘贴与 Finder 拖放、组合输入中点工具栏、三篇笔记截图。需要用户授权屏幕控制或人工按上列步骤操作。

## 未过项 / 下一步

- 实机矩阵全部待做（上）。
- 按钮逐项行为记录只完成列表三项；H1-H3、标记、链接、缩进、对齐沿用既有单元测试，未逐项实机。

- Claude 实施状态：代码与测试已提交；实机未完成（权限被拒）
- Codex 验收状态：未验收
