# Pre-PR 1 · Rust 引擎与 LSP 调度解耦

| 项目 | 内容 |
| --- | --- |
| 状态 | 已完成 |
| 创建日期 | 2026-11-26 |
| 用户确认 | 已确认（2026-11-26） |
| 更新日期 | 2026-11-26 |
| 完成日期 | 2026-11-26 |

## 目标与范围

将 Go 源文档、声明索引、解析器、请求注册和长查询调度拆到各自的模块。本次仅调整结构，保留已发布的 LSP 能力、JSON 形状、错误码与消息、UTF-16 范围、结果过滤与排序、缓存失效及性能基线。后续六条功能线通过所属处理器和一处描述项接入，语义查询共享引擎的只读接口。[协作指南](../COLLABORATION.md)

| 文件 | 迁入的职责与依赖方向 |
| --- | --- |
| `src/semantic.rs` | 保留 `Engine`、`GraphChanges`、文档和包图生命周期、缓存所有权及失效协调。 |
| `src/semantic/source.rs` | 接收 `Source`、`LineIndex`、导入语法、光标语法目标和 `TextEditDelta`。只依赖语法树、坐标和通用语义数据，编辑摘要由调用者消费。 |
| `src/semantic/index.rs` | 接收 `FileIndex`、`DiskIndex`、`IndexReader`、磁盘指纹和草稿文件选择。索引从 `source` 读取语法，按原顺序合并活动文件和草稿。 |
| `src/semantic/resolver.rs` | 接收 `TypeIdentity`、`ResolvedSymbol`、`Resolver` 及名称解析、别名、类型推断与泛型辅助逻辑，调用 `index`、`source` 和现有 `scope` / `symbols`。 |
| `src/semantic/{snapshot,definition,references,query}.rs` | 分别承载只读查询入口、声明与类型定义入口、引用扫描、类型化 `QueryProgress`。实现跳转和工作区符号保留在原功能文件。 |
| `src/dispatch.rs`、`src/protocol.rs` | `dispatch` 按功能注册方法与能力，解码参数并绑定结果编码。 `protocol` 提供共用 URI 转换、结果编码和响应发送，供路由、调度及已有符号代码复用。 |
| `src/scheduler.rs`、`src/main.rs` | `scheduler` 管理加载工作线程、等待队列、可推进任务及回复。 `main` 保留 stdio、会话状态、工作区根选择、通知应用顺序与事件循环，调用路由和调度器。 |

**范围边界**。现有 Go 查询算法和协议语义原样迁移。新增 LSP 能力、`import_edits`、`DiagnosticReport` 等跨线业务由对应 Track 后续交付。`workspace.rs` 的 `go list` 元数据模型及多工作区演进归 Track F。共享 Python/Rust 测试夹具由 Pre-PR 2 负责，本 PR 的 Rust 测试沿用 `TestProject`，Python 验收直接复用现有入口。

## 验收标准

- [x] **模块边界**。以当前 3,681 行的 `semantic.rs` 和 732 行的 `main.rs` 为基线，两文件各缩减至少一半，并核对上表职责。`source` 对 `implementation` 零依赖。功能模块借助只读 Facade 拿到同一快照的文档、UTF-16 范围、作用域、声明、名称和类型解析结果及活动包图。新增查询接线只需功能描述项和处理器。
- [x] **文本编辑和失效**。多次增量编辑按通知顺序换算，覆盖 LF、CRLF、非 BMP 字符、全量替换和批内无效范围。实现缓存的空行或普通注释位移、零行差复用及保守清除与基线一致。失败的编辑保留原文档、版本和缓存。
- [x] **统一长查询**。控制包图回调和批次推进，交错执行 `textDocument/references`、`textDocument/implementation` 和 `workspace/symbol`。等待与扫描阶段的显式取消各对目标 ID 恰好回复一次 `-32800`。正文或包图变更按现有影响范围恰好回复一次 `-32801`。核对过时代次、同代次测试视图升级、等待中的工作区符号读取最新缓冲区，以及其它在途 ID 的结果。
- [x] **协议与生命周期**。初始化能力、客户端选项协商、文件监听注册、请求参数错误、空结果、包加载错误与回退、关闭和退出行为与基线一致。保持传入消息、包图回调与查询批次的现有优先级。
- [x] **Rust 验证**。新增调度器交错和取消测试、编辑 delta 和复用测试通过，已有 Rust 测试全绿。`cargo check --workspace --locked` 和 `cargo test --workspace --locked` 零告警，`cargo build -p rs-gopls --release --locked` 通过。
- [x] **历史回归**。发布版通过阶段 6 声明，及 GoLand 声明、类型定义、引用、实现、Hover、SignatureHelp、DocumentSymbol 和 WorkspaceSymbol 的对应语料和检查脚本。`check_workspace_updates.py` 全部用例通过。
- [x] **性能抽检**。同一主机上对照运行中的 GoLand、重构前及重构后发布版。按用例记录核心实现导航、文档大纲及工作区符号的热查询 P50/P95 和样本数。分别报告索引就绪后首查、重复查询和编辑后查询，同结果的两个 rs-gopls 发布版满足下文的回退判定。

## 实现依据

### 既有 GoLand 语义

