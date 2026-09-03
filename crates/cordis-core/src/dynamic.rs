use crate::{
    Context, CordisError, EffectGuard, EffectScope, EffectSet, EventId, EventMode, FiberId,
    FiberState, InjectSpec, RuntimeHandle, ServiceId, TransitionKind,
};
use schemars::Schema;
use serde_json::Value;
use std::collections::BTreeSet;
use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Mutex as SyncMutex};
use tokio::sync::{Mutex, Notify};

/// Owned asynchronous result used by object-safe dynamic component boundaries.
pub type ComponentFuture<'a, T> = Pin<Box<dyn Future<Output = Result<T, CordisError>> + Send + 'a>>;

/// A capability requested by a dynamic component manifest.
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct Capability(Arc<str>);

impl Capability {
    pub fn new(value: impl Into<Arc<str>>) -> Self {
        Self(value.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Owned descriptor shared by loaders, native adapters, and WebAssembly factories.
#[derive(Clone, Debug)]
pub struct DynamicComponentDescriptor {
    pub name: Arc<str>,
    pub version: Arc<str>,
    pub kernel_abi: Arc<str>,
    pub injects: Vec<InjectSpec>,
    pub provides: Vec<ServiceId>,
    pub config_schema: Schema,
    pub capabilities: BTreeSet<Capability>,
}

/// A type-erased service request at a native/WebAssembly boundary.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DynamicCall {
    pub service: ServiceId,
    pub method: u32,
    pub payload: Vec<u8>,
}

/// A type-erased event callback request at a native/WebAssembly boundary.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct EventCall {
    pub event: EventId,
    pub listener_id: u64,
    pub mode: EventMode,
    pub payload: Vec<u8>,
    pub next_token: Option<u64>,
}

/// Dynamic equivalent of `ControlFlow`, retaining the encoded event payload.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum EventReply {
    Continue(Vec<u8>),
    Break(Vec<u8>),
}

/// Registration requested by a guest. The host remains authoritative for cleanup.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum RegistrationRequest {
    Provide(ServiceId),
    Listen {
        event: EventId,
        listener_id: u64,
        mode: EventMode,
    },
}

/// Host-side operations available to a dynamic component instance.
pub trait KernelHost: Send + Sync + 'static {
    fn log(&self, fiber: FiberId, level: &str, message: &str);

    fn call_service(&self, fiber: FiberId, call: DynamicCall) -> ComponentFuture<'_, Vec<u8>>;

    fn dispatch_event(&self, fiber: FiberId, call: EventCall) -> ComponentFuture<'_, EventReply>;

    /// Registers the host-side inverse in `scope` before returning success.
    fn register(
        &self,
        fiber: FiberId,
        request: RegistrationRequest,
        scope: EffectScope,
    ) -> ComponentFuture<'_, ()>;
}

/// Per-instance authority passed to a dynamic component factory.
#[derive(Clone)]
pub struct InstanceHost {
    fiber: FiberId,
    runtime: RuntimeHandle,
    effects: EffectSet,
    kernel: Arc<dyn KernelHost>,
}

impl std::fmt::Debug for InstanceHost {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("InstanceHost")
            .field("fiber", &self.fiber)
            .field("effects", &self.effects)
            .finish_non_exhaustive()
    }
}

impl InstanceHost {
    pub fn new(
        fiber: FiberId,
        runtime: RuntimeHandle,
        effects: EffectSet,
        kernel: Arc<dyn KernelHost>,
    ) -> Self {
        Self {
            fiber,
            runtime,
            effects,
            kernel,
        }
    }

    pub const fn fiber(&self) -> FiberId {
        self.fiber
    }

    pub const fn runtime(&self) -> &RuntimeHandle {
        &self.runtime
    }

    pub const fn effects(&self) -> &EffectSet {
        &self.effects
    }

    pub fn log(&self, level: &str, message: &str) {
        self.kernel.log(self.fiber, level, message);
    }

    /// Routes a dynamic service call through this instance's Kernel authority.
    ///
    /// # Errors
    ///
    /// Returns routing, dependency, codec, or component errors from the Kernel.
    pub async fn call_service(&self, call: DynamicCall) -> Result<Vec<u8>, CordisError> {
        self.kernel.call_service(self.fiber, call).await
    }

