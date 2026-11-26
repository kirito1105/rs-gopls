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
