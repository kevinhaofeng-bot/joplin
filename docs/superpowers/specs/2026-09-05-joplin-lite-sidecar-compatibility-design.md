# Joplin Lite Sidecar 兼容层设计

日期：2026-09-05

状态：已批准进入实施

上位设计：`docs/superpowers/specs/2026-09-03-joplin-lite-design.md`

## 1. 本阶段目标

在不连接真实 Joplin profile、不访问 Joplin Server、也不改变现有客户端的前提下，建立 Joplin Lite 与官方 `@joplin/lib` 之间的第一条可测试边界：

- 一个只通过标准输入/输出通信的 Node sidecar；
- 一套覆盖同步核心 item 类型的人工脱敏夹具；
- 一个由 Rust Core 使用的、有超时和故障隔离的协议客户端；
- 官方 Joplin 序列化结果与 Rust/TypeScript 边界之间的端到端契约测试。

这不是生产同步功能，也不声称可以迁移真实笔记。它的交付价值是把以后最危险的兼容问题变成固定测试，而不是在真实资料库上试错。

## 2. 架构裁决

### 2.1 采用的方案

首版继续采用版本固定的 `@joplin/lib` sidecar，并明确数据库所有权：

- sidecar 是隔离 Joplin canonical profile 的唯一写入者，负责官方数据模型、同步、E2EE 和冲突语义；
- Rust Core 不直接写 canonical profile，只负责 sidecar 生命周期、请求调度、可重建派生状态，以及后续搜索/OCR/向量索引；
- UI 只调用 Rust 暴露的稳定领域接口，不知道背后当前是 Node 还是未来的 Rust 实现；
- 未来只允许在相同夹具与双实现对照测试通过后，逐个替换 sidecar 能力。

这一所有权边界补足上位设计中“Rust Core 负责本地数据库访问”的歧义：Rust 可以拥有自己的运行状态库和派生索引，但在首版不能绕过官方模型直接改写 Joplin canonical 数据库。

### 2.2 未采用的方案

1. **现在用 Rust 重写同步与数据模型**：体积最小，但会同时承担序列化、迁移、E2EE、资源、冲突和游标恢复风险，拒绝。
2. **Rust 自定义笔记库，sidecar 只做远端编解码**：看似分工清楚，实际会形成两套写入状态机，官方 `Synchronizer` 也无法继续拥有完整事务语义，拒绝。
3. **让 Electron/Joplin Desktop 作为后台服务**：复用度最高，但保留了本项目要消除的 Electron 常驻成本，拒绝。

## 3. 本阶段范围

### 3.1 包含

- 新增独立 workspace `packages/app-lite-sync`，固定使用当前 fork 内的 `@joplin/lib` 3.7 系列源码。
- 定义协议版本 `1` 的 NDJSON 请求/响应格式。
- 支持 `hello`、`decodeItem`、`encodeItem`、`shutdown` 四个命令。
- `decodeItem` 和 `encodeItem` 复用官方 `BaseItem`/对应模型的序列化实现，不复制一份 Rust 或 TypeScript 兼容逻辑。
- 覆盖 Note、Folder、Resource、Tag、NoteTag、MasterKey、Revision 七种同步 item。
- Rust 实现可注入 sidecar 命令的客户端：启动握手、请求关联、帧长限制、超时、进程退出和协议错误。
- 使用真实 Node sidecar 完成至少一次 Rust 到 Node 再返回的集成测试。

### 3.2 不包含

- 不打开 `~/.config/joplin-desktop` 或任何用户真实 profile。
- 不创建生产 canonical profile，不写入真实笔记或附件。
- 不连接 Joplin Server、WebDAV 或任何网络地址。
- 不实现同步、E2EE 解锁、冲突合并、资源二进制传输或 Data API。
- 不在正常 Joplin Lite 启动时拉起 sidecar。
- 不把系统 Node 当成最终发布依赖；固定 Node runtime 的打包另立计划。
- 不加入 WYSIWYG、搜索或 UI 功能；它们在兼容边界稳定后按上位设计继续实施。

## 4. 协议

### 4.1 传输

- stdin 和 stdout 使用 UTF-8 NDJSON；每个 frame 必须以 LF 结束，EOF 前没有 LF 的剩余字节属于截断 frame，不能作为请求或响应处理。
- stdout 只允许协议 frame；诊断只写 stderr。
- 每个输入和输出 wire frame 最大 `8 MiB`，上限包含终止 LF；若接受 CRLF 输入，CR 和 LF 两个原始字节都计入上限。资源二进制永远不通过该协议内联传输。
- 两端必须先在原始字节流上分帧和限长，再用 fatal UTF-8 解码；不得使用会替换非法字节、接受 EOF 尾段或在遇到换行前无限缓存的宽松行读取器。
- 缓冲区在尚未出现 LF 时一旦达到 `8 MiB`，由于再追加终止 LF 必然超限，必须立即返回固定错误并关闭该流，不得等待 EOF 或继续积累。
- 一旦收到非法 UTF-8、超长 frame、截断 frame 或无法解析的 JSON，sidecar 返回可关联时尽量关联的稳定错误；无法安全继续分帧时退出。
- 第一版 Rust 客户端串行发送请求；协议保留 `id`，以后可以在不破坏格式的前提下增加并发。

### 4.2 请求与响应

请求必须包含：

