# 原生输入法实机跟踪观察

2026-09-28。只使用新私有临时目录 `/tmp/joplin-ime-ui.ucFdaI` 下的合成资料库 `library`，不加载原始笔记。

诊断包 `/tmp/joplin-ime-candidate/20260928T090516Z-8354c14c0/Joplin Lite.app`，二进制 SHA-256 `d9acc760dabed0297cfb6c97c20e3740a3e766f1e103fc773d0ae488553ec0fa`。整包签名校验通过，但构建记录为源码 dirty，期间有输入日志权限提交，因此仅是诊断包，不登记为精确干净源码的交付候选。

启动时仅对该进程设置 `JOPLIN_LITE_INPUT_TRACE=/tmp/joplin-ime-ui.ucFdaI/input.jsonl`。实际文件权限为 0600。

## 系统键盘事件与实际 IME 回调

通过原生菜单新建笔记；保持现有输入法配置不变。显式清空 CGEvent modifiers，以物理键码 n/i/h/a/o 每键约 200ms 输入。

1. 标题依次收到 marked 文本 n → ni → ni h → ni ha → ni hao。
2. Space 提交 `replace_text_in_range("你好")`；Return 后明确点击正文获得焦点。
3. 正文依次收到相同 marked 回调，Space 提交 `replace_text_in_range("你好", marked="ni hao")`，Cmd-S 保存。
4. SQLite 实际记录：标题 `你好`，正文 `<p>你好</p>`。

正文第一次 n 同时有 key_down 和 marked 回调；没有因此多出一个 n。不能仅凭看见 key_down 就判定泄漏或重复输入。

截图 `title-composition.png`、`body-composition.png`、`body-committed.png`，日志 `input.jsonl` 均位于上述私有目录。观察完成后通过菜单正常退出诊断进程 PID78735，避免继续收集其他输入。

## 判定边界

本轮实机基本拼音组合/提交/保存路径正常，没有复现之前的“8泥豪7/nihaou”。这说明此前截图不足以认定产品根因，不能反过来宣称所有 IME 问题已修复。仍需干净最终候选、图片前后及切换笔记/工具栏/取消组合/长文等完整矩阵；系统事件注入也不等同用户物理键盘的全面验证。