    /// Routes an event callback through this instance's Kernel authority.
    ///
    /// # Errors
    ///
    /// Returns routing, listener, codec, or component errors from the Kernel.
    pub async fn dispatch_event(&self, call: EventCall) -> Result<EventReply, CordisError> {
        self.kernel.dispatch_event(self.fiber, call).await
    }

    /// Creates an effect before exposing a registration handle to guest code.
    ///
    /// # Errors
    ///
    /// Returns an inactive-effect or Kernel registration error.
    pub async fn register(&self, request: RegistrationRequest) -> Result<EffectGuard, CordisError> {
        let label = match &request {
            RegistrationRequest::Provide(service) => format!("provide:{}", service.name()),
            RegistrationRequest::Listen { event, .. } => format!("listen:{}", event.name()),
        };
        let (guard, scope) = self.effects.effect(label)?;
        if let Err(error) = self.kernel.register(self.fiber, request, scope).await {
            let _ = guard.dispose().await;
            return Err(error);
        }
        Ok(guard)
    }
}

/// Runtime-neutral factory for native or WebAssembly dynamic components.
pub trait ComponentFactory: Send + Sync + 'static {
    fn descriptor(&self) -> &DynamicComponentDescriptor;

    fn instantiate(&self, host: InstanceHost) -> ComponentFuture<'_, Box<dyn ComponentInstance>>;
}

/// Runtime-neutral lifecycle and callback interface for one component instance.
pub trait ComponentInstance: Send + 'static {
    fn activate(&mut self, config: Value) -> ComponentFuture<'_, ()>;

    fn deactivate(&mut self) -> ComponentFuture<'_, ()>;

    fn call_service(&mut self, call: DynamicCall) -> ComponentFuture<'_, Vec<u8>>;

    fn call_event(&mut self, call: EventCall) -> ComponentFuture<'_, EventReply>;
}

struct ActiveComponent {
    instance: Box<dyn ComponentInstance>,
    effects: EffectSet,
}

struct DynamicFiberState {
    factory: Arc<dyn ComponentFactory>,
    kernel: Arc<dyn KernelHost>,
    config: Value,
    revision: u64,
    settled_revision: Option<u64>,
    load_error: Option<CordisError>,
    unload_error: Option<CordisError>,
    active: Option<ActiveComponent>,
}

/// A dynamic component mounted on the ordinary Supervisor lifecycle.
///
/// Calls through this handle are serialized with load and unload so a component
/// instance (and, for Wasmtime, its `Store`) is never entered concurrently.
#[derive(Clone)]
pub struct DynamicFiber {
    fiber: FiberId,
    runtime: RuntimeHandle,
    state: Arc<Mutex<DynamicFiberState>>,
    changed: Arc<Notify>,
    reload: Arc<Mutex<()>>,
    calls: Arc<CallGate>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct CallOwner {
    task: Option<tokio::task::Id>,
    thread: std::thread::ThreadId,
}

impl CallOwner {
    fn current() -> Self {
        Self {
            task: tokio::task::try_id(),
            thread: std::thread::current().id(),
        }
    }
}

#[derive(Debug, Default)]
struct CallGate {
    owner: SyncMutex<Option<CallOwner>>,
    changed: Notify,
}

impl CallGate {
    async fn acquire(self: &Arc<Self>, fiber: FiberId) -> Result<CallGuard, CordisError> {
        let owner = CallOwner::current();
        loop {
            let changed = self.changed.notified();
            {
                let mut active = self
                    .owner
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                match *active {
                    None => {
                        *active = Some(owner);
                        return Ok(CallGuard { gate: self.clone() });
                    }
                    Some(active_owner) if active_owner == owner => {
                        return Err(CordisError::ReentrantCall { fiber });
                    }
                    Some(_) => {}
                }
            }
            changed.await;
        }
    }
}

#[derive(Debug)]
struct CallGuard {
    gate: Arc<CallGate>,
}

impl Drop for CallGuard {
    fn drop(&mut self) {
        *self
            .gate
            .owner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = None;
        self.gate.changed.notify_waiters();
    }
}

impl std::fmt::Debug for DynamicFiber {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("DynamicFiber")
            .field("fiber", &self.fiber)
            .finish_non_exhaustive()
    }
}

