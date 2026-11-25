# GoLand 接口与实现跳转基线

从 GoLand 构建 `GO-263.7307.354` 中打开的 `E:\Code\localchat\server` 采集。项目修订为 `532228d3d36bd9f0fffb4bda35c180485b2eadd6`，SDK 为 Go 1.27.4。[第四阶段计划](plans/2026-11-25-go-implementation.md) · [项目查询语料](../tools/fixtures/localchat-goland-implementations.json)

## 真实项目实测

同一光标位置分别查询 **Go to Implementation** 和 **Go to Super**。20 个查询记录了两个动作的 108 个目标。语料的 `action` 为 LSP 验收选择对应的动作，另一个动作的结果也保留在语料中。

| 查询 | GoLand 返回的目标 |
| --- | --- |
| `Store` 类型声明 | 39 个类型，包含 SQL 和 SQLite 实现、嵌入实现的包装类型及测试实现。 |
| `UserStore` 类型声明 | 10 个类型，包含两个数据库实现和测试替身。 |
| `UserStore.GetAll` 声明与调用 | 声明处获得 6 个实现方法。调用处另包含 `UserStore.GetAll` 声明。 |
| `SessionStore` 类型声明与 `Store.Session` 的返回类型 | 声明处获得 4 个实现类型。返回类型位置另包含 `SessionStore` 声明。 |
| `FileBackend` 类型声明 | 唯一目标为 `internal/filestore/local.go` 中的 `LocalFileBackend`。 |
| `SqlUserStore.GetAll` 声明 | **Go to Implementation** 为空，**Go to Super** 跳到 `UserStore.GetAll`。 |
| `internal/store/store.go` 的包名 | 两个动作都为空。 |

三个具体类型的反查结果还包含 `golang.org/x/tools@v0.43.0/go/analysis/passes/httpresponse/testdata/src/typeparams` 中的 `C[P any]`。Go 1.27.4 的 `go list -m` 包含该模块，`go list -deps -test -e ./...` 的活动包图没有这个 `testdata` 包。语料保留这些 GoLand 目标并用 `exclusions` 标出原因。LSP 对照按计划采用活动包图，并要求其余目标与标识符范围完全匹配。

## 补充模块实测

在同一 GoLand 构建中打开 [补充模块](../tools/fixtures/implementation-navigation)，设置 Go 1.27.4 后采集 29 个查询和两个动作的 95 个目标。[补充查询语料](../tools/fixtures/implementation-navigation/goland-implementations.json)

- `Reader` 的类型候选包含值与指针接收者、提升实现、类型别名和扩展接口。两个同深度嵌入导致的歧义和同名异签名被排除。方法查询将多个提升来源归并到实际声明。
- 跨包嵌入 `Secret` 的类型出现在正向类型查询。直接声明同名私有方法的外包类型没有匹配。具体类型反查遵循 GoLand 观察到的私有方法范围。
- 泛型方法的用法定位到泛型声明。`NumericConvert` 的类型查询结合类型参数约束，包含整数转换器和可实例化的泛型转换器。空接口及无实现的接口给出空结果。

补充模块可复制到临时工作区，在 GoLand 中配置其 Go SDK 后，用同一脚本重放。将下方的项目和语料路径改为补充模块对应的路径。

## 重新采集

在指定 GoLand 中打开项目并等待索引完成。脚本在 IDE 进程的智能模式运行搜索，使用 **Go to Implementation** 的完整候选查询及 **Go to Super** 的 Go 搜索处理器。它记录目标标识符范围和 IDE 导航偏移。

```powershell
$project = 'E:\Code\localchat\server'
$goland = 'E:\Apps\goland-2026.3.0.1.win\bin\goland64.exe'
$directory = Join-Path $env:TEMP 'rs-gopls-goland-baseline'
New-Item -ItemType Directory -Force -Path $directory | Out-Null
$fixture = Get-Content -LiteralPath .\tools\fixtures\localchat-goland-implementations.json -Raw | ConvertFrom-Json
$queries = $fixture.cases | ForEach-Object {
    @($_.id, $_.query.path, $_.query.line, $_.query.character, $_.query.token) -join [char]9
}
@($project) + @($queries) | Set-Content -LiteralPath (Join-Path $directory 'implementation-queries.tsv') -Encoding utf8NoBOM
$script = (Resolve-Path .\tools\capture_goland_implementations.groovy).Path
Start-Process -FilePath $goland -ArgumentList @('ideScript', ('"{0}"' -f $script)) -WindowStyle Hidden -Wait
Get-Content -LiteralPath (Join-Path $directory 'implementation-locations.tsv') -Encoding utf8
```

首行为 GoLand 构建号。后续各行为动作、查询 ID、目标路径、标识符起止行列、源码记号和导航行列，空结果用 `none` 表示。行列使用零起点 UTF-16，依赖和 SDK 路径分别归一化为 `GOMODCACHE:` 与 `GOROOT:`。

## 正向查询验收

发布构建通过真实项目的 12 个 **Go to Implementation** 对照，按集合比较 UTF-16 标识符范围。包图加载和扫描期间的取消、未保存缓冲区覆盖及包图更新也已验证。声明跳转、类型定义和引用的既有真实项目检查继续通过。

```powershell
python -B .\tools\check_goland_implementations.py $server --workspace $project --action implementation
```

## 双向查询与回归

真实项目的 20 个查询和补充模块的 29 个查询通过发布版 stdio 验收。运行 `cargo check -p rs-gopls --locked`、`cargo test -p rs-gopls --locked` 和 `cargo build -p rs-gopls --release --locked` 后，用 Cargo 配置的产物目录取得 `$server` 并运行以下检查。

