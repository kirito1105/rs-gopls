# Go DocumentSymbol 与 WorkspaceSymbol 开发计划

| 项目 | 内容 |
| --- | --- |
| 状态 | 已完成 |
| 创建日期 | 2026-11-26 |
| 用户确认 | 已确认（2026-11-26，确认按默认取舍推进） |
| 更新日期 | 2026-11-26 |
| 完成日期 | 2026-11-26 |

## 目标与范围

为 Zed 接入 `textDocument/documentSymbol` 单文件大纲与面包屑导航，以及 `workspace/symbol` 项目级符号搜索。两个请求以当前编辑内容为准，工作区搜索支持 `$/cancelRequest`。用 GoLand `GO-263.7307.354` 的 Structure View 和 Go to Symbol 实测作为对照。[项目愿景](../vision.md)

### 本阶段覆盖范围

1. **DocumentSymbol（`textDocument/documentSymbol`）**按客户端 `hierarchicalDocumentSymbolSupport` 返回 `DocumentSymbolResponse::Nested` 或 `DocumentSymbolResponse::Flat`（`SymbolInformation` 列表）。仅枚举当前 URI 内的声明，排除 `package`、`import` 和函数体内局部声明。结构体字段和接口方法、嵌入项归入所属类型的 `children`，递归展开字段内联的 `struct` / `interface` 成员。
2. **WorkspaceSymbol（`workspace/symbol`）**返回带完整 `Location`（文件 URI 与标识符的 UTF-16 范围）和 `containerName` 的 `SymbolInformation` 列表。覆盖活跃 Go 包的包级 `func`、`type`、`const`、`var`、具体接收者方法、命名及嵌入结构体字段、接口方法与命名嵌入接口。`init` 纳入工作区索引。排除空名、`_`、局部变量、参数、类型参数和导入项，显式 import 别名也排除。

### 待确认事项

