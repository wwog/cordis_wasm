//! Wasmtime Component Model integration for Cordis.

mod capability;
mod hmr;
mod loader;
mod runtime;

pub use capability::{WasiCapabilities, WasiPreopen};
pub use hmr::{
    ArtifactCache, ArtifactHash, CacheMetrics, CompiledArtifact, EntryReload, FiberReloadRuntime,
    HmrError, HmrFuture, HmrManager, HmrWatcher, ReloadReport, ReloadRuntime, ReloadStatus,
};
pub use loader::{
    BuiltinRegistry, CheckReport, WasmApplication, WasmEntryDriver, check_entries,
    check_entries_with_builtins,
};
pub use runtime::{ArtifactPolicy, GuestTaskGroup, WasmComponentFactory};

use wasmtime::component::Component;
use wasmtime::{Config, Engine, Store, StoreLimits, StoreLimitsBuilder};

/// Generated host and guest types for the versioned Cordis kernel world.
pub mod bindings {
    wasmtime::component::bindgen!({
        path: "wit",
        world: "cordis-plugin",
        imports: { default: async | trappable },
        exports: { default: async },
    });
}

/// Component compiler configuration shared by all plugin stores.
#[derive(Clone, Debug)]
pub struct WasmEngine {
    engine: Engine,
}

/// Per-plugin resource and execution budgets.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct WasmLimits {
    pub fuel_per_call: u64,
    pub epoch_deadline_ticks: u64,
    pub max_memory_bytes: usize,
    pub max_table_elements: usize,
    pub max_instances: usize,
    pub max_tables: usize,
    pub max_memories: usize,
    pub max_registrations: usize,
    pub max_payload_bytes: usize,
}

impl Default for WasmLimits {
    fn default() -> Self {
        Self {
            fuel_per_call: 10_000_000,
            epoch_deadline_ticks: 1,
            max_memory_bytes: 64 * 1024 * 1024,
            max_table_elements: 10_000,
            max_instances: 32,
            max_tables: 32,
            max_memories: 32,
            max_registrations: 10_000,
            max_payload_bytes: 1024 * 1024,
        }
    }
}

/// Store-owned host state and enforceable budgets.
#[derive(Debug)]
pub struct StoreState<T> {
    host: T,
    limiter: StoreLimits,
    fuel_per_call: u64,
    epoch_deadline_ticks: u64,
    active_registrations: usize,
    max_registrations: usize,
}

impl<T> StoreState<T> {
    pub const fn host(&self) -> &T {
        &self.host
    }

    pub const fn host_mut(&mut self) -> &mut T {
        &mut self.host
    }

    pub const fn active_registrations(&self) -> usize {
        self.active_registrations
    }

    /// Reserves one host-tracked registration before exposing it to a guest.
    ///
    /// # Errors
    ///
    /// Returns [`WasmHostError::RegistrationLimitExceeded`] at the configured limit.
    pub fn reserve_registration(&mut self) -> Result<(), WasmHostError> {
        if self.active_registrations == self.max_registrations {
            return Err(WasmHostError::RegistrationLimitExceeded {
                limit: self.max_registrations,
            });
        }
        self.active_registrations += 1;
        Ok(())
    }

    /// Releases one previously reserved registration.
    ///
    /// # Errors
    ///
    /// Returns [`WasmHostError::RegistrationCountUnderflow`] for an unmatched release.
    pub fn release_registration(&mut self) -> Result<(), WasmHostError> {
        self.active_registrations = self
            .active_registrations
            .checked_sub(1)
            .ok_or(WasmHostError::RegistrationCountUnderflow)?;
        Ok(())
    }
}

impl WasmEngine {
    /// Creates an async Component Model engine without ambient WASI capabilities.
    ///
    /// # Errors
    ///
    /// Returns [`WasmHostError::Engine`] if Wasmtime rejects the configuration.
    pub fn new() -> Result<Self, WasmHostError> {
        let mut config = Config::new();
        config.wasm_component_model(true);
        config.consume_fuel(true);
        config.epoch_interruption(true);
        Ok(Self {
            engine: Engine::new(&config)?,
        })
    }

