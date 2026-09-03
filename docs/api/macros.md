# Macros

Cordis ships six procedural macros. They turn a plain trait or struct into a runtime-service
specification, an event specification, or a native component whose dependency graph, config schema,
and effect lifecycle the runtime understands. All six live in `cordis-macros` and are re-exported
through the `cordis` facade, so you write `#[cordis::service]` etc. and never name the macro crate.

The compile-time identity they generate is the heart of the model: every service and event gets a
**name** plus a 32-byte ABI hash computed by BLAKE3 over a canonical signature. Two declarations
with the same name but different method signatures get different hashes and therefore do not
satisfy each other.

## `#[cordis::service(name = "...")]`

Applied to a `trait`. Declares one *service* — a named, remote-callable RPC surface.

- Arguments: `name` (a string). Optional; defaults to the trait identifier.
- The trait body may contain only `async fn` methods. Each method must take `&self`, take owned
  arguments, and return `Result<T, E>`. No defaults, no generics, no `where` clauses. Plain
  identifiers for arguments (no `ref`, no `mut`, no destructuring). Arguments must be owned because
  native and Wasm dispatch use the same wire ABI.

```rust
use cordis::ServiceCallError;

#[cordis::service(name = "example.counter")]
pub trait Counter {
    async fn add(&self, amount: i64) -> Result<i64, String>;
}
```

The macro rewrites each method's signature to

```rust
fn add(&self, amount: i64) -> impl std::future::Future<Output = Result<i64, String>> + Send;
```

so the trait becomes object-safe and can be implemented by a concrete provider.

### Generated companion types

