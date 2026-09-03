# 4. Events

Services are direct calls: a consumer names a provider and asks it for something. **Events** are the
announcement half — a way to say "something happened" without knowing which plugins, if any, are
listening. The counter example only *provides* a service; this chapter builds the event half of the
kernel so that chapter 7 can turn it into a real listener that surfaces data.

## The WIT event surface

A guest communicates events through three host imports, defined in `crates/cordis-guest/wit/kernel.wit`:

```wit
record event-id {
    name: string,
    abi-hash: list<u8>,
}

enum event-mode {
    emit,
    parallel,
    serial,
    bail,
    waterfall,
}

variant event-reply {
    continue-value(list<u8>),
    break-value(list<u8>),
}

register-listener: func(
    context: call-context,
    event: event-id,
    listener-id: u64,
    mode: event-mode,
) -> result<registration, kernel-error>;

dispatch-event: func(
    context: call-context,
    event: event-id,
    listener-id: u64,
    mode: event-mode,
    payload: list<u8>,
    next-token: option<u64>,
) -> result<event-reply, kernel-error>;
```

`event-id` is structurally identical to `service-id` — a name plus a 32-byte ABI hash. The hash
serves the same purpose as it does for services: it pins the event's *payload contract*, so a
listener and an emitter agree on the wire format even across independently compiled components.

The guest also implements the event half of the `plugin` export:

```wit
handle-event: func(
    context: call-context,
    event: event-id,
    listener-id: u64,
    mode: event-mode,
    payload: list<u8>,
    next-token: option<u64>,
) -> result<event-reply, kernel-error>;
```

## The five `EventMode`s

The WIT and the native core agree on five modes. The native macro maps each name to a core runtime
type in `crates/cordis-macros/src/lib.rs`:

| WIT mode | Native core type | Dispatch semantics |
|---|---|---|
| `emit` | `AsyncEvent` (fire-and-forget) | Broadcast to every matching listener, no await, no collected result. |
| `parallel` | `AsyncEvent` | All matching listeners run concurrently; all results awaited and collected. |
| `serial` | `AsyncEvent` | Listeners run in order; the first `Break` wins and stops the rest. |
| `bail` | `BailEvent` | Synchronous version of serial. |
| `waterfall` | `WaterfallEvent` | Around-middleware: each listener gets a `next()` continuation. |

The native event types (`AsyncEvent`, `BailEvent`, `WaterfallEvent`, in `crates/cordis-core/src/event.rs`)
implement the *runtime* semantics: `AsyncEvent::parallel` joins all listeners concurrently,
`AsyncEvent::serial` stops at the first `Break`, `BailEvent` is the sync equivalent, and
`WaterfallEvent` threads a one-shot `Next` continuation through the chain. Each returns a
`ControlFlow<B>` where `Continue` means "keep going" and `Break` means "stop, this is the answer."

### What the guest currently sees

Now the honest part. The guest-facing dispatch path in the **host kernel**
(`RuntimeKernel::dispatch_event`, `crates/cordis-wasm/src/loader.rs`) does **not** yet fan out an
event across multiple listeners, and it does **not** orchestrate waterfall `next()` chains. It
routes a single call:

```rust
fn dispatch_event(&self, _: FiberId, call: EventCall) -> ComponentFuture<'_, EventReply> {
    Box::pin(async move {
        let owner = self
            .listeners
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(&(call.event.clone(), call.listener_id))
            .copied()
            .ok_or_else(|| CordisError::ComponentFailed {
                component: call.event.to_string(),
                message: format!("listener {} is not registered", call.listener_id),
            })?;
        self.route(owner).await?.call_event(call).await
    })
}
```

The listener map is `BTreeMap<(EventId, u64), FiberId>` — it keys a **specific** `listener_id` to a
single owning fiber. A dispatch finds that one fiber and routes one `handle_event` call to it. The
`mode` field is carried through the `EventCall` for the guest to see, and `next_token` is passed
through, but the host does not enumerate listeners, run them concurrently, aggregate `Break`s, or
manage a `next()` token chain.

