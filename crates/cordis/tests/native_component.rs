use cordis::{
    Component, ComponentContext, ComponentDefinition, Context, CordisError, Disposer, EffectSet,
    EventSpec, EventTarget, FiberState, ListenerOptions, MethodContext, MethodFiberRuntime,
    NativeServiceRegistry, ProviderKey, Runtime, RuntimeHandle, ServiceCallError,
    ServiceDispatcher, ServiceFuture, ServiceId, ServiceSpec,
};
use schemars::JsonSchema;
use serde::Deserialize;
use std::ops::ControlFlow;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

#[cordis::service(name = "app.logger")]
#[allow(dead_code)]
trait Logger {
    async fn log(&self, message: String) -> Result<(), String>;
}

#[cordis::service(name = "app.stable-wire")]
#[allow(dead_code)]
trait StableWireOne {
    async fn first(&self, value: u64) -> Result<String, String>;
    async fn second(&self) -> Result<(), String>;
}

#[cordis::service(name = "app.stable-wire")]
#[allow(dead_code)]
trait StableWireTwo {
    async fn second(&self) -> Result<(), String>;

    /// Documentation and argument names are not part of the wire ABI.
    async fn first(&self, renamed: u64) -> Result<String, String>;
}

#[derive(Debug, Default)]
struct RecordingLogger {
    messages: Mutex<Vec<String>>,
}

impl Logger for RecordingLogger {
    fn log(&self, message: String) -> impl Future<Output = Result<(), String>> + Send {
        let result = if message.is_empty() {
            Err("message cannot be empty".to_owned())
        } else {
            self.messages.lock().unwrap().push(message);
            Ok(())
        };
        std::future::ready(result)
    }
}

#[derive(Debug)]
struct WrongLoggerDispatcher;

impl ServiceDispatcher for WrongLoggerDispatcher {
    fn service_id(&self) -> ServiceId {
        ServiceId::new("app.logger", [0xFF; 32])
    }

    fn dispatch(&self, _method_id: u32, _payload: Vec<u8>) -> ServiceFuture {
        Box::pin(async { unreachable!("identity mismatch must reject this dispatcher") })
    }
}

#[cordis::event(name = "app.before-run")]
#[allow(dead_code)]
trait BeforeRun {
    type Input = String;
    type Output = String;
}

#[cordis::event(name = "app.announce", mode = "emit")]
#[allow(dead_code)]
trait Announce {
    type Input = String;
    type Output = ();
}

#[cordis::event(name = "app.choose", mode = "serial")]
#[allow(dead_code)]
trait Choose {
    type Input = String;
    type Output = String;
}

#[cordis::event(name = "app.check", mode = "bail")]
#[allow(dead_code)]
trait Check {
    type Input = String;
    type Output = String;
}

#[cordis::event(name = "app.rewrite", mode = "waterfall")]
#[allow(dead_code)]
trait Rewrite {
    type Input = String;
    type Output = String;
}

#[derive(Debug, Deserialize, JsonSchema)]
struct Config {
    prefix: String,
}

#[cordis::component(name = "consumer", config = Config)]
#[cordis::inject(Logger)]
struct Consumer;

#[cordis::component_impl]
impl Consumer {
    #[cordis::apply]
    async fn start(
        &mut self,
        context: ComponentContext<ConsumerDependencies>,
        config: Config,
    ) -> Result<(), cordis::CordisError> {
        tokio::task::yield_now().await;
        context
            .deps()
            .logger
            .log(format!("{}:ready", config.prefix))
            .await
            .map_err(|error| cordis::CordisError::SupervisorFailed {
                message: error.to_string(),
            })?;
        Ok(())
    }
}

#[cordis::service(name = "app.timer")]
trait Timer {
    async fn tick(&self) -> Result<(), String>;
}

#[derive(Debug, Default)]
struct RecordingTimer {
    calls: AtomicUsize,
}

impl Timer for RecordingTimer {
    fn tick(&self) -> impl Future<Output = Result<(), String>> + Send {
        self.calls.fetch_add(1, Ordering::SeqCst);
        std::future::ready(Ok(()))
    }
}

