# rs-gopls 并行协作与模块拆分

按功能线分配模块和验收材料，由公共模块的维护人统一接线。先解耦请求调度与测试夹具，再固定跨线接口，让各线能独立开发和验证。

## 协作目标与当前冲突热区

以 `779fe78` 为基线，优先处理以下五处。直接按功能切分支时，各线会同时修改请求分派、语义查询和测试入口。文本合并成功也可能改变其他请求的调度或失效行为。

| 冲突热区 | 多人容易改到同一处的原因 |
| --- | --- |
| [`src/semantic.rs`](../src/semantic.rs)（3,681 行） | 缓冲区编辑、磁盘索引、名称与类型解析、分批查询都在这里，新功能和缓存修复会改到相同的入口。 |
| [`tools/check_workspace_updates.py`](../tools/check_workspace_updates.py) | 工作区、未保存编辑、包图刷新和查询取消共用会话辅助函数及同一用例清单。 |
| [`src/main.rs`](../src/main.rs)（732 行） | 新 LSP 能力都经过能力声明和请求分派，长查询还共用事件循环和取消逻辑。 |
| [`src/symbols.rs`](../src/symbols.rs)（1,497 行）与 [`src/workspace.rs`](../src/workspace.rs)（423 行） | 声明索引与活动包图被多条线复用。改动符号身份、测试包归属或包图失效会同时影响多种查询。 |
| [`tools/compare_definitions.py`](../tools/compare_definitions.py) 的 `LspClient` | 验收和基准脚本从声明跳转脚本导入客户端。通知与多请求并发需要改变共享的消息接收方式，容易影响既有脚本。 |

## 并行前的两个解耦 PR

两个 PR 可从同一 `main` 并行开发。Pre-PR 1 负责生产代码及随模块迁移的内联测试，Pre-PR 2 负责共享测试支撑和独立用例的注册。分别用现有入口验证，通过后再合入跨线接口。

### Pre-PR 1 · Rust 调度与语义引擎拆文件

- 拆出 `Source` / `LineIndex`、`DiskIndex` / `IndexReader` 和 `Resolver`。让 `Source::edit` 提供通用的文档变更信息，由实现跳转缓存判定能否沿用 `implementation::LineShift`，解除源文件编辑对实现跳转模块的依赖。
- 将 `main.rs` 消费的 `Search` / `SearchResult` 泛化为统一任务接口。任务分批推进，完成时交付序列化后的 LSP 结果。调度器按请求 ID 管理等待包图、取消和回复，并核对包图代次。用引用、实现跳转和工作区符号验证多个请求交错时每个 ID 只收到一次回复。
- 请求路由只负责解码参数、调用处理器和提交任务。由各模块提供能力描述，统一注册。通过稳定的只读 Facade 提供文档快照、UTF-16 位置转换、作用域、声明索引、名称与类型解析及活动包图查询，把可变缓存和索引失效留在引擎内部。

### Pre-PR 2 · Python / Rust 测试夹具解耦

- 将 `LspClient`、工作区会话和可控制完成时机的延迟 `go list` 夹具抽到 `tools/lsp_harness.py`。客户端允许多个请求同时在途，按请求 ID 分发响应，单独保留通知队列，并处理服务端请求。用响应乱序、通知交错和动态文件监听注册验证共享客户端。
- 将 `check_workspace_updates.py` 改成统一验收入口，由 `tools/workspace_checks/` 下的文件各自注册用例。原命令默认运行全部用例，新增用例在所属功能线的文件内注册。
- Rust 测试统一复用 `TestProject` 来创建隔离的临时项目和包图。各线在自己的模块内维护用例和专用数据。[Rust 测试支撑](../src/test_support.rs)

## 六条并行功能线

| 功能线 | LSP 能力 | 独占源码目录 |
| --- | --- | --- |
| Track A · 上下文代码补全 | `textDocument/completion`、`completionItem/resolve` | `src/semantic/completion/` |
| Track B · 实时诊断引擎 | `textDocument/publishDiagnostics`、`textDocument/diagnostic` | `src/semantic/diagnostics/` |
| Track C · 安全重命名 | `textDocument/prepareRename`、`textDocument/rename` | `src/semantic/rename/` |
| Track D · 格式化与代码操作 | `textDocument/formatting`、`textDocument/codeAction`（`source.organizeImports`、`quickfix`） | `src/semantic/actions/` |
| Track E · 编辑器单文件增强 | `textDocument/semanticTokens/full`、`textDocument/inlayHint`、`textDocument/foldingRange`、`textDocument/documentHighlight` | `src/semantic/editor/` |
| Track F · 基座、层级与性能 | `textDocument/prepareCallHierarchy` 与 `callHierarchy/*`，`textDocument/prepareTypeHierarchy` 与 `typeHierarchy/*` | `src/semantic/hierarchy/` |

