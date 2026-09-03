# 3. Services and injection

A **service** is a named capability one plugin provides and others consume. The counter example is
two plugins and one service: the provider owns `example.counter`, the consumer calls it. This
chapter explains how a service identity works, why it carries a 32-byte hash, how a service gets
from "provided in `activate`" to "routable by name," and how the `isolate` map in `cordis.json`
decides which provider a consumer sees. It also contrasts the guest's hand-rolled registration with
the native macro path that papered over all of it.

## `ServiceId { name, abi_hash }`

A service identity is two fields:

```wit
record service-id {
    name: string,
    abi-hash: list<u8>,
}
```

On the guest side these become the values you construct directly:

```rust
const COUNTER_ABI: [u8; 32] = [0x43; 32];

fn counter_service() -> ServiceId {
    ServiceId {
        name: "example.counter".into(),
        abi_hash: COUNTER_ABI.to_vec(),
    }
}
```

`name` is a string in one flat namespace per application. The counter example uses the dotted
convention `example.counter` to namespace it, but nothing enforces uniqueness. The consumer and
provider both define the *same* `counter_service()` function so the name and the hash line up.

### Why the ABI hash exists

`name` alone is not enough to decide that two components agree on a service protocol. Two plugins
could both name a service `example.counter` and mean entirely different things — one could expose
`get() -> u64`, the other `increment(delta: u32) -> Result<(), E>`. If the runtime matched on name
only, a consumer compiled against the first would silently route calls to the second and fail in
confusing ways.

The hash disambiguates. In the native path, `#[cordis::service]` computes a **BLAKE3** digest over a
canonical string built from the service name plus every method's *canonical signature*. Look at
`service_abi_hash` in `crates/cordis-macros/src/lib.rs`:

```rust
fn service_abi_hash(name: &str, methods: &[ServiceMethod]) -> [u8; 32] {
    let mut canonical = String::from(name);
    let mut signatures = methods
        .iter()
        .map(|method| canonical_service_method(&method.method, &method.arguments, &method.ok, &method.error))
        .collect::<Vec<_>>();
    signatures.sort_unstable();
    for signature in signatures {
        canonical.push('\n');
        canonical.push_str(&signature);
    }
    hash_text(&canonical)
}
```

The README states the property precisely: the hash is derived only from the service name, the method
names, the parameter types in order, and the return type. Comments, parameter renames, and method
declaration order do **not** affect it. So two independently written consumers and providers get the
same hash as long as their wire contract matches, and a mismatch is caught before any call is
routed.

The guest path does not run this macro — it supplies a hard-coded `[u8; 32]` constant. That is the
contract: you, the plugin author, are responsible for choosing a hash that matches the protocol you
are speaking. In practice the two ends of a protocol define a shared constant (as the two counter
crates do with `COUNTER_ABI = [0x43; 32]`); a real `cordis` service macro could compute it from the
Rust types and you would copy the resulting bytes into the guest.

### How the host matches the hash

The host converts between the WIT `list<u8>` and its internal `[u8; 32]` in `crates/cordis-wasm/src/runtime.rs`:

```rust
fn hash_from_bytes(bytes: &[u8]) -> Result<[u8; 32], WasmHostError> {
    <[u8; 32]>::try_from(bytes).map_err(|_| WasmHostError::Descriptor {
        message: "service ABI hash must contain 32 bytes".to_owned(),
    })
}
```

This is a strict conversion. If a guest sends a hash that is not exactly 32 bytes the load fails with
a `Descriptor` error, before any activation. Once converted, the internal `ServiceId` derives `Eq`
and `Ord` over **both** fields, so two ids are equal only when name *and* hash match. The supervisor
looks providers up by `ServiceId` — so a provider only satisfies a consumer when the full identity
aligns.

## How a service becomes routable

A provider does not *expose* a method directly. It calls `provide_service` during `activate`, and
the host does the rest:

**Guest side** (provider):

```rust
fn activate(context: CallContext, _config: Vec<u8>) -> Result<(), KernelError> {
    let registration = host::provide_service(context, &counter_service())?;
    REGISTRATION.with(|slot| *slot.borrow_mut() = Some(registration));
    Ok(())
}
```

**Host side** — `GuestState::provide_service` → `add_registration` → `InstanceHost::register`, which
eventually reaches `RuntimeKernel::provide_service` in `crates/cordis-wasm/src/loader.rs`:

```rust
fn provide_service(&self, fiber: FiberId, key: ProviderKey, scope: EffectScope) -> ComponentFuture<'_, ()> {
    Box::pin(async move {
        self.runtime.provide(key.clone(), fiber).await?;
        // register a disposer that withdraws on teardown ...
    })
}
```

