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

SDK 的四个目标在 Go 的不同版本上可能移动。严格比较已固定的 SDK 范围时使用 Go 1.27.4，并令客户端和语言服务器通过同一 `PATH` 选择它。普通项目仍可使用自己的工具链。
