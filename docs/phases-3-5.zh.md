# Phase 3–5 实现笔记

## 设计校准

实现将 Wasmtime 保持在 `cordis-core` 之外。Core 现在拥有对象安全的
`ComponentFactory`、`ComponentInstance` 与 `KernelHost` 边界；native 适配器、
Wasmtime host、Loader 与 HMR 都依赖该边界。这修正了旧版计划中暗示 Wasmtime 适配器
自身应拥有 Fiber 生命周期状态的表述。Fiber 身份、effect 所有权、依赖解析与强制清理
仍由 Core 作为权威。

`RuntimeHandle::mount_dynamic` 是生命周期桥接点：它创建/配置一个普通的
Supervisor Fiber，安装与运行时无关的 executor，在 load 时提交依赖，并让 instance 调用
对 activate/deactivate 串行化。每个 load epoch 拥有全新的 EffectSet；
构造或激活失败时会在 Fiber 落入 `Failed` 之前将其释放。活跃替换使用显式的
`reload_fiber` 卸载/加载路径，而 `restart_fiber` 保留其更窄的含义——重试已失败的 Fiber。

Kernel ABI 的唯一来源是 `wit/kernel.wit`。host 与 guest 的绑定都从中生成。
服务方法由稳定的 `u32` ID 加上 MessagePack 载荷与 ABI hash 组成；
事件携带其 dispatch 模式以及一个可选的、host 拥有的 waterfall 续传 token。

## Phase 3：Wasmtime 与 guest SDK

- 每个 component instance 拥有独立的 async Store、WASIp2 上下文、ResourceTable、
  registration 表、task group、fuel 预算、epoch deadline 以及内存/资源限制。
- WASIp2 启动时不继承任何文件系统、环境、stdio 或网络权限。
  虚拟化的单调时钟与已关闭/已消费的 CLI 流是基线运行时 import。
  文件系统、socket/HTTP、random 与 wall-clock import 必须由嵌入的 descriptor
  声明，并获 host 策略允许。显式 preopen 拒绝绝对 guest 路径与父目录穿越。
  这是 Wasmtime 48 可实现的边界；旧计划中 `WasiCtx` 可以省略所有时钟/RNG 实现的
  笼统说法并不正确，因为 WASI 本身就要求实现这些接口。
- Descriptor 检查在激活之前进行，并检查 Kernel ABI、所请求的能力、
  服务 ABI hash 长度以及 JSON Schema 语法。
- host import 在经由 `KernelHost` 路由之前验证 Fiber/effect 上下文与载荷大小。
  Registration handle 在返回成功之前会创建一个 Core effect。
  guest 的资源 drop 清理单个 effect；即使 guest 泄漏了 handle，Fiber teardown
  也会释放完整的 EffectSet。
- Wasmtime spike 证明同 Store 的 async 重入在技术上是可行的，但生产环境的
  动态 handle 刻意用 `ReentrantCall` 拒绝 guest -> host -> 同 Fiber 的循环，
  以免冒 instance 锁死锁的风险。WIT 为未来 host 拥有的 one-shot/过期续传机制
  保留了 `next-token`；受控的跨边界 waterfall 重入暂不公开。
  in-flight 的 guest future 不得被 drop：必须使用协作式取消或销毁整个 Store。
- `cordis-guest` 提供生成的绑定、MessagePack 辅助函数与 `export_plugin!`。
  `cargo run -p xtask -- build-guests` 通过真实的 Supervisor 生命周期构建并挂载
  provider/consumer，以针对 `wasm32-wasip2` 产物做端到端验证。

## Phase 4：Loader 与 Include

`EntryTree` 是声明的事实来源。它计算键控 diff，并且只向 `EntryDriver`
发送所需的先子后父的 stop、update 以及先父后子的 start。
组禁用是继承的。本地托管 realm 以 Entry ID 为键，因此在移动后依然存活；
全局 realm 以服务加用户标签为键。Intercept 与托管 realm 是继承的，
只有受影响的子孙才会收到更新。

Component 引用必须显式使用 `builtin:` 或 `file:`。动态配置在 driver 调用之前
会对照 Draft 2020-12 schema 检查。Include 接受 JSON 与 YAML 的 entry 数组，
按稳定 ID 应用有序的 merge/replace/remove/insert patch，检测 target-ID 不匹配，
检测只读文件，并先写入一个已同步的兄弟临时文件再 rename。

YAML 为 Rhai 表达式保留 `!expr`。求值只接收一份 JSON 快照作为 `ctx`，
只接受单个表达式，有操作/深度/容器限制，并禁用求值、模块导入/导出、
函数、循环与异常语法。没有注册任何文件系统、网络、进程或 host 对象。

## Phase 5：WASM HMR

`HmrWatcher` 使用 `notify-debouncer-mini`，并在每个批次内对路径去重。
manager 将产物字节连同 ABI/能力/WASI 策略、资源预算、运行时版本、操作系统与
架构一起哈希。其编译后的 factory 缓存是有界的 LRU，并带有命中/未命中/逐出指标；
序列化的 Wasmtime 产物绝不会被不安全地反序列化。
`FiberReloadRuntime` 将 Loader Entry ID 绑定到 `DynamicFiber` handle，因此事务性的
replace/restore 操作执行与依赖变更相同的 Supervisor 卸载/加载生命周期，
而不是替换一个带外的 instance 指针。

Reload 在批次边界上是事务性的：

1. 读取并编译每个变更的产物。
2. 实例化仅 descriptor 的候选，并校验 WIT ABI/能力。
3. 若任一预检失败，则保持所有活跃 Entry 不变。
4. 按稳定 ID 顺序替换受影响的 Entry。
5. 若 apply 失败，按逆序恢复失败的 Entry 与其之前的所有 Entry。
6. 只有整个批次成功后才会发布新的当前 hash；显式报告回滚失败，
   且不触碰无关的 Entry。
