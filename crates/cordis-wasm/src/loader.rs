//! Runtime composition for declarative WebAssembly entries.

use crate::{
    ArtifactPolicy, FiberReloadRuntime, HmrManager, ReloadReport, WasmComponentFactory, WasmEngine,
    WasmLimits,
};
use cordis_core::{
    ComponentFactory, ComponentFuture, Context, CordisError, Disposer, DynamicCall, DynamicFiber,
    EffectScope, EventCall, EventId, EventReply, FiberId, FiberState, KernelHost, ProviderKey,
    RealmId, Runtime, RuntimeHandle, RuntimeSnapshot, ServiceId,
};
use cordis_loader::{
    ComponentRef, EntryDriver, EntryId, EntrySpec, EntryTree, LoaderError, LoaderFuture,
    ManagedRealm, ResolvedEntry,
};
use cordis_logger::{LogLevel, Logger};
use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;
use std::sync::{Arc, Mutex as SyncMutex, RwLock};
use tokio::sync::Mutex;

const HMR_CACHE_CAPACITY: usize = 32;

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
enum RealmKey {
    Default(ServiceId),
    Local(EntryId, String),
    Global(String, String),
}

#[derive(Clone, Debug)]
struct MountedEntry {
    fiber: DynamicFiber,
    hmr_tracked: bool,
}

/// Process-local factories addressable through `builtin:<name>` Entry references.
#[derive(Clone, Default)]
pub struct BuiltinRegistry {
    factories: Arc<RwLock<BTreeMap<String, Arc<dyn ComponentFactory>>>>,
}

impl std::fmt::Debug for BuiltinRegistry {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("BuiltinRegistry")
            .field(
                "names",
                &self
                    .factories
                    .read()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .keys()
                    .collect::<Vec<_>>(),
            )
            .finish()
    }
}

impl BuiltinRegistry {
    /// Registers one process-local component factory.
    ///
    /// # Errors
    ///
    /// Returns an error for an empty or duplicate name.
    pub fn register(
        &self,
        name: impl Into<String>,
        factory: Arc<dyn ComponentFactory>,
    ) -> Result<(), LoaderError> {
        let name = name.into();
        if name.is_empty() {
            return Err(LoaderError::Driver(
                "builtin component name cannot be empty".into(),
            ));
        }
        let mut factories = self
            .factories
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if factories.contains_key(&name) {
            return Err(LoaderError::Driver(format!(
                "builtin component `{name}` is already registered"
            )));
        }
        factories.insert(name, factory);
        Ok(())
    }

    fn get(&self, name: &str) -> Option<Arc<dyn ComponentFactory>> {
        self.factories
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(name)
            .cloned()
    }
}

/// Concrete Kernel router shared by every dynamic Entry in one application.
#[derive(Debug)]
struct RuntimeKernel {
    runtime: RuntimeHandle,
    logger: Logger,
    routes: RwLock<BTreeMap<FiberId, DynamicFiber>>,
    route_changed: tokio::sync::Notify,
    listeners: Arc<RwLock<BTreeMap<(EventId, u64), FiberId>>>,
}

impl RuntimeKernel {
    fn new(runtime: RuntimeHandle) -> Self {
        Self {
            runtime,
            logger: Logger::new(1024),
            routes: RwLock::new(BTreeMap::new()),
            route_changed: tokio::sync::Notify::new(),
            listeners: Arc::new(RwLock::new(BTreeMap::new())),
        }
    }

    fn bind(&self, fiber: DynamicFiber) {
        self.routes
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(fiber.fiber(), fiber);
        self.route_changed.notify_waiters();
    }

    const fn logger(&self) -> &Logger {
        &self.logger
    }

    fn unbind(&self, fiber: FiberId) {
        self.routes
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .remove(&fiber);
        self.listeners
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .retain(|_, owner| *owner != fiber);
        self.route_changed.notify_waiters();
    }

