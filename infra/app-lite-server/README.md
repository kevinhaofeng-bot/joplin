# app-lite-server 部署与备份

同步协议见 `docs/research/sync-protocol-v1.md`，HTTP 与客户端设计见 `docs/research/sync-client-design-v1.md`。

## Kevin NAS 当前实际部署候选

模板在 `nas/`，实际操作/证据/失败/回退见 `docs/research/replica-evidence/229-nas-persistent-container.md`。这是NAS独立候选，不代表完整App已验收，也不是可以盲跑到任意已有Docker主机的通用安装器。

- 独立daemon Unix socket `/run/joplin-lite-docker.sock`；root-only，默认Docker/containerd单元保持关闭，不开启默认bridge/IP转发/NAT或Docker公网API。
- 数据根 `/volume1/Projects/joplin-lite-runtime`；四个独立单位为 `joplin-lite-docker.service`、`joplin-lite-sync.service`、`joplin-lite-private-link.service`、`joplin-lite-backup.timer`。只重启这些自有单位，不重启NAS或文件服务。
- LAN为HTTPS8787；Mac私有STCP入口 `https://localhost:18787`，必须在同步设置同时配置随机token与额外证书。生成STCP配置只读既有授权FRP配置，写新700/600目录，不覆盖既有FRP/SMB。基础设施frpc不嵌入编辑器。
- `scheduled-backup.sh <data> <private-work-dir> <backup.sh>`先调用一致性/附件hash检查，再写入已初始化加密restic；`RESTIC_REPOSITORY`、`RESTIC_PASSWORD_FILE`由root-only env提供。自然hourly timer已在北京时间2026-10-05 03:00触发、退出0，第三快照全量检查及第二服务/新客户端恢复通过（22篇11资源、哈希/语义相等，详230补证）。单元尚有restic无缓存warning，不影响此次检查/恢复，但不称规模性能验收；最新自然快照尚未周期离机复制。
- 配置/凭据不进仓库，Mac连接资料在私有 `Library/Application Support/Joplin Lite Network`。233已部署受管CA/每日leaf续期（见下），并留旧自签名回退；CA自身2036-10-01到期及其轮换、周期离机数据备份/整机恢复仍开放。restic同NAS存储池本身不能代替灾难备份；不称全部运维或完整App已验收。
- 现有NAS原policy-rc.d、SMB/NFS/Spotlight与Mac旧FRP配置均保持。回退只停新单位/bootout新LaunchAgent，保留数据、keys、加密库；不要顺手卸包、删库或改默认Docker配置。

230历史接续：Mac私有 `Library/Application Support/Joplin Lite Recovery/offsite-230` 保存一次独立加密仓库/配置/镜像快照；已实际从Mac返回副本恢复第二服务/全新客户端并逐hash/语义比较。不是周期离机复制或整机重装验收，详 `docs/research/replica-evidence/230-off-device-recovery-and-ca-rotation.md`。`nas/issue-server-tls.sh <已有CA目录> <全新候选目录> [有效天数]` 只生成新候选，不替换活TLS；NAS OpenSSL3真实7项及现有Rust客户端同CA续签对照通过。当时未部署CA/续期，已由233下面的实际部署结果覆盖。

233当前常驻部署：`nas/joplin-lite-sync-managed.conf` 为新增drop-in，仅把TLS挂载指向 `/volume1/Projects/joplin-lite-runtime/tls-managed/current`。原base unit/旧tls/env及root-only回退副本保留。`nas/joplin-lite-tls-renewal.service/.timer` 每日03:30+随机0–15分钟运行，默认提前30天签发365天，Persistent及OnBoot补查。续期unit必须用Wants而不是Requires依赖sync：hook重启sync不能反向杀死正在运行的oneshot。真实永久单元换证、两服务/私有连接与同Mac收敛验证及新鲜infra39PASS见 `docs/research/replica-evidence/233-managed-tls-live-deployment.md`。CA稳定根至2036年，但CA自身轮换尚未实现。

233恢复配置权威为Mac私有 `Library/Application Support/Joplin Lite Recovery/managed-tls-233/managed-recovery-final.tar.age`（不是早期同目录的managed-recovery.tar.age，后者含已淘汰Requires配置）。已解密恢复、匹配证书/密钥、逐配置cmp和实际同CA签发校验；数据仓库仍使用230。回退仅停止/禁用自有续期timer/oneshot、移走新增drop-in并恢复原TLS/客户端旧信任，不回滚用户数据；精确范围与限制见233。

## 通用构建、协议与脚本

232实现、233已接入上述常驻服务：`nas/renew-server-tls.sh <managed-dir> <service-hook> [提前天数30] [新leaf天数365]` 在同一私有CA下签发新身份，以原子`current`指针选中，再重启和验证；Linux/OpenSSL3专用。managed目录必须为受信任700目录，包含不链接的`ca/{cert,key}.pem`、`identities/leaf-*`和相对`current`链接。原身份保留；新身份继承原文件/目录属主和私有权限，供nonroot只读容器挂载。不得直接拿229现有普通`tls`目录运行。

