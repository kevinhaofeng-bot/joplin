# IME 独立检查与复刻前提差异

2026-09-28。

## IME

审查提交 `eea1c0f99`：取消从光标起始的组合输入，丢弃无效撤销步骤；若组合输入替换了原选区，则保留撤销步骤以恢复被删除文本。

独立执行两项，均退出 0、各 1 通过：

- `a_cancelled_composition_leaves_undo_as_it_found_it`，日志 `/tmp/joplin-ime-cancel-independent.log`。
- `body_pinyin_composition_survives_autosave_and_commits_once`，日志 `/tmp/joplin-ime-autosave-independent.log`。

执行时工作区还含输入跟踪功能改动；不是仅 checkout 该提交的独立构建。测试通过不代表之前实机的异常字符已定位，更不代表当前运行的旧候选含此修复。输入跟踪应默认关闭，只对隔离合成内容启用；已要求 Claude 为新建文本日志设置 Unix 0600 权限。

随后提交 `386849488` 提供可选跟踪，`c428952b4` 为新日志设置 0600。主代理独立执行 `input_trace_records_body_composition_and_keys_reaching_the_app`（日志 `/tmp/joplin-input-trace-independent.log`）以及 `input_trace::tests::a_new_trace_file_is_private_to_its_owner`（日志 `/tmp/joplin-trace-permissions-independent.log`），各 1 通过、退出 0。源码未配置 `JOPLIN_LITE_INPUT_TRACE` 时不创建日志，事件详情闭包也不求值；新建文件的权限测试通过不代表已有文件会自动改权限。后续实机只使用全新私有临时目录中的合成输入。

## Evernote 实现差异：不能视为已完成复刻

主代理实际重新读取：`/Users/kevinhao/Projects/joplin-reconstruction/evernote-11.32.5/common-editor-sourcemap/@evernote/common-editor/src/apps/peso/modules/list/list.ts`。

`insertListAtSelection` 保留 stored marks、按选区切换/移除列表；`insertOrToggleList` 对非文本块和文本块分别处理，将段落及资源纳入列表转换，然后修改已有列表项类型。

本项目 `native_editor/commands.rs::CommandCatalogue::state` 对包含非文本块的选区禁用列表等命令，行内格式命令也要求 text_only_selection。证据01已记录图片不属于列表项的“独立设计”，但用户没有批准以此永久缩小复刻目标。

因此：纯文本列表转换通过不证明图文选区的 Evernote 行为已实现。此处已明确交给 Claude 后续评估及施工，要求源文件/符号→行为→实现→保存与撤销测试映射；不能直接把现有禁用行为登记为交付通过。行内格式的具体 Evernote 规则还需另读对应实现，不能从列表源码推断。
