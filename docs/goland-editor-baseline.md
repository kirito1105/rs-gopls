# GoLand 单文件编辑器基线

## 环境和输入

2026-11-27 在 Windows 11 amd64 上采集。GoLand 版本为 `2026.3.0.1`，构建为 `GO-263.7307.354`，安装于 `E:\Apps\goland-2026.3.0.1.win`。`ideScript` 启动了独立的非 headless GoLand JVM。临时项目启用了 Go modules 和 Go SDK `1.27.4`。采集在 `DumbService` 智能模式的读操作中进行，打开文件和移动光标在 EDT 上完成。80 个需要 `Editor` 的查询均通过 `FileEditorManager` 获得对应编辑器，采集时编辑器未获得焦点。基线包含 `go.mod`、查询清单和各文档原始字节的 SHA-256。

夹具使用自包含模块 `example.test/editor`。`boundary.go` 提供同包另一文件的正对照，`recovery.go` 用 build tag 隔离故意保留的无效常量、未解析名称及未闭合函数。`folding_crlf.go` 在磁盘上保留 CRLF，GoLand 的 `Document` 读入后统一换行为 LF。两端坐标都以零基 UTF-16 行列表示，范围终点排除。[夹具及 85 个请求](../tools/fixtures/editor/editor-queries.json)

## 采集结果

| 查询 | 成功 / 总数 | 受阻 | 原生结果数 |
| --- | ---: | ---: | --- |
| `semanticTokens/full` | 5 / 5 | 0 | 512 条注解 |
| `documentHighlight` | 69 / 69 | 0 | 305 处范围，含 4 个空结果查询 |
| `inlayHint` | 7 / 7 | 0 | 50 条提示，含 1 个空结果查询 |
| `foldingRange` | 4 / 4 | 0 | 完整 pass 94 个描述符，快速 pass 79 个 |

### 语义高亮

`GoHighlightingAnnotator` 遍历已提交的 Go PSI，经 `HighlightInfo.fromAnnotation` 提取范围、源码、属性键与 severity。此阶段的结果覆盖 5 个文件，分别为 `editor.go` 257 条、`boundary.go` 12 条、`inlay.go` 168 条、`folding_crlf.go` 61 条以及 `recovery.go` 14 条。词法、rainbow visitor 和最终配色方案绘制属于后续观测范围。

| 样本 | GoLand 运行态与协议映射 |
| --- | --- |
| `semantic-editor` 的包名、导入 | 包名、显式别名和 `.` 的注解为 `GO_PACKAGE`。隐式 `"strings"` 字符串无注解，使用处 `strings.ToUpper` 的限定符有 `GO_PACKAGE`。`clock` 局部遮蔽后为 `GO_LOCAL_VARIABLE`。 |
| `semantic-editor` 的类型与方法 | `Alias` 定义为 `GO_TYPE_SPECIFICATION`，引用为 `GO_TYPE_REFERENCE`。直接结构体与接口各有专用属性。嵌入声明里的 `Embedded` 使用结构体引用属性，显式字面量键 `Embedded` 使用成员属性。方法与接口方法共享函数属性。泛型 `T` 在本 annotator 的采集结果中为空，Track E legend 单独定义 `typeParameter`、`method` 与 `property`。 |
| `semantic-editor` 与 `semantic-recovery` 的重用和内置名 | 同块 `:=` 重用的 `x` 得到 `GO_REASSIGNMENT_IN_SHORT_VAR_DECLARATION`。常量子句中的未遮蔽 `iota` 得到 `GO_BUILTIN_CONSTANT`，局部遮蔽后为 `GO_LOCAL_CONSTANT`，常量子句外的 `iota` 无注解。 |
| 同包跨文件、外部库、dot import | GoLand 可以给 `boundaryValue`、`boundaryCall`、`clock.Second`、`strings.ToUpper` 与 `Pi` 的成员或引用着色。Track E 仅按当前文件能证明的身份生成令牌，保留已知的包限定符和点导入声明。 |
| `semantic-crlf` | `strconv` 选择器限定符在 `[124,21]`，同一行前面的 `🙂` 占两个 UTF-16 代码单元。 |

