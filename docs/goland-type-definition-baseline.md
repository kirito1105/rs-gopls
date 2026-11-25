# GoLand 类型定义基线

采集自 GoLand 2026.3 构建 `GO-263.7307.354` 中打开的 `localchat/server`，项目修订 `532228d3d36bd9f0fffb4bda35c180485b2eadd6`，IDE 选择 Go 1.27.4。对照数据包含 34 个查询，其中 32 个有文件落点，导入限定符和包名声明的 2 个查询为空。[类型定义语料](../tools/fixtures/localchat-goland-types.tsv)

| 查询 | GoLand 类型定义 |
| --- | --- |
| `api.app` 上的字段名 | `internal/app/app.go` 的 `App` |
| `session` 和 `appErr` | 分别到 `Session` 和 `AppError` 的类型声明 |
| `GetSession` 方法名 | 多返回值签名开头的 `(` |
| `Store` 和 `Session` 方法名 | 分别到 `Store` 和 `SessionStore` 接口声明 |
| `users`、`sessions` | 源签名中指针切片类型开头的 `[` |
| `msgs` 和 `Params` | 源声明的 `map` |
| `call` 回调变量 | 函数字面量的 `func` |
| `slices.Compact` 函数名 | 声明里的类型参数 `S` |
| `execPath, _ := os.Executable()` 的 `_` | SDK 中的 `error` |

查询与目标范围均使用零起点 UTF-16。对于未命名的类型，目标是 GoLand 实际停下的源码词法记号。原始采集也给出声明及默认 Ctrl+B 动作，这两种动作的落点可用同一组光标比较。

## 重新采集

打开固定修订的 `localchat/server`，等 GoLand 完成索引。按既有采集步骤，使用 `tools/fixtures/localchat-goland-types.tsv` 的前五列生成 `queries.tsv`，再调用仓库里的 `tools/capture_goland_definitions.groovy`。脚本在 IDE 进程中执行类型声明动作使用的 `GttdKt.gotoTypeDeclaration`，并生成 `type-declarations.tsv`、`destinations.tsv` 和 `ctrl-b.tsv`。[已有采集命令](goland-definition-baseline.md#重新采集)

SDK 的目标在 Go 的不同版本上可能移动。严格比较已固定的 SDK 范围时使用 Go 1.27.4，并令客户端和语言服务器通过同一 `PATH` 选择它。普通项目仍可使用自己的工具链。

## 检查与对照

在本机选择与 GoLand 基线相同的 Go 1.27.4 工具链，再运行类型定义检查或性能对照。保留 `--allow-differences` 仅用于记录对照服务的差异。候选发布版用默认严格模式验收。

~~~powershell
$sdk = Join-Path $env:USERPROFILE 'go\pkg\mod\golang.org\toolchain@v0.0.1-go1.27.4.windows-amd64'
$env:PATH = (Join-Path $sdk 'bin') + ';' + $env:PATH
$env:GOTOOLCHAIN = 'local'
$workspace = 'E:\Code\localchat\server'
$server = '.\target\release\rs-gopls.exe'
python -B .\tools\check_goland_definitions.py $server --workspace $workspace --method typeDefinition
python -B .\tools\compare_workspace_definitions.py $server --workspace $workspace --method typeDefinition --dependency-iterations 20 --json .\target\type-definition-performance.json
~~~

固定 SDK 后，gopls v0.24.0 的 34 个对照结果中，24 个与 GoLand 落点一致，5 个目标有差异，5 个请求返回错误。共同性能样本使用 8 个已经核对范围的局部、接口、成员及跨包位置。

## 补充类型案例

[补充 Go 模块](../tools/fixtures/type-navigation)的 24 个位置通过同一 GoLand 构建中的只读内存 Go 文件采集。它确认了别名和命名容器仍跳到各自的类型声明，`[]Value` 落在 `Value`，而 `[]*Value` 落在切片的 `[`。泛型字段名本身跳到声明里的类型参数，由该字段推断出的变量跳到实例化类型。还覆盖函数类型、局部类型及内置标量推断。打开补充模块后也可通过相同的 IDE 动作重放这些位置。

~~~powershell
python -B .\tools\check_goland_definitions.py .\target\release\rs-gopls.exe --workspace .\tools\fixtures\type-navigation --fixture .\tools\fixtures\type-navigation\goland-types.tsv --method typeDefinition
~~~

## 类型定义发布验收

发布版代码为 `ceae273`。真实项目通过 34/34，其中 32 个精确文件范围与两个预期空结果。补充 GoLand 语料通过 24/24。原有 21 个 GoLand 声明位置、124 个合成声明位置、16 个项目锚点及 8 组工作区回归均通过。Zed 隔离窗口 `server — api.go` 加载了 `rs-gopls-cb673e3c5c9109bd.exe`。发布二进制的 stdio 初始化同时声明 `definitionProvider` 和 `typeDefinitionProvider`。

在同一台 Windows 机器上固定 Go 1.27.4，轮换两套服务的运行次序。每套服务有 5 个独立会话、400 次范围编辑、100 次依赖覆盖和 100 次磁盘恢复，共 605 次精确结果。新进程复用 Go 和系统缓存，gopls 诊断配置为 `Save`。

| 类型定义指标 | rs-gopls | gopls v0.24.0 |
| --- | ---: | ---: |
| 首次正确结果 P50 | 425.37 ms | 1067.46 ms |
| 调用方编辑后 P95 | 0.53 ms | 21.08 ms |
| 依赖覆盖后 P95 | 1.36 ms | 39.10 ms |
| 关闭编辑器缓冲区后 P95 | 0.48 ms | 44.71 ms |
| 采样进程树峰值工作集 | 36.8 MiB | 461.0 MiB |

原声明跳转在相同 Go 1.27.4 环境下重跑。新增类型功能后的调用方编辑 P95 为 0.53 ms，同轮第二阶段发布版为 0.51 ms，仍在计划的回退上限内。
