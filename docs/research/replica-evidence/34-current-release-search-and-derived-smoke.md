# 最新 Release 搜索与派生索引独立验收

2026-09-28；Release 包与哈希见证据33（源码 ca923f481，干净构建）。

## 实际窗口操作

隔离库 `/tmp/joplin-current-ui.Od2T2b/library` 重开后，在 PID82817 / 窗口26493 按 Cmd+K，输入“单元格复制丁”。显示1条本地结果，摘要包含该表格文字；Enter 关闭搜索面板并显示正确笔记及两格文本。截图 `search-open.png`、`search-cell.png`、`search-cell-opened.png` 均在隔离根目录。仅证明本样本中文表格文字可搜索及打开，不证明全部过滤、排序、命中高亮或大规模延迟。

## 提取器集成测试

运行 `cargo test --manifest-path packages/app-lite-gpui/Cargo.toml --test derived_live_smoke --test extractor_child`：提取器17通过；live smoke 默认2忽略。日志 `/tmp/joplin-current-integration-20260928.log`。

随后明确设置 `JOPLIN_LITE_LIVE_SMOKE_BIN` 为本次签名包内 `Contents/MacOS/joplin-lite`，运行同一 manifest 的 `--test derived_live_smoke -- --ignored --test-threads=1 --nocapture`，2通过、0失败、0忽略，6.71秒，退出0。日志 `/tmp/joplin-release-derived-smoke-20260928.log`。

已阅读测试隔离和断言：每例新建临时资料库，启动普通应用路径，通过独立数据库观察者检查真实 PDF / PNG OCR 的索引状态及中英文查询，核验笔记ID与匹配资源；测试回收自己创建的应用进程。未访问原资料库。

RSS 仅诊断采样：PDF 索引后主进程60464 KiB、观察到子进程最大13280 KiB；图片索引后主进程81504 KiB、观察到子进程最大119536 KiB。这不是全进程总峰值，也不是200块/10图产品性能门，不能用于关闭该验收门。

边界：live smoke 验证真实应用启动/提取/数据库搜索，不自动等同通过原生界面插入附件、显示搜索结果、打开附件的整条用户操作链。整款软件仍未通过交付验收。