This means: **the five modes are the declared contract, and the native core implements them
fully, but an individual Wasm guest today receives one event call at a time and is expected to
interpret `mode` and `next_token` itself.** Chapter 7's status/tracking plugin uses that fact — it
registers a single listener and reacts to the event in `handle_event`. If you need true
fan-out or a waterfall chain across several Wasm guests, the host kernel needs the mode-aware
dispatch loop that the native `event.rs` already has; that is the boundary to extend.

The practical consequence for a plugin author: register one listener per thing you want to react to,
decide the reply yourself, and don't depend on the host to sequence anything.

## `EventId { name, abi_hash }`

Identical shape to `ServiceId`. The guest constructs it by hand from a name and a 32-byte constant:

```rust
const STATUS_ABI: [u8; 32] = [0x51; 32];

fn status_event() -> EventId {
    EventId {
        name: "example.status".into(),
        abi_hash: STATUS_ABI.to_vec(),
    }
}
```

Just as with services, the host converts the incoming `list<u8>` to `[u8; 32]` in `event_from_wit`
and rejects any other length:

```rust
fn event_from_wit(event: wit::EventId) -> Result<cordis_core::EventId, wit::KernelError> {
    let hash = <[u8; 32]>::try_from(event.abi_hash.as_slice()).map_err(|_| {
        wit::KernelError::InvalidArgument("event ABI hash must contain 32 bytes".to_owned())
    })?;
    Ok(cordis_core::EventId::new(event.name, hash))
}
```

Because `EventId` derives `Ord`, a pair of independently written components that agree on name and
hash are the *same* event. Name alone would let two unrelated events collide.

## Registering a listener

A listener is registered from `activate`, by calling the host import:

```rust
fn activate(context: CallContext, _config: Vec<u8>) -> Result<(), KernelError> {
    let registration = host::register_listener(context, &status_event(), LISTENER_ID, EventMode::Serial)?;
    REGISTRATION.with(|slot| *slot.borrow_mut() = Some(registration));
    Ok(())
}
```

`register_listener` takes four arguments: the context, the event id, a **`listener_id`** (`u64`), and
the mode. The `listener_id` is your choice — it is the stable key that identifies *this* listener in
the host's map. On the host it becomes `RegistrationRequest::Listen { event, listener_id, mode }`,
which lands in the kernel's listener table keyed by `(event, listener_id)`.

The returned `Registration` is an effect-owning handle, exactly like `provide_service`. Store it (the
examples use a `thread_local!`) so `deactivate` can release it, and rely on the host as the authority
for cleanup when the fiber unloads.

Two host-side rules matter when you register:

- **The `(event, listener_id)` pair must be unique.** `RuntimeKernel::register_listener` in
  `crates/cordis-wasm/src/loader.rs` rejects a duplicate key:
  ```rust
  if listeners.contains_key(&key) {
      return Err(... "listener {listener_id} is already registered");
  }
  ```
  Register a different `listener_id` per distinct event reaction in the same fiber.
- **Dispatch only works after you register.** A `dispatch_event` for an unregistered listener id
  returns `listener {id} is not registered`. This mirrors services: declaring an interest in the
  descriptor is not enough, you must actually call `register_listener` in `activate`.

## Emitting an event

Emitting is the host import `dispatch_event`. A guest calls it to deliver a payload to a listener it
knows about:

```rust
let reply = host::dispatch_event(
    context,
    &status_event(),
    LISTENER_ID,
    EventMode::Serial,
    &payload,
    None,           // next_token
)?;
```

On the host, `GuestState::dispatch_event` validates the context and payload, converts the WIT event
to the core `EventId`, wraps everything in an `EventCall`, and routes to the owner fiber. The
`EventReply` it returns is converted back to WIT (`Continue(payload)` → `ContinueValue(payload)`,
`Break(payload)` → `BreakValue(payload)`).

Note that the emitter passes a `listener_id` — you emit to a *specific* listener, not to "everyone
watching the event." That is a consequence of the single-listener dispatch boundary discussed above.
In the native path you would emit to an `EventTarget` (global or per-realm) and the runtime would
resolve all matching listeners; in the guest path today you name the one listener you want to reach.

