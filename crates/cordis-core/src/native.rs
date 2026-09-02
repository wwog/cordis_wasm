use crate::{
    CommittedView, Context, CordisError, Disposer, EffectSet, FiberId, InjectSpec, RuntimeHandle,
    ServiceId, ServiceKey, TransitionKind,
};
use futures::FutureExt;
use schemars::{JsonSchema, Schema, generate::SchemaSettings};
use serde::{Serialize, de::DeserializeOwned};
use std::collections::BTreeMap;
use std::fmt;
use std::future::Future;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::pin::Pin;
use std::sync::{Arc, RwLock};
use tokio::sync::Mutex as AsyncMutex;

/// Static identity generated for a service trait.
pub trait ServiceSpec: ServiceKey {
    fn service_id() -> ServiceId
    where
        Self: Sized,
    {
        ServiceId::of::<Self>()
    }
}

impl<T: ServiceKey> ServiceSpec for T {}

/// Owned future returned by a native or WebAssembly service dispatcher.
pub type ServiceFuture =
    Pin<Box<dyn Future<Output = Result<Vec<u8>, CordisError>> + Send + 'static>>;

/// Object-safe transport boundary shared by generated native clients and Wasm routing.
pub trait ServiceDispatcher: Send + Sync + 'static {
    fn service_id(&self) -> ServiceId;

    fn dispatch(&self, method_id: u32, payload: Vec<u8>) -> ServiceFuture;
}

/// Checked, type-erased service transport used internally by generated clients.
#[derive(Clone)]
pub struct ServiceClient {
    service: ServiceId,
    dispatcher: Arc<dyn ServiceDispatcher>,
}

impl ServiceClient {
    /// Creates a client after verifying the dispatcher's service name and ABI hash.
    ///
    /// # Errors
    ///
    /// Returns [`CordisError::ServiceIdentityMismatch`] when the dispatcher does not implement
    /// the requested service ABI.
    pub fn new<S: ServiceSpec>(
        dispatcher: Arc<dyn ServiceDispatcher>,
    ) -> Result<Self, CordisError> {
        let expected = S::service_id();
        let actual = dispatcher.service_id();
        if actual != expected {
            return Err(CordisError::ServiceIdentityMismatch { expected, actual });
        }
        Ok(Self {
            service: expected,
            dispatcher,
        })
    }

    pub fn service_id(&self) -> &ServiceId {
        &self.service
    }

    /// Dispatches one already encoded service call.
    ///
    /// # Errors
    ///
    /// Returns the transport or provider error reported by the dispatcher.
    pub async fn call(&self, method_id: u32, payload: Vec<u8>) -> Result<Vec<u8>, CordisError> {
        self.dispatcher.dispatch(method_id, payload).await
    }
}

impl fmt::Debug for ServiceClient {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ServiceClient")
            .field("service", &self.service)
            .finish_non_exhaustive()
    }
}

/// Separates a service's declared error from failures in the Cordis transport.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ServiceCallError<E> {
    Transport(CordisError),
    Service(E),
}

impl<E: fmt::Display> fmt::Display for ServiceCallError<E> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Transport(error) => write!(formatter, "service transport failed: {error}"),
            Self::Service(error) => write!(formatter, "service call failed: {error}"),
        }
    }
}

impl<E> std::error::Error for ServiceCallError<E>
where
    E: std::error::Error + 'static,
{
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Transport(error) => Some(error),
            Self::Service(error) => Some(error),
        }
    }
}

impl<E> From<CordisError> for ServiceCallError<E> {
    fn from(error: CordisError) -> Self {
        Self::Transport(error)
    }
}

/// Encodes one service payload using the canonical `MessagePack` codec.
///
/// # Errors
///
/// Returns [`CordisError::ServiceEncodeFailed`] when `value` cannot be serialized.
pub fn encode_service_payload<T: Serialize>(value: &T) -> Result<Vec<u8>, CordisError> {
    rmp_serde::to_vec(value).map_err(|error| CordisError::ServiceEncodeFailed {
        message: error.to_string(),
    })
}

/// Decodes one service payload using the canonical `MessagePack` codec.
///
/// # Errors
///
/// Returns [`CordisError::ServiceDecodeFailed`] when `payload` is malformed or has the wrong
/// wire type.
pub fn decode_service_payload<T: DeserializeOwned>(payload: &[u8]) -> Result<T, CordisError> {
    rmp_serde::from_slice(payload).map_err(|error| CordisError::ServiceDecodeFailed {
        message: error.to_string(),
    })
}

/// Stable event identity shared by native and WebAssembly components.
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct EventId {
    name: Arc<str>,
    abi_hash: [u8; 32],
}