本次结构调整直接沿用已有 GoLand 基线及其已记录的 IDE/LSP 行为差异。只读核对本机实际安装目录 `E:\Apps\goland-2026.3.0.1.win` 的 `build.txt`，构建号为 `GO-263.7307.354`。`plugins/go-plugin/lib/go-plugin.jar` 中的 `META-INF/plugin.xml`、`plugins/go-plugin/lib/modules/intellij.go.impl.jar` 中的 `intellij.go.impl.xml` 及相应类条目支持下表。语义验收依据仍为已有运行采集及仓库夹具。

| 功能 | 已核对的安装包入口和语义依据 |
| --- | --- |
| 声明 / 类型定义 | `com.goide.psi.impl.GoReference`、`GoTypeReference`。沿用局部和包级解析、类型解析及精确落点。[声明基线](../goland-definition-baseline.md) · [类型定义基线](../goland-type-definition-baseline.md) |
| 引用 / 实现 | `com.goide.usages.GoFindUsagesProvider`、`GoFindUsagesHandlerFactory` 和 `com.goide.go.GoInheritorsSearch`。沿用源码用法、声明选项、正反向搜索和已记载的活动包范围。[引用基线](../goland-references-baseline.md) · [实现基线](../goland-implementation-baseline.md) |
| Hover / SignatureHelp | `com.goide.documentation.GoDocumentationProvider`、`com.goide.editor.GoParameterInfoHandler`。沿用文档和签名输出、光标参数定位及编辑态基线。[Hover 与签名基线](../goland-hover-signature-baseline.md) |
| 大纲 / 工作区符号 | `com.goide.tree.GoStructureViewFactory`、`com.goide.go.GoSymbolContributor`、`GoTypeContributor`。沿用现有层级、查找范围及匹配规则的发布契约。[符号基线](../goland-symbols-baseline.md) |

### Rust 当前耦合

行号以源码提交 `779fe78fda59a3267de2a0158ff514b7043c5eac` 为准。协作指南提交 `33a00b4` 与该提交的源码一致。

| 位置 | 现有依赖和迁移边界 |
| --- | --- |
| `src/semantic.rs:33–63, 686–812, 3287–3319` | `Source` 懒建声明并生成包签名，`DiskIndex` 保存声明、内容指纹和可选解析树，`IndexReader` 混合打开覆盖、磁盘缓存、包文件与草稿。将声明构建和索引选择放进 `index`。 |
| `src/semantic.rs:266–287, 3079–3142` · `src/semantic/implementation.rs:48–142` | `Engine::edit` 按实现缓存是否为空启用跟踪。 `Source::edit` 每次改动前直接调用 `implementation::LineShift::for_edit` 并返回该私有类型的集合。 `ImplementationCache::shift` 重算查询键和结果范围，必要时清除接口索引。 |
| `src/semantic.rs:65–145, 386–487, 963–1147` | `Search` 和 `SearchResult` 将三个任务的类型和结果集中编码，`Engine::advance_search` 再按任务类型分派。引用的启动、批次和候选扫描迁入 `references`。 |
| `src/semantic.rs:489–556, 580–684, 814–962, 1149–3020, 3385–3533` | 声明入口内直接组装解析器。 `active_package`、`draft_files` 和 `draft_package` 补足编辑态包归属。名称、成员、类型和泛型解析共用 `IndexReader`、`scope` 及 `symbols`。 `unify_types` 在 2583–2594 行调用 `signature_help.rs:531–626` 的函数类型统一逻辑，该逻辑随解析器迁移。它与签名帮助共用的 `function_at`（`signature_help.rs:252–263`）和 `node_at_range`（`presentation.rs:334–342`）归到 `source`。 |
| `src/semantic/implementation.rs:192–207, 217–460, 462–483` · `src/semantic/workspace_symbols.rs:85–188, 468–487` | 实现跳转和工作区符号各自维护批次、缓存和结果收尾，通过 `Search` 回到入口。实现模块还扩展 `IndexReader` 做冷索引的 `type` 文本预筛。 |
| `src/main.rs:50–89, 93–237, 239–282` | `NavigationKind`、`Query` 和 `ActiveSearch` 串接等待、启动、推进和取消。包图失败按请求类型选择 Hover / SignatureHelp 回退，完成按 `SearchResult` 选择编码。 |
| `src/main.rs:295–355, 357–483, 495–559, 578–645, 647–732` | 事件循环直接检查搜索队列，正向路由、能力、通知、参数/URI 解析与结果编码集中在同文件。工作区符号还从入口导入 `path_uri`。 |
| `src/semantic.rs:312–383` · `src/main.rs:120–165, 647–717` | `WorkspaceLoad` 带 `generation` 与 `include_tests`。引擎仅安装当前代次的成功回调，失败处理匹配完整载入键。请求回调重跑包图就绪判断，编辑与文件通知决定哪些在途请求失效。 |

### 文档编辑与声明索引

- `Source` 管理文本、语法树、`LineIndex`、导入语法、轮廓及标识符偏移。 `index` 的 `IndexedSource` 复用同一 `Source`，在旁边保存按文档懒建的 `OnceCell<FileIndex>`，供打开文档、请求临时源和引用磁盘源使用。 `DiskIndex` 维持原有声明、指纹及按需 `Rc` 源引用，缓存仍归 `Engine` 持有。引用源保留打开缓冲优先、磁盘不存在时跳过的规则，普通磁盘索引及实现候选冷读入保留现有错误处理和预筛。
- `Source::edit` 对每个已验证的改动，在执行 `InputEdit` 和文本替换前向可选观察者产出借用型 `TextEditDelta<'_>`。摘要携带可选原 LSP 范围、原字节区间、插入文本、新结束行及本次修改前的文本与树引用。多条 change 以累积后的坐标解释，树的范围依次应用 `InputEdit`，最终仅重解析一次。缺省观察者走现有解析路径。观察者可在判定整批需清除后停止接收后续摘要。摘要借用已有工作文本和树，延用每个通知一次增量解析的开销。
- `implementation` 的观察者把符合原规则的 delta 转成模块私有的 `LineShift` 并暂存。保留整行边界、顶层普通注释/空白、语法树错误、起始 BOM、指令与块注释等全部判定。全量替换或任一不符合复用条件的改动使整批失效。 `Engine` 在最终解析成功后更新文档版本和工作区符号索引，再让实现缓存按顺序应用位移或清除。零行差保持现有快路径。非零位移清除接口索引，保留包探测缓存，并丢弃查询键或任一结果端点无法转换的条目。

