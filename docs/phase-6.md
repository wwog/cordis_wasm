# Phase 6 implementation notes

## Runtime composition and CLI

`WasmEntryDriver` lives in `cordis-wasm`, the lowest layer that already owns Wasmtime
and HMR and can depend on `cordis-loader` without introducing a dependency cycle. Core
remains unaware of artifacts and Entry configuration.

For every active Entry the driver:

1. reads and preflights the Component descriptor;
2. validates config before activation;
3. maps default, local, or labelled global realms to stable runtime Realm IDs;
4. mounts a `DynamicFiber` below the application root;
5. binds its service/event route before waiting for activation;
6. tracks the artifact and Fiber in the transactional HMR manager.

Provider registration receives its resolved realm directly from `InstanceHost`. This
avoids an activation race where a provider could become visible before its Entry route
was known. Registration cleanup treats an already-withdrawn provider as successful because
Supervisor retirement removes provider visibility before guest deactivation by design.

`cordis check` parses Include documents, resolves the Entry tree, compiles descriptors,
checks ABI/capabilities, and validates JSON Schema without activation. `cordis run` adds
Supervisor activation, artifact watching, transactional HMR, Ctrl-C cleanup, and final
shutdown. `cordis inspect` waits for lifecycle quiescence before printing the Fiber graph.

## Logger, Timer, and tracked collections

`cordis-logger` owns target-level filtering, a fixed-capacity record buffer, and effect-owned
exporters. `ConsoleExporter` is outside Core; the CLI installs it for the lifetime of `run`.
Guest logs are recorded with their Fiber ID and are also emitted through `tracing`.

`cordis-timer` provides effect-owned timeout and interval tasks plus cancellation-aware sleep.
Disposal aborts and joins spawned tasks. Timer tests use Tokio's paused clock, so they do not
depend on wall-clock timing.

Core's `TrackedList` and `TrackedMap` allocate a unique registration identity for every insert.
Their disposers remove by that identity, so equal values or equal keys owned by different
effects cannot remove each other accidentally.