#[derive(Debug)]
#[cordis::component(name = "method-consumer")]
struct MethodConsumer {
    activations: Arc<AtomicUsize>,
    disposals: Arc<AtomicUsize>,
    fail_apply: bool,
    observed_state: Arc<Mutex<Vec<usize>>>,
    state: usize,
}

#[cordis::component_impl]
impl MethodConsumer {
    #[cordis::apply]
    async fn start(
        &mut self,
        _context: ComponentContext<MethodConsumerDependencies>,
        _config: (),
    ) -> Result<(), CordisError> {
        tokio::task::yield_now().await;
        if self.fail_apply {
            return Err(CordisError::SupervisorFailed {
                message: "expected apply failure".to_owned(),
            });
        }
        self.state = 40;
        Ok(())
    }

    #[cordis::inject(Timer)]
    async fn bind_timer(
        &mut self,
        context: MethodContext<MethodConsumerBindTimerDependencies>,
    ) -> Result<(), CordisError> {
        context
            .deps()
            .timer
            .tick()
            .await
            .map_err(|error| CordisError::SupervisorFailed {
                message: error.to_string(),
            })?;
        self.activations.fetch_add(1, Ordering::SeqCst);
        self.state += 1;
        self.observed_state.lock().unwrap().push(self.state);
        let disposals = self.disposals.clone();
        let (_guard, scope) = context.effects().effect("timer-binding")?;
        scope.defer(Disposer::infallible(move || async move {
            disposals.fetch_add(1, Ordering::SeqCst);
        }))?;
        Ok(())
    }
}

async fn begin_load(
    handle: &RuntimeHandle,
    fiber: cordis::FiberId,
    context: Context,
) -> cordis::FiberTransition {
    handle
        .configure_dependencies(fiber, context, Vec::new())
        .await
        .unwrap()
        .transitions
        .into_iter()
        .next()
        .unwrap()
}

async fn wait_for_state(handle: &RuntimeHandle, fiber: cordis::FiberId, expected: FiberState) {
    for _ in 0..1_000 {
        let state = handle
            .snapshot()
            .await
            .unwrap()
            .fibers
            .into_iter()
            .find(|snapshot| snapshot.id == fiber)
            .unwrap()
            .state;
        if state == expected {
            return;
        }
        tokio::task::yield_now().await;
    }
    panic!("fiber {fiber} did not reach {expected:?}");
}

#[tokio::test]
async fn macros_generate_native_component_metadata_and_adapter() {
    assert_eq!(LoggerService::NAME, "app.logger");
    assert_ne!(LoggerService::ABI_HASH, [0; 32]);
    assert_eq!(
        StableWireOneService::ABI_HASH,
        StableWireTwoService::ABI_HASH
    );

    let descriptor = Consumer::descriptor();
    assert_eq!(descriptor.name, "consumer");
    assert_eq!(descriptor.injects.len(), 1);
    assert_eq!(descriptor.injects[0].service, LoggerService::service_id());
    let schema = (descriptor.config_schema)();
    assert!(schema.get("properties").is_some());

    let runtime = Runtime::start();
    let handle = runtime.handle();
    let fiber = handle.create_fiber(None).await.unwrap();
    let logger = Arc::new(RecordingLogger::default());
    let logger_client = LoggerClient::from_native(Arc::clone(&logger));
    assert_eq!(logger_client.service_id(), &LoggerService::service_id());
    assert_eq!(
        logger_client.log(String::new()).await,
        Err(ServiceCallError::Service(
            "message cannot be empty".to_owned()
        ))
    );
    assert!(matches!(
        LoggerClient::new(Arc::new(WrongLoggerDispatcher)),
        Err(CordisError::ServiceIdentityMismatch { .. })
    ));
    let dynamic_client =
        LoggerClient::new(Arc::new(LoggerDispatcher::new(Arc::clone(&logger)))).unwrap();
    dynamic_client
        .log("dynamic:ready".to_owned())
        .await
        .unwrap();
    let context = ComponentContext::new(
        Context::root(fiber),
        ConsumerDependencies::new(logger_client),
        EffectSet::new("consumer"),
    );
    let effects = Consumer
        .apply(
            context,
            Config {
                prefix: "test".to_owned(),
            },
        )
        .await
        .unwrap();
    assert!(effects.effect_set().metadata().is_empty());
    assert_eq!(
        logger.messages.lock().unwrap().as_slice(),
        ["dynamic:ready", "test:ready"]
    );
    runtime.shutdown().await.unwrap();
}

