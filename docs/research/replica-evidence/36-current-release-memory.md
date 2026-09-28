# 最新 Release 真实规模空闲内存复验

2026-09-28；二进制 SHA256 `8480d20656846012be1b925f74ed58e230edf64fa849bcb464835df71df1f561`（ca923f481，签名包见33）。

来源仅为旧的隔离导入副本 `/tmp/joplin-final-claude/mem-profile`；只读确认1666篇笔记、4153资源记录。SQLite `.backup` 获取一致快照，并 APFS 克隆资源到新目录 `/tmp/joplin-current-memory.tPavET/library`。未打开原资料库。

执行已审读脚本 `measure-product-memory.sh`，3次独立启动、各静置10秒；脚本回收自己启动的进程。Apple M3 Max / 48GiB / macOS14.7.2。日志和 samples.tsv 位于 `/tmp/joplin-current-memory.tPavET/results`。

| 次数 | RSS KiB | RSS MiB | footprint 输出 |
| --- | ---: | ---: | ---: |
| 1 | 94144 | 91.9 | 634 MB |
| 2 | 92672 | 90.5 | 639 MB |
| 3 | 83824 | 81.9 | 626 MB |

结论：空闲 RSS 低于规范120MiB，但 physical footprint 明显高于 RSS，且连续三次重现；不能只用 RSS 宣称低内存目标达成。当前脚本只保留 footprint 汇总数字，下一步需保存完整 footprint/vmmap 输出以解释映射、压缩、图形或其他账目，不能凭猜测下结论。

未覆盖200块/10图、50卡片、输入p95、首帧、查询延迟，也不是总进程树峰值。性能整体门保持未通过。