    async fn route(&self, fiber: FiberId) -> Result<DynamicFiber, CordisError> {
        loop {
            let changed = self.route_changed.notified();
            if let Some(route) = self
                .routes
                .read()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .get(&fiber)
                .cloned()
            {
                return Ok(route);
            }
            let snapshot = self.runtime.snapshot().await?;
            let state = snapshot
                .fibers
                .iter()
                .find(|candidate| candidate.id == fiber)
                .map(|candidate| candidate.state);
            if matches!(
                state,
                None | Some(FiberState::Failed | FiberState::Disposed)
            ) {
                return Err(CordisError::ComponentFailed {
                    component: format!("fiber:{fiber}"),
                    message: "provider has no dynamic route".to_owned(),
                });
            }
            changed.await;
        }
    }
}

impl KernelHost for RuntimeKernel {
    fn log(&self, fiber: FiberId, level: &str, message: &str) {
        let level = match level {
            "error" => LogLevel::Error,
            "warn" => LogLevel::Warn,
            "debug" => LogLevel::Debug,
            "trace" => LogLevel::Trace,
            _ => LogLevel::Info,
        };
        self.logger.log(level, "cordis.guest", message, Some(fiber));
        match level {
            LogLevel::Error => tracing::error!(target: "cordis.guest", %fiber, "{message}"),
            LogLevel::Warn => tracing::warn!(target: "cordis.guest", %fiber, "{message}"),
            LogLevel::Debug => tracing::debug!(target: "cordis.guest", %fiber, "{message}"),
            LogLevel::Trace => tracing::trace!(target: "cordis.guest", %fiber, "{message}"),
            LogLevel::Info => tracing::info!(target: "cordis.guest", %fiber, "{message}"),
        }
    }

    fn call_service(&self, fiber: FiberId, call: DynamicCall) -> ComponentFuture<'_, Vec<u8>> {
        Box::pin(async move {
            let committed = self.runtime.commit_dependencies(fiber).await?;
            let provider = committed.lookup(&call.service)?.ok_or_else(|| {
                CordisError::MissingCommittedProvider {
                    service: call.service.clone(),
                }
            })?;
            self.route(provider).await?.call_service(call).await
        })
    }

    fn dispatch_event(&self, _: FiberId, call: EventCall) -> ComponentFuture<'_, EventReply> {
        Box::pin(async move {
            let owner = self
                .listeners
                .read()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .get(&(call.event.clone(), call.listener_id))
                .copied()
                .ok_or_else(|| CordisError::ComponentFailed {
                    component: call.event.to_string(),
                    message: format!("listener {} is not registered", call.listener_id),
                })?;
            self.route(owner).await?.call_event(call).await
        })
    }

    fn provide_service(
        &self,
        fiber: FiberId,
        key: ProviderKey,
        scope: EffectScope,
    ) -> ComponentFuture<'_, ()> {
        Box::pin(async move {
            self.runtime.provide(key.clone(), fiber).await?;
            let runtime = self.runtime.clone();
            let cleanup_key = key.clone();
            if let Err(error) = scope.defer(Disposer::new(move || async move {
                match runtime.withdraw(cleanup_key, fiber).await {
                    Ok(_) | Err(CordisError::ProviderNotFound { .. }) => Ok(()),
                    Err(error) => Err(error),
                }
            })) {
                let _ = self.runtime.withdraw(key, fiber).await;
                return Err(error);
            }
            Ok(())
        })
    }

    fn register_listener(
        &self,
        fiber: FiberId,
        event: EventId,
        listener_id: u64,
        _: cordis_core::EventMode,
        scope: EffectScope,
    ) -> ComponentFuture<'_, ()> {
        Box::pin(async move {
            let key = (event, listener_id);
            let mut listeners = self
                .listeners
                .write()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if listeners.contains_key(&key) {
                return Err(CordisError::ComponentFailed {
                    component: key.0.to_string(),
                    message: format!("listener {listener_id} is already registered"),
                });
            }
            listeners.insert(key.clone(), fiber);
            drop(listeners);
            let listeners = self.listeners.clone();
            scope.defer(Disposer::infallible(move || async move {
                listeners
                    .write()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .remove(&key);
            }))?;
            Ok(())
        })
    }
}

