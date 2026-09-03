# 5. Configuration and the sandbox

A plugin decides what it does from two inputs it never owns: the **config** object in `cordis.json`,
and the **sandbox** the host grants it. This chapter walks how config bytes reach `activate`, what
the JSON Schema enforces, and — most importantly for chapter 6 — how `ArtifactPolicy`,
`WasiCapabilities`, and `WasmLimits` gate what a guest is actually allowed to do.

## How config bytes reach `activate`

The path from `cordis.json` to your guest's `activate` is one serialization step plus a validation
pause:

1. **The loader reads the entry.** In `WasmEntryDriver::start_entry`, the entry's `config` (a
   `serde_json::Value`) is handed to `mount_dynamic` alongside the factory.

2. **It is validated against the schema.** `WasmApplication::reconcile` → `EntryTree::reconcile` →
   `validate_config` (loader.rs) runs the config through the *component's* declared schema:

   ```rust
   fn validate_config(entry: &ResolvedEntry, factory: &dyn ComponentFactory) -> Result<(), LoaderError> {
       let schema = serde_json::to_value(&factory.descriptor().config_schema).map_err(...)?;
       let validator = jsonschema::draft202012::new(&schema).map_err(...)?;
       if let Err(error) = validator.validate(&entry.spec.config) {
           return Err(LoaderError::InvalidConfig { entry: ..., path: ..., message: ... });
       }
       Ok(())
   }
   ```

   If the config doesn't match, the entry fails to *start* — the error is loud, and in `check` a
   bad config is caught during preflight without ever activating the component.

3. **The config is serialized to bytes for the guest.** In `WasmComponentInstance::activate`:
   ```rust
   let payload = serde_json::to_vec(&config)?;
   ```
   The JSON object is turned back into raw bytes and passed as the `config: list<u8>` argument of the
   WIT `activate` export.

4. **Your guest receives them.** `activate(context: CallContext, config: Vec<u8>)`. The config is the
   exact bytes from step 3. So a config entry `{"port": 8080}` becomes the bytes of the JSON
   `{"port":8080}`, and your plugin parses them with `serde_json::from_slice` (or whatever you like).
   The guest SDK does **not** pre-decode config for you — it hands you the raw bytes.

The key insight: config is a **JSON value on the host side** and a **byte blob on the guest side**.
The JSON Schema is the host-side gate; the guest must decode the bytes itself.

## The `config_schema`

The schema is a JSON Schema (Draft 2020-12), supplied as JSON bytes in the descriptor. On the host it
is parsed and converted to a `schemars::Schema` in `descriptor_from_wit`:

```rust
let config_value = serde_json::from_slice::<Value>(&descriptor.config_schema).map_err(...)?;
let config_schema = Schema::try_from(config_value).map_err(...)?;
```

So a *strict* schema looks like the counter example's:

```rust
config_schema: br#"{"type":"object","additionalProperties":false}"#.to_vec(),
```

And a schema that accepts a real port/root looks like:

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

Let a `cordis.json` entry then be:

```json
{ "id": "web", "component": "file:...", "config": { "port": 8080, "root": "./public" } }
```

Here `port` is **required** and `root` is optional; `additionalProperties: false` rejects typos like
`"porr"`.

Two conventions to notice:

- **`additionalProperties: false` is the guardrail.** It turns an unknown field into a validation
  error instead of a silent ignore. This is the default recommendation for a plugin's config — a
  strict schema catches mistakes at load time, not at runtime.
- **The guest parses the bytes itself.** Even with a schema, your `activate` must turn the raw bytes
  into a typed value. The schema guarantees the host *accepts* the config; it does not give the
  guest a decoded value.

If you omit `config_schema` entirely you cannot — the field is required by the WIT record. The
practical "no config" idiom is the strict empty-object schema above.

## `ArtifactPolicy` — the capability gate

The host's `ArtifactPolicy` (runtime.rs) is the set of capabilities a guest may use.

```rust
pub struct ArtifactPolicy {
    pub kernel_abi: String,
    pub allowed_capabilities: BTreeSet<Capability>,
    pub wasi: WasiCapabilities,
}

impl Default for ArtifactPolicy {
    fn default() -> Self {
        Self {
            kernel_abi: "0.1".to_owned(),
            allowed_capabilities: BTreeSet::new(),   // nothing allowed
            wasi: WasiCapabilities::deny_all(),
        }
    }
}
```

The default denies **every** capability. So a plugin that wants network access — the whole point of
chapter 6 — needs the host to build an `ArtifactPolicy` with `"network"` in `allowed_capabilities`.
The policy is consulted in three places during `WasmComponentFactory::from_bytes`:

1. **`validate_descriptor`** — each capability the guest *declares* in its descriptor
   `capabilities` list must be present in `allowed_capabilities`, or you get
   `WasmHostError::CapabilityDenied`.
2. **`validate_wasi_imports`** — each WASI import the component actually needs must be in the
   descriptor's declared `capabilities` *and* in `allowed_capabilities`. This is what closes the
   hole where a guest compiles against a WASI interface without declaring it.
3. **`ArtifactHash`** (hmr.rs) — the hash over the artifact includes the policy, so an artifact that
   needs a different capability set than the running policy gets a different cache key and is
   recompiled.

The capability string is a coarse name, not a fine-grained permission. `capability_for_wasi_import`
maps a WASI interface prefix to exactly one capability:

| WASI import prefix | Capability |
|---|---|
| `wasi:io/`, `wasi:cli/`, `wasi:clocks/monotonic-clock` | *(none — always allowed)* |
| `wasi:filesystem/` | `filesystem` |
| `wasi:sockets/` **or** `wasi:http/` | `network` |
| `wasi:random/` | `random` |
| `wasi:clocks/wall-clock` | `clock:wall` |
| any other `wasi:` prefix | the prefix itself |

The `wasi:sockets/` **and** `wasi:http/` both mapping to `network` is the load-bearing fact for
chapter 6: a guest that uses either the socket or the HTTP interface must declare `"network"` in its
`capabilities`, and the host must allow `"network"`.

Note that the CLI always uses `ArtifactPolicy::default()` — `crates/cordis-cli/src/main.rs` passes
it to `check_entries` and `WasmApplication::new`. There is no CLI flag to grant a capability today.
So a network-capable guest **cannot** be run with the shipped CLI as-is; it needs an embedding host
that constructs an `ArtifactPolicy` with `"network"` allowed, or a change to the CLI to thread a
policy. Chapter 6 states this plainly and shows both the guest side and the host side you'd need.

## `WasiCapabilities` — the WASI sandbox

`ArtifactPolicy::wasi` is a `WasiCapabilities`, which is *only* about preopened filesystem
directories today (capability.rs):

```rust
pub struct WasiPreopen {
    pub host_path: PathBuf,
    pub guest_path: String,
    pub writable: bool,
}

pub struct WasiCapabilities {
    preopens: Vec<WasiPreopen>,
}

impl WasiCapabilities {
    pub fn deny_all() -> Self { Self::default() }
    pub fn with_preopen(mut self, preopen: WasiPreopen) -> Self { ... }
}
```

The default `deny_all` grants **no** preopens — not even a bare root. A guest that wants to read its
own directory must be given a preopen explicitly, and that preopen must satisfy two constraints:

- The `guest_path` must be **relative** and may not contain `..` (validated by `validate_guest_path`
  in capability.rs). The intent is a sandboxed path that cannot escape.
- The `host_path` is canonicalized before being passed to Wasmtime. A non-existent host path is an
  error.

This is deliberately coarse. There is no per-file read/write ACL — a preopen is all-or-nothing over a
directory. `WasiCapabilities::build` turns the preopens into a `WasiState` (a `WasiCtx` plus a
`ResourceTable`) that each guest store is built with. The `wasi:` capability name above gates whether
the guest may *import* the interface; the preopens gate whether, having imported it, it has anywhere
to go.

## `WasmLimits` — the resource budget

Wasmtime enforces per-store resource limits from `WasmLimits` (lib.rs):

| Field | Default | What it bounds |
|---|---|---|
| `fuel_per_call` | 10_000_000 | Fuel consumed per guest call before `Trap::OutOfFuel`. |
| `epoch_deadline_ticks` | 1 | Epoch ticks before `Trap::Interrupt`. |
| `max_memory_bytes` | 64 MiB | Guest linear memory growth cap. |
| `max_table_elements` | 10_000 | Table elements. |
| `max_instances` | 32 | Core instances. |
| `max_tables` | 32 | Tables. |
| `max_memories` | 32 | Memories. |
| `max_registrations` | 10_000 | Host-tracked registrations per store. |
| `max_payload_bytes` | 1 MiB | Max size of a service/event payload crossing the boundary. |

Two of these are worth calling out because they trip people up:

- **`fuel_per_call` + `epoch_deadline_ticks`** are re-armed on **every** call in `prepare_call`
  (lib.rs). They are per-call budgets, not cumulative — an infinite loop in a guest traps with
  `OutOfFuel` or `Interrupt`, but a long-running *legitimate* guest is not starved across calls. The
  wat findings in `docs/sundry/wasmtime-findings.md` document this: you must reset fuel and the
  epoch deadline before each call, because Wasmtime does not carry a previous call's leftover budget.
- **`max_payload_bytes`** gates both directions. The host validates outbound payloads in
  `validate_payload` and inbound replies in `validate_payload_limit`. Exceeding it returns
  `PayloadLimitExceeded`, which surfaces to the guest as a `kernel-error`.

These limits are the runtime's second line of defense (the capability policy is the first): even a
guest that is *allowed* to import a WASI interface cannot exhaust memory, leak registrations, or
spin forever.

## The `capabilities` field, reunited

Put it all together. The guest's descriptor declares what it *wants*:

```rust
capabilities: vec!["network".into()],
```

The host policy declares what it *allows*:

```rust
let policy = ArtifactPolicy {
    kernel_abi: "0.1".into(),
    allowed_capabilities: BTreeSet::from([Capability::new("network")]),
    wasi: WasiCapabilities::deny_all(),
};
```

For the component to load, **both** must line up, and the WIT/WASI import split must be closed on
both sides:

- the descriptor's `capabilities` contains `"network"` → passes `validate_descriptor`;
- `"network"` is in `allowed_capabilities` → passes `validate_descriptor`;
- the component's `wasi:http`/`wasi:sockets` imports map to `"network"` → passes
  `validate_wasi_imports`.

If any single one fails, you get a `CapabilityDenied` error at load time. Chapter 6 walks the full
"guest wants HTTP" case, including why a plugin that needs network access needs exactly this
policy, and what the CLI's current default means in practice.

Next: [Writing a web server plugin](06-writing-a-web-server-plugin.md) — the main exercise: a plugin
that provides an HTTP service, and the real network-capability boundary.