impl EventId {
    pub fn new(name: impl Into<Arc<str>>, abi_hash: [u8; 32]) -> Self {
        Self {
            name: name.into(),
            abi_hash,
        }
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    pub const fn abi_hash(&self) -> &[u8; 32] {
        &self.abi_hash
    }
}

impl fmt::Display for EventId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}@", self.name)?;
        for byte in &self.abi_hash[..4] {
            write!(formatter, "{byte:02x}")?;
        }
        Ok(())
    }
}

/// Dispatch semantics selected by an event declaration.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum EventMode {
    Emit,
    Parallel,
    Serial,
    Bail,
    Waterfall,
}

/// Static identity and payload types generated for an event declaration.
pub trait EventSpec: Send + Sync + 'static {
    type Input: Clone + Serialize + DeserializeOwned + Send + 'static;
    type Output: Serialize + DeserializeOwned + Send + 'static;

    const NAME: &'static str;
    const ABI_HASH: [u8; 32];
    const MODE: EventMode;

    fn event_id() -> EventId {
        EventId::new(Self::NAME, Self::ABI_HASH)
    }
}

/// Encodes one event payload using the canonical `MessagePack` codec.
///
/// # Errors
///
/// Returns [`CordisError::EventEncodeFailed`] when `value` cannot be serialized.
pub fn encode_event_payload<T: Serialize>(value: &T) -> Result<Vec<u8>, CordisError> {
    rmp_serde::to_vec(value).map_err(|error| CordisError::EventEncodeFailed {
        message: error.to_string(),
    })
}

/// Decodes one event payload using the canonical `MessagePack` codec.
///
/// # Errors
///
/// Returns [`CordisError::EventDecodeFailed`] when `payload` is malformed or has the wrong wire
/// type.
pub fn decode_event_payload<T: DeserializeOwned>(payload: &[u8]) -> Result<T, CordisError> {
    rmp_serde::from_slice(payload).map_err(|error| CordisError::EventDecodeFailed {
        message: error.to_string(),
    })
}

/// Resolves the dispatcher selected by one committed fiber load epoch.
pub trait DependencyResolver {
    /// Returns the dispatcher for a declared, active service dependency.
    ///
    /// # Errors
    ///
    /// Returns an error when the committed view has no provider or the native provider did not
    /// register its dispatcher.
    fn resolve(&self, service: &ServiceId) -> Result<Arc<dyn ServiceDispatcher>, CordisError>;
}

/// Compile-time dependency declaration for a native component or injected method.
pub trait DependencySet: Send + Sync + 'static {
    fn injects() -> Vec<InjectSpec>;

    /// Builds the generated typed client set from a committed dependency view.
    ///
    /// # Errors
    ///
    /// Returns an error when a selected provider has no matching dispatcher.
    fn resolve(resolver: &dyn DependencyResolver) -> Result<Self, CordisError>
    where
        Self: Sized;
}

#[derive(Clone, Copy, Debug, Default)]
pub struct NoDependencies;

impl DependencySet for NoDependencies {
    fn injects() -> Vec<InjectSpec> {
        Vec::new()
    }

    fn resolve(_resolver: &dyn DependencyResolver) -> Result<Self, CordisError> {
        Ok(Self)
    }
}

/// Internal native service dispatchers keyed by their provider fiber.
type ProviderDispatcherMap = BTreeMap<(FiberId, ServiceId), Arc<dyn ServiceDispatcher>>;

/// Native service dispatchers available to generated dependency clients.
#[derive(Clone, Default)]
pub struct NativeServiceRegistry {
    dispatchers: Arc<RwLock<ProviderDispatcherMap>>,
}

impl fmt::Debug for NativeServiceRegistry {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("NativeServiceRegistry")
            .field("len", &self.len())
            .finish()
    }
}

impl NativeServiceRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    /// Associates a dispatcher with the fiber that owns the provider.
    pub fn insert(&self, provider: FiberId, dispatcher: Arc<dyn ServiceDispatcher>) {
        let service = dispatcher.service_id();
        self.dispatchers
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert((provider, service), dispatcher);
    }

    pub fn remove(&self, provider: FiberId, service: &ServiceId) {
        self.dispatchers
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .remove(&(provider, service.clone()));
    }

    pub fn len(&self) -> usize {
        self.dispatchers
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    fn resolve(
        &self,
        provider: FiberId,
        service: &ServiceId,
    ) -> Result<Arc<dyn ServiceDispatcher>, CordisError> {
        self.dispatchers
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(&(provider, service.clone()))
            .cloned()
            .ok_or_else(|| CordisError::MissingServiceDispatcher {
                provider,
                service: service.clone(),
            })
    }
}

struct CommittedDependencyResolver<'a> {
    committed: &'a CommittedView,
    services: &'a NativeServiceRegistry,
}

