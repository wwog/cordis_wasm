//! Transactional hot reload for Cordis WebAssembly artifacts.

use crate::{ArtifactPolicy, WasmComponentFactory, WasmEngine, WasmLimits};
use cordis_core::{ComponentFactory, DynamicFiber};
use notify_debouncer_mini::notify::{RecommendedWatcher, RecursiveMode, Watcher};
use notify_debouncer_mini::{DebounceEventResult, Debouncer, new_debouncer};
use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::future::Future;
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::Arc;
use std::sync::RwLock;
use std::sync::mpsc::{Receiver, RecvTimeoutError};
use std::time::Duration;

pub type HmrFuture<'a, T> = Pin<Box<dyn Future<Output = Result<T, HmrError>> + Send + 'a>>;

#[derive(Clone, Copy, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct ArtifactHash([u8; 32]);

impl ArtifactHash {
    pub fn from_bytes(bytes: &[u8], policy: &ArtifactPolicy, limits: &WasmLimits) -> Self {
        let mut hasher = blake3::Hasher::new();
        hasher.update(bytes);
        hasher.update(env!("CARGO_PKG_VERSION").as_bytes());
        hasher.update(std::env::consts::ARCH.as_bytes());
        hasher.update(std::env::consts::OS.as_bytes());
        hasher.update(policy.kernel_abi.as_bytes());
        for capability in &policy.allowed_capabilities {
            hasher.update(capability.as_str().as_bytes());
            hasher.update(&[0]);
        }
        for preopen in policy.wasi.preopens() {
            hasher.update(preopen.host_path.as_os_str().as_encoded_bytes());
            hasher.update(preopen.guest_path.as_bytes());
            hasher.update(&[u8::from(preopen.writable)]);
        }
        hasher.update(&limits.fuel_per_call.to_le_bytes());
        hasher.update(&limits.epoch_deadline_ticks.to_le_bytes());
        hasher.update(&limits.max_memory_bytes.to_le_bytes());
        hasher.update(&limits.max_table_elements.to_le_bytes());
        hasher.update(&limits.max_instances.to_le_bytes());
        hasher.update(&limits.max_tables.to_le_bytes());
        hasher.update(&limits.max_memories.to_le_bytes());
        hasher.update(&limits.max_registrations.to_le_bytes());
        hasher.update(&limits.max_payload_bytes.to_le_bytes());
        Self(*hasher.finalize().as_bytes())
    }

    pub fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }
}

impl std::fmt::Debug for ArtifactHash {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "ArtifactHash({self})")
    }
}

impl std::fmt::Display for ArtifactHash {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        for byte in self.0 {
            write!(formatter, "{byte:02x}")?;
        }
        Ok(())
    }
}

#[derive(Clone, Debug)]
pub struct CompiledArtifact {
    hash: ArtifactHash,
    factory: Option<Arc<WasmComponentFactory>>,
}

impl CompiledArtifact {
    pub fn hash(&self) -> ArtifactHash {
        self.hash
    }

    pub fn factory(&self) -> Option<&WasmComponentFactory> {
        self.factory.as_deref()
    }

