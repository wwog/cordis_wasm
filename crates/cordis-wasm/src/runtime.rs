use crate::bindings::cordis::kernel::host as wit;
use crate::bindings::exports::cordis::kernel::plugin as guest;
use crate::{
    ArtifactHash, StoreState, WasiCapabilities, WasmEngine, WasmHostError, WasmLimits, bindings,
};
use cordis_core::{
    Capability, ComponentFactory, ComponentFuture, ComponentInstance, CordisError, DynamicCall,
    DynamicComponentDescriptor, EventCall, EventMode, EventReply, InjectSpec, InstanceHost,
    RegistrationRequest, ServiceId,
};
use futures::FutureExt;
use schemars::Schema;
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};
use std::panic::AssertUnwindSafe;
use std::sync::Arc;
use tokio::task::JoinHandle;
use wasmtime::Store;
use wasmtime::component::{Component, Linker, Resource};
use wasmtime_wasi::{WasiCtxView, WasiView};

/// Descriptor and capability checks performed before a component becomes a candidate.
#[derive(Clone, Debug)]
pub struct ArtifactPolicy {
    pub kernel_abi: String,
    pub allowed_capabilities: BTreeSet<Capability>,
    pub wasi: WasiCapabilities,
}

impl Default for ArtifactPolicy {
    fn default() -> Self {
        Self {
            kernel_abi: "0.1".to_owned(),
            allowed_capabilities: BTreeSet::new(),
            wasi: WasiCapabilities::deny_all(),
        }
    }
}

/// A validated, compiled WebAssembly component implementing the Cordis kernel world.
#[derive(Clone, Debug)]
pub struct WasmComponentFactory {
    engine: WasmEngine,
    component: Arc<Component>,
    descriptor: DynamicComponentDescriptor,
    limits: WasmLimits,
    policy: ArtifactPolicy,
}

impl WasmComponentFactory {
    /// Compiles, links, instantiates, and queries the descriptor without activating the guest.
    ///
    /// # Errors
    ///
    /// Returns compilation, linking, descriptor, ABI, or capability policy errors.
    pub async fn from_bytes(
        engine: WasmEngine,
        bytes: impl AsRef<[u8]>,
        limits: WasmLimits,
        policy: ArtifactPolicy,
    ) -> Result<Self, WasmHostError> {
        let bytes = bytes.as_ref();
        let hash = ArtifactHash::from_bytes(bytes, &policy, &limits);
        let component = Arc::new(engine.compile_cached(bytes, hash)?);
        let descriptor = inspect_descriptor(&engine, &component, &limits, &policy.wasi).await?;
        validate_descriptor(&descriptor, &policy)?;
        validate_wasi_imports(engine.engine(), &component, &descriptor, &policy)?;
        Ok(Self {
            engine,
            component,
            descriptor,
            limits,
            policy,
        })
    }

    pub fn component(&self) -> &Component {
        &self.component
    }

    pub const fn policy(&self) -> &ArtifactPolicy {
        &self.policy
    }
}

impl ComponentFactory for WasmComponentFactory {
    fn descriptor(&self) -> &DynamicComponentDescriptor {
        &self.descriptor
    }

    fn instantiate(&self, host: InstanceHost) -> ComponentFuture<'_, Box<dyn ComponentInstance>> {
        Box::pin(async move {
            let state = GuestState::new(
                Some(host.clone()),
                self.limits.max_payload_bytes,
                self.limits.max_registrations,
                self.policy.wasi.build().map_err(wasm_error)?,
            );
            let mut store = self
                .engine
                .new_store(state, &self.limits)
                .map_err(wasm_error)?;
            let linker = build_linker(self.engine.engine()).map_err(wasm_error)?;
            let bindings =
                bindings::CordisPlugin::instantiate_async(&mut store, &self.component, &linker)
                    .await
                    .map_err(|error| component_error(&self.descriptor.name, error))?;
            Ok(Box::new(WasmComponentInstance {
                engine: self.engine.clone(),
                descriptor: self.descriptor.clone(),
                store,
                bindings,
                host,
                active: false,
                tasks: GuestTaskGroup::default(),
            }) as Box<dyn ComponentInstance>)
        })
    }
}

