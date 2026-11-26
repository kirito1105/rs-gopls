# Track D · 格式化与代码操作开发计划

| 项目 | 内容 |
| --- | --- |
| 状态 | 已完成 |
| 创建日期 | 2026-11-27 |
| 用户确认 | 已确认（2026-11-27 确认 F + B + D 三线联合开工，Codex 使用 full-access 模式，严格路径脱敏） |
| 更新日期 | 2026-11-27 |
| 完成日期 | 2026-11-27 |

## 目标与范围

按 [并行协作与模块拆分](../COLLABORATION.md#track-d--格式化与代码操作) 实现 **Track D · 格式化与代码操作**，与 Track F（基座）、Track B（实时诊断）协同构成完整的 `F → B → D` 链路，并率先冻结 **`D → A`（AST 级导入编辑 `import_edits`）** 跨线契约供并行开发 Track A（上下文代码补全）的同事复用。[项目愿景](../vision.md)

### 本阶段覆盖范围

1. **AST 级导入编辑契约（`D → A`，优先冻结）**
   - 独占 `src/semantic/actions/imports.rs`，暴露：
     `pub(crate) fn import_edits(document: &DocumentSnapshot<'_>, operation: ImportOperation<'_>) -> Result<Vec<TextEdit>, ImportEditError>`
   - 支持两种操作：
     - **`AddImport { path, alias }`**：向现有 `import (...)` 分组或 `package` 声明后插入新导入（单条 `import "x"` 必要时升级为分组块），自动按标准库 / 第三方 / 本地模块分组与字典序插入，已有等价导入时返回空编辑，不破坏已有注释与 `_` / `.` / `"C"`（cgo）导入。
     - **`OrganizeImports { unused_import_paths, module_prefixes }`**：删除指定未使用导入与重复导入、去除与默认包名一致的冗余别名、清理空 `import ()` 块、按分组与字典序重排导入，严格保留导入行上/行尾注释归属、`_` 副作用导入、`.` 点导入及 cgo `"C"` 导入块（含紧邻 `import "C"` 上方的 cgo C 注释，绝不拆散！）。
   - 返回基于当前 `DocumentSnapshot` 的互不重叠 UTF-16 `TextEdit` 列表；语法树无法安全定位导入区域时返回明确的 `ImportEditError` 而非破坏源码。
2. **文档格式化（`textDocument/formatting`）**
   - 独占 `src/semantic/actions/formatting.rs`：
     - 复用 Go 工具链自带的 `gofmt`（参照 GoLand `GoFmtFormattingService`），通过 stdin 传入当前未保存缓冲区文本（无需落盘），支持 CRLF/LF 行尾保留与精确最小化（或单块全文档）UTF-16 `TextEdit` 输出。
     - 防止空输出覆盖非空文档（对齐 GoLand `isUnexpectedEmptyResult` 保护）；源码有语法错误时返回空编辑或标准失败提示，不破坏编辑态代码；支持无 `gofmt` 时的清晰错误回退。
3. **代码操作（`textDocument/codeAction`）**
   - 独占 `src/semantic/actions/code_action.rs`：
     - **`source.organizeImports`**：结合 Track B 的 `DiagnosticReport`（识别未使用导入）与当前工作区模块路径，调用 `import_edits(OrganizeImports)` 生成 `CodeAction`（支持 `only: ["source.organizeImports"]` 按保存自动整理）。
     - **`quickfix`**：只读消费 Track B 的 `Engine::diagnostics_for(&document)`，按稳定 `DiagnosticCode` 与类型化 `DiagnosticFixData` 匹配光标/选区命中的诊断，生成对应 `CodeAction`（携带 `diagnostics` 关联与 `WorkspaceEdit`）：
       1. `import.unused` → 删除未使用导入（复用 `import_edits`）；
       2. `name.unresolved`（包限定符未导入且工作区/标准库存在候选包）→ 自动添加导入 `Import "<pkg>"`（复用 `import_edits(AddImport)`，多候选时输出多个按包名排序的 QuickFix）；
       3. `var.unused` → 将未使用局部变量改为 `_`（或安全替换为 `_ = x`）；
       4. `shortvar.no_new_vars` → 将 `:=` 替换为 `=`。
     - 按 LSP `CodeActionContext.only` 过滤请求类型，避免无关计算。

### 明确不在本阶段范围

- `textDocument/rangeFormatting`、`textDocument/onTypeFormatting` 与 `codeAction/resolve` 延迟解析（本阶段 `codeAction` 直接内联返回 `edit`）。
- 复杂重构类 CodeAction（如 Extract Function、Inline Variable、Implement Interface 方法桩生成，后续可独立立项）。
- 第三方外部格式化器配置（如 `gofumpt`、`goimports` 外部二进制强依赖；导入整理全部由 Rust AST 引擎独立完成，格式化默认使用 Go SDK 自带 `gofmt`）。

### 并行协作与写入边界

- **独占路径**：`src/semantic/actions/*`、`tools/workspace_checks/actions.py`、`tools/fixtures/actions/*`、`tools/*actions*`、`docs/goland-actions-baseline.md`。
- **薄接线**：仅在 `src/dispatch.rs`（注册 `documentFormattingProvider`、`codeActionProvider` 及两个处理器）和 `src/semantic.rs` 暴露模块入口，由 Track F 统一维护。

## 验收标准

### `D → A` 导入编辑契约
- [x] **`import_edits` 行为完备**：单元测试覆盖无 import 块新增、单行 import 转分组块、已有分组按标准库/第三方/本地模块插入、重复导入返回空编辑、保留行上/行尾注释与 `_`/`.`/`"C"`（含紧邻 cgo 注释）、清理空 `import ()` 块、CRLF 与非 BMP UTF-16 范围、语法破坏时安全返回 `ImportEditError`。

### 格式化与代码操作
- [x] **`textDocument/formatting`**：初始化声明 `documentFormattingProvider`。对未保存缓冲区的缩进、空行、结构体字段对齐与注释排版输出与 `gofmt` 一致的结果；已格式化文件返回空编辑；存在语法错误时不破坏内容；空结果防覆盖保护生效。
- [x] **`source.organizeImports`**：初始化声明 `codeActionProvider`（含 `codeActionKinds: ["quickfix", "source.organizeImports"]`）。请求 `only: ["source.organizeImports"]` 时正确删除未使用导入、去重、排序分组并保留注释与副作用/cgo 导入。
- [x] **`quickfix` 与 Track B 快照联动**：覆盖删除未使用导入、未解析限定符自动导入（单候选与多候选）、未使用局部变量改 `_`（单变量短声明同步 `:=` 改 `=`）、`:=` 改 `=` 四类核心修复；当 `only` 过滤或快照无对应诊断时不返回无关操作；应用返回的 `WorkspaceEdit` 后重新拉取 Track B 诊断，对应错误消失。

### GoLand 对照、性能与回归
- [x] **GoLand 基线与脱敏**：在 GoLand `GO-263.7307.354` 上通过脱敏脚本 `tools/capture_goland_actions.groovy` 采集 `tools/fixtures/actions/` 的 `GoImportOptimizer` 与 `gofmt` / QuickFix 对照基线，归档至 `docs/goland-actions-baseline.md`，不含硬编码个人路径。
- [x] **性能与全量回归**：按 [导航性能对照](../PLANS.md#导航性能对照) 记录 `formatting`、`organizeImports` 与 `codeAction` 耗时；`cargo check --workspace --locked`、`cargo test --workspace --locked` 零告警；`tools/check_workspace_updates.py`（含新增 `actions` 模块）与既有全部回归通过。

## 实现依据

### GoLand 安装包静态核查（`GO-263.7307.354`）

只读检查 GoLand `2026.3.0.1` 安装目录 `<GOLAND_HOME>` 的 `plugins/go-plugin/lib/modules/intellij.go.impl.jar`（SHA-256 `2A13F1A753D5E15DB70C5758E17DB1DC28CC790E1D329E574D8B6A078D636784`），通过 `jbr/bin/java.exe` 与 ASM `Textifier` 核查：

| GoLand 类 / 注册 | 已确认的处理规则 |
| --- | --- |
| `intellij.go.impl.xml` | 注册 `<lang.formatter implementationClass="com.goide.formatter.GoFormattingModelBuilder"/>`、`<formattingService id="GoFmtFormattingService" implementation="com.goide.formatter.service.GoFmtFormattingService"/>`、`<lang.importOptimizer implementationClass="com.goide.codeInsight.imports.GoImportOptimizer"/>`、`<referenceImporter implementation="com.goide.psi.impl.imports.GoReferenceImporter"/>`。 |
| `com.goide.formatter.service.GoFmtFormattingService` | 调用 Go SDK 的 `gofmt` 执行格式化，并内置 `isUnexpectedEmptyResult` 防止异常空输出清空源码、`getExePath` 解析可执行文件。 |
| `com.goide.codeInsight.imports.GoImportOptimizer` | 通过 `GoImportsSorter` 排序分组；`filterUnusedImports` 显式保留 `_`、`.` 与 `"C"`（`getUsedCImports`）；`findDuplicatedEntries` 去重；`findRedundantImportIdentifiers` + `getAssumedPackageName`（处理 `go-` 前缀与 `isVersionSuffix` 如 `/v2`）识别冗余别名；`deleteImportSpec` 保留注释归属；`deleteEmptyDeclarations` 删除空导入块。 |
| `com.goide.psi.impl.imports.GoReferenceImporter` & `GoImportPackageQuickFix` | 基于未解析包限定引用检索候选包路径并插入对应导入声明。 |

### Rust 实现方案与取舍

1. **纯 Rust AST 导入引擎（`src/semantic/actions/imports.rs`）**：不依赖外部 `goimports` 二进制，直接解析 Tree-sitter `import_declaration` / `import_spec` / 相邻注释节点，毫秒内产出互不重叠的精确 UTF-16 `TextEdit`，完美适配 Track A 实时补全的 `additionalTextEdits` 与 Track D 的 `source.organizeImports`。
2. **分组规则**：遵循 Go 社区及 GoLand 标准三段分组——① 标准库（首段不含 `.`）、② 第三方依赖（首段含 `.` 且不匹配当前工作区模块前缀）、③ 本地模块（匹配当前工作区 `module` 前缀），组间空一行，组内按导入路径字典序排列；独立 `"C"` cgo 导入块单独保留原位不参与合并。
3. **`gofmt` 格式化封装（`src/semantic/actions/formatting.rs`）**：从 `GOROOT/bin/gofmt` 或 `PATH` 查找 `gofmt`，通过 stdin/stdout 异步或短超时管道传输当前文档内容；输出与原文一致时返回空 `Vec<TextEdit>`，有差异时计算公共前后缀缩小替换范围（最小单块编辑），保留原文件换行风格（CRLF 兼容）。
4. **QuickFix 数据驱动（`src/semantic/actions/code_action.rs`）**：不重复跑语义分析，直接读取 Track B 的 `DiagnosticReport` 并校验快照版本/代次一致；候选包自动导入只读复用 `Workspace::navigation_packages()` 检索项目包、已引入依赖与标准库包。

## 实施步骤（Codex 委派规划）

在独立分支上按 Wave 推进（Codex 使用 `modeId: "full-access"`）：

1. **D-WP1 · 冻结 `D → A` 契约：`import_edits` AST 导入引擎**（Wave 0，**与 F-WP1 / B-WP0a 同期完成，立即解锁 Track A**）
   - 产出：`src/semantic/actions/{mod,imports}.rs` 及全面单元测试（分组、排序、注释、cgo、`_`/`.`、CRLF、UTF-16、错误保护）。
2. **D-WP2 · `textDocument/formatting` 与 `source.organizeImports`**（Wave 2，与 Track B 阶段二、Track F 层级并行）
   - 产出：`src/semantic/actions/formatting.rs`、`code_action.rs`（`source.organizeImports`）、`src/dispatch.rs` 薄接线与基础工作区用例。
3. **D-WP3 · `quickfix` 对接 Track B 诊断模型 + GoLand 验收与回归**（Wave 3，依赖 Track B 阶段二产出 `DiagnosticFixData`）
   - 产出：4 类核心 QuickFix、`tools/workspace_checks/actions.py`、`tools/fixtures/actions/*`、`tools/capture_goland_actions.groovy`、`tools/check_goland_actions.py`、`docs/goland-actions-baseline.md` 与全量回归验收。

## 验证计划

沿用环境配置的 Cargo 产物目录与工具链（禁用 `--target-dir` 及覆盖 `CARGO_TARGET_DIR`），所有脚本不含硬编码个人路径：

```powershell
cargo check -p rs-gopls --locked
cargo test -p rs-gopls --locked semantic::actions
cargo check --workspace --locked
cargo test --workspace --locked
cargo build -p rs-gopls --release --locked

$target = (cargo metadata --format-version 1 --no-deps --locked | ConvertFrom-Json).target_directory
$server = Join-Path $target 'release\rs-gopls.exe'
python -B .\tools\check_workspace_updates.py $server --module actions
python -B .\tools\check_workspace_updates.py $server
python -B .\tools\check_goland_actions.py $server --workspace .\tools\fixtures\actions --fixture .\tools\fixtures\actions\goland-actions.json
```

## 完成记录

- 2026-11-27 完成 GoLand `GO-263.7307.354` 格式化与导入优化器字节码静态核查，确定 `D → A` 导入编辑契约及与 Track B / F 的协同节奏，创建待确认计划。
- 2026-11-27 验证 Wave 0 操作夹具与采集脚本。`go test ./helper/...` 通过。GoLand `GO-263.7307.354` 的格式化和导入整理共 4 个用例，各完成 30 次重复计时且结果一致。格式化正文与 Go SDK 的 `gofmt` 一致，导入整理保留两种导入注释与 cgo 前导注释及 `import "C"`。[操作夹具](../../tools/fixtures/actions/) · [采集脚本](../../tools/capture_goland_actions.groovy)
- [x] 2026-11-27 验证 Wave 3 操作工具。格式化、导入整理和 QuickFix 共 9 项通过精确文本校验，导入整理的编辑范围避开 cgo 前导注释和 `import "C"`。使用 GoLand `GO-263.7307.354` 和 Go `1.26.3` 对照两项格式化的首查与热查耗时，rs-gopls 完成 270 次编辑后查询。[操作校验](../../tools/check_goland_actions.py) · [操作基准](../../tools/compare_actions.py)
- 2026-11-27 完成 D-WP1（`4809672`，`import_edits` 19 项单测）、D-WP2+3（`f1e3054`，`formatting` + `code_action`）及 Wave 3 接线与单变量短声明 QuickFix 修复（`a5a8e45`）。发布版 SHA-256 `0A0532A807D96E98D0301E527CA2E082F6D08A356B43AC58A64690B98768C362`，`source.organizeImports` 暖态 P50 `0.1789 ms`、`quickfix` 暖态 P50 `0.3862 ms`、`formatting`（`gofmt`）P50 `106.6592 ms`，主进程峰值工作集 `10.44 MiB`；全工作区 120/120 Rust 测试、32/32 Python 单测、29/29 工作区回归与 9/9 操作严格对照全部通过。归档基线文档 [GoLand 操作基线](../goland-actions-baseline.md)，状态更新为「已完成」。
