# Cordis-Wasm（Wasmtime Host）

`cordis-wasm` 是 Wasmtime Component Model 集成。它编译、校验并 host 实现 Cordis kernel world 的
WebAssembly 组件，强制每个插件的 resource 与执行预算、capability 策略以及版本化的两层 ABI。一个插件就是
一个 Wasmtime *Component*；guest 只能通过 `cordis:kernel@0.1.0` WIT 接口触达 host。

两层 ABI：

- **Kernel WIT** 是固定且版本化的；它只描述生命周期、注册、调用、事件与错误的管道（plumbing）。
- **业务服务协议**由 `#[cordis::service]` 生成，并以 MessagePack 承载于 kernel 的 `call-service` 之上；
  ABI hash 防止同名但互不兼容的服务相互满足。

## `WasmEngine`

```rust
#[derive(Clone, Debug)]
pub struct WasmEngine { /* engine: Engine */ }

impl WasmEngine {
    pub fn new() -> Result<Self, WasmHostError>;
    pub const fn engine(&self) -> &Engine;
    pub fn compile(&self, bytes: impl AsRef<[u8]>) -> Result<Component, WasmHostError>;
    pub fn new_store<T>(&self, host: T, limits: &WasmLimits) -> Result<Store<StoreState<T>>, WasmHostError>;
    pub fn prepare_call<T>(&self, store: &mut Store<StoreState<T>>) -> Result<(), WasmHostError>;
}
```

所有插件 store 共享的组件编译器配置。`new` 构建一个启用了 fuel 与 epoch 中断的异步 Component Model
engine，并且**没有任何环境 WASI capabilities**（capabilities 来自 `ArtifactPolicy.wasi`）。

- `compile` 编译并校验一个组件。
- `new_store` 用来自 `WasmLimits` 的 `StoreLimits` 创建一个受限的 `Store`，并执行一次 `prepare_call`。
- `prepare_call` 在 guest 调用之前立即重新装配 fuel 与 epoch 截止时间：设置 fuel、设置 epoch 截止时间，
  并启用 deadline trap。

**Errors** — 当 Wasmtime 拒绝配置/字节，或 fuel 配置失败时，三者都会返回 `WasmHostError::Engine`。

## `WasmLimits`

```rust
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct WasmLimits {
    pub fuel_per_call: u64,
    pub epoch_deadline_ticks: u64,
    pub max_memory_bytes: usize,
    pub max_table_elements: usize,
    pub max_instances: usize,
    pub max_tables: usize,
    pub max_memories: usize,
    pub max_registrations: usize,
    pub max_payload_bytes: usize,
}
```

每个插件的 resource 与执行预算。`Default` 授予：`fuel_per_call: 10_000_000`、
`epoch_deadline_ticks: 1`、`max_memory_bytes: 64 MiB`、`max_table_elements: 10_000`、
`max_instances/tables/memories: 32`、`max_registrations: 10_000`、`max_payload_bytes: 1 MiB`。

## `StoreState<T>`

```rust
#[derive(Debug)]
pub struct StoreState<T> { /* host: T, limiter: StoreLimits, fuel_per_call, epoch_deadline_ticks, active_registrations, max_registrations */ }

impl<T> StoreState<T> {
    pub const fn host(&self) -> &T;
    pub const fn host_mut(&mut self) -> &mut T;
    pub const fn active_registrations(&self) -> usize;
    pub fn reserve_registration(&mut self) -> Result<(), WasmHostError>;
    pub fn release_registration(&mut self) -> Result<(), WasmHostError>;
}
```

Store 持有的 host 状态与可强制执行的预算。注册计数器跟踪 host 撰写的注册（provider/listener），因此即使
guest 从不 drop 其句柄，host 也能强制执行 `max_registrations`。

**Errors** — 达到配置的限制时，`reserve_registration` 返回 `WasmHostError::RegistrationLimitExceeded { limit }`；
对于不匹配的释放，`release_registration` 返回 `WasmHostError::RegistrationCountUnderflow`。

## `ArtifactPolicy`

```rust
#[derive(Clone, Debug)]
pub struct ArtifactPolicy {
    pub kernel_abi: String,
    pub allowed_capabilities: BTreeSet<Capability>,
    pub wasi: WasiCapabilities,
}
impl Default for ArtifactPolicy {
    /* kernel_abi: "0.1", allowed_capabilities: empty, wasi: WasiCapabilities::deny_all() */
}
```

组件成为候选之前执行的 descriptor 与 capability 检查。默认拒绝一切：空的 capability 集合且没有任何 WASI
preopen。

## `WasmComponentFactory`

```rust
#[derive(Clone, Debug)]
pub struct WasmComponentFactory { /* engine, component: Arc<Component>, descriptor, limits, policy */ }

impl WasmComponentFactory {
    pub async fn from_bytes(
        engine: WasmEngine, bytes: impl AsRef<[u8]>, limits: WasmLimits, policy: ArtifactPolicy,
    ) -> Result<Self, WasmHostError>;
    pub fn component(&self) -> &Component;
    pub const fn policy(&self) -> &ArtifactPolicy;
}
impl ComponentFactory for WasmComponentFactory { /* descriptor(), instantiate(&host) */ }
```

一个实现 Cordis kernel world 的、已校验并编译的 WebAssembly 组件。`from_bytes` 编译、链接、实例化，并
**查询 descriptor**，但不激活 guest — 因此 host 在运行任何代码之前就得知组件的名字、injects、provides、
config schema 与 capabilities。随后它把 descriptor（kernel ABI、capabilities）与 WASI imports 对照策略
校验。