/// Executes Loader Entry operations against Supervisor-owned Wasmtime fibers.
pub struct WasmEntryDriver {
    runtime: RuntimeHandle,
    root: FiberId,
    root_context: Context,
    base_dir: PathBuf,
    builtins: BuiltinRegistry,
    kernel: Arc<RuntimeKernel>,
    reload: Arc<FiberReloadRuntime>,
    hmr: Mutex<HmrManager<FiberReloadRuntime>>,
    realms: Mutex<BTreeMap<RealmKey, RealmId>>,
    entries: Mutex<BTreeMap<EntryId, MountedEntry>>,
}

impl std::fmt::Debug for WasmEntryDriver {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("WasmEntryDriver")
            .field("root", &self.root)
            .field("base_dir", &self.base_dir)
            .finish_non_exhaustive()
    }
}

impl WasmEntryDriver {
    fn new(
        runtime: RuntimeHandle,
        root: (FiberId, Context),
        base_dir: PathBuf,
        builtins: BuiltinRegistry,
        engine: WasmEngine,
        limits: WasmLimits,
        policy: ArtifactPolicy,
    ) -> Arc<Self> {
        let (root, root_context) = root;
        let kernel = Arc::new(RuntimeKernel::new(runtime.clone()));
        let reload = Arc::new(FiberReloadRuntime::default());
        let hmr = HmrManager::new(engine, limits, policy, reload.clone(), HMR_CACHE_CAPACITY);
        Arc::new(Self {
            runtime,
            root,
            root_context,
            base_dir,
            builtins,
            kernel,
            reload,
            hmr: Mutex::new(hmr),
            realms: Mutex::new(BTreeMap::new()),
            entries: Mutex::new(BTreeMap::new()),
        })
    }

    /// Returns the canonical artifact paths currently tracked for HMR.
    pub async fn artifact_paths(&self) -> Vec<PathBuf> {
        self.hmr.lock().await.tracked_paths().cloned().collect()
    }

    pub fn logger(&self) -> &Logger {
        self.kernel.logger()
    }

    /// Applies one transactional artifact change batch.
    pub async fn reload_paths(&self, paths: impl IntoIterator<Item = PathBuf>) -> ReloadReport {
        self.hmr.lock().await.reload_paths(paths).await
    }

    async fn start_entry(&self, entry: &ResolvedEntry) -> Result<(), LoaderError> {
        let started = std::time::Instant::now();
        self.dbg_log(format!("mounting entry `{}`", entry.spec.id));
        let (factory, hmr_tracked) = self.resolve_factory(entry).await?;
        if let Err(error) = validate_config(entry, factory.as_ref()) {
            self.untrack_if(entry.spec.id.as_str(), hmr_tracked).await;
            return Err(error);
        }

        let context = match self.entry_context(entry, factory.as_ref()).await {
            Ok(context) => context,
            Err(error) => {
                self.untrack_if(entry.spec.id.as_str(), hmr_tracked).await;
                return Err(error);
            }
        };
        let parent = {
            let entries = self.entries.lock().await;
            entry
                .parent
                .as_ref()
                .and_then(|parent| entries.get(parent))
                .map(|mounted| mounted.fiber.fiber())
                .or(Some(self.root))
        };
        let mounted = match self
            .runtime
            .mount_dynamic(
                parent,
                Some(&context),
                factory,
                self.kernel.clone(),
                entry.spec.config.clone(),
            )
            .await
        {
            Ok(mounted) => mounted,
            Err(error) => {
                self.untrack_if(entry.spec.id.as_str(), hmr_tracked).await;
                return Err(driver_error(error));
            }
        };

        self.kernel.bind(mounted.clone());
        if hmr_tracked {
            self.reload.bind(
                entry.spec.id.to_string(),
                mounted.clone(),
                entry.spec.config.clone(),
            );
        }
        self.entries.lock().await.insert(
            entry.spec.id.clone(),
            MountedEntry {
                fiber: mounted.clone(),
                hmr_tracked,
            },
        );

        let state = match self.runtime.snapshot().await {
            Ok(snapshot) => snapshot
                .fibers
                .into_iter()
                .find(|fiber| fiber.id == mounted.fiber())
                .map(|fiber| fiber.state),
            Err(error) => {
                self.remove_mount(&entry.spec.id).await;
                let _ = mounted.retire().await;
                return Err(driver_error(error));
            }
        };
        if matches!(state, Some(FiberState::Loading | FiberState::Failed))
            && let Err(error) = mounted.await_active().await
        {
            self.remove_mount(&entry.spec.id).await;
            let _ = mounted.retire().await;
            return Err(driver_error(error));
        }
        self.dbg_log(format!(
            "mounted entry `{}` ({}ms, fiber state {:?})",
            entry.spec.id,
            started.elapsed().as_millis(),
            state,
        ));
        Ok(())
    }

