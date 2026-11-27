# GoLand 格式化与代码操作基线

记录 **Track D · 格式化与代码操作**（`import_edits` AST 导入契约、`textDocument/formatting`、`textDocument/codeAction` 的 `source.organizeImports` 与 `quickfix`）的 GoLand 对照基线、严格校验结果及发布版性能数据。[Track D 计划](plans/2026-11-27-go-actions-formatting.md)

## 测试环境与脱敏约定

- **GoLand 构建号**：`GO-263.7307.354`（通过环境变量 `GOLAND_EXE` 或 `--goland` 参数指定，不硬编码个人路径）
- **Go SDK**：`go1.26.3 windows/amd64`（`CGO_ENABLED=1`）
- 合并发布版 SHA-256 `CE1A7C089F88C97474B3A7198F8BDD19BAFA6917B067A87A593864EFA347113E`。文末重放命令通过 Cargo 元数据定位二进制。
- **对照语料**：仓内自包含模块 [tools/fixtures/actions](../tools/fixtures/actions)（9 个用例，覆盖未格式化与已格式化源码、三段分组导入整理、cgo 前导注释与 `import "C"` 保护及 5 项 QuickFix）

## 严格正确性对照

使用 [tools/check_goland_actions.py](../tools/check_goland_actions.py) 对发布版执行 stdio 严格校验（按 UTF-16 坐标实际应用服务端返回的 `TextEdit` / `WorkspaceEdit` 后逐字节核对正文），**9/9 项全部通过，无差异**：

| 用例 ID | 能力 | 校验内容 | 结果 |
| --- | --- | --- | --- |
| `format-unformatted` | `textDocument/formatting` | 与 `gofmt` 输出逐字节一致，最小化 UTF-16 编辑 | PASS |
| `format-clean` | `textDocument/formatting` | 已格式化文件返回空编辑 | PASS |
| `organize-imports` | `source.organizeImports` | 删未使用导入、去重、删冗余别名、三段分组排序并保留行上/行尾注释 | PASS |
| `organize-cgo-imports` | `source.organizeImports` | 完整保留紧邻 `import "C"` 上方的 cgo C 注释与 `import "C"` 声明原位不动 | PASS |
| `unused-import` | `quickfix` | 删除未使用导入 | PASS |
| `missing-stdlib-import` | `quickfix` | 为未导入标准库限定符自动添加导入 | PASS |
| `missing-local-import` | `quickfix` | 为未导入本地工作区包限定符自动添加导入 | PASS |
| `unused-variable` | `quickfix` | 单变量短声明 `unused := 42` 修复为 `_ = 42`，多变量保留 `:=` | PASS |
| `no-new-variables` | `quickfix` | 无新变量短声明 `:=` 替换为 `=` | PASS |

端到端闭环验证（[tools/workspace_checks/actions.py](../tools/workspace_checks/actions.py)）：通过 stdio 应用 4 类 QuickFix 后再次拉取 Track B 诊断，对应诊断全部清零。

## 性能实测（发布版）

每个用例在包图就绪后首查一次，预热 5 次，再测 30 次暖态查询和 30 次插入首行注释后的查询。每次计时从发送 LSP 请求到解码响应，逐项校验编辑结果。下表使用最近秩分位数，单位为毫秒。[计时脚本](../tools/compare_actions.py)

| 合并发布版 | 样本数（首查 / 暖态 / 编辑后） | 首查 P50 / P95 | 暖态 P50 / P95 | 编辑后 P50 / P95 |
| --- | ---: | ---: | ---: | ---: |
| `textDocument/formatting` | 2 / 60 / 60 | 27.0987 / 127.6176 | 25.8410 / 29.0452 | 24.8124 / 27.8888 |
| `source.organizeImports` | 2 / 60 / 60 | 2.7014 / 37.2133 | 0.1972 / 0.2480 | 0.5032 / 0.6109 |
| `quickfix` | 5 / 150 / 150 | 0.4956 / 1.1497 | 0.3924 / 0.4956 | 0.7895 / 1.0168 |

Windows 每 50 ms 采样一次。服务器峰值工作集为 14.31 MiB，含 `go.exe` 和 `gofmt.exe` 的进程树采样峰值为 39.41 MiB。

### `gofmt` 路径缓存前后

缓存前后的两轮实测均使用 Go 1.26.3、相同夹具和未显式设置 `GOROOT` 的进程环境。夹具 SHA-256 为 `D9EA2E3DA5FBC45E2660F49A7D6F41FB614A39D222640994B375057D50B89C97`。两个格式化用例位于同一目录，每个版本使用独立服务器进程。

| 格式化版本 | 首查 P50 / P95（2 次） | 暖态 P50 / P95（60 次） | 编辑后 P50 / P95（60 次） |
| --- | ---: | ---: | ---: |
| Track D 归档基线 | 105.2913 / 109.6119 | 106.6592 / 118.4161 | 108.2061 / 117.9093 |
| 合并版本 `3eeb2e5`，逐次查找 | 129.4313 / 132.1753 | 120.5525 / 193.7707 | 115.0664 / 132.9770 |
| 合并版本 `ae120e5`，缓存路径 | 27.0987 / 127.6176 | 25.8410 / 29.0452 | 24.8124 / 27.8888 |

缓存版本第一次格式化用时 127.6176 ms，包含 `go env GOROOT`。同目录第二个用例首查用时 27.0987 ms，复用了已解析的路径。与同一合并版本的逐次查找相比，暖态 P50 降低 78.6%，P95 降低 85.0%。每次格式化仍会启动 `gofmt` 并处理当前缓冲区文本。

## 重放与现场采集

```powershell
$target = (cargo metadata --format-version 1 --no-deps --locked | ConvertFrom-Json).target_directory
$server = Join-Path $target 'release\rs-gopls.exe'
python -B .\tools\check_goland_actions.py $server --json (Join-Path $target 'actions-check.json')
python -B .\tools\compare_actions.py $server --json (Join-Path $target 'actions-bench-cached-gofmt.json')
python -B .\tools\compare_actions.py $server --capture-goland --goland-bench (Join-Path $target 'goland-actions.json') --json (Join-Path $target 'actions-goland-bench.json')
```
