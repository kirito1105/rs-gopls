# Track C · Go 安全重命名

| 项目 | 内容 |
| --- | --- |
| 状态 | 已完成 |
| 创建日期 | 2026-11-27 |
| 用户确认 | 2026-11-27 已确认执行 |
| 更新日期 | 2026-11-27 |
| 完成日期 | 2026-11-27 |

## 目标与范围

编辑器从 Go 声明或用法发起重命名时，先选中光标处的标识符，再返回整个活动工作区内可以一并应用的文本编辑。支持局部绑定（含参数、接收者与类型参数）、包级类型、函数、常量、变量、具名字段、导入别名和方法。跨文件查询覆盖本项目的活动包、测试视图和未保存缓冲区。文件、目录、包路径迁移及注释、字符串和结构体标签中的文本重写超出本线范围。[协作指南](../COLLABORATION.md#track-c--安全重命名)

2026-11-27 已确认以下两项设计。

- **方法重命名保留接口契约。** 对独立方法直接改名。方法与接口关联时，将保持现有实现关系所需的接口、实现声明和用法一并改名。遇到只读依赖、冲突或无法证明完整的关系，整个操作返回具体原因。GoLand 可通过弹窗选择范围，LSP 的标准 rename 请求只有光标和新名称。
- **只向能接收版本化编辑的客户端声明重命名。** 客户端支持 `workspace.workspaceEdit.documentChanges` 时启用。再依据 `textDocument.rename.prepareSupport` 分别返回带 `prepareProvider` 的能力或布尔值。客户端直接发起 rename 也执行完整校验。[LSP 重命名协议](https://github.com/microsoft/language-server-protocol/blob/gh-pages/_specifications/lsp/3.17/language/rename.md)

## 验收标准

- [x] **光标与名称。** 从声明或引用调用 `textDocument/prepareRename`，返回当前文件里准确的 UTF-16 `range` 和旧拼写 `placeholder`。空白、注释、包路径、空标识符、预声明实体、工作区外声明和不可唯一解析的目标返回空。直接 rename 遇到无效目标或新名返回可读错误。覆盖关键字、单个下划线、含空格、数字开头、合法非 ASCII 字母和包含非 BMP 字符的行。原名返回空编辑。按声明身份区分预声明名与合法遮蔽它的用户声明。[Go 标识符与关键字](https://go.dev/ref/spec#Identifiers)
- [x] **编辑完整且精确。** 覆盖闭包、同块 `:=` 重声明、跨文件包级符号、导入方和内外部测试包。区分局部变量作 map 键与结构体键名，核对具名字段的声明、选择器和字面量键。显式导入别名只修改本文件相应绑定，隐式导入名改名时插入别名并保留路径。验证同名无关符号、字符串、标签和注释逐字节保留。
- [x] **改名后绑定安全。** 拒绝同作用域重声明、目标被内层同名变量捕获、改名目标捕获原有同名用法、跨包引用因变为非导出名而失效，以及成员提升引起的歧义。包含语法合法且仍能编译、但引用绑定改变的反例。对匿名嵌入同时充当类型用法与字段声明的情况，只有两种身份及其用法都验证成功才交付编辑。失败时不交付部分文件。
- [x] **方法集。** 从接口和具体方法两端执行，核对完整且一致的契约编辑。覆盖值与指针接收者、嵌入接口及提升方法、同名不同签名、多个接口和只读依赖。独立方法成功，无法安全改动的接口族返回原因，无关类型的同名方法原样保留。
- [x] **快照和协议。** 成功响应使用 `WorkspaceEdit.documentChanges`，每个 URI 一组互不重叠的 UTF-16 编辑，已打开文件的 `version` 等于取样时版本，未打开文件为 `null`。核对未保存修改、新建草稿、CRLF 和编辑跨越非 BMP 字符的情况。包图等待、分批验证、磁盘文件变化和取消期间，每个请求只回复一次。显式取消返回 `-32800`，已使用的输入过期返回 `-32801`。新请求能按最新状态重算。
- [x] **行为对照与回归。** 在同一套可重置 Go 夹具上采集运行中 GoLand 的实际修改及冲突，逐文件核对完整结果并记录 IDE 交互选择。对成功样本应用 LSP 编辑再运行 Go 测试。Rust 检查和受影响回归通过，结果记入本计划。

## 实现依据

2026-11-27 从用户提供的 `tmp/goland-2026.3.0.1.win.zip` 读取 `build.txt` 和 `product-info.json`，得到 GoLand 2026.3.0.1、构建 `GO-263.7307.354`。下列注册位于嵌套的 `plugins/go-plugin/lib/modules/intellij.go.impl.jar!/intellij.go.impl.xml`。已用 `javap -p -c` 检查该 JAR 的类；JAR 的 SHA-256 为 `2a13f1a753d5e15db70c5758e17db1dc28cc790e1d329e574d8b6a078d636784`。

| GoLand 安装包中核实的调用 | 对应 Rust 行为 |
| --- | --- |
| `com.goide.refactor.rename.GoRenameInputValidator` 的模式通过 `GoPackageUtil.isGoPackage` 匹配包目标，`isInputValid` 委托给 `com.goide.refactor.GoNamesValidator.isIdentifier`。后者另行注册为 Go 的语言名称校验器，比较 Go lexer token 与 `GoTypes.IDENTIFIER`。 | 用 Go 标识符词法检查整个新名字，再按声明种类处理空标识符及具有语言特殊含义的目标。导入别名和包重命名分别判定。 |
| `GoRenamePsiElementProcessor.findExistingNameConflicts` 先调用 `GoRefactoringUtil.findDuplicateConflicts`，其中使用 `GoDuplicatesSearch.processDuplicates`；再调用 `findVisibilityConflicts`。对有全局使用范围的公开元素改为非大写开头时，后者搜索本包以外的引用。 | 按作用域和声明身份验证重声明与跨包访问，并复核可能被捕获的既有用法。 |
| `GoRenameMethodProcessor.canProcessElement` 接收 `GoMethodDeclaration` 和 `GoMethodSpec`。交互式 `substituteElementToRename` 使用 `GoChangeSignatureHandler.findMethodSpec` 和范围选择；`prepareRenaming` 在联动模式中对方法规格调用 `GoInheritorsSearch.METHOD_INHERITORS_SEARCH`，筛选可修改文件。`findReferences` 与 `findExistingNameConflicts` 对选中范围继续搜集用法和检查冲突。 | 复用方法集计算，区分签名匹配、确实无关和信息不足。只读契约无法保留时拒绝整次编辑。在 IDE 采集时记录选择项，以验证成功样本与拒绝行为的差异。 |

当前 Rust 基线为 `b47c4d1`，两项解耦已合入。`Engine::references` 可按声明路径和范围分批筛选用法，`Resolver::reference_location` 可处理短声明的绑定。`references.rs` 的局部快路径仅收集 `ReferenceQuery::Name`，而 `source.rs` 将 `map[int]int{k: 0}` 中的 `k` 标为 `LiteralKey`，需要单独验证。`implementation/method_sets.rs` 当前会将方法集不完整和确定不匹配都表示为 `None`。这两个入口都需要能表达证据是否完整，才能用于生成编辑。[引用扫描](../../src/semantic/references.rs) · [声明解析](../../src/semantic/resolver.rs) · [方法集](../../src/semantic/implementation/method_sets.rs)

从具体方法进入时，`GoChangeSignatureHandler.findMethodSpec` 通过 `GoSuperMethodSearch` 取首个结果。联动搜索随后调用 `GoTypeUtil.isImplementsInterface`，候选用法再以 `PsiReference.isReferenceTo` 核对身份。在双契约样本中，从 `Both.Click` 进入的实测选中了 `First.Click`，改名后 `Both` 失去 `Second` 契约。Rust 同时保留两份契约。[实机对照](../goland-rename-baseline.md#实机对照结果)

## 实施步骤

1. 在 `tools/fixtures/rename/` 建立可复制的多包工作区和按光标指定的用例。用同一构建的 GoLand 在独立副本采集重命名前后文件和冲突。将采集入口、索引就绪条件、方法联动选择和重放命令记入 `docs/goland-rename-baseline.md`。
2. 在 `src/semantic/rename/tests.rs` 先写失败用例，再在同目录实现目标与新名验证。通过 Facade 读取文档、声明和活动包图，并从作用域和引用扫描取得声明身份。先跑通局部变量和跨包声明，再覆盖字面量键、显式和隐式导入、字段及泛型绑定。
3. 用只在请求内生效的候选文本应用拟议编辑。将原始声明位置映射到修改后位置，对所有受影响的旧名和新名用法重新解析并核对绑定。对照 `var y int` 与函数内的 `x` 改名为 `y`，以及内层 `y` 捕获外层 `x` 两个方向。临时结果与正式编辑态、磁盘索引和导航缓存隔离。
4. 经窄接口复用 `implementation/method_sets`，从两端收集受影响的方法和接口，核对变更前后的方法集关系。用已确认不匹配、多个契约相交、无法解析和只读契约分别验证。重复和环状关系按声明身份去重。
5. 经共享 Owner 薄接线在 `src/dispatch.rs` 注册两个请求，按初始化客户端能力声明支持。复用调度器的包图等待、分批推进和取消。提交结果前核对所用包图、缓冲区版本与内容、关闭文件的磁盘输入。使用稳定顺序编码编辑。新增的协议测试由 `tools/workspace_checks/rename.py` 自动注册。
6. 重放 IDE 用例和下列回归，回填实际命令、样本数、差异与原因。在全部验收项通过后更新状态和完成日期。

## 验证计划

- Rust 沿用已配置的 Cargo 目录和工具链，依次运行 `cargo check -p rs-gopls --locked`、`cargo test -p rs-gopls --locked semantic::rename`、`cargo test -p rs-gopls --locked` 和 `cargo build -p rs-gopls --release --locked`。
- 用 `cargo metadata --no-deps --format-version 1` 返回的 `target_directory` 定位发布版。运行 `python tools/check_workspace_updates.py <发布版> --module rename` 和 `python tools/check_workspace_updates.py <发布版>`，通过 stdio 核对能力协商、错误码、版本、编辑和取消。修改共享引用或调度行为时，同时重放受影响的导航正确性与性能基线。[导航性能对照](../PLANS.md#导航性能对照)
- 在每个新建夹具副本上应用返回的完整编辑，核对预期文件、文本和声明身份，运行 `go test ./...`。冲突样本核对无编辑和拒绝原因。GoLand 实测重点核对局部遮蔽、跨包可见性、隐式导入别名和多接口方法联动，保存 IDE 选择及完整差异。

## 完成记录

2026-11-27 用户确认执行。两个请求已接入 stdio。重命名在独立语义快照中应用候选编辑，逐处核对映射后的声明位置，校验接口契约和接收者已有的方法，再返回版本化编辑。隐式导入别名通过插入声明实现。嵌入类型同时带有字段身份时，类型用法、字段选择器和字面量键在同一次请求中核对。无法证明安全的操作向客户端说明原因。

审查中复现并加入了三组回归条件。其一，type switch 和 select 分支中的短声明可能复用分支变量，漏改会让已取得的变量地址观察到不同值。其二，缺少文件监听时，新加入的导入方也必须参与搜索；重命名所用包图增加了 Go 输入文件核验。其三，字段名会影响按位置构造、结构类型等价和提升方法，改名前需单独检查。[作用域](../../src/scope.rs) · [包图输入](../../src/workspace/inputs.rs) · [重命名测试](../../src/semantic/rename/tests.rs)

GoLand `GO-263.7307.354` 在同一个临时工作区完成 12 例，每例都从校验过的 7 个文件开始，采集后恢复原始字节。5 个普通改名的文件结果与 LSP 完全相同，跨包可见性冲突也一致。其余 6 例分别展示了隐式导入的包级与文件级作用域、2 次遮蔽和 3 次多接口联动。方法联动选择通过 GoLand 的回调传入。[GoLand 基线与重放命令](../goland-rename-baseline.md) · [完整 IDE 差异](../../tools/fixtures/rename/goland-rename.json)

### 验证结果

2026-11-27 在 Windows amd64 上以 Go 1.27.0 和 Cargo 1.97.1 验证。发布版 SHA-256 为 `9a78f81f80b489c8a340dceff2ecaba60c6ebfa83d9eb440de63028b29c5a4b6`。

| 验证 | 实际结果 |
| --- | --- |
| `cargo fmt --all -- --check`、`cargo check -p rs-gopls --locked` | 均通过，Cargo check 零告警。 |
| `cargo test -p rs-gopls --locked`、`cargo build -p rs-gopls --release --locked` | 96 个 Rust 测试通过，发布版构建成功。MSVC 打印“正在创建库…和对象…”，rustc 将其报告为一条 `linker_messages` 告警；在同一台主机重建原版也出现相同提示。 |
| `python -B -m unittest discover -s tools -p 'test_*.py'` | 26 个 Python 测试全部通过。 |
| `python -B tools/check_workspace_updates.py <发布版>` | 40/40，包括 17 项重命名验收和全部 23 项历史回归。覆盖 12 个 GoLand 共用样本的 LSP 一侧，9 个成功样本应用编辑后逐文件核对并通过 Go 测试，另 3 个正确拒绝。 |
| 历史导航语料 | 原版和本发布版均精确匹配 29/29 个实现目标集与 24/24 个类型定义目标集，核对 URI 和 UTF-16 范围。 |

### 历史实现导航性能回归

同机从 `b47c4d17778080245fadee311701c03ccca196e2` 重建原版（SHA-256 `4d0f54bfba5d15331ba205f2bba7add36888450a5e4a3af0ff97aa9a61d0a15a`），与本发布版共用固定的 `implementation-navigation` 工作区、6 个查询、编辑顺序及包缓存。空闲主机上交错启动 12 对进程，各有 6 次先行。下表每个结果按“P50 / P95”记录，单位为毫秒。启动测至首个正确的非空结果，编辑后耗时包含 `didChange`。

| 查询 | 每版样本数 | 原版 | 本发布版 |
| --- | ---: | ---: | ---: |
| 启动至首个正确结果 | 12 | 186.815 / 191.336 | 189.266 / 208.408 |
| 索引就绪后首次查询新光标 | 72 | 0.390 / 0.514 | 0.378 / 0.521 |
| 同一光标复查 | 2160 | 0.219 / 0.329 | 0.233 / 0.345 |
| 编辑后查询 | 720 | 0.321 / 0.448 | 0.319 / 0.437 |

首轮 5 对进程与工作区验证同时运行，冷启动 P95 从 196.823 ms 升至 220.247 ms，新光标 P95 从 0.490 ms 升至 0.648 ms。空闲复测的新光标 P95 差值为 0.006 ms；两轮启动 P95 均对应该轮最慢启动。两轮原始样本和正确性明细位于 `%TEMP%\rs-gopls-track-c-baseline-irowejsn\` 下的 `navigation-first-final.json`、`navigation-quiet-final.json` 和 `checks.json`。

### 与运行中 GoLand 的导航对照

两端在同一主机、Go 1.27.0、相同夹具字节和相同打开文件上，执行 6 个实现或反向接口查询。每个查询都核对完整目标文件和 UTF-16 标识符范围，包括预热、未保存编辑和恢复后的目标。两端各 228 次，逐次结果一致。GoLand 从发起索引就绪的 PSI 查询计到取完范围，rs-gopls 计 stdio 请求往返。编辑后耗时从提交未保存的修改后开始计。[查询清单、环境、目标范围及原始耗时](../../tools/fixtures/rename/goland-implementation-timings.json)

| 场景 | 每端样本数 | GoLand P50 / P95（ms） | rs-gopls P50 / P95（ms） |
| --- | ---: | ---: | ---: |
| 索引就绪后首次查询新光标 | 6 | 10.657 / 665.707 | 0.720 / 973.905 |
| 同一光标预热 5 次后复查 | 180 | 3.561 / 7.561 | 0.207 / 0.318 |
| 未保存编辑后的首次查询 | 6 | 7.617 / 21.198 | 0.260 / 0.409 |

首查只有 6 个光标，最近秩 P95 是最慢的一个。GoLand 的最大值来自 `First` 正向查询，rs-gopls 的最大值来自 `Wrapped.Extra` 反向查询。

### 与上游多根工作区的集成

提交 `3bfc962` 完成上述单根实测。与上游 `d7e08e2` 集成后，重命名预览继承全部工作区根，包图加载前核对各根的模块配置，增删工作区根会使等待中的请求失效。新增协议样本从第二个根的声明改名，核对两个模块的编辑，再移除声明所在的根，确认后续请求识别只读依赖。[跨工作区改名用例](../../tools/workspace_checks/rename.py)

两个独立模块可以有相同的导入路径。实测中，第二个模块的生产代码定义了私有方法，同包测试文件定义了对应接口。修复前只改到了生产代码，使用方测试编译失败。改用包图中对应的生产包身份后，重放得到本模块的两处编辑，另一个模块的同名方法保留原样，应用编辑后 Go 测试通过。[方法契约回归测试](../../src/semantic/rename/methods.rs)