### 只读查询入口

- `Engine::snapshot(&mut self) -> EngineSnapshot<'_>` 在 `snapshot.rs` 提供。私有字段按需借用文档、包图、解析器和磁盘索引。 `document(path)` 优先借用已打开的 `IndexedSource`，未打开的 Go 文件使用一次性读取。 `DocumentSnapshot` 提供路径、可选版本、文本、根节点、字节位置与 UTF-16 范围换算、局部绑定、文件声明与大纲。非 Go 文档和磁盘错误继续由既有调用者映射到各自的空结果或 LSP 错误。
- `active_graph(require_tests)` 返回可按包 ID、文件或导入查找的只读 `&Workspace` 或 `Pending(WorkspaceLoad)`，沿用全图干净和测试视图要求。 `package_for(document, require_tests)` 按 `GraphChanges::affects_source` 及现有草稿规则返回包归属、空结果或 `Pending`。请求包装器在原有时机记录 `include_tests` 并交调度器加载，Facade 仅计算就绪状态。等待中的单文件查询保留局部解析及未受影响包的快路径。
- `resolve_name(document, position)` 和 `resolve_type_definition(document, position)` 返回现有的声明和类型落点，`infer_type(document, node)` 复用 `TypeIdentity`。它们在同一次查询内通过 `IndexReader` 构造 `Resolver`，使用同一包身份与文档。借用结果限于该快照，分批任务只保留拥有所有权的候选键和进度。以 `pub(crate)` 暴露这些查询方法和只读值。`DocumentSymbol` 复用文档和范围入口，声明/类型定义、Hover 和签名复用作用域、声明和解析访问，引用与符号批次复用活动包图访问。现有按需解析和缓存路径以原调用点的行为为准。

### 请求注册与任务状态

- `dispatch.rs` 按导航、Hover/SignatureHelp 和符号组织功能描述项。每项提供能力写入函数与方法处理器列表。初始化遍历描述项写能力，运行态按方法找到处理器。 `main` 处理 `initialize`、`shutdown`、`exit` 和 `initialized`，保留根选择、嵌套大纲协商和动态监听注册时机。每个处理器保留现有参数解码、URI 表示、查询及序列化的错误映射。`DefinitionKind` 仅用于声明/类型定义，移除中心 `NavigationKind`。
- 语义层使用 `QueryProgress<T, Task = Infallible>`，状态为 `Ready(T)`、`Pending(WorkspaceLoad)`、`Searching(Task)`。长查询模块各自提供具体任务、推进和结果收尾，类型化返回原有位置列表或 `SymbolInformation` 列表。路由以一个通用适配器装箱为 `Box<dyn SearchTask>`，接口为 `advance(&mut self, &mut Engine) -> anyhow::Result<bool>` 和 `finish(self: Box<Self>) -> RequestResult`。完成时用绑定的旧编码函数生成 JSON 或错误。同步快路径直接编码，只有要继续等待或扫描时装箱，中心移除 `Search` 与 `SearchResult` 变体表。
- `Pending` 保存 ID、可选规范化文档键及可重跑的类型化参数。包图回调重跑原处理器，仍等待时复用该记录，扫描时把同一 ID 移交搜索队列。最新加载失败时调用处理器自己的回退，Hover/SignatureHelp 保留原本地回退，其余沿用 `RequestFailed`。旧代次成功或不匹配的失败回调均不得生成过期回复。同代次普通图可以安装，等待测试视图的处理器继续请求测试图。
- `Searching` 记录 ID、启动包图代次和任务。调度器每轮取队首批次，未完成者送回队尾。推进和发送结果前核对代次，发现过期时移除任务并以现有工作区变更消息回复 `-32801`。任务内部保留包图清洁度及工作区符号索引 revision 检查。引用维持每批 8 文件 / 2 ms，实现预筛和搜索各维持 32 步 / 2 ms，工作区符号维持 8 文件索引、128 文件扫描及 2 ms 预算。保留传入消息优先、其后包图结果、最后搜索 token 的 `select_biased!` 顺序，以及单工作线程加载语义。

| 事件 | 调度器动作 |
| --- | --- |
| `$/cancelRequest` | 按原整数或字符串 ID 从等待或搜索队列取出记录，响应 `-32800`。保留等待和扫描各自原有消息。重复取消或取消已回复 ID 时队列无记录。 |
| 打开、修改、关闭及未打开文件的磁盘变化 | 按原 `path_key` 取消该文档上的等待查询，取消所有已进入 `Searching` 的任务，响应 `-32801`。仍等待的工作区符号及其它文档查询保留，后续按最新文档和包图重跑。 |
| 监听通知引起包图代次改变 | 按现有通知顺序移除仍在运行的搜索并响应 `-32801`，请求最新包图。调度器的代次检查也阻止过期搜索发布结果。 |
| 查询完成、错误或 shutdown | 取出记录后发送唯一终态回复。shutdown 先清空等待和搜索队列，沿用 `RequestFailed`，再返回原 shutdown 响应。 |