    pub fn factory_arc(&self) -> Option<Arc<WasmComponentFactory>> {
        self.factory.clone()
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct CacheMetrics {
    pub hits: u64,
    pub misses: u64,
    pub evictions: u64,
}

#[derive(Debug)]
pub struct ArtifactCache {
    capacity: usize,
    artifacts: BTreeMap<ArtifactHash, Arc<CompiledArtifact>>,
    lru: VecDeque<ArtifactHash>,
    metrics: CacheMetrics,
}

impl ArtifactCache {
    pub fn new(capacity: usize) -> Self {
        Self {
            capacity: capacity.max(1),
            artifacts: BTreeMap::new(),
            lru: VecDeque::new(),
            metrics: CacheMetrics::default(),
        }
    }

    pub fn get(&mut self, hash: ArtifactHash) -> Option<Arc<CompiledArtifact>> {
        let artifact = self.artifacts.get(&hash).cloned();
        if artifact.is_some() {
            self.metrics.hits += 1;
            self.touch(hash);
        } else {
            self.metrics.misses += 1;
        }
        artifact
    }

    pub fn insert(&mut self, artifact: Arc<CompiledArtifact>) {
        let hash = artifact.hash;
        self.artifacts.insert(hash, artifact);
        self.touch(hash);
        while self.artifacts.len() > self.capacity {
            if let Some(oldest) = self.lru.pop_front()
                && self.artifacts.remove(&oldest).is_some()
            {
                self.metrics.evictions += 1;
            }
        }
    }

    pub fn metrics(&self) -> CacheMetrics {
        self.metrics
    }

    fn touch(&mut self, hash: ArtifactHash) {
        self.lru.retain(|candidate| *candidate != hash);
        self.lru.push_back(hash);
    }
}

pub trait ReloadRuntime: Send + Sync + 'static {
    /// Replaces one Entry. Errors may occur after partial activation, so the
    /// manager will also pass this Entry to `restore` during rollback.
    fn replace<'a>(&'a self, entry: &'a str, candidate: Arc<CompiledArtifact>)
    -> HmrFuture<'a, ()>;

    fn restore<'a>(&'a self, entry: &'a str, previous: Arc<CompiledArtifact>) -> HmrFuture<'a, ()>;
}

/// Connects the transactional HMR manager to Supervisor-owned dynamic fibers.
#[derive(Debug, Default)]
pub struct FiberReloadRuntime {
    entries: RwLock<BTreeMap<String, (DynamicFiber, serde_json::Value)>>,
}

impl FiberReloadRuntime {
    /// Associates one loader Entry with its active dynamic fiber and config.
    pub fn bind(&self, entry: impl Into<String>, fiber: DynamicFiber, config: serde_json::Value) {
        self.entries
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(entry.into(), (fiber, config));
    }

    /// Removes an Entry association and returns its dynamic fiber.
    pub fn unbind(&self, entry: &str) -> Option<DynamicFiber> {
        self.entries
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .remove(entry)
            .map(|(fiber, _)| fiber)
    }

