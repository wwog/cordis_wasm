# Public API review: pre-0.1.0

## Decisions retained

- Wasmtime types remain outside `cordis-core`; dynamic components cross object-safe Core traits.
- `restart_fiber` retries `Failed`, while `reload_fiber` explicitly reloads `Active`.
- Loader declares state and delegates effects to `EntryDriver`; `WasmEntryDriver` is the concrete
  dynamic runtime adapter.
- Registration and collection APIs remove by stable identity and make cleanup idempotent where
  Supervisor ordering can already have removed visibility.
- `cordis-cli` is a binary package; command implementation details are not public library API.

## Blocking issues before 0.1.0

1. Decide whether `KernelHost::register` should retain `Option<RealmId>` or use separate typed
   provider/listener registration methods before freezing the trait.
2. Complete Timer `Stream`, throttle, and debounce APIs and settle their cancellation error types.
3. Add a `tracing-subscriber` Layer or document that `Logger` is deliberately a parallel service;
   the current dual emission must not become an accidental permanent API.
4. Define built-in component registration instead of returning an unsupported error for every
   `builtin:` Entry.
5. Decide how `cordis run` watches and transactionally reconciles configuration file changes;
   the current watcher covers already-tracked artifacts.
6. Replace free-form CLI parsing with a stable command contract and add command-level tests.
7. Run Miri/Loom where supported, dependency advisory/license checks, package dry-runs, and the
   cross-platform CI matrix.
8. Establish benchmark baselines before accepting optimization-specific public knobs.

The workspace is therefore functionally runnable but not release-approved. The version remains
the development target; publication is blocked until this list and the release checklist are
closed.
