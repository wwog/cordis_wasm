# 动态组件（host 桥接）

dynamic 路径是 core 与任何动态加载的组件之间的 runtime 无关桥接——目前就是 Wasmtime Component
Model，但这些 trait 刻意不含任何 Wasmtime 类型，因此 native "dynamic" factory 也能实现它们。
动态组件由 [supervisor](supervisor.zh.md) 以 `DynamicFiber` 挂在普通的 fiber 生命周期上，并且
只通过 `KernelHost` 接口与 host 通信。

该模块位于 `cordis_core::dynamic`，并从 `cordis_core` 再导出。

## `ComponentFuture`

```rust
pub type ComponentFuture<'a, T> = Pin<Box<dyn Future<Output = Result<T, CordisError>> + Send + 'a>>;
```

object-safe 动态组件边界使用的 owned 异步结果。

## `Capability`

```rust
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct Capability(Arc<str>);
impl Capability {
    pub fn new(value: impl Into<Arc<str>>) -> Self;
    pub fn as_str(&self) -> &str;
}
```

dynamic component manifest 请求的能力。在允许组件运行之前，host 对照其 policy 检查该能力（见
[wasm](wasm.zh.md) 的 `ArtifactPolicy`）。

## `DynamicComponentDescriptor`

```rust
#[derive(Clone, Debug)]
pub struct DynamicComponentDescriptor {
    pub name: Arc<str>,
    pub version: Arc<str>,
    pub kernel_abi: Arc<str>,
    pub injects: Vec<InjectSpec>,
    pub provides: Vec<ServiceId>,
    pub config_schema: Schema,
    pub capabilities: BTreeSet<Capability>,
}
```

由 loader、native adapter 与 WebAssembly factory 共享的 owned descriptor。它是 WIT
`plugin-descriptor` 的 runtime 无关形式：name/version/kernel ABI、它注入与提供的服务、它的
config schema、以及它需要的能力。

## `DynamicCall` 与 `EventCall`

```rust
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DynamicCall {
    pub service: ServiceId,
    pub method: u32,
    pub payload: Vec<u8>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct EventCall {
    pub event: EventId,
    pub listener_id: u64,
    pub mode: EventMode,
    pub payload: Vec<u8>,
    pub next_token: Option<u64>,
}
```

native/WebAssembly 边界上的 type-erased 请求。`DynamicCall` 指名 service、method id 与编码后的
payload。`EventCall` 再加上 listener id、dispatch 模式与 waterfall 的 `next_token`（穿过
waterfall 链的延续 token）。

## `EventReply`

```rust
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum EventReply {
    Continue(Vec<u8>),
    Break(Vec<u8>),
}
```

`ControlFlow` 的 dynamic 对应物，保留编码后的 event payload。

## `RegistrationRequest`

```rust
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum RegistrationRequest {
    Provide(ServiceId),
    Listen { event: EventId, listener_id: u64, mode: EventMode },
}
```

guest 请求的注册。清理的权威仍留在 host：`InstanceHost::register` 把一个请求变成 effect 守护的
注册，其逆由 host 一侧持有。

## `KernelHost`

```rust
pub trait KernelHost: Send + Sync + 'static {
    fn log(&self, fiber: FiberId, level: &str, message: &str);

    fn call_service(&self, fiber: FiberId, call: DynamicCall) -> ComponentFuture<'_, Vec<u8>>;

    fn dispatch_event(&self, fiber: FiberId, call: EventCall) -> ComponentFuture<'_, EventReply>;

    fn provide_service(&self, fiber: FiberId, key: ProviderKey, scope: EffectScope)
        -> ComponentFuture<'_, ()>;

    fn register_listener(&self, fiber: FiberId, event: EventId, listener_id: u64, mode: EventMode, scope: EffectScope)
        -> ComponentFuture<'_, ()>;
}
```

dynamic component instance 可用的 host 侧操作。guest 只通过这个接口到达 host。两个注册方法都
接收一个 `EffectScope`，并把注册的逆装进它——因此当宿主 effect dispose 时，provider/listener 会
被拆除。这就是论文的 §6.1 边界：获取（registration）是可逆的，发出（出站的 `call_service`
payload）则不是。

Wasmtime 实现位于 [wasm](wasm.zh.md)；loader 的 `RuntimeKernel` 是 [wasm-driver](wasm-driver.zh.md)
中的具体路由器。

## `InstanceHost`

```rust
#[derive(Clone)]
pub struct InstanceHost { /* fiber, runtime, context, effects, kernel: Arc<dyn KernelHost> */ }

impl InstanceHost {
    pub fn new(fiber: FiberId, runtime: RuntimeHandle, effects: EffectSet, kernel: Arc<dyn KernelHost>) -> Self;
    pub fn new_in_context(fiber: FiberId, runtime: RuntimeHandle, context: Context, effects: EffectSet, kernel: Arc<dyn KernelHost>) -> Self;

    pub const fn fiber(&self) -> FiberId;
    pub const fn runtime(&self) -> &RuntimeHandle;
    pub const fn effects(&self) -> &EffectSet;
    pub const fn context(&self) -> &Context;

    pub fn log(&self, level: &str, message: &str);
    pub async fn call_service(&self, call: DynamicCall) -> Result<Vec<u8>, CordisError>;
    pub async fn dispatch_event(&self, call: EventCall) -> Result<EventReply, CordisError>;
    pub async fn register(&self, request: RegistrationRequest) -> Result<EffectGuard, CordisError>;
}
```

