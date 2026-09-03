# 0.1.0 TypeScript 行为对等性

Rust 重写保留了 Cordis 的生命周期语义，而非 JavaScript 对象机制或二进制兼容性。
该矩阵来源于 `cordis-ts/packages/*/tests` 下的参考测试。

| 参考行为 | Rust 0.1.0 状态 | 证据 / 刻意差异 |
|---|---|---|
| effect 释放、手动释放、LIFO 清理、async iterator 清理、失败延续 | 等价且更严格 | `cordis-core::effect` 增加了 exactly-once 并发释放、panic 隔离与聚合失败。 |
| Fiber 惰性、失败的激活、显式恢复、依赖驱动的重载 | 等价 | `FiberMachine` 与 Supervisor 测试覆盖了合并、过期代、失败恢复、consumer 优先的关闭以及生成的状态转换序列。 |
| 必需/可选 service 注入与 provider 替换 | 等价 | 已提交的 epoch 防止激活中途的依赖漂移；provider 身份是所属的 Fiber，而非 JS 对象身份。 |
| Context 隔离与共享 realm 标签 | 等价 | 不可变 overlay 加上 Loader 的 local/global 托管 realm 取代了原型突变。 |
| 事件 emit、parallel、serial、bail、waterfall、prepend、once/effect 所有权 | 等价 | Rust 事件是类型化的；waterfall 在编译期强制要求输入/输出类型一致，并在运行期强制 one-shot `Next`。 |
| 方法级注入 | 生命周期等价，类型更严格 | 每个被注解的方法拥有一个子 Fiber 与 EffectSet；依赖是生成的类型化字段，而非 decorator/proxy。 |
| Loader create/update/move/remove、分组、disable 传播、intercept、自更新 | 等价 | Keyed reconciliation 额外在批处理范围内是事务性的。 |
| Include JSON/YAML、merge/group/insert 补丁、表达式、写回 | 等价的受支持子集 | Rhai 取代 JavaScript 求值，并刻意在受限作用域中运行。 |
| HMR 批处理、未变更文件去重、应用回滚、依赖重载 | 生命周期等价 | 动态代码仅能是 Wasmtime Component；不存在 Node 模块缓存或链接的 JavaScript 文件图。 |
| Timer timeout、interval iterator、throttle、debounce、释放 | 意图等价 | Rust 暴露 effect 所有的句柄与一个 `Stream`；释放会干净地终止 interval 流，被取消的 sleep 返回 `TimerError::ContextDisposed`。 |
| Logger 缓冲区、target 过滤、exporter 释放 | Core 等价 | `Logger` 是一个并行的应用服务，而 Rust 生态的诊断仍保留在 `tracing` 中；它不安装递归的 subscriber 桥。 |
| JS service 遮蔽、Proxy 反射、prototype/关联属性注入 | 未按字面移植 | 类型化客户端、显式 Context、intercept 元数据与宏生成的依赖字段提供了 Rust 原生的边界。 |
| 任意 JS 插件值与可调用 mixin | 有意排除 | 原生组件实现类型化 trait；动态插件使用带版本号的 Kernel WIT 与 MessagePack 业务协议。 |

任何未来的对等性声明都必须指明可观察行为及其对应的 Rust 测试。匹配某个没有 Rust 安全性或
API 收益的 JavaScript 实现细节，不构成兼容性要求。