    async fn resolve_factory(
        &self,
        entry: &ResolvedEntry,
    ) -> Result<(Arc<dyn ComponentFactory>, bool), LoaderError> {
        match entry.component.as_ref() {
            Some(ComponentRef::Builtin(name)) => self
                .builtins
                .get(name)
                .map(|factory| (factory, false))
                .ok_or_else(|| {
                    LoaderError::Driver(format!("builtin component `{name}` is not registered"))
                }),
            Some(ComponentRef::File(path)) => {
                let path = self.base_dir.join(path);
                let bytes = std::fs::read(&path).map_err(driver_error)?;
                let compiled_at = std::time::Instant::now();
                let artifact = self
                    .hmr
                    .lock()
                    .await
                    .track(entry.spec.id.to_string(), path, &bytes)
                    .await
                    .map_err(driver_error)?;
                self.dbg_log(format!(
                    "compiled artifact `{}` in {}ms",
                    entry.spec.id,
                    compiled_at.elapsed().as_millis(),
                ));
                let factory = artifact.factory_arc().ok_or_else(|| {
                    LoaderError::Driver("compiled artifact has no factory".into())
                })?;
                Ok((factory, true))
            }
            None => Err(LoaderError::Driver(format!(
                "Entry `{}` has no component",
                entry.spec.id
            ))),
        }
    }

    async fn untrack_if(&self, entry: &str, hmr_tracked: bool) {
        if hmr_tracked {
            self.hmr.lock().await.untrack(entry);
        }
    }

    /// Emits a `cordis.loader` debug log when the logger is configured below
    /// [`LogLevel::Debug`], otherwise a no-op. Internal lifecycle traces are
    /// gated behind `set_level("cordis", LogLevel::Debug)` by the host.
    fn dbg_log(&self, message: impl AsRef<str>) {
        self.kernel
            .logger()
            .log(LogLevel::Debug, "cordis.loader", message.as_ref(), None);
    }

    async fn stop_entry(&self, id: &EntryId) -> Result<(), LoaderError> {
        let mounted = self
            .entries
            .lock()
            .await
            .remove(id)
            .ok_or_else(|| LoaderError::Driver(format!("Entry `{id}` is not mounted")))?;
        let result = mounted.fiber.retire().await.map_err(driver_error);
        self.kernel.unbind(mounted.fiber.fiber());
        if mounted.hmr_tracked {
            self.reload.unbind(id.as_str());
            self.hmr.lock().await.untrack(id.as_str());
        }
        result
    }

    async fn remove_mount(&self, id: &EntryId) {
        if let Some(mounted) = self.entries.lock().await.remove(id) {
            self.kernel.unbind(mounted.fiber.fiber());
            if mounted.hmr_tracked {
                self.reload.unbind(id.as_str());
                self.hmr.lock().await.untrack(id.as_str());
            }
        }
    }

    async fn entry_context(
        &self,
        entry: &ResolvedEntry,
        factory: &dyn ComponentFactory,
    ) -> Result<Context, LoaderError> {
        let descriptor = factory.descriptor();
        let services = descriptor
            .injects
            .iter()
            .map(|inject| inject.service.clone())
            .chain(descriptor.provides.iter().cloned())
            .collect::<BTreeSet<_>>();
        let mut context = self.root_context.clone();
        for service in services {
            let realm = self.realm_for(entry, &service).await?;
            context = context.isolate(service.clone(), realm);
            if let Some(intercept) = entry.intercept.get(service.name()) {
                context = context.intercept(service, intercept.clone());
            }
        }
        Ok(context)
    }

