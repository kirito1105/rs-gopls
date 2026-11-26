# GoLand 文档符号与工作区符号基线

2026-11-26 在 Windows 11 Pro `10.0.26300`（AMD Ryzen 7 255，8 核 16 线程）上运行 GoLand `GO-263.7307.354`。LocalChat 使用 `E:\Code\localchat\server`，修订为 `532228d3d36bd9f0fffb4bda35c180485b2eadd6`。合成工作区由 `go.work` 加载主模块 `example.test/symbols` 和次模块 `example.test/second`，两者的源码包名都是 `symbols`。[开发计划](plans/2026-11-26-go-document-and-workspace-symbols.md) · [LocalChat 语料](../tools/fixtures/localchat-goland-symbols.json) · [合成语料](../tools/fixtures/symbol-navigation/goland-symbols.json)

- IDE 与命令行验证均使用 `go1.27.4 windows/amd64`，SDK 根目录为 `C:/Users/rick/go/pkg/mod/golang.org/toolchain@v0.0.1-go1.27.4.windows-amd64`。启用 Go Modules 集成，IDE 构建目标为默认 `windows/amd64`，未设置自定义标签。命令行 `GOFLAGS` 为空，`CGO_ENABLED=1`。LocalChat 未加载 `go.work`，合成工作区加载其根目录的 `go.work`。
- LocalChat 恢复 `internal/api/api.go` 编辑页，符号搜索的 PSI 上下文固定为 `internal/model/user.go`。合成工作区无已打开编辑页，上下文为 `primary/outline.go`。大纲开启私有成员并应用 GoLand 内置顺序，主样本关闭 `Show package structure`。合成语料另附开启该选项的完整树。
- 工作区主样本使用 Project Scope，经 `GotoSymbolModel2`、GoLand 的 `MyViewModel` 和 `DefaultChooseByNameItemProvider` 调用已注册的 `GoSymbolContributor`。索引探针先枚举 `processNames` 提供的包限定键，再直接读取 `processElementsWithName`。IDE 的回调顺序、权重和重复条目保存在各查询的 `results`，项目与库作用域证据单独存入 `scopeWitnesses`。入口通过安装包 `intellij.go.impl.jar`（REA `ev_6c7c39a219d2e02923e7b819150326c434f3135777e22df0b1f7e49c6cdda5ee`）和已安装平台 JAR 的字节码定位，以下行为均来自运行时采集。[采集脚本](../tools/capture_goland_symbols.groovy) · [查询输入](../tools/fixtures/symbol-navigation/symbol-queries.json)

## 结构视图

| 样本 | 实测结果 |
| --- | --- |
| 跨文件接收者 | `method.go` 的文件视图保留从 `outline.go` 引用的 `Record` 父节点，其下是本文件的 `HandleRequest`、`BuildHTTP`、`Begin`。`outline.go` 的文件视图只显示 `Record` 的本文件成员。开启包视图后，三个方法也挂到 `outline.go` 的 `Record` 下。独立 PSI 声明仍在 `method.go`，范围也指向该文件。 |
| 分组声明和 `iota` | `First, Second` 共享 spec 范围 `[8,1,8,27]`，各有独立标识符范围。`Third, Fourth` 共享 `[9,1,9,14]`，PSI 表达式列表为空，GoLand 树分别显示两个名称。`Label, Title` 同样拆为两项。结构树按常量、变量、类型、函数排序，原始 PSI 保留源码声明顺序。 |
| 类型和字段 | GoLand 分开列出别名 `Alias`、定义类型 `Count`、结构体及接口。`Record.X`、`Record.Y` 具有相同的字段声明范围 `[27,1,27,9]`，标识符分别在 `[27,1,27,2]` 与 `[27,4,27,5]`。`Embedded`、`*clock.Location`、`Box[T]` 显示为 `Embedded`、`Location`、`Box`。`Inner.NestedField` 和 `Port.InlineMethod` 出现在各自的字段节点内。 |
| 接口和约束 | `Handler` 下有 `Base()` 和 `HandleRequest(int) error`。嵌入项 `Base` 的 `GoMethodSpec.identifier` 为空，节点与类型引用标识符均位于 `[38,1,38,5]`。`~int`、`Count \| ~string` 和 `~int \| ~string` 记录为 PSI `GoConstraintElem`，包含术语和各自范围，结构树只显示这两个约束接口的类型节点以及 `UnionConstraint.Marker()`。 |
| `init`、`_` 与导入 | 两个 `init()` 均可见，标识符在 `[59,5,59,9]` 和 `[60,5,60,9]`。`var _ Handler = (*Record[int])(nil)` 可见为 `_: Handler`。显式别名 `clock` 有 PSI 声明，结构树 0 项。 |
| CRLF 与 UTF-16 | `outline.go` 有 61 个 CRLF，`AfterEmoji` 与 `🙂` 同行。标识符范围 `[16,22,16,32]` 回读为 `AfterEmoji`，起列比 Unicode 标量列大 1。 |

