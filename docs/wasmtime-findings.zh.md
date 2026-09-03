# Wasmtime 48.0.1 spike 调研结论

本文档只记录已由可执行测试证明的行为。测试位于 `crates/cordis-wasm/src/lib.rs`。

## Engine 与限制

- 使用最小 feature：`async`、`component-model`、`cranelift`、`runtime`、`std`。
- `consume_fuel` 能以 `Trap::OutOfFuel` 中断无限 guest 代码。
- `epoch_interruption` 能以 `Trap::Interrupt` 中断无限 guest 代码。
- `StoreLimits` 能阻止超限 memory grow；registration 数量必须由 Cordis host 自行计数。
- 每次 guest 调用前必须重新设置 fuel 与 epoch deadline；不能沿用上一次调用的剩余预算。

## Resource destructor

- guest 显式执行 `resource.drop` 时，Wasmtime 调用 host destructor。
- guest 丢失 own handle 后，drop Store 不会补调用该 destructor。
- 结论：WIT registration resource 只用于提前幂等释放。host 在创建 registration 的同一事务中必须把 disposer 登记到 Fiber `EffectGuard`，强制卸载以 EffectGuard 为权威。

## 同 Store 重入与 waterfall

- async guest 调用进入 host import 后，host 可以用同一个 Store 再调用同一 Component instance。
- 同一 `dispatch` export 递归三层并恢复外层的 onion 调用测试通过。
- v0.1 结论：WASM waterfall 可以保留 onion `next()` 语义，不需要降级成返回式 middleware。
- `Next` 仍必须是 host 管理的 one-shot token；duplicate、过期 token、trap 路径和 Fiber teardown 都必须回收 token。
- 当前 spike 不需要启用 `component-model-async`。只有未来允许同一 instance 的独立顶层并发调用时，才重新评估该 feature。

## 取消约束

- 直接 drop 一个停在 async host import 中的 `call_async` future 后，同一 instance 的后续调用返回 `Trap::CannotEnterComponent`。
- instance actor 禁止用 `tokio::time::timeout` 丢弃 in-flight Wasmtime future。
- guest 纯计算由 fuel/epoch 在 Wasmtime 内部产生 trap；host import 必须监听取消信号并正常返回错误，使原始 guest future 完成展开。
- 如果 host import 不可合作取消，超时后的唯一可靠恢复路径是终止并丢弃整个 Store，而不是复用 instance。