impl DynamicFiber {
    pub const fn fiber(&self) -> FiberId {
        self.fiber
    }

    /// Waits for the current component revision to activate.
    ///
    /// # Errors
    ///
    /// Returns the activation failure, or [`CordisError::InactiveFiber`] while
    /// required dependencies keep the fiber waiting.
    pub async fn await_active(&self) -> Result<(), CordisError> {
        let revision = self.state.lock().await.revision;
        loop {
            let changed = self.changed.notified();
            let (settled, error, active) = {
                let state = self.state.lock().await;
                (
                    state.settled_revision,
                    state.load_error.clone(),
                    state.active.is_some(),
                )
            };
            if settled.is_some_and(|settled| settled >= revision) {
                let fiber = self.runtime.await_settled(self.fiber).await?;
                return error.or(fiber.failure).map_or_else(
                    || {
                        (fiber.state == FiberState::Active)
                            .then_some(())
                            .ok_or(CordisError::InactiveFiber { fiber: self.fiber })
                    },
                    Err,
                );
            }
            if active {
                return Ok(());
            }

            let snapshot = self.runtime.snapshot().await?;
            let fiber = snapshot
                .fibers
                .iter()
                .find(|candidate| candidate.id == self.fiber)
                .ok_or(CordisError::UnknownFiber { fiber: self.fiber })?;
            if fiber.state == FiberState::Pending && !fiber.desired.is_ready() {
                return Err(CordisError::InactiveFiber { fiber: self.fiber });
            }
            changed.await;
        }
    }

    /// Replaces the factory and configuration through an unload/load restart.
    ///
    /// A failed candidate remains in the Fiber's failed state so callers such as
    /// the HMR transaction manager can explicitly restore the previous factory.
    ///
    /// # Errors
    ///
    /// Returns a Supervisor, unload, instantiation, or activation error.
    pub async fn replace(
        &self,
        factory: Arc<dyn ComponentFactory>,
        config: Value,
    ) -> Result<(), CordisError> {
        let _reload = self.reload.lock().await;
        let snapshot = self.runtime.snapshot().await?;
        let fiber = snapshot
            .fibers
            .iter()
            .find(|candidate| candidate.id == self.fiber)
            .ok_or(CordisError::UnknownFiber { fiber: self.fiber })?;
        if !matches!(fiber.state, FiberState::Active | FiberState::Failed) {
            return Err(CordisError::InactiveFiber { fiber: self.fiber });
        }

        let (old_factory, old_config, old_revision, revision) = {
            let mut state = self.state.lock().await;
            let old_factory = state.factory.clone();
            let old_config = std::mem::replace(&mut state.config, config);
            let old_revision = state.revision;
            state.factory = factory;
            state.revision = state.revision.saturating_add(1);
            state.load_error = None;
            (old_factory, old_config, old_revision, state.revision)
        };
        let restart = if fiber.state == FiberState::Active {
            self.runtime.reload_fiber(self.fiber).await
        } else {
            self.runtime.restart_fiber(self.fiber).await
        };
        if let Err(error) = restart {
            let mut state = self.state.lock().await;
            state.factory = old_factory;
            state.config = old_config;
            state.revision = old_revision;
            return Err(error);
        }
        self.await_revision(revision).await
    }

    /// Calls a service export on the active instance.
    ///
    /// # Errors
    ///
    /// Returns [`CordisError::InactiveFiber`] outside the active epoch, or the
    /// component's service error.
    pub async fn call_service(&self, call: DynamicCall) -> Result<Vec<u8>, CordisError> {
        let _call = self.calls.acquire(self.fiber).await?;
        let mut state = self.state.lock().await;
        let active = state
            .active
            .as_mut()
            .ok_or(CordisError::InactiveFiber { fiber: self.fiber })?;
        active.instance.call_service(call).await
    }

    /// Calls an event export on the active instance.
    ///
    /// # Errors
    ///
    /// Returns [`CordisError::InactiveFiber`] outside the active epoch, or the
    /// component's event error.
    pub async fn call_event(&self, call: EventCall) -> Result<EventReply, CordisError> {
        let _call = self.calls.acquire(self.fiber).await?;
        let mut state = self.state.lock().await;
        let active = state
            .active
            .as_mut()
            .ok_or(CordisError::InactiveFiber { fiber: self.fiber })?;
        active.instance.call_event(call).await
    }