    fn replacement(
        &self,
        entry: &str,
        artifact: &CompiledArtifact,
    ) -> Result<(DynamicFiber, Arc<dyn ComponentFactory>, serde_json::Value), HmrError> {
        let factory = artifact
            .factory_arc()
            .ok_or_else(|| HmrError::Apply(format!("Entry `{entry}` has no compiled factory")))?;
        let (fiber, config) = self
            .entries
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(entry)
            .cloned()
            .ok_or_else(|| HmrError::Apply(format!("Entry `{entry}` is not bound")))?;
        Ok((fiber, factory, config))
    }
}

impl ReloadRuntime for FiberReloadRuntime {
    fn replace<'a>(
        &'a self,
        entry: &'a str,
        candidate: Arc<CompiledArtifact>,
    ) -> HmrFuture<'a, ()> {
        Box::pin(async move {
            let (fiber, factory, config) = self.replacement(entry, &candidate)?;
            fiber
                .replace(factory, config)
                .await
                .map_err(|error| HmrError::Apply(error.to_string()))
        })
    }

    fn restore<'a>(&'a self, entry: &'a str, previous: Arc<CompiledArtifact>) -> HmrFuture<'a, ()> {
        self.replace(entry, previous)
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ReloadStatus {
    Updated,
    Unchanged,
    Failed(String),
    RolledBack,
    RollbackFailed(String),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct EntryReload {
    pub entry: String,
    pub previous: ArtifactHash,
    pub candidate: Option<ArtifactHash>,
    pub status: ReloadStatus,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct ReloadReport {
    pub committed: bool,
    pub entries: Vec<EntryReload>,
}

#[derive(Debug)]
pub struct HmrManager<R> {
    engine: WasmEngine,
    limits: WasmLimits,
    policy: ArtifactPolicy,
    runtime: Arc<R>,
    cache: ArtifactCache,
    paths: BTreeMap<PathBuf, BTreeSet<String>>,
    entry_paths: BTreeMap<String, PathBuf>,
    current: BTreeMap<String, Arc<CompiledArtifact>>,
}

impl<R: ReloadRuntime> HmrManager<R> {
    pub fn new(
        engine: WasmEngine,
        limits: WasmLimits,
        policy: ArtifactPolicy,
        runtime: Arc<R>,
        cache_capacity: usize,
    ) -> Self {
        Self {
            engine,
            limits,
            policy,
            runtime,
            cache: ArtifactCache::new(cache_capacity),
            paths: BTreeMap::new(),
            entry_paths: BTreeMap::new(),
            current: BTreeMap::new(),
        }
    }

    pub fn cache(&self) -> &ArtifactCache {
        &self.cache
    }

    /// Returns the canonical paths of all currently tracked artifacts.
    pub fn tracked_paths(&self) -> impl Iterator<Item = &PathBuf> {
        self.paths.keys()
    }

    /// Stops tracking one Entry without changing its running instance.
    pub fn untrack(&mut self, entry: &str) -> Option<Arc<CompiledArtifact>> {
        if let Some(path) = self.entry_paths.remove(entry)
            && let Some(entries) = self.paths.get_mut(&path)
        {
            entries.remove(entry);
            if entries.is_empty() {
                self.paths.remove(&path);
            }
        }
        self.current.remove(entry)
    }

    /// Preflights and starts tracking an already-active Entry artifact.
    ///
    /// # Errors
    ///
    /// Returns a preflight error for an invalid component, descriptor, or policy.
    pub async fn track(
        &mut self,
        entry: impl Into<String>,
        path: impl Into<PathBuf>,
        bytes: &[u8],
    ) -> Result<Arc<CompiledArtifact>, HmrError> {
        let entry = entry.into();
        let path = canonical_or_original(path.into());
        let artifact = self.compile(bytes).await?;
        if let Some(old_path) = self.entry_paths.insert(entry.clone(), path.clone())
            && let Some(entries) = self.paths.get_mut(&old_path)
        {
            entries.remove(&entry);
            if entries.is_empty() {
                self.paths.remove(&old_path);
            }
        }
        self.paths.entry(path).or_default().insert(entry.clone());
        self.current.insert(entry, artifact.clone());
        Ok(artifact)
    }

    /// Reloads all Entries whose tracked artifacts occur in `changed_paths`.
    /// Candidate compilation, descriptor/WIT checks, and capability checks all
    /// finish before the first active instance is replaced.
    pub async fn reload_paths(
        &mut self,
        changed_paths: impl IntoIterator<Item = PathBuf>,
    ) -> ReloadReport {
        let changed = changed_paths
            .into_iter()
            .map(canonical_or_original)
            .collect::<BTreeSet<_>>();
        let mut bytes_by_path = BTreeMap::new();
        let mut failures = BTreeMap::new();
        for path in &changed {
            if !self.paths.contains_key(path) {
                continue;
            }
            match std::fs::read(path) {
                Ok(bytes) => {
                    bytes_by_path.insert(path.clone(), bytes);
                }
                Err(error) => {
                    failures.insert(path.clone(), error.to_string());
                }
            }
        }

        let mut prepared_by_path = BTreeMap::new();
        for (path, bytes) in bytes_by_path {
            match self.compile(&bytes).await {
                Ok(artifact) => {
                    prepared_by_path.insert(path, artifact);
                }
                Err(error) => {
                    failures.insert(path, error.to_string());
                }
            }
        }
        if !failures.is_empty() {
            return self.preflight_failure_report(&changed, &failures);
        }

        let mut candidates = BTreeMap::new();
        for (path, artifact) in prepared_by_path {
            if let Some(entries) = self.paths.get(&path) {
                for entry in entries {
                    candidates.insert(entry.clone(), artifact.clone());
                }
            }
        }
        self.commit_candidates(candidates).await
    }

    async fn compile(&mut self, bytes: &[u8]) -> Result<Arc<CompiledArtifact>, HmrError> {
        let hash = ArtifactHash::from_bytes(bytes, &self.policy, &self.limits);
        if let Some(artifact) = self.cache.get(hash) {
            return Ok(artifact);
        }
        let factory = WasmComponentFactory::from_bytes(
            self.engine.clone(),
            bytes,
            self.limits.clone(),
            self.policy.clone(),
        )
        .await
        .map_err(|error| HmrError::Preflight(error.to_string()))?;
        let artifact = Arc::new(CompiledArtifact {
            hash,
            factory: Some(Arc::new(factory)),
        });
        self.cache.insert(artifact.clone());
        Ok(artifact)
    }

    fn preflight_failure_report(
        &self,
        changed: &BTreeSet<PathBuf>,
        failures: &BTreeMap<PathBuf, String>,
    ) -> ReloadReport {
        let mut entries = Vec::new();
        for path in changed {
            for entry in self.paths.get(path).into_iter().flatten() {
                let previous = self.current[entry].hash;
                let status = failures.get(path).map_or(ReloadStatus::Unchanged, |error| {
                    ReloadStatus::Failed(error.clone())
                });
                entries.push(EntryReload {
                    entry: entry.clone(),
                    previous,
                    candidate: None,
                    status,
                });
            }
        }
        ReloadReport {
            committed: false,
            entries,
        }
    }

    async fn commit_candidates(
        &mut self,
        candidates: BTreeMap<String, Arc<CompiledArtifact>>,
    ) -> ReloadReport {
        let mut report = ReloadReport::default();
        let mut changes = Vec::new();
        for (entry, candidate) in candidates {
            let Some(previous) = self.current.get(&entry).cloned() else {
                continue;
            };
            if previous.hash == candidate.hash {
                report.entries.push(EntryReload {
                    entry,
                    previous: previous.hash,
                    candidate: Some(candidate.hash),
                    status: ReloadStatus::Unchanged,
                });
            } else {
                changes.push((entry, previous, candidate));
            }
        }
        if changes.is_empty() {
            report.committed = true;
            return report;
        }

        let mut attempted = Vec::new();
        for (entry, previous, candidate) in &changes {
            attempted.push((entry.clone(), previous.clone(), candidate.clone()));
            if let Err(error) = self.runtime.replace(entry, candidate.clone()).await {
                report.entries.push(EntryReload {
                    entry: entry.clone(),
                    previous: previous.hash,
                    candidate: Some(candidate.hash),
                    status: ReloadStatus::Failed(error.to_string()),
                });
                for (rollback_entry, rollback_previous, rollback_candidate) in
                    attempted.into_iter().rev()
                {
                    let status = match self
                        .runtime
                        .restore(&rollback_entry, rollback_previous.clone())
                        .await
                    {
                        Ok(()) => ReloadStatus::RolledBack,
                        Err(error) => ReloadStatus::RollbackFailed(error.to_string()),
                    };
                    if rollback_entry == *entry {
                        if matches!(status, ReloadStatus::RollbackFailed(_)) {
                            report
                                .entries
                                .last_mut()
                                .expect("failed entry exists")
                                .status = status;
                        }
                    } else {
                        report.entries.push(EntryReload {
                            entry: rollback_entry,
                            previous: rollback_previous.hash,
                            candidate: Some(rollback_candidate.hash),
                            status,
                        });
                    }
                }
                report
                    .entries
                    .sort_by(|left, right| left.entry.cmp(&right.entry));
                return report;
            }
        }

        for (entry, previous, candidate) in changes {
            self.current.insert(entry.clone(), candidate.clone());
            report.entries.push(EntryReload {
                entry,
                previous: previous.hash,
                candidate: Some(candidate.hash),
                status: ReloadStatus::Updated,
            });
        }
        report
            .entries
            .sort_by(|left, right| left.entry.cmp(&right.entry));
        report.committed = true;
        report
    }
}

pub struct HmrWatcher {
    debouncer: Debouncer<RecommendedWatcher>,
    receiver: Receiver<Result<Vec<PathBuf>, HmrError>>,
}

impl std::fmt::Debug for HmrWatcher {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.debug_struct("HmrWatcher").finish_non_exhaustive()
    }
}

impl HmrWatcher {
    /// Watches artifact parent directories with path-level debounce filtering.
    ///
    /// # Errors
    ///
    /// Returns a watcher backend or path registration error.
    pub fn new(
        paths: impl IntoIterator<Item = PathBuf>,
        debounce: Duration,
    ) -> Result<Self, HmrError> {
        let targets = paths
            .into_iter()
            .map(canonical_or_original)
            .collect::<BTreeSet<_>>();
        let callback_targets = targets.clone();
        let (sender, receiver) = std::sync::mpsc::channel();
        let mut debouncer = new_debouncer(debounce, move |result: DebounceEventResult| {
            let value = result
                .map(|events| {
                    affected_targets(
                        events.into_iter().map(|event| event.path),
                        &callback_targets,
                    )
                })
                .map_err(|error| HmrError::Watch(error.to_string()));
            if match &value {
                Ok(paths) => !paths.is_empty(),
                Err(_) => true,
            } {
                let _ = sender.send(value);
            }
        })
        .map_err(|error| HmrError::Watch(error.to_string()))?;
        let watch_roots = targets
            .iter()
            .filter_map(|path| path.parent().map(PathBuf::from))
            .collect::<BTreeSet<_>>();
        for path in watch_roots {
            debouncer
                .watcher()
                .watch(&path, RecursiveMode::NonRecursive)
                .map_err(|error| HmrError::Watch(error.to_string()))?;
        }
        Ok(Self {
            debouncer,
            receiver,
        })
    }

    /// Waits for one debounced set of changed artifact paths.
    ///
    /// # Errors
    ///
    /// Returns a watcher backend or channel error.
    pub fn next_timeout(&self, timeout: Duration) -> Result<Option<Vec<PathBuf>>, HmrError> {
        match self.receiver.recv_timeout(timeout) {
            Ok(result) => result.map(Some),
            Err(RecvTimeoutError::Timeout) => Ok(None),
            Err(RecvTimeoutError::Disconnected) => {
                Err(HmrError::Watch("watcher disconnected".into()))
            }
        }
    }

    pub fn watcher(&mut self) -> &mut dyn Watcher {
        self.debouncer.watcher()
    }
}

#[derive(Clone, Debug, thiserror::Error, Eq, PartialEq)]
pub enum HmrError {
    #[error("artifact preflight failed: {0}")]
    Preflight(String),
    #[error("reload apply failed: {0}")]
    Apply(String),
    #[error("artifact watcher failed: {0}")]
    Watch(String),
}

fn canonical_or_original(path: PathBuf) -> PathBuf {
    path.canonicalize().unwrap_or_else(|_| {
        path.parent()
            .and_then(|parent| parent.canonicalize().ok())
            .and_then(|parent| path.file_name().map(|name| parent.join(name)))
            .unwrap_or(path)
    })
}

fn affected_targets(
    event_paths: impl IntoIterator<Item = PathBuf>,
    targets: &BTreeSet<PathBuf>,
) -> Vec<PathBuf> {
    event_paths
        .into_iter()
        .flat_map(|path| {
            let event_path = canonical_or_original(path);
            targets
                .iter()
                .filter(move |target| {
                    **target == event_path || target.parent() == Some(&event_path)
                })
                .cloned()
        })
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    #[derive(Default)]
    struct Runtime {
        events: Mutex<Vec<String>>,
        fail_replace: Mutex<Option<String>>,
        fail_restore: Mutex<Option<String>>,
    }

    impl ReloadRuntime for Runtime {
        fn replace<'a>(&'a self, entry: &'a str, _: Arc<CompiledArtifact>) -> HmrFuture<'a, ()> {
            Box::pin(async move {
                self.events.lock().unwrap().push(format!("replace:{entry}"));
                if self.fail_replace.lock().unwrap().as_deref() == Some(entry) {
                    Err(HmrError::Apply(format!("{entry} trapped")))
                } else {
                    Ok(())
                }
            })
        }

        fn restore<'a>(&'a self, entry: &'a str, _: Arc<CompiledArtifact>) -> HmrFuture<'a, ()> {
            Box::pin(async move {
                self.events.lock().unwrap().push(format!("restore:{entry}"));
                if self.fail_restore.lock().unwrap().as_deref() == Some(entry) {
                    Err(HmrError::Apply(format!("{entry} restore failed")))
                } else {
                    Ok(())
                }
            })
        }
    }

    fn artifact(byte: u8) -> Arc<CompiledArtifact> {
        Arc::new(CompiledArtifact {
            hash: ArtifactHash([byte; 32]),
            factory: None,
        })
    }

    fn manager(runtime: Arc<Runtime>) -> HmrManager<Runtime> {
        HmrManager::new(
            WasmEngine::new().unwrap(),
            WasmLimits::default(),
            ArtifactPolicy::default(),
            runtime,
            2,
        )
    }

    #[tokio::test]
    async fn apply_failure_rolls_back_failed_and_prior_entries_in_reverse_order() {
        let runtime = Arc::new(Runtime::default());
        *runtime.fail_replace.lock().unwrap() = Some("b".into());
        let mut manager = manager(runtime.clone());
        manager.current.insert("a".into(), artifact(1));
        manager.current.insert("b".into(), artifact(1));
        let report = manager
            .commit_candidates(BTreeMap::from([
                ("a".into(), artifact(2)),
                ("b".into(), artifact(2)),
            ]))
            .await;
        assert!(!report.committed);
        assert_eq!(
            *runtime.events.lock().unwrap(),
            ["replace:a", "replace:b", "restore:b", "restore:a"]
        );
        assert_eq!(manager.current["a"].hash, ArtifactHash([1; 32]));
    }

    #[tokio::test]
    async fn rollback_failure_is_reported_without_touching_unaffected_entry() {
        let runtime = Arc::new(Runtime::default());
        *runtime.fail_replace.lock().unwrap() = Some("b".into());
        *runtime.fail_restore.lock().unwrap() = Some("a".into());
        let mut manager = manager(runtime.clone());
        manager.current.insert("a".into(), artifact(1));
        manager.current.insert("b".into(), artifact(1));
        manager.current.insert("z".into(), artifact(9));
        let report = manager
            .commit_candidates(BTreeMap::from([
                ("a".into(), artifact(2)),
                ("b".into(), artifact(2)),
            ]))
            .await;
        assert!(
            report
                .entries
                .iter()
                .any(|entry| matches!(entry.status, ReloadStatus::RollbackFailed(_)))
        );
        assert!(
            !runtime
                .events
                .lock()
                .unwrap()
                .iter()
                .any(|event| event.contains('z'))
        );
    }

    #[test]
    fn cache_is_bounded_lru_and_reports_hits() {
        let mut cache = ArtifactCache::new(2);
        cache.insert(artifact(1));
        cache.insert(artifact(2));
        assert!(cache.get(ArtifactHash([1; 32])).is_some());
        cache.insert(artifact(3));
        assert!(cache.get(ArtifactHash([2; 32])).is_none());
        assert_eq!(cache.metrics().hits, 1);
        assert_eq!(cache.metrics().evictions, 1);
    }

    #[tokio::test]
    async fn unchanged_hash_is_deduplicated_without_runtime_call() {
        let runtime = Arc::new(Runtime::default());
        let mut manager = manager(runtime.clone());
        manager.current.insert("a".into(), artifact(1));
        let report = manager
            .commit_candidates(BTreeMap::from([("a".into(), artifact(1))]))
            .await;
        assert!(report.committed);
        assert!(matches!(report.entries[0].status, ReloadStatus::Unchanged));
        assert!(runtime.events.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn half_written_or_bad_component_fails_before_apply() {
        let runtime = Arc::new(Runtime::default());
        let mut manager = manager(runtime.clone());
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("plugin.wasm");
        let canonical = canonical_or_original(path.clone());
        manager
            .paths
            .insert(canonical.clone(), BTreeSet::from(["a".into()]));
        manager.entry_paths.insert("a".into(), canonical);
        manager.current.insert("a".into(), artifact(1));
        std::fs::write(&path, b"half-written").unwrap();
        let report = manager.reload_paths([path]).await;
        assert!(!report.committed);
        assert!(matches!(report.entries[0].status, ReloadStatus::Failed(_)));
        assert!(runtime.events.lock().unwrap().is_empty());
    }

    #[test]
    fn watcher_targets_parent_rename_events_and_deduplicates_paths() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("plugin.wasm");
        std::fs::write(&path, b"first").unwrap();
        let target = canonical_or_original(path.clone());
        let parent = target.parent().unwrap().to_path_buf();
        let paths = affected_targets(
            [path.clone(), path, parent],
            &BTreeSet::from([target.clone()]),
        );
        assert_eq!(paths, [target]);
        HmrWatcher::new(
            [directory.path().join("plugin.wasm")],
            Duration::from_millis(30),
        )
        .unwrap();
    }
}