待验证的重构假设包括多条 delta 的位移与清除顺序、对同一 ID 重复取消后的队列所有权、同代次测试视图升级和已过期包图回调，以及拆分后热查询的分配与解析次数。

## 实施步骤

1. 收到明确确认后记录确认内容，将状态改为「进行中」。从指定源码基线构建发布版并保存基线可执行文件、SHA-256 和环境信息，确认回归语料及 GoLand 实测输入可重放。
2. 迁移 `Source` / `LineIndex`、索引和加载源逻辑。把 `LineShift` 判定集中到实现缓存，接入 delta 观察者并验证无副作用失败、缓存复用和 UTF-16 边界。
3. 移动解析器、声明/类型定义和引用入口，接入只读 Facade。保持测试视图、草稿文件、别名和类型推断行为，先跑受影响包的检查与语义测试。
4. 按功能描述项迁移能力和请求路由，迁移加载与长查询队列。先接入引用，再接入实现和工作区符号，随后迁移等待包图的定义、Hover 和签名提示。通过内存连接和现有 `TestProject` 确定性模拟分批、编辑、取消和包图回调。
5. 执行工作区构建、历史回归和三方性能对照。在本计划中记录实际命令、行数、逐项验收、样本和结果。全部验收通过后更新为「已完成」并填完成日期。目标、范围或验收条件发生实质变化时先更新本计划并重新确认。

## 验证计划

### 构建与确定性测试

在仓库根目录执行。所有 Cargo 调用沿用环境配置的产物目录和项目约定的 toolchain、profile、features 与 target。首先检查受影响的 `rs-gopls` 包，随后运行定向测试，最后运行约定的全工作区验收。

```powershell
cargo check -p rs-gopls --locked
cargo test -p rs-gopls --locked semantic::source::tests
cargo test -p rs-gopls --locked semantic::implementation::tests
cargo test -p rs-gopls --locked scheduler::tests
cargo test -p rs-gopls --locked symbol_request_tests
cargo check --workspace --locked
cargo test --workspace --locked
cargo build -p rs-gopls --release --locked
$cargoArtifacts = (cargo metadata --format-version 1 --no-deps --locked | ConvertFrom-Json).target_directory
$server = Join-Path $cargoArtifacts 'release\rs-gopls.exe'
```

- `source::tests` 与 `implementation::tests` 检查 delta 的批内坐标、CRLF、UTF-16 代理对边界、行尾截断、全量替换、第二条编辑失败时的回滚、普通注释/空行插删、同一行数替换和指令/BOM/语义编辑清除。
- `scheduler::tests` 使用内存连接和可控的工作步长，交错三种实际任务，用不同整数和字符串 ID 计数。覆盖等待、建索引和扫描取消，重复取消、完成后取消、同文档编辑、无关文档编辑、包图监听刷新、旧成功/失败回调、测试图升级、加载回退及 shutdown。每次清空队列后断言每个 ID 恰好一条回复，并验证继续的请求及重试命中完整结果。

### 发布版历史回归

用基线 Go SDK `go1.27.4 windows/amd64` 和固定工作区。先核对 `go version`、`go env GOROOT GOOS GOARCH GOWORK` 及 LocalChat 修订 `532228d3d36bd9f0fffb4bda35c180485b2eadd6`。使用同一发布版跑完整 LocalChat 语料与仓库补充夹具，沿用各检查器的严格模式及已固化的差异说明。

```powershell
$sdk = Join-Path $env:USERPROFILE 'go\pkg\mod\golang.org\toolchain@v0.0.1-go1.27.4.windows-amd64'
$env:PATH = (Join-Path $sdk 'bin') + ';' + $env:PATH
$env:GOTOOLCHAIN = 'local'
$localchat = 'E:\Code\localchat\server'
$typeFixture = (Resolve-Path '.\tools\fixtures\type-navigation').Path
$implFixture = (Resolve-Path '.\tools\fixtures\implementation-navigation').Path
$helpFixture = (Resolve-Path '.\tools\fixtures\hover-signature').Path
$symbolFixture = (Resolve-Path '.\tools\fixtures\symbol-navigation').Path
go version
go env GOROOT GOOS GOARCH GOWORK
git -C $localchat rev-parse HEAD

python -B .\tools\check_definitions.py $server --stage 6 --workspace $localchat
python -B .\tools\check_goland_definitions.py $server --workspace $localchat --method definition
python -B .\tools\check_goland_definitions.py $server --workspace $localchat --method typeDefinition
python -B .\tools\check_goland_definitions.py $server --workspace $typeFixture --method typeDefinition --fixture (Join-Path $typeFixture 'goland-types.tsv')
python -B .\tools\check_goland_references.py $server --workspace $localchat
python -B .\tools\check_goland_implementations.py $server --workspace $localchat
python -B .\tools\check_goland_implementations.py $server --workspace $implFixture --fixture (Join-Path $implFixture 'goland-implementations.json')
python -B .\tools\check_goland_hover.py $server --workspace $localchat
python -B .\tools\check_goland_hover.py $server --workspace $helpFixture --fixture (Join-Path $helpFixture 'goland-hover-signature.json')
python -B .\tools\check_goland_signature_help.py $server --workspace $localchat
python -B .\tools\check_goland_signature_help.py $server --workspace $helpFixture --fixture (Join-Path $helpFixture 'goland-hover-signature.json')
python -B .\tools\check_goland_symbols.py $server --workspace $localchat --fixture .\tools\fixtures\localchat-goland-symbols.json
python -B .\tools\check_goland_symbols.py $server --workspace $symbolFixture --fixture (Join-Path $symbolFixture 'goland-symbols.json')
python -B .\tools\check_workspace_updates.py $server
```