struct GuestState {
    instance: Option<InstanceHost>,
    wasi: crate::capability::WasiState,
    max_payload_bytes: usize,
    max_registrations: usize,
    next_registration: u32,
    registrations: BTreeMap<u32, cordis_core::EffectGuard>,
}

impl std::fmt::Debug for GuestState {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("GuestState")
            .field("fiber", &self.instance.as_ref().map(InstanceHost::fiber))
            .field("registrations", &self.registrations.len())
            .finish_non_exhaustive()
    }
}

impl GuestState {
    fn new(
        instance: Option<InstanceHost>,
        max_payload_bytes: usize,
        max_registrations: usize,
        wasi: crate::capability::WasiState,
    ) -> Self {
        Self {
            instance,
            wasi,
            max_payload_bytes,
            max_registrations,
            next_registration: 1,
            registrations: BTreeMap::new(),
        }
    }

    fn instance(&self) -> Result<&InstanceHost, wit::KernelError> {
        self.instance
            .as_ref()
            .ok_or(wit::KernelError::InactiveContext)
    }

    fn validate_context(&self, context: &wit::CallContext) -> Result<(), wit::KernelError> {
        let instance = self.instance()?;
        if context.fiber_id != instance.fiber().get()
            || context.effect_id != instance.effects().id().get()
        {
            return Err(wit::KernelError::InvalidArgument(
                "call context does not belong to this Store".to_owned(),
            ));
        }
        Ok(())
    }

    fn validate_payload(&self, payload: &[u8]) -> Result<(), wit::KernelError> {
        if payload.len() > self.max_payload_bytes {
            return Err(wit::KernelError::InvalidArgument(format!(
                "payload is {} bytes, limit is {} bytes",
                payload.len(),
                self.max_payload_bytes
            )));
        }
        Ok(())
    }

    async fn add_registration(
        &mut self,
        request: RegistrationRequest,
    ) -> Result<Resource<wit::Registration>, wit::KernelError> {
        if self.registrations.len() >= self.max_registrations {
            return Err(wit::KernelError::CapabilityDenied(format!(
                "registration limit {} exceeded",
                self.max_registrations
            )));
        }
        let instance = self.instance()?.clone();
        let guard = instance.register(request).await.map_err(to_kernel_error)?;
        let representation = self.next_registration;
        self.next_registration = self.next_registration.checked_add(1).ok_or_else(|| {
            wit::KernelError::Internal("registration identifier space exhausted".to_owned())
        })?;
        self.registrations.insert(representation, guard);
        Ok(Resource::new_own(representation))
    }
}

impl WasiView for StoreState<GuestState> {
    fn ctx(&mut self) -> WasiCtxView<'_> {
        let state = self.host_mut();
        WasiCtxView {
            ctx: &mut state.wasi.context,
            table: &mut state.wasi.table,
        }
    }
}

impl wit::HostRegistration for GuestState {
    async fn drop(&mut self, resource: Resource<wit::Registration>) -> wasmtime::Result<()> {
        if let Some(guard) = self.registrations.remove(&resource.rep()) {
            let _ = guard.dispose().await;
        }
        Ok(())
    }
}

impl wit::Host for GuestState {
    #[allow(clippy::unused_async_trait_impl)]
    async fn log(
        &mut self,
        context: wit::CallContext,
        level: String,
        message: String,
    ) -> wasmtime::Result<()> {
        self.validate_context(&context)
            .map_err(|error| wasmtime::Error::msg(format!("{error:?}")))?;
        self.instance()?.log(&level, &message);
        Ok(())
    }

    async fn call_service(
        &mut self,
        context: wit::CallContext,
        service: wit::ServiceId,
        method: u32,
        payload: Vec<u8>,
    ) -> wasmtime::Result<Result<Vec<u8>, wit::KernelError>> {
        if let Err(error) = self.validate_context(&context) {
            return Ok(Err(error));
        }
        if let Err(error) = self.validate_payload(&payload) {
            return Ok(Err(error));
        }
        let instance = match self.instance() {
            Ok(instance) => instance.clone(),
            Err(error) => return Ok(Err(error)),
        };
        let service = match service_from_wit(service) {
            Ok(service) => service,
            Err(error) => return Ok(Err(wit::KernelError::InvalidArgument(error.to_string()))),
        };
        let call = DynamicCall {
            service,
            method,
            payload,
        };
        let reply = match instance.call_service(call).await {
            Ok(reply) => reply,
            Err(error) => return Ok(Err(to_kernel_error(error))),
        };
        if let Err(error) = self.validate_payload(&reply) {
            return Ok(Err(error));
        }
        Ok(Ok(reply))
    }