各线独占下列路径。路径从仓库根目录算起，`<线名>` 取表中源码目录的末段，例如 A 用 `completion`，F 用 `hierarchy`。

- 测试放 `src/semantic/<线名>/tests.rs` 和 `tools/workspace_checks/<线名>.py`，专用 Go 夹具及查询样本放 `tools/fixtures/<线名>/`。
- 计划放 `docs/plans/YYYY-MM-DD-go-<线名>-<功能名>.md`，GoLand 基线放 `docs/goland-<线名>-baseline.md`。同一功能的后续验收更新原计划。

GoLand REA 可从 `GO-263.7307.354` 的 `plugins/go-plugin/lib/modules/intellij.go.impl.jar` 内的 `intellij.go.impl.xml` 和下列类开始。计划记录实际安装包、构建号、类或方法和 Rust 对应方案，行为基线由运行中的 GoLand 采集。[实现依据写法](plans/2026-11-26-go-document-and-workspace-symbols.md#实现依据)

### Track A · 上下文代码补全

通过 Facade 只读使用 `scope`、`symbols` 和 `Resolver`，在本线处理光标上下文、候选排序和补全项解析。自动导入通过 D 的 `import_edits` 生成 `additionalTextEdits`，解析补全项时核对文档版本。GoLand 从 `GoCompletionContributor` 和 `GoAutoImportCompletionContributor` 查起。

### Track B · 实时诊断引擎

阶段一覆盖语法和包加载错误，只读消费文档快照及 F 的结构化 `go list` 错误。阶段二通过 `Resolver` 和声明索引补充语义与类型诊断。推送和拉取复用按快照生成的诊断报告，D 的 QuickFix 读取同一份结构化结果。GoLand 从 `GoAnnotator` 和 `localInspection` 注册查起。

### Track C · 安全重命名

从 Facade 只读复用引用扫描、`scope` 的遮蔽检查和 `implementation/method_sets` 的方法集判定。先验证目标及新名称，核对修改后的引用绑定与方法集约束，再通过 `WorkspaceEdit.documentChanges` 返回编辑。已打开文档携带版本，生成编辑前核对使用的快照。GoLand 从 `GoRenamePsiElementProcessor`、`GoRenameMethodProcessor` 和 `GoRenameInputValidator` 查起。

### Track D · 格式化与代码操作

优先冻结 `src/semantic/actions/imports.rs` 的 AST 级导入编辑接口，供 A 自动导入和本线 Organize Imports 复用。格式化读取文档快照，QuickFix 用 B 的诊断代码和数据选择修复，再为同一快照生成编辑。GoLand 从 `GoFormattingModelBuilder`、`GoFmtFormattingService`、`GoImportOptimizer` 和 `GoReferenceImporter` 查起。

### Track E · 编辑器单文件增强

只读使用当前文件的语法树、行索引和局部绑定。四类请求都在单文件内完成，零跨包长查询，可独立于包图验收，写入范围与其他线几乎没有交集。GoLand 从 `GoHighlightingAnnotator`、`GoInlayParameterHintsProvider`、`GoFoldingBuilder` 和 `highlightUsagesHandlerFactory` 注册查起。

### Track F · 基座、层级与性能

指定一名 Owner 维护 `src/workspace.rs`、`src/workspace_roots.rs` 及 `src/main.rs` 的核心调度与事件循环。负责多 Workspace Folder 及文件夹变更、结构化 `go list` 错误输出和统一基准框架。层级查询只读复用活动包图、引用扫描和方法集，通过统一任务接口分批推进。F 在现有 `implementation` 模块负责 Phase 4 冷反查优化，沿用原计划与基线。GoLand 从 `GoCallHierarchyProvider`、`GoTypeHierarchyProvider`、`VgoWorkspaceModelUpdater` 和 `GoInheritorsSearch` 查起。[第四阶段计划](plans/2026-11-25-go-implementation.md) · [实现跳转基线](goland-implementation-baseline.md)

## 跨线窄接口契约

生产方与消费方先确认下列签名、数据语义和快照失效规则，将接口及最小契约用例先合入 `main`，再按约定并行。消费方可用本线夹具验证空结果、失败和过期输入。签名变更由双方和公共模块 Owner 一起评审。

1. **工作区结构化包错误（F → B）**　包图快照提供 `fn package_errors(&self) -> &[PackageLoadError]`。F 从 `go list` 的 `Error` / `DepsErrors` 提取包 ID、错误类别、消息及可选文件 URI 和 UTF-16 范围，工作区根和包图代次由快照标识。保留测试包身份，无可靠位置时提供包级错误。B 从相应代次的快照读取错误。
2. **结构化诊断模型（B → D）**　`fn diagnostics_for(&self, document: &DocumentSnapshot<'_>) -> Option<&DiagnosticReport>` 按文档 URI、文本快照和包图代次读取报告，快照过期时返回 `None`。每项包含稳定的诊断代码、严重度、UTF-16 范围、展示消息和类型化的修复数据。D 按代码和数据匹配修复。
3. **AST 级导入编辑（D → A）**　`fn import_edits(document: &DocumentSnapshot<'_>, operation: ImportOperation<'_>) -> Result<Vec<TextEdit>, ImportEditError>` 支持添加指定路径与可选别名的导入，也可整理现有导入。返回基于该快照的互不重叠的 UTF-16 编辑。保留导入注释及其归属和 `_` 副作用导入，已有相同导入时返回空编辑，无法安全修改时报告原因。

## 按团队规模分工

| 人数 | 推荐分工 |
| --- | --- |
| 2 人 | 一人负责 F、B、C，一人负责 D、A、E。先交 F 的错误接口和 D 的导入编辑接口，再推进诊断和补全。 |
| 3–4 人 | 3 人时分为 F、B + C、D + A + E。4 人时分为 F、B、D + A、C + E。兼管 D 和 A 的人先交付导入契约。 |
| 5–6 人 | 5 人时 A、B、D、F 各一人，C 和 E 由一人负责。6 人时各线一人，F 的 Owner 负责接线。 |

## 日常防冲突与合入清单

- 一条功能线一个分支前缀，例如 `feat/completion-context`、`feat/diagnostics-package-errors`。解耦用 `refactor/pre-pr-engine-split`、`refactor/pre-pr-test-harness`。
- 公共文件采用单一 Owner 加薄接线。功能 PR 在本线提交处理器、能力描述和测试，在 PR 中列出需接入的能力和事件。由 F 的 Owner 串行提交 `main.rs`、`semantic.rs` 和 `workspace.rs` 的公共改动。
- 计划、GoLand 基线、夹具和测试按线独立提交。新功能先按模板拟好待确认的计划并获得用户确认，范围或验收有实质变化时更新原计划并重新确认。交付前勾选验收项并记录实际结果。[协作规则](../AGENTS.md) · [计划模板](PLANS.md)
- 合入 `main` 前，由 Owner 汇总本批已验收的能力和基线链接，统一更新发布文档。[README.md](../README.md) · [docs/vision.md](vision.md)

### 合入前检查

- [ ] Rust 先跑 `cargo check -p rs-gopls --locked`，再跑相关 `cargo test -p rs-gopls --locked` 用例，构建与测试零告警。沿用环境配置的产物目录、工具链、profile、features 和 target。禁用 `--target-dir`，禁止覆盖 `CARGO_TARGET_DIR` / `CARGO_BUILD_BUILD_DIR` 或另建目录重试。
- [ ] 新能力的 GoLand 基线可重放。导航性能直接对照运行中的 GoLand，固定环境并核对完整结果与范围，记录索引就绪后首查、复查、编辑后查询的样本数及 P50/P95。[导航性能对照](PLANS.md#导航性能对照)
- [ ] 历史回归无退化。已有声明跳转、类型定义、引用、实现跳转、Hover、签名提示和符号查询通过受影响的回归。共用调度或包图有改动时，验证未保存编辑、包图刷新和取消，并复测已有导航性能。[构建与验收命令](../README.md#build-and-verify) · [工作区回归入口](../tools/check_workspace_updates.py)
