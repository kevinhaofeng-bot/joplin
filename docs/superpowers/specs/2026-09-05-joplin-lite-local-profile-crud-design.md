# Joplin Lite 独立资料库与本地 CRUD 设计

日期：2026-09-05

状态：架构已定稿，待分任务实施

上位设计：`docs/superpowers/specs/2026-09-03-joplin-lite-design.md`

冻结基线：`docs/superpowers/specs/2026-09-05-joplin-lite-sidecar-compatibility-design.md`

## 1. 阶段目标

本阶段把已经验证的 Joplin 官方编解码 sidecar 扩展为一个只操作临时、隔离资料库的本地数据服务，打通第一条真实但无网络的领域链：

```text
Rust 测试客户端 -> NDJSON sidecar -> 官方 Joplin Model -> 临时 database.sqlite
```

交付范围是：

- 安全接管一个归属于 Joplin Lite 的独立 canonical profile；
- 通过当前 fork 的官方 `@joplin/lib` 创建和迁移 SQLite 数据库；
- 提供 Note、Folder、Tag 及 Note-Tag 关系的最小本地 CRUD；
- 用乐观并发、幂等创建、单写锁和有界协议保护数据；
- 从 Rust 通过稳定领域 DTO 完成跨进程、跨语言、重启后仍可读取的集成测试。

本阶段仍不连接 Joplin Server，不启用同步和 E2EE，不读取现有用户资料库，也不把 sidecar 接入普通 Tauri 启动路径。它证明的是“能安全地拥有并修改一个隔离 Joplin 资料库”，不是生产迁移完成。

## 2. 冻结架构边界

### 2.1 单一 canonical 写入者

- Node sidecar 是 canonical Joplin profile（`database.sqlite`、`resources/` 和 Joplin 设置）的唯一写入者。
- canonical 数据必须通过当前 fork 的官方 `JoplinDatabase`、`Note`、`Folder`、`Tag`、`NoteTag`、`ItemChange` 等实现写入。
- Rust Core 不直接写 canonical SQLite，不复制 Joplin 表结构、迁移、时间戳、删除标记或 item-change 规则。
- Rust 可以拥有自己的运行状态库和以后可重建的搜索/OCR/向量索引；这些派生数据不进入 canonical 数据库。
- UI 以后只调用 Rust 暴露的领域接口，不感知 Node 类名、Joplin 下划线字段或 SQL。

这是对上位设计中“Rust Core 本地数据库访问”的收敛：Rust 只直接拥有非 canonical 数据；在官方兼容实现被固定夹具和双实现测试替代以前，canonical 写入权不迁移。

### 2.2 冻结的 sidecar 基线

以下能力不得在本阶段回退或改写：

- protocol version `1`、UTF-8 NDJSON、每帧含 LF 最大 `8 MiB`；
- 原始字节分帧、fatal UTF-8、拒绝 EOF 尾帧和超长无分隔输入；
- `hello`、`decodeItem`、`encodeItem`、`shutdown` 及七类人工 fixture；
- Rust 串行请求、启动/请求/关闭超时、子进程回收和固定脱敏错误；
- 普通 Joplin Lite 启动不自动启动 sidecar，也不新增 Tauri command；
- 测试不访问真实 profile、真实笔记、真实 token 或网络。

新命令是 version 1 的向后兼容能力扩展，通过 `hello.capabilities` 探测；不改变现有 frame 形状。

## 3. Profile 所有权与路径防线

### 3.1 可接受的根目录

`openProfile` 只接受同时满足以下条件的路径：

1. 是绝对路径，最后一个组件精确等于 `com.kevinhao.joplin-lite`；
2. 路径任一组件大小写不敏感地都不等于 `joplin-desktop`；
3. 最近存在父目录的 canonical path 同样不包含 legacy 组件；
4. profile 根不是符号链接；
5. 已存在的 `resources`、`indexes`、`logs`、`tmp`、`cache` 必须是根下的真实目录而非符号链接；
6. 已存在的 `database.sqlite`、`database.sqlite-journal`、`database.sqlite-wal`、`database.sqlite-shm`、`settings.json` 和所有权标记必须是普通文件而非符号链接；
7. 同级 OS lease 文件由 Rust 使用 `O_NOFOLLOW` 独立验证；sidecar 在 SQLite open 紧邻前和完成后重复检查根、数据库主文件与辅助文件，检测到替换就关闭并失败。

