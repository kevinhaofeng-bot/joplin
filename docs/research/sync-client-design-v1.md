# Joplin Lite 同步：HTTP 传输与客户端引擎方案 v1

协议沿用 `sync-protocol-v1.md`，本文件只定义协议文档没有写的两件事：HTTP 映射，以及客户端怎么把本地 outbox 变成协议操作、再把拉下来的变更写回本地。服务端协议和冲突策略是本项目独立设计。上行机制参考 Evernote `MutationUpsyncActivity`（见 06 号证据）：持久队列、分批、逐项结果、重试放回队首。

依据：交底 `2026-09-26-claude-remaining-delivery-handoff.md` 阶段3，要求“核查现有 protocol/server/client/outbox……完成真实 HTTP、离线两客户端、幂等、附件断点、冲突和恢复矩阵”。协议文档里的“待审核后接网络”这道门，按这份交底视为已放行。NAS 部署另需用户在对话中明确同意。

## 1. HTTP 映射（`app-lite-server` 的 `http` 模块与二进制，`app-lite-protocol` 的 `client` feature）

| 方法 路径 | 请求 | 响应 |
| --- | --- | --- |
| `POST /v1/push` | `PushRequest` JSON | `PushResponse` JSON |
| `POST /v1/pull` | `PullRequest` JSON | `PullResponse` JSON |
| `GET /v1/blobs/<sha256>` | — | `BlobStatus` JSON |
| `PUT /v1/blobs/<sha256>?size=N&offset=M` | 原始字节 | `BlobStatus` JSON |
| `GET /v1/blobs/<sha256>/range?offset=M&len=L` | — | 原始字节 |

- 认证：每个请求都要带 `Authorization: Bearer <token>`。token 由服务端启动参数或环境变量提供，至少 32 字节，用常量时间比较；不匹配返回 401，且不暴露服务端任何状态。
- 上限：JSON 请求体不超过 8 MiB；单个 chunk 不超过 4 MiB；range 的 `len` 不超过 4 MiB。超限返回 413。
- 状态码：4xx 表示请求本身不合法，客户端按永久错误处理；5xx 和连接失败按可重试处理。HTTP 200 只说明传输成功，每个操作的结果仍以 `OpResult` 为准（协议第 2 节）。
- 服务端单线程或小线程池，底层是已有的 SQLite `ServerStore`。附件写入已有每个 hash 一把 OS 独占锁（Codex 修复）。
- 客户端实现同一个 `SyncTransport` trait，进程内的 `ServerStore` 也实现它。这样同一套引擎测试既能跑进程内，也能跑真实 HTTP。

## 2. 客户端引擎（`app-lite-core`，不含网络依赖）

新增 schema v11，只做加法（三张新表，已有表不变）：
- `sync_entities(entity_type, entity_id, server_revision, PRIMARY KEY(entity_type, entity_id))`：记录每个实体最后一次已知的服务端修订号，作为 `base_revision`。本地的 `revision` 和服务端修订号是两套独立编号，不能混用。
- `sync_inflight(entity_type, entity_id, op_id, base_revision, action_json, outbox_ids_json, created_time, PRIMARY KEY(entity_type, entity_id))`：每个实体至多一条“在途操作”，在发送之前落盘。响应丢失后原样重发同一个 op_id 和同一份 payload，直到拿到确定结果（Accepted/Conflict/Permanent），然后才基于最新本地状态生成下一条。否则会出现两种错误：同一 op_id 带上新内容，服务端判为 `op_id reused`；或者换新 op_id，却和自己上一次已被接受的写入冲突。
- `sync_failures(op_id PRIMARY KEY, entity_type, entity_id, reason, updated_time)`：记录永久失败的原因，供状态栏显示。
- 设备 ID 存在 `settings` 的 `sync.device_id`，首次同步时生成。从备份恢复时清空这一项，新客户端不会冒用旧身份。

一次同步按下面顺序执行：
1. 上行附件：本地待发的笔记引用了哪些资源，就先确保这些 blob 在服务端完整。用 `blob_status` 查询后从断点续传，只有状态为 `Complete` 的才继续后面的步骤。
2. push：已有在途操作的实体，原样重发。其余实体按 outbox 合并，同一实体只发最新状态，参照 Evernote 的 rollup；生成新的 op_id，与所覆盖的 outbox 行 id 一起写入 `sync_inflight` 后再发送。实体仍然存在就发 `Put{payload}`，笔记进回收站也按 Put 发，带上 `deleted_time`；已彻底删除的发 `Delete`。实体从未上传过、本地又已不存在的，直接丢弃这些 outbox 行，不发送。每批最多 100 条。发送时不持有 UI 锁，由调用方在后台线程执行。
3. 逐项处理结果：
   - `Accepted`：在同一个事务里删除在途操作所覆盖的 outbox 行（发送之后新产生的行保留）和在途行本身，并更新 `server_revision`。
   - `Conflict`：本地版本另存为一篇新笔记，标题加“（冲突副本 设备时间）”，然后按服务端版本覆盖原实体并登记 `sync_conflicts`，最后清掉这批 outbox 行。服务端版本是删除时，本地编辑作为新笔记保留，不会静默消失。
   - `Retryable`，或整个请求的传输失败：保留在途行，下次原样重发。
   - `Permanent`：保留在途行和 outbox 行，写入 `sync_failures`。不自动重试，由用户看到原因后处理。
4. pull：从 `sync_cursor` 开始拉取。每一页的变更应用和 cursor 推进放在同一个本地事务里，应用失败就不推进 cursor。应用远端变更时不写 outbox，避免回声。遇到自己设备发出、且修订号不高于已知值的变更，直接跳过。如果本地该实体还有待发的 outbox 行，就按冲突处理（本地另存副本）。拉下来的资源元数据先下载 blob（`read_range`，边下边校验 SHA-256），完整后才写入资源行。

payload 只包含可移植字段：
- note：`title`、`body_html`、`notebook_id`、`tag_ids`、`resource_ids`、`created_time`、`updated_time`、`deleted_time`
- notebook：`title`、`stack_id`、`is_default`
- stack、tag：`title`
- resource：`title`、`mime`、`file_extension`、`size`、`sha256`

历史修订、编辑日志、搜索索引、设置都不同步。远端来的 `body_html` 当作不可信输入，先经 `CanonicalDocument::parse_html` 规范化；规范化结果和原文不一致，或者引用了未知资源时，该变更记为失败，cursor 不推进。

## 3. 测试矩阵（先失败，后实现）

同一个引擎分别跑在进程内的 `ServerStore` 和真实 HTTP 上，两个客户端各用独立的隔离 profile：
- 响应丢失后重试只生效一次；
- 两个客户端的不相关编辑能收敛；
- 同一篇笔记被两端同时编辑时生成可见的冲突副本；
- 删除和编辑竞争时，编辑不会丢失；
- 服务端或客户端重启后能从断点继续；
- 附件上传中断后能续传，两端的哈希一致；
- pull 应用失败时 cursor 不推进；
- 恢复出来的新客户端不会重放旧身份；
- 401、413 和永久错误在界面上可见。

## 4. 不在本方案内

- 界面：同步按钮、状态栏、冲突列表。这些属于 `app-lite-gpui`，由另一会话在引擎 API 确定后接线。
- NAS 容器与部署：`infra/app-lite-server`（Dockerfile、compose、`backup.sh`、`restore-drill.sh`）可以先写好并在本机测试；真正部署到 NAS 需要用户在对话中明确同意。
