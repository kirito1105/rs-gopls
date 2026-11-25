# Go 导航第四阶段开发计划

| 项目 | 内容 |
| --- | --- |
| 状态 | 进行中 |
| 创建日期 | 2026-11-25 |
| 用户确认 | 2026-11-25 已确认，开始实施 |
| 更新日期 | 2026-11-26 |
| 完成日期 | 待填写 |

## 目标与范围

在 Zed 中从 Go 接口类型或接口方法跳到实现，从具体类型或方法反查其满足的接口与对应方法。支持从声明和用法位置发起请求，通过 `textDocument/implementation` 返回实际声明位置。查询目标、搜索范围和落点以 GoLand 2026.3 在 `E:\Code\localchat\server` 的实测为准。接口到实现使用 **Go to Implementation**，具体实现反查接口时对照 **Go to Super**，并在同一光标位置记录 **Go to Implementation** 的行为。

延续第三阶段的真实项目基线、未保存缓冲区优先策略和相同条件下的 gopls 性能对照。先采集 GoLand 基线，再实现并在 Zed 验收。[项目愿景](../vision.md) · [第三阶段验收记录](2026-11-25-go-navigation-phase-three.md)

## 验收标准

- [x] GoLand 实测记录可在固定项目修订下重放。覆盖接口类型和方法、具体类型和方法、声明和调用处、同包和跨包、测试实现，以及单目标、多目标和空结果。
- [ ] LSP 初始化声明 `implementationProvider`。发布版严格匹配 GoLand 确认的目标集合与 UTF-16 标识符范围，结果稳定且去重。无匹配项返回空结果。
- [ ] 补充夹具验证值与指针接收者、嵌入接口与提升方法、同名异签名、跨包私有方法、别名、泛型及空接口和约束接口。按 GoLand 实测确认各场景中出现的候选和落点。
- [ ] 未保存的接口或实现改动能即时改变结果，关闭覆盖缓冲区后恢复磁盘结果。工作区包图更新后，搜索使用当前构建条件与测试包视图。长查询可取消。
- [ ] Zed 的隔离配置加载发布版 `rs-gopls`。在 `localchat/server` 实际调用实现跳转，核对单目标落点、多目标选择列表、空结果和实现反查接口。现场抽查声明跳转、类型定义跳转和查找引用。
- [ ] 第三阶段记录的声明、类型定义、引用及工作区回归全部通过。同轮对比 gopls，记录共同正确样本的首次正确响应、暖态和编辑后 P50/P95、进程树峰值工作集，以及结果差异和错误。

## 实现依据

只读检查 GoLand 安装目录 `E:\Apps\goland-2026.3.0.1.win` 的 `build.txt`、插件注册和字节码，构建号为 `GO-263.7307.354`。Go 插件文件为 `plugins/go-plugin/lib/modules/intellij.go.impl.jar`，SHA-256 为 `2A13F1A753D5E15DB70C5758E17DB1DC28CC790E1D329E574D8B6A078D636784`。可使用同目录下 `jbr/bin/java.exe`、`lib/intellij.libraries.asm.jar` 中的 `org.jetbrains.org.objectweb.asm.util.Textifier` 和对应 JAR 重查下列类。

| 安装包中的类或注册 | 已确认的调用流程 |
| --- | --- |
| `lib/intellij.platform.lang.impl.jar` 中的 `com.intellij.codeInsight.navigation.GotoImplementationHandler` 和 `ImplementationSearcher` | 光标目标由 `TargetElementUtil` 解析，实现候选通过 `DefinitionsScopedSearch.search` 查询。 |
| Go 插件的 `intellij.go.impl.xml` 和 `com.goide.go.GoInheritorsSearch.processQuery` | 插件为 `DefinitionsScopedSearch` 注册 Go 类型和方法搜索。类型入口取得接口的直接及完整方法列表，在搜索范围内查找候选，再用 `GoTypeUtil.isImplementsInterface` 验证。 |
| `com.goide.go.GoMethodInheritorsSearch.processQuery` 和 `GoGotoUtil` | 方法入口从所属接口查候选，再通过 `getSameNamedMethodOfTypeSpec` 取得方法位置。`GoGotoUtil` 用方法及方法规格指纹索引查候选，并追踪嵌入类型和别名索引。 |
| `intellij.go.impl.xml`、`com.goide.go.GoGotoSuperHandler`、`GoSuperMethodSearch` | `codeInsight.gotoSuper` 注册了独立的 Go 入口。类型和方法搜索复用接口候选查询与满足关系检查，具体方法可映射到对应接口方法。 |

Rust 方案复用 [`src/main.rs`](../../src/main.rs) 的 LSP 分派和分批取消流程、[`src/symbols.rs`](../../src/symbols.rs) 的声明索引、[`src/semantic.rs`](../../src/semantic.rs) 的符号解析和文件覆盖，以及 [`src/workspace.rs`](../../src/workspace.rs) 的包图和测试包视图。用方法名及可安全过滤的签名信息筛选候选，覆盖没有直接导入接口包的结构化实现。用完整的值或指针方法集、签名类型身份及不可导出方法的包身份验证满足关系，并处理嵌入产生的歧义。类型查询返回类型声明，方法查询以实测的方法声明为落点。索引更新沿用现有文件和包图失效机制。空接口、约束接口、别名、提升方法、局部和依赖范围、原声明是否包含在候选中，均以运行中的 GoLand 结果确定。

