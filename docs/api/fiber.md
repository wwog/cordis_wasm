# Fiber

A **fiber** is one component instance in its lifecycle. `cordis_core::fiber` provides the pure
`FiberMachine` state machine that the [supervisor](supervisor.md) runs behind every fiber record,
plus the types that describe a fiber's lifecycle state, its desired (target) state, and the in-flight
transition. The machine is *pure* — it has no I/O and no locks — so the supervisor can drive it
serially as its single writer.

The state machine is the Rust counterpart of the paper's §4.1 state machine (Figure 1) and its nine
rules. The `DesiredState`/`DesiredEpoch`/`EpochEntry` types encode the key distinction the paper
draws between the **target** view (`target_n(γ)`) and the **committed** view (`ω_n`): `DesiredEpoch`
is the resolution the fiber *should* run against, while `CommittedView` (see
[service](service.md)) is the one it actually activated against.

## `FiberState`

```rust
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FiberState {
    Pending,    // not yet active
    Loading,    // a Load transition is in flight
    Active,     // loaded and running
    Unloading,  // an Unload transition is in flight
    Failed,     // a Load finished with an error
    Disposed,   // retired and removed
}
```

| Paper state | Rust |
|---|---|
| `Inactive` | `Pending` |
| `Reloading` | `Loading` |
| `Active` | `Active` |
| `Unloading` | `Unloading` |
| (failure extension) | `Failed`, `Disposed` |

## `DesiredState`, `DesiredEpoch`, `EpochEntry`

```rust
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum DesiredState {
    Waiting,               // no runnable epoch
    Ready(DesiredEpoch),   // a runnable target epoch
    Retired,               // retire and dispose
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DesiredEpoch {
    entries: Arc<[EpochEntry]>,
}
impl DesiredEpoch {
    pub fn from_resolution(resolution: &DependencyResolution) -> Option<Self>;
    pub fn entries(&self) -> &[EpochEntry];
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct EpochEntry {
    pub key: ProviderKey,
    pub provider: Option<FiberId>,
}
```

- A `DesiredEpoch` is an *ordered provider selection for one load attempt*.
- `DesiredEpoch::from_resolution` returns `Some` only when the resolution is **ready** — every
  `ResolvedInject` either has a provider or is `Requirement::Optional`. This is the `σ ⊨ d`
  satisfaction of Definition 21.
- `EpochEntry` records the **provider** (`Option<FiberId>`), never the value. This is the paper's
  "recording a provider rather than a value": two providers with equal values are different fibers,
  and substitution only depends on *which* fiber is selected.
- `DesiredState` is the **latest lifecycle target**; changes to it **coalesce** while a transition
  runs (see inertia below).

## `FiberTransition` and `TransitionKind`

```rust
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FiberTransition {
    pub fiber: FiberId,
    pub generation: u64,
    pub kind: TransitionKind,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum TransitionKind {
    Load { epoch: DesiredEpoch },
    Unload,
}
```

A `FiberTransition` is the *work to run outside the supervisor task*: the fiber, a strict
`generation`, and the kind. The supervisor hands it to an executor, and the executor reports
completion back with the same `generation`.

## `TransitionAdvance`

```rust
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum TransitionAdvance {
    IgnoredStale,                          // wrong generation, or no active transition
    Settled,                               // reached a stable state
    Start(FiberTransition),                // chain into the next transition
}
```

The result of applying a transition *completion* to the machine.

- `IgnoredStale` — the completion carried a generation that does not match the active transition, so
  it is dropped and the current transition is untouched.
- `Settled` — the machine reached a stable state (`Active`, `Pending`, `Disposed`, or `Failed`).
- `Start(next)` — the machine chained into a follow-up transition (e.g. a `Load` completes but the
  target changed, so it chains an `Unload`).

## `FiberMachine`

