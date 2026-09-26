# 原生导出入口与图片插入修复验收

## 范围

在既有 Rust/GPUI 原生资料库加入当前笔记导出入口；不改变 canonical HTML/SQLite 存储、不替换稳定保存链。导出仍仅支持默认笔记本、活动、无标签的选中笔记，不能称作全库备份。另修复编辑工具栏图片按钮的 GPUI Emit/dispatch 时序错误。

## 已核验

- GPT-6 Sol 完成实现和图片修复；独立审查通过。导出 Picking 阶段的资源失败遗漏已补回归并关闭。
- 图片图标先排队发出准备插入事件，再 defer 打开选择器，避免在事件尚未处理和窗口仍借用时分发。测试只替换最终 OS 面板边界，不再跳过生产 dispatcher。取消与平台错误分开处理。
- 控制器独立图片相关 11 项、顶栏点击 1 项通过；本轮原生导出 8 项通过；fmt/diff 检查通过。Release 构建成功，仍有既有 warnings。
- 实机图片图标和顶部插入资源均弹出 NSOpenPanel；选择 PNG 后，当前画布和缩略图立即显示，不需要切换笔记。
- 实机原生菜单“导出当前笔记…”弹出保存面板；取消后可再次发起并成功导出 1 篇笔记、2 个图片资源。界面显示成功路径与不能替代全库备份的范围说明。
- 导出目录 `/tmp/joplin-lite-export-accept.RLn08F43/ui-export-verified` 包含 manifest、canonical notes、readable 页面、index 和资源。1 篇正文 SHA-256、2 个资源大小及 SHA-256 均与 manifest 一致；readable 引用的本地资源存在。
- 正常 Cmd-Q 后以同一隔离 profile 重开，原 3 条笔记保留，选中的笔记仍显示两张图片。

## 运行身份与回退

测试 App：`/tmp/joplin-lite-export-accept.RLn08F43/JoplinLiteExportTest.app`。

显式 profile：`/tmp/joplin-lite-export-accept.RLn08F43/profile`。不是正式安装或个人资料库迁移；此目录已有测试内容，不得当空目录清理。

测试二进制 SHA-256：`2bf95edc684fa011f690cab5a3b798cda5dcc67507f6eae56a99afac5f0c7693`。旧测试二进制备份为同父目录 `velotype-before-picker-fix`。本轮重新启动 session 64408。

## 未完成与限制

- 浏览器工具明确拒绝 file:// 页面，并禁止换工具或间接方式绕过；因此没有转成本地 HTTP 服务。浏览器实际渲染仍未验收，静态路径/哈希检查不能替代它。
- 尚未实机覆盖“新编辑后立即导出”的竞态；对应自动化测试已覆盖。
- 完整测试进程曾在多线程和单线程 SIGSEGV 中断，不能声称全套通过。正在单独定位，不以此重写保存或编辑器。
- 未宣称 Task 8、全库备份/恢复、NAS 同步、真实资料库迁移或整款产品完成。
