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

## 实机设置继续核验

- 保留并恢复系统剪贴板，通过Cmd+V准确输入测试地址及专用测试令牌；地址显示正确、令牌遮蔽，截图 `settings-url-paste.png`、`settings-token.png`。
- 点击导入按钮打开原生Open窗口，通过路径框选择 `lan-cert.pem`，确认按钮实际可用并点击。截图 `picker-selected.png`。
- 实际 `sync.json` 已生成，权限0600。只输出布尔比较，确认地址、令牌、证书与测试源一致，未输出秘密值。配置文件不是测试脚本写入。
- 点击实际应用“立即同步”，界面提示“未能连接同步服务器，稍后自动重试”；隔离库笔记数0。截图 `sync-result.png`。所以设置导入/落盘已有实机证据，但实际同步未通过；下一步核查限时NAS测试服务状态再验证恢复，不能把该失败隐去。

## NAS恢复后的实际应用闭环

- 原限时服务会话55573退出124，NAS `ss -ltn sport = :48788` 无监听，确认超时退出而非凭据错误。仅恢复原隔离测试服务1800秒，会话9436，HTTPS监听成功；不改NAS其他服务。
- 不重启应用、不重填配置，通过真实菜单再次“立即同步”。界面出现5篇合成笔记并显示已同步，截图 `sync-recovered.png`。因使用手动触发，不宣称本次证明自动重试时序。
- 只读SQLite确认笔记5、资源5、sync_outbox为0、sync_failures为0。
- Ruby调用SQLite读取所有笔记id/title/body_html，与 `/tmp/joplin-nas-acceptance.utBFH6/source/library.sqlite` 完整比较一致；按resource_blobs.relative_path重新计算5个文件SHA256，0不匹配，命令退出0。
- 因而当前签名包已完成“界面配置—原生证书选择—保存—真实NAS TLS拉取—内容与附件校验”的测试数据闭环。仍不是常驻NAS部署、真实全库迁移、自动重试时序或整款软件交付通过。