1. **单文件范围与接收者方法层级**。独立接收者方法是顶层声明，可与接收者类型分处不同文件。默认把方法放在当前文件的顶层，显示名如 `(*T).M` 或 `(T).M`，扁平模式的 `containerName` 为接收者基类型名 `T`。大纲用 `DocumentSymbol.range` 判断光标所在的声明，父节点范围覆盖全部 `children`。GoLand Structure View 可跨文件把方法挂在接收者类型下，并按种类权重排序。本阶段默认采用 LSP 单文件范围与源码顺序。[LSP Document Symbols](https://github.com/microsoft/language-server-protocol/blob/gh-pages/_specifications/lsp/3.17/language/documentSymbol.md)
2. **大纲中的空白标识符**。默认跳过 `_`，包括 `var _ I = (*T)(nil)` 形式的编译期接口断言。确认时可选择按 GoLand 原始枚举保留这类声明。
3. **工作区搜索范围**。默认对齐 GoLand 的 Project Scope。覆盖工作区活跃包、`go.work` 生效的 `use` 模块、内部 `_test.go` 与外部 `pkg_test` 测试视图。按物理文件的 `path_key` 去重，排除合成测试入口、GOROOT、外部 module cache 依赖及 `vendor/`。
4. **空查询与匹配顺序**。默认 `""` 返回 `[]`，对应 GoLand 的 `MyViewModel#canShowListForEmptyPattern == false`。名称和限定名支持大小写不敏感匹配。查询可写为简单名、前缀、子串、驼峰缩写、子序列或 `pkg.Symbol`、`Type.Method`、`pkg.Type.Method`。默认优先级为精确命中、前缀、子串、驼峰、一般子序列，同级按名称、容器、规范化路径及 UTF-16 范围起止点依次决胜。

### 本阶段之外

后续可扩展 `workspaceSymbol/resolve` 延迟范围解析、外部依赖和 SDK 搜索、基于编辑距离的拼写容错、函数体内局部类型索引，以及大纲的 GoDoc `@Deprecated` 标签解析。

## 验收标准

### 单文件大纲

- [x] **声明覆盖与层级**。用合成文件核对分组 `const` / `var`、多名称 spec、显式与省略表达式的 `iota`、结构体、接口、定义类型和类型别名。包级 `func` 保留同一文件中的多个 `init()`，接收者方法按源码位置列在顶层。结构体的多名称字段拆分为独立条目，覆盖匿名嵌入字段、内联结构体与接口成员、接口方法与嵌入接口。`package`、`import`、`_` 和函数体内局部变量按已确认的范围过滤。
- [x] **种类、详情与位置**。核对每类声明的 `SymbolKind` 和嵌套模式的 `detail`。`range` 覆盖完整声明并严格包含标识符的 `selectionRange`，父范围覆盖全部子范围。用 CRLF 文件与同一行标识符前的非 BMP 字符核对 UTF-16 起止坐标及实际选中文本。
- [x] **客户端能力协商**。初始化声明 `documentSymbolProvider`。`hierarchicalDocumentSymbolSupport` 为 `true` 时返回带 `children` 的嵌套 `DocumentSymbol`，为 `false` 或缺省时返回带 `containerName` 的扁平 `SymbolInformation`。两种模式均保持当前 URI 内的源码顺序和精确跳转落点。
- [x] **缓冲区与错误恢复**。未保存更改即刻反映到名称、层级和范围。编辑为未闭合函数、未闭合结构体或局部语法错误时，仍返回同文件中可辨认的其余有效声明，进程能继续响应后续请求。

### 工作区符号搜索

- [x] **项目范围与物理文件去重**。初始化声明 `workspaceSymbolProvider`。在 `go.work` 多模块、同名包、内部 `_test.go` 和外部 `pkg_test` 场景中命中全部预期声明。普通包与测试视图共享的文件只产出一份符号，合成测试入口、GOROOT、外部 module cache 和 `vendor/` 中的同名符号均被过滤。
- [x] **覆盖、匹配与排序**。包级各类声明、接收者方法、结构体命名和嵌入字段、接口方法及嵌入项均有可核对的 `name`、`kind`、`containerName` 和完整 UTF-16 `Location`。分别验证简单名、大小写不敏感的前缀和子串、驼峰缩写、子序列和三种限定查询。空查询返回 `[]`。索引排除 `_`、局部变量、参数、类型参数和显式 import 别名。对同分与同名候选验证跨会话顺序一致。
- [x] **编辑、关闭与磁盘同步**。`didChange` 后，下一查询立即反映符号新增、改名、删除及位置移动。`didClose` 后恢复当前磁盘符号和位置，关闭从未保存的草稿时移除其符号。覆盖项目根下新建未保存 `.go`、磁盘文件创建/修改/删除，以及 `workspace/didChangeWatchedFiles` 触发的包图与 `go.work` 范围刷新。打开时磁盘通知仍以缓冲区视图为准。
- [x] **异步加载与取消**。用可暂停的包加载和多批次符号数据验证非空查询等待最新包图、扫描与索引更新分批推进。等待包图、建索引和查询期间的 `$/cancelRequest` 均按请求 ID 返回一次 `-32800`。包图等待期间发生编辑时按最新内容启动搜索，已开始扫描的过期查询按 `ContentModified` 结束。并发查询和后续重试均得到完整结果。

### GoLand 对照与回归

- [x] **可重放基线与正确性**。在运行中的 GoLand `GO-263.7307.354` 上采集 `E:\Code\localchat\server`（修订 `532228d3d36bd9f0fffb4bda35c180485b2eadd6`）和 `tools/fixtures/symbol-navigation` 的 Structure View 与 Go to Symbol 基线。对照两个 LSP 请求在共同样本上的完整符号集合和 UTF-16 范围，记录 LSP 与 IDE 有意采用不同规则的样本及双方实际结果。
- [x] **性能对照**。在固定条件下记录两个请求在 GoLand 和 rs-gopls 发布版索引就绪后的新查询、重复查询和编辑后查询的 P50/P95、样本数及缓存状态。记录双方启动到首次正确结果的耗时和主进程、进程树峰值工作集。[导航性能对照](../PLANS.md#导航性能对照)
- [x] **构建与历史回归**。`cargo check --workspace --locked` 和 `cargo test --workspace --locked` 零告警通过，发布版通过声明、类型定义、引用、实现、Hover、SignatureHelp 六项既有 LSP 回归，以及阶段 6 声明检查和工作区更新回归。

## 实现依据

### REA 产物证据

GoLand `2026.3.0.1` 安装于 `E:\Apps\goland-2026.3.0.1.win`，`build.txt` 为 `GO-263.7307.354`。用 REA `6.7.0 inspect-artifact` 核对 JAR 清单，用安装包的 `jbr/bin/java.exe` 与 `lib/intellij.libraries.asm.jar` 中的 `org.jetbrains.org.objectweb.asm.util.Textifier` 核对关键字节码。以下路径相对于安装目录。

| 安装包文件 | 可核查内容 | REA Evidence ID |
| --- | --- | --- |
| `plugins/go-plugin/lib/go-plugin.jar` | `META-INF/plugin.xml` 注册 `lang.psiStructureViewFactory`、`gotoSymbolContributor` 和 `gotoClassContributor`。 | `ev_68ccc094c4ff1a2b333440868b1980c2641b9fdf3b91c5acc27ccdb85e7d4223` |
| `plugins/go-plugin/lib/modules/intellij.go.impl.jar` | `intellij.go.impl.xml`、Go PSI、结构树、搜索贡献者和 stub 索引。 | `ev_6c7c39a219d2e02923e7b819150326c434f3135777e22df0b1f7e49c6cdda5ee` |
| `plugins/go-plugin/lib/intellij.go.frontback.jar` | `com.goide.parser.GoParser` 和 `com.goide.GoIcons`。 | `ev_85643f48bd423aaf82faacb0fea6936d24f4e8b267f8c28c1a4b2f32947a05e5` |
| `plugins/platform-structureView-plugin/lib/modules/intellij.platform.structureView.jar` | 通用结构视图的 PSI 图标、树过滤与排序。 | `ev_df6c947d298bb7edcfeb2ec39f4c7b45ee8ecaadba299289e0bbaddfa5eada13` |
| `lib/intellij.platform.lang.impl.jar` | Go to Symbol / Go to Class 模型、名称匹配与搜索作用域。 | `ev_0f68e2c379dad456d286dc369987fd766ebe332baf579f8742ac8e74b1e5a1af` |
| `lib/intellij.platform.searchEverywhere.jar` | `SeSymbolsProviderFactory#getItemsProvider` 连接 `GotoSymbolModel2`。 | `ev_804849bad2d46165d4b2820368844706e48c081df2c60685bab36348330341a9` |

### 已确认的文件结构视图行为

- `GoStructureViewFactory#getStructureViewBuilder` 创建结构视图，`GoStructureViewFactory$Model` 与 `GoStructureViewFactory$Element#getChildrenBase` 提供模型与子节点。`addFilesChildren` 通过 `GoFile#getTypes`、`getConstants`、`getVars` 和 `getFunctions` 枚举文件声明。`addTypesChildren` 展开类型成员，`GoPsiImplUtil#calcMethods` 通过 `GoMethodIndex` 查找接收者方法，方法可位于同包的其他文件。
- `GoPsiImplUtil#getFieldDefinitions` 读取 `GoFieldDeclaration` 的命名字段定义及匿名字段定义。`GoInterfaceTypeImpl#getMethodSpecList` 单独读取方法节点。`GoParser#MethodSpecConstraintElem` 在 `MethodSpec`（`MethodSpec_0` / `MethodSpec_1`）和 `ConstraintElem`（含 `ConstraintTerm`）之间分支。
- `GoPsiImplUtil#getTypeSpecIcon` 与 `GoIcons` 提供类型种类与图标依据。`GoStructureViewFactory$Element#getPresentationTextInner` 通过 `GoPsiPresentationBuilder#build` 生成单行文本，`getLocationString` 提供位置显示。`GoNaturalSorter$1#getWeight` 的种类权重依次为常量、变量、类型、字段、函数或方法。

### 已确认的工作区符号搜索行为

- `GoSymbolContributor` 使用 `GoAllPublicNamesIndex`、`GoAllPrivateNamesIndex`、`GoNonPackageLevelNamesIndex`。`GoTypeContributor` 使用 `GoTypesIndex`。`GoGotoContributorBase#processNames` / `processElementsWithName` 先枚举名称，再在搜索作用域中取得同名元素。
- `GoNamedStubElementType#indexStub` 过滤空名、`_` 和 `shouldIndex() == false` 的声明。`GoLightStubBuilder` 跳过 `BLOCK` 子树，因此函数体内局部符号未进入此索引路径。
- `AbstractGotoSEContributor#getInitialSelectedScope` 负责初始作用域选择，也读取已保存的选择。`FindSymbolParameters#searchScopeFor` 区分 Project Scope 与包含库的 All Scope。项目作用域没有 `_test.go` 后缀过滤。`GoSdkLibraryRootProvider`、`VgoDependencyWorkspaceFileIndexContributor` 和 `GoVendorWorkspaceFileIndexContributor` 将 Go SDK、外部模块依赖和 vendor 归为外部库。`VgoWorkspaceModelUpdater#addWorkspaceModule` 把使用模块关联到 `go.work` 对应的工作区。
- `GoNamedElementImpl#getQualifiedName` 生成 `pkg.Name`、`Receiver.Method`、`Interface.Method` 等限定名，`GoItemPresentation#getLocationStringInner` 提供位置文本。`DefaultChooseByNameItemProvider#buildNamePattern`、`processByNames` 和 `matchQualifiedName` 结合 `MinusculeMatcher` 处理名称与限定名。`MyViewModel#canShowListForEmptyPattern` 返回 `false`。

### Rust 实现方案与取舍

1. **共用语法提取**。在 `src/symbols.rs` 增加按源码顺序保存的 `OutlineSymbol` / `OutlineKind` 及工作区条目投影。保留 `FileSymbols` 哈希表供现有类型解析使用。有序记录直接从 Tree-sitter AST 提取声明跨度、名称跨度、签名或类型详情、接收者和父类型信息，以及递归子成员，无需类型检查。分组声明按 spec 取范围，多名称条目共享 spec 范围并各自定位。裸嵌入项的声明与选中范围同宽时，默认纳入所属声明体中的相邻语法分隔符或边界符，使 `range` 严格包含 `selectionRange`。使用 `Source` 的行索引把字节跨度转为 UTF-16。提取限制在语法声明和可恢复的错误节点，跳过函数体内声明和缺失名称的节点。
2. **`src/semantic/document_symbols.rs`**。在 `Source` 上用惰性的 `OnceCell` 缓存单文件大纲。优先读取编辑缓冲区，关闭后回到磁盘。嵌套与扁平结果使用同一份提取记录。扁平位置以标识符为落点，包级条目用包名作 `containerName`，成员用所属类型，接收者方法用基类型。常量、变量、函数、方法、结构体、接口和字段依次映射为 `Constant`、`Variable`、`Function`、`Method`、`Struct`、`Interface` 和 `Field`，其余定义类型和别名用 `Class`。`detail` 从源码提取签名、类型、别名目标、字段 tag 或初始化表达式，省略表达式的 `iota` 条目沿用分组上下文。单个命名嵌入接口项以 `Interface` 作为子项，`~T` 和 `A | B` 作为约束语法保留在所属类型中。
3. **`src/semantic/workspace_symbols.rs`**。复用现有 `Workspace` 的项目根与包清单，工作区符号查询要求含测试视图的当前包图。筛选真实项目文件，排除外部路径和合成入口。把工作区内的打开草稿并入候选，通过目录和语法包名确定归属。项目根有效而磁盘包清单为空时，仍可索引项目内可归属的首个草稿。按 `path_key` 缓存文件符号，以磁盘内容指纹或缓冲区文档版本和内容标识区分来源。`didOpen` / `didChange` 更新覆盖内容，`didClose` 恢复磁盘，watcher 使对应磁盘条目失效。包图换代后重算候选集合并剔除移出的文件，复用仍有效的条目。索引按文件分批构建和查询，按简单名、包名和所属类型匹配与排序。包级符号的 `containerName` 使用包名，成员使用包名与所属类型，`Location` 随响应一次返回。[LSP Workspace Symbols](https://github.com/microsoft/language-server-protocol/blob/gh-pages/_specifications/lsp/3.17/workspace/symbol.md)
4. **`src/main.rs` 请求入口**。初始化读取 `hierarchical_document_symbol_support` 并声明 `document_symbol_provider`、`workspace_symbol_provider`。`DocumentSymbolRequest` 同步返回单文件结果。`WorkspaceSymbolRequest` 解析查询文本，空查询直接返回，其他查询复用 `QueryProgress::Pending` 与 `Searching` 的后台包图等待及可交错批处理队列。以包图代次和文件版本约束索引提交及响应，等待中的查询使用最新图，扫描中的过期查询复用 `ContentModified`。取消时清除对应等待或搜索任务，共享包图加载可继续服务其他请求。

### 运行采集验证的问题

- 核对 `GoSymbolContributor` 对 `init()`、匿名嵌入字段、接口嵌入项和显式 import 别名的实际命中。比较简单名、`pkg.Type.Method` 和完整导入路径查询的落点与顺序。
- 核对多模块 `go.work` 的同名包、`_test.go` 和外部测试包在 GoLand Project Scope 下的实际落点集合与重复情况。
- 核对结构视图中跨文件接收者方法、多名称字段、省略表达式的 `iota` 和 `~T` / `A | B` 泛型约束的实际条目、层级和范围。[运行采集结果](../goland-symbols-baseline.md)

## 实施步骤

1. **采集 GoLand 运行基线**。收到确认后记录确认结果并将计划设为「进行中」。新增 `tools/capture_goland_symbols.groovy` 与 `tools/fixtures/symbol-navigation`，在运行中的 GoLand `GO-263.7307.354` 上采集 `localchat/server` 与合成夹具的数据。将可重放语料和待验证推断的实测结果归档到 `docs/goland-symbols-baseline.md`。
2. **扩展语法符号提取**。在 `src/symbols.rs` 增加有序声明、`range`、`selection_range`、细分种类、`detail`、容器与递归成员。用单元测试验证分组、多名称、嵌入、多个 `init`、约束和错误恢复。
3. **接入 `textDocument/documentSymbol`**。在 `src/semantic/document_symbols.rs` 和 `src/main.rs` 实现缓存及嵌套、扁平投影。单元和 stdio 集成测试覆盖初始化协商、CRLF、非 BMP、未保存编辑和语法错误。
4. **接入 `workspace/symbol`**。在 `src/semantic/workspace_symbols.rs` 和 `src/main.rs` 实现项目范围过滤、测试包去重、增量文件缓存、分批索引、匹配排序和完整位置响应。接入包图等待、编辑/关闭、watcher 失效及取消。
5. **自动化 GoLand 对照和更新回归**。新增 `tools/check_goland_symbols.py`，扩展 `tools/check_workspace_updates.py`。新增 `tools/compare_symbols.py` 对照 GoLand 与发布版的启动、新查、复查、编辑后延迟和峰值工作集。
6. **全量验证与交付**。逐项勾选已通过的验收，记录命令、样本数、差异和实测结果，按需更新 `README.md` 与 `docs/vision.md`。全部通过后填写完成日期。

## 验证计划

### 样本与运行基线

- 固定同一 Windows 主机、GoLand `GO-263.7307.354`、rs-gopls 发布版提交及二进制 SHA-256、Go SDK `go1.27.4`、搜索范围、打开文件和编辑序列。两侧核对实际 Go SDK 路径及 `go version`，同时记录影响构建视图的 `GOOS`、`GOARCH`、构建标签与 `GOWORK`。
- 真实样本使用 `E:\Code\localchat\server` 的 `532228d3d36bd9f0fffb4bda35c180485b2eadd6`。`tools/fixtures/symbol-navigation` 覆盖分组声明、泛型约束、跨文件方法、CRLF、非 BMP 字符、测试视图、`go.work` 同名包、外部库和 vendor 排除、空查询、同分排序及编辑态错误。用相同的查询、打开文件和变更顺序覆盖新增、改名、删除、关闭恢复与取消。
- `tools/capture_goland_symbols.groovy` 沿用仓库的 `ideScript` 采集入口。脚本从 `$env:TEMP\rs-gopls-goland-baseline\symbol-queries.json` 读取两套样本的项目根、文档 URI、搜索范围、查询文本和编辑步骤，在 IDE 中执行 Structure View 与 Go to Symbol 查询。存储每次完整条目、容器、原始 PSI 范围和标识符范围，并转为同一 UTF-16 坐标口径。输出采集参数和重放方式到 `docs/goland-symbols-baseline.md`，交由 Python 检查器读取结构化基线。[既有 Groovy 采集方式](../goland-hover-signature-baseline.md#重新采集)

```powershell
$goland = 'E:\Apps\goland-2026.3.0.1.win\bin\goland64.exe'
$capture = (Resolve-Path '.\tools\capture_goland_symbols.groovy').Path
Start-Process -FilePath $goland -ArgumentList @('ideScript', ('"{0}"' -f $capture)) -WindowStyle Hidden
```

### 构建与回归命令

日常先运行包级检查，再跑受影响的测试。最终运行工作区检查、工作区测试及发布构建。沿用现有 toolchain、profile、features、target 和环境配置的 Cargo 产物目录。

```powershell
cargo check -p rs-gopls --locked
cargo test -p rs-gopls --locked
cargo check --workspace --locked
cargo test --workspace --locked
cargo build -p rs-gopls --release --locked
```

以下命令以 GoLand 脚本已采集基线且测试进程已选中 `go1.27.4` 为前提。新增符号检查器沿用现有 `--workspace` / `--fixture` 参数约定。

```powershell
$target = (cargo metadata --format-version 1 --no-deps --locked | ConvertFrom-Json).target_directory
$server = Join-Path $target 'release\rs-gopls.exe'
$localchat = 'E:\Code\localchat\server'
$symbolFixture = (Resolve-Path '.\tools\fixtures\symbol-navigation').Path

python -B .\tools\check_goland_symbols.py $server --workspace $localchat --fixture .\tools\fixtures\localchat-goland-symbols.json --json (Join-Path $target 'localchat-symbols-correctness.json')
python -B .\tools\check_goland_symbols.py $server --workspace $symbolFixture --fixture (Join-Path $symbolFixture 'goland-symbols.json') --json (Join-Path $target 'fixture-symbols-correctness.json')
python -B .\tools\check_workspace_updates.py $server
python -B .\tools\check_definitions.py $server --stage 6 --workspace $localchat
python -B .\tools\check_goland_definitions.py $server --workspace $localchat --method definition
python -B .\tools\check_goland_definitions.py $server --workspace $localchat --method typeDefinition
python -B .\tools\check_goland_references.py $server --workspace $localchat
python -B .\tools\check_goland_implementations.py $server --workspace $localchat
python -B .\tools\check_goland_hover.py $server --workspace $localchat
python -B .\tools\check_goland_signature_help.py $server --workspace $localchat
```

### 性能计量

- `tools/compare_symbols.py` 对照 Groovy 样本与发布版 stdio 结果。每次计时都核对完整目标集合。嵌套大纲核对声明范围和标识符范围，扁平大纲与工作区搜索核对跳转范围。跨文件方法挂载、种类排序等有意差异单列，耗时比较使用结果一致的共同样本。
- 按请求和样本分别记录索引就绪后的首次新查询、同查询预热后的复查、相同未保存编辑后的首次查询。记录有效样本数、P50/P95（ms）、预热次数、索引与结果缓存状态。GoLand 从调用导航到完整结果返回，记录是否包含界面操作。rs-gopls 从发出请求到收到完整 JSON-RPC 响应，记录 stdio 开销包含在内。编辑后测量从双方都已接收编辑后提交查询开始，核对更新后的名称和位置。
- 双方用独立启动会话和相同的预选查询，从创建进程计到首次返回通过校验的完整结果，记录启动时的索引缓存状态及样本数、P50/P95。用同一外部采样器按固定间隔采集主进程与整个进程树（含 `go list` 等子进程）的峰值工作集，记录采样方法、间隔、区间和 MiB。将原始样本与结果归档到 `docs/goland-symbols-baseline.md`。[导航性能对照](../PLANS.md#导航性能对照)

性能采集沿用既有 `RS_GOPLS_BENCHMARK=1` 开关。Groovy 脚本保存两套样本的原始计时，以下为 Python 对照入口。

```powershell
$baseline = Join-Path $env:TEMP 'rs-gopls-goland-baseline'
python -B .\tools\compare_symbols.py $server --workspace $localchat --fixture .\tools\fixtures\localchat-goland-symbols.json --goland-bench (Join-Path $baseline 'localchat-symbols-bench.json') --json (Join-Path $target 'localchat-symbols-benchmark.json')
python -B .\tools\compare_symbols.py $server --workspace $symbolFixture --fixture (Join-Path $symbolFixture 'goland-symbols.json') --goland-bench (Join-Path $baseline 'fixture-symbols-bench.json') --json (Join-Path $target 'fixture-symbols-benchmark.json')
```

## 完成记录

- 2026-11-26 复核六个 JAR 的 REA Evidence ID、插件注册及关键 ASM Textifier 观察后创建计划。同日收到用户确认，按默认取舍采集结构视图与项目符号搜索基线，并接入两个 LSP 请求。
- 2026-11-26 在同一 Windows 主机完成发布验收。固定 GoLand `GO-263.7307.354`、`go1.27.4 windows/amd64`、LocalChat 修订 `532228d3d36bd9f0fffb4bda35c180485b2eadd6` 与 Project Scope。发布产物 `target/release/rs-gopls.exe` 的 SHA-256 为 `FF7983D4B07E0BC70C392060586406076FD44856B7C3A8AD0657B979BE32E5EB`。[GoLand 实测条件](../goland-symbols-baseline.md)

### 交付文件

- **Rust 实现**覆盖请求分派、包图和缓冲区同步、符号提取、单文件大纲及工作区搜索。[main.rs](../../src/main.rs) · [semantic.rs](../../src/semantic.rs) · [workspace.rs](../../src/workspace.rs) · [symbols.rs](../../src/symbols.rs) · [document_symbols.rs](../../src/semantic/document_symbols.rs) · [workspace_symbols.rs](../../src/semantic/workspace_symbols.rs)
- **Rust 测试**覆盖能力协商、索引就绪与取消，提取和缓存单测随实现模块交付。Hover 与 SignatureHelp 的既有测试同步使用带版本的缓冲区接口。[symbol_request_tests.rs](../../src/symbol_request_tests.rs) · [test_support.rs](../../src/test_support.rs) · [hover.rs](../../src/semantic/hover.rs) · [signature_help.rs](../../src/semantic/signature_help.rs)
- **GoLand 采集与夹具**包含 Groovy 采集入口、LocalChat 语料及带查询配置的双模块工作区。[采集脚本](../../tools/capture_goland_symbols.groovy) · [LocalChat 语料](../../tools/fixtures/localchat-goland-symbols.json) · [symbol-navigation](../../tools/fixtures/symbol-navigation)
- **校验与性能脚本**覆盖 stdio 基线重放、工作区更新和计时对照，并复用带大纲能力协商的 LSP 客户端与 Windows 工作集采样器。[check_goland_symbols.py](../../tools/check_goland_symbols.py) · [check_workspace_updates.py](../../tools/check_workspace_updates.py) · [compare_symbols.py](../../tools/compare_symbols.py) · [compare_definitions.py](../../tools/compare_definitions.py) · [compare_workspace_definitions.py](../../tools/compare_workspace_definitions.py)

### 对齐修复

- 命名接口嵌入项纳入索引。`Base` 查询返回类型声明和 `Handler` 内的嵌入位置。
- 接收者限定的查询支持模糊匹配，同档优先采用更精确的限定部分。`UserStore.GetAll` 与 `SqlUserStore.GetAll` 分别匹配 GoLand 的 7 个和 2 个唯一落点。
- 裸嵌入项的 `range` 在父声明内延伸到相邻换行或分隔符，严格包含标识符的 `selectionRange`。嵌套大纲保留父节点的包含关系，扁平大纲继续用标识符定位。

### 构建与回归

`cargo check -p rs-gopls --locked`、`cargo check --workspace --locked`、`cargo test --workspace --locked` 和 `cargo build -p rs-gopls --release --locked` 均通过，工作区检查和测试零告警，共通过 39 个 Rust 测试。合成工作区以固定 SDK 通过 `go list -test` 和 `go test`。

| 验证项目 | 通过结果 |
| --- | --- |
| `check_goland_symbols.py` · LocalChat | 8 个文件的嵌套与扁平大纲均通过，每种模式 355 个符号。20/20 条工作区查询通过，其中 14 条物理落点集合与 GoLand 一致。 |
| `check_goland_symbols.py` · 合成工作区 | 5 个文件的嵌套与扁平大纲均通过，每种模式 43 个符号。36/36 条工作区查询通过，其中 27 条物理落点集合与 GoLand 一致。 |
| `check_workspace_updates.py` | 23/23。新增覆盖未保存符号增删改移、未闭合函数和结构体、关闭恢复、草稿、watcher 刷新、`go.work` 切换、等待包图期间编辑与取消，以及 512 文件上的并发查询和重试。取消各返回一次 `-32800`，编辑造成的过期扫描返回 `-32801`。 |
| `check_definitions.py --stage 6` | 合成声明 124/124，LocalChat 声明 16/16。 |

两次初始化会话分别校验大纲模式和跨会话查询顺序。正确性明细与回归日志保存在 Cargo 产物目录的 `localchat-symbols-correctness.json`、`fixture-symbols-correctness.json` 和 `symbols-regressions.log`。

| 六项历史 LSP 回归 | 通过结果 |
| --- | --- |
| `check_goland_definitions.py --method definition` | 20/20，19 个落点一致，1 个既有 IDE 包级差异 |
| `check_goland_definitions.py --method typeDefinition` | 34/34，32 个落点一致，2 个预期空结果 |
| `check_goland_references.py` | 34/34 |
| `check_goland_implementations.py` | 20/20，沿用 3 个 GoLand 依赖 `testdata` 落点排除 |
| `check_goland_hover.py` | 21/21 |
| `check_goland_signature_help.py` | 13/13，另通过编辑、关闭恢复和取消验证 |

### 性能实测

`compare_symbols.py` 的两次运行均通过。LocalChat 的 28 组样本有 18 组进入对照，合成工作区的 41 组样本有 30 组进入对照。大纲核对层级、顺序和范围，工作区搜索对照唯一物理落点集合。原始计时和差异落点见 Cargo 产物目录的 `localchat-symbols-benchmark.json` 与 `fixture-symbols-benchmark.json`。[GoLand 直接对照](../goland-symbols-baseline.md#与-goland-直接对照)

GoLand 在索引就绪的智能模式下计导航调用与完整范围提取。rs-gopls 在含测试视图的包图与符号索引就绪后计 stdio JSON-RPC 往返。每个样本首查一次，预热 5 次后复查 30 次，双方提交同一条未保存的首行注释后再测一次。P50 / P95 使用最近秩，单位毫秒。

| 工作区与请求 | 样本数（首查 / 复查 / 编辑后） | GoLand 首查 | rs-gopls 首查 | GoLand 复查 | rs-gopls 复查 | GoLand 编辑后 | rs-gopls 编辑后 |
| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| LocalChat 文档大纲 | 4 / 120 / 4 | 12.7766 / 702.7896 | 0.4674 / 3.9206 | 4.3344 / 34.3412 | 0.8212 / 4.2933 | 7.9545 / 97.8107 | 0.7026 / 3.0899 |
| LocalChat 工作区符号 | 14 / 420 / 14 | 75.3697 / 353.2884 | 0.2846 / 0.6029 | 66.9168 / 94.2602 | 0.2480 / 0.4105 | 69.4322 / 103.3824 | 1.3689 / 3.1638 |
| 合成文档大纲 | 3 / 90 / 3 | 3.3816 / 4.4413 | 0.2248 / 0.2481 | 0.3862 / 0.5862 | 0.1872 / 0.3088 | 3.7816 / 5.5206 | 0.2228 / 0.2766 |
| 合成工作区符号 | 27 / 810 / 27 | 79.1728 / 152.3131 | 0.0946 / 0.1398 | 60.5495 / 125.1907 | 0.0903 / 0.1543 | 74.4624 / 133.5104 | 0.4729 / 0.8037 |

### 独立启动与峰值工作集

双方在同一份临时工作区副本上分别启动独立进程，从进程创建计至首个通过校验的完整结果。GoLand 复用预建磁盘索引，以 5 ms 轮询检测结果文件。rs-gopls 的内存索引从空开始，双方共享系统与 Go 磁盘缓存。Windows 外部采样器以 50 ms 为目标间隔，记录主进程工作集和包含存活子进程的进程树工作集。下表内存列取各侧 5 次启动的最大采样峰值。

| 工作区与首次请求 | 启动数（GoLand / rs-gopls） | GoLand P50 / P95（ms） | rs-gopls P50 / P95（ms） | GoLand 峰值（主进程 / 进程树，MiB） | rs-gopls 峰值（主进程 / 进程树，MiB） |
| --- | ---: | ---: | ---: | ---: | ---: |
| LocalChat 文档大纲 `internal/store/store.go` | 5 / 5 | 20126.5681 / 20722.1949 | 19.7394 / 24.0201 | 2661.89 / 2708.55 | 6.60 / 6.60 |
| LocalChat 工作区符号 `FileBackend` | 5 / 5 | 19051.6897 / 19403.0904 | 427.5002 / 440.1122 | 2480.64 / 2491.70 | 10.29 / 35.04 |
| 合成文档大纲 `primary/outline_test.go` | 5 / 5 | 16724.0624 / 18031.6121 | 10.9361 / 12.8291 | 2233.77 / 2245.40 | 5.21 / 5.21 |
| 合成工作区符号 `Record` | 5 / 5 | 16602.4779 / 17013.0950 | 200.4102 / 202.5634 | 2166.78 / 2201.68 | 6.62 / 25.84 |

GoLand 空磁盘索引单次启动的首次正确工作区符号耗时在 LocalChat 为 42387.2996 ms，主进程 / 进程树峰值为 3452.72 / 4048.44 MiB。合成工作区为 28676.1511 ms 和 2961.53 / 3230.20 MiB。rs-gopls 运行已索引样本的完整会话在两套工作区分别采得 11.49 / 35.56 和 7.23 / 25.11 MiB，进程树均包含 `go.exe`。同期已运行的 GoLand 参考进程两次均采得 162.01 / 162.01 MiB。

### 有意差异

- **大纲**。rs-gopls 只列当前文件内的声明，接收者方法按源码顺序置于顶层，过滤 `_`。GoLand 可挂载跨文件接收者父节点、按种类排序，并显示空白标识符声明。裸嵌入项的 GoLand PSI 声明范围可与标识符范围同宽，rs-gopls 按严格包含约定纳入相邻边界。
- **搜索**。rs-gopls 支持一般子序列和 `pkg.Type.Method`，过滤显式 import 别名，按文件与标识符范围去重。GoLand 的 ChooseByName 在 LocalChat 和合成工作区分别产生 73 行、47 行重复交付，`clock` 导入别名有一个唯一落点。具体查询和双方实际结果已归档。[有意差异与落点](../goland-symbols-baseline.md#有意差异)

### 交付收尾与后续

LocalChat 采集和校验前后记录的 5837 个文件路径、大小和修改时间一致，`server` 路径的 Git 状态为空。本计划 11 项验收均已通过，于 2026-11-26 完成。

实现跳转的 Zed 界面现场验收继续在 Phase 4 跟踪。[第四阶段计划](2026-11-25-go-implementation.md#完成记录)
