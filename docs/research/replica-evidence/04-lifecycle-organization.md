# 04 笔记生命周期、组织与浏览

- 任务：任务4
- 实现提交：`0b436b4d8`（永久删除与历史附件）、`308546e3a`（复制、多选、批量移动/加标签、菜单；并修复 `3d4f209e7` 引入的生产构建错误）
- 测试构建 SHA：`308546e3a`

## 读过的 Evernote 源码

| 文件 | SHA256 | 符号 / 行段 |
| --- | --- | --- |
| `main-readable/src/modules/93286__show-note-context-menu.js` | `1d2b0e0c…578573597` | 298-322：Move（`S.moveNotes({notes})`，回收站中禁用）、Copy（`showCopyNoteModal`）、Duplicate（`S.duplicateNote(id)`）只向渲染进程派发动作 |
| `renderer-readable/chunks/9435.js` | `236728fa…005d7d99e` | 约 963805 `getNotesMenuOptions`：在回收站 `moveNotesEnabled=false`、`copyNotesEnabled` 恒为 true、多选下编辑标签/移入回收站/恢复/永久删除；约 977352 多选操作栏 |

**证据缺口：**复制/副本的实际写入在 Conduit 服务端/同步层，本重建中不存在；本产品的复制语义（同标题、同正文、共享附件引用、复制标签、历史从新开始、默认落在原笔记本）为独立设计。回收站中的笔记不允许复制（与 Evernote 不同，避免从回收站生成活动笔记），记于 ledger。

## 实际修复的缺陷（先 RED）

| 回归 | 修复前 | 修复后 |
| --- | --- | --- |
| `organization_stage_c::purge_keeps_a_resource_that_another_notes_history_still_references` | FAIL：永久删除 A 后，B 的历史版本所引用的附件被回收 | ok |
| `organization_stage_c::purge_removes_the_notes_own_history_and_its_history_only_resource` | FAIL：永久删除后该笔记 2 条历史正文仍在库中 | ok |

实现：`purge_note` 先删除自身 `note_revisions`；候选资源包括自身历史引用的资源；仅当没有当前关系、也没有任何剩余历史正文（规范 `:/<id>`）引用时才回收。

## 新增功能

- 仓库：`copy_note`（单事务）、`add_tag_to_notes`（全成功或全回滚，已有标签的笔记不变）；`move_notes` 原已为单事务批量。
- 模型：Cmd-点击 `ToggleNoteInSelection`；原“移动/加标签（选中笔记）”动作作用于全部选中笔记，走同一 typed action 与仓库事务；普通点击、导航、新建、复制、回收站操作清空多选；组织修改保留多选（先加标签再移动可连用）。
- 界面：卡片 Cmd-点击多选并高亮；菜单“复制笔记”（先 flush 当前笔记，再复制并选中副本）。

测试：`organization_stage_c` 新增 4 项；`app::tests::cmd_click_multi_selection_…`、`copy_selected_note_…`；挂载 `mounted_copy_note_menu_action_…`。一次多选规则设计错误（组织修改后清空多选导致第二步只移动一篇）由该测试发现并已修正。

全套：core **274/274**；GUI **1338/1338**；`cargo check --bin velotype`（非测试构建）通过。

**过程缺陷记录：**`3d4f209e7` 的非测试代码缺少 `Path` 导入，测试构建未暴露；此后每次 GUI 改动增加生产 `cargo check`。

## 复核既有能力（未改）

删除标签不删笔记（`deleting_notebook_rehomes_notes_and_deleting_tag_removes_its_route_relation`）、原笔记本不存在时恢复回退（`organization_and_trash_changes_queue_search_and_restore_to_live_notebook`）、共享资源最后一次引用才回收（`permanent_purge_reclaims_only_the_last_resource_occurrence_…`）、三栏/两栏/一栏与列表模式/排序/前进后退/重启状态（`ui::navigation_retention_tests`、`ui::note_card_tests`、`app::tests` 相关用例）。回退是否有“明确可见”的提示未实机核对。

## Release 实机

未做（屏幕控制权限被拒）：批量操作后重开与过滤、选中状态不指向错误实体、抽拉动画与减少动态效果、缩略图，均无实机证据。

## 未解决

- 多选只支持 Cmd-点击，未做 Shift 连续选择与多选下的批量复制/批量移入回收站。
- 多选时组织面板仍显示主选中笔记的标签状态。

- Claude 实施状态：提交待验收（实机未做）
- Codex 验收状态：未验收
