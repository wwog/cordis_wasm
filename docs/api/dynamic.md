# Dynamic Components (Host Bridge)

The dynamic path is the runtime-neutral bridge between the core and any dynamically loaded
component — today that is the Wasmtime Component Model, but the traits are deliberately free of any
Wasmtime type so a native "dynamic" factory could implement them too. A dynamic component is
mounted by the [supervisor](supervisor.md) as a `DynamicFiber` on the ordinary fiber lifecycle, and
talks to the host exclusively through the `KernelHost` interface.

This module lives in `cordis_core::dynamic` and is re-exported from `cordis_core`.

## `ComponentFuture`

```rust
pub type ComponentFuture<'a, T> = Pin<Box<dyn Future<Output = Result<T, CordisError>> + Send + 'a>>;
```

Owned asynchronous result used by object-safe dynamic component boundaries.

## `Capability`

```rust
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct Capability(Arc<str>);
impl Capability {
    pub fn new(value: impl Into<Arc<str>>) -> Self;
    pub fn as_str(&self) -> &str;
}
```

A capability requested by a dynamic component manifest. The host checks it against its policy before
the component is allowed to run (see [wasm](wasm.md) `ArtifactPolicy`).

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

Owned descriptor shared by loaders, native adapters, and WebAssembly factories. It is the
runtime-neutral form of the WIT `plugin-descriptor`: name/version/kernel ABI, the services it injects
and provides, its config schema, and the capabilities it needs.

## `DynamicCall` and `EventCall`

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

Type-erased requests at a native/WebAssembly boundary. A `DynamicCall` names the service, the method
id, and the encoded payload. An `EventCall` adds the listener id, the dispatch mode, and the
waterfall `next_token` (the continuation token threaded through a waterfall chain).

## `EventReply`

```rust
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum EventReply {
    Continue(Vec<u8>),
    Break(Vec<u8>),
}
```

Dynamic equivalent of `ControlFlow`, retaining the encoded event payload.

## `RegistrationRequest`

```rust
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum RegistrationRequest {
    Provide(ServiceId),
    Listen { event: EventId, listener_id: u64, mode: EventMode },
}
```

Registration requested by a guest. The host remains authoritative for cleanup: `InstanceHost::register`
turns a request into an effect-guarded registration whose inverse is held on the host side.

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

Host-side operations available to a dynamic component instance. The guest reaches the host only
through this interface. The two registration methods both take an `EffectScope` and install the
registration's inverse in it — so a provider/listener is torn down when the hosting effect disposes.
This is the paper's §6.1 boundary: acquisition (registration) is revertible, emission (the outbound
`call_service` payload) is not.

A Wasmtime implementation is in [wasm](wasm.md); the loader's `RuntimeKernel` is the concrete router
in [wasm-driver](wasm-driver.md).

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

Per-instance authority passed to a dynamic component factory. It carries the fiber id, the runtime
handle, the context, the effect set, and the `KernelHost`. `new` starts from `Context::root(fiber)`;
`new_in_context` uses a caller-provided context (used by `mount_dynamic` to extend the parent).

- `call_service` / `dispatch_event` route through the kernel with this instance's fiber.
- `register` creates an `EffectGuard` **before** exposing a registration handle to guest code. If the
  kernel registration fails, the guard is disposed and the error returned. This is why the host effect
  table is the final authority: a guest that drops its `Registration` handle still has the host-side
  guard, and `force_cleanup` clears it even if the guest never drops it.

**Errors** — `call_service`/`dispatch_event`: routing, dependency, codec, or component errors from the
Kernel. `register`: an inactive-effect or Kernel registration error.

## `ComponentFactory`

```rust
pub trait ComponentFactory: Send + Sync + 'static {
    fn descriptor(&self) -> &DynamicComponentDescriptor;

    fn instantiate(&self, host: InstanceHost) -> ComponentFuture<'_, Box<dyn ComponentInstance>>;
}
```

Runtime-neutral factory for native or WebAssembly dynamic components. `descriptor` gives the host all
the metadata it needs to mount the component; `instantiate` builds (but does not activate) an
instance, given the per-instance host.

## `ComponentInstance`

```rust
pub trait ComponentInstance: Send + 'static {
    fn activate(&mut self, config: Value) -> ComponentFuture<'_, ()>;
    fn deactivate(&mut self) -> ComponentFuture<'_, ()>;
    fn call_service(&mut self, call: DynamicCall) -> ComponentFuture<'_, Vec<u8>>;
    fn call_event(&mut self, call: EventCall) -> ComponentFuture<'_, EventReply>;
}
```

Runtime-neutral lifecycle and callback interface for one component instance. All four are `&mut self`,
and `DynamicFiber` serializes them with load/unload so an instance (and, for Wasmtime, its `Store`)
is never entered concurrently.

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

A dynamic component mounted on the ordinary Supervisor lifecycle. Calls through this handle are
serialized with load and unload, so an instance is never entered concurrently.

### `await_active`

Waits for the current component revision to activate. Returns the activation failure, or
`CordisError::InactiveFiber` while required dependencies keep the fiber waiting.

### `replace`

Replaces the factory and configuration through an unload/load restart. A failed candidate remains in
the fiber's `Failed` state so callers such as the HMR transaction manager can explicitly restore the
previous factory. On restart failure, the old factory/config/revision are restored and the error
returned.

### `call_service` / `call_event`

Call a service/event export on the active instance. `CordisError::InactiveFiber` outside the active
epoch, or the component's service error.

### `retire`

Irreversibly retires the fiber and waits for instance/effect cleanup.

### The re-entrancy guard

`DynamicFiber` wraps each call in a `CallGate`. The gate is a one-slot mutex keyed by *current task +
thread*: it rejects **same-fiber re-entrancy** (`CordisError::ReentrantCall`) to avoid a Wasmtime
Store deadlock, while still allowing an instance to re-enter itself from a *different* task. This is
a restrictive-but-necessary addition recorded in `docs/wasmtime-findings.md`; the paper discusses
inertia (§4.4) but not re-entrancy.

## Mounting

`RuntimeHandle::mount_dynamic` (see [supervisor](supervisor.md)) is the entry point:

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

It creates a fiber, `extend`s the context to it, installs an executor that runs `run_dynamic_transition`
on each load/unload, and configures the fiber's dependencies from the factory descriptor's injects.
The returned handle may initially be waiting for required providers — use `await_active` when
activation is required.
