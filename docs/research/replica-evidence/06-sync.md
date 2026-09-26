# 06 离线优先 NAS 同步（第一步：协议与契约，等待审核门）

- 任务：任务6
- 实现提交：`e6a73c019`
- 状态：**只完成规划第一步**（协议文档 + 契约测试）。规划要求“先提交小型协议文档和契约测试给 Codex 审核，再接网络”，本轮停在该门前。

## 读过的 Evernote 源码

| 文件（`…/main-readable/src/modules/`） | SHA256 | 符号 / 行段 |
| --- | --- | --- |
| `42665__module-42665.js` | `0a965cac5d014e8cfb47cbc1a5b74629b2adb469136aca6a3a529630ea2b7336` | `MutationUpsyncActivity.runSyncImpl`：`shiftUnsyncedMutations` → `rollupForUpsync` → 每批 `maxMutationsPerBatch`（默认 25），单次活动上限 120 s；`processMutationUpsyncResults` 分 success/retry/failed；retry `unshiftUnsyncedMutations` 放回队首并以 `maxRetryDelay` 抛 RetryError；failed 触发 `rebuildOptimisticGraph`；成功清 `RetryableMutationUpsync` 并更新 `lastSyncTime` |
| `06903__module-6903.js` | `c673b4d3…` | 活动注册（仅确认类型） |

采用：持久队列、分批、逐项三类结果、重试放回队首、成功才清状态。
**独立设计：**服务端协议（op_id 幂等记录、base_revision 前置条件、冲突返回服务端版本、单调 cursor 日志、内容寻址断点上传）；Evernote 服务端/Conduit 同步协议不在本重建中，不声称复刻。

## 交付

- `docs/research/sync-protocol-v1.md`：协议 v1 草案（实体操作、pull、附件、客户端义务、门）。
- `packages/app-lite-protocol`：线上类型与 id/sha 校验。
- `packages/app-lite-server`：SQLite（WAL、synchronous=FULL）存储：`push`、`pull`、`blob_status`、`put_chunk`、`read_range`。

## 契约测试（`packages/app-lite-server/tests/contract.rs`，先编译失败 RED，后 GREEN 8/8）

| 用例 | 对应规划要求 |
| --- | --- |
| `a_retried_op_after_a_lost_response_applies_exactly_once` | 提交后响应丢失→相同 op 重试只生效一次 |
| `reusing_an_op_id_for_different_content_is_permanent` | op_id 滥用明确失败 |
| `unrelated_edits_from_two_offline_devices_converge_through_pull` | 双端不相关编辑收敛 |
| `concurrent_edit_of_the_same_note_is_a_visible_conflict_not_an_overwrite` | 同笔记真正冲突可见（冲突结果亦幂等） |
| `delete_versus_edit_never_silently_loses_the_edit` | 删除 vs 编辑不静默丢失 |
| `pull_pages_by_monotonic_cursor_and_survives_reopen` | 单调 cursor、分页、重开持久 |
| `invalid_ids_or_non_object_payloads_are_permanent_errors` | 远程输入按不可信处理 |
| `interrupted_blob_upload_resumes_and_publishes_only_a_verified_whole` | 上传中断续传、hash 校验、半文件不可见、重复上传不重复 |

变异核验：跳过已记录结果查询 → 3 项契约失败。命令：`cd packages/app-lite-server && cargo test --offline`。

## 未做（等待门或需用户确认）

- HTTP 传输、认证、体积限制（依赖协议审核结果）。
- 客户端 sync 引擎（outbox 发送、ack 才清队列、pull 与 cursor 同事务、冲突副本、状态 UI）。
- 两个隔离客户端 + 本地服务的断网/重启/冲突演练。
- `infra/app-lite-server`（Dockerfile/compose、`backup.sh`、`restore-drill.sh`）与 NAS 部署：属外部服务器操作，需先读 `~/servers.md` 并获得用户明确同意。
- 菜单“保存当前笔记”保持原文案（不冒充同步）。

- Claude 实施状态：第一步提交待审核
- Codex 验收状态：未验收（协议待审核）
