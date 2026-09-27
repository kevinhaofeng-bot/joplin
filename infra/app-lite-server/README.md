# app-lite-server 部署与备份

同步协议见 `docs/research/sync-protocol-v1.md`，HTTP 与客户端设计见 `docs/research/sync-client-design-v1.md`。

- 独立服务：自己的目录、端口（默认 8787）和容器，不替换任何已在运行的服务。
- token：在 `app-lite-server.env` 中设置 `APP_LITE_SERVER_TOKEN`，至少 32 字节随机值。该文件不入库。
- 传输：明文 HTTP，只应在局域网内使用；如需跨网络访问，放在支持 TLS 的反向代理之后。
- 构建与启动：`docker compose -f infra/app-lite-server/compose.yaml up -d --build`（在仓库根目录执行）。
- 备份：`infra/app-lite-server/backup.sh <data 目录> <新备份目录>`。服务运行中也可以执行；需要宿主机有 `sqlite3` 和 `shasum`。数据库经在线备份 API 复制，blob 逐个复核哈希，未完成的分片不备份。失败时不会留下半成品目录。
- 恢复演练：`infra/app-lite-server/restore-drill.sh <备份目录> <新临时目录> <app-lite-server 可执行文件> <sync_drill 可执行文件>`。它把备份恢复到临时目录，在本机随机端口启动第二个服务，让一个全新客户端完整拉取并重新哈希全部附件。
- 实际恢复：按演练脚本前半段，把 `sync.sqlite` 与 `blobs/` 复制到新的 data 目录，再启动服务。客户端不需要改动：如果恢复点早于某个客户端最后一次同步，客户端在下次同步前核对锚点（它已知的最高游标及其 op_id）时会发现，然后从头重新对账。内容相同的直接采用；本机不同的内容保存为冲突副本；服务端丢失的内容重新上传。状态行会提示这一点。
