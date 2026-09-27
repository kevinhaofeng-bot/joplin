# app-lite-server 部署与备份

同步协议见 `docs/research/sync-protocol-v1.md`，HTTP 与客户端设计见 `docs/research/sync-client-design-v1.md`。

- 独立服务：自己的目录、端口（默认 8787）和容器，不替换任何已在运行的服务。
- token：在 `app-lite-server.env` 中设置 `APP_LITE_SERVER_TOKEN`，至少 32 字节随机值。该文件不入库。
- 传输：默认明文 HTTP，只应在局域网内使用。设置 `APP_LITE_TLS_CERT` 与 `APP_LITE_TLS_KEY`（PEM 文件路径，compose 中放在 `./tls` 并挂载为 `/tls`）后，服务端自己提供 HTTPS，不需要反向代理；只设其一会拒绝启动。自签名证书：把证书 PEM 文本填入客户端 `sync.json` 的 `server_certificate_pem`；客户端不信任证书时会明确提示并暂停自动同步，不会反复重试。
- 弱网：每个连接读写超时 60 s；TLS 握手加请求头必须在 10 s 内完成；最多 64 个连接、32 条推送流；JSON 请求与响应超过 1 KiB 时 gzip 压缩。
- 推送：`GET /v1/events`（Server-Sent Events），每 25 s 心跳，5 min 后 `bye`。若仍放在反向代理后，代理必须对这个路径关闭缓冲（服务端已发送 `X-Accel-Buffering: no`），空闲超时要大于 25 s。
- 构建与启动：`docker compose -f infra/app-lite-server/compose.yaml up -d --build`（在仓库根目录执行）。
- 备份：`infra/app-lite-server/backup.sh <data 目录> <新备份目录>`。服务运行中也可以执行；需要宿主机有 `sqlite3` 和 `shasum`。数据库经在线备份 API 复制，blob 逐个复核哈希，未完成的分片不备份。失败时不会留下半成品目录。
- 恢复演练：`infra/app-lite-server/restore-drill.sh <备份目录> <新临时目录> <app-lite-server 可执行文件> <sync_drill 可执行文件>`。它把备份恢复到临时目录，在本机随机端口启动第二个服务，让一个全新客户端完整拉取并重新哈希全部附件。
- 实际恢复：按演练脚本前半段，把 `sync.sqlite` 与 `blobs/` 复制到新的 data 目录，再启动服务。客户端不需要改动：如果恢复点早于某个客户端最后一次同步，客户端在下次同步前核对锚点（它已知的最高游标及其 op_id）时会发现，然后从头重新对账。内容相同的直接采用；本机不同的内容保存为冲突副本；服务端丢失的内容重新上传。状态行会提示这一点。
