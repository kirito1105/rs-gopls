# Go Hover 与 SignatureHelp 开发计划

| 项目 | 内容 |
| --- | --- |
| 状态 | 进行中 |
| 创建日期 | 2026-11-26 |
| 用户确认 | 2026-11-26 已确认，按默认范围与格式开始实施 |
| 更新日期 | 2026-11-26 |
| 完成日期 | 待填写 |

## 目标与范围

在 Zed 中为 Go 源码提供 **悬停提示（`textDocument/hover`）** 与 **函数签名提示（`textDocument/signatureHelp`）**，延续 rs-gopls 的未保存缓冲区优先、后台包图加载与低延迟导航架构，并以 GoLand 2026.3（`GO-263.7307.354`）在 `E:\Code\localchat\server` 及补充夹具上的实测行为作为设计与验收基线。[项目愿景](../vision.md)

### 本阶段覆盖范围

1. **Hover（`textDocument/hover`）**
   - 声明与引用位置：包级函数、方法、接口方法、命名类型与类型别名、结构体字段（含嵌入字段与 struct tag）、包级/局部 `var` 与 `const`（含短变量声明、类型推断、字面量/常量初始化表达式）、函数参数与接收者、泛型类型参数、包名与导入路径（`import` 语句及限定符）。
   - 展示内容：Markdown 格式输出，包含 Go 代码块形式的声明签名（尽可能补齐包限定/接收者/类型参数/结构体标签/推断类型）以及清理后的 GoDoc 注释（上方紧邻注释 + 同行尾注释，过滤 `//go:*` 与 `//+build` 等编译指令，支持分组 `var`/`const`/`type` 的注释回退和包文档 `doc.go`/包注释）。
   - 返回精确的 UTF-16 `range`（对应悬停的目标标识符或导入路径）；空白处、字面量或无法解析的位置返回 `null`。
   - 支持内置预声明标识符（如 `error`、`any`、`comparable`、`string`、`int`、`make`、`new`、`len`、`cap`、`append`、`copy`、`delete`、`close`、`panic`、`recover`、`min`、`max`、`clear` 等，通过 Go SDK `builtin` 或内置声明解析）。
2. **SignatureHelp（`textDocument/signatureHelp`）**
   - 调用场景：普通函数调用、方法调用（含接口方法与提升方法）、函数类型变量/字段/返回值/闭包调用、方法表达式（`T.M(recv, ...)` / `(*T).M(recv, ...)`）、泛型实例化调用（显式与推断类型参数替换）、Go 内置函数调用（`append`、`make`、`new`、`copy`、`delete`、`len`、`cap`、`min`、`max`、`clear` 等）。
   - 参数高亮与边界：按最内层有效调用参数列表定位（遇到函数字面量、复合字面量或切片/索引括号时停止向上穿透，光标位于 `)` 之后不误触发）；展开分组参数（如 `a, b int` 拆为两个可高亮参数项）；按光标前顶层逗号数计算 `activeParameter`；变参函数（`...T`）超出最后一位参数时将 `activeParameter` 钳位到变参位；返回参数标签偏移（UTF-16 `[start, end]`）与关联的函数 GoDoc 注释。
   - 触发字符：声明 `signatureHelpProvider`（`triggerCharacters: ["(", ","]`，`retriggerCharacters: [","]`）。
   - 未保存/不完整输入容错：在用户刚输入 `foo(` 或 `foo(a, ` 等尚未闭合括号的编辑态源码下，依然准确返回签名与当前激活参数位。

### 明确不在本阶段范围

- 完整的常量折叠求值器（如复杂跨包算术/位运算 `iota` 表达式的精确求值；本阶段先覆盖直接字面量/单步表达式与简单 `iota` 展示，复杂常量折叠留待后续）。
- Go 关键字语言规范百科文档（如悬停在 `defer`/`select`/`go` 上弹出 GoLand 的内置 HTML 教学示例）与 DFA 空指针分析附加段落。
- 结构体字面量字段提示（`GoStructLiteralParameterInfoHandler`）与切片索引提示（`GoSliceParameterInfoHandler`）；待确认是否需要同时在 `textDocument/signatureHelp` 中支持泛型类型实参 `[T, U]` 提示（见下方待确认事项）。

### 待确认事项

1. `SignatureHelp` 本阶段是否仅覆盖函数/方法/闭包/内置函数调用的 `(...)` 实参列表，还是顺带覆盖泛型调用的类型实参 `[...]`（对应 GoLand 的 `GoCallTypeArgumentsParameterInfoHandler`）？默认建议先专注 `(...)` 调用实参，保持与 LSP 客户端触发体验一致。
2. Hover 中的 GoDoc 格式：输出干净的 Go 签名代码块 + 纯文本/轻量 Markdown 段落（过滤指令注释），不复刻 GoLand 专有的 HTML 超链接/外部 `pkg.go.dev` 跳转链接，是否符合预期？

