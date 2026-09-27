# 复刻交付状态（任务ID追踪）

实施：Claude Code。验收列只能由 Codex 填写；初始全部为“未独立验收”。

| 任务 | 证据 | 实现提交 | Claude 状态 | Codex 验收 |
| --- | --- | --- | --- | --- |
| 0 基线/测试阻断 | [00-baseline.md](replica-evidence/00-baseline.md) | `ffc16f1ae` | 提交待验收 | 未独立验收 |
| 1 编辑与捕获核验 | [01-editor-capture.md](replica-evidence/01-editor-capture.md) | `20c5314df` | 代码测试已交；实机未做（屏幕控制权限被拒） | 未独立验收 |
| 2 迁移闭环 | [02-migration.md](replica-evidence/02-migration.md) | `a287133a2`..`03838eb2b` | 真实副本导入通过（22.6% 降级）；实机未做 | 未独立验收 |
| 3 完整备份与恢复 | [03-backup-restore.md](replica-evidence/03-backup-restore.md) | `3d4f209e7` | 往返与真实规模通过；实机未做 | 未独立验收 |
| 4 生命周期/组织/浏览 | [04-lifecycle-organization.md](replica-evidence/04-lifecycle-organization.md) | `0b436b4d8`、`308546e3a` | 修复永久删除两处数据缺陷；复制/多选/批量已交；实机未做 | 未独立验收 |
| 5 多模态与搜索 | [05-multimodal-search.md](replica-evidence/05-multimodal-search.md) | `a89acda94`、`224840804` | 修复 SQLite 锁丢失致写入丢失；OCR/PDF 端到端通过；界面实机未做 | 未独立验收 |
| 6 NAS 同步 | [06-sync.md](replica-evidence/06-sync.md) | `e6a73c019`、`93f6271f4`..`9405cb11d` | HTTP 传输、客户端同步引擎（上行/下行/冲突副本/附件断点/恢复身份）与两客户端 HTTP 矩阵已交；GUI 接线、容器与 NAS 部署未做（部署需用户同意） | 未独立验收 |
| 8 剩余交付（迁移/可读导出/表格） | [08-remaining-delivery.md](replica-evidence/08-remaining-delivery.md) | `56c48d6aa`..`22c9c233a` | 迁移严格通过 1386→1581/1666；全库可读导出与恢复；新鲜导入与原生往返已验；实机未做 | 未独立验收 |
| 7 安装与最终验收 | — | — | 未开始 | 未独立验收 |