`structure` 是文件模式的 GoLand 树，`packageStructure` 是合成夹具的包模式树，`declarations` 枚举同文件的原始 PSI 声明。`psiRange`、`declarationRange` 和 `statementRange` 分别记录节点、完整 spec 或字段及外层声明。各范围用零基 UTF-16 的 `[起行, 起列, 止行, 止列]`，终点排除。`identifierRange` 直接来自 Go PSI 的标识符 getter。嵌入接口项的类型引用叶子另记为 `referenceIdentifierRange`。裸嵌入 `Embedded` 的标识符和字段声明范围同宽。`navigationOffset` 记录原始导航坐标。

## 工作区搜索

| 查询或边界 | Project Scope 实测 |
| --- | --- |
| 名称、前缀、子串、驼峰、子序列 | `FileBackend` 首先命中同名接口，随后有 `LocalFileBackend`、`NewFileBackend` 等候选。`NewFile` 首个目标为 `NewFileBackend`。小写 `reviewitemsnotfound` 命中 `ErrReviewItemsNotFound`，`NFB` 包含 `NewFileBackend`，`MHS` 命中两模块的 `MakeHTTPServer`。`NwFlBcknd`、`MkHTSrv` 和空查询各返回 0 项。 |
| 限定名 | `filestore.NewFileBackend` 首个目标为同名函数。`HandleRequest` 先交付具体方法，再交付 `Handler` 的接口方法。`Record.HandleRequest` 与 `Handler.HandleRequest` 各保留其对应的物理目标。`User.PreSave` 指向 `user.go`。`symbols.Record.HandleRequest`、`symbols.Handler.HandleRequest`、`model.User.PreSave`、两套项目的完整导入路径方法查询和 `example.test/second.MakeHTTPServer` 均为 0 项。`X` 和 `Record.X` 也为 0 项。 |
| `init` | 合成 `init` 返回 2 个物理目标，分别指向两条 `init()`。LocalChat 这个查询命中 `init*` 一类候选，精确名称无 `init` 声明。 |
| 匿名嵌入字段 | `Embedded`、`Box` 各命中同名类型与真实的 `GoAnonymousFieldDefinition`，两个落点各自定位。`Location` 只有 `Record` 内 `*clock.Location` 的字段落点，标识符范围 `[29,8,29,16]`。 |
| 嵌入接口 | `Base` 同时命中类型声明和 `Handler` 内的 `GoMethodSpec` 嵌入项，后者的导航偏移为 `[38,1]`。`NestedField` 与 `InlineMethod` 各有一个物理落点。 |
| 导入别名与排除项 | `clock` 命中 `GoImportSpec`，落点 `[4,1,4,6]`。`_`、`localOnly` 和泛型参数 `T` 精确查询均为 0 项。计划默认将显式导入别名排除在 LSP 工作区符号集合之外。 |
| 多模块与测试包 | `symbols.Record` 与 `symbols.MakeHTTPServer` 分别命中两个模块的源文件。`TestSymbol` 命中内部测试 `symbols.TestSymbolInternal` 和外部测试 `symbols_test.TestSymbolExternal`，分别定位 `_test.go` 的实体声明。精确 contributor 探针每个物理位置交付一次，ChooseByName 在这几组查询中各交付两次。 |
| 外部库与 vendor | `uuid.NewString`、`json.MarshalIndent`、`dependency.VendorOnlySymbol` 均在 All Scope 精确正对照中命中，物理文件也标记为库。Project Scope 精确探针均为 0 项。`NewString` 的 Project Scope 模糊结果只有项目内 `NewRandomString`。 |