Rust 现有 `ProfilePaths` 仍负责从 Tauri app-data 位置派生路径和第一次检查；sidecar 在真正写盘前独立复核，形成跨进程的纵深防御。

### 3.2 所有权标记

profile 根使用 `.joplin-lite-profile.json` 标记，内容固定为：

```json
{"owner":"com.kevinhao.joplin-lite","formatVersion":1}
```

- 新标记使用排他创建；不得先检查再覆盖，也不得跟随符号链接。
- 已有标记必须是普通文件并严格匹配 owner 和 formatVersion；否则返回 `PROFILE_NOT_OWNED`。
- 无标记目录只允许在它恰好包含空的 `resources/`、`indexes/`、`logs/` 三个脚手架目录时被接管。
- 任一脚手架目录不为空，或根中已经存在数据库、设置、未知文件/目录时，拒绝接管。
- 绝不把已有但无标记的 Joplin 数据库“猜测”为 Joplin Lite profile。

这条规则允许 Rust 基础壳已经创建的空目录被安全升级，同时阻止路径配置错误时收编其他应用的数据。

生产调用方必须先通过现有 Rust `ProfilePaths::ensure()` 建立这三个空脚手架目录；`openProfile` 不接管不存在的根或完全空的任意目录。测试也显式建立同样脚手架。排他创建标记时若进程中断而留下无效/部分文件，后续按 fail closed 处理，不自动猜测修复。

### 3.3 单写租约

不采用基于 mtime/PID 的 stale 目录回收。即使先读 token 再执行原子 rename，仍存在 ABA 窗口：后到回收者可能把刚建立的新 owner 锁搬走，最终产生两个 canonical 写入者。

本项目首发目标是 macOS，因此由 Rust supervisor 在启动 profile sidecar 前获取 Darwin/BSD `flock` advisory lease：

- lease 文件放在 profile 根的同级目录，名称固定为 `.com.kevinhao.joplin-lite.canonical.lock`，避免在所有权标记建立前改动待接管目录；
- 以 `O_NOFOLLOW | O_CREAT | O_RDWR` 和 `0600` 打开，拒绝 symlink 和非普通文件；
- 精确使用 `flock(fd, LOCK_EX | LOCK_NB)`；不得替换为不满足本方案 fork/dup 生存期语义的 `fcntl(F_SETLK)`；竞争失败映射为 `PROFILE_IN_USE`，不等待、不删除、不回收任何路径；
- Rust 原 FD 从创建起设置 `O_CLOEXEC`，避免多线程 supervisor 同时 spawn 其他进程时泄漏租约；仅在目标 sidecar 的 `pre_exec` 中把同一 open-file-description 复制到固定的高编号 child FD 并清除该副本的 CLOEXEC，原 FD 已等于目标 FD 时也必须正确处理；通过只含 FD 编号的环境变量告诉 sidecar，不得通过 shell 或路径 token 模拟租约；
- Rust client 自身保留一份 FD，Node sidecar 也在整个 profile 会话中保留继承的 FD；只有两端都关闭或进程退出时内核才释放 lease；
- Node 在开库前用 `fstat(inheritedFd)` 与 `lstat(expectedSiblingLockPath)` 核对 device/inode、普通文件和 profile 对应关系；没有继承租约或核对失败返回 `PROFILE_LOCK_REQUIRED` 并退出。Node 不具备独立证明 FD 当前持有 flock 的能力；“只有受信 Rust supervisor 能启动 profile sidecar 并传入 FD”是本阶段明确的本地信任边界；
- Rust 和 Node 都不得在共享 open-file-description 上提前调用 `LOCK_UN` 或会自动 unlock 的 guard；显式 unlock 会解除双方共享的 lease。子进程启动、握手、open、请求、shutdown、EOF、协议错误和 Drop 的所有退出路径只能关闭各自 FD；最后一份 FD 关闭或异常崩溃后由内核回收，不存在 stale 文件“看起来还锁住”的问题；
- lease 文件可以永久保留为空控制文件，是否被锁只由内核状态决定；任何流程都不得 unlink 或 rename 它。

现有无 profile 的 codec sidecar 仍可通过普通 `SidecarClient::start` 启动，但 `openProfile` 必须由新增的 `SidecarClient::start_for_profile` 路径提供继承租约。Windows 支持另立计划；本阶段不以不安全的目录锁冒充跨平台实现。