    /// Irreversibly retires the fiber and waits for instance/effect cleanup.
    ///
    /// # Errors
    ///
    /// Returns a Supervisor or component teardown error.
    pub async fn retire(&self) -> Result<(), CordisError> {
        let _reload = self.reload.lock().await;
        self.runtime.retire_fiber(self.fiber).await?;
        self.runtime.await_disposed(self.fiber).await?;
        self.runtime.remove_executor(self.fiber);
        self.state
            .lock()
            .await
            .unload_error
            .take()
            .map_or(Ok(()), Err)
    }

    async fn await_revision(&self, revision: u64) -> Result<(), CordisError> {
        loop {
            let changed = self.changed.notified();
            let settled = {
                let state = self.state.lock().await;
                state
                    .settled_revision
                    .filter(|settled| *settled >= revision)
                    .map(|_| state.load_error.clone())
            };
            if let Some(error) = settled {
                let fiber = self.runtime.await_settled(self.fiber).await?;
                return error.or(fiber.failure).map_or_else(
                    || {
                        (fiber.state == FiberState::Active)
                            .then_some(())
                            .ok_or(CordisError::InactiveFiber { fiber: self.fiber })
                    },
                    Err,
                );
            }
            changed.await;
        }
    }
}

impl RuntimeHandle {
    /// Mounts a dynamic component on a new Supervisor-owned fiber.
    ///
    /// `base_context` is extended to the new fiber; `None` creates a root
    /// context. The returned handle may initially be waiting for required
    /// providers. Use [`DynamicFiber::await_active`] when activation is required.
    ///
    /// # Errors
    ///
    /// Returns a fiber creation or dependency configuration error.
    pub async fn mount_dynamic(
        &self,
        parent: Option<FiberId>,
        base_context: Option<&Context>,
        factory: Arc<dyn ComponentFactory>,
        kernel: Arc<dyn KernelHost>,
        config: Value,
    ) -> Result<DynamicFiber, CordisError> {
        let fiber = self.create_fiber(parent).await?;
        let context = base_context.map_or_else(|| Context::root(fiber), |base| base.extend(fiber));
        let injects = factory.descriptor().injects.clone();
        let state = Arc::new(Mutex::new(DynamicFiberState {
            factory,
            kernel,
            config,
            revision: 0,
            settled_revision: None,
            load_error: None,
            unload_error: None,
            active: None,
        }));
        let changed = Arc::new(Notify::new());
        let executor_state = state.clone();
        let executor_changed = changed.clone();
        let executor_runtime = self.clone();
        self.install_executor(
            fiber,
            Arc::new(move |transition| {
                let state = executor_state.clone();
                let changed = executor_changed.clone();
                let runtime = executor_runtime.clone();
                Box::pin(async move {
                    run_dynamic_transition(
                        runtime,
                        transition.kind,
                        transition.fiber,
                        state,
                        changed,
                    )
                    .await
                })
            }),
        );

        if let Err(error) = self.configure_dependencies(fiber, context, injects).await {
            let _ = self.retire_fiber(fiber).await;
            let _ = self.await_disposed(fiber).await;
            self.remove_executor(fiber);
            return Err(error);
        }
        Ok(DynamicFiber {
            fiber,
            runtime: self.clone(),
            state,
            changed,
            reload: Arc::new(Mutex::new(())),
            calls: Arc::new(CallGate::default()),
        })
    }
}

