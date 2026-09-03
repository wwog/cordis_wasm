# 6. Writing a web server plugin

This is the main exercise. You will write a plugin that *provides* a web service — a guest component
that opens a listener and serves requests. It is also the chapter where the runtime's real
constraints surface hardest, so the first thing to do is state them plainly before you write a
single line.

## The realistic constraints

A guest component runs inside a Wasmtime store. It reaches the outside world only through the WIT
kernel interface and the WASI interfaces the host links in. Two hard facts shape everything:

**Fact 1 — the host provides `wasi:sockets`, not `wasi:http`.** `build_linker` in
`crates/cordis-wasm/src/runtime.rs` calls `wasmtime_wasi::p2::add_to_linker_async`, which links the
filesystem, clocks, random, stdio, and **sockets** interfaces. That `p2` module in wasmtime-wasi
48.0.1 wires `wasi:sockets/network`, `tcp`, `udp`, `ip-name-lookup`, and `instance-network` — but it
does **not** wire `wasi:http`. There is no HTTP handler in the linker. A guest that imports
`wasi:http` will fail to *link*, not merely to run, because no host function backs that import.

So "a web service" in this repo means: **open a TCP socket with `wasi:sockets`, and speak the HTTP
protocol yourself** (or port a tiny HTTP/1.1 parser into the guest). The `wasi:http/ → network`
mapping you saw in chapter 5's `capability_for_wasi_import` tells the *policy checker* that a guest
importing `wasi:http` should be treated like a network user — but the linker has nothing to back it
today. If you want `wasi:http` to work, the host must add an HTTP implementation; that is a host-side
extension, and it is outside what the current crate ships.

**Fact 2 — `network` is denied twice, and the CLI denies it outright.** Even a guest that only uses
`sockets` needs two separate "allows" to pass before it can do anything:

1. **Cordis policy.** `ArtifactPolicy::default()` has `allowed_capabilities = {}` — nothing is
   allowed. A guest declaring `"network"` in its `capabilities` is rejected by `validate_descriptor`
   unless the host policy adds `Capability::new("network")`. And the CLI always builds
   `ArtifactPolicy::default()`. So `cargo run -p cordis-cli -- run ...` cannot grant network to any
   plugin today.
2. **WASI sockets context.** Even if the Cordis policy allowed it, wasmtime-wasi's `WasiSocketsCtx`
   defaults `tcp`, `udp`, and `ip_name_lookup` to **disabled** (`AllowedNetworkUses` derives
   `Default`, all false). The `WasiCtxBuilder` in `WasiCapabilities::build` (capability.rs) never
   calls `allow_tcp(true)`, `allow_udp(true)`, `allow_ip_name_lookup(true)`, or `inherit_network()`.
   So at the transport level every socket connect/bind would be denied with `PermissionDenied`.

The boundary is therefore explicit and honest: **the current host, as shipped, cannot run a
network-capable guest.** The guest code is writable and correct; the host side needs three additions
to let it run — an `ArtifactPolicy` with `"network"` allowed, a `WasiCapabilities`/`WasiCtxBuilder`
that enables the relevant socket uses, and (for `wasi:http`) a linked HTTP implementation. This
chapter gives you both sides: the guest you would write, and the host configuration you must supply.

## (a) The guest crate

Create a guest crate alongside the existing examples — say
`examples/wasm-http-server/` with these essential files.

`Cargo.toml` — same shape as the counter examples, with `cordis-guest` as the only dependency and
`crate-type = ["cdylib"]`:

```toml
[package]
name = "wasm-http-server"
version.workspace = true
edition.workspace = true
rust-version.workspace = true
license.workspace = true
publish = false

[lib]
crate-type = ["cdylib"]

[dependencies]
cordis-guest = { path = "../../crates/cordis-guest", version = "0.1.0" }

[lints]
workspace = true
```

Add `"examples/wasm-http-server"` to the workspace `members` in the root `Cargo.toml`.

Now the plugin. The structure is the same skeleton as chapter 2, with three things different: the
descriptor declares a **provided service** and a **`"network"` capability**; `activate` wires the
web service; and `call_service` dispatches the service method.

