# 03 完整导出、备份及空库恢复

- 任务：任务3
- 实现提交：`3d4f209e7`
- 测试构建 SHA：`3d4f209e7`

## 读过的 Evernote 源码

| 文件（`…/evernote-11.32.5/main-readable/src/modules/`） | SHA256 | 符号 / 行段 |
| --- | --- | --- |
| `11354__enex-exporter.js` | `322367f8…4cab113d6fd7073556bb050` | `S()` 214-366：逐笔记流式写 `<note>`、标题/时间/标签/属性/缩略图哈希；`k()` 86-146：读取附件失败只记录日志并 `return`（附件被跳过）；进度回调 false → “Enex export cancelled” |
| `12524__single-html-exporter.js` | 见下 | 单篇 HTML 导出（已有 `readable_export` 覆盖，不改） |
| `26944__export-notes-into-html-action.js` | `5d239842…51a4` | 多篇 HTML 导出动作入口 |

（`12524` 完整 SHA256：`dc27542dabfd1a026f53c07e199d3b7d5b32f2fd2d7a7d7a503657166f9d60fc`。）

采用：进度与取消、内容与附件一起导出。
**独立设计：**Evernote ENEX 不含笔记本/组层级，附件读取失败会被跳过；本产品全库备份 = `VACUUM INTO` 一致快照 + 快照引用的全部内容寻址 blob + 带版本的 manifest（计数、数据库与逐 blob SHA-256/大小），任一 blob 缺失或不符即失败，不生成备份。

## manifest 范围

快照包含：全部笔记（含回收站）、笔记本/组、标签与关系、历史版本及历史独有附件、封面（`selected_thumbnail_id`）、附件元数据、设置、搜索索引（随快照，可重建）。
恢复时：清空 `sync_outbox`、`sync_cursor`（新客户端不重放旧设备的待发操作）；`sync_conflicts` 保留（用户可见信息）。当前 schema 未存设备身份。

## 实现

| 文件 | 作用 |
| --- | --- |
| `app-lite-core/src/import_export/library_backup.rs` | `backup_library`、`restore_library_backup`、`RestoredLibrary`（经 `publish_staged_library` 发布） |
| `import_commit.rs` | `unique_library_destination` 公开 |
| `app-lite-gpui/src/ui/library_backup.rs` | 菜单“备份整个资料库…”（先 flush）/“恢复到新资料库…”；后台执行；结果说明；恢复结果走“打开导入的资料库” |
| `examples/backup_verify.rs` | 规模验收探针（只输出计数/大小/耗时） |

## 测试

`tests/library_backup.rs` 5 项（先编译失败 RED，后 GREEN）：往返（组/笔记本/标签/共享 blob/历史独有附件/回收站/设置/搜索/outbox=0）；备份目标已存在拒绝且不留临时目录；取消不发布；未知版本、篡改 blob、缺失 blob、篡改数据库均拒绝且目标不存在；恢复目标已存在拒绝。GUI `mounted_backup_then_restore_creates_a_new_library_and_keeps_the_active_one`。

变异核验：删除恢复时清空 outbox → 往返测试失败（outbox 15≠0）；关闭 blob 哈希与集合校验 → 篡改测试失败。

全套：core `--tests` **270/270**；GUI `--test-threads=1` **1335/1335**。

## 真实规模（任务2导入的真实资料库副本）

`backup_verify /tmp/joplin-lite-t2-accept/imports-jex/all_notebooks-1790424538 /tmp/joplin-lite-t3-accept`（Release，M3 Max）：

| 项 | 结果 |
| --- | --- |
| 备份 | 30.3 s，4,127 blob，1,258,386,565 B |
| 恢复（含逐 blob 校验与发布前再次校验） | 29.4 s |
| 计数 | 源与恢复完全一致：1,666 笔记、31 笔记本、2 组、64 标签、425 关系、4,153 资源、4,127 blob |
| blob 集合 | 一致 |

## Release 实机

未做（屏幕控制权限被拒）。菜单、保存面板、恢复后“打开导入的资料库”、删除后历史仍显示附件、可读 HTML 浏览效果均未实机验收。

## 未解决

- 备份不含可读 HTML 页面（可读导出仍是 `readable_export` 的受限选区）；如需“全库可读导出”另立任务。
- 备份期间若资源 GC 恰好删除快照引用的 blob，备份失败（需重试），不会生成缺附件的备份。
- `/tmp/joplin-lite-t3-accept` 含 2.5 GB 测试数据，验收后可删除。

- Claude 实施状态：提交待验收（实机未做）
- Codex 验收状态：未验收
