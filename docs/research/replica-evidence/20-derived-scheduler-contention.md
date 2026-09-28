# 派生索引调度争用复核

2026-09-28。对应证据19的完整套件偶发Pending失败。

生产调度器`spawn_derived_text_scheduler`对进程全局`DERIVED_TEXT_WORKER_LOCK`使用try_lock，失败后保持scheduled=true，等待250ms，再经50ms事件循环继续。原测试只推进50ms，因此其他窗口/并行测试持锁时“必须已处理”的断言并不成立。

确定性复现：初始挂载完成后主动持有同一锁，再保存关联附件并推进50ms。原终态断言稳定失败为Pending attempts=0，日志`/tmp/joplin-scheduler-lock-red.log`。这证明锁争用足以产生之前的症状，不冒充已采集到首次失败瞬间的锁持有者。

测试修正后先严格断言锁占用时任务保持Pending attempts=0，再释放锁、推进250+50ms既有周期，严格断言任务为Unsupported失败且attempts=1。未再次保存、注入事件、直接调用提取器，未修改生产重试或延迟。定向通过`/tmp/joplin-scheduler-lock-green.log`，不是简单删除终态断言或加真实sleep。

完整应用回归日志`/tmp/joplin-cover-scheduler-full.log`，最终退出状态另行确认。保留首次失败与重现证据。此项不代表真实系统输入法、NAS或整款软件验收完成。

最终确认退出0：1438主测试、17集成测试通过，合计4项忽略；git diff --check通过。生产后台单子进程互斥和退避未改。
