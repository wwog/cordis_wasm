# Wasm 应用 driver（`cordis-wasm::loader`）

`cordis-wasm::loader` 把 Loader、Supervisor 与 Wasmtime 串成一个可运行的应用程序。`WasmEntryDriver`
在 Supervisor 持有的 Wasmtime fiber 之上实现 `EntryDriver`；`WasmApplication` 拥有整件事的一次运行；
`check_entries` 是 `cordis check` 背后的纯预检路径。

这就是"声明式运行时闭环"：`WasmEntryDriver` 把 Loader Entry、managed realm、动态 Fiber、Kernel 路由与
HMR 绑定到同一个生命周期（`cordis check` 只做预检，`run` 才激活组件）。

## `WasmApplication`

```rust
pub struct WasmApplication { /* runtime, root, tree, driver */ }
impl WasmApplication {
    pub async fn new(base_dir: impl Into<PathBuf>, limits: WasmLimits, policy: ArtifactPolicy) -> Result<Self, LoaderError>;
    pub async fn new_with_builtins(base_dir, limits, policy, builtins: BuiltinRegistry) -> Result<Self, LoaderError>;
    pub async fn reconcile(&mut self, entries: Vec<EntrySpec>) -> Result<(), LoaderError>;
    pub const fn driver(&self) -> &Arc<WasmEntryDriver>;
    pub async fn snapshot(&self) -> Result<RuntimeSnapshot, LoaderError>;
    pub async fn settle(&self) -> Result<RuntimeSnapshot, LoaderError>;
    pub async fn shutdown(self) -> Result<RuntimeSnapshot, LoaderError>;
}
```

拥有一个可运行的 Loader + Supervisor + Wasmtime 应用。

- `new` 创建一个以 `base_dir` 为根、没有任何 builtins 的空应用；`new_with_builtins` 额外添加进程内的
  built-in factories。两者都会构建 engine、启动 `Runtime`、创建 root fiber，并接好一个 `WasmEntryDriver`。
- `reconcile` 把应用 reconcile 到一棵新的声明式 entry 树（经由 `EntryTree::reconcile`）。
- `driver()` 暴露 driver（用于 `logger()`、`reload_paths`、`artifact_paths`）。
- `snapshot()` / `settle()` 返回 Supervisor 快照；`settle` 会先等待静止（quiescence）。
- `shutdown` 以子优先（child-first）的顺序停止所有 entries，退役 root，然后关闭 Supervisor。

**Errors** — `new`/`new_with_builtins`：engine、Supervisor 或 root fiber 创建错误。`reconcile`：entry
校验、组件预检或生命周期错误。`shutdown`：在尝试 shutdown 之后出现的第一个清理或 Supervisor 错误。

## `WasmEntryDriver`

```rust
pub struct WasmEntryDriver { /* runtime, root, root_context, base_dir, builtins, kernel, reload, hmr, realms, entries */ }
impl WasmEntryDriver {
    pub async fn artifact_paths(&self) -> Vec<PathBuf>;
    pub fn logger(&self) -> &Logger;
    pub async fn reload_paths(&self, paths: impl IntoIterator<Item = PathBuf>) -> ReloadReport;
}
impl EntryDriver for WasmEntryDriver { /* start, update, stop */ }
```

针对 Supervisor 持有的 Wasmtime fiber 执行 Loader Entry 操作。对每个已解析的 entry，它解析出一个 factory
（`builtin:`）或编译一个组件（`file:`），把 config 对照 factory 的 schema 校验，构建 entry context
（扩展 root context，然后把每个声明的 service `isolate` 进其 realm，并对任何 intercept 值执行
`intercept`），挂载一个 `DynamicFiber`，把它绑定进 kernel 路由表，并（对文件型 entry）把它绑定进 HMR
跟踪。

- `start_entry` 执行挂载，若 fiber 处于 `Loading`/`Failed` 则等待激活；任何失败时它都会取消跟踪、解除绑定
  并退役该挂载，返回 `LoaderError::Driver`。
- `stop_entry` 退役已挂载的 fiber，把它从 kernel 路由表与 HMR 解除绑定，并取消跟踪其 artifact。
- `update` 停止前一个 entry，再启动下一个 — 若新 entry 失败则回滚（重新启动前一个）。
- `artifact_paths()` 返回当前为 HMR 跟踪的规范 artifact 路径（`loader.rs:334`）。

`EntryDriver::update`/`stop` 的回滚正是让失败的 entry 更新具备事务性的原因：失败时会重启之前的 artifact，
因此坏的 config 或组件绝不会让运行时停留在半应用状态。

## `BuiltinRegistry`

```rust
#[derive(Clone, Default)]
pub struct BuiltinRegistry { /* factories: Arc<RwLock<BTreeMap<String, Arc<dyn ComponentFactory>>>> */ }
impl BuiltinRegistry {
    pub fn register(&self, name: impl Into<String>, factory: Arc<dyn ComponentFactory>) -> Result<(), LoaderError>;
}
```