服务钩子 `nas/sync-tls-service-hook.sh` 同时校验CA、localhost和**当前leaf公钥pin**，仅无认证401算健康；不能把仍挂载旧inode的容器当作切换成功。默认只重启自有`joplin-lite-sync.service`、检查8787；并行验收可用`APP_LITE_TLS_RESTART_UNIT`限定前缀单位及`APP_LITE_TLS_HEALTH_PORT`指定localhost端口。错误启动/健康检查回到旧指针并重启复验；回退失败保持失败而不是报成功。`flock`防重入，冲突exit75；健康操作有界，不会无限重试。

切换前写入并落盘`activation-pending`旧身份标记，成功或健康回退后才删除并落盘。进程被强制结束所留下的标记，在下一次运行中先恢复并核验旧身份，再检查到期窗口；这是中断状态恢复，不宣称NAS断电实测。详细39项infra回归、真实systemd定时器/nonroot Rust容器及同客户端结果见 `docs/research/replica-evidence/232-managed-tls-renewal.md`，常驻接入的真实缺陷及修正消费者闭环见233。`tls-renew-test-service.sh`只属测试，不安装为运行钩子；这些结果不替代整款App同包验收。

- 独立服务：自己的目录、端口（默认 8787）和容器，不替换任何已在运行的服务。
- token：在 `app-lite-server.env` 中设置 `APP_LITE_SERVER_TOKEN`，至少 32 字节随机值。该文件不入库。
- 传输：默认明文 HTTP，只应在局域网内使用。设置 `APP_LITE_TLS_CERT` 与 `APP_LITE_TLS_KEY`（PEM 文件路径，compose 中放在 `./tls` 并挂载为 `/tls`）后，服务端自己提供 HTTPS，不需要反向代理；只设其一会拒绝启动。自签名证书：把证书 PEM 文本填入客户端 `sync.json` 的 `server_certificate_pem`；客户端不信任证书时会明确提示并暂停自动同步，不会反复重试。
- 弱网：每个连接读写超时 60 s；TLS 握手加请求头必须在 10 s 内完成；最多 64 个连接、32 条推送流；JSON 请求与响应超过 1 KiB 时 gzip 压缩。
- 推送：`GET /v1/events`（Server-Sent Events），每 25 s 心跳，5 min 后 `bye`。若仍放在反向代理后，代理必须对这个路径关闭缓冲（服务端已发送 `X-Accel-Buffering: no`），空闲超时要大于 25 s。
- 构建与启动：`docker compose -f infra/app-lite-server/compose.yaml up -d --build`（在仓库根目录执行）。
- 备份：`infra/app-lite-server/backup.sh <data 目录> <新备份目录>`。服务运行中也可以执行；需要宿主机有 `sqlite3` 和 `shasum`。数据库经在线备份 API 复制，blob 逐个复核哈希，未完成的分片不备份。失败时不会留下半成品目录。
- 恢复演练：`infra/app-lite-server/restore-drill.sh <备份目录> <新临时目录> <app-lite-server 可执行文件> <sync_drill 可执行文件>`。它把备份恢复到临时目录，在本机随机端口启动第二个服务，让一个全新客户端完整拉取并重新哈希全部附件。
- 恢复前检查：数据库完整性、changes/entities计数、附件清单计数、唯一SHA256文件名和实际哈希；拒绝清单/数据库/附件符号链接。在这些检查完成前不创建恢复目录。依赖 Bash、sqlite3、shasum、od 及标准文本工具，不依赖 xxd；仅用于受信任备份的完整性核查，不是备份来源认证。
- 脚本回归：`bash infra/app-lite-server/test-restore-validation.sh [<app-lite-server 可执行文件> <sync_drill 可执行文件>]`。不提供参数只做坏备份拒绝测试；提供两个程序则在临时目录运行真实本机服务及恢复，保留日志和测试数据，不触碰既有服务。
- 备份路径回归：`bash infra/app-lite-server/test-backup-paths.sh`，使用真实SQLite和合成附件，核对中文、空格、引号、换行及相对目录，验证已有目标不覆盖、附件损坏不发布半成品；不需要编译或连接NAS。
- 备份前置回归：`bash infra/app-lite-server/test-backup-preflight.sh`，验证缺失/链接数据库、错误表结构、缺失附件目录不能发布成功备份，已有悬空目标链接保持不变，普通只读数据库和持续写入的WAL仍可在线备份；测试额外需要Python 3标准库，夹具退出自动清理。备份本身不需要Python；使用只读源连接，任何计数查询失败都会中止发布。
- 带数据恢复验收：`bash infra/app-lite-server/test-populated-restore.sh <当前构建的app-lite-server> <sync_drill> <multimodal_probe> <PNG测试文件> <PDF测试文件>`。五篇合成笔记上行、在线备份、独立恢复、新客户端拉取，严格比较正文、封面、附件关联/顺序与哈希。先构建同一工作区的程序，不能混用旧release服务端和新客户端。探针的音视频是占位字节，此测试不验证播放能力。
- 实际恢复：按演练脚本前半段，把 `sync.sqlite` 与 `blobs/` 复制到新的 data 目录，再启动服务。客户端不需要改动：如果恢复点早于某个客户端最后一次同步，客户端在下次同步前核对锚点（它已知的最高游标及其 op_id）时会发现，然后从头重新对账。内容相同的直接采用；本机不同的内容保存为冲突副本；服务端丢失的内容重新上传。状态行会提示这一点。