    async fn provide_service(
        &mut self,
        context: wit::CallContext,
        service: wit::ServiceId,
    ) -> wasmtime::Result<Result<Resource<wit::Registration>, wit::KernelError>> {
        if let Err(error) = self.validate_context(&context) {
            return Ok(Err(error));
        }
        let service = match service_from_wit(service) {
            Ok(service) => service,
            Err(error) => return Ok(Err(wit::KernelError::InvalidArgument(error.to_string()))),
        };
        Ok(self
            .add_registration(RegistrationRequest::Provide(service))
            .await)
    }

    async fn register_listener(
        &mut self,
        context: wit::CallContext,
        event: wit::EventId,
        listener_id: u64,
        mode: wit::EventMode,
    ) -> wasmtime::Result<Result<Resource<wit::Registration>, wit::KernelError>> {
        if let Err(error) = self.validate_context(&context) {
            return Ok(Err(error));
        }
        let event = match event_from_wit(event) {
            Ok(event) => event,
            Err(error) => return Ok(Err(error)),
        };
        Ok(self
            .add_registration(RegistrationRequest::Listen {
                event,
                listener_id,
                mode: event_mode_from_wit(mode),
            })
            .await)
    }

    async fn dispatch_event(
        &mut self,
        context: wit::CallContext,
        event: wit::EventId,
        listener_id: u64,
        mode: wit::EventMode,
        payload: Vec<u8>,
        next_token: Option<u64>,
    ) -> wasmtime::Result<Result<wit::EventReply, wit::KernelError>> {
        if let Err(error) = self.validate_context(&context) {
            return Ok(Err(error));
        }
        if let Err(error) = self.validate_payload(&payload) {
            return Ok(Err(error));
        }
        let instance = match self.instance() {
            Ok(instance) => instance.clone(),
            Err(error) => return Ok(Err(error)),
        };
        let event = match event_from_wit(event) {
            Ok(event) => event,
            Err(error) => return Ok(Err(error)),
        };
        let call = EventCall {
            event,
            listener_id,
            mode: event_mode_from_wit(mode),
            payload,
            next_token,
        };
        let reply = match instance.dispatch_event(call).await {
            Ok(reply) => reply,
            Err(error) => return Ok(Err(to_kernel_error(error))),
        };
        let reply_payload = match &reply {
            EventReply::Continue(payload) | EventReply::Break(payload) => payload,
        };
        if let Err(error) = self.validate_payload(reply_payload) {
            return Ok(Err(error));
        }
        Ok(Ok(event_reply_to_wit(reply)))
    }
}

/// A running Store and generated typed guest exports.
struct WasmComponentInstance {
    engine: WasmEngine,
    descriptor: DynamicComponentDescriptor,
    store: Store<StoreState<GuestState>>,
    bindings: bindings::CordisPlugin,
    host: InstanceHost,
    active: bool,
    tasks: GuestTaskGroup,
}

impl WasmComponentInstance {
    fn call_context(&self) -> wit::CallContext {
        wit::CallContext {
            fiber_id: self.host.fiber().get(),
            effect_id: self.host.effects().id().get(),
        }
    }

    async fn force_cleanup(&mut self) -> Result<(), CordisError> {
        self.tasks.shutdown().await;
        self.store.data_mut().host_mut().registrations.clear();
        self.host
            .effects()
            .dispose()
            .await
            .map_err(|error| CordisError::DisposerFailed {
                message: error.to_string(),
            })
    }
}

