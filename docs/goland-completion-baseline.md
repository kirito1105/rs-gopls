# GoLand 上下文代码补全基线

2026-11-26 在 Windows 11 Pro `10.0.26300`（AMD Ryzen 7 255，8 核 16 线程）上采集 GoLand `GO-263.7307.354` 的 BASIC 补全。合成工作区是 `tools/fixtures/completion` 的 `example.test/completion`，共 126 组查询、3,122 条候选和 26 份插入后文档。真实项目使用 `$env:RS_GOPLS_BENCH_WORKSPACE` 的 `532228d3d36bd9f0fffb4bda35c180485b2eadd6`，共 22 组查询、1,073 条候选和 1 份插入后文档。[Track A 计划](plans/2026-11-26-go-completion-context.md) · [合成语料](../tools/fixtures/completion/goland-completion.json) · [真实项目语料](../tools/fixtures/completion/external-goland-completion.json)

- IDE 与命令行验证使用 `go1.27.4 windows/amd64`，SDK 根为 `<GOROOT>`。两项目启用 Go Modules 集成，`go.mod` 均声明 `go 1.25.0`。IDE 采用默认构建目标和 cgo 自动检测（`UNSURE`），自定义构建标签为空。命令行 `GOOS=windows`、`GOARCH=amd64`、`CGO_ENABLED=1`，`GOWORK` 和 `GOFLAGS` 为空。合成模块带本地 `replace` 和对应的 vendor 文件，Go 命令验证使用 `-mod=vendor`。
- 查询清单以项目相对路径、零基 UTF-16 行列和实际光标行固定上下文。缓冲区编辑按给定顺序应用，每步校验 `expectedText`。合成项目的初始编辑页为空，真实项目恢复 `internal/api/api.go`。采集器为每个查询阶段创建一个关联真实文件的独立编辑器。通过 `ideScript` 调用时启动器给本轮分配了独立进程，两项目共享同一个运行中的 GoLand JVM。[合成查询](../tools/fixtures/completion/completion-queries.json) · [真实项目查询](../tools/fixtures/completion/external-completion-queries.json)
- `CodeCompletionHandlerBase` 以调用次数 1 执行显式 BASIC 补全。EDT 等待后台贡献者全部完成，后台使用生产 `CompletionParameters`。脚本在 `completionFinished` 取排序后的 `LookupImpl.items`、预选下标、优先级和可用的 PSI 来源，通过候选的延迟渲染器采集显示文本和过时删除线。用 lookup 的前缀长度和初始化上下文的标识符终点记录 `prefixRange` 和 `replacementRange`，格式均为 `[起始行, 起始列, 结束行, 结束列]`。JSON 的来源字段为 `null` 时表示该候选没有提供相应数据。平台入口来自安装包 `lib/intellij.platform.lang.impl.jar` 的字节码（REA 产物记录 `ev_0f68e2c379dad456d286dc369987fd766ebe332baf579f8742ac8e74b1e5a1af`）。[采集脚本](../tools/capture_goland_completion.groovy)
- 插入探针用 `LookupImpl.finishLookup` 执行 Enter，`literal-mid-key` 使用 Tab。输出保留选中后的完整编辑器文本和光标。IDE 的 Document 使用 LF，`lineEnding` 保留磁盘行尾记录。两项目每次编辑期间都禁用撤销记录并阻止保存，退出阶段重载并提交原文，核对缓冲区、磁盘字节和文件修改时间。

## 同名遮蔽

样本统一使用 `append`，通过候选 PSI、类型和点号后的成员区分预声明函数、同包变量、`strings` 别名和局部变量。