```rust
#[derive(Clone, Debug)]
pub struct FiberMachine { /* opaque */ }

impl FiberMachine {
    pub fn new(fiber: FiberId) -> Self;
    pub const fn state(&self) -> FiberState;
    pub fn desired(&self) -> &DesiredState;
    pub fn active_transition(&self) -> Option<&FiberTransition>;
    pub fn failure(&self) -> Option<&CordisError>;
    pub fn teardown_error(&self) -> Option<&CordisError>;

    pub fn set_desired(&mut self, desired: DesiredState) -> Option<FiberTransition>;
    pub fn complete(&mut self, generation: u64, result: Result<(), CordisError>) -> TransitionAdvance;
    pub fn restart(&mut self) -> Option<FiberTransition>;
    pub fn reload(&mut self) -> Option<FiberTransition>;
}
```

### `set_desired`

Updates the target and **starts work only when the machine is idle**. If a transition is in flight
(`active` is `Some`), it just records `desired` and returns `None` — the change will be picked up
when the current transition completes (coalescing). Otherwise:

- `Pending` + `Ready(epoch)` → starts a `Load { epoch }` (state → `Loading`).
- `Pending`/`Failed` + `Retired` → state → `Disposed`, returns `None`.
- `Active` + `Ready(same_epoch)` (the loaded epoch already equals it) → `None`, no work.
- `Active` + anything else → starts an `Unload`.

### `complete`

Completes one externally executed transition, matching on the `generation`. A stale generation
returns `IgnoredStale` and leaves the machine untouched. On a real completion:

- A `Load` with `Ok(())`: records the epoch; if the desired is still `Ready` with the *same* epoch,
  state → `Active` (matches Algorithm 5's `if fiber.target = target0 then ACTIVE`); otherwise it
  **chains** an `Unload` (the target changed mid-transition). A `Load` with `Err` → state → `Failed`,
  recording `failure`.
- An `Unload`: records any teardown error; then `Waiting` → `Pending`, `Ready(epoch)` → **chains** a
  fresh `Load`, or `Retired` → `Disposed`.

### `restart`

Explicitly retries a **failed** fiber against its latest desired state. Only works when
`state == Failed`; clears `failure`. `Waiting` → `Pending`; `Ready(epoch)` → starts a `Load`;
`Retired` → `Disposed`.

### `reload`

Forces an **active** fiber through `unload` then a fresh `load` of its desired epoch. Starts an
`Unload` only when there is no active transition, the state is `Active`, and the desired is `Ready`.
Unlike `restart`, this does not retry a failed fiber.

## The inertia rule (transition coalescing and chaining)

Once a transition begins it completes — this is the paper's inertia (§4.4). Two consequences:

1. **A target change during a transition coalesces.** `set_desired` while `active.is_some()` records
   the new target and returns `None`; no second transition starts. The change is observed when the
   in-flight transition completes.
2. **A target change chains rather than interrupts.** When a `Load` completes and the target is no
   longer the epoch that was loaded, `complete` returns `Start(Unload)`, which the supervisor runs
   next, and that `Unload` may in turn `Start(Load)` for the new epoch. The chain is driven one
   transition at a time; the machine never aborts a transition halfway.

The machine is proven by a generated-transition-sequences test
(`generated_transition_sequences_preserve_state_invariants`) that drives 128 random schedules × 256
steps and asserts the invariant: the machine has an active transition **iff** it is `Loading` or
`Unloading`, and once `Disposed` it stays `Disposed`.

## Provider identity, not value

`set_desired` compares epochs by *which provider fiber* each entry selects, not by any value the
provider holds. `FiberId`s are fresh and never reused, so a replaced provider is never mistaken for
its predecessor even when the services they provide compare equal (see `fiber.rs:395-397`).

## Errors

The machine itself does not return `CordisError`; it records `failure` (a failed load) and
`teardown_error` (a failed unload) inside the state and exposes them via accessors. The supervisor
turns those into the fiber snapshot's `failure`/`teardown_error` fields.

## Deeper reference

The state machine has a dedicated page — [fiber-machine](fiber-machine.md) — covering the
Fibonacci-style exhaustive invariant test, the precise coalescing protocol, and the full
load/unload completion matrix.