impl ComponentInstance for WasmComponentInstance {
    fn activate(&mut self, config: Value) -> ComponentFuture<'_, ()> {
        Box::pin(async move {
            let payload = serde_json::to_vec(&config).map_err(|error| {
                CordisError::InvalidComponentConfig {
                    component: self.descriptor.name.to_string(),
                    path: String::new(),
                    message: error.to_string(),
                }
            })?;
            validate_payload_limit(&payload, self.store.data().host().max_payload_bytes)?;
            self.engine
                .prepare_call(&mut self.store)
                .map_err(wasm_error)?;
            let context = self.call_context();
            let result = AssertUnwindSafe(self.bindings.cordis_kernel_plugin().call_activate(
                &mut self.store,
                context,
                &payload,
            ))
            .catch_unwind()
            .await
            .map_err(|_| component_panic(&self.descriptor.name))?
            .map_err(|error| component_error(&self.descriptor.name, error))?;
            result.map_err(from_kernel_error)?;
            self.active = true;
            Ok(())
        })
    }

    fn deactivate(&mut self) -> ComponentFuture<'_, ()> {
        Box::pin(async move {
            let guest_result = if self.active {
                self.engine
                    .prepare_call(&mut self.store)
                    .map_err(wasm_error)?;
                let context = self.call_context();
                self.bindings
                    .cordis_kernel_plugin()
                    .call_deactivate(&mut self.store, context)
                    .await
                    .map_err(|error| component_error(&self.descriptor.name, error))?
                    .map_err(from_kernel_error)
            } else {
                Ok(())
            };
            self.active = false;
            let cleanup = self.force_cleanup().await;
            guest_result.and(cleanup)
        })
    }

    fn call_service(&mut self, call: DynamicCall) -> ComponentFuture<'_, Vec<u8>> {
        Box::pin(async move {
            require_active(self.active, &self.descriptor.name)?;
            validate_payload_limit(&call.payload, self.store.data().host().max_payload_bytes)?;
            self.engine
                .prepare_call(&mut self.store)
                .map_err(wasm_error)?;
            let service = service_to_wit(&call.service);
            let context = self.call_context();
            let result = self
                .bindings
                .cordis_kernel_plugin()
                .call_call_service(
                    &mut self.store,
                    context,
                    &service,
                    call.method,
                    &call.payload,
                )
                .await
                .map_err(|error| component_error(&self.descriptor.name, error))?
                .map_err(from_kernel_error)?;
            validate_payload_limit(&result, self.store.data().host().max_payload_bytes)?;
            Ok(result)
        })
    }

    fn call_event(&mut self, call: EventCall) -> ComponentFuture<'_, EventReply> {
        Box::pin(async move {
            require_active(self.active, &self.descriptor.name)?;
            validate_payload_limit(&call.payload, self.store.data().host().max_payload_bytes)?;
            self.engine
                .prepare_call(&mut self.store)
                .map_err(wasm_error)?;
            let event = event_to_wit(&call.event);
            let context = self.call_context();
            let result = self
                .bindings
                .cordis_kernel_plugin()
                .call_handle_event(
                    &mut self.store,
                    context,
                    &event,
                    call.listener_id,
                    event_mode_to_wit(call.mode),
                    &call.payload,
                    call.next_token,
                )
                .await
                .map_err(|error| component_error(&self.descriptor.name, error))?
                .map_err(from_kernel_error)?;
            let result = event_reply_from_wit(result);
            let payload = match &result {
                EventReply::Continue(payload) | EventReply::Break(payload) => payload,
            };
            validate_payload_limit(payload, self.store.data().host().max_payload_bytes)?;
            Ok(result)
        })
    }
}

/// Host-owned tasks are always aborted and joined before a Store is discarded.
#[derive(Debug, Default)]
pub struct GuestTaskGroup {
    tasks: Vec<JoinHandle<()>>,
}

impl GuestTaskGroup {
    pub fn spawn(&mut self, task: impl std::future::Future<Output = ()> + Send + 'static) {
        self.tasks.push(tokio::spawn(task));
    }

    pub fn len(&self) -> usize {
        self.tasks.len()
    }

    pub fn is_empty(&self) -> bool {
        self.tasks.is_empty()
    }

    pub async fn shutdown(&mut self) {
        for task in &self.tasks {
            task.abort();
        }
        for task in self.tasks.drain(..) {
            let _ = task.await;
        }
    }
}

