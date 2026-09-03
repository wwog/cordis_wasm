# Cordis (Rust)

> 一个以 **可逆 effect** 与 **响应式依赖注入** 为内核的 Rust 插件化运行时，使用 **Wasmtime Component Model** 作为唯一的动态插件机制。
>
> 本项目是 [Cordis TypeScript 实现](https://github.com/cordiverse/cordis)（`cordis@4.0.0-rc.9`）的 Rust 原生重写，语义对齐、开发体验对齐，但不承诺二进制兼容 TS 插件。理论依据是论文 *A Programming Paradigm for Spatiotemporal Composability*（arXiv:2608.25512）。
>
> **状态：0.1.0 发布候选。** Phase 1–6 的代码与本地门禁已落地；正式发布仍以 release commit 的跨平台、Miri 与依赖安全 CI 全绿为准。
> 论文落地在：./docs/2608.25512v1.txt


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

## 实施规则

- 优先选择最小、直接、可测试的实现；没有明确收益时不增加抽象层。
- 状态变化必须有唯一入口，不用多个布尔值表达同一状态。
- 公共 API 不使用含义不明的 `bool` 参数，改用 enum 或具名 options。
- 不跨 `.await` 持有同步锁；不在 Supervisor 内执行用户代码。
- 优化必须由复杂度、内存或基准数据支撑；先保证语义，再加 fast path。
- 动态插件走 Wasmtime Component Model；
- 每完成一项同时补测试和文档，不积累“最后再测”的任务。

## 许可

MIT
