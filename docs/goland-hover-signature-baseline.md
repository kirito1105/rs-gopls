# GoLand Hover 与 SignatureHelp 基线

从 GoLand 构建 `GO-263.7307.354` 采集，包含真实项目 `E:\Code\localchat\server`（修订 `532228d3d36bd9f0fffb4bda35c180485b2eadd6`，Go SDK `1.27.4`，位于 `C:/Users/rick/go/pkg/mod/golang.org/toolchain@v0.0.1-go1.27.4.windows-amd64`）与补充模块 [hover-signature](../tools/fixtures/hover-signature)。[开发计划](plans/2026-11-26-go-hover-and-signature-help.md) · [真实项目语料](../tools/fixtures/localchat-goland-hover-signature.json) · [补充模块语料](../tools/fixtures/hover-signature/goland-hover-signature.json)

## 真实项目实测（34 组）

在 `localchat/server` 上采集 21 组 Hover（`GoDocumentationProvider`）与 13 组 ParameterInfo（`GoParameterInfoHandler`）：

| 类别 | 典型样本 | GoLand 实测观察 |
| --- | --- | --- |
| 接口与文档 | `FileBackend`、`FavoriteStore` | 展示完整 `type Name interface { ... }` 与紧邻上方注释（含中文注释 `"收藏 / 稍后读"`）。 |
| 函数与方法 | `NewFileBackend`、`User.PreSave`、`UserStore.GetAll` | 展示接收者/参数名/返回类型与 GoDoc；接口方法以 `func (UserStore) GetAll(offset int, limit int, includeBots bool) ([]*model.User, error)` 展开分组参数。 |
| 结构体与字段 | `User`、`User.Username`、`Routes.APIRoot` | 结构体展示字段与 struct tag；字段悬停展示字段名、类型、struct tag、所属类型及同行尾注释（如 `/api/v1`）。 |
| 变量与常量 | `ErrReviewItemsNotFound`、`UserPasswordMaxLength`、`now`、`user` | 展示 `var`/`const`、推断类型与初始化表达式（如 `var now int64 = GetMillis()`、`const UserPasswordMaxLength int = 72`）；函数参数展示 `var user *model.User`。 |
| 导入、标准库与内置 | `"net/http"`、`strings.TrimSpace`、`append` | 导入路径展示包名与 `doc.go`/包注释；标准库与内置函数展示完整签名及 SDK 文档。 |
| 空白与非符号 | `filestore.go:1:0` | 返回空结果（`null`）。 |
| 调用参数提示 | `GetAll`、`model.NewAppError`、`fmt.Errorf`、嵌套 `TrimSpace(ToLower(u.Email))`、`append`、`make`、`api.requireAccessibleDoc` | 分组参数按单参数展开并高亮当前位；嵌套调用取最内层调用；变参支持第 2+ 实参定位；调用外返回空结果。 |

## 补充模块实测（33 组）

在 [hover-signature](../tools/fixtures/hover-signature) 上采集 14 组 Hover 与 19 组 ParameterInfo：

- **指令过滤与分组注释**：`helper.Map` 上方的 `//go:noinline` 指令被过滤，仅保留 `Map transforms each element using fn.`；分组常量 `StatusError` 继承外层 `const (...)` 注释；结构体字段 `Item.ID` 与 `Item.Name` 分别保留上方注释和同行尾注释及 struct tag；类型别名 `IntBox` 展示 `type IntBox = Box[int]` 与其注释。
- **泛型与替换**：`helper.Map([]int{1, 2}, ...)` 的参数提示替换类型实参为 `items []int, fn func(int) string`；`box.Set(42)` 替换为 `v int`。
- **分组展开与变参钳位**：`Add(a, b int, label string)` 展开为 `a int, b int, label string`（3 个参数位）；`helper.Join("-", "a", "b", "c")` 在第 4 个实参位高亮最后一个变参 `parts ...string`；`f.Format("pre", 1, 2)`、`make([]int, 0, 4)`、`append(nums, 1, 2, 3)` 同理。
- **方法表达式与函数值**：`(*helper.Box[int]).Set(&box, 99)` 将接收者作为第 0 个参数，方法形参作为第 1 个参数；回调变量 `cb(3, "tag")` 与具名函数类型 `op(7)` 均解析底层函数签名。
- **字面量边界阻断**：光标位于 `func(v int) string { ... }` 内部或复合字面量 `helper.Item{Name: "beta"}` 内部时，不穿透显示外层调用参数提示。

## 重新采集

```powershell
$goland = 'E:\Apps\goland-2026.3.0.1.win\bin\goland64.exe'
$script = (Resolve-Path .\tools\capture_goland_hover_and_signature.groovy).Path
# 将项目根目录与制表符分隔查询列表写入 $env:TEMP\rs-gopls-goland-baseline\hover-signature-queries.tsv
# 然后执行：
Start-Process -FilePath $goland -ArgumentList @('ideScript', ('"{0}"' -f $script)) -WindowStyle Hidden
```

