# Guest SDK (`cordis-guest`)

`cordis-guest` is what a WebAssembly plugin author depends on. It provides the generated kernel
bindings, a handful of typed helper functions for the MessagePack boundary, and the `Guest` trait the
plugin implements. The plugin is compiled to a Wasmtime Component that exports the `cordis:kernel@0.1.0`
`plugin` interface; the host imports the `host` interface.

The guest is **untrusted by assumption**: the host effect table is the final authority. A guest that
drops its `Registration` handle without calling `drop` is still cleaned up by the host's
`force_cleanup`, because the host holds the `EffectGuard` (see [wasm](wasm.md)).

## `bindings`

```rust
pub mod bindings {
    wit_bindgen::generate!({
        path: "wit",
        world: "cordis-plugin",
        pub_export_macro: true,
    });
}
```

Generated Cordis kernel imports and plugin exports. `pub_export_macro: true` generates the
`export!` macro used by `export_plugin!`. The concrete Rust items come from the WIT in
`crates/cordis-guest/wit/kernel.wit` (see [wasm](wasm.md) for the full WIT).

## `host` / `plugin` re-exports

```rust
pub use bindings::cordis::kernel::host;
pub use bindings::exports::cordis::kernel::plugin;
```

- `host` — the imported interface a guest uses to reach the host: `host::call_service`,
  `host::provide_service`, `host::register_listener`, `host::dispatch_event`, `host::log`, plus the
  types `CallContext`, `ServiceId`, `EventId`, `EventMode`, `EventReply`, `KernelError`, `Registration`.
- `plugin` — the exported interface the guest implements: `plugin::Guest`, `plugin::PluginDescriptor`.

## `KERNEL_ABI`

```rust
pub const KERNEL_ABI: &str = "0.1";
```

The kernel ABI implemented by this SDK release. A plugin reports it in its descriptor's `wit_version`;
the host checks it against `ArtifactPolicy::kernel_abi`.

## `encode` / `decode`

```rust
pub fn encode<T: Serialize>(value: &T) -> Result<Vec<u8>, host::KernelError>;
pub fn decode<T: DeserializeOwned>(bytes: &[u8]) -> Result<T, host::KernelError>;
```

Encodes / decodes a typed service/event value for the dynamic kernel boundary, using `rmp-serde`
(MessagePack) — the same canonical codec as the host's `encode_service_payload` /
`encode_event_payload`, so native and Wasm components speak the same wire format.

**Errors** — `invalid-argument` when serialization fails / the payload does not match `T`.

## `schema_json`

```rust
pub fn schema_json(value: &serde_json::Value) -> Result<Vec<u8>, host::KernelError>;
```

Encodes a plugin configuration schema for its descriptor (JSON bytes).

**Errors** — `invalid-argument` if JSON serialization fails.

## `call_service`

```rust
pub fn call_service<Req, Res>(
    context: &host::CallContext,
    service: &host::ServiceId,
    method: u32,
    request: &Req,
) -> Result<Res, host::KernelError>
where
    Req: Serialize,
    Res: DeserializeOwned;
```

Calls a host service with typed MessagePack input and output. Encodes `request`, calls
`host::call_service(*context, service, method, &payload)`, and decodes the reply into `Res`.

**Errors** — the host error or a request/reply codec error.

## `export_plugin!`

```rust
#[macro_export]
macro_rules! export_plugin {
    ($component:ident) => {
        $crate::bindings::export!($component with_types_in $crate::bindings);
    };
}
```

Exports a type implementing the generated plugin `Guest` trait. Place it at the bottom of the crate
root: `cordis_guest::export_plugin!(CounterProvider);`.

## The `Guest` trait

The WIT `plugin` interface generates a `Guest` trait (in `cordis_guest::plugin`) that you implement.
Its methods correspond one-to-one to the WIT `plugin` functions:

```rust
pub trait Guest {
    fn descriptor() -> PluginDescriptor;
    fn activate(context: CallContext, config: Vec<u8>) -> Result<(), KernelError>;
    fn deactivate(context: CallContext) -> Result<(), KernelError>;
    fn call_service(
        context: CallContext, service: ServiceId, method: u32, payload: Vec<u8>,
    ) -> Result<Vec<u8>, KernelError>;
    fn handle_event(
        context: CallContext, event: EventId, listener_id: u64, mode: EventMode, payload: Vec<u8>,
        next_token: Option<u64>,
    ) -> Result<EventReply, KernelError>;
}
```

- `descriptor` returns the plugin's static descriptor: `name`, `version`, `wit_version`
  (`KERNEL_ABI`), `inject`/`provide` service ids, `config_schema` (JSON bytes), `capabilities`.
- `activate` is called once, with the context and the config payload, when the fiber loads. It may
  call `host::provide_service` / `host::register_listener` to register with the host.
- `deactivate` is called on unload; the host then force-cleans any remaining registrations.
- `call_service` dispatches one service call to the plugin.
- `handle_event` dispatches one event callback; returns `EventReply::ContinueValue` /
  `EventReply::BreakValue`.

## Example: `wasm-counter-provider`

From `examples/wasm-counter-provider/src/lib.rs`:

```rust
use cordis_guest::host::{
    self, CallContext, EventId, EventMode, EventReply, KernelError, ServiceId,
};
use cordis_guest::plugin::{Guest, PluginDescriptor};
use std::cell::RefCell;

const COUNTER_ABI: [u8; 32] = [0x43; 32];
const GET_METHOD: u32 = 1;

thread_local! {
    static REGISTRATION: RefCell<Option<host::Registration>> = const { RefCell::new(None) };
    static VALUE: RefCell<u64> = const { RefCell::new(0) };
}

struct CounterProvider;

impl Guest for CounterProvider {
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

    fn activate(context: CallContext, _config: Vec<u8>) -> Result<(), KernelError> {
        let registration = host::provide_service(context, &counter_service())?;
        REGISTRATION.with(|slot| *slot.borrow_mut() = Some(registration));
        Ok(())
    }

    fn deactivate(_context: CallContext) -> Result<(), KernelError> {
        REGISTRATION.with(|slot| slot.borrow_mut().take());
        Ok(())
    }

    fn call_service(
        _context: CallContext, service: ServiceId, method: u32, payload: Vec<u8>,
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

    fn handle_event(
        _context: CallContext, _event: EventId, _listener_id: u64, _mode: EventMode,
        payload: Vec<u8>, _next_token: Option<u64>,
    ) -> Result<EventReply, KernelError> {
        Ok(EventReply::ContinueValue(payload))
    }
}

fn counter_service() -> ServiceId {
    ServiceId { name: "example.counter".into(), abi_hash: COUNTER_ABI.to_vec() }
}

cordis_guest::export_plugin!(CounterProvider);
```

The consumer (examples/wasm-counter-consumer) is the mirror image: it declares
`inject: vec![counter_service()]` and, in `activate`, calls
`cordis_guest::call_service(&context, &counter_service(), GET_METHOD, &1_u64)`. Note the ABI hash
`[0x43; 32]` is hardcoded on each side — the guest does not run the proc macro, so it must match the
host's generated hash by convention.

## Errors

`KernelError` variants (from the WIT): `invalid-argument`, `inactive-context`,
`inactive-dependency`, `undeclared-dependency`, `capability-denied`, `internal`. `encode`/`decode`/
`schema_json`/`call_service` return `KernelError` when the codec or the host rejects the operation.
