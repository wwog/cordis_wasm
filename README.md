# Cordis (Rust)

> 一个以 **可逆 effect** 与 **响应式依赖注入** 为内核的 Rust 插件化运行时，使用 **Wasmtime Component Model** 作为唯一的动态插件机制。
>
> 本项目是 [Cordis TypeScript 实现](https://github.com/cordiverse/cordis)（`cordis@4.0.0-rc.9`）的 Rust 原生重写，语义对齐、开发体验对齐，但不承诺二进制兼容 TS 插件。理论依据是论文 *A Programming Paradigm for Spatiotemporal Composability*（arXiv:2608.25512）。
>
> **状态：0.1.0 发布候选。** Phase 1–6 的代码与本地门禁已落地；正式发布仍以 release commit 的跨平台、Miri 与依赖安全 CI 全绿为准。


**保证语义的部分在框架里，而不在插件代码里**：

- 组件卸载时，Fiber 状态机先让**依赖者**离开并等它们清理完，再执行**提供者**的逆操作——递归排空整个依赖图；
- provider 用单调递增的 Fiber id 标识，值相同但 provider 换人，消费者仍会重载；
- 依赖环在加载前就被 SCC 诊断发现，不会死锁等待；
- WASM 插件调用 `provide/listen/timer` 时，host binding 自动把 effect 归属到当前 Fiber，guest 丢了句柄也由 host 强制清理。

动态加载、沙箱与 HMR 分别由 `cordis-wasm`、`cordis-loader` 和 `cordis-wasm::hmr` 实现；设计校准与边界说明见 [Phase 3–5 implementation notes](https://github.com/wwog/cordis_wasm/blob/master/docs/phases-3-5.md)。

### 和"普通 DI 容器"的区别

把 Cordis 简化成依赖注入容器会丢掉它的核心：同一个 Context 同时承担两件事——

- **可逆 effect**（时间可组合性）：组件删除后，它对系统状态的贡献**精确消失**。effect 的形式是"执行动作并返回 disposer"，多个 disposer 按 LIFO 组合；异步加载中途失效时，只回滚已经完成的步骤。
- **响应式 coeffect**（空间可组合性）：组件声明依赖，依赖满足时自动出现，提供者改变或消失时自动停用，并在新提供者稳定后重新激活。

DI 容器回答"怎么拿到依赖"；Cordis 回答"**依赖和它产生的副作用，如何在运行中被精确地装上和拆下**"。前者是一次性的，后者是贯穿生命周期的。


## 快速开始

```bash
cargo run -p cordis --example native_counter
# 输出：counter value: 3

cargo test --workspace

# 构建示例 Component，再预检或运行声明式应用
cargo run -p xtask -- build-guests
cargo run -p cordis-cli -- check examples/wasm-app/cordis.json
cargo run -p cordis-cli -- inspect examples/wasm-app/cordis.json
cargo run -p cordis-cli -- run examples/wasm-app/cordis.json
```

native 路径零序列化：`CounterClient::from_native(Arc<T>)` 直接走宏生成的 object-safe adapter；`CounterClient::new(Arc<dyn ServiceDispatcher>)` 校验完整 `ServiceId` 后走 MessagePack 动态路径，这条路径就是将来 Wasmtime 边界复用的路径。

## 设计要点

- **native 组件静态链接，动态组件只走 Wasmtime Component Model**。不引入 `dlopen`：Rust ABI 不稳定，卸载也不安全。
- **Supervisor actor 单写者**：组件图、服务表、Fiber 状态只由它串行修改；组件代码与事件回调在 Supervisor 外执行，不跨 `await` 持锁。这是对 JS 单线程事件循环的 Rust 化替代。
- **两层 ABI**：Kernel WIT 固定且版本化，只描述生命周期/注册/调用/事件/错误；业务服务协议由 `#[cordis::service]` 生成，用 ABI hash 防止同名不兼容服务互相满足。
- **ABI hash 的稳定性**：只由服务名、方法名、参数类型顺序和返回类型构成；注释、参数改名、方法声明顺序都不影响线协议。
- **guest 不可信假设**：host effect 表是最终权威，Wasmtime spike 已确认 guest 遗失句柄时 Store drop 不会触发析构，因此清理不依赖 guest 善意。
- **失败即回滚**：Fiber `apply` 失败进入 `Failed` 并回滚已完成 effects；HMR 用事务流程替换组件，失败从旧 artifact 重建。
- **声明式运行闭环**：`WasmEntryDriver` 将 Loader Entry、managed realm、动态 Fiber、Kernel 路由和 HMR 绑定为同一生命周期；`cordis check` 只预检，`run` 才激活组件。
- **配置与 artifact 热更新**：`cordis run` 同时监听配置和已挂载 Component；配置 diff 是可逆批事务，失败保留上一棵 Entry Tree。
- **内置组件显式注册**：嵌入方通过 `BuiltinRegistry` 绑定 `builtin:<name>`；内置与 WASM 共用 `ComponentFactory` 和 Supervisor 生命周期，但不进入 artifact HMR。

## 文档

- [plan.md](https://github.com/wwog/cordis_wasm/blob/master/plan.md)：完整架构研究稿与分阶段实现计划
- [TODO.md](https://github.com/wwog/cordis_wasm/blob/master/TODO.md)：执行清单与当前进度
- [docs/wasmtime-findings.md](https://github.com/wwog/cordis_wasm/blob/master/docs/wasmtime-findings.md)：Wasmtime 48 的实测结论（重入、取消、资源析构）
- [docs/phases-3-5.md](https://github.com/wwog/cordis_wasm/blob/master/docs/phases-3-5.md)：Phase 3–5 的实现边界、纠偏结论与失败语义
- [docs/phase-6.md](https://github.com/wwog/cordis_wasm/blob/master/docs/phase-6.md)：运行时装配、CLI、Timer/Logger 与可靠性基线
- [docs/parity.md](https://github.com/wwog/cordis_wasm/blob/master/docs/parity.md)：TypeScript 可观察行为与 Rust 语义差异
- [docs/api-review.md](https://github.com/wwog/cordis_wasm/blob/master/docs/api-review.md)：0.1.0 public API 冻结决策
- [docs/release-checklist.md](https://github.com/wwog/cordis_wasm/blob/master/docs/release-checklist.md)：发布门禁及当前证据
- [docs/dependency-review.md](https://github.com/wwog/cordis_wasm/blob/master/docs/dependency-review.md)：许可证、来源与 RustSec 审计记录

## 当前进度

| 阶段 | 状态 |
|---|---|
| Phase 0：Wasmtime 风险验证 | 核心 spikes 已完成 |
| Phase 1：cordis-core（Fiber / effect / service / 事件） | ✅ 完成 |
| Phase 2：宏与 native 组件体验 | ✅ 完成 |
| Phase 3：Wasmtime host 与 guest SDK | ✅ 已完成 |
| Phase 4：Loader / Include | ✅ 已完成 |
| Phase 5：WASM HMR | ✅ 已完成 |
| Phase 6：Timer / Logger / CLI / 发布 | 发布候选，本地门禁已完成 |

详细清单见 [TODO.md](https://github.com/wwog/cordis_wasm/blob/master/TODO.md)。

## 许可

MIT
