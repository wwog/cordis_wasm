use cordis::{
    Component, ComponentContext, ComponentDefinition, Context, EffectSet, EventSpec, Runtime,
    ServiceSpec,
};
use schemars::JsonSchema;
use serde::Deserialize;

#[cordis::service(name = "app.logger")]
#[allow(dead_code)]
trait Logger {
    fn log(&self, message: &str);
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
        _context: ComponentContext<ConsumerDependencies>,
        config: Config,
    ) -> Result<(), cordis::CordisError> {
        tokio::task::yield_now().await;
        assert_eq!(config.prefix, "test");
        Ok(())
    }
}

#[tokio::test]
async fn macros_generate_native_component_metadata_and_adapter() {
    assert_eq!(LoggerService::NAME, "app.logger");
    assert_ne!(LoggerService::ABI_HASH, [0; 32]);
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
    let context = ComponentContext::new(
        Context::root(fiber),
        ConsumerDependencies,
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
    runtime.shutdown().await.unwrap();
}

#[allow(dead_code)]
fn assert_event_types()
where
    BeforeRunEvent: EventSpec<Input = String, Output = String>,
{
}