#[tokio::test]
async fn method_inject_runs_in_effect_owned_child_fiber() {
    let runtime = Runtime::start();
    let handle = runtime.handle();
    let realm = handle.allocate_realm().await.unwrap();
    let timer_service = TimerService::service_id();
    let provider_key = ProviderKey::new(timer_service.clone(), realm);

    let parent = handle.create_fiber(None).await.unwrap();
    let parent_context = Context::root(parent).isolate(timer_service.clone(), realm);
    let parent_load = begin_load(&handle, parent, parent_context.clone()).await;
    let services = NativeServiceRegistry::new();
    let method_runtime = MethodFiberRuntime::new(handle.clone(), services.clone());
    let activations = Arc::new(AtomicUsize::new(0));
    let disposals = Arc::new(AtomicUsize::new(0));
    let observed_state = Arc::new(Mutex::new(Vec::new()));
    let context = ComponentContext::new(
        parent_context,
        MethodConsumerDependencies::new(),
        EffectSet::new("method-consumer"),
    )
    .with_method_runtime(method_runtime);

    let effects = MethodConsumer {
        activations: activations.clone(),
        disposals: disposals.clone(),
        fail_apply: false,
        observed_state: observed_state.clone(),
        state: 0,
    }
    .apply(context, ())
    .await
    .unwrap();
    handle
        .complete_transition(parent, parent_load.generation, Ok(()))
        .await
        .unwrap();

    let snapshot = handle.snapshot().await.unwrap();
    let child = snapshot
        .fibers
        .iter()
        .find(|fiber| fiber.parent == Some(parent))
        .unwrap()
        .id;
    assert_eq!(
        snapshot
            .fibers
            .iter()
            .find(|fiber| fiber.id == child)
            .unwrap()
            .state,
        FiberState::Pending
    );
    assert_eq!(activations.load(Ordering::SeqCst), 0);

    let first_provider = handle.create_fiber(None).await.unwrap();
    let first_load = begin_load(&handle, first_provider, Context::root(first_provider)).await;
    handle
        .complete_transition(first_provider, first_load.generation, Ok(()))
        .await
        .unwrap();
    let first_timer = Arc::new(RecordingTimer::default());
    services.insert(
        first_provider,
        Arc::new(TimerDispatcher::new(first_timer.clone())),
    );
    handle
        .provide(provider_key.clone(), first_provider)
        .await
        .unwrap();
    wait_for_state(&handle, child, FiberState::Active).await;
    assert_eq!(activations.load(Ordering::SeqCst), 1);
    assert_eq!(first_timer.calls.load(Ordering::SeqCst), 1);
    assert_eq!(*observed_state.lock().unwrap(), vec![41]);

    handle
        .withdraw(provider_key.clone(), first_provider)
        .await
        .unwrap();
    wait_for_state(&handle, child, FiberState::Pending).await;
    assert_eq!(disposals.load(Ordering::SeqCst), 1);

    let second_provider = handle.create_fiber(None).await.unwrap();
    let second_load = begin_load(&handle, second_provider, Context::root(second_provider)).await;
    handle
        .complete_transition(second_provider, second_load.generation, Ok(()))
        .await
        .unwrap();
    let second_timer = Arc::new(RecordingTimer::default());
    services.insert(
        second_provider,
        Arc::new(TimerDispatcher::new(second_timer.clone())),
    );
    handle.provide(provider_key, second_provider).await.unwrap();
    wait_for_state(&handle, child, FiberState::Active).await;
    assert_eq!(activations.load(Ordering::SeqCst), 2);
    assert_eq!(second_timer.calls.load(Ordering::SeqCst), 1);
    assert_eq!(*observed_state.lock().unwrap(), vec![41, 42]);

    effects.effect_set().dispose().await.unwrap();
    wait_for_state(&handle, child, FiberState::Disposed).await;
    assert_eq!(disposals.load(Ordering::SeqCst), 2);
    runtime.shutdown().await.unwrap();
}