    async fn realm_for(
        &self,
        entry: &ResolvedEntry,
        service: &ServiceId,
    ) -> Result<RealmId, LoaderError> {
        let key = match entry.realms.get(service.name()) {
            Some(ManagedRealm::Local { owner, service }) => {
                RealmKey::Local(owner.clone(), service.clone())
            }
            Some(ManagedRealm::Global { label, service }) => {
                RealmKey::Global(label.clone(), service.clone())
            }
            None => RealmKey::Default(service.clone()),
        };
        let mut realms = self.realms.lock().await;
        if let Some(realm) = realms.get(&key) {
            return Ok(*realm);
        }
        let realm = self.runtime.allocate_realm().await.map_err(driver_error)?;
        realms.insert(key, realm);
        Ok(realm)
    }
}

impl EntryDriver for WasmEntryDriver {
    fn start<'a>(&'a self, entry: &'a ResolvedEntry) -> LoaderFuture<'a, ()> {
        Box::pin(async move { self.start_entry(entry).await })
    }

    fn update<'a>(
        &'a self,
        previous: &'a ResolvedEntry,
        next: &'a ResolvedEntry,
    ) -> LoaderFuture<'a, ()> {
        Box::pin(async move {
            self.stop_entry(&previous.spec.id).await?;
            if let Err(error) = self.start_entry(next).await {
                return match self.start_entry(previous).await {
                    Ok(()) => Err(error),
                    Err(rollback) => Err(LoaderError::Driver(format!(
                        "{error}; rollback failed: {rollback}"
                    ))),
                };
            }
            Ok(())
        })
    }

    fn stop<'a>(&'a self, entry: &'a ResolvedEntry) -> LoaderFuture<'a, ()> {
        Box::pin(async move {
            if let Err(error) = self.stop_entry(&entry.spec.id).await {
                return match self.start_entry(entry).await {
                    Ok(()) => Err(error),
                    Err(rollback) => Err(LoaderError::Driver(format!(
                        "{error}; stop rollback failed: {rollback}"
                    ))),
                };
            }
            Ok(())
        })
    }
}

/// Owns one runnable Loader + Supervisor + Wasmtime application.
pub struct WasmApplication {
    runtime: Runtime,
    root: FiberId,
    tree: EntryTree<WasmEntryDriver>,
    driver: Arc<WasmEntryDriver>,
}

impl std::fmt::Debug for WasmApplication {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("WasmApplication")
            .field("root", &self.root)
            .field("entries", &self.tree.entries().len())
            .finish_non_exhaustive()
    }
}

impl WasmApplication {
    /// Creates an empty application rooted at `base_dir`.
    ///
    /// # Errors
    ///
    /// Returns an engine, Supervisor, or root Fiber creation error.
    pub async fn new(
        base_dir: impl Into<PathBuf>,
        limits: WasmLimits,
        policy: ArtifactPolicy,
    ) -> Result<Self, LoaderError> {
        Self::new_with_builtins(base_dir, limits, policy, BuiltinRegistry::default()).await
    }

    /// Creates an empty application with process-local built-in factories.
    ///
    /// # Errors
    ///
    /// Returns an engine, Supervisor, or root Fiber creation error.
    pub async fn new_with_builtins(
        base_dir: impl Into<PathBuf>,
        limits: WasmLimits,
        policy: ArtifactPolicy,
        builtins: BuiltinRegistry,
    ) -> Result<Self, LoaderError> {
        let engine = WasmEngine::new().map_err(driver_error)?;
        let runtime = Runtime::start();
        let handle = runtime.handle();
        let root = handle.create_fiber(None).await.map_err(driver_error)?;
        let root_context = Context::root(root);
        let driver = WasmEntryDriver::new(
            handle,
            (root, root_context),
            base_dir.into(),
            builtins,
            engine,
            limits,
            policy,
        );
        Ok(Self {
            runtime,
            root,
            tree: EntryTree::new(driver.clone()),
            driver,
        })
    }