`runtime.provide(key, fiber)` records that `fiber` provides the service in that realm. From that
moment, any consumer whose committed view resolves to this provider can call it. The registration is
an **effect**: the host registers a `Disposer` that calls `runtime.withdraw` when the fiber unloads,
so the service disappears with its provider.

What the guest gets back from `provide_service` is a `Registration` handle. The host is the
authority for cleanup — even a guest that drops the handle does not wrinkle the effect, because the
disposer is owned by the fiber's `EffectGuard`. That is the "guest is untrusted" assumption from the
README, and it is why the guest-side pattern of storing the registration in a `thread_local!` is
about *ordering deactivation* rather than *guaranteeing* cleanup.

## `call_service` — the consumer side

The consumer invokes the service during `activate`:

```rust
fn activate(context: CallContext, _config: Vec<u8>) -> Result<(), KernelError> {
    let _: u64 = cordis_guest::call_service(&context, &counter_service(), GET_METHOD, &1_u64)?;
    Ok(())
}
```

`cordis_guest::call_service` is the typed wrapper around the host import:

```rust
pub fn call_service<Req, Res>(context: &host::CallContext, service: &host::ServiceId, method: u32, request: &Req) -> Result<Res, host::KernelError> {
    let payload = encode(request)?;
    let reply = host::call_service(*context, service, method, &payload)?;
    decode(&reply)
}
```

It encodes `Req` to MessagePack, calls the host import, and decodes `Res`. On the host, this import
(`GuestState::call_service`) does exactly the routing you would hope:

1. Validates the context and payload limits.
2. Resolves the *current* fiber's committed dependency view with `runtime.commit_dependencies(fiber)`.
3. Looks up the provider for the service in that view — `committed.lookup(&call.service)`.
4. Routes the call to the provider fiber through `self.route(provider).await?.call_service(call)`.

The critical detail is **step 2**. The call is routed through the caller's committed view, not by
global name. A consumer can only reach a service it declared in `inject`, and only the provider that
its *context* resolves. This is what makes the realm wiring (below) effective: routing is a function
of the caller's context, not of the service name alone.

## Realms: how `isolate` wires consumer to provider

Re-read the relevant part of `examples/wasm-app/cordis.json`:

```json
{ "id": "consumer", "component": "file:...", "config": {}, "isolate": { "example.counter": "example" } },
{ "id": "provider", "component": "file:...", "config": {}, "isolate": { "example.counter": "example" } }
```

Both entries map the service `example.counter` to the realm label `example`. The loader, in
`WasmEntryDriver::entry_context`, collects the union of the descriptor's injects and provides, and
for each one calls `realm_for`:

```rust
async fn realm_for(&self, entry: &ResolvedEntry, service: &ServiceId) -> Result<RealmId, LoaderError> {
    let key = match entry.realms.get(service.name()) {
        Some(ManagedRealm::Local { owner, service }) => RealmKey::Local(owner.clone(), service.clone()),
        Some(ManagedRealm::Global { label, service }) => RealmKey::Global(label.clone(), service.clone()),
        None => RealmKey::Default(service.clone()),
    };
    ...
}
```

The value in the `isolate` map determines the realm key:
- a bare **string** (`"example"`) → a **global** realm shared by every entry that names that label;
- **`true`** → a **local** realm scoped to that entry and its descendants only;
- **absent** → the **default** realm for that service.

Both entries resolve `example.counter` to the same global realm key, so both get the same
`RealmId`. The provider registers its service in that realm; the consumer's committed view resolves
to it.

This is why **both** entries must carry the mapping. If the consumer omitted it, it would resolve
`example.counter` in the default realm — where no provider registered — and its fiber would stay
`Pending` forever (the classic "never activates" symptom, chapter 8). If the provider omitted it, the
provider would register in the default realm while the consumer looks in `example`, and the two would
never meet.

Realms are the runtime's isolation mechanism: put two providers of the same service in different
realms and each consumer sees only the one its context names. The `"example"` label is shared by the
two entries precisely so they *do* meet. The paper calls this realm isolation; the tutorial calls it
"the two entries agree on where the service lives."

## Construct vs runtime service routing

There are two distinct moments in a service's life, and it helps to name them:

- **Construct (load) time.** The descriptor's `provide` and `inject` lists are read when the
  component is loaded. `provide` seeds the set of services that may be routed here; `inject` seeds the
  dependency resolution that decides whether the fiber may activate. Neither list does any actual
  routing yet — they are the *declared* intent.