### 文档高亮

`IdentifierHighlightingComputer` 计算当前文件完整范围，控制流走注册的 Go handler，普通用法走 `HighlightUsagesKt` 与 `GoReadWriteAccessDetector`。`access` 和 `attribute` 保留 IDE 的实际高亮类别，`customHandlerRange` 标识 Go handler 计算出的范围。GoLand 给控制流与普通非值符号使用原生 `READ` 色，Track E 给两类范围投影为 LSP `TEXT`。四个空结果是 `comment-suppression`、`import-dot`、`unresolved-identity` 和 `unresolved-eof`。

| 样本 | GoLand 运行态 |
| --- | --- |
| `assignment-right-edge`、`write-variable`、`write-short-reuse`、`write-shadowed` | 前三项各返回同一组 11 处，右侧贴边能命中。内层 `x` 单独返回 2 处。普通参数、接收者和字段声明读，`var` 与 `const` 声明写。 |
| 通道、左值、字面量 | `ch <- v` 的 `ch` 写、`v` 读，`<-ch` 的 `ch` 读。括号内赋值目标、`*p = y` 中的 `p`、selector 末段字段和结构体键均为写。取地址实参、selector 基底、索引基底和下标、map 键均为读，已有 `range` 绑定的赋值为写。 |
| `function-exit-*`、`function-shadowed-panic` | 外层函数得到 6 处，包括 `defer` / `go` 的完整 `panic(...)` 调用及直接 `return`；闭包得到自己的 3 处。遮蔽后的 `panic` 走普通符号用法。 |
| `loop-labelled-break`、`loop-bare-keyword` | `break Outer` 和 `continue Outer` 与同一标签范围成组。裸跳转按目标关键字拆组。光标移到 `break Outer` 的标签名时走普通标签引用。表达式 `switch`、类型 `switch` 和 `select` 分别与各自 `break` 成组。 |
| `return-named-declaration`、`return-first-expression` | 光标在命名结果 `left` 时叠加普通赋值，光标移到 `return 1 + 2` 的表达式时，GoLand 高亮签名中的类型 `int`。裸 `return` 查询走函数退出点，返回槽位查询省略裸返回。 |
| `return-anonymous-first-type`、`return-anonymous-second-type`、`return-forwarded-call` | 匿名签名的 `int` / `string` 光标得到 28 / 10 处，包括同文件中其他对应类型用法。`slotSource()` 转发结果保留整个调用的槽位范围，同时叠加 `slotSource` 符号各处用法。rs-gopls 重放确认了这两种普通引用叠加。 |

### 内联提示

`ParameterHintsPass` 保留注册的 Go provider、语言开关及方法排除配置。常量走注册的 `GoInlayConstantDefinitionProvider` collector，记录 sink 接收到的内联文本。参数提示、无键字段、命名结果、常量值和文件高亮开关在本次采集中均开启。查询的 UTF-16 范围筛选在采到原生提示之后执行，范围终点排除。名称类映射到 `kind: 2` 及右侧留白，常量提示记录左侧留白。