The service it provides is a web endpoint. In this repo a "web service" is a `call_service` method —
the HTTP request is *encoded as a service call payload* and the response is the encoded reply. That
is the mechanism that actually exists. The illustrative block below shows the shape; whether you
parse HTTP over a socket inside the guest or accept the request as an encoded value over a
`call_service`, the service boundary is the same.

> **illustrative** — this block conveys the intended shape. The `cordis-guest` SDK does not ship an
> HTTP client or a socket helper; you supply the protocol handling. Everything outside the `// REAL`
> comments is the actual API that the guest SDK and the kernel provide.

```rust
use cordis_guest::host::{
    self, CallContext, EventId, EventMode, EventReply, KernelError, ServiceId,
};
use cordis_guest::plugin::{Guest, PluginDescriptor};
use std::cell::RefCell;

const WEB_ABI: [u8; 32] = [0x57; 32];       // REAL: you choose/host agrees the hash
const SERVE_METHOD: u32 = 1;                // REAL: method id — see chapter 3
const LISTENER_ID: u64 = 7;

thread_local! {
    static REGISTRATION: RefCell<Option<host::Registration>> = const { RefCell::new(None) };
    static PORT: RefCell<u16> = const { RefCell::new(8080) };
}

struct HttpServer;

#[derive(serde::Serialize, serde::Deserialize)]   // REAL: serde is available via cordis-guest
struct ServeRequest {
    path: String,
    method: String,
}
#[derive(serde::Serialize, serde::Deserialize)]
struct ServeResponse {
    body: String,
    status: u16,
}

fn web_service() -> ServiceId {
    ServiceId {
        name: "example.web".into(),
        abi_hash: WEB_ABI.to_vec(),
    }
}

impl Guest for HttpServer {
    fn descriptor() -> PluginDescriptor {
        PluginDescriptor {
            name: "example.wasm-http-server".into(),
            version: env!("CARGO_PKG_VERSION").into(),
            wit_version: cordis_guest::KERNEL_ABI.into(),
            inject: Vec::new(),
            provide: vec![web_service()],
            config_schema: /* see (b) */ br#"{...}"#.to_vec(),
            capabilities: vec!["network".into()],     // REAL: must declare network
        }
    }

    fn activate(context: CallContext, config: Vec<u8>) -> Result<(), KernelError> {
        // REAL: parse config bytes (e.g. the port) with serde_json here.
        let registration = host::provide_service(context, &web_service())?;
        REGISTRATION.with(|slot| *slot.borrow_mut() = Some(registration));
        // ILLUSTRATIVE: open a TCP listener over wasi:sockets, or register the service so the
        // host routes requests to it. See the note below — this is where you supply a real socket
        // accept loop (wasi:sockets/tcp.tcp-socket, start-listen, accept) or a host-provided
        // HTTP dispatch. Nothing in cordis-guest-rs does this for you.
        Ok(())
    }

    fn deactivate(_context: CallContext) -> Result<(), KernelError> {
        REGISTRATION.with(|slot| slot.borrow_mut().take());
        Ok(())
    }

    fn call_service(
        _context: CallContext,
        service: ServiceId,
        method: u32,
        payload: Vec<u8>,
    ) -> Result<Vec<u8>, KernelError> {
        if service.name != web_service().name || method != SERVE_METHOD {
            return Err(KernelError::InvalidArgument("unknown web service method".into()));
        }
        let request: ServeRequest = cordis_guest::decode(&payload)?;
        // ILLUSTRATIVE: route `request.path`/`request.method` and build a response.
        let response = ServeResponse {
            status: 200,
            body: format!("hello over {}", request.path),
        };
        cordis_guest::encode(&response)
    }

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
}

cordis_guest::export_plugin!(HttpServer);
```

The genuinely real parts are: the descriptor fields, `host::provide_service` and the `Registration`
thread-local, the `call_service` match on name+method, the `encode`/`decode` round trip, and
`export_plugin!`. The genuinely **supplied-by-you** parts are: computing/agreeing `WEB_ABI` and
`SERVE_METHOD`; parsing the config bytes; the socket accept loop or HTTP dispatch; and any HTTP
parsing. That is not a gap you filled wrong — it is the exact state of the guest SDK, and the
counter provider is the proof that this is the intended shape.

