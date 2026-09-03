# Reliability scope for 0.1.0

## Deterministic and fault coverage

- Fiber lifecycle: 128 seeds × 256 generated operations cover desired-state changes, success and
  failure completions, stale generations, restart, reload, and irreversible retirement.
- Activation and teardown: dynamic component tests inject activation failure, cleanup failure,
  panic, dependency withdrawal, partial provider registration, and unload failure.
- Routing: service ABI, missing/undeclared/inactive dependency, payload limits, reentrant calls,
  event listener lookup, and guest traps are covered at native and Wasmtime boundaries.
- Resource limits: fuel, epoch interruption, linear memory, registration count, capability policy,
  and explicit/forced registration cleanup are covered.
- Loader/HMR: invalid schema, bad/half-written component, watcher rename events, unchanged hashes,
  apply failure, reverse rollback, rollback failure, and multi-operation configuration rollback are
  injected deterministically.

## Miri and Loom boundary

Miri is not shipped for the pinned stable 1.98.0 toolchain. CI therefore installs nightly plus the
Miri component and runs the Core effect and tracked-collection suites, following Miri's supported
toolchain model. Both suites pass locally under Miri: 11 effect tests and 2 tracked-collection
tests. Tokio features are selected per crate, so the Core target does not initialize unsupported
signal or I/O drivers merely because the CLI needs them.

Loom is not applicable to the 0.1.0 implementation after API review: Cordis contains no custom
lock-free algorithm, hand-written wake protocol, `unsafe` block, or memory-order-dependent state
machine. Coordination is delegated to Tokio channels/Notify/Mutex and `std::sync::Mutex`; the only
atomics allocate monotonic diagnostic IDs with relaxed ordering and do not synchronize data. If a
custom concurrent primitive is introduced later, a Loom model becomes a prerequisite for merging
it.
