# Cordis (Rust)

> 一个以 **可逆 effect** 与 **响应式依赖注入** 为内核的 Rust 插件化运行时，使用 **Wasmtime Component Model** 作为唯一的动态插件机制。
>
> 本项目是 [Cordis TypeScript 实现](https://github.com/cordiverse/cordis)（`cordis@4.0.0-rc.9`）的 Rust 原生重写，语义对齐、开发体验对齐，但不承诺二进制兼容 TS 插件。理论依据是论文 *A Programming Paradigm for Spatiotemporal Composability*（arXiv:2608.25512）。
>
> **状态：开发中（0.1.0-dev）。** 目前内核（Fiber / effect / service / 事件）与 native 宏已落地并通过测试；Wasmtime 动态插件层正在实现。API 尚未冻结，下文示例不代表最终形态。


**保证语义的部分在框架里，而不在插件代码里**：

- 组件卸载时，Fiber 状态机先让**依赖者**离开并等它们清理完，再执行**提供者**的逆操作——递归排空整个依赖图；
- provider 用单调递增的 Fiber id 标识，值相同但 provider 换人，消费者仍会重载；
- 依赖环在加载前就被 SCC 诊断发现，不会死锁等待；
- WASM 插件调用 `provide/listen/timer` 时，host binding 自动把 effect 归属到当前 Fiber，guest 丢了句柄也由 host 强制清理。

至于第 1、5、6 条需求（动态加载、HMR、沙箱），对应 `cordis-wasm` / `cordis-loader` / `cordis-hmr`，正在按 [plan.md](plan.md) 的 Phase 3–5 逐步落地。

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
```

native 路径零序列化：`CounterClient::from_native(Arc<T>)` 直接走宏生成的 object-safe adapter；`CounterClient::new(Arc<dyn ServiceDispatcher>)` 校验完整 `ServiceId` 后走 MessagePack 动态路径，这条路径就是将来 Wasmtime 边界复用的路径。

## 设计要点

- **native 组件静态链接，动态组件只走 Wasmtime Component Model**。不引入 `dlopen`：Rust ABI 不稳定，卸载也不安全。
- **Supervisor actor 单写者**：组件图、服务表、Fiber 状态只由它串行修改；组件代码与事件回调在 Supervisor 外执行，不跨 `await` 持锁。这是对 JS 单线程事件循环的 Rust 化替代。
- **两层 ABI**：Kernel WIT 固定且版本化，只描述生命周期/注册/调用/事件/错误；业务服务协议由 `#[cordis::service]` 生成，用 ABI hash 防止同名不兼容服务互相满足。
- **ABI hash 的稳定性**：只由服务名、方法名、参数类型顺序和返回类型构成；注释、参数改名、方法声明顺序都不影响线协议。
- **guest 不可信假设**：host effect 表是最终权威，Wasmtime spike 已确认 guest 遗失句柄时 Store drop 不会触发析构，因此清理不依赖 guest 善意。
- **失败即回滚**：Fiber `apply` 失败进入 `Failed` 并回滚已完成 effects；HMR 用事务流程替换组件，失败从旧 artifact 重建。

## 文档

- [plan.md](plan.md)：完整架构研究稿与分阶段实现计划
- [TODO.md](TODO.md)：执行清单与当前进度
- [docs/wasmtime-findings.md](docs/wasmtime-findings.md)：Wasmtime 48 的实测结论（重入、取消、资源析构）

## 当前进度

| 阶段 | 状态 |
|---|---|
| Phase 0：Wasmtime 风险验证 | 核心 spikes 已完成 |
| Phase 1：cordis-core（Fiber / effect / service / 事件） | ✅ 完成 |
| Phase 2：宏与 native 组件体验 | ✅ 完成 |
| Phase 3：Wasmtime host 与 guest SDK | 🚧 进行中 |
| Phase 4：Loader / Include | 未开始 |
| Phase 5：WASM HMR | 未开始 |
| Phase 6：Timer / Logger / CLI / 发布 | 未开始 |

详细清单见 [TODO.md](TODO.md)。

## 许可

MIT