```powershell
python -B .\tools\check_goland_implementations.py $server --workspace $project
$fixtureRoot = '.\tools\fixtures\implementation-navigation'
python -B .\tools\check_goland_implementations.py $server --workspace $fixtureRoot --fixture (Join-Path $fixtureRoot 'goland-implementations.json')
python -B .\tools\check_workspace_updates.py $server
```

15 组工作区回归通过，包含签名编辑、关闭后恢复、未保存新实现、磁盘构建条件变化、待加载取消和分批搜索取消。已有 124 个合成声明位置、16 个项目锚点、21 个 GoLand 声明对照、58 个 GoLand 类型定义对照及 34 组引用结果通过。[第三阶段验收记录](plans/2026-11-25-go-navigation-phase-three.md)

## 性能对照

同一 Windows 主机使用 Go 1.27.4、相同的 `localchat/server` 修订、Go 与系统缓存、打开文件和请求顺序。gopls v0.24.0 使用 `Save` 诊断配置，两套服务交替运行，各测 5 个独立会话。每个会话记录首次正确的 `Store` 查询，打开并预热共同样本后记录 80 次暖态查询，再记录 80 次未保存的顶层注释插入或删除及查询。每次都比较完整目标范围。

gopls 对真实项目 20 例中的 15 例给出相同结果。4 例的接口声明是否出现在候选中有差异，包名查询返回 RPC 错误。15 个共同样本进入性能测量，每套服务的 805 次响应均匹配。

| 指标 | rs-gopls P50 / P95 | gopls v0.24.0 P50 / P95 |
| --- | ---: | ---: |
| 首次正确响应，5 次 | 3904.31 / 4018.89 ms | 1085.25 / 1114.43 ms |
| 正向暖态，200 次 | 0.21 / 0.53 ms | 0.79 / 1.23 ms |
| 反向暖态，200 次 | 0.17 / 0.22 ms | 0.62 / 0.85 ms |
| 正向注释编辑后，200 次 | 0.39 / 0.80 ms | 8.46 / 28.61 ms |
| 反向注释编辑后，200 次 | 0.34 / 0.47 ms | 31.91 / 53.13 ms |
| 进程树峰值工作集，中位 / 最大 | 64.3 / 67.8 MiB | 470.6 / 483.2 MiB |

正向和反向暖态 P95 分别减少 57% 和 74%，达到主要路径目标。首次查询仍需要加载包图并扫描候选，P50 约为 gopls 的 3.6 倍。未保存的顶层普通注释和空行改动可重映射缓存坐标。签名、导入等语义改动触发重新搜索，这类改动由功能回归验证。

```powershell
$target = (cargo metadata --format-version 1 --no-deps --locked | ConvertFrom-Json).target_directory
$server = Join-Path $target 'release\rs-gopls.exe'
python -B .\tools\compare_workspace_implementations.py $server --workspace $project --sessions 5 --iterations 80 --json (Join-Path $target 'implementation-performance.json')
```

### 已有导航的同轮性能回归

与第三阶段发布版 `be7641d` 的版本化二进制对比，声明和类型定义每套各通过 505 次精确响应，引用每套各通过 405 次精确响应。范围编辑后各路径的 P95 回退都在计划上限内。

| 范围编辑后 P95 | 第四阶段 | 第三阶段 |
| --- | ---: | ---: |
| 声明跳转 | 0.58 ms | 0.53 ms |
| 类型定义跳转 | 0.56 ms | 0.51 ms |
| 文件内引用 | 0.68 ms | 0.62 ms |
| 同包引用 | 3.56 ms | 3.67 ms |
| 跨包引用 | 3.65 ms | 3.40 ms |

逐请求结果保存在 Cargo 产物目录的 `implementation-performance.json`、`phase4-definition-performance.json`、`phase4-typeDefinition-performance.json` 和 `phase4-references-performance.json`。

## Zed 加载与现场验收

Zed 1.29.3 已用仓库隔离配置打开 `localchat/server` 的 `filestore.go`。进程检查确认 Zed 启动了 `rs-gopls-8d4f0a4bad3f4f96.exe`，与正式测速的发布版 SHA-256 `8D4F0A4BAD3F4F96C2D01869BC14DDCAFEFE457E77FF8560D9B5B7EFB97A9927` 一致。发布版的 stdio 初始化声明了 `implementationProvider`，全部 GoLand 对照已通过。

以下位置可在当前隔离窗口用 **Go to Implementation**（`Ctrl+F12`）重放。行列从 1 开始。Windows UI 自动化的启动错误 `CreateProcessWithLogonW 1920` 阻塞了编辑器动作观察，实际结果待补记。

| 查询位置 | 预期界面行为 |
| --- | --- |
| `internal/filestore/filestore.go:11:7` | 跳到 `internal/filestore/local.go:10:6` 的 `LocalFileBackend`。 |
| `internal/store/store.go:38:3` | 列出 `UserStore.GetAll` 的 6 个实现声明。 |
| `internal/store/sqlstore/user_store.go:91:25` | 跳到 `internal/store/store.go:38:2` 的 `UserStore.GetAll`。 |
| `internal/store/store.go:1:10` | 没有跳转目标。 |

现场同时抽查 **Go to Definition**、**Go to Type Definition** 和 **Find All References**。恢复 UI 自动化或取得现场结果后，核对各项输出并完成计划状态。