## GoLand 全样本耗时

| 项目与请求 | 首次新查 P50 / P95（样本数） | 预热后复查 P50 / P95（样本数） | 编辑后首查 P50 / P95（样本数） |
| --- | ---: | ---: | ---: |
| LocalChat 结构视图 | 26.300 / 5034.742（8） | 8.887 / 34.341（240） | 35.135 / 126.292（8） |
| LocalChat Go to Symbol | 94.847 / 353.288（20） | 69.911 / 112.918（600） | 73.054 / 103.382（20） |
| 合成结构视图 | 4.441 / 29.490（5） | 0.495 / 3.580（150） | 5.521 / 22.897（5） |
| 合成 Go to Symbol | 79.173 / 511.098（36） | 61.891 / 307.112（1080） | 75.824 / 363.473（36） |

LocalChat 的 8 个文件包含 355 个文件视图节点和 356 项 PSI 声明，20 条工作区查询共交付 146 项，按查询内物理落点计为 73 项，另有 8 个精确探针。合成工作区的 5 个文件包含 45 个文件视图节点、48 项 PSI 声明和 122 个包视图节点，36 条查询共交付 82 项，按查询内物理落点计为 35 项，另有 17 个精确探针。全部 2070 次复查的集合和回调顺序与对应首次查询一致。LocalChat 的首个文件视图 `internal/model/user.go` 耗时 5034.742 ms。