| 样本 | GoLand 运行态 |
| --- | --- |
| `inlay-calls`（24 条） | `1+2` 显示 `first:`，`x+2` 无提示，`(x+2)*2` 显示 `first:`。无名变参只在首槽显示 `...:`。普通方法与绑定方法值有 `amount:`，方法表达式 `HintReceiver.method(receiver, 4)` 无提示。SDK 的 `len` 和 `append` 分别给出 `v:`、`elems...:`，`make` / `new` 无提示。GoLand 还能为同包另一文件定义的 `boundaryCall(7)` 给出 `amount:`。 |
| `inlay-fields`（8 条） | 同名显式复合值省略 `HintEmbedded:` 或 `Payload:`，对应槽仍参与配对。普通标识符值可获得这两个标签。带键复合值无提示。 |
| `inlay-named-returns`、`inlay-forwarded-returns`（各 3 条） | 仅符合筛选的显式表达式有提示。裸返回、`_` 槽和单调用多结果转发都无提示。 |
| `inlay-constants`（11 条） | 10 个常量提示和 1 个 `len` 形参提示。覆盖 `iota` 与省略 RHS 继承、`H = 0x10` → `= 16`、`B = 1 + 2` → `= 3`、`Reference` → `= 19`。直接同值初值和含换行的求值结果无提示。`len("中文🙂")` 的提示为 `= 4`。同一 Go SDK 编译确认 Go 常量值为 UTF-8 字节数 `10`。追加检查 `intellij.go.impl.jar` 中的 `com.goide.psi.impl.GoExpressionEvaluator.Evaluator.evaluateBuiltinCall`，其字符串分支使用 Java `String.length()`。 |
| `inlay-recovery`、`inlay-utf16-range`（0 / 1 条） | 循环常量与过量无键元素无提示。带 `🙂` 的单行子范围只返回 `[49,21]` 的 `amount:`。 |

### 折叠范围

打开对应文件的 `Editor` 后调用 `FoldingUpdate.getFoldingsFor`，经 `GoFoldingBuilder` 及 `CustomFoldingBuilder` 收集完整和快速描述符。规范化文件保留完整 pass 的 `range`、`placeholder`、PSI 来源、`collapsedByDefault` 和联动 `group`，原始文件另保留快速 pass。折叠模型安装和 UI 绘制属于后续观测范围。IDE 设置默认折叠导入与文件头，默认展开普通文档注释、方法和自定义 region，Go 单语句和空体压缩开关开启，格式字符串压缩关闭。

| 样本 | 完整 / 快速描述符 | GoLand 运行态 |
| --- | ---: | --- |
| `folding-crlf` | 34 / 27 | 导入、语句块、switch/select 与非空子句、分组、类型体、复合字面量、注释、raw string 和自定义 region 均得到描述符。单 spec 跨行的三个分组无外层描述符。空行打断 `//` 组。两片闭包折叠共享一个组，原始字符串范围为 `[121,5]` 至 `[123,1]`。 |
| `folding-editor` | 54 / 47 | 保留单语句缩进折叠的两片区间和默认折叠属性。Track E 把联动的两片映射到连续结构范围，行级投影时将独占一行的 `}` / `)` 留在可见行。 |
| `folding-recovery` | 2 / 2 | 未闭合函数保留完整结构的折叠结果，残缺体无描述符。 |
| `folding-netbeans` | 4 / 3 | 独立文件识别 3 个 NetBeans region，包含嵌套和与普通注释同范围的样本。同范围交付 region 描述符，连续的两个结束标记还能形成一个内嵌普通注释组。 |

`folding-crlf` 先遇到 `//region`，该文件中的 `<editor-fold>` 区间无 NetBeans region 描述符。这与 REA 中平台按首次标记选择 provider 的路径一致。rs-gopls 也由首个匹配的开始或结束标记选定全文件使用的 provider，并在各语法父节点内按栈配对。GoLand 的折叠组与默认展开状态保留为基线元数据，标准 LSP 折叠响应投影起止行列与 `kind`。

