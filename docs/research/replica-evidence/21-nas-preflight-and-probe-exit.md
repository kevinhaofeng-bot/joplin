# NAS隔离验收准备与探针退出码修复

2026-09-28。依据本机servers.md接入kevin-nas；不复制凭据到仓库。

## 当前事实

- NAS为x86_64，Docker inactive，PATH中无Docker/Podman/Rust；sqlite3、od、openssl、shasum可用，项目卷约13TiB空闲。
- 当前服务端用已有cargo-zigbuild交叉编译为静态ELF；SHA256 `a1c14bcecd7df0bd8ca63a3beb2dceac2ae6cc395e8044595102620af15e0220`，本机/NAS一致。日志`/tmp/joplin-nas-cross-build.log`，退出0。
- NAS独立目录`/volume1/Projects/joplin-lite-acceptance.zZJblu`，发现共享ACL继承导致初始权限过宽后，已只对该目录收紧为0700、测试令牌/私钥0600。未改共享根目录权限。传统SCP成功，默认SFTP路径不兼容。
- 限时1800秒的测试服务仅监听NAS `127.0.0.1:48787`，TLS，进程由SSH执行会话30919承载；使用独立随机令牌和3天测试证书，不接入原笔记库。后续必须核实进程是否仍活着，不能据旧记录重复启动。
- Mac目录`/tmp/joplin-nas-acceptance.utBFH6`保留合成五篇笔记、测试附件与日志。原计划本机48789转发被NAS SSH拒绝（administratively prohibited）。未更改SSH策略；本次转发PID41194已TERM关闭。

## 未成功的操作与验收器修复

首次sync_drill重试200轮、pending=10，但退出0。依据输出，实际上没有任何条目到达NAS；backup-1和restored均为空库，**不得作为恢复通过证据**。

修复examples/sync_drill.rs：未收敛、还有待同步数据、可见失败或附件哈希不符时，完整打印诊断后退出1。同一失败链复测退出1，`upload-failure-exit-verified.log`明确converged=false pending=10。成功的本机五篇/五附件备份恢复链再次退出0，日志`/tmp/joplin-sync-probe-success-exit.log`，避免“一律失败”的伪修复。

下一步构建并运行Linux客户端探针于NAS，先通过回环TLS做实际NAS存储和恢复验证；Linux探针交叉构建日志`/tmp/joplin-nas-client-cross-build.log`。此处尚无NAS同步成功、NAS恢复成功或常驻部署通过的结论。未安装Docker、改防火墙、公网开放端口或替换既有服务。

## 后续实测：NAS本机 TLS 与有数据恢复通过

Linux客户端交叉编译退出0，传统SCP传入上述隔离目录。确认原测试服务仍监听后，在NAS新建source-linux合成库：TLS上传accepted=11、pending=0，5篇笔记及5个附件，重新计算附件哈希mismatched=0。日志留在NAS隔离目录upload-linux.log。

在线备份backup-2包含blobs=5、changes=11、entities=11；restore-drill.sh恢复至全新recovered-2，临时第二服务向新客户端下发pulled=11、pending=0，5篇笔记及5个附件重新校验mismatched=0，命令退出0。上传端和恢复端内容摘要均为`0e2d51dd73ccc59581dd5e7cd588db5225f8a214dd1360f3f30060474ef8c386`。

范围仅为真实NAS上的回环TLS上传、在线备份、新服务与新客户端恢复，不代表Mac到NAS网络同步、GUI同步、常驻部署或日常定时恢复链通过；音视频仍为合成占位附件，未证明播放。原始backup-1空库继续排除于成功证据之外。

## 后续实测：Mac到NAS局域网TLS往返通过

独立lan-data测试服务绑定192.168.5.170:48788，使用独立3天证书（SAN包含该IP）、原隔离测试令牌，timeout900限时运行，未改防火墙或SSH策略。服务SSH执行会话12379；不是常驻部署。

Mac使用显式可信公开证书，不关闭TLS校验。source合成库上传accepted=11，fresh目录mac-lan-fresh拉取pulled=11，两次退出0、pending=0，均5篇笔记/5个附件，附件重新计算哈希mismatched=0。两端内容摘要一致：`d1d585d8bac308008060c86a65acf7a6b7e7e5edc153597757ed9a9eedf3cb2e`。本机证据目录仍为`/tmp/joplin-nas-acceptance.utBFH6`。

此结果补齐真实Mac到NAS局域网的客户端协议往返，不替代GUI配置/同步交互、长时间断网恢复、常驻服务、定时备份或正式部署验收。