```json
{"id":"request-1","protocolVersion":1,"command":"hello","params":{}}
```

成功响应：

```json
{"id":"request-1","ok":true,"result":{"protocolVersion":1,"joplinVersion":"3.7.0"}}
```

失败响应：

```json
{"id":"request-1","ok":false,"error":{"code":"INVALID_REQUEST","message":"请求格式无效"}}
```

错误消息必须是固定、脱敏的中文文本，不得拼入原始 frame、笔记正文、路径、token、密码或密钥内容。详细堆栈只允许进入测试捕获或开发 stderr，并同样不得包含请求正文。

### 4.3 命令

- `hello`：返回协议版本、Joplin 库版本和能力列表，用于拒绝版本不匹配。
- `decodeItem`：接收 `raw` 字符串，调用官方反序列化，返回 JSON item。
- `encodeItem`：接收 JSON `item`，调用该类型的官方普通序列化；本阶段不调用需要 E2EE/ShareService 的 `serializeForSync`。
- `shutdown`：完成当前响应后正常退出，退出码为 0。

未知命令返回 `UNKNOWN_COMMAND`；协议版本不等于 `1` 返回 `PROTOCOL_MISMATCH`；官方解析拒绝的 item 返回 `INVALID_ITEM`；超时和进程退出由 Rust 映射为 `TIMEOUT` 与 `SIDECAR_EXITED`。

## 5. 兼容夹具

夹具全部为人工构造，不得复制用户真实笔记。每个 fixture 使用固定 32 位十六进制 ID、固定毫秒时间戳和可辨识的测试内容，至少验证：

- 七种同步 item 都能被官方实现解码；
- 解码后再编码、再解码，约定字段保持相等；
- Note 的多行正文、内部资源链接、待办字段和时间戳不丢失；
- Resource 只测试元数据，二进制不进入 NDJSON；
- Revision 的 diff 字段按官方规则保留；
- decode 和 encode 两个入口都必须通过官方校验路径拒绝非 32 位十六进制 ID 和包含路径穿越的资源扩展名；不得在本适配器复制一套字段校验规则；
- 错误响应中找不到 fixture 正文或恶意输入原文。

比较采用“规范字段等价”，不要求属性文本顺序以外的无意义字节完全相同。fixture 清单显式写出每种类型需要比较的字段；任何字段删减都必须修改清单和测试。

## 6. Rust 客户端边界

Rust 模块只接受由调用者注入的可执行程序、参数和工作目录，便于测试和以后接入 Tauri resource。它不得通过 shell 拼接命令。

状态机为：

```text
Stopped -> Starting -> Ready -> Stopping -> Stopped
                    \-> Failed
Ready --timeout/protocol error/process exit--> Failed
```

- 启动后第一条请求必须是 `hello`，并在 `5 秒`内完成。
- 普通请求默认超时 `10 秒`；测试可注入更短超时。
- 单个响应 wire frame（含终止 LF）超过 `8 MiB`、包含非法 UTF-8 或在 EOF 前缺少 LF，立即终止子进程并进入 `Failed`。
- `Drop` 和显式关闭都要回收子进程；显式关闭给予协议响应和自然退出共用的 `5 秒`宽限期，不得开启第二个完整宽限窗口；到期后立即 kill，并继续完成有界回收。该宽限期不被描述为操作系统 kill/reap 的绝对墙钟硬上限。
- 对外错误只暴露稳定枚举和脱敏消息；不得把 stderr 或原始 JSON直接传给 UI。
- 本阶段不在 Tauri `run()` 中注册或自动启动该客户端，避免基础壳无意获得新副作用。

## 7. 文件与职责

- `packages/app-lite-sync/src/protocol.ts`：frame 类型、版本、大小限制和纯验证函数。
- `packages/app-lite-sync/src/codec.ts`：唯一允许接触官方 Joplin item 序列化 API 的适配器。
- `packages/app-lite-sync/src/server.ts`：stdin/stdout 循环、命令分发和脱敏错误映射。
- `packages/app-lite-sync/src/main.ts`：最小进程入口，不放业务逻辑。
- `packages/app-lite-sync/fixtures/v1/`：人工 raw item 与字段清单。
- `packages/app-lite/src-tauri/src/sync_sidecar/`：Rust frame、客户端、状态和错误。
- 两个 workspace 各自保留单元测试；跨语言集成测试放在 Rust 模块下。

## 8. 验证与退出门槛

本阶段完成必须同时满足：

1. TypeScript 测试覆盖四个命令、七种 item、畸形输入、超长输入和脱敏错误。
2. Rust 测试覆盖握手、正常请求、ID/版本不匹配、超时、提前退出、超长响应和关闭回收。
3. 跨语言集成测试实际启动 `packages/app-lite-sync`，让 Rust 对 Note fixture 完成 decode/encode 往返。
4. `tsc`、Rust `fmt`、Clippy 和两个 workspace 的测试通过。
5. 现有 app-lite 前端/Rust 测试不回归，基础应用启动仍不自动拉起 Node。
6. 代码搜索证明本阶段没有真实服务 URL、token、用户 profile 路径或网络客户端。

通过后，下一计划才允许创建 sidecar 所拥有的隔离 canonical profile，并实现本地 Note/Folder/Tag CRUD。生产 Joplin Server 同步仍需在 CRUD、资源与冲突契约成熟后单独开启。
