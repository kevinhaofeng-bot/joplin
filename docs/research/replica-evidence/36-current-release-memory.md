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

## 同窗口空库对照

同一二进制、新建 `/tmp/joplin-empty-memory.xm40cs/library`（0篇、0资源），PID88142。CGWindowList 确认窗口1160×789；启动约26秒 RSS57136KiB，footprint84MB。完整输出在该目录 `footprint.txt`、`vmmap.txt`。

空库：IOSurface42MB、IOAccelerator16MB，与真实库相近；graphics dirty7648KiB、reclaimable160MB。真实库 graphics dirty526MB、reclaimable0。因此不能将真实库约630MB说成固定的空窗口启动开销；加载内容后图形资源驻留/可回收状态差异是下一步排查重点。尚未锁定具体纹理、驱动账目或缓存行为，不能直接断言是缩略图泄漏。对照进程已正常退出。

## 逐区域映射与长静置补验

第五次隔离进程 PID88475 在启动4分11秒后 RSS92720KiB，完整 `vmmap-detail.txt` 显示16个独立32MiB的 IOAccelerator(graphics) 区域，合计512MiB，均为 `PURGE=V`，resident/dirty 均为32MiB。graphics 汇总 resident/dirty525.7MiB、volatile512MiB。其余可辨认的窗口表面为3个2320×1522 BGRA、各13.8MiB的 CAMetalLayer drawable。

随后 `footprint-late.txt` 仍报告物理占用620MB、峰值631MB，graphics526MB且reclaimable0。两个工具对purgeable与计费账目的表述不同；不能将512MiB直接从physical footprint扣除，也不能据此断言实际不可回收或确定泄漏。至少可排除“仅启动瞬时峰值，稍候自然回落至低内存”的解释。

下一步实现方应对这16个32MiB区域追踪Metal/驱动分配来源，并比较内容路径和空库；当前没有证据把它们直接归因于某个atlas、缩略图或instance buffer。原始输出保存在 `/tmp/joplin-current-memory.tPavET/`。进程经其自身原生退出菜单正常退出；未修改实现代码或原资料库。

## Claude对照实验的独立原始输出核对

Codex读取Claude scratchpad `mem-out/{empty,text,images,top8,top8-small}/footprint.txt`（基路径 `/private/tmp/claude-501/-Users-kevinhao-Projects-joplin/7ec497ae-baa7-4faf-80bf-dc5acc1fff91/scratchpad`）。可确认记录的数字：empty71MB、text82MB、images614MB、top8 93MB、top8-small102MB；images的graphics dirty530MB/reclaimable0，其余这些样本graphics dirty约7.5–12MB/reclaimable514MB。来自施工方合成fixture，未独立核对每个fixture内容及完整运行控制，不能替代原真实规模隔离库复测。

审读 `ui/mod.rs::spawn_derived_text_scheduler` 1399–1522：每次Some(Ok(...))分支调用shell_cx.notify，DerivedTextIndexed也触发notify，即使未刷新活动搜索。与Claude提出的OCR工作/刷新关联线索相符，但这是静态可行路径而不是因果证明。需要同一fixture、相同窗口条件、OCR前后及受控刷新比较，且不能靠禁用OCR让性能门表面通过。当前仍未认定根因或修复。