## 4. 官方 Joplin 运行时初始化

打开 profile 的次序固定为：

1. 核对 Rust 传入的 OS lease，再执行路径、symlink 和标记检查；
2. 初始化无 stdout 输出的固定脱敏 logger，并安装 Node filesystem driver；
3. 使用 sidecar 自己直接声明的 `sqlite3@5.1.6` 初始化 Joplin shim，并注册已有七类 item；
4. 设置 `Setting` 的 `appId`、`appName`、`appType`、`env`、`profileDir`、`rootProfileDir`、`resourceDir`、`tempDir`、`cacheDir` 等必需常量；
5. `new JoplinDatabase(new DatabaseDriverNode())` 并打开根下 `database.sqlite`，由官方 `initialize()` 运行 schema 创建或迁移；
6. 将数据库注入 `BaseModel` 和 `reg`，加载不使用系统 keychain 的本地 Settings；
7. 明确执行 `BaseItem.revisionService_ = RevisionService.instance()`，使 `Note.save` 的历史和 ItemChange 路径可用；
8. 成功后才把会话状态设为 `open`。

Joplin 全局单例很多，因此一个 sidecar 进程一生最多打开一个 canonical profile：

- 未打开时 CRUD 返回 `PROFILE_NOT_OPEN`；
- 已打开后再次 `openProfile`，即便路径相同也返回 `PROFILE_ALREADY_OPEN`；
- 不支持在同一进程内关闭 A 再切换到 B；切换必须退出并启动新进程；
- Jest 保持 `runInBand`，每个 profile 集成测试使用独立子进程和独立临时目录。

`registerItemClasses()` 可能先为纯 codec 建立内存数据库。profile 初始化必须在注册之后把 `BaseModel` 和 `reg` 重新绑定到真实 JoplinDatabase；测试覆盖 `hello→open→codec→CRUD` 和 `hello→codec→open→CRUD` 两种顺序，确保旧 codec 能力不会把 DB 切回内存实现。

## 5. 命令与领域 DTO

### 5.1 会话命令

- `profileStatus {}`：返回 `{ state: "closed" | "open", formatVersion: 1 }`，不返回路径。
- `openProfile { profilePath }`：验证并打开资料库，返回 `{ state: "open", schemaVersion, formatVersion: 1 }`。
- `shutdown {}`：停止接收后续请求，刷盘并关闭会话，再返回成功 frame 后自然退出。

`hello.capabilities` 在原三项基础上按固定顺序追加本阶段命令。

### 5.2 DTO 字段

所有外部字段使用 camelCase 固定 allowlist，禁止直接返回 model blob、SQL row 或调用方指定字段名。

```text
FolderDto:
  id, parentId, title, createdTime, updatedTime, deletedTime

TagDto:
  id, title, createdTime, updatedTime, noteCount

NoteSummaryDto:
  id, parentId, title, isTodo, todoDue, todoCompleted,
  createdTime, updatedTime, userCreatedTime, userUpdatedTime, deletedTime

NoteDetailDto:
  NoteSummaryDto + body, markupLanguage, tagIds
```

`isTodo` 使用 JSON boolean；`todoDue`、`todoCompleted` 和全部时间字段使用非负整数毫秒；`markupLanguage` 是 `"markdown" | "html"`，本阶段 create 只产生 `"markdown"`。列表默认不包含已删除项目。笔记列表绝不包含正文；只有 `getNote` 返回正文。DTO 不返回 encryption ciphertext、share metadata、应用内部 order、正文摘要或任意未知字段。

mutation 响应形状固定为：

```text
createFolder/createTag/createNote -> { item: <Dto>, created: boolean }
updateFolder/updateTag/updateNote -> { item: <Dto>, changed: boolean }
trashFolder/trashNote             -> { id, deletedTime }
deleteTag                         -> { id, deleted: true }
setNoteTags                       -> { noteId, tagIds, updatedTime, changed }
```

update 请求完全没有提供可变字段时返回 `VALIDATION_FAILED`；提供了字段但规范化后与当前值相同时，在并发令牌匹配后返回原 DTO 和 `changed:false`，不推进时间戳、不产生写入。

### 5.3 Folder 命令

