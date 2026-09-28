# 正文格式快捷键与同步浮层焦点：独立检查

2026-09-28。补丁提交 `da3612738`，本地未推送。检查针对工作区，不代表安装包实机通过。实现代理报告完整 bin 回归1448通过、0失败、2忽略；主代理本轮独立结果限于下面两项。

## 原因与实现核对

- 主编辑器虽使用 BlockEditor 按键上下文，但缺少 BoldSelection、ItalicSelection、UnderlineSelection 的 action 处理；原 spike 的父级处理不适用于主编辑器。
- 新处理仅在可编辑 surface 中注册，调用工具栏同一 CommandCatalogue，不新建一套格式数据逻辑。
- 同步设置面板补 occlude，避免点击输入框时穿透到下面正文；输入容器具有独立 SyncSettingsInput 上下文。

## 主代理独立执行

- `mounted_body_cmd_b_i_u_use_command_history_and_persist_canonical_marks`：1通过、0失败，退出0。日志 `/tmp/joplin-shortcuts-independent.log`。实际分发三个快捷键，逐项核验格式、撤销栈，再保存并解析 canonical HTML 核对三种 marks。
- `formatting_shortcuts_do_not_mutate_body_when_title_search_or_sync_input_has_focus`：1通过、0失败，退出0。日志 `/tmp/joplin-shortcuts-focus-independent.log`。覆盖标题、搜索及设置焦点不修改正文，且检查设置 token 控件实际拥有焦点、文本输入确实进入设置草稿。

## 未关闭

上述使用 GPUI 测试窗口而非 macOS 真实键盘。仍需最终提交全量结果、新签名包中快捷键、中文输入、同步设置证书导入和实际 NAS 同步。此补丁不是全部工具栏及整款产品验收通过。