原始计时分别位于 `%TEMP%\rs-gopls-goland-baseline\localchat-symbols-bench.json` 和 `%TEMP%\rs-gopls-goland-baseline\fixture-symbols-bench.json`，包含每例的次数、原始毫秒样本和分位数。[导航性能对照](PLANS.md#导航性能对照)

索引就绪后，每个不同请求先测一次 `firstMs`，随后对同一输入预热 5 次，保存 30 个 `timingsMs`。最后在该样本的 `editFile` 首行插入同一条注释，调用 `PsiDocumentManager.commitDocument`，记录 `afterEditFirstMs`。计时从提交导航调用开始，涵盖智能读操作、IDE 查询和完整范围提取，单位为毫秒。界面绘制、统计、校验、缓冲区编辑提交和恢复都在计时区间外。首次样本共享同一 GoLand 进程及其已有索引缓存，首个文件查询可能承担 PSI 或方法索引的首次加载。P50/P95 使用最近秩计算。

每轮校验完整目标集合、重复次数和标识符范围。编辑后只接受目标文件的预期行偏移，恢复后再与基准核对。保存否决器仅保护当前文档，`reloadFromDisk` 恢复后逐字节核对磁盘，并检查文档已重新提交且无待保存更改。LocalChat `server` 路径采集前后均为 clean，父仓库原有改动的 Git 状态一致。

## 与 GoLand 直接对照

2026-11-26 在同一 Windows 主机、GoLand `GO-263.7307.354`、`go1.27.4 windows/amd64` 与上述 Project Scope 下，对照 rs-gopls 发布版 `rs-gopls.exe`（SHA-256 `FF7983D4B07E0BC70C392060586406076FD44856B7C3A8AD0657B979BE32E5EB`）。LocalChat 修订为 `532228d3d36bd9f0fffb4bda35c180485b2eadd6`，合成工作区使用上文同一查询配置与编辑序列。[正确性检查器](../tools/check_goland_symbols.py) · [计时对照脚本](../tools/compare_symbols.py)

### 正确性

分别以支持嵌套和使用扁平大纲的客户端进行 stdio 验证。嵌套模式核对层级、源码顺序、声明及标识符 UTF-16 范围，扁平模式核对容器名与标识符落点。工作区搜索核对结果集合、种类、容器与 UTF-16 范围，并比较两次会话的结果顺序。按文件与标识符范围判断同一落点，同一文件中的两条 `init()` 各自保留。

| 工作区 | 大纲通过文件（每种 LSP 模式） | 大纲节点（GoLand / rs-gopls 每种模式） | 工作区查询通过 | 物理落点集合一致 |
| --- | ---: | ---: | ---: | ---: |
| LocalChat | 8/8 | 355 / 355 | 20/20 | 14/20 |
| 合成工作区 | 5/5 | 45 / 43 | 36/36 | 27/36 |

合成工作区的 `Base` 在两侧均命中类型声明与 `Handler` 内嵌入项，共 2 个唯一落点。接收者限定查询 `UserStore.GetAll` 与 `SqlUserStore.GetAll` 分别对齐 7 个与 2 个唯一落点。逐项核验与差异落点见 Cargo 产物目录的 `localchat-symbols-correctness.json`、`fixture-symbols-correctness.json`。

### 有意差异

| 样本或行为 | GoLand 实测 | rs-gopls 发布版 |
| --- | --- | --- |
| 大纲的接收者层级 | `primary/method.go` 在跨文件的 `Record` 父节点下展示 3 个方法。 | 当前 URI 的 3 个方法按源码顺序列在顶层，选中范围均在本文件。 |
| 大纲排序 | 在 `internal/api/api.go` 和 `internal/store/sqlstore/user_store.go` 等文件按种类权重排列节点。 | 两种大纲模式均保持源码顺序。 |
| 大纲中的 `_` | `primary/outline.go` 显示编译期接口断言的空白标识符声明。 | 过滤空白标识符。 |
| 裸嵌入声明边界 | `Embedded` 与 `Base` 的 PSI 声明和标识符范围同宽。以 `Base` 为例，两者均为 `[38,1,38,5]`。 | `Base` 的 `selectionRange` 仍为 `[38,1,38,5]`，`range` 扩至 `[38,1,39,0]`，在父范围内严格包含标识符。 |
| 一般子序列 | `NwFlBcknd` 与 `MkHTSrv` 均为 0 个落点。 | 分别命中 2 个工厂函数、2 个模块的 `MakeHTTPServer`。同一规则也扩展 `NFB`、`PreSave`、`init`、`Box` 与 `T` 的命中。 |
| `pkg.Type.Method` | `model.User.PreSave`、`symbols.Record.HandleRequest` 和 `symbols.Handler.HandleRequest` 均为 0 个落点。 | 各命中 1 个方法或接口方法声明。 |
| 单字母字段与模型全名 | `X` 与 `Record.X` 各为 0 个落点。`APIRoot` 取得 6 个物理落点，其中 5 个来自模型全名的匹配。 | `X` 与 `Record.X` 分别取得 4 个与 2 个匹配落点，均包含 `Record.X`。`APIRoot` 按符号名匹配，取得 3 个落点，与 GoLand 共享 1 个。 |
| 显式导入别名 | `clock` 命中 1 个 `GoImportSpec` 物理落点。 | 项目符号索引过滤显式导入别名，`clock` 返回 0 项。 |
| ChooseByName 重复交付 | LocalChat 的 146 行回调含 73 行重复，合成工作区的 82 行含 47 行重复。`TestSymbol` 的 2 个测试声明各交付 2 次。 | 每次查询内同一物理落点只返回一次，`TestSymbol` 返回 2 项。 |

### 索引就绪后的延迟

共对照 LocalChat 的 18/28 组样本与合成工作区的 30/41 组样本。大纲共同样本对齐层级、顺序和范围，搜索共同样本对齐唯一物理落点集合。GoLand 在智能模式中从导航调用计至完整范围提取，rs-gopls 在含测试视图的包图和符号索引就绪后计 stdio JSON-RPC 往返。双方在同一已索引会话中对每组先查一次，预热 5 次后复查 30 次，再提交未保存的首行注释并计编辑后首查。每轮核对完整结果和预期行偏移，分位数用最近秩。

| 工作区与请求 | 场景 | 每侧样本数 | GoLand P50 / P95（ms） | rs-gopls P50 / P95（ms） |
| --- | --- | ---: | ---: | ---: |
| LocalChat 文档大纲 | 首查 | 4 | 12.7766 / 702.7896 | 0.4674 / 3.9206 |
| LocalChat 文档大纲 | 复查 | 120 | 4.3344 / 34.3412 | 0.8212 / 4.2933 |
| LocalChat 文档大纲 | 编辑后 | 4 | 7.9545 / 97.8107 | 0.7026 / 3.0899 |
| LocalChat 工作区符号 | 首查 | 14 | 75.3697 / 353.2884 | 0.2846 / 0.6029 |
| LocalChat 工作区符号 | 复查 | 420 | 66.9168 / 94.2602 | 0.2480 / 0.4105 |
| LocalChat 工作区符号 | 编辑后 | 14 | 69.4322 / 103.3824 | 1.3689 / 3.1638 |
| 合成文档大纲 | 首查 | 3 | 3.3816 / 4.4413 | 0.2248 / 0.2481 |
| 合成文档大纲 | 复查 | 90 | 0.3862 / 0.5862 | 0.1872 / 0.3088 |
| 合成文档大纲 | 编辑后 | 3 | 3.7816 / 5.5206 | 0.2228 / 0.2766 |
| 合成工作区符号 | 首查 | 27 | 79.1728 / 152.3131 | 0.0946 / 0.1398 |
| 合成工作区符号 | 复查 | 810 | 60.5495 / 125.1907 | 0.0903 / 0.1543 |
| 合成工作区符号 | 编辑后 | 27 | 74.4624 / 133.5104 | 0.4729 / 0.8037 |

### 独立启动与峰值工作集

每个请求在同一临时工作区副本上为双方各启动 5 个独立进程，从创建进程计至首次正确的完整结果。GoLand 复用预建的磁盘索引，以 5 ms 轮询原子写入的结果文件。rs-gopls 的内存索引从空开始，双方共享系统与 Go 磁盘缓存。Windows 外部采样器用 `GetProcessMemoryInfo` 和 Tool Help 进程快照，以 50 ms 为目标间隔记录主进程工作集和包含存活子进程的进程树工作集。表中每项内存值为 5 次运行中最大的采样峰值。

| 工作区与首次请求 | 每侧启动数 | GoLand P50 / P95（ms） | rs-gopls P50 / P95（ms） | GoLand 峰值（主进程 / 进程树，MiB） | rs-gopls 峰值（主进程 / 进程树，MiB） |
| --- | ---: | ---: | ---: | ---: | ---: |
| LocalChat 文档大纲 `internal/store/store.go` | 5 | 20126.5681 / 20722.1949 | 19.7394 / 24.0201 | 2661.89 / 2708.55 | 6.60 / 6.60 |
| LocalChat 工作区符号 `FileBackend` | 5 | 19051.6897 / 19403.0904 | 427.5002 / 440.1122 | 2480.64 / 2491.70 | 10.29 / 35.04 |
| 合成文档大纲 `primary/outline_test.go` | 5 | 16724.0624 / 18031.6121 | 10.9361 / 12.8291 | 2233.77 / 2245.40 | 5.21 / 5.21 |
| 合成工作区符号 `Record` | 5 | 16602.4779 / 17013.0950 | 200.4102 / 202.5634 | 2166.78 / 2201.68 | 6.62 / 25.84 |

GoLand 从空磁盘索引开始时，每套工作区各测一次从启动到首次正确符号搜索的耗时与工作集。

| 工作区与查询 | 首次正确结果（ms） | GoLand 峰值（主进程 / 进程树，MiB） |
| --- | ---: | ---: |
| LocalChat `FileBackend` | 42387.2996 | 3452.72 / 4048.44 |
| 合成工作区 `Record` | 28676.1511 | 2961.53 / 3230.20 |

rs-gopls 的索引就绪查询会话从进程创建开始采样，包含包图加载和 `go.exe` 子进程。同期已运行的 GoLand PID `30100` 提供参考工作集。

| 工作区 | 采样数（GoLand / rs-gopls） | GoLand 参考峰值（主进程 / 进程树，MiB） | rs-gopls 峰值（主进程 / 进程树，MiB） |
| --- | ---: | ---: | ---: |
| LocalChat | 58 / 59 | 162.01 / 162.01 | 11.49 / 35.56 |
| 合成工作区 | 16 / 16 | 162.01 / 162.01 | 7.23 / 25.11 |

完整对照报告位于本次 Cargo 产物目录的 `localchat-symbols-benchmark.json` 和 `fixture-symbols-benchmark.json`，包括共同样本 ID、双方逐次计时、差异落点和逐进程采样峰值。

## 重放

在仓库根目录运行以下 PowerShell 命令。`symbol-queries.json` 中固定文件、作用域、查询与编辑序列。GoLand 输出到临时目录，每个项目独立生成语料和计时 JSON。默认项目配置缺失时，采集器为合成夹具创建 GoLand 模块。

~~~powershell
$sampleDir = (Resolve-Path '.\tools\fixtures\symbol-navigation').Path
$baselineDir = Join-Path $env:TEMP 'rs-gopls-goland-baseline'
$goland = 'E:\Apps\goland-2026.3.0.1.win\bin\goland64.exe'
$script = (Resolve-Path '.\tools\capture_goland_symbols.groovy').Path
$server = 'E:\Code\localchat\server'
$revision = '532228d3d36bd9f0fffb4bda35c180485b2eadd6'
if ((git -C $server rev-parse HEAD) -ne $revision) { throw 'Unexpected LocalChat revision' }
if (git -C $server status --porcelain=v1 --untracked-files=all -- .) { throw 'LocalChat server is dirty' }
New-Item -ItemType Directory -Path $baselineDir -Force | Out-Null
Copy-Item -LiteralPath (Join-Path $sampleDir 'symbol-queries.json') -Destination $baselineDir
$env:RS_GOPLS_SYMBOL_FIXTURE_ROOT = $sampleDir
$env:RS_GOPLS_SYMBOL_PROJECT = ''
$env:RS_GOPLS_BENCHMARK = '1'
$startedAt = [DateTime]::UtcNow
Start-Process -FilePath $goland -ArgumentList @('ideScript', ('"{0}"' -f $script)) -WindowStyle Hidden -Wait
$outputs = @('localchat-symbols.json', 'fixture-symbols.json', 'localchat-symbols-bench.json', 'fixture-symbols-bench.json')
foreach ($name in $outputs) {
    $file = Get-Item -LiteralPath (Join-Path $baselineDir $name) -ErrorAction Stop
    if ($file.LastWriteTimeUtc -lt $startedAt) { throw "Stale capture $name, see symbols-capture-error.txt" }
}
if (git -C $server status --porcelain=v1 --untracked-files=all -- .) { throw 'LocalChat server changed during capture' }
Copy-Item -LiteralPath (Join-Path $baselineDir 'localchat-symbols.json') -Destination '.\tools\fixtures\localchat-goland-symbols.json'
Copy-Item -LiteralPath (Join-Path $baselineDir 'fixture-symbols.json') -Destination (Join-Path $sampleDir 'goland-symbols.json')
~~~

合成工作区已使用同一 SDK 执行以下命令。`go list -test` 列出两个源模块、内部与外部测试视图以及合成测试入口，`go test` 通过。

~~~powershell
$sdkGo = 'C:\Users\rick\go\pkg\mod\golang.org\toolchain@v0.0.1-go1.27.4.windows-amd64\bin\go.exe'
& $sdkGo -C $sampleDir list -test ./primary/... ./secondary/...
& $sdkGo -C $sampleDir test ./primary/... ./secondary/...
~~~

发布版对照使用上述采集的语料和计时 JSON。[stdio 校验命令](plans/2026-11-26-go-document-and-workspace-symbols.md#构建与回归命令) · [性能对照命令](plans/2026-11-26-go-document-and-workspace-symbols.md#性能计量)
