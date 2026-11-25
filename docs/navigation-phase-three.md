# Go 导航第三阶段开发计划

本阶段依次交付“跳到类型定义”和“查找引用”。功能使用 GoLand 在真实项目上的运行结果验收。性能用同场景的 gopls 对比，每步独立提交到本地 Git。[第二阶段验收记录](goland-definition-baseline.md#第二阶段发布验收)

## 1. 采集 GoLand 类型定义基线

- 在 GoLand 2026.3 打开 `localchat/server`，固定项目修订、IDE 构建和 Go SDK。调用 **Go to Type Declaration**，记录查询文件、UTF-16 光标、候选文件和标识符范围，包含空结果及多候选。对典型位置分别记录声明与类型声明动作结果。[GoLand 源码导航](https://www.jetbrains.com/help/go/navigating-through-the-source-code.html#navigate-between-code-elements)
- 从 `internal/api`、`internal/app` 和 `internal/store` 选取字段、局部变量、接口、指针、容器元素、调用结果与 SDK 类型。用夹具补充别名、泛型、匿名类型及未保存编辑。依赖和 SDK 路径按 `GOMODCACHE` 与 `GOROOT` 归一化。
- 单独提交采集入口和实测语料。把 IDE 专有目标与语义差异保留为可复查记录。

## 2. 实现并验收类型定义

- 接入 `textDocument/typeDefinition` 与能力声明。复用名称绑定、成员解析、类型推断、后台包图、UTF-16 坐标和缓冲区覆盖。
- 按实际 GoLand 结果确定命名类型、指针、容器、返回值、别名、泛型、预声明或匿名类型的落点。验收未保存变化、空结果与原有声明跳转。用 release 构建和 Zed 隔离配置核对功能；功能与性能验收完成后进入查找引用。

## 3. 建立 GoLand 查找引用基线并实现

- 在同一 `localchat/server` 修订运行 GoLand **Find Usages**，固定项目范围及搜索选项。选取局部变量、字段、私有函数、导出类型和方法，记录每个源码用法的文件与 UTF-16 范围，并记录声明和额外的 IDE 用法类别。
- 接入 `textDocument/references`，依次实现文件内、同包和跨包搜索。用包图缩小候选文件范围，再复用声明解析验证符号身份。稳定输出顺序、去重，并处理 `includeDeclaration`、遮蔽、未保存引用、内外测试包和取消请求。
- 对接口方法等有差异的搜索保留可重复运行的用例。接口的实现跳转另列阶段。

## 回归与性能验收

- 每步先运行受影响包的 `cargo check`，再运行对应回归，沿用项目工具链、profile 和 Cargo 产物目录。原有 21 个 GoLand 文件目标、124 个合成目标、16 个项目锚点及 7 组工作区回归继续通过。
- 在同一 Go SDK、工作区和缓存条件下对比 gopls。类型定义按规范化目标范围，引用按去重后的用法集合验收。准确性匹配的共同样本进入耗时对比，错误和行为差异单独记录。
- 正式测量轮换运行次序，至少 5 次独立会话，覆盖首次正确结果、暖态 P50/P95 与峰值工作集。类型定义覆盖编辑后查询，引用覆盖局部和跨包搜索。主要路径以较 gopls 的 P95 下降至少 20% 为目标，已有声明跳转暖态 P95 的回退上限取 10% 和 1 ms 中较大者。

## 类型定义验收

已完成真实项目和补充用例共 58 项 GoLand 对照，保留原有声明跳转回归，并在 Zed 隔离配置验证发布版。共同路径的 gopls 性能比较记录在基线文档中。[类型定义发布验收](goland-type-definition-baseline.md#类型定义发布验收)
