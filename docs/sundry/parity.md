# TypeScript behavior parity for 0.1.0

The Rust rewrite preserves Cordis lifecycle semantics, not JavaScript object mechanics or binary
compatibility. This matrix is derived from the reference tests under `cordis-ts/packages/*/tests`.

| Reference behavior | Rust 0.1.0 status | Evidence / deliberate difference |
|---|---|---|
| Effect disposal, manual disposal, LIFO cleanup, async iterator cleanup, failure continuation | Equivalent and stricter | `cordis-core::effect` adds exactly-once concurrent disposal, panic isolation, and aggregated failures. |
| Fiber inertia, failed activation, explicit recovery, dependency-driven reload | Equivalent | `FiberMachine` and Supervisor tests cover coalescing, stale generations, failure recovery, consumer-first teardown, and generated transition sequences. |
| Required/optional service injection and provider replacement | Equivalent | Committed epochs prevent mid-activation dependency drift; provider identity is the owning Fiber rather than JS object identity. |
| Context isolation and shared realm labels | Equivalent | Immutable overlays plus Loader local/global managed realms replace prototype mutation. |
| Event emit, parallel, serial, bail, waterfall, prepend, once/effect ownership | Equivalent | Rust events are typed; waterfall enforces equal input/output types at compile time and one-shot `Next` at runtime. |
| Method-level injection | Equivalent lifecycle, stronger typing | Each annotated method owns a child Fiber and EffectSet; dependencies are generated typed fields rather than decorators/proxies. |
| Loader create/update/move/remove, groups, disable propagation, intercept, self-update | Equivalent | Keyed reconciliation is additionally transactional across a batch. |
| Include JSON/YAML, merge/group/insert patches, expressions, write-back | Equivalent supported subset | Rhai replaces JavaScript evaluation and runs with a deliberately restricted scope. |
| HMR batching, unchanged-file dedupe, apply rollback, dependency reload | Equivalent lifecycle | Dynamic code is only a Wasmtime Component; there is no Node module cache or linked-JavaScript-file graph. |
| Timer timeout, interval iterator, throttle, debounce, disposal | Equivalent intent | Rust exposes effect-owned handles and a `Stream`; disposal ends interval streams cleanly and cancelled sleeps return `TimerError::ContextDisposed`. |
| Logger buffer, target filtering, exporter disposal | Equivalent core | `Logger` is a parallel application service, while Rust ecosystem diagnostics remain in `tracing`; it does not install a recursive subscriber bridge. |
| JS service shadow, Proxy reflection, prototype/associated property injection | Not ported literally | Typed clients, explicit Context, intercept metadata, and macro-generated dependency fields provide the Rust-native boundary. |
| Arbitrary JS plugin values and callable mixins | Intentionally excluded | Native components implement typed traits; dynamic plugins use the versioned Kernel WIT and MessagePack business protocols. |

Any future parity claim must name the observable behavior and its Rust test. Matching a JavaScript
implementation detail without a Rust safety or API benefit is not a compatibility requirement.
