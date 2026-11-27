# GoLand 实时诊断基线与性能对照

记录 **Track B · 实时诊断引擎**（`textDocument/diagnostic` 与 `textDocument/publishDiagnostics`）的 GoLand 对照基线、严格校验结果、有意差异及发布版性能数据。[Track B 计划](plans/2026-11-27-go-diagnostics-engine.md)

## 测试环境与脱敏约定

- **GoLand 构建号**：`GO-263.7307.354`（通过环境变量 `GOLAND_EXE` 或 `--goland` 参数指定，不在仓库中硬编码本机路径）
- **Go SDK**：`go1.26.3 windows/amd64`（`CGO_ENABLED=1`）
- **rs-gopls 发布版**：`target/release/rs-gopls.exe`，SHA-256 `0A0532A807D96E98D0301E527CA2E082F6D08A356B43AC58A64690B98768C362`
- **对照语料**：仓内自包含模块 [tools/fixtures/diagnostics](../tools/fixtures/diagnostics)（11 个样本文件，含 2 份干净文件、2 份语法错误文件、7 份语义与类型错误文件）及当前 Go SDK 标准库 10 个包（57 个源文件，用于零误报回归）

## 严格正确性对照

使用 [tools/check_goland_diagnostics.py](../tools/check_goland_diagnostics.py) 对发布版执行 stdio 严格校验，**11/11 项全部通过**：

| 样本 ID | 覆盖场景 | 结果 | 差异说明 |
| --- | --- | --- | --- |
| `clean` | 泛型、接口、嵌入、闭包、别名、`_` 导入 | PASS（0 诊断） | 无 |
| `clean-test` | 测试包干净代码 | PASS（0 诊断） | 无 |
| `missing-paren` | 缺少右括号 + 非 BMP UTF-16 范围 | PASS | 无 |
| `missing-expression` | 缺少表达式语法错 | PASS | 无 |
| `unused-import` | 未使用导入（`Unnecessary` + `RemoveUnusedImport`） | PASS | 无 |
| `self-import` | 包自导入 | DIFF（通过） | LSP 同时回报 `package.load` 包加载错误 |
| `invalid-import` | 无法解析的导入路径 | DIFF（通过） | LSP 同时回报 `package.load` 包加载错误 |
| `names` | 未解析标识符/类型/字段/方法与跨包未导出访问 | PASS | 无 |
| `declarations` | 同包/同块重声明、`:=` 无新变量、常量重赋值、未使用局部变量 | PASS | 无 |
| `calls` | 非函数调用、实参数量不匹配、返回值数量不匹配 | PASS | 无 |
| `types` | 类型不匹配、接口未实现、非 bool 条件、`nil` 无显式类型、未知字面量字段、重复 `case`、`_` 作值 | PASS | 无 |

附加验证：
- **零误报门禁**：`clean.go`、`clean_test.go` 以及 Go `1.26.3` 标准库 10 个包共 57 个源文件均为 **0 诊断**。
- **Push / Pull 能力协商**：[tools/workspace_checks/diagnostics.py](../tools/workspace_checks/diagnostics.py) 确认支持 Pull 能力的客户端（如 Zed）不接收重复 `publishDiagnostics` 推送且支持 `previousResultId -> unchanged`；仅声明 Push 能力的客户端在打开、编辑、关闭（空数组清理）和包图安装后收到携带正确文档版本的推送。

## 性能实测（发布版）

使用 [tools/compare_diagnostics.py](../tools/compare_diagnostics.py) 在含测试包图与索引就绪后测量 stdio JSON-RPC 往返耗时（每样本 1 次就绪首查、预热 5 次后 30 次暖态复查、30 次插入/回滚未保存首行注释后的编辑查询），P50 / P95 使用最近秩（ms）：

| 请求 | 场景 | 样本数 | rs-gopls P50（ms） | rs-gopls P95（ms） | 最小值 / 最大值（ms） |
| --- | --- | ---: | ---: | ---: | ---: |
| `textDocument/diagnostic` | 就绪首查 | 11 | 0.3256 | 26.8146 | 0.1654 / 26.8146 |
| `textDocument/diagnostic` | 暖态复查 | 330 | 0.1588 | 0.2048 | 0.1025 / 0.4953 |
| `textDocument/diagnostic` | 编辑后查询 | 330 | 0.4134 | 0.9804 | 0.1208 / 1.1823 |

- **100 条诊断热缓存 Pull 专项**：30 次连续 stdio 往返 P50 `0.109 ms`、P95 `0.132 ms`。
- **峰值工作集**（Windows 50 ms 采样，父进程及存活子进程树含 `go.exe`/`git.exe`）：主进程峰值 `9.84 MiB`（`10,317,824 B`），进程树峰值 `29.50 MiB`（`30,932,992 B`）。

## 重放与现场采集

在 GoLand 中打开 `tools/fixtures/diagnostics` 并配置 Go SDK 后，可随时设置 `$env:GOLAND_EXE` 并运行以下命令现场采集与对齐计时：

```powershell
$target = (cargo metadata --format-version 1 --no-deps --locked | ConvertFrom-Json).target_directory
$server = Join-Path $target 'release\rs-gopls.exe'
python -B .\tools\check_goland_diagnostics.py $server --json (Join-Path $target 'diagnostics-check.json')
python -B .\tools\compare_diagnostics.py $server --capture-goland --goland-bench (Join-Path $target 'goland-diagnostics.json') --json (Join-Path $target 'diagnostics-goland-bench.json')
```