- `listFolders {}`：返回全部活动 folder 的扁平 DTO；客户端按 `parentId` 建树。
- `createFolder { id?, parentId, title }`
- `updateFolder { id, expectedUpdatedTime, title?, parentId? }`
- `trashFolder { id, expectedUpdatedTime }`

`parentId` 为空字符串表示根。非空 parent 必须存在且未删除；移动时调用官方 `Folder.canNestUnder`，拒绝自身或后代环。`trashFolder` 使用官方递归进回收站语义，连同子 folder 和其中 note 一起标记删除，不做永久删除。

### 5.4 Tag 命令

- `listTags {}`：返回活动 tag 及 noteCount。
- `createTag { id?, title }`
- `updateTag { id, expectedUpdatedTime, title }`
- `deleteTag { id, expectedUpdatedTime }`
- `setNoteTags { noteId, expectedUpdatedTime, tagIds }`

标签标题使用官方 trim、NFC 和大小写冲突校验。删除调用官方 `Tag.untagAll`，先解除关系再生成 tag 删除语义。`setNoteTags` 只接受已有 tag ID，输入先去重并完整校验 note/tag 存在，再调用官方 `Tag.setNoteTagsByIds`；它不隐式创建标签。关系更改成功后 note 的 `updated_time` 必须单调增加，使 UI 的并发令牌和以后同步的变更可见。

### 5.5 Note 命令

- `listNotes { parentId?, page?, limit? }`
- `getNote { id }`
- `createNote { id?, parentId, title, body, isTodo?, todoDue? }`
- `updateNote { id, expectedUpdatedTime, title?, body?, parentId?, isTodo?, todoDue?, todoCompleted? }`
- `trashNote { id, expectedUpdatedTime }`

列表排序固定为 `updated_time DESC, id ASC`，`page` 从 1 开始，`limit` 默认为 50、最小 1、最大 100；返回 `{ items, page, hasMore }`。`parentId` 省略表示所有活动 note；提供时必须是已有活动 folder。

创建 note 必须指定已有活动 folder。正文是 Joplin Markdown canonical 文本；所见即所得结构模型和往返保护属于下一阶段，不在这里引入第二种持久化格式。

## 6. 写入正确性

### 6.1 输入 allowlist 与官方校验

- 每个命令只接受文档列出的键；未知键、错误类型、空 ID、非有限时间戳和越界分页都返回 `VALIDATION_FAILED`。
- 调用方提供的 ID 必须是 32 位小写十六进制，并继续交给官方 `userSideValidation` 复核。
- title 长度、NUL 字节、加密项目不可编辑、保留 folder 名称等规则不自行放宽；所有保存调用使用 `{ userSideValidation: true }`。
- create/update 不接受 `created_time`、`updated_time`、encryption、share、deleted、sync 等内部字段。
- 任何公开错误都不能拼入 title、body、tag、路径、SQL、token、密钥或底层异常消息。

### 6.2 幂等创建

所有 create 命令允许调用方提供 ID：

- ID 不存在时，使用该 ID 创建；
- ID 已存在且 create 的 canonical 业务字段完全相同，返回已有 DTO，并带 `created: false`；
- ID 已存在但字段不同，返回 `CONFLICT`；
- 未提供 ID 时由官方模型生成，返回 `created: true`。

相同内容的判断只比较该 create 命令拥有的字段，不比较自动时间戳和内部字段。这样 Rust 在未收到响应时可以用相同 ID 安全重试，而不会重复创建。

官方 `BaseModel.isNew()` 默认只检查对象有没有 `id`，不会查询数据库。因此确定 ID 的新建分支在确认不存在后必须显式传 `{ isNew: true, userSideValidation: true }`；更新分支显式传 `isNew: false`。幂等比较前先按官方规则规范化输入（包括 tag trim/NFC、folder 前导斜线和模型 filter），并在响应前重新 load；测试必须证明数据库真实插入、默认字段存在且重启后可读，不能只相信 save 返回对象。

### 6.3 乐观并发

update、trash、delete、setNoteTags 都要求 `expectedUpdatedTime`：

