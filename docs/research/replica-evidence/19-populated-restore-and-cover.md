# 有笔记与关联附件的恢复验证

2026-09-28。本轮新增test-populated-restore.sh及封面同步修复，尚未提交。

## 真实链路

使用当前代码编译的服务端、sync_drill与multimodal_probe，创建5篇合成笔记（图片、PDF、音频、视频、普通文件），上行到本机临时服务，在线backup.sh，restore-drill.sh启动独立恢复服务，然后全新客户端拉取。音视频是探针提供的占位字节，本用例不证明可播放。

比对5篇正文canonical HTML、标题、笔记本、封面选择，5条note_resources关联及顺序、资源元数据、客户端重读全部附件的SHA256；源/恢复库均完整性检查通过。不存在个人资料或正式NAS修改。

初次调用磁盘已有release服务端返回HTTP400，日志`/tmp/joplin-populated-restore.log`；重建当前服务端和客户端后上传通过，不能把旧二进制结果归为当前源代码回归。随后只读检查器遇到macOS SQLite只读WAL数据库错误；在确认客户端已退出且无WAL文件后使用immutable只读URI读取，没有以该方式读取活跃库。

## 实际发现的产品缺陷

`/tmp/joplin-populated-restore-final.log`逐字段比对失败：源图片笔记有selected_thumbnail_id，恢复端为NULL。根因是entity_payload未带该字段，RemoteNote也未写回默认/选择封面。

新增真实HTTP测试`selected_image_cover_survives_sync_to_a_fresh_client`：两张图片选第二张，旧实现新客户端列表选中第一张（`/tmp/joplin-sync-cover-red.log`）。补充可选载荷字段、关联/图片有效性检查，远端资源关联写入后使用既有封面选择逻辑。旧载荷无字段仍可解析。

定向4项HTTP用例通过，`/tmp/joplin-sync-cover-green.log`。端到端重新运行退出0：`/tmp/joplin-populated-restore-cover-fixed.log`；证据目录`/var/folders/hs/6pzhpdqx7kl_3bxn_h8qhyt00000gn/T/joplin-populated-restore.4bNWV8`。所有严格比对保留，未删掉封面断言。

## 尚未关闭

当前core完整cargo test退出0：345通过、1忽略，日志`/tmp/joplin-sync-cover-full.log`。这不替代下面未覆盖的语义边界。

后续修复：封面独立冲突原先被matches_local仅比较正文/标题而忽略，新增真实HTTP三图片双端分歧用例，旧代码只保留一端选择（`/tmp/joplin-cover-conflict-red.log`）。将明确的封面选择加入可见状态对比，并在conflict_copy中复制该字段。两端现在均保留第二张和第三张各自的选择，不退回第一张；5项HTTP测试通过（`/tmp/joplin-cover-conflict-green.log`）。

兼容/防御测试`legacy_note_payload_works_but_invalid_cover_references_are_rejected`使用真实ServerStore发布5种载荷：省略字段正常接受；数字、非法ID、未关联ID、关联的非图片资源全部跳过并留下4条可见失败记录，后续同步不阻断。日志`/tmp/joplin-cover-legacy.log`。

整款GUI需重建，不可用已有065129Z包声称包含本次core修复。真实NAS、大库备份恢复、播放、UI实机及完整成品验收均未完成。CUA reset再次Transport closed，未新建或反复重启既有测试实例。

本轮完整core回归347通过、0失败、1忽略（`/tmp/joplin-cover-final-core.log`）。应用第一轮1437通过、1失败、2忽略，失败为`mounted_scheduler_advances_a_derived_job_saved_after_open`，50ms推进后仍Pending而非Unsupported失败，日志`/tmp/joplin-cover-final-gui.log`。单独重跑通过（`/tmp/joplin-cover-scheduler-recheck.log`），没有改测试或放宽断言，不能据此认定间歇问题消除。完整应用再次重跑日志`/tmp/joplin-cover-final-gui-recheck.log`，最终状态需另行核对。

重跑进程已退出0：1438主测试与17集成测试通过，4项忽略。仍保留首次调度失败为待查间歇问题，不将重试通过描述为调度缺陷修复。当前同步封面修改未提交。
