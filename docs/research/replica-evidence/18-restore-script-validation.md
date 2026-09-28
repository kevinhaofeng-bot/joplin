# 恢复脚本前置校验与依赖修复

2026-09-28，Codex独立实现。当前为未提交增量；不涉及NAS或现有服务写入。

## 失败与修复

- 旧restore-drill.sh在核查附件名之前已创建恢复目录；非法`../outside`测试实际失败，证据目录`/var/folders/hs/6pzhpdqx7kl_3bxn_h8qhyt00000gn/T/joplin-restore-validation.xdvaGc`。
- 新路径先验证三个清单/数据库文件及blobs目录，拒绝符号链接；SQLite integrity_check、changes/entities计数、唯一的64位小写SHA256名称、文件哈希与总数通过后才创建恢复目录。复制后保留原有二次哈希检查。
- 缺少xxd通过测试PATH中返回127的xxd重现；真实备份完成，但旧token生成处退出127，日志`/tmp/joplin-restore-validation-red.log`。改用od读取32字节随机值并检查64位十六进制结果。

## 验证边界

`/tmp/joplin-restore-validation-green.log`退出0：非法路径、重复、blob计数错误、损坏blob、数据库计数错误均拒绝且未创建恢复目录。实际本机服务在线备份、随机回环端口恢复、新客户端同步、SQLite完整性及blob逐字节比较成功；xxd仍被禁用。

这个正常用例是空服务库加一个独立blob，客户端资源计数为0，因此只证明脚本和环境依赖，不证明真实附件实体同步、实际大库完整性、NAS Docker部署或断电恢复。源服务和恢复服务均为测试专用进程；未改变运行中的产品服务。所有临时证据保留。

服务端完整`cargo test`退出0，28通过、0忽略，日志`/tmp/joplin-server-restore-regression.log`。`bash -n`和`git diff --check`通过。
