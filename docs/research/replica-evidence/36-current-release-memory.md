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

## 完整账目补验

第四次隔离启动 PID87467：启动约19秒时 RSS89936KiB；完整 footprint630MB，vmmap physical footprint630.4M、peak630.7M。文件 `footprint-full.txt` / `vmmap-summary.txt` 位于同一诊断目录。

footprint 类别：IOAccelerator(graphics)526MB、IOSurface42MB、其他 IOAccelerator16MB；MALLOC_TINY15MB、SMALL11MB、MEDIUM约8.6MB。vmmap 的 graphics 类别 virtual597.3M、resident301.9M、volatile224M、empty288M；这些与 footprint 是不同账目，不能直接相加或把 volatile 全部扣掉后宣称达标。

已确认差异主要在图形资源，尚未确定具体分配调用。直接阅读当前依赖 GPUI0.2.2 的 metal_atlas.rs / metal_renderer.rs：atlas 按需创建、默认1024²、上限16384²；renderer 包含全视口 path 中间纹理/MSAA，instance buffer 默认2MiB。仅是下一步排查线索，不能据静态代码断言526MB全来自某一纹理。需比较空库与同尺寸窗口、缩放窗口、图片/缩略图缓存及实际GPU分配。

诊断进程已通过自己的原生退出菜单结束，不保留额外高内存测试进程。修改交 Claude，Codex 不改源代码。
