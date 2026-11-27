# Go 上下文代码补全（Track A）

| 项目 | 内容 |
| --- | --- |
| 状态 | 已完成 |
| 创建日期 | 2026-11-26 |
| 用户确认 | 已确认（2026-11-26，确认一并交付 D→A 导入编辑契约、自动导入覆盖工作区/可达依赖/按需 SDK、以及最小公共只读与路由接线） |
| 更新日期 | 2026-11-26 |
| 完成日期 | 2026-11-26 |

## 目标与范围

为 Zed 接入 `textDocument/completion` 上下文代码补全与 `completionItem/resolve` 补全项解析。以当前编辑缓冲区和活动包图为准，支持不完整 Go 源码中的补全、未导入符号和导入路径补全。冻结并实现 D→A 的 `src/semantic/actions/imports.rs` 窄接口，由 `import_edits` 生成自动导入的 `additionalTextEdits`。以运行中的 GoLand `GO-263.7307.354` 为对照。[Track A 协作规则](../COLLABORATION.md#track-a--上下文代码补全)

### 本阶段覆盖范围

1. **容错光标与替换范围**。识别 `foo.` 在 EOF、换行前、右括号前和后接下一行语句时的选择器位置，以及 `foo.Ba`、`S{`、`S{Fi: 1}`、`func f(a `、`var t pkg.`、`goto `、`import "fmt/` 和 `package ` 的补全上下文。按光标计算标识符或引号内路径的 UTF-16 插入与替换范围，在注释、普通字符串和数字后抑制代码补全。
2. **无限定表达式**。枚举局部绑定、参数、接收者、命名结果、类型参数、同文件和同包声明。根据文件所属包视图区分 `package p` 与 `package p_test`，普通文件排除 `_test.go` 的声明。已有导入提供包名或显式别名，`_` 不引入名称，`.` 将包的导出声明引入文件作用域。提供 Go 预声明名称，`iota` 仅用于 `const` spec，跳过 `init`。按词法作用域、生效位置和遮蔽规则保留可见绑定。
3. **选择器**。`pkg.` 枚举已导入包的可见导出声明。值接收者由 `Resolver::infer_type` 确定字段和方法，保留泛型实参代入、可寻址性及跨包可见性。支持一级指针自动解引用，拒绝双重指针与接口指针的隐式选择器。按嵌入层级处理提升成员，过滤最浅同深度的同名歧义。`T.` / `(*T).` 按相应的方法集提供方法表达式。
4. **类型位置**。提供局部类型、类型参数、包级类型、限定类型、预声明类型和适用的类型关键字，过滤值声明。接收者类型限于同包且可用作接收者的命名类型，排除接口和外部包。
5. **结构体字面量**。其他元素均带键时补剩余字段键，均无键时补位置值，空字面量和混合编辑态同时允许字段键与值候选。明确的值位置沿用表达式补全。过滤已使用的键、已填嵌入字段的提升后代和跨包不可见字段。插入适用的 `: `，复用源中已有的冒号和空格。
6. **关键字、标签与包名**。文件级提供 `package` / `import` / `func` / `type` / `const` / `var`，语句级提供适用的控制流关键字、`range`、`case` / `default` 和类型 switch 的 `.(type)`。`break` / `continue` / `goto` 按语句种类与函数边界补全标签。`package` 子句候选包括同目录已有包名、测试文件的 `<pkg>_test`、转成合法标识符的目录名和 `main`。
7. **导入路径**。`import "..."` 内覆盖工作区包、可达依赖、当前构建视图的 vendor 和 `GOROOT/src` 标准库。执行 `internal`、vendor 可见性和自导入规则，排除 `main`、`testdata`、以下划线或点开头的目录。允许外部测试包 `p_test` 导入对应生产包 `p`。补全路径时保留已有引号和导入别名。
8. **自动导入**。为 `Mar` 等无限定标识符和 `json.` / `json.Mar` 等未解析限定符查找工作区包、可达依赖包和按需索引的 SDK 标准库导出生产符号。复用现有正常导入与别名的候选基础优先级为 35，新导入为 5。按导入路径和实际包声明处理 `/vN`、`go-` 和非法标识符字符，别名冲突时选用下一个可用的 `pkg2` / `pkg3`。自动导入调用 `import_edits`，生成与主补全编辑互不重叠的 UTF-16 `additionalTextEdits`。
9. **排序、插入与解析**。对齐 GoLand 的基础权重，字段和标签为 45，包与导入路径为 `39 + commonPrefixSegments - slashes - dots`，普通候选和已导入候选为 35，待导入候选为 5，预期类型加权为 `+5` / `+4`。考虑同包、导入使用频次以及模块、依赖和 SDK 层次。根据 `snippetSupport`、`labelDetailsSupport`、`insertReplaceSupport`、`resolveSupport`、`deprecatedSupport` 和 Markdown 支持渲染候选。解析时复核文档版本与包图代次，过期项拒绝生成编辑。

### 待确认事项

1. **D→A 导入编辑契约**。本计划在第 1 步一并冻结并实现 `src/semantic/actions/imports.rs` 的 `Add`，保护注释及归属、`_` 副作用导入和 cgo 前导注释，预留 `Organize` 接口，确保 Track A 自动导入可端到端验收。空文件缺少可定位的 `package` 时返回安全错误，仅有包声明的文件支持插入首个导入。[跨线窄接口契约](../COLLABORATION.md#跨线窄接口契约)
2. **未导入候选范围**。默认覆盖活动包图中的工作区包、可达依赖包与按需索引的 Go SDK 标准库导出符号。模块缓存仅读取包图中可达依赖的目录，避免全量冷扫描并控制查询延迟。
3. **公共接线**。`src/scope.rs`、`src/symbols.rs`、`src/semantic/resolver.rs` 提供最小只读扩展，`src/dispatch.rs` 和 `src/main.rs` 做能力注册、参数读取与调度接线。主体收敛在 `src/semantic/completion/` 和 `src/semantic/actions/imports.rs`，公共改动由约定 Owner 串行合入。[日常防冲突与合入清单](../COLLABORATION.md#日常防冲突与合入清单)

### 变更边界与非目标

- 本阶段之外包括 ML 排序（`intellij.go.completionMlRanking`）、Full-Line 与云端 AI 补全、`type ... from json` 和 JSON 字段生成、`Implement Interface` 代码生成、结构体 tag 键值、`time` 布局字符串、`//go:build` / `// +build` 标签、注释内符号、`go.mod`、`GoTemplate` 和 cgo 指令补全。
- `GoMethodLikeFunctionCompletionProvider` 二次调用的重写与完整的导入整理不在本阶段范围内。Track D 在同一 `imports.rs` 契约上实现 `Organize`。

## 验收标准

### 导入编辑

- [x] **首个导入与单项转组**。对空文本请求 `Add` 返回 `ImportEditError`。对仅含包声明的文件在包声明后插入首个导入，有单项导入时转成分组，核对包注释和导入前导、行尾注释的归属，以及原别名、路径和引号拼写。
- [x] **导入组与副作用**。对已有块、多块和带 cgo 前导注释的 `import "C"` 添加普通导入，应用编辑后原有组、cgo 声明和注释归属正确。`_` 和 `.` 导入原样保留，同路径同别名重复添加返回空编辑。对损坏的导入块和无法安全定位的插入返回错误。
- [x] **编辑坐标与行尾**。对 LF、CRLF 及带非 BMP 字符的测试文本核对编辑范围和实际应用结果。导入编辑互不重叠，与主补全编辑互不重叠，生成文本沿用原文件换行。

### 补全上下文与候选

- [x] **容错光标**。在 `tools/fixtures/completion/` 复放 `foo.` 的 EOF、换行、右括号和下一行样本，以及 `foo.Ba`、`S{`、`S{Fi: 1}`、`func f(a `、`var t pkg.`、`goto `、`import "fmt/`、`package `。逐项核对分类、命中范围和实际插入，覆盖行内非 BMP 字符。注释、普通字符串和数字后的抑制样本返回空候选。
- [x] **无限定作用域**。用多文件、内部测试和外部测试样本核对参数、接收者、命名结果、类型参数、局部绑定和包级名称。`p` / `p_test` 与普通文件 / `_test.go` 的可见集合符合包视图。核对同名遮蔽、已有包别名、`_` 和点导入，以及 `iota` 和 `init` 的枚举结果。
- [x] **选择器与类型**。核对限定包、值接收者和类型方法表达式的候选种类与签名，覆盖泛型代入、跨包可见性、提升成员及同深度歧义、值可寻址性、一级指针、双重指针和接口指针。类型位置只出现类型及合法关键字，接收者位置过滤接口和外部包。
- [x] **字面量、关键字、标签和包名**。核对带键、无键、空及混合字面量的候选集合，已填字段与嵌入后代被过滤，已有 `: ` 时应用编辑无重复。对照文件级、语句级、`range`、`case` / `default`、`.(type)`、函数边界内的标签及同目录、测试包、目录回退和 `main` 包名。
- [x] **导入路径与自动导入**。用 `internal`、vendor、`main`、`testdata`、`_` / `.` 目录、自导入和 `p_test` 夹具核对路径可见性、引号和别名。复放 `Mar`、`json.`、`json.Mar`，比较复用现有导入和新导入的顺序、主编辑和附加编辑。覆盖 `pkg2` / `pkg3` 冲突回退、`/v2`、`go-` 与非法路径末段，应用编辑后名称解析到预期生产声明。

### 协议与工作区更新

- [x] **权重、插入与能力协商**。核对各档基础优先级、预期类型加权、同包、模块 / 依赖 / SDK 相对顺序及同分项的跨会话稳定性。分别以声明能力和缺省能力初始化，核对 `completionProvider`、触发字符、`resolveProvider`、snippet 或纯文本、`labelDetails`、过时标记、Markdown 或纯文本，以及 `TextEdit` / `InsertReplaceEdit`。应用函数、结构体字面量、字段键和限定包候选时，已有括号、花括号、冒号或点只保留一份。
- [x] **按需解析与过期保护**。按 `resolveSupport.properties` 验证详情与文档的延迟填充。仅在客户端声明可解析 `additionalTextEdits` 时延迟该字段，其余自动导入在首响应提供编辑。`resolve` 保持排序、过滤和主编辑字段稳定。文档版本或包图代次变化时返回 `ContentModified`（`-32801`），过期项无法生成附加编辑。
- [x] **动态编辑、刷新与取消**。`tools/workspace_checks/completion.py` 的用例由 `tools/check_workspace_updates.py` 发现。验证未保存编辑、逐字输入触发补全、关闭恢复、文档与包图变化后的过期解析，以及工作区包图刷新后新增包进入自动导入。等待包图或分批枚举时取消的请求按 ID 返回一次 `-32800`，已开始且快照过期的任务返回 `-32801`，后续查询能正常完成。

### GoLand 对照与回归

- [x] **可重放 GoLand 基线**。在运行中的 `GO-263.7307.354` 上用 `tools/capture_goland_completion.groovy` 采集 LocalChat 和 `tools/fixtures/completion/`，在 `docs/goland-completion-baseline.md` 记录来源、上下文、完整候选、排序、UTF-16 范围和实际插入结果。`tools/check_goland_completion.py` 重放并核对共同样本，记录有意差异和待运行态问题的实测结论。
- [x] **性能对照**。`tools/compare_completion.py` 对结果一致的样本记录索引就绪后首查、复查、编辑后查询的 P50/P95、样本数、缓存状态和计时边界。报告保留候选、排序或编辑范围有差异的样本及两侧实际结果。[导航性能对照](../PLANS.md#导航性能对照)
- [x] **构建与历史回归**。`cargo check -p rs-gopls --locked` 和 `cargo test -p rs-gopls --locked` 零告警通过，发布版通过声明、类型定义、引用、实现、Hover、SignatureHelp、文档大纲、工作区符号和工作区更新回归。

## 实现依据

### REA 产物证据

GoLand 安装于 `E:\Apps\goland-2026.3.0.1.win`。`build.txt` 为 `GO-263.7307.354`，`product-info.json` 的版本、构建号与产品代码为 `2026.3.0.1`、`263.7307.354`、`GO`。用 REA `inspect-artifact` 核对 JAR 并记录证据 ID，用 Vineflower `1.11.2` 反编译，用 JBR 25 的 ClassFile API 核对字节码。只读 PSI 探针通过 `GoBackendParserDefinition`、`GoParser` 与 `GoCompletionContributor#beforeCompletion` 核对语法解析与补全占位符行为。以下路径相对于安装目录。[既有 REA 证据](2026-11-26-go-document-and-workspace-symbols.md#rea-产物证据)

| 安装包文件 | 可核查内容 | REA Evidence ID |
| --- | --- | --- |
| `plugins/go-plugin/lib/modules/intellij.go.impl.jar` | SHA-256 `2a13f1a753d5e15db70c5758e17db1dc28cc790e1d329e574d8b6a078d636784`。`intellij.go.impl.xml` 第 374–410 行注册补全贡献者、权重、置信度与字符过滤器，第 662–699 行注册 cgo、时间布局、tag 与构建标签扩展。`META-INF/vgo.xml` 第 101–103 行注册 vgo 补全与 `vgoPackageWeigher`。 | `ev_6c7c39a219d2e02923e7b819150326c434f3135777e22df0b1f7e49c6cdda5ee` |
| `plugins/go-plugin/lib/intellij.go.frontback.jar` | 结合 `com.goide.parser.GoParser` 与 `GoBackendParserDefinition` 核对语法解析入口，并核对默认格式与导入展示文案。 | `ev_85643f48bd423aaf82faacb0fea6936d24f4e8b267f8c28c1a4b2f32947a05e5` |

### 已确认的补全入口与候选行为

- `GoCompletionContributor` 注册 `GoReferenceCompletionProvider`，`GoKeywordCompletionContributor` 和 `GoPackageNameCompletionContributor` 分别处理关键字与包名。以 IntelliJ 默认的 `IntellijIdeaRulezzz ` 占位符为基础，`beforeCompletion` 在形参、接收者、嵌入字段和结果类型等类型恢复分支使用无尾空格的 `IntellijIdeaRulezzz`，数字和字符串分支清空占位，导入字符串单独调整替换结束位置。`GoCompletionConfidence` 与 `GoCharFilter` 控制自动弹出和字符接收。`x.`、`S{`、`S{Fi: 1}`、`func f(a `、`var t pkg.` 与 `goto ` 用于对照这些入口和后续 PSI 恢复的联合作用。
- `GoReference#processUnqualifiedResolve` 和 `GoTypeReference#processUnqualifiedResolve` 分别枚举表达式与类型可见名称。`GoLabelReference#processResolveVariants` 按语句与函数边界处理标签。限定查找经 `GoReference#processQualifier`、`processExistingType` 和 `processAnonymousRefs` 处理包、接收者类型与嵌入成员。`GoReferenceCompletionScopeProcessor#accept` 使用 `GoPsiUtil#isUnexportedAndFromOtherPackage` 过滤跨包未导出项，`getPriorityDelta` / `addElement` 计算预期类型权重并生成候选。
- `GoStructLiteralCompletion#allowedVariants` 按其他元素是否带冒号返回字段名、值或两者。已在键值对的值侧时走值补全。`GoReferenceCompletionProvider#fillStructFieldNameVariants` 调用 `GoFieldNameReference#processResolveVariants`，使用 `alreadyAssignedFields`、`alreadyFilledPromotedFields` 和 `topLevelFields` 过滤字面量候选。`GoPackageNameCompletionContributor` 委托包名 provider 读取同目录包声明，并处理测试包后缀、目录名回退与 `main`。

### 已确认的导入、权重与插入行为

- `GoAutoImportCompletionContributor` 以 `order="first"` 注册，从 `GoAllPublicNamesIndex` 读取公开生产名称。`ExceptTestsScope`、`GoPathScopeHelper#couldBeReferenced` 和 `GoPsiImplUtil#canBeAutoImported` 提供测试与路径约束，`NamedElementProcessor#cachedImportData` 读取已有导入。`GoImportPathsCompletionProvider` 经 `DefaultGoImportPathsProvider` 和 `VgoImportPathsProvider` 枚举路径。
- `GoAutoImportInsertHandler#autoImport` 使用 `GoImportPackageQuickFix#isRequireAlias`，包名推测来自 `GoImportOptimizer#getAssumedPackageName`，包括版本后缀、`go-` 与标识符合法性处理。`GoImportPackageQuickFix#perform` 和 `GoReferenceImporter` 接入导入修复。AST 编辑入口为 `GoFile#addImport`、`GoPsiImplUtil#addImport` / `addImportSpec`，`GoImportsSorter#findBeforeAnchorForNewImport` 选择插入锚点。`GoImportOptimizer` 提供整理路径。
- 字段和标签基础权重为 45，包与导入路径为 `39 + commonPrefixSegments - slashes - dots`，正常和已导入项为 35，未导入项为 5。`getPriorityDelta` 对匹配预期类型加 5，对 `nil` 或非字面量转换加 4。注册顺序为 `stats` → `goSamePackageWeigher` → `goPackageUsageWeigher` → `vgoPackageWeigher` → `proximity`，Vgo 层次为当前模块 3、直接依赖或工作区 2、SDK 1、默认 0。
- `GoCompletionUtil$Lazy` 组织候选呈现与插入，`ParenthesesInsertHandler`、`FieldColonSpaceInsertHandler`、`BracesInsertHandler` 和 `GoPackageInsertHandler` 处理括号、字段冒号、花括号和包标识符。`GoDocumentationProvider#generateDoc` 提供 Go 文档渲染依据。

### Rust 实现方案与取舍

1. **`src/semantic/actions/imports.rs`**。实现 `fn import_edits(document: &DocumentSnapshot<'_>, operation: ImportOperation<'_>) -> Result<Vec<TextEdit>, ImportEditError>`。`ImportOperation::Add` 接收路径和可选别名，预留 `Organize`。通过语法树确定包声明、导入 spec、注释归属与 cgo 边界，复用行索引，按现有行尾生成最小编辑。选定可安全修改的普通导入块或新增声明。相同导入返回空编辑，已有路径的别名冲突或无法安全修改时返回错误。`completion` 结合主编辑做范围校验后随响应交付。
2. **`src/semantic/completion/{mod,cursor,candidates}.rs`**。`mod.rs` 组织快照查询与分批任务，`cursor.rs` 以 Tree-sitter 节点、相邻 token 与受限错误节点恢复光标上下文，计算主编辑字节范围再转 UTF-16。`candidates.rs` 经 `scope.rs`、`symbols.rs`、`resolver.rs` 的只读接口枚举词法和包级声明，复用已有类型、泛型代入、方法集和可见性判定。预期类型从调用参数、赋值和字面量等上下文获取。选择器按最浅层和歧义规则合并，字面量保留嵌入字段路径以生成合法的键及嵌套字面量。
3. **`src/semantic/completion/imports.rs`**。以活动快照的包图、导入表及 SDK 根为输入，复用包视图和路径身份处理 `internal`、vendor 与测试例外。按包懒读 SDK 导出符号，索引按 SDK 路径、版本和生效构建配置复用，工作区及依赖条目随包图和源文件变化失效。复用已有别名，结合当前位置的词法可见名称与文件的导入名分配可用别名。未解析限定符需要更换别名时，主编辑同时覆盖限定符与已输入成员。
4. **`src/semantic/completion/{ranking,render}.rs`**。`ranking.rs` 将基础权重、预期类型和包级权重投影到稳定的 `sortText`。使用声明和导入使用数据、同包与路径距离模拟可从快照计算的权重，同分按名称、包路径和声明位置决胜。IDE 的使用统计作为基线运行条件记录。`render.rs` 负责能力协商下的编辑、标签、签名和 GoDoc，复用已有分隔符，按客户端支持提供占位符。`CompletionItem.data` 携带文档身份、版本、包图代次和候选标识，解析时从当前快照校验后填充可延迟字段。
5. **薄接线与测试**。模块声明及 `src/dispatch.rs`、`src/main.rs` 接入沿用现有能力表、初始化、UTF-16 编码和任务调度。只需本地语法的请求可直接返回，依赖包图的查询在图就绪后取快照，可取消的枚举分批推进，已开始的过期任务通过 `ContentModified` 结束。`src/semantic/completion/tests.rs` 复用 `TestProject`，动态回归复用 `tools/lsp_harness.py` 与工作区用例发现入口。[共享测试与调度](../COLLABORATION.md#并行前的两个解耦-pr)

### 待运行态验证的问题

- 同名的局部绑定、包级声明、导入别名和预声明名在有效及错误代码中的覆盖与实际首选顺序。
- 最浅同深度提升成员在 GoLand 列表中的候选与 Go 规范的歧义规则。核对提升字段在字面量中选中后的实际写法。
- 单项导入转成分组时，紧邻及行尾注释的归属、原引号和别名、多个导入组的插入位置，以及 cgo 前导注释的保留。
- `p_test` 对生产包的路径和限定补全，`internal`、vendor 与 SDK `testdata` 的实际导入路径和自动导入候选。
- 运行中的 GoLand 在相同补全样本上索引就绪后的首查、复查和编辑后延迟，及其实际结果缓存状态。

## 实施步骤

1. **冻结并实现 D→A 导入编辑契约**。收到用户确认后记录确认结果并将计划设为「进行中」。与 Track D 对齐签名、别名、快照和错误语义，在 `src/semantic/actions/imports.rs` 实现 `Add`，预留 `Organize`，用契约单测核对空文件、首个导入、注释、多块、cgo、幂等和 UTF-16 编辑。
2. **搭建并采集 GoLand 补全基线**。新增 `tools/fixtures/completion/` 和 `tools/capture_goland_completion.groovy`，在运行中的 GoLand `GO-263.7307.354` 上对两套工作区采集完整候选、坐标、插入结果和计时。将样本、采集条件和待验证问题的实测结果归档到 `docs/goland-completion-baseline.md`。
3. **补齐只读语义枚举**。在 `scope.rs`、`symbols.rs`、`resolver.rs` 增加补全所需的可见绑定、包声明和成员枚举接口，核对跨文件作用域、测试包身份、嵌入及方法集合，跑受影响的已有解析回归。
4. **实现光标分类与本地候选**。交付 `completion/cursor.rs` 与 `candidates.rs`，实现容错上下文、替换范围和预期类型，覆盖本地表达式、选择器、类型、结构体字面量、关键字、标签和包名。用合成夹具逐项核对上下文与候选集合。
5. **实现路径补全与自动导入**。在 `completion/imports.rs` 接入工作区与可达依赖、按需加载 SDK 标准库导出索引、路径可达性与别名冲突处理。复用 `actions::imports::import_edits` 生成附加编辑，校验应用后引用和导入有效。
6. **实现排序、渲染与解析**。交付 `completion/ranking.rs` 与 `render.rs`，核对 GoLand 权重、snippet / `textEdit` 插入、客户端能力差异，以及 `completionItem/resolve` 的延迟文档和快照校验。
7. **接入 LSP 能力与调度**。在 `src/dispatch.rs` 与 `src/main.rs` 注册两个方法和触发字符，读取客户端能力，复用包图等待和任务取消。用 stdio 会话验证能力协商、逐字编辑、过期结果和每个请求 ID 的单次回复。
8. **完成对拍、基准与回归**。完善 `src/semantic/completion/tests.rs`、`tools/check_goland_completion.py`、`tools/compare_completion.py` 和 `tools/workspace_checks/completion.py`。按验证计划运行构建及历史回归，将结果、样本差异与耗时归档，逐项勾选本计划并在全部通过后填写完成日期。

## 验证计划

### 样本与运行基线

- 固定同一 Windows 主机、GoLand `GO-263.7307.354`、rs-gopls 发布版提交与二进制 SHA-256、Go SDK `go1.27.4`、项目作用域、打开文件、光标位置、触发方式及编辑序列。记录 Go SDK 路径和 `go version`，核对 `GOOS`、`GOARCH`、构建标签、vendor 模式与 `GOWORK`。
- 真实样本使用 `E:\Code\localchat\server` 的 `532228d3d36bd9f0fffb4bda35c180485b2eadd6`。合成样本使用 `tools/fixtures/completion/`，覆盖上述光标、测试视图、指针与嵌入、导入及别名、CRLF 与非 BMP 字符。记录同一 BASIC 补全调用的完整候选、种类和来源、排序、UTF-16 范围及选中后的文档。用未保存编辑重放后恢复基线文本。
- `tools/capture_goland_completion.groovy` 沿用 `ideScript` 入口，从 `$env:TEMP\rs-gopls-goland-baseline\completion-queries.json` 读取工作区、文件 URI、光标和编辑步骤。在基线文档记录生成查询文件、采集和重放方式，按 `RS_GOPLS_BENCHMARK=1` 采集原始计时。[既有 Groovy 采集方式](../goland-symbols-baseline.md)

```powershell
$goland = 'E:\Apps\goland-2026.3.0.1.win\bin\goland64.exe'
$capture = (Resolve-Path '.\tools\capture_goland_completion.groovy').Path
Start-Process -FilePath $goland -ArgumentList @('ideScript', ('"{0}"' -f $capture)) -WindowStyle Hidden
```

### 构建与回归命令

先执行包级检查，再运行受影响的测试和发布构建。沿用现有 toolchain、profile、features、target 和环境配置的 Cargo 产物目录。

```powershell
cargo check -p rs-gopls --locked
cargo test -p rs-gopls --locked
cargo build -p rs-gopls --release --locked
```

以下命令以基线已采集、测试进程已选中 `go1.27.4` 为前提。补全脚本沿用 `--workspace` / `--fixture` / `--json` 参数，工作区用例通过 `completion.py` 的 `CASES` 注册。

```powershell
$target = (cargo metadata --format-version 1 --no-deps --locked | ConvertFrom-Json).target_directory
$server = Join-Path $target 'release\rs-gopls.exe'
$localchat = 'E:\Code\localchat\server'
$completionFixture = (Resolve-Path '.\tools\fixtures\completion').Path
$symbolFixture = (Resolve-Path '.\tools\fixtures\symbol-navigation').Path

python -B .\tools\check_goland_completion.py $server --workspace $localchat --fixture (Join-Path $completionFixture 'localchat-goland-completion.json') --json (Join-Path $target 'localchat-completion-correctness.json')
python -B .\tools\check_goland_completion.py $server --workspace $completionFixture --fixture (Join-Path $completionFixture 'goland-completion.json') --json (Join-Path $target 'fixture-completion-correctness.json')
python -B .\tools\check_workspace_updates.py $server
python -B .\tools\check_definitions.py $server --stage 6 --workspace $localchat
python -B .\tools\check_goland_definitions.py $server --workspace $localchat --method definition
python -B .\tools\check_goland_definitions.py $server --workspace $localchat --method typeDefinition
python -B .\tools\check_goland_references.py $server --workspace $localchat
python -B .\tools\check_goland_implementations.py $server --workspace $localchat
python -B .\tools\check_goland_hover.py $server --workspace $localchat
python -B .\tools\check_goland_signature_help.py $server --workspace $localchat
python -B .\tools\check_goland_symbols.py $server --workspace $localchat --fixture .\tools\fixtures\localchat-goland-symbols.json
python -B .\tools\check_goland_symbols.py $server --workspace $symbolFixture --fixture (Join-Path $symbolFixture 'goland-symbols.json')
```

### 性能计量

- `tools/compare_completion.py` 对照运行中 GoLand 的 Groovy 记录与发布版 stdio 响应，每次计时核对完整候选集合、可比顺序、标识符和导入范围及编辑应用结果。P50/P95 使用结果一致的共同样本。差异样本报告两侧实际结果、范围和原因。
- 分别记录索引就绪后的首次新查询、相同查询的预热后复查，以及双方完成相同未保存编辑后的首次查询。记录每组有效样本数、P50/P95（ms）、预热次数、包图与索引状态、SDK 按需索引及结果缓存状态。GoLand 从发起补全到候选采集完毕，注明是否包含界面操作。rs-gopls 从发送请求到完整 JSON-RPC 响应，注明 stdio 开销。解析计时单独记录适用的字段和客户端能力。[导航性能对照](../PLANS.md#导航性能对照)

```powershell
$baseline = Join-Path $env:TEMP 'rs-gopls-goland-baseline'
python -B .\tools\compare_completion.py $server --workspace $localchat --fixture (Join-Path $completionFixture 'localchat-goland-completion.json') --goland-bench (Join-Path $baseline 'localchat-completion-bench.json') --json (Join-Path $target 'localchat-completion-benchmark.json')
python -B .\tools\compare_completion.py $server --workspace $completionFixture --fixture (Join-Path $completionFixture 'goland-completion.json') --goland-bench (Join-Path $baseline 'fixture-completion-bench.json') --json (Join-Path $target 'fixture-completion-benchmark.json')
```

## 完成记录

### 2026-11-26 · Wave 1A

- 实现 D→A 的 `ImportOperation::Add` 与类型化错误。`Organize` 返回 `UnsupportedOperation`，由 Track D 接续。按解码后的路径和显式别名判定幂等。用本机 Go 编译器验证同一路径的 `_` 和普通导入可以共存。涉及默认包名的跨路径冲突由补全调用方根据包图判断。
- 导入契约测试覆盖首个导入、单项转组、多块、cgo、注释、引号、损坏语法和别名冲突。LF、CRLF、非 BMP 字符和合成主补全编辑已按 UTF-16 坐标应用并核对文本。实际补全响应的主编辑与附加编辑仍需联合验收。
- 提供 `visible_bindings`、`FileSymbols::declarations`、`DocumentSnapshot::raw_imports`，以及 `Resolver::package_declarations`、`visible_members`、`struct_fields`。测试覆盖词法遮蔽和未完成函数体、测试包视图与编辑缓冲区、跨包可见性、约束方法、指针和方法表达式、提升歧义及嵌入字段的泛型类型。
- 沿用配置的 Cargo 产物目录。`cargo check -p rs-gopls --locked` 零告警通过。`cargo test -p rs-gopls --locked` 通过 77 项，零失败、零忽略、零告警。

### 2026-11-26 · Wave 3B

- 修复 `S{` 吞入下一行赋值、未闭合导入的右引号、限定自动导入的 UTF-16 范围、导入分组的末尾空行，以及 `case` / `default` 和 `.(type)` 候选的上下文边界。扩展驼峰和限定匹配，按混合大小写的词段筛选候选，并为包名和同分名称采用稳定顺序。补全复用已取得的声明源，SDK 和模块缓存先按导出名筛选。回归覆盖不可匹配的大写词段、外部依赖缓冲区的打开和关闭，以及已有括号、冒号和点号的编辑。
- 重新采集 GoLand `GO-263.7307.354`。采集器记录每次候选变化的结果和采样下标，核对编辑后的候选顺序。对照检查使用 Go 命令验证依赖包可达性，工作区检查验证跨会话顺序，计时脚本在出现未解释失败时返回非零退出码。[GoLand 对照基线](../goland-completion-baseline.md#rs-gopls-对照结果与性能基准)
- 发布版 `target/release/rs-gopls.exe` 的 SHA-256 为 `dea74df1f6071c0f5ac84b8f44c6f42f7b574e4b1dfbfa60f8f131fbf4e1c388`。Cargo 沿用环境配置的产物目录。Go 验证固定 `go1.27.4 windows/amd64`，LocalChat 修订为 `532228d3d36bd9f0fffb4bda35c180485b2eadd6`。

#### 验证命令与结果

下表沿用验证计划中的 `$server`、`$target`、`$localchat`、`$completionFixture`、`$symbolFixture` 与 `$baseline`。

| 命令 | 实际结果 |
| --- | --- |
| `cargo check -p rs-gopls --locked` | 通过，零告警。 |
| `cargo test -p rs-gopls --locked` | 109/109 通过，零失败、零忽略、零告警。 |
| `cargo build -p rs-gopls --release --locked` | 通过，零告警。 |
| `python -B .\tools\check_goland_completion.py $server --workspace $localchat --fixture (Join-Path $completionFixture 'localchat-goland-completion.json') --json (Join-Path $target 'localchat-completion-correctness.json')` | 22/22 通过。13 个完全一致，9 个有意差异，0 个失败。 |
| `python -B .\tools\check_goland_completion.py $server --workspace $completionFixture --fixture (Join-Path $completionFixture 'goland-completion.json') --json (Join-Path $target 'fixture-completion-correctness.json')` | 126/126 通过。58 个完全一致，68 个有意差异，0 个失败。 |
| `python -B .\tools\check_workspace_updates.py $server --module completion` | 5/5 通过。 |
| `python -B .\tools\check_workspace_updates.py $server` | 28/28 通过，含历史 23 项及补全 5 项。 |
| `python -B .\tools\check_definitions.py $server --stage 6 --workspace $localchat` | 合成 124/124、LocalChat 16/16。 |
| `python -B .\tools\check_goland_definitions.py $server --workspace $localchat --method definition` | 20/20 通过，其中 19 个直接匹配，1 个既有包落点差异。 |
| `python -B .\tools\check_goland_definitions.py $server --workspace $localchat --method typeDefinition` | 34/34 通过，其中 32 个直接匹配，2 个预期空结果。 |
| `python -B .\tools\check_goland_references.py $server --workspace $localchat` | 34/34 匹配。 |
| `python -B .\tools\check_goland_implementations.py $server --workspace $localchat` | 20/20 匹配。 |
| `python -B .\tools\check_goland_hover.py $server --workspace $localchat` | 21/21 匹配。 |
| `python -B .\tools\check_goland_signature_help.py $server --workspace $localchat` | 13/13 匹配，编辑、关闭恢复与取消检查通过。 |
| `python -B .\tools\check_goland_symbols.py $server --workspace $localchat --fixture .\tools\fixtures\localchat-goland-symbols.json` | 两种大纲模式各 8/8，工作区符号 20/20。 |
| `python -B .\tools\check_goland_symbols.py $server --workspace $symbolFixture --fixture (Join-Path $symbolFixture 'goland-symbols.json')` | 两种大纲模式各 5/5，工作区符号 36/36。 |
| 设置 `PYTHONPATH=tools` 后执行 `python -B -m unittest tools/test_lsp_harness.py tools/test_workspace_checks.py` | 25/25 通过。 |
| `python -B .\tools\compare_completion.py $server --workspace $localchat --fixture (Join-Path $completionFixture 'localchat-goland-completion.json') --goland-bench (Join-Path $baseline 'localchat-completion-bench.json') --json (Join-Path $target 'localchat-completion-benchmark.json')` | 完成全部 704 个采样，0 个校验失败。 |
| `python -B .\tools\compare_completion.py $server --workspace $completionFixture --fixture (Join-Path $completionFixture 'goland-completion.json') --goland-bench (Join-Path $baseline 'fixture-completion-bench.json') --json (Join-Path $target 'fixture-completion-benchmark.json')` | 完成全部 4,032 个采样，0 个校验失败。 |
| 在合成夹具执行 `go list -mod=vendor -test ./...` 与 `go test -mod=vendor ./...` | 两条命令均通过。 |

#### 索引就绪后的性能

补全 P50/P95 用结果与顺序一致的共同样本。解析列为全部通过解析检查的选中项，包括有已验证差异的查询。GoLand 的呈现已计入 BASIC 补全，本轮采集无独立解析计时。两端计时边界和缓存状态见基线。

| 工作区 | 场景 | 共同 / 总样本数 | GoLand P50 / P95（ms） | rs-gopls P50 / P95（ms） | rs-gopls resolve 数量与 P50 / P95（ms） |
| --- | --- | ---: | ---: | ---: | ---: |
| LocalChat | 首查 | 13 / 22 | 43.3941 / 446.7385 | 2.6595 / 29.5850 | 1 · 0.8627 / 0.8627 |
| LocalChat | 复查 | 390 / 660 | 29.2823 / 123.5895 | 0.3580 / 11.9409 | 30 · 0.9455 / 1.0807 |
| LocalChat | 编辑后首查 | 13 / 22 | 36.0754 / 139.9207 | 0.6280 / 13.8461 | 1 · 0.9524 / 0.9524 |
| 合成夹具 | 首查 | 58 / 126 | 78.7436 / 139.6909 | 1.4776 / 7.7571 | 26 · 0.2537 / 0.9853 |
| 合成夹具 | 复查 | 1,740 / 3,780 | 31.3772 / 95.6739 | 0.4297 / 2.9012 | 780 · 0.2207 / 1.0156 |
| 合成夹具 | 编辑后首查 | 58 / 126 | 32.4451 / 106.7794 | 0.5188 / 3.3743 | 26 · 0.2167 / 0.9007 |

- 有意差异包括 Go 的作用域、歧义与指针规则，字面量键位置约束，导入注释和路径保留，项目别名与依赖索引范围，IDE 专有模板及稳定排序。显式 `p_test` 导入下的同包测试声明按 Go 编译视图直接对齐。合成 102 次复查与 3 次编辑后首查观察到 GoLand 候选变化，报告保留这两侧的实际结果。[差异依据与明细路径](../goland-completion-baseline.md#差异依据)
- 首次 SDK 搜索仍有尾延迟。合成 `scope-local` 为 622.6629 ms，LocalChat 差异样本 `local-inferred` 最终为 1779.8217 ms。LocalChat 该样本在扩展模块缓存预筛选之前为 2109.1100 ms。
- 采集和重放前后 `git -C E:\Code\localchat\server status --short -- .` 均为空。rs-gopls 的 Track A 改动留在工作区供审查，Wave 3B 未创建提交。
