# Evernote 核心复刻：组织与搜索底层快照

日期：2026-09-12。此文件记录 `joplin-lite-native-v0.14.0-organization-search-core-snapshot` 的边界；它是可推送的开发快照，不是完整产品验收。

## 源码证据与本地实现

本阶段继续使用 [核心行为源码映射](evernote-11.32.5-core-product-behavior-map.md) 的只读 Evernote 11.32.5 证据。其 `38218` note-list query builder 的容器、标签交集、回收站和分页语义对应本项目 `app-lite-core` 的有界列表查询；`51113` 导航树、`74300` 列表与选中笔记分离、`76905` 列表模式、`9435` 卡片/列表样式对应 GPUI 的组织路由、稳定 NoteId 选择及卡片、摘要、紧凑视图。实现是独立的 Rust/GPUI 代码，不是源码移植。

搜索层参考 `83028` 的本地查询与结果投影、`36175`/`12876`/`41774` 的索引队列分层，已落地查询解析、SQLite schema v8 的 `unicode61` + trigram FTS、稳定索引行映射、至多 100 条的增量处理入口，以及仅返回轻量笔记投影的查询接口。双 FTS 是本项目针对中英混合搜索的选择，不声称是 Evernote 的原样实现。

## 本快照实际覆盖

- 本地笔记本、stack、标签、快捷方式、回收站的事务与有界列表投影；卡片缩略图、编辑会话保留、新建路径及窄宽工具栏的阶段性实现。
- 搜索解析与索引/查询底层及迁移测试。索引处理函数已有，但尚未接入完整应用后台调度。
- 现有编辑器、保存和图片稳定路径继续保留；侧栏/列表切换仍为即时切换。Evernote 风格缓动因 GPUI 中间帧会触发昂贵的编辑器重排，暂缓交付。

## 未完成，不能据此宣称已验收

全库搜索入口及 Cmd-K、笔记内 Cmd-F、搜索历史/建议、附件 OCR/PDF 索引、后台索引调度与失败分类、NAS 同步和迁移仍在后续阶段。1,662 篇库的搜索 Release 性能、真实用户库迁移和当前快照的完整端到端交互验收也未完成。`docs/research/evernote-11.32.5-core-product-behavior-map.md` 第 4 节是阶段起点的历史状态表，并非本标签的实时完成清单。

## 推送前验证

在本快照工作树上：`cargo fmt -- --check` 两 crate 通过；`app-lite-core` 默认及 `test-support` 全部测试通过；`app-lite-gpui` 主测试目标 1,270 通过，1 个已知的 donor Markdown cut 测试被显式跳过；两 crate 的 `cargo check --tests` 通过。Release 构建与远端提交/标签验证以本标签推送记录为准，不在本文件预先宣称通过。
