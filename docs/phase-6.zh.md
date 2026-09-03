# Phase 6 实现笔记

## 运行时组合与 CLI

`WasmEntryDriver` 位于 `cordis-wasm`，即拥有 Wasmtime 与 HMR 的最底层，
并且可以依赖 `cordis-loader` 而不会引入依赖环。Core 仍然不感知 artifacts 与
Entry 配置。

对于每个活跃的 Entry，driver 会：

1. 读取并 preflight Component descriptor；
2. 在激活之前校验配置；
3. 将 default、local 或带标签的 global realm 映射为稳定的运行时 Realm ID；
4. 在应用根节点之下挂载一个 `DynamicFiber`；
5. 在等待激活之前绑定其 service/event 路由；
6. 在事务性的 HMR manager 中跟踪 artifact 与 Fiber。

Provider 注册会直接从 `InstanceHost` 接收其解析后的 realm。这避免了激活竞态，
否则 provider 有可能在其 Entry 路由已知之前就可见。注册清理会把已经 withdraw 的
provider 视为成功，因为按设计，Supervisor 的 retirement 会在 guest 停用之前
先移除 provider 的可见性。

`cordis check` 解析 Include 文档、解析 Entry 树、编译 descriptor、检查
ABI/能力并校验 JSON Schema，且不进行激活。`cordis run` 额外执行 Supervisor 激活、
artifact 监听、事务性 HMR、Ctrl-C 清理与最终关闭。`cordis inspect` 在打印
Fiber 图之前会等待生命周期进入静止状态。