### 性能抽检

- 重构前在同一工作区以 `cargo check -p rs-gopls --locked` 和 `cargo build -p rs-gopls --release --locked` 构建 `779fe78` 的源码，包元数据目录仍由 `cargo metadata` 读取。将输出的 exe 复制到下面的计时目录，保存哈希。重构后重用同一个 Cargo 产物目录构建候选版。
- 固定 Windows 主机、GoLand 构建 `GO-263.7307.354`、Go SDK、LocalChat 修订、活动构建条件、作用域、输入位置、打开文件和注释编辑序列。GoLand 在已完成索引的运行进程中，从触发查询到提取完目标范围计时，rs-gopls 计 stdio 请求至 JSON 回复。报告编辑是计入整轮还是在计时前提交。每次核对完整目标集合及 UTF-16 起止，既有差异单列。[导航性能对照](../PLANS.md#导航性能对照)
- **实现跳转**。用 LocalChat 的 `store-type`（正向）和 `sql-user-store-get-all`（反向）采样。按已有采集流程生成 `implementation-queries.tsv`，将 `capture_goland_implementations.groovy` 复制到临时目录，在副本围绕对应动作与完整范围提取记录 `System.nanoTime()`。用 `Start-Process -FilePath $goLand -ArgumentList @('ideScript', ('"{0}"' -f $implTimingScript)) -WindowStyle Hidden -Wait` 调用该副本。记录已完成索引的首查、预热 5 次后的 30 次复查及未保存的首行 `// rs-gopls benchmark overlay` 加 LF 的插入或撤回查询。仅把当前修改文件的光标和目标范围应用预期行差，按符号采集器的流程恢复缓冲区后复查。Rust 侧先以非空且无命中的工作区符号查询等包图和索引就绪，再计两例首查。复用已有实现基准测同光标与编辑后查询，5 轮交替候选/基线次序，每档每轮 80 次。`first-correct` 包含启动，单列。[实现采集流程](../goland-implementation-baseline.md#重新采集) · [实现实测](../goland-implementation-baseline.md#与-goland-直接对照) · [符号采集器](../../tools/capture_goland_symbols.groovy)
- **符号**。打开 LocalChat 与合成符号工作区，按已有重放流程运行 GoLand 采集器，在临时目录保留语义 JSON 和 `*-symbols-bench.json`，以仓库中已提交语料作检查输入。分别用同一份当轮 GoLand 采集输入对比两个 rs-gopls 发布版，并核对大纲与工作区符号的共同样本 ID 一致。[符号采集与重放](../goland-symbols-baseline.md#重放)
- **回退判定**。对共同样本按请求及场景分别记录 P50/P95、样本数和缓存状态。热态与编辑后每个分位数的拟定容差为基线的 10% 或 0.10 ms 取较大者。任一越界时在相同条件下互换运行顺序复测，同一分位数再次越界视为回退。低样本首查与启动单列报告。

确认后先在改动前保存基线。以下使用上节的 `$server`、`$localchat`、`$symbolFixture` 和 SDK 设置。

```powershell
$perfDir = Join-Path $env:TEMP 'rs-gopls-pre-pr-1'
New-Item -ItemType Directory -Force -Path $perfDir | Out-Null
$baseline = Join-Path $perfDir 'rs-gopls-before-779fe78.exe'
Copy-Item -LiteralPath $server -Destination $baseline
Get-FileHash -LiteralPath $baseline -Algorithm SHA256
```

候选发布版构建完成后执行以下命令。

```powershell
Get-FileHash -LiteralPath $server -Algorithm SHA256
$goLand = 'E:\Apps\goland-2026.3.0.1.win\bin\goland64.exe'
$goLandCapture = Join-Path $env:TEMP 'rs-gopls-goland-baseline'
foreach ($phase in @('before', 'after')) {
    $binary = if ($phase -eq 'before') { $baseline } else { $server }
    python -B .\tools\compare_symbols.py $binary --workspace $localchat --fixture .\tools\fixtures\localchat-goland-symbols.json --goland $goLand --goland-bench (Join-Path $goLandCapture 'localchat-symbols-bench.json') --startup-runs 1 --json (Join-Path $perfDir "$phase-localchat-symbols.json")
    python -B .\tools\compare_symbols.py $binary --workspace $symbolFixture --fixture (Join-Path $symbolFixture 'goland-symbols.json') --goland $goLand --goland-bench (Join-Path $goLandCapture 'fixture-symbols-bench.json') --startup-runs 1 --json (Join-Path $perfDir "$phase-fixture-symbols.json")
}
@'
from argparse import Namespace
from pathlib import Path
import json
import os
import sys
import time

sys.path.insert(0, "tools")
from check_goland_implementations import DEFAULT_FIXTURE, initialize, load_cases, select_cases
from compare_definitions import LspClient
from compare_symbols import READY_QUERY
from compare_workspace_implementations import measure, run_session
from compare_workspace_references import successful, summary

before, after, workspace, report = map(Path, sys.argv[1:])
cases = select_cases(load_cases(DEFAULT_FIXTURE, workspace), ["store-type", "sql-user-store-get-all"])
args = Namespace(timeout=60.0, iterations=80)
environment = {**os.environ, "GOPROXY": "off"}
pair = [("before", before), ("after", after)]
sessions = []

def ready_first(binary):
    client = LspClient([str(binary)], workspace, args.timeout, environment=environment)
    try:
        initialize(client)
        for path, text in {case.query.path: case.query.text for case in cases}.items():
            client.open_document(path, text)
        if client.request("workspace/symbol", query=READY_QUERY) != []:
            raise RuntimeError("Index readiness query matched a symbol")
        samples = [
            measure(client, case, frozenset(), time.perf_counter_ns(), f"ready-first-{case.action}")
            for case in cases
        ]
        client.shutdown()
        return samples
    finally:
        client.close()

for index in range(5):
    order = pair if index % 2 == 0 else reversed(pair)
    for label, binary in order:
        first_samples = ready_first(binary)
        session = run_session(str(binary), workspace, cases, cases[0], args, environment)
        session["samples"].extend(first_samples)
        sessions.append({"server": label, "session": index + 1, **session})
report.write_text(json.dumps({"sessions": sessions, "summary": summary(sessions)}, indent=2), encoding="utf-8")
raise SystemExit(0 if all(map(successful, sessions)) else 1)
'@ | python -B - $baseline $server $localchat (Join-Path $perfDir 'implementation-before-after.json')
```

## 完成记录

- 2026-11-26 创建待确认规格。核对源码基线行号、GoLand 安装包入口、PowerShell 命令语法和 Python 参数，确认实现样本与已有符号采集的输入语料一致。实施、构建和实测结果将在确认后补入本节。

- 2026-11-26 收到用户确认，将计划更新为「进行中」。Rust 源码与 `779fe78` 一致，改动前从 Cargo 配置的 `E:\Code\rs-gopls\target\release\rs-gopls.exe` 复制现有发布版到 `%TEMP%\rs-gopls-pre-pr-1\rs-gopls-before-779fe78.exe`。SHA-256 为 `FF7983D4B07E0BC70C392060586406076FD44856B7C3A8AD0657B979BE32E5EB`。构建环境为 Rust/Cargo 1.95.0、`x86_64-pc-windows-msvc`。

- 2026-11-26 完成首轮 Rust 重构与构建。初版候选的 SHA-256 为 `FA769ED5189B5AFE5805D4631EE4643E2DC889B1E1C4A37B8D53470F557225D5`，副本保存在 `%TEMP%\rs-gopls-pre-pr-1\rs-gopls-after-initial-FA769ED5.exe`。复核基线副本的 SHA-256 与保存时一致。
- 按顺序运行 `cargo check -p rs-gopls --locked`、`cargo test -p rs-gopls --locked`、`cargo check --workspace --locked`、`cargo test --workspace --locked` 和 `cargo build -p rs-gopls --release --locked`，全部通过，零告警。包级和工作区测试均通过 52 项，含 39 项既有测试和 13 项新增测试。Zed 扩展测试目标有 0 项测试。既有符号请求测试 3 项直接编译通过。
- 新增源码 3 项、实现缓存 4 项、只读快照 1 项、调度器 5 项测试。测试核对了批内 LF/CRLF 和 UTF-16 坐标、整批回滚、缓存位移与清除、三种实际长查询交错、等待与建索引及扫描取消、同文档与无关文档编辑、监听刷新、旧回调、测试图升级、加载回退和关闭。调度测试按整数与字符串 ID 核对唯一回复。
- `src/semantic/source.rs` 对 `implementation` 零依赖，`LineShift` 判定与应用归实现缓存。新拆分的语义路径使用 `source::node_at_range`，既有展示模块通过其原入口定位节点。事件循环保持传入消息、包图回调、搜索批次的优先级。

| Rust 文件 | 基线行数 | 候选行数 |
| --- | ---: | ---: |
| `src/semantic.rs` | 3,681 | 289 |
| `src/main.rs` | 732 | 354 |
| `src/semantic/source.rs` | — | 673 |
| `src/semantic/index.rs` | — | 402 |
| `src/semantic/resolver.rs` | — | 2,283 |
| `src/semantic/snapshot.rs` | — | 301 |
| `src/semantic/definition.rs` | — | 108 |
| `src/semantic/references.rs` | — | 319 |
| `src/semantic/query.rs` | — | 27 |
| `src/protocol.rs` | — | 69 |
| `src/dispatch.rs` | — | 332 |
| `src/scheduler.rs` | — | 992 |
| `src/semantic/implementation.rs` | 690 | 889 |
| `src/semantic/workspace_symbols.rs` | 1,062 | 1,069 |
| `src/semantic/document_symbols.rs` | 308 | 303 |
| `src/semantic/hover.rs` | 1,149 | 1,144 |
| `src/semantic/signature_help.rs` | 1,054 | 953 |

`semantic.rs` 缩减 92.15%，`main.rs` 缩减 51.64%。

### 最终构建与历史回归

- 最终候选来自 Cargo 配置产物目录的 `target\release\rs-gopls.exe`，SHA-256 为 `AD694D0B033D8AC0CA39D3C6E94A3E013D230FC22C7631570425149A3BA5FEDA`。基线 `779fe78` 副本的 SHA-256 为 `FF7983D4B07E0BC70C392060586406076FD44856B7C3A8AD0657B979BE32E5EB`。
- 最终版的 `cargo check -p rs-gopls --locked`、`cargo check --workspace --locked`、`cargo test --workspace --locked` 和 `cargo build -p rs-gopls --release --locked` 均退出 0，零告警。工作区通过 52 项，Zed 扩展目标 0 项。定向测试 `semantic::source::tests`、`semantic::implementation::tests`、`scheduler::tests`、`symbol_request_tests`、`document_symbols::tests`、`workspace_symbols::tests` 分别通过 5、4、5、3、3、5 项。
- Go 回归使用 PATH 首位的 `go1.27.4 windows/amd64` 和 `GOTOOLCHAIN=local`。LocalChat 固定为 `532228d3d36bd9f0fffb4bda35c180485b2eadd6`，`go env` 确认 GOOS/GOARCH 为 `windows/amd64`，其 GOWORK 为空。回归前后 LocalChat 的 `server` 路径干净，`go.mod` 和 `go.sum` 哈希一致。
- `check_workspace_updates.py` 按基线顺序通过 23/23，退出 0。下面 13 条历史命令均使用同一最终发布版。

| 命令 | 工作区 | 校验结果 | 退出码 |
| --- | --- | --- | ---: |
| `check_definitions.py --stage 6` | 合成夹具和 LocalChat | 124/124 合成位置，16/16 项目锚点 | 0 |
| `check_goland_definitions.py --method definition` | LocalChat | 19 个相同落点，1 个已记录的 `ide-package` 差异 | 0 |
| `check_goland_definitions.py --method typeDefinition` | LocalChat | 32 个相同落点，2 个预期空结果 | 0 |
| `check_goland_definitions.py --method typeDefinition` | `type-navigation` | 24/24 | 0 |
| `check_goland_references.py` | LocalChat | 34/34 | 0 |
| `check_goland_implementations.py` | LocalChat | 20/20，应用已记录的活动包图排除项 | 0 |
| `check_goland_implementations.py` | `implementation-navigation` | 29/29 | 0 |
| `check_goland_hover.py` | LocalChat | 21/21 | 0 |
| `check_goland_hover.py` | `hover-signature` | 14/14 | 0 |
| `check_goland_signature_help.py` | LocalChat | 13/13，编辑、关闭恢复和取消通过 | 0 |
| `check_goland_signature_help.py` | `hover-signature` | 19/19，编辑、关闭恢复和取消通过 | 0 |
| `check_goland_symbols.py` | LocalChat | 每种模式大纲 8/8，工作区查询 20/20，其中 14 组相同物理落点 | 0 |
| `check_goland_symbols.py` | `symbol-navigation` | 每种模式大纲 5/5，工作区查询 36/36，其中 27 组相同物理落点 | 0 |

### 三方性能抽检

GoLand 构建为 `GO-263.7307.354`，当轮运行进程用于工作集采样。实现跳转使用同一主机、SDK 和 LocalChat 修订的 GoLand 存档 `target/goland-implementation-timing-76aa881.tsv`。两例各核对了 35 次的完整目标及 UTF-16 范围，包括首个探针、后续 4 次预热和 30 次复查。GoLand 计时从解析光标至提取完整范围。rs-gopls 计 stdio 往返，`edited` 包含未保存首行注释的插入或撤回通知，`first-correct` 包含进程启动。已索引首查在非空无匹配的工作区符号查询之后发起。

五轮按 `before/after`、`after/before` 交替执行。每轮每版包含 80 次暖态、80 次编辑后查询、两例各一次 `ready-first`，以及一次 `store-type` 启动首答。每版 815 次计时响应均与 GoLand 语料匹配。两例分别命中 39 个和 1 个目标。

| 查询 | 场景 | rs-gopls 每版样本数 | GoLand P50 / P95（ms） | before P50 / P95（ms） | after P50 / P95（ms） |
| --- | --- | ---: | ---: | ---: | ---: |
| `store-type` | `ready-first` | 5 | 1706.1301 (首个探针，1 次) | 326.8629 / 334.7817 | 310.8073 / 320.3976 |
| `store-type` | `warm` | 200 | 162.9350 / 192.8807 （30 次） | 0.4380 / 0.6026 | 0.3791 / 0.5115 |
| `store-type` | `edited` | 200 | — | 0.5678 / 0.7658 | 0.5302 / 0.7026 |
| `sql-user-store-get-all` | `ready-first` | 5 | 377.7740 (首个探针，1 次) | 233.4232 / 260.7721 | 228.3508 / 258.0580 |
| `sql-user-store-get-all` | `warm` | 200 | 6.8254 / 7.9456 （30 次） | 0.2454 / 0.3767 | 0.2690 / 0.3778 |
| `sql-user-store-get-all` | `edited` | 200 | — | 0.3800 / 0.5690 | 0.4448 / 0.6202 |
| `store-type` | `first-correct` | 5 | — | 750.2351 / 759.3933 | 750.1315 / 776.5114 |

符号沿用 `localchat-symbols-bench.json` 和 `fixture-symbols-bench.json`，与各自的 `*-timed.json`、已签入语料和完整范围逐一核对。GoLand 计智能模式下从导航调用至完整范围提取，rs-gopls 在含测试包的包图及符号索引就绪后计 stdio 往返。每组预热 5 次、复查 30 次。编辑通知在首个编辑后请求计时前发出，随后恢复缓冲区并验证结果。LocalChat 的 18/28 组与合成工作区的 30/41 组共同样本 ID 在两版完全一致，已记录的差异也逐项相同。表中 LocalChat 使用最终版首轮数据，合成工作区使用最终版换序数据。

| 工作区与请求 | 场景 | 每侧样本数 | GoLand P50 / P95（ms） | before P50 / P95（ms） | after P50 / P95（ms） |
| --- | --- | ---: | ---: | ---: | ---: |
| LocalChat `documentSymbol` | 就绪首查 | 4 | 12.7766 / 702.7896 | 0.8208 / 4.9231 | 0.7193 / 3.8453 |
| LocalChat `documentSymbol` | 暖态 | 120 | 4.3344 / 34.3412 | 0.7387 / 4.2375 | 0.8083 / 3.5398 |
| LocalChat `documentSymbol` | 编辑后 | 4 | 7.9545 / 97.8107 | 0.7022 / 3.3990 | 0.6804 / 3.0991 |
| LocalChat `workspaceSymbol` | 就绪首查 | 14 | 75.3697 / 353.2884 | 0.3124 / 1.0906 | 0.2626 / 0.5418 |
| LocalChat `workspaceSymbol` | 暖态 | 420 | 66.9168 / 94.2602 | 0.2328 / 0.3973 | 0.2405 / 0.4179 |
| LocalChat `workspaceSymbol` | 编辑后 | 14 | 69.4322 / 103.3824 | 1.3468 / 2.7532 | 1.4296 / 2.3945 |
| 合成工作区 `documentSymbol` | 就绪首查 | 3 | 3.3816 / 4.4413 | 0.2966 / 0.3141 | 0.2935 / 0.3291 |
| 合成工作区 `documentSymbol` | 暖态 | 90 | 0.3862 / 0.5862 | 0.1968 / 0.3210 | 0.1962 / 0.3313 |
| 合成工作区 `documentSymbol` | 编辑后 | 3 | 3.7816 / 5.5206 | 0.1870 / 0.2030 | 0.2447 / 0.2967 |
| 合成工作区 `workspaceSymbol` | 就绪首查 | 27 | 79.1728 / 152.3131 | 0.1000 / 0.1363 | 0.1292 / 0.1870 |
| 合成工作区 `workspaceSymbol` | 暖态 | 810 | 60.5495 / 125.1907 | 0.0943 / 0.1710 | 0.0979 / 0.1885 |
| 合成工作区 `workspaceSymbol` | 编辑后 | 27 | 74.4624 / 133.5104 | 0.5354 / 0.7283 | 0.4957 / 0.8254 |

- 初版在合成大纲编辑后 P95 上两次越界，差值分别为 +0.1870 ms 和 +0.1692 ms。成对诊断覆盖每个大纲 30 次带编辑查询及 30 次先等待编辑处理完毕的查询。随后在 `scheduler.rs` 跳过空等待队列的路径规范化，并在 `protocol.rs` 直接将已编码的 JSON 值移入成功响应。初版二进制和报告已留存，修补后重跑了构建、完整历史回归和三方抽检。
- 最终版首轮合成大纲编辑后 P95 为 0.2542 → 0.3814 ms。换序复测为 0.2030 → 0.2967 ms，差值 +0.0937 ms，回到 0.10 ms 容差内。最终版的 8 个实现分位数、8 个 LocalChat 符号分位数以及换序的 8 个合成符号分位数全部满足 `max(10%, 0.10 ms)`。

最终版首轮四条符号基准各使用 `--startup-runs 1`。下表给出进程启动至首个正确结果的单次耗时（ms）及括号中的主进程 / 进程树采样峰值（MiB）。GoLand 在临时副本上复用预建磁盘索引，两侧共享系统和 Go 缓存。Windows 工作集以 50 ms 为目标采样间隔，GoLand 启动结果文件以 5 ms 为目标轮询间隔。

| 工作区与请求 | 首查样本 | before | after | GoLand 对照 before | GoLand 对照 after |
| --- | --- | ---: | ---: | ---: | ---: |
| LocalChat `documentSymbol` | `internal/store/store.go` | 18.24（6.36/6.36） | 14.49（6.18/6.18） | 18096.85（2386.08/2397.16） | 17868.00（2297.63/2308.70） |
| LocalChat `workspaceSymbol` | `simple` | 429.03（10.09/35.11） | 428.08（10.13/31.43） | 17017.67（2291.11/2302.18） | 17646.38（2487.41/2498.48） |
| 合成工作区 `documentSymbol` | `primary/outline_test.go` | 10.12（5.20/5.20） | 7.86（5.19/5.19） | 15009.55（2216.41/2237.68） | 25108.15（2058.53/2069.59） |
| 合成工作区 `workspaceSymbol` | `same-type` | 199.96（6.56/21.46） | 202.38（6.57/30.45） | 15563.22（2223.78/2250.74） | 23785.62（2096.62/2107.70） |

四条最终符号命令、两条换序符号命令和五轮实现跳转脚本均退出 0。原始报告在 `%TEMP%\rs-gopls-pre-pr-1`，文件为 `final-{before,after}-{localchat,fixture}-symbols.json`、`final-reverse-{before,after}-fixture-symbols.json`、`implementation-final-before-after.json`。逐分位数容差核对在 `final-perf-tolerances.json` 与 `final-reverse-perf-tolerances.json`。构建、历史回归和计时命令日志在 `%TEMP%\rs-gopls-pre-pr-validation-2026-11-26`。