## 实施步骤

1. 确认后记录确认结果并将状态改为「进行中」。在 GoLand 打开固定修订的 `localchat/server`，等索引完成后采集双向跳转。优先选用 `internal/store` 的 `Store`、`UserStore`、`UserStore.GetAll` 和 `internal/store/sqlstore` 的对应实现，在 `internal/api` 和 `internal/app` 补充调用处和测试实现。交叉核对 IDE 中的实际动作与采集输出。提交可重放的采集入口、真实项目和补充夹具，以及 `docs/goland-implementation-baseline.md` 中的实测记录。
2. 接入 `textDocument/implementation` 和能力声明，按 GoLand 基线实现接口类型、接口方法到实现的查询。用严格的 stdio 检查器验证目标集合与范围，运行受影响的回归后提交。
3. 实现具体类型和方法到接口的查询，按基线补齐嵌入、提升、别名、泛型及作用域边界。验证缓冲区覆盖、包图变更和取消，并运行前三项导航回归后提交。
4. 构建发布版，完成同条件 gopls 对比及 Zed 实际跳转验收，记录版本、提交号、二进制摘要、功能结果和性能数据。验证通过后提交验收记录及最终计划状态。

每个验证通过的步骤各提交一次本地 Git，同步勾选本计划的验收项并补记实际结果。目标、范围或验收条件有实质变化时，更新本计划并重新确认。

## 验证计划

- **GoLand 基线**　沿用第三阶段 `localchat/server` 修订 `532228d3d36bd9f0fffb4bda35c180485b2eadd6` 和 Go 1.27.4，采集时复核并记录实际环境。每个查询记录动作、查询路径、零起点 UTF-16 光标、目标路径和标识符范围。依赖和 SDK 路径按 `GOMODCACHE` 与 `GOROOT` 归一化。记录 UI 中的多候选和空结果，补充可重放的语义边界夹具。
- **构建与回归**　Rust 改动先运行 `cargo check -p rs-gopls --locked`，再按需运行 `cargo test -p rs-gopls --locked`。发布验收运行 `cargo build -p rs-gopls --release --locked`，通过 `cargo metadata` 读取配置的产物目录。严格运行新增实现跳转对照，并复用 `tools/check_definitions.py --stage 6`、`tools/check_goland_definitions.py`、`tools/check_goland_references.py` 和 `tools/check_workspace_updates.py` 的现有参数与语料。核对第三阶段的 21 个 GoLand 声明位置、124 个合成声明位置、16 个项目锚点、58 个类型定义位置、34 组引用结果及 12 组工作区回归。[类型定义验收](../goland-type-definition-baseline.md#类型定义发布验收) · [引用验收](../goland-references-baseline.md#引用发布验收)
- **性能对照**　在同一 Windows 主机固定项目快照、Go SDK、构建环境、打开文件、请求序列、编辑内容和缓存条件。记录 gopls 版本，优先沿用第三阶段的 v0.24.0 及 `Save` 诊断配置。轮换两套服务的运行顺序，每套至少 5 个独立进程会话。每个会话以有确定非空目标的共同用例记录首次正确响应，再对结果一致的共同样本测量暖态和编辑后 P50/P95、进程树峰值工作集，并记录样本数。记录其他样本的结果差异或错误。沿用第三阶段的性能目标，主要共同路径的 P95 较 gopls 低至少 20%。旧功能与第三阶段发布版 `be7641d` 同轮对照，建议各路径暖态 P95 的回退上限为 10% 与 1 ms 中较大者。
- **Zed 实测**　先运行 `tools/test_zed.ps1 -PrepareOnly` 准备发布二进制，再用输出的隔离配置打开 `localchat/server`，确认 Go 文件使用 `rs-gopls`。在已采集的光标位置触发跳转，核对编辑器落点或多目标列表。记录 Zed 版本、已加载的发布二进制摘要、LSP 能力、查询位置和可见结果。沿用仓库约定的工具链、profile、features 和 target。

## 完成记录

- 2026-11-25 创建待确认计划。起点为 `main` 的 `7960707`，工作区干净，本地领先 `origin/main` 33 个提交。第三阶段发布版为 `be7641d`，验收记录提交为 `e0b6d2f`。
- 2026-11-25 用户确认计划，开始采集 GoLand 基线。
- 2026-11-26 在指定 GoLand 中完成 `localchat/server` 的 20 个查询和两个动作共 108 个目标的采集。记录了调用处包含接口声明的行为，以及活动包图外的依赖目标。[GoLand 实现跳转基线](../goland-implementation-baseline.md)
- 2026-11-26 发布构建通过 12 个 GoLand 正向查询、缓冲区覆盖与活动包图更新、待加载和扫描取消检查。声明跳转 124 个合成目标与 16 个真实项目锚点、类型定义 34 个真实项目位置、引用 34 组结果通过同轮回归。
- 2026-11-26 补充模块通过 Go 1.27.4 的 `go test ./...`。GoLand 双向重放与语料一致，共 29 个补充查询，已核验真实项目和补充模块全部 203 个原始目标的 UTF-16 范围。