impl DependencyResolver for CommittedDependencyResolver<'_> {
    fn resolve(&self, service: &ServiceId) -> Result<Arc<dyn ServiceDispatcher>, CordisError> {
        let provider = self.committed.lookup(service)?.ok_or_else(|| {
            CordisError::MissingCommittedProvider {
                service: service.clone(),
            }
        })?;
        self.services.resolve(provider, service)
    }
}

/// Runtime bridge used by generated method-level injects.
#[derive(Clone, Debug)]
pub struct MethodFiberRuntime {
    runtime: RuntimeHandle,
    services: NativeServiceRegistry,
}

impl MethodFiberRuntime {
    pub fn new(runtime: RuntimeHandle, services: NativeServiceRegistry) -> Self {
        Self { runtime, services }
    }

    pub fn services(&self) -> &NativeServiceRegistry {
        &self.services
    }
}

/// Shared, asynchronously serialized ownership used by generated component adapters.
#[doc(hidden)]
pub struct ComponentCell<T> {
    inner: Arc<AsyncMutex<T>>,
}

impl<T> fmt::Debug for ComponentCell<T> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ComponentCell")
            .finish_non_exhaustive()
    }
}

impl<T> Clone for ComponentCell<T> {
    fn clone(&self) -> Self {
        Self {
            inner: self.inner.clone(),
        }
    }
}

impl<T> ComponentCell<T> {
    pub fn new(value: T) -> Self {
        Self {
            inner: Arc::new(AsyncMutex::new(value)),
        }
    }

    pub async fn lock(&self) -> tokio::sync::MutexGuard<'_, T> {
        self.inner.lock().await
    }
}

/// Converts a panic in generated component code into a fiber failure.
#[doc(hidden)]
pub async fn catch_component_future<T, F>(fiber: FiberId, future: F) -> Result<T, CordisError>
where
    F: Future<Output = Result<T, CordisError>> + Send,
{
    match AssertUnwindSafe(future).catch_unwind().await {
        Ok(result) => result,
        Err(payload) => Err(CordisError::FiberExecutorPanicked {
            fiber,
            message: panic_payload_message(payload.as_ref()),
        }),
    }
}

/// Immutable metadata generated by `#[cordis::component]`.
#[derive(Clone, Debug)]
pub struct ComponentDescriptor {
    pub name: &'static str,
    pub injects: Vec<InjectSpec>,
    pub config_schema: fn() -> Schema,
}

/// Metadata and associated types shared by the generated component adapter.
pub trait ComponentDefinition: Send + 'static {
    type Config: DeserializeOwned + JsonSchema + Send + Sync + 'static;
    type Deps: DependencySet;

    fn descriptor() -> &'static ComponentDescriptor;
}

/// Context passed to a native component's apply method.
#[derive(Clone, Debug)]
pub struct ComponentContext<D: DependencySet> {
    context: Context,
    deps: Arc<D>,
    effects: EffectSet,
    method_runtime: Option<MethodFiberRuntime>,
}

impl<D: DependencySet> ComponentContext<D> {
    pub fn new(context: Context, deps: D, effects: EffectSet) -> Self {
        Self {
            context,
            deps: Arc::new(deps),
            effects,
            method_runtime: None,
        }
    }

    /// Enables method-level inject registration for this component load.
    #[must_use]
    pub fn with_method_runtime(mut self, runtime: MethodFiberRuntime) -> Self {
        self.method_runtime = Some(runtime);
        self
    }

    pub fn context(&self) -> &Context {
        &self.context
    }

    pub fn deps(&self) -> &D {
        &self.deps
    }

    pub fn effects(&self) -> &EffectSet {
        &self.effects
    }

    pub fn effect_set(&self) -> EffectSet {
        self.effects.clone()
    }

