use cordis::{
    Component, ComponentContext, ComponentDefinition, Context, CordisError, EffectSet, EventSpec,
    Runtime, ServiceCallError, ServiceDispatcher, ServiceFuture, ServiceId, ServiceSpec,
};
use schemars::JsonSchema;
use serde::Deserialize;
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
        self,
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

#[tokio::test]
async fn macros_generate_native_component_metadata_and_adapter() {
    assert_eq!(LoggerService::NAME, "app.logger");
    assert_ne!(LoggerService::ABI_HASH, [0; 32]);
    assert_eq!(
        StableWireOneService::ABI_HASH,
        StableWireTwoService::ABI_HASH
    );
    assert_eq!(BeforeRunEvent::NAME, "app.before-run");

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

#[allow(dead_code)]
fn assert_event_types()
where
    BeforeRunEvent: EventSpec<Input = String, Output = String>,
{
}