1. 在 mutation 紧邻执行前重新加载目标；
2. 不存在或已删除返回 `NOT_FOUND`；
3. 当前 `updated_time` 与调用值不相等返回 `CONFLICT`，不写任何内容；
4. 对仍保留项目的 update 和 `setNoteTags`，新 `updated_time = max(Date.now(), oldUpdatedTime + 1)`；
5. 用官方 save 且 `autoTimestamp: false`；title、body、todo 等用户内容变化同时把 `user_updated_time` 推进到同一新值，仅移动 folder 或改变 tag 关系时保留旧 `user_updated_time`；
6. `trashNote`、`trashFolder` 和 `deleteTag` 保留官方 delete/trash 时间戳与递归语义，不宣称 `old + 1`；它们仍在执行前检查并发令牌，成功后以 `deletedTime` 或 `{ deleted: true }` 作为终态。

sidecar 串行处理命令，因此 compare 与 save 之间没有本进程内的并发写入。以后同步进入同一 sidecar 时也必须复用同一串行调度器；在此之前不得并行开放第二条 canonical 写入路径。

### 6.4 ItemChange 与刷盘

- 每个成功 mutation 在响应前 `await ItemChange.waitForAllSaved()`，避免 `Note.save` 已排队的后台 change 尚未完成就返回或关库。
- 官方 `Note.save` 会产生 note `item_changes`；成功等待后必须查询本次 note 的 change type 与 ID，作为附加落盘证据。
- `Folder.save`、`Tag.save` 和 `NoteTag.save` 不产生对应 `item_changes`，不得为了统一测试而伪造；它们的同步变化由 canonical item 的 `updated_time` 和官方 delete 时的 `deleted_items` 等语义表达。
- `ItemChange.waitForAllSaved()` 本身不传播后台 `add` 失败。profile 会话安装一个 sidecar 范围的 promise/error tracker，包装 `ItemChange.add`：立即接住被 `Note.save` 丢弃的 rejection、记录首个固定分类错误并跟踪 pending promise，关闭时恢复原方法。错误内容不写日志、不进入协议。
- 每次 Note mutation 前先完成上一 barrier 并读取 `ItemChange.lastChangeId()`；mutation 后等待 tracker 和官方 wait，再用 `changesSinceId(barrierId)` 要求本次涉及的每个 note 都存在 `id > barrierId` 且 item_id/item_type/type 正确的记录。不得只查询“该 note 曾经有记录”，也不得用可冻结或重复的时间戳判断新旧。递归 trash 要覆盖受影响的全部 note。
- tracker 捕获失败、缺少本次新 change 或 change 类型错误时返回 `STORAGE_ERROR` 并终止；失败注入必须先制造一条旧 UPDATE，再让下一次 UPDATE 的 ItemChange 事务失败，证明旧记录不会被误认成新证据。
- 多项目官方操作（递归 trash、tag 关系更新）完成后再统一等待；若官方调用报错，返回固定失败，不声称原子回滚整个多步操作。
- `shutdown` 依次停止新命令、等待现有 mutation、等待 ItemChange、保存 Settings、关闭数据库并关闭继承的 lease FD；Rust 在子进程退出/回收后关闭自己持有的 FD。
- 关闭中任一步失败都返回固定 `STORAGE_ERROR`，仍尽最大努力关闭和释放自己的资源，然后退出非零；不得继续接受请求。

## 7. 错误模型

sidecar 新增以下稳定领域错误：

| code | 固定中文消息 | sidecar 是否继续可用 |
|---|---|---|
| `PROFILE_NOT_OPEN` | 资料库尚未打开 | 是 |
| `PROFILE_ALREADY_OPEN` | 资料库已经打开 | 是 |
| `PROFILE_INVALID` | 资料库路径无效 | 是 |
| `PROFILE_NOT_OWNED` | 资料库不属于 Joplin Lite | 是 |
| `PROFILE_IN_USE` | 资料库正在被使用 | 是 |
| `PROFILE_LOCK_REQUIRED` | 资料库写入租约无效 | 否，进程退出 |
| `PROFILE_OPEN_FAILED` | 无法打开资料库 | 否，进程退出 |
| `NOT_FOUND` | 项目不存在 | 是 |
| `VALIDATION_FAILED` | 输入内容无效 | 是 |
| `CONFLICT` | 项目已被其他操作修改 | 是 |
| `STORAGE_ERROR` | 无法保存资料库 | 否，进程退出 |

Rust 必须把已知领域 code 映射到独立 `SidecarErrorKind`，不得全部降格为 `InvalidResponse`。表中“是”的可恢复领域错误不改变 `Ready`；`PROFILE_LOCK_REQUIRED`、`PROFILE_OPEN_FAILED`、`STORAGE_ERROR` 是终止性领域错误，返回失败 frame 后 sidecar 清理并退出，Rust 进入 `Failed` 并完成回收。未知 code、无效 frame、ID 不匹配和结构不合法也是协议错误并终止 sidecar。

