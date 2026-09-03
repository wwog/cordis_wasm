# Events

The event subsystem is Cordis's dispatch surface. An **event** is a named, typed declaration (trait)
with a fixed **dispatch mode**. Listeners register against an `EffectScope` — so an event listener is
always effect-owned and torn down when the owning fiber unloads. Native and WebAssembly components
share the same `EventId`, the same `EventMode`, and the same MessagePack payload codec.

There are five dispatch modes, each a distinct runtime type with distinct semantics. The macro
`#[cordis::event]` picks the runtime type from the mode and generates a `dispatch` function (see
[macros](macros.md)).

## `EventId`

```rust
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct EventId { /* name: Arc<str>, abi_hash: [u8; 32] */ }

impl EventId {
    pub fn new(name: impl Into<Arc<str>>, abi_hash: [u8; 32]) -> Self;
    pub fn name(&self) -> &str;
    pub const fn abi_hash(&self) -> &[u8; 32];
}
```

Stable event identity shared by native and WebAssembly components. `name` + 32-byte ABI hash,
exactly like `ServiceId`. `Display` is `{name}@{first-4-bytes}`.

## `EventSpec`

```rust
pub trait EventSpec: Send + Sync + 'static {
    type Input: Clone + Serialize + DeserializeOwned + Send + 'static;
    type Output: Serialize + DeserializeOwned + Send + 'static;

    const NAME: &'static str;
    const ABI_HASH: [u8; 32];
    const MODE: EventMode;

    fn event_id() -> EventId { EventId::new(Self::NAME, Self::ABI_HASH) }
}
```

Static identity and payload types generated for an event declaration. The macro's marker type
implements it.

## `EventMode`

```rust
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum EventMode {
    Emit,
    Parallel,
    Serial,
    Bail,
    Waterfall,
}
```

Dispatch semantics selected by an event declaration. The five variants map to the five runtime types
and to the TS `DispatchMode` ('emit' | 'parallel' | 'serial' | 'bail' | 'waterfall').

## `EventTarget`

```rust
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum EventTarget {
    Global,
    Realm(RealmId),
}
```

The realm used to select listeners for one dispatch. A global listener matches any dispatch target; a
realm listener matches only a dispatch to the same `RealmId`.

## `ListenerOptions`

```rust
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ListenerOptions {
    pub target: EventTarget,
    pub prepend: bool,
}
impl ListenerOptions {
    pub const fn global() -> Self;
    pub const fn realm(realm: RealmId) -> Self;
    #[must_use] pub const fn prepend(mut self) -> Self;
}
impl Default for ListenerOptions { /* global, no prepend */ }
```

Registration-time listener selection and ordering. `prepend` puts the listener before existing
listeners registered for the same target.

## The five dispatch modes

| Mode | Runtime | Semantics |
|---|---|---|
| `Emit` | `AsyncEvent` | Fire-and-forget: start matching listeners in order, do not await. Errors/panics go to an error sink. |
| `Parallel` | `AsyncEvent` | Run all matching listeners concurrently; await them all; preserve registration order in the result. Aggregates errors. |
| `Serial` | `AsyncEvent` | Run listeners in order, stop at the first `Break`; return that value. Returns the first error. |
| `Bail` | `BailEvent` | Synchronous version of serial: call listeners in order, stop at the first `Break`. |
| `Waterfall` | `WaterfallEvent` | Onion middleware: each listener wraps the rest of the chain around a final `next`. |

## `AsyncEvent<P, B>`

```rust
#[derive(Clone)]
pub struct AsyncEvent<P: 'static, B: 'static> { /* opaque */ }
impl<P, B> AsyncEvent<P, B> {
    pub fn new() -> Self;

    pub fn listen<F, Fut>(
        &self, effect: &EffectScope, options: ListenerOptions, callback: F,
    ) -> Result<ListenerId, CordisError>
    where
        F: Fn(P) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Result<ControlFlow<B>, CordisError>> + Send + 'static;
}
impl<P, B> AsyncEvent<P, B> where P: Clone + Send + 'static, B: Send + 'static {
    pub async fn parallel(&self, target: EventTarget, payload: &P) -> Result<Vec<ControlFlow<B>>, CordisError>;
    pub async fn serial(&self, target: EventTarget, payload: &P) -> Result<Option<B>, CordisError>;
    pub fn emit_nowait<S>(&self, target: EventTarget, payload: &P, error_sink: S) -> Result<(), CordisError>
    where S: Fn(CordisError) + Send + Sync + 'static;
}
```

- `listen` registers a listener **owned by `effect`** — the handler's inverse is deferred into the
  scope, so an effect-owned listener is removed when the fiber unloads. Returns a `ListenerId`.
- `parallel` runs all matching listeners concurrently via `join_all`; registration order is preserved
  in the output. Any error/panic is collected; if there are any, returns
  `CordisError::EventListenersFailed { errors }`.
- `serial` runs in order and stops at the first `ControlFlow::Break`, returning its value.
- `emit_nowait` starts matching listeners in order without waiting; asynchronous failures and panics
  are delivered to `error_sink`. Returns immediately if invoking a listener panics before producing
  its future — listeners already started keep running.

