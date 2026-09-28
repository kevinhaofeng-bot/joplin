# 永久删除与共享附件回收独立复核

2026-09-28，工作树HEAD `8cc129b47`；本轮只审查与运行测试，未修改实现。

## 本轮实际执行

`CARGO_TARGET_DIR=/Users/kevinhao/Projects/joplin/.shared-target cargo test --manifest-path packages/app-lite-core/Cargo.toml --test organization_stage_c`

13 passed，0 failed。审读测试后确认覆盖：唯一附件最终删除、两篇笔记共享附件时第一篇删除保留文件、最后引用删除后回收与重开、其他笔记历史引用保留、自身历史删除、复制笔记保留共享附件、SQLite删除注入失败原子回滚、unlink失败持久队列重开恢复、批量永久删除原子性。测试附件为合成字节，不证明真实图片解码或可见UI。

`CARGO_TARGET_DIR=/Users/kevinhao/Projects/joplin/.shared-target cargo test --manifest-path packages/app-lite-gpui/Cargo.toml --bin velotype mounted_purge`

2 passed，0 failed，1460 filtered out。覆盖确认框数量与精确目标、选择变化后确认失效。是GPUI测试上下文，不是实际macOS窗口操作。构建仍有154条既有警告，未在本轮处理。

## 源码核对

`packages/app-lite-core/src/repository.rs` 的 `purge_notes` 在IMMEDIATE事务内验证全部目标均在废纸篓、写tombstone、删除自身历史和笔记、排队搜索删除及同步，再统一决定资源回收。`reclaim_unreferenced_purge_resources` 检查存活note_resources和其他笔记历史引用，并检查同SHA256的其他资源记录。`drain_resource_gc` 持有IMMEDIATE事务到文件删除与队列确认，重新出现blob元数据则取消旧回收任务。

以上与测试断言一致；并非仅凭方法名判定安全。测试内提及Evernote renderer9435.js EXPUNGE路径，但本轮未重新逐行读取该逆向文件，因此不新增“已核验原型源码等价”的结论。

## 验收边界

仓储层的共享附件安全性和失败恢复、GPUI测试层确认行为本轮通过。实际签名App中“复制带图笔记→删除其中一篇→永久删除→剩余笔记图片显示→重启→最后引用回收”的端到端链仍待实机验证；不将本轮测试替代该门，也不据此认定整款软件可交付。
