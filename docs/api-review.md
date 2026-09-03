# Public API review: 0.1.0

## Decisions retained

- Wasmtime types remain outside `cordis-core`; dynamic components cross object-safe Core traits.
- `restart_fiber` retries `Failed`, while `reload_fiber` explicitly reloads `Active`.
- Loader declares state and delegates effects to `EntryDriver`; `WasmEntryDriver` is the concrete
  dynamic runtime adapter.
- Registration and collection APIs remove by stable identity and make cleanup idempotent where
  Supervisor ordering can already have removed visibility.
- `cordis-cli` is a binary package; command implementation details are not public library API.
- Runtime IDs remain opaque and are allocated by their owning runtime; they are not user-created
  integers.
- The Kernel WIT stays at package version `0.1.0`; business service/event compatibility continues
  to use independent 32-byte ABI hashes.

## Resolved blocking issues

1. `KernelHost` exposes separate `provide_service(ProviderKey, ...)` and
   `register_listener(...)` methods. Guest-shaped `RegistrationRequest` is resolved inside
   `InstanceHost`; no nullable realm crosses the host trait.
2. Timer exposes an effect-owned `IntervalStream`, `Debouncer<T>`, and `Throttler<T>`. Parent or
   manual disposal ends streams/schedulers; interrupted sleep and post-disposal calls use
   `TimerError::ContextDisposed`, while disposer failures use `TimerError::Cleanup`.
3. `Logger` is deliberately a parallel application service, not a `tracing-subscriber` Layer.
   Runtime adapters may emit outward to both systems, but tracing is never fed back into Logger.
4. `BuiltinRegistry` resolves `builtin:<name>` to an `Arc<dyn ComponentFactory>` for both preflight
   and runtime mounting. Duplicate names fail; built-ins share lifecycle but not artifact HMR.
5. `EntryTree::reconcile` rolls successful operations back in reverse order. `cordis run` watches
   the configuration itself, commits valid trees transactionally, and rebuilds artifact watch
   targets only after commit.
6. The binary parser has a closed typed command set for check/run/inspect/build-component plus
   help/version, with tests for valid and rejected shapes.
7. Reliability scope is recorded in `reliability.md`; nightly Miri and dependency policy jobs are
   configured. Loom is not applicable because no custom synchronization primitive remains.
8. `benchmarks.md` records lifecycle and Context baselines. No optimization-specific public knob
   was accepted for 0.1.0.

## Release-only evidence still required

- The crates.io namespace must be resolved before publication. The current names `cordis`,
  `cordis-core`, `cordis-loader`, `cordis-cli`, and `cordis-timer` already exist and list owners
  other than the repository name (`shigma` or `dshbox-dev`). Confirm publisher access or adopt a
  collision-free package naming scheme before generating the release commit.

  A fallback namespace probe on 2026-09-03 found the following names available. Availability is
  not reservation and must be rechecked immediately before publishing. Package renames can retain
  the existing Rust library and binary names through explicit manifest targets.

  | Current package | Fallback package |
  | --- | --- |
  | `cordis` | `cordis-wasm` |
  | `cordis-core` | `cordis-wasm-core` |
  | `cordis-guest` | `cordis-wasm-guest` |
  | `cordis-loader` | `cordis-wasm-loader` |
  | `cordis-logger` | `cordis-wasm-logger` |
  | `cordis-macros` | `cordis-wasm-macros` |
  | `cordis-timer` | `cordis-wasm-timer` |
  | `cordis-wasm` | `cordis-wasm-host` |
  | `cordis-cli` | `cordis-wasm-cli` |

- The exact release commit must pass Linux/MSRV, macOS, Windows, nightly Miri, cargo-deny, and
  RustSec jobs.
- Leaf crate tarballs verify locally. Every dependent `.crate` archive is also generated and
  compiled from its extracted package contents with `[patch.crates-io]` pointing only to extracted
  Cordis dependency archives. Cargo's own registry-backed verification completes after each
  dependency is published. Release order is: `cordis-macros`, `cordis-core`, `cordis-guest`; then
  loader/logger/timer; then wasm and facade; finally CLI.

No unresolved code-level API blocker remains in this review. Publication still requires the
namespace decision and external evidence above; the workspace version alone is not release
evidence.
