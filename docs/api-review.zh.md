# 公开 API 审查：0.1.0

## 保留的决策

- Wasmtime 类型仍保持在 `cordis-core` 之外；动态组件跨 object-safe 的 Core trait。
- `restart_fiber` 重试 `Failed`，而 `reload_fiber` 显式重载 `Active`。
- Loader 声明状态并将 effect 委托给 `EntryDriver`；`WasmEntryDriver` 是具体的动态运行时
  适配器。
- 注册与收集 API 通过稳定身份移除，并在 Supervisor 顺序可能已经移除可见性的地方让清理保持
  幂等。
- `cordis-cli` 是二进制包；命令实现细节不是公开的库 API。
- 运行时 ID 保持不透明，并由其所属的运行时分配；它们不是用户创建的整数。
- Kernel WIT 保持在包版本 `0.1.0`；业务 service/event 兼容性继续使用独立的 32 字节
  ABI 哈希。

## 已解决的阻断性问题

1. `KernelHost` 暴露独立的 `provide_service(ProviderKey, ...)` 与
   `register_listener(...)` 方法。guest 形态的 `RegistrationRequest` 在
   `InstanceHost` 内部解析；没有可空的 realm 跨越 host trait。
2. Timer 暴露 effect 所有的 `IntervalStream`、`Debouncer<T>` 与 `Throttler<T>`。父级或
   手动释放会终止流/调度器；被中断的 sleep 与释放后的调用使用 `TimerError::ContextDisposed`，
   而 disposer 失败使用 `TimerError::Cleanup`。
3. `Logger` 刻意做成并行的应用服务，而非 `tracing-subscriber` 的 Layer。
   运行时适配器可以向外发送到两个系统，但 tracing 永远不会被回馈到 Logger 中。
4. `BuiltinRegistry` 为 preflight 与运行时挂载都将 `builtin:<name>` 解析为
   `Arc<dyn ComponentFactory>`。重名会失败；built-in 共享生命周期，但不共享 artifact HMR。
5. `EntryTree::reconcile` 以逆序回滚已成功的操作。`cordis run` 监视配置本身，以事务方式
   提交有效的树，并且只在提交后才重建 artifact watch 目标。
6. 二进制解析器针对 check/run/inspect/build-component 加上 help/version 具有封闭的类型化
   命令集，并对有效与被拒绝的形态都有测试。
7. 可靠性范围记录在 `reliability.md`；已配置 nightly Miri 与依赖策略任务。Loom 不适用，
   因为已无自定义同步原语。
8. `benchmarks.md` 记录了生命周期与 Context 的基线。0.1.0 未接受任何针对优化的公开调节项。

## 仍需的仅发布证据

- crates.io 命名空间必须在发布前解决。当前名称 `cordis`、`cordis-core`、`cordis-loader`、
  `cordis-cli` 与 `cordis-timer` 已经存在，且列出的所有者并非仓库名称
  （`shigma` 或 `dshbox-dev`）。在生成发布提交之前，请确认发布者权限，或采用无冲突的包
  命名方案。

  2026-09-03 的一次后备命名空间探测发现以下名称可用。可用并不等于保留，必须在发布前立即
  重新检查。通过显式的 manifest targets，包重命名可以保留现有的 Rust 库与二进制名称。

  | 当前包 | 后备包 |
  | --- | --- |
  | `cordis` | `cordis-wasm` |
  | `cordis-core` | `cordis-wasm-core` |
  | `cordis-guest` | `cordis-wasm-guest` |
  | `cordis-loader` | `cordis-wasm-loader` |
  | `cordis-logger` | `cordis-wasm-logger` |
  | `cordis-macros` | `cordis-wasm-macros` |
  | `cordis-timer` | `cordis-wasm-timer` |
  | `cordis-wasm` | `cordis-wasm-host` |
  | `cordis-cli` | `cordis-wasm-cli` |

- 精确的发布提交必须通过 Linux/MSRV、macOS、Windows、nightly Miri、cargo-deny 与
  RustSec 任务。
- 叶子 crate 压缩包在本地验证通过。每个依赖 `.crate` 归档也都会生成，并从其解压后的包
  内容中编译，其中 `[patch.crates-io]` 仅指向解压后的 Cordis 依赖归档。Cargo 自身基于
  registry 的验证在每个依赖发布后完成。发布顺序为：`cordis-macros`、`cordis-core`、
  `cordis-guest`；然后是 loader/logger/timer；再是 wasm 与 facade；最后是 CLI。

本次审查中没有未解决的代码级 API 阻断项。发布仍需要上述的命名空间决策与外部证据；仅凭
工作区版本并不构成发布证据。