### Where the HTTP surface is skeletal

Read `crates/cordis-wasm/src/runtime.rs` and `capability.rs` to see the boundary for yourself. Three
grep checks determine it:

- `capability_for_wasi_import` returns `Some("network")` for both `wasi:sockets/` and `wasi:http/`.
  So the *policy* treats HTTP as network.
- `build_linker` calls `wasmtime_wasi::p2::add_to_linker_async` only. That adds sockets, but the
  `p2` module has no `bindings::http::...::add_to_linker`. So `wasi:http` is not satisfiable.
- `WasiCapabilities::build` builds a `WasiCtxBuilder`, calls `preopened_dir` per preopen, and nothing
  else. It never enables TCP/UDP/name-lookup nor `inherit_network`, so sockets are denied at the
  transport layer.

So the honest summary of "what the host currently supports" for a web server:

| Layer | `wasi:sockets` (TCP) | `wasi:http` |
|---|---|---|
| Capability check | maps to `network` | maps to `network` |
| Linker provides it | **yes** | **no** |
| Transport uses enabled by default | **no** (allow_tcp is off) | n/a |
| Cordis default policy allows | **no** | **no** |

## (b) The `config_schema` accepting port/root

The config our web server needs is a `port` and maybe a `root` directory. A matching schema:

```json
{
  "type": "object",
  "properties": {
    "port": { "type": "integer", "minimum": 1, "maximum": 65535 },
    "root": { "type": "string" }
  },
  "additionalProperties": false,
  "required": ["port"]
}
```

As bytes in the descriptor:

```rust
config_schema: br#"
{
  "type": "object",
  "properties": {
    "port": { "type": "integer", "minimum": 1, "maximum": 65535 },
    "root": { "type": "string" }
  },
  "additionalProperties": false,
  "required": ["port"]
}"#.to_vec(),
```

`port` is **required**; `root` is **optional** but, once present, must be a string. `additionalProperties:
false` rejects any key you did not declare. If an entry supplies `{"port": 8080, "root":
"./public"}`, the host validates it against this schema before activation, then hands the bytes
of `{"port":8080,"root":"./public"}` to your `activate`. Your guest then parses them to set the
port. If an entry supplies `{"por": 8080}` the entry fails to start with an `InvalidConfig` error —
the typo is caught before the component ever activates.

## (c) Adding the entry to `cordis.json`

The declarative config looks like the counter example's, with one addition — the `isolate` mapping
for the service the web plugin provides, and (for a consumer that will call it) the same mapping:

```json
{
  "entries": [
    {
      "id": "web-server",
      "component": "file:../../target/wasm32-wasip2/debug/wasm_http_server.wasm",
      "config": { "port": 8080, "root": "./public" },
      "isolate": { "example.web": "web" }
    }
  ]
}
```

If you add a consumer that calls `example.web`, it must carry `"isolate": { "example.web": "web" }`
too, exactly as the counter consumer does — otherwise it resolves the service in the default realm
and never sees the provider. `config` here is non-empty, which is why the schema in (b) is not the
strict empty-object schema: this plugin genuinely wants a port.

## (d) Building the component

Build the guest against the WASIp2 target:

```sh
cargo build --target wasm32-wasip2 -p wasm-http-server
```

This produces `target/wasm32-wasip2/debug/wasm_http_server.wasm`. Because the crate is in the
workspace, `cargo build -p wasm-http-server` resolves it and the `cdylib` crate-type gives you the
component. Note: if you added the crate to the workspace but not to `xtask`, you skip the
`build-guests` convenience and build it directly, as above. (Add it to `xtask`'s package list if you
want `build-guests` to pick it up too.)

The `wasm32-wasip2` target must be installed:

```sh
rustup target add wasm32-wasip2
```

The artifact at `target/wasm32-wasip2/debug/` is what the `file:` path in (c) refts.

## (e) Running `cordis check` / `cordis run`

