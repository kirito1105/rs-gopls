# GoLand 定义跳转基线

基线采集于 2026-11-25。项目为 `localchat/server`，修订为 `532228d3d36bd9f0fffb4bda35c180485b2eadd6`。GoLand 为 2026.3，构建号 `GO-263.7307.354`。项目声明 Go 1.25.0，采集时 GoLand 自动选择 Go 1.27.4 SDK。本组目标只涉及项目代码和已固定版本的 `gorilla/mux`。

[跳转数据](../tools/fixtures/localchat-goland.tsv) 记录了 20 次查询。GoLand 返回 19 个文件位置和 1 个包级导航对象。查询坐标和标识符范围使用从零开始的 UTF-16 行列，范围的结束位置不包含在内。外部依赖位置以 `$GOMODCACHE` 为根。

| 查询 | GoLand 落点 | 上一阶段 rs-gopls |
| --- | --- | --- |
| `internal/app/user.go:79` 的 `u.Sanitize()` | `internal/model/user.go:103` | 空结果 |
| `internal/app/card_action_test.go:28` 的 `tree[0].AttachmentID` | `internal/app/card_action.go:59` | 空结果 |
| `internal/store/sqlitestore/code_review_store_test.go:31` 的 `items[0].Id` | `internal/model/code_review.go:77` | 空结果 |

GoLand 的默认 Ctrl+B 调用 **Go to Declaration or Usages**。同一组查询中，有 19 项与纯声明动作的结果一致，另有 1 项进入查找用法。

- 对 `mux.Router` 的限定符 `mux`，GoLand 给出 `gorilla/mux` 的包导航对象，带包目录和包名。LSP 文件位置的验收单独报告这一项。
- 对 `api.go` 里的 `"github.com/gorilla/mux"` 导入字面量，GoLand 的纯声明动作落在字面量自身，默认 Ctrl+B 进入查找用法。上一阶段 rs-gopls 和 gopls v0.24.0 返回被导入包的包声明。当前 rs-gopls 的纯定义结果与 GoLand 一致。

## 重新采集

用目标构建的 GoLand 打开 `localchat/server`，等待项目索引完成。从 rs-gopls 仓库执行以下命令。脚本在已经打开的 GoLand 进程中调用 **Go to Declaration** 和 **Go to Declaration or Usages** 的目标计算链，记录候选的真实导航请求。它使用只读编辑器视图。

```powershell
$project = 'E:\Code\localchat\server'
$goland = 'E:\Apps\goland-2026.3.0.1.win\bin\goland64.exe'
$capture = (Resolve-Path .\tools\capture_goland_definitions.groovy).Path
$directory = Join-Path $env:TEMP 'rs-gopls-goland-baseline'
New-Item -ItemType Directory -Path $directory -Force | Out-Null
$queries = Get-Content .\tools\fixtures\localchat-goland.tsv -Encoding utf8 | Select-Object -Skip 1 |
    ForEach-Object { $_.Split("`t")[0..4] -join "`t" }
@($project) + @($queries) | Set-Content -LiteralPath (Join-Path $directory 'queries.tsv') -Encoding utf8NoBOM
Start-Process -FilePath $goland -ArgumentList @('ideScript', ('"{0}"' -f $capture)) -WindowStyle Hidden -Wait
Get-Content -LiteralPath (Join-Path $directory 'destinations.tsv') -Encoding utf8
Get-Content -LiteralPath (Join-Path $directory 'ctrl-b.tsv') -Encoding utf8
```

