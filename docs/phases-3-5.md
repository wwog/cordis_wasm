# Phase 3–5 implementation notes

## Design calibration

The implementation keeps Wasmtime out of `cordis-core`. Core now owns the object-safe
`ComponentFactory`, `ComponentInstance`, and `KernelHost` boundary; native adapters,
the Wasmtime host, Loader, and HMR all depend on that boundary. This corrects the older
plan wording that implied the Wasmtime adapter itself should own Fiber lifecycle state.
Fiber identity, effect ownership, dependency resolution, and forced cleanup remain
authoritative in Core.

`RuntimeHandle::mount_dynamic` is the lifecycle bridge: it creates/configures an ordinary
Supervisor Fiber, installs the runtime-neutral executor, commits dependencies at load, and
serializes instance calls against activate/deactivate. Each load epoch owns a fresh EffectSet;
failed construction or activation disposes it before the Fiber settles as `Failed`. Active
replacement uses the explicit `reload_fiber` unload/load path, while `restart_fiber` retains
its narrower meaning of retrying a failed Fiber.

The Kernel ABI has one source at `wit/kernel.wit`. Both host and guest bindings are
generated from it. Service methods are stable `u32` IDs plus MessagePack payloads and
ABI hashes; events carry their dispatch mode and an optional host-owned waterfall
continuation token.

## Phase 3: Wasmtime and guest SDK

- Every component instance has a separate async Store, WASIp2 context, ResourceTable,
  registration table, task group, fuel budget, epoch deadline, and memory/resource limits.
- WASIp2 starts with no inherited filesystem, environment, stdio, or network authority.
  The virtualized monotonic clock and closed/eaten CLI streams are baseline runtime imports.
  Filesystem, sockets/HTTP, random, and wall-clock imports must be declared by the embedded
  descriptor and allowed by host policy. Explicit preopens reject absolute guest paths and
  parent traversal. This is the implementable Wasmtime 48 boundary; the earlier plan's blanket
  claim that a `WasiCtx` can omit every clock/RNG implementation was incorrect because WASI
  itself requires implementations for those interfaces.
- Descriptor inspection happens before activation and checks Kernel ABI, requested
  capabilities, service ABI hash length, and JSON Schema syntax.
- Host imports validate Fiber/effect context and payload size before routing through
  `KernelHost`. Registration handles create a Core effect before success is returned.
  Guest resource drop cleans the individual effect; Fiber teardown disposes the complete
  EffectSet even when the guest leaked handles.
- The Wasmtime spike proves same-Store async reentry is technically possible, but the
  production dynamic handle deliberately rejects guest -> host -> same-Fiber cycles with
  `ReentrantCall` instead of risking an instance-lock deadlock. WIT reserves `next-token`
  for a future host-owned one-shot/expiry continuation mechanism; controlled cross-boundary
  waterfall reentry is not advertised yet. An in-flight guest future must not be dropped:
  cooperative cancellation or Store destruction is required.
- `cordis-guest` provides generated bindings, MessagePack helpers, and `export_plugin!`.
  `cargo run -p xtask -- build-guests` builds and mounts the provider/consumer through the
  real Supervisor lifecycle for end-to-end verification against `wasm32-wasip2` artifacts.

## Phase 4: Loader and Include

`EntryTree` is the declaration source of truth. It computes keyed diffs and sends only
the required child-first stops, updates, and parent-first starts to an `EntryDriver`.
Group disablement is inherited. Local managed realms are keyed by Entry ID and therefore
survive moves; global realms are keyed by service plus user label. Intercepts and managed
realms are inherited and only affected descendants receive an update.

Component references must explicitly use `builtin:` or `file:`. Dynamic config is checked
against Draft 2020-12 schemas before driver calls. Include accepts JSON and YAML entry
arrays, applies ordered merge/replace/remove/insert patches by stable ID, detects target-ID
mismatches, detects read-only files, and writes through a synced sibling temporary file
followed by rename.

YAML reserves `!expr` for a Rhai expression. Evaluation receives only a JSON snapshot as
`ctx`, accepts a single expression, has operation/depth/container limits, and disables
evaluation, module import/export, functions, loops, and exception syntax. No filesystem,
network, process, or host object is registered.

## Phase 5: WASM HMR

`HmrWatcher` uses `notify-debouncer-mini` and deduplicates paths in each batch. The manager
hashes artifact bytes together with the ABI/capability/WASI policy, resource budgets,
runtime version, OS, and architecture. Its compiled factory cache is bounded LRU with
hit/miss/eviction metrics; serialized Wasmtime artifacts are never deserialized unsafely.
`FiberReloadRuntime` binds Loader Entry IDs to `DynamicFiber` handles, so transactional
replace/restore operations execute the same Supervisor unload/load lifecycle as dependency
changes rather than swapping an out-of-band instance pointer.

Reload is transactional at the batch boundary:

1. Read and compile every changed artifact.
2. Instantiate descriptor-only candidates and validate WIT ABI/capabilities.
3. If any preflight fails, leave every active Entry untouched.
4. Replace affected Entries in stable ID order.
5. If an apply fails, restore the failed Entry and all earlier Entries in reverse order.
6. Publish new current hashes only after the whole batch succeeds; report rollback failure
   explicitly without touching unrelated Entries.
