# Track B · Go 实时诊断引擎开发计划

| 项目 | 内容 |
| --- | --- |
| 状态 | 进行中 |
| 创建日期 | 2026-11-27 |
| 用户确认 | 已确认（2026-11-27 确认 F + B + D 三线联合开工，Codex 使用 full-access 模式，严格路径脱敏） |
| 更新日期 | 2026-11-27 |
| 完成日期 | 待填写 |

## 目标与范围

按 [并行协作与模块拆分](../COLLABORATION.md#track-b--实时诊断引擎) 实现 **Track B · 实时诊断引擎**，为 Zed 提供低延迟、未保存缓冲区优先、低误报的 Go 实时诊断，同时覆盖 LSP 3.17 的 **推送诊断（`textDocument/publishDiagnostics`）** 与 **拉取诊断（`textDocument/diagnostic`）**。以 GoLand 2026.3（构建号 `GO-263.7307.354`）的 `GoAnnotator` 与核心错误检查作为语义设计蓝本与验收基线。[项目愿景](../vision.md)

### 阶段划分与覆盖范围

1. **跨线窄接口契约（与 Track F、Track D 协同冻结）**
   - **F → B（结构化包错误，由 Track F 正式设计交付）**：消费 Track F 在 [Track F 计划](2026-11-27-go-hierarchy-foundation.md) 中统一设计的 `Workspace::package_errors(&self) -> &[PackageLoadError]`（含多工作区根、包 ID、`for_test` 身份、错误类别、消息、`ImportStack` 与可选位置）。
   - **B → D（结构化诊断模型，供 Track D QuickFix 直接消费）**：提供 `fn diagnostics_for(&self, document: &DocumentSnapshot<'_>) -> Option<&DiagnosticReport>`，按文档 URI、文本版本/指纹与包图代次校验快照一致性（过期返回 `None`）。每条诊断包含稳定 `DiagnosticCode`、严重度、UTF-16 范围、展示消息、可选 `DiagnosticTag`（如 `Unnecessary`）与类型化修复数据 `DiagnosticFixData`，供 [Track D 计划](2026-11-27-go-actions-formatting.md) 的 QuickFix 直接消费。
   - **与其他功能线的依赖**：F + B + D 三条线内部完全闭环，对 Track A、C、E 零外部依赖。
2. **阶段一 · 语法诊断、包加载错误与 Push/Pull 协议**
   - **语法诊断**：基于 Tree-sitter `ERROR` / `MISSING` 节点生成高精度 UTF-16 诊断，合并相邻碎裂错误节点，支持未保存增量编辑、CRLF 与非 BMP 字符，并尽量为缺括号、缺分号、缺包声明等常见语法错误提供可读消息。
   - **包加载错误**：消费 `Workspace::package_errors()`，将带 `file:line:col` 的错误映射到精确 UTF-16 位置，无可靠位置时回退至 `package` 子句或首行；当打开缓冲区已修复磁盘上的语法错时自动抑制过时的磁盘 `go list` 错误，避免重复报错。
   - **快照缓存与 LSP 3.17 能力协商**：按 `(path_key, doc_version / disk_fingerprint, graph_generation)` 缓存 `DiagnosticReport` 并生成 `result_id`。为避免 Zed 等支持 Pull 的客户端在同时收到 Push 通知时显示重复波浪线，严格按客户端 `initialize` 能力协商：
     - **Pull 优先**：服务端声明 `diagnosticProvider` 并始终响应 `textDocument/diagnostic`（支持 `Full` / `Unchanged` 与 `$/cancelRequest`）。当客户端声明 `capabilities.textDocument.diagnostic` 时走 Pull 模式，不主动推送 `publishDiagnostics`；若客户端同时声明 `capabilities.workspace.diagnostics.refreshSupport == true`，在包图安装/变更后发送 `workspace/diagnostic/refresh` 触发重拉。
     - **Push 回退**：当客户端未声明 `textDocument.diagnostic` 且声明了 `capabilities.textDocument.publishDiagnostics` 时，在 `didOpen`、`didChange`、`didClose`（发空数组清理）和 `workspace_loaded` 后主动推送 `textDocument/publishDiagnostics`（携带文档 `version` 防过期覆盖）。
     - **无诊断能力的最小客户端**（如仅测符号/跳转的既有单测 `capabilities: {}`）：不主动推送，保证 100% 向后兼容。
3. **阶段二 · 语义与类型诊断（严格低误报）**
   - **导入与包检查**：未解析导入路径、自导入、循环导入（基于 `Workspace` 导入图）、未使用导入（排除 `_` 与 `.` 导入，带 `Unnecessary` 标记与 `RemoveUnusedImport` 修复数据）、重复导入别名。
   - **名称与作用域检查**：未解析标识符、未解析包限定符号（区分跨包访问未导出符号）、未解析类型、未解析结构体字段/方法（跳过已报错或无法推断的动态/未知类型上下文，确保在真实项目中零误报）。
   - **声明与短声明检查**：同作用域/同包重复声明、结构体重复字段、接口重复方法、方法与字段重名、形参/类型参数重名、短变量声明 `:=` 左侧无新变量、给常量重新赋值。
   - **未使用局部变量**：函数体内局部 `var` / `:=` 变量声明后未使用（排除 `_`、包级变量与函数参数，对齐 Go 编译器与 `GoUnusedVariableInspection`，带 `Unnecessary` 标记与 `ReplaceUnusedVarWithBlank` 修复数据）。
   - **调用、返回与核心类型检查**：非函数调用、调用/转换参数个数不足或过多（支持变参 `...T`、多返回值透传 `f(g())`、方法表达式接收者、常见内置函数）、`return` 返回值个数不匹配（含命名返回值裸 `return`）、确定性类型不匹配（赋值/初始化/传参/返回/非 `bool` 条件/`nil` 无显式类型/接口实现满足性复用 `implementation/method_sets`）、结构体复合字面量未知字段、重复 `switch case` 常量、`_` 用作值。

### 明确不在本阶段范围

- `workspace/diagnostic` 全工作区主动拉取扫描（本阶段设置 `workspace_diagnostics: false`，聚焦打开文档与单文件拉取）。
- GoLand 80+ 项风格/代码异味类 `localInspection`（如命名风格、注释规范、`golint`/`staticcheck` 规则）与 `intellij.go.dfa.jar` 的路径敏感 DFA 空指针/不可达分析。
- 跨包常量折叠求值器（如复杂 `iota` 位运算导致的常量溢出检查）及 Go 版本特性门控提示（如 `GOEXPERIMENT` / 旧 SDK 语法降级提示）。
- QuickFix 文本编辑的生成与 `textDocument/codeAction` 响应（归属 Track D，本线只输出结构化 `DiagnosticFixData`）。

### 并行协作与写入边界

为避免与并行开发 Track A/C/D/E/F 的其他成员冲突，严格遵守 [COLLABORATION.md](../COLLABORATION.md#六条并行功能线)：

| 区域 | 归属与策略 |
| --- | --- |
| `src/semantic/diagnostics/*` | Track B 完全独占，核心实现与单元测试全放此处。 |
| `tools/workspace_checks/diagnostics.py`、`tools/fixtures/diagnostics/`、`tools/*diagnostics*` | Track B 完全独占，通过 `workspace_checks` 自动发现注册，不改 `check_workspace_updates.py`。 |
| `docs/plans/2026-11-27-go-diagnostics-engine.md`、`docs/goland-diagnostics-baseline.md` | Track B 完全独占。 |
| `src/workspace.rs`（`F → B` 契约） | 与 Track F Owner 同步；建议作为独立极小契约提交先行冻结，避免后续 rebase 冲突。 |
| `src/semantic.rs`、`src/dispatch.rs`、`src/main.rs`、`src/scheduler.rs`、`src/protocol.rs` | 仅做最薄接线（独立于核心算法的薄提交），合入前与公共模块 Owner 对齐。 |

### 待确认事项（2026-11-27 协同更新）

1. **F + B + D 联合推进**：F、B、D 内部构成 `F → B → D` 闭合依赖链，对 Track A / C / E 零外部依赖。`F → B` 由 Track F 在 Wave 0 统筹多工作区基座一次设计到位，不再需要临时过渡实现；Wave 0 同步冻结全部三条跨线契约（`F → B`、`B → D`、`D → A`），方便并行开发 Track A / C / E 的同事第一时间对齐。
2. **对照语料与脱敏规范**：采用仓内自包含夹具 + 当前环境 Go SDK 标准库（经 `go env GOROOT` 动态解析）作为默认对照与零误报样本；所有新增计划、基线文档与脚本均**不硬编码个人绝对路径或私有仓库路径**。
3. **Push / Pull 能力协商**：按客户端 `initialize` 能力自动切换（Zed 等支持 Pull 的客户端走 Pull + 可选 `workspace/diagnostic/refresh`，避免重复波浪线；仅支持 Push 的客户端自动走 `publishDiagnostics`；无诊断能力声明的最小测试保持静默兼容）。
4. **Codex 执行模式**：全部 Codex 子 agent 使用 `modeId: "full-access"`，在独立分支与按 Wave 隔离的文件边界内自动完成实现、单测与回归。

## 验收标准

### 跨线契约与边界
- [ ] **`F → B` 包错误契约**：`Workspace::package_errors(&self) -> &[PackageLoadError]` 正确解析 `go list -e` 的 `Error` 与 `DepsErrors`，保留包 ID、测试包身份、错误消息与可选位置；不破坏现有 `Workspace` 导航、测试包与草稿逻辑。
- [ ] **`B → D` 诊断模型契约**：`Engine::diagnostics_for(&self, document: &DocumentSnapshot<'_>) -> Option<&DiagnosticReport>` 在快照匹配时返回结构化诊断（稳定 `DiagnosticCode`、UTF-16 范围与类型化 `DiagnosticFixData`），文档变更或包图换代后对旧快照返回 `None`。
- [ ] **最小共享文件侵入**：核心诊断逻辑与单测全部位于 `src/semantic/diagnostics/`，工作区集成用例通过 `tools/workspace_checks/diagnostics.py` 自动发现，不修改其他功能线目录。

### 阶段一：语法、包错误与 LSP 协议
- [ ] **语法诊断与编辑态响应**：覆盖缺括号、缺分号、非法表达式、缺包名、CRLF 与非 BMP UTF-16 位置。未保存 `didChange` 立即更新诊断，修复语法后清空；不产生大面积重复叠错。
- [ ] **包加载错误与去重**：覆盖损坏的导入、构建约束冲突、`go.mod` 缺失依赖等 `go list` 错误，准确落位到目标文件或包声明；缓冲区已修复磁盘语法错时抑制过时磁盘包错误。
- [ ] **Push 与 Pull 能力协商及一致性**：初始化声明 `diagnosticProvider`；`textDocument/diagnostic` 正确返回 `Full` 与 `Unchanged`（基于 `previousResultId`）并支持 `$/cancelRequest`；客户端声明 Pull 能力时不主动推送 `publishDiagnostics`（并在支持 `refreshSupport` 时于包图更新后发 `workspace/diagnostic/refresh`），仅声明 Push 能力时在打开、编辑、关闭（空数组清理）和包图安装后推送且携带正确文档 `version`；无诊断能力声明的最小客户端保持静默兼容。

### 阶段二：语义与类型诊断
- [ ] **导入、名称、声明与未使用检查**：覆盖未解析导入/自导入/循环导入/未使用导入、未解析变量/类型/字段/方法及跨包未导出访问、重复声明与 `:=` 无新变量/常量重赋值、未使用局部变量（含 `Unnecessary` tag）。
- [ ] **调用、返回与核心类型检查**：覆盖非函数调用、实参/返回数量不匹配（含变参、多值透传、方法表达式、内置函数）、确定性类型不匹配、接口未实现检查、复合字面量未知字段、重复 `switch case` 与 `_` 作值。
- [ ] **低误报保证**：在干净的多包/泛型/测试包/接口嵌入/类型别名/闭包夹具与真实 Go 源码文件上保持 **0 误报**（无合理性存疑的假阳性错误）。

### GoLand 对照、性能与历史回归
- [ ] **GoLand 基线可重放与路径脱敏**：在 GoLand `GO-263.7307.354` 上通过 `tools/capture_goland_diagnostics.groovy` 采集 `tools/fixtures/diagnostics/` 的 `GoAnnotator` 基线，归档至 `docs/goland-diagnostics-baseline.md`，`tools/check_goland_diagnostics.py` 严格核对共同错误集合与 UTF-16 范围，单列有意差异；新增脚本与文档不含硬编码个人路径或私有仓库路径。
- [ ] **性能对照 GoLand**：按 [导航性能对照](../PLANS.md#导航性能对照) 固定主机、GoLand 构建号、Go SDK 与样本，记录索引就绪首查、重复查询与编辑后查询的 P50/P95、样本数与峰值工作集。
- [ ] **全量构建与历史回归**：`cargo check --workspace --locked`、`cargo test --workspace --locked` 零告警通过；`tools/test_lsp_harness.py`、`tools/test_workspace_checks.py` 与 `tools/check_workspace_updates.py` 全部通过（原 23 项 + 新增 diagnostics 用例）；既有声明、类型定义、引用、实现、Hover、SignatureHelp、符号检查全部无退化。

## 实现依据

### GoLand 安装包静态核查（`GO-263.7307.354`）

只读检查 GoLand `2026.3.0.1` 安装目录 `<GOLAND_HOME>` 的 `build.txt`、JAR 清单、XML 注册及字节码（使用安装包自带 `jbr/bin/java.exe` 与 `lib/intellij.libraries.asm.jar` 的 `org.jetbrains.org.objectweb.asm.util.Textifier`）：

| 安装包文件 | SHA-256 | 核查内容 |
| --- | --- | --- |
| `plugins\go-plugin\lib\go-plugin.jar` | `5CFB1C4FBD07E9A0375470B021D69157AE1BC13E68DC9DAA1EE62B42127AAFD9` | 插件主描述符 `META-INF/plugin.xml` |
| `plugins\go-plugin\lib\modules\intellij.go.impl.jar` | `2A13F1A753D5E15DB70C5758E17DB1DC28CC790E1D329E574D8B6A078D636784` | `intellij.go.impl.xml`、`META-INF/go-inspections.xml`、`GoAnnotator`、31 个 `SpecChecker`、29 个 `legacyErrorInspections`、`GoListOutputParser` |
| `plugins\go-plugin\lib\intellij.go.frontback.jar` | `15EECA839B9F938A954D70B7E98C8396BF0A21C051F1B13FD33E2A183EF8CE6E` | Go 词法/语法树与共享前后端元素 |

### 已确认的 GoLand 诊断架构与规则

| GoLand 类 / 注册 | 字节码确认的处理机制 |
| --- | --- |
| `intellij.go.impl.xml` → `com.goide.highlighting.GoAnnotator` | 注册为核心 Go `<annotator>`。`annotate` 按文件状态分派：被构建约束排除的文件仅运行 `GoBuildDirectiveSpecChecker`（`buildExcludedFileVisitors`）；正常文件运行 `buildErrorCheckers`（31 个语言规范检查器）与 `buildVisitors`（`buildCommonVisitors` 26 项 + `buildNonCodeFragmentVisitors` 3 项），并支持 `GoErrorAnnotatorSuppressor` 扩展抑制。 |
| `GoAnnotator#buildErrorCheckers` → `com.goide.highlighting.errors.Go*SpecChecker` | 包含 `BasicErrorsChecker` 与 30 个按 Go Spec 拆分的检查器，如 `GoShortVariableDeclarationSpecChecker`（`go.error.annotator.no.new.variables`、`cannot.reassign.a.constant`）、`GoCallExpressionSpecChecker`（`cannot.call.pointer.method`）、`GoAssignmentStatementSpecChecker`、`GoConstDeclarationSpecChecker`、`GoVariableDeclarationSpecChecker`、`GoStructTypeSpecChecker`、`GoInterfaceTypeSpecChecker`、`GoTypeParametersSpecChecker` 等。 |
| `com.goide.highlighting.legacyErrorInspections.GoUnresolvedReferenceInspection` | `visitReferenceExpression`、`visitFieldName`、`visitImportSpec` 分别检测未解析标识符/类型/标签/导入、未知结构体字段与歧义提升字段（`unresolved.reference`、`unresolved.type`、`unknown.field`、`ambiguous.promoted.field`），并直接关联 `calcQuickFixes` 所需上下文。 |
| `GoUnusedImportInspection` & `GoInvalidPackageImportInspection` & `GoSelfImportInspection` | 在 `visitGoFile` 校验每个 `GoImportSpec`：未使用导入、无效/无法解析导入、禁止导入 `internal`/`vendor`/`builtin`/绝对路径、`main` 包导入、自导入。 |
| `GoDuplicateInspection` & `GoUnusedVariableInspection` | `GoDuplicateInspection` 检查同包/同作用域重复声明、字段与方法同名、重复字段/方法/形参/类型参数/标签；`GoUnusedVariableInspection` 仅对函数体内局部变量（`shouldCheck`/`isUnused`）报 `ERROR` 并提供 QuickFix 数据。 |
| `GoFunctionCallInspection` & `GoTypesCompatibilityInspection` | `GoFunctionCallInspection` 检查函数调用与类型转换的参数过多/不足；`GoTypesCompatibilityInspection` 检查返回参数个数、`if`/`for` 非 `bool` 条件、无效 `range`、只写通道接收、`nil` 限制、泛型类型实参与类型兼容性。 |
| `META-INF/go-inspections.xml`（84 项 `localInspection`） | 仅 `GoCyclicImportsInspection` 为 `ERROR`、`GoMissingTrailingCommaInspection` 为 `NON_SWITCHABLE_ERROR`；其余 82 项多为 `WARNING`/`WEAK WARNING`（如未使用的未导出顶层符号、风格建议等），不属于实时编译错主线。 |

### Rust 实现方案与取舍

1. **目录结构（独占 `src/semantic/diagnostics/`）**
   - `mod.rs`：诊断入口、管线编排（语法 → 包错误 → 阶段二语义/类型）、快照查询 `diagnostics_for`。
   - `model.rs`：冻结 `B → D` 契约类型（`DiagnosticReport`, `DiagnosticItem`, `DiagnosticCode`, `DiagnosticSeverity`, `DiagnosticTag`, `DiagnosticFixData`）与 LSP `lsp_types::Diagnostic` 转换。
   - `cache.rs`：按 `(path_key, doc_version / fingerprint, graph_generation)` 缓存报告与 `result_id`，文档修改/关闭或包图换代时精确失效。
   - `syntax.rs`：单遍遍历 Tree-sitter CST 的 `has_error()` 子树，仅进入含错误分支（无语法错误时 $O(1)$ 跳过），合并相邻噪声节点并映射 UTF-16 范围。
   - `package_errors.rs`：将 `Workspace::package_errors()` 转换为按文件归属的诊断条目，带缓冲区修复去重。
   - `imports.rs` / `unresolved.rs` / `declarations.rs` / `unused.rs` / `types.rs`：阶段二各语义分析子模块，严格只读使用 `EngineSnapshot` / `DocumentSnapshot` / `scope` / `symbols` / `Resolver` / `implementation::method_sets`。
   - `tests.rs`：各子模块与契约的 Rust 单元测试。
2. **低误报优先原则（关键取舍）**
   - rs-gopls 的类型解析器目前以按需推断为主，尚未实现完整的 Go 规范常量求值与复杂泛型的所有边界。为保证真实项目可用性，阶段二所有语义/类型规则遵循 **保守判定**：当表达式位于 `ERROR` 子树内、依赖未加载的包图、或涉及未解析/未知类型/类型参数约束无法完全确定时，**静默跳过而非误报**。
3. **性能与增量策略**
   - 无语法错的干净文件，`syntax.rs` 通过根节点 `root.has_error()` 在纳秒级直接跳过。
   - 包图尚未就绪时，`publishDiagnostics` 和 `textDocument/diagnostic` 优先返回当前文档的语法诊断，不阻塞编辑器；包图加载完成（`workspace_loaded`）后自动重算已打开文档的语义诊断并推送更新。
   - 对同一文档与同一包图代次的重复 Pull 请求直接命中缓存并返回 `UnchangedDocumentDiagnosticReport`。
4. **待验证的推断**
   - GoLand 在运行态对“同文件存在语法错误时是否抑制后续语义错误”的具体边界，需通过 WP0 的 Groovy 采集脚本实测确认。
   - 既有 Python 回归脚本（特别是 `check_definitions.py`、`check_workspace_updates.py`）在收到主动推送的 `textDocument/publishDiagnostics` 通知时是否全部通过 `lsp_harness.py` 的通知队列无干扰放行，需在 WP2 接线后全量复测确认。

## 实施步骤（任务拆分与 Codex 委派方案）

按依赖关系拆分为 **5 个工作包（WP0–WP4）**，在独立分支 `feat/diagnostics-engine` 上推进；每个工作包边界清晰、独立可测，适合由主控拆解给 **Codex** 实施并逐包验收：

| 工作包 | 核心产出 | 独占/写入路径 | 执行方式与推荐 Codex 模型 |
| --- | --- | --- | --- |
| **WP0a · 契约冻结** | `Workspace::package_errors`（`F → B`）与 `diagnostics/model.rs`（`B → D`）及契约单测 | `src/workspace.rs`、`src/semantic/diagnostics/{mod,model}.rs` | **串行起点**，Codex（`codex/gpt-6.2-sol`）实现，完成后立即解锁 WP1 与 WP0b |
| **WP0b · GoLand 采集与夹具** | `tools/fixtures/diagnostics/` 合成夹具、`tools/capture_goland_diagnostics.groovy`、采集归档 `docs/goland-diagnostics-baseline.md` | `tools/fixtures/diagnostics/*`、`tools/capture_goland_diagnostics.groovy`、`docs/goland-diagnostics-baseline.md` | **与 WP1 并行**，Codex（`codex/gpt-6.2-sol`）搭建夹具与脚本，主控运行 GoLand 采集并核对基线 |
| **WP1 · 阶段一引擎核心** | `syntax.rs`、`package_errors.rs`、`cache.rs`、报告组装与 Rust 单元测试 | `src/semantic/diagnostics/{mod,syntax,package_errors,cache,tests}.rs` | **与 WP0b 并行**，Codex（`codex/gpt-6.2-astra`）实现并自测 |
| **WP2 · 阶段一 LSP 接线与回归** | `diagnosticProvider`、`textDocument/diagnostic`（Full/Unchanged/Cancel）、`publishDiagnostics` 推送、`tools/workspace_checks/diagnostics.py` | 薄改动 `src/{semantic,dispatch,main,scheduler,protocol}.rs` + 独占 `tools/workspace_checks/diagnostics.py` | 依赖 WP1，Codex（`codex/gpt-6.2-sol`）实现，全跑既有 23+ 项工作区回归确认零破坏 |
| **WP3 · 阶段二（上）：导入/名称/声明/未使用** | `imports.rs`、`unresolved.rs`、`declarations.rs`、`unused.rs` 及完备单测 | `src/semantic/diagnostics/{imports,unresolved,declarations,unused}.rs` | 依赖 WP2，Codex（`codex/gpt-6.2-astra`）实现（可与 WP4a 在独立文件并行） |
| **WP4a · 阶段二（下）：调用/返回/类型检查** | `types.rs`（调用实参、返回值、类型兼容性、接口实现、字面量字段、重复 case、`_` 用作值）及单测 | `src/semantic/diagnostics/types.rs` | 依赖 WP2，Codex（`codex/gpt-6.2-astra`）实现（与 WP3 文件无冲突，主控统一在 `mod.rs` 挂载） |
| **WP4b · GoLand 对照、性能基准与收尾** | `tools/check_goland_diagnostics.py`、`tools/compare_diagnostics.py`、扩展 `workspace_checks/diagnostics.py`、更新基线文档与本计划完成记录 | `tools/*diagnostics*`、`tools/workspace_checks/diagnostics.py`、`docs/*` | 依赖 WP3 + WP4a，Codex 编写校验脚本，主控执行 GoLand 性能对照与全量验收 |

> **协作与合并顺序建议**：WP0a 完成后可先向团队公示 `PackageLoadError` 与 `DiagnosticReport` 签名供 Track F / Track D 对齐；WP2 合并前验证所有 16 个既有 Python 校验脚本；WP3 与 WP4a 若并行在独立 worktree 开发，各自仅写入专属 `.rs` 文件，最终回到 `feat/diagnostics-engine` 统一挂载并运行 Cargo 验证（遵守项目 Cargo 产物目录约定）。

## 验证计划

### 构建与单元测试（每工作包完成后执行）

沿用环境配置的 Cargo 产物目录、工具链、profile、features 与 target（禁止 `--target-dir` 或覆盖 `CARGO_TARGET_DIR`）：

```powershell
cargo check -p rs-gopls --locked
cargo test -p rs-gopls --locked semantic::diagnostics
cargo check --workspace --locked
cargo test --workspace --locked
cargo build -p rs-gopls --release --locked
```

### GoLand 采集与严格对照

固定 GoLand `GO-263.7307.354`（通过 `$env:GOLAND_EXE` 或命令行参数传入，不在脚本与文档中硬编码个人路径）与当前环境 Go SDK（采集时记录精确 `go version` 与 `go env GOOS GOARCH`）：

```powershell
# 示例：执行前设置 $env:GOLAND_EXE 指向本机 goland64.exe
$capture = (Resolve-Path '.\tools\capture_goland_diagnostics.groovy').Path
Start-Process -FilePath $env:GOLAND_EXE -ArgumentList @('ideScript', ('"{0}"' -f $capture)) -WindowStyle Hidden

$target = (cargo metadata --format-version 1 --no-deps --locked | ConvertFrom-Json).target_directory
$server = Join-Path $target 'release\rs-gopls.exe'
$diagFixture = (Resolve-Path '.\tools\fixtures\diagnostics').Path

python -B .\tools\check_goland_diagnostics.py $server --workspace $diagFixture --fixture (Join-Path $diagFixture 'goland-diagnostics.json')
python -B .\tools\check_workspace_updates.py $server --module diagnostics
python -B .\tools\check_workspace_updates.py $server
python -B -m unittest discover -s tools -p 'test_lsp_harness.py'
python -B -m unittest discover -s tools -p 'test_workspace_checks.py'
```

### 性能对照与历史回归

- 使用 `tools/compare_diagnostics.py` 按 [导航性能对照](../PLANS.md#导航性能对照) 记录 GoLand 与 `rs-gopls` 发布版在共同有效诊断样本上的索引就绪首查、重复查询与编辑后查询的 P50/P95、样本数、缓存状态及峰值工作集，差异样本单列。
- 复跑既有 `check_definitions.py --stage 6` 及现有全部夹具回归（`type-navigation`、`implementation-navigation`、`hover-signature`、`symbol-navigation`），确认接入 `publishDiagnostics` 与 `diagnosticProvider` 后无任何功能退化。

## 完成记录

- 2026-11-27 完成本机 GoLand `2026.3.0.1`（`GO-263.7307.354`）插件清单与字节码静态分析（`GoAnnotator`、31 个 `SpecChecker`、29 个 `legacyErrorInspections`、`go-inspections.xml` 与 `GoListOutputParser`），核对 `lsp-types 0.97` 的 Push/Pull 协议类型与 Pre-PR 1/2 后的 Rust 模块边界，创建待确认计划并等待用户确认。
- 2026-11-27 验证 Wave 0 诊断夹具与采集脚本。`go test ./clean/...` 通过。GoLand `GO-263.7307.354` 的 11 个文件各完成 30 次重复计时，结果一致。两份干净源文件均为零错误，语法错样例核对了非 BMP 字符后的 UTF-16 范围。[诊断夹具](../../tools/fixtures/diagnostics/) · [采集脚本](../../tools/capture_goland_diagnostics.groovy)
