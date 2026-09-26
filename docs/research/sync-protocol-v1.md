# Joplin Lite 同步协议 v1（草案，待 Codex 审核后再接网络）

独立设计。参考 Evernote `MutationUpsyncActivity`（`main-readable/src/modules/42665__module-42665.js`，SHA256 `0a965cac…ea2b7336`）的机制：持久队列、批量上行（默认 25）、逐项 success/retry/permanent、retry 放回队首并带最大延迟、成功后清 retry 状态并更新 lastSyncTime。不声称来自 Evernote 服务端。

## 1. 标识

- `device_id`、`op_id`、实体 id：32 位小写十六进制。`op_id` 全局唯一，客户端生成后随 outbox 行持久化，重试原样重发。
- 实体种类：`note`、`notebook`、`stack`、`tag`、`resource`（元数据）。笔记的标签与附件引用放在笔记 payload 内。
- 附件字节以 SHA-256 标识，独立于实体操作传输。

## 2. 实体操作（push）

```text
Operation { op_id, device_id, entity: {kind, id}, base_revision: u64, action: Put{payload: JSON object} | Delete }
PushRequest { protocol: 1, device_id, ops: [Operation]  (≤100 条, ≤4 MiB) }
PushResponse { results: [OpResult] }  // 与 ops 等长、同序
OpResult = Accepted { op_id, revision, cursor }
         | Conflict { op_id, server_revision, server_deleted, server_payload }
         | Retryable { op_id, reason, retry_after_ms }
         | Permanent { op_id, reason }
```

服务端逐条处理，每条一个事务：

1. `op_id` 已记录 → 返回首次记录的结果，不再执行（响应丢失后重试只生效一次）。同一 `op_id` 内容哈希不同 → `Permanent("op_id reused")`。
2. 校验种类、id、payload 为 JSON 对象、大小上限；不合法 → `Permanent`，并记录。
3. `base_revision` ≠ 当前 revision（不存在为 0）→ `Conflict`（带服务端当前版本），并记录。删除与编辑的竞争由此显式暴露，不静默覆盖。
4. 否则 revision+1，写实体，追加变更日志（单调 `cursor`），记录结果 → `Accepted`。

`Retryable` 只用于服务端暂时无法处理（忙、磁盘、关闭中），不记录，客户端原样重发。HTTP 200 不代表成功：客户端只按逐项结果处理。

## 3. 增量拉取（pull）

```text
PullRequest { protocol: 1, cursor: u64 (exclusive), limit (≤500) }
PullResponse { changes: [Change], next_cursor, has_more }
Change { cursor, entity, revision, deleted, payload, op_id, device_id }
```

`cursor` 严格递增。客户端把应用 changes 与保存 `next_cursor` 放在同一本地事务；应用失败不推进 cursor。

## 4. 附件

```text
BlobStatus(sha256) → Complete | Partial{bytes} | Missing
PutChunk { sha256, size, offset, bytes }   // offset 必须等于已接收长度（断点续传）
ReadRange { sha256, offset, len }
```

未完成上传存为 `uploads/<sha256>.part`，对外不可见；长度达到 `size` 时校验 SHA-256，通过后原子改名到 `blobs/<sha256>`；不符则删除分片并返回永久错误。重复上传已完成 blob 直接成功，不产生重复。

## 5. 客户端义务（后续实现，本轮不含网络）

- 本地修改与 outbox 行同一 SQLite 事务（已存在：`sync_outbox`）。
- 只在 `Accepted` 后删除 outbox 行；`Retryable` 保留并退避；`Permanent` 保留并在状态栏给出原因；`Conflict` 生成可见冲突副本（本地版本另存为新笔记）并采用服务端版本。
- 发送不持有 UI 线程锁。
- 恢复出的新客户端不重放旧 outbox（任务3已清空）。

## 6. 本轮交付与门

交付：`packages/app-lite-protocol`（类型与序列化）、`packages/app-lite-server` 的 SQLite 存储与契约测试（进程内，无 HTTP）。
待 Codex 审核本协议后：HTTP 传输、认证与体积限制、客户端 sync 引擎、Docker 与 NAS 部署（部署前读 `~/servers.md` 并征得用户同意）。
