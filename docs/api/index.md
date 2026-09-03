# Cordis (Rust) — API Reference

Cordis is a Rust plugin runtime built on a **revertible-effect** model: every effect a component
performs is paired with an inverse that the runtime applies, LIFO, when the owning fiber unloads.
It adds **reactive dependency injection** — a component activates only when the services it
declares are actually provided — and it uses the **Wasmtime Component Model** as its only dynamic
plugin mechanism. The semantics are specified in the paper `2608.25512v1.txt` and mapped to this
implementation in [semantics.md](../semantics.md).

This reference documents the public API surface of each crate. Every page lists the public items
with their signatures, a short statement of what each does, and the `# Errors` / `# Panics`
behavior documented on the item in the source.

## Crate layout

| Crate | Role |
|---|---|
| `cordis` | Facade. Re-exports `cordis_core::*`, the six proc macros, and `serde`/`schemars`. This is the crate you depend on for native components. |
| `cordis-core` | All runtime semantics: context, effects, fibers, services, events, the supervisor, native components, and the dynamic host bridge. Zero Wasmtime dependency. |
| `cordis-macros` | The six procedural macros (`service`, `event`, `component`, `component_impl`, `inject`, `apply`). |
| `cordis-guest` | Guest SDK for a WebAssembly plugin: generated kernel bindings plus the `Guest` trait you implement. |
| `cordis-wasm` | Host integration for Wasmtime: the engine, limits, factory, loader driver, HMR manager, and the kernel runtime. |
| `cordis-loader` | Declarative entry trees, config includes, and transactional reconciliation. |
| `cordis-cli` | The `cordis` command: `check`, `run`, `inspect`, `build-component`. |
| `cordis-timer` | Effect-owned timers (`timeout`, `interval`, `debounce`, `throttle`, ...) cancelled by fiber cleanup. |
| `cordis-logger` | Structured logging with bounded history and effect-owned exporters. |

## API pages

- [context](context.md) — `Context`: `root`, `fiber`, `extend`, `isolate`, `intercept`, `resolve_realm`, `intercept_layers`.
- [macros](macros.md) — the six procedural macros and the ABI-identity model.
- [effect](effect.md) — the effect subsystem: `EffectScope`, `EffectGuard`, `Disposer`, the exactly-once disposal state machine, LIFO recovery, `spawn_stream`.
- [fiber](fiber.md) — `FiberMachine`, `FiberState`, `DesiredState`, the transition machine and the inertia rule.
- [fiber-machine](fiber-machine.md) — the deeper state-machine invariant: the Fibonacci validation test, transition coalescing, chaining.
- [service](service.md) — `ServiceId`, `ServiceKey`, `ServiceSpec`, `ServiceClient`, `ServiceDispatcher`, the payload codec.
- [event](event.md) — `EventId`, `EventSpec`, `EventMode`, `AsyncEvent`, `BailEvent`, `WaterfallEvent`, `EventTarget`, `Next`, `ControlFlow`.
- [native-component](native-component.md) — the native authoring path: `Component`, `ComponentContext`, `ComponentDefinition`, `NoDependencies`, `config_schema`.
- [dynamic](dynamic.md) — the dynamic host bridge: `ComponentFactory`, `ComponentInstance`, `InstanceHost`, `KernelHost`, `DynamicFiber`.
- [supervisor](supervisor.md) — the single-writer actor: `Runtime`, `RuntimeHandle`, snapshots, command surface, the unload guard, cycle detection.
- [wasm](wasm.md) — `cordis-wasm`: `WasmEngine`, `WasmLimits`, `WasmComponentFactory`, `ArtifactPolicy`, the kernel WIT world.
- [config](config.md) — the `config.{json,yaml}` file syntax: entry fields, `isolate`/`intercept`, component refs, JSON-Schema-validated `config`, includes and patches, and the YAML `!expr` Rhai dynamic configuration.
- [loader](loader.md) — `cordis-loader`: `EntrySpec`, `EntryId`, `ComponentRef`, `EntryTree::reconcile`, `IncludeDocument`.
- [wasm-driver](wasm-driver.md) — `cordis-wasm::loader`: `WasmApplication`, `WasmEntryDriver`, `BuiltinRegistry`, preflight.
- [hmr](hmr.md) — `cordis-wasm::hmr`: `HmrManager`, `HmrWatcher`, `ArtifactCache`, transactional rollback.
- [guest](guest.md) — `cordis-guest`: generated bindings, `KERNEL_ABI`, `encode`/`decode`, `export_plugin!`, the `Guest` trait.
- [cli](cli.md) — `cordis-cli` subcommands and the HMR run loop.
- [native-timer](native-timer.md) — `cordis-timer`.
- [logging](logging.md) — `cordis-logger`.

## Conventions used throughout

- **ABI identity** is a `name` plus a 32-byte BLAKE3 hash over the canonical signature. Native and
  WebAssembly components use the same `ServiceId`/`EventId`, and a provider only satisfies a
  consumer when the full identity — name **and** hash — matches.
- **Anything `#[doc(hidden)]`** in the source is marked as such here. It is real, exported API but
  is intended for macro-generated code, not for direct use.
- **Errors** are reported as `CordisError` (core) or the crate-specific error enum (`LoaderError`,
  `WasmHostError`, `HmrError`, `TimerError`).