async fn inspect_descriptor(
    engine: &WasmEngine,
    component: &Component,
    limits: &WasmLimits,
    wasi: &WasiCapabilities,
) -> Result<DynamicComponentDescriptor, WasmHostError> {
    let state = GuestState::new(
        None,
        limits.max_payload_bytes,
        limits.max_registrations,
        wasi.build()?,
    );
    let mut store = engine.new_store(state, limits)?;
    let linker = build_linker(engine.engine())?;
    let instance =
        bindings::CordisPlugin::instantiate_async(&mut store, component, &linker).await?;
    engine.prepare_call(&mut store)?;
    let descriptor = instance
        .cordis_kernel_plugin()
        .call_descriptor(&mut store)
        .await?;
    descriptor_from_wit(descriptor)
}

fn build_linker(
    engine: &wasmtime::Engine,
) -> Result<Linker<StoreState<GuestState>>, WasmHostError> {
    let mut linker = Linker::new(engine);
    bindings::CordisPlugin::add_to_linker::<_, wasmtime::component::HasSelf<GuestState>>(
        &mut linker,
        StoreState::host_mut,
    )?;
    wasmtime_wasi::p2::add_to_linker_async(&mut linker)?;
    Ok(linker)
}

fn descriptor_from_wit(
    descriptor: guest::PluginDescriptor,
) -> Result<DynamicComponentDescriptor, WasmHostError> {
    let config_value =
        serde_json::from_slice::<Value>(&descriptor.config_schema).map_err(|error| {
            WasmHostError::Descriptor {
                message: format!("invalid config schema JSON: {error}"),
            }
        })?;
    let config_schema =
        Schema::try_from(config_value).map_err(|error| WasmHostError::Descriptor {
            message: format!("config schema is not a JSON Schema object or boolean: {error}"),
        })?;
    Ok(DynamicComponentDescriptor {
        name: descriptor.name.into(),
        version: descriptor.version.into(),
        kernel_abi: descriptor.wit_version.into(),
        injects: descriptor
            .inject
            .into_iter()
            .map(service_from_wit)
            .collect::<Result<Vec<_>, _>>()?
            .into_iter()
            .map(InjectSpec::required)
            .collect(),
        provides: descriptor
            .provide
            .into_iter()
            .map(service_from_wit)
            .collect::<Result<_, _>>()?,
        config_schema,
        capabilities: descriptor
            .capabilities
            .into_iter()
            .map(Capability::new)
            .collect(),
    })
}

fn validate_descriptor(
    descriptor: &DynamicComponentDescriptor,
    policy: &ArtifactPolicy,
) -> Result<(), WasmHostError> {
    if descriptor.kernel_abi.as_ref() != policy.kernel_abi {
        return Err(WasmHostError::KernelAbiMismatch {
            expected: policy.kernel_abi.clone(),
            actual: descriptor.kernel_abi.to_string(),
        });
    }
    for capability in &descriptor.capabilities {
        if !policy.allowed_capabilities.contains(capability) {
            return Err(WasmHostError::CapabilityDenied {
                capability: capability.as_str().to_owned(),
            });
        }
    }
    Ok(())
}

fn validate_wasi_imports(
    engine: &wasmtime::Engine,
    component: &Component,
    descriptor: &DynamicComponentDescriptor,
    policy: &ArtifactPolicy,
) -> Result<(), WasmHostError> {
    for (import, _) in component.component_type().imports(engine) {
        let Some(capability) = capability_for_wasi_import(import) else {
            continue;
        };
        let capability = Capability::new(capability);
        if !descriptor.capabilities.contains(&capability) {
            return Err(WasmHostError::Descriptor {
                message: format!(
                    "WASI import `{import}` requires undeclared capability `{}`",
                    capability.as_str()
                ),
            });
        }
        if !policy.allowed_capabilities.contains(&capability) {
            return Err(WasmHostError::CapabilityDenied {
                capability: capability.as_str().to_owned(),
            });
        }
    }
    Ok(())
}

fn capability_for_wasi_import(import: &str) -> Option<&str> {
    if import.starts_with("wasi:io/")
        || import.starts_with("wasi:cli/")
        || import.starts_with("wasi:clocks/monotonic-clock")
    {
        None
    } else if import.starts_with("wasi:filesystem/") {
        Some("filesystem")
    } else if import.starts_with("wasi:sockets/") || import.starts_with("wasi:http/") {
        Some("network")
    } else if import.starts_with("wasi:random/") {
        Some("random")
    } else if import.starts_with("wasi:clocks/wall-clock") {
        Some("clock:wall")
    } else if import.starts_with("wasi:") {
        Some(import)
    } else {
        None
    }
}

