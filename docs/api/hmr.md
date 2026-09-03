# Hot Module Replacement (`cordis-wasm::hmr`)

`cordis-wasm::hmr` is the transactional hot-reload manager. When a tracked `.wasm` artifact changes on
disk, it recompiles and *preflights* every candidate, then replaces the affected fibers in a
transaction: all-or-nothing, with rollback to the previous artifacts on failure. The HMR watcher
(`HmrWatcher`) is the filesystem side; `HmrManager` is the transactional core.

**The intentional "no module import graph" difference.** Paper HMR (§5.2.2) classifies a *module
import graph* (`get_imports`, Webpack/Vite acceptance boundaries). Dynamic code here is a single
Wasmtime Component, so there is no JS module graph: HMR reduces to "artifact content hash changed →
replace that fiber." The semantic core survives — it is still fiber replacement plus transactional
rollback, and it still needs no developer-annotated acceptance boundary (the paper's central claim),
because the fiber already bounds the component's effects. This is documented in
[semantics.md](../semantics.md) §5.2.2.

## `ArtifactHash`

```rust
#[derive(Clone, Copy, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct ArtifactHash([u8; 32]);
impl ArtifactHash {
    pub fn from_bytes(bytes: &[u8], policy: &ArtifactPolicy, limits: &WasmLimits) -> Self;
    pub fn as_bytes(&self) -> &[u8; 32];
}
```

The content identity of a compiled artifact. It hashes not only the bytes but everything that would
change the artifact's behavior: the crate version, the target arch/OS, the kernel ABI, the allowed
capabilities, the WASI preopens, and every `WasmLimits` budget field. So recompiling the same bytes
under a different policy, or on a different machine, produces a different hash and correctly triggers
a reload.

## `CompiledArtifact`

```rust
#[derive(Clone, Debug)]
pub struct CompiledArtifact { /* hash, factory: Option<Arc<WasmComponentFactory>> */ }
impl CompiledArtifact {
    pub fn hash(&self) -> ArtifactHash;
    pub fn factory(&self) -> Option<&WasmComponentFactory>;
    pub fn factory_arc(&self) -> Option<Arc<WasmComponentFactory>>;
}
```

A compiled artifact plus its optional factory. `factory` is `None` when an artifact was produced as a
placeholder (some tests construct one directly), but for real compiles it is the `WasmComponentFactory`.

## `ArtifactCache`

```rust
#[derive(Debug)]
pub struct ArtifactCache { /* capacity, artifacts, lru, metrics */ }
impl ArtifactCache {
    pub fn new(capacity: usize) -> Self;
    pub fn get(&mut self, hash: ArtifactHash) -> Option<Arc<CompiledArtifact>>;
    pub fn insert(&mut self, artifact: Arc<CompiledArtifact>);
    pub fn metrics(&self) -> CacheMetrics;
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct CacheMetrics { pub hits: u64, pub misses: u64, pub evictions: u64 }
```

A bounded **least-recently-used** cache keyed by `ArtifactHash`. `get` counts a hit/miss and touches
the entry; `insert` evicts the oldest `capacity` entries when over. Capacity is `max(capacity, 1)`. The
default HMR cache capacity in the loader is `32` (`HMR_CACHE_CAPACITY`).

## `ReloadRuntime`

```rust
pub trait ReloadRuntime: Send + Sync + 'static {
    /// Replaces one Entry. Errors may occur after partial activation, so the
    /// manager will also pass this Entry to `restore` during rollback.
    fn replace<'a>(&'a self, entry: &'a str, candidate: Arc<CompiledArtifact>) -> HmrFuture<'a, ()>;

    /// Re-applies the previous artifact during rollback.
    fn restore<'a>(&'a self, entry: &'a str, previous: Arc<CompiledArtifact>) -> HmrFuture<'a, ()>;
}
```

The interface the HMR manager uses to apply a replacement. `replace` swaps one entry's artifact; the
manager treats any error as a trigger for full rollback. `restore` re-applies the previous artifact.

## `FiberReloadRuntime`

```rust
#[derive(Debug, Default)]
pub struct FiberReloadRuntime { /* entries: RwLock<BTreeMap<String, (DynamicFiber, Value)>> */ }
impl FiberReloadRuntime {
    pub fn bind(&self, entry: impl Into<String>, fiber: DynamicFiber, config: Value);
    pub fn unbind(&self, entry: &str) -> Option<DynamicFiber>;
}
impl ReloadRuntime for FiberReloadRuntime { /* replace, restore */ }
```

Connects the transactional HMR manager to Supervisor-owned dynamic fibers. `bind` associates one
loader Entry with its active `DynamicFiber` and config; `replace` (and `restore`, which delegates to
`replace`) calls `fiber.replace(factory, config)` to perform an unload/load swap on the fiber.

## `HmrManager<R>`

```rust
#[derive(Debug)]
pub struct HmrManager<R> { /* engine, limits, policy, runtime: Arc<R>, cache, paths, entry_paths, current */ }

impl<R: ReloadRuntime> HmrManager<R> {
    pub fn new(engine: WasmEngine, limits: WasmLimits, policy: ArtifactPolicy, runtime: Arc<R>, cache_capacity: usize) -> Self;
    pub fn cache(&self) -> &ArtifactCache;
    pub fn tracked_paths(&self) -> impl Iterator<Item = &PathBuf>;
    pub fn untrack(&mut self, entry: &str) -> Option<Arc<CompiledArtifact>>;
    pub async fn track(&mut self, entry: impl Into<String>, path: impl Into<PathBuf>, bytes: &[u8]) -> Result<Arc<CompiledArtifact>, HmrError>;
    pub async fn reload_paths(&self, changed_paths: impl IntoIterator<Item = PathBuf>) -> ReloadReport;
}
```

The transactional core.

### `track`

Preflights and starts tracking an already-active Entry's artifact. Compiles the bytes, then records
the path → entry mapping and the current artifact. Returns the compiled artifact.

**Errors** — a preflight error for an invalid component, descriptor, or policy.

### `reload_paths`

Reloads all Entries whose tracked artifacts occur in `changed_paths`. It works in two phases:

1. **Preflight** — for every changed tracked path, read and `compile` the candidate. Any read or
   compile failure produces a `preflight_failure_report` and **no instance is touched**. This is the
   candidate-compile / descriptor / WIT / capability check *before* activating anything.
2. **Commit** — `commit_candidates` replaces each changed entry. Any replace failure triggers rollback
   of the failed entry *and all previously committed entries* in reverse order.

### `commit_candidates` and transactional rollback

For each candidate whose hash differs from the current one, it calls `runtime.replace`. On a failure
it iterates the `attempted` list **in reverse** (`attempted.into_iter().rev()`), calling
`restore` on each — so a failed HMR batch restores every prior replacement, not just the failing one.
A rollback failure is reported as `ReloadStatus::RollbackFailed`. The `current` map is only updated
for entries that fully succeeded; the rollback path restores the previous hash.

This is §5.2.2 transactional replace (Algorithm 10, backup/restore). Test:
`apply_failure_rolls_back_failed_and_prior_entries_in_reverse_order` drives `a`, `b` with a failure on
`b` and asserts the event order `["replace:a", "replace:b", "restore:b", "restore:a"]`.

## `ReloadReport` / `EntryReload` / `ReloadStatus`

```rust
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct ReloadReport {
    pub committed: bool,
    pub entries: Vec<EntryReload>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct EntryReload {
    pub entry: String,
    pub previous: ArtifactHash,
    pub candidate: Option<ArtifactHash>,
    pub status: ReloadStatus,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ReloadStatus {
    Updated,
    Unchanged,
    Failed(String),
    RolledBack,
    RollbackFailed(String),
}
```

One reload batch's result. `committed` is false when anything failed and rolled back. `EntryReload`
records each entry's previous and candidate hashes and its status. An unchanged hash is
deduplicated with `ReloadStatus::Unchanged` and no runtime call.

## `HmrWatcher`

```rust
#[derive(Debug)]
pub struct HmrWatcher { /* debouncer: Debouncer<RecommendedWatcher>, receiver */ }
impl HmrWatcher {
    pub fn new(paths: impl IntoIterator<Item = PathBuf>, debounce: Duration) -> Result<Self, HmrError>;
    pub fn next_timeout(&self, timeout: Duration) -> Result<Option<Vec<PathBuf>>, HmrError>;
    pub fn watcher(&mut self) -> &mut dyn Watcher;
}
```

Watches artifact parent directories with path-level debounce filtering. It watches the *parents* of
the target paths (non-recursively), debounces events, and reports only the affected *targets* — so a
rename in the parent directory, or a write to a sibling, is filtered to exactly the tracked paths.

- `new` sets up the watcher on each target's parent directory. **Errors** — watcher backend or path
  registration error.
- `next_timeout` waits up to `timeout` for one debounced set of changed paths. **Errors** — watcher
  backend or channel error (a disconnected channel is an error, not `Ok(None)`).

## `HmrError`

```rust
pub type HmrFuture<'a, T> = Pin<Box<dyn Future<Output = Result<T, HmrError>> + Send + 'a>>;

#[derive(Clone, Debug, thiserror::Error, Eq, PartialEq)]
pub enum HmrError {
    #[error("artifact preflight failed: {0}")] Preflight(String),
    #[error("reload apply failed: {0}")] Apply(String),
    #[error("artifact watcher failed: {0}")] Watch(String),
}
```

## Errors / behavior notes

- A **half-written or bad component** fails in preflight, before any apply. Test:
  `half_written_or_bad_component_fails_before_apply`.
- An **unchanged hash** is deduplicated without touching the runtime.
- A **rollback failure** is reported as `ReloadStatus::RollbackFailed` and does not touch an
  unaffected entry. Test: `rollback_failure_is_reported_without_touching_unaffected_entry`.
- The cache is bounded LRU and reports hit/eviction metrics. Test: `cache_is_bounded_lru_and_reports_hits`.
