# 可读 HTML 全库导出独立检查

2026-09-28，隔离签名候选 `da3612738`，资料库 `/tmp/joplin-shortcuts-ui.Y7gmgX`。

实际通过 App 原生菜单“导出整个资料库为可读 HTML…”和系统保存对话框执行。产物 `/tmp/joplin-shortcuts-ui.Y7gmgX/readable-native-20260928a`，截图 `readable-export-status.png` 位于隔离资料库根目录。

独立核对结果：

- manifest 含 7 篇笔记、6 条资源；存在 index.html、notes、readable、history、resources。
- 7 个 notes 正文文件逐字对比源数据库 body_html，相同；各正文及 history 文件 SHA-256 均匹配 manifest，0 不一致。
- 6 个导出资源文件的 SHA-256 与字节数均匹配 manifest，0 不一致。两个资源记录共用同一 blob，但以各自资源 ID 导出，不能混称 6 个不同原始附件。
- readable 页面中的 6 个相对资源链接全部解析到真实存在的导出文件，0 缺失。
- 检查混排笔记的 HTML，含列表、strong/em/u 标记、相对图片链接及图片后的文字。

限制：以上证明菜单导出可执行、源正文与附件完整、链接不悬空；尚未进行浏览器视觉呈现、多媒体播放以及从此导出恢复后重新编辑的实机验收。history 校验为导出清单内部一致性，不据此宣称本轮逐条对比了源数据库全部历史版本。不等同整款产品通过。
