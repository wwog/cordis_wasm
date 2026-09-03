# 2. Anatomy of a guest plugin

A guest plugin is a Wasmtime **component** that implements one WIT world, `cordis-plugin`. The
SDK in `crates/cordis-guest/` turns that world into a handful of Rust items you fill in. Everything
you write in a guest crate is, at its core, an implementation of a single trait — the `Guest` trait
from the generated bindings.

This chapter dissects the two shipped guests. They are small enough to read whole, and together they
cover every method of the trait except the ones a reader must supply for the more interesting
plugins in chapters 6 and 7.

## The generated bindings

`crates/cordis-guest/src/lib.rs` opens with one macro call:

```rust
pub mod bindings {
    wit_bindgen::generate!({
        path: "wit",
        world: "cordis-plugin",
        pub_export_macro: true,
    });
}

pub use bindings::cordis::kernel::host;
pub use bindings::exports::cordis::kernel::plugin;
```

`wit_bindgen::generate!` reads `wit/kernel.wit` and generates:

- `host` — the **imports** a guest can call. These are the functions a guest uses to reach back
  into the runtime: `call_service`, `provide_service`, `register_listener`, `dispatch_event`, `log`.
- `plugin` — the **exports** a guest must implement. This is the `Guest` trait with its five
  methods, and the `PluginDescriptor` record.

The WIT kernel world (`crates/cordis-guest/wit/kernel.wit`) is the fixed, versioned contract between
host and guest. The host copy at `crates/cordis-wasm/wit/kernel.wit` is asserted byte-identical to
the guest copy by a test in `crates/cordis-wasm/src/lib.rs`:

```rust
assert_eq!(
    include_str!("../../cordis-guest/wit/kernel.wit"),
    include_str!("../wit/kernel.wit")
);
```

This is why there is exactly one kernel ABI: the guest and host compile against the same definition,
and a mismatch is caught at load time (chapter 8).

## The `Guest` trait

The generated trait you implement lives on the `plugin` module. Its five methods:

| Method | Called by the host | What it must do |
|---|---|---|
| `descriptor()` | At load time, before activation | Return static metadata: name, version, kernel WIT version, injects, provides, config schema, capabilities. |
| `activate(context, config)` | When all injected services are available | Register services/listeners, start background work. `config` is the bytes from the entry's `config` in `cordis.json`. |
| `deactivate(context)` | When the fiber unloads | Release what `activate` acquired that the runtime does not own. |
| `call_service(context, service, method, payload)` | When another component routes a call to a service you provide | Dispatch method `method` on service `service` with the given payload; return encoded bytes. |
| `handle_event(context, event, listener_id, mode, payload, next_token)` | When an event you listen to fires | Produce an `EventReply`. |

The two exported `ServiceId`s and `EventId`s you see everywhere are typed aliases for the WIT
records:

```wit
record service-id { name: string, abi-hash: list<u8> }
record event-id   { name: string, abi-hash: list<u8> }
```

`abi-hash` is a 32-byte list. The guest uses a `[u8; 32]` constant — see `COUNTER_ABI` in the
examples — and converts it with `.to_vec()` when it constructs a `ServiceId`. The host converts the
list back to `[u8; 32]` in `service_from_wit` / `event_from_wit` and rejects any length that is not
exactly 32 bytes. Chapter 3 explains what that hash means; here you only need to know that it is a
fixed value you must supply.

## `descriptor()` and `PluginDescriptor`

`descriptor()` is the only method the host calls before it knows *anything* about what your
component does, and the `PluginDescriptor` it returns is the entire load-time contract.

```rust
fn descriptor() -> PluginDescriptor {
    PluginDescriptor {
        name: "example.wasm-counter-provider".into(),
        version: env!("CARGO_PKG_VERSION").into(),
        wit_version: cordis_guest::KERNEL_ABI.into(),
        inject: Vec::new(),
        provide: vec![counter_service()],
        config_schema: br#"{"type":"object","additionalProperties":false}"#.to_vec(),
        capabilities: Vec::new(),
    }
}
```

The fields, in the order the WIT record declares them:

- **`name`** — the human-readable identity shown in `check` output and in diagnostics. There is no
  enforcement that it is unique across the application; it is metadata. The two examples use a
  dotted prefix (`example.wasm-counter-provider`) as a convention to namespace names.
- **`version`** — semver string. The examples use `env!("CARGO_PKG_VERSION")`, so it tracks the crate
  version.
- **`wit_version`** — the kernel ABI this SDK targets. Always `cordis_guest::KERNEL_ABI`, which is
  `"0.1"`. The host compares this against its `ArtifactPolicy::kernel_abi` (also `"0.1"`) and
  rejects a mismatch before activation.
