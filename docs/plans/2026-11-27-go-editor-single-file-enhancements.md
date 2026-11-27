# Go 编辑器单文件增强（Track E）

| 项目 | 内容 |
| --- | --- |
| 状态 | 已完成 |
| 创建日期 | 2026-11-27 |
| 用户确认 | 已确认（2026-11-27） |
| 更新日期 | 2026-11-27 |
| 完成日期 | 2026-11-27 |

## 目标与范围

在 LSP 3.17 下为 Go 编辑器一次性交付 `textDocument/semanticTokens/full`、`textDocument/documentHighlight`、`textDocument/inlayHint` 和 `textDocument/foldingRange`。编辑器可依据当前文档着色语义符号、高亮光标对应的符号与控制流、显示行内名称和值，以及折叠结构区域。运行态基线采用 GoLand `2026.3.0.1`（`GO-263.7307.354`）。[Track E 协作规则](../COLLABORATION.md#track-e--编辑器单文件增强)

### 单文件查询边界

- 四项请求沿用文档符号的同步处理模式。每项请求使用当前的 `DocumentSnapshot`，优先读取打开文档的未保存版本，关闭后恢复读取当前磁盘文件。查询以当前语法树、UTF-16 `LineIndex`、`scope` 局部绑定、当前文件 `FileSymbols` 声明及成员、内置 universe 和有界单文件常量求值为依据。
- 四项查询均为零跨包长查询，独立于 `go list` 的启动、等待和包图更新。请求在包图尚未就绪的 `PausedGoList` 下同步返回，与同一文档在包图就绪后的结果一致。成员身份、调用签名和常量值只有在当前文件足以证明时才使用。省略依赖同包其他文件声明、外部包 selector 成员或 dot import 引入名称的结果，保留点导入声明本身可识别的 `.`。无别名 import 只以路径末段和版本后缀作保守近似，实际包名校验依赖跨包信息。局部和文件声明优先于导入近似，无法唯一绑定时省略，避免把同拼写的符号误合并。

### 源码与接线

| 路径 | 职责 |
| --- | --- |
| `src/semantic/editor/mod.rs` | 对外查询入口、类型及单文件身份、位置转换等公共辅助。 |
| `src/semantic/editor/semantic_tokens.rs` | 符号分类、修饰符和相对编码。 |
| `src/semantic/editor/document_highlight.rs` | 光标定位、符号身份、读写分类及控制流高亮。 |
| `src/semantic/editor/inlay_hint.rs` | 实参、无键结构体字段、命名返回值和常量值提示。 |
| `src/semantic/editor/folding_range.rs` | 结构区域收集、region 配对及列级与行级范围投影。 |
| `src/semantic/editor/tests.rs` | 本线 Rust 单元与集成测试，复用共享测试支撑。 |

公共模块由维护人统一薄接线。`src/semantic.rs` 声明 `mod editor;`，在 `Engine` 暴露四个同步方法，接收文档路径和各自的请求参数，折叠查询另接收当前客户端的折叠配置。`src/dispatch.rs` 的 `FEATURES` 注册四项服务器能力与同步处理器，并沿用 `document_symbols` 的请求和回复路径。由 `src/main.rs` 在 `initialize` 读取 `textDocument.foldingRange.lineFoldingOnly`，通过 `src/dispatch.rs` 请求上下文传递。可选的 `semanticTokens` 类型与格式、`foldingRange` 的 `rangeLimit` 和 `collapsedText` 协商细节以客户端验证为依据，固定服务器 legend 顺序。同步更新 `src/scheduler.rs` 的初始化 capabilities 断言。

若单文件 callable、参数名或常量和字面量辅助可直接复用，仅在 `signature_help.rs` 或 `hover/values.rs` 作必要的 `pub(crate)` / `pub(super)` 只读可见性调整，保持已有签名提示与 Hover 行为。

## 验收标准

### 语义令牌与文档高亮

- [x] 对包、导入、声明与引用夹具解码完整语义令牌。核对固定 legend、UTF-16 相对编码、严格升序、无重叠及无重复。直接声明的结构体和接口、泛型类型参数、方法及字段有对应类别。`type A = S` 的 `A` 及其引用为 `type`，`S` 可依其自身声明着色。仅在预声明名未被遮蔽时设置 `defaultLibrary`，`iota` 限于常量 spec。同块 `:=` 重用和确认写入产生 `modification`。
- [x] 光标落在注释内返回空，标识符或关键字紧邻右侧及 EOF 执行一字符回退。普通符号按准确绑定匹配，覆盖短声明同块重用、遮蔽、字段、方法、导入别名和标签。无法解析的同拼写引用保持独立，跨文件或外部包成员无猜测 token 或高亮。
- [x] 用嵌套函数、循环、两类 switch 和 select 对照控制流范围。`func`、直接 `return` 和未遮蔽的裸 `panic` 对应最近函数，包含 `defer` / `go` 的 panic 调用。带标号的 `break L` 与裸 `break` 各自成组。`continue` 对齐合法目标。返回槽位覆盖命名与匿名签名、单表达式多返回转发、裸返回省略和命名返回值叠加符号高亮。
- [x] 对照 `READ` / `WRITE` / `TEXT` 的范围及类别。特别核对 `ch <- v` 的 `ch` 为写、`v` 为读，`<-ch` 的 `ch` 为读，并覆盖解引用左值、selector 末段字段、索引基底和键、接收、range、复合字面量键及参数类声明。

### 内联提示与折叠

- [x] 检查提示锚点、标签、kind、padding 与请求范围。调用和显式命名返回的 `1 + 2` 显示名称，`x + 2` 抑制，深层表达式按直接子节点规则处理。覆盖组参数、无名、`_`、变参首槽、无名变参、函数值、方法及预声明函数遮蔽，排除 `make` / `new`。无键结构体按直接字段顺序，用嵌入字段占位，覆盖过量、带键、显式复合值同名抑制以及普通标识符作为值。
- [x] 常量组覆盖 `iota`、相同 RHS 数量的省略继承、同文件引用和循环、字符串 `len` 及运算；核对等值显式初值抑制、等值异写仍提示、无效常量和受禁控制字符抑制。重复请求返回同序且无重复的范围内提示。
- [x] 逐类核对九种折叠区域及允许的嵌套。多行单 spec 的 `var` / `const` / `type` 分组省略，空行打断连续 `//` 组。region 和注释组的范围相同或交叉时优先保留 region，嵌套 region 按 LIFO 配对。列级结果使用半开 UTF-16 列，纯行级结果省略列、保留独占一行的闭括号和闭圆括号，并去除退化范围。

### 协议、快照与回归

- [x] `initialize` 公布四项 provider。语义令牌只声明全量能力。用至少两种客户端折叠配置实际发请求，核对有列和纯行结果及缺省协商行为。
- [x] 在 CRLF、非 BMP 字符、残缺语法、未保存编辑和 `didClose` 场景核对四类响应。完整可识别的部分继续返回有效 UTF-16 坐标，残缺部分按可证明的范围降级。编辑后响应反映新快照，关闭后与当前磁盘文本相符。
- [x] `PausedGoList` 拦住包图加载时发出四项请求，四个 ID 都在放行前收到一次完整响应。放行并确认包图就绪后重发，对同一文档逐项比对一致结果。
- [x] 从运行中的 GoLand 采集四类共同样本并可重放。核对完整范围、类别、提示文本和锚点，将单文件及协议差异按样本记录。
- [x] 包级检查和测试零告警，编辑器专项与全量工作区检查通过。既有声明跳转、类型定义、引用、实现跳转、Hover、签名提示、文档和工作区符号，以及受本次接线影响的调度和工作区更新用例通过。

## 实现依据

### REA 产物证据

GoLand 安装包位于 `E:\Apps\goland-2026.3.0.1.win`。`build.txt` 为 `GO-263.7307.354`，`product-info.json` 的版本、构建号与产品代码为 `2026.3.0.1`、`263.7307.354`、`GO`。以下 JAR 路径相对于安装目录。REA `inspect-artifact` 核对归档身份与条目。注册和处理规则来自 XML、Vineflower `1.11.2` 反编译和必要的 JVM 字节码复核。仓库外临时材料 `%TEMP%\rs-gopls-track-e-263.7307.354\GoLand-Track-E.md` 和 `%TEMP%\gopls-track-e-go263-rea-evidence.json` 分别提供内联提示与折叠报告，以及语义令牌与文档高亮相关 Go 实现 JAR 的清单。实际编辑器结果及设置记录见基线。[GoLand 运行态采集结果](../goland-editor-baseline.md#采集结果)

| 安装包文件 | 可核查内容 | REA Evidence ID |
| --- | --- | --- |
| `plugins/go-plugin/lib/modules/intellij.go.impl.jar` | SHA-256 `2a13f1a753d5e15db70c5758e17db1dc28cc790e1d329e574d8b6a078d636784`。四项 Go 处理器的主体、`intellij.go.impl.xml` 注册及关联类。 | `ev_6c7c39a219d2e02923e7b819150326c434f3135777e22df0b1f7e49c6cdda5ee`，清单 `ev_2f8e91fbd4efee5fa874b5ab15a691a6585b6fdd26b74d27e10960910302e4ed` |
| `plugins/go-plugin/lib/go-plugin.jar` | SHA-256 `5cfb1c4fbd07e9a0375470b021d69157ae1bc13e68dc9daa1ee62b42127aafd9`。聚合 `META-INF/plugin.xml` 中的 Go 提示、折叠及 URL 桥接注册。 | `ev_68ccc094c4ff1a2b333440868b1980c2641b9fdf3b91c5acc27ccdb85e7d4223` |
| `plugins/go-plugin/lib/intellij.go.frontback.jar` | SHA-256 `15eeca839b9f938a954d70b7e98c8396bf0a21c051f1b13fd33e2a183ef8ce6e`。Go parser、PSI 及前后端共享的分类依据。 | `ev_85643f48bd423aaf82faacb0fea6936d24f4e8b267f8c28c1a4b2f32947a05e5` |
| `plugins/go-plugin/lib/modules/intellij.go.microservices.jar` | SHA-256 `7ccdba194672c074168d3d54ffd80c638db5ba3e3c76ec48e30ba872fd8d8fb0`。`intellij.go.microservices.xml:28` 注册 Go URL 提示桥接。 | `ev_d060eebd40b3f79def03ec509a9b44efc52e3fedc6d585fb84d8b40b77b549fb` |
| `lib/intellij.platform.lang.jar`、`lib/intellij.platform.lang.impl.jar`、`lib/intellij.platform.ide.impl.jar` | 内联提示收集、默认配置与 URL 动作过滤，Visual Studio 和 NetBeans region provider 注册。 | 见临时 REA 报告第 1、2、3 节 |
| `lib/intellij.platform.core.jar`、`lib/intellij.platform.foldings.jar`、`lib/intellij.platform.editor.ex.jar`、`lib/intellij.goland.jar` | 平台 region、折叠更新、编辑器模型，以及 `META-INF/GoLandPlugin.xml` 聚合注册。 | 见临时 REA 报告第 1、3 节 |

### 语义令牌

**GoLand 入口与规则**。`intellij.go.impl.xml:312–314, 325–326, 335` 注册语义 annotator 及相关的注解范围、词法高亮、rainbow visitor 和配色页。`com.goide.highlighting.GoHighlightingAnnotator.annotate` / `highlightRefIfNeeded` 按定义和引用目标选择 `GoSyntaxHighlightingColors`。使用 `GoPsiImplUtil.builtin`、`GoExpressionUtil.checkIfShadowsOtherVar` 与 `GoInspectionUtil.getPrimaryDefinition` 核对预声明与绑定。类型着色直接检查 `GoSpecType.getType()`。

**Rust 实现方案与取舍**。服务器注册固定 LSP legend，`tokenTypes = [namespace, type, struct, interface, typeParameter, parameter, variable, property, function, method, label]`，其中 `label` 为自定义扩展，前十项为标准类型。`tokenModifiers = [declaration, readonly, defaultLibrary, modification]`，`full: true`，`range: None`。只发语义符号，词法与转义颜色由客户端 Tree-sitter / TextMate 提供。[LSP 3.17 Semantic Tokens](https://github.com/microsoft/language-server-protocol/blob/gh-pages/_specifications/lsp/3.17/language/semanticTokens.md)

| 源码位置 | 令牌规则 |
| --- | --- |
| 包与导入 | `package` 名、显式 import 别名、dot import 的 `.` 及解析到对应导入声明的引用为 `namespace`。隐式导入只按最后路径段匹配，遇独立 `vN` 版本段时以去掉该段后的末段作候选，服从局部和文件声明遮蔽。 |
| 类型 | 直接声明的 `struct` / `interface` 类型名及引用分别为 `struct` / `interface`。普通类型与类型别名为 `type`，`type A = S` 的 `A` 不沿 RHS 追底层定义。泛型类型参数为 `typeParameter`。 |
| 函数、方法、字段 | 顶层函数为 `function`。方法声明、接口方法与当前文件可确定接收者的方法调用为 `method`。结构体字段声明、可确定的 selector 字段和复合字面量键为 `property`。 |
| 变量、常量、参数、接收者 | 参数、命名返回值和接收者为 `parameter`。常量为 `variable + readonly`，普通包级及局部变量为 `variable`。新声明加 `declaration`，同块 `:=` 重用及确认写入位置加 `modification`。 |
| 未遮蔽的内置预声明名 | 类型为 `type + defaultLibrary`，函数为 `function + defaultLibrary`。`true`、`false`、`iota` 为 `variable + readonly + defaultLibrary`，`iota` 仅在 `const_spec` 内生效。`nil` 为 `variable + defaultLibrary`。 |
| 标签与未解析引用 | 标签定义和引用为 `label`。依赖其他文件或外部包解析的引用省略语义令牌，保留客户端词法高亮。 |

按 `(line, start)` 严格升序编码，消除重叠和重复。以 UTF-16 计算相对 `delta_line`、`delta_start` 和 `length`。若输入范围跨行则按行拆分，避免把换行编码进令牌。语义标识符自身均为单行。

### 文档高亮

**GoLand 入口与规则**。`intellij.go.impl.xml:448–452` 注册用法、高亮及读写判定，`716–718` 注册包子句、导入和排除注释的引用搜索。`GoHighlightExitPointsHandlerFactory` 的 `GoFunctionExitPointHandler`、`GoBreakAndContinueStatementsExitPointHandler` 与 `GoReturnParametersHandler` 分别确定退出点、跳转目标和返回槽位。`GoCommentHighlightUsagesHandlerFactory` 抑制注释内查询。`GoReadWriteAccessDetector` 和 `GoReferenceExpressionImplBase.getReadWriteAccess` 判定读写，`GoLabelReference.processResolveVariants` 确定标签作用域。

**Rust 实现方案与取舍**。光标在注释内直接返回空。若位置落在标识符、关键字字符之外或 EOF，仅检查紧邻左侧一个字符。控制流分组使用 `DocumentHighlightKind::TEXT`。[LSP 3.17 Document Highlight](https://github.com/microsoft/language-server-protocol/blob/gh-pages/_specifications/lsp/3.17/language/documentHighlight.md)

1. **函数退出点**。在 `func`、`return` 或未遮蔽裸 `panic` 的 callee 处，返回最近函数、方法或闭包的 `func` 关键字、直接所属的完整 `return` 语句与未遮蔽的 `panic(...)` 整调用，包括 `defer panic(...)` 与 `go panic(...)`。跳过嵌套闭包和 `return` 表达式子树。
2. **循环与分支跳出**。在 `for`、表达式或类型 `switch`、`select`、`break` 或 `continue` 处，返回同一跳转目标的关键字或标签名（标签不含冒号），以及各完整跳转语句。标签目标与裸目标分组独立，`break L` 与同循环裸 `break` 各自匹配。收集和解析均以函数边界为限。
3. **返回槽位**。有函数体时，顶层返回表达式、签名中的命名结果标识符或匿名返回类型可触发槽位匹配。定位光标时不穿过嵌套块、参数列表或字面量找外层槽位。高亮签名相应零基槽位及所属函数各直接 `return` 的对应表达式。单表达式转发多个返回值时保留该表达式，跳过裸 `return`。命名返回值还叠加普通符号高亮。
4. **普通符号**。局部绑定复用 `scope::reference_binding`，沿用遮蔽与同块 `:=` 重用身份。顶层声明、可证明的当前文件字段和方法、导入别名与标签按准确身份匹配。未解析的同拼写标识符分别处理。

| 类别 | 覆盖范围 |
| --- | --- |
| `WRITE` | `var` / `const` 声明，`=` / `+=` 等赋值左侧，`++` / `--`，`:=` 新声明与重用，`for k, v = range xs` 的目标，发送语句 `ch <- v` 中的 `ch`，接收赋值目标。穿透括号与一元 `*` / `&` / `<-` 查找赋值左侧，如 `(x) = y`、`*p = y`。结构体字面量键 `T{F: v}` 以及赋值 selector 的末段字段 `obj.F` 为写。GoLand `ReadWrite` 投影为 `WRITE`。 |
| `READ` | 参数、接收者、命名返回值和字段声明，普通取值，`f(&x)` 中的 `x`，`a[i] = y` 中的 `a` / `i`，map 键 `k`，`obj.F = y` 中的 `obj`，`ch <- v` 中的 `v`，`<-ch` 中的 `ch`。 |
| `TEXT` | 类型、函数、方法、包、标签等非值符号，以及控制流分组。 |

### 内联提示

**GoLand 入口与规则**。`intellij.go.impl.xml:327–328` 注册 `GoInlayParameterHintsProvider`（类文件 SHA-256 `7ce71f69af06f1f641e3ccfd6bb9e725f0feb8d3ae0d1906da985e3097e11df9`）和 `GoInlayConstantDefinitionProvider`。前者委托 `GoParameterInfoHandler` 查找 callable 签名，使用 `GoAddKeysToStructLiteralValueQuickFix` 匹配无键字段。后者经 `GoExpressionEvaluator` 求值。已核查的 Go 文件 provider 中没有 Go 局部变量、range 绑定或方法链类型提示入口。URL 提示由 microservices 桥接至平台框架的 IDE 动作，本阶段覆盖四类普通内联提示。

**Rust 实现方案与取舍**。只返回锚点位于 `params.range` 的提示，稳定排序去重。[LSP 3.17 Inlay Hint](https://github.com/microsoft/language-server-protocol/blob/gh-pages/_specifications/lsp/3.17/language/inlayHint.md)

1. **调用实参名**。返回 `InlayHintKind::PARAMETER`，标签为 `name:`、`name...:` 或 `...:`，`padding_right: true`，锚点为实参起点。解析当前文件函数、当前文件类型的方法、函数字面量、局部函数值及未遮蔽内置函数，排除 `make` / `new`。展平成组参数后按 `min(params, args)` 配对，跳过无名非变参和 `_`，只在第一个落入变参槽的实参显示变参名。`showInlay` 对实参节点及直接子节点都应用 GoLand 的允许条件，接受字面量、二元、一元、括号、叶节点以及解析为预声明 `nil` / `true` / `false` 的引用。被检查层级的普通标识符、selector 和调用抑制提示，例如 `1 + 2` 显示，`x + 2` 省略。
2. **无键结构体字段名**。返回 `InlayHintKind::PARAMETER`，标签 `Field:`，`padding_right: true`，锚点为元素起点。按当前文件结构体直接字段的声明顺序配对，展开同组字段，嵌入字段保留槽位。任一元素有键或元素数超过字段数时整组跳过。元素为显式复合字面量，且其类型末尾名称与字段名忽略大小写相同时，跳过该元素。字段值可为普通标识符、selector 或调用，省略实参的 `showInlay` 筛选。
3. **命名返回值**。返回 `InlayHintKind::PARAMETER`，标签 `name:`，`padding_right: true`，锚点为返回表达式起点。最近签名所有者有命名返回值且 `return` 带显式表达式时，按位置配对，沿用 `showInlay`，跳过空名和 `_`。裸返回省略提示，`return other()` 按一个源码表达式处理。
4. **常量求值结果**。省略 `kind`，标签 `= <value>`，`padding_left: true`，锚点为常量名末尾。用有界求值和访问中的绑定集合防循环。支持 `iota`、从前面最近有 RHS 的同组子句继承相同数量的表达式（`iota` 取当前子句序号）、整型、浮点、布尔和字符串字面量、算术、位运算、移位、比较、逻辑、同文件常量引用及字符串字面量的 `len`。带显式 `=` 时分别去引号，原文与求值结果相同即抑制。拒绝含 BEL、BS、FF、LF、CR、VT 的字符串结果。

### 折叠范围

**GoLand 入口与规则**。`intellij.go.impl.xml:460` 注册 `GoFoldingBuilder`（类文件 SHA-256 `b4ba686f51495a798c2ae6bff9eb88ffba012162be7835fbe63c9d443c43fc2c`）。`buildLanguageFoldRegions`、`fold`、`foldTypes`、`addCommentFolds` 与 `foldingDescriptorsForCaseClause` 确定区域边界。初始状态和专用压缩来自 `GoFoldingSettings` 与平台编辑器配置。平台 `CustomFoldingBuilder` 通过 `VisualStudioCustomFoldingProvider` 和 `NetBeansCustomFoldingProvider` 识别区域标记。

**Rust 实现方案与取舍**。从当前语法树与 token 收集下表区域，并校验可证明的半开范围。排序去重，保留合法嵌套，排除局部交叉。未列出 kind 的结构区域省略该字段。[LSP 3.17 Folding Range](https://github.com/microsoft/language-server-protocol/blob/gh-pages/_specifications/lsp/3.17/language/foldingRange.md)

| 区域 | 范围和条件 |
| --- | --- |
| 文件导入列表 | `FoldingRangeKind::Imports`。多于一条 import 声明或首条跨行时，从首个 `import` 关键字末尾到导入列表末尾。紧随关键字为空白时首界再前进一个字符。对照 GoLand 对原始范围长度大于 3 的检查。 |
| 通用块 | 函数、方法、闭包体，`if` / `else` / `for` 和独立词法块的跨行 `{ ... }`。 |
| switch、select 和分支 | 表达式 switch、类型 switch、select 整体的花括号范围。非空 `case` / `default` 分支体从冒号后的范围起点到分支末尾。 |
| 分组声明 | `var` / `const` / `type` 的 `(...)`，至少两个 spec。单 spec 即使跨行也省略组折叠。 |
| 类型体 | `struct` / `interface` 的花括号范围，包含可识别的嵌套匿名类型。 |
| 复合字面量 | 元素非空且跨行的 `{ ... }`。 |
| 注释 | `FoldingRangeKind::Comment`。完整块注释及同一父节点下连续至少两条的 `//` 注释组，空行或代码打断分组。 |
| raw string | 跨行反引号字符串的完整 token 范围。 |
| 自定义区域 | `FoldingRangeKind::Region`。`//region` / `//endregion`（含空格写法）和 `<editor-fold` / `</editor-fold` 标记按栈 LIFO 配对，从开始注释到结束注释末尾。同范围或局部交叉命中普通注释组时保留 `Region`。 |

列级客户端收到半开的 UTF-16 `start_character` / `end_character`。在 `lineFoldingOnly: true` 时省略列，结束偏移若为下一行第 0 列则以实际覆盖末行为准，独占一行的 `}` / `)` 留在可见行，过滤投影后退化的同行范围。同行结构（如单行块注释）按客户端能力和实测呈现决定是否保留。GoLand 的单语句两片式压缩首轮投影为连续结构折叠，标准 LSP 未提供折叠组联动和 `collapsedByDefault` 初始状态字段，显示与初始状态由客户端负责。

### 待运行态验证的问题

- 核对 `type A = S`、导入遮蔽、短声明重用和预声明名在 annotator 与编辑器颜色中的实际归类。记录 GoLand 能解析而单文件约束省略的样本。
- 核对光标贴边、引号周围及 EOF、嵌套函数内外的 `panic`、标签与裸跳转分组、单调用返回槽位及命名返回值叠加的最终标记。对照访问探测器与实际编辑器高亮范围。
- 核对 `showInlay` 的直接子节点、无名变参、方法值与方法表达式签名、无键字面量中嵌入槽位、`iota` 继承与数值、字符串格式。记录 SDK 提供的内置形参名及 IDE 的提示开关和排除项。
- 核对折叠的快速与完整 pass、注释和 region 重合、CRLF、独占关闭符、单行范围以及编辑态缺少闭合符号的表现。若编辑器服务需索引、打开文件、光标焦点或特定动作，在样本中记录触发条件。

## 实施步骤

1. 收到用户明确确认后，记录确认结果并将计划设为「进行中」。与公共模块维护人核对同步查询入口、快照接口、请求上下文与初始化断言。
2. 建立 `tools/fixtures/editor/` 的 Go 源码、请求与编辑序列，新增 `tools/capture_goland_editor.groovy` 采集及 `tools/check_goland_editor.py` 重放校验。将 GoLand 的有效运行结果、映射规则和待验证项记入 `docs/goland-editor-baseline.md`。
3. 实现 `editor/mod.rs` 的单文件身份和语义辅助，交付语义令牌与文档高亮。单测核对绑定、GoLand 读写分类、控制流范围与 UTF-16 相对编码。
4. 实现内联提示的单文件 callable 与常量求值，随后实现折叠的结构收集、region 配对及列和行投影。将涉及签名提示和 Hover 的复用限定在只读辅助层。
5. 完成 `Engine`、`FEATURES`、初始化能力配置和调度器断言的薄接线。以 stdio 会话验证能力、未保存文档、关闭恢复、残缺输入及在 `PausedGoList` 放行前返回。
6. 完善 `src/semantic/editor/tests.rs` 和 `tools/workspace_checks/editor.py`，运行 GoLand 对照、Rust 和 Python 回归。更新本计划的验收勾选、实际结果及待处理项。

## 验证计划

### 样本与 GoLand 运行基线

- 固定同一主机、GoLand `GO-263.7307.354`、rs-gopls 提交与二进制 SHA-256、Go SDK 路径与版本、夹具修订、编辑器设置及索引状态。用 `tools/fixtures/editor/` 中同一组源码、UTF-16 光标或查询范围及未保存编辑序列采集两端。样本包括遮蔽、多返回与嵌套函数、内联提示边界、各折叠结构、CRLF、非 BMP 字符和残缺语法。
- 通过运行中的 GoLand `ideScript` 启动 Groovy 采集器，打开夹具项目及指定文件，等索引就绪后，以编辑器服务或扩展入口触发对应处理。采集语义高亮的范围与属性、用法及控制流高亮的完整范围和读写类、提示锚点与文本，以及每个折叠区域的首尾和来源。常量收集需等 IDE 退出 dumb mode，折叠采集使用匹配当前文档的 editor，并运行含注释和 raw string 的完整 pass。记录高亮收集阶段、提示开关、折叠 pass、有无编辑器焦点和具体动作。原始结果写入临时目录，规范化对照样本写入 `tools/fixtures/editor/`。比较完整的 UTF-16 结果集合、kind 和提示标签，基线文档记录列与行投影及有意差异。[已有 ideScript 重放入口](../goland-symbols-baseline.md#重放)
- 若未有可用 GoLand 会话、项目或索引未就绪、或者服务依赖 UI，先保留能采到的样本，在基线文档和本计划记录未采项目、失败阶段、证据及恢复条件。可复现的 UI 条件按记录重新打开文件、聚焦编辑器并触发对应动作再采集。静态预期与 rs-gopls 合成验证可继续，GoLand 运行态验收保持待验证。必验项持续受阻时，本计划在实现阶段标记「受阻」，待所需 IDE、索引或 UI 条件满足后重放。

```powershell
$editorGoland = 'E:\Apps\goland-2026.3.0.1.win\bin\goland64.exe'
$editorCapture = (Resolve-Path '.\tools\capture_goland_editor.groovy').Path
Start-Process -FilePath $editorGoland -ArgumentList @('ideScript', ('"{0}"' -f $editorCapture)) -WindowStyle Hidden
```

### 构建与回归命令

先执行包级检查，再运行包级测试，重点核对 `semantic::editor::tests`、`scheduler` 和 `symbol_request_tests`。沿用项目 toolchain、profile、features、target 与环境配置的 Cargo 产物目录。禁止传入 `--target-dir`、覆盖 `CARGO_TARGET_DIR` / `CARGO_BUILD_BUILD_DIR` 或另建目录做重试。

```powershell
cargo check -p rs-gopls --locked
cargo test -p rs-gopls --locked
cargo build -p rs-gopls --release --locked

$editorTarget = (cargo metadata --format-version 1 --no-deps --locked | ConvertFrom-Json).target_directory
$editorServer = Join-Path $editorTarget 'release\rs-gopls.exe'
python tools/check_workspace_updates.py $editorServer --module editor
python tools/check_workspace_updates.py $editorServer
```

- 工作区回归沿用 `tools/lsp_harness.py`，由 `tools/workspace_checks/editor.py` 注册专用用例。`PausedGoList` 测试在包图放行前收齐四个请求的完整响应，再与就绪后结果比较，计时仅用于记录响应情况。编辑与 `didClose` 用例分别核对缓冲区、磁盘和现有调度行为。
- `tools/check_goland_editor.py` 读取采集后保存的夹具和查询序列，用待测 LSP 服务重放四项请求，逐样本报告缺项、额外结果及字段差异，并对未约定的差异返回失败。
- 包级检查及测试须零告警。工作区专项与全量通过后，在 `docs/goland-editor-baseline.md` 记录实际采集和重放命令、样本数及差异。如果接线涉及共用调度或包图路径，另验证包图刷新和取消，并按协作规则复测既有导航。[导航性能对照](../PLANS.md#导航性能对照)

## 完成记录

### 2026-11-27 · 计划创建

已核对 GoLand 安装包的现有 REA 证据、Track E 模块边界及验收入口。计划待用户确认。实现、构建与测试、运行态采集及重放结果待后续填入本节。

### 2026-11-27 · 用户确认

已收到明确确认，Track E 进入实施。Wave 1A 开始准备单文件夹具与 GoLand 运行态基线。

### 2026-11-27 · 最终验收与结项

- `src/semantic/editor/` 交付单文件身份和位置辅助、语义令牌、文档高亮、内联提示和折叠，以及 17 项编辑器单测。`src/semantic.rs` 暴露四个同步入口，`src/dispatch.rs` 注册能力和请求，`src/main.rs` 传递折叠协商结果，`src/scheduler.rs` 扩充初始化断言并新增 3 项集成测试。
- `tools/fixtures/editor/` 收录 Go 模块、查询清单和规范化基线。`tools/capture_goland_editor.groovy` 负责运行态采集，`tools/check_goland_editor.py` 负责重放，`tools/workspace_checks/editor.py` 注册四项 stdio 回归。README 和愿景文档已补充四项能力和基线链接。[GoLand 单文件编辑器基线](../goland-editor-baseline.md) · [README](../../README.md#single-file-editor-features) · [项目愿景](../vision.md#当前能力与方向)

| 验证命令 | 实测结果 |
| --- | --- |
| `cargo metadata --format-version 1 --no-deps --locked` | 退出 0。从配置的产物目录定位 `E:\Code\rs-gopls\target\release\rs-gopls.exe`。 |
| `cargo check -p rs-gopls --locked` | 退出 0，零告警。 |
| `cargo test -p rs-gopls --locked` | 退出 0，139 通过，失败、忽略和告警均为 0。 |
| `cargo build -p rs-gopls --release --locked` | 退出 0，零告警。构建前后 release SHA-256 同为 `fe7b34e282c0e4d1fca3b08ebff61fbdba9d3cff2f51d83d89d85e49bb7c9d38`。 |
| `python -B tools/check_goland_editor.py $editorServer --report $editorReport` | 退出 0。Go SDK `1.27.4` 下 85/85 个基线样本通过，额外 4/4 个纯行折叠请求通过，共 89 个 LSP 请求，未约定差异为 0。报告写入 `%TEMP%`。 |
| `python -B tools/check_workspace_updates.py $editorServer --module editor` | 退出 0，4/4 通过，零失败及告警。覆盖初始化与折叠协商、未保存编辑与 `didClose`、CRLF 与非 BMP 和残缺语法、`PausedGoList` 放行前后。 |
| `python -B tools/check_workspace_updates.py $editorServer` | 退出 0，32/32 通过，零失败及告警。编辑器 4、声明与类型定义 4、包图刷新 4、Hover 与签名提示 2、文档与工作区符号 3、引用 4、实现 6、补全 5，包含未保存编辑、包图更新与取消用例。 |

GoLand `GO-263.7307.354` 的运行态记录含 85 个成功采集、0 个部分采集和 0 个受阻样本。仓库基线与两次规范化采集的 SHA-256 均为 `fcc8f4160d32dc16392ff3c3886a57300d165376b3dd0089f3c3175618fb0eef`。原始采集、运行日志与 REA 证据存放在仓库外。

| 能力 | 通过 / 请求 | 匹配条目 | 已约定差异或投影 |
| --- | ---: | ---: | --- |
| 语义令牌 | 5 / 5 | 487 | 30 条。单文件省略 10，方法、嵌入字段及类型参数细分 16，空白标识符与非符号注解省略 4。 |
| 文档高亮 | 69 / 69 | 301 | 4 条。点导入、外部包成员及两个同包跨文件引用按单文件边界省略。 |
| 内联提示 | 7 / 7 | 48 | 2 条。省略跨文件签名提示，`len("中文🙂")` 按 Go 的 UTF-8 字节数提示 `= 10`。 |
| 折叠（含列） | 4 / 4 | 83 | 11 条。合并 9 组联动折叠片段，2 个交叉或同范围注释组由 region 优先。 |
| 折叠（纯行） | 4 / 4 | 82 | 60 条。59 个范围保留独占一行的关闭符，1 个同行范围在投影时去除。 |

四项查询按当前文档能证明的身份、签名和值返回结果。无别名导入按路径末段及版本后缀近似。GoLand 的折叠组联动与初始折叠状态保存在采集元数据中，LSP 响应提供起止坐标与类型。12 项验收全部通过，无遗留阻塞。[逐样本差异与投影](../goland-editor-baseline.md#rs-gopls-重放)