## `EventReply` — what the listener returns

`handle_event` returns an `EventReply`:

```rust
fn handle_event(
    _context: CallContext,
    _event: EventId,
    _listener_id: u64,
    _mode: EventMode,
    payload: Vec<u8>,
    _next_token: Option<u64>,
) -> Result<EventReply, KernelError> {
    // react to the payload ...
    Ok(EventReply::ContinueValue(output_bytes))
}
```

The two variants mirror `ControlFlow`:

- **`ContinueValue(bytes)`** — "keep going / nothing decisive to report." A pure observer returns
  this. In a serial or waterfall chain it means the listener did not short-circuit.
- **`BreakValue(bytes)`** — "this is the answer, stop." In a serial or bail chain this breaks the
  loop and becomes the result. In a waterfall, returning without calling `next()` is the short
  circuit — the guest surface models that as `BreakValue`.

Because the guest host today routes one listener per dispatch, the *effective* use of the two
variants is: the provider/dispatcher reads `BreakValue` as "the listener has something to say" and
`ContinueValue` as "no news," and the plugin decides what to do from there. Chapter 7 uses
`ContinueValue` for a logging observer and shows where `BreakValue` would carry a decision.

## A small provider+listener example

Put together, a provider that emits and a consumer that listens look like this. The **provider**
declares nothing and emits during `activate`:

```rust
// provider (illustrative shapes — real code uses the same calls as the counter provider)
fn activate(context: CallContext, config: Vec<u8>) -> Result<(), KernelError> {
    let payload = cordis_guest::encode(&"started")?;
    let _ = host::dispatch_event(context, &status_event(), LISTENER_ID, EventMode::Serial, &payload, None)?;
    Ok(())
}
```

The **consumer** injects nothing (it is not a service), declares no service, and registers a listener
in `activate`:

```rust
// consumer
fn descriptor() -> PluginDescriptor {
    PluginDescriptor {
        name: "example.wasm-status-consumer".into(),
        version: env!("CARGO_PKG_VERSION").into(),
        wit_version: cordis_guest::KERNEL_ABI.into(),
        inject: Vec::new(),
        provide: Vec::new(),
        config_schema: br#"{"type":"object","additionalProperties":false}"#.to_vec(),
        capabilities: Vec::new(),
    }
}

fn activate(context: CallContext, _config: Vec<u8>) -> Result<(), KernelError> {
    let registration = host::register_listener(context, &status_event(), LISTENER_ID, EventMode::Serial)?;
    REGISTRATION.with(|slot| *slot.borrow_mut() = Some(registration));
    Ok(())
}

fn handle_event(
    context: CallContext,
    event: EventId,
    listener_id: u64,
    mode: EventMode,
    payload: Vec<u8>,
    next_token: Option<u64>,
) -> Result<EventReply, KernelError> {
    let message = cordis_guest::decode::<String>(&payload)?;
    host::log(context, "info", &format!("status: {message}"))?;
    Ok(EventReply::ContinueValue(payload))
}
```

This is the shape chapter 7 builds into a real status-tracking surface. The key difference from the
counter consumer is that it **registers a listener** rather than calling a service — events are
reactive, services are imperative.

## Events vs services, in one table

| | Service | Event |
|---|---|---|
| Registration | `host::provide_service` in `activate` | `host::register_listener` in `activate` |
| Trigger | A consumer `call_service` | An emitter `dispatch_event` |
| Target | Resolved via caller's committed view + realm | A specific `(event, listener_id)` |
| Guest method | `call_service` | `handle_event` |
| Return | `Vec<u8>` (encoded reply) | `EventReply` (`ContinueValue` / `BreakValue`) |
| Use case | "Give me a value now" | "Something happened; react" |

Next: [Configuration and the sandbox](05-config-and-capabilities.md) — config bytes, the JSON
Schema, `ArtifactPolicy`, `WasiCapabilities`, and `WasmLimits`.