- **`inject`** — the services this component *requires*. Each is a full `ServiceId`. That list is
  what keeps the fiber in `Pending` until every entry is provided in the resolved realm. The
  consumer sets `inject: vec![counter_service()]`; the provider sets `inject: Vec::new()`.
- **`provide`** — the services this component *offers*. The host does not route a call to you unless
  the service you provide is matched by identity. The provider sets
  `provide: vec![counter_service()]`; the consumer sets `provide: Vec::new()`.
- **`config_schema`** — a JSON Schema, as JSON bytes, describing the shape of `config`. The host
  parses it and validates the entry's `config` against it *before* activation. See below.
- **`capabilities`** — the WASI capabilities this component needs permission to use (for example
  `"network"`, `"filesystem"`, `"random"`). The default policy allows none of them, so for the
  counter examples it is `Vec::new()`. Chapter 5 and chapter 6 make this concrete.

### What a strict `config_schema` means

The provider uses:

```rust
config_schema: br#"{"type":"object","additionalProperties":false}"#.to_vec(),
```

That JSON parses to a JSON Schema (Draft 2020-12) that says: *the config must be an object, and it
may not contain any properties not named in the schema.* Since the schema also names no `properties`
at all, the only valid config is the empty object `{}`. That is why `examples/wasm-app/cordis.json`
gives both entries `"config": {}` and why `check` accepts it.

This is the strictest "no configuration" schema the runtime supports. If you instead omitted the
schema or wrote `{}` (an empty object, which in JSON Schema means "any value"), then a `config`
with arbitrary fields would pass validation. The design pattern is: declare the shape precisely, and
use `additionalProperties: false` to reject typos. Chapter 5 shows a schema that actually accepts a
`port` and a `root`, and chapter 6 uses it.

Two details about how `config_schema` flows through the host:

1. The bytes must be valid JSON *and* a valid JSON Schema. The host parses it with
   `serde_json::from_slice::<Value>` in `descriptor_from_wit` (runtime.rs), then converts it to a
   `schemars::Schema` via `Schema::try_from`. If either step fails you get a `Descriptor` error at
   load time.
2. It is validated by **two** paths. `check` validates config against the schema during preflight,
   and `run`/`inspect` validate it again when a real `WasmEntryDriver` starts an entry (see
   `validate_config` in `crates/cordis-wasm/src/loader.rs`). Both use the same
   `jsonschema::draft202012` validator on the JSON-serialized `config`.

## `activate`, `deactivate`, and the thread-local registration pattern

The provider's `activate` is the heart of the guest:

```rust
fn activate(context: CallContext, _config: Vec<u8>) -> Result<(), KernelError> {
    let registration = host::provide_service(context, &counter_service())?;
    REGISTRATION.with(|slot| *slot.borrow_mut() = Some(registration));
    Ok(())
}

fn deactivate(_context: CallContext) -> Result<(), KernelError> {
    REGISTRATION.with(|slot| slot.borrow_mut().take());
    Ok(())
}
```

Two things to notice.