运行态补充了原计划中尚待核实的结果，包括返回槽位的类型范围和普通引用叠加、独立与混用两种 region 的结果，以及字符串 `len` 的实际数值。`len` 的提示差异可由追加的安装包检查解释。包级解析能力、IDE 原生着色和折叠联动映射按 Track E 的单文件与协议范围处理。[静态实现依据和 Rust 取舍](plans/2026-11-27-go-editor-single-file-enhancements.md#实现依据)

## rs-gopls 重放

2026-11-27 使用 Windows amd64 release 构建。工作区基准提交为 `9cffb0c5f50ffa1a17876c0a30d5c1504f8b00c9`，二进制包含该提交之上的 Track E 工作区改动。`rs-gopls.exe` 的 SHA-256 为 `fe7b34e282c0e4d1fca3b08ebff61fbdba9d3cff2f51d83d89d85e49bb7c9d38`。重放使用与采集相同的 Go SDK `1.27.4`。脚本先核对 Go 文件、模块和查询清单的原始字节摘要，再以临时工作区打开原始文本，通过真实 stdio LSP 发送查询。[重放脚本](../tools/check_goland_editor.py)

```powershell
cargo check -p rs-gopls --locked
cargo test -p rs-gopls --locked
cargo build -p rs-gopls --release --locked
$editorSdk = 'C:\Users\rick\go\pkg\mod\golang.org\toolchain@v0.0.1-go1.27.4.windows-amd64'
$target = (cargo metadata --format-version 1 --no-deps --locked | ConvertFrom-Json).target_directory
$server = Join-Path $target 'release\rs-gopls.exe'
$editorPreviousPath = $env:Path
try {
    $env:Path = "$(Join-Path $editorSdk 'bin');$editorPreviousPath"
    python tools/check_goland_editor.py $server
} finally {
    $env:Path = $editorPreviousPath
}
```

三条 Cargo 命令均退出 0，零告警。Rust 测试通过 139 项，失败或忽略均为 0。重放退出 0，85 个基线样本全部通过，第二个 LSP 会话额外发送 4 个纯行折叠请求，共 89 个请求，未约定差异为 0。首轮未修复结果经同一核对逻辑复查为 31 条未约定差异，脚本退出 1。可用 `--verbose` 查看逐查询结果及对应行列，也可用 `--report <路径>` 生成 JSON。

| 能力 | 通过 / 查询 | GoLand 原生条目 | rs-gopls 条目 | 匹配 | 单列差异 / 投影 |
| --- | ---: | ---: | ---: | ---: | ---: |
| 语义令牌 | 5 / 5 | 512 | 503 | 487 | 30 |
| 文档高亮 | 69 / 69 | 305 | 301 | 301 | 4 |
| 内联提示 | 7 / 7 | 50 | 49 | 48 | 2 |
| 折叠（含列） | 4 / 4 | 94 | 83 | 83 | 11 |
| 折叠（纯行） | 4 / 4 | 94 | 82 | 82 | 60 |

文档高亮和折叠的匹配按 LSP 投影后的范围及类别计数。折叠投影记录可对应已匹配的范围。纯行的 60 条记录是从含列的 83 个预期范围进一步投影的结果。语义令牌的细分类别和内联常量的不同值计入单列差异。

### 单文件解析边界

- 语义令牌省略 5 个同包跨文件引用，涉及 `boundaryValue`、两处 `boundaryCall`、`PackageVariable`、`F`。另省略 4 个外部包成员 `Second`、`ToUpper`、`Println`、`Itoa`，以及点导入成员 `Pi`。包限定符及点导入声明本身按当前文件核对。
- 文档高亮省略 `Pi`、`Second`、`boundaryValue`、`boundaryCall` 各 1 处。其余 301 处为 153 个 `TEXT`、71 个 `READ` 和 77 个 `WRITE`。
- 内联提示省略 `boundaryCall(7)` 的 1 条 `amount:`，签名位于同包另一文件。

### GoLand 字符串常量求值

- `inlay-constants` 的 `len("中文🙂")` 按 GoLand 的 Java `String.length()` 得到 `= 4`。按 Go 的 UTF-8 字节长度得到的 `= 10` 在相同锚点通过核对，仍使用左侧留白。

### LSP 投影

- 语义令牌细分 9 个 `method` 和 2 个嵌入字段的 `property`，并补充 GoLand annotator 未标出的 5 个 `typeParameter`。另跳过 2 处空白标识符 `_` 及 2 条非符号注解 `go:build`、`0x10`。5 个文件按查询顺序的直接匹配数为 247、10、159、58、13。
- 文档高亮将 101 处控制流或返回槽位和 52 处普通非值符号映射为 `TEXT`。`ReadWrite` 投影为 `WRITE`，命名结果声明与普通引用同范围时保留 `READ`。
- 折叠将 9 组由两片组成的块合为 9 个连续范围。`folding-crlf` 和 `folding-netbeans` 各有 1 个与 region 交叉的普通注释组，由 region 优先。GoLand 的初始折叠状态和组联动保留在采集基线。按 CRLF、editor、recovery、NetBeans 的顺序，含列范围为 32、46、2、3，纯行范围为 32、46、1、3。
- 纯行响应省略所有列字段，59 个范围将独占一行的 `}` 或 `)` 留在可见行，`folding-recovery` 中 1 个同行范围在纯行投影时省略。

本次修复后，隐式导入的限定符查询包含引号内的声明路径。方法表达式使用无参数名的签名，保留无名变参提示。11 个单语句分支的折叠范围从冒号起至语句起点，右边距采用 GoLand 默认的 120 列与 4 列 tab 停靠。混用 region 标记的文件按首个标记选定 provider，未选中风格的标记仍可参加普通注释折叠。

## 重采

在仓库根目录执行。采集器读取临时目录里的 SDK 路径、Go module 及查询清单。打开项目、写入 `.idea`、启动索引及关闭项目均针对临时副本。

```powershell
$sampleDir = (Resolve-Path '.\tools\fixtures\editor').Path
$baselineDir = Join-Path $env:TEMP 'rs-gopls-goland-editor'
$workspace = Join-Path $baselineDir 'workspace'
$goland = 'E:\Apps\goland-2026.3.0.1.win\bin\goland64.exe'
$capture = (Resolve-Path '.\tools\capture_goland_editor.groovy').Path
$sdk = 'C:\Users\rick\go\pkg\mod\golang.org\toolchain@v0.0.1-go1.27.4.windows-amd64'
New-Item -ItemType Directory -Path $workspace -Force | Out-Null
Get-ChildItem -LiteralPath $sampleDir -Filter '*.go' | Copy-Item -Destination $workspace -Force
Copy-Item -LiteralPath (Join-Path $sampleDir 'go.mod'), (Join-Path $sampleDir 'editor-queries.json') -Destination $workspace -Force
@{ sdkHome = $sdk } | ConvertTo-Json | Set-Content -Encoding utf8 (Join-Path $baselineDir 'capture-settings.json')
$startedAt = [DateTime]::UtcNow
Start-Process -FilePath $goland -ArgumentList @('ideScript', ('"{0}"' -f $capture)) -WindowStyle Hidden -Wait
$failure = Get-Item -LiteralPath (Join-Path $baselineDir 'editor-capture-error.txt') -ErrorAction SilentlyContinue
if ($failure -and $failure.LastWriteTimeUtc -ge $startedAt) { throw (Get-Content -Raw $failure.FullName) }
foreach ($name in @('goland-editor-raw.json', 'goland-editor.json')) {
    $result = Get-Item -LiteralPath (Join-Path $baselineDir $name) -ErrorAction Stop
    if ($result.LastWriteTimeUtc -lt $startedAt) { throw "Stale capture $name. See editor-progress.log." }
}
Copy-Item -LiteralPath (Join-Path $baselineDir 'goland-editor.json') -Destination $sampleDir -Force
```

原始结果 `goland-editor-raw.json` 和 `editor-progress.log` 位于 `%TEMP%\rs-gopls-goland-editor`。原始结果包含 IDE 进程和查询阶段信息。规范化文件以查询 `id` 对齐同一输入，保留 IDE 的原生分类、半开 UTF-16 范围和提示文本。[采集器](../tools/capture_goland_editor.groovy) · [规范化基线](../tools/fixtures/editor/goland-editor.json)

夹具在同一 Go SDK 下执行 `go test ./...` 通过。重复采集的规范化查询结果一致，包含范围、类别、标签及顺序。

在同一个 release 二进制上，`python -B tools/check_workspace_updates.py $editorServer --module editor` 通过 4/4 项，`python -B tools/check_workspace_updates.py $editorServer` 通过 32/32 项，两次均退出 0。专项覆盖能力协商、未保存版本与 `didClose`、CRLF 与残缺语法，以及包图放行前后响应一致性。[Track E 完成记录](plans/2026-11-27-go-editor-single-file-enhancements.md#完成记录)