**Errors** — `from_bytes` 返回编译、链接、descriptor、ABI 或 capability 策略错误。

## `WasiCapabilities` / `WasiPreopen`

```rust
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct WasiPreopen {
    pub host_path: PathBuf,
    pub guest_path: String,
    pub writable: bool,
}
impl WasiPreopen {
    pub fn read_only(host_path: impl Into<PathBuf>, guest_path: impl Into<String>) -> Self;
    pub fn read_write(host_path: impl Into<PathBuf>, guest_path: impl Into<String>) -> Self;
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct WasiCapabilities { /* preopens: Vec<WasiPreopen> */ }
impl WasiCapabilities {
    pub fn deny_all() -> Self;
    #[must_use] pub fn with_preopen(mut self, preopen: WasiPreopen) -> Self;
    pub fn preopens(&self) -> &[WasiPreopen];
}
```

`WASIp2` 策略。其默认值（`deny_all`）不授予任何环境进程、文件系统或网络访问权；你按 preopen 逐个加入。
`with_preopen` 添加一个。guest 的 preopen 路径必须是**相对的且不含 `..`** — `build` 拒绝绝对路径与
`ParentDir` 分量，因此 preopen 无法逃出 guest 的虚拟树。

## `GuestTaskGroup`

```rust
#[derive(Debug, Default)]
pub struct GuestTaskGroup { /* tasks: Vec<JoinHandle<()>> */ }

impl GuestTaskGroup {
    pub fn spawn(&mut self, task: impl Future<Output = ()> + Send + 'static);
    pub fn len(&self) -> usize;
    pub fn is_empty(&self) -> bool;
    pub async fn shutdown(&mut self);
}
```

绑定到单个实例的 host 持有的任务。**在 Store 被丢弃之前，host 持有的任务总是会被 abort 并 join** —
`shutdown` abort 每个任务并等待每个句柄，因此把引用泄漏进 Store 的任务不可能活得比它更久。

## `cordis:kernel@0.1.0` 的 WIT world

权威源是 `crates/cordis-guest/wit/kernel.wit`；`cordis-wasm` 在 `crates/cordis-wasm/wit/kernel.wit` 下
打包了一份逐字节一致的副本，并有测试断言二者逐字节相同。

world `cordis-plugin` 有两个接口：

### `host`（由插件 import）

| 函数 | 签名 |
|---|---|
| `log` | `(context: call-context, level: string, message: string)` |
| `call-service` | `(context, service: service-id, method: u32, payload: list<u8>) -> result<list<u8>, kernel-error>` |
| `provide-service` | `(context, service: service-id) -> result<registration, kernel-error>` |
| `register-listener` | `(context, event: event-id, listener-id: u64, mode: event-mode) -> result<registration, kernel-error>` |
| `dispatch-event` | `(context, event: event-id, listener-id: u64, mode: event-mode, payload: list<u8>, next-token: option<u64>) -> result<event-reply, kernel-error>` |

`call-context` 是 `{ fiber-id: u64, effect-id: u64 }`；host 会在每次调用前把它对照当前 Store 校验
（若不属于该 Store 则返回 `InvalidArgument`）。`event-mode` 是五变体枚举。`event-reply` 是
`continue-value`/`break-value`，各自是一个 `list<u8>`。`registration` 是一个 `resource` — 正是它的析构
函数（`drop`）释放 host 侧的注册。

### `plugin`（由插件 export）

| 函数 | 签名 |
|---|---|
| `descriptor` | `() -> plugin-descriptor` |
| `activate` | `(context, config: list<u8>) -> result<_, kernel-error>` |
| `deactivate` | `(context) -> result<_, kernel-error>` |
| `call-service` | `(context, service, method, payload) -> result<list<u8>, kernel-error>` |
| `handle-event` | `(context, event, listener-id, mode, payload, next-token) -> result<event-reply, kernel-error>` |

`plugin-descriptor` 声明 `name`、`version`、`wit-version`（即 kernel ABI）、以 `list<service-id>` 形式
给出的 `inject`/`provide`、以 `list<u8>`（JSON 字节）形式给出的 `config-schema`，以及以 `list<string>`
形式给出的 `capabilities`。

## `WasmHostError`

```rust
#[derive(Debug, thiserror::Error)]
pub enum WasmHostError {
    #[error("wasmtime component error: {0}")] Engine(#[from] wasmtime::Error),
    #[error("guest registration limit {limit} exceeded")] RegistrationLimitExceeded { limit: usize },
    #[error("guest registration count underflow")] RegistrationCountUnderflow,
    #[error("invalid component descriptor: {message}")] Descriptor { message: String },
    #[error("kernel ABI mismatch: expected {expected}, got {actual}")] KernelAbiMismatch { expected: String, actual: String },
    #[error("component capability `{capability}` is denied")] CapabilityDenied { capability: String },
    #[error("invalid WASI capability: {message}")] Capability { message: String },
}
```

host 侧的错误枚举。`Engine` 包装任何 Wasmtime 错误。

## Kernel bindings 说明

两个 `bindings` 模块（`bindings::cordis::kernel::host`、`bindings::exports::cordis::kernel::plugin`）
在构建时由 `wit_bindgen`（host）与 `wit-bindgen::generate!`（guest）生成。它们把 WIT 类型暴露为 Rust —
例如 `host::KernelError`、`host::EventMode`、`plugin::Guest`、`plugin::PluginDescriptor`。
