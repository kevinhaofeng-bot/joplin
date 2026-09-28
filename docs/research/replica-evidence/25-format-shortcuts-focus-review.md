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

## 新包与实机起点

- 新包 `/tmp/joplin-shortcuts-candidate/20260928T080438Z-da3612738/Joplin Lite.app`，源码干净，构建退出0，整包ad-hoc签名校验通过（未公证）。二进制SHA256 `bcbb2fa0a8318c1a84b82069a9484382ab3c7444903997801502d727e28bd7d1`。
- 新隔离资料库 `/tmp/joplin-shortcuts-ui.Y7gmgX`，启动PID65759、窗口26194（仅本次观察有效）。真实AX菜单打开同步设置成功。
- 立即截图早于事件完成，地址看似空；延后截图显示实际已输入，但系统中文输入法将模拟英文按键转换成中文。证据 `settings-url-after.png` 位于该隔离资料库。不能把即时空截图认定为输入丢失，也不能据此宣称地址输入正确。下一步使用不经过拼音按键转换的文本输入，继续保存/证书/同步闭环。