错误响应中的 sidecar message 只用于协议一致性校验；UI 最终展示 Rust 自己的固定中文映射，不透传 Node 文本。

`shutdown` 成功必须同时满足：响应严格等于 `{ stopped: true }`、sidecar 在同一 5 秒预算内自然退出、退出码为 0。成功 frame 后退出码非零、成功 frame 后挂住、关闭失败 frame 或 EOF/stdio 异常都必须报告失败并回收；server 在所有终止路径的 `finally` 中尝试关闭 profile，不能只在显式 shutdown 时释放数据库和 lease。

## 8. 测试边界

### 8.1 只允许人工临时数据

- 每个集成测试创建自己的临时父目录和精确 basename profile；
- 内容使用固定英文/中文人工文本，不复制真实 note 或附件；
- 测试结束关闭 sidecar 并删除该精确临时目录；
- 源码和测试禁止出现 `~/.config/joplin-desktop`、真实服务器 URL、token、用户名或用户正文；
- 本阶段禁止任何网络模块调用，普通 Tauri 启动仍无 sidecar 副作用。

### 8.2 必须覆盖

1. 干净 install 后单命令构建必要上游和 sidecar 直接依赖的 sqlite3 native binding，再完成 sidecar 测试和 tsc；受控测试必须先移除该 binding，不能靠旧工作树掩盖。
2. 新空脚手架成功写标记、建库、迁移；重启后原数据可读。
3. marker 不匹配、未知无标记数据库，以及 root/database/SQLite side-file/resources/tmp/cache symlink 均拒绝且不改盘。
4. 两个 Rust-supervised sidecar 竞争同一 profile 时只有一个成功；异常杀死 Rust、Node 或两者后由内核自动释放 lease；lease 控制文件从不被删除或重命名。
5. profile 未打开、重复打开和打开失败错误稳定且脱敏。
6. Folder 根/子级创建、重命名、移动、防环和递归 trash。
7. Tag 创建、NFC/大小写冲突、重命名、关联替换和删除解关联。
8. Note 创建、无正文列表、详情正文、分页、编辑、移动、待办字段和 trash。
9. client ID 以 `isNew:true` 真实插入、幂等重试与同 ID 异内容冲突。
10. 每类 stale `expectedUpdatedTime` 都不写入，并返回 `CONFLICT`；冻结时钟覆盖用户时间戳和官方 trash 语义。
11. Note mutation 后存在对应 ItemChange；Folder/Tag/NoteTag 按各自官方 updated/deleted 证据验收；shutdown 后 SQLite 可由新进程打开。
12. Rust 真实启动 Node，完成 open→Folder→Tag→Note→set tags→update→shutdown→restart→read。
13. 所有失败响应和 Rust 错误中找不到 fixture 正文、恶意路径和 secret marker。

依赖准备阶段允许 Yarn 使用其正常缓存/官方包下载渠道安装 sqlite3 预构建产物；sidecar 测试与运行阶段本身必须在网络禁用或网络调用被 fail-fast 拦截时通过。若预构建不可用，文档必须列出本机编译器和匹配 Node headers 的离线回退前提。最终应用发布仍需把固定 Node runtime 与匹配的 sqlite3 binding 一起打包，不能要求用户现场编译。

## 9. 本阶段退出门槛

只有同时满足以下条件才可进入 UI/所见即所得阶段：

- clean bootstrap、Node 单元/集成测试、TypeScript、Rust 单元/跨语言测试、fmt、Clippy 和现有 app-lite 测试全绿；
- 原生 Tauri 构建仍成功，普通应用启动不拉起 Node，不打开数据库，不访问网络；
- profile 所有权、锁、幂等、乐观并发、ItemChange 和重启持久性均有独立回归测试；
- 工作树无生成物和测试 profile 污染，`git diff --check` 和敏感信息扫描通过；
- 代码审查没有 Critical 或 Important 问题；
- 完成后创建一个新的 annotated tag，再开始默认所见即所得编辑器接入。

资源二进制、同步、E2EE、Data API、搜索/OCR/向量和真实资料迁移都保留在后续阶段，不能用本阶段通过来替代它们的验收。
