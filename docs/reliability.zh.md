# 0.1.0 可靠性范围

## 确定性与故障覆盖

- Fiber 生命周期：128 个种子 × 256 个生成操作，覆盖期望状态变更、成功与失败的完成、
  过期代、重启、重载以及不可逆的退休。
- 激活与关闭：动态组件测试注入激活失败、清理失败、panic、依赖撤回、部分 provider 注册与
  卸载失败。
- 路由：service ABI、缺失/未声明/不活动的依赖、payload 上限、可重入调用、事件监听器查找与
  guest trap，均在本机与 Wasmtime 边界得到覆盖。
- 资源限制：fuel、epoch 中断、线性内存、注册数量、能力策略以及显式/强制注册清理均被覆盖。
- Loader/HMR：非法 schema、损坏/写入一半的组件、watcher rename 事件、未变更的哈希、
  应用失败、逆向回滚、回滚失败以及多操作配置回滚，均被确定性注入。

## Miri 与 Loom 边界

Miri 不为固定的 stable 1.98.0 工具链发布。因此 CI 会安装 nightly 加 Miri 组件，并运行
Core effect 与 tracked-collection 测试套件，遵循 Miri 的受支持工具链模型。两个套件在本地
Miri 下均通过：11 个 effect 测试与 2 个 tracked-collection 测试。Tokio 功能按 crate 选择，
因此 Core 目标不会仅仅因为 CLI 需要它们而初始化不支持的 signal 或 I/O driver。

Loom 在 API 审查后不适用于 0.1.0 实现：Cordis 不包含自定义的无锁算法、手写的唤醒协议、
`unsafe` 块或依赖内存序的状态机。协调委托给 Tokio 的 channel/Notify/Mutex 与
`std::sync::Mutex`；唯一的原子操作以 relaxed 顺序分配单调的诊断 ID，且不同步任何数据。
如果将来引入自定义并发原语，Loom 模型将成为合并它的前提条件。
