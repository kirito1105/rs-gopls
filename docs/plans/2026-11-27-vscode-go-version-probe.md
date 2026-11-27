# 在 VS Code 官方 Go 扩展中使用 rs-gopls

| 项目 | 内容 |
| --- | --- |
| 状态 | 已完成 |
| 创建日期 | 2026-11-27 |
| 用户确认 | 2026-11-27 已确认执行 |
| 更新日期 | 2026-11-27 |
| 完成日期 | 2026-11-27 |

## 目标与范围

让官方 Go 扩展通过 `go.alternateTools.gopls` 启动本仓库的可执行文件。此次修改限于命令行版本查询、对应测试和 VS Code 配置示例，沿用现有 stdio 语言服务。

## 验收标准

- [x] 运行 `rs-gopls version -json` 时，即使调用方保持标准输入打开，进程也会及时以状态 0 退出。标准输出是一个 JSON 对象，`Main.Path` 标识 rs-gopls，`Main.Version` 是可解析的项目版本。
- [x] `rs-gopls version` 输出一行可读的程序名和项目版本。无参启动仍可通过 stdio 完成初始化、导航、重命名及正常关闭。
- [x] 在独立 VS Code 配置中启用官方 `golang.go` 并指向本次构建。用同一 Go 示例验证扩展启动的可执行文件、跨包跳转和改名涉及的文件，记录 VS Code、扩展和服务器版本。

## 实现依据

本机安装的官方 Go 扩展为 0.58.0。其 `getLanguageServerToolPath` 接受替代工具路径，`getLocalGoplsVersion` 先执行 `version -json`，读取 `Main.Version`，必要时再执行 `version`。当前 Rust 入口直接创建 stdio 连接，已有 LSP 初始化响应从 `CARGO_PKG_VERSION` 读取版本，两处可共用同一版本来源。[官方 Go 扩展源码](https://github.com/golang/vscode-go/blob/v0.58.0/extension/src/language/goLanguageServer.ts) · [服务器入口](../../src/main.rs)

## 实施步骤

1. 写进程级回归测试，保持 stdin 打开，先在当前分支复现版本查询等待。
2. 在创建 LSP 连接前处理两个版本命令，保留现有服务启动路径，并在 README 写明官方 Go 扩展的设置。
3. 执行 Rust 检查与测试，在独立 VS Code 环境用真实的 Go 扩展运行编辑器验收。

## 验证计划

运行 `cargo fmt --all -- --check`、`cargo check -p rs-gopls --locked` 和 `cargo test -p rs-gopls --locked`。沿用 Cargo 配置的产物目录，在没有 Generic LSP Client 参与的编辑器会话打开测试模块，核对定义位置和工作区改名结果。

## 完成记录

- 分支 `codex/vscode-go-version-probe` 基于上游 `main` 的 `6e6f0a7`。修改前，两个进程级用例都在 stdin 保持打开时等待超过 5 秒。修改后两者均正常退出，分别输出 `rs-gopls 0.1.0` 和包含 `Main.Path = rs-gopls`、`Main.Version = 0.1.0` 的 JSON。
- `cargo fmt --all -- --check`、`cargo check -p rs-gopls --locked`、`cargo test -p rs-gopls --locked` 和 release 构建通过。Rust 测试共 259 项，其中 2 项覆盖命令行版本查询。
- 在 VS Code 1.148.0 的独立扩展宿主加载官方 Go 扩展 0.58.0。记录到扩展先对 release 版 `rs-gopls.exe` 执行 `version -json`，再无参启动同一文件。扩展日志记录了初始化、定义查询、改名前检查、改名、关闭响应和退出通知。
- 在仓库示例的副本调用 VS Code 的定义和改名命令。`client/client.go` 第 9 行的 `api.Bonus` 跳到 `api/api.go` 第 7 行。改为 `Award` 后，编辑覆盖 `api/api.go`、`client/alias.go` 和 `client/client.go`。保存后，全部 7 个示例文件与预期逐字节一致，`go test ./...` 的两个包均通过。
- 本机体验配置 `rs-gopls Official Go` 仅安装 `golang.go@0.58.0`，打开独立示例后确认运行的是本次 release 产物。扩展的 Go 输出面板会显示 `rs-gopls(undefined/undefined)`，因为它尝试按官方 gopls 的 JSON 格式解析 LSP 初始化返回的版本字符串；命令行版本查询返回 `0.1.0`。
- 按后续体验要求，将跨包查找实现用例收录到 [implementation-navigation 示例](../../tools/fixtures/implementation-navigation/api/notifier.go)。只打开接口文件时，官方 Go 扩展在 `Notifier` 和 `Notify` 上分别找到 `email`、`sms` 包内的两个类型和两个方法。仓库副本通过 `go test ./...`，release 版通过新增的 2 组精确位置断言和已有的 29 组导航对照。