First, `host::provide_service` returns a `Registration` — a handle to the host-side registration.
The guest SDK threads this through by hand today: the example stores it in a `thread_local!` slot so
`deactivate` can take it back out. It also relies on the host being authoritative for cleanup (the
README's "guest is untrusted" assumption): even if the guest never drops the handle, the host clears
all registrations for the store when the fiber unloads. Chapter 3 contrasts this manual threading
with the native macro path that hides it all.

Second, `activate` is the only place a guest registers anything. There is no "on every call"
registration — a service is registered once during activation and stays until deactivation. That is
why `deactivate` must undo it: the runtime won't call `deactivate` if it also auto-released the
registration, so the guest is responsible for both sides of the acquisition.

## `call_service` — providing a method

The provider's `call_service` dispatches one method:

```rust
fn call_service(
    _context: CallContext,
    service: ServiceId,
    method: u32,
    payload: Vec<u8>,
) -> Result<Vec<u8>, KernelError> {
    if service.name != counter_service().name || method != GET_METHOD {
        return Err(KernelError::InvalidArgument("unknown service method".into()));
    }
    let increment = if payload.is_empty() {
        1
    } else {
        cordis_guest::decode::<u64>(&payload)?
    };
    let value = VALUE.with(|value| {
        let mut value = value.borrow_mut();
        *value += increment;
        *value
    });
    cordis_guest::encode(&value)
}
```

The shape of a `call_service` implementation is always the same:

- **Match the service and the method.** Here the guest checks both the service name and a numeric
  `method` constant (`GET_METHOD = 1`). If the call does not match, it returns an error. `method` is
  a `u32` because the kernel boundary is type-erased; the numeric id is the first four bytes of the
  BLAKE3 hash of the method signature (chapter 3).
- **Decode the payload.** The payload is `MessagePack` bytes. `cordis_guest::decode::<T>(&bytes)`
  turns it into `T`, and `cordis_guest::encode(&value)` turns the reply back. The empty payload case
  — `payload.is_empty()` — is a small convenience: the consumer calls with `&1_u64` (encodes to a few
  bytes) so it isn't hit here, but the guest tolerates an empty body by defaulting to `1`.
- **Return encoded bytes.**

Note that the guest never inspects the `CallContext` in `call_service` here. It is threaded through
because the host validates that the context belongs to the store, and a guest that issues a
*sub-call* — e.g. a consumer calling `call_service` — needs it. The provider just ignores it.

## `handle_event` — the real example passes through

The provider's `handle_event` is essentially a no-op:

```rust
fn handle_event(
    _context: CallContext,
    _event: EventId,
    _listener_id: u64,
    _mode: EventMode,
    payload: Vec<u8>,
    _next_token: Option<u64>,
) -> Result<EventReply, KernelError> {
    Ok(EventReply::ContinueValue(payload))
}
```

It echoes the payload back as a `ContinueValue`. This is the minimum a guest must return when it
receives an event; it does not actually *listen* to anything (it never calls `register_listener`).
Chapter 4 explains the five modes and what `ContinueValue` vs `BreakValue` mean, and chapter 7 turns
this into a real listener.

## `export_plugin!`

The last line of every guest crate is the macro call:

```rust
cordis_guest::export_plugin!(CounterProvider);
```

It expands to:

```rust
bindings::export!(CounterProvider with_types_in bindings);
```

That is the `wit_bindgen` export macro, which wires your `Guest` impl to the generated component
exports so Wasmtime can call into it. It must be invoked once, at crate root, naming the type that
implements `Guest`. If you forget it, the component compiles but exports nothing, and the host
fails to link it.

## The `CallContext`

Every host entry point receives a `CallContext`:

```wit
record call-context {
    fiber-id: u64,
    effect-id: u64,
}
```

The two fields are the identity of the *currently executing* fiber's access scope: the fiber that
owns the call, and the effect (one activation of a fiber) it is running inside. The host uses it for
two things:

- **Validation.** In every `host::*` entry point, `crates/cordis-wasm/src/runtime.rs` calls
  `validate_context` and compares the context's `fiber_id` and `effect_id` to the ones the store was
  mounted with. A mismatch returns `InvalidArgument` — this is how the host rejects a guest that
  smuggles a context from one fiber into another.
- **Sub-calls.** When a guest calls back into the runtime (say `call_service`), it passes the
  context it was given, and the host uses that context to resolve the *current* fiber's committed
  dependency view. This is why a guest must thread the context through rather than reconstructing it
  itself: the host is the single authority on what a fiber may see.

A guest that never calls back into the host can ignore the context (as the counter provider does in
`call_service`); a guest that does must pass it along, unchanged.

## The pieces, assembled

A guest crate is therefore always the same skeleton. Fill in the four data methods and one or more
of the dispatch/event methods, and export:

```rust
use cordis_guest::host::{CallContext, EventId, EventMode, EventReply, KernelError, ServiceId};
use cordis_guest::plugin::{Guest, PluginDescriptor};

struct MyPlugin;

impl Guest for MyPlugin {
    fn descriptor() -> PluginDescriptor { /* name, version, wit_version, inject, provide, config_schema, capabilities */ }
    fn activate(context: CallContext, config: Vec<u8>) -> Result<(), KernelError> { /* register services/listeners */ }
    fn deactivate(_context: CallContext) -> Result<(), KernelError> { /* release them */ }
    fn call_service(_context: CallContext, service: ServiceId, method: u32, payload: Vec<u8>) -> Result<Vec<u8>, KernelError> { /* dispatch */ }
    fn handle_event(_context: CallContext, event: EventId, listener_id: u64, mode: EventMode, payload: Vec<u8>, next_token: Option<u64>) -> Result<EventReply, KernelError> { /* reply */ }
}

cordis_guest::export_plugin!(MyPlugin);
```

The `activate`/`deactivate` pair and the descriptor are the minimum for a component to make it past
load; `call_service` and `handle_event` are where the behavior lives. The next chapter shows how the
`provide`/`inject` halves of the descriptor connect two components, and how the guest threads the
kernel boundary by hand.

Next: [Services and injection](03-services-and-inject.md) — `provide`/`inject`, `ServiceId` and the
ABI hash, realms, and the native macro contrast.