    pub const fn engine(&self) -> &Engine {
        &self.engine
    }

    /// Compiles and validates one WebAssembly component.
    ///
    /// # Errors
    ///
    /// Returns [`WasmHostError::Engine`] for invalid bytes or a compilation failure.
    pub fn compile(&self, bytes: impl AsRef<[u8]>) -> Result<Component, WasmHostError> {
        Component::new(&self.engine, bytes).map_err(WasmHostError::Engine)
    }

    /// Creates a limited Store without adding any ambient WASI capabilities.
    ///
    /// # Errors
    ///
    /// Returns [`WasmHostError::Engine`] if the initial fuel cannot be configured.
    pub fn new_store<T>(
        &self,
        host: T,
        limits: &WasmLimits,
    ) -> Result<Store<StoreState<T>>, WasmHostError> {
        let limiter = StoreLimitsBuilder::new()
            .memory_size(limits.max_memory_bytes)
            .table_elements(limits.max_table_elements)
            .instances(limits.max_instances)
            .tables(limits.max_tables)
            .memories(limits.max_memories)
            .trap_on_grow_failure(true)
            .build();
        let mut store = Store::new(
            &self.engine,
            StoreState {
                host,
                limiter,
                fuel_per_call: limits.fuel_per_call,
                epoch_deadline_ticks: limits.epoch_deadline_ticks,
                active_registrations: 0,
                max_registrations: limits.max_registrations,
            },
        );
        store.limiter(|state| &mut state.limiter);
        self.prepare_call(&mut store)?;
        Ok(store)
    }

    /// Rearms fuel and the epoch deadline immediately before a guest call.
    ///
    /// # Errors
    ///
    /// Returns [`WasmHostError::Engine`] when Wasmtime rejects the fuel budget.
    pub fn prepare_call<T>(&self, store: &mut Store<StoreState<T>>) -> Result<(), WasmHostError> {
        let fuel = store.data().fuel_per_call;
        let epoch_ticks = store.data().epoch_deadline_ticks;
        store.set_fuel(fuel)?;
        store.set_epoch_deadline(epoch_ticks);
        store.epoch_deadline_trap();
        Ok(())
    }
}

