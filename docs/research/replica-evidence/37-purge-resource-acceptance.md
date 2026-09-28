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

## 实机链准备进展（尚未通过）

签名包ca923f481，PID82817，隔离库 `/tmp/joplin-current-ui.Od2T2b/library`。实际剪贴板粘贴合成测试窗口PNG，编辑区与缩略图立即可见；原剪贴板已恢复。新笔记 `3fca5290c9d238cc8089a3f0a4774e97` 经原生“复制笔记”菜单生成 `be39da98d102c1609e06497624733ee8`，数据库两条note_resources均指向 `fadeae902da38b285ed5db0fd79e3816`，blob SHA256 `d0b7f0cbc33f575f0144becf1f81abba8b0d8d509f09d3bd49caabd3b3a35aae`。

随后两次AX菜单调用“移至废纸篓”（第二次先显式展开菜单）返回菜单对象，但DB所有deleted_time仍为0。不能认为已删除，也尚不能排除测试焦点/菜单派发问题；需要实际按钮路径继续验证。截图 `purge-image.png`、`purge-trash-settled.png`、`purge-menu-result.png` 保存在上述隔离目录。未执行永久删除，未触碰原资料库。

### 后续实际删除结果

重新返回全部笔记、明确单选顶部副本，各步分开等待后，再调用菜单成功：副本deleted_time=1790590354056。此前调用无效不能据此认定删除实现故障（之前已切到空废纸篓，无有效选中目标）。随后打开废纸篓、单选唯一副本，实际点击“永久删除”，看到明确不可撤销确认框，再点击“确认”。截图 `purge-selected.png`、`purge-confirm.png`。

数据库验证副本行已消失，留下对应note tombstone；原笔记与其他两篇测试笔记仍在，原笔记资源关系保留。文件SHA256复算仍为 `d0b7f0cbc33f575f0144becf1f81abba8b0d8d509f09d3bd49caabd3b3a35aae`。本次永久删除仅针对刚创建的合成副本，不可通过废纸篓恢复，但合成原笔记及图片仍完整。下一步仍须回原笔记目视图片、重启和最后引用回收，不能提前标整个链通过。

已正常退出PID82817，并以同一签名二进制、同一隔离profile重新启动PID93218（窗口26612）。实际点击原笔记后，缩略图与编辑区图片都正常出现，见 `purge-reopened-selected.png`。因此“删除共享资源的一个拥有者后，剩余拥有者重启仍能显示图片”实机分支通过。最后引用回收实机分支仍待完成。

### 最后引用回收实机结果

随后将合成原笔记移入废纸篓（deleted_time1790590710153），明确单选、展开组织面板，点击永久删除并目视确认不可撤销提示，再确认。截图 `purge-last-selected.png`、`purge-last-confirm.png`。

删除后SQL：notes=2（原两篇表格/文字测试笔记仍在）、resources=0、note_resources=0、resource_gc_queue=0；存在原图片笔记与资源的tombstone。对应SHA256文件存在性检查为 `blob_absent`。至此，本次真实PNG“粘贴→复制共享→删除一份→重启剩余可显示→删除最后一份→资源及文件回收”实机链通过。这里只证明单资源合成场景，不推广为迁移库或同步多端全部删除安全性。

本轮永久删除两篇合成图片笔记及其测试图片，不能从废纸篓恢复；原始PNG测试截图仍保留在临时证据目录，可重新构造。未操作个人原资料库。此补充暂未提交，以避免与Claude施工提交并发操作Git索引。