For `trait Counter`, the macro emits (all with the trait's visibility):

| Item | Purpose |
|---|---|
| `struct CounterService;` | The zero-sized **marker**. Implements `ServiceKey` (`NAME`, `ABI_HASH`) and `ServiceSpec`. |
| `struct CounterClient;` | The typed **client**. Sends calls to a provider through a `ServiceDispatcher`. |
| `struct CounterDispatcher<T>;` | The **object-safe dispatcher** wrapping an `Arc<T>` provider implementation. Implements `ServiceDispatcher`. |

`CounterClient` has three public constructors and one accessor:

```rust
impl CounterClient {
    // Checks the dispatcher's ServiceId (name + hash) before use.
    pub fn new(dispatcher: Arc<dyn ServiceDispatcher>) -> Result<Self, CordisError>;

    // Zero-serialization direct path, used by the native_counter example.
    pub fn from_native<T: Counter + Send + Sync + 'static>(service: Arc<T>) -> Self;

    pub fn service_id(&self) -> &ServiceId;
}
```

`CounterClient::new` first calls `ServiceClient::new::<CounterService>(dispatcher)`, which returns
`CordisError::ServiceIdentityMismatch` when the dispatcher's name or hash does not match.

Like the spec, the client methods are `async` and return `Result<T, ServiceCallError<E>>`. The
dynamic branch encodes arguments with `encode_service_payload`, calls `counter.call(method_id, payload)`,
then decodes the response `Result<T, E>`; transport failures surface as
`ServiceCallError::Transport`, provider failures as `ServiceCallError::Service`.

`CounterDispatcher::new(service)` wraps an `Arc<T>`; `dispatch` matches on the method id and for an
unknown id returns `CordisError::UnknownServiceMethod`.

## `#[cordis::event(name = "...", mode = "...")]`

Applied to a `trait`. Declares one *event* — a dispatch surface with a fixed semantics. The trait
may declare only two associated types, `type Input` and `type Output`, with values.

- Arguments: `name` (string, defaults to the trait identifier) and `mode` (one of `emit`,
  `parallel`, `serial`, `bail`, `waterfall`; defaults to `parallel`).
- For `mode = "waterfall"`, `Input` and `Output` must be identical types.

```rust
#[cordis::event(name = "app.counter", mode = "serial")]
pub trait CounterChanged {
    type Input = u64;
    type Output = ();
}
```

### Generated companion types

The macro emits a marker `CounterChangedEvent` (with `NAME`, `ABI_HASH`, `MODE`, `runtime()`, plus
`encode_input`/`decode_input`/`encode_output`/`decode_output`), implements `EventSpec` for it, and
emits a type alias `CounterChangedRuntime` holding the runtime type for the mode:

| Mode | Runtime type |
|---|---|
| `emit` / `parallel` / `serial` | `AsyncEvent<Input, Output>` |
| `bail` | `BailEvent<Input, Output>` |
| `waterfall` | `WaterfallEvent<Input>` |

The marker also gets a `dispatch` associated function. Its signature depends on the mode (see
[event](event.md) for the semantics of each):

```rust
// emit: fire-and-forget, errors go to an error sink
fn dispatch<S>(event, target, input, error_sink) -> Result<(), CordisError> where S: Fn(CordisError) + Send + Sync + 'static;

// parallel
async fn dispatch(event, target, input) -> Result<Vec<ControlFlow<Output>>, CordisError>;

// serial / bail
// bail is sync: fn ... -> Result<Option<Output>, CordisError>
async fn dispatch(event, target, input) -> Result<Option<Output>, CordisError>;

// waterfall
async fn dispatch(event, target, input) -> Result<Output, CordisError>;
```

## `#[cordis::component(name = "...", config = "...")]`

Applied to a `struct`. Declares a *native component*: its config type, its injected dependencies,
and its descriptor. Combines with `#[cordis::inject(...)]` on the same struct.

- Arguments: `name` (string, defaults to the struct identifier) and `config` (a type, defaults to
  `()`). The config type must implement `DeserializeOwned + JsonSchema + Send + Sync + 'static`.
- `#[cordis::inject(ServiceA, ServiceB, ...)]` lists the services the component depends on. Each is
  a service **trait** path; the macro derives the marker (`ServiceAService`) from it.

```rust
#[derive(Debug, Deserialize, JsonSchema)]
pub struct CounterConfig { amount: i64 }

#[derive(Debug)]
#[cordis::component(name = "counter-consumer", config = CounterConfig)]
#[cordis::inject(Counter)]
pub struct CounterConsumer;
```

For `struct CounterConsumer`, the macro emits:

- `struct CounterConsumerDependencies` with one field per inject, named by the snake-case of the
  service trait (`counter: CounterClient`), plus a `new(...)` constructor. It implements
  `DependencySet`, whose `injects()` returns the `InjectSpec`s and whose `resolve(resolver)` builds
  clients from a committed view.
- `impl ComponentDefinition for CounterConsumer` with `type Config = CounterConfig`,
  `type Deps = CounterConsumerDependencies`, and a `descriptor()` returning the
  `ComponentDescriptor { name, injects, config_schema }` cached in a `OnceLock`.

## `#[cordis::component_impl]`

Applied to an `impl` block targeting the component struct. Turns it into a `Component`. It requires:

- Exactly one method marked `#[cordis::apply]`, with signature
  `async fn name(&mut self, context: ComponentContext<XxxDependencies>, config: XxxConfig) -> Result<(), CordisError>`.
  This is the component's *effect body*: register listeners/providers on the context and return;
  failure tears the effects down and fails the fiber.
- Zero or more `#[cordis::inject(...)]` methods of the form
  `async fn name(&mut self, method_context: MethodContext<...>) -> Result<(), CordisError>`.
  Each becomes a separate *method-level child fiber* owned by the component's effect tree (see
  [native-component](native-component.md#method-context-and-method-level-inject)).

```rust
#[cordis::component_impl]
impl CounterConsumer {
    #[cordis::apply]
    async fn start(
        &mut self,
        context: ComponentContext<CounterConsumerDependencies>,
        config: CounterConfig,
    ) -> Result<(), cordis::CordisError> {
        let value = context.deps().counter.add(config.amount).await
            .map_err(service_error)?;
        println!("counter value: {value}");
        Ok(())
    }
}
```

The generated `Component::apply` runs the method-level registrations, then the `apply` method,
inside `catch_component_future`. On error it disposes the effect set and returns the error. On
success it returns `ComponentEffects`, retaining the effect set so the supervisor can dispose it
when the fiber unloads.

## `#[cordis::inject(...)]`

The inject attribute is meaningful in two places and is processed by two different macros:

- On a `#[cordis::component]` struct: declares the component's required services.
- On a method inside `#[cordis::component_impl]`: declares that method's own dependencies and makes
  it a child fiber with access to them via a `MethodContext`.

The form is `#[cordis::inject(Service, OtherService)]`, a comma-separated list of trait paths, each
of which must be a `ServiceSpec`.

## `#[cordis::apply]`

A marker, not a transform. It tags the one `async fn` in a `#[cordis::component_impl]` block that is
the component's effect body. The expanded block's `Component::apply` invokes it, and exactly one is
required.

## ABI-hash identity

`#[cordis::service]` computes its hash from the service `name` plus each method's canonical
signature. The canonical form of a method is:

```text
method_name(arg_type1,arg_type2,...)->Result<OkType,ErrType>
```

The service hash is BLAKE3 over `name` followed by the canonical forms **sorted unconditionally**
(so method declaration order does not affect the wire protocol). Each method also gets a `method_id`
computed as the first four bytes (little-endian) of BLAKE3 over `name\ncanonical_method`.

`#[cordis::event]` computes its hash over

```text
name\nmode\n<Input type>\n<Output type>
```

so the mode participates in an event's identity, and only the types (not the parameter names) matter.

This is the paper's key-namespacing route (§6.6): a service called `example.counter` from one module
never satisfies a consumer expecting a different `example.counter` ABI, even if the names match,
because the hashes differ.

## Complete runnable example

The full native path, copied from `crates/cordis/examples/native_counter.rs`:

```rust
use cordis::{Component, ComponentContext, Context, EffectSet, Runtime, ServiceCallError};
use schemars::JsonSchema;
use serde::Deserialize;
use std::sync::Arc;
use std::sync::atomic::{AtomicI64, Ordering};

#[cordis::service(name = "example.counter")]
pub trait Counter {
    async fn add(&self, amount: i64) -> Result<i64, String>;
}

#[derive(Debug, Default)]
struct AtomicCounter { value: AtomicI64 }

impl Counter for AtomicCounter {
    fn add(&self, amount: i64) -> impl Future<Output = Result<i64, String>> + Send {
        let result = self.value.fetch_update(Ordering::Relaxed, Ordering::Relaxed, |value| {
            value.checked_add(amount)
        }).map(|previous| previous + amount).map_err(|_| "counter overflow".to_owned());
        std::future::ready(result)
    }
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct CounterConfig { amount: i64 }

#[derive(Debug)]
#[cordis::component(name = "counter-consumer", config = CounterConfig)]
#[cordis::inject(Counter)]
pub struct CounterConsumer;

#[cordis::component_impl]
impl CounterConsumer {
    #[cordis::apply]
    async fn start(
        &mut self,
        context: ComponentContext<CounterConsumerDependencies>,
        config: CounterConfig,
    ) -> Result<(), cordis::CordisError> {
        let value = context.deps().counter.add(config.amount).await.map_err(service_error)?;
        println!("counter value: {value}");
        Ok(())
    }
}

fn service_error(error: ServiceCallError<String>) -> cordis::CordisError {
    match error {
        ServiceCallError::Transport(error) => error,
        ServiceCallError::Service(message) => cordis::CordisError::SupervisorFailed { message },
    }
}

#[tokio::main]
async fn main() -> Result<(), cordis::CordisError> {
    let runtime = Runtime::start();
    let fiber = runtime.handle().create_fiber(None).await?;
    let counter = Arc::new(AtomicCounter::default());
    let client = CounterClient::from_native(counter);
    let context = ComponentContext::new(
        Context::root(fiber),
        CounterConsumerDependencies::new(client),
        EffectSet::new("counter-consumer"),
    );
    let effects = CounterConsumer.apply(context, CounterConfig { amount: 3 }).await?;
    effects.effect_set().dispose().await.map_err(|error| cordis::CordisError::DisposerFailed {
        message: error.to_string(),
    })?;
    runtime.shutdown().await?;
    Ok(())
}
```

Run it with `cargo run -p cordis --example native_counter`; it prints `counter value: 3`.

## Compile-time errors

The macro reports errors at `cargo build` time for contract violations. The meaningful ones:

- A service/event trait with generic parameters.
- A service method that is not `async`, takes `&mut self`, has a default body, is generic, has a
  `where` clause, uses a reference argument, or does not return `Result<T, E>`.
- An event trait declaring anything other than `type Input`/`type Output`, or a `waterfall` event
  whose `Input` and `Output` differ.
- `#[cordis::component_impl]` on a foreign trait impl, more than one `#[cordis::apply]` method, an
  `apply` method with the wrong arity/type, or `apply` combined with `inject`.
- A method-level inject with the wrong arity or missing `&mut self`.
- Duplicate service method ids, or injected services without distinct client field names.