    /// Reconciles the application to a new declarative Entry tree.
    ///
    /// # Errors
    ///
    /// Returns an Entry validation, component preflight, or lifecycle error.
    pub async fn reconcile(&mut self, entries: Vec<EntrySpec>) -> Result<(), LoaderError> {
        self.tree.reconcile(entries).await
    }

    pub const fn driver(&self) -> &Arc<WasmEntryDriver> {
        &self.driver
    }

    /// Returns the current Supervisor snapshot.
    ///
    /// # Errors
    ///
    /// Returns an error after the Supervisor has closed.
    pub async fn snapshot(&self) -> Result<RuntimeSnapshot, LoaderError> {
        self.runtime.handle().snapshot().await.map_err(driver_error)
    }

    /// Waits for all runnable lifecycle work to settle.
    ///
    /// # Errors
    ///
    /// Returns an error after the Supervisor has closed.
    pub async fn settle(&self) -> Result<RuntimeSnapshot, LoaderError> {
        self.runtime
            .handle()
            .await_quiescent()
            .await
            .map_err(driver_error)
    }

    /// Stops all Entries child-first, retires the root, and shuts down Supervisor.
    ///
    /// # Errors
    ///
    /// Returns the first cleanup or Supervisor error after shutdown is attempted.
    pub async fn shutdown(mut self) -> Result<RuntimeSnapshot, LoaderError> {
        let cleanup = self.tree.reconcile(Vec::new()).await;
        let handle = self.runtime.handle();
        let root_cleanup = handle.retire_fiber(self.root).await.map_err(driver_error);
        let snapshot = self.runtime.shutdown().await.map_err(driver_error)?;
        cleanup?;
        root_cleanup?;
        Ok(snapshot)
    }
}

/// Summary returned by `cordis check` after descriptor and config preflight.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct CheckReport {
    pub entries: usize,
    pub components: BTreeSet<String>,
}

struct PreflightDriver {
    base_dir: PathBuf,
    engine: WasmEngine,
    limits: WasmLimits,
    policy: ArtifactPolicy,
    builtins: BuiltinRegistry,
    report: SyncMutex<CheckReport>,
}

impl EntryDriver for PreflightDriver {
    fn start<'a>(&'a self, entry: &'a ResolvedEntry) -> LoaderFuture<'a, ()> {
        Box::pin(async move {
            let factory: Arc<dyn ComponentFactory> = match entry.component.as_ref() {
                Some(ComponentRef::Builtin(name)) => self.builtins.get(name).ok_or_else(|| {
                    LoaderError::Driver(format!("builtin component `{name}` is not registered"))
                })?,
                Some(ComponentRef::File(path)) => {
                    let bytes = std::fs::read(self.base_dir.join(path)).map_err(driver_error)?;
                    Arc::new(
                        WasmComponentFactory::from_bytes(
                            self.engine.clone(),
                            bytes,
                            self.limits.clone(),
                            self.policy.clone(),
                        )
                        .await
                        .map_err(driver_error)?,
                    )
                }
                None => {
                    return Err(LoaderError::Driver(format!(
                        "Entry `{}` has no component",
                        entry.spec.id
                    )));
                }
            };
            validate_config(entry, factory.as_ref())?;
            let mut report = self
                .report
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            report.entries += 1;
            report
                .components
                .insert(factory.descriptor().name.to_string());
            Ok(())
        })
    }

    fn update<'a>(&'a self, _: &'a ResolvedEntry, next: &'a ResolvedEntry) -> LoaderFuture<'a, ()> {
        self.start(next)
    }

    fn stop<'a>(&'a self, _: &'a ResolvedEntry) -> LoaderFuture<'a, ()> {
        Box::pin(async { Ok(()) })
    }
}