| 条件 | 实测首选与绑定 |
| --- | --- |
| 没有声明遮蔽 | `shadow-builtin` 的同名首选是 `builtin.append`，优先级 40。 |
| 同包 `append` 存在 | `shadow-package` 首选类型为 `PackageAppend` 的包级变量，优先级 40，点号后给出 `PackageField`。 |
| 文件导入 `append "strings"` | `shadow-import` 首选导入别名，优先级 39，点号后给出 `strings` 的导出成员。 |
| 局部与包级或别名重名 | 两个局部选择器样本均给出 `LocalField`。同名包级变量存在时，无限定前缀 `app` 的首查选中包级 `PackageAppend`，复查出现局部 `struct {...}`。局部变量遮蔽 `strings` 别名时，无限定前缀首查选中该别名，复查也出现局部变量。 |
| 同文件导入与包级同名的错误编辑态 | `shadow-import-package-error` 和 `shadow-local-import-package-error` 的无限定首查均选中 `strings` 别名。后一个样本复查出现包级 `PackageAppend`。选择器分别回到包级 `PackageField` 和局部 `LocalField`。 |

有效 Go 代码中的同名绑定由词法作用域确定。上述无限定候选展示和选择器解析出现分歧，错误导入声明也没有统一的首选优先链。[Go 规范的声明与作用域](https://go.dev/ref/spec#Declarations_and_scope)

## 提升成员与字面量

- `selector-ambiguous-field` 与 `selector-ambiguous-method` 每次各给出 1 个同名项。两轮采集的首查及复查中观察到 `Right.Shared string`、`Right.Collision() string` 与 `Left.Shared int`、`Left.Collision() int` 的切换。同 SDK 的编译器对两个实际选择器均报 `ambiguous selector`。Go 要求最浅层的同名成员唯一。[Go 规范的选择器](https://go.dev/ref/spec#Selectors)
- `literal-promoted` 选中 `LeftOnly` 得到 `embedded.Outer{Basic: 1, LeftOnly: }`，光标落在冒号后的空格之后。指针嵌入的 `literal-promoted-pointer` 返回 0 项。已填 `Left` 的 `literal-filled-embedded` 仅返回剩余的顶层 `Right`。在夹具声明的 Go 1.25 语言模式下，补全后的 `LeftOnly` 填入值后仍需改成嵌套字面量才可编译。用同一 SDK 对临时 overlay 运行 `go test -vet=off -gcflags=example.test/completion=-lang=go1.27`，直接的 `LeftOnly: 1` 编译通过，`RightOnly: 2` 报 `invalid implicit pointer indirection`。[Go 规范的复合字面量](https://go.dev/ref/spec#Composite_literals)
- `literal-keyed` 插入 `Field: `，`literal-partial-key` 把 `Fi: 1` 补成 `Field: 1`，Tab 的 `literal-mid-key` 保留原来的冒号和值。函数样本 `selector-call-parens` 生成 `helper.NewRecord()`，`selector-call-existing-parens` 复用 `foo.LeftMethod()` 已输入的括号。

## 导入插入与别名

| 样本 | 接收补全后的文档 |
| --- | --- |
| `auto-single-comment` | 原来的单项导入转成分组。前导注释仍在 `import (` 上方，显式别名 `formatting` 保留，反引号路径变成双引号 `"fmt"`。原 `fmt` 行尾注释移到分组闭括号行。新增 `"encoding/json"` 排在 `formatting "fmt"` 前。 |
| `import-raw-aliased` | 路径补全只把反引号内的内容换成 `encoding/json`。原别名、反引号和行尾注释保留。 |
| `auto-existing-block` | 新导入进第一个普通块，排在 `"fmt" // Standard library.` 前。两个块的前导注释与第二块的 `_ "image/png"`、`. "math"` 均保留。 |
| `auto-cgo`、`auto-cgo-block` | cgo 的前导注释仍紧邻独立的 `import "C"`。已有普通块时新项进该块。只有 cgo 时新增独立的 `import "encoding/json"`，原 `import "C"` 行尾注释随新行移动。 |
| `auto-crlf-utf16` | `json.Mar` 的前缀范围是 `[5,36,5,44]`，同行的两个 emoji 各占两个 UTF-16 单元。接收后两处 emoji、CJK 字符和注释保留，磁盘 CRLF 与原始内容一致。 |
| `auto-existing-reuse` | 无限定位 `Mar` 首选已经导入的 `j.Marshal`，权重 35。新导入的 `xml.Marshal` 和 `asn1.Marshal` 权重 5。接收后直接用 `j.Marshal()`，原导入块无文本变化。 |
| `auto-local-alias-collision`、`auto-import-alias-collision`、`auto-double-alias-collision` | 局部变量或已有导入占用 `conflict` 时生成 `conflict2`。两个名字均占用时生成 `conflict3`。导入别名与调用限定符同步写入。 |
| `auto-versioned`、`auto-go-prefix`、`auto-odd-path` | `pkg/versioned/v2` 用 `versioned`，`pkg/go-util` 用 `util`，两者用裸路径导入。路径 `pkg/odd-name` 写入显式 `oddname` 别名。 |

## 导入路径与包可见性

| 样本 | 实际候选 |
| --- | --- |
| 生产包与 `p_test` | `import-production-self` 的 11 项中没有 `example.test/completion`。同前缀在 `import-external-test-self` 返回 12 项，生产包是首选。外部测试的 `production.` 给出 `ProductionOnly`、`SiblingPublic`，也给出来自内部测试的 `InternalTestOnly` 和 `TestInternal`。外部测试通过临时 overlay 调用 `production.InternalTestOnly()`，`go test` 通过。限定补全过滤 `siblingPrivate`，无限定前缀 `SiblingP` 会建议已导入的 `production.SiblingPublic`。移除生产包导入后可接受 `completion.ProductionOnly`，编辑实际加入 `"example.test/completion"`。 |
| `internal` | 在 `pkg/sibling_caller` 内，路径补全命中 `example.test/completion/pkg/internal/secret`，自动导入命中 `secret.InternalWitness`。在 `pkg` 树之外的 `imports_edit`，两个对应查询均为 0 项。 |
| vendor | `import-vendor` 返回规范路径 `example.test/vendordep/visible`，候选目录来自本地 `replace`。`import-vendor-physical` 返回 0 项。已有路径上的 `vendor-qualified` 解析到 `vendor/` 副本。自动导入同时列出本地 `replace` 与 vendor 文件的 `visible.VendorWitness`，选中 vendor 项写入规范路径。 |
| SDK `testdata` | `go/build/testdata/other/` 和 `go/build/te` 都返回 0 项。`file.F` 自动导入出现 `path/filepath` 的模糊候选，SDK 的 `go/build/testdata/other/file.F` 未出现。 |
| 项目目录 | `cmd/app` 的路径与其独有导出符号查询均为 0 项。`pkg/testdata/sample` 在路径和自动导入中均出现，路径列表还含 `pkg/_hidden` 与 `pkg/.hidden`。单独的未闭合导入字符串在本次 BASIC 调用中返回 0 项，有闭合引号的工作区路径前缀返回 10 项。 |

## 其他上下文样本

- EOF、换行、右括号和下一行语句前的 `foo.` 首查各有 16 项，包含提升成员与后缀模板。普通注释中的 `Mo` 给出同文档的词 `Moon`，普通字符串和数字后的查询均为 0 项。
- `**Outer` 的选择器给出 `Basic` 等成员，`*Interface` 样本只有后缀模板。实例化的 `Box[string]` 返回 `Value` 和 `Get`，两者展示类型均为 `T`。`helper.Legacy` 的 `deprecated` 为 `true`。
- `iota` 只出现在常量样本，普通文件的 `InternalTestOnly` 查询为 0 项。闭包内的 `goto` 只给出 `InnerPoint`。同目录测试文件的包子句首选 `completion_test`，单文件目录回退到 `sibling_caller`。

## 索引就绪后的延迟

计时前等待项目初始化活动完成，并连续 3 秒确认智能模式、根目录与索引代次稳定。首查按清单顺序各执行一次。复查在同一已提交缓冲区上预热 5 次，再采样 30 次。编辑后首查在缓冲区开头插入一行注释和一行空白并提交 PSI，按行偏移重定位原光标。计时从调用补全到取得排序候选、呈现和范围，涵盖 IDE 内部候选处理与排序。界面弹窗绘制、文档编辑、接收补全和 JSON 写出在计时区间之外。

每次调用重新执行 BASIC 补全。IDE 复用同会话索引、SDK 索引、PSI 和内部缓存。复查及编辑后查询核对完整候选、顺序、预选项和 UTF-16 范围。复查的变化结果、原因和采样下标记入 `repeatVariants`，编辑后变化记入 `afterEditResult`。分位数按最近秩计算。[导航性能对照](PLANS.md#导航性能对照)

| 工作区 | 场景 | 有效样本数 | P50（ms） | P95（ms） |
| --- | --- | ---: | ---: | ---: |
| 合成 | 索引就绪首查 | 126 | 87.9019 | 228.1814 |
| 合成 | 复查 | 3,678 | 48.6848 | 107.5989 |
| 合成 | 编辑后首查 | 123 | 58.5118 | 119.7102 |
| 真实项目 | 索引就绪首查 | 22 | 63.3988 | 920.2987 |
| 真实项目 | 复查 | 660 | 34.3005 | 537.3292 |
| 真实项目 | 编辑后首查 | 22 | 51.2288 | 560.9514 |

合成夹具的 3,780 次复查中有 102 次结果变化，分布在 20 个查询。126 次编辑后首查中有 3 次变化，涉及 `selector-newline`、`selector-before-paren`、`selector-ambiguous-field`。选择器的变化来自 `Shared` 或 `Collision` 的声明切换，作用域和字面量也观察到候选变化。原始计时文件保留变动样本及其实际结果。真实项目的复查和编辑后首查均与首查一致。两套工作区均未观察到候选集合相同但顺序或预选项改变的复查。

## rs-gopls 对照结果与性能基准

2026-11-27 在同一 Windows 主机重跑两版 rs-gopls，使用 2026-11-26 从运行中的 GoLand `GO-263.7307.354` 采集的原始计时。rs-gopls 固定 `go1.27.4 windows/amd64`、`GOWORK=off`、空 `GOFLAGS` 和 `CGO_ENABLED=1`，沿用同一工作区修订、打开文件和编辑序列。[冷延迟计划](plans/2026-11-26-go-completion-cold-latency.md) · [正确性检查](../tools/check_goland_completion.py) · [性能对照](../tools/compare_completion.py)

| rs-gopls 发布版 | SHA-256 |
| --- | --- |
| `1c42031` 基线 | `7effac3491b38afebdee30f4d38beeea37143d30ee4e5b6e79ca85238bfad3bf` |
| P0+P1 | `38752603876b8834d7a8372d6e759158bf3950a7ab21b94200f99f7f77fcbfe5` |

### 正确性

检查器逐项核对候选名称、种类、来源、过时标记、顺序及 UTF-16 插入和替换范围，校验泛型代入，并对有选中动作的样本应用主编辑和附加编辑。合成夹具和真实项目分别成功应用 26 和 1 次选中操作。工作区补全检查 5/5 通过，覆盖未保存缓冲区、能力协商、跨会话顺序、关闭恢复、包图刷新、过期解析及取消。

| 工作区 | 回放通过 | 所有核对字段一致 | 有意差异查询 | 未解释失败 |
| --- | ---: | ---: | ---: | ---: |
| 真实项目 | 22/22 | 13 | 9 | 0 |
| 合成夹具 | 126/126 | 58 | 68 | 0 |

两版 rs-gopls 的逐查询判定相同。真实项目的 22 份补全和 1 份 resolve 响应、合成夹具的 126 份补全和 26 份 resolve 响应均逐字节一致，接受补全后的文本也一致。

### 差异依据

| 行为 | GoLand 实测与 rs-gopls 规则 |
| --- | --- |
| 作用域与选择器 | 遮蔽样本按实际声明落点核对。rs-gopls 采用词法作用域，过滤同深度歧义、双重指针及不符合方法集和可寻址性规则的项。GoLand 的泛型展示仍保留 `T`，rs-gopls 的 `Value`、`Get`、`Set` 使用实际类型 `string`。`continue` 只接受外层循环标签。 |
| 显式测试导入与自动导入 | `external-qualified-production` 与 `external-qualified-internal-test` 已直接对齐，双方按 Go 测试编译包图展示 `InternalTestOnly`。自动导入则使用生产声明。SDK、工作区与活动包图中的可达包都参与搜索，过滤项目 `testdata`、点目录、下划线目录和 vendor/replace 重复声明。真实项目另有 137 条 GoLand 候选来自活动 `go list -e -deps -test ./...` 集合之外的依赖包。 |
| 不完整或带键字面量 | `literal-open-brace` 中 rs-gopls 恢复 `A`、`Field` 和后续四个声明。明确的键位置只建议未用的键，`local-literal` 保留 `app`、过滤已用的 `Routes`。`literal-mid-key` 与 `local-edit-literal` 中 GoLand 另给出值和自动导入建议。Go 1.25 的提升字段以合法的嵌套字面量插入，指针嵌入只提供可直接填写的顶层字段。 |
| 限定名与别名 | GoLand 的 `GoAutoImportCompletionContributor` 从项目别名索引补入本文件之外的 `append` 和 `j`。rs-gopls 使用当前文件导入和光标处可见绑定。`local-edit-type` 的 `var user user.User` 由同 SDK 编译通过。GoLand 写作 `user2.User`。对 `u¦.Id` 等已有选择器后缀的输入，rs-gopls 过滤会把单个限定名扩成 `导入包.成员` 的候选，保留光标后的 `.Id`。GoLand 在部分无限定查询里也展示 `receiver.Value`、`u.NotifyProps` 等展开候选。 |
| 导入应用 | 合成夹具和真实项目的未闭合路径按实际引号范围补齐右引号。单项转组保留原反引号和行尾注释，独立 cgo 导入保持前导及行尾注释。`auto-crlf-utf16` 实际应用后保留 CRLF、两个 emoji 和中文。 |
| 匹配与排序 | 安装包的 `GoAutoImportCompletionContributor` 同时以限定名和导出名执行 `isStartMatch`。rs-gopls 保留词首、驼峰及限定匹配，并以权重、包图使用频次、模块层次及稳定的自然名称顺序决胜。合成 42、真实项目 5 个查询的共同候选仍有相对顺序变化，报告记录两侧序列。`keyword-range` 的 `range` 在两侧均排首位，rs-gopls 还返回 41 个匹配 `ra` 的 `debug/elf` 导出常量，已用同 SDK 编译核对。 |
| IDE 专有项 | 后缀和实时模板、注释单词、填充字段、方法生成与表达式模板在 GoLand 报告中可见，位于 Track A 范围之外。 |

匹配入口的安装包证据包括 `intellij.go.impl.jar` 中的 `GoAutoImportCompletionContributor#collectAndSortAllPublicProductionNames`、`GoCompletionUtil#createPrefixMatcher`，以及 `intellij.platform.util.text.matching.jar` 中的 `MinusculeMatcherImpl`（REA `ev_ff027ed2bb7112044eb6c1e7cc8240784c7a247f1ef673f8350805fff90e1438`）。同一 SDK 的临时编译探针通过了 `debug/elf` 常量与 `os/user` 的声明内别名，未写入真实项目。

### 共同样本延迟

双方每个会话在索引就绪后依次首查，预热 5 次后复查 30 次，再提交相同的未保存首行注释和空行并首查。GoLand 计时包含候选遍历、排序、呈现和范围采集，弹窗绘制、接收补全和缓冲区编辑在计时之外。rs-gopls 通过 `workspace/symbol` 等待含测试视图的包图就绪，从发送 JSON-RPC 请求计至 stdio 响应完整解码。P0+P1 在后台预热 SDK 清单和导出名，按需索引命中的声明。两侧每次独立发起补全，复用各自索引及语义缓存。

| 工作区 | 场景 | 共同 / 总样本数 | 差异样本数 | GoLand P50 / P95（ms） | `1c42031` P50 / P95（ms） | P0+P1 P50 / P95（ms） |
| --- | --- | ---: | ---: | ---: | ---: | ---: |
| 真实项目 | 首查 | 13 / 22 | 9 | 43.3941 / 446.7385 | 3.0743 / 24.4822 | 3.0224 / 13.2341 |
| 真实项目 | 复查 | 390 / 660 | 270 | 29.2823 / 123.5895 | 0.5112 / 15.4531 | 0.3289 / 10.5048 |
| 真实项目 | 编辑后首查 | 13 / 22 | 9 | 36.0754 / 139.9207 | 0.9153 / 15.1580 | 0.8157 / 12.4818 |
| 合成夹具 | 首查 | 58 / 126 | 68 | 78.7436 / 139.6909 | 1.2631 / 7.1502 | 1.2855 / 4.6605 |
| 合成夹具 | 复查 | 1,740 / 3,780 | 2,040 | 31.3772 / 95.6739 | 0.6051 / 3.6261 | 0.5360 / 2.6349 |
| 合成夹具 | 编辑后首查 | 58 / 126 | 68 | 32.4451 / 106.7794 | 0.6673 / 3.6799 | 0.6852 / 2.4879 |

四份首轮报告的校验失败数均为 0。共同及差异样本的查询和采样下标在两版之间一致，复测也保持相同。合成夹具首查 P50 三轮均上升，编辑后首查 P50 在三轮中的两轮上升。真实项目编辑后首查 P50 在反序复测中由 0.6856 ms 升至 0.7918 ms。上述补全 P50 升幅及合成夹具首查后共同 resolve P95 的升幅约为 +0.02–0.14 ms，均在用户确认的 ±0.15 ms 容差内。同组补全 P95、真实项目首轮及全部解析样本 P95 均改善。全量性能验收通过，P0+P1 计划状态为「已完成」。[逐轮结果](plans/2026-11-26-go-completion-cold-latency.md#wave-b-验收记录)

### 补全项解析

`completionItem/resolve` 单独计 stdio 往返，覆盖选中目标延迟提供的 `detail`、`documentation` 和 `additionalTextEdits`，包含必要的目标加载。解析在补全计时之外，加载结果可能影响后续查询。GoLand 的呈现在 BASIC 计时内，采集未测量对应独立解析请求。

| 工作区 | 场景 | 共同 / 通过解析的样本数 | `1c42031` 共同 P50 / P95（ms） | P0+P1 共同 P50 / P95（ms） | `1c42031` 全部 P50 / P95（ms） | P0+P1 全部 P50 / P95（ms） |
| --- | --- | ---: | ---: | ---: | ---: | ---: |
| 真实项目 | 首查后 | 0 / 1 | — | — | 0.8055 / 0.8055 | 0.3972 / 0.3972 |
| 真实项目 | 复查后 | 0 / 30 | — | — | 1.0632 / 1.2074 | 0.2959 / 0.3925 |
| 真实项目 | 编辑后 | 0 / 1 | — | — | 1.0833 / 1.0833 | 0.3267 / 0.3267 |
| 合成夹具 | 首查后 | 12 / 26 | 0.2192 / 0.2501 | 0.2086 / 0.2811 | 0.2297 / 0.9877 | 0.2248 / 0.3158 |
| 合成夹具 | 复查后 | 360 / 780 | 0.2151 / 0.3214 | 0.2025 / 0.3357 | 0.2499 / 1.0568 | 0.2460 / 0.3753 |
| 合成夹具 | 编辑后 | 12 / 26 | 0.2264 / 0.3439 | 0.2036 / 0.3031 | 0.2517 / 1.0421 | 0.2313 / 0.3528 |

合成夹具首查后共同 resolve 的 P95 三轮均上升，复查后 P95 只在首轮上升。全部解析样本的尾延迟在首轮均下降。

### 冷态尾延迟与诊断

两版各启动 5 个独立进程，等待包图后按原顺序查询，每个进程随后复查同一缓冲区 3 次。保留正常系统和 Go 磁盘缓存。GoLand 一列为该查询的原始索引就绪首查和同会话 30 次复查。所有数值为 ms。

| 样本 | GoLand 首查 | GoLand 复查 P50 / P95 | `1c42031` 冷态 P50 / P95 | P0+P1 冷态 P50 / P95 | `1c42031` 复查 P50 / P95 | P0+P1 复查 P50 / P95 |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| 合成 `scope-local` | 113.3461 | 62.2025 / 80.4628 | 635.4500 / 646.4124 | 9.7820 / 10.8237 | 2.4528 / 3.7793 | 2.1103 / 3.0896 |
| 真实项目 `local-inferred` | 551.3285 | 155.4033 / 172.8605 | 1,696.4282 / 1,852.0193 | 432.2427 / 460.6832 | 17.5027 / 22.6334 | 11.7927 / 14.1462 |

全量基准首轮中，`scope-local` 在两版分别为 632.4269 和 10.5919 ms，两侧各给出 2 项。`local-inferred` 分别为 1,657.6378 和 420.4965 ms。两版 Rust 均给出 154 项，GoLand 给出 284 项，131 条既有差异明细保持一致。

同一 P0+P1 二进制另启 5 个进程进行诊断。两例的首查均命中后台已建立的 SDK 清单。合成例在请求开始搜索前 27–41 ms 就绪，真实项目为 444–514 ms。每次后台扫描预热了 1,629 份 SDK 文件的导出名。下表的计数在同一例的五次首查中一致。

| 样本 | 诊断冷态 P50 / P95（ms） | 诊断复查 P50 / P95（ms） | 读盘 / 二读 / 三读及以上 | 预筛选 / 工作区及 vendor / 交接文本 | 完整 / 声明解析 | 类型位置 / 预期权重命中 | 峰值源数 / 字节 | 释放 |
| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| `scope-local` | 10.0031 / 11.7274 | 2.3972 / 3.2574 | 18 / 0 / 0 | 17 / 17 / 0 | 0 / 1 | 0 / 0 | 1 / 22,628 | 5/5 |
| `local-inferred` | 402.7284 / 410.5462 | 12.5678 / 15.7552 | 574 / 6 / 0 | 518 / 87 / 35 | 17 / 76 | 279 / 21 | 8 / 1,520,368 | 5/5 |

已有 `FileIndex` 的展示源二次读取两例均为 0。真实项目基线冷态的候选展开曾额外全量解析 396 次，64 份已有索引的文件重开，52 份出现第三次读取。优化版冷态全部 17 次完整解析属于本次请求的索引工作。SDK 的有效 `GOROOT` 在后台探测后保存，包图也从 `Goroot` 包目录提取根路径。已有清单供首查直接使用。诊断的 115 个请求均报告释放，完成、取消与失效后的源释放另由 Rust 测试核实。[阶段与释放记录](plans/2026-11-26-go-completion-cold-latency.md#wave-b-验收记录)

Cargo 产物目录中以 `external-` 和 `fixture-` 开头的 `*-completion-correctness-cold-latency.json`、`*-completion-benchmark-1c42031.json`、`*-completion-benchmark-cold-latency.json` 保留逐项结果。冷态与诊断见 `wave-b-cold-timing-1c42031.json`、`wave-b-timing.json` 和 `wave-b-diagnostic.json`。GoLand 原始计时 SHA-256 分别为 `4ba9d4b68e437e7c43fccc3aaddc3d687b8a4ad4d9664a4185924155460b70e5`（真实项目）与 `5b46b541a2942af755a4cd35691e24560d57cebe2774c8881584b48b950b0874`（合成）。

## 重放

在仓库根目录使用 PowerShell 7。两份清单合并写入 IDE 脚本读取的 `completion-queries.json`。用 `GOLAND_EXE` 指定 GoLand，用 `RS_GOPLS_BENCH_WORKSPACE` 指定已信任的真实项目根目录。脚本可为合成夹具创建项目模块。

~~~powershell
$sampleDir = (Resolve-Path '.\tools\fixtures\completion').Path
$baselineDir = Join-Path $env:TEMP 'rs-gopls-goland-baseline'
$goland = $env:GOLAND_EXE
$script = (Resolve-Path '.\tools\capture_goland_completion.groovy').Path
$spec = Get-Content -Raw (Join-Path $sampleDir 'completion-queries.json') | ConvertFrom-Json -AsHashtable
$local = Get-Content -Raw (Join-Path $sampleDir 'external-completion-queries.json') | ConvertFrom-Json -AsHashtable
$server = (Resolve-Path -LiteralPath $env:RS_GOPLS_BENCH_WORKSPACE).Path
$local.projects[0].root = $server
if ((git -C $server rev-parse HEAD) -ne $local.projects[0].revision) { throw 'Unexpected benchmark workspace revision' }
if (git -C $server status --short --untracked-files=all -- .) { throw 'Benchmark workspace is dirty' }
New-Item -ItemType Directory -Path $baselineDir -Force | Out-Null
$spec.projects[0].root = $sampleDir
$spec.projects += $local.projects
$spec.benchmark = $true
$spec | ConvertTo-Json -Depth 15 | Set-Content -Encoding utf8 (Join-Path $baselineDir 'completion-queries.json')
$env:RS_GOPLS_BENCHMARK = '1'
$startedAt = [DateTime]::UtcNow
Start-Process -FilePath $goland -ArgumentList @('ideScript', ('"{0}"' -f $script)) -WindowStyle Hidden

$outputs = @('fixture-completion.json', 'external-completion.json',
             'fixture-completion-bench.json', 'external-completion-bench.json')
do {
    $failure = Get-Item -LiteralPath (Join-Path $baselineDir 'completion-capture-error.txt') -ErrorAction SilentlyContinue
    if ($failure -and $failure.LastWriteTimeUtc -ge $startedAt) { throw (Get-Content -Raw $failure.FullName) }
    $pending = $outputs | Where-Object {
        $result = Get-Item -LiteralPath (Join-Path $baselineDir $_) -ErrorAction SilentlyContinue
        !$result -or $result.LastWriteTimeUtc -lt $startedAt
    }
    if ($pending) { Start-Sleep -Milliseconds 250 }
} while ($pending -and [DateTime]::UtcNow -lt $startedAt.AddMinutes(15))
if ($pending) { throw 'Capture timed out. See completion-progress.log.' }
if (git -C $server status --short --untracked-files=all -- .) { throw 'Benchmark workspace changed during capture' }
Copy-Item -LiteralPath (Join-Path $baselineDir 'fixture-completion.json') -Destination (Join-Path $sampleDir 'goland-completion.json')
Copy-Item -LiteralPath (Join-Path $baselineDir 'external-completion.json') -Destination (Join-Path $sampleDir 'external-goland-completion.json')
~~~

计时原始数据保留在 `%TEMP%\rs-gopls-goland-baseline\fixture-completion-bench.json` 和 `%TEMP%\rs-gopls-goland-baseline\external-completion-bench.json`。夹具使用相同 SDK 通过 `go list -mod=vendor -test ./...` 和 `go test -mod=vendor ./...`。采集前后，真实项目的 `git status --short -- .` 均为空，父级仓库的已有状态逐项一致。