## 验收标准

- [x] **GoLand 基线可重放**：在运行中的 GoLand（`GO-263.7307.354`）采集 `E:\Code\localchat\server`（修订 `532228d3d36bd9f0fffb4bda35c180485b2eadd6`）与补充夹具的 Hover（`GoDocumentationProvider`）及 ParameterInfo（`GoParameterInfoHandler`）基线，提交可重放的 Groovy 采集脚本与结构化基线语料，记录实际 Go SDK 版本。
- [ ] **Hover 能力与准确性**：LSP 初始化声明 `hoverProvider`。发布版在真实项目与补充夹具上覆盖函数、方法、接口方法、结构体、接口、类型别名、字段（含 tag 与嵌入）、包/局部变量、常量、参数/接收者、泛型、导入/包名、内置符号与空结果；签名要素、类型信息、GoDoc 注释（含指令过滤、尾注释、分组声明回退）及 UTF-16 `range` 与基线一致。
- [ ] **SignatureHelp 能力与准确性**：LSP 初始化声明 `signatureHelpProvider`（触发字符 `(`、`,`）。发布版在真实项目与补充夹具上覆盖函数/方法/接口方法/函数值/方法表达式/泛型调用/内置函数、嵌套调用、分组参数展开、变参钳位、光标在未闭合括号与逗号后的编辑态输入，以及函数字面量/复合字面量边界不误触发；签名文本、参数列表偏移、`activeParameter` 与 GoDoc 一致。
- [ ] **未保存编辑与工作区一致性**：未保存缓冲区修改函数签名、变量类型或注释后，Hover 与 SignatureHelp 立即反映最新内容；关闭覆盖缓冲区后恢复磁盘结果；后台包图加载期间保持本地解析可用，包图刷新后自动更新跨包结果；不破坏现有声明、类型定义、引用、实现跳转与工作区回归。
- [ ] **性能对照 GoLand**：按 [导航性能对照](../PLANS.md#导航性能对照) 固定主机、GoLand 构建号、项目修订、Go SDK 与样本集合，分别对比已索引暖态新查询、重复查询与编辑后查询的 P50/P95 及峰值工作集，并将结果归档到基线文档。
- [ ] **既有回归全部通过**：`cargo check -p rs-gopls --locked`、`cargo test -p rs-gopls --locked`、发布版构建以及 `check_definitions.py --stage 6`、`check_goland_definitions.py`、`check_goland_references.py`、`check_goland_implementations.py`、`check_workspace_updates.py` 全部通过。

## 实现依据

### REA 产物证据

使用 `rea-agents@6.7.0 inspect-artifact` 对 `E:\Apps\goland-2026.3.0.1.win\plugins\go-plugin\lib\modules\intellij.go.impl.jar` 进行只读静态清点：
- **GoLand 构建号**：`GO-263.7307.354`（`E:\Apps\goland-2026.3.0.1.win\build.txt`）
- **REA Evidence ID**：`ev_6c7c39a219d2e02923e7b819150326c434f3135777e22df0b1f7e49c6cdda5ee`（Inspection ID：`ai_5e5ea293b789113e08b644e2c659bc561d6ea08a7a0acc261df63a511d23ea33`）
- **产物 SHA-256**：`2a13f1a753d5e15db70c5758e17db1dc28cc790e1d329e574d8b6a078d636784`（ZIP/JAR 中央目录静态清单，未执行产物）

### GoLand 字节码观察（JBR + ASM Textifier）

配合安装包自带 `jbr/bin/java.exe` 与 `lib/intellij.libraries.asm.jar`（`org.jetbrains.org.objectweb.asm.util.Textifier`）核查插件注册 `intellij.go.impl.xml` 及相关类：

| 安装包中的类或注册 | 已确认的处理流程与规则 |
| --- | --- |
| `intellij.go.impl.xml` | 注册 `<lang.documentationProvider language="go" implementationClass="com.goide.documentation.GoDocumentationProvider"/>`、`<codeInsight.typeInfo language="go" implementationClass="com.goide.codeInsight.GoExpressionTypeProvider"/>` 与 `<codeInsight.parameterInfo language="go" implementationClass="com.goide.editor.GoParameterInfoHandler"/>`（另有 `GoCallTypeArgumentsParameterInfoHandler`、`GoTypeArgumentsParameterInfoHandler`、`GoStructLiteralParameterInfoHandler`、`GoSliceParameterInfoHandler`）。 |
| `com.goide.documentation.GoDocumentationProvider` (`generateHoverDoc`, `generateDoc`, `adjustDocElement`, `calcSignature`, `getElementSubstitution`) | `generateHoverDoc` 复用 `generateDoc`；`adjustDocElement` 将 `GoPackageClause` / `GoImportSpec` 归一化为目标包目录，将 `GoVarOrConstDeclaration` / `GoTypeDeclaration` / `GoFieldDeclaration` 归一化到首定义。`calcSignature` 按 `GoTypeSpec`（区分 `=` 别名与类型定义及类型参数）、`GoVarOrConstDefinition`（`const`/`var` + `getGoType` + `getResolvedValueText`）、`GoParamDefinition`/`GoReceiver`、`GoFieldDefinition`（含所属类型与 `getTagText`）、`GoSignatureOwner` 渲染签名，并支持泛型替换 `getElementSubstitution`。 |
| `com.goide.documentation.GoDocumentationProvider` (`getCommentsForElement`, `getUpperComments`, `getAfterComments`, `excludeDirectives`, `findDocFileForDirectory`) | 文档注释合并紧邻声明上方（无 `\n\n` 空行隔断、且行首起始）的注释与声明同行尾部注释；通过 `excludeDirectives` 过滤标准 Go 编译指令（`isStandardDirectiveComment`）与 GoLand 指令注释；单定义或首定义缺少自有注释时回退到外层 `var`/`const`/`type` 声明注释；包文档优先从包内 `doc.go` 或带包注释的源文件提取。 |
| `com.goide.editor.GoParameterInfoUtil$Companion` (`findGoArgumentListForParameterInfo`) | 从光标处向上查找最内层 `GoArgumentList`，遇到 `GoFunctionLit` 或 `GoLiteralValue` 停止穿透；排除光标处于切片/索引 `[...]` 内部或位于闭合 `)` 之外的情形。 |
| `com.goide.editor.GoParameterInfoHandler` (`findElementForParameterInfo`, `findFunctionType`, `updateParameterInfo`, `updateUI`, `getParameterPresentations`, `getGoDoc`) | 识别 `GoCallExpr` 与 `GoBuiltinCallExpr`，通过 `findFunctionType` 获取被调表达式类型并展开具名函数类型的底层 `GoFunctionType`。`updateParameterInfo` 通过统计光标前同级 `COMMA` 计算当前参数索引。`updateUI` 用 `getParameterPresentations` 展开分组参数并保留变参 `...`，变参越界时以 `Math.min(index, params.size() - 1)` 钳位，并从所属 `GoFunctionOrMethodDeclaration` 附加 GoDoc。 |

### Rust 实现方案与取舍

1. **模块拆分**（尽量保持 `src/semantic.rs` 精简，新增独立子模块）：
   - [`src/semantic/hover.rs`](../../src/semantic/hover.rs)：目标解析、声明签名格式化、GoDoc 注释提取与指令过滤、包/导入/内置符号悬停组装。
   - [`src/semantic/signature_help.rs`](../../src/semantic/signature_help.rs)：活跃调用点与逗号计数定位（含未闭合括号的 Tree-sitter ERROR 节点容错）、被调函数签名解析（普通函数、方法、接口方法、函数值、方法表达式、泛型实参替换、内置函数）、参数展开与偏移计算。
   - [`src/symbols.rs`](../../src/symbols.rs) 与 [`src/semantic.rs`](../../src/semantic.rs)：按需补齐参数名/返回值名保留（当前 `FunctionSignature` 只保留了参数/返回值类型，需要保留参数名称以便渲染 `a int, b string` 签名与 Hover）、字段 tag/类型渲染辅助，以及对 `Engine::hover` / `Engine::signature_help` 的统一入口（复用现有 `QueryProgress::Pending` 后台包图等待机制）。
   - [`src/main.rs`](../../src/main.rs)：声明 `hover_provider` 与 `signature_help_provider`，接入 `textDocument/hover` 与 `textDocument/signatureHelp` 请求分派及取消处理。
2. **签名与类型渲染策略**：
   - 对已知源码位置的声明（同文件、同包、跨包、SDK 源码），优先直接结合 AST 节点与解析后的类型/泛型实参生成清晰的单行或多行 Go 签名；跨包类型按上下文保留必要包限定名。
   - 未保存编辑产生语法不完整（如 `foo(a, `）时，先通过 Tree-sitter 树定位外层 `call_expression` / `ERROR` 节点，必要时退回轻量词法括号/逗号栈扫描，确保实时输入场景下的 `SignatureHelp` 稳定命中。
3. **待验证的推断**：
   - GoLand 在 `localchat/server` 上对分组 `const`/`var` 块中第二项及以后成员、接口嵌入方法提升、以及方法表达式调用（`(*SqlStore).Close(s)`）的注释与签名细节，需通过步骤 1 的 GoLand 实测基线确认。
   - `go version` 当前环境 PATH 上为 `go1.24.3`（此前第四阶段文档记录使用 `go1.27.4`）；基线采集与性能对比时需统一并记录实际 GoLand 所用 SDK 与测试所用 `go` 路径。

## 实施步骤

1. **计划确认与 GoLand 基线采集**：
   - 收到用户确认后更新计划状态为「进行中」。
   - 编写可重放的 GoLand Groovy 采集脚本（如 `tools/capture_goland_hover_and_signature.groovy`），在运行中的 GoLand 中采集 `localchat/server` 及新建的 `tools/fixtures/hover-signature/` 补充夹具的 Hover 与 ParameterInfo 实测基线，输出到 `docs/goland-hover-signature-baseline.md` 与 `tools/fixtures/` JSON 语料并提交。
2. **基础数据结构扩展与 `textDocument/hover` 实现**（委派 Paseo `编码工作` agent 实施并自测）：
   - 补齐函数/方法签名参数名、返回值名、接收者文本与文档注释提取基础设施（含指令过滤、尾注释、分组声明回退、包文档与内置符号支持）。
   - 接入 `hoverProvider` 与 `textDocument/hover`，实现完整 Hover 渲染与 UTF-16 范围返回，补齐单元测试与 stdio 严格检查器（`tools/check_goland_hover.py`），通过回归后提交。
3. **`textDocument/signatureHelp` 实现**（委派 Paseo `编码工作` agent 实施并自测）：
   - 实现调用点定位（含嵌套调用、字面量边界阻断、不完整括号编辑容错）、被调签名解析（函数、方法、函数值、方法表达式、泛型实例化、内置函数）、分组参数展开、变参钳位、UTF-16 参数偏移与 GoDoc 附加。
   - 接入 `signatureHelpProvider` 与 `textDocument/signatureHelp`，补齐单元测试、编辑态用例与 stdio 检查器（`tools/check_goland_signature_help.py`），通过回归后提交。
4. **工作区动态更新、性能对照与最终回归验收**：
   - 在 `tools/check_workspace_updates.py` 中补充未保存编辑/关闭恢复/包图刷新下的 Hover 与 SignatureHelp 用例。
   - 构建 release 版本，按 [导航性能对照](../PLANS.md#导航性能对照) 编写对比脚本并直接对照 GoLand 实测暖态新查询、重复查询与编辑后 P50/P95 及峰值工作集，复跑全部历史导航回归，更新基线文档、README 与本计划完成记录后提交。

## 验证计划

- **GoLand 基线**：在运行中的 GoLand `GO-263.7307.354` 上，固定 `localchat/server` 修订 `532228d3d36bd9f0fffb4bda35c180485b2eadd6` 与统一 Go SDK，采集 Hover 与 ParameterInfo 样本（覆盖函数、方法、接口、结构体、字段与 tag、类型别名、变量/常量、参数、泛型、导入/包文档、内置符号、嵌套调用、变参、方法表达式、空结果）。
- **构建与回归**：
  - 日常改动运行 `cargo check -p rs-gopls --locked` 与 `cargo test -p rs-gopls --locked`（遵守仓库 Cargo 产物目录约定，不覆盖 target 目录）。
  - 发布验收运行 `cargo build -p rs-gopls --release --locked`，用产物目录中的 `release\rs-gopls.exe` 执行：
    - 新增：`tools/check_goland_hover.py`、`tools/check_goland_signature_help.py`
    - 既有：`tools/check_definitions.py --stage 6`、`tools/check_goland_definitions.py`、`tools/check_goland_references.py`、`tools/check_goland_implementations.py`、`tools/check_workspace_updates.py`
- **性能对照**：按 [导航性能对照](../PLANS.md#导航性能对照) 与运行中的 GoLand 同样本对比 Hover 与 SignatureHelp 的暖态新查询、重复查询与编辑后耗时（P50/P95、样本数与缓存状态）及峰值工作集，差异样本单列。

## 完成记录

- 2026-11-26 使用 `reverse-engineer-anything`（REA `ev_6c7c39a219d2e02923e7b819150326c434f3135777e22df0b1f7e49c6cdda5ee`）与 GoLand JBR/ASM 字节码完成 `GoDocumentationProvider` 与 `GoParameterInfoHandler` 静态分析，创建待确认计划。
- 2026-11-26 用户确认计划，更新状态为「进行中」。在 GoLand `GO-263.7307.354`（Go `1.27.4`）采集 `localchat/server` 34 组与补充夹具 33 组 Hover / ParameterInfo 基线并归档。[GoLand Hover 与 SignatureHelp 基线](../goland-hover-signature-baseline.md)