#[tokio::test]
async fn failed_parent_apply_retires_registered_method_fibers() {
    let runtime = Runtime::start();
    let handle = runtime.handle();
    let realm = handle.allocate_realm().await.unwrap();
    let timer_service = TimerService::service_id();
    let parent = handle.create_fiber(None).await.unwrap();
    let parent_context = Context::root(parent).isolate(timer_service, realm);
    let parent_load = begin_load(&handle, parent, parent_context.clone()).await;
    let context = ComponentContext::new(
        parent_context,
        MethodConsumerDependencies::new(),
        EffectSet::new("failing-method-consumer"),
    )
    .with_method_runtime(MethodFiberRuntime::new(
        handle.clone(),
        NativeServiceRegistry::new(),
    ));

    let error = MethodConsumer {
        activations: Arc::new(AtomicUsize::new(0)),
        disposals: Arc::new(AtomicUsize::new(0)),
        fail_apply: true,
        observed_state: Arc::new(Mutex::new(Vec::new())),
        state: 0,
    }
    .apply(context, ())
    .await
    .unwrap_err();
    let snapshot = handle.snapshot().await.unwrap();
    let child = snapshot
        .fibers
        .iter()
        .find(|fiber| fiber.parent == Some(parent))
        .unwrap();
    assert_eq!(child.state, FiberState::Disposed);

    handle
        .complete_transition(parent, parent_load.generation, Err(error))
        .await
        .unwrap();
    runtime.shutdown().await.unwrap();
}

#[tokio::test]
async fn event_macro_generates_codec_and_all_dispatch_modes() {
    assert_eq!(BeforeRunEvent::NAME, "app.before-run");
    assert_eq!(BeforeRunEvent::MODE, cordis::EventMode::Parallel);
    assert_eq!(
        <BeforeRunEvent as EventSpec>::event_id().name(),
        "app.before-run"
    );

    let encoded = BeforeRunEvent::encode_input(&"payload".to_owned()).unwrap();
    assert_eq!(BeforeRunEvent::decode_input(&encoded).unwrap(), "payload");
    assert!(matches!(
        BeforeRunEvent::decode_input(&[0xC1]),
        Err(CordisError::EventDecodeFailed { .. })
    ));
    let event_effects = EffectSet::new("event-listeners");
    let (_listener_guard, listener_scope) = event_effects.effect("before-run").unwrap();
    let before_run = BeforeRunEvent::runtime();
    before_run
        .listen(
            &listener_scope,
            ListenerOptions::global(),
            |input| async move { Ok(ControlFlow::Break(format!("{input}:done"))) },
        )
        .unwrap();
    assert_eq!(
        BeforeRunEvent::dispatch(&before_run, EventTarget::Global, &"start".to_owned())
            .await
            .unwrap(),
        [ControlFlow::Break("start:done".to_owned())]
    );

    let announce = AnnounceEvent::runtime();
    AnnounceEvent::dispatch(&announce, EventTarget::Global, &"ready".to_owned(), |_| {}).unwrap();
    assert_eq!(
        ChooseEvent::dispatch(
            &ChooseEvent::runtime(),
            EventTarget::Global,
            &"choice".to_owned()
        )
        .await
        .unwrap(),
        None
    );
    assert_eq!(
        CheckEvent::dispatch(
            &CheckEvent::runtime(),
            EventTarget::Global,
            &"check".to_owned()
        )
        .unwrap(),
        None
    );
    assert_eq!(
        RewriteEvent::dispatch(
            &RewriteEvent::runtime(),
            EventTarget::Global,
            "rewrite".to_owned()
        )
        .await
        .unwrap(),
        "rewrite"
    );

    event_effects.dispose().await.unwrap();
}

#[allow(dead_code)]
fn assert_event_types()
where
    BeforeRunEvent: EventSpec<Input = String, Output = String>,
{
}
