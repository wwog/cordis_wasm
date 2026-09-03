# 热模块替换（`cordis-wasm::hmr`）

`cordis-wasm::hmr` 是事务性热重载管理器。当被跟踪的 `.wasm` artifact 在磁盘上发生变化时，它会重新编译并对每个候选做 *preflight*（预检），然后在一个事务中替换受影响的 fibers：全有或全无（all-or-nothing），失败时回滚到先前的 artifacts。HMR watcher（`HmrWatcher`）是文件系统一侧；`HmrManager` 是事务核心。

**有意保留的"无模块 import 图"差异。** 论文的 HMR（§5.2.2）分类的是*模块 import 图*（`get_imports`，Webpack/Vite 的 accept boundary）。这里的动态代码是单个 Wasmtime Component，没有 JS 模块图：HMR 简化为"artifact 内容 hash 变化 → 替换该 fiber"。语义核心得以保留——它仍然是 fiber 替换加上事务性回滚，而且仍然不需要开发者注记的 accept boundary（论文的中心论断），因为 fiber 已经界定了组件的 effects。这一点记录在 [semantics.zh.md](../semantics.zh.md) §5.2.2。

## `ArtifactHash`

```rust
#[derive(Clone, Copy, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct ArtifactHash([u8; 32]);
impl ArtifactHash {
    pub fn from_bytes(bytes: &[u8], policy: &ArtifactPolicy, limits: &WasmLimits) -> Self;
    pub fn as_bytes(&self) -> &[u8; 32];
}
```

已编译 artifact 的内容身份。它不只对字节做哈希，还对一切会改变 artifact 行为的东西做哈希：crate 版本、目标 arch/OS、kernel ABI、允许的 capabilities、WASI preopens，以及每一个 `WasmLimits` 预算字段。因此，在同一策略下用不同机器、或在不同策略下重新编译同样的字节，会产生不同的 hash，并正确地触发一次重载。

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

一个已编译的 artifact 加上它可选的 factory。当 artifact 是作为占位符产出时（有些测试直接构造一个），`factory` 为 `None`；但对于真正的编译，它是 `WasmComponentFactory`。

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

一个以 `ArtifactHash` 为 key 的**最近最少使用（LRU）**有界 cache。`get` 计入一次命中/未命中并 touch 该条目；`insert` 在超出容量时驱逐最旧的 `capacity` 个条目。容量是 `max(capacity, 1)`。loader 中默认的 HMR cache 容量是 `32`（`HMR_CACHE_CAPACITY`）。

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

HMR manager 用来应用替换的接口。`replace` 交换某个 entry 的 artifact；manager 把任何错误都当作完整回滚的触发器。`restore` 重新应用先前的 artifact。

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

把事务性 HMR manager 连接到 Supervisor 拥有的 dynamic fibers。`bind` 把一个 loader Entry 与它活跃的 `DynamicFiber` 和 config 关联起来；`replace`（以及委托给 `replace` 的 `restore`）调用 `fiber.replace(factory, config)` 在 fiber 上执行一次 unload/load 交换。

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

事务核心。

### `track`

预检并开始跟踪一个已激活 Entry 的 artifact。先编译这些字节，然后记录 path → entry 映射与当前 artifact。返回编译后的 artifact。

**错误** —— 对非法 component、descriptor 或 policy 的预检错误。

### `reload_paths`

重载所有跟踪的 artifact 出现在 `changed_paths` 中的 Entries。它分两个阶段工作：

1. **Preflight（预检）** —— 对每条变化的跟踪路径，读取并 `compile` 候选。任何读取或编译失败都会产生 `preflight_failure_report`，并且**不触碰任何实例**。这是在激活任何东西*之前*的候选编译 / descriptor / WIT / capability 检查。
2. **Commit（提交）** —— `commit_candidates` 替换每条变化的 entry。任何替换失败都会触发对失败的 entry *以及所有先前已提交的 entries* 按逆序回滚。

### `commit_candidates` 与事务性回滚

对每个 hash 与当前值不同的候选，它调用 `runtime.replace`。一旦失败，它**逆序**遍历 `attempted` 列表（`attempted.into_iter().rev()`），对每个调用 `restore`——因此一个失败的 HMR batch 会恢复所有先前的替换，而不仅是失败的那一个。回滚失败会报告为 `ReloadStatus::RollbackFailed`。`current` map 只对完全成功的 entries 更新；回滚路径会恢复先前的 hash。

这就是 §5.2.2 的事务性替换（算法 10，backup/restore）。测试 `apply_failure_rolls_back_failed_and_prior_entries_in_reverse_order` 以 `b` 上的一个失败驱动 `a`、`b`，并断言事件顺序 `["replace:a", "replace:b", "restore:b", "restore:a"]`。

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

一次重载 batch 的结果。当有任何失败并已回滚时，`committed` 为 false。`EntryReload` 记录每个 entry 的 previous 与 candidate hash 及其 status。未变化的 hash 会被去重为 `ReloadStatus::Unchanged`，且不调用 runtime。

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

以路径级防抖过滤监视 artifact 的父目录。它（非递归地）监视目标路径的*父目录*，对事件做防抖，并只报告受影响的*目标*——因此父目录中的一次 rename、或对某个 sibling 的写入，都会被过滤到精确的已跟踪路径。

- `new` 在每个目标的父目录上建立 watcher。**错误** —— watcher 后端或路径注册错误。
- `next_timeout` 最多等待 `timeout` 时间以获取一组防抖后的变化路径。**错误** —— watcher 后端或 channel 错误（断开的 channel 是错误，而非 `Ok(None)`）。

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

## 错误 / 行为说明

- 一个**写了一半或损坏的 component** 会在预检阶段失败，早于任何 apply。测试：`half_written_or_bad_component_fails_before_apply`。
- **未变化的 hash** 会被去重，且不触碰 runtime。
- **回滚失败**会报告为 `ReloadStatus::RollbackFailed`，并且不会触碰未受影响的 entry。测试：`rollback_failure_is_reported_without_touching_unaffected_entry`。
- cache 是**有界 LRU** 并报告命中/驱逐指标。测试：`cache_is_bounded_lru_and_reports_hits`。
