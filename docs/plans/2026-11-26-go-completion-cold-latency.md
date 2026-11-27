# Go 上下文代码补全首次冷触发尾延迟优化（P0 + P1）

| 项目 | 内容 |
| --- | --- |
| 状态 | 已完成 |
| 创建日期 | 2026-11-26 |
| 用户确认 | 已确认（2026-11-27，确认按本计划实施 P0-1、P0-2、P1-1、P1-2 优化与验收目标）；2026-11-27 用户确认采纳选项 B，第 5 项按亚毫秒级统计容差口径（允许 ±0.15 ms 内波动，且同组 P95、真实项目首轮及全量 resolve P95 不退化）结项 |
| 更新日期 | 2026-11-27 |
| 完成日期 | 2026-11-27 |

## 目标与范围

以 `1c42031`（`1c42031d03018d50564880324dd7197aba391e3d`）为基线，消除包图就绪后首次触发自动导入搜索的尾延迟。合成夹具 `scope-local` 的 5 次冷态 P50 为 591.67 ms，上一轮基准为 622.66 ms，GoLand 为 113.35 ms。真实项目 `local-inferred` 分别为 1,622.86 ms、1,779.82 ms 和 551.33 ms。补全语义、完整候选集合、排序、插入与解析编辑均须与 Rust 基线零退化。[Track A 计划](2026-11-26-go-completion-context.md) · [GoLand 补全对照基线](../goland-completion-baseline.md#共同样本延迟)

1. **P0-1 · 消除 `resolved_candidate` 解析放大与重复读盘**。真实项目候选展开 935.04 ms，占端到端冷态诊断样本的 58.05%，期间依赖包额外全量解析 396 次，64 个已有声明索引的文件重开，其中 52 个文件有第三次磁盘读取。优先从 `FileSymbols` / `Symbol` 读取轻量展示和声明摘要，按请求有界复用仍需语法树的源。为类型种类分类（199.13 ms、101 次解析）与预期类型兼容打分（338.55 ms、218 次解析）引入快照级记忆化，优先查询包级轻量声明索引。
2. **P0-2 · 包图就绪后台预热 SDK 清单并复用 `GOROOT`**。合成样本同步定位 SDK 根和等待清单合计 480.44 ms（81.14%），真实项目同步定位 SDK 根为 32.59 ms。复用工作区已有的 `GOROOT` 探测结果，在工作区初始化加载或安装包图时启动后台 `go list std`，按 SDK 路径和构建配置复用清单。首个自动导入查询须命中清单。包图发布、导航和服务退出可在清单任务运行时独立完成。
3. **P1-1 · 工作区与 vendor 磁盘文件接入导出名预筛选**。真实项目的工作区路径未筛选直接建索引耗时 108.73 ms、全量解析 85 次。对活动包图内关闭的工作区与 vendor `.go` 文件使用 `sdk_names` 字节词法扫描，已打开或未保存的缓冲区直接使用内存 `IndexedSource` / `FileIndex`。按文件内容与包图变化精确失效。
4. **P1-2 · 有界并行预筛选并复用命中文本**。合成与真实项目的现有串行预筛选分别耗时 97.90 ms 和 150.65 ms。以标准库 `std::thread::scope` 分批并行读盘和扫描，按确定的路径顺序合并，保留 `-32800` 取消语义。命中文件直接将同一次读取的文本交给声明索引，消除预筛选后的第二次读盘。

### 变更边界与非目标

- 缓存限于进程内。P2 跨进程持久化磁盘 Stub 缓存属于后续阶段。
- 同一次 LSP 补全响应返回全部合法候选及客户端能力要求的编辑。沿用 Track A 的 GoLand 对拍口径、排序和 `completionItem/resolve` 契约。
- 实现使用标准库和仓库现有 crate，沿用现有 `Cargo.lock`。

## 验收标准

- [x] 合成夹具 `scope-local` 在同一主机完成 5 次独立冷启动，首个自动导入查询的中位数 ≤ 120 ms（基线 591.67 ms，GoLand 113.35 ms）。预热态同序重放 P50/P95 无退化。
- [x] 真实项目 `local-inferred` 在同一主机完成 5 次独立冷启动，中位数 ≤ 550 ms（基线 1,622.86 ms，GoLand 551.33 ms）。预热态同序重放 P50/P95 无退化，保留既有候选差异的对照记录。
- [x] 冷态诊断确认首次自动导入命中 SDK 清单，并复用 `GOROOT`。已有 `FileIndex` 的候选展示路径的二、三次读取与重解析归零，类型分类和打分复用索引与记忆化结果，工作区和 vendor 的可筛选磁盘文件先预筛选，命中文本直接建索引。请求结束、取消或失效后释放补全临时文本与 AST，记录缓存数量、峰值和释放结果。
- [x] `check_goland_completion.py` 的真实项目 22/22（13 一致、9 既有有意差异、0 失败）与合成夹具 126/126（58 一致、68 既有有意差异、0 失败）逐查询保持 `1c42031` 判定。两版原始响应的候选内容、顺序、UTF-16 主编辑 / 附加编辑、解析结果和编辑后的文本一致。
- [x] 两套 `compare_completion.py` 的首查、复查、编辑后首查及适用的 resolve 共同样本按选项 B 验收。补全 P50 与共同 resolve P50/P95 允许 ±0.15 ms 内亚毫秒级统计波动，且同组补全 P95 及全部解析样本 P95 不退化。真实项目首轮无退化。保留样本数与既有差异，更新 [GoLand 补全对照基线](../goland-completion-baseline.md)。
- [x] `cargo check -p rs-gopls --locked`、`cargo test -p rs-gopls --locked`、`cargo build -p rs-gopls --release --locked` 零告警通过。新增单元和集成覆盖确认 SDK 预热复用与配置切换、工作区和 vendor 未保存缓冲区优先与预筛选失效、并行合并确定性、取消单次回包和后续查询恢复。
- [x] 全量历史回归通过。`check_workspace_updates.py` 历史 28/28、Python `unittest` 25/25、Stage 6 定义检查及全部 GoLand 定义、类型定义、引用、实现、Hover、SignatureHelp、文档大纲和工作区符号检查通过。

## 实现依据

### 冷态剖析

[target/track-a-cold-profile-1c42031.json](../../target/track-a-cold-profile-1c42031.json) 的 `clean.runs` 主样本每个工作区 5 次。每次新启 LSP 进程，打开采集指定文件，等待 `workspace/symbol` 图就绪，再按原首查顺序重放，保留正常系统和 Go 磁盘缓存。`scope-local` 在前 15 个查询后触发，位置为 `main.go` 的 UTF-16 零基 `18:11`，包含夹具对该行的未保存编辑。`local-inferred` 是首个补全查询，位置为 `internal/model/user.go` 的 `47:16`。两例分别返回 2 和 154 个候选，GoLand 分别为 2 和 284 个。真实项目记录 131 条既有差异明细。

取 `instrumented.runs` 中每个工作区端到端冷时延正好居中的主样本，再取其中首个请求的 `profile[0]`。下表耗时单位为 ms。父阶段的包含耗时与子阶段重叠。后台命令的时钟独立于请求，相同 SDK 清单在两例均列出 385 个包、1,765 个 Go 文件条目，经构建文件和可导入包过滤后为 356 个包、1,629 个文件。

| 实测阶段 | 合成 `scope-local` | 真实项目 `local-inferred` |
| --- | ---: | ---: |
| 发布版端到端冷态 P50 / 上一轮首查 / GoLand 首查 | 591.6660 / 622.6629 / 113.3461 | 1,622.8624 / 1,779.8217 / 551.3285 |
| 代表样本端到端 / 请求内 `total_ms` | 592.0962 / 590.6285 | 1,610.8698 / 1,607.7493 |
| 光标 / 预期类型初始化 / 本地候选 | 0.0350 / 0.0606 / 0.1326 | 0.0343 / 7.6477 / 10.5189 |
| 导入搜索（包含） / 包图目录准备 | 587.6163 / 2.4158 | 1,594.2182 / 10.8843 |
| `sdk_catalog`（含 `sdk_root`） | 33.9644（33.3901） | 33.0652（32.5945） |
| 等待清单的 `sdk_blocked_sleep` | 447.0517（199 次） | 0 |
| 后台 `go list std` / 清单解码 / 后台总耗时 | 499.2837 / 2.2599 / 501.5530 | 516.4011 / 1.8112 / 518.2190 |
| 图中 SDK / 模块缓存 / 补充 SDK 清单预筛选 | 50.8454 / 0 / 47.0564 | 77.0338 / 46.9550 / 26.6598 |
| 预筛选合计（读取文件数） | 97.9018（1,075） | 150.6486（1,504） |
| 预筛选读盘 / 字节词法扫描 / 名字匹配 | 62.3322 / 33.5837 / 0.3889 | 97.0764 / 50.0914 / 0.8098 |
| 工作区未预筛选索引（全量解析数） | 1.2505（17） | 108.7253（85） |
| 预筛选命中后的索引（另读并全量解析数） | 2.6897（1） | 351.4168（57） |
| 导入候选展开（包含） | 0.0140 | 935.0433 |
| 展开内的依赖候选展示 / 类型种类 / 符号类型 / 预期类型打分 | 0 / 0 / 0 / 0 | 384.6497 / 199.1292 / 9.6519 / 338.5544 |
| 排序与响应渲染（包含） | 1.1805 | 2.1863 |

合成样本的 480.4418 ms 由 `sdk_root` 与清单等待相加，分母为 592.0962 ms。真实项目的 396 次额外全量解析来自图中 SDK 151 次及模块缓存 245 次，按触发点分别为展示源重开 64 次、类型分类 101 次、符号类型推断 13 次、预期类型打分 218 次。部分类型追链会为依赖文件首次建索引。52 个第三次读取来自图中 SDK 24 个和模块缓存 28 个。本地候选阶段另有 3 次展示源读取。

### GoLand 安装包与索引证据

`<GOLAND_HOME>\build.txt` 为 `GO-263.7307.354`。已在该安装包的 `plugins/go-plugin/lib/modules/intellij.go.impl.jar` 复核 `intellij.go.impl.xml` 第 231 行的 `com.goide.stubs.index.GoAllPublicNamesIndex` 和第 398 行的 `com.goide.completion.GoAutoImportCompletionContributor`。JAR SHA-256 为 `2a13f1a753d5e15db70c5758e17db1dc28cc790e1d329e574d8b6a078d636784`，沿用 Track A 的 REA 证据 `ev_6c7c39a219d2e02923e7b819150326c434f3135777e22df0b1f7e49c6cdda5ee`。[Track A 产物证据](2026-11-26-go-completion-context.md#rea-产物证据)

`GoAutoImportCompletionContributor#collectAndSortAllPublicProductionNames` 通过 `StubIndex.processAllKeys` 枚举 `GoAllPublicNamesIndex.ALL_PUBLIC_NAMES` 的键，并按限定名与导出名匹配。内部补全 provider 给查找范围加上 `ExceptTestsScope`，按匹配键调用 `StubIndex.getElements`。该索引键为 `go.all.name`，本机 GoLand 系统目录下可检查 `%LOCALAPPDATA%\JetBrains\GoLand2026.3\index\stubs\go.all.name`。Rust 采用同样的先按名字缩小查找、再以声明核实的处理顺序，复用内存索引和活动构建视图。[GoLand 差异依据](../goland-completion-baseline.md#差异依据)

### Rust 基线路径

下表源码行号均以 `1c42031d03018d50564880324dd7197aba391e3d` 为准。

| 基线路径 | 源码与行为 |
| --- | --- |
| P0-1 候选展开 | `Search::advance` 从 `file_index` 收集声明，随后为每个命中调用 `resolved_candidate`。后者通过 `reference_source` 取得跨文件展示源并执行 `describe`，接着分别计算底层类型、字面量插入、符号类型及预期类型权重。[imports.rs:480–522](../../src/semantic/completion/imports.rs#L480-L522) · [candidates.rs:886–1037](../../src/semantic/completion/candidates.rs#L886-L1037) |
| P0-1 声明索引与 AST | `FileIndex::new` 保存符号、导入与包签名。`IndexedSource::declarations` 为打开的文件惰性建立索引。磁盘 `file_index` 首次读取并解析后只存 `DiskIndex`，初始 `reference_source` 为空。展示路径首次请求它时重新读盘并解析，随后将 `Rc<IndexedSource>` 保存在磁盘索引条目中。`Resolver::type_info` 逐文件查找 `type_details`，可能触发更多冷建索引。[index.rs:18–109](../../src/semantic/index.rs#L18-L109) · [index.rs:251–285](../../src/semantic/index.rs#L251-L285) · [index.rs:302–315](../../src/semantic/index.rs#L302-L315) · [resolver.rs:1037–1097](../../src/semantic/resolver.rs#L1037-L1097) · [expected.rs:71–112](../../src/semantic/completion/expected.rs#L71-L112) |
| P0-2 SDK 根、清单与调度 | `Workspace::load` 请求 `Goroot` 与目录元数据，`read_packages` 把 `Goroot` 标记为包的标准库属性。`builtin_source` 在单个 `Workspace` 的 `OnceCell` 首访同步调用 `go env GOROOT`。`ImportCache::prepare` 在补全代次变化时把清单置为 `Unloaded`，首次 `sdk` 调用定位根、启动后台 `go list -e -json=Dir,ImportPath,Name,GoFiles,CgoFiles std`，目录按规范化 SDK 源路径及 `VERSION` 修改时间识别。导入队列空而清单未就绪时休眠 2 ms，再由调度器推进。当前加载线程交付包图后，调度器安装包图并重跑等待请求。[workspace.rs:43–71](../../src/workspace.rs#L43-L71) · [workspace.rs:110–159](../../src/workspace.rs#L110-L159) · [workspace.rs:304–325](../../src/workspace.rs#L304-L325) · [workspace.rs:365–417](../../src/workspace.rs#L365-L417) · [imports.rs:64–151](../../src/semantic/completion/imports.rs#L64-L151) · [imports.rs:564–571](../../src/semantic/completion/imports.rs#L564-L571) · [imports.rs:825–896](../../src/semantic/completion/imports.rs#L825-L896) · [scheduler.rs:67–115](../../src/scheduler.rs#L67-L115) |
| P1 磁盘预筛选与二次读取 | 图中包的 `prefilter_disk` 要求包加载成功且位于外部符号路径，标准库和模块缓存满足该来源条件。打开文件绕过磁盘预筛选。词法扫描枚举顶层导出声明并处理分组、注释和字面量，`disk_file_matches` 只缓存名字。命中后图路径由 `file_index`、清单路径由 `ImportCache::exports` 重新读取并解析尚未索引的文件。图路径使用包图及草稿文件，排除测试文件。SDK 清单专用的 `is_symbol_source` 会排除 vendor，因此 vendor 筛选须接在图路径上。[imports.rs:153–191](../../src/semantic/completion/imports.rs#L153-L191) · [imports.rs:206–227](../../src/semantic/completion/imports.rs#L206-L227) · [imports.rs:460–554](../../src/semantic/completion/imports.rs#L460-L554) · [imports.rs:585–600](../../src/semantic/completion/imports.rs#L585-L600) · [sdk_names.rs:6–75](../../src/semantic/completion/sdk_names.rs#L6-L75) · [sdk_names.rs:90–157](../../src/semantic/completion/sdk_names.rs#L90-L157) · [workspace.rs:205–216](../../src/workspace.rs#L205-L216) · [workspace.rs:434–442](../../src/workspace.rs#L434-L442) |
| 快照、取消与文件变化 | 初始收集计算 `Expected`，需要导入搜索时建立携带文档版本与包图代次的 `CompletionTask`，每批推进时重建借用快照并复核，resolve 还检查文档指纹。调度器在推进及发布前核对包图代次，收到取消或内容变化时移除对应任务并回包。编辑使打开源的惰性索引失效，磁盘事件调用 `ImportCache::source_changed` 并更新文件或包图。[candidates.rs:130–140](../../src/semantic/completion/candidates.rs#L130-L140) · [mod.rs:190–210](../../src/semantic/completion/mod.rs#L190-L210) · [mod.rs:238–249](../../src/semantic/completion/mod.rs#L238-L249) · [mod.rs:332–378](../../src/semantic/completion/mod.rs#L332-L378) · [scheduler.rs:179–284](../../src/scheduler.rs#L179-L284) · [index.rs:70–85](../../src/semantic/index.rs#L70-L85) · [semantic.rs:236–297](../../src/semantic.rs#L236-L297) |

### Rust 方案与待验证问题

1. **P0-1**。首次从语法树构造 `FileIndex` 时记录与 `describe` 等价的声明展示摘要，包含声明种类、原签名与标签尾缀、形参存在性、弃用标记和目标范围。候选按客户端能力生成相同的详情、标签、snippet 与编辑。签名特化、推断初始化器、首响应文档与 resolve 仍走现有语义路径，优先使用打开源或本请求同内容的已解析源。索引后紧接的候选展开共享同一解析结果。补全读取磁盘 AST 时只进入请求缓存，导航沿用自己的 `reference_source` 路径。上限初设为 8 份源和 16 MiB 源文本，超预算单个文件临时处理，完成、取消或过期时释放。常驻条目仅补充声明与展示摘要。[render.rs:93–159](../../src/semantic/completion/render.rs#L93-L159) · [render.rs:214–235](../../src/semantic/completion/render.rs#L214-L235) · [mod.rs:251–285](../../src/semantic/completion/mod.rs#L251-L285)

   按包视图从 `FileIndex` 聚合类型声明位置、`TypeDetails`、导入映射和成员摘要，类型追链可查到未导出声明。查询优先复用已有索引，缺项保留原解析路径。将拥有数据的记忆化结果从本地收集带到同一 `CompletionTask` 的各批，键包含包图代次、包身份、声明文件与范围、缓冲区版本或磁盘内容身份，以及指针、泛型实参与接收者上下文。兼容打分同时区分完整 `Expected`（精确及底层类型、接口方法集、nil 与类型断言）和调用签名与单结果类型。沿用方法集、别名、泛型代入、数值族和未类型化值规则。未完成的递归与 I/O 错误可重新求值。
2. **P0-2**。由工作区共用有效 `GOROOT` 与 builtin 源路径。优先复用已得到的探测值，图加载可从可信标准库包的 `Goroot` 标记、目录和导入路径确认 SDK 根。图数据不足时由后台在同一工作目录和构建环境解析有效根。初始化尽早启动或复用清单任务，包图安装时接入同键任务或补发。清单键包含规范化 SDK 路径、SDK 版本或 `VERSION` 指纹、有效 Go 工具链与构建配置（目标 OS/架构及其子架构、CGO、构建标签、`GOFLAGS` 和影响选包的工具链环境）。普通包图换代复用同键进行中或就绪的清单，配置换代使旧结果失效。每个活动配置单飞，后台通过通道发布完整清单。查询期间若清单仍在加载，挂起完整搜索并让出调度，导航继续推进。加载失败允许在相关配置变化或受控重试时恢复。退出时撤销任务并交由后台收尾，主线程无须 join 清单任务。

   **时序验证**。合成代表样本的包图就绪探针为 149.10 ms，后台 SDK 清单为 501.55 ms。仅在包图安装时启动可能赶不上第一个自动导入查询。优先验证从初始化或首个包图加载起点并行预热。分别记录图就绪、清单完成、首个自动导入到达和候选返回时刻，另测紧接就绪探针发出的自动导入。验收仍从原就绪探针后按原查询顺序计时。若提前预热仍无法达标，在同一计划记录实际时序与阻塞原因，涉及范围或验收调整时重新确认。
3. **P1-1**。复用 `sdk_names::exported_names` 及原 `SymbolMatcher` 筛选活动包图选中的工作区与 vendor 磁盘文件。构建约束、生产/测试视图、`internal`、vendor 遮蔽和导入别名沿用原包图和绑定判定。文件已打开时始终从内存索引取声明。词法扫描只作保守否决，覆盖 Unicode 与分组声明，损坏或无法判定的内容保留容错解析。磁盘导出名按 `path_key` 和文件内容身份保存，保存、监听变更、删除和关闭后发现磁盘内容变化时只更新受影响路径。`didChange` 以缓冲区版本使内存索引失效。包图换代更新文件选择和包级索引，内容身份相同的磁盘名字可复用。SDK 换代清理旧 SDK 条目。
4. **P1-2**。沿用 `BATCH_SIZE = 8` 的调度粒度。由标准库后台协调线程在 `std::thread::scope` 内并行处理一个批次，同时运行的扫描线程总数至多 `min(available_parallelism, 4)`。全进程最多一个待消费批次。批次开始前先复用打开文件与已有摘要。工作线程只读磁盘与扫描字节，返回路径、扫描结论及命中或需容错的文本。调度侧按规范化路径稳定归并，再按基线包与文件消费顺序进入语义解析，保留同名声明的首次命中选择。解析器、`Rc` 和索引更新留在原线程。预筛选缓存正命中但尚无 `FileIndex` 时直接读取并解析一次。待消费批次以 8 份源文本、16 MiB 为初始上限，超预算的单文件单独处理以保留合法候选。每批提交前核对源版本与包图代次。主循环可在批次进行时处理取消，丢弃已取消或过期的结果，在文件间检查取消，按请求 ID 单次回送 `-32800`，快照过期仍为 `-32801`。[imports.rs:36](../../src/semantic/completion/imports.rs#L36) · [main.rs:105–155](../../src/main.rs#L105-L155)

## 实施步骤

1. **记录确认并保存基线**。收到用户明确确认后记录日期与覆盖范围，置为「进行中」。保留同一 Cargo 产物目录中 `1c42031` 的发布版和对照 JSON，核对 SHA-256、样本顺序与 SDK 环境。
2. **实现 P0-2**。共享工作区 SDK 根，从初始化或包图加载起点接入清单预热并按有效配置复用。验证同键重复加载、配置切换、清单失败和退出，以及索引就绪后的冷触发时序。
3. **实现 P0-1**。让候选展示消耗索引摘要，复用本请求必要的解析源，补齐类型与预期类型记忆化。用解析计数核对候选展开，对照种类、标签、权重、插入与 resolve。
4. **实现 P1-1**。把保守预筛选接入工作区和 vendor 磁盘路径。覆盖分组和 Unicode 声明、未保存文件、关闭恢复、文件变更、构建视图及包图刷新。
5. **实现 P1-2**。分批并行预筛选，消费命中文本，验证跨会话顺序、同分与同名项、读盘次数、请求取消和并发上限。
6. **完成对拍、性能和历史回归**。使用下列命令核对两套对拍，分别重放五次冷态与诊断，跑完整 `compare_completion.py` 与全量历史回归。将实测结果写入 GoLand 补全基线和本计划，逐项勾选，通过全部验收后填写完成日期。

## 验证计划

### 固定条件与构建

在同一 Windows 主机用 PowerShell 7 从仓库根目录执行。固定 GoLand `GO-263.7307.354`、两版 rs-gopls 提交与二进制 SHA-256、`go1.27.4 windows/amd64`、`CGO_ENABLED=1`、`GOWORK=off`、空 `GOFLAGS` 及构建标签，维持夹具的 vendor 视图。真实项目固定 `532228d3d36bd9f0fffb4bda35c180485b2eadd6` 和干净源码。两侧沿用样本的打开文件、BASIC 补全、搜索范围、光标、UTF-16 标识符范围及未保存编辑序列。GoLand 使用已有运行态采集，需要重采时按现有基线的步骤复放两套样本。[导航性能对照](../PLANS.md#导航性能对照) · [GoLand 采集与重放](../goland-completion-baseline.md#重放)

~~~powershell
$cargoOutput = (cargo metadata --format-version 1 --no-deps --locked | ConvertFrom-Json).target_directory
$server = Join-Path $cargoOutput 'release\rs-gopls.exe'
$baselineServer = Join-Path $cargoOutput 'rs-gopls-1c42031-cold-baseline.exe'
$profileReference = (Resolve-Path '.\target\track-a-cold-profile-1c42031.json').Path
$external = $env:RS_GOPLS_BENCH_WORKSPACE
$fixture = (Resolve-Path '.\tools\fixtures\completion').Path
$symbolFixture = (Resolve-Path '.\tools\fixtures\symbol-navigation').Path
$golandBaseline = Join-Path $env:TEMP 'rs-gopls-goland-baseline'
$env:GOTOOLCHAIN = 'go1.27.4'
$goSdk = go env GOROOT
$env:PATH = "$(Join-Path $goSdk 'bin');$env:PATH"
$env:GOROOT = $goSdk
$env:GOTOOLCHAIN = 'local'
$env:GOOS = 'windows'
$env:GOARCH = 'amd64'
$env:CGO_ENABLED = '1'
$env:GOWORK = 'off'
$env:GOFLAGS = ''
$env:PYTHONPATH = (Resolve-Path '.\tools').Path
go version
go env GOROOT GOOS GOARCH CGO_ENABLED GOWORK GOFLAGS
git -C $external rev-parse HEAD
git -C $external status --short --untracked-files=all -- .
~~~

确认后、源码变更前，哈希核对当前 `1c42031` 发布版并用 `Copy-Item -LiteralPath $server -Destination $baselineServer` 保存。当前重建版的哈希对应剖析记录的 `clean_after_rebuild.exe_sha256`。需要重建基线时，在该提交执行同一套 Cargo 命令，再保存二进制。开发后用原 toolchain、profile、features、target 与环境配置的 Cargo 产物目录执行。

~~~powershell
cargo check -p rs-gopls --locked
cargo test -p rs-gopls --locked
cargo build -p rs-gopls --release --locked
Get-FileHash -Algorithm SHA256 -LiteralPath $baselineServer, $server
~~~

### 冷态重放、诊断与逐项比较

冷态每例新启进程 5 次。使用 `warm_package_graph` 等到含测试视图的包图就绪，立即复放 JSON 中的先导查询和目标，每例在相同缓冲区复查 3 次。发布版计时从发送请求到完整 stdio 响应解码，包图探针、编辑、对拍与 resolve 另计。对照数据预热池化为每例 15 次，合成与真实项目的 P50/P95 分别为 2.4444 / 3.9785 ms 与 16.4446 / 23.8323 ms。诊断模式由实现提供可选 `RS_GOPLS_COMPLETION_PROFILE` 文件输出，记录首查阶段耗时、SDK 任务状态与时间戳、读盘和解析计数、缓存水位及释放。诊断结果独立于发布版时延验收。对照模式通过同一 harness 比较两版每个查询的完整候选与接受编辑结果。

~~~powershell
$coldReplay = @'
import hashlib
import os
import json
import sys
from pathlib import Path
from time import perf_counter

from check_goland_completion import (
    CompletionCorpus, ExpectedCompletion, OpenBuffers, completion_items,
    completion_session, warm_package_graph,
)
from check_goland_symbols import go_environment
from compare_completion import summarize

server, before, reference, output = map(Path, sys.argv[1:5])
mode = sys.argv[5]
profile = json.loads(reference.read_text(encoding="utf-8-sig"))
timeout = 90.0

def replay(program, corpus, environment, ids, repeat_last=0):
    results = {}
    with completion_session(str(program), corpus.root, timeout, environment=environment) as (client, caps):
        assert caps.get("completionProvider")
        buffers = OpenBuffers(client, corpus)
        started = perf_counter()
        warm_package_graph(client)
        probe_ms = (perf_counter() - started) * 1000
        for query_id in ids:
            query = corpus.queries[query_id]
            expected = ExpectedCompletion(corpus, query)
            with buffers.replay(query):
                repeats = 1 + repeat_last if query_id == ids[-1] else 1
                samples = [expected.request(client) for _ in range(repeats)]
            for sample in samples:
                assert not sample.failures, (query_id, sample.failures)
            results[query_id] = samples
    return probe_ms, results

report = {}
for project, capture, target in (
    ("fixture", "goland-completion.json", "scope-local"),
    (next(key for key in profile["clean"]["environment"] if key != "fixture"), "external-goland-completion.json", "local-inferred"),
):
    root = Path("tools/fixtures/completion") if project == "fixture" else Path(os.environ["RS_GOPLS_BENCH_WORKSPACE"])
    corpus = CompletionCorpus(root, Path("tools/fixtures/completion") / capture)
    environment, version = go_environment(corpus, timeout)
    corpus.load_active_packages(environment, timeout)
    environment["RS_GOPLS_COMPLETION_PROFILE"] = ""
    if mode == "parity":
        ids = list(corpus.queries)
        _, old = replay(before, corpus, environment, ids)
        _, new = replay(server, corpus, environment, ids)
        for query_id in ids:
            assert old[query_id][0].rs_gopls == new[query_id][0].rs_gopls, query_id
            assert old[query_id][0].intentional_divergences == new[query_id][0].intentional_divergences, query_id
        report[project] = {"verified_queries": len(ids), "go_version": version}
        continue
    recorded = next(run for run in profile["clean"]["runs"] if run["project"] == project and not run["standalone"])
    ids = [entry[0] for entry in recorded["prior_queries"]] + [target]
    assert corpus.queries[target].point == tuple(recorded["query"]["point"])
    runs = []
    for index in range(1, 6):
        trace = output.with_name(f"{output.stem}-{project}-{index}.ndjson")
        if mode == "diagnostic":
            environment["RS_GOPLS_COMPLETION_PROFILE"] = str(trace)
        probe_ms, result = replay(server, corpus, environment, ids, repeat_last=3)
        cold, *warm = result[target]
        for sample in (cold, *warm):
            items = completion_items(sample.rs_gopls["completion"])
            digest = hashlib.sha256(json.dumps(items, sort_keys=True).encode("utf-8")).hexdigest()
            assert digest == recorded["items_sha256"], (project, index, digest)
            assert len(sample.intentional_divergences) == recorded["divergences"], project
        if mode == "diagnostic":
            assert trace.is_file(), trace
        runs.append({"probe_ms": probe_ms, "cold_ms": cold.completion_ms,
                     "warm_ms": [sample.completion_ms for sample in warm],
                     "trace": str(trace) if mode == "diagnostic" else None})
    report[project] = {
        "go_version": version, "query": target, "runs": runs,
        "cold": summarize([run["cold_ms"] for run in runs]),
        "warm": summarize([ms for run in runs for ms in run["warm_ms"]]),
    }
with server.open("rb") as executable:
    report["server_sha256"] = hashlib.file_digest(executable, "sha256").hexdigest()
output.write_text(json.dumps(report, ensure_ascii=False, indent=2) + "\n", encoding="utf-8")
print(output)
'@

$runStamp = Get-Date -Format 'yyyyMMdd-HHmmss-fff'
$coldReplay | python -B - $server $baselineServer $profileReference (Join-Path $cargoOutput "track-a-cold-$runStamp.json") timing
$coldReplay | python -B - $server $baselineServer $profileReference (Join-Path $cargoOutput "track-a-cold-diagnostic-$runStamp.json") diagnostic
$coldReplay | python -B - $server $baselineServer $profileReference (Join-Path $cargoOutput "track-a-full-parity-$runStamp.json") parity
~~~

### GoLand 对拍与全量基准

对拍保留逐查询一致、既有差异和失败明细。全量基准使用两套 GoLand 原始计时和同序 stdio 重放，依次完成全部首查、5 次预热后 30 次复查、相同未保存编辑后的首查。共同样本须通过候选、顺序、范围及编辑检查。独立报告 resolve（包括真实项目无共同 resolve 样本时的全部通过解析样本），以及 `local-inferred` 的既有差异。记录 GoLand 从发起 BASIC 到排序与候选采集完成的耗时，弹窗绘制、接受和编辑在计时窗外。记录两侧 P50/P95、样本数、索引与缓存状态、计时边界。若采集工作集，注明 Windows 进程树与脚本的 50 ms 采样间隔。另在 SDK 预热进行时验证导航仍可返回正确目标与标识符范围，并记录从启动到双方首次正确结果的时间及对应计时边界。[导航性能对照](../PLANS.md#导航性能对照)

~~~powershell
$externalResults = Join-Path $fixture 'external-goland-completion.json'
$fixtureResults = Join-Path $fixture 'goland-completion.json'
$externalBench = Join-Path $golandBaseline 'external-completion-bench.json'
$fixtureBench = Join-Path $golandBaseline 'fixture-completion-bench.json'

python -B .\tools\check_goland_completion.py $server --workspace $external --fixture $externalResults --json (Join-Path $cargoOutput 'external-completion-correctness-cold-latency.json')
python -B .\tools\check_goland_completion.py $server --workspace $fixture --fixture $fixtureResults --json (Join-Path $cargoOutput 'fixture-completion-correctness-cold-latency.json')

python -B .\tools\compare_completion.py $baselineServer --workspace $external --fixture $externalResults --goland-bench $externalBench --json (Join-Path $cargoOutput 'external-completion-benchmark-1c42031.json')
python -B .\tools\compare_completion.py $baselineServer --workspace $fixture --fixture $fixtureResults --goland-bench $fixtureBench --json (Join-Path $cargoOutput 'fixture-completion-benchmark-1c42031.json')
python -B .\tools\compare_completion.py $server --workspace $external --fixture $externalResults --goland-bench $externalBench --json (Join-Path $cargoOutput 'external-completion-benchmark-cold-latency.json')
python -B .\tools\compare_completion.py $server --workspace $fixture --fixture $fixtureResults --goland-bench $fixtureBench --json (Join-Path $cargoOutput 'fixture-completion-benchmark-cold-latency.json')
~~~

### 历史回归

~~~powershell
python -B .\tools\check_workspace_updates.py $server
python -B -m unittest tools/test_lsp_harness.py tools/test_workspace_checks.py
python -B .\tools\check_definitions.py $server --stage 6 --workspace $external
python -B .\tools\check_goland_definitions.py $server --workspace $external --method definition
python -B .\tools\check_goland_definitions.py $server --workspace $external --method typeDefinition
python -B .\tools\check_goland_references.py $server --workspace $external
python -B .\tools\check_goland_implementations.py $server --workspace $external
python -B .\tools\check_goland_hover.py $server --workspace $external
python -B .\tools\check_goland_signature_help.py $server --workspace $external
python -B .\tools\check_goland_symbols.py $server --workspace $external --fixture (Get-Item .\tools\fixtures\*-goland-symbols.json).FullName
python -B .\tools\check_goland_symbols.py $server --workspace $symbolFixture --fixture (Join-Path $symbolFixture 'goland-symbols.json')
Push-Location -LiteralPath $fixture
try {
    go list -mod=vendor -test ./...
    go test -mod=vendor ./...
} finally {
    Pop-Location
}
git -C $external status --short --untracked-files=all -- .
~~~

## 完成记录

2026-11-27 核对源码 `HEAD` 为 `1c42031d03018d50564880324dd7197aba391e3d`。将现有发布版保存为 `target/rs-gopls-1c42031-cold-baseline.exe`，两份文件的 SHA-256 都为 `7effac3491b38afebdee30f4d38beeea37143d30ee4e5b6e79ca85238bfad3bf`，与剖析记录的重建版哈希一致。

### Wave A 实施记录

- P0-2 在初始化后启动 SDK 预热。包图从 `Goroot` 包目录确认 SDK 根，后台获取有效构建配置，并用选定 SDK 的 Go 可执行文件取得工具、发布版本及用户构建标签。清单按规范化根、`VERSION` 指纹和构建配置复用。原生目录遍历同时预筛选 SDK 导出名，遇到覆盖层或无法确定的 SDK 文件选择时回退到 `go list std`。复核了 GoLand `GO-263.7307.354` 的 JAR 哈希、索引注册与贡献者类，沿用上文的 REA 记录。
- P0-1 在 `FileIndex` 中保存声明展示、签名、参数类型、GoDoc、选择范围及类型追链所需的轻量数据。确定边界的只读依赖在主线程解析声明时跳过函数体和大型有类型的复合字面量内容，保留原始 UTF-16 范围。语法不确定时使用完整解析。已打开文件直接使用内存语法树。补全请求最多保留 8 份源和 16 MiB，包含需要保留的已关闭主文档。类型位置和分类随请求代次释放。推断权重以源身份和完整预期类型为键，在相关包变化时失效。
- P1-1 与 P1-2 在活动包图的关闭文件上预筛选。后台线程读取文件并扫描声明名，主线程按路径合并结果并按原文件顺序解析。命中文本随批次交付。在同包查找中，跳过的文件仅在其名字可能遮蔽导入限定名时核实声明。保存、关闭、监听事件和包图刷新更新受影响缓存。历史取消检查通过同一 stdio 批次发送活动查询、取消和修改，实际扫描中的取消及缓存释放由 Rust 测试覆盖。

### Wave B 验收记录

2026-11-27 完成验证，按用户确认的选项 B 验收，7/7 项通过。合成夹具的共同样本首查 P50 和首查后共同 resolve P95 在三轮均高于 `1c42031`，升幅约 +0.02–0.14 ms。各轮观察到的升幅均在 ±0.15 ms 亚毫秒级统计容差内。同组补全 P95、真实项目首轮及全部解析样本 P95 均无退化。

#### 环境与构建

同一 Windows 主机使用 PowerShell 7.7.0 和 Cargo 配置的 `<cargo-output>`。固定 `go1.27.4 windows/amd64`、`GOTOOLCHAIN=local`、`CGO_ENABLED=1`、`GOWORK=off`、空 `GOFLAGS`、`GOAMD64=v1` 和夹具的 vendor 视图。真实项目为 `532228d3d36bd9f0fffb4bda35c180485b2eadd6`。启动验证前无残留 rs-gopls 或 Python 进程。首轮性能重放前的系统空闲计数为 95%–96%。反序与第三轮开跑前未发现基准或构建进程，总 CPU 忙碌计数为 0%–5%。

| 文件 | SHA-256 |
| --- | --- |
| `target/rs-gopls-1c42031-cold-baseline.exe` | `7effac3491b38afebdee30f4d38beeea37143d30ee4e5b6e79ca85238bfad3bf` |
| `target/release/rs-gopls.exe` | `38752603876b8834d7a8372d6e759158bf3950a7ab21b94200f99f7f77fcbfe5` |
| GoLand `GO-263.7307.354` 真实项目计时 | `4ba9d4b68e437e7c43fccc3aaddc3d687b8a4ad4d9664a4185924155460b70e5` |
| GoLand `GO-263.7307.354` 合成计时 | `5b46b541a2942af755a4cd35691e24560d57cebe2774c8881584b48b950b0874` |

| 命令 | 结果 |
| --- | --- |
| `cargo check -p rs-gopls --locked` | 退出 0，零告警 |
| `cargo test -p rs-gopls --locked` | 119 通过，0 失败、忽略、筛除及告警 |
| `cargo build -p rs-gopls --release --locked` | 退出 0，零告警，发布版哈希与 Wave A 一致 |
| `check_goland_completion.py` 真实项目 | 22/22，13 一致、9 既有差异、0 失败 |
| `check_goland_completion.py` 合成夹具 | 126/126，58 一致、68 既有差异、0 失败 |
| 冷重放 `timing`、`diagnostic`、`parity` 与原始响应对拍 | 全部退出 0。两版 148 个补全及 27 个 resolve 响应逐字节相同，逐查询对拍判定和应用编辑后的文本相同。 |
| `compare_completion.py` 两版各两个工作区 | 四条命令退出 0，校验失败均为 0。反序复测两套工作区，合成夹具另测第三轮，样本数与逐样本归类一致。 |

原始响应按工作区合并的 SHA-256 分别为合成 `e74b4b665ca8f31fc954ab9912801f4db4f46a350e8f2d63a2447a85d4e5cd44` 和真实项目 `de6fa6f8e6681b1c6ca47e94c43218f0b5e5326c9632fe12c5509ef0e79d1f0b`。Wave A 的三份 `wave-a-delivery-*.json` 均对应当前 P0+P1 哈希。Wave B 独立重跑全部三种冷重放及原始响应对拍。旧 `wave-a-extended.ps1` 等重放辅助文件位于 Git 忽略的 `target/`。

#### 冷态与诊断

两版均用上表所列二进制各做 5 次独立冷启动，冷态后共采 15 次复查。计划最初的 591.6660 / 1,622.8624 ms 来自同提交较早构建 `dea74df1f6071c0f5ac84b8f44c6f42f7b574e4b1dfbfa60f8f131fbf4e1c388`。GoLand 为单次索引就绪首查及 30 次同序复查。所有时间为 ms。

| 查询 | GoLand 首查 | GoLand 复查 P50 / P95 | 基线冷态 P50 / P95 | P0+P1 冷态 P50 / P95 | 基线复查 P50 / P95 | P0+P1 复查 P50 / P95 |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| 合成 `scope-local` | 113.3461 | 62.2025 / 80.4628 | 635.4500 / 646.4124 | 9.7820 / 10.8237 | 2.4528 / 3.7793 | 2.1103 / 3.0896 |
| 真实项目 `local-inferred` | 551.3285 | 155.4033 / 172.8605 | 1,696.4282 / 1,852.0193 | 432.2427 / 460.6832 | 17.5027 / 22.6334 | 11.7927 / 14.1462 |

合成首次搜索仍返回 2 项。真实项目 Rust 两版各返回 154 项，GoLand 为 284 项，131 条既有差异明细相同。Wave A 交付计时的冷态 / 复查 P50/P95 为合成 10.4532 / 12.1247 和 1.9334 / 2.6388，真实项目 440.4761 / 448.0049 和 12.6152 / 19.0104。其诊断冷态 / 复查 P50/P95 为合成 10.7492 / 10.9427 和 2.3477 / 3.3105，真实项目 436.8714 / 445.5573 和 12.4412 / 14.0003。

| Wave B 冷诊断指标 | 合成 `scope-local` | 真实项目 `local-inferred` |
| --- | ---: | ---: |
| 诊断冷态 P50 / P95（ms） | 10.0031 / 11.7274 | 402.7284 / 410.5462 |
| 诊断复查 P50 / P95（ms） | 2.3972 / 3.2574 | 12.5678 / 15.7552 |
| SDK 后台耗时范围 / 就绪早于首次搜索（ms） | 190–217 / 27–41 | 168–191 / 444–514 |
| 首次搜索命中 SDK 清单 / 每次预热导出名的文件数 | 5/5 / 1,629 | 5/5 / 1,629 |
| 读盘 / 二读 / 三读及以上 / 已索引文件的展示源读取 | 18 / 0 / 0 / 0 | 574 / 6 / 0 / 0 |
| 预筛选读取 / 其中工作区及 vendor / 交接文本 | 17 / 17 / 0 | 518 / 87 / 35 |
| 完整解析 / 声明解析 | 0 / 1 | 17 / 76 |
| 类型位置 / 预期权重缓存命中 | 0 / 0 | 279 / 21 |
| 峰值临时源数 / 文本字节数 | 1 / 22,628 | 8 / 1,520,368 |
| 首查结束释放 / 全部诊断请求结束释放 | 5/5 / 95/95 | 5/5 / 20/20 |

SDK 后台探测保存有效 `GOROOT`，包图从标准库包目录取根，首查复用已就绪清单。真实项目旧诊断在候选展开中额外全量解析 396 次，重开 64 份已索引文件，52 份出现第三次读取。Wave B 五次首查的已有索引展示源读取和第三次读取均为 0。代表样本候选展开由 935.0433 ms 降到 15.8449 ms。

活动取消及失效定向重放退出 0，两次 `cancelled` 和一次 `stale` 均报告释放。本轮 Rust 的 `a_completion_task_releases_request_sources_on_finish_cancel_or_stale` 验证三条路径都释放已经持有的源和 AST，`completion_source_cache_bounds_count_and_bytes_and_drops_the_sources` 验证缓存上限及释放。其余测试覆盖 SDK 构建键复用和切换、工作区和 vendor 缓冲区优先及失效、有序合并，以及取消单次回包和后续请求恢复。

#### 全量性能

单元为 ms，分位数使用最近秩。每套首轮均先跑基线再跑 P0+P1，并使用相同的 GoLand 原始计时。首查目标 `scope-local` 由 632.4269 降至 10.5919 ms，`local-inferred` 由 1,657.6378 降至 420.4965 ms。

| 工作区 | 场景 | 共同 / 总样本数 | GoLand P50 / P95 | 基线 P50 / P95 | P0+P1 P50 / P95 |
| --- | --- | ---: | ---: | ---: | ---: |
| 真实项目 | 首查 | 13 / 22 | 43.3941 / 446.7385 | 3.0743 / 24.4822 | 3.0224 / 13.2341 |
| 真实项目 | 复查 | 390 / 660 | 29.2823 / 123.5895 | 0.5112 / 15.4531 | 0.3289 / 10.5048 |
| 真实项目 | 编辑后首查 | 13 / 22 | 36.0754 / 139.9207 | 0.9153 / 15.1580 | 0.8157 / 12.4818 |
| 合成 | 首查 | 58 / 126 | 78.7436 / 139.6909 | 1.2631 / 7.1502 | 1.2855 / 4.6605 |
| 合成 | 复查 | 1,740 / 3,780 | 31.3772 / 95.6739 | 0.6051 / 3.6261 | 0.5360 / 2.6349 |
| 合成 | 编辑后首查 | 58 / 126 | 32.4451 / 106.7794 | 0.6673 / 3.6799 | 0.6852 / 2.4879 |

GoLand 在 BASIC 调用内呈现候选。rs-gopls 的独立 resolve 计时如下，真实项目使用全部通过解析的样本。

| 工作区 | resolve 场景 | 共同 / 通过解析 | 基线共同 P50 / P95 | P0+P1 共同 P50 / P95 | 基线全部 P50 / P95 | P0+P1 全部 P50 / P95 |
| --- | --- | ---: | ---: | ---: | ---: | ---: |
| 真实项目 | 首查后 | 0 / 1 | — | — | 0.8055 / 0.8055 | 0.3972 / 0.3972 |
| 真实项目 | 复查后 | 0 / 30 | — | — | 1.0632 / 1.2074 | 0.2959 / 0.3925 |
| 真实项目 | 编辑后 | 0 / 1 | — | — | 1.0833 / 1.0833 | 0.3267 / 0.3267 |
| 合成 | 首查后 | 12 / 26 | 0.2192 / 0.2501 | 0.2086 / 0.2811 | 0.2297 / 0.9877 | 0.2248 / 0.3158 |
| 合成 | 复查后 | 360 / 780 | 0.2151 / 0.3214 | 0.2025 / 0.3357 | 0.2499 / 1.0568 | 0.2460 / 0.3753 |
| 合成 | 编辑后 | 12 / 26 | 0.2264 / 0.3439 | 0.2036 / 0.3031 | 0.2517 / 1.0421 | 0.2313 / 0.3528 |

下表列出各轮观察到升幅的指标，每格按基线、P0+P1 顺序列值。第二轮先跑 P0+P1 再跑基线，第三轮合成夹具恢复首轮顺序。第一、二轮的真实项目共同样本相同。

| 工作区和指标 | 首轮（ms） | 第二轮（ms） | 第三轮（ms） |
| --- | ---: | ---: | ---: |
| 合成首查 P50 | 1.2631 → 1.2855 | 1.3379 → 1.4716 | 1.3698 → 1.5023 |
| 合成编辑后首查 P50 | 0.6673 → 0.6852 | 0.6517 → 0.5374 | 0.5403 → 0.6400 |
| 合成首查后共同 resolve P50 | 0.2192 → 0.2086 | 0.1940 → 0.2262 | 0.1959 → 0.2352 |
| 合成首查后共同 resolve P95 | 0.2501 → 0.2811 | 0.2444 → 0.3863 | 0.2846 → 0.3476 |
| 合成复查后共同 resolve P95 | 0.3214 → 0.3357 | 0.3324 → 0.3051 | 0.3069 → 0.2816 |
| 合成编辑后共同 resolve P95 | 0.3439 → 0.3031 | 0.3699 → 0.3469 | 0.2852 → 0.3120 |
| 真实项目编辑后首查 P50 | 0.9153 → 0.8157 | 0.6856 → 0.7918 | — |

#### 历史回归与启动导航

| 命令 | 结果 |
| --- | --- |
| `check_workspace_updates.py` | 28/28，其中补全 5/5 |
| `python -B -m unittest tools/test_lsp_harness.py tools/test_workspace_checks.py` | 25/25 |
| `check_definitions.py --stage 6 --workspace $external` | 合成 124/124，真实项目 16/16 |
| `check_goland_definitions.py --method definition` | 19 个落点一致，1 个既有 IDE 包对象差异 |
| `check_goland_definitions.py --method typeDefinition` | 32 个落点一致，2 个预期为空 |
| `check_goland_references.py` | 34/34 |
| `check_goland_implementations.py` | 20/20 |
| `check_goland_hover.py` | 21/21 |
| `check_goland_signature_help.py` | 13/13 |
| `check_goland_symbols.py` 真实项目 | 8 份文档的大纲两种模式通过，两次会话的 20 个工作区查询通过，其中 14 个物理集合相同 |
| `check_goland_symbols.py` 合成 | 5 份文档的大纲两种模式通过，两次会话的 36 个工作区查询通过，其中 27 个物理集合相同 |
| `go list -mod=vendor -test ./...` | 退出 0，列出 14 个包或测试视图条目 |
| `go test -mod=vendor ./...` | 退出 0，根包通过 |
| `git -C $env:RS_GOPLS_BENCH_WORKSPACE status --short --untracked-files=all -- .` | 验证前后均为空 |

真实项目的 `internal/api/api.go` 在零基 UTF-16 `156:42` 立即查询 `execPath`。两版均 5/5 返回同文件的精确范围 `[155:1, 155:9)`。从启动前、经过初始化和 `didOpen` 到解码首个响应，基线 P50/P95 为 10.7271 / 16.7920 ms，P0+P1 为 10.9717 / 13.3050 ms。单独请求耗时分别为 2.3940 / 3.4341 ms 和 2.6109 / 2.7280 ms。优化版的 5 个正确响应均在 SDK 预热开始后返回，距离 SDK 清单就绪还有 160.095–187.990 ms。

两侧另在同一个 `532228d3` 真实项目源码副本上执行相同定义查询，避免 GoLand 项目初始化写入原工作区。GoLand 使用已授权的本机配置和 `dontReopenProjects` 启动新进程，等待副本进入智能模式。首次正确目标及范围相同。从发起进程启动到取得首个正确结果，GoLand 单次为 13,437 ms，其中从进程实际创建起为 13,375 ms，从打开项目起为 8,835.9406 ms，定义请求为 1,081.7750 ms。相同副本上 rs-gopls 的 5 次启动到首个正确结果 P50/P95 为基线 10.4737 / 16.6024 ms、P0+P1 10.7476 / 12.6831 ms。两侧计时均止于取得定义结果，包含各自启动和文档准备。

原始报告位于 Cargo 产物目录，构建与回归见 `wave-b-rust.log` 和 `wave-b-history.log`，首轮及复测见 `wave-b-cold-and-compare.log` 和 `wave-b-repeat-compare.log`。冷态、诊断和逐字节对拍分别见 `wave-b-timing.json`、`wave-b-cold-timing-1c42031.json`、`wave-b-diagnostic.json` 和 `wave-b-wire-parity.json`。取消和导航证据见 `wave-b-cancel-and-stale.ndjson`、`wave-b-navigation-external.json`、`wave-b-navigation-snapshot.json` 和 `wave-b-goland-startup-definition.json`。[GoLand 补全对照基线](../goland-completion-baseline.md#共同样本延迟)