可通过 `builtin:<name>` Entry 引用寻址的进程内 factories。嵌入方在这里把名字绑定到 factories；builtins 与
WASM 共享同一个 `ComponentFactory` 与 Supervisor 生命周期，但**不会**进入 artifact HMR。

**Errors** — 名字为空或重复时，`register` 返回 `LoaderError::Driver`。

## `RuntimeKernel`（KernelHost 实现）

`RuntimeKernel` 是同一应用内每个动态 Entry 共享的具体 `KernelHost` 路由表。它对模块是私有的，但却是 guest
路由的核心。

```rust
struct RuntimeKernel {
    runtime: RuntimeHandle,
    logger: Logger,
    routes: RwLock<BTreeMap<FiberId, DynamicFiber>>,
    route_changed: Notify,
    listeners: RwLock<BTreeMap<(EventId, u64), FiberId>>,
}
```

- `log` 把 WIT 的 level 字符串映射到 `LogLevel`，并同时记录到 `Logger` 与 `tracing`。
- `call_service` 查找调用者的 committed view，解析 provider，并把调用路由到该 provider 的
  `DynamicFiber`。未声明的 service → `UndeclaredDependency`；provider 缺失 → `MissingCommittedProvider`。
  这就是 §6.3 的基于能力的访问控制：组件只能访问它声明过的内容。
- `dispatch_event` 通过 `(event, listener_id)` 查找 listener 所有者并路由到它。
- `provide_service` 占据 provider 槽位（`runtime.provide`），并把 **withdraw** 作为一个 `Disposer`
  延迟进 guest 的作用域 — 因此当 effect 被 dispose 时，provider 槽位即被释放。这就是 §3.2.1：provision
  是可逆 effect，host 延迟的是它的逆。
- `register_listener` 登记 listener 所有者，并把它的移除作为一个 `Disposer` 延迟。

`bind`/`unbind` 维护路由表；`route` 等待某个 fiber 拥有路由，若 fiber 处于 `Failed`/`Disposed` 或未知则
失败。由于真正插入 supervisor 的是 host 的 `RuntimeKernel::provide_service` / `register_listener`，host
effect 表就是清理的最终权威 — guest 的不当行为无法泄漏注册。

## `check_entries` / `check_entries_with_builtins`

```rust
pub struct CheckReport {
    pub entries: usize,
    pub components: BTreeSet<String>,
}

pub async fn check_entries(
    base_dir: impl Into<PathBuf>, entries: Vec<EntrySpec>,
    limits: WasmLimits, policy: ArtifactPolicy,
) -> Result<CheckReport, LoaderError>;

pub async fn check_entries_with_builtins(
    base_dir: impl Into<PathBuf>, entries: Vec<EntrySpec>,
    limits: WasmLimits, policy: ArtifactPolicy, builtins: BuiltinRegistry,
) -> Result<CheckReport, LoaderError>;
```

`cordis check` 背后的**预检（preflight）**路径。它校验 entry 树与每个被引用的组件，但**不激活它们**：对每个
`file:` entry，用 `WasmComponentFactory::from_bytes` 编译该组件（它已经完成了 descriptor/WIT/capability
检查）；对每个 `builtin:` entry，则查找已注册的 factory；然后把 entry config 对照 factory 的 schema 校验。
`PreflightDriver` 统计 entries，并把组件名字记录进 `CheckReport`。

**Errors** — entry 校验、artifact、ABI、capability 或 config schema 错误。由于它经由
`EntryTree::reconcile` 运行，一次预检失败会回滚预检 driver 已经做的任何事（尽管预检 driver 的 `stop` 是
no-op）。

## `cordis.json` 的 entry 格式

应用配置文档（`cordis.json`/`.yaml`），其根要么是一个 entry 数组，要么是一个带 `entries` 数组的对象。
CLI 的示例 `examples/wasm-app/cordis.json`：

```json
{
  "entries": [
    {
      "id": "consumer",
      "component": "file:../../target/wasm32-wasip2/debug/wasm_counter_consumer.wasm",
      "config": {},
      "isolate": { "example.counter": "example" }
    },
    {
      "id": "provider",
      "component": "file:../../target/wasm32-wasip2/debug/wasm_counter_provider.wasm",
      "config": {},
      "isolate": { "example.counter": "example" }
    }
  ]
}
```

这里两个 entry 都加入 `example.counter` 的 `example` 全局 realm，因此 provider 的 provide 与 consumer 的
inject 解析到同一个 realm key，consumer 得以针对 provider 激活。注意 `isolate` 的值是字符串
（`"example"`），它们会被反序列化为 `IsolationRule::Global("example")`。
