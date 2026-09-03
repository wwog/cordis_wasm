# Supervisor Runtime

The supervisor is the **single-writer actor** at the heart of Cordis. The component graph, the
service/provider table, and every fiber's lifecycle state are mutated only by one task, which
processes commands in order. Component code and event callbacks run *outside* the supervisor — a
transition is handed to an executor task, which reports completion back via a command — so no user
code ever runs inside the single-writer lock, and nothing holds a synchronous lock across an `await`.

The supervisor is the Rust replacement for the JS single-threaded event loop (README: "Supervisor
actor single-writer"). It maps to the paper's §4 rules: `CreateFiber` = `O-Insert`, `RetireFiber` =
`O-Retire`, the transition machinery = `L-Begin`/`L-Iter`/`L-Finish`/`L-Leave`/`L-Divert`, the
unload block = the `L-Unload` guard, and the SCC detection = §6.5 dependency cycles. The
correspondence table is in [semantics.md](../semantics.md) §4.

## `Runtime`

```rust
#[derive(Debug)]
pub struct Runtime { /* handle: RuntimeHandle, supervisor: JoinHandle<()> */ }

impl Runtime {
    pub fn start() -> Self;
    pub fn handle(&self) -> RuntimeHandle;
    pub async fn shutdown(self) -> Result<RuntimeSnapshot, CordisError>;
}
```

Owns the single-writer supervisor task. `start` spawns it (with a command buffer of 64); `handle`
clones a handle; `shutdown` sends `Shutdown`, waits for the supervisor task to finish, and returns its
final snapshot.

**Panics** — `start` panics when called outside a Tokio runtime.

**Errors** — `shutdown` returns an error if the supervisor closed or its task failed.

## `RuntimeHandle`

```rust
#[derive(Clone)]
pub struct RuntimeHandle { /* commands: mpsc::Sender<Command>, executors, changes */ }
```

Cloneable command handle for the runtime supervisor. Every public method sends a `Command` over the
channel, awaits the reply, notifies waiters, and dispatches any transitions that became ready.

### Command methods

```rust
pub async fn create_fiber(&self, parent: Option<FiberId>) -> Result<FiberId, CordisError>;
pub(crate) async fn create_live_child_fiber(&self, parent: FiberId) -> Result<FiberId, CordisError>;

pub async fn allocate_realm(&self) -> Result<RealmId, CordisError>;

pub async fn configure_dependencies(
    &self, fiber: FiberId, context: Context, injects: Vec<InjectSpec>,
) -> Result<DependencyChange, CordisError>;

pub async fn commit_dependencies(&self, fiber: FiberId) -> Result<CommittedView, CordisError>;

pub async fn provide(&self, key: ProviderKey, provider: FiberId) -> Result<RegistryChange, CordisError>;
pub async fn withdraw(&self, key: ProviderKey, provider: FiberId) -> Result<RegistryChange, CordisError>;

pub async fn complete_transition(
    &self, fiber: FiberId, generation: u64, result: Result<(), CordisError>,
) -> Result<TransitionUpdate, CordisError>;

pub async fn retire_fiber(&self, fiber: FiberId) -> Result<Vec<FiberTransition>, CordisError>;
pub async fn restart_fiber(&self, fiber: FiberId) -> Result<Vec<FiberTransition>, CordisError>;
pub async fn reload_fiber(&self, fiber: FiberId) -> Result<Vec<FiberTransition>, CordisError>;

pub async fn snapshot(&self) -> Result<RuntimeSnapshot, CordisError>;
pub async fn await_quiescent(&self) -> Result<RuntimeSnapshot, CordisError>;
```

**Errors everywhere** — `CordisError::RuntimeClosed` after shutdown, plus the specific error each
command reports (see the `# Errors` doc on each).

- `create_fiber(parent)` — creates a fiber after validating its optional parent. `UnknownFiber` for
  an unknown parent.
- `create_live_child_fiber(parent)` — creates a child while its parent is `Loading` or `Active`. This
  is the lifecycle-safe entry point used by generated method-level injects; `InactiveFiber` if the
  parent is not live.
- `allocate_realm()` — allocates a `RealmId` that will never be reused by this process.
- `configure_dependencies(fiber, context, injects)` — declares or replaces a fiber's dependencies and
  computes its desired resolution. An explicit replacement also retries a failed fiber. `ContextFiberMismatch`,
  `UnknownFiber`, `DuplicateInject`, `MissingRealm`, or `RuntimeClosed`.
- `commit_dependencies(fiber)` — freezes the current ready resolution for one load epoch.
  `InactiveDependency` if a required provider is absent.
- `provide(key, provider)` — occupies one `(service, realm)` provider slot. `DuplicateProvider` if
  the slot is occupied. **Errors** also on an unknown provider fiber.
- `withdraw(key, provider)` — releases one slot owned by `provider`. `ProviderNotFound`,
  `ProviderOwnershipMismatch`.
- `complete_transition(fiber, generation, result)` — reports work completion (this is how the
  executor tells the supervisor a `Load`/`Unload` finished). `UnknownFiber`,
  `TransitionBlocked` (while a guard blocks the unload), or `RuntimeClosed`.
- `retire_fiber(fiber)` — marks a fiber retired and returns cleanup work when required.
- `restart_fiber(fiber)` — explicitly retries a failed fiber against its latest desired epoch.
- `reload_fiber(fiber)` — forces an active fiber through unload and a fresh load of its desired
  epoch. Unlike `restart_fiber`, it does not retry a failed fiber.
- `snapshot()` — returns a stable snapshot produced by the supervisor.
- `await_quiescent()` — waits until no fiber transition is in flight and returns a stable snapshot.
  Fibers waiting for missing dependencies are considered quiescent.

### Internal helpers

`install_executor`, `remove_executor`, `await_disposed`, and `await_settled` are `pub(crate)` and are
used by the dynamic/native paths.

## Snapshot types

```rust
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FiberSnapshot {
    pub id: FiberId,
    pub parent: Option<FiberId>,
    pub desired: DependencyResolution,
    pub committed: Option<CommittedView>,
    pub state: FiberState,
    pub active_transition: Option<FiberTransition>,
    pub dependency_error: Option<CordisError>,
    pub failure: Option<CordisError>,
    pub teardown_error: Option<CordisError>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RuntimeSnapshot {
    pub fibers: Vec<FiberSnapshot>,
    pub allocated_realms: u64,
    pub provider_count: usize,
}
```

`FiberSnapshot` is the externally observable view of one fiber: its desired (target) resolution, its
committed (ω_n) view, its state, any in-flight transition, and the three error slots. `RuntimeSnapshot`
is the whole graph plus accounting.

## `DependencyChange`, `RegistryChange`, `TransitionUpdate`

```rust
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DependencyChange {
    pub resolution: DependencyResolution,
    pub transitions: Vec<FiberTransition>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RegistryChange {
    pub key: ProviderKey,
    pub affected: Vec<FiberId>,
    pub transitions: Vec<FiberTransition>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TransitionUpdate {
    pub status: CompletionStatus,
    pub ready: Vec<FiberTransition>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CompletionStatus {
    Applied,
    IgnoredStale,
}
```

- `DependencyChange` is `configure_dependencies`'s result: the new resolution plus any transitions it
  made runnable (e.g. a load that can now start).
- `RegistryChange` is a provide/withdraw result, already narrowed to affected consumers: the changed
  key, the fibers whose desired resolution changed, and the transitions to dispatch.
- `TransitionUpdate` is `complete_transition`'s result: whether the completion was applied or ignored
  as stale, plus the transitions that became ready.

## Lifecycle of a fiber

1. `create_fiber` inserts a `Pending` fiber.
2. `configure_dependencies` gives it a context + injects; the supervisor `resolve_dependencies`
   computes the `(service, realm)` keys against the providers table. If ready, the fiber becomes
   `Ready(epoch)` and a `Load` starts.
3. `commit_dependencies` freezes the ready resolution into the `CommittedView`.
4. The executor runs the `Load` (activate the component); `complete_transition` reports back.
   `complete_load` sets `Active` (or chains `Unload` if the target changed mid-flight).
5. `provide`/`withdraw` recompute affected consumers; a provider appearing wakes consumers, a provider
   leaving deactivates them.
6. `retire_fiber` sets the desired to `Retired` and eventually `Disposed`.

## The unload guard (spatial composability)

The paper's `L-Unload` rule requires `¬relied_n(γ)`: a provider may unload only after its dependents
have gone (Theorem 70). The supervisor implements this as the **blocked unloads** map.

- `schedule_transition_batch` puts every `Unload` on `state.blocked_unloads` and does not dispatch it
  yet.
- `release_ready_unloads` releases a blocked unload only when `!has_active_consumers`.
- `has_active_consumers(provider)` is `relied_n`: it tests whether any `Loading`/`Active`/`Unloading`
  fiber has a **committed** view naming the provider.

So an unload waits for its consumers to fully unload first — consumer-first teardown. The test
`teardown_drains_consumers_before_providers` retires a provider on a `provider -> middle -> leaf`
chain and asserts `leaf`, then `middle`, then `provider` are torn down.

## Dependency-cycle detection

`dependency_cycles` runs an SCC over the provider–consumer graph and marks each cycle member `Waiting`
with a `CordisError::DependencyCycle { fibers }`. A component in a cycle never activates; this
"permanently inactive" outcome is predictable from the dependency declarations alone (§6.5). A
self-loop is detected too (`graph[&fiber].contains(&fiber)`). Test:
`dependency_cycle_reports_every_scc_member`.

## Errors

- `RuntimeClosed` — any command after shutdown.
- `SupervisorFailed { message }` — the supervisor task panicked.
- `TransitionBlocked { fiber }` — completing a transition on a fiber whose unload is blocked.
- Plus the specific errors listed per command above.