两个报告的首行均为构建号。其余每行包含用例 ID、目标类型、目标路径、起始行列、落点标识符和结束行列。比较时按工作区根或模块缓存根归一化路径。GoLand 的桌面交互和候选展示可按相同查询位置手动核对。[GoLand 导航文档](https://www.jetbrains.com/help/go/navigating-through-the-source-code.html#navigate-between-code-elements)

## 对照当前语言服务器

复用已构建的 rs-gopls 可执行文件，或用 `gopls` 作为第一个参数。GoLand 源码位置按目标 URI 和完整标识符范围比较。

~~~powershell
python -B .\tools\check_goland_definitions.py .\target\release\rs-gopls.exe --workspace 'E:\Code\localchat\server' --allow-differences
~~~

`--json` 输出每个查询的 GoLand 期望、实际候选及分类。默认严格模式用退出码报告缺失或不同目标。基线 rs-gopls `675640b` 的 19 个文件位置中，15 个一致、3 个为空、1 个不同。gopls v0.24.0 有 18 个一致、1 个不同。两个服务器对 GoLand 包级导航用例均返回当前文件的导入位置。

## 比较 gopls 的性能

使用同一工作区和 Go 工具链，比对候选版、上一阶段版与 gopls。默认每个服务运行 5 个独立进程，每轮测 80 次调用方范围编辑及 80 轮依赖文件覆盖与恢复。每次计时都核对 GoLand 的实际定义范围。报告区分首次查询、暖态编辑与依赖覆盖/恢复，并记录超时、错误和 Windows 进程工作集。进程树工作集为周期采样峰值，新进程共享系统和 Go 缓存。

~~~powershell
python -B .\tools\compare_workspace_definitions.py .\target\release\rs-gopls.exe `
    --workspace 'E:\Code\localchat\server' `
    --prior .\target\release\rs-gopls-675640b.exe `
    --json .\target\localchat-definition-performance.json
~~~

`--prior` 指向保留的上一阶段发布版，可用于检查 Rust 自身是否回退。报告包含各二进制哈希、工作区修订、工具链、请求目标和少量具有代表性的文件变更路径。活动 gopls 诊断配置为 `Save`。

### 容器类型阶段的实测

下表候选版为 `53c2eb5`，上一阶段版为 `675640b`。两个工作区均使用 Go 1.25.0、gopls v0.24.0、Windows amd64。三套服务在 `localchat/server` 分别通过 1,205 次精确范围校验。标准库以整个 `GOROOT/src` 为工作区，五个 [net/http 源码锚点](../tools/fixtures/net-http-go1.25.tsv) 按 SDK 源码声明选取，三套服务分别通过 605 次校验。两组实验每个服务各启动 5 次，覆盖 400 次调用方范围编辑。依赖覆盖和恢复在 localchat 各 400 次，在 SDK 各 100 次。

| 工作区和指标 | rs-gopls 候选版 | rs-gopls 上一阶段版 | gopls |
| --- | ---: | ---: | ---: |
| localchat 首次正确跳转 P50 | 556 ms | 544 ms | 1,347 ms |
| localchat 调用方范围编辑 P95 | 0.50 ms | 0.47 ms | 20.32 ms |
| localchat 依赖覆盖 / 恢复 P95 | 0.96 / 2.80 ms | 0.85 / 2.90 ms | 27.16 / 33.77 ms |
| localchat 进程树采样峰值 | 50.4 MiB | 50.1 MiB | 453.1 MiB |
| SDK 首次正确跳转 P50 | 1,532 ms | 1,515 ms | 2,343 ms |
| SDK 调用方范围编辑 P95 | 2.56 ms | 2.51 ms | 106.90 ms |
| SDK 依赖覆盖 / 恢复 P95 | 2.53 / 20.59 ms | 2.82 / 19.58 ms | 115.81 / 150.20 ms |
| SDK 进程树采样峰值 | 86.4 MiB | 85.6 MiB | 565.8 MiB |

调用方范围编辑和首次正确跳转达到了计划设定的回退阈值。两个工作区共有查询的编辑 P95 也达到了比 gopls 低 20% 的目标。这里统计的是定义跳转负载，gopls 使用 `Save` 诊断设置，Go 缓存在进程之间共享。构建图变化和冷 Go 缓存需要独立场景。

重现 SDK 场景时，将 Go 1.25.0 的 `bin` 放在当前进程 `PATH` 首位，并设置 `GOTOOLCHAIN=local`。使用 SDK 的 `src` 作为 `--workspace`，用 `--fixture .\tools\fixtures\net-http-go1.25.tsv` 指定样本。通过五次 `--case` 指定文件里的用例 ID，另设 `--dependency-iterations 20`。

### 函数值的真实项目用例

同一 GoLand 构建另外验证了 `internal/api/docmost_test.go` 中 `call("wrong").Code`，以及 `internal/app/svn_feature_tag_admin.go` 中 `slices.Contains` 的声明跳转。两个 GoLand 动作在这两个位置均返回相同目标。参照位置见[回调与泛型函数用例](../tools/fixtures/localchat-goland-calls.tsv)。

这些目标来自 SDK 包，按各自的 `GOROOT` 解析文件。Go 1.27.4 与 Go 1.25.0 在本组标识符上的 UTF-16 范围一致。容器版本在回调用例上返回空结果，加入函数值支持后两个用例均通过。

### 保存后的包图变化

单独用临时 Go Modules 工作区交替保存两种 `go.mod` 依赖替换，经过动态监听通知后请求同一个跨包符号。每一轮计量从保存到首次正确落点的时间，并统计途中的旧落点、空结果和错误。

~~~powershell
python -B .\tools\compare_graph_refresh.py .\target\release\rs-gopls.exe --sessions 5 --iterations 10 --json .\target\final-graph-refresh.json
~~~

## 第二阶段发布验收

最终发布源代码为 `9afa8ec`。`localchat/server` 上的两份 GoLand 基线合计 21 个文件位置全部命中，包级导航对象单独记录。合成语义套件通过 124 个位置，原有真实项目套件通过 16 个位置。7 组工作区用例涵盖 Go Modules、`go.work`、GOPATH、测试包、后台刷新、未保存新文件及关闭缓冲区后的磁盘位置。Rust 检查、测试、Clippy 和 Zed 扩展的 Wasm 检查均通过。

| 工作负载 | rs-gopls | 上一阶段 rs-gopls | gopls v0.24.0 |
| --- | ---: | ---: | ---: |
| localchat 首次正确跳转 P50 | 570 ms | 557 ms | 1,365 ms |
| localchat 原有路径范围编辑 P95 | 0.50 ms | 0.47 ms | 20.31 ms |
| localchat 新增容器与回调路径范围编辑 P95 | 0.36 ms | — | 35.40 ms |
| Go SDK 范围编辑 P95 | 2.47 ms | 2.28 ms | 90.40 ms |
| Go SDK 关闭依赖缓冲区后 P95 | 3.77 ms | 18.23 ms | 138.44 ms |
| 临时模块保存 go.mod 后首次正确跳转 P95 | 69.38 ms | — | 303.82 ms |

原有 localchat 场景用最终发布版重跑。三套服务各通过 505 次精确范围校验，其中每套 400 次调用方范围编辑。新增 localchat 和 SDK 的完整对照使用缓存修复提交 `7d53361`，分别每套通过 605 次精确校验。包图场景两套服务各通过 50 次保存后的精确跳转，途中无旧落点、空结果或错误。后来提交的 `9afa8ec` 增加了内部测试包对未保存新文件的验证，最终发布版重新通过所有正确性套件。

两套服务使用同一 Go 1.25.0 工具链，gopls 采用 `Save` 诊断配置，五次独立会话共用系统和 Go 缓存。启动中位数和原有路径 P95 均在相对上一阶段的阈值内。关闭 SDK 依赖缓冲区的尾延迟在缓存修复前曾达到 19.36 ms，修复后以同样的依赖覆盖与恢复场景复测。

Zed 1.29.3 使用隔离配置打开 `localchat/server`，窗口为 `server — api.go`。进程父子关系确认该窗口的语言服务器为 `rs-gopls-cd5d4b5ee1da4f03.exe`，SHA-256 为 `CD5D4B5EE1DA4F037DD4C0CE3AECC0502067A4D71B706D614FA499820C6B9418`，进程来自同一发布构建。跳转范围由上述 stdio 套件验收。