/// Validates an Entry tree and every referenced component without activating it.
///
/// # Errors
///
/// Returns an Entry validation, artifact, ABI, capability, or config schema error.
pub async fn check_entries(
    base_dir: impl Into<PathBuf>,
    entries: Vec<EntrySpec>,
    limits: WasmLimits,
    policy: ArtifactPolicy,
) -> Result<CheckReport, LoaderError> {
    check_entries_with_builtins(
        base_dir,
        entries,
        limits,
        policy,
        BuiltinRegistry::default(),
    )
    .await
}

/// Validates an Entry tree against WebAssembly artifacts and registered built-ins.
///
/// # Errors
///
/// Returns an Entry validation, artifact, ABI, capability, or config schema error.
pub async fn check_entries_with_builtins(
    base_dir: impl Into<PathBuf>,
    entries: Vec<EntrySpec>,
    limits: WasmLimits,
    policy: ArtifactPolicy,
    builtins: BuiltinRegistry,
) -> Result<CheckReport, LoaderError> {
    let driver = Arc::new(PreflightDriver {
        base_dir: base_dir.into(),
        engine: WasmEngine::new().map_err(driver_error)?,
        limits,
        policy,
        builtins,
        report: SyncMutex::new(CheckReport::default()),
    });
    let mut tree = EntryTree::new(driver.clone());
    tree.reconcile(entries).await?;
    let report = driver
        .report
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .clone();
    Ok(report)
}

fn validate_config(
    entry: &ResolvedEntry,
    factory: &dyn ComponentFactory,
) -> Result<(), LoaderError> {
    let schema = serde_json::to_value(&factory.descriptor().config_schema).map_err(|error| {
        LoaderError::InvalidSchema {
            component: entry.spec.component.clone(),
            message: error.to_string(),
        }
    })?;
    let validator =
        jsonschema::draft202012::new(&schema).map_err(|error| LoaderError::InvalidSchema {
            component: entry.spec.component.clone(),
            message: error.to_string(),
        })?;
    if let Err(error) = validator.validate(&entry.spec.config) {
        return Err(LoaderError::InvalidConfig {
            entry: entry.spec.id.clone(),
            path: error.instance_path().as_str().to_owned(),
            message: error.to_string(),
        });
    }
    Ok(())
}