**Errors** — `listen`: `InactiveEffect` if disposal has started. `parallel`:
`EventListenersFailed`. `serial` and `emit_nowait`: the first listener error or panic (for
`emit_nowait`, only the synchronous ones; async ones go to the sink).

**Panics** — `emit_nowait` panics when called outside a Tokio runtime.

## `BailEvent<P, B>`

```rust
#[derive(Clone)]
pub struct BailEvent<P: 'static, B: 'static> { /* opaque */ }
impl<P, B> BailEvent<P, B> {
    pub fn new() -> Self;
    pub fn listen<F>(&self, effect: &EffectScope, options: ListenerOptions, callback: F)
        -> Result<ListenerId, CordisError>
    where F: Fn(P) -> Result<ControlFlow<B>, CordisError> + Send + Sync + 'static;
}
impl<P, B: 'static> BailEvent<P, B> where P: Clone + 'static {
    pub fn bail(&self, target: EventTarget, payload: &P) -> Result<Option<B>, CordisError>;
}
```

Effect-owned synchronous event for deterministic bail dispatch. `bail` runs listeners in order and
stops at the first `Break`; its listener callback is **synchronous** (returns `Result<
ControlFlow<B>, CordisError>`, not a future). This is what makes `bail` deterministic and
non-async.

**Errors** — `listen`: `InactiveEffect`. `bail`: the first listener error or panic
(`CordisError::EventListenerPanicked`).

## `WaterfallEvent<T>`

```rust
#[derive(Clone)]
pub struct WaterfallEvent<T: Send + 'static> { /* opaque */ }
impl<T: Send + 'static> WaterfallEvent<T> {
    pub fn new() -> Self;
    pub fn listen<F, Fut>(&self, effect: &EffectScope, options: ListenerOptions, callback: F)
        -> Result<ListenerId, CordisError>
    where
        F: Fn(T, Next<T>) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Result<T, CordisError>> + Send + 'static;
    pub async fn run(&self, target: EventTarget, value: T) -> Result<T, CordisError>;
}
```

Effect-owned onion middleware. Each listener receives the value and a `Next` continuation; calling
`next.call(value)` invokes the rest of the chain, and the listener may transform the value before and
after. `run` folds the matching listeners into a chain and runs it.

**Errors** — `listen`: `InactiveEffect`. `run`: propagates listener errors and converts panics to
`CordisError::EventListenerPanicked`.

## `Next<T>`

```rust
pub struct Next<T: Send + 'static> { /* step: Option<WaterfallStep<T>> */ }
impl<T: Send + 'static> Next<T> {
    pub async fn call(&mut self, value: T) -> Result<T, CordisError>;
}
```

One-shot continuation passed to a waterfall listener. `call` invokes the rest of the waterfall
**exactly once**: a second call on the same `Next` returns `CordisError::NextAlreadyUsed`, because
wrapping the chain around itself would recurse.

**Errors** — `CordisError::NextAlreadyUsed` on a second call, or the downstream listener's error.

## `ListenerId`

```rust
pub struct ListenerId(u64);   // from cordis_core::id
```

The stable identity of one registration. Exposed to let callers reference (and remove) a listener.

## `ControlFlow` semantics

Every non-waterfall listener returns (or wraps a future that returns) `std::ops::ControlFlow<B>`:

- `ControlFlow::Continue(())` — the listener ran and the dispatch should keep going.
- `ControlFlow::Break(value)` — the listener is bailing; the dispatch stops and returns
  `Some(value)` (serial/bail) — `parallel` keeps all flows and returns them as a `Vec`.

`B` is the event's `Output`. A `ControlFlow` lets a listener either participate normally or
short-circuit with a value, without conflating "ran fine" with "ran and want to stop."

## Event payload codec

```rust
pub fn encode_event_payload<T: Serialize>(value: &T) -> Result<Vec<u8>, CordisError>;
pub fn decode_event_payload<T: DeserializeOwned>(payload: &[u8]) -> Result<T, CordisError>;
```

Encodes / decodes one event payload using the canonical `MessagePack` codec.

**Errors** — `encode`: `CordisError::EventEncodeFailed { message }`; `decode`:
`CordisError::EventDecodeFailed { message }`.

## Example

```rust
let event = AsyncEvent::<u64, ()>::new();
let (owner, scope) = EffectGuard::new("listener");
event.listen(&scope, ListenerOptions::realm(realm), move |value| async move {
    println!("got {value}");
    Ok(ControlFlow::Continue(()))
})?;

// serial: stop at the first Break
let result = event.serial(EventTarget::Realm(realm), &42).await?;
```

A waterfall example from the source test:

```rust
let event = WaterfallEvent::<i32>::new();
event.listen(&scope, ListenerOptions::global(), move |value, mut next| async move {
    let value = next.call(value + 1).await?;
    Ok(value + 1)
})?;
assert_eq!(event.run(EventTarget::Global, 0).await.unwrap(), 22);
```

## Errors

- `InactiveEffect` — `listen` after disposal started.
- `EventListenersFailed { errors }` — `parallel` with any listener error/panic.
- `EventListenerPanicked { message }` — a listener panicked.
- `NextAlreadyUsed` — a waterfall `next` called twice.
