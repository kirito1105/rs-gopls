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
