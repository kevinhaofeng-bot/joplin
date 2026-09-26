# 复刻交付状态（任务ID追踪）

实施：Claude Code。验收列只能由 Codex 填写；初始全部为“未独立验收”。

| 任务 | 证据 | 实现提交 | Claude 状态 | Codex 验收 |
| --- | --- | --- | --- | --- |
| 0 基线/测试阻断 | [00-baseline.md](replica-evidence/00-baseline.md) | `ffc16f1ae` | 提交待验收 | 未独立验收 |
| 1 编辑与捕获核验 | [01-editor-capture.md](replica-evidence/01-editor-capture.md) | `20c5314df` | 代码测试已交；实机未做（屏幕控制权限被拒） | 未独立验收 |
| 2 迁移闭环 | [02-migration.md](replica-evidence/02-migration.md) | `a287133a2`..`03838eb2b` | 真实副本导入通过（22.6% 降级）；实机未做 | 未独立验收 |
| 3 完整备份与恢复 | [03-backup-restore.md](replica-evidence/03-backup-restore.md) | `3d4f209e7` | 往返与真实规模通过；实机未做 | 未独立验收 |
| 4 生命周期/组织/浏览 | — | — | 未开始 | 未独立验收 |
| 5 多模态与搜索 | — | — | 未开始 | 未独立验收 |
| 6 NAS 同步 | — | — | 未开始 | 未独立验收 |
| 7 安装与最终验收 | — | — | 未开始 | 未独立验收 |
