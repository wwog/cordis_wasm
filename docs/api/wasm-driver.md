# Wasm Application Driver (`cordis-wasm::loader`)

`cordis-wasm::loader` ties the Loader, the Supervisor, and Wasmtime into a single runnable
application. `WasmEntryDriver` implements `EntryDriver` over Supervisor-owned Wasmtime fibers;
`WasmApplication` owns one run of the whole thing; `check_entries` is the preflight-only path behind
`cordis check`.

This is the "declarative runtime closed loop": `WasmEntryDriver` binds Loader Entry, managed realm,
dynamic Fiber, Kernel routing, and HMR to the same lifecycle (`cordis check` only preflights, `run`
activates components).

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

Owns one runnable Loader + Supervisor + Wasmtime application.

- `new` creates an empty application rooted at `base_dir` with no builtins; `new_with_builtins` adds
  process-local built-in factories. Both build an engine, start a `Runtime`, create the root fiber,
  and wire a `WasmEntryDriver`.
- `reconcile` reconciles the application to a new declarative entry tree (through `EntryTree::reconcile`).
- `driver()` exposes the driver (for `logger()`, `reload_paths`, `artifact_paths`).
- `snapshot()` / `settle()` return the Supervisor snapshot; `settle` first waits for quiescence.
- `shutdown` stops all entries child-first, retires the root, and shuts the Supervisor down.

**Errors** — `new`/`new_with_builtins`: engine, Supervisor, or root fiber creation error.
`reconcile`: entry validation, component preflight, or lifecycle error. `shutdown`: the first cleanup
or Supervisor error after shutdown is attempted.

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

Executes Loader Entry operations against Supervisor-owned Wasmtime fibers. For each resolved entry it
resolves a factory (`builtin:`) or compiles a component (`file:`), validates the config against the
factory's schema, builds the entry context (extending the root context, then `isolate`-ing each
declared service into its realm, and `intercept`-ing any intercept values), mounts a `DynamicFiber`,
binds it in the kernel router, and (for file-backed entries) binds it into HMR tracking.

- `start_entry` mounts and, if the fiber is `Loading`/`Failed`, awaits activation; on any failure it
  untracks, unbinds, and retires the mount, returning a `LoaderError::Driver`.
- `stop_entry` retires the mounted fiber, unbinds it from the kernel router and HMR, and untracks its
  artifact.
- `update` stops the previous entry then starts the next — and rolls back (re-starting the previous)
  if the new one fails.
- `artifact_paths()` returns the canonical artifact paths currently tracked for HMR
  (`loader.rs:334`).

The `EntryDriver::update`/`stop` rollback is what makes a failed entry update transactional: the
previous artifact is restarted on failure, so a bad config or component never leaves the runtime in a
half-applied state.

## `BuiltinRegistry`

```rust
#[derive(Clone, Default)]
pub struct BuiltinRegistry { /* factories: Arc<RwLock<BTreeMap<String, Arc<dyn ComponentFactory>>>> */ }
impl BuiltinRegistry {
    pub fn register(&self, name: impl Into<String>, factory: Arc<dyn ComponentFactory>) -> Result<(), LoaderError>;
}
```

Process-local factories addressable through `builtin:<name>` Entry references. The embedder binds
names to factories here; builtins and WASM share the same `ComponentFactory` and Supervisor lifecycle
but do **not** enter artifact HMR.

**Errors** — `register` returns `LoaderError::Driver` for an empty or duplicate name.

## `RuntimeKernel` (KernelHost impl)

`RuntimeKernel` is the concrete `KernelHost` router shared by every dynamic Entry in one application.
It is private to the module but is the heart of guest routing.

```rust
struct RuntimeKernel {
    runtime: RuntimeHandle,
    logger: Logger,
    routes: RwLock<BTreeMap<FiberId, DynamicFiber>>,
    route_changed: Notify,
    listeners: RwLock<BTreeMap<(EventId, u64), FiberId>>,
}
```

- `log` maps the WIT level string to `LogLevel` and logs to both the `Logger` and `tracing`.
- `call_service` looks up the caller's committed view, resolves the provider, and routes the call to
  that provider's `DynamicFiber`. Undeclared service → `UndeclaredDependency`; absent provider →
  `MissingCommittedProvider`. This is §6.3 capability-based access control: a component accesses only
  what it declared.
- `dispatch_event` looks up the listener owner by `(event, listener_id)` and routes to it.
- `provide_service` occupies the provider slot (`runtime.provide`) and defers the **withdraw** as a
  `Disposer` into the guest's scope — so when the effect disposes, the provider slot is released. This
  is §3.2.1: provision is a revertible effect, and the host defers the inverse.
- `register_listener` adds the listener owner and defers its removal as a `Disposer`.

`bind`/`unbind` maintain the route table; `route` waits for a fiber to have a route or fails if the
fiber is `Failed`/`Disposed` or unknown. Because the host's `RuntimeKernel::provide_service` /
`register_listener` are what actually insert into the supervisor, the host effect table is the final
authority for cleanup — guest misbehavior cannot leak a registration.

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

The **preflight** path behind `cordis check`. It validates the entry tree and every referenced
component **without activating it**: for each `file:` entry it compiles the component with
`WasmComponentFactory::from_bytes` (which already does descriptor/WIT/capability checks), and for each
`builtin:` it looks up the registered factory; then it validates the entry config against the factory's
schema. The `PreflightDriver` counts entries and records component names into the `CheckReport`.

**Errors** — entry validation, artifact, ABI, capability, or config schema error. Because it runs
through `EntryTree::reconcile`, a preflight failure rolls back whatever the preflight driver already
did (though the preflight driver's `stop` is a no-op).

## The `cordis.json` entry format

An application config document (`cordis.json`/`.yaml`) whose root is an entry array or an object with
an `entries` array. The CLI's example `examples/wasm-app/cordis.json`:

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

Here both entries join the `example` global realm for `example.counter`, so the provider's
provide and the consumer's inject resolve to the same realm key and the consumer activates against
the provider. Note `isolate` values are strings (`"example"`), which deserialize into
`IsolationRule::Global("example")`.
