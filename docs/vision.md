# rs-gopls 项目愿景

用 Rust 实现 Go 语言服务器，为 Zed 提供准确、低延迟的代码导航。在真实 Go 项目中，即使编辑尚未保存，用户也能跳到正确的声明或类型，并找到绑定到同一符号的引用。

## 设计原则

- 以 GoLand 安装包中的 Go 语义引擎实现为设计蓝本。参考与功能相关的解析、索引和查询策略，结合 Go 规范与 Zed 的 LSP 接口在 Rust 中独立实现。
- Rust 负责增量解析、声明索引和语义解析，Go 工具链提供包图和构建元数据。编辑器中的未保存缓冲区优先于磁盘内容。
- 功能以 GoLand 在 `localchat/server` 等真实项目中的可复现行为为参照。验收核对实际落点和完整引用集合，记录具有意义的编辑器行为差异。参见[声明跳转基线](goland-definition-baseline.md)、[类型定义基线](goland-type-definition-baseline.md)和[引用基线](goland-references-baseline.md)。
- 性能直接对照 GoLand 的实际查询。用相同项目、Go SDK 和导航样本核对完整目标，再分场景比较启动、已索引查询和编辑后耗时。新能力应满足已有导航的正确性和响应速度要求。[导航性能对照](PLANS.md#导航性能对照)

## 当前能力与方向

已交付声明跳转、类型定义跳转和查找引用，覆盖 Go Modules、`go.work`、GOPATH 和内外测试包，并在 Zed 隔离配置验证。后续方向是接口与实现之间的导航，以及扩大真实项目的回归覆盖。[第三阶段验收记录](plans/2026-11-25-go-navigation-phase-three.md)
