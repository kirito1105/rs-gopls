# GoLand 层级导航与性能基线

记录 **Track F · 基座、层级与性能**（`callHierarchy/*`、`typeHierarchy/*`、多 Workspace Folder 与 Phase 4 冷反查索引优化）的 GoLand 对照基线、严格校验结果、有意差异及发布版性能数据。[Track F 计划](plans/2026-11-27-go-hierarchy-foundation.md)

## 测试环境与脱敏约定

- **GoLand 构建号**：`GO-263.7307.354`（通过环境变量 `GOLAND_EXE` 或 `--goland` 参数指定，不硬编码个人路径）
- **Go SDK**：`go1.26.3 windows/amd64`（`CGO_ENABLED=1`）
- **rs-gopls 发布版**：`target/release/rs-gopls.exe`，SHA-256 `0A0532A807D96E98D0301E527CA2E082F6D08A356B43AC58A64690B98768C362`
- **对照语料**：仓内自包含模块 [tools/fixtures/hierarchy](../tools/fixtures/hierarchy)（20 个调用与类型层级查询，覆盖跨包调用、接口方法、递归调用、多调用点聚合、测试包调用、接口嵌入链、结构体嵌入与类型别名）

## 严格正确性对照

使用 [tools/check_goland_hierarchy.py](../tools/check_goland_hierarchy.py) 对发布版执行 stdio 严格校验，**20/20 项全部通过，无任何未记录差异**：

- **Call Hierarchy（11/11）**：`prepare-function`、`prepare-cross-package-call`、`prepare-interface-call`、`prepare-method-call`、`leaf-callers`、`recursive-callers`、`interface-method-callers`、`dispatch-callees`、`method-callees`、`recursive-callees`、`leaf-callees`。
- **Type Hierarchy（9/9）**：`prepare-interface`、`prepare-struct`、`prepare-alias-use`、`reader-subtypes`、`read-closer-subtypes`、`base-subtypes`、`service-supertypes`、`read-closer-supertypes`、`derived-supertypes`。
- **有意差异记录**：在 GoLand 现场采集对照中，GoLand 调用层级会略去递归自调用边，`Base` 的结构体嵌入子类型集合为空；`rs-gopls` 均按 LSP 规范完整保留递归调用边与直接结构体嵌入子类型。

## 层级查询性能实测（发布版）

使用 [tools/compare_hierarchy.py](../tools/compare_hierarchy.py) 在索引就绪后测量 stdio JSON-RPC 往返耗时（每样本 1 次首查、预热 5 次后 30 次暖态复查、30 次未保存首行注释编辑后查询），P50 / P95 使用最近秩（ms）：

| 请求 | 样本数（首查 / 暖态 / 编辑后） | 首查 P50 / P95（ms） | 暖态 P50 / P95（ms） | 编辑后 P50 / P95（ms） |
| --- | ---: | ---: | ---: | ---: |
| `textDocument/prepareCallHierarchy` | 4 / 120 / 120 | 0.3444 / 0.3716 | 0.2209 / 0.3560 | 0.2405 / 0.3578 |
| `callHierarchy/incomingCalls` | 3 / 90 / 90 | 0.9087 / 1.3041 | 0.5555 / 0.8468 | 0.8057 / 1.0597 |
| `callHierarchy/outgoingCalls` | 4 / 120 / 120 | 0.5973 / 0.6263 | 0.4179 / 0.7751 | 0.6210 / 0.7323 |
| `textDocument/prepareTypeHierarchy` | 3 / 90 / 90 | 0.2212 / 0.2363 | 0.1714 / 0.2066 | 0.2245 / 0.2689 |
| `typeHierarchy/subtypes` | 3 / 90 / 90 | 135.3790 / 1230.1994 | 0.5177 / 0.7043 | 0.6931 / 0.8600 |
| `typeHierarchy/supertypes` | 3 / 90 / 90 | 0.4886 / 102.3716 | 0.3805 / 0.5557 | 0.5414 / 0.6689 |

- **峰值工作集**（Windows 50 ms 采样，含 `go.exe`/`git.exe`）：主进程峰值 `30.59 MiB`（`32,071,680 B`），进程树峰值 `45.05 MiB`（`47,243,264 B`）。

## Phase 4 具体类型冷反查优化实测

在 `src/semantic/implementation/` 引入跨查询复用的工作区级接口方法名倒排索引后（提交 `97fc13d`）：
- **单元测试确认**：清空查询结果缓存后切换到方法集互不相交的具体类型，新增磁盘文件探测次数为 **0**；未保存覆盖新增接口即时可见、关闭恢复磁盘剪枝、文件 watcher 精确失效对应包视图、泛型接口别名与传递嵌入均通过测试。
- **多包合成项目（99 包含 `io/fs`）**：预热一个具体类型后切换到另一具体类型的反查耗时由 **59 / 63 ms** 降至 **24 / 30 ms**（P50 / P95）。
- **既有真实项目回归对照**（6 轮交替独立进程，核对全部目标与 UTF-16 范围）：跨类型切换反查 P50 降低 **88.7%**，P95 降低 **90.8%**；`tools/fixtures/implementation-navigation` 29/29 项与 6 组工作区实现回归全部通过。

## 重放与现场采集

```powershell
$target = (cargo metadata --format-version 1 --no-deps --locked | ConvertFrom-Json).target_directory
$server = Join-Path $target 'release\rs-gopls.exe'
python -B .\tools\check_goland_hierarchy.py $server --json (Join-Path $target 'hierarchy-check.json')
python -B .\tools\compare_hierarchy.py $server --capture-goland --goland-bench (Join-Path $target 'goland-hierarchy.json') --json (Join-Path $target 'hierarchy-goland-bench.json')
```