fn driver_error(error: impl std::fmt::Display) -> LoaderError {
    LoaderError::Driver(error.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use cordis_core::{
        Capability, ComponentInstance, DynamicComponentDescriptor, InjectSpec, InstanceHost,
    };
    use cordis_loader::IsolationRule;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::Duration;

    struct FixtureBuiltin {
        descriptor: DynamicComponentDescriptor,
        activations: Arc<AtomicUsize>,
        deactivations: Arc<AtomicUsize>,
    }

    impl FixtureBuiltin {
        fn new() -> Arc<Self> {
            Arc::new(Self {
                descriptor: DynamicComponentDescriptor {
                    name: "fixture.builtin".into(),
                    version: "0.1.0".into(),
                    kernel_abi: "0.1".into(),
                    injects: Vec::<InjectSpec>::new(),
                    provides: Vec::new(),
                    config_schema: true.into(),
                    capabilities: BTreeSet::<Capability>::new(),
                },
                activations: Arc::new(AtomicUsize::new(0)),
                deactivations: Arc::new(AtomicUsize::new(0)),
            })
        }
    }

    impl ComponentFactory for FixtureBuiltin {
        fn descriptor(&self) -> &DynamicComponentDescriptor {
            &self.descriptor
        }

        fn instantiate(&self, _: InstanceHost) -> ComponentFuture<'_, Box<dyn ComponentInstance>> {
            let activations = self.activations.clone();
            let deactivations = self.deactivations.clone();
            Box::pin(async move {
                Ok(Box::new(FixtureInstance {
                    activations,
                    deactivations,
                }) as Box<dyn ComponentInstance>)
            })
        }
    }

    struct FixtureInstance {
        activations: Arc<AtomicUsize>,
        deactivations: Arc<AtomicUsize>,
    }

    impl ComponentInstance for FixtureInstance {
        fn activate(&mut self, _: serde_json::Value) -> ComponentFuture<'_, ()> {
            self.activations.fetch_add(1, Ordering::SeqCst);
            Box::pin(async { Ok(()) })
        }

        fn deactivate(&mut self) -> ComponentFuture<'_, ()> {
            self.deactivations.fetch_add(1, Ordering::SeqCst);
            Box::pin(async { Ok(()) })
        }

        fn call_service(&mut self, call: DynamicCall) -> ComponentFuture<'_, Vec<u8>> {
            Box::pin(async move { Ok(call.payload) })
        }

        fn call_event(&mut self, call: EventCall) -> ComponentFuture<'_, EventReply> {
            Box::pin(async move { Ok(EventReply::Continue(call.payload)) })
        }
    }

    fn guest_entries(fixtures: &std::path::Path) -> Result<Vec<EntrySpec>, LoaderError> {
        let mut provider = EntrySpec::leaf(
            "provider",
            format!(
                "file:{}",
                fixtures.join("wasm_counter_provider.wasm").display()
            ),
        )?;
        provider.config = serde_json::json!({});
        provider.isolate.insert(
            "example.counter".into(),
            IsolationRule::Global("example".into()),
        );

        let mut consumer = EntrySpec::leaf(
            "consumer",
            format!(
                "file:{}",
                fixtures.join("wasm_counter_consumer.wasm").display()
            ),
        )?;
        consumer.config = serde_json::json!({});
        consumer.isolate.insert(
            "example.counter".into(),
            IsolationRule::Global("example".into()),
        );
        Ok(vec![consumer, provider])
    }

    #[tokio::test]
    async fn declarative_guest_artifacts_check_mount_route_and_shutdown()
    -> Result<(), Box<dyn std::error::Error>> {
        let Ok(fixtures) = std::env::var("CORDIS_GUEST_FIXTURES") else {
            return Ok(());
        };
        let fixtures = PathBuf::from(fixtures);
        let entries = guest_entries(&fixtures)?;
        let report = check_entries(
            ".",
            entries.clone(),
            WasmLimits::default(),
            ArtifactPolicy::default(),
        )
        .await?;
        assert_eq!(report.entries, 2);
        assert_eq!(report.components.len(), 2);

        let mut application =
            WasmApplication::new(".", WasmLimits::default(), ArtifactPolicy::default()).await?;
        application.reconcile(entries).await?;
        let snapshot = tokio::time::timeout(Duration::from_secs(5), application.settle()).await??;
        assert!(
            snapshot
                .fibers
                .iter()
                .filter(|fiber| fiber.id != application.root)
                .all(|fiber| fiber.state == FiberState::Active)
        );

        let snapshot = application.shutdown().await?;
        assert!(
            snapshot
                .fibers
                .iter()
                .all(|fiber| fiber.state == FiberState::Disposed)
        );
        Ok(())
    }

    #[tokio::test]
    async fn registered_builtin_preflights_mounts_and_stops_without_hmr_artifact() {
        let factory = FixtureBuiltin::new();
        let builtins = BuiltinRegistry::default();
        builtins.register("fixture", factory.clone()).unwrap();
        assert!(builtins.register("fixture", factory.clone()).is_err());
        let entries = vec![EntrySpec::leaf("builtin", "builtin:fixture").unwrap()];

        let report = check_entries_with_builtins(
            ".",
            entries.clone(),
            WasmLimits::default(),
            ArtifactPolicy::default(),
            builtins.clone(),
        )
        .await
        .unwrap();
        assert_eq!(report.entries, 1);
        assert_eq!(
            report.components,
            BTreeSet::from(["fixture.builtin".into()])
        );

        let mut application = WasmApplication::new_with_builtins(
            ".",
            WasmLimits::default(),
            ArtifactPolicy::default(),
            builtins,
        )
        .await
        .unwrap();
        application.reconcile(entries).await.unwrap();
        application.settle().await.unwrap();
        assert_eq!(factory.activations.load(Ordering::SeqCst), 1);
        assert!(application.driver().artifact_paths().await.is_empty());
        application.shutdown().await.unwrap();
        assert_eq!(factory.deactivations.load(Ordering::SeqCst), 1);
    }
}