    /// Registers one generated method as an effect-owned child fiber.
    ///
    /// # Errors
    ///
    /// Returns an error if this context has no method runtime, the parent is inactive, dependency
    /// configuration fails, or the owning effect has begun disposal.
    pub async fn register_method<D2, F, Fut>(
        &self,
        label: &'static str,
        callback: F,
    ) -> Result<FiberId, CordisError>
    where
        D2: DependencySet,
        F: Fn(MethodContext<D2>) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Result<(), CordisError>> + Send + 'static,
    {
        let method_runtime = self
            .method_runtime
            .clone()
            .ok_or(CordisError::MissingMethodRuntime)?;
        let (_guard, scope) = self.effects.effect(format!("method:{label}"))?;
        let parent = self.context.fiber();
        let child = method_runtime
            .runtime
            .create_live_child_fiber(parent)
            .await?;
        let child_context = self.context.extend(child);
        let active_effects = Arc::new(AsyncMutex::new(None::<EffectSet>));
        let callback = Arc::new(callback);

        let executor_runtime = method_runtime.runtime.clone();
        let executor_services = method_runtime.services.clone();
        let executor_context = child_context.clone();
        let executor_effects = active_effects.clone();
        let executor = Arc::new(move |transition: crate::FiberTransition| {
            let runtime = executor_runtime.clone();
            let services = executor_services.clone();
            let context = executor_context.clone();
            let active_effects = executor_effects.clone();
            let callback = callback.clone();
            Box::pin(async move {
                match transition.kind {
                    TransitionKind::Load { .. } => {
                        let committed = runtime.commit_dependencies(transition.fiber).await?;
                        let resolver = CommittedDependencyResolver {
                            committed: &committed,
                            services: &services,
                        };
                        let deps = D2::resolve(&resolver)?;
                        let effects = EffectSet::new(format!("method:{label}"));
                        let context = MethodContext::new(context, deps, effects.clone());
                        let callback_future = catch_unwind(AssertUnwindSafe(|| callback(context)));
                        let result = match callback_future {
                            Ok(callback_future) => {
                                AssertUnwindSafe(callback_future).catch_unwind().await
                            }
                            Err(payload) => Err(payload),
                        };
                        let result = match result {
                            Ok(result) => result,
                            Err(payload) => Err(CordisError::FiberExecutorPanicked {
                                fiber: transition.fiber,
                                message: panic_payload_message(payload.as_ref()),
                            }),
                        };
                        if let Err(error) = result {
                            let _ = effects.dispose().await;
                            return Err(error);
                        }
                        *active_effects.lock().await = Some(effects);
                        Ok(())
                    }
                    TransitionKind::Unload => {
                        let effects = active_effects.lock().await.take();
                        if let Some(effects) = effects {
                            effects.dispose().await.map_err(|report| {
                                CordisError::DisposerFailed {
                                    message: report.to_string(),
                                }
                            })?;
                        }
                        Ok(())
                    }
                }
            }) as crate::supervisor::FiberWork
        });
        method_runtime.runtime.install_executor(child, executor);

        let retire_runtime = method_runtime.runtime.clone();
        scope.defer(Disposer::new(move || async move {
            retire_runtime.retire_fiber(child).await?;
            retire_runtime.await_disposed(child).await?;
            retire_runtime.remove_executor(child);
            Ok(())
        }))?;

        if let Err(error) = method_runtime
            .runtime
            .configure_dependencies(child, child_context, D2::injects())
            .await
        {
            method_runtime.runtime.remove_executor(child);
            let _ = method_runtime.runtime.retire_fiber(child).await;
            return Err(error);
        }
        Ok(child)
    }
}

fn panic_payload_message(payload: &(dyn std::any::Any + Send)) -> String {
    if let Some(message) = payload.downcast_ref::<&str>() {
        (*message).to_owned()
    } else if let Some(message) = payload.downcast_ref::<String>() {
        message.clone()
    } else {
        "non-string panic payload".to_owned()
    }
}

/// Context passed to one method-level injected child fiber.
#[derive(Clone, Debug)]
pub struct MethodContext<D: DependencySet> {
    context: Context,
    deps: Arc<D>,
    effects: EffectSet,
}

impl<D: DependencySet> MethodContext<D> {
    fn new(context: Context, deps: D, effects: EffectSet) -> Self {
        Self {
            context,
            deps: Arc::new(deps),
            effects,
        }
    }

    pub fn context(&self) -> &Context {
        &self.context
    }

    pub fn deps(&self) -> &D {
        &self.deps
    }

    pub fn effects(&self) -> &EffectSet {
        &self.effects
    }

    pub fn effect_set(&self) -> EffectSet {
        self.effects.clone()
    }
}

/// Effects retained after a component applies successfully.
#[derive(Clone, Debug)]
pub struct ComponentEffects {
    effects: EffectSet,
}

impl ComponentEffects {
    pub fn new(effects: EffectSet) -> Self {
        Self { effects }
    }

    pub fn effect_set(&self) -> &EffectSet {
        &self.effects
    }
}

/// Executable native component implemented by `#[cordis::component_impl]`.
pub trait Component: ComponentDefinition {
    fn apply(
        self,
        context: ComponentContext<Self::Deps>,
        config: Self::Config,
    ) -> impl Future<Output = Result<ComponentEffects, CordisError>> + Send;
}

/// Generates a Draft 2020-12 JSON Schema without requiring macros to name schemars internals.
pub fn config_schema<T: JsonSchema>() -> Schema {
    SchemaSettings::draft2020_12()
        .into_generator()
        .into_root_schema_for::<T>()
}