设置环境变量 `RS_GOPLS_BENCHMARK=1` 可按 [导航性能对照](PLANS.md#导航性能对照) 执行 5 次预热 + 30 次计时采样。

## 与 GoLand 直接对照

2026-11-26 在同一 Windows 主机、固定 `localchat/server` 修订（`532228d3d36bd9f0fffb4bda35c180485b2eadd6`）与统一 Go SDK（`go1.27.4`）下，对照 GoLand `GO-263.7307.354` 与 rs-gopls 发布版（提交 `70b8f0a`，SHA-256 `6D60E9786239FCDA8FEE1424787888C8055A3211C002A5F11C58660AD82782F1`）。

- **共同样本**：Hover 18 组、SignatureHelp 13 组。三项 GoLand 采集存在差异的 Hover 样本单列不计入共同耗时（`hover-pkg-qualifier` 在 GoLand 采集返回空、rs-gopls 返回 `package model`；`hover-import-path` 在 GoLand 采集落点偏向父包 `net`、rs-gopls 返回完整 `"net/http"`；`hover-embedded-field-decl` 在 GoLand 展示结构体声明、rs-gopls 展示嵌入字段声明）。
- **计时边界与缓存状态**：GoLand 在已索引项目智能模式内计时，分别记录各查询在预热前的首次执行（`firstMs`）及 5 次预热后 30 次重复查询（不含界面弹窗绘制）；rs-gopls 计时覆盖完整 stdio JSON-RPC LSP 请求往返，每轮均校验签名、GoDoc、UTF-16 范围/参数偏移与激活参数位，启动耗时从创建进程计至首次跨包正确 Hover 响应，5 次独立会话重复查询每例 30 次，编辑后查询在打开缓冲区交替插入/删除顶层注释并校验位移后的结果。

| 场景 | 样本数（GoLand / rs-gopls） | GoLand 已索引 P50 / P95（ms） | rs-gopls 发布版 P50 / P95（ms） |
| --- | ---: | ---: | ---: |
| Hover 索引就绪后新查询 | 18 / 85 | 84.467 / 1262.683 | 0.163 / 48.893 |
| Hover 重复同一查询 | 540 / 2700 | 1.728 / 48.548 | 0.107 / 0.198 |
| Hover 顶层注释编辑后 | — / 600 | — | 0.127 / 0.286 |
| SignatureHelp 索引就绪后新查询 | 13 / 65 | 1.267 / 315.074 | 0.190 / 22.339 |
| SignatureHelp 重复同一查询 | 390 / 1950 | 0.096 / 0.177 | 0.127 / 0.258 |
| SignatureHelp 顶层注释编辑后 | — / 600 | — | 0.126 / 0.205 |
| 启动到首次跨包正确 Hover | — / 5 | — | 414.502 / 427.602 |

代表性单样本 P50 对照：

| 样本 | GoLand 首次 / 重复（ms） | rs-gopls 新查 / 重复（ms） |
| --- | ---: | ---: |
| `hover-interface-decl` | 1262.68 / 5.69 | 0.286 / 0.104 |
| `hover-func-decl-doc` | 225.44 / 32.52 | 0.140 / 0.100 |
| `hover-cross-pkg-method-call` | 28.14 / 2.10 | 0.159 / 0.126 |
| `hover-stdlib-func` | 91.21 / 35.60 | 15.234 / 0.116 |
| `hover-builtin-append` | 9.91 / 1.04 | 51.912 / 0.159 |
| `hover-local-inferred` | 11.03 / 1.73 | 0.424 / 0.116 |
| `sig-iface-method-p1` | 0.73 / 0.11 | 0.168 / 0.125 |
| `sig-cross-pkg-func-p2` | 0.45 / 0.06 | 0.178 / 0.118 |
| `sig-stdlib-variadic-p1` | 0.46 / 0.09 | 0.185 / 0.120 |
| `sig-nested-inner` | 1.53 / 0.05 | 0.151 / 0.118 |
| `sig-builtin-append-p1` | 1.27 / 0.10 | 0.255 / 0.198 |

rs-gopls 峰值工作集（5 次会话每 50 ms 采样，Win32 `GetProcessMemoryInfo` + 工具帮助快照）：主进程中位 `12.45 MiB` / 最大 `12.60 MiB`；包含子进程 `go list` 的进程树中位 `32.67 MiB` / 最大 `37.21 MiB`。`hover-builtin-append` 首次新查的 `51.912 ms` 来自每个新进程首次按需加载 `GOROOT/src/builtin/builtin.go`，后续查询降至 `0.159 ms`。原始报告位于 Cargo 产物目录的 `hover-signature-benchmark.json`。

```powershell
$target = (cargo metadata --format-version 1 --no-deps --locked | ConvertFrom-Json).target_directory
$server = Join-Path $target 'release\rs-gopls.exe'
$glBench = Join-Path $env:TEMP 'rs-gopls-goland-baseline\localchat-hover-signature-bench.json'
python -B .\tools\compare_hover_and_signature.py $server --goland-bench $glBench --sessions 5 --iterations 30 --json (Join-Path $target 'hover-signature-benchmark.json')
```

