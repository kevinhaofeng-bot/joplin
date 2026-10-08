# Joplin Lite 0.7.3 真实资料库正式交付记录

日期：2026-10-09

## 结论

Joplin Lite 已从隔离验收资料库切换到用户的真实资料库，并完成正式安装、真实内容打开、全文搜索、资源完整性和 NAS 同步静止态验收。本标签记录的是正式部署状态，不包含新的编辑器实现，也不宣称完整复刻 Evernote 的所有功能。

## 固定对象

- Git 基线：`50a842cb6133b658c4c8cb3ee2cd6e952f659508`（`joplin-lite-native-v0.7.2-personal-delivery`）。
- 应用源构建提交：`c194a7a68510fa2718f92f39a7702891087a3711`。
- 正式安装：`/Applications/Joplin Lite.app`。
- 应用版本：`0.7.2`（build `16411`）；`codesign --verify --deep --strict` 通过。
- 正式资料库：`~/Library/Application Support/com.ArielKevin.Joplin-Lite/library`。
- 原 24 篇笔记验收资料库已备份到 `~/Library/Application Support/Joplin Lite Backups/pre-real-library-20261008T155001Z`，没有覆盖真实资料库。

## 真实资料库验收

- SQLite `integrity_check`：`ok`。
- 笔记：1667。
- 笔记本：31。
- 笔记本组：2。
- 标签：64。
- 资源记录：4153。
- 实际资源文件：4127；文件名所载 SHA-256 与文件内容逐个校验一致。
- 搜索并打开真实笔记“日本驾照两个密码”，标题、正文和图片均可见。

## 正式同步链

- 客户端仅连接本机私有入口 `https://localhost:18789`；认证令牌和 CA 材料只保存在用户私有配置中，不进入仓库。
- 本机 LaunchAgent `com.arielkevin.joplinlite.real-library-private-link` 当前运行，转发进程为 `frpc`。
- NAS 服务 `joplin-lite-real-sync.service` 与 `joplin-lite-real-private-link.service` 已安装、启用并运行。
- NAS 服务数据目录：`/volume1/Projects/joplin-lite-scale-234.sm9hfmWS/restored/data`。
- 同步实体：5917；服务器游标：5917。
- 客户端 `sync_outbox`、`sync_inflight`、`sync_failures`、`sync_conflicts`、`sync_deferred` 均为 0。
- 界面最终状态为“上传 0、下载 0”。
- 原验收同步服务仍保留且未改动，可独立回看旧验收环境。

## 稳定基线和回滚边界

- 本次没有重写已经通过验收的编辑、图片、搜索、导入、备份或同步实现，只进行了正式资料库和私有同步入口切换。
- 如正式配置异常，先停止真实资料库专用私有入口，保留正式资料库和 NAS 服务数据不动，再依据上述备份恢复旧验收资料库；不得用空库覆盖真实资料库。
- 后续功能开发必须在独立测试资料库中完成回归，再切换到正式资料库，不得直接扩展破坏这条已验证链。

## 标签含义

`joplin-lite-native-v0.7.3-real-library-delivery` 固定“真实资料库已经接入并同步稳定”的部署证据。应用二进制仍为 0.7.2；0.7.3 是仓库交付标签，不伪装成重新编译过的 App 版本。