#[derive(Debug, thiserror::Error)]
pub enum WasmHostError {
    #[error("wasmtime component error: {0}")]
    Engine(#[from] wasmtime::Error),

    #[error("guest registration limit {limit} exceeded")]
    RegistrationLimitExceeded { limit: usize },

    #[error("guest registration count underflow")]
    RegistrationCountUnderflow,

    #[error("invalid component descriptor: {message}")]
    Descriptor { message: String },

    #[error("kernel ABI mismatch: expected {expected}, got {actual}")]
    KernelAbiMismatch { expected: String, actual: String },

    #[error("component capability `{capability}` is denied")]
    CapabilityDenied { capability: String },

    #[error("invalid WASI capability: {message}")]
    Capability { message: String },
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use wasmtime::component::Linker;

    #[test]
    fn packaged_host_wit_matches_the_guest_canonical_source() {
        assert_eq!(
            include_str!("../../cordis-guest/wit/kernel.wit"),
            include_str!("../wit/kernel.wit")
        );
    }

    const HOST_IMPORT_COMPONENT: &str = r#"
        (component
            (import "record" (func $record (param "value" u32)))

            (core module $module
                (import "" "record" (func $record (param i32)))
                (func (export "run") (param i32) (result i32)
                    local.get 0
                    call $record
                    local.get 0
                    i32.const 1
                    i32.add)
            )

            (core func $record-lowered (canon lower (func $record)))
            (core instance $instance
                (instantiate $module
                    (with "" (instance
                        (export "record" (func $record-lowered))
                    ))
                )
            )

            (func (export "run") (param "value" u32) (result u32)
                (canon lift (core func $instance "run")))
        )
    "#;

    const RESOURCE_COMPONENT: &str = r#"
        (component
            (import "registration" (type $registration (sub resource)))
            (import "open" (func $open (result (own $registration))))

            (core module $module
                (import "" "open" (func $open (result i32)))
                (import "" "drop-registration" (func $drop-registration (param i32)))
                (func (export "retain")
                    call $open
                    drop)
                (func (export "release")
                    call $open
                    call $drop-registration)
            )

            (core func $open-lowered (canon lower (func $open)))
            (core func $drop-registration (canon resource.drop $registration))
            (core instance $instance
                (instantiate $module
                    (with "" (instance
                        (export "open" (func $open-lowered))
                        (export "drop-registration" (func $drop-registration))
                    ))
                )
            )
            (func (export "retain") (canon lift (core func $instance "retain")))
            (func (export "release") (canon lift (core func $instance "release")))
        )
    "#;

    const INTERRUPT_COMPONENT: &str = r#"
        (component
            (core module $module
                (func (export "spin")
                    (loop $again
                        i32.const 1
                        drop
                        br $again))
            )
            (core instance $instance (instantiate $module))
            (func (export "spin") (canon lift (core func $instance "spin")))
        )
    "#;

    const MEMORY_COMPONENT: &str = r#"
        (component
            (core module $module
                (memory 1)
                (func (export "grow") (result i32)
                    i32.const 1
                    memory.grow)
            )
            (core instance $instance (instantiate $module))
            (func (export "grow") (result s32)
                (canon lift (core func $instance "grow")))
        )
    "#;

    const REENTRANT_COMPONENT: &str = r#"
        (component
            (import "reenter" (func $reenter (param "depth" u32) (result u32)))
            (core module $module
                (import "" "reenter" (func $reenter (param i32) (result i32)))
                (func (export "dispatch") (param i32) (result i32)
                    local.get 0
                    i32.eqz
                    if (result i32)
                        i32.const 1
                    else
                        local.get 0
                        i32.const 1
                        i32.sub
                        call $reenter
                        i32.const 1
                        i32.add
                    end)
            )
            (core func $reenter-lowered (canon lower (func $reenter)))
            (core instance $instance
                (instantiate $module
                    (with "" (instance
                        (export "reenter" (func $reenter-lowered))
                    ))
                )
            )
            (func (export "dispatch") (param "depth" u32) (result u32)
                (canon lift (core func $instance "dispatch")))
        )
    "#;

    const CANCELLATION_COMPONENT: &str = r#"
        (component
            (import "wait" (func $wait))
            (core module $module
                (import "" "wait" (func $wait))
                (func (export "outer")
                    call $wait)
                (func (export "probe") (result i32)
                    i32.const 9)
            )
            (core func $wait-lowered (canon lower (func $wait)))
            (core instance $instance
                (instantiate $module
                    (with "" (instance
                        (export "wait" (func $wait-lowered))
                    ))
                )
            )
            (func (export "outer") (canon lift (core func $instance "outer")))
            (func (export "probe") (result u32)
                (canon lift (core func $instance "probe")))
        )
    "#;

    #[derive(Debug)]
    struct HostState {
        values: Vec<u32>,
        drops: Arc<AtomicUsize>,
    }

    impl Drop for HostState {
        fn drop(&mut self) {
            self.drops.fetch_add(1, Ordering::SeqCst);
        }
    }

    #[tokio::test]
    async fn component_calls_typed_host_import_and_guest_export()
    -> Result<(), Box<dyn std::error::Error>> {
        let wasm = WasmEngine::new()?;
        let bytes = wat::parse_str(HOST_IMPORT_COMPONENT)?;
        let component = wasm.compile(bytes)?;
        let drops = Arc::new(AtomicUsize::new(0));
        let mut store = Store::new(
            wasm.engine(),
            HostState {
                values: Vec::new(),
                drops: drops.clone(),
            },
        );
        store.set_fuel(u64::MAX)?;
        store.set_epoch_deadline(u64::MAX);
        let mut linker = Linker::<HostState>::new(wasm.engine());
        linker
            .root()
            .func_wrap_async("record", |mut store, (value,): (u32,)| {
                Box::new(async move {
                    store.data_mut().values.push(value);
                    Ok(())
                })
            })?;

        let instance = linker.instantiate_async(&mut store, &component).await?;
        let run = instance.get_typed_func::<(u32,), (u32,)>(&mut store, "run")?;
        assert_eq!(run.call_async(&mut store, (41,)).await?, (42,));
        assert_eq!(store.data().values, vec![41]);

        drop(store);
        assert_eq!(drops.load(Ordering::SeqCst), 1);
        Ok(())
    }

    #[derive(Debug)]
    struct RegistrationToken;

    #[derive(Debug)]
    struct ResourceState {
        drops: Arc<std::sync::Mutex<Vec<u32>>>,
        next_representation: u32,
    }

    #[tokio::test]
    async fn resource_destructor_requires_explicit_guest_drop()
    -> Result<(), Box<dyn std::error::Error>> {
        use wasmtime::component::{Resource, ResourceType};

        let wasm = WasmEngine::new()?;
        let component = wasm.compile(wat::parse_str(RESOURCE_COMPONENT)?)?;
        let drops = Arc::new(std::sync::Mutex::new(Vec::new()));
        let mut store = Store::new(
            wasm.engine(),
            ResourceState {
                drops: drops.clone(),
                next_representation: 1,
            },
        );
        store.set_fuel(u64::MAX)?;
        store.set_epoch_deadline(u64::MAX);
        let mut linker = Linker::<ResourceState>::new(wasm.engine());
        linker.root().resource(
            "registration",
            ResourceType::host::<RegistrationToken>(),
            |store, representation| {
                store.data().drops.lock().unwrap().push(representation);
                Ok(())
            },
        )?;
        linker.root().func_wrap("open", |mut store, (): ()| {
            let representation = store.data().next_representation;
            store.data_mut().next_representation += 1;
            Ok((Resource::<RegistrationToken>::new_own(representation),))
        })?;

        let instance = linker.instantiate_async(&mut store, &component).await?;
        let retain = instance.get_typed_func::<(), ()>(&mut store, "retain")?;
        let release = instance.get_typed_func::<(), ()>(&mut store, "release")?;
        release.call_async(&mut store, ()).await?;
        assert_eq!(*drops.lock().unwrap(), vec![1]);

        retain.call_async(&mut store, ()).await?;
        assert_eq!(*drops.lock().unwrap(), vec![1]);

        drop(store);
        assert_eq!(*drops.lock().unwrap(), vec![1]);
        Ok(())
    }

    #[tokio::test]
    async fn fuel_interrupts_unbounded_guest_code() -> Result<(), Box<dyn std::error::Error>> {
        let wasm = WasmEngine::new()?;
        let component = wasm.compile(wat::parse_str(INTERRUPT_COMPONENT)?)?;
        let limits = WasmLimits {
            fuel_per_call: 100,
            epoch_deadline_ticks: u64::MAX,
            ..WasmLimits::default()
        };
        let mut store = wasm.new_store((), &limits)?;
        let instance = Linker::new(wasm.engine())
            .instantiate_async(&mut store, &component)
            .await?;
        let spin = instance.get_typed_func::<(), ()>(&mut store, "spin")?;

        let error = spin.call_async(&mut store, ()).await.unwrap_err();
        assert_eq!(
            error.downcast_ref::<wasmtime::Trap>(),
            Some(&wasmtime::Trap::OutOfFuel)
        );
        Ok(())
    }

    #[tokio::test]
    async fn epoch_deadline_interrupts_guest_code() -> Result<(), Box<dyn std::error::Error>> {
        let wasm = WasmEngine::new()?;
        let component = wasm.compile(wat::parse_str(INTERRUPT_COMPONENT)?)?;
        let limits = WasmLimits {
            fuel_per_call: u64::MAX,
            epoch_deadline_ticks: 1,
            ..WasmLimits::default()
        };
        let mut store = wasm.new_store((), &limits)?;
        let instance = Linker::new(wasm.engine())
            .instantiate_async(&mut store, &component)
            .await?;
        let spin = instance.get_typed_func::<(), ()>(&mut store, "spin")?;
        wasm.engine().increment_epoch();

        let error = spin.call_async(&mut store, ()).await.unwrap_err();
        assert_eq!(
            error.downcast_ref::<wasmtime::Trap>(),
            Some(&wasmtime::Trap::Interrupt)
        );
        Ok(())
    }

    #[tokio::test]
    async fn memory_growth_obeys_store_limit() -> Result<(), Box<dyn std::error::Error>> {
        let wasm = WasmEngine::new()?;
        let component = wasm.compile(wat::parse_str(MEMORY_COMPONENT)?)?;
        let limits = WasmLimits {
            max_memory_bytes: 64 * 1024,
            ..WasmLimits::default()
        };
        let mut store = wasm.new_store((), &limits)?;
        let instance = Linker::new(wasm.engine())
            .instantiate_async(&mut store, &component)
            .await?;
        let grow = instance.get_typed_func::<(), (i32,)>(&mut store, "grow")?;

        assert!(grow.call_async(&mut store, ()).await.is_err());
        Ok(())
    }

    #[test]
    fn registration_budget_rejects_excess_and_underflow() {
        let wasm = WasmEngine::new().unwrap();
        let limits = WasmLimits {
            max_registrations: 1,
            ..WasmLimits::default()
        };
        let mut store = wasm.new_store((), &limits).unwrap();

        store.data_mut().reserve_registration().unwrap();
        assert!(matches!(
            store.data_mut().reserve_registration(),
            Err(WasmHostError::RegistrationLimitExceeded { limit: 1 })
        ));
        store.data_mut().release_registration().unwrap();
        assert!(matches!(
            store.data_mut().release_registration(),
            Err(WasmHostError::RegistrationCountUnderflow)
        ));
    }

    #[derive(Default)]
    struct ReentryState {
        dispatch: Option<wasmtime::component::TypedFunc<(u32,), (u32,)>>,
    }

    #[tokio::test]
    async fn same_instance_onion_reentry_is_supported() -> Result<(), Box<dyn std::error::Error>> {
        let wasm = WasmEngine::new()?;
        let component = wasm.compile(wat::parse_str(REENTRANT_COMPONENT)?)?;
        let mut store = Store::new(wasm.engine(), ReentryState::default());
        store.set_fuel(u64::MAX)?;
        store.set_epoch_deadline(u64::MAX);
        let mut linker = Linker::<ReentryState>::new(wasm.engine());
        linker
            .root()
            .func_wrap_async("reenter", |mut store, (depth,): (u32,)| {
                Box::new(async move {
                    let dispatch = store.data().dispatch.expect("dispatch export is set");
                    dispatch.call_async(&mut store, (depth,)).await
                })
            })?;
        let instance = linker.instantiate_async(&mut store, &component).await?;
        let dispatch = instance.get_typed_func::<(u32,), (u32,)>(&mut store, "dispatch")?;
        store.data_mut().dispatch = Some(dispatch);

        assert_eq!(dispatch.call_async(&mut store, (3,)).await?, (4,));
        Ok(())
    }

    #[tokio::test]
    async fn dropping_in_flight_call_leaves_instance_entered()
    -> Result<(), Box<dyn std::error::Error>> {
        let wasm = WasmEngine::new()?;
        let component = wasm.compile(wat::parse_str(CANCELLATION_COMPONENT)?)?;
        let mut store = Store::new(wasm.engine(), ());
        store.set_fuel(u64::MAX)?;
        store.set_epoch_deadline(u64::MAX);
        let mut linker = Linker::<()>::new(wasm.engine());
        linker.root().func_wrap_async("wait", |_store, (): ()| {
            Box::new(async move {
                std::future::pending::<()>().await;
                Ok(())
            })
        })?;
        let instance = linker.instantiate_async(&mut store, &component).await?;
        let outer = instance.get_typed_func::<(), ()>(&mut store, "outer")?;
        let probe = instance.get_typed_func::<(), (u32,)>(&mut store, "probe")?;

        assert!(
            tokio::time::timeout(
                std::time::Duration::from_millis(10),
                outer.call_async(&mut store, ()),
            )
            .await
            .is_err()
        );
        let error = probe.call_async(&mut store, ()).await.unwrap_err();
        assert_eq!(
            error.downcast_ref::<wasmtime::Trap>(),
            Some(&wasmtime::Trap::CannotEnterComponent)
        );
        Ok(())
    }
}
