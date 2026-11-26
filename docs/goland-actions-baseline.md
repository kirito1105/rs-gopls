# GoLand 格式化与代码操作基线

记录 **Track D · 格式化与代码操作**（`import_edits` AST 导入契约、`textDocument/formatting`、`textDocument/codeAction` 的 `source.organizeImports` 与 `quickfix`）的 GoLand 对照基线、严格校验结果及发布版性能数据。[Track D 计划](plans/2026-11-27-go-actions-formatting.md)

## 测试环境与脱敏约定

- **GoLand 构建号**：`GO-263.7307.354`（通过环境变量 `GOLAND_EXE` 或 `--goland` 参数指定，不硬编码个人路径）
- **Go SDK**：`go1.26.3 windows/amd64`（`CGO_ENABLED=1`）
- **rs-gopls 发布版**：`target/release/rs-gopls.exe`，SHA-256 `0A0532A807D96E98D0301E527CA2E082F6D08A356B43AC58A64690B98768C362`
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

使用 [tools/compare_actions.py](../tools/compare_actions.py) 测量 stdio JSON-RPC 往返耗时（每样本 1 次首查、预热 5 次后 30 次暖态复查、30 次未保存首行注释编辑后查询），P50 / P95 使用最近秩（ms）：

| 能力 | 样本数（首查 / 暖态 / 编辑后） | 首查 P50 / P95（ms） | 暖态 P50 / P95（ms） | 编辑后 P50 / P95（ms） |
| --- | ---: | ---: | ---: | ---: |
| `textDocument/formatting`（调用 `gofmt`） | 2 / 60 / 60 | 105.2913 / 109.6119 | 106.6592 / 118.4161 | 108.2061 / 117.9093 |
| `source.organizeImports`（纯 Rust AST） | 2 / 60 / 60 | 1.0992 / 38.2810 | 0.1789 / 0.2224 | 0.4778 / 0.6022 |
| `quickfix`（数据驱动消费 Track B 快照） | 5 / 150 / 150 | 0.3864 / 0.7853 | 0.3862 / 0.4593 | 0.7184 / 0.8831 |

- **峰值工作集**（Windows 50 ms 采样，含 `go.exe`/`gofmt.exe`）：主进程峰值 `10.44 MiB`（`10,948,608 B`），进程树峰值 `26.55 MiB`（`27,844,608 B`）。

## 重放与现场采集

```powershell
$target = (cargo metadata --format-version 1 --no-deps --locked | ConvertFrom-Json).target_directory
$server = Join-Path $target 'release\rs-gopls.exe'
python -B .\tools\check_goland_actions.py $server --json (Join-Path $target 'actions-check.json')
python -B .\tools\compare_actions.py $server --capture-goland --goland-bench (Join-Path $target 'goland-actions.json') --json (Join-Path $target 'actions-goland-bench.json')
```