- **Runtime (call) time.** `call_service` and `provide_service`/`register_listener` happen when the
  fiber is active. `provide_service` registers into a realm; `call_service` resolves the caller's
  committed view and routes to a specific provider fiber. The **committed view** is the key: it is
  the provider selection frozen at the moment the consumer activated, recorded as a `FiberId`, not a
  value. If the provider is later replaced, the supervisor recomputes affected consumers, which
  reload against the new provider — and the committed view changes with it.

The practical rule: a component's *declared* `provide` list makes it a candidate; its actual
`provide_service` call in `activate` makes it the *current* provider in a realm. A descriptor that
declares a service in `provide` but never calls `provide_service` will be routable-in-principle but
not present-in-practice, and a consumer waiting on it stays `Pending`.

## Contrast: the native `#[cordis::service]` macro path

Everything above describes the **guest**, which does the kernel handshake by hand. The native path
is deliberate and far less manual. Consider what the macro generates for a service trait (see
`crates/cordis-macros/src/lib.rs` and `crates/cordis-core/src/native.rs`):

```rust
// You write:
#[cordis::service]
trait Counter {
    async fn get(&self, key: u64) -> Result<u64, CordisError>;
}
```

The macro expands that into several generated items, including:

- a **marker** type implementing `ServiceKey` with the computed `NAME` and `ABI_HASH`;
- a **client** struct with a `service_id()` accessor and a generated method per trait method;
- two constructors:
  - `new(Arc<dyn ServiceDispatcher>)` — the dynamic path, which wraps the dispatcher in a
    `ServiceClient` after verifying the dispatcher's service id **matches the expected id**. This is
    the same `ServiceClient::new::<S>(dispatcher)` the README calls the MessagePack dynamic path,
    and it is the path the Wasm boundary reuses.
  - `from_native(Arc<T>)` where `T: Counter` — the zero-serialization fast path, which wraps the
    concrete service directly in an object-safe adapter. Note the README: `from_native` is the
    native static-fast path, `new` is the dynamic path.
- a **dispatcher** struct implementing `ServiceDispatcher` so the host can route to it generically.

On the native side, a component gets its dependencies via a generated `DependencySet`:

```rust
impl ::cordis::DependencySet for CounterDependencies {
    fn injects() -> Vec<InjectSpec> {
        vec![InjectSpec::required(Counter::service_id())]
    }
    fn resolve(resolver: &dyn DependencyResolver) -> Result<Self, CordisError> {
        Ok(Self::new(CounterClient::new(resolver.resolve(&Counter::service_id())?)?))
    }
}
```

So the native path **generates** the ABI hash, generates the client, and generates the dependency
resolution. The glue the guest does by hand — constructing `ServiceId` with a matching hash, calling
`call_service` with the right method id, storing a `Registration` in a thread-local — is the macro's
job on the native side.

The contrast in one line: **the native macro generates the identity, the client, and the dependency
resolution; the guest does all three by hand.** The repository's stance today is that the guest is
the lower-level, explicit path — the counter examples are its template — while the macro path is
what an embedding host uses for native components. Chapter 6's web-server plugin is exactly the kind
of plugin where you will feel that gap: you have to compute the method id and the hash yourself, or
reuse a constant the host side recognizes.

### Why the guest does it by hand today

There is no `#[cordis::service]`-equivalent macro for a `wasm32-wasip2` guest that produces a WIT
`service-id` with a matching hash. The `cordis-guest` crate is deliberately minimal: it generates the
kernel bindings and a few helpers, but not a service-derivation layer. So a guest plugin either:

- copies the constant hash from the protocol's source of truth (as the counter crates do), or
- uses a service id whose hash the **host** was built with.

Either way the hash is a fixed `[u8; 32]` in your guest, and you own the task of keeping the two
sides aligned. This is a real boundary worth stating plainly: it is not a bug, it is the current
state of the guest SDK, and it is one of the first places you will touch if you extend it.

## The service lifecycle, condensed

| Moment | What happens | Where |
|---|---|---|
| Load | Descriptor read; `inject`/`provide` lists become dependency resolution input | `WasmComponentFactory::from_bytes` → `descriptor_from_wit` |
| Activate | `provide_service` registers the provider in its realm; the effect owns cleanup | Guest `activate` → `GuestState::provide_service` → `RuntimeKernel::provide_service` |
| Call | Caller's committed view resolves the provider fiber; call routed to it | `GuestState::call_service` → `RuntimeKernel::call_service` |
| Unload | Disposer calls `withdraw`; provider removed from its realm | `RuntimeKernel::provide_service` scope defer |

Next: [Events](04-events.md) — the WIT event surface, the five `EventMode`s, `EventReply`, and
listener registration.
