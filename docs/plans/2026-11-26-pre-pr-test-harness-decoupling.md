# Pre-PR 2 · 解耦 Python 与 Rust 测试夹具

| 项目 | 内容 |
| --- | --- |
| 状态 | 已完成 |
| 创建日期 | 2026-11-26 |
| 用户确认 | 已确认（2026-11-26） |
| 更新日期 | 2026-11-26 |
| 完成日期 | 2026-11-26 |

## 目标与范围

- 将 stdio LSP 客户端、工作区会话、UTF-16 与文件 URI 等公共助手、可暂停的 `go list` 夹具移到 `tools/lsp_harness.py`。客户端提供按请求 ID 等待原始响应的接口，保留同步 `request`，独立保存服务端通知，并自动应答服务端请求。
- 将 `tools/check_workspace_updates.py` 拆为薄 CLI 和 `tools/workspace_checks/` 下按能力注册的模块。所有既有 Python 脚本的命令行参数、默认值、退出码和测试覆盖 100% 向后兼容。工作区入口默认按原顺序执行 23 项用例，增加可重复的 `--module`，沿用可重复的 `--case`。
- 将内存 LSP 会话的通用操作沉淀到 `src/test_support.rs`，供 `src/symbol_request_tests.rs` 和后续功能线使用。保留 `TestProject` 的临时目录、文件写入和合成包图构建接口。声明、类型定义、引用、实现、Hover、SignatureHelp、符号和包图刷新继续使用既有语料与断言。
- 本次行为改动限于测试与验证工具。既有 GoLand 采集作为导航回归依据。本计划的生产入口依赖由两个并行 PR 共用的测试契约固定。[并行协作与模块拆分](../COLLABORATION.md#并行前的两个解耦-pr)

### 与 Pre-PR 1 的文件边界

| 变更 | 写入范围 |
| --- | --- |
| Pre-PR 1 · Rust 生产入口与新增模块 | 现有 `src/main.rs`、`src/semantic.rs`。新增 `src/dispatch.rs`、`src/protocol.rs`、`src/scheduler.rs` 及 `src/semantic/{source,index,resolver,snapshot,definition,references,query}.rs`，含随模块迁移或新增的内联测试。 |
| Pre-PR 1 · 既有语义接线 | `src/semantic/{implementation,workspace_symbols,document_symbols,hover,signature_help}.rs`。其调用的数据结构若需调整导入，由同一 PR 在 `src/semantic/` 内接线。 |
| Pre-PR 2 · Python 测试和验证工具 | 现有 `tools/compare_definitions.py`、`tools/check_definitions.py`、`tools/check_workspace_updates.py`、`tools/check_goland_signature_help.py`、`tools/compare_graph_refresh.py`。新增 `tools/lsp_harness.py`、`tools/test_lsp_harness.py`、`tools/test_workspace_checks.py` 以及 `tools/workspace_checks/{__init__,_assertions,definition,references,implementation,hover_signature,symbols,graph_refresh}.py`。 |
| Pre-PR 2 · Rust 测试 | `src/test_support.rs`、`src/symbol_request_tests.rs`。 |
| 规格文档 | Pre-PR 1 对应 [引擎与调度解耦计划](2026-11-26-pre-pr-engine-dispatch-decoupling.md)，Pre-PR 2 对应本计划。`docs/COLLABORATION.md` 由协作说明的独立提交维护。 |

两个计划的写入范围已按上述路径对照。合入时还要保持 Rust 测试接线。Pre-PR 1 在自己的文件内保留 `main.rs` 中两个 `#[cfg(test)]` 模块入口，以及现有符号测试可见的 `State/Running`、`path_uri`、`handle_request`、`handle_notification`、`WorkspaceResult` 和包图、任务推进观察入口。调度迁移需要的适配放到它所拥有的生产入口或调度文件内。Pre-PR 2 的通用会话夹具只依赖 `lsp_server` 等通用消息类型，符号测试通过本地适配调用生产入口。两个 PR 各自从同一主线验证，合并后再次运行符号请求测试。交付时以各自相对共同基点的 `git diff --name-only` 核对实际文件清单和交集。

## 验收标准

- [x] `tools/lsp_harness.py` 的独立 stdio 自测通过。模拟服务器确认两个请求同时在途并逆序返回，客户端按整数或字符串 ID 分发。交错的 `textDocument/publishDiagnostics`、`window/logMessage` 等通知按到达顺序完整可取。客户端空闲时也能按原 ID 恰好应答一次 `client/registerCapability`，并可校验 Go 文件监听注册。标准响应、JSON-RPC 错误、超时、迟到响应、重复响应和 EOF 均有针对性断言。
- [x] 旧命令 `python -B .\tools\check_workspace_updates.py <candidate>` 按原顺序通过 23/23，现有参数与退出码完整兼容。重复 `--case` 去重并保留首见顺序，`--module` 能选中全部所属用例，合用取交集。新增一个注册模块即可让入口发现其用例。重复注册名报告冲突。
  - [x] 独立 Python 自测已确认 23 项默认顺序、自动发现、重名与重序号冲突、用例及模块筛选、失败后继续执行和 0/1/2 三种退出码。
- [x] 全部 16 个相关 Python 入口通过语法、导入与旧 `--help` 检查。声明、类型定义、引用、实现、Hover、SignatureHelp、文档与工作区符号以及图刷新通过原有严格回归和基准脚本验收，保留输出及 JSON 字段契约、完整结果范围和既有允许差异。
  - [x] 27 个 Python 文件编译通过，16 个入口的导入和帮助检查通过。工作区入口只新增 `--module` 帮助和用法，原有 9 类参数错误及共享助手重导出均验证通过。
- [x] `go list` 可在真实元数据捕获后暂停，在测试指定的时刻放行，也可注入一次失败。快照等待、包图代次替换、刷新合并、退出清理，以及显式的提前响应、取消和重复响应检查通过。
  - [x] 真实 Go 转发器已验证暂停和放行、旧新快照、真实失败的 stderr 和退出码、一次性故障注入及恢复。Python 自测已验证异常时放行、提前响应和重复响应断言。
- [x] Rust 共用会话和项目夹具，现有三个符号请求测试的断言全部通过。`cargo check --workspace --locked` 与 `cargo test --workspace --locked` 退出码为 0，编译与测试零告警。
- [x] 记录两个 Pre-PR 相对共同基点的完整改动路径，交集为空。两边各自以及合并后均可通过受影响的 Rust 测试。

## 实现依据

### Python 导入与兼容面

`compare_definitions.py` 当前显式定义 `LspClient`、`position_at`、`path_from_uri`、`executable`。其余模块级函数为 `caller`、`point_in_caller`、`verify_location`、`definition`、`measure`、`net_http_source`、`main`，另有基准常量 `MODULE`、`TARGET`。前四项移到共享模块并从原路径重导出。合成声明查询与计时逻辑由该基准入口调用。[现有声明基准](../../tools/compare_definitions.py)

| 直接消费者（共 15 个，另计定义模块自身） | 从 `compare_definitions` 导入 | 从 `check_workspace_updates` 导入或需要核对的行为 |
| --- | --- | --- |
| `check_definitions.py` | `LspClient, executable, path_from_uri, position_at` | 定义 `Site`、`check_location`，阶段 1–6 的声明范围与编辑用例 |
| `check_goland_definitions.py` | `path_from_uri`、`LspClient`、`executable`，均在函数内导入 | `normalize_locations` 支持 `Location` 和 `LocationLink`，`--method definition/typeDefinition` |
| `check_goland_references.py` | `LspClient, executable` | 引用去重及范围检查所共用的 `Location` 类型 |
| `check_goland_implementations.py` | `LspClient, executable` | 兼容初始化、正向与反向实现、活动包范围 |
| `check_goland_hover.py` | `LspClient, executable` | Markdown、GoDoc 和 UTF-16 范围 |
| `check_goland_signature_help.py` | `LspClient, executable, position_at` | `check_editing` 局部导入 `expect_error, go_list_relay, session, wait_for_response, wait_for_snapshot, write` |
| `check_goland_symbols.py` | `LspClient, executable, path_from_uri` | 嵌套和扁平大纲能力协商，文档与工作区符号 |
| `check_workspace_updates.py` | `LspClient, executable, path_from_uri` | 当前同时定义工作区会话、watcher 握手、响应辅助函数、go list 转发器和全部用例 |
| `compare_workspace_definitions.py` | `LspClient, executable` | 声明和类型定义计时共用 `WorkingSetSampler`、`describe_binary`，采样读取 `client.process.pid` |
| `compare_workspace_references.py` | `LspClient, executable` | 复用声明基准的范围编辑参数和工作集采样，核对引用集合 |
| `compare_workspace_implementations.py` | `LspClient, executable` | 复用声明基准的采样器和引用基准的汇总，核对共同目标集合 |
| `compare_first_implementations.py` | `LspClient, executable` | 复用实现基准的查询和引用基准的汇总，核对新进程首个结果 |
| `compare_hover_and_signature.py` | `LspClient, executable` | 复用 Hover、签名检查器和 `WorkingSetSampler`，通过 `client.close()` 回收会话 |
| `compare_symbols.py` | `LspClient, executable` | 复用符号语料及能力检查、Hover 基准的散列/分位数和声明基准的采样器 |
| `compare_graph_refresh.py` | `LspClient, executable` | 导入 `next_message, notify_changes, site, write`，自行等候并应答 watcher 注册，按剩余时间临时设置 `client.timeout` |

现有 `LspClient` 的 reader 只负责把帧推入 `inbox`。同步 `request()` 读队列时会跳过通知和其他请求的响应。工作区的 `wait_for_response` 则要求目标 ID 是下一条响应，`symbol_responses` 允许指定的一组 ID 内乱序，并在 `check_no_extra_symbol_responses` 用 0.2 秒观察窗检查多余响应。拆分时保留这些用例自己的断言，以及 `send_batch` 在同一写入中发送连续 LSP 帧的顺序。[客户端](../../tools/compare_definitions.py) · [工作区检查](../../tools/check_workspace_updates.py)

共享模块把 `Site` 与 `site` 放在同一处，`check_definitions.Site` 保留重导出。把 `write`、`notify_changes`、`session`、Go 转发器和通用响应辅助函数移到共享模块。`workspace_checks/_assertions.py` 共享跨能力断言，继续复用 `check_location`、`normalize_locations`、`Location` 和符号检查器的 `contains`、`lsp_span`、`request_document`。`go-work` 同时检查声明、引用与符号。`symbol-active-search` 用引用查询准备含测试包的图。共享模块只依赖标准库，能力模块引用共享助手，注册入口负责发现能力模块。

### 工作区用例注册

表中序号是现有 `CASES` 的执行顺序。每个文件声明自己的 `CASES` 列表，保留用例名、函数签名 `(executable_path, root, timeout)`、夹具数据和预期结果。

| 顺序 | 模块 | 现有用例与函数 | 需要保留的断言 |
| ---: | --- | --- | --- |
| 1 | `graph_refresh` | `mutations` → `check_mutations` | watcher 创建、修改、重命名、删除，构建标签，缓冲区优先，`go.mod replace` |
| 2 | `graph_refresh` | `go-work` → `check_go_work` | 替换 `go.work use` 后声明、引用及 workspace symbol 指向新模块 |
| 3 | `definition` | `test-packages` → `check_test_packages` | 普通文件、内部测试与外部测试包的声明可见性 |
| 4 | `graph_refresh` | `gopath` → `check_gopath` | `GO111MODULE=off` 下声明、类型定义和含声明的引用 |
| 5 | `graph_refresh` | `async-refresh` → `check_async_refresh` | 暂停快照、本地与无关包可用、等待请求用最新图、合并重复刷新、取消 `-32800`、过期 `-32801`、失败 `-32803` 及恢复 |
| 6 | `definition` | `overlay-new-file` → `check_overlay_new_file` | 未保存文件与内部测试可见性、外部测试排除、关闭即移除、文件未落盘 |
| 7 | `definition` | `overlay-disk-restore` → `check_overlay_disk_restore` | 跨包落点随缓冲区移动，关闭后恢复，大小和 mtime 相同也重新读取 |
| 8 | `definition` | `type-overlay` → `check_type_overlay` | Unicode 光标、局部推断与遮蔽、增量编辑和关闭恢复 |
| 9 | `hover_signature` | `editor-help-overlays` → `check_editor_help_overlays` | 跨包签名、GoDoc、局部函数变量类型，以及未保存编辑和关闭 |
| 10 | `hover_signature` | `editor-help-refresh` → `check_editor_help_refresh` | 暂停期间本地 Hover/SignatureHelp 可用，等待查询取最新签名和 GoDoc |
| 11 | `symbols` | `symbol-updates` → `check_symbol_updates` | 文档与工作区符号增删改移、Unicode、未闭合结构、草稿、watcher、关闭恢复 |
| 12 | `symbols` | `symbol-waiting-for-graph` → `check_symbols_waiting_for_graph` | 并发等待、取消、即时大纲、事件处理屏障、最新图与编辑内容、重试及无额外响应 |
| 13 | `symbols` | `symbol-active-search` → `check_active_workspace_symbols` | 512 个文件，建索引和扫描时各取消一个请求，编辑使扫描过期，并发重试精确完整 |
| 14 | `references` | `reference-locals` → `check_reference_locals` | 短声明复用、遮蔽、UTF-16 与 `includeDeclaration` 两种取值 |
| 15 | `references` | `reference-workspace` → `check_reference_workspace` | 跨包、内外测试、同名排除、范围编辑、草稿和 watcher 删除 |
| 16 | `references` | `reference-cancel` → `check_reference_cancel` | 等待包图时取消和文档过期，放行后重试 |
| 17 | `references` | `reference-active-cancel` → `check_active_reference_cancel` | 192 个使用文件，扫描中取消、打开缓冲区使请求过期、重试 |
| 18 | `implementation` | `implementation-updates` → `check_implementation_updates` | 内外测试实现、顺序稳定、签名变化、草稿、构建标签与关闭恢复 |
| 19 | `implementation` | `implementation-candidate-packages` → `check_implementation_candidate_packages` | 嵌入与别名的候选传播、跨包编辑扩充、关闭恢复 |
| 20 | `implementation` | `implementation-reverse-candidates` → `check_implementation_reverse_candidates` | 类型反查、嵌入、别名、签名排除、未保存接口 |
| 21 | `implementation` | `implementation-reverse-watched-sources` → `check_implementation_reverse_watched_sources` | 已缓存和被筛掉的候选受 watcher 更新，缓冲区优先与关闭恢复 |
| 22 | `implementation` | `implementation-cancel` → `check_implementation_cancel` | 等待图时取消、文档过期与重试 |
| 23 | `implementation` | `implementation-active-cancel` → `check_active_implementation_cancel` | 384 个实现，活动搜索屏障、取消、过期与全量重试 |

### Rust 测试支撑

- `TestProject::new` 以 PID 和原子计数创建临时根，`write` 创建父目录并写入文件，`Drop` 回收临时目录。`workspace(files, includes_tests)` 将 `Dir/ImportPath/Name/GoFiles` 交给 `Workspace::from_test_listing` 以构造测试包图。三个符号请求测试、三个文档符号测试和四个工作区符号测试已使用该夹具。工作区符号的多根测试在同一夹具上直接构造多条 listing，保留其包身份和边界断言。[TestProject](../../src/test_support.rs) · [文档符号测试](../../src/semantic/document_symbols.rs) · [工作区符号测试](../../src/semantic/workspace_symbols.rs) · [包图测试入口](../../src/workspace.rs)
- `Connection::memory()` 在现有 Rust 源码中只出现在 `symbol_request_tests::start`。该文件反复使用 `request`、`response`、`notify`、`open`、`change` 和接收队列空断言。复用会话的建立与消息封装、整数或字符串 ID 的响应核对、打开/编辑参数和空队列检查。符号能力断言、图代次注入、任务推进及取消的断言归用例。Hover 和 SignatureHelp 的内联 `fixture_root/open` 复用同一份已签入的 Go 项目并直接调用 `Engine`。[符号请求测试](../../src/symbol_request_tests.rs) · [Hover 测试](../../src/semantic/hover.rs) · [SignatureHelp 测试](../../src/semantic/signature_help.rs)

## 实施步骤

1. **建立共享传输。**
   - 保留客户端的构造参数、`process.pid`、`timeout`、`stderr_tail()`、`shutdown()`、`close()` 和文档操作。保留按 UTF-8 字节数写入 `Content-Length`、UTF-16 协商、`definitionProvider` 检查、`initializationOptions.diagnosticsTrigger=Save` 和默认环境。
   - 新增 `start_request(method, **params)` 返回 ID，`wait_response(id, timeout=None)` 返回原始响应。同步 `request` 维持取 `result` 和将错误转为 `RuntimeError` 的约定，读取发起时的 `client.timeout`。`send` 和 `send_batch` 先登记带方法和显式 ID 的请求，再串行写入并刷新整批 LSP 帧。登记锁与写入锁分开。
   - 自动 ID 从 1 递增并避开本会话已使用的显式整数 ID。区分双向请求 ID 以及整数和字符串 ID，检测在途 ID 冲突，防止超时请求的迟到响应匹配到重试。
2. **按消息类型分发并自测。**
   - 读线程按 ID 唤醒对应等待者，通知按到达顺序进入独立队列，由 `get_notification(timeout=None)` 读取。自动回应 `workspace/workspaceFolders` 的文件夹列表、`workspace/configuration` 的逐项 `null`，以及 `window/workDoneProgress/create`、`client/registerCapability`、`client/unregisterCapability` 的 `result: null`。未知客户端方法回 `-32601`。握手检查读取已应答的注册请求记录。
   - 用单调时钟计算每次等待的截止时间。迟到、重复或未知响应可供用例诊断。EOF、坏帧、写入失败和关闭唤醒所有未完成的等待者，并保留已匹配的响应和已收到的通知。
   - 用独立模拟服务端驱动真实 stdio 帧。核对同步请求、显式 ID、批量写入、字符串和整数同值、乱序、空闲时服务端请求和关闭。用响应到达次序实现测试专用的严格等待和指定 ID 集合收集。放行前的立即响应、过早到达的等待请求、漏取消以及 0.2 秒观察窗内的额外响应继续触发各自断言。
3. **迁移会话与可暂停的 Go 转发器。**
   - 工作区 `session` 默认使用 `GO111MODULE=on`、`GOWORK=off`、`GOPROXY=off`，允许调用者覆盖。沿用 `watch_files` 和 `hierarchical`。工作区回归校验 `**/*.go`、`**/go.mod`、`**/go.sum`、`**/go.work`、`**/go.work.sum`。图刷新基准通过同一等待器接受任意带非空监听项的 `workspace/didChangeWatchedFiles` 注册。
   - 从原 PATH 解析真实 Go，在隔离目录编译并在测试子进程 PATH 前置 `go.exe`（Windows）或 `go`。非 `list` 命令照常转发。`calls` 记录每次 `list`，`fail` 注入一次失败，`arm` 触发捕获真实命令的 stdout、stderr 和退出码。捕获后写 `ready`，读到 `release` 后转发。保留原有 30 秒放行兜底和快照等待超时。
   - 以 `PausedGoList` 提供环境、布置暂停、等待快照、放行、故障注入和调用记录。一轮完成后再清理旧 `ready` 与 `release` 以便重用。把放行纳入 `finally`，覆盖发送请求或读取快照失败。迁移 `async-refresh`、`editor-help-refresh`、`symbol-waiting-for-graph`、`reference-cancel`、`implementation-cancel` 和独立签名编辑检查。
   - 图刷新基准复用共享会话初始化与握手，`measure(..., environment)` 可注入该夹具验证受控加载。旧 CLI 使用真实 Go 从写入 `go.mod` 并发出文件事件计至首次精确落点，轮询仍使用原 10 毫秒间隔及重试分类。
4. **注册用例与切换调用方。**
   - `workspace_checks` 用 `pkgutil.iter_modules` 枚举并按名排序，装载公开的非子包模块。每个注册条目含用例名、函数和可选旧序号。原 23 项用序号排序，新项按模块名及模块内声明顺序追加。重复的名称或旧序号须报出冲突的模块。
   - `candidate` 的可执行文件解析和 `--timeout` 的默认 60 秒及正有限数验证沿用原实现。`--module` 取去掉 `.py` 的文件名，重复时选模块并集，与 `--case` 合用取交集，空交集返回参数错误 2。仅按用例筛选时去重并保持指定顺序，按模块筛选时沿默认顺序。每项使用独立子目录。成功打印 `PASS <name>`，失败打印 `FAIL <name>: <error>` 到 stderr 并继续执行后续用例。全部成功返回 0，有失败返回 1。
   - 图刷新从 `next_message` 迁至已应答注册等待器，签名编辑检查直接接入共享夹具。兼容重导出原工作区入口其余外部使用的助手和 `CASES`。完成后，`completion.py`、`diagnostics.py`、`rename.py`、`actions.py`、`editor.py`、`hierarchy.py` 可各在本模块新增注册项。
5. **收拢 Rust 夹具与回归。**
   - `TestSession` 封装内存连接对、标准消息和响应断言。由调用者提供派发入口，测试直接初始化后可以显式单步推进并核查原始 `Response`。项目夹具沿用 `TestProject` 和 `Workspace::from_test_listing` 构建合成包图，复用目录和文件创建。
   - 在 `symbol_request_tests.rs` 用这个会话替换重复代码。验证能力协商、空查询即时返回、过期包图丢弃、最新含测试包的图、冷暖搜索并发取消、编辑失效与重试。

## 验证计划

以下命令在用户确认和实现后从仓库根运行。实施前保存当前 23 个用例名及顺序、16 个入口的帮助和参数错误基线，实施后比较。`tools/test_workspace_checks.py` 固定旧清单，覆盖自动发现、重名、参数筛选与 0/1/2 三类退出码，包括一项模拟失败后仍继续运行下一项。

~~~powershell
python -B -m unittest discover -s tools -p 'test_lsp_harness.py' -v
python -B -m unittest discover -s tools -p 'test_workspace_checks.py' -v
@'
from importlib import import_module
from pathlib import Path
import subprocess
import sys

tools = Path("tools").resolve()
sys.path.insert(0, str(tools))
for path in sorted(tools.rglob("*.py")):
    compile(path.read_bytes(), str(path), "exec")
for path in sorted(tools.glob("*.py")):
    if path.stem.startswith(("check_", "compare_")):
        import_module(path.stem)
        subprocess.run([sys.executable, "-B", str(path), "--help"], check=True)
from lsp_harness import LspClient, executable, path_from_uri, position_at
from compare_definitions import LspClient as old_client
import compare_definitions
assert old_client is LspClient
assert compare_definitions.executable is executable
assert compare_definitions.path_from_uri is path_from_uri
assert compare_definitions.position_at is position_at
'@ | python -B -

cargo check -p rs-gopls --locked
cargo test -p rs-gopls --locked symbol_request_tests
cargo test -p rs-gopls --locked document_symbols::tests
cargo test -p rs-gopls --locked workspace_symbols::tests
cargo check --workspace --locked
cargo test --workspace --locked
cargo build -p rs-gopls --release --locked
$cargoOut = (cargo metadata --format-version 1 --no-deps --locked | ConvertFrom-Json).target_directory
$server = Join-Path $cargoOut 'release\rs-gopls.exe'
~~~

Rust 命令沿用环境配置的产物目录、工具链、profile、features 和 target。确认三个定向筛选实际运行 3、3、5 项，工作区检查和测试均为零告警。记录每条验收命令退出码。

~~~powershell
$goSdkBin = Join-Path $env:USERPROFILE 'go\pkg\mod\golang.org\toolchain@v0.0.1-go1.27.4.windows-amd64\bin'
$env:PATH = "$goSdkBin;$env:PATH"
$env:GOTOOLCHAIN = 'local'
$localchat = 'E:\Code\localchat\server'
$fixture = '.\tools\fixtures'
$typeFixture = Join-Path $fixture 'type-navigation'
$implFixture = Join-Path $fixture 'implementation-navigation'
$helpFixture = Join-Path $fixture 'hover-signature'
$symbolFixture = Join-Path $fixture 'symbol-navigation'
go version
go env GOROOT GOOS GOARCH GOWORK

python -B .\tools\check_workspace_updates.py $server
python -B .\tools\check_workspace_updates.py $server --case type-overlay --case test-packages --case type-overlay
python -B .\tools\check_workspace_updates.py $server --module symbols
python -B .\tools\check_workspace_updates.py $server --module graph_refresh --case go-work
python -B .\tools\check_workspace_updates.py $server --module symbols --case go-work
$LASTEXITCODE  # 预期 2
python -B .\tools\check_workspace_updates.py $server --timeout nan
$LASTEXITCODE  # 预期 2

python -B .\tools\check_definitions.py $server --stage 6 --workspace $localchat
python -B .\tools\check_goland_definitions.py $server --workspace $localchat --method definition
python -B .\tools\check_goland_definitions.py $server --workspace $localchat --method typeDefinition
python -B .\tools\check_goland_definitions.py $server --workspace $typeFixture --fixture (Join-Path $typeFixture 'goland-types.tsv') --method typeDefinition
python -B .\tools\check_goland_references.py $server --workspace $localchat
python -B .\tools\check_goland_implementations.py $server --workspace $localchat
python -B .\tools\check_goland_implementations.py $server --workspace $implFixture --fixture (Join-Path $implFixture 'goland-implementations.json')
python -B .\tools\check_goland_hover.py $server --workspace $localchat
python -B .\tools\check_goland_hover.py $server --workspace $helpFixture --fixture (Join-Path $helpFixture 'goland-hover-signature.json')
python -B .\tools\check_goland_signature_help.py $server --workspace $localchat
python -B .\tools\check_goland_signature_help.py $server --workspace $helpFixture --fixture (Join-Path $helpFixture 'goland-hover-signature.json')
python -B .\tools\check_goland_symbols.py $server --workspace $localchat --fixture (Join-Path $fixture 'localchat-goland-symbols.json')
python -B .\tools\check_goland_symbols.py $server --workspace $symbolFixture --fixture (Join-Path $symbolFixture 'goland-symbols.json')
~~~

默认工作区命令逐项得到表中 23 条 `PASS`。重复 `--case` 的运行依次只输出 `type-overlay`、`test-packages`，符号模块输出第 11–13 项，交集命令只输出 `go-work`。正确性检查命令预期退出 0，核对发布基线的真实项目和补充语料，包括阶段 6 的 124 个合成位置及 16 个项目锚点、58 项类型定义、34 组引用、49 组正反向实现、Hover 21+14、SignatureHelp 13+19，以及符号的大纲和 20+36 条工作区查询。[历史回归基线](2026-11-26-go-document-and-workspace-symbols.md#完成记录) · [补充类型定义](../goland-type-definition-baseline.md#补充类型案例) · [Hover 与签名基线](../goland-hover-signature-baseline.md)

下面使用当前 CLI 跑基准回归。GoLand 启动器对应 `GO-263.7307.354`，符号基准用同版本的运行中 GoLand 进程和对应 `*-bench.json` / `*-timed.json`。LocalChat 固定在 `532228d3d36bd9f0fffb4bda35c180485b2eadd6`。同时记录 gopls 版本、rs-gopls 二进制散列和语料修订。

~~~powershell
$gopls = (Get-Command gopls.exe).Source
$goland = 'E:\Apps\goland-2026.3.0.1.win\bin\goland64.exe'
$baselineDir = Join-Path $env:TEMP 'rs-gopls-goland-baseline'
& $gopls version
Get-FileHash -LiteralPath $server -Algorithm SHA256
git -C $localchat rev-parse HEAD

python -B .\tools\compare_definitions.py $server --baseline $gopls --iterations 4
python -B .\tools\compare_definitions.py $server --baseline $gopls --iterations 4 --stdlib
python -B .\tools\compare_workspace_definitions.py $server --workspace $localchat --method definition --gopls $gopls --sessions 1 --iterations 4 --dependency-iterations 2 --json (Join-Path $cargoOut 'pre-pr2-definitions.json')
python -B .\tools\compare_workspace_definitions.py $server --workspace $localchat --method typeDefinition --gopls $gopls --sessions 1 --iterations 4 --dependency-iterations 2 --json (Join-Path $cargoOut 'pre-pr2-types.json')
python -B .\tools\compare_workspace_references.py $server --workspace $localchat --gopls $gopls --sessions 1 --iterations 4 --json (Join-Path $cargoOut 'pre-pr2-references.json')
python -B .\tools\compare_workspace_implementations.py $server --workspace $localchat --gopls $gopls --sessions 1 --iterations 4 --json (Join-Path $cargoOut 'pre-pr2-implementations.json')
python -B .\tools\compare_first_implementations.py $server --workspace $localchat --gopls $gopls --case store-type --sessions 1 --json (Join-Path $cargoOut 'pre-pr2-first-implementations.json')
python -B .\tools\compare_graph_refresh.py $server --gopls $gopls --sessions 1 --iterations 4 --json (Join-Path $cargoOut 'pre-pr2-graph-refresh.json')
python -B .\tools\compare_hover_and_signature.py $server --workspace $localchat --fixture (Join-Path $fixture 'localchat-goland-hover-signature.json') --go-bin $goSdkBin --goland-bench (Join-Path $baselineDir 'localchat-hover-signature-bench.json') --sessions 1 --iterations 2 --json (Join-Path $cargoOut 'pre-pr2-hover-signature.json')
python -B .\tools\compare_symbols.py $server --workspace $localchat --fixture (Join-Path $fixture 'localchat-goland-symbols.json') --goland $goland --goland-bench (Join-Path $baselineDir 'localchat-symbols-bench.json') --startup-runs 1 --json (Join-Path $cargoOut 'pre-pr2-localchat-symbols.json')
python -B .\tools\compare_symbols.py $server --workspace $symbolFixture --fixture (Join-Path $symbolFixture 'goland-symbols.json') --goland $goland --goland-bench (Join-Path $baselineDir 'fixture-symbols-bench.json') --startup-runs 1 --json (Join-Path $cargoOut 'pre-pr2-fixture-symbols.json')
~~~

各基准预期退出 0，核对现有报告结构、样本数和逐样本精确范围。图刷新每侧 4 次保存均得到匹配结果，检查 `retries` 分类和 watcher 握手。对运行中 GoLand 的导航性能复测按模板固定主机、构建号、SDK、项目修订、搜索范围、已打开文件及编辑序列，逐次核对完整结果集合和 UTF-16 范围。记录索引就绪后新查、重复和编辑后的 P50/P95、样本数、缓存、计时边界及 UI 条件，启动到首个正确结果和工作集按原采样口径记录。[导航性能对照](../PLANS.md#导航性能对照) · [GoLand 实现基线](../goland-implementation-baseline.md#与-goland-直接对照) · [GoLand 符号重放](../goland-symbols-baseline.md#重放)

按顺序在本地 `main` 创建两个提交。合入前分别以相同基点验证两套 Rust 源码，合入后以每次提交的实际文件清单核对交集。

~~~powershell
$base = git rev-parse 'HEAD~2'
if ($LASTEXITCODE -ne 0) { throw 'Could not determine the common base' }
$engineFiles = @(git show --name-only --pretty= 'HEAD~1')
$harnessFiles = @(git show --name-only --pretty= HEAD)
$overlap = @($engineFiles | Where-Object { $_ -in $harnessFiles })
if ($overlap.Count) { throw "Overlapping changes: $($overlap -join ', ')" }
~~~

## 完成记录

- 2026-11-26 已按提交 `779fe78` 的测试源码以及其后的协作说明 `33a00b4` 完成只读勘察。现有 16 个 Python 入口均通过语法、导入和 `--help` 检查。现有工作区清单确认为 23 项，未知用例以及 `--timeout nan/0/inf` 返回 2。已对照 Pre-PR 1 草案的路径及现有 Rust 测试接线。
- 2026-11-26 用户确认本计划，开始 Python 测试夹具和工作区检查模块化重构。
- 2026-11-26 实施前从当前工作区采集 Python 基线。23 项用例顺序与上表一致。16 个入口的 `--help` 原始输出及退出码均已保存。缺少候选程序、未知用例、未知选项、可执行文件不存在，以及超时值 `0`、`-1`、`nan`、`inf`、`abc` 共 9 种参数错误均退出 2。采集文件为 `%TEMP%\rs-gopls-pre-pr2-2026-11-26-python-baseline.json`。
- 2026-11-26 完成 Python 实施步骤 1–4，改动上表中的 16 个 Python 文件和本计划。已核对 18 项原用例的函数体，另外 5 项沿用原断言并改用 `PausedGoList`。
- 两条独立 `unittest` 命令分别通过 15 项和 10 项，退出码均为 0。全部 27 个 Python 文件的字节码编译及 16 个入口的导入和 `--help` 校验退出 0。15 份帮助输出逐字节一致，工作区入口的差异仅为新增 `--module`。9 项原参数错误的退出码和诊断一致，用法同样只增加 `--module`。`compare_definitions`、`check_definitions` 和 `check_workspace_updates` 的兼容助手均通过对象身份断言。
- 使用 Go `go1.24.3 windows/amd64` 独立验证转发器，退出码为 0。非 `list` 命令正常转发，5 次 `list` 调用覆盖两次连续暂停、真实 Go 错误、注入失败及恢复。

### 最终正确性验证

- `TestSession` 统一内存连接、请求与通知构造、Go 文档打开及全量替换、整数与字符串 ID 的原始响应核对和空队列断言。符号测试使用本地派发适配调用生产入口，并显式推进加载及搜索。三个符号请求测试、三个文档符号测试和五个工作区符号测试均通过。
- `cargo check -p rs-gopls --locked`、`cargo check --workspace --locked`、`cargo test --workspace --locked` 和 `cargo build -p rs-gopls --release --locked` 均退出 0，零告警。工作区通过 52 项，Zed 扩展目标 0 项。最终 `rs-gopls.exe` 的 SHA-256 为 `AD694D0B033D8AC0CA39D3C6E94A3E013D230FC22C7631570425149A3BA5FEDA`。
- 两条 Python `unittest` 命令分别通过 15 和 10 项，均退出 0。27 个 Python 文件编译、16 个入口导入和 `--help` 校验通过。15 份帮助输出逐字节一致，工作区入口的帮助及用法只增加 `--module`。保存的 9 个失败调用均保持预期退出码与诊断，共用助手的重导出通过对象身份核对。
- 回归使用 PATH 首位的 `go1.27.4 windows/amd64` 和 `GOTOOLCHAIN=local`。默认工作区回归覆盖真实 `go list` 转发、暂停与放行、一次失败注入及恢复、代次替换、合并、取消和退出清理。历史回归 13/13 条退出 0，覆盖阶段 6 声明的 124 个合成位置和 16 个项目锚点、20 项 GoLand 声明、58 项类型定义、34 组引用、49 项实现、35 项 Hover、32 项 SignatureHelp，以及 LocalChat 和合成项目的大纲及 56 条工作区符号查询。[历史回归逐命令结果](2026-11-26-pre-pr-engine-dispatch-decoupling.md#最终构建与历史回归)

| `check_workspace_updates.py` 参数 | 结果 | 退出码 |
| --- | --- | ---: |
| 默认执行 | 23/23，顺序与基线一致 | 0 |
| `--case type-overlay --case test-packages --case type-overlay` | `type-overlay`、`test-packages`，按首见顺序 | 0 |
| `--module symbols` | `symbol-updates`、`symbol-waiting-for-graph`、`symbol-active-search` | 0 |
| `--module graph_refresh --case go-work` | `go-work` | 0 |
| `--module symbols --case go-work` | 空交集参数错误 | 2 |
| `--timeout nan` | 超时值必须为有限正数 | 2 |

### 基准命令结果

使用同一最终发布版、`gopls v0.24.0`、GoLand `GO-263.7307.354` 和 LocalChat 修订 `532228d3d36bd9f0fffb4bda35c180485b2eadd6`。原有九条基准均退出 0，两个符号基准复用同一发布版的三方验证产物。文本报告含首答及编辑后分位数，七份 JSON 报告核对了原字段、样本数量和完整结果校验。计时中的源文档变更均为缓冲区编辑或临时模块变更，LocalChat 的 `server` 路径在验证后仍干净。

| 命令 | 样本核对 | 报告 | 退出码 |
| --- | --- | --- | ---: |
| `compare_definitions.py --iterations 4` | 合成声明，每侧 1 次启动和 4 次编辑查询 | `c2-synthetic-definition.log` | 0 |
| `compare_definitions.py --stdlib --iterations 4` | 标准库 `net/http`，每侧 1 次启动和 4 次编辑查询 | `c2-stdlib-definition.log` | 0 |
| `compare_workspace_definitions.py --method definition` | 每侧 9/9，包括调用方编辑及依赖覆盖与恢复 | `pre-pr2-definitions.json` | 0 |
| `compare_workspace_definitions.py --method typeDefinition` | 每侧 9/9，包括调用方编辑及依赖覆盖与恢复 | `pre-pr2-types.json` | 0 |
| `compare_workspace_references.py` | 每侧 5/5 计时响应，9 个用例的 18/18 项预检 | `pre-pr2-references.json` | 0 |
| `compare_workspace_implementations.py` | 候选预检 20/20，和 gopls 共 15 个共同用例，每侧计时 9/9 | `pre-pr2-implementations.json` | 0 |
| `compare_first_implementations.py --case store-type` | 每侧 1 个启动首答，39 个完整目标 | `pre-pr2-first-implementations.json` | 0 |
| `compare_graph_refresh.py` | watcher 握手通过，每侧 4/4 保存得到精确目标，`retries` 均为 `{}` | `pre-pr2-graph-refresh.json` | 0 |
| `compare_hover_and_signature.py --iterations 2` | 18 个 Hover 和 13 个 SignatureHelp 共同用例，复查 36/26 次，编辑后各 8 次 | `pre-pr2-hover-signature.json` | 0 |
| `compare_symbols.py --startup-runs 1`（LocalChat） | 18/28 组共同样本，包括 4 组大纲、14 组工作区符号 | `final-after-localchat-symbols.json` | 0 |
| `compare_symbols.py --startup-runs 1`（合成工作区） | 30/41 组共同样本，包括 3 组大纲、27 组工作区符号，换序复测通过 | `final-reverse-after-fixture-symbols.json` | 0 |

七份 `pre-pr2-*.json` 保存在 Cargo 产物目录，符号报告保存在 `%TEMP%\rs-gopls-pre-pr-1`。命令日志及退出码汇总保存在 `%TEMP%\rs-gopls-pre-pr-validation-2026-11-26`。[三方性能数据](2026-11-26-pre-pr-engine-dispatch-decoupling.md#三方性能抽检)

### 独立验证与文件边界

共同基点为 `33a00b4af22d5092b39feb85a342e1566db2d9b7`。将两个 Rust 源码组合分别与该基点验证，沿用同一工作区的 Cargo 产物目录。Pre-PR 1 独立通过 52 项，Pre-PR 2 独立通过 39 项，两个组合的包检查及工作区测试均退出 0、零告警。恢复合并源码后再跑 `symbol_request_tests`，3/3 通过，并核对发布版 SHA-256 一致。

Pre-PR 1 共 18 个文件。

```text
docs/plans/2026-11-26-pre-pr-engine-dispatch-decoupling.md
src/dispatch.rs
src/main.rs
src/protocol.rs
src/scheduler.rs
src/semantic.rs
src/semantic/definition.rs
src/semantic/document_symbols.rs
src/semantic/hover.rs
src/semantic/implementation.rs
src/semantic/index.rs
src/semantic/query.rs
src/semantic/references.rs
src/semantic/resolver.rs
src/semantic/signature_help.rs
src/semantic/snapshot.rs
src/semantic/source.rs
src/semantic/workspace_symbols.rs
```

Pre-PR 2 共 19 个文件。

```text
docs/plans/2026-11-26-pre-pr-test-harness-decoupling.md
src/symbol_request_tests.rs
src/test_support.rs
tools/check_definitions.py
tools/check_goland_signature_help.py
tools/check_workspace_updates.py
tools/compare_definitions.py
tools/compare_graph_refresh.py
tools/lsp_harness.py
tools/test_lsp_harness.py
tools/test_workspace_checks.py
tools/workspace_checks/__init__.py
tools/workspace_checks/_assertions.py
tools/workspace_checks/definition.py
tools/workspace_checks/graph_refresh.py
tools/workspace_checks/hover_signature.py
tools/workspace_checks/implementation.py
tools/workspace_checks/references.py
tools/workspace_checks/symbols.py
```

两个提交按以上清单分别暂存。按 `git show --name-only --pretty= HEAD~1` 和 `git show --name-only --pretty= HEAD` 核对，文件交集为 0。
