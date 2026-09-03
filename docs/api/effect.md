# Effects

The effect subsystem is the kernel of Cordis's revertible-effect model. The paper (§3.1) writes an
effect as `𝔈Γ ≔ Γ → Γ × (Γ → Γ)`: running an effect produces a new state **and** an inverse that
returns to the old state. In Rust, the inverse is a [`Disposer`](#disposer), and an
[`EffectScope`](#effectscope) accumulates inverses so the fiber can `recoverΓ` — apply them LIFO —
when it unloads.

The whole model is "revertible effect, disposer as inverse": a component performs effects by
returning disposers; the runtime owns those disposers and is responsible for running them in reverse
registration order when the owning fiber unloads. The disposer is *not* verified to actually revert
the effect — the paper is explicit (§5.1.1) that "the inverse reverts the effect it accompanies is an
obligation on the component author rather than a property the runtime verifies," and this
implementation agrees by design.

## `Disposer`

```rust
pub struct Disposer { /* opaque */ }
```

An asynchronous inverse operation captured by an effect. Constructed from a closure that returns a
future.

```rust
impl Disposer {
    pub fn new<F, Fut>(callback: F) -> Self
    where
        F: FnOnce() -> Fut + Send + 'static,
        Fut: Future<Output = Result<(), CordisError>> + Send + 'static;

    pub fn infallible<F, Fut>(callback: F) -> Self
    where
        F: FnOnce() -> Fut + Send + 'static,
        Fut: Future<Output = ()> + Send + 'static;
}
```

- `new` wraps a fallible inverse; its future returns `Result<(), CordisError>`.
- `infallible` wraps an inverse whose future returns `()` and treats any error as `Ok`. Use it when
  the inverse cannot fail, or when you have nothing meaningful to report.

A `Disposer` runs exactly once — it is moved into the disposal loop and consumed.

## `EffectGuard`

```rust
#[derive(Clone)]
pub struct EffectGuard { /* opaque */ }
```

The *handle* to one effect: you clone it, inspect its state, and eventually dispose it. Created as a
pair with an `EffectScope`:

```rust
impl EffectGuard {
    pub fn new(label: impl Into<Arc<str>>) -> (Self, EffectScope);

    pub fn spawn_stream<S>(label: impl Into<Arc<str>>, stream: S) -> (Self, EffectScope)
    where
        S: Stream<Item = Result<Disposer, CordisError>> + Send + Unpin + 'static;

    pub fn id(&self) -> EffectId;
    pub fn is_armed(&self) -> bool;
    pub fn is_disposed(&self) -> bool;
    pub fn metadata(&self) -> EffectMeta;

    pub async fn dispose(&self) -> DisposeReport;
}
```

### `new`

Creates an armed effect (state `Armed`, no disposers) and its `EffectScope`.

### `spawn_stream`

Starts an asynchronous **effect iterator** immediately. Each successful stream item is a
`Disposer` — the inverse for the step that just completed — and is registered as it is produced.
Disposal stops the iterator only at an item boundary, waits for the in-flight item, then runs every
collected inverse. A stream error, or a panic in the stream, triggers disposal.

**Panics** — panics when called outside a Tokio runtime.

### `dispose`

Runs every inverse **in reverse registration order** (LIFO), then marks the effect `Disposed`.
Concurrent and later calls wait for, then return, the *same* report — disposal is idempotent and
exactly-once.

**Errors** — returns `Err(DisposeErrors)` with all disposer failures and panics, after every disposer
has been attempted. A failing disposer never skips the remaining ones.

### The exactly-once state machine

Each `EffectInner` carries a `Mutex<EffectState>`:

```rust
enum EffectStatus {
    Armed,        // accepting disposers
    Draining,     // a stream runner is in flight; stop accepting, wait for it
    Disposing,    // running disposers now
    Disposed(DisposeReport),
}
```

- Registering a disposer (via `EffectScope::defer`) requires `Armed`; otherwise it returns
  `CordisError::InactiveEffect`.
- `dispose()` on an `Armed` effect with no active stream runner transitions straight to `Disposing`
  and runs the disposers. With a stream runner active, it first drains the runner.
- The transition is atomic under the mutex, so a fast disposer cannot race the waiter registration.

This is the paper's "firing twice would apply an inverse at a state no application produced"
guarantee (§5.1.1): the disposer set is consumed once, and a second `dispose()` returns the stored
report without re-running anything (test: `dispose_is_idempotent`).

## `EffectScope`

```rust
#[derive(Clone, Debug)]
pub struct EffectScope { /* opaque */ }
```

Dropped in favor of clones; it is the *registration endpoint* inside an effect. You get one from
`EffectGuard::new` / `spawn_stream` and hand it to the effect body so it can register inverses.

```rust
impl EffectScope {
    pub fn id(&self) -> EffectId;

    pub fn defer(&self, disposer: Disposer) -> Result<(), CordisError>;

    pub fn child(&self, label: impl Into<Arc<str>>) -> Result<(EffectGuard, EffectScope), CordisError>;
}
```

### `defer`

Adds one inverse to this effect.

**Errors** — returns `CordisError::InactiveEffect { effect }` after disposal has started.

### `child`

Creates a nested effect that is part of this effect's inverse **and** metadata tree. The child is
registered as a disposer of the parent, so disposing the parent disposes the child; a child failure
is reported as `CordisError::ChildEffectFailed { effect, errors }`.

**Errors** — returns `CordisError::InactiveEffect { effect }` after disposal has started.

## `EffectSet`

```rust
#[derive(Clone, Debug)]
pub struct EffectSet { /* opaque */ }
```

Owns the top-level effects of one **fiber**. A fiber creates one `EffectSet` and registers all of its
component/member effects under it.

```rust
impl EffectSet {
    pub fn new(label: impl Into<Arc<str>>) -> Self;
    pub fn id(&self) -> EffectId;
    pub fn effect(&self, label: impl Into<Arc<str>>) -> Result<(EffectGuard, EffectScope), CordisError>;
    pub fn metadata(&self) -> Vec<EffectMeta>;
    pub async fn dispose(&self) -> DisposeReport;
}
```

- `effect` creates a top-level effect owned by this set (delegates to `scope.child`).
- `metadata` returns one `EffectMeta` per live effect.
- `dispose` disposes every owned effect in reverse creation order; concurrent and later calls return
  the same report.

**Errors** — `effect` returns `InactiveEffect` after set disposal has started; `dispose` returns the
aggregated child-effect failures (as `DisposeErrors`) after all effects were attempted.

## `DisposeReport` and `DisposeErrors`

```rust
pub type DisposeReport = Result<(), DisposeErrors>;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DisposeErrors { /* opaque */ }
impl DisposeErrors {
    pub fn errors(&self) -> &[CordisError];
}
```

The report of one disposal run. `Ok(())` means every inverse ran without error. `Err(DisposeErrors)`
collects everything that went wrong — including panics converted to `CordisError::DisposerPanic` —
but only after **every** disposer had a chance to run. `DisposeErrors` implements `Display` ("N
disposer(s) failed") and `std::error::Error`.

## `EffectMeta`

```rust
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct EffectMeta {
    pub id: EffectId,
    pub label: Arc<str>,
    pub children: Vec<EffectMeta>,
}
```

A tree node used to expose nested effect labels for diagnostics, e.g. `metadata()` on a fiber's
`EffectSet`. Each node is its effect id, its label, and its (still-live) children. A label is
human-readable, e.g. `"method:start"`, `"dynamic:example.counter"`, or `"timer:interval"`.

## LIFO recovery and the paper correspondence

- `run_disposers` iterates `disposers.into_iter().rev()` — the inverse is applied in reverse
  registration order (paper Theorem 16; test `disposers_run_in_lifo_order`).
- Recovery **continues past a failing inverse** — a disposer that returns `Err` or panics is
  recorded and the loop keeps going (test `failures_do_not_skip_remaining_disposers`).
- `EffectScope::defer` pushing onto a `Vec` is `trackΓ`; `run_disposers` is `recoverΓ`.
- `spawn_stream` is the paper's effect iterator `ℑΓ` (Definition 17/18): each stream item is an
  inverse.

## Errors

- `InactiveEffect { effect }` — `defer`/`child`/`effect` after disposal started.
- `DisposeErrors` — one or more inverses failed or panicked during `dispose`.
- `ChildEffectFailed { effect, errors }` — a child effect failed during its parent's disposal.
- `DisposerPanic { message }` / `DisposerFailed { message }` — recorded inside `DisposeErrors`.
