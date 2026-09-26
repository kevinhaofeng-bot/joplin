# 跨块剪切测试清理 SIGSEGV 修复

原全套测试的单线程、多线程崩溃报告都指向 `editor::selection::tests::cross_block_cut_writes_markdown_deletes_range_and_undo_restores` 末尾退出。单独 exact 测试复现退出码 139，LLDB 定位到 GPUI 0.2.2 `TestAppContext::quit` 的 `self.app.borrow_mut()`，尚未进入 shutdown。

根因：`add_window_view` 返回的 `VisualTestContext` 经 `into_mut` 注册了 on_quit 释放自身的回调。直接经该对象 Deref 调用 `cx.quit()` 时，quit 先执行回调释放了自身所在对象，再访问 self.app，发生 use-after-free。

最小修复：测试末尾使用 `cx.cx.clone().quit()`。独立克隆保持 app/on_quit 的 Rc 有效，原窗口测试上下文可以正常释放，shutdown 仍执行。不改 GPUI/vendor、不改生产编辑器、不跳过清理。

证据：修改前 exact SIGSEGV；修改后 exact 1/1，selection 测试组 12/12，控制器独立 selection 12/12；fmt/diff 检查通过。独立 Sol 只读审查 Approved。同文件无其他同类 add_window_view 调用，未扩散修改。诊断详情与 LLDB 记录在 `.superpowers/sdd/2026-09-11-evernote-core-notes-roadmap/test-crash-diagnosis-2026-09-26.md`。

该结论只解释已定位的测试清理崩溃，不能推导产品或完整测试全部无缺陷。

## 修复后的整套检查

控制器执行 `cargo test --locked --bin velotype -- --test-threads=1`，已经越过原 selection SIGSEGV，后续在 `ui::tests::mounted_cmd_v_after_typing_queues_saved_point_until_journal_worker_finishes` 因 `cocoa-0.26.0/src/appkit.rs:791` 空指针触发 SIGABRT，退出 101。日志 `/tmp/joplin-lite-after-cleanup-full.log`。这是另一个未定位根因的失败点，不能称全套通过，也不能未经分析归因于真实 App 粘贴功能。