With the current CLI this is where you hit the wall, and it is worth knowing exactly what the wall
is.

```sh
cargo run -p cordis-cli -- check examples/wasm-app/cordis.json
```

For a guest that declares `"network"` in `capabilities`, `check` fails with:

```
cordis: driver failed: component capability `network` is denied
```

That is `WasmHostError::CapabilityDenied` surfaced through the preflight driver. It is not a bug in
your guest — it is `ArtifactPolicy::default()` (used by the CLI) denying the capability. To make
`check` pass you must use an `ArtifactPolicy` whose `allowed_capabilities` contains `"network"`,
which means either:

- an **embedding host** that builds the application directly (not through the CLI), constructing an
  `ArtifactPolicy` with `Capability::new("network")` and a `WasiCapabilities` that enables the
  needed socket uses; or
- an **extension to the CLI** to thread a policy from config or a flag.

Neither exists in the repo today. There is no CLI flag to allow a capability. So the truthful
statement is: **you can write this guest and build it; you cannot run it with the shipped
`cordis` CLI until the host grants `network`.** That is a real limitation of the current
implementation, and the whole reason chapter 5 spent a page on `ArtifactPolicy`.

If you do build a host that grants it, the host code is roughly:

```rust
// ILLUSTRATIVE — host side you must supply. Not in the CLI.
use cordis_wasm::{ArtifactPolicy, WasiCapabilities, WasmApplication};
use cordis_core::Capability;
use std::collections::BTreeSet;

let policy = ArtifactPolicy {
    kernel_abi: "0.1".into(),
    allowed_capabilities: BTreeSet::from([Capability::new("network")]),
    wasi: {
        // Wasmtime's WasiCtxBuilder must also enable TCP for sockets to actually work.
        // See WasiCapabilities::build in crates/cordis-wasm/src/capability.rs.
        WasiCapabilities::deny_all()
    },
};
```

Note the `wasi` component is where you would also need to teach `WasiCapabilities` (or replace the
`WasiCtxBuilder` call it wraps) to call `allow_tcp(true)` and `inherit_network()` — otherwise, even
with `network` allowed at the policy layer, the socket operations return `PermissionDenied`. This is
the second denial layer from Fact 2 and it is easy to miss.

Assuming the policy granted it and the sockets were enabled, `run` would then print the usual
`running N fibers across M artifacts; press Ctrl-C to stop` and serve until you press Ctrl-C, with the
web fiber in `Active` state.

## What is real vs what you supply — a checklist

| Aspect | Real in the repo | You supply |
|---|---|---|
| Guest `Guest` trait, descriptor, `export_plugin!` | Yes | Fill it in |
| `provide_service` / `call_service` wire-up | Yes | Match name + method id |
| `encode` / `decode` MessagePack boundary | Yes | Your request/response types |
| `config_schema` + host validation | Yes | The schema JSON |
| `"network"` capability gate | Yes | Declare it; get host to allow it |
| TCP listener accept loop | **No** — skeletal | Implement over `wasi:sockets` |
| HTTP/1.1 parsing | **No** — skeletal | Implement or link a parser |
| `wasi:http` handler | **No** | Host-side extension |
| Policy enabling `network` | **No** (CLI defaults deny) | Host-side configuration |

The password is honesty: the interesting, working part is the kernel service boundary; the
web-server part is where the guest SDK stops and you take over. That is the current state of the
repo, and it is worth stating rather than papering over.

## Try breaking it

- Remove `"network"` from `capabilities` but keep a socket import in the guest. `check` now gives a
  different error — `WASI import ... requires undeclared capability network` from
  `validate_wasi_imports`, not `CapabilityDenied`. The import check and the declaration check are
  separate and both must pass.
- Change `config` to `{"port": "8080"}` (a string). The host rejects it with `InvalidConfig` at the
  schema path `port` — because `port` is declared `type: integer`.
- Remove `additionalProperties: false` and add a bogus key. It now passes silently — the strictness
  was the guardrail.

Next: [Events and views](07-events-and-views.md) — a plugin that reacts to an event and surfaces the
result where one can see it.
