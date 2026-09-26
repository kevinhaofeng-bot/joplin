# Codex 独立审查：3d16771bc

结论：不通过成品验收；同步协议实施门暂不放行。此次为代码审查与独立自动化复验，未执行 GUI 实机操作、个人数据迁移或服务器部署。未修改实现。

## Findings

1. **P1：单个不支持结构导致整篇迁移降级。** `packages/app-lite-core/src/import_export/jex_body.rs:510` 仅对两类错误直接拒绝，其余进入整篇源字符串转段落；HTML会成为显示出来的标签字符串，Markdown标记成为普通文字，空行被移除，资源统一追加到末尾。包含一个表格/引用的图文笔记也会丢失本可保留的格式和图片位置。实施报告证实真实副本376/1666篇受影响。要求节点级保真转换或局部降级、保留原始内容和附件位置；不得将当前成功计数作为迁移达标证据。

2. **P1：同步上传缺少同一hash的互斥。** `packages/app-lite-server/src/lib.rs:241-264` 对partial长度的检查、append、hash、rename没有锁；数据库Mutex不覆盖此路径。两个客户端同时以同offset上传相同附件可以同时通过检查，随后重复追加，破坏part，甚至在另一请求校验时继续追加。要求按hash串行化整个状态检查至发布过程，持久绑定声明size，并新增并发上传/重试契约；目前8项顺序契约不能证明此场景安全。

3. **P2：恢复先无限复制再验证长度，取消不能中断单文件。** `packages/app-lite-core/src/import_export/library_backup.rs:354` 调用copy_hashing读到EOF后才比较manifest size；数据库亦如此。声明小文件却实际巨大的损坏备份可耗尽磁盘，单文件复制期间取消无效。要求校验声明容量、以size+1限量复制、循环检查取消；复用既有readable_export边界并加超长/取消回归。

## 独立运行

- `cargo test --manifest-path packages/app-lite-core/Cargo.toml --features test-support --tests`：退出0，日志 `/tmp/joplin-review-core.log`。此次明确开启此前遗漏的feature。
- `packages/app-lite-server` 下 `cargo test --offline`：退出0，契约8/8。
- 上述通过不覆盖以上全部失败条件，也不等于GUI或产品验收。

## 尚未通过的交付要求

全库可读HTML导出仍缺；HTTP/客户端同步/NAS恢复演练未交付；界面实际操作与最终产品内存指标未独立验收。要求Claude先修以上问题并给出针对性回归及构建，再由Codex复验。
