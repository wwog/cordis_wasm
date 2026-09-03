# Cordis 0.1.0 发布检查清单

## 必须通过的门槛

- [x] 公开 API 审查完成，并记录了 semver 相关决策。
- [x] Core 生命周期/属性模型测试覆盖了生成的操作序列。
- [x] 故障注入覆盖了激活、关闭、路由、watcher 与回滚失败。
- [x] Miri 在本地通过了受支持的 Core effect 与 tracked-collection 目标；同样的命令
  在 nightly CI 任务中运行。
- [x] Loom 适用性已审查：已无自定义同步原语需要建模。
- [ ] 发布提交在 Linux、macOS 与 Windows 的 CI 上均为绿色。
- [x] MSRV 1.98.0 检查、严格 Clippy、rustdoc 警告与格式检查在本地均为绿色。
- [x] `wasm32-wasip2` provider/consumer 运行时组合 E2E 在本地为绿色。
- [x] 依赖 license/advisory 审查已记录且为绿色；一个信息性的 unmaintained
  传递依赖已在 `dependency-review.md` 中记录。
- [x] 基准测试在引入针对优化的公开 API 之前建立了基线。
- [x] README、语义差异、CLI 帮助、示例与 changelog 均为最新。
- [x] 已检查包内容；core/macros/guest 压缩包可独立验证，且完整的依赖图可从解压后的
  `.crate` 归档中仅凭打包的内部依赖编译通过。解压后的 CLI 归档在 release 模式下
  可安装，并报告 `cordis 0.1.0`。
- [ ] crates.io 所有权已确认为被占用的 `cordis*` 包名，或者所有包及内部依赖版本
  已迁移到一个可用的命名方案上。

工作区版本为 `0.1.0` 并不构成发布证据。其余未勾选项需要包命名空间的决策，且精确的发布提交
需存在于远程 CI 提供方上。