fn service_from_wit(service: wit::ServiceId) -> Result<ServiceId, WasmHostError> {
    let hash = hash_from_bytes(&service.abi_hash)?;
    Ok(ServiceId::new(service.name, hash))
}

fn service_to_wit(service: &ServiceId) -> wit::ServiceId {
    wit::ServiceId {
        name: service.name().to_owned(),
        abi_hash: service.abi_hash().to_vec(),
    }
}

fn event_from_wit(event: wit::EventId) -> Result<cordis_core::EventId, wit::KernelError> {
    let hash = <[u8; 32]>::try_from(event.abi_hash.as_slice()).map_err(|_| {
        wit::KernelError::InvalidArgument("event ABI hash must contain 32 bytes".to_owned())
    })?;
    Ok(cordis_core::EventId::new(event.name, hash))
}

fn event_to_wit(event: &cordis_core::EventId) -> wit::EventId {
    wit::EventId {
        name: event.name().to_owned(),
        abi_hash: event.abi_hash().to_vec(),
    }
}

fn hash_from_bytes(bytes: &[u8]) -> Result<[u8; 32], WasmHostError> {
    <[u8; 32]>::try_from(bytes).map_err(|_| WasmHostError::Descriptor {
        message: "service ABI hash must contain 32 bytes".to_owned(),
    })
}

fn event_mode_from_wit(mode: wit::EventMode) -> EventMode {
    match mode {
        wit::EventMode::Emit => EventMode::Emit,
        wit::EventMode::Parallel => EventMode::Parallel,
        wit::EventMode::Serial => EventMode::Serial,
        wit::EventMode::Bail => EventMode::Bail,
        wit::EventMode::Waterfall => EventMode::Waterfall,
    }
}

fn event_mode_to_wit(mode: EventMode) -> wit::EventMode {
    match mode {
        EventMode::Emit => wit::EventMode::Emit,
        EventMode::Parallel => wit::EventMode::Parallel,
        EventMode::Serial => wit::EventMode::Serial,
        EventMode::Bail => wit::EventMode::Bail,
        EventMode::Waterfall => wit::EventMode::Waterfall,
    }
}

fn event_reply_from_wit(reply: wit::EventReply) -> EventReply {
    match reply {
        wit::EventReply::ContinueValue(payload) => EventReply::Continue(payload),
        wit::EventReply::BreakValue(payload) => EventReply::Break(payload),
    }
}

fn event_reply_to_wit(reply: EventReply) -> wit::EventReply {
    match reply {
        EventReply::Continue(payload) => wit::EventReply::ContinueValue(payload),
        EventReply::Break(payload) => wit::EventReply::BreakValue(payload),
    }
}

fn require_active(active: bool, component: &str) -> Result<(), CordisError> {
    if active {
        Ok(())
    } else {
        Err(CordisError::ComponentFailed {
            component: component.to_owned(),
            message: "instance is not active".to_owned(),
        })
    }
}

fn validate_payload_limit(payload: &[u8], limit: usize) -> Result<(), CordisError> {
    if payload.len() <= limit {
        Ok(())
    } else {
        Err(CordisError::PayloadLimitExceeded {
            actual: payload.len(),
            limit,
        })
    }
}

fn to_kernel_error(error: CordisError) -> wit::KernelError {
    match error {
        CordisError::InactiveDependency { key } => {
            wit::KernelError::InactiveDependency(key.to_string())
        }
        CordisError::UndeclaredDependency { service } => {
            wit::KernelError::UndeclaredDependency(service.to_string())
        }
        CordisError::CapabilityDenied { capability } => {
            wit::KernelError::CapabilityDenied(capability)
        }
        other => wit::KernelError::Internal(other.to_string()),
    }
}

#[allow(clippy::needless_pass_by_value)]
fn from_kernel_error(error: wit::KernelError) -> CordisError {
    CordisError::ComponentFailed {
        component: "wasm-guest".to_owned(),
        message: format!("{error:?}"),
    }
}