async fn run_dynamic_transition(
    runtime: RuntimeHandle,
    kind: TransitionKind,
    fiber: FiberId,
    state: Arc<Mutex<DynamicFiberState>>,
    changed: Arc<Notify>,
) -> Result<(), CordisError> {
    match kind {
        TransitionKind::Load { .. } => {
            runtime.commit_dependencies(fiber).await?;
            let mut state = state.lock().await;
            let revision = state.revision;
            if let Some(error) = state.unload_error.take() {
                state.settled_revision = Some(revision);
                state.load_error = Some(error.clone());
                changed.notify_waiters();
                return Err(error);
            }
            let factory = state.factory.clone();
            let effects = EffectSet::new(format!("dynamic:{}", factory.descriptor().name));
            let host = InstanceHost::new(fiber, runtime, effects.clone(), state.kernel.clone());
            let result = match factory.instantiate(host).await {
                Ok(mut instance) => match instance.activate(state.config.clone()).await {
                    Ok(()) => {
                        state.active = Some(ActiveComponent { instance, effects });
                        Ok(())
                    }
                    Err(error) => {
                        let _ = instance.deactivate().await;
                        let _ = effects.dispose().await;
                        Err(error)
                    }
                },
                Err(error) => {
                    let _ = effects.dispose().await;
                    Err(error)
                }
            };
            state.settled_revision = Some(revision);
            state.load_error = result.as_ref().err().cloned();
            changed.notify_waiters();
            result
        }
        TransitionKind::Unload => {
            let mut state = state.lock().await;
            let Some(mut active) = state.active.take() else {
                return Ok(());
            };
            let deactivate = active.instance.deactivate().await;
            let dispose =
                active
                    .effects
                    .dispose()
                    .await
                    .map_err(|report| CordisError::DisposerFailed {
                        message: report.to_string(),
                    });
            let result = deactivate.and(dispose);
            state.unload_error = result.as_ref().err().cloned();
            result
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Disposer, Runtime};
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

    #[derive(Default)]
    struct NoopKernel;

    impl KernelHost for NoopKernel {
        fn log(&self, _: FiberId, _: &str, _: &str) {}

        fn call_service(&self, _: FiberId, call: DynamicCall) -> ComponentFuture<'_, Vec<u8>> {
            Box::pin(async move { Ok(call.payload) })
        }

        fn dispatch_event(&self, _: FiberId, call: EventCall) -> ComponentFuture<'_, EventReply> {
            Box::pin(async move { Ok(EventReply::Continue(call.payload)) })
        }

        fn register(
            &self,
            _: FiberId,
            _: RegistrationRequest,
            _: EffectScope,
        ) -> ComponentFuture<'_, ()> {
            Box::pin(async { Ok(()) })
        }
    }

    struct FixtureFactory {
        descriptor: DynamicComponentDescriptor,
        fail_activate: bool,
        activations: Arc<AtomicUsize>,
        deactivations: Arc<AtomicUsize>,
        disposals: Arc<AtomicUsize>,
    }

    impl FixtureFactory {
        fn new(name: &str, fail_activate: bool) -> Arc<Self> {
            Arc::new(Self {
                descriptor: DynamicComponentDescriptor {
                    name: name.into(),
                    version: "0.1.0".into(),
                    kernel_abi: "0.1".into(),
                    injects: Vec::new(),
                    provides: Vec::new(),
                    config_schema: true.into(),
                    capabilities: BTreeSet::new(),
                },
                fail_activate,
                activations: Arc::new(AtomicUsize::new(0)),
                deactivations: Arc::new(AtomicUsize::new(0)),
                disposals: Arc::new(AtomicUsize::new(0)),
            })
        }
    }

    impl ComponentFactory for FixtureFactory {
        fn descriptor(&self) -> &DynamicComponentDescriptor {
            &self.descriptor
        }

        fn instantiate(
            &self,
            host: InstanceHost,
        ) -> ComponentFuture<'_, Box<dyn ComponentInstance>> {
            let fail_activate = self.fail_activate;
            let activations = self.activations.clone();
            let deactivations = self.deactivations.clone();
            let disposals = self.disposals.clone();
            Box::pin(async move {
                let (_, scope) = host.effects().effect("fixture")?;
                scope.defer(Disposer::infallible(move || async move {
                    disposals.fetch_add(1, Ordering::SeqCst);
                }))?;
                Ok(Box::new(FixtureInstance {
                    fail_activate,
                    activations,
                    deactivations,
                    active: AtomicBool::new(false),
                }) as Box<dyn ComponentInstance>)
            })
        }
    }

    struct FixtureInstance {
        fail_activate: bool,
        activations: Arc<AtomicUsize>,
        deactivations: Arc<AtomicUsize>,
        active: AtomicBool,
    }

    impl ComponentInstance for FixtureInstance {
        fn activate(&mut self, _: Value) -> ComponentFuture<'_, ()> {
            Box::pin(async move {
                self.activations.fetch_add(1, Ordering::SeqCst);
                if self.fail_activate {
                    return Err(CordisError::ComponentFailed {
                        component: "fixture".to_owned(),
                        message: "activation rejected".to_owned(),
                    });
                }
                self.active.store(true, Ordering::SeqCst);
                Ok(())
            })
        }

        fn deactivate(&mut self) -> ComponentFuture<'_, ()> {
            Box::pin(async move {
                self.deactivations.fetch_add(1, Ordering::SeqCst);
                self.active.store(false, Ordering::SeqCst);
                Ok(())
            })
        }

        fn call_service(&mut self, call: DynamicCall) -> ComponentFuture<'_, Vec<u8>> {
            Box::pin(async move {
                if !self.active.load(Ordering::SeqCst) {
                    return Err(CordisError::InactiveFiber {
                        fiber: FiberId::next(),
                    });
                }
                Ok(call.payload)
            })
        }

        fn call_event(&mut self, call: EventCall) -> ComponentFuture<'_, EventReply> {
            Box::pin(async move { Ok(EventReply::Continue(call.payload)) })
        }
    }

    #[tokio::test]
    async fn dynamic_component_uses_supervisor_lifecycle_and_effect_cleanup() {
        let runtime = Runtime::start();
        let first = FixtureFactory::new("first", false);
        let mounted = runtime
            .handle()
            .mount_dynamic(None, None, first.clone(), Arc::new(NoopKernel), Value::Null)
            .await
            .unwrap();
        mounted.await_active().await.unwrap();

        let service = ServiceId::new("fixture", [0; 32]);
        assert_eq!(
            mounted
                .call_service(DynamicCall {
                    service,
                    method: 7,
                    payload: vec![1, 2, 3],
                })
                .await
                .unwrap(),
            vec![1, 2, 3]
        );

        let second = FixtureFactory::new("second", false);
        mounted
            .replace(second.clone(), serde_json::json!({ "generation": 2 }))
            .await
            .unwrap();
        assert_eq!(first.activations.load(Ordering::SeqCst), 1);
        assert_eq!(first.deactivations.load(Ordering::SeqCst), 1);
        assert_eq!(first.disposals.load(Ordering::SeqCst), 1);
        assert_eq!(second.activations.load(Ordering::SeqCst), 1);

        mounted.retire().await.unwrap();
        assert_eq!(second.deactivations.load(Ordering::SeqCst), 1);
        assert_eq!(second.disposals.load(Ordering::SeqCst), 1);
        let snapshot = runtime.shutdown().await.unwrap();
        assert_eq!(snapshot.fibers[0].state, FiberState::Disposed);
    }

    #[tokio::test]
    async fn failed_replacement_is_cleaned_and_can_be_rolled_back() {
        let runtime = Runtime::start();
        let original = FixtureFactory::new("original", false);
        let mounted = runtime
            .handle()
            .mount_dynamic(
                None,
                None,
                original.clone(),
                Arc::new(NoopKernel),
                Value::Null,
            )
            .await
            .unwrap();
        mounted.await_active().await.unwrap();

        let failing = FixtureFactory::new("failing", true);
        assert!(matches!(
            mounted
                .replace(failing.clone(), serde_json::json!({ "bad": true }))
                .await,
            Err(CordisError::ComponentFailed { .. })
        ));
        assert_eq!(failing.activations.load(Ordering::SeqCst), 1);
        assert_eq!(failing.deactivations.load(Ordering::SeqCst), 1);
        assert_eq!(failing.disposals.load(Ordering::SeqCst), 1);

        mounted
            .replace(original.clone(), Value::Null)
            .await
            .unwrap();
        assert_eq!(original.activations.load(Ordering::SeqCst), 2);
        mounted.retire().await.unwrap();
        runtime.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn same_task_reentry_is_rejected_without_deadlock() {
        let gate = Arc::new(CallGate::default());
        let fiber = FiberId::next();
        let first = gate.acquire(fiber).await.unwrap();
        assert_eq!(
            gate.acquire(fiber).await.unwrap_err(),
            CordisError::ReentrantCall { fiber }
        );
        drop(first);
        gate.acquire(fiber).await.unwrap();
    }
}