传给 dynamic component factory 的 per-instance 权威。它携带 fiber id、runtime handle、context、
effect set 与 `KernelHost`。`new` 从 `Context::root(fiber)` 开始；`new_in_context` 使用调用方
提供的 context（`mount_dynamic` 用它扩展父 context）。

- `call_service` / `dispatch_event` 以该 instance 的 fiber 经由 kernel 路由。
- `register` 在向 guest 代码暴露注册 handle **之前**创建一个 `EffectGuard`。若 kernel 注册失败，
  guard 被 dispose 并返回错误。这正是 host effect 表是最终权威的原因：即使 guest drop 了它的
  `Registration` handle，host 侧的 guard 依然存在，而且即便 guest 从不 drop 它，`force_cleanup`
  也会清掉它。

**错误** —— `call_service`/`dispatch_event`：来自 kernel 的路由、依赖、codec 或组件错误。
`register`：inactive-effect 或 kernel 注册错误。

## `ComponentFactory`

```rust
pub trait ComponentFactory: Send + Sync + 'static {
    fn descriptor(&self) -> &DynamicComponentDescriptor;

    fn instantiate(&self, host: InstanceHost) -> ComponentFuture<'_, Box<dyn ComponentInstance>>;
}
```

native 或 WebAssembly dynamic component 的 runtime 无关 factory。`descriptor` 给 host 挂载组件
所需的全部元数据；`instantiate` 在给定 per-instance host 的情况下构建（但不激活）一个 instance。

## `ComponentInstance`

```rust
pub trait ComponentInstance: Send + 'static {
    fn activate(&mut self, config: Value) -> ComponentFuture<'_, ()>;
    fn deactivate(&mut self) -> ComponentFuture<'_, ()>;
    fn call_service(&mut self, call: DynamicCall) -> ComponentFuture<'_, Vec<u8>>;
    fn call_event(&mut self, call: EventCall) -> ComponentFuture<'_, EventReply>;
}
```

单个 component instance 的 runtime 无关生命周期与回调接口。四个方法都是 `&mut self`，
`DynamicFiber` 用 load/unload 将它们串行化，因此一个 instance（对 Wasmtime 来说还有它的
`Store`）绝不会被并发进入。

## `DynamicFiber`

```rust
#[derive(Clone)]
pub struct DynamicFiber { /* fiber, runtime, state: Arc<Mutex<DynamicFiberState>>, changed, reload, calls */ }

impl DynamicFiber {
    pub const fn fiber(&self) -> FiberId;
    pub async fn await_active(&self) -> Result<(), CordisError>;
    pub async fn replace(&self, factory: Arc<dyn ComponentFactory>, config: Value) -> Result<(), CordisError>;
    pub async fn call_service(&self, call: DynamicCall) -> Result<Vec<u8>, CordisError>;
    pub async fn call_event(&self, call: EventCall) -> Result<EventReply, CordisError>;
    pub async fn retire(&self) -> Result<(), CordisError>;
}
```

挂在普通 Supervisor 生命周期上的动态组件。通过这个 handle 的调用与 load 和 unload 串行化，
因此 instance 绝不会被并发进入。

### `await_active`

等待当前组件 revision 激活。返回激活失败，或在必需依赖让 fiber 一直等待时返回
`CordisError::InactiveFiber`。

### `replace`

通过一次 unload/load 重启替换 factory 与配置。失败的候选会停留在 fiber 的 `Failed` 状态，因此像
HMR 事务管理器这样的调用方可以显式恢复先前的 factory。重启失败时，旧的 factory/config/revision
会被恢复并返回错误。

### `call_service` / `call_event`

在 active instance 上调用 service/event export。在 active epoch 之外返回
`CordisError::InactiveFiber`，否则返回组件的 service 错误。

### `retire`

不可逆地退役该 fiber，并等待 instance/effect 清理。

### 重入 guard

`DynamicFiber` 把每次调用包在一个 `CallGate` 中。该 gate 是一个以*当前 task + 线程*为键的单槽
mutex：它拒绝**同 fiber 重入**（`CordisError::ReentrantCall`），以避免 Wasmtime Store 死锁，同时
仍允许 instance 从*不同的* task 重入自身。这是一个限制性但必要的补充，记录于
`docs/wasmtime-findings.md`；论文讨论了 inertia（§4.4）但不讨论重入。

## 挂载

`RuntimeHandle::mount_dynamic`（见 [supervisor](supervisor.zh.md)）是入口：

```rust
pub async fn mount_dynamic(
    &self,
    parent: Option<FiberId>,
    base_context: Option<&Context>,
    factory: Arc<dyn ComponentFactory>,
    kernel: Arc<dyn KernelHost>,
    config: Value,
) -> Result<DynamicFiber, CordisError>;
```

它创建一个 fiber，把 context `extend` 给它，安装一个在每次 load/unload 时运行
`run_dynamic_transition` 的 executor，并根据 factory descriptor 的 injects 配置该 fiber 的依赖。
返回的 handle 一开始可能正在等待必需 provider——当需要激活时使用 `await_active`。
