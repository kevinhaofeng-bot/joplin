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

## 续：HTTP 传输与客户端同步引擎（本会话，2026-09-27）

授权依据：交底 `2026-09-26-claude-remaining-delivery-handoff.md` 阶段3，要求“核查现有 protocol/server/client/outbox……完成真实 HTTP、离线两客户端、幂等、附件断点、冲突和恢复矩阵”。上文“等待协议审核门”按这份交底放行。方案见 `docs/research/sync-client-design-v1.md`，是独立设计。

| 提交 | 内容 | 关键测试与先失败/变异证据 |
| --- | --- | --- |
| `93f6271f4` | `SyncTransport` trait 与 `TransportError`；服务端 `http` 模块（tiny_http、4 个工作线程、常量时间 Bearer 校验、JSON 8 MiB / 分片与 range 4 MiB 上限、409 返回续传偏移）；`app-lite-server` 二进制，token 从 `APP_LITE_SERVER_TOKEN` 读取；协议 crate 的 `client` feature 提供阻塞式 ureq 客户端（明文 HTTP，TLS 交给反向代理） | server：契约 10、HTTP 5。先失败：接口不存在时编译失败。变异：去掉鉴权后 401 测试失败 |
| `c19db6275` | schema v11：`sync_entities`、`sync_inflight`、`sync_failures`；上行引擎。在途操作先落盘，响应丢失后原样重发；候选实体 = outbox ∪ 从未上传的现存实体（导入库和默认笔记本也会上传） | `sync_push` 4 项。变异：每次都丢弃在途操作，则“响应丢失”测试失败。v10→v11 升级保留笔记与 outbox |
| `e72900858` | 下行：先拉后推，推送冲突后再拉再推；每页应用与游标推进在同一事务，不产生回声；冲突副本；远端删对本地编辑保留编辑；本地删对远端编辑按远端恢复；拉到自己丢了响应的操作按“已接受”处理；新设备的空默认笔记本采用资料库的 ID；远端正文不规范时记为可见失败并跳过 | `sync_two_clients` 5 项。变异：关掉冲突副本则 2 项失败，关掉默认笔记本归并则 1 项失败 |
| `a8c40e75a` | 附件：上传前从断点补齐 blob，失败则整轮停止；下载流式写入并校验哈希，应用时在写事务内复核并移出 GC 队列；远端删除本地仍在用的附件时保留并重新上传 | `sync_resources` 2 项。变异：跳过上传则 2 项失败，删除在用附件则 1 项失败 |
| `6855112a1` | 内容相同不生成冲突副本；从未上传过的现存实体按“有未确认修改”处理；备份恢复后重置同步身份（新设备 ID，不重放旧队列，有未确认修改的实体重新上传当前状态） | 两项新测试先失败：多出冲突副本；恢复后仍是旧设备 ID |
| `9405cb11d` | 两客户端经真实 HTTP：收敛并保留冲突副本；附件经服务端重启和客户端重开后按字节到达；错误 token 返回 Unauthorized，本地队列与服务端都不变 | `sync_http` 3 项（集成验证，不是新功能，没有“先失败”一步） |

最新 core 全套 380 通过，日志 `/tmp/joplin-stage3-claude/core-2e.log`；server 15 通过。

服务端重启这条测试是在上传完成后重启，没有在上传中途重启。上传中途断开后的续传由进程内 `FlakyUpload` 测试覆盖，分片落盘后的持久性由契约测试 `interrupted_blob_upload_resumes…` 覆盖。

### 未完成 / 需用户决定

- GUI 接线：同步入口、服务器地址与 token 设置、状态栏（本地已保存 / 待同步 / 同步中 / 最后成功 / 失败原因）、冲突与失败列表。本地已保存不能显示成已同步。
- 冲突副本、失败列表的界面呈现，以及实机验证。
- `infra/app-lite-server`：Dockerfile、compose、`backup.sh`、`restore-drill.sh`。
- NAS 部署：需要先读 `~/servers.md`，并由用户在对话中明确同意，其他会话的授权不算数。尚未执行。
- 标签与笔记本的删除/改名冲突采取“远端覆盖”，不生成副本，这是有意取舍。

- Claude 实施状态：同步引擎与 HTTP 已提交，待验收；GUI、部署未做
- Codex 验收状态：未验收