#[allow(clippy::needless_pass_by_value)]
fn wasm_error(error: WasmHostError) -> CordisError {
    CordisError::ComponentFailed {
        component: "wasm-host".to_owned(),
        message: error.to_string(),
    }
}

#[allow(clippy::needless_pass_by_value)]
fn component_error(component: &str, error: wasmtime::Error) -> CordisError {
    CordisError::ComponentFailed {
        component: component.to_owned(),
        message: error.to_string(),
    }
}

fn component_panic(component: &str) -> CordisError {
    CordisError::ComponentFailed {
        component: component.to_owned(),
        message: "host panic while polling guest call".to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use cordis_core::{Context, Disposer, EffectScope, KernelHost, ProviderKey, Runtime};
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

    #[tokio::test]
    async fn task_group_aborts_and_joins_every_task() {
        let dropped = Arc::new(AtomicBool::new(false));
        let dropped_in_task = dropped.clone();
        let mut tasks = GuestTaskGroup::default();
        tasks.spawn(async move {
            struct OnDrop(Arc<AtomicBool>);
            impl Drop for OnDrop {
                fn drop(&mut self) {
                    self.0.store(true, Ordering::SeqCst);
                }
            }
            let _guard = OnDrop(dropped_in_task);
            std::future::pending::<()>().await;
        });
        tokio::task::yield_now().await;
        tasks.shutdown().await;
        assert!(tasks.is_empty());
        assert!(dropped.load(Ordering::SeqCst));
    }

    #[test]
    fn capability_policy_rejects_unlisted_request() {
        let descriptor = DynamicComponentDescriptor {
            name: "guest".into(),
            version: "0.1.0".into(),
            kernel_abi: "0.1".into(),
            injects: Vec::new(),
            provides: Vec::new(),
            config_schema: true.into(),
            capabilities: BTreeSet::from([Capability::new("fs:read")]),
        };
        assert!(matches!(
            validate_descriptor(&descriptor, &ArtifactPolicy::default()),
            Err(WasmHostError::CapabilityDenied { .. })
        ));
    }

    #[test]
    fn kernel_abi_and_payload_limits_are_rejected_before_routing() {
        let descriptor = DynamicComponentDescriptor {
            name: "guest".into(),
            version: "0.1.0".into(),
            kernel_abi: "1.0".into(),
            injects: Vec::new(),
            provides: Vec::new(),
            config_schema: true.into(),
            capabilities: BTreeSet::new(),
        };
        assert!(matches!(
            validate_descriptor(&descriptor, &ArtifactPolicy::default()),
            Err(WasmHostError::KernelAbiMismatch { .. })
        ));
        assert_eq!(
            validate_payload_limit(&[0; 5], 4),
            Err(CordisError::PayloadLimitExceeded {
                actual: 5,
                limit: 4
            })
        );
    }

    #[test]
    fn sensitive_wasi_imports_require_explicit_capabilities() {
        assert_eq!(
            capability_for_wasi_import("wasi:filesystem/types@0.2.9"),
            Some("filesystem")
        );
        assert_eq!(
            capability_for_wasi_import("wasi:sockets/tcp@0.2.9"),
            Some("network")
        );
        assert_eq!(
            capability_for_wasi_import("wasi:random/random@0.2.9"),
            Some("random")
        );
        assert_eq!(
            capability_for_wasi_import("wasi:clocks/wall-clock@0.2.9"),
            Some("clock:wall")
        );
        assert_eq!(
            capability_for_wasi_import("wasi:clocks/monotonic-clock@0.2.9"),
            None
        );
        assert_eq!(capability_for_wasi_import("cordis:kernel/host@0.1.0"), None);
    }

    #[derive(Default)]
    struct FixtureHost {
        active_registrations: Arc<AtomicUsize>,
    }

    impl KernelHost for FixtureHost {
        fn log(&self, _: cordis_core::FiberId, _: &str, _: &str) {}

        fn call_service(
            &self,
            _: cordis_core::FiberId,
            _: DynamicCall,
        ) -> ComponentFuture<'_, Vec<u8>> {
            Box::pin(async {
                rmp_serde::to_vec_named(&7_u64).map_err(|error| CordisError::ServiceEncodeFailed {
                    message: error.to_string(),
                })
            })
        }

        fn dispatch_event(
            &self,
            _: cordis_core::FiberId,
            call: EventCall,
        ) -> ComponentFuture<'_, EventReply> {
            Box::pin(async move { Ok(EventReply::Continue(call.payload)) })
        }

        fn provide_service(
            &self,
            _: cordis_core::FiberId,
            _: cordis_core::ProviderKey,
            scope: EffectScope,
        ) -> ComponentFuture<'_, ()> {
            let registrations = self.active_registrations.clone();
            Box::pin(async move {
                registrations.fetch_add(1, Ordering::SeqCst);
                scope.defer(Disposer::new(move || async move {
                    registrations.fetch_sub(1, Ordering::SeqCst);
                    Ok(())
                }))?;
                Ok(())
            })
        }

        fn register_listener(
            &self,
            _: cordis_core::FiberId,
            _: cordis_core::EventId,
            _: u64,
            _: cordis_core::EventMode,
            scope: EffectScope,
        ) -> ComponentFuture<'_, ()> {
            let registrations = self.active_registrations.clone();
            Box::pin(async move {
                registrations.fetch_add(1, Ordering::SeqCst);
                scope.defer(Disposer::new(move || async move {
                    registrations.fetch_sub(1, Ordering::SeqCst);
                    Ok(())
                }))?;
                Ok(())
            })
        }
    }

    #[tokio::test]
    async fn guest_sdk_artifacts_run_end_to_end() -> Result<(), Box<dyn std::error::Error>> {
        let Ok(fixtures) = std::env::var("CORDIS_GUEST_FIXTURES") else {
            return Ok(());
        };
        let engine = WasmEngine::new()?;
        let limits = WasmLimits::default();
        let policy = ArtifactPolicy::default();
        let provider = Arc::new(
            WasmComponentFactory::from_bytes(
                engine.clone(),
                std::fs::read(std::path::Path::new(&fixtures).join("wasm_counter_provider.wasm"))?,
                limits.clone(),
                policy.clone(),
            )
            .await?,
        );
        let consumer = Arc::new(
            WasmComponentFactory::from_bytes(
                engine,
                std::fs::read(std::path::Path::new(&fixtures).join("wasm_counter_consumer.wasm"))?,
                limits,
                policy,
            )
            .await?,
        );
        assert_eq!(provider.descriptor().provides[0].name(), "example.counter");
        assert_eq!(
            consumer.descriptor().injects[0].service.name(),
            "example.counter"
        );

        let runtime = Runtime::start();
        let handle = runtime.handle();
        let realm = handle.allocate_realm().await?;
        let service = provider.descriptor().provides[0].clone();
        let root = handle.create_fiber(None).await?;
        let provider_context = Context::root(root).isolate(service.clone(), realm);
        let host = Arc::new(FixtureHost::default());
        let registrations = host.active_registrations.clone();
        let mounted_provider = handle
            .mount_dynamic(
                None,
                Some(&provider_context),
                provider.clone(),
                host.clone(),
                serde_json::json!({}),
            )
            .await?;
        mounted_provider.await_active().await?;
        assert_eq!(registrations.load(Ordering::SeqCst), 1);
        let reply = mounted_provider
            .call_service(DynamicCall {
                service: service.clone(),
                method: 1,
                payload: rmp_serde::to_vec_named(&2_u64)?,
            })
            .await?;
        assert_eq!(rmp_serde::from_slice::<u64>(&reply)?, 2);

        handle
            .provide(
                ProviderKey::new(service.clone(), realm),
                mounted_provider.fiber(),
            )
            .await?;
        let base_context = Context::root(mounted_provider.fiber()).isolate(service, realm);
        let mounted_consumer = handle
            .mount_dynamic(
                None,
                Some(&base_context),
                consumer,
                host,
                serde_json::json!({}),
            )
            .await?;
        mounted_consumer.await_active().await?;
        mounted_consumer.retire().await?;
        mounted_provider.retire().await?;
        handle.retire_fiber(root).await?;
        assert_eq!(registrations.load(Ordering::SeqCst), 0);
        runtime.shutdown().await?;
        Ok(())
    }
}
